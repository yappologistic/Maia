//! Block devices (lsblk) and mounting, unmounting, ejecting, unlocking and locking them
//! through udisks (udisksctl): the same service desktop file managers use, so permissions
//! and password prompts behave as the user expects.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

/// Where `lsblk`, `udisksctl` and the mount table come from. Release builds use the
/// system's; test builds only ever use fake tools a simulation sets up (or none), so no
/// test can touch a real disk.
#[derive(Clone, Debug)]
pub struct Tools {
    dir: Option<PathBuf>,
}

#[cfg(test)]
thread_local! {
    /// Folder holding fake `lsblk`, `udisksctl` and `mounts` for this thread's app.
    pub static FAKE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

impl Tools {
    pub fn current() -> Option<Tools> {
        #[cfg(test)]
        return FAKE.with(|f| f.borrow().clone()).map(|d| Tools { dir: Some(d) });
        #[cfg(not(test))]
        Some(Tools { dir: None })
    }

    /// The tool exists (on PATH, or among the fakes).
    pub fn has(&self, name: &str) -> bool {
        match &self.dir {
            Some(d) => d.join(name).is_file(),
            None => crate::media::have(name),
        }
    }

    /// The program to run for `name`: the system's (found on PATH when run), or a fake.
    pub fn path_of(&self, name: &str) -> PathBuf {
        match &self.dir {
            Some(d) => d.join(name),
            None => PathBuf::from(name),
        }
    }

    /// `path_of`, but absolute for the system's tools (found on PATH now), as pkexec
    /// wants its program.
    pub fn program(&self, name: &str) -> PathBuf {
        match &self.dir {
            Some(d) => d.join(name),
            None => crate::admin::program(name),
        }
    }

    /// A file under /etc, or under the fakes' folder.
    pub fn etc(&self, rel: &str) -> PathBuf {
        match &self.dir {
            Some(d) => d.join(rel),
            None => Path::new("/etc").join(rel),
        }
    }

    pub fn cmd(&self, name: &str) -> Command {
        match &self.dir {
            Some(d) => Command::new(d.join(name)),
            None => Command::new(name),
        }
    }
}

/// The mount table to read (a fake one under test).
pub fn mounts_table() -> PathBuf {
    Tools::current().and_then(|t| t.dir).map(|d| d.join("mounts")).unwrap_or_else(|| PathBuf::from("/proc/self/mounts"))
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Drive {
    /// Device node, e.g. /dev/sdb1.
    pub dev: String,
    /// The whole disk it lives on (itself for a partitionless disk), e.g. /dev/sdb.
    pub disk: String,
    pub label: String,
    pub fstype: String,
    pub size: u64,
    pub mounts: Vec<PathBuf>,
    /// Can be powered off: removable media or a hot-pluggable disk that holds no part of
    /// the running system.
    pub removable: bool,
    pub crypt: Crypt,
}

/// Encryption, as far as it shows on a drive.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum Crypt {
    #[default]
    Plain,
    /// An encrypted volume (LUKS) not unlocked yet: opening it asks for its passphrase.
    Locked,
    /// The filesystem inside an unlocked encrypted volume: that volume's device, which
    /// locking closes again.
    Open(String),
}

/// Mount points that mean "this disk runs the system": never offer to eject it.
fn system_mount(p: &std::path::Path) -> bool {
    let s = p.to_string_lossy();
    // The system's own trees, and anything inside them; /home itself but not folders in
    // it (a stick mounted at ~/usb is still a stick).
    s == "/" || s == "[SWAP]" || s == "/home" || ["/boot", "/efi", "/usr", "/var", "/opt", "/srv", "/root", "/nix", "/gnu"].iter().any(|m| s == *m || s.starts_with(&format!("{m}/")))
}

/// `lsblk -P -b -o PATH,PKNAME,LABEL,FSTYPE,SIZE,RM,HOTPLUG,TYPE,MOUNTPOINTS` output.
pub fn parse_lsblk(text: &str) -> Vec<Drive> {
    struct Row {
        path: String,
        pk: String,
        label: String,
        fstype: String,
        size: u64,
        rm: bool,
        hotplug: bool,
        kind: String,
        mounts: Vec<PathBuf>,
    }
    let mut rows = vec![];
    for line in text.lines() {
        let kv = pairs(line);
        let get = |k: &str| kv.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone()).unwrap_or_default();
        let path = get("PATH");
        if path.is_empty() {
            continue;
        }
        let mounts = get("MOUNTPOINTS").split('\n').filter(|m| !m.is_empty()).map(PathBuf::from).collect();
        rows.push(Row { path, pk: get("PKNAME"), label: get("LABEL"), fstype: get("FSTYPE"), size: get("SIZE").parse().unwrap_or(0), rm: get("RM") == "1", hotplug: get("HOTPLUG") == "1", kind: get("TYPE"), mounts });
    }
    // A row's parent: the one whose kernel name its PKNAME gives.
    let parent = |r: &Row| rows.iter().find(|o| !r.pk.is_empty() && o.path.rsplit('/').next() == Some(r.pk.as_str()));
    // The whole disk, up through the parents: a partition's, or the one under the
    // partition an unlocked volume opens.
    let disks: Vec<String> = rows
        .iter()
        .map(|r| {
            let mut at = r;
            for _ in 0..8 {
                match parent(at) {
                    Some(p) => at = p,
                    None if at.pk.is_empty() => break,
                    None => return format!("/dev/{}", at.pk),
                }
            }
            at.path.clone()
        })
        .collect();
    let mut out = vec![];
    for (r, disk) in rows.iter().zip(&disks) {
        if !matches!(r.kind.as_str(), "part" | "disk" | "crypt" | "loop" | "rom") {
            continue;
        }
        // A disk carrying any system mount is never "removable", whatever it reports.
        let system = rows.iter().zip(&disks).any(|(o, od)| (o.path == *disk || od == disk) && o.mounts.iter().any(|m| system_mount(m)));
        let disk_row = rows.iter().find(|o| o.path == *disk);
        let rm = r.rm || r.hotplug || disk_row.is_some_and(|d| d.rm || d.hotplug);
        let crypt = if r.kind == "crypt" {
            Crypt::Open(parent(r).map_or_else(|| format!("/dev/{}", r.pk), |p| p.path.clone()))
        } else if r.fstype == "crypto_LUKS" && !rows.iter().any(|o| parent(o).is_some_and(|p| p.path == r.path)) {
            Crypt::Locked
        } else {
            Crypt::Plain
        };
        out.push(Drive { dev: r.path.clone(), disk: disk.clone(), label: r.label.clone(), fstype: r.fstype.clone(), size: r.size, mounts: r.mounts.clone(), removable: rm && !system, crypt });
    }
    out
}

/// KEY="value" pairs with lsblk's \xNN escapes decoded.
fn pairs(line: &str) -> Vec<(String, String)> {
    let mut out = vec![];
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && b[i] == b' ' {
            i += 1;
        }
        let ks = i;
        while i < b.len() && b[i] != b'=' {
            i += 1;
        }
        if i + 1 >= b.len() || b[i + 1] != b'"' {
            break;
        }
        let key = line[ks..i].to_string();
        i += 2;
        let mut val = vec![];
        while i < b.len() && b[i] != b'"' {
            if b[i] == b'\\' && i + 3 < b.len() && b[i + 1] == b'x' {
                let hex = |c: u8| (c as char).to_digit(16);
                if let (Some(h), Some(l)) = (hex(b[i + 2]), hex(b[i + 3])) {
                    val.push((h * 16 + l) as u8);
                    i += 4;
                    continue;
                }
            }
            val.push(b[i]);
            i += 1;
        }
        i += 1;
        out.push((key, String::from_utf8_lossy(&val).into_owned()));
    }
    out
}

impl Drive {
    pub fn mounted(&self) -> bool {
        !self.mounts.is_empty()
    }

    /// Name for lists: the label, else "500 GB Volume" (or "Encrypted 500 GB" while locked).
    pub fn title(&self) -> String {
        if !self.label.trim().is_empty() {
            self.label.clone()
        } else if self.crypt == Crypt::Locked {
            format!("Encrypted {}", crate::fsx::fmt_size(self.size))
        } else {
            format!("{} Volume", crate::fsx::fmt_size(self.size))
        }
    }

    /// An unmounted filesystem worth offering, or a locked encrypted volume (once
    /// unlocked, the filesystem inside is offered instead): not swap/LVM containers, not
    /// tiny boot partitions (EFI, MSR) on internal disks.
    pub fn offer_mount(&self) -> bool {
        if self.mounted() || self.fstype.is_empty() {
            return false;
        }
        if self.crypt != Crypt::Locked && matches!(self.fstype.as_str(), "swap" | "crypto_LUKS" | "LVM2_member" | "linux_raid_member" | "zfs_member" | "BitLocker") {
            return false;
        }
        self.removable || self.size >= 1 << 30
    }
}

pub fn list(t: &Tools) -> Vec<Drive> {
    let mut c = t.cmd("lsblk");
    c.args(["-P", "-b", "-o", "PATH,PKNAME,LABEL,FSTYPE,SIZE,RM,HOTPLUG,TYPE,MOUNTPOINTS"]);
    run(c, Duration::from_secs(5)).map(|o| parse_lsblk(&o)).unwrap_or_default()
}

/// Output of a command (stdout on success; the error text otherwise).
pub(crate) fn run(c: Command, timeout: Duration) -> Result<String, String> {
    run_fed(c, timeout, None)
}

/// `run`, with `input` written to the command's stdin: how a secret reaches it without
/// showing on its command line.
fn run_fed(mut c: Command, timeout: Duration, input: Option<&[u8]>) -> Result<String, String> {
    let mut child = c.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    // Closed once written, so the command sees where it ends.
    if let (Some(mut i), Some(data)) = (child.stdin.take(), input) {
        let _ = std::io::Write::write_all(&mut i, data);
    }
    let (mut out, mut err) = (child.stdout.take(), child.stderr.take());
    let ro = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(o) = out.as_mut() {
            let _ = std::io::Read::read_to_string(o, &mut s);
        }
        s
    });
    let re = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = err.as_mut() {
            let _ = std::io::Read::read_to_string(e, &mut s);
        }
        s
    });
    let t0 = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if t0.elapsed() < timeout => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let (o, e) = (ro.join().unwrap_or_default(), re.join().unwrap_or_default());
    match status {
        Some(st) if st.success() => Ok(o),
        Some(_) => Err(clean_error(&e, &o)),
        None => Err("Timed out".into()),
    }
}

/// The useful line of a udisks error ("…: Target is busy" rather than a D-Bus dump).
fn clean_error(err: &str, out: &str) -> String {
    let text = if err.trim().is_empty() { out } else { err };
    let line = text.lines().map(str::trim).filter(|l| !l.is_empty()).last().unwrap_or("Failed");
    let line = line.rsplit_once(": ").map(|(_, t)| t).unwrap_or(line);
    let mut s = line.trim().trim_end_matches('.').to_string();
    if let Some(c) = s.get(..1) {
        s = c.to_uppercase() + &s[1..];
    }
    s
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    Mount,
    Unmount,
    Eject,
    /// Erase a removable drive and make one filesystem on it (`Job::format`).
    Format,
    /// Open an encrypted volume with its passphrase and mount what it holds (`Job::unlock`).
    Unlock,
    /// Unmount the filesystem inside an unlocked volume and close the volume again.
    Lock,
}

/// udisksctl with `args`. Password prompts can take a while; a stuck udisks shouldn't
/// hang forever.
fn udisks(tools: &Tools, args: &[&str]) -> Result<String, String> {
    let mut c = tools.cmd("udisksctl");
    c.args(args);
    run(c, Duration::from_secs(120))
}

/// Run an operation (may wait for a password prompt). Mount returns where it mounted.
pub fn perform(tools: &Tools, op: Op, d: &Drive, all: &[Drive]) -> Result<Option<PathBuf>, String> {
    match op {
        Op::Mount => mount(tools, d),
        Op::Unmount => {
            udisks(tools, &["unmount", "-b", &d.dev])?;
            lock(tools, d).map(|_| None)
        }
        Op::Lock => release(tools, &[d]).map(|_| None),
        Op::Eject => {
            if !d.removable {
                return Err("This drive can't be ejected".into());
            }
            release(tools, &all.iter().filter(|x| x.disk == d.disk).collect::<Vec<_>>())?;
            udisks(tools, &["power-off", "-b", &d.disk]).map(|_| None)
        }
        Op::Format => Err("Formatting needs a filesystem and a label".into()),
        Op::Unlock => Err("Unlocking needs the passphrase".into()),
    }
}

fn mount(tools: &Tools, d: &Drive) -> Result<Option<PathBuf>, String> {
    let out = udisks(tools, &["mount", "-b", &d.dev])?;
    // "Mounted /dev/sdb1 at /run/media/user/LABEL"
    let at = out.split(" at ").nth(1).map(|s| PathBuf::from(s.trim().trim_end_matches('.')));
    Ok(at.or_else(|| list(tools).into_iter().find(|x| x.dev == d.dev).and_then(|x| x.mounts.first().cloned())))
}

/// Close the encrypted volume `d` was unlocked from, if it was.
fn lock(tools: &Tools, d: &Drive) -> Result<(), String> {
    match &d.crypt {
        Crypt::Open(from) => udisks(tools, &["lock", "-b", from]).map(|_| ()),
        _ => Ok(()),
    }
}

/// Unmount whichever of `parts` are mounted, then lock the volumes any were unlocked
/// from: all a disk needs before it is powered off or erased.
fn release(tools: &Tools, parts: &[&Drive]) -> Result<(), String> {
    for p in parts.iter().filter(|x| x.mounted()) {
        udisks(tools, &["unmount", "-b", &p.dev])?;
    }
    parts.iter().try_for_each(|p| lock(tools, p))
}

/// A passphrase on its way to udisks: never printed or copied, and zeroed when dropped.
pub struct Passphrase(pub String);

impl Default for Passphrase {
    /// Room for a long one up front, so typing never moves it and leaves a copy behind.
    fn default() -> Self {
        Passphrase(String::with_capacity(256))
    }
}

impl Drop for Passphrase {
    fn drop(&mut self) {
        let mut b = std::mem::take(&mut self.0).into_bytes();
        let n = b.capacity();
        b.clear();
        b.resize(n, 0);
        // Otherwise a write nobody reads may be optimised away.
        std::hint::black_box(&b);
    }
}

/// How an unlock refused for its passphrase comes back.
pub const WRONG_PASSPHRASE: &str = "Wrong passphrase";

/// udisks' error means the passphrase didn't open the volume (cryptsetup's words for
/// it vary between versions).
pub fn wrong_passphrase(err: &str) -> bool {
    let e = err.to_lowercase();
    ["passphrase", "operation not permitted", "no key available"].iter().any(|w| e.contains(w))
}

/// Unlock the encrypted volume `d` with `pass` and mount the filesystem inside it,
/// returning where. udisksctl reads the passphrase from its input as a key file, so it
/// never shows on a command line.
pub fn unlock(tools: &Tools, d: &Drive, pass: &Passphrase) -> Result<Option<PathBuf>, String> {
    let mut c = tools.cmd("udisksctl");
    c.args(["unlock", "-b", &d.dev, "--key-file", "/dev/stdin"]);
    run_fed(c, Duration::from_secs(120), Some(pass.0.as_bytes())).map_err(|e| if wrong_passphrase(&e) { WRONG_PASSPHRASE.to_string() } else { e })?;
    let inside = list(tools).into_iter().find(|x| x.crypt == Crypt::Open(d.dev.clone())).ok_or("Unlocked, but nothing inside showed up")?;
    // The desktop may have mounted it already.
    match inside.mounts.first() {
        Some(m) => Ok(Some(m.clone())),
        None => mount(tools, &inside),
    }
}

/// The filesystems a drive can be formatted with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fs {
    Fat32,
    Exfat,
    Ext4,
}

impl Fs {
    pub const ALL: [Fs; 3] = [Fs::Fat32, Fs::Exfat, Fs::Ext4];

    pub fn name(self) -> &'static str {
        match self {
            Fs::Fat32 => "FAT32",
            Fs::Exfat => "exFAT",
            Fs::Ext4 => "ext4",
        }
    }

    /// udisks' name for it.
    fn kind(self) -> &'static str {
        match self {
            Fs::Fat32 => "vfat",
            Fs::Exfat => "exfat",
            Fs::Ext4 => "ext4",
        }
    }

    /// The MBR partition type it goes in.
    fn part_type(self) -> &'static str {
        match self {
            Fs::Fat32 => "0x0c",
            Fs::Exfat => "0x07",
            Fs::Ext4 => "0x83",
        }
    }

    /// The longest label it keeps.
    pub fn max_label(self) -> usize {
        match self {
            Fs::Fat32 => 11,
            Fs::Exfat => 15,
            Fs::Ext4 => 16,
        }
    }
}

/// A label as `fs` stores it (FAT32 in capitals), or why it can't be one: too long, or
/// a character other than letters, digits, spaces, `-`, `_` and `.`.
pub fn label_for(fs: Fs, label: &str) -> Result<String, String> {
    let l = label.trim();
    if !l.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.')) {
        return Err("Letters, digits, spaces, - _ and . only".into());
    }
    if l.len() > fs.max_label() {
        return Err(format!("{} labels are {} characters at most", fs.name(), fs.max_label()));
    }
    Ok(if fs == Fs::Fat32 { l.to_ascii_uppercase() } else { l.to_string() })
}

/// udisks' object path for a block device: `/dev/sdb` is `…/block_devices/sdb`, other
/// characters than letters, digits and `_` written as `_` and two hex digits.
pub fn object_path(dev: &str) -> String {
    let name = dev.rsplit('/').next().unwrap_or(dev);
    let mut out = String::from("/org/freedesktop/UDisks2/block_devices/");
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b == b'_' {
            out.push(b as char);
        } else {
            out.push_str(&format!("_{b:02x}"));
        }
    }
    out
}

/// The two udisks calls (through gdbus) that erase `disk` and make it one partition of
/// `fs` named `label`: a new MBR partition table, then the partition, formatted.
pub fn format_calls(disk: &str, fs: Fs, label: &str) -> [Vec<String>; 2] {
    let base = |method: &str| -> Vec<String> {
        ["call", "--system", "--interactive", "--timeout", "600", "--dest", "org.freedesktop.UDisks2", "--object-path", &object_path(disk), "--method", method].map(String::from).to_vec()
    };
    let mut table = base("org.freedesktop.UDisks2.Block.Format");
    table.extend(["'dos'".to_string(), "@a{sv} {}".to_string()]);
    let mut part = base("org.freedesktop.UDisks2.PartitionTable.CreatePartitionAndFormat");
    // Ownership of the new filesystem's top folder to whoever formats it: ext4 has one.
    let own = if fs == Fs::Ext4 { ", 'take-ownership': <true>" } else { "" };
    part.extend(["0".to_string(), "0".to_string(), format!("'{}'", fs.part_type()), "''".to_string(), "@a{sv} {}".to_string(), format!("'{}'", fs.kind()), format!("{{'label': <'{label}'>{own}}}")]);
    [table, part]
}

/// Erase the disk `d` is on and make one partition of `fs` named `label` on it. Only a
/// removable disk that holds nothing of the running system, as lsblk says right now;
/// its partitions are unmounted first.
pub fn format(tools: &Tools, d: &Drive, fs: Fs, label: &str) -> Result<(), String> {
    let label = label_for(fs, label)?;
    let now = list(tools);
    let parts: Vec<&Drive> = now.iter().filter(|x| x.disk == d.disk).collect();
    if parts.is_empty() {
        return Err("The drive is no longer connected".into());
    }
    if !d.removable || parts.iter().any(|x| !x.removable) {
        return Err("Only a removable drive can be formatted here".into());
    }
    if !d.disk.starts_with("/dev/") || !d.disk[5..].bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err("Not a drive Maia can format".into());
    }
    release(tools, &parts)?;
    for args in format_calls(&d.disk, fs, &label) {
        let mut c = tools.cmd("gdbus");
        c.args(&args);
        run(c, Duration::from_secs(900))?;
    }
    Ok(())
}

impl Op {
    pub fn verb(self) -> &'static str {
        match self {
            Op::Mount => "mount",
            Op::Unmount => "unmount",
            Op::Eject => "eject",
            Op::Format => "format",
            Op::Unlock => "unlock",
            Op::Lock => "lock",
        }
    }
}

thread_local! {
    /// Devices with an operation running (drawn with a spinner, not offered again).
    static BUSY: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

pub fn busy(dev: &str) -> bool {
    BUSY.with(|b| b.borrow().iter().any(|d| d == dev))
}

/// An operation running on a worker thread (udisks may wait on a password prompt).
pub struct Job {
    pub op: Op,
    pub drive: Drive,
    /// Panel to show the drive in once mounted (and whether in a new tab).
    pub open_in: Option<(u64, bool)>,
    pub rx: Receiver<Result<Option<PathBuf>, String>>,
}

impl Job {
    pub fn start(op: Op, drive: Drive, all: Vec<Drive>, open_in: Option<(u64, bool)>, ctx: &egui::Context) -> Result<Job, String> {
        Job::spawn(op, drive, open_in, ctx, move |tools, d| perform(tools, op, d, &all))
    }

    /// Format the disk `drive` is on (see `format`), on a worker: udisks may ask for a
    /// password, and making a filesystem takes a while.
    pub fn format(drive: Drive, fs: Fs, label: String, ctx: &egui::Context) -> Result<Job, String> {
        if Tools::current().is_some_and(|t| !t.has("gdbus")) {
            return Err("Formatting needs gdbus (from GLib)".into());
        }
        Job::spawn(Op::Format, drive, None, ctx, move |tools, d| format(tools, d, fs, &label).map(|_| None))
    }

    /// Unlock the encrypted `drive` with `pass` and mount what it holds (see `unlock`),
    /// on a worker, which zeroes the passphrase once udisks has it.
    pub fn unlock(drive: Drive, pass: Passphrase, open_in: Option<(u64, bool)>, ctx: &egui::Context) -> Result<Job, String> {
        Job::spawn(Op::Unlock, drive, open_in, ctx, move |tools, d| unlock(tools, d, &pass))
    }

    /// Run `work` on a worker, the drive marked busy meanwhile.
    fn spawn(op: Op, drive: Drive, open_in: Option<(u64, bool)>, ctx: &egui::Context, work: impl FnOnce(&Tools, &Drive) -> Result<Option<PathBuf>, String> + Send + 'static) -> Result<Job, String> {
        let tools = Tools::current().ok_or("Drive tools are unavailable")?;
        if busy(&drive.dev) {
            return Err(format!("{} is busy", drive.title()));
        }
        BUSY.with(|b| b.borrow_mut().push(drive.dev.clone()));
        let (tx, rx) = std::sync::mpsc::channel();
        let (d, ctx) = (drive.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(work(&tools, &d));
            ctx.request_repaint();
        });
        Ok(Job { op, drive, open_in, rx })
    }

    /// The result once the operation finished (the device stops being busy).
    pub fn poll(&self) -> Option<Result<Option<PathBuf>, String>> {
        let r = match self.rx.try_recv() {
            Ok(r) => r,
            Err(std::sync::mpsc::TryRecvError::Empty) => return None,
            Err(_) => Err("Failed".into()),
        };
        BUSY.with(|b| b.borrow_mut().retain(|d| *d != self.drive.dev));
        Some(r)
    }
}

/// The drive mounted at `p`, if any.
pub fn at_mount<'a>(all: &'a [Drive], p: &Path) -> Option<&'a Drive> {
    all.iter().find(|d| d.mounts.iter().any(|m| m == p))
}

/// Can be unmounted from Maia: mounted somewhere that isn't part of the system.
pub fn can_unmount(d: &Drive) -> bool {
    d.mounted() && !d.mounts.iter().any(|m| system_mount(m))
}

/// Can be locked from Maia: unlocked from an encrypted volume, and not mounted as part
/// of the system.
pub fn can_lock(d: &Drive) -> bool {
    matches!(d.crypt, Crypt::Open(_)) && (!d.mounted() || can_unmount(d))
}
