//! Persistent configuration, user data and session (XDG locations, atomic writes).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::theme::ThemeName;

pub const CONFIG_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
/// Kind of storage a folder lives on (decides folder-size scanning and thumbnails).
pub enum DriveKind {
    #[default]
    Ssd,
    Hdd,
    Network,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RecentsStore {
    /// Remembered across restarts.
    #[default]
    Persistent,
    /// Kept only until Maia quits.
    Session,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ThumbMode {
    /// Everywhere except network locations.
    #[default]
    LocalDisks,
    #[serde(alias = "Always")]
    AllDrives,
    Never,
}

/// How the listing shows what git ignores.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum IgnoredFiles {
    #[default]
    Dim,
    Hide,
    Show,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CharKeyAction {
    #[default]
    TriggerHotkey,
    Filter,
    Jump,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MiddleClick {
    #[default]
    OpenInSplit,
    OpenInNewTab,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum InspectorLayout {
    #[default]
    Auto,
    Right,
    Bottom,
}

/// Markdown, data files and tables each remember Rendered or Source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SourceViews {
    pub markdown: bool,
    pub data: bool,
    pub table: bool,
}

impl SourceViews {
    pub fn get(&self, k: crate::inspector::RichKind) -> bool {
        use crate::inspector::RichKind as K;
        match k {
            K::Markdown => self.markdown,
            K::Data => self.data,
            K::Table => self.table,
        }
    }
    pub fn set(&mut self, k: crate::inspector::RichKind, v: bool) {
        use crate::inspector::RichKind as K;
        match k {
            K::Markdown => self.markdown = v,
            K::Data => self.data = v,
            K::Table => self.table = v,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Startup {
    #[default]
    LastSession,
    Home,
    Layout(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum NewTabShows {
    #[default]
    Current,
    Home,
    Computer,
    Custom(PathBuf),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomAction {
    pub name: String,
    /// Command line; placeholders %f (first path), %F (all paths), %d (dir), %n (names).
    pub command: String,
    /// Comma-separated extensions, empty = any.
    pub extensions: String,
    pub files: bool,
    pub folders: bool,
    pub background: bool,
    pub terminal: bool,
    pub confirm: bool,
}

impl Default for CustomAction {
    fn default() -> Self {
        CustomAction {
            name: String::new(),
            command: String::new(),
            extensions: String::new(),
            files: true,
            folders: true,
            background: false,
            terminal: false,
            confirm: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    // Appearance
    pub theme: ThemeName,
    pub follow_system_theme: bool,
    /// Highlighted code in the theme's own colours (else the same set for every theme).
    pub syntax_theme: bool,
    pub round_corners: bool,
    pub rect_buttons: bool,
    /// UI font family (empty = the system UI font).
    pub font_family: String,
    pub font_size: f32,
    pub inspector_font_size: f32,
    /// Room between lines in text and code previews, as a multiple of the font's own.
    pub preview_line_spacing: f32,
    pub asset_cache_mb: u32,
    pub spacing: f32,
    pub animations: bool,
    pub tooltips: bool,
    pub alt_rows: bool,
    pub inactive_opacity: f32,
    pub breadcrumbs: bool,
    pub compact_paths: bool,
    // Listing
    pub show_ext: bool,
    pub show_hidden: bool,
    pub show_system: bool,
    pub folders_first: bool,
    pub highlight_recents: bool,
    pub recents_on_top: bool,
    pub recents_store: RecentsStore,
    /// Photos sent or attached without where they were taken and the camera's details.
    pub share_without_place: bool,
    /// A folder's README above its files.
    pub show_readme: bool,
    /// The height its card was dragged to, in points (0: its own).
    pub readme_height: f32,
    /// Folder sizes: master switch plus one switch per kind of drive.
    pub folder_sizes: bool,
    pub sizes_ssd: bool,
    pub sizes_hdd: bool,
    pub sizes_network: bool,
    /// Checkboxes on items for mouse multi-selection.
    pub selection_boxes: bool,
    /// File and folder icons from the desktop icon theme (else Maia's built-in icons).
    pub system_icons: bool,
    /// Options and palette lists show titles only (the header toggle).
    pub compact_lists: bool,
    /// Monospace family for text previews in the inspector; empty = system monospace.
    pub inspector_font: String,
    pub thumbnails: ThumbMode,
    pub thumb_fill: bool,
    /// Every copy read back from the disk and compared with what was copied.
    pub check_copies: bool,
    pub scroll_beyond: bool,
    pub date_format: String,
    // Mouse & keyboard
    pub act_on_press: bool,
    pub single_click: bool,
    pub single_click_folders_only: bool,
    pub open_folders_new_tab: bool,
    /// Folders opened from other apps or a second launch go to the open window as tabs.
    pub single_instance: bool,
    /// Search by meaning (a local model through Ollama). Off: nothing runs or is stored.
    pub semantic_search: bool,
    /// The folders it reads, comma-separated ("~" is home).
    pub semantic_folders: String,
    /// Pictures are read too, by how they look.
    pub semantic_pictures: bool,
    pub dblclick_bg_parent: bool,
    /// Enter and a double-click show pictures in Maia's own viewer, not their app.
    pub picture_viewer: bool,
    pub reverse_scroll: bool,
    pub char_key_action: CharKeyAction,
    pub middle_click: MiddleClick,
    // Inspector & startup
    pub inspector_layout: InspectorLayout,
    /// Previews shown as written rather than rendered (the inspector's Source toggle).
    pub source_views: SourceViews,
    /// Diffs shown side by side, old beside new, rather than one under the other.
    pub diff_side: bool,
    pub startup: Startup,
    pub new_tab: NewTabShows,
    /// Authors' GitHub pictures in the history. Off: GitHub is never contacted.
    pub github_pictures: bool,
    /// The editor files open in at a line; empty finds one ($VISUAL, $EDITOR, installed).
    pub editor: String,
    /// Files and folders git ignores: faded, left out, or shown like the rest.
    pub git_ignored: IgnoredFiles,
    /// Days an item stays in the trash before it goes for good (0: it stays).
    pub trash_days: u32,
    pub cross_filesystems: bool,
    // Hotkeys: command id -> bindings (overrides defaults when present).
    pub hotkeys: BTreeMap<String, Vec<String>>,
    pub custom_actions: Vec<CustomAction>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            version: CONFIG_VERSION,
            theme: ThemeName::default(),
            follow_system_theme: false,
            syntax_theme: true,
            round_corners: true,
            rect_buttons: false,
            font_family: String::new(),
            font_size: 12.0,
            inspector_font_size: 12.5,
            preview_line_spacing: 1.25,
            asset_cache_mb: 256,
            spacing: 1.0,
            animations: true,
            tooltips: true,
            alt_rows: true,
            inactive_opacity: 0.85,
            breadcrumbs: true,
            compact_paths: true,
            show_ext: true,
            show_hidden: false,
            show_system: false,
            folders_first: true,
            highlight_recents: true,
            recents_on_top: false,
            recents_store: RecentsStore::Persistent,
            share_without_place: false,
            show_readme: true,
            readme_height: 0.0,
            folder_sizes: true,
            sizes_ssd: true,
            sizes_hdd: false,
            sizes_network: false,
            selection_boxes: true,
            system_icons: true,
            compact_lists: false,
            inspector_font: String::new(),
            thumbnails: ThumbMode::LocalDisks,
            thumb_fill: false,
            check_copies: false,
            scroll_beyond: false,
            date_format: "%x %H:%M".into(),
            act_on_press: true,
            single_click: false,
            single_click_folders_only: false,
            open_folders_new_tab: false,
            single_instance: true,
            semantic_search: false,
            semantic_folders: "~/Documents".into(),
            semantic_pictures: true,
            dblclick_bg_parent: false,
            picture_viewer: false,
            reverse_scroll: false,
            char_key_action: CharKeyAction::TriggerHotkey,
            middle_click: MiddleClick::OpenInSplit,
            inspector_layout: InspectorLayout::Auto,
            source_views: SourceViews::default(),
            diff_side: false,
            startup: Startup::LastSession,
            new_tab: NewTabShows::Current,
            github_pictures: false,
            editor: String::new(),
            git_ignored: IgnoredFiles::Dim,
            trash_days: 0,
            cross_filesystems: false,
            hotkeys: BTreeMap::new(),
            custom_actions: vec![CustomAction {
                name: "Make executable".into(),
                command: "chmod +x %F".into(),
                extensions: "sh,py,AppImage,run".into(),
                folders: false,
                ..Default::default()
            }],
        }
    }
}

// ---------------------------------------------------------------- view settings

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub enum SortKey {
    Name,
    Type,
    Size,
    Modified,
    Created,
    Folders,
    Files,
    Usage,
    Accessed,
    Deleted,
    Original,
    /// By each item's first tag: the colour labels in their order, then the words.
    Tags,
    /// When a photo was taken or a film made.
    Taken,
    /// A picture's or a film's pixels.
    Dimensions,
    Duration,
    /// Pictures by their main colour's hue, then the greys (`colours.rs`).
    Colour,
}

impl SortKey {
    pub fn default_desc(self) -> bool {
        !matches!(self, SortKey::Name | SortKey::Type | SortKey::Original | SortKey::Tags | SortKey::Colour)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub enum Column {
    Name,
    Type,
    Size,
    Modified,
    Created,
    Folders,
    Files,
    Usage,
    Permissions,
    Extension,
    Accessed,
    /// Trash only: when the item was deleted.
    Deleted,
    /// Trash only: the folder it was deleted from.
    Original,
    Tags,
    Taken,
    Dimensions,
    Duration,
}

impl Column {
    pub const ALL: [Column; 17] = [
        Column::Name,
        Column::Type,
        Column::Size,
        Column::Modified,
        Column::Created,
        Column::Folders,
        Column::Files,
        Column::Usage,
        Column::Permissions,
        Column::Extension,
        Column::Accessed,
        Column::Deleted,
        Column::Original,
        Column::Tags,
        Column::Taken,
        Column::Dimensions,
        Column::Duration,
    ];
    pub fn title(self) -> &'static str {
        match self {
            Column::Name => "Name",
            Column::Type => "Type",
            Column::Size => "Size",
            Column::Modified => "Date Modified",
            Column::Created => "Date Created",
            Column::Folders => "Folders",
            Column::Files => "Files",
            Column::Usage => "Folder Usage",
            Column::Permissions => "Permissions",
            Column::Extension => "Extension",
            Column::Accessed => "Date Accessed",
            Column::Deleted => "Date Deleted",
            Column::Original => "Original Location",
            Column::Tags => "Tags",
            Column::Taken => "Date Taken",
            Column::Dimensions => "Dimensions",
            Column::Duration => "Duration",
        }
    }
    pub fn sort_key(self) -> Option<SortKey> {
        Some(match self {
            Column::Name => SortKey::Name,
            Column::Type | Column::Extension => SortKey::Type,
            Column::Size => SortKey::Size,
            Column::Modified => SortKey::Modified,
            Column::Created => SortKey::Created,
            Column::Folders => SortKey::Folders,
            Column::Files => SortKey::Files,
            Column::Usage => SortKey::Usage,
            Column::Accessed => SortKey::Accessed,
            Column::Deleted => SortKey::Deleted,
            Column::Original => SortKey::Original,
            Column::Tags => SortKey::Tags,
            Column::Taken => SortKey::Taken,
            Column::Dimensions => SortKey::Dimensions,
            Column::Duration => SortKey::Duration,
            Column::Permissions => return None,
        })
    }
    pub fn default_width(self) -> f32 {
        match self {
            Column::Name => 320.0,
            Column::Type => 130.0,
            Column::Size => 90.0,
            Column::Modified | Column::Created | Column::Accessed | Column::Deleted => 140.0,
            Column::Original => 220.0,
            Column::Folders | Column::Files => 70.0,
            Column::Usage => 120.0,
            Column::Permissions => 100.0,
            Column::Extension => 80.0,
            Column::Tags => 140.0,
            Column::Taken => 140.0,
            Column::Dimensions => 110.0,
            Column::Duration => 80.0,
        }
    }
    pub fn right_aligned(self) -> bool {
        matches!(self, Column::Size | Column::Folders | Column::Files | Column::Dimensions | Column::Duration)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum GroupBy {
    #[default]
    None,
    Name,
    Type,
    #[serde(alias = "Date")]
    Modified,
    Created,
    Accessed,
    Size,
    /// Folders by how many items they hold.
    TotalItems,
    /// Share of the current folder's size.
    Usage,
    /// Trash only.
    Deleted,
    /// Trash only.
    Original,
    /// Computer only.
    FreeSpace,
    /// Computer only.
    FileSystem,
    /// Computer only.
    PercentFull,
    /// By each item's first tag (as the Tags sort orders them).
    Tag,
    /// By the month a photo was taken or a film made, newest first.
    Taken,
    /// Downloads by the site they came from (what the browser wrote on them).
    Origin,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewSettings {
    pub zoom: f32,
    pub sort: SortKey,
    pub desc: bool,
    pub columns: Vec<(Column, f32)>,
    pub group: GroupBy,
    /// When non-empty, only items whose type label is listed are shown.
    pub only_types: Vec<String>,
    pub show_folders: bool,
    pub show_files: bool,
    /// The Columns view: one column per folder level. It sits beside the zoom axis and
    /// leaves `zoom` as it was.
    pub column_view: bool,
    /// The Stack view: the items standing in a row, the one at the cursor facing you.
    /// Like Columns it sits beside the zoom axis; the two are never on together.
    pub stack_view: bool,
}

impl Default for ViewSettings {
    fn default() -> Self {
        ViewSettings {
            zoom: 0.0,
            sort: SortKey::Name,
            desc: false,
            columns: [Column::Name, Column::Type, Column::Modified, Column::Folders, Column::Files, Column::Size]
                .iter()
                .map(|c| (*c, c.default_width()))
                .collect(),
            group: GroupBy::None,
            only_types: vec![],
            show_folders: true,
            show_files: true,
            column_view: false,
            stack_view: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SaveScope {
    #[default]
    ThisFolder,
    Subfolders,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedView {
    pub scope: SaveScope,
    pub view: ViewSettings,
}

// ---------------------------------------------------------------- layouts & session

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum NodeSnap {
    Leaf(usize),
    Split { vertical: bool, ratio: f32, a: Box<NodeSnap>, b: Box<NodeSnap> },
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TabSnap {
    pub path: PathBuf,
    pub view: ViewSettings,
    pub flatten: bool,
    /// The smart folder shown, by its id.
    pub smart: Option<u64>,
    pub pinned: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PanelSnap {
    pub tabs: Vec<TabSnap>,
    pub active: usize,
    pub inspector: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LayoutSnap {
    pub tree: NodeSnap,
    pub panels: Vec<PanelSnap>,
    pub active: usize,
    #[serde(default)]
    pub window_size: Option<[f32; 2]>,
    #[serde(default)]
    pub maximized: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Session {
    pub layout: Option<LayoutSnap>,
    pub sidebar: bool,
    pub sidebar_width: f32,
    pub terminal_height: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct UserData {
    pub bookmarks: Vec<PathBuf>,
    pub recents: Vec<PathBuf>,
    pub pinned_ctx: Vec<String>,
    pub quick: Vec<String>,
    pub aliases: HashMap<String, String>,
    /// Commands last run from the palette, newest first.
    pub recent_cmds: Vec<String>,
    /// Each git project's tabs and panels as they were left, by the project's top folder.
    pub workspaces: HashMap<PathBuf, LayoutSnap>,
    /// Places by a letter (Ctrl+M to set, Ctrl+' to go).
    pub marks: BTreeMap<String, PathBuf>,
    pub layouts: BTreeMap<String, LayoutSnap>,
    pub views: HashMap<PathBuf, SavedView>,
    pub default_view: Option<ViewSettings>,
    pub collapsed_groups: Vec<String>,
    pub sidebar_collapsed: Vec<String>,
    /// Network addresses connected to before, newest first (no passwords).
    pub servers: Vec<String>,
    /// Saved searches, in the sidebar's order.
    pub smart: Vec<crate::smart::SmartFolder>,
    /// The newest version started (the welcome tour and What's New are shown once).
    pub seen_version: String,
    /// Tags and colour labels known to be on some file, for the sidebar.
    pub tags: Vec<String>,
    /// Folders Copy To… and Move To… sent things into, newest first.
    pub destinations: Vec<PathBuf>,
    /// Each folder left: how many times and when last, for Jump.
    pub visits: HashMap<PathBuf, crate::jump::Visit>,
    /// Files and folders kept first in their folder, whatever the sort.
    pub pinned: Vec<PathBuf>,
    /// Folders whose README was closed.
    pub readme_hidden: Vec<PathBuf>,
}

impl UserData {
    pub fn with_defaults() -> UserData {
        UserData {
            quick: [
                "open_in_new_tab",
                "toggle_flatten",
                "sort_name_asc",
                "sort_name_desc",
                "sort_size_asc",
                "sort_size_desc",
                "toggle_inspector",
                "split_right",
                "split_bottom",
                "new_tab",
                "close_tab",
                "toggle_extensions",
                "toggle_hidden",
                "toggle_system",
                "toggle_recents",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            sidebar_collapsed: vec!["recents".into()],
            ..Default::default()
        }
    }

    pub fn push_recent(&mut self, p: &Path) {
        self.recents.retain(|r| r != p);
        self.recents.insert(0, p.to_path_buf());
        self.recents.truncate(200);
    }

    /// Resolve the view settings for a folder (own → nearest "and subfolders" ancestor → default).
    pub fn view_for(&self, path: &Path) -> (ViewSettings, Option<SaveScope>) {
        self.saved_view_for(path).unwrap_or_else(|| (self.default_view.clone().unwrap_or_default(), None))
    }

    /// The view saved for a folder: its own, or a parent's saved for its subfolders.
    pub fn saved_view_for(&self, path: &Path) -> Option<(ViewSettings, Option<SaveScope>)> {
        if let Some(v) = self.views.get(path) {
            return Some((v.view.clone(), Some(v.scope)));
        }
        let mut cur = path.parent();
        while let Some(p) = cur {
            if let Some(v) = self.views.get(p) {
                if v.scope == SaveScope::Subfolders {
                    return Some((v.view.clone(), None));
                }
            }
            cur = p.parent();
        }
        None
    }
}

// ---------------------------------------------------------------- files

pub fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| crate::fsx::home().join(".config")).join("maia")
}
pub fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| crate::fsx::home().join(".local/share")).join("maia")
}
pub fn state_dir() -> PathBuf {
    dirs::state_dir().unwrap_or_else(|| crate::fsx::home().join(".local/state")).join("maia")
}
pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}
pub fn userdata_path() -> PathBuf {
    data_dir().join("userdata.json")
}
pub fn session_path() -> PathBuf {
    state_dir().join("session.json")
}

/// Atomic write: temp file, fsync, rename.
pub fn save_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let data = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Lenient load; on parse failure keep a .bak copy and return defaults.
pub fn load_json<T: DeserializeOwned + Default>(path: &Path) -> T {
    let Ok(bytes) = std::fs::read(path) else { return T::default() };
    match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("maia: could not parse {}: {e}; keeping a .bak copy", path.display());
            let _ = std::fs::copy(path, path.with_extension("json.bak"));
            T::default()
        }
    }
}

pub fn load_config() -> Config {
    let path = config_path();
    let cfg: Config = load_json(&path);
    if path.exists() && cfg.version > CONFIG_VERSION {
        let _ = std::fs::copy(&path, path.with_extension("json.bak"));
    }
    cfg
}

pub fn load_userdata() -> UserData {
    if userdata_path().exists() { load_json(&userdata_path()) } else { UserData::with_defaults() }
}

pub fn mtime(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

impl Config {
    pub fn track_recents(&self) -> bool {
        self.recents_store != RecentsStore::Off
    }
    /// May folder sizes be computed for a folder on this kind of drive?
    pub fn sizes_on(&self, k: DriveKind) -> bool {
        self.folder_sizes && match k {
            DriveKind::Ssd => self.sizes_ssd,
            DriveKind::Hdd => self.sizes_hdd,
            DriveKind::Network => self.sizes_network,
        }
    }
    pub fn thumbs_on(&self, k: DriveKind) -> bool {
        match self.thumbnails {
            ThumbMode::Never => false,
            ThumbMode::AllDrives => true,
            ThumbMode::LocalDisks => k != DriveKind::Network,
        }
    }
}
