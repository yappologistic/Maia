//! Floating UIs: GoTo, palette, options, menus, dialogs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{self, Align2, Color32, CornerRadius, Event, FontId, Id, Key, Modifiers, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

use crate::app::{Action, App};
use crate::commands::{self, ALL, Binding, Cat, Chord, Cmd};
use crate::config::{CharKeyAction, IgnoredFiles, InspectorLayout, MiddleClick, SaveScope, SavedView, RecentsStore, Startup, ThumbMode, NewTabShows};
use crate::ctxmenu::CtxState;
use crate::fsx;
use crate::fuzzy;
use crate::ops::{Conflict, OpKind};
use crate::rename::{Case, DateKind};
use crate::tab;
use crate::theme::{self, Glyph, Palette, ThemeName, paint_glyph};

pub struct ConflictItem {
    pub src: PathBuf,
    pub dst: PathBuf,
}

pub struct GoItem {
    pub label: String,
    pub sub: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub header: bool,
    /// Drives: used fraction for the fill bar.
    pub usage: Option<f32>,
}

pub struct GoToState {
    pub input: String,
    last: Option<String>,
    pub sel: usize,
    pub items: Vec<GoItem>,
    focus: bool,
    dir_cache: Option<(PathBuf, Vec<(String, bool)>)>,
    /// Folder listing being read in the background for path completion.
    dir_job: Option<(PathBuf, std::sync::mpsc::Receiver<Vec<(String, bool)>>)>,
    /// Sidebar snapshot generation the items were built from.
    snap_gen: u64,
    back: Vec<String>,
    fwd: Vec<String>,
    /// Entry to highlight after the item list is rebuilt (after Back / Up).
    highlight: Option<PathBuf>,
    /// Picking where Copy To… or Move To… sends the selection, not where to go.
    pub dest: Option<crate::copyto::Dest>,
}

pub struct Capture {
    pub cmd: Cmd,
    pub seq: Vec<Chord>,
    pub last: Instant,
}

pub struct PaletteState {
    pub query: String,
    pub sel: usize,
    pub capture: Option<Capture>,
    pub conflict: Option<(Cmd, Binding, Cmd)>,
    focus: bool,
    /// The category picked on the left; None lists them all.
    pub cat: Option<Cat>,
    scroll: bool,
    top: bool,
}

pub struct OptState {
    pub query: String,
    focus: bool,
    /// Keyboard-selected row (index among the rows on show).
    sel: usize,
    scroll_to_sel: bool,
    /// The section on show; until the first frame, the one shown last.
    pub section: Option<Section>,
    /// The arrows have been used: the selected row shows its ring.
    keyed: bool,
    search_id: Option<Id>,
    /// Text typed while the search had no focus: its cursor goes to the end.
    cursor_end: bool,
    /// Scroll the settings back to the top (another section, another search).
    top: bool,
}

pub struct PathDropState {
    pub panel: u64,
    pub dir: PathBuf,
    pub next: Option<PathBuf>,
    pub query: String,
    pub sel: usize,
    pub entries: Vec<PathBuf>,
    /// File details per entry, read once when the dropdown opens (not every frame).
    pub info: std::collections::HashMap<PathBuf, fsx::Entry>,
    pub anchor: Option<Pos2>,
    focus: bool,
}

pub struct ConflictState {
    pub srcs: Vec<PathBuf>,
    pub dest: PathBuf,
    pub mv: bool,
    pub items: Vec<ConflictItem>,
    pub idx: usize,
    pub decide: HashMap<PathBuf, Conflict>,
    pub apply_all: bool,
    /// The name being typed for the item at hand (Rename…).
    pub rename: Option<String>,
    focus_name: bool,
}

pub struct PropState {
    pub paths: Vec<PathBuf>,
    /// Permissions and owners being changed, before Apply.
    pub change: crate::perms::Change,
    /// Change everything inside the folders too.
    pub deep: bool,
    /// People and groups to pick from (read when first wanted).
    ids: Option<(Vec<(u32, String)>, Vec<(u32, String)>)>,
    /// The change Apply asked for, started once the dialog is drawn.
    apply: Option<OpKind>,
    /// File details read at most once a second (not on every frame).
    snap: Option<PropSnap>,
    /// A checksum to compare with, as pasted.
    pub expected: String,
    pub sum_job: Option<crate::checksum::Job>,
    pub sum: Option<(crate::checksum::Algo, Result<String, String>)>,
}

struct PropSnap {
    at: Instant,
    single: Option<(std::fs::Metadata, fsx::Entry)>,
    vol: Option<(u64, u64)>,
    link: Option<String>,
    /// Several items: (is a folder, size of a file).
    multi: Vec<(bool, u64)>,
    /// Every item's (mode, owner, group, is a folder), for the permissions.
    perms: Vec<(u32, u32, u32, bool)>,
}

impl PropState {
    fn refresh(&mut self) {
        let fresh = self.snap.as_ref().map(|s| s.at.elapsed() < Duration::from_secs(1) || self.paths.len() > 500).unwrap_or(false);
        if fresh {
            return;
        }
        let mut s = PropSnap { at: Instant::now(), single: None, vol: None, link: None, multi: vec![], perms: vec![] };
        if self.paths.len() == 1 {
            let p = &self.paths[0];
            if let (Ok(m), Some(e)) = (std::fs::symlink_metadata(p), fsx::Entry::from_path(p)) {
                s.link = e.is_link.then(|| std::fs::read_link(p).map(|t| t.display().to_string()).unwrap_or_default());
                s.single = Some((m, e));
                s.vol = fsx::disk_space(p);
            }
        } else {
            s.multi = self.paths.iter().map(|p| match std::fs::metadata(p) {
                Ok(m) if m.is_dir() => (true, 0),
                Ok(m) => (false, m.len()),
                Err(_) => (false, 0),
            }).collect();
        }
        use std::os::unix::fs::MetadataExt;
        s.perms = self.paths.iter().filter_map(|p| std::fs::symlink_metadata(p).ok()).map(|m| (m.mode() & 0o7777, m.uid(), m.gid(), m.is_dir())).collect();
        self.snap = Some(s);
    }
}

pub enum Popup {
    None,
    GoTo(GoToState),
    Palette(PaletteState),
    Ctx(CtxState),
    Options(OptState),
    Types { panel: u64, query: String },
    TabSearch { query: String, sel: usize },
    SavedView { panel: u64 },
    Zoom { panel: u64, anchor: Pos2 },
    Layouts { delete: bool },
    Prompt { title: String, text: String, on_ok: Option<Box<dyn FnOnce(&mut App, String)>> },
    Confirm { msg: String, ok: String, on_ok: Option<Box<dyn FnOnce(&mut App)>> },
    /// A question with a few answers, each a label, whether it can't be undone, and what
    /// it does; Cancel besides.
    Choose { msg: String, sub: String, choices: Vec<(String, bool, Option<Box<dyn FnOnce(&mut App)>>)> },
    Conflict(ConflictState),
    /// Sync To Other Panel: what it will do, worked out first.
    Sync(Box<crate::compare::SyncState>),
    Properties(PropState),
    /// Open With: the apps for the type, and a command typed instead.
    OpenWith { paths: Vec<PathBuf>, query: String, all: bool, command: String },
    /// Custom actions, made and changed: the one selected.
    Actions { sel: usize },
    RunExec { path: PathBuf },
    MainMenu,
    QuickMenu,
    PanelMenu { panel: u64, anchor: Pos2 },
    PathMenu { panel: u64, path: PathBuf, anchor: Pos2 },
    PathDropdown(PathDropState),
    History { panel: u64, anchor: Pos2 },
    RenameMenu { panel: u64, pos: Pos2 },
    FindReplace { panel: u64 },
    About,
    TabMenu { panel: u64, idx: usize, pos: Pos2 },
    TabsOptions { panel: u64, pos: Pos2 },
    GroupMenu { panel: u64, label: String, pos: Pos2 },
    /// A file in a commit of the history.
    GitFile { panel: u64, file: usize, pos: Pos2 },
    /// The branch chip's menu: the history, branches, stashes and work trees.
    Branches(BranchState),
    /// One stash: apply, pop, drop.
    Stash { panel: u64, root: PathBuf, stash: crate::gitwork::Stash, anchor: Pos2, sel: usize },
    /// One branch of the branch menu: switch to it (or, a remote one, check it out),
    /// compare it with the branch HEAD is on (`into`) or merge it in, rename, delete.
    Branch { root: PathBuf, branch: crate::gitwork::Branch, anchor: Pos2, sel: usize, panel: u64, into: String, remote: bool },
    /// A commit in the history.
    GitCommit(CommitMenu),
    /// A new branch or tag at a commit.
    NewRef(NewRef),
    /// Something that went wrong, said in full (git's own words).
    Notice { title: String, text: String },
    DropMenu { paths: Vec<PathBuf>, dest: PathBuf, pos: Pos2 },
    Connect(ConnectState),
    ArchivePassword(PasswordState),
    /// Compress: the archive's name, format, level, and a password if wanted.
    Compress(CompressState),
    /// The passphrase of an encrypted drive being opened.
    Unlock(UnlockState),
    Meaning(MeaningState),
    /// The theme picker: every theme as a small window, with its code colours.
    Themes,
    CodeSearch(Box<crate::devtools::CodeState>),
    Tasks(crate::devtools::TasksState),
    CleanBuilds(crate::devtools::CleanState),
    Duplicates(crate::dupes::State),
    BatchRename(crate::batchname::State),
    FormatDrive(Box<crate::formatdrive::State>),
    MusicTags(Box<crate::musictags::State>),
    Tidy(crate::tidy::State),
    Print(Box<crate::sendto::PrintState>),
    Projects(crate::projects::State),
    QuickOpen(Box<crate::quickopen::State>),
    UndoHistory(crate::undohist::State),
    Marks(crate::marks::State),
    Holders(Box<crate::holders::State>),
    SmartEdit(crate::smart::Edit),
    SpaceUsed(Box<crate::spaceuse::State>),
    /// Pictures full window.
    Viewer(Box<crate::viewer::State>),
    /// The welcome tour or What's New.
    Tour(Box<crate::tour::State>),
    /// The jobs under way, above the status bar's chip.
    Transfers(crate::transfers::State),
    Jump(Box<crate::jump::State>),
    /// Send To Phone: the code, the files, the time left.
    Phone(Box<crate::phone::State>),
    /// Paste from earlier copies.
    Pastes(crate::pastes::State),
    /// A folder's pictures by the day they were taken.
    Calendar(Box<crate::calendar::State>),
}

/// The branch menu, its lists read on a worker as it opens.
pub struct BranchState {
    pub panel: u64,
    pub root: PathBuf,
    /// The chip it opens above.
    pub anchor: Rect,
    pub refs: Option<crate::gitwork::Refs>,
    job: Option<crate::gitlog::Slot<crate::gitwork::Refs>>,
    pub sel: usize,
    keyed: bool,
    opened: Instant,
}

/// A commit's menu in the history, and what it acts on.
pub struct CommitMenu {
    pub root: PathBuf,
    pub hash: String,
    pub subject: String,
    /// A merge: picked and reverted against its first parent.
    pub merge: bool,
    /// HEAD is on it.
    pub head: bool,
    pub tags: Vec<String>,
    /// The branch HEAD is on ("HEAD" when on none).
    pub branch: String,
    pub pos: Pos2,
    /// The keyboard's item: none (`usize::MAX`) until the keys are used.
    pub sel: usize,
    /// The commit marked to compare with, and the panel whose history it is in.
    pub marked: Option<String>,
    pub panel: u64,
}

/// Create branch here or Create tag here: a name, and for a tag a message that makes it
/// an annotated one.
pub struct NewRef {
    pub root: PathBuf,
    pub at: String,
    pub subject: String,
    pub tag: bool,
    pub name: String,
    pub message: String,
    focus: bool,
}

/// The Search by Meaning window: a question, and the passages closest to it in meaning.
pub struct MeaningState {
    pub query: String,
    focus: bool,
    sel: usize,
    pub hits: Vec<crate::semantic::Hit>,
    /// The question the hits answer, and the search running for a newer one.
    asked: String,
    job: Option<std::sync::mpsc::Receiver<(String, Result<Vec<crate::semantic::Hit>, String>)>>,
    error: Option<String>,
    typed: Instant,
    /// Every name under the home folder, gathered while the search is open.
    names: Option<crate::namefind::Names>,
    /// Name matches: path and whether it is a folder; and the query and name count they
    /// were found for.
    pub(crate) named: Vec<(PathBuf, bool)>,
    named_for: (String, usize),
    /// Rows' file details, for their icons.
    entries: std::collections::HashMap<PathBuf, Option<fsx::Entry>>,
    /// The weaker meaning matches shown too (for this question).
    show_weak: bool,
}

/// What runs again once an archive's password is given.
pub enum Retry {
    Job(crate::ops::OpKind),
    /// The panel whose tab is inside the archive.
    Reload(u64),
    /// A file inside it, opened in its app.
    Open(PathBuf),
}

/// The password for an archive, asked for when something inside it was refused.
pub struct PasswordState {
    pub archive: PathBuf,
    pub retry: Option<Retry>,
    pub text: String,
    pub wrong: bool,
}

/// The passphrase of an encrypted drive, asked as it is opened. The dialog stays up while
/// udisks tries it, to say if it was wrong.
pub struct UnlockState {
    pub drive: crate::drives::Drive,
    /// Where to show the drive once mounted (see `App::drive_op`).
    pub open_in: Option<(u64, bool)>,
    pub pass: crate::drives::Passphrase,
    pub error: Option<String>,
}

impl UnlockState {
    pub fn new(drive: crate::drives::Drive, open_in: Option<(u64, bool)>) -> UnlockState {
        UnlockState { drive, open_in, pass: Default::default(), error: None }
    }
}

/// What Compress asks before it packs: the name (the format follows its extension, and
/// the other way round), how hard to work, and a password for a zip or a 7z.
pub struct CompressState {
    pub srcs: Vec<PathBuf>,
    pub dir: PathBuf,
    pub name: String,
    pub format: crate::archwrite::Format,
    pub level: crate::archwrite::Level,
    pub password: String,
    pub again: String,
    /// The field to focus on the next frame: the name, or (Compress With Password) the
    /// password.
    focus: Option<bool>,
    /// The name last looked for in the folder, and whether something has it.
    taken: Option<(String, bool)>,
}

impl CompressState {
    pub fn new(srcs: Vec<PathBuf>, dir: PathBuf, format: crate::archwrite::Format, locked: bool) -> CompressState {
        // One item is named after itself ("photo.png" -> "photo.zip"); several, "Archive".
        let base = match srcs.as_slice() {
            [one] => {
                let n = one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if one.is_dir() { n } else { Path::new(&n).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(n) }
            }
            _ => "Archive".to_string(),
        };
        let name = fsx::unique_dest(&dir, &format!("{base}{}", format.ext())).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        CompressState { srcs, dir, name, format, level: crate::archwrite::last_level(), password: String::new(), again: String::new(), focus: Some(locked), taken: None }
    }

    /// The name without the extension the format adds.
    fn stem(&self) -> &str {
        match crate::archwrite::Format::of(&self.name) {
            Some(f) => &self.name[..self.name.len() - f.ext().len()],
            None => self.name.trim_end_matches('.'),
        }
    }

    /// The file it makes: the name, with the format's extension when it lacks one.
    pub fn out(&self) -> PathBuf {
        let n = self.name.trim();
        if crate::archwrite::Format::of(n) == Some(self.format) { self.dir.join(n) } else { self.dir.join(format!("{}{}", self.stem().trim(), self.format.ext())) }
    }

    /// Whether the format takes the password: zip always, 7z with the 7z command.
    pub fn lockable(&self) -> bool {
        self.format.lockable() && (self.format != crate::archwrite::Format::SevenZ || crate::archive::can_lock_7z())
    }

    /// Why Compress can't run yet, if it can't ("" when there is nothing to say).
    pub fn problem(&self) -> Option<String> {
        let n = self.name.trim();
        if self.stem().trim().is_empty() || n.contains('/') {
            return Some(String::new());
        }
        let names: Vec<String> = self.srcs.iter().filter_map(|s| s.file_name().map(|n| n.to_string_lossy().into_owned())).collect();
        if let Some(d) = names.iter().enumerate().find(|(i, n)| names[..*i].contains(n)).map(|x| x.1) {
            return Some(format!("Two of these are called “{d}”"));
        }
        if self.taken.as_ref().is_some_and(|t| t.1) {
            return Some(format!("There is a “{}” here already", self.out().file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
        }
        if !self.password.is_empty() && self.lockable() {
            if self.password.contains(['\n', '\r']) {
                return Some("A password on one line".into());
            } else if self.again.is_empty() {
                return Some(String::new());
            } else if self.again != self.password {
                return Some("The two passwords differ".into());
            }
        }
        None
    }
}

/// Connect to a network place: address, and a login when the server wants one.
pub struct ConnectState {
    pub uri: String,
    pub user: String,
    pub password: String,
    pub anonymous: bool,
    pub error: Option<String>,
    focus: bool,
}

impl Popup {

    /// Variant name (simulation failure reports).
    #[cfg(test)]
    pub fn kind_name(&self) -> &'static str {
        match self {
            Popup::None => "None",
            Popup::GoTo(..) => "GoTo",
            Popup::Palette(..) => "Palette",
            Popup::Ctx(..) => "Ctx",
            Popup::Options(..) => "Options",
            Popup::Types { .. } => "Types",
            Popup::TabSearch { .. } => "TabSearch",
            Popup::SavedView { .. } => "SavedView",
            Popup::Zoom { .. } => "Zoom",
            Popup::Layouts { .. } => "Layouts",
            Popup::Prompt { .. } => "Prompt",
            Popup::Confirm { .. } => "Confirm",
            Popup::Choose { .. } => "Choose",
            Popup::Conflict(..) => "Conflict",
            Popup::Sync(..) => "Sync",
            Popup::Properties(..) => "Properties",
            Popup::OpenWith { .. } => "OpenWith",
            Popup::Actions { .. } => "Actions",
            Popup::RunExec { .. } => "RunExec",
            Popup::Connect(..) => "Connect",
            Popup::ArchivePassword(..) => "ArchivePassword",
            Popup::Compress(..) => "Compress",
            Popup::Unlock(..) => "Unlock",
            Popup::Meaning(..) => "Meaning",
            Popup::Themes => "Themes",
            Popup::CodeSearch(..) => "CodeSearch",
            Popup::Tasks(..) => "Tasks",
            Popup::CleanBuilds(..) => "CleanBuilds",
            Popup::Duplicates(..) => "Duplicates",
            Popup::BatchRename(..) => "BatchRename",
            Popup::FormatDrive(..) => "FormatDrive",
            Popup::MusicTags(..) => "MusicTags",
            Popup::Tidy(..) => "Tidy",
            Popup::Print(..) => "Print",
            Popup::Projects(..) => "Projects",
            Popup::QuickOpen(..) => "QuickOpen",
            Popup::UndoHistory(..) => "UndoHistory",
            Popup::Marks(..) => "Marks",
            Popup::Holders(..) => "Holders",
            Popup::SmartEdit(..) => "SmartEdit",
            Popup::SpaceUsed(..) => "SpaceUsed",
            Popup::Viewer(..) => "Viewer",
            Popup::Tour(..) => "Tour",
            Popup::Transfers(..) => "Transfers",
            Popup::Jump(..) => "Jump",
            Popup::Phone(..) => "Phone",
            Popup::Pastes(..) => "Pastes",
            Popup::Calendar(..) => "Calendar",
            Popup::MainMenu => "MainMenu",
            Popup::QuickMenu => "QuickMenu",
            Popup::PanelMenu { .. } => "PanelMenu",
            Popup::PathMenu { .. } => "PathMenu",
            Popup::PathDropdown(..) => "PathDropdown",
            Popup::History { .. } => "History",
            Popup::RenameMenu { .. } => "RenameMenu",
            Popup::FindReplace { .. } => "FindReplace",
            Popup::About => "About",
            Popup::TabMenu { .. } => "TabMenu",
            Popup::TabsOptions { .. } => "TabsOptions",
            Popup::GroupMenu { .. } => "GroupMenu",
            Popup::GitFile { .. } => "GitFile",
            Popup::Branches(..) => "Branches",
            Popup::Stash { .. } => "Stash",
            Popup::Branch { .. } => "Branch",
            Popup::GitCommit(..) => "GitCommit",
            Popup::NewRef(..) => "NewRef",
            Popup::Notice { .. } => "Notice",
            Popup::DropMenu { .. } => "DropMenu",
        }
    }
    pub fn goto(input: String) -> Popup {
        Popup::GoTo(GoToState { input, last: None, sel: 0, items: vec![], focus: true, dir_cache: None, dir_job: None, snap_gen: 0, back: vec![], fwd: vec![], highlight: None, dest: None })
    }
    /// Go To picking a folder for Copy To… or Move To….
    pub fn goto_dest(dest: crate::copyto::Dest) -> Popup {
        let mut p = Popup::goto(String::new());
        if let Popup::GoTo(st) = &mut p {
            st.dest = Some(dest);
        }
        p
    }
    pub fn palette() -> Popup {
        Popup::Palette(PaletteState { query: String::new(), sel: 0, capture: None, conflict: None, focus: true, cat: None, scroll: false, top: true })
    }
    pub fn options() -> Popup {
        Popup::Options(OptState { query: String::new(), focus: true, sel: 0, scroll_to_sel: false, section: None, keyed: false, search_id: None, cursor_end: false, top: true })
    }
    /// Options open at `section`.
    pub fn options_at(section: Section) -> Popup {
        let mut p = Popup::options();
        if let Popup::Options(st) = &mut p {
            st.section = Some(section);
        }
        p
    }
    pub fn types(panel: u64) -> Popup {
        Popup::Types { panel, query: String::new() }
    }
    pub fn tab_search() -> Popup {
        Popup::TabSearch { query: String::new(), sel: 0 }
    }
    pub fn saved_view(panel: u64) -> Popup {
        Popup::SavedView { panel }
    }
    pub fn zoom(panel: u64, anchor: Pos2) -> Popup {
        Popup::Zoom { panel, anchor }
    }
    pub fn layouts(delete: bool) -> Popup {
        Popup::Layouts { delete }
    }
    pub fn meaning() -> Popup {
        Popup::Meaning(MeaningState { query: String::new(), focus: true, sel: 0, hits: vec![], asked: String::new(), job: None, error: None, typed: Instant::now(), names: None, named: vec![], named_for: (String::new(), 0), entries: Default::default(), show_weak: false })
    }
    pub fn prompt(title: &str, text: String, on_ok: Box<dyn FnOnce(&mut App, String)>) -> Popup {
        Popup::Prompt { title: title.into(), text, on_ok: Some(on_ok) }
    }
    pub fn confirm(msg: String, ok: &str, on_ok: Box<dyn FnOnce(&mut App)>) -> Popup {
        Popup::Confirm { msg, ok: ok.into(), on_ok: Some(on_ok) }
    }
    pub fn conflict(srcs: Vec<PathBuf>, dest: PathBuf, mv: bool, items: Vec<ConflictItem>) -> Popup {
        Popup::Conflict(ConflictState { srcs, dest, mv, items, idx: 0, decide: HashMap::new(), apply_all: false, rename: None, focus_name: false })
    }
    pub fn properties(paths: Vec<PathBuf>) -> Popup {
        Popup::Properties(PropState { paths, change: Default::default(), deep: false, ids: None, apply: None, snap: None, expected: String::new(), sum_job: None, sum: None })
    }
    pub fn open_with(paths: Vec<PathBuf>) -> Popup {
        Popup::OpenWith { paths, query: String::new(), all: false, command: String::new() }
    }
    pub fn connect(uri: String, error: Option<String>) -> Popup {
        Popup::Connect(ConnectState { uri, user: String::new(), password: String::new(), anonymous: false, error, focus: true })
    }

    pub fn run_exec(path: PathBuf) -> Popup {
        Popup::RunExec { path }
    }
    pub fn panel_menu(panel: u64, anchor: Pos2) -> Popup {
        Popup::PanelMenu { panel, anchor }
    }
    pub fn path_menu(panel: u64, path: PathBuf, anchor: Pos2) -> Popup {
        Popup::PathMenu { panel, path, anchor }
    }
    pub fn history(panel: u64, anchor: Pos2) -> Popup {
        Popup::History { panel, anchor }
    }
    pub fn git_file(panel: u64, file: usize, pos: Pos2) -> Popup {
        Popup::GitFile { panel, file, pos }
    }
    pub fn branches(panel: u64, root: PathBuf, anchor: Rect) -> Popup {
        Popup::Branches(BranchState { panel, root, anchor, refs: None, job: None, sel: 0, keyed: false, opened: Instant::now() })
    }
    pub fn notice(title: String, text: String) -> Popup {
        Popup::Notice { title, text }
    }
    pub fn rename_menu(panel: u64, pos: Pos2) -> Popup {
        Popup::RenameMenu { panel, pos }
    }
    pub fn path_dropdown(panel: u64, dir: PathBuf, next: Option<PathBuf>) -> Popup {
        let entries = subdirs(&dir);
        let info = entries.iter().take(5000).filter_map(|p| fsx::Entry::from_path(p).map(|e| (p.clone(), e))).collect();
        let sel = next.as_ref().and_then(|n| entries.iter().position(|e| e == n)).unwrap_or(0);
        Popup::PathDropdown(PathDropState { panel, dir, next, query: String::new(), sel, entries, info, anchor: None, focus: true })
    }

    pub fn is_capturing(&self) -> bool {
        matches!(self, Popup::Palette(p) if p.capture.is_some())
    }

    pub fn kind_cmd(&self) -> Option<Cmd> {
        match self {
            Popup::Palette(_) => Some(Cmd::Palette),
            Popup::GoTo(_) => Some(Cmd::GoTo),
            Popup::Options(_) => Some(Cmd::Options),
            _ => None,
        }
    }
}

/// Folder contents for the breadcrumb dropdown: folders first, then files (hidden ones skipped).
fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<(bool, PathBuf)> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().take(10_000).map(|d| (d.file_type().map(|t| t.is_dir()).unwrap_or(false) || d.path().is_dir(), d.path())).collect())
        .unwrap_or_default();
    v.retain(|(_, p)| !p.file_name().map(|n| n.to_string_lossy().starts_with('.')).unwrap_or(false));
    v.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| tab::natural_cmp(&a.1.to_string_lossy().to_lowercase(), &b.1.to_string_lossy().to_lowercase())));
    v.into_iter().map(|(_, p)| p).collect()
}

// ------------------------------------------------------------------ helpers

/// Consume navigation keys for list popups.
pub(crate) struct Keys {
    pub up: bool,
    pub down: bool,
    pub enter: bool,
    pub ctrl_enter: bool,
    pub tab: bool,
    pub esc: bool,
    left: bool,
    right: bool,
    alt_left: bool,
    alt_right: bool,
    pub backspace: bool,
    pub page_up: bool,
    pub page_down: bool,
}

pub(crate) fn take_keys(ctx: &egui::Context) -> Keys {
    ctx.input_mut(|i| Keys {
        up: i.consume_key(Modifiers::NONE, Key::ArrowUp),
        down: i.consume_key(Modifiers::NONE, Key::ArrowDown),
        ctrl_enter: i.consume_key(Modifiers::CTRL, Key::Enter),
        enter: i.consume_key(Modifiers::NONE, Key::Enter),
        tab: i.consume_key(Modifiers::NONE, Key::Tab),
        esc: i.consume_key(Modifiers::NONE, Key::Escape),
        alt_left: i.consume_key(Modifiers::ALT, Key::ArrowLeft),
        alt_right: i.consume_key(Modifiers::ALT, Key::ArrowRight),
        left: false,
        right: false,
        backspace: i.key_pressed(Key::Backspace),
        page_up: i.consume_key(Modifiers::NONE, Key::PageUp),
        page_down: i.consume_key(Modifiers::NONE, Key::PageDown),
    })
}

pub(crate) fn step(sel: &mut usize, n: usize, k: &Keys, skip: impl Fn(usize) -> bool) {
    if n == 0 {
        *sel = 0;
        return;
    }
    let mut d: isize = 0;
    if k.down {
        d = 1;
    }
    if k.up {
        d = -1;
    }
    if k.page_down {
        d = 10;
    }
    if k.page_up {
        d = -10;
    }
    if d == 0 {
        *sel = (*sel).min(n - 1);
        if skip(*sel) {
            // Move off headers.
            if let Some(j) = (*sel..n).find(|&j| !skip(j)) {
                *sel = j;
            }
        }
        return;
    }
    let mut s = (*sel as isize + d).clamp(0, n as isize - 1) as usize;
    while skip(s) {
        let ns = s as isize + d.signum();
        if ns < 0 || ns >= n as isize {
            break;
        }
        s = ns as usize;
    }
    *sel = s;
}

/// The space inside a dialog's edge, and its corners: one measure for every dialog, so
/// titles, lists and buttons line up the same way in each.
pub(crate) const DIALOG_PAD: f32 = 20.0;
pub(crate) const DIALOG_RADIUS: u8 = 12;

/// A menu's or the palette's frame. Its shadow is lighter on a light theme, where the
/// dark one's weight read as a smudge.
pub(crate) fn popup_frame(pal: &Palette) -> egui::Frame {
    egui::Frame::NONE
        .fill(crate::frost::fill(pal.raised))
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::same(8))
        .shadow(egui::epaint::Shadow { offset: [0, 6], blur: 24, spread: 0, color: Color32::from_black_alpha(if pal.dark { 90 } else { 34 }) })
}

/// A dialog's frame: `DIALOG_PAD` inside, rounder corners and a deeper, softer shadow
/// than a menu's, as it stands further off the window.
pub(crate) fn dialog_frame(pal: &Palette) -> egui::Frame {
    popup_frame(pal)
        .corner_radius(CornerRadius::same(DIALOG_RADIUS))
        .inner_margin(egui::Margin::same(DIALOG_PAD as i8))
        .shadow(egui::epaint::Shadow { offset: [0, 12], blur: 36, spread: 0, color: Color32::from_black_alpha(if pal.dark { 110 } else { 40 }) })
}

/// A path with the home folder written as ~.
pub(crate) fn home_short(p: &Path) -> String {
    match p.strip_prefix(fsx::home()) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

/// A dialog's title, with an optional dim line under it.
pub(crate) fn dialog_title(ui: &mut Ui, title: &str, sub: Option<&str>, pal: &Palette) {
    let fs = ui.style().text_styles.get(&egui::TextStyle::Body).map_or(13.0, |f| f.size);
    ui.add(egui::Label::new(egui::RichText::new(title).font(crate::theme::semibold(fs + 1.5)).color(pal.text)).wrap());
    if let Some(sub) = sub {
        ui.add(egui::Label::new(egui::RichText::new(sub).color(pal.text_dim)).wrap());
    }
}

/// A group's name over its card, as Options labels its groups.
fn group_label(ui: &mut Ui, text: &str, pal: &Palette) {
    ui.add_space(8.0);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    ui.painter().text(pos2(r.left() + 4.0, r.center().y - 1.0), Align2::LEFT_CENTER, text, crate::theme::semibold(11.5), pal.text_dim);
}

/// A quiet card holding a group of rows inside a dialog.
pub(crate) fn card<R>(ui: &mut Ui, pal: &Palette, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::NONE
        .fill(pal.card())
        .stroke(Stroke::new(1.0, pal.card_line()))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A label and its value on one row of a card: the label in a column of its own, both
/// on the first line when the value wraps.
fn field_row(ui: &mut Ui, key_w: f32, key: &str, pal: &Palette, value: impl FnOnce(&mut Ui)) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        let g = ui.painter().layout_no_wrap(key.to_string(), egui::TextStyle::Body.resolve(ui.style()), pal.text_dim);
        let (r, _) = ui.allocate_exact_size(vec2(key_w, g.size().y), Sense::hover());
        ui.painter().galley(r.min, g, pal.text_dim);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            value(ui)
        });
    });
}

/// A dialog's footer: a rule across it, then its keys on the left and its buttons on the
/// right, the two centred on each other; where the keys would be squeezed beside the
/// buttons, above them. The buttons as for `dialog_buttons`, the first the rightmost.
///
/// Laid out with the buttons' width from the frame before. When that width changes (the
/// dialog opening, a label growing) the frame is drawn again before it is shown, so the
/// footer never shows a frame laid out for the old width.
pub(crate) fn footer(ui: &mut Ui, items: &[(&str, &str)], pal: &Palette, add: impl FnOnce(&mut Ui)) {
    footer_at(ui, items, pal, None, add);
}

/// `footer` with its buttons' labels (main one first) measured this frame: for buttons that
/// come and go, which the frame before would lay out a frame late, the keys under them.
pub(crate) fn footer_with(ui: &mut Ui, items: &[(&str, &str)], labels: &[&str], pal: &Palette, add: impl FnOnce(&mut Ui)) {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let pad = ui.spacing().button_padding.x;
    let w = labels.iter().map(|l| (ui.painter().layout_no_wrap(l.to_string(), font.clone(), pal.text).size().x + 2.0 * pad).max(80.0)).sum::<f32>() + 8.0 * labels.len().saturating_sub(1) as f32;
    footer_at(ui, items, pal, Some(w), add);
}

fn footer_at(ui: &mut Ui, items: &[(&str, &str)], pal: &Palette, measured: Option<f32>, add: impl FnOnce(&mut Ui)) {
    let id = ui.id().with("footer");
    let known = measured.or_else(|| ui.ctx().data(|d| d.get_temp::<f32>(id)));
    let bw = known.unwrap_or(0.0);
    ui.add_space(14.0);
    let (left, avail) = (ui.cursor().left(), ui.available_width());
    let y = ui.cursor().top();
    ui.painter().line_segment([pos2(left, y), pos2(left + avail, y)], Stroke::new(1.0, pal.border));
    ui.add_space(15.0);
    let top = ui.cursor().top();
    let side = !items.is_empty() && avail - bw - 24.0 >= 240.0;
    let keys_w = if side { avail - bw - 24.0 } else { avail };
    let mut kh = 0.0;
    if !items.is_empty() {
        let mut keys = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(left, top), vec2(keys_w, 4000.0))).layout(egui::Layout::top_down(egui::Align::Min)));
        hint_chips(&mut keys, items, pal);
        kh = keys.min_rect().height();
    }
    let s = ui.spacing();
    let bh = (ui.text_style_height(&egui::TextStyle::Button) + 2.0 * s.button_padding.y).max(s.interact_size.y);
    let (bt, band) = if side || items.is_empty() { (top, kh.max(bh)) } else { (top + kh + 12.0, bh) };
    let mut b = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(left, bt), vec2(avail, band))).layout(egui::Layout::right_to_left(egui::Align::Center)));
    b.spacing_mut().item_spacing.x = 8.0;
    add(&mut b);
    let w = b.min_rect().width();
    ui.allocate_exact_size(vec2(avail, bt + band - top), Sense::hover());
    if measured.is_none() && known.is_none_or(|k| (k - w).abs() > 0.5) {
        ui.ctx().data_mut(|d| d.insert_temp(id, w));
        ui.ctx().request_discard("dialog footer width");
    }
}

/// A dialog's buttons, on the right; the first given is the rightmost (the main one).
pub(crate) fn dialog_buttons(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.add_space(16.0);
    // In a row of its own first: laid out right to left straight in the dialog, the
    // buttons would centre on all the height left below them.
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            add(ui)
        });
    });
}

/// The main button of a dialog, in the accent.
pub(crate) fn primary_button(ui: &mut Ui, label: &str, enabled: bool, pal: &Palette) -> egui::Response {
    ui.add_enabled(enabled, egui::Button::new(egui::RichText::new(label).color(pal.on_accent())).fill(pal.accent).min_size(vec2(80.0, 0.0)))
}

/// A dialog's other buttons: a quiet fill and outline, so they read as buttons and not
/// as text beside the accent one.
pub(crate) fn plain_button(ui: &mut Ui, label: &str, pal: &Palette) -> egui::Response {
    ui.add(egui::Button::new(label).fill(pal.card()).stroke(Stroke::new(1.0, pal.card_line())).min_size(vec2(80.0, 0.0)))
}

/// Top-centered popup. Returns (inner, rect).
/// How far below the window's top a centred popup opens: less in a short window.
pub(crate) fn popup_top(screen: Rect) -> f32 {
    (screen.height() * 0.09).clamp(12.0, 70.0).round()
}

/// A popup's list, as tall as `max` but leaving `chrome` (its search field, hints and
/// margins) room in the window: in a short one the list shrank off the bottom.
pub(crate) fn fit_list(ctx: &egui::Context, max: f32, chrome: f32) -> f32 {
    let s = ctx.content_rect();
    // The dialog's padding beyond the frame's old eight points comes out of the list.
    max.min(s.height() - popup_top(s) - 12.0 - chrome - 2.0 * (DIALOG_PAD - 8.0)).max(96.0)
}

pub(crate) fn centered<R>(ctx: &egui::Context, id: &str, width: f32, pal: &Palette, add: impl FnOnce(&mut Ui) -> R) -> (R, Rect) {
    centered_top(ctx, id, width, popup_top(ctx.content_rect()), pal, add)
}

/// `centered`, `top` below the window's top: a dialog that grows to fill the window.
pub(crate) fn centered_top<R>(ctx: &egui::Context, id: &str, width: f32, top: f32, pal: &Palette, add: impl FnOnce(&mut Ui) -> R) -> (R, Rect) {
    let screen = ctx.content_rect();
    let w = width.min(screen.width() - 24.0);
    // A dialog taller than the window scrolls instead of losing its buttons off the
    // bottom; the scroll area claims its height (see `anchored`).
    let inner = w - 2.0 * DIALOG_PAD;
    let room = (screen.height() - top - 12.0 - 2.0 * DIALOG_PAD).max(60.0);
    note_layer(Id::new(id));
    let r = egui::Area::new(Id::new(id))
        .order(egui::Order::Foreground)
        .fixed_pos(pos2((screen.center().x - w / 2.0).round(), screen.top() + top))
        .show(ctx, |ui| {
            crate::frost::behind(ui, DIALOG_RADIUS);
            dialog_frame(pal)
                .show(ui, |ui| {
                    ui.set_width(inner);
                    egui::ScrollArea::vertical().id_salt("dialog").max_height(room).min_scrolled_height(room).auto_shrink([false, true]).show(ui, |ui| {
                        ui.set_width(inner);
                        add(ui)
                    }).inner
                })
                .inner
        });
    crate::ui::note_popup(r.response.rect);
    (r.inner, r.response.rect)
}

fn anchored<R>(ctx: &egui::Context, id: &str, pos: Pos2, width: f32, pal: &Palette, add: impl FnOnce(&mut Ui) -> R) -> (R, Rect) {
    anchored_from(ctx, id, pos, Align2::LEFT_TOP, width, pal, add)
}

/// `anchored`, with `pos` the menu's corner given by `pivot`: a menu opening upward
/// from the status bar hangs from its lower edge, whatever its height.
pub(crate) fn anchored_from<R>(ctx: &egui::Context, id: &str, pos: Pos2, pivot: Align2, width: f32, pal: &Palette, add: impl FnOnce(&mut Ui) -> R) -> (R, Rect) {
    let screen = ctx.content_rect();
    let x = pos.x.min(screen.right() - width - 8.0).max(screen.left() + 4.0);
    // Kept off the window's edges, and scrolled when taller than the window (a Quick menu
    // with a long list ran off the bottom). The scroll area claims its height: the area
    // remembers its last size and would cut a longer menu short (see ctxmenu.rs).
    let room = if pivot == Align2::LEFT_BOTTOM { pos.y - screen.top() - 26.0 } else { screen.height() - 16.0 - 10.0 }.max(60.0);
    let r = egui::Area::new(Id::new(id)).order(egui::Order::Foreground).pivot(pivot).fixed_pos(pos2(x, pos.y)).constrain_to(screen.shrink(6.0)).show(ctx, |ui| {
        crate::frost::behind(ui, 10);
        popup_frame(pal)
            .inner_margin(egui::Margin::same(5))
            .show(ui, |ui| {
                ui.set_width(width - 10.0);
                egui::ScrollArea::vertical().max_height(room).min_scrolled_height(room).auto_shrink([false, true]).show(ui, add).inner
            })
            .inner
    });
    crate::ui::note_popup(r.response.rect);
    (r.inner, r.response.rect)
}

pub(crate) fn clicked_outside(ctx: &egui::Context, rect: Rect) -> bool {
    ctx.input(|i| i.pointer.any_pressed()) && ctx.pointer_interact_pos().map(|p| !rect.contains(p)).unwrap_or(false)
}

pub(crate) fn search_field(ui: &mut Ui, text: &mut String, hint: &str, focus: &mut bool, pal: &Palette, glyph: Glyph) -> egui::Response {
    let resp = ui
        .horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(20.0, 22.0), Sense::hover());
            paint_glyph(ui.painter(), Rect::from_center_size(r.center(), Vec2::splat(14.0)), glyph, pal.text_dim);
            ui.add(egui::TextEdit::singleline(text).hint_text(egui::RichText::new(hint).color(pal.text_dim)).frame(crate::ui::bare_edit()).desired_width(ui.available_width() - 8.0).font(FontId::proportional(15.0)))
        })
        .inner;
    if *focus {
        resp.request_focus();
        *focus = false;
    }
    ui.add_space(2.0);
    ui.painter().line_segment([pos2(ui.min_rect().left(), ui.cursor().top()), pos2(ui.min_rect().right(), ui.cursor().top())], Stroke::new(1.0, pal.border));
    ui.add_space(4.0);
    resp
}

pub(crate) fn hints(ui: &mut Ui, items: &[(&str, &str)], pal: &Palette) {
    // A rule between the list and the keys, as under the search field: a row scrolled
    // half out of view ran straight into them.
    ui.add_space(2.0);
    let y = ui.cursor().top();
    ui.painter().line_segment([pos2(ui.min_rect().left(), y), pos2(ui.min_rect().right(), y)], Stroke::new(1.0, pal.border));
    ui.add_space(4.0);
    hint_chips(ui, items, pal);
}

/// `hints` without the rule above them, for a footer that draws its own.
pub(crate) fn hint_chips(ui: &mut Ui, items: &[(&str, &str)], pal: &Palette) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (k, label) in items {
            if k.is_empty() {
                ui.label(egui::RichText::new(*label).size(11.0).color(pal.text_dim));
                continue;
            }
            // A key and its label wrap together, as one block measured first: wrapped apart, a
            // label began the next line on its own, away from its key.
            let g = ui.painter().layout_no_wrap(k.to_string(), FontId::proportional(10.5), pal.text_dim);
            let lg = ui.painter().layout_no_wrap(label.to_string(), FontId::proportional(11.0), pal.text_dim);
            let chip = g.size() + vec2(10.0, 6.0);
            // A key that shares the next one's label ("Up" "Down" to navigate) has none of
            // its own; an empty label threw off the wrapped row and the chips overlapped.
            let lw = if label.is_empty() { 0.0 } else { 4.0 + lg.size().x };
            let (whole, _) = ui.allocate_exact_size(vec2(chip.x + lw, chip.y.max(lg.size().y)), Sense::hover());
            let r = Rect::from_min_size(pos2(whole.left(), whole.center().y - chip.y / 2.0), chip);
            ui.painter().rect(r, CornerRadius::same(4), pal.bg, Stroke::new(1.0, pal.border), StrokeKind::Inside);
            ui.painter().galley(r.min + vec2(5.0, 3.0), g, pal.text_dim);
            crate::ui::note_control(ui, r, "key hint");
            if !label.is_empty() {
                ui.painter().galley(pos2(r.right() + 4.0, whole.center().y - lg.size().y / 2.0), lg, pal.text_dim);
            }
            if !label.is_empty() {
                ui.add_space(6.0);
            }
        }
    });
}

/// Full-width selectable row; returns its response. `paint` draws inside the rect.
pub(crate) fn row(ui: &mut Ui, h: f32, selected: bool, pal: &Palette, paint: impl FnOnce(&Ui, Rect)) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    if selected {
        ui.painter().rect_filled(r, CornerRadius::same(6), pal.selected);
    } else if resp.hovered() {
        ui.painter().rect_filled(r, CornerRadius::same(6), pal.hover);
    }
    paint(ui, r);
    resp
}

pub(crate) fn highlight_job(text: &str, q: &fuzzy::Query, font: FontId, col: Color32, accent: Color32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    highlight_into(&mut job, text, q, font, col, accent);
    job
}

/// `text` added to `job`, the letters matching `q` in `accent`.
fn highlight_into(job: &mut egui::text::LayoutJob, text: &str, q: &fuzzy::Query, font: FontId, col: Color32, accent: Color32) {
    let hits: std::collections::HashSet<usize> = fuzzy::positions(q, text).map(|(_, p)| p.into_iter().collect()).unwrap_or_default();
    for (i, ch) in text.chars().enumerate() {
        let c = if hits.contains(&i) { accent } else { col };
        job.append(&ch.to_string(), 0.0, egui::TextFormat { font_id: font.clone(), color: c, ..Default::default() });
    }
}

fn menu_item(ui: &mut Ui, label: &str, chip: Option<String>, pal: &Palette) -> bool {
    let resp = row(ui, 26.0, false, pal, |ui, r| {
        // Fit the row (leaving room for the shortcut chip); paths keep their tail.
        let chip_w = chip.as_ref().map(|c| ui.painter().layout_no_wrap(c.clone(), FontId::proportional(11.0), pal.text_dim).size().x + 22.0).unwrap_or(0.0);
        let room = (r.width() - 20.0 - chip_w).max(24.0);
        let font = FontId::proportional(13.0);
        let g = if label.contains('/') { crate::listing::elided_tail(ui, label, font, room, pal.text) } else { crate::listing::elided(ui, label, font, room, pal.text) };
        ui.painter().galley(pos2(r.left() + 10.0, r.center().y - g.size().y / 2.0), g, pal.text);
        if let Some(c) = &chip {
            crate::ui::paint_chip(ui.painter(), pos2(r.right() - 6.0, r.center().y), c, pal);
        }
    });
    resp.clicked()
}

/// A group's name inside a menu: small and dim, in line with the items' text below it,
/// with room above it so it reads as the start of a group.
pub(crate) fn menu_heading(ui: &mut Ui, text: &str, pal: &Palette) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
    ui.painter().text(pos2(r.left() + 10.0, r.bottom() - 4.0), Align2::LEFT_BOTTOM, text, FontId::proportional(11.5), pal.text_dim);
}

fn menu_sep(ui: &mut Ui, pal: &Palette) {
    ui.add_space(3.0);
    let y = ui.cursor().top();
    ui.painter().line_segment([pos2(ui.min_rect().left() + 6.0, y), pos2(ui.min_rect().right() - 6.0, y)], Stroke::new(1.0, pal.border));
    ui.add_space(4.0);
}

fn cmd_item(ui: &mut Ui, app: &App, c: Cmd) -> bool {
    cmd_row(ui, app, c)
}

// ------------------------------------------------------------------ entry point

thread_local! {
    /// Layers drawn by popups this frame, and the last open popup's shapes (for the
    /// close fade): (shapes, fade start time).
    static POPUP_LAYERS: std::cell::RefCell<Vec<egui::LayerId>> = const { std::cell::RefCell::new(Vec::new()) };
    pub(crate) static FADE: std::cell::RefCell<(Vec<egui::epaint::ClippedShape>, Option<f64>)> = const { std::cell::RefCell::new((Vec::new(), None)) };
}

const CLOSE_FADE: f64 = 0.08;

pub(crate) fn note_layer(id: Id) {
    POPUP_LAYERS.with(|l| l.borrow_mut().push(egui::LayerId::new(egui::Order::Foreground, id)));
}

/// A closed dialog (palette, GoTo, Options and other centered popups) fades out over
/// 80 ms: its last frame is replayed with falling opacity on a layer that takes no input.
/// Menus close instantly: they are opened and dismissed too often for a fade.
fn close_fade(ctx: &egui::Context, animations: bool, open_now: bool) {
    let now = ctx.input(|i| i.time);
    let layers: Vec<egui::LayerId> = POPUP_LAYERS.with(|l| std::mem::take(&mut *l.borrow_mut()));
    FADE.with(|f| {
        let mut f = f.borrow_mut();
        if open_now {
            // Remember what the open popup looks like this frame.
            let shapes: Vec<egui::epaint::ClippedShape> = ctx.graphics(|g| layers.iter().filter_map(|l| g.get(*l)).flat_map(|pl| pl.all_entries().cloned()).collect());
            *f = (shapes, None);
            return;
        }
        if f.0.is_empty() || !animations {
            f.0.clear();
            return;
        }
        let start = *f.1.get_or_insert(now);
        let t = ((now - start) / CLOSE_FADE) as f32;
        if t >= 1.0 {
            f.0.clear();
            f.1 = None;
            return;
        }
        let mut painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::new("popup_close_fade")));
        painter.set_opacity((1.0 - t) * (1.0 - t));
        for cs in &f.0 {
            painter.with_clip_rect(cs.clip_rect).add(cs.shape.clone());
        }
        ctx.request_repaint();
    });
}

pub fn draw(app: &mut App, ctx: &egui::Context) {
    if matches!(app.popup, Popup::None) {
        close_fade(ctx, app.cfg.animations, false);
        return;
    }
    let mut popup = std::mem::replace(&mut app.popup, Popup::None);
    let keep = match &mut popup {
        Popup::None => false,
        Popup::GoTo(st) => goto(app, ctx, st),
        Popup::Palette(st) => palette(app, ctx, st),
        Popup::Ctx(st) => crate::ctxmenu::draw(app, ctx, st),
        Popup::Options(st) => options(app, ctx, st),
        Popup::Types { panel, query } => types(app, ctx, *panel, query),
        Popup::TabSearch { query, sel } => tab_search(app, ctx, query, sel),
        Popup::SavedView { panel } => saved_view(app, ctx, *panel),
        Popup::Zoom { panel, anchor } => zoom(app, ctx, *panel, *anchor),
        Popup::Layouts { delete } => layouts(app, ctx, *delete),
        Popup::Prompt { title, text, on_ok } => prompt(app, ctx, title, text, on_ok),
        Popup::Confirm { msg, ok, on_ok } => confirm(app, ctx, msg, ok, on_ok),
        Popup::Choose { msg, sub, choices } => choose(app, ctx, msg, sub, choices),
        Popup::Conflict(st) => conflict(app, ctx, st),
        Popup::Sync(st) => sync(app, ctx, st),
        Popup::Properties(st) => properties(app, ctx, st),
        Popup::OpenWith { paths, query, all, command } => open_with(app, ctx, paths, query, all, command),
        Popup::Actions { sel } => actions(app, ctx, sel),
        Popup::RunExec { path } => run_exec(app, ctx, path),
        Popup::Connect(st) => connect(app, ctx, st),
        Popup::ArchivePassword(st) => archive_password(app, ctx, st),
        Popup::Compress(st) => compress(app, ctx, st),
        Popup::Unlock(st) => unlock_drive(app, ctx, st),
        Popup::Meaning(st) => meaning(app, ctx, st),
        Popup::Themes => themes(app, ctx),
        Popup::CodeSearch(st) => crate::devtools::code_search_popup(app, ctx, st),
        Popup::Tasks(st) => crate::devtools::tasks_popup(app, ctx, st),
        Popup::CleanBuilds(st) => crate::devtools::clean_popup(app, ctx, st),
        Popup::Duplicates(st) => crate::dupes::popup(app, ctx, st),
        Popup::BatchRename(st) => crate::batchname::popup(app, ctx, st),
        Popup::FormatDrive(st) => crate::formatdrive::popup(app, ctx, st),
        Popup::MusicTags(st) => crate::musictags::popup(app, ctx, st),
        Popup::Tidy(st) => crate::tidy::popup(app, ctx, st),
        Popup::Print(st) => crate::sendto::dialog(app, ctx, st),
        Popup::Projects(st) => crate::projects::popup(app, ctx, st),
        Popup::QuickOpen(st) => crate::quickopen::popup(app, ctx, st),
        Popup::UndoHistory(st) => crate::undohist::popup(app, ctx, st),
        Popup::Marks(st) => crate::marks::popup(app, ctx, st),
        Popup::Holders(st) => crate::holders::popup(app, ctx, st),
        Popup::SmartEdit(st) => crate::smart::popup(app, ctx, st),
        Popup::SpaceUsed(st) => crate::spaceuse::popup(app, ctx, st),
        Popup::Viewer(st) => crate::viewer::draw(app, ctx, st),
        Popup::Tour(st) => crate::tour::draw(app, ctx, st),
        Popup::Transfers(st) => crate::transfers::popup(app, ctx, st),
        Popup::Jump(st) => crate::jump::popup(app, ctx, st),
        Popup::Phone(st) => crate::phone::popup(app, ctx, st),
        Popup::Pastes(st) => crate::pastes::popup(app, ctx, st),
        Popup::Calendar(st) => crate::calendar::popup(app, ctx, st),
        Popup::MainMenu => main_menu(app, ctx),
        Popup::QuickMenu => quick_menu(app, ctx, None),
        Popup::PanelMenu { panel, anchor } => {
            let (p, a) = (*panel, *anchor);
            quick_menu(app, ctx, Some((p, a)))
        }
        Popup::PathMenu { panel, path, anchor } => path_menu(app, ctx, *panel, path, *anchor),
        Popup::PathDropdown(st) => path_dropdown(app, ctx, st),
        Popup::History { panel, anchor } => history(app, ctx, *panel, *anchor),
        Popup::RenameMenu { panel, pos } => rename_menu(app, ctx, *panel, *pos),
        Popup::FindReplace { panel } => find_replace(app, ctx, *panel),
        Popup::About => about(app, ctx),
        Popup::TabMenu { panel, idx, pos } => tab_menu(app, ctx, *panel, *idx, *pos),
        Popup::TabsOptions { panel, pos } => tabs_options(app, ctx, *panel, *pos),
        Popup::GroupMenu { panel, label, pos } => group_menu(app, ctx, *panel, label, *pos),
        Popup::GitFile { panel, file, pos } => git_file_menu(app, ctx, *panel, *file, *pos),
        Popup::Branches(st) => branch_menu(app, ctx, st),
        Popup::Stash { panel, root, stash, anchor, sel } => stash_menu(app, ctx, *panel, root, stash, *anchor, sel),
        Popup::Branch { root, branch, anchor, sel, panel, into, remote } => branch_actions(app, ctx, root, branch, *anchor, sel, (*panel, into, *remote)),
        Popup::GitCommit(m) => commit_menu(app, ctx, m),
        Popup::NewRef(st) => new_ref(app, ctx, st),
        Popup::Notice { title, text } => notice(app, ctx, title, text),
        Popup::DropMenu { paths, dest, pos } => drop_menu(app, ctx, paths, dest, *pos),
    };
    if keep && matches!(app.popup, Popup::None) {
        app.popup = popup;
    } else if !keep {
        // Give the keyboard back to the listing.
        ctx.memory_mut(|m| {
            if let Some(f) = m.focused() {
                m.surrender_focus(f);
            }
        });
    }
    close_fade(ctx, app.cfg.animations, !matches!(app.popup, Popup::None));
}

// ------------------------------------------------------------------ GoTo

fn is_pathlike(s: &str) -> bool {
    s.starts_with('/') || s.starts_with('~') || s.starts_with('$')
}

fn goto_items(app: &App, st: &mut GoToState, known: &crate::sidebar::Known, ctx: &egui::Context) {
    let input = st.input.clone();
    let mut items = vec![];
    let label_of = |p: &Path| -> String {
        if tab::is_computer(p) {
            "Computer".into()
        } else if tab::is_github(p) {
            "GitHub".into()
        } else {
            p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
        }
    };
    // Short location hint: the parent folder, with the home folder as ~.
    let home = fsx::home();
    let sub_of = |p: &Path| -> String {
        if tab::is_place(p) {
            return String::new();
        }
        let s = p.parent().map(|x| x.display().to_string()).unwrap_or_default();
        let h = home.display().to_string();
        if s.starts_with(&h) { format!("~{}", &s[h.len()..]) } else { s }
    };
    let push = |items: &mut Vec<GoItem>, p: &Path| {
        items.push(GoItem { label: label_of(p), sub: sub_of(p), path: p.to_path_buf(), is_dir: tab::is_place(p) || known.dirs.contains(p), header: false, usage: None });
    };
    let header = |items: &mut Vec<GoItem>, s: &str| items.push(GoItem { label: s.into(), sub: String::new(), path: PathBuf::new(), is_dir: false, header: true, usage: None });
    if input.trim().is_empty() {
        let collapsed = |g: &str| app.ud.collapsed_groups.iter().any(|x| x == &format!("goto:{g}"));
        // Picking a destination: where things were sent last come first.
        if st.dest.is_some() && !app.ud.destinations.is_empty() {
            header(&mut items, "Recent destinations");
            if !collapsed("Recent destinations") {
                for p in &app.ud.destinations {
                    push(&mut items, p);
                    if let Some(it) = items.last_mut() {
                        it.is_dir = true;
                    }
                }
            }
        }
        // Same groups, order and icons as the sidebar.
        let recents: Vec<PathBuf> = known.recents.iter().take(10).cloned().collect();
        if !recents.is_empty() {
            header(&mut items, "Recents");
            if !collapsed("Recents") {
                for p in &recents {
                    push(&mut items, p);
                }
            }
        }
        if !app.ud.bookmarks.is_empty() {
            header(&mut items, "Bookmarks");
            if !collapsed("Bookmarks") {
                for b in &app.ud.bookmarks {
                    push(&mut items, b);
                }
            }
        }
        header(&mut items, "Storage");
        if !collapsed("Storage") {
            for (path, label, t, f) in &known.mounts {
                let usage = (*t > 0).then(|| (t - f) as f32 / *t as f32);
                items.push(GoItem { label: label.clone(), sub: String::new(), path: path.clone(), is_dir: true, header: false, usage });
            }
        }
        header(&mut items, "Places");
        if !collapsed("Places") {
            for (_, p, _) in &known.places {
                push(&mut items, p);
            }
        }
    } else if is_pathlike(&input) {
        let expanded = fsx::expand_path(&input);
        let (dir, prefix) = if input.ends_with('/') { (expanded.clone(), String::new()) } else { (expanded.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/")), expanded.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()) };
        // Read the folder off the UI thread (a dead network mount must not freeze typing);
        // the list fills in when the read lands.
        if st.dir_cache.as_ref().map(|(d, _)| *d != dir).unwrap_or(true) && st.dir_job.as_ref().map(|(d, _)| *d != dir).unwrap_or(true) {
            let (tx, rx) = std::sync::mpsc::channel();
            let (d2, ctx) = (dir.clone(), ctx.clone());
            std::thread::spawn(move || {
                let list = std::fs::read_dir(&d2)
                    .map(|rd| {
                        rd.flatten()
                            .take(50_000)
                            .map(|d| {
                                let is_dir = d.file_type().map(|t| t.is_dir() || (t.is_symlink() && d.path().is_dir())).unwrap_or(false);
                                (d.file_name().to_string_lossy().into_owned(), is_dir)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let _ = tx.send(list);
                ctx.request_repaint();
            });
            st.dir_job = Some((dir.clone(), rx));
        }
        let empty = vec![];
        let list = st.dir_cache.as_ref().filter(|(d, _)| *d == dir).map(|(_, l)| l).unwrap_or(&empty);
        let lp = prefix.to_lowercase();
        let q = fuzzy::Query::new(&prefix);
        let mut scored: Vec<(i32, &String, bool)> = list
            .iter()
            .filter(|(n, _)| app.cfg.show_hidden || !n.starts_with('.') || prefix.starts_with('.'))
            .filter_map(|(n, d)| {
                let ln = n.to_lowercase();
                if lp.is_empty() {
                    Some((0, n, *d))
                } else if ln.starts_with(&lp) {
                    Some((1000 - n.len() as i32, n, *d))
                } else {
                    fuzzy::score(&q, &ln).map(|s| (s, n, *d))
                }
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.2.cmp(&a.2)).then_with(|| tab::natural_cmp(&a.1.to_lowercase(), &b.1.to_lowercase())));
        if lp.is_empty() {
            scored.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| tab::natural_cmp(&a.1.to_lowercase(), &b.1.to_lowercase())));
        }
        // A destination typed down to its folder ("~/Photos/") is that folder: first.
        if st.dest.is_some() && input.ends_with('/') && st.dir_cache.as_ref().is_some_and(|(d, _)| *d == dir) {
            items.push(GoItem { label: label_of(&dir), sub: dir.parent().map(|p| format!("› {}", p.display())).unwrap_or_default(), path: dir.clone(), is_dir: true, header: false, usage: None });
        }
        for (_, n, d) in scored.into_iter().take(300) {
            let p = dir.join(n);
            items.push(GoItem { label: n.clone(), sub: format!("› {}", dir.display()), path: p, is_dir: d, header: false, usage: None });
        }
    } else {
        // Fuzzy across places, bookmarks, recents, mounts and folders seen in open tabs.
        let q = fuzzy::Query::new(&input);
        let mut cands: Vec<(PathBuf, i32)> = vec![];
        let mut seen = std::collections::HashSet::new();
        let mut add = |p: PathBuf, bonus: i32, cands: &mut Vec<(PathBuf, i32)>| {
            if seen.insert(p.clone()) {
                cands.push((p, bonus));
            }
        };
        let dests: std::collections::HashSet<&PathBuf> = if st.dest.is_some() { app.ud.destinations.iter().collect() } else { Default::default() };
        for (i, r) in app.ud.destinations.iter().enumerate().filter(|_| st.dest.is_some()) {
            add(r.clone(), 60 - i as i32, &mut cands);
        }
        for (i, r) in app.ud.recents.iter().enumerate() {
            add(r.clone(), 40 - (i as i32).min(40), &mut cands);
        }
        for b in &app.ud.bookmarks {
            add(b.clone(), 30, &mut cands);
        }
        for (_, p, _) in &known.places {
            add(p.clone(), 25, &mut cands);
        }
        for m in &known.mounts {
            add(m.0.clone(), 10, &mut cands);
        }
        let mut tab_dirs = std::collections::HashSet::new();
        let mut budget = 200_000usize;
        for p in app.panels.values() {
            for t in &p.tabs {
                for e in &t.entries {
                    if budget == 0 {
                        break;
                    }
                    budget -= 1;
                    if e.is_dir {
                        tab_dirs.insert(e.path.clone());
                        add(e.path.clone(), 5, &mut cands);
                    }
                }
            }
        }
        let mut scored: Vec<(i32, PathBuf)> = cands
            .into_iter()
            .filter_map(|(p, bonus)| {
                let name = p.file_name()?.to_string_lossy().to_lowercase();
                fuzzy::score(&q, &name).map(|s| (s + bonus, p))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        // Only folders known to exist (from the background snapshot or open tabs): no stat calls here.
        for (_, p) in scored.into_iter().filter(|(_, p)| tab::is_place(p) || known.dirs.contains(p) || tab_dirs.contains(p) || dests.contains(p)).take(200) {
            push(&mut items, &p);
            // A folder seen only in an open tab is still a folder (it had a file's icon).
            if let Some(it) = items.last_mut() {
                it.is_dir = true;
            }
        }
    }
    if st.dest.is_some() {
        // Only folders take things; a group left empty goes too, unless it is folded.
        items.retain(|it| it.header || (it.is_dir && crate::copyto::offered(&it.path)));
        let folded = |it: &GoItem| app.ud.collapsed_groups.iter().any(|g| *g == format!("goto:{}", it.label));
        let mut keep = vec![true; items.len()];
        for i in 0..items.len() {
            keep[i] = !items[i].header || folded(&items[i]) || items.get(i + 1).is_some_and(|n| !n.header);
        }
        let mut k = keep.into_iter();
        items.retain(|_| k.next().unwrap_or(true));
    }
    st.items = items;
}

fn goto(app: &mut App, ctx: &egui::Context, st: &mut GoToState) -> bool {
    let pal = app.pal;
    let mut landed = false;
    if let Some((d, rx)) = &st.dir_job {
        if let Ok(list) = rx.try_recv() {
            st.dir_cache = Some((d.clone(), list));
            st.dir_job = None;
            landed = true;
        }
    }
    let same_input = st.last.as_deref() == Some(st.input.as_str());
    if !same_input {
        if let Some(d) = &mut st.dest {
            d.refused = None;
        }
    }
    if !same_input || landed || crate::sidebar::generation() != st.snap_gen {
        // Background data changed under an unchanged query: keep the highlighted row.
        let keep = same_input.then(|| st.items.get(st.sel).filter(|i| !i.header).map(|i| i.path.clone())).flatten();
        let known = crate::sidebar::known(app, ctx);
        st.snap_gen = known.generation;
        goto_items(app, st, &known, ctx);
        st.last = Some(st.input.clone());
        let want = st.highlight.take().or(keep);
        st.sel = want.and_then(|h| st.items.iter().position(|i| !i.header && i.path == h)).unwrap_or_else(|| if same_input { st.sel.min(st.items.len().saturating_sub(1)) } else { 0 });
    }
    let k = take_keys(ctx);
    let n = st.items.len();
    {
        let items = &st.items;
        step(&mut st.sel, n, &k, |i| items[i].header);
    }
    let mut close = false;
    let mut go: Option<(PathBuf, bool, bool)> = None; // path, is_dir, new_tab
    if k.esc {
        return false;
    }
    // A network address connects (and opens) the place.
    if (k.enter || k.ctrl_enter) && st.dest.is_none() && crate::network::is_uri(&st.input) {
        let panel = app.active;
        app.connect(crate::network::normalize(&st.input), crate::network::Login { user: String::new(), password: String::new(), anonymous: false }, Some((panel, k.ctrl_enter)));
        return false;
    }
    let cur = st.items.get(st.sel).filter(|i| !i.header).map(|i| (i.path.clone(), i.is_dir));
    if k.enter || k.ctrl_enter {
        match &cur {
            Some((p, d)) => go = Some((p.clone(), *d, k.ctrl_enter)),
            // A destination typed in full is checked when it is used.
            None if st.dest.is_some() => go = Some((fsx::expand_path(&st.input), true, false)),
            None => {
                let p = fsx::expand_path(&st.input);
                if p.exists() {
                    go = Some((p.clone(), p.is_dir(), k.ctrl_enter));
                }
            }
        }
    }
    if k.tab {
        if let Some((p, true)) = &cur {
            st.back.push(st.input.clone());
            st.input = if tab::is_place(p) { "/".into() } else { format!("{}/", p.display()).replace("//", "/") };
            st.focus = true;
        }
    }
    // Backspace at a separator goes up one level.
    if k.backspace && st.input.ends_with('/') && st.input.len() > 1 {
        let trimmed = st.input.trim_end_matches('/');
        if let Some(i) = trimmed.rfind('/') {
            st.input = trimmed[..=i].to_string();
            ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Backspace));
            st.focus = true;
        }
    }
    let (_, rect) = centered(ctx, "goto", 620.0, &pal, |ui| {
        if let Some(d) = &st.dest {
            dialog_title(ui, &d.title(), None, &pal);
            ui.add_space(4.0);
        }
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(20.0, 22.0), Sense::hover());
            let g = if !is_pathlike(&st.input) { Glyph::Search } else if fsx::mounts().iter().any(|m| m.path == fsx::expand_path(st.input.trim_end_matches('/'))) || st.input == "/" { Glyph::Drive } else { Glyph::Folder };
            paint_glyph(ui.painter(), Rect::from_center_size(r.center(), Vec2::splat(14.0)), g, pal.text_dim);
            let w = ui.available_width() - 4.0 * 28.0;
            let hint = match &st.dest {
                Some(d) if d.mv => "Folder to move into…",
                Some(_) => "Folder to copy into…",
                None => "Open or expand a folder…",
            };
            let resp = ui.add(egui::TextEdit::singleline(&mut st.input).hint_text(egui::RichText::new(hint).color(pal.text_dim)).frame(crate::ui::bare_edit()).desired_width(w).font(FontId::proportional(15.0)));
            if st.focus {
                resp.request_focus();
                if let Some(mut s) = egui::TextEdit::load_state(ui.ctx(), resp.id) {
                    let c = egui::text::CCursor::new(st.input.chars().count());
                    s.cursor.set_char_range(Some(egui::text::CCursorRange::one(c)));
                    s.store(ui.ctx(), resp.id);
                }
                st.focus = false;
            }
            if crate::ui::glyph_button(ui, Glyph::Back, "Back", !st.back.is_empty(), &pal).clicked() {
                if let Some(b) = st.back.pop() {
                    let was = std::mem::replace(&mut st.input, b);
                    st.highlight = Some(fsx::expand_path(was.trim_end_matches('/')));
                    st.fwd.push(was);
                    st.focus = true;
                }
            }
            if crate::ui::glyph_button(ui, Glyph::Forward, "Forward", !st.fwd.is_empty(), &pal).clicked() {
                if let Some(f) = st.fwd.pop() {
                    st.back.push(std::mem::replace(&mut st.input, f));
                    st.focus = true;
                }
            }
            if crate::ui::glyph_button(ui, Glyph::Up, "Up", is_pathlike(&st.input), &pal).clicked() {
                let p = fsx::expand_path(st.input.trim_end_matches('/'));
                if let Some(parent) = p.parent() {
                    st.back.push(st.input.clone());
                    st.highlight = Some(p.clone());
                    st.input = format!("{}/", parent.display()).replace("//", "/");
                    st.focus = true;
                }
            }
            if crate::ui::glyph_button(ui, Glyph::Close, "Close (Esc)", true, &pal).clicked() {
                close = true;
            }
        });
        ui.add_space(2.0);
        let q = if is_pathlike(&st.input) {
            let s = st.input.rsplit('/').next().unwrap_or("").to_string();
            fuzzy::Query::new(&s)
        } else {
            fuzzy::Query::new(&st.input)
        };
        let row_h = 30.0;
        egui::ScrollArea::vertical().max_height(fit_list(ctx, 420.0, 110.0)).auto_shrink([false, true]).show_rows(ui, row_h, st.items.len(), |ui, range| {
            for i in range {
                let it = &st.items[i];
                if it.header {
                    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                    let key = format!("goto:{}", it.label);
                    let collapsed = app.ud.collapsed_groups.contains(&key);
                    let hc = if resp.hovered() { pal.text } else { pal.text_dim };
                    let g = match it.label.as_str() {
                        "Recents" => Glyph::Clock,
                        "Recent destinations" => Glyph::Forward,
                        "Bookmarks" => Glyph::Bookmark,
                        "Storage" => Glyph::Drive,
                        _ => Glyph::Folder,
                    };
                    paint_glyph(ui.painter(), Rect::from_center_size(pos2(r.left() + 16.0, r.center().y), Vec2::splat(14.0)), g, hc);
                    ui.painter().text(pos2(r.left() + 32.0, r.center().y), Align2::LEFT_CENTER, &it.label, FontId::proportional(13.0), hc);
                    let c = pos2(r.right() - 14.0, r.center().y + 2.0);
                    if collapsed {
                        paint_glyph(ui.painter(), Rect::from_center_size(c, Vec2::splat(9.0)), Glyph::ChevronRight, pal.text_dim);
                    } else {
                        ui.painter().add(egui::Shape::line(vec![c + vec2(-3.5, -1.5), c + vec2(0.0, 2.0), c + vec2(3.5, -1.5)], Stroke::new(1.3, pal.text_dim)));
                    }
                    if resp.clicked() {
                        if collapsed {
                            app.ud.collapsed_groups.retain(|g| *g != key);
                        } else {
                            app.ud.collapsed_groups.push(key);
                        }
                        app.ud_dirty = true;
                        st.last = None;
                    }
                    continue;
                }
                let sel = i == st.sel;
                let in_root = st.input.trim().is_empty();
                let resp = row(ui, row_h, sel, &pal, |ui, r| {
                    // Root items are indented under their group header.
                    let x0 = r.left() + if in_root { 30.0 } else { 6.0 };
                    let ic = Rect::from_center_size(pos2(x0 + 10.0, r.center().y), Vec2::splat(16.0));
                    if tab::is_computer(&it.path) || it.usage.is_some() {
                        paint_glyph(ui.painter(), ic, Glyph::Drive, if sel { pal.on_sel() } else { pal.text_dim });
                    } else if !(it.is_dir && crate::icons::dir(ui.painter(), ic, &it.path)) {
                        let ext = it.path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                        theme::paint_icon(ui.painter(), ic, theme::categorize(&ext, it.is_dir, false), "", false);
                    }
                    let tc = if sel { pal.on_sel() } else { pal.text };
                    let tx = x0 + 26.0;
                    let mut job = highlight_job(&it.label, &q, FontId::proportional(13.0), tc, if sel { tc } else { pal.accent });
                    // One line ending in "…" inside the row; with a location hint, keep room for it.
                    let room = r.right() - tx - if it.sub.is_empty() { 8.0 } else { 90.0 };
                    job.wrap = egui::text::TextWrapping { max_width: room.max(40.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
                    let g = ui.fonts_mut(|f| f.layout_job(job));
                    let gw = g.size().x;
                    let ty = if it.usage.is_some() { r.center().y - g.size().y / 2.0 - 4.0 } else { r.center().y - g.size().y / 2.0 };
                    ui.painter().galley(pos2(tx, ty), g, tc);
                    if let Some(u) = it.usage {
                        let bar = Rect::from_min_size(pos2(tx, r.center().y + 6.0), vec2(90.0, 4.0));
                        ui.painter().rect_filled(bar, CornerRadius::same(1), pal.border);
                        ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * u.clamp(0.0, 1.0), bar.height())), CornerRadius::same(1), if sel { pal.on_sel() } else { pal.accent });
                    }
                    if !it.sub.is_empty() {
                        let chev = Rect::from_center_size(pos2(tx + gw + 10.0, r.center().y), Vec2::splat(9.0));
                        let sc = if sel { pal.on_sel_dim() } else { pal.text_dim.gamma_multiply(0.75) };
                        paint_glyph(ui.painter(), chev, Glyph::ChevronRight, sc);
                        let sub = crate::listing::elided(ui, &it.sub, FontId::proportional(11.5), (r.right() - chev.right() - 10.0).max(20.0), sc);
                        ui.painter().galley(pos2(chev.right() + 4.0, r.center().y - sub.size().y / 2.0), sub, sc);
                    }
                });
                if sel && (k.up || k.down || k.page_down || k.page_up) {
                    resp.scroll_to_me(None);
                }
                if resp.clicked() {
                    go = Some((it.path.clone(), it.is_dir, ui.input(|i| i.modifiers.ctrl)));
                }
            }
        });
        if st.items.is_empty() {
            ui.label(egui::RichText::new(if is_pathlike(&st.input) { "No matching entries" } else { "No matches" }).color(pal.text_dim));
        }
        if let Some(d) = &st.dest {
            if let Some(why) = &d.refused {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(why).color(pal.danger));
            }
            hints(ui, &[("↑↓", "navigate"), ("Enter", if d.mv { "move here" } else { "copy here" }), ("Tab", "expand"), ("Esc", "cancel")], &pal);
        } else if is_pathlike(&st.input) {
            hints(ui, &[("↑↓", "navigate"), ("Enter", "use"), ("Tab", "expand"), ("Ctrl+Enter", "open in new tab"), ("Esc", "dismiss")], &pal);
        } else {
            hints(ui, &[("↑↓", "navigate"), ("Enter", "use"), ("Tab", "expand"), ("Esc", "dismiss")], &pal);
        }
    });
    if let (Some((p, ..)), Some(d)) = (&go, &mut st.dest) {
        match crate::copyto::send(app, d, p.clone()) {
            Ok(()) => return false,
            Err(why) => {
                d.refused = Some(why);
                go = None;
            }
        }
    }
    if let Some((p, is_dir, new_tab)) = go {
        let id = app.active;
        if new_tab {
            let (path, select) = if is_dir { (p, None) } else { (p.parent().map(Path::to_path_buf).unwrap_or_else(fsx::home), Some(p)) };
            app.actions.push(Action::NewTab { panel: id, path, select, focus: true });
        } else if is_dir {
            app.actions.push(Action::Navigate { panel: id, path: p });
        } else {
            app.actions.push(Action::OpenFile(p));
        }
        return false;
    }
    !(close || clicked_outside(ctx, rect))
}

// ------------------------------------------------------------------ palette

pub(crate) fn applicable(app: &App, c: Cmd) -> bool {
    let Some(p) = app.panels.get(&app.active) else { return true };
    let t = p.tab();
    let panels = app.panels.len();
    match c {
        Cmd::Back => t.can_back() || t.flatten,
        Cmd::Forward => t.can_forward(),
        Cmd::ClosePanel | Cmd::FocusNextPanel | Cmd::FocusPrevPanel | Cmd::CopyToOther | Cmd::MoveToOther => panels > 1,
        Cmd::InspectorLineNumbers | Cmd::InspectorWrap | Cmd::InspectorMinimap | Cmd::InspectorBlame | Cmd::InspectorFollow => p.inspector.is_some(),
        Cmd::InspectorSource => p.inspector.as_ref().is_some_and(|i| i.rich_kind().is_some()),
        Cmd::GitHistory | Cmd::GitBranches | Cmd::GitChanges | Cmd::GitSearch => crate::gitstatus::repo_for(&t.path).is_some(),
        Cmd::OnlyChanged => t.only_changed || crate::gitstatus::repo_for(&t.path).is_some(),
        Cmd::ComparePanels | Cmd::SelectDifferences | Cmd::SyncPanels => panels > 1,
        Cmd::OpenLinkTarget => t.target_entries().first().map(|e| e.is_link).unwrap_or(false),
        Cmd::RestoreTrash => t.path.starts_with(crate::sidebar::trash_dir()),
        Cmd::Undo => !app.undo.is_empty(),
        Cmd::Transfers => crate::transfers::live(app) > 0,
        Cmd::PasteEarlier => !app.pastes.is_empty() && !tab::is_place(&t.path),
        Cmd::SendToPhone => t.archive.is_none() && !tab::is_place(&t.path) && !t.targets_idx().is_empty(),
        Cmd::Redo => app.undo.can_redo(),
        Cmd::ReopenTab => !app.closed_tabs.is_empty(),
        Cmd::EditTags => t.archive.is_none() && !t.path.starts_with(crate::sidebar::trash_dir()) && crate::musictags::applies(&t.target_entries().iter().map(|e| (e.ext(), e.is_dir)).collect::<Vec<_>>()),
        Cmd::CompressLocked => t.archive.is_none() && !tab::is_place(&t.path) && !t.targets_idx().is_empty(),
        Cmd::BatchRename => t.archive.is_none() && !tab::is_place(&t.path) && !t.targets_idx().is_empty(),
        Cmd::CopyTo => !tab::is_place(&t.path) && !t.targets_idx().is_empty(),
        Cmd::MoveTo => !tab::is_place(&t.path) && t.archive.is_none() && !t.targets_idx().is_empty(),
        Cmd::Rename | Cmd::Copy | Cmd::Cut | Cmd::Trash | Cmd::DeletePermanent | Cmd::Duplicate | Cmd::CreateLink => !t.targets_idx().is_empty(),
        Cmd::LoadLayout | Cmd::DeleteLayout => !app.ud.layouts.is_empty(),
        Cmd::SortFullAsc | Cmd::SortFullDesc => tab::is_computer(&t.path) && !(t.settings.sort == crate::config::SortKey::Usage && t.settings.desc == (c == Cmd::SortFullDesc)),
        Cmd::SortNameAsc | Cmd::SortNameDesc => !(t.settings.sort == crate::config::SortKey::Name && t.settings.desc == (c == Cmd::SortNameDesc)),
        Cmd::SortSizeAsc | Cmd::SortSizeDesc => !(t.settings.sort == crate::config::SortKey::Size && t.settings.desc == (c == Cmd::SortSizeDesc)),
        Cmd::CloseOtherTabs => p.tabs.iter().enumerate().any(|(i, t)| i != p.active && !t.pinned),
        Cmd::CloseRightTabs => p.tabs.iter().skip(p.active + 1).any(|t| !t.pinned),
        Cmd::CloseLeftTabs => p.tabs.iter().take(p.active).any(|t| !t.pinned),
        Cmd::MoveTabRight => p.tabs.get(p.active + 1).is_some_and(|t| t.pinned == p.tab().pinned),
        Cmd::MoveTabLeft => p.active > 0 && p.tabs.get(p.active - 1).is_some_and(|t| t.pinned == p.tab().pinned),
        Cmd::TabNewOrRecent | Cmd::ToggleFlatten | Cmd::OpenTerminal => !tab::is_place(&t.path),
        Cmd::OpenInEditor => !tab::is_place(&t.path) && t.archive.is_none(),
        Cmd::CopyPathRelative => !tab::is_place(&t.path) && t.archive.is_none(),
        Cmd::CopyCodeBlocks => !tab::is_place(&t.path) && t.archive.is_none(),
        Cmd::Email => t.archive.is_none() && !t.target_entries().is_empty() && t.target_entries().iter().all(|e| !e.is_dir),
        Cmd::Print => t.archive.is_none() && !t.target_entries().is_empty() && t.target_entries().iter().all(|e| !e.is_dir && crate::sendto::printable(&e.name, e.ext())) && crate::drives::Tools::current().is_some_and(|t| crate::sendto::cups(&t)),
        Cmd::RotateLeft | Cmd::RotateRight | Cmd::ResizeHalf | Cmd::ResizeQuarter | Cmd::ResizeFit | Cmd::ResizeCustom => t.archive.is_none() && !t.path.starts_with(crate::sidebar::trash_dir()) && !t.target_entries().is_empty() && t.target_entries().iter().all(|e| !e.is_dir && crate::picfix::is_picture(e.ext())) && crate::drives::Tools::current().is_some_and(|t| crate::picfix::magick(&t).is_some()),
        Cmd::CropPicture => t.archive.is_none() && !t.path.starts_with(crate::sidebar::trash_dir()) && matches!(t.target_entries().as_slice(), [e] if !e.is_dir && crate::picfix::can_crop(e.ext())) && crate::drives::Tools::current().is_some_and(|t| crate::picfix::magick(&t).is_some()),
        Cmd::ConvertJpg | Cmd::ConvertPng | Cmd::ConvertWebp | Cmd::ConvertPdf | Cmd::ConvertMp3 | Cmd::ConvertOpus | Cmd::ConvertFlac | Cmd::ConvertMp4 | Cmd::ConvertWebm | Cmd::ConvertSoundMp3 | Cmd::MergePdfs | Cmd::SplitPdf => crate::convert::Target::of(c).is_some_and(|to| crate::convert::applicable(app, to)),
        Cmd::SetWallpaper => t.archive.is_none() && matches!(t.target_entries().as_slice(), [e] if !e.is_dir && crate::wallpaper::is_wallpaper(e.ext())),
        Cmd::CompareFiles => t.archive.is_none() && t.target_entries().iter().filter(|e| !e.is_dir).count() == 2 && t.target_entries().len() == 2,
        Cmd::CopyWebLink | Cmd::OpenWebLink | Cmd::GitIgnore => t.archive.is_none() && crate::gitstatus::repo_for(&t.path).is_some(),
        Cmd::GitIgnoreExt => t.archive.is_none() && crate::gitstatus::repo_for(&t.path).is_some() && crate::share::shared_ext(t.target_entries().iter().map(|e| (e.path.as_path(), e.is_dir))).is_some(),
        Cmd::CodeSearch | Cmd::FindTodos => !tab::is_place(&t.path) && t.archive.is_none(),
        Cmd::RunTask => !tab::is_place(&t.path) && t.archive.is_none(),
        Cmd::SaveSmart => !t.filter.trim().is_empty() && !t.content.on && t.smart.is_none() && !tab::is_place(&t.path) && t.archive.is_none(),
        Cmd::WhatsUsing => t.archive.is_none() && (!tab::is_place(&t.path) || t.target_entries().iter().any(|e| tab::drive_dev(&e.path).is_none())),
        Cmd::AddToShelf => t.archive.is_none() && !tab::is_place(&t.path) && !t.targets_idx().is_empty(),
        Cmd::ShelfMoveHere | Cmd::ShelfCopyHere => !app.shelf.items.is_empty() && t.archive.is_none() && !tab::is_place(&t.path),
        Cmd::ClearShelf => !app.shelf.items.is_empty(),
        Cmd::TidyFolder => !tab::is_place(&t.path) && t.archive.is_none() && !t.path.starts_with(crate::sidebar::trash_dir()),
        Cmd::CleanBuilds | Cmd::FindDuplicates | Cmd::SpaceUsed | Cmd::FreeUpSpace | Cmd::ViewPictures | Cmd::ViewCalendar => !tab::is_place(&t.path) && t.archive.is_none(),
        Cmd::PreviousVersions => t.archive.is_none() && !t.path.starts_with(crate::sidebar::trash_dir()) && matches!(t.target_entries().as_slice(), [e] if !e.is_dir),
        Cmd::CopyPathLine => p.line_hint.as_ref().is_some_and(|h| t.target_entries().iter().any(|e| e.path == h.0)),
        Cmd::OpenTabInWindow => p.tabs.len() > 0,
        Cmd::GroupDeleted | Cmd::GroupOriginal => crate::trashcan::is_trash_files(&t.path),
        Cmd::GroupFree | Cmd::GroupFs | Cmd::GroupFull => tab::is_computer(&t.path),
        Cmd::GroupItems | Cmd::GroupUsage | Cmd::GroupName | Cmd::GroupType | Cmd::GroupDate | Cmd::GroupCreated | Cmd::GroupAccessed | Cmd::GroupSize => !tab::is_place(&t.path) && !t.tree_active(),
        Cmd::GroupTag => !tab::is_place(&t.path) && !t.tree_active(),
        Cmd::SortTags | Cmd::SortTaken | Cmd::SortDimensions | Cmd::SortDuration | Cmd::SortColour => !tab::is_place(&t.path),
        Cmd::GroupTaken | Cmd::GroupOrigin => !tab::is_place(&t.path) && !t.tree_active(),
        Cmd::GroupExpandAll => t.settings.group != crate::config::GroupBy::None && !t.collapsed_groups.is_empty(),
        Cmd::GroupCollapseAll => t.settings.group != crate::config::GroupBy::None,
        _ => true,
    }
}

/// A menu row for a command: dimmed (and inert) when it can't apply.
fn cmd_row(ui: &mut Ui, app: &App, c: Cmd) -> bool {
    cmd_row_as(ui, app, c, c.info().title)
}

/// `cmd_row` under another label, for a command whose name follows its state.
fn cmd_row_as(ui: &mut Ui, app: &App, c: Cmd, title: &str) -> bool {
    let pal = app.pal;
    let enabled = applicable(app, c);
    let chips: Vec<String> = app.keymap.bindings(c).iter().take(3).map(commands::binding_to_string).collect();
    let resp = row(ui, 26.0, false, &pal, |ui, r| {
        ui.painter().text(pos2(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, title, FontId::proportional(13.0), if enabled { pal.text } else { pal.text_dim.gamma_multiply(0.6) });
        let mut x = r.right() - 6.0;
        for ch in chips.iter().rev() {
            x -= crate::ui::paint_chip(ui.painter(), pos2(x, r.center().y), ch, &pal) + 3.0;
        }
    });
    enabled && resp.clicked()
}

fn palette(app: &mut App, ctx: &egui::Context, st: &mut PaletteState) -> bool {
    let mut toggle_compact = false;
    let pal = app.pal;
    // ---------------- hotkey capture
    if let Some(cap) = st.capture.as_mut() {
        let events = ctx.input(|i| i.events.clone());
        let mods = ctx.input(|i| i.modifiers);
        let mut finish = false;
        let mut cancel = false;
        for ev in &events {
            match ev {
                Event::Key { key, pressed: true, modifiers, repeat: false, .. } => match key {
                    Key::Escape if modifiers.is_none() => cancel = true,
                    Key::Enter if modifiers.is_none() && !cap.seq.is_empty() => finish = true,
                    Key::Backspace if modifiers.is_none() => cap.seq.clear(),
                    _ => {
                        cap.seq.push(Chord::from_key(*key, *modifiers));
                        cap.last = Instant::now();
                    }
                },
                Event::Copy => cap.seq.push(Chord::from_key(Key::C, Modifiers { ctrl: true, shift: mods.shift, ..Default::default() })),
                Event::Cut => cap.seq.push(Chord::from_key(Key::X, Modifiers { ctrl: true, shift: mods.shift, ..Default::default() })),
                Event::Paste(_) => cap.seq.push(Chord::from_key(Key::V, Modifiers { ctrl: true, shift: mods.shift, ..Default::default() })),
                Event::PointerButton { button, pressed: true, modifiers, .. } => {
                    if let Some(c) = Chord::from_mouse(*button, *modifiers) {
                        cap.seq.push(c);
                        cap.last = Instant::now();
                    }
                }
                _ => {}
            }
        }
        ctx.input_mut(|i| i.events.clear());
        if !cap.seq.is_empty() && cap.last.elapsed().as_secs_f32() > 1.2 {
            finish = true;
        }
        if !cap.seq.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        let cmd = cap.cmd;
        let seq_txt = commands::binding_to_string(&cap.seq);
        centered(ctx, "capture", 420.0, &pal, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(6.0);
                ui.label(egui::RichText::new(format!("New hotkey for “{}”", cmd.info().title)).strong());
                ui.add_space(10.0);
                ui.label(egui::RichText::new(if seq_txt.is_empty() { "Press keys…".to_string() } else { seq_txt.clone() }).size(20.0).color(pal.accent));
                ui.add_space(10.0);
                hints(ui, &[("Enter", "save"), ("Backspace", "clear"), ("Esc", "cancel")], &pal);
                ui.label(egui::RichText::new("Sequences (e.g. G then H) are allowed; pause to finish.").small().color(pal.text_dim));
            });
        });
        if cancel {
            st.capture = None;
        } else if finish {
            let binding = cap.seq.clone();
            st.capture = None;
            match app.keymap.conflicts(&binding, cmd) {
                Some(other) => st.conflict = Some((cmd, binding, other)),
                None => add_binding(app, cmd, binding),
            }
        }
        return true;
    }
    if let Some((cmd, binding, other)) = st.conflict.clone() {
        let mut keep_open = true;
        centered(ctx, "hk_conflict", 440.0, &pal, |ui| {
            ui.label(format!("{} is already bound to “{}”.", commands::binding_to_string(&binding), other.info().title));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Replace").clicked() {
                    remove_binding(app, other, &binding);
                    add_binding(app, cmd, binding.clone());
                    st.conflict = None;
                }
                if ui.button("Add anyway").clicked() {
                    add_binding(app, cmd, binding.clone());
                    st.conflict = None;
                }
                if ui.button("Cancel").clicked() {
                    st.conflict = None;
                }
            });
        });
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            st.conflict = None;
        }
        let _ = &mut keep_open;
        return true;
    }
    // ---------------- list
    // Shift+Tab before the plain keys, which would take it for Tab.
    let (prev_cat, next_cat, quick_key) = ctx.input_mut(|i| {
        (
            i.consume_key(Modifiers::SHIFT, Key::Tab) | i.consume_key(Modifiers::CTRL, Key::PageUp),
            i.consume_key(Modifiers::CTRL, Key::PageDown),
            i.consume_key(Modifiers::CTRL, Key::D),
        )
    });
    let k = take_keys(ctx);
    if k.esc {
        return false;
    }
    let next_cat = next_cat || k.tab;
    let q = fuzzy::Query::new(&st.query);
    let searching = !q.is_empty();
    let hits = if searching { find_commands(&app.ud, &st.query) } else { vec![] };
    let quick: Vec<Cmd> = app.ud.quick.iter().filter_map(|id| Cmd::from_id(id)).collect();
    let in_cat = |c: Cmd, cat: Cat| if cat == Cat::Quick { quick.contains(&c) } else { c.info().cat == cat };
    let count = |cat: Option<Cat>| match cat {
        None => hits.len(),
        Some(cat) => hits.iter().filter(|&&c| in_cat(c, cat)).count(),
    };
    // The left column: everything, then each category. Tab steps through it, past the
    // ones a search leaves empty.
    let rail: Vec<Option<Cat>> = std::iter::once(None).chain(Cat::ORDER.into_iter().map(Some)).collect();
    if prev_cat || next_cat {
        let open: Vec<Option<Cat>> = rail.iter().copied().filter(|&r| r.is_none() || !searching || count(r) > 0).collect();
        let i = open.iter().position(|&r| r == st.cat).unwrap_or(0) as isize;
        st.cat = open[(i + if next_cat { 1 } else { -1 }).rem_euclid(open.len() as isize) as usize];
        st.sel = 0;
        st.top = true;
    }
    // What the list shows, as headed groups (an empty heading has none).
    let mut groups: Vec<(&str, Vec<Cmd>)> = vec![];
    match (searching, st.cat) {
        (true, cat) => groups.push(("", hits.iter().copied().filter(|&c| cat.is_none_or(|cat| in_cat(c, cat))).collect())),
        (false, Some(cat)) => groups.push(("", ALL.iter().copied().filter(|&c| in_cat(c, cat)).collect())),
        (false, None) => {
            groups.push(("Recent", app.ud.recent_cmds.iter().filter_map(|id| Cmd::from_id(id)).collect()));
            groups.push(("Quick", quick.clone()));
            for cat in Cat::ORDER.into_iter().filter(|&c| c != Cat::Quick) {
                groups.push((cat.name(), ALL.iter().copied().filter(|c| c.info().cat == cat).collect()));
            }
        }
    }
    groups.retain(|g| !g.1.is_empty());
    let flat: Vec<Cmd> = groups.iter().flat_map(|g| g.1.iter().copied()).collect();
    let before = st.sel;
    step(&mut st.sel, flat.len(), &k, |_| false);
    if st.sel != before || k.up || k.down {
        st.scroll = true;
    }
    let chosen = flat.get(st.sel).copied();
    let mut act: Option<(Cmd, RowAct)> = None;
    // F2 names the selected command's alias, as it renames a file.
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F2)) {
        act = chosen.map(|c| (c, RowAct::Alias));
    } else if quick_key {
        act = chosen.map(|c| (c, RowAct::Star));
    } else if k.ctrl_enter {
        act = chosen.map(|c| (c, RowAct::AddKey));
    } else if k.enter {
        act = chosen.map(|c| (c, RowAct::Run));
    }
    let compact = app.cfg.compact_lists;
    let row_h = if compact { 28.0 } else { 34.0 };
    let card_fill = mix(pal.raised, pal.bg, 0.45);
    let card_line = pal.border.gamma_multiply(0.8);
    let mut close = false;
    let mut toggle_compact = false;
    let mut go_cat: Option<Option<Cat>> = None;
    let mut shown: Option<Cmd> = None;
    // One size whatever is listed, so the window does not jump while typing or
    // between categories; it shrinks with the window.
    let screen = ctx.content_rect();
    let top = popup_top(screen);
    let w = 760f32.min(screen.width() - 24.0).round();
    let h = 540f32.min(screen.height() - top - 12.0).max(160.0).round();
    let pos = pos2((screen.center().x - w / 2.0).round(), screen.top() + top);
    note_layer(Id::new("palette"));
    let area = egui::Area::new(Id::new("palette")).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
        crate::frost::behind(ui, 10);
        popup_frame(&pal).show(ui, |ui| {
            let (_, inner) = ui.allocate_space(vec2(w - 16.0, h - 16.0));
            let top_down = egui::Layout::top_down(egui::Align::Min);
            let mut head = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(top_down));
            head.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if crate::ui::glyph_button(ui, Glyph::Close, "Close (Esc)", true, &pal).clicked() {
                    close = true;
                }
                let cb = compact_button(ui, compact, &pal);
                crate::ui::note_control(ui, cb.rect, "compact rows");
                if cb.clicked() {
                    toggle_compact = true;
                }
                let before = st.query.clone();
                let fw = (ui.available_width() - 4.0).floor();
                ui.allocate_ui_with_layout(vec2(fw, 0.0), top_down, |ui| {
                    ui.set_width(fw);
                    search_field(ui, &mut st.query, "Select a command…", &mut st.focus, &pal, Glyph::Search);
                });
                if st.query != before {
                    st.sel = 0;
                    st.top = true;
                }
            });
            let narrow = inner.width() < 600.0;
            let foot = Rect::from_min_max(pos2(inner.left(), inner.bottom() - 22.0), inner.max);
            let keys: &[(&str, &str)] = if narrow {
                &[("↑↓", "move"), ("Tab", "category"), ("Enter", "run"), ("Esc", "close")]
            } else {
                &[("↑↓", "move"), ("Tab", "category"), ("Enter", "run"), ("Ctrl+D", "quick"), ("Ctrl+Enter", "add hotkey"), ("F2", "alias"), ("Esc", "close")]
            };
            hint_row(ui, foot, keys, &pal);
            let about = Rect::from_min_max(pos2(inner.left(), foot.top() - 26.0), pos2(inner.right(), foot.top() - 4.0));
            let rule = about.top().round() - 0.5;
            ui.painter().line_segment([pos2(inner.left(), rule), pos2(inner.right(), rule)], Stroke::new(1.0, pal.border));
            let body = Rect::from_min_max(pos2(inner.left(), head.min_rect().bottom().round() + 2.0), pos2(inner.right(), rule - 6.0));
            // A narrow window keeps only the categories' glyphs.
            let side = Rect::from_min_max(body.min, pos2(body.left() + if narrow { 36.0 } else { 168.0 }, body.bottom()));
            let lx = side.right().round() + 6.5;
            ui.painter().line_segment([pos2(lx, body.top()), pos2(lx, body.bottom())], Stroke::new(1.0, pal.border));
            let main = Rect::from_min_max(pos2(lx + 7.5, body.top()), body.max);
            let mut sui = ui.new_child(egui::UiBuilder::new().max_rect(side).layout(top_down));
            egui::ScrollArea::vertical().id_salt("palette categories").max_height(side.height()).auto_shrink([false, false]).show(&mut sui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                for &rc in &rail {
                    let on = rc == st.cat;
                    let n = count(rc);
                    // While searching, categories without a match step back.
                    let quiet = searching && n == 0;
                    let name = rc.map_or("All", |c| c.name());
                    let resp = row(ui, 30.0, on, &pal, |ui, r| {
                        let faint = pal.text_dim.gamma_multiply(0.55);
                        let (gc, tc) = if on { (pal.on_sel(), pal.on_sel()) } else if quiet { (faint, faint) } else { (pal.text_dim, pal.text) };
                        let gx = if narrow { r.center().x } else { r.left() + 16.0 };
                        let gr = Rect::from_center_size(pos2(gx.round(), r.center().y.round()), Vec2::splat(13.0));
                        match rc {
                            Some(c) => paint_cat_glyph(ui.painter(), gr, c, gc),
                            None => paint_glyph(ui.painter(), gr, Glyph::List, gc),
                        }
                        if !narrow {
                            let mut right = r.right() - 8.0;
                            if searching && n > 0 {
                                let ng = ui.painter().layout_no_wrap(n.to_string(), FontId::proportional(11.0), if on { pal.on_sel_dim() } else { pal.text_dim });
                                right -= ng.size().x;
                                ui.painter().galley(pos2(right, (r.center().y - ng.size().y / 2.0).round()), ng, pal.text_dim);
                                right -= 6.0;
                            }
                            let g = crate::listing::elided(ui, name, FontId::proportional(13.0), right - r.left() - 34.0, tc);
                            ui.painter().galley(pos2(r.left() + 34.0, (r.center().y - g.size().y / 2.0).round()), g, tc);
                        }
                    });
                    crate::ui::note_control(ui, resp.rect, rc.map_or("category All", |c| c.control()));
                    let resp = if narrow { resp.on_hover_text(name) } else { resp };
                    if resp.clicked() {
                        go_cat = Some(rc);
                    }
                }
            });
            let mut mui = ui.new_child(egui::UiBuilder::new().max_rect(main).layout(top_down));
            // Back to the top by scrolling there: a jump would lose to a scroll still
            // under way from the keys before.
            let mut top = std::mem::take(&mut st.top);
            egui::ScrollArea::vertical().id_salt("palette list").max_height(main.height()).auto_shrink([false, false]).show(&mut mui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                // Room on the right for the scroll bar.
                let cw = (ui.available_width() - 10.0).floor();
                let mut vi = 0;
                for (gi, (heading, cmds)) in groups.iter().enumerate() {
                    if gi > 0 {
                        ui.add_space(12.0);
                    }
                    if !heading.is_empty() {
                        let (hr, _) = ui.allocate_exact_size(vec2(cw, 20.0), Sense::hover());
                        ui.painter().text(pos2(hr.left() + 4.0, hr.center().y - 1.0), Align2::LEFT_CENTER, *heading, FontId::proportional(11.5), pal.text_dim);
                        if std::mem::take(&mut top) {
                            ui.scroll_to_rect(hr, Some(egui::Align::TOP));
                            st.scroll = false;
                        }
                    }
                    let (card, _) = ui.allocate_exact_size(vec2(cw, cmds.len() as f32 * row_h), Sense::hover());
                    if std::mem::take(&mut top) {
                        ui.scroll_to_rect(card, Some(egui::Align::TOP));
                        st.scroll = false;
                    }
                    ui.painter().rect(card, CornerRadius::same(8), card_fill, Stroke::new(1.0, card_line), StrokeKind::Inside);
                    for (j, &c) in cmds.iter().enumerate() {
                        let r = Rect::from_min_size(card.min + vec2(0.0, j as f32 * row_h), vec2(cw, row_h));
                        let sel = vi == st.sel;
                        if sel && std::mem::take(&mut st.scroll) {
                            ui.scroll_to_rect(r, None);
                        }
                        let look = CmdLook { sel, first: j == 0, cat: searching && st.cat.is_none(), compact };
                        let (a, hovered) = palette_row(ui, app, c, r, vi, &look, &q, &pal);
                        if hovered {
                            shown = Some(c);
                        }
                        if let Some(a) = a {
                            if matches!(a, RowAct::Run) {
                                st.sel = vi;
                            }
                            act = Some((c, a));
                        }
                        vi += 1;
                    }
                }
                if flat.is_empty() {
                    ui.add_space(28.0);
                    let msg = if searching { "No command matches" } else { "Star a command, or press Ctrl+D on it, to keep it here." };
                    ui.vertical_centered(|ui| ui.label(egui::RichText::new(msg).size(12.5).color(pal.text_dim)));
                }
                ui.add_space(6.0);
            });
            st.scroll = false;
            // What the command under the pointer, or else the selected one, does.
            if let Some(c) = shown.or(chosen) {
                let mut dj = highlight_job(c.info().desc, &q, FontId::proportional(12.0), pal.text_dim, pal.accent.gamma_multiply(0.85));
                dj.wrap = egui::text::TextWrapping { max_width: about.width() - 8.0, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
                let dg = ui.fonts_mut(|f| f.layout_job(dj));
                ui.painter().galley(pos2(about.left() + 4.0, (about.center().y - dg.size().y / 2.0).round()), dg, pal.text_dim);
            }
        });
    });
    let rect = area.response.rect;
    crate::ui::note_popup(rect);
    if let Some(rc) = go_cat {
        st.cat = rc;
        st.sel = 0;
        st.top = true;
    }
    // A click anywhere in it leaves the keyboard with the search, so typing goes on.
    if ctx.input(|i| i.pointer.any_click()) && ctx.pointer_interact_pos().is_some_and(|p| rect.contains(p)) {
        st.focus = true;
    }
    if toggle_compact {
        app.cfg.compact_lists = !app.cfg.compact_lists;
        app.set_config_changed();
    }
    match act {
        Some((c, RowAct::Star)) => {
            let id = c.info().id.to_string();
            if let Some(i) = app.ud.quick.iter().position(|q| *q == id) {
                app.ud.quick.remove(i);
            } else {
                app.ud.quick.push(id);
            }
            app.ud_dirty = true;
        }
        Some((c, RowAct::Alias)) => {
            let text = app.ud.aliases.get(c.info().id).cloned().unwrap_or_default();
            edit_alias(app, c, text, format!("Alias for “{}” (empty removes it)", c.info().title));
            return false;
        }
        Some((c, RowAct::AddKey)) => {
            st.capture = Some(Capture { cmd: c, seq: vec![], last: Instant::now() });
            return true;
        }
        Some((c, RowAct::Remove(b))) => remove_binding(app, c, &b),
        Some((c, RowAct::Reset)) => {
            app.cfg.hotkeys.remove(c.info().id);
            app.set_config_changed();
        }
        Some((c, RowAct::Run)) => {
            if applicable(app, c) {
                let id = c.info().id.to_string();
                app.ud.recent_cmds.retain(|r| *r != id);
                app.ud.recent_cmds.insert(0, id);
                app.ud.recent_cmds.truncate(5);
                app.ud_dirty = true;
                app.actions.push(Action::Cmd(c));
            }
            return false;
        }
        None => {}
    }
    !(close || clicked_outside(ctx, rect))
}

/// What a click on a palette row (or its key) asks for.
enum RowAct {
    Run,
    Star,
    Alias,
    AddKey,
    Remove(Binding),
    Reset,
}

struct CmdLook {
    sel: bool,
    first: bool,
    /// Search results across every category name theirs.
    cat: bool,
    compact: bool,
}

/// One command in the palette: its title (and alias) on the left, its hotkeys on the
/// right, and under the pointer or the selection the star, alias and add-hotkey
/// buttons beside them. Returns what was clicked and whether the pointer is over it.
#[allow(clippy::too_many_arguments)]
fn palette_row(ui: &mut Ui, app: &App, c: Cmd, r: Rect, i: usize, look: &CmdLook, q: &fuzzy::Query, pal: &Palette) -> (Option<RowAct>, bool) {
    let info = c.info();
    let enabled = applicable(app, c);
    let starred = app.ud.quick.iter().any(|q| q == info.id);
    let overridden = app.cfg.hotkeys.contains_key(info.id);
    if !look.first {
        let y = r.top().round() + 0.5;
        ui.painter().line_segment([pos2(r.left() + 12.0, y), pos2(r.right() - 12.0, y)], Stroke::new(1.0, pal.border.gamma_multiply(0.7)));
    }
    let resp = ui.interact(r, Id::new(("palette row", i)), Sense::click());
    let hovered = resp.hovered() || ui.rect_contains_pointer(r);
    let inside = r.shrink(2.0);
    if look.sel {
        ui.painter().rect_filled(inside, CornerRadius::same(6), pal.selected);
    } else if hovered {
        ui.painter().rect_filled(inside, CornerRadius::same(6), pal.hover);
    }
    // The dim grey and the accent sink into a saturated selection.
    let (dim, acc) = if look.sel { (pal.on_sel_dim(), pal.on_sel()) } else { (pal.text_dim, pal.accent) };
    let tcol = if !enabled { dim.gamma_multiply(0.7) } else if look.sel { pal.on_sel() } else { pal.text };
    let cy = r.center().y;
    let mut out = None;
    // Right to left: hotkeys, then the row's buttons while it is pointed at or chosen.
    let mut x = r.right() - 10.0;
    for b in app.keymap.bindings(c).iter().rev() {
        let label = commands::binding_to_string(b);
        let w = crate::ui::paint_chip(ui.painter(), pos2(x, cy), &label, pal);
        let cr = Rect::from_min_max(pos2(x - w, cy - 9.0), pos2(x, cy + 9.0));
        if ui.interact(cr, Id::new(("chip", i, &label)), Sense::click()).on_hover_text("Click to remove this hotkey").clicked() {
            out = Some(RowAct::Remove(b.clone()));
        }
        x -= w + 4.0;
    }
    if look.sel || hovered {
        if overridden {
            let rr = Rect::from_center_size(pos2(x - 10.0, cy), Vec2::splat(20.0));
            let rresp = ui.interact(rr, Id::new(("reset", i)), Sense::click()).on_hover_text("Reset hotkeys to default");
            paint_glyph(ui.painter(), rr.shrink(3.0), Glyph::Refresh, if rresp.hovered() { tcol } else { dim });
            if rresp.clicked() {
                out = Some(RowAct::Reset);
            }
            x -= 24.0;
        }
        let pr = Rect::from_center_size(pos2(x - 10.0, cy), Vec2::splat(20.0));
        let presp = ui.interact(pr, Id::new(("addkey", i)), Sense::click()).on_hover_text("Add a hotkey (Ctrl+Enter)");
        if presp.hovered() {
            ui.painter().rect_filled(pr, CornerRadius::same(4), pal.raised);
        }
        paint_glyph(ui.painter(), pr.shrink(4.0), Glyph::Plus, dim);
        if presp.clicked() {
            out = Some(RowAct::AddKey);
        }
        x -= 24.0;
        let ag = ui.painter().layout_no_wrap("alias".into(), FontId::proportional(11.0), dim);
        let ar = Rect::from_min_max(pos2(x - ag.size().x - 8.0, cy - 10.0), pos2(x, cy + 10.0));
        crate::ui::note_control(ui, ar, "alias button");
        let aresp = ui.interact(ar, Id::new(("alias", i)), Sense::click()).on_hover_text("Set the alias that finds this command (F2)");
        if aresp.hovered() {
            ui.painter().rect_filled(ar, CornerRadius::same(4), pal.raised);
        }
        ui.painter().galley(pos2(ar.left() + 4.0, cy - ag.size().y / 2.0), ag, dim);
        if aresp.clicked() {
            out = Some(RowAct::Alias);
        }
        x = ar.left() - 4.0;
        let sr = Rect::from_center_size(pos2(x - 10.0, cy), Vec2::splat(20.0));
        crate::ui::note_control(ui, sr, "star button");
        let sresp = ui.interact(sr, Id::new(("star", i)), Sense::click()).on_hover_text(if starred { "Remove from Quick (Ctrl+D)" } else { "Add to Quick (Ctrl+D)" });
        crate::ui::star(ui.painter(), sr.center(), 6.5, if starred { acc } else { dim }, starred);
        if sresp.clicked() {
            out = Some(RowAct::Star);
        }
        x -= 22.0;
    } else if starred {
        crate::ui::star(ui.painter(), pos2(x - 10.0, cy), 5.0, pal.accent.gamma_multiply(0.7), true);
        x -= 22.0;
    }
    // The title takes what is left, cut short before the buttons.
    let tx = r.left() + 12.0;
    let room = (x - 8.0 - tx).max(24.0);
    let font = FontId::proportional(if look.compact { 13.0 } else { 13.5 });
    let mut job = highlight_job(info.title, q, font, tcol, acc);
    if let Some(a) = app.ud.aliases.get(info.id) {
        job.append(&format!("“{a}”"), 8.0, egui::TextFormat { font_id: FontId::proportional(12.0), color: dim, ..Default::default() });
    }
    job.wrap = egui::text::TextWrapping { max_width: room, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let g = ui.fonts_mut(|f| f.layout_job(job));
    let tw = g.size().x;
    ui.painter().galley(pos2(tx, (cy - g.size().y / 2.0).round()), g, tcol);
    if look.cat && tw + 60.0 < room {
        let cg = crate::listing::elided(ui, info.cat.name(), FontId::proportional(11.5), room - tw - 12.0, dim);
        ui.painter().galley(pos2(tx + tw + 10.0, (cy - cg.size().y / 2.0).round()), cg, dim);
    }
    if out.is_none() && resp.clicked() {
        out = Some(RowAct::Run);
    }
    (out, hovered)
}

/// Commands matching `query`, best first. An alias typed in full comes first, then the
/// title (whole, prefix, every word started, a word, substring, then letters taken
/// from the starts of its words, "tohi" for Toggle Hidden), then the category's name
/// with the title, then a word of the description. Scattered letters match nothing:
/// "git" found Quit through the G of General.
pub(crate) fn find_commands(ud: &crate::config::UserData, query: &str) -> Vec<Cmd> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return vec![];
    }
    let words: Vec<&str> = needle.split_whitespace().collect();
    let letters: Vec<char> = needle.chars().filter(|c| !c.is_whitespace()).collect();
    let split = |t: &str| -> Vec<String> { t.split(|ch: char| !ch.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect() };
    let all_started = |tw: &[String]| words.iter().all(|w| tw.iter().any(|x| x.starts_with(w)));
    let mut ranked: Vec<(i32, usize, Cmd)> = ALL
        .iter()
        .enumerate()
        .filter_map(|(n, &c)| {
            let i = c.info();
            let t = i.title.to_lowercase();
            let tw = split(&t);
            let r = if ud.aliases.get(i.id).is_some_and(|a| a.eq_ignore_ascii_case(&needle)) {
                5000
            } else if t == needle {
                4000
            } else if t.starts_with(&needle) {
                3000
            } else if words.len() > 1 && all_started(&tw) {
                2700
            } else if tw.iter().any(|x| x.starts_with(&needle)) {
                2500
            } else if t.contains(&needle) {
                2000
            } else if word_starts(&letters, &tw) {
                1000
            } else if all_started(&[split(&i.cat.name().to_lowercase()), tw].concat()) {
                700
            } else if split(&i.desc.to_lowercase()).iter().any(|x| x.starts_with(&needle)) || (words.len() > 1 && i.desc.to_lowercase().contains(&needle)) {
                500
            } else {
                return None;
            };
            Some((r, n, c))
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    ranked.into_iter().map(|(_, _, c)| c).collect()
}

/// Whether `q` is the start of one word of `words`, then of a later one, and so on.
fn word_starts(q: &[char], words: &[String]) -> bool {
    if q.is_empty() {
        return true;
    }
    words.iter().enumerate().any(|(wi, w)| {
        let w: Vec<char> = w.chars().collect();
        (1..=q.len().min(w.len())).rev().any(|len| w[..len] == q[..len] && word_starts(&q[len..], &words[wi + 1..]))
    })
}

/// Set, change or (with empty text) clear a command's alias. An alias another command
/// already answers to is refused: typing it could only ever find one of them first.
pub fn set_alias(ud: &mut crate::config::UserData, c: Cmd, text: &str) -> Result<(), Cmd> {
    let (id, t) = (c.info().id, text.trim());
    if let Some(other) = ud.aliases.iter().filter(|(k, a)| *k != id && a.eq_ignore_ascii_case(t)).find_map(|(k, _)| Cmd::from_id(k)) {
        return Err(other);
    }
    if t.is_empty() {
        ud.aliases.remove(id);
    } else {
        ud.aliases.insert(id.to_string(), t.to_string());
    }
    Ok(())
}

/// The alias prompt; a refused alias brings it back with the reason, and saving returns
/// to the palette.
fn edit_alias(app: &mut App, c: Cmd, text: String, title: String) {
    app.popup = Popup::prompt(&title, text, Box::new(move |app: &mut App, t: String| match set_alias(&mut app.ud, c, &t) {
        Ok(()) => {
            app.ud_dirty = true;
            app.popup = Popup::palette();
        }
        Err(o) => edit_alias(app, c, t.clone(), format!("“{}” already finds “{}”: pick another for “{}”", t.trim(), o.info().title, c.info().title)),
    }));
}

fn current_bindings(app: &App, c: Cmd) -> Vec<String> {
    app.keymap.bindings(c).iter().map(commands::binding_to_string).collect()
}

fn add_binding(app: &mut App, c: Cmd, b: Binding) {
    let mut v = current_bindings(app, c);
    let s = commands::binding_to_string(&b);
    if !v.contains(&s) {
        v.push(s);
    }
    app.cfg.hotkeys.insert(c.info().id.to_string(), v);
    app.set_config_changed();
}

fn remove_binding(app: &mut App, c: Cmd, b: &Binding) {
    let s = commands::binding_to_string(b);
    let v: Vec<String> = current_bindings(app, c).into_iter().filter(|x| *x != s).collect();
    app.cfg.hotkeys.insert(c.info().id.to_string(), v);
    app.set_config_changed();
}

// ------------------------------------------------------------------ options

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Opt {
    Theme,
    Font,
    InspectorFontSize,
    LineSpacing,
    RectButtons,
    AssetCache,
    FollowSystem,
    SyntaxTheme,
    Round,
    FontSize,
    Spacing,
    Animations,
    Tooltips,
    AltRows,
    InactiveOpacity,
    Breadcrumbs,
    CompactPaths,
    ShowExt,
    ShowHidden,
    ShowSystem,
    FoldersFirst,
    HighlightRecents,
    RecentsOnTop,
    TrackRecents,
    CalcSizes,
    SizesSsd,
    SizesHdd,
    SizesNet,
    SelectionBoxes,
    InspectorFont,
    SystemIcons,
    Thumbnails,
    ThumbFill,
    ScrollBeyond,
    DateFormat,
    ActOnPress,
    SingleClick,
    SingleClickFolders,
    OpenFoldersNewTab,
    DblClickParent,
    PictureViewer,
    ReverseScroll,
    CharKey,
    MiddleClick,
    InspectorLayout,
    Startup,
    NewTab,
    GitPictures,
    IgnoredFiles,
    TrashAge,
    DefaultFm,
    Picker,
    SingleInstance,
    Meaning,
    MeaningFolders,
    MeaningPictures,
    ConfigFile,
    Updates,
    ResetAll,
    Install,
    Editor,
    Tour,
    CustomActions,
    CheckCopies,
}

/// The pages of Options, in the order of its left column.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Section {
    Appearance,
    Files,
    Mouse,
    Preview,
    Search,
    Git,
    System,
}

impl Section {
    pub(crate) const ALL: [Section; 7] = [Section::Appearance, Section::Files, Section::Mouse, Section::Preview, Section::Search, Section::Git, Section::System];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Files => "Files & folders",
            Section::Mouse => "Mouse & keyboard",
            Section::Preview => "Preview",
            Section::Search => "Search",
            Section::Git => "Git",
            Section::System => "Startup & system",
        }
    }

    /// The name the simulations click it by.
    pub(crate) fn control(self) -> &'static str {
        match self {
            Section::Appearance => "section Appearance",
            Section::Files => "section Files & folders",
            Section::Mouse => "section Mouse & keyboard",
            Section::Preview => "section Preview",
            Section::Search => "section Search",
            Section::Git => "section Git",
            Section::System => "section Startup & system",
        }
    }

    /// The section `d` places away, wrapping at either end.
    pub(crate) fn step(self, d: isize) -> Section {
        let i = Section::ALL.iter().position(|s| *s == self).unwrap_or(0) as isize;
        Section::ALL[(i + d).rem_euclid(Section::ALL.len() as isize) as usize]
    }

    fn paint(self, p: &egui::Painter, r: Rect, col: Color32) {
        match self {
            // Half a disc: light and dark.
            Section::Appearance => {
                let (c, rad) = (r.center(), r.width() * 0.4);
                p.circle_stroke(c, rad, Stroke::new(1.3, col));
                let half: Vec<Pos2> = (0..=16).map(|k| {
                    let a = std::f32::consts::PI * (0.5 + k as f32 / 16.0);
                    c + vec2(a.cos(), a.sin()) * rad
                }).collect();
                p.add(egui::Shape::convex_polygon(half, col, Stroke::NONE));
            }
            Section::Files => paint_glyph(p, r, Glyph::Folder, col),
            // A mouse: a rounded body split at the buttons.
            Section::Mouse => {
                let b = Rect::from_center_size(r.center(), vec2(r.width() * 0.56, r.height() * 0.84));
                let st = Stroke::new(1.3, col);
                p.rect_stroke(b, CornerRadius::same((b.width() / 2.0) as u8), st, StrokeKind::Middle);
                p.line_segment([pos2(b.center().x, b.top() + 2.0), pos2(b.center().x, b.top() + b.height() * 0.36)], st);
            }
            Section::Preview => paint_glyph(p, r, Glyph::Eye, col),
            Section::Search => paint_glyph(p, r, Glyph::Search, col),
            Section::Git => {
                theme::paint_symbol(p, r, "git-branch", col);
            }
            Section::System => paint_glyph(p, r, Glyph::Monitor, col),
        }
    }
}

/// Every option: its section, the group it is listed under there (empty: no heading),
/// its title and a short line saying what it does. The order is the order on screen.
pub(crate) const OPTS: &[(Opt, Section, &str, &str, &str)] = &[
    (Opt::Theme, Section::Appearance, "Theme", "Color scheme", "The window's colors."),
    (Opt::FollowSystem, Section::Appearance, "Theme", "Follow system light/dark", "Switch between light and dark as the desktop does."),
    (Opt::SyntaxTheme, Section::Appearance, "Theme", "Code colors follow the theme", "Code in previews and diffs takes the theme's colors."),
    (Opt::Font, Section::Appearance, "Text", "Font", "Empty uses the system font."),
    (Opt::FontSize, Section::Appearance, "Text", "Font size", "Size of all text."),
    (Opt::Spacing, Section::Appearance, "Layout & motion", "Spacing", "From compact to roomy."),
    (Opt::Round, Section::Appearance, "Layout & motion", "Round corners", "Rounded panels, buttons and selections."),
    (Opt::RectButtons, Section::Appearance, "Layout & motion", "Rectangular window buttons", "Square minimize, maximize and close."),
    (Opt::AltRows, Section::Appearance, "Layout & motion", "Alternating row colors", "Stripe rows in Details view."),
    (Opt::InactiveOpacity, Section::Appearance, "Layout & motion", "Inactive panel opacity", "How visible unfocused panels stay."),
    (Opt::Animations, Section::Appearance, "Layout & motion", "Animations", "Turn every animation on or off."),
    (Opt::Tooltips, Section::Appearance, "Layout & motion", "Tooltips", "Hints when hovering buttons."),
    (Opt::SystemIcons, Section::Appearance, "Icons & paths", "System icon theme", "The desktop's file icons instead of Maia's."),
    (Opt::Breadcrumbs, Section::Appearance, "Icons & paths", "Path breadcrumbs", "Clickable path segments instead of plain text."),
    (Opt::CompactPaths, Section::Appearance, "Icons & paths", "Compact paths", "Show ~ for your home folder."),
    (Opt::ShowHidden, Section::Files, "List", "Show hidden files", "Show dotfiles."),
    (Opt::ShowSystem, Section::Files, "List", "Show system files", "Backups (*~, *.bak), sockets and devices."),
    (Opt::ShowExt, Section::Files, "List", "Show file extensions", "Display .txt, .png and so on."),
    (Opt::FoldersFirst, Section::Files, "List", "Folders first", "List folders before files."),
    (Opt::DateFormat, Section::Files, "List", "Date format", "A strftime pattern, like %Y-%m-%d %H:%M."),
    (Opt::CalcSizes, Section::Files, "Folder sizes", "Calculate folder sizes", "Count sizes and items in the background."),
    (Opt::SizesSsd, Section::Files, "Folder sizes", "Solid-state drives", "SSDs and other fast storage."),
    (Opt::SizesHdd, Section::Files, "Folder sizes", "Spinning hard drives", "Rotational disks: slower and noisier."),
    (Opt::SizesNet, Section::Files, "Folder sizes", "Network locations", "NFS, SMB and other remote mounts."),
    (Opt::Thumbnails, Section::Files, "Thumbnails", "Load thumbnails", "Where pictures get previews in icon views."),
    (Opt::ThumbFill, Section::Files, "Thumbnails", "Thumbnail fill", "Crop to fill the square instead of fitting."),
    (Opt::TrackRecents, Section::Files, "Recents", "Remember recents", "Visited folders and opened files."),
    (Opt::HighlightRecents, Section::Files, "Recents", "Highlight recents", "Mark recently used items with a dot."),
    (Opt::RecentsOnTop, Section::Files, "Recents", "Recents on top", "Sort recently used items first."),
    (Opt::CheckCopies, Section::Files, "Copies", "Check copies", "Read each copy back from the disk and compare it."),
    (Opt::TrashAge, Section::Files, "Trash", "Empty the trash", "What stays longer goes for good. Off unless you choose."),
    (Opt::CustomActions, Section::Files, "Right-click menu", "Custom actions", "Your own commands for the files you right-click."),
    (Opt::SingleClick, Section::Mouse, "Clicks", "Single click to open", "Open items with one click."),
    (Opt::SingleClickFolders, Section::Mouse, "Clicks", "Folders only", "Files still open with a double click."),
    (Opt::ActOnPress, Section::Mouse, "Clicks", "Act on press", "Select on mouse down, not on release."),
    (Opt::OpenFoldersNewTab, Section::Mouse, "Clicks", "Open folders in new tab", "Double-clicked folders open in a new tab."),
    (Opt::DblClickParent, Section::Mouse, "Clicks", "Double-click background goes up", "Double-click empty space for the parent."),
    (Opt::PictureViewer, Section::Mouse, "Clicks", "Open pictures in Maia's viewer", "Enter and double-click show them full window."),
    (Opt::MiddleClick, Section::Mouse, "Clicks", "Middle-click on folder", "Open it in a split or a new tab."),
    (Opt::SelectionBoxes, Section::Mouse, "Clicks", "Selection boxes", "Checkboxes for picking several items."),
    (Opt::ReverseScroll, Section::Mouse, "Scrolling", "Reverse scroll direction", "Invert the mouse wheel."),
    (Opt::ScrollBeyond, Section::Mouse, "Scrolling", "Scroll beyond last item", "Scroll past the end of a list."),
    (Opt::CharKey, Section::Mouse, "Keyboard", "Character keys", "What typing letters in a list does."),
    (Opt::InspectorLayout, Section::Preview, "", "Inspector layout", "Where the inspector docks."),
    (Opt::InspectorFont, Section::Preview, "", "Inspector font", "For text previews; empty uses monospace."),
    (Opt::InspectorFontSize, Section::Preview, "", "Inspector font size", "Text size in previews."),
    (Opt::LineSpacing, Section::Preview, "", "Line spacing", "Room between lines of text and code."),
    (Opt::Meaning, Section::Search, "", "Search by meaning", "Find files by topic, with a local Ollama model."),
    (Opt::MeaningFolders, Section::Search, "", "Folders to search", "Separated by commas; ~ is your home folder."),
    (Opt::MeaningPictures, Section::Search, "", "Include pictures", "Find pictures by what they show."),
    (Opt::IgnoredFiles, Section::Git, "", "Ignored files", "What git ignores, in the listing."),
    (Opt::GitPictures, Section::Git, "", "Show GitHub pictures", "Asks GitHub by username or email; kept on this computer."),
    (Opt::Startup, Section::System, "Startup", "On startup", "What Maia shows when it starts."),
    (Opt::NewTab, Section::System, "Startup", "New tab shows", "The folder a new tab opens."),
    (Opt::DefaultFm, Section::System, "System", "Default file manager", "Open folders from other apps in Maia."),
    (Opt::Picker, Section::System, "System", "Open and save dialog", "Pick files for other apps in Maia."),
    (Opt::SingleInstance, Section::System, "System", "Reuse the open window", "Other apps and new launches open tabs here."),
    (Opt::AssetCache, Section::System, "System", "Asset cache size", "Memory for thumbnails and previews."),
    (Opt::Editor, Section::System, "Developer", "Editor", "Empty finds one; a command can use {file} and {line}."),
    (Opt::Updates, Section::System, "Updates", "Check for updates", "Asks GitHub for a newer Maia, only when you check."),
    (Opt::Tour, Section::System, "Updates", "What's new", "What this version added, and the welcome tour."),
    (Opt::ConfigFile, Section::System, "Settings", "Config file", "Every option as text, in your editor."),
    (Opt::ResetAll, Section::System, "Settings", "Reset to defaults", "Every option and hotkey; asks first."),
    (Opt::Install, Section::System, "Settings", "Install Maia", "Add Maia to the app launcher and ~/.local/bin."),
];

/// The switch a sub-option sits under: it is indented below it and inert while it is off.
pub(crate) fn opt_parent(o: Opt) -> Option<Opt> {
    match o {
        Opt::SizesSsd | Opt::SizesHdd | Opt::SizesNet => Some(Opt::CalcSizes),
        Opt::SingleClickFolders => Some(Opt::SingleClick),
        Opt::MeaningFolders | Opt::MeaningPictures => Some(Opt::Meaning),
        _ => None,
    }
}

fn parent_on(c: &crate::config::Config, o: Opt) -> bool {
    match opt_parent(o) {
        Some(Opt::CalcSizes) => c.folder_sizes,
        Some(Opt::SingleClick) => c.single_click,
        Some(Opt::Meaning) => c.semantic_search,
        _ => true,
    }
}

/// Options matching `query` across every section, best first (rows of `OPTS`). The
/// title (prefix, word, substring, fuzzy) outranks the description, section and group;
/// a loose subsequence across everything only fills in at the end (on its own it
/// matched half the list and Enter flipped the wrong switch).
pub(crate) fn find_options(query: &str) -> Vec<usize> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return vec![];
    }
    let q = fuzzy::Query::new(query);
    let rank = |title: &str, desc: &str, place: &str| -> Option<i32> {
        let (t, d) = (title.to_lowercase(), desc.to_lowercase());
        if t.starts_with(&needle) {
            Some(3000)
        } else if t.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(&needle)) {
            Some(2500)
        } else if t.contains(&needle) {
            Some(2000)
        } else if let Some(sc) = fuzzy::score(&q, &t) {
            Some(1000 + sc)
        } else if d.contains(&needle) || place.to_lowercase().contains(&needle) {
            Some(500)
        } else {
            fuzzy::score(&q, &format!("{t} {d}")).map(|sc| sc / 4)
        }
    };
    let mut ranked: Vec<(i32, usize)> = OPTS.iter().enumerate().filter_map(|(i, (_, sec, group, title, desc))| rank(title, desc, &format!("{} {group}", sec.name())).map(|r| (r, i))).collect();
    // The loose matches only when nothing matches better ("size" isn't in "Square
    // minimize, maximize and close", though its letters are, in order).
    if ranked.iter().any(|r| r.0 >= 500) {
        ranked.retain(|r| r.0 >= 500);
    }
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    ranked.into_iter().map(|(_, i)| i).collect()
}

/// The Updates row: where checking stands, and the step to take (Check, Update, Restart).
/// Controls run right to left: the button at the edge, its status left of it.
fn update_control(ui: &mut Ui, app: &mut App, pal: &Palette) {
    use crate::update::State;
    let ctx = ui.ctx().clone();
    let dim = |ui: &mut Ui, s: &str| ui.label(egui::RichText::new(s).size(12.0).color(pal.text_dim));
    let button = |ui: &mut Ui, label: &str, accent: bool| {
        let b = if accent { ui.add(egui::Button::new(egui::RichText::new(label).color(Color32::WHITE)).fill(pal.accent)) } else { ui.button(label) };
        crate::ui::note_control(ui, b.rect, "update button");
        b
    };
    match app.updater.state() {
        State::Idle => {
            if button(ui, "Check now", false).clicked() {
                app.updater.check(&ctx);
            }
            dim(ui, concat!("Maia ", env!("CARGO_PKG_VERSION")));
        }
        State::Checking => {
            dim(ui, "Checking…");
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        State::Latest => {
            if button(ui, "Check again", false).clicked() {
                app.updater.check(&ctx);
            }
            dim(ui, "Up to date");
        }
        State::Available(r) => {
            // A release's text is Markdown: its headings shown without their marks.
            let notes: String = r.notes.lines().map(|l| l.trim_start_matches('#').trim_start()).filter(|l| !l.trim().is_empty()).take(8).collect::<Vec<_>>().join("\n");
            let b = button(ui, &format!("Update to {}", r.version.trim_start_matches('v')), true);
            let b = if notes.is_empty() { b } else { b.on_hover_text(notes) };
            if b.clicked() {
                app.updater.install(r, &ctx);
            }
        }
        State::Downloading(v) => {
            dim(ui, &format!("Downloading {}…", v.trim_start_matches('v')));
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        State::Installing(v) => {
            dim(ui, &format!("Installing {}…", v.trim_start_matches('v')));
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        State::Ready(v) => {
            if button(ui, "Restart", true).clicked() {
                app.updater.restart(&ctx);
            }
            dim(ui, &format!("{} installed", v.trim_start_matches('v')));
        }
        State::Failed(e) => {
            if button(ui, "Try again", false).clicked() {
                app.updater.check(&ctx);
            }
            dim(ui, "Couldn't update").on_hover_text(e);
        }
        State::CantWrite(dir) => {
            dim(ui, "Update it where you installed it").on_hover_text(format!("Maia can't write to {}: update it the way it was installed (a package manager, say).", dir.display()));
        }
    }
}

/// Choice control: a box showing the value and a chevron; click steps
/// to the next value, right-click (or Shift+click) to the previous one.
fn choice<T: PartialEq + Clone>(ui: &mut Ui, id: &str, cur: &mut T, opts: &[(T, &str)]) -> bool {
    let _ = id;
    let i = opts.iter().position(|(v, _)| v == cur).unwrap_or(0);
    let label = opts.get(i).map(|(_, l)| *l).unwrap_or("—");
    let (r, resp) = ui.allocate_exact_size(vec2(130.0, 20.0), Sense::click());
    crate::ui::note_control(ui, r, "choice");
    let v = ui.visuals();
    let (fill, stroke, text) = (v.widgets.inactive.bg_fill, v.widgets.noninteractive.bg_stroke, v.text_color());
    let fill = if resp.hovered() { v.widgets.hovered.bg_fill } else { fill };
    ui.painter().rect(r, CornerRadius::same(4), fill, stroke, StrokeKind::Inside);
    ui.painter().text(r.center() - vec2(6.0, 0.0), Align2::CENTER_CENTER, label, FontId::proportional(11.5), text);
    paint_glyph(ui.painter(), Rect::from_center_size(pos2(r.right() - 10.0, r.center().y), Vec2::splat(10.0)), Glyph::ChevronRight, text.gamma_multiply(0.7));
    let names: Vec<&str> = opts.iter().map(|(_, l)| *l).collect();
    let resp = resp.on_hover_text(names.join(" · "));
    let back = resp.secondary_clicked() || (resp.clicked() && ui.input(|i| i.modifiers.shift));
    if (resp.clicked() || back) && !opts.is_empty() {
        let n = opts.len();
        let j = if back { (i + n - 1) % n } else { (i + 1) % n };
        *cur = opts[j].0.clone();
        return true;
    }
    false
}

/// Options' switch, for other pages that want one.
pub(crate) fn switch(ui: &mut Ui, on: &mut bool, pal: &Palette) -> bool {
    toggle(ui, on, pal)
}

/// Switch: a pill whose knob carries a check (on) or a cross (off).
fn toggle(ui: &mut Ui, on: &mut bool, pal: &Palette) -> bool {
    let (r, resp) = ui.allocate_exact_size(vec2(32.0, 16.0), Sense::click());
    crate::ui::note_control(ui, r, "switch");
    let t = ui.ctx().animate_bool(resp.id, *on);
    let lerp_c = |a: Color32, b: Color32| Color32::from_rgb(egui::lerp(a.r() as f32..=b.r() as f32, t) as u8, egui::lerp(a.g() as f32..=b.g() as f32, t) as u8, egui::lerp(a.b() as f32..=b.b() as f32, t) as u8);
    let off_track = Color32::from_rgb(((pal.border.r() as u16 * 2 + pal.text_dim.r() as u16) / 3) as u8, ((pal.border.g() as u16 * 2 + pal.text_dim.g() as u16) / 3) as u8, ((pal.border.b() as u16 * 2 + pal.text_dim.b() as u16) / 3) as u8);
    ui.painter().rect_filled(r, CornerRadius::same(8), lerp_c(off_track, pal.accent));
    let x = egui::lerp(r.left() + 8.0..=r.right() - 8.0, t);
    let c = pos2(x, r.center().y);
    ui.painter().circle_filled(c, 6.0, lerp_c(pal.bg, Color32::from_rgb(0xe8, 0xf4, 0xfa)));
    let st = Stroke::new(1.3, lerp_c(pal.text_dim, pal.accent));
    if t > 0.5 {
        ui.painter().add(egui::Shape::line(vec![c + vec2(-2.6, 0.2), c + vec2(-0.8, 2.0), c + vec2(2.8, -1.8)], st));
    } else {
        ui.painter().line_segment([c + vec2(-2.0, -2.0), c + vec2(2.0, 2.0)], st);
        ui.painter().line_segment([c + vec2(-2.0, 2.0), c + vec2(2.0, -2.0)], st);
    }
    if resp.clicked() {
        *on = !*on;
        return true;
    }
    false
}

fn stepper(ui: &mut Ui, v: &mut f32, step: f32, min: f32, max: f32, pal: &Palette) -> bool {
    let mut changed = false;
    if ui.add(egui::Button::new("+").min_size(vec2(22.0, 20.0))).clicked() {
        *v = (*v + step).min(max);
        changed = true;
    }
    ui.label(egui::RichText::new(format!("{v:.1}")).color(pal.text));
    if ui.add(egui::Button::new("−").min_size(vec2(22.0, 20.0))).clicked() {
        *v = (*v - step).max(min);
        changed = true;
    }
    changed
}

/// Is option `o` at its default value? `reset` restores it.
fn opt_default(c: &mut crate::config::Config, o: Opt, reset: bool) -> bool {
    let d = crate::config::Config::default();
    macro_rules! f {
        ($($field:ident),*) => {{
            let same = true $(&& c.$field == d.$field)*;
            if reset { $(c.$field = d.$field.clone();)* }
            same
        }};
    }
    match o {
        Opt::Theme => f!(theme),
        // Lives in the desktop's settings, not Maia's: nothing to reset.
        Opt::DefaultFm | Opt::Picker => true,
        // Buttons, not settings.
        Opt::ConfigFile | Opt::ResetAll | Opt::Install | Opt::Updates | Opt::Tour | Opt::CustomActions => true,
        Opt::SingleInstance => f!(single_instance),
        Opt::Meaning => f!(semantic_search),
        Opt::MeaningFolders => f!(semantic_folders),
        Opt::MeaningPictures => f!(semantic_pictures),
        Opt::Font => f!(font_family),
        Opt::InspectorFontSize => f!(inspector_font_size),
        Opt::LineSpacing => f!(preview_line_spacing),
        Opt::RectButtons => f!(rect_buttons),
        Opt::AssetCache => f!(asset_cache_mb),
        Opt::FollowSystem => f!(follow_system_theme),
        Opt::SyntaxTheme => f!(syntax_theme),
        Opt::Round => f!(round_corners),
        Opt::FontSize => f!(font_size),
        Opt::Spacing => f!(spacing),
        Opt::Animations => f!(animations),
        Opt::Tooltips => f!(tooltips),
        Opt::AltRows => f!(alt_rows),
        Opt::InactiveOpacity => f!(inactive_opacity),
        Opt::Breadcrumbs => f!(breadcrumbs),
        Opt::CompactPaths => f!(compact_paths),
        Opt::ShowExt => f!(show_ext),
        Opt::ShowHidden => f!(show_hidden),
        Opt::ShowSystem => f!(show_system),
        Opt::FoldersFirst => f!(folders_first),
        Opt::HighlightRecents => f!(highlight_recents),
        Opt::RecentsOnTop => f!(recents_on_top),
        Opt::TrackRecents => f!(recents_store),
        Opt::CalcSizes => f!(folder_sizes),
        Opt::SizesSsd => f!(sizes_ssd),
        Opt::SizesHdd => f!(sizes_hdd),
        Opt::SizesNet => f!(sizes_network),
        Opt::SelectionBoxes => f!(selection_boxes),
        Opt::SystemIcons => f!(system_icons),
        Opt::InspectorFont => f!(inspector_font),
        Opt::Thumbnails => f!(thumbnails),
        Opt::ThumbFill => f!(thumb_fill),
        Opt::CheckCopies => f!(check_copies),
        Opt::ScrollBeyond => f!(scroll_beyond),
        Opt::DateFormat => f!(date_format),
        Opt::ActOnPress => f!(act_on_press),
        Opt::SingleClick => f!(single_click),
        Opt::SingleClickFolders => f!(single_click_folders_only),
        Opt::OpenFoldersNewTab => f!(open_folders_new_tab),
        Opt::DblClickParent => f!(dblclick_bg_parent),
        Opt::PictureViewer => f!(picture_viewer),
        Opt::ReverseScroll => f!(reverse_scroll),
        Opt::CharKey => f!(char_key_action),
        Opt::MiddleClick => f!(middle_click),
        Opt::InspectorLayout => f!(inspector_layout),
        Opt::Startup => f!(startup),
        Opt::NewTab => f!(new_tab),
        Opt::GitPictures => f!(github_pictures),
        Opt::TrashAge => f!(trash_days),
        Opt::Editor => f!(editor),
        Opt::IgnoredFiles => f!(git_ignored),
    }
}

fn option_control(ui: &mut Ui, app: &mut App, o: Opt) -> bool {
    let pal = app.pal;
    let layouts: Vec<String> = app.ud.layouts.keys().cloned().collect();
    option_control_inner(ui, app, o, &pal, &layouts)
}

fn option_control_inner(ui: &mut Ui, app: &mut App, o: Opt, pal: &Palette, layouts: &[String]) -> bool {
    let pal = *pal;
    if o == Opt::DefaultFm {
        let mut on = app.default_fm() == Some(true);
        if toggle(ui, &mut on, &pal) {
            app.set_default_fm(on);
        }
        return false;
    }
    if o == Opt::Picker {
        let mut on = app.picker_setting() == Some(true);
        if toggle(ui, &mut on, &pal) {
            app.set_picker_setting(on);
        }
        return false;
    }
    let c = &mut app.cfg;
    match o {
        Opt::Font => ui.add(egui::TextEdit::singleline(&mut c.font_family).hint_text(egui::RichText::new("System font").color(pal.text_dim)).desired_width(150.0).margin(egui::Margin::symmetric(5, 2))).lost_focus(),
        Opt::InspectorFontSize => stepper(ui, &mut c.inspector_font_size, 0.5, 8.0, 28.0, &pal),
        Opt::LineSpacing => ui.add(egui::Slider::new(&mut c.preview_line_spacing, 1.0..=2.0).step_by(0.05).fixed_decimals(2)).changed(),
        Opt::RectButtons => toggle(ui, &mut c.rect_buttons, &pal),
        Opt::AssetCache => ui.add(egui::Slider::new(&mut c.asset_cache_mb, 32..=2048).suffix(" MB").logarithmic(true)).changed(),
        Opt::Theme => {
            // The theme as a small swatch and its name; the picker shows them all.
            let t = c.theme;
            let p = theme::palette(t);
            let mut open = false;
            // The row's controls run right to left: the button at the edge, then the name
            // and its swatch.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let b = ui.button("Choose…");
                crate::ui::note_control(ui, b.rect, "choose theme");
                open = b.clicked();
                ui.label(egui::RichText::new(t.label()).size(12.5));
                let (r, _) = ui.allocate_exact_size(vec2(22.0, 16.0), Sense::hover());
                ui.painter().rect_filled(r, CornerRadius::same(5), p.panel);
                ui.painter().rect_stroke(r, CornerRadius::same(5), Stroke::new(1.0, p.border), StrokeKind::Inside);
                ui.painter().rect_filled(Rect::from_center_size(pos2(r.center().x, r.bottom() - 5.0), vec2(r.width() - 10.0, 3.0)), CornerRadius::same(2), p.accent);
            });
            if open {
                app.popup = Popup::Themes;
            }
            false
        }
        Opt::FollowSystem => toggle(ui, &mut c.follow_system_theme, &pal),
        Opt::SyntaxTheme => toggle(ui, &mut c.syntax_theme, &pal),
        Opt::Round => toggle(ui, &mut c.round_corners, &pal),
        Opt::FontSize => stepper(ui, &mut c.font_size, 0.5, 9.0, 24.0, &pal),
        Opt::Spacing => ui.add(egui::Slider::new(&mut c.spacing, 0.8..=1.6).step_by(0.05).fixed_decimals(2)).changed(),
        Opt::Animations => toggle(ui, &mut c.animations, &pal),
        Opt::Tooltips => toggle(ui, &mut c.tooltips, &pal),
        Opt::AltRows => toggle(ui, &mut c.alt_rows, &pal),
        Opt::InactiveOpacity => ui.add(egui::Slider::new(&mut c.inactive_opacity, 0.4..=1.0)).changed(),
        Opt::Breadcrumbs => toggle(ui, &mut c.breadcrumbs, &pal),
        Opt::CompactPaths => toggle(ui, &mut c.compact_paths, &pal),
        Opt::ShowExt => toggle(ui, &mut c.show_ext, &pal),
        Opt::ShowHidden => toggle(ui, &mut c.show_hidden, &pal),
        Opt::ShowSystem => toggle(ui, &mut c.show_system, &pal),
        Opt::FoldersFirst => toggle(ui, &mut c.folders_first, &pal),
        Opt::HighlightRecents => toggle(ui, &mut c.highlight_recents, &pal),
        Opt::RecentsOnTop => toggle(ui, &mut c.recents_on_top, &pal),
        Opt::TrackRecents => choice(ui, "recents", &mut c.recents_store, &[(RecentsStore::Persistent, "Persistent"), (RecentsStore::Session, "This session"), (RecentsStore::Off, "Off")]),
        Opt::CalcSizes => toggle(ui, &mut c.folder_sizes, &pal),
        Opt::SizesSsd => toggle(ui, &mut c.sizes_ssd, &pal),
        Opt::SizesHdd => toggle(ui, &mut c.sizes_hdd, &pal),
        Opt::SizesNet => toggle(ui, &mut c.sizes_network, &pal),
        Opt::SelectionBoxes => toggle(ui, &mut c.selection_boxes, &pal),
        Opt::SystemIcons => toggle(ui, &mut c.system_icons, &pal),
        Opt::SingleInstance => toggle(ui, &mut c.single_instance, &pal),
        Opt::Meaning => toggle(ui, &mut c.semantic_search, &pal),
        Opt::MeaningPictures => toggle(ui, &mut c.semantic_pictures, &pal),
        Opt::GitPictures => toggle(ui, &mut c.github_pictures, &pal),
        Opt::TrashAge => choice(ui, "trash age", &mut c.trash_days, &[(0, "Never"), (7, "After 7 days"), (30, "After 30 days"), (90, "After 90 days")]),
        Opt::Editor => {
            // Empty is Auto: the hint says what that comes to here.
            let hint = crate::editor::auto_label().map_or("Auto".to_string(), |l| format!("Auto: {l}"));
            let r = ui.add(egui::TextEdit::singleline(&mut c.editor).hint_text(egui::RichText::new(hint).color(pal.text_dim)).desired_width(150.0).margin(egui::Margin::symmetric(5, 2)));
            crate::ui::note_control(ui, r.rect, "editor field");
            r.lost_focus()
        }
        Opt::IgnoredFiles => choice(ui, "ignored", &mut c.git_ignored, &[(IgnoredFiles::Dim, "Dim"), (IgnoredFiles::Hide, "Hide"), (IgnoredFiles::Show, "Show")]),
        Opt::MeaningFolders => ui.add(egui::TextEdit::singleline(&mut c.semantic_folders).hint_text(egui::RichText::new("~/Documents").color(pal.text_dim)).desired_width(150.0).margin(egui::Margin::symmetric(5, 2))).lost_focus(),
        Opt::DefaultFm | Opt::Picker => false,
        Opt::Updates => {
            update_control(ui, app, &pal);
            false
        }
        Opt::Tour => {
            // Right to left: What's New at the edge, the tour beside it.
            for (label, c, what) in [("What's new", Cmd::WhatsNew, "whats new button"), ("Tour", Cmd::Welcome, "tour button")] {
                let b = ui.button(label);
                crate::ui::note_control(ui, b.rect, what);
                if b.clicked() {
                    app.actions.push(Action::Cmd(c));
                }
            }
            false
        }
        Opt::CustomActions => {
            let b = ui.button("Edit…");
            crate::ui::note_control(ui, b.rect, "option button");
            if b.clicked() {
                app.actions.push(Action::Cmd(Cmd::CustomActions));
            }
            let n = c.custom_actions.len();
            ui.label(egui::RichText::new(if n == 0 { "None yet".to_string() } else { format!("{n} action{}", if n == 1 { "" } else { "s" }) }).color(pal.text_dim));
            false
        }
        Opt::ConfigFile | Opt::ResetAll | Opt::Install => {
            let (label, c) = match o {
                Opt::ConfigFile => ("Open", Cmd::OpenConfig),
                Opt::ResetAll => ("Reset…", Cmd::ResetConfig),
                _ => ("Install", Cmd::Install),
            };
            let b = ui.button(label);
            crate::ui::note_control(ui, b.rect, "option button");
            if b.clicked() {
                app.actions.push(Action::Cmd(c));
            }
            false
        }
        Opt::InspectorFont => {
            let r = ui.add(egui::TextEdit::singleline(&mut c.inspector_font).hint_text(egui::RichText::new("monospace").color(pal.text_dim)).desired_width(150.0).margin(egui::Margin::symmetric(5, 2)));
            r.changed()
        }
        Opt::Thumbnails => choice(ui, "thumbs", &mut c.thumbnails, &[(ThumbMode::LocalDisks, "Local disks"), (ThumbMode::AllDrives, "All drives"), (ThumbMode::Never, "Never")]),
        Opt::ThumbFill => toggle(ui, &mut c.thumb_fill, &pal),
        Opt::CheckCopies => toggle(ui, &mut c.check_copies, &pal),
        Opt::ScrollBeyond => toggle(ui, &mut c.scroll_beyond, &pal),
        Opt::DateFormat => ui.add(egui::TextEdit::singleline(&mut c.date_format).desired_width(150.0).margin(egui::Margin::symmetric(5, 2))).changed(),
        Opt::ActOnPress => toggle(ui, &mut c.act_on_press, &pal),
        Opt::SingleClick => toggle(ui, &mut c.single_click, &pal),
        Opt::SingleClickFolders => toggle(ui, &mut c.single_click_folders_only, &pal),
        Opt::OpenFoldersNewTab => toggle(ui, &mut c.open_folders_new_tab, &pal),
        Opt::DblClickParent => toggle(ui, &mut c.dblclick_bg_parent, &pal),
        Opt::PictureViewer => toggle(ui, &mut c.picture_viewer, &pal),
        Opt::ReverseScroll => toggle(ui, &mut c.reverse_scroll, &pal),
        Opt::CharKey => choice(ui, "charkey", &mut c.char_key_action, &[(CharKeyAction::TriggerHotkey, "Hotkeys, else filter"), (CharKeyAction::Filter, "Filter"), (CharKeyAction::Jump, "Jump to name")]),
        Opt::MiddleClick => choice(ui, "middle", &mut c.middle_click, &[(MiddleClick::OpenInSplit, "Open in split"), (MiddleClick::OpenInNewTab, "Open in new tab")]),
        Opt::InspectorLayout => choice(ui, "insp", &mut c.inspector_layout, &[(InspectorLayout::Auto, "Auto"), (InspectorLayout::Right, "Right"), (InspectorLayout::Bottom, "Bottom")]),
        Opt::Startup => {
            let mut opts: Vec<(Startup, String)> = vec![(Startup::LastSession, "Last session".into()), (Startup::Home, "Home folder".into())];
            for l in layouts {
                opts.push((Startup::Layout(l.clone()), format!("Layout: {l}")));
            }
            let refs: Vec<(Startup, &str)> = opts.iter().map(|(a, b)| (a.clone(), b.as_str())).collect();
            choice(ui, "startup", &mut c.startup, &refs)
        }
        Opt::NewTab => choice(ui, "newtab", &mut c.new_tab, &[(NewTabShows::Current, "Current folder"), (NewTabShows::Home, "Home"), (NewTabShows::Computer, "Computer")]),
    }
}

/// How a row of Options is drawn.
struct RowLook {
    compact: bool,
    /// A search result: the section's name goes before the title.
    label: bool,
    selected: bool,
    scroll: bool,
    first: bool,
}

/// One option in its card: the title, a dim line saying what it does (a tooltip with
/// titles only, or when the line is cut), the control at the right edge and, while the
/// value differs from the default, a reset arrow beside it. Returns (a setting changed,
/// the row itself was clicked).
fn option_row(ui: &mut Ui, app: &mut App, oi: usize, r: Rect, look: &RowLook, q: &fuzzy::Query, pal: &Palette) -> (bool, bool) {
    let pal = *pal;
    let (o, sec, _, title, desc) = OPTS[oi];
    // Without curl there is no way to ask: said in place of what it would do.
    let desc: &str = if o == Opt::GitPictures && app.avatars.curl() == Some(false) { "Needs curl, which isn't installed; authors keep their initials." } else { desc };
    // What keeps the portal from reaching Maia here, in place of what it would do.
    let desc: &str = if o == Opt::Picker { app.picker_missing().unwrap_or(desc) } else { desc };
    // Sub-options sit indented under their switch and fade while it is off.
    let indent = if opt_parent(o).is_some() && !look.label { 20.0 } else { 0.0 };
    let inert = !parent_on(&app.cfg, o);
    let mut changed = false;
    if !look.first {
        let y = r.top().round() + 0.5;
        ui.painter().line_segment([pos2(r.left() + 12.0 + indent, y), pos2(r.right() - 12.0, y)], Stroke::new(1.0, pal.border.gamma_multiply(0.7)));
    }
    let resp = ui.interact(r, Id::new(("optrow", oi)), Sense::click());
    let inside = r.shrink(2.0);
    if ui.rect_contains_pointer(r) {
        ui.painter().rect_filled(inside, CornerRadius::same(6), pal.hover);
    }
    if look.selected {
        ui.painter().rect_stroke(inside, CornerRadius::same(6), Stroke::new(1.0, pal.accent.gamma_multiply(0.8)), StrokeKind::Inside);
    }
    if look.scroll {
        ui.scroll_to_rect(r, None);
    }
    let pad = if look.compact { 4.0 } else { 6.0 };
    let ctrl_rect = Rect::from_min_max(pos2(r.left() + (r.width() * 0.4).max(150.0), r.top() + pad), pos2(r.right() - 12.0, r.bottom() - pad));
    let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(ctrl_rect).layout(egui::Layout::right_to_left(egui::Align::Center)));
    // Controls at the popup's own size, as its titles are: at the window's font size
    // they would overflow the rows.
    {
        use egui::TextStyle;
        let st = cui.style_mut();
        for (t, size) in [(TextStyle::Body, 12.5), (TextStyle::Button, 12.5), (TextStyle::Monospace, 12.0), (TextStyle::Small, 11.0)] {
            let font = if t == TextStyle::Monospace { FontId::monospace(size) } else { FontId::proportional(size) };
            st.text_styles.insert(t, font);
        }
        st.spacing.interact_size.y = ctrl_rect.height().min(22.0);
        st.spacing.button_padding = vec2(7.0, 1.0);
    }
    if inert {
        cui.disable();
    }
    if option_control(&mut cui, app, o) {
        changed = true;
    }
    let mut left = cui.min_rect().left();
    if !opt_default(&mut app.cfg, o, false) {
        let rr = Rect::from_center_size(pos2(left - 16.0, r.center().y), Vec2::splat(20.0));
        crate::ui::note_control(ui, rr, "reset option");
        let rresp = ui.interact(rr, Id::new(("optreset", oi)), Sense::click()).on_hover_text("Reset to default");
        paint_glyph(ui.painter(), rr.shrink(3.0), Glyph::Refresh, if rresp.hovered() { pal.text } else { pal.text_dim });
        if rresp.clicked() {
            opt_default(&mut app.cfg, o, true);
            changed = true;
        }
        left = rr.left();
    }
    let x = r.left() + 12.0 + indent;
    let room = (left - 8.0 - x).max(24.0);
    let tc = if inert { pal.text_dim.gamma_multiply(0.6) } else { pal.text };
    let font = FontId::proportional(12.5);
    let mut job = egui::text::LayoutJob::default();
    if look.label {
        job.append(&format!("{} · ", sec.name()), 0.0, egui::TextFormat { font_id: font.clone(), color: pal.text_dim, ..Default::default() });
    }
    highlight_into(&mut job, title, q, font, tc, pal.accent);
    job.wrap = egui::text::TextWrapping { max_width: room, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let g = ui.fonts_mut(|f| f.layout_job(job));
    let ty = if look.compact { r.center().y - g.size().y / 2.0 } else { r.top() + 7.0 };
    ui.painter().galley(pos2(x, ty.round()), g, tc);
    let mut cut = true;
    if !look.compact {
        let dc = pal.text_dim.gamma_multiply(if inert { 0.6 } else { 1.0 });
        let dfont = FontId::proportional(11.0);
        cut = ui.painter().layout_no_wrap(desc.to_string(), dfont.clone(), dc).size().x > room;
        let dg = crate::listing::elided(ui, desc, dfont, room, dc);
        ui.painter().galley(pos2(x, (r.top() + 24.0).round()), dg, dc);
    }
    let resp = if cut { resp.on_hover_text(desc) } else { resp };
    (changed, resp.clicked())
}

/// What follows a change made in Options (or the welcome tour's choices): a theme that
/// follows the desktop, the keymap and listings rebuilt, tooltips on or off.
pub(crate) fn option_changed(app: &mut App, ctx: &egui::Context) {
    if app.cfg.follow_system_theme {
        if let Some(dark) = app.system_dark(ctx) {
            app.follow_system(dark);
        }
    }
    app.set_config_changed();
    ctx.all_styles_mut(|s| s.interaction.tooltip_delay = if app.cfg.tooltips { 0.5 } else { 1e9 });
}

/// One option as Options draws it, in a card of another dialog: title, what it does,
/// its control and the reset arrow. Returns whether it changed.
pub(crate) fn option_in(ui: &mut Ui, app: &mut App, o: Opt, r: Rect, first: bool) -> bool {
    let Some(oi) = OPTS.iter().position(|x| x.0 == o) else { return false };
    let pal = app.pal;
    let look = RowLook { compact: false, label: false, selected: false, scroll: false, first };
    option_row(ui, app, oi, r, &look, &fuzzy::Query::new(""), &pal).0
}

/// One line of key hints in `r`; those that would not fit are left out.
pub(crate) fn hint_row(ui: &Ui, r: Rect, items: &[(&str, &str)], pal: &Palette) {
    let p = ui.painter();
    let cy = r.center().y;
    let mut x = r.left();
    for (k, label) in items {
        let kg = p.layout_no_wrap(k.to_string(), FontId::proportional(10.5), pal.text_dim);
        let lg = p.layout_no_wrap(label.to_string(), FontId::proportional(11.0), pal.text_dim);
        let (ks, ls) = (kg.size(), lg.size());
        if x + ks.x + 14.0 + ls.x > r.right() {
            break;
        }
        let kr = Rect::from_min_size(pos2(x, (cy - (ks.y + 6.0) / 2.0).round()), ks + vec2(10.0, 6.0));
        p.rect(kr, CornerRadius::same(4), pal.bg, Stroke::new(1.0, pal.border), StrokeKind::Inside);
        p.galley(kr.min + vec2(5.0, 3.0), kg, pal.text_dim);
        crate::ui::note_control(ui, kr, "key hint");
        p.galley(pos2(kr.right() + 4.0, (cy - ls.y / 2.0).round()), lg, pal.text_dim);
        x = kr.right() + 4.0 + ls.x + 14.0;
    }
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// Options: the sections on the left, the chosen one's settings in cards on the right.
/// Typing searches every section at once and lists the matches with their section.
fn options(app: &mut App, ctx: &egui::Context, st: &mut OptState) -> bool {
    let pal = app.pal;
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    // The section shown last is shown again, for as long as Maia runs.
    let last_id = Id::new("options section");
    let mut section = st.section.unwrap_or_else(|| ctx.data(|d| d.get_temp::<usize>(last_id)).and_then(|i| Section::ALL.get(i).copied()).unwrap_or(Section::Appearance));
    // A text field other than the search (a font, a date format) keeps its keys.
    let focused = ctx.memory(|m| m.focused());
    let editing = focused.is_some_and(|f| Some(f) != st.search_id && egui::TextEdit::load_state(ctx, f).is_some());
    let (up, down, enter, prev, next) = if editing {
        (false, false, false, false, false)
    } else {
        ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::CTRL, Key::PageUp),
                i.consume_key(Modifiers::CTRL, Key::PageDown),
            )
        })
    };
    // Typing after a click on a switch (the search no longer has the keyboard) still
    // searches.
    if !editing && (focused.is_none() || focused != st.search_id) {
        let typed = ctx.input_mut(|i| {
            let mut t = String::new();
            i.events.retain(|e| match e {
                Event::Text(x) => {
                    t.push_str(x);
                    false
                }
                _ => true,
            });
            t
        });
        if !typed.is_empty() {
            st.query.push_str(&typed);
            st.focus = true;
            st.cursor_end = true;
            st.sel = 0;
            st.top = true;
        }
    }
    if prev || next {
        section = section.step(if next { 1 } else { -1 });
        st.query.clear();
        st.sel = 0;
        st.top = true;
    }
    let searching = !st.query.trim().is_empty();
    let rows: Vec<usize> = if searching { find_options(&st.query) } else { (0..OPTS.len()).filter(|&i| OPTS[i].1 == section).collect() };
    // Install only for a copy that isn't the installed one.
    let rows: Vec<usize> = rows.into_iter().filter(|&i| OPTS[i].0 != Opt::Install || !crate::install::installed_here()).collect();
    if up && st.sel > 0 {
        st.sel -= 1;
        st.scroll_to_sel = true;
    }
    if down && st.sel + 1 < rows.len() {
        st.sel += 1;
        st.scroll_to_sel = true;
    }
    if up || down || enter {
        st.keyed = true;
    }
    st.sel = st.sel.min(rows.len().saturating_sub(1));
    let mut changed = false;
    let mut cmd = None;
    let actions_before = app.actions.len();
    if enter {
        // Enter flips a switch, opens the theme picker or presses a row's button; a
        // sub-option does nothing while its switch is off.
        if let Some(&i) = rows.get(st.sel) {
            let o = OPTS[i].0;
            if parent_on(&app.cfg, o) {
                match o {
                    Opt::DefaultFm => {
                        let on = app.default_fm() == Some(true);
                        app.set_default_fm(!on);
                    }
                    Opt::Picker => {
                        let on = app.picker_setting() == Some(true);
                        app.set_picker_setting(!on);
                    }
                    Opt::Theme => app.popup = Popup::Themes,
                    Opt::ConfigFile => cmd = Some(Cmd::OpenConfig),
                    Opt::ResetAll => cmd = Some(Cmd::ResetConfig),
                    Opt::Install => cmd = Some(Cmd::Install),
                    Opt::Tour => cmd = Some(Cmd::WhatsNew),
                    Opt::CustomActions => cmd = Some(Cmd::CustomActions),
                    Opt::Updates => {
                        if let crate::update::State::Available(r) = app.updater.state() {
                            app.updater.install(r, ctx);
                        } else if !matches!(app.updater.state(), crate::update::State::Ready(_)) {
                            app.updater.check(ctx);
                        }
                    }
                    _ => changed |= opt_flip(&mut app.cfg, o),
                }
            }
        }
    }
    let q = fuzzy::Query::new(&st.query);
    let compact = app.cfg.compact_lists;
    let row_h = if compact { 32.0 } else { 44.0 };
    let card_fill = mix(pal.raised, pal.bg, 0.45);
    let card_line = pal.border.gamma_multiply(0.8);
    let mut close = false;
    let mut toggle_compact = false;
    let mut go_section = None;
    let mut clicked_row = None;
    // A larger window than the other dialogs, of a fixed size so it does not jump
    // between sections; it shrinks with the window.
    let screen = ctx.content_rect();
    let w = 860f32.min(screen.width() - 24.0).round();
    let h = 600f32.min(screen.height() - 24.0).round();
    let pos = pos2((screen.center().x - w / 2.0).round(), (screen.top() + ((screen.height() - h) / 2.0).clamp(12.0, 70.0)).round());
    note_layer(Id::new("options"));
    let area = egui::Area::new(Id::new("options")).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
        crate::frost::behind(ui, DIALOG_RADIUS);
        dialog_frame(&pal).show(ui, |ui| {
            let (_, inner) = ui.allocate_space(vec2(w - 2.0 * DIALOG_PAD, h - 2.0 * DIALOG_PAD));
            let top_down = egui::Layout::top_down(egui::Align::Min);
            let mut head = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(top_down));
            // The buttons first, from the right edge; the search takes what is left.
            head.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if crate::ui::glyph_button(ui, Glyph::Close, "Close (Esc)", true, &pal).clicked() {
                    close = true;
                }
                let cb = compact_button(ui, compact, &pal);
                crate::ui::note_control(ui, cb.rect, "titles only");
                if cb.clicked() {
                    toggle_compact = true;
                }
                let before = st.query.clone();
                let fw = (ui.available_width() - 4.0).floor();
                ui.allocate_ui_with_layout(vec2(fw, 0.0), top_down, |ui| {
                    ui.set_width(fw);
                    let r = search_field(ui, &mut st.query, "Search options…", &mut st.focus, &pal, Glyph::Search);
                    st.search_id = Some(r.id);
                    if std::mem::take(&mut st.cursor_end) {
                        if let Some(mut s) = egui::TextEdit::load_state(ui.ctx(), r.id) {
                            s.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(st.query.chars().count()))));
                            s.store(ui.ctx(), r.id);
                        }
                    }
                });
                if st.query != before {
                    st.sel = 0;
                    st.top = true;
                }
            });
            let foot = Rect::from_min_max(pos2(inner.left(), inner.bottom() - 22.0), inner.max);
            hint_row(ui, foot, &[("↑↓", "options"), ("Ctrl PgUp PgDn", "sections"), ("Enter", "toggle"), ("Esc", "close")], &pal);
            let body = Rect::from_min_max(pos2(inner.left(), head.min_rect().bottom().round() + 2.0), pos2(inner.right(), foot.top() - 6.0));
            // A narrow window keeps only the sections' glyphs.
            let narrow = inner.width() < 620.0;
            let side = Rect::from_min_max(body.min, pos2(body.left() + if narrow { 36.0 } else { 172.0 }, body.bottom()));
            let lx = side.right().round() + 6.5;
            ui.painter().line_segment([pos2(lx, body.top()), pos2(lx, body.bottom())], Stroke::new(1.0, pal.border));
            let main = Rect::from_min_max(pos2(lx + 7.5, body.top()), body.max);
            let hits: Vec<Section> = if searching { rows.iter().map(|&i| OPTS[i].1).collect() } else { vec![] };
            let mut sui = ui.new_child(egui::UiBuilder::new().max_rect(side).layout(top_down));
            egui::ScrollArea::vertical().id_salt("opt sections").max_height(side.height()).auto_shrink([false, false]).show(&mut sui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                for sec in Section::ALL {
                    // Chosen as a listing's selection is: its fill under the usual text.
                    let on = !searching && sec == section;
                    // While searching, sections without a match step back.
                    let quiet = searching && !hits.contains(&sec);
                    let resp = row(ui, 30.0, on, &pal, |ui, r| {
                        let faint = pal.text_dim.gamma_multiply(0.55);
                        let (gc, tc) = if on { (pal.text, pal.text) } else if quiet { (faint, faint) } else { (pal.text_dim, pal.text) };
                        let gx = if narrow { r.center().x } else { r.left() + 16.0 };
                        sec.paint(ui.painter(), Rect::from_center_size(pos2(gx.round(), r.center().y.round()), Vec2::splat(14.0)), gc);
                        if !narrow {
                            let g = crate::listing::elided(ui, sec.name(), FontId::proportional(13.0), r.width() - 42.0, tc);
                            ui.painter().galley(pos2(r.left() + 34.0, (r.center().y - g.size().y / 2.0).round()), g, tc);
                        }
                    });
                    crate::ui::note_control(ui, resp.rect, sec.control());
                    let resp = if narrow { resp.on_hover_text(sec.name()) } else { resp };
                    if resp.clicked() {
                        go_section = Some(sec);
                    }
                }
            });
            let mut mui = ui.new_child(egui::UiBuilder::new().max_rect(main).layout(top_down));
            let mut sa = egui::ScrollArea::vertical().id_salt("opt body").max_height(main.height()).auto_shrink([false, false]);
            if std::mem::take(&mut st.top) {
                sa = sa.vertical_scroll_offset(0.0);
            }
            sa.show(&mut mui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                // Room on the right for the scroll bar.
                let cw = (ui.available_width() - 10.0).floor();
                let mut k = 0;
                while k < rows.len() {
                    // Search results are one card; a section has a card per group.
                    let group = if searching { "" } else { OPTS[rows[k]].2 };
                    let n = if searching { rows.len() } else { rows[k..].iter().take_while(|&&i| OPTS[i].2 == group).count() };
                    if k > 0 {
                        ui.add_space(12.0);
                    }
                    if !group.is_empty() {
                        let (hr, _) = ui.allocate_exact_size(vec2(cw, 20.0), Sense::hover());
                        ui.painter().text(pos2(hr.left() + 4.0, hr.center().y - 1.0), Align2::LEFT_CENTER, group, FontId::proportional(11.5), pal.text_dim);
                    }
                    let (card, _) = ui.allocate_exact_size(vec2(cw, n as f32 * row_h), Sense::hover());
                    ui.painter().rect(card, CornerRadius::same(8), card_fill, Stroke::new(1.0, card_line), StrokeKind::Inside);
                    for j in 0..n {
                        let vi = k + j;
                        let r = Rect::from_min_size(card.min + vec2(0.0, j as f32 * row_h), vec2(cw, row_h));
                        let look = RowLook { compact, label: searching, selected: (st.keyed || searching) && vi == st.sel, scroll: st.scroll_to_sel && vi == st.sel, first: j == 0 };
                        let (c, clicked) = option_row(ui, app, rows[vi], r, &look, &q, &pal);
                        changed |= c;
                        if clicked {
                            clicked_row = Some(vi);
                        }
                    }
                    k += n;
                }
                if rows.is_empty() {
                    ui.add_space(28.0);
                    ui.vertical_centered(|ui| ui.label(egui::RichText::new("No option matches").size(12.5).color(pal.text_dim)));
                }
                ui.add_space(6.0);
            });
            st.scroll_to_sel = false;
        });
    });
    let rect = area.response.rect;
    crate::ui::note_popup(rect);
    if let Some(vi) = clicked_row {
        st.sel = vi;
    }
    if let Some(sec) = go_section {
        section = sec;
        st.query.clear();
        st.sel = 0;
        st.top = true;
    }
    st.section = Some(section);
    ctx.data_mut(|d| d.insert_temp(last_id, Section::ALL.iter().position(|s| *s == section).unwrap_or(0)));
    if toggle_compact {
        app.cfg.compact_lists = !app.cfg.compact_lists;
        changed = true;
    }
    if changed {
        option_changed(app, ctx);
    }
    if let Some(c) = cmd {
        app.actions.push(Action::Cmd(c));
    }
    // Reset asks first, in a dialog of its own.
    if app.actions[actions_before..].iter().any(|a| matches!(a, Action::Cmd(Cmd::ResetConfig))) {
        return false;
    }
    let combo_open = egui::Popup::is_any_open(ctx);
    !(close || (!combo_open && clicked_outside(ctx, rect)))
}

// ------------------------------------------------------------------ small popups

fn types(app: &mut App, ctx: &egui::Context, panel: u64, query: &mut String) -> bool {
    let pal = app.pal;
    let (show_hidden, show_system) = (app.cfg.show_hidden, app.cfg.show_system);
    let Some(p) = app.panels.get_mut(&panel) else { return false };
    let fallback = pos2(p.rect.right() - 320.0, p.rect.bottom() - 380.0);
    let t = p.tab_mut();
    // Opens under the Type column's filter button.
    let anchor = t.funnel_at.unwrap_or(fallback);
    let counts = t.type_samples(show_hidden, show_system);
    let q = fuzzy::Query::new(query);
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    let mut close = false;
    let (_, rect) = anchored(ctx, "types", anchor, 300.0, &pal, |ui| {
        // Header: filter, check all, uncheck all | close.
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(20.0, 22.0), Sense::hover());
            paint_glyph(ui.painter(), Rect::from_center_size(r.center(), Vec2::splat(12.0)), Glyph::Search, pal.text_dim);
            let te = ui.add(egui::TextEdit::singleline(query).hint_text(egui::RichText::new(format!("Filter {} types…", counts.len())).color(pal.text_dim)).frame(crate::ui::bare_edit()).desired_width(ui.available_width() - 84.0));
            if !te.has_focus() && query.is_empty() {
                te.request_focus();
            }
            let icon_btn = |ui: &mut Ui, checked: bool, tip: &str| -> bool {
                let (r, resp) = ui.allocate_exact_size(vec2(24.0, 22.0), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(r, CornerRadius::same(4), pal.row_hover);
                }
                let b = Rect::from_center_size(r.center(), Vec2::splat(13.0));
                ui.painter().rect_stroke(b, CornerRadius::same(2), Stroke::new(1.2, pal.text_dim), StrokeKind::Middle);
                let c = b.center();
                let st = Stroke::new(1.3, pal.text_dim);
                if checked {
                    ui.painter().add(egui::Shape::line(vec![c + vec2(-3.0, 0.0), c + vec2(-0.8, 2.4), c + vec2(3.2, -2.4)], st));
                } else {
                    ui.painter().line_segment([c + vec2(-2.6, -2.6), c + vec2(2.6, 2.6)], st);
                    ui.painter().line_segment([c + vec2(-2.6, 2.6), c + vec2(2.6, -2.6)], st);
                }
                resp.on_hover_text(tip).clicked()
            };
            if icon_btn(ui, true, "Show only these types (check all)") {
                t.settings.only_types = counts.iter().map(|(k, ..)| k.clone()).collect();
                t.dirty = true;
            }
            if icon_btn(ui, false, "Show every type (uncheck all)") {
                t.settings.only_types.clear();
                t.dirty = true;
            }
            let (sr, _) = ui.allocate_exact_size(vec2(7.0, 20.0), Sense::hover());
            ui.painter().line_segment([sr.center_top(), sr.center_bottom()], Stroke::new(1.0, pal.border));
            if crate::ui::glyph_button(ui, Glyph::Close, "Close", true, &pal).clicked() {
                close = true;
            }
        });
        let yy = ui.cursor().top() + 1.0;
        let xr = ui.min_rect().x_range();
        ui.painter().line_segment([pos2(xr.min - 5.0, yy), pos2(xr.max + 5.0, yy)], Stroke::new(1.0, pal.border));
        ui.add_space(4.0);
        egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            for (k, n, sample) in &counts {
                if !q.is_empty() && fuzzy::score(&q, &k.to_lowercase()).is_none() {
                    continue;
                }
                let on = t.settings.only_types.contains(k);
                // Checked types are highlighted like a selection; the checkbox sits at the right.
                let resp = row(ui, 24.0, on, &pal, |ui, r| {
                    let ic = Rect::from_center_size(pos2(r.left() + 13.0, r.center().y), Vec2::splat(16.0));
                    if !crate::icons::entry(ui.painter(), ic, sample) {
                        theme::paint_icon(ui.painter(), ic, theme::categorize(sample.ext(), sample.is_dir, sample.is_executable()), "", false);
                    }
                    let tc = if on { pal.on_sel() } else { pal.text };
                    let job = highlight_job(k, &q, FontId::proportional(12.5), tc, if on { tc } else { pal.accent });
                    let g = ui.fonts_mut(|f| f.layout_job(job));
                    ui.painter().galley(pos2(r.left() + 30.0, r.center().y - g.size().y / 2.0), g, tc);
                    let cb = Rect::from_center_size(pos2(r.right() - 14.0, r.center().y), Vec2::splat(14.0));
                    if on {
                        ui.painter().rect_filled(cb, CornerRadius::same(3), tc.gamma_multiply(0.18));
                        ui.painter().rect_stroke(cb, CornerRadius::same(3), Stroke::new(1.2, tc), StrokeKind::Inside);
                        ui.painter().add(egui::Shape::line(vec![cb.left_center() + vec2(3.0, 0.0), cb.center_bottom() + vec2(-1.0, -3.5), cb.right_top() + vec2(-3.0, 3.5)], Stroke::new(1.6, tc)));
                    } else {
                        ui.painter().rect_stroke(cb, CornerRadius::same(3), Stroke::new(1.2, pal.text_dim.gamma_multiply(0.6)), StrokeKind::Inside);
                    }
                    ui.painter().text(pos2(cb.left() - 10.0, r.center().y), Align2::RIGHT_CENTER, crate::ui::fmt_count(*n), FontId::proportional(11.0), if on { pal.on_sel_dim() } else { pal.text_dim.gamma_multiply(0.8) });
                });
                if resp.clicked() {
                    if on {
                        t.settings.only_types.retain(|x| x != k);
                    } else {
                        t.settings.only_types.push(k.clone());
                    }
                    t.dirty = true;
                }
            }
        });
    });
    !(close || clicked_outside(ctx, rect))
}

/// A checkbox drawn to match the theme.
/// A box drawn as the app's own, with its label; either toggles it. True when clicked.
fn boxed_check(ui: &mut Ui, on: bool, label: egui::RichText, what: &'static str, pal: &Palette) -> bool {
    ui.horizontal(|ui| {
        let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
        checkbox_glyph(ui.painter(), r, on, pal);
        crate::ui::note_control(ui, r, what);
        let t = ui.add(egui::Label::new(label).selectable(false).sense(Sense::click()));
        resp.clicked() || t.clicked()
    })
    .inner
}

pub fn checkbox_glyph(p: &egui::Painter, r: Rect, on: bool, pal: &Palette) {
    if on {
        p.rect_filled(r, CornerRadius::same(3), pal.accent);
        p.add(egui::Shape::line(vec![r.left_center() + vec2(3.0, 0.0), r.center_bottom() + vec2(-1.0, -3.5), r.right_top() + vec2(-3.0, 3.5)], Stroke::new(1.6, Color32::WHITE)));
    } else {
        p.rect_stroke(r, CornerRadius::same(3), Stroke::new(1.2, pal.text_dim.gamma_multiply(0.8)), StrokeKind::Inside);
    }
}

fn tab_search(app: &mut App, ctx: &egui::Context, query: &mut String, sel: &mut usize) -> bool {
    let pal = app.pal;
    let k = take_keys(ctx);
    if k.esc {
        return false;
    }
    let q = fuzzy::Query::new(query);
    // (label, path, Some((panel, idx)) or None for closed)
    let mut rows: Vec<(String, PathBuf, Option<(u64, usize)>)> = vec![];
    let mut leaves = vec![];
    app.tree.leaves(&mut leaves);
    for pid in leaves {
        for (i, t) in app.panels[&pid].tabs.iter().enumerate() {
            rows.push((t.title(), t.path.clone(), Some((pid, i))));
        }
    }
    for c in app.closed_tabs.iter().rev().take(20) {
        let name = c.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| c.path.display().to_string());
        rows.push((name, c.path.clone(), None));
    }
    if !q.is_empty() {
        rows.retain(|(l, p, _)| fuzzy::score(&q, &format!("{l} {}", home_short(p)).to_lowercase()).is_some());
    }
    step(sel, rows.len(), &k, |_| false);
    let mut pick = if k.enter { rows.get(*sel).cloned() } else { None };
    let mut focus = true;
    let (_, rect) = centered(ctx, "tabsearch", 520.0, &pal, |ui| {
        search_field(ui, query, "Search open and recently closed tabs…", &mut focus, &pal, Glyph::Search);
        egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
            let mut closed_header = false;
            for (i, (label, path, loc)) in rows.iter().enumerate() {
                if loc.is_none() && !closed_header {
                    closed_header = true;
                    menu_heading(ui, "Recently closed", &pal);
                }
                let resp = row(ui, 30.0, i == *sel, &pal, |ui, r| {
                    let mut job = highlight_job(label, &q, FontId::proportional(13.5), pal.text, pal.accent);
                    // A tab named after a long folder is cut, not drawn past the window.
                    job.wrap = egui::text::TextWrapping { max_width: (r.width() - 20.0).max(8.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
                    let g = ui.fonts_mut(|f| f.layout_job(job));
                    let gw = g.size().x;
                    ui.painter().galley(pos2(r.left() + 10.0, r.center().y - g.size().y / 2.0), g, pal.text);
                    // From home as ~, its end kept: the folder's own name matters most.
                    let s = crate::listing::elided_tail(ui, &home_short(path), FontId::proportional(11.5), r.width() - gw - 30.0, pal.text_dim);
                    ui.painter().galley(pos2(r.left() + 20.0 + gw, r.center().y - s.size().y / 2.0), s, pal.text_dim);
                });
                if resp.clicked() {
                    pick = Some((label.clone(), path.clone(), *loc));
                }
            }
        });
    });
    if let Some((_, path, loc)) = pick {
        match loc {
            Some((pid, i)) => app.actions.push(Action::ActivateTab { panel: pid, idx: i }),
            None => {
                app.closed_tabs.retain(|c| c.path != path);
                let id = app.active;
                app.actions.push(Action::NewTab { panel: id, path, select: None, focus: true });
            }
        }
        return false;
    }
    !clicked_outside(ctx, rect)
}

fn saved_view(app: &mut App, ctx: &egui::Context, panel: u64) -> bool {
    let pal = app.pal;
    let Some(p) = app.panels.get(&panel) else { return false };
    let anchor = pos2(p.rect.left() + 100.0, p.rect.top() + 70.0);
    let path = p.path();
    let settings = p.tab().settings.clone();
    let current = app.ud.views.get(&path).map(|v| v.scope);
    let mut choice_v: Option<Option<SaveScope>> = None;
    let mut set_default = false;
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    let (_, rect) = anchored(ctx, "savedview", anchor, 300.0, &pal, |ui| {
        ui.label(egui::RichText::new("Saved view settings").strong());
        ui.label(egui::RichText::new("View size, columns, sort, grouping and type filters.").small().color(pal.text_dim));
        ui.add_space(4.0);
        for (v, label) in [(None, "None (inherit)"), (Some(SaveScope::ThisFolder), "This folder"), (Some(SaveScope::Subfolders), "This folder and subfolders")] {
            if ui.radio(current == v, label).clicked() {
                choice_v = Some(v);
            }
        }
        ui.separator();
        if ui.button("Set as default for all folders").clicked() {
            set_default = true;
        }
    });
    if let Some(v) = choice_v {
        match v {
            None => {
                app.ud.views.remove(&path);
            }
            Some(scope) => {
                app.ud.views.insert(path.clone(), SavedView { scope, view: settings.clone() });
            }
        }
        if let Some(p) = app.panels.get_mut(&panel) {
            let t = p.tab_mut();
            t.saved_scope = v;
            // Saved: folders without a view of their own go back to the free one on leaving
            // (this view, if there was none). Forgotten: this view becomes the free one.
            if v.is_some() {
                t.free_view.get_or_insert_with(|| t.settings.clone());
            } else {
                t.free_view = None;
            }
        }
        app.ud_dirty = true;
    }
    if set_default {
        app.ud.default_view = Some(settings);
        app.ud_dirty = true;
        app.toast("Saved as the default view");
    }
    !clicked_outside(ctx, rect)
}

fn zoom(app: &mut App, ctx: &egui::Context, panel: u64, anchor: Pos2) -> bool {
    let pal = app.pal;
    let Some(p) = app.panels.get_mut(&panel) else { return false };
    let t = p.tab_mut();
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    // Opens above the footer chip. The named views sit beside a vertical slider, each
    // at the height of its zoom value, so the dense views bunch up at the bottom; Columns
    // and Stack, which have no zoom, below them. It hangs from its lower edge, whatever the
    // Spacing.
    let (w, track) = (176.0, 168.0);
    let columns = t.settings.column_view;
    let stacked = t.settings.stack_view;
    let (mut set_columns, mut set_stack) = (false, false);
    let (_, rect) = anchored_from(ctx, "zoom", pos2(anchor.x - w + 30.0, anchor.y - 10.0), Align2::LEFT_BOTTOM, w, &pal, |ui| {
        let (r, resp) = ui.allocate_exact_size(vec2(w - 16.0, track + 20.0), Sense::click_and_drag());
        let rail_x = r.right() - 10.0;
        let (y0, y1) = (r.bottom() - 10.0, r.top() + 10.0);
        let y_of = |v: f32| y0 + (y1 - y0) * v;
        let z = t.settings.zoom;
        let mut set = None;
        let painter = ui.painter();
        for (name, v) in crate::listing::VIEW_STOPS {
            let y = y_of(v);
            let lr = Rect::from_min_max(pos2(r.left(), y - 10.0), pos2(rail_x - 14.0, y + 10.0));
            let on = !columns && !stacked && (crate::listing::view_name(z) == name || (name == "M Icons" && (z - 0.3).abs() < 0.001));
            let hov = ui.rect_contains_pointer(lr);
            if on {
                painter.rect_filled(lr, CornerRadius::same(4), pal.accent);
            } else if hov {
                painter.rect_filled(lr, CornerRadius::same(4), pal.hover);
            }
            let col = if on { Color32::WHITE } else { pal.text };
            crate::listing::paint_view_glyph(painter, Rect::from_center_size(pos2(lr.left() + 12.0, y), Vec2::splat(11.0)), crate::listing::mode_for(v), col);
            painter.text(pos2(lr.left() + 26.0, y), Align2::LEFT_CENTER, name, FontId::proportional(12.5), col);
            if !on && v > 0.25 {
                painter.line_segment([pos2(lr.left() + 26.0, y + 10.0), pos2(lr.right(), y + 10.0)], Stroke::new(1.0, pal.border.gamma_multiply(0.6)));
            }
            if hov && resp.clicked() {
                set = Some(v);
            }
        }
        // Slider rail and knob.
        painter.line_segment([pos2(rail_x, y0), pos2(rail_x, y1)], Stroke::new(4.0, pal.border));
        painter.line_segment([pos2(rail_x, y0), pos2(rail_x, y_of(z))], Stroke::new(4.0, pal.accent.gamma_multiply(0.6)));
        painter.circle_filled(pos2(rail_x, y_of(z)), 7.0, if columns || stacked { pal.text_dim } else { pal.accent });
        if let Some(pp) = resp.interact_pointer_pos() {
            if resp.dragged() || (resp.clicked() && pp.x > rail_x - 16.0) {
                let v = ((pp.y - y0) / (y1 - y0)).clamp(0.0, 1.0);
                // Snap the dense views to their stops; icons stay continuous.
                set = Some(if v < 0.3 { (v * 10.0).round() / 10.0 } else { v });
            }
        }
        let (sep, _) = ui.allocate_exact_size(vec2(w - 16.0, 3.0), Sense::hover());
        ui.painter().line_segment([pos2(sep.left() + 4.0, sep.center().y), pos2(sep.right() - 4.0, sep.center().y)], Stroke::new(1.0, pal.border));
        let (cr, cresp) = ui.allocate_exact_size(vec2(w - 16.0, 20.0), Sense::click());
        let lr = Rect::from_min_max(cr.min, pos2(rail_x - 14.0, cr.bottom()));
        if columns {
            ui.painter().rect_filled(lr, CornerRadius::same(4), pal.accent);
        } else if cresp.hovered() {
            ui.painter().rect_filled(lr, CornerRadius::same(4), pal.hover);
        }
        let col = if columns { Color32::WHITE } else { pal.text };
        crate::listing::paint_view_glyph(ui.painter(), Rect::from_center_size(pos2(lr.left() + 12.0, lr.center().y), Vec2::splat(11.0)), crate::listing::Mode::Columns, col);
        ui.painter().text(pos2(lr.left() + 26.0, lr.center().y), Align2::LEFT_CENTER, "Columns", FontId::proportional(12.5), col);
        set_columns = cresp.clicked();
        let (sr, sresp) = ui.allocate_exact_size(vec2(w - 16.0, 20.0), Sense::click());
        let lr = Rect::from_min_max(sr.min, pos2(rail_x - 14.0, sr.bottom()));
        if stacked {
            ui.painter().rect_filled(lr, CornerRadius::same(4), pal.accent);
        } else if sresp.hovered() {
            ui.painter().rect_filled(lr, CornerRadius::same(4), pal.hover);
        }
        let col = if stacked { Color32::WHITE } else { pal.text };
        crate::listing::paint_view_glyph(ui.painter(), Rect::from_center_size(pos2(lr.left() + 12.0, lr.center().y), Vec2::splat(11.0)), crate::listing::Mode::Stack, col);
        ui.painter().text(pos2(lr.left() + 26.0, lr.center().y), Align2::LEFT_CENTER, "Stack", FontId::proportional(12.5), col);
        set_stack = sresp.clicked();
        if let Some(v) = set {
            t.settings.zoom = v;
            t.dirty |= std::mem::take(&mut t.settings.column_view) | std::mem::take(&mut t.settings.stack_view);
            t.scroll_to_cursor = true;
        }
    });
    if set_columns {
        t.settings.column_view = true;
        t.settings.stack_view = false;
        t.dirty = true;
        t.scroll_to_cursor = true;
    }
    if set_stack {
        t.settings.stack_view = true;
        t.settings.column_view = false;
        t.dirty = true;
        t.scroll_to_cursor = true;
    }
    !clicked_outside(ctx, rect)
}

fn layouts(app: &mut App, ctx: &egui::Context, delete: bool) -> bool {
    let pal = app.pal;
    let names: Vec<String> = app.ud.layouts.keys().cloned().collect();
    let k = take_keys(ctx);
    if k.esc {
        return false;
    }
    let mut pick = None;
    let mut focus = false;
    let mut dummy = String::new();
    let (_, rect) = centered(ctx, "layouts", 420.0, &pal, |ui| {
        let _ = (&mut focus, &mut dummy);
        ui.label(egui::RichText::new(if delete { "Delete layout" } else { "Load layout" }).strong());
        ui.add_space(4.0);
        if names.is_empty() {
            ui.label(egui::RichText::new("No saved layouts yet. Use “Save Layout As…”.").color(pal.text_dim));
        }
        for n in &names {
            if menu_item(ui, n, None, &pal) {
                pick = Some(n.clone());
            }
        }
    });
    if let Some(n) = pick {
        if delete {
            app.ud.layouts.remove(&n);
            app.ud_dirty = true;
            app.toast(format!("Deleted layout “{n}”"));
        } else if let Some(l) = app.ud.layouts.get(&n).cloned() {
            app.apply_layout(&l);
        }
        return false;
    }
    !clicked_outside(ctx, rect)
}

fn prompt(app: &mut App, ctx: &egui::Context, title: &str, text: &mut String, on_ok: &mut Option<Box<dyn FnOnce(&mut App, String)>>) -> bool {
    let pal = app.pal;
    let mut ok = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    let mut cancel = false;
    centered(ctx, "prompt", 420.0, &pal, |ui| {
        dialog_title(ui, title, None, &pal);
        ui.add_space(8.0);
        let r = ui.add(egui::TextEdit::singleline(text).desired_width(f32::INFINITY));
        r.request_focus();
        dialog_buttons(ui, |ui| {
            if primary_button(ui, "OK", true, &pal).clicked() {
                ok = true;
            }
            if plain_button(ui, "Cancel", &pal).clicked() {
                cancel = true;
            }
        });
    });
    if ok {
        if let Some(f) = on_ok.take() {
            f(app, text.clone());
        }
        return false;
    }
    !cancel
}

fn connect(app: &mut App, ctx: &egui::Context, st: &mut ConnectState) -> bool {
    let pal = app.pal;
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    let uri = crate::network::normalize(&st.uri);
    let busy = app.connecting(&uri);
    // The dialog stays until the place opens; a failure comes back here as `error`.
    if let Some(e) = app.take_connect_error(&uri) {
        st.error = Some(e);
        st.password.clear();
    }
    let valid = crate::network::is_uri(&st.uri);
    let mut go = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) && valid && !busy;
    let mut cancel = false;
    let mut pick: Option<String> = None;
    centered(ctx, "connect", 440.0, &pal, |ui| {
        dialog_title(ui, "Connect to server", None, &pal);
        ui.add_space(8.0);
        let field = |ui: &mut Ui, label: &str, id: &'static str, text: &mut String, hint: &str, secret: bool, focus: bool| {
            ui.label(egui::RichText::new(label).small().color(pal.text_dim));
            let r = ui.add_enabled(!busy, egui::TextEdit::singleline(text).hint_text(egui::RichText::new(hint).color(pal.text_dim)).password(secret).desired_width(f32::INFINITY));
            crate::ui::note_control(ui, r.rect, id);
            if focus {
                r.request_focus();
            }
        };
        field(ui, "Address", "address field", &mut st.uri, "smb://server/share, sftp://user@host, ftp://host", false, st.focus);
        st.focus = false;
        ui.add_space(4.0);
        ui.columns(2, |cols| {
            field(&mut cols[0], "User", "user field", &mut st.user, "Optional", false, false);
            field(&mut cols[1], "Password", "password field", &mut st.password, "Optional", true, false);
        });
        ui.add_space(2.0);
        ui.add_enabled(!busy, egui::Checkbox::new(&mut st.anonymous, "Connect as a guest"));
        if let Some(e) = &st.error {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(e).color(pal.danger));
        }
        if !app.ud.servers.is_empty() {
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Recent").small().color(pal.text_dim));
            for s in app.ud.servers.iter().take(5) {
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(r, CornerRadius::same(5), pal.hover);
                }
                paint_glyph(ui.painter(), Rect::from_center_size(pos2(r.left() + 12.0, r.center().y), Vec2::splat(13.0)), Glyph::Globe, pal.text_dim);
                let g = crate::listing::elided(ui, s, FontId::proportional(app.cfg.font_size - 1.0), r.width() - 30.0, pal.text);
                ui.painter().galley(pos2(r.left() + 26.0, r.center().y - g.size().y / 2.0), g, pal.text);
                if resp.clicked() && !busy {
                    pick = Some(s.clone());
                }
            }
        }
        dialog_buttons(ui, |ui| {
            if primary_button(ui, "Connect", valid && !busy, &pal).clicked() {
                go = true;
            }
            if plain_button(ui, "Cancel", &pal).clicked() {
                cancel = true;
            }
            if busy {
                let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                crate::ui::spinner(ui.painter(), r.center(), 6.0, pal.accent, ui.input(|i| i.time));
                ui.ctx().request_repaint();
            }
        });
    });
    if let Some(s) = pick {
        st.uri = s;
        st.error = None;
    }
    if go {
        st.error = None;
        let login = crate::network::Login { user: st.user.trim().to_string(), password: st.password.clone(), anonymous: st.anonymous };
        let panel = app.active;
        app.connect_from_dialog(uri, login, panel);
    }
    !cancel && !app.connected(&crate::network::normalize(&st.uri))
}

fn archive_password(app: &mut App, ctx: &egui::Context, st: &mut PasswordState) -> bool {
    let pal = app.pal;
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    let mut go = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) && !st.text.is_empty();
    let mut cancel = false;
    let name = st.archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    centered(ctx, "archive password", 400.0, &pal, |ui| {
        dialog_title(ui, &format!("“{name}” is password-protected"), None, &pal);
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Password").small().color(pal.text_dim));
        let r = ui.add(egui::TextEdit::singleline(&mut st.text).password(true).desired_width(f32::INFINITY));
        crate::ui::note_control(ui, r.rect, "archive password field");
        r.request_focus();
        if r.changed() {
            st.wrong = false;
        }
        if st.wrong {
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Wrong password").color(pal.danger));
        }
        dialog_buttons(ui, |ui| {
            if primary_button(ui, "Unlock", !st.text.is_empty(), &pal).clicked() {
                go = true;
            }
            if plain_button(ui, "Cancel", &pal).clicked() {
                cancel = true;
            }
        });
    });
    if go {
        crate::archive::set_password(&st.archive, std::mem::take(&mut st.text));
        match st.retry.take() {
            Some(Retry::Job(kind)) => app.actions.push(Action::StartJob(kind)),
            Some(Retry::Reload(panel)) => {
                if let Some(p) = app.panels.get_mut(&panel) {
                    p.tab_mut().reload();
                }
            }
            Some(Retry::Open(path)) => app.open_from_archive(path),
            None => {}
        }
        return false;
    }
    !cancel
}

/// An encrypted drive's passphrase, hidden as typed, handed to the unlock job as it is
/// (the field's undo history, which keeps copies of it, goes too). Meanwhile the dialog
/// waits; a wrong passphrase comes back here, over an empty field.
fn unlock_drive(app: &mut App, ctx: &egui::Context, st: &mut UnlockState) -> bool {
    let pal = app.pal;
    let id = Id::new("unlock passphrase");
    let forget = || ctx.data_mut(|d| d.remove::<egui::text_edit::TextEditState>(id));
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        forget();
        return false;
    }
    let busy = crate::drives::busy(&st.drive.dev);
    let ok = !st.pass.0.is_empty() && !busy;
    let mut go = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) && ok;
    let mut cancel = false;
    let sub = format!("{} encrypted drive · {}", fsx::fmt_size(st.drive.size), st.drive.dev);
    centered(ctx, "unlock drive", 400.0, &pal, |ui| {
        dialog_title(ui, &format!("Unlock “{}”", st.drive.title()), Some(&sub), &pal);
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Passphrase").small().color(pal.text_dim));
        let r = ui.add_enabled(!busy, egui::TextEdit::singleline(&mut st.pass.0).id(id).password(true).desired_width(f32::INFINITY));
        crate::ui::note_control(ui, r.rect, "unlock passphrase");
        if !busy {
            r.request_focus();
        }
        if r.changed() {
            st.error = None;
        }
        let note = if busy { Some(("Unlocking…", pal.text_dim)) } else { st.error.as_deref().map(|e| (e, pal.danger)) };
        if let Some((text, col)) = note {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(text).color(col));
        }
        dialog_buttons(ui, |ui| {
            let b = primary_button(ui, "Unlock", ok, &pal);
            crate::ui::note_control(ui, b.rect, "unlock button");
            go |= b.clicked() && ok;
            cancel = plain_button(ui, "Cancel", &pal).clicked();
        });
    });
    if go || cancel {
        forget();
    }
    if go {
        st.error = None;
        match crate::drives::Job::unlock(st.drive.clone(), std::mem::take(&mut st.pass), st.open_in, ctx) {
            Ok(j) => app.drive_jobs.push(j),
            Err(e) => st.error = Some(e),
        }
    }
    !cancel
}

/// Compress: the name (prefilled, as the menu used to name it), the format and the
/// level as chips, and a password, typed twice, for a zip or a 7z. Enter compresses.
fn compress(app: &mut App, ctx: &egui::Context, st: &mut CompressState) -> bool {
    use crate::archwrite::{Format, Level};
    let pal = app.pal;
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    // Looked for once per name typed, not on every frame.
    let out = st.out();
    if st.taken.as_ref().is_none_or(|t| t.0 != st.name) {
        st.taken = Some((st.name.clone(), std::fs::symlink_metadata(&out).is_ok()));
    }
    let problem = st.problem();
    // Taken either way: Enter with the passwords not yet agreeing leaves the cursor where
    // it was rather than send it out of the field.
    let mut go = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) && problem.is_none();
    let mut cancel = false;
    let what = match st.srcs.as_slice() {
        [one] => format!("“{}”", one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
        v => format!("{} items", v.len()),
    };
    let lockable = st.lockable();
    centered(ctx, "compress", 420.0, &pal, |ui| {
        dialog_title(ui, &format!("Compress {what}"), None, &pal);
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Name").small().color(pal.text_dim));
        let r = ui.add(egui::TextEdit::singleline(&mut st.name).desired_width(f32::INFINITY));
        crate::ui::note_control(ui, r.rect, "compress name");
        if st.focus == Some(false) {
            r.request_focus();
            st.focus = None;
        }
        // An extension typed picks the format.
        if r.changed() {
            if let Some(f) = Format::of(&st.name) {
                st.format = f;
            }
        }
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            for f in Format::ALL {
                let what = match f {
                    Format::Zip => "compress zip",
                    Format::TarGz => "compress tar.gz",
                    Format::TarXz => "compress tar.xz",
                    Format::SevenZ => "compress 7z",
                };
                let tip = match f {
                    Format::Zip => "Opens anywhere; a password locks it with AES-256",
                    Format::TarGz => "Keeps Linux permissions and links; quick",
                    Format::TarXz => "Keeps Linux permissions and links; smaller, slower",
                    Format::SevenZ => "Small; a password locks the names too",
                };
                if crate::devtools::chip(ui, f.label(), st.format == f, what, tip, &pal, false) && st.format != f {
                    st.name = format!("{}{}", st.stem(), f.ext());
                    st.format = f;
                }
            }
        });
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            for l in Level::ALL {
                let (what, tip) = match l {
                    Level::Fast => ("compress fast", "Quickest, a bigger archive"),
                    Level::Normal => ("compress normal", "The tools' usual balance"),
                    Level::Smallest => ("compress smallest", "Slowest, the smallest archive"),
                };
                if crate::devtools::chip(ui, l.label(), st.level == l, what, tip, &pal, false) {
                    st.level = l;
                }
            }
        });
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Password").small().color(pal.text_dim));
        let hint = if lockable {
            "None"
        } else if st.format.lockable() {
            "A locked 7z needs the 7z command"
        } else {
            "Only zip and 7-Zip take one"
        };
        let r = ui.add_enabled(lockable, egui::TextEdit::singleline(&mut st.password).password(true).hint_text(hint).desired_width(f32::INFINITY));
        crate::ui::note_control(ui, r.rect, "compress password");
        if st.focus == Some(true) {
            r.request_focus();
            st.focus = None;
        }
        if lockable && !st.password.is_empty() {
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Again").small().color(pal.text_dim));
            let r = ui.add(egui::TextEdit::singleline(&mut st.again).password(true).desired_width(f32::INFINITY));
            crate::ui::note_control(ui, r.rect, "compress again");
        }
        match &problem {
            // The passwords are said to differ only once the second is being typed.
            Some(why) if !why.is_empty() && (!why.contains("passwords") || !st.again.is_empty()) => {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(why.as_str()).small().color(pal.danger));
            }
            _ => {}
        }
        dialog_buttons(ui, |ui| {
            let b = primary_button(ui, "Compress", problem.is_none(), &pal);
            crate::ui::note_control(ui, b.rect, "compress button");
            go |= b.clicked() && problem.is_none();
            if plain_button(ui, "Cancel", &pal).clicked() {
                cancel = true;
            }
        });
    });
    if go {
        *crate::archwrite::LAST_LEVEL.lock().unwrap_or_else(|e| e.into_inner()) = st.level;
        let password = (st.lockable() && !st.password.is_empty()).then(|| crate::archive::Secret(std::mem::take(&mut st.password)));
        st.again.clear();
        let out = st.out();
        app.actions.push(Action::StartJob(crate::ops::OpKind::Compress { srcs: std::mem::take(&mut st.srcs), out, password, level: st.level }));
        return false;
    }
    !cancel
}

/// `text` with each of `words` in it (any case) in `hit`'s format.
fn append_marked(job: &mut egui::text::LayoutJob, text: &str, words: &[String], plain: egui::TextFormat, hit: egui::TextFormat) {
    let low = text.to_lowercase();
    // Lowercasing that changes byte lengths (rare scripts) leaves the text unmarked.
    if words.is_empty() || low.len() != text.len() {
        job.append(text, 0.0, plain);
        return;
    }
    let mut marks = vec![false; text.len()];
    for w in words {
        let mut from = 0;
        while let Some(i) = low[from..].find(w.as_str()) {
            let s = from + i;
            marks[s..s + w.len()].iter_mut().for_each(|m| *m = true);
            from = s + w.len();
        }
    }
    let mut at = 0;
    while at < text.len() {
        let on = marks[at];
        let mut end = at;
        while end < text.len() && marks[end] == on {
            end += 1;
        }
        while !text.is_char_boundary(end) {
            end += 1;
        }
        job.append(&text[at..end], 0.0, if on { hit.clone() } else { plain.clone() });
        at = end;
    }
}

/// A passage starting a little before the first of `words` in it, so the match is in
/// view; as it is when none is found.
fn around(passage: &str, words: &[String]) -> String {
    let low = passage.to_lowercase();
    let first = (low.len() == passage.len()).then(|| words.iter().filter_map(|w| low.find(w.as_str())).min()).flatten();
    match first {
        Some(i) if i > 80 => {
            let from = passage.floor_char_boundary(i - 60);
            let from = passage[from..].find(' ').map_or(from, |s| from + s + 1);
            format!("…{}", &passage[from..])
        }
        _ => passage.to_string(),
    }
}

/// Ctrl+K: files and folders anywhere in the home folder by name, at once; with search by
/// meaning on, files by what they are about under them.
fn meaning(app: &mut App, ctx: &egui::Context, st: &mut MeaningState) -> bool {
    use crate::semantic::Phase;
    let pal = app.pal;
    let k = take_keys(ctx);
    if k.esc {
        return false;
    }
    let names = st.names.get_or_insert_with(|| crate::namefind::Names::start(crate::fsx::home(), ctx));
    let phase = app.semantic.as_ref().map(|e| e.phase());
    let index = app.semantic.as_ref().map(|e| e.index());
    let searchable = index.as_ref().is_some_and(|i| i.passages() > 0) && matches!(phase, Some(Phase::Ready | Phase::Indexing(..)));
    // A search for what was typed, once typing pauses; the newest question wins.
    if let Some(rx) = &st.job {
        if let Ok((q, r)) = rx.try_recv() {
            st.job = None;
            if q == st.query.trim() {
                match r {
                    Ok(h) => {
                        st.hits = h;
                        st.error = None;
                    }
                    Err(e) => st.error = Some(e),
                }
            }
        }
    }
    let q = st.query.trim().to_string();
    // Names: looked through again as the query changes, and as the walk brings more.
    let count = names.len();
    if (q != st.named_for.0 || count != st.named_for.1) && (q != st.named_for.0 || !names.walking() || st.typed.elapsed().as_millis() >= 150) {
        st.named = names.find(&q, 40);
        st.named_for = (q.clone(), count);
        st.sel = 0;
    }
    if names.walking() {
        ctx.request_repaint_after(std::time::Duration::from_millis(150));
    }
    if q.is_empty() {
        st.hits.clear();
        st.asked.clear();
    } else if searchable && q != st.asked && st.job.is_none() && st.typed.elapsed().as_millis() >= 200 {
        st.asked = q.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let (idx, c, asked) = (index.clone().unwrap_or_default(), ctx.clone(), q.clone());
        std::thread::spawn(move || {
            let r = crate::semantic::search(&idx, &asked, 30);
            let _ = tx.send((asked, r));
            c.request_repaint();
        });
        st.job = Some(rx);
    }
    if st.job.is_some() || (searchable && !q.is_empty() && q != st.asked) {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    // The rows: names first (a few when there are meaning matches too), then the
    // meaning matches not already among them.
    struct Row<'a> {
        path: &'a Path,
        is_dir: bool,
        passage: Option<&'a str>,
    }
    if q != st.asked {
        st.show_weak = false;
    }
    let meant: Vec<&crate::semantic::Hit> = if q == st.asked { st.hits.iter().filter(|h| !st.named.iter().any(|(p, _)| p == &h.path)).collect() } else { vec![] };
    // Matches far behind the best one wait under a link.
    let weak = meant.iter().filter(|h| !h.strong).count();
    let meant: Vec<&crate::semantic::Hit> = meant.into_iter().filter(|h| h.strong || st.show_weak).collect();
    let few = if meant.is_empty() { st.named.len() } else { st.named.len().min(5) };
    let rows: Vec<Row> = st.named[..few].iter().map(|(p, d)| Row { path: p, is_dir: *d, passage: None }).chain(meant.iter().map(|h| Row { path: &h.path, is_dir: false, passage: Some(&h.passage) })).collect();
    step(&mut st.sel, rows.len(), &k, |_| false);
    let mut open: Option<PathBuf> = if k.enter { rows.get(st.sel).map(|r| r.path.to_path_buf()) } else { None };
    let mut turn_on = false;
    let (mut download, mut retry) = (false, false);
    let fs = app.cfg.font_size;
    let words = crate::semantic::terms(&st.query);
    let marks: Vec<String> = st.query.split_whitespace().map(str::to_lowercase).collect();
    // The list may use most of the window's height; it claims what its rows need, since
    // the window keeps its first (empty) size otherwise and would show them in a sliver.
    let list_max = (ctx.content_rect().height() - 260.0).max(160.0);
    let thumbs = &mut app.thumbs;
    let ppp = ctx.pixels_per_point();
    if st.entries.len() > 400 {
        st.entries.clear();
    }
    for r in &rows {
        st.entries.entry(r.path.to_path_buf()).or_insert_with(|| fsx::Entry::from_path(r.path));
    }
    let entries = &st.entries;
    let walking = names.walking();
    let mut more = false;
    centered(ctx, "meaning", 760.0, &pal, |ui| {
        let hint = if phase.is_some() { "Search by name, or describe what you are looking for…" } else { "Search files and folders by name…" };
        let r = search_field(ui, &mut st.query, hint, &mut st.focus, &pal, Glyph::Search);
        crate::ui::note_control(ui, r.rect, "meaning field");
        if r.changed() {
            st.typed = Instant::now();
        }
        ui.add_space(2.0);
        let dim = |ui: &mut Ui, s: &str| {
            ui.label(egui::RichText::new(s).size(fs - 1.5).color(pal.text_dim));
        };
        // Where search by meaning is: a line of status, and a button when there is a
        // step to take. Off, a quiet way to turn it on while nothing is typed.
        match phase.clone() {
            None if q.is_empty() => {
                ui.horizontal(|ui| {
                    dim(ui, "Search by meaning is off.");
                    let b = ui.add(egui::Button::new(egui::RichText::new("Turn on").size(fs - 1.5).color(pal.accent)).frame(false));
                    crate::ui::note_control(ui, b.rect, "turn on search by meaning");
                    turn_on = b.clicked();
                });
            }
            None => {}
            Some(Phase::Checking) => dim(ui, "Looking for Ollama…"),
            Some(Phase::NotRunning) => {
                dim(ui, "Ollama isn't running, so this finds names only. Install it (on Arch: pacman -S ollama) and start it, then try again.");
                retry = ui.button("Try again").clicked();
            }
            Some(Phase::NoModel) => {
                dim(ui, "The model (EmbeddingGemma 2, for text and pictures) isn't downloaded yet.");
                download = ui.add(egui::Button::new(egui::RichText::new("Download, about 860 MB").color(Color32::WHITE)).fill(pal.accent)).clicked();
            }
            Some(Phase::Downloading(done, all)) => {
                let f = if all > 0 { done as f32 / all as f32 } else { 0.0 };
                ui.add(egui::ProgressBar::new(f).desired_height(5.0).fill(pal.accent));
                dim(ui, &format!("Downloading the model… {} of {}", fsx::fmt_size(done), fsx::fmt_size(all)));
            }
            Some(Phase::Indexing(n, all)) => dim(ui, &format!("Reading your files for search by meaning… {n} of {all}")),
            Some(Phase::Ready) if q.is_empty() => {
                let read = index.as_ref().map_or(0, |i| i.files());
                match app.semantic.as_ref().map_or(0, |e| e.waiting()) {
                    0 => dim(ui, &format!("{read} files read for search by meaning")),
                    w => dim(ui, &format!("{read} files read · {w} pictures wait until the laptop is plugged in")),
                }
            }
            Some(Phase::Ready) => {}
            Some(Phase::Failed(e)) => {
                ui.label(egui::RichText::new(e).color(pal.danger));
                retry = ui.button("Try again").clicked();
            }
        }
        if let Some(e) = &st.error {
            ui.label(egui::RichText::new(e).color(pal.danger));
        }
        if !rows.is_empty() {
            ui.add_space(4.0);
        }
        let small = fs - 1.5;
        let row_h = (fs * 1.35 + 2.0 * small * 1.35 + 16.0).round();
        let name_h = (fs * 1.35 + small * 1.35 + 16.0).round();
        let label_h = (small * 1.35 + 10.0).round();
        let both = few > 0 && !meant.is_empty();
        let rows_h: f32 = rows.iter().map(|r| if r.passage.is_some() { row_h } else { name_h } + 2.0).sum::<f32>() + if both { 2.0 * label_h } else { 0.0 };
        let home = crate::fsx::home();
        egui::ScrollArea::vertical().max_height(list_max).min_scrolled_height(rows_h.min(list_max)).auto_shrink([false, true]).show(ui, |ui| {
            for (i, row) in rows.iter().enumerate() {
                if both && (i == 0 || i == few) {
                    let (lr, _) = ui.allocate_exact_size(vec2(ui.available_width(), label_h), Sense::hover());
                    let text = if i == 0 { "Names" } else { "By meaning" };
                    ui.painter().text(pos2(lr.left() + 12.0, lr.bottom() - 4.0), Align2::LEFT_BOTTOM, text, FontId::proportional(small), pal.text_dim);
                }
                let name = row.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let folder = row.path.parent().map(|p| match p.strip_prefix(&home) {
                    Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
                    Ok(rest) => format!("~/{}", rest.display()),
                    Err(_) => p.display().to_string(),
                }).unwrap_or_default();
                let h = if row.passage.is_some() { row_h } else { name_h };
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
                ui.add_space(2.0);
                if i == st.sel {
                    ui.painter().rect_filled(r, CornerRadius::same(6), pal.selected);
                } else if resp.hovered() {
                    ui.painter().rect_filled(r, CornerRadius::same(6), pal.hover);
                }
                let fmt = |size: f32, color: Color32| egui::TextFormat { font_id: FontId::proportional(size), color, ..Default::default() };
                // A picture shows itself, small; anything else its icon.
                let picture = !row.is_dir && crate::semantic::is_picture(row.path);
                let side = (r.height() - 12.0).min(40.0);
                let box_r = Rect::from_min_size(pos2(r.left() + 12.0, r.center().y - side / 2.0), Vec2::splat(side));
                let mut drawn = false;
                if picture {
                    if let Some(l) = thumbs.get(row.path, None, box_r.width() * ppp) {
                        ui.painter().rect_filled(box_r, CornerRadius::same(4), pal.raised);
                        let [tw, th] = l.tex.size();
                        let k = (box_r.width() / tw as f32).min(box_r.height() / th as f32);
                        let sz = vec2(tw as f32 * k, th as f32 * k);
                        ui.painter().image(l.tex.id(), Rect::from_center_size(box_r.center(), sz), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                        drawn = true;
                    }
                }
                if !drawn {
                    let ic = Rect::from_center_size(box_r.center(), Vec2::splat(22.0_f32.min(side)));
                    match entries.get(row.path).and_then(|e| e.as_ref()) {
                        Some(e) => {
                            if !crate::icons::entry(ui.painter(), ic, e) {
                                theme::paint_icon(ui.painter(), ic, theme::categorize(&e.ext(), e.is_dir, e.is_executable()), &e.ext(), e.is_link);
                            }
                        }
                        None => theme::paint_icon(ui.painter(), ic, theme::categorize("", row.is_dir, false), "", false),
                    }
                }
                let x = box_r.right() + 10.0;
                let w = r.right() - 12.0 - x;
                let mut job = egui::text::LayoutJob::default();
                let marked: &[String] = if row.passage.is_some() { &words } else { &marks };
                // On the selected row the dim grey and the accent sink into the selection.
                let sel = i == st.sel;
                let (text, dim, mark) = if sel { (pal.on_sel(), pal.on_sel_dim(), pal.on_sel()) } else { (pal.text, pal.text_dim, pal.accent) };
                append_marked(&mut job, &name, marked, fmt(fs, text), fmt(fs, mark));
                if row.passage.is_some() {
                    job.append(&format!("   {folder}"), 0.0, fmt(small, dim));
                }
                job.wrap = egui::text::TextWrapping { max_width: w, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
                let g = ui.fonts_mut(|f| f.layout_job(job));
                ui.painter().galley(pos2(x, r.top() + 7.0), g, pal.text);
                // Under a name, its folder; under a meaning match, the passage around the
                // question's words (in the accent).
                let mut job = egui::text::LayoutJob::default();
                match row.passage {
                    None => job.append(&folder, 0.0, fmt(small, dim)),
                    Some(_) if picture => job.append("Picture", 0.0, fmt(small, dim)),
                    Some(p) => append_marked(&mut job, &around(p, &words), &words, fmt(small, dim), fmt(small, mark)),
                }
                let lines = if row.passage.is_some() { 2 } else { 1 };
                job.wrap = egui::text::TextWrapping { max_width: w, max_rows: lines, break_anywhere: row.passage.is_none(), overflow_character: Some('…') };
                let g = ui.fonts_mut(|f| f.layout_job(job));
                ui.painter().galley(pos2(x, r.top() + 7.0 + fs * 1.35 + 2.0), g, pal.text_dim);
                if i == st.sel && (k.up || k.down) {
                    ui.scroll_to_rect(r, None);
                }
                if resp.clicked() {
                    open = Some(row.path.to_path_buf());
                }
            }
            if weak > 0 && !st.show_weak {
                let b = ui.add(egui::Button::new(egui::RichText::new(format!("Show {weak} weaker {}", if weak == 1 { "match" } else { "matches" })).size(small).color(pal.accent)).frame(false));
                crate::ui::note_control(ui, b.rect, "weaker matches");
                more = b.clicked();
            }
            if rows.is_empty() && !q.is_empty() && !walking && st.job.is_none() && (!searchable || q == st.asked) && st.error.is_none() {
                let text = if searchable { "Nothing by that name, and nothing that stands out by meaning" } else { "Nothing by that name" };
                ui.label(egui::RichText::new(text).color(pal.text_dim));
            }
        });
    });
    if more {
        st.show_weak = true;
    }
    if turn_on {
        app.cfg.semantic_search = true;
        app.set_config_changed();
    }
    if let Some(e) = &app.semantic {
        if download {
            e.download();
        }
        if retry {
            e.poke();
        }
    }
    if let Some(path) = open {
        // A folder opened; a file shown in its folder, selected.
        app.actions.push(Action::Navigate { panel: app.active, path });
        return false;
    }
    true
}

/// The theme picker's state for this run of Maia, kept in egui's memory (the popup has
/// none of its own): the search and the focused card for one opening, the All, Dark or
/// Light filter for the session.
#[derive(Clone, Default)]
struct ThemesState {
    query: String,
    /// 0 All, 1 Dark, 2 Light.
    filter: u8,
    focus: Option<ThemeName>,
    /// The arrows have been used: the focus ring shows and the focused theme previews.
    keyed: bool,
    scroll_to_focus: bool,
    search_focus: bool,
    search_id: Option<Id>,
    /// Where each card was drawn last frame, for Up and Down across rows.
    cards: Vec<(ThemeName, Pos2)>,
    /// The pass this was last drawn in: a gap means the picker was opened again.
    pass: u64,
}

/// The theme picker: every theme as a small window in a card, searchable and filtered to
/// the dark or light ones. Hovering a card, or reaching it with the arrows, shows that
/// theme on the whole window; a click or Enter keeps it, Esc goes back to Options with
/// the chosen one restored.
fn themes(app: &mut App, ctx: &egui::Context) -> bool {
    let pal = app.pal;
    let sid = Id::new("themes state");
    let mut st: ThemesState = ctx.data_mut(|d| d.get_temp(sid)).unwrap_or_default();
    let pass = ctx.cumulative_pass_nr();
    let current = app.cfg.theme;
    let fresh = st.pass + 1 < pass || st.pass == 0;
    if fresh {
        // Opened again: a fresh search, the chosen theme focused, the filter kept.
        st = ThemesState { filter: st.filter, focus: Some(current), search_focus: true, scroll_to_focus: true, ..Default::default() };
    }
    st.pass = pass;
    // Keys: the search keeps typing; arrows, Home and End move across the cards.
    let focused = ctx.memory(|m| m.focused());
    let in_search = focused.is_some() && focused == st.search_id;
    let other_field = focused.is_some_and(|f| !in_search && egui::TextEdit::load_state(ctx, f).is_some());
    let (left, right, up, down, home, end, enter, esc) = ctx.input_mut(|i| {
        let mut k = |key| !other_field && i.consume_key(Modifiers::NONE, key);
        (k(Key::ArrowLeft), k(Key::ArrowRight), k(Key::ArrowUp), k(Key::ArrowDown), k(Key::Home), k(Key::End), k(Key::Enter), i.consume_key(Modifiers::NONE, Key::Escape))
    });
    if !in_search && !other_field {
        let typed = ctx.input_mut(|i| {
            let mut t = String::new();
            i.events.retain(|e| match e {
                Event::Text(x) => {
                    t.push_str(x);
                    false
                }
                _ => true,
            });
            t
        });
        if !typed.is_empty() {
            st.query.push_str(&typed);
            st.search_focus = true;
        }
    }
    let q = fuzzy::Query::new(&st.query);
    // A name matches loosely, as names do elsewhere; its line by whole words (letters
    // scattered through a sentence matched almost anything).
    let words: Vec<String> = st.query.to_lowercase().split_whitespace().map(String::from).collect();
    let matches = |t: ThemeName| {
        let line = format!("{} {}", t.label(), t.blurb()).to_lowercase();
        q.is_empty() || fuzzy::score(&q, &t.label().to_lowercase()).is_some() || words.iter().all(|w| line.contains(w.as_str()))
    };
    let groups: Vec<(&str, Vec<ThemeName>)> = [("Dark", false), ("Light", true)]
        .into_iter()
        .filter(|(_, light)| st.filter == 0 || (st.filter == 2) == *light)
        .map(|(name, light)| (name, ThemeName::ALL.into_iter().filter(|t| t.is_light() == light && matches(*t)).collect::<Vec<_>>()))
        .filter(|(_, v)| !v.is_empty())
        .collect();
    let order: Vec<ThemeName> = groups.iter().flat_map(|g| g.1.iter().copied()).collect();
    if st.focus.is_none_or(|f| !order.contains(&f)) {
        st.focus = order.iter().copied().find(|&t| t == current).or_else(|| order.first().copied());
    }
    let at = st.focus.and_then(|f| order.iter().position(|&t| t == f));
    let mut moved = None;
    if let Some(i) = at {
        if left && i > 0 {
            moved = Some(order[i - 1]);
        }
        if right && i + 1 < order.len() {
            moved = Some(order[i + 1]);
        }
        if home {
            moved = order.first().copied();
        }
        if end {
            moved = order.last().copied();
        }
        if up || down {
            // The nearest card in the row above or below, by its centre.
            let here = st.cards.iter().find(|c| Some(c.0) == st.focus).map(|c| c.1);
            if let Some(h) = here {
                let row_y = st.cards.iter().map(|c| c.1.y).filter(|&y| if down { y > h.y + 1.0 } else { y < h.y - 1.0 }).fold(None, |best: Option<f32>, y| Some(best.map_or(y, |b| if down { b.min(y) } else { b.max(y) })));
                if let Some(y) = row_y {
                    moved = st.cards.iter().filter(|c| (c.1.y - y).abs() < 1.0).min_by(|a, b| (a.1.x - h.x).abs().total_cmp(&(b.1.x - h.x).abs())).map(|c| c.0);
                }
            }
        }
    }
    if let Some(t) = moved {
        st.focus = Some(t);
        st.keyed = true;
        st.scroll_to_focus = true;
    }
    let mut done = esc;
    let mut picked = if enter { st.focus } else { None };
    if picked.is_some() {
        done = true;
    }
    let mut hovered = None;
    let mut syntax = app.cfg.syntax_theme;
    let mut cards = vec![];
    // Options' size: a fixed window that shrinks with a small one.
    let screen = ctx.content_rect();
    let w = 860f32.min(screen.width() - 24.0).round();
    let h = 600f32.min(screen.height() - 24.0).round();
    let pos = pos2((screen.center().x - w / 2.0).round(), (screen.top() + ((screen.height() - h) / 2.0).clamp(12.0, 70.0)).round());
    note_layer(Id::new("themes"));
    let area = egui::Area::new(Id::new("themes")).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
        crate::frost::behind(ui, DIALOG_RADIUS);
        dialog_frame(&pal).show(ui, |ui| {
            let (_, inner) = ui.allocate_space(vec2(w - 2.0 * DIALOG_PAD, h - 2.0 * DIALOG_PAD));
            let top_down = egui::Layout::top_down(egui::Align::Min);
            // Header: the close button and the filter from the right; the search takes
            // what is left.
            let mut head = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(top_down));
            head.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if crate::ui::glyph_button(ui, Glyph::Close, "Back to Options (Esc)", true, &pal).clicked() {
                    done = true;
                }
                ui.add_space(4.0);
                let mut seg = |ui: &mut Ui| {
                    for (k, label, id) in [(0u8, "All", "all themes"), (1, "Dark", "dark themes"), (2, "Light", "light themes")] {
                        let g = ui.painter().layout_no_wrap(label.to_string(), FontId::proportional(12.0), pal.text);
                        let (r, resp) = ui.allocate_exact_size(vec2(g.size().x + 18.0, 20.0), Sense::click());
                        crate::ui::note_control(ui, r, id);
                        let on = st.filter == k;
                        if on {
                            ui.painter().rect_filled(r, CornerRadius::same(4), pal.accent.gamma_multiply(0.85));
                        } else if resp.hovered() {
                            ui.painter().rect_filled(r, CornerRadius::same(4), pal.hover);
                        }
                        let c = if on { Color32::WHITE } else { pal.text_dim };
                        ui.painter().text(r.center(), Align2::CENTER_CENTER, label, FontId::proportional(12.0), c);
                        if resp.clicked() {
                            st.filter = k;
                            st.scroll_to_focus = true;
                        }
                    }
                };
                // Laid out left to right in a slot of its measured width (in a right-to-left
                // row the cursor's left edge is infinite), centred on the search row.
                let seg_w = ["All", "Dark", "Light"].iter().map(|l| ui.painter().layout_no_wrap(l.to_string(), FontId::proportional(12.0), pal.text).size().x + 18.0 + 1.0).sum::<f32>() + 4.0 + 6.0;
                let (slot, _) = ui.allocate_exact_size(vec2(seg_w, 26.0), Sense::hover());
                let mut sui = ui.new_child(egui::UiBuilder::new().max_rect(slot).layout(egui::Layout::left_to_right(egui::Align::Center)));
                crate::inspector::seg_frame(&mut sui, &pal, |ui| seg(ui));
                let fw = (ui.available_width() - 4.0).floor();
                ui.allocate_ui_with_layout(vec2(fw, 0.0), top_down, |ui| {
                    ui.set_width(fw);
                    let r = search_field(ui, &mut st.query, "Search themes…", &mut st.search_focus, &pal, Glyph::Search);
                    crate::ui::note_control(ui, r.rect, "theme search");
                    st.search_id = Some(r.id);
                    // Typed while the search had not the keyboard: the cursor after it.
                    if r.gained_focus() {
                        if let Some(mut s) = egui::TextEdit::load_state(ui.ctx(), r.id) {
                            s.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(st.query.chars().count()))));
                            s.store(ui.ctx(), r.id);
                        }
                    }
                });
            });
            // Footer: the code colours' switch on the left, Done on the right.
            let foot = Rect::from_min_max(pos2(inner.left(), inner.bottom() - 28.0), inner.max);
            ui.painter().line_segment([pos2(inner.left(), foot.top() - 6.5), pos2(inner.right(), foot.top() - 6.5)], Stroke::new(1.0, pal.border));
            let mut fui = ui.new_child(egui::UiBuilder::new().max_rect(foot).layout(egui::Layout::left_to_right(egui::Align::Center)));
            fui.spacing_mut().item_spacing.x = 10.0;
            fui.add_space(4.0);
            let sw = toggle(&mut fui, &mut syntax, &pal);
            let _ = sw;
            let lr = fui.label(egui::RichText::new("Code colors follow the theme").size(12.5).color(pal.text));
            if lr.interact(Sense::click()).clicked() {
                syntax = !syntax;
            }
            fui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let b = primary_button(ui, "Done", true, &pal);
                crate::ui::note_control(ui, b.rect, "themes done");
                if b.clicked() {
                    done = true;
                }
            });
            // The cards, in a grid that follows the width.
            let body = Rect::from_min_max(pos2(inner.left(), head.min_rect().bottom().round() + 4.0), pos2(inner.right(), foot.top() - 14.0));
            let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(body).layout(top_down));
            let gap = 12.0;
            let pad = 6.0;
            // Options' type: its rows' titles and lines are of fixed sizes.
            let name_font = FontId::proportional(12.5);
            let blurb_font = FontId::proportional(11.0);
            // Wide enough for the longest name: names are never cut.
            let longest = ThemeName::ALL.iter().map(|t| bui.painter().layout_no_wrap(t.label().to_string(), name_font.clone(), pal.text).size().x).fold(0.0, f32::max);
            let avail = body.width() - 10.0;
            // (and the chosen one's check beside it).
            let min_w = (longest + 2.0 * pad + 8.0 + 24.0).max(150.0);
            let most = (((avail + gap) / (min_w + gap)).floor() as usize).clamp(2, 5);
            // As many columns as fit: a gap or two in a group's last row beats cards
            // made needlessly big.
            let cols = most;
            let cw = ((avail - gap * (cols as f32 - 1.0)) / cols as f32).floor();
            // Shorter in a short window, so more than one row shows.
            let preview_h = (cw * 0.52).clamp(64.0, 118.0).min(((body.height() - 90.0) * 0.6).max(64.0)).round();
            let name_h = (12.5f32 * 1.35).round();
            let blurb_h = (11.0f32 * 1.35).round();
            let card_h = pad + preview_h + 8.0 + 4.0 + 8.0 + name_h + blurb_h + pad + 4.0;
            egui::ScrollArea::vertical().id_salt("theme cards").max_height(body.height()).auto_shrink([false, false]).show(&mut bui, |ui| {
                ui.spacing_mut().item_spacing = vec2(gap, gap);
                if groups.is_empty() {
                    ui.add_space(28.0);
                    ui.vertical_centered(|ui| ui.label(egui::RichText::new("No theme matches").size(12.5).color(pal.text_dim)));
                }
                for (name, list) in &groups {
                    let (hr, _) = ui.allocate_exact_size(vec2(avail, 18.0), Sense::hover());
                    ui.painter().text(pos2(hr.left() + 4.0, hr.center().y), Align2::LEFT_CENTER, format!("{name} · {}", list.len()), FontId::proportional(11.5), pal.text_dim);
                    for row in list.chunks(cols) {
                        ui.horizontal(|ui| {
                            for &t in row {
                                let (r, resp) = ui.allocate_exact_size(vec2(cw, card_h), Sense::click());
                                crate::ui::note_control(ui, r, t.label());
                                cards.push((t, r.center()));
                                let chosen = t == current;
                                let focus = st.keyed && st.focus == Some(t);
                                let hov = resp.hovered();
                                if focus && std::mem::take(&mut st.scroll_to_focus) {
                                    ui.scroll_to_rect(r.expand(gap / 2.0), None);
                                }
                                let p = theme::palette(t);
                                ui.painter().rect(r, CornerRadius::same(12), if hov { pal.hover } else { pal.card() }, Stroke::new(1.0, if hov { pal.text_dim.gamma_multiply(0.5) } else { pal.card_line() }), StrokeKind::Inside);
                                let preview = Rect::from_min_size(r.min + vec2(pad, pad), vec2(cw - 2.0 * pad, preview_h));
                                theme_card(ui.painter(), preview, p, 6);
                                // Its colours: the accent, then the five code colours.
                                let s = p.syntax;
                                let sw = [p.accent, s.keyword, s.kind, s.string, s.number, s.comment];
                                let strip = Rect::from_min_size(pos2(preview.left(), preview.bottom() + 8.0), vec2(preview.width(), 4.0));
                                let seg_w = (strip.width() - 3.0 * (sw.len() as f32 - 1.0)) / sw.len() as f32;
                                for (k, c) in sw.iter().enumerate() {
                                    ui.painter().rect_filled(Rect::from_min_size(pos2(strip.left() + k as f32 * (seg_w + 3.0), strip.top()), vec2(seg_w, strip.height())), CornerRadius::same(2), *c);
                                }
                                // The name, then its line, dim.
                                let ty = strip.bottom() + 8.0;
                                for (k, (text, font, color)) in [(t.label(), &name_font, pal.text), (t.blurb(), &blurb_font, pal.text_dim)].into_iter().enumerate() {
                                    let g = crate::listing::elided(ui, text, font.clone(), preview.width() - if chosen && k == 0 { 22.0 } else { 0.0 }, color);
                                    ui.painter().galley(pos2(preview.left(), ty + if k == 0 { 0.0 } else { name_h }), g, color);
                                }
                                // Chosen: an accent ring and a check badge; focused from the
                                // keyboard: a ring in the text colour.
                                if chosen {
                                    ui.painter().rect_stroke(r, CornerRadius::same(12), Stroke::new(2.0, pal.accent), StrokeKind::Inside);
                                    let c = pos2(preview.right() - 8.0, ty + name_h / 2.0);
                                    ui.painter().circle_filled(c, 8.0, pal.accent);
                                    ui.painter().add(egui::Shape::line(vec![c + vec2(-3.5, 0.2), c + vec2(-1.0, 2.7), c + vec2(3.7, -2.3)], Stroke::new(1.6, Color32::WHITE)));
                                }
                                if focus {
                                    ui.painter().rect_stroke(r.expand(3.0), CornerRadius::same(15), Stroke::new(1.5, pal.text.gamma_multiply(0.8)), StrokeKind::Inside);
                                }
                                if hov {
                                    hovered = Some(t);
                                }
                                if resp.clicked() {
                                    picked = Some(t);
                                    st.focus = Some(t);
                                }
                            }
                        });
                    }
                    ui.add_space(6.0);
                }
            });
        });
    });
    crate::ui::note_popup(area.response.rect);
    st.cards = cards;
    // A hovered card, or one reached with the arrows, shows on the whole window until the
    // pointer or the focus leaves it; then the chosen theme is back.
    let preview = hovered.or(if st.keyed { st.focus } else { None });
    if picked.is_none() {
        if let Some(t) = preview.filter(|&t| t != current) {
            app.preview_theme = Some(t);
        }
    }
    if let Some(t) = picked {
        app.cfg.theme = t;
        app.set_config_changed();
    }
    if syntax != app.cfg.syntax_theme {
        app.cfg.syntax_theme = syntax;
        app.set_config_changed();
    }
    // (Not the click that opened it.)
    let clicked_out = !fresh && clicked_outside(ctx, area.response.rect);
    ctx.data_mut(|d| d.insert_temp(sid, st));
    if done || clicked_out {
        app.popup = Popup::options();
        return false;
    }
    true
}

/// A theme as a small window: its sidebar, title strip, three rows (one selected), an
/// accent button, and two lines of code in its code colours. `rad` rounds its corners.
pub(crate) fn theme_card(painter: &egui::Painter, r: Rect, p: Palette, rad: u8) {
    let round = CornerRadius::same(rad);
    painter.rect_filled(r, round, p.panel);
    let side = Rect::from_min_max(r.min, pos2(r.left() + (r.width() * 0.28).round(), r.bottom()));
    painter.rect_filled(side, CornerRadius { nw: rad, sw: rad, ne: 0, se: 0 }, p.sidebar);
    let bar = |x: f32, y: f32, w: f32, c: Color32| painter.rect_filled(Rect::from_min_size(pos2(x, y), vec2(w, 4.0)), CornerRadius::same(2), c);
    for (k, w) in [0.62f32, 0.48, 0.55].into_iter().enumerate() {
        bar(side.left() + 7.0, side.top() + 12.0 + k as f32 * 11.0, (side.width() - 14.0) * w, p.text_dim.gamma_multiply(0.7));
    }
    let main = Rect::from_min_max(pos2(side.right(), r.top()), r.max);
    painter.rect_filled(Rect::from_min_size(main.min, vec2(main.width(), 9.0)), CornerRadius { nw: 0, ne: rad, sw: 0, se: 0 }, p.chrome);
    // As many rows as fit above the code, the second one selected.
    // Of a fixed size, as the picker's type is: at larger fonts the comment fell off.
    let size = 8.5;
    let code_top = main.bottom() - 8.0 - 2.0 * size * 1.35;
    let mut k = 0;
    while main.top() + 14.0 + k as f32 * 11.0 + 8.0 <= code_top - 4.0 {
        let y = main.top() + 14.0 + k as f32 * 11.0;
        let row = Rect::from_min_size(pos2(main.left() + 5.0, y - 3.0), vec2(main.width() - 10.0, 10.0));
        if k == 1 {
            painter.rect_filled(row, CornerRadius::same(3), p.selected);
        }
        bar(row.left() + 5.0, y, row.width() * [0.55, 0.42, 0.62, 0.36, 0.5][k % 5], if k == 1 { p.text } else { p.text.gamma_multiply(0.85) });
        k += 1;
    }
    // The accent: below the title strip when there is room above the code, else in it
    // (a small card's code starts right under the strip and the pill sat on it).
    let pill = Rect::from_min_size(pos2(main.right() - 26.0, main.top() + 13.0), vec2(18.0, 7.0));
    let pill = if pill.bottom() <= code_top - 3.0 { pill } else { Rect::from_min_size(pos2(main.right() - 22.0, main.top() + 2.5), vec2(14.0, 4.0)) };
    painter.rect_filled(pill, CornerRadius::same(4), p.accent);
    // Code, in the theme's code colours: a shorter sample where the whole one would be
    // cut in the middle of a word.
    let s = p.syntax;
    let long: [&[(&str, Color32)]; 2] = [&[("fn", s.keyword), (" add", p.text), ("(n: ", p.text), ("u32", s.kind), (")", p.text)], &[("\"sum\"", s.string), (" + ", p.text), ("42", s.number), (" // ok", s.comment)]];
    let short: [&[(&str, Color32)]; 2] = [&[("fn", s.keyword), (" add", p.text)], &[("\"sum\"", s.string), (" 42", s.number)]];
    let width = |line: &[(&str, Color32)]| painter.ctx().fonts_mut(|f| f.layout_no_wrap(line.iter().map(|x| x.0).collect(), FontId::monospace(size), p.text).size().x);
    let lines = if width(long[0]) > main.width() - 12.0 { short } else { long };
    for (k, line) in lines.iter().enumerate() {
        let mut job = egui::text::LayoutJob::default();
        for (t, c) in line.iter() {
            job.append(t, 0.0, egui::TextFormat { font_id: FontId::monospace(size), color: *c, ..Default::default() });
        }
        job.wrap = egui::text::TextWrapping { max_width: main.width() - 12.0, max_rows: 1, break_anywhere: true, overflow_character: None };
        let g = painter.ctx().fonts_mut(|f| f.layout_job(job));
        painter.galley(pos2(main.left() + 8.0, main.bottom() - 8.0 - (2 - k) as f32 * (size * 1.35)), g, p.text);
    }
    painter.rect_stroke(r, round, Stroke::new(1.0, p.border), StrokeKind::Inside);
}

fn confirm(app: &mut App, ctx: &egui::Context, msg: &str, ok_label: &str, on_ok: &mut Option<Box<dyn FnOnce(&mut App)>>) -> bool {
    let pal = app.pal;
    let mut ok = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
    let mut cancel = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
    // Dim the background for destructive confirmations.
    let screen = ctx.content_rect();
    ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, Id::new("dim"))).rect_filled(screen, CornerRadius::ZERO, Color32::from_black_alpha(90));
    centered(ctx, "confirm", 440.0, &pal, |ui| {
        ui.add_space(4.0);
        dialog_title(ui, msg, None, &pal);
        dialog_buttons(ui, |ui| {
            if ui.add(egui::Button::new(egui::RichText::new(ok_label).color(Color32::WHITE)).fill(pal.danger).min_size(vec2(72.0, 0.0))).clicked() {
                ok = true;
            }
            if plain_button(ui, "Cancel", &pal).clicked() {
                cancel = true;
            }
        });
    });
    if ok {
        if let Some(f) = on_ok.take() {
            f(app);
        }
        return false;
    }
    !cancel
}

/// A question and its answers as buttons, the first the one Enter gives; one that
/// can't be undone is drawn as a warning and never the first.
fn choose(app: &mut App, ctx: &egui::Context, msg: &str, sub: &str, choices: &mut [(String, bool, Option<Box<dyn FnOnce(&mut App)>>)]) -> bool {
    let pal = app.pal;
    let mut pick = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)).then_some(0);
    let mut cancel = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
    centered(ctx, "choose", 460.0, &pal, |ui| {
        ui.add_space(4.0);
        dialog_title(ui, msg, (!sub.is_empty()).then_some(sub), &pal);
        dialog_buttons(ui, |ui| {
            // Right to left: the first answer at the right edge.
            for (i, (label, risky, _)) in choices.iter().enumerate() {
                let b = if *risky {
                    ui.add(egui::Button::new(egui::RichText::new(label.as_str()).color(Color32::WHITE)).fill(pal.danger).min_size(vec2(72.0, 0.0)))
                } else if i == 0 {
                    primary_button(ui, label, true, &pal)
                } else {
                    plain_button(ui, label, &pal)
                };
                crate::ui::note_control(ui, b.rect, "choose answer");
                if b.clicked() {
                    pick = Some(i);
                }
            }
            if plain_button(ui, "Cancel", &pal).clicked() {
                cancel = true;
            }
        });
    });
    if let Some(f) = pick.and_then(|i| choices.get_mut(i)).and_then(|c| c.2.take()) {
        f(app);
        return false;
    }
    !cancel
}

fn conflict(app: &mut App, ctx: &egui::Context, st: &mut ConflictState) -> bool {
    let pal = app.pal;
    let Some(item) = st.items.get(st.idx) else { return false };
    let both_dirs = item.src.is_dir() && item.dst.is_dir();
    let (a, b) = (fsx::Entry::from_path(&item.src), fsx::Entry::from_path(&item.dst));
    // Which of the two is newer, and bigger, said beside it: what decides it at a glance.
    let info = |e: &Option<fsx::Entry>, o: &Option<fsx::Entry>| -> String {
        let Some(e) = e else { return String::new() };
        let mut v = if e.is_dir { format!("Folder · modified {}", fsx::fmt_time(e.modified)) } else { format!("{} · modified {}", fsx::fmt_size(e.size), fsx::fmt_time(e.modified)) };
        if let Some(o) = o {
            if e.modified.zip(o.modified).is_some_and(|(a, b)| a.duration_since(b).is_ok_and(|d| d.as_secs() >= 2)) {
                v.push_str(" · newer");
            }
            if !e.is_dir && e.size > o.size {
                v.push_str(" · bigger");
            }
        }
        v
    };
    let mut decision: Option<Conflict> = None;
    let mut cancel = false;
    // A free name beside it, offered to start from.
    let free = |dest: &Path| fsx::unique_dest(dest, &item.dst.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // Typing a new name: Enter takes it, Escape goes back to the choices.
    if st.rename.is_some() {
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::Escape) {
                st.rename = None;
            } else if i.consume_key(Modifiers::NONE, Key::Enter) {
                decision = st.rename.clone().filter(|n| !n.trim().is_empty() && !n.contains('/')).map(|n| Conflict::Rename(n.trim().to_string()));
            }
        });
    } else {
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            return false;
        }
        // Keys for each choice: a keyboard-first dialog.
        decision = ctx.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::Enter) {
                Some(Conflict::KeepBoth)
            } else if i.consume_key(Modifiers::NONE, Key::R) {
                Some(Conflict::Overwrite)
            } else if !both_dirs && i.consume_key(Modifiers::NONE, Key::N) {
                Some(Conflict::Newer)
            } else if i.consume_key(Modifiers::NONE, Key::S) {
                Some(Conflict::Skip)
            } else if both_dirs && i.consume_key(Modifiers::NONE, Key::M) {
                Some(Conflict::Merge)
            } else {
                None
            }
        });
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::A)) {
            st.apply_all = !st.apply_all;
        }
        if !st.apply_all && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F2)) {
            st.rename = Some(free(&st.dest));
            st.focus_name = true;
        }
    }
    let remaining = st.items.len() - st.idx;
    let thumbs = &mut app.thumbs;
    centered(ctx, "conflict", 560.0, &pal, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        let name = item.dst.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        dialog_title(ui, &format!("“{name}” already exists"), Some(&format!("in {}", home_short(&st.dest))), &pal);
        ui.add_space(6.0);
        let verb = if st.mv { "Moving" } else { "Copying" };
        card(ui, &pal, |ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            // Each side with its picture (or icon), so two photos can be told apart.
            for (label, e, o) in [(verb, &a, &b), ("Existing", &b, &a)] {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::hover());
                    if let Some(e) = e {
                        let pic = (!e.is_dir && crate::thumbs::can_thumb(e)).then(|| thumbs.get(&e.path, e.modified, 40.0)).flatten();
                        match pic {
                            Some(t) => {
                                let s = (40.0 / t.dims[0].max(t.dims[1]).max(1) as f32).min(1.0);
                                let sz = vec2(t.dims[0] as f32 * s, t.dims[1] as f32 * s).max(Vec2::splat(4.0));
                                let ir = Rect::from_center_size(r.center(), sz);
                                ui.painter().image(t.tex.id(), ir, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                                ui.painter().rect_stroke(ir, CornerRadius::same(3), Stroke::new(1.0, pal.card_line()), StrokeKind::Outside);
                            }
                            None => {
                                if !crate::icons::entry(ui.painter(), r, e) {
                                    theme::paint_icon(ui.painter(), r, theme::categorize(&e.ext(), e.is_dir, e.is_executable()), &e.ext(), e.is_link);
                                }
                            }
                        }
                    }
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(egui::RichText::new(label).color(pal.text_dim));
                        ui.label(info(e, o));
                    });
                });
            }
        });
        if let Some(n) = st.rename.as_mut() {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("New name").color(pal.text_dim));
                let id = Id::new("conflict-rename");
                let r = ui.add(egui::TextEdit::singleline(n).id(id).desired_width(ui.available_width()));
                crate::ui::note_control(ui, r.rect, "new name");
                if std::mem::take(&mut st.focus_name) {
                    r.request_focus();
                    // The name without its extension selected, as renaming does.
                    let stem = n.rfind('.').filter(|&i| i > 0).unwrap_or(n.len());
                    if let Some(mut s) = egui::TextEdit::load_state(ui.ctx(), id) {
                        s.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(n[..stem].chars().count()))));
                        s.store(ui.ctx(), id);
                    }
                }
            });
        } else if remaining > 1 {
            ui.add_space(4.0);
            ui.checkbox(&mut st.apply_all, format!("Apply to all {remaining} conflicts"));
        }
        let mut keys: Vec<(&str, &str)> = if st.rename.is_some() {
            vec![("Enter", "use this name"), ("Esc", "back")]
        } else {
            let mut k = vec![("Enter", "keep both"), ("R", "replace")];
            if !both_dirs {
                k.push(("N", "if newer"));
            }
            k.push(("S", "skip"));
            if both_dirs {
                k.push(("M", "merge"));
            }
            if !st.apply_all {
                k.push(("F2", "rename"));
            }
            if remaining > 1 {
                k.push(("A", "all"));
            }
            k
        };
        if st.rename.is_none() {
            keys.push(("Esc", "cancel"));
        }
        ui.add_space(4.0);
        hints(ui, &keys, &pal);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if st.rename.is_none() {
                if plain_button(ui, "Replace", &pal).on_hover_text("The one there goes to the trash; Undo brings it back").clicked() {
                    decision = Some(Conflict::Overwrite);
                }
                if !both_dirs && plain_button(ui, "If newer", &pal).on_hover_text("Replace it only when the one coming is newer; else skip").clicked() {
                    decision = Some(Conflict::Newer);
                }
                if both_dirs && plain_button(ui, "Merge", &pal).on_hover_text("Copy into the existing folder, replacing files").clicked() {
                    decision = Some(Conflict::Merge);
                }
                if plain_button(ui, "Skip", &pal).clicked() {
                    decision = Some(Conflict::Skip);
                }
                if !st.apply_all && plain_button(ui, "Rename…", &pal).clicked() {
                    st.rename = Some(free(&st.dest));
                    st.focus_name = true;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                match st.rename.as_ref() {
                    Some(n) => {
                        let ok = !n.trim().is_empty() && !n.contains('/');
                        if primary_button(ui, "Use this name", ok, &pal).clicked() {
                            decision = Some(Conflict::Rename(n.trim().to_string()));
                        }
                        if plain_button(ui, "Back", &pal).clicked() {
                            st.rename = None;
                        }
                    }
                    None => {
                        // Keeping both loses nothing: the main button.
                        if primary_button(ui, "Keep both", true, &pal).clicked() {
                            decision = Some(Conflict::KeepBoth);
                        }
                        if plain_button(ui, "Cancel", &pal).clicked() {
                            cancel = true;
                        }
                    }
                }
            });
        });
    });
    if cancel {
        return false;
    }
    if let Some(d) = decision {
        st.rename = None;
        let range = if st.apply_all { st.idx..st.items.len() } else { st.idx..st.idx + 1 };
        for i in range {
            let it = &st.items[i];
            let d = match &d {
                Conflict::Merge if !(it.src.is_dir() && it.dst.is_dir()) => Conflict::Overwrite,
                // A folder is never older or newer as a whole: replaced, as asked for all.
                Conflict::Newer if it.src.is_dir() => Conflict::Overwrite,
                d => d.clone(),
            };
            st.decide.insert(it.src.clone(), d);
        }
        st.idx = if st.apply_all { st.items.len() } else { st.idx + 1 };
        if st.idx >= st.items.len() {
            let decide = std::mem::take(&mut st.decide);
            let kind = if st.mv { OpKind::Move { srcs: st.srcs.clone(), dest: st.dest.clone(), decide } } else { OpKind::Copy { srcs: st.srcs.clone(), dest: st.dest.clone(), decide } };
            app.actions.push(Action::StartJob(kind));
            return false;
        }
    }
    true
}

/// Sync To Other Panel: what comes over, what is replaced and, mirroring, what goes, all
/// the way down, before anything is done.
fn sync(app: &mut App, ctx: &egui::Context, st: &mut crate::compare::SyncState) -> bool {
    let pal = app.pal;
    st.poll();
    let (go, cancel, flip) = ctx.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::Enter), i.consume_key(Modifiers::NONE, Key::Escape), i.consume_key(Modifiers::NONE, Key::M)));
    if cancel {
        return false;
    }
    let extra = st.plan.as_ref().map_or(0, |p| p.extra.len());
    if flip && extra > 0 {
        st.mirror = !st.mirror;
    }
    let job = st.job();
    let mut start = go && job.is_some();
    let mut close = false;
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (_, rect) = centered(ctx, "sync", 540.0, &pal, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        dialog_title(ui, &format!("Sync into “{}”", name(&st.to)), Some(&format!("From {}: what is missing or older there is brought up to date, in every folder", home_short(&st.from))), &pal);
        ui.add_space(6.0);
        let Some(plan) = &st.plan else {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                crate::ui::spinner(ui.painter(), r.center(), 5.0, pal.accent, ui.input(|i| i.time));
                ui.label(egui::RichText::new("Comparing both folders…").color(pal.text_dim));
            });
            ctx.request_repaint();
            dialog_buttons(ui, |ui| close = plain_button(ui, "Cancel", &pal).clicked());
            return;
        };
        let nothing = [plan.copy.is_empty(), plan.update.is_empty(), plan.extra.is_empty(), plan.unlisted.is_empty()];
        if nothing.iter().all(|&n| n) {
            ui.label(egui::RichText::new("Both sides are the same already").color(pal.text_dim));
            dialog_buttons(ui, |ui| close = primary_button(ui, "Close", true, &pal).clicked());
            return;
        }
        let count = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        card(ui, &pal, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            let dot = |ui: &mut Ui, col: Color32, text: String| {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(10.0, 14.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 3.5, col);
                    ui.label(text);
                });
            };
            if !plan.copy.is_empty() {
                dot(ui, pal.accent, format!("{} to copy over ({})", count(plan.copy.len(), "new item", "new items"), fsx::fmt_size(plan.bytes)));
            }
            if !plan.update.is_empty() {
                dot(ui, crate::gitstatus::GREEN, format!("{} here, replacing the older ones there (to the trash)", count(plan.update.len(), "newer file", "newer files")));
            }
            match plan.unlisted.len() {
                0 => {}
                1 => dot(ui, pal.danger, "1 folder couldn't be read and is left as it is".to_string()),
                n => dot(ui, pal.danger, format!("{n} folders couldn't be read and are left as they are")),
            }
            if extra > 0 {
                let text = egui::RichText::new(format!("Mirror: also move {} only there to the trash", count(extra, "item", "items"))).color(if st.mirror { pal.danger } else { pal.text });
                if boxed_check(ui, st.mirror, text, "mirror", &pal) {
                    st.mirror = !st.mirror;
                }
            }
        });
        // Every change, as it will be made, the folder there each lands in.
        let rel = |p: &Path| p.strip_prefix(&st.to).map_or_else(|_| p.display().to_string(), |r| r.display().to_string());
        let rows: Vec<(&str, Color32, String)> = plan
            .copy
            .iter()
            .map(|(_, d)| ("New", pal.accent, rel(d)))
            .chain(plan.update.iter().map(|(_, d)| ("Newer", crate::gitstatus::GREEN, rel(d))))
            .chain(plan.unlisted.iter().map(|d| ("Unread", pal.danger, rel(d))))
            .chain(plan.extra.iter().filter(|_| st.mirror).map(|d| ("Trash", pal.danger, rel(d))))
            .collect();
        group_label(ui, &format!("In “{}”", name(&st.to)), &pal);
        let h = (rows.len() as f32 * 24.0).min(fit_list(ctx, 240.0, 260.0));
        egui::ScrollArea::vertical().id_salt("sync-list").max_height(h).min_scrolled_height(h).auto_shrink([false, true]).show_rows(ui, 24.0, rows.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (tag, col, path) in &rows[range] {
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
                let chip = Rect::from_min_size(pos2(r.left() + 4.0, r.center().y - 8.0), vec2(48.0, 16.0));
                ui.painter().rect_filled(chip, CornerRadius::same(8), col.gamma_multiply(0.18));
                ui.painter().text(chip.center(), Align2::CENTER_CENTER, *tag, FontId::proportional(10.5), *col);
                let g = crate::listing::elided_tail(ui, path, FontId::proportional(12.5), r.width() - 64.0, pal.text);
                ui.painter().galley(pos2(chip.right() + 8.0, r.center().y - g.size().y / 2.0), g, pal.text);
            }
        });
        let mut keys = vec![("Enter", "sync")];
        if extra > 0 {
            keys.push(("M", "mirror"));
        }
        keys.push(("Esc", "cancel"));
        ui.add_space(4.0);
        footer(ui, &keys, &pal, |ui| {
            if primary_button(ui, "Sync", job.is_some(), &pal).clicked() {
                start = true;
            }
            if plain_button(ui, "Cancel", &pal).clicked() {
                close = true;
            }
        });
    });
    if start {
        if let Some(kind) = st.job() {
            app.actions.push(Action::StartJob(kind));
        }
        return false;
    }
    !(close || clicked_outside(ctx, rect))
}

/// Custom actions: the list on the left, the one selected on the right, saved as typed.
fn actions(app: &mut App, ctx: &egui::Context, sel: &mut usize) -> bool {
    let pal = app.pal;
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    let mut close = false;
    let mut changed = false;
    let (_, rect) = centered(ctx, "actions", 660.0, &pal, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        dialog_title(ui, "Custom actions", Some("Your own commands, in the right-click menu under Custom actions"), &pal);
        ui.add_space(6.0);
        let list = &mut app.cfg.custom_actions;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            ui.vertical(|ui| {
                ui.set_width(180.0);
                for (i, a) in list.iter().enumerate() {
                    let name = if a.name.trim().is_empty() { "Untitled".to_string() } else { a.name.clone() };
                    let r = row(ui, 28.0, i == *sel, &pal, |ui, r| {
                        let col = if i == *sel { pal.on_sel() } else { pal.text };
                        let g = crate::listing::elided(ui, &name, FontId::proportional(13.0), r.width() - 20.0, col);
                        ui.painter().galley(pos2(r.left() + 10.0, r.center().y - g.size().y / 2.0), g, col);
                    });
                    crate::ui::note_control(ui, r.rect, "action row");
                    if r.clicked() {
                        *sel = i;
                    }
                }
                ui.add_space(4.0);
                if plain_button(ui, "+ Add action", &pal).clicked() {
                    list.push(crate::config::CustomAction { name: "New action".into(), files: true, ..Default::default() });
                    *sel = list.len() - 1;
                    changed = true;
                }
            });
            ui.vertical(|ui| {
                let Some(a) = list.get_mut(*sel) else {
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("No actions yet: make one, then right-click files to run it.").color(pal.text_dim));
                    return;
                };
                let body = egui::TextStyle::Body.resolve(ui.style());
                let key_w = ["Name", "Command", "Only for"].iter().map(|k| ui.painter().layout_no_wrap(k.to_string(), body.clone(), pal.text_dim).size().x).fold(0.0, f32::max).ceil();
                let mut delete = false;
                card(ui, &pal, |ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    let w = ui.available_width() - key_w - 12.0;
                    field_row(ui, key_w, "Name", &pal, |ui| {
                        changed |= ui.add(egui::TextEdit::singleline(&mut a.name).desired_width(w)).changed();
                    });
                    field_row(ui, key_w, "Command", &pal, |ui| {
                        let r = ui.add(egui::TextEdit::singleline(&mut a.command).hint_text(egui::RichText::new("gimp %F").color(pal.text_dim)).font(egui::TextStyle::Monospace).desired_width(w));
                        crate::ui::note_control(ui, r.rect, "action command");
                        changed |= r.changed();
                        ui.label(egui::RichText::new("%f the first item · %F all of them · %d the folder · %n their names").small().color(pal.text_dim));
                    });
                    field_row(ui, key_w, "Only for", &pal, |ui| {
                        changed |= ui.add(egui::TextEdit::singleline(&mut a.extensions).hint_text(egui::RichText::new("jpg, png (empty: any file)").color(pal.text_dim)).desired_width(w)).changed();
                    });
                });
                card(ui, &pal, |ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    for (label, on) in [("Offer it for files", &mut a.files), ("Offer it for folders", &mut a.folders), ("Run it in a terminal", &mut a.terminal), ("Ask before running it", &mut a.confirm)] {
                        ui.horizontal(|ui| {
                            ui.label(label);
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| changed |= toggle(ui, on, &pal));
                        });
                    }
                });
                ui.add_space(2.0);
                let b = ui.add(egui::Button::new(egui::RichText::new("Delete this action").color(pal.danger)).fill(pal.card()).stroke(Stroke::new(1.0, pal.card_line())));
                if b.clicked() {
                    delete = true;
                }
                if delete {
                    list.remove(*sel);
                    *sel = sel.saturating_sub(1);
                    changed = true;
                }
            });
        });
        dialog_buttons(ui, |ui| close = primary_button(ui, "Done", true, &pal).clicked());
    });
    if changed {
        app.cfg_dirty = true;
    }
    !(close || clicked_outside(ctx, rect))
}

fn properties(app: &mut App, ctx: &egui::Context, st: &mut PropState) -> bool {
    let pal = app.pal;
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    let mut close = false;
    st.refresh();
    let snap = st.snap.take();
    let (_, rect) = centered(ctx, "props", 500.0, &pal, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        let Some(snap) = &snap else { return };
        if st.paths.len() == 1 {
            let p = st.paths[0].clone();
            let Some((m, e)) = snap.single.clone() else {
                dialog_title(ui, "Item no longer exists", Some(&p.display().to_string()), &pal);
                dialog_buttons(ui, |ui| close = plain_button(ui, "Close", &pal).clicked());
                return;
            };
            // The item: its icon, name and kind.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                let (r, _) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::hover());
                if !crate::icons::entry(ui.painter(), r, &e) {
                    theme::paint_icon(ui.painter(), r, theme::categorize(&e.ext(), e.is_dir, e.is_executable()), &e.ext(), e.is_link);
                }
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    let db = crate::mime::mime_db();
                    let mime = db.for_name(&e.name, e.is_dir, e.is_executable());
                    dialog_title(ui, &e.name, Some(&format!("{} · {mime}", db.comment(&mime))), &pal);
                });
            });
            ui.add_space(6.0);
            // Every label's column as wide as the widest one shown.
            let body = egui::TextStyle::Body.resolve(ui.style());
            let key_w = ["Location", "Size", "Accessed", "Owner", "Link target", "Checksum"].iter().map(|k| ui.painter().layout_no_wrap(k.to_string(), body.clone(), pal.text_dim).size().x).fold(0.0, f32::max).ceil();
            card(ui, &pal, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                let kv = |ui: &mut Ui, k: &str, v: String| field_row(ui, key_w, k, &pal, |ui| {
                    ui.add(egui::Label::new(v).wrap());
                });
                kv(ui, "Location", p.parent().map(|x| x.display().to_string()).unwrap_or_default());
                if e.is_dir {
                    match app.stats.map.get(&p) {
                        Some(s) => kv(ui, "Size", format!("{} ({} bytes)\n{} file{}, {} folder{}", fsx::fmt_size(s.size), s.size, s.files, if s.files == 1 { "" } else { "s" }, s.folders, if s.folders == 1 { "" } else { "s" })),
                        None => {
                            app.stats.request(&p);
                            kv(ui, "Size", "Calculating…".into())
                        }
                    }
                } else {
                    kv(ui, "Size", format!("{} ({} bytes)", fsx::fmt_size(e.size), e.size));
                }
                if let Some((total, free)) = snap.vol {
                    kv(ui, "Volume", format!("{} free of {}", fsx::fmt_size(free), fsx::fmt_size(total)));
                }
                if let Some(t) = &snap.link {
                    kv(ui, "Link target", t.clone());
                }
                if let Some(t) = crate::tags::get(&p, |t| t.join(", ")).filter(|t| !t.is_empty()) {
                    kv(ui, "Tags", t);
                }
            });
            card(ui, &pal, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                let kv = |ui: &mut Ui, k: &str, v: String| field_row(ui, key_w, k, &pal, |ui| {
                    ui.add(egui::Label::new(v).wrap());
                });
                kv(ui, "Created", fsx::fmt_time(e.created));
                kv(ui, "Modified", fsx::fmt_time(e.modified));
                kv(ui, "Accessed", fsx::fmt_time(m.accessed().ok()));
            });
            if m.is_file() {
                group_label(ui, "Checksum", &pal);
                card(ui, &pal, |ui| checksum_row(ui, ctx, &p, st, &pal));
            }
            group_label(ui, "Permissions", &pal);
            if perms_card(ui, st, &snap.perms, key_w, &pal) {
                close = true;
            }
        } else {
            let mut total = 0u64;
            let mut pending = false;
            let (mut files, mut dirs) = (0, 0);
            for (p, &(is_dir, len)) in st.paths.iter().zip(snap.multi.iter()) {
                if is_dir {
                    dirs += 1;
                    match app.stats.map.get(p) {
                        Some(s) => total += s.size,
                        None => {
                            pending = true;
                            app.stats.request(p);
                        }
                    }
                } else {
                    files += 1;
                    total += len;
                }
            }
            let count = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
            dialog_title(ui, &format!("{} items", st.paths.len()), Some(&format!("{}, {}", count(files, "file", "files"), count(dirs, "folder", "folders"))), &pal);
            ui.add_space(6.0);
            let body = egui::TextStyle::Body.resolve(ui.style());
            let key_w = ["Location", "Size"].iter().map(|k| ui.painter().layout_no_wrap(k.to_string(), body.clone(), pal.text_dim).size().x).fold(0.0, f32::max).ceil();
            card(ui, &pal, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                field_row(ui, key_w, "Size", &pal, |ui| {
                    ui.label(format!("{}{}", fsx::fmt_size(total), if pending { " (calculating…)" } else { "" }));
                });
                if let Some(parent) = st.paths[0].parent() {
                    field_row(ui, key_w, "Location", &pal, |ui| {
                        ui.add(egui::Label::new(parent.display().to_string()).wrap());
                    });
                }
            });
            group_label(ui, "Permissions", &pal);
            if perms_card(ui, st, &snap.perms, key_w, &pal) {
                close = true;
            }
        }
    });
    ctx.request_repaint_after(std::time::Duration::from_millis(300));
    if let Some(kind) = st.apply.take() {
        app.actions.push(Action::StartJob(kind));
        // Read again at once, to show what the job did.
        st.snap = None;
    } else {
        st.snap = snap;
    }
    !(close || clicked_outside(ctx, rect))
}

/// Who may read, write and run the items, in columns under their names, with the special
/// bits and the owner and group below; Apply starts the change. A box where the items
/// differ shows a dash until clicked. True when Close was clicked.
fn perms_card(ui: &mut Ui, st: &mut PropState, items: &[(u32, u32, u32, bool)], key_w: f32, pal: &Palette) -> bool {
    let body = egui::TextStyle::Body.resolve(ui.style());
    // A bit as the items have it: all on, all off, or some of each (None).
    let base = |mask: u32| -> Option<bool> {
        let on = items.iter().filter(|i| i.0 & mask != 0).count();
        (on == 0 || on == items.len()).then_some(on > 0)
    };
    let shown = |c: &crate::perms::Change, mask: u32| if c.set & mask != 0 { Some(true) } else if c.clear & mask != 0 { Some(false) } else { base(mask) };
    let toggle = |c: &mut crate::perms::Change, mask: u32| {
        let on = !shown(c, mask).unwrap_or(false);
        (c.set, c.clear) = (c.set & !mask, c.clear & !mask);
        // Only what differs from how they all are already is a change.
        if base(mask) != Some(on) {
            if on { c.set |= mask } else { c.clear |= mask }
        }
    };
    let boxed = |ui: &mut Ui, r: Rect, id: Id, state: Option<bool>| -> bool {
        let resp = ui.interact(r.expand(3.0), id, Sense::click());
        match state {
            Some(on) => checkbox_glyph(ui.painter(), r, on, pal),
            None => {
                ui.painter().rect_stroke(r, CornerRadius::same(3), Stroke::new(1.2, pal.accent), StrokeKind::Inside);
                ui.painter().line_segment([r.center() - vec2(4.0, 0.0), r.center() + vec2(4.0, 0.0)], Stroke::new(1.8, pal.accent));
            }
        }
        if resp.hovered() && state != Some(true) {
            ui.painter().rect_stroke(r, CornerRadius::same(3), Stroke::new(1.2, pal.text_dim), StrokeKind::Inside);
        }
        if resp.has_focus() {
            ui.painter().rect_stroke(r.expand(2.5), CornerRadius::same(5), Stroke::new(1.5, pal.accent), StrokeKind::Inside);
        }
        resp.clicked()
    };
    let col_w = 72.0;
    let row_h = 22.0;
    let dirs = items.iter().any(|i| i.3);
    card(ui, pal, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        let (head, _) = ui.allocate_exact_size(vec2(key_w + 12.0 + 3.0 * col_w, 18.0), Sense::hover());
        for (k, h) in ["Read", "Write", "Execute"].iter().enumerate() {
            let cx = head.left() + key_w + 12.0 + col_w * (k as f32 + 0.5);
            ui.painter().text(pos2(cx, head.center().y), Align2::CENTER_CENTER, *h, FontId::proportional(11.5), pal.text_dim);
        }
        for (who, shift, names) in [("Owner", 6, ["owner read", "owner write", "owner execute"]), ("Group", 3, ["group read", "group write", "group execute"]), ("Others", 0, ["others read", "others write", "others execute"])] {
            let (r, _) = ui.allocate_exact_size(vec2(key_w + 12.0 + 3.0 * col_w, row_h), Sense::hover());
            ui.painter().text(pos2(r.left(), r.center().y), Align2::LEFT_CENTER, who, body.clone(), pal.text);
            for (k, bit) in [4u32, 2, 1].into_iter().enumerate() {
                let mask = bit << shift;
                let cx = r.left() + key_w + 12.0 + col_w * (k as f32 + 0.5);
                let br = Rect::from_center_size(pos2(cx, r.center().y), Vec2::splat(16.0));
                crate::ui::note_control(ui, br, names[k]);
                if boxed(ui, br, Id::new(("perm", shift, bit)), shown(&st.change, mask)) {
                    toggle(&mut st.change, mask);
                }
            }
        }
        // The special bits, by what they do.
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for (label, tip, mask) in [("Run as owner", "Set user ID: it runs with its owner's rights", 0o4000), ("Run as group", "Set group ID: it runs with its group's rights; inside a folder, new items take its group", 0o2000), ("Sticky", "In a folder, only an item's owner may delete or rename it", 0o1000)] {
                let (r, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                if boxed(ui, r, Id::new(("perm-special", mask)), shown(&st.change, mask)) {
                    toggle(&mut st.change, mask);
                }
                ui.add(egui::Label::new(egui::RichText::new(label).size(12.0).color(pal.text_dim)).selectable(false)).on_hover_text(tip);
                ui.add_space(8.0);
            }
        });
        // One item: its mode as numbers and letters, as it will be.
        if let [(mode, _, _, is_dir)] = items {
            let m = st.change.mode(*mode, false);
            ui.label(egui::RichText::new(format!("{m:04o}  {}", fsx::fmt_mode(m, *is_dir))).monospace().color(pal.text_dim));
        }
        // Owner and group: who they are now (or "Several"), picked from those on this computer.
        let (users, groups) = st.ids.get_or_insert_with(|| (crate::perms::users(), crate::perms::groups()));
        let same = |f: fn(&(u32, u32, u32, bool)) -> u32| items.first().map(f).filter(|v| items.iter().all(|i| f(i) == *v));
        let (now_u, now_g) = (st.change.owner.or(same(|i| i.1)), st.change.group.or(same(|i| i.2)));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(key_w + 6.0, row_h), Sense::hover());
            ui.painter().text(pos2(r.left(), r.center().y), Align2::LEFT_CENTER, "Owner", body.clone(), pal.text);
            for (id, now, list, field, current) in [("owner", now_u, &*users, 0, same(|i| i.1)), ("group", now_g, &*groups, 1, same(|i| i.2))] {
                let text = now.map_or("Several".to_string(), |n| list.iter().find(|x| x.0 == n).map_or_else(|| n.to_string(), |x| x.1.clone()));
                let r = egui::ComboBox::from_id_salt(("prop", id)).selected_text(text).width(132.0).height(260.0).show_ui(ui, |ui| {
                    for (uid, name) in list {
                        if ui.selectable_label(now == Some(*uid), name).clicked() {
                            let v = (current != Some(*uid)).then_some(*uid);
                            if field == 0 { st.change.owner = v } else { st.change.group = v }
                        }
                    }
                });
                crate::ui::note_control(ui, r.response.rect, if field == 0 { "owner" } else { "group" });
                if field == 0 {
                    ui.label(egui::RichText::new(":").color(pal.text_dim));
                }
            }
        });
        if dirs {
            ui.add_space(2.0);
            if boxed_check(ui, st.deep, egui::RichText::new("Change everything inside too"), "deep", pal) {
                st.deep = !st.deep;
            }
            if st.deep && st.change.set & 0o111 != 0 {
                ui.label(egui::RichText::new("Files inside get Execute only where they had it").small().color(pal.text_dim));
            }
        }
    });
    let mut close = false;
    let changed = !st.change.is_empty();
    dialog_buttons(ui, |ui| {
        if changed {
            if primary_button(ui, "Apply", true, pal).clicked() {
                st.apply = Some(OpKind::Perms { paths: st.paths.clone(), change: std::mem::take(&mut st.change), deep: st.deep });
            }
            if plain_button(ui, "Close", pal).clicked() {
                close = true;
            }
        } else if primary_button(ui, "Close", true, pal).clicked() {
            close = true;
        }
    });
    close
}

/// Paste a checksum to compare (its length picks the algorithm), or work out SHA-256.
fn checksum_row(ui: &mut Ui, ctx: &egui::Context, p: &Path, st: &mut PropState, pal: &Palette) {
    use crate::checksum::{Algo, Job, clean};
    if let Some(r) = st.sum_job.as_ref().and_then(|j| j.poll().map(|r| (j.algo, r))) {
        st.sum = Some(r);
        st.sum_job = None;
    }
    let want = clean(&st.expected);
    let mut start = None;
    ui.horizontal(|ui| {
        let busy = st.sum_job.is_some();
        // Room for the SHA-256 button only while it is there.
        let bw = if !busy && st.sum.is_none() { 74.0 } else { 0.0 };
        let r = ui.add(egui::TextEdit::singleline(&mut st.expected).hint_text(egui::RichText::new("Paste one to compare").color(pal.text_dim)).desired_width(ui.available_width() - bw));
        crate::ui::note_control(ui, r.rect, "checksum field");
        // Pasting a checksum works out the same kind for this file.
        if let Some(a) = Algo::for_hex(&clean(&st.expected)).filter(|_| r.changed()) {
            if st.sum.as_ref().map(|s| s.0) != Some(a) && st.sum_job.as_ref().map(|j| j.algo) != Some(a) {
                start = Some(a);
            }
        }
        if !busy && st.sum.is_none() {
            let b = ui.button("SHA-256");
            crate::ui::note_control(ui, b.rect, "checksum button");
            if b.clicked() {
                start = Some(Algo::Sha256);
            }
        }
    });
    if let Some(a) = start {
        st.sum = None;
        st.sum_job = Some(Job::start(p, a, ctx));
    }
    if let Some(j) = &st.sum_job {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
            crate::ui::spinner(ui.painter(), r.center(), 5.0, pal.accent, ui.input(|i| i.time));
            ui.label(egui::RichText::new(format!("Calculating {}…", j.algo.label())).color(pal.text_dim));
        });
        ctx.request_repaint();
        return;
    }
    match &st.sum {
        Some((a, Ok(hex))) => {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(a.label()).small().color(pal.text_dim));
                let b = ui.small_button("Copy");
                crate::ui::note_control(ui, b.rect, "copy checksum");
                if b.clicked() {
                    ctx.copy_text(hex.clone());
                }
            });
            ui.add(egui::Label::new(egui::RichText::new(hex).monospace().size(11.5)).wrap());
            if Algo::for_hex(&want) == Some(*a) {
                let (ok, msg) = if want == *hex { (true, "Matches") } else { (false, "Doesn't match") };
                ui.label(egui::RichText::new(msg).strong().color(if ok { crate::gitstatus::GREEN } else { pal.danger }));
            }
        }
        Some((_, Err(e))) => {
            ui.label(egui::RichText::new(e).color(pal.danger));
        }
        None => {}
    }
    if !want.is_empty() && Algo::for_hex(&want).is_none() {
        ui.label(egui::RichText::new("Not an MD5, SHA-1, SHA-256 or SHA-512 checksum").small().color(pal.text_dim));
    }
}

fn open_with(app: &mut App, ctx: &egui::Context, paths: &mut Vec<PathBuf>, query: &mut String, all: &mut bool, command: &mut String) -> bool {
    let pal = app.pal;
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    let db = crate::mime::mime_db();
    let first = paths[0].clone();
    let mime = db.for_path(&first, false);
    let apps = crate::mime::apps();
    let list: Vec<&crate::mime::App> = if *all { apps.apps.iter().filter(|a| !a.hidden).collect() } else { apps.for_mime(&mime) };
    let q = fuzzy::Query::new(query);
    let mut launch: Option<crate::mime::App> = None;
    let mut set_def: Option<String> = None;
    let mut run: Option<String> = None;
    // Typing filters the apps, unless the command field has the keyboard.
    let cmd_id = Id::new("openwith-command");
    let mut focus = !ctx.memory(|m| m.has_focus(cmd_id));
    let (_, rect) = centered(ctx, "openwith", 480.0, &pal, |ui| {
        ui.label(egui::RichText::new(format!("Open {} with…", if paths.len() == 1 { first.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() } else { format!("{} files", paths.len()) })).strong());
        ui.label(egui::RichText::new(format!("{} ({mime})", db.comment(&mime))).small().color(pal.text_dim));
        search_field(ui, query, "Filter applications…", &mut focus, &pal, Glyph::Search);
        egui::ScrollArea::vertical().max_height(340.0).show(ui, |ui| {
            for (i, a) in list.iter().enumerate() {
                if !q.is_empty() && fuzzy::score(&q, &a.name.to_lowercase()).is_none() {
                    continue;
                }
                ui.horizontal(|ui| {
                    let label = if i == 0 && !*all { format!("{}  (default)", a.name) } else { a.name.clone() };
                    if ui.add(egui::Button::new(label).frame(false)).clicked() {
                        launch = Some((*a).clone());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Set default").clicked() {
                            set_def = Some(a.id.clone());
                        }
                    });
                });
            }
            if list.is_empty() {
                ui.label(egui::RichText::new("No registered applications for this type.").color(pal.text_dim));
            }
        });
        ui.checkbox(all, "Show all applications");
        // Or any command, the files after it (or where %f / %F put them).
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(command).id(cmd_id).hint_text(egui::RichText::new("Or a command, like gimp %F").color(pal.text_dim)).font(egui::TextStyle::Monospace).desired_width(ui.available_width() - 64.0));
            crate::ui::note_control(ui, r.rect, "open with command");
            let go = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if (ui.add_enabled(!command.trim().is_empty(), egui::Button::new("Run")).clicked() || go) && !command.trim().is_empty() {
                run = Some(command.trim().to_string());
            }
        });
    });
    if let Some(c) = run {
        let command = if c.contains('%') { c.clone() } else { format!("{c} %F") };
        crate::ctxmenu::run_command(app, crate::config::CustomAction { name: c, command, files: true, folders: true, ..Default::default() }, paths);
        return false;
    }
    if let Some(id) = set_def {
        match crate::mime::set_default(&id, &mime) {
            Ok(()) => app.toast(format!("Default for {mime} updated")),
            Err(e) => app.toast(format!("Couldn't set default: {e}")),
        }
    }
    if let Some(a) = launch {
        if let Err(e) = crate::mime::launch(&a, paths, first.parent()) {
            app.toast(format!("Couldn't launch {}: {e}", a.name));
        }
        return false;
    }
    !clicked_outside(ctx, rect)
}

fn run_exec(app: &mut App, ctx: &egui::Context, path: &mut PathBuf) -> bool {
    let pal = app.pal;
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    let mut done = false;
    centered(ctx, "runexec", 440.0, &pal, |ui| {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        ui.label(egui::RichText::new(format!("“{name}” is executable")).strong());
        ui.label(egui::RichText::new("Do you want to run it?").color(pal.text_dim));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Run").clicked() {
                if let Err(e) = crate::mime::run_with(path, &[]) {
                    app.toast(format!("Couldn't run: {e}"));
                }
                done = true;
            }
            if ui.button("Run in terminal").clicked() {
                let a = crate::mime::App { id: "run".into(), name: name.clone(), exec: format!("\"{}\"", path.display()), mimes: vec![], terminal: true, hidden: false, icon: String::new() };
                if let Err(e) = crate::mime::launch(&a, &[], path.parent()) {
                    app.toast(format!("Couldn't run: {e}"));
                }
                done = true;
            }
            if ui.button("Open").on_hover_text("Open with the default application").clicked() {
                app.actions.push(Action::OpenFile(path.clone()));
                done = true;
            }
            if ui.button("Open with…").clicked() {
                app.actions.push(Action::Open(Popup::open_with(vec![path.clone()])));
                done = true;
            }
            if ui.button("Cancel").clicked() {
                done = true;
            }
        });
    });
    !done
}

fn main_menu(app: &mut App, ctx: &egui::Context) -> bool {
    let pal = app.pal;
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    let mut run = None;
    let (_, rect) = anchored(ctx, "mainmenu", pos2(8.0, crate::ui::TITLE_H), 280.0, &pal, |ui| {
        for c in [Cmd::NewWindow, Cmd::NewTab] {
            if cmd_item(ui, app, c) {
                run = Some(c);
            }
        }
        menu_sep(ui, &pal);
        for c in [Cmd::SaveLayout, Cmd::LoadLayout, Cmd::DeleteLayout] {
            if cmd_item(ui, app, c) {
                run = Some(c);
            }
        }
        menu_sep(ui, &pal);
        for c in [Cmd::Options, Cmd::Palette, Cmd::GoTo, Cmd::OpenConfig, Cmd::RevealConfig] {
            if cmd_item(ui, app, c) {
                run = Some(c);
            }
        }
        menu_sep(ui, &pal);
        for c in [Cmd::ConnectServer, Cmd::Install, Cmd::SetDefault, Cmd::About, Cmd::Quit] {
            if cmd_item(ui, app, c) {
                run = Some(c);
            }
        }
    });
    if let Some(c) = run {
        app.actions.push(Action::Cmd(c));
        return false;
    }
    !clicked_outside(ctx, rect)
}

fn quick_menu(app: &mut App, ctx: &egui::Context, panel: Option<(u64, Pos2)>) -> bool {
    let pal = app.pal;
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    let anchor = match panel {
        Some((_, a)) => pos2(a.x - 250.0, a.y + 4.0),
        None => {
            // Opens at the mouse pointer.
            let r = app.panels.get(&app.active).map(|p| p.rect).unwrap_or(ctx.content_rect());
            *app.quick_anchor.get_or_insert_with(|| ctx.pointer_latest_pos().unwrap_or(pos2(r.right() - 290.0, r.top() + 70.0)))
        }
    };
    let mut run = None;
    let quick: Vec<Cmd> = app.ud.quick.iter().filter_map(|id| Cmd::from_id(id)).collect();
    let group = app.panels.get(&app.active).map(|p| p.tab().settings.group).unwrap_or_default();
    let (_, rect) = anchored(ctx, "quickmenu", anchor, 280.0, &pal, |ui| {
        menu_heading(ui, "Quick", &pal);
        for c in &quick {
            if cmd_item(ui, app, *c) {
                run = Some(*c);
            }
        }
        if panel.is_some() {
            menu_sep(ui, &pal);
            menu_heading(ui, "Sort", &pal);
            for c in [Cmd::SortName, Cmd::SortSize, Cmd::SortModified, Cmd::SortType, Cmd::ToggleFoldersFirst] {
                if cmd_item(ui, app, c) {
                    run = Some(c);
                }
            }
            menu_sep(ui, &pal);
            menu_heading(ui, "Group", &pal);
            for c in crate::commands::GROUP_CMDS {
                if !applicable(app, c) {
                    continue;
                }
                let g = c.group_key().unwrap_or_default();
                let label = format!("{}{}", if g == group { "● " } else { "" }, c.info().title.trim_start_matches("Group By "));
                if menu_item(ui, &label, None, &pal) {
                    run = Some(c);
                }
            }
            menu_sep(ui, &pal);
            for c in [Cmd::SavedView, Cmd::ToggleInspector, Cmd::ClosePanel] {
                if cmd_item(ui, app, c) {
                    run = Some(c);
                }
            }
        }
    });
    if let Some(c) = run {
        app.actions.push(Action::Cmd(c));
        return false;
    }
    !clicked_outside(ctx, rect)
}

fn path_menu(app: &mut App, ctx: &egui::Context, panel: u64, path: &Path, anchor: Pos2) -> bool {
    let pal = app.pal;
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    let mut done = false;
    let computer = tab::is_place(path);
    let (_, rect) = anchored(ctx, "pathmenu", anchor, 250.0, &pal, |ui| {
        if !computer {
            if menu_item(ui, "Copy path", None, &pal) {
                ctx.copy_text(path.display().to_string());
                done = true;
            }
            if menu_item(ui, "Copy path (quoted)", None, &pal) {
                ctx.copy_text(format!("\"{}\"", path.display()));
                done = true;
            }
            if menu_item(ui, "Open in new tab", None, &pal) {
                app.actions.push(Action::NewTab { panel, path: path.to_path_buf(), select: None, focus: true });
                done = true;
            }
            if menu_item(ui, "Open terminal here", None, &pal) {
                let _ = crate::mime::open_terminal(path);
                done = true;
            }
            menu_sep(ui, &pal);
        }
        let mut c = app.cfg.compact_paths;
        if ui.checkbox(&mut c, "Show compact paths").changed() {
            app.cfg.compact_paths = c;
            app.cfg_dirty = true;
        }
        let mut b = app.cfg.breadcrumbs;
        if ui.checkbox(&mut b, "Path breadcrumbs").changed() {
            app.cfg.breadcrumbs = b;
            app.cfg_dirty = true;
        }
    });
    !(done || clicked_outside(ctx, rect))
}

fn path_dropdown(app: &mut App, ctx: &egui::Context, st: &mut PathDropState) -> bool {
    let pal = app.pal;
    let k = take_keys(ctx);
    if k.esc {
        return false;
    }
    if k.alt_left {
        if let Some(parent) = st.dir.parent() {
            let child = st.dir.clone();
            *st = match Popup::path_dropdown(st.panel, parent.to_path_buf(), Some(child)) {
                Popup::PathDropdown(mut s) => {
                    s.anchor = st.anchor.map(|a| a - vec2(60.0, 0.0));
                    s
                }
                _ => unreachable!(),
            };
        }
    }
    if k.alt_right {
        if let Some(next) = st.next.clone().or_else(|| st.entries.get(st.sel).cloned()) {
            let anchor = st.anchor;
            if let Popup::PathDropdown(mut s) = Popup::path_dropdown(st.panel, next, None) {
                s.anchor = anchor.map(|a| a + vec2(60.0, 0.0));
                *st = s;
            }
        }
    }
    let q = fuzzy::Query::new(&st.query);
    let shown: Vec<PathBuf> = st.entries.iter().filter(|p| q.is_empty() || fuzzy::score(&q, &p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default()).is_some()).cloned().collect();
    step(&mut st.sel, shown.len(), &k, |_| false);
    let mut go: Option<(PathBuf, bool)> = None;
    if k.enter || k.ctrl_enter {
        if let Some(p) = shown.get(st.sel) {
            go = Some((p.clone(), k.ctrl_enter));
        }
    }
    let anchor = st.anchor.unwrap_or_else(|| app.panels.get(&st.panel).map(|p| p.rect.left_top() + vec2(120.0, 66.0)).unwrap_or(Pos2::ZERO));
    let mut close = false;
    let n_items = st.entries.len();
    let (_, rect) = anchored(ctx, "pathdrop", anchor, 250.0, &pal, |ui| {
        // Search row: magnifier, "Filter N items…", close.
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(20.0, 22.0), Sense::hover());
            paint_glyph(ui.painter(), Rect::from_center_size(r.center(), Vec2::splat(12.0)), Glyph::Search, pal.text_dim);
            let hint = format!("Filter {} {}…", n_items, if n_items == 1 { "item" } else { "items" });
            let r = ui.add(egui::TextEdit::singleline(&mut st.query).hint_text(egui::RichText::new(hint).color(pal.text_dim)).frame(crate::ui::bare_edit()).desired_width(ui.available_width() - 24.0));
            if st.focus {
                r.request_focus();
                st.focus = false;
            }
            if crate::ui::glyph_button(ui, Glyph::Close, "Close (Esc)", true, &pal).clicked() {
                close = true;
            }
        });
        let yy = ui.cursor().top() + 1.0;
        let xr = ui.min_rect().x_range();
        ui.painter().line_segment([pos2(xr.min - 5.0, yy), pos2(xr.max + 5.0, yy)], Stroke::new(1.0, pal.border));
        ui.add_space(4.0);
        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
            for (i, p) in shown.iter().enumerate() {
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let highlighted = Some(p) == st.next.as_ref();
                let sel = i == st.sel;
                let resp = row(ui, 24.0, sel, &pal, |ui, r| {
                    let ic = Rect::from_center_size(pos2(r.left() + 14.0, r.center().y), Vec2::splat(16.0));
                    if let Some(e) = st.info.get(p) {
                        if !crate::icons::entry(ui.painter(), ic, e) {
                            theme::paint_icon(ui.painter(), ic, theme::categorize(&e.ext(), e.is_dir, e.is_executable()), "", e.is_link);
                        }
                    }
                    // The folder the path continues into is marked like the current crumb.
                    let col = if sel { pal.on_sel() } else if highlighted { pal.accent } else { pal.text };
                    // Long names end in "…" inside the row instead of running past the popup.
                    let g = crate::listing::elided(ui, &name, FontId::proportional(12.5), (r.width() - 38.0).max(20.0), col);
                    ui.painter().galley(pos2(r.left() + 30.0, r.center().y - g.size().y / 2.0), g, col);
                });
                if i == st.sel && (k.up || k.down) {
                    resp.scroll_to_me(None);
                }
                if resp.clicked() {
                    go = Some((p.clone(), false));
                }
                if resp.middle_clicked() {
                    go = Some((p.clone(), true));
                }
            }
            if shown.is_empty() {
                ui.label(egui::RichText::new("Empty folder").color(pal.text_dim));
            }
        });
    });
    if close {
        return false;
    }
    if let Some((p, new_tab)) = go {
        if !p.is_dir() {
            app.actions.push(Action::OpenFile(p));
        } else if new_tab {
            app.actions.push(Action::NewTab { panel: st.panel, path: p, select: None, focus: false });
        } else {
            app.actions.push(Action::Navigate { panel: st.panel, path: p });
        }
        return false;
    }
    !clicked_outside(ctx, rect)
}

fn history(app: &mut App, ctx: &egui::Context, panel: u64, anchor: Pos2) -> bool {
    let pal = app.pal;
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    let Some(p) = app.panels.get(&panel) else { return false };
    let t = p.tab();
    let hist: Vec<(usize, PathBuf)> = t.history.iter().enumerate().map(|(i, h)| (i, h.path.clone())).rev().collect();
    let pos = t.hist_pos;
    let recents: Vec<PathBuf> = app.ud.recents.iter().take(10).cloned().collect();
    let mut go = None;
    let (_, rect) = anchored(ctx, "history", anchor, 320.0, &pal, |ui| {
        menu_heading(ui, "History", &pal);
        for (i, h) in hist.iter().take(15) {
            let name = if tab::is_computer(h) { "Computer".into() } else if tab::is_github(h) { "GitHub".into() } else { h.display().to_string() };
            let label = if *i == pos { format!("● {name}") } else { name };
            if menu_item(ui, &label, None, &pal) {
                go = Some(h.clone());
            }
        }
        if !recents.is_empty() {
            menu_sep(ui, &pal);
            menu_heading(ui, "Recent", &pal);
            for r in &recents {
                if menu_item(ui, &r.display().to_string(), None, &pal) {
                    go = Some(r.clone());
                }
            }
        }
    });
    if let Some(p) = go {
        app.actions.push(Action::Navigate { panel, path: p });
        return false;
    }
    !clicked_outside(ctx, rect)
}

fn rename_menu(app: &mut App, ctx: &egui::Context, panel: u64, pos: Pos2) -> bool {
    let pal = app.pal;
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    let Some(p) = app.panels.get_mut(&panel) else { return false };
    let Some(r) = p.tab_mut().rename.as_mut() else { return false };
    let mut done = false;
    let mut find = false;
    let mut clip: Option<String> = None;
    let (_, rect) = anchored(ctx, "renamemenu", pos, 270.0, &pal, |ui| {
        if menu_item(ui, "Cut", Some("Ctrl+X".into()), &pal) {
            clip = Some(r.copy_text());
            r.insert_all("");
            done = true;
        }
        if menu_item(ui, "Copy", Some("Ctrl+C".into()), &pal) {
            clip = Some(r.copy_text());
            done = true;
        }
        if menu_item(ui, "Paste", Some("Ctrl+V".into()), &pal) {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            done = true;
        }
        if menu_item(ui, "Delete", Some("Backspace".into()), &pal) {
            r.insert_all("");
            done = true;
        }
        if menu_item(ui, "Select all", Some("Ctrl+A".into()), &pal) {
            r.select_all();
            done = true;
        }
        menu_sep(ui, &pal);
        if menu_item(ui, "Insert incremental ID", None, &pal) {
            r.insert_incremental_id();
            done = true;
        }
        if menu_item(ui, "Insert current date", None, &pal) {
            r.insert_date(DateKind::Today);
            done = true;
        }
        if menu_item(ui, "Insert file creation date", None, &pal) {
            r.insert_date(DateKind::Created);
            done = true;
        }
        if menu_item(ui, "Insert file last-modified date", None, &pal) {
            r.insert_date(DateKind::Modified);
            done = true;
        }
        if menu_item(ui, "Insert parent folder name", None, &pal) {
            r.insert_parent_name();
            done = true;
        }
        if menu_item(ui, "Insert unique ID", None, &pal) {
            r.insert_unique_id();
            done = true;
        }
        if menu_item(ui, "Insert original name", None, &pal) {
            r.insert_original();
            done = true;
        }
        menu_sep(ui, &pal);
        if menu_item(ui, "Find & replace…", None, &pal) {
            find = true;
        }
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Case:").small().color(pal.text_dim));
            for (l, c) in [("lower", Case::Lower), ("UPPER", Case::Upper), ("Title", Case::Title), ("Sentence", Case::Sentence)] {
                if ui.small_button(l).clicked() {
                    r.change_case(c);
                    done = true;
                }
            }
        });
    });
    if let Some(c) = clip {
        ctx.copy_text(c);
    }
    if find {
        app.actions.push(Action::Open(Popup::FindReplace { panel }));
        return false;
    }
    !(done || clicked_outside(ctx, rect))
}

fn find_replace(app: &mut App, ctx: &egui::Context, panel: u64) -> bool {
    let pal = app.pal;
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    let Some(p) = app.panels.get_mut(&panel) else { return false };
    let Some(r) = p.tab_mut().rename.as_mut() else { return false };
    let mut done = false;
    centered(ctx, "findreplace", 400.0, &pal, |ui| {
        ui.label(egui::RichText::new(format!("Find & replace in {} name{}", r.fields.len(), if r.fields.len() == 1 { "" } else { "s" })).strong());
        egui::Grid::new("fr").num_columns(2).show(ui, |ui| {
            ui.label("Find");
            let f = ui.add(egui::TextEdit::singleline(&mut r.find).desired_width(260.0));
            if r.find.is_empty() && !f.has_focus() {
                f.request_focus();
            }
            ui.end_row();
            ui.label("Replace");
            ui.add(egui::TextEdit::singleline(&mut r.replace).desired_width(260.0));
            ui.end_row();
        });
        ui.horizontal(|ui| {
            if ui.button("Replace all").clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                r.find_replace();
                done = true;
            }
            if ui.button("Cancel").clicked() {
                done = true;
            }
        });
    });
    !done
}

fn about(app: &mut App, ctx: &egui::Context) -> bool {
    let pal = app.pal;
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    let (_, rect) = centered(ctx, "about", 380.0, &pal, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(6.0);
            ui.label(egui::RichText::new("Maia").size(24.0).strong());
            ui.label(egui::RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).color(pal.text_dim));
            ui.add_space(6.0);
            ui.label("A fast, keyboard-first file manager for Linux.");
            ui.label(egui::RichText::new("Single binary · Rust + egui").small().color(pal.text_dim));
            ui.add_space(6.0);
        });
    });
    !clicked_outside(ctx, rect)
}

fn tab_menu(app: &mut App, ctx: &egui::Context, panel: u64, idx: usize, pos: Pos2) -> bool {
    let pal = app.pal;
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    // Commands act on the right-clicked tab.
    if let Some(p) = app.panels.get_mut(&panel) {
        if idx < p.tabs.len() {
            p.active = idx;
        }
    }
    app.focus_panel(panel);
    let mut run = None;
    let (_, rect) = anchored(ctx, "tabmenu", pos, 300.0, &pal, |ui| {
        for c in [Cmd::MoveTabLeft, Cmd::MoveTabRight] {
            if cmd_row(ui, app, c) {
                run = Some(c);
            }
        }
        let pinned = app.panels.get(&panel).and_then(|p| p.tabs.get(idx)).is_some_and(|t| t.pinned);
        if cmd_row_as(ui, app, Cmd::PinTab, if pinned { "Unpin Tab" } else { "Pin Tab" }) {
            run = Some(Cmd::PinTab);
        }
        menu_sep(ui, &pal);
        if cmd_row(ui, app, Cmd::OpenTabInWindow) {
            run = Some(Cmd::OpenTabInWindow);
        }
        menu_sep(ui, &pal);
        for c in [Cmd::CloseTab, Cmd::CloseOtherTabs, Cmd::CloseRightTabs, Cmd::CloseLeftTabs, Cmd::CloseAllTabs] {
            if cmd_row(ui, app, c) {
                run = Some(c);
            }
        }
    });
    if let Some(c) = run {
        app.actions.push(Action::Cmd(c));
        return false;
    }
    !clicked_outside(ctx, rect)
}

fn tabs_options(app: &mut App, ctx: &egui::Context, panel: u64, pos: Pos2) -> bool {
    let pal = app.pal;
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    app.focus_panel(panel);
    let mut run = None;
    let (_, rect) = anchored(ctx, "tabsopts", pos, 280.0, &pal, |ui| {
        for c in [Cmd::NewTab, Cmd::ReopenTab, Cmd::TabSearch, Cmd::CloseAllTabs] {
            if cmd_row(ui, app, c) {
                run = Some(c);
            }
        }
    });
    if let Some(c) = run {
        app.actions.push(Action::Cmd(c));
        return false;
    }
    !clicked_outside(ctx, rect)
}

/// A file in a commit of the history: open it, find it, read it as it was, follow its
/// own history, copy its path.
fn git_file_menu(app: &mut App, ctx: &egui::Context, panel: u64, file: usize, pos: Pos2) -> bool {
    let pal = app.pal;
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    let Some(h) = app.panels.get(&panel).and_then(|p| p.inspector.as_ref()).and_then(|i| i.git.as_ref()) else { return false };
    let Some(f) = h.file(file) else { return false };
    let (path, gone) = (h.root.join(&f.path), f.status == b'D');
    let rel = f.path.clone();
    let item = |ui: &mut Ui, text: &str, enabled: bool| -> bool {
        let r = row(ui, 26.0, false, &pal, |ui, r| {
            ui.painter().text(pos2(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, text, FontId::proportional(13.0), if enabled { pal.text } else { pal.text_dim.gamma_multiply(0.6) });
        });
        enabled && r.clicked()
    };
    enum Pick {
        Open,
        Show,
        AsOf,
        History,
        Copy,
    }
    let mut pick = None;
    let (_, rect) = anchored(ctx, "gitfilemenu", pos, 240.0, &pal, |ui| {
        if item(ui, "Open", !gone) {
            pick = Some(Pick::Open);
        }
        if item(ui, "Show in folder", true) {
            pick = Some(Pick::Show);
        }
        if item(ui, if gone { "View before it was deleted" } else { "View as of this commit" }, true) {
            pick = Some(Pick::AsOf);
        }
        if item(ui, "File history", true) {
            pick = Some(Pick::History);
        }
        menu_sep(ui, &pal);
        if item(ui, "Copy path", true) {
            pick = Some(Pick::Copy);
        }
    });
    let Some(pick) = pick else { return !clicked_outside(ctx, rect) };
    let h = app.panels.get_mut(&panel).and_then(|p| p.inspector.as_mut()).and_then(|i| i.git.as_mut());
    match (pick, h) {
        (Pick::Open, _) => app.actions.push(Action::OpenFile(path)),
        // A deleted file's folder, else the file itself, selected.
        (Pick::Show, _) => app.actions.push(Action::Navigate { panel, path: if gone { path.parent().map(Path::to_path_buf).unwrap_or(path) } else { path } }),
        (Pick::AsOf, Some(h)) => h.view_as_of(file),
        (Pick::History, Some(h)) => h.show_file_history(rel),
        (Pick::Copy, _) => {
            let s = crate::clipboard::quote_if_needed(&path.to_string_lossy());
            ctx.copy_text(s.clone());
            app.toast(format!("Copied {s}"));
        }
        _ => {}
    }
    false
}

/// One entry of the branch menu, in keyboard order.
#[derive(Clone, Copy, PartialEq, Debug)]
enum BranchItem {
    History,
    Pull,
    Push,
    Fetch,
    Branch(usize),
    New,
    Remote(usize),
    Stash(usize),
    Worktree(usize),
}

/// The branch menu's entries, and where its group headings go (before which entry).
fn branch_items(r: Option<&crate::gitwork::Refs>) -> (Vec<BranchItem>, Vec<(usize, &'static str)>) {
    let mut v = vec![BranchItem::History];
    let mut heads = vec![];
    let Some(r) = r else { return (v, heads) };
    // With somewhere to send to and get from, and a branch to do it for.
    if !r.remotes.is_empty() && r.local.iter().any(|b| b.current) {
        v.extend([BranchItem::Pull, BranchItem::Push, BranchItem::Fetch]);
    }
    heads.push((v.len(), "Branches"));
    v.extend((0..r.local.len().min(BRANCHES_SHOWN)).map(BranchItem::Branch));
    v.push(BranchItem::New);
    if !r.remote.is_empty() {
        heads.push((v.len(), "Remote branches"));
        v.extend((0..r.remote.len().min(BRANCHES_SHOWN)).map(BranchItem::Remote));
    }
    if !r.stashes.is_empty() {
        heads.push((v.len(), "Stashes"));
        v.extend((0..r.stashes.len().min(BRANCHES_SHOWN)).map(BranchItem::Stash));
    }
    if !r.worktrees.is_empty() {
        heads.push((v.len(), "Work trees"));
        v.extend((0..r.worktrees.len()).map(BranchItem::Worktree));
    }
    (v, heads)
}

/// Entries listed per group; the most recently used come first.
const BRANCHES_SHOWN: usize = 30;

/// Commits to push (up) and to pull (down), drawn right-aligned at `right`: the bundled
/// fonts have no arrows. Returns where they start.
fn paint_counts(p: &egui::Painter, right: f32, cy: f32, ahead: u32, behind: u32, font: &FontId, col: Color32) -> f32 {
    let mut x = right;
    let arrow = (font.size * 0.7).round();
    for (up, n) in [(false, behind), (true, ahead)] {
        if n == 0 {
            continue;
        }
        let g = p.layout_no_wrap(n.to_string(), font.clone(), col);
        x -= g.size().x;
        p.galley(pos2(x, cy - g.size().y / 2.0), g, col);
        x -= arrow + 2.0;
        let (cx, h) = (x + arrow / 2.0, arrow / 2.0);
        let st = Stroke::new(1.3, col);
        let (tip, tail) = if up { (cy - h, cy + h) } else { (cy + h, cy - h) };
        p.line_segment([pos2(cx, tail), pos2(cx, tip)], st);
        let back = if up { tip + h * 0.7 } else { tip - h * 0.7 };
        p.line_segment([pos2(cx - h * 0.7, back), pos2(cx, tip)], st);
        p.line_segment([pos2(cx + h * 0.7, back), pos2(cx, tip)], st);
        x -= 6.0;
    }
    x
}

/// The branch chip's menu: the history first, then the branches (the current one
/// marked, each with what it has to push and pull), a new branch, the remote branches
/// nothing tracks yet, the stashes and the other work trees. Up, Down and Enter work it;
/// a branch, local or remote, has its own menu on a right click or Shift+F10, and F2 and
/// Delete rename and delete the selected one.
fn branch_menu(app: &mut App, ctx: &egui::Context, st: &mut BranchState) -> bool {
    let pal = app.pal;
    let k = take_keys(ctx);
    if k.esc {
        return false;
    }
    if st.refs.is_none() && st.job.is_none() {
        st.job = Some(crate::gitwork::refs_job(ctx, &st.root));
    }
    if let Some(mut r) = crate::gitlog::take(&st.job) {
        // The current branch first, then the most recently used.
        if let Some(k) = r.local.iter().position(|b| b.current) {
            let b = r.local.remove(k);
            r.local.insert(0, b);
        }
        st.refs = Some(r);
        st.job = None;
    }
    // Hanging from its lower edge, the menu would grow upward under the pointer as the
    // lists come in: it waits for them (a moment at most, then it says it is reading).
    if st.refs.is_none() && st.opened.elapsed() < std::time::Duration::from_millis(300) {
        ctx.request_repaint_after(std::time::Duration::from_millis(15));
        return true;
    }
    let (items, heads) = branch_items(st.refs.as_ref());
    if k.up || k.down || k.page_up || k.page_down {
        st.keyed = true;
    }
    step(&mut st.sel, items.len(), &k, |_| false);
    let history = app.panels.get(&st.panel).and_then(|p| p.inspector.as_ref()).is_some_and(|i| i.git.is_some());
    let k_enter = k.enter;
    let mut pick = if k.enter { items.get(st.sel).copied() } else { None };
    // F2 renames the selected branch, Delete deletes it.
    if let (Some(BranchItem::Branch(b)), Some(r)) = (items.get(st.sel).filter(|_| st.keyed), &st.refs) {
        let (f2, del) = ctx.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::F2), i.consume_key(Modifiers::NONE, Key::Delete)));
        let b = &r.local[*b];
        if f2 {
            rename_branch(app, &st.root, b.name.clone());
            return false;
        }
        if del && !b.current {
            delete_branch(app, &st.root, b.name.clone());
            return false;
        }
    }
    // A branch's own menu: on a right click, or Shift+F10 on the one the keys are on.
    let mut actions = None;
    let menu_key = st.keyed && ctx.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::F10));
    // Opens upward from the chip, hanging from its lower edge.
    let w = 300.0;
    let pos = pos2(st.anchor.right() - w + 8.0, st.anchor.top() - 4.0);
    let font = FontId::proportional(13.0);
    let small = FontId::proportional(11.5);
    let refs = st.refs.clone();
    let (_, rect) = anchored_from(ctx, "branchmenu", pos, Align2::LEFT_BOTTOM, w, &pal, |ui| {
        let mut h = heads.iter().peekable();
        for (i, it) in items.iter().enumerate() {
            if let Some((_, name)) = h.next_if(|(at, _)| *at == i) {
                menu_sep(ui, &pal);
                menu_heading(ui, name, &pal);
            }
            let sel = st.keyed && i == st.sel;
            let resp = row(ui, 26.0, sel, &pal, |ui, r| {
                let p = ui.painter();
                let text = if sel { pal.on_sel() } else { pal.text };
                let dim = if sel { pal.on_sel().gamma_multiply(0.75) } else { pal.text_dim };
                let x = r.left() + 10.0;
                let (label, right): (String, Option<String>) = match (*it, &refs) {
                    (BranchItem::History, _) => ((if history { "Close history" } else { "History" }).into(), None),
                    (BranchItem::Pull, _) => ("Pull".into(), None),
                    (BranchItem::Push, Some(r)) => ((if r.local.iter().any(|b| b.current && b.upstream.is_empty()) { "Publish branch" } else { "Push" }).into(), None),
                    (BranchItem::Fetch, _) => ("Fetch".into(), None),
                    (BranchItem::New, _) => ("New branch…".into(), None),
                    (BranchItem::Branch(b), Some(r)) => (r.local[b].name.clone(), None),
                    (BranchItem::Remote(k), Some(r)) => (r.remote[k].clone(), None),
                    (BranchItem::Stash(k), Some(r)) => (r.stashes[k].subject.clone(), Some(r.stashes[k].sel.clone())),
                    (BranchItem::Worktree(k), Some(r)) => {
                        let w = &r.worktrees[k];
                        (w.path.file_name().map_or_else(|| w.path.display().to_string(), |n| n.to_string_lossy().into_owned()), Some(w.branch.clone()))
                    }
                    _ => (String::new(), None),
                };
                let mut end = r.right() - 8.0;
                // What a pull would bring and a push would send, by the branch HEAD is on.
                if let (BranchItem::Pull | BranchItem::Push, Some(rf)) = (*it, &refs) {
                    if let Some(b) = rf.local.iter().find(|b| b.current) {
                        let (a, d) = if matches!(it, BranchItem::Pull) { (0, b.behind) } else { (b.ahead, 0) };
                        end = paint_counts(p, end, r.center().y, a, d, &small, dim) - 4.0;
                    }
                }
                if let (BranchItem::Branch(b), Some(rf)) = (*it, &refs) {
                    let b = &rf.local[b];
                    end = paint_counts(p, end, r.center().y, b.ahead, b.behind, &small, dim) - 4.0;
                    // The branch HEAD is on carries a dot in the accent.
                    if b.current {
                        p.circle_filled(pos2(x + 3.0, r.center().y), 3.0, if sel { pal.on_sel() } else { pal.accent });
                    }
                }
                if let Some(t) = &right {
                    let g = crate::listing::elided(ui, t, small.clone(), ((end - x) * 0.45).max(20.0), dim);
                    end -= g.size().x;
                    p.galley(pos2(end, r.center().y - g.size().y / 2.0), g, dim);
                    end -= 10.0;
                }
                let lx = if matches!(it, BranchItem::Branch(_)) { x + 12.0 } else { x };
                let g = if matches!(it, BranchItem::Remote(_)) { crate::listing::elided_tail(ui, &label, font.clone(), (end - lx).max(20.0), text) } else { crate::listing::elided(ui, &label, font.clone(), (end - lx).max(20.0), text) };
                p.galley(pos2(lx, r.center().y - g.size().y / 2.0), g, text);
            });
            crate::ui::note_control(ui, resp.rect, "branch menu item");
            let resp = match (*it, &refs) {
                (BranchItem::Worktree(k), Some(r)) => resp.on_hover_text(r.worktrees[k].path.display().to_string()),
                (BranchItem::Branch(b), Some(r)) if !r.local[b].upstream.is_empty() => resp.on_hover_text(format!("Tracks {}", r.local[b].upstream)),
                _ => resp,
            };
            if sel && (k.up || k.down || k.page_up || k.page_down) {
                resp.scroll_to_me(None);
            }
            if resp.clicked() {
                pick = Some(*it);
            }
            let at = || if menu_key { pos2(resp.rect.left() + 40.0, resp.rect.bottom()) } else { ctx.pointer_interact_pos().unwrap_or(resp.rect.center()) };
            if resp.secondary_clicked() || menu_key && sel {
                match (*it, &refs) {
                    (BranchItem::Branch(b), Some(r)) => actions = Some((r.local[b].clone(), at(), false)),
                    (BranchItem::Remote(k), Some(r)) => actions = Some((crate::gitwork::Branch { name: r.remote[k].clone(), ..Default::default() }, at(), true)),
                    _ => {}
                }
            }
        }
        if refs.is_none() {
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(egui::RichText::new("Reading branches…").size(12.0).color(pal.text_dim));
            });
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
    });
    if let Some((branch, anchor, remote)) = actions {
        let into = st.refs.as_ref().and_then(|r| r.local.iter().find(|b| b.current)).map_or_else(|| "HEAD".to_string(), |b| b.name.clone());
        app.popup = Popup::Branch { root: st.root.clone(), branch, anchor, sel: if menu_key { 0 } else { usize::MAX }, panel: st.panel, into, remote };
        return false;
    }
    let Some(it) = pick else { return !clicked_outside(ctx, rect) };
    let (panel, root) = (st.panel, st.root.clone());
    let start = |app: &mut App, op: crate::gitwork::Op, title: String| crate::gitwork::start(&app.ctx, &root, op, title);
    match (it, &st.refs) {
        (BranchItem::History, _) => {
            app.actions.push(Action::FocusPanel(panel));
            match app.panels.get_mut(&panel) {
                Some(p) if history => crate::inspector::close_history(p),
                _ => app.actions.push(Action::Cmd(Cmd::GitHistory)),
            }
        }
        (BranchItem::Pull, _) => start(app, crate::gitwork::Op::Pull(None), "Couldn't pull".into()),
        (BranchItem::Push, _) => start(app, crate::gitwork::Op::Push(false), "Couldn't push".into()),
        (BranchItem::Fetch, _) => start(app, crate::gitwork::Op::Fetch, "Couldn't fetch".into()),
        (BranchItem::Branch(b), Some(r)) if !r.local[b].current => {
            let name = r.local[b].name.clone();
            start(app, crate::gitwork::Op::Switch(name.clone()), format!("Couldn't switch to {name}"));
        }
        (BranchItem::New, _) => {
            app.popup = Popup::prompt(
                "New branch from here",
                String::new(),
                Box::new(move |app, name| {
                    let name = name.trim().to_string();
                    crate::gitwork::start(&app.ctx, &root, crate::gitwork::Op::Create(name.clone()), format!("Couldn't create {name}"));
                }),
            );
        }
        (BranchItem::Remote(k), Some(r)) => {
            let name = r.remote[k].clone();
            start(app, crate::gitwork::Op::Track(name.clone()), format!("Couldn't check out {name}"));
        }
        (BranchItem::Stash(k), Some(r)) => {
            let anchor = pos2(pos.x + 24.0, (rect.top() + 40.0).min(st.anchor.top() - 120.0));
            // Opened by Enter, its first item is ready for the next one.
            app.popup = Popup::Stash { panel, root: root.clone(), stash: r.stashes[k].clone(), anchor, sel: if k_enter { 0 } else { usize::MAX } };
        }
        (BranchItem::Worktree(k), Some(r)) => {
            app.actions.push(Action::NewTab { panel, path: r.worktrees[k].path.clone(), select: None, focus: true });
        }
        _ => {}
    }
    false
}

/// One stash's menu: apply it, apply and drop it, or drop it (after asking).
fn stash_menu(app: &mut App, ctx: &egui::Context, panel: u64, root: &Path, stash: &crate::gitwork::Stash, anchor: Pos2, sel: &mut usize) -> bool {
    let pal = app.pal;
    let k = take_keys(ctx);
    if k.esc {
        return false;
    }
    let _ = panel;
    const ITEMS: [&str; 3] = ["Apply", "Apply and drop", "Drop…"];
    // Nothing is marked until the keys are used.
    if *sel == usize::MAX && (k.up || k.down) {
        *sel = 0;
    } else if *sel != usize::MAX {
        step(sel, ITEMS.len(), &k, |_| false);
    }
    let mut pick = if k.enter && *sel < ITEMS.len() { Some(*sel) } else { None };
    let (_, rect) = anchored(ctx, "stashmenu", anchor, 260.0, &pal, |ui| {
        menu_title(ui, &stash.sel, &stash.subject, &pal);
        for (i, label) in ITEMS.iter().enumerate() {
            let on = i == *sel;
            let resp = row(ui, 26.0, on, &pal, |ui, r| {
                ui.painter().text(pos2(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, *label, FontId::proportional(13.0), if on { pal.on_sel() } else { pal.text });
            });
            crate::ui::note_control(ui, resp.rect, "stash menu item");
            if resp.clicked() {
                pick = Some(i);
            }
        }
    });
    let Some(i) = pick else { return !clicked_outside(ctx, rect) };
    let s = stash.clone();
    match i {
        0 => crate::gitwork::start(&app.ctx, root, crate::gitwork::Op::StashApply(s.clone()), format!("Couldn't apply {}", s.sel)),
        1 => crate::gitwork::start(&app.ctx, root, crate::gitwork::Op::StashPop(s.clone()), format!("Couldn't apply {}", s.sel)),
        _ => {
            let root = root.to_path_buf();
            let msg = format!("Drop {}? {}", s.sel, "It can't be brought back from Maia.");
            app.popup = Popup::confirm(msg, "Drop", Box::new(move |app| crate::gitwork::start(&app.ctx, &root, crate::gitwork::Op::StashDrop(s.clone()), format!("Couldn't drop {}", s.sel))));
        }
    }
    false
}

/// A menu's heading: a name, and under it a dimmer line (a stash's or a commit's subject).
fn menu_title(ui: &mut Ui, name: &str, sub: &str, pal: &Palette) {
    menu_heading(ui, name, pal);
    let g = crate::listing::elided(ui, sub, FontId::proportional(11.5), ui.available_width() - 20.0, pal.text_dim);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), g.size().y + 4.0), Sense::hover());
    ui.painter().galley(pos2(r.left() + 10.0, r.top()), g, pal.text_dim);
    menu_sep(ui, pal);
}

/// One item of a small menu worked by Up, Down and Enter: marked when the keys are on
/// it, dimmed when it can't be used.
fn key_item(ui: &mut Ui, label: &str, on: bool, enabled: bool, pal: &Palette) -> egui::Response {
    let resp = row(ui, 26.0, on, pal, |ui, r| {
        let col = if !enabled { pal.text_dim.gamma_multiply(0.6) } else if on { pal.on_sel() } else { pal.text };
        let g = crate::listing::elided(ui, label, FontId::proportional(13.0), r.width() - 20.0, col);
        ui.painter().galley(pos2(r.left() + 10.0, r.center().y - g.size().y / 2.0), g, col);
    });
    crate::ui::note_control(ui, resp.rect, "menu item");
    resp
}

/// A local branch renamed, its new name asked for.
fn rename_branch(app: &mut App, root: &Path, from: String) {
    let root = root.to_path_buf();
    app.popup = Popup::prompt(
        &format!("Rename {from}"),
        from.clone(),
        Box::new(move |app, to| {
            if to.trim() != from {
                crate::gitwork::start(&app.ctx, &root, crate::gitwork::Op::RenameBranch { to: to.trim().to_string(), from: from.clone() }, format!("Couldn't rename {from}"));
            }
        }),
    );
}

/// A local branch deleted when its commits are on another branch; when not, the app
/// asks whether to delete it anyway.
fn delete_branch(app: &mut App, root: &Path, name: String) {
    crate::gitwork::start(&app.ctx, root, crate::gitwork::Op::DeleteBranch { name: name.clone(), force: false }, format!("Couldn't delete {name}"));
}

/// The history of panel `panel`, opened in its inspector if it isn't yet.
fn history_in(app: &mut App, panel: u64) -> Option<&mut crate::gitlog::History> {
    let p = app.panels.get_mut(&panel)?;
    if p.inspector.as_ref().and_then(|i| i.git.as_ref()).is_none() {
        let r = crate::gitstatus::repo_for(&p.path())?;
        let opened = p.inspector.is_none();
        p.inspector.get_or_insert_with(crate::inspector::Inspector::default).show_history(crate::gitlog::History::new(r.root.clone(), r.branch.clone(), opened));
    }
    p.inspector.as_mut()?.git.as_deref_mut()
}

/// One branch's menu (the branch menu's right click, or Shift+F10): switch to it or, a
/// remote one, check it out; compare it with the branch HEAD is on, or merge it in;
/// rename it, delete it. The branch HEAD is on can only be renamed.
fn branch_actions(app: &mut App, ctx: &egui::Context, root: &Path, b: &crate::gitwork::Branch, anchor: Pos2, sel: &mut usize, (panel, into, remote): (u64, &str, bool)) -> bool {
    let pal = app.pal;
    let k = take_keys(ctx);
    if k.esc {
        return false;
    }
    #[derive(Clone, Copy, PartialEq)]
    enum It {
        Switch,
        Track,
        Compare,
        Merge,
        Rename,
        Delete,
    }
    let items: &[It] = match (remote, b.current) {
        (true, _) => &[It::Track, It::Compare, It::Merge],
        (false, true) => &[It::Rename],
        (false, false) => &[It::Switch, It::Compare, It::Merge, It::Rename, It::Delete],
    };
    if *sel == usize::MAX && (k.up || k.down) {
        *sel = 0;
    } else if *sel != usize::MAX {
        step(sel, items.len(), &k, |_| false);
    }
    let mut pick = if k.enter && *sel < items.len() { Some(items[*sel]) } else { None };
    let (_, rect) = anchored(ctx, "branchactions", anchor, 260.0, &pal, |ui| {
        let sub = match (remote, b.upstream.is_empty()) {
            (true, _) => "Remote branch".to_string(),
            (false, true) => "Not published".to_string(),
            (false, false) => format!("Tracks {}", b.upstream),
        };
        menu_title(ui, &b.name, &sub, &pal);
        for (i, &it) in items.iter().enumerate() {
            if i > 0 && matches!(it, It::Compare | It::Rename) {
                menu_sep(ui, &pal);
            }
            let label = match it {
                It::Switch => "Switch to it".into(),
                It::Track => "Check out".into(),
                It::Compare => format!("Compare with {into}"),
                It::Merge => format!("Merge into {into}"),
                It::Rename => "Rename…".into(),
                It::Delete => "Delete".into(),
            };
            let resp = key_item(ui, &label, i == *sel, true, &pal);
            let resp = match it {
                It::Track => resp.on_hover_text("A local branch of it, tracking it"),
                It::Compare => resp.on_hover_text(format!("The commits each has that the other doesn't, and what {} changes since they split", b.name)),
                It::Merge => resp.on_hover_text("A fast-forward when it can be, else a merge commit. Reset to the commit it was at takes it back."),
                _ => resp,
            };
            if resp.clicked() {
                pick = Some(it);
            }
        }
    });
    let Some(it) = pick else { return !clicked_outside(ctx, rect) };
    let name = b.name.clone();
    match it {
        It::Switch => crate::gitwork::start(&app.ctx, root, crate::gitwork::Op::Switch(name.clone()), format!("Couldn't switch to {name}")),
        It::Track => crate::gitwork::start(&app.ctx, root, crate::gitwork::Op::Track(name.clone()), format!("Couldn't check out {name}")),
        It::Compare => {
            app.actions.push(Action::FocusPanel(panel));
            if let Some(h) = history_in(app, panel) {
                h.compare([into.to_string(), name.clone()], ["HEAD".into(), name], false);
            }
        }
        It::Merge => crate::gitwork::start(&app.ctx, root, crate::gitwork::Op::Merge(name.clone()), format!("Couldn't merge {name}")),
        It::Rename => rename_branch(app, root, name),
        It::Delete => delete_branch(app, root, name),
    }
    false
}

/// A commit's menu in the history: check it out, branch or tag it, cherry-pick or
/// revert it, reset the current branch to it (asking how), mark it to compare with or
/// compare it with the one marked, delete its tags, copy its hash. Up, Down and Enter
/// work it.
fn commit_menu(app: &mut App, ctx: &egui::Context, m: &mut CommitMenu) -> bool {
    use crate::gitwork::Op;
    let pal = app.pal;
    let k = take_keys(ctx);
    if k.esc {
        return false;
    }
    #[derive(Clone, Copy, PartialEq)]
    enum It {
        Checkout,
        Branch,
        Tag,
        Pick,
        Revert,
        Reset,
        Compare,
        Mark,
        Untag(usize),
        Copy,
    }
    let mut items = vec![It::Checkout, It::Branch, It::Tag, It::Pick, It::Revert, It::Reset];
    // Comparing: one commit marked, then another's menu compares with it.
    let other = m.marked.clone().filter(|x| *x != m.hash);
    items.extend(other.is_some().then_some(It::Compare));
    items.push(It::Mark);
    items.extend((0..m.tags.len()).map(It::Untag));
    items.push(It::Copy);
    // HEAD's own commit has nothing to check out, pick or reset to.
    let off = |it: It| m.head && matches!(it, It::Checkout | It::Pick | It::Reset);
    if m.sel == usize::MAX {
        if k.up || k.down {
            m.sel = items.iter().position(|&it| !off(it)).unwrap_or(0);
        }
    } else {
        step(&mut m.sel, items.len(), &k, |i| off(items[i]));
    }
    let mut pick = items.get(m.sel).copied().filter(|_| k.enter);
    let short = &m.hash[..m.hash.len().min(7)];
    let (_, rect) = anchored(ctx, "commitmenu", m.pos, 270.0, &pal, |ui| {
        menu_title(ui, short, &m.subject, &pal);
        for (i, &it) in items.iter().enumerate() {
            if it == It::Pick || it == It::Untag(0) || it == It::Copy && m.tags.is_empty() || it == It::Compare || it == It::Mark && other.is_none() {
                menu_sep(ui, &pal);
            }
            let label = match it {
                It::Checkout => "Check out this commit".into(),
                It::Branch => "Create branch here…".into(),
                It::Tag => "Create tag here…".into(),
                It::Pick => format!("Cherry-pick onto {}", m.branch),
                It::Revert => "Revert".into(),
                It::Reset => format!("Reset {} to here…", m.branch),
                It::Compare => format!("Compare with {}", other.as_deref().map_or("", |x| &x[..x.len().min(7)])),
                It::Mark if m.marked.as_deref() == Some(m.hash.as_str()) => "Unmark".into(),
                It::Mark => "Mark for comparison".into(),
                It::Untag(t) => format!("Delete tag {}", m.tags[t]),
                It::Copy => "Copy hash".into(),
            };
            let resp = key_item(ui, &label, i == m.sel, !off(it), &pal);
            let resp = match it {
                _ if off(it) => resp.on_hover_text("HEAD is on this commit"),
                It::Revert => resp.on_hover_text("A new commit that undoes this one"),
                It::Compare => resp.on_hover_text("The commits each has that the other doesn't, and the files that differ"),
                It::Mark => resp.on_hover_text("Then another commit's menu compares with this one"),
                _ => resp,
            };
            if resp.clicked() && !off(it) {
                pick = Some(it);
            }
        }
    });
    let Some(it) = pick.filter(|&it| !off(it)) else { return !clicked_outside(ctx, rect) };
    let (root, hash, h) = (m.root.clone(), m.hash.clone(), short.to_string());
    let start = |app: &mut App, op: Op, title: String| crate::gitwork::start(&app.ctx, &root, op, title);
    match it {
        It::Checkout => start(app, Op::Checkout(hash), format!("Couldn't check out {h}")),
        It::Branch | It::Tag => app.popup = Popup::NewRef(NewRef { root, at: hash, subject: m.subject.clone(), tag: it == It::Tag, name: String::new(), message: String::new(), focus: true }),
        It::Pick => start(app, Op::CherryPick { hash, merge: m.merge }, format!("Couldn't cherry-pick {h}")),
        It::Revert => start(app, Op::Revert { hash, merge: m.merge }, format!("Couldn't revert {h}")),
        It::Reset => {
            let b = m.branch.clone();
            let go = |hard: bool| -> Option<Box<dyn FnOnce(&mut App)>> {
                let (root, to, title) = (root.clone(), hash.clone(), format!("Couldn't reset {b}"));
                Some(Box::new(move |app: &mut App| crate::gitwork::start(&app.ctx, &root, Op::Reset { to, hard }, title)))
            };
            let choices = vec![("Keep changes".to_string(), false, go(false)), ("Discard changes".to_string(), true, go(true))];
            let sub = format!("Commits after {h} leave {b}. Keep changes leaves what they changed in your files; Discard changes resets the files too, keeping uncommitted work in the stash.");
            app.popup = Popup::Choose { msg: format!("Reset {b} to {h}?"), sub, choices };
        }
        It::Compare => {
            if let (Some(x), Some(hist)) = (other, history_in(app, m.panel)) {
                hist.compare([x[..x.len().min(7)].to_string(), h], [x, hash], true);
            }
        }
        It::Mark => {
            let unmark = m.marked.as_deref() == Some(hash.as_str());
            if let Some(hist) = history_in(app, m.panel) {
                hist.mark = (!unmark).then_some(hash);
            }
            app.toast(if unmark { format!("Unmarked {h}") } else { format!("Marked {h}: another commit's menu compares with it") });
        }
        It::Untag(t) => start(app, Op::DeleteTag(m.tags[t].clone()), format!("Couldn't delete the tag {}", m.tags[t])),
        It::Copy => {
            ctx.copy_text(hash.clone());
            app.toast(format!("Copied {hash}"));
        }
    }
    false
}

/// Create branch here or Create tag here. Enter creates; a branch can be switched to
/// at once (Ctrl+Enter), and a tag with a message is an annotated one.
fn new_ref(app: &mut App, ctx: &egui::Context, st: &mut NewRef) -> bool {
    let pal = app.pal;
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    let mut go = ctx.input_mut(|i| if i.consume_key(Modifiers::CTRL, Key::Enter) { Some(!st.tag) } else { i.consume_key(Modifiers::NONE, Key::Enter).then_some(false) });
    let mut cancel = false;
    let short = &st.at[..st.at.len().min(7)];
    let title = format!("New {} at {short}", if st.tag { "tag" } else { "branch" });
    let named = !st.name.trim().is_empty();
    centered(ctx, "newref", 440.0, &pal, |ui| {
        dialog_title(ui, &title, Some(&st.subject), &pal);
        ui.add_space(8.0);
        let r = ui.add(egui::TextEdit::singleline(&mut st.name).hint_text(egui::RichText::new("Name").color(pal.text_dim)).desired_width(f32::INFINITY));
        crate::ui::note_control(ui, r.rect, "ref name");
        if std::mem::take(&mut st.focus) {
            r.request_focus();
        }
        if st.tag {
            ui.add_space(6.0);
            let m = ui.add(egui::TextEdit::singleline(&mut st.message).hint_text(egui::RichText::new("Message (optional: makes it an annotated tag)").color(pal.text_dim)).desired_width(f32::INFINITY));
            crate::ui::note_control(ui, m.rect, "tag message");
        }
        dialog_buttons(ui, |ui| {
            if primary_button(ui, "Create", named, &pal).clicked() {
                go = Some(false);
            }
            if !st.tag && plain_button(ui, "Create and switch", &pal).on_hover_text("Ctrl+Enter").clicked() {
                go = Some(true);
            }
            if plain_button(ui, "Cancel", &pal).clicked() {
                cancel = true;
            }
        });
    });
    let Some(switch) = go.filter(|_| named) else { return !cancel };
    let (name, at) = (st.name.trim().to_string(), st.at.clone());
    let (op, title) = if st.tag {
        (crate::gitwork::Op::Tag { name, at, message: st.message.clone() }, format!("Couldn't tag {short}"))
    } else {
        (crate::gitwork::Op::Branch { name: name.clone(), at, switch }, format!("Couldn't create {name}"))
    };
    crate::gitwork::start(&app.ctx, &st.root, op, title);
    false
}

/// What went wrong, in git's own words, with one button to close it.
fn notice(app: &mut App, ctx: &egui::Context, title: &str, text: &str) -> bool {
    let pal = app.pal;
    let close = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::NONE, Key::Escape));
    let mut ok = false;
    centered(ctx, "notice", 480.0, &pal, |ui| {
        ui.add_space(4.0);
        dialog_title(ui, title, None, &pal);
        ui.add_space(8.0);
        card(ui, &pal, |ui| {
            ui.add(egui::Label::new(egui::RichText::new(text).font(FontId::monospace(11.5)).color(pal.text)).wrap());
        });
        dialog_buttons(ui, |ui| {
            if primary_button(ui, "OK", true, &pal).clicked() {
                ok = true;
            }
        });
    });
    !(close || ok)
}

fn group_menu(app: &mut App, ctx: &egui::Context, panel: u64, label: &str, pos: Pos2) -> bool {
    let pal = app.pal;
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    let Some(p) = app.panels.get_mut(&panel) else { return false };
    let t = p.tab_mut();
    let labels: Vec<String> = t.header_labels();
    let collapsed = t.collapsed_groups.contains(label);
    let any_collapsed = !t.collapsed_groups.is_empty();
    let all_collapsed = labels.iter().all(|l| t.collapsed_groups.contains(l));
    let mut done = false;
    let item = |ui: &mut Ui, text: &str, enabled: bool| -> bool {
        let r = row(ui, 26.0, false, &pal, |ui, r| {
            ui.painter().text(pos2(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, text, FontId::proportional(13.0), if enabled { pal.text } else { pal.text_dim.gamma_multiply(0.6) });
        });
        enabled && r.clicked()
    };
    let (_, rect) = anchored(ctx, "groupmenu", pos, 230.0, &pal, |ui| {
        if item(ui, if collapsed { "Expand Group" } else { "Collapse Group" }, true) {
            if collapsed {
                t.collapsed_groups.remove(label);
            } else {
                t.collapsed_groups.insert(label.to_string());
            }
            done = true;
        }
        if item(ui, "Expand All Groups", any_collapsed) {
            t.collapsed_groups.clear();
            done = true;
        }
        if item(ui, "Collapse All Groups", !all_collapsed) {
            t.collapsed_groups = labels.iter().cloned().collect();
            done = true;
        }
    });
    if done {
        t.dirty = true;
        return false;
    }
    !clicked_outside(ctx, rect)
}

/// Shown after dragging files with the right mouse button.
fn drop_menu(app: &mut App, ctx: &egui::Context, paths: &[PathBuf], dest: &Path, pos: Pos2) -> bool {
    let pal = app.pal;
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    let mut pick: Option<u8> = None;
    let (_, rect) = anchored(ctx, "dropmenu", pos, 200.0, &pal, |ui| {
        ui.label(egui::RichText::new(format!("{} item{} → {}", paths.len(), if paths.len() == 1 { "" } else { "s" }, dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| dest.display().to_string()))).small().color(pal.text_dim));
        for (k, label) in [(0u8, "Move here"), (1, "Copy here"), (2, "Link here")] {
            if menu_item(ui, label, None, &pal) {
                pick = Some(k);
            }
        }
        menu_sep(ui, &pal);
        if menu_item(ui, "Cancel", None, &pal) {
            pick = Some(9);
        }
    });
    match pick {
        Some(0) => app.begin_transfer(paths.to_vec(), dest.to_path_buf(), true),
        Some(1) => app.begin_transfer(paths.to_vec(), dest.to_path_buf(), false),
        Some(2) => app.actions.push(Action::Drop { paths: paths.to_vec(), dest: dest.to_path_buf(), copy: false, link: true }),
        Some(_) => {}
        None => return !clicked_outside(ctx, rect),
    }
    false
}

/// Header button that condenses a list to titles only (and back).
pub fn compact_button(ui: &mut Ui, on: bool, pal: &Palette) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
    if resp.hovered() || on {
        ui.painter().rect_filled(r, CornerRadius::same(5), if on { pal.accent.gamma_multiply(0.3) } else { pal.hover });
    }
    let c = r.center();
    let col = if on { pal.accent } else { pal.text_dim };
    let st = Stroke::new(1.2, col);
    ui.painter().rect_stroke(Rect::from_center_size(c, vec2(12.0, 13.0)), CornerRadius::same(2), st, StrokeKind::Middle);
    let rows: &[f32] = if on { &[-3.0, 0.0, 3.0] } else { &[-3.0, 1.5] };
    for dy in rows {
        ui.painter().line_segment([c + vec2(-3.5, *dy), c + vec2(3.5, *dy)], st);
    }
    resp.on_hover_text(if on { "Show descriptions" } else { "Titles only" })
}

/// Enter on an option row: flip switches; other controls are left to the mouse.
fn opt_flip(c: &mut crate::config::Config, o: Opt) -> bool {
    let b: &mut bool = match o {
        Opt::RectButtons => &mut c.rect_buttons,
        Opt::FollowSystem => &mut c.follow_system_theme,
        Opt::SyntaxTheme => &mut c.syntax_theme,
        Opt::Round => &mut c.round_corners,
        Opt::Animations => &mut c.animations,
        Opt::Tooltips => &mut c.tooltips,
        Opt::AltRows => &mut c.alt_rows,
        Opt::Breadcrumbs => &mut c.breadcrumbs,
        Opt::CompactPaths => &mut c.compact_paths,
        Opt::ShowExt => &mut c.show_ext,
        Opt::ShowHidden => &mut c.show_hidden,
        Opt::ShowSystem => &mut c.show_system,
        Opt::FoldersFirst => &mut c.folders_first,
        Opt::HighlightRecents => &mut c.highlight_recents,
        Opt::RecentsOnTop => &mut c.recents_on_top,
        Opt::CalcSizes => &mut c.folder_sizes,
        Opt::SizesSsd => &mut c.sizes_ssd,
        Opt::SizesHdd => &mut c.sizes_hdd,
        Opt::SizesNet => &mut c.sizes_network,
        Opt::SelectionBoxes => &mut c.selection_boxes,
        Opt::SystemIcons => &mut c.system_icons,
        Opt::ThumbFill => &mut c.thumb_fill,
        Opt::CheckCopies => &mut c.check_copies,
        Opt::ScrollBeyond => &mut c.scroll_beyond,
        Opt::ActOnPress => &mut c.act_on_press,
        Opt::SingleClick => &mut c.single_click,
        Opt::SingleClickFolders => &mut c.single_click_folders_only,
        Opt::OpenFoldersNewTab => &mut c.open_folders_new_tab,
        Opt::DblClickParent => &mut c.dblclick_bg_parent,
        Opt::PictureViewer => &mut c.picture_viewer,
        Opt::ReverseScroll => &mut c.reverse_scroll,
        Opt::SingleInstance => &mut c.single_instance,
        Opt::Meaning => &mut c.semantic_search,
        Opt::MeaningPictures => &mut c.semantic_pictures,
        Opt::GitPictures => &mut c.github_pictures,
        _ => return false,
    };
    *b = !*b;
    true
}

/// Icon for a palette category.
fn paint_cat_glyph(p: &egui::Painter, r: Rect, cat: Cat, col: Color32) {
    match cat {
        Cat::Quick => crate::ui::star(p, r.center(), r.width() * 0.48, col, false),
        Cat::Folder => paint_glyph(p, r, Glyph::Folder, col),
        Cat::Files => paint_glyph(p, r, Glyph::Docs, col),
        Cat::Selection => checkbox_glyph(p, r.shrink(1.5), true, &crate::theme::Palette { accent: Color32::TRANSPARENT, text: col, text_dim: col, ..crate::theme::palette(ThemeName::Midnight) }),
        Cat::Tabs => {
            let st = Stroke::new(1.2, col);
            let b = r.shrink(1.5);
            p.rect_stroke(Rect::from_min_max(pos2(b.left(), b.top() + 3.0), b.max), CornerRadius::same(2), st, StrokeKind::Middle);
            p.line_segment([pos2(b.left() + 1.0, b.top() + 3.0), pos2(b.left() + 1.0, b.top()), ], st);
            p.line_segment([pos2(b.left() + 1.0, b.top()), pos2(b.center().x, b.top())], st);
            p.line_segment([pos2(b.center().x, b.top()), pos2(b.center().x, b.top() + 3.0)], st);
        }
        Cat::Panels => paint_glyph(p, r, Glyph::Split, col),
        Cat::View => paint_glyph(p, r, Glyph::Pics, col),
        Cat::Sort => {
            let st = Stroke::new(1.2, col);
            let (l, t, b) = (r.left() + 2.0, r.top() + 2.0, r.bottom() - 2.0);
            p.line_segment([pos2(l + 2.5, t), pos2(l + 2.5, b)], st);
            p.add(egui::Shape::line(vec![pos2(l, b - 2.5), pos2(l + 2.5, b), pos2(l + 5.0, b - 2.5)], st));
            for (k, w) in [(0.0, 6.0), (3.5, 4.5), (7.0, 3.0)] {
                p.line_segment([pos2(l + 7.0, t + 1.0 + k), pos2(l + 7.0 + w, t + 1.0 + k)], st);
            }
        }
        Cat::Group => {
            let st = Stroke::new(1.2, col);
            let b = r.shrink(1.5);
            for (y0, y1) in [(b.top(), b.center().y - 1.0), (b.center().y + 1.0, b.bottom())] {
                p.rect_stroke(Rect::from_min_max(pos2(b.left(), y0), pos2(b.right(), y1)), CornerRadius::same(1), st, StrokeKind::Middle);
            }
        }
        Cat::Inspector => paint_glyph(p, r, Glyph::Eye, col),
        Cat::Layouts => paint_glyph(p, r, Glyph::Monitor, col),
        Cat::Misc => paint_glyph(p, r, Glyph::Menu, col),
    }
}
