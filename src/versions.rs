//! Previous Versions: earlier copies of a file kept in the disk's snapshots. Snapper keeps
//! them in `<subvolume>/.snapshots/<n>/snapshot`, Timeshift in `timeshift-btrfs/snapshots/
//! <date>/@…` at the top of a btrfs file system or, copying with rsync, in
//! `timeshift/snapshots/<date>/localhost` on the backup disk. The mount table says which
//! subvolume a file is in and where the file system's top is mounted. All of it is read on
//! a thread; the inspector lists what was found and previews the version picked.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, mpsc};
use std::time::{Duration, SystemTime};

use egui::{CornerRadius, FontId, Key, Rect, Sense, Stroke, Ui, pos2, vec2};

use crate::app::{Action, App};
use crate::fsx::Entry;
use crate::listing::Env;
use crate::theme::Glyph;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Snapper,
    Timeshift,
}

/// One line of /proc/self/mountinfo: where it is mounted, the path inside its file
/// system that appears there (a btrfs subvolume's), its device and type.
#[derive(Clone, Debug)]
pub struct Mount {
    pub point: PathBuf,
    pub root: PathBuf,
    pub dev: String,
    pub fstype: String,
}

pub fn parse_mountinfo(text: &str) -> Vec<Mount> {
    let unescape = |s: &str| PathBuf::from(crate::fsx::unescape_mount(s));
    text.lines()
        .filter_map(|l| {
            let (left, right) = l.split_once(" - ")?;
            let f: Vec<&str> = left.split(' ').collect();
            Some(Mount { dev: f.get(2)?.to_string(), root: unescape(f.get(3)?), point: unescape(f.get(4)?), fstype: right.split(' ').next()?.to_string() })
        })
        .collect()
}

#[cfg(test)]
thread_local! {
    /// The mount table this thread's app reads, as mountinfo text. Test builds never read
    /// the real one, so they never look into the machine's own snapshots.
    pub static FAKE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// The mount table to read, when it isn't the system's.
fn given_table() -> Option<String> {
    #[cfg(test)]
    return Some(FAKE.with(|f| f.borrow().clone()).unwrap_or_default());
    #[cfg(not(test))]
    None
}

#[derive(Clone, Debug)]
pub struct Version {
    pub entry: Entry,
    /// When the snapshot was taken.
    pub when: Option<SystemTime>,
    /// What the snapshot says about itself ("timeline", "before update").
    pub label: String,
    pub tool: Tool,
}

#[derive(Clone, Debug, Default)]
pub struct Found {
    /// Newest first, each different from the next newer one (or the file itself).
    pub versions: Vec<Version>,
    /// Snapshots looked in.
    pub snapshots: usize,
    /// Tools whose snapshots are there but only an administrator can read.
    pub locked: Vec<Tool>,
}

struct Candidate {
    path: PathBuf,
    when: Option<SystemTime>,
    label: String,
    tool: Tool,
}

/// The mount `file` is on: the deepest mount point above it.
fn mount_of<'a>(file: &Path, mounts: &'a [Mount]) -> Option<&'a Mount> {
    mounts.iter().filter(|m| file.starts_with(&m.point)).max_by_key(|m| m.point.components().count())
}

/// File systems a backup disk can be: the network and the kernel's own are left alone
/// (a dead share must not hang the search).
fn local_fs(t: &str) -> bool {
    matches!(t, "ext2" | "ext3" | "ext4" | "btrfs" | "xfs" | "f2fs" | "vfat" | "exfat" | "ntfs" | "ntfs3" | "fuseblk" | "bcachefs" | "zfs" | "jfs" | "reiserfs")
}

pub fn find(file: &Path, mounts: &[Mount], stop: &AtomicBool) -> Found {
    let mut found = Found::default();
    let Some(m) = mount_of(file, mounts) else { return found };
    let mut cands = vec![];
    snapper(file, m, &mut cands, &mut found);
    timeshift_btrfs(file, m, mounts, &mut cands, &mut found);
    timeshift_rsync(file, mounts, &mut cands, &mut found);
    found.locked.dedup();
    let mut order: Vec<usize> = (0..cands.len()).collect();
    crate::tab::merge_sort(&mut order, &|a: &usize, b: &usize| cands[*b].when.cmp(&cands[*a].when));
    let mut newer = file.to_path_buf();
    for i in order {
        if stop.load(Relaxed) {
            return Found::default();
        }
        let c = &cands[i];
        let Some(entry) = Entry::from_path(&c.path).filter(|e| !e.is_dir && !e.special) else { continue };
        if differs(&c.path, &newer, stop) {
            found.versions.push(Version { entry, when: c.when, label: c.label.clone(), tool: c.tool });
        }
        newer = c.path.clone();
    }
    found
}

/// Whether two files hold different contents. Another size settles it; the same size and
/// time is taken as the same file (a snapshot keeps the time of what it copied), and only
/// files of one size with different times are read through.
fn differs(a: &Path, b: &Path, stop: &AtomicBool) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else { return true };
    if ma.len() != mb.len() {
        return true;
    }
    if ma.modified().ok() == mb.modified().ok() {
        return false;
    }
    let (Ok(mut fa), Ok(mut fb)) = (std::fs::File::open(a), std::fs::File::open(b)) else { return true };
    let (mut ba, mut bb) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
    loop {
        if stop.load(Relaxed) {
            return true;
        }
        let Ok(n) = read_full(&mut fa, &mut ba) else { return true };
        let Ok(k) = read_full(&mut fb, &mut bb) else { return true };
        if n != k || ba[..n] != bb[..k] {
            return true;
        }
        if n == 0 {
            return false;
        }
    }
}

fn read_full(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

fn denied(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::PermissionDenied
}

/// Snapper: a `.snapshots` folder at the top of the subvolume the file is in, which is
/// its mount point or a folder between it and the file.
fn snapper(file: &Path, m: &Mount, out: &mut Vec<Candidate>, found: &mut Found) {
    for top in file.ancestors().skip(1) {
        if !top.starts_with(&m.point) {
            break;
        }
        let dir = top.join(".snapshots");
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if denied(&e) => {
                found.locked.push(Tool::Snapper);
                return;
            }
            Err(_) => continue,
        };
        let Ok(rel) = file.strip_prefix(top) else { return };
        for d in rd.flatten() {
            // Snapshot 0 is the subvolume as it is now.
            if !d.file_name().to_str().and_then(|n| n.parse::<u64>().ok()).is_some_and(|n| n > 0) {
                continue;
            }
            let snap = d.path().join("snapshot");
            match std::fs::metadata(&snap) {
                Ok(_) => found.snapshots += 1,
                Err(e) if denied(&e) => {
                    found.locked.push(Tool::Snapper);
                    continue;
                }
                Err(_) => continue,
            }
            let path = snap.join(rel);
            if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
                let (when, label) = snapper_info(&d.path());
                out.push(Candidate { path, when, label, tool: Tool::Snapper });
            }
        }
        return;
    }
}

/// A Snapper snapshot's date (in UTC) and description, from its info.xml.
fn snapper_info(dir: &Path) -> (Option<SystemTime>, String) {
    let xml = std::fs::read_to_string(dir.join("info.xml")).unwrap_or_default();
    let tag = |t: &str| -> Option<String> {
        let a = xml.find(&format!("<{t}>"))? + t.len() + 2;
        let b = a + xml[a..].find(&format!("</{t}>"))?;
        Some(xml[a..b].replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&").trim().to_string())
    };
    let when = tag("date").and_then(|d| parse_utc(&d)).or_else(|| std::fs::metadata(dir).and_then(|m| m.modified()).ok());
    let label = tag("description").filter(|d| !d.is_empty()).or_else(|| tag("cleanup").filter(|c| !c.is_empty())).unwrap_or_default();
    (when, label)
}

/// "2026-10-12 08:00:01", in UTC.
pub fn parse_utc(s: &str) -> Option<SystemTime> {
    let n: Vec<i64> = s.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).filter_map(|p| p.parse().ok()).collect();
    let [y, mo, d, h, mi, se] = *n.get(..6)? else { return None };
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3600 + mi * 60 + se;
    (secs >= 0).then(|| SystemTime::UNIX_EPOCH + Duration::from_secs(secs as u64))
}

/// Timeshift's snapshot folders under `base`, each with when it was taken and its note.
fn timeshift_dirs(base: &Path, found: &mut Found) -> Vec<(PathBuf, Option<SystemTime>, String)> {
    let rd = match std::fs::read_dir(base) {
        Ok(rd) => rd,
        Err(e) => {
            if denied(&e) {
                found.locked.push(Tool::Timeshift);
            }
            return vec![];
        }
    };
    let mut v = vec![];
    for d in rd.flatten() {
        let name = d.file_name().to_string_lossy().into_owned();
        // Named for the time it was taken: 2026-10-12_08-00-01.
        let Some(at) = crate::timefmt::parse_local(&name) else { continue };
        let info = std::fs::read_to_string(d.path().join("info.json")).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
        let field = |k: &str| info.as_ref().and_then(|i| i.get(k)).and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_u64().map(|n| n.to_string()))).unwrap_or_default();
        let when = field("created").parse::<u64>().ok().map(|s| SystemTime::UNIX_EPOCH + Duration::from_secs(s)).unwrap_or(at);
        let label = Some(field("comments")).filter(|c| !c.trim().is_empty()).unwrap_or_else(|| timeshift_tags(&field("tags")));
        found.snapshots += 1;
        v.push((d.path(), Some(when), label));
    }
    v
}

/// Timeshift's letters for why a snapshot was taken, said in words.
fn timeshift_tags(tags: &str) -> String {
    let words: Vec<&str> = tags
        .split_whitespace()
        .filter_map(|t| match t {
            "O" => Some("on demand"),
            "B" => Some("boot"),
            "H" => Some("hourly"),
            "D" => Some("daily"),
            "W" => Some("weekly"),
            "M" => Some("monthly"),
            _ => None,
        })
        .collect();
    words.join(", ")
}

/// Timeshift on btrfs: its snapshots sit at the top of the file system (the subvolume
/// with id 5), each holding copies of `@` and `@home`. They can only be reached where
/// that top is mounted somewhere.
fn timeshift_btrfs(file: &Path, m: &Mount, mounts: &[Mount], out: &mut Vec<Candidate>, found: &mut Found) {
    if m.fstype != "btrfs" {
        return;
    }
    let (Ok(sub), Ok(rel)) = (m.root.strip_prefix("/"), file.strip_prefix(&m.point)) else { return };
    if sub.as_os_str().is_empty() {
        return;
    }
    for top in mounts.iter().filter(|t| t.dev == m.dev && t.root == Path::new("/")) {
        for (dir, when, label) in timeshift_dirs(&top.point.join("timeshift-btrfs/snapshots"), found) {
            let path = dir.join(sub).join(rel);
            if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
                out.push(Candidate { path, when, label, tool: Tool::Timeshift });
            }
        }
    }
}

/// Timeshift with rsync: a `timeshift` folder at the top of whichever disk it backs up
/// to, the whole system copied under `localhost` in each snapshot.
fn timeshift_rsync(file: &Path, mounts: &[Mount], out: &mut Vec<Candidate>, found: &mut Found) {
    let Ok(rel) = file.strip_prefix("/") else { return };
    let mut seen: Vec<&Path> = vec![];
    for m in mounts.iter().filter(|m| local_fs(&m.fstype)) {
        if seen.contains(&m.point.as_path()) {
            continue;
        }
        seen.push(&m.point);
        let base = m.point.join("timeshift/snapshots");
        if std::fs::symlink_metadata(&base).is_err() && std::fs::symlink_metadata(m.point.join("timeshift")).is_err() {
            continue;
        }
        for (dir, when, label) in timeshift_dirs(&base, found) {
            let path = dir.join("localhost").join(rel);
            if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
                out.push(Candidate { path, when, label, tool: Tool::Timeshift });
            }
        }
    }
}

/// One plain sentence for the snapshots that can't be read.
pub fn locked_note(locked: &[Tool]) -> Option<String> {
    match locked {
        [] => None,
        [Tool::Snapper] => Some("Snapper's snapshots here can only be read by an administrator.".into()),
        [Tool::Timeshift] => Some("Timeshift's snapshots here can only be read by an administrator.".into()),
        _ => Some("The snapshots here can only be read by an administrator.".into()),
    }
}

/// A search in progress; dropping it stops it.
pub struct Scan {
    rx: mpsc::Receiver<Found>,
    stop: Arc<AtomicBool>,
}

impl Drop for Scan {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
    }
}

impl Scan {
    pub fn start(file: PathBuf, ctx: &egui::Context) -> Scan {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let (st, ctx, table) = (stop.clone(), ctx.clone(), given_table());
        let _ = std::thread::Builder::new().name("versions".into()).spawn(move || {
            let text = table.unwrap_or_else(|| std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default());
            let f = find(&file, &parse_mountinfo(&text), &st);
            let _ = tx.send(f);
            ctx.request_repaint();
        });
        Scan { rx, stop }
    }
}

// ------------------------------------------------------------------ the page

/// The inspector's Previous Versions page for one file.
pub struct Page {
    pub file: PathBuf,
    scan: Option<Scan>,
    pub found: Option<Found>,
    pub sel: usize,
    /// The page opened the inspector, and closes it again.
    pub opened: bool,
    /// Escape was pressed: the page closes on the next frame.
    pub closing: bool,
    /// Scroll the selected row into view.
    reveal: bool,
}

impl Page {
    pub fn new(file: PathBuf, opened: bool, ctx: &egui::Context) -> Page {
        let scan = Some(Scan::start(file.clone(), ctx));
        Page { file, scan, found: None, sel: 0, opened, closing: false, reveal: false }
    }

    pub fn selected(&self) -> Option<&Version> {
        self.found.as_ref()?.versions.get(self.sel)
    }

    pub fn takes(&self, k: Key) -> bool {
        matches!(k, Key::ArrowUp | Key::ArrowDown | Key::PageUp | Key::PageDown | Key::Home | Key::End | Key::Escape)
    }

    pub fn key(&mut self, k: Key) {
        let n = self.found.as_ref().map_or(0, |f| f.versions.len());
        let d: isize = match k {
            Key::Escape => {
                self.closing = true;
                return;
            }
            Key::ArrowDown => 1,
            Key::ArrowUp => -1,
            Key::PageDown => 8,
            Key::PageUp => -8,
            Key::Home => -(self.sel as isize),
            _ => n as isize,
        };
        self.sel = self.sel.saturating_add_signed(d).min(n.saturating_sub(1));
        self.reveal = true;
    }
}

/// Previous Versions for the file under the cursor, in the inspector (opened for it when
/// it was closed).
pub fn open(app: &mut App) {
    let ctx = app.ctx.clone();
    let t = app.tab();
    let file = match t.target_entries().as_slice() {
        [e] if !e.is_dir => e.path.clone(),
        _ => {
            app.toast("Select a file to see its earlier versions");
            return;
        }
    };
    let p = app.panel();
    let opened = p.inspector.is_none();
    let ins = p.inspector.get_or_insert_with(Default::default);
    ins.git = None;
    ins.compare = None;
    ins.versions = Some(Box::new(Page::new(file, opened, &ctx)));
}

/// What the page leaves for the rest of the inspector.
pub enum Band {
    /// Closed (its × or Escape).
    Close,
    /// The page drew the whole inspector (nothing to preview).
    Whole,
    /// Below this rect, preview this version.
    Preview(Rect, Entry),
}

/// The list of versions across the top of the inspector, with Restore and Restore as
/// Copy under it.
pub fn band(ui: &mut Ui, rect: Rect, pg: &mut Page, env: &mut Env) -> Band {
    let pal = *env.pal;
    // Nothing of it outside the inspector, however short a window makes that.
    let mut inside = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt(("versions band", env.panel_id)));
    inside.set_clip_rect(rect.intersect(ui.clip_rect()));
    let ui = &mut inside;
    if let Some(s) = &pg.scan {
        if let Ok(f) = s.rx.try_recv() {
            pg.found = Some(f);
            pg.scan = None;
            pg.sel = 0;
        }
    }
    if pg.closing {
        return Band::Close;
    }
    let fs = env.cfg.font_size;
    let small = fs - 1.5;
    ui.painter().rect_filled(rect, CornerRadius::ZERO, pal.bg);
    // ---------------------------------------------------------- header
    let hdr = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + 30.0));
    ui.painter().line_segment([hdr.left_bottom(), hdr.right_bottom()], Stroke::new(1.0f32, pal.border));
    let mut hui = ui.new_child(egui::UiBuilder::new().max_rect(hdr.shrink2(vec2(6.0, 3.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    hui.set_clip_rect(hdr.intersect(ui.clip_rect()));
    if crate::ui::glyph_button_as(&mut hui, Glyph::Close, "Close previous versions (Esc)", true, &pal, "versions close").clicked() {
        // Gone on the next frame: the preview drawn now would land on this header.
        pg.closing = true;
        ui.ctx().request_repaint();
        return Band::Whole;
    }
    hui.add_space(4.0);
    let name = pg.file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let g = crate::listing::elided(&hui, &format!("Previous versions of {name}"), FontId::proportional(fs - 0.5), hui.available_width() - 4.0, pal.text);
    let (r, _) = hui.allocate_exact_size(g.size(), Sense::hover());
    hui.painter().galley(r.min, g, pal.text);
    // ---------------------------------------------------------- the list
    let row_h = (fs * 1.35 + 10.0 * env.cfg.spacing).round();
    let n = pg.found.as_ref().map_or(0, |f| f.versions.len());
    let note = pg.found.as_ref().and_then(|f| locked_note(&f.locked));
    let empty = match &pg.found {
        None => Some("Looking in the snapshots…".to_string()),
        Some(f) if f.versions.is_empty() && f.locked.is_empty() && f.snapshots == 0 => Some("This disk keeps no snapshots of this file's folder.".to_string()),
        Some(f) if f.versions.is_empty() && f.locked.is_empty() => Some("The snapshots hold no earlier version of this file.".to_string()),
        Some(_) => None,
    };
    let body = Rect::from_min_max(pos2(rect.left(), hdr.bottom() + 1.0), rect.max);
    if n == 0 {
        // Nothing to pick: what was found, said in the middle.
        let text = empty.or(note).unwrap_or_default();
        let g = ui.painter().layout(text, FontId::proportional(fs), pal.text_dim, (body.width() - 32.0).max(40.0));
        ui.painter().galley(pos2(body.center().x - g.size().x / 2.0, body.center().y - g.size().y / 2.0), g, pal.text_dim);
        if pg.scan.is_some() {
            crate::ui::spinner(ui.painter(), pos2(body.center().x, body.center().y - 28.0), 6.0, pal.accent, ui.input(|i| i.time));
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        }
        return Band::Whole;
    }
    let buttons_h = 34.0 + 4.0 * env.cfg.spacing;
    // The note gives way when it would leave no room for a row and the buttons.
    let note_g = note.map(|t| ui.painter().layout(t, FontId::proportional(small), pal.text_dim, (body.width() - 28.0).max(40.0))).filter(|g| body.height() - 8.0 - buttons_h - g.size().y - 8.0 >= row_h);
    let note_h = note_g.as_ref().map_or(0.0, |g| g.size().y + 8.0);
    // The list takes what it needs, up to two fifths of the inspector; the preview the rest.
    let fits = ((body.height() - 8.0 - buttons_h - note_h) / row_h).floor().max(1.0);
    let most = ((body.height() * 0.4 - buttons_h - note_h) / row_h).floor().max(2.0).min(fits);
    let list_h = (n as f32).min(most) * row_h;
    let list = Rect::from_min_size(body.min + vec2(0.0, 4.0), vec2(body.width(), list_h));
    let mut lui = ui.new_child(egui::UiBuilder::new().max_rect(list.shrink2(vec2(6.0, 0.0))).id_salt(("versions list", env.panel_id)));
    lui.set_clip_rect(list.intersect(ui.clip_rect()));
    // Rows touch: the list's height is counted in whole rows.
    lui.spacing_mut().item_spacing.y = 0.0;
    let mut area = egui::ScrollArea::vertical().id_salt(("versions", env.panel_id)).max_height(list_h).auto_shrink([false, false]);
    if std::mem::take(&mut pg.reveal) {
        let top = pg.sel as f32 * row_h;
        let most = (n as f32 * row_h - list_h).max(0.0);
        area = area.vertical_scroll_offset((top - list_h / 2.0 + row_h / 2.0).clamp(0.0, most));
    }
    let versions = pg.found.as_ref().map(|f| f.versions.clone()).unwrap_or_default();
    let mut pick = None;
    area.show_rows(&mut lui, row_h, n, |ui, range| {
        for i in range {
            let v = &versions[i];
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
            crate::ui::note_control(ui, r, "version row");
            let on = i == pg.sel;
            if on {
                ui.painter().rect_filled(r, CornerRadius::same(6), pal.selected);
            } else if resp.hovered() {
                ui.painter().rect_filled(r, CornerRadius::same(6), pal.hover);
            }
            if resp.clicked() {
                pick = Some(i);
            }
            let (tc, dc) = if on { (pal.on_sel(), pal.on_sel_dim()) } else { (pal.text, pal.text_dim) };
            let size = ui.painter().layout_no_wrap(crate::fsx::fmt_size(v.entry.size), FontId::proportional(small), dc);
            let sw = size.size().x;
            ui.painter().galley(pos2(r.right() - 10.0 - sw, r.center().y - size.size().y / 2.0), size, dc);
            let when = v.when.map(|t| crate::timefmt::date(t, &env.cfg.date_format)).unwrap_or_default();
            let room = r.width() - 30.0 - sw;
            let wg = crate::listing::elided(ui, &when, FontId::proportional(fs), room, tc);
            let ww = wg.size().x;
            ui.painter().galley(pos2(r.left() + 10.0, r.center().y - wg.size().y / 2.0), wg, tc);
            // A snapshot that says nothing of itself is named by what took it.
            let label = if !v.label.is_empty() { v.label.as_str() } else if v.tool == Tool::Snapper { "Snapper" } else { "Timeshift" };
            if room - ww > 40.0 {
                let lg = crate::listing::elided(ui, label, FontId::proportional(small), room - ww - 12.0, dc);
                ui.painter().galley(pos2(r.left() + 22.0 + ww, r.center().y - lg.size().y / 2.0), lg, dc);
            }
        }
    });
    if let Some(i) = pick {
        pg.sel = i;
    }
    let mut y = list.bottom() + 4.0;
    if let Some(g) = note_g {
        ui.painter().galley(pos2(body.left() + 14.0, y + 4.0), g, pal.text_dim);
        y += note_h;
    }
    // ---------------------------------------------------------- Restore, Restore as Copy
    let brow = Rect::from_min_size(pos2(body.left(), y), vec2(body.width(), buttons_h));
    let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(brow.shrink2(vec2(12.0, 4.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    bui.set_clip_rect(brow.intersect(ui.clip_rect()));
    bui.spacing_mut().item_spacing.x = 8.0;
    let chosen = pg.selected().cloned();
    let restore = crate::popups::primary_button(&mut bui, "Restore", chosen.is_some(), &pal).on_hover_text("Put this version in place of the file; the file goes to the trash");
    crate::ui::note_control(&bui, restore.rect, "version restore");
    let copy = if crate::ui::no_room(&bui, 120.0) { None } else { Some(crate::popups::plain_button(&mut bui, "Restore as Copy", &pal)) };
    if let Some(c) = &copy {
        crate::ui::note_control(&bui, c.rect, "version copy");
    }
    let as_copy = copy.is_some_and(|c| c.clicked());
    if let (Some(v), true) = (&chosen, restore.clicked() || as_copy) {
        let (from, file, when) = (v.entry.path.clone(), pg.file.clone(), v.when);
        env.actions.push(Action::Run(Box::new(move |app: &mut App| restore_version(app, from, file, when, as_copy))));
        pg.closing = true;
        ui.ctx().request_repaint();
        return Band::Whole;
    }
    let sep = brow.bottom().round() + 0.5;
    ui.painter().line_segment([pos2(rect.left(), sep), pos2(rect.right(), sep)], Stroke::new(1.0f32, pal.border));
    match chosen {
        Some(v) if sep + 40.0 < rect.bottom() => Band::Preview(Rect::from_min_max(pos2(rect.left(), sep + 1.0), rect.max), v.entry),
        _ => Band::Whole,
    }
}

/// `name (from 2026-10-12).ext`, for a copy of the version taken then.
pub fn copy_name(file: &Path, when: Option<SystemTime>) -> String {
    let day = when.map(|t| crate::timefmt::format(t, "%Y-%m-%d")).unwrap_or_else(|| "a snapshot".into());
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match name.rfind('.').filter(|&i| i > 0) {
        Some(i) => format!("{} (from {day}){}", &name[..i], &name[i..]),
        None => format!("{name} (from {day})"),
    }
}

/// Copy `from` to `to` through a hidden file beside it, renamed into place once whole,
/// with the copy's time kept.
fn copy_into(from: &Path, to: &Path) -> std::io::Result<()> {
    let name = to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = to.with_file_name(format!(".{name}.part"));
    let r = std::fs::copy(from, &tmp).and_then(|_| {
        if let Ok(t) = std::fs::metadata(from).and_then(|m| m.modified()) {
            let _ = std::fs::File::options().write(true).open(&tmp).and_then(|f| f.set_modified(t));
        }
        Ok(())
    });
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return r;
    }
    std::fs::rename(&tmp, to).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Put the version back: in the file's place, the file going to the trash first (Undo
/// brings it back), or as a copy beside it.
pub fn restore(from: &Path, file: &Path, when: Option<SystemTime>, as_copy: bool) -> Result<PathBuf, String> {
    if as_copy {
        let dir = file.parent().ok_or("No folder to put it in")?;
        let dest = crate::fsx::unique_dest(dir, &copy_name(file, when));
        copy_into(from, &dest).map_err(|e| e.to_string())?;
        return Ok(dest);
    }
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let staged = file.with_file_name(format!(".{name}.restore"));
    copy_into(from, &staged).map_err(|e| e.to_string())?;
    if let Err(e) = crate::trashcan::delete(file) {
        let _ = std::fs::remove_file(&staged);
        return Err(e.to_string());
    }
    std::fs::rename(&staged, file).map(|_| file.to_path_buf()).map_err(|e| {
        // Put the file back rather than leave nothing there.
        let back = crate::trashcan::list().into_iter().filter(|it| it.original == file).max_by(|a, b| a.deleted.cmp(&b.deleted));
        if let Some(it) = back {
            let _ = crate::trashcan::restore(&it);
        }
        let _ = std::fs::remove_file(&staged);
        e.to_string()
    })
}

fn restore_version(app: &mut App, from: PathBuf, file: PathBuf, when: Option<SystemTime>, as_copy: bool) {
    let ctx = app.ctx.clone();
    let day = when.map(|t| crate::timefmt::date(t, &app.cfg.date_format)).unwrap_or_default();
    app.errands.run(&ctx, move || {
        let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match restore(&from, &file, when, as_copy) {
            Ok(dest) if as_copy => {
                let undo = crate::app::UndoEntry { label: format!("Restore {name} as a copy"), kind: crate::app::UndoKind::Created(vec![dest.clone()]) };
                crate::errands::Done { toast: format!("Saved the version from {day} as {}", dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()), undo: Some(undo), changed: vec![], select: Some(dest) }
            }
            Ok(_) => {
                let undo = crate::app::UndoEntry { label: format!("Restore {name}"), kind: crate::app::UndoKind::Replaced(vec![file.clone()]) };
                crate::errands::Done { toast: format!("Restored {name} from {day}"), undo: Some(undo), changed: vec![file.clone()], select: Some(file) }
            }
            Err(e) => crate::errands::Done::say(format!("Couldn't restore {name}: {e}")),
        }
    });
}
