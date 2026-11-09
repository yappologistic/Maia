//! Live folder updates via raw inotify, coalesced.

use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MASK: u32 = libc::IN_CREATE | libc::IN_DELETE | libc::IN_MOVED_FROM | libc::IN_MOVED_TO | libc::IN_CLOSE_WRITE | libc::IN_ATTRIB | libc::IN_DELETE_SELF | libc::IN_MOVE_SELF | libc::IN_MODIFY;

pub struct Watch {
    fd: i32,
    wds: Arc<Mutex<HashMap<i32, PathBuf>>>,
    by_path: HashMap<PathBuf, i32>,
    rx: Receiver<(PathBuf, Option<PathBuf>)>,
    /// Directory -> time of first unprocessed change.
    dirty: HashMap<PathBuf, Instant>,
    pub changed_files: Vec<PathBuf>,
}

impl Watch {
    pub fn new(ctx: egui::Context) -> Watch {
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
        let wds: Arc<Mutex<HashMap<i32, PathBuf>>> = Arc::new(Mutex::new(HashMap::new()));
        let (tx, rx) = channel();
        if fd >= 0 {
            let wds2 = wds.clone();
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
                    if n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                        continue;
                    }
                    // Done when reading fails or the Watch is gone (its drop wakes this read).
                    if n <= 0 || Arc::strong_count(&wds2) == 1 {
                        unsafe { libc::close(fd) };
                        return;
                    }
                    let mut off = 0usize;
                    let hdr = std::mem::size_of::<libc::inotify_event>();
                    while off + hdr <= n as usize {
                        let ev: libc::inotify_event = unsafe { std::ptr::read_unaligned(buf.as_ptr().add(off) as *const libc::inotify_event) };
                        let name_bytes = &buf[off + hdr..off + hdr + ev.len as usize];
                        let name = name_bytes.split(|b| *b == 0).next().unwrap_or(&[]);
                        off += hdr + ev.len as usize;
                        if ev.mask & libc::IN_Q_OVERFLOW != 0 {
                            // The kernel dropped events: which folder changed can't be
                            // known, so every watched one is to be looked at again.
                            let dirs: Vec<PathBuf> = wds2.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
                            for dir in dirs {
                                if tx.send((dir, None)).is_err() {
                                    unsafe { libc::close(fd) };
                                    return;
                                }
                            }
                            ctx.request_repaint_after(Duration::from_millis(120));
                            continue;
                        }
                        if ev.mask & libc::IN_IGNORED != 0 {
                            continue;
                        }
                        let Some(dir) = wds2.lock().unwrap_or_else(|e| e.into_inner()).get(&ev.wd).cloned() else { continue };
                        let file = if name.is_empty() { None } else { Some(dir.join(std::ffi::OsStr::from_bytes(name))) };
                        if tx.send((dir, file)).is_err() {
                            unsafe { libc::close(fd) };
                            return;
                        }
                    }
                    ctx.request_repaint_after(Duration::from_millis(120));
                }
            });
        }
        Watch { fd, wds, by_path: HashMap::new(), rx, dirty: HashMap::new(), changed_files: vec![] }
    }

    /// Make the set of watched dirs equal to `want`.
    pub fn sync(&mut self, want: &HashSet<PathBuf>) {
        if self.fd < 0 {
            return;
        }
        let gone: Vec<PathBuf> = self.by_path.keys().filter(|p| !want.contains(*p)).cloned().collect();
        for p in gone {
            if let Some(wd) = self.by_path.remove(&p) {
                unsafe { libc::inotify_rm_watch(self.fd, wd) };
                self.wds.lock().unwrap_or_else(|e| e.into_inner()).remove(&wd);
            }
        }
        for p in want {
            if self.by_path.contains_key(p) {
                continue;
            }
            let Ok(c) = CString::new(p.as_os_str().as_bytes()) else { continue };
            let wd = unsafe { libc::inotify_add_watch(self.fd, c.as_ptr(), MASK) };
            if wd >= 0 {
                self.by_path.insert(p.clone(), wd);
                self.wds.lock().unwrap_or_else(|e| e.into_inner()).insert(wd, p.clone());
            }
        }
    }

    /// Returns directories whose contents changed and have settled (debounced).
    pub fn poll(&mut self) -> Vec<PathBuf> {
        while let Ok((dir, file)) = self.rx.try_recv() {
            self.dirty.entry(dir).or_insert_with(Instant::now);
            if let Some(f) = file {
                self.changed_files.push(f);
            }
        }
        let now = Instant::now();
        let ready: Vec<PathBuf> = self.dirty.iter().filter(|(_, t)| now.duration_since(**t) >= Duration::from_millis(100)).map(|(p, _)| p.clone()).collect();
        for p in &ready {
            self.dirty.remove(p);
        }
        ready
    }

    pub fn has_pending(&self) -> bool {
        !self.dirty.is_empty()
    }

    /// Put `dir` back in line for a reload: its change was reported but couldn't be
    /// acted on just then (a rename editor was open over the listing), so it must not
    /// be lost to the folder going stale.
    pub fn rearm(&mut self, dir: &Path) {
        self.dirty.entry(dir.to_path_buf()).or_insert_with(Instant::now);
    }
}

impl Drop for Watch {
    /// Let go of the thread's map, then remove a watch: the event that sends wakes the
    /// thread, which closes the descriptor and ends.
    fn drop(&mut self) {
        if self.fd < 0 {
            return;
        }
        self.wds = Arc::default();
        let wd = match self.by_path.values().next() {
            Some(&wd) => wd,
            None => unsafe { libc::inotify_add_watch(self.fd, c"/".as_ptr(), libc::IN_ATTRIB) },
        };
        unsafe { libc::inotify_rm_watch(self.fd, wd) };
    }
}
