//! Archives as folders, through `bsdtar` (libarchive: zip, tar and its compressions, 7z,
//! rar, iso, cpio, deb, rpm). A path inside an archive is the archive's path joined with
//! the path inside it, so tabs, breadcrumbs and history need nothing special. What is
//! inside is extracted to be opened or copied out; changes are `archwrite`'s. A lone
//! compressed file (notes.txt.gz) is an archive of one file, decompressed by `bsdcat`.
//!
//! Passwords: libarchive decrypts zip but not 7z, so encrypted 7z goes to the `7z`
//! command when it is installed. Both tools run without a terminal and read the password
//! from a pipe (never the command line, where any user could see it). Passwords are kept
//! in memory for the session and never written anywhere.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

const EXTS: &[&str] = &[".zip", ".tar", ".tar.gz", ".tgz", ".tar.bz2", ".tbz2", ".tbz", ".tar.xz", ".txz", ".tar.zst", ".tzst", ".tar.lz", ".tar.lzma", ".7z", ".rar", ".iso", ".cpio", ".jar", ".apk", ".cbz", ".cbr", ".xpi", ".deb", ".rpm", ".gz", ".bz2", ".xz", ".zst"];

/// Compressions of one file, without a tar inside: (extension, the tool that undoes it
/// when bsdcat is missing).
const SINGLE: [(&str, &str); 4] = [(".gz", "gzip"), (".bz2", "bzip2"), (".xz", "xz"), (".zst", "zstd")];

/// A lone compressed file ("notes.txt.gz"), which holds one file named by its stem.
pub fn single(archive: &Path) -> bool {
    let n = archive.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    SINGLE.iter().any(|(e, _)| n.ends_with(e)) && !n.contains(".tar.")
}

/// A command that writes `archive`'s one file, decompressed, to its output.
fn cat(archive: &Path) -> Result<Command, String> {
    if crate::media::have("bsdcat") {
        let mut c = tool("bsdcat");
        c.arg("--").arg(archive);
        return Ok(c);
    }
    let n = archive.to_string_lossy().to_lowercase();
    let t = SINGLE.iter().find(|(e, _)| n.ends_with(e)).map(|x| x.1).filter(|t| crate::media::have(t)).ok_or("bsdcat isn't installed")?;
    let mut c = tool(t);
    c.args(["-dc", "--"]).arg(archive);
    Ok(c)
}

/// The listing of a lone compressed file: its one file, sized from gzip's trailer or by
/// decompressing it (bzip2, xz and zstd keep no size that is always there).
fn list_single(archive: &Path) -> Result<Vec<Item>, String> {
    let name = archive.file_name().map(|n| stem(&n.to_string_lossy()).to_string()).unwrap_or_default();
    let meta = std::fs::metadata(archive).map_err(|e| e.to_string())?;
    let gz = archive.to_string_lossy().to_lowercase().ends_with(".gz");
    let size = if gz && meta.len() >= 18 {
        use std::os::unix::fs::FileExt;
        let mut b = [0u8; 4];
        std::fs::File::open(archive).and_then(|f| f.read_exact_at(&mut b, meta.len() - 4)).map_err(|e| e.to_string())?;
        u32::from_le_bytes(b) as u64
    } else {
        let mut c = cat(archive)?;
        c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = c.spawn().map_err(|e| e.to_string())?;
        let mut n = 0u64;
        if let Some(mut o) = child.stdout.take() {
            let mut buf = vec![0u8; 1 << 16];
            let t0 = std::time::Instant::now();
            while let Ok(k) = std::io::Read::read(&mut o, &mut buf) {
                if k == 0 || t0.elapsed() > Duration::from_secs(60) {
                    break;
                }
                n += k as u64;
            }
        }
        let _ = child.kill();
        if !child.wait().is_ok_and(|s| s.success()) && n == 0 {
            return Err("Not a compressed file".into());
        }
        n
    };
    Ok(vec![Item { path: PathBuf::from(name), is_dir: false, link: false, size, modified: meta.modified().ok(), dot: false }])
}

/// Decompress a lone compressed file's one file into `dir`.
fn extract_single(archive: &Path, dir: &Path) -> Result<(), String> {
    let name = archive.file_name().map(|n| stem(&n.to_string_lossy()).to_string()).unwrap_or_default();
    let out = std::fs::File::create(dir.join(&name)).map_err(|e| e.to_string())?;
    let mut c = cat(archive)?;
    c.stdin(Stdio::null()).stdout(out).stderr(Stdio::piped());
    let o = c.output().map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).lines().next().unwrap_or("Couldn't decompress it").trim_start_matches("bsdcat: ").to_string())
    }
}

pub fn is_archive(name: &str) -> bool {
    let n = name.to_lowercase();
    EXTS.iter().any(|e| n.ends_with(e) && n.len() > e.len())
}

/// A name without its archive extension ("photos.tar.gz" -> "photos").
pub fn stem(name: &str) -> &str {
    let l = name.to_lowercase();
    EXTS.iter().filter(|e| l.ends_with(*e) && l.len() > e.len()).map(|e| &name[..name.len() - e.len()]).min_by_key(|s| s.len()).unwrap_or(name)
}

pub fn available() -> bool {
    #[cfg(test)]
    if GONE.with(|g| g.get()) {
        return false;
    }
    crate::media::have("bsdtar")
}

#[cfg(test)]
thread_local! {
    /// As if bsdtar were not installed, for this thread's app.
    pub static GONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// What Compress and Extract Here say when bsdtar is missing. They stay in the menus
/// so it is clear the feature exists and what it takes.
pub const COMPRESS_NEEDS: &str = "Compressing needs bsdtar (libarchive-tools or bsdtar)";
pub const EXTRACT_NEEDS: &str = "Extracting needs bsdtar (libarchive-tools or bsdtar)";

/// Errors that a password can fix: asking for one, and one that was refused.
pub const NEEDS_PASSWORD: &str = "Password-protected";
pub const WRONG_PASSWORD: &str = "Wrong password";

pub fn wants_password(err: &str) -> bool {
    err == NEEDS_PASSWORD || err == WRONG_PASSWORD
}

static PASSWORDS: Mutex<Vec<(PathBuf, String)>> = Mutex::new(Vec::new());

/// Use `pw` for `archive` from now on (this session only).
pub fn set_password(archive: &Path, pw: String) {
    let mut g = PASSWORDS.lock().unwrap_or_else(|e| e.into_inner());
    g.retain(|(a, _)| a != archive);
    g.push((archive.to_path_buf(), pw));
}

/// Forget the password given for `archive`, as a new session would.
#[cfg(test)]
pub fn forget_password(archive: &Path) {
    PASSWORDS.lock().unwrap_or_else(|e| e.into_inner()).retain(|(a, _)| a != archive);
}

pub(crate) fn password(archive: &Path) -> Option<String> {
    PASSWORDS.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(a, _)| a == archive).map(|(_, p)| p.clone())
}

/// What bsdtar says when the encryption is one only the 7z tool can open.
pub(crate) const NOT_SUPPORTED: &str = "encrypted, but currently not supported";

/// Given to bsdtar when no password is known: an encrypted entry then fails at once
/// instead of prompting (it would ask over and over, seconds each for AES zips). Not a
/// secret, so the command line is fine.
pub(crate) const NO_PASSWORD: &str = "maia: no password given";

/// Zip and 7z, which the 7z command decrypts quickly (bsdtar takes seconds to refuse a
/// wrong password for an AES zip).
fn seven_kind(archive: &Path) -> bool {
    let n = archive.to_string_lossy().to_lowercase();
    [".zip", ".7z", ".jar", ".apk", ".cbz", ".xpi"].iter().any(|e| n.ends_with(e))
}

pub(crate) fn tool(name: &str) -> Command {
    let mut c = Command::new(name);
    unsafe {
        use std::os::unix::process::CommandExt;
        c.pre_exec(|| {
            // No controlling terminal: a password prompt reads the pipe, never a tty.
            libc::setsid();
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
    c
}

/// (archive, path inside it) when `p` lies inside an archive file. Only called for
/// paths that are not folders on disk.
pub fn split(p: &Path) -> Option<(PathBuf, PathBuf)> {
    let mut cur = p;
    let mut inner = Vec::new();
    loop {
        let name = cur.file_name()?;
        if is_archive(&name.to_string_lossy()) && cur.is_file() {
            let inside: PathBuf = inner.iter().rev().collect();
            return Some((cur.to_path_buf(), inside));
        }
        inner.push(name.to_os_string());
        cur = cur.parent()?;
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// Path inside the archive, without "./" or a trailing slash.
    pub path: PathBuf,
    pub is_dir: bool,
    pub link: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// Stored as "./name" (many zips): extraction must name it that way.
    pub dot: bool,
}

/// `LC_ALL=C bsdtar -tvf` output. Folders that only exist as prefixes of other entries
/// (zip files often have none) are added.
pub fn parse_list(text: &str, now: SystemTime) -> Vec<Item> {
    let mut out: Vec<Item> = vec![];
    for line in text.lines() {
        // perms links owner group size month day time-or-year name
        let mut rest = line;
        let mut f = vec![];
        for _ in 0..8 {
            let t = rest.trim_start();
            let end = t.find(' ').unwrap_or(t.len());
            f.push(&t[..end]);
            rest = &t[end..];
        }
        let Some(name) = rest.strip_prefix(' ') else { continue };
        let (perms, size) = (f[0], f[4].parse::<u64>().unwrap_or(0));
        if perms.len() < 10 {
            continue;
        }
        let link = perms.starts_with('l');
        let name = if link { name.split(" -> ").next().unwrap_or(name) } else { name };
        let name = unescape(name);
        let dot = name.starts_with("./");
        let name = name.trim_start_matches("./").trim_end_matches('/');
        if name.is_empty() || name == "." || leaves_archive(name) {
            continue;
        }
        out.push(Item { path: PathBuf::from(name), is_dir: perms.starts_with('d'), link, size, modified: when(f[5], f[6], f[7], now), dot });
    }
    with_implied(out)
}

/// The listing plus the folders that only exist as prefixes of other entries.
fn with_implied(mut out: Vec<Item>) -> Vec<Item> {
    let have: std::collections::HashSet<PathBuf> = out.iter().map(|i| i.path.clone()).collect();
    let mut implied = std::collections::BTreeSet::new();
    for i in &out {
        let mut p = i.path.parent();
        while let Some(d) = p.filter(|d| !d.as_os_str().is_empty()) {
            if !have.contains(d) {
                implied.insert(d.to_path_buf());
            }
            p = d.parent();
        }
    }
    let dot = out.iter().any(|i| i.dot);
    out.extend(implied.into_iter().map(|path| Item { path, is_dir: true, link: false, size: 0, modified: None, dot }));
    out
}

/// bsdtar prints unprintable bytes as \ooo and a backslash as \\.
fn unescape(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() && b[i + 1] == b'\\' {
            out.push(b'\\');
            i += 2;
        } else if b[i] == b'\\' && i + 3 < b.len() + 0 && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            out.push(((b[i + 1] - b'0') << 6) | ((b[i + 2] - b'0') << 3) | (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// "Oct 14 22:57" (this year, or last if that is in the future) or "Mar 5 2020",
/// in local time.
fn when(month: &str, day: &str, t: &str, now: SystemTime) -> Option<SystemTime> {
    let m = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"].iter().position(|x| *x == month)? as i32;
    let day: i32 = day.parse().ok()?;
    let now_secs = now.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs() as i64;
    let local = |year: i32, hh: i32, mm: i32| -> Option<i64> {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        tm.tm_year = year - 1900;
        tm.tm_mon = m;
        tm.tm_mday = day;
        tm.tm_hour = hh;
        tm.tm_min = mm;
        tm.tm_isdst = -1;
        let s = unsafe { libc::mktime(&mut tm) };
        (s != -1).then_some(s)
    };
    let secs = if let Some((hh, mm)) = t.split_once(':') {
        let (hh, mm) = (hh.parse().ok()?, mm.parse().ok()?);
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let t0 = now_secs as libc::time_t;
        unsafe { libc::localtime_r(&t0, &mut tm) };
        let this = local(tm.tm_year + 1900, hh, mm)?;
        if this > now_secs + 86_400 { local(tm.tm_year + 1899, hh, mm)? } else { this }
    } else {
        local(t.parse().ok()?, 0, 0)?
    };
    (secs >= 0).then(|| SystemTime::UNIX_EPOCH + Duration::from_secs(secs as u64))
}

/// Output of a command, or its error text, with a time limit.
fn run(c: Command, timeout: Duration, pw: Option<&str>) -> Result<Vec<u8>, String> {
    let (ok, out, err) = run_full(c, timeout, pw)?;
    if ok { Ok(out) } else { Err(err) }
}

/// (succeeded, stdout, the useful line of stderr). `pw` is written to the tool's stdin,
/// once: a refused password then meets the end of input instead of asking forever.
fn run_full(mut c: Command, timeout: Duration, pw: Option<&str>) -> Result<(bool, Vec<u8>, String), String> {
    // C.UTF-8: English month names to parse, and any file name intact.
    c.env("LC_ALL", "C.UTF-8").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("{}: {e}", c.get_program().to_string_lossy()))?;
    if let Some(mut i) = child.stdin.take() {
        if let Some(pw) = pw {
            let _ = std::io::Write::write_all(&mut i, format!("{pw}\n").as_bytes());
        }
    }
    let (mut out, mut err) = (child.stdout.take(), child.stderr.take());
    let ro = std::thread::spawn(move || {
        let mut v = vec![];
        if let Some(o) = out.as_mut() {
            let _ = std::io::Read::read_to_end(o, &mut v);
        }
        v
    });
    let re = std::thread::spawn(move || {
        let mut v = vec![];
        if let Some(e) = err.as_mut() {
            let _ = std::io::Read::read_to_end(e, &mut v);
        }
        v
    });
    let t0 = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if t0.elapsed() < timeout => std::thread::sleep(Duration::from_millis(15)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let (o, e) = (ro.join().unwrap_or_default(), re.join().unwrap_or_default());
    let line = said(&String::from_utf8_lossy(&e), pw);
    match status {
        Some(st) => Ok((st.success(), o, line)),
        None => Err("Timed out".into()),
    }
}

pub type Listing = Arc<Vec<Item>>;

/// Everything in an archive, cached until the file changes.
pub fn list(archive: &Path) -> Result<Listing, String> {
    let mtime = std::fs::metadata(archive).and_then(|m| m.modified()).ok();
    if let Some(hit) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|c| c.0 == archive && c.1 == mtime) {
        return Ok(hit.2.clone());
    }
    if single(archive) {
        let items: Listing = Arc::new(list_single(archive)?);
        remember(archive, mtime, items.clone());
        return Ok(items);
    }
    let pw = password(archive);
    let mut c = tool("bsdtar");
    if pw.is_none() {
        c.arg("--passphrase").arg(NO_PASSWORD);
    }
    c.arg("-tvf").arg(archive);
    // One unreadable name makes bsdtar exit with an error after listing the rest: keep them.
    let (ok, out, err) = run_full(c, Duration::from_secs(60), pw.as_deref())?;
    let items: Listing = Arc::new(if !ok && out.is_empty() {
        // 7z with its names encrypted too.
        if !err.contains(NOT_SUPPORTED) {
            return Err(err);
        }
        list7(archive, pw.as_deref())?
    } else {
        parse_list(&String::from_utf8_lossy(&out), SystemTime::now())
    });
    remember(archive, mtime, items.clone());
    Ok(items)
}

/// The useful line of a tool's complaints: a password asked for or refused, else the
/// first thing it said.
pub(crate) fn said(msg: &str, pw: Option<&str>) -> String {
    // A password asked for, or refused: bsdtar says "passphrase", 7z "password".
    let lower = msg.to_lowercase();
    if (lower.contains("passphrase") || lower.contains("password")) && !lower.contains(NOT_SUPPORTED) {
        if pw.is_some_and(|p| !p.is_empty()) { WRONG_PASSWORD.to_string() } else { NEEDS_PASSWORD.to_string() }
    } else {
        // The first complaint says what went wrong ("Error exit delayed…" comes last).
        msg.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("Enter pass")).map(|l| l.replace("Enter passphrase:", "")).next().unwrap_or_else(|| "bsdtar failed".into()).trim_start_matches("bsdtar: ").to_string()
    }
}

type Cached = Vec<(PathBuf, Option<SystemTime>, Listing)>;
static CACHE: Mutex<Cached> = Mutex::new(Vec::new());

fn remember(archive: &Path, mtime: Option<SystemTime>, items: Listing) {
    let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    g.retain(|c| c.0 != archive);
    if g.len() >= 8 {
        g.remove(0);
    }
    g.push((archive.to_path_buf(), mtime, items));
}

/// The listing already read, if any (never runs a tool: for the UI thread).
pub fn cached(archive: &Path) -> Option<Listing> {
    CACHE.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|c| c.0 == archive).map(|c| c.2.clone())
}

/// The entries directly inside `inner` (a folder in the archive), as listing rows whose
/// paths continue the archive's path.
pub fn entries(archive: &Path, inner: &Path) -> Result<Vec<crate::fsx::Entry>, String> {
    let all = list(archive)?;
    if !inner.as_os_str().is_empty() && !all.iter().any(|i| i.path == inner && i.is_dir) {
        return Err(format!("No folder {} in this archive", inner.display()));
    }
    Ok(all
        .iter()
        .filter(|i| i.path.parent().unwrap_or(Path::new("")) == inner)
        .map(|i| {
            let name = i.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let mut e = crate::fsx::Entry::placeholder(archive.join(&i.path), name, i.is_dir);
            e.size = i.size;
            e.modified = i.modified;
            e.is_link = i.link;
            e.mode = if i.is_dir { 0o40755 } else { 0o100644 };
            e
        })
        .collect())
}

pub fn list_async(path: PathBuf, archive: PathBuf, inner: PathBuf, ctx: egui::Context) -> std::sync::mpsc::Receiver<crate::fsx::Listing> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = entries(&archive, &inner);
        let _ = tx.send(crate::fsx::Listing { path, result });
        ctx.request_repaint();
    });
    rx
}

/// Extract `members` (paths inside the archive, folders with their contents) into `dir`.
/// On failure `dir` may hold partial or zero-filled files: callers extract into a
/// staging folder and keep it only on success.
pub fn extract(archive: &Path, members: &[PathBuf], dir: &Path) -> Result<(), String> {
    if single(archive) {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        return extract_single(archive, dir);
    }
    extract_with(archive, members, dir, false, &mut |c, pw| run(c, Duration::from_secs(3600), pw).map(|_| ()))
}

/// `extract`, each tool run by `go` (given the command and the password to feed it);
/// `verbose` has bsdtar name each member as it writes it.
pub(crate) fn extract_with(archive: &Path, members: &[PathBuf], dir: &Path, verbose: bool, go: &mut dyn FnMut(Command, Option<&str>) -> Result<(), String>) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    if single(archive) {
        let name = archive.file_name().map(|n| stem(&n.to_string_lossy()).to_string()).unwrap_or_default();
        let mut c = cat(archive)?;
        c.stdout(std::fs::File::create(dir.join(name)).map_err(|e| e.to_string())?);
        return go(c, None);
    }
    let all = list(archive)?;
    let pw = password(archive);
    if pw.is_some() && seven_kind(archive) && seven().is_ok() {
        return extract7(archive, members, dir, pw.as_deref(), go);
    }
    let mut c = tool("bsdtar");
    c.stdout(Stdio::null());
    if pw.is_none() {
        c.arg("--passphrase").arg(NO_PASSWORD);
    }
    c.arg(if verbose { "-xvf" } else { "-xf" }).arg(archive).arg("-C").arg(dir).arg("--");
    for m in members {
        // A folder pattern takes everything under it; bsdtar matches names as stored.
        let dot = all.iter().find(|i| i.path == *m).is_some_and(|i| i.dot);
        c.arg(if dot { Path::new(".").join(m) } else { m.clone() });
    }
    match go(c, pw.as_deref()) {
        Err(e) if e.contains(NOT_SUPPORTED) => extract7(archive, members, dir, pw.as_deref(), go),
        r => r,
    }
}

/// One member's bytes, read without putting it on the disk (a comic's page, a book's
/// chapter); refused past `cap` bytes.
pub fn read_member(archive: &Path, inner: &Path, cap: usize) -> Result<Vec<u8>, String> {
    let all = list(archive)?;
    let dot = all.iter().find(|i| i.path == inner).is_some_and(|i| i.dot);
    if all.iter().find(|i| i.path == inner).is_some_and(|i| i.size as usize > cap) {
        return Err("Too big to read here".into());
    }
    if single(archive) {
        return run(cat(archive)?, Duration::from_secs(60), None).and_then(|b| if b.len() > cap { Err("Too big to read here".into()) } else { Ok(b) });
    }
    let pw = password(archive);
    let mut c = tool("bsdtar");
    if pw.is_none() {
        c.arg("--passphrase").arg(NO_PASSWORD);
    }
    c.arg("-xOf").arg(archive).arg("--").arg(if dot { Path::new(".").join(inner) } else { inner.to_path_buf() });
    let out = run(c, Duration::from_secs(60), pw.as_deref())?;
    if out.len() > cap {
        return Err("Too big to read here".into());
    }
    Ok(out)
}

/// The 7z command, under whichever name it is installed.
pub(crate) fn seven() -> Result<&'static str, String> {
    #[cfg(test)]
    if NO_SEVEN.with(|n| n.get()) {
        return Err("Encrypted 7z archives need the 7z command".into());
    }
    ["7z", "7zz", "7za"].into_iter().find(|t| crate::media::have(t)).ok_or_else(|| "Encrypted 7z archives need the 7z command".to_string())
}

#[cfg(test)]
thread_local! {
    /// Tests on this thread see no 7z command, as on a system without it.
    pub static NO_SEVEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Failures of the 7z tool, which only runs for encrypted archives: without a password
/// any failure means it wants one.
fn seven_err(e: String, pw: Option<&str>) -> String {
    if pw.is_none() { NEEDS_PASSWORD.into() } else { e }
}

/// An encrypted 7z's listing, from `7z l -slt`.
fn list7(archive: &Path, pw: Option<&str>) -> Result<Vec<Item>, String> {
    let mut c = tool(seven()?);
    c.args(["l", "-slt", "-ba", "--"]).arg(archive);
    let out = run(c, Duration::from_secs(60), pw).map_err(|e| seven_err(e, pw))?;
    Ok(parse_slt(&String::from_utf8_lossy(&out)))
}

/// A member name that, joined to the archive's own path, would point outside it: one
/// going up a level, or one starting at the root ("/etc/passwd" in a tar names the real
/// file). Such members aren't listed.
fn leaves_archive(name: &str) -> bool {
    use std::path::Component;
    Path::new(name).components().any(|c| matches!(c, Component::RootDir | Component::ParentDir))
}

/// `7z l -slt` blocks ("Path = a/b.txt", "Size = 6", "Modified = 2026-10-15 19:19:57.79",
/// "Attributes = D drwxr-xr-x") as listing items.
pub fn parse_slt(text: &str) -> Vec<Item> {
    let mut out = vec![];
    for block in text.split("\n\n") {
        let field = |k: &str| block.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix(" = "));
        let Some(path) = field("Path").filter(|p| !p.is_empty() && !leaves_archive(p)) else { continue };
        let attrs = field("Attributes").unwrap_or("");
        let unix = attrs.split_whitespace().nth(1).unwrap_or("");
        out.push(Item {
            path: PathBuf::from(path.trim_end_matches('/')),
            is_dir: attrs.starts_with('D') || unix.starts_with('d'),
            link: unix.starts_with('l'),
            size: field("Size").and_then(|s| s.parse().ok()).unwrap_or(0),
            modified: field("Modified").and_then(crate::timefmt::parse_local),
            dot: false,
        });
    }
    with_implied(out)
}

fn extract7(archive: &Path, members: &[PathBuf], dir: &Path, pw: Option<&str>, go: &mut dyn FnMut(Command, Option<&str>) -> Result<(), String>) -> Result<(), String> {
    let mut c = tool(seven()?);
    let mut o = std::ffi::OsString::from("-o");
    o.push(dir);
    c.stdout(Stdio::null()).args(["x", "-bso0", "-bsp0", "-y"]).arg(o).arg("--").arg(archive).args(members);
    go(c, pw).map_err(|e| seven_err(e, pw))
}

/// Where a member is extracted to be opened (one folder per archive and version). An
/// archive changed in place twice in one second is two versions: nanoseconds and size.
pub fn open_cache(archive: &Path) -> PathBuf {
    let m = std::fs::metadata(archive).ok();
    let mtime = m.as_ref().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0);
    let size = m.map(|m| m.len()).unwrap_or(0);
    let key = crate::thumbcache::md5_hex(format!("{}\0{mtime}\0{size}", archive.display()).as_bytes());
    // Tests run outside the sandbox too: never the real cache.
    let base = if cfg!(test) { None } else { dirs::cache_dir() };
    base.unwrap_or_else(std::env::temp_dir).join("maia/archives").join(key)
}

/// Extract one file to the cache and return where it landed. It goes through a staging
/// folder, so a refused password never leaves a zero-filled file to be opened later.
pub fn extract_for_open(archive: &Path, inner: &Path) -> Result<PathBuf, String> {
    let dir = open_cache(archive);
    let dest = dir.join(inner);
    if !dest.exists() {
        let stage = dir.join(format!(".part-{}-{:?}", std::process::id(), std::thread::current().id()));
        let r = extract(archive, &[inner.to_path_buf()], &stage).and_then(|_| {
            std::fs::create_dir_all(dest.parent().unwrap_or(&dir)).map_err(|e| e.to_string())?;
            std::fs::rename(stage.join(inner), &dest).map_err(|e| e.to_string())
        });
        let _ = std::fs::remove_dir_all(&stage);
        r?;
    }
    dest.exists().then_some(dest).ok_or_else(|| "Couldn't extract it".into())
}

/// A password a job carries: never printed, not even by Debug.
#[derive(Clone)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(…)")
    }
}

/// Whether a 7z archive can be made with a password (the 7z command is installed).
pub fn can_lock_7z() -> bool {
    seven().is_ok()
}

#[cfg(test)]
/// Compress `names` (inside `parent`) into `out` locked with `pw`: a zip by bsdtar with
/// AES-256, a 7z by the 7z command with the names encrypted too. Both read the password
/// from their input, never the command line; 7z asks for it twice. Once made, the
/// archive opens in Maia with it for the rest of the session.
pub fn create_locked(out: &Path, parent: &Path, names: &[std::ffi::OsString], pw: &str) -> Result<(), String> {
    if pw.is_empty() || pw.contains(['\n', '\r']) {
        return Err("Type a password on one line".into());
    }
    let tmp = out.with_file_name(format!(".{}.part", out.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
    let r = if format_for(out) == "7zip" {
        let mut c = tool(seven()?);
        // The part file's name says nothing of its kind: -t7z does.
        c.current_dir(parent).args(["a", "-bso0", "-bsp0", "-y", "-t7z", "-mhe=on", "-p", "--"]).arg(&tmp).args(names);
        run(c, Duration::from_secs(3600), Some(&format!("{pw}\n{pw}")))
    } else {
        let mut c = tool("bsdtar");
        c.arg("-cf").arg(&tmp).args(["--format", "zip", "--options", "zip:encryption=aes256"]).arg("-C").arg(parent).arg("--").args(names);
        run(c, Duration::from_secs(3600), Some(pw))
    };
    match r.and_then(|_| std::fs::rename(&tmp, out).map_err(|e| e.to_string())) {
        Ok(()) => {
            set_password(out, pw.to_string());
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
/// Compress `names` (inside `parent`) into `out`; the format follows its extension.
pub fn create(out: &Path, parent: &Path, names: &[std::ffi::OsString]) -> Result<(), String> {
    let tmp = out.with_file_name(format!(".{}.part", out.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
    let mut c = tool("bsdtar");
    c.arg("-cf").arg(&tmp).arg("--format").arg(format_for(out)).args(filter_for(out)).arg("-C").arg(parent).arg("--").args(names);
    let r = run(c, Duration::from_secs(3600), None);
    match r {
        Ok(_) => std::fs::rename(&tmp, out).map_err(|e| e.to_string()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

pub(crate) fn format_for(out: &Path) -> &'static str {
    let n = out.to_string_lossy().to_lowercase();
    if [".zip", ".cbz"].iter().any(|e| n.ends_with(e)) {
        "zip"
    } else if n.ends_with(".7z") {
        "7zip"
    } else {
        "pax"
    }
}

pub(crate) fn filter_for(out: &Path) -> Vec<&'static str> {
    let n = out.to_string_lossy().to_lowercase();
    if n.ends_with(".gz") || n.ends_with(".tgz") {
        vec!["-z"]
    } else if n.ends_with(".xz") || n.ends_with(".txz") {
        vec!["-J"]
    } else if n.ends_with(".bz2") || n.ends_with(".tbz2") || n.ends_with(".tbz") {
        vec!["-j"]
    } else if n.ends_with(".zst") || n.ends_with(".tzst") {
        vec!["--zstd"]
    } else {
        vec![]
    }
}

/// Members grouped as the job needs them: archive -> paths inside it.
pub fn group(paths: &[PathBuf]) -> HashMap<PathBuf, Vec<PathBuf>> {
    let mut m: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    for p in paths {
        if let Some((a, inner)) = split(p) {
            m.entry(a).or_default().push(inner);
        }
    }
    m
}
