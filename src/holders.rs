//! What's using a file, a folder or a network port: the processes that have it open, run
//! from it or work inside it, read from /proc on a thread. Ports come from the kernel's
//! socket tables (/proc/net/tcp, tcp6, udp, udp6), matched to processes through the
//! socket inodes among their open files. The user's own processes can be stopped: asked
//! first, sent SIGTERM, and offered SIGKILL when still there a few seconds later.
//! Maia itself is never listed: it can't be stopped from here.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use egui::{Color32, CornerRadius, FontId, Rect, Sense, Stroke, StrokeKind, pos2, vec2};

use crate::app::App;
use crate::popups::{self as pp, Popup};

/// How long a stopped process has to end before Force Stop is offered.
const GRACE: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, PartialEq)]
pub struct Proc {
    pub pid: u32,
    /// When it started (clock ticks after boot): a PID used again is another process.
    pub start: u64,
    pub name: String,
    pub uid: u32,
}

impl Proc {
    pub fn mine(&self) -> bool {
        self.uid == unsafe { libc::getuid() }
    }
}

/// How a process holds a path.
#[derive(Clone, Debug, PartialEq)]
pub enum Hold {
    Open(PathBuf),
    /// Its working folder.
    Inside(PathBuf),
    /// The program it runs.
    Runs(PathBuf),
}

/// The fields after the name in /proc/PID/stat: the state, then the rest. The name is in
/// parentheses and may hold anything, spaces and parentheses too, so it ends at the last ')'.
fn stat_fields(stat: &str) -> Option<Vec<&str>> {
    let at = stat.rfind(')')?;
    Some(stat[at + 1..].split_whitespace().collect())
}

/// A running process (not a zombie), as /proc tells it now.
pub fn info(pid: u32) -> Option<Proc> {
    let dir = PathBuf::from(format!("/proc/{pid}"));
    let stat = std::fs::read_to_string(dir.join("stat")).ok()?;
    let f = stat_fields(&stat)?;
    if f.first() == Some(&"Z") {
        return None;
    }
    let start = f.get(19)?.parse().ok()?;
    let status = std::fs::read_to_string(dir.join("status")).ok()?;
    let uid = status.lines().find_map(|l| l.strip_prefix("Uid:"))?.split_whitespace().next()?.parse().ok()?;
    // The program's own file name is whole; the kernel's name for it stops at 15 bytes.
    let comm = stat.find('(').and_then(|a| stat.rfind(')').map(|b| stat[a + 1..b].to_string())).unwrap_or_default();
    let exe = std::fs::read_link(dir.join("exe")).ok().and_then(|e| e.file_name().map(|n| n.to_string_lossy().into_owned()));
    let name = match exe {
        Some(e) if e.starts_with(&comm) || comm.is_empty() => e.trim_end_matches(" (deleted)").to_string(),
        _ => comm,
    };
    Some(Proc { pid, start, name, uid })
}

/// Whether the process that was `p` is still running.
pub fn alive(p: &Proc) -> bool {
    info(p.pid).is_some_and(|now| now.start == p.start)
}

/// Every process but Maia.
pub fn pids() -> Vec<u32> {
    let me = std::process::id();
    let Ok(rd) = std::fs::read_dir("/proc") else { return vec![] };
    let mut v: Vec<u32> = rd.flatten().filter_map(|d| d.file_name().to_str()?.parse().ok()).filter(|&p| p != me).collect();
    v.sort_unstable();
    v
}

/// What `path` (a link in /proc) points at, when it is a path on disk.
fn target(link: &Path) -> Option<PathBuf> {
    let t = std::fs::read_link(link).ok()?;
    let s = t.as_os_str().as_encoded_bytes();
    (s.first() == Some(&b'/') && !s.ends_with(b" (deleted)")).then_some(t)
}

/// How each of `pids` holds one of `paths`: a file open or run, a folder worked in, or
/// anything open or run from inside a folder. One hold per process, the first found.
pub fn holding(paths: &[PathBuf], pids: &[u32]) -> Vec<(Proc, Hold)> {
    let hit = |t: &Path| paths.iter().any(|p| t == p || t.starts_with(p));
    let mut out = vec![];
    for &pid in pids {
        let dir = PathBuf::from(format!("/proc/{pid}"));
        let hold = target(&dir.join("cwd")).filter(|t| hit(t)).map(Hold::Inside).or_else(|| target(&dir.join("exe")).filter(|t| hit(t)).map(Hold::Runs)).or_else(|| {
            std::fs::read_dir(dir.join("fd")).ok()?.flatten().find_map(|fd| target(&fd.path()).filter(|t| hit(t)).map(Hold::Open))
        });
        if let (Some(h), Some(p)) = (hold, info(pid)) {
            out.push((p, h));
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq)]
pub struct Port {
    pub tcp: bool,
    pub addr: IpAddr,
    pub port: u16,
    pub inode: u64,
    pub uid: u32,
}

/// An address as the socket tables write it: hex, each 32-bit word in the machine's order.
fn parse_addr(hex: &str) -> Option<IpAddr> {
    let word = |i: usize| u32::from_str_radix(hex.get(i * 8..i * 8 + 8)?, 16).ok().map(u32::to_ne_bytes);
    match hex.len() {
        8 => Some(IpAddr::V4(Ipv4Addr::from(word(0)?))),
        32 => {
            let mut b = [0u8; 16];
            for i in 0..4 {
                b[i * 4..i * 4 + 4].copy_from_slice(&word(i)?);
            }
            let a = Ipv6Addr::from(b);
            Some(a.to_ipv4_mapped().map_or(IpAddr::V6(a), IpAddr::V4))
        }
        _ => None,
    }
}

/// The listening sockets in one of the kernel's tables: TCP in LISTEN, UDP not connected
/// to anyone (which is what a UDP server is).
pub fn parse_table(text: &str, tcp: bool) -> Vec<Port> {
    let mut v = vec![];
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        let (Some(local), Some(remote), Some(st), Some(uid), Some(inode)) = (f.get(1), f.get(2), f.get(3), f.get(7), f.get(9)) else { continue };
        let listening = if tcp { *st == "0A" } else { *st == "07" && remote.split(':').nth(1) == Some("0000") };
        let Some((a, p)) = local.split_once(':') else { continue };
        let (Some(addr), Ok(port), Ok(uid), Ok(inode)) = (parse_addr(a), u16::from_str_radix(p, 16), uid.parse(), inode.parse()) else { continue };
        if listening && inode != 0 {
            v.push(Port { tcp, addr, port, inode, uid });
        }
    }
    v
}

/// Every listening port, by number.
pub fn listening() -> Vec<Port> {
    let mut v = vec![];
    for (f, tcp) in [("tcp", true), ("tcp6", true), ("udp", false), ("udp6", false)] {
        if let Ok(t) = std::fs::read_to_string(format!("/proc/net/{f}")) {
            v.extend(parse_table(&t, tcp));
        }
    }
    v.sort_by_key(|p| (p.port, !p.tcp, p.addr.is_ipv6()));
    v
}

/// Which of `pids` hold each socket inode open.
pub fn socket_owners(inodes: &HashSet<u64>, pids: &[u32]) -> HashMap<u64, Vec<u32>> {
    let mut out: HashMap<u64, Vec<u32>> = HashMap::new();
    for &pid in pids {
        let Ok(rd) = std::fs::read_dir(format!("/proc/{pid}/fd")) else { continue };
        for fd in rd.flatten() {
            let Ok(t) = std::fs::read_link(fd.path()) else { continue };
            let t = t.to_string_lossy();
            if let Some(n) = t.strip_prefix("socket:[").and_then(|s| s.strip_suffix(']')).and_then(|s| s.parse().ok()) {
                if inodes.contains(&n) && !out.get(&n).is_some_and(|v| v.contains(&pid)) {
                    out.entry(n).or_default().push(pid);
                }
            }
        }
    }
    out
}

/// Processes test builds may signal: only children the test started itself.
#[cfg(test)]
pub static OURS: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());
/// Signals a test build was asked to send to anything else, never sent.
#[cfg(test)]
pub static REFUSED: std::sync::Mutex<Vec<(u32, i32)>> = std::sync::Mutex::new(Vec::new());

fn signal(pid: u32, sig: i32) -> bool {
    #[cfg(test)]
    if !OURS.lock().is_ok_and(|o| o.contains(&pid)) {
        if let Ok(mut r) = REFUSED.lock() {
            r.push((pid, sig));
        }
        return false;
    }
    unsafe { libc::kill(pid as i32, sig) == 0 }
}

/// Send `sig` to `p` unless its PID went to another process meanwhile, and wait up to
/// `wait` for it to end. True when it is gone.
pub fn stop(p: &Proc, sig: i32, wait: Duration) -> bool {
    if !alive(p) {
        return true;
    }
    if !signal(p.pid, sig) {
        return !alive(p);
    }
    let t0 = Instant::now();
    while t0.elapsed() < wait {
        if !alive(p) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !alive(p)
}

// ------------------------------------------------------------------ the dialog

pub enum What {
    Paths(Vec<PathBuf>),
    Ports,
}

/// One row: a process holding the thing asked about (with how, for paths), or a port and
/// the process listening on it (None when it belongs to someone whose processes can't
/// be read).
#[derive(Clone, Debug)]
pub struct Row {
    pub proc: Option<Proc>,
    pub hold: Option<Hold>,
    pub port: Option<Port>,
}

/// Asked before a stop: the process, and whether it is the forced one.
struct Ask {
    proc: Proc,
    force: bool,
}

pub struct State {
    pub what: What,
    /// What failed because of these processes, when that is why the list opened.
    pub why: Option<String>,
    pub filter: String,
    focus: bool,
    pub rows: Vec<Row>,
    pub sel: usize,
    scan: Option<mpsc::Receiver<Vec<Row>>>,
    pub scanned: bool,
    ask: Option<Ask>,
    /// Stops under way: the process, whether forced, and the answer (gone or not).
    stopping: Vec<(Proc, bool, mpsc::Receiver<bool>)>,
    /// Still running after SIGTERM: Force Stop is offered for these.
    pub stubborn: Vec<Proc>,
    pub said: Option<String>,
}

pub fn open(ctx: &egui::Context, what: What, why: Option<String>) -> Popup {
    let mut st = State { what, why, filter: String::new(), focus: true, rows: vec![], sel: 0, scan: None, scanned: false, ask: None, stopping: vec![], stubborn: vec![], said: None };
    st.rescan(ctx);
    Popup::Holders(Box::new(st))
}

impl State {
    fn rescan(&mut self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let paths = match &self.what {
            What::Paths(p) => Some(p.clone()),
            What::Ports => None,
        };
        let ctx = ctx.clone();
        let _ = std::thread::Builder::new().name("holders".into()).spawn(move || {
            let pids = pids();
            let rows = match paths {
                Some(paths) => holding(&paths, &pids).into_iter().map(|(p, h)| Row { proc: Some(p), hold: Some(h), port: None }).collect(),
                None => {
                    let ports = listening();
                    let owners = socket_owners(&ports.iter().map(|p| p.inode).collect(), &pids);
                    let mut known: HashMap<u32, Option<Proc>> = HashMap::new();
                    let mut rows = vec![];
                    for port in ports {
                        let mut procs: Vec<Proc> = owners.get(&port.inode).into_iter().flatten().filter_map(|&pid| known.entry(pid).or_insert_with(|| info(pid)).clone()).collect();
                        // A server's workers share its socket: the first started is the one to show.
                        procs.sort_by_key(|p| p.start);
                        // The user's own processes can all be read: a socket of theirs held by
                        // none of them is Maia's.
                        if procs.is_empty() && port.uid == unsafe { libc::getuid() } {
                            continue;
                        }
                        rows.push(Row { proc: procs.into_iter().next(), hold: None, port: Some(port) });
                    }
                    rows
                }
            };
            let _ = tx.send(rows);
            ctx.request_repaint();
        });
        self.scan = Some(rx);
    }

    /// The rows the filter keeps (ports: by number or program).
    pub fn shown(&self) -> Vec<usize> {
        let q = self.filter.trim().to_lowercase();
        (0..self.rows.len())
            .filter(|&i| {
                let r = &self.rows[i];
                q.is_empty() || r.port.as_ref().is_some_and(|p| p.port.to_string().starts_with(&q)) || r.proc.as_ref().is_some_and(|p| p.name.to_lowercase().contains(&q))
            })
            .collect()
    }

    fn start_stop(&mut self, p: Proc, force: bool, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let (q, ctx2) = (p.clone(), ctx.clone());
        let _ = std::thread::Builder::new().name("stop".into()).spawn(move || {
            let gone = stop(&q, if force { libc::SIGKILL } else { libc::SIGTERM }, if force { Duration::from_secs(1) } else { GRACE });
            let _ = tx.send(gone);
            ctx2.request_repaint();
        });
        self.stubborn.retain(|s| s.pid != p.pid);
        self.stopping.push((p, force, rx));
    }

    pub fn stopping(&self, pid: u32) -> bool {
        self.stopping.iter().any(|s| s.0.pid == pid)
    }
}

/// A small button inside a row: (clicked, its rect).
/// `key`: the chosen row's button shows the key that presses it, as a cap inside.
#[allow(clippy::too_many_arguments)]
fn row_button(ui: &mut egui::Ui, right: f32, cy: f32, label: &str, danger: bool, what: &'static str, pal: &crate::theme::Palette, small: f32, key: bool) -> (bool, Rect) {
    let font = FontId::proportional(small);
    let g = ui.painter().layout_no_wrap(label.to_string(), font, pal.text);
    let kfont = FontId::proportional(10.0);
    let kw = if key { crate::kit::caps_layout(ui.painter(), "Enter", 0.0, 0.0, &kfont, pal).1 + 6.0 } else { 0.0 };
    let r = Rect::from_min_max(pos2(right - g.size().x - 16.0 - kw, cy - g.size().y / 2.0 - 3.0), pos2(right, cy + g.size().y / 2.0 + 3.0));
    let resp = ui.interact(r, egui::Id::new(("holder button", what, r.min.y as i32)), Sense::click());
    crate::ui::note_control(ui, r, what);
    let fill = if resp.hovered() { pal.hover } else { pal.card() };
    ui.painter().rect(r, CornerRadius::same(5), fill, Stroke::new(1.0_f32, if danger { pal.danger } else { pal.card_line() }), StrokeKind::Inside);
    let col = if danger { pal.danger } else { pal.text };
    let gw = g.size().x;
    ui.painter().galley(pos2(r.left() + 8.0, r.center().y - g.size().y / 2.0), g, col);
    if key {
        let (caps, _) = crate::kit::caps_layout(ui.painter(), "Enter", r.left() + 8.0 + gw + 6.0, r.center().y, &kfont, pal);
        for (cr, c) in caps {
            crate::kit::keycap(ui.painter(), cr, &c, kfont.clone(), crate::kit::held(ui.ctx(), &c), pal);
        }
    }
    (resp.clicked(), r)
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

/// How a process holds what was asked about, the path written from there: the title
/// already names it.
fn hold_text(h: &Hold, asked: &[PathBuf]) -> String {
    let (Hold::Open(p) | Hold::Inside(p) | Hold::Runs(p)) = h;
    let what = match asked.iter().find(|a| p.starts_with(a)) {
        Some(a) if a == p => "it".to_string(),
        Some(a) => crate::project::relative(p, a),
        None => name_of(p),
    };
    match h {
        Hold::Open(_) => format!("has {what} open"),
        Hold::Inside(_) => format!("working in {what}"),
        Hold::Runs(_) => format!("running {what}"),
    }
}

/// The list: what holds the paths (or listens on ports), each of the user's own with a
/// Stop button; a stop asks first, and a process that outlasts SIGTERM is offered Force Stop.
pub fn popup(app: &mut App, ctx: &egui::Context, st: &mut State) -> bool {
    let pal = app.pal;
    let k = pp::take_keys(ctx);
    if k.esc {
        if st.ask.take().is_some() {
            return true;
        }
        return false;
    }
    if let Some(rx) = &st.scan {
        if let Ok(rows) = rx.try_recv() {
            st.rows = rows;
            st.scan = None;
            st.scanned = true;
        }
    }
    let mut again = false;
    let mut i = 0;
    while i < st.stopping.len() {
        match st.stopping[i].2.try_recv() {
            Ok(gone) => {
                let (p, force, _) = st.stopping.remove(i);
                if gone {
                    st.said = Some(format!("Stopped {} ({})", p.name, p.pid));
                } else if force {
                    st.said = Some(format!("{} ({}) couldn't be stopped", p.name, p.pid));
                } else {
                    st.said = Some(format!("{} ({}) is still running", p.name, p.pid));
                    st.stubborn.push(p.clone());
                    if st.ask.is_none() {
                        st.ask = Some(Ask { proc: p, force: true });
                    }
                }
                again = true;
            }
            Err(mpsc::TryRecvError::Empty) => i += 1,
            Err(_) => {
                st.stopping.remove(i);
            }
        }
    }
    if again {
        st.rescan(ctx);
    }
    let rows = st.shown();
    if st.ask.is_none() {
        pp::step(&mut st.sel, rows.len(), &k, |_| false);
    }
    let sel_proc = rows.get(st.sel).and_then(|&i| st.rows[i].proc.clone()).filter(|p| p.mine() && !st.stopping(p.pid));
    let mut confirm = st.ask.is_some() && k.enter;
    let mut ask: Option<Ask> = match (&st.ask, &sel_proc) {
        (None, Some(p)) if k.enter => Some(Ask { force: st.stubborn.iter().any(|s| s.pid == p.pid), proc: p.clone() }),
        _ => None,
    };
    let refresh_key = crate::devtools::take(ctx, egui::Modifiers::COMMAND, egui::Key::R);
    let (mut cancel_ask, mut refresh, mut close) = (false, refresh_key, false);
    let fs = app.cfg.font_size;
    let small = fs - 1.5;
    let ports = matches!(st.what, What::Ports);
    let list_max = pp::fit_list(ctx, 420.0, if ports { 200.0 } else { 170.0 });
    let title = match &st.what {
        What::Ports => "What's using a port".to_string(),
        What::Paths(p) if p.len() == 1 => format!("What's using “{}”", name_of(&p[0])),
        What::Paths(p) => format!("What's using {} items", p.len()),
    };
    let sub = match (&st.why, &st.what) {
        (Some(w), _) => Some(w.clone()),
        (None, What::Paths(p)) if p.len() == 1 => Some(pp::home_short(&p[0])),
        _ => None,
    };
    let mut reset_sel = false;
    let asked: Vec<PathBuf> = match &st.what {
        What::Paths(p) => p.clone(),
        What::Ports => vec![],
    };
    let asked = asked.as_slice();
    pp::centered(ctx, "holders", 640.0, &pal, |ui| {
        pp::dialog_title(ui, &title, None, &pal);
        if let Some(s) = &sub {
            let col = if st.why.is_some() { pal.danger } else { pal.text_dim };
            ui.add(egui::Label::new(egui::RichText::new(s).color(col)).wrap());
        }
        ui.add_space(6.0);
        if ports {
            let r = pp::search_field(ui, &mut st.filter, "Filter by port…", &mut st.focus, &pal, crate::theme::Glyph::Search);
            crate::ui::note_control(ui, r.rect, "port filter");
            reset_sel = r.changed();
        }
        let row_h = (fs * 1.35 + 12.0).round();
        let n = rows.len();
        if n > 0 {
            let mut area = egui::ScrollArea::vertical().id_salt("holders list").max_height(list_max).min_scrolled_height((n as f32 * row_h).min(list_max)).auto_shrink([false, true]);
            if k.up || k.down || k.page_up || k.page_down {
                let most = (n as f32 * row_h - list_max).max(0.0);
                area = area.vertical_scroll_offset((st.sel as f32 * row_h - list_max / 2.0 + row_h / 2.0).clamp(0.0, most));
            }
            area.show_rows(ui, row_h, n, |ui, range| {
                for j in range {
                    let row = &st.rows[rows[j]];
                    let on = j == st.sel;
                    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                    // Chosen quietly, the accent's mark at its edge (`kit::chosen`); a
                    // high-contrast theme keeps its solid selection.
                    let hot = resp.hovered() || ui.rect_contains_pointer(r);
                    if on {
                        crate::kit::chosen(ui.painter(), r, 6, &pal);
                    } else if hot {
                        ui.painter().rect_filled(r, CornerRadius::same(6), crate::kit::touch(&pal));
                    }
                    if resp.clicked() {
                        st.sel = j;
                    }
                    let (tc, dc) = if on && pal.strong { (pal.on_sel(), pal.on_sel_dim()) } else { (pal.text, pal.text_dim) };
                    let cy = r.center().y;
                    // Right to left: Stop (for the user's own, on the row chosen or pointed
                    // at, the chosen one's with its key), then whose it is.
                    let mut x = r.right() - 8.0;
                    if let Some(p) = row.proc.as_ref().filter(|p| p.mine()).filter(|_| on || hot) {
                        if st.stopping(p.pid) {
                            crate::ui::spinner(ui.painter(), pos2(x - 10.0, cy), 5.5, pal.accent, ui.input(|i| i.time));
                            ui.ctx().request_repaint_after(Duration::from_millis(50));
                            x -= 28.0;
                        } else {
                            let force = st.stubborn.iter().any(|s| s.pid == p.pid);
                            let (hit, br) = row_button(ui, x, cy, if force { "Force Stop" } else { "Stop" }, force, "holder stop", &pal, small, on);
                            if hit && st.ask.is_none() {
                                ask = Some(Ask { proc: p.clone(), force });
                                st.sel = j;
                            }
                            x = br.left() - 12.0;
                        }
                    }
                    // Whose it is, as a small tag.
                    let whose = match (&row.proc, &row.port) {
                        (Some(p), _) if p.mine() => "Yours".to_string(),
                        (Some(p), _) => crate::inspector::user_name(p.uid),
                        (None, Some(port)) => crate::inspector::user_name(port.uid),
                        _ => String::new(),
                    };
                    if !whose.is_empty() {
                        let wg = ui.painter().layout_no_wrap(whose, FontId::proportional(small - 0.5), pal.text_dim);
                        let tr = Rect::from_min_size(pos2(x - wg.size().x - 12.0, cy - wg.size().y / 2.0 - 2.0), wg.size() + vec2(12.0, 4.0));
                        ui.painter().rect(tr, CornerRadius::same(4), pal.card(), Stroke::new(1.0, pal.card_line()), StrokeKind::Inside);
                        ui.painter().galley(tr.min + vec2(6.0, 2.0), wg, pal.text_dim);
                        x = tr.left();
                    }
                    let mut left = r.left() + 12.0;
                    if let Some(port) = &row.port {
                        let pg = crate::listing::tabular(ui, &port.port.to_string(), FontId::proportional(fs), 120.0, tc);
                        let col_w = ui.painter().layout_no_wrap("00000".into(), FontId::proportional(fs), tc).size().x;
                        ui.painter().galley(pos2(left, cy - pg.size().y / 2.0), pg, tc);
                        left += col_w + 10.0;
                        let addr = match port.addr {
                            a if a.is_unspecified() => "any address".to_string(),
                            a => a.to_string(),
                        };
                        let proto = format!("{} · {addr}", if port.tcp { "TCP" } else { "UDP" });
                        let w = (r.width() * 0.3).min(220.0);
                        let ag = crate::listing::elided(ui, &proto, FontId::proportional(small), w, dc);
                        ui.painter().galley(pos2(left, cy - ag.size().y / 2.0), ag, dc);
                        left += w + 10.0;
                    }
                    let room = x - 12.0 - left;
                    // The name, its PID in a column of its own (even digits, so a list of them
                    // lines up), then what it is doing with the item.
                    let name = match &row.proc {
                        Some(p) => p.name.clone(),
                        None => "Another user's program".to_string(),
                    };
                    let ng = crate::listing::elided(ui, &name, FontId::proportional(fs), room.max(20.0) * 0.4, if row.proc.is_some() { tc } else { dc });
                    let nw = ng.size().x;
                    ui.painter().galley(pos2(left, cy - ng.size().y / 2.0), ng, tc);
                    if let Some(p) = &row.proc {
                        let pid_x = left + nw.max(room * 0.22) + 10.0;
                        let pid_w = ui.painter().layout_no_wrap("PID 0000000".into(), FontId::proportional(small), dc).size().x;
                        if pid_x + pid_w < x - 12.0 {
                            let pg = crate::listing::tabular(ui, &format!("PID {}", p.pid), FontId::proportional(small), pid_w, dc);
                            ui.painter().galley(pos2(pid_x + pid_w - pg.size().x, cy - pg.size().y / 2.0), pg, dc);
                            let hx = pid_x + pid_w + 12.0;
                            if let Some(h) = row.hold.as_ref().filter(|_| x - 12.0 - hx > 24.0) {
                                let dg = crate::listing::elided(ui, &hold_text(h, asked), FontId::proportional(small), x - 12.0 - hx, dc);
                                ui.painter().galley(pos2(hx, cy - dg.size().y / 2.0), dg, dc);
                            }
                        }
                    }
                }
            });
        } else {
            let msg = if !st.scanned {
                "Looking…"
            } else if !st.rows.is_empty() {
                "No port like that"
            } else if ports {
                "Nothing is listening on a port"
            } else {
                "Nothing of yours is using it. Other users' programs can only be seen with administrator access."
            };
            ui.add_space(4.0);
            ui.add(egui::Label::new(egui::RichText::new(msg).color(pal.text_dim)).wrap());
            ui.add_space(4.0);
        }
        ui.add_space(4.0);
        match &st.ask {
            Some(a) => {
                let q = if a.force {
                    format!("Force {} ({}) to stop? Anything it hasn't saved is lost.", a.proc.name, a.proc.pid)
                } else {
                    format!("Stop {} ({})? It may lose what it hasn't saved.", a.proc.name, a.proc.pid)
                };
                ui.add(egui::Label::new(egui::RichText::new(q).color(pal.text)).wrap());
                pp::dialog_buttons(ui, |ui| {
                    let label = if a.force { "Force Stop" } else { "Stop" };
                    let b = ui.add(egui::Button::new(egui::RichText::new(label).color(Color32::WHITE)).fill(pal.danger).min_size(vec2(72.0, 0.0)));
                    crate::ui::note_control(ui, b.rect, "holder confirm");
                    confirm |= b.clicked();
                    let c = pp::plain_button(ui, "Cancel", &pal);
                    crate::ui::note_control(ui, c.rect, "holder cancel");
                    cancel_ask = c.clicked();
                });
            }
            None => {
                if let Some(s) = &st.said {
                    ui.label(egui::RichText::new(s).size(small).color(pal.text_dim));
                }
                pp::footer(ui, &[("Enter", "stop"), ("Ctrl+R", "refresh"), ("Esc", "close")], &pal, |ui| {
                    let c = pp::plain_button(ui, "Close", &pal);
                    crate::ui::note_control(ui, c.rect, "holder close");
                    close = c.clicked();
                    let r = pp::plain_button(ui, "Refresh", &pal);
                    crate::ui::note_control(ui, r.rect, "holder refresh");
                    refresh |= r.clicked();
                });
            }
        }
    });
    if reset_sel {
        st.sel = 0;
    }
    if close {
        return false;
    }
    if cancel_ask {
        st.ask = None;
    } else if confirm {
        if let Some(a) = st.ask.take() {
            st.start_stop(a.proc, a.force, ctx);
        }
    } else if let Some(a) = ask {
        st.ask = Some(a);
    }
    if refresh && st.scan.is_none() {
        st.said = None;
        st.rescan(ctx);
    }
    true
}

/// The paths a failed job couldn't change because something was using them.
pub fn is_busy(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(libc::EBUSY)
}

/// Whether a tool's message says the target was busy ("target is busy", udisks'
/// DeviceBusy, "Device or resource busy").
pub fn says_busy(msg: &str) -> bool {
    let m = msg.to_lowercase();
    m.contains("busy")
}
