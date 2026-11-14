//! Jump: the folders gone to most and last, found by a few letters. Maia counts each
//! folder left and when (`UserData::visits`); with zoxide installed its folders join
//! them. A folder ranks by how well the letters match and by its frecency: how often it
//! was used, weighed by how long ago. Whether each still exists is looked at on workers,
//! one for each mount, so a dead network mount hides only its own folders.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use egui::{CornerRadius, FontId, Rect, Sense, Vec2, pos2, vec2};
use serde::{Deserialize, Serialize};

use crate::app::{Action, App};
use crate::popups as pp;

/// A folder's visits: how many, and the last (seconds since the epoch).
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Visit {
    pub n: u32,
    pub at: u64,
}

/// Folders kept, at most; past it the least used go.
const KEEP: usize = 800;

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Visits weighed by age, as zoxide weighs its own: four times within the hour, twice
/// within the day, half within the week, a quarter after.
pub fn frecency(v: &Visit, now: u64) -> f64 {
    let age = now.saturating_sub(v.at);
    let w = match age {
        0..3600 => 4.0,
        3600..86_400 => 2.0,
        86_400..604_800 => 0.5,
        _ => 0.25,
    };
    v.n as f64 * w
}

/// Count a visit to `p`.
pub fn note(visits: &mut HashMap<PathBuf, Visit>, p: &Path, now: u64) {
    let v = visits.entry(p.to_path_buf()).or_default();
    v.n = v.n.saturating_add(1);
    v.at = now;
    if visits.len() > KEEP + KEEP / 4 {
        let paths: Vec<PathBuf> = visits.keys().cloned().collect();
        let mut all: Vec<(f64, u32)> = paths.iter().enumerate().map(|(i, p)| (visits.get(p).map_or(0.0, |v| frecency(v, now)), i as u32)).collect();
        crate::tab::merge_sort(&mut all, &|a: &(f64, u32), b: &(f64, u32)| b.0.total_cmp(&a.0));
        for (_, i) in all.into_iter().skip(KEEP) {
            visits.remove(&paths[i as usize]);
        }
    }
}

/// zoxide's folders and their scores, from `zoxide query --list --score` ("  12.5 /path").
pub fn parse_zoxide(text: &str) -> Vec<(PathBuf, f64)> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let (s, p) = l.split_once(' ')?;
            let p = p.trim_start();
            (p.starts_with('/')).then(|| Some((PathBuf::from(p), s.parse::<f64>().ok()?))).flatten()
        })
        .collect()
}

enum Msg {
    /// These exist.
    Alive(Vec<PathBuf>),
    Zoxide(Vec<(PathBuf, f64)>),
}

struct Row {
    path: PathBuf,
    /// As shown: "~/Projects/maia".
    text: String,
    frec: f64,
}

pub struct State {
    pub query: String,
    focus: bool,
    sel: usize,
    /// Every folder known, and its frecency: Maia's own, then zoxide's.
    known: Vec<(PathBuf, f64)>,
    alive: HashSet<PathBuf>,
    rx: Receiver<Msg>,
    rows: Vec<Row>,
    /// The highest frecency, for the meters.
    top: f64,
    /// Bumped by whatever the workers send.
    news: u64,
    /// What `rows` were ranked for: the query and the news.
    ranked_for: Option<(String, u64)>,
    /// The folder shown now: no place to jump to, so not offered.
    here: PathBuf,
}

impl State {
    pub fn new(app: &App, ctx: &egui::Context) -> State {
        let t = now();
        let known: Vec<(PathBuf, f64)> = app.ud.visits.iter().map(|(p, v)| (p.clone(), frecency(v, t))).collect();
        let (tx, rx) = channel();
        let paths: Vec<PathBuf> = known.iter().map(|k| k.0.clone()).collect();
        let (tx2, ctx2) = (tx.clone(), ctx.clone());
        let _ = std::thread::Builder::new().name("jump".into()).spawn(move || {
            check(paths, &tx2, &ctx2);
            let Some(tools) = crate::drives::Tools::current().filter(|t| t.has("zoxide")) else { return };
            let mut c = crate::media::tool_cmd(tools.cmd("zoxide"));
            c.args(["query", "--list", "--score"]);
            let Some(out) = crate::media::output_capped(c, Duration::from_secs(3), 8 << 20, &|| false) else { return };
            let z = parse_zoxide(&String::from_utf8_lossy(&out));
            let paths = z.iter().map(|x| x.0.clone()).collect();
            let _ = tx2.send(Msg::Zoxide(z));
            ctx2.request_repaint();
            check(paths, &tx2, &ctx2);
        });
        let here = app.panels.get(&app.active).map(|p| p.path()).unwrap_or_default();
        State { query: String::new(), focus: true, sel: 0, known, alive: HashSet::new(), rx, rows: vec![], top: 1.0, news: 0, ranked_for: None, here }
    }

    fn take(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            self.news += 1;
            match m {
                Msg::Alive(v) => self.alive.extend(v),
                Msg::Zoxide(z) => {
                    // A folder both know keeps the higher score: the two count the same visits.
                    let mut at: HashMap<PathBuf, usize> = self.known.iter().enumerate().map(|(i, k)| (k.0.clone(), i)).collect();
                    for (p, s) in z {
                        match at.get(&p) {
                            Some(&i) => self.known[i].1 = self.known[i].1.max(s),
                            None => {
                                at.insert(p.clone(), self.known.len());
                                self.known.push((p, s));
                            }
                        }
                    }
                }
            }
        }
    }

    fn rank(&mut self) {
        let key = (self.query.clone(), self.news);
        if self.ranked_for.as_ref() == Some(&key) {
            return;
        }
        self.ranked_for = Some(key);
        let q = crate::fuzzy::Query::new(&self.query);
        self.top = self.known.iter().filter(|k| self.alive.contains(&k.0)).map(|k| k.1).fold(0.0, f64::max).max(0.01);
        let mut rows: Vec<Row> = vec![];
        let mut v: Vec<(f64, u32)> = vec![];
        for (p, f) in &self.known {
            if !self.alive.contains(p) || *p == self.here {
                continue;
            }
            let text = pp::home_short(p);
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            // The folder's own name counts for more than the folders above it.
            let m = match (crate::fuzzy::score_ci(&q, &text), crate::fuzzy::score_ci(&q, &name)) {
                (None, None) => continue,
                (a, b) => a.unwrap_or(i32::MIN / 2).max(b.map_or(i32::MIN / 2, |b| b + 40)),
            };
            let score = if q.is_empty() { *f } else { m as f64 + 25.0 * (1.0 + f).ln() };
            v.push((score, rows.len() as u32));
            rows.push(Row { path: p.clone(), text, frec: *f });
        }
        crate::tab::merge_sort(&mut v, &|a: &(f64, u32), b: &(f64, u32)| b.0.total_cmp(&a.0));
        v.truncate(200);
        let mut rows: Vec<Option<Row>> = rows.into_iter().map(Some).collect();
        self.rows = v.into_iter().filter_map(|x| rows[x.1 as usize].take()).collect();
    }
}

/// Which of `paths` exist, sent in batches; each mount on a thread of its own, so a dead
/// one holds up only its own folders.
fn check(paths: Vec<PathBuf>, tx: &Sender<Msg>, ctx: &egui::Context) {
    let mounts: Vec<PathBuf> = std::fs::read_to_string("/proc/self/mountinfo")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split(' ').nth(4).map(|m| PathBuf::from(crate::fsx::unescape_mount(m))))
        .collect();
    let mut by: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    for p in paths {
        let m = mounts.iter().filter(|m| p.starts_with(m)).max_by_key(|m| m.as_os_str().len()).cloned().unwrap_or_default();
        by.entry(m).or_default().push(p);
    }
    for (_, group) in by {
        let (tx, ctx) = (tx.clone(), ctx.clone());
        let _ = std::thread::Builder::new().name("jump check".into()).spawn(move || {
            for chunk in group.chunks(64) {
                let ok: Vec<PathBuf> = chunk.iter().filter(|p| p.is_dir()).cloned().collect();
                if tx.send(Msg::Alive(ok)).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        });
    }
}

/// A folder's path: the folders above it dim, its own name bright, the letters typed lit.
/// Too long for `room`, it loses its start, as the name at its end is what tells it apart.
fn path_galley(ui: &egui::Ui, text: &str, q: &crate::fuzzy::Query, font: FontId, room: f32, (tc, dc, acc): (egui::Color32, egui::Color32, egui::Color32)) -> std::sync::Arc<egui::Galley> {
    let cut = text.rfind('/').map_or(0, |i| i + 1);
    let hits: HashSet<usize> = crate::fuzzy::positions(q, text).map(|(_, p)| p.into_iter().collect()).unwrap_or_default();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let build = |skip: usize| {
        let mut job = egui::text::LayoutJob::default();
        let fmt = |c| egui::TextFormat { font_id: font.clone(), color: c, ..Default::default() };
        if skip > 0 {
            job.append("…", 0.0, fmt(dc));
        }
        let mut buf = [0u8; 4];
        for (i, &(b, ch)) in chars.iter().enumerate().skip(skip) {
            let col = if hits.contains(&i) { acc } else if b < cut { dc } else { tc };
            job.append(ch.encode_utf8(&mut buf), 0.0, fmt(col));
        }
        ui.fonts_mut(|f| f.layout_job(job))
    };
    let g = build(0);
    if g.size().x <= room || chars.len() < 2 {
        return g;
    }
    // Dropped in proportion first, then a few more until it fits.
    let mut skip = ((1.0 - room / g.size().x) * chars.len() as f32).ceil() as usize + 1;
    loop {
        let g = build(skip.min(chars.len() - 1));
        if g.size().x <= room || skip >= chars.len() - 1 {
            return g;
        }
        skip += 2;
    }
}

/// How used a folder is against the most used, in five steps.
fn level(frec: f64, top: f64) -> usize {
    match frec / top {
        r if r >= 0.6 => 5,
        r if r >= 0.3 => 4,
        r if r >= 0.12 => 3,
        r if r >= 0.04 => 2,
        _ => 1,
    }
}

/// Five bars rising left to right, as many lit as the folder is used.
fn meter(p: &egui::Painter, right: f32, cy: f32, lit: usize, on: egui::Color32, off: egui::Color32) {
    for i in 0..5 {
        let h = 4.0 + 2.0 * i as f32;
        let x = right - (5 - i) as f32 * 4.0;
        let r = Rect::from_min_max(pos2(x, cy + 6.0 - h), pos2(x + 2.5, cy + 6.0));
        p.rect_filled(r, CornerRadius::same(1), if i < lit { on } else { off });
    }
}

pub fn open(app: &mut App) {
    let ctx = app.ctx.clone();
    app.popup = pp::Popup::Jump(Box::new(State::new(app, &ctx)));
}

/// The popup. Returns false when it closes.
pub fn popup(app: &mut App, ctx: &egui::Context, st: &mut State) -> bool {
    let pal = app.pal;
    let k = pp::take_keys(ctx);
    if k.esc {
        return false;
    }
    st.take();
    st.rank();
    let n = st.rows.len();
    pp::step(&mut st.sel, n, &k, |_| false);
    let fs = app.cfg.font_size;
    let mut go = st.rows.get(st.sel).filter(|_| k.enter).map(|r| r.path.clone());
    let list_max = pp::fit_list(ctx, 460.0, 90.0);
    let q = crate::fuzzy::Query::new(&st.query);
    let mut reset = false;
    let (_, rect) = pp::centered(ctx, "jump", 620.0, &pal, |ui| {
        let r = pp::search_field(ui, &mut st.query, "Jump to a folder…", &mut st.focus, &pal, crate::theme::Glyph::Search);
        crate::ui::note_control(ui, r.rect, "jump field");
        reset = r.changed();
        let row_h = (fs * 1.35 + 12.0).round();
        if n == 0 {
            ui.add_space(4.0);
            ui.label(egui::RichText::new("No folder like that yet").color(pal.text_dim));
            ui.add_space(4.0);
        } else {
            let mut area = egui::ScrollArea::vertical().id_salt("jump list").max_height(list_max).min_scrolled_height((n as f32 * row_h).min(list_max)).auto_shrink([false, true]);
            if k.up || k.down || k.page_up || k.page_down {
                let most = (n as f32 * row_h - list_max).max(0.0);
                area = area.vertical_scroll_offset((st.sel as f32 * row_h - list_max / 2.0 + row_h / 2.0).clamp(0.0, most));
            }
            area.show_rows(ui, row_h, n, |ui, range| {
                for j in range {
                    let row = &st.rows[j];
                    let on = j == st.sel;
                    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                    if on {
                        ui.painter().rect_filled(r, CornerRadius::same(6), pal.selected);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(r, CornerRadius::same(6), pal.hover);
                    }
                    let (tc, dc, acc) = if on { (pal.on_sel(), pal.on_sel_dim(), pal.on_sel()) } else { (pal.text, pal.text_dim, pal.accent) };
                    let ic = Rect::from_center_size(pos2(r.left() + 20.0, r.center().y), Vec2::splat(16.0));
                    crate::theme::paint_symbol(ui.painter(), ic, "folder", dc);
                    let meter_w = 22.0;
                    let left = ic.right() + 10.0;
                    let room = r.right() - 14.0 - meter_w - 12.0 - left;
                    let g = path_galley(ui, &row.text, &q, FontId::proportional(fs), room, (tc, dc, acc));
                    ui.painter().galley(pos2(left, r.center().y - g.size().y / 2.0), g, tc);
                    meter(ui.painter(), r.right() - 14.0, r.center().y, level(row.frec, st.top), if on { pal.on_sel() } else { pal.accent }, if on { pal.on_sel_dim().gamma_multiply(0.5) } else { pal.border });
                    crate::ui::note_control(ui, r.shrink(1.0), "jump row");
                    if resp.clicked() {
                        go = Some(row.path.clone());
                    }
                }
            });
        }
        pp::hints(ui, &[("Enter", "go"), ("Esc", "close")], &pal);
    });
    if reset {
        st.sel = 0;
    }
    if let Some(path) = go {
        let panel = app.active;
        app.actions.push(Action::Navigate { panel, path });
        return false;
    }
    !pp::clicked_outside(ctx, rect)
}
