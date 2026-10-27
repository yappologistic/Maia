//! Filesystem access: fast directory listing, recursive walking, mounts, formatting.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::Arc;
use std::time::SystemTime;

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    /// Lowercased name when it differs from `name` (most names are already lowercase);
    /// read it through `lname()`.
    lower: Option<Box<str>>,
    pub path: PathBuf,
    pub is_dir: bool,
    pub is_link: bool,
    /// Socket, FIFO or device node.
    pub special: bool,
    pub hidden: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
    pub mode: u32,
    /// The owner's user id (`u32::MAX` when unknown).
    pub uid: u32,
    /// Byte offset of the extension inside `lname()` (its length when there is none).
    ext_at: u16,
    /// For flattened search results: byte offset in `path` where the part relative to the
    /// search root starts (0 = not a search result). Read it through `rel()`.
    pub rel_at: u16,
    /// Tree depth below the tab's folder (0 = top level). `DEAD` = superseded row.
    pub depth: u8,
}

pub const DEAD: u8 = u8::MAX;

impl Entry {
    pub fn from_path(path: &Path) -> Option<Entry> {
        let lmeta = fs::symlink_metadata(path).ok()?;
        let is_link = lmeta.file_type().is_symlink();
        let special = {
            use std::os::unix::fs::FileTypeExt;
            let t = lmeta.file_type();
            t.is_socket() || t.is_fifo() || t.is_block_device() || t.is_char_device()
        };
        // Follow links for type/size, fall back to the link itself if broken.
        let meta = if is_link { fs::metadata(path).unwrap_or(lmeta) } else { lmeta };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        let is_dir = meta.is_dir();
        let mut e = Entry {
            name: String::new(),
            lower: None,
            path: path.to_path_buf(),
            is_dir,
            is_link,
            special,
            hidden: false,
            size: if is_dir { 0 } else { meta.len() },
            modified: meta.modified().ok(),
            created: meta.created().ok(),
            accessed: meta.accessed().ok(),
            mode: meta.permissions().mode(),
            uid: std::os::unix::fs::MetadataExt::uid(&meta),
            ext_at: 0,
            rel_at: 0,
            depth: 0,
        };
        e.set_name(name);
        Some(e)
    }

    /// A row for something that isn't a file (an unmounted drive).
    pub fn placeholder(path: PathBuf, name: String, is_dir: bool) -> Entry {
        let mut e = Entry { name: String::new(), lower: None, path, is_dir, is_link: false, special: false, hidden: false, size: 0, modified: None, created: None, accessed: None, mode: 0, uid: u32::MAX, ext_at: 0, rel_at: 0, depth: 0 };
        e.set_name(name);
        e
    }

    /// Rename (display name only) and refresh the derived lowercase name and extension.
    pub fn set_name(&mut self, name: String) {
        let lower = name.to_lowercase();
        // Extension: after the last dot, not counting a leading one (".bashrc" has none).
        let ext_at = if self.is_dir {
            lower.len()
        } else {
            match lower.rfind('.') {
                Some(i) if i > 0 => i + 1,
                _ => lower.len(),
            }
        };
        self.ext_at = ext_at.min(u16::MAX as usize) as u16;
        self.hidden = name.starts_with('.');
        self.lower = if lower == name { None } else { Some(lower.into_boxed_str()) };
        self.name = name;
    }

    pub fn lname(&self) -> &str {
        self.lower.as_deref().unwrap_or(&self.name)
    }

    /// Lowercased extension without the dot ("" for folders and extensionless files).
    pub fn ext(&self) -> &str {
        self.lname().get(self.ext_at as usize..).unwrap_or("")
    }

    /// A search result inside a hidden folder (some part of its relative path starts
    /// with a dot). Byte scan, no allocation.
    pub fn rel_hidden(&self) -> bool {
        if self.rel_at == 0 {
            return false;
        }
        let b = self.path.as_os_str().as_encoded_bytes();
        let tail = b.get(self.rel_at as usize..).unwrap_or(&[]);
        tail.first() == Some(&b'.') || tail.windows(2).any(|w| w == b"/.")
    }

    /// For flattened search results: the path relative to the search root.
    pub fn rel(&self) -> Option<std::borrow::Cow<'_, str>> {
        if self.rel_at == 0 {
            return None;
        }
        let b = self.path.as_os_str().as_encoded_bytes();
        let tail = b.get(self.rel_at as usize..).unwrap_or(&[]);
        Some(String::from_utf8_lossy(tail))
    }

    pub fn kind(&self) -> String {
        if self.is_dir {
            "Folder".into()
        } else if self.ext().is_empty() {
            "File".into()
        } else {
            format!("{} file", self.ext().to_uppercase())
        }
    }

    pub fn is_executable(&self) -> bool {
        !self.is_dir && !self.special && !self.is_broken_link() && self.mode & 0o111 != 0
    }

    /// A link whose target is missing: its own details stand in for the target's, and a
    /// link's own mode (lrwxrwxrwx) made it look executable.
    pub fn is_broken_link(&self) -> bool {
        self.is_link && self.mode & 0o170000 == 0o120000
    }

    /// Type label for sockets, pipes and device nodes (from the st_mode type bits).
    pub fn special_kind(&self) -> Option<&'static str> {
        if !self.special {
            return None;
        }
        Some(match self.mode & 0o170000 {
            0o140000 => "Socket",
            0o010000 => "Pipe",
            0o060000 => "Block device",
            0o020000 => "Character device",
            _ => "Special file",
        })
    }
}

pub fn list_dir(path: &Path) -> std::io::Result<Vec<Entry>> {
    let rd = fs::read_dir(path)?;
    let mut out = Vec::with_capacity(256);
    for de in rd.flatten() {
        if let Some(e) = Entry::from_path(&de.path()) {
            out.push(e);
        }
    }
    Ok(out)
}

/// Result of an async listing.
pub struct Listing {
    pub path: PathBuf,
    pub result: Result<Vec<Entry>, String>,
}

/// List a directory on a worker thread; the receiver gets exactly one message.
pub fn list_dir_async(path: PathBuf, ctx: egui::Context) -> Receiver<Listing> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let result = list_dir(&path).map_err(|e| e.to_string());
        let _ = tx.send(Listing { path, result });
        ctx.request_repaint();
    });
    rx
}

/// Like `list_dir_async`, but on a small shared pool of workers: used for tree-view
/// children, where recursive expansion can queue thousands of folders at once.
pub fn list_dir_pooled(path: PathBuf, ctx: egui::Context) -> Receiver<Listing> {
    use std::sync::{Condvar, Mutex, OnceLock};
    type Job = (PathBuf, Sender<Listing>, egui::Context);
    static POOL: OnceLock<Arc<(Mutex<std::collections::VecDeque<Job>>, Condvar)>> = OnceLock::new();
    let pool = POOL.get_or_init(|| {
        let p: Arc<(Mutex<std::collections::VecDeque<Job>>, Condvar)> = Arc::new((Mutex::new(Default::default()), Condvar::new()));
        for i in 0..4 {
            let p = p.clone();
            let _ = std::thread::Builder::new().name(format!("tree{i}")).spawn(move || loop {
                let job = {
                    let (q, cv) = &*p;
                    let Ok(mut q) = q.lock() else { return };
                    loop {
                        if let Some(j) = q.pop_front() {
                            break j;
                        }
                        let Ok(g) = cv.wait(q) else { return };
                        q = g;
                    }
                };
                let (path, tx, ctx) = job;
                // Whoever asked may be gone (navigated away): skip the work then.
                let result = list_dir(&path).map_err(|e| e.to_string());
                if tx.send(Listing { path, result }).is_ok() {
                    ctx.request_repaint();
                }
            });
        }
        p
    });
    let (tx, rx) = channel();
    if let Ok(mut q) = pool.0.lock() {
        q.push_back((path, tx, ctx));
        pool.1.notify_one();
    }
    rx
}

/// Streaming recursive walk used for "flattened" views and recursive search.
pub struct Walk {
    pub rx: Receiver<Vec<Entry>>,
    pub cancel: Arc<AtomicBool>,
    pub done: Arc<AtomicBool>,
    /// Stopped early at `WALK_LIMIT` entries.
    pub truncated: Arc<AtomicBool>,
}

/// Most entries a flattened listing collects (each costs a few hundred bytes).
pub const WALK_LIMIT: usize = 1_000_000;

/// Kernel pseudo-filesystems (procfs, sysfs, cgroups, debugfs…): enormous, endless or
/// with side effects on read; never walked recursively or measured.
pub fn is_virtual_fs(p: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()) else { return false };
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
        return false;
    }
    const VIRTUAL: &[i64] = &[
        0x9fa0,     // proc
        0x62656572, // sysfs
        0x1cd1,     // devpts
        0x27e0eb,   // cgroup
        0x63677270, // cgroup2
        0x64626720, // debugfs
        0x74726163, // tracefs
        0x73636673, // securityfs
        0xcafe4a11, // bpf
        0x6165676c, // pstore
        0x62656570, // configfs
        0x65735543, // fusectl
        0xde5e81e4, // efivarfs
        0x19800202, // mqueue
        0x0187,     // autofs (walking it triggers mounts)
        0x42494e4d, // binfmt_misc
        0x6e736673, // nsfs
    ];
    VIRTUAL.contains(&(s.f_type as i64))
}

/// NFS, SMB/CIFS, AFS, Ceph and similar network filesystems (slow, may hang on a dead server).
pub fn is_network_fs(p: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()) else { return false };
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
        return false;
    }
    const NETWORK: &[i64] = &[
        0x6969,     // nfs
        0x517b,     // smb
        0xff534d42, // cifs
        0xfe534d42, // smb2
        0x5346414f, // afs
        0x00c36400, // ceph
        0x564c,     // ncp
        0x01021997, // 9p (v9fs)
    ];
    NETWORK.contains(&(s.f_type as i64))
}

impl Drop for Walk {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub fn walk_async(root: PathBuf, show_hidden: bool, ctx: egui::Context) -> Walk {
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let truncated = Arc::new(AtomicBool::new(false));
    let (c2, d2, t2) = (cancel.clone(), done.clone(), truncated.clone());
    std::thread::spawn(move || {
        if !is_virtual_fs(&root) {
            walk_worker(&root, show_hidden, &tx, &c2, &t2, &ctx);
        }
        d2.store(true, Ordering::Relaxed);
        ctx.request_repaint();
    });
    Walk { rx, cancel, done, truncated }
}

fn walk_worker(
    root: &Path,
    show_hidden: bool,
    tx: &Sender<Vec<Entry>>,
    cancel: &AtomicBool,
    truncated: &AtomicBool,
    ctx: &egui::Context,
) {
    let mut total = 0usize;
    // Directories already queued, by (device, inode): bind mounts can loop back on themselves.
    let mut seen = std::collections::HashSet::new();
    if let Ok(m) = fs::metadata(root) {
        seen.insert((m.dev(), m.ino()));
    }
    let mut stack = vec![(root.to_path_buf(), fs::metadata(root).map(|m| m.dev()).unwrap_or(0))];
    let mut batch = Vec::with_capacity(4096);
    let mut last_flush = std::time::Instant::now();
    // Every result path starts with `root/`; results store only where the relative part begins.
    let rb = root.as_os_str().as_encoded_bytes();
    let rel_at = (rb.len() + usize::from(rb.last() != Some(&b'/'))).clamp(1, u16::MAX as usize) as u16;
    while let Some((dir, dir_dev)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for de in rd.flatten() {
            let name = de.file_name();
            let hidden = name.as_encoded_bytes().first() == Some(&b'.');
            if hidden && !show_hidden {
                continue;
            }
            let p = de.path();
            let Some(mut e) = Entry::from_path(&p) else { continue };
            // Never follow symlinked dirs (cycles). Other mounts (separate btrfs subvolumes
            // such as /home, USB drives) are walked, but not kernel pseudo-filesystems or
            // network shares reached by crossing a mount point.
            if e.is_dir && !e.is_link {
                if let Ok(m) = fs::symlink_metadata(&p) {
                    let crossed = m.dev() != dir_dev;
                    if (!crossed || !(is_virtual_fs(&p) || is_network_fs(&p))) && seen.insert((m.dev(), m.ino())) {
                        stack.push((p.clone(), m.dev()));
                    }
                }
            }
            e.rel_at = rel_at;
            batch.push(e);
            total += 1;
            if total >= WALK_LIMIT {
                truncated.store(true, Ordering::Relaxed);
                let _ = tx.send(std::mem::take(&mut batch));
                return;
            }
        }
        if batch.len() >= 4096 || last_flush.elapsed().as_millis() > 50 {
            if tx.send(std::mem::take(&mut batch)).is_err() {
                return;
            }
            ctx.request_repaint();
            last_flush = std::time::Instant::now();
        }
    }
    if !batch.is_empty() {
        let _ = tx.send(batch);
    }
}

#[derive(Clone, Debug)]
pub struct Mount {
    pub device: String,
    pub path: PathBuf,
    pub fstype: String,
}

/// User-relevant mounts (root, /home, removable media), from /proc/self/mounts.
pub fn mounts() -> Vec<Mount> {
    mounts_in(&crate::drives::mounts_table())
}

/// Network filesystems listed with the disks: ones the user mounted on purpose. rclone is
/// how Google Drive, OneDrive or S3 usually end up in a folder.
const NETWORK_FS: [&str; 5] = ["fuse.sshfs", "fuse.rclone", "cifs", "nfs", "nfs4"];

pub fn mounts_in(table: &Path) -> Vec<Mount> {
    let Ok(s) = fs::read_to_string(table) else { return vec![] };
    let mut out: Vec<Mount> = Vec::new();
    for line in s.lines() {
        let mut it = line.split_whitespace();
        let (Some(dev), Some(mp), Some(fs)) = (it.next(), it.next(), it.next()) else { continue };
        let mp = unescape_mount(mp);
        let wanted = dev.starts_with("/dev/")
            && !mp.starts_with("/boot")
            && !mp.starts_with("/efi")
            && !mp.starts_with("/var")
            && !mp.starts_with("/snap")
            && !mp.starts_with("/.snapshots")
            && !mp.starts_with("/srv")
            && !mp.starts_with("/opt")
            && !mp.starts_with("/usr")
            && !mp.starts_with("/root")
            && fs != "swap";
        let removable = mp.starts_with("/run/media") || mp.starts_with("/media") || mp.starts_with("/mnt");
        if (wanted || removable || NETWORK_FS.contains(&fs))
            && !out.iter().any(|m| m.path == Path::new(&mp))
        {
            out.push(Mount { device: dev.to_string(), path: PathBuf::from(mp), fstype: fs.to_string() });
        }
    }
    out
}

fn unescape_mount(s: &str) -> String {
    // /proc/mounts escapes space, tab, newline, backslash as octal.
    s.replace("\\040", " ").replace("\\011", "\t").replace("\\012", "\n").replace("\\134", "\\")
}

/// (total, free) bytes for the filesystem containing `p`.
pub fn disk_space(p: &Path) -> Option<(u64, u64)> {
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let f = st.f_frsize as u64;
    Some((st.f_blocks as u64 * f, st.f_bavail as u64 * f))
}

/// An error as a person reads it: std's io errors end in " (os error 13)", which says
/// nothing more than the words before it.
pub fn plain_error(e: &str) -> &str {
    match e.rfind(" (os error ") {
        Some(i) if e.ends_with(')') && e[i + 11..e.len() - 1].bytes().all(|b| b.is_ascii_digit()) => &e[..i],
        _ => e,
    }
}

pub fn fmt_size(n: u64) -> String {
    const U: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if v >= 100.0 { format!("{v:.0} {}", U[i]) } else { format!("{v:.1} {}", U[i]) }
}

pub fn fmt_time(t: Option<SystemTime>) -> String {
    t.map(|t| crate::timefmt::format(t, "%Y-%m-%d %H:%M")).unwrap_or_default()
}

pub fn fmt_mode(mode: u32, is_dir: bool) -> String {
    let mut s = String::with_capacity(10);
    s.push(if is_dir { 'd' } else { '-' });
    for shift in [6, 3, 0] {
        let b = (mode >> shift) & 7;
        s.push(if b & 4 != 0 { 'r' } else { '-' });
        s.push(if b & 2 != 0 { 'w' } else { '-' });
        s.push(if b & 1 != 0 { 'x' } else { '-' });
    }
    s
}

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Expand `~` and `$VARS` in a typed path.
pub fn expand_path(input: &str) -> PathBuf {
    let s = input.trim();
    let s = if s == "~" {
        home().to_string_lossy().into_owned()
    } else if let Some(rest) = s.strip_prefix("~/") {
        format!("{}/{}", home().display(), rest)
    } else {
        s.to_string()
    };
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' {
            let mut var = String::new();
            while let Some(&n) = chars.peek() {
                if n.is_alphanumeric() || n == '_' {
                    var.push(n);
                    chars.next();
                } else {
                    break;
                }
            }
            match std::env::var(&var) {
                Ok(v) if !var.is_empty() => out.push_str(&v),
                _ => {
                    out.push('$');
                    out.push_str(&var);
                }
            }
        } else {
            out.push(c);
        }
    }
    PathBuf::from(out)
}

/// Unique destination name: "name (1).ext", "name (2).ext", ...
pub fn unique_dest(dir: &Path, name: &str) -> PathBuf {
    let cand = dir.join(name);
    if fs::symlink_metadata(&cand).is_err() {
        return cand;
    }
    let p = Path::new(name);
    let (stem, ext) = match (p.file_stem(), p.extension()) {
        (Some(s), Some(e)) if !name.starts_with('.') || name.matches('.').count() > 1 => {
            (s.to_string_lossy().into_owned(), format!(".{}", e.to_string_lossy()))
        }
        _ => (name.to_string(), String::new()),
    };
    for i in 1.. {
        let c = dir.join(format!("{stem} ({i}){ext}"));
        if fs::symlink_metadata(&c).is_err() {
            return c;
        }
    }
    unreachable!()
}

/// What kind of storage `path` lives on: network shares by mount type, spinning disks by
/// the block device's `rotational` flag, everything else (SSD, tmpfs, fuse) counts as fast.
/// Cached per device.
pub fn drive_kind(path: &Path) -> crate::config::DriveKind {
    use crate::config::DriveKind;
    use std::os::unix::fs::MetadataExt;
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<u64, DriveKind>>> = std::sync::OnceLock::new();
    let Ok(m) = fs::metadata(path) else { return DriveKind::Ssd };
    let dev = m.dev();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(k) = cache.lock().ok().and_then(|c| c.get(&dev).copied()) {
        return k;
    }
    let k = classify_drive(path, dev);
    if let Ok(mut c) = cache.lock() {
        c.insert(dev, k);
    }
    k
}

fn classify_drive(path: &Path, dev: u64) -> crate::config::DriveKind {
    use crate::config::DriveKind;
    // The mount holding `path` (longest matching mount point) decides "network".
    let mounts = fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    let canon = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut best: Option<(usize, String)> = None;
    for l in mounts.lines() {
        let mut it = l.split_whitespace();
        let (Some(_), Some(mp), Some(ty)) = (it.next(), it.next(), it.next()) else { continue };
        let mp = mp.replace("\\040", " ");
        if canon.starts_with(&mp) && best.as_ref().map(|b| mp.len() > b.0).unwrap_or(true) {
            best = Some((mp.len(), ty.to_string()));
        }
    }
    const NET: &[&str] = &["nfs", "nfs4", "cifs", "smb3", "smbfs", "fuse.sshfs", "sshfs", "fuse.rclone", "davfs", "fuse.davfs2", "9p", "afs", "ceph", "glusterfs", "fuse.gvfsd-fuse", "fuse.smbnetfs"];
    if best.map(|(_, t)| NET.contains(&t.as_str())).unwrap_or(false) {
        return DriveKind::Network;
    }
    let base = PathBuf::from(format!("/sys/dev/block/{}:{}", libc::major(dev), libc::minor(dev)));
    if let Ok(real) = fs::canonicalize(&base) {
        for p in [real.join("queue/rotational"), real.parent().map(|p| p.join("queue/rotational")).unwrap_or_default()] {
            if let Ok(s) = fs::read_to_string(&p) {
                return if s.trim() == "1" { DriveKind::Hdd } else { DriveKind::Ssd };
            }
        }
    }
    DriveKind::Ssd
}

/// Non-blocking `drive_kind` for UI code: answers from a per-path cache and classifies
/// unknown paths on a worker thread (stat on a dead network mount can block for minutes).
/// Until the answer is in, the path counts as a network location (no thumbnails or size
/// scans), which is the cautious choice.
pub fn drive_kind_cached(path: &Path, ctx: &egui::Context) -> crate::config::DriveKind {
    use std::sync::{Mutex, OnceLock};
    static KNOWN: OnceLock<Mutex<std::collections::HashMap<PathBuf, Option<crate::config::DriveKind>>>> = OnceLock::new();
    let known = KNOWN.get_or_init(Default::default);
    let Ok(mut k) = known.lock() else { return crate::config::DriveKind::Network };
    match k.get(path) {
        Some(Some(d)) => *d,
        Some(None) => crate::config::DriveKind::Network,
        None => {
            if k.len() > 4096 {
                k.clear();
            }
            k.insert(path.to_path_buf(), None);
            let (p, ctx) = (path.to_path_buf(), ctx.clone());
            std::thread::spawn(move || {
                let d = drive_kind(&p);
                if let Some(m) = KNOWN.get() {
                    if let Ok(mut m) = m.lock() {
                        m.insert(p, Some(d));
                    }
                }
                ctx.request_repaint();
            });
            crate::config::DriveKind::Network
        }
    }
}

/// Rename `from` to `to` only if nothing is at `to`: the kernel checks and renames in one
/// step, so another program can't put a file there in between and have it replaced.
/// File systems without that (some FUSE and network ones) get the check, then the rename.
pub fn rename_new(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c = |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes()).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput));
    let (f, t) = (c(from)?, c(to)?);
    // The system call, not glibc's wrapper, which older systems lack.
    let r = unsafe { libc::syscall(libc::SYS_renameat2, libc::AT_FDCWD, f.as_ptr(), libc::AT_FDCWD, t.as_ptr(), libc::RENAME_NOREPLACE) };
    if r == 0 {
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    match e.raw_os_error() {
        Some(libc::EINVAL) | Some(libc::ENOSYS) | Some(libc::ENOTSUP) => {
            if fs::symlink_metadata(to).is_ok() {
                return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists));
            }
            fs::rename(from, to)
        }
        _ => Err(e),
    }
}
