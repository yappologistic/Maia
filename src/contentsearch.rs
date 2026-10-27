//! The filter's contents mode: the query is looked for inside files instead of in their
//! names. Files are read on worker threads and matches stream back with the first line
//! that contains the query; a new query, folder or hidden setting starts over.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

use crate::fsx::{self, Entry};
use crate::tab::Tab;

/// Bigger files are skipped: reading a 4 GB disk image to find a word is never what was meant.
pub const MAX_BYTES: u64 = 32 << 20;

/// Matching paths and the line each one matched on.
pub type Hits = HashMap<PathBuf, String>;

#[derive(Default)]
pub struct State {
    /// Contents mode is switched on for this tab.
    pub on: bool,
    /// What the current matches were searched for: query, folder, flattened, hidden files.
    key: Option<(String, PathBuf, bool, bool, usize)>,
    hits: Hits,
    run: Option<Arc<Run>>,
}

#[derive(Default)]
struct Run {
    found: Mutex<Vec<(PathBuf, String)>>,
    cancel: AtomicBool,
    done: AtomicBool,
}

impl State {
    /// Lend the matches to a view rebuild (None: contents mode isn't filtering).
    pub fn take(&mut self) -> Option<Hits> {
        self.key.as_ref()?;
        Some(std::mem::take(&mut self.hits))
    }

    pub fn put(&mut self, h: Option<Hits>) {
        if let Some(h) = h {
            self.hits = h;
        }
    }

    pub fn busy(&self) -> bool {
        self.run.is_some()
    }

    pub fn snippet(&self, p: &Path) -> Option<&str> {
        self.key.as_ref().and(self.hits.get(p)).map(String::as_str)
    }
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some(r) = &self.run {
            r.cancel.store(true, Relaxed);
        }
    }
}

/// Keep the tab's search in step with its filter, folder and the hidden-files setting,
/// and take in what the workers found. Called every frame; allocates only on a change.
pub fn pump(t: &mut Tab, hidden: bool) {
    let name = crate::query::name_part(&t.filter);
    let q = name.trim();
    let active = t.content.on && !q.is_empty() && !crate::tab::is_place(&t.path);
    let same = match &t.content.key {
        None => !active,
        Some(k) => active && k.0 == q && k.1 == t.path && k.2 == t.flatten && k.3 == hidden && k.4 == t.expanded.len(),
    };
    if !same {
        let key = active.then(|| (q.to_string(), t.path.clone(), t.flatten, hidden, t.expanded.len()));
        // Tree view also looks inside the folders unfolded below this one.
        let dirs: Vec<PathBuf> = if t.tree_active() { t.expanded.iter().filter(|d| d.starts_with(&t.path)).cloned().collect() } else { vec![] };
        let st = &mut t.content;
        if let Some(r) = st.run.take() {
            r.cancel.store(true, Relaxed);
        }
        st.run = key.as_ref().map(|k| start(&k.0, k.1.clone(), dirs, k.2, hidden, t.ctx.clone()));
        st.hits.clear();
        st.key = key;
        t.dirty = true;
    }
    let st = &mut t.content;
    if let Some(r) = &st.run {
        // Read `done` first: everything found before it is then already in the list.
        let fin = r.done.load(Relaxed);
        let got = r.found.lock().map(|mut f| std::mem::take(&mut *f)).unwrap_or_default();
        if !got.is_empty() {
            st.hits.extend(got);
            t.stream_dirty = true;
        }
        if fin {
            st.run = None;
            t.dirty = true;
        }
    }
}

/// Flip contents mode (the palette command; the filter's toggle does the same).
pub fn toggle(p: &mut crate::panel::Panel) {
    let t = p.tab_mut();
    t.content.on = !t.content.on;
    t.dirty = true;
    p.focus_filter = true;
}

fn start(q: &str, root: PathBuf, dirs: Vec<PathBuf>, flat: bool, hidden: bool, ctx: egui::Context) -> Arc<Run> {
    let run = Arc::new(Run::default());
    let r = run.clone();
    let needle = q.to_lowercase();
    std::thread::spawn(move || {
        // procfs and sysfs files block or never end when read.
        if !fsx::is_virtual_fs(&root) {
            let ok = |e: &&Entry| !e.is_dir && !e.special && e.size > 0 && e.size <= MAX_BYTES && (hidden || !(e.hidden || e.rel_hidden()));
            let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, 4);
            // Each batch of the listing (or of the recursive walk, as it streams in) is
            // shared out among the workers.
            let mut each = |batch: Vec<Entry>| {
                let files: Vec<&Entry> = batch.iter().filter(ok).collect();
                let next = AtomicUsize::new(0);
                std::thread::scope(|s| {
                    for _ in 0..workers {
                        s.spawn(|| {
                            let mut buf = vec![0u8; 64 << 10];
                            while let Some(e) = files.get(next.fetch_add(1, Relaxed)) {
                                if r.cancel.load(Relaxed) {
                                    return;
                                }
                                if let Some(line) = find(&e.path, &needle, &mut buf, &r.cancel) {
                                    if let Ok(mut f) = r.found.lock() {
                                        f.push((e.path.clone(), line));
                                    }
                                    ctx.request_repaint();
                                }
                            }
                        });
                    }
                });
                !r.cancel.load(Relaxed)
            };
            if flat {
                let w = fsx::walk_async(root, hidden, ctx.clone());
                while let Ok(b) = w.rx.recv() {
                    if !each(b) {
                        break;
                    }
                }
            } else {
                for d in std::iter::once(root).chain(dirs) {
                    if !each(fsx::list_dir(&d).unwrap_or_default()) {
                        break;
                    }
                }
            }
        }
        r.done.store(true, Relaxed);
        ctx.request_repaint();
    });
    run
}

/// The first line of `p` containing `needle` (already lowercase), or None for no match, a
/// binary file (a NUL byte in the first 8 KB) or anything that can't be read.
pub fn find(p: &Path, needle: &str, buf: &mut [u8], cancel: &AtomicBool) -> Option<String> {
    use std::os::unix::fs::OpenOptionsExt;
    // Non-blocking open: a FIFO that slipped through must not hang a worker.
    let mut f = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(p).ok()?;
    if !f.metadata().ok()?.is_file() {
        return None;
    }
    // Each read keeps the tail of the one before, so a match across the seam is found.
    let keep = needle.len() + 80;
    let (mut have, mut total) = (0, 0);
    while !cancel.load(Relaxed) && total < MAX_BYTES as usize {
        let n = f.read(buf.get_mut(have..)?).ok()?;
        if n == 0 || total == 0 && buf[..n.min(8192)].contains(&0) {
            return None;
        }
        total += n;
        let len = have + n;
        if let Some(line) = search(&buf[..len], needle) {
            return Some(line);
        }
        have = keep.min(len);
        buf.copy_within(len - have..len, 0);
    }
    None
}

fn search(hay: &[u8], needle: &str) -> Option<String> {
    let nb = needle.as_bytes();
    let (line, cut) = if needle.is_ascii() {
        let i = hay.windows(nb.len()).position(|w| w.eq_ignore_ascii_case(nb))?;
        // The line around the match, starting a little before it on long lines.
        let mut s = hay[..i].iter().rposition(|&b| b == b'\n').map_or(0, |k| k + 1).max(i.saturating_sub(40));
        let mut e = hay[i..].iter().position(|&b| b == b'\n').map_or(hay.len(), |k| i + k).min(i + nb.len() + 80);
        while s < i && hay[s] & 0xC0 == 0x80 {
            s += 1;
        }
        while e > i && e < hay.len() && hay[e] & 0xC0 == 0x80 {
            e -= 1;
        }
        (String::from_utf8_lossy(&hay[s..e]).into_owned(), s > 0 && hay[s - 1] != b'\n')
    } else {
        // Other scripts: lowercase the text itself. Case folding can change byte lengths
        // but never the number of lines, so the line is found by its number.
        let text = String::from_utf8_lossy(hay);
        let low = text.to_lowercase();
        let i = low.find(needle)?;
        (text.split('\n').nth(low[..i].matches('\n').count()).unwrap_or("").chars().take(120).collect(), false)
    };
    // Runs of spaces and tabs (indentation) read as one space.
    let mut out = String::from(if cut { "…" } else { "" });
    let mut sep = "";
    for w in line.split_whitespace() {
        out.push_str(sep);
        out.push_str(w);
        sep = " ";
    }
    Some(out)
}
