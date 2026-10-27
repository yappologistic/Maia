//! Per-tab state: location, listing, view (sort/filter/group), history, selection.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::SystemTime;

use crate::config::{GroupBy, SaveScope, SortKey, ViewSettings};
use crate::fsx::{self, DEAD, Entry, Listing, Walk};
use crate::rename::Rename;
use crate::stats::DirStats;

pub const COMPUTER: &str = "maia:computer";

pub fn is_computer(p: &Path) -> bool {
    // (Or its form under an earlier name, as an older saved session may have it.)
    p.as_os_str() == COMPUTER || p.to_str().is_some_and(|s| s.ends_with(":computer") && !s.contains('/') && s.len() < 24)
}

/// Your GitHub profile (`github.rs`): a place Maia draws, not a folder.
pub const GITHUB: &str = "maia:github";

pub fn is_github(p: &Path) -> bool {
    p.as_os_str() == GITHUB
}

/// Computer or GitHub: places with nothing on disk behind them, where nothing is written.
pub fn is_place(p: &Path) -> bool {
    is_computer(p) || is_github(p)
}

/// Computer view rows for drives that aren't mounted: "maia:drive:/dev/sdb1".
pub const DRIVE: &str = "maia:drive:";

pub fn drive_dev(p: &Path) -> Option<&str> {
    let s = p.to_str()?;
    s.strip_prefix(DRIVE).or_else(|| s.split_once(":drive:").filter(|(name, _)| !name.contains('/')).map(|(_, dev)| dev))
}

#[derive(Clone, Debug, Default)]
pub struct HistState {
    pub path: PathBuf,
    pub cursor: Option<PathBuf>,
    pub selected: Vec<PathBuf>,
    pub scroll: f32,
}

#[derive(Clone, Debug)]
/// One display row. Kept small (12 bytes): there is one per visible entry, so header
/// labels live in `Tab::group_labels` and rows refer to them by index.
pub enum Row {
    Header { label: u32, count: u32, collapsed: bool },
    Item(u32),
}

#[derive(Clone, Debug, Default)]
pub struct VolInfo {
    pub total: u64,
    pub free: u64,
    pub fstype: String,
    pub device: String,
    pub mounted: bool,
    /// Removable media (offers Eject).
    pub removable: bool,
    pub crypt: crate::drives::Crypt,
}

/// Inputs that affect which entries are visible and in what order.
pub struct ViewCtx<'a> {
    pub show_hidden: bool,
    pub show_system: bool,
    pub folders_first: bool,
    pub recents_on_top: bool,
    pub recents: &'a HashSet<PathBuf>,
    /// Kept first in their folder, before recents and folders-first.
    pub pinned: &'a HashSet<PathBuf>,
    pub stats: &'a HashMap<PathBuf, DirStats>,
}

pub struct Tab {
    pub id: u64,
    pub path: PathBuf,
    pub entries: Vec<Entry>,
    pub vols: HashMap<PathBuf, VolInfo>,
    /// Visible entries (indices into `entries`) in display order.
    pub view: Vec<u32>,
    /// entry index -> position in `view` (u32::MAX when not visible).
    pos_of: Vec<u32>,
    /// Display rows (with group headers) for Details view.
    pub rows: Vec<Row>,
    /// Labels of the group headers in `rows` (`Row::Header::label` indexes this).
    pub group_labels: Vec<String>,
    pub settings: ViewSettings,
    pub saved_scope: Option<SaveScope>,
    /// The view chosen outside folders with a saved view, put back on leaving them
    /// (None while that view is the one shown).
    pub free_view: Option<ViewSettings>,
    pub filter: String,
    last_filter: String,
    /// Whether the rows were last built as a tree: zoom moves in and out of the tree view
    /// without marking the tab dirty, and unfolded folders' rows must come and go with it.
    pub built_tree: bool,
    pub flatten: bool,
    /// The smart folder this flattened listing shows, which only its matches are walked for.
    pub smart: Option<crate::smart::SmartFolder>,
    pub walk: Option<Walk>,
    pub loading: Option<Receiver<Listing>>,
    pub error: Option<String>,
    /// The folder no longer exists (deleted, renamed away, drive removed).
    pub gone: bool,
    /// The archive file this tab is looking inside (read-only), if any.
    pub archive: Option<PathBuf>,
    pub selected: HashSet<u32>,
    pub cursor: Option<u32>,
    pub anchor: Option<u32>,
    pub history: Vec<HistState>,
    pub hist_pos: usize,
    /// One-shot UI requests.
    pub scroll_to_cursor: bool,
    pub restore_scroll: Option<f32>,
    pub current_scroll: f32,
    pub pending_restore: Option<HistState>,
    pub dirty: bool,
    /// Background results (walk batches, tree listings) arrived: rebuild when `rebuild_due`.
    pub stream_dirty: bool,
    /// When the last rebuild ran and what it cost (paces streaming rebuilds).
    last_rebuild: Option<std::time::Instant>,
    last_rebuild_ms: f32,
    pub collapsed_groups: HashSet<String>,
    pub rename: Option<Rename>,
    /// Counts for the footer: (visible folders, total folders, visible files, total files).
    pub counts: (usize, usize, usize, usize),
    pub hidden_count: usize,
    /// What git ignores here, left out of the view (the Ignored files option is Hide).
    pub git_hide: Option<std::sync::Arc<HashSet<std::ffi::OsString>>>,
    /// Only what git marks as changed is listed (Show Only Changed Files), and the
    /// work tree whose marks say so while it is on.
    pub only_changed: bool,
    pub git_only: Option<std::sync::Arc<HashSet<PathBuf>>>,
    /// The status answer `git_only` was made from.
    pub git_only_from: Option<std::rc::Rc<crate::gitstatus::Repo>>,
    /// In a picker window: the files the app's filter lets through (folders always pass).
    pub pick: Option<std::sync::Arc<crate::picker::Accept>>,
    /// In a trash folder: original location and deletion time per item.
    pub trash_meta: HashMap<PathBuf, (PathBuf, Option<SystemTime>)>,
    /// Tree-view context rows: ancestors kept only to show where filter matches live (drawn dimmed).
    pub dimmed: HashSet<u32>,
    pub loaded_at: Option<std::time::Instant>,
    pub ctx: egui::Context,
    /// Layout facts from the last frame, for keyboard navigation.
    pub page_items: usize,
    pub nav_row_step: usize,
    pub nav_col_step: usize,
    pub cursor_screen_pos: Option<egui::Pos2>,
    /// Where the items on screen were drawn last frame (their picture or icon): the
    /// viewer grows a picture out of its place and puts it back there.
    pub item_rects: Vec<(u32, egui::Rect)>,
    /// The folder under the pointer fanning out its pictures (`peek.rs`).
    pub peek: crate::peek::Peek,
    /// Years marked on the scrollbar of a list sorted by a date (`ribbon.rs`).
    pub ribbon: crate::ribbon::Ribbon,
    /// Rows easing to their new places after the order changed (`glide.rs`).
    pub glide: crate::glide::Glide,
    /// Counts the times the shown order was worked out.
    pub view_gen: u64,
    /// What kinds of file fill the list, for the status bar's ring (`glance.rs`).
    pub glance: crate::glance::Glance,
    pub pending_rename: Option<PathBuf>,
    pub rubber: Option<Rubber>,
    pub shown_zoom: f32,
    /// The Stack view: where the row stands (an index into `view`, easing towards the
    /// cursor's), and wheel movement short of a whole item.
    pub stack_pos: f32,
    pub stack_acc: f32,
    /// The glow behind the card at the cursor, easing to its picture's colour (RGB, and
    /// how much of it shows).
    pub stack_glow: [f32; 4],
    /// The scrubber's marks for the listing and sort they were made for: where each
    /// letter, month, size or kind begins.
    pub stack_marks: Option<(u64, std::rc::Rc<Vec<(usize, String)>>)>,
    /// Partial wheel notches toward the next zoom step.
    pub zoom_acc: f32,
    /// Where the Type column's filter button was last drawn (the Types popup opens there).
    pub funnel_at: Option<egui::Pos2>,
    /// Width the tab was last drawn at in its strip.
    pub shown_tab_w: f32,
    /// Kept small at the strip's left and out of Close Other Tabs.
    pub pinned: bool,
    /// The folder's README, shown above the listing.
    pub readme: Option<Box<crate::readme::Card>>,
    pub hscroll: f32,
    pub born: std::time::Instant,
    /// Details tree view: expanded folders and their pending child listings.
    pub expanded: HashSet<PathBuf>,
    tree_loads: Vec<Receiver<Listing>>,
    /// Folders whose children have been listed into `entries` (a new listing of one of
    /// these replaces its old children).
    tree_listed: HashSet<PathBuf>,
    /// Where each listed folder's children sit in `entries` (appended together when the
    /// listing lands): the tree rebuild groups children without hashing every path.
    tree_groups: HashMap<PathBuf, std::ops::Range<u32>>,
    /// Folders whose subfolders unfold recursively as their listings arrive (Shift+expand).
    expand_rec: HashSet<PathBuf>,
    /// Every folder of the listing was unfolded at once (Ctrl+E in Tree view).
    pub expand_all_on: bool,
    /// Flattened listing: when the recursive walk began, and how long it took once done.
    walk_started: Option<std::time::Instant>,
    pub walk_secs: Option<f32>,
    /// Place the file cursor on the first item once the listing arrives.
    cursor_first: bool,
    /// Drag across checkboxes: the state being painted onto rows.
    pub check_drag: Option<bool>,
    /// The filter's contents mode and its running search.
    pub content: crate::contentsearch::State,
    /// The Columns view's other folders (made when it is first drawn).
    pub strip: Option<Box<crate::columns::Strip>>,
}

/// Rubber-band selection in content coordinates.
#[derive(Clone, Debug)]
pub struct Rubber {
    pub start: egui::Pos2,
    pub base: HashSet<u32>,
    pub fade: Option<std::time::Instant>,
    pub last_rect: egui::Rect,
}

impl Tab {
    pub fn new(id: u64, path: PathBuf, settings: ViewSettings, ctx: egui::Context) -> Tab {
        let mut t = Tab {
            id,
            path: path.clone(),
            entries: vec![],
            vols: HashMap::new(),
            view: vec![],
            pos_of: vec![],
            rows: vec![],
            group_labels: vec![],
            settings,
            saved_scope: None,
            free_view: None,
            filter: String::new(),
            last_filter: String::new(),
            built_tree: false,
            flatten: false,
            smart: None,
            walk: None,
            loading: None,
            error: None,
            selected: HashSet::new(),
            cursor: None,
            anchor: None,
            history: vec![HistState { path: path.clone(), ..Default::default() }],
            hist_pos: 0,
            scroll_to_cursor: false,
            restore_scroll: Some(0.0),
            current_scroll: 0.0,
            pending_restore: None,
            dirty: true,
            stream_dirty: false,
            last_rebuild: None,
            last_rebuild_ms: 0.0,
            collapsed_groups: HashSet::new(),
            rename: None,
            counts: (0, 0, 0, 0),
            hidden_count: 0,
            git_hide: None,
            only_changed: false,
            git_only: None,
            git_only_from: None,
            pick: None,
            gone: false,
            archive: None,
            dimmed: HashSet::new(),
            trash_meta: HashMap::new(),
            loaded_at: None,
            ctx,
            page_items: 20,
            nav_row_step: 1,
            nav_col_step: 0,
            cursor_screen_pos: None,
            item_rects: Vec::new(),
            peek: Default::default(),
            ribbon: Default::default(),
            glide: Default::default(),
            view_gen: 0,
            glance: Default::default(),
            pending_rename: None,
            rubber: None,
            shown_zoom: -1.0,
            stack_pos: -1.0,
            stack_acc: 0.0,
            stack_glow: [0.0; 4],
            stack_marks: None,
            zoom_acc: 0.0,
            funnel_at: None,
            shown_tab_w: 0.0,
            pinned: false,
            readme: None,
            hscroll: 0.0,
            born: std::time::Instant::now(),
            expanded: HashSet::new(),
            tree_loads: vec![],
            tree_listed: HashSet::new(),
            tree_groups: HashMap::new(),
            expand_rec: HashSet::new(),
            expand_all_on: false,
            walk_started: None,
            walk_secs: None,
            cursor_first: true,
            check_drag: None,
            content: Default::default(),
            strip: None,
        };
        t.reload();
        t
    }

    pub fn title(&self) -> String {
        if let Some(s) = &self.smart {
            return s.name.clone();
        }
        let base = if is_computer(&self.path) {
            "Computer".to_string()
        } else if is_github(&self.path) {
            "GitHub".to_string()
        } else if self.path == Path::new("/") {
            "/".to_string()
        } else if let Some((_, name)) = crate::sidebar::net_root(&self.path).filter(|r| r.0 == self.path) {
            name
        } else {
            self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| self.path.display().to_string())
        };
        if self.flatten || self.expand_all_on { format!("{base} […]") } else { base }
    }

    pub fn is_loading(&self) -> bool {
        self.loading.is_some() || !self.tree_loads.is_empty() || self.walk.as_ref().map(|w| !w.done.load(std::sync::atomic::Ordering::Relaxed)).unwrap_or(false) || self.content.busy()
    }

    // ------------------------------------------------------------ loading

    /// Re-read the current location (keeps selection & scroll).
    pub fn reload(&mut self) {
        self.error = None;
        self.archive = None;
        if is_computer(&self.path) {
            self.load_computer();
            return;
        }
        if is_github(&self.path) {
            self.walk = None;
            self.loading = None;
            self.flatten = false;
            self.set_entries(vec![]);
            return;
        }
        if !self.path.is_dir() {
            if let Some((archive, inner)) = crate::archive::split(&self.path) {
                self.flatten = false;
                self.walk = None;
                self.archive = Some(archive.clone());
                self.loading = Some(crate::archive::list_async(self.path.clone(), archive, inner, self.ctx.clone()));
                return;
            }
        }
        if self.flatten {
            self.start_walk();
        } else {
            self.walk = None;
            self.loading = Some(fsx::list_dir_async(self.path.clone(), self.ctx.clone()));
        }
    }

    fn load_computer(&mut self) {
        self.walk = None;
        self.loading = None;
        self.vols.clear();
        let mut entries = vec![];
        let drives = crate::sidebar::drives();
        for m in fsx::mounts() {
            if let Some(mut e) = Entry::from_path(&m.path) {
                let (total, free) = fsx::disk_space(&m.path).unwrap_or((0, 0));
                e.set_name(volume_label(&m.device, &m.path));
                e.size = total.saturating_sub(free);
                let d = crate::drives::at_mount(&drives, &m.path);
                let (removable, crypt) = (d.is_some_and(|d| d.removable), d.map(|d| d.crypt.clone()).unwrap_or_default());
                self.vols.insert(m.path.clone(), VolInfo { total, free, fstype: m.fstype.clone(), device: m.device.clone(), mounted: true, removable, crypt });
                entries.push(e);
            }
        }
        // Filesystems that could be mounted, listed after the mounted ones.
        for d in drives.iter().filter(|d| d.offer_mount()) {
            let path = PathBuf::from(format!("{DRIVE}{}", d.dev));
            entries.push(Entry::placeholder(path.clone(), d.title(), true));
            self.vols.insert(path, VolInfo { total: d.size, free: d.size, fstype: d.fstype.clone(), device: d.dev.clone(), mounted: false, removable: d.removable, crypt: d.crypt.clone() });
        }
        self.set_entries(entries);
    }

    fn start_walk(&mut self) {
        self.loading = None;
        let sel = self.selected_paths();
        let cur = self.cursor_path();
        self.entries.clear();
        self.selected.clear();
        self.cursor = None;
        self.anchor = None;
        self.view.clear();
        self.rows.clear();
        self.group_labels.clear();
        // Walk everything; hidden filtering happens in the view so toggles stay instant.
        self.walk = Some(match &self.smart {
            Some(s) => crate::smart::walk(s.clone(), self.ctx.clone()),
            None => fsx::walk_async(self.path.clone(), true, self.ctx.clone()),
        });
        self.walk_started = Some(std::time::Instant::now());
        self.walk_secs = None;
        self.pending_restore = Some(HistState { path: self.path.clone(), cursor: cur, selected: sel, scroll: self.current_scroll });
        self.dirty = true;
    }

    /// Pump async results. Returns true if anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.loading {
            if let Ok(l) = rx.try_recv() {
                self.loading = None;
                if l.path == self.path {
                    match l.result {
                        Ok(v) => self.set_entries(v),
                        Err(e) => {
                            // "Not found" (os error 2): the folder itself is gone, as opposed
                            // to unreadable or slow; the app then moves to the closest parent.
                            self.gone = e.contains("(os error 2)") && !is_computer(&self.path);
                            self.error = Some(e);
                            self.set_entries(vec![]);
                        }
                    }
                    changed = true;
                }
            }
        }
        // Landed child listings (Tree view). Bounded per frame: an "expand all" over a big
        // tree can finish thousands of folders at once.
        let mut landed: Vec<Listing> = vec![];
        let mut i = 0;
        while i < self.tree_loads.len() && landed.len() < 256 {
            match self.tree_loads[i].try_recv() {
                Ok(l) => {
                    self.tree_loads.swap_remove(i);
                    landed.push(l);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => i += 1,
                Err(_) => {
                    self.tree_loads.swap_remove(i);
                }
            }
        }
        if !landed.is_empty() {
            if self.tree_loads.len() > 0 {
                self.ctx.request_repaint();
            }
            // Folders listed before (a refresh or re-expand) supersede their old children,
            // in one pass for the whole batch. First-time loads have nothing to replace;
            // scanning every entry per landed folder made expand-all quadratic.
            let reloaded: HashSet<PathBuf> = landed.iter().filter(|l| !self.tree_listed.insert(l.path.clone())).map(|l| l.path.clone()).collect();
            if !reloaded.is_empty() {
                for e in self.entries.iter_mut() {
                    if e.depth > 0 && e.depth != DEAD && e.path.parent().is_some_and(|p| reloaded.contains(p)) {
                        e.depth = DEAD;
                    }
                }
            }
            for l in landed {
                let depth = self.child_depth(&l.path);
                if let Ok(mut kids) = l.result {
                    for k in &mut kids {
                        k.depth = depth;
                    }
                    // Recursive unfold continues into the subfolders (bounded, so a huge
                    // tree cannot run away; hidden folders stay folded).
                    if self.expand_rec.contains(&l.path) && self.expanded.len() < 20_000 {
                        for k in kids.iter().filter(|k| k.is_dir && !k.is_link && !k.hidden) {
                            self.expand_rec.insert(k.path.clone());
                            self.expanded.insert(k.path.clone());
                            self.tree_loads.push(fsx::list_dir_pooled(k.path.clone(), self.ctx.clone()));
                        }
                    }
                    let start = self.entries.len() as u32;
                    self.entries.extend(kids);
                    self.tree_groups.insert(l.path.clone(), start..self.entries.len() as u32);
                }
            }
            // Streaming in: rebuild at a measured pace; once everything landed, right away.
            if self.tree_loads.is_empty() { self.dirty = true } else { self.stream_dirty = true }
            changed = true;
        }
        if let Some(w) = &self.walk {
            let mut got = false;
            while let Ok(batch) = w.rx.try_recv() {
                self.entries.extend(batch);
                got = true;
            }
            if got {
                self.stream_dirty = true;
                changed = true;
            }
            if w.done.load(std::sync::atomic::Ordering::Relaxed) && self.walk_secs.is_none() {
                self.walk_secs = self.walk_started.map(|t| t.elapsed().as_secs_f32());
                self.dirty = true;
                // The walk grew `entries` by doubling; drop the spare capacity (up to half).
                self.entries.shrink_to_fit();
                changed = true;
            }
            if w.done.load(std::sync::atomic::Ordering::Relaxed) && self.pending_restore.is_some() {
                self.dirty = true;
                changed = true;
            }
        }
        changed
    }

    /// The flattened listing stopped at `fsx::WALK_LIMIT` entries.
    pub fn walk_truncated(&self) -> bool {
        self.walk.as_ref().map(|w| w.truncated.load(std::sync::atomic::Ordering::Relaxed)).unwrap_or(false)
    }

    /// Is the tree view active (Details view, not flattened)?
    pub fn tree_active(&self) -> bool {
        !self.flatten && crate::listing::mode_of(&self.settings) == crate::listing::Mode::Tree && !is_place(&self.path) && self.archive.is_none()
    }

    pub fn toggle_expand(&mut self, dir: &Path) {
        if self.expanded.remove(dir) {
            self.expand_rec.remove(dir);
            self.dirty = true;
            return;
        }
        self.expanded.insert(dir.to_path_buf());
        self.load_children(dir);
    }

    /// Unfold `dir` and, as their listings arrive, every folder below it.
    pub fn expand_recursive(&mut self, dir: &Path) {
        self.expand_rec.insert(dir.to_path_buf());
        if self.expanded.insert(dir.to_path_buf()) || !self.entries.iter().any(|e| e.depth > 0 && e.depth != DEAD && e.path.parent() == Some(dir)) {
            self.load_children(dir);
        } else {
            // Already listed: recurse into the children we have.
            let subs: Vec<PathBuf> = self.entries.iter().filter(|e| e.depth > 0 && e.depth != DEAD && e.is_dir && !e.is_link && e.path.parent() == Some(dir)).map(|e| e.path.clone()).collect();
            for s in subs {
                self.expand_recursive(&s);
            }
        }
        self.dirty = true;
    }

    /// Fold `dir` and everything below it.
    pub fn collapse_recursive(&mut self, dir: &Path) {
        self.expanded.retain(|d| !d.starts_with(dir));
        self.expand_rec.retain(|d| !d.starts_with(dir));
        self.dirty = true;
    }

    /// Ctrl+E in Tree view: unfold every folder of the listing recursively, or fold them all back.
    pub fn toggle_expand_all(&mut self) {
        if self.expand_all_on {
            self.expanded.clear();
            self.expand_rec.clear();
            self.expand_all_on = false;
            self.dirty = true;
            return;
        }
        self.expand_all_on = true;
        let dirs: Vec<PathBuf> = self.entries.iter().filter(|e| e.depth == 0 && e.is_dir && !e.is_link).map(|e| e.path.clone()).collect();
        for d in dirs {
            self.expand_recursive(&d);
        }
    }

    pub fn load_children(&mut self, dir: &Path) {
        self.tree_loads.push(fsx::list_dir_pooled(dir.to_path_buf(), self.ctx.clone()));
    }

    /// Catch up with a change Maia made itself. The folder watcher only follows the front
    /// tab of each panel and never a flattened walk, so without this a tab behind
    /// another or a flattened listing keeps showing what was moved out of it.
    pub fn changed(&mut self, c: &crate::ops::Changed) {
        if let Some(s) = self.strip.as_mut() {
            s.changed(c);
        }
        // Inside an archive: read again when a change was made to it.
        if let Some(a) = &self.archive {
            if self.rename.is_none() && c.arrived.iter().any(|d| d == a) {
                self.reload();
            }
            return;
        }
        if is_place(&self.path) || self.rename.is_some() {
            return;
        }
        if c.gone.iter().any(|g| self.path.starts_with(g)) {
            // Its own folder went: the plain listing finds that out and moves up.
            self.flatten = false;
            self.smart = None;
            self.reload();
            return;
        }
        if self.forget(&c.gone) {
            self.dirty = true;
        }
        if self.flatten {
            // What left is already dropped; what arrived can only be found by walking.
            if c.arrived.iter().any(|d| d.starts_with(&self.path)) {
                self.reload();
            }
        } else if c.dirs().any(|d| *d == self.path) {
            self.reload();
        } else if self.tree_active() {
            let open: Vec<PathBuf> = c.dirs().filter(|d| self.expanded.contains(*d)).cloned().collect();
            for d in open {
                self.load_children(&d);
            }
        }
    }

    /// Drop the rows of `gone` and of everything under them, until the listing is read
    /// again. Returns whether any row went.
    fn forget(&mut self, gone: &[PathBuf]) -> bool {
        use std::os::unix::ffi::OsStrExt;
        let gone: HashSet<&[u8]> = gone.iter().filter(|g| g.starts_with(&self.path)).map(|g| g.as_os_str().as_bytes()).collect();
        if gone.is_empty() {
            return false;
        }
        let root = self.path.as_os_str().len();
        let mut any = false;
        for e in self.entries.iter_mut().filter(|e| e.depth != DEAD) {
            let b = e.path.as_os_str().as_bytes();
            // The row itself, or a folder above it that went.
            if gone.contains(b) || b.iter().enumerate().skip(root + 1).any(|(i, &c)| c == b'/' && gone.contains(&b[..i])) {
                e.depth = DEAD;
                any = true;
            }
        }
        if any {
            let dead = |i: &u32| self.entries.get(*i as usize).is_none_or(|e| e.depth == DEAD);
            self.selected.retain(|i| !dead(i));
            if self.cursor.as_ref().is_some_and(dead) {
                self.cursor = None;
            }
            if self.anchor.as_ref().is_some_and(dead) {
                self.anchor = None;
            }
        }
        any
    }

    /// Depth of a direct child of `dir` relative to this tab's folder.
    fn child_depth(&self, dir: &Path) -> u8 {
        dir.strip_prefix(&self.path).map(|r| r.components().count() as u8).unwrap_or(1).clamp(1, DEAD - 1)
    }

    pub(crate) fn set_entries(&mut self, v: Vec<Entry>) {
        // Remember selection/cursor by path across reloads.
        let restore = self.pending_restore.take().unwrap_or_else(|| HistState {
            path: self.path.clone(),
            cursor: self.cursor_path(),
            selected: self.selected_paths(),
            scroll: self.current_scroll,
        });
        self.entries = v;
        self.loaded_at = Some(std::time::Instant::now());
        self.trash_meta = if crate::trashcan::is_trash_files(&self.path) { crate::trashcan::info_for(&self.path) } else { HashMap::new() };
        self.tree_loads.clear();
        self.tree_listed.clear();
        self.tree_groups.clear();
        if self.tree_active() {
            let dirs: Vec<PathBuf> = self.expanded.iter().filter(|d| d.starts_with(&self.path) && d.is_dir()).cloned().collect();
            for d in dirs {
                self.load_children(&d);
            }
        }
        self.apply_restore(&restore);
        self.dirty = true;
    }

    fn apply_restore(&mut self, r: &HistState) {
        // A few paths (the usual case): one scan beats hashing every entry's path, which
        // cost tens of ms on a big flattened folder.
        let wanted = r.selected.len() + usize::from(r.cursor.is_some());
        if wanted <= 32 {
            let mut sel = HashSet::new();
            let mut cur = None;
            if wanted > 0 {
                // Raw byte comparison (length first); `Path ==` walks components.
                let cur_b = r.cursor.as_ref().map(|p| p.as_os_str());
                let sel_b: Vec<&std::ffi::OsStr> = r.selected.iter().map(|p| p.as_os_str()).collect();
                for (i, e) in self.entries.iter().enumerate() {
                    let b = e.path.as_os_str();
                    if cur_b == Some(b) {
                        cur = Some(i as u32);
                    }
                    if sel_b.contains(&b) {
                        sel.insert(i as u32);
                    }
                }
            }
            self.selected = sel;
            self.cursor = cur;
            self.anchor = self.cursor;
            return;
        }
        let index: HashMap<&Path, u32> = self.entries.iter().enumerate().map(|(i, e)| (e.path.as_path(), i as u32)).collect();
        self.selected = r.selected.iter().filter_map(|p| index.get(p.as_path()).copied()).collect();
        self.cursor = r.cursor.as_ref().and_then(|p| index.get(p.as_path()).copied());
        self.anchor = self.cursor;
        if let Some(c) = &r.cursor {
            if self.cursor.is_none() && !self.entries.is_empty() {
                // The cursor item vanished (deleted): keep the position stable (the caller handles it).
                let _ = c;
            }
        }
    }

    // ------------------------------------------------------------ navigation

    fn snapshot(&self) -> HistState {
        HistState { path: self.path.clone(), cursor: self.cursor_path(), selected: self.selected_paths(), scroll: self.current_scroll }
    }

    pub fn navigate(&mut self, path: PathBuf, settings: Option<(ViewSettings, Option<SaveScope>)>) {
        // A file path: go to its parent and select it (an archive opens like a folder).
        let enters = path.file_name().is_some_and(|n| crate::archive::is_archive(&n.to_string_lossy())) && crate::archive::available();
        if !is_computer(&path) && path.is_file() && !enters {
            if let Some(parent) = path.parent() {
                let p = parent.to_path_buf();
                self.navigate(p, settings);
                self.pending_restore = Some(HistState { path: self.path.clone(), cursor: Some(path.clone()), selected: vec![path], scroll: 0.0 });
                self.scroll_to_cursor = true;
                return;
            }
        }
        if path == self.path && !self.flatten {
            self.reload();
            return;
        }
        let snap = self.snapshot();
        if let Some(h) = self.history.get_mut(self.hist_pos) {
            *h = snap;
        }
        self.history.truncate(self.hist_pos + 1);
        self.history.push(HistState { path: path.clone(), ..Default::default() });
        self.hist_pos = self.history.len() - 1;
        self.go(path, None, settings);
    }

    fn go(&mut self, path: PathBuf, restore: Option<HistState>, settings: Option<(ViewSettings, Option<SaveScope>)>) {
        self.path = path;
        self.flatten = false;
        self.smart = None;
        self.expand_all_on = false;
        self.expand_rec.clear();
        self.filter.clear();
        self.last_filter.clear();
        self.rename = None;
        self.selected.clear();
        self.cursor = None;
        self.anchor = None;
        // Another folder's row starts where its cursor is, not flipping over from the last.
        self.stack_pos = -1.0;
        self.collapsed_groups.clear();
        self.expanded.clear();
        self.tree_loads.clear();
        self.tree_listed.clear();
        self.tree_groups.clear();
        self.cursor_first = restore.as_ref().and_then(|r| r.cursor.as_ref()).is_none();
        // A folder with a saved view shows it; any other keeps the view in use.
        match settings {
            Some((s, scope)) => {
                let before = std::mem::replace(&mut self.settings, s);
                // Browsing in Columns stays in Columns: a folder's saved view lends it
                // its sort, not a view that would take the columns away.
                self.settings.column_view |= before.column_view;
                self.settings.stack_view = (self.settings.stack_view || before.stack_view) && !self.settings.column_view;
                self.free_view.get_or_insert(before);
                self.saved_scope = scope;
            }
            None => {
                if let Some(f) = self.free_view.take() {
                    self.settings = f;
                }
                self.saved_scope = None;
            }
        }
        self.restore_scroll = Some(restore.as_ref().map(|r| r.scroll).unwrap_or(0.0));
        self.pending_restore = Some(restore.unwrap_or_default());
        if self.pending_restore.as_ref().map(|r| r.cursor.is_some()).unwrap_or(false) {
            self.scroll_to_cursor = true;
        }
        self.entries.clear();
        self.view.clear();
        self.rows.clear();
        self.group_labels.clear();
        self.reload();
    }

    pub fn can_back(&self) -> bool {
        self.hist_pos > 0
    }
    pub fn can_forward(&self) -> bool {
        self.hist_pos + 1 < self.history.len()
    }

    pub fn back(&mut self, settings: impl Fn(&Path) -> Option<(ViewSettings, Option<SaveScope>)>) {
        // A smart folder was opened from somewhere: back there, not to its plain folder.
        if self.flatten && (self.smart.is_none() || !self.can_back()) {
            self.set_flatten(false);
            return;
        }
        if !self.can_back() {
            return;
        }
        self.history[self.hist_pos] = self.snapshot();
        self.hist_pos -= 1;
        let h = self.history[self.hist_pos].clone();
        let s = settings(&h.path);
        self.go(h.path.clone(), Some(h), s);
    }

    pub fn forward(&mut self, settings: impl Fn(&Path) -> Option<(ViewSettings, Option<SaveScope>)>) {
        if !self.can_forward() {
            return;
        }
        self.history[self.hist_pos] = self.snapshot();
        self.hist_pos += 1;
        let h = self.history[self.hist_pos].clone();
        let s = settings(&h.path);
        self.go(h.path.clone(), Some(h), s);
    }

    /// Go to parent, placing the cursor on the folder we came from.
    pub fn up(&mut self, settings: impl Fn(&Path) -> Option<(ViewSettings, Option<SaveScope>)>) {
        if self.flatten {
            self.set_flatten(false);
            return;
        }
        if is_computer(&self.path) {
            return;
        }
        let Some(parent) = self.path.parent().filter(|_| !is_github(&self.path)).map(|p| p.to_path_buf()) else {
            let s = settings(Path::new(COMPUTER));
            self.navigate(PathBuf::from(COMPUTER), s);
            return;
        };
        let child = self.path.clone();
        let s = settings(&parent);
        self.navigate(parent, s);
        self.pending_restore = Some(HistState { path: self.path.clone(), cursor: Some(child), selected: vec![], scroll: 0.0 });
        self.scroll_to_cursor = true;
    }

    pub fn set_flatten(&mut self, on: bool) {
        if is_place(&self.path) || self.archive.is_some() || self.flatten == on {
            return;
        }
        self.flatten = on;
        self.smart = None;
        self.reload();
    }

    /// Show a smart folder: its folder, flattened to the files that match.
    pub fn open_smart(&mut self, sf: crate::smart::SmartFolder, settings: Option<(ViewSettings, Option<SaveScope>)>) {
        if self.path != sf.root || self.flatten {
            self.navigate(sf.root.clone(), settings);
        } else {
            // Saved from this folder's own search: the search is the smart folder now.
            if let Some((v, _)) = settings {
                self.settings = v;
            }
            self.filter.clear();
            self.last_filter.clear();
            self.selected.clear();
            self.cursor = None;
            self.anchor = None;
        }
        self.smart = Some(sf);
        self.flatten = true;
        self.reload();
    }

    // ------------------------------------------------------------ selection helpers

    pub fn cursor_path(&self) -> Option<PathBuf> {
        self.cursor.and_then(|i| self.entries.get(i as usize)).map(|e| e.path.clone())
    }

    pub fn selected_paths(&self) -> Vec<PathBuf> {
        let mut v: Vec<(usize, PathBuf)> = self
            .selected
            .iter()
            .filter_map(|&i| self.entries.get(i as usize).map(|e| (self.view_pos(i).unwrap_or(usize::MAX), e.path.clone())))
            .collect();
        v.sort();
        v.into_iter().map(|(_, p)| p).collect()
    }

    /// Selection in display order, or the cursor item if nothing is selected.
    pub fn targets(&self) -> Vec<PathBuf> {
        let s = self.selected_paths();
        if !s.is_empty() {
            return s;
        }
        self.cursor_path().into_iter().collect()
    }

    pub fn targets_idx(&self) -> Vec<u32> {
        if self.selected.is_empty() { self.cursor.into_iter().collect() } else { self.selected.iter().copied().collect() }
    }

    pub fn set_sort(&mut self, k: SortKey) {
        if self.settings.sort == k {
            self.settings.desc = !self.settings.desc;
        } else {
            self.settings.sort = k;
            self.settings.desc = k.default_desc();
        }
        self.dirty = true;
        self.scroll_to_cursor = true;
    }

    pub fn target_entries(&self) -> Vec<&Entry> {
        let mut idx: Vec<u32> = if self.selected.is_empty() { self.cursor.into_iter().collect() } else { self.selected.iter().copied().collect() };
        idx.sort_by_key(|i| self.view_pos(*i).unwrap_or(usize::MAX));
        idx.iter().filter_map(|&i| self.entries.get(i as usize)).collect()
    }

    pub fn view_pos(&self, idx: u32) -> Option<usize> {
        match self.pos_of.get(idx as usize) {
            Some(&p) if p != u32::MAX => Some(p as usize),
            _ => None,
        }
    }

    fn index_view(&mut self) {
        self.pos_of.clear();
        self.pos_of.resize(self.entries.len(), u32::MAX);
        for (p, &i) in self.view.iter().enumerate() {
            self.pos_of[i as usize] = p as u32;
        }
    }

    pub fn select_only(&mut self, idx: u32) {
        self.selected.clear();
        self.selected.insert(idx);
        self.cursor = Some(idx);
        self.anchor = Some(idx);
    }

    pub fn select_range_to(&mut self, idx: u32, additive: bool) {
        let a = self.anchor.and_then(|a| self.view_pos(a)).unwrap_or(0);
        let b = self.view_pos(idx).unwrap_or(0);
        if !additive {
            self.selected.clear();
        }
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        for &i in &self.view[lo..=hi.min(self.view.len().saturating_sub(1))] {
            self.selected.insert(i);
        }
        self.cursor = Some(idx);
    }

    /// Move the cursor by `delta` positions in the view.
    pub fn move_cursor(&mut self, delta: isize, extend: bool, cursor_only: bool) {
        if self.view.is_empty() {
            return;
        }
        let cur = self.cursor.and_then(|c| self.view_pos(c));
        let pos = match cur {
            None => {
                if delta >= 0 { 0 } else { self.view.len() - 1 }
            }
            Some(p) => (p as isize + delta).clamp(0, self.view.len() as isize - 1) as usize,
        };
        let idx = self.view[pos];
        if cursor_only {
            self.cursor = Some(idx);
        } else if extend {
            if self.anchor.is_none() {
                self.anchor = self.cursor.or(Some(idx));
            }
            self.select_range_to(idx, false);
        } else {
            self.select_only(idx);
        }
        self.scroll_to_cursor = true;
    }

    pub fn move_cursor_to(&mut self, pos: usize, extend: bool) {
        if self.view.is_empty() {
            return;
        }
        let pos = pos.min(self.view.len() - 1);
        match self.cursor.and_then(|c| self.view_pos(c)) {
            Some(cur) => self.move_cursor(pos as isize - cur as isize, extend, false),
            None => {
                let idx = self.view[pos];
                if extend {
                    self.anchor = Some(self.view[0]);
                    self.select_range_to(idx, false);
                } else {
                    self.select_only(idx);
                }
                self.scroll_to_cursor = true;
            }
        }
    }

    // ------------------------------------------------------------ view building

    /// Labels of the group headers currently shown, in order.
    pub fn header_labels(&self) -> Vec<String> {
        self.rows.iter().filter_map(|r| if let Row::Header { label, .. } = r { self.group_labels.get(*label as usize).cloned() } else { None }).collect()
    }

    pub fn type_key(e: &Entry) -> String {
        if e.is_dir { "folder".into() } else if e.ext().is_empty() { "(no extension)".into() } else { e.ext().to_string() }
    }

    /// Streaming rebuilds take at most about a fifth of the time (and at least every 0.5 s):
    /// results keep appearing while drawing stays smooth.
    pub fn rebuild_due(&self) -> Option<std::time::Duration> {
        let wait = std::time::Duration::from_secs_f32((self.last_rebuild_ms * 4.0 / 1000.0).clamp(0.0, 0.5));
        let since = self.last_rebuild.map(|t| t.elapsed()).unwrap_or(wait);
        wait.checked_sub(since).filter(|d| !d.is_zero())
    }

    pub fn rebuild(&mut self, c: &ViewCtx) {
        let t0 = std::time::Instant::now();
        // Contents mode filters by the files its search found instead of by name.
        let content = self.content.take();
        self.rebuild_inner(c, content.as_ref());
        self.content.put(content);
        self.view_gen += 1;
        self.last_rebuild = Some(std::time::Instant::now());
        self.last_rebuild_ms = t0.elapsed().as_secs_f32() * 1000.0;
    }

    fn rebuild_inner(&mut self, c: &ViewCtx, content: Option<&crate::contentsearch::Hits>) {
        self.dirty = false;
        self.stream_dirty = false;
        self.built_tree = self.tree_active();
        let walking = self.walk.as_ref().map(|w| !w.done.load(std::sync::atomic::Ordering::Relaxed)).unwrap_or(false);
        // Parsed once per rebuild, so ages count from now; the rows only run the result.
        let fq = crate::query::parse(&self.filter, std::time::SystemTime::now());
        let fcx = crate::query::Cx::view(c.stats, if content.is_some() { crate::query::Text::Off } else { crate::query::Text::Full });
        // A smart folder's words rank its files as the filter ranked them when it was
        // saved, until the filter has words of its own.
        let rank = self.smart.as_ref().filter(|s| s.tag.is_empty() && !fq.ranked(&fcx)).map(|s| crate::query::parse(&s.query_text(), std::time::SystemTime::now())).filter(|r| r.ranked(&fcx));
        let ranked = fq.ranked(&fcx) || rank.is_some();
        if fq.pending {
            // An owner's name waits for the list of users: look again shortly.
            self.stream_dirty = true;
            self.ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
        let only: HashSet<String> = self.settings.only_types.iter().cloned().collect();
        let (show_folders, show_files) = (self.settings.show_folders, self.settings.show_files);
        // A flattened walk lists every folder on the way; a tag's view lists those tagged.
        let flatten = self.flatten && !self.smart.as_ref().is_some_and(|s| !s.tag.is_empty());
        let db = crate::mime::mime_db();
        let (git_hide, here) = (self.git_hide.clone(), self.path.clone());
        let git_only = self.git_only.clone();
        let pick = self.pick.clone();
        let hidden_ok = |e: &Entry| -> bool {
            if !c.show_hidden && (e.hidden || e.rel_hidden()) {
                return false;
            }
            if git_hide.as_ref().is_some_and(|set| crate::gitstatus::is_ignored(set, &e.path, &here)) {
                return false;
            }
            c.show_system || !is_system(e)
        };
        // Filters that apply to every row (top level and tree children).
        let row_ok = |e: &Entry| -> bool {
            if e.is_dir && (!show_folders || flatten) || !e.is_dir && !show_files {
                return false;
            }
            // A folder stays when something below it changed.
            if git_only.as_ref().is_some_and(|set| !set.contains(&e.path)) {
                return false;
            }
            if content.is_some_and(|h| e.is_dir || !h.contains_key(&e.path)) {
                return false;
            }
            if pick.as_ref().is_some_and(|a| !a.ok(e)) {
                return false;
            }
            only.is_empty() || only.contains(e.special_kind().map(String::from).unwrap_or_else(|| db.label(&e.name, &e.ext(), e.is_dir, e.is_executable())).as_str())
        };
        let mut hidden_count = 0;
        let (mut tf, mut tfi) = (0, 0);
        let mut top: Vec<u32> = Vec::with_capacity(self.entries.len());
        let mut kids: HashMap<&Path, Vec<u32>> = HashMap::new();
        // Fuzzy score of each entry in `top` (same order), when filtering.
        let mut scores: Vec<i32> = vec![];
        let tree_view = self.tree_active();
        let tree = tree_view && fq.is_empty() && only.is_empty() && content.is_none() && pick.is_none();
        self.dimmed.clear();
        if tree_view && !tree {
            let ncx = crate::query::Cx::view(c.stats, crate::query::Text::Name);
            self.rebuild_tree_filtered(c, &|e| fq.score(e, &ncx).is_some(), &hidden_ok, &row_ok);
            return;
        }
        // Big flat lists (a flattened folder): filter and score in parallel chunks, then
        // concatenate in order (same result as the loop below).
        if !tree && self.entries.len() >= 40_000 {
            let threads = std::thread::available_parallelism().map(|t| t.get()).unwrap_or(1).clamp(1, 8);
            let chunk = self.entries.len().div_ceil(threads);
            let (hidden_ok, row_ok, fq, fcx, rank) = (&hidden_ok, &row_ok, &fq, &fcx, &rank);
            let parts: Vec<(Vec<u32>, Vec<i32>, usize, usize, usize)> = std::thread::scope(|sc| {
                let hs: Vec<_> = self
                    .entries
                    .chunks(chunk)
                    .enumerate()
                    .map(|(k, part)| {
                        sc.spawn(move || {
                            let (mut top, mut scores, mut hidden, mut tf, mut tfi) = (vec![], vec![], 0, 0, 0);
                            for (j, e) in part.iter().enumerate() {
                                let i = (k * chunk + j) as u32;
                                if e.depth != 0 {
                                    continue;
                                }
                                if e.is_dir { tf += 1 } else { tfi += 1 }
                                if !hidden_ok(e) {
                                    hidden += 1;
                                    continue;
                                }
                                if !row_ok(e) {
                                    continue;
                                }
                                match fq.score(e, fcx) {
                                    Some(sc) if ranked => scores.push(rank.as_ref().map_or(Some(sc), |r| r.score(e, fcx)).unwrap_or(0)),
                                    Some(_) => {}
                                    None => continue,
                                }
                                top.push(i);
                            }
                            (top, scores, hidden, tf, tfi)
                        })
                    })
                    .collect();
                hs.into_iter().map(|h| h.join().unwrap_or_default()).collect()
            });
            for (t, sc, h, a, b) in parts {
                top.extend(t);
                scores.extend(sc);
                hidden_count += h;
                tf += a;
                tfi += b;
            }
            self.hidden_count = hidden_count;
            self.finish_rebuild_with_totals(c, top, HashMap::new(), scores, walking, (tf, tfi));
            return;
        }
        for (i, e) in self.entries.iter().enumerate() {
            if e.depth == DEAD {
                continue;
            }
            if e.depth > 0 {
                continue; // tree children: grouped below from `tree_groups`
            }
            if e.is_dir { tf += 1 } else { tfi += 1 }
            if !hidden_ok(e) {
                hidden_count += 1;
                continue;
            }
            if !row_ok(e) {
                continue;
            }
            match fq.score(e, &fcx) {
                Some(sc) if ranked => scores.push(rank.as_ref().map_or(Some(sc), |r| r.score(e, &fcx)).unwrap_or(0)),
                Some(_) => {}
                None => continue,
            }
            top.push(i as u32);
        }
        if tree {
            for (parent, r) in &self.tree_groups {
                let v: Vec<u32> = (r.start..r.end).filter(|&i| {
                    let e = &self.entries[i as usize];
                    e.depth != DEAD && e.depth > 0 && hidden_ok(e) && row_ok(e)
                }).collect();
                if !v.is_empty() {
                    kids.insert(parent.as_path(), v);
                }
            }
        }
        self.hidden_count = hidden_count;
        let kids: HashMap<PathBuf, Vec<u32>> = kids.into_iter().map(|(k, v)| (k.to_path_buf(), v)).collect();
        self.finish_rebuild_with_totals(c, top, kids, scores, walking, (tf, tfi));
    }

    /// Tree view while filtering by text or type: matches stay inside their hierarchy.
    /// Searched are the items currently shown in the tree (top level plus unfolded
    /// folders); a folder that does not match itself but leads to a match is kept as
    /// dimmed context, so you can see where each match lives.
    fn rebuild_tree_filtered(&mut self, c: &ViewCtx, q: &dyn Fn(&Entry) -> bool, hidden_ok: &dyn Fn(&Entry) -> bool, row_ok: &dyn Fn(&Entry) -> bool) {
        let walking = self.walk.as_ref().map(|w| !w.done.load(std::sync::atomic::Ordering::Relaxed)).unwrap_or(false);
        // Only ever asked for ancestor folders: index those (thousands), not every entry.
        let by_path: HashMap<&Path, u32> = self.entries.iter().enumerate().filter(|(_, e)| e.depth != DEAD && e.is_dir).map(|(i, e)| (e.path.as_path(), i as u32)).collect();
        // An item is reachable when every folder above it (inside the tab) is unfolded.
        let reachable = |e: &Entry| -> bool {
            let mut p = e.path.parent();
            let mut d = e.depth;
            while d > 0 {
                let Some(dir) = p else { return false };
                if !self.expanded.contains(dir) {
                    return false;
                }
                match by_path.get(dir) {
                    Some(&j) if hidden_ok(&self.entries[j as usize]) => {}
                    _ => return false,
                }
                d -= 1;
                p = dir.parent();
            }
            true
        };
        let (mut tf, mut tfi, mut hidden_count) = (0, 0, 0);
        let mut keep: HashSet<u32> = HashSet::new();
        let mut matched: HashSet<u32> = HashSet::new();
        for (i, e) in self.entries.iter().enumerate() {
            if e.depth == DEAD {
                continue;
            }
            if e.depth == 0 {
                if e.is_dir { tf += 1 } else { tfi += 1 }
                if !hidden_ok(e) {
                    hidden_count += 1;
                    continue;
                }
            } else if !hidden_ok(e) {
                continue;
            }
            if !row_ok(e) || !q(e) || !reachable(e) {
                continue;
            }
            matched.insert(i as u32);
            keep.insert(i as u32);
            let mut p = e.path.parent();
            for _ in 0..e.depth {
                let Some(dir) = p else { break };
                if let Some(&j) = by_path.get(dir) {
                    if !keep.insert(j) {
                        break;
                    }
                }
                p = dir.parent();
            }
        }
        let mut top = vec![];
        let mut kids: HashMap<&Path, Vec<u32>> = HashMap::new();
        for &i in &keep {
            let e = &self.entries[i as usize];
            if e.depth == 0 {
                top.push(i);
            } else if let Some(p) = e.path.parent() {
                kids.entry(p).or_default().push(i);
            }
        }
        self.dimmed = keep.difference(&matched).copied().collect();
        self.hidden_count = hidden_count;
        let kids: HashMap<PathBuf, Vec<u32>> = kids.into_iter().map(|(k, v)| (k.to_path_buf(), v)).collect();
        let _ = (tf, tfi);
        self.finish_rebuild_with_totals(c, top, kids, vec![], walking, (tf, tfi));
    }

    fn finish_rebuild_with_totals(&mut self, c: &ViewCtx, mut top: Vec<u32>, mut kids: HashMap<PathBuf, Vec<u32>>, scores: Vec<i32>, walking: bool, (tf, tfi): (usize, usize)) {
        // Sort (skip while a big walk is still streaming in; sort once it finishes).
        let entries = &self.entries;
        let s = &self.settings;
        let tm = &self.trash_meta;
        let cmp = |a: &u32, b: &u32| compare(&entries[*a as usize], &entries[*b as usize], s.sort, s.desc, c, tm);
        if !(walking && entries.len() > 50_000) {
            // Filtering: best matches first (coarse tiers), then the chosen sort.
            let tier = |k: usize| if scores.len() == top.len() { -(scores[k] / 25) } else { 0 };
            // Numeric sort keys (name, size, dates, counts): sort compact records rather than
            // indices into `entries`. Each comparison then touches 48 contiguous bytes
            // instead of two random 136-byte entries (cache misses dominated big sorts), and
            // folder sizes are looked up once per item instead of once per comparison.
            let when = |t: Option<SystemTime>| -> u64 {
                let ns: i64 = match t.unwrap_or(SystemTime::UNIX_EPOCH).duration_since(SystemTime::UNIX_EPOCH) {
                    Ok(d) => d.as_nanos().min(i64::MAX as u128) as i64,
                    Err(e) => -(e.duration().as_nanos().min(i64::MAX as u128) as i64),
                };
                (ns as u64) ^ (1 << 63) // order-preserving signed -> unsigned
            };
            let stat = |e: &Entry| c.stats.get(&e.path);
            // Keys read from caches filled off this thread; an item without one goes last
            // either way round.
            let keyed: Option<Vec<Option<u64>>> = match s.sort {
                SortKey::Tags => Some(crate::tags::sort_keys(top.iter().map(|&i| entries[i as usize].path.as_path()))),
                SortKey::Taken | SortKey::Dimensions | SortKey::Duration => Some(top.iter().map(|&i| crate::facts::sort_key(&entries[i as usize], s.sort)).collect()),
                SortKey::Colour => Some(top.iter().map(|&i| crate::colours::sort_key(&entries[i as usize])).collect()),
                _ => None,
            };
            let by_name = |a: &u32, b: &u32| compare(&entries[*a as usize], &entries[*b as usize], SortKey::Name, s.desc, c, tm);
            let primary = |e: &Entry| -> Option<u64> {
                Some(match s.sort {
                    SortKey::Name => 0,
                    SortKey::Size | SortKey::Usage => if e.is_dir { stat(e).map(|x| x.size).unwrap_or(0) } else { e.size },
                    SortKey::Modified => when(e.modified),
                    SortKey::Created => when(e.created),
                    SortKey::Accessed => when(e.accessed),
                    SortKey::Folders => stat(e).map(|x| x.folders as u64).unwrap_or(0),
                    SortKey::Files => stat(e).map(|x| x.files as u64).unwrap_or(0),
                    SortKey::Deleted => when(tm.get(&e.path).and_then(|t| t.1)),
                    SortKey::Type | SortKey::Original | SortKey::Tags | SortKey::Taken | SortKey::Dimensions | SortKey::Duration | SortKey::Colour => return None,
                })
            };
            if !matches!(s.sort, SortKey::Type | SortKey::Original) || keyed.is_some() {
                // Name tie-break: the first 16 bytes of the lowercase name. Where two
                // prefixes first differ, byte order equals natural order unless a number is
                // being compared there (a digit at that byte or just before it); only
                // those cases and full ties need the complete comparison.
                let mut dec: Vec<(i32, u8, u64, u128, u32)> = top
                    .iter()
                    .enumerate()
                    .map(|(k, &i)| {
                        let e = &entries[i as usize];
                        let b = e.lname().as_bytes();
                        let mut p = [0u8; 16];
                        let n = b.len().min(16);
                        p[..n].copy_from_slice(&b[..n]);
                        // Recents on top (one lookup per item, not per comparison), then
                        // folders-first.
                        let recent = c.recents_on_top && c.recents.contains(&e.path);
                        let pinned = !c.pinned.is_empty() && c.pinned.contains(&e.path);
                        let (missing, key) = match &keyed {
                            Some(v) => (v[k].is_none(), v[k].unwrap_or(0)),
                            None => (false, primary(e).unwrap_or(0)),
                        };
                        let rank = if pinned { 0 } else { 8 } + if recent { 0 } else { 4 } + if e.is_dir == c.folders_first { 0 } else { 2 } + missing as u8;
                        (tier(k), rank, key, u128::from_be_bytes(p), i)
                    })
                    .collect();
                par_sort_by(&mut dec, &|a: &(i32, u8, u64, u128, u32), b: &(i32, u8, u64, u128, u32)| {
                    a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)).then_with(|| {
                        if a.2 != b.2 {
                            let o = a.2.cmp(&b.2);
                            return if s.desc { o.reverse() } else { o };
                        }
                        if a.3 != b.3 {
                            let k = ((a.3 ^ b.3).leading_zeros() / 8) as usize;
                            let (x, y) = (a.3.to_be_bytes(), b.3.to_be_bytes());
                            if !x[k].is_ascii_digit() && !y[k].is_ascii_digit() && (k == 0 || !x[k - 1].is_ascii_digit()) {
                                let o = x[k].cmp(&y[k]);
                                return if s.desc { o.reverse() } else { o };
                            }
                        }
                        // The key read from a cache is settled already: by name from here.
                        if keyed.is_some() { by_name(&a.4, &b.4) } else { cmp(&a.4, &b.4) }
                    })
                });
                top = dec.into_iter().map(|d| d.4).collect();
            } else if scores.len() != top.len() {
                par_sort_by(&mut top, &cmp);
            } else {
                let mut tiered: Vec<(i32, u32)> = top.iter().enumerate().map(|(k, &i)| (tier(k), i)).collect();
                par_sort_by(&mut tiered, &|(ta, a): &(i32, u32), (tb, b): &(i32, u32)| ta.cmp(tb).then_with(|| cmp(a, b)));
                top = tiered.into_iter().map(|(_, i)| i).collect();
            }
        }
        // Every unfolded folder's children: thousands of small sorts after an "expand
        // all", spread over threads when there are enough rows to be worth it.
        let mut groups: Vec<&mut Vec<u32>> = kids.values_mut().collect();
        let rows: usize = groups.iter().map(|v| v.len()).sum();
        let threads = std::thread::available_parallelism().map(|t| t.get()).unwrap_or(1).min(8);
        if rows > 20_000 && threads > 1 {
            let per = groups.len().div_ceil(threads);
            std::thread::scope(|sc| {
                for chunk in groups.chunks_mut(per.max(1)) {
                    sc.spawn(|| {
                        for v in chunk.iter_mut() {
                            merge_sort(v, &cmp);
                        }
                    });
                }
            });
        } else {
            for v in groups {
                merge_sort(v, &cmp);
            }
        }
        self.build_rows(top, &kids, c.stats);
        // Shown out of the folder's own items, as the totals count them: unfolded tree
        // rows are not in the totals (a folder of 1 showed "3/1" with two kids open).
        let (mut vf, mut vfi) = (0, 0);
        for &i in &self.view {
            let e = &self.entries[i as usize];
            if e.depth > 0 {
                continue;
            }
            if e.is_dir { vf += 1 } else { vfi += 1 }
        }
        self.counts = (vf, tf, vfi, tfi);
        self.last_filter = self.filter.clone();
        self.index_view();
        // Apply a pending restore once results are complete.
        if !walking {
            if let Some(r) = self.pending_restore.take() {
                self.apply_restore(&r);
            }
            if self.cursor_first && self.loading.is_none() {
                self.cursor_first = false;
                if self.cursor.is_none() {
                    self.cursor = self.view.first().copied();
                    self.anchor = self.cursor;
                }
            }
        }
        // Keep the cursor valid.
        if let Some(cur) = self.cursor {
            if (cur as usize) >= self.entries.len() {
                self.cursor = None;
            }
        }
        self.selected.retain(|&i| (i as usize) < self.entries.len());
        // When filtering, land on the first match.
        if !self.filter.is_empty() {
            // What the filter hides is not acted on: F2 or Delete after typing a name
            // renamed or trashed a file still selected from before, out of sight.
            let view: std::collections::HashSet<u32> = self.view.iter().copied().collect();
            self.selected.retain(|i| view.contains(i));
            let cur_visible = self.cursor.map(|c| self.view_pos(c).is_some()).unwrap_or(false);
            if !cur_visible {
                if let Some(&first) = self.view.first() {
                    self.cursor = Some(first);
                    self.anchor = Some(first);
                    self.scroll_to_cursor = true;
                }
            }
        }
    }

    /// Rows in display order: optional group headers, top-level items, and expanded
    /// tree children inline under their folders. `view` = the navigable items.
    fn build_rows(&mut self, top: Vec<u32>, kids: &HashMap<PathBuf, Vec<u32>>, stats: &HashMap<PathBuf, crate::stats::DirStats>) {
        self.rows.clear();
        self.group_labels.clear();
        let mut ordered: Vec<(Option<(String, usize, bool)>, Vec<u32>)> = vec![];
        // A flattened walk can be huge and is not grouped; the Timeline is bounded by its
        // month and reads by day.
        let timeline = self.smart.as_ref().is_some_and(|s| s.id == crate::smart::TIMELINE);
        // Columns lists each folder in one run, as its other columns do.
        if self.settings.group == GroupBy::None || self.settings.column_view || self.settings.stack_view || (self.flatten && !timeline) {
            ordered.push((None, top));
        } else {
            let mut order: Vec<String> = vec![];
            let mut groups: HashMap<String, Vec<u32>> = HashMap::new();
            let parent_total: u64 = top.iter().map(|&i| {
                let e = &self.entries[i as usize];
                if e.is_dir { stats.get(&e.path).map(|s| s.size).unwrap_or(0) } else { e.size }
            }).sum();
            let gc = GroupCtx { stats, vols: &self.vols, trash: &self.trash_meta, parent_total };
            for &i in &top {
                let label = group_label(&self.entries[i as usize], self.settings.group, &gc);
                if !groups.contains_key(&label) {
                    order.push(label.clone());
                }
                groups.entry(label).or_default().push(i);
            }
            let rank = |l: &str| group_rank(l, self.settings.group);
            order.sort_by(|a, b| {
                let o = rank(a).cmp(&rank(b)).then_with(|| a.to_lowercase().cmp(&b.to_lowercase()));
                if self.settings.desc && matches!(self.settings.group, GroupBy::Name | GroupBy::Type) { o.reverse() } else { o }
            });
            for l in order {
                let items = groups.remove(&l).unwrap_or_default();
                let collapsed = self.collapsed_groups.contains(&l);
                ordered.push((Some((l, items.len(), collapsed)), items));
            }
        }
        fn push_tree(rows: &mut Vec<Row>, entries: &[Entry], expanded: &HashSet<PathBuf>, kids: &HashMap<PathBuf, Vec<u32>>, i: u32, guard: u32) {
            rows.push(Row::Item(i));
            let e = &entries[i as usize];
            if e.is_dir && guard < 64 && expanded.contains(&e.path) {
                for &k in kids.get(&e.path).map(|v| v.as_slice()).unwrap_or(&[]) {
                    push_tree(rows, entries, expanded, kids, k, guard + 1);
                }
            }
        }
        for (hdr, items) in ordered {
            let collapsed = match hdr {
                Some((label, count, collapsed)) => {
                    self.rows.push(Row::Header { label: self.group_labels.len() as u32, count: count.min(u32::MAX as usize) as u32, collapsed });
                    self.group_labels.push(label);
                    collapsed
                }
                None => false,
            };
            if !collapsed {
                for i in items {
                    push_tree(&mut self.rows, &self.entries, &self.expanded, kids, i, 0);
                }
            }
        }
        // Keyboard navigation follows display order and skips collapsed groups.
        self.view = self.rows.iter().filter_map(|r| if let Row::Item(i) = r { Some(*i) } else { None }).collect();
    }

    /// Distinct type labels in this listing (for the Types popup), most common first.
    /// Like `type_counts`, plus one entry of each type (for its icon).
    pub fn type_samples(&self, show_hidden: bool, show_system: bool) -> Vec<(String, usize, Entry)> {
        let db = crate::mime::mime_db();
        let mut m: HashMap<String, (usize, usize)> = HashMap::new();
        for (i, e) in self.entries.iter().enumerate() {
            if e.depth == DEAD || !show_hidden && e.hidden || !show_system && is_system(e) || self.flatten && e.is_dir {
                continue;
            }
            m.entry(e.special_kind().map(String::from).unwrap_or_else(|| db.label(&e.name, &e.ext(), e.is_dir, e.is_executable()))).or_insert((0, i)).0 += 1;
        }
        let mut v: Vec<(String, usize, Entry)> = m.into_iter().map(|(k, (n, i))| (k, n, self.entries[i].clone())).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v
    }

    pub fn type_counts(&self, show_hidden: bool, show_system: bool) -> Vec<(String, usize)> {
        let db = crate::mime::mime_db();
        let mut m: HashMap<String, usize> = HashMap::new();
        for e in &self.entries {
            if e.depth == DEAD || !show_hidden && e.hidden || !show_system && is_system(e) || self.flatten && e.is_dir {
                continue;
            }
            *m.entry(e.special_kind().map(String::from).unwrap_or_else(|| db.label(&e.name, &e.ext(), e.is_dir, e.is_executable()))).or_default() += 1;
        }
        let mut v: Vec<_> = m.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v
    }
}

pub fn volume_label(device: &str, mount: &Path) -> String {
    if mount == Path::new("/") {
        return "Root".into();
    }
    // /dev/disk/by-label/<label> -> ../../sdX
    if let Ok(rd) = std::fs::read_dir("/dev/disk/by-label") {
        let dev = std::fs::canonicalize(device).unwrap_or_else(|_| PathBuf::from(device));
        for de in rd.flatten() {
            if std::fs::canonicalize(de.path()).ok().as_deref() == Some(dev.as_path()) {
                return de.file_name().to_string_lossy().replace("\\x20", " ");
            }
        }
    }
    mount.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| mount.display().to_string())
}

pub fn is_system(e: &Entry) -> bool {
    e.special || e.name.ends_with('~') || e.ext() == "bak"
}

/// Natural, case-insensitive comparison ("file2" < "file10").
/// Stable sort, split across threads for big lists (chunks sorted in parallel, then
/// merged pairwise in parallel). Same result as `sort_by`; several times faster on the
/// hundreds of thousands of items a flattened folder can hold.
/// Stable sort for small Copy items: insertion-sorted runs of 24, then pairwise merges
/// through one buffer, skipping pairs already in order. Release builds use the standard
/// library built for size, whose sort is several times slower on large lists.
pub fn merge_sort<T: Copy, F: Fn(&T, &T) -> Ordering>(v: &mut [T], f: &F) {
    const RUN: usize = 32;
    let n = v.len();
    // Already in order (a re-sort, children listed by name): one pass and done.
    if v.windows(2).all(|w| f(&w[1], &w[0]) != Ordering::Less) {
        return;
    }
    // Binary insertion: few comparisons, which is what costs when they compare names.
    for c in v.chunks_mut(RUN) {
        for i in 1..c.len() {
            let x = c[i];
            let (mut lo, mut hi) = (0, i);
            while lo < hi {
                let mid = (lo + hi) / 2;
                if f(&x, &c[mid]) == Ordering::Less { hi = mid } else { lo = mid + 1 }
            }
            c.copy_within(lo..i, lo + 1);
            c[lo] = x;
        }
    }
    if n <= RUN {
        return;
    }
    let mut buf = v.to_vec();
    let (mut width, mut in_v) = (RUN, true);
    while width < n {
        let (src, dst): (&[T], &mut [T]) = if in_v { (&*v, &mut buf[..]) } else { (&buf[..], &mut *v) };
        for (k, out) in dst.chunks_mut(2 * width).enumerate() {
            let lo = k * 2 * width;
            let mid = (lo + width).min(n);
            let hi = (lo + 2 * width).min(n);
            let (a, b) = (&src[lo..mid], &src[mid..hi]);
            if b.is_empty() || f(&b[0], &a[a.len() - 1]) != Ordering::Less {
                out[..a.len()].copy_from_slice(a);
                out[a.len()..].copy_from_slice(b);
                continue;
            }
            merge(a, b, out, f);
        }
        in_v = !in_v;
        width *= 2;
    }
    if !in_v {
        v.copy_from_slice(&buf);
    }
}

/// Merge two sorted runs into `out` (their combined length). Branch-free in the loop:
/// on shuffled input a mispredicted branch per item costs more than the comparison.
fn merge<T: Copy, F: Fn(&T, &T) -> Ordering>(a: &[T], b: &[T], out: &mut [T], f: &F) {
    let (mut i, mut j, mut k) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        // Stable: equal items keep the left run's first.
        let take_b = f(&b[j], &a[i]) == Ordering::Less;
        out[k] = if take_b { b[j] } else { a[i] };
        j += take_b as usize;
        i += !take_b as usize;
        k += 1;
    }
    out[k..k + a.len() - i].copy_from_slice(&a[i..]);
    out[k + a.len() - i..].copy_from_slice(&b[j..]);
}

pub fn par_sort_by<T: Copy + Send + Sync, F: Fn(&T, &T) -> Ordering + Sync>(v: &mut [T], f: &F) {
    let n = v.len();
    let threads = std::thread::available_parallelism().map(|t| t.get()).unwrap_or(1).min(8);
    if n < 40_000 || threads < 2 {
        merge_sort(v, f);
        return;
    }
    let mut width = n.div_ceil(threads);
    std::thread::scope(|s| {
        for c in v.chunks_mut(width) {
            s.spawn(move || merge_sort(c, f));
        }
    });
    let mut buf = v.to_vec();
    let mut in_v = true; // where the current runs live
    while width < n {
        {
            let (src, dst): (&[T], &mut [T]) = if in_v { (&*v, &mut buf[..]) } else { (&buf[..], &mut *v) };
            std::thread::scope(|s| {
                for (k, out) in dst.chunks_mut(2 * width).enumerate() {
                    let lo = k * 2 * width;
                    let mid = (lo + width).min(n);
                    let hi = (lo + 2 * width).min(n);
                    let (a, b) = (&src[lo..mid], &src[mid..hi]);
                    s.spawn(move || merge(a, b, out, f));
                }
            });
        }
        in_v = !in_v;
        width *= 2;
    }
    if !in_v {
        v.copy_from_slice(&buf);
    }
}

pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    // Bytes, not chars: UTF-8 byte order equals code point order, and digit runs are ASCII.
    // No allocation (this runs millions of times when sorting a big flattened folder).
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    loop {
        match (a.get(i), b.get(j)) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let ea = i + a[i..].iter().take_while(|c| c.is_ascii_digit()).count();
                let eb = j + b[j..].iter().take_while(|c| c.is_ascii_digit()).count();
                let (na, nb) = (&a[i..ea], &b[j..eb]);
                let za = na.iter().take_while(|&&c| c == b'0').count();
                let zb = nb.iter().take_while(|&&c| c == b'0').count();
                let (ta, tb) = (&na[za..], &nb[zb..]);
                let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb)).then_with(|| na.len().cmp(&nb.len()));
                if o != Ordering::Equal {
                    return o;
                }
                i = ea;
                j = eb;
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(y);
                }
                i += 1;
                j += 1;
            }
        }
    }
}

fn compare(a: &Entry, b: &Entry, key: SortKey, desc: bool, c: &ViewCtx, trash: &HashMap<PathBuf, (PathBuf, Option<SystemTime>)>) -> Ordering {
    if !c.pinned.is_empty() {
        let (pa, pb) = (c.pinned.contains(&a.path), c.pinned.contains(&b.path));
        if pa != pb {
            return pb.cmp(&pa);
        }
    }
    if c.recents_on_top {
        let (ra, rb) = (c.recents.contains(&a.path), c.recents.contains(&b.path));
        if ra != rb {
            return rb.cmp(&ra);
        }
    }
    if a.is_dir != b.is_dir {
        let dirs_first = if a.is_dir { Ordering::Less } else { Ordering::Greater };
        return if c.folders_first { dirs_first } else { dirs_first.reverse() };
    }
    let size = |e: &Entry| if e.is_dir { c.stats.get(&e.path).map(|s| s.size).unwrap_or(0) } else { e.size };
    let o = match key {
        SortKey::Name => Ordering::Equal,
        SortKey::Type => a.ext().cmp(&b.ext()),
        SortKey::Size | SortKey::Usage => size(a).cmp(&size(b)),
        SortKey::Modified => a.modified.unwrap_or(SystemTime::UNIX_EPOCH).cmp(&b.modified.unwrap_or(SystemTime::UNIX_EPOCH)),
        SortKey::Created => a.created.unwrap_or(SystemTime::UNIX_EPOCH).cmp(&b.created.unwrap_or(SystemTime::UNIX_EPOCH)),
        SortKey::Accessed => a.accessed.unwrap_or(SystemTime::UNIX_EPOCH).cmp(&b.accessed.unwrap_or(SystemTime::UNIX_EPOCH)),
        SortKey::Deleted => trash.get(&a.path).and_then(|t| t.1).unwrap_or(SystemTime::UNIX_EPOCH).cmp(&trash.get(&b.path).and_then(|t| t.1).unwrap_or(SystemTime::UNIX_EPOCH)),
        SortKey::Original => trash.get(&a.path).map(|t| t.0.as_path()).cmp(&trash.get(&b.path).map(|t| t.0.as_path())),
        SortKey::Folders => c.stats.get(&a.path).map(|s| s.folders).unwrap_or(0).cmp(&c.stats.get(&b.path).map(|s| s.folders).unwrap_or(0)),
        SortKey::Files => c.stats.get(&a.path).map(|s| s.files).unwrap_or(0).cmp(&c.stats.get(&b.path).map(|s| s.files).unwrap_or(0)),
        SortKey::Tags => {
            let lead = |p: &Path| crate::tags::get(p, |t| crate::tags::leading(t).map(|l| (crate::tags::rank(l), l.to_lowercase()))).flatten();
            match (lead(&a.path), lead(&b.path)) {
                (Some(x), Some(y)) => x.cmp(&y),
                // Without one last, whichever way round.
                (x, y) => return y.is_none().cmp(&x.is_none()).reverse().then_with(|| natural_cmp(a.lname(), b.lname())),
            }
        }
        SortKey::Taken | SortKey::Dimensions | SortKey::Duration => match (crate::facts::sort_key(a, key), crate::facts::sort_key(b, key)) {
            (Some(x), Some(y)) => x.cmp(&y),
            (x, y) => return y.is_none().cmp(&x.is_none()).reverse().then_with(|| natural_cmp(a.lname(), b.lname())),
        },
        SortKey::Colour => match (crate::colours::sort_key(a), crate::colours::sort_key(b)) {
            (Some(x), Some(y)) => x.cmp(&y),
            (x, y) => return y.is_none().cmp(&x.is_none()).reverse().then_with(|| natural_cmp(a.lname(), b.lname())),
        },
    };
    let o = o.then_with(|| natural_cmp(&a.lname(), &b.lname()));
    if desc { o.reverse() } else { o }
}

pub struct GroupCtx<'a> {
    pub stats: &'a HashMap<PathBuf, crate::stats::DirStats>,
    pub vols: &'a HashMap<PathBuf, VolInfo>,
    pub trash: &'a HashMap<PathBuf, (PathBuf, Option<SystemTime>)>,
    pub parent_total: u64,
}

const DATE_GROUPS: [&str; 10] = ["Today", "Yesterday", "Earlier this week", "Last week", "Earlier this month", "Last month", "Earlier this year", "Last year", "Older", "Unknown"];

fn date_group(t: Option<SystemTime>) -> String {
    let Some(m) = t else { return "Unknown".into() };
    let nowt = std::time::SystemTime::now();
    let days = crate::timefmt::local_day(nowt) - crate::timefmt::local_day(m);
    let (now, d) = (crate::timefmt::local(nowt), crate::timefmt::local(m));
    let wd = ((now.tm_wday + 6) % 7) as i64; // days since Monday
    if days <= 0 {
        "Today"
    } else if days == 1 {
        "Yesterday"
    } else if days <= wd {
        "Earlier this week"
    } else if days <= wd + 7 {
        "Last week"
    } else if d.tm_mon == now.tm_mon && d.tm_year == now.tm_year {
        "Earlier this month"
    } else if (now.tm_year * 12 + now.tm_mon) - (d.tm_year * 12 + d.tm_mon) == 1 {
        "Last month"
    } else if d.tm_year == now.tm_year {
        "Earlier this year"
    } else if d.tm_year == now.tm_year - 1 {
        "Last year"
    } else {
        "Older"
    }
    .into()
}

fn group_label(e: &Entry, g: GroupBy, c: &GroupCtx) -> String {
    let dir_stats = || c.stats.get(&e.path);
    match g {
        GroupBy::None => String::new(),
        GroupBy::Name => match e.lname().chars().next() {
            Some(ch) if ('a'..='h').contains(&ch) => "A – H".into(),
            Some(ch) if ('i'..='p').contains(&ch) => "I – P".into(),
            Some(ch) if ('q'..='z').contains(&ch) => "Q – Z".into(),
            _ => "Other".into(),
        },
        // Same labels as the Type column.
        GroupBy::Type => crate::mime::mime_db().label(&e.name, &e.ext(), e.is_dir, e.is_executable()),
        GroupBy::Modified => date_group(e.modified),
        GroupBy::Created => date_group(e.created),
        GroupBy::Accessed => date_group(e.accessed),
        GroupBy::Deleted => date_group(c.trash.get(&e.path).and_then(|t| t.1)),
        GroupBy::Original => c.trash.get(&e.path).and_then(|t| t.0.parent()).map(|p| p.display().to_string()).unwrap_or_else(|| "Unknown".into()),
        GroupBy::Size => {
            if e.is_dir {
                return "Folders".into();
            }
            match e.size {
                0 => "Empty".into(),
                s if s < 16 << 10 => "Tiny (< 16 KB)".into(),
                s if s < 1 << 20 => "Small (< 1 MB)".into(),
                s if s < 128 << 20 => "Medium (< 128 MB)".into(),
                s if s < 1 << 30 => "Large (< 1 GB)".into(),
                _ => "Huge (≥ 1 GB)".into(),
            }
        }
        GroupBy::TotalItems => {
            if !e.is_dir {
                return "Files".into();
            }
            match dir_stats().map(|s| s.files + s.folders) {
                None => "Not counted yet".into(),
                Some(0) => "Empty".into(),
                Some(n) if n < 10 => "1 – 9 items".into(),
                Some(n) if n < 100 => "10 – 99 items".into(),
                Some(n) if n < 1000 => "100 – 999 items".into(),
                Some(_) => "1000+ items".into(),
            }
        }
        GroupBy::Usage => {
            let s = if e.is_dir { dir_stats().map(|s| s.size).unwrap_or(0) } else { e.size };
            if c.parent_total == 0 {
                return "< 1%".into();
            }
            match s as f64 / c.parent_total as f64 {
                f if f >= 0.5 => "Over half".into(),
                f if f >= 0.1 => "10 – 50%".into(),
                f if f >= 0.01 => "1 – 10%".into(),
                _ => "< 1%".into(),
            }
        }
        GroupBy::FreeSpace | GroupBy::PercentFull if c.vols.get(&e.path).is_some_and(|v| !v.mounted) => "Not mounted".into(),
        GroupBy::FreeSpace => match c.vols.get(&e.path) {
            Some(v) => match v.free {
                f if f < 1 << 30 => "Under 1 GB free".into(),
                f if f < 10 << 30 => "1 – 10 GB free".into(),
                f if f < 100 << 30 => "10 – 100 GB free".into(),
                _ => "Over 100 GB free".into(),
            },
            None => "Unknown".into(),
        },
        GroupBy::FileSystem => c.vols.get(&e.path).map(|v| v.fstype.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| "Unknown".into()),
        // The first tag only: an item shows once, under the tag it sorts by.
        GroupBy::Tag => crate::tags::get(&e.path, |t| crate::tags::leading(t).cloned()).flatten().unwrap_or_else(|| NO_TAGS.into()),
        // By the month: photos span years, and a day's group would hold a handful.
        GroupBy::Taken => crate::facts::month_label(e),
        GroupBy::Origin => crate::origin::group(e),
        GroupBy::PercentFull => match c.vols.get(&e.path) {
            Some(v) if v.total > 0 => match (v.total - v.free) as f64 / v.total as f64 {
                f if f >= 0.9 => "Nearly full (90%+)".into(),
                f if f >= 0.5 => "Over half full".into(),
                _ => "Under half full".into(),
            },
            _ => "Unknown".into(),
        },
    }
}

const NO_TAGS: &str = "No tags";

fn group_rank(l: &str, g: GroupBy) -> usize {
    if g == GroupBy::Tag {
        return if l == NO_TAGS { 99 } else { crate::tags::rank(l) as usize };
    }
    if g == GroupBy::Taken {
        return crate::facts::month_rank(l);
    }
    if g == GroupBy::Origin {
        return crate::origin::rank(l);
    }
    let order: &[&str] = match g {
        GroupBy::Name => &["A – H", "I – P", "Q – Z", "Other"],
        GroupBy::Type => &["Folder"],
        GroupBy::Modified | GroupBy::Created | GroupBy::Accessed | GroupBy::Deleted => &DATE_GROUPS,
        GroupBy::Size => &["Folders", "Empty", "Tiny (< 16 KB)", "Small (< 1 MB)", "Medium (< 128 MB)", "Large (< 1 GB)", "Huge (≥ 1 GB)"],
        GroupBy::TotalItems => &["1000+ items", "100 – 999 items", "10 – 99 items", "1 – 9 items", "Empty", "Not counted yet", "Files"],
        GroupBy::Usage => &["Over half", "10 – 50%", "1 – 10%", "< 1%"],
        GroupBy::FreeSpace => &["Under 1 GB free", "1 – 10 GB free", "10 – 100 GB free", "Over 100 GB free", "Unknown"],
        GroupBy::PercentFull => &["Nearly full (90%+)", "Over half full", "Under half full", "Unknown"],
        GroupBy::None | GroupBy::Original | GroupBy::FileSystem | GroupBy::Tag | GroupBy::Taken | GroupBy::Origin => &[],
    };
    order.iter().position(|o| *o == l).unwrap_or(if g == GroupBy::Type { 1 } else { 99 })
}
