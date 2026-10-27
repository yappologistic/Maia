//! Your GitHub profile as a place beside Computer: who you are on the left (picture,
//! bio, followers, achievements, organisations, languages), your year and what you did
//! lately on the right, drawn like a history. The row under the cursor opens to the files
//! its commit changed; Enter goes to the commit in its clone when there is one.
//!
//! Everything comes through `gh`, signed in by the user, with two read-only GraphQL
//! queries; Maia never sees the token. Achievements are in no API, so they are read from
//! the public profile page once a day, and vanish quietly when that page changes shape.
//! Answers are kept in the cache folder, so the page opens at once with the last ones
//! while new ones are asked for.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant, SystemTime};

use egui::{Align2, Color32, CornerRadius, FontId, Key, Pos2, Rect, ScrollArea, Sense, Shape, Stroke, TextureHandle, Ui, UiBuilder, pos2, vec2};
use serde_json::Value;

use crate::app::Action;
use crate::config::Config;
use crate::theme::Palette;

/// New answers are asked for when the page is shown and the last are older than this.
const STALE: Duration = Duration::from_secs(5 * 60);
/// The achievements page is read at most this often.
const ACH_AGE: Duration = Duration::from_secs(86_400);
/// Pictures are kept this long.
const PIC_AGE: Duration = Duration::from_secs(7 * 86_400);
/// Rows kept in the timeline.
const ITEMS: usize = 80;

const VIEWER: &str = "query { viewer { id login name bio pronouns location company websiteUrl createdAt avatarUrl(size: 192) status { message } \
followers(first: 3) { totalCount nodes { login avatarUrl(size: 64) } } following { totalCount } \
organizations(first: 6) { nodes { login avatarUrl(size: 64) } } \
contributionsCollection { contributionCalendar { totalContributions weeks { contributionDays { date weekday contributionCount contributionLevel } } } } \
repositories(first: 30, ownerAffiliations: OWNER, orderBy: {field: PUSHED_AT, direction: DESC}) { totalCount nodes { nameWithOwner createdAt url \
languages(first: 6, orderBy: {field: SIZE, direction: DESC}) { edges { size node { name color } } } \
releases(first: 2, orderBy: {field: CREATED_AT, direction: DESC}) { nodes { name tagName createdAt url } } } } } }";

/// Recent commits of the eight repositories pushed to last: what the timeline is made of.
const HISTORY: &str = "query($id: ID!) { viewer { repositories(first: 8, ownerAffiliations: OWNER, orderBy: {field: PUSHED_AT, direction: DESC}) { nodes { nameWithOwner \
defaultBranchRef { target { ... on Commit { history(first: 15, author: {id: $id}) { nodes { oid messageHeadline committedDate url } } } } } } } } }";

// ------------------------------------------------------------------ what is shown

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Commit,
    Release,
    Created,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub kind: Kind,
    pub title: String,
    /// OWNER/NAME.
    pub repo: String,
    /// Seconds since the epoch.
    pub when: i64,
    pub sha: Option<String>,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Achievement {
    pub slug: String,
    pub name: String,
    /// 0 with no tier yet, then bronze, silver, gold.
    pub tier: u8,
    /// The "x2" GitHub writes on it; 1 without.
    pub count: u8,
}

#[derive(Clone, Debug)]
pub struct Day {
    pub date: String,
    pub count: u32,
    pub level: u8,
}

#[derive(Clone, Debug, Default)]
pub struct Profile {
    pub login: String,
    pub name: String,
    pub bio: String,
    pub pronouns: String,
    pub location: String,
    pub company: String,
    pub website: String,
    pub status: String,
    pub avatar: String,
    pub joined: i64,
    pub followers: u32,
    pub following: u32,
    pub repos: u32,
    pub faces: Vec<(String, String)>,
    pub orgs: Vec<(String, String)>,
    /// Name, colour and share of the bytes in the repositories, largest first.
    pub langs: Vec<(String, Color32, f32)>,
    pub days: Vec<Day>,
    /// The first day's place in its week (0 is Sunday, as GitHub draws it).
    pub first_weekday: usize,
    pub total: u32,
    pub items: Vec<Item>,
    pub achievements: Vec<Achievement>,
    /// Clones on this computer by OWNER/NAME, lower case.
    pub clones: HashMap<String, PathBuf>,
}

impl Profile {
    pub fn clone_of(&self, repo: &str) -> Option<&PathBuf> {
        self.clones.get(&repo.to_lowercase())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Trouble {
    NoGh,
    SignedOut,
    Unreachable,
}

// ------------------------------------------------------------------ reading answers

/// Seconds since the epoch for GitHub's "2026-10-27T04:04:12Z".
pub fn iso(s: &str) -> Option<i64> {
    let n = |a: usize, l: usize| -> Option<i64> { s.get(a..a + l)?.parse().ok() };
    let (y, m, d) = (n(0, 4)?, n(5, 2)?, n(8, 2)?);
    let (hh, mm, ss) = (n(11, 2).unwrap_or(0), n(14, 2).unwrap_or(0), n(17, 2).unwrap_or(0));
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * 86_400 + hh * 3600 + mm * 60 + ss)
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().trim().to_string()
}

fn hex(s: &str) -> Option<Color32> {
    let s = s.strip_prefix('#')?;
    let v = u32::from_str_radix(s, 16).ok().filter(|_| s.len() == 6)?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// The profile query's answer. None when it isn't one (an error, or signed out).
pub fn parse_viewer(v: &Value) -> Option<Profile> {
    let u = v.get("data")?.get("viewer")?;
    let login = text(u.get("login")?);
    if login.is_empty() {
        return None;
    }
    let pics = |list: &Value| -> Vec<(String, String)> { list.as_array().map(|a| a.iter().map(|n| (text(&n["login"]), text(&n["avatarUrl"]))).filter(|(l, _)| !l.is_empty()).collect()).unwrap_or_default() };
    let mut p = Profile {
        name: text(&u["name"]),
        bio: text(&u["bio"]).split_whitespace().collect::<Vec<_>>().join(" "),
        pronouns: text(&u["pronouns"]),
        location: text(&u["location"]),
        company: text(&u["company"]),
        website: text(&u["websiteUrl"]),
        status: text(&u["status"]["message"]),
        avatar: text(&u["avatarUrl"]),
        joined: iso(&text(&u["createdAt"])).unwrap_or(0),
        followers: u["followers"]["totalCount"].as_u64().unwrap_or(0) as u32,
        following: u["following"]["totalCount"].as_u64().unwrap_or(0) as u32,
        repos: u["repositories"]["totalCount"].as_u64().unwrap_or(0) as u32,
        faces: pics(&u["followers"]["nodes"]),
        orgs: pics(&u["organizations"]["nodes"]),
        login,
        ..Default::default()
    };
    let cal = &u["contributionsCollection"]["contributionCalendar"];
    p.total = cal["totalContributions"].as_u64().unwrap_or(0) as u32;
    for (wi, w) in cal["weeks"].as_array().into_iter().flatten().enumerate() {
        for (di, d) in w["contributionDays"].as_array().into_iter().flatten().enumerate() {
            if wi == 0 && di == 0 {
                p.first_weekday = d["weekday"].as_u64().unwrap_or(0).min(6) as usize;
            }
            let level = match d["contributionLevel"].as_str().unwrap_or_default() {
                "FIRST_QUARTILE" => 1,
                "SECOND_QUARTILE" => 2,
                "THIRD_QUARTILE" => 3,
                "FOURTH_QUARTILE" => 4,
                _ => 0,
            };
            p.days.push(Day { date: text(&d["date"]), count: d["contributionCount"].as_u64().unwrap_or(0) as u32, level });
        }
    }
    let year_ago = iso(&p.days.first().map(|d| d.date.clone()).unwrap_or_default()).unwrap_or(0);
    let mut langs: Vec<(String, Color32, u64)> = vec![];
    for r in u["repositories"]["nodes"].as_array().into_iter().flatten() {
        let repo = text(&r["nameWithOwner"]);
        for e in r["languages"]["edges"].as_array().into_iter().flatten() {
            let (name, size) = (text(&e["node"]["name"]), e["size"].as_u64().unwrap_or(0));
            let col = hex(e["node"]["color"].as_str().unwrap_or_default()).unwrap_or(Color32::GRAY);
            match langs.iter_mut().find(|l| l.0 == name) {
                Some(l) => l.2 += size,
                None => langs.push((name, col, size)),
            }
        }
        for rel in r["releases"]["nodes"].as_array().into_iter().flatten() {
            let title = [text(&rel["name"]), text(&rel["tagName"])].into_iter().find(|s| !s.is_empty()).unwrap_or_default();
            if let Some(when) = iso(&text(&rel["createdAt"])).filter(|&t| t >= year_ago) {
                let short = repo.rsplit('/').next().unwrap_or(&repo);
                let title = if title.to_lowercase().contains(&short.to_lowercase()) { format!("Released {title}") } else { format!("Released {short} {title}") };
                p.items.push(Item { kind: Kind::Release, title, repo: repo.clone(), when, sha: None, url: text(&rel["url"]) });
            }
        }
        if let Some(when) = iso(&text(&r["createdAt"])).filter(|&t| t >= year_ago) {
            let short = repo.rsplit('/').next().unwrap_or(&repo).to_string();
            p.items.push(Item { kind: Kind::Created, title: format!("Created {short}"), repo: repo.clone(), when, sha: None, url: text(&r["url"]) });
        }
    }
    langs.sort_by(|a, b| b.2.cmp(&a.2));
    let all: u64 = langs.iter().map(|l| l.2).sum::<u64>().max(1);
    p.langs = langs.into_iter().take(5).map(|(n, c, s)| (n, c, s as f32 / all as f32)).collect();
    Some(p)
}

/// The recent commits query's answer.
pub fn parse_history(v: &Value) -> Vec<Item> {
    let mut out = vec![];
    for r in v["data"]["viewer"]["repositories"]["nodes"].as_array().into_iter().flatten() {
        let repo = text(&r["nameWithOwner"]);
        for c in r["defaultBranchRef"]["target"]["history"]["nodes"].as_array().into_iter().flatten() {
            let Some(when) = iso(&text(&c["committedDate"])) else { continue };
            out.push(Item { kind: Kind::Commit, title: text(&c["messageHeadline"]), repo: repo.clone(), when, sha: Some(text(&c["oid"])).filter(|s| !s.is_empty()), url: text(&c["url"]) });
        }
    }
    out
}

/// The badges on a public profile page: name and slug from each picture, the tier from
/// the picture's file name, the count from the label beside it. Each badge appears more
/// than once on the page; the first counts.
pub fn parse_achievements(html: &str) -> Vec<Achievement> {
    let mut out: Vec<Achievement> = vec![];
    let mut at = 0;
    while let Some(i) = html[at..].find("alt=\"Achievement: ") {
        let start = at + i;
        let name_from = start + "alt=\"Achievement: ".len();
        let Some(name_len) = html[name_from..].find('"') else { break };
        let name = html[name_from..name_from + name_len].trim().to_string();
        at = name_from + name_len;
        // The picture's address and its hover card come before the alt text.
        let tag = &html[html[..start].rfind('<').unwrap_or(start)..start];
        let slug = tag.split("/achievements/").nth(1).and_then(|s| s.split('/').next()).unwrap_or_default().to_string();
        let tier = ["-gold-", "-silver-", "-bronze-"].iter().position(|t| tag.contains(t)).map_or(0, |i| 3 - i as u8);
        let rest = &html[at..html[at..].find("</a>").map_or(html.len(), |e| at + e)];
        let count = rest.split(">x").nth(1).and_then(|s| s.split('<').next()).and_then(|n| n.trim().parse::<u8>().ok()).unwrap_or(1);
        if name.is_empty() || name.len() > 40 || slug.is_empty() || out.iter().any(|a| a.slug == slug) {
            continue;
        }
        out.push(Achievement { slug, name, tier, count });
        if out.len() >= 16 {
            break;
        }
    }
    out
}

fn ach_line(a: &Achievement) -> String {
    format!("{}\t{}\t{}\t{}", a.slug, a.name, a.tier, a.count)
}

fn ach_from_line(l: &str) -> Option<Achievement> {
    let mut f = l.split('\t');
    Some(Achievement { slug: f.next()?.to_string(), name: f.next()?.to_string(), tier: f.next()?.parse().ok()?, count: f.next()?.parse().ok()? })
}

// ------------------------------------------------------------------ asking

pub(crate) fn cache_dir() -> PathBuf {
    // Tests run outside the sandbox too: never the real cache.
    let base = if cfg!(test) { None } else { dirs::cache_dir() };
    base.unwrap_or_else(std::env::temp_dir).join("maia/github")
}

/// Written readable by the user alone: private repositories' names are in it.
fn keep(path: &Path, data: &[u8]) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = path.with_extension("part");
    let ok = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp).and_then(|mut f| f.write_all(data));
    if ok.is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

fn age(path: &Path) -> Option<Duration> {
    SystemTime::now().duration_since(std::fs::metadata(path).ok()?.modified().ok()?).ok()
}

fn gh(tools: &crate::drives::Tools, args: &[&str]) -> Option<Vec<u8>> {
    let mut c = crate::media::tool_cmd(Command::new(tools.path_of("gh")));
    c.args(args).env("GH_PROMPT_DISABLED", "1").env("GH_NO_UPDATE_NOTIFIER", "1").env("NO_COLOR", "1");
    crate::media::output_capped(c, Duration::from_secs(30), 16 << 20, &|| false)
}

fn graphql(tools: &crate::drives::Tools, query: &str, id: Option<&str>) -> Option<Value> {
    let q = format!("query={query}");
    let idv = id.map(|i| format!("id={i}"));
    let mut args = vec!["api", "graphql", "-f", q.as_str()];
    if let Some(i) = &idv {
        args.extend(["-F", i.as_str()]);
    }
    serde_json::from_slice(&gh(tools, &args)?).ok()
}

#[cfg(not(test))]
fn web(url: &str) -> Option<Vec<u8>> {
    if !crate::media::have("curl") {
        return None;
    }
    let mut c = crate::media::tool("curl");
    c.args(crate::avatars::curl_args(url));
    match crate::avatars::parse_curl(&crate::media::output_capped(c, Duration::from_secs(15), (2 << 20) + 16, &|| false)?) {
        crate::avatars::Http::Ok(b) => Some(b),
        _ => None,
    }
}

/// Test builds never reach the network.
#[cfg(test)]
fn web(_: &str) -> Option<Vec<u8>> {
    None
}

/// The clones Recent Projects knows of, by the github.com repository their origin names.
fn clones() -> HashMap<String, PathBuf> {
    let mut m = HashMap::new();
    for root in crate::projects::load_cache(&crate::projects::cache_path()) {
        if let Some((o, r)) = crate::gitlog::origin_url(&root).as_deref().and_then(crate::avatars::github_repo) {
            m.entry(format!("{o}/{r}").to_lowercase()).or_insert(root);
        }
    }
    m
}

enum Msg {
    /// From the cache (`false`) or just asked (`true`).
    Profile(Box<Profile>, bool),
    Trouble(Trouble),
}

fn assemble(viewer: &Value, history: Option<&Value>, ach: Vec<Achievement>, clones: HashMap<String, PathBuf>) -> Option<Profile> {
    let mut p = parse_viewer(viewer)?;
    if let Some(h) = history {
        p.items.extend(parse_history(h));
    }
    p.items.sort_by(|a, b| b.when.cmp(&a.when));
    p.items.truncate(ITEMS);
    p.achievements = ach;
    p.clones = clones;
    Some(p)
}

fn cached_achievements(dir: &Path) -> Vec<Achievement> {
    std::fs::read_to_string(dir.join("achievements.txt")).map(|s| s.lines().filter_map(ach_from_line).collect()).unwrap_or_default()
}

/// Shows what was kept first (when `cached`), then asks. The tools come with it: a test's
/// fakes are only known to the thread that set them.
fn fetch(tx: Sender<Msg>, ctx: egui::Context, cached: bool, tools: Option<crate::drives::Tools>) {
    let dir = cache_dir();
    let send = |m: Msg| {
        let _ = tx.send(m);
        ctx.request_repaint();
    };
    let read = |n: &str| std::fs::read(dir.join(n)).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let local = clones();
    if cached {
        if let Some(p) = read("viewer.json").and_then(|v| assemble(&v, read("history.json").as_ref(), cached_achievements(&dir), local.clone())) {
            send(Msg::Profile(Box::new(p), false));
        }
    }
    let Some(tools) = tools.filter(|t| t.has("gh")) else {
        return send(Msg::Trouble(Trouble::NoGh));
    };
    // Who is signed in, from gh's own settings: no token is read, nothing is asked online.
    let user = gh(&tools, &["config", "get", "user", "-h", "github.com"]).map(|b| String::from_utf8_lossy(&b).trim().to_string()).unwrap_or_default();
    if user.is_empty() {
        return send(Msg::Trouble(Trouble::SignedOut));
    }
    let Some(viewer) = graphql(&tools, VIEWER, None).filter(|v| v.pointer("/data/viewer/login").is_some()) else {
        return send(Msg::Trouble(Trouble::Unreachable));
    };
    let id = viewer.pointer("/data/viewer/id").and_then(Value::as_str).unwrap_or_default().to_string();
    let history = graphql(&tools, HISTORY, Some(&id)).filter(|v| v.pointer("/data/viewer").is_some());
    let login = viewer.pointer("/data/viewer/login").and_then(Value::as_str).unwrap_or_default().to_string();
    let mut ach = cached_achievements(&dir);
    if age(&dir.join("achievements.txt")).is_none_or(|a| a > ACH_AGE) && login.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        if let Some(page) = web(&format!("https://github.com/{login}?tab=achievements")) {
            ach = parse_achievements(&String::from_utf8_lossy(&page));
            keep(&dir.join("achievements.txt"), ach.iter().map(ach_line).collect::<Vec<_>>().join("\n").as_bytes());
        }
    }
    keep(&dir.join("viewer.json"), viewer.to_string().as_bytes());
    if let Some(h) = &history {
        keep(&dir.join("history.json"), h.to_string().as_bytes());
    }
    match assemble(&viewer, history.as_ref(), ach, local) {
        Some(p) => send(Msg::Profile(Box::new(p), true)),
        None => send(Msg::Trouble(Trouble::Unreachable)),
    }
}

/// A commit's files: from its clone when there is one, else from GitHub.
pub struct FileRow {
    pub path: String,
    pub add: u32,
    pub del: u32,
}

fn commit_files(repo: String, sha: String, root: Option<PathBuf>, tools: Option<crate::drives::Tools>) -> Vec<FileRow> {
    if let Some(root) = root {
        let v = crate::gitlog::files(&root, &format!("{sha}^"), &sha);
        if !v.is_empty() {
            return v.into_iter().map(|c| FileRow { add: c.lines.map_or(0, |l| l.0), del: c.lines.map_or(0, |l| l.1), path: c.path }).collect();
        }
    }
    let Some(tools) = tools.filter(|t| t.has("gh")) else { return vec![] };
    let ok = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b));
    if !ok(&repo) || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return vec![];
    }
    let Some(v) = gh(&tools, &["api", &format!("repos/{repo}/commits/{sha}")]).and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else { return vec![] };
    v["files"].as_array().into_iter().flatten().map(|f| FileRow { path: text(&f["filename"]), add: f["additions"].as_u64().unwrap_or(0) as u32, del: f["deletions"].as_u64().unwrap_or(0) as u32 }).collect()
}

// ------------------------------------------------------------------ pictures

enum Pic {
    Asking(Receiver<Option<egui::ColorImage>>),
    Ready(TextureHandle),
    None,
}

fn picture(url: String, px: u32) -> Option<egui::ColorImage> {
    let dir = cache_dir().join("pictures");
    let key = crate::thumbcache::md5_hex(url.as_bytes());
    let path = dir.join(&key);
    let bytes = match age(&path) {
        Some(a) if a < PIC_AGE => std::fs::read(&path).ok(),
        _ => None,
    };
    let bytes = match bytes {
        Some(b) => b,
        None => {
            let sized = if url.contains('?') { format!("{url}&s={px}") } else { format!("{url}?s={px}") };
            match web(&sized).filter(|_| url.starts_with("https://")) {
                Some(b) => {
                    keep(&path, &b);
                    b
                }
                None => std::fs::read(&path).ok()?,
            }
        }
    };
    crate::imgdec::decode_bytes(&bytes).map(|img| crate::avatars::round(&img, px))
}

// ------------------------------------------------------------------ the page's state

pub struct Page {
    pub data: Option<Box<Profile>>,
    pub trouble: Option<Trouble>,
    rx: Option<Receiver<Msg>>,
    asked: Option<Instant>,
    /// When the answers shown were GitHub's.
    pub updated: Option<SystemTime>,
    pub sel: usize,
    pub keyed: bool,
    reveal: bool,
    files: HashMap<String, Result<Vec<FileRow>, Receiver<Vec<FileRow>>>>,
    pics: HashMap<String, Pic>,
}

impl Default for Page {
    fn default() -> Self {
        Page { data: None, trouble: None, rx: None, asked: None, updated: None, sel: 0, keyed: false, reveal: false, files: HashMap::new(), pics: HashMap::new() }
    }
}

impl Page {
    /// Ask GitHub again (Refresh, or the page shown after a while).
    pub fn refresh(&mut self, ctx: &egui::Context) {
        if self.rx.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let cached = self.data.is_none();
        let ctx = ctx.clone();
        let tools = crate::drives::Tools::current();
        self.asked = Some(Instant::now());
        self.rx = Some(rx);
        std::thread::spawn(move || fetch(tx, ctx, cached, tools));
    }

    pub fn busy(&self) -> bool {
        self.rx.is_some()
    }

    /// What the status bar says under the page.
    pub fn status_line(&self) -> String {
        let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        let when = self.updated.and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|d| crate::gitlog::ago(d.as_secs() as i64, now));
        match (self.busy(), &self.trouble, when) {
            (true, _, _) if self.data.is_some() => "Updating…".into(),
            (_, Some(Trouble::Unreachable), Some(w)) if self.data.is_some() => format!("GitHub couldn't be reached · showing what it said {w}"),
            (_, _, Some(w)) if self.data.is_some() => format!("Updated {w}"),
            _ => String::new(),
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if self.asked.is_none_or(|t| t.elapsed() > STALE) {
            self.refresh(ctx);
        }
        let Some(rx) = &self.rx else { return };
        loop {
            match rx.try_recv() {
                Ok(Msg::Profile(p, fresh)) => {
                    self.updated = if fresh { Some(SystemTime::now()) } else { std::fs::metadata(cache_dir().join("viewer.json")).and_then(|m| m.modified()).ok() };
                    let keep_sel = self.data.as_ref().and_then(|d| d.items.get(self.sel)).map(|i| (i.repo.clone(), i.when));
                    self.data = Some(p);
                    if let (Some((repo, when)), Some(d)) = (keep_sel, &self.data) {
                        self.sel = d.items.iter().position(|i| i.repo == repo && i.when == when).unwrap_or(0);
                    }
                    if fresh {
                        self.trouble = None;
                        self.files.clear();
                    }
                }
                Ok(Msg::Trouble(t)) => self.trouble = Some(t),
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(_) => {
                    self.rx = None;
                    return;
                }
            }
        }
    }

    fn texture(&mut self, ctx: &egui::Context, url: &str, px: u32) -> Option<egui::TextureId> {
        if url.is_empty() {
            return None;
        }
        let key = format!("{url}#{px}");
        let pic = self.pics.entry(key.clone()).or_insert_with(|| {
            let (tx, rx) = channel();
            let (u, c) = (url.to_string(), ctx.clone());
            std::thread::spawn(move || {
                let _ = tx.send(picture(u, px));
                c.request_repaint();
            });
            Pic::Asking(rx)
        });
        if let Pic::Asking(rx) = pic {
            match rx.try_recv() {
                Ok(Some(img)) => *pic = Pic::Ready(ctx.load_texture(key, img, egui::TextureOptions::LINEAR)),
                Ok(None) | Err(std::sync::mpsc::TryRecvError::Disconnected) => *pic = Pic::None,
                Err(_) => {}
            }
        }
        match pic {
            Pic::Ready(t) => Some(t.id()),
            _ => None,
        }
    }

    fn files_of(&mut self, ctx: &egui::Context, item: &Item, root: Option<PathBuf>) -> Option<&[FileRow]> {
        let sha = item.sha.clone()?;
        let e = self.files.entry(sha.clone()).or_insert_with(|| {
            let (tx, rx) = channel();
            let (repo, c) = (item.repo.clone(), ctx.clone());
            let tools = crate::drives::Tools::current();
            std::thread::spawn(move || {
                let _ = tx.send(commit_files(repo, sha, root, tools));
                c.request_repaint();
            });
            Err(rx)
        });
        if let Err(rx) = e {
            match rx.try_recv() {
                Ok(v) => *e = Ok(v),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => *e = Ok(vec![]),
                Err(_) => {}
            }
        }
        e.as_ref().ok().map(|v| v.as_slice())
    }

    /// Keys the page answers to while it has the keyboard.
    pub fn takes(&self, key: Key) -> bool {
        self.data.as_ref().is_some_and(|d| !d.items.is_empty()) && matches!(key, Key::ArrowUp | Key::ArrowDown | Key::Home | Key::End | Key::PageUp | Key::PageDown | Key::Enter | Key::O)
    }

    pub fn key(&mut self, key: Key, panel: u64, actions: &mut Vec<Action>) {
        let Some(d) = &self.data else { return };
        let n = d.items.len();
        if n == 0 {
            return;
        }
        let page = 8;
        let was = self.sel;
        match key {
            Key::ArrowUp if self.keyed => self.sel = self.sel.saturating_sub(1),
            Key::ArrowDown if self.keyed => self.sel = (self.sel + 1).min(n - 1),
            Key::ArrowUp | Key::ArrowDown => {}
            Key::Home => self.sel = 0,
            Key::End => self.sel = n - 1,
            Key::PageUp => self.sel = self.sel.saturating_sub(page),
            Key::PageDown => self.sel = (self.sel + page).min(n - 1),
            Key::Enter => return self.open(self.sel, panel, actions, false),
            Key::O => return self.open(self.sel, panel, actions, true),
            _ => {}
        }
        // The first arrow shows the cursor where it is; later ones move it.
        self.keyed = true;
        self.reveal = self.reveal || was != self.sel || matches!(key, Key::ArrowUp | Key::ArrowDown);
    }

    /// A commit in its clone's history when there is one; else, or with `web`, on GitHub.
    fn open(&mut self, i: usize, panel: u64, actions: &mut Vec<Action>, web: bool) {
        let Some(d) = &self.data else { return };
        let Some(it) = d.items.get(i) else { return };
        let root = d.clone_of(&it.repo).filter(|r| r.is_dir()).cloned();
        match (root, it.sha.clone(), web) {
            (Some(root), Some(sha), false) => actions.push(Action::Run(Box::new(move |app| {
                app.navigate(panel, root.clone());
                let branch = crate::projects::read(&root).map(|p| p.branch).unwrap_or_default();
                if let Some(p) = app.panels.get_mut(&panel) {
                    let opened = p.inspector.is_none();
                    let mut h = crate::gitlog::History::new(root, branch, opened);
                    h.open_hash(sha);
                    p.inspector.get_or_insert_with(crate::inspector::Inspector::default).show_history(h);
                }
            }))),
            (Some(root), None, false) => actions.push(Action::Navigate { panel, path: root }),
            _ if crate::markdown::is_web(&it.url) => {
                let url = it.url.clone();
                actions.push(Action::Run(Box::new(move |app| {
                    if let Err(e) = crate::mime::open_uri(&url) {
                        app.toast(format!("Couldn't open: {e}"));
                    }
                })));
            }
            _ => {}
        }
    }
}

fn open_web(actions: &mut Vec<Action>, url: String) {
    actions.push(Action::Run(Box::new(move |app| {
        if let Err(e) = crate::mime::open_uri(&url) {
            app.toast(format!("Couldn't open: {e}"));
        }
    })));
}

// ------------------------------------------------------------------ drawing

/// Each achievement's own colour; a badge's ring says its tier.
fn badge_colors(slug: &str) -> Color32 {
    let c = Color32::from_rgb;
    match slug {
        "pull-shark" => c(0x2a, 0x5a, 0x8a),
        "starstruck" => c(0x8a, 0x5a, 0x1a),
        "quickdraw" => c(0x6a, 0x3a, 0x8a),
        "yolo" => c(0x8a, 0x2a, 0x3a),
        "pair-extraordinaire" => c(0x2a, 0x6a, 0x5a),
        "galaxy-brain" => c(0x3a, 0x3a, 0x7a),
        "public-sponsor" | "heart-on-your-sleeve" => c(0x8a, 0x2a, 0x5a),
        "open-sourcerer" => c(0x2a, 0x6a, 0x3a),
        "arctic-code-vault-contributor" => c(0x2a, 0x5a, 0x6a),
        _ => {
            let h = slug.bytes().fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
            c(0x30 + (h % 0x50) as u8, 0x30 + (h / 7 % 0x50) as u8, 0x50 + (h / 49 % 0x40) as u8)
        }
    }
}

fn tier_color(t: u8) -> Color32 {
    match t {
        3 => Color32::from_rgb(0xe8, 0xc0, 0x60),
        2 => Color32::from_rgb(0xa8, 0xb0, 0xb8),
        1 => Color32::from_rgb(0xc2, 0x7a, 0x3a),
        _ => Color32::from_gray(0x70),
    }
}

/// A badge in Maia's own drawing: a disc in the achievement's colour, ringed by its tier,
/// with a mark for what it is.
pub fn paint_badge(p: &egui::Painter, c: Pos2, r: f32, a: &Achievement) {
    p.circle_filled(c, r, badge_colors(&a.slug));
    p.circle_stroke(c, r - 0.8, Stroke::new((r * 0.16).max(1.2), tier_color(a.tier)));
    let w = Color32::from_white_alpha(230);
    let s = r / 10.0;
    let at = |x: f32, y: f32| pos2(c.x + (x - 10.0) * s, c.y + (y - 10.0) * s);
    let line = Stroke::new((1.6 * s).max(1.0), w);
    match a.slug.as_str() {
        "pull-shark" => {
            p.add(Shape::convex_polygon(vec![at(6.0, 13.5), at(11.5, 5.5), at(12.5, 13.5)], w, Stroke::NONE));
            p.line_segment([at(4.5, 14.5), at(15.5, 14.5)], line);
        }
        "starstruck" => crate::ui::star(p, c, r * 0.55, w, true),
        "quickdraw" => {
            p.add(Shape::convex_polygon(vec![at(11.5, 3.5), at(6.5, 11.0), at(10.0, 11.0)], w, Stroke::NONE));
            p.add(Shape::convex_polygon(vec![at(10.0, 9.0), at(13.5, 9.0), at(8.5, 16.5)], w, Stroke::NONE));
        }
        "yolo" => {
            p.line_segment([at(10.0, 15.0), at(10.0, 5.5)], line);
            p.line_segment([at(6.0, 9.5), at(10.0, 5.5)], line);
            p.line_segment([at(14.0, 9.5), at(10.0, 5.5)], line);
        }
        "pair-extraordinaire" => {
            p.circle_filled(at(7.5, 9.0), 2.4 * s, w);
            p.circle_filled(at(12.5, 9.0), 2.4 * s, w.gamma_multiply(0.7));
            p.line_segment([at(4.5, 14.5), at(15.5, 14.5)], line);
        }
        "galaxy-brain" => {
            p.circle_stroke(c, 5.0 * s, line);
            p.circle_filled(at(14.0, 6.5), 1.6 * s, w);
            p.circle_filled(c, 1.8 * s, w);
        }
        "public-sponsor" | "heart-on-your-sleeve" => {
            p.circle_filled(at(8.0, 8.5), 2.6 * s, w);
            p.circle_filled(at(12.0, 8.5), 2.6 * s, w);
            p.add(Shape::convex_polygon(vec![at(5.6, 9.6), at(14.4, 9.6), at(10.0, 15.0)], w, Stroke::NONE));
        }
        "arctic-code-vault-contributor" => {
            for k in 0..3 {
                let a = std::f32::consts::PI / 3.0 * k as f32;
                let d = vec2(a.cos(), a.sin()) * 5.5 * s;
                p.line_segment([c - d, c + d], line);
            }
        }
        _ => {
            let initial: String = a.name.chars().next().map(|ch| ch.to_uppercase().collect()).unwrap_or_default();
            p.text(c, Align2::CENTER_CENTER, initial, FontId::proportional(r * 1.1), w);
        }
    }
}

fn heat(pal: &Palette, level: u8) -> Color32 {
    let none = pal.border.lerp_to_gamma(pal.text, 0.05);
    match level {
        0 => none,
        l => none.lerp_to_gamma(pal.accent, [0.3, 0.52, 0.76, 1.0][(l as usize - 1).min(3)]),
    }
}

fn month_day(date: &str) -> String {
    let m: usize = date.get(5..7).and_then(|s| s.parse().ok()).unwrap_or(1);
    let d: u32 = date.get(8..10).and_then(|s| s.parse().ok()).unwrap_or(1);
    let name = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][m.clamp(1, 12) - 1];
    format!("{name} {d}, {}", date.get(..4).unwrap_or_default())
}

fn joined(t: i64) -> String {
    let date = crate::timefmt::format(SystemTime::UNIX_EPOCH + Duration::from_secs(t.max(0) as u64), "%B %Y");
    format!("Joined {date}")
}

fn centred(ui: &Ui, rect: Rect, lines: &[(&str, bool)], fs: f32, pal: &Palette) {
    let h = lines.len() as f32 * fs * 1.6;
    let mut y = rect.center().y - h / 2.0;
    for (s, main) in lines {
        let g = ui.painter().layout(s.to_string(), FontId::proportional(if *main { fs } else { fs - 1.5 }), if *main { pal.text } else { pal.text_dim }, (rect.width() - 40.0).max(60.0));
        ui.painter().galley(pos2(rect.center().x - g.size().x / 2.0, y), g, pal.text);
        y += fs * 1.6;
    }
}

pub fn page(ui: &mut Ui, rect: Rect, pg: &mut Page, cfg: &Config, pal: &Palette, actions: &mut Vec<Action>, panel: u64) {
    let ctx = ui.ctx().clone();
    pg.poll(&ctx);
    let fs = cfg.font_size;
    let p = ui.painter_at(rect);
    p.rect_filled(rect, CornerRadius::ZERO, pal.panel);
    if pg.data.is_none() {
        match &pg.trouble {
            Some(Trouble::NoGh) => centred(ui, rect, &[("Your GitHub profile shows here once GitHub's gh is installed", true), ("and signed in with gh auth login.", false)], fs, pal),
            Some(Trouble::SignedOut) => centred(ui, rect, &[("Sign in to GitHub with gh auth login", true), ("then come back here.", false)], fs, pal),
            Some(Trouble::Unreachable) => centred(ui, rect, &[("GitHub couldn't be reached", true), ("Refresh (F5) to try again.", false)], fs, pal),
            None => {
                crate::ui::spinner(&p, pos2(rect.center().x, rect.center().y - fs), 6.0, pal.accent, ui.input(|i| i.time));
                ui.ctx().request_repaint_after(Duration::from_millis(16));
                centred(ui, Rect::from_center_size(pos2(rect.center().x, rect.center().y + fs), vec2(rect.width(), fs * 2.0)), &[("Asking GitHub…", false)], fs, pal);
            }
        }
        return;
    }
    let k = fs / 13.0;
    let sp = cfg.spacing;
    // One block in the middle of the panel: the card, then the year and the timeline.
    // A wide panel keeps margins on both sides; a narrow one puts the card's line over
    // the timeline instead.
    let wide = rect.width() >= 520.0 * k;
    let margin = (24.0 * k).round();
    let block_w = (rect.width() - margin * 2.0).min((900.0 * k).round()).max(0.0);
    let x0 = (rect.center().x - block_w / 2.0).round();
    let card_w = if wide { (240.0 * k).round().min(block_w * 0.4) } else { 0.0 };
    let gutter = if wide { (28.0 * k).round() } else { 0.0 };
    let card = Rect::from_min_max(pos2(x0, rect.top()), pos2(x0 + card_w, rect.bottom()));
    let right = if wide { Rect::from_min_max(pos2(card.right() + gutter, rect.top()), pos2(x0 + block_w, rect.bottom())) } else { rect };
    if wide {
        let mid = card.right() + gutter / 2.0;
        p.line_segment([pos2(mid, rect.top() + 16.0 * k), pos2(mid, rect.bottom() - 16.0 * k)], Stroke::new(1.0, pal.border));
        let mut cu = ui.new_child(UiBuilder::new().max_rect(card));
        cu.set_clip_rect(card.intersect(ui.clip_rect()));
        ScrollArea::vertical().id_salt(("gh card", panel)).auto_shrink([false, false]).show(&mut cu, |ui| {
            let inner = ui.available_rect_before_wrap();
            let inner = Rect::from_min_max(inner.min, pos2(inner.left() + card.width() - 4.0, inner.bottom()));
            let h = card_ui(ui, inner, pg, fs, sp, pal, actions, k);
            ui.allocate_space(vec2(card.width() - 4.0, h + 16.0 * k));
        });
    }
    timeline(ui, right, pg, fs, sp, pal, actions, panel, wide, k);
}

/// Text centred on `cx`, wrapped to `w`.
fn centred_text(ui: &Ui, s: &str, font: FontId, color: Color32, w: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple(s.to_string(), font, color, w);
    job.halign = egui::Align::Center;
    ui.fonts_mut(|f| f.layout_job(job))
}

/// The card, each line centred in the column: returns its height.
#[allow(clippy::too_many_arguments)]
fn card_ui(ui: &mut Ui, r: Rect, pg: &mut Page, fs: f32, sp: f32, pal: &Palette, actions: &mut Vec<Action>, k: f32) -> f32 {
    let ctx = ui.ctx().clone();
    let ppp = ctx.pixels_per_point();
    let Some((avatar, faces, orgs)) = pg.data.as_ref().map(|d| (d.avatar.clone(), d.faces.clone(), d.orgs.clone())) else { return 0.0 };
    let cx = r.center().x;
    let w = r.width();
    let mut y = r.top() + 20.0 * k;
    let p = ui.painter().clone();
    let gap = (8.0 + 2.0 * sp) * k;
    // The picture, with a dot when a status is set.
    let big = (104.0 * k).round();
    let pr = Rect::from_min_size(pos2((cx - big / 2.0).round(), y), vec2(big, big));
    match pg.texture(&ctx, &avatar, (big * ppp).round() as u32) {
        Some(t) => {
            p.image(t, pr, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        None => {
            // From the text's colour: in a light theme the raised colour is the page's own.
            p.circle_filled(pr.center(), big / 2.0, pal.text_dim.gamma_multiply(0.16));
        }
    }
    let Some(d) = pg.data.as_deref() else { return 0.0 };
    if !d.status.is_empty() {
        let c = pr.right_bottom() - vec2(big * 0.14, big * 0.14);
        p.circle_filled(c, 10.0 * k, pal.raised);
        p.circle_stroke(c, 10.0 * k, Stroke::new(2.0, pal.panel));
        p.circle_filled(c, 3.5 * k, pal.accent);
    }
    y = pr.bottom() + gap * 1.2;
    let put = |g: std::sync::Arc<egui::Galley>, y: &mut f32, after: f32| {
        let h = g.size().y;
        // Centred galleys are laid out around their own middle.
        let x = if g.job.halign == egui::Align::Center { cx } else { cx - g.size().x / 2.0 };
        p.galley(pos2(x, *y), g, pal.text);
        *y += h + after;
    };
    let title = if d.name.is_empty() { &d.login } else { &d.name };
    put(crate::listing::elided(ui, title, FontId::proportional(fs + 4.0), w, pal.text), &mut y, 2.0);
    let handle = if d.pronouns.is_empty() { format!("@{}", d.login) } else { format!("@{} · {}", d.login, d.pronouns) };
    put(crate::listing::elided(ui, &handle, FontId::proportional(fs - 1.5), w, pal.text_dim), &mut y, gap);
    if !d.status.is_empty() {
        put(centred_text(ui, &d.status, FontId::proportional(fs - 1.0), pal.text_dim, w), &mut y, gap * 0.6);
    }
    if !d.bio.is_empty() {
        put(centred_text(ui, &d.bio, FontId::proportional(fs - 0.5), pal.text.lerp_to_gamma(pal.text_dim, 0.25), w), &mut y, gap);
    }
    // Where, with whom, and a link: each line its icon and words, centred together.
    let small = FontId::proportional(fs - 1.5);
    let icon = (fs - 1.0).round();
    let mut link = None;
    let mut facts = false;
    for (sym, s, is_link) in [("pin", &d.location, false), ("user", &d.company, false), ("globe", &d.website, true)] {
        if s.is_empty() {
            continue;
        }
        facts = true;
        let shown = s.trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/');
        let g = crate::listing::elided(ui, shown, small.clone(), w - icon - 7.0, if is_link { pal.accent } else { pal.text });
        let lw = icon + 7.0 + g.size().x;
        let row = Rect::from_min_size(pos2(cx - lw / 2.0, y), vec2(lw, g.size().y.max(icon)));
        crate::theme::paint_symbol(&p, Rect::from_center_size(pos2(row.left() + icon / 2.0, row.center().y), vec2(icon, icon)), sym, pal.text_dim);
        let gw = g.size().x;
        p.galley(pos2(row.left() + icon + 7.0, row.center().y - g.size().y / 2.0), g, pal.text);
        if is_link {
            let hit = Rect::from_min_size(pos2(row.left() + icon + 7.0, row.top()), vec2(gw, row.height()));
            let resp = ui.interact(hit, ui.id().with("gh site"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
            crate::ui::note_control(ui, hit, "github website");
            if resp.clicked() {
                link = Some(s.clone());
            }
        }
        y += row.height() + 4.0 * k;
    }
    if facts {
        y += gap - 4.0 * k;
    }
    let (login, followers, following, joined_at, langs, achievements) = (d.login.clone(), d.followers, d.following, d.joined, d.langs.clone(), d.achievements.clone());
    // Followers' faces over the counts.
    let face = (22.0 * k).round();
    let shown_faces = faces.len().min(3);
    if shown_faces > 0 {
        let step = face - 6.0 * k;
        let fw = step * (shown_faces - 1) as f32 + face;
        let mut x = cx - fw / 2.0;
        for (i, (_, url)) in faces.iter().enumerate().take(3) {
            let c = pos2(x + face / 2.0, y + face / 2.0);
            p.circle_filled(c, face / 2.0 + 2.0, pal.panel);
            match pg.texture(&ctx, url, (face * ppp).round() as u32) {
                Some(t) => {
                    p.image(t, Rect::from_center_size(c, vec2(face, face)), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
                None => {
                    p.circle_filled(c, face / 2.0, crate::gitlog::lane_color(pal, i + 1));
                }
            }
            x += step;
        }
        y += face + 6.0 * k;
    }
    let num = |n: u32| crate::github::thousands(n);
    let counts = format!("{} followers · {} following", num(followers), num(following));
    let g = crate::listing::elided(ui, &counts, small.clone(), w, pal.text);
    let hit = Rect::from_min_size(pos2(cx - g.size().x / 2.0, y - if shown_faces > 0 { face + 6.0 * k } else { 0.0 }), vec2(g.size().x, g.size().y + if shown_faces > 0 { face + 6.0 * k } else { 0.0 }));
    let gh = g.size().y;
    put(g, &mut y, gap * 1.4);
    let resp = ui.interact(hit, ui.id().with("gh people"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Open your followers on GitHub");
    crate::ui::note_control(ui, hit, "github followers");
    if resp.clicked() {
        link = Some(format!("https://github.com/{login}?tab=followers"));
    }
    let _ = gh;
    // A section's name in the middle of a line across the column.
    let section = |y: &mut f32, name: &str| {
        let g = p.layout_no_wrap(name.to_string(), FontId::proportional(fs - 2.5), pal.text_dim);
        let mid = *y + g.size().y / 2.0;
        let gw = g.size().x;
        p.galley(pos2(cx - gw / 2.0, *y), g, pal.text_dim);
        if gw + 24.0 < w {
            p.line_segment([pos2(r.left(), mid), pos2(cx - gw / 2.0 - 8.0, mid)], Stroke::new(1.0, pal.border));
            p.line_segment([pos2(cx + gw / 2.0 + 8.0, mid), pos2(r.right(), mid)], Stroke::new(1.0, pal.border));
        }
        *y += (fs - 2.5) * 1.3 + 8.0 * k;
    };
    if !achievements.is_empty() {
        section(&mut y, "Achievements");
        let b = (30.0 * k).round();
        let step = b + 8.0 * k;
        let per_row = (((w + 8.0 * k) / step).floor() as usize).max(1);
        for (i, a) in achievements.iter().enumerate() {
            let row = i / per_row;
            let in_row = (achievements.len() - row * per_row).min(per_row);
            let row_w = in_row as f32 * step - 8.0 * k;
            let c = pos2(cx - row_w / 2.0 + (i % per_row) as f32 * step + b / 2.0, y + row as f32 * step + b / 2.0);
            paint_badge(&p, c, b / 2.0, a);
            if a.count > 1 {
                let t = format!("×{}", a.count);
                let g = p.layout_no_wrap(t, FontId::proportional((fs - 4.5).max(8.0)), pal.text);
                let br = Rect::from_min_size(pos2(c.x + b / 2.0 - g.size().x + 1.0, c.y + b / 2.0 - g.size().y + 2.0), g.size() + vec2(5.0, 0.0));
                p.rect_filled(br, CornerRadius::same(6), pal.selected);
                p.galley(br.min + vec2(2.5, 0.0), g, pal.text);
            }
            let hit = Rect::from_center_size(c, vec2(b, b));
            let tip = if a.count > 1 { format!("{} ×{}", a.name, a.count) } else { a.name.clone() };
            let resp = ui.interact(hit, ui.id().with(("gh badge", i)), Sense::click()).on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand);
            crate::ui::note_control(ui, hit, "github achievement");
            if resp.clicked() {
                link = Some(format!("https://github.com/{login}?achievement={}&tab=achievements", a.slug));
            }
        }
        y += achievements.len().div_ceil(per_row) as f32 * step + gap * 1.4 - 8.0 * k;
    }
    if !orgs.is_empty() {
        section(&mut y, "Organisations");
        let o = (24.0 * k).round();
        let step = o + 6.0 * k;
        let fit = (((w + 6.0 * k) / step).floor() as usize).max(1).min(orgs.len());
        let ow = fit as f32 * step - 6.0 * k;
        for (i, (name, url)) in orgs.iter().enumerate().take(fit) {
            let at = Rect::from_min_size(pos2(cx - ow / 2.0 + i as f32 * step, y), vec2(o, o));
            match pg.texture(&ctx, url, (o * ppp).round() as u32) {
                Some(t) => {
                    p.image(t, at, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
                None => {
                    p.circle_filled(at.center(), o / 2.0, pal.text_dim.gamma_multiply(0.16));
                }
            }
            let resp = ui.interact(at, ui.id().with(("gh org", i)), Sense::click()).on_hover_text(name.as_str()).on_hover_cursor(egui::CursorIcon::PointingHand);
            crate::ui::note_control(ui, at, "github organisation");
            if resp.clicked() {
                link = Some(format!("https://github.com/{name}"));
            }
        }
        y += o + gap * 1.4;
    }
    if !langs.is_empty() {
        section(&mut y, "Languages");
        let bar = Rect::from_min_size(pos2(r.left(), y), vec2(w, (6.0 * k).round()));
        let mut x = bar.left();
        for (i, (_, c, share)) in langs.iter().enumerate() {
            let x1 = if i + 1 == langs.len() { bar.right() } else { (x + share * w).min(bar.right()) };
            let round = CornerRadius { nw: if i == 0 { 3 } else { 0 }, sw: if i == 0 { 3 } else { 0 }, ne: if i + 1 == langs.len() { 3 } else { 0 }, se: if i + 1 == langs.len() { 3 } else { 0 } };
            p.rect_filled(Rect::from_min_max(pos2(x, bar.top()), pos2(x1, bar.bottom())), round, *c);
            x = x1;
        }
        y = bar.bottom() + 7.0 * k;
        // Each language with its share: as many as fit on a line, each line centred.
        let font = FontId::proportional(fs - 2.5);
        let items: Vec<(std::sync::Arc<egui::Galley>, Color32)> = langs.iter().map(|(name, c, share)| (p.layout_no_wrap(format!("{name} {}%", (share * 100.0).round().max(1.0)), font.clone(), pal.text_dim), *c)).collect();
        let item_w = |g: &egui::Galley| 9.0 * k + g.size().x;
        let sep = 12.0 * k;
        let mut lines: Vec<Vec<usize>> = vec![vec![]];
        let mut lw = 0.0;
        for (i, (g, _)) in items.iter().enumerate() {
            let need = item_w(g) + if lines.last().is_some_and(|l| !l.is_empty()) { sep } else { 0.0 };
            if lw + need > w && lines.last().is_some_and(|l| !l.is_empty()) {
                lines.push(vec![]);
                lw = 0.0;
            }
            lw += item_w(g) + if lines.last().is_some_and(|l| !l.is_empty()) { sep } else { 0.0 };
            if let Some(l) = lines.last_mut() {
                l.push(i);
            }
        }
        let line_h = font.size * 1.35;
        for l in &lines {
            let total: f32 = l.iter().map(|&i| item_w(&items[i].0)).sum::<f32>() + sep * l.len().saturating_sub(1) as f32;
            let mut x = cx - total / 2.0;
            for &i in l {
                let (g, c) = &items[i];
                p.circle_filled(pos2(x + 3.0 * k, y + line_h / 2.0), 3.0 * k, *c);
                let gw = item_w(g);
                p.galley(pos2(x + 9.0 * k, y + (line_h - g.size().y) / 2.0), g.clone(), pal.text_dim);
                x += gw + sep;
            }
            y += line_h + 2.0 * k;
        }
        y += gap * 1.2;
    }
    if joined_at > 0 {
        put(crate::listing::elided(ui, &joined(joined_at), small, w, pal.text_dim), &mut y, 0.0);
    }
    if let Some(l) = link.filter(|l| crate::markdown::is_web(l)) {
        open_web(actions, l);
    }
    y - r.top()
}

/// 1442 as "1,442": a count read whole, not rounded to "1.4K".
pub fn thousands(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn timeline(ui: &mut Ui, rect: Rect, pg: &mut Page, fs: f32, sp: f32, pal: &Palette, actions: &mut Vec<Action>, panel: u64, wide: bool, k: f32) {
    let ctx = ui.ctx().clone();
    let pad = if wide { (4.0 * k).round() } else { (16.0 * k).round() };
    let mut top = rect.top() + 14.0 * k;
    let inner_w = rect.width() - pad * 2.0;
    let p = ui.painter_at(rect);
    let Some(d) = pg.data.as_deref() else { return };
    // In a narrow panel the card shrinks to a line over the year.
    if !wide {
        let line = format!("{} · {} followers · {} following", if d.name.is_empty() { &d.login } else { &d.name }, thousands(d.followers), thousands(d.following));
        let g = crate::listing::elided(ui, &line, FontId::proportional(fs), inner_w, pal.text);
        let gh = g.size().y;
        p.galley(pos2(rect.left() + pad, top), g, pal.text);
        top += gh + 10.0 * k;
    }
    // The year: weeks across, days down, as wide as there is room for.
    let weeks = (d.first_weekday + d.days.len()).div_ceil(7).max(1);
    let gap = 2.0;
    let cell = ((inner_w - (weeks - 1) as f32 * gap) / weeks as f32).clamp(2.0, (fs * 0.75).round());
    let grid_w = weeks as f32 * (cell + gap) - gap;
    // Centred over the timeline when the cells reach their largest before the edges.
    let grid = Rect::from_min_size(pos2((rect.left() + pad).max(rect.center().x - grid_w / 2.0).round(), top), vec2(grid_w, 7.0 * (cell + gap) - gap));
    let hover = ui.interact(grid, ui.id().with(("gh year", panel)), Sense::hover());
    let mut tip = None;
    for (i, day) in d.days.iter().enumerate() {
        let n = i + d.first_weekday;
        let r = Rect::from_min_size(pos2(grid.left() + (n / 7) as f32 * (cell + gap), grid.top() + (n % 7) as f32 * (cell + gap)), vec2(cell, cell));
        p.rect_filled(r, CornerRadius::same(if cell > 5.0 { 2 } else { 1 }), heat(pal, day.level));
        if hover.hover_pos().is_some_and(|h| r.expand(gap / 2.0).contains(h)) {
            p.rect_stroke(r.expand(1.0), CornerRadius::same(2), Stroke::new(1.0, pal.text), egui::StrokeKind::Outside);
            tip = Some(match day.count {
                0 => format!("No contributions on {}", month_day(&day.date)),
                1 => format!("1 contribution on {}", month_day(&day.date)),
                n => format!("{n} contributions on {}", month_day(&day.date)),
            });
        }
    }
    if let Some(t) = tip {
        hover.on_hover_text(t);
    }
    top = grid.bottom() + 7.0 * k;
    let total = format!("{} contributions in the last year", thousands(d.total));
    let g = crate::listing::elided(ui, &total, FontId::proportional(fs - 1.5), inner_w, pal.text_dim);
    let gh = g.size().y;
    p.galley(pos2(grid.left(), top), g, pal.text_dim);
    top += gh + 14.0 * k;
    let list = Rect::from_min_max(pos2(rect.left() + 6.0 * k, top), pos2(rect.right() - 6.0 * k, rect.bottom()));
    if d.items.is_empty() {
        centred(ui, Rect::from_min_size(list.min, vec2(list.width(), (fs * 4.0).min(list.height()))), &[("Nothing in the last year yet", false)], fs, pal);
        return;
    }
    let n = d.items.len();
    pg.sel = pg.sel.min(n - 1);
    let (font, small, mono) = (FontId::proportional(fs), FontId::proportional(fs - 1.5), FontId::monospace(fs - 2.0));
    let row_h = ((fs * 1.35 + (fs - 1.5) * 1.35) + 7.0 * sp).round();
    let file_h = (fs * 1.45).round();
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let sel = pg.sel;
    // The cursor's row opens to its files, asked for once it is the cursor's.
    let open_item = d.items[sel].clone();
    let root = d.clone_of(&open_item.repo).filter(|r| r.is_dir()).cloned();
    let files: Vec<(String, u32, u32)> = pg.files_of(&ctx, &open_item, root.clone()).map(|v| v.iter().take(8).map(|f| (f.path.clone(), f.add, f.del)).collect()).unwrap_or_default();
    let more = pg.files.get(open_item.sha.as_deref().unwrap_or_default()).and_then(|r| r.as_ref().ok()).map_or(0, |v| v.len().saturating_sub(8));
    let waiting = open_item.sha.is_some() && pg.files.get(open_item.sha.as_deref().unwrap_or_default()).is_some_and(|r| r.is_err());
    let extra_lines = if open_item.kind == Kind::Commit { files.len() + usize::from(more > 0) + usize::from(waiting) + 1 } else { 1 };
    let extra = extra_lines as f32 * file_h + 10.0 * k;
    let Some(d) = pg.data.as_deref() else { return };
    let mut inner = ui.new_child(UiBuilder::new().max_rect(list));
    inner.set_clip_rect(list.intersect(ui.clip_rect()));
    let (mut clicked, mut opened) = (None, None);
    let reveal = std::mem::take(&mut pg.reveal);
    ScrollArea::vertical().id_salt(("gh timeline", panel)).auto_shrink([false, false]).show_viewport(&mut inner, |ui, vp| {
        let total_h = n as f32 * row_h + extra + 8.0;
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), total_h), Sense::hover());
        let y_of = |i: usize| r.top() + i as f32 * row_h + if i > sel { extra } else { 0.0 };
        if reveal {
            ui.scroll_to_rect(Rect::from_min_size(pos2(r.left(), y_of(sel)), vec2(r.width(), row_h + extra)), None);
        }
        let p = ui.painter().clone();
        let lx = r.left() + 10.0 * k;
        let tx = lx + 14.0 * k;
        let dot = ((fs + 1.0) * 0.27).max(3.0);
        let line_col = pal.accent.gamma_multiply(0.35);
        let first = d.items.iter().enumerate().position(|(i, _)| y_of(i) + row_h + if i == sel { extra } else { 0.0 } >= r.top() + vp.min.y).unwrap_or(n);
        for (i, it) in d.items.iter().enumerate().skip(first) {
            let y0 = y_of(i);
            if y0 > r.top() + vp.max.y {
                break;
            }
            let h = row_h + if i == sel { extra } else { 0.0 };
            let rr = Rect::from_min_size(pos2(r.left(), y0), vec2(r.width(), h));
            let resp = ui.interact(rr, ui.id().with(("gh row", i)), Sense::click());
            crate::ui::note_control(ui, rr, "github timeline row");
            let under = if i == sel { pal.row_hover } else if resp.hovered() { pal.hover } else { pal.panel };
            if under != pal.panel {
                p.rect_filled(rr.shrink2(vec2(2.0, 1.0)), CornerRadius::same(5), under);
            }
            if resp.clicked() {
                clicked = Some(i);
            }
            if resp.double_clicked() {
                opened = Some(i);
            }
            let cy = y0 + row_h / 2.0;
            let top_y = if i == 0 { cy } else { y0 };
            let bot_y = if i + 1 == n { cy } else { y0 + h };
            p.line_segment([pos2(lx, top_y), pos2(lx, bot_y)], Stroke::new(1.6, line_col));
            if it.kind == Kind::Commit {
                p.circle_filled(pos2(lx, cy), dot + 1.8, under);
                p.circle_filled(pos2(lx, cy), dot, pal.accent);
            } else {
                p.circle_filled(pos2(lx, cy), dot + 2.6, under);
                p.circle_stroke(pos2(lx, cy), dot + 0.8, Stroke::new(1.8, pal.accent));
            }
            let w = rr.right() - 8.0 - tx;
            if w < 16.0 {
                continue;
            }
            let g = crate::listing::elided(ui, &it.title, font.clone(), w, pal.text);
            let m = small.size * 1.35;
            let ty = cy - (g.size().y + m) / 2.0;
            let gh = g.size().y;
            p.galley(pos2(tx, ty), g, pal.text);
            let short = it.repo.rsplit('/').next().unwrap_or(&it.repo);
            let meta = format!("{short} · {}", crate::gitlog::ago(it.when, now));
            let g = crate::listing::elided(ui, &meta, small.clone(), w, pal.text_dim);
            p.galley(pos2(tx, ty + gh + 1.0 + (small.size * 1.3 - g.size().y) / 2.0), g, pal.text_dim);
            if i != sel {
                continue;
            }
            // Opened: the files, their counts, and where the commit can be seen.
            let mut fy = y0 + row_h + 2.0;
            let right = rr.right() - 12.0 * k;
            if it.kind == Kind::Commit {
                for (path, add, del) in &files {
                    let dg = p.layout_no_wrap(format!("−{del}"), mono.clone(), pal.danger);
                    let ag = p.layout_no_wrap(format!("+{add}"), mono.clone(), crate::gitstatus::GREEN);
                    let counts_w = dg.size().x + ag.size().x + 10.0 * k;
                    let pg_ = crate::listing::elided_tail(ui, path, mono.clone(), (right - tx - counts_w - 12.0 * k).max(20.0), pal.text);
                    let my = fy + (file_h - pg_.size().y) / 2.0;
                    p.galley(pos2(tx, my), pg_, pal.text);
                    let dw = dg.size().x;
                    p.galley(pos2(right - dw, my), dg, pal.danger);
                    let aw = ag.size().x;
                    p.galley(pos2(right - dw - 10.0 * k - aw, my), ag, crate::gitstatus::GREEN);
                    fy += file_h;
                }
                if waiting {
                    crate::ui::spinner(&p, pos2(tx + 6.0, fy + file_h / 2.0), 4.5, pal.accent, ui.input(|i| i.time));
                    ui.ctx().request_repaint_after(Duration::from_millis(16));
                    fy += file_h;
                }
                if more > 0 {
                    let g = p.layout_no_wrap(format!("and {more} more"), small.clone(), pal.text_dim);
                    p.galley(pos2(tx, fy + (file_h - g.size().y) / 2.0), g, pal.text_dim);
                    fy += file_h;
                }
            }
            let short_sha: String = it.sha.as_deref().unwrap_or_default().chars().take(7).collect();
            let place = match &root {
                Some(r) => format!("on this computer at {}", crate::popups::home_short(r)),
                None => "on GitHub".to_string(),
            };
            let line = if short_sha.is_empty() { place } else { format!("{short_sha} · {place}") };
            let g = crate::listing::elided(ui, &line, mono.clone(), right - tx, pal.text_dim);
            p.galley(pos2(tx, fy + (file_h - g.size().y) / 2.0), g, pal.text_dim);
        }
    });
    if let Some(i) = clicked {
        pg.sel = i;
        pg.keyed = true;
    }
    if let Some(i) = opened {
        pg.open(i, panel, actions, false);
    }
}

