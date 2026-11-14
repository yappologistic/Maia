//! Network places through GVFS (`gio`): SMB, SFTP, FTP, WebDAV and AFP shares, and
//! phones and cameras over MTP or PTP. GVFS's FUSE bridge shows a mounted place as an
//! ordinary folder (under $XDG_RUNTIME_DIR/gvfs), so browsing it needs nothing special.
//! Passwords are answered to `gio mount` as it asks and never stored.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::drives::Tools;

const SCHEMES: &[&str] = &["smb", "sftp", "ssh", "ftp", "ftps", "dav", "davs", "afp", "nfs", "mtp", "gphoto2"];

/// "smb://nas/media", "sftp://me@host" and the like.
pub fn is_uri(s: &str) -> bool {
    s.split_once("://").is_some_and(|(scheme, rest)| SCHEMES.contains(&scheme.to_ascii_lowercase().as_str()) && !rest.trim().is_empty())
}

/// What `gio` understands ("ssh://" is SFTP; no trailing spaces).
pub fn normalize(s: &str) -> String {
    let s = s.trim();
    match s.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("ssh") => format!("sftp://{rest}"),
        Some((scheme, rest)) => format!("{}://{rest}", scheme.to_ascii_lowercase()),
        None => s.to_string(),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub name: String,
    pub uri: String,
    /// A phone or camera (MTP / PTP) rather than a server share.
    pub device: bool,
    pub mounted: bool,
    /// The folder it shows as, once mounted.
    pub path: Option<PathBuf>,
}

/// Network mounts and attached devices in `gio mount -l -i` output. Local disks (handled
/// by Drives) are left out.
pub fn parse_mounts(text: &str) -> Vec<Place> {
    let mut out: Vec<Place> = vec![];
    // A volume's name and activation root, waiting to see whether a mount follows.
    let mut volume: Option<(String, Option<String>, usize)> = None;
    let flush = |v: &mut Option<(String, Option<String>, usize)>, out: &mut Vec<Place>| {
        if let Some((name, Some(root), _)) = v.take() {
            if !out.iter().any(|p| p.uri == root) && network_uri(&root) {
                out.push(Place { device: is_device(&root), name, uri: root, mounted: false, path: None });
            }
        }
    };
    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Volume(").and_then(|r| r.split_once("): ")) {
            flush(&mut volume, &mut out);
            volume = Some((rest.1.to_string(), None, indent));
        } else if let Some(root) = t.strip_prefix("activation_root=") {
            if let Some(v) = volume.as_mut() {
                v.1 = Some(root.to_string());
            }
        } else if let Some(rest) = t.strip_prefix("Mount(").and_then(|r| r.split_once("): ")) {
            let (name, uri) = rest.1.split_once(" -> ").unwrap_or((rest.1, ""));
            if volume.as_ref().is_some_and(|v| indent > v.2) {
                // The volume is mounted: the mount stands for it.
                volume = None;
            } else {
                flush(&mut volume, &mut out);
            }
            if network_uri(uri) && !out.iter().any(|p| p.uri == uri) {
                out.push(Place { device: is_device(uri), name: name.to_string(), uri: uri.to_string(), mounted: true, path: None });
            }
        } else if t.starts_with("Drive(") {
            flush(&mut volume, &mut out);
        }
    }
    flush(&mut volume, &mut out);
    out
}

fn network_uri(uri: &str) -> bool {
    uri.split_once("://").is_some_and(|(s, _)| SCHEMES.contains(&s))
}

fn is_device(uri: &str) -> bool {
    uri.starts_with("mtp://") || uri.starts_with("gphoto2://")
}

/// Output of a short `gio` call.
fn gio(t: &Tools, args: &[&str]) -> Result<String, String> {
    let mut c = t.cmd("gio");
    c.args(args).env("LC_ALL", "C.UTF-8").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let out = c.output().map_err(|e| format!("gio: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(clean(&String::from_utf8_lossy(&out.stderr)))
    }
}

/// gio's useful words ("gio: smb://x/: Failed to mount…: No such file" -> the reason).
fn clean(err: &str) -> String {
    let line = err.lines().map(str::trim).filter(|l| !l.is_empty()).last().unwrap_or("Failed");
    let line = line.rsplit(": ").next().unwrap_or(line).trim().trim_end_matches('.');
    // What gio says for a host or share that isn't there.
    if line == "No such file or directory" {
        return "No server or share by that name".into();
    }
    let mut s = line.to_string();
    if let Some(c) = s.get(..1) {
        s = c.to_uppercase() + &s[1..];
    }
    s
}

/// The folder a mounted place shows as.
pub fn local_path(t: &Tools, uri: &str) -> Option<PathBuf> {
    let info = gio(t, &["info", uri]).ok()?;
    info.lines().find_map(|l| l.strip_prefix("local path: ")).map(|p| PathBuf::from(p.trim()))
}

/// Mounted shares and attached devices, with their folders.
pub fn places(t: &Tools) -> Vec<Place> {
    let Ok(text) = gio(t, &["mount", "-l", "-i"]) else { return vec![] };
    let mut v = parse_mounts(&text);
    for p in v.iter_mut().filter(|p| p.mounted) {
        p.path = local_path(t, &p.uri);
    }
    v
}

pub struct Login {
    pub user: String,
    pub password: String,
    pub anonymous: bool,
}

/// Mount `uri`, answering gio's questions (user, domain, password) as they come, and
/// return the folder it shows as.
pub fn mount(t: &Tools, uri: &str, login: &Login) -> Result<PathBuf, String> {
    let mut c = t.cmd("gio");
    c.arg("mount");
    if login.anonymous {
        c.arg("-a");
    }
    c.arg(uri).env("LC_ALL", "C.UTF-8").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("gio: {e}"))?;
    let mut stdin = child.stdin.take();
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    for pipe in [child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>), child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>)].into_iter().flatten() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut pipe = pipe;
            let mut buf = [0u8; 512];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);
    let mut seen = String::new();
    let mut asked_password = 0;
    let t0 = Instant::now();
    let status = loop {
        while let Ok(chunk) = rx.try_recv() {
            seen.push_str(&String::from_utf8_lossy(&chunk));
            // Answer the question the output now ends with.
            let tail = seen.trim_end_matches(' ');
            let answer = if tail.ends_with(':') || tail.ends_with("]:") {
                let last = tail.rsplit('\n').next().unwrap_or(tail).to_ascii_lowercase();
                if last.starts_with("user") {
                    Some(login.user.clone())
                } else if last.starts_with("domain") {
                    Some(String::new())
                } else if last.starts_with("password") {
                    asked_password += 1;
                    Some(login.password.clone())
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(a) = answer {
                seen.push('\n');
                if asked_password > 1 {
                    // Asked again: the password was refused.
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("Wrong user name or password".into());
                }
                if let Some(s) = stdin.as_mut() {
                    let _ = s.write_all(format!("{a}\n").as_bytes());
                    let _ = s.flush();
                }
            }
        }
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if t0.elapsed() < Duration::from_secs(90) => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("The server didn't answer".into());
            }
        }
    };
    // Late output (the error text) after the process ended.
    while let Ok(chunk) = rx.recv_timeout(Duration::from_millis(100)) {
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }
    let already = seen.contains("already mounted");
    if !status.success() && !already {
        return Err(clean(&seen));
    }
    local_path(t, uri).ok_or_else(|| "Mounted, but it has no folder to show (is gvfs-fuse running?)".into())
}

pub fn unmount(t: &Tools, uri: &str) -> Result<(), String> {
    gio(t, &["mount", "-u", uri]).map(|_| ())
}

/// What a phone that only shows as plugged in says when it is clicked: GVFS's MTP part,
/// which opens phones, isn't installed (KDE Plasma doesn't install it; Dolphin uses its own).
pub const PHONE_NEEDS: &str = "Opening phones needs gvfs-mtp";
/// And a camera (picture transfer): GVFS's gphoto2 part opens those, a package apart on
/// some systems.
pub const CAMERA_NEEDS: &str = "Opening cameras needs gvfs-gphoto2";

/// The places a device stands for while GVFS can't see it, by the way it transfers:
/// "usb-mtp://1-2" a phone, "usb-ptp://3-1" a camera, each a device on the bus.
const USB_MTP: &str = "usb-mtp://";
const USB_PTP: &str = "usb-ptp://";

pub fn is_usb_only(uri: &str) -> bool {
    uri.starts_with(USB_MTP) || uri.starts_with(USB_PTP)
}

/// What opening a device that only shows as plugged in needs.
pub fn usb_needs(uri: &str) -> &'static str {
    if uri.starts_with(USB_PTP) { CAMERA_NEEDS } else { PHONE_NEEDS }
}

/// The devices on the bus that GVFS doesn't list: a phone unless GVFS lists one over MTP,
/// a camera unless it lists one over gphoto2. One listed kind says nothing of the other,
/// as its MTP and gphoto2 parts are packages apart on some systems.
pub fn usb_unlisted(gvfs: &[Place], usb: Vec<Place>) -> Vec<Place> {
    let has = |scheme: &str| gvfs.iter().any(|p| p.uri.starts_with(scheme));
    let (mtp, ptp) = (has("mtp://"), has("gphoto2://"));
    usb.into_iter().filter(|p| !(p.uri.starts_with(USB_MTP) && mtp || p.uri.starts_with(USB_PTP) && ptp)).collect()
}

/// Phones and cameras plugged in over USB, from `sys` (/sys/bus/usb/devices): every device
/// with a media transfer (MTP) or picture transfer (PTP, still image) interface, as places
/// that can't be opened, named by their maker and model. GVFS lists the same devices
/// itself when its MTP and gphoto2 parts are installed; these stand in when it doesn't,
/// so the phone shows up and can say what opening it needs.
pub fn usb_devices_in(sys: &std::path::Path) -> Vec<Place> {
    let read = |p: PathBuf| std::fs::read_to_string(p).map(|s| s.trim().to_string()).unwrap_or_default();
    // Each device and whether it speaks MTP (else only PTP): an MTP interface wins.
    let mut devs: Vec<(String, bool)> = vec![];
    for e in std::fs::read_dir(sys).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        // An interface is "1-2:1.0": its device is the part before the colon.
        let Some((dev, _)) = name.split_once(':') else { continue };
        let mtp = read(e.path().join("interface")).eq_ignore_ascii_case("MTP");
        if !mtp && read(e.path().join("bInterfaceClass")) != "06" {
            continue;
        }
        match devs.iter_mut().find(|d| d.0 == dev) {
            Some(d) => d.1 |= mtp,
            None => devs.push((dev.to_string(), mtp)),
        }
    }
    devs.sort();
    devs.into_iter()
        .map(|(dev, mtp)| {
            let words = [read(sys.join(&dev).join("manufacturer")), read(sys.join(&dev).join("product"))];
            let mut name = words.iter().filter(|w| !w.is_empty()).cloned().collect::<Vec<_>>().join(" ");
            // "Google Pixel 7" says it once; "Google Google Pixel 7" would twice.
            if let [m, p] = &words {
                if !m.is_empty() && p.starts_with(m.as_str()) {
                    name = p.clone();
                }
            }
            if name.is_empty() {
                name = "Phone".into();
            }
            Place { name, uri: format!("{}{dev}", if mtp { USB_MTP } else { USB_PTP }), device: true, mounted: false, path: None }
        })
        .collect()
}

/// A short name for a place: "media on nas", "me@host", or the device's own name.
pub fn label(uri: &str) -> String {
    let Some((scheme, rest)) = uri.split_once("://") else { return uri.to_string() };
    let rest = rest.trim_end_matches('/');
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    match (scheme, path.split('/').next().filter(|s| !s.is_empty())) {
        ("smb" | "afp" | "nfs", Some(share)) => format!("{share} on {host}"),
        _ => host.to_string(),
    }
}

/// Run `f` on a worker thread; its result comes back through the receiver.
pub fn spawn<T: Send + 'static>(ctx: &egui::Context, f: impl FnOnce() -> T + Send + 'static) -> std::sync::mpsc::Receiver<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let _ = tx.send(f());
        ctx.request_repaint();
    });
    rx
}
