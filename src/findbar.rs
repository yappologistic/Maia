//! Find in a preview: the bar along its bottom and the matches it marks in the text.

use egui::{CornerRadius, FontId, Rect, Sense, Stroke, Ui, UiBuilder, Vec2, pos2, vec2};

use crate::theme::{self, Glyph, Palette, paint_glyph};

pub const BAR_H: f32 = 30.0;
/// Matches marked; past it the count says "+".
const MAX: usize = 50_000;

/// A match: its line, and where in the line it starts and ends (bytes).
pub type Hit = (u32, u32, u32);

pub struct Finder {
    pub query: String,
    /// The replacement, while the editor's Replace row is open.
    pub replace: Option<String>,
    /// Put the keyboard in the field (and select what is in it) on the next frame.
    pub focus: bool,
    pub hits: Vec<Hit>,
    /// The query and the text the hits were found for.
    key: (String, u64, usize),
    pub cur: usize,
    /// Scroll the current match into view on the next frame.
    pub reveal: bool,
    /// The field had the keyboard last frame (egui lets go of it on Escape before the
    /// app sees the key, so it is remembered here).
    pub focused: bool,
    opened: Option<f64>,
}

#[derive(PartialEq, Eq)]
pub enum Out {
    None,
    Next,
    Prev,
    Close,
    Replace,
    ReplaceAll,
}

impl Finder {
    pub fn new(query: String) -> Finder {
        Finder { query, replace: None, focus: true, hits: vec![], key: (String::new(), u64::MAX, 0), cur: 0, reveal: false, focused: false, opened: None }
    }

    /// Upper case in the query makes it match case.
    pub fn case(&self) -> bool {
        self.query.chars().any(char::is_uppercase)
    }

    /// Find the query in `lines` again when it or the text (`stamp`, the line count)
    /// changed; the match shown stays the one at or after the line it was on.
    pub fn update<S: AsRef<str>>(&mut self, lines: &[S], stamp: u64) {
        let key = (self.query.clone(), stamp, lines.len());
        if self.key == key {
            return;
        }
        let near = self.hits.get(self.cur).map_or(0, |h| h.0);
        let fresh = self.key.0 != key.0;
        self.key = key;
        self.hits.clear();
        if self.query.is_empty() {
            return;
        }
        let case = self.case();
        let mut at = vec![];
        for (i, l) in lines.iter().enumerate() {
            at.clear();
            find_in(l.as_ref(), &self.query, case, &mut at);
            self.hits.extend(at.iter().map(|&(a, b)| (i as u32, a, b)));
            if self.hits.len() >= MAX {
                break;
            }
        }
        self.cur = self.hits.iter().position(|h| h.0 >= near).unwrap_or(0);
        // A new query shows its first match; text that grew keeps the place.
        self.reveal |= fresh && !self.hits.is_empty();
    }

    pub fn step(&mut self, d: isize) {
        let n = self.hits.len();
        if n > 0 {
            self.cur = (self.cur as isize + d).rem_euclid(n as isize) as usize;
            self.reveal = true;
        }
    }

    /// "3 of 12", or why there are none.
    pub fn count(&self) -> String {
        match self.hits.len() {
            _ if self.query.is_empty() => String::new(),
            0 => "No matches".into(),
            n => format!("{} of {}{}", self.cur + 1, crate::table::fmt_count(n), if n >= MAX { "+" } else { "" }),
        }
    }
}

/// Where `q` occurs in `line`: ASCII letters ignoring case unless `case`.
pub fn find_in(line: &str, q: &str, case: bool, out: &mut Vec<(u32, u32)>) {
    let (h, n) = (line.as_bytes(), q.as_bytes());
    if n.is_empty() || n.len() > h.len() {
        return;
    }
    let mut i = 0;
    while i + n.len() <= h.len() {
        let w = &h[i..i + n.len()];
        if if case { w == n } else { w.eq_ignore_ascii_case(n) } {
            out.push((i as u32, (i + n.len()) as u32));
            i += n.len();
        } else {
            i += 1;
        }
    }
}

thread_local! {
    /// Where find bars were drawn this frame: the toasts rise above one that is at the
    /// window's bottom right, where they would cover its buttons.
    pub static DRAWN: std::cell::RefCell<Vec<Rect>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The bar, sliding up from the bottom of `body` as it opens; `any`: there are matches to
/// step through. Returns what was asked and the room left above it.
#[allow(clippy::too_many_arguments)]
pub fn bar(ui: &mut Ui, body: Rect, f: &mut Finder, count: &str, any: bool, id: u64, animations: bool, pal: &Palette) -> (Out, Rect) {
    let now = ui.input(|i| i.time);
    let t = if animations { ((now - *f.opened.get_or_insert(now)) / 0.12).min(1.0) as f32 } else { 1.0 };
    if t < 1.0 {
        ui.ctx().request_repaint();
    }
    let ease = 1.0 - (1.0 - t) * (1.0 - t);
    let rows = if f.replace.is_some() { 2.0 } else { 1.0 };
    // A body shorter than the bar (a terminal pane closing or squeezed by a small
    // window) shows none of it, rather than a bar reaching up over what is above.
    if body.height() < BAR_H * rows {
        return (Out::None, body);
    }
    let h = (BAR_H * rows * ease).round();
    let bar = Rect::from_min_max(pos2(body.left(), body.bottom() - h), body.max);
    DRAWN.with(|d| d.borrow_mut().push(bar));
    let mut out = Out::None;
    let p = ui.painter().with_clip_rect(bar.intersect(ui.clip_rect()));
    p.rect_filled(bar, CornerRadius::ZERO, pal.bg);
    p.line_segment([pos2(bar.left(), bar.top() - 0.5), pos2(bar.right(), bar.top() - 0.5)], Stroke::new(1.0, pal.border));
    let top = Rect::from_min_size(bar.min, vec2(bar.width(), BAR_H));
    // The filter's pill, its count inside it at the end; up and down in one capsule
    // beside it, as a panel groups its switches; the way out bare at the end. All three
    // sit three points inside the bar, as everything in a 30 point row does.
    use crate::kit::{INSET, ITEM, ROW_CAP_H, ROW_GLYPH, ROW_ITEM};
    let round = crate::kit::round();
    let row = Rect::from_center_size(top.center(), vec2(top.width() - 12.0, ROW_CAP_H));
    let mut bui = ui.new_child(UiBuilder::new().max_rect(row).layout(egui::Layout::left_to_right(egui::Align::Center)));
    bui.set_clip_rect(bar.intersect(ui.clip_rect()));
    bui.set_opacity(ease);
    let close_r = Rect::from_center_size(pos2(row.right() - ITEM.x / 2.0, row.center().y), ITEM);
    let nav = Rect::from_min_max(pos2(close_r.left() - 4.0 - 2.0 * ROW_ITEM.x - 2.0 * INSET, row.top()), pos2(close_r.left() - 4.0, row.bottom()));
    let pill = Rect::from_min_max(row.min, pos2((nav.left() - 6.0).max(row.left() + 60.0), row.bottom()));
    if crate::kit::bar_item_at(&bui, close_r, egui::Id::new(("find close", id)), "toolbar button", "Close (Esc)", true, false, round, pal, |p, r, c| paint_glyph(p, r, Glyph::Close, c)).clicked() {
        out = Out::Close;
    }
    crate::kit::capsule(bui.painter(), nav, round, pal);
    for (k, (icon, tip, o)) in [(Glyph::ChevronUp, "Previous match (Shift+Enter)", Out::Prev), (Glyph::ChevronDown, "Next match (Enter)", Out::Next)].into_iter().enumerate() {
        let ir = Rect::from_min_size(pos2(nav.left() + INSET + k as f32 * ROW_ITEM.x, nav.top() + INSET), ROW_ITEM);
        if crate::kit::bar_item_at(&bui, ir, egui::Id::new(("find step", id, k)), tip, tip, any, false, round, pal, |p, r, c| paint_glyph(p, r.shrink((crate::kit::GLYPH - ROW_GLYPH) / 2.0 + 1.0), icon, c)).clicked() {
            out = o;
        }
    }
    crate::kit::capsule(bui.painter(), pill, round, pal);
    paint_glyph(bui.painter(), Rect::from_center_size(pos2(pill.left() + 15.0, pill.center().y), Vec2::splat(13.0)), Glyph::Search, pal.text_dim);
    let none = !f.query.is_empty() && !any && count == "No matches";
    let cg = crate::listing::figures(&bui, count, FontId::proportional(11.5), 200.0, if none { pal.danger } else { pal.text_dim }, if none { pal.danger } else { crate::kit::words_of(pal.text_dim) });
    // In a narrow panel the field keeps room to type in; the count gives way.
    let show_count = pill.width() - 32.0 - cg.size().x - 14.0 >= 40.0;
    let field_right = if show_count {
        bui.painter().galley(pos2(pill.right() - 12.0 - cg.size().x, pill.center().y - cg.size().y / 2.0), cg.clone(), pal.text_dim);
        pill.right() - 12.0 - cg.size().x - 6.0
    } else {
        pill.right() - 10.0
    };
    let fid = egui::Id::new(("insp-find", id));
    let fr = Rect::from_min_max(pos2(pill.left() + 26.0, pill.top() + 1.0), pos2(field_right, pill.bottom() - 1.0));
    let mut fui = bui.new_child(UiBuilder::new().max_rect(fr).layout(egui::Layout::left_to_right(egui::Align::Center)));
    fui.spacing_mut().interact_size.y = fr.height();
    let te = egui::TextEdit::singleline(&mut f.query).id(fid).hint_text(egui::RichText::new("Find").color(pal.text_dim)).frame(crate::ui::bare_edit()).desired_width(fr.width()).font(FontId::proportional(12.5));
    let r = Some(fui.add(te));
    let Some(r) = r else { return (out, body) };
    // As the bar slides up, only what shows of the field is there to click.
    crate::ui::note_control(&bui, r.rect.intersect(bar), "find field");
    if f.focus {
        f.focus = false;
        r.request_focus();
        if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), fid) {
            let all = egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(f.query.chars().count()));
            st.cursor.set_char_range(Some(all));
            st.store(ui.ctx(), fid);
        }
    }
    if f.focused {
        bui.input_mut(|i| {
            if i.consume_key(egui::Modifiers::SHIFT, egui::Key::Enter) || i.consume_key(egui::Modifiers::SHIFT, egui::Key::F3) {
                out = Out::Prev;
            } else if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) || i.consume_key(egui::Modifiers::NONE, egui::Key::F3) {
                out = Out::Next;
            }
        });
        // Enter takes the focus away from a single-line field: kept for the next press.
        if out == Out::Next || out == Out::Prev {
            r.request_focus();
        }
    }
    f.focused = ui.memory(|m| m.has_focus(fid));
    if let Some(rep) = f.replace.as_mut() {
        let row = Rect::from_center_size(pos2(bar.center().x, top.bottom() + BAR_H / 2.0), vec2(bar.width() - 12.0, ROW_CAP_H));
        let mut rui = ui.new_child(UiBuilder::new().max_rect(row).layout(egui::Layout::left_to_right(egui::Align::Center)));
        rui.set_clip_rect(bar.intersect(ui.clip_rect()));
        rui.set_opacity(ease);
        rui.spacing_mut().item_spacing.x = 6.0;
        // The replacement in a pill as the query's, the two actions after it.
        let w = (rui.available_width() - 130.0).max(60.0);
        let pr = Rect::from_min_size(row.min, vec2(w, ROW_CAP_H));
        crate::kit::capsule(rui.painter(), pr, round, pal);
        theme::paint_symbol(rui.painter(), Rect::from_center_size(pos2(pr.left() + 15.0, pr.center().y), Vec2::splat(13.0)), "replace", pal.text_dim);
        rui.allocate_exact_size(vec2(26.0, ROW_CAP_H), Sense::hover());
        let r = rui.add(egui::TextEdit::singleline(rep).id_salt(("insp-replace", id)).hint_text(egui::RichText::new("Replace").color(pal.text_dim)).frame(crate::ui::bare_edit()).desired_width(w - 36.0).font(FontId::proportional(12.5)));
        rui.add_space(10.0);
        crate::ui::note_control(&rui, r.rect, "replace field");
        if r.lost_focus() && rui.input(|i| i.key_pressed(egui::Key::Enter)) {
            out = Out::Replace;
            r.request_focus();
        }
        let on = any;
        if small_button(&mut rui, "Replace", on, pal) {
            out = Out::Replace;
        }
        if small_button(&mut rui, "All", on, pal) {
            out = Out::ReplaceAll;
        }
    }
    (out, Rect::from_min_max(body.min, pos2(body.right(), bar.top() - 1.0)))
}

fn small_button(ui: &mut Ui, label: &'static str, enabled: bool, pal: &Palette) -> bool {
    let r = ui.add_enabled_ui(enabled, |ui| crate::kit::small_button(ui, label, pal)).inner;
    crate::ui::note_control(ui, r.rect, label);
    enabled && r.clicked()
}

/// One match in the text: tinted in the accent, the current one with its hairline as
/// well, so the letters under it stay as readable as the rest.
pub fn paint_match(p: &egui::Painter, r: Rect, current: bool, pal: &Palette) {
    if current {
        p.rect(r.expand(1.0), CornerRadius::same(3), pal.accent.gamma_multiply(if pal.dark { 0.22 } else { 0.18 }), Stroke::new(1.0, pal.accent.gamma_multiply(0.85)), egui::StrokeKind::Inside);
    } else {
        p.rect_filled(r, CornerRadius::same(2), pal.accent.gamma_multiply(if pal.dark { 0.16 } else { 0.13 }));
    }
}

/// Paint the matches on one row of a text preview: `cols(a, b)` gives the columns of the
/// line's bytes a..b; `x0` is where the row's text starts and `cw` a column's width.
#[allow(clippy::too_many_arguments)]
pub fn paint_hits(p: &egui::Painter, hits: &[Hit], cur: Option<usize>, line: u32, a: usize, b: usize, text: &str, x0: f32, top: f32, h: f32, cw: f32, pal: &Palette) {
    let first = hits.partition_point(|x| x.0 < line);
    for (k, &(_, s, e)) in hits[first..].iter().take_while(|x| x.0 == line).enumerate() {
        let (s, e) = ((s as usize).max(a), (e as usize).min(b));
        if s >= e {
            continue;
        }
        let cols = |x: usize, y: usize| text.get(x..y).map_or(0, |t| t.chars().map(crate::inspector::char_cols).sum::<usize>()) as f32;
        let r = Rect::from_min_size(pos2(x0 + cols(a, s) * cw, top + 1.0), vec2((cols(s, e) * cw).max(2.0), h - 2.0));
        paint_match(p, r, cur == Some(first + k), pal);
    }
}

/// The x where a match starts, for scrolling sideways to it.
pub fn hit_x(text: &str, start: usize, cw: f32) -> f32 {
    text.get(..start).map_or(0, |t| t.chars().map(crate::inspector::char_cols).sum::<usize>()) as f32 * cw
}
