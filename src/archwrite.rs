//! Writing archives: compressing, and changes made inside one (files added, members
//! deleted or renamed, new folders). Each runs as a job whose tool is watched: what it
//! has read so far (/proc/PID/io) moves the card, Pause stops it where it is, Cancel
//! kills it, and nothing it was writing is left behind.
//!
//! An archive is never changed where it lies. The change is made to a copy beside it
//! (zip and 7z, by the 7z command) or to a new archive built from its contents (tar and
//! its compressions, or when 7z is missing), and that is swapped in once whole: the old
//! one goes to the trash, which is what Undo brings back. A locked archive stays locked
//! the way it was, with the password it was opened with.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::archive::{self, NEEDS_PASSWORD, WRONG_PASSWORD};
use crate::ops::{Gate, Progress};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Zip,
    TarGz,
    TarXz,
    SevenZ,
}

impl Format {
    pub const ALL: [Format; 4] = [Format::Zip, Format::TarGz, Format::TarXz, Format::SevenZ];

    pub fn ext(self) -> &'static str {
        match self {
            Format::Zip => ".zip",
            Format::TarGz => ".tar.gz",
            Format::TarXz => ".tar.xz",
            Format::SevenZ => ".7z",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Zip => "Zip",
            Format::TarGz => "Tar.gz",
            Format::TarXz => "Tar.xz",
            Format::SevenZ => "7-Zip",
        }
    }

    /// The format a name asks for by its extension.
    pub fn of(name: &str) -> Option<Format> {
        let n = name.to_lowercase();
        Format::ALL.into_iter().find(|f| n.ends_with(f.ext()) && n.len() > f.ext().len())
    }

    /// Zip and 7z take a password; a tarball has none to take.
    pub fn lockable(self) -> bool {
        matches!(self, Format::Zip | Format::SevenZ)
    }
}

/// How hard the compressor works: three steps, as a number field would only ask what
/// the numbers mean.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Level {
    Fast,
    #[default]
    Normal,
    Smallest,
}

impl Level {
    pub const ALL: [Level; 3] = [Level::Fast, Level::Normal, Level::Smallest];

    pub fn label(self) -> &'static str {
        match self {
            Level::Fast => "Fast",
            Level::Normal => "Normal",
            Level::Smallest => "Smallest",
        }
    }

    /// The tools' own level: zstd counts to 19 and starts its default at 3; the rest count
    /// to 9.
    fn n(self, zstd: bool) -> u8 {
        match (self, zstd) {
            (Level::Fast, _) => 1,
            (Level::Normal, true) => 3,
            (Level::Normal, false) => 6,
            (Level::Smallest, true) => 19,
            (Level::Smallest, false) => 9,
        }
    }
}

/// The level last chosen, offered again for the rest of the session.
pub static LAST_LEVEL: Mutex<Level> = Mutex::new(Level::Normal);

pub fn last_level() -> Level {
    *LAST_LEVEL.lock().unwrap_or_else(|e| e.into_inner())
}

/// A change inside an archive. Paths are inside it, without the archive's own.
#[derive(Clone, Debug)]
pub enum Edit {
    /// Files from the disk into the folder `into`. A name already there is replaced (a
    /// folder merged into), or, unless `replace`, the newcomer takes a free name.
    Add { srcs: Vec<PathBuf>, into: PathBuf, replace: bool },
    /// Members deleted, folders with what is in them.
    Delete { members: Vec<PathBuf> },
    /// Each member given a new name in its folder.
    Rename { pairs: Vec<(PathBuf, String)> },
    /// A new empty folder, or file, at `at`.
    Make { at: PathBuf, folder: bool },
}

impl Edit {
    pub fn count(&self) -> usize {
        match self {
            Edit::Add { srcs, .. } => srcs.len(),
            Edit::Delete { members } => members.len(),
            Edit::Rename { pairs } => pairs.len(),
            Edit::Make { .. } => 1,
        }
    }
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The card's title: "Copying 2 items into photos.zip", "Renaming a.txt to b.txt in …".
pub fn label(archive: &Path, e: &Edit) -> String {
    let a = name_of(archive);
    let what = |v: &[PathBuf]| match v {
        [one] => name_of(one),
        _ => format!("{} items", v.len()),
    };
    match e {
        Edit::Add { srcs, .. } => format!("Copying {} into {a}", what(srcs)),
        Edit::Delete { members } => format!("Deleting {} from {a}", what(members)),
        Edit::Rename { pairs } => match pairs.as_slice() {
            [(from, to)] => format!("Renaming {} to {to} in {a}", name_of(from)),
            v => format!("Renaming {} items in {a}", v.len()),
        },
        Edit::Make { at, .. } => format!("Creating {} in {a}", name_of(at)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Zip,
    Seven,
    Tar,
}

const TARS: [&str; 10] = [".tar", ".tar.gz", ".tgz", ".tar.bz2", ".tbz2", ".tbz", ".tar.xz", ".txz", ".tar.zst", ".tzst"];

/// Whether Maia can change this kind of archive, by its name alone (no disk, so the UI
/// thread may ask); why not, when it can't.
pub fn kind(archive: &Path) -> Result<Kind, String> {
    let name = name_of(archive);
    let l = name.to_lowercase();
    if !archive::available() {
        return Err("Changing archives needs bsdtar".into());
    }
    if [".zip", ".cbz"].iter().any(|e| l.ends_with(e)) {
        return Ok(Kind::Zip);
    }
    if l.ends_with(".7z") {
        return Ok(Kind::Seven);
    }
    if TARS.iter().any(|e| l.ends_with(e)) {
        return Ok(Kind::Tar);
    }
    if archive::single(archive) {
        return Err(format!("“{name}” is one compressed file: extract it to change it"));
    }
    let ext = l.rsplit_once('.').map(|x| x.1.to_uppercase()).unwrap_or_default();
    Err(format!("{ext} archives can only be read here: extract it to change it"))
}

/// `kind`, and that the archive and its folder can be written.
fn writable(archive: &Path) -> Result<Kind, String> {
    let k = kind(archive)?;
    let ok = |p: &Path| std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(p.as_os_str())).is_ok_and(|c| unsafe { libc::access(c.as_ptr(), libc::W_OK) } == 0);
    let name = name_of(archive);
    if !ok(archive.parent().unwrap_or(Path::new("/"))) {
        return Err(format!("“{name}” is in a folder you can't change"));
    }
    if !ok(archive) {
        return Err(format!("“{name}” is read-only"));
    }
    Ok(k)
}

/// How an archive is locked, as a change must lock what it adds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lock {
    Open,
    ZipCrypto,
    ZipAes,
    /// A 7z with its contents locked, and with its names too when `names`.
    Seven { names: bool },
}

/// A zip's lock, read from its central directory: (any entry locked, with AES, the
/// smallest locked file to try a password on).
pub fn zip_lock(path: &Path) -> Option<(bool, bool, Option<String>)> {
    use std::os::unix::fs::FileExt;
    let f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let n = len.min(65_557 + 20) as usize;
    let mut tail = vec![0u8; n];
    f.read_exact_at(&mut tail, len - n as u64).ok()?;
    let le = |b: &[u8], i: usize, w: usize| -> u64 { b.get(i..i + w).map_or(0, |s| s.iter().rev().fold(0u64, |a, &x| a << 8 | x as u64)) };
    let at = (0..n.saturating_sub(21)).rev().find(|&i| tail[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])?;
    let (mut size, mut off) = (le(&tail, at + 12, 4), le(&tail, at + 16, 4));
    if (size == 0xFFFF_FFFF || off == 0xFFFF_FFFF) && at >= 20 && tail[at - 20..at - 16] == [0x50, 0x4b, 0x06, 0x07] {
        let mut z = [0u8; 56];
        f.read_exact_at(&mut z, le(&tail, at - 12, 8)).ok()?;
        if z[..4] != [0x50, 0x4b, 0x06, 0x06] {
            return None;
        }
        (size, off) = (le(&z, 40, 8), le(&z, 48, 8));
    }
    if size > 256 << 20 {
        return None;
    }
    let mut cd = vec![0u8; size as usize];
    f.read_exact_at(&mut cd, off).ok()?;
    let (mut locked, mut aes, mut probe): (bool, bool, Option<(String, u64)>) = (false, false, None);
    let mut i = 0;
    while i + 46 <= cd.len() && cd[i..i + 4] == [0x50, 0x4b, 0x01, 0x02] {
        let (flags, method, usize_) = (le(&cd, i + 8, 2), le(&cd, i + 10, 2), le(&cd, i + 24, 4));
        let (nl, xl, cl) = (le(&cd, i + 28, 2) as usize, le(&cd, i + 30, 2) as usize, le(&cd, i + 32, 2) as usize);
        let name = String::from_utf8_lossy(cd.get(i + 46..i + 46 + nl)?).into_owned();
        if flags & 1 == 1 && !name.ends_with('/') {
            locked = true;
            aes |= method == 99;
            if probe.as_ref().is_none_or(|p| usize_ < p.1) {
                probe = Some((name, usize_));
            }
        }
        i += 46 + nl + xl + cl;
    }
    Some((locked, aes, probe.map(|p| p.0)))
}

/// `7z l -slt` blocks: whether any file is locked, and the smallest one.
fn seven_probe(text: &str) -> (bool, Option<String>) {
    let mut best: Option<(String, u64)> = None;
    let mut any = false;
    for block in text.split("\n\n") {
        let field = |k: &str| block.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix(" = "));
        let dir = field("Attributes").is_some_and(|a| a.starts_with('D')) || field("Folder") == Some("+");
        if field("Encrypted") == Some("+") && !dir {
            any = true;
            let (path, size) = (field("Path").unwrap_or("").to_string(), field("Size").and_then(|s| s.parse().ok()).unwrap_or(0));
            if !path.is_empty() && best.as_ref().is_none_or(|b| size < b.1) {
                best = Some((path, size));
            }
        }
    }
    (any, best.map(|b| b.0))
}

/// How `archive` is locked, and a locked file to try the password on. A 7z whose names
/// are locked lists only with the right password, which is checked here.
fn lock_of(archive: &Path, kind: Kind, pw: Option<&str>) -> Result<(Lock, Option<String>), String> {
    match kind {
        Kind::Tar => Ok((Lock::Open, None)),
        Kind::Zip => Ok(match zip_lock(archive) {
            Some((true, aes, probe)) => (if aes { Lock::ZipAes } else { Lock::ZipCrypto }, probe),
            _ => (Lock::Open, None),
        }),
        Kind::Seven => {
            // Without the 7z command nothing locked can be read either: the rebuild says so.
            let Ok(t) = archive::seven() else { return Ok((Lock::Open, None)) };
            let mut c = archive::tool(t);
            c.args(["l", "-slt", "-ba", "-t7z", "--"]).arg(archive);
            if let Ok(o) = quiet(c, None) {
                let (any, probe) = seven_probe(&o);
                return Ok((if any { Lock::Seven { names: false } } else { Lock::Open }, probe));
            }
            let Some(pw) = pw else { return Err(NEEDS_PASSWORD.into()) };
            let mut c = archive::tool(t);
            c.args(["l", "-slt", "-ba", "-t7z", "--"]).arg(archive);
            quiet(c, Some(pw)).map_err(|_| WRONG_PASSWORD.to_string())?;
            Ok((Lock::Seven { names: true }, None))
        }
    }
}

/// A short tool run, its output or its failure: the password, if any, fed as often as it
/// is asked for (7z asks twice to lock what it writes).
fn quiet(mut c: Command, pw: Option<&str>) -> Result<String, String> {
    c.env("LC_ALL", "C.UTF-8").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| e.to_string())?;
    if let (Some(mut i), Some(pw)) = (child.stdin.take(), pw) {
        let _ = i.write_all(format!("{pw}\n").repeat(4).as_bytes());
    }
    let o = child.wait_with_output().map_err(|e| e.to_string())?;
    if o.status.success() { Ok(String::from_utf8_lossy(&o.stdout).into_owned()) } else { Err(archive::said(&unprompted(&String::from_utf8_lossy(&o.stderr)), pw)) }
}

/// A tool's complaints without its password prompts, which name the password whatever
/// went wrong.
fn unprompted(s: &str) -> String {
    let mut s = s.to_string();
    for p in ["Enter passphrase:", "Enter password (will not be echoed):", "Verify password (will not be echoed) :", "Enter password:"] {
        s = s.replace(p, "");
    }
    s
}

pub const CANCELLED: &str = "Cancelled";

/// Bytes a process has read so far, all its threads together.
fn read_so_far(pid: libc::pid_t) -> Option<u64> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/io")).ok()?;
    s.lines().find_map(|l| l.strip_prefix("rchar: "))?.trim().parse().ok()
}

/// Run a tool for a job. `feed` goes to its input (passwords); what it reads counts from
/// `base` into the job's done bytes, up to `span` of them; the name it says it is at
/// (bsdtar -v: "a docs/notes.txt") shows under the bar. Paused, its process group is
/// stopped; cancelled, killed.
pub(crate) fn watch(mut c: Command, feed: Option<String>, p: &Mutex<Progress>, gate: &Gate, ctx: &egui::Context, base: u64, span: u64) -> Result<(), String> {
    let lock = || p.lock().unwrap_or_else(|e| e.into_inner());
    c.env("LC_ALL", "C.UTF-8").stdin(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("{}: {e}", c.get_program().to_string_lossy()))?;
    if let Some(mut i) = child.stdin.take() {
        if let Some(f) = &feed {
            let _ = i.write_all(f.as_bytes());
        }
    }
    let pid = child.id() as libc::pid_t;
    let err = child.stderr.take();
    std::thread::scope(|sc| {
        let said = sc.spawn(|| {
            let (mut errs, mut line) = (String::new(), Vec::<u8>::new());
            let Some(mut e) = err else { return errs };
            let mut buf = [0u8; 8192];
            let at = |l: &[u8]| (l.starts_with(b"a ") || l.starts_with(b"x ")).then(|| String::from_utf8_lossy(&l[2..]).into_owned());
            while let Ok(n) = e.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let mut last = None;
                for &b in &buf[..n] {
                    if b != b'\n' {
                        line.push(b);
                        continue;
                    }
                    // A member that failed ends its line with why: "x notes.txt: Incorrect
                    // passphrase". Kept, without the "x ", for the error said at the end.
                    let name = at(&line);
                    if (name.is_none() || name.as_ref().is_some_and(|n| n.contains(": "))) && errs.len() < 64 << 10 {
                        errs.push_str(&String::from_utf8_lossy(if name.is_some() { &line[2..] } else { &line }));
                        errs.push('\n');
                    }
                    if name.is_some() {
                        last = name;
                    }
                    line.clear();
                }
                // The line begun and not yet ended is the item being written now.
                if let Some(name) = at(&line).or(last) {
                    lock().current = name;
                }
            }
            errs.push_str(&String::from_utf8_lossy(&line));
            errs
        });
        let (mut stopped, mut shown) = (false, Instant::now());
        let status = loop {
            match child.try_wait() {
                Ok(Some(st)) => break Ok(st),
                Ok(None) => {}
                Err(e) => break Err(e.to_string()),
            }
            if gate.cancelled() {
                unsafe { libc::kill(-pid, libc::SIGKILL) };
                let _ = child.kill();
                let _ = child.wait();
                break Err(CANCELLED.to_string());
            }
            if gate.paused() != stopped {
                stopped = !stopped;
                unsafe { libc::kill(-pid, if stopped { libc::SIGSTOP } else { libc::SIGCONT }) };
            }
            if let Some(n) = read_so_far(pid) {
                let mut g = lock();
                g.done_bytes = g.done_bytes.max(base + n.min(span));
            }
            if shown.elapsed() > Duration::from_millis(200) {
                ctx.request_repaint();
                shown = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(30));
        };
        let errs = said.join().unwrap_or_default();
        let st = status?;
        if st.success() {
            let mut g = lock();
            g.done_bytes = g.done_bytes.max(base + span);
            return Ok(());
        }
        Err(archive::said(&unprompted(&errs), feed.as_deref()))
    })
}

/// A hidden name beside `p` for work in progress: ".photos.zip.part".
fn beside(p: &Path, what: &str) -> PathBuf {
    let dir = p.parent().unwrap_or(Path::new("/"));
    crate::fsx::unique_dest(dir, &format!(".{}.{what}", name_of(p)))
}

/// `src` laid out at `dst` without copying where the disk allows: folders made, files
/// hard-linked (copied where a link is refused), links made again, special files left
/// out. With `merge`, into what is at `dst` already, replacing files.
fn link_tree(src: &Path, dst: &Path, merge: bool) -> std::io::Result<()> {
    let m = std::fs::symlink_metadata(src)?;
    let there = std::fs::symlink_metadata(dst).ok();
    if merge && there.as_ref().is_some_and(|t| !(t.is_dir() && m.is_dir())) {
        crate::ops::remove_tree(dst)?;
    }
    if m.file_type().is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(src)?, dst)
    } else if m.is_dir() {
        if !(merge && dst.is_dir()) {
            std::fs::create_dir(dst)?;
        }
        for e in std::fs::read_dir(src)? {
            let e = e?;
            link_tree(&e.path(), &dst.join(e.file_name()), merge)?;
        }
        let _ = std::fs::set_permissions(dst, m.permissions());
        if let (Ok(t), Ok(d)) = (m.modified(), std::fs::File::open(dst)) {
            let _ = d.set_modified(t);
        }
        Ok(())
    } else if m.is_file() {
        std::fs::hard_link(src, dst).or_else(|_| std::fs::copy(src, dst).map(|_| ()))
    } else {
        Ok(())
    }
}

/// A staging folder removed, read-only folders inside it (from a tarball) included.
fn clear(dir: &Path) {
    if std::fs::remove_dir_all(dir).is_ok() || !dir.exists() {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
        if let Ok(rd) = std::fs::read_dir(&d) {
            stack.extend(rd.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()));
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// The command that packs `names` (inside `base`) into `tmp`, in the format `out`'s name
/// asks for, at `level`, locked as `lock` says; and what to feed it.
fn pack(out: &Path, tmp: &Path, base: &Path, names: &[OsString], level: Level, lock: Lock, pw: Option<&str>) -> Result<(Command, Option<String>), String> {
    let format = archive::format_for(out);
    if let (Lock::Seven { names: hidden }, Some(pw)) = (lock, pw) {
        let mut c = archive::tool(archive::seven()?);
        // The part file's name says nothing of its kind: -t7z does.
        c.current_dir(base).stdout(Stdio::null()).args(["a", "-bso0", "-bsp0", "-y", "-t7z", "-snl", LITERAL, &format!("-mx={}", level.n(false)), "-p"]);
        if hidden {
            c.arg("-mhe=on");
        }
        c.arg("--").arg(tmp).args(names);
        return Ok((c, Some(format!("{pw}\n").repeat(4))));
    }
    let filter = archive::filter_for(out);
    let mut opts = vec![];
    let n = level.n(filter.contains(&"--zstd"));
    match (format, filter.first().copied()) {
        ("zip", _) => opts.push(format!("zip:compression-level={n}")),
        ("7zip", _) => opts.push(format!("7zip:compression-level={n}")),
        (_, Some("-z")) => opts.push(format!("gzip:compression-level={n}")),
        (_, Some("-J")) => opts.push(format!("xz:compression-level={n}")),
        (_, Some("-j")) => opts.push(format!("bzip2:compression-level={n}")),
        (_, Some("--zstd")) => opts.push(format!("zstd:compression-level={n}")),
        _ => {}
    }
    let feed = match (lock, pw) {
        (Lock::ZipAes, Some(pw)) => {
            opts.push("zip:encryption=aes256".into());
            Some(format!("{pw}\n"))
        }
        (Lock::ZipCrypto, Some(pw)) => {
            opts.push("zip:encryption=zipcrypt".into());
            Some(format!("{pw}\n"))
        }
        _ => None,
    };
    let mut c = archive::tool("bsdtar");
    c.stdout(Stdio::null()).arg("-cvf").arg(tmp).arg("--format").arg(format).args(filter);
    if !opts.is_empty() {
        c.arg("--options").arg(opts.join(","));
    }
    if names.is_empty() {
        // Everything deleted: an empty archive, still of its kind.
        c.args(["-T", "/dev/null"]);
    } else {
        c.arg("-C").arg(base).arg("--").args(names);
    }
    Ok((c, feed))
}

/// Compress `srcs` into `out` at `level`, locked with `pw` when given (a zip with
/// AES-256, a 7z with its names locked too). Sources from several folders are gathered
/// under one as links first. Once made, a locked archive opens with its password for
/// the rest of the session.
pub fn compress(out: &Path, srcs: &[PathBuf], level: Level, pw: Option<&str>, p: &Mutex<Progress>, gate: &Gate, ctx: &egui::Context) -> Result<(), String> {
    if pw.is_some_and(|pw| pw.is_empty() || pw.contains(['\n', '\r'])) {
        return Err("Type a password on one line".into());
    }
    let total: u64 = srcs.iter().map(|s| crate::ops::tree_size(s)).sum();
    p.lock().unwrap_or_else(|e| e.into_inner()).total_bytes = total;
    let names: Vec<OsString> = srcs.iter().filter_map(|s| s.file_name().map(|n| n.to_os_string())).collect();
    let mut parents: Vec<&Path> = srcs.iter().filter_map(|s| s.parent()).collect();
    parents.sort();
    parents.dedup();
    let tmp = beside(out, "part");
    let mut stage = None;
    let r = (|| {
        let base = match parents.as_slice() {
            [one] => one.to_path_buf(),
            _ => {
                let st = beside(out, "gather");
                std::fs::create_dir(&st).map_err(|e| e.to_string())?;
                stage = Some(st.clone());
                for s in srcs {
                    link_tree(s, &st.join(name_of(s)), false).map_err(|e| format!("{}: {e}", s.display()))?;
                }
                st
            }
        };
        let lock = match (pw, Format::of(&name_of(out))) {
            (None, _) => Lock::Open,
            (Some(_), Some(Format::SevenZ)) => Lock::Seven { names: true },
            (Some(_), _) => Lock::ZipAes,
        };
        let (c, feed) = pack(out, &tmp, &base, &names, level, lock, pw)?;
        watch(c, feed, p, gate, ctx, 0, total)?;
        std::fs::rename(&tmp, out).map_err(|e| e.to_string())
    })();
    if let Some(st) = stage {
        clear(&st);
    }
    match r {
        Ok(()) => {
            if let Some(pw) = pw {
                archive::set_password(out, pw.to_string());
            }
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Extract `members` (all, when none) of `archive` into `dir` for a job: its reading
/// counted against the archive's size, Pause and Cancel heeded.
pub fn extract(archive: &Path, members: &[PathBuf], dir: &Path, p: &Mutex<Progress>, gate: &Gate, ctx: &egui::Context) -> Result<(), String> {
    let size = std::fs::metadata(archive).map(|m| m.len()).unwrap_or(0);
    {
        let mut g = p.lock().unwrap_or_else(|e| e.into_inner());
        g.total_bytes += size;
    }
    let base = p.lock().unwrap_or_else(|e| e.into_inner()).done_bytes;
    archive::extract_with(archive, members, dir, true, &mut |c, pw| watch(c, pw.map(|p| format!("{p}\n")), p, gate, ctx, base, size))
}

/// The members `e` works on that aren't there, or names it would take that are.
fn check(e: &Edit, all: &[archive::Item]) -> Result<(), String> {
    let has = |p: &Path| all.iter().any(|i| i.path == p);
    match e {
        Edit::Delete { members } => members.iter().find(|m| !has(m)).map_or(Ok(()), |m| Err(format!("{} isn't in it", m.display()))),
        Edit::Rename { pairs } => {
            for (from, to) in pairs {
                if !has(from) {
                    return Err(format!("{} isn't in it", from.display()));
                }
                if to.is_empty() || to.contains('/') || to == "." || to == ".." {
                    return Err(format!("“{to}” can't be a name"));
                }
                if has(&from.with_file_name(to)) {
                    return Err(format!("There is a “{to}” there already"));
                }
            }
            Ok(())
        }
        Edit::Make { at, .. } if has(at) => Err(format!("There is a “{}” there already", name_of(at))),
        Edit::Add { into, .. } if !into.as_os_str().is_empty() && !all.iter().any(|i| i.path == *into && i.is_dir) => Err(format!("No folder {} in it", into.display())),
        _ => Ok(()),
    }
}

/// A free name in `dir` (inside the archive) for `name`: "notes (1).txt".
fn free_name(all: &[archive::Item], dir: &Path, name: &str, also: &[String]) -> String {
    let taken = |n: &str| all.iter().any(|i| i.path == dir.join(n)) || also.iter().any(|a| a == n);
    if !taken(name) {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.').filter(|&i| i > 0) {
        Some(i) => (&name[..i], &name[i..]),
        None => (name, ""),
    };
    (1..).map(|k| format!("{stem} ({k}){ext}")).find(|n| !taken(n)).unwrap_or_else(|| name.to_string())
}

/// A rename's moves for 7z: the member and everything under it.
fn rename_moves(all: &[archive::Item], pairs: &[(PathBuf, String)]) -> Vec<(PathBuf, PathBuf)> {
    let mut v = vec![];
    for (from, to) in pairs {
        let to = from.with_file_name(to);
        for i in all.iter().filter(|i| i.path.starts_with(from)) {
            let rest = i.path.strip_prefix(from).unwrap_or(Path::new(""));
            v.push((i.path.clone(), if rest.as_os_str().is_empty() { to.clone() } else { to.join(rest) }));
        }
    }
    v
}

/// Lay the files an Add or Make brings in out under `stage`, at their places inside the
/// archive; the top names to hand the tool (relative to `stage`).
fn lay_out(e: &Edit, stage: &Path, all: &[archive::Item], merge: bool) -> Result<(), String> {
    match e {
        Edit::Add { srcs, into, replace } => {
            let place = stage.join(into);
            std::fs::create_dir_all(&place).map_err(|e| e.to_string())?;
            let mut used = vec![];
            for s in srcs {
                let n = name_of(s);
                let n = if *replace { n } else { free_name(all, into, &n, &used) };
                link_tree(s, &place.join(&n), merge || *replace).map_err(|e| format!("{}: {e}", s.display()))?;
                used.push(n);
            }
            Ok(())
        }
        Edit::Make { at, folder } => {
            let p = stage.join(at);
            std::fs::create_dir_all(p.parent().unwrap_or(stage)).map_err(|e| e.to_string())?;
            if *folder { std::fs::create_dir(&p) } else { std::fs::File::create_new(&p).map(|_| ()) }.map_err(|e| e.to_string())
        }
        _ => Ok(()),
    }
}

/// The names directly in `dir`.
fn tops(dir: &Path) -> Vec<OsString> {
    let mut v: Vec<OsString> = std::fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.file_name()).collect()).unwrap_or_default();
    v.sort();
    v
}

/// What says the 7z command can't do this one in place (a rename in a zip written with
/// data descriptors, as bsdtar writes them): built anew instead.
/// 7z treats `*` and `?` in names as wildcards unless told otherwise, so a member called
/// "draft?.txt" couldn't be added, deleted or renamed on its own.
const LITERAL: &str = "-spd";

const NOT_IN_PLACE: &str = "maia: not in place";

/// Change a zip or 7z with the 7z command, on a copy at `part`.
#[allow(clippy::too_many_arguments)]
fn in_place(t: &str, archive: &Path, part: &Path, kind: Kind, e: &Edit, lock: (Lock, Option<String>), pw: Option<&str>, all: &[archive::Item], p: &Mutex<Progress>, gate: &Gate, ctx: &egui::Context) -> Result<(), String> {
    let ty = if kind == Kind::Zip { "-tzip" } else { "-t7z" };
    // 7-Zip 26 renames a ZipCrypto member without its data descriptor flag, and the
    // password no longer opens it: built anew instead.
    if matches!(e, Edit::Rename { .. }) && lock.0 == Lock::ZipCrypto {
        return Err(NOT_IN_PLACE.into());
    }
    let feed = pw.filter(|_| lock.0 != Lock::Open).map(|pw| format!("{pw}\n").repeat(4));
    // A wrong password would lock what is added with it: it is tried on one file first.
    if let (Some(probe), Some(_)) = (&lock.1, &feed) {
        let mut c = archive::tool(t);
        c.stdout(Stdio::null()).args(["t", "-bso0", "-bsp0", ty, "--"]).arg(archive).arg(probe);
        quiet(c, pw).map_err(|e| if archive::wants_password(&e) { WRONG_PASSWORD.to_string() } else { e })?;
    }
    let size = std::fs::metadata(archive).map(|m| m.len()).unwrap_or(0);
    let added: u64 = match e {
        Edit::Add { srcs, .. } => srcs.iter().map(|s| crate::ops::tree_size(s)).sum(),
        _ => 0,
    };
    p.lock().unwrap_or_else(|e| e.into_inner()).total_bytes = size * 2 + added;
    crate::ops::copy_file(archive, part, p, gate, ctx).map_err(|e| e.to_string())?;
    let mut c = archive::tool(t);
    c.stdout(Stdio::null());
    let mut stage = None;
    let lock_args = |c: &mut Command| {
        if feed.is_some() {
            c.arg("-p");
            match lock.0 {
                Lock::ZipAes => drop(c.arg("-mem=AES256")),
                Lock::ZipCrypto => drop(c.arg("-mem=ZipCrypto")),
                Lock::Seven { names: true } => drop(c.arg("-mhe=on")),
                _ => {}
            }
        }
    };
    match e {
        Edit::Add { .. } | Edit::Make { .. } => {
            let st = beside(archive, "add");
            std::fs::create_dir(&st).map_err(|e| e.to_string())?;
            stage = Some(st.clone());
            if let Err(e) = lay_out(e, &st, all, false) {
                clear(&st);
                return Err(e);
            }
            // Whatever is there already gives way: newer or not.
            c.current_dir(&st).args(["a", "-bso0", "-bsp0", "-y", ty, "-snl", LITERAL, "-up1q1r2x2y2z2w2"]);
            lock_args(&mut c);
            c.arg("--").arg(part).args(tops(&st));
        }
        Edit::Delete { members } => {
            c.args(["d", "-bso0", "-bsp0", "-y", ty, LITERAL]);
            lock_args(&mut c);
            c.arg("--").arg(part).args(members);
        }
        Edit::Rename { pairs } => {
            c.args(["rn", "-bso0", "-bsp0", "-y", ty, LITERAL]);
            lock_args(&mut c);
            c.arg("--").arg(part);
            for (a, b) in rename_moves(all, pairs) {
                c.arg(a).arg(b);
            }
        }
    }
    let r = watch(c, feed, p, gate, ctx, size, size + added);
    if let Some(st) = stage {
        clear(&st);
    }
    match r {
        Err(x) if matches!(e, Edit::Rename { .. }) && kind == Kind::Zip && x != CANCELLED && !archive::wants_password(&x) => Err(NOT_IN_PLACE.into()),
        r => r,
    }
}

/// Build the archive anew at `part`: everything extracted beside it, changed there,
/// packed again the way it was.
#[allow(clippy::too_many_arguments)]
fn rebuild(archive: &Path, part: &Path, e: &Edit, lock: Lock, pw: Option<&str>, all: &[archive::Item], p: &Mutex<Progress>, gate: &Gate, ctx: &egui::Context) -> Result<(), String> {
    let size = std::fs::metadata(archive).map(|m| m.len()).unwrap_or(0);
    let inside: u64 = all.iter().map(|i| i.size).sum();
    let added: u64 = match e {
        Edit::Add { srcs, .. } => srcs.iter().map(|s| crate::ops::tree_size(s)).sum(),
        _ => 0,
    };
    {
        let mut g = p.lock().unwrap_or_else(|e| e.into_inner());
        (g.total_bytes, g.done_bytes) = (size + inside + added, 0);
    }
    let st = beside(archive, "edit");
    let r = (|| {
        archive::extract_with(archive, &[], &st, true, &mut |c, pw| watch(c, pw.map(|p| format!("{p}\n")), p, gate, ctx, 0, size))?;
        if gate.cancelled() {
            return Err(CANCELLED.to_string());
        }
        match e {
            Edit::Delete { members } => {
                for m in members {
                    crate::ops::remove_tree(&st.join(m)).map_err(|x| format!("{}: {x}", m.display()))?;
                }
            }
            Edit::Rename { pairs } => {
                for (from, to) in pairs {
                    crate::fsx::rename_new(&st.join(from), &st.join(from.with_file_name(to))).map_err(|x| format!("{}: {x}", from.display()))?;
                }
            }
            _ => lay_out(e, &st, all, true)?,
        }
        let (c, feed) = pack(archive, part, &st, &tops(&st), Level::Normal, lock, pw)?;
        watch(c, feed, p, gate, ctx, size, inside + added)
    })();
    clear(&st);
    r
}

/// Put the finished `part` in place of `archive`: with the old one's permissions and
/// extended attributes (its tags), the old one to the trash, where Undo finds it.
fn swap(part: &Path, archive: &Path) -> Result<(), String> {
    if let Ok(m) = std::fs::metadata(archive) {
        let _ = std::fs::set_permissions(part, m.permissions());
    }
    crate::ops::copy_xattrs(archive, part);
    crate::trashcan::delete(archive).map_err(|e| format!("the old version couldn't go to the trash, so nothing changed: {e}"))?;
    std::fs::rename(part, archive).map_err(|e| {
        // Not in place: the old one back from the trash rather than nothing there.
        if let Some(it) = crate::picfix::newest_trashed(archive) {
            let _ = crate::trashcan::restore(&it);
        }
        e.to_string()
    })
}

/// Make the change `e` to `archive` (a job's work). Err carries NEEDS_PASSWORD or
/// WRONG_PASSWORD when a password is the trouble, CANCELLED when Cancel stopped it.
pub fn edit(archive: &Path, e: &Edit, p: &Mutex<Progress>, gate: &Gate, ctx: &egui::Context) -> Result<(), String> {
    let kind = writable(archive)?;
    let all = archive::list(archive)?;
    check(e, &all)?;
    let pw = archive::password(archive);
    let lock = lock_of(archive, kind, pw.as_deref())?;
    if lock.0 != Lock::Open && pw.is_none() {
        return Err(NEEDS_PASSWORD.into());
    }
    let part = beside(archive, "maia-part");
    // 7z works in place where it can; members stored as "./name" it would not find.
    let seven = archive::seven().ok().filter(|_| kind != Kind::Tar && !all.iter().any(|i| i.dot));
    let r = match seven {
        Some(t) => match in_place(t, archive, &part, kind, e, lock.clone(), pw.as_deref(), &all, p, gate, ctx) {
            Err(x) if x == NOT_IN_PLACE => {
                let _ = std::fs::remove_file(&part);
                rebuild(archive, &part, e, lock.0, pw.as_deref(), &all, p, gate, ctx)
            }
            r => r,
        },
        None => rebuild(archive, &part, e, lock.0, pw.as_deref(), &all, p, gate, ctx),
    };
    let r = r.and_then(|_| if gate.cancelled() { Err(CANCELLED.to_string()) } else { swap(&part, archive) });
    if r.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    r
}

// ------------------------------------------------------------------ from the interface

use crate::app::{Action, App};
use crate::commands::Cmd;
use crate::ops::OpKind;
use crate::popups::Popup;

/// A command given inside an archive's tab, where only some apply. True when it was
/// dealt with here; false lets the usual one run (Paste and Rename, which find their way
/// back here through `add` and `commit_rename`).
pub fn inside(app: &mut App, cmd: Cmd) -> bool {
    let Some(archive) = app.tab().archive.clone() else { return false };
    match cmd {
        Cmd::Cut => app.toast("Files in an archive are copied out, not cut"),
        Cmd::Duplicate | Cmd::CreateLink => app.toast("Copies and links aren't made inside an archive"),
        _ => match kind(&archive) {
            Err(e) => app.toast(e),
            Ok(_) => match cmd {
                Cmd::Paste | Cmd::Rename => return false,
                Cmd::Trash | Cmd::DeletePermanent => {
                    let members: Vec<PathBuf> = app.tab().targets().iter().filter_map(|p| p.strip_prefix(&archive).ok().map(Path::to_path_buf)).collect();
                    if !members.is_empty() {
                        app.move_cursor_past_selection();
                        app.actions.push(Action::StartJob(OpKind::ArchiveEdit { archive, edit: Edit::Delete { members } }));
                    }
                }
                Cmd::NewFolder | Cmd::NewFile => new_item(app, archive, cmd == Cmd::NewFolder),
                _ => {}
            },
        },
    }
    true
}

/// New Folder or New File inside an archive: the name asked for first, as each change
/// rewrites the archive and a rename after would be a second.
fn new_item(app: &mut App, archive: PathBuf, folder: bool) {
    let here = app.tab().path.clone();
    let Ok(into) = here.strip_prefix(&archive).map(Path::to_path_buf) else { return };
    let (title, name) = if folder { ("New folder", "New folder") } else { ("New file", "New file.txt") };
    let all = archive::cached(&archive);
    let name = free_name(all.as_deref().map_or(&[][..], |v| v.as_slice()), &into, name, &[]);
    app.popup = Popup::prompt(title, name, Box::new(move |app: &mut App, name: String| {
        let name = name.trim().to_string();
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            app.toast(format!("“{name}” can't be a name"));
            return;
        }
        let at = into.join(&name);
        if archive::cached(&archive).is_some_and(|all| all.iter().any(|i| i.path == at)) {
            app.toast(format!("There is a “{name}” there already"));
            return;
        }
        app.actions.push(Action::StartJob(OpKind::ArchiveEdit { archive, edit: Edit::Make { at, folder } }));
    }));
}

/// Files pasted or dropped into `dest`, a folder inside an archive: added by a job, after
/// asking about names it holds already. Always a copy: what was cut stays where it was.
pub fn add(app: &mut App, srcs: Vec<PathBuf>, dest: &Path, cut: bool) {
    let Some((archive, into)) = archive::split(dest) else { return };
    if let Err(e) = kind(&archive) {
        app.toast(e);
        return;
    }
    if srcs.iter().any(|s| archive::split(s).is_some() && !s.exists()) {
        app.toast(if srcs.iter().all(|s| s.starts_with(&archive)) { "Files move inside an archive once it is extracted" } else { "Files inside an archive go into another one once extracted" });
        return;
    }
    if cut {
        app.toast("Copied into the archive: the originals stay where they were");
    }
    if srcs.iter().any(|s| *s == archive || archive.starts_with(s)) {
        app.toast("An archive can't go inside itself");
        return;
    }
    let taken: Vec<String> = match archive::cached(&archive) {
        Some(all) => srcs.iter().map(|s| name_of(s)).filter(|n| all.iter().any(|i| i.path == into.join(n))).collect(),
        None => vec![],
    };
    let start = move |app: &mut App, srcs: Vec<PathBuf>, replace: bool| {
        app.actions.push(Action::StartJob(OpKind::ArchiveEdit { archive, edit: Edit::Add { srcs, into, replace } }));
    };
    if taken.is_empty() {
        return start(app, srcs, false);
    }
    let msg = match taken.as_slice() {
        [one] => format!("“{one}” is in the archive already"),
        v => format!("{} items are in the archive already", v.len()),
    };
    let start = std::rc::Rc::new(std::cell::RefCell::new(Some(start)));
    let (s2, srcs2) = (start.clone(), srcs.clone());
    let choices: Vec<(String, bool, Option<Box<dyn FnOnce(&mut App)>>)> = vec![
        ("Keep both".to_string(), false, Some(Box::new(move |app: &mut App| {
            if let Some(f) = start.borrow_mut().take() {
                f(app, srcs, false)
            }
        }))),
        ("Replace".to_string(), false, Some(Box::new(move |app: &mut App| {
            if let Some(f) = s2.borrow_mut().take() {
                f(app, srcs2, true)
            }
        }))),
    ];
    app.popup = Popup::Choose { msg, sub: "Keep both (the new ones under other names), or replace them; a folder is merged into the one there.".into(), choices };
}

/// The names typed in a rename inside an archive: one job renames them all. False when
/// the tab is not in an archive.
pub fn commit_rename(app: &mut App, panel: u64) -> bool {
    let Some(p) = app.panels.get_mut(&panel) else { return false };
    let t = p.tab_mut();
    let Some(archive) = t.archive.clone() else { return false };
    let Some(r) = t.rename.as_mut() else { return true };
    let plan = r.plan();
    let renaming: Vec<&PathBuf> = plan.iter().map(|(p, _)| p).collect();
    // Taken by something else in the folder: the archive's listing is the tab's.
    let taken = plan.iter().any(|(p, n)| t.entries.iter().any(|e| e.name == *n && !renaming.contains(&&e.path) && e.path.parent() == p.parent()));
    if !r.compute_conflicts().is_empty() || taken {
        r.error = Some(if r.is_batch() { "Some names are invalid or collide".into() } else { "That name is invalid or already exists".into() });
        return true;
    }
    t.rename = None;
    let pairs: Vec<(PathBuf, String)> = plan.into_iter().filter_map(|(p, n)| Some((p.strip_prefix(&archive).ok()?.to_path_buf(), n))).collect();
    if !pairs.is_empty() {
        app.actions.push(Action::StartJob(OpKind::ArchiveEdit { archive, edit: Edit::Rename { pairs } }));
    }
    true
}
