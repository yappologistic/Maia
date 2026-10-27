//! The terminal pane: your shells on pseudo-terminals, docked under the panels, the one
//! shown going to the folder shown whenever it waits at its prompt. A thread for each
//! reads what its shell writes and puts it through the emulator (`vt`), so the UI only
//! draws the screen it leaves. Closing a shell hangs it up and reaps it, closing the
//! pane all of them: nothing runs while it is closed.

use std::io::Write as _;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use egui::{Color32, CornerRadius, Event, FontId, Id, Key, Modifiers, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder, Vec2, pos2, vec2};

use crate::app::{Action, App};
use crate::commands::{Chord, Cmd};
use crate::findbar::{Finder, Out};
use crate::theme::{Glyph, Palette, paint_glyph};
use crate::vt::{self, Cell, Term};

/// The gap above the pane (as between split panels) and its header's height.
const GAP: f32 = 3.0;
const HEADER_H: f32 = 30.0;
const MIN_H: f32 = 90.0;
const DEFAULT_H: f32 = 240.0;
/// Space around the cells.
const PAD: f32 = 6.0;

/// What the reader thread and the UI share.
pub struct Shared {
    term: Mutex<Term>,
    /// The screen changed since it was drawn (a burst of output asks for one repaint).
    fresh: AtomicBool,
    /// The shell ended: its terminal closes.
    ended: AtomicBool,
    /// Its terminal is the one shown: output there asks for a repaint, elsewhere not.
    shown: AtomicBool,
}

impl Shared {
    pub fn term(&self) -> MutexGuard<'_, Term> {
        self.term.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn ended(&self) -> bool {
        self.ended.load(Ordering::Acquire)
    }
}

/// A program on a pseudo-terminal, read on a thread.
pub struct Pty {
    master: Arc<OwnedFd>,
    child: Option<Child>,
    /// An eventfd: written when the pane closes, which ends the reader's wait.
    stop: OwnedFd,
    pub shared: Arc<Shared>,
    /// Typed input the terminal has not taken yet (its buffer was full).
    pending: Vec<u8>,
}

impl Pty {
    /// Start `cmd` as a session leader with the terminal as its controlling one.
    pub fn spawn(mut cmd: Command, cols: usize, rows: usize, ctx: egui::Context) -> std::io::Result<Pty> {
        let err = std::io::Error::last_os_error;
        let fd = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(err());
        }
        let master = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut name = [0 as libc::c_char; 128];
        if unsafe { libc::grantpt(fd) != 0 || libc::unlockpt(fd) != 0 || libc::ptsname_r(fd, name.as_mut_ptr(), name.len()) != 0 } {
            return Err(err());
        }
        let sfd = unsafe { libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
        if sfd < 0 {
            return Err(err());
        }
        let slave = unsafe { OwnedFd::from_raw_fd(sfd) };
        // Line editing that knows a character can be several bytes.
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(sfd, &mut t) } == 0 {
            t.c_iflag |= libc::IUTF8;
            unsafe { libc::tcsetattr(sfd, libc::TCSANOW, &t) };
        }
        set_size(fd, cols, rows);
        cmd.env("TERM", "xterm-256color").env("COLORTERM", "truecolor").env_remove("COLUMNS").env_remove("LINES");
        cmd.stdin(Stdio::from(slave.try_clone()?)).stdout(Stdio::from(slave.try_clone()?)).stderr(Stdio::from(slave));
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            })
        };
        let child = cmd.spawn()?;
        // The parent keeps no end of the terminal's slave side: the shell's exit closes it.
        drop(cmd);
        unsafe { libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK) };
        let efd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if efd < 0 {
            return Err(err());
        }
        let stop = unsafe { OwnedFd::from_raw_fd(efd) };
        let shared = Arc::new(Shared { term: Mutex::new(Term::new(cols, rows)), fresh: AtomicBool::new(false), ended: AtomicBool::new(false), shown: AtomicBool::new(true) });
        let master = Arc::new(master);
        let (m, s, sh) = (master.clone(), stop.try_clone()?, shared.clone());
        std::thread::Builder::new().name("terminal".into()).spawn(move || read(m, s, sh, ctx))?;
        Ok(Pty { master, child: Some(child), stop, shared, pending: vec![] })
    }

    /// Send input; what the terminal can't take now waits for `flush`.
    pub fn write(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        self.flush();
    }

    /// Send waiting input; true when some is still waiting.
    pub fn flush(&mut self) -> bool {
        while !self.pending.is_empty() {
            let n = unsafe { libc::write(self.master.as_raw_fd(), self.pending.as_ptr().cast(), self.pending.len()) };
            if n <= 0 {
                break;
            }
            self.pending.drain(..n as usize);
        }
        !self.pending.is_empty()
    }

    pub fn resize(&self, cols: usize, rows: usize) {
        self.shared.term().screen.resize(cols, rows);
        set_size(self.master.as_raw_fd(), cols, rows);
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(|c| c.id())
    }

    /// The shell waits at its prompt: no program of its runs in the foreground.
    pub fn idle(&self) -> bool {
        let fg = unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) };
        fg > 0 && self.pid() == Some(fg as u32)
    }

    pub fn cwd(&self) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/{}/cwd", self.pid()?)).ok()
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let one = 1u64;
        unsafe { libc::write(self.stop.as_raw_fd(), (&one as *const u64).cast(), 8) };
        let Some(mut child) = self.child.take() else { return };
        let fg = unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) };
        unsafe {
            if fg > 0 {
                libc::kill(-fg, libc::SIGHUP);
            }
            libc::kill(-(child.id() as i32), libc::SIGHUP);
        }
        // Reaped on a thread: a shell takes a moment to go, and one that ignores the
        // hangup is killed after two seconds.
        let _ = std::thread::Builder::new().name("terminal reaper".into()).spawn(move || {
            for _ in 0..40 {
                if !matches!(child.try_wait(), Ok(None)) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            let _ = child.kill();
            let _ = child.wait();
        });
    }
}

fn set_size(fd: i32, cols: usize, rows: usize) {
    let ws = libc::winsize { ws_row: rows as u16, ws_col: cols as u16, ws_xpixel: 0, ws_ypixel: 0 };
    unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) };
}

/// The reader: waits for output, parses it into the screen and asks for a repaint (once
/// until the UI has drawn it). Ends when the shell does or the pane closes.
fn read(master: Arc<OwnedFd>, stop: OwnedFd, shared: Arc<Shared>, ctx: egui::Context) {
    let mut buf = vec![0u8; 16 << 10];
    loop {
        let mut fds = [libc::pollfd { fd: master.as_raw_fd(), events: libc::POLLIN, revents: 0 }, libc::pollfd { fd: stop.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
        if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            break;
        }
        if fds[1].revents != 0 {
            return;
        }
        let n = unsafe { libc::read(master.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 && matches!(std::io::Error::last_os_error().raw_os_error(), Some(libc::EAGAIN | libc::EINTR)) {
            continue;
        }
        // EIO: the shell and everything it started closed the terminal.
        if n <= 0 {
            break;
        }
        let replies = {
            let mut t = shared.term();
            t.feed(&buf[..n as usize]);
            std::mem::take(&mut t.screen.replies)
        };
        if !replies.is_empty() {
            unsafe { libc::write(master.as_raw_fd(), replies.as_ptr().cast(), replies.len()) };
        }
        if !shared.fresh.swap(true, Ordering::AcqRel) && shared.shown.load(Ordering::Acquire) {
            ctx.request_repaint();
        }
    }
    shared.ended.store(true, Ordering::Release);
    ctx.request_repaint();
}

/// One shell in the pane, with what is kept of it while another one is shown.
pub struct Shell {
    pub pty: Pty,
    id: u64,
    /// The active panel's folder when last looked at.
    seen: PathBuf,
    /// The folder the shell was started in or last sent to.
    dir: PathBuf,
    /// Text being selected: where it begins and where it is (lasting row, column).
    sel: Option<vt::Span>,
    find: Option<Find>,
    /// When it opened: its tab grows in.
    born: f64,
    /// What it is called in the header (its title, else its folder with ~) and on its tab
    /// (the title, else the folder's own name), for the screen change count `named`.
    full: String,
    short: String,
    named: u64,
    /// Its tab's width when last drawn: a closed one leaves that much room, closing up.
    tab_w: f32,
}

/// Find in what the shell wrote, scrollback included.
struct Find {
    bar: Finder,
    hits: Vec<vt::Span>,
    /// The query, the screen's change count and which screen the hits were found for.
    key: (String, u64, bool),
    /// When they were found: output streaming in finds again four times a second.
    at: f64,
    cur: usize,
}

/// Matches marked; past it the count says "+".
const MAX_HITS: usize = 10_000;

/// What a link under the pointer leads to; a path is looked for on disk by a worker.
#[derive(Clone)]
enum Goal {
    Url(String),
    Path(Probe, Option<u32>),
}

type Probe = Arc<Mutex<Option<Option<(PathBuf, bool)>>>>;
/// A path looked up: as written, the folder the shell said it is in, and the shell.
type ProbeKey = (String, Option<String>, u32);

/// The link under the pointer while Ctrl is held: found again only when the pointer goes
/// to another cell or the text changes.
struct Hover {
    key: (u64, u64, u64, usize),
    link: Option<(vt::Span, Goal)>,
}

/// What a link click opens, once the screen is let go.
#[derive(Clone)]
enum Open {
    Url(String),
    Path(PathBuf, bool, Option<u32>),
}

/// The pane in the window.
pub struct Pane {
    pub shells: Vec<Shell>,
    /// The shell shown.
    pub cur: usize,
    /// The last screen, shown while a closed pane folds away.
    fading: Option<Arc<Shared>>,
    pub focused: bool,
    /// The height dragged to, in points.
    pub height: f32,
    /// Cells the shells were last told they have.
    size: (usize, usize),
    /// Wheel movement short of a whole row.
    wheel: f32,
    /// When the cursor last moved or a key went in: the blink starts over.
    blink: f64,
    next_id: u64,
    hover: Option<Hover>,
    /// Paths looked up since Ctrl went down.
    probes: Vec<(ProbeKey, Probe)>,
    ctrl: bool,
    /// The button a program reporting the mouse saw go down, and the cell it was told of last.
    held: Option<u8>,
    last_cell: Option<(usize, usize)>,
    /// A closed tab's place in the strip, its width and when it closed (as the panels'
    /// closed tabs collapse).
    ghost: Option<(usize, f32, f64)>,
    /// The header's name for the screen shown while a closed pane folds away.
    fading_name: String,
}

impl Pane {
    pub fn new(height: f32) -> Pane {
        Pane {
            shells: vec![],
            cur: 0,
            fading: None,
            focused: false,
            height: if height > 0.0 { height } else { DEFAULT_H },
            size: (80, 24),
            wheel: 0.0,
            blink: 0.0,
            next_id: 0,
            hover: None,
            probes: vec![],
            ctrl: false,
            held: None,
            last_cell: None,
            ghost: None,
            fading_name: String::new(),
        }
    }

    /// The shell shown.
    pub fn pty(&self) -> Option<&Pty> {
        self.shells.get(self.cur).map(|s| &s.pty)
    }

    /// The screen's rows as text (tests).
    #[cfg(test)]
    pub fn text(&self) -> String {
        let Some(p) = self.pty() else { return String::new() };
        let t = p.shared.term();
        (0..t.screen.rows).map(|y| t.screen.row_text(y)).collect::<Vec<_>>().join("\n")
    }

    /// The find bar's matches and the one shown (tests).
    #[cfg(test)]
    pub fn found(&self) -> Option<(usize, usize, Option<vt::Span>)> {
        let f = self.shells.get(self.cur)?.find.as_ref()?;
        Some((f.hits.len(), f.cur, f.hits.get(f.cur).copied()))
    }

    /// The selection in the shell shown (tests).
    #[cfg(test)]
    pub fn selection(&self) -> Option<String> {
        let s = self.shells.get(self.cur)?;
        let (a, b) = s.sel?;
        Some(s.pty.shared.term().screen.text(a, b))
    }

    /// The link under the pointer, once known: its text (tests).
    #[cfg(test)]
    pub fn hovered(&self) -> Option<String> {
        let (span, goal) = self.hover.as_ref()?.link.as_ref()?;
        ready(goal)?;
        Some(self.pty()?.shared.term().screen.text(span.0, span.1))
    }

    /// Show shell `i`, the one before it hidden: its output stops asking for repaints.
    fn show(&mut self, i: usize, seen: &Path) {
        if let Some(old) = self.shells.get(self.cur) {
            old.pty.shared.shown.store(false, Ordering::Release);
        }
        self.cur = i.min(self.shells.len().saturating_sub(1));
        if let Some(s) = self.shells.get_mut(self.cur) {
            s.pty.shared.shown.store(true, Ordering::Release);
            // It goes on from where it was: only the next move of the panel takes it along.
            s.seen = seen.to_path_buf();
        }
        (self.hover, self.held, self.last_cell) = (None, None, None);
    }
}

/// A folder the shell can go to for this tab: a real one on this machine (not the
/// Computer view, an archive, the trash or a kernel filesystem).
fn place(t: &crate::tab::Tab) -> Option<PathBuf> {
    let p = &t.path;
    let real = t.archive.is_none() && p.is_absolute() && !crate::tab::is_place(p);
    (real && !p.ancestors().any(crate::trashcan::is_trash_files) && !crate::fsx::is_virtual_fs(p) && p.is_dir()).then(|| p.clone())
}

/// Ctrl+`: open the pane, give it the keyboard, or (when it has it) close it.
pub fn toggle(app: &mut App) {
    if app.terminal.shells.is_empty() {
        new_shell(app);
    } else if !app.terminal.focused {
        app.terminal.focused = true;
    } else {
        close(app);
    }
}

/// Another shell, in the folder the active panel shows, shown after the one that was.
pub fn new_shell(app: &mut App) {
    let t = app.panel().tab();
    let (here, dir) = (t.path.clone(), place(t).unwrap_or_else(crate::fsx::home));
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
    let mut cmd = Command::new(shell);
    cmd.current_dir(&dir);
    let (cols, rows) = app.terminal.size;
    match Pty::spawn(cmd, cols, rows, app.ctx.clone()) {
        Ok(p) => {
            let now = app.ctx.input(|i| i.time);
            let pane = &mut app.terminal;
            let at = if pane.shells.is_empty() { 0 } else { pane.cur + 1 };
            let id = pane.next_id;
            pane.next_id += 1;
            pane.shells.insert(at, Shell { pty: p, id, seen: here.clone(), dir, sel: None, find: None, born: now, full: String::new(), short: String::new(), named: u64::MAX, tab_w: 0.0 });
            pane.show(at, &here);
            (pane.fading, pane.focused, pane.blink) = (None, true, now);
        }
        Err(e) => app.toast(format!("Couldn't start a terminal: {e}")),
    }
}

/// Close the whole pane: every shell is hung up.
fn close(app: &mut App) {
    let pane = &mut app.terminal;
    pane.fading = pane.pty().map(|p| p.shared.clone());
    pane.fading_name = pane.shells.get(pane.cur).map(|s| s.full.clone()).unwrap_or_default();
    pane.shells.clear();
    pane.cur = 0;
    (pane.focused, pane.hover, pane.held) = (false, None, None);
    app.ctx.memory_mut(|m| m.surrender_focus(id()));
}

/// Close shell `i`; the last one closes the pane.
pub fn close_shell(app: &mut App, i: usize) {
    if app.terminal.shells.len() <= 1 {
        if !app.terminal.shells.is_empty() {
            close(app);
        }
        return;
    }
    let seen = app.panels.get(&app.active).map(|p| p.tab().path.clone()).unwrap_or_default();
    let pane = &mut app.terminal;
    if i >= pane.shells.len() {
        return;
    }
    let gone = pane.shells.remove(i);
    // Two left one: the strip goes, with nothing to close up.
    if pane.shells.len() > 1 {
        pane.ghost = Some((i, gone.tab_w, app.ctx.input(|t| t.time)));
    }
    if i < pane.cur {
        pane.cur -= 1;
    } else if i == pane.cur {
        // The one after it shows (before it, for the last), going on from where it was.
        pane.cur = i.min(pane.shells.len() - 1);
        pane.show(pane.cur, &seen);
    }
}

/// The shell `d` places after (or before) the one shown, round the end.
pub fn switch(app: &mut App, d: isize) {
    let seen = app.panels.get(&app.active).map(|p| p.tab().path.clone()).unwrap_or_default();
    let pane = &mut app.terminal;
    let n = pane.shells.len();
    if n == 0 {
        return;
    }
    let i = (pane.cur as isize + d).rem_euclid(n as isize) as usize;
    pane.show(i, &seen);
    pane.focused = true;
}

/// Open the find bar on the shell shown, or put the keyboard back in it.
pub fn find(app: &mut App) {
    let pane = &mut app.terminal;
    let Some(s) = pane.shells.get_mut(pane.cur) else { return };
    let f = s.find.get_or_insert_with(|| Find { bar: Finder::new(String::new()), hits: vec![], key: (String::new(), u64::MAX, false), at: f64::MIN, cur: 0 });
    f.bar.focus = true;
    pane.focused = true;
}

fn id() -> Id {
    Id::new("terminal")
}

/// Room the pane takes at the bottom of `full`, with the gap above it.
pub fn room(app: &App, full: Rect) -> f32 {
    if app.terminal.shells.is_empty() {
        return 0.0;
    }
    let most = (full.height() - crate::panel::MIN_PANEL_H - GAP).max(MIN_H);
    app.terminal.height.clamp(MIN_H, most).min(full.height() * 0.8) + GAP
}

/// Follow the folder the active panel shows: with the shell at its prompt, it goes
/// there, typed as a command with a space before it (which keeps it out of the history
/// where HISTCONTROL ignores such lines). Only the shell shown follows: one put away in
/// another tab is left where it was working, and a long job in it is never typed into.
fn follow(app: &mut App) {
    let Some(t) = app.panels.get(&app.active).map(|p| p.tab()) else { return };
    let cur = app.terminal.cur;
    let Some(sh) = app.terminal.shells.get_mut(cur).filter(|s| t.path != s.seen) else { return };
    sh.seen = t.path.clone();
    let Some(dir) = place(t) else { return };
    if !sh.pty.idle() || sh.pty.cwd().as_deref() == Some(dir.as_path()) {
        return;
    }
    let Some(s) = dir.to_str().filter(|s| !s.contains(char::is_control)) else { return };
    // Ctrl+E Ctrl+U first: whatever was typed and not sent goes.
    sh.pty.write(format!("\x05\x15 cd -- '{}'\r", s.replace('\'', r"'\''")).as_bytes());
    sh.dir = dir;
}

/// Shells that ended go; when the last one has, the pane closes.
fn reap(app: &mut App) {
    if !app.terminal.shells.iter().any(|s| s.pty.shared.ended()) {
        return;
    }
    if app.terminal.shells.iter().all(|s| s.pty.shared.ended()) {
        return close(app);
    }
    while let Some(i) = app.terminal.shells.iter().position(|s| s.pty.shared.ended()) {
        close_shell(app, i);
    }
}

/// Keys while the terminal has the keyboard: all go to the shell, but for the command
/// that closes the pane, those that switch tabs, and the terminal's own: Ctrl+Shift+T,
/// W and PageUp/PageDown for its shells, Ctrl+Shift+F to find, Shift+PageUp/PageDown,
/// Home and End through the scrollback.
pub fn keys(app: &mut App, ctx: &egui::Context, events: &[Event]) {
    let cur = app.terminal.cur;
    let Some(sh) = app.terminal.shells.get_mut(cur) else {
        app.terminal.focused = false;
        return;
    };
    // The find field has the keyboard (Enter steps, Escape gives it back to the shell).
    if sh.find.as_ref().is_some_and(|f| f.bar.focused) {
        for ev in events {
            if let Event::Key { key, pressed: true, modifiers: m, .. } = ev {
                if *key == Key::Escape && !m.any() {
                    sh.find = None;
                    ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
                } else if let (Some(c @ Cmd::TerminalPanel), _) = app.keymap.lookup(&[Chord::from_key(*key, *m)]) {
                    app.actions.push(Action::Cmd(c));
                    ctx.input_mut(|i| i.consume_key(*m, *key));
                }
            }
        }
        return;
    }
    let pty = &mut sh.pty;
    let mods = ctx.input(|i| i.modifiers);
    let (app_cursor, bracketed, alt, rows) = {
        let t = pty.shared.term();
        (t.screen.app_cursor, t.screen.bracketed_paste, t.screen.alt(), t.screen.rows)
    };
    let mut out = vec![];
    for ev in events {
        match ev {
            Event::Key { key, pressed: true, modifiers: m, .. } => {
                let mine = match key {
                    Key::T => Some(Cmd::NewTerminal),
                    Key::W => Some(Cmd::CloseTerminal),
                    Key::PageDown => Some(Cmd::NextTerminal),
                    Key::PageUp => Some(Cmd::PrevTerminal),
                    Key::F => Some(Cmd::FindInTerminal),
                    _ => None,
                }
                .filter(|_| (m.ctrl || m.command) && m.shift && !m.alt);
                let back = match key {
                    Key::PageUp => Some(rows.saturating_sub(1).max(1) as isize),
                    Key::PageDown => Some(-(rows.saturating_sub(1).max(1) as isize)),
                    Key::Home => Some(isize::MAX / 2),
                    Key::End => Some(isize::MIN / 2),
                    _ => None,
                }
                .filter(|_| m.shift && !m.ctrl && !m.command && !m.alt && !alt);
                if let (Some(c @ (Cmd::TerminalPanel | Cmd::NextTab | Cmd::PrevTab)), _) = app.keymap.lookup(&[Chord::from_key(*key, *m)]) {
                    app.actions.push(Action::Cmd(c));
                } else if let Some(c) = mine {
                    app.actions.push(Action::Cmd(c));
                } else if let Some(d) = back {
                    pty.shared.term().screen.scroll_view(d);
                } else if (m.ctrl && m.shift && *key == Key::V) || (m.shift && !m.ctrl && *key == Key::Insert) {
                    paste(&mut out, &crate::clipboard::get_text().unwrap_or_default(), bracketed);
                } else {
                    key_bytes(*key, *m, app_cursor, &mut out);
                }
            }
            Event::Text(t) => {
                if mods.alt {
                    out.push(0x1b);
                }
                out.extend_from_slice(t.as_bytes());
            }
            Event::Ime(egui::ImeEvent::Commit(t)) => out.extend_from_slice(t.as_bytes()),
            Event::Copy if mods.shift => {
                if let Some((a, b)) = sh.sel {
                    ctx.copy_text(pty.shared.term().screen.text(a, b));
                }
            }
            Event::Copy => out.push(0x03),
            Event::Cut => out.push(0x18),
            Event::Paste(s) if mods.shift => paste(&mut out, s, bracketed),
            Event::Paste(_) => out.push(0x16),
            _ => {}
        }
    }
    if !out.is_empty() {
        pty.shared.term().screen.scroll = 0;
        pty.write(&out);
        app.terminal.blink = ctx.input(|i| i.time);
    }
    ctx.input_mut(|i| i.events.retain(|e| !matches!(e, Event::Key { .. } | Event::Text(_) | Event::Copy | Event::Cut | Event::Paste(_) | Event::Ime(_))));
}

/// Pasted text: line breaks as Enter sends them, marked as pasted when the program asked.
fn paste(out: &mut Vec<u8>, s: &str, bracketed: bool) {
    let s = s.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(s.replace("\x1b[201~", "").as_bytes());
        out.extend_from_slice(b"\x1b[201~");
    } else {
        out.extend_from_slice(s.as_bytes());
    }
}

/// What a key sends, as xterm does. Keys that type text send it with their Text event.
fn key_bytes(key: Key, m: Modifiers, app_cursor: bool, out: &mut Vec<u8>) {
    // xterm's modifier parameter: 1 + Shift + 2 Alt + 4 Ctrl.
    let n = 1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.ctrl as u8;
    let mut csi = |c: char, ss3: bool| {
        let _ = if n > 1 {
            write!(out, "\x1b[1;{n}{c}")
        } else if ss3 {
            write!(out, "\x1bO{c}")
        } else {
            write!(out, "\x1b[{c}")
        };
    };
    match key {
        Key::ArrowUp => csi('A', app_cursor),
        Key::ArrowDown => csi('B', app_cursor),
        Key::ArrowRight => csi('C', app_cursor),
        Key::ArrowLeft => csi('D', app_cursor),
        Key::Home => csi('H', app_cursor),
        Key::End => csi('F', app_cursor),
        Key::F1 => csi('P', true),
        Key::F2 => csi('Q', true),
        Key::F3 => csi('R', true),
        Key::F4 => csi('S', true),
        Key::Insert | Key::Delete | Key::PageUp | Key::PageDown | Key::F5 | Key::F6 | Key::F7 | Key::F8 | Key::F9 | Key::F10 | Key::F11 | Key::F12 => {
            let k = match key {
                Key::Insert => 2,
                Key::Delete => 3,
                Key::PageUp => 5,
                Key::PageDown => 6,
                Key::F5 => 15,
                Key::F6 => 17,
                Key::F7 => 18,
                Key::F8 => 19,
                Key::F9 => 20,
                Key::F10 => 21,
                Key::F11 => 23,
                _ => 24,
            };
            let _ = if n > 1 { write!(out, "\x1b[{k};{n}~") } else { write!(out, "\x1b[{k}~") };
        }
        Key::Tab if m.shift => out.extend_from_slice(b"\x1b[Z"),
        Key::Escape => out.push(0x1b),
        _ => {
            let b = match key {
                Key::Enter => b'\r',
                Key::Tab => b'\t',
                Key::Backspace if m.ctrl => 0x08,
                Key::Backspace => 0x7f,
                _ if !m.ctrl => return,
                Key::Space | Key::Num2 => 0,
                Key::OpenBracket | Key::Num3 => 0x1b,
                Key::Backslash | Key::Num4 => 0x1c,
                Key::CloseBracket | Key::Num5 => 0x1d,
                Key::Num6 => 0x1e,
                Key::Slash | Key::Minus | Key::Num7 => 0x1f,
                Key::Num8 => 0x7f,
                k => match k.name().as_bytes() {
                    [c @ b'A'..=b'Z'] => c - b'@',
                    _ => return,
                },
            };
            if m.alt {
                out.push(0x1b);
            }
            out.push(b);
        }
    }
}

/// The 16 ANSI colours from the theme: its danger red, the git status green and amber,
/// its accent for blue and code colours for magenta, cyan and the greys.
fn ansi(pal: &Palette) -> [Color32; 16] {
    let s = &pal.syntax;
    let ink = |c: Color32| if pal.dark { c } else { c.lerp_to_gamma(Color32::BLACK, 0.25) };
    let black = if pal.dark { pal.bg.lerp_to_gamma(pal.text, 0.22) } else { pal.text };
    let base = [black, pal.danger, ink(crate::gitstatus::GREEN), ink(crate::gitstatus::AMBER), pal.accent, s.keyword, s.kind, pal.text_dim];
    let mut c = [Color32::BLACK; 16];
    for (i, b) in base.into_iter().enumerate() {
        c[i] = b;
        // The bright half: a step toward the text colour (lighter on dark, darker on light).
        c[i + 8] = b.lerp_to_gamma(pal.text, 0.3);
    }
    (c[8], c[15]) = (s.comment, pal.text);
    c
}

fn color(code: u16, ansi: &[Color32; 16], default: Color32) -> Color32 {
    match code {
        vt::DEFAULT => default,
        0..16 => ansi[code as usize],
        16..232 => {
            let i = code - 16;
            let v = |n: u16| if n == 0 { 0 } else { (55 + n * 40) as u8 };
            Color32::from_rgb(v(i / 36), v(i / 6 % 6), v(i % 6))
        }
        232..256 => Color32::from_gray(8 + (code - 232) as u8 * 10),
        _ => {
            let ch = |s: u16| {
                let v = (code >> s & 31) as u8;
                v << 3 | v >> 2
            };
            Color32::from_rgb(ch(10), ch(5), ch(0))
        }
    }
}

/// Draw the pane under the panels, whose lowest edge is at `above` (it eases with them
/// as the pane opens and closes), with the divider that resizes it.
pub fn pane(app: &mut App, ui: &mut Ui, full: Rect, above: f32) {
    follow(app);
    reap(app);
    let rect = Rect::from_min_max(pos2(full.left(), above + GAP), full.max);
    let pal = app.pal;
    let (animations, round) = (app.cfg.animations, app.cfg.round_corners);
    let modal = !matches!(app.popup, crate::popups::Popup::None);
    let pane = &mut app.terminal;
    if rect.height() < 1.0 {
        pane.fading = None;
        return;
    }
    let Some(shared) = pane.pty().map(|p| p.shared.clone()).or_else(|| pane.fading.clone()) else { return };
    let open = !pane.shells.is_empty();
    let ctx = ui.ctx().clone();
    let now = ctx.input(|i| i.time);
    // A press anywhere else gives the keyboard back to the files.
    let press = ctx.input(|i| i.pointer.any_pressed().then(|| i.pointer.interact_pos()).flatten());
    if let Some(p) = press {
        pane.focused = open && rect.contains(p);
        pane.blink = now;
    }
    let focused = pane.focused && !modal;
    let finding = pane.shells.get(pane.cur).and_then(|s| s.find.as_ref()).is_some_and(|f| f.bar.focused || f.bar.focus);
    ctx.memory_mut(|m| {
        if focused && !finding {
            m.request_focus(id());
            m.set_focus_lock_filter(id(), egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true });
        } else if m.has_focus(id()) {
            m.surrender_focus(id());
        }
    });
    // The divider.
    if open {
        let handle = Rect::from_center_size(pos2(rect.center().x, rect.top() - GAP / 2.0), vec2(rect.width(), 6.0));
        let resp = ui.interact(handle, Id::new("terminal divider"), Sense::drag());
        if resp.hovered() || resp.dragged() {
            ctx.set_cursor_icon(egui::CursorIcon::ResizeVertical);
            ui.painter().rect_filled(handle.shrink(2.0), CornerRadius::same(1), pal.accent.gamma_multiply(0.6));
        }
        if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.dragged()) {
            pane.height = full.bottom() - p.y;
        }
        if resp.double_clicked() {
            pane.height = DEFAULT_H;
        }
    }
    let rad = if round { 8 } else { 0 };
    let p = ui.painter_at(rect);
    p.rect_filled(rect, CornerRadius::same(rad), pal.panel);
    let body = Rect::from_min_max(pos2(rect.left(), rect.top() + HEADER_H), rect.max);
    p.rect_filled(body, CornerRadius { nw: 0, ne: 0, sw: rad, se: rad }, pal.bg);
    p.line_segment([body.left_top(), body.right_top()], Stroke::new(1.0_f32, pal.border));
    let font = FontId::monospace(app.cfg.inspector_font_size);
    let (cw, row_h) = ui.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font).round()));
    // The size the pane is going to (not the one it eases through): the shells are told once.
    let goal = room(app, full) - GAP - HEADER_H - 2.0 * PAD;
    let cols = ((full.width() - 2.0 * PAD) / cw).floor().max(2.0) as usize;
    let rows = (goal / row_h).floor().max(1.0) as usize;
    let pane = &mut app.terminal;
    if open && (cols, rows) != pane.size {
        for s in &mut pane.shells {
            s.pty.resize(cols, rows);
            // Rewrapped rows are numbered anew: a selection would land on other text.
            s.sel = None;
        }
        pane.size = (cols, rows);
    }
    if pane.shells.iter_mut().fold(false, |w, s| s.pty.flush() | w) {
        ctx.request_repaint_after(std::time::Duration::from_millis(10));
    }
    // ------------------------------------------------ header
    let hdr = Rect::from_min_max(rect.min, pos2(rect.right(), body.top()));
    let mut hui = ui.new_child(UiBuilder::new().max_rect(hdr.shrink2(vec2(6.0, 3.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    hui.set_clip_rect(hdr.intersect(ui.clip_rect()));
    let mut closing = false;
    let many = pane.shells.len() > 1;
    if open && crate::ui::glyph_button_as(&mut hui, Glyph::Close, if many { "Close all terminals" } else { "Close terminal" }, true, &pal, "terminal close").clicked() {
        closing = true;
    }
    hui.add_space(4.0);
    pane.shells.iter_mut().for_each(name);
    let mut tabbed = None;
    if many {
        tabbed = strip(&mut hui, hdr, body, pane, &pal, rad, animations, now, &app.keymap);
    } else {
        let title = pane.shells.first().map_or(pane.fading_name.as_str(), |s| s.full.as_str());
        hui.add(egui::Label::new(egui::RichText::new(title).size(12.0).color(pal.text_dim)).truncate().selectable(false));
    }
    // ------------------------------------------------ find
    let mut term = shared.term();
    let s = &mut term.screen;
    let (x0, mut y0) = (body.left() + PAD, body.top() + PAD);
    let mut view = body;
    let mut reveal = None;
    let mut close_find = false;
    let sh_id = pane.shells.get(pane.cur).map_or(0, |s| s.id);
    if let Some(f) = pane.shells.get_mut(pane.cur).and_then(|s| s.find.as_mut()) {
        if refresh(f, s, now) {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        let count = match f.hits.len() {
            _ if f.bar.query.is_empty() => String::new(),
            0 => "No matches".into(),
            n => format!("{} of {}{}", f.cur + 1, crate::table::fmt_count(n), if n >= MAX_HITS { "+" } else { "" }),
        };
        let (out, rest) = crate::findbar::bar(ui, body, &mut f.bar, &count, !f.hits.is_empty(), u64::MAX - sh_id, animations, &pal);
        view = rest;
        let n = f.hits.len();
        match out {
            Out::Next | Out::Prev if n > 0 => {
                f.cur = (f.cur as isize + if out == Out::Next { 1 } else { -1 }).rem_euclid(n as isize) as usize;
                f.bar.reveal = true;
            }
            Out::Close => close_find = true,
            _ => {}
        }
        if f.bar.reveal {
            f.bar.reveal = false;
            reveal = f.hits.get(f.cur).copied();
        }
    }
    // Rows the bar would cover go off the top instead: the newest output and the prompt
    // stay in sight, and the shell is not told of a smaller screen.
    let under = (y0 + s.rows as f32 * row_h - view.bottom()).min(body.bottom() - view.bottom());
    if under > 0.0 {
        y0 -= (under / row_h).ceil() * row_h;
    }
    // The rows in sight, from the first whole one.
    let first = (((body.top() + PAD - y0) / row_h).round().max(0.0) as usize).min(s.rows);
    let last = (((view.bottom() - y0) / row_h).floor().max(0.0) as usize).clamp(first, s.rows);
    // The match shown is brought into view, about the middle of it.
    if let Some(h) = reveal {
        let top = s.view_line(first);
        if h.0.0 < top || h.0.0 >= top + (last - first).max(1) as u64 {
            let want = s.dropped as i64 + s.history() as i64 + ((first + last) / 2) as i64 - h.0.0 as i64;
            s.scroll = want.clamp(0, s.history() as i64) as usize;
        }
    }
    if close_find {
        pane.shells[pane.cur].find = None;
    }
    // ------------------------------------------------ pointer
    let resp = ui.interact(view, id(), Sense::click_and_drag());
    let shown = (((view.bottom() - y0) / row_h).ceil().max(0.0) as usize).min(s.rows);
    let cell = |s: &vt::Screen, p: Pos2| (((p.x - x0) / cw).floor().clamp(0.0, (s.cols - 1) as f32) as usize, ((p.y - y0) / row_h).floor().clamp(first.min(s.rows - 1) as f32, (s.rows - 1) as f32) as usize);
    let at = |s: &vt::Screen, p: Pos2| {
        let (x, y) = cell(s, p);
        (s.view_line(y), x)
    };
    let mods = ctx.input(|i| i.modifiers);
    let ctrl = (mods.ctrl || mods.command) && !mods.alt;
    // Ctrl held over a link underlines it, and a click opens it.
    if ctrl && !pane.ctrl {
        pane.probes.clear();
    }
    pane.ctrl = ctrl;
    // The pointer is on the cells, not on a menu or a popup over them.
    let over = resp.contains_pointer() && !modal;
    let hover = ctx.pointer_hover_pos().filter(|p| open && ctrl && over && view.contains(*p) && !resp.dragged());
    match (hover, pane.pty()) {
        (Some(p), Some(pty)) => {
            let (n, x) = at(s, p);
            let key = (sh_id, s.changes, n, x);
            if pane.hover.as_ref().map(|h| h.key) != Some(key) {
                let pid = pty.pid().unwrap_or(0);
                let link = link_at(s, n, x, &mut pane.probes, pid, &ctx);
                pane.hover = Some(Hover { key, link });
            }
        }
        _ => pane.hover = None,
    }
    let link = pane.hover.as_ref().and_then(|h| h.link.as_ref()).and_then(|(span, g)| ready(g).map(|o| (*span, o)));
    let mut opening = None;
    if let Some((_, o)) = &link {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
        if resp.clicked() {
            opening = Some(o.clone());
        }
    }
    let reporting = open && s.mouse != vt::Track::Off && !mods.shift && !modal;
    let mut report = vec![];
    if reporting {
        let evs = ctx.input(|i| i.events.clone());
        let held_mods = (mods.shift, mods.alt, mods.ctrl);
        for e in &evs {
            match e {
                Event::PointerButton { pos, button, pressed, modifiers: m } => {
                    let b = match button {
                        egui::PointerButton::Primary => 0,
                        egui::PointerButton::Middle => 1,
                        egui::PointerButton::Secondary => 2,
                        _ => continue,
                    };
                    let (x, y) = cell(s, *pos);
                    let m = (m.shift, m.alt, m.ctrl);
                    if *pressed && over && view.contains(*pos) && link.is_none() {
                        pane.held = Some(b);
                        pane.last_cell = Some((x, y));
                        report.extend(s.report(vt::Mouse::Press(b), x, y, m).unwrap_or_default());
                    } else if !*pressed && pane.held == Some(b) {
                        pane.held = None;
                        report.extend(s.report(vt::Mouse::Release(b), x, y, m).unwrap_or_default());
                    }
                }
                Event::PointerMoved(pos) if pane.held.is_some() || (over && view.contains(*pos)) => {
                    let c = cell(s, *pos);
                    if pane.last_cell != Some(c) {
                        pane.last_cell = Some(c);
                        report.extend(s.report(vt::Mouse::Motion(pane.held), c.0, c.1, held_mods).unwrap_or_default());
                    }
                }
                _ => {}
            }
        }
    } else {
        pane.held = None;
        let sel = pane.shells.get_mut(pane.cur).map(|s| &mut s.sel);
        if let Some(sel) = sel {
            if resp.drag_started() {
                if let Some(p) = ctx.input(|i| i.pointer.press_origin()) {
                    let c = at(s, p);
                    *sel = Some((c, c));
                }
            } else if resp.dragged() {
                if let (Some(p), Some(sel)) = (resp.interact_pointer_pos(), sel.as_mut()) {
                    sel.1 = at(s, p);
                }
            } else if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.triple_clicked()) {
                *sel = Some(s.line_span(at(s, p).0));
            } else if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.double_clicked()) {
                *sel = s.word(at(s, p));
            } else if resp.clicked() {
                *sel = None;
            }
        }
    }
    // The wheel scrolls back through what went by; a program that asked for the mouse
    // hears it, and in another full-screen one it moves the cursor, as xterm's alternate
    // scroll does.
    if resp.hovered() {
        let notches: f32 = ctx.input(|i| {
            i.events
                .iter()
                .map(|e| match e {
                    Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta, .. } => delta.y / row_h,
                    Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta, .. } => delta.y * 3.0,
                    Event::MouseWheel { delta, .. } => delta.y * rows as f32,
                    _ => 0.0,
                })
                .sum()
        });
        pane.wheel += notches;
        let whole = pane.wheel.trunc();
        pane.wheel -= whole;
        if whole != 0.0 {
            let up = whole > 0.0;
            let wheel = ctx.pointer_hover_pos().map(|p| cell(s, p)).and_then(|(x, y)| s.report(vt::Mouse::Wheel(up), x, y, (mods.shift, mods.alt, mods.ctrl)).filter(|_| reporting));
            if let Some(w) = wheel {
                report.extend(w.repeat(whole.abs() as usize));
            } else if s.alt() {
                let k = if up { if s.app_cursor { "\x1bOA" } else { "\x1b[A" } } else if s.app_cursor { "\x1bOB" } else { "\x1b[B" };
                report.extend(k.repeat(whole.abs() as usize).as_bytes());
            } else {
                s.scroll_view(whole as isize);
            }
        }
    }
    if let Some(sh) = pane.shells.get_mut(pane.cur).filter(|_| !report.is_empty()) {
        sh.pty.write(&report);
    }
    if shared.fresh.swap(false, Ordering::AcqRel) {
        pane.blink = now;
    }
    // ------------------------------------------------ cells
    let ansi = ansi(&pal);
    let cur_shell = pane.shells.get(pane.cur);
    let sel = cur_shell.and_then(|c| c.sel).map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
    let (hits, hit_cur): (&[vt::Span], Option<usize>) = match cur_shell.and_then(|c| c.find.as_ref()).filter(|f| !f.bar.query.is_empty()) {
        Some(f) => (&f.hits, Some(f.cur)),
        None => (&[], None),
    };
    let mut bgs: Vec<(Rect, Color32)> = vec![];
    let mut marks: Vec<(Rect, bool)> = vec![];
    let mut galleys = Vec::with_capacity(shown);
    ui.fonts_mut(|f| {
        for y in first..shown {
            let n = s.view_line(y);
            let Some(row) = s.line(n) else { continue };
            let top = y0 + y as f32 * row_h;
            let mut job = egui::text::LayoutJob::default();
            let mut key = None;
            // Where the text is laid and where its column is: a glyph wider or narrower
            // than its cells (a fallback font's) puts the next run back on its column.
            let (mut laid, mut want) = (0.0, 0.0);
            let end = row.cells.iter().rposition(|c| c.ch() != ' ').map_or(0, |i| i + 1);
            for (x, &c) in row.cells.iter().enumerate().take(s.cols) {
                let picked = sel.is_some_and(|(a, b)| (n, x) >= a && (n, x) <= b);
                let (fg, bg) = colors(c, picked, &ansi, &pal);
                if bg != pal.bg {
                    let r = Rect::from_min_size(pos2(x0 + x as f32 * cw, top), vec2(cw, row_h));
                    match bgs.last_mut() {
                        Some((last, col)) if *col == bg && (last.right() - r.left()).abs() < 0.5 && last.top() == r.top() => last.max.x = r.right(),
                        _ => bgs.push((r, bg)),
                    }
                }
                if x >= end || c.flags() & vt::SPACER != 0 {
                    continue;
                }
                let ch = c.ch();
                let w = if c.flags() & vt::WIDE != 0 { 2.0 * cw } else { cw };
                let k = (fg, c.c & (vt::ITALIC | vt::UNDERLINE | vt::STRIKE));
                if key != Some(k) || (want - laid) > 0.5 || (laid - want) > 0.5 {
                    let lead = want - laid;
                    let line = |on: bool| if on { Stroke::new(1.0_f32, fg) } else { Stroke::NONE };
                    let format = egui::TextFormat { font_id: font.clone(), color: fg, italics: k.1 & vt::ITALIC != 0, underline: line(k.1 & vt::UNDERLINE != 0), strikethrough: line(k.1 & vt::STRIKE != 0), ..Default::default() };
                    job.append("", lead, format);
                    (key, laid) = (Some(k), want);
                }
                job.text.push(ch);
                if let Some(sec) = job.sections.last_mut() {
                    sec.byte_range.end = egui::epaint::text::ByteIndex(job.text.len());
                }
                laid += if ch.is_ascii() { cw } else { f.glyph_width(&font, ch) };
                want += w;
            }
            // A selection runs on to the end of the rows it spans, past what a kept row holds.
            if let Some((a, b)) = sel.filter(|(a, b)| (a.0..=b.0).contains(&n)) {
                let from = if n == a.0 { a.1 } else { 0 }.max(row.cells.len());
                let to = if n == b.0 { b.1 + 1 } else { s.cols };
                if from < to {
                    bgs.push((Rect::from_min_max(pos2(x0 + from as f32 * cw, top), pos2(x0 + to as f32 * cw, top + row_h)), pal.selected));
                }
            }
            // Matches on this row: sorted and apart, so those ending on or after it.
            let first = hits.partition_point(|h| h.1.0 < n);
            for (k, h) in hits[first..].iter().enumerate().take_while(|(_, h)| h.0.0 <= n) {
                let from = if h.0.0 == n { h.0.1 } else { 0 };
                let to = if h.1.0 == n { h.1.1 + 1 } else { s.cols };
                marks.push((Rect::from_min_max(pos2(x0 + from as f32 * cw, top + 1.0), pos2(x0 + to as f32 * cw, top + row_h - 1.0)), hit_cur == Some(first + k)));
            }
            if !job.text.is_empty() {
                galleys.push((pos2(x0, top), f.layout_job(job)));
            }
        }
    });
    let p = ui.painter_at(view);
    for (r, c) in bgs {
        p.rect_filled(r, CornerRadius::ZERO, c);
    }
    // As the preview's find marks them.
    for (r, now) in marks {
        if now {
            p.rect_filled(r.expand(1.0), CornerRadius::same(3), pal.accent.gamma_multiply(0.55));
            p.rect_stroke(r.expand(1.0), CornerRadius::same(3), Stroke::new(1.0_f32, pal.accent), StrokeKind::Outside);
        } else {
            p.rect_filled(r, CornerRadius::same(2), pal.accent.gamma_multiply(0.25));
        }
    }
    for (pos, g) in galleys {
        p.galley(pos, g, pal.text);
    }
    // The link under the pointer, underlined along each row it is on.
    if let Some(((a, b), _)) = link {
        for n in a.0..=b.0 {
            let Some(y) = (first..shown).find(|&y| s.view_line(y) == n) else { continue };
            let from = if n == a.0 { a.1 } else { 0 };
            let to = if n == b.0 { b.1 + 1 } else { s.cols };
            let yy = y0 + (y + 1) as f32 * row_h - 1.5;
            p.line_segment([pos2(x0 + from as f32 * cw, yy), pos2(x0 + to as f32 * cw, yy)], Stroke::new(1.0_f32, pal.accent));
        }
    }
    // ------------------------------------------------ cursor
    let (cx, cy) = s.cursor();
    let vy = cy + s.scroll;
    if open && s.cursor_shown && (first..shown).contains(&vy) {
        // It blinks for ten seconds after the last key or output, then rests (no repaints).
        let phase = (now - pane.blink).max(0.0) / 0.53;
        let blink = animations && focused && !finding && phase < 19.0;
        if blink {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64((1.0 - phase.fract()) * 0.53));
        }
        if !blink || (phase as u64) % 2 == 0 {
            let cell = s.line(s.view_line(vy)).and_then(|r| r.cells.get(cx).copied()).unwrap_or(Cell::BLANK);
            let w = if cell.flags() & vt::WIDE != 0 { 2.0 * cw } else { cw };
            let r = Rect::from_min_size(pos2(x0 + cx as f32 * cw, y0 + vy as f32 * row_h), vec2(w, row_h));
            match (focused && !finding, s.shape) {
                (false, _) => {
                    p.rect_stroke(r.shrink(0.5), CornerRadius::same(1), Stroke::new(1.0_f32, pal.accent), StrokeKind::Inside);
                }
                (true, vt::Shape::Bar) => {
                    p.rect_filled(Rect::from_min_size(r.min, vec2(2.0, row_h)), CornerRadius::ZERO, pal.accent);
                }
                (true, vt::Shape::Underline) => {
                    p.rect_filled(Rect::from_min_max(pos2(r.left(), r.bottom() - 2.0), r.max), CornerRadius::ZERO, pal.accent);
                }
                (true, vt::Shape::Block) => {
                    p.rect_filled(r, CornerRadius::same(1), pal.accent);
                    if cell.ch() != ' ' {
                        p.text(r.min, egui::Align2::LEFT_TOP, cell.ch(), font.clone(), pal.bg);
                    }
                }
            }
        }
    }
    drop(term);
    match tabbed {
        Some(Tabbed::Show(i)) => {
            let seen = app.panels.get(&app.active).map(|p| p.tab().path.clone()).unwrap_or_default();
            app.terminal.show(i, &seen);
            app.terminal.focused = true;
        }
        Some(Tabbed::Close(i)) => close_shell(app, i),
        Some(Tabbed::New) => new_shell(app),
        None => {}
    }
    match opening {
        Some(Open::Url(u)) => {
            if let Err(e) = crate::mime::open_uri(&u) {
                app.toast(format!("Couldn't open {u}: {e}"));
            }
        }
        Some(Open::Path(p, false, Some(line))) => match crate::editor::open_at(&app.cfg.editor, &p, Some(line)) {
            Ok(label) => app.toast(format!("Opened {}:{line} in {label}", p.file_name().map_or(p.display().to_string(), |n| n.to_string_lossy().into_owned()))),
            // No editor to start: the file selected in its folder, its preview at the line.
            Err(e) => {
                app.toast(e);
                app.show_at_line(p, line);
            }
        },
        // A folder opens in the panel, a file shows selected in its own.
        Some(Open::Path(p, ..)) => app.actions.push(Action::Navigate { panel: app.active, path: p }),
        None => {}
    }
    if closing {
        close(app);
    }
}

/// Name a shell again when its screen changed (see `Shell::full`). Without a title its
/// folder is the one the shell said it is in (OSC 7), else the one it is in while it
/// waits at its prompt; while a program runs it keeps the name it had.
fn name(sh: &mut Shell) {
    let (changes, title, said) = {
        let t = sh.pty.shared.term();
        if t.screen.changes == sh.named {
            return;
        }
        (t.screen.changes, (!t.screen.title.is_empty()).then(|| t.screen.title.clone()), t.screen.cwd.clone())
    };
    sh.named = changes;
    if let Some(t) = title {
        if sh.full != t {
            sh.short.clone_from(&t);
            sh.full = t;
        }
        return;
    }
    let dir = match said.map(PathBuf::from).or_else(|| sh.pty.idle().then(|| sh.pty.cwd()).flatten()) {
        Some(d) => d,
        None if sh.full.is_empty() => sh.dir.clone(),
        None => return,
    };
    let full = tilde(&dir);
    if full != sh.full {
        sh.short = match dir.file_name() {
            _ if full == "~" => full.clone(),
            Some(n) => n.to_string_lossy().into_owned(),
            None => full.clone(),
        };
        sh.full = full;
    }
}

/// Find again when the query or the text changed: at once for a new query, at most four
/// times a second while output streams in. The match shown stays the same text; a new
/// query starts at the one nearest the prompt, the last. True: come back for more.
fn refresh(f: &mut Find, s: &vt::Screen, now: f64) -> bool {
    if f.key.0 == f.bar.query && f.key.1 == s.changes && f.key.2 == s.alt() {
        return false;
    }
    let fresh = f.key.0 != f.bar.query || f.key.2 != s.alt();
    if !fresh && now - f.at < 0.25 {
        return true;
    }
    let old = f.hits.get(f.cur).copied();
    if f.bar.query.is_empty() {
        f.hits.clear();
    } else {
        s.find(&f.bar.query, f.bar.case(), MAX_HITS, &mut f.hits);
    }
    f.key = (f.bar.query.clone(), s.changes, s.alt());
    f.at = now;
    let last = f.hits.len().saturating_sub(1);
    f.cur = match old.filter(|_| !fresh) {
        Some(o) => f.hits.iter().position(|h| h.0 >= o.0).unwrap_or(last),
        None => {
            f.bar.reveal = !f.hits.is_empty();
            last
        }
    };
    false
}

/// What a link's target is, once known (a path the worker has not found yet is not).
fn ready(g: &Goal) -> Option<Open> {
    match g {
        Goal::Url(u) => Some(Open::Url(u.clone())),
        Goal::Path(probe, line) => {
            let (p, dir) = probe.lock().ok()?.clone()??;
            Some(Open::Path(p, dir, *line))
        }
    }
}

/// The link at a cell: a hyperlink the program wrote, else an address or a path in the
/// line around it (only this line is read). A path is looked for on a worker.
fn link_at(s: &vt::Screen, n: u64, x: usize, probes: &mut Vec<(ProbeKey, Probe)>, pid: u32, ctx: &egui::Context) -> Option<(vt::Span, Goal)> {
    let row = s.line(n)?;
    let mut look = |text: String| -> Probe {
        let key = (text, s.cwd.clone(), pid);
        if let Some((_, p)) = probes.iter().find(|(k, _)| *k == key) {
            return p.clone();
        }
        let probe: Probe = Arc::new(Mutex::new(None));
        let (slot, ctx, k) = (probe.clone(), ctx.clone(), key.clone());
        let _ = std::thread::Builder::new().name("terminal link".into()).spawn(move || {
            let base = k.1.map(PathBuf::from).or_else(|| std::fs::read_link(format!("/proc/{}/cwd", k.2)).ok());
            let found = crate::termlink::resolve(&k.0, base.as_deref());
            if let Ok(mut s) = slot.lock() {
                *s = Some(found);
            }
            ctx.request_repaint();
        });
        probes.push((key, probe.clone()));
        probe
    };
    if let Some(uri) = row.link(x) {
        let (a, b) = row.links.iter().rev().find(|l| (l.0 as usize..l.1 as usize).contains(&x)).map(|l| (l.0 as usize, l.1 as usize))?;
        let goal = match crate::termlink::file_uri(uri) {
            Some(p) => Goal::Path(look(p), None),
            None => Goal::Url(uri.to_string()),
        };
        return Some((((n, a), (n, b - 1)), goal));
    }
    let (a, b) = s.logical(n);
    let (mut text, mut at) = (String::new(), vec![]);
    s.line_text(a, b, &mut text, &mut at);
    let i = at.iter().position(|p| p.n == n && (p.x as usize..p.x as usize + p.w as usize).contains(&x))?;
    let (ca, cb, t) = crate::termlink::at(&text, i)?;
    let (first, last) = (at.get(ca)?, at.get(cb.checked_sub(1)?)?);
    let span = ((first.n, first.x as usize), (last.n, last.x as usize + last.w as usize - 1));
    let goal = match t {
        crate::termlink::Target::Url(u) => Goal::Url(u),
        crate::termlink::Target::Path { text, line, .. } => Goal::Path(look(text), line),
    };
    Some((span, goal))
}

/// What a click on the tab strip asked for.
enum Tabbed {
    Show(usize),
    Close(usize),
    New,
}

/// The shells as tabs in the header, as the panels' tabs are drawn: the one shown cut
/// from the screen below it, the others lit when hovered, each closed by its ×, and +
/// for another.
#[allow(clippy::too_many_arguments)]
fn strip(ui: &mut Ui, hdr: Rect, body: Rect, pane: &mut Pane, pal: &Palette, rad: u8, animations: bool, now: f64, keymap: &crate::commands::Keymap) -> Option<Tabbed> {
    let n = pane.shells.len();
    // A closed tab's room closes up over 80 ms.
    let ghost = pane.ghost.filter(|g| animations && now - g.2 < 0.08);
    pane.ghost = ghost;
    if ghost.is_some() {
        ui.ctx().request_repaint();
    }
    let plus_w = 28.0;
    let gap = ui.spacing().item_spacing.x;
    let avail = ui.available_width() - plus_w - (gap + 1.0) * n as f32;
    let per = (avail / n as f32).clamp(56.0, 200.0);
    let pointer = ui.ctx().pointer_hover_pos();
    let mut out = None;
    let rad = rad.min(7);
    let slot = |ui: &mut Ui, i: usize| {
        if let Some((_, w, at)) = ghost.filter(|g| g.0 == i) {
            let e = 1.0 - (1.0 - ((now - at) / 0.08) as f32).powi(2);
            ui.allocate_exact_size(vec2((w * (1.0 - e)).max(0.0), hdr.height() - 6.0), Sense::hover());
        }
    };
    let cur = pane.cur;
    for (i, s) in pane.shells.iter_mut().enumerate() {
        slot(ui, i);
        let grow = if animations { ((now - s.born) / 0.11).clamp(0.0, 1.0) as f32 } else { 1.0 };
        if grow < 1.0 {
            ui.ctx().request_repaint();
        }
        if crate::ui::no_room(ui, per * (0.3 + 0.7 * grow)) {
            break;
        }
        s.tab_w = per * (0.3 + 0.7 * grow);
        let (r, resp) = ui.allocate_exact_size(vec2(s.tab_w, hdr.height() - 6.0), Sense::click());
        let r = Rect::from_min_max(pos2(r.left(), hdr.top() + 4.0), pos2(r.right(), hdr.bottom()));
        crate::ui::note_control(ui, r, "terminal tab");
        let active = i == cur;
        let hovered = resp.hovered();
        let p = ui.painter();
        if active {
            // Over the header's line into the screen, with feet curving out into the header.
            let full = Rect::from_min_max(r.min, pos2(r.right(), body.top() + 1.0));
            p.rect_filled(full, CornerRadius { nw: rad, ne: rad, sw: 0, se: 0 }, pal.bg);
            if rad > 0 {
                let f = rad as f32;
                for (x, dir) in [(full.left(), -1.0f32), (full.right(), 1.0)] {
                    let foot = Rect::from_min_max(pos2(x.min(x + dir * f), full.bottom() - f), pos2(x.max(x + dir * f), full.bottom()));
                    p.rect_filled(foot, CornerRadius::ZERO, pal.bg);
                    p.circle_filled(pos2(x + dir * f, full.bottom() - f), f, pal.panel);
                }
            }
        } else if hovered {
            p.rect_filled(r.shrink2(vec2(2.0, 3.0)), CornerRadius::same(rad.min(6)), pal.hover);
        }
        let ic = Rect::from_center_size(pos2(r.left() + 14.0, r.center().y), Vec2::splat(13.0));
        crate::theme::paint_symbol(p, ic, "square-terminal", pal.text_dim);
        let close_w = if (hovered || active) && r.width() >= 52.0 { 18.0 } else { 0.0 };
        let text = Rect::from_min_max(pos2(r.left() + 26.0, r.top()), pos2(r.right() - 6.0 - close_w, r.bottom()));
        let g = crate::listing::elided(ui, &s.short, FontId::proportional(12.0), text.width(), if active { pal.text } else { pal.text_dim });
        ui.painter().galley(pos2(text.left(), r.center().y - g.size().y / 2.0), g, pal.text);
        if close_w > 0.0 {
            let cr = Rect::from_center_size(pos2(r.right() - 14.0, r.center().y), Vec2::splat(16.0));
            let on = pointer.is_some_and(|pp| cr.contains(pp));
            if on {
                ui.painter().rect_filled(cr, CornerRadius::same(4), pal.raised);
            }
            paint_glyph(ui.painter(), cr.shrink(3.0), Glyph::Close, if on { pal.text } else { pal.text_dim });
            if on && resp.clicked() {
                out = Some(Tabbed::Close(i));
                continue;
            }
        }
        let resp = if s.short != s.full { resp.on_hover_text(&s.full) } else { resp };
        if resp.middle_clicked() {
            out = Some(Tabbed::Close(i));
        } else if resp.clicked() {
            out = Some(Tabbed::Show(i));
        }
    }
    slot(ui, n);
    if !crate::ui::no_room(ui, plus_w) {
        let (r, resp) = ui.allocate_exact_size(vec2(plus_w, hdr.height() - 8.0), Sense::click());
        crate::ui::note_control(ui, r, "terminal new");
        if resp.hovered() {
            ui.painter().rect_filled(r, CornerRadius::same(5), pal.hover);
        }
        paint_glyph(ui.painter(), Rect::from_center_size(r.center(), Vec2::splat(13.0)), Glyph::Plus, pal.text_dim);
        if crate::ui::cmd_tip(resp, None, Cmd::NewTerminal, keymap, pal).clicked() {
            out = Some(Tabbed::New);
        }
    }
    out
}

/// A path with the home folder written as ~.
fn tilde(p: &Path) -> String {
    match p.strip_prefix(crate::fsx::home()) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

/// A cell's text and background colours.
fn colors(c: Cell, picked: bool, ansi: &[Color32; 16], pal: &Palette) -> (Color32, Color32) {
    if picked {
        return (pal.on_sel(), pal.selected);
    }
    let f = c.flags();
    // Bold text in one of the eight colours takes its bright twin, as xterm does.
    let fg = if f & vt::BOLD != 0 && c.fg < 8 { c.fg + 8 } else { c.fg };
    let (mut fg, mut bg) = (color(fg, ansi, pal.text), color(c.bg, ansi, pal.bg));
    if f & vt::INVERSE != 0 {
        (fg, bg) = (bg, fg);
    }
    if f & vt::DIM != 0 {
        fg = fg.lerp_to_gamma(bg, 0.4);
    }
    if f & vt::HIDDEN != 0 {
        fg = bg;
    }
    (fg, bg)
}
