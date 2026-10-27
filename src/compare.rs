//! Compare Panels: entries one panel's folder has and the other's lacks, entries newer
//! than their counterpart, same-named files of another size, and same-named folders with
//! anything different inside (looked through on a worker). The marks follow reloads of
//! either folder and clear when either panel moves. Sync plans the copies all the way
//! down, shown before anything is done.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant, SystemTime};

use egui::{Painter, Rect, pos2};

use crate::app::App;
use crate::commands::Cmd;
use crate::fsx::Entry;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Diff {
    Only,
    Newer,
    Size,
    /// A folder on both sides, with something different inside.
    Inside,
}

/// (panel, tab, folder, when its listing landed) for both sides, and the marks of both;
/// the folders on both sides found different inside arrive from a worker.
struct Pair {
    sides: [(u64, u64, PathBuf, Option<Instant>); 2],
    marks: HashMap<PathBuf, Diff>,
    deep: Option<Receiver<(PathBuf, PathBuf)>>,
    stop: Arc<AtomicBool>,
}

impl Drop for Pair {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// How much later a file's time must be to count as newer. FAT, on most USB sticks and SD
/// cards, rounds times to an even second, so a copy kept there can read as much as two
/// seconds behind the file it came from; less slack and that copy is redone every sync.
const SLACK: Duration = Duration::from_secs(2);

pub(crate) fn newer(a: Option<SystemTime>, b: Option<SystemTime>) -> bool {
    let Some(a) = a else { return false };
    b.and_then(|t| t.checked_add(SLACK)).is_none_or(|b| a > b)
}

/// Whether one of the two folders holds the other, after links are resolved. Syncing a
/// folder into one of its own subfolders copies without end, and the other way round
/// Mirror finds the source among the "only there" items and trashes it. One folder on
/// both sides doesn't count; its plan is simply empty.
pub fn one_inside_other(a: &Path, b: &Path) -> bool {
    let resolve = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (a, b) = (resolve(a), resolve(b));
    let below = |x: &Path, y: &Path| x.ancestors().skip(1).any(|up| up == y);
    below(&a, &b) || below(&b, &a)
}

thread_local! {
    static CMP: RefCell<Option<Pair>> = const { RefCell::new(None) };
}

/// Marks for `mine` against `theirs`, keyed by the paths in `mine`.
pub fn side(mine: &[&Entry], theirs: &[&Entry], out: &mut HashMap<PathBuf, Diff>) {
    let by: HashMap<&str, &Entry> = theirs.iter().map(|e| (e.name.as_str(), *e)).collect();
    for e in mine {
        let d = match by.get(e.name.as_str()) {
            Some(o) if o.is_dir == e.is_dir => {
                if e.is_dir {
                    continue;
                }
                if newer(e.modified, o.modified) {
                    Diff::Newer
                } else if e.size != o.size {
                    Diff::Size
                } else {
                    continue;
                }
            }
            _ => Diff::Only,
        };
        out.insert(e.path.clone(), d);
    }
}

/// The panel F5 would copy to: the one focused before, else any other.
pub fn other(app: &App) -> Option<u64> {
    if app.prev_active != app.active && app.panels.contains_key(&app.prev_active) {
        return Some(app.prev_active);
    }
    let mut leaves = vec![];
    app.tree.leaves(&mut leaves);
    leaves.into_iter().find(|&l| l != app.active)
}

fn build(app: &App, a: u64, b: u64) -> Option<Pair> {
    let (ta, tb) = (app.panels.get(&a)?.tab(), app.panels.get(&b)?.tab());
    if ta.flatten || tb.flatten || crate::tab::is_place(&ta.path) || crate::tab::is_place(&tb.path) {
        return None;
    }
    fn shown(t: &crate::tab::Tab) -> Vec<&Entry> {
        t.view.iter().map(|&i| &t.entries[i as usize]).filter(|e| e.depth == 0).collect()
    }
    let (ea, eb) = (shown(ta), shown(tb));
    let mut marks = HashMap::new();
    side(&ea, &eb, &mut marks);
    side(&eb, &ea, &mut marks);
    // Folders on both sides: looked through on a worker, each pair until a difference.
    let by: HashMap<&str, &Entry> = eb.iter().filter(|e| e.is_dir).map(|e| (e.name.as_str(), *e)).collect();
    let both: Vec<(PathBuf, PathBuf)> = ea.iter().filter(|e| e.is_dir).filter_map(|e| by.get(e.name.as_str()).map(|o| (e.path.clone(), o.path.clone()))).collect();
    let stop = Arc::new(AtomicBool::new(false));
    let deep = (!both.is_empty()).then(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        let (st, ctx) = (stop.clone(), app.ctx.clone());
        std::thread::spawn(move || {
            for (x, y) in both {
                if st.load(Ordering::Relaxed) {
                    return;
                }
                if differs(&x, &y, &st) && tx.send((x, y)).is_ok() {
                    ctx.request_repaint();
                }
            }
        });
        rx
    });
    Some(Pair { sides: [(a, ta.id, ta.path.clone(), ta.loaded_at), (b, tb.id, tb.path.clone(), tb.loaded_at)], marks, deep, stop })
}

/// Whether two folders hold anything different, all the way down: an entry one has and
/// the other lacks, of another kind, size or time.
pub fn differs(a: &Path, b: &Path, stop: &AtomicBool) -> bool {
    let list = |d: &Path| -> HashMap<std::ffi::OsString, std::fs::Metadata> { std::fs::read_dir(d).map(|rd| rd.flatten().filter_map(|e| Some((e.file_name(), e.metadata().ok()?))).collect()).unwrap_or_default() };
    let (la, lb) = (list(a), list(b));
    if la.len() != lb.len() || stop.load(Ordering::Relaxed) {
        return true;
    }
    la.iter().any(|(name, ma)| match lb.get(name) {
        None => true,
        Some(mb) if ma.is_dir() != mb.is_dir() || ma.is_symlink() != mb.is_symlink() => true,
        Some(_) if ma.is_dir() && !ma.is_symlink() => differs(&a.join(name), &b.join(name), stop),
        Some(mb) => ma.len() != mb.len() || newer(ma.modified().ok(), mb.modified().ok()) || newer(mb.modified().ok(), ma.modified().ok()),
    })
}

/// What a sync does: copies of what the other side lacks, newer files over older ones
/// (the older to the trash), and, mirroring, what only the other side has to the trash.
#[derive(Default, Debug, PartialEq)]
pub struct Plan {
    /// (from here, to there): new there.
    pub copy: Vec<(PathBuf, PathBuf)>,
    /// (from here, over this there): newer here.
    pub update: Vec<(PathBuf, PathBuf)>,
    /// Only there.
    pub extra: Vec<PathBuf>,
    pub bytes: u64,
    /// Folders the walk couldn't list, on either side, with nothing under them planned.
    /// Read as empty, an unlistable source folder would put its whole twin under "only
    /// there", for Mirror to trash.
    pub unlisted: Vec<PathBuf>,
}

/// Plan syncing `from` into `to`, all the way down.
pub fn plan(from: &Path, to: &Path, stop: &AtomicBool) -> Plan {
    let mut p = Plan::default();
    if !one_inside_other(from, to) {
        walk(from, to, &mut p, stop);
    }
    p
}

/// A folder's entries by name with their own metadata (links not followed), or None when
/// any part of it can't be read: a listing with holes would plan wrong.
fn listing(d: &Path) -> Option<Vec<(std::ffi::OsString, std::fs::Metadata)>> {
    let entries = std::fs::read_dir(d).ok()?.map(|e| e.and_then(|e| Ok((e.file_name(), e.path().symlink_metadata()?))));
    let mut v = entries.collect::<std::io::Result<Vec<_>>>().ok()?;
    v.sort_by(|a, b| a.0.cmp(&b.0));
    Some(v)
}

fn walk(from: &Path, to: &Path, p: &mut Plan, stop: &AtomicBool) {
    if stop.load(Ordering::Relaxed) {
        return;
    }
    let (here, there) = (listing(from), listing(to));
    let (Some(here), Some(there)) = (&here, &there) else {
        p.unlisted.extend([(from, here.is_none()), (to, there.is_none())].into_iter().filter(|d| d.1).map(|d| d.0.to_path_buf()));
        return;
    };
    let by: HashMap<&std::ffi::OsString, &std::fs::Metadata> = there.iter().map(|(n, m)| (n, m)).collect();
    for (name, m) in here {
        let (src, dst) = (from.join(name), to.join(name));
        match by.get(name) {
            None => {
                p.bytes += if m.is_dir() { crate::ops::tree_size(&src) } else { m.len() };
                p.copy.push((src, dst));
            }
            Some(o) if m.is_dir() && o.is_dir() => walk(&src, &dst, p, stop),
            Some(o) if m.is_dir() != o.is_dir() || newer(m.modified().ok(), o.modified().ok()) => {
                p.bytes += if m.is_dir() { crate::ops::tree_size(&src) } else { m.len() };
                p.update.push((src, dst));
            }
            Some(_) => {}
        }
    }
    let mine: std::collections::HashSet<&std::ffi::OsString> = here.iter().map(|(n, _)| n).collect();
    p.extra.extend(there.iter().filter(|(n, _)| !mine.contains(n)).map(|(n, _)| to.join(n)));
}

pub fn run(app: &mut App, cmd: Cmd) {
    // Selecting and syncing compare afresh first, so they act on what is on screen now.
    let Some(o) = other(app) else {
        app.toast("Compare needs two panels");
        return;
    };
    // The comparison on show, while neither side has moved or changed (its folders
    // looked through already); else a fresh one.
    let held = CMP.with(|c| c.borrow_mut().take()).filter(|p| {
        let (x, y) = (app.panels.get(&p.sides[0].0).map(|p| p.tab()), app.panels.get(&p.sides[1].0).map(|p| p.tab()));
        p.sides[0].0 == app.active && p.sides[1].0 == o && [x, y].iter().zip(&p.sides).all(|(t, s)| t.is_some_and(|t| t.id == s.1 && t.path == s.2 && t.loaded_at == s.3))
    });
    let Some(pair) = held.or_else(|| build(app, app.active, o)) else {
        app.toast("Compare works on two folder listings");
        return;
    };
    let (path, dest) = (pair.sides[0].2.clone(), pair.sides[1].2.clone());
    let mine = pair.marks.keys().filter(|p| p.parent() == Some(&path)).count();
    let (n, looking) = (pair.marks.len(), pair.deep.is_some());
    CMP.with(|c| *c.borrow_mut() = Some(pair));
    match cmd {
        Cmd::SelectDifferences => {
            let t = app.tab();
            t.selected = t.view.iter().copied().filter(|&i| mark(&t.entries[i as usize].path).is_some()).collect();
        }
        Cmd::SyncPanels if one_inside_other(&path, &dest) => app.toast("One of these folders is inside the other"),
        Cmd::SyncPanels => app.popup = crate::popups::Popup::Sync(Box::new(SyncState::start(path, dest, &app.ctx))),
        // Folders still being looked through may yet differ inside.
        _ if n == 0 && !looking => app.toast("No differences"),
        _ if n == 0 => app.toast("Looking through the folders on both sides…"),
        _ => app.toast(format!("{mine} different here, {} there", n - mine)),
    }
}

/// The Sync dialog: the plan worked out on a worker, shown before anything is done.
pub struct SyncState {
    pub from: PathBuf,
    pub to: PathBuf,
    pub plan: Option<Plan>,
    rx: Option<Receiver<Plan>>,
    stop: Arc<AtomicBool>,
    /// Also send what only the other side has to the trash.
    pub mirror: bool,
}

impl Drop for SyncState {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl SyncState {
    pub fn start(from: PathBuf, to: PathBuf, ctx: &egui::Context) -> SyncState {
        let (tx, rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let (f, t, st, ctx) = (from.clone(), to.clone(), stop.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(plan(&f, &t, &st));
            ctx.request_repaint();
        });
        SyncState { from, to, plan: None, rx: Some(rx), stop, mirror: false }
    }

    pub fn poll(&mut self) {
        if let Some(p) = self.rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.plan = Some(p);
            self.rx = None;
        }
    }

    /// The job that carries the plan out.
    pub fn job(&self) -> Option<crate::ops::OpKind> {
        let p = self.plan.as_ref()?;
        let copies: Vec<(PathBuf, PathBuf)> = p.copy.iter().chain(&p.update).cloned().collect();
        let remove = if self.mirror { p.extra.clone() } else { vec![] };
        (!copies.is_empty() || !remove.is_empty()).then_some(crate::ops::OpKind::Sync { copies, remove })
    }
}

/// Once a frame: drop the marks when either panel has moved, compare again when either
/// folder's listing was reloaded (a sync or undo landing, a change on disk).
pub fn pump(app: &App) {
    CMP.with(|c| {
        let mut c = c.borrow_mut();
        let Some(p) = c.as_mut() else { return };
        if let Some(rx) = &p.deep {
            loop {
                match rx.try_recv() {
                    Ok((a, b)) => {
                        p.marks.insert(a, Diff::Inside);
                        p.marks.insert(b, Diff::Inside);
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(_) => {
                        p.deep = None;
                        break;
                    }
                }
            }
        }
        let p = &*p;
        let mut reloaded = false;
        for (pid, tid, path, at) in &p.sides {
            match app.panels.get(pid).map(|p| p.tab()) {
                Some(t) if t.id == *tid && t.path == *path && !t.flatten => {
                    if t.is_loading() || t.dirty {
                        return;
                    }
                    reloaded |= t.loaded_at != *at;
                }
                _ => {
                    *c = None;
                    return;
                }
            }
        }
        if reloaded {
            *c = build(app, p.sides[0].0, p.sides[1].0);
        }
    });
}

pub fn mark(path: &Path) -> Option<Diff> {
    CMP.with(|c| c.borrow().as_ref()?.marks.get(path).copied())
}

/// A short bar along the left edge of `r`: accent where only this side has the entry,
/// green where it is newer, amber where only the size differs or a folder's contents do.
pub fn paint(p: &Painter, r: Rect, path: &Path, pal: &crate::theme::Palette) {
    let Some(d) = mark(path) else { return };
    let col = match d {
        Diff::Only => pal.accent,
        Diff::Newer => crate::gitstatus::GREEN,
        Diff::Size | Diff::Inside => crate::gitstatus::AMBER,
    };
    p.rect_filled(Rect::from_min_max(pos2(r.left() + 1.0, r.top() + 3.0), pos2(r.left() + 3.5, r.bottom() - 3.0)), 1.0, col);
}
