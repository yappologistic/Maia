//! Copy To… and Move To…: Go To picks the folder, and the copy or move then goes the way
//! F5 and F6 send theirs, through `App::begin_transfer` (conflicts asked, queued per
//! disk, undone).

use std::path::{Path, PathBuf};

use crate::app::App;
use crate::config::UserData;
use crate::popups::Popup;
use crate::tab;

/// Destinations remembered, newest first: enough to cover a week of sorting into the
/// same few folders, few enough that the picker's first group stays short.
pub const KEEP: usize = 8;

/// What Go To does with the folder chosen while it picks a destination.
pub struct Dest {
    pub srcs: Vec<PathBuf>,
    pub mv: bool,
    /// Why the folder chosen last was refused, shown until the input changes.
    pub refused: Option<String>,
}

impl Dest {
    pub fn title(&self) -> String {
        let verb = if self.mv { "Move" } else { "Copy" };
        match self.srcs.as_slice() {
            [one] => format!("{verb} “{}” to…", one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| one.display().to_string())),
            v => format!("{verb} {} items to…", v.len()),
        }
    }
}

/// Go To opened to pick where the selection goes.
pub fn open(app: &mut App, mv: bool) {
    let t = app.tab();
    if tab::is_place(&t.path) {
        return;
    }
    if mv && t.archive.is_some() {
        app.toast("Archives are read-only here: extract it to change it");
        return;
    }
    let srcs = t.targets();
    if srcs.is_empty() {
        app.toast(if mv { "Select what to move" } else { "Select what to copy" });
        return;
    }
    app.popup = Popup::goto_dest(Dest { srcs, mv, refused: None });
}

/// A trash folder (the home one or a drive's `.Trash-1000`), or inside one: things go
/// there through Move To Trash, which keeps where they came from.
pub fn in_trash(p: &Path) -> bool {
    let home = crate::sidebar::trash_dir();
    let home = home.parent().unwrap_or(&home);
    p.ancestors().any(|a| {
        a == home
            || a.file_name().is_some_and(|n| {
                let n = n.to_string_lossy();
                n == ".Trash" || n.starts_with(".Trash-")
            })
    })
}

/// Could `p` be offered as a destination at all? Only names are looked at (no disk
/// access): the picker's rows are filtered with it.
pub fn offered(p: &Path) -> bool {
    !tab::is_place(p) && tab::drive_dev(p).is_none() && !in_trash(p)
}

/// Why `to` can't take `srcs`, or None when it can. Reads the disk (is `to` a folder,
/// where do links lead), so it runs once, when a folder is chosen.
pub fn refusal(srcs: &[PathBuf], to: &Path, mv: bool) -> Option<String> {
    if tab::is_place(to) || tab::drive_dev(to).is_some() {
        return Some("Pick a folder".into());
    }
    if in_trash(to) {
        return Some("Not into the trash: Move To Trash puts things there".into());
    }
    if !to.is_dir() {
        if crate::archive::split(to).is_some() {
            return Some("Archives are read-only: pick a folder outside it".into());
        }
        return Some(format!("No folder at {}", short(to)));
    }
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let rto = real(to);
    // A link to a folder moves as a link; only a real folder can't go inside itself.
    if srcs.iter().any(|s| std::fs::symlink_metadata(s).is_ok_and(|m| m.is_dir()) && rto.starts_with(real(s))) {
        return Some("A folder can't go inside itself".into());
    }
    if mv && srcs.iter().all(|s| s.parent().map(real).as_deref() == Some(rto.as_path())) {
        return Some("Already in that folder".into());
    }
    None
}

/// The selection sent into `to`, or why not.
pub fn send(app: &mut App, d: &Dest, to: PathBuf) -> Result<(), String> {
    if let Some(why) = refusal(&d.srcs, &to, d.mv) {
        return Err(why);
    }
    // What is already there stays put; the rest moves.
    let srcs: Vec<PathBuf> = if d.mv { d.srcs.iter().filter(|s| s.parent() != Some(to.as_path())).cloned().collect() } else { d.srcs.clone() };
    remember(&mut app.ud, &to);
    app.ud_dirty = true;
    app.begin_transfer(srcs, to, d.mv);
    Ok(())
}

pub fn remember(ud: &mut UserData, to: &Path) {
    ud.destinations.retain(|p| p != to);
    ud.destinations.insert(0, to.to_path_buf());
    ud.destinations.truncate(KEEP);
}

/// `p` with the home folder written as ~.
fn short(p: &Path) -> String {
    match p.strip_prefix(crate::fsx::home()) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}
