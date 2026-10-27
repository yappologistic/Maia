//! The picker window: Maia opened to answer an app's Open or Save request (portal.rs).
//! It is the ordinary window, with every view, the filter, Go To, the palette and
//! previews, plus a bar along the bottom: the name to save under, the app's file filter
//! and its own questions, Cancel and the accept button. Enter picks what is selected,
//! Escape cancels. The answer goes out when the window closes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use egui::{Align2, CornerRadius, FontId, Id, Key, Modifiers, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

use crate::app::App;
use crate::fsx::Entry;
use crate::portal::{Answer, Mode, Pick};
use crate::theme::{Glyph, paint_glyph};

// ------------------------------------------------------------------ the app's filter

/// The files the chosen filter lets through: glob patterns (case-sensitive, as the
/// portal says) and MIME types, `image/*` for any image, a type's subtypes included.
#[derive(Debug, Default)]
pub struct Accept {
    folders_only: bool,
    globs: Vec<Vec<char>>,
    mimes: Vec<String>,
}

impl Accept {
    pub fn new(p: &Pick, filter: Option<usize>) -> Option<Accept> {
        if p.directory || p.mode == Mode::SaveFiles {
            return Some(Accept { folders_only: true, ..Default::default() });
        }
        let f = p.filters.get(filter?)?;
        let mut a = Accept::default();
        for (kind, s) in &f.rules {
            match kind {
                0 => a.globs.push(s.chars().collect()),
                1 => a.mimes.push(s.to_ascii_lowercase()),
                _ => {}
            }
        }
        // A filter with no rule Maia knows would hide everything: none at all then.
        (!a.globs.is_empty() || !a.mimes.is_empty()).then_some(a)
    }

    pub fn ok(&self, e: &Entry) -> bool {
        if e.is_dir {
            return true;
        }
        if self.folders_only {
            return false;
        }
        if !self.globs.is_empty() {
            // One buffer per thread (big folders are filtered on several), not one per file.
            thread_local!(static NAME: std::cell::RefCell<Vec<char>> = const { std::cell::RefCell::new(Vec::new()) });
            let hit = NAME.with(|b| {
                let mut b = b.borrow_mut();
                b.clear();
                b.extend(e.name.chars());
                self.globs.iter().any(|g| glob(g, &b))
            });
            if hit {
                return true;
            }
        }
        if self.mimes.is_empty() {
            return false;
        }
        let mime = crate::mime::mime_db().for_name(&e.name, false, e.is_executable());
        self.mimes.iter().any(|m| mime_is(&mime, m))
    }
}

/// Where a file an app wants saved goes: in the folder the user picked, under the last
/// plain part of the name the app suggested. The app may be sandboxed, and the name is
/// its to choose, so "../../.profile" or "/home/u/.bashrc" lands in `dir` all the same.
/// None for a name with no plain part ("..", "/").
pub fn save_path(dir: &Path, name: &str) -> Option<PathBuf> {
    let parts = Path::new(name).components().filter_map(|c| match c {
        std::path::Component::Normal(part) => Some(part),
        _ => None,
    });
    parts.last().map(|part| dir.join(part))
}

/// `*`, `?` and `[abc]` / `[a-z]` / `[!a]` classes, as shell globs and the portal's
/// filters (`*.[pP][nN][gG]`) use them.
pub fn glob(pat: &[char], name: &[char]) -> bool {
    // One class at p: (matches c, length of the class), or None when it isn't one.
    fn class(pat: &[char], p: usize, c: char) -> Option<(bool, usize)> {
        let mut i = p + 1;
        let neg = matches!(pat.get(i), Some('!' | '^'));
        if neg {
            i += 1;
        }
        let mut hit = false;
        let mut first = true;
        while let Some(&x) = pat.get(i) {
            if x == ']' && !first {
                return Some((hit != neg, i + 1 - p));
            }
            first = false;
            if pat.get(i + 1) == Some(&'-') && pat.get(i + 2).is_some_and(|&y| y != ']') {
                let y = pat[i + 2];
                hit |= x <= c && c <= y;
                i += 3;
            } else {
                hit |= x == c;
                i += 1;
            }
        }
        None
    }
    let (mut p, mut n) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while n < name.len() {
        let step = match pat.get(p) {
            Some('*') => {
                star = Some(p);
                mark = n;
                p += 1;
                continue;
            }
            Some('?') => Some(1),
            Some('[') => match class(pat, p, name[n]) {
                Some((true, len)) => Some(len),
                Some((false, _)) => None,
                None => (name[n] == '[').then_some(1),
            },
            Some(&c) => (c == name[n]).then_some(1),
            None => None,
        };
        match step {
            Some(len) => {
                p += len;
                n += 1;
            }
            None => match star {
                Some(s) => {
                    p = s + 1;
                    mark += 1;
                    n = mark;
                }
                None => return false,
            },
        }
    }
    pat[p.min(pat.len())..].iter().all(|&c| c == '*')
}

/// The shared MIME database's subclasses and aliases: `text/x-csrc` is a `text/plain`.
fn parents() -> &'static std::collections::HashMap<String, Vec<String>> {
    static P: OnceLock<std::collections::HashMap<String, Vec<String>>> = OnceLock::new();
    P.get_or_init(|| {
        let mut m: std::collections::HashMap<String, Vec<String>> = Default::default();
        let mut dirs = vec![dirs::data_dir().unwrap_or_else(|| crate::fsx::home().join(".local/share"))];
        dirs.extend(std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into()).split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
        for d in dirs {
            for f in ["mime/subclasses", "mime/aliases"] {
                let Ok(t) = std::fs::read_to_string(d.join(f)) else { continue };
                for l in t.lines() {
                    if let Some((a, b)) = l.split_once(' ') {
                        let v = m.entry(a.to_ascii_lowercase()).or_default();
                        if !v.iter().any(|x| x == b) {
                            v.push(b.to_ascii_lowercase());
                        }
                    }
                }
            }
        }
        m
    })
}

/// Whether a file of type `mime` is what `want` asks for (`image/*`, `text/plain`).
pub fn mime_is(mime: &str, want: &str) -> bool {
    let mut seen = vec![mime.to_string()];
    let mut i = 0;
    while i < seen.len() && i < 32 {
        let m = seen[i].clone();
        let hit = match want.strip_suffix("/*") {
            Some(major) => m.split('/').next() == Some(major),
            None => want == "*" || m == want,
        };
        if hit {
            return true;
        }
        // Every text file is plain text underneath, as the shared database says.
        if m.starts_with("text/") && !seen.iter().any(|s| s == "text/plain") {
            seen.push("text/plain".into());
        }
        for p in parents().get(&m).into_iter().flatten() {
            if !seen.contains(p) {
                seen.push(p.clone());
            }
        }
        i += 1;
    }
    false
}

// ------------------------------------------------------------------ the window

/// What the bar has open: the filter's list, or one of the app's questions.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Menu {
    Filter,
    Choice(usize),
}

pub struct Picker {
    pub pick: Pick,
    pub filter: Option<usize>,
    pub accept: Option<Arc<Accept>>,
    /// Save: the name in the field.
    pub name: String,
    /// The option chosen for each of the app's questions.
    pub chosen: Vec<String>,
    menu: Option<Menu>,
    /// The row lit in the open menu.
    menu_sel: usize,
    /// Focus the name field, its name part selected.
    focus_name: bool,
    /// The single file last selected, whose name went into the field.
    named_from: Option<PathBuf>,
    /// How the window ended, once it has.
    pub done: Option<Answer>,
}

/// The answer and the folder it was picked in, for main to write out after the window
/// has gone.
pub static ANSWER: Mutex<Option<(Answer, Option<PathBuf>)>> = Mutex::new(None);

static PICKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// This process is a picker window (no tour, no session of its own).
pub fn picking() -> bool {
    PICKING.load(std::sync::atomic::Ordering::Relaxed)
}

fn remembered() -> PathBuf {
    crate::config::data_dir().join("picker-folder")
}

impl Picker {
    pub fn new(pick: Pick) -> Picker {
        let filter = pick.current_filter.or((!pick.filters.is_empty()).then_some(0));
        let accept = Accept::new(&pick, filter).map(Arc::new);
        let chosen = pick.choices.iter().map(|c| if c.chosen.is_empty() { c.options.first().map_or("false".into(), |o| o.0.clone()) } else { c.chosen.clone() }).collect();
        let name = match (&pick.current_file, pick.mode) {
            (Some(f), Mode::Save) => f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            _ => pick.current_name.clone(),
        };
        let focus_name = pick.mode == Mode::Save;
        Picker { pick, filter, accept, name, chosen, menu: None, menu_sel: 0, focus_name, named_from: None, done: None }
    }

    /// Where the window opens: the file or folder the app gave, else where the last
    /// pick was made, else home. A file opens its folder with it selected.
    pub fn start(&self) -> PathBuf {
        let p = &self.pick;
        if let Some(f) = p.current_file.as_ref().filter(|f| f.exists()) {
            return f.clone();
        }
        if let Some(d) = p.current_folder.as_ref().filter(|d| d.is_dir()) {
            return d.clone();
        }
        std::fs::read_to_string(remembered()).ok().map(|s| PathBuf::from(s.trim())).filter(|d| d.is_absolute() && d.is_dir()).unwrap_or_else(crate::fsx::home)
    }

    pub fn title(&self) -> String {
        if !self.pick.title.is_empty() {
            return self.pick.title.clone();
        }
        match (self.pick.mode, self.pick.directory) {
            (Mode::Open, true) => "Choose a Folder".into(),
            (Mode::Open, false) => if self.pick.multiple { "Open Files" } else { "Open File" }.into(),
            _ => "Save".into(),
        }
    }

    pub fn accept_label(&self) -> String {
        if !self.pick.accept_label.is_empty() {
            return self.pick.accept_label.clone();
        }
        match (self.pick.mode, self.pick.directory) {
            (Mode::Open, true) => "Select".into(),
            (Mode::Open, false) => "Open".into(),
            _ => "Save".into(),
        }
    }

    fn set_filter(&mut self, i: usize) {
        self.filter = Some(i);
        self.accept = Accept::new(&self.pick, self.filter).map(Arc::new);
    }

    fn answer(&self, uris: Vec<String>) -> Answer {
        let choices = self.pick.choices.iter().zip(&self.chosen).map(|(c, v)| (c.id.clone(), v.clone())).collect();
        Answer { response: 0, uris, choices, filter: self.filter }
    }
}

/// A process about to open a picker window.
pub fn begin() {
    PICKING.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Read the MIME tables a filter by type needs before the window opens, not while the
/// list is first drawn.
pub fn warm(p: &Pick) {
    if p.filters.iter().any(|f| f.rules.iter().any(|r| r.0 == 1)) {
        let _ = crate::mime::mime_db();
        let _ = parents();
    }
}

fn finish(app: &mut App, a: Answer, folder: Option<PathBuf>) {
    let Some(p) = app.picker.as_mut() else { return };
    *ANSWER.lock().unwrap_or_else(|e| e.into_inner()) = Some((a.clone(), folder));
    p.done = Some(a);
    app.quit = true;
}

pub fn cancel(app: &mut App) {
    finish(app, Answer::cancelled(), None);
}

/// After the window closed: write the answer for the backend (cancelled when the window
/// was closed without one), and remember where it was picked.
pub fn write_answer(out: &mut impl std::io::Write) {
    let (a, folder) = ANSWER.lock().unwrap_or_else(|e| e.into_inner()).take().unwrap_or((Answer::cancelled(), None));
    if let Some(f) = folder {
        let _ = std::fs::create_dir_all(crate::config::data_dir());
        let _ = std::fs::write(remembered(), f.to_string_lossy().as_bytes());
    }
    let _ = out.write_all(&a.encode());
    let _ = out.flush();
}

/// The folders selected, in the order listed: in folder mode the cursor alone is not a
/// choice (it rests on the first row), the folder shown is.
fn selected_folders(app: &mut App) -> Vec<PathBuf> {
    let t = app.tab();
    let mut v: Vec<(usize, PathBuf)> = t.selected.iter().filter_map(|&i| Some((t.view_pos(i)?, t.entries.get(i as usize).filter(|e| e.is_dir)?.path.clone()))).collect();
    v.sort();
    v.into_iter().map(|x| x.1).collect()
}

/// The folder shown, when it is a real one (not Computer, not inside an archive).
fn real_folder(app: &mut App) -> Option<PathBuf> {
    let t = app.tab();
    (t.archive.is_none() && !crate::tab::is_place(&t.path) && t.path.is_dir()).then(|| t.path.clone())
}

fn uri(p: &Path) -> String {
    crate::clipboard::path_to_uri(p)
}

/// The accept button, or Enter: what it answers depends on the mode.
pub fn accept(app: &mut App) {
    let Some(p) = app.picker.as_ref() else { return };
    let (mode, directory, multiple) = (p.pick.mode, p.pick.directory, p.pick.multiple);
    let inside_archive = app.tab().archive.is_some();
    match mode {
        Mode::Open if directory => {
            let sel = selected_folders(app);
            let picked = if sel.is_empty() { real_folder(app).into_iter().collect() } else { sel };
            let picked: Vec<PathBuf> = picked.into_iter().filter(|d| d.is_dir() && !inside_archive).take(if multiple { usize::MAX } else { 1 }).collect();
            if picked.is_empty() {
                app.toast("Choose a folder on this computer");
                return;
            }
            let here = real_folder(app);
            let a = app.picker.as_ref().map(|p| p.answer(picked.iter().map(|d| uri(d)).collect()));
            if let Some(a) = a {
                finish(app, a, here);
            }
        }
        Mode::Open => {
            let files: Vec<PathBuf> = app.tab().target_entries().iter().filter(|e| !e.is_dir).map(|e| e.path.clone()).collect();
            if inside_archive && !files.is_empty() {
                app.toast("Files inside an archive can't be handed over: extract them first");
                return;
            }
            let files: Vec<PathBuf> = if multiple { files } else { files.into_iter().take(1).collect() };
            if files.is_empty() {
                // A folder alone: go into it, as Enter does anywhere else.
                if let Some(d) = app.tab().target_entries().first().filter(|e| e.is_dir).map(|e| e.path.clone()) {
                    let id = app.active;
                    app.navigate(id, d);
                }
                return;
            }
            let here = real_folder(app);
            let a = app.picker.as_ref().map(|p| p.answer(files.iter().map(|f| uri(f)).collect()));
            if let Some(a) = a {
                finish(app, a, here);
            }
        }
        Mode::SaveFiles => {
            let Some(dir) = selected_folders(app).into_iter().next().or_else(|| real_folder(app)) else {
                app.toast("Choose a folder on this computer to save in");
                return;
            };
            let names = app.picker.as_ref().map(|p| p.pick.files.clone()).unwrap_or_default();
            let paths: Vec<PathBuf> = names.iter().filter_map(|n| save_path(&dir, n)).collect();
            let there: Vec<&PathBuf> = paths.iter().filter(|p| p.exists()).collect();
            let a = app.picker.as_ref().map(|p| p.answer(paths.iter().map(|f| uri(f)).collect()));
            let Some(a) = a else { return };
            if there.is_empty() {
                finish(app, a, Some(dir));
            } else {
                let msg = if there.len() == 1 { format!("“{}” is already there", file_name(there[0])) } else { format!("{} of these files are already there", there.len()) };
                ask_replace(app, msg, a, dir);
            }
        }
        Mode::Save => save(app),
    }
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn ask_replace(app: &mut App, msg: String, a: Answer, dir: PathBuf) {
    // The Enter that saved must not also answer the question, drawn later this frame.
    app.ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
    let go: Box<dyn FnOnce(&mut App)> = Box::new(move |app: &mut App| finish(app, a, Some(dir)));
    app.popup = crate::popups::Popup::Choose { msg, sub: "Replacing it overwrites what is in it.".into(), choices: vec![("Replace".into(), true, Some(go))] };
}

/// Save: the name in the field, in the folder shown (or a path typed in it). A folder's
/// name goes into it; a name already taken asks first.
fn save(app: &mut App) {
    let Some(name) = app.picker.as_ref().map(|p| p.name.trim().to_string()) else { return };
    if name.is_empty() {
        if let Some(p) = app.picker.as_mut() {
            p.focus_name = true;
        }
        return;
    }
    let typed = PathBuf::from(&name);
    let path = if let Some(rest) = name.strip_prefix("~/") {
        crate::fsx::home().join(rest)
    } else if typed.is_absolute() {
        typed
    } else {
        let Some(here) = real_folder(app) else {
            app.toast("Choose a folder on this computer to save in");
            return;
        };
        here.join(typed)
    };
    if path.is_dir() {
        let id = app.active;
        app.navigate(id, path);
        if let Some(p) = app.picker.as_mut() {
            p.name.clear();
            p.focus_name = true;
        }
        return;
    }
    let Some(dir) = path.parent().filter(|d| d.is_dir()).map(Path::to_path_buf) else {
        app.toast(format!("There is no folder {}", path.parent().map(|d| d.display().to_string()).unwrap_or_default()));
        return;
    };
    if crate::archive::split(&path).is_some() {
        app.toast("An archive can't be saved into");
        return;
    }
    let Some(a) = app.picker.as_ref().map(|p| p.answer(vec![uri(&path)])) else { return };
    if path.exists() {
        ask_replace(app, format!("“{}” is already there", file_name(&path)), a, dir);
    } else {
        finish(app, a, Some(dir));
    }
}

/// Enter in the list. True when the picker took it.
pub fn enter(app: &mut App) -> bool {
    let Some(p) = app.picker.as_ref() else { return false };
    let (mode, directory) = (p.pick.mode, p.pick.directory);
    let targets: Vec<(PathBuf, bool)> = app.tab().target_entries().iter().map(|e| (e.path.clone(), e.is_dir)).collect();
    match mode {
        Mode::Open if directory => accept(app),
        Mode::SaveFiles => accept(app),
        // Folders open as everywhere else; files are the answer.
        Mode::Open => {
            if targets.iter().all(|(_, d)| *d) {
                return false;
            }
            accept(app);
        }
        Mode::Save => {
            if targets.len() == 1 && targets[0].1 {
                return false;
            }
            if let (Some(p), [(f, false)]) = (app.picker.as_mut(), targets.as_slice()) {
                p.name = file_name(f);
                p.named_from = Some(f.clone());
            }
            save(app);
        }
    }
    true
}

/// A double click (or Enter on one item) on `idx`. True when the picker took it: a file
/// in Open mode is picked, one in Save mode names the file. Folders open as anywhere.
pub fn activate(app: &mut App, panel: u64, idx: u32) -> bool {
    let Some(p) = app.picker.as_ref() else { return false };
    let mode = p.pick.mode;
    let Some(e) = app.panels.get(&panel).and_then(|p| p.tab().entries.get(idx as usize)).cloned() else { return false };
    if e.is_dir {
        return false;
    }
    if let Some(p) = app.panels.get_mut(&panel) {
        p.tab_mut().select_only(idx);
    }
    match mode {
        Mode::Open if !app.picker.as_ref().is_some_and(|p| p.pick.directory) => accept(app),
        Mode::Save => {
            if let Some(p) = app.picker.as_mut() {
                p.name = e.name.clone();
                p.named_from = Some(e.path.clone());
            }
            save(app);
        }
        _ => {}
    }
    true
}

/// Alt+T in a picker with filters: their list. True when it opened.
pub fn open_filters(app: &mut App) -> bool {
    let Some(p) = app.picker.as_mut().filter(|p| !p.pick.filters.is_empty()) else { return false };
    p.menu = Some(Menu::Filter);
    p.menu_sel = p.filter.unwrap_or(0);
    true
}

/// Before the app reads the keyboard: an open menu takes the arrows, Enter and Escape;
/// otherwise Escape cancels (once the filter is empty, which it clears first).
pub fn keys(app: &mut App, ctx: &egui::Context) {
    let Some(p) = app.picker.as_mut() else { return };
    if p.done.is_some() {
        return;
    }
    if let Some(m) = p.menu {
        let n = menu_len(p, m);
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::Escape) {
                p.menu = None;
            } else if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                p.menu_sel = (p.menu_sel + 1).min(n.saturating_sub(1));
            } else if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                p.menu_sel = p.menu_sel.saturating_sub(1);
            } else if i.consume_key(Modifiers::NONE, Key::Enter) {
                let sel = p.menu_sel;
                choose(p, m, sel);
            }
        });
        return;
    }
    let busy = !matches!(app.popup, crate::popups::Popup::None) || app.hints.is_some() || app.terminal.focused;
    if busy {
        return;
    }
    let Some(t) = app.panels.get(&app.active).map(|p| p.tab()) else { return };
    if t.rename.is_some() || !t.filter.is_empty() || t.flatten {
        return;
    }
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        cancel(app);
    }
}

fn menu_len(p: &Picker, m: Menu) -> usize {
    match m {
        Menu::Filter => p.pick.filters.len(),
        Menu::Choice(i) => p.pick.choices.get(i).map_or(0, |c| c.options.len()),
    }
}

fn choose(p: &mut Picker, m: Menu, i: usize) {
    match m {
        Menu::Filter if i < p.pick.filters.len() => p.set_filter(i),
        Menu::Choice(c) => {
            if let (Some(v), Some(slot)) = (p.pick.choices.get(c).and_then(|ch| ch.options.get(i)).map(|o| o.0.clone()), p.chosen.get_mut(c)) {
                *slot = v;
            }
        }
        _ => {}
    }
    p.menu = None;
}

/// Every tab lists what the chosen filter lets through, new tabs too; and in Save mode a
/// file selected in the list puts its name in the field.
pub fn sync(app: &mut App) {
    let Some(p) = app.picker.as_mut() else { return };
    let want = p.accept.clone();
    for t in app.panels.values_mut().flat_map(|p| p.tabs.iter_mut()) {
        if t.pick.as_ref().map(Arc::as_ptr) != want.as_ref().map(Arc::as_ptr) {
            t.pick = want.clone();
            t.dirty = true;
        }
    }
    if p.pick.mode == Mode::Save {
        let one = app.panels.get(&app.active).map(|q| q.tab()).and_then(|t| (t.selected.len() == 1).then(|| t.target_entries().first().map(|e| (e.path.clone(), e.name.clone(), e.is_dir))).flatten());
        if let Some((path, name, false)) = one {
            if p.named_from.as_ref() != Some(&path) {
                p.name = name;
                p.named_from = Some(path);
            }
        }
    }
}

/// The bar's height for the font and Spacing in use (the shelf's, with room for a field).
pub fn height(app: &App) -> f32 {
    (app.cfg.font_size * 1.35 + 14.0 * app.cfg.spacing + 14.0).round()
}

/// What Accept would answer, said at the left of the bar in Open mode, and whether there
/// is anything to accept. One pass over the selection, nothing copied but the one name
/// shown: a selection can be a flattened folder's hundreds of thousands of files.
fn summary(app: &mut App) -> (String, bool) {
    let Some(p) = app.picker.as_ref() else { return (String::new(), false) };
    let (dir, multiple) = (p.pick.directory, p.pick.multiple);
    let t = app.tab();
    // Past the status bar's own limit for summing a selection, counted, not sorted out.
    if t.selected.len() > 200_000 {
        return (format!("{} items", crate::ui::fmt_count(t.selected.len())), true);
    }
    let lone = t.selected.is_empty();
    let idx: Box<dyn Iterator<Item = u32>> = if lone { Box::new(t.cursor.into_iter()) } else { Box::new(t.selected.iter().copied()) };
    // The kind counted (folders in folder mode, else files), the first of them in the
    // order listed, and whether a folder is among them (Enter goes into it).
    let (mut n, mut first, mut folder) = (0usize, None::<(usize, u32)>, false);
    for i in idx {
        let Some(e) = t.entries.get(i as usize) else { continue };
        folder |= e.is_dir;
        if e.is_dir != dir || (dir && lone) {
            continue;
        }
        n += 1;
        let pos = t.view_pos(i).unwrap_or(usize::MAX);
        if first.is_none_or(|(p, _)| pos < p) {
            first = Some((pos, i));
        }
    }
    let name = |i: u32| t.entries.get(i as usize).map(|e| e.name.clone()).unwrap_or_default();
    let text = match (n, multiple, first) {
        (0, _, _) if dir => t.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| t.path.display().to_string()),
        (0, _, _) => String::new(),
        (1, _, Some((_, i))) | (_, false, Some((_, i))) => name(i),
        (n, _, _) => format!("{n} {}", if dir { "folders" } else { "files" }),
    };
    let can = dir || n > 0 || folder;
    (text, can)
}

/// A button of the bar, drawn as the shelf's are. True when clicked.
fn button(ui: &mut Ui, r: Rect, label: &str, accent: bool, enabled: bool, what: &'static str, fs: f32, pal: &crate::theme::Palette) -> bool {
    let resp = ui.interact(r, Id::new(("picker button", what)), if enabled { Sense::click() } else { Sense::hover() });
    crate::ui::note_control(ui, r, what);
    let hover = enabled && resp.hovered();
    let fill = if accent { if hover { pal.accent.gamma_multiply(0.85) } else { pal.accent } } else if hover { pal.hover } else { pal.card() };
    let fill = if enabled { fill } else { fill.gamma_multiply(0.45) };
    ui.painter().rect(r, CornerRadius::same(6), fill, if accent { Stroke::NONE } else { Stroke::new(1.0f32, pal.card_line()) }, StrokeKind::Inside);
    let col = if accent { egui::Color32::WHITE } else { pal.text };
    let g = crate::listing::elided(ui, label, FontId::proportional(fs - 0.5), r.width() - 12.0, if enabled { col } else { col.gamma_multiply(0.6) });
    ui.painter().galley(pos2(r.center().x - g.size().x / 2.0, r.center().y - g.size().y / 2.0), g, col);
    enabled && resp.clicked()
}

/// A chip that opens a menu: its text, elided to `w`, and a chevron.
fn chip(ui: &mut Ui, r: Rect, text: &str, open: bool, what: &'static str, fs: f32, pal: &crate::theme::Palette) -> bool {
    let resp = ui.interact(r, Id::new(("picker chip", what, r.left() as i32)), Sense::click());
    crate::ui::note_control(ui, r, what);
    let fill = if open { pal.selected } else if resp.hovered() { pal.hover } else { pal.card() };
    ui.painter().rect(r, CornerRadius::same(6), fill, Stroke::new(1.0f32, pal.card_line()), StrokeKind::Inside);
    let g = crate::listing::elided(ui, text, FontId::proportional(fs - 1.0), r.width() - 30.0, pal.text);
    ui.painter().galley(pos2(r.left() + 9.0, r.center().y - g.size().y / 2.0), g, pal.text);
    paint_glyph(ui.painter(), Rect::from_center_size(pos2(r.right() - 12.0, r.center().y), Vec2::splat(10.0)), if open { Glyph::ChevronDown } else { Glyph::ChevronUp }, pal.text_dim);
    resp.clicked()
}

/// The bar along the bottom of a picker window.
pub fn bar(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    let fs = app.cfg.font_size;
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, CornerRadius::ZERO, pal.chrome);
    ui.painter().line_segment([rect.left_top(), rect.right_top()], Stroke::new(1.0f32, pal.border));
    let inner = rect.shrink2(vec2(10.0, 0.0));
    let cy = inner.center().y;
    let h = (fs * 1.35 + 8.0 * app.cfg.spacing).round().min(rect.height() - 8.0);
    let text_w = |ui: &Ui, s: &str, size: f32| ui.painter().layout_no_wrap(s.to_string(), FontId::proportional(size), pal.text).size().x;
    let (said, open_ok) = if app.picker.as_ref().is_some_and(|p| p.pick.mode == Mode::Open) { summary(app) } else { (String::new(), true) };
    let Some(p) = app.picker.as_ref() else { return };
    let mode = p.pick.mode;
    let label = p.accept_label();
    // ---------------- right: the accept button, then Cancel
    let can = match mode {
        Mode::Save => !p.name.trim().is_empty(),
        Mode::Open => open_ok,
        _ => true,
    };
    let aw = (text_w(ui, &label, fs - 0.5) + 28.0).clamp(84.0, 180.0);
    let ar = Rect::from_min_max(pos2(inner.right() - aw, cy - h / 2.0), pos2(inner.right(), cy + h / 2.0));
    let cw = (text_w(ui, "Cancel", fs - 0.5) + 28.0).max(84.0);
    let cr = Rect::from_min_max(pos2(ar.left() - 8.0 - cw, ar.top()), pos2(ar.left() - 8.0, ar.bottom()));
    let mut right = cr.left() - 12.0;
    if button(ui, ar, &label, true, can, "picker accept", fs, &pal) {
        accept(app);
    }
    if button(ui, cr, "Cancel", false, true, "picker cancel", fs, &pal) {
        cancel(app);
        return;
    }
    let Some(p) = app.picker.as_ref() else { return };
    // ---------------- left: the name (Save), the filter, the app's questions
    let left = inner.left();
    let mut chips: Vec<(Rect, String, Menu, &'static str)> = vec![];
    let mut checks: Vec<(Rect, usize)> = vec![];
    let name_min = 150.0;
    let reserve = if mode == Mode::Save { name_min + 10.0 } else { 0.0 };
    let mut x = right;
    // Laid out from the right edge of the free space leftward, the filter first: a narrow
    // window loses the questions before the filter, and the filter before the name.
    let place = |w: f32, x: &mut f32| -> Option<Rect> {
        let w = w.min(*x - left - reserve);
        if w < 60.0 {
            return None;
        }
        let r = Rect::from_min_max(pos2(*x - w, cy - h / 2.0), pos2(*x, cy + h / 2.0));
        *x = r.left() - 8.0;
        Some(r)
    };
    if let Some(f) = p.filter.and_then(|i| p.pick.filters.get(i)) {
        if let Some(r) = place((text_w(ui, &f.name, fs - 1.0) + 36.0).min(260.0), &mut x) {
            chips.push((r, f.name.clone(), Menu::Filter, "picker filter"));
        }
    }
    for (i, c) in p.pick.choices.iter().enumerate().rev() {
        let chosen = p.chosen.get(i).cloned().unwrap_or_default();
        if c.options.is_empty() {
            let w = text_w(ui, &c.label, fs - 1.0) + 34.0;
            if let Some(r) = place(w.min(220.0), &mut x) {
                checks.push((r, i));
            }
        } else {
            let shown = c.options.iter().find(|o| o.0 == chosen).map_or(chosen.clone(), |o| o.1.clone());
            let t = format!("{}: {shown}", c.label);
            if let Some(r) = place((text_w(ui, &t, fs - 1.0) + 36.0).min(240.0), &mut x) {
                chips.push((r, t, Menu::Choice(i), "picker choice"));
            }
        }
    }
    right = x;
    let open = p.menu;
    let mut opened = None;
    for (r, t, m, what) in &chips {
        if chip(ui, *r, t, open == Some(*m), what, fs, &pal) {
            opened = Some((*m, *r));
        }
    }
    let mut toggled = None;
    for (r, i) in &checks {
        let resp = ui.interact(*r, Id::new(("picker check", *i)), Sense::click());
        crate::ui::note_control(ui, *r, "picker check");
        if resp.hovered() {
            ui.painter().rect_filled(*r, CornerRadius::same(6), pal.hover);
        }
        let on = p.chosen.get(*i).is_some_and(|v| v == "true");
        crate::popups::checkbox_glyph(ui.painter(), Rect::from_center_size(pos2(r.left() + 14.0, cy), Vec2::splat(14.0)), on, &pal);
        let g = crate::listing::elided(ui, &p.pick.choices[*i].label, FontId::proportional(fs - 1.0), r.width() - 32.0, pal.text);
        ui.painter().galley(pos2(r.left() + 28.0, cy - g.size().y / 2.0), g, pal.text);
        if resp.clicked() {
            toggled = Some(*i);
        }
    }
    if mode == Mode::Save {
        let w = (right - left).min(460.0);
        let fr = Rect::from_min_max(pos2(left, cy - h / 2.0), pos2(left + w, cy + h / 2.0));
        name_field(app, ui, fr, fs, &pal);
    } else if mode == Mode::SaveFiles {
        // The names the app will save under, as many as fit.
        let n = p.pick.files.len();
        let mut s = p.pick.files.iter().take(12).map(String::as_str).collect::<Vec<_>>().join(", ");
        if n > 12 {
            s += &format!(" and {} more", n - 12);
        }
        let g = crate::listing::elided(ui, &s, FontId::proportional(fs - 1.0), (right - left).max(8.0), pal.text_dim);
        ui.painter().galley(pos2(left, cy - g.size().y / 2.0), g, pal.text_dim);
    } else {
        let s = said;
        if !s.is_empty() && right - left > 30.0 {
            let g = crate::listing::elided(ui, &s, FontId::proportional(fs - 1.0), right - left, pal.text_dim);
            ui.painter().galley(pos2(left, cy - g.size().y / 2.0), g, pal.text_dim);
        }
    }
    let Some(p) = app.picker.as_mut() else { return };
    if let Some(i) = toggled {
        if let Some(v) = p.chosen.get_mut(i) {
            *v = if v == "true" { "false".into() } else { "true".into() };
        }
    }
    if let Some((m, _)) = opened {
        if p.menu == Some(m) {
            p.menu = None;
        } else {
            p.menu = Some(m);
            p.menu_sel = match m {
                Menu::Filter => p.filter.unwrap_or(0),
                Menu::Choice(i) => p.pick.choices.get(i).and_then(|c| c.options.iter().position(|o| Some(&o.0) == p.chosen.get(i))).unwrap_or(0),
            };
        }
    }
    // Where the open menu hangs from: above its chip.
    let anchor = p.menu.and_then(|m| chips.iter().find(|c| c.2 == m).map(|c| c.0));
    match anchor {
        Some(r) => menu(app, ui.ctx(), r),
        None => {
            if let Some(p) = app.picker.as_mut() {
                p.menu = None;
            }
        }
    }
}

fn name_field(app: &mut App, ui: &mut Ui, r: Rect, fs: f32, pal: &crate::theme::Palette) {
    let id = Id::new("picker name");
    let Some(p) = app.picker.as_mut() else { return };
    ui.painter().rect(r, CornerRadius::same(6), pal.bg, Stroke::new(1.0f32, pal.card_line()), StrokeKind::Inside);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink2(vec2(8.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    let te = egui::TextEdit::singleline(&mut p.name).id(id).frame(crate::ui::bare_edit()).font(FontId::proportional(fs)).desired_width(r.width() - 16.0).hint_text(egui::RichText::new("Name").color(pal.text_dim));
    let resp = child.add(te);
    crate::ui::note_control(ui, r, "picker name");
    if std::mem::take(&mut p.focus_name) {
        resp.request_focus();
        // The name part selected, the extension left: typing replaces just the name.
        let n = p.name.chars().count();
        let stem = match p.name.rfind('.') {
            Some(dot) if dot > 0 => p.name[..dot].chars().count(),
            _ => n,
        };
        let mut s = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
        s.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(stem))));
        s.store(ui.ctx(), id);
    }
    if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
        save(app);
    }
}

/// The open menu, over the bar: the filters or one question's options.
fn menu(app: &mut App, ctx: &egui::Context, chip: Rect) {
    let pal = app.pal;
    let Some(p) = app.picker.as_mut() else { return };
    let Some(m) = p.menu else { return };
    let rows: Vec<(String, bool)> = match m {
        Menu::Filter => p.pick.filters.iter().enumerate().map(|(i, f)| (f.name.clone(), p.filter == Some(i))).collect(),
        Menu::Choice(c) => p.pick.choices.get(c).map(|ch| ch.options.iter().map(|o| (o.1.clone(), p.chosen.get(c) == Some(&o.0))).collect()).unwrap_or_default(),
    };
    let sel = p.menu_sel;
    let width = rows.iter().map(|r| ctx.fonts_mut(|f| f.layout_no_wrap(r.0.clone(), FontId::proportional(12.5), pal.text).size().x)).fold(chip.width(), f32::max).clamp(160.0, 360.0) + 44.0;
    let mut picked = None;
    let (_, rect) = crate::popups::anchored_from(ctx, "picker menu", pos2(chip.left(), chip.top() - 4.0), Align2::LEFT_BOTTOM, width, &pal, |ui| {
        for (i, (label, on)) in rows.iter().enumerate() {
            let resp = crate::popups::row(ui, 24.0, i == sel, &pal, |ui, r| {
                let tc = if i == sel { pal.on_sel() } else { pal.text };
                let g = crate::listing::elided(ui, label, FontId::proportional(12.5), r.width() - 34.0, tc);
                ui.painter().galley(pos2(r.left() + 26.0, r.center().y - g.size().y / 2.0), g, tc);
                if *on {
                    let c = pos2(r.left() + 13.0, r.center().y);
                    ui.painter().add(egui::Shape::line(vec![c + vec2(-4.0, 0.0), c + vec2(-1.0, 3.0), c + vec2(4.5, -3.5)], Stroke::new(1.6f32, if i == sel { tc } else { pal.accent })));
                }
            });
            crate::ui::note_control(ui, resp.rect, "picker menu row");
            if resp.clicked() {
                picked = Some(i);
            }
        }
    });
    if let Some(i) = picked {
        choose(p, m, i);
    } else if crate::popups::clicked_outside(ctx, rect) && !ctx.pointer_interact_pos().is_some_and(|pp| chip.contains(pp)) {
        p.menu = None;
    }
}

/// On X11, a picker stays above the app's window and is placed as its dialog
/// (`x11:<hex XID>`). Wayland's parent handles need xdg-foreign on winit's own
/// connection, which winit does not offer: there it is a window of its own.
pub fn set_parent(window: &winit::window::Window, parent: &str) {
    use winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, PropMode};
    use x11rb::wrapper::ConnectionExt as _;
    let Some(xid) = parent.strip_prefix("x11:").and_then(|h| u32::from_str_radix(h.trim_start_matches("0x"), 16).ok()) else { return };
    let ours = match window.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Xlib(h)) => h.window as u32,
        Ok(RawWindowHandle::Xcb(h)) => h.window.get(),
        _ => return,
    };
    let Ok((conn, _)) = x11rb::xcb_ffi::XCBConnection::connect(None) else { return };
    let _ = conn.change_property32(PropMode::REPLACE, ours, AtomEnum::WM_TRANSIENT_FOR, AtomEnum::WINDOW, &[xid]);
    let atom = |name: &[u8]| conn.intern_atom(false, name).ok().and_then(|c| c.reply().ok()).map(|r| r.atom);
    if let (Some(ty), Some(dialog)) = (atom(b"_NET_WM_WINDOW_TYPE"), atom(b"_NET_WM_WINDOW_TYPE_DIALOG")) {
        let _ = conn.change_property32(PropMode::REPLACE, ours, ty, AtomEnum::ATOM, &[dialog]);
    }
    let _ = conn.flush();
}
