//! `org.freedesktop.FileManager1` on the session bus: what browsers and other apps call
//! for "Show in folder". Maia answers it while it runs, and, while it is the default
//! file manager, a service file in the user's own D-Bus folder (read before the
//! system's) has the bus start Maia for it. A few hundred lines of the D-Bus wire
//! format stand in for a D-Bus library: one connection, one name, three methods.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use crate::single::Request;

pub const NAME: &str = "org.freedesktop.FileManager1";
const PATH: &str = "/org/freedesktop/FileManager1";

const INTROSPECT: &str = r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node>
 <interface name="org.freedesktop.FileManager1">
  <method name="ShowFolders"><arg type="as" name="URIs" direction="in"/><arg type="s" name="StartupId" direction="in"/></method>
  <method name="ShowItems"><arg type="as" name="URIs" direction="in"/><arg type="s" name="StartupId" direction="in"/></method>
  <method name="ShowItemProperties"><arg type="as" name="URIs" direction="in"/><arg type="s" name="StartupId" direction="in"/></method>
 </interface>
 <interface name="org.freedesktop.DBus.Introspectable"><method name="Introspect"><arg type="s" name="xml" direction="out"/></method></interface>
 <interface name="org.freedesktop.DBus.Peer"><method name="Ping"/></interface>
</node>"#;

// ------------------------------------------------------------------ the wire format

/// A message as far as Maia reads one.
#[derive(Debug, Default, PartialEq)]
pub struct Msg {
    pub kind: u8,
    pub serial: u32,
    pub reply_to: Option<u32>,
    pub path: String,
    pub interface: String,
    pub member: String,
    pub sender: String,
    pub signature: String,
    pub error: String,
    pub body: Vec<u8>,
}

fn pad(v: &mut Vec<u8>, to: usize) {
    while v.len() % to != 0 {
        v.push(0);
    }
}

fn put_u32(v: &mut Vec<u8>, n: u32) {
    pad(v, 4);
    v.extend_from_slice(&n.to_le_bytes());
}

pub(crate) fn put_str(v: &mut Vec<u8>, s: &str) {
    put_u32(v, s.len() as u32);
    v.extend_from_slice(s.as_bytes());
    v.push(0);
}

fn put_sig(v: &mut Vec<u8>, s: &str) {
    v.push(s.len() as u8);
    v.extend_from_slice(s.as_bytes());
    v.push(0);
}

/// A whole message: header (little-endian, version 1), its fields, then the body.
pub fn build(kind: u8, serial: u32, fields: &[(u8, char, &str)], reply_to: Option<u32>, body_sig: &str, body: &[u8]) -> Vec<u8> {
    let mut v = vec![b'l', kind, 0, 1];
    v.extend_from_slice(&(body.len() as u32).to_le_bytes());
    v.extend_from_slice(&serial.to_le_bytes());
    let mut f = vec![];
    let field = |f: &mut Vec<u8>, code: u8, sig: &str, write: &dyn Fn(&mut Vec<u8>)| {
        // Offsets count from the message's start: the array begins at 16.
        let mut whole: Vec<u8> = vec![0; 16];
        whole.extend_from_slice(f);
        pad(&mut whole, 8);
        whole.push(code);
        put_sig(&mut whole, sig);
        write(&mut whole);
        f.clear();
        f.extend_from_slice(&whole[16..]);
    };
    for (code, ty, val) in fields {
        match ty {
            'g' => field(&mut f, *code, "g", &|w| put_sig(w, val)),
            'o' => field(&mut f, *code, "o", &|w| put_str(w, val)),
            _ => field(&mut f, *code, "s", &|w| put_str(w, val)),
        }
    }
    if let Some(r) = reply_to {
        field(&mut f, 5, "u", &|w| put_u32(w, r));
    }
    if !body_sig.is_empty() {
        field(&mut f, 8, "g", &|w| put_sig(w, body_sig));
    }
    v.extend_from_slice(&(f.len() as u32).to_le_bytes());
    v.extend_from_slice(&f);
    pad(&mut v, 8);
    v.extend_from_slice(body);
    v
}

/// Reads values out of a body (or a header) at D-Bus alignments.
struct Cur<'a> {
    b: &'a [u8],
    at: usize,
}

impl Cur<'_> {
    fn align(&mut self, n: usize) {
        self.at = self.at.div_ceil(n) * n;
    }
    fn u32(&mut self) -> Option<u32> {
        self.align(4);
        let v = u32::from_le_bytes(self.b.get(self.at..self.at + 4)?.try_into().ok()?);
        self.at += 4;
        Some(v)
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        let s = String::from_utf8_lossy(self.b.get(self.at..self.at + n)?).into_owned();
        self.at += n + 1;
        Some(s)
    }
    fn sig(&mut self) -> Option<String> {
        let n = *self.b.get(self.at)? as usize;
        let s = String::from_utf8_lossy(self.b.get(self.at + 1..self.at + 1 + n)?).into_owned();
        self.at += n + 2;
        Some(s)
    }
}

/// The number in a variant (unwrapping variants in variants) after `skip` strings at the
/// start of a body: what a portal setting says.
pub fn variant_u32(body: &[u8], skip: usize) -> Option<u32> {
    let mut c = Cur { b: body, at: 0 };
    for _ in 0..skip {
        c.str()?;
    }
    loop {
        match c.sig()?.as_str() {
            "v" => continue,
            "u" => return c.u32(),
            _ => return None,
        }
    }
}

/// The strings of an `as` at the start of a body.
pub fn strings(body: &[u8]) -> Vec<String> {
    let mut c = Cur { b: body, at: 0 };
    let Some(len) = c.u32() else { return vec![] };
    let end = c.at + len as usize;
    let mut v = vec![];
    while c.at < end {
        match c.str() {
            Some(s) => v.push(s),
            None => break,
        }
    }
    v
}

/// Read one whole message; None at the end of the stream or on nonsense.
pub fn read(r: &mut impl Read) -> Option<Msg> {
    let mut head = [0u8; 16];
    r.read_exact(&mut head).ok()?;
    if head[0] != b'l' {
        // Big-endian peers exist in theory; the bus speaks in the reader's order to us.
        return None;
    }
    let body_len = u32::from_le_bytes(head[4..8].try_into().ok()?) as usize;
    let serial = u32::from_le_bytes(head[8..12].try_into().ok()?);
    let fields_len = u32::from_le_bytes(head[12..16].try_into().ok()?) as usize;
    if body_len > 64 << 20 || fields_len > 1 << 20 {
        return None;
    }
    let mut rest = vec![0u8; (16 + fields_len).div_ceil(8) * 8 - 16 + body_len];
    r.read_exact(&mut rest).ok()?;
    let mut whole = head.to_vec();
    whole.extend_from_slice(&rest);
    let mut m = Msg { kind: head[1], serial, ..Default::default() };
    let mut c = Cur { b: &whole, at: 16 };
    let end = 16 + fields_len;
    while c.at < end {
        c.align(8);
        if c.at >= end {
            break;
        }
        let code = *whole.get(c.at)?;
        c.at += 1;
        // Read by the value's type, so a field this doesn't know is passed over.
        match c.sig()?.as_str() {
            "s" | "o" => {
                let v = c.str()?;
                match code {
                    1 => m.path = v,
                    2 => m.interface = v,
                    3 => m.member = v,
                    4 => m.error = v,
                    7 => m.sender = v,
                    _ => {}
                }
            }
            "g" => {
                let v = c.sig()?;
                if code == 8 {
                    m.signature = v;
                }
            }
            "u" => {
                let v = c.u32()?;
                if code == 5 {
                    m.reply_to = Some(v);
                }
            }
            _ => return None,
        }
    }
    let at = (16 + fields_len).div_ceil(8) * 8;
    m.body = whole.get(at..at + body_len)?.to_vec();
    Some(m)
}

// ------------------------------------------------------------------ the connection

/// A connection to a bus, authenticated as this user.
pub struct Bus {
    s: UnixStream,
    serial: u32,
    /// Calls to us that came while a call of ours waited for its answer: the one the bus
    /// started us for arrives right after we take the name, before RequestName returns.
    held: std::collections::VecDeque<Msg>,
}

impl Bus {
    /// Connect to `address` (`unix:path=…` or `unix:abstract=…`).
    pub fn connect(address: &str) -> Option<Bus> {
        let s = address.split(';').find_map(|a| {
            let rest = a.strip_prefix("unix:")?;
            for kv in rest.split(',') {
                if let Some(p) = kv.strip_prefix("path=") {
                    return UnixStream::connect(p).ok();
                }
                if let Some(n) = kv.strip_prefix("abstract=") {
                    use std::os::linux::net::SocketAddrExt;
                    let addr = std::os::unix::net::SocketAddr::from_abstract_name(n.as_bytes()).ok()?;
                    return UnixStream::connect_addr(&addr).ok();
                }
            }
            None
        })?;
        let mut b = Bus { s, serial: 0, held: Default::default() };
        b.auth()?;
        b.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "Hello", "", &[])?;
        Some(b)
    }

    fn auth(&mut self) -> Option<()> {
        let uid = unsafe { libc::getuid() }.to_string();
        let hex: String = uid.bytes().map(|b| format!("{b:02x}")).collect();
        self.s.write_all(format!("\0AUTH EXTERNAL {hex}\r\n").as_bytes()).ok()?;
        let mut line = vec![];
        let mut one = [0u8; 1];
        while !line.ends_with(b"\r\n") && line.len() < 512 {
            self.s.read_exact(&mut one).ok()?;
            line.push(one[0]);
        }
        line.starts_with(b"OK ").then_some(())?;
        self.s.write_all(b"BEGIN\r\n").ok()
    }

    fn next(&mut self) -> u32 {
        self.serial += 1;
        self.serial
    }

    pub fn send(&mut self, m: &[u8]) -> Option<()> {
        self.s.write_all(m).ok()
    }

    /// The next message that comes (a signal asked for with AddMatch).
    pub fn recv(&mut self) -> Option<Msg> {
        self.held.pop_front().or_else(|| read(&mut self.s))
    }

    /// The socket and the last serial used: for a service that reads on one thread and
    /// answers from others (the Open and Save dialog).
    pub fn into_parts(self) -> (UnixStream, u32, std::collections::VecDeque<Msg>) {
        (self.s, self.serial, self.held)
    }

    /// Call a method and wait for its answer. Signals before it are skipped; calls to us
    /// are held for `recv`, as the one the bus started us for can come first.
    pub fn call(&mut self, dest: &str, path: &str, iface: &str, member: &str, sig: &str, body: &[u8]) -> Option<Msg> {
        let serial = self.next();
        let m = build(1, serial, &[(1, 'o', path), (2, 's', iface), (3, 's', member), (6, 's', dest)], None, sig, body);
        self.send(&m)?;
        loop {
            let r = read(&mut self.s)?;
            if r.reply_to == Some(serial) {
                return (r.kind == 2).then_some(r);
            }
            if r.kind == 1 {
                self.held.push_back(r);
            }
        }
    }

    /// Ask for a well-known name; true when it is ours.
    pub fn own(&mut self, name: &str) -> bool {
        let mut body = vec![];
        put_str(&mut body, name);
        // Take it from an owner that allows that; never wait in line behind one that doesn't.
        put_u32(&mut body, 0x2 | 0x4);
        self.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName", "su", &body)
            .and_then(|r| Cur { b: &r.body, at: 0 }.u32())
            .is_some_and(|code| code == 1 || code == 4)
    }

    fn reply(&mut self, to: &Msg, sig: &str, body: &[u8]) -> Option<()> {
        let serial = self.next();
        let m = build(2, serial, &[(6, 's', &to.sender)], Some(to.serial), sig, body);
        self.send(&m)
    }

    fn error(&mut self, to: &Msg, name: &str, text: &str) -> Option<()> {
        let serial = self.next();
        let mut body = vec![];
        put_str(&mut body, text);
        let m = build(3, serial, &[(4, 's', name), (6, 's', &to.sender)], Some(to.serial), "s", &body);
        self.send(&m)
    }
}

/// `file://` URIs (and plain paths) as paths; others (`smb://`…) are not on this disk.
pub fn paths(uris: &[String]) -> Vec<PathBuf> {
    uris.iter().filter_map(|u| if u.starts_with('/') { Some(PathBuf::from(u)) } else { crate::clipboard::uri_to_path(u) }).collect()
}

/// What a call asks of the window, if it is one of ours.
pub fn request(m: &Msg) -> Option<Request> {
    if m.kind != 1 || m.path != PATH || (m.interface != NAME && !m.interface.is_empty()) {
        return None;
    }
    let p = paths(&strings(&m.body));
    match m.member.as_str() {
        "ShowFolders" => Some(Request::Open(p)),
        "ShowItems" => Some(Request::Show(p)),
        "ShowItemProperties" => Some(Request::Properties(p)),
        _ => None,
    }
}

/// Answer calls on `bus` until it closes, handing what they ask to `tx`.
pub fn answer(mut bus: Bus, tx: Sender<Request>, wake: impl Fn()) {
    while let Some(m) = bus.recv() {
        if m.kind != 1 {
            continue;
        }
        let ok = match (m.interface.as_str(), m.member.as_str()) {
            ("org.freedesktop.DBus.Introspectable", "Introspect") => {
                let mut body = vec![];
                put_str(&mut body, INTROSPECT);
                bus.reply(&m, "s", &body)
            }
            ("org.freedesktop.DBus.Peer", "Ping") => bus.reply(&m, "", &[]),
            _ => match request(&m) {
                Some(r) => {
                    let sent = tx.send(r).is_ok();
                    wake();
                    if sent { bus.reply(&m, "", &[]) } else { bus.error(&m, "org.freedesktop.DBus.Error.Failed", "Maia is closing") }
                }
                None => bus.error(&m, "org.freedesktop.DBus.Error.UnknownMethod", "No such method"),
            },
        };
        if ok.is_none() {
            return;
        }
    }
}

// ------------------------------------------------------------------ being started for it

/// Where the session bus looks first for the program to start for a name.
pub fn service_path() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| crate::fsx::home().join(".local/share")).join("dbus-1/services").join(format!("{NAME}.service"))
}

pub fn service_file(exe: &Path) -> String {
    format!("[D-BUS Service]\nName={NAME}\nExec={} --fm1\n", exe.display())
}

/// `maia --fm1`, what the bus starts for the name while no window has it: it answers
/// without a window, as a file manager's service does, and hands each call to the open
/// window, starting one only when a call asks to be shown something. A dock that only
/// looks the name up at login (Dash to Dock does, for its trash and drives) opens
/// nothing. It keeps the name for the session: the bus runs it in a unit of its own,
/// which would take the windows it started with it if it ended.
pub fn run_backend() -> bool {
    let Some(mut bus) = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok().and_then(|a| Bus::connect(&a)) else { return false };
    if !bus.own(NAME) {
        return false;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || rx.into_iter().for_each(hand_over));
    answer(bus, tx, || {});
    let _ = worker.join();
    true
}

/// Give `r` to the open window, or start one for it, in a scope of its own.
fn hand_over(r: Request) {
    let sock = crate::single::socket_path();
    if crate::single::send(&sock, &r) {
        return;
    }
    let paths = match &r {
        Request::Open(p) | Request::Show(p) | Request::Properties(p) => p.clone(),
        Request::Focus => vec![],
    };
    let exe = exe();
    let scoped = std::process::Command::new("systemd-run").args(["--user", "--scope", "--collect", "--quiet", "--"]).arg(&exe).args(&paths).stdin(std::process::Stdio::null()).spawn();
    if scoped.is_err() {
        let _ = std::process::Command::new(&exe).args(&paths).stdin(std::process::Stdio::null()).spawn();
    }
    // A new window shows the folders; properties are asked for once it listens.
    if matches!(r, Request::Properties(_)) {
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if crate::single::send(&sock, &r) {
                break;
            }
        }
    }
}

/// Have the bus start Maia for "Show in folder" (or no longer): written while Maia is the
/// default file manager, then the bus reads its folders again.
pub fn set_service(on: bool, exe: &Path) -> std::io::Result<()> {
    let p = service_path();
    if on {
        let want = service_file(exe);
        if std::fs::read_to_string(&p).ok().as_deref() != Some(want.as_str()) {
            if let Some(d) = p.parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::write(&p, want)?;
            reload();
        }
    } else if std::fs::read_to_string(&p).is_ok_and(|t| t.contains("Exec=") && t.contains("maia")) {
        std::fs::remove_file(&p)?;
        reload();
    }
    Ok(())
}

fn reload() {
    if let Some(mut b) = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok().and_then(|a| Bus::connect(&a)) {
        b.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "ReloadConfig", "", &[]);
    }
}

/// The program the bus should start: the installed copy when there is one.
pub fn exe() -> PathBuf {
    let installed = dirs::executable_dir().unwrap_or_else(|| crate::fsx::home().join(".local/bin")).join("maia");
    if installed.is_file() { installed } else { std::env::current_exe().unwrap_or(installed) }
}

/// On a thread of its own: keep the service file in step with being the default file
/// manager, take the name, and answer it for as long as Maia runs.
pub fn serve(tx: Sender<Request>, wake: impl Fn() + Send + 'static) {
    let _ = std::thread::Builder::new().name("filemanager1".into()).spawn(move || {
        let _ = set_service(crate::install::is_default(), &exe());
        let Some(mut bus) = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok().and_then(|a| Bus::connect(&a)) else { return };
        if bus.own(NAME) {
            answer(bus, tx, wake);
        }
    });
}
