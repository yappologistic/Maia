//! MIME types (shared-mime-info), .desktop applications, and launching.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

pub struct MimeDb {
    by_ext: HashMap<String, String>,
    by_name: HashMap<String, String>,
    comments: Mutex<HashMap<String, String>>,
    labels: Mutex<HashMap<String, String>>,
}

#[derive(Clone, Debug)]
pub struct App {
    pub id: String,
    pub name: String,
    pub exec: String,
    pub mimes: Vec<String>,
    pub terminal: bool,
    pub hidden: bool,
    /// Icon= value: a theme icon name or an absolute path.
    pub icon: String,
}

pub struct Apps {
    pub apps: Vec<App>,
    defaults: HashMap<String, Vec<String>>,
    added: HashMap<String, Vec<String>>,
    removed: HashMap<String, Vec<String>>,
    /// mimeinfo.cache entries, user data dir first (the xdg-mime fallback order).
    cache: HashMap<String, Vec<String>>,
}

static MIME: OnceLock<MimeDb> = OnceLock::new();
static APPS: OnceLock<Apps> = OnceLock::new();

/// Warm the databases on a worker thread at startup.
pub fn preload() {
    std::thread::spawn(|| {
        let _ = mime_db();
        let _ = apps();
    });
}

fn data_dirs() -> Vec<PathBuf> {
    let mut v = vec![dirs::data_dir().unwrap_or_else(|| crate::fsx::home().join(".local/share"))];
    let sys = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    v.extend(sys.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    v
}

pub fn mime_db() -> &'static MimeDb {
    MIME.get_or_init(|| {
        let mut by_ext: HashMap<String, (i32, String)> = HashMap::new();
        let mut by_name = HashMap::new();
        for d in data_dirs().iter().rev() {
            let Ok(s) = std::fs::read_to_string(d.join("mime/globs2")) else { continue };
            for line in s.lines() {
                if line.starts_with('#') {
                    continue;
                }
                let mut it = line.splitn(4, ':');
                let (Some(w), Some(mime), Some(glob)) = (it.next(), it.next(), it.next()) else { continue };
                let w: i32 = w.parse().unwrap_or(50);
                if let Some(ext) = glob.strip_prefix("*.") {
                    if ext.contains(['*', '?', '[']) {
                        continue;
                    }
                    let ext = ext.to_lowercase();
                    match by_ext.get(&ext) {
                        Some((ow, _)) if *ow >= w => {}
                        _ => {
                            by_ext.insert(ext, (w, mime.to_string()));
                        }
                    }
                } else if !glob.contains(['*', '?', '[']) {
                    by_name.insert(glob.to_string(), mime.to_string());
                }
            }
        }
        MimeDb {
            by_ext: by_ext.into_iter().map(|(k, (_, m))| (k, m)).collect(),
            by_name,
            comments: Mutex::new(HashMap::new()),
            labels: Mutex::new(HashMap::new()),
        }
    })
}

impl MimeDb {
    pub fn for_path(&self, path: &Path, is_dir: bool) -> String {
        let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
        let exec = !is_dir && crate::fsx::Entry::from_path(path).map(|e| e.is_executable()).unwrap_or(false);
        self.for_name(&name, is_dir, exec)
    }

    /// MIME from the name alone (no filesystem access).
    pub fn for_name(&self, name: &str, is_dir: bool, exec: bool) -> String {
        if is_dir {
            return "inode/directory".into();
        }
        if let Some(m) = self.by_name.get(name) {
            return m.clone();
        }
        // Longest compound extension first (e.g. tar.gz).
        let lname = name.to_lowercase();
        let mut idx = 0;
        while let Some(p) = lname[idx..].find('.') {
            let ext = &lname[idx + p + 1..];
            if let Some(m) = self.by_ext.get(ext) {
                return m.clone();
            }
            idx += p + 1;
        }
        if exec {
            return "application/x-executable".into();
        }
        "application/octet-stream".into()
    }

    /// Whether a file of this name has a type of its own ("Makefile"), whatever its
    /// extension: its label is not its extension's.
    pub fn named(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    /// The Type column label: "Folder", the MIME description, "<EXT> File" for unknown
    /// extensions, or "File" without one. Cached per extension.
    pub fn label(&self, name: &str, ext: &str, is_dir: bool, exec: bool) -> String {
        if is_dir {
            return "Folder".into();
        }
        let key = format!("{}\0{ext}\0{}", if self.by_name.contains_key(name) { name } else { "" }, exec as u8);
        if let Some(l) = self.labels.lock().unwrap().get(&key) {
            return l.clone();
        }
        let mime = self.for_name(name, false, exec);
        let label = if mime == "application/octet-stream" {
            if ext.is_empty() { "File".to_string() } else { format!("{} File", ext.to_uppercase()) }
        } else {
            let c = self.comment(&mime);
            if c.is_empty() { format!("{} File", ext.to_uppercase()) } else { c }
        };
        self.labels.lock().unwrap().insert(key, label.clone());
        label
    }

    /// The kind as a row says it: the description cut to its point ("Text", "Markdown",
    /// "PNG image", "Spreadsheet"). The whole description stays where there is room for
    /// it, the preview and Properties. Cached per extension, as `label` is.
    pub fn short(&self, name: &str, ext: &str, is_dir: bool, exec: bool) -> String {
        if is_dir {
            return "Folder".into();
        }
        let key = format!("short\0{}\0{ext}\0{}", if self.by_name.contains_key(name) { name } else { "" }, exec as u8);
        if let Some(l) = self.labels.lock().ok().and_then(|m| m.get(&key).cloned()) {
            return l;
        }
        let long = self.label(name, ext, false, exec);
        let mime = self.for_name(name, false, exec);
        let short = short_kind(&mime, &long);
        if let Ok(mut m) = self.labels.lock() {
            m.insert(key, short.clone());
        }
        short
    }

    /// Human readable description ("Shell script"), from the mime XML.
    pub fn comment(&self, mime: &str) -> String {
        if let Some(c) = self.comments.lock().unwrap().get(mime) {
            return c.clone();
        }
        let mut out = String::new();
        for d in data_dirs() {
            if let Ok(s) = std::fs::read_to_string(d.join("mime").join(format!("{mime}.xml"))) {
                // First <comment> without xml:lang is the English one.
                for part in s.split("<comment").skip(1) {
                    if part.starts_with('>') {
                        if let Some(end) = part.find("</comment>") {
                            out = part[1..end].to_string();
                        }
                        break;
                    }
                }
                if !out.is_empty() {
                    break;
                }
            }
        }
        if let Some(first) = out.get(0..1) {
            out = first.to_uppercase() + &out[1..];
        }
        self.comments.lock().unwrap().insert(mime.to_string(), out.clone());
        out
    }
}

pub fn apps() -> &'static Apps {
    APPS.get_or_init(|| {
        let mut apps: HashMap<String, App> = HashMap::new();
        // Lowest priority first so user entries override system ones.
        for d in data_dirs().iter().rev() {
            scan_apps(&d.join("applications"), "", &mut apps);
        }
        let mut defaults: HashMap<String, Vec<String>> = HashMap::new();
        let mut added: HashMap<String, Vec<String>> = HashMap::new();
        let mut removed: HashMap<String, Vec<String>> = HashMap::new();
        // Same precedence as xdg-mime: desktop-specific lists before generic ones,
        // config dirs before data dirs, user before system.
        let desktops: Vec<String> = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().split(':').filter(|s| !s.is_empty()).map(|s| s.to_lowercase()).collect();
        let mut dirs_cfg = vec![dirs::config_dir().unwrap_or_default()];
        dirs_cfg.extend(std::env::var("XDG_CONFIG_DIRS").unwrap_or_else(|_| "/etc/xdg".into()).split(':').map(PathBuf::from));
        let mut lists = vec![];
        for d in dirs_cfg.iter().cloned().chain(data_dirs().into_iter().map(|d| d.join("applications"))) {
            for de in &desktops {
                lists.push(d.join(format!("{de}-mimeapps.list")));
            }
            lists.push(d.join("mimeapps.list"));
        }
        for d in data_dirs() {
            lists.push(d.join("applications/defaults.list"));
        }
        for l in lists {
            let Ok(s) = std::fs::read_to_string(&l) else { continue };
            let mut section = String::new();
            for line in s.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    section = line.to_string();
                    continue;
                }
                let Some((k, v)) = line.split_once('=') else { continue };
                let ids: Vec<String> = v.split(';').filter(|s| !s.is_empty()).map(String::from).collect();
                let target = match section.as_str() {
                    "[Default Applications]" => &mut defaults,
                    "[Added Associations]" => &mut added,
                    "[Removed Associations]" => &mut removed,
                    _ => continue,
                };
                // Earlier files win for defaults; associations accumulate.
                if section == "[Default Applications]" {
                    target.entry(k.to_string()).or_insert(ids);
                } else {
                    target.entry(k.to_string()).or_default().extend(ids);
                }
            }
        }
        let mut cache: HashMap<String, Vec<String>> = HashMap::new();
        for d in data_dirs() {
            let Ok(s) = std::fs::read_to_string(d.join("applications/mimeinfo.cache")) else { continue };
            for line in s.lines() {
                if let Some((k, v)) = line.split_once('=') {
                    let e = cache.entry(k.to_string()).or_default();
                    for id in v.split(';').filter(|s| !s.is_empty()) {
                        if !e.iter().any(|x| x == id) {
                            e.push(id.to_string());
                        }
                    }
                }
            }
        }
        let mut list: Vec<App> = apps.into_values().collect();
        list.sort_by_key(|a| a.name.to_lowercase());
        Apps { apps: list, defaults, added, removed, cache }
    })
}

fn scan_apps(dir: &Path, prefix: &str, out: &mut HashMap<String, App>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for de in rd.flatten() {
        let p = de.path();
        let fname = de.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            scan_apps(&p, &format!("{prefix}{fname}-"), out);
        } else if fname.ends_with(".desktop") {
            let id = format!("{prefix}{fname}");
            if let Some(app) = parse_desktop(&p, &id) {
                out.insert(id, app);
            } else {
                out.remove(&id);
            }
        }
    }
}

fn parse_desktop(p: &Path, id: &str) -> Option<App> {
    let s = std::fs::read_to_string(p).ok()?;
    let mut in_main = false;
    let (mut name, mut exec, mut mimes, mut terminal, mut hidden, mut is_app) = (None, None, vec![], false, false, false);
    let mut icon = String::new();
    for line in s.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_main = line == "[Desktop Entry]";
            continue;
        }
        if !in_main {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        match k.trim() {
            "Name" => name = Some(v.trim().to_string()),
            "Exec" => exec = Some(v.trim().to_string()),
            "MimeType" => mimes = v.split(';').filter(|s| !s.is_empty()).map(String::from).collect(),
            "Terminal" => terminal = v.trim() == "true",
            "NoDisplay" => hidden |= v.trim() == "true",
            "Icon" if icon.is_empty() => icon = v.trim().to_string(),
            "Hidden" => {
                if v.trim() == "true" {
                    return None;
                }
            }
            "Type" => is_app = v.trim() == "Application",
            _ => {}
        }
    }
    if !is_app {
        return None;
    }
    Some(App { id: id.to_string(), name: name?, exec: exec?, mimes, terminal, hidden, icon })
}

impl Apps {
    pub fn by_id(&self, id: &str) -> Option<&App> {
        self.apps.iter().find(|a| a.id == id)
    }

    /// Apps for a mime type in xdg-mime order: explicit defaults, added associations,
    /// mimeinfo.cache order (user first), then any other app declaring the type.
    pub fn for_mime(&self, mime: &str) -> Vec<&App> {
        let me: &'static Apps = apps();
        let removed: Vec<&String> = me.removed.get(mime).map(|v| v.iter().collect()).unwrap_or_default();
        let mut out: Vec<&App> = vec![];
        let push = |a: &'static App, out: &mut Vec<&App>| {
            if !out.iter().any(|o| o.id == a.id) && !removed.contains(&&a.id) {
                out.push(a);
            }
        };
        let parent = mime.split('/').next().unwrap_or("");
        let keys = [mime.to_string(), format!("{parent}/*")];
        for key in &keys {
            for id in me.defaults.get(key).into_iter().flatten() {
                if let Some(a) = me.by_id(id) {
                    push(a, &mut out);
                }
            }
        }
        for id in me.added.get(mime).into_iter().flatten().chain(me.cache.get(mime).into_iter().flatten()) {
            if let Some(a) = me.by_id(id) {
                push(a, &mut out);
            }
        }
        for a in &me.apps {
            if a.mimes.iter().any(|m| m == mime) {
                push(a, &mut out);
            }
        }
        // Text editors can open any text/* type.
        if parent == "text" || mime == "application/x-shellscript" || mime == "application/json" {
            for id in me.defaults.get("text/plain").into_iter().flatten().chain(me.cache.get("text/plain").into_iter().flatten()) {
                if let Some(a) = me.by_id(id) {
                    push(a, &mut out);
                }
            }
        }
        out
    }

    pub fn default_for(&self, mime: &str) -> Option<&App> {
        self.for_mime(mime).into_iter().next()
    }
}

/// Split an Exec= line into argv, expanding field codes.
pub fn expand_exec(exec: &str, paths: &[PathBuf], app_name: &str) -> Vec<String> {
    let mut args: Vec<String> = vec![];
    let mut cur = String::new();
    let mut in_q = false;
    let mut chars = exec.chars().peekable();
    let mut had_files = false;
    let push_cur = |cur: &mut String, args: &mut Vec<String>| {
        if !cur.is_empty() {
            args.push(std::mem::take(cur));
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '"' => in_q = !in_q,
            '\\' if in_q => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            ' ' if !in_q => push_cur(&mut cur, &mut args),
            '%' => match chars.next() {
                Some('f') | Some('u') => {
                    had_files = true;
                    if let Some(p) = paths.first() {
                        cur.push_str(&p.to_string_lossy());
                    }
                }
                Some('F') | Some('U') => {
                    had_files = true;
                    push_cur(&mut cur, &mut args);
                    args.extend(paths.iter().map(|p| p.to_string_lossy().into_owned()));
                }
                Some('c') => cur.push_str(app_name),
                Some('%') => cur.push('%'),
                _ => {}
            },
            _ => cur.push(c),
        }
    }
    push_cur(&mut cur, &mut args);
    if !had_files {
        args.extend(paths.iter().map(|p| p.to_string_lossy().into_owned()));
    }
    args
}

/// Programs the app asked to start during tests (nothing is ever launched from a test).
#[cfg(test)]
pub static LAUNCHED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

pub fn spawn_detached(#[allow(unused_mut)] mut cmd: Command) -> Result<(), String> {
    #[cfg(test)]
    {
        LAUNCHED.lock().unwrap_or_else(|e| e.into_inner()).push(format!("{cmd:?}"));
        return Ok(());
    }
    #[allow(unreachable_code)]
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

pub fn launch(app: &App, paths: &[PathBuf], cwd: Option<&Path>) -> Result<(), String> {
    let mut argv = expand_exec(&app.exec, paths, &app.name);
    if argv.is_empty() {
        return Err("empty Exec".into());
    }
    if app.terminal {
        let mut t = terminal_argv();
        t.push("-e".into());
        t.extend(argv);
        argv = t;
    }
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    spawn_detached(cmd)
}

/// A description shortened for a row. Office formats go by what they hold, whoever's
/// format it is; the rest drop the words most descriptions end with ("Markdown document",
/// "Rust source code") and any aside in brackets.
pub(crate) fn short_kind(mime: &str, long: &str) -> String {
    const BY_MIME: &[(&str, &str)] = &[
        ("text/plain", "Text"),
        ("application/pdf", "PDF"),
        ("text/html", "Web page"),
        ("text/csv", "Spreadsheet"),
        ("text/tab-separated-values", "Spreadsheet"),
        ("application/vnd.oasis.opendocument.spreadsheet", "Spreadsheet"),
        ("application/vnd.ms-excel", "Spreadsheet"),
        ("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", "Spreadsheet"),
        ("application/vnd.oasis.opendocument.text", "Document"),
        ("application/msword", "Document"),
        ("application/vnd.openxmlformats-officedocument.wordprocessingml.document", "Document"),
        ("application/rtf", "Document"),
        ("application/vnd.oasis.opendocument.presentation", "Presentation"),
        ("application/vnd.ms-powerpoint", "Presentation"),
        ("application/vnd.openxmlformats-officedocument.presentationml.presentation", "Presentation"),
    ];
    if let Some((_, s)) = BY_MIME.iter().find(|(m, _)| *m == mime) {
        return s.to_string();
    }
    let mut s = long;
    if let Some(i) = s.find(" (") {
        s = &s[..i];
    }
    let s = match (s.strip_suffix(" document"), s.strip_suffix(" source code")) {
        (Some(x), _) if !x.is_empty() => x.to_string(),
        (_, Some(x)) if !x.is_empty() => format!("{x} source"),
        _ => s.to_string(),
    };
    if s.is_empty() { long.to_string() } else { s }
}

/// What opening `path` would do: (type label, mime, app name, argv). For diagnostics.
pub fn describe_open(path: &Path) -> (String, String, String, Vec<String>) {
    let is_dir = path.is_dir();
    let e = crate::fsx::Entry::from_path(path);
    let (ext, exec) = e.as_ref().map(|e| (e.ext().to_string(), e.is_executable())).unwrap_or_default();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let db = mime_db();
    let mime = db.for_name(&name, is_dir, exec);
    let label = db.label(&name, &ext, is_dir, exec);
    match apps().default_for(&mime) {
        Some(a) => (label, mime, a.name.clone(), expand_exec(&a.exec, &[path.to_path_buf()], &a.name)),
        None => (label, mime, "xdg-open".into(), vec!["xdg-open".into(), path.to_string_lossy().into_owned()]),
    }
}

/// Open with the default application (via our own lookup, then xdg-open).
pub fn open_default(path: &Path) -> Result<(), String> {
    let mime = mime_db().for_path(path, path.is_dir());
    if path.extension().map(|e| e == "desktop").unwrap_or(false) {
        if let Some(app) = parse_desktop(path, "local") {
            return launch(&app, &[], path.parent());
        }
    }
    if let Some(app) = apps().default_for(&mime) {
        return launch(app, &[path.to_path_buf()], path.parent());
    }
    let mut c = Command::new("xdg-open");
    c.arg(path);
    spawn_detached(c)
}

/// A web or mail address, with the desktop's handler for it.
pub fn open_uri(uri: &str) -> Result<(), String> {
    let mut c = Command::new("xdg-open");
    c.arg(uri);
    spawn_detached(c)
}

fn terminal_argv() -> Vec<String> {
    if let Ok(t) = std::env::var("TERMINAL") {
        if !t.is_empty() {
            return vec![t];
        }
    }
    for t in ["x-terminal-emulator", "kitty", "alacritty", "foot", "wezterm", "ghostty", "konsole", "gnome-terminal", "kgx", "xfce4-terminal", "xterm"] {
        if which(t) {
            return vec![t.to_string()];
        }
    }
    vec!["xterm".into()]
}

pub fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

pub fn open_terminal(dir: &Path) -> Result<(), String> {
    let argv = terminal_argv();
    let mut c = Command::new(&argv[0]);
    c.args(&argv[1..]).current_dir(dir);
    // Some terminals ignore cwd; pass the usual flags where known.
    match argv[0].as_str() {
        "kitty" => {
            c.arg("--directory").arg(dir);
        }
        "wezterm" => {
            c.arg("start").arg("--cwd").arg(dir);
        }
        "gnome-terminal" | "kgx" | "xfce4-terminal" => {
            c.arg(format!("--working-directory={}", dir.display()));
        }
        "konsole" => {
            c.arg("--workdir").arg(dir);
        }
        "alacritty" => {
            c.arg("--working-directory").arg(dir);
        }
        "foot" => {
            c.arg(format!("--working-directory={}", dir.display()));
        }
        _ => {}
    }
    spawn_detached(c)
}

/// `argv` started in the user's terminal, in `dir`. Each terminal takes the folder and
/// the program its own way; the rest take `-e` and the folder they are started in.
pub fn run_in_terminal(dir: &Path, argv: &[String]) -> Result<(), String> {
    let t = terminal_argv();
    let mut c = Command::new(&t[0]);
    c.args(&t[1..]).current_dir(dir);
    let name = Path::new(&t[0]).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let at = |flag: &str| format!("{flag}={}", dir.display());
    match name.as_str() {
        "kitty" => c.arg("--directory").arg(dir).args(argv),
        "foot" => c.arg(at("--working-directory")).args(argv),
        "wezterm" => c.args(["start", "--cwd"]).arg(dir).arg("--").args(argv),
        "gnome-terminal" | "kgx" => c.arg(at("--working-directory")).arg("--").args(argv),
        "xfce4-terminal" => c.arg(at("--working-directory")).arg("-x").args(argv),
        "konsole" => c.arg("--workdir").arg(dir).arg("-e").args(argv),
        "alacritty" => c.arg("--working-directory").arg(dir).arg("-e").args(argv),
        "ghostty" => c.arg(at("--working-directory")).arg("-e").args(argv),
        _ => c.arg("-e").args(argv),
    };
    spawn_detached(c)
}

/// `argv` wrapped so the terminal stays open when it ends: its exit status, then the
/// user's shell in the same folder, where the output can still be read.
pub fn held(argv: &[String]) -> Vec<String> {
    let script = r#""$@"; s=$?; printf '\n[exit %d]\n' "$s"; exec "${SHELL:-/bin/sh}""#;
    let mut v = vec!["sh".to_string(), "-c".to_string(), script.to_string(), "sh".to_string()];
    v.extend(argv.iter().cloned());
    v
}

/// Run an executable with arguments, detached (used for drops onto scripts).
pub fn run_with(exe: &Path, args: &[PathBuf]) -> Result<(), String> {
    let mut c = Command::new(exe);
    c.args(args);
    if let Some(d) = exe.parent() {
        c.current_dir(d);
    }
    spawn_detached(c)
}

pub fn set_default(app_id: &str, mime: &str) -> Result<(), String> {
    #[cfg(test)]
    {
        LAUNCHED.lock().unwrap_or_else(|e| e.into_inner()).push(format!("xdg-mime default {app_id} {mime}"));
        // Inside the simulation sandbox, do what xdg-mime does, to the sandbox's own file.
        if std::env::var("MAIA_SIM_SANDBOX").as_deref() == Ok("1") {
            if let Some(dir) = dirs::config_dir() {
                let p = dir.join("mimeapps.list");
                let old = std::fs::read_to_string(&p).unwrap_or_default();
                let mut out: Vec<String> = old.lines().filter(|l| !l.starts_with(&format!("{mime}="))).map(String::from).collect();
                match out.iter().position(|l| l.trim() == "[Default Applications]") {
                    Some(i) => out.insert(i + 1, format!("{mime}={app_id};")),
                    None => out.extend(["[Default Applications]".to_string(), format!("{mime}={app_id};")]),
                }
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::write(p, out.join("\n") + "\n");
            }
        }
        return Ok(());
    }
    #[allow(unreachable_code)]
    Command::new("xdg-mime")
        .args(["default", app_id, mime])
        .status()
        .map_err(|e| e.to_string())
        .and_then(|s| if s.success() { Ok(()) } else { Err("xdg-mime failed".into()) })
}

/// Spawn another Maia window showing `path`.
pub fn new_window(path: &Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut c = Command::new(exe);
    c.arg("--window").arg(path);
    spawn_detached(c)
}
