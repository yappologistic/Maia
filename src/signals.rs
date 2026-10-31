//! Process signals. SIGTERM/SIGHUP/SIGINT (logout, shutdown, `kill`) close the window the
//! normal way so the session is saved; if the UI doesn't finish within a few seconds, or a
//! second signal arrives, the process exits anyway. Fatal signals (crashes, aborts) append a
//! line to `crash.log` in the state dir before the default action (core dump) runs.

use std::ffi::CString;
use std::io::Write as _;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

static WAKE_FD: AtomicI32 = AtomicI32::new(-1);
static QUITS: AtomicU32 = AtomicU32::new(0);
static LOG_PATH: OnceLock<CString> = OnceLock::new();

/// Grace period between a quit signal and a forced exit.
const GRACE_MS: i32 = 3000;

fn log_path() -> std::path::PathBuf {
    crate::config::state_dir().join("crash.log")
}

/// Append a timestamped line to the crash log (normal context, not for signal handlers).
pub fn log(msg: &str) {
    let p = log_path();
    let _ = std::fs::create_dir_all(p.parent().unwrap_or(&p));
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = writeln!(f, "{secs} {msg}");
    }
    eprintln!("maia: {msg}");
}

extern "C" fn on_quit(sig: libc::c_int) {
    if QUITS.fetch_add(1, Ordering::Relaxed) >= 1 {
        unsafe { libc::_exit(128 + sig) };
    }
    let fd = WAKE_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        unsafe { libc::write(fd, [sig as u8].as_ptr().cast(), 1) };
    }
}

/// Async-signal-safe: only open/write/close, digits formatted by hand.
extern "C" fn on_fatal(sig: libc::c_int) {
    if let Some(path) = LOG_PATH.get() {
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_APPEND | libc::O_CREAT | libc::O_CLOEXEC, 0o600) };
        if fd >= 0 {
            let mut buf = [0u8; 64];
            let mut n = 0;
            let num = |buf: &mut [u8; 64], n: &mut usize, mut v: u64| {
                let mut d = [0u8; 20];
                let mut i = d.len();
                loop {
                    i -= 1;
                    d[i] = b'0' + (v % 10) as u8;
                    v /= 10;
                    if v == 0 {
                        break;
                    }
                }
                for &c in &d[i..] {
                    buf[*n] = c;
                    *n += 1;
                }
            };
            let now = unsafe { libc::time(std::ptr::null_mut()) }.max(0) as u64;
            num(&mut buf, &mut n, now);
            for &c in b" fatal signal " {
                buf[n] = c;
                n += 1;
            }
            num(&mut buf, &mut n, sig as u64);
            buf[n] = b'\n';
            n += 1;
            unsafe {
                libc::write(fd, buf.as_ptr().cast(), n);
                libc::close(fd);
            }
        }
    }
    // SA_RESETHAND restored the default action; re-raise it so the core dump still happens.
    unsafe { libc::raise(sig) };
}

/// Install the handlers. `quit` is called (from a helper thread) on the first quit signal.
pub fn install(quit: impl Fn() + Send + 'static) {
    if let Ok(c) = CString::new(log_path().into_os_string().into_encoded_bytes()) {
        let _ = LOG_PATH.set(c);
    }
    let _ = std::fs::create_dir_all(crate::config::state_dir());
    unsafe {
        let mut fds = [0; 2];
        if libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) == 0 {
            WAKE_FD.store(fds[1], Ordering::Relaxed);
            let rd = fds[0];
            std::thread::spawn(move || {
                let mut b = [0u8; 1];
                loop {
                    let r = libc::read(rd, b.as_mut_ptr().cast(), 1);
                    if r == 1 {
                        break;
                    }
                    if r < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    return;
                }
                quit();
                // The UI should be gone well before this; if it's wedged, don't ignore the signal.
                let mut pfd = libc::pollfd { fd: rd, events: libc::POLLIN, revents: 0 };
                libc::poll(&mut pfd, 1, GRACE_MS);
                libc::_exit(128 + b[0] as i32);
            });
            for sig in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
                let mut sa: libc::sigaction = std::mem::zeroed();
                sa.sa_sigaction = on_quit as extern "C" fn(libc::c_int) as usize;
                sa.sa_flags = libc::SA_RESTART;
                libc::sigemptyset(&mut sa.sa_mask);
                libc::sigaction(sig, &sa, std::ptr::null_mut());
            }
        }
        for sig in [libc::SIGSEGV, libc::SIGILL, libc::SIGBUS, libc::SIGFPE, libc::SIGABRT] {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_fatal as extern "C" fn(libc::c_int) as usize;
            sa.sa_flags = libc::SA_RESETHAND | libc::SA_NODEFER | libc::SA_ONSTACK;
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
    }
}

/// Unrecoverable startup failure (no display, no OpenGL…): say why in the crash log, on
/// stderr and as a desktop notification (launchers swallow stderr), then exit.
pub fn fatal(what: &str, why: &dyn std::fmt::Display) -> ! {
    let why = why.to_string();
    log(&format!("{what}: {why}"));
    let body = if why.is_empty() { what.to_string() } else { format!("{what}: {why}") };
    if let Ok(mut c) = std::process::Command::new("notify-send").args(["-a", "Maia", "-u", "critical", "Maia couldn't start", &body]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn() {
        let _ = c.wait();
    }
    std::process::exit(1)
}

/// `expect` that ends in `fatal` instead of an abort.
pub trait OrDie<T> {
    fn or_die(self, what: &str) -> T;
}

impl<T, E: std::fmt::Display> OrDie<T> for Result<T, E> {
    fn or_die(self, what: &str) -> T {
        self.unwrap_or_else(|e| fatal(what, &e))
    }
}

impl<T> OrDie<T> for Option<T> {
    fn or_die(self, what: &str) -> T {
        self.unwrap_or_else(|| fatal(what, &""))
    }
}
