//! Application state, input routing, command dispatch and persistence.ggg

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{self, Event, Key, Modifiers, Pos2, Rect, vec2};

use crate::commands::{self, Chord, Cmd, Keymap};
use crate::config::{self, Config, LayoutSnap, NewTabShows, SaveScope, Session, SortKey, Startup, TabSnap, UserData, ViewSettings};
use crate::fsx;
use crate::inspector::Inspector;
use crate::ops::{self, Job, OpKind};
use crate::panel::{Node, Panel, Side};
use crate::popups::{ConflictItem, Popup};
use crate::rename::{Outcome, Rename};
use crate::stats::Stats;
use crate::tab::{self, COMPUTER, Tab, ViewCtx};
use crate::theme::{self, Palette};
use crate::thumbs::Thumbs;
use crate::watch::Watch;

/// Cross-cutting requests produced while drawing; applied after the frame's UI.
pub enum Action {
    Cmd(Cmd),
    Navigate { panel: u64, path: PathBuf },
    NewTab { panel: u64, path: PathBuf, select: Option<PathBuf>, focus: bool },
    Split { panel: u64, path: PathBuf, side: Side, select: Option<PathBuf> },
    Activate { panel: u64, idx: u32, mods: Modifiers },
    OpenFile(PathBuf),
    CtxMenu { panel: u64, pos: Pos2, background: bool },
    Drop { paths: Vec<PathBuf>, dest: PathBuf, copy: bool, link: bool },
    RunWith { exe: PathBuf, args: Vec<PathBuf> },
    FocusPanel(u64),
    ActivateTab { panel: u64, idx: usize },
    CloseTab { panel: u64, idx: usize },
    MoveTab { from: u64, idx: usize, to: u64, to_idx: Option<usize>, side: Option<Side>, copy: bool },
    BookmarkAdd(PathBuf),
    BookmarkRemove(PathBuf),
    BookmarkMove { from: usize, to: usize },
    CommitRename(u64),
    CancelRename(u64),
    Toast(String),
    /// A preview's Rendered / Source choice, remembered for its kind.
    SourceView(crate::inspector::RichKind, bool),
    Open(Popup),
    StartJob(OpKind),
    ExternalDrop { paths: Vec<PathBuf>, dest: PathBuf },
    /// Show a smart folder (by id), here or in a new tab.
    OpenSmart { panel: u64, id: u64, new_tab: bool, focus: bool },
    /// Work that needs the whole app, handed up from a view that holds only its part.
    Run(Box<dyn FnOnce(&mut App)>),
}

pub enum Drag {
    Tab { panel: u64, idx: usize, title: String },
    Files { paths: Vec<PathBuf>, from_panel: u64, right: bool },
    Bookmark { idx: usize },
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct UndoEntry {
    pub label: String,
    pub kind: UndoKind,
}

/// What Undo can take back, the newest last, each with when it was done; and what Redo
/// can do again, the next one last, each with when it was undone. The entries are shared
/// with the thread that writes them to disk, so writing never copies them on a frame.
#[derive(Default)]
pub struct UndoStack {
    items: Vec<std::sync::Arc<UndoEntry>>,
    at: Vec<std::time::SystemTime>,
    redo: Vec<std::sync::Arc<UndoEntry>>,
    redo_at: Vec<std::time::SystemTime>,
    /// Counts every change, for the file that keeps them across restarts.
    pub(crate) generation: u64,
}

impl UndoStack {
    /// A new change: what was undone before it can't be redone any more.
    pub fn push(&mut self, e: UndoEntry) {
        self.forget_redo();
        self.push_redone(e);
    }

    /// A change done again by Redo: the rest of what was undone stays to redo.
    pub fn push_redone(&mut self, e: UndoEntry) {
        self.items.push(std::sync::Arc::new(e));
        self.at.push(std::time::SystemTime::now());
        crate::undofile::cap(&mut self.items, &mut self.at);
        self.generation += 1;
    }

    pub fn pop(&mut self) -> Option<UndoEntry> {
        self.at.pop();
        let e = self.items.pop()?;
        self.generation += 1;
        Some(std::sync::Arc::unwrap_or_clone(e))
    }

    /// A change taken back, for Redo.
    pub fn push_undone(&mut self, e: UndoEntry) {
        self.redo.push(std::sync::Arc::new(e));
        self.redo_at.push(std::time::SystemTime::now());
        crate::undofile::cap(&mut self.redo, &mut self.redo_at);
        self.generation += 1;
    }

    pub fn pop_redo(&mut self) -> Option<UndoEntry> {
        self.redo_at.pop();
        let e = self.redo.pop()?;
        self.generation += 1;
        Some(std::sync::Arc::unwrap_or_clone(e))
    }

    pub fn forget_redo(&mut self) {
        if !self.redo.is_empty() {
            self.redo.clear();
            self.redo_at.clear();
            self.generation += 1;
        }
    }

    /// When the change at `i` (counted from the oldest) was made.
    pub fn when(&self, i: usize) -> Option<std::time::SystemTime> {
        self.at.get(i).copied()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn last(&self) -> Option<&UndoEntry> {
        self.items.last().map(|e| &**e)
    }

    /// What Redo can do again, the next one last.
    pub fn redos(&self) -> impl ExactSizeIterator<Item = &UndoEntry> + DoubleEndedIterator {
        self.redo.iter().map(|e| &**e)
    }

    /// When the change Redo would do `i`-th from the bottom was undone.
    pub fn undone_when(&self, i: usize) -> Option<std::time::SystemTime> {
        self.redo_at.get(i).copied()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Both lists as they stand, for the thread that writes them.
    pub(crate) fn snapshot(&self) -> crate::undofile::Snapshot {
        let pair = |v: &[std::sync::Arc<UndoEntry>], t: &[std::time::SystemTime]| v.iter().cloned().zip(t.iter().copied()).collect();
        crate::undofile::Snapshot { undo: pair(&self.items, &self.at), redo: pair(&self.redo, &self.redo_at) }
    }

    /// What the last run left, under what was done since it started. A change made since
    /// then leaves nothing to redo.
    pub(crate) fn restore(&mut self, s: crate::undofile::Snapshot) {
        let fresh = !self.items.is_empty();
        let (mut items, mut at): (Vec<_>, Vec<_>) = s.undo.into_iter().unzip();
        items.append(&mut self.items);
        at.append(&mut self.at);
        crate::undofile::cap(&mut items, &mut at);
        (self.items, self.at) = (items, at);
        if !fresh && self.redo.is_empty() {
            (self.redo, self.redo_at) = s.redo.into_iter().unzip();
        }
        self.generation += 1;
    }
}

impl std::ops::Index<usize> for UndoStack {
    type Output = UndoEntry;
    fn index(&self, i: usize) -> &UndoEntry {
        &self.items[i]
    }
}

impl UndoKind {
    /// How many files or folders the change touched.
    pub fn count(&self) -> usize {
        match self {
            UndoKind::Rename(v) | UndoKind::Move(v) | UndoKind::Sorted(v, _) | UndoKind::MovedOver(v, _) => v.len(),
            UndoKind::Copied(v, _) => v.len(),
            UndoKind::Created(v) | UndoKind::Trashed(v) | UndoKind::Replaced(v) => v.len(),
            UndoKind::Synced(made, _, removed) => made.len() + removed.len(),
            UndoKind::Tagged(v) => v.len(),
        }
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub enum UndoKind {
    Rename(Vec<(PathBuf, PathBuf)>),
    Move(Vec<(PathBuf, PathBuf)>),
    Created(Vec<PathBuf>),
    Trashed(Vec<PathBuf>),
    /// Pictures changed in place, their originals in the trash.
    Replaced(Vec<PathBuf>),
    /// Files moved into folders made for them (outermost first), which go again if
    /// nothing else is in them by then.
    Sorted(Vec<(PathBuf, PathBuf)>, Vec<PathBuf>),
    /// Copies made, and among them those that replaced something (which is in the trash).
    Copied(Vec<PathBuf>, Vec<PathBuf>),
    /// A move over things that were there (in the trash now).
    MovedOver(Vec<(PathBuf, PathBuf)>, Vec<PathBuf>),
    /// A sync: copies made, those among them over something (in the trash now), and what
    /// mirroring sent to the trash.
    Synced(Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>),
    /// Files' tags changed: each one's before and after.
    Tagged(Vec<crate::tags::Change>),
}

pub struct App {
    pub cfg: Config,
    pub ud: UserData,
    pub keymap: Keymap,
    pub pal: Palette,
    pub panels: HashMap<u64, Panel>,
    pub tree: Node,
    pub active: u64,
    pub prev_active: u64,
    pub next_id: u64,
    pub closed_tabs: Vec<TabSnap>,
    pub thumbs: Thumbs,
    pub stats: Stats,
    pub watch: Watch,
    pub jobs: Vec<Job>,
    /// The status bar's transfers chip as last drawn, and the panel whose bar has it.
    pub jobs_chip: Option<egui::Rect>,
    pub jobs_panel: u64,
    pub clipboard: Option<(Vec<PathBuf>, bool)>,
    /// The sets copied and cut this session, newest first (Paste From Earlier).
    pub pastes: Vec<crate::pastes::Set>,
    /// Pictures and text from the clipboard being written as files.
    pub clip_files: Vec<crate::clipfile::Pending>,
    pub popup: Popup,
    pub sidebar: bool,
    pub sidebar_width: f32,
    pub toast: Option<(String, Instant)>,
    pub seq: Vec<Chord>,
    pub seq_time: Instant,
    pub drag: Option<Drag>,
    pub recents_set: HashSet<PathBuf>,
    pub pinned_set: HashSet<PathBuf>,
    pub actions: Vec<Action>,
    pub ud_dirty: bool,
    pub cfg_dirty: bool,
    pub last_save: Instant,
    pub ud_mtime: Option<std::time::SystemTime>,
    pub save_session: bool,
    pub jump_buf: (String, Instant),
    pub undo: UndoStack,
    /// Undo and Redo kept across restarts.
    pub undo_keep: crate::undofile::Keeper,
    /// Jobs that move data take turns on each disk.
    pub queue: std::sync::Arc<ops::Queue>,
    /// Moves that Undo or Redo handed to a job, by job: what to record when it ends.
    pub job_steps: HashMap<u64, crate::redo::Step>,
    pub theme_applied: (theme::ThemeName, f32, f32, bool, bool),
    pub ctx: egui::Context,
    pub fresh: bool,
    /// Set to close the window at the end of the frame.
    pub quit: bool,
    pub quick_anchor: Option<Pos2>,
    /// The drag that just ended used the right mouse button (drop shows a menu).
    pub drop_right: bool,
    /// Theme previewed while hovering a swatch in Options.
    pub preview_theme: Option<theme::ThemeName>,
    pub font_applied: String,
    /// Panel rectangles as drawn, easing toward the split layout (splits and closes animate).
    pub shown_rects: std::collections::HashMap<u64, egui::Rect>,
    /// Panels along the top edge: their tab strips live in the title bar.
    pub title_tab_panels: Vec<u64>,
    /// Closed tabs whose slot is still collapsing: (panel, index, width, when).
    pub tab_ghosts: Vec<(u64, usize, f32, std::time::Instant)>,
    /// When background work last produced results; memory is handed back once things settle.
    pub trim_after: Option<std::time::Instant>,
    /// When the first trim-worthy work after the last trim happened.
    pub trim_pending_since: Option<std::time::Instant>,
    /// Mounts, unmounts and ejects in progress.
    pub drive_jobs: Vec<crate::drives::Job>,
    /// Drive list generation the Computer views were last loaded with.
    pub drives_seen: u64,
    /// Input to add to the next frame (pointer state the system took over during drag
    /// and drop, and drags from other apps).
    pub inject: Vec<egui::Event>,
    /// Letter tags on every control, while choosing one by its letters.
    pub hints: Option<crate::hints::Hints>,
    /// A system drag is over the window: Some(true) when it is Maia's own, carried out
    /// and back in.
    pub ext_drag: Option<bool>,
    /// A system drag was dropped this frame (Some(own)).
    pub ext_drop: Option<bool>,
    /// Folders handed over by later launches (single instance).
    pub requests: Option<std::sync::mpsc::Receiver<crate::single::Request>>,
    /// Whether Maia is the desktop's file manager: 0 not known yet, 1 no, 2 yes, 3 asking.
    default_fm: std::sync::Arc<std::sync::atomic::AtomicU8>,
    /// Messages from background work, shown as toasts.
    notes: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    /// Files opened from archives that want a password: (archive, refused, file).
    pw_asks: std::sync::Arc<std::sync::Mutex<Vec<(PathBuf, bool, PathBuf)>>>,
    /// The archive folder the password was last asked for (asked once per landing).
    pw_asked: Option<PathBuf>,
    /// The tag cache's generation last seen.
    tags_gen: u64,
    /// The origin cache's generation last seen.
    origin_gen: u64,
    /// Search by meaning, while it is switched on, and the folders it was started for.
    pub semantic: Option<crate::semantic::Engine>,
    semantic_for: Option<String>,
    /// Authors' GitHub pictures for the history (idle while the setting is off).
    pub avatars: crate::avatars::Avatars,
    pub github: crate::github::Page,
    /// A new theme washing over the old window (`wash.rs`).
    pub wash: crate::wash::Wash,
    /// Updates from the project's releases, when asked.
    pub updater: crate::update::Updater,
    /// Network places being mounted.
    net_jobs: Vec<NetJob>,
    /// When the trash was last looked through for items past their days, and the days
    /// it was last set to.
    trash_aged: Option<Instant>,
    trash_days: u32,
    /// Copies and .gitignore changes running on threads.
    shares: Vec<std::sync::mpsc::Receiver<ShareDone>>,
    /// How the Connect dialog's last attempt ended: (address, Ok or the reason).
    dialog_result: Option<(String, Result<(), String>)>,
    /// Wallpapers, printing and picture fixes, run one after another.
    pub errands: crate::errands::Runner,
    /// Files gathered to move or copy together, for this session.
    pub shelf: crate::shelf::Shelf,
    /// The terminal under the panels.
    pub terminal: crate::terminal::Pane,
    /// The desktop's light or dark preference as the portal last said, and as last applied.
    appearance: std::sync::Arc<std::sync::atomic::AtomicU8>,
    appearance_seen: u8,
    /// The theme left behind when the desktop last switched, to come back to.
    pub other_theme: Option<theme::ThemeName>,
    /// This window answers an app's Open or Save request (picker.rs).
    pub picker: Option<Box<crate::picker::Picker>>,
    /// Whether apps open and save files with Maia: 0 not known yet, 1 no, 2 yes, 3 asking;
    /// and what keeps it from working here.
    picker_on: std::sync::Arc<std::sync::atomic::AtomicU8>,
    picker_missing: std::sync::Arc<std::sync::Mutex<Option<&'static str>>>,
}

/// What a thread sharing files hands back: text for the clipboard, what to say, and a
/// work tree to read again.
struct ShareDone {
    copy: Option<String>,
    /// Addresses to open in the browser.
    open: Vec<String>,
    toast: String,
    rescan: Option<PathBuf>,
    /// Renames made in a panel's folder: (panel, from and to).
    renamed: Option<(u64, Vec<(PathBuf, PathBuf)>)>,
}

/// A network place being mounted, and where to show it once it is.
struct NetJob {
    uri: String,
    open_in: Option<(u64, bool)>,
    from_dialog: bool,
    rx: std::sync::mpsc::Receiver<Result<PathBuf, String>>,
}

impl App {
    pub fn new(ctx: &egui::Context, paths: Vec<PathBuf>) -> App {
        let ctx = ctx.clone();
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let (font_family, cfg_mono) = {
            let c = config::load_config();
            (c.font_family, c.inspector_font)
        };
        theme::install_fonts(&ctx, &font_family, &cfg_mono);
        crate::mime::preload();
        crate::freeup::note_home_later();
        // Nothing of Maia's on disk yet: a first start (the welcome tour).
        let first = [config::config_path(), config::userdata_path(), config::session_path()].iter().all(|p| !p.exists());
        let cfg = config::load_config();
        let ud = config::load_userdata();
        let mut pal = theme::palette(cfg.theme);
        if !cfg.syntax_theme {
            pal.syntax = theme::Syntax::classic(pal.dark);
        }
        theme::apply(&ctx, &pal, cfg.font_size, cfg.spacing, cfg.animations);
        let session: Session = config::load_json(&config::session_path());
        let mut app = App {
            keymap: Keymap::build(&cfg.hotkeys),
            pal,
            panels: HashMap::new(),
            tree: Node::Leaf(0),
            active: 0,
            prev_active: 0,
            next_id: 1,
            closed_tabs: vec![],
            thumbs: Thumbs::new(ctx.clone(), 3),
            stats: Stats::new(ctx.clone()),
            watch: Watch::new(ctx.clone()),
            jobs: vec![],
            jobs_chip: None,
            jobs_panel: 0,
            clipboard: None,
            pastes: vec![],
            clip_files: vec![],
            popup: Popup::None,
            sidebar: if session.sidebar_width > 0.0 { session.sidebar } else { true },
            sidebar_width: if session.sidebar_width > 0.0 { session.sidebar_width } else { 176.0 },
            toast: None,
            seq: vec![],
            seq_time: Instant::now(),
            drag: None,
            recents_set: HashSet::new(),
            pinned_set: HashSet::new(),
            actions: vec![],
            ud_dirty: false,
            cfg_dirty: false,
            last_save: Instant::now(),
            ud_mtime: config::mtime(&config::userdata_path()),
            save_session: paths.is_empty(),
            jump_buf: (String::new(), Instant::now()),
            undo: UndoStack::default(),
            // Tests share one home: only those that check keeping Undo ask for it.
            undo_keep: crate::undofile::Keeper::new((!cfg!(test)).then(crate::undofile::path), &ctx),
            queue: Default::default(),
            job_steps: HashMap::new(),
            theme_applied: (cfg.theme, cfg.font_size, cfg.spacing, cfg.animations, cfg.syntax_theme),
            ctx: ctx.clone(),
            fresh: true,
            quit: false,
            quick_anchor: None,
            drop_right: false,
            preview_theme: None,
            font_applied: format!("{}\u{1f}{}", font_family, cfg_mono),
            shown_rects: Default::default(),
            title_tab_panels: vec![],
            tab_ghosts: vec![],
            trim_after: None,
            trim_pending_since: None,
            drive_jobs: vec![],
            drives_seen: 0,
            inject: vec![],
            hints: None,
            ext_drag: None,
            ext_drop: None,
            requests: None,
            default_fm: Default::default(),
            notes: Default::default(),
            pw_asks: Default::default(),
            pw_asked: None,
            tags_gen: 0,
            origin_gen: 0,
            semantic: None,
            semantic_for: None,
            avatars: crate::avatars::Avatars::new(),
            github: Default::default(),
            wash: Default::default(),
            updater: Default::default(),
            net_jobs: vec![],
            trash_aged: None,
            trash_days: cfg.trash_days,
            shares: vec![],
            dialog_result: None,
            errands: Default::default(),
            shelf: Default::default(),
            terminal: crate::terminal::Pane::new(session.terminal_height),
            appearance: Default::default(),
            appearance_seen: crate::appearance::UNKNOWN,
            other_theme: None,
            picker: None,
            picker_on: Default::default(),
            picker_missing: Default::default(),
            cfg,
            ud,
        };
        app.refresh_recents();
        if !cfg!(test) {
            let c = ctx.clone();
            crate::appearance::watch(app.appearance.clone(), move || c.request_repaint());
        }
        // Startup: explicit paths → those; else last session / home / layout.
        let mut restored = false;
        if paths.is_empty() {
            let layout = match &app.cfg.startup {
                Startup::LastSession => session.layout.clone(),
                Startup::Layout(name) => app.ud.layouts.get(name).cloned(),
                Startup::Home => None,
            };
            if let Some(l) = layout {
                restored = app.apply_layout(&l);
            }
        }
        if !restored {
            let id = app.alloc_id();
            let first = paths.first().cloned().unwrap_or_else(fsx::home);
            let t = app.make_tab(first);
            let mut p = Panel::new(id, t);
            for extra in paths.iter().skip(1) {
                let t = app.make_tab(extra.clone());
                p.tabs.push(t);
            }
            app.panels.insert(id, p);
            app.tree = Node::Leaf(id);
            app.active = id;
            app.prev_active = id;
        }
        // A picker window is there for one answer: no tour, no news.
        if !crate::picker::picking() {
            crate::tour::at_start(&mut app, first);
        }
        app
    }

    pub fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn view_for(&self, path: &Path) -> (ViewSettings, Option<SaveScope>) {
        self.ud.view_for(path)
    }

    /// The view to switch to on going to `path`: None keeps the tab's own.
    pub fn nav_view(ud: &UserData, path: &Path) -> Option<(ViewSettings, Option<SaveScope>)> {
        if tab::is_computer(path) { Some(ud.view_for(path)) } else { ud.saved_view_for(path) }
    }

    pub fn make_tab(&mut self, path: PathBuf) -> Tab {
        let id = self.alloc_id();
        let path = if tab::is_place(&path) || path.exists() { path } else { fsx::home() };
        let (v, scope) = self.view_for(&path);
        let mut t = Tab::new(id, path.clone(), v, self.ctx.clone());
        t.saved_scope = scope;
        if self.ud.saved_view_for(&path).is_some() {
            t.free_view = Some(self.ud.default_view.clone().unwrap_or_default());
        }
        if path.is_file() {
            t.navigate(path, None);
        }
        t
    }

    pub fn panel(&mut self) -> &mut Panel {
        if !self.panels.contains_key(&self.active) {
            self.active = self.tree.first_leaf();
        }
        if !self.panels.contains_key(&self.active) {
            // Layout and panels disagree (should not happen): recover with a fresh panel
            // instead of crashing.
            let id = self.active;
            let t = self.make_tab(fsx::home());
            self.panels.insert(id, Panel::new(id, t));
        }
        self.panels.get_mut(&self.active).expect("active panel")
    }

    pub fn tab(&mut self) -> &mut Tab {
        self.panel().tab_mut()
    }

    /// Pinned items keep their pin under a new name in the same folder.
    pub fn follow_pins(&mut self, pairs: &[(PathBuf, PathBuf)]) {
        let mut moved = false;
        for p in &mut self.ud.pinned {
            if let Some((_, to)) = pairs.iter().find(|(from, _)| from == p) {
                *p = to.clone();
                moved = true;
            }
        }
        if moved {
            self.refresh_recents();
            self.ud_dirty = true;
        }
    }

    pub fn refresh_recents(&mut self) {
        self.recents_set = self.ud.recents.iter().take(30).cloned().collect();
        self.pinned_set = self.ud.pinned.iter().cloned().collect();
    }

    pub fn toast(&mut self, s: impl Into<String>) {
        let s: String = s.into();
        self.toast = Some((fsx::plain_error(&s).to_string(), Instant::now()));
    }

    /// Drag and drop with other apps: a drag over the window becomes an ordinary file
    /// drag (every drop target works), and the drop a release at its position.
    pub fn on_dnd(&mut self, ev: crate::clipboard::Dnd) {
        use crate::clipboard::Dnd;
        let z = self.ctx.zoom_factor();
        let files = |paths| Some(Drag::Files { paths, from_panel: 0, right: false });
        match ev {
            Dnd::Over { x, y, paths, own } => {
                self.inject.push(egui::Event::PointerMoved(Pos2::new(x / z, y / z)));
                if !matches!(self.drag, Some(Drag::Files { .. })) {
                    self.drag = files(paths);
                }
                self.ext_drag = Some(own);
            }
            Dnd::Dropped { x, y, paths, own } => {
                let p = Pos2::new(x / z, y / z);
                if !matches!(self.drag, Some(Drag::Files { .. })) {
                    self.drag = files(paths);
                }
                self.ext_drag = None;
                self.ext_drop = Some(own);
                let modifiers = self.ctx.input(|i| i.modifiers);
                self.inject.push(egui::Event::PointerMoved(p));
                self.inject.push(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers });
            }
            Dnd::Left => {
                if self.ext_drag.take().is_some() {
                    self.drag = None;
                    self.inject.push(egui::Event::PointerGone);
                }
            }
        }
    }

    /// Whether Maia opens folders for other apps (None while that is being read; the first
    /// call starts reading it in the background).
    pub fn default_fm(&self) -> Option<bool> {
        use std::sync::atomic::Ordering::Relaxed;
        match self.default_fm.load(Relaxed) {
            1 => Some(false),
            2 => Some(true),
            3 => None,
            _ => {
                self.default_fm.store(3, Relaxed);
                let (state, ctx) = (self.default_fm.clone(), self.ctx.clone());
                std::thread::spawn(move || {
                    state.store(if crate::install::is_default() { 2 } else { 1 }, Relaxed);
                    ctx.request_repaint();
                });
                None
            }
        }
    }

    /// Make Maia the desktop's file manager, or hand folders back, in the background.
    pub fn set_default_fm(&mut self, on: bool) {
        use std::sync::atomic::Ordering::Relaxed;
        self.default_fm.store(if on { 2 } else { 1 }, Relaxed);
        let (state, notes, ctx) = (self.default_fm.clone(), self.notes.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let r = if on { crate::install::set_default() } else { crate::install::unset_default() };
            let msg = r.unwrap_or_else(|e| format!("Couldn't change the default file manager: {e}"));
            state.store(if crate::install::is_default() { 2 } else { 1 }, Relaxed);
            notes.lock().unwrap_or_else(|e| e.into_inner()).push(msg);
            ctx.request_repaint();
        });
    }

    /// Whether apps open and save files with Maia (None while that is being read; the
    /// first call starts reading it in the background).
    pub fn picker_setting(&self) -> Option<bool> {
        use std::sync::atomic::Ordering::Relaxed;
        match self.picker_on.load(Relaxed) {
            1 => Some(false),
            2 => Some(true),
            3 => None,
            _ => {
                self.picker_on.store(3, Relaxed);
                let (state, missing, ctx) = (self.picker_on.clone(), self.picker_missing.clone(), self.ctx.clone());
                std::thread::spawn(move || {
                    let (on, m) = crate::portal::state();
                    *missing.lock().unwrap_or_else(|e| e.into_inner()) = m;
                    state.store(if on { 2 } else { 1 }, Relaxed);
                    ctx.request_repaint();
                });
                None
            }
        }
    }

    /// What keeps the portal from reaching Maia here, once known.
    pub fn picker_missing(&self) -> Option<&'static str> {
        *self.picker_missing.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Route the desktop's Open and Save dialogs to Maia, or back, in the background.
    pub fn set_picker_setting(&mut self, on: bool) {
        use std::sync::atomic::Ordering::Relaxed;
        self.picker_on.store(if on { 2 } else { 1 }, Relaxed);
        let (state, notes, ctx) = (self.picker_on.clone(), self.notes.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let r = crate::portal::set(on);
            let msg = r.unwrap_or_else(|e| format!("Couldn't change the Open and Save dialog: {e}"));
            state.store(if crate::portal::is_on(&crate::portal::Places::user()) { 2 } else { 1 }, Relaxed);
            notes.lock().unwrap_or_else(|e| e.into_inner()).push(msg);
            ctx.request_repaint();
        });
    }

    /// Mount a network place in the background, then show it in `open_in`.
    pub fn connect(&mut self, uri: String, login: crate::network::Login, open_in: Option<(u64, bool)>) {
        self.start_net(uri, login, open_in, false);
    }

    pub fn connect_from_dialog(&mut self, uri: String, login: crate::network::Login, panel: u64) {
        self.dialog_result = None;
        self.start_net(uri, login, Some((panel, false)), true);
    }

    fn start_net(&mut self, uri: String, login: crate::network::Login, open_in: Option<(u64, bool)>, from_dialog: bool) {
        let Some(tools) = crate::drives::Tools::current().filter(|t| t.has("gio")) else {
            self.toast("Network places need GVFS (the gio command)");
            return;
        };
        if self.connecting(&uri) {
            return;
        }
        let u = uri.clone();
        let rx = crate::network::spawn(&self.ctx, move || crate::network::mount(&tools, &u, &login));
        self.net_jobs.push(NetJob { uri, open_in, from_dialog, rx });
    }

    pub fn connecting(&self, uri: &str) -> bool {
        self.net_jobs.iter().any(|j| j.uri == uri)
    }

    /// The dialog's attempt at `uri` failed: why (once).
    pub fn take_connect_error(&mut self, uri: &str) -> Option<String> {
        match &self.dialog_result {
            Some((u, Err(_))) if u == uri => self.dialog_result.take().and_then(|r| r.1.err()),
            _ => None,
        }
    }

    /// The dialog's attempt at `uri` succeeded (the dialog can close).
    pub fn connected(&mut self, uri: &str) -> bool {
        let ok = matches!(&self.dialog_result, Some((u, Ok(()))) if u == uri);
        if ok {
            self.dialog_result = None;
        }
        ok
    }

    /// Unmount a network place; tabs inside it move to Computer first.
    pub fn disconnect(&mut self, uri: String, path: Option<PathBuf>) {
        let Some(tools) = crate::drives::Tools::current().filter(|t| t.has("gio")) else { return };
        if let Some(path) = path {
            let s = self.view_for(Path::new(tab::COMPUTER));
            for p in self.panels.values_mut() {
                for t in &mut p.tabs {
                    if t.path.starts_with(&path) {
                        t.navigate(PathBuf::from(tab::COMPUTER), Some(s.clone()));
                    }
                }
            }
        }
        let (notes, ctx) = (self.notes.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let msg = match crate::network::unmount(&tools, &uri) {
                Ok(()) => format!("Disconnected {}", crate::network::label(&uri)),
                Err(e) => format!("Couldn't disconnect {}: {e}", crate::network::label(&uri)),
            };
            notes.lock().unwrap_or_else(|e| e.into_inner()).push(msg);
            crate::sidebar::invalidate();
            ctx.request_repaint();
        });
    }

    /// Email `files`, the photos among them without their place and camera.
    pub fn email_clean(&mut self, files: Vec<PathBuf>) {
        let ctx = self.ctx.clone();
        self.share(&ctx, move || {
            let toast = match crate::scrub::copies(&files, crate::drives::Tools::current().as_ref()).and_then(|f| crate::sendto::email(&f)) {
                Ok(()) => "Opening a new email, the photos without their place".to_string(),
                Err(e) => e,
            };
            ShareDone { copy: None, open: vec![], toast, rescan: None, renamed: None }
        });
    }

    fn share(&mut self, ctx: &egui::Context, work: impl FnOnce() -> ShareDone + Send + 'static) {
        let (tx, rx) = std::sync::mpsc::channel();
        let c = ctx.clone();
        let started = std::thread::Builder::new().name("share".into()).spawn(move || {
            let _ = tx.send(work());
            c.request_repaint();
        });
        if started.is_ok() {
            self.shares.push(rx);
        }
    }

    /// Pictures and text pasted as files: named as they land, like a new file.
    fn pump_clip_files(&mut self) {
        let mut i = 0;
        while i < self.clip_files.len() {
            let Ok(done) = self.clip_files[i].rx.try_recv() else {
                i += 1;
                continue;
            };
            let p = self.clip_files.remove(i);
            match done {
                crate::clipfile::Pasted::File(f) => {
                    self.undo.push(UndoEntry { label: format!("Paste {}", f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()), kind: UndoKind::Created(vec![f.clone()]) });
                    let dir = f.parent().map(Path::to_path_buf).unwrap_or_default();
                    match self.panels.get_mut(&p.panel).and_then(|pn| pn.tabs.iter_mut().find(|t| t.id == p.tab)).filter(|t| t.path == dir) {
                        Some(t) => {
                            t.filter.clear();
                            t.pending_rename = Some(f);
                            t.reload();
                        }
                        None => self.toast(format!("Pasted {}", f.display())),
                    }
                }
                crate::clipfile::Pasted::Nothing => self.paste_own(p.dest),
                crate::clipfile::Pasted::Failed(e) => self.toast(e),
            }
        }
    }

    fn pump_shares(&mut self, ctx: &egui::Context) {
        let mut i = 0;
        while i < self.shares.len() {
            match self.shares[i].try_recv() {
                Ok(done) => {
                    self.shares.remove(i);
                    if let Some(t) = done.copy {
                        ctx.copy_text(t);
                    }
                    for u in &done.open {
                        if let Err(e) = crate::mime::open_uri(u) {
                            self.toast(format!("Couldn't open the link: {e}"));
                            return;
                        }
                    }
                    if let Some(root) = done.rescan {
                        crate::gitstatus::rescan(&root);
                    }
                    if let Some((panel, pairs)) = done.renamed.filter(|r| !r.1.is_empty()) {
                        let label = format!("Rename {} items", pairs.len());
                        if let Some(t) = self.panels.get_mut(&panel).map(|p| p.tab_mut()) {
                            let new: Vec<PathBuf> = pairs.iter().map(|p| p.1.clone()).collect();
                            t.pending_restore = Some(tab::HistState { path: t.path.clone(), cursor: new.first().cloned(), selected: new, scroll: t.current_scroll });
                            t.reload();
                        }
                        self.follow_pins(&pairs);
                        self.undo.push(UndoEntry { label, kind: UndoKind::Rename(pairs) });
                    }
                    if !done.toast.is_empty() {
                        self.toast(done.toast);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => i += 1,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.shares.remove(i);
                }
            }
        }
    }

    /// With a number of days chosen, the trash is looked through on a thread when Maia
    /// starts and every six hours it runs; only frames that happen anyway check. A new
    /// choice waits until it has been left alone a while: the choice steps through 7
    /// days on its way to 30, and that must not take what 30 would keep.
    fn age_trash(&mut self) {
        // The main window's job: a picker is open for a few seconds.
        if self.picker.is_some() {
            return;
        }
        const EVERY: Duration = Duration::from_secs(6 * 3600);
        const SETTLE: Duration = Duration::from_secs(if cfg!(test) { 1 } else { 60 });
        let days = self.cfg.trash_days;
        if days != self.trash_days {
            self.trash_days = days;
            self.trash_aged = Instant::now().checked_sub(EVERY - SETTLE);
            return;
        }
        if days == 0 || self.trash_aged.is_some_and(|t| t.elapsed() < EVERY) {
            return;
        }
        self.trash_aged = Some(Instant::now());
        let (notes, ctx) = (self.notes.clone(), self.ctx.clone());
        let _ = std::thread::Builder::new().name("trash age".into()).spawn(move || {
            let n = crate::trashcan::empty_older_everywhere(days);
            if n > 0 {
                notes.lock().unwrap_or_else(|e| e.into_inner()).push(format!("Emptied {n} {} from the trash, older than {days} days", if n == 1 { "item" } else { "items" }));
                crate::sidebar::invalidate();
                ctx.request_repaint();
            }
        });
    }

    fn pump_net(&mut self) {
        let mut i = 0;
        while i < self.net_jobs.len() {
            let Ok(r) = self.net_jobs[i].rx.try_recv() else {
                i += 1;
                continue;
            };
            let job = self.net_jobs.remove(i);
            crate::sidebar::invalidate();
            match r {
                Ok(path) => {
                    self.ud.servers.retain(|s| *s != job.uri);
                    if !job.uri.starts_with("mtp://") && !job.uri.starts_with("gphoto2://") {
                        self.ud.servers.insert(0, job.uri.clone());
                        self.ud.servers.truncate(10);
                        self.ud_dirty = true;
                    }
                    match job.open_in {
                        Some((panel, true)) => self.actions.push(Action::NewTab { panel, path, select: None, focus: true }),
                        Some((panel, false)) => self.navigate(panel, path),
                        None => self.toast(format!("Connected to {}", crate::network::label(&job.uri))),
                    }
                    if job.from_dialog {
                        self.dialog_result = Some((job.uri, Ok(())));
                    }
                }
                Err(e) if job.from_dialog => self.dialog_result = Some((job.uri, Err(e))),
                // The server wants a login: ask for it.
                Err(e) if e.to_lowercase().contains("password") || e.to_lowercase().contains("denied") => self.popup = Popup::connect(job.uri, Some(e)),
                Err(e) => self.toast(format!("Couldn't connect to {}: {e}", crate::network::label(&job.uri))),
            }
        }
    }

    /// Later launches: their folders open as tabs here (files in their folder, selected),
    /// and the window comes forward.
    fn pump_requests(&mut self, ctx: &egui::Context) {
        let reqs: Vec<crate::single::Request> = match &self.requests {
            Some(rx) => rx.try_iter().collect(),
            None => return,
        };
        for r in reqs {
            use crate::single::Request;
            match r {
                Request::Show(paths) => {
                    // Each folder once, in a tab, on the first thing asked for in it.
                    let panel = self.active;
                    let mut by_dir: Vec<(PathBuf, Vec<PathBuf>)> = vec![];
                    for p in paths.into_iter().filter(|p| std::fs::symlink_metadata(p).is_ok()) {
                        let Some(d) = p.parent().map(Path::to_path_buf) else { continue };
                        match by_dir.iter_mut().find(|(x, _)| *x == d) {
                            Some((_, v)) => v.push(p),
                            None => by_dir.push((d, vec![p])),
                        }
                    }
                    if by_dir.is_empty() {
                        self.toast("Couldn't show that: it doesn't exist");
                    }
                    for (dir, items) in by_dir {
                        self.actions.push(Action::NewTab { panel, path: dir, select: items.first().cloned(), focus: true });
                    }
                }
                Request::Properties(paths) => {
                    let paths: Vec<PathBuf> = paths.into_iter().filter(|p| p.exists()).collect();
                    if !paths.is_empty() {
                        self.popup = Popup::properties(paths);
                    }
                }
                Request::Focus => {}
                Request::Open(paths) => {
                let panel = self.active;
                for p in paths {
                    if !p.exists() {
                        self.toast(format!("Couldn't open {}: it doesn't exist", p.display()));
                    } else if p.is_dir() {
                        self.actions.push(Action::NewTab { panel, path: p, select: None, focus: true });
                    } else if let Some(parent) = p.parent().filter(|d| d.is_dir()) {
                        self.actions.push(Action::NewTab { panel, path: parent.to_path_buf(), select: Some(p.clone()), focus: true });
                    } else {
                        self.toast(format!("Couldn't open {}", p.display()));
                    }
                }
                }
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    /// Mount, unmount, eject or lock the drive with this device node, in the background;
    /// a locked one asks for its passphrase first. `open_in`: once mounted, show it in
    /// this panel (in a new tab when true).
    pub fn drive_op(&mut self, op: crate::drives::Op, dev: &str, open_in: Option<(u64, bool)>) {
        use crate::drives::Op;
        let all = crate::sidebar::drives();
        let Some(d) = all.iter().find(|d| d.dev == dev).cloned() else {
            self.toast("That drive is no longer connected");
            return;
        };
        if matches!(op, Op::Mount | Op::Unlock) && d.crypt == crate::drives::Crypt::Locked {
            self.popup = Popup::Unlock(crate::popups::UnlockState::new(d, open_in));
            return;
        }
        if op != Op::Mount {
            // Tabs showing the drive move to Computer first, so nothing holds it busy.
            let gone: Vec<PathBuf> = if op == Op::Eject { all.iter().filter(|x| x.disk == d.disk).flat_map(|x| x.mounts.clone()).collect() } else { d.mounts.clone() };
            let s = self.view_for(Path::new(tab::COMPUTER));
            for p in self.panels.values_mut() {
                for t in &mut p.tabs {
                    if gone.iter().any(|m| t.path.starts_with(m)) {
                        t.navigate(PathBuf::from(tab::COMPUTER), Some(s.clone()));
                    }
                }
                if let Some(i) = p.inspector.as_mut() {
                    i.invalidate();
                }
            }
        }
        match crate::drives::Job::start(op, d, all, open_in, &self.ctx) {
            Ok(j) => self.drive_jobs.push(j),
            Err(e) => self.toast(e),
        }
    }

    /// Erase the drive `d` is on and format it: tabs on any of its partitions move to
    /// Computer first, so nothing holds it busy.
    pub fn format_drive(&mut self, d: crate::drives::Drive, fs: crate::drives::Fs, label: String) {
        let all = crate::sidebar::drives();
        let gone: Vec<PathBuf> = all.iter().filter(|x| x.disk == d.disk).flat_map(|x| x.mounts.clone()).collect();
        let s = self.view_for(Path::new(tab::COMPUTER));
        for p in self.panels.values_mut() {
            for t in &mut p.tabs {
                if gone.iter().any(|m| t.path.starts_with(m)) {
                    t.navigate(PathBuf::from(tab::COMPUTER), Some(s.clone()));
                }
            }
            if let Some(i) = p.inspector.as_mut() {
                i.invalidate();
            }
        }
        match crate::drives::Job::format(d, fs, label, &self.ctx) {
            Ok(j) => self.drive_jobs.push(j),
            Err(e) => self.toast(e),
        }
    }

    /// The drive mounted at `path` (for sidebar and Computer view rows).
    pub fn drive_at(&self, path: &Path) -> Option<crate::drives::Drive> {
        if let Some(dev) = tab::drive_dev(path) {
            return crate::sidebar::drives().into_iter().find(|d| d.dev == dev);
        }
        crate::drives::at_mount(&crate::sidebar::drives(), path).cloned()
    }

    /// Finished drive operations, and Computer views reloaded when drives change.
    fn pump_drives(&mut self, ctx: &egui::Context) {
        use crate::drives::Op;
        // Drives are only polled while something shows them (or an operation runs).
        let shown = self.sidebar || !self.drive_jobs.is_empty() || self.panels.values().any(|p| p.tabs.iter().any(|t| tab::is_computer(&t.path)));
        if shown {
            crate::sidebar::refresh(self, ctx);
        }
        let g = crate::sidebar::drives_generation();
        if g != self.drives_seen {
            self.drives_seen = g;
            for p in self.panels.values_mut() {
                for t in &mut p.tabs {
                    if tab::is_computer(&t.path) {
                        t.reload();
                    }
                }
            }
        }
        let mut i = 0;
        while i < self.drive_jobs.len() {
            let Some(r) = self.drive_jobs[i].poll() else {
                i += 1;
                continue;
            };
            let job = self.drive_jobs.remove(i);
            crate::sidebar::invalidate();
            let name = job.drive.title();
            // The passphrase dialog waits on its unlock: it closes, or says what went wrong.
            if let Popup::Unlock(st) = &mut self.popup {
                if st.drive.dev == job.drive.dev && job.op == Op::Unlock {
                    match &r {
                        Ok(_) => self.popup = Popup::None,
                        Err(e) => {
                            st.error = Some(e.clone());
                            continue;
                        }
                    }
                }
            }
            match (job.op, r) {
                (Op::Mount | Op::Unlock, Ok(Some(at))) if job.open_in.is_some() => {
                    let (panel, new_tab) = job.open_in.unwrap_or_default();
                    if new_tab {
                        self.actions.push(Action::NewTab { panel, path: at, select: None, focus: false });
                    } else {
                        self.navigate(panel, at);
                    }
                }
                (Op::Mount, Ok(_)) => self.toast(format!("Mounted {name}")),
                (Op::Unlock, Ok(_)) => self.toast(format!("Unlocked {name}")),
                (Op::Unmount, Ok(_)) if matches!(job.drive.crypt, crate::drives::Crypt::Open(_)) => self.toast(format!("Unmounted and locked {name}")),
                (Op::Unmount, Ok(_)) => self.toast(format!("Unmounted {name}")),
                (Op::Lock, Ok(_)) => self.toast(format!("Locked {name}")),
                (Op::Eject, Ok(_)) => self.toast(format!("{name} can be safely removed")),
                (Op::Format, Ok(_)) => self.toast(format!("Formatted {name}")),
                (op, Err(e)) => {
                    let msg = format!("Couldn't {} {name}: {e}", op.verb());
                    // Something has the drive open: say what, unless a dialog is up. The
                    // list says why it opened; a toast as well would say it twice.
                    if crate::holders::says_busy(&e) && !job.drive.mounts.is_empty() && matches!(self.popup, Popup::None) {
                        self.popup = crate::holders::open(&self.ctx, crate::holders::What::Paths(job.drive.mounts.clone()), Some(msg));
                    } else {
                        self.toast(msg);
                    }
                }
            }
        }
    }

    // ---------------------------------------------------------------- layouts

    pub fn layout_snap(&self, ctx: &egui::Context) -> LayoutSnap {
        let mut leaves = vec![];
        self.tree.leaves(&mut leaves);
        let index_of = |id: u64| leaves.iter().position(|&l| l == id).unwrap_or(0);
        let (size, maximized) = ctx.input(|i| (i.viewport().inner_rect.map(|r| [r.width(), r.height()]), i.viewport().maximized.unwrap_or(false)));
        LayoutSnap {
            tree: self.tree.snap(&index_of),
            panels: leaves.iter().map(|id| self.panels[id].snap()).collect(),
            active: index_of(self.active),
            window_size: size,
            maximized,
        }
    }

    /// Remember the tabs and panels for the project the active tab is in, to open it as
    /// it was left. The window's size is the window's, not the project's.
    pub fn keep_workspace(&mut self) {
        let Some(root) = crate::project::git_root(&self.panel().path()) else { return };
        let mut snap = self.layout_snap(&self.ctx.clone());
        snap.window_size = None;
        snap.maximized = false;
        self.ud.workspaces.insert(root, snap);
        self.ud_dirty = true;
    }

    /// Open the project at `root` as it was left; false when nothing was kept for it.
    pub fn open_workspace(&mut self, root: &Path) -> bool {
        let Some(snap) = self.ud.workspaces.get(root).cloned() else { return false };
        // Only one that still holds a folder of the project: a moved project starts fresh.
        if !snap.panels.iter().flat_map(|p| p.tabs.iter()).any(|t| t.path.starts_with(root) && t.path.exists()) {
            return false;
        }
        self.keep_workspace();
        self.apply_layout(&snap)
    }

    pub fn apply_layout(&mut self, l: &LayoutSnap) -> bool {
        if l.panels.is_empty() {
            return false;
        }
        let mut new_panels = HashMap::new();
        let mut ids = vec![];
        for ps in &l.panels {
            let mut tabs = vec![];
            for ts in &ps.tabs {
                // A file path opens its folder with the file selected.
                let (path, select) = if tab::is_place(&ts.path) || ts.path.is_dir() {
                    (ts.path.clone(), None)
                } else if ts.path.is_file() {
                    (ts.path.parent().map(Path::to_path_buf).unwrap_or_else(fsx::home), Some(ts.path.clone()))
                } else {
                    (fsx::home(), None)
                };
                let id = self.alloc_id();
                let mut t = Tab::new(id, path.clone(), ts.view.clone(), self.ctx.clone());
                if let Some(f) = select {
                    t.pending_restore = Some(tab::HistState { path: path.clone(), cursor: Some(f.clone()), selected: vec![f], scroll: 0.0 });
                    t.scroll_to_cursor = true;
                }
                t.saved_scope = self.ud.views.get(&path).map(|v| v.scope);
                match ts.smart.and_then(|id| crate::smart::by_id_in(&self.ud, id)) {
                    Some(sf) if sf.root == path => t.open_smart(sf, None),
                    _ if ts.flatten => t.set_flatten(true),
                    _ => {}
                }
                t.pinned = ts.pinned;
                tabs.push(t);
            }
            if tabs.is_empty() {
                let id = self.alloc_id();
                tabs.push(Tab::new(id, fsx::home(), self.view_for(&fsx::home()).0, self.ctx.clone()));
            }
            let pid = self.alloc_id();
            let mut p = Panel::new(pid, tabs.remove(0));
            p.tabs.extend(tabs);
            p.active = ps.active.min(p.tabs.len() - 1);
            if ps.inspector {
                p.inspector = Some(Inspector::default());
            }
            ids.push(pid);
            new_panels.insert(pid, p);
        }
        let Some(tree) = Node::from_snap(&l.tree, &|i| ids.get(i).copied()) else { return false };
        // Drop panels that didn't make it into the tree.
        let mut leaves = vec![];
        tree.leaves(&mut leaves);
        new_panels.retain(|id, _| leaves.contains(id));
        self.panels = new_panels;
        self.tree = tree;
        self.active = ids.get(l.active).copied().filter(|id| self.panels.contains_key(id)).unwrap_or_else(|| self.tree.first_leaf());
        self.prev_active = self.active;
        if let Some([w, h]) = l.window_size {
            if !self.fresh || !l.maximized {
                self.ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w, h)));
            }
        }
        if l.maximized {
            self.ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
        }
        true
    }

    // ---------------------------------------------------------------- persistence

    pub fn save_all(&mut self, ctx: &egui::Context) {
        if self.cfg_dirty {
            let _ = config::save_json(&config::config_path(), &self.cfg);
            self.cfg_dirty = false;
        }
        if self.ud_dirty {
            self.save_userdata();
        }
        if self.save_session {
            let s = Session { layout: Some(self.layout_snap(ctx)), sidebar: self.sidebar, sidebar_width: self.sidebar_width, terminal_height: self.terminal.height };
            let _ = config::save_json(&config::session_path(), &s);
        }
        self.last_save = Instant::now();
    }

    pub fn save_userdata(&mut self) {
        // Merge-safe: reload others' bookmarks if the file changed under us is handled by hot reload.
        if self.cfg.recents_store == config::RecentsStore::Persistent {
            let _ = config::save_json(&config::userdata_path(), &self.ud);
        } else {
            // Session-only (or off): never write the recents list to disk.
            let mut ud = self.ud.clone();
            ud.recents.clear();
            ud.visits.clear();
            let _ = config::save_json(&config::userdata_path(), &ud);
        }
        self.ud_mtime = config::mtime(&config::userdata_path());
        self.ud_dirty = false;
    }

    fn hot_reload(&mut self) {
        let m = config::mtime(&config::userdata_path());
        if m.is_some() && m != self.ud_mtime && !self.ud_dirty {
            self.ud = config::load_userdata();
            self.ud_mtime = m;
            self.refresh_recents();
        }
    }

    /// Match the desktop's light or dark: back to the theme last left, when it fits, else
    /// Midnight or Paper.
    pub fn follow_system(&mut self, dark: bool) {
        if self.cfg.theme.is_light() != dark {
            return;
        }
        let back = self.other_theme.filter(|t| t.is_light() != dark);
        self.other_theme = Some(self.cfg.theme);
        self.cfg.theme = back.unwrap_or(if dark { theme::ThemeName::Midnight } else { theme::ThemeName::Paper });
        self.set_config_changed();
    }

    /// The desktop's preference: the portal's, else what the window system says.
    pub fn system_dark(&self, ctx: &egui::Context) -> Option<bool> {
        match self.appearance.load(std::sync::atomic::Ordering::Relaxed) {
            crate::appearance::DARK => Some(true),
            crate::appearance::LIGHT => Some(false),
            _ => ctx.system_theme().map(|t| t == egui::Theme::Dark),
        }
    }

    pub fn set_config_changed(&mut self) {
        self.cfg_dirty = true;
        self.keymap = Keymap::build(&self.cfg.hotkeys);
        for p in self.panels.values_mut() {
            for t in &mut p.tabs {
                t.dirty = true;
            }
        }
    }

    // ---------------------------------------------------------------- frame

    /// Restore layout invariants so drawing and commands can index freely: every layout
    /// leaf has a panel, every panel has a tab and an in-range active index, and the
    /// active panel exists. Cheap (a handful of panels); runs every frame and after each action.
    fn heal(&mut self) {
        // However a tab got among them (opened beside a pinned one, dragged), pinned
        // tabs stay together at the left.
        for p in self.panels.values_mut() {
            p.settle_pins();
        }
        let mut leaves = vec![];
        self.tree.leaves(&mut leaves);
        for id in &leaves {
            if !self.panels.contains_key(id) {
                let t = self.make_tab(fsx::home());
                self.panels.insert(*id, Panel::new(*id, t));
            }
        }
        let empty: Vec<u64> = self.panels.iter().filter(|(_, p)| p.tabs.is_empty()).map(|(id, _)| *id).collect();
        for id in empty {
            let t = self.make_tab(fsx::home());
            if let Some(p) = self.panels.get_mut(&id) {
                p.tabs.push(t);
            }
        }
        for p in self.panels.values_mut() {
            p.active = p.active.min(p.tabs.len().saturating_sub(1));
        }
        if !self.panels.contains_key(&self.active) {
            if let Some(&first) = leaves.iter().find(|id| self.panels.contains_key(id)) {
                self.active = first;
            }
        }
    }

    pub fn frame(&mut self, ctx: &egui::Context) {
        let ctx = ctx.clone();
        // egui may run a frame twice (a discarded layout pass): test registries start fresh.
        #[cfg(test)]
        {
            crate::ui::CONTROLS.with(|c| c.borrow_mut().clear());
            crate::ui::POPUPS.with(|c| c.borrow_mut().clear());
        }
        self.heal();
        let t = (self.preview_theme.unwrap_or(self.cfg.theme), self.cfg.font_size, self.cfg.spacing, self.cfg.animations, self.cfg.syntax_theme);
        self.preview_theme = None;
        crate::icons::set_enabled(self.cfg.system_icons);
        // One wheel notch scrolls three rows (eased out by egui over ~100 ms).
        let notch = 3.0 * crate::listing::row_height(&self.cfg);
        if ctx.options(|o| o.input_options.line_scroll_speed) != notch {
            ctx.options_mut(|o| o.input_options.line_scroll_speed = notch);
        }
        let want_fonts = format!("{}\u{1f}{}", self.cfg.font_family, self.cfg.inspector_font);
        if want_fonts != self.font_applied {
            self.font_applied = want_fonts;
            theme::install_fonts(&ctx, &self.cfg.font_family, &self.cfg.inspector_font);
        }
        self.thumbs.budget_bytes = (self.cfg.asset_cache_mb.max(32) as usize) << 20;
        // Only the theme changed: it washes in over a copy of the old window, which the
        // runner makes after this frame (still drawn in the old one).
        let only_theme = t.0 != self.theme_applied.0 && (t.1, t.2, t.3, t.4) == (self.theme_applied.1, self.theme_applied.2, self.theme_applied.3, self.theme_applied.4);
        if t != self.theme_applied && (!only_theme || self.wash.theme_changing(&ctx, self.cfg.animations)) {
            self.pal = theme::palette(t.0);
            if !t.4 {
                self.pal.syntax = theme::Syntax::classic(self.pal.dark);
            }
            theme::apply(&ctx, &self.pal, self.cfg.font_size, self.cfg.spacing, self.cfg.animations);
            self.theme_applied = t;
        }
        self.pump(&ctx);
        crate::hints::input(self, &ctx);
        crate::picker::keys(self, &ctx);
        self.handle_input(&ctx);
        self.draw(&ctx);
        self.wash.paint(&ctx);
        self.process_actions(&ctx);
        self.ext_drop = None;
        // Tests share one home and run side by side: saving on its own, one test's Show
        // Hidden reached the tests started after it. Those that check saving ask for it.
        if !cfg!(test) && self.last_save.elapsed() > Duration::from_secs(8) {
            self.hot_reload();
            self.save_all(&ctx);
        }
        self.fresh = false;
    }

    /// Poll background work: listings, walks, stats, thumbnails, jobs, watcher.
    fn pump(&mut self, ctx: &egui::Context) {
        crate::picker::sync(self);
        self.pump_drives(ctx);
        self.pump_requests(ctx);
        self.pump_net();
        self.pump_shares(ctx);
        self.pump_clip_files();
        self.age_trash();
        crate::errands::pump(self);
        crate::tags::pump(self);
        self.shelf.pump(ctx);
        let look = self.appearance.load(std::sync::atomic::Ordering::Relaxed);
        if look != self.appearance_seen {
            self.appearance_seen = look;
            if self.cfg.follow_system_theme && look != crate::appearance::UNKNOWN {
                self.follow_system(look == crate::appearance::DARK);
            }
        }
        let notes = std::mem::take(&mut *self.notes.lock().unwrap_or_else(|e| e.into_inner()));
        if let Some(n) = notes.into_iter().last() {
            self.toast(n);
        }
        self.pump_passwords();
        // Search by meaning runs only while switched on, for the folders now chosen.
        let want = self.cfg.semantic_search.then(|| format!("{}\u{1f}{}", self.cfg.semantic_folders, self.cfg.semantic_pictures));
        if want != self.semantic_for {
            let (folders, pictures) = (crate::semantic::folders(&self.cfg.semantic_folders), self.cfg.semantic_pictures);
            self.semantic = want.as_ref().map(|_| crate::semantic::Engine::start(folders, pictures, ctx));
            self.semantic_for = want;
        }
        self.avatars.set_on(self.cfg.github_pictures);
        self.avatars.pump(ctx);
        let thumbs_landed = self.thumbs.pump(ctx);
        let stats_changed = self.stats.pump();
        let changed_dirs = self.watch.poll();
        let changed_files = std::mem::take(&mut self.watch.changed_files);
        for f in &changed_files {
            self.thumbs.invalidate(f);
            crate::tags::invalidate(f);
            crate::origin::invalidate(f);
            crate::facts::invalidate(f);
            crate::colours::invalidate(f);
            if let Some(d) = f.parent() {
                crate::peek::invalidate(d);
            }
        }
        // Dates taken, sizes and lengths: asked for by last frame's rows, read only for
        // the folders shown; a list sorted or grouped by them looks again as they land.
        let mut shown: Vec<PathBuf> = self.panels.values().map(|p| p.tab().path.clone()).collect();
        shown.sort();
        crate::colours::focus(&shown);
        crate::facts::focus(shown);
        crate::facts::flush(ctx);
        // Colours to sort pictures by, likewise.
        crate::colours::flush(ctx);
        if crate::colours::changed() {
            for t in self.panels.values_mut().flat_map(|p| p.tabs.iter_mut()) {
                if t.settings.sort == SortKey::Colour {
                    t.stream_dirty = true;
                }
            }
        }
        if crate::facts::changed() {
            for t in self.panels.values_mut().flat_map(|p| p.tabs.iter_mut()) {
                if matches!(t.settings.sort, SortKey::Taken | SortKey::Dimensions | SortKey::Duration) || t.settings.group == crate::config::GroupBy::Taken {
                    t.stream_dirty = true;
                }
            }
        }
        // Origins likewise: a list grouped by them looks again as they land.
        crate::origin::flush(ctx);
        let og = crate::origin::generation();
        if og != self.origin_gen {
            self.origin_gen = og;
            for t in self.panels.values_mut().flat_map(|p| p.tabs.iter_mut()) {
                if t.settings.group == crate::config::GroupBy::Origin {
                    t.stream_dirty = true;
                }
            }
        }
        // Tags asked for by last frame's rows go to the reader; when some land, a tag:
        // filter looks again.
        crate::tags::flush(ctx);
        let tg = crate::tags::generation();
        if tg != self.tags_gen {
            self.tags_gen = tg;
            for t in self.panels.values_mut().flat_map(|p| p.tabs.iter_mut()) {
                if t.filter.contains("tag") || t.settings.sort == SortKey::Tags || t.settings.group == crate::config::GroupBy::Tag {
                    t.stream_dirty = true;
                }
            }
            // Tags met on files join the sidebar's.
            self.ud_dirty |= crate::tags::remember(&mut self.ud.tags);
        }
        for d in &changed_dirs {
            self.stats.invalidate(d);
        }
        let mut want = HashSet::new();
        let mut any_loading = false;
        for p in self.panels.values_mut() {
            let insp_path = p.inspector.as_ref().and_then(|i| i.folder_path.clone());
            for (ti, t) in p.tabs.iter_mut().enumerate() {
                if changed_dirs.iter().any(|d| *d == t.path) && !t.flatten && t.rename.is_none() {
                    t.reload();
                }
                let sub: Vec<PathBuf> = changed_dirs.iter().filter(|d| t.expanded.contains(*d)).cloned().collect();
                if t.rename.is_none() {
                    for d in sub {
                        t.load_children(&d);
                    }
                }
                crate::contentsearch::pump(t, self.cfg.show_hidden);
                if t.poll() {
                    t.dirty = true;
                }
                if t.gone {
                    // A vanished folder sends the tab to its closest
                    // existing parent instead of leaving an error page.
                    t.gone = false;
                    let mut up = t.path.parent().map(Path::to_path_buf);
                    while let Some(p) = up.clone() {
                        if p.is_dir() {
                            break;
                        }
                        up = p.parent().map(Path::to_path_buf);
                    }
                    let target = up.unwrap_or_else(fsx::home);
                    let from = t.path.clone();
                    t.navigate(target, None);
                    t.pending_restore = None;
                    let _ = from;
                }
                if stats_changed && matches!(t.settings.sort, SortKey::Size | SortKey::Folders | SortKey::Files | SortKey::Usage) {
                    t.dirty = true;
                }
                if t.is_loading() {
                    any_loading = true;
                }
                if ti == p.active {
                    crate::columns::watch(t, &changed_dirs, &mut want);
                }
                if ti == p.active && !tab::is_place(&t.path) && !t.flatten && t.archive.is_none() {
                    want.insert(t.path.clone());
                    if t.tree_active() {
                        want.extend(t.expanded.iter().filter(|d| d.starts_with(&t.path)).take(64).cloned());
                    }
                }
            }
            if let Some(ip) = insp_path {
                want.insert(ip);
            }
        }
        // Changes started from the branch and commit menus: said once done, and read again.
        for (root, title, op, r) in crate::gitwork::finished() {
            crate::gitstatus::rescan(&root);
            if op.moved(&r) {
                for h in self.panels.values_mut().filter_map(|p| p.inspector.as_mut()?.git.as_mut()).filter(|h| h.root == root) {
                    h.reload();
                }
            }
            use crate::gitsync::{BEHIND, CONFLICTS, DIVERGED};
            use crate::gitwork::{Op, UNMERGED};
            let go = |root: &PathBuf, op: Op, title: &str| -> Option<Box<dyn FnOnce(&mut App)>> {
                let (root, title) = (root.clone(), title.to_string());
                Some(Box::new(move |app: &mut App| crate::gitwork::start(&app.ctx, &root, op, title)))
            };
            match r {
                Err(e) if e == DIVERGED => {
                    let choices = vec![("Merge".to_string(), false, go(&root, Op::Pull(Some(false)), "Couldn't merge")), ("Rebase".to_string(), false, go(&root, Op::Pull(Some(true)), "Couldn't rebase"))];
                    self.popup = Popup::Choose { msg: "This branch and its remote have both moved on".into(), sub: "Merge them into one commit, or put your commits on top of the remote's (rebase).".into(), choices };
                }
                Err(e) if e == BEHIND => {
                    let choices = vec![("Pull first".to_string(), false, go(&root, Op::Pull(None), "Couldn't pull")), ("Replace the remote's".to_string(), true, go(&root, Op::Push(true), "Couldn't push"))];
                    self.popup = Popup::Choose { msg: "The remote has commits this branch doesn't".into(), sub: "Pull them first, or replace the remote branch with yours. Replacing drops its commits for everyone.".into(), choices };
                }
                Err(e) if e == UNMERGED => {
                    let Op::DeleteBranch { name, .. } = op else { continue };
                    let title = format!("Couldn't delete {name}");
                    let choices = vec![("Delete anyway".to_string(), true, go(&root, Op::DeleteBranch { name: name.clone(), force: true }, &title))];
                    self.popup = Popup::Choose { msg: format!("{name} has commits no other branch has"), sub: "Deleting it anyway leaves them findable only by their hash, until git cleans up.".into(), choices };
                }
                Err(e) if e == CONFLICTS => {
                    self.toast("Stopped at conflicts: settle them on the Changes page");
                    if crate::gitstatus::repo_for(&self.tab().path).is_some_and(|r| r.root == root) {
                        self.actions.push(Action::Cmd(Cmd::GitChanges));
                    }
                }
                Ok(msg) => self.toast(msg),
                Err(e) if matches!(self.popup, Popup::None) => self.popup = Popup::notice(title, e),
                Err(_) => self.toast(title),
            }
        }
        crate::gitstatus::pump(&self.panels, &changed_dirs, &mut want, self.cfg.git_ignored != crate::config::IgnoredFiles::Show, ctx);
        self.watch.sync(&want);
        let recents = &self.recents_set;
        let stats = &self.stats.map;
        let ctxv = ViewCtx {
            show_hidden: self.cfg.show_hidden,
            show_system: self.cfg.show_system,
            folders_first: self.cfg.folders_first,
            recents_on_top: self.cfg.recents_on_top,
            recents,
            pinned: &self.pinned_set,
            stats,
        };
        let mut new_renames = vec![];
        let hide = self.cfg.git_ignored == crate::config::IgnoredFiles::Hide;
        for p in self.panels.values_mut() {
            for t in &mut p.tabs {
                // What git ignores leaves the view when it is to be hidden, and comes back
                // when it is not (or once the next status says otherwise).
                let ig = hide.then(|| crate::gitstatus::repo_for(&t.path)).flatten().filter(|r| !r.ignored.is_empty() && !crate::gitstatus::inside_ignored(r, &t.path)).map(|r| r.ignored.clone());
                if ig.as_ref().map(std::sync::Arc::as_ptr) != t.git_hide.as_ref().map(std::sync::Arc::as_ptr) {
                    t.git_hide = ig;
                    t.dirty = true;
                }
                // Only the changed files: each new status answer narrows the list again.
                let only = t.only_changed.then(|| crate::gitstatus::repo_for(&t.path)).flatten();
                if only.as_ref().map(std::rc::Rc::as_ptr) != t.git_only_from.as_ref().map(std::rc::Rc::as_ptr) {
                    t.git_only = only.as_ref().map(|r| std::sync::Arc::new(r.marks.keys().cloned().collect()));
                    t.git_only_from = only;
                    t.dirty = true;
                }
                if t.dirty || t.built_tree != t.tree_active() {
                    t.rebuild(&ctxv);
                } else if t.stream_dirty {
                    match t.rebuild_due() {
                        None => t.rebuild(&ctxv),
                        Some(wait) => self.ctx.request_repaint_after(wait),
                    }
                }
                if let Some(target) = t.pending_rename.clone() {
                    if !t.is_loading() {
                        if let Some(i) = t.entries.iter().position(|e| e.path == target) {
                            t.pending_rename = None;
                            new_renames.push((p.id, t.id, i as u32));
                        } else if t.loaded_at.map(|l| l.elapsed() > Duration::from_secs(2)).unwrap_or(false) {
                            t.pending_rename = None;
                        }
                    }
                }
            }
        }
        for (pid, tid, idx) in new_renames {
            if let Some(t) = self.panels.get_mut(&pid).and_then(|p| p.tabs.iter_mut().find(|t| t.id == tid)) {
                let e = &t.entries[idx as usize];
                t.rename = Some(Rename::new(vec![(idx, e.path.clone(), e.name.clone())], true));
                t.select_only(idx);
                t.scroll_to_cursor = true;
            }
        }
        // Folder statistics for visible folders of active tabs.
        let mut req = vec![];
        for p in self.panels.values() {
            let t = p.tab();
            if t.flatten || tab::is_place(&t.path) || t.archive.is_some() {
                continue;
            }
            for &i in t.view.iter().take(2000) {
                let e = &t.entries[i as usize];
                if e.is_dir && !e.is_link {
                    req.push(e.path.clone());
                }
            }
            req.push(t.path.clone());
        }
        if let Some(first) = req.first().cloned() {
            if self.stats.allowed(&self.cfg, &first, &ctx) {
                for r in req {
                    self.stats.request(&r);
                }
            }
        }
        // Jobs: mark finished, drop after a moment.
        let mut changes: Vec<ops::Changed> = vec![];
        let mut pw_job = None;
        for j in &mut self.jobs {
            if j.finished_at.is_none() && j.progress.lock().unwrap().finished {
                j.finished_at = Some(Instant::now());
                let p = j.progress.lock().unwrap();
                changes.push(ops::Changed::of(&j.kind, &p));
                if let Some((archive, wrong)) = &p.password {
                    pw_job = Some((archive.clone(), *wrong, j.kind.clone(), j.id));
                }
                if let OpKind::Move { .. } = j.kind {
                    if !p.produced.is_empty() {
                        let kind = if p.replaced.is_empty() { UndoKind::Move(p.produced.clone()) } else { UndoKind::MovedOver(p.produced.clone(), p.replaced.clone()) };
                        self.undo.push(UndoEntry { label: crate::ui::job_undo_label(&j.label), kind });
                    }
                }
                if let OpKind::Sort { .. } = j.kind {
                    if !p.produced.is_empty() || !p.made.is_empty() {
                        let label = crate::ui::job_undo_label(&j.label);
                        self.undo.push(UndoEntry { label, kind: UndoKind::Sorted(p.produced.clone(), p.made.clone()) });
                    }
                }
                if let OpKind::Copy { .. } | OpKind::Extract { .. } | OpKind::Compress { .. } | OpKind::Convert { .. } = j.kind {
                    if !p.produced.is_empty() {
                        let label = match &j.kind {
                            OpKind::Convert { srcs, to, .. } => crate::convert::undo_label(srcs, *to),
                            _ => crate::ui::job_undo_label(&j.label),
                        };
                        let made: Vec<PathBuf> = p.produced.iter().map(|x| x.1.clone()).collect();
                        let kind = if p.replaced.is_empty() { UndoKind::Created(made) } else { UndoKind::Copied(made, p.replaced.clone()) };
                        self.undo.push(UndoEntry { label, kind });
                    }
                }
                if let OpKind::Sync { .. } = j.kind {
                    if !p.produced.is_empty() || !p.gone.is_empty() {
                        let made = p.produced.iter().map(|x| x.1.clone()).collect();
                        self.undo.push(UndoEntry { label: crate::ui::job_undo_label(&j.label), kind: UndoKind::Synced(made, p.replaced.clone(), p.gone.clone()) });
                    }
                }
                // Changed in place: the old one is in the trash, for Undo to swap back.
                if let OpKind::ArchiveEdit { archive, .. } = &j.kind {
                    if !p.replaced.is_empty() {
                        self.undo.push(UndoEntry { label: crate::ui::job_undo_label(&j.label), kind: UndoKind::Replaced(vec![archive.clone()]) });
                    }
                }
                if let OpKind::Trash { paths } = &j.kind {
                    self.undo.push(UndoEntry { label: crate::ui::job_undo_label(&j.label), kind: UndoKind::Trashed(paths.clone()) });
                }
                if let Some(step) = self.job_steps.remove(&j.id) {
                    crate::redo::placed(&mut self.undo, step, &p);
                }
            }
        }
        // An extraction refused for want of a password: ask, and run it again.
        if let Some((archive, wrong, kind, id)) = pw_job.filter(|_| matches!(self.popup, Popup::None)) {
            self.jobs.retain(|j| j.id != id);
            self.popup = Popup::ArchivePassword(crate::popups::PasswordState { archive, retry: Some(crate::popups::Retry::Job(kind)), text: String::new(), wrong });
        }
        for c in &changes {
            self.show_changed(c);
        }
        self.jobs.retain(|j| match j.finished_at {
            Some(t) => {
                let errs = !j.progress.lock().unwrap().errors.is_empty();
                (errs && t.elapsed() < Duration::from_secs(30)) || t.elapsed() < Duration::from_millis(1800)
            }
            None => true,
        });
        // Worker threads (folder sizes, listings, icons) free large temporary buffers into
        // their own malloc arenas, which glibc keeps. Two seconds after background work
        // settles, give whole free pages back to the system.
        // Image decodes (thumbnails, previews) count as background work too.
        if stats_changed || any_loading || thumbs_landed {
            self.trim_after = Some(std::time::Instant::now() + std::time::Duration::from_secs(2));
            self.trim_pending_since.get_or_insert_with(std::time::Instant::now);
        } else if self.trim_after.is_some_and(|t| std::time::Instant::now() >= t)
            // Folder sizing that never settles (a dead mount) mustn't postpone this forever.
            && (self.stats.idle() || self.trim_pending_since.is_some_and(|t| t.elapsed() > Duration::from_secs(30)))
        {
            self.trim_pending_since = None;
            self.trim_after = None;
            unsafe {
                libc::malloc_trim(0);
            }
        }
        if let Some(t) = self.trim_after {
            // Past due but waiting for folder sizing: check back slowly, not every frame.
            let wait = t.saturating_duration_since(std::time::Instant::now());
            ctx.request_repaint_after(if wait.is_zero() { Duration::from_millis(500) } else { wait + Duration::from_millis(50) });
        }
        if !self.jobs.is_empty() || any_loading || self.watch.has_pending() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        self.undo_keep.pump(&mut self.undo, &ctx);
        if self.seq.len() > 0 && self.seq_time.elapsed() > Duration::from_millis(1100) {
            self.seq.clear();
        }
        crate::compare::pump(self);
    }

    // ---------------------------------------------------------------- input

    fn handle_input(&mut self, ctx: &egui::Context) {
        let (events, mods) = ctx.input(|i| (i.events.clone(), i.modifiers));
        // Hotkey capture for the palette's "+" button takes everything.
        if self.popup.is_capturing() {
            return;
        }
        let popup_open = !matches!(self.popup, Popup::None);
        // Inline rename owns the keyboard.
        let renaming = self.panels.get(&self.active).map(|p| p.tab().rename.is_some()).unwrap_or(false);
        if renaming && !popup_open {
            let mut clip = None;
            let outcome = {
                let t = self.tab();
                t.rename.as_mut().map(|r| r.handle(&events, &mut clip)).unwrap_or(Outcome::Continue)
            };
            if let Some(c) = clip {
                ctx.copy_text(c);
            }
            let id = self.active;
            match outcome {
                Outcome::Commit => self.actions.push(Action::CommitRename(id)),
                Outcome::Cancel => self.actions.push(Action::CancelRename(id)),
                Outcome::Continue => {}
            }
            ctx.input_mut(|i| {
                i.events.retain(|e| !matches!(e, Event::Key { .. } | Event::Text(_) | Event::Copy | Event::Cut | Event::Paste(_) | Event::Ime(_)))
            });
            return;
        }
        if popup_open {
            // Popups handle their own keys; still allow the palette/goto toggles.
            for ev in &events {
                if let Event::Key { key, pressed: true, modifiers, .. } = ev {
                    let c = Chord::from_key(*key, *modifiers);
                    let (hit, _) = self.keymap.lookup(&[c]);
                    if matches!(hit, Some(Cmd::Hints)) {
                        self.actions.push(Action::Cmd(Cmd::Hints));
                        ctx.input_mut(|i| i.consume_key(*modifiers, *key));
                        continue;
                    }
                    if matches!(hit, Some(Cmd::Palette) | Some(Cmd::GoTo) | Some(Cmd::Options)) {
                        if self.popup.kind_cmd() == hit {
                            self.popup = Popup::None;
                        } else {
                            self.actions.push(Action::Cmd(hit.unwrap()));
                        }
                        ctx.input_mut(|i| i.consume_key(*modifiers, *key));
                    }
                }
            }
            return;
        }
        // The terminal pane, while it has the keyboard, takes every key.
        if self.terminal.focused {
            crate::terminal::keys(self, ctx, &events);
            return;
        }
        let text_focus = ctx.egui_wants_keyboard_input();
        let filter_focus = self.panels.get(&self.active).map(|p| p.filter_has_focus).unwrap_or(false);
        // The preview's editor has the keyboard: its own keys, and only a few of Maia's.
        // (Asked of the editor: egui lets go of the focus on Escape before this runs.)
        let editing = self.panels.get(&self.active).and_then(|p| p.inspector.as_ref()?.edit.as_ref()).is_some_and(|e| e.focused);
        let mut typed: Vec<Event> = vec![];
        let finding = self.panels.get(&self.active).and_then(|p| p.inspector.as_ref()?.find.as_ref()).is_some_and(|f| f.focused);
        let mut consumed: Vec<usize> = vec![];
        let mut plain_fired = false;
        for (n, ev) in events.iter().enumerate() {
            // An open history takes the keys that move through it, open and go back.
            if let Event::Key { key, pressed: true, modifiers, .. } = ev {
                // Ctrl+Enter on the Changes page commits, its fields focused or not.
                if *key == Key::Enter && modifiers.ctrl && !modifiers.alt && !modifiers.shift {
                    if let Some(h) = self.panels.get_mut(&self.active).and_then(|p| p.inspector.as_mut()).and_then(|i| i.git.as_mut()).filter(|h| h.changes.open) {
                        h.changes.commit_now();
                        consumed.push(n);
                        continue;
                    }
                }
                let id = self.active;
                // The GitHub page moves through its timeline; an open preview keeps its keys.
                if !text_focus && !modifiers.ctrl && !modifiers.alt && !modifiers.shift && self.github.takes(*key) {
                    if self.panels.get(&id).is_some_and(|p| tab::is_github(&p.tab().path) && p.inspector.as_ref().is_none_or(|i| i.git.is_none())) {
                        self.github.key(*key, id, &mut self.actions);
                        consumed.push(n);
                        continue;
                    }
                }
                if let Some(p) = self.panels.get_mut(&id).filter(|_| !text_focus && !modifiers.ctrl && !modifiers.alt) {
                    if let Some(h) = p.inspector.as_mut().and_then(|i| i.git.as_mut()).filter(|h| h.takes(*key)) {
                        if !h.key(*key, id, &mut self.actions) {
                            crate::inspector::close_history(p);
                        }
                        consumed.push(n);
                        continue;
                    }
                    // A data tree or a database's tables, clicked: their keys until a
                    // click elsewhere or Escape.
                    if let Some(i) = p.inspector.as_mut().filter(|i| i.git.is_none() && i.takes(*key)) {
                        i.key(*key);
                        consumed.push(n);
                        continue;
                    }
                }
            }
            let chord = match ev {
                Event::Key { key, pressed: true, modifiers, .. } => Chord::from_key(*key, *modifiers),
                Event::Copy => Chord::from_key(Key::C, Modifiers { ctrl: true, shift: mods.shift, ..Default::default() }),
                Event::Cut => Chord::from_key(Key::X, Modifiers { ctrl: true, shift: mods.shift, ..Default::default() }),
                Event::Paste(_) => Chord::from_key(Key::V, Modifiers { ctrl: true, shift: mods.shift, ..Default::default() }),
                Event::PointerButton { button, pressed: true, modifiers, .. } => match Chord::from_mouse(*button, *modifiers) {
                    // Middle clicks are contextual (tabs, items); only thumb buttons are global.
                    Some(c) if c.input != commands::Input::Mouse(3) => c,
                    _ => continue,
                },
                _ => continue,
            };
            // Escape in the find field closes the bar, back to what it was finding in.
            if finding && chord.input == commands::Input::Key(Key::Escape) && !chord.ctrl && !chord.alt && !chord.shift {
                let id = self.active;
                if let Some(i) = self.panels.get_mut(&id).and_then(|p| p.inspector.as_mut()) {
                    i.find = None;
                    i.keys = true;
                    if i.edit.is_some() {
                        ctx.memory_mut(|m| m.request_focus(egui::Id::new(("insp-edit", id))));
                    }
                }
                consumed.push(n);
                continue;
            }
            if editing {
                let id = self.active;
                let ctrl = chord.ctrl && !chord.alt;
                let plain = !chord.ctrl && !chord.alt && !chord.shift;
                let mine = match chord.input {
                    commands::Input::Key(Key::S) if ctrl && !chord.shift => {
                        self.save_edit(id, false);
                        true
                    }
                    commands::Input::Key(Key::F | Key::H) if ctrl => {
                        self.run(Cmd::FindInPreview);
                        true
                    }
                    commands::Input::Key(Key::Escape) if plain => {
                        self.leave_edit(id, false);
                        true
                    }
                    // Tab indents as the file does.
                    commands::Input::Key(Key::Tab) if plain => {
                        let indent = self.panels.get(&id).and_then(|p| p.inspector.as_ref()?.edit.as_ref()).map_or("    ", |e| e.indent);
                        typed.push(Event::Text(indent.into()));
                        true
                    }
                    _ => false,
                };
                if mine {
                    consumed.push(n);
                    continue;
                }
                // Only the palette, Options and Quit get past the editor.
                if !matches!(self.keymap.lookup(&[chord]).0, Some(Cmd::Palette | Cmd::Options | Cmd::Quit)) {
                    continue;
                }
            }
            // Ctrl+C and Ctrl+A in a preview that has the keyboard take its selection.
            if !text_focus && chord.ctrl && !chord.alt && !chord.shift && matches!(chord.input, commands::Input::Key(Key::C | Key::A)) && crate::textsel::key(self, ctx, chord.input == commands::Input::Key(Key::C)) {
                consumed.push(n);
                continue;
            }
            if text_focus {
                // Filter box: arrows/enter drive the list; plain typing stays in the box.
                if filter_focus {
                    if let commands::Input::Key(k) = chord.input {
                        if !chord.ctrl && !chord.alt {
                            match k {
                                Key::ArrowDown | Key::ArrowUp | Key::PageDown | Key::PageUp => {
                                    let page = self.tab().page_items.max(1) as isize;
                                    let d = match k {
                                        Key::ArrowDown => 1,
                                        Key::ArrowUp => -1,
                                        Key::PageDown => page,
                                        _ => -page,
                                    };
                                    self.tab().move_cursor(d, chord.shift, false);
                                    consumed.push(n);
                                    continue;
                                }
                                Key::Enter => {
                                    self.actions.push(Action::Cmd(Cmd::Activate));
                                    consumed.push(n);
                                    continue;
                                }
                                Key::Escape => {
                                    let t = self.tab();
                                    if t.filter.is_empty() {
                                        ctx.memory_mut(|m| m.surrender_focus(m.focused().unwrap_or(egui::Id::NULL)));
                                    } else {
                                        t.filter.clear();
                                        t.dirty = true;
                                    }
                                    consumed.push(n);
                                    continue;
                                }
                                Key::Tab => {}
                                Key::F2 => {}
                                _ => continue,
                            }
                        }
                    }
                }
                let text_edit_key = matches!(chord.input, commands::Input::Key(Key::A | Key::C | Key::X | Key::V | Key::Z | Key::Y | Key::ArrowLeft | Key::ArrowRight | Key::Backspace | Key::Delete | Key::Home | Key::End))
                    && chord.ctrl
                    && !chord.alt;
                if text_edit_key || (!chord.has_command_mod() && !matches!(chord.input, commands::Input::Key(Key::Tab | Key::F2))) {
                    continue;
                }
            }
            if chord.is_plain() && !chord.shift && self.cfg.char_key_action != config::CharKeyAction::TriggerHotkey && !matches!(chord.input, commands::Input::Key(Key::Space)) {
                continue;
            }
            let mut seq = self.seq.clone();
            seq.push(chord);
            let (exact, longer) = self.keymap.lookup(&seq);
            if longer {
                self.seq = seq;
                self.seq_time = Instant::now();
                consumed.push(n);
                if let Some(c) = exact {
                    // Allow the shorter binding to fire if nothing follows (handled on timeout by ignoring).
                    let _ = c;
                }
                continue;
            }
            self.seq.clear();
            if let Some(cmd) = exact {
                if chord.is_plain() {
                    plain_fired = true;
                }
                // Escape-like keys in an empty context shouldn't swallow egui behavior.
                self.actions.push(Action::Cmd(cmd));
                consumed.push(n);
            } else if let commands::Input::Key(Key::Escape) = chord.input {
                // Escape: clear filter, then selection.
                let t = self.tab();
                if !t.filter.is_empty() {
                    t.filter.clear();
                    t.dirty = true;
                } else if t.flatten {
                    t.set_flatten(false);
                } else {
                    t.selected.clear();
                }
                consumed.push(n);
            } else if !text_focus {
                // Listing navigation keys (Page keys go to a paged preview under the pointer).
                if let commands::Input::Key(k) = chord.input {
                    let over_pages = matches!(k, Key::PageUp | Key::PageDown)
                        && self.panels.get(&self.active).is_some_and(|p| {
                            p.inspector.as_ref().is_some_and(|i| i.wants_page_keys()) && p.inspector_rect.zip(ctx.pointer_hover_pos()).is_some_and(|(r, pp)| r.contains(pp))
                        });
                    if over_pages {
                        continue;
                    }
                    if self.nav_key(k, chord) {
                        consumed.push(n);
                    }
                }
            }
        }
        // Plain text typed into the listing → filter (or jump).
        if !text_focus && !plain_fired {
            for (n, ev) in events.iter().enumerate() {
                if let Event::Text(t) = ev {
                    if mods.ctrl || mods.alt || t.trim().is_empty() {
                        continue;
                    }
                    match self.cfg.char_key_action {
                        config::CharKeyAction::Jump => self.jump_to(t),
                        _ => {
                            let id = self.active;
                            let p = self.panel();
                            p.tab_mut().filter.push_str(t);
                            p.tab_mut().dirty = true;
                            p.focus_filter = true;
                            let _ = id;
                        }
                    }
                    consumed.push(n);
                }
            }
        }
        if !consumed.is_empty() {
            ctx.input_mut(|i| {
                let mut k = 0;
                i.events.retain(|_| {
                    let keep = !consumed.contains(&k);
                    k += 1;
                    keep
                });
                i.events.extend(typed);
            });
        }
    }

    fn jump_to(&mut self, t: &str) {
        if self.jump_buf.1.elapsed() > Duration::from_millis(900) {
            self.jump_buf.0.clear();
        }
        self.jump_buf.0.push_str(&t.to_lowercase());
        self.jump_buf.1 = Instant::now();
        let prefix = self.jump_buf.0.clone();
        let tab = self.tab();
        let start = tab.cursor.and_then(|c| tab.view_pos(c)).unwrap_or(0);
        let n = tab.view.len();
        let single = prefix.chars().count() == 1;
        for k in 0..n {
            let pos = (start + k + if single { 1 } else { 0 }) % n.max(1);
            let idx = tab.view[pos];
            if tab.entries[idx as usize].lname().starts_with(&prefix) {
                tab.select_only(idx);
                tab.scroll_to_cursor = true;
                break;
            }
        }
    }

    /// Arrow/Home/End/Page keys in the listing. Returns true if handled.
    fn nav_key(&mut self, k: Key, c: Chord) -> bool {
        let t = self.tab();
        let (row_step, col_step) = (t.nav_row_step.max(1) as isize, t.nav_col_step.max(1) as isize);
        let page = t.page_items.max(1) as isize;
        let cursor_only = c.ctrl;
        let d = match k {
            Key::ArrowDown => row_step,
            Key::ArrowUp => -row_step,
            Key::ArrowRight => col_step,
            Key::ArrowLeft => -col_step,
            Key::PageDown => page,
            Key::PageUp => -page,
            Key::Home => {
                t.move_cursor_to(0, c.shift);
                return true;
            }
            Key::End => {
                let n = t.view.len().saturating_sub(1);
                t.move_cursor_to(n, c.shift);
                return true;
            }
            _ => return false,
        };
        if matches!(k, Key::ArrowLeft | Key::ArrowRight) && t.settings.column_view {
            return crate::columns::step(self, k == Key::ArrowRight);
        }
        if matches!(k, Key::ArrowLeft | Key::ArrowRight) && t.nav_col_step == 0 {
            // Details tree: Right expands / enters, Left collapses / goes to the parent row.
            if !t.tree_active() {
                return false;
            }
            let Some(cur) = t.cursor else { return false };
            let e = t.entries[cur as usize].clone();
            if c.shift {
                // Shift+Right unfolds everything below, Shift+Left folds it all back.
                if e.is_dir && !e.is_link {
                    if k == Key::ArrowRight { t.expand_recursive(&e.path) } else { t.collapse_recursive(&e.path) }
                }
                return true;
            }
            if k == Key::ArrowRight {
                if e.is_dir && !e.is_link {
                    if t.expanded.contains(&e.path) {
                        t.move_cursor(1, false, cursor_only);
                    } else {
                        t.toggle_expand(&e.path);
                    }
                }
            } else if e.is_dir && t.expanded.contains(&e.path) {
                t.toggle_expand(&e.path);
            } else if e.depth > 0 {
                let parent = e.path.parent().map(Path::to_path_buf);
                if let Some(pi) = t.view.iter().copied().find(|&i| Some(&t.entries[i as usize].path) == parent.as_ref()) {
                    if cursor_only { t.cursor = Some(pi) } else { t.select_only(pi) }
                    t.scroll_to_cursor = true;
                }
            }
            return true;
        }
        t.move_cursor(d, c.shift, cursor_only);
        true
    }

    // ---------------------------------------------------------------- commands

    pub fn run(&mut self, cmd: Cmd) {
        // Every command below may index the active panel directly: make sure it exists.
        let _ = self.panel();
        let ctx = self.ctx.clone();
        // Drives aren't files: in the Computer view nothing can be cut, deleted, linked or
        // duplicated (a mounted volume's root would take its whole contents with it).
        if tab::is_place(&self.panel().path()) && matches!(cmd, Cmd::Cut | Cmd::Trash | Cmd::DeletePermanent | Cmd::Duplicate | Cmd::CreateLink | Cmd::Paste | Cmd::PasteEarlier | Cmd::Rename) {
            return;
        }
        // Inside an archive: zip, 7z and tarballs change by a job that rewrites them.
        if self.tab().archive.is_some() && matches!(cmd, Cmd::Cut | Cmd::Trash | Cmd::DeletePermanent | Cmd::Duplicate | Cmd::CreateLink | Cmd::Paste | Cmd::Rename | Cmd::NewFolder | Cmd::NewFile) && crate::archwrite::inside(self, cmd) {
            return;
        }
        match cmd {
            Cmd::GoTo => self.popup = Popup::goto(String::new()),
            Cmd::AddressBar => {
                let p = self.panel().path();
                let s = if tab::is_place(&p) { "/".to_string() } else { format!("{}/", p.display()).replace("//", "/") };
                self.popup = Popup::goto(s);
            }
            Cmd::Back => {
                let ud = &self.ud;
                let f = |p: &Path| App::nav_view(ud, p);
                let id = self.active;
                let old = self.panels[&id].path();
                self.panels.get_mut(&id).unwrap().tab_mut().back(f);
                self.track_leave(&old);
            }
            Cmd::Forward => {
                let ud = &self.ud;
                let f = |p: &Path| App::nav_view(ud, p);
                let id = self.active;
                let old = self.panels[&id].path();
                self.panels.get_mut(&id).unwrap().tab_mut().forward(f);
                self.track_leave(&old);
            }
            Cmd::Up => {
                let ud = &self.ud;
                let f = |p: &Path| App::nav_view(ud, p);
                let id = self.active;
                let old = self.panels[&id].path();
                self.panels.get_mut(&id).unwrap().tab_mut().up(f);
                self.track_leave(&old);
            }
            Cmd::Home => self.navigate(self.active, fsx::home()),
            Cmd::Computer => self.navigate(self.active, PathBuf::from(COMPUTER)),
            Cmd::Refresh if tab::is_github(&self.tab().path) => self.github.refresh(&ctx),
            Cmd::Refresh => {
                let t = self.tab();
                let entries: Vec<PathBuf> = t.entries.iter().map(|e| e.path.clone()).collect();
                let p = t.path.clone();
                t.reload();
                for e in entries {
                    self.thumbs.invalidate(&e);
                }
                self.stats.invalidate(&p);
                if let Some(i) = self.panel().inspector.as_mut() {
                    i.invalidate();
                }
            }
            Cmd::Activate if crate::picker::enter(self) => {}
            Cmd::Activate => {
                let t = self.tab();
                let targets: Vec<u32> = if t.selected.len() > 1 {
                    let mut v: Vec<u32> = t.selected.iter().copied().collect();
                    v.sort_by_key(|i| t.view_pos(*i));
                    v
                } else {
                    t.cursor.or_else(|| t.selected.iter().next().copied()).into_iter().collect()
                };
                let id = self.active;
                if targets.len() == 1 {
                    self.actions.push(Action::Activate { panel: id, idx: targets[0], mods: Modifiers::NONE });
                } else {
                    for i in targets.into_iter().take(30) {
                        self.actions.push(Action::Activate { panel: id, idx: i, mods: Modifiers::CTRL });
                    }
                }
            }
            Cmd::OpenInNewTab => {
                let id = self.active;
                let ents: Vec<(PathBuf, bool)> = self.tab().target_entries().iter().map(|e| (e.path.clone(), e.is_dir)).collect();
                for (p, d) in ents {
                    if d {
                        self.actions.push(Action::NewTab { panel: id, path: p, select: None, focus: false });
                    } else if let Some(parent) = p.parent() {
                        self.actions.push(Action::NewTab { panel: id, path: parent.to_path_buf(), select: Some(p.clone()), focus: false });
                    }
                }
            }
            Cmd::OpenInNewWindow => {
                let p = self.target_folder();
                if let Some(dev) = tab::drive_dev(&p) {
                    let dev = dev.to_string();
                    self.drive_op(crate::drives::Op::Mount, &dev, None);
                    return;
                }
                if let Err(e) = crate::mime::new_window(&p) {
                    self.toast(format!("Couldn't open window: {e}"));
                }
            }
            Cmd::OpenInRightSplit | Cmd::OpenInBottomSplit => {
                let side = if cmd == Cmd::OpenInRightSplit { Side::Right } else { Side::Bottom };
                let id = self.active;
                let ent = self.tab().target_entries().first().map(|e| (e.path.clone(), e.is_dir));
                match ent {
                    Some((p, true)) => self.actions.push(Action::Split { panel: id, path: p, side, select: None }),
                    Some((p, false)) => {
                        let parent = p.parent().map(Path::to_path_buf).unwrap_or_else(fsx::home);
                        self.actions.push(Action::Split { panel: id, path: parent, side, select: Some(p) })
                    }
                    None => {
                        let p = self.panel().path();
                        self.actions.push(Action::Split { panel: id, path: p, side, select: None })
                    }
                }
            }
            Cmd::TerminalPanel => crate::terminal::toggle(self),
            Cmd::NewTerminal => crate::terminal::new_shell(self),
            Cmd::CloseTerminal => crate::terminal::close_shell(self, self.terminal.cur),
            Cmd::NextTerminal => crate::terminal::switch(self, 1),
            Cmd::PrevTerminal => crate::terminal::switch(self, -1),
            Cmd::FindInTerminal => crate::terminal::find(self),
            Cmd::OpenTerminal => {
                let p = self.panel().path();
                let dir = if tab::is_place(&p) { PathBuf::from("/") } else { p };
                if let Err(e) = crate::mime::open_terminal(&dir) {
                    self.toast(format!("Couldn't start a terminal: {e}"));
                }
            }
            Cmd::ToggleBookmark => {
                let sel: Vec<PathBuf> = self.tab().target_entries().iter().filter(|e| e.is_dir).map(|e| e.path.clone()).collect();
                let explicit = !self.tab().selected.is_empty();
                let targets = if explicit && !sel.is_empty() { sel } else { vec![self.panel().path()] };
                for p in targets {
                    if tab::is_place(&p) {
                        continue;
                    }
                    if let Some(i) = self.ud.bookmarks.iter().position(|b| *b == p) {
                        self.ud.bookmarks.remove(i);
                        self.toast(format!("Removed bookmark {}", p.display()));
                    } else {
                        self.ud.bookmarks.push(p.clone());
                        self.toast(format!("Bookmarked {}", p.display()));
                    }
                }
                self.ud_dirty = true;
            }
            Cmd::OpenLinkTarget => {
                let link = self.tab().target_entries().first().filter(|e| e.is_link).map(|e| e.path.clone());
                if let Some(l) = link {
                    if let Ok(target) = std::fs::read_link(&l) {
                        let abs = if target.is_absolute() { target } else { l.parent().unwrap_or(Path::new("/")).join(target) };
                        self.navigate(self.active, abs);
                    }
                }
            }
            Cmd::TabNewOrRecent => {
                let p = self.panel().path();
                if !tab::is_place(&p) {
                    self.popup = Popup::path_dropdown(self.active, p, None);
                }
            }
            Cmd::NewTab => {
                let cur = self.panel().path();
                let path = match &self.cfg.new_tab {
                    NewTabShows::Current => cur,
                    NewTabShows::Home => fsx::home(),
                    NewTabShows::Computer => PathBuf::from(COMPUTER),
                    NewTabShows::Custom(p) => p.clone(),
                };
                let id = self.active;
                self.actions.push(Action::NewTab { panel: id, path, select: None, focus: true });
            }
            Cmd::CloseTab => {
                let (id, idx) = (self.active, self.panel().active);
                self.close_tab(id, idx);
            }
            Cmd::PinTab => {
                let t = self.tab();
                t.pinned = !t.pinned;
                // Settled into place at once, so the next command finds it where it shows.
                self.panel().settle_pins();
            }
            Cmd::CloseAllTabs => {
                let id = self.active;
                let n = self.panel().tabs.len();
                // Pinned tabs stay, unless they are all there is.
                let all = self.panel().tabs.iter().all(|t| t.pinned);
                for i in (0..n).rev() {
                    if !all && self.panel().tabs.get(i).is_some_and(|t| t.pinned) {
                        continue;
                    }
                    self.close_tab(id, i);
                }
            }
            Cmd::CloseOtherTabs => {
                let id = self.active;
                let n = self.panel().tabs.len();
                let keep = self.panel().active.min(n.saturating_sub(1));
                let kept = self.panel().tabs[keep].id;
                for i in (0..n).rev() {
                    if i != keep && !self.panel().tabs[i].pinned {
                        let p = self.panels.get_mut(&id).unwrap();
                        let t = p.tabs.remove(i);
                        self.closed_tabs.push(TabSnap { path: t.path.clone(), view: t.settings.clone(), ..Default::default() });
                    }
                }
                let p = self.panel();
                p.active = p.tabs.iter().position(|t| t.id == kept).unwrap_or(0);
            }
            Cmd::ReopenTab => {
                if let Some(s) = self.closed_tabs.pop() {
                    let id = self.active;
                    self.actions.push(Action::NewTab { panel: id, path: s.path, select: None, focus: true });
                }
            }
            Cmd::DuplicateTab => {
                let id = self.active;
                let p = self.panel().path();
                self.actions.push(Action::NewTab { panel: id, path: p, select: None, focus: true });
            }
            Cmd::NextTab | Cmd::PrevTab => {
                let p = self.panel();
                let n = p.tabs.len();
                p.active = if cmd == Cmd::NextTab { (p.active + 1) % n } else { (p.active + n - 1) % n };
            }
            Cmd::MoveTabLeft | Cmd::MoveTabRight => {
                let p = self.panel();
                let i = p.active;
                let j = if cmd == Cmd::MoveTabLeft { i.checked_sub(1) } else { Some(i + 1).filter(|j| *j < p.tabs.len()) };
                if let Some(j) = j {
                    p.tabs.swap(i, j);
                    p.active = j;
                }
            }
            Cmd::Tab1 | Cmd::Tab2 | Cmd::Tab3 | Cmd::Tab4 | Cmd::Tab5 | Cmd::Tab6 | Cmd::Tab7 | Cmd::Tab8 => {
                let n = [Cmd::Tab1, Cmd::Tab2, Cmd::Tab3, Cmd::Tab4, Cmd::Tab5, Cmd::Tab6, Cmd::Tab7, Cmd::Tab8].iter().position(|c| *c == cmd).unwrap();
                let p = self.panel();
                if n < p.tabs.len() {
                    p.active = n;
                }
            }
            Cmd::LastTab => {
                let p = self.panel();
                p.active = p.tabs.len() - 1;
            }
            Cmd::TabSearch => self.popup = Popup::tab_search(),
            Cmd::SplitRight | Cmd::SplitBottom => {
                let id = self.active;
                let p = self.panel().path();
                let side = if cmd == Cmd::SplitRight { Side::Right } else { Side::Bottom };
                self.actions.push(Action::Split { panel: id, path: p, side, select: None });
            }
            Cmd::ClosePanel => {
                let id = self.active;
                self.close_panel(id);
            }
            Cmd::FocusNextPanel | Cmd::FocusPrevPanel => {
                let mut leaves = vec![];
                self.tree.leaves(&mut leaves);
                if leaves.len() > 1 {
                    let i = leaves.iter().position(|&l| l == self.active).unwrap_or(0);
                    let n = leaves.len();
                    let j = if cmd == Cmd::FocusNextPanel { (i + 1) % n } else { (i + n - 1) % n };
                    self.focus_panel(leaves[j]);
                }
                ctx.memory_mut(|m| {
                    if let Some(f) = m.focused() {
                        m.surrender_focus(f)
                    }
                });
            }
            Cmd::CopyToOther | Cmd::MoveToOther => {
                let other = if self.prev_active != self.active && self.panels.contains_key(&self.prev_active) {
                    Some(self.prev_active)
                } else {
                    let mut leaves = vec![];
                    self.tree.leaves(&mut leaves);
                    leaves.into_iter().find(|&l| l != self.active)
                };
                let Some(o) = other else {
                    self.toast("No other panel");
                    return;
                };
                let dest = self.panels[&o].path();
                let srcs = self.tab().targets();
                if !srcs.is_empty() && !tab::is_place(&dest) {
                    self.begin_transfer(srcs, dest, cmd == Cmd::MoveToOther);
                }
            }
            Cmd::CopyTo | Cmd::MoveTo => crate::copyto::open(self, cmd == Cmd::MoveTo),
            Cmd::ToggleFlatten => {
                let t = self.tab();
                if t.tree_active() {
                    // Tree view keeps the hierarchy: unfold everything instead of flattening.
                    t.toggle_expand_all();
                } else {
                    let on = !t.flatten;
                    t.set_flatten(on);
                }
            }
            // Ctrl+F searches an open history that has the keyboard.
            Cmd::FocusFilter | Cmd::FindInPreview if self.panels.get(&self.active).and_then(|p| p.inspector.as_ref()).is_some_and(|i| i.git.is_some() && (i.keys || cmd == Cmd::FindInPreview)) => self.run(Cmd::GitSearch),
            // Ctrl+F finds in a preview that was clicked last, else filters the list.
            Cmd::FocusFilter if self.panels.get(&self.active).and_then(|p| p.inspector.as_ref()).is_some_and(|i| i.keys || i.find.is_some() && self.ctx.memory(|m| m.focused()) == Some(egui::Id::new(("insp-find", self.active)))) => self.run(Cmd::FindInPreview),
            Cmd::FocusFilter => self.panel().focus_filter = true,
            Cmd::FindInPreview => {
                let p = self.panel();
                let i = p.inspector.get_or_insert_with(Inspector::default);
                let f = i.find.get_or_insert_with(|| crate::findbar::Finder::new(String::new()));
                f.focus = true;
            }
            Cmd::CustomActions => self.popup = Popup::Actions { sel: 0 },
            Cmd::EditInPreview => {
                let id = self.active;
                let p = self.panel();
                let t = p.tab();
                let Some(e) = t.cursor.and_then(|i| t.entries.get(i as usize)).filter(|e| !e.is_dir).cloned() else { return };
                let i = p.inspector.get_or_insert_with(Inspector::default);
                if i.edit.as_ref().is_some_and(|ed| ed.path == e.path) {
                    return;
                }
                match crate::quickedit::Edit::open(&e.path) {
                    Ok(ed) => {
                        i.edit = Some(Box::new(ed));
                        i.find = None;
                        self.ctx.memory_mut(|m| m.request_focus(egui::Id::new(("insp-edit", id))));
                    }
                    Err(why) => self.toast(format!("Can't edit {}: {why}", e.name)),
                }
            }
            Cmd::ContentSearch => crate::contentsearch::toggle(self.panel()),
            Cmd::CodeSearch | Cmd::FindTodos => match crate::devtools::target_dir(self) {
                Some(d) => self.popup = crate::devtools::code_search(&ctx, d, cmd == Cmd::FindTodos),
                None => self.toast("Search Code needs a folder"),
            },
            Cmd::RunTask => match crate::devtools::target_dir(self) {
                Some(d) => self.popup = crate::devtools::tasks(&ctx, d),
                None => self.toast("Run Task needs a folder"),
            },
            Cmd::TidyFolder => crate::tidy::open(self),
            Cmd::EditTags => crate::musictags::open(self),
            Cmd::Timeline => {
                let panel = self.active;
                self.open_smart(panel, crate::smart::TIMELINE, false, true);
            }
            Cmd::BatchRename => {
                let panel = self.active;
                match crate::batchname::open(self, panel) {
                    Some(p) => self.popup = p,
                    None => self.toast("Select the items to rename"),
                }
            }
            Cmd::RecentProjects => self.popup = crate::projects::open(self, &ctx),
            Cmd::GitHubProfile => self.navigate(self.active, PathBuf::from(tab::GITHUB)),
            Cmd::QuickOpen => match crate::quickopen::open(self, &ctx) {
                Some(p) => self.popup = p,
                None => self.toast("Quick Open needs a folder"),
            },
            Cmd::WhatsUsing => {
                let t = self.tab();
                let mut paths: Vec<PathBuf> = t.target_entries().iter().filter(|e| tab::drive_dev(&e.path).is_none()).map(|e| e.path.clone()).collect();
                if paths.is_empty() && !tab::is_place(&t.path) {
                    paths.push(t.path.clone());
                }
                match paths.is_empty() {
                    true => self.toast("Select what to look for"),
                    false => self.popup = crate::holders::open(&ctx, crate::holders::What::Paths(paths), None),
                }
            }
            Cmd::WhatsUsingPort => self.popup = crate::holders::open(&ctx, crate::holders::What::Ports, None),
            Cmd::SaveSmart => {
                let t = self.tab();
                let sf = crate::smart::from_filter(&t.path, &t.filter);
                match t.filter.trim().is_empty() {
                    true => self.toast("Type what to search for in the filter first"),
                    false => self.popup = crate::smart::edit(&sf, false),
                }
            }
            Cmd::NewSmart => {
                let t = self.tab();
                let root = if tab::is_place(&t.path) || t.archive.is_some() { fsx::home() } else { t.path.clone() };
                let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.popup = crate::smart::edit(&crate::smart::SmartFolder { root, name, ..Default::default() }, false);
            }
            Cmd::FindDuplicates => match crate::devtools::target_dir(self) {
                Some(d) => self.popup = crate::dupes::open(&ctx, d),
                None => self.toast("Find Duplicates needs a folder"),
            },
            Cmd::PreviousVersions => crate::versions::open(self),
            Cmd::SpaceUsed => crate::spaceuse::open(self),
            Cmd::FreeUpSpace => crate::spaceuse::open_free(self),
            Cmd::ViewPictures => crate::viewer::open(self),
            Cmd::ViewCalendar => crate::calendar::open(self),
            Cmd::AddToShelf => crate::shelf::add_selection(self),
            Cmd::ShelfMoveHere | Cmd::ShelfCopyHere => crate::shelf::put_here(self, cmd == Cmd::ShelfMoveHere),
            Cmd::ClearShelf => self.shelf.items.clear(),
            Cmd::CleanBuildsHome => self.popup = crate::devtools::clean(&ctx, fsx::home()),
            Cmd::CleanBuilds => match crate::devtools::target_dir(self) {
                Some(d) => self.popup = crate::devtools::clean(&ctx, d),
                None => self.toast("Clean Build Folders needs a folder"),
            },
            Cmd::OpenInEditor => {
                let mut paths = self.tab().selected_paths();
                if paths.is_empty() {
                    paths = self.tab().target_entries().iter().map(|e| e.path.clone()).collect();
                }
                if paths.is_empty() && !tab::is_place(&self.panel().path()) {
                    paths = vec![self.panel().path()];
                }
                let hint = self.panel().line_hint.clone();
                for p in paths.into_iter().take(20) {
                    let line = hint.as_ref().filter(|h| h.0 == p).map(|h| h.1);
                    self.open_in_editor(&p, line);
                }
            }
            Cmd::ChooseTheme => self.popup = Popup::Themes,
            Cmd::CheckUpdates => {
                self.updater.check(&ctx);
                self.popup = Popup::options_at(crate::popups::Section::System);
            }
            Cmd::MeaningSearch => {
                if let Some(e) = &self.semantic {
                    e.poke();
                }
                self.popup = Popup::meaning();
            }
            Cmd::ComparePanels | Cmd::SelectDifferences | Cmd::SyncPanels => crate::compare::run(self, cmd),
            Cmd::TypesFilter if crate::picker::open_filters(self) => {}
            Cmd::TypesFilter => self.popup = Popup::types(self.active),
            Cmd::ToggleHidden => {
                self.cfg.show_hidden = !self.cfg.show_hidden;
                self.set_config_changed();
            }
            Cmd::ToggleSystem => {
                self.cfg.show_system = !self.cfg.show_system;
                self.set_config_changed();
            }
            Cmd::ToggleExtensions => {
                self.cfg.show_ext = !self.cfg.show_ext;
                self.set_config_changed();
            }
            Cmd::ToggleRecentsOnTop => {
                self.cfg.recents_on_top = !self.cfg.recents_on_top;
                self.set_config_changed();
            }
            Cmd::ToggleSidebar => self.sidebar = !self.sidebar,
            // Columns sits beside the zoom axis: there is nothing in it to grow.
            Cmd::ZoomIn | Cmd::ZoomOut if self.tab().settings.column_view || self.tab().settings.stack_view => {}
            Cmd::ZoomIn | Cmd::ZoomOut => {
                let t = self.tab();
                t.settings.zoom = crate::listing::zoom_step(t.settings.zoom, if cmd == Cmd::ZoomIn { 1 } else { -1 });
                t.scroll_to_cursor = true;
            }
            Cmd::ViewDetails | Cmd::ViewTree | Cmd::ViewList | Cmd::ViewMedium | Cmd::ViewLarge | Cmd::ViewXL => {
                let z = match cmd {
                    Cmd::ViewDetails => 0.0,
                    Cmd::ViewTree => 0.1,
                    Cmd::ViewList => 0.2,
                    Cmd::ViewMedium => 0.3,
                    Cmd::ViewLarge => 0.65,
                    _ => 1.0,
                };
                let t = self.tab();
                t.settings.zoom = z;
                // (Groups come back with the other views.)
                t.dirty |= std::mem::take(&mut t.settings.column_view) | std::mem::take(&mut t.settings.stack_view);
                t.scroll_to_cursor = true;
            }
            Cmd::ViewColumns => {
                let t = self.tab();
                t.settings.column_view = true;
                t.settings.stack_view = false;
                t.dirty = true;
                t.scroll_to_cursor = true;
            }
            Cmd::ViewStack => {
                let t = self.tab();
                t.settings.stack_view = true;
                t.settings.column_view = false;
                t.dirty = true;
                t.scroll_to_cursor = true;
            }
            Cmd::GroupNone | Cmd::GroupName | Cmd::GroupType | Cmd::GroupDate | Cmd::GroupCreated | Cmd::GroupAccessed | Cmd::GroupSize | Cmd::GroupItems | Cmd::GroupUsage | Cmd::GroupDeleted | Cmd::GroupOriginal | Cmd::GroupFree | Cmd::GroupFs | Cmd::GroupFull => {
                let t = self.tab();
                t.settings.group = cmd.group_key().unwrap_or_default();
                t.collapsed_groups.clear();
                t.dirty = true;
            }
            Cmd::GroupTag => {
                let t = self.tab();
                t.settings.group = crate::config::GroupBy::Tag;
                t.collapsed_groups.clear();
                t.dirty = true;
            }
            Cmd::SortTags => self.tab().set_sort(SortKey::Tags),
            Cmd::SortTaken => self.tab().set_sort(SortKey::Taken),
            Cmd::SortDimensions => self.tab().set_sort(SortKey::Dimensions),
            Cmd::SortDuration => self.tab().set_sort(SortKey::Duration),
            Cmd::SortColour => self.tab().set_sort(SortKey::Colour),
            Cmd::GroupOrigin => {
                let t = self.tab();
                t.settings.group = crate::config::GroupBy::Origin;
                t.collapsed_groups.clear();
                t.dirty = true;
            }
            Cmd::GroupTaken => {
                let t = self.tab();
                t.settings.group = crate::config::GroupBy::Taken;
                t.collapsed_groups.clear();
                t.dirty = true;
            }
            Cmd::GroupExpandAll => {
                let t = self.tab();
                t.collapsed_groups.clear();
                t.dirty = true;
            }
            Cmd::GroupCollapseAll => {
                let t = self.tab();
                let labels: Vec<String> = t.header_labels();
                t.collapsed_groups.extend(labels);
                t.dirty = true;
            }
            Cmd::SavedView => self.popup = Popup::saved_view(self.active),
            Cmd::SortName | Cmd::SortSize | Cmd::SortModified | Cmd::SortType | Cmd::SortCreated => {
                let k = match cmd {
                    Cmd::SortName => SortKey::Name,
                    Cmd::SortSize => SortKey::Size,
                    Cmd::SortModified => SortKey::Modified,
                    Cmd::SortType => SortKey::Type,
                    _ => SortKey::Created,
                };
                self.tab().set_sort(k);
            }
            Cmd::SortNameAsc | Cmd::SortNameDesc | Cmd::SortSizeAsc | Cmd::SortSizeDesc | Cmd::SortFullAsc | Cmd::SortFullDesc => {
                let (k, desc) = match cmd {
                    Cmd::SortNameAsc => (SortKey::Name, false),
                    Cmd::SortNameDesc => (SortKey::Name, true),
                    Cmd::SortSizeAsc => (SortKey::Size, false),
                    Cmd::SortSizeDesc => (SortKey::Size, true),
                    Cmd::SortFullAsc => (SortKey::Usage, false),
                    _ => (SortKey::Usage, true),
                };
                let t = self.tab();
                t.settings.sort = k;
                t.settings.desc = desc;
                t.dirty = true;
                t.scroll_to_cursor = true;
            }
            Cmd::ToggleReadme => {
                // Back on everywhere, the folders it was closed in included.
                self.cfg.show_readme = !self.cfg.show_readme;
                if self.cfg.show_readme {
                    self.ud.readme_hidden.clear();
                    self.ud_dirty = true;
                }
                self.cfg_dirty = true;
            }
            Cmd::ToggleRecents => {
                self.cfg.highlight_recents = !self.cfg.highlight_recents;
                self.set_config_changed();
            }
            Cmd::CloseRightTabs | Cmd::CloseLeftTabs => {
                let id = self.active;
                let cur = self.panel().active;
                let n = self.panel().tabs.len();
                let range: Vec<usize> = if cmd == Cmd::CloseRightTabs { ((cur + 1)..n).rev().collect() } else { (0..cur).rev().collect() };
                let kept = self.panel().tabs.get(cur).map(|t| t.id);
                for i in range {
                    let p = self.panels.get_mut(&id).unwrap();
                    if p.tabs[i].pinned {
                        continue;
                    }
                    let t = p.tabs.remove(i);
                    self.closed_tabs.push(TabSnap { path: t.path.clone(), view: t.settings.clone(), ..Default::default() });
                }
                let p = self.panel();
                p.active = kept.and_then(|k| p.tabs.iter().position(|t| t.id == k)).unwrap_or(0);
            }
            Cmd::OpenTabInWindow => {
                let p = self.panel().path();
                if crate::mime::new_window(&p).is_ok() && self.panel().tabs.len() > 1 {
                    let (id, idx) = (self.active, self.panel().active);
                    self.close_tab(id, idx);
                }
            }
            Cmd::ToggleFoldersFirst => {
                self.cfg.folders_first = !self.cfg.folders_first;
                self.set_config_changed();
            }
            Cmd::ToggleInspector => {
                let id = self.active;
                let p = self.panel();
                if p.inspector.as_ref().is_some_and(|i| i.edit.is_some()) {
                    self.leave_edit(id, true);
                    return;
                }
                p.inspector = if p.inspector.is_some() { None } else { Some(Inspector::default()) };
            }
            Cmd::GitHistory => {
                let p = self.panel();
                let repo = crate::gitstatus::repo_for(&p.path());
                match (p.inspector.as_mut().and_then(|i| i.git.as_mut()), repo) {
                    (Some(h), _) => h.top(),
                    (None, Some(r)) => {
                        let opened = p.inspector.is_none();
                        p.inspector.get_or_insert_with(Inspector::default).show_history(crate::gitlog::History::new(r.root.clone(), r.branch.clone(), opened));
                    }
                    _ => {}
                }
            }
            Cmd::GitBranches => {
                let p = self.panel();
                if let Some(r) = crate::gitstatus::repo_for(&p.path()) {
                    // Above the branch chip; from the palette with no chip shown, above the
                    // panel's lower right corner.
                    let at = p.branch_chip.unwrap_or_else(|| Rect::from_min_size(p.rect.right_bottom() - vec2(120.0, 30.0), vec2(100.0, 20.0)));
                    self.popup = Popup::branches(p.id, r.root.clone(), at);
                }
            }
            Cmd::OnlyChanged => {
                let t = self.panel().tab_mut();
                t.only_changed = !t.only_changed;
                let on = t.only_changed;
                if on && crate::gitstatus::repo_for(&self.tab().path).is_none() {
                    self.panel().tab_mut().only_changed = false;
                    self.toast("Not in a git repository");
                } else {
                    self.toast(if on { "Showing only changed files" } else { "Showing every file" });
                }
            }
            Cmd::GitSearch => {
                let p = self.panel();
                // An open history is searched as it is; else this folder's is opened.
                if p.inspector.as_ref().and_then(|i| i.git.as_ref()).is_none() {
                    let Some(r) = crate::gitstatus::repo_for(&p.path()) else { return };
                    let opened = p.inspector.is_none();
                    p.inspector.get_or_insert_with(Inspector::default).show_history(crate::gitlog::History::new(r.root.clone(), r.branch.clone(), opened));
                }
                let here = p.path();
                if let Some(h) = p.inspector.as_mut().and_then(|i| i.git.as_mut()) {
                    h.here = here.strip_prefix(&h.root).map(|r| r.to_string_lossy().into_owned()).unwrap_or_default();
                    h.find();
                }
            }
            Cmd::GitChanges => {
                let p = self.panel();
                if let Some(r) = crate::gitstatus::repo_for(&p.path()) {
                    if p.inspector.as_ref().and_then(|i| i.git.as_ref()).is_none() {
                        let opened = p.inspector.is_none();
                        p.inspector.get_or_insert_with(Inspector::default).show_history(crate::gitlog::History::new(r.root.clone(), r.branch.clone(), opened));
                    }
                    if let Some(h) = p.inspector.as_mut().and_then(|i| i.git.as_mut()) {
                        h.show_changes();
                    }
                }
            }
            Cmd::InspectorLineNumbers => {
                if let Some(i) = self.panel().inspector.as_mut() {
                    i.line_numbers = !i.line_numbers;
                }
            }
            Cmd::InspectorWrap => {
                if let Some(i) = self.panel().inspector.as_mut() {
                    // The history's diffs follow the code setting.
                    if i.git.is_some() {
                        i.wrap_code = !i.wrap_code;
                    } else {
                        i.toggle_wrap();
                    }
                }
            }
            Cmd::InspectorSource => {
                if let Some(k) = self.panel().inspector.as_ref().and_then(|i| i.rich_kind()) {
                    let v = !self.cfg.source_views.get(k);
                    self.cfg.source_views.set(k, v);
                    self.cfg_dirty = true;
                }
            }
            Cmd::InspectorBlame => {
                if let Some(i) = self.panel().inspector.as_mut() {
                    i.blame_on = !i.blame_on;
                }
            }
            Cmd::InspectorFollow => {
                if let Some(i) = self.panel().inspector.as_mut() {
                    i.toggle_follow();
                }
            }
            Cmd::InspectorMinimap => {
                if let Some(i) = self.panel().inspector.as_mut() {
                    i.minimap = !i.minimap;
                }
            }
            Cmd::Properties => {
                let mut t = self.tab().targets();
                if t.is_empty() {
                    t = vec![self.panel().path()];
                }
                t.retain(|p| !tab::is_place(p) && tab::drive_dev(p).is_none());
                if !t.is_empty() {
                    for p in &t {
                        if p.is_dir() {
                            self.stats.request(p);
                        }
                    }
                    self.popup = Popup::properties(t);
                }
            }
            Cmd::Copy | Cmd::Cut => {
                let mut paths = self.tab().targets();
                paths.retain(|p| tab::drive_dev(p).is_none());
                if paths.is_empty() {
                    return;
                }
                let cut = cmd == Cmd::Cut;
                let dirs = self.tab().target_entries().iter().filter(|e| e.is_dir && paths.contains(&e.path)).count();
                let from = self.panel().path();
                crate::pastes::remember(&mut self.pastes, crate::pastes::Set { paths: paths.clone(), cut, dirs, from, at: crate::jump::now() });
                crate::clipboard::set_files(&paths, cut);
                self.toast(format!("{} {} item{}", if cut { "Cut" } else { "Copied" }, paths.len(), if paths.len() == 1 { "" } else { "s" }));
                if cut {
                    // Cut files march a moment from now (`listing::paint_ants`).
                    let now = self.ctx.input(|i| i.time);
                    self.ctx.data_mut(|d| d.insert_temp(egui::Id::new("maia cut since"), now));
                }
                self.clipboard = Some((paths, cut));
            }
            Cmd::Paste => self.paste(),
            Cmd::PasteEarlier => crate::pastes::open(self),
            Cmd::CopyPathRelative => {
                let mut paths = self.tab().selected_paths();
                if paths.is_empty() {
                    paths = self.tab().target_entries().iter().map(|e| e.path.clone()).collect();
                }
                if paths.is_empty() {
                    paths = vec![self.panel().path()];
                }
                let root = crate::project::root(paths[0].parent().filter(|_| !paths[0].is_dir()).unwrap_or(&paths[0]));
                let s = paths.iter().map(|p| crate::project::relative(p, &root)).collect::<Vec<_>>().join("\n");
                ctx.copy_text(s.clone());
                self.toast(format!("Copied {}", if paths.len() == 1 { s } else { format!("{} paths", paths.len()) }));
            }
            Cmd::CopyCodeBlocks => {
                let mut paths = self.tab().selected_paths();
                if paths.is_empty() {
                    paths = self.tab().target_entries().iter().map(|e| e.path.clone()).collect();
                }
                if paths.is_empty() {
                    self.toast("Select files to copy");
                    return;
                }
                self.share(&ctx, move || {
                    let first = &paths[0];
                    let root = crate::project::root(if first.is_dir() { first } else { first.parent().unwrap_or(first) });
                    let b = crate::share::code_blocks(&paths, &root);
                    ShareDone { copy: (b.files > 0).then(|| b.text.clone()), open: vec![], toast: b.note(), rescan: None, renamed: None }
                });
            }
            Cmd::CopyWebLink => {
                let mut ents: Vec<(PathBuf, bool)> = self.tab().target_entries().iter().map(|e| (e.path.clone(), e.is_dir)).collect();
                if ents.is_empty() {
                    ents = vec![(self.panel().path(), true)];
                }
                let hint = self.panel().line_hint.clone();
                self.share(&ctx, move || {
                    let toast = match crate::share::web_links(&ents, hint) {
                        Ok((links, caution)) => {
                            let what = if ents.len() == 1 { "Copied the link".to_string() } else { format!("Copied {} links", ents.len()) };
                            return ShareDone { copy: Some(links), open: vec![], toast: caution.map_or(what.clone(), |c| format!("{what}; {c}")), rescan: None, renamed: None };
                        }
                        Err(e) => format!("Couldn't make a link: {e}"),
                    };
                    ShareDone { copy: None, open: vec![], toast, rescan: None, renamed: None }
                });
            }
            Cmd::OpenWebLink => {
                let mut ents: Vec<(PathBuf, bool)> = self.tab().target_entries().iter().map(|e| (e.path.clone(), e.is_dir)).collect();
                if ents.is_empty() {
                    ents = vec![(self.panel().path(), true)];
                }
                // A handful of tabs at most, however many are selected.
                ents.truncate(5);
                let hint = self.panel().line_hint.clone();
                self.share(&ctx, move || match crate::share::web_links(&ents, hint) {
                    Ok((links, caution)) => {
                        let toast = caution.map_or_else(|| "Opened in the browser".to_string(), |c| format!("Opened in the browser; {c}"));
                        ShareDone { copy: None, open: links.lines().map(String::from).collect(), toast, rescan: None, renamed: None }
                    }
                    Err(e) => ShareDone { copy: None, open: vec![], toast: format!("Couldn't make a link: {e}"), rescan: None, renamed: None },
                });
            }
            Cmd::SetWallpaper | Cmd::Email | Cmd::Print | Cmd::RotateLeft | Cmd::RotateRight | Cmd::ResizeHalf | Cmd::ResizeQuarter | Cmd::ResizeFit | Cmd::ResizeCustom | Cmd::CropPicture => crate::errands::run(self, cmd),
            Cmd::CompareFiles => {
                let files: Vec<PathBuf> = self.tab().target_entries().iter().filter(|e| !e.is_dir).map(|e| e.path.clone()).collect();
                let [a, b] = files.as_slice() else {
                    self.toast("Select two files to compare");
                    return;
                };
                let c = crate::filediff::Compare::start(a.clone(), b.clone(), &ctx);
                let ins = self.panel().inspector.get_or_insert_with(Inspector::default);
                ins.git = None;
                ins.compare = Some(Box::new(c));
            }
            Cmd::GitIgnore | Cmd::GitIgnoreExt => {
                let Some(root) = crate::gitstatus::repo_for(&self.tab().path).map(|r| r.root.clone()) else {
                    self.toast("Not in a git repository");
                    return;
                };
                let ents: Vec<(PathBuf, bool)> = self.tab().target_entries().iter().map(|e| (e.path.clone(), e.is_dir)).filter(|e| e.0 != root).collect();
                let lines: Vec<String> = if cmd == Cmd::GitIgnoreExt {
                    match crate::share::shared_ext(ents.iter().map(|e| (e.0.as_path(), e.1))) {
                        Some(ext) => vec![crate::share::ignore_ext_line(&ext)],
                        None => {
                            self.toast("Select files with one extension");
                            return;
                        }
                    }
                } else {
                    ents.iter().map(|(p, d)| crate::share::ignore_line(&crate::project::relative(p, &root), *d)).collect()
                };
                if lines.is_empty() {
                    self.toast("Select files or folders to ignore");
                    return;
                }
                let paths: Vec<PathBuf> = ents.into_iter().map(|e| e.0).collect();
                self.share(&ctx, move || {
                    let toast = match crate::share::add_ignores(&root, &lines, &paths) {
                        Ok((added, tracked)) => crate::share::ignore_note(&lines, added, &tracked),
                        Err(e) => e,
                    };
                    ShareDone { copy: None, open: vec![], toast, rescan: Some(root), renamed: None }
                });
            }
            Cmd::CopyPathLine => {
                // Only the file a search showed a line of has one.
                let hint = self.panel().line_hint.clone();
                let here: Vec<PathBuf> = self.tab().target_entries().iter().map(|e| e.path.clone()).collect();
                match hint.filter(|h| here.contains(&h.0)) {
                    Some((p, line)) => {
                        let root = crate::project::root(p.parent().unwrap_or(&p));
                        let s = crate::project::with_line(&crate::project::relative(&p, &root), line);
                        ctx.copy_text(s.clone());
                        self.toast(format!("Copied {s}"));
                    }
                    None => self.toast("No line is known for that: find it with Search Code first"),
                }
            }
            Cmd::CopyPath | Cmd::CopyPathRaw | Cmd::CopyName => {
                let mut paths = self.tab().selected_paths();
                if paths.is_empty() {
                    paths = vec![self.panel().path()];
                }
                // An unmounted drive's path is its device node.
                for p in paths.iter_mut() {
                    if let Some(d) = tab::drive_dev(p) {
                        *p = PathBuf::from(d);
                    }
                }
                let s = paths
                    .iter()
                    .map(|p| match cmd {
                        Cmd::CopyName => p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                        Cmd::CopyPathRaw => p.to_string_lossy().into_owned(),
                        _ => crate::clipboard::quote_if_needed(&p.to_string_lossy()),
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                ctx.copy_text(s.clone());
                self.toast(format!("Copied {}", if paths.len() == 1 { s } else { format!("{} paths", paths.len()) }));
            }
            Cmd::Trash => {
                let paths = self.tab().targets();
                if paths.is_empty() {
                    return;
                }
                self.move_cursor_past_selection();
                self.actions.push(Action::StartJob(OpKind::Trash { paths }));
            }
            Cmd::DeletePermanent => {
                let paths = self.tab().targets();
                if paths.is_empty() {
                    return;
                }
                let msg = if paths.len() == 1 {
                    format!("Permanently delete “{}”? This can't be undone.", paths[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
                } else {
                    format!("Permanently delete {} items? This can't be undone.", paths.len())
                };
                self.popup = Popup::confirm(msg, "Delete", Box::new(move |app: &mut App| {
                    app.move_cursor_past_selection();
                    app.actions.push(Action::StartJob(OpKind::Delete { paths: paths.clone() }));
                }));
            }
            Cmd::Rename => {
                let t = self.tab();
                let mut idx: Vec<u32> = if t.selected.is_empty() { t.cursor.into_iter().collect() } else { t.selected.iter().copied().collect() };
                idx.sort_by_key(|i| t.view_pos(*i));
                if idx.is_empty() || tab::is_place(&t.path) {
                    return;
                }
                let items = idx.iter().map(|&i| (i, t.entries[i as usize].path.clone(), t.entries[i as usize].name.clone())).collect();
                t.rename = Some(Rename::new(items, false));
                t.scroll_to_cursor = true;
                ctx.memory_mut(|m| {
                    if let Some(f) = m.focused() {
                        m.surrender_focus(f)
                    }
                });
            }
            Cmd::NewFolder | Cmd::NewFile => {
                let dir = self.panel().path();
                if tab::is_place(&dir) {
                    return;
                }
                let name = if cmd == Cmd::NewFolder { "New folder" } else { "New file.txt" };
                let dest = fsx::unique_dest(&dir, name);
                let r = if cmd == Cmd::NewFolder { std::fs::create_dir(&dest) } else { std::fs::File::create_new(&dest).map(|_| ()) };
                match r {
                    Ok(()) => {
                        self.undo.push(UndoEntry { label: format!("Create {}", dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()), kind: UndoKind::Created(vec![dest.clone()]) });
                        let t = self.tab();
                        t.filter.clear();
                        t.pending_rename = Some(dest);
                        t.reload();
                    }
                    Err(e) => match crate::drives::Tools::current().filter(|t| crate::admin::denied(&e) && t.has("pkexec")) {
                        // Not ours to write: offer to do it with administrator rights.
                        Some(tools) => {
                            let task = if cmd == Cmd::NewFolder { crate::admin::Task::NewFolder(dest.clone()) } else { crate::admin::Task::NewFile(dest.clone()) };
                            let msg = format!("You don't have permission to change “{}”. Create “{name}” as administrator?", dir.display());
                            self.popup = Popup::confirm(msg, "Use administrator", Box::new(move |app: &mut App| {
                                app.actions.push(Action::StartJob(OpKind::Admin { task, tools }));
                            }));
                        }
                        None => self.toast(format!("Couldn't create: {e}")),
                    },
                }
            }
            Cmd::ConnectServer => self.popup = Popup::connect(String::new(), None),
            Cmd::ExtractHere => {
                if !crate::archive::available() {
                    self.toast(crate::archive::EXTRACT_NEEDS);
                    return;
                }
                let t = self.tab();
                let targets = t.targets();
                if let Some(archive) = t.archive.clone() {
                    // Inside an archive: what is selected (or all of it) goes beside the archive.
                    let members: Vec<PathBuf> = targets.iter().filter_map(|p| p.strip_prefix(&archive).ok().map(Path::to_path_buf)).collect();
                    let dest = archive.parent().map(Path::to_path_buf).unwrap_or_default();
                    self.actions.push(Action::StartJob(OpKind::Extract { archive, members, dest }));
                    return;
                }
                let archives: Vec<PathBuf> = targets.into_iter().filter(|p| p.file_name().is_some_and(|n| crate::archive::is_archive(&n.to_string_lossy())) && p.is_file()).collect();
                if archives.is_empty() {
                    self.toast("Select an archive to extract");
                }
                for a in archives {
                    let dest = a.parent().map(Path::to_path_buf).unwrap_or_default();
                    self.actions.push(Action::StartJob(OpKind::Extract { archive: a, members: vec![], dest }));
                }
            }
            Cmd::ConvertJpg | Cmd::ConvertPng | Cmd::ConvertWebp | Cmd::ConvertPdf | Cmd::ConvertMp3 | Cmd::ConvertOpus | Cmd::ConvertFlac | Cmd::ConvertMp4 | Cmd::ConvertWebm | Cmd::ConvertSoundMp3 | Cmd::MergePdfs | Cmd::SplitPdf => {
                if let Some(to) = crate::convert::Target::of(cmd) {
                    crate::convert::start(self, to);
                }
            }
            Cmd::CompressLocked | Cmd::CompressZip | Cmd::CompressTarGz | Cmd::CompressTarXz | Cmd::Compress7z => {
                if !crate::archive::available() {
                    self.toast(crate::archive::COMPRESS_NEEDS);
                    return;
                }
                let t = self.tab();
                let srcs = t.targets();
                if t.archive.is_some() || tab::is_place(&t.path) || srcs.is_empty() {
                    self.toast("Select what to compress");
                    return;
                }
                let format = match cmd {
                    Cmd::CompressTarGz => crate::archwrite::Format::TarGz,
                    Cmd::CompressTarXz => crate::archwrite::Format::TarXz,
                    Cmd::Compress7z => crate::archwrite::Format::SevenZ,
                    _ => crate::archwrite::Format::Zip,
                };
                self.popup = Popup::Compress(crate::popups::CompressState::new(srcs, t.path.clone(), format, cmd == Cmd::CompressLocked));
            }
            Cmd::CreateLink => {
                let paths = self.tab().targets();
                let dir = self.panel().path();
                let mut made = vec![];
                for p in paths {
                    let name = format!("{} (link)", p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                    let dest = fsx::unique_dest(&dir, &name);
                    if let Err(e) = std::os::unix::fs::symlink(&p, &dest) {
                        self.toast(format!("Couldn't create link: {e}"));
                    } else {
                        made.push(dest);
                    }
                }
                if !made.is_empty() {
                    self.undo.push(UndoEntry { label: "Create links".into(), kind: UndoKind::Created(made) });
                }
                self.tab().reload();
            }
            Cmd::Duplicate => {
                let paths = self.tab().targets();
                let dir = self.panel().path();
                if !paths.is_empty() {
                    self.actions.push(Action::StartJob(OpKind::Copy { srcs: paths, dest: dir, decide: HashMap::new() }));
                }
            }
            Cmd::OpenWith => {
                let paths: Vec<PathBuf> = self.tab().target_entries().iter().filter(|e| !e.is_dir).map(|e| e.path.clone()).collect();
                if !paths.is_empty() {
                    self.popup = Popup::open_with(paths);
                }
            }
            Cmd::ContextMenu => {
                let id = self.active;
                // An open history has the keyboard: the selected commit's menu.
                match self.panels.get(&id).and_then(|p| p.inspector.as_ref()?.git.as_ref()?.menu(id)) {
                    Some(m) => self.popup = m,
                    None => {
                        let pos = self.tab().cursor_screen_pos.unwrap_or_else(|| self.panels[&id].list_rect.center());
                        let bg = self.tab().targets().is_empty();
                        self.actions.push(Action::CtxMenu { panel: id, pos, background: bg });
                    }
                }
            }
            Cmd::RestoreTrash => self.restore_from_trash(),
            Cmd::EmptyTrash => {
                self.popup = Popup::confirm("Permanently delete everything in the trash?".into(), "Empty trash", Box::new(|app: &mut App| {
                    let errs = crate::trashcan::list().iter().filter(|it| crate::trashcan::purge(it).is_err()).count();
                    if errs == 0 {
                        app.toast("Trash emptied");
                    } else {
                        app.toast(format!("Couldn't remove {errs} item(s) from the trash"));
                    }
                    for p in app.panels.values_mut() {
                        for t in &mut p.tabs {
                            t.reload();
                        }
                    }
                }));
            }
            Cmd::SelectAll => {
                let t = self.tab();
                t.selected = t.view.iter().copied().collect();
            }
            Cmd::SelectNone => self.tab().selected.clear(),
            Cmd::InvertSelection => {
                let t = self.tab();
                let all: HashSet<u32> = t.view.iter().copied().collect();
                t.selected = all.difference(&t.selected).copied().collect();
            }
            Cmd::SelectMatching => self.popup = Popup::prompt("Select matching", String::new(), Box::new(|app: &mut App, text: String| crate::selmatch::run(app, &text))),
            Cmd::ToggleSelection => {
                let t = self.tab();
                if let Some(c) = t.cursor {
                    if !t.selected.remove(&c) {
                        t.selected.insert(c);
                    }
                    t.anchor = Some(c);
                }
            }
            Cmd::SaveLayout => self.popup = Popup::prompt("Save layout as", String::new(), Box::new(|app: &mut App, name: String| {
                if name.trim().is_empty() {
                    return;
                }
                let snap = app.layout_snap(&app.ctx.clone());
                app.ud.layouts.insert(name.trim().to_string(), snap);
                app.ud_dirty = true;
                app.toast(format!("Saved layout “{}”", name.trim()));
            })),
            Cmd::LoadLayout => self.popup = Popup::layouts(false),
            Cmd::DeleteLayout => self.popup = Popup::layouts(true),
            Cmd::Palette => self.popup = Popup::palette(),
            Cmd::Options => self.popup = Popup::options(),
            Cmd::MainMenu => self.popup = Popup::MainMenu,
            Cmd::QuickMenu => {
                self.quick_anchor = None;
                self.popup = Popup::QuickMenu;
            }
            Cmd::NewWindow => {
                let _ = crate::mime::new_window(&fsx::home());
            }
            Cmd::Quit => {
                if !self.ask_unsaved() {
                    self.quit = true;
                }
            }
            Cmd::FontBigger => {
                self.cfg.font_size = (self.cfg.font_size + 0.5).min(24.0);
                self.cfg_dirty = true;
            }
            Cmd::FontSmaller => {
                self.cfg.font_size = (self.cfg.font_size - 0.5).max(9.0);
                self.cfg_dirty = true;
            }
            Cmd::ToggleAnimations => {
                self.cfg.animations = !self.cfg.animations;
                self.cfg_dirty = true;
            }
            Cmd::OpenConfig => {
                if !config::config_path().exists() {
                    let _ = config::save_json(&config::config_path(), &self.cfg);
                }
                let _ = crate::mime::open_default(&config::config_path());
            }
            Cmd::RevealConfig => {
                let _ = std::fs::create_dir_all(config::config_dir());
                let id = self.active;
                self.actions.push(Action::NewTab { panel: id, path: config::config_dir(), select: None, focus: true });
            }
            Cmd::ResetConfig => {
                self.popup = Popup::confirm("Reset all options and hotkeys to their defaults?".into(), "Reset", Box::new(|app: &mut App| {
                    let _ = std::fs::copy(config::config_path(), config::config_path().with_extension("json.bak"));
                    app.cfg = Config::default();
                    app.set_config_changed();
                    app.toast("Settings reset (previous saved as config.json.bak)");
                }));
            }
            Cmd::Install => match crate::install::install() {
                Ok(msg) => self.toast(msg),
                Err(e) => self.toast(format!("Install failed: {e}")),
            },
            Cmd::SetDefault => self.set_default_fm(true),
            Cmd::Uninstall => match crate::install::uninstall() {
                Ok(msg) => self.toast(msg),
                Err(e) => self.toast(format!("Uninstall failed: {e}")),
            },
            Cmd::About => self.popup = Popup::About,
            Cmd::Welcome => self.popup = crate::tour::open(crate::tour::Kind::Welcome, false),
            Cmd::WhatsNew => self.popup = crate::tour::open(crate::tour::Kind::News, false),
            Cmd::Redo => {
                self.redo_last();
            }
            Cmd::Undo => {
                self.undo_last();
            }
            Cmd::UndoHistory => self.popup = Popup::UndoHistory(crate::undohist::State::new(&self.undo)),
            Cmd::Transfers => {
                if crate::transfers::live(self) == 0 {
                    self.toast("Nothing is being copied or moved");
                } else {
                    // Above the chip; in a bar too narrow for it, above the window's corner.
                    let corner = ctx.content_rect().right_bottom() - egui::vec2(8.0, 6.0);
                    let at = self.jobs_chip.unwrap_or(egui::Rect::from_min_max(corner - egui::vec2(1.0, crate::ui::FOOTER_H), corner));
                    self.popup = Popup::Transfers(crate::transfers::State::new(at));
                }
            }
            Cmd::Hints => crate::hints::start(self, &ctx),
            Cmd::SetMark => match crate::marks::here(self) {
                Some(_) => self.popup = Popup::Marks(crate::marks::State { set: true, sel: 0 }),
                None => self.toast("Nothing here to mark"),
            },
            Cmd::GoToMark => self.popup = Popup::Marks(crate::marks::State { set: false, sel: 0 }),
            Cmd::Jump => crate::jump::open(self),
            Cmd::SendToPhone => crate::phone::open(self),
            Cmd::ToggleSharePlace => {
                self.cfg.share_without_place = !self.cfg.share_without_place;
                self.cfg_dirty = true;
                self.toast(if self.cfg.share_without_place { "Photos are shared without their place and camera" } else { "Photos are shared as they are" });
            }
            Cmd::PinToTop => {
                let sel = self.tab().targets();
                if sel.is_empty() || tab::is_place(&self.panel().path()) || self.tab().archive.is_some() {
                    self.toast("Select what to pin");
                } else {
                    // All pinned already: the same command takes them off.
                    let unpin = sel.iter().all(|p| self.pinned_set.contains(p));
                    self.ud.pinned.retain(|p| !sel.contains(p));
                    if !unpin {
                        self.ud.pinned.extend(sel.iter().cloned());
                    }
                    self.refresh_recents();
                    self.ud_dirty = true;
                    for p in self.panels.values_mut() {
                        for t in &mut p.tabs {
                            t.dirty = true;
                        }
                    }
                }
            }
        }
    }

    fn target_folder(&mut self) -> PathBuf {
        let t = self.tab();
        t.target_entries().first().filter(|e| e.is_dir).map(|e| e.path.clone()).unwrap_or_else(|| t.path.clone())
    }

    pub(crate) fn move_cursor_past_selection(&mut self) {
        let t = self.tab();
        let positions: Vec<usize> = t.targets_idx().iter().filter_map(|&i| t.view_pos(i)).collect();
        let (Some(&lo), Some(&hi)) = (positions.iter().min(), positions.iter().max()) else { return };
        let next = if hi + 1 < t.view.len() { Some(hi + 1) } else { lo.checked_sub(1) };
        t.selected.clear();
        if let Some(n) = next {
            let idx = t.view[n];
            t.cursor = Some(idx);
            t.anchor = Some(idx);
            t.selected.insert(idx);
        } else {
            t.cursor = None;
        }
    }

    pub(crate) fn track_leave(&mut self, old: &Path) {
        if self.cfg.track_recents() && !tab::is_place(old) {
            // Folders inside archives are not counted, judged by name: no disk in the frame.
            if old.is_absolute() && !old.ancestors().any(|a| a.file_name().is_some_and(|n| crate::archive::is_archive(&n.to_string_lossy()))) {
                crate::jump::note(&mut self.ud.visits, old, crate::jump::now());
            }
            self.ud.push_recent(old);
            self.ud_dirty = true;
            self.refresh_recents();
        }
    }

    /// Open `path` in the editor chosen in Options, at `line` when one is known.
    pub fn open_in_editor(&mut self, path: &Path, line: Option<u32>) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
        match crate::editor::open_at(&self.cfg.editor, path, line) {
            Ok(label) => self.toast(format!("Opened {} in {label}", line.map_or(name.clone(), |l| format!("{name}:{l}")))),
            Err(e) => self.toast(e),
        }
    }

    /// Show `path` selected in its folder with the preview open at `line`, marked.
    pub fn show_at_line(&mut self, path: PathBuf, line: u32) {
        let id = self.active;
        if let Some(p) = self.panels.get_mut(&id) {
            let ins = p.inspector.get_or_insert_with(Inspector::default);
            ins.git = None;
            ins.goto = Some((path.clone(), line, true));
            p.line_hint = Some((path.clone(), line));
        }
        self.actions.push(Action::Navigate { panel: id, path });
    }

    pub fn navigate(&mut self, panel: u64, path: PathBuf) {
        if let Some(dev) = tab::drive_dev(&path) {
            let dev = dev.to_string();
            self.drive_op(crate::drives::Op::Mount, &dev, Some((panel, false)));
            return;
        }
        let s = App::nav_view(&self.ud, &path);
        let Some(p) = self.panels.get_mut(&panel) else { return };
        let old = p.path();
        p.tab_mut().navigate(path, s);
        if let Some(i) = p.inspector.as_mut() {
            i.invalidate();
        }
        self.track_leave(&old);
    }

    pub fn focus_panel(&mut self, id: u64) {
        if id != self.active && self.panels.contains_key(&id) {
            self.prev_active = self.active;
            self.active = id;
        }
    }

    /// Save what panel `id`'s preview is editing; a file changed on disk since asks first.
    pub fn save_edit(&mut self, id: u64, force: bool) -> bool {
        let Some(ed) = self.panels.get_mut(&id).and_then(|p| p.inspector.as_mut()).and_then(|i| i.edit.as_mut()) else { return false };
        let name = ed.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match ed.save(force) {
            Ok(trashed) => {
                if trashed {
                    self.undo.push(UndoEntry { label: format!("Edit {name}"), kind: UndoKind::Replaced(vec![ed.target.clone()]) });
                }
                self.toast(format!("Saved {name}"));
                true
            }
            Err(crate::quickedit::SaveError::Changed) => {
                let reload: Box<dyn FnOnce(&mut App)> = Box::new(move |app| {
                    if let Some(i) = app.panels.get_mut(&id).and_then(|p| p.inspector.as_mut()) {
                        i.edit = i.edit.take().and_then(|ed| crate::quickedit::Edit::open(&ed.path).ok().map(Box::new));
                    }
                });
                let choices = vec![("Save over it".to_string(), true, Some(Box::new(move |app: &mut App| {
                    app.save_edit(id, true);
                }) as Box<dyn FnOnce(&mut App)>)), ("Load it again".to_string(), true, Some(reload))];
                self.popup = Popup::Choose { msg: format!("“{name}” changed since you started editing it"), sub: "Save over the newer file, or load it again and lose your changes.".into(), choices };
                false
            }
            Err(crate::quickedit::SaveError::Failed(e)) => {
                self.toast(format!("Couldn't save {name}: {e}"));
                false
            }
        }
    }

    /// Stop editing in panel `id`'s preview, and close the preview when `close`; changes
    /// not saved are asked about first.
    pub fn leave_edit(&mut self, id: u64, close: bool) {
        let Some(i) = self.panels.get_mut(&id).and_then(|p| p.inspector.as_mut()) else { return };
        let Some(ed) = i.edit.as_ref() else { return };
        let done = move |app: &mut App| {
            if let Some(p) = app.panels.get_mut(&id) {
                if close {
                    p.inspector = None;
                } else if let Some(i) = p.inspector.as_mut() {
                    i.edit = None;
                    i.find = None;
                    i.keys = true;
                    i.invalidate();
                }
            }
        };
        if !ed.dirty() {
            return done(self);
        }
        let name = ed.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let choices: Vec<(String, bool, Option<Box<dyn FnOnce(&mut App)>>)> = vec![
            ("Save".to_string(), false, Some(Box::new(move |app: &mut App| {
                if app.save_edit(id, false) {
                    done(app);
                }
            }))),
            ("Don't save".to_string(), true, Some(Box::new(done))),
        ];
        self.popup = Popup::Choose { msg: format!("Save the changes to “{name}”?"), sub: "They are lost if you don't.".into(), choices };
    }

    /// Ask about edits not saved before the window closes; false when there are none.
    pub fn ask_unsaved(&mut self) -> bool {
        let dirty: Vec<(u64, String)> = self.panels.iter().filter_map(|(id, p)| p.inspector.as_ref()?.edit.as_ref().filter(|e| e.dirty()).map(|e| (*id, e.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()))).collect();
        let Some((_, name)) = dirty.first().cloned() else { return false };
        let ids: Vec<u64> = dirty.iter().map(|d| d.0).collect();
        let choices: Vec<(String, bool, Option<Box<dyn FnOnce(&mut App)>>)> = vec![
            ("Save and quit".to_string(), false, Some(Box::new(move |app: &mut App| {
                if ids.iter().all(|&id| app.save_edit(id, false)) {
                    app.quit = true;
                }
            }))),
            ("Quit without saving".to_string(), true, Some(Box::new(|app: &mut App| app.quit = true))),
        ];
        let msg = if dirty.len() == 1 { format!("“{name}” has changes not saved") } else { format!("{} files have changes not saved", dirty.len()) };
        self.popup = Popup::Choose { msg, sub: "They are lost if you quit without saving.".into(), choices };
        true
    }

    pub fn close_tab(&mut self, panel: u64, idx: usize) {
        let Some(p) = self.panels.get_mut(&panel) else { return };
        if idx >= p.tabs.len() {
            return;
        }
        let t = p.tabs.remove(idx);
        if self.cfg.animations && !p.tabs.is_empty() {
            self.tab_ghosts.push((panel, idx, t.shown_tab_w, std::time::Instant::now()));
        }
        self.closed_tabs.push(TabSnap { path: t.path.clone(), view: t.settings.clone(), ..Default::default() });
        if self.closed_tabs.len() > 50 {
            self.closed_tabs.remove(0);
        }
        if p.tabs.is_empty() {
            let mut leaves = vec![];
            self.tree.leaves(&mut leaves);
            if leaves.len() > 1 {
                self.close_panel(panel);
            } else {
                let home = fsx::home();
                let nt = self.make_tab(home);
                let p = self.panels.get_mut(&panel).unwrap();
                p.tabs.push(nt);
                p.active = 0;
            }
            return;
        }
        if p.active > idx || p.active >= p.tabs.len() {
            p.active = p.active.saturating_sub(1);
        }
    }

    pub fn close_panel(&mut self, id: u64) {
        let mut leaves = vec![];
        self.tree.leaves(&mut leaves);
        if leaves.len() <= 1 {
            return;
        }
        if let Some(p) = self.panels.remove(&id) {
            for t in p.tabs {
                self.closed_tabs.push(TabSnap { path: t.path.clone(), view: t.settings.clone(), ..Default::default() });
            }
        }
        self.tree.remove(id);
        if self.active == id {
            self.active = if self.panels.contains_key(&self.prev_active) { self.prev_active } else { self.tree.first_leaf() };
        }
    }

    // ---------------------------------------------------------------- file ops

    pub fn paste(&mut self) {
        // Paste into the single selected folder, else the current folder.
        let t = self.tab();
        let sel = t.target_entries();
        let dest = if t.selected.len() == 1 && sel.len() == 1 && sel[0].is_dir { sel[0].path.clone() } else { t.path.clone() };
        if tab::is_place(&dest) {
            return;
        }
        let (paths, cut) = match crate::clipboard::get_files() {
            Some(x) => x,
            // Something newer than any files: a picture or text, written as a file.
            None if crate::clipboard::available() => {
                let (panel, tab) = (self.active, self.tab().id);
                let p = crate::clipfile::start(panel, tab, dest, &self.ctx);
                self.clip_files.push(p);
                return;
            }
            None => return self.paste_own(dest),
        };
        self.paste_files(paths, cut, dest);
    }

    /// The files this app copied, when the system clipboard has nothing of its own.
    fn paste_own(&mut self, dest: PathBuf) {
        match &self.clipboard {
            Some((p, c)) => {
                let (paths, cut) = (p.clone(), *c);
                self.paste_files(paths, cut, dest);
            }
            None => self.toast("Nothing to paste"),
        }
    }

    fn paste_files(&mut self, paths: Vec<PathBuf>, cut: bool, dest: PathBuf) {
        if cut {
            self.clipboard = None;
        }
        crate::pastes::pasted(&mut self.pastes, &paths, cut);
        self.begin_transfer(paths, dest, cut);
    }

    /// Start a copy/move, asking about conflicts first.
    pub fn begin_transfer(&mut self, srcs: Vec<PathBuf>, dest: PathBuf, mv: bool) {
        // Into an archive: a job that rewrites it.
        if crate::archive::split(&dest).is_some() && !dest.is_dir() {
            crate::archwrite::add(self, srcs, &dest, mv);
            return;
        }
        // Out of an archive: extracted (always a copy; the archive stays as it is).
        if srcs.iter().any(|s| !s.exists() && crate::archive::split(s).is_some()) {
            for (archive, members) in crate::archive::group(&srcs) {
                self.actions.push(Action::StartJob(OpKind::Extract { archive, members, dest: dest.clone() }));
            }
            return;
        }
        let conflicts: Vec<ConflictItem> = srcs
            .iter()
            .filter_map(|s| {
                let name = s.file_name()?;
                let target = dest.join(name);
                if target != *s && std::fs::symlink_metadata(&target).is_ok() {
                    Some(ConflictItem { src: s.clone(), dst: target })
                } else {
                    None
                }
            })
            .collect();
        // Behind other transfers it waits its turn: a name taken is asked inside the job when
        // it comes to it, not now, and anything that lands there meanwhile is asked too.
        let behind = !conflicts.is_empty() && self.jobs.iter().any(|j| j.finished_at.is_none() && matches!(j.kind, OpKind::Copy { .. } | OpKind::Move { .. } | OpKind::Extract { .. } | OpKind::Sync { .. } | OpKind::Compress { .. } | OpKind::ArchiveEdit { .. } | OpKind::Place { .. }) && !j.progress.lock().is_ok_and(|p| p.finished));
        if conflicts.is_empty() || behind {
            let decide: HashMap<PathBuf, ops::Conflict> = if behind { srcs.iter().map(|s| (s.clone(), ops::Conflict::Ask)).collect() } else { HashMap::new() };
            let kind = if mv { OpKind::Move { srcs, dest, decide } } else { OpKind::Copy { srcs, dest, decide } };
            self.actions.push(Action::StartJob(kind));
        } else {
            self.popup = Popup::conflict(srcs, dest, mv, conflicts);
        }
    }

    /// Every tab that shows what changed reads it again, front or not, and the folders'
    /// sizes are counted again.
    pub(crate) fn show_changed(&mut self, c: &ops::Changed) {
        if c.is_empty() {
            return;
        }
        for d in c.dirs() {
            self.stats.invalidate(d);
        }
        for t in self.panels.values_mut().flat_map(|p| p.tabs.iter_mut()) {
            t.changed(c);
        }
    }

    pub fn start_job(&mut self, kind: OpKind) {
        // A place (Computer, GitHub) is a relative path: written to, it would land in
        // whatever folder Maia was started from.
        if let OpKind::Copy { dest, .. } | OpKind::Move { dest, .. } | OpKind::Extract { dest, .. } = &kind {
            if !dest.is_absolute() {
                return;
            }
        }
        let id = self.alloc_id();
        self.jobs.push(ops::spawn_in(id, kind, self.ctx.clone(), &self.queue, self.cfg.check_copies));
    }

    fn restore_from_trash(&mut self) {
        let sel = self.tab().targets();
        let wanted: Vec<crate::trashcan::Item> = crate::trashcan::list().into_iter().filter(|it| sel.contains(&it.file)).collect();
        if wanted.is_empty() {
            self.toast("Select items inside the trash to restore");
            return;
        }
        // Those whose place is taken again are asked about; the rest go back now.
        let (taken, free): (Vec<_>, Vec<_>) = wanted.into_iter().partition(|it| std::fs::symlink_metadata(&it.original).is_ok());
        self.restore(free, false);
        let Some(first) = taken.first() else { return };
        let msg = match taken.len() {
            1 => format!("“{}” is back where it came from already", first.original.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
            n => format!("{n} items are back where they came from already"),
        };
        let both = taken.clone();
        let choices: Vec<(String, bool, Option<Box<dyn FnOnce(&mut App)>>)> = vec![
            ("Keep both".to_string(), false, Some(Box::new(move |app: &mut App| app.restore(both, false)))),
            ("Replace".to_string(), false, Some(Box::new(move |app: &mut App| app.restore(taken, true)))),
        ];
        self.popup = Popup::Choose { msg, sub: "Keep both (the one from the trash under a new name), or put the one there in the trash in its place.".into(), choices };
    }

    /// Put trashed items back: where their place is taken, beside it under a free name,
    /// or (`replace`) in its place, what is there going to the trash.
    fn restore(&mut self, items: Vec<crate::trashcan::Item>, replace: bool) {
        if items.is_empty() {
            return;
        }
        let n = items.len();
        let mut back = vec![];
        let errs: Vec<String> = items
            .iter()
            .filter_map(|it| {
                let taken = std::fs::symlink_metadata(&it.original).is_ok();
                let to = match (taken, it.original.parent(), it.original.file_name()) {
                    (true, Some(dir), Some(name)) if !replace => fsx::unique_dest(dir, &name.to_string_lossy()),
                    _ => it.original.clone(),
                };
                if taken && replace {
                    if let Err(e) = crate::trashcan::delete(&it.original) {
                        return Some(e.to_string());
                    }
                }
                match crate::trashcan::restore_as(it, &to) {
                    Ok(()) => {
                        back.push((it.file.clone(), to));
                        None
                    }
                    Err(e) => Some(e.to_string()),
                }
            })
            .collect();
        // Every tab showing where they come back to, not only this one.
        self.show_changed(&ops::Changed::moved(&back));
        if errs.is_empty() {
            self.toast(format!("Restored {n} item{}", if n == 1 { "" } else { "s" }));
        } else {
            self.toast(format!("Restore failed: {}", errs.join("; ")));
        }
        self.tab().reload();
    }

    /// Take back the latest change; false when some of it couldn't be. What was taken
    /// back goes to Redo. A move across disks goes back through a job with its card.
    pub fn undo_last(&mut self) -> bool {
        if !self.job_steps.is_empty() {
            self.toast("Undo waits for the move under way");
            return false;
        }
        if crate::tags::busy() {
            self.toast("Undo waits for the tags being changed");
            return false;
        }
        // When the change was made: a version trashed after it (by an Undo since) is not
        // the one it replaced.
        let made = self.undo.len().checked_sub(1).and_then(|i| self.undo.when(i));
        let Some(u) = self.undo.pop() else {
            self.toast("Nothing to undo");
            return true;
        };
        let here = |p: &Path| std::fs::symlink_metadata(p).is_ok();
        let mut errs = 0;
        let mut moved_back = ops::Changed::default();
        // What Redo would do again: the part of the change that was taken back.
        let again = match &u.kind {
            UndoKind::Tagged(ch) => {
                self.toast(format!("Undid “{}”", u.label));
                crate::tags::start(self, crate::tags::Job::Restore(ch.clone(), true), crate::tags::Record::Undid(u.label.clone()));
                return true;
            }
            UndoKind::Rename(pairs) => {
                let plan: Vec<(PathBuf, String)> = pairs
                    .iter()
                    .map(|(from, to)| (to.clone(), from.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()))
                    .collect();
                errs = crate::rename::apply(&plan).len();
                let back: Vec<(PathBuf, PathBuf)> = pairs.iter().map(|(from, to)| (to.clone(), from.clone())).collect();
                moved_back = ops::Changed::moved(&back);
                self.follow_pins(&back);
                UndoKind::Rename(pairs.iter().filter(|(from, to)| here(from) && !here(to)).cloned().collect())
            }
            UndoKind::Copied(made, replaced) => {
                // The copies over something: theirs back from the trash in their place.
                let swapped = crate::redo::swap_each(replaced, &mut errs);
                let gone = crate::redo::trash_each(made.iter().filter(|p| !replaced.contains(p)), &mut errs);
                UndoKind::Copied(gone.into_iter().chain(swapped.iter().cloned()).collect(), swapped)
            }
            UndoKind::Synced(made, replaced, removed) => {
                let swapped = crate::redo::swap_each(replaced, &mut errs);
                let gone = crate::redo::trash_each(made.iter().filter(|p| !replaced.contains(p)), &mut errs);
                // What mirroring removed, back where it was.
                let back = crate::redo::untrash_each(removed, &mut errs);
                UndoKind::Synced(gone.into_iter().chain(swapped.iter().cloned()).collect(), swapped, back)
            }
            UndoKind::Move(pairs) | UndoKind::MovedOver(pairs, _) if crate::redo::crosses(pairs.iter().map(|(s, d)| (d.as_path(), s.as_path()))) => {
                let over = matches!(u.kind, UndoKind::MovedOver(..));
                let back = match &u.kind {
                    UndoKind::MovedOver(_, r) => r.clone(),
                    _ => vec![],
                };
                let pairs: Vec<(PathBuf, PathBuf)> = pairs.iter().map(|(s, d)| (d.clone(), s.clone())).collect();
                let label = crate::redo::place_label(&pairs, true);
                let id = self.alloc_id();
                self.job_steps.insert(id, crate::redo::Step::Undid { label: u.label.clone(), over });
                self.jobs.push(ops::spawn_in(id, OpKind::Place { pairs, over: vec![], back, label }, self.ctx.clone(), &self.queue, self.cfg.check_copies));
                return true;
            }
            UndoKind::MovedOver(pairs, replaced) => {
                let mut back = vec![];
                for (src, dst) in pairs {
                    if here(src) || crate::fsx::rename_new(dst, src).is_err() {
                        errs += 1;
                    } else {
                        back.push((dst.clone(), src.clone()));
                    }
                }
                // What was there before comes back from the trash, now its place is free.
                let restored = crate::redo::untrash_each(replaced, &mut errs);
                moved_back = ops::Changed::moved(&back);
                moved_back.arrived.extend(replaced.iter().filter_map(|r| r.parent().map(Path::to_path_buf)));
                UndoKind::MovedOver(back.into_iter().map(|(d, s)| (s, d)).collect(), restored)
            }
            UndoKind::Move(pairs) => {
                let mut back = vec![];
                for (src, dst) in pairs {
                    if here(src) || std::fs::rename(dst, src).is_err() {
                        errs += 1;
                    } else {
                        back.push((dst.clone(), src.clone()));
                    }
                }
                moved_back = ops::Changed::moved(&back);
                UndoKind::Move(back.into_iter().map(|(d, s)| (s, d)).collect())
            }
            UndoKind::Created(paths) => {
                let gone = crate::redo::trash_each(paths.iter(), &mut errs);
                for p in paths {
                    moved_back.gone.push(p.clone());
                    if let Some(d) = p.parent().filter(|d| !moved_back.left.iter().any(|x| x == d)) {
                        moved_back.left.push(d.to_path_buf());
                    }
                }
                UndoKind::Created(gone)
            }
            UndoKind::Trashed(orig) => {
                // Restore the most recent trash entry for each original path.
                let mut best: HashMap<PathBuf, crate::trashcan::Item> = HashMap::new();
                for it in crate::trashcan::list() {
                    if orig.contains(&it.original) && best.get(&it.original).map(|b| b.deleted < it.deleted).unwrap_or(true) {
                        best.insert(it.original.clone(), it);
                    }
                }
                let back: Vec<(PathBuf, PathBuf)> = best.values().filter(|it| crate::trashcan::restore(it).is_ok()).map(|it| (it.file.clone(), it.original.clone())).collect();
                errs += best.len() - back.len();
                moved_back = ops::Changed::moved(&back);
                UndoKind::Trashed(orig.iter().filter(|p| back.iter().any(|b| b.1 == **p)).cloned().collect())
            }
            UndoKind::Replaced(paths) => UndoKind::Replaced(crate::redo::swap_each_by(paths, made, &mut errs)),
            UndoKind::Sorted(pairs, made) => {
                let mut back = vec![];
                for (src, dst) in pairs {
                    if here(src) || std::fs::rename(dst, src).is_err() {
                        errs += 1;
                    } else {
                        back.push((dst.clone(), src.clone()));
                    }
                }
                moved_back = ops::Changed::moved(&back);
                // Innermost first; one something else was put into stays.
                let mut removed = vec![];
                for d in made.iter().rev() {
                    if std::fs::remove_dir(d).is_ok() {
                        moved_back.gone.push(d.clone());
                        removed.push(d.clone());
                    }
                }
                removed.reverse();
                UndoKind::Sorted(back.into_iter().map(|(d, s)| (s, d)).collect(), removed)
            }
        };
        if again.count() > 0 {
            self.undo.push_undone(UndoEntry { label: u.label.clone(), kind: again });
        }
        if moved_back.is_empty() {
            for p in self.panels.values_mut() {
                p.tab_mut().reload();
            }
        } else {
            self.show_changed(&moved_back);
        }
        if errs > 0 {
            self.toast(format!("Undo “{}” partly failed", u.label));
        } else {
            self.toast(format!("Undid “{}”", u.label));
        }
        errs == 0
    }

    // ---------------------------------------------------------------- actions

    fn process_actions(&mut self, ctx: &egui::Context) {
        let mut guard = 0;
        while !self.actions.is_empty() && guard < 8 {
            guard += 1;
            let actions = std::mem::take(&mut self.actions);
            for a in actions {
                self.apply(a, ctx);
                self.heal();
            }
        }
    }

    fn apply(&mut self, a: Action, ctx: &egui::Context) {
        match a {
            Action::Cmd(c) => self.run(c),
            Action::Navigate { panel, path } => {
                self.focus_panel(panel);
                self.navigate(panel, path);
            }
            Action::NewTab { panel, path, .. } | Action::Split { panel, path, .. } if tab::drive_dev(&path).is_some() => {
                let dev = tab::drive_dev(&path).unwrap_or_default().to_string();
                self.drive_op(crate::drives::Op::Mount, &dev, Some((panel, true)));
            }
            Action::NewTab { panel, path, select, focus } => {
                let mut t = self.make_tab(path.clone());
                match select {
                    // A folder to select in this one: highlighted, not opened.
                    Some(s) if s.is_dir() && s.parent() == Some(path.as_path()) => {
                        t.pending_restore = Some(tab::HistState { path, cursor: Some(s.clone()), selected: vec![s], scroll: 0.0 });
                        t.scroll_to_cursor = true;
                    }
                    Some(s) => t.navigate(s, None),
                    None => {}
                }
                if let Some(p) = self.panels.get_mut(&panel) {
                    let at = p.active + 1;
                    let at = at.min(p.tabs.len());
                    p.tabs.insert(at, t);
                    if focus {
                        p.active = at;
                    }
                }
            }
            Action::Split { panel, path, side, select } => {
                let mut t = self.make_tab(path);
                if let Some(s) = select {
                    t.navigate(s, None);
                }
                let id = self.alloc_id();
                if self.tree.split(panel, id, side) {
                    self.panels.insert(id, Panel::new(id, t));
                    self.focus_panel(id);
                }
            }
            Action::Activate { panel, idx, mods } => self.activate(panel, idx, mods),
            Action::SourceView(k, source) => {
                self.cfg.source_views.set(k, source);
                self.cfg_dirty = true;
            }
            Action::OpenFile(p) => {
                if let Err(e) = crate::mime::open_default(&p) {
                    self.toast(format!("Couldn't open: {e}"));
                }
                if self.cfg.track_recents() {
                    self.ud.push_recent(&p);
                    self.ud_dirty = true;
                    self.refresh_recents();
                }
            }
            Action::CtxMenu { panel, pos, background } => {
                self.focus_panel(panel);
                self.popup = crate::ctxmenu::build(self, panel, pos, background);
            }
            Action::Drop { paths, dest, copy, link } => {
                if paths.iter().any(|p| *p == dest) {
                    return;
                }
                if std::mem::take(&mut self.drop_right) && !link {
                    let pos = ctx.pointer_latest_pos().unwrap_or_default();
                    self.popup = Popup::DropMenu { paths, dest, pos };
                    return;
                }
                if link {
                    for p in &paths {
                        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        let _ = std::os::unix::fs::symlink(p, fsx::unique_dest(&dest, &name));
                    }
                    return;
                }
                // Files from other apps are copied (Shift moves them): whatever they
                // came from keeps its own.
                let copy = copy || (self.ext_drop == Some(false) && !ctx.input(|i| i.modifiers.shift));
                // Default: move within a filesystem, copy across.
                let same_fs = {
                    use std::os::unix::fs::MetadataExt;
                    let d = std::fs::metadata(&dest).map(|m| m.dev()).ok();
                    paths.iter().all(|p| std::fs::metadata(p).map(|m| Some(m.dev()) == d).unwrap_or(false))
                };
                let mv = !copy && (same_fs || ctx.input(|i| i.modifiers.shift));
                if mv && paths.iter().all(|p| p.parent() == Some(dest.as_path())) {
                    return;
                }
                self.begin_transfer(paths, dest, mv);
            }
            Action::RunWith { exe, args } => {
                let msg = format!("Run “{}” with {} item{}?", exe.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), args.len(), if args.len() == 1 { "" } else { "s" });
                self.popup = Popup::confirm(msg, "Run", Box::new(move |app: &mut App| {
                    if let Err(e) = crate::mime::run_with(&exe, &args) {
                        app.toast(format!("Couldn't run: {e}"));
                    }
                }));
            }
            Action::FocusPanel(id) => self.focus_panel(id),
            Action::ActivateTab { panel, idx } => {
                self.focus_panel(panel);
                if let Some(p) = self.panels.get_mut(&panel) {
                    p.active = idx.min(p.tabs.len() - 1);
                }
            }
            Action::CloseTab { panel, idx } => self.close_tab(panel, idx),
            Action::MoveTab { from, idx, to, to_idx, side, copy } => self.move_tab(from, idx, to, to_idx, side, copy),
            Action::BookmarkAdd(p) => {
                if !self.ud.bookmarks.contains(&p) {
                    self.ud.bookmarks.push(p);
                    self.ud_dirty = true;
                }
            }
            Action::BookmarkRemove(p) => {
                self.ud.bookmarks.retain(|b| *b != p);
                self.ud_dirty = true;
            }
            Action::BookmarkMove { from, to } => {
                if from < self.ud.bookmarks.len() {
                    let b = self.ud.bookmarks.remove(from);
                    let to = to.min(self.ud.bookmarks.len());
                    self.ud.bookmarks.insert(to, b);
                    self.ud_dirty = true;
                }
            }
            Action::CommitRename(panel) => self.commit_rename(panel),
            Action::CancelRename(panel) => {
                if let Some(p) = self.panels.get_mut(&panel) {
                    p.tab_mut().rename = None;
                }
            }
            Action::Toast(s) => self.toast(s),
            Action::Open(p) => self.popup = p,
            Action::OpenSmart { panel, id, new_tab, focus } => self.open_smart(panel, id, new_tab, focus),
            Action::StartJob(k) => self.start_job(k),
            Action::ExternalDrop { paths, dest } => self.begin_transfer(paths, dest, false),
            Action::Run(f) => f(self),
        }
    }

    fn activate(&mut self, panel: u64, idx: u32, mods: Modifiers) {
        if crate::picker::activate(self, panel, idx) {
            return;
        }
        let Some(p) = self.panels.get_mut(&panel) else { return };
        let t = p.tab_mut();
        let Some(e) = t.entries.get(idx as usize).cloned() else { return };
        // An unmounted drive: mount it, then open it.
        if let Some(dev) = tab::drive_dev(&e.path) {
            let dev = dev.to_string();
            self.drive_op(crate::drives::Op::Mount, &dev, Some((panel, mods.ctrl || mods.command)));
            return;
        }
        let in_archive = t.archive.is_some();
        if mods.alt && !in_archive {
            self.popup = Popup::properties(vec![e.path.clone()]);
            return;
        }
        // Archives open like folders; what is inside one is extracted to be opened.
        let archive = !e.is_dir && !in_archive && crate::archive::is_archive(&e.name) && crate::archive::available();
        if in_archive && !e.is_dir {
            self.open_from_archive(e.path);
        } else if e.is_dir || archive {
            if mods.ctrl || mods.command {
                self.actions.push(Action::NewTab { panel, path: e.path, select: None, focus: false });
            } else {
                self.navigate(panel, e.path);
            }
        } else if self.cfg.picture_viewer && crate::viewer::is_picture(e.ext()) && self.active == panel && crate::viewer::open_at(self, Some(e.path.clone())) {
            // Shown in Maia's own viewer, as Options asks.
        } else if e.ext() == "desktop" || !e.is_executable() {
            self.actions.push(Action::OpenFile(e.path));
        } else {
            // Plain executable: ask how to run it.
            let path = e.path.clone();
            self.popup = Popup::run_exec(path);
        }
    }

    /// Extract a file from an archive to the cache, then open it like any file.
    pub fn open_from_archive(&mut self, path: PathBuf) {
        let Some((archive, inner)) = crate::archive::split(&path) else { return };
        let (notes, asks, ctx) = (self.notes.clone(), self.pw_asks.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let r = crate::archive::extract_for_open(&archive, &inner).and_then(|f| crate::mime::open_default(&f));
            match r {
                Err(e) if crate::archive::wants_password(&e) => asks.lock().unwrap_or_else(|e| e.into_inner()).push((archive, e == crate::archive::WRONG_PASSWORD, path)),
                Err(e) => notes.lock().unwrap_or_else(|e| e.into_inner()).push(format!("Couldn't open {}: {e}", inner.display())),
                Ok(()) => {}
            }
            ctx.request_repaint();
        });
    }

    /// Ask for an archive's password when opening a file in it, or showing a folder in it,
    /// was refused for want of one.
    fn pump_passwords(&mut self) {
        use crate::popups::{PasswordState, Retry};
        if !matches!(self.popup, Popup::None) {
            return;
        }
        if let Some((archive, wrong, path)) = self.pw_asks.lock().unwrap_or_else(|e| e.into_inner()).pop() {
            self.popup = Popup::ArchivePassword(PasswordState { archive, retry: Some(Retry::Open(path)), text: String::new(), wrong });
            return;
        }
        let id = self.active;
        let Some(t) = self.panels.get(&id).map(|p| p.tab()) else { return };
        match (&t.error, &t.archive) {
            (Some(e), Some(archive)) if crate::archive::wants_password(e) => {
                if self.pw_asked.as_ref() != Some(&t.path) {
                    self.pw_asked = Some(t.path.clone());
                    let (archive, wrong) = (archive.clone(), e == crate::archive::WRONG_PASSWORD);
                    self.popup = Popup::ArchivePassword(PasswordState { archive, retry: Some(Retry::Reload(id)), text: String::new(), wrong });
                }
            }
            (None, _) if !t.is_loading() || t.archive.is_none() => self.pw_asked = None,
            _ => {}
        }
    }

    fn move_tab(&mut self, from: u64, idx: usize, to: u64, to_idx: Option<usize>, side: Option<Side>, copy: bool) {
        let Some(src) = self.panels.get(&from) else { return };
        if idx >= src.tabs.len() {
            return;
        }
        let path = src.tabs[idx].path.clone();
        let settings = src.tabs[idx].settings.clone();
        let pinned = src.tabs[idx].pinned;
        // Moving a panel's only tab onto its own side is meaningless.
        if from == to && side.is_some() && src.tabs.len() == 1 && !copy {
            return;
        }
        let tid = self.alloc_id();
        let mut t = Tab::new(tid, path, settings, self.ctx.clone());
        t.saved_scope = None;
        t.pinned = pinned;
        let mut removed = None;
        if !copy {
            let Some(p) = self.panels.get_mut(&from) else { return };
            removed = Some(p.tabs.remove(idx));
            t.selected.clear();
            if p.active >= p.tabs.len() && !p.tabs.is_empty() {
                p.active = p.tabs.len() - 1;
            }
        }
        match side {
            Some(s) => {
                let id = self.alloc_id();
                if self.tree.split(to, id, s) {
                    self.panels.insert(id, Panel::new(id, t));
                    self.focus_panel(id);
                } else if let (Some(old), Some(p)) = (removed, self.panels.get_mut(&from)) {
                    // Couldn't split there: put the tab back instead of losing it.
                    let at = idx.min(p.tabs.len());
                    p.tabs.insert(at, old);
                    p.active = at;
                }
            }
            None => {
                if let Some(p) = self.panels.get_mut(&to) {
                    let at = to_idx.unwrap_or(p.tabs.len()).min(p.tabs.len());
                    p.tabs.insert(at, t);
                    p.active = at;
                }
                self.focus_panel(to);
            }
        }
        // Empty source panel closes.
        if self.panels.get(&from).map(|p| p.tabs.is_empty()).unwrap_or(false) {
            self.panels.remove(&from);
            self.tree.remove(from);
            if self.active == from {
                self.active = self.tree.first_leaf();
            }
        }
    }

    /// Batch Rename's plan, carried out on a thread; undone in one step.
    pub fn rename_batch(&mut self, panel: u64, plan: Vec<(PathBuf, String)>) {
        let ctx = self.ctx.clone();
        self.share(&ctx, move || {
            let errors = crate::rename::apply(&plan);
            // What was renamed is what is now at its new name and gone from its old one.
            let pairs: Vec<(PathBuf, PathBuf)> = plan.iter().map(|(p, n)| (p.clone(), p.with_file_name(n))).filter(|(a, b)| std::fs::symlink_metadata(b).is_ok() && std::fs::symlink_metadata(a).is_err()).collect();
            let toast = if errors.is_empty() { format!("Renamed {} {}", pairs.len(), if pairs.len() == 1 { "item" } else { "items" }) } else { errors.join("; ") };
            ShareDone { copy: None, open: vec![], toast, rescan: None, renamed: Some((panel, pairs)) }
        });
    }

    fn commit_rename(&mut self, panel: u64) {
        if crate::archwrite::commit_rename(self, panel) {
            return;
        }
        let Some(p) = self.panels.get_mut(&panel) else { return };
        let t = p.tab_mut();
        let Some(r) = t.rename.as_mut() else { return };
        let bad = r.compute_conflicts();
        if !bad.is_empty() {
            r.error = Some(if r.is_batch() { format!("{} name(s) are invalid or collide", bad.len()) } else { r.reason().unwrap_or_else(|| "That name can't be used".into()) });
            r.shake = self.ctx.input(|i| i.time);
            self.ctx.request_repaint();
            return;
        }
        let plan = r.plan();
        let batch = r.is_batch();
        t.rename = None;
        if plan.is_empty() {
            return;
        }
        let new_paths: Vec<PathBuf> = plan.iter().map(|(p, n)| p.with_file_name(n)).collect();
        let errors = crate::rename::apply(&plan);
        t.pending_restore = Some(tab::HistState { path: t.path.clone(), cursor: new_paths.first().cloned(), selected: new_paths.clone(), scroll: t.current_scroll });
        t.reload();
        let pairs: Vec<(PathBuf, PathBuf)> = plan.iter().map(|(p, _)| p.clone()).zip(new_paths).collect();
        // Undo takes back only what was renamed: a name refused stays as it was.
        let done: Vec<(PathBuf, PathBuf)> = pairs.iter().filter(|(a, b)| std::fs::symlink_metadata(b).is_ok() && std::fs::symlink_metadata(a).is_err()).cloned().collect();
        let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let label = match done.as_slice() {
            [(from, to)] if !batch => format!("Rename {} to {}", name(from), name(to)),
            _ => format!("Rename {} items", done.len()),
        };
        if !done.is_empty() {
            self.follow_pins(&done);
            self.undo.push(UndoEntry { label, kind: UndoKind::Rename(done.clone()) });
            // Other tabs on the same folder show the new names too.
            self.show_changed(&ops::Changed::moved(&done));
        }
        if errors.is_empty() {
            return;
        }
        // One rename refused for lack of permission: offer it as administrator.
        let tools = crate::drives::Tools::current().filter(|t| t.has("pkexec"));
        match (&pairs[..], tools) {
            ([(from, to)], Some(tools)) if errors[0].contains("os error 13") || errors[0].contains("os error 1)") => {
                let task = crate::admin::Task::Rename { from: from.clone(), to: to.clone() };
                let name = from.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.popup = Popup::confirm(format!("You don't have permission to rename “{name}”. Rename it as administrator?"), "Use administrator", Box::new(move |app: &mut App| {
                    app.actions.push(Action::StartJob(OpKind::Admin { task, tools }));
                }));
            }
            _ => self.toast(errors.join("; ")),
        }
    }
}

impl App {
    pub fn on_exit(&mut self) {
        if cfg!(test) {
            return;
        }
        // A picker's one folder is not how the project was left.
        if self.picker.is_none() {
            self.keep_workspace();
        }
        let ctx = self.ctx.clone();
        self.save_all(&ctx);
        self.undo_keep.flush(&self.undo);
    }
}
