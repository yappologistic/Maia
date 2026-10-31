//! Search by meaning: files in the chosen folders are cut into passages, each turned
//! into a fingerprint of its meaning by a local model (through Ollama), and a search
//! compares the question's fingerprint with all of them. Off unless switched on in
//! Options; nothing runs, listens or is stored until then.
//!
//! The index lives in the cache (`maia/meaning.idx`): per file its path, mtime and size,
//! per passage where it sits in the file and its fingerprint as DIMS signed bytes. Only
//! files that changed since the last run are read again. The indexer runs at the lowest
//! CPU priority there is, so it never competes with anything else.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

use crate::ollama::{self, DIMS};

/// Passages are about this many characters (some 450 of the model's tokens), overlapping
/// a little so a sentence cut at one edge is whole in the next.
const PASSAGE: usize = 1800;
const OVERLAP: usize = 200;
/// Enough for a long document; a log or data dump past this is not what one searches.
const MAX_PASSAGES: usize = 120;
const MAX_TEXT: u64 = 4 << 20;
const MAX_PDF: u64 = 60 << 20;
const MAX_FILES: usize = 200_000;
/// Passages per request to Ollama: enough to keep a graphics card busy, few enough that
/// a search waiting behind one is answered within a moment.
const BATCH: usize = 16;
/// Pictures per request.
const PICTURE_BATCH: usize = 2;

/// Pictures, read by the model as they look (with "Include pictures" on).
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp"];
const MAX_IMAGE: u64 = 40 << 20;
/// Pictures go to the model at most this many pixels on a side. The model's work grows
/// with the pixels (a 2000-pixel screenshot takes it some 40 s on a laptop CPU, the same
/// shrunk to 512 under 2 s) and what it makes of a picture is as good at this size.
const IMAGE_SIDE: u32 = 512;
/// Smaller than this on its longest side: an icon, cursor or button, not a picture.
const MIN_PICTURE: u32 = 256;
/// A folder with this many pictures, nearly all named by a program ("frame_0042",
/// "3f9a1c0e…", not a camera's "IMG_1234"), holds frames cut from a video or a dataset:
/// thousands of near-copies, skipped.
const FRAME_RUN: usize = 40;

pub fn is_picture(p: &Path) -> bool {
    p.extension().is_some_and(|e| IMAGE_EXTS.contains(&e.to_string_lossy().to_ascii_lowercase().as_str()))
}

/// Writing meant to be read (and PDFs). Code, data, settings and logs are left out: an
/// editor searches them better, and their thousands of files drown what people look for.
const DOC_EXTS: &[&str] = &["txt", "text", "md", "markdown", "rst", "org", "tex", "adoc", "rtf", "html", "htm"];

/// Folders of other people's or a program's files, not walked into: packages, vendored
/// and built code, run logs.
const SKIP_DIRS: &[&str] = &[
    "node_modules", "bower_components", "target", "__pycache__", "venv", "site-packages", "build", "dist", "out", "vendor", "vendored", "third_party", "third-party", "thirdparty",
    "3rdparty", "external", "deps", "coverage", "htmlcov", "generated", "__generated__", "wandb", "mlruns", "lightning_logs", "Pods", "DerivedData",
];

/// What a file is, for the kinds a question can ask for ("a photo of…", "the pdf about…").
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Picture,
    Pdf,
    Doc,
}

fn kind(p: &Path) -> Kind {
    if is_picture(p) {
        Kind::Picture
    } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")) {
        Kind::Pdf
    } else {
        Kind::Doc
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    /// Asking Ollama whether it is there and has the model.
    Checking,
    NotRunning,
    NoModel,
    /// Fetching the model: bytes done, bytes in all.
    Downloading(u64, u64),
    /// Files looked at, files to look at.
    Indexing(usize, usize),
    Ready,
    Failed(String),
}

#[derive(Clone)]
struct FileRec {
    path: PathBuf,
    mtime: i64,
    size: u64,
    /// Passages: byte where each starts in the text, and its length.
    passages: Vec<(u32, u32)>,
    /// Each passage's opening words, shown with a match (no file read to show it).
    previews: Vec<String>,
    /// First passage's fingerprint index in `Index::vecs` (DIMS bytes each).
    first: usize,
    /// Every word in the text, once, lowercase and space-separated: for the question's
    /// own words, wherever in a long text they are.
    words: String,
}

#[derive(Default)]
pub struct Index {
    files: Vec<FileRec>,
    vecs: Vec<i8>,
    /// Each fingerprint's length (as stored bytes), worked out on the first search.
    lens: std::sync::OnceLock<Vec<f32>>,
}

impl Index {
    fn lens(&self) -> &[f32] {
        self.lens.get_or_init(|| self.vecs.chunks(DIMS).map(|v| (v.iter().map(|&x| x as i32 * x as i32).sum::<i32>() as f32).sqrt().max(1.0)).collect())
    }
}

impl Index {
    pub fn passages(&self) -> usize {
        self.vecs.len() / DIMS
    }
    pub fn files(&self) -> usize {
        self.files.len()
    }
}

#[derive(Clone, Debug)]
pub struct Hit {
    pub path: PathBuf,
    pub score: f32,
    /// The passage that matched, on one line.
    pub passage: String,
    /// Close to the best match; the rest trail far behind it.
    pub strong: bool,
}

struct Shared {
    phase: Phase,
    index: Arc<Index>,
    /// Pictures left for when the laptop is on its charger.
    waiting: usize,
    /// The user asked to fetch the model.
    pull: bool,
}

/// The running engine; dropping it stops the indexer.
pub struct Engine {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    wake: Arc<(Mutex<bool>, std::sync::Condvar)>,
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        self.poke();
    }
}

/// "~/Documents, ~/Notes" as folders.
pub fn folders(list: &str) -> Vec<PathBuf> {
    list.split(',').map(str::trim).filter(|s| !s.is_empty()).map(|s| match s.strip_prefix('~') {
        Some(rest) => crate::fsx::home().join(rest.trim_start_matches('/')),
        None => PathBuf::from(s),
    }).collect()
}

fn index_path() -> PathBuf {
    let base = if cfg!(test) { None } else { dirs::cache_dir() };
    base.unwrap_or_else(std::env::temp_dir).join("maia/meaning.idx")
}

impl Engine {
    pub fn start(folders: Vec<PathBuf>, pictures: bool, ctx: &egui::Context) -> Engine {
        let shared = Arc::new(Mutex::new(Shared { phase: Phase::Checking, index: Arc::new(Index::default()), waiting: 0, pull: false }));
        let stop = Arc::new(AtomicBool::new(false));
        let wake = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let (s, st, w, ctx) = (shared.clone(), stop.clone(), wake.clone(), ctx.clone());
        std::thread::Builder::new().name("meaning".into()).spawn(move || run(folders, pictures, s, st, w, ctx)).ok();
        Engine { shared, stop, wake }
    }

    pub fn phase(&self) -> Phase {
        self.shared.lock().unwrap_or_else(|e| e.into_inner()).phase.clone()
    }

    pub fn index(&self) -> Arc<Index> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner()).index.clone()
    }

    /// Pictures not read yet because the laptop runs on its battery.
    pub fn waiting(&self) -> usize {
        self.shared.lock().unwrap_or_else(|e| e.into_inner()).waiting
    }

    /// Fetch the model now (the user said so), then index.
    pub fn download(&self) {
        self.shared.lock().unwrap_or_else(|e| e.into_inner()).pull = true;
        self.poke();
    }

    /// Look again: Ollama may have been started, or files changed.
    pub fn poke(&self) {
        let (m, cv) = &*self.wake;
        *m.lock().unwrap_or_else(|e| e.into_inner()) = true;
        cv.notify_one();
    }
}

fn set(shared: &Mutex<Shared>, ctx: &egui::Context, phase: Phase) {
    shared.lock().unwrap_or_else(|e| e.into_inner()).phase = phase;
    ctx.request_repaint();
}

/// The indexer: check Ollama, fetch the model when asked, bring the index up to date,
/// then sleep until poked (or a few minutes pass) and look again.
fn run(folders: Vec<PathBuf>, pictures: bool, shared: Arc<Mutex<Shared>>, stop: Arc<AtomicBool>, wake: Arc<(Mutex<bool>, std::sync::Condvar)>, ctx: egui::Context) {
    // The lowest priority Linux has: only runs when nothing else wants the CPU.
    unsafe {
        let p = libc::sched_param { sched_priority: 0 };
        libc::sched_setscheduler(0, libc::SCHED_IDLE, &p);
    }
    // Last run's index, read here rather than on the window's thread.
    shared.lock().unwrap_or_else(|e| e.into_inner()).index = Arc::new(load(&index_path()));
    ctx.request_repaint();
    while !stop.load(Relaxed) {
        set(&shared, &ctx, Phase::Checking);
        let st = ollama::status();
        let want_pull = std::mem::take(&mut shared.lock().unwrap_or_else(|e| e.into_inner()).pull);
        match st {
            ollama::Status::NotRunning => set(&shared, &ctx, Phase::NotRunning),
            ollama::Status::NoModel if want_pull => {
                let r = ollama::pull(|done, all| set(&shared, &ctx, Phase::Downloading(done, all)), &stop);
                match r {
                    Ok(()) => continue,
                    Err(e) => set(&shared, &ctx, Phase::Failed(e)),
                }
            }
            ollama::Status::NoModel => set(&shared, &ctx, Phase::NoModel),
            ollama::Status::Ready => match update(&folders, pictures, &shared, &stop, &ctx) {
                Ok(()) => set(&shared, &ctx, Phase::Ready),
                Err(e) => set(&shared, &ctx, Phase::Failed(e)),
            },
        }
        // Sleep until poked; look again every few minutes for changed files.
        let (m, cv) = &*wake;
        let mut poked = m.lock().unwrap_or_else(|e| e.into_inner());
        if !*poked {
            poked = cv.wait_timeout(poked, std::time::Duration::from_secs(300)).map(|r| r.0).unwrap_or_else(|e| e.into_inner().0);
        }
        *poked = false;
    }
}

/// Work for the model: the fingerprint rows to fill and what goes in each.
struct Job {
    rows: Vec<usize>,
    inputs: Vec<serde_json::Value>,
}

/// Rows and their fingerprints; None for an input the model couldn't take (a broken
/// picture), whose file is left out.
type Done = (Vec<usize>, Vec<Option<Vec<i8>>>);

/// Searches running now: the indexer holds its next request until they are answered, so
/// a question never queues behind a batch of files.
static SEARCHING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A fingerprint as signed bytes, scaled so its largest number is ±127: every byte's
/// range is used (scaling a whole fingerprint changes no cosine, which `search` takes
/// with each one's own length).
fn q8(e: &[f32]) -> Vec<i8> {
    let top = e.iter().fold(0f32, |m, x| m.max(x.abs())).max(1e-9);
    e.iter().map(|x| (x / top * 127.0).round().clamp(-127.0, 127.0) as i8).collect()
}

fn embed_job(rows: Vec<usize>, inputs: Vec<serde_json::Value>) -> Result<Done, String> {
    let n = inputs.len();
    match ollama::embed_values(inputs.clone()) {
        Ok(v) => Ok((rows, v.iter().map(|e| Some(q8(e))).collect())),
        // A single input refused while Ollama is fine: that input is the trouble.
        Err(_) if n == 1 && ollama::status() == ollama::Status::Ready => Ok((rows, vec![None])),
        Err(e) if n == 1 => Err(e),
        // One bad picture spoils a batch: each again on its own.
        Err(_) => {
            let mut out = Vec::with_capacity(n);
            for (r, i) in rows.iter().zip(inputs) {
                out.extend(embed_job(vec![*r], vec![i])?.1);
            }
            Ok((rows, out))
        }
    }
}

/// Bring the index up to date with the folders: read new and changed files, drop gone
/// ones. Saved as it goes, so an interrupted run keeps what it did.
///
/// Files are read and pictures shrunk on this thread while two others carry batches to
/// the model, so the model (on a graphics card, the slow part is otherwise the waiting)
/// always has the next batch ready.
fn update(folders: &[PathBuf], pictures: bool, shared: &Mutex<Shared>, stop: &AtomicBool, ctx: &egui::Context) -> Result<(), String> {
    let mut found = vec![];
    for f in folders {
        walk(f, pictures, &mut found, stop);
    }
    found.sort();
    found.dedup();
    // Newest first, text and pictures together: what was worked on lately is likeliest
    // to be looked for, and pictures are findable from the start.
    found.sort_by_key(|(_, mtime, _)| std::cmp::Reverse(*mtime));
    let old = shared.lock().unwrap_or_else(|e| e.into_inner()).index.clone();
    let known: std::collections::HashMap<&Path, &FileRec> = old.files.iter().map(|f| (f.path.as_path(), f)).collect();
    let total = found.len();
    let (job_tx, job_rx) = std::sync::mpsc::sync_channel::<Job>(4);
    let (done_tx, done_rx) = std::sync::mpsc::channel::<Result<Done, String>>();
    let job_rx = Mutex::new(job_rx);
    std::thread::scope(|sc| {
        for _ in 0..2 {
            let (rx, tx) = (&job_rx, done_tx.clone());
            sc.spawn(move || loop {
                let Ok(job) = rx.lock().unwrap_or_else(|e| e.into_inner()).recv() else { return };
                let t0 = std::time::Instant::now();
                while SEARCHING.load(Relaxed) > 0 && t0.elapsed().as_secs() < 3 && !stop.load(Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                if tx.send(embed_job(job.rows, job.inputs)).is_err() {
                    return;
                }
            });
        }
        drop(done_tx);
        // The loop owns the way to the senders: whatever way it ends, they then finish.
        let run = |job_tx: std::sync::mpsc::SyncSender<Job>| -> Result<(), String> {
            let mut next = Index::default();
            let mut saved = std::time::Instant::now();
            let mut changed = false;
            let mut copies: std::collections::HashMap<(std::ffi::OsString, u64), usize> = Default::default();
            let mut waiting = 0;
            let mut failed: std::collections::HashSet<usize> = Default::default();
            let (mut texts, mut pics): (Job, Job) = (Job { rows: vec![], inputs: vec![] }, Job { rows: vec![], inputs: vec![] });
            let mut inflight = 0usize;
            // Results in, written to their rows; with `all`, waits for every batch out.
            let take = |next: &mut Index, failed: &mut std::collections::HashSet<usize>, inflight: &mut usize, all: bool| -> Result<(), String> {
                while *inflight > 0 {
                    let r = if all { done_rx.recv().map_err(|_| "The indexer stopped".to_string())? } else {
                        match done_rx.try_recv() {
                            Ok(r) => r,
                            Err(_) => return Ok(()),
                        }
                    };
                    *inflight -= 1;
                    let (rows, vecs) = r?;
                    for (row, v) in rows.into_iter().zip(vecs) {
                        match v {
                            Some(v) => next.vecs[row * DIMS..(row + 1) * DIMS].copy_from_slice(&v),
                            None => {
                                failed.insert(row);
                            }
                        }
                    }
                }
                Ok(())
            };
            let send = |job: &mut Job, inflight: &mut usize| -> Result<(), String> {
                if job.rows.is_empty() {
                    return Ok(());
                }
                let j = std::mem::replace(job, Job { rows: vec![], inputs: vec![] });
                job_tx.send(j).map_err(|_| "The indexer stopped".to_string())?;
                *inflight += 1;
                Ok(())
            };
            for (k, (path, mtime, size)) in found.into_iter().enumerate() {
                if stop.load(Relaxed) {
                    return Ok(());
                }
                if k % 20 == 0 {
                    set(shared, ctx, Phase::Indexing(k, total));
                }
                take(&mut next, &mut failed, &mut inflight, false)?;
                // Unchanged since the last run: keep its passages as they are.
                if let Some(f) = known.get(path.as_path()).filter(|f| f.mtime == mtime && f.size == size) {
                    let first = next.vecs.len() / DIMS;
                    if is_picture(&path) {
                        copies.entry((path.file_name().map(|n| n.to_os_string()).unwrap_or_default(), size)).or_insert(first);
                    }
                    next.vecs.extend_from_slice(&old.vecs[f.first * DIMS..(f.first + f.passages.len()) * DIMS]);
                    next.files.push(FileRec { path, mtime, size, first, ..(*f).clone() });
                } else if is_picture(&path) {
                    // A picture: one fingerprint for how it looks. A copy of one already
                    // read (same name and size, as in a package unpacked twice) shares it.
                    let key = (path.file_name().map(|n| n.to_os_string()).unwrap_or_default(), size);
                    let first = match copies.get(&key) {
                        Some(&row) => row,
                        // Pictures keep the machine busy: on battery they wait for the charger
                        // (the next look, every few minutes, takes them up).
                        None if on_battery() => {
                            waiting += 1;
                            continue;
                        }
                        None => {
                            let Some(bytes) = picture_bytes(&path) else { continue };
                            let row = next.vecs.len() / DIMS;
                            next.vecs.resize((row + 1) * DIMS, 0);
                            pics.rows.push(row);
                            pics.inputs.push(ollama::image(&bytes));
                            copies.insert(key, row);
                            row
                        }
                    };
                    next.files.push(FileRec { path, mtime, size, passages: vec![(0, 0)], previews: vec![String::new()], first, words: String::new() });
                    changed = true;
                    if pics.rows.len() >= PICTURE_BATCH {
                        send(&mut pics, &mut inflight)?;
                    }
                } else {
                    let Some(text) = read(&path, size) else { continue };
                    // Number dumps, logs of values and tables of figures say nothing a meaning can find.
                    let passages: Vec<(u32, u32)> = cut(&text).into_iter().filter(|&(a, n)| wordy(&text[a as usize..(a + n) as usize])).collect();
                    if passages.is_empty() {
                        continue;
                    }
                    let title = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let first = next.vecs.len() / DIMS;
                    next.vecs.resize((first + passages.len()) * DIMS, 0);
                    for (j, &(a, n)) in passages.iter().enumerate() {
                        texts.rows.push(first + j);
                        texts.inputs.push(format!("title: {title} | text: {}", &text[a as usize..(a + n) as usize]).into());
                        if texts.rows.len() >= BATCH {
                            send(&mut texts, &mut inflight)?;
                        }
                    }
                    let previews = passages.iter().map(|&(a, n)| preview(&text[a as usize..(a + n) as usize])).collect();
                    next.files.push(FileRec { path, mtime, size, passages, previews, first, words: vocabulary(&text) });
                    changed = true;
                }
                // Searchable (and saved) as it grows, a few times a minute at most.
                if saved.elapsed().as_secs() >= 8 {
                    send(&mut texts, &mut inflight)?;
                    send(&mut pics, &mut inflight)?;
                    take(&mut next, &mut failed, &mut inflight, true)?;
                    saved = std::time::Instant::now();
                    publish(shared, &next, &failed, ctx);
                }
            }
            send(&mut texts, &mut inflight)?;
            send(&mut pics, &mut inflight)?;
            take(&mut next, &mut failed, &mut inflight, true)?;
            shared.lock().unwrap_or_else(|e| e.into_inner()).waiting = waiting;
            // Nothing new and nothing gone: the index stands as it is.
            if changed || next.files.len() != old.files.len() {
                publish(shared, &next, &failed, ctx);
            }
            Ok(())
        };
        run(job_tx)
    })
}

/// Make `next` the index searches use, and save it; files whose fingerprint couldn't be
/// made are left out.
fn publish(shared: &Mutex<Shared>, next: &Index, failed: &std::collections::HashSet<usize>, ctx: &egui::Context) {
    let files = next.files.iter().filter(|f| !failed.contains(&f.first)).cloned().collect();
    let copy = Index { files, vecs: next.vecs.clone(), ..Default::default() };
    let _ = save(&index_path(), &copy);
    shared.lock().unwrap_or_else(|e| e.into_inner()).index = Arc::new(copy);
    ctx.request_repaint();
}

/// Folders whose pictures belong to software (icons, cursors, app resources), not to
/// the person: their documents are still read, their pictures not.
const SOFTWARE: &[&str] = &["icons", "cursors", "squashfs-root", "app.asar.unpacked", "site-packages", "emoji", "flags", "sprites", "pkg"];

/// Files worth reading under `dir`: documents and PDFs, outside hidden, package and build
/// folders, and with `pictures` the pictures that are someone's (not icons, not video
/// frames or datasets).
fn walk(dir: &Path, pictures: bool, out: &mut Vec<(PathBuf, i64, u64)>, stop: &AtomicBool) {
    let mut todo = vec![(dir.to_path_buf(), pictures)];
    while let Some((d, pictures)) = todo.pop() {
        if stop.load(Relaxed) || out.len() >= MAX_FILES {
            return;
        }
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let start = out.len();
        for e in rd.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if !SKIP_DIRS.contains(&name.as_ref()) {
                    todo.push((e.path(), pictures && !SOFTWARE.contains(&name.to_ascii_lowercase().as_str())));
                }
            } else if ft.is_file() {
                // Text files that are a build's or a package's, not writing.
                let lower = name.to_ascii_lowercase();
                if matches!(lower.as_str(), "cmakelists.txt" | "requirements.txt" | "requirements-dev.txt" | "constraints.txt" | "robots.txt" | "copying" | "notice" | "notice.txt" | "third_party_notices.txt")
                    || lower.starts_with("license")
                    || lower.starts_with("licence")
                    || lower.contains(".min.")
                {
                    continue;
                }
                let ext = name.rsplit_once('.').map(|(_, x)| x.to_ascii_lowercase()).unwrap_or_default();
                let pdf = ext == "pdf";
                let picture = pictures && IMAGE_EXTS.contains(&ext.as_str());
                if !pdf && !picture && !DOC_EXTS.contains(&ext.as_str()) {
                    continue;
                }
                let Ok(m) = e.metadata() else { continue };
                if m.len() == 0 || m.len() > if pdf { MAX_PDF } else if picture { MAX_IMAGE } else { MAX_TEXT } {
                    continue;
                }
                // A few kilobytes is an icon or a blank.
                if picture && m.len() < 6 << 10 {
                    continue;
                }
                use std::os::unix::fs::MetadataExt;
                out.push((e.path(), m.mtime(), m.len()));
            }
        }
        // Frames of a video or a dataset: many pictures here, nearly all named by a program.
        let here = &out[start..];
        let pics = here.iter().filter(|(p, ..)| is_picture(p)).count();
        if pics >= FRAME_RUN && here.iter().filter(|(p, ..)| is_picture(p) && machine_named(p)).count() * 10 >= pics * 7 {
            let mut k = start;
            while k < out.len() {
                if is_picture(&out[k].0) { out.swap_remove(k); } else { k += 1; }
            }
        }
    }
}

#[cfg(test)]
pub static TEST_BATTERY: AtomicBool = AtomicBool::new(false);

/// Running on a battery that is draining (a desktop, or a laptop on its charger, isn't).
fn on_battery() -> bool {
    #[cfg(test)]
    return TEST_BATTERY.load(Relaxed);
    #[cfg(not(test))]
    std::fs::read_dir("/sys/class/power_supply").into_iter().flatten().flatten().any(|e| {
        let read = |f: &str| std::fs::read_to_string(e.path().join(f)).unwrap_or_default();
        read("type").trim() == "Battery" && read("status").trim() == "Discharging"
    })
}

/// A name a program gave: "frame_0042", "clip-000123", "17", "3f9a1c0e-7b2d"; not a
/// camera's or phone's ("IMG_1234", "PXL_2026…", "DSC0042"), which are someone's photos.
pub fn machine_named(p: &Path) -> bool {
    const CAMERA: &[&str] = &["img", "dsc", "pxl", "mvimg", "vid", "gopr", "dji", "photo", "whatsapp", "screenshot", "signal"];
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    if CAMERA.iter().any(|c| stem.starts_with(c)) {
        return false;
    }
    let b = stem.as_bytes();
    // Hex: "3f9a1c0e", "3f9a1c0e-7b2d-…".
    let hex = b.iter().take_while(|c| c.is_ascii_hexdigit()).count();
    if hex >= 8 && stem.split(['-', '_']).all(|part| part.len() >= 4 && part.bytes().all(|c| c.is_ascii_hexdigit())) {
        return true;
    }
    // A few letters, maybe a separator, then three or more digits (and more numbers).
    let letters = b.iter().take_while(|c| c.is_ascii_lowercase()).count();
    if letters > 16 {
        return false;
    }
    let mut rest = &b[letters..];
    if let Some((&c, r)) = rest.split_first() {
        if matches!(c, b'-' | b'_' | b'.' | b' ') {
            rest = r;
        }
    }
    let digits = rest.iter().take_while(|c| c.is_ascii_digit()).count();
    digits >= 3 && rest[digits..].split(|c| matches!(c, b'-' | b'_' | b'.')).skip(1).all(|n| !n.is_empty() && n.iter().all(u8::is_ascii_digit)) && (digits == rest.len() || matches!(rest[digits], b'-' | b'_' | b'.'))
}

/// A picture as the model takes it: decoded, shrunk to IMAGE_SIDE and written as PNG.
/// None for one too small to be more than an icon.
fn picture_bytes(path: &Path) -> Option<Vec<u8>> {
    let (img, [w, h]) = crate::imgdec::load(path, IMAGE_SIDE)?;
    if w.max(h) < MIN_PICTURE {
        return None;
    }
    crate::thumbcache::encode_png(&img, &[])
}

/// A file's text: as it is, or a PDF's through pdftotext when that is installed.
fn read(path: &Path, size: u64) -> Option<String> {
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")) {
        if !crate::media::have("pdftotext") {
            return None;
        }
        let mut c = crate::media::tool("pdftotext");
        c.args(["-q", "-enc", "UTF-8", "-l", "60"]).arg(path).arg("-");
        let out = crate::media::output(c, std::time::Duration::from_secs(60))?;
        return Some(String::from_utf8_lossy(&out).into_owned());
    }
    let _ = size;
    let bytes = std::fs::read(path).ok()?;
    let head = &bytes[..bytes.len().min(8192)];
    if bytes.len() as u64 > MAX_TEXT || head.contains(&0) {
        return None;
    }
    // A web page a program wrote: lines thousands of characters long.
    let html = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"));
    if html && head.split(|&b| b == b'\n').any(|l| l.len() > 1000) {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Passages of `text`: (start, length) in bytes, ending at a paragraph or line break
/// where one is near.
pub fn cut(text: &str) -> Vec<(u32, u32)> {
    let mut out = vec![];
    let mut at = 0;
    while at < text.len() && out.len() < MAX_PASSAGES {
        let mut end = text.floor_char_boundary((at + PASSAGE).min(text.len()));
        let w0 = text.floor_char_boundary(at + PASSAGE * 2 / 3);
        if end < text.len() && w0 > at && w0 < end {
            // Back to a paragraph break, else a line break, in the passage's last third.
            let window = &text[w0..end];
            if let Some(i) = window.rfind("\n\n").or_else(|| window.rfind('\n')).or_else(|| window.rfind(". ")) {
                end = w0 + i + 1;
            }
        }
        if text[at..end].trim().len() > 20 {
            out.push((at as u32, (end - at) as u32));
        }
        if end >= text.len() {
            break;
        }
        at = text.ceil_char_boundary(end.saturating_sub(OVERLAP).max(at + 1));
    }
    out
}

/// How far a passage must stand out from everything else for this question to count as
/// a match by meaning, in standard deviations.
const STANDS_OUT: f32 = 2.0;

/// The files that answer `query`, best first, each with the passage that does.
///
/// Meaning: every passage's cosine with the question, put on this question's own scale:
/// less the average of its kind (pictures and text sit at different levels for the same
/// question), in standard deviations of all of them. Only what stands out counts, which
/// also lets a search find nothing. Words: the question's own words in a file's name,
/// folders and text rescue what a meaning can't hold exactly (names, codes, numbers).
/// A file counts if it stands out or holds half the words; its score is how far it
/// stands out, plus more the more of the words it holds, plus one when it is the kind
/// asked for. Results under half the best one's score are marked weak.
pub fn search(index: &Index, query: &str, limit: usize) -> Result<Vec<Hit>, String> {
    struct Asking;
    impl Drop for Asking {
        fn drop(&mut self) {
            SEARCHING.fetch_sub(1, Relaxed);
        }
    }
    SEARCHING.fetch_add(1, Relaxed);
    let asking = Asking;
    let q = ollama::embed(&[format!("task: search result | query: {query}")])?.pop().unwrap_or_default();
    drop(asking);
    let q8: Vec<i32> = q8(&q).into_iter().map(i32::from).collect();
    let qn = (q8.iter().map(|x| x * x).sum::<i32>() as f32).sqrt().max(1.0);
    let row = |r: usize| &index.vecs[r * DIMS..(r + 1) * DIMS];
    let lens = index.lens();
    let cos = |r: usize| row(r).iter().zip(&q8).map(|(&a, &b)| a as i32 * b).sum::<i32>() as f32 / (lens[r] * qn);
    // Every passage's cosine, and the scale: each kind's average, then the spread.
    let files = &index.files;
    let scores: Vec<Vec<f32>> = files.iter().map(|f| (0..f.passages.len()).map(|p| cos(f.first + p)).collect()).collect();
    let (mut sum, mut n) = ([0f64; 2], [0usize; 2]);
    for (f, sc) in files.iter().zip(&scores) {
        let m = is_picture(&f.path) as usize;
        sum[m] += sc.iter().map(|&x| x as f64).sum::<f64>();
        n[m] += sc.len();
    }
    let all = (sum[0] + sum[1]) / (n[0] + n[1]).max(1) as f64;
    // A kind with few passages takes the average of all.
    let mean = [0, 1].map(|m| if n[m] >= 20 { sum[m] / n[m] as f64 } else { all });
    let centred = |f: &FileRec, x: f32| x as f64 - mean[is_picture(&f.path) as usize];
    let count = (n[0] + n[1]).max(1) as f64;
    let mid = files.iter().zip(&scores).map(|(f, sc)| sc.iter().map(|&x| centred(f, x)).sum::<f64>()).sum::<f64>() / count;
    let spread = (files.iter().zip(&scores).map(|(f, sc)| sc.iter().map(|&x| (centred(f, x) - mid).powi(2)).sum::<f64>()).sum::<f64>() / count).sqrt().max(0.02);
    let z = |f: &FileRec, x: f32| ((centred(f, x) - mid) / spread) as f32;
    // Among n passages one can stand out by √(n−1) deviations at most: a small collection
    // asks for less.
    let gate = STANDS_OUT.min(0.5 * (count as f32 - 1.0).max(0.0).sqrt());
    // The question's words, and the kinds it asks for.
    let words = terms(query);
    let lookup = !words.is_empty() && words.iter().all(|w| w.chars().any(|c| c.is_numeric()) && (w.chars().any(char::is_alphabetic) || w.chars().count() >= 4));
    let (meaning_weight, words_weight) = if lookup { (0.5, 4.0) } else { (1.0, 2.0) };
    let asked = kinds_asked(query);
    let home = crate::fsx::home();
    let mut found: Vec<(f32, usize, usize)> = vec![];
    for (fi, (f, sc)) in files.iter().zip(&scores).enumerate() {
        let Some((best, bp)) = sc.iter().enumerate().map(|(p, &x)| (z(f, x), p)).max_by(|a, b| a.0.total_cmp(&b.0)) else { continue };
        // Words: in the name and folders (below the home folder), and in the passages.
        let (mut held, mut wp, mut wp_held) = (0usize, None, 0usize);
        if !words.is_empty() {
            let place = f.path.strip_prefix(&home).unwrap_or(&f.path).to_string_lossy().to_lowercase();
            let lower: Vec<String> = f.previews.iter().map(|t| t.to_lowercase()).collect();
            for w in &words {
                if has_word(&place, w) || has_word(&f.words, w) {
                    held += 1;
                }
            }
            for (p, t) in lower.iter().enumerate() {
                let k = words.iter().filter(|w| has_word(t, w)).count();
                if k > wp_held {
                    (wp, wp_held) = (Some(p), k);
                }
            }
        }
        let coverage = if words.is_empty() { 0.0 } else { held as f32 / words.len() as f32 };
        if best < gate && coverage < 0.5 {
            continue;
        }
        let mut score = meaning_weight * best.max(0.0) + words_weight * coverage.powf(1.5);
        if asked.contains(&kind(&f.path)) {
            score += 1.0;
        }
        // Shown: the passage that stands out, else the one with the words.
        let p = if best >= gate { bp } else { wp.unwrap_or(bp) };
        found.push((score, fi, p));
    }
    found.sort_by(|a, b| b.0.total_cmp(&a.0));
    // The same picture saved in several places (or near copies of it) shown once: the
    // best placed one stands for the rest.
    let look = |fi: usize| row(files[fi].first);
    let alike = |a: &[i8], b: &[i8]| {
        let dot = |x: &[i8], y: &[i8]| x.iter().zip(y).map(|(&p, &q)| p as i32 * q as i32).sum::<i32>() as f32;
        dot(a, b) >= 0.99 * (dot(a, a) * dot(b, b)).sqrt()
    };
    let mut shown: Vec<usize> = vec![];
    found.retain(|&(_, fi, _)| {
        if !is_picture(&files[fi].path) {
            return true;
        }
        if shown.iter().any(|&o| alike(look(o), look(fi))) {
            return false;
        }
        shown.push(fi);
        true
    });
    let top = found.first().map_or(0.0, |h| h.0);
    Ok(found.into_iter().take(limit).map(|(score, fi, p)| {
        let f = &files[fi];
        Hit { path: f.path.clone(), score, passage: f.previews.get(p).cloned().unwrap_or_default(), strong: score >= top * 0.5 }
    }).collect())
}

/// Whether `word` (lowercase) is in `hay` (lowercase) as a word of its own, or the start
/// of one with an ending ("dog" in "dogs", "plan" in "planned"); not inside another
/// ("cat" in "category"). Two-letter words count only whole.
fn has_word(hay: &str, word: &str) -> bool {
    const ENDINGS: &[&str] = &["s", "es", "ed", "d", "ing", "er", "ers", "ly", "'s"];
    let b = hay.as_bytes();
    let mut from = 0;
    while let Some(i) = hay[from..].find(word) {
        let at = from + i;
        let end = at + word.len();
        let starts = at == 0 || !hay[..at].chars().next_back().is_some_and(char::is_alphanumeric);
        if starts {
            let rest = &hay[end..];
            let tail = rest.find(|c: char| !c.is_alphanumeric() && c != '\'').unwrap_or(rest.len());
            if tail == 0 || (word.chars().count() >= 3 && ENDINGS.contains(&&rest[..tail])) {
                return true;
            }
        }
        from = hay.ceil_char_boundary(at + 1).min(b.len());
        if from >= hay.len() {
            break;
        }
    }
    false
}

/// The kinds of file a question names ("a photo of…", "…in a pdf", "my notes on…"): a
/// nudge for them, not a filter.
fn kinds_asked(query: &str) -> Vec<Kind> {
    let mut out = vec![];
    for w in query.split(|c: char| !c.is_alphanumeric()).map(str::to_lowercase) {
        let k = match w.as_str() {
            "photo" | "photos" | "picture" | "pictures" | "pic" | "pics" | "image" | "images" | "screenshot" | "screenshots" | "wallpaper" | "wallpapers" | "drawing" | "selfie" => Kind::Picture,
            "pdf" | "pdfs" | "paper" | "papers" => Kind::Pdf,
            "document" | "documents" | "doc" | "docs" | "note" | "notes" | "notebook" | "essay" | "report" | "readme" => Kind::Doc,
            _ => continue,
        };
        if !out.contains(&k) {
            out.push(k);
        }
    }
    out
}

/// A passage's opening words on one line.
fn preview(text: &str) -> String {
    let mut out = String::new();
    for w in text.split_whitespace() {
        if out.len() > 420 {
            out.push('…');
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}

/// Mostly words, not numbers or symbols: half its visible characters letters, and a few
/// real words.
pub fn wordy(s: &str) -> bool {
    let (mut letters, mut visible) = (0usize, 0usize);
    for c in s.chars().filter(|c| !c.is_whitespace()) {
        visible += 1;
        if c.is_alphabetic() {
            letters += 1;
        }
    }
    // Real writing uses many different words; a log or a table repeats a few ("frame",
    // "pts", "scene_score" a thousand times).
    let words: Vec<String> = s.split(|c: char| !c.is_alphabetic()).filter(|w| w.chars().count() >= 3).map(str::to_lowercase).collect();
    let mut distinct: Vec<&str> = words.iter().map(String::as_str).collect();
    distinct.sort_unstable();
    distinct.dedup();
    letters * 2 >= visible && distinct.len() >= 6 && distinct.len() * 4 >= words.len()
}

/// Every word of `text` once, lowercase, space-separated (at most a few thousand).
fn vocabulary(text: &str) -> String {
    let mut words: Vec<String> = text.split(|c: char| !c.is_alphanumeric()).filter(|w| (2..=40).contains(&w.chars().count())).map(str::to_lowercase).collect();
    words.sort_unstable();
    words.dedup();
    words.truncate(6000);
    words.join(" ")
}

/// The words of a question that are worth matching as written: lowercase, two letters
/// or more, without the small words every question has or the words that say what kind
/// of file is wanted ("photo", "pdf"), which describe it rather than what is in it.
pub fn terms(query: &str) -> Vec<String> {
    const SKIP: &[&str] = &[
        "a", "an", "the", "of", "in", "on", "at", "to", "for", "with", "and", "or", "my", "me", "that", "this", "these", "those", "from", "about", "where", "what", "which", "who",
        "when", "how", "why", "is", "was", "are", "were", "be", "it", "its", "some", "any", "find", "show", "file", "files", "photo", "photos", "picture", "pictures", "pic", "pics",
        "image", "images", "screenshot", "screenshots", "pdf", "pdfs", "document", "documents", "doc", "docs", "there", "have", "has", "had", "our", "your", "their", "can", "not",
        "but", "all", "i", "we", "you",
    ];
    let mut v: Vec<String> = vec![];
    for w in query.split(|c: char| !c.is_alphanumeric()).map(str::to_lowercase) {
        if w.chars().count() >= 2 && !SKIP.contains(&w.as_str()) && !v.contains(&w) {
            v.push(w);
        }
    }
    v
}

/// Changed whenever what goes into an index changes (5: the full 768 numbers, scaled per
/// fingerprint; documents only): an older index is read again from the start.
const MAGIC: &[u8; 8] = b"MAIAMNG5";

fn save(path: &Path, idx: &Index) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let mut b: Vec<u8> = Vec::with_capacity(idx.vecs.len() + idx.files.len() * 96);
    b.extend_from_slice(MAGIC);
    b.extend_from_slice(&(DIMS as u32).to_le_bytes());
    b.extend_from_slice(&(idx.files.len() as u32).to_le_bytes());
    for f in &idx.files {
        let p = f.path.as_os_str().as_bytes();
        b.extend_from_slice(&(p.len() as u32).to_le_bytes());
        b.extend_from_slice(p);
        b.extend_from_slice(&f.mtime.to_le_bytes());
        b.extend_from_slice(&f.size.to_le_bytes());
        b.extend_from_slice(&(f.passages.len() as u32).to_le_bytes());
        for &(a, n) in &f.passages {
            b.extend_from_slice(&a.to_le_bytes());
            b.extend_from_slice(&n.to_le_bytes());
        }
        b.extend(idx.vecs[f.first * DIMS..(f.first + f.passages.len()) * DIMS].iter().map(|&v| v as u8));
        for s in f.previews.iter().chain([&f.words]) {
            b.extend_from_slice(&(s.len() as u32).to_le_bytes());
            b.extend_from_slice(s.as_bytes());
        }
    }
    std::fs::create_dir_all(path.parent().unwrap_or(Path::new("/")))?;
    let tmp = path.with_extension("new");
    std::fs::write(&tmp, &b)?;
    std::fs::rename(&tmp, path)
}

/// The saved index, or an empty one when there is none (or it is from another version).
pub fn load(path: &Path) -> Index {
    use std::os::unix::ffi::OsStrExt;
    let Ok(b) = std::fs::read(path) else { return Index::default() };
    let mut at = 0usize;
    let mut take = |n: usize| -> Option<&[u8]> {
        let s = b.get(at..at + n)?;
        at += n;
        Some(s)
    };
    let mut parse = || -> Option<Index> {
        if take(8)? != MAGIC || u32::from_le_bytes(take(4)?.try_into().ok()?) as usize != DIMS {
            return None;
        }
        let n = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
        let mut idx = Index::default();
        for _ in 0..n {
            let pl = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
            let path = PathBuf::from(std::ffi::OsStr::from_bytes(take(pl)?));
            let mtime = i64::from_le_bytes(take(8)?.try_into().ok()?);
            let size = u64::from_le_bytes(take(8)?.try_into().ok()?);
            let np = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
            let mut passages = Vec::with_capacity(np.min(MAX_PASSAGES));
            for _ in 0..np {
                let a = u32::from_le_bytes(take(4)?.try_into().ok()?);
                let l = u32::from_le_bytes(take(4)?.try_into().ok()?);
                passages.push((a, l));
            }
            let first = idx.vecs.len() / DIMS;
            idx.vecs.extend(take(np * DIMS)?.iter().map(|&v| v as i8));
            let mut previews = Vec::with_capacity(np.min(MAX_PASSAGES));
            for _ in 0..np {
                let l = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
                previews.push(String::from_utf8_lossy(take(l)?).into_owned());
            }
            let l = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
            let words = String::from_utf8_lossy(take(l)?).into_owned();
            idx.files.push(FileRec { path, mtime, size, passages, previews, first, words });
        }
        Some(idx)
    };
    parse().unwrap_or_default()
}
