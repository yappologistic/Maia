//! The Columns view's other folders. The tab is the column being worked in: its folder,
//! selection, filter, rename and every file operation are the tab's own, as in any
//! view. Beside it the strip lists the folders from its root down to the tab's, each
//! with the entry leading on highlighted, and after it the folder under the cursor.
//! Each of those is a `Tab` of its own that is never shown as a tab, so it streams,
//! sorts and hides files exactly as the tab does, off the UI thread.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::app::App;
use crate::config::{GroupBy, SortKey, ViewSettings};
use crate::fsx::{self, DEAD, Entry};
use crate::tab::{self, HistState, Tab, ViewCtx};

/// Listings kept after they leave the strip, so stepping back and forth is instant.
const SPARE: usize = 6;
/// A folder with more entries than this is listed again rather than copied when the tab
/// leaves it.
const COPY_MAX: usize = 20_000;

pub struct Col {
    pub tab: Tab,
    /// The strip's frame count when it was last part of the strip.
    used: u64,
    /// The entry leading on to the next column (an index into `tab.entries`).
    pub child: Option<u32>,
    /// The path `child` was looked up for.
    child_for: Option<PathBuf>,
}

#[derive(Default)]
pub struct Strip {
    pub cols: Vec<Col>,
    /// The folders left of the tab's, the root first.
    pub chain: Vec<PathBuf>,
    /// The folder under the cursor, listed after the tab's column.
    pub preview: Option<PathBuf>,
    /// Horizontal offset shown, and the one it is easing to.
    pub scroll: f32,
    pub target: f32,
    /// The scroll has been placed once (a view switched to Columns starts in place).
    pub placed: bool,
    /// A click in a column moved the tab: the strip keeps its offset, even past its new
    /// end, so the column clicked stays under the pointer for a second click.
    pub held: bool,
    pub by_click: bool,
    /// What the scroll was last placed for (folder, preview, widths).
    pub laid_for: u64,
    /// Where the tab's column is easing to, to keep the cursor in sight.
    pub own_target: Option<f32>,
    /// The tab's folder the chain was made for.
    chain_for: Option<PathBuf>,
    /// The listing of the tab's own folder last copied into a column.
    copied: Option<Instant>,
    look: Option<(SortKey, bool, bool, bool, bool, bool, bool)>,
    frame: u64,
}

/// Where the strip starts for `path`: the deepest of the home folder, a mounted drive and
/// a network place that holds it, else the filesystem root. Starting at the home folder
/// spares the two columns above it that say the same thing every time, and a drive or a
/// share reads as a place of its own.
pub fn root_for(path: &Path) -> PathBuf {
    let mut best = PathBuf::from("/");
    let mut take = |p: &Path| {
        if path.starts_with(p) && p.components().count() > best.components().count() {
            best = p.to_path_buf();
        }
    };
    take(&fsx::home());
    for m in crate::sidebar::mount_points() {
        take(&m);
    }
    if let Some((r, _)) = crate::sidebar::net_root(path) {
        take(&r);
    }
    best
}

/// The folders from `root` down to `path`, both included.
pub fn chain(root: &Path, path: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = path.ancestors().take_while(|a| a.starts_with(root)).map(Path::to_path_buf).collect();
    v.reverse();
    v
}

/// A column's view: the tab's sort and hidden files, never grouped or narrowed to types,
/// and always with its folders (the way on is through them).
fn side_settings(s: &ViewSettings) -> ViewSettings {
    ViewSettings { group: GroupBy::None, only_types: vec![], show_folders: true, column_view: true, stack_view: false, columns: vec![], ..s.clone() }
}

impl Strip {
    fn col(&self, p: &Path) -> Option<usize> {
        self.cols.iter().position(|c| c.tab.path == p)
    }

    pub fn get(&self, p: &Path) -> Option<&Col> {
        self.col(p).map(|i| &self.cols[i])
    }

    /// The listing of `p`, started when there is none.
    fn open(&mut self, p: &Path, t: &Tab) -> usize {
        if let Some(i) = self.col(p) {
            return i;
        }
        let id = u64::MAX - self.frame.wrapping_mul(64) - self.cols.len() as u64;
        self.cols.push(Col { tab: Tab::new(id, p.to_path_buf(), side_settings(&t.settings), t.ctx.clone()), used: self.frame, child: None, child_for: None });
        self.cols.len() - 1
    }

    /// A change Maia made: the columns it touched read their folders again.
    pub fn changed(&mut self, c: &crate::ops::Changed) {
        for col in &mut self.cols {
            col.tab.changed(c);
        }
    }

    /// Every folder the strip shows besides the tab's own.
    pub fn shown(&self) -> impl Iterator<Item = &PathBuf> {
        self.chain.iter().chain(self.preview.iter())
    }
}

/// Bring the strip up to date with the tab before it is drawn: the chain of folders, the
/// folder under the cursor, their listings landed and sorted.
pub fn sync(t: &mut Tab, c: &ViewCtx) {
    // A click in a column moved the tab after this frame's listings were sorted: sorted
    // now, its column is there for the second click of a double-click.
    if t.dirty && !t.entries.is_empty() {
        t.rebuild(c);
    }
    let mut st = t.strip.take().map(|b| *b).unwrap_or_default();
    st.frame += 1;
    if st.chain_for.as_ref() != Some(&t.path) {
        st.chain = if t.path.is_absolute() { chain(&root_for(&t.path), &t.path) } else { vec![] };
        st.chain.pop();
        st.chain_for = Some(t.path.clone());
        st.held = std::mem::take(&mut st.by_click);
    }
    // What the tab lists is copied when it lands, so leaving the folder leaves its column
    // filled. A listing big enough to make the copy cost is read again instead.
    let plain = !t.flatten && t.smart.is_none() && !tab::is_place(&t.path) && t.error.is_none();
    if plain && !t.is_loading() && t.loaded_at.is_some() && t.loaded_at != st.copied && t.entries.len() <= COPY_MAX {
        st.copied = t.loaded_at;
        let entries: Vec<Entry> = t.entries.iter().filter(|e| e.depth == 0).cloned().collect();
        let i = st.open(&t.path.clone(), t);
        let col = &mut st.cols[i].tab;
        col.loading = None;
        col.set_entries(entries);
    }
    // A folder just gone to shows the listing its column had until its own lands, rather
    // than an empty column for a frame or two.
    if t.entries.is_empty() && t.loading.is_some() && !t.flatten {
        if let Some(col) = st.get(&t.path).filter(|c| c.tab.loading.is_none() && c.tab.error.is_none() && !c.tab.entries.is_empty()) {
            let entries = col.tab.entries.clone();
            t.set_entries(entries);
            t.rebuild(c);
        }
    }
    // The folder under the cursor, while it is the only one picked.
    let under = t.cursor.filter(|_| t.selected.len() <= 1).and_then(|i| t.entries.get(i as usize)).filter(|e| e.is_dir && e.depth != DEAD && tab::drive_dev(&e.path).is_none() && !tab::is_place(&e.path));
    st.preview = under.map(|e| e.path.clone());
    // Columns follow the tab's sort and the hidden-file options.
    let look = (t.settings.sort, t.settings.desc, t.settings.show_files, c.show_hidden, c.show_system, c.folders_first, c.recents_on_top);
    if st.look != Some(look) {
        st.look = Some(look);
        for col in &mut st.cols {
            col.tab.settings = side_settings(&t.settings);
            col.tab.dirty = true;
        }
    }
    let needed: Vec<PathBuf> = st.chain.iter().cloned().collect();
    for p in &needed {
        st.open(p, t);
    }
    // One listing for the folder under the cursor at a time: holding an arrow key down a
    // long list starts the next only when the last has landed.
    if let Some(p) = st.preview.clone() {
        let waiting = st.cols.iter().any(|c| c.tab.loading.is_some() && !needed.contains(&c.tab.path) && c.tab.path != t.path);
        if st.col(&p).is_some() || !waiting {
            st.open(&p, t);
        }
    }
    let frame = st.frame;
    let wants: Vec<(PathBuf, Option<PathBuf>)> = st.chain.iter().enumerate().map(|(k, p)| (p.clone(), Some(st.chain.get(k + 1).cloned().unwrap_or_else(|| t.path.clone())))).chain(st.preview.clone().map(|p| (p, None))).chain(std::iter::once((t.path.clone(), None))).collect();
    for (p, child) in wants {
        let Some(i) = st.col(&p) else { continue };
        let col = &mut st.cols[i];
        col.used = frame;
        let rebuilt = pump(&mut col.tab, c);
        if rebuilt || col.child_for != child {
            col.child = child.as_ref().and_then(|ch| keep_child(&mut col.tab, ch));
            col.child_for = child;
            col.tab.scroll_to_cursor = true;
        }
    }
    // The spares: whatever left the strip last stays, the oldest go.
    for col in st.cols.iter_mut().filter(|c| c.used != frame) {
        pump(&mut col.tab, c);
    }
    let mut spare: Vec<u64> = st.cols.iter().filter(|c| c.used != frame).map(|c| c.used).collect();
    if spare.len() > SPARE {
        spare.sort_unstable();
        let cut = spare[spare.len() - SPARE];
        st.cols.retain(|c| c.used == frame || c.used >= cut);
    }
    t.strip = Some(Box::new(st));
}

/// Results landed in a column's listing: sort it again. Returns whether it did.
fn pump(t: &mut Tab, c: &ViewCtx) -> bool {
    if t.poll() {
        t.dirty = true;
    }
    if t.dirty || t.stream_dirty {
        t.rebuild(c);
        return true;
    }
    false
}

/// The entry leading on, placed in the view among its kind by name even when hidden
/// files are not shown (the way down passes through it).
fn keep_child(t: &mut Tab, child: &Path) -> Option<u32> {
    let i = t.entries.iter().position(|e| e.path == child && e.depth != DEAD)? as u32;
    if !t.view.contains(&i) {
        let e = &t.entries[i as usize];
        let at = t.view.iter().position(|&j| {
            let o = &t.entries[j as usize];
            o.is_dir == e.is_dir && tab::natural_cmp(o.lname(), e.lname()) == std::cmp::Ordering::Greater
        });
        let last_kind = t.view.iter().rposition(|&j| t.entries[j as usize].is_dir == e.is_dir).map(|k| k + 1);
        let at = at.or(last_kind).unwrap_or(if e.is_dir { 0 } else { t.view.len() });
        t.view.insert(at, i);
    }
    t.cursor = Some(i);
    Some(i)
}

/// The strip's folders for the folder watcher, and those it saw change read again.
pub fn watch(t: &mut Tab, changed: &[PathBuf], want: &mut HashSet<PathBuf>) {
    let Some(st) = t.strip.as_mut() else { return };
    if !t.settings.column_view {
        return;
    }
    // Nothing inside an archive can be watched (it is not on disk as folders).
    let archive = t.archive.as_deref();
    want.extend(st.shown().filter(|p| !tab::is_place(p) && archive.is_none_or(|a| !p.starts_with(a))).cloned());
    for col in st.cols.iter_mut().filter(|c| changed.contains(&c.tab.path)) {
        col.tab.reload();
    }
}

/// Left or Right in the Columns view: back to the folder above, or into the folder (or
/// archive, or drive) under the cursor. A file stays where it is.
pub fn step(app: &mut App, right: bool) -> bool {
    let panel = app.active;
    if !right {
        app.run(crate::commands::Cmd::Up);
        return true;
    }
    let t = app.tab();
    let Some(idx) = t.cursor else { return true };
    let Some(e) = t.entries.get(idx as usize) else { return true };
    let archive = !e.is_dir && t.archive.is_none() && crate::archive::is_archive(&e.name) && crate::archive::available();
    if e.is_dir || archive {
        app.actions.push(crate::app::Action::Activate { panel, idx, mods: egui::Modifiers::NONE });
    }
    true
}

/// A click in another column: its folder becomes the tab's, with `item` picked, and the
/// listing the column showed is the tab's at once (the menu a right-click opens needs it).
pub fn focus(app: &mut App, panel: u64, dir: PathBuf, item: Option<PathBuf>) {
    let Some(t) = app.panels.get_mut(&panel).map(|p| p.tab_mut()) else { return };
    let cached = t.strip.as_ref().and_then(|s| s.get(&dir)).filter(|c| c.tab.loading.is_none() && c.tab.error.is_none()).map(|c| c.tab.entries.clone());
    if let Some(st) = t.strip.as_mut() {
        st.by_click = true;
    }
    app.focus_panel(panel);
    if t_path(app, panel).as_ref() != Some(&dir) {
        app.navigate(panel, dir.clone());
    }
    let Some(t) = app.panels.get_mut(&panel).map(|p| p.tab_mut()) else { return };
    if t.path != dir {
        return; // a drive to mount first, or somewhere refused
    }
    // The column keeps its own scroll: what was clicked stays where it was.
    t.restore_scroll = None;
    match cached {
        Some(entries) if t.entries.is_empty() => {
            t.pending_restore = Some(HistState { path: dir, cursor: item.clone(), selected: item.into_iter().collect(), scroll: 0.0 });
            t.set_entries(entries);
        }
        _ => {
            if let Some(i) = item.as_ref().and_then(|p| t.entries.iter().position(|e| e.path == *p)) {
                t.select_only(i as u32);
            } else {
                t.pending_restore = Some(HistState { path: dir, cursor: item.clone(), selected: item.into_iter().collect(), scroll: 0.0 });
            }
        }
    }
}

fn t_path(app: &App, panel: u64) -> Option<PathBuf> {
    app.panels.get(&panel).map(|p| p.tab().path.clone())
}
