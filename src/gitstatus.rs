//! Git status marks for folders inside a work tree. `git status` runs in the background
//! when a tab arrives in a folder and when the watcher sees a change in the repository
//! (its `.git` folder included); answers are cached per repository. Outside a work tree
//! nothing runs: a folder only counts once a `.git` is found above it.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use egui::{Color32, Painter, Rect, Stroke, vec2};

pub const AMBER: Color32 = Color32::from_rgb(0xe8, 0xa3, 0x3d);
pub const GREEN: Color32 = Color32::from_rgb(0x4c, 0xb8, 0x60);
pub const BLUE: Color32 = Color32::from_rgb(0x4f, 0x8f, 0xe8);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Modified,
    Staged,
    Untracked,
    Deleted,
    Renamed,
    Conflict,
    /// A folder with changes somewhere below it.
    Inside,
}

pub struct Repo {
    pub root: PathBuf,
    pub branch: String,
    /// Commits to push and to pull, against the branch's upstream (0 with none).
    pub ahead: u32,
    pub behind: u32,
    pub marks: HashMap<PathBuf, Mark>,
    /// What git ignores, as it reports it: a folder that is ignored as a whole is one
    /// entry, so a `node_modules` costs one path, not its files. Kept as bytes: the
    /// listing looks its paths' prefixes up, and hashing bytes is a pass over them
    /// where a `PathBuf` hashes component by component.
    pub ignored: std::sync::Arc<HashSet<std::ffi::OsString>>,
}

/// Whether `git status` is also asked for ignored files (the Ignored files option is
/// not Show). Read by the workers; a change asks every folder again.
static IGNORED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

type Answers = std::sync::Arc<std::sync::Mutex<Vec<(PathBuf, Option<Repo>)>>>;

#[derive(Default)]
struct Git {
    /// Folder -> the work tree it is in, as last read (None: not in one).
    dirs: HashMap<PathBuf, Option<Rc<Repo>>>,
    /// Folders being asked about, and whether to ask once more when the answer is in.
    busy: Vec<(PathBuf, bool)>,
    /// The folder each tab showed when last looked at: a change is a navigation.
    seen: Vec<(u64, PathBuf)>,
    answers: Answers,
    /// Work trees to read again next frame (after Maia changed one itself).
    again: Vec<PathBuf>,
}

thread_local! {
    static GIT: RefCell<Git> = RefCell::default();
}

/// Once a frame: take in answers, ask again where tabs moved or the watcher saw changes,
/// and add the repositories' `.git` folders to the watched set.
pub fn pump(panels: &HashMap<u64, crate::panel::Panel>, changed: &[PathBuf], watch: &mut HashSet<PathBuf>, ignored: bool, ctx: &egui::Context) {
    GIT.with(|g| {
        let g = &mut *g.borrow_mut();
        // Ignored files asked for or no longer: every folder is read again.
        if IGNORED.swap(ignored, std::sync::atomic::Ordering::Relaxed) != ignored {
            g.seen.clear();
        }
        for r in std::mem::take(&mut g.again) {
            ask(g, r, ctx);
        }
        let answers = g.answers.lock().map(|mut a| std::mem::take(&mut *a)).unwrap_or_default();
        for (dir, repo) in answers {
            let repo = repo.map(Rc::new);
            // A fresh read of a work tree serves every folder known to be in it.
            if let Some(r) = &repo {
                for v in g.dirs.values_mut() {
                    if v.as_ref().is_some_and(|o| o.root == r.root) {
                        *v = Some(r.clone());
                    }
                }
            }
            if g.dirs.len() > 4096 {
                g.dirs.clear();
            }
            g.dirs.insert(dir.clone(), repo);
            if let Some(i) = g.busy.iter().position(|b| b.0 == dir) {
                if g.busy.swap_remove(i).1 {
                    ask(g, dir, ctx);
                }
            }
        }
        for d in changed {
            // The innermost work tree: a repository can sit inside another's ignored folder.
            let hit = g.dirs.values().flatten().filter(|r| d.starts_with(&r.root)).max_by_key(|r| r.root.as_os_str().len()).map(|r| r.root.clone());
            if let Some(r) = hit {
                ask(g, r, ctx);
            }
        }
        for p in panels.values() {
            let t = p.tab();
            if crate::tab::is_place(&t.path) || crate::tab::drive_dev(&t.path).is_some() {
                continue;
            }
            let moved = match g.seen.iter_mut().find(|s| s.0 == t.id) {
                Some(s) if s.1 == t.path => false,
                Some(s) => {
                    s.1 = t.path.clone();
                    true
                }
                None => {
                    // Closed tabs leave entries behind; a handful of tabs is the usual case.
                    if g.seen.len() > 64 {
                        g.seen.clear();
                    }
                    g.seen.push((t.id, t.path.clone()));
                    true
                }
            };
            if moved {
                ask(g, t.path.clone(), ctx);
            }
            if let Some(Some(r)) = g.dirs.get(&t.path) {
                watch.insert(r.root.join(".git"));
            }
        }
    });
}

fn ask(g: &mut Git, dir: PathBuf, ctx: &egui::Context) {
    if let Some(b) = g.busy.iter_mut().find(|b| b.0 == dir) {
        b.1 = true;
        return;
    }
    g.busy.push((dir.clone(), false));
    let (answers, ctx) = (g.answers.clone(), ctx.clone());
    std::thread::spawn(move || {
        let r = query(&dir);
        if let Ok(mut a) = answers.lock() {
            a.push((dir, r));
        }
        ctx.request_repaint();
    });
}

/// The repository `dir` is in, as last read.
pub fn repo_for(dir: &Path) -> Option<Rc<Repo>> {
    GIT.with(|g| g.borrow().dirs.get(dir)?.clone())
}

/// Read the work tree at `root` again, as soon as the next frame: Maia changed it.
pub fn rescan(root: &Path) {
    GIT.with(|g| g.borrow_mut().again.push(root.to_path_buf()));
}

/// Whether git ignores `path`, a folder's entry or one further down, as seen from the
/// folder `shown`. Inside a folder that is itself ignored nothing is: everything there
/// would be, and a listing faded or emptied as a whole says nothing.
pub fn ignored(repo: Option<&Repo>, path: &Path, shown: &Path) -> bool {
    let Some(set) = repo.map(|r| &r.ignored).filter(|s| !s.is_empty()) else { return false };
    is_ignored(set, path, shown)
}

/// `ignored` against the set alone (the listing's rebuild holds only that). From the
/// folder down, so an entry deep in an ignored folder is found at its first step.
pub fn is_ignored(set: &HashSet<std::ffi::OsString>, path: &Path, shown: &Path) -> bool {
    let (p, top) = (path.as_os_str().as_bytes(), shown.as_os_str().as_bytes());
    if p.len() <= top.len() || !p.starts_with(top) || p[top.len()] != b'/' && !top.ends_with(b"/") {
        return false;
    }
    let mut at = top.len() + 1;
    loop {
        let end = p[at..].iter().position(|&b| b == b'/').map_or(p.len(), |k| at + k);
        if set.contains(OsStr::from_bytes(&p[..end])) {
            return true;
        }
        if end >= p.len() {
            return false;
        }
        at = end + 1;
    }
}

/// Whether the folder `shown` is ignored, or lies inside an ignored one.
pub fn inside_ignored(repo: &Repo, shown: &Path) -> bool {
    !repo.ignored.is_empty() && shown.ancestors().take_while(|a| a.starts_with(&repo.root) && *a != repo.root).any(|a| repo.ignored.contains(a.as_os_str()))
}

/// Options that stop a repository's own config from running programs when Maia reads
/// it: an fsmonitor hook, and filters (status re-reads changed files through their
/// clean commands). Repositories come from anywhere, archives and downloads included,
/// so their config is not trusted; the user's own (global, system) is left alone.
pub fn guard(at: &Path) -> Vec<String> {
    let mut v = vec!["-c".to_string(), "core.fsmonitor=false".to_string()];
    for key in local_filters(at) {
        v.extend(["-c".to_string(), format!("{key}=")]);
    }
    v
}

/// The filter commands the repository's own config defines (which `guard` empties).
pub fn local_filters(at: &Path) -> Vec<String> {
    let mut c = crate::media::tool("git");
    c.arg("-C").arg(at).args(["config", "--show-scope", "-z", "--get-regexp", r"^filter\..*\.(clean|smudge|process)$"]);
    // Entries are "scope\0key\nvalue\0" (a key may hold spaces: filter."my x".clean).
    let out = crate::media::output(c, std::time::Duration::from_secs(5)).unwrap_or_default();
    let mut f = out.split(|&b| b == 0);
    let mut v = vec![];
    while let (Some(scope), Some(entry)) = (f.next(), f.next()) {
        let key = entry.split(|&b| b == b'\n').next().unwrap_or_default();
        if matches!(scope, b"local" | b"worktree") {
            v.push(String::from_utf8_lossy(key).into_owned());
        }
    }
    v
}

/// Read the status of the work tree around `dir`: None outside one, or without git.
pub fn query(dir: &Path) -> Option<Repo> {
    dir.ancestors().find(|a| a.join(".git").exists())?;
    if !crate::media::have("git") {
        return None;
    }
    // Never take the index lock: a status run must not itself look like a change.
    let git = |at: &Path, args: &[&str]| {
        let mut c = crate::media::tool("git");
        c.arg("--no-optional-locks").args(guard(at)).arg("-C").arg(at).args(args);
        crate::media::output(c, std::time::Duration::from_secs(20))
    };
    let top = git(dir, &["rev-parse", "--show-toplevel", "--show-prefix"])?;
    // The root as this path reaches it (through symlinks too): the prefix is where `dir`
    // sits inside the work tree.
    let prefix = top.split(|&b| b == b'\n').nth(1)?;
    let root = dir.ancestors().nth(Path::new(OsStr::from_bytes(prefix)).components().count())?.to_path_buf();
    let ignored = IGNORED.load(std::sync::atomic::Ordering::Relaxed);
    let out = git(&root, &["status", "--porcelain=v1", "-z", "--branch", if ignored { "--ignored" } else { "--ignored=no" }])?;
    Some(parse(root, &out))
}

/// `git status --porcelain=v1 -z --branch` output as marks on absolute paths.
pub fn parse(root: PathBuf, out: &[u8]) -> Repo {
    let mut marks = HashMap::new();
    let mut ignored = HashSet::new();
    let mut branch = String::new();
    let (mut ahead, mut behind) = (0, 0);
    let mut it = out.split(|&b| b == 0);
    while let Some(rec) = it.next() {
        if let Some(b) = rec.strip_prefix(b"## ") {
            // "main...origin/main [ahead 3, behind 1]"
            let s = String::from_utf8_lossy(b);
            let s = s.strip_prefix("No commits yet on ").unwrap_or(&s);
            branch = s.split("...").next().and_then(|s| s.split(' ').next()).unwrap_or("").to_string();
            if let Some(counts) = s.rsplit_once(" [").and_then(|(_, c)| c.strip_suffix(']')) {
                for part in counts.split(", ") {
                    match part.split_once(' ') {
                        Some(("ahead", n)) => ahead = n.parse().unwrap_or(0),
                        Some(("behind", n)) => behind = n.parse().unwrap_or(0),
                        _ => {}
                    }
                }
            }
            continue;
        }
        if rec.len() < 4 {
            continue;
        }
        let (x, y) = (rec[0], rec[1]);
        // A rename or copy is followed by the path it came from.
        if x == b'R' || x == b'C' {
            it.next();
        }
        let rel = &rec[3..];
        let p = root.join(OsStr::from_bytes(rel.strip_suffix(b"/").unwrap_or(rel)));
        let m = match (x, y) {
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => Mark::Conflict,
            (b'?', _) => Mark::Untracked,
            (b'!', _) => {
                ignored.insert(p.into_os_string());
                continue;
            }
            (_, b'M' | b'T') => Mark::Modified,
            (b'R' | b'C', _) => Mark::Renamed,
            (b'D', _) | (_, b'D') => Mark::Deleted,
            _ => Mark::Staged,
        };
        for a in p.ancestors().skip(1) {
            if a == root || !a.starts_with(&root) {
                break;
            }
            marks.entry(a.to_path_buf()).or_insert(Mark::Inside);
        }
        marks.insert(p, m);
    }
    Repo { root, branch, ahead, behind, marks, ignored: std::sync::Arc::new(ignored) }
}

/// A dot on the icon's lower right corner; a ring for folders with changes inside.
pub fn paint(p: &Painter, icon: Rect, repo: Option<&Repo>, path: &Path, pal: &crate::theme::Palette) {
    let Some(&m) = repo.and_then(|r| r.marks.get(path)) else { return };
    let col = match m {
        Mark::Modified | Mark::Inside => AMBER,
        Mark::Staged => GREEN,
        Mark::Untracked => pal.text_dim,
        Mark::Deleted => pal.danger,
        Mark::Renamed => BLUE,
        Mark::Conflict => Color32::from_rgb(0xc0, 0x61, 0xcb),
    };
    let r = (icon.width() * 0.14).clamp(2.6, 6.0);
    let c = icon.right_bottom() - vec2(r * 0.4, r * 0.4);
    if m == Mark::Inside {
        p.circle_stroke(c, r - 0.7, Stroke::new(1.4, col));
    } else {
        p.circle_filled(c, r, col);
    }
}
