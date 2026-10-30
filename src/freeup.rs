//! Free up space, Space used's second view: what under its folder could go to make room on
//! the disk. Build output and installed packages (found as Clean Build Folders finds them),
//! the download caches of a few tools, the thumbnail cache and the trash, found and sized
//! on a thread. Chosen ones are deleted for good by the usual job once confirmed: the trash
//! is on the same disk, so moving them there would free nothing.
//!
//! Also a record of how full each disk was, one reading a day, taken when Maia starts and
//! when Space used reads a disk (never on a timer). Once it covers a week the view draws
//! the last thirty days over its list.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, mpsc};
use std::time::SystemTime;

use egui::{Color32, CornerRadius, FontId, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

use crate::theme::Palette;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Builds,
    Packages,
    Caches,
    Thumbs,
    Trash,
}

impl Group {
    const ALL: [Group; 5] = [Group::Builds, Group::Packages, Group::Caches, Group::Thumbs, Group::Trash];

    pub fn title(self) -> &'static str {
        match self {
            Group::Builds => "Build output",
            Group::Packages => "Installed packages",
            Group::Caches => "Download caches",
            Group::Thumbs => "Thumbnails",
            Group::Trash => "Trash",
        }
    }

    /// What deleting it means, said after what it holds.
    fn then(self) -> &'static str {
        match self {
            Group::Builds => "a build makes it again",
            Group::Packages => "an install brings them back",
            Group::Caches => "downloaded again when needed",
            Group::Thumbs => "made again as pictures are shown",
            Group::Trash => "gone for good",
        }
    }

    /// Checked when found: what comes back by itself. Packages take an install to bring
    /// back and the trash never comes back, so they wait to be chosen.
    pub fn ticked(self) -> bool {
        matches!(self, Group::Builds | Group::Caches | Group::Thumbs)
    }

    fn icon(self) -> &'static str {
        match self {
            Group::Builds => "code",
            Group::Packages => "list-tree",
            Group::Caches => "download",
            Group::Thumbs => "image",
            Group::Trash => "trash-2",
        }
    }
}

/// The group of a folder Clean Build Folders finds, by its kind.
pub fn group_of(kind: &str) -> Group {
    match kind {
        "Node packages" | "Python environment" | "Python test environments" => Group::Packages,
        _ => Group::Builds,
    }
}

/// Download caches that a tool fills and fetches again when it needs them: in the cache
/// folder (true) or the home folder, where, and what it is.
const CACHES: &[(bool, &str, &str)] = &[
    (true, "pip", "pip downloads"),
    (true, "yarn", "Yarn downloads"),
    (true, "go-build", "Go build cache"),
    (true, "paru/clone", "paru builds"),
    (true, "yay", "yay builds"),
    (false, ".npm/_cacache", "npm downloads"),
    (false, ".cargo/registry/cache", "Cargo downloads"),
    (false, ".gradle/caches", "Gradle downloads"),
];

/// Where the things that are always in the same place are: the home and cache folders,
/// and every trash folder.
pub struct Places {
    pub home: PathBuf,
    pub cache: PathBuf,
    pub trashes: Vec<PathBuf>,
}

impl Places {
    /// This user's. Reads the mounts for the drives' trash folders: on a thread.
    fn here() -> Places {
        let home = crate::fsx::home();
        Places { cache: dirs::cache_dir().unwrap_or_else(|| home.join(".cache")), trashes: crate::trashcan::trash_dirs(), home }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub group: Group,
    pub path: PathBuf,
    /// What the row says: a project's folder, or what a cache is.
    pub name: String,
    /// Said dim after it: its kind, or where a cache is.
    pub note: String,
    /// A trash's entries as the scan saw them; see `Part::items`.
    pub items: Vec<(std::ffi::OsString, bool)>,
}

fn writable(p: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()) else { return false };
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

/// Where Free up's walks go from `root`: the home folder when `root` holds it (the rest of
/// a system disk holds no one's projects), else `root`.
pub fn reach(root: &Path, home: &Path) -> PathBuf {
    if home.starts_with(root) { home.to_path_buf() } else { root.to_path_buf() }
}

/// Everything under `root` that could go, handed to `found` as it turns up: the trash, the
/// thumbnails and the download caches first, then build output from a walk. Only folders
/// inside `root`, on its disk, that this user may delete: never the system's own (a package
/// manager's node_modules under /usr). The walk goes where `reach` says.
/// The `.trashinfo` file that goes with the trashed entry `name`.
fn info_name(name: &std::ffi::OsStr) -> std::ffi::OsString {
    let mut n = name.to_os_string();
    n.push(".trashinfo");
    n
}

pub fn find(root: &Path, places: &Places, stop: &AtomicBool, found: &mut dyn FnMut(Found)) {
    use std::os::unix::fs::MetadataExt;
    let Ok(dev) = std::fs::metadata(root).map(|m| m.dev()) else { return };
    let on_disk = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir() && m.dev() == dev);
    let here = |p: &Path| p.starts_with(root) && on_disk(p) && p.parent().is_some_and(writable);
    let short = |p: &Path| crate::popups::home_short(p);
    // Found already, and the trash: the walk leaves what is inside them alone.
    let mut taken: Vec<PathBuf> = places.trashes.clone();
    for t in &places.trashes {
        if !(t.starts_with(root) && on_disk(t) && writable(t)) {
            continue;
        }
        let entries = std::fs::read_dir(t.join("files")).map(|rd| rd.flatten().map(|e| e.file_name()).collect::<Vec<_>>()).unwrap_or_default();
        let items: Vec<(std::ffi::OsString, bool)> = entries.into_iter().map(|n| {
            let info = t.join("info").join(info_name(&n)).is_file();
            (n, info)
        }).collect();
        if let n @ 1.. = items.len() {
            let note = if n == 1 { "1 item".to_string() } else { format!("{} items", crate::table::fmt_count(n)) };
            found(Found { group: Group::Trash, path: t.clone(), name: short(t), note, items });
        }
    }
    let thumbs = places.cache.join("thumbnails");
    if here(&thumbs) {
        found(Found { group: Group::Thumbs, name: short(&thumbs), note: String::new(), path: thumbs.clone(), items: vec![] });
    }
    taken.push(thumbs);
    for &(in_cache, rel, what) in CACHES {
        let p = if in_cache { places.cache.join(rel) } else { places.home.join(rel) };
        if here(&p) {
            found(Found { group: Group::Caches, name: what.into(), note: short(&p), path: p.clone(), items: vec![] });
        }
        taken.push(p);
    }
    let walk = reach(root, &places.home);
    if on_disk(&walk) {
        crate::buildclean::walk(&walk, stop, &mut |f| {
            if !taken.iter().any(|t| f.path.starts_with(t)) && f.path.parent().is_some_and(writable) {
                found(Found { group: group_of(f.kind), name: short(&f.path), note: f.kind.to_string(), path: f.path, items: vec![] });
            }
        });
    }
}

pub enum Msg {
    Found(Found),
    Size(PathBuf, u64),
    /// The disk's record, read from the file.
    Days(Vec<Day>),
    Done,
}

/// A scan in progress; dropping it stops it.
pub struct Scan {
    rx: mpsc::Receiver<Msg>,
    stop: Arc<AtomicBool>,
    /// Bumped to stop the size walks (they check it as they go).
    generation: Arc<AtomicU64>,
}

impl Drop for Scan {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        self.generation.fetch_add(1, Relaxed);
    }
}

impl Scan {
    /// Look under `root`; `places` none for this user's own, and the disk's record read
    /// from `book`.
    fn start(root: PathBuf, places: Option<Places>, book: PathBuf, ctx: &egui::Context) -> Scan {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let generation = Arc::new(AtomicU64::new(0));
        let (st, gen2, ctx) = (stop.clone(), generation.clone(), ctx.clone());
        let _ = std::thread::Builder::new().name("free up".into()).spawn(move || {
            let _ = tx.send(Msg::Days(days_in(&book, &crate::trashcan::topdir(&root))));
            ctx.request_repaint();
            let places = places.unwrap_or_else(Places::here);
            let (size_tx, size_rx) = mpsc::channel::<PathBuf>();
            let size_rx = Mutex::new(size_rx);
            let g = gen2.load(Relaxed);
            std::thread::scope(|s| {
                for _ in 0..2 {
                    let (tx, ctx, size_rx, gen2) = (tx.clone(), ctx.clone(), &size_rx, &gen2);
                    s.spawn(move || {
                        loop {
                            let Some(p) = size_rx.lock().ok().and_then(|r| r.recv().ok()) else { return };
                            if gen2.load(Relaxed) != g {
                                continue;
                            }
                            let size = crate::stats::compute(&p, gen2, g).size;
                            if gen2.load(Relaxed) == g {
                                let _ = tx.send(Msg::Size(p, size));
                                ctx.request_repaint();
                            }
                        }
                    });
                }
                find(&root, &places, &st, &mut |f| {
                    let _ = size_tx.send(f.path.clone());
                    let _ = tx.send(Msg::Found(f));
                    ctx.request_repaint();
                });
                drop(size_tx);
            });
            let _ = tx.send(Msg::Done);
            ctx.request_repaint();
        });
        Scan { rx, stop, generation }
    }
}

// ------------------------------------------------------------------ The record

/// One day's reading of a disk: the day (since the epoch, local time), bytes used and the
/// disk's size.
pub type Day = (i64, u64, u64);

/// Days kept for each disk: a little over a year.
const KEEP: usize = 400;

/// A reading taken on the same day replaces the day's only when it moved this much: most
/// starts change nothing, and nothing is written for them.
const MOVED: u64 = 64 << 20;

pub fn book_path() -> PathBuf {
    crate::config::data_dir().join("space-history")
}

/// One process writes the record at a time; another Maia losing a day's reading to a race
/// costs nothing.
static BOOK: Mutex<()> = Mutex::new(());

/// The record: a line a reading, `day used total disk`, the disk by its mount point.
fn read_book(file: &Path) -> Vec<(i64, u64, u64, String)> {
    let s = std::fs::read_to_string(file).unwrap_or_default();
    s.lines()
        .filter_map(|l| {
            let mut it = l.splitn(4, '\t');
            Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?.to_string()))
        })
        .collect()
}

/// Note in `file` that the disk mounted at `top` has `used` of `total` bytes in use on `day`.
pub fn note_in(file: &Path, top: &Path, used: u64, total: u64, day: i64) {
    let _g = BOOK.lock();
    let key = top.to_string_lossy().into_owned();
    if key.contains('\n') {
        return;
    }
    let mut all = read_book(file);
    let last = all.iter().rposition(|r| r.3 == key);
    match last.map(|i| &mut all[i]) {
        Some(r) if r.0 == day && r.1.abs_diff(used) < MOVED => return,
        Some(r) if r.0 == day => (r.1, r.2) = (used, total),
        // The clock went back: the record stays as it was.
        Some(r) if r.0 > day => return,
        _ => all.push((day, used, total, key.clone())),
    }
    let n = all.iter().filter(|r| r.3 == key).count();
    if n > KEEP {
        let mut drop = n - KEEP;
        all.retain(|r| {
            let gone = drop > 0 && r.3 == key;
            drop -= gone as usize;
            !gone
        });
    }
    let text: String = all.iter().map(|r| format!("{}\t{}\t{}\t{}\n", r.0, r.1, r.2, r.3)).collect();
    if let Some(d) = file.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = file.with_extension(format!("tmp{}", std::process::id()));
    if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, file).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// The record of the disk mounted at `top`, oldest first.
pub fn days_in(file: &Path, top: &Path) -> Vec<Day> {
    let key = top.to_string_lossy();
    read_book(file).into_iter().filter(|r| r.3 == key).map(|r| (r.0, r.1, r.2)).collect()
}

/// Today's reading of `dir`'s disk, taken on a thread. Test builds write only in the
/// simulations' sandbox, never the real data folder.
pub fn note(dir: &Path, total: u64, free: u64) {
    if cfg!(test) && std::env::var_os("MAIA_SIM_SANDBOX").is_none() {
        return;
    }
    note_in(&book_path(), &crate::trashcan::topdir(dir), total.saturating_sub(free), total, crate::timefmt::local_day(SystemTime::now()));
}

/// At start: today's reading of the home folder's disk, on a thread of its own.
pub fn note_home_later() {
    let _ = std::thread::Builder::new().name("disk record".into()).spawn(|| {
        let home = crate::fsx::home();
        if let Some((total, free)) = crate::fsx::disk_space(&home) {
            note(&home, total, free);
        }
    });
}

/// Days a record must reach back before it is drawn: a week.
const WEEK: i64 = 6;
/// The days the chart shows, up to today.
const SPAN: i64 = 30;

/// Whether the record is worth a chart today: three readings at least, the first a week
/// back or more.
pub fn charted(days: &[Day], today: i64) -> bool {
    days.len() >= 3 && days.first().is_some_and(|d| today - d.0 >= WEEK)
}

/// How the bytes in use moved over the last week: from the reading a week back (or the
/// first after it, when that day was missed) to the newest. The days it covers, and the
/// change.
pub fn week_change(days: &[Day], today: i64) -> Option<(i64, i64)> {
    let last = days.last()?;
    let from = days.iter().rev().find(|d| d.0 <= today - 7).or_else(|| days.iter().find(|d| d.0 >= today - 7))?;
    (from.0 < last.0).then(|| (last.0 - from.0, last.1 as i64 - from.1 as i64))
}

// ------------------------------------------------------------------ The view

pub struct Part {
    pub path: PathBuf,
    pub name: String,
    pub note: String,
    pub size: Option<u64>,
    pub checked: bool,
    /// A trash's entries when it was scanned (a name in its files folder, and whether a
    /// .trashinfo went with it). Only these are deleted: something trashed after the
    /// scan was never shown or counted, so it stays.
    pub items: Vec<(std::ffi::OsString, bool)>,
}

pub struct Grp {
    pub group: Group,
    pub parts: Vec<Part>,
    /// Its folders listed under it.
    pub open: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Row {
    Group(usize),
    Part(usize, usize),
    /// Find Duplicates, for files that are copies of each other: Maia can't tell which to
    /// keep, so the person picks there.
    Dupes,
}

pub struct View {
    pub root: PathBuf,
    scan: Option<Scan>,
    pub groups: Vec<Grp>,
    /// The record of this disk, once read.
    pub days: Vec<Day>,
    /// The row with the cursor, a place in `rows()`.
    pub sel: usize,
    /// Asking whether to delete what is checked.
    pub confirm: bool,
    pub done: bool,
    /// What a delete just started frees, until the disk is read again after it.
    pub freeing: Option<u64>,
    /// Said under the list in place of the totals, until the next key.
    pub note: Option<String>,
    pub reveal: bool,
}

impl View {
    pub fn new(root: PathBuf, ctx: &egui::Context) -> View {
        View::new_in(root, None, book_path(), ctx)
    }

    /// `new`, with the places and the record given (tests).
    pub fn new_in(root: PathBuf, places: Option<Places>, book: PathBuf, ctx: &egui::Context) -> View {
        let scan = Scan::start(root.clone(), places, book, ctx);
        View { root, scan: Some(scan), groups: vec![], days: vec![], sel: 0, confirm: false, done: false, freeing: None, note: None, reveal: false }
    }

    /// Take in what the scan sent.
    pub fn pump(&mut self) {
        let Some(s) = &self.scan else { return };
        let mut done = false;
        while let Ok(m) = s.rx.try_recv() {
            match m {
                Msg::Days(d) => self.days = d,
                Msg::Found(f) => {
                    let gi = match self.groups.iter().position(|g| g.group == f.group) {
                        Some(i) => i,
                        None => {
                            // Groups stand in one order, whichever turns up first.
                            let at = self.groups.iter().position(|g| Group::ALL.iter().position(|&x| x == g.group) > Group::ALL.iter().position(|&x| x == f.group)).unwrap_or(self.groups.len());
                            self.groups.insert(at, Grp { group: f.group, parts: vec![], open: false });
                            at
                        }
                    };
                    self.groups[gi].parts.push(Part { checked: f.group.ticked(), path: f.path, name: f.name, note: f.note, size: None, items: f.items });
                }
                Msg::Size(p, n) => {
                    if let Some(part) = self.groups.iter_mut().flat_map(|g| g.parts.iter_mut()).find(|x| x.path == p) {
                        part.size = Some(n);
                    }
                }
                Msg::Done => done = true,
            }
        }
        if done {
            // Biggest first once every size is in; the cursor stays on its row.
            let at = self.rows().get(self.sel).copied().map(|r| self.key(r));
            for g in &mut self.groups {
                g.parts.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.path.cmp(&b.path)));
            }
            self.groups.retain(|g| g.group != Group::Trash || g.parts.iter().any(|p| p.size.is_some_and(|s| s > 0)));
            self.done = true;
            self.scan = None;
            if let Some(k) = at {
                self.sel = self.rows().iter().position(|&r| self.key(r) == k).unwrap_or(0);
            }
        }
    }

    /// What a row stands for, to find it again once the rows move.
    fn key(&self, r: Row) -> (Option<Group>, Option<PathBuf>) {
        match r {
            Row::Group(g) => (Some(self.groups[g].group), None),
            Row::Part(g, p) => (Some(self.groups[g].group), Some(self.groups[g].parts[p].path.clone())),
            Row::Dupes => (None, None),
        }
    }

    pub fn rows(&self) -> Vec<Row> {
        let mut v = Vec::with_capacity(self.groups.len() + 1);
        for (gi, g) in self.groups.iter().enumerate() {
            v.push(Row::Group(gi));
            if g.open && g.parts.len() > 1 {
                v.extend((0..g.parts.len()).map(|p| Row::Part(gi, p)));
            }
        }
        if self.done {
            v.push(Row::Dupes);
        }
        v
    }

    pub fn scanning(&self) -> bool {
        self.scan.is_some()
    }

    /// Sizes still to come.
    pub fn counting(&self) -> usize {
        self.groups.iter().flat_map(|g| &g.parts).filter(|p| p.size.is_none()).count()
    }

    /// Something is checked, every checked folder counted, and no delete running.
    pub fn ready(&self) -> bool {
        let checked = || self.groups.iter().flat_map(|g| &g.parts).filter(|p| p.checked);
        self.freeing.is_none() && checked().next().is_some() && checked().all(|p| p.size.is_some())
    }

    pub fn chosen(&self) -> u64 {
        self.groups.iter().flat_map(|g| &g.parts).filter(|p| p.checked).filter_map(|p| p.size).sum()
    }

    pub fn found(&self) -> u64 {
        self.groups.iter().flat_map(|g| &g.parts).filter_map(|p| p.size).sum()
    }

    /// Check or clear a row: a group all of its folders at once.
    pub fn flip(&mut self, r: Row) {
        match r {
            Row::Group(g) => {
                let g = &mut self.groups[g];
                let on = !g.parts.iter().all(|p| p.checked);
                for p in &mut g.parts {
                    p.checked = on;
                }
            }
            Row::Part(g, p) => self.groups[g].parts[p].checked ^= true,
            Row::Dupes => {}
        }
    }

    /// Open or close the group of `r`, the cursor staying on the group.
    pub fn open(&mut self, r: Row, open: bool) {
        let (Row::Group(g) | Row::Part(g, _)) = r else { return };
        if self.groups[g].parts.len() < 2 {
            return;
        }
        self.groups[g].open = open;
        self.sel = self.rows().iter().position(|&x| x == Row::Group(g)).unwrap_or(0);
        self.reveal = true;
    }

    /// The checked folders, taken out of the list: what the delete is given (a trash's
    /// entries as scanned, each with its .trashinfo; never the trash itself), and the
    /// bytes they hold.
    pub fn take_chosen(&mut self) -> (Vec<PathBuf>, u64) {
        let (mut paths, mut size) = (vec![], 0);
        for g in &mut self.groups {
            for p in g.parts.iter().filter(|p| p.checked) {
                size += p.size.unwrap_or(0);
                if g.group == Group::Trash {
                    for (name, info) in &p.items {
                        paths.push(p.path.join("files").join(name));
                        if *info {
                            paths.push(p.path.join("info").join(info_name(name)));
                        }
                    }
                } else {
                    paths.push(p.path.clone());
                }
            }
            g.parts.retain(|p| !p.checked);
        }
        self.groups.retain(|g| !g.parts.is_empty());
        self.sel = self.sel.min(self.rows().len().saturating_sub(1));
        self.confirm = false;
        (paths, size)
    }

    /// What the footer says.
    pub fn status(&self) -> (String, bool) {
        let size = crate::fsx::fmt_size;
        if self.confirm {
            let trash = self.groups.iter().any(|g| g.group == Group::Trash && g.parts.iter().any(|p| p.checked));
            let then = if trash { "What is in the trash can't be brought back." } else { "A build, an install or a download makes it again." };
            return (format!("Delete {} for good? {then}", size(self.chosen())), true);
        }
        if let Some(n) = self.freeing {
            return (format!("Freeing {}…", size(n)), false);
        }
        if let Some(t) = &self.note {
            return (t.clone(), false);
        }
        let n: usize = self.groups.iter().map(|g| g.parts.len()).sum();
        if self.scanning() && n == 0 {
            return ("Looking for what can go…".into(), false);
        }
        if self.scanning() {
            return (format!("Looking… {} found", crate::table::fmt_count(n)), false);
        }
        if self.counting() > 0 {
            return (format!("Counting {} folders…", crate::table::fmt_count(self.counting())), false);
        }
        if n == 0 {
            return ("Nothing here that comes back by itself".into(), false);
        }
        let chosen = self.chosen();
        if chosen == 0 {
            (format!("{} found, none chosen", size(self.found())), false)
        } else {
            (format!("{} chosen of {}", size(chosen), size(self.found())), false)
        }
    }
}

/// What the view is drawn with.
pub struct Look<'a> {
    pub fs: f32,
    pub pal: &'a Palette,
    pub now: f64,
    /// The disk's size and free bytes.
    pub disk: Option<(u64, u64)>,
    /// The colour of the disk once the chosen are gone.
    pub after: Color32,
}

/// The rows' heights: a group's two lines, and a folder's one.
fn heights(fs: f32) -> (f32, f32) {
    let small = fs - 1.5;
    ((fs * 1.35 + small * 1.35 + 14.0).round(), (fs * 1.35 + 12.0).round())
}

fn chart_height(fs: f32) -> f32 {
    let small = fs - 1.5;
    (small * 1.35 * 2.0 + 84.0).round()
}

fn bars_height(fs: f32) -> f32 {
    let small = fs - 1.5;
    ((small * 1.35).max(10.0) + 16.0).round()
}

/// Space between the chart, the list and the bars.
const GAP: f32 = 16.0;

/// The rows' height, all of them.
fn rows_height(v: &View, fs: f32) -> f32 {
    let (two, one) = heights(fs);
    v.rows().iter().map(|r| if matches!(r, Row::Part(..)) { one } else { two }).sum::<f32>().max(two)
}

/// Whether the chart fits over `rows` of list in `room`: it gives way to the list once
/// fewer than two rows would show.
fn chart_fits(v: &View, fs: f32, today: i64, room: f32, rows: f32) -> bool {
    charted(&v.days, today) && room >= chart_height(fs) + GAP + rows.min(2.0 * heights(fs).0) + GAP + bars_height(fs)
}

/// The height the view would take with nothing cut short.
pub fn natural(v: &View, fs: f32, today: i64) -> f32 {
    let rows = rows_height(v, fs);
    let chart = if charted(&v.days, today) { chart_height(fs) + GAP } else { 0.0 };
    chart + rows + GAP + bars_height(fs)
}

/// What the person did in the view this frame that its owner acts on.
#[derive(Default)]
pub struct Out {
    /// Find Duplicates was asked for.
    pub dupes: bool,
}

/// The view in `area`: the chart when there is a week of record and room for it, the list,
/// and the disk now and once the chosen are gone.
pub fn draw(ui: &mut Ui, area: Rect, v: &mut View, look: &Look) -> Out {
    let mut out = Out::default();
    let fs = look.fs;
    let (two, one) = heights(fs);
    let today = crate::timefmt::local_day(SystemTime::now());
    let bars_h = bars_height(fs);
    let ch = chart_height(fs);
    let rows_h = rows_height(v, fs);
    let mut top = area.top();
    if chart_fits(v, fs, today, area.height(), rows_h) {
        draw_chart(ui, Rect::from_min_size(area.min, vec2(area.width(), ch)), &v.days, today, look);
        top += ch + GAP;
    }
    // The bars follow the list: filling the window, what is left stays below them.
    let list_h = rows_h.min(area.bottom() - bars_h - GAP - top).max(0.0);
    let list = Rect::from_min_size(pos2(area.left(), top), vec2(area.width(), list_h));
    draw_list(ui, list, v, two, one, look, &mut out);
    let used = look.disk.map(|(t, f)| t.saturating_sub(f));
    if let (Some((total, _)), Some(used)) = (look.disk, used) {
        let chosen = v.freeing.unwrap_or_else(|| v.chosen());
        draw_bars(ui, Rect::from_min_size(pos2(area.left(), list.bottom() + GAP), vec2(area.width(), bars_h)), total, used, chosen, look);
    }
    out
}

/// A box ticked for some of a group's folders: filled, with a dash.
fn mixed_glyph(p: &egui::Painter, r: Rect, pal: &Palette) {
    p.rect_filled(r, CornerRadius::same(3), pal.accent);
    p.line_segment([r.left_center() + vec2(3.5, 0.0), r.right_center() - vec2(3.5, 0.0)], Stroke::new(1.6, Color32::WHITE));
}

fn draw_list(ui: &mut Ui, r: Rect, v: &mut View, two: f32, one: f32, look: &Look, out: &mut Out) {
    let pal = look.pal;
    let (fs, small) = (look.fs, look.fs - 1.5);
    let rows = v.rows();
    let mut lui = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::top_down(egui::Align::Min)));
    let ui = &mut lui;
    ui.spacing_mut().item_spacing.y = 0.0;
    let (mut flip, mut toggle_open, mut select) = (None, None, None);
    egui::ScrollArea::vertical().id_salt("free list").max_height(r.height()).min_scrolled_height(r.height()).auto_shrink([false, false]).show(ui, |ui| {
        let reveal = std::mem::take(&mut v.reveal);
        for (i, &row) in rows.iter().enumerate() {
            let h = if matches!(row, Row::Part(..)) { one } else { two };
            let (rr, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
            crate::ui::note_control(ui, rr, match row {
                Row::Group(_) => "free group",
                Row::Part(..) => "free part",
                Row::Dupes => "free dupes",
            });
            if reveal && i == v.sel {
                ui.scroll_to_rect(rr, None);
            }
            let on = i == v.sel;
            if on {
                ui.painter().rect_filled(rr, CornerRadius::same(6), pal.selected);
            } else if resp.hovered() {
                ui.painter().rect_filled(rr, CornerRadius::same(6), pal.hover);
            }
            let (tc, dc) = if on { (pal.on_sel(), pal.on_sel_dim()) } else { (pal.text, pal.text_dim) };
            let p = ui.painter();
            // Chevron, box and icon in columns, so a folder's box stands under its group's icon.
            let chev = rr.left() + 14.0;
            let cbx = chev + 18.0;
            let icx = cbx + 26.0;
            let cb = Rect::from_center_size(pos2(cbx, rr.center().y), Vec2::splat(14.0));
            let edge = rr.right() - 12.0;
            let at_box = resp.interact_pointer_pos().is_some_and(|q| q.x < cb.right() + 6.0);
            match row {
                Row::Group(gi) => {
                    let g = &v.groups[gi];
                    if g.parts.len() > 1 {
                        let cr = Rect::from_center_size(pos2(chev, rr.center().y), Vec2::splat(12.0));
                        crate::theme::paint_symbol(p, cr, if g.open { "chevron-down" } else { "chevron-right" }, dc);
                    }
                    let n_on = g.parts.iter().filter(|x| x.checked).count();
                    if n_on > 0 && n_on < g.parts.len() {
                        mixed_glyph(p, cb, pal);
                    } else {
                        crate::popups::checkbox_glyph(p, cb, n_on > 0, pal);
                    }
                    crate::theme::paint_symbol(p, Rect::from_center_size(pos2(icx, rr.center().y), Vec2::splat(16.0)), g.group.icon(), dc);
                    let size: Option<u64> = if g.parts.iter().all(|x| x.size.is_some()) { Some(g.parts.iter().filter_map(|x| x.size).sum()) } else { None };
                    let right = size_at(ui, edge, rr.center().y, size, FontId::proportional(fs), tc, dc, look.now);
                    let x = icx + 18.0;
                    let room = right - 16.0 - x;
                    let what = match g.group {
                        Group::Builds | Group::Packages => {
                            let n = g.parts.len();
                            if n == 1 { g.parts[0].name.clone() } else { format!("{} folders", crate::table::fmt_count(n)) }
                        }
                        Group::Caches => {
                            let names: Vec<&str> = g.parts.iter().take(3).map(|x| x.name.as_str()).collect();
                            let more = g.parts.len().saturating_sub(3);
                            if more > 0 { format!("{} and {more} more", names.join(", ")) } else { names.join(", ") }
                        }
                        Group::Thumbs => g.parts[0].name.clone(),
                        Group::Trash => {
                            let notes: Vec<&str> = g.parts.iter().map(|x| x.note.as_str()).collect();
                            notes.join(", ")
                        }
                    };
                    let line1 = (rr.top() + 7.0).round();
                    let tg = crate::listing::elided(ui, g.group.title(), FontId::proportional(fs), room, tc);
                    ui.painter().galley(pos2(x, line1), tg, tc);
                    let dg = crate::listing::elided(ui, &format!("{what} · {}", g.group.then()), FontId::proportional(small), room, dc);
                    ui.painter().galley(pos2(x, line1 + 2.0 + fs * 1.35), dg, dc);
                    if resp.clicked() {
                        select = Some(i);
                        if at_box {
                            flip = Some(row);
                        } else if g.parts.len() > 1 {
                            toggle_open = Some((row, !g.open));
                        }
                    }
                }
                Row::Part(gi, pi) => {
                    let part = &v.groups[gi].parts[pi];
                    let cb = Rect::from_center_size(pos2(icx, rr.center().y), Vec2::splat(14.0));
                    crate::popups::checkbox_glyph(p, cb, part.checked, pal);
                    let right = size_at(ui, edge, rr.center().y, part.size, FontId::proportional(small), dc, dc, look.now);
                    let x = icx + 18.0;
                    let room = right - 16.0 - x;
                    let ng = crate::listing::elided_tail(ui, &part.name, FontId::proportional(fs), room, tc);
                    let nw = ng.size().x;
                    let y = rr.center().y - ng.size().y / 2.0;
                    ui.painter().galley(pos2(x, y), ng, tc);
                    let left = room - nw - 10.0;
                    if left > 40.0 && !part.note.is_empty() {
                        let g = crate::listing::elided(ui, &part.note, FontId::proportional(small), left, dc);
                        ui.painter().galley(pos2(x + nw + 10.0, rr.center().y - g.size().y / 2.0), g, dc);
                    }
                    if resp.clicked() {
                        select = Some(i);
                        if resp.interact_pointer_pos().is_some_and(|q| q.x < cb.right() + 6.0) {
                            flip = Some(row);
                        }
                    }
                }
                Row::Dupes => {
                    crate::theme::paint_symbol(p, Rect::from_center_size(pos2(icx, rr.center().y), Vec2::splat(16.0)), "copy", dc);
                    let fg = ui.painter().layout_no_wrap("Find…".into(), FontId::proportional(small), dc);
                    let right = edge - fg.size().x;
                    ui.painter().galley(pos2(right, rr.center().y - fg.size().y / 2.0), fg, dc);
                    let x = icx + 18.0;
                    let room = right - 16.0 - x;
                    let line1 = (rr.top() + 7.0).round();
                    let tg = crate::listing::elided(ui, "Duplicate files", FontId::proportional(fs), room, tc);
                    ui.painter().galley(pos2(x, line1), tg, tc);
                    let sub = format!("Copies with the same contents in {} · you pick which go", crate::popups::home_short(&reach(&v.root, &crate::fsx::home())));
                    let dg = crate::listing::elided(ui, &sub, FontId::proportional(small), room, dc);
                    ui.painter().galley(pos2(x, line1 + 2.0 + fs * 1.35), dg, dc);
                    if resp.clicked() {
                        select = Some(i);
                        out.dupes = true;
                    }
                }
            }
        }
    });
    if let Some(i) = select {
        v.sel = i;
        v.note = None;
    }
    if let Some(r) = flip {
        v.flip(r);
    }
    if let Some((r, open)) = toggle_open {
        v.open(r, open);
    }
}

/// A size at `edge`, or a spinner while it is counted. Returns where it starts.
#[allow(clippy::too_many_arguments)]
fn size_at(ui: &Ui, edge: f32, y: f32, size: Option<u64>, font: FontId, col: Color32, dim: Color32, now: f64) -> f32 {
    match size {
        Some(s) => {
            let g = ui.painter().layout_no_wrap(crate::fsx::fmt_size(s), font, col);
            let x = edge - g.size().x;
            ui.painter().galley(pos2(x, y - g.size().y / 2.0), g, col);
            x
        }
        None => {
            crate::ui::spinner(ui.painter(), pos2(edge - 5.0, y), 4.5, dim, now);
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            edge - 10.0
        }
    }
}

/// The disk as one bar against its size: what stays solid, what the chosen would free
/// hatched after it, then the free room. Before and after in one place, so the change is
/// the hatched stretch itself rather than the difference between two thin lines.
fn draw_bars(ui: &mut Ui, r: Rect, total: u64, used: u64, chosen: u64, look: &Look) {
    let pal = look.pal;
    let small = look.fs - 1.5;
    let font = FontId::proportional(small);
    let after = used.saturating_sub(chosen);
    let bar = Rect::from_min_max(pos2(r.left() + 12.0, r.top()), pos2(r.right() - 12.0, r.bottom()));
    if bar.width() < 80.0 {
        return;
    }
    let frac = |b: u64| (b as f32 / total.max(1) as f32).clamp(0.0, 1.0);
    let p = ui.painter();
    p.rect(bar, CornerRadius::same(7), pal.card().lerp_to_gamma(pal.text, if pal.dark { 0.05 } else { 0.045 }), Stroke::new(1.0, pal.card_line()), StrokeKind::Inside);
    let inner = bar.shrink(1.0);
    // The stretch eases as boxes are ticked; a sliver still shows on a disk far bigger
    // than what goes.
    let w = inner.width();
    let x_after = inner.left() + crate::kit::slide(ui.ctx(), egui::Id::new("free up after"), w * frac(after));
    let x_used = inner.left() + (w * frac(used)).max(x_after - inner.left() + if chosen > 0 { 3.0 } else { 0.0 });
    let tint = |k: f32| look.after.gamma_multiply(if pal.dark { k } else { k * 0.8 });
    if x_after > inner.left() {
        p.rect_filled(Rect::from_min_max(inner.min, pos2(x_after, inner.bottom())), CornerRadius::same(6), tint(0.45));
    }
    if x_used > x_after + 0.5 {
        let freed = Rect::from_min_max(pos2(x_after, inner.top()), pos2(x_used, inner.bottom()));
        let hp = p.with_clip_rect(freed);
        hp.rect_filled(freed, CornerRadius::ZERO, tint(0.14));
        let h = freed.height();
        let mut x = freed.left() - h;
        while x < freed.right() {
            hp.line_segment([pos2(x, freed.bottom()), pos2(x + h, freed.top())], Stroke::new(1.5, tint(0.55)));
            x += 5.0;
        }
    }
    // Inside it: what is used now, and what it comes to once the chosen are gone.
    let (now, of) = (crate::fsx::fmt_size(used), crate::fsx::fmt_size(total));
    let text = if chosen > 0 { format!("{now} → {} of {of}", crate::fsx::fmt_size(after)) } else { format!("{now} of {of}") };
    let lg = p.layout_no_wrap("Used now".into(), font.clone(), pal.text_dim);
    let vg = crate::listing::tabular(ui, &text, font, bar.width() - lg.size().x - 36.0, pal.text);
    let cy = bar.center().y;
    ui.painter().galley(pos2(bar.left() + 12.0, (cy - lg.size().y / 2.0).round()), lg, pal.text_dim);
    ui.painter().galley(pos2(bar.right() - 12.0 - vg.size().x, (cy - vg.size().y / 2.0).round()), vg, pal.text);
}

/// The last thirty days of the record as a bar a day, scaled between the lowest and highest
/// reading so a few gigabytes show; the scale's ends are written beside it. The week's
/// change over it, or the day pointed at.
fn draw_chart(ui: &mut Ui, r: Rect, days: &[Day], today: i64, look: &Look) {
    let pal = look.pal;
    let small = look.fs - 1.5;
    let font = FontId::proportional(small);
    let line = (small * 1.35).round();
    let first = today - (SPAN - 1);
    let shown: Vec<&Day> = days.iter().filter(|d| d.0 >= first && d.0 <= today).collect();
    let (Some(lo), Some(hi)) = (shown.iter().map(|d| d.1).min(), shown.iter().map(|d| d.1).max()) else { return };
    // A quarter of the spread below the lowest, so its bar still shows. A flat record gets a
    // two-hundredth of the disk: a day's churn on a big disk doesn't fill the chart.
    let size = shown.iter().map(|d| d.2).max().unwrap_or(0);
    let spread = (hi - lo).max(size / 200).max(1 << 20);
    let floor = lo.saturating_sub(spread / 4);
    let ceil = hi + spread / 8;
    let p = ui.painter();
    let ticks = [crate::fsx::fmt_size(ceil), crate::fsx::fmt_size(floor)].map(|t| p.layout_no_wrap(t, font.clone(), pal.text_dim));
    let tw = ticks.iter().map(|g| g.size().x).fold(0.0, f32::max);
    let plot = Rect::from_min_max(pos2(r.left() + 12.0 + tw + 12.0, r.top() + line + 10.0), pos2(r.right() - 12.0, r.bottom() - line - 6.0));
    if plot.width() < 60.0 || plot.height() < 20.0 {
        return;
    }
    let slot = plot.width() / SPAN as f32;
    let bar_w = (slot * 0.62).clamp(2.0, 18.0);
    let resp = ui.interact(plot, egui::Id::new("free chart"), Sense::hover());
    crate::ui::note_control(ui, plot, "free chart");
    let hover = resp.hover_pos().map(|q| first + ((q.x - plot.left()) / slot).floor() as i64).and_then(|d| shown.iter().find(|x| x.0 == d).copied());
    let p = ui.painter();
    let y_of = |b: u64| plot.bottom() - plot.height() * ((b.saturating_sub(floor)) as f32 / (ceil - floor).max(1) as f32);
    for (i, g) in ticks.into_iter().enumerate() {
        let y = if i == 0 { plot.top() } else { plot.bottom() };
        p.galley(pos2(r.left() + 12.0 + tw - g.size().x, y - g.size().y / 2.0), g, pal.text_dim);
        p.line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], Stroke::new(1.0, pal.border.gamma_multiply(0.6)));
    }
    for d in &shown {
        let cx = plot.left() + slot * ((d.0 - first) as f32 + 0.5);
        let y = y_of(d.1).min(plot.bottom() - 2.0);
        let b = Rect::from_min_max(pos2((cx - bar_w / 2.0).round(), y.round()), pos2((cx + bar_w / 2.0).round(), plot.bottom()));
        let lit = hover.is_some_and(|h| h.0 == d.0) || hover.is_none() && d.0 == today;
        let col = if lit { pal.accent } else { pal.accent.lerp_to_gamma(pal.raised, 0.45) };
        p.rect_filled(b, CornerRadius { nw: 2, ne: 2, sw: 0, se: 0 }, col);
    }
    // The caption: what the bars are, and the week's change or the day pointed at.
    let left = p.layout_no_wrap("Used, last 30 days".into(), font.clone(), pal.text_dim);
    p.galley(pos2(plot.left(), r.top()), left, pal.text_dim);
    let right = match hover {
        Some(d) => format!("{}: {} used", crate::timefmt::format(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs((d.0 * 86_400 + 43_200).max(0) as u64), "%b %-d"), crate::fsx::fmt_size(d.1)),
        None => match week_change(days, today) {
            Some((n, ch)) => format!("{}{} in {n} {}", if ch < 0 { "−" } else { "+" }, crate::fsx::fmt_size(ch.unsigned_abs()), if n == 1 { "day" } else { "days" }),
            None => String::new(),
        },
    };
    let rg = p.layout_no_wrap(right, font.clone(), pal.text);
    p.galley(pos2(plot.right() - rg.size().x, r.top()), rg, pal.text);
    let from = crate::timefmt::format(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs((first * 86_400 + 43_200).max(0) as u64), "%b %-d");
    let fg = p.layout_no_wrap(from, font.clone(), pal.text_dim);
    p.galley(pos2(plot.left(), plot.bottom() + 6.0), fg, pal.text_dim);
    let tg = p.layout_no_wrap("Today".into(), font, pal.text_dim);
    p.galley(pos2(plot.right() - tg.size().x, plot.bottom() + 6.0), tg, pal.text_dim);
}
