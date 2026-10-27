//! Send To Phone: the selection served over the local network for five minutes, from an
//! address a phone's camera reads off a QR code. A one-shot HTTP server on a worker,
//! listening on this machine's LAN address only, on a port the system picks, under a
//! path with 128 random bits in it: anyone else on the network finds nothing. One file is
//! served as it is; several get a small page of links. A folder goes as a zip that
//! bsdtar writes straight into the connection, when bsdtar is installed.
//!
//! The server stops when the popup closes, at the end of the five minutes, or with Maia.
//! Connections are served by `serve`, over any reader and writer, so it can be driven
//! without a network.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Color32, CornerRadius, FontId, Key, Modifiers, Rect, Sense, Vec2, pos2, vec2};

use crate::app::App;
use crate::popups as pp;

/// How long the selection stays shared.
pub const SPAN: Duration = Duration::from_secs(300);
/// Connections served at once; more wait for the system to hand them over.
const AT_ONCE: usize = 8;

#[derive(Clone)]
pub struct Item {
    pub path: PathBuf,
    pub name: String,
    /// A folder, sent as a zip.
    pub dir: bool,
    /// Bytes, for a file.
    pub size: u64,
}

/// What the server tells the window.
#[derive(Default)]
pub struct Shared {
    /// The address to open, once listening; or why there is none.
    pub url: Option<Result<String, String>>,
    /// Each device and the items it has fetched whole.
    pub got: BTreeMap<IpAddr, BTreeSet<usize>>,
    /// The command that lets phones through the firewall, while it turns them away.
    pub blocked: Option<String>,
}

/// Pictures sent without where they were taken and the camera's details, while `on`
/// (it can be switched with the popup open).
pub struct Clean {
    pub on: AtomicBool,
    /// exiftool, for the kinds Maia can't clean itself.
    pub tools: Option<crate::drives::Tools>,
}

/// The running server: dropped, it stops.
pub struct Server {
    stop: Arc<AtomicBool>,
    pub shared: Arc<Mutex<Shared>>,
    pub clean: Arc<Clean>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
    }
}

/// The address to listen on: a private IPv4 address of an interface that is up, a
/// wireless or wired one first, never a container's, a VM's or a VPN's bridge.
pub fn lan_address() -> Option<Ipv4Addr> {
    #[cfg(test)]
    if let Some(ip) = *TEST_LAN.lock().unwrap_or_else(|e| e.into_inner()) {
        return Some(ip);
    }
    let mut found: Vec<(String, Ipv4Addr)> = vec![];
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 {
            return None;
        }
        let mut cur = ifap;
        while !cur.is_null() {
            let ifa = &*cur;
            let up = ifa.ifa_flags & libc::IFF_UP as u32 != 0 && ifa.ifa_flags & libc::IFF_LOOPBACK as u32 == 0;
            if up && !ifa.ifa_addr.is_null() && (*ifa.ifa_addr).sa_family as i32 == libc::AF_INET {
                let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                let name = std::ffi::CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned();
                found.push((name, ip));
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(ifap);
    }
    pick(&found)
}

/// Test builds: the address to listen on in place of the network's (the sandbox has
/// only its loopback), so a simulation can fetch as a phone would.
#[cfg(test)]
pub static TEST_LAN: Mutex<Option<Ipv4Addr>> = Mutex::new(None);

/// The best of the interfaces' addresses for a phone on the same network.
pub fn pick(found: &[(String, Ipv4Addr)]) -> Option<Ipv4Addr> {
    const VIRTUAL: [&str; 12] = ["docker", "br-", "veth", "virbr", "lxc", "lxd", "tun", "tap", "wg", "tailscale", "zt", "vbox"];
    found
        .iter()
        .filter(|(n, ip)| ip.is_private() && !VIRTUAL.iter().any(|v| n.starts_with(v)) && !n.starts_with("vmnet"))
        .min_by_key(|(n, ip)| {
            let kind = if n.starts_with("wl") { 0 } else if n.starts_with("en") || n.starts_with("eth") { 1 } else { 2 };
            let range = match ip.octets() {
                [192, 168, ..] => 0,
                [10, ..] => 1,
                _ => 2,
            };
            (kind, range)
        })
        .map(|x| x.1)
}

/// The port tried first. The same one every time, so a firewall that turns phones away
/// needs to be told about it only once; a random one when it is taken.
pub const PORT: u16 = 47250;

/// The command that lets phones in, when the firewall is known to drop what comes to
/// `port`; None when it lets them in or can't be asked. Only reads: changing the firewall
/// is the user's call.
pub fn firewall_blocks(t: &crate::drives::Tools, port: u16) -> Option<String> {
    let read = |rel: &str| std::fs::read_to_string(t.etc(rel)).unwrap_or_default();
    let conf = read("ufw/ufw.conf");
    if conf.lines().any(|l| l.trim() == "ENABLED=yes") {
        let policy = read("default/ufw");
        let drops = policy.lines().find_map(|l| l.trim().strip_prefix("DEFAULT_INPUT_POLICY=")).is_none_or(|v| v.trim_matches('"') != "ACCEPT");
        return (drops && !ufw_allows(&read("ufw/user.rules"), port)).then(|| format!("sudo ufw allow {port}/tcp"));
    }
    if t.has("firewall-cmd") && exits_with(t, &["--state"]) == Some(0) && exits_with(t, &[&format!("--query-port={port}/tcp")]) == Some(1) {
        return Some(format!("sudo firewall-cmd --permanent --add-port={port}/tcp && sudo firewall-cmd --reload"));
    }
    None
}

/// Whether ufw's own rules accept TCP to `port`: a rule for that port or a range or list
/// holding it, or one for every port (allowing a whole network, say).
pub fn ufw_allows(rules: &str, port: u16) -> bool {
    let holds = |spec: &str| {
        spec.split(',').any(|p| match p.split_once(':') {
            Some((a, b)) => a.parse::<u16>().is_ok_and(|a| a <= port) && b.parse::<u16>().is_ok_and(|b| port <= b),
            None => p.parse::<u16>() == Ok(port),
        })
    };
    rules.lines().any(|l| {
        let w: Vec<&str> = l.split_whitespace().collect();
        if w.first() != Some(&"-A") || w.get(1) != Some(&"ufw-user-input") || !w.windows(2).any(|p| p == ["-j", "ACCEPT"]) {
            return false;
        }
        let arg = |k: &str| w.windows(2).find(|p| p[0] == k).map(|p| p[1]);
        if arg("-p").is_some_and(|p| p != "tcp" && p != "all") {
            return false;
        }
        match (arg("--dport"), arg("--dports")) {
            (Some(d), _) | (None, Some(d)) => holds(d),
            (None, None) => true,
        }
    })
}

/// The exit code of `firewall-cmd args`, waited for two seconds at most: its service can
/// hang, and a hung one can't be asked.
fn exits_with(t: &crate::drives::Tools, args: &[&str]) -> Option<i32> {
    let mut c = crate::media::tool_cmd(t.cmd("firewall-cmd"));
    c.args(args).stdout(std::process::Stdio::null());
    let mut child = c.spawn().ok()?;
    let t0 = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) => return st.code(),
            Ok(None) if t0.elapsed() < Duration::from_secs(2) => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// 128 random bits, written in letters and digits a phone's camera reads back well.
fn token() -> Option<String> {
    let mut b = [0u8; 16];
    let n = unsafe { libc::getrandom(b.as_mut_ptr().cast(), b.len(), 0) };
    if n != 16 {
        return None;
    }
    const ABC: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::with_capacity(26);
    let (mut acc, mut bits) = (0u32, 0);
    for x in b {
        acc = acc << 8 | x as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ABC[(acc >> bits) as usize & 31] as char);
        }
    }
    if bits > 0 {
        out.push(ABC[(acc << (5 - bits)) as usize & 31] as char);
    }
    Some(out)
}

/// Start sharing `items`, folders only when `zip` can pack them.
pub fn start(items: Vec<Item>, zip: Option<crate::drives::Tools>, tools: Option<crate::drives::Tools>, clean_on: bool, ctx: &egui::Context) -> Server {
    let stop = Arc::new(AtomicBool::new(false));
    let shared = Arc::new(Mutex::new(Shared::default()));
    let clean = Arc::new(Clean { on: AtomicBool::new(clean_on), tools: tools.clone() });
    let (s2, sh2, c2, cl2) = (stop.clone(), shared.clone(), ctx.clone(), clean.clone());
    let _ = std::thread::Builder::new().name("send to phone".into()).spawn(move || {
        let fail = |e: &str| {
            sh2.lock().unwrap_or_else(|e| e.into_inner()).url = Some(Err(e.to_string()));
            c2.request_repaint();
        };
        let Some(ip) = lan_address() else { return fail("This computer is on no local network: join the phone's Wi-Fi network or plug in a cable.") };
        let Some(tok) = token() else { return fail("Couldn't make a private address") };
        let l = match TcpListener::bind((ip, PORT)).or_else(|_| TcpListener::bind((ip, 0))) {
            Ok(l) => l,
            Err(e) => return fail(&format!("Couldn't listen on {ip}: {e}")),
        };
        let port = l.local_addr().map(|a| a.port()).unwrap_or(0);
        if l.set_nonblocking(true).is_err() {
            return fail("Couldn't listen");
        }
        let blocked = tools.as_ref().and_then(|t| firewall_blocks(t, port));
        {
            let mut s = sh2.lock().unwrap_or_else(|e| e.into_inner());
            s.url = Some(Ok(format!("http://{ip}:{port}/{tok}/")));
            s.blocked = blocked;
        }
        c2.request_repaint();
        let (items, tok, busy) = (Arc::new(items), Arc::new(tok), Arc::new(AtomicUsize::new(0)));
        let end = Instant::now() + SPAN;
        // While the firewall is in the way, asked again now and then, so allowing the port
        // with the popup open clears the warning.
        let mut asked = Instant::now();
        while !s2.load(Relaxed) && Instant::now() < end {
            if asked.elapsed() >= Duration::from_secs(2) && sh2.lock().unwrap_or_else(|e| e.into_inner()).blocked.is_some() {
                asked = Instant::now();
                let b = tools.as_ref().and_then(|t| firewall_blocks(t, port));
                if b.is_none() {
                    sh2.lock().unwrap_or_else(|e| e.into_inner()).blocked = None;
                    c2.request_repaint();
                }
            }
            match l.accept() {
                Ok((conn, peer)) if busy.load(Relaxed) < AT_ONCE => {
                    let (items, tok, busy, stop, shared, ctx, zip, clean) = (items.clone(), tok.clone(), busy.clone(), s2.clone(), sh2.clone(), c2.clone(), zip.clone(), cl2.clone());
                    busy.fetch_add(1, Relaxed);
                    let _ = std::thread::Builder::new().name("send to phone conn".into()).spawn(move || {
                        let _ = conn.set_nonblocking(false);
                        let _ = conn.set_read_timeout(Some(Duration::from_secs(10)));
                        let _ = conn.set_write_timeout(Some(Duration::from_secs(30)));
                        let mut got = |k: usize| {
                            shared.lock().unwrap_or_else(|e| e.into_inner()).got.entry(peer.ip()).or_default().insert(k);
                            ctx.request_repaint();
                        };
                        let r = conn.try_clone().map(BufReader::new);
                        if let Ok(mut r) = r {
                            let mut w = &conn;
                            serve(&mut r, &mut w, &items, &tok, zip.as_ref(), &clean, &stop, &mut got);
                        }
                        let _ = conn.shutdown(std::net::Shutdown::Both);
                        busy.fetch_sub(1, Relaxed);
                    });
                }
                // Too many at once: this one is turned away rather than kept waiting.
                Ok((conn, _)) => drop::<TcpStream>(conn),
                Err(_) => std::thread::sleep(Duration::from_millis(60)),
            }
        }
        s2.store(true, Relaxed);
        c2.request_repaint();
    });
    Server { stop, shared, clean }
}

/// One request read from `r` and answered on `w`: the page, an item, or nothing at all
/// for a path without the token. `got` hears of each item sent whole.
pub fn serve(r: &mut impl BufRead, w: &mut impl Write, items: &[Item], tok: &str, zip: Option<&crate::drives::Tools>, clean: &Clean, stop: &AtomicBool, got: &mut dyn FnMut(usize)) {
    let mut line = String::new();
    if r.by_ref().take(4096).read_line(&mut line).is_err() {
        return;
    }
    // The headers, up to the blank line; nothing in them matters here.
    let mut seen = 0;
    loop {
        let mut h = String::new();
        match r.by_ref().take(8192).read_line(&mut h) {
            Ok(0) | Err(_) => break,
            Ok(n) => seen += n,
        }
        if h == "\r\n" || h == "\n" || seen > 32 << 10 {
            break;
        }
    }
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let head = method == "HEAD";
    if method != "GET" && !head {
        let _ = w.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    }
    let path = target.split('?').next().unwrap_or("");
    // The whole token, then a slash or nothing: "/TOKEN0" is not "/TOKEN/0".
    let Some(rest) = path.strip_prefix('/').and_then(|p| p.strip_prefix(tok)).filter(|r| r.is_empty() || r.starts_with('/')) else {
        let _ = w.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    };
    let rest = rest.trim_matches('/');
    let pick = if rest.is_empty() { (items.len() == 1).then_some(0) } else { rest.split('/').next().and_then(|n| n.parse::<usize>().ok()).filter(|&k| k < items.len()) };
    match pick {
        Some(k) => {
            if send_item(w, &items[k], zip, clean, stop, head) {
                got(k);
            }
        }
        None if rest.is_empty() => {
            let body = page(items, tok);
            let _ = write!(w, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", body.len());
            if !head {
                let _ = w.write_all(body.as_bytes());
            }
        }
        None => {
            let _ = w.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    }
}

/// The name a phone saves an item under, for the header: "report.pdf", a folder's with
/// ".zip".
fn saved_name(it: &Item) -> String {
    if it.dir { format!("{}.zip", it.name) } else { it.name.clone() }
}

/// A name in a header, the way every browser reads it (RFC 5987).
fn disposition(name: &str) -> String {
    let ascii: String = name.chars().map(|c| if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' { c } else { '_' }).collect();
    let mut enc = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            enc.push(b as char);
        } else {
            enc.push_str(&format!("%{b:02X}"));
        }
    }
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{enc}")
}

/// Send one item; true when it went whole.
fn send_item(w: &mut impl Write, it: &Item, zip: Option<&crate::drives::Tools>, clean: &Clean, stop: &AtomicBool, head: bool) -> bool {
    let name = saved_name(it);
    let ext = it.path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
    if !it.dir && clean.on.load(Relaxed) && crate::scrub::applies(&ext) {
        // Cleaned in memory; one that can't be is not sent rather than sent whole.
        let Ok(b) = crate::scrub::clean(&it.path, clean.tools.as_ref()) else {
            let _ = w.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            return false;
        };
        let _ = write!(w, "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nContent-Disposition: {}\r\nConnection: close\r\n\r\n", b.len(), disposition(&name));
        return !head && w.write_all(&b).and_then(|_| w.flush()).is_ok();
    }
    if it.dir {
        let Some(tools) = zip else { return false };
        let _ = write!(w, "HTTP/1.1 200 OK\r\nContent-Type: application/zip\r\nContent-Disposition: {}\r\nConnection: close\r\n\r\n", disposition(&name));
        if head {
            return false;
        }
        // The zip is written as it is read, so its length isn't known: the connection
        // closing ends it.
        let parent = it.path.parent().map(PathBuf::from).unwrap_or_default();
        let mut c = crate::media::tool_cmd(tools.cmd("bsdtar"));
        c.env("LC_ALL", "C.UTF-8").arg("-cf").arg("-").arg("--format").arg("zip").arg("-C").arg(&parent).arg("--").arg(&it.name).stdout(std::process::Stdio::piped());
        let Ok(mut child) = c.spawn() else { return false };
        let ok = match child.stdout.take() {
            Some(mut out) => pump(&mut out, w, stop),
            None => false,
        };
        if !ok {
            let _ = child.kill();
        }
        return child.wait().is_ok_and(|s| s.success()) && ok;
    }
    let Ok(mut f) = std::fs::File::open(&it.path) else {
        let _ = w.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return false;
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(it.size);
    let _ = write!(w, "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {len}\r\nContent-Disposition: {}\r\nConnection: close\r\n\r\n", disposition(&name));
    !head && pump(&mut f, w, stop)
}

/// Copy until the end, or false when the phone went away or sharing stopped.
fn pump(r: &mut impl Read, w: &mut impl Write, stop: &AtomicBool) -> bool {
    let mut buf = vec![0u8; 256 << 10];
    loop {
        if stop.load(Relaxed) {
            return false;
        }
        match r.read(&mut buf) {
            Ok(0) => return w.flush().is_ok(),
            Ok(n) => {
                if w.write_all(&buf[..n]).is_err() {
                    return false;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The page several items get: each a link, with its size.
pub fn page(items: &[Item], tok: &str) -> String {
    let mut rows = String::new();
    for (k, it) in items.iter().enumerate() {
        let size = if it.dir { "folder, as a zip".to_string() } else { crate::fsx::fmt_size(it.size) };
        rows.push_str(&format!("<a href=\"/{tok}/{k}/\" download=\"{}\"><b>{}</b><small>{size}</small></a>\n", esc(&saved_name(it)), esc(&saved_name(it))));
    }
    format!(
        "<!doctype html><html><head><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\"><title>From Maia</title>\
<style>body{{font:16px system-ui,sans-serif;margin:0;padding:16px;background:#f4f4f6;color:#222}}a{{display:flex;justify-content:space-between;gap:12px;padding:14px 16px;margin:0 0 8px;background:#fff;border-radius:12px;color:inherit;text-decoration:none}}b{{font-weight:500;overflow-wrap:anywhere}}small{{color:#777;white-space:nowrap}}@media(prefers-color-scheme:dark){{body{{background:#17171a;color:#eee}}a{{background:#24242a}}small{{color:#999}}}}</style></head><body>\n{rows}</body></html>\n"
    )
}

// ------------------------------------------------------------------ the popup

pub struct State {
    pub items: Vec<Item>,
    /// Folders left out: bsdtar isn't there to zip them.
    pub left_out: usize,
    pub server: Server,
    opened: Instant,
    /// The code for the address, once it is known.
    code: Option<(String, crate::qr::Code)>,
}

/// Send To Phone for the selection.
pub fn open(app: &mut App) {
    let t = app.tab();
    let sel: Vec<crate::fsx::Entry> = t.target_entries().into_iter().cloned().collect();
    if sel.is_empty() || crate::tab::is_place(&t.path) || t.archive.is_some() {
        app.toast("Select what to send");
        return;
    }
    let tools = crate::drives::Tools::current();
    let zip = tools.clone().filter(|t| t.has("bsdtar"));
    let mut items = vec![];
    let mut left_out = 0;
    for e in sel {
        if e.is_dir && zip.is_none() {
            left_out += 1;
            continue;
        }
        // Sizes of folders aren't counted for this: the zip's size is known only once sent.
        items.push(Item { path: e.path.clone(), name: e.name.clone(), dir: e.is_dir, size: if e.is_dir { 0 } else { e.size } });
    }
    if items.is_empty() {
        app.toast("Folders can be sent once bsdtar is installed (it zips them)");
        return;
    }
    let ctx = app.ctx.clone();
    let server = start(items.clone(), zip, tools, app.cfg.share_without_place, &ctx);
    app.popup = pp::Popup::Phone(Box::new(State { items, left_out, server, opened: Instant::now(), code: None }));
}

/// The popup. Returns false when it closes, which stops the server.
pub fn popup(app: &mut App, ctx: &egui::Context, st: &mut State) -> bool {
    let pal = app.pal;
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        return false;
    }
    let left = SPAN.saturating_sub(st.opened.elapsed());
    if left.is_zero() {
        app.toast("Stopped sending to the phone after five minutes");
        return false;
    }
    let (url, got, blocked) = {
        let s = st.server.shared.lock().unwrap_or_else(|e| e.into_inner());
        (s.url.clone(), s.got.iter().map(|(ip, k)| (*ip, k.len())).collect::<Vec<_>>(), s.blocked.clone())
    };
    if let Some(Ok(u)) = &url {
        if st.code.as_ref().is_none_or(|c| c.0 != *u) {
            st.code = crate::qr::encode(u.as_bytes()).map(|c| (u.clone(), c));
        }
    }
    let fs = app.cfg.font_size;
    let small = FontId::proportional(fs - 1.5);
    let n = st.items.len();
    let total: u64 = st.items.iter().map(|i| i.size).sum();
    let mut copy = None;
    let mut toggled = None;
    let (_, rect) = pp::centered(ctx, "send to phone", 600.0, &pal, |ui| {
        let side = 216.0f32.min(ui.available_width() * 0.45).round();
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 20.0;
            let (qr, _) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
            match (&url, &st.code) {
                (Some(Ok(_)), Some((_, code))) => paint_code(ui.painter(), qr, code),
                (Some(Err(_)), _) => {
                    ui.painter().rect_filled(qr, CornerRadius::same(10), pal.card());
                    crate::theme::paint_symbol(ui.painter(), Rect::from_center_size(qr.center(), Vec2::splat(40.0)), "smartphone", pal.text_dim);
                }
                _ => {
                    ui.painter().rect_filled(qr, CornerRadius::same(10), pal.card());
                    crate::ui::spinner(ui.painter(), qr.center(), 8.0, pal.text_dim, ui.input(|i| i.time));
                    ui.ctx().request_repaint();
                }
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                let what = if n == 1 { saved_name(&st.items[0]) } else { format!("{n} items") };
                pp::dialog_title(ui, &what, (total > 0).then(|| crate::fsx::fmt_size(total)).as_deref(), &pal);
                ui.add_space(6.0);
                if n > 1 {
                    for it in st.items.iter().take(5) {
                        ui.horizontal(|ui| {
                            let w = ui.available_width();
                            let sz = if it.dir { "zip".to_string() } else { crate::fsx::fmt_size(it.size) };
                            let sg = ui.painter().layout_no_wrap(sz, small.clone(), pal.text_dim);
                            let (r, _) = ui.allocate_exact_size(vec2(w, fs + 6.0), Sense::hover());
                            let ng = crate::listing::elided(ui, &saved_name(it), FontId::proportional(fs), w - sg.size().x - 12.0, pal.text);
                            ui.painter().galley(pos2(r.left(), r.center().y - ng.size().y / 2.0), ng, pal.text);
                            ui.painter().galley(pos2(r.right() - sg.size().x, r.center().y - sg.size().y / 2.0), sg, pal.text_dim);
                        });
                    }
                    if n > 5 {
                        ui.label(egui::RichText::new(format!("and {} more", n - 5)).font(small.clone()).color(pal.text_dim));
                    }
                    ui.add_space(6.0);
                }
                // Photos: with or without where they were taken and the camera's details.
                let ext = |i: &Item| i.path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
                let pics: Vec<&Item> = st.items.iter().filter(|i| !i.dir && crate::scrub::applies(&ext(i))).collect();
                if !pics.is_empty() {
                    let mut on = st.server.clean.on.load(Relaxed);
                    ui.horizontal(|ui| {
                        if pp::switch(ui, &mut on, &pal) {
                            st.server.clean.on.store(on, Relaxed);
                            toggled = Some(on);
                        }
                        ui.label(egui::RichText::new("Leave out place and camera").font(small.clone()).color(pal.text));
                    });
                    let stuck = pics.iter().filter(|i| !crate::scrub::can(&ext(i), st.server.clean.tools.as_ref())).count();
                    if on && stuck > 0 {
                        let what = if stuck == 1 { "1 picture isn't".to_string() } else { format!("{stuck} pictures aren't") };
                        ui.add(egui::Label::new(egui::RichText::new(format!("{what} sent: cleaning HEIC and raw photos needs exiftool")).font(small.clone()).color(pal.text_dim)).wrap());
                    }
                }
                if st.left_out > 0 {
                    let f = if st.left_out == 1 { "1 folder is" } else { "Some folders are" };
                    ui.add(egui::Label::new(egui::RichText::new(format!("{f} left out: zipping folders needs bsdtar")).font(small.clone()).color(pal.text_dim)).wrap());
                }
                match &url {
                    Some(Ok(u)) => {
                        let l = ui.add(egui::Label::new(egui::RichText::new(u.trim_end_matches('/')).font(FontId::monospace(fs - 2.0)).color(pal.text)).truncate().sense(Sense::click()));
                        crate::ui::note_control(ui, l.rect, "phone address");
                        if l.on_hover_text("Copy the address").clicked() {
                            copy = Some((u.clone(), "Copied the address"));
                        }
                        match &blocked {
                            None => {
                                ui.label(egui::RichText::new("Point the phone's camera at the code; it must be on this network.").font(small.clone()).color(pal.text_dim));
                            }
                            Some(cmd) => {
                                ui.add(egui::Label::new(egui::RichText::new("The firewall turns phones away. Let them in once, in a terminal:").font(small.clone()).color(pal.danger)).wrap());
                                let c = ui.add(egui::Label::new(egui::RichText::new(cmd).font(FontId::monospace(fs - 2.0)).color(pal.text)).wrap().sense(Sense::click()));
                                crate::ui::note_control(ui, c.rect, "firewall command");
                                if c.on_hover_text("Copy the command").clicked() {
                                    copy = Some((cmd.clone(), "Copied the command"));
                                }
                            }
                        }
                    }
                    Some(Err(e)) => {
                        ui.add(egui::Label::new(egui::RichText::new(e).color(pal.danger)).wrap());
                    }
                    None => {}
                }
                for (ip, k) in &got {
                    ui.label(egui::RichText::new(format!("{ip} downloaded {k} of {n}")).font(small.clone()).color(pal.accent));
                }
            });
        });
        // The time left, draining from the right.
        if matches!(url, Some(Ok(_))) {
            ui.add_space(12.0);
            let secs = left.as_secs();
            let tg = ui.painter().layout_no_wrap(format!("{}:{:02}", secs / 60, secs % 60), small.clone(), pal.text_dim);
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), tg.size().y.max(8.0)), Sense::hover());
            let bar = Rect::from_min_max(pos2(r.left(), r.center().y - 2.0), pos2(r.right() - tg.size().x - 10.0, r.center().y + 2.0));
            ui.painter().rect_filled(bar, CornerRadius::same(2), pal.border);
            let f = left.as_secs_f32() / SPAN.as_secs_f32();
            ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * f, bar.height())), CornerRadius::same(2), pal.accent);
            ui.painter().galley(pos2(r.right() - tg.size().x, r.center().y - tg.size().y / 2.0), tg, pal.text_dim);
        }
        pp::hints(ui, &[("Esc", "stop sending")], &pal);
    });
    if let Some(on) = toggled {
        app.cfg.share_without_place = on;
        app.cfg_dirty = true;
    }
    if let Some((text, said)) = copy {
        ctx.copy_text(text);
        app.toast(said);
    }
    // The bar moves a pixel or so a second; nothing needs drawing in between.
    ctx.request_repaint_after(Duration::from_millis(500));
    !pp::clicked_outside(ctx, rect)
}

/// The code on white with its quiet zone, whatever the theme: a camera wants dark on
/// light. Each module is a whole number of pixels on the pixel grid, so no seam shows
/// between them; runs of dark modules in a row are one rectangle each.
fn paint_code(p: &egui::Painter, r: Rect, code: &crate::qr::Code) {
    let quiet = 4;
    let ppp = p.ctx().pixels_per_point();
    let cells = (code.size + 2 * quiet) as f32;
    let m = ((r.width() * ppp / cells).floor().max(1.0)) / ppp;
    let side = m * cells;
    let c = r.center();
    let min = pos2(((c.x - side / 2.0) * ppp).round() / ppp, ((c.y - side / 2.0) * ppp).round() / ppp);
    let card = Rect::from_min_size(min, Vec2::splat(side));
    p.rect_filled(card, CornerRadius::same(8), Color32::WHITE);
    let ink = Color32::from_gray(16);
    let at = card.min + vec2(quiet as f32 * m, quiet as f32 * m);
    // One mesh of plain quads: rects each get a soft edge, and where rows meet the edges
    // leave light seams through the dark, which a camera reads worse.
    let mut mesh = egui::Mesh::default();
    for y in 0..code.size {
        let mut x = 0;
        while x < code.size {
            if !code.get(x, y) {
                x += 1;
                continue;
            }
            let start = x;
            while x < code.size && code.get(x, y) {
                x += 1;
            }
            let rr = Rect::from_min_size(at + vec2(start as f32 * m, y as f32 * m), vec2((x - start) as f32 * m, m));
            mesh.add_colored_rect(rr, ink);
        }
    }
    p.add(egui::Shape::mesh(mesh));
}
