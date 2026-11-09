//! freedesktop.org Trash specification: trash, list, restore, purge.
//! Home trash at $XDG_DATA_HOME/Trash; other filesystems use $topdir/.Trash/$uid
//! (when valid) or $topdir/.Trash-$uid.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Debug)]
pub struct Item {
    /// The .trashinfo file.
    pub info: PathBuf,
    /// The trashed file inside files/.
    pub file: PathBuf,
    pub original: PathBuf,
    pub deleted: String,
}

fn uid() -> u32 {
    unsafe { libc::getuid() }
}

fn home_trash() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| crate::fsx::home().join(".local/share")).join("Trash")
}

/// Top directory (mount point) of the filesystem holding `p`.
pub(crate) fn topdir(p: &Path) -> PathBuf {
    let dev = fs::symlink_metadata(p).map(|m| m.dev()).ok();
    let mut cur = p.to_path_buf();
    while let Some(parent) = cur.parent() {
        if fs::metadata(parent).map(|m| Some(m.dev()) != dev).unwrap_or(true) {
            break;
        }
        cur = parent.to_path_buf();
    }
    cur
}

fn trash_dir_for(p: &Path) -> std::io::Result<PathBuf> {
    let home = home_trash();
    // The home trash may not exist yet (nor ~/.local/share, on a new account): the disk
    // it will be made on is the nearest folder above it that does.
    let home_dev = home.ancestors().find_map(|a| fs::metadata(a).ok()).map(|m| m.dev());
    let dev = fs::symlink_metadata(p)?.dev();
    if Some(dev) == home_dev {
        return Ok(home);
    }
    let top = topdir(p);
    // $topdir/.Trash must be a real, sticky directory.
    let shared = top.join(".Trash");
    if let Ok(m) = fs::symlink_metadata(&shared) {
        if m.is_dir() && m.permissions().mode() & 0o1000 != 0 {
            let d = shared.join(uid().to_string());
            if fs::create_dir_all(&d).is_ok() {
                return Ok(d);
            }
        }
    }
    Ok(top.join(format!(".Trash-{}", uid())))
}

fn encode(p: &Path) -> String {
    let mut s = String::new();
    for &b in p.as_os_str().as_encoded_bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

fn decode(s: &str) -> PathBuf {
    crate::clipboard::percent_decoded(s)
}

/// `tailed("name", ".2")` is "name.2", from the raw bytes.
fn tailed(name: &OsStr, tail: &str) -> OsString {
    let mut b = name.as_bytes().to_vec();
    b.extend_from_slice(tail.as_bytes());
    OsString::from_vec(b)
}

/// Move `p` into the trash.
pub fn delete(p: &Path) -> std::io::Result<()> {
    let abs = if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir()?.join(p) };
    let dir = trash_dir_for(&abs)?;
    let (files, info) = (dir.join("files"), dir.join("info"));
    fs::create_dir_all(&files)?;
    fs::create_dir_all(&info)?;
    let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    // The name it waits under in files/, from its own bytes: a name that isn't UTF-8
    // comes back from the trash as it went in (the .trashinfo keeps the original).
    let base = abs.file_name().map(|n| n.to_os_string()).unwrap_or_else(|| OsString::from("file"));
    // Reserve a unique name atomically via the .trashinfo file.
    for n in 1u32.. {
        let name = if n == 1 { base.clone() } else { tailed(&base, &format!(".{n}")) };
        let mut ib = name.as_bytes().to_vec();
        ib.extend_from_slice(b".trashinfo");
        let info_path = info.join(OsString::from_vec(ib));
        let target = files.join(&name);
        if fs::symlink_metadata(&target).is_ok() {
            continue;
        }
        let mut f = match fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&info_path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        let date = crate::timefmt::format(SystemTime::now(), "%Y-%m-%dT%H:%M:%S");
        // Paths in the home trash are absolute; in topdir trashes, relative to the topdir.
        let rel = if dir.starts_with(home_trash()) { abs.clone() } else { abs.strip_prefix(topdir(&abs)).map(Path::to_path_buf).unwrap_or(abs.clone()) };
        write!(f, "[Trash Info]\nPath={}\nDeletionDate={date}\n", encode(&rel))?;
        drop(f);
        return match fs::rename(&abs, &target) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = fs::remove_file(&info_path);
                Err(e)
            }
        };
    }
    unreachable!()
}

pub(crate) fn trash_dirs() -> Vec<PathBuf> {
    let mut v = vec![home_trash()];
    // Every mount point (including tmpfs and removable media) may hold a topdir trash.
    let mounts = fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    for mp in mounts.lines().filter_map(|l| l.split_whitespace().nth(1)) {
        let mp = PathBuf::from(crate::fsx::unescape_mount(mp));
        for d in [mp.join(".Trash").join(uid().to_string()), mp.join(format!(".Trash-{}", uid()))] {
            if d.join("info").is_dir() && !v.contains(&d) {
                v.push(d);
            }
        }
    }
    v
}

pub fn list() -> Vec<Item> {
    let mut out = vec![];
    for dir in trash_dirs() {
        let home = dir == home_trash();
        let top = dir.parent().map(|p| if p.ends_with(".Trash") { p.parent().unwrap_or(p) } else { p }).unwrap_or(Path::new("/")).to_path_buf();
        let Ok(rd) = fs::read_dir(dir.join("info")) else { continue };
        for de in rd.flatten() {
            let info = de.path();
            let Some(stem) = info.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".trashinfo")) else { continue };
            let Ok(s) = fs::read_to_string(&info) else { continue };
            let mut original = None;
            let mut deleted = String::new();
            for line in s.lines() {
                if let Some(v) = line.strip_prefix("Path=") {
                    let p = decode(v);
                    original = Some(if p.is_absolute() || home { p } else { top.join(p) });
                } else if let Some(v) = line.strip_prefix("DeletionDate=") {
                    deleted = v.to_string();
                }
            }
            if let Some(original) = original {
                out.push(Item { file: dir.join("files").join(stem), info, original, deleted });
            }
        }
    }
    out
}

pub fn restore(it: &Item) -> std::io::Result<()> {
    restore_as(it, &it.original)
}

/// Put `it` back at `to` (its own place, or another name beside it).
pub fn restore_as(it: &Item, to: &Path) -> std::io::Result<()> {
    if fs::symlink_metadata(to).is_ok() {
        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{} already exists", to.display())));
    }
    if let Some(p) = to.parent() {
        fs::create_dir_all(p)?;
    }
    fs::rename(&it.file, to)?;
    fs::remove_file(&it.info)
}

pub fn purge(it: &Item) -> std::io::Result<()> {
    if fs::symlink_metadata(&it.file).is_ok() {
        crate::ops::remove_tree(&it.file)?;
    }
    fs::remove_file(&it.info)
}

/// Original location and deletion time of every item in one trash `files` folder.
pub fn info_for(files_dir: &Path) -> std::collections::HashMap<PathBuf, (PathBuf, Option<SystemTime>)> {
    let mut m = std::collections::HashMap::new();
    let Some(dir) = files_dir.parent() else { return m };
    let home = dir == home_trash();
    let top = dir.parent().map(|p| if p.ends_with(".Trash") { p.parent().unwrap_or(p) } else { p }).unwrap_or(Path::new("/")).to_path_buf();
    let Ok(rd) = fs::read_dir(dir.join("info")) else { return m };
    for de in rd.flatten() {
        let info = de.path();
        let Some(stem) = info.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".trashinfo")) else { continue };
        let Ok(s) = fs::read_to_string(&info) else { continue };
        let (mut original, mut deleted) = (None, None);
        for line in s.lines() {
            if let Some(v) = line.strip_prefix("Path=") {
                let p = decode(v);
                original = Some(if p.is_absolute() || home { p } else { top.join(p) });
            } else if let Some(v) = line.strip_prefix("DeletionDate=") {
                deleted = crate::timefmt::parse_local(v);
            }
        }
        if let Some(o) = original {
            m.insert(files_dir.join(stem), (o, deleted));
        }
    }
    m
}

/// Remove for good what was trashed before `cutoff` in the trash folders `dirs`. An item
/// whose date can't be read stays. Returns how many went.
pub fn empty_older(dirs: &[PathBuf], cutoff: SystemTime) -> usize {
    let mut n = 0;
    for dir in dirs {
        let files = dir.join("files");
        for (file, (_, deleted)) in info_for(&files) {
            if deleted.is_some_and(|d| d < cutoff) {
                let Some(stem) = file.file_name() else { continue };
                let info = dir.join("info").join(format!("{}.trashinfo", stem.to_string_lossy()));
                if purge(&Item { info, file: file.clone(), original: PathBuf::new(), deleted: String::new() }).is_ok() {
                    n += 1;
                }
            }
        }
    }
    n
}

/// `empty_older` in every trash folder there is: the home one and each drive's.
pub fn empty_older_everywhere(days: u32) -> usize {
    let cutoff = SystemTime::now() - std::time::Duration::from_secs(days as u64 * 86_400);
    empty_older(&trash_dirs(), cutoff)
}

/// Is `p` the `files` folder of a trash (home or per-volume)?
pub fn is_trash_files(p: &Path) -> bool {
    p.file_name().map(|n| n == "files").unwrap_or(false) && p.parent().map(|d| d.join("info").is_dir() && (d == home_trash() || d.file_name().map(|n| n.to_string_lossy().starts_with(".Trash")).unwrap_or(false) || d.parent().map(|pp| pp.ends_with(".Trash")).unwrap_or(false))).unwrap_or(false)
}
