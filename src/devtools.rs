//! The developer popups: Search Code with its TODO list, Run Task, and Clean Build Folders.
//! Everything they read comes from worker threads (`codesearch`, `tasks`, `buildclean`);
//! these only draw what has arrived and act on what is picked.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use egui::{self, Align2, Color32, CornerRadius, Event, FontId, Id, Key, Modifiers, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

use crate::app::{Action, App};
use crate::codesearch::{FileHits, Hit, Query, Search};
use crate::popups::{self as pp, Popup};
use crate::theme::{self, Glyph, Palette};

/// A small toggle with a text label: accent-filled while on.
pub(crate) fn chip(ui: &mut Ui, label: &str, on: bool, what: &'static str, tip: &str, pal: &Palette, mono: bool) -> bool {
    let font = if mono { FontId::monospace(12.0) } else { FontId::proportional(12.0) };
    let g = ui.painter().layout_no_wrap(label.to_string(), font.clone(), pal.text);
    let w = (g.size().x + 14.0).max(26.0);
    let (r, resp) = ui.allocate_exact_size(vec2(w, 22.0), Sense::click());
    crate::ui::note_control(ui, r, what);
    let (fill, stroke) = if on {
        (pal.accent.gamma_multiply(0.85), Stroke::NONE)
    } else if resp.hovered() {
        (pal.hover, Stroke::new(1.0, pal.card_line()))
    } else {
        (pal.card(), Stroke::new(1.0, pal.card_line()))
    };
    ui.painter().rect(r, CornerRadius::same(5), fill, stroke, StrokeKind::Inside);
    let col = if on { Color32::WHITE } else if resp.hovered() { pal.text } else { pal.text_dim };
    ui.painter().text(r.center(), Align2::CENTER_CENTER, label, font, col);
    resp.on_hover_text(tip).clicked()
}

/// Take a key with these modifiers, and the text it would type when Alt is among them.
pub(crate) fn take(ctx: &egui::Context, m: Modifiers, k: Key) -> bool {
    ctx.input_mut(|i| {
        let hit = i.consume_key(m, k);
        if hit && m.alt {
            i.events.retain(|e| !matches!(e, Event::Text(_)));
        }
        hit
    })
}

/// Ctrl+Shift+C, which arrives as a copy.
fn take_copy_line(ctx: &egui::Context) -> bool {
    ctx.input_mut(|i| {
        let held = i.modifiers.shift && i.modifiers.command;
        let hit = held && i.events.iter().any(|e| matches!(e, Event::Copy | Event::Key { key: Key::C, pressed: true, .. }));
        if hit {
            i.events.retain(|e| !matches!(e, Event::Copy | Event::Key { key: Key::C, .. }));
        }
        hit
    })
}

/// "3 days ago", "5 weeks ago": how long since `t`.
pub fn ago(t: Option<SystemTime>) -> String {
    let Some(d) = t.and_then(|t| SystemTime::now().duration_since(t).ok()) else { return String::new() };
    let days = d.as_secs() / 86_400;
    let n = |k: u64, unit: &str| format!("{k} {unit}{} ago", if k == 1 { "" } else { "s" });
    match days {
        0 => "today".into(),
        1 => "yesterday".into(),
        2..=13 => n(days, "day"),
        14..=59 => n(days / 7, "week"),
        60..=729 => n(days / 30, "month"),
        _ => n(days / 365, "year"),
    }
}

/// Not changed for a month: a project nobody has built or installed into lately.
pub fn is_stale(t: Option<SystemTime>) -> bool {
    t.and_then(|t| SystemTime::now().duration_since(t).ok()).is_some_and(|d| d.as_secs() >= 30 * 86_400)
}

/// 1234 as "1,234".
pub fn fmt_count(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The folder a developer command works in: the selected folder when it is the only thing
/// selected, else the panel's. None where there are no files to work on (Computer, an
/// archive).
pub fn target_dir(app: &mut App) -> Option<PathBuf> {
    let t = app.tab();
    if crate::tab::is_place(&t.path) || t.archive.is_some() {
        return None;
    }
    let explicit = !t.selected.is_empty();
    let sel: Vec<(PathBuf, bool)> = t.target_entries().iter().map(|e| (e.path.clone(), e.is_dir)).collect();
    match sel.as_slice() {
        [(p, true)] if explicit => Some(p.clone()),
        _ => Some(t.path.clone()),
    }
}

// ------------------------------------------------------------------ Search Code

/// What a search was last run with, so it opens the same way again.
#[derive(Clone)]
struct Last {
    q: Query,
    project: bool,
}

pub struct CodeState {
    /// The folder it was opened in.
    pub start: PathBuf,
    /// From the top of the project; else from `start`.
    pub project: bool,
    pub q: Query,
    focus_query: bool,
    focus_files: bool,
    select_all: bool,
    run: Option<Search>,
    asked: Option<(Query, bool)>,
    typed: Instant,
    pub results: Vec<FileHits>,
    /// The selected line: its file's place in the walk, and its index in the file.
    pub sel: Option<(u64, usize)>,
    collapsed: HashSet<u64>,
    pub error: Option<String>,
    pub root: Option<PathBuf>,
    scroll_to_sel: bool,
    pub capped: bool,
    pub done: bool,
}

pub fn code_search(ctx: &egui::Context, start: PathBuf, todo: bool) -> Popup {
    let last: Last = ctx.data_mut(|d| d.get_temp(Id::new("code search last"))).unwrap_or(Last { q: Query::default(), project: true });
    let mut q = last.q;
    q.todo = todo;
    if todo {
        q.text.clear();
    }
    let select_all = !q.text.is_empty();
    let typed = Instant::now().checked_sub(Duration::from_secs(1)).unwrap_or_else(Instant::now);
    Popup::CodeSearch(Box::new(CodeState { start, project: last.project, q, focus_query: true, focus_files: false, select_all, run: None, asked: None, typed, results: vec![], sel: None, collapsed: HashSet::new(), error: None, root: None, scroll_to_sel: false, capped: false, done: false }))
}

#[derive(Clone, Copy)]
enum Row {
    File(usize),
    /// A line, and whether the lines before and after it are shown with it.
    Hit(usize, usize, bool, bool),
    More(usize),
    Binary,
}

impl CodeState {
    /// A change that should search again at once (a switch, not typing).
    fn now(&mut self) {
        self.typed = Instant::now().checked_sub(Duration::from_secs(1)).unwrap_or_else(Instant::now);
    }

    /// Take in what the search found, and start a new one when the query has changed
    /// (once typing pauses).
    fn pump(&mut self, ctx: &egui::Context) {
        if let Some(r) = &self.run {
            for f in r.take() {
                let at = self.results.partition_point(|x| x.seq < f.seq);
                self.results.insert(at, f);
            }
            if self.root.is_none() {
                self.root = r.root();
            }
            self.capped = r.capped();
            self.done = r.done();
        }
        let key = (self.q.clone(), self.project);
        if self.asked.as_ref() == Some(&key) {
            return;
        }
        if self.typed.elapsed() < Duration::from_millis(150) {
            ctx.request_repaint_after(Duration::from_millis(60));
            return;
        }
        ctx.data_mut(|d| d.insert_temp(Id::new("code search last"), Last { q: Query { todo: false, ..self.q.clone() }, project: self.project }));
        self.asked = Some(key);
        self.run = None;
        self.results.clear();
        self.sel = None;
        self.collapsed.clear();
        self.error = None;
        self.capped = false;
        self.done = true;
        if self.q.todo || !self.q.text.trim().is_empty() {
            match Search::start(self.start.clone(), self.project, &self.q, ctx) {
                Ok(s) => {
                    self.run = Some(s);
                    self.done = false;
                    self.root = None;
                }
                Err(e) => self.error = (!e.is_empty()).then_some(e),
            }
        }
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows = vec![];
        for (fi, f) in self.results.iter().enumerate() {
            rows.push(Row::File(fi));
            if self.collapsed.contains(&f.seq) {
                continue;
            }
            if f.binary {
                rows.push(Row::Binary);
                continue;
            }
            for (hi, h) in f.hits.iter().enumerate() {
                let prev = hi.checked_sub(1).map(|j| &f.hits[j]);
                let next = f.hits.get(hi + 1);
                // A neighbour that is shown anyway (a match itself, or the line after
                // one) isn't shown twice.
                let before = h.before.is_some() && !prev.is_some_and(|p| p.line + 1 == h.line || (p.line + 2 == h.line && p.after.is_some()));
                let after = h.after.is_some() && !next.is_some_and(|n| n.line == h.line + 1);
                rows.push(Row::Hit(fi, hi, before, after));
            }
            if f.total > f.hits.len() {
                rows.push(Row::More(fi));
            }
        }
        rows
    }

    /// The query as it stands has been searched for, to the end.
    pub fn settled(&self) -> bool {
        self.done && self.asked.as_ref().is_some_and(|a| a.0 == self.q && a.1 == self.project)
    }

    /// The selected file and line.
    pub fn selected(&self) -> Option<(&FileHits, &Hit)> {
        let (seq, hi) = self.sel?;
        let f = self.results.iter().find(|f| f.seq == seq)?;
        Some((f, f.hits.get(hi)?))
    }
}

/// One matching line: the text in the theme's code colours (`code`, unless the row is
/// selected) or its text colour, the matches in the accent on a tint of it.
fn line_job(h: &Hit, font: &FontId, col: Color32, mark: Color32, tint: Color32, width: f32, code: Option<&Palette>) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let plain = egui::TextFormat { font_id: font.clone(), color: col, ..Default::default() };
    if h.cut.0 {
        job.append("…", 0.0, plain.clone());
    }
    let t = &h.text;
    let syn = if code.is_some() { &h.syntax[1][..] } else { &[] };
    let mut cuts: Vec<usize> = h.spans.iter().flat_map(|&(s, e)| [s, e]).chain(syn.iter().flat_map(|&(s, e, _)| [s, e])).chain([0, t.len()]).map(|x| x.min(t.len())).collect();
    cuts.sort_unstable();
    cuts.dedup();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if !t.is_char_boundary(a) || !t.is_char_boundary(b) {
            continue;
        }
        let hit = h.spans.iter().any(|&(s, e)| s <= a && b <= e);
        let kind = syn.iter().find(|&&(s, e, _)| s <= a && b <= e).map(|x| x.2);
        let color = match (hit, code, kind) {
            (true, ..) => mark,
            (false, Some(pal), Some(k)) => crate::inspector::syntax_color(k, pal),
            _ => col,
        };
        job.append(&t[a..b], 0.0, egui::TextFormat { color, background: if hit { tint } else { Color32::TRANSPARENT }, ..plain.clone() });
    }
    job.wrap = egui::text::TextWrapping { max_width: width.max(10.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    job
}

/// A line beside a match, a step back from it: its code colours faded, or `col`.
fn context_job(text: &str, spans: &[(usize, usize, crate::syntax::Kind)], font: &FontId, col: Color32, code: Option<&Palette>, width: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let fmt = |color: Color32| egui::TextFormat { font_id: font.clone(), color, ..Default::default() };
    let mut at = 0;
    if let Some(pal) = code {
        for &(s, e, k) in spans {
            if s < at || e > text.len() || !text.is_char_boundary(s) || !text.is_char_boundary(e) {
                continue;
            }
            job.append(&text[at..s], 0.0, fmt(col));
            job.append(&text[s..e], 0.0, fmt(crate::inspector::syntax_color(k, pal).gamma_multiply(0.75)));
            at = e;
        }
    }
    job.append(&text[at..], 0.0, fmt(col));
    job.wrap = egui::text::TextWrapping { max_width: width.max(10.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    job
}

/// Search Code: a query with its switches, the file filter and scope, and the matching
/// lines by file. Enter opens the selected line in the editor and keeps the list; Ctrl+Enter
/// shows it in Maia, with the preview at that line.
pub fn code_search_popup(app: &mut App, ctx: &egui::Context, st: &mut CodeState) -> bool {
    let pal = app.pal;
    let k = pp::take_keys(ctx);
    if k.esc {
        return false;
    }
    for (key, which) in [(Key::C, 0), (Key::W, 1), (Key::R, 2), (Key::T, 3), (Key::P, 4), (Key::A, 5)] {
        if take(ctx, Modifiers::ALT, key) {
            match which {
                0 => st.q.case = !st.q.case,
                1 => st.q.word = !st.q.word,
                2 => st.q.regex = !st.q.regex,
                3 => st.q.todo = !st.q.todo,
                4 => st.project = !st.project,
                _ => st.q.all = !st.q.all,
            }
            st.now();
        }
    }
    let copy_line = take_copy_line(ctx);
    // Typing always goes to the query: a click on a switch or a line takes the focus
    // from it, and gives it back.
    if ctx.memory(|m| m.focused().is_none()) {
        st.focus_query = true;
    }
    if k.tab {
        if ctx.memory(|m| m.has_focus(Id::new("code files"))) {
            st.focus_query = true;
        } else {
            st.focus_files = true;
        }
    }
    st.pump(ctx);
    let rows = st.rows();
    // Up and Down go from line to line, past the file names.
    let nav: Vec<(u64, usize)> = rows.iter().filter_map(|r| match r {
        Row::Hit(fi, hi, ..) => Some((st.results[*fi].seq, *hi)),
        _ => None,
    }).collect();
    let mut at = st.sel.and_then(|s| nav.iter().position(|n| *n == s));
    if at.is_none() && !nav.is_empty() {
        at = Some(0);
        st.sel = Some(nav[0]);
    }
    if let Some(i) = at {
        let d: isize = if k.down { 1 } else if k.up { -1 } else if k.page_down { 10 } else if k.page_up { -10 } else { 0 };
        if d != 0 {
            let i = (i as isize + d).clamp(0, nav.len() as isize - 1) as usize;
            st.sel = Some(nav[i]);
            st.scroll_to_sel = true;
        }
    }
    let mut open: Option<(PathBuf, u32)> = None;
    let mut show: Option<(PathBuf, u32)> = None;
    if let Some((f, h)) = st.selected() {
        if k.enter {
            open = Some((f.path.clone(), h.line));
        }
        if k.ctrl_enter {
            show = Some((f.path.clone(), h.line));
        }
    }
    let fs = app.cfg.font_size;
    let small = fs - 1.5;
    let list_max = pp::fit_list(ctx, 620.0, 200.0);
    let mut toggle_file: Option<u64> = None;
    let shown_root = st.root.clone().or_else(|| crate::gitstatus::repo_for(&st.start).filter(|_| st.project).map(|r| r.root.clone())).unwrap_or_else(|| st.start.clone());
    pp::centered(ctx, "codesearch", 860.0, &pal, |ui| {
        // The query, with case, word and expression switches at its end.
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(20.0, 22.0), Sense::hover());
            theme::paint_symbol(ui.painter(), Rect::from_center_size(r.center(), Vec2::splat(14.0)), "text-search", pal.text_dim);
            let w = ui.available_width() - 3.0 * 32.0 - 6.0;
            let hint = if st.q.todo { "Narrow the TODOs…" } else { "Search code…" };
            let id = Id::new("code query");
            let r = ui.add(egui::TextEdit::singleline(&mut st.q.text).id(id).hint_text(egui::RichText::new(hint).color(pal.text_dim)).frame(crate::ui::bare_edit()).desired_width(w.max(40.0)).font(FontId::proportional(15.0)));
            crate::ui::note_control(ui, r.rect, "code query");
            if st.focus_query {
                r.request_focus();
                st.focus_query = false;
                if std::mem::take(&mut st.select_all) {
                    if let Some(mut s) = egui::TextEdit::load_state(ui.ctx(), id) {
                        let n = st.q.text.chars().count();
                        s.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(n))));
                        s.store(ui.ctx(), id);
                    }
                }
            }
            if r.changed() {
                st.typed = Instant::now();
            }
            let mut flipped = false;
            if chip(ui, "Aa", st.q.case, "match case", "Match case  Alt+C", &pal, false) {
                st.q.case = !st.q.case;
                flipped = true;
            }
            if chip(ui, "ab", st.q.word, "whole word", "Whole words  Alt+W", &pal, true) {
                st.q.word = !st.q.word;
                flipped = true;
            }
            if chip(ui, ".*", st.q.regex, "regex", "Regular expression  Alt+R", &pal, true) {
                st.q.regex = !st.q.regex;
                flipped = true;
            }
            if flipped {
                st.now();
            }
        });
        ui.add_space(2.0);
        let y = ui.cursor().top();
        ui.painter().line_segment([pos2(ui.min_rect().left(), y), pos2(ui.min_rect().right(), y)], Stroke::new(1.0, pal.border));
        ui.add_space(6.0);
        // Which files, and from where.
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            let w = (ui.available_width() * 0.42).clamp(140.0, 340.0);
            let r = ui.add(egui::TextEdit::singleline(&mut st.q.files).id(Id::new("code files")).hint_text(egui::RichText::new("Files: rust, *.toml, src/**, !tests/").color(pal.text_dim)).desired_width(w).font(FontId::proportional(12.5)).margin(egui::Margin::symmetric(6, 3)));
            crate::ui::note_control(ui, r.rect, "code files");
            if st.focus_files {
                r.request_focus();
                st.focus_files = false;
            }
            if r.changed() {
                st.typed = Instant::now();
            }
            let mut changed = false;
            if chip(ui, "Project", st.project, "code scope project", "From the top of the project  Alt+P", &pal, false) && !st.project {
                st.project = true;
                changed = true;
            }
            if chip(ui, "This folder", !st.project, "code scope folder", "From the folder you are in  Alt+P", &pal, false) && st.project {
                st.project = false;
                changed = true;
            }
            if chip(ui, "TODO", st.q.todo, "code todo", "TODO, FIXME, HACK and XXX comments  Alt+T", &pal, false) {
                st.q.todo = !st.q.todo;
                changed = true;
            }
            if chip(ui, "All files", st.q.all, "code all files", "Ignored, hidden and binary files too  Alt+A", &pal, false) {
                st.q.all = !st.q.all;
                changed = true;
            }
            if changed {
                st.now();
            }
        });
        ui.add_space(4.0);
        // Where it looked and what it found.
        let files = st.results.len();
        let lines: usize = st.results.iter().map(|f| f.total).sum();
        let plural = |n: usize, one: &str, many: &str| format!("{} {}", fmt_count(n), if n == 1 { one } else { many });
        let mut status = format!("In {}", pp::home_short(&shown_root));
        if st.q.todo && files > 0 {
            let mut tags = [0u32; 4];
            for f in &st.results {
                for (t, n) in tags.iter_mut().zip(f.tags) {
                    *t += n;
                }
            }
            let parts: Vec<String> = crate::codesearch::TODO_TAGS.iter().zip(tags).filter(|(_, n)| *n > 0).map(|(t, n)| format!("{n} {t}")).collect();
            let what = if parts.is_empty() { plural(lines, "comment", "comments") } else { parts.join(" · ") };
            status.push_str(&format!(" · {what} in {}", plural(files, "file", "files")));
        } else if files > 0 {
            status.push_str(&format!(" · {} in {}", plural(lines, "line", "lines"), plural(files, "file", "files")));
        } else if st.done && st.run.is_some() {
            let n = st.run.as_ref().map_or(0, |r| r.searched());
            status.push_str(&format!(" · nothing in {}", plural(n, "file", "files")));
        }
        if st.capped {
            status.push_str(&format!(" · stopped at {} lines", fmt_count(crate::codesearch::MAX_MATCHES)));
        } else if st.run.is_some() && !st.done {
            status.push_str(" · searching…");
        }
        match &st.error {
            Some(e) => ui.add(egui::Label::new(egui::RichText::new(e).size(small).color(pal.danger)).wrap()),
            None => ui.add(egui::Label::new(egui::RichText::new(status).size(small).color(pal.text_dim)).truncate()),
        };
        // The lines, by file. Only what is in view is laid out.
        let mono = FontId::monospace(fs - 0.5);
        let mono_small = FontId::monospace(small - 0.5);
        let (mono_h, ctx_h, cw) = ui.fonts_mut(|f| (f.row_height(&mono), f.row_height(&mono_small), f.glyph_width(&mono_small, '0')));
        let file_h = (fs * 1.35 + 12.0).round();
        let height = |r: &Row| match r {
            Row::File(_) => file_h,
            Row::Hit(_, _, b, a) => (mono_h + 6.0 + (*b as u8 + *a as u8) as f32 * (ctx_h + 1.0)).round(),
            Row::More(_) | Row::Binary => (ctx_h + 10.0).round(),
        };
        let mut tops = Vec::with_capacity(rows.len());
        let mut total = 0.0;
        for r in &rows {
            tops.push(total);
            total += height(r);
        }
        if !rows.is_empty() {
            ui.add_space(2.0);
            let sel = st.sel;
            let scroll = std::mem::take(&mut st.scroll_to_sel);
            egui::ScrollArea::vertical().id_salt("code results").max_height(list_max).min_scrolled_height(total.min(list_max)).auto_shrink([false, true]).show_viewport(ui, |ui, vp| {
                let w = ui.available_width();
                let (area, _) = ui.allocate_exact_size(vec2(w, total), Sense::hover());
                // The selected line comes into view even when it is far outside it.
                if scroll {
                    let at = rows.iter().position(|r| matches!(r, Row::Hit(fi, hi, ..) if sel == Some((st.results[*fi].seq, *hi))));
                    if let Some(i) = at {
                        ui.scroll_to_rect(Rect::from_min_size(pos2(area.left(), area.top() + tops[i]), vec2(w, height(&rows[i]))), None);
                    }
                }
                let first = tops.partition_point(|t| *t <= vp.min.y).saturating_sub(1);
                for (i, row) in rows.iter().enumerate().skip(first) {
                    let top = tops[i];
                    if top > vp.max.y {
                        break;
                    }
                    let r = Rect::from_min_size(pos2(area.left(), area.top() + top), vec2(w, height(row)));
                    match *row {
                        Row::File(fi) => {
                            let f = &st.results[fi];
                            let resp = ui.interact(r, Id::new(("code file", f.seq)), Sense::click());
                            let p = ui.painter();
                            if resp.hovered() {
                                p.rect_filled(r.shrink2(vec2(0.0, 1.0)), CornerRadius::same(6), pal.hover);
                            }
                            if resp.clicked() {
                                toggle_file = Some(f.seq);
                            }
                            let open = !st.collapsed.contains(&f.seq);
                            let cy = r.center().y;
                            theme::paint_glyph(p, Rect::from_center_size(pos2(r.left() + 10.0, cy), Vec2::splat(10.0)), if open { Glyph::ChevronDown } else { Glyph::ChevronRight }, pal.text_dim);
                            let name = f.rel.rsplit('/').next().unwrap_or(&f.rel);
                            let ext = name.rsplit_once('.').map(|x| x.1.to_ascii_lowercase()).unwrap_or_default();
                            let ic = Rect::from_center_size(pos2(r.left() + 30.0, cy), Vec2::splat(16.0));
                            theme::paint_icon(p, ic, theme::categorize(&ext, false, false), &ext, false);
                            let count = if f.binary { "binary".to_string() } else { fmt_count(f.total) };
                            let chip_w = p.layout_no_wrap(count.clone(), FontId::proportional(11.0), pal.text_dim).size().x + 16.0;
                            let x = ic.right() + 8.0;
                            let mut job = egui::text::LayoutJob::default();
                            let dir = &f.rel[..f.rel.len() - name.len()];
                            job.append(dir, 0.0, egui::TextFormat { font_id: FontId::proportional(fs), color: pal.text_dim, ..Default::default() });
                            job.append(name, 0.0, egui::TextFormat { font_id: FontId::proportional(fs), color: pal.text, ..Default::default() });
                            job.wrap = egui::text::TextWrapping { max_width: (r.right() - chip_w - 12.0 - x).max(20.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
                            let g = p.layout_job(job);
                            p.galley(pos2(x, cy - g.size().y / 2.0), g, pal.text);
                            crate::ui::paint_chip(p, pos2(r.right() - 6.0, cy), &count, &pal);
                        }
                        Row::Hit(fi, hi, before, after) => {
                            let f = &st.results[fi];
                            let h = &f.hits[hi];
                            let on = sel == Some((f.seq, hi));
                            let resp = ui.interact(r, Id::new(("code hit", f.seq, hi)), Sense::click());
                            let p = ui.painter();
                            if on {
                                p.rect_filled(r.shrink2(vec2(0.0, 1.0)), CornerRadius::same(6), pal.selected);
                            } else if resp.hovered() {
                                p.rect_filled(r.shrink2(vec2(0.0, 1.0)), CornerRadius::same(6), pal.hover);
                            }
                            if resp.clicked() {
                                st.sel = Some((f.seq, hi));
                                if ui.input(|i| i.modifiers.command) {
                                    show = Some((f.path.clone(), h.line));
                                } else {
                                    open = Some((f.path.clone(), h.line));
                                }
                            }
                            let (text, dim, mark) = if on { (pal.on_sel(), pal.on_sel_dim(), pal.on_sel()) } else { (pal.text, pal.text_dim, pal.accent) };
                            // Line numbers in a gutter as wide as the file's longest.
                            let digits = f.hits.last().map_or(1, |l| (l.line + 1).to_string().len()) as f32;
                            let gutter = 30.0 + digits * cw;
                            let x = r.left() + gutter + 10.0;
                            let room = r.right() - 8.0 - x;
                            let mut y = r.top() + 3.0;
                            // Code colours on the list's background; a selected row keeps its own.
                            let code = (!on).then_some(&pal);
                            let context = |y: f32, n: u32, s: &str, spans: &[(usize, usize, crate::syntax::Kind)]| {
                                p.text(pos2(r.left() + gutter, y + 0.5), Align2::RIGHT_TOP, n.to_string(), mono_small.clone(), dim.gamma_multiply(0.7));
                                let g = p.layout_job(context_job(s, spans, &mono_small, dim.gamma_multiply(0.85), code, room));
                                p.galley(pos2(x, y + 0.5), g, dim);
                            };
                            if before {
                                context(y, h.line - 1, h.before.as_deref().unwrap_or(""), &h.syntax[0]);
                                y += ctx_h + 1.0;
                            }
                            p.text(pos2(r.left() + gutter, y + (mono_h - ctx_h) / 2.0), Align2::RIGHT_TOP, h.line.to_string(), mono_small.clone(), dim);
                            let tint = if on { pal.on_sel().gamma_multiply(0.2) } else { pal.accent.gamma_multiply(0.22) };
                            let g = p.layout_job(line_job(h, &mono, text, mark, tint, room, code));
                            p.galley(pos2(x, y), g, text);
                            y += mono_h + 1.0;
                            if after {
                                context(y, h.line + 1, h.after.as_deref().unwrap_or(""), &h.syntax[2]);
                            }
                        }
                        Row::More(fi) => {
                            let f = &st.results[fi];
                            let n = f.total - f.hits.len();
                            let s = format!("{} more {} in this file", fmt_count(n), if n == 1 { "line" } else { "lines" });
                            ui.painter().text(pos2(r.left() + 40.0, r.center().y), Align2::LEFT_CENTER, s, FontId::proportional(small), pal.text_dim);
                        }
                        Row::Binary => {
                            ui.painter().text(pos2(r.left() + 40.0, r.center().y), Align2::LEFT_CENTER, "A binary file: it matches, but has no lines to show", FontId::proportional(small), pal.text_dim);
                        }
                    }
                }
            });
        }
        pp::hints(ui, &[("Enter", "open in editor"), ("Ctrl+Enter", "show in Maia"), ("Ctrl+Shift+C", "copy path:line"), ("Tab", "files"), ("Esc", "close")], &pal);
    });
    if let Some(seq) = toggle_file {
        if !st.collapsed.remove(&seq) {
            st.collapsed.insert(seq);
            if st.sel.is_some_and(|s| s.0 == seq) {
                st.sel = None;
            }
        }
    }
    if copy_line {
        if let Some((f, h)) = st.selected() {
            let root = match (&st.root, st.project) {
                (Some(r), true) => r.clone(),
                _ => crate::project::root(&st.start),
            };
            let s = crate::project::with_line(&crate::project::relative(&f.path, &root), h.line);
            ctx.copy_text(s.clone());
            app.toast(format!("Copied {s}"));
        }
    }
    if let Some((path, line)) = open {
        app.open_in_editor(&path, Some(line));
    }
    if let Some((path, line)) = show {
        app.show_at_line(path, line);
        return false;
    }
    true
}

// ------------------------------------------------------------------ Run Task

pub struct TasksState {
    pub start: PathBuf,
    pub query: String,
    focus: bool,
    pub sel: usize,
    job: Option<mpsc::Receiver<Vec<crate::tasks::Group>>>,
    pub groups: Option<Vec<crate::tasks::Group>>,
}

pub fn tasks(ctx: &egui::Context, start: PathBuf) -> Popup {
    let (tx, rx) = mpsc::channel();
    let (dir, c) = (start.clone(), ctx.clone());
    std::thread::spawn(move || {
        let _ = tx.send(crate::tasks::detect(&dir));
        c.request_repaint();
    });
    Popup::Tasks(TasksState { start, query: String::new(), focus: true, sel: 0, job: Some(rx), groups: None })
}

/// Run Task: the project's tasks by manifest, filtered as typed; Enter runs one in the
/// terminal, in its manifest's folder.
pub fn tasks_popup(app: &mut App, ctx: &egui::Context, st: &mut TasksState) -> bool {
    let pal = app.pal;
    let k = pp::take_keys(ctx);
    if k.esc {
        return false;
    }
    if let Some(rx) = &st.job {
        if let Ok(g) = rx.try_recv() {
            st.groups = Some(g);
            st.job = None;
        }
    }
    enum TRow {
        Head(usize),
        Task(usize, usize),
    }
    let q = crate::fuzzy::Query::new(&st.query);
    let groups: &[crate::tasks::Group] = st.groups.as_deref().unwrap_or_default();
    let mut rows: Vec<TRow> = vec![];
    for (gi, g) in groups.iter().enumerate() {
        let hits: Vec<usize> = g.tasks.iter().enumerate().filter(|(_, t)| q.is_empty() || crate::fuzzy::score(&q, &t.name.to_lowercase()).is_some() || crate::fuzzy::score(&q, &format!("{} {}", g.source, t.name).to_lowercase()).is_some()).map(|(i, _)| i).collect();
        if !hits.is_empty() {
            rows.push(TRow::Head(gi));
            rows.extend(hits.into_iter().map(|ti| TRow::Task(gi, ti)));
        }
    }
    pp::step(&mut st.sel, rows.len(), &k, |i| matches!(rows.get(i), Some(TRow::Head(_))));
    let mut run: Option<(usize, usize)> = match rows.get(st.sel) {
        Some(TRow::Task(g, t)) if k.enter => Some((*g, *t)),
        _ => None,
    };
    let fs = app.cfg.font_size;
    let list_max = pp::fit_list(ctx, 460.0, 120.0);
    let mut reset_sel = false;
    pp::centered(ctx, "tasks", 640.0, &pal, |ui| {
        let r = pp::search_field(ui, &mut st.query, "Run a task…", &mut st.focus, &pal, Glyph::Search);
        crate::ui::note_control(ui, r.rect, "task field");
        reset_sel = r.changed();
        let row_h = (fs * 1.35 + 10.0).round();
        let head_h = (fs * 1.35 + 8.0).round();
        let rows_h: f32 = rows.iter().map(|r| if matches!(r, TRow::Head(_)) { head_h } else { row_h }).sum();
        egui::ScrollArea::vertical().max_height(list_max).min_scrolled_height(rows_h.min(list_max)).auto_shrink([false, true]).show(ui, |ui| {
            for (i, row) in rows.iter().enumerate() {
                match row {
                    TRow::Head(gi) => {
                        let g = &groups[*gi];
                        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), head_h), Sense::hover());
                        let mut text = g.source.clone();
                        if g.dir != st.start {
                            text.push_str(&format!("  ·  {}", pp::home_short(&g.dir)));
                        }
                        let gl = crate::listing::elided(ui, &text, FontId::proportional(fs - 1.5), r.width() - 20.0, pal.text_dim);
                        ui.painter().galley(pos2(r.left() + 10.0, r.bottom() - 4.0 - gl.size().y), gl, pal.text_dim);
                    }
                    TRow::Task(gi, ti) => {
                        let t = &groups[*gi].tasks[*ti];
                        let on = i == st.sel;
                        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                        if on {
                            ui.painter().rect_filled(r, CornerRadius::same(6), pal.selected);
                        } else if resp.hovered() {
                            ui.painter().rect_filled(r, CornerRadius::same(6), pal.hover);
                        }
                        if on && (k.up || k.down || k.page_up || k.page_down) {
                            ui.scroll_to_rect(r, None);
                        }
                        let (tc, dc) = if on { (pal.on_sel(), pal.on_sel_dim()) } else { (pal.text, pal.text_dim) };
                        let name = crate::listing::elided(ui, &t.name, FontId::proportional(fs), r.width() * 0.45, tc);
                        let nw = name.size().x;
                        ui.painter().galley(pos2(r.left() + 12.0, r.center().y - name.size().y / 2.0), name, tc);
                        if !t.detail.is_empty() {
                            let x = r.left() + 12.0 + nw + 14.0;
                            // A command in monospace; a recipe's or target's comment as prose.
                            let prose = matches!(groups[*gi].source.as_str(), "justfile" | "Justfile" | ".justfile" | "Makefile" | "makefile" | "GNUmakefile" | "Taskfile.yml" | "Taskfile.yaml" | "taskfile.yml");
                            let font = if prose { FontId::proportional(fs - 1.5) } else { FontId::monospace(fs - 2.0) };
                            let d = crate::listing::elided(ui, &t.detail, font, r.right() - 10.0 - x, dc);
                            ui.painter().galley(pos2(x, r.center().y - d.size().y / 2.0), d, dc);
                        }
                        if resp.clicked() {
                            run = Some((*gi, *ti));
                        }
                    }
                }
            }
            let dim = |ui: &mut Ui, s: &str| {
                ui.add_space(4.0);
                ui.add(egui::Label::new(egui::RichText::new(s).color(pal.text_dim)).wrap());
            };
            match &st.groups {
                None => dim(ui, "Reading the project…"),
                Some(g) if g.is_empty() => dim(ui, "No tasks here: no package.json, Cargo.toml, Makefile, justfile, pyproject.toml or other manifest that has them."),
                Some(_) if rows.is_empty() => dim(ui, "No task by that name"),
                _ => {}
            }
        });
        pp::hints(ui, &[("Enter", "run in terminal"), ("Esc", "close")], &pal);
    });
    if reset_sel {
        st.sel = 0;
    }
    if let Some((gi, ti)) = run {
        let (g, t) = (&groups[gi], &groups[gi].tasks[ti]);
        let res = crate::mime::run_in_terminal(&g.dir, &crate::mime::held(&t.argv));
        let msg = match res {
            Ok(()) => format!("Running {} in the terminal", t.argv.join(" ")),
            Err(e) => format!("Couldn't start a terminal: {e}"),
        };
        app.toast(msg);
        return false;
    }
    true
}

// ------------------------------------------------------------------ Clean Build Folders

pub struct CleanState {
    pub root: PathBuf,
    scan: Option<crate::buildclean::Scan>,
    pub items: Vec<crate::buildclean::Found>,
    pub checked: HashSet<PathBuf>,
    pub sel: usize,
    pub done: bool,
    sorted: bool,
}

pub fn clean(ctx: &egui::Context, root: PathBuf) -> Popup {
    let scan = crate::buildclean::Scan::start(root.clone(), ctx);
    Popup::CleanBuilds(CleanState { root, scan: Some(scan), items: vec![], checked: HashSet::new(), sel: 0, done: false, sorted: false })
}

/// Clean Build Folders: what the scan finds, with sizes as they are added up; the checked
/// ones go to the trash once confirmed.
pub fn clean_popup(app: &mut App, ctx: &egui::Context, st: &mut CleanState) -> bool {
    use crate::buildclean::Msg;
    let pal = app.pal;
    let k = pp::take_keys(ctx);
    if k.esc {
        return false;
    }
    if let Some(s) = &st.scan {
        while let Ok(m) = s.rx.try_recv() {
            match m {
                Msg::Found(f) => st.items.push(f),
                Msg::Size(p, n) => {
                    if let Some(f) = st.items.iter_mut().find(|f| f.path == p) {
                        f.size = Some(n);
                    }
                }
                Msg::Done => st.done = true,
            }
        }
    }
    if st.done && !st.sorted {
        // Biggest first once every size is in; the selection stays on its folder.
        let at = st.items.get(st.sel).map(|f| f.path.clone());
        st.items.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.path.cmp(&b.path)));
        st.sel = at.and_then(|p| st.items.iter().position(|f| f.path == p)).unwrap_or(0);
        st.sorted = true;
        st.scan = None;
    }
    pp::step(&mut st.sel, st.items.len(), &k, |_| false);
    if take(ctx, Modifiers::NONE, Key::Space) {
        if let Some(f) = st.items.get(st.sel) {
            if !st.checked.remove(&f.path) {
                st.checked.insert(f.path.clone());
            }
        }
    }
    if take(ctx, Modifiers::COMMAND, Key::A) {
        if st.checked.len() == st.items.len() {
            st.checked.clear();
        } else {
            st.checked = st.items.iter().map(|f| f.path.clone()).collect();
        }
    }
    let fs = app.cfg.font_size;
    let small = fs - 1.5;
    let mut trash = k.enter && !st.checked.is_empty();
    let mut delete = false;
    let mut stale = false;
    let mut cancel = false;
    let mut toggle: Option<usize> = None;
    let list_max = pp::fit_list(ctx, 440.0, 190.0);
    let chosen = st.items.iter().filter(|f| st.checked.contains(&f.path)).count();
    let chosen_size: u64 = st.items.iter().filter(|f| st.checked.contains(&f.path)).filter_map(|f| f.size).sum();
    let all_size: u64 = st.items.iter().filter_map(|f| f.size).sum();
    pp::centered(ctx, "clean builds", 700.0, &pal, |ui| {
        let sub = format!("In {}. A build or an install makes checked folders again.", pp::home_short(&st.root));
        pp::dialog_title(ui, "Clean build folders", Some(&sub), &pal);
        ui.add_space(8.0);
        let row_h = (fs * 1.35 + small * 1.35 + 14.0).round();
        let rows_h = st.items.len() as f32 * row_h;
        if !st.items.is_empty() {
            egui::ScrollArea::vertical().id_salt("clean list").max_height(list_max).min_scrolled_height(rows_h.min(list_max)).auto_shrink([false, true]).show(ui, |ui| {
                for (i, f) in st.items.iter().enumerate() {
                    let on = i == st.sel;
                    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                    if on {
                        ui.painter().rect_filled(r, CornerRadius::same(6), pal.selected);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(r, CornerRadius::same(6), pal.hover);
                    }
                    if on && (k.up || k.down || k.page_up || k.page_down) {
                        ui.scroll_to_rect(r, None);
                    }
                    if resp.clicked() {
                        toggle = Some(i);
                    }
                    let (tc, dc) = if on { (pal.on_sel(), pal.on_sel_dim()) } else { (pal.text, pal.text_dim) };
                    let cb = Rect::from_center_size(pos2(r.left() + 16.0, r.center().y), Vec2::splat(14.0));
                    crate::popups::checkbox_glyph(ui.painter(), cb, st.checked.contains(&f.path), &pal);
                    crate::ui::note_control(ui, cb, "clean check");
                    let size = f.size.map_or("…".to_string(), crate::fsx::fmt_size);
                    let sg = ui.painter().layout_no_wrap(size, FontId::proportional(fs), tc);
                    let sw = sg.size().x;
                    ui.painter().galley(pos2(r.right() - 12.0 - sw, r.center().y - sg.size().y / 2.0), sg, tc);
                    let x = cb.right() + 12.0;
                    let room = r.right() - 24.0 - sw - x;
                    let rel = crate::project::relative(&f.path, &st.root);
                    let g = crate::listing::elided_tail(ui, &rel, FontId::proportional(fs), room, tc);
                    ui.painter().galley(pos2(x, r.top() + 6.0), g, tc);
                    let when = ago(f.modified);
                    let sub = if when.is_empty() { f.kind.to_string() } else { format!("{} · changed {when}", f.kind) };
                    let g = crate::listing::elided(ui, &sub, FontId::proportional(small), room, dc);
                    ui.painter().galley(pos2(x, r.top() + 8.0 + fs * 1.35), g, dc);
                }
            });
        }
        ui.add_space(6.0);
        let status = if !st.done {
            format!("Looking… {} found", st.items.len())
        } else if st.items.is_empty() {
            "No build folders here".to_string()
        } else {
            format!("{} {}, {}", st.items.len(), if st.items.len() == 1 { "folder" } else { "folders" }, crate::fsx::fmt_size(all_size))
        };
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(status).size(small).color(pal.text_dim));
            // The folders of projects nobody has built in a month: the usual ones to go.
            let old = st.items.iter().filter(|f| is_stale(f.modified)).count();
            if st.done && old > 0 {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let b = ui.add(egui::Button::new(egui::RichText::new(format!("Check the {old} untouched for a month")).size(small).color(pal.accent)).frame(false));
                    crate::ui::note_control(ui, b.rect, "clean stale");
                    stale = b.on_hover_text("Ctrl+M").clicked();
                });
            }
        });
        pp::footer(ui, &[("Space", "check"), ("Ctrl+A", "all"), ("Ctrl+M", "a month old"), ("Enter", "move to trash"), ("Shift+Delete", "delete"), ("Esc", "close")], &pal, |ui| {
            let label = if chosen == 0 { "Move to Trash".to_string() } else { format!("Move {} to Trash", crate::fsx::fmt_size(chosen_size)) };
            let b = pp::primary_button(ui, &label, chosen > 0, &pal);
            crate::ui::note_control(ui, b.rect, "clean trash");
            trash |= b.clicked();
            // The trash keeps what it holds on the disk: deleting is what frees the room.
            let d = ui.add_enabled(chosen > 0, egui::Button::new(egui::RichText::new("Delete").color(if chosen > 0 { pal.danger } else { pal.text_dim })).fill(pal.card()).stroke(Stroke::new(1.0, pal.card_line())).min_size(vec2(72.0, 0.0)));
            crate::ui::note_control(ui, d.rect, "clean delete");
            delete |= d.on_hover_text("Delete for good, freeing the space now (Shift+Delete)").clicked();
            let c = pp::plain_button(ui, "Cancel", &pal);
            crate::ui::note_control(ui, c.rect, "clean cancel");
            cancel = c.clicked();
        });
    });
    if let Some(i) = toggle {
        st.sel = i;
        if let Some(f) = st.items.get(i) {
            if !st.checked.remove(&f.path) {
                st.checked.insert(f.path.clone());
            }
        }
    }
    if stale || (take(ctx, Modifiers::COMMAND, Key::M) && st.done) {
        st.checked = st.items.iter().filter(|f| is_stale(f.modified)).map(|f| f.path.clone()).collect();
    }
    if cancel {
        return false;
    }
    if (delete || take(ctx, Modifiers::SHIFT, Key::Delete)) && !st.checked.is_empty() {
        let paths: Vec<PathBuf> = st.items.iter().filter(|f| st.checked.contains(&f.path)).map(|f| f.path.clone()).collect();
        let size: u64 = st.items.iter().filter(|f| st.checked.contains(&f.path)).filter_map(|f| f.size).sum();
        let what = if paths.len() == 1 { format!("“{}”", crate::project::relative(&paths[0], &st.root)) } else { format!("{} folders", paths.len()) };
        app.popup = Popup::confirm(format!("Delete {what} ({}) for good? This can't be undone; a build or an install makes them again.", crate::fsx::fmt_size(size)), "Delete", Box::new(move |app: &mut App| {
            app.actions.push(Action::StartJob(crate::ops::OpKind::Delete { paths }));
        }));
        return false;
    }
    if trash && !st.checked.is_empty() {
        let paths: Vec<PathBuf> = st.items.iter().filter(|f| st.checked.contains(&f.path)).map(|f| f.path.clone()).collect();
        let size: u64 = st.items.iter().filter(|f| st.checked.contains(&f.path)).filter_map(|f| f.size).sum();
        let what = if paths.len() == 1 { format!("“{}”", crate::project::relative(&paths[0], &st.root)) } else { format!("{} folders", paths.len()) };
        app.popup = Popup::confirm(format!("Move {what} ({}) to the trash?", crate::fsx::fmt_size(size)), "Move to Trash", Box::new(move |app: &mut App| {
            app.actions.push(Action::StartJob(crate::ops::OpKind::Trash { paths }));
        }));
        return false;
    }
    true
}
