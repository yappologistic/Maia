//! One Maia per desktop session: a second launch hands its folders to the window that is
//! already open, which shows them as new tabs, and exits. The first launch listens on a
//! socket in the runtime directory. Records are `kind\0value\0`, so any file name works.

use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;

/// What a later launch asked for.
#[derive(Debug, PartialEq)]
pub enum Request {
    /// Show these folders (and files, in their folder) as new tabs.
    Open(Vec<PathBuf>),
    /// Launched without paths: bring the window forward.
    Focus,
    /// Show these in their folders, selected ("Show in folder" from another app).
    Show(Vec<PathBuf>),
    /// Show these files' properties.
    Properties(Vec<PathBuf>),
}

/// The socket for this session (one per display, so two sessions of one user are apart).
pub fn socket_path() -> PathBuf {
    let uid = unsafe { libc::getuid() };
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let display = std::env::var("WAYLAND_DISPLAY").or_else(|_| std::env::var("DISPLAY")).unwrap_or_default().replace('/', "_");
    dir.join(format!("maia-{uid}-{display}.sock"))
}

/// Hand `paths` to the Maia listening at `sock`. True when it took them.
pub fn forward_to(sock: &Path, paths: &[PathBuf]) -> bool {
    send(sock, &if paths.is_empty() { Request::Focus } else { Request::Open(paths.to_vec()) })
}

/// `r` as the socket's records.
pub fn encode(r: &Request) -> Vec<u8> {
    let (kind, paths): (&[u8], &[PathBuf]) = match r {
        Request::Focus => return b"focus\0\0".to_vec(),
        Request::Open(p) => (b"open", p),
        Request::Show(p) => (b"show", p),
        Request::Properties(p) => (b"props", p),
    };
    let mut msg = vec![];
    for p in paths {
        msg.extend_from_slice(kind);
        msg.push(0);
        msg.extend_from_slice(p.as_os_str().as_bytes());
        msg.push(0);
    }
    msg
}

/// Hand `r` to the Maia listening at `sock`. True when it took it.
pub fn send(sock: &Path, r: &Request) -> bool {
    let Ok(mut s) = UnixStream::connect(sock) else { return false };
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
    let msg = encode(r);
    if s.write_all(&msg).is_err() || s.shutdown(std::net::Shutdown::Write).is_err() {
        return false;
    }
    let mut reply = [0u8; 2];
    s.read_exact(&mut reply).is_ok() && &reply == b"ok"
}

/// Become the listening instance at `sock`: a socket nobody answers on is left over from
/// a crash and is replaced. None when another Maia already listens there.
pub fn claim_at(sock: &Path) -> Option<UnixListener> {
    match UnixListener::bind(sock) {
        Ok(l) => Some(l),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if UnixStream::connect(sock).is_ok() {
                return None;
            }
            std::fs::remove_file(sock).ok()?;
            UnixListener::bind(sock).ok()
        }
        Err(_) => None,
    }
}

pub fn parse(msg: &[u8]) -> Option<Request> {
    let mut it = msg.split(|b| *b == 0);
    let (mut paths, mut show, mut props) = (vec![], vec![], vec![]);
    let mut focus = false;
    while let Some(kind) = it.next() {
        if kind.is_empty() {
            continue;
        }
        let value = it.next()?;
        match kind {
            b"open" if !value.is_empty() => paths.push(PathBuf::from(std::ffi::OsStr::from_bytes(value))),
            b"show" if !value.is_empty() => show.push(PathBuf::from(std::ffi::OsStr::from_bytes(value))),
            b"props" if !value.is_empty() => props.push(PathBuf::from(std::ffi::OsStr::from_bytes(value))),
            b"focus" => focus = true,
            _ => {}
        }
    }
    if !paths.is_empty() {
        Some(Request::Open(paths))
    } else if !show.is_empty() {
        Some(Request::Show(show))
    } else if !props.is_empty() {
        Some(Request::Properties(props))
    } else if focus {
        Some(Request::Focus)
    } else {
        None
    }
}

/// Answer later launches on a worker thread until the listener goes away.
pub fn serve(listener: UnixListener, tx: Sender<Request>, wake: impl Fn() + Send + 'static) {
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { continue };
            let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
            let mut msg = vec![];
            // A client that never finishes its message must not hold the next one up.
            if Read::by_ref(&mut s).take(1 << 20).read_to_end(&mut msg).is_err() {
                continue;
            }
            if let Some(r) = parse(&msg) {
                if tx.send(r).is_err() {
                    return;
                }
                wake();
                let _ = s.write_all(b"ok");
            }
        }
    });
}
