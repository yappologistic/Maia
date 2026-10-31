//! Commit history of a work tree as a vertical graph in the inspector: newest at the top,
//! one row per commit, each line of work in a lane of its own. A single `git log` runs
//! per history and is held back by its own pipe between pages, so a repository of any
//! size costs only the pages scrolled through. Nothing here writes to the repository:
//! a commit's menu (a right click, or Shift+F10) acts on it through `gitwork`.
//!
//! A commit opens to its files with their line counts, grouped by folder when there are
//! many. A file's diff (or, for a picture, its old and new versions) opens under its row,
//! or on its own in the whole panel. Everything git is asked runs on a worker, through
//! `gitstatus::guard`, and is read only once it is on screen.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use egui::epaint::CubicBezierShape;
use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Align, Align2, Color32, CornerRadius, FontId, Key, Layout, Pos2, Rect, ScrollArea, Sense, Stroke, StrokeKind, Ui, UiBuilder, Vec2, pos2, vec2};

use crate::app::Action;
use crate::listing::Env;
use crate::theme::{Glyph, Palette};

/// Commits read per page; the next page is read when the view nears the end.
pub const PAGE: usize = 300;
/// Lanes drawn side by side. Lanes past these share one faint column, so a history with
/// dozens of open branches stays readable.
pub const LANES: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    /// The branch HEAD is on, or HEAD itself when detached.
    Head,
    Branch,
    Remote,
    Tag,
}

pub struct Commit {
    pub hash: String,
    pub parents: Vec<String>,
    pub author: String,
    pub email: String,
    pub time: i64,
    pub refs: Vec<(RefKind, String)>,
    pub subject: String,
}

/// A commit's place in the graph: its lane, whether a child above continues into it, the
/// other lanes that end in it (children on other lanes), the lanes its parents continue
/// in (first parent first) and the lanes passing by.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    pub col: u16,
    pub up: bool,
    pub ins: Vec<u16>,
    pub outs: Vec<u16>,
    pub pass: Vec<u16>,
}

/// Which commit each lane is heading for, carried from one page to the next.
#[derive(Default)]
pub struct Lanes(Vec<Option<String>>);

impl Lanes {
    pub fn place(&mut self, c: &Commit) -> Row {
        let l = &mut self.0;
        let hits: Vec<u16> = (0..l.len()).filter(|&i| l[i].as_deref() == Some(&c.hash)).map(|i| i as u16).collect();
        let free = |l: &mut Vec<Option<String>>| {
            let i = l.iter().position(Option::is_none).unwrap_or(l.len());
            if i == l.len() {
                l.push(None);
            }
            i
        };
        let col = match hits.first() {
            Some(&h) => h as usize,
            None => free(l),
        };
        for &h in &hits {
            l[h as usize] = None;
        }
        let pass = (0..l.len()).filter(|&i| l[i].is_some()).map(|i| i as u16).collect();
        let mut outs = Vec::with_capacity(c.parents.len());
        for (k, p) in c.parents.iter().enumerate() {
            // The first parent always continues straight down, even when another lane
            // already heads for it: the two meet at the parent, where the branch began,
            // instead of the trunk stepping sideways here.
            let j = match l.iter().position(|x| x.as_deref() == Some(p.as_str())) {
                Some(j) if k > 0 => j,
                _ => {
                    let j = if l[col].is_none() { col } else { free(l) };
                    l[j] = Some(p.clone());
                    j
                }
            };
            outs.push(j as u16);
        }
        while l.last().is_some_and(Option::is_none) {
            l.pop();
        }
        Row { col: col as u16, up: !hits.is_empty(), ins: hits.get(1..).unwrap_or_default().to_vec(), outs, pass }
    }
}

/// One `git log -z --format=…` record (see `FORMAT`).
pub fn parse(rec: &[u8]) -> Option<Commit> {
    let s = String::from_utf8_lossy(rec);
    let mut f = s.trim_start_matches('\n').split('\x1f');
    let hash = f.next()?.to_string();
    let parents = f.next()?.split_whitespace().map(String::from).collect();
    let author = f.next()?.to_string();
    let email = f.next()?.to_string();
    let time = f.next()?.parse().unwrap_or(0);
    let mut refs = vec![];
    for d in f.next()?.split(", ") {
        let r = match d.strip_prefix("HEAD -> ") {
            Some(b) => (RefKind::Head, b.trim_start_matches("refs/heads/")),
            None if d == "HEAD" => (RefKind::Head, d),
            None => match d.strip_prefix("tag: refs/tags/") {
                Some(t) => (RefKind::Tag, t),
                None => match (d.strip_prefix("refs/heads/"), d.strip_prefix("refs/remotes/")) {
                    (Some(b), _) => (RefKind::Branch, b),
                    (_, Some(r)) if !r.ends_with("/HEAD") => (RefKind::Remote, r),
                    // Stash, notes and a remote's HEAD pointer.
                    _ => continue,
                },
            },
        };
        refs.push((r.0, r.1.to_string()));
    }
    let subject = f.next()?.to_string();
    (hash.len() >= 7).then_some(Commit { hash, parents, author, email, time, refs, subject })
}

pub const FORMAT: &str = "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%D%x1f%s";
/// The same with the rest of the message after it, for a search to look in.
const FORMAT_BODY: &str = "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%D%x1f%s%x1f%b";

/// Up to `n` records from a running `git log -z`.
pub fn read_page(r: &mut impl BufRead, n: usize) -> Vec<Commit> {
    let (mut v, mut buf) = (vec![], vec![]);
    while v.len() < n {
        buf.clear();
        if r.read_until(0, &mut buf).unwrap_or(0) == 0 {
            break;
        }
        v.extend(parse(buf.strip_suffix(&[0]).unwrap_or(&buf)));
    }
    v
}

/// The message after the subject in a `FORMAT_BODY` record.
fn body_of(rec: &[u8]) -> std::borrow::Cow<'_, str> {
    let mut f = rec.splitn(8, |&b| b == 0x1f);
    String::from_utf8_lossy(f.nth(7).unwrap_or_default())
}

/// Up to `n` records of a running `git log -z` that match `q`, handed over as they are
/// found (every 60 ms at most) so a rare word's first commits show while git is still
/// walking. True when the log has ended.
pub fn read_matching(r: &mut impl BufRead, n: usize, q: &crate::gitfind::Query, mut hand: impl FnMut(Vec<Commit>)) -> bool {
    let (mut buf, mut got, mut found) = (vec![], vec![], 0);
    let mut last = std::time::Instant::now();
    loop {
        buf.clear();
        if r.read_until(0, &mut buf).unwrap_or(0) == 0 {
            hand(got);
            return true;
        }
        let rec = buf.strip_suffix(&[0]).unwrap_or(&buf);
        if let Some(c) = parse(rec).filter(|c| crate::gitfind::matches(q, c, &body_of(rec))) {
            got.push(c);
            found += 1;
        }
        if found >= n {
            hand(got);
            return false;
        }
        if !got.is_empty() && last.elapsed() > std::time::Duration::from_millis(60) {
            hand(std::mem::take(&mut got));
            last = std::time::Instant::now();
        }
    }
}

pub(crate) fn git(root: &Path) -> std::process::Command {
    let mut c = crate::media::tool("git");
    c.arg("--no-optional-locks").arg("--literal-pathspecs").args(crate::gitstatus::guard(root)).arg("-C").arg(root).args(["-c", "log.showSignature=false", "-c", "core.quotePath=false"]);
    c
}

/// `git log` over every branch, remote and tag. Topological order keeps each line of work
/// together, so lanes stay short and few; with a commit-graph file (written by `git gc`)
/// git streams it as it walks, and without one the first page waits for the walk, on
/// this thread, never the UI's.
pub fn log_args() -> [&'static str; 11] {
    ["log", "--topo-order", "-z", "--no-color", "--decorate=full", FORMAT, "--branches", "--remotes", "--tags", "HEAD", "--"]
}

/// The commits on HEAD that changed one file, following it back through renames.
pub fn file_log_args(path: &str) -> Vec<String> {
    ["log", "-z", "--no-color", "--decorate=full", FORMAT, "--follow", "HEAD", "--", path].map(String::from).to_vec()
}

/// The `git log` a list reads: the whole history or one file's, and a search's flags
/// before the `--`, its paths after it (a file's history follows its one file, so it keeps
/// that alone). Words to look for need the whole message.
pub fn list_args(path: Option<&str>, q: Option<&crate::gitfind::Query>) -> Vec<String> {
    let mut a = path.map_or_else(|| log_args().map(String::from).to_vec(), file_log_args);
    if let Some(q) = q {
        if !q.words.is_empty() {
            for x in a.iter_mut().filter(|x| *x == FORMAT) {
                *x = FORMAT_BODY.into();
            }
        }
        let dd = a.iter().position(|x| x == "--").unwrap_or(a.len());
        a.splice(dd..dd, q.flags());
        if path.is_none() {
            a.extend(q.paths.iter().cloned());
        }
    }
    a
}

/// Output of a short git command, at most `limit` bytes (the rest is cut, and the
/// process killed). The flag says whether it was cut.
pub(crate) fn run(root: &Path, args: &[&str], limit: u64) -> (Vec<u8>, bool) {
    let mut v = vec![];
    let Ok(mut child) = git(root).args(args).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else { return (v, false) };
    if let Some(out) = child.stdout.take() {
        let _ = out.take(limit + 1).read_to_end(&mut v);
    }
    let cut = v.len() as u64 > limit;
    v.truncate(limit as usize);
    let _ = child.kill();
    let _ = child.wait();
    (v, cut)
}

/// Where the work tree's origin remote points, as configured.
pub fn origin_url(root: &Path) -> Option<String> {
    let s = String::from_utf8_lossy(&run(root, &["config", "--get", "remote.origin.url"], 4096).0).trim().to_string();
    (!s.is_empty()).then_some(s)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    /// `A`dded, `M`odified, `D`eleted, `R`enamed, `C`opied or `T`ype changed.
    pub status: u8,
    pub path: String,
    pub from: Option<String>,
    /// Lines added and removed; None for a binary file.
    pub lines: Option<(u32, u32)>,
}

/// `git diff-tree -z --name-status` output.
pub fn parse_changes(out: &[u8]) -> Vec<Change> {
    let mut v = vec![];
    let mut it = out.split(|&b| b == 0).map(|s| String::from_utf8_lossy(s).into_owned());
    while let Some(st) = it.next() {
        let Some(&k) = st.as_bytes().first() else { continue };
        let Some(a) = it.next() else { break };
        let (path, from) = if matches!(k, b'R' | b'C') { (it.next().unwrap_or_default(), Some(a)) } else { (a, None) };
        v.push(Change { status: k, path, from, lines: Some((0, 0)) });
    }
    v
}

/// `git diff-tree -z --numstat` output: lines added and removed per file (None when git
/// calls it binary), under its new path.
pub fn parse_numstat(out: &[u8]) -> Vec<(Option<(u32, u32)>, String)> {
    let mut v = vec![];
    let mut it = out.split(|&b| b == 0);
    while let Some(rec) = it.next() {
        let s = String::from_utf8_lossy(rec);
        let mut f = s.splitn(3, '\t');
        let (Some(a), Some(d), Some(p)) = (f.next(), f.next(), f.next()) else { continue };
        let n = a.parse().ok().zip(d.parse().ok());
        // A rename or copy leaves the path empty and gives the old and the new one after.
        let path = if p.is_empty() {
            let _ = it.next();
            it.next().map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default()
        } else {
            p.to_string()
        };
        v.push((n, path));
    }
    v
}

/// Line counts into the changes they belong to. Both lists come from the same diff, so
/// they are in the same order; a path that doesn't match (one list cut short) is looked
/// up instead.
pub fn join_numstat(files: &mut [Change], counts: &[(Option<(u32, u32)>, String)]) {
    let mut by_path: Option<HashMap<&str, Option<(u32, u32)>>> = None;
    for (i, f) in files.iter_mut().enumerate() {
        f.lines = match counts.get(i) {
            Some((n, p)) if *p == f.path => *n,
            _ => by_path.get_or_insert_with(|| counts.iter().map(|(n, p)| (p.as_str(), *n)).collect()).get(f.path.as_str()).copied().unwrap_or(Some((0, 0))),
        };
    }
}

/// The commit against its first parent (a merge shows what it brought in), or against
/// nothing for a root commit.
fn base<'a>(hash: &'a str, parents: &'a [String]) -> [&'a str; 2] {
    match parents.first() {
        Some(p) => [p, hash],
        None => ["--root", hash],
    }
}

/// The changed files of a commit, with their line counts, and its message body.
pub fn changes(root: &Path, hash: &str, parents: &[String]) -> (String, Vec<Change>) {
    let [old, new] = base(hash, parents);
    let body = run(root, &["show", "-s", "--format=%b", hash], 64 << 10).0;
    (String::from_utf8_lossy(&body).trim().to_string(), files(root, old, new))
}

/// The files that differ from `old` to `new` (`--root` for a first commit), with their
/// line counts.
pub fn files(root: &Path, old: &str, new: &str) -> Vec<Change> {
    let a = ["diff-tree", "--no-commit-id", "-r", "-z", "-M", "--name-status", old, new];
    let n = ["diff-tree", "--no-commit-id", "-r", "-z", "-M", "--numstat", old, new];
    let mut files = parse_changes(&run(root, &a, 4 << 20).0);
    join_numstat(&mut files, &parse_numstat(&run(root, &n, 4 << 20).0));
    files
}

pub type Spans = Vec<Vec<(usize, usize, crate::syntax::Kind)>>;

/// One file's change in a commit, from its first hunk on, with code highlighted past
/// each line's `+`, `-` or space. The flag says the diff was cut at 1 MB.
pub fn diff(root: &Path, hash: &str, parents: &[String], ch: &Change) -> (Vec<String>, Option<Spans>, bool) {
    let mut a = vec!["diff-tree", "-p", "-M", "--no-color", "--no-ext-diff"];
    a.extend(base(hash, parents));
    a.extend(["--", &ch.path]);
    a.extend(ch.from.as_deref());
    let (out, cut) = run(root, &a, 1 << 20);
    let lines = diff_lines(&out);
    let spans = crate::syntax::lang_for(&ch.path).map(|lang| {
        let code: Vec<String> = lines.iter().map(|l| if l.starts_with("@@") { String::new() } else { l.get(1..).unwrap_or("").to_string() }).collect();
        crate::syntax::highlight(lang, &code, 20_000).into_iter().map(|s| s.into_iter().map(|(a, b, k)| (a + 1, b + 1, k)).collect()).collect()
    });
    (lines, spans, cut)
}

pub fn diff_lines(out: &[u8]) -> Vec<String> {
    let s = String::from_utf8_lossy(out);
    s.lines().skip_while(|l| !l.starts_with("@@")).map(|l| if l.len() > 2000 { format!("{}…", &l[..l.floor_char_boundary(2000)]) } else { l.replace('\t', "    ") }).collect()
}

/// Pictures get their old and new versions shown instead of a diff.
pub fn is_picture(path: &str) -> bool {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    !ext.contains('/') && (crate::thumbs::is_image_ext(&ext) || ext == "svg")
}

/// A picture's bytes decoded and scaled to fit `px`, with its own size.
fn picture(bytes: &[u8], path: &str, px: u32) -> Option<(egui::ColorImage, [u32; 2])> {
    if path.to_ascii_lowercase().ends_with(".svg") {
        let (w, h, d) = crate::svg::render(bytes, px)?;
        return Some((egui::ColorImage::from_rgba_premultiplied([w as usize, h as usize], &d), [w, h]));
    }
    let img = crate::imgdec::decode_bytes(bytes)?;
    let dims = [img.w, img.h];
    let img = if img.w > px || img.h > px { crate::imgdec::downscale(&img, px) } else { img };
    Some((egui::ColorImage::from_rgba_unmultiplied([img.w as usize, img.h as usize], &img.px), dims))
}

/// Pictures larger than this are not read (a few large photos would fill memory).
const PICTURE_BYTES: u64 = 32 << 20;

/// One side of a changed picture: `old` reads it from the commit's first parent, under
/// the name it had there.
pub fn picture_side(root: &Path, hash: &str, parents: &[String], ch: &Change, old: bool, px: u32) -> Option<(egui::ColorImage, [u32; 2])> {
    let (rev, path) = if old { (parents.first()?.as_str(), ch.from.as_deref().unwrap_or(&ch.path)) } else { (hash, ch.path.as_str()) };
    let (bytes, cut) = run(root, &["cat-file", "blob", &format!("{rev}:{path}")], PICTURE_BYTES);
    if cut || bytes.is_empty() {
        return None;
    }
    picture(&bytes, path, px)
}

/// A file as it was in a commit.
pub enum Blob {
    Text(Vec<String>, Option<Spans>, bool),
    Picture(egui::ColorImage, [u32; 2]),
    /// Why there is nothing to show.
    Nothing(&'static str),
}

/// The file as of `rev`: highlighted text, a picture, or why neither.
pub fn blob(root: &Path, rev: &str, path: &str) -> Blob {
    let spec = format!("{rev}:{path}");
    if is_picture(path) {
        let (bytes, cut) = run(root, &["cat-file", "blob", &spec], PICTURE_BYTES);
        return match picture(&bytes, path, 2048) {
            _ if cut => Blob::Nothing("Too large to show"),
            Some((i, d)) => Blob::Picture(i, d),
            None => Blob::Nothing("Can't show this picture"),
        };
    }
    let (bytes, cut) = run(root, &["cat-file", "blob", &spec], 1 << 20);
    match crate::inspector::text_lines(&bytes) {
        Some(lines) => {
            let spans = crate::syntax::lang_for(path).map(|l| crate::syntax::highlight(l, &lines, 20_000));
            Blob::Text(lines, spans, cut)
        }
        None => Blob::Nothing("Binary file"),
    }
}

/// "3 days ago" for a commit time (seconds since the epoch).
pub fn ago(t: i64, now: i64) -> String {
    let d = (now - t).max(0);
    let (n, unit) = match d {
        0..60 => return "just now".into(),
        60..3600 => (d / 60, "minute"),
        3600..86_400 => (d / 3600, "hour"),
        86_400..604_800 => (d / 86_400, "day"),
        604_800..2_592_000 => (d / 604_800, "week"),
        2_592_000..31_536_000 => (d / 2_592_000, "month"),
        _ => (d / 31_536_000, "year"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

/// "5 files changed" above a commit's files.
pub fn summary(files: usize) -> String {
    format!("{files} file{} changed", if files == 1 { "" } else { "s" })
}

/// How a file's change splits the five blocks after its counts: (green, red). Fewer
/// than five lines fill as many blocks as there are lines, and a side that has any
/// lines always gets a block.
pub fn blocks(add: u32, del: u32) -> (usize, usize) {
    let t = add as u64 + del as u64;
    if t == 0 {
        return (0, 0);
    }
    let n = t.min(5);
    let mut g = (add as u64 * n + t / 2) / t;
    if add > 0 && g == 0 {
        g = 1;
    }
    if del > 0 && g == n {
        g = n - 1;
    }
    (g as usize, (n - g) as usize)
}

/// A folder heading or a file in a commit's files grouped by folder, in the order shown.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// `name` is what the heading says: a folder that only holds one folder is one
    /// heading ("src/ui"). `len` nodes inside it follow; `files` is how many files.
    Dir { path: String, name: String, depth: u16, len: usize, files: usize },
    File { idx: usize, depth: u16 },
}

/// Group many files by folder: worth it past eight files in more than one folder.
pub fn grouped(files: &[Change]) -> bool {
    fn dir(f: &Change) -> &str {
        f.path.rsplit_once('/').map_or("", |d| d.0)
    }
    files.len() > 8 && files.iter().any(|f| dir(f) != dir(&files[0]))
}

/// A commit's files as a tree: folders first, then files, each sorted by name.
pub fn group(files: &[Change]) -> Vec<Node> {
    #[derive(Default)]
    struct T {
        dirs: BTreeMap<String, T>,
        files: Vec<usize>,
    }
    fn count(t: &T) -> usize {
        t.files.len() + t.dirs.values().map(count).sum::<usize>()
    }
    fn walk(t: &T, prefix: &str, depth: u16, out: &mut Vec<Node>) {
        for (name, d) in &t.dirs {
            let (mut name, mut d) = (name.clone(), d);
            while d.files.is_empty() && d.dirs.len() == 1 {
                let Some((n, inner)) = d.dirs.iter().next() else { break };
                name = format!("{name}/{n}");
                d = inner;
            }
            let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            let at = out.len();
            out.push(Node::Dir { path: path.clone(), name, depth, len: 0, files: count(d) });
            walk(d, &path, depth + 1, out);
            let n = out.len() - at - 1;
            if let Node::Dir { len, .. } = &mut out[at] {
                *len = n;
            }
        }
        out.extend(t.files.iter().map(|&idx| Node::File { idx, depth }));
    }
    let mut root = T::default();
    for (i, f) in files.iter().enumerate() {
        let mut t = &mut root;
        if let Some((dir, _)) = f.path.rsplit_once('/') {
            for p in dir.split('/') {
                t = t.dirs.entry(p.to_string()).or_default();
            }
        }
        t.files.push(i);
    }
    for t in std::iter::once(&mut root) {
        sort_files(t, files);
    }
    fn sort_files(t: &mut T, files: &[Change]) {
        t.files.sort_by(|&a, &b| files[a].path.cmp(&files[b].path));
        for d in t.dirs.values_mut() {
            sort_files(d, files);
        }
    }
    let mut out = Vec::with_capacity(files.len() + 8);
    walk(&root, "", 0, &mut out);
    out
}

/// One or two letters for an author's circle: the first and last words' initials.
pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split(|c: char| c.is_whitespace() || matches!(c, '.' | '_' | '-' | '@')).filter(|w| w.chars().next().is_some_and(char::is_alphanumeric)).collect();
    let first = |w: &str| w.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default();
    match words.as_slice() {
        [] => "?".into(),
        [w] => first(w),
        [a, .., z] => first(a) + &first(z),
    }
}

/// Which of the author colours a person has: from their email (their name when there is
/// none), so the same person keeps it everywhere and across sessions.
pub fn author_hue(email: &str, name: &str) -> usize {
    let key = if email.trim().is_empty() { name } else { email }.trim().to_lowercase();
    let mut h: u32 = 0x811c_9dc5;
    for b in key.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    (h % AUTHOR_HUES.len() as u32) as usize
}

const AUTHOR_HUES: [u32; 10] = [0x7aa2f7, 0xd9a05b, 0xb58ee0, 0x62b58f, 0xe0849a, 0x58b4c9, 0xc9b36a, 0x9aa5b8, 0xe07a5f, 0x8fbf6a];

/// An author's colour, toned like the lanes so it sits on the theme.
pub fn author_color(pal: &Palette, email: &str, name: &str) -> Color32 {
    let h = AUTHOR_HUES[author_hue(email, name)];
    let c = Color32::from_rgb((h >> 16) as u8, (h >> 8) as u8, h as u8);
    let (to, t) = if pal.dark { (pal.text_dim, 0.15) } else { (pal.text, 0.3) };
    c.lerp_to_gamma(to, t)
}

/// A worker's answer, once it has one.
pub(crate) type Slot<T> = Arc<Mutex<Option<T>>>;

pub(crate) fn spawn<T: Send + 'static>(ctx: &egui::Context, f: impl FnOnce() -> T + Send + 'static) -> Slot<T> {
    let (slot, ctx) = (Slot::default(), ctx.clone());
    let theirs = Arc::clone(&slot);
    std::thread::spawn(move || {
        let v = f();
        if let Ok(mut s) = theirs.lock() {
            *s = Some(v);
        }
        ctx.request_repaint();
    });
    slot
}

pub(crate) fn take<T>(slot: &Option<Slot<T>>) -> Option<T> {
    slot.as_ref()?.lock().ok()?.take()
}

/// Between the view and the reader of the running `git log`.
#[derive(Default)]
struct Pages {
    /// The view wants another page.
    want: bool,
    got: Vec<Commit>,
    /// git has nothing more (or could not run).
    end: bool,
    /// The view is gone: stop, and kill git.
    closed: bool,
    /// git's pid while the reader has not yet reaped it.
    pid: u32,
}

struct Feed(Arc<(Mutex<Pages>, Condvar)>);

fn lock(m: &Mutex<Pages>) -> MutexGuard<'_, Pages> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Feed {
    fn lock(&self) -> MutexGuard<'_, Pages> {
        lock(&self.0.0)
    }
    fn ask(&self) {
        self.lock().want = true;
        self.0.1.notify_one();
    }
}

impl Drop for Feed {
    /// A long walk (a huge repository without a commit-graph) stops with its history.
    fn drop(&mut self) {
        let mut p = self.lock();
        p.closed = true;
        if p.pid != 0 {
            unsafe { libc::kill(p.pid as i32, libc::SIGKILL) };
        }
        drop(p);
        self.0.1.notify_one();
    }
}

fn feed(root: PathBuf, ctx: &egui::Context, args: Vec<String>, filter: Option<Arc<crate::gitfind::Query>>) -> Feed {
    let f = Feed(Arc::default());
    let (sh, ctx) = (Arc::clone(&f.0), ctx.clone());
    std::thread::spawn(move || {
        let (m, cv) = &*sh;
        let Ok(mut child) = git(&root).args(&args).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else {
            lock(m).end = true;
            ctx.request_repaint();
            return;
        };
        lock(m).pid = child.id();
        if let Some(out) = child.stdout.take() {
            let mut r = BufReader::new(out);
            loop {
                let mut p = lock(m);
                while !p.want && !p.closed {
                    p = cv.wait(p).unwrap_or_else(|e| e.into_inner());
                }
                if p.closed {
                    break;
                }
                p.want = false;
                drop(p);
                let end = match filter.as_deref() {
                    // A search's page fills as matches turn up.
                    Some(q) => read_matching(&mut r, PAGE, q, |v| {
                        lock(m).got.extend(v);
                        ctx.request_repaint();
                    }),
                    None => {
                        let page = read_page(&mut r, PAGE);
                        let end = page.len() < PAGE;
                        lock(m).got.extend(page);
                        end
                    }
                };
                let mut p = lock(m);
                p.end = end;
                ctx.request_repaint();
                if p.end || p.closed {
                    break;
                }
            }
        }
        // Reaped only after the pid is cleared, so the view never signals a reused one.
        let mut p = lock(m);
        p.pid = 0;
        p.end = true;
        drop(p);
        let _ = child.kill();
        let _ = child.wait();
    });
    f
}

/// One list of commits: the whole history, or the commits that changed one file, either
/// narrowed by a search.
struct List {
    path: Option<String>,
    query: Option<Arc<crate::gitfind::Query>>,
    commits: Vec<Commit>,
    rows: Vec<Row>,
    lanes: Lanes,
    feed: Option<Feed>,
    asked: bool,
    done: bool,
    sel: usize,
}

impl List {
    fn new(path: Option<String>) -> List {
        List { path, query: None, commits: vec![], rows: vec![], lanes: Lanes::default(), feed: None, asked: false, done: false, sel: 0 }
    }

    fn args(&self) -> Vec<String> {
        list_args(self.path.as_deref(), self.query.as_deref())
    }

    /// Drawn in one line rather than as a graph: its commits' parents may not be in it.
    fn straight(&self) -> bool {
        self.path.is_some() || self.query.is_some()
    }
    fn ask(&mut self) {
        if let Some(f) = self.feed.as_ref().filter(|_| !self.done && !self.asked) {
            f.ask();
            self.asked = true;
        }
    }
}

type DiffText = (Vec<String>, Option<Spans>, bool);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
enum Item {
    Dir(usize),
    File(usize),
    /// A comparison's heading over the commits only one side has (0 old, 1 new), and one
    /// of those commits.
    Side(u8),
    Pick(u8, usize),
}

/// Two branches or commits compared, in place of one commit (see `gitcompare`).
struct Cmp {
    /// The old side and the new, as named and as asked of git.
    names: [String; 2],
    revs: [String; 2],
    /// Two commits on one line are put older first.
    order: bool,
    data: Option<Result<crate::gitcompare::Sides, String>>,
    job: Option<Slot<Result<crate::gitcompare::Sides, String>>>,
    /// The files since the two split (what the new side adds), not the two directly.
    split: bool,
    folded: [bool; 2],
}

/// A file whose diff is open under its row (for a picture: its larger versions).
struct Inline {
    file: usize,
    text: Option<DiffText>,
    job: Option<Slot<DiffText>>,
    wrapped: Option<crate::inspector::Wrapped>,
}

/// One side of a changed picture, decoded only once it is on screen.
struct Pic {
    file: usize,
    old: bool,
    px: u32,
    started: bool,
    job: Option<Slot<Option<(egui::ColorImage, [u32; 2])>>>,
    tex: Option<(egui::TextureHandle, [u32; 2])>,
    failed: bool,
}

/// Pictures decoded at once; the rest wait their turn.
const PICTURE_JOBS: usize = 3;

struct Open {
    /// The commit's place in the list (`usize::MAX` for a comparison).
    at: usize,
    /// What the files are compared between: the commit and its parents, or for a
    /// comparison its new side and the old side (or where they split).
    hash: String,
    parents: Vec<String>,
    cmp: Option<Box<Cmp>>,
    /// The keyboard's place in `items`.
    sel: usize,
    info: Option<(String, Vec<Change>)>,
    job: Option<Slot<(String, Vec<Change>)>>,
    /// Lines added and removed in all, and the largest counts of one file (so the
    /// columns are as wide as they need to be).
    totals: (u64, u64),
    widest: (u32, u32),
    picture: Vec<bool>,
    /// The files grouped by folder (empty when they are few), and the folded headings.
    nodes: Vec<Node>,
    folded: HashSet<usize>,
    /// What the file list shows, with each one's depth.
    items: Vec<(Item, u16)>,
    inline: Vec<Inline>,
    pics: Vec<Pic>,
    /// A file in the whole panel.
    diff: Option<Diff>,
    reveal: bool,
}

/// One file in the whole panel: its change, or (`blob`) the file as of the commit.
struct Diff {
    file: usize,
    blob: bool,
    text: Option<View>,
    job: Option<Slot<Got>>,
}

enum Got {
    Diff(DiffText),
    Blob(Blob),
}

enum View {
    /// Lines, highlighting, cut at 1 MB, and whether they are a diff.
    Lines(Vec<String>, Option<Spans>, bool, bool),
    Pic(egui::TextureHandle, [u32; 2]),
    Msg(&'static str),
}

impl Diff {
    fn new(file: usize, blob: bool) -> Diff {
        Diff { file, blob, text: None, job: None }
    }
}

impl Open {
    fn new(at: usize, hash: String, parents: Vec<String>) -> Open {
        Open { at, hash, parents, cmp: None, sel: 0, info: None, job: None, totals: (0, 0), widest: (0, 0), picture: vec![], nodes: vec![], folded: HashSet::new(), items: vec![], inline: vec![], pics: vec![], diff: None, reveal: false }
    }

    fn compare(names: [String; 2], revs: [String; 2], order: bool) -> Open {
        let mut o = Open::new(usize::MAX, String::new(), vec![]);
        o.cmp = Some(Box::new(Cmp { names, revs, order, data: None, job: None, split: true, folded: [false; 2] }));
        o
    }

    /// The comparison's sides, once read.
    fn sides(&self) -> Option<&crate::gitcompare::Sides> {
        self.cmp.as_ref()?.data.as_ref()?.as_ref().ok()
    }

    /// The old side the files are compared with: where the two split, or the old side
    /// itself.
    fn cmp_base(&self) -> Option<String> {
        let (c, s) = (self.cmp.as_ref()?, self.sides()?);
        Some(if c.split { s.base.clone().unwrap_or_else(|| s.hashes[0].clone()) } else { s.hashes[0].clone() })
    }

    /// Files since the split, or between the two: read again.
    fn set_split(&mut self, split: bool, tree: bool) {
        let Some(c) = self.cmp.as_mut().filter(|c| c.split != split) else { return };
        c.split = split;
        self.parents = self.cmp_base().into_iter().collect();
        (self.info, self.job, self.diff) = (None, None, None);
        self.inline.clear();
        self.pics.clear();
        self.rebuild(tree);
    }

    fn files(&self) -> &[Change] {
        self.info.as_ref().map_or(&[][..], |i| &i.1[..])
    }

    fn set_info(&mut self, info: (String, Vec<Change>), tree: bool) {
        let files = &info.1;
        let mut totals = (0, 0);
        let mut widest = (0, 0);
        for (a, d) in files.iter().filter_map(|f| f.lines) {
            totals = (totals.0 + a as u64, totals.1 + d as u64);
            widest = (widest.0.max(a), widest.1.max(d));
        }
        (self.totals, self.widest) = (totals, widest);
        self.picture = files.iter().map(|f| is_picture(&f.path)).collect();
        self.nodes = if grouped(files) { group(files) } else { vec![] };
        self.info = Some(info);
        self.rebuild(tree);
    }

    /// The rows of the file list, as the grouping and the folded headings leave them; a
    /// comparison's commits, new side first, above them.
    fn rebuild(&mut self, tree: bool) {
        let keep = self.items.get(self.sel).copied();
        self.items.clear();
        if let (Some(s), Some(c)) = (self.sides(), self.cmp.as_ref()) {
            let mut v = vec![];
            for side in [1u8, 0] {
                v.push((Item::Side(side), 0));
                if !c.folded[side as usize] {
                    v.extend((0..s.only[side as usize].len()).map(|i| (Item::Pick(side, i), 1)));
                }
            }
            self.items = v;
        }
        if tree && !self.nodes.is_empty() {
            let mut i = 0;
            while i < self.nodes.len() {
                match &self.nodes[i] {
                    Node::Dir { depth, len, .. } => {
                        self.items.push((Item::Dir(i), *depth));
                        if self.folded.contains(&i) {
                            i += len;
                        }
                    }
                    Node::File { idx, depth } => self.items.push((Item::File(*idx), *depth)),
                }
                i += 1;
            }
        } else {
            let n = self.files().len();
            self.items.extend((0..n).map(|i| (Item::File(i), 0)));
        }
        self.sel = keep.and_then(|k| self.items.iter().position(|x| *x == k)).unwrap_or(self.sel).min(self.items.len().saturating_sub(1));
    }

    /// The files in the order the list shows them, folded ones included.
    fn order(&self, tree: bool) -> Vec<usize> {
        if tree && !self.nodes.is_empty() {
            self.nodes.iter().filter_map(|n| if let Node::File { idx, .. } = n { Some(*idx) } else { None }).collect()
        } else {
            (0..self.files().len()).collect()
        }
    }

    fn expanded(&self, file: usize) -> bool {
        self.inline.iter().any(|x| x.file == file)
    }

    /// Enter or a click on a row: a heading folds or unfolds, a file's diff opens or
    /// closes under it; a comparison's commit is returned, to be opened.
    fn toggle(&mut self, item: usize, tree: bool) -> Option<String> {
        match self.items.get(item).map(|x| x.0) {
            Some(Item::Dir(n)) => {
                if !self.folded.remove(&n) {
                    self.folded.insert(n);
                }
                self.rebuild(tree);
            }
            Some(Item::File(f)) => match self.inline.iter().position(|x| x.file == f) {
                Some(k) => {
                    self.inline.remove(k);
                }
                None => self.inline.push(Inline { file: f, text: None, job: None, wrapped: None }),
            },
            Some(Item::Side(side)) => {
                if self.sides().is_some_and(|s| !s.only[side as usize].is_empty()) {
                    if let Some(c) = self.cmp.as_mut() {
                        c.folded[side as usize] ^= true;
                    }
                    self.rebuild(tree);
                }
            }
            Some(Item::Pick(side, i)) => return self.sides().and_then(|s| s.only[side as usize].get(i)).map(|c| c.hash.clone()),
            None => {}
        }
        None
    }

    /// `[` and `]` in the list: open the selected file's diff alone, then move to the
    /// previous or next file and open its instead.
    fn walk(&mut self, d: isize) {
        let only = |o: &mut Open, f: usize| {
            o.inline.retain(|x| x.file == f);
            if o.inline.is_empty() {
                o.inline.push(Inline { file: f, text: None, job: None, wrapped: None });
            }
            o.reveal = true;
        };
        if let Some(&(Item::File(f), _)) = self.items.get(self.sel) {
            if !(self.expanded(f) && self.inline.len() == 1) {
                return only(self, f);
            }
        }
        let mut i = self.sel as isize;
        loop {
            i += d;
            let Some(&(it, _)) = usize::try_from(i).ok().and_then(|i| self.items.get(i)) else { return };
            if let Item::File(f) = it {
                self.sel = i as usize;
                return only(self, f);
            }
        }
    }

    /// `[` and `]` with a file in the whole panel: the previous or next file there.
    fn step_file(&mut self, d: isize, tree: bool) {
        let Some((cur, blob)) = self.diff.as_ref().map(|x| (x.file, x.blob)) else { return };
        let order = self.order(tree);
        let Some(k) = order.iter().position(|&f| f == cur) else { return };
        let Some(&next) = k.checked_add_signed(d).and_then(|k| order.get(k)) else { return };
        self.diff = Some(Diff::new(next, blob));
        if let Some(i) = self.items.iter().position(|x| x.0 == Item::File(next)) {
            self.sel = i;
            self.reveal = true;
        }
    }
}

/// A picture's side, asked for if it is not yet and a worker is free.
#[allow(clippy::too_many_arguments)]
fn want_pic(pics: &mut Vec<Pic>, ctx: &egui::Context, root: &Path, hash: &str, parents: &[String], ch: &Change, file: usize, old: bool, px: u32) -> usize {
    let k = match pics.iter().position(|p| p.file == file && p.old == old && p.px == px) {
        Some(k) => k,
        None => {
            pics.push(Pic { file, old, px, started: false, job: None, tex: None, failed: false });
            pics.len() - 1
        }
    };
    let busy = pics.iter().filter(|p| p.job.is_some()).count();
    let p = &mut pics[k];
    if !p.started && busy < PICTURE_JOBS {
        p.started = true;
        let (root, hash, parents, ch) = (root.to_path_buf(), hash.to_string(), parents.to_vec(), ch.clone());
        p.job = Some(spawn(ctx, move || picture_side(&root, &hash, &parents, &ch, old, px)));
    }
    k
}

/// A commit to open once the page holding it is read.
enum Want {
    Hash(String),
    At(usize),
}

pub struct History {
    pub root: PathBuf,
    branch: String,
    /// The inspector was opened for the history: closing one closes both.
    pub opened: bool,
    list: List,
    /// The whole history, kept while one file's is shown.
    all: Option<List>,
    /// The keyboard has been used: the selected row is marked from then on.
    keyed: bool,
    reveal: bool,
    open: Option<Open>,
    /// Where the open diff's wrapped rows start, when wrapping.
    wrapped: Option<crate::inspector::Wrapped>,
    /// A file's diff side by side: its rows, worked out once.
    side: crate::sidediff::View,
    /// Many files are shown by folder (the toggle above them switches).
    tree: bool,
    want: Option<Want>,
    /// The work tree's changes: the row above the commits, and their page.
    pub(crate) changes: crate::gitchanges::Changes,
    /// The keyboard is on the Changes row (above the first commit).
    on_changes: bool,
    /// Where the selected commit's menu opens from the keyboard: under its row when it
    /// was drawn last frame.
    menu_at: Pos2,
    /// The find bar over the graph, while it is open.
    search: Option<Box<Search>>,
    /// The commit marked to compare another with.
    pub mark: Option<String>,
    /// The comparison a commit was opened from: Back returns to it.
    back_to: Option<Box<Open>>,
    /// The folder shown, from the top of the work tree (a search started there looks in it).
    pub here: String,
}

/// Searching the history: the bar, and the list as it was before, put back as it was
/// (its place and its scroll included) once the search is cleared.
struct Search {
    find: crate::findbar::Finder,
    /// The text the list was narrowed for, and the text as last seen with when it changed:
    /// git starts once typing pauses.
    applied: String,
    seen: (String, f64),
    base: Option<List>,
    /// A path filled in: the caret goes after it rather than selecting it.
    caret_end: bool,
    /// Enter has stepped through this list's commits: the first Enter picks the first.
    stepped: bool,
}

/// How long typing pauses before the search runs.
const SEARCH_WAIT: f64 = 0.25;

/// What a simulated user can check: the state behind the view.
#[cfg(test)]
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Probe {
    /// The file whose history is shown.
    pub filter: Option<String>,
    pub at: Option<usize>,
    pub subject: Option<String>,
    pub tree: bool,
    /// Rows of the file list, and the keyboard's place in them.
    pub items: usize,
    pub sel: usize,
    /// Files with their diff open under them, and how many of those have lines.
    pub inline: Vec<usize>,
    pub inline_lines: Vec<usize>,
    /// Rows the open diffs are drawn in when wrapped.
    pub inline_wrapped: Vec<usize>,
    /// The file shown in the whole panel, as of the commit or as its change.
    pub full: Option<(usize, bool)>,
    /// Picture sides decoded, and those that could not be.
    pub pictures: usize,
    pub failed: usize,
    pub folded: usize,
    /// A comparison: its sides as named, how many commits only each has, whether its files
    /// are since the split, and how many files differ (once read).
    pub cmp: Option<[String; 2]>,
    pub cmp_counts: Option<[usize; 2]>,
    pub split: bool,
    /// The search's text as typed, and whether the list is narrowed by it.
    pub search: Option<String>,
    pub narrowed: bool,
    pub mark: Option<String>,
}

#[cfg(test)]
impl History {
    /// (commits loaded, all read, commit open, its files, lines shown in the whole panel).
    pub fn state(&self) -> (usize, bool, Option<usize>, Option<usize>, Option<usize>) {
        let o = self.open.as_ref();
        let lines = o.and_then(|o| match o.diff.as_ref()?.text.as_ref()? {
            View::Lines(l, ..) => Some(l.len()),
            _ => None,
        });
        (self.list.commits.len(), self.list.done, o.map(|o| o.at), o.and_then(|o| o.info.as_ref()).map(|i| i.1.len()), lines)
    }
    pub fn probe(&self) -> Probe {
        let o = self.open.as_ref();
        Probe {
            filter: self.list.path.clone(),
            at: o.map(|o| o.at),
            subject: o.and_then(|o| self.list.commits.get(o.at)).map(|c| c.subject.clone()),
            tree: self.tree,
            items: o.map_or(0, |o| o.items.len()),
            sel: o.map_or(0, |o| o.sel),
            inline: o.map_or(vec![], |o| o.inline.iter().map(|x| x.file).collect()),
            inline_lines: o.map_or(vec![], |o| o.inline.iter().filter_map(|x| x.text.as_ref().map(|t| t.0.len())).collect()),
            inline_wrapped: o.map_or(vec![], |o| o.inline.iter().filter_map(|x| x.wrapped.as_ref().map(|w| w.rows.len())).collect()),
            full: o.and_then(|o| o.diff.as_ref().map(|d| (d.file, d.blob))),
            pictures: o.map_or(0, |o| o.pics.iter().filter(|p| p.tex.is_some()).count()),
            failed: o.map_or(0, |o| o.pics.iter().filter(|p| p.failed).count()),
            folded: o.map_or(0, |o| o.folded.len()),
            cmp: o.and_then(|o| o.cmp.as_ref()).map(|c| c.names.clone()),
            cmp_counts: o.and_then(|o| o.sides()).map(|s| s.counts),
            split: o.and_then(|o| o.cmp.as_ref()).is_some_and(|c| c.split),
            search: self.search.as_ref().map(|s| s.find.query.clone()),
            narrowed: self.list.query.is_some(),
            mark: self.mark.clone(),
        }
    }
    /// The subjects of the commits listed, in order.
    pub fn subjects(&self) -> Vec<String> {
        self.list.commits.iter().map(|c| c.subject.clone()).collect()
    }
    /// The keyboard's commit in the graph.
    pub fn sel(&self) -> usize {
        self.list.sel
    }
    /// The open commit's files, as listed.
    pub fn files(&self) -> Vec<Change> {
        self.open.as_ref().map_or(vec![], |o| o.files().to_vec())
    }
    /// The hash of a loaded commit.
    pub fn hash(&self, i: usize) -> Option<String> {
        self.list.commits.get(i).map(|c| c.hash.clone())
    }
    /// What the whole panel shows: (first lines, as of the commit).
    pub fn full_text(&self) -> Option<Vec<String>> {
        match self.open.as_ref()?.diff.as_ref()?.text.as_ref()? {
            View::Lines(l, ..) => Some(l.iter().take(40).cloned().collect()),
            _ => None,
        }
    }
    /// Rows the open diff is drawn in when wrapped (None when not wrapping).
    pub fn wrapped_rows(&self) -> Option<usize> {
        self.wrapped.as_ref().map(|w| w.rows.len())
    }
    pub fn changes(&self) -> crate::gitchanges::Probe {
        self.changes.probe()
    }
    /// The keyboard is on the Changes row.
    pub fn on_changes(&self) -> bool {
        self.on_changes
    }
    /// The running git (0 when none).
    pub fn git_pid(&self) -> u32 {
        self.list.feed.as_ref().map_or(0, |f| f.lock().pid)
    }
    pub fn more(&mut self, ctx: &egui::Context) {
        self.poll(ctx);
        if let Some(f) = &self.list.feed {
            f.ask();
        }
    }
}

impl History {
    pub fn new(root: PathBuf, branch: String, opened: bool) -> History {
        let changes = crate::gitchanges::Changes::new(root.clone());
        History { root, branch, opened, list: List::new(None), all: None, keyed: false, reveal: false, open: None, wrapped: None, side: Default::default(), tree: true, want: None, changes, on_changes: false, menu_at: Pos2::ZERO, search: None, mark: None, back_to: None, here: String::new() }
    }

    /// Read the history again from the top: HEAD moved (a commit, a switch). A search
    /// runs again over the new history.
    pub fn reload(&mut self) {
        self.list = List::new(None);
        self.all = None;
        self.open = None;
        self.back_to = None;
        self.want = None;
        self.wrapped = None;
        self.reveal = true;
        if let Some(s) = self.search.as_mut() {
            s.base = None;
            s.applied.clear();
            s.seen.1 = f64::MIN;
        }
    }

    /// Ctrl+F: the find bar over the graph, its field focused. Started in a subfolder of
    /// the work tree, it begins with that folder's `path:`, the caret after it.
    pub fn find(&mut self) {
        self.open = None;
        self.back_to = None;
        self.changes.open = false;
        if self.search.is_none() {
            let text = if self.list.path.is_some() { String::new() } else { crate::gitfind::start_text(&self.here) };
            let caret_end = !text.is_empty();
            self.search = Some(Box::new(Search { find: crate::findbar::Finder::new(text), applied: String::new(), seen: (String::new(), f64::MIN), base: None, caret_end, stepped: false }));
        }
        if let Some(s) = self.search.as_mut() {
            s.find.focus = true;
        }
    }

    /// The search closed: the list as it was before it.
    pub fn end_search(&mut self) {
        if let Some(b) = self.search.take().and_then(|s| s.base) {
            self.list = b;
            self.open = None;
            self.back_to = None;
            self.want = None;
            self.wrapped = None;
        }
    }

    /// The search's text, once typing pauses, as the list's query: a narrower `git log`,
    /// or the list from before the search when nothing is left to look for.
    fn narrow(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let Some(s) = self.search.as_mut() else { return };
        let text = s.find.query.trim();
        if text != s.seen.0 {
            s.seen = (text.to_string(), now);
        }
        if s.seen.0 == s.applied {
            return;
        }
        let wait = SEARCH_WAIT - (now - s.seen.1);
        if wait > 0.0 && !s.seen.0.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait));
            return;
        }
        s.applied = s.seen.0.clone();
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        let q = crate::gitfind::parse(&s.applied, secs);
        if self.list.query.as_deref() == Some(&q) {
            return;
        }
        if q.is_empty() {
            match s.base.take() {
                Some(b) => self.list = b,
                None => return,
            }
        } else {
            let base = s.base.get_or_insert_with(|| std::mem::replace(&mut self.list, List::new(None)));
            let mut l = List::new(base.path.clone());
            l.query = Some(Arc::new(q));
            self.list = l;
        }
        s.stepped = false;
        self.open = None;
        self.back_to = None;
        self.want = None;
        self.wrapped = None;
    }

    /// Two branches or commits compared, in place of the graph: `old` and `new` as shown
    /// and as git is asked for them. With `order`, the older of two on one line is the old
    /// side.
    pub fn compare(&mut self, names: [String; 2], revs: [String; 2], order: bool) {
        self.changes.open = false;
        self.back_to = None;
        self.wrapped = None;
        self.open = Some(Open::compare(names, revs, order));
    }

    /// The status marks were read anew (`repo`): follow the work tree's changes, and a
    /// branch switched elsewhere.
    pub fn follow(&mut self, repo: Option<&std::rc::Rc<crate::gitstatus::Repo>>, ctx: &egui::Context) {
        let Some(r) = repo.filter(|r| r.root == self.root) else { return };
        self.changes.follow(std::rc::Rc::as_ptr(r) as usize, ctx);
        if !r.branch.is_empty() && r.branch != self.branch {
            self.branch = r.branch.clone();
            if self.list.path.is_none() {
                self.reload();
            }
        }
    }

    /// Back to the graph (the palette command run while the history is open).
    pub fn top(&mut self) {
        self.open = None;
        self.back_to = None;
        self.changes.open = false;
        self.reveal = true;
    }

    /// The Changes page, in place of the graph.
    pub fn show_changes(&mut self) {
        self.open = None;
        self.changes.open = true;
    }

    /// Take in what the workers finished, and start what is wanted.
    pub(crate) fn poll(&mut self, ctx: &egui::Context) {
        if self.changes.poll(ctx) {
            self.reload();
        }
        self.narrow(ctx);
        // The first page is asked for here, the next ones as the graph nears its end.
        let l = &mut self.list;
        if l.feed.is_none() {
            l.feed = Some(feed(self.root.clone(), ctx, l.args(), l.query.clone()));
        }
        let Some(f) = l.feed.as_ref() else { return };
        if l.commits.is_empty() && !l.asked && !l.done {
            f.ask();
            l.asked = true;
        }
        let mut p = f.lock();
        let page = std::mem::take(&mut p.got);
        l.done = p.end;
        drop(p);
        if !page.is_empty() || l.done {
            l.asked = false;
        }
        // One file's commits are not a graph, nor are a search's: they go down in one line.
        let follow = l.straight();
        for c in page {
            l.rows.push(if follow { Row { col: 0, up: !l.commits.is_empty(), ins: vec![], outs: vec![0], pass: vec![] } } else { l.lanes.place(&c) });
            l.commits.push(c);
        }
        if follow && l.done {
            if let Some(r) = l.rows.last_mut() {
                r.outs.clear();
            }
        }
        if let Some(w) = self.want.take() {
            let l = &mut self.list;
            let at = match &w {
                Want::Hash(x) => l.commits.iter().position(|c| c.hash == *x),
                Want::At(i) => (*i < l.commits.len()).then_some(*i),
            };
            match at {
                Some(i) => self.open_commit(i),
                None if l.done => {}
                None => {
                    l.ask();
                    self.want = Some(w);
                }
            }
        }
        let tree = self.tree;
        if let Some(o) = &mut self.open {
            let root = &self.root;
            if let Some(cm) = o.cmp.as_mut() {
                if cm.data.is_none() && cm.job.is_none() {
                    let (root, [a, b], order) = (root.clone(), cm.revs.clone(), cm.order);
                    cm.job = Some(spawn(ctx, move || crate::gitcompare::sides(&root, &a, &b, order)));
                }
                if let Some(r) = take(&cm.job) {
                    cm.job = None;
                    if let Ok(s) = &r {
                        if s.swapped {
                            cm.names.swap(0, 1);
                        }
                        // A long side starts folded, so the files are in sight.
                        cm.folded = s.only.each_ref().map(|v| v.len() > 10);
                        // Two on one line, or with nothing in common: one way to compare.
                        if s.base.is_none() || s.base.as_deref() == Some(s.hashes[0].as_str()) {
                            cm.split = false;
                        }
                        o.hash = s.hashes[1].clone();
                    }
                    cm.data = Some(r);
                    o.parents = o.cmp_base().into_iter().collect();
                    o.rebuild(tree);
                }
            }
            // A comparison's files wait for its sides.
            let ready = o.cmp.is_none() || o.sides().is_some();
            if ready && o.info.is_none() && o.job.is_none() {
                let (root, hash, parents, cmp) = (root.clone(), o.hash.clone(), o.parents.clone(), o.cmp.is_some());
                o.job = Some(spawn(ctx, move || match parents.first() {
                    Some(old) if cmp => (String::new(), files(&root, old, &hash)),
                    _ => changes(&root, &hash, &parents),
                }));
            }
            if let Some(r) = take(&o.job) {
                o.set_info(r, tree);
                o.job = None;
            }
            let (hash, parents) = (&o.hash, &o.parents);
            if let Some((_, files)) = &o.info {
                for x in &mut o.inline {
                    if x.text.is_none() && x.job.is_none() && !o.picture[x.file] {
                        let (root, hash, parents, ch) = (root.clone(), hash.clone(), parents.clone(), files[x.file].clone());
                        x.job = Some(spawn(ctx, move || diff(&root, &hash, &parents, &ch)));
                    }
                    if let Some(r) = take(&x.job) {
                        x.text = Some(r);
                        x.job = None;
                    }
                }
                if let Some(d) = &mut o.diff {
                    if d.text.is_none() && d.job.is_none() && (d.blob || !o.picture[d.file]) {
                        let (root, hash, parents, ch, blob_of) = (root.clone(), hash.clone(), parents.clone(), files[d.file].clone(), d.blob);
                        d.job = Some(spawn(ctx, move || {
                            if !blob_of {
                                return Got::Diff(diff(&root, &hash, &parents, &ch));
                            }
                            // A deleted file is shown as it was just before.
                            match (ch.status, parents.first()) {
                                (b'D', Some(p)) => Got::Blob(blob(&root, p, &ch.path)),
                                (b'D', None) => Got::Blob(Blob::Nothing("Deleted here")),
                                _ => Got::Blob(blob(&root, &hash, &ch.path)),
                            }
                        }));
                    }
                    if let Some(g) = take(&d.job) {
                        d.job = None;
                        d.text = Some(match g {
                            Got::Diff((l, s, cut)) => View::Lines(l, s, cut, true),
                            Got::Blob(Blob::Text(l, s, cut)) => View::Lines(l, s, cut, false),
                            Got::Blob(Blob::Picture(img, dims)) => View::Pic(ctx.load_texture("git-file", img, egui::TextureOptions::LINEAR), dims),
                            Got::Blob(Blob::Nothing(m)) => View::Msg(m),
                        });
                    }
                }
            }
            for p in &mut o.pics {
                if let Some(r) = take(&p.job) {
                    p.job = None;
                    match r {
                        Some((img, dims)) => p.tex = Some((ctx.load_texture("git-picture", img, egui::TextureOptions::LINEAR), dims)),
                        None => p.failed = true,
                    }
                }
            }
        }
    }

    fn open_commit(&mut self, i: usize) {
        if let Some(c) = self.list.commits.get(i) {
            self.list.sel = i;
            self.reveal = true;
            self.wrapped = None;
            self.open = Some(Open::new(i, c.hash.clone(), c.parents.clone()));
        }
    }

    /// A commit by its hash (a parent link, a blame line, a comparison's commit): looked
    /// for in the pages still to come, and in the whole history when one file's or a
    /// search's does not have it. Back from it returns to a comparison it came from.
    pub fn open_hash(&mut self, hash: String) {
        if self.open.as_ref().is_some_and(|o| o.cmp.is_some()) {
            self.back_to = self.open.take().map(Box::new);
        }
        if self.list.query.is_some() && !self.list.commits.iter().any(|c| c.hash == hash) {
            let back = self.back_to.take();
            self.end_search();
            self.back_to = back;
        }
        if self.list.path.is_some() && !self.list.commits.iter().any(|c| c.hash == hash) {
            let back = self.back_to.take();
            self.show_all();
            self.back_to = back;
        }
        self.want = Some(Want::Hash(hash));
    }

    /// The next newer (-1) or older (+1) commit in the list.
    fn step_commit(&mut self, d: isize) {
        if self.open.as_ref().is_some_and(|o| o.cmp.is_some()) {
            return;
        }
        let Some(at) = self.open.as_ref().and_then(|o| o.at.checked_add_signed(d)) else { return };
        if at < self.list.commits.len() {
            self.open_commit(at);
        } else if !self.list.done {
            self.want = Some(Want::At(at));
            self.list.ask();
        }
    }

    fn back(&mut self) {
        match &mut self.open {
            Some(o) if o.diff.is_some() => o.diff = None,
            _ => match self.back_to.take() {
                Some(b) => {
                    self.wrapped = None;
                    self.open = Some(*b);
                }
                None => self.top(),
            },
        }
    }

    /// Commit `i`'s menu, opening at `pos`; `keyed`, its first item is ready for Enter.
    fn commit_menu(&self, i: usize, pos: Pos2, keyed: bool, panel: u64) -> Option<crate::popups::Popup> {
        let c = self.list.commits.get(i)?;
        let tags = c.refs.iter().filter(|r| r.0 == RefKind::Tag).map(|r| r.1.clone()).collect();
        Some(crate::popups::Popup::GitCommit(crate::popups::CommitMenu {
            root: self.root.clone(),
            hash: c.hash.clone(),
            subject: c.subject.clone(),
            merge: c.parents.len() > 1,
            head: c.refs.iter().any(|r| r.0 == RefKind::Head),
            tags,
            branch: self.branch.clone(),
            pos,
            sel: if keyed { 0 } else { usize::MAX },
            marked: self.mark.clone(),
            panel,
        }))
    }

    /// Shift+F10 on the graph: the selected commit's menu, under its row.
    pub fn menu(&self, panel: u64) -> Option<crate::popups::Popup> {
        if self.open.is_some() || self.changes.open || self.on_changes {
            return None;
        }
        self.commit_menu(self.list.sel, self.menu_at, true, panel)
    }

    /// A file of the open commit (the right-click menu's).
    pub fn file(&self, i: usize) -> Option<&Change> {
        self.open.as_ref()?.files().get(i)
    }

    /// The file as it was in the open commit, in the whole panel.
    pub fn view_as_of(&mut self, file: usize) {
        if let Some(o) = self.open.as_mut().filter(|o| file < o.files().len()) {
            self.wrapped = None;
            o.diff = Some(Diff::new(file, true));
        }
    }

    /// The commits that changed one file, in place of the whole history.
    pub fn show_file_history(&mut self, path: String) {
        self.end_search();
        self.back_to = None;
        let whole = std::mem::replace(&mut self.list, List::new(Some(path)));
        if whole.path.is_none() {
            self.all = Some(whole);
        }
        self.open = None;
        self.want = None;
        self.reveal = true;
    }

    /// The whole history again, where it was left.
    pub fn show_all(&mut self) {
        self.end_search();
        if let Some(l) = self.all.take() {
            self.list = l;
            self.open = None;
            self.want = None;
            self.reveal = true;
        }
    }

    /// Whether a key is the history's: moving, opening and going back always, and `[`
    /// and `]` (previous and next file) once a commit is open.
    pub fn takes(&self, k: Key) -> bool {
        if self.changes.open {
            return self.changes.takes(k);
        }
        matches!(k, Key::ArrowUp | Key::ArrowDown | Key::PageUp | Key::PageDown | Key::Home | Key::End | Key::Enter | Key::Escape) || matches!(k, Key::OpenBracket | Key::CloseBracket) && self.open.is_some()
    }

    /// The work tree's changes are drawn as a row over the commits: not over one file's
    /// or a search's.
    fn changes_row(&self) -> bool {
        self.list.path.is_none() && self.list.query.is_none() && self.changes.any()
    }

    /// A key while the history has the keyboard; false when it should close.
    pub fn key(&mut self, k: Key, panel: u64, actions: &mut Vec<Action>) -> bool {
        if self.changes.open {
            self.keyed = true;
            self.changes.key(k, panel, actions);
            return true;
        }
        let step = |sel: &mut usize, n: usize| {
            let d: isize = match k {
                Key::ArrowDown => 1,
                Key::ArrowUp => -1,
                Key::PageDown => 10,
                Key::PageUp => -10,
                Key::Home => -(*sel as isize),
                Key::End => n as isize,
                _ => return false,
            };
            *sel = sel.saturating_add_signed(d).min(n.saturating_sub(1));
            true
        };
        self.keyed = true;
        let tree = self.tree;
        if k == Key::Escape {
            // egui lets go of the field on Escape before this runs: it had it last frame.
            let typing = self.search.as_ref().is_some_and(|s| s.find.focused);
            match &self.open {
                _ if typing => self.end_search(),
                Some(_) => self.back(),
                None if self.search.is_some() => self.end_search(),
                None if self.all.is_some() => self.show_all(),
                None => return false,
            }
            return true;
        }
        // The Changes row sits above the first commit, in the whole history only.
        let changes = self.changes_row();
        self.on_changes &= changes;
        match &mut self.open {
            None if k == Key::Enter && self.on_changes => self.show_changes(),
            None if k == Key::Enter => self.open_commit(self.list.sel),
            None if changes && (k == Key::Home || k == Key::ArrowUp && self.list.sel == 0 && !self.on_changes || k == Key::PageUp && self.list.sel < 10) => {
                self.list.sel = 0;
                self.on_changes = true;
                self.reveal = true;
            }
            None if self.on_changes && matches!(k, Key::ArrowDown | Key::PageDown | Key::End) => {
                self.on_changes = false;
                if k != Key::ArrowDown {
                    self.reveal |= step(&mut self.list.sel, self.list.commits.len());
                }
                self.reveal = true;
            }
            None if self.on_changes => {}
            None => self.reveal |= step(&mut self.list.sel, self.list.commits.len()),
            Some(o) if o.diff.is_some() => match k {
                Key::OpenBracket => o.step_file(-1, tree),
                Key::CloseBracket => o.step_file(1, tree),
                _ => {}
            },
            Some(o) => match k {
                Key::Enter => {
                    if let Some(x) = o.toggle(o.sel, tree) {
                        self.open_hash(x);
                    }
                }
                Key::OpenBracket => o.walk(-1),
                Key::CloseBracket => o.walk(1),
                _ => o.reveal |= step(&mut o.sel, o.items.len()),
            },
        }
        true
    }

    /// A diff with lines is open, in the panel or under a file: the Wrap button applies.
    fn wraps(&self) -> bool {
        if self.changes.open {
            return self.changes.wraps();
        }
        let Some(o) = &self.open else { return false };
        match &o.diff {
            Some(d) => matches!(d.text, Some(View::Lines(..))),
            None => o.inline.iter().any(|x| x.text.as_ref().is_some_and(|t| !t.0.is_empty())),
        }
    }
}

/// Lane colours: the theme's accent for the first lane, then quiet hues toned toward the
/// theme's own text colour so they sit on dark and light backgrounds alike. Hues close
/// to the accent are skipped, so the first two lanes never look alike.
pub fn lane_color(pal: &Palette, lane: usize) -> Color32 {
    const HUES: [u32; 8] = [0x7aa2f7, 0xd9a05b, 0xb58ee0, 0x62b58f, 0xe0849a, 0x58b4c9, 0xc9b36a, 0x9aa5b8];
    if lane == 0 {
        return pal.accent;
    }
    let hue = |c: Color32| egui::ecolor::Hsva::from(c).h;
    let a = hue(pal.accent);
    let hues: Vec<Color32> = HUES.iter().map(|&h| Color32::from_rgb((h >> 16) as u8, (h >> 8) as u8, h as u8)).filter(|&c| ((hue(c) - a + 1.5).rem_euclid(1.0) - 0.5).abs() > 0.07).collect();
    let c = hues[(lane - 1) % hues.len()];
    let (to, t) = if pal.dark { (pal.text_dim, 0.18) } else { (pal.text, 0.25) };
    c.lerp_to_gamma(to, t)
}

fn status_color(s: u8, pal: &Palette) -> Color32 {
    match s {
        b'A' => crate::gitstatus::GREEN,
        b'D' => pal.danger,
        b'R' | b'C' => crate::gitstatus::BLUE,
        _ => pal.warn,
    }
}

/// What a file's letter means, for its tooltip.
fn status_tip(f: &Change) -> String {
    match (f.status, &f.from) {
        (b'A', _) => "Added".into(),
        (b'D', _) => "Deleted".into(),
        (b'R', Some(from)) => format!("Renamed from {from}"),
        (b'C', Some(from)) => format!("Copied from {from}"),
        (b'T', _) => "Type changed (a file, a link or a submodule)".into(),
        _ => "Modified".into(),
    }
}

/// A commit's refs as pills: a local branch whose remote copy sits on the same commit
/// becomes one ("main · origin"), the remote's own pill dropped.
pub fn labels(refs: &[(RefKind, String)]) -> Vec<(RefKind, &str, Option<&str>)> {
    let local = |n: &str| refs.iter().any(|(k, b)| matches!(k, RefKind::Branch | RefKind::Head) && b == n);
    refs.iter()
        .filter_map(|(k, n)| match k {
            RefKind::Remote if n.split_once('/').is_some_and(|(_, b)| local(b)) => None,
            RefKind::Branch | RefKind::Head => {
                let remote = refs.iter().find_map(|(rk, rn)| (*rk == RefKind::Remote).then(|| rn.split_once('/')).flatten().filter(|(_, b)| b == n).map(|(r, _)| r));
                Some((*k, n.as_str(), remote))
            }
            _ => Some((*k, n.as_str(), None)),
        })
        .collect()
}

/// One ref as a small pill; a remote branch carries a cloud, a branch in step with its
/// remote names that remote after it. Returns its width.
#[allow(clippy::too_many_arguments)]
fn pill(ui: &Ui, p: &egui::Painter, at: Pos2, text: &str, remote: Option<&str>, font: &FontId, kind: RefKind, col: Color32, pal: &Palette, max: f32) -> f32 {
    let cloud = if kind == RefKind::Remote { font.size + 2.0 } else { 0.0 };
    let mut job = LayoutJob::default();
    job.append(text, 0.0, TextFormat { font_id: font.clone(), color: if kind == RefKind::Remote { pal.text_dim } else { pal.text }, ..Default::default() });
    if let Some(r) = remote {
        job.append(&format!(" · {r}"), 0.0, TextFormat { font_id: font.clone(), color: pal.text_dim, ..Default::default() });
    }
    job.wrap = TextWrapping { max_width: (max - 12.0 - cloud).max(8.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let g = ui.fonts_mut(|f| f.layout_job(job));
    let r = Rect::from_min_size(pos2(at.x, at.y - g.size().y / 2.0 - 1.0), g.size() + vec2(12.0 + cloud, 2.0));
    let rad = CornerRadius::same((r.height() / 2.0) as u8);
    match kind {
        RefKind::Tag => {
            p.rect_stroke(r, rad, Stroke::new(1.0, col.gamma_multiply(0.55)), egui::StrokeKind::Inside);
        }
        RefKind::Head => {
            p.rect_filled(r, rad, col.gamma_multiply(0.42));
        }
        RefKind::Branch => {
            p.rect_filled(r, rad, col.gamma_multiply(0.22));
        }
        RefKind::Remote => {
            p.rect_filled(r, rad, pal.text_dim.gamma_multiply(0.14));
            crate::theme::paint_glyph(p, Rect::from_center_size(pos2(r.left() + 5.0 + cloud / 2.0, r.center().y), Vec2::splat(font.size + 1.0)), Glyph::Cloud, pal.text_dim);
        }
    }
    let c = g.size();
    p.galley(pos2(r.left() + 6.0 + cloud, r.center().y - c.y / 2.0), g, pal.text);
    r.width()
}

/// An author's initials in a circle of their colour.
fn avatar(p: &egui::Painter, c: Pos2, d: f32, name: &str, col: Color32) {
    p.circle_filled(c, d / 2.0, col.gamma_multiply(0.24));
    p.text(c, Align2::CENTER_CENTER, initials(name), FontId::proportional((d * 0.4).round()), col);
}

/// The commit header's author circle and the graph rows' smaller one, across.
fn face_sizes(fs: f32) -> (f32, f32) {
    ((fs * 2.3).round(), fs.round())
}

/// Both sizes in pixels, which the pictures are cut to.
fn face_px(fs: f32, ppp: f32) -> [u32; 2] {
    let (big, small) = face_sizes(fs);
    [(big * ppp).round() as u32, (small * ppp).round() as u32]
}

/// An author's picture in place of their circle: drawn pixel for pixel when it was cut
/// to this size, with a faint ring so a white picture keeps its edge on a light theme.
fn face(p: &egui::Painter, c: Pos2, d: f32, tex: &egui::TextureHandle, ppp: f32, pal: &Palette) {
    let side = tex.size()[0] as f32 / ppp;
    let side = if (side - d).abs() <= 1.0 { side } else { d };
    let min = pos2(((c.x - side / 2.0) * ppp).round() / ppp, ((c.y - side / 2.0) * ppp).round() / ppp);
    let r = Rect::from_min_size(min, Vec2::splat(side));
    p.image(tex.id(), r, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
    p.circle_stroke(r.center(), side / 2.0, Stroke::new(1.0, pal.text.gamma_multiply(0.14)));
}

/// The history in place of the inspector's preview. False when it should close.
/// `wrap`: diffs wrap their long lines (the code previews' setting, shared).
pub fn view(ui: &mut Ui, rect: Rect, h: &mut History, wrap: &mut bool, env: &mut Env) -> bool {
    let pal = *env.pal;
    h.poll(ui.ctx());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, pal.bg);
    let hdr = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + 30.0));
    ui.painter().line_segment([hdr.left_bottom(), hdr.right_bottom()], Stroke::new(1.0, pal.border));
    let mut hui = ui.new_child(UiBuilder::new().max_rect(hdr.shrink2(vec2(6.0, 3.0))).layout(Layout::left_to_right(Align::Center)));
    hui.set_clip_rect(hdr.intersect(ui.clip_rect()));
    hui.spacing_mut().item_spacing.x = 2.0;
    let deep = h.open.is_some() || h.changes.open;
    // The find bar is only over the graph: nothing typed in it while it is away.
    if let Some(s) = h.search.as_mut().filter(|_| deep) {
        s.find.focused = false;
    }
    if crate::ui::glyph_button(&mut hui, Glyph::Close, if deep { "Close history" } else { "Close history (Esc)" }, true, &pal).clicked() {
        return false;
    }
    if deep && crate::ui::glyph_button(&mut hui, Glyph::Back, "Back (Esc)", true, &pal).clicked() {
        if h.changes.open {
            h.changes.open = false;
        } else {
            h.back();
        }
    }
    // Up and down walk the commits from a commit, and the files from a file.
    let (full, n) = (h.open.as_ref().is_some_and(|o| o.diff.is_some()), h.list.commits.len());
    // A comparison has no newer or older to walk to, only its files.
    if let Some(o) = h.open.as_ref().filter(|o| !h.changes.open && (full || o.cmp.is_none())) {
        let (up, down) = if full {
            let order = o.order(h.tree);
            let k = o.diff.as_ref().and_then(|d| order.iter().position(|&f| f == d.file)).unwrap_or(0);
            (k > 0, k + 1 < order.len())
        } else {
            (o.at > 0, o.at + 1 < n || !h.list.done)
        };
        let (tu, td) = if full { ("Previous file ([)", "Next file (])") } else { ("Newer commit", "Older commit") };
        let (nu, nd) = if full { ("previous file", "next file") } else { ("newer commit", "older commit") };
        hui.add_space(2.0);
        let a = crate::ui::glyph_button_as(&mut hui, Glyph::ChevronUp, tu, up, &pal, nu).clicked();
        let b = crate::ui::glyph_button_as(&mut hui, Glyph::ChevronDown, td, down, &pal, nd).clicked();
        let tree = h.tree;
        match (a, b, full) {
            (true, _, true) => h.open.as_mut().into_iter().for_each(|o| o.step_file(-1, tree)),
            (_, true, true) => h.open.as_mut().into_iter().for_each(|o| o.step_file(1, tree)),
            (true, _, false) => h.step_commit(-1),
            (_, true, false) => h.step_commit(1),
            _ => {}
        }
    }
    // The graph is searched as a preview's text is, from the same button.
    if !deep {
        hui.add_space(4.0);
        crate::inspector::seg_frame(&mut hui, &pal, |ui| {
            if crate::inspector::seg_button(ui, h.search.is_some(), "Find (Ctrl+F)", &pal, |p, r, c| crate::theme::paint_glyph(p, r, Glyph::Search, c)) {
                if h.search.is_some() {
                    h.end_search();
                } else {
                    h.find();
                }
            }
        });
    }
    // A diff wraps like a code preview, from the same button; a file's own diff can also
    // be shown side by side.
    if h.wraps() {
        hui.add_space(4.0);
        let side = env.cfg.diff_side;
        let whole = h.open.as_ref().is_some_and(|o| o.diff.is_some());
        crate::inspector::seg_frame(&mut hui, &pal, |ui| {
            if whole && crate::filediff::diff_layout(ui, side, &pal) {
                env.actions.push(crate::app::Action::Run(Box::new(crate::filediff::toggle_side)));
            }
            if !(whole && side) && crate::inspector::seg_button(ui, *wrap, "Wrap lines", &pal, |p, r, c| crate::theme::paint_glyph(p, r, Glyph::Wrap, c)) {
                *wrap = !*wrap;
            }
        });
    }
    // What is shown, as the same tinted pill the preview puts its file name in. A
    // commit's short hash copies the whole one.
    let (icon, label, hash) = match &h.open {
        _ if h.changes.open => ("git-compare", "Changes".to_string(), None),
        None => ("git-branch", h.branch.clone(), None),
        Some(o) => match (&o.diff, &o.info, &o.cmp) {
            (Some(d), Some(i), _) => (if d.blob { "file-text" } else { "git-compare" }, i.1[d.file].path.rsplit('/').next().unwrap_or("").to_string(), None),
            (_, _, Some(c)) => ("git-compare", format!("{} → {}", c.names[0], c.names[1]), None),
            _ => ("", o.hash[..o.hash.len().min(7)].to_string(), Some(o.hash.clone())),
        },
    };
    hui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let iw = if icon.is_empty() { 0.0 } else { 16.0 };
        let room = ui.available_width() - 6.0 - iw - 20.0;
        if room < 24.0 || label.is_empty() {
            return;
        }
        let font = if icon.is_empty() { FontId::monospace(11.5) } else { FontId::proportional(12.0) };
        let g = crate::listing::elided(ui, &label, font, room.min(260.0), pal.text);
        let (r, resp) = ui.allocate_exact_size(vec2(g.size().x + iw + 20.0, crate::kit::CHIP_H), if hash.is_some() { Sense::click() } else { Sense::hover() });
        let lit = hash.is_some() && resp.hovered();
        // A chip as the status bar's: what this view is of.
        crate::kit::chip_face(ui.painter(), r, lit, false, None, crate::kit::round(), &pal);
        crate::theme::paint_symbol(ui.painter(), Rect::from_center_size(pos2(r.left() + 16.0, r.center().y), Vec2::splat(12.0)), icon, pal.text);
        ui.painter().galley(pos2(r.left() + 10.0 + iw, r.center().y - g.size().y / 2.0), g, pal.text);
        if let Some(full) = hash {
            crate::ui::note_control(ui, r, "copy hash");
            if resp.on_hover_text("Copy the full hash").clicked() {
                ui.ctx().copy_text(full.clone());
                env.actions.push(Action::Toast(format!("Copied {full}")));
            }
        }
    });
    let body = Rect::from_min_max(pos2(rect.left(), hdr.bottom() + 1.0), rect.max);
    let mut bui = ui.new_child(UiBuilder::new().max_rect(body).id_salt(("git", env.panel_id)));
    bui.set_clip_rect(body.intersect(ui.clip_rect()));
    if h.changes.open {
        crate::gitchanges::page(&mut bui, body, &mut h.changes, *wrap, env);
        return true;
    }
    match h.open.as_ref().map(|o| o.diff.is_some()) {
        None => {
            let body = search_bar(&mut bui, body, h, env);
            graph(&mut bui, body, h, env)
        }
        Some(false) => commit(&mut bui, body, h, *wrap, env),
        Some(true) => whole(&mut bui, body, h, *wrap, env),
    }
    true
}

/// The find bar along the graph's foot while searching, Maia's find bar with its keys:
/// Enter and Shift+Enter (or F3) step through the commits found, Down goes into them,
/// Escape closes it and puts the whole history back. Returns the room left above it.
fn search_bar(ui: &mut Ui, body: Rect, h: &mut History, env: &Env) -> Rect {
    let Some(s) = h.search.as_mut() else { return body };
    let id = env.panel_id;
    let fid = egui::Id::new(("insp-find", id));
    // Down from the field: the keyboard goes to the commits.
    let down = s.find.focused && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::ArrowDown));
    let l = &h.list;
    let n = l.commits.len();
    let more = if l.done { "" } else { "+" };
    let count = match n {
        _ if s.find.query.trim().is_empty() || l.query.is_none() => String::new(),
        0 if l.done => "No matches".into(),
        0 => String::new(),
        1 if l.done => "1 commit".into(),
        n => format!("{}{more} commits", crate::table::fmt_count(n)),
    };
    let (out, rest) = crate::findbar::bar(ui, body, &mut s.find, &count, n > 0 && l.query.is_some(), id, env.cfg.animations, env.pal);
    if std::mem::take(&mut s.caret_end) {
        if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), fid) {
            let end = egui::text::CCursor::new(s.find.query.chars().count());
            st.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
            st.store(ui.ctx(), fid);
        }
    }
    let step = |h: &mut History, d: isize| {
        let n = h.list.commits.len();
        let Some(s) = h.search.as_mut().filter(|_| n > 0) else { return };
        h.list.sel = if s.stepped { (h.list.sel as isize + d).rem_euclid(n as isize) as usize } else { h.list.sel.min(n - 1) };
        s.stepped = true;
        (h.keyed, h.reveal, h.on_changes) = (true, true, false);
    };
    match out {
        crate::findbar::Out::Next => step(h, 1),
        crate::findbar::Out::Prev => step(h, -1),
        crate::findbar::Out::Close => h.end_search(),
        _ => {}
    }
    if down {
        ui.memory_mut(|m| m.surrender_focus(fid));
        if let Some(s) = h.search.as_mut() {
            s.find.focused = false;
        }
        step(h, 0);
    }
    rest
}

fn dim(ui: &Ui, at: Rect, msg: &str, fs: f32, pal: &Palette) {
    ui.painter().text(at.center(), Align2::CENTER_CENTER, msg, FontId::proportional(fs), pal.text_dim);
}

fn graph(ui: &mut Ui, body: Rect, h: &mut History, env: &mut Env) {
    let pal = *env.pal;
    let fs = env.cfg.font_size;
    // One file's history: what it is, and the way back to every commit.
    let mut body = body;
    if let Some(path) = h.list.path.clone() {
        let bar = Rect::from_min_size(body.min, vec2(body.width(), (fs + 14.0).round()));
        body.min.y = bar.bottom();
        let p = ui.painter();
        p.rect_filled(bar, CornerRadius::ZERO, pal.alt);
        p.line_segment([bar.left_bottom(), bar.right_bottom()], Stroke::new(1.0, pal.border));
        crate::theme::paint_symbol(p, Rect::from_center_size(pos2(bar.left() + 16.0, bar.center().y), Vec2::splat(12.0)), "file-text", pal.text_dim);
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", &path));
        let end = bar.right() - 38.0;
        let g = crate::listing::elided(ui, name, FontId::proportional(fs - 0.5), end - bar.left() - 30.0, pal.text);
        let dx = bar.left() + 30.0 + g.size().x + 10.0;
        p.galley(pos2(bar.left() + 30.0, bar.center().y - g.size().y / 2.0), g, pal.text);
        if !dir.is_empty() && end - dx > 30.0 {
            let g = crate::listing::elided(ui, dir, FontId::proportional(fs - 2.0), end - dx, pal.text_dim);
            p.galley(pos2(dx, bar.center().y - g.size().y / 2.0), g, pal.text_dim);
        }
        let mut cui = ui.new_child(UiBuilder::new().max_rect(Rect::from_min_max(pos2(bar.right() - 32.0, bar.top()), bar.max)).layout(Layout::left_to_right(Align::Center)));
        if crate::ui::glyph_button_as(&mut cui, Glyph::Close, "Show all commits (Esc)", true, &pal, "show all commits").clicked() {
            h.show_all();
            return;
        }
    }
    // The work tree's changes come first, as a commit still to be made.
    let off = usize::from(h.changes_row());
    if h.list.commits.is_empty() && off == 0 {
        let msg = match (h.list.done, h.list.path.is_some(), h.list.query.is_some()) {
            (false, _, true) => "Searching…",
            (false, ..) => "Loading…",
            (true, _, true) => "No commits match",
            (true, true, _) => "No commits changed this file",
            (true, false, _) => "No commits yet",
        };
        dim(ui, body, msg, fs, &pal);
        return;
    }
    let (font, small, tag) = (FontId::proportional(fs), FontId::proportional(fs - 1.5), FontId::proportional((fs - 2.5).max(8.5)));
    let row_h = ((fs * 1.35 + (fs - 1.5) * 1.35) + 7.0 * env.cfg.spacing).round();
    let step = (fs + 1.0).round();
    let dot = (step * 0.27).max(3.0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let l = &mut h.list;
    let n = off + l.commits.len() + usize::from(!l.done);
    let anim = env.cfg.animations;
    let on_changes = h.on_changes && off > 0;
    let changes_line = if off > 0 { h.changes.summary_line() } else { String::new() };
    let mut open_changes = false;
    // With pictures on, every author gets a picture-sized place, so the names don't shift
    // as pictures arrive.
    let pictures = env.cfg.github_pictures;
    let avatars = &mut *env.avatars;
    let (ppp, fd) = (ui.ctx().pixels_per_point(), face_sizes(fs).1);
    let fpx = face_px(fs, ppp);
    let ctx = ui.ctx().clone();
    let root = &h.root;
    let (mut open, mut menu) = (None, None);
    let mark = h.mark.as_deref();
    let hit_bg = pal.accent.gamma_multiply(0.3);
    h.menu_at = body.center();
    let mut inner = ui.new_child(UiBuilder::new().max_rect(body.shrink2(vec2(4.0, 4.0))));
    // A scroll area is at least 64 points tall: in a short panel the rows would run on
    // under the find bar below.
    inner.set_clip_rect(body.intersect(ui.clip_rect()));
    ScrollArea::vertical().auto_shrink([false, false]).id_salt(("git_graph", l.path.is_some(), l.query.is_some())).show_viewport(&mut inner, |ui, vp| {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), n as f32 * row_h), Sense::hover());
        if std::mem::take(&mut h.reveal) {
            let at = if on_changes { 0 } else { l.sel + off };
            ui.scroll_to_rect(Rect::from_min_size(pos2(r.left(), r.top() + at as f32 * row_h), vec2(r.width(), row_h)), None);
        }
        let first = (vp.min.y / row_h).floor().max(0.0) as usize;
        let last = ((vp.max.y / row_h).ceil() as usize + 1).min(n);
        if last + 100 > l.commits.len() + off {
            l.ask();
        }
        let gx = r.left() + 8.0 + step / 2.0;
        let x = |l: u16| gx + (l as usize).min(LANES) as f32 * step;
        // The text starts after the widest row on screen, easing there as rows scroll by,
        // so a busy stretch far down never pushes the quiet ones aside.
        let wide = l.rows[first.saturating_sub(off).min(l.rows.len())..last.saturating_sub(off).min(l.rows.len())].iter().flat_map(|r| r.pass.iter().chain(&r.outs).chain(&r.ins).chain([&r.col])).max().map_or(1, |&l| (l as usize + 1).min(LANES + 1));
        let wide = ui.ctx().animate_value_with_time(ui.id().with("gitw"), wide as f32, if anim { 0.18 } else { 0.0 });
        let tx = gx + (wide - 1.0) * step + step / 2.0 + 8.0;
        let fold = pal.text_dim.gamma_multiply(0.4);
        let colors: Vec<Color32> = (0..=LANES).map(|l| if l < LANES { lane_color(&pal, l) } else { fold }).collect();
        let col = |l: u16| colors[(l as usize).min(LANES)];
        let line = |l: u16| Stroke::new(1.6, col(l));
        let p = ui.painter().clone();
        let curve = |a: Pos2, b: Pos2, s: Stroke| {
            let k = (b.y - a.y) * 0.6;
            p.add(CubicBezierShape::from_points_stroke([a, pos2(a.x, a.y + k), pos2(b.x, b.y - k), b], false, Color32::TRANSPARENT, s));
        };
        for i in first..last {
            let rr = Rect::from_min_size(pos2(r.left(), r.top() + i as f32 * row_h), vec2(r.width(), row_h));
            if i < off {
                // A hollow ring in the first lane, then what is changed.
                let resp = ui.interact(rr, ui.id().with("gitchanges"), Sense::click());
                crate::ui::note_control(ui, rr, "changes row");
                let under = if on_changes && h.keyed { pal.row_hover } else if resp.hovered() { pal.hover } else { pal.bg };
                if under != pal.bg {
                    p.rect_filled(rr.shrink2(vec2(2.0, 1.0)), CornerRadius::same(5), under);
                }
                if resp.clicked() {
                    open_changes = true;
                }
                let c = pos2(x(0), rr.center().y);
                p.circle_stroke(c, dot, Stroke::new(1.6, pal.accent));
                // A dotted line down to the commits below.
                let mut yy = c.y + dot + 3.0;
                while yy < rr.bottom() {
                    p.line_segment([pos2(c.x, yy), pos2(c.x, (yy + 2.0).min(rr.bottom()))], Stroke::new(1.6, pal.accent.gamma_multiply(0.6)));
                    yy += 5.0;
                }
                let w = rr.right() - 8.0 - tx;
                if w >= 16.0 {
                    let g = crate::listing::elided(ui, "Changes", font.clone(), w, pal.text);
                    let m = small.size * 1.35;
                    let y0 = rr.center().y - (g.size().y + m) / 2.0;
                    let gh = g.size().y;
                    p.galley(pos2(tx, y0), g, pal.text);
                    let ag = crate::listing::elided(ui, &changes_line, small.clone(), w, pal.text_dim);
                    p.galley(pos2(tx, y0 + gh + 1.0 + (small.size * 1.3 - ag.size().y) / 2.0), ag, pal.text_dim);
                }
                continue;
            }
            let i = i - off;
            if i >= l.commits.len() {
                crate::ui::spinner(&p, pos2(tx + 6.0, rr.center().y), 5.0, pal.accent, ui.input(|i| i.time));
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
                continue;
            }
            let resp = ui.interact(rr, ui.id().with(("gitrow", i)), Sense::click());
            let under = if i == l.sel && h.keyed && !on_changes { pal.row_hover } else if resp.hovered() { pal.hover } else { pal.bg };
            if under != pal.bg {
                p.rect_filled(rr.shrink2(vec2(2.0, 1.0)), CornerRadius::same(5), under);
            }
            if resp.clicked() {
                open = Some(i);
            }
            if resp.secondary_clicked() {
                menu = Some((i, ui.ctx().pointer_interact_pos().unwrap_or(rr.center())));
            }
            if i == l.sel {
                h.menu_at = pos2(tx, rr.bottom());
            }
            let (c, row) = (&l.commits[i], &l.rows[i]);
            let (top, cy, bot) = (rr.top(), rr.center().y, rr.bottom());
            let mut folded = false;
            for &l in &row.pass {
                if (l as usize) < LANES || !std::mem::replace(&mut folded, true) {
                    p.line_segment([pos2(x(l), top), pos2(x(l), bot)], line(l));
                }
            }
            let cx = x(row.col);
            if row.up {
                p.line_segment([pos2(cx, top), pos2(cx, cy)], line(row.col));
            }
            for &l in &row.ins {
                curve(pos2(x(l), top), pos2(cx, cy), line(l));
            }
            for &l in &row.outs {
                if l == row.col {
                    p.line_segment([pos2(cx, cy), pos2(cx, bot)], line(l));
                } else {
                    curve(pos2(cx, cy), pos2(x(l), bot), line(l));
                }
            }
            // The dot sits in a ring of the row's own background, so lines stop short of it.
            let dc = col(row.col);
            let head = c.refs.iter().any(|r| r.0 == RefKind::Head);
            p.circle_filled(pos2(cx, cy), dot + 1.8, under);
            if c.parents.len() > 1 {
                p.circle_stroke(pos2(cx, cy), dot - 0.6, Stroke::new(1.6, dc));
            } else {
                p.circle_filled(pos2(cx, cy), dot, dc);
            }
            if head {
                p.circle_stroke(pos2(cx, cy), dot + 3.2, Stroke::new(1.2, dc.gamma_multiply(0.7)));
            }
            // Subject line: ref pills first (as many as fit in half the row), then the subject.
            let w = rr.right() - 8.0 - tx;
            if w < 16.0 {
                continue;
            }
            let g = crate::listing::elided(ui, &c.subject, font.clone(), w, pal.text);
            let m = small.size * 1.35;
            let y0 = cy - (g.size().y + m) / 2.0;
            let mut px = tx;
            let labels = labels(&c.refs);
            for (k, (kind, name, remote)) in labels.iter().enumerate() {
                let room = tx + w * 0.5 - px;
                if k > 0 && name.len() as f32 * tag.size * 0.6 + 16.0 > room || room < 40.0 {
                    let more = format!("+{}", labels.len() - k);
                    let mg = p.layout_no_wrap(more, tag.clone(), pal.text_dim);
                    p.galley(pos2(px, y0 + g.size().y / 2.0 - mg.size().y / 2.0), mg.clone(), pal.text_dim);
                    px += mg.size().x + 6.0;
                    break;
                }
                px += pill(ui, &p, pos2(px, y0 + g.size().y / 2.0), name, *remote, &tag, *kind, dc, &pal, room) + 4.0;
            }
            let g = match l.query.as_deref() {
                // What was searched for, marked in the subject.
                Some(q) => marked(ui, &c.subject, &crate::gitfind::spans(q, &c.subject, false), font.clone(), tx + w - px - 2.0, pal.text, hit_bg),
                None if px > tx => crate::listing::elided(ui, &c.subject, font.clone(), tx + w - px - 2.0, pal.text),
                None => g,
            };
            p.galley(pos2(px + if px > tx { 2.0 } else { 0.0 }, y0), g.clone(), pal.text);
            // Meta line: short hash, the author after a dot of their colour, when.
            let my = y0 + g.size().y + 1.0;
            let hf = FontId::monospace(small.size - 0.5);
            let hl = l.query.as_deref().map_or(0, |q| crate::gitfind::hash_hit(q, &c.hash)).min(7);
            let hg = if hl > 0 { marked(ui, &c.hash[..7], &[(0, hl)], hf, f32::INFINITY, pal.text_dim, hit_bg) } else { p.layout_no_wrap(c.hash[..7].to_string(), hf, pal.text_dim.gamma_multiply(0.8)) };
            let hw = hg.size().x;
            let mh = small.size * 1.3;
            let hr = Rect::from_min_size(pos2(tx, my + (mh - hg.size().y) / 2.0), hg.size());
            // The commit marked to compare with: its hash in a pill of the accent.
            if mark == Some(c.hash.as_str()) {
                p.rect_filled(hr.expand2(vec2(4.0, 1.0)), CornerRadius::same(4), pal.accent.gamma_multiply(0.3));
                ui.interact(hr, ui.id().with(("gitmark", i)), Sense::hover()).on_hover_text("Marked to compare: another commit's menu compares with it");
            }
            p.galley(hr.min, hg, pal.text_dim);
            let ax = tx + hw + 10.0;
            if tx + w - ax > 30.0 {
                let (dc, nx) = if pictures { (pos2(ax + fd / 2.0, my + mh / 2.0), ax + fd + 5.0) } else { (pos2(ax + 3.0, my + mh / 2.0), ax + 10.0) };
                match pictures.then(|| avatars.picture(&ctx, &c.email, &c.hash, root, fpx)).flatten() {
                    Some(pic) => face(&p, dc, fd, &pic.small, ppp, &pal),
                    None => {
                        p.circle_filled(dc, 3.0, author_color(&pal, &c.email, &c.author));
                    }
                }
                let who = format!("{}  ·  {}", c.author, ago(c.time, now));
                let ag = match l.query.as_deref() {
                    Some(q) => marked(ui, &who, &crate::gitfind::spans(q, &c.author, true), small.clone(), tx + w - nx, pal.text_dim, hit_bg),
                    None => crate::listing::elided(ui, &who, small.clone(), tx + w - nx, pal.text_dim),
                };
                p.galley(pos2(nx, my + (mh - ag.size().y) / 2.0), ag, pal.text_dim);
            }
        }
    });
    if let Some(i) = open {
        h.on_changes = false;
        h.open_commit(i);
    }
    // The row stays marked while its menu is open.
    if let Some((i, pos)) = menu {
        (h.list.sel, h.keyed, h.on_changes) = (i, true, false);
        env.actions.extend(h.commit_menu(i, pos, false, env.panel_id).map(Action::Open));
    }
    if open_changes {
        h.on_changes = true;
        h.show_changes();
    }
}

/// `text` on one line, cut with "…" at `width`, its `spans` (byte ranges) on a tint.
#[allow(clippy::too_many_arguments)]
fn marked(ui: &Ui, text: &str, spans: &[(usize, usize)], font: FontId, width: f32, color: Color32, bg: Color32) -> Arc<egui::Galley> {
    let mut job = LayoutJob::default();
    let f = |bg: Color32| TextFormat { font_id: font.clone(), color, background: bg, ..Default::default() };
    let mut at = 0;
    for &(a, b) in spans {
        let (a, b) = (a.max(at).min(text.len()), b.min(text.len()));
        if a >= b || !text.is_char_boundary(a) || !text.is_char_boundary(b) {
            continue;
        }
        job.append(&text[at..a], 0.0, f(Color32::TRANSPARENT));
        job.append(&text[a..b], 0.0, f(bg));
        at = b;
    }
    job.append(&text[at..], 0.0, f(Color32::TRANSPARENT));
    job.wrap = TextWrapping { max_width: width.max(8.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    ui.fonts_mut(|f| f.layout_job(job))
}

/// What a click in the file list does, once the list is drawn.
enum Act {
    Hash(String),
    Toggle(usize),
    /// A file in the whole panel; true when a double click got there.
    Large(usize, bool),
    Menu(usize, Pos2),
    Tree(bool),
    /// A comparison's files since the split, or between the two directly.
    Split(bool),
}

/// Over the files: how many and their lines, and the list or folder switch; or why there
/// are none to list.
#[allow(clippy::too_many_arguments)]
fn files_summary(ui: &mut Ui, r: Rect, n: usize, totals: (u64, u64), toggle: bool, tree: bool, none: Option<&str>, fs: f32, pal: &Palette) -> Option<Act> {
    let mono = FontId::monospace((fs - 2.5).max(8.0));
    let small = FontId::proportional(fs - 1.5);
    if let Some(msg) = none {
        let g = crate::listing::elided(ui, msg, FontId::proportional(fs - 0.5), r.width() - 16.0, pal.text_dim);
        ui.painter().galley(pos2(r.left() + 8.0, r.center().y - g.size().y / 2.0), g, pal.text_dim);
        return None;
    }
    let end = r.right() - 8.0 - if toggle { 64.0 } else { 0.0 };
    let mut x = r.left() + 8.0;
    let parts = [(summary(n), &small, pal.text_dim, true), (format!("+{}", totals.0), &mono, crate::gitstatus::GREEN, totals.0 > 0), (format!("−{}", totals.1), &mono, pal.danger, totals.1 > 0)];
    for (k, (t, f, c, shown)) in parts.into_iter().enumerate() {
        if !shown || end - x < 12.0 {
            continue;
        }
        // The words may end in "…"; a count shows whole or not at all (a short "−9"
        // was left out with room to spare).
        let g = if k == 0 { crate::listing::elided(ui, &t, f.clone(), end - x, c) } else { ui.painter().layout_no_wrap(t, f.clone(), c) };
        if k > 0 && g.size().x > end - x {
            break;
        }
        let w = g.size().x;
        ui.painter().galley(pos2(x, r.center().y - g.size().y / 2.0), g, c);
        x += w + if k == 0 { 10.0 } else { 6.0 };
    }
    let mut act = None;
    if toggle {
        let mut tui = ui.new_child(UiBuilder::new().max_rect(Rect::from_min_max(pos2(r.right() - 62.0, r.top()), r.max)).layout(Layout::left_to_right(Align::Center)));
        let items = [(Glyph::List, "Show as a list", "Show as a list"), (Glyph::ListTree, "Group by folder", "Group by folder")];
        if let Some(k) = crate::kit::glyph_choice(&mut tui, egui::Id::new("commit files as"), &items, tree as usize, crate::kit::round(), pal) {
            act = Some(Act::Tree(k == 1));
        }
    }
    act
}

/// A short hash in the accent, underlined on hover: a link to its commit.
fn hash_link(ui: &mut Ui, hash: &str, tip: &str, what: &'static str, fs: f32, pal: &Palette) -> bool {
    let g = ui.painter().layout_no_wrap(hash[..hash.len().min(7)].to_string(), FontId::monospace(fs - 2.5), pal.accent);
    let (r, resp) = ui.allocate_exact_size(g.size() + vec2(0.0, 2.0), Sense::click());
    crate::ui::note_control(ui, r, what);
    if resp.hovered() {
        ui.painter().line_segment([pos2(r.left(), r.bottom() - 0.5), pos2(r.right(), r.bottom() - 0.5)], Stroke::new(1.0, pal.accent));
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    ui.painter().galley(r.min + vec2(0.0, 1.0), g, pal.accent);
    resp.on_hover_text(tip).clicked()
}

/// Two words as one switch, in the look of the toolbar's segments; the one clicked.
fn text_seg(ui: &mut Ui, labels: [&str; 2], on: usize, tips: [&str; 2], what: [&'static str; 2], pal: &Palette) -> Option<usize> {
    let font = FontId::proportional(11.5);
    let room = ((ui.available_width() - 4.0) / 2.0 - 16.0).max(16.0);
    let gs = labels.map(|t| crate::listing::elided(ui, t, font.clone(), room, pal.text));
    let w: f32 = gs.iter().map(|g| g.size().x + 16.0).sum::<f32>() + 4.0;
    let (r, _) = ui.allocate_exact_size(vec2(w, crate::kit::ROW_CAP_H), Sense::hover());
    let round = crate::kit::round();
    crate::kit::capsule(ui.painter(), r, round, pal);
    let (mut x, mut pick) = (r.left() + crate::kit::INSET, None);
    let cells: Vec<Rect> = gs
        .iter()
        .map(|g| {
            let br = Rect::from_min_size(pos2(x, r.top() + crate::kit::INSET), vec2(g.size().x + 16.0, crate::kit::ROW_ITEM.y));
            x = br.right();
            br
        })
        .collect();
    // The lifted pill slides to the side picked, as a dialog's choices do.
    let id = ui.scope_id().with(what);
    let l = crate::kit::slide(ui.ctx(), id.with("l"), cells[on.min(1)].left() - r.left()) + r.left();
    let pw = crate::kit::slide(ui.ctx(), id.with("w"), cells[on.min(1)].width());
    crate::kit::item_face(ui.painter(), Rect::from_min_size(pos2(l, r.top() + crate::kit::INSET), vec2(pw, crate::kit::ROW_ITEM.y)), false, true, round, pal);
    for (i, g) in gs.into_iter().enumerate() {
        let br = cells[i];
        let resp = ui.interact(br, ui.id().with(what[i]), Sense::click());
        crate::ui::note_control(ui, br, what[i]);
        if i != on && resp.hovered() {
            crate::kit::item_face(ui.painter(), br, true, false, round, pal);
        }
        let col = crate::kit::item_ink(true, resp.hovered(), i == on, pal);
        ui.painter().galley_with_override_text_color(br.center() - g.size() / 2.0, g, col);
        if resp.on_hover_text(tips[i]).clicked() && i != on {
            pick = Some(i);
        }
    }
    pick
}

/// A comparison's head: the new side over the old, where the two split (a link), and the
/// switch between what the new side adds and every difference between the two.
fn cmp_header(ui: &mut Ui, c: &Cmp, fs: f32, pal: &Palette, act: &mut Option<Act>) {
    let [old, new] = [&c.names[0], &c.names[1]];
    ui.add(egui::Label::new(egui::RichText::new(new).size(fs + 1.0).color(pal.text)).wrap());
    ui.add(egui::Label::new(egui::RichText::new(format!("compared with {old}")).size(fs - 1.0).color(pal.text_dim)).wrap());
    let s = match &c.data {
        Some(Ok(s)) => s,
        Some(Err(e)) => {
            ui.add_space(4.0);
            ui.add(egui::Label::new(egui::RichText::new(e).size(fs - 0.5).color(pal.danger)).wrap());
            return;
        }
        None => return,
    };
    ui.add_space(2.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        match &s.base {
            Some(b) => {
                ui.label(egui::RichText::new("Split at").size(fs - 2.0).color(pal.text_dim));
                if hash_link(ui, b, "Open the commit both come from", "split link", fs, pal) {
                    *act = Some(Act::Hash(b.clone()));
                }
            }
            None => {
                ui.label(egui::RichText::new("No history in common").size(fs - 2.0).color(pal.text_dim));
            }
        }
    });
    // On one line of history, both ways show the same.
    if s.base.as_ref().is_some_and(|b| *b != s.hashes[0]) {
        ui.add_space(2.0);
        let labels = [format!("What {new} adds"), "Every difference".to_string()];
        let tips = [format!("The changes on {new} since it split from {old}"), format!("Everything that differs between the two, the changes on {old} included")];
        if let Some(i) = text_seg(ui, [&labels[0], &labels[1]], usize::from(!c.split), [&tips[0], &tips[1]], ["what it adds", "every difference"], pal) {
            *act = Some(Act::Split(i == 0));
        }
    }
}

fn commit(ui: &mut Ui, body: Rect, h: &mut History, wrap: bool, env: &mut Env) {
    let pal = *env.pal;
    let (fs, sp) = (env.cfg.font_size, env.cfg.spacing);
    let (keyed, tree) = (h.keyed, h.tree);
    let root = h.root.clone();
    let Some(o) = h.open.as_mut() else { return };
    let commits = &h.list.commits;
    // A comparison has no commit of its own in the list.
    let c = commits.get(o.at);
    let lane = h.list.rows.get(o.at).map_or(pal.accent, |r| lane_color(&pal, r.col as usize));
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(c.map_or(0, |c| c.time).max(0) as u64);
    let mut inner = ui.new_child(UiBuilder::new().max_rect(body.shrink2(vec2(6.0, 4.0))));
    let mut act = None;
    let reveal = std::mem::take(&mut o.reveal);
    let ctx = ui.ctx().clone();
    let pictures = env.cfg.github_pictures;
    let avatars = &mut *env.avatars;
    let salt = o.cmp.as_ref().map_or_else(|| o.hash.clone(), |c| format!("{}...{}", c.revs[0], c.revs[1]));
    ScrollArea::vertical().auto_shrink([false, false]).id_salt(("git_commit", salt.as_str())).show(&mut inner, |ui| {
        ui.add_space(6.0);
        egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 0)).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            if let Some(cm) = o.cmp.as_deref() {
                return cmp_header(ui, cm, fs, &pal, &mut act);
            }
            let Some(c) = c else { return };
            ui.add(egui::Label::new(egui::RichText::new(&c.subject).size(fs + 1.0).color(pal.text)).wrap());
            if !c.refs.is_empty() {
                let tag = FontId::proportional((fs - 2.5).max(8.5));
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), tag.size + 8.0), Sense::hover());
                let mut px = r.left();
                for (kind, name, remote) in labels(&c.refs) {
                    if r.right() - px < 40.0 {
                        break;
                    }
                    px += pill(ui, ui.painter(), pos2(px, r.center().y), name, remote, &tag, kind, lane, &pal, r.right() - px) + 4.0;
                }
            }
            // The author: their GitHub picture when that is on and found, else initials in
            // their colour; then their name over when.
            ui.add_space(2.0);
            let d = face_sizes(fs).0;
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), d), Sense::hover());
            let at = pos2(r.left() + d / 2.0, r.center().y);
            let ppp = ctx.pixels_per_point();
            match pictures.then(|| avatars.picture(&ctx, &c.email, &c.hash, &root, face_px(fs, ppp))).flatten() {
                Some(pic) => face(ui.painter(), at, d, &pic.big, ppp, &pal),
                None => avatar(ui.painter(), at, d, &c.author, author_color(&pal, &c.email, &c.author)),
            }
            let (x, w) = (r.left() + d + 9.0, r.right() - r.left() - d - 9.0);
            if w > 20.0 {
                let a = crate::listing::elided(ui, &c.author, FontId::proportional(fs - 0.5), w, pal.text);
                let t = crate::listing::elided(ui, &format!("{}  ·  {}", ago(c.time, now), crate::fsx::fmt_time(Some(when))), FontId::proportional(fs - 2.0), w, pal.text_dim);
                let y = r.center().y - (a.size().y + t.size().y) / 2.0;
                let ah = a.size().y;
                ui.painter().galley(pos2(x, y), a, pal.text);
                ui.painter().galley(pos2(x, y + ah), t, pal.text_dim);
            }
            if let Some((msg, _)) = o.info.as_ref().filter(|i| !i.0.is_empty()) {
                ui.add_space(2.0);
                ui.add(egui::Label::new(egui::RichText::new(msg).size(fs - 0.5).color(pal.text_dim)).wrap());
            }
            // Parents, each a link to its commit.
            if !c.parents.is_empty() {
                ui.add_space(2.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.label(egui::RichText::new(if c.parents.len() > 1 { "Parents" } else { "Parent" }).size(fs - 2.0).color(pal.text_dim));
                    for p in &c.parents {
                        let tip = commits.iter().find(|x| x.hash == *p).map_or_else(|| "Open this commit".to_string(), |x| x.subject.clone());
                        if hash_link(ui, p, &tip, "parent link", fs, &pal) {
                            act = Some(Act::Hash(p.clone()));
                        }
                    }
                });
            }
        });
        ui.add_space(8.0);
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
        ui.painter().line_segment([r.left_center(), r.right_center()], Stroke::new(1.0, pal.border));
        let cmp = o.cmp.is_some();
        let none = match &o.info {
            Some((_, f)) if !f.is_empty() => None,
            Some(_) => Some(if cmp { "No files differ" } else { "No files changed" }),
            None => Some("Loading…"),
        };
        // A comparison git couldn't read says why in its head, and has nothing more.
        if cmp && o.cmp.as_ref().is_some_and(|c| matches!(c.data, Some(Err(_)))) {
            return;
        }
        if let (Some(msg), false) = (none, cmp) {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(msg).size(fs - 0.5).color(pal.text_dim));
            });
            return;
        }
        let files: &[Change] = o.info.as_ref().map_or(&[][..], |i| &i.1[..]);
        let sides = o.cmp.as_deref().and_then(|c| Some((c, c.data.as_ref()?.as_ref().ok()?)));
        let mono = FontId::monospace((fs - 2.5).max(8.0));
        let small = FontId::proportional(fs - 1.5);
        let sum_h = (fs + 14.0).round();
        // The files. Each row's height is known before it is drawn, so only the rows on
        // screen are drawn and only their pictures read.
        let row_h = (fs + 4.0 + 4.0 * sp).round();
        let code = env.cfg.inspector_font_size;
        let spacing = env.cfg.preview_line_spacing;
        let line_h = crate::inspector::row_height(ui, code, env.cfg.preview_line_spacing);
        let cw = ui.fonts_mut(|f| f.glyph_width(&mono, '0'));
        let pic_small = (fs * 4.6).round();
        let pic_big = (fs * 15.0).round().min(body.height() * 0.6).max(pic_small);
        let cap = (body.height() * 0.62).max(line_h * 8.0);
        let block = |x: &Inline| match &x.text {
            Some((l, _, cut)) if !l.is_empty() => {
                let rows = x.wrapped.as_ref().filter(|_| wrap).map_or(l.len(), |w| w.rows.len()) + usize::from(*cut);
                (rows as f32 * line_h + 16.0).min(cap)
            }
            _ => row_h,
        };
        let extra = |o: &Open, f: usize| -> f32 {
            let open = o.inline.iter().find(|x| x.file == f);
            match (o.picture[f], open) {
                (true, Some(_)) => pic_big + 8.0,
                (true, None) => pic_small + 8.0,
                (false, Some(x)) => block(x) + 8.0,
                (false, None) => 0.0,
            }
        };
        let heights: Vec<f32> = o.items.iter().map(|&(it, _)| row_h + if let Item::File(f) = it { extra(o, f) } else { 0.0 }).collect();
        // The summary over the files: after a comparison's commits.
        let fstart = o.items.iter().position(|(it, _)| matches!(it, Item::Dir(_) | Item::File(_))).unwrap_or(o.items.len());
        let total: f32 = heights.iter().sum::<f32>() + sum_h;
        let (all, _) = ui.allocate_exact_size(vec2(ui.available_width(), total + 10.0), Sense::hover());
        let clip = ui.clip_rect();
        let badge = (fs + 2.0).round();
        // The listing's icons: 16 points, 7 before the name. A file's letter sits under
        // its folder's icon.
        let (icon, gap) = (16.0, 7.0);
        let indent = badge + 6.0;
        let show_counts = all.width() > 230.0;
        let show_bar = all.width() > 300.0;
        let digits = |n: u32| n.to_string().len() as f32;
        let add_w = if o.widest.0 > 0 { (digits(o.widest.0) + 1.0) * cw } else { 0.0 };
        let del_w = if o.widest.1 > 0 { (digits(o.widest.1) + 1.0) * cw } else { 0.0 };
        let bw = (fs * 0.45).round().max(4.0);
        let bar_w = 5.0 * bw + 8.0;
        let font = FontId::proportional(fs - 0.5);
        let mut y = all.top();
        let toggle = !o.nodes.is_empty();
        let summary_at = |ui: &mut Ui, y: f32| {
            let r = Rect::from_min_size(pos2(all.left(), y), vec2(all.width(), sum_h));
            if fstart > 0 {
                ui.painter().line_segment([pos2(r.left(), r.top() + 2.0), pos2(r.right(), r.top() + 2.0)], Stroke::new(1.0, pal.border));
            }
            files_summary(ui, r, files.len(), o.totals, toggle, tree, none, fs, &pal)
        };
        for k in 0..o.items.len() {
            if k == fstart {
                if let Some(a) = summary_at(ui, y) {
                    act = Some(a);
                }
                y += sum_h;
            }
            let (it, depth) = o.items[k];
            let r = Rect::from_min_size(pos2(all.left(), y), vec2(all.width(), heights[k]));
            y += heights[k];
            let row = Rect::from_min_size(r.min, vec2(r.width(), row_h));
            if reveal && k == o.sel {
                ui.scroll_to_rect(row.expand2(vec2(0.0, 6.0)), None);
            }
            if r.bottom() < clip.top() || r.top() > clip.bottom() {
                continue;
            }
            let id = ui.id().with(("gitfile", k, it));
            let resp = ui.interact(row, id, Sense::click());
            let on = k == o.sel && keyed;
            if on || resp.hovered() {
                ui.painter().rect_filled(row, CornerRadius::same(4), if on { pal.row_hover } else { pal.hover });
            }
            let x0 = row.left() + 6.0 + depth as f32 * indent;
            if resp.clicked() {
                o.sel = k;
                act = Some(Act::Toggle(k));
            }
            let f = match it {
                Item::Dir(n) => {
                    let Node::Dir { name, path, files: count, .. } = &o.nodes[n] else { continue };
                    let folded = o.folded.contains(&n);
                    crate::theme::paint_glyph(ui.painter(), Rect::from_center_size(pos2(x0 + badge / 2.0, row.center().y), Vec2::splat(13.0)), if folded { Glyph::ChevronRight } else { Glyph::ChevronDown }, pal.text_dim);
                    let ir = Rect::from_min_size(pos2(x0 + badge + 6.0, row.center().y - icon / 2.0), Vec2::splat(icon));
                    crate::icons::folder(ui.painter(), ir, &root.join(path));
                    let nx = ir.right() + gap;
                    let g = crate::listing::elided(ui, name, font.clone(), (row.right() - nx - 40.0).max(8.0), pal.text);
                    let cx = nx + g.size().x + 8.0;
                    ui.painter().galley(pos2(nx, row.center().y - g.size().y / 2.0), g, pal.text);
                    ui.painter().text(pos2(cx, row.center().y), Align2::LEFT_CENTER, count.to_string(), small.clone(), pal.text_dim);
                    continue;
                }
                Item::Side(side) => {
                    let Some((cm, sd)) = sides else { continue };
                    let (n, shown) = (sd.counts[side as usize], sd.only[side as usize].len());
                    if shown > 0 {
                        let g = if cm.folded[side as usize] { Glyph::ChevronRight } else { Glyph::ChevronDown };
                        crate::theme::paint_glyph(ui.painter(), Rect::from_center_size(pos2(x0 + badge / 2.0, row.center().y), Vec2::splat(13.0)), g, pal.text_dim);
                    }
                    let mut t = crate::gitcompare::heading(n, &cm.names[side as usize]);
                    if n > shown && !cm.folded[side as usize] {
                        t = format!("{t} · the newest {shown}");
                    }
                    let nx = x0 + badge + 6.0;
                    let g = crate::listing::elided(ui, &t, font.clone(), (row.right() - nx - 8.0).max(8.0), if n > 0 { pal.text } else { pal.text_dim });
                    ui.painter().galley(pos2(nx, row.center().y - g.size().y / 2.0), g, pal.text);
                    continue;
                }
                Item::Pick(side, i) => {
                    let Some(c) = sides.and_then(|(_, sd)| sd.only[side as usize].get(i)) else { continue };
                    // Its hash, subject, and on the right who made it and when.
                    let hg = ui.painter().layout_no_wrap(c.hash[..7].to_string(), mono.clone(), pal.text_dim);
                    let hw = hg.size().x;
                    ui.painter().galley(pos2(x0, row.center().y - hg.size().y / 2.0), hg, pal.text_dim);
                    let mut right = row.right() - 8.0;
                    let sx = x0 + hw + 8.0;
                    if right - sx > 260.0 {
                        let g = crate::listing::elided(ui, &format!("{}  ·  {}", c.author, ago(c.time, now)), small.clone(), (right - sx) * 0.38, pal.text_dim);
                        right -= g.size().x;
                        ui.painter().galley(pos2(right, row.center().y - g.size().y / 2.0), g, pal.text_dim);
                        right -= 10.0;
                    }
                    let g = crate::listing::elided(ui, &c.subject, font.clone(), (right - sx).max(8.0), pal.text);
                    ui.painter().galley(pos2(sx, row.center().y - g.size().y / 2.0), g, pal.text);
                    crate::ui::note_control(ui, row, "compared commit");
                    resp.on_hover_text(format!("{}\n{} · {}", c.subject, c.author, &c.hash[..10.min(c.hash.len())]));
                    continue;
                }
                Item::File(f) => f,
            };
            let ch = &files[f];
            // A quick third click (counted as a triple by egui) is still a double.
            if resp.double_clicked() || resp.triple_clicked() {
                act = Some(Act::Large(f, true));
            }
            if resp.secondary_clicked() {
                o.sel = k;
                act = Some(Act::Menu(f, ui.ctx().pointer_interact_pos().unwrap_or(row.center())));
            }
            // The status letter in a tinted square, with what it means on hover.
            let sc = status_color(ch.status, &pal);
            let br = Rect::from_center_size(pos2(x0 + badge / 2.0, row.center().y), Vec2::splat(badge));
            ui.painter().rect_filled(br, CornerRadius::same(4), sc.gamma_multiply(0.16));
            ui.painter().text(br.center(), Align2::CENTER_CENTER, (ch.status as char).to_string(), FontId::monospace((fs - 3.0).max(8.0)), sc);
            ui.interact(br, id.with("tip"), Sense::hover()).on_hover_text(status_tip(ch));
            // Right side: the open-large button on hover, then the counts and the blocks.
            let expanded = o.expanded(f);
            let mut right = row.right() - 6.0;
            let ob = Rect::from_center_size(pos2(right - 10.0, row.center().y), Vec2::splat(20.0));
            right -= 26.0;
            if resp.hovered() || expanded || ui.rect_contains_pointer(ob) {
                let b = ui.interact(ob, id.with("large"), Sense::click());
                crate::ui::note_control(ui, ob, "open large");
                if b.hovered() {
                    ui.painter().rect_filled(ob, CornerRadius::same(4), pal.row_hover);
                }
                crate::theme::paint_glyph(ui.painter(), Rect::from_center_size(ob.center(), Vec2::splat(12.0)), Glyph::Maximize, if b.hovered() { pal.text } else { pal.text_dim });
                if b.on_hover_text("Open large").clicked() {
                    act = Some(Act::Large(f, false));
                }
            }
            match ch.lines {
                Some((a, d)) if show_counts => {
                    if show_bar {
                        let (g, rd) = blocks(a, d);
                        for b in 0..5 {
                            let bx = right - bar_w + b as f32 * (bw + 2.0);
                            let c = if b < g { crate::gitstatus::GREEN } else if b < g + rd { pal.danger } else { pal.border };
                            ui.painter().rect_filled(Rect::from_min_size(pos2(bx, row.center().y - bw / 2.0), Vec2::splat(bw)), CornerRadius::same(1), c);
                        }
                        right -= bar_w + 8.0;
                    }
                    if d > 0 {
                        ui.painter().text(pos2(right, row.center().y), Align2::RIGHT_CENTER, format!("−{d}"), mono.clone(), pal.danger);
                    }
                    right -= del_w + if del_w > 0.0 { 6.0 } else { 0.0 };
                    if a > 0 {
                        ui.painter().text(pos2(right, row.center().y), Align2::RIGHT_CENTER, format!("+{a}"), mono.clone(), crate::gitstatus::GREEN);
                    }
                    right -= add_w;
                }
                None if show_counts => {
                    let g = ui.painter().layout_no_wrap("binary".into(), small.clone(), pal.text_dim);
                    right -= g.size().x;
                    ui.painter().galley(pos2(right, row.center().y - g.size().y / 2.0), g, pal.text_dim);
                }
                _ => {}
            }
            // The name: struck through when deleted, "old → new" when renamed, the folder
            // after it in the flat list.
            let (dir, name) = ch.path.rsplit_once('/').unwrap_or(("", &ch.path));
            let mut job = LayoutJob::default();
            let nf = |c: Color32, strike: bool| TextFormat { font_id: font.clone(), color: c, strikethrough: if strike { Stroke::new(1.0, c) } else { Stroke::NONE }, valign: Align::Center, ..Default::default() };
            if let Some(from) = &ch.from {
                let (fdir, fname) = from.rsplit_once('/').unwrap_or(("", from));
                job.append(if fdir == dir { fname } else { from }, 0.0, nf(pal.text_dim, false));
                job.append("→", 6.0, nf(pal.text_dim, false));
                job.append(name, 6.0, nf(pal.text, false));
            } else {
                job.append(name, 0.0, if ch.status == b'D' { nf(pal.text_dim, true) } else { nf(pal.text, false) });
            }
            // The file's icon (the new name's for a rename), faded with a deleted name.
            let ir = Rect::from_min_size(pos2(x0 + badge + 6.0, row.center().y - icon / 2.0), Vec2::splat(icon));
            let mut ip = ui.painter().clone();
            if ch.status == b'D' {
                ip.multiply_opacity(0.45);
            }
            crate::icons::file(&ip, ir, name);
            let nx = ir.right() + gap;
            job.wrap = TextWrapping { max_width: (right - 10.0 - nx).max(8.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
            let g = ui.fonts_mut(|f| f.layout_job(job));
            let dx = nx + g.size().x + 10.0;
            ui.painter().galley(pos2(nx, row.center().y - g.size().y / 2.0), g, pal.text);
            if !(tree && !o.nodes.is_empty()) && !dir.is_empty() && right - 10.0 - dx > 30.0 {
                let g = crate::listing::elided(ui, dir, small.clone(), right - 10.0 - dx, pal.text_dim);
                ui.painter().galley(pos2(dx, row.center().y - g.size().y / 2.0), g, pal.text_dim);
            }
            // Under the row: a picture's two versions, or the open diff.
            let under = Rect::from_min_max(pos2(nx, row.bottom() + 2.0), pos2(r.right() - 6.0, r.bottom() - 6.0));
            if o.picture[f] {
                pair(ui, under, &mut o.pics, ch, f, &ctx, &root, &o.hash, &o.parents, 512, &pal);
            } else if let Some(x) = o.inline.iter_mut().find(|x| x.file == f) {
                let b = Rect::from_min_max(pos2(row.left() + 4.0, under.top()), pos2(row.right() - 4.0, under.bottom()));
                match &x.text {
                    None => crate::ui::spinner(ui.painter(), pos2(nx + 6.0, b.top() + row_h / 2.0), 5.0, pal.accent, ui.input(|i| i.time)),
                    Some((l, _, _)) if l.is_empty() => {
                        ui.painter().text(pos2(nx, b.top() + row_h / 2.0), Align2::LEFT_CENTER, "No text changes", small.clone(), pal.text_dim);
                    }
                    Some((lines, spans, cut)) => {
                        ui.painter().rect_filled(b, CornerRadius::same(6), pal.alt);
                        let mut cui = ui.new_child(UiBuilder::new().max_rect(b).id_salt(("inline", f)));
                        cui.set_clip_rect(b.intersect(clip));
                        if !wrap {
                            x.wrapped = None;
                        }
                        let wrapped = if wrap { Some(&mut x.wrapped) } else { None };
                        crate::inspector::text_view(&mut cui, b, lines, *cut, spans.as_deref(), false, wrapped, code, &pal, true, spacing);
                    }
                }
                if x.text.is_none() {
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
                }
            }
        }
        if fstart == o.items.len() {
            if let Some(a) = summary_at(ui, y) {
                act = Some(a);
            }
        }
    });
    match act {
        Some(Act::Hash(x)) => h.open_hash(x),
        Some(Act::Split(split)) => {
            if let Some(o) = h.open.as_mut() {
                o.set_split(split, tree);
            }
        }
        Some(Act::Tree(t)) => {
            h.tree = t;
            if let Some(o) = h.open.as_mut() {
                o.rebuild(t);
            }
        }
        Some(Act::Menu(f, pos)) => env.actions.push(Action::Open(crate::popups::Popup::git_file(env.panel_id, f, pos))),
        Some(Act::Toggle(k)) => {
            if let Some(x) = h.open.as_mut().and_then(|o| o.toggle(k, tree)) {
                h.open_hash(x);
            }
        }
        Some(Act::Large(f, double)) => {
            if let Some(o) = h.open.as_mut() {
                // A double click's first click opened or closed the diff under the row:
                // leave it open, as the first click meant.
                if double && !o.expanded(f) {
                    o.inline.push(Inline { file: f, text: None, job: None, wrapped: None });
                }
                h.wrapped = None;
                o.diff = Some(Diff::new(f, false));
            }
        }
        None => {}
    }
}

/// A changed picture's old and new versions side by side (one of them when it was added
/// or deleted), each read once it is on screen.
#[allow(clippy::too_many_arguments)]
fn pair(ui: &Ui, area: Rect, pics: &mut Vec<Pic>, ch: &Change, f: usize, ctx: &egui::Context, root: &Path, hash: &str, parents: &[String], px: u32, pal: &Palette) {
    let (old, new) = (ch.status != b'A' && !parents.is_empty(), ch.status != b'D');
    let gap = 26.0;
    let both = old && new;
    let sw = ((area.width() - gap) / 2.0).min(area.height() * 2.4).max(8.0);
    let left = Rect::from_min_size(area.min, vec2(sw, area.height()));
    let right = left.translate(vec2(sw + gap, 0.0));
    if both {
        crate::theme::paint_glyph(ui.painter(), Rect::from_center_size(pos2(left.right() + gap / 2.0, left.center().y), Vec2::splat(13.0)), Glyph::Forward, pal.text_dim);
    }
    let sides = [(old, true, left), (new, false, if both { right } else { left })];
    for (shown, is_old, s) in sides {
        if !shown || !ui.is_rect_visible(s) {
            continue;
        }
        ui.painter().rect_stroke(s, CornerRadius::same(5), Stroke::new(1.0, pal.border), StrokeKind::Inside);
        let k = want_pic(pics, ctx, root, hash, parents, ch, f, is_old, px);
        let p = &pics[k];
        let tip = if is_old { "Before" } else { "After" };
        match &p.tex {
            Some((tex, dims)) => {
                let [w, hh] = tex.size();
                let a = s.shrink(4.0);
                let k = (a.width() / w.max(1) as f32).min(a.height() / hh.max(1) as f32).min(2.0);
                let ir = Rect::from_center_size(a.center(), vec2(w as f32 * k, hh as f32 * k));
                crate::inspector::checkerboard(ui.painter(), ir, pal);
                // Gone after this commit: shown faded.
                let tint = if is_old && !new { Color32::WHITE.gamma_multiply(0.5) } else { Color32::WHITE };
                ui.painter().image(tex.id(), ir, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), tint);
                ui.interact(s, ui.id().with(("pic", f, is_old, px)), Sense::hover()).on_hover_text(format!("{tip}  ·  {} × {}", dims[0], dims[1]));
            }
            None if p.failed => {
                ui.painter().text(s.center(), Align2::CENTER_CENTER, "Can't show", FontId::proportional(11.5), pal.text_dim);
            }
            None => {
                crate::ui::spinner(ui.painter(), s.center(), 5.0, pal.accent, ui.input(|i| i.time));
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
            }
        }
    }
}

/// One file in the whole panel: its diff, its two versions when it is a picture, or the
/// file as of the commit.
fn whole(ui: &mut Ui, body: Rect, h: &mut History, wrap: bool, env: &mut Env) {
    let pal = *env.pal;
    let fs = env.cfg.font_size;
    let root = h.root.clone();
    let Some(o) = h.open.as_mut() else { return };
    let Some((f, blob)) = o.diff.as_ref().map(|d| (d.file, d.blob)) else { return };
    if o.info.is_none() {
        return dim(ui, body, "Loading…", fs, &pal);
    }
    if !blob && o.picture[f] {
        let ctx = ui.ctx().clone();
        let a = body.shrink(16.0);
        let hh = (a.width() / 2.0).min(a.height());
        let area = Rect::from_center_size(a.center(), vec2(a.width(), hh));
        let Some((_, files)) = &o.info else { return };
        return pair(ui, area, &mut o.pics, &files[f], f, &ctx, &root, &o.hash, &o.parents, 1600, &pal);
    }
    let Some(d) = o.diff.as_mut() else { return };
    match &d.text {
        None => dim(ui, body, "Loading…", fs, &pal),
        Some(View::Lines(l, ..)) if l.is_empty() => dim(ui, body, if blob { "Empty file" } else { "No text changes" }, fs, &pal),
        Some(View::Lines(lines, ..)) if env.cfg.diff_side && matches!(d.text, Some(View::Lines(.., true))) => crate::sidediff::view(ui, body, lines, &mut h.side, env.cfg.inspector_font_size, env.cfg.preview_line_spacing, &pal),
        Some(View::Lines(lines, spans, cut, is_diff)) => {
            if !wrap {
                h.wrapped = None;
            }
            let wrapped = if wrap { Some(&mut h.wrapped) } else { None };
            crate::inspector::text_view(ui, body, lines, *cut, spans.as_deref(), !*is_diff, wrapped, env.cfg.inspector_font_size, &pal, *is_diff, env.cfg.preview_line_spacing)
        }
        Some(View::Pic(tex, dims)) => {
            let a = body.shrink(12.0);
            let [w, hh] = tex.size();
            let k = (a.width() / w.max(1) as f32).min(a.height() / hh.max(1) as f32).min(4.0);
            let ir = Rect::from_center_size(a.center(), vec2(w as f32 * k, hh as f32 * k));
            crate::inspector::checkerboard(ui.painter(), ir, &pal);
            ui.painter().image(tex.id(), ir, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
            let g = ui.painter().layout_no_wrap(format!("{} × {}", dims[0], dims[1]), FontId::proportional(11.5), pal.text_dim);
            let r = Rect::from_min_size(pos2(body.right() - g.size().x - 18.0, body.bottom() - 24.0), g.size() + vec2(10.0, 6.0));
            ui.painter().rect_filled(r, CornerRadius::same(4), pal.raised.gamma_multiply(0.9));
            ui.painter().galley(r.min + vec2(5.0, 3.0), g, pal.text_dim);
        }
        Some(View::Msg(m)) => dim(ui, body, m, fs, &pal),
    }
}
