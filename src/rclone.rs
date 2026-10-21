//! rclone remotes (Google Drive, OneDrive, S3, ...) as network places: each remote in
//! `rclone listremotes` shows in the sidebar, connected while it is mounted. Connecting
//! starts the user's own unit `rclone-<remote>.service` when there is one, so its mount
//! options stay in charge, and else runs `rclone mount` at ~/<remote>. Disconnecting
//! refuses while the unit reports files still waiting to upload, then unmounts.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::drives::Tools;
use crate::network::Place;

pub const SCHEME: &str = "rclone:";

pub fn uri(remote: &str) -> String {
    format!("{SCHEME}{remote}")
}

/// The remote a place's URI names, if it is one of these.
pub fn remote_of(uri: &str) -> Option<&str> {
    uri.strip_prefix(SCHEME).filter(|r| !r.is_empty())
}

/// `rclone listremotes`: one "name:" a line.
pub fn parse_remotes(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter_map(|l| l.strip_suffix(':')).filter(|n| !n.is_empty()).map(str::to_string).collect()
}

/// Each remote as a place, connected where the mount table has it (`gdrive:` or
/// `gdrive:Some/Folder` mounted with type fuse.rclone).
pub fn places_from(remotes: &[String], mounts: &[crate::fsx::Mount]) -> Vec<Place> {
    remotes
        .iter()
        .map(|r| {
            let at = mounts.iter().find(|m| m.fstype == "fuse.rclone" && m.device.split_once(':').is_some_and(|(n, _)| n == r)).map(|m| m.path.clone());
            Place { name: r.clone(), uri: uri(r), device: false, mounted: at.is_some(), path: at }
        })
        .collect()
}

/// The remotes rclone knows, against the mount table at `table`. None without rclone.
pub fn places(t: &Tools, table: &Path) -> Vec<Place> {
    if !t.has("rclone") {
        return vec![];
    }
    let mut c = t.cmd("rclone");
    c.arg("listremotes");
    let Ok(out) = crate::drives::run(c, Duration::from_secs(5)) else { return vec![] };
    places_from(&parse_remotes(&out), &crate::fsx::mounts_in(table))
}

fn mounted_at(table: &Path, remote: &str) -> Option<PathBuf> {
    places_from(&[remote.to_string()], &crate::fsx::mounts_in(table)).into_iter().next()?.path
}

/// The user's unit for `remote`, when systemd knows one.
fn unit(t: &Tools, remote: &str) -> Option<String> {
    if !t.has("systemctl") {
        return None;
    }
    let u = format!("rclone-{remote}.service");
    let mut c = t.cmd("systemctl");
    c.args(["--user", "show", "-p", "LoadState", "--value", &u]);
    let out = crate::drives::run(c, Duration::from_secs(5)).ok()?;
    (out.trim() == "loaded").then_some(u)
}

/// Files not yet on the remote, from the status rclone gives its unit
/// ("vfs cache: objects 3 (was 3) in use 0, to upload 2, uploading 1, ...").
pub fn pending_uploads(status: &str) -> u64 {
    let after = |key: &str| status.split(key).nth(1).map(|s| s.chars().take_while(char::is_ascii_digit).collect::<String>()).and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
    after("to upload ") + after("uploading ")
}

/// Connect `remote` and wait for its folder: the unit when there is one, else
/// `rclone mount` at ~/<remote>.
pub fn mount(t: &Tools, table: &Path, remote: &str) -> Result<PathBuf, String> {
    if let Some(at) = mounted_at(table, remote) {
        return Ok(at);
    }
    match unit(t, remote) {
        Some(u) => {
            let mut c = t.cmd("systemctl");
            c.args(["--user", "start", &u]);
            crate::drives::run(c, Duration::from_secs(60))?;
        }
        None => {
            let dir = crate::fsx::home().join(remote);
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let mut c = t.cmd("rclone");
            c.arg("mount").arg(format!("{remote}:")).arg(&dir).args(["--daemon", "--vfs-cache-mode", "writes"]);
            crate::drives::run(c, Duration::from_secs(60))?;
        }
    }
    // The mount shows up a moment after the unit or the daemon has started.
    for _ in 0..150 {
        if let Some(at) = mounted_at(table, remote) {
            return Ok(at);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!("{remote} started but its folder never appeared"))
}

/// Disconnect `remote`: refused while files still wait to upload, or while something
/// has files open in its folder; then the unit, if any, is stopped.
pub fn unmount(t: &Tools, table: &Path, remote: &str) -> Result<(), String> {
    let unit = unit(t, remote);
    if let Some(u) = &unit {
        let mut c = t.cmd("systemctl");
        c.args(["--user", "show", "-p", "StatusText", "--value", u]);
        let n = pending_uploads(&crate::drives::run(c, Duration::from_secs(5)).unwrap_or_default());
        if n > 0 {
            return Err(format!("{n} {} still uploading. Try again in a minute", if n == 1 { "file is" } else { "files are" }));
        }
    }
    if let Some(at) = mounted_at(table, remote) {
        let mut c = t.cmd(if t.has("fusermount3") { "fusermount3" } else { "fusermount" });
        c.arg("-u").arg(&at);
        crate::drives::run(c, Duration::from_secs(15)).map_err(|e| if e.to_lowercase().contains("busy") { "something still has files open there".to_string() } else { e })?;
    }
    if let Some(u) = unit {
        let mut c = t.cmd("systemctl");
        c.args(["--user", "stop", &u]);
        crate::drives::run(c, Duration::from_secs(30))?;
    }
    Ok(())
}
