//! Searchable context menu with pinning and inline (accordion) submenus.

use std::collections::HashSet;
use std::path::PathBuf;

use egui::{self, Align2, CornerRadius, FontId, Id, Key, Modifiers, Pos2, Rect, Sense, Stroke, Vec2, pos2, vec2};

use crate::app::{Action, App};
use crate::commands::Cmd;
use crate::fuzzy;
use crate::popups::Popup;
use crate::tab;
use crate::theme::{Glyph, paint_glyph};

#[derive(Clone)]
pub enum Act {
    Cmd(Cmd),
    OpenWith(String),
    OtherApp,
    Custom(usize),
    Terminal(PathBuf),
    /// Sidebar actions on one place: (what, panel to act in).
    Place(PlaceAct, u64),
    /// Mount, unmount or eject the target drives.
    Drive(crate::drives::Op),
    /// Unmount this network place.
    Disconnect(String),
    /// Add a tag to the selection, or remove it when every item has it (true).
    Tag(String, bool),
    /// Ask for a new tag's name, then add it.
    NewTag,
    /// A smart folder's entry in the sidebar: (what, its id, panel to act in).
    Smart(SmartAct, u64, u64),
    /// Copy what is selected in a panel's preview (true), or select all of it.
    Preview(bool, u64),
    /// A tag in the sidebar: (what, the tag, panel to act in).
    TagPlace(TagAct, String, u64),
    None,
}

#[derive(Clone, Copy)]
pub enum TagAct {
    Open,
    NewTab,
    Rename,
    Remove,
}

/// The menu of a tag in the sidebar. A colour label keeps its name.
pub fn build_tag(panel: u64, tag: &str, pos: Pos2) -> Popup {
    let it = |key: &str, label: &str, a: TagAct| Item { id: format!("tagplace:{key}"), label: label.into(), act: Act::TagPlace(a, tag.to_string(), panel), children: vec![] };
    let mut items = vec![it("open", "Open", TagAct::Open), it("new_tab", "Open In New Tab", TagAct::NewTab), Item::sep()];
    if crate::tags::color_of(tag).is_none() {
        items.push(it("rename", "Rename Tag…", TagAct::Rename));
    }
    items.push(it("remove", "Remove From All Files…", TagAct::Remove));
    let key = smart_key(crate::smart::tag_id(tag));
    Popup::Ctx(CtxState { sidebar: Some(key), pos, items, query: String::new(), sel: 0, expanded: HashSet::new(), targets: vec![], focus: true, reveal: None })
}

/// "notes.txt", "3 items": what a tag change was made to, for Undo's list.
fn what_tagged(paths: &[PathBuf]) -> String {
    match paths {
        [one] => one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        v => format!("{} items", v.len()),
    }
}

#[derive(Clone, Copy)]
pub enum SmartAct {
    Open,
    NewTab,
    Edit,
    Rename,
    Remove,
}

/// The path the sidebar stands a smart folder's row for (its row stays lit while its
/// menu is open).
pub fn smart_key(id: u64) -> PathBuf {
    PathBuf::from(format!("maia:smart:{id}"))
}

/// The menu of a smart folder in the sidebar.
pub fn build_smart(panel: u64, id: u64, pos: Pos2) -> Popup {
    let it = |key: &str, label: &str, a: SmartAct| Item { id: format!("smart:{key}"), label: label.into(), act: Act::Smart(a, id, panel), children: vec![] };
    let items = vec![it("open", "Open", SmartAct::Open), it("new_tab", "Open In New Tab", SmartAct::NewTab), Item::sep(), it("edit", "Edit…", SmartAct::Edit), it("rename", "Rename…", SmartAct::Rename), Item::sep(), it("remove", "Remove", SmartAct::Remove)];
    Popup::Ctx(CtxState { sidebar: Some(smart_key(id)), pos, items, query: String::new(), sel: 0, expanded: HashSet::new(), targets: vec![], focus: true, reveal: None })
}

/// The menu of a selection in panel `panel`'s preview.
pub fn build_preview(panel: u64, pos: Pos2) -> Popup {
    let it = |key: &str, label: &str, copy: bool| Item { id: format!("preview:{key}"), label: label.into(), act: Act::Preview(copy, panel), children: vec![] };
    let items = vec![it("copy", "Copy", true), it("select_all", "Select All", false)];
    Popup::Ctx(CtxState { sidebar: None, pos, items, query: String::new(), sel: 0, expanded: HashSet::new(), targets: vec![], focus: true, reveal: None })
}

#[derive(Clone, Copy)]
pub enum PlaceAct {
    Open,
    NewTab,
    RightSplit,
    BottomSplit,
    NewWindow,
    CopyPath,
    AddBookmark,
    RemoveBookmark,
    Properties,
}

#[derive(Clone)]
pub struct Item {
    pub id: String,
    pub label: String,
    pub act: Act,
    pub children: Vec<Item>,
}

impl Item {
    fn cmd(c: Cmd) -> Item {
        Item { id: c.info().id.into(), label: c.info().title.into(), act: Act::Cmd(c), children: vec![] }
    }
    fn labeled(c: Cmd, label: &str) -> Item {
        Item { id: c.info().id.into(), label: label.into(), act: Act::Cmd(c), children: vec![] }
    }
    fn sub(id: &str, label: &str, children: Vec<Item>) -> Item {
        Item { id: id.into(), label: label.into(), act: Act::None, children }
    }
    fn drive(op: crate::drives::Op) -> Item {
        let label = match op {
            crate::drives::Op::Mount => "Mount",
            crate::drives::Op::Unmount => "Unmount",
            crate::drives::Op::Eject => "Eject",
            crate::drives::Op::Format => "Format…",
            crate::drives::Op::Unlock => "Unlock…",
            crate::drives::Op::Lock => "Lock",
        };
        Item { id: format!("drive_{}", op.verb()), label: label.into(), act: Act::Drive(op), children: vec![] }
    }
    /// Mount / Unlock / Unmount / Lock / Eject, as they apply to these drives.
    fn drive_items(drives: &[crate::drives::Drive]) -> Vec<Item> {
        use crate::drives::{Crypt, Op, can_lock, can_unmount};
        let mut v = vec![];
        if drives.iter().any(|d| !d.mounted() && d.crypt != Crypt::Locked) {
            v.push(Item::drive(Op::Mount));
        }
        if drives.iter().any(|d| d.crypt == Crypt::Locked) {
            v.push(Item::drive(Op::Unlock));
        }
        if drives.iter().any(can_unmount) {
            v.push(Item::drive(Op::Unmount));
        }
        if drives.iter().any(can_lock) {
            v.push(Item::drive(Op::Lock));
        }
        if drives.iter().any(|d| d.removable) {
            v.push(Item::drive(Op::Eject));
        }
        // One drive at a time: erasing is asked about by name.
        if let [d] = drives {
            if d.removable && crate::drives::Tools::current().is_some_and(|t| t.has("gdbus")) {
                v.push(Item::drive(Op::Format));
            }
        }
        v
    }
    fn sep() -> Item {
        Item { id: "-".into(), label: String::new(), act: Act::None, children: vec![] }
    }
}

pub struct CtxState {
    /// Opened from the sidebar for this place (it stays highlighted meanwhile).
    pub sidebar: Option<PathBuf>,
    pub pos: Pos2,
    pub items: Vec<Item>,
    pub query: String,
    pub sel: usize,
    pub expanded: HashSet<String>,
    pub targets: Vec<PathBuf>,
    focus: bool,
    /// A submenu just opened: scrolled into view on the next frame.
    reveal: Option<String>,
}

pub fn build(app: &mut App, panel: u64, pos: Pos2, background: bool) -> Popup {
    let Some(p) = app.panels.get(&panel) else { return Popup::None };
    let t = p.tab();
    let here = t.path.clone();
    let computer = tab::is_computer(&here);
    let in_trash = here.starts_with(crate::sidebar::trash_dir());
    let ents: Vec<(PathBuf, bool, bool, String)> = if background { vec![] } else { t.target_entries().iter().map(|e| (e.path.clone(), e.is_dir, e.is_link, e.ext().to_string())).collect() };
    let mut items = vec![];
    if ents.is_empty() {
        if t.archive.as_deref().is_some_and(|a| crate::archwrite::kind(a).is_ok()) {
            // Inside a zip, 7z or tarball: what can be made in it.
            items.push(Item::cmd(Cmd::NewFolder));
            items.push(Item::cmd(Cmd::Paste));
            items.push(Item::sep());
        }
        if !computer && t.archive.is_none() {
            items.push(Item::cmd(Cmd::NewFolder));
            items.push(Item::cmd(Cmd::NewFile));
            items.push(Item::cmd(Cmd::Paste));
            if !app.shelf.items.is_empty() {
                items.push(Item::cmd(Cmd::ShelfMoveHere));
                items.push(Item::cmd(Cmd::ShelfCopyHere));
            }
            items.push(Item { id: "terminal_here".into(), label: "Open Terminal Here".into(), act: Act::Terminal(here.clone()), children: vec![] });
            items.push(Item::sep());
        }
        items.push(Item::cmd(Cmd::SelectAll));
        items.push(Item::sub("view", "View", vec![
            Item::labeled(Cmd::ViewDetails, "Details"),
            Item::labeled(Cmd::ViewTree, "Tree"),
            Item::labeled(Cmd::ViewList, "List"),
            Item::labeled(Cmd::ViewMedium, "Medium icons"),
            Item::labeled(Cmd::ViewLarge, "Large icons"),
            Item::labeled(Cmd::ViewXL, "Extra large icons"),
            Item::labeled(Cmd::ViewColumns, "Columns"),
            Item::labeled(Cmd::ViewStack, "Stack"),
        ]));
        items.push(Item::sub("sort", "Sort by", vec![
            Item::labeled(Cmd::SortName, "Name"),
            Item::labeled(Cmd::SortSize, "Size"),
            Item::labeled(Cmd::SortModified, "Date modified"),
            Item::labeled(Cmd::SortCreated, "Date created"),
            Item::labeled(Cmd::SortType, "Type"),
            Item::labeled(Cmd::ToggleFoldersFirst, "Toggle folders first"),
        ]));
        let in_trash_files = crate::trashcan::is_trash_files(&here);
        let group_items: Vec<Item> = crate::commands::GROUP_CMDS
            .iter()
            .filter(|c| match c {
                Cmd::GroupDeleted | Cmd::GroupOriginal => in_trash_files,
                Cmd::GroupFree | Cmd::GroupFs | Cmd::GroupFull => computer,
                Cmd::GroupNone => true,
                _ => !computer,
            })
            .map(|&c| Item::labeled(c, if c == Cmd::GroupNone { "None" } else { c.info().title.trim_start_matches("Group By ") }))
            .collect();
        items.push(Item::sub("group", "Group by", group_items));
        items.push(Item::cmd(Cmd::SavedView));
        items.push(Item::cmd(Cmd::ToggleHidden));
        items.push(Item::cmd(Cmd::Refresh));
        if computer {
            items.push(Item::cmd(Cmd::ConnectServer));
        } else if !in_trash && t.archive.is_none() {
            items.push(Item::cmd(Cmd::FindDuplicates));
            if !t.filter.trim().is_empty() && !t.content.on && t.smart.is_none() {
                items.push(Item::cmd(Cmd::SaveSmart));
            }
            items.push(Item::cmd(Cmd::SpaceUsed));
            items.push(Item::cmd(Cmd::TidyFolder));
        }
        if in_trash {
            items.push(Item::sep());
            items.push(Item::cmd(Cmd::EmptyTrash));
        }
        if !computer {
            items.push(Item::sep());
            items.push(Item::cmd(Cmd::ToggleBookmark));
            items.push(Item::cmd(Cmd::Properties));
        }
    } else if t.archive.is_some() {
        // Inside an archive: open, copy out, extract; in one Maia can write, rename and
        // delete too.
        items.push(Item::labeled(Cmd::Activate, "Open"));
        items.push(Item::cmd(Cmd::Copy));
        items.push(Item::cmd(Cmd::CopyTo));
        items.push(Item::cmd(Cmd::ExtractHere));
        if t.archive.as_deref().is_some_and(|a| crate::archwrite::kind(a).is_ok()) {
            items.push(Item::sep());
            items.push(Item::cmd(Cmd::Rename));
            items.push(Item::labeled(Cmd::Trash, "Delete"));
        }
    } else if computer {
        let drives: Vec<crate::drives::Drive> = ents.iter().filter_map(|e| app.drive_at(&e.0)).collect();
        let mounted: Vec<&PathBuf> = ents.iter().filter(|e| tab::drive_dev(&e.0).is_none()).map(|e| &e.0).collect();
        items.push(Item::labeled(Cmd::Activate, "Open"));
        items.push(Item::cmd(Cmd::OpenInNewTab));
        items.push(Item::cmd(Cmd::OpenInRightSplit));
        items.push(Item::cmd(Cmd::OpenInBottomSplit));
        if mounted.len() == ents.len() {
            items.push(Item::cmd(Cmd::OpenInNewWindow));
            if let [m] = mounted.as_slice() {
                items.push(Item { id: "terminal_here".into(), label: "Open Terminal Here".into(), act: Act::Terminal((*m).clone()), children: vec![] });
            }
        }
        items.push(Item::sep());
        items.push(Item::sub("copy_as", "Copy as", vec![Item::cmd(Cmd::CopyPath), Item::cmd(Cmd::CopyPathRaw)]));
        let d = Item::drive_items(&drives);
        if !d.is_empty() {
            items.push(Item::sep());
            items.extend(d);
        }
        if !mounted.is_empty() {
            items.push(Item::sep());
            items.push(Item::cmd(Cmd::WhatsUsing));
            items.push(Item::cmd(Cmd::Properties));
        }
    } else {
        let single = ents.len() == 1;
        let any_file = ents.iter().any(|e| !e.1);
        let all_dirs = ents.iter().all(|e| e.1);
        items.push(Item::cmd(Cmd::Activate));
        if any_file {
            let db = crate::mime::mime_db();
            let first_file = ents.iter().find(|e| !e.1).unwrap();
            let mime = db.for_path(&first_file.0, false);
            let mut kids: Vec<Item> = crate::mime::apps()
                .for_mime(&mime)
                .into_iter()
                .take(12)
                .map(|a| Item { id: format!("app:{}", a.id), label: a.name.clone(), act: Act::OpenWith(a.id.clone()), children: vec![] })
                .collect();
            kids.push(Item { id: "other_app".into(), label: "Other application…".into(), act: Act::OtherApp, children: vec![] });
            items.push(Item::sub("open_with_menu", "Open with", kids));
        }
        if any_file || single {
            items.push(Item::cmd(Cmd::OpenInEditor));
        }
        if crate::gitstatus::repo_for(&here).is_some() {
            items.push(Item::cmd(Cmd::OpenWebLink));
        }
        if let [a, b] = ents.as_slice() {
            if !a.1 && !b.1 {
                items.push(Item::cmd(Cmd::CompareFiles));
            }
        }
        if let [(_, false, _, ext)] = ents.as_slice() {
            if crate::wallpaper::is_wallpaper(ext) {
                items.push(Item::cmd(Cmd::SetWallpaper));
            }
        }
        items.push(Item::cmd(Cmd::OpenInNewTab));
        items.push(Item::cmd(Cmd::OpenInRightSplit));
        items.push(Item::cmd(Cmd::OpenInBottomSplit));
        items.push(Item::cmd(Cmd::OpenInNewWindow));
        let term_dir = if single && ents[0].1 { ents[0].0.clone() } else { here.clone() };
        items.push(Item { id: "terminal_here".into(), label: "Open Terminal Here".into(), act: Act::Terminal(term_dir), children: vec![] });
        if single && ents[0].2 {
            items.push(Item::cmd(Cmd::OpenLinkTarget));
        }
        // Custom actions applicable to the selection.
        let custom: Vec<Item> = app
            .cfg
            .custom_actions
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                let exts: Vec<String> = a.extensions.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect();
                ents.iter().all(|(_, d, _, ext)| if *d { a.folders } else { a.files && (exts.is_empty() || exts.contains(ext)) })
            })
            .map(|(i, a)| Item { id: format!("custom:{}", a.name), label: a.name.clone(), act: Act::Custom(i), children: vec![] })
            .collect();
        if !custom.is_empty() {
            items.push(Item::sub("custom", "Custom actions", custom));
        }
        items.push(Item::sep());
        items.push(Item::cmd(Cmd::Cut));
        items.push(Item::cmd(Cmd::Copy));
        if single && all_dirs {
            items.push(Item::labeled(Cmd::Paste, "Paste Into Folder"));
        }
        items.push(Item::cmd(Cmd::CopyTo));
        items.push(Item::cmd(Cmd::MoveTo));
        let mut copy = vec![Item::cmd(Cmd::CopyPath), Item::cmd(Cmd::CopyPathRaw), Item::cmd(Cmd::CopyName), Item::cmd(Cmd::CopyPathRelative)];
        // path:line, for the file a search showed a line of.
        if p.line_hint.as_ref().is_some_and(|h| ents.iter().any(|e| e.0 == h.0)) {
            copy.push(Item::cmd(Cmd::CopyPathLine));
        }
        copy.push(Item::cmd(Cmd::CopyCodeBlocks));
        let repo = crate::gitstatus::repo_for(&here);
        if repo.is_some() {
            copy.push(Item::cmd(Cmd::CopyWebLink));
        }
        items.push(Item::sub("copy_as", "Copy as", copy));
        if t.archive.is_none() {
            items.push(Item::cmd(Cmd::SendToPhone));
        }
        if any_file && ents.iter().all(|e| !e.1) {
            items.push(Item::cmd(Cmd::Email));
            let paper = ents.iter().all(|e| crate::sendto::printable(&e.0.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), &e.3));
            if paper && crate::drives::Tools::current().is_some_and(|t| crate::sendto::cups(&t)) {
                items.push(Item::cmd(Cmd::Print));
            }
        }
        items.push(Item::sep());
        items.push(Item::labeled(Cmd::Rename, if single { "Rename" } else { "Rename All (inline)" }));
        if !single {
            items.push(Item::cmd(Cmd::BatchRename));
        }
        items.push(Item::cmd(Cmd::Duplicate));
        // Pictures, with ImageMagick to change them.
        if !in_trash && ents.iter().all(|e| !e.1 && crate::picfix::is_picture(&e.3)) && crate::drives::Tools::current().is_some_and(|t| crate::picfix::magick(&t).is_some()) {
            let mut kids = vec![
                Item::labeled(Cmd::RotateLeft, "Rotate left"),
                Item::labeled(Cmd::RotateRight, "Rotate right"),
                Item::sub("resize", "Resize", vec![
                    Item::labeled(Cmd::ResizeHalf, "Half size"),
                    Item::labeled(Cmd::ResizeQuarter, "Quarter size"),
                    Item::labeled(Cmd::ResizeFit, "Fit within 1920 px"),
                    Item::labeled(Cmd::ResizeCustom, "Other size…"),
                ]),
            ];
            if single && crate::picfix::can_crop(&ents[0].3) {
                kids.push(Item::labeled(Cmd::CropPicture, "Crop…"));
            }
            items.push(Item::sub("picture", "Picture", kids));
        }
        // Below what was there before it, so long menus keep their places.
        if let [(_, false, _, ext)] = ents.as_slice() {
            if crate::viewer::is_picture(ext) {
                items.push(Item::cmd(Cmd::ViewPictures));
                items.push(Item::cmd(Cmd::ViewCalendar));
            }
        }
        // Shown without bsdtar too: chosen, they say what to install.
        if ents.iter().any(|e| !e.1 && crate::archive::is_archive(&e.0.to_string_lossy())) {
            items.push(Item::cmd(Cmd::ExtractHere));
        }
        items.push(Item::sub("compress", "Compress", vec![Item::labeled(Cmd::CompressZip, "Zip (.zip)"), Item::labeled(Cmd::CompressTarGz, "Tar.gz (.tar.gz)"), Item::labeled(Cmd::CompressTarXz, "Tar.xz (.tar.xz)"), Item::labeled(Cmd::Compress7z, "7-Zip (.7z)"), Item::labeled(Cmd::CompressLocked, "With a password…")]));
        if !in_trash && t.archive.is_none() && crate::musictags::applies(&ents.iter().map(|e| (e.3.as_str(), e.1)).collect::<Vec<_>>()) {
            items.push(Item::labeled(Cmd::EditTags, "Edit Tags…"));
        }
        // Files of one kind, into the formats the installed tools write.
        if !in_trash && t.archive.is_none() && ents.iter().all(|e| !e.1) {
            let exts: Vec<&str> = ents.iter().map(|e| e.3.as_str()).collect();
            if let Some(kids) = crate::convert::menu(&exts) {
                items.push(Item::sub("convert", "Convert", kids.into_iter().map(|(c, l)| Item::labeled(c, l)).collect()));
            }
        }
        // Only what git doesn't ignore yet can be added.
        if repo.as_deref().is_some_and(|r| ents.iter().any(|e| e.0 != r.root && !crate::gitstatus::ignored(Some(r), &e.0, &here))) {
            let what = match ents.as_slice() {
                [e] if e.1 => "This Folder".to_string(),
                [_] => "This File".to_string(),
                _ => format!("These {} Items", ents.len()),
            };
            let mut kids = vec![Item::labeled(Cmd::GitIgnore, &what)];
            if let Some(ext) = crate::share::shared_ext(ents.iter().map(|e| (e.0.as_path(), e.1))) {
                kids.push(Item::labeled(Cmd::GitIgnoreExt, &format!("All *.{ext} Files")));
            }
            items.push(Item::sub("gitignore", "Add to .gitignore", kids));
        }
        items.push(Item::cmd(Cmd::CreateLink));
        if in_trash {
            items.push(Item::cmd(Cmd::RestoreTrash));
        } else {
            items.push(Item::cmd(Cmd::Trash));
        }
        items.push(Item::cmd(Cmd::DeletePermanent));
        items.push(Item::sep());
        if t.archive.is_none() && !in_trash {
            items.push(Item::sub("tags", "Tags", tag_items(&ents.iter().map(|e| e.0.clone()).collect::<Vec<_>>())));
        }
        if all_dirs {
            items.push(Item::cmd(Cmd::ToggleBookmark));
        }
        if !in_trash && t.archive.is_none() {
            let all = ents.iter().all(|e| app.pinned_set.contains(&e.0));
            items.push(Item::labeled(Cmd::PinToTop, if all { "Unpin" } else { "Pin To Top" }));
        }
        if !in_trash {
            items.push(Item::cmd(Cmd::WhatsUsing));
        }
        if !in_trash && ents.iter().any(|e| !app.shelf.has(&e.0)) {
            items.push(Item::labeled(Cmd::AddToShelf, "Add to Shelf"));
        }
        if single && !ents[0].1 && !in_trash {
            items.push(Item::cmd(Cmd::PreviousVersions));
        }
        items.push(Item::cmd(Cmd::Properties));
    }
    let targets = ents.into_iter().map(|e| e.0).collect();
    Popup::Ctx(CtxState { sidebar: None, pos, items, query: String::new(), sel: 0, expanded: HashSet::new(), targets, focus: true, reveal: None })
}

/// Context menu for a sidebar place (recent, bookmark, drive or standard folder).
/// The colour labels, then the other tags in use, each marked when every selected item
/// has it; then New Tag.
fn tag_items(paths: &[PathBuf]) -> Vec<Item> {
    // Read now for a usual selection; a huge one goes by what has been read already.
    let have: Vec<Vec<String>> = paths.iter().map(|p| if paths.len() <= 500 { crate::tags::read(p) } else { crate::tags::get(p, |t| t.to_vec()).unwrap_or_default() }).collect();
    let all_have = |tag: &str| !have.is_empty() && have.iter().all(|t| t.iter().any(|x| x.eq_ignore_ascii_case(tag)));
    let mut names: Vec<String> = crate::tags::COLORS.iter().map(|(n, _)| n.to_string()).collect();
    let mut words: Vec<String> = crate::tags::known();
    for t in have.iter().flatten() {
        if crate::tags::color_of(t).is_none() && !words.iter().any(|w| w.eq_ignore_ascii_case(t)) {
            words.push(t.clone());
        }
    }
    words.sort_by_key(|w| w.to_lowercase());
    names.extend(words);
    let mut v: Vec<Item> = names.into_iter().map(|n| Item { id: format!("tag:{n}"), act: Act::Tag(n.clone(), all_have(&n)), label: n, children: vec![] }).collect();
    v.push(Item { id: "new_tag".into(), label: "New Tag…".into(), act: Act::NewTag, children: vec![] });
    v
}

pub fn build_place(panel: u64, path: PathBuf, bookmarked: bool, drive: Option<crate::drives::Drive>, network: Option<String>, pos: Pos2) -> Popup {
    let place = |id: &str, label: &str, a: PlaceAct| Item { id: format!("place:{id}"), label: label.into(), act: Act::Place(a, panel), children: vec![] };
    if let (Some(d), Some(_)) = (&drive, tab::drive_dev(&path)) {
        // A drive that isn't mounted: opening it mounts it.
        let mut items = vec![place("open", "Open", PlaceAct::Open), place("new_tab", "Open In New Tab", PlaceAct::NewTab), Item::sep()];
        items.extend(Item::drive_items(std::slice::from_ref(d)));
        return Popup::Ctx(CtxState { sidebar: Some(path.clone()), pos, items, query: String::new(), sel: 0, expanded: HashSet::new(), targets: vec![path], focus: true, reveal: None });
    }
    let computer = tab::is_computer(&path);
    let mut items = vec![
        place("open", "Open", PlaceAct::Open),
        place("new_tab", "Open In New Tab", PlaceAct::NewTab),
        place("right_split", "Open In Right Split", PlaceAct::RightSplit),
        place("bottom_split", "Open In Bottom Split", PlaceAct::BottomSplit),
        place("new_window", "Open In New Window", PlaceAct::NewWindow),
    ];
    if !computer {
        items.push(Item { id: "terminal_here".into(), label: "Open Terminal Here".into(), act: Act::Terminal(path.clone()), children: vec![] });
        items.push(Item::sep());
        items.push(place("copy_path", "Copy As Path", PlaceAct::CopyPath));
        items.push(Item::sep());
        items.push(if bookmarked { place("unbookmark", "Remove Bookmark", PlaceAct::RemoveBookmark) } else { place("bookmark", "Add Bookmark", PlaceAct::AddBookmark) });
        if let Some(uri) = network {
            items.push(Item::sep());
            items.push(Item { id: "disconnect".into(), label: "Disconnect".into(), act: Act::Disconnect(uri), children: vec![] });
        }
        if let Some(d) = &drive {
            let di = Item::drive_items(std::slice::from_ref(d));
            if !di.is_empty() {
                items.push(Item::sep());
                items.extend(di);
            }
        }
        items.push(Item::sep());
        items.push(place("properties", "Properties", PlaceAct::Properties));
    }
    Popup::Ctx(CtxState { sidebar: Some(path.clone()), pos, items, query: String::new(), sel: 0, expanded: HashSet::new(), targets: vec![path], focus: true, reveal: None })
}

/// Visible rows: (depth, item, breadcrumb label)
fn visible(st: &CtxState, pinned: &[String]) -> Vec<(usize, Item, String, bool)> {
    let mut out = vec![];
    let q = fuzzy::Query::new(&st.query);
    if !q.is_empty() {
        fn walk<'a>(items: &'a [Item], prefix: &str, q: &fuzzy::Query, out: &mut Vec<(i32, &'a Item, String)>) {
            for it in items {
                if it.id == "-" {
                    continue;
                }
                let label = if prefix.is_empty() { it.label.clone() } else { format!("{prefix} › {}", it.label) };
                if it.children.is_empty() {
                    if let Some(s) = fuzzy::score(q, &label.to_lowercase()) {
                        out.push((s, it, label));
                    }
                } else {
                    walk(&it.children, &label, q, out);
                }
            }
        }
        let mut found = vec![];
        walk(&st.items, "", &q, &mut found);
        found.sort_by(|a, b| b.0.cmp(&a.0));
        return found.into_iter().map(|(_, it, l)| (0, it.clone(), l, false)).collect();
    }
    // Pinned leaves first.
    fn leaves<'a>(items: &'a [Item], out: &mut Vec<&'a Item>) {
        for it in items {
            if it.children.is_empty() {
                out.push(it);
            } else {
                leaves(&it.children, out);
            }
        }
    }
    let mut all = vec![];
    leaves(&st.items, &mut all);
    let mut any_pinned = false;
    for pid in pinned {
        if let Some(it) = all.iter().find(|it| it.id == *pid) {
            out.push((0, (*it).clone(), it.label.clone(), true));
            any_pinned = true;
        }
    }
    if any_pinned {
        out.push((0, Item::sep(), String::new(), false));
    }
    fn tree(items: &[Item], depth: usize, st: &CtxState, out: &mut Vec<(usize, Item, String, bool)>) {
        for it in items {
            out.push((depth, it.clone(), it.label.clone(), false));
            if !it.children.is_empty() && st.expanded.contains(&it.id) {
                tree(&it.children, depth + 1, st, out);
            }
        }
    }
    tree(&st.items, 0, st, &mut out);
    out
}


pub fn draw(app: &mut App, ctx: &egui::Context, st: &mut CtxState) -> bool {
    let pal = app.pal;
    let pinned = app.ud.pinned_ctx.clone();
    let rows = visible(st, &pinned);
    let is_sep = |it: &Item| it.id == "-" || it.id.is_empty();
    let (up, down, enter, esc, right, left) = ctx.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::ArrowUp),
            i.consume_key(Modifiers::NONE, Key::ArrowDown),
            i.consume_key(Modifiers::NONE, Key::Enter),
            i.consume_key(Modifiers::NONE, Key::Escape),
            i.consume_key(Modifiers::NONE, Key::ArrowRight),
            i.consume_key(Modifiers::NONE, Key::ArrowLeft),
        )
    });
    if esc {
        return false;
    }
    let n = rows.len();
    if n > 0 {
        let mut s = st.sel.min(n - 1) as isize;
        let d = if down { 1 } else if up { -1 } else { 0 };
        if d != 0 {
            loop {
                s = (s + d).rem_euclid(n as isize);
                if !is_sep(&rows[s as usize].1) {
                    break;
                }
            }
        } else if is_sep(&rows[s as usize].1) {
            s = (s + 1).min(n as isize - 1);
        }
        st.sel = s as usize;
    }
    let mut run: Option<Item> = None;
    let mut toggle: Option<String> = None;
    let mut pin: Option<String> = None;
    if let Some((_, it, _, _)) = rows.get(st.sel) {
        if enter || right {
            if it.children.is_empty() {
                if enter {
                    run = Some(it.clone());
                }
            } else if !st.expanded.contains(&it.id) || enter {
                toggle = Some(it.id.clone());
            }
        }
        if left && !it.children.is_empty() && st.expanded.contains(&it.id) {
            toggle = Some(it.id.clone());
        }
    }
    // Current choices, shown as radio marks on View / Sort by / Group by entries.
    let (cur_group, cur_sort, cur_zoom, cur_columns, cur_stack) = app.panels.get(&app.active).map(|p| {
        let s = &p.tab().settings;
        (s.group, s.sort, s.zoom, s.column_view, s.stack_view)
    }).unwrap_or((crate::config::GroupBy::None, crate::config::SortKey::Name, 0.0, false, false));
    let radio_of = |c: Cmd| -> Option<bool> {
        if let Some(g) = c.group_key() {
            return Some(g == cur_group);
        }
        use crate::config::SortKey as K;
        let sk = match c {
            Cmd::SortName => Some(K::Name),
            Cmd::SortSize => Some(K::Size),
            Cmd::SortModified => Some(K::Modified),
            Cmd::SortType => Some(K::Type),
            Cmd::SortCreated => Some(K::Created),
            _ => None,
        };
        if let Some(k) = sk {
            return Some(k == cur_sort);
        }
        let vz = match c {
            Cmd::ViewDetails => Some(0.0),
            Cmd::ViewTree => Some(0.1),
            Cmd::ViewList => Some(0.2),
            Cmd::ViewMedium => Some(0.3),
            Cmd::ViewLarge => Some(0.65),
            Cmd::ViewXL => Some(1.0),
            _ => None,
        };
        if c == Cmd::ViewColumns {
            return Some(cur_columns);
        }
        if c == Cmd::ViewStack {
            return Some(cur_stack);
        }
        vz.map(|z: f32| !cur_columns && !cur_stack && crate::listing::view_name(z) == crate::listing::view_name(cur_zoom))
    };
    let screen = ctx.content_rect();
    let row_h = 24.0;
    let w = 340.0;
    // The menu's height as drawn: rows and separators each with the Spacing gap after
    // them, the search row and the margins. Short of it, a menu opened low ran off the
    // bottom of the window.
    let gap = ctx.global_style().spacing.item_spacing.y;
    let seps = rows.iter().filter(|r| is_sep(&r.1)).count();
    let est_h = ((n - seps) as f32 * (row_h + gap) + seps as f32 * (7.0 + gap) + 44.0 + 3.0 * gap).min(screen.height() - 20.0);
    let x = st.pos.x.min(screen.right() - w - 6.0).max(4.0);
    let y = if st.pos.y + est_h > screen.bottom() { (screen.bottom() - est_h - 6.0).max(4.0) } else { st.pos.y };
    let mut close_now = false;
    let resp = egui::Area::new(Id::new("ctxmenu")).order(egui::Order::Foreground).fixed_pos(pos2(x, y)).show(ctx, |ui| {
        crate::frost::behind(ui, 10);
        egui::Frame::NONE
            .fill(crate::frost::fill(pal.raised))
            .stroke(Stroke::new(1.0, pal.border))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(egui::Margin::same(5))
            .shadow(egui::epaint::Shadow { offset: [0, 6], blur: 22, spread: 0, color: egui::Color32::from_black_alpha(90) })
            .show(ui, |ui| {
                ui.set_width(w - 10.0);
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
                    paint_glyph(ui.painter(), Rect::from_center_size(r.center(), Vec2::splat(13.0)), Glyph::Search, pal.text_dim);
                    let before = st.query.clone();
                    let te = ui.add(egui::TextEdit::singleline(&mut st.query).hint_text(egui::RichText::new("Select a command…").color(pal.text_dim)).frame(crate::ui::bare_edit()).desired_width(ui.available_width() - 26.0));
                    let (cr, cresp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                    if cresp.hovered() {
                        ui.painter().rect_filled(cr, CornerRadius::same(4), pal.hover);
                    }
                    paint_glyph(ui.painter(), Rect::from_center_size(cr.center(), Vec2::splat(10.0)), Glyph::Close, pal.text_dim);
                    if cresp.on_hover_text("Close (Esc)").clicked() {
                        close_now = true;
                    }
                    if st.focus {
                        te.request_focus();
                        st.focus = false;
                    }
                    if st.query != before {
                        st.sel = 0;
                    }
                });
                {
                    let yy = ui.cursor().top() + 2.0;
                    let x = ui.min_rect().x_range();
                    ui.painter().line_segment([pos2(x.min - 5.0, yy), pos2(x.max + 5.0, yy)], Stroke::new(1.0, pal.border));
                    ui.add_space(5.0);
                }
                // The area holding the menu remembers its last size, which would cap a
                // longer menu opened after a shorter one: let the list claim its full
                // height (it still shrinks to fit).
                egui::ScrollArea::vertical().max_height(screen.height() - 80.0).min_scrolled_height(screen.height() - 80.0).auto_shrink([false, true]).show(ui, |ui| {
                    for (i, (depth, it, label, is_pinned)) in rows.iter().enumerate() {
                        if is_sep(&it) {
                            ui.add_space(3.0);
                            let yy = ui.cursor().top();
                            ui.painter().line_segment([pos2(ui.min_rect().left() + 6.0, yy), pos2(ui.min_rect().right() - 6.0, yy)], Stroke::new(1.0, pal.border));
                            ui.add_space(4.0);
                            continue;
                        }
                        let sel = i == st.sel;
                        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                        // An opened submenu near the bottom: scroll so all of it shows.
                        if st.reveal.as_deref() == Some(it.id.as_str()) {
                            let kids = rows[i + 1..].iter().take_while(|row| row.0 > *depth).count();
                            ui.scroll_to_rect(Rect::from_min_size(r.min, vec2(r.width(), row_h * (kids + 1) as f32)), None);
                            st.reveal = None;
                        }
                        let hovered = resp.hovered();
                        if hovered && !sel {
                            st.sel = i;
                        }
                        if sel {
                            ui.painter().rect_filled(r, CornerRadius::same(4), pal.selected);
                        }
                        let on = if sel { pal.on_sel() } else { pal.text };
                        // Fixed icon column; nested entries indent one step per level.
                        let ic = Rect::from_center_size(pos2(r.left() + 14.0, r.center().y), Vec2::splat(16.0));
                        if let Act::OpenWith(id) = &it.act {
                            if let Some(a) = crate::mime::apps().by_id(id) {
                                crate::icons::named(ui.painter(), ic, &a.icon);
                            }
                        }
                        if let Act::Terminal(_) = &it.act {
                            crate::icons::named(ui.painter(), ic, "utilities-terminal");
                        }
                        if let Act::Tag(name, _) = &it.act {
                            match crate::tags::color_of(name) {
                                Some(c) => {
                                    ui.painter().circle_filled(ic.center(), 5.0, c);
                                }
                                None => paint_glyph(ui.painter(), ic.shrink(1.5), Glyph::Tag, if sel { pal.on_sel() } else { pal.text_dim }),
                            }
                        }
                        let x0 = r.left() + 32.0 + *depth as f32 * 22.0;
                        let enabled = match &it.act {
                            Act::Cmd(c) => !matches!(c, Cmd::Paste) || app.clipboard.is_some() || true,
                            _ => true,
                        };
                        let q = fuzzy::Query::new(&st.query);
                        let job = {
                            let hits: HashSet<usize> = fuzzy::positions(&q, label).map(|(_, p)| p.into_iter().collect()).unwrap_or_default();
                            let mut job = egui::text::LayoutJob::default();
                            for (ci, ch) in label.chars().enumerate() {
                                let col = if hits.contains(&ci) && !sel { pal.accent } else if enabled { on } else { pal.text_dim };
                                job.append(&ch.to_string(), 0.0, egui::TextFormat { font_id: FontId::proportional(12.5), color: col, ..Default::default() });
                            }
                            job
                        };
                        let g = ui.fonts_mut(|f| f.layout_job(job));
                        ui.painter().galley(pos2(x0, r.center().y - g.size().y / 2.0), g, pal.text);
                        let dimc = if sel { pal.on_sel_dim() } else { pal.text_dim };
                        let mut rx = r.right() - 10.0;
                        if !it.children.is_empty() {
                            let c = pos2(rx - 4.0, r.center().y);
                            let g = if st.expanded.contains(&it.id) { Glyph::ChevronDown } else { Glyph::ChevronRight };
                            paint_glyph(ui.painter(), Rect::from_center_size(c, Vec2::splat(12.0)), g, dimc);
                        } else {
                            if let Act::Tag(_, true) = &it.act {
                                // Every selected item has this tag: a check, and a click removes it.
                                let c = pos2(rx - 6.0, r.center().y);
                                let col = if sel { pal.on_sel() } else { pal.accent };
                                ui.painter().add(egui::Shape::line(vec![c + vec2(-4.5, 0.0), c + vec2(-1.5, 3.0), c + vec2(4.5, -3.5)], Stroke::new(1.6, col)));
                                rx -= 22.0;
                            }
                            if let Act::Cmd(c) = &it.act {
                                // Radio mark for the current view / sort / grouping.
                                if let Some(on_now) = radio_of(*c) {
                                    let cc = pos2(rx - 6.0, r.center().y);
                                    if on_now {
                                        ui.painter().circle_filled(cc, 6.5, if sel { pal.on_sel() } else { pal.accent });
                                        ui.painter().circle_filled(cc, 2.5, if sel { pal.selected } else { egui::Color32::WHITE });
                                    } else {
                                        ui.painter().circle_stroke(cc, 6.0, Stroke::new(1.2, dimc.gamma_multiply(0.7)));
                                    }
                                    rx -= 22.0;
                                } else {
                                    // Shortcuts as plain dim text, right-aligned.
                                    for b in app.keymap.bindings(*c).iter().take(2).rev() {
                                        let g = ui.painter().layout_no_wrap(crate::commands::binding_to_string(b), FontId::proportional(11.5), dimc);
                                        ui.painter().galley(pos2(rx - g.size().x, r.center().y - g.size().y / 2.0), g.clone(), dimc);
                                        rx -= g.size().x + 10.0;
                                    }
                                }
                            }
                            // Pin toggle on hover.
                            if hovered || sel || *is_pinned {
                                let sr = Rect::from_center_size(pos2(rx - 10.0, r.center().y), Vec2::splat(18.0));
                                let pinned_now = pinned.contains(&it.id);
                                let sresp = ui.interact(sr, Id::new(("pin", &it.id, i)), Sense::click()).on_hover_text(if pinned_now { "Unpin" } else { "Pin to top" });
                                crate::ui::star(ui.painter(), sr.center(), 5.5, if pinned_now { if sel { pal.on_sel() } else { pal.accent } } else { dimc }, pinned_now);
                                if sresp.clicked() {
                                    pin = Some(it.id.clone());
                                    continue;
                                }
                            }
                        }
                        if resp.clicked() {
                            if it.children.is_empty() {
                                run = Some(it.clone());
                            } else {
                                toggle = Some(it.id.clone());
                            }
                        }
                    }
                    if rows.is_empty() {
                        ui.label(egui::RichText::new("No matching actions").color(pal.text_dim));
                    }
                });
            });
    });
    let rect = resp.response.rect;
    if close_now {
        return false;
    }
    if let Some(id) = toggle {
        if !st.expanded.remove(&id) {
            st.expanded.insert(id.clone());
            st.reveal = Some(id);
        }
    }
    if let Some(id) = pin {
        if let Some(i) = app.ud.pinned_ctx.iter().position(|p| *p == id) {
            app.ud.pinned_ctx.remove(i);
        } else {
            app.ud.pinned_ctx.push(id);
        }
        app.ud_dirty = true;
    }
    if let Some(it) = run {
        match it.act {
            Act::Cmd(c) => app.actions.push(Action::Cmd(c)),
            Act::OpenWith(id) => {
                if let Some(a) = crate::mime::apps().by_id(&id) {
                    let files: Vec<PathBuf> = st.targets.iter().filter(|p| !p.is_dir()).cloned().collect();
                    let cwd = files.first().and_then(|f| f.parent().map(|p| p.to_path_buf()));
                    if let Err(e) = crate::mime::launch(a, &files, cwd.as_deref()) {
                        app.toast(format!("Couldn't launch {}: {e}", a.name));
                    }
                }
            }
            Act::OtherApp => app.actions.push(Action::Open(Popup::OpenWith { paths: st.targets.iter().filter(|p| !p.is_dir()).cloned().collect(), query: String::new(), all: true, command: String::new() })),
            Act::Custom(i) => run_custom(app, i, &st.targets),
            Act::Terminal(dir) => {
                if let Err(e) = crate::mime::open_terminal(&dir) {
                    app.toast(format!("Couldn't start a terminal: {e}"));
                }
            }
            Act::Place(a, panel) => {
                let Some(path) = st.targets.first().cloned() else { return false };
                match a {
                    PlaceAct::Open => app.actions.push(Action::Navigate { panel, path }),
                    PlaceAct::NewTab => app.actions.push(Action::NewTab { panel, path, select: None, focus: true }),
                    PlaceAct::RightSplit => app.actions.push(Action::Split { panel, path, side: crate::panel::Side::Right, select: None }),
                    PlaceAct::BottomSplit => app.actions.push(Action::Split { panel, path, side: crate::panel::Side::Bottom, select: None }),
                    PlaceAct::NewWindow => {
                        let _ = crate::mime::new_window(&path);
                    }
                    PlaceAct::CopyPath => crate::clipboard::set_text(&path.display().to_string()),
                    PlaceAct::AddBookmark => app.actions.push(Action::BookmarkAdd(path)),
                    PlaceAct::RemoveBookmark => app.actions.push(Action::BookmarkRemove(path)),
                    PlaceAct::Properties => app.actions.push(Action::Open(Popup::properties(vec![path]))),
                }
            }
            Act::Disconnect(uri) => {
                let path = st.targets.first().cloned();
                app.disconnect(uri, path);
            }
            Act::Tag(tag, on) => {
                let paths = app.tab().targets();
                let label = format!("{} {} {tag}", if on { "Untag" } else { "Tag" }, what_tagged(&paths));
                let how = if on { crate::tags::How::Remove(tag) } else { crate::tags::How::Add(tag) };
                crate::tags::start(app, crate::tags::Job::Edit(paths, how), crate::tags::Record::New(label));
            }
            Act::NewTag => {
                let paths = app.tab().targets();
                app.actions.push(Action::Open(Popup::prompt(
                    "New tag",
                    String::new(),
                    Box::new(move |app: &mut App, name: String| {
                        let name = name.replace(',', " ").trim().to_string();
                        if name.is_empty() {
                            return;
                        }
                        let label = format!("Tag {} {name}", what_tagged(&paths));
                        crate::tags::start(app, crate::tags::Job::Edit(paths, crate::tags::How::Add(name)), crate::tags::Record::New(label));
                    }),
                )));
            }
            Act::Drive(crate::drives::Op::Format) => {
                if let Some(d) = st.targets.first().and_then(|t| app.drive_at(t)).filter(|d| d.removable) {
                    app.actions.push(Action::Open(crate::formatdrive::open(d)));
                }
            }
            Act::Drive(op) => {
                for t in &st.targets {
                    if let Some(d) = app.drive_at(t) {
                        let applies = match op {
                            crate::drives::Op::Mount => !d.mounted() && d.crypt != crate::drives::Crypt::Locked,
                            crate::drives::Op::Unlock => d.crypt == crate::drives::Crypt::Locked,
                            crate::drives::Op::Unmount => crate::drives::can_unmount(&d),
                            crate::drives::Op::Lock => crate::drives::can_lock(&d),
                            crate::drives::Op::Eject => d.removable,
                            crate::drives::Op::Format => false,
                        };
                        // Ejecting one partition ejects its whole disk: once is enough.
                        if applies && !crate::drives::busy(&d.dev) && !(op == crate::drives::Op::Eject && app.drive_jobs.iter().any(|j| j.drive.disk == d.disk)) {
                            app.drive_op(op, &d.dev, None);
                        }
                    }
                }
            }
            Act::Smart(a, id, panel) => {
                let Some(sf) = app.ud.smart.iter().find(|s| s.id == id).cloned() else { return false };
                match a {
                    SmartAct::Open => app.actions.push(Action::OpenSmart { panel, id, new_tab: false, focus: true }),
                    SmartAct::NewTab => app.actions.push(Action::OpenSmart { panel, id, new_tab: true, focus: true }),
                    SmartAct::Edit => app.actions.push(Action::Open(crate::smart::edit(&sf, true))),
                    SmartAct::Rename => app.actions.push(Action::Open(Popup::prompt("Rename smart folder", sf.name.clone(), Box::new(move |app: &mut App, name: String| app.rename_smart(id, name))))),
                    SmartAct::Remove => app.remove_smart(id),
                }
            }
            Act::Preview(copy, panel) => crate::textsel::menu_act(app, panel, copy),
            Act::TagPlace(a, tag, panel) => {
                let id = crate::smart::tag_id(&tag);
                match a {
                    TagAct::Open => app.actions.push(Action::OpenSmart { panel, id, new_tab: false, focus: true }),
                    TagAct::NewTab => app.actions.push(Action::OpenSmart { panel, id, new_tab: true, focus: true }),
                    TagAct::Rename => app.actions.push(Action::Open(Popup::prompt(
                        "Rename tag",
                        tag.clone(),
                        Box::new(move |app: &mut App, name: String| {
                            let name = name.replace(',', " ").trim().to_string();
                            if !name.is_empty() && name != tag {
                                crate::tags::bulk(app, crate::tags::Bulk::Rename(tag, name));
                            }
                        }),
                    ))),
                    TagAct::Remove => crate::tags::bulk(app, crate::tags::Bulk::Remove(tag)),
                }
            }
            Act::None => {}
        }
        return false;
    }
    let outside = ctx.input(|i| i.pointer.any_pressed()) && ctx.pointer_interact_pos().map(|p| !rect.contains(p)).unwrap_or(false);
    !outside
}

fn run_custom(app: &mut App, i: usize, targets: &[PathBuf]) {
    let Some(a) = app.cfg.custom_actions.get(i).cloned() else { return };
    run_command(app, a, targets);
}

/// Run a command over `targets`, its placeholders filled in: a custom action, or one typed
/// in Open With.
pub fn run_command(app: &mut App, a: crate::config::CustomAction, targets: &[PathBuf]) {
    let dir = app.panels.get(&app.active).map(|p| p.path()).unwrap_or_default();
    let q = |p: &std::path::Path| crate::clipboard::quote_if_needed(&p.to_string_lossy());
    let cmdline = a
        .command
        .replace("%F", &targets.iter().map(|p| q(p)).collect::<Vec<_>>().join(" "))
        .replace("%f", &targets.first().map(|p| q(p)).unwrap_or_default())
        .replace("%d", &q(&dir))
        .replace("%n", &targets.iter().filter_map(|p| p.file_name()).map(|n| crate::clipboard::quote_if_needed(&n.to_string_lossy())).collect::<Vec<_>>().join(" "));
    let (a_confirm, a_name) = (a.confirm, a.name.clone());
    let exec = move |app: &mut App| {
        let res = if a.terminal {
            let entry = crate::mime::App { id: "custom".into(), name: a.name.clone(), exec: format!("sh -c \"{}\"", cmdline.replace('"', "\\\"")), mimes: vec![], terminal: true, hidden: false, icon: String::new() };
            crate::mime::launch(&entry, &[], Some(&dir))
        } else {
            let mut c = std::process::Command::new("sh");
            c.arg("-c").arg(&cmdline).current_dir(&dir);
            crate::mime::spawn_detached(c)
        };
        match res {
            Ok(()) => app.toast(format!("Ran “{}”", a.name)),
            Err(e) => app.toast(format!("“{}” failed: {e}", a.name)),
        }
    };
    if a_confirm {
        app.actions.push(Action::Open(Popup::confirm(format!("Run “{a_name}” on {} item(s)?", targets.len()), "Run", Box::new(exec))));
    } else {
        exec(app);
    }
}

#[allow(dead_code)]
fn _unused(_: Align2) {}
