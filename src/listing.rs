//! The file listing: Details / List / Columns / Icons on one continuous zoom axis.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use egui::{
    self, Align2, Color32, CornerRadius, FontId, Galley, Id, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, UiBuilder, Vec2, pos2, text::LayoutJob,
    vec2,
};

use crate::app::{Action, Drag};
use crate::commands::{Cmd, Keymap};
use crate::config::{Column, Config, MiddleClick, SortKey};
use crate::fsx::{self, Entry};
use crate::fuzzy;
use crate::panel::{Panel, Side};
use crate::popups::Popup;
use crate::stats::Stats;
use crate::tab::{self, Row, Rubber, Tab};
use crate::theme::{self, Palette};
use crate::thumbs::{self, Thumbs};

pub struct Env<'a> {
    pub cfg: &'a Config,
    pub pal: &'a Palette,
    pub thumbs: &'a mut Thumbs,
    pub stats: &'a mut Stats,
    pub recents: &'a HashSet<PathBuf>,
    pub cut: &'a HashSet<PathBuf>,
    pub actions: &'a mut Vec<Action>,
    pub keymap: &'a Keymap,
    pub drag: &'a mut Option<Drag>,
    pub panel_id: u64,
    pub active: bool,
    pub bookmarks: &'a [PathBuf],
    pub avatars: &'a mut crate::avatars::Avatars,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    Details,
    Tree,
    List,
    Icons(f32),
    Columns,
}

/// Zoom stops of the named views: the slider runs Details 0%, Tree 10%, List 20%,
/// then icons grow linearly from medium (30%) through large (65%) to extra large (100%).
pub const VIEW_STOPS: [(&str, f32); 6] = [("Details", 0.0), ("Tree", 0.1), ("List", 0.2), ("M Icons", 0.3), ("L Icons", 0.65), ("XL Icons", 1.0)];

/// One zoom step (a wheel notch or Ctrl+= / Ctrl+-): the dense views move one named view at
/// a time (Details, Tree, List, M Icons); icons then grow or shrink by 5.5%.
pub fn zoom_step(z: f32, dir: i32) -> f32 {
    const DENSE: [f32; 4] = [0.0, 0.1, 0.2, 0.3];
    if dir > 0 {
        if z < 0.3 - 1e-3 {
            DENSE.iter().copied().find(|s| *s > z + 1e-3).unwrap_or(0.3)
        } else {
            (z + 0.055).min(1.0)
        }
    } else if z <= 0.3 + 1e-3 {
        DENSE.iter().rev().copied().find(|s| *s < z - 1e-3).unwrap_or(0.0)
    } else {
        (z - 0.055).max(0.3)
    }
}

pub fn icon_size(z: f32) -> f32 {
    48.0 + ((z - 0.3).max(0.0) / 0.7).min(1.0) * 96.0
}

pub fn mode_for(z: f32) -> Mode {
    if z < 0.05 {
        Mode::Details
    } else if z < 0.15 {
        Mode::Tree
    } else if z < 0.25 {
        Mode::List
    } else {
        Mode::Icons(icon_size(z))
    }
}

/// The view a tab shows: Columns, or the one its zoom names.
pub fn mode_of(s: &crate::config::ViewSettings) -> Mode {
    if s.column_view { Mode::Columns } else { mode_for(s.zoom) }
}

/// The name of the view a tab shows (footer chip, View popup).
pub fn view_label(s: &crate::config::ViewSettings) -> &'static str {
    if s.column_view { "Columns" } else { view_name(s.zoom) }
}

pub fn view_name(z: f32) -> &'static str {
    match mode_for(z) {
        Mode::Columns => "Columns",
        Mode::Details => "Details",
        Mode::Tree => "Tree",
        Mode::List => "List",
        Mode::Icons(s) if s < 72.0 => "M Icons",
        Mode::Icons(s) if s < 120.0 => "L Icons",
        Mode::Icons(_) => "XL Icons",
    }
}

/// Small glyph for a view mode (footer chip, zoom popup, menus).
pub fn paint_view_glyph(p: &egui::Painter, r: Rect, mode: Mode, col: Color32) {
    let st = Stroke::new(1.2, col);
    let (l, t, w) = (r.left(), r.top(), r.width());
    match mode {
        Mode::Details => {
            for k in 0..3 {
                let y = t + 2.5 + k as f32 * (r.height() - 5.0) / 2.0;
                p.circle_filled(pos2(l + 1.5, y), 1.0, col);
                p.line_segment([pos2(l + 4.5, y), pos2(r.right() - 0.5, y)], st);
            }
        }
        Mode::Tree => {
            let ys = [t + 2.5, t + r.height() / 2.0, r.bottom() - 2.5];
            p.line_segment([pos2(l + 1.0, ys[0]), pos2(r.right() - 0.5, ys[0])], st);
            p.line_segment([pos2(l + 2.5, ys[0]), pos2(l + 2.5, ys[2])], st);
            for y in &ys[1..] {
                p.line_segment([pos2(l + 2.5, *y), pos2(l + 5.0, *y)], st);
                p.line_segment([pos2(l + 6.5, *y), pos2(r.right() - 0.5, *y)], st);
            }
        }
        Mode::List => {
            for k in 0..3 {
                let y = t + 2.5 + k as f32 * (r.height() - 5.0) / 2.0;
                p.line_segment([pos2(l + 0.5, y), pos2(l + w * 0.42, y)], st);
                p.line_segment([pos2(l + w * 0.58, y), pos2(r.right() - 0.5, y)], st);
            }
        }
        Mode::Icons(_) => theme::paint_glyph(p, r, theme::Glyph::Pics, col),
        Mode::Columns => {
            // Three panes side by side.
            p.rect_stroke(r.shrink(0.5), CornerRadius::same(1), st, StrokeKind::Inside);
            for f in [0.36, 0.7] {
                let x = (l + w * f).round() + 0.5;
                p.line_segment([pos2(x, t + 0.5), pos2(x, r.bottom() - 0.5)], st);
            }
        }
    }
}

/// Single-line galley elided with "…" to `width`.
/// Like `elided`, but keeps the end ("…/parent/folder"): for paths, where the tail matters.
pub fn elided_tail(ui: &Ui, text: &str, font: FontId, width: f32, color: Color32) -> Arc<Galley> {
    let fixed = crate::rtl::fix_rtl(text);
    let full = ui.painter().layout_no_wrap(fixed.clone(), font.clone(), color);
    if full.size().x <= width {
        return full;
    }
    let chars: Vec<char> = fixed.chars().collect();
    // Longest suffix that fits after the ellipsis (binary search on its length).
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let s: String = std::iter::once('…').chain(chars[chars.len() - mid..].iter().copied()).collect();
        if ui.painter().layout_no_wrap(s, font.clone(), color).size().x <= width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let s: String = std::iter::once('…').chain(chars[chars.len() - lo..].iter().copied()).collect();
    ui.painter().layout_no_wrap(s, font, color)
}

pub fn elided(ui: &Ui, text: &str, font: FontId, width: f32, color: Color32) -> Arc<Galley> {
    let fixed = crate::rtl::fix_rtl(text);
    let mut job = LayoutJob::simple_singleline(fixed, font, color);
    job.wrap = egui::text::TextWrapping { max_width: width.max(8.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    ui.fonts_mut(|f| f.layout_job(job))
}

pub fn display_name(e: &Entry, cfg: &Config) -> String {
    if !cfg.show_ext && !e.is_dir {
        if let Some(stem) = e.path.file_stem() {
            if !e.name.starts_with('.') {
                return stem.to_string_lossy().into_owned();
            }
        }
    }
    e.name.clone()
}

/// Name with fuzzy-match highlights, plus an optional dimmed suffix (flattened relative path).
fn name_job(name: &str, suffix: Option<&str>, q: &fuzzy::Query, font: &FontId, col: Color32, dim: Color32, accent: Color32, width: f32, rows: usize) -> LayoutJob {
    let mut job = LayoutJob::default();
    let hits: HashSet<usize> = if q.is_empty() { HashSet::new() } else { fuzzy::positions(q, name).map(|(_, p)| p.into_iter().collect()).unwrap_or_default() };
    let fmt = |c: Color32| egui::TextFormat { font_id: font.clone(), color: c, ..Default::default() };
    if hits.is_empty() {
        let fixed = crate::rtl::fix_rtl(name);
        job.append(&fixed, 0.0, fmt(col));
    } else {
        if crate::rtl::has_rtl(name) {
            let fixed = crate::rtl::fix_rtl(name);
            job.append(&fixed, 0.0, fmt(accent));
        } else {
            let mut buf = String::new();
            let mut buf_hit = false;
            for (i, ch) in name.chars().enumerate() {
                let h = hits.contains(&i);
                if h != buf_hit && !buf.is_empty() {
                    job.append(&buf, 0.0, fmt(if buf_hit { accent } else { col }));
                    buf.clear();
                }
                buf_hit = h;
                buf.push(ch);
            }
            if !buf.is_empty() {
                job.append(&buf, 0.0, fmt(if buf_hit { accent } else { col }));
            }
        }
    }
    if let Some(s) = suffix {
        let fixed_s = crate::rtl::fix_rtl(s);
        job.append(&fixed_s, 8.0, fmt(dim));
    }
    job.wrap = egui::text::TextWrapping { max_width: width.max(8.0), max_rows: rows, break_anywhere: true, overflow_character: Some('…') };
    job
}

/// A row's colour labels and other tags once read (they are asked for until then).
fn row_tags(p: &std::path::Path) -> Option<(Vec<Color32>, String)> {
    crate::tags::get(p, |t| {
        if t.is_empty() {
            return None;
        }
        let dots = t.iter().filter_map(|x| crate::tags::color_of(x)).collect();
        let words: Vec<&str> = t.iter().filter(|x| crate::tags::color_of(x).is_none()).map(String::as_str).collect();
        Some((dots, words.join(", ")))
    })
    .flatten()
}

/// Colour dots in a row from `x`, centred on `y`, each ringed in `ring` so they read on
/// any background. Returns the width they take.
fn paint_dots(p: &egui::Painter, x: f32, y: f32, dots: &[Color32], r: f32, ring: Color32) -> f32 {
    let step = r * 2.0 + 2.5;
    for (i, c) in dots.iter().enumerate() {
        let cc = pos2(x + r + i as f32 * step, y);
        p.circle_filled(cc, r + 1.2, ring);
        p.circle_filled(cc, r, *c);
    }
    if dots.is_empty() { 0.0 } else { step * dots.len() as f32 - 2.5 }
}

/// Secondary text, match highlight and the ring around colour dots on an item: on a
/// selected one the dim grey and the accent sink into the selection colour, and a blue
/// dot would vanish without a ring.
fn sel_colors(pal: &theme::Palette, selected: bool) -> (Color32, Color32, Color32) {
    if selected { (pal.on_sel_dim(), pal.on_sel(), pal.on_sel()) } else { (pal.text_dim, pal.accent, Color32::TRANSPARENT) }
}

/// After a row's name: its colour dots, then its other tags in small dim text. Returns
/// the width they need, measured before the name is laid out so it leaves the room.
fn tags_width(ui: &Ui, tags: &(Vec<Color32>, String), font: &FontId) -> f32 {
    let r = (font.size * 0.28).clamp(3.0, 5.0);
    let dots = if tags.0.is_empty() { 0.0 } else { 8.0 + (r * 2.0 + 2.5) * tags.0.len() as f32 };
    let words = if tags.1.is_empty() { 0.0 } else { 8.0 + ui.painter().layout_no_wrap(tags.1.clone(), FontId::proportional(font.size - 1.5), Color32::WHITE).size().x };
    dots + words
}

fn paint_tags(ui: &Ui, x: f32, y: f32, tags: &(Vec<Color32>, String), font: &FontId, dim: Color32, ring: Color32) {
    let r = (font.size * 0.28).clamp(3.0, 5.0);
    let mut x = x;
    if !tags.0.is_empty() {
        x += 8.0 + paint_dots(ui.painter(), x + 8.0, y, &tags.0, r, ring);
    }
    if !tags.1.is_empty() {
        let g = ui.painter().layout_no_wrap(tags.1.clone(), FontId::proportional(font.size - 1.5), dim);
        ui.painter().galley(pos2(x + 8.0, y - g.size().y / 2.0), g, dim);
    }
}

/// The Tags column: the dots, then the words cut to the room left.
fn tags_cell(ui: &Ui, r: Rect, tags: &(Vec<Color32>, String), font: &FontId, dim: Color32, ring: Color32) {
    let rad = (font.size * 0.28).clamp(3.0, 5.0);
    // As many dots as fit, the words after them when room is left.
    let fit = (((r.width() + 2.5) / (rad * 2.0 + 2.5)).floor() as usize).min(tags.0.len());
    let mut x = r.left();
    if fit > 0 {
        x += paint_dots(ui.painter(), x, r.center().y, &tags.0[..fit], rad, ring) + 6.0;
    }
    if !tags.1.is_empty() && r.right() - x > 16.0 {
        let g = elided(ui, &tags.1, FontId::proportional(font.size - 1.5), r.right() - x, dim);
        ui.painter().galley(pos2(x, r.center().y - g.size().y / 2.0), g, dim);
    }
}

/// A matched line after the name, with the words it matched in the accent colour.
fn append_match(job: &mut LayoutJob, line: &str, needle: &str, font: &FontId, dim: Color32, accent: Color32) {
    let fmt = |c: Color32| egui::TextFormat { font_id: font.clone(), color: c, ..Default::default() };
    let gap = if job.sections.len() > 1 { 12.0 } else { 8.0 };
    // Lowercasing that keeps byte offsets (nearly all text) lets the match be found in place.
    let (low, nl) = (line.to_lowercase(), needle.to_lowercase());
    let at = (!nl.is_empty() && low.len() == line.len()).then(|| low.find(&nl)).flatten().filter(|i| line.is_char_boundary(i + nl.len()));
    match at {
        Some(i) => {
            job.append(&line[..i], gap, fmt(dim));
            job.append(&line[i..i + nl.len()], 0.0, fmt(accent));
            job.append(&line[i + nl.len()..], 0.0, fmt(dim));
        }
        None => job.append(line, gap, fmt(dim)),
    }
}

/// Row pitch: generous leading around the text, scaled by the Spacing option
/// (12 px text at default spacing gives ~24.5 px rows).
pub fn row_height(cfg: &Config) -> f32 {
    (cfg.font_size * 1.35 + 8.3 * cfg.spacing).round()
}

pub fn listing(ui: &mut Ui, rect: Rect, panel: &mut Panel, env: &mut Env) {
    let t = panel.tab_mut();
    // Ctrl+wheel → zoom, one step per notch (fine-grained touchpad scrolling accumulates
    // into whole notches).
    let hover = ui.ctx().pointer_hover_pos().map(|p| rect.contains(p)).unwrap_or(false);
    // (Columns has no zoom: its rows follow the font size.)
    if hover && !t.settings.column_view {
        let zd = ui.input(|i| i.zoom_delta());
        if (zd - 1.0).abs() > 0.0001 {
            // egui turns a wheel notch into `line_scroll_speed` points and zooms by
            // e^(points/200), so ln(zd)·200/speed counts notches.
            let speed = ui.ctx().options(|o| o.input_options.line_scroll_speed).max(1.0);
            t.zoom_acc += zd.ln() * 200.0 / speed;
            while t.zoom_acc.abs() >= 0.999 {
                let dir = if t.zoom_acc > 0.0 { 1 } else { -1 };
                t.zoom_acc -= dir as f32;
                t.settings.zoom = zoom_step(t.settings.zoom, dir);
                t.scroll_to_cursor = true;
            }
        }
    }
    // Animate the zoom: switching among Details, Tree and List is instant (as is entering
    // or leaving icons); icon sizes ease to their new size in about 90 ms.
    let target = t.settings.zoom;
    if t.shown_zoom < 0.0 || !env.cfg.animations || target < 0.3 - 1e-3 || t.shown_zoom < 0.3 - 1e-3 {
        t.shown_zoom = target;
    } else if (t.shown_zoom - target).abs() > 0.001 {
        let dt = ui.input(|i| i.stable_dt).min(0.05);
        t.shown_zoom += (target - t.shown_zoom) * (dt * 30.0).min(1.0);
        ui.ctx().request_repaint();
    } else {
        t.shown_zoom = target;
    }
    if t.check_drag.is_some() && !ui.input(|i| i.pointer.primary_down()) {
        t.check_drag = None;
    }
    // Contents mode matched inside the files: no letters of the name to highlight.
    let name = crate::query::name_part(&t.filter);
    let q = fuzzy::Query::new(if t.content.on { "" } else { &name });
    let mut ui = ui.new_child(UiBuilder::new().max_rect(rect).id_salt(("listing", env.panel_id, t.id)));
    ui.set_clip_rect(rect.intersect(ui.clip_rect()));
    let mut er = rect;
    if t.settings.column_view {
        er = columns(&mut ui, rect, t, env, &q);
    } else {
        t.strip = None;
        match mode_for(t.shown_zoom) {
            Mode::Details | Mode::Tree => details(&mut ui, rect, t, env, &q),
            Mode::List => grid(&mut ui, rect, t, env, &q, Mode::List),
            m @ Mode::Icons(_) => grid(&mut ui, rect, t, env, &q, m),
            Mode::Columns => {}
        }
    }
    // Empty states (in the Columns view, in the tab's own column).
    let rect = er;
    if t.view.is_empty() && !t.is_loading() {
        let msg = if let Some(e) = &t.error {
            format!("Can't open this folder: {}", fsx::plain_error(e))
        } else if !t.filter.trim().is_empty() && !t.content.on {
            format!("Nothing matches “{}”", t.filter.trim())
        } else if !t.filter.is_empty() || !t.settings.only_types.is_empty() || !t.settings.show_files || !t.settings.show_folders {
            "No matches".into()
        } else if t.hidden_count > 0 {
            format!("{} hidden item{}", t.hidden_count, if t.hidden_count == 1 { "" } else { "s" })
        } else if t.smart.is_some() {
            "No files match".into()
        } else {
            "This folder is empty".into()
        };
        // Cut to the panel: a long error or filter ran past a narrow one.
        let g = elided(&ui, &msg, FontId::proportional(env.cfg.font_size), (rect.width() - 24.0).max(40.0), env.pal.text_dim);
        // What kind of empty, at a glance, above the words (when there is room for it).
        let glyph = if t.error.is_some() {
            theme::Glyph::Lock
        } else if !t.filter.is_empty() || t.smart.is_some() || !t.settings.only_types.is_empty() {
            theme::Glyph::Search
        } else if t.hidden_count > 0 {
            theme::Glyph::Eye
        } else {
            theme::Glyph::Folder
        };
        let side = (env.cfg.font_size * 2.6).round();
        let tall = rect.height() > side * 3.0 + g.size().y;
        let top = rect.center().y - g.size().y / 2.0 + if tall { side * 0.6 } else { 0.0 };
        if tall {
            let gr = Rect::from_center_size(pos2(rect.center().x, top - side * 0.75), Vec2::splat(side));
            theme::paint_glyph(ui.painter(), gr, glyph, env.pal.text_dim.gamma_multiply(0.55));
        }
        ui.painter().galley(pos2(rect.center().x - g.size().x / 2.0, top), g, env.pal.text_dim);
    } else if t.view.is_empty() && (t.flatten || t.content.busy()) {
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, if t.content.busy() || t.smart.is_some() { "Searching…" } else { "Indexing…" }, FontId::proportional(env.cfg.font_size), env.pal.text_dim);
    }
    // Files dragged inside Maia and released over this list but not on a folder row
    // (rows got the first chance above) go into this folder. Pointer position, not
    // `hovered()`: while files are dragged the dragged row owns the pointer.
    if let Some(Drag::Files { paths, .. }) = env.drag.as_ref() {
        let over = ui.ctx().pointer_latest_pos().is_some_and(|p| rect.contains(p));
        if over && ui.input(|i| i.pointer.any_released()) && !tab::is_computer(&t.path) {
            if paths.iter().any(|p| p.parent() != Some(t.path.as_path())) {
                let m = ui.input(|i| i.modifiers);
                env.actions.push(Action::Drop { paths: paths.clone(), dest: t.path.clone(), copy: m.ctrl && !m.shift, link: m.ctrl && m.shift });
            }
            *env.drag = None;
        }
    }
    // External drops (files from other apps).
    let dropped: Vec<PathBuf> = ui.ctx().input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
    if !dropped.is_empty() {
        let pp = ui.ctx().pointer_hover_pos();
        let mine = match pp {
            Some(p) => rect.contains(p),
            None => env.active,
        };
        if mine && !tab::is_computer(&t.path) {
            env.actions.push(Action::ExternalDrop { paths: dropped, dest: t.path.clone() });
        }
    }
    let hovering_files = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
    if hovering_files && hover {
        ui.painter().rect_stroke(rect.shrink(2.0), CornerRadius::same(6), Stroke::new(2.0, env.pal.accent), StrokeKind::Inside);
    }
}

// ------------------------------------------------------------------ shared item logic

/// A Computer view row for a drive that isn't mounted (drawn dimmed).
fn unmounted(t: &Tab, e: &Entry) -> bool {
    t.vols.get(&e.path).is_some_and(|v| !v.mounted)
}

/// Drive icon in the Computer view (`ui::paint_drive`), a spinner while it is being
/// mounted, unlocked or ejected.
fn paint_drive(ui: &Ui, r: Rect, t: &Tab, e: &Entry, col: Color32) {
    let v = t.vols.get(&e.path);
    if v.is_some_and(|v| crate::drives::busy(&v.device)) {
        crate::ui::spinner(ui.painter(), r.center(), r.width() * 0.36, col, ui.input(|i| i.time));
        ui.ctx().request_repaint();
        return;
    }
    crate::ui::paint_drive(ui.painter(), r, v.is_some_and(|v| v.removable), v.map_or(&crate::drives::Crypt::Plain, |v| &v.crypt), col, ui.visuals().extreme_bg_color);
}

fn entry_size_text(e: &Entry, t: &Tab, env: &Env) -> String {
    if tab::is_computer(&t.path) {
        if let Some(v) = t.vols.get(&e.path) {
            if !v.mounted {
                return fsx::fmt_size(v.total);
            }
            return format!("{} / {}", fsx::fmt_size(v.total.saturating_sub(v.free)), fsx::fmt_size(v.total));
        }
    }
    if e.is_dir {
        match env.stats.map.get(&e.path) {
            Some(s) => fsx::fmt_size(s.size),
            // A linked folder is not measured (its contents are counted where they
            // are): "--" as for a file's folder count, not a blank that looks pending.
            None if e.is_link => "--".into(),
            None => String::new(),
        }
    } else {
        fsx::fmt_size(e.size)
    }
}

fn type_text(e: &Entry, t: &Tab) -> String {
    if let Some(v) = t.vols.get(&e.path) {
        return if v.fstype == "crypto_LUKS" { "Encrypted (LUKS)".into() } else { v.fstype.clone() };
    }
    if e.is_dir {
        return "Folder".into();
    }
    if let Some(k) = e.special_kind() {
        return k.into();
    }
    if e.is_broken_link() {
        return "Broken link".into();
    }
    let db = crate::mime::mime_db();
    let m = db.for_name(&e.name, false, e.is_executable());
    let c = db.comment(&m);
    if c.is_empty() { e.kind() } else { c }
}

fn fmt_date(t: Option<std::time::SystemTime>, cfg: &Config) -> String {
    t.map(|t| crate::timefmt::format(t, &cfg.date_format)).unwrap_or_default()
}

fn abbrev(n: u64) -> String {
    if n == 0 {
        "--".into()
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1000 {
        format!("{:.1}K", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

/// The item clicked last, by path: a double-click is two clicks on the same item (the
/// first may have been in another column of the Columns view).
static LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Selection / activation / context menu / drag for one item.
fn handle_item(ui: &Ui, resp: &Response, idx: u32, t: &mut Tab, env: &mut Env) {
    let mods = ui.input(|i| i.modifiers);
    let e = &t.entries[idx as usize];
    let (is_dir, path) = (e.is_dir, e.path.clone());
    let renaming = t.rename.is_some();
    let in_rename = t.rename.as_ref().map(|r| r.field_for(idx).is_some()).unwrap_or(false);
    if in_rename {
        return; // the rename field handles its own clicks
    }
    let pressed = resp.hovered() && ui.input(|i| i.pointer.primary_pressed());
    let select = |t: &mut Tab| {
        if mods.ctrl || mods.command {
            if !t.selected.remove(&idx) {
                t.selected.insert(idx);
            }
            t.cursor = Some(idx);
            t.anchor = Some(idx);
        } else if mods.shift {
            t.select_range_to(idx, false);
        } else {
            t.select_only(idx);
        }
    };
    if pressed && renaming {
        env.actions.push(Action::CommitRename(env.panel_id));
    }
    if env.cfg.act_on_press {
        if pressed && (mods.ctrl || mods.shift || !t.selected.contains(&idx)) {
            select(t);
        }
        if resp.clicked() && !mods.ctrl && !mods.shift && t.selected.len() > 1 && t.selected.contains(&idx) {
            t.select_only(idx);
        }
    } else if resp.clicked() {
        select(t);
    }
    if resp.clicked() && env.cfg.single_click && !mods.ctrl && !mods.shift && (is_dir || !env.cfg.single_click_folders_only) {
        env.actions.push(Action::Activate { panel: env.panel_id, idx, mods });
    }
    // egui takes any two quick clicks for a double-click; here both must land on this item.
    let this = Id::new(&path).value();
    let double = resp.double_clicked() && LAST.load(std::sync::atomic::Ordering::Relaxed) == this;
    if resp.clicked() {
        LAST.store(this, std::sync::atomic::Ordering::Relaxed);
    }
    if double && !env.cfg.single_click {
        env.actions.push(Action::Activate { panel: env.panel_id, idx, mods });
    }
    if resp.middle_clicked() {
        if t.flatten {
            if let Some(parent) = path.parent() {
                env.actions.push(Action::NewTab { panel: env.panel_id, path: parent.to_path_buf(), select: Some(path.clone()), focus: false });
            }
        } else if is_dir {
            match env.cfg.middle_click {
                MiddleClick::OpenInSplit => env.actions.push(Action::Split { panel: env.panel_id, path: path.clone(), side: Side::Right, select: None }),
                MiddleClick::OpenInNewTab => env.actions.push(Action::NewTab { panel: env.panel_id, path: path.clone(), select: None, focus: false }),
            }
        }
    }
    if resp.secondary_clicked() {
        if !t.selected.contains(&idx) {
            t.select_only(idx);
        }
        let pos = resp.interact_pointer_pos().unwrap_or(resp.rect.left_bottom());
        env.actions.push(Action::CtxMenu { panel: env.panel_id, pos, background: false });
    }
    let right = resp.drag_started_by(egui::PointerButton::Secondary);
    if (resp.drag_started_by(egui::PointerButton::Primary) || right) && env.drag.is_none() && !tab::is_computer(&t.path) {
        if !t.selected.contains(&idx) {
            t.select_only(idx);
        }
        *env.drag = Some(Drag::Files { paths: t.selected_paths(), from_panel: env.panel_id, right });
    }
}

/// Selection checkbox for an item: shown on hover, and on selected items during a
/// multi-selection. Click toggles; dragging across checkboxes paints the same state.
/// Selection checkbox, drawn over the item's icon slot while the row is hovered, during a
/// multi-selection, or while check-dragging. Returns true when it was drawn (skip the icon).
fn item_checkbox(ui: &Ui, r: Rect, idx: u32, row_hovered: bool, t: &mut Tab, env: &mut Env) -> bool {
    if !env.cfg.selection_boxes {
        return false;
    }
    let selected = t.selected.contains(&idx);
    let multi = t.selected.len() > 1;
    let pointer = ui.ctx().pointer_hover_pos();
    let over = pointer.map(|p| r.expand(2.0).contains(p)).unwrap_or(false);
    // Continue a check-drag over this box.
    if let Some(state) = t.check_drag {
        if over && ui.input(|i| i.pointer.primary_down()) && selected != state {
            if state { t.selected.insert(idx); } else { t.selected.remove(&idx); }
        }
    }
    if !(row_hovered || multi && selected || t.check_drag.is_some()) {
        return false;
    }
    crate::popups::checkbox_glyph(ui.painter(), r, selected, env.pal);
    let resp = ui.interact(r.expand(2.0), Id::new(("chk", t.id, idx)), Sense::click_and_drag());
    if resp.is_pointer_button_down_on() && ui.input(|i| i.pointer.primary_pressed()) {
        let state = !selected;
        if state { t.selected.insert(idx); } else { t.selected.remove(&idx); }
        t.cursor = Some(idx);
        t.anchor = Some(idx);
        t.check_drag = Some(state);
    }
    true
}

/// Tree chevron (rotates when expanded). Returns true if clicked.
fn tree_chevron(ui: &Ui, r: Rect, id: Id, expanded: bool, pal: &Palette) -> bool {
    let resp = ui.interact(r.expand(2.0), id, Sense::click());
    let t = ui.ctx().animate_bool_with_time(id, expanded, 0.12);
    let a = t * std::f32::consts::FRAC_PI_2;
    let c = r.center();
    let rot = |x: f32, y: f32| c + vec2(x * a.cos() - y * a.sin(), x * a.sin() + y * a.cos());
    let col = if resp.hovered() { pal.text } else { pal.text_dim };
    ui.painter().add(egui::Shape::line(vec![rot(-1.5, -3.5), rot(2.0, 0.0), rot(-1.5, 3.5)], Stroke::new(1.4, col)));
    resp.clicked()
}

/// Drop-target behaviour for an item while files are being dragged. Returns true if highlighted.
fn drop_target(ui: &Ui, rect: Rect, e: &Entry, env: &mut Env) -> bool {
    let Some(Drag::Files { paths, .. }) = env.drag.as_ref() else { return false };
    let Some(pp) = ui.ctx().pointer_hover_pos() else { return false };
    // An unmounted drive has no folder to drop into yet.
    if !rect.contains(pp) || paths.contains(&e.path) || tab::drive_dev(&e.path).is_some() {
        return false;
    }
    let runnable = !e.is_dir && (e.is_executable() || e.ext() == "desktop");
    if !e.is_dir && !runnable {
        return false;
    }
    if ui.input(|i| i.pointer.any_released()) {
        let m = ui.input(|i| i.modifiers);
        if e.is_dir {
            env.actions.push(Action::Drop { paths: paths.clone(), dest: e.path.clone(), copy: m.ctrl && !m.shift, link: m.ctrl && m.shift });
        } else {
            env.actions.push(Action::RunWith { exe: e.path.clone(), args: paths.clone() });
        }
        *env.drag = None;
    }
    true
}

fn paint_item_bg(p: &egui::Painter, r: Rect, selected: bool, cursor: bool, hovered: bool, drop: bool, env: &Env, rad: u8) {
    let pal = env.pal;
    let fill = if selected && cursor && env.active {
        pal.cursor
    } else if selected {
        pal.selected
    } else if hovered {
        pal.row_hover
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        p.rect_filled(r, CornerRadius::same(rad), fill);
    }
    if cursor && env.active && !selected {
        p.rect_stroke(r.shrink(0.5), CornerRadius::same(rad), Stroke::new(1.0, pal.accent.gamma_multiply(0.7)), StrokeKind::Inside);
    }
    if drop {
        p.rect_stroke(r.shrink(0.5), CornerRadius::same(rad), Stroke::new(1.5, pal.accent), StrokeKind::Inside);
    }
}

/// While renaming, every other item fades back (over 100 ms) so the edited name stands out:
/// returns true when `idx` is being renamed, and veils the item otherwise.
fn rename_focus(ui: &Ui, r: Rect, idx: u32, t: &Tab, env: &Env) -> bool {
    let editing = t.rename.as_ref().map(|rn| rn.fields.iter().any(|f| f.idx == idx)).unwrap_or(false);
    let k = ui.ctx().animate_bool_with_time(Id::new(("rename_dim", t.id)), t.rename.is_some(), if env.cfg.animations { 0.1 } else { 0.0 });
    if !editing && k > 0.001 {
        let veil = egui::LayerId::new(egui::Order::Middle, Id::new(("rename_veil", t.id)));
        ui.ctx().layer_painter(veil).with_clip_rect(ui.clip_rect()).rect_filled(r, CornerRadius::ZERO, env.pal.panel.gamma_multiply(0.72 * k));
    }
    editing
}

/// Inline rename editor. Returns true if drawn.
fn rename_field(ui: &mut Ui, rect: Rect, idx: u32, t: &mut Tab, env: &mut Env, font: &FontId) -> bool {
    let Some(r) = t.rename.as_mut() else { return false };
    let Some(fi) = r.fields.iter().position(|f| f.idx == idx) else { return false };
    let conflicts = r.conflicts();
    let bad = conflicts.contains(&fi);
    let pal = env.pal;
    let f = &r.fields[fi];
    let text = f.text_string();
    let galley = ui.fonts_mut(|fo| fo.layout_no_wrap(text.clone(), font.clone(), pal.text));
    let box_r = Rect::from_min_max(pos2(rect.left(), rect.center().y - galley.size().y / 2.0 - 3.0), pos2(rect.right(), rect.center().y + galley.size().y / 2.0 + 3.0));
    if bad {
        ui.painter().rect_stroke(box_r, CornerRadius::same(4), Stroke::new(1.0, pal.danger), StrokeKind::Inside);
    }
    let origin = pos2(box_r.left(), box_r.center().y - galley.size().y / 2.0);
    let clip = ui.clip_rect();
    let painter = ui.painter().with_clip_rect(box_r.intersect(clip));
    let (a, b) = f.selection();
    if a != b {
        let xa = galley.pos_from_cursor(egui::text::CCursor::new(a)).min.x;
        let xb = galley.pos_from_cursor(egui::text::CCursor::new(b)).min.x;
        painter.rect_filled(Rect::from_min_max(pos2(origin.x + xa, origin.y), pos2(origin.x + xb, origin.y + galley.size().y)), CornerRadius::ZERO, pal.selected);
    }
    painter.galley(origin, galley.clone(), pal.text);
    // Blinking caret
    let blink = (ui.input(|i| i.time) * 2.0) as i64 % 2 == 0;
    if blink {
        let xc = galley.pos_from_cursor(egui::text::CCursor::new(f.caret)).min.x;
        painter.line_segment([pos2(origin.x + xc, origin.y), pos2(origin.x + xc, origin.y + galley.size().y)], Stroke::new(1.5, pal.accent));
    }
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
    let resp = ui.interact(box_r, Id::new(("rename", t.id, idx)), Sense::click_and_drag());
    if resp.clicked() || resp.dragged() {
        if let Some(pp) = resp.interact_pointer_pos() {
            let c = galley.cursor_from_pos(pp - origin);
            let shift = ui.input(|i| i.modifiers.shift) || resp.dragged() && !resp.drag_started();
            r.click_at(c.index.into(), shift);
        }
    }
    if resp.double_clicked() {
        r.select_all();
    }
    if resp.secondary_clicked() {
        env.actions.push(Action::Open(Popup::rename_menu(env.panel_id, resp.interact_pointer_pos().unwrap_or(box_r.left_bottom()))));
    }
    if fi == 0 {
        if let Some(err) = &r.error {
            ui.painter().text(pos2(box_r.left(), box_r.bottom() + 2.0), Align2::LEFT_TOP, err, FontId::proportional(11.0), pal.danger);
        }
    }
    true
}

/// Rubber-band selection on the content background. `hit` maps a content-space rect to item indices.
fn rubber_band(ui: &mut Ui, bg: &Response, origin: Pos2, viewport_screen: Rect, t: &mut Tab, env: &mut Env, hit: &dyn Fn(Rect) -> Vec<u32>) {
    let mods = ui.input(|i| i.modifiers);
    if bg.drag_started_by(egui::PointerButton::Primary) {
        if let Some(pp) = bg.interact_pointer_pos() {
            let base = if mods.ctrl || mods.shift { t.selected.clone() } else { HashSet::new() };
            t.rubber = Some(Rubber { start: (pp - origin).to_pos2(), base, fade: None, last_rect: Rect::NOTHING });
        }
    }
    if let Some(rb) = t.rubber.as_mut() {
        if rb.fade.is_none() && bg.dragged_by(egui::PointerButton::Primary) {
            if let Some(pp) = ui.ctx().pointer_interact_pos() {
                let cur = (pp - origin).to_pos2();
                let r = Rect::from_two_pos(rb.start, cur);
                rb.last_rect = r;
                let mut sel = rb.base.clone();
                sel.extend(hit(r));
                t.selected = sel;
                // Auto-scroll near the edges.
                let edge = 28.0;
                let mut d = Vec2::ZERO;
                if pp.y < viewport_screen.top() + edge {
                    d.y = 14.0;
                } else if pp.y > viewport_screen.bottom() - edge {
                    d.y = -14.0;
                }
                if pp.x < viewport_screen.left() + edge {
                    d.x = 14.0;
                } else if pp.x > viewport_screen.right() - edge {
                    d.x = -14.0;
                }
                if d != Vec2::ZERO {
                    ui.scroll_with_delta(d);
                    ui.ctx().request_repaint();
                }
            }
        }
        if bg.drag_stopped() && rb.fade.is_none() {
            rb.fade = Some(std::time::Instant::now());
        }
        let alpha = match rb.fade {
            None => 1.0,
            Some(f) => 1.0 - (f.elapsed().as_secs_f32() / 0.25),
        };
        if alpha <= 0.0 || !env.cfg.animations && rb.fade.is_some() {
            t.rubber = None;
        } else {
            let sr = rb.last_rect.translate(origin.to_vec2());
            ui.painter().rect(sr, CornerRadius::same(2), env.pal.accent.gamma_multiply(0.12 * alpha), Stroke::new(1.0, env.pal.accent.gamma_multiply(0.8 * alpha)), StrokeKind::Inside);
            ui.ctx().request_repaint();
        }
    }
}

fn background_clicks(ui: &Ui, bg: &Response, t: &mut Tab, env: &mut Env) {
    let mods = ui.input(|i| i.modifiers);
    if bg.clicked() || bg.secondary_clicked() {
        if t.rename.is_some() {
            env.actions.push(Action::CommitRename(env.panel_id));
        }
        if !mods.ctrl && !mods.shift {
            t.selected.clear();
        }
    }
    if bg.double_clicked() && env.cfg.dblclick_bg_parent {
        env.actions.push(Action::FocusPanel(env.panel_id));
        env.actions.push(Action::Cmd(Cmd::Up));
    }
    if bg.secondary_clicked() {
        let pos = bg.interact_pointer_pos().unwrap_or(bg.rect.center());
        env.actions.push(Action::CtxMenu { panel: env.panel_id, pos, background: true });
    }
}

// ------------------------------------------------------------------ details

fn details(ui: &mut Ui, rect: Rect, t: &mut Tab, env: &mut Env, q: &fuzzy::Query) {
    let pal = *env.pal;
    let row_h = row_height(env.cfg);
    // As tall as the inspector's toolbar beside it, so their lines run on as one.
    let header_h = 30.0;
    let font = FontId::proportional(env.cfg.font_size);
    let small = FontId::proportional(env.cfg.font_size - 1.0);
    let pad = 10.0;
    let mut cols = t.settings.columns.clone();
    if !cols.iter().any(|(c, _)| *c == Column::Name) {
        cols.insert(0, (Column::Name, 300.0));
    }
    // The trash always shows where items came from and when they were deleted.
    if !t.trash_meta.is_empty() && !cols.iter().any(|(c, _)| matches!(c, Column::Deleted | Column::Original)) {
        cols.insert(1, (Column::Original, 220.0));
        cols.insert(2, (Column::Deleted, 140.0));
    }
    // The drive list always shows how full each volume is.
    if tab::is_computer(&t.path) && !cols.iter().any(|(c, _)| *c == Column::Usage) {
        cols.push((Column::Usage, 150.0));
    }
    let total_w: f32 = cols.iter().map(|c| c.1).sum::<f32>() + pad * 2.0;
    let body = Rect::from_min_max(pos2(rect.left(), rect.top() + header_h), rect.max);
    let content_w = total_w.max(body.width());
    // Name column stretches to fill spare width.
    let spare = (body.width() - total_w).max(0.0);
    let col_w = |c: Column, w: f32| if c == Column::Name { w + spare } else { w };
    let computer = tab::is_computer(&t.path);
    let tree = t.tree_active();
    // With a Tags column the name leaves them to it.
    let tag_col = cols.iter().any(|(c, _)| *c == Column::Tags);
    // Usage needs the parent's total.
    let needs_usage = cols.iter().any(|(c, _)| *c == Column::Usage);
    let parent_total: u64 = if needs_usage {
        t.view.iter().filter(|&&i| t.entries[i as usize].depth == 0).map(|&i| {
            let e = &t.entries[i as usize];
            if e.is_dir { env.stats.map.get(&e.path).map(|s| s.size).unwrap_or(0) } else { e.size }
        }).sum()
    } else {
        0
    };
    t.page_items = ((body.height() / row_h) as usize).max(1);
    t.nav_row_step = 1;
    t.nav_col_step = 0;
    let git = crate::gitstatus::repo_for(&t.path);
    let dim_ignored = env.cfg.git_ignored == crate::config::IgnoredFiles::Dim && git.as_ref().is_some_and(|r| !crate::gitstatus::inside_ignored(r, &t.path));
    let mut sa = egui::ScrollArea::both().id_salt(("details", t.id)).auto_shrink([false, false]);
    if let Some(s) = t.restore_scroll.take() {
        sa = sa.vertical_scroll_offset(s).horizontal_scroll_offset(0.0);
    }
    let mut body_ui = ui.new_child(UiBuilder::new().max_rect(body));
    let rows = std::mem::take(&mut t.rows);
    let group_labels = std::mem::take(&mut t.group_labels);
    let out = sa.show_viewport(&mut body_ui, |ui, vp| {
        let extra = if env.cfg.scroll_beyond { body.height() * 0.5 } else { 0.0 };
        let content_h = (rows.len() as f32 * row_h + 6.0 + extra).max(vp.height());
        let (crect, bg) = ui.allocate_exact_size(vec2(content_w, content_h), Sense::click_and_drag());
        let origin = crect.min;
        let first = (vp.min.y / row_h).floor().max(0.0) as usize;
        let last = ((vp.max.y / row_h).ceil() as usize + 1).min(rows.len());
        let viewport_screen = vp.translate(origin.to_vec2());
        // Background interactions first so rows take priority.
        background_clicks(ui, &bg, t, env);
        let rows_ref = &rows;
        let hit = |r: Rect| -> Vec<u32> {
            let a = (r.top() / row_h).floor().max(0.0) as usize;
            let b = ((r.bottom() / row_h).ceil() as usize).min(rows_ref.len());
            (a..b).filter_map(|i| if let Row::Item(x) = rows_ref[i] { Some(x) } else { None }).collect()
        };
        rubber_band(ui, &bg, origin, viewport_screen, t, env, &hit);
        let mut cursor_row = None;
        for ri in first..last {
            let y = origin.y + ri as f32 * row_h;
            let rr = Rect::from_min_size(pos2(origin.x, y), vec2(content_w, row_h));
            match &rows[ri] {
                Row::Header { label, count, collapsed } => {
                    let label = group_labels.get(*label as usize).map(String::as_str).unwrap_or("");
                    let resp = ui.interact(rr, Id::new(("ghdr", t.id, label)), Sense::click());
                    if resp.hovered() {
                        ui.painter().rect_filled(rr, CornerRadius::same(4), pal.hover);
                    }
                    let c = pos2(rr.left() + pad + 6.0, rr.center().y);
                    let g = if *collapsed { theme::Glyph::ChevronRight } else { theme::Glyph::ChevronDown };
                    let gr = Rect::from_center_size(c, Vec2::splat(10.0));
                    if *collapsed {
                        theme::paint_glyph(ui.painter(), gr, g, pal.text_dim);
                    } else {
                        ui.painter().add(egui::Shape::line(vec![c + vec2(-3.5, -1.5), c + vec2(0.0, 2.0), c + vec2(3.5, -1.5)], Stroke::new(1.4, pal.text_dim)));
                    }
                    let txt = format!("{label}  ({count})");
                    let gal = ui.painter().layout_no_wrap(txt, small.clone(), pal.text_dim);
                    let tx = rr.left() + pad + 18.0;
                    let gw = gal.size().x;
                    ui.painter().galley(pos2(tx, rr.center().y - gal.size().y / 2.0), gal, pal.text_dim);
                    ui.painter().line_segment([pos2(tx + gw + 10.0, rr.center().y), pos2(rr.right() - pad, rr.center().y)], Stroke::new(1.0, pal.border));
                    if resp.clicked() {
                        if !t.collapsed_groups.remove(label) {
                            t.collapsed_groups.insert(label.to_string());
                        }
                        t.dirty = true;
                    }
                    if resp.secondary_clicked() {
                        let pos = resp.interact_pointer_pos().unwrap_or(rr.left_bottom());
                        env.actions.push(Action::Open(Popup::GroupMenu { panel: env.panel_id, label: label.to_string(), pos }));
                    }
                }
                Row::Item(idx) => {
                    let idx = *idx;
                    let resp = ui.interact(rr, Id::new(("row", t.id, idx)), Sense::click_and_drag());
                    let e = t.entries[idx as usize].clone();
                    let selected = t.selected.contains(&idx);
                    // For a screen reader (only asked while one listens): the name, picked or not.
                    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &e.name));
                    let is_cursor = t.cursor == Some(idx);
                    if is_cursor {
                        cursor_row = Some(rr);
                        t.cursor_screen_pos = Some(pos2(rr.left() + 40.0, rr.bottom()));
                    }
                    let drop = drop_target(ui, rr, &e, env);
                    if env.cfg.alt_rows && ri % 2 == 1 && !selected {
                        ui.painter().rect_filled(rr, CornerRadius::ZERO, pal.alt);
                    }
                    // The item being renamed drops its highlight; the others fade back.
                    let editing = rename_focus(ui, rr, idx, t, env);
                    paint_item_bg(ui.painter(), rr.shrink2(vec2(3.0, 0.5)), selected && !editing, is_cursor && !editing, resp.hovered() && !editing, drop, env, 4);
                    crate::compare::paint(ui.painter(), rr, &e.path, &pal);
                    handle_item(ui, &resp, idx, t, env);
                    // Cut items, tree context rows that only lead to filter matches, and
                    // what git ignores, are faded.
                    let faded = env.cut.contains(&e.path) || t.dimmed.contains(&idx) || unmounted(t, &e) || dim_ignored && crate::gitstatus::ignored(git.as_deref(), &e.path, &t.path);
                    let dimc = |c: Color32| if faded { c.gamma_multiply(0.45) } else { c };
                    let (sub, acc, ring) = sel_colors(&pal, selected && !editing);
                    let mut x = origin.x + pad;
                    for (c, w) in &cols {
                        let w = col_w(*c, *w);
                        let cell = Rect::from_min_size(pos2(x, y), vec2(w, row_h));
                        let inner = cell.shrink2(vec2(6.0, 0.0));
                        match c {
                            Column::Name => {
                                let mut x = inner.left() + e.depth as f32 * 16.0;
                                if tree {
                                    let ch = Rect::from_min_size(pos2(x, cell.center().y - 6.0), Vec2::splat(12.0));
                                    if e.is_dir && !e.is_link && tree_chevron(ui, ch, Id::new(("chev", t.id, &e.path)), t.expanded.contains(&e.path), &pal) {
                                        // Shift+click unfolds (or folds) the whole subtree.
                                        match (ui.input(|i| i.modifiers.shift), t.expanded.contains(&e.path)) {
                                            (true, false) => t.expand_recursive(&e.path),
                                            (true, true) => t.collapse_recursive(&e.path),
                                            _ => t.toggle_expand(&e.path),
                                        }
                                    }
                                    x += 15.0;
                                }
                                let ic = Rect::from_min_size(pos2(x, cell.center().y - 8.0), Vec2::splat(16.0));
                                let cat = theme::categorize(&e.ext(), e.is_dir, e.is_executable());
                                let boxed = !computer && item_checkbox(ui, ic.shrink(1.0), idx, ui.rect_contains_pointer(rr), t, env);
                                if boxed {
                                    // The checkbox stands in for the icon.
                                } else if computer {
                                    paint_drive(ui, ic, t, &e, dimc(pal.text_dim));
                                } else {
                                    let mut ip = ui.painter().clone();
                                    if faded {
                                        ip.multiply_opacity(0.45);
                                    }
                                    if !crate::icons::entry(&ip, ic, &e) {
                                        theme::paint_icon(&ip, ic, cat, "", e.is_link);
                                    }
                                }
                                crate::gitstatus::paint(ui.painter(), ic, git.as_deref(), &e.path, &pal);
                                let name_r = Rect::from_min_max(pos2(ic.right() + 7.0, cell.top()), pos2(inner.right() - if env.recents.contains(&e.path) && env.cfg.highlight_recents { 10.0 } else { 0.0 }, cell.bottom()));
                                if !rename_field(ui, name_r, idx, t, env, &font) {
                                    let name = display_name(&e, env.cfg);
                                    let rel = e.rel();
                                    let suffix = rel.as_deref().and_then(|r| std::path::Path::new(r).parent()).filter(|p| !p.as_os_str().is_empty()).map(|p| format!("› {}", p.display()));
                                    let tags = if computer || tag_col { None } else { row_tags(&e.path) };
                                    let room = tags.as_ref().map_or(0.0, |tg| tags_width(ui, tg, &font));
                                    let mut job = name_job(&name, suffix.as_deref(), q, &font, dimc(pal.text), dimc(sub), acc, (name_r.width() - room).max(24.0), 1);
                                    // The line that matched, after the name (elided with it).
                                    if let Some(line) = t.content.snippet(&e.path) {
                                        append_match(&mut job, line, crate::query::name_part(&t.filter).trim(), &font, dimc(sub), acc);
                                    }
                                    let g = ui.fonts_mut(|f| f.layout_job(job));
                                    let gw = g.size().x;
                                    ui.painter().galley(pos2(name_r.left(), cell.center().y - g.size().y / 2.0), g, pal.text);
                                    if let Some(tg) = &tags {
                                        paint_tags(ui, name_r.left() + gw, cell.center().y, tg, &font, sub, ring);
                                    }
                                }
                                if env.cfg.highlight_recents && env.recents.contains(&e.path) {
                                    ui.painter().circle_filled(pos2(inner.right() - 3.0, cell.center().y), 2.5, pal.accent.gamma_multiply(0.8));
                                }
                            }
                            Column::Usage => {
                                let (frac, label) = if computer {
                                    match t.vols.get(&e.path) {
                                        Some(v) if !v.mounted => (0.0, String::new()),
                                        Some(v) if v.total > 0 => {
                                            let f = (v.total - v.free) as f32 / v.total as f32;
                                            (f, format!("{:.0}%", f * 100.0))
                                        }
                                        _ => (0.0, String::new()),
                                    }
                                } else {
                                    let s = if e.is_dir { env.stats.map.get(&e.path).map(|s| s.size) } else { Some(e.size) };
                                    // Nested tree rows are relative to their own parent folder.
                                    let base = if e.depth > 0 { e.path.parent().and_then(|p| env.stats.map.get(p)).map(|s| s.size).unwrap_or(0) } else { parent_total };
                                    match s {
                                        Some(s) if base > 0 => {
                                            let f = s as f32 / base as f32;
                                            (f, format!("{:.1}%", f * 100.0))
                                        }
                                        _ => (0.0, String::new()),
                                    }
                                };
                                if !label.is_empty() {
                                    // Bar with the percentage centered on it.
                                    let bar = Rect::from_min_max(pos2(inner.left(), cell.center().y - 7.0), pos2(inner.right(), cell.center().y + 7.0));
                                    ui.painter().rect_filled(bar, CornerRadius::same(4), pal.raised);
                                    let col = if computer && frac > 0.9 { pal.danger } else if computer && frac > 0.75 { Color32::from_rgb(0xe8, 0xa3, 0x3d) } else { pal.accent };
                                    ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * frac.clamp(0.0, 1.0), bar.height())), CornerRadius::same(4), col.gamma_multiply(0.75));
                                    ui.painter().text(bar.center(), Align2::CENTER_CENTER, label, FontId::proportional(env.cfg.font_size - 2.5), pal.text);
                                }
                            }
                            Column::Tags => {
                                if let Some(tg) = row_tags(&e.path) {
                                    tags_cell(ui, inner, &tg, &font, dimc(sub), ring);
                                }
                            }
                            other => {
                                let txt = match other {
                                    Column::Type => type_text(&e, t),
                                    Column::Size => entry_size_text(&e, t, env),
                                    Column::Modified => fmt_date(e.modified, env.cfg),
                                    Column::Created => fmt_date(e.created, env.cfg),
                                    Column::Accessed => fmt_date(e.accessed, env.cfg),
                                    Column::Deleted => fmt_date(t.trash_meta.get(&e.path).and_then(|m| m.1), env.cfg),
                                    Column::Original => t.trash_meta.get(&e.path).and_then(|m| m.0.parent()).map(|p| p.display().to_string()).unwrap_or_default(),
                                    Column::Folders => if e.is_dir { env.stats.map.get(&e.path).map(|s| abbrev(s.folders)).unwrap_or_else(|| if e.is_link { "--".into() } else { String::new() }) } else { "--".into() },
                                    Column::Files => if e.is_dir { env.stats.map.get(&e.path).map(|s| abbrev(s.files)).unwrap_or_else(|| if e.is_link { "--".into() } else { String::new() }) } else { "--".into() },
                                    Column::Permissions => fsx::fmt_mode(e.mode, e.is_dir),
                                    Column::Extension => e.ext().to_string(),
                                    Column::Taken => fmt_date(crate::facts::get(&e).and_then(|f| f.taken), env.cfg),
                                    Column::Dimensions => crate::facts::get(&e).and_then(|f| f.dims).map(crate::facts::fmt_dims).unwrap_or_default(),
                                    Column::Duration => crate::facts::get(&e).and_then(|f| f.duration).map(crate::media::fmt_time).unwrap_or_default(),
                                    _ => String::new(),
                                };
                                let g = elided(ui, &txt, small.clone(), inner.width(), dimc(sub));
                                let gx = if other.right_aligned() { inner.right() - g.size().x } else { inner.left() };
                                ui.painter().galley(pos2(gx, cell.center().y - g.size().y / 2.0), g, sub);
                            }
                        }
                        x += w;
                    }
                }
            }
        }
        if t.scroll_to_cursor {
            t.scroll_to_cursor = false;
            let target = cursor_row.or_else(|| {
                let c = t.cursor?;
                let ri = rows.iter().position(|r| matches!(r, Row::Item(i) if *i == c))?;
                Some(Rect::from_min_size(pos2(origin.x, origin.y + ri as f32 * row_h), vec2(10.0, row_h)))
            });
            if let Some(r) = target {
                ui.scroll_to_rect(r.expand2(vec2(0.0, 2.0)), None);
            }
        }
    });
    t.rows = rows;
    t.group_labels = group_labels;
    t.current_scroll = out.state.offset.y;
    t.hscroll = out.state.offset.x;
    // Header (scrolls horizontally with the body).
    let header = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + header_h));
    let painter = ui.painter().with_clip_rect(header.intersect(ui.clip_rect()));
    painter.line_segment([header.left_bottom(), header.right_bottom()], Stroke::new(1.0, pal.border));
    let mut x = header.left() + pad - t.hscroll;
    let mut new_widths: Option<(usize, f32)> = None;
    let mut toggle_col: Option<Column> = None;
    let mut sort_click: Option<SortKey> = None;
    for (ci, (c, w)) in cols.iter().enumerate() {
        let w = col_w(*c, *w);
        let cell = Rect::from_min_size(pos2(x, header.top()), vec2(w, header_h));
        let resp = ui.interact(cell.intersect(header), Id::new(("hdr", t.id, ci)), Sense::click());
        let active_sort = c.sort_key() == Some(t.settings.sort);
        if resp.hovered() {
            painter.rect_filled(cell, CornerRadius::same(4), pal.hover);
        }
        // Thin separator at each column's right edge (the resize handle).
        painter.line_segment([pos2(cell.right() - 0.5, cell.top() + 6.0), pos2(cell.right() - 0.5, cell.bottom() - 6.0)], Stroke::new(1.0, pal.border));
        let label = c.title();
        let col = if active_sort { pal.text } else { pal.text_dim };
        let g = elided(ui, label, small.clone(), w - 26.0, col);
        let boxes = env.cfg.selection_boxes && !tab::is_computer(&t.path);
        let gx = if c.right_aligned() { cell.right() - 8.0 - g.size().x - if active_sort { 12.0 } else { 0.0 } } else if *c == Column::Name && boxes { cell.left() + 29.0 } else { cell.left() + 6.0 };
        if *c == Column::Name && boxes {
            // Select-all checkbox, in line with the rows' icon slot.
            let cb = Rect::from_min_size(pos2(cell.left() + 7.0, cell.center().y - 7.0), Vec2::splat(14.0));
            let all = !t.view.is_empty() && t.selected.len() >= t.view.len() && t.view.iter().all(|i| t.selected.contains(i));
            crate::popups::checkbox_glyph(&painter, cb, all, &pal);
            if ui.interact(cb.expand(2.0).intersect(header), Id::new(("hdr_all", t.id)), Sense::click()).on_hover_text("Select all / none").clicked() {
                if all { t.selected.clear(); } else { t.selected = t.view.iter().copied().collect(); }
            }
        }
        let gw = g.size().x;
        painter.galley(pos2(gx, cell.center().y - g.size().y / 2.0), g, col);
        if active_sort {
            // The sort arrow sits at the column's right edge (left of the Type funnel).
            let ax = if c.right_aligned() { cell.right() - 12.0 } else if *c == Column::Type { cell.right() - 30.0 } else { (cell.right() - 12.0).max(gx + gw + 8.0) };
            let ac = pos2(ax, cell.center().y);
            let d = if t.settings.desc { 1.0 } else { -1.0 };
            painter.add(egui::Shape::convex_polygon(vec![ac + vec2(-3.5, -1.5 * d), ac + vec2(3.5, -1.5 * d), ac + vec2(0.0, 2.5 * d)], pal.accent, Stroke::NONE));
        }
        if *c == Column::Type {
            let fr = Rect::from_center_size(pos2(cell.right() - 12.0, cell.center().y), Vec2::splat(16.0));
            t.funnel_at = Some(fr.left_bottom() + vec2(-140.0, 4.0));
            let fresp = ui.interact(fr, Id::new(("hdr_funnel", t.id)), Sense::click()).on_hover_text("Filter by type (Alt+T)");
            let active = !t.settings.only_types.is_empty();
            if fresp.hovered() || active {
                painter.rect_filled(fr, CornerRadius::same(3), if active { pal.accent.gamma_multiply(0.3) } else { pal.raised });
            }
            crate::ui::funnel(&painter, fr.center(), if active { pal.accent } else { pal.text_dim });
            if fresp.clicked() {
                env.actions.push(Action::FocusPanel(env.panel_id));
                env.actions.push(Action::Open(Popup::types(env.panel_id)));
            }
        }
        let resp = resp.on_hover_text("Click to sort · right-click to choose columns");
        if resp.clicked() {
            if let Some(k) = c.sort_key() {
                sort_click = Some(k);
            }
        }
        resp.context_menu(|ui| {
            ui.label(egui::RichText::new("Columns").small().color(pal.text_dim));
            for cc in Column::ALL {
                if cc == Column::Name {
                    continue;
                }
                let mut on = cols.iter().any(|(x, _)| *x == cc);
                if ui.checkbox(&mut on, cc.title()).changed() {
                    toggle_col = Some(cc);
                }
            }
        });
        // Resize handle
        let hr = Rect::from_center_size(pos2(cell.right(), cell.center().y), vec2(8.0, header_h));
        let hresp = ui.interact(hr.intersect(header), Id::new(("hdr_rs", t.id, ci)), Sense::click_and_drag());
        if hresp.hovered() || hresp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeColumn);
            painter.line_segment([pos2(cell.right(), cell.top() + 5.0), pos2(cell.right(), cell.bottom() - 5.0)], Stroke::new(1.0, pal.accent));
        } else if ci + 1 < cols.len() {
            painter.line_segment([pos2(cell.right(), cell.top() + 7.0), pos2(cell.right(), cell.bottom() - 7.0)], Stroke::new(1.0, pal.border));
        }
        if hresp.dragged() {
            let dx = hresp.drag_delta().x;
            new_widths = Some((ci, (cols[ci].1 + dx).max(40.0)));
        }
        if hresp.double_clicked() {
            new_widths = Some((ci, c.default_width()));
        }
        x += w;
    }
    if let Some((ci, w)) = new_widths {
        if let Some(c) = t.settings.columns.iter_mut().find(|(c, _)| *c == cols[ci].0) {
            c.1 = w;
        }
    }
    if let Some(k) = sort_click {
        t.set_sort(k);
    }
    if let Some(cc) = toggle_col {
        if let Some(pos) = t.settings.columns.iter().position(|(c, _)| *c == cc) {
            t.settings.columns.remove(pos);
        } else {
            t.settings.columns.push((cc, cc.default_width()));
        }
    }
}

// ------------------------------------------------------------------ list / columns / icons

fn grid(ui: &mut Ui, rect: Rect, t: &mut Tab, env: &mut Env, q: &fuzzy::Query, mode: Mode) {
    // Nothing inside an archive is on disk to make a thumbnail from.
    let thumbs_here = t.archive.is_none() && env.cfg.thumbs_on(crate::fsx::drive_kind_cached(&t.path, ui.ctx()));
    let pal = *env.pal;
    let font = FontId::proportional(env.cfg.font_size);
    let small = FontId::proportional(env.cfg.font_size - 1.5);
    let row_h = row_height(env.cfg);
    let n = t.view.len();
    let body = rect.shrink2(vec2(6.0, 4.0));
    // Geometry
    let (cell, cols, rows_per_col, horizontal) = match mode {
        Mode::List => {
            let rpc = (((body.height() - 4.0) / row_h).floor() as usize).max(1);
            (vec2(260.0 * env.cfg.spacing.max(0.8), row_h), (n + rpc - 1) / rpc, rpc, true)
        }
        Mode::Icons(s) => {
            let text_h = if s < 72.0 { env.cfg.font_size * 2.7 } else { env.cfg.font_size * 3.8 };
            // Cell widths: 66 / 122 / 178 for 48 / 96 / 144 icons.
            let cw = s * 1.17 + 10.0;
            let cols = ((body.width() / cw).floor() as usize).max(1);
            (vec2(cw, s + text_h + 16.0), cols, 0, false)
        }
        Mode::Details | Mode::Tree | Mode::Columns => unreachable!(),
    };
    // Spread leftover width between icon cells (items keep their size).
    let gap = if let Mode::Icons(_) = mode { ((body.width() - cols as f32 * cell.x) / cols as f32).max(0.0) } else { 0.0 };
    let step = vec2(cell.x + gap, cell.y);
    let pos_of = |i: usize| -> Vec2 {
        if horizontal {
            vec2((i / rows_per_col) as f32 * step.x, (i % rows_per_col) as f32 * step.y)
        } else {
            vec2((i % cols) as f32 * step.x, (i / cols) as f32 * step.y)
        }
    };
    let content = if horizontal {
        vec2((cols as f32 * step.x + 8.0).max(body.width()), body.height())
    } else {
        let rows = (n + cols - 1) / cols.max(1);
        let extra = if env.cfg.scroll_beyond { body.height() * 0.5 } else { 0.0 };
        vec2(body.width(), (rows as f32 * step.y + 8.0 + extra).max(body.height()))
    };
    if horizontal {
        t.nav_row_step = 1;
        t.nav_col_step = rows_per_col;
        t.page_items = rows_per_col;
    } else {
        t.nav_row_step = cols;
        t.nav_col_step = 1;
        t.page_items = cols * ((body.height() / step.y).floor() as usize).max(1);
    }
    let mut sa = if horizontal { egui::ScrollArea::horizontal() } else { egui::ScrollArea::vertical() }.id_salt(("grid", t.id, horizontal)).auto_shrink([false, false]);
    if let Some(s) = t.restore_scroll.take() {
        sa = if horizontal { sa.horizontal_scroll_offset(0.0) } else { sa.vertical_scroll_offset(s) };
    }
    let mut body_ui = ui.new_child(UiBuilder::new().max_rect(body));
    // Cloned: item handlers mutate selection using `t.view`.
    let view = t.view.clone();
    let now = std::time::Instant::now();
    let git = crate::gitstatus::repo_for(&t.path);
    let dim_ignored = env.cfg.git_ignored == crate::config::IgnoredFiles::Dim && git.as_ref().is_some_and(|r| !crate::gitstatus::inside_ignored(r, &t.path));
    let out = sa.show_viewport(&mut body_ui, |ui, vp| {
        // Vertical wheel scrolls the horizontal List view.
        if horizontal && ui.rect_contains_pointer(body) {
            let dy = ui.input(|i| i.smooth_scroll_delta.y);
            if dy != 0.0 {
                ui.scroll_with_delta(vec2(dy, 0.0));
            }
        }
        let (crect, bg) = ui.allocate_exact_size(content, Sense::click_and_drag());
        let origin = crect.min;
        let viewport_screen = vp.translate(origin.to_vec2());
        background_clicks(ui, &bg, t, env);
        let view_ref = &view;
        let hit = |r: Rect| -> Vec<u32> {
            // Cells intersecting the band.
            let mut out = vec![];
            for (i, &idx) in view_ref.iter().enumerate() {
                let p = pos_of(i);
                if Rect::from_min_size(p.to_pos2(), cell).intersects(r) {
                    out.push(idx);
                }
            }
            out
        };
        rubber_band(ui, &bg, origin, viewport_screen, t, env, &hit);
        // Visible range
        let (first, last) = if horizontal {
            let c0 = (vp.min.x / step.x).floor().max(0.0) as usize;
            let c1 = (vp.max.x / step.x).ceil() as usize + 1;
            (c0 * rows_per_col, (c1 * rows_per_col).min(n))
        } else {
            let r0 = (vp.min.y / step.y).floor().max(0.0) as usize;
            let r1 = (vp.max.y / step.y).ceil() as usize + 1;
            (r0 * cols, (r1 * cols).min(n))
        };
        let mut cursor_rect = None;
        for i in first..last {
            let idx = view[i];
            let r = Rect::from_min_size(origin + pos_of(i), cell);
            let e = t.entries[idx as usize].clone();
            let resp = ui.interact(r, Id::new(("cell", t.id, idx)), Sense::click_and_drag());
            let selected = t.selected.contains(&idx);
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &e.name));
            let is_cursor = t.cursor == Some(idx);
            if is_cursor {
                cursor_rect = Some(r);
                t.cursor_screen_pos = Some(r.center_bottom());
            }
            let drop = drop_target(ui, r, &e, env);
            let faded = env.cut.contains(&e.path) || unmounted(t, &e) || dim_ignored && crate::gitstatus::ignored(git.as_deref(), &e.path, &t.path);
            let dimc = |c: Color32| if faded { c.gamma_multiply(0.45) } else { c };
            let cat = theme::categorize(&e.ext(), e.is_dir, e.is_executable());
            match mode {
                Mode::List => {
                    let editing = rename_focus(ui, r, idx, t, env);
                    paint_item_bg(ui.painter(), r.shrink2(vec2(2.0, 0.5)), selected && !editing, is_cursor && !editing, resp.hovered() && !editing, drop, env, 4);
                    crate::compare::paint(ui.painter(), r, &e.path, &pal);
                    handle_item(ui, &resp, idx, t, env);
                    let ic = Rect::from_min_size(pos2(r.left() + 6.0, r.center().y - 8.0), Vec2::splat(16.0));
                    let boxed = !tab::is_computer(&t.path) && item_checkbox(ui, ic.shrink(1.0), idx, ui.rect_contains_pointer(r), t, env);
                    if boxed {
                        // The checkbox stands in for the icon.
                    } else if tab::is_computer(&t.path) {
                        paint_drive(ui, ic, t, &e, dimc(pal.text_dim));
                    } else {
                        if !crate::icons::entry(ui.painter(), ic, &e) {
                            theme::paint_icon(ui.painter(), ic, cat, "", e.is_link);
                        }
                    }
                    crate::gitstatus::paint(ui.painter(), ic, git.as_deref(), &e.path, &pal);
                    let name_r = Rect::from_min_max(pos2(ic.right() + 7.0, r.top()), pos2(r.right() - 8.0, r.bottom()));
                    if !rename_field(ui, name_r, idx, t, env, &font) {
                        let tags = row_tags(&e.path);
                        let room = tags.as_ref().map_or(0.0, |tg| tags_width(ui, tg, &font));
                        let (sub, acc, ring) = sel_colors(&pal, selected && !editing);
                        let job = name_job(&display_name(&e, env.cfg), None, q, &font, dimc(pal.text), sub, acc, (name_r.width() - room).max(24.0), 1);
                        let g = ui.fonts_mut(|f| f.layout_job(job));
                        let gw = g.size().x;
                        ui.painter().galley(pos2(name_r.left(), r.center().y - g.size().y / 2.0), g, pal.text);
                        if let Some(tg) = &tags {
                            paint_tags(ui, name_r.left() + gw, r.center().y, tg, &font, sub, ring);
                        }
                    }
                }
                Mode::Icons(s) => {
                    let rad = if env.cfg.round_corners { 8 } else { 0 };
                    let editing = rename_focus(ui, r, idx, t, env);
                    paint_item_bg(ui.painter(), r.shrink(2.0), selected && !editing, is_cursor && !editing, resp.hovered() && !editing, drop, env, rad);
                    handle_item(ui, &resp, idx, t, env);
                    let icon_r = Rect::from_min_size(pos2(r.center().x - s / 2.0, r.top() + 8.0), Vec2::splat(s));
                    crate::compare::paint(ui.painter(), Rect::from_min_max(pos2(r.left() + 2.0, icon_r.top()), icon_r.max), &e.path, &pal);
                    let mut drew = false;
                    let want_thumb = thumbs_here && s >= 40.0 && thumbs::can_thumb(&e);
                    if want_thumb {
                        let ppp = ui.ctx().pixels_per_point();
                        if let Some(l) = env.thumbs.get(&e.path, e.modified, s * ppp) {
                            let [tw, th] = l.tex.size();
                            let (tw, th) = (tw as f32, th as f32);
                            let alpha = if env.cfg.animations { (now.duration_since(l.at).as_secs_f32() / 0.18).min(1.0) } else { 1.0 };
                            if alpha < 1.0 {
                                ui.ctx().request_repaint();
                            }
                            let (dst, uv) = if env.cfg.thumb_fill {
                                // Crop to a square
                                let side = tw.min(th);
                                let u0 = (tw - side) / 2.0 / tw;
                                let v0 = (th - side) / 2.0 / th;
                                (icon_r, Rect::from_min_max(pos2(u0, v0), pos2(1.0 - u0, 1.0 - v0)))
                            } else {
                                let k = (s / tw).min(s / th);
                                let size = vec2(tw * k, th * k);
                                (Rect::from_min_size(pos2(icon_r.center().x - size.x / 2.0, icon_r.bottom() - size.y), size), Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)))
                            };
                            ui.painter().image(l.tex.id(), dst, uv, Color32::WHITE.gamma_multiply(alpha * if faded { 0.45 } else { 1.0 }));
                            drew = true;
                        }
                    }
                    if !drew {
                        if tab::is_computer(&t.path) {
                            paint_drive(ui, icon_r.shrink(s * 0.15), t, &e, dimc(pal.text_dim));
                        } else {
                            if !crate::icons::entry(ui.painter(), icon_r, &e) {
                                theme::paint_icon(ui.painter(), icon_r, cat, &e.ext(), e.is_link);
                            }
                        }
                    }
                    crate::gitstatus::paint(ui.painter(), icon_r.shrink(s * 0.1), git.as_deref(), &e.path, &pal);
                    // Colour labels as a badge on the icon's top right corner.
                    if let Some((dots, _)) = row_tags(&e.path).filter(|t| !t.0.is_empty()) {
                        let rr = (s * 0.07).clamp(3.5, 6.0);
                        let w = (rr * 2.0 + 2.5) * dots.len() as f32 - 2.5;
                        paint_dots(ui.painter(), icon_r.right() - w, icon_r.top() + rr + 1.0, &dots, rr, pal.bg);
                    }
                    let text_r = Rect::from_min_max(pos2(r.left() + 5.0, icon_r.bottom() + 5.0), pos2(r.right() - 5.0, r.bottom() - 3.0));
                    let name_line = Rect::from_min_size(text_r.min, vec2(text_r.width(), font.size + 6.0));
                    if !rename_field(ui, name_line, idx, t, env, &font) {
                        let rows = if s < 72.0 { 1 } else { 2 };
                        let (sub, acc, _) = sel_colors(&pal, selected && !editing);
                        let mut job = name_job(&display_name(&e, env.cfg), None, q, &font, dimc(pal.text), sub, acc, text_r.width(), rows);
                        job.halign = egui::Align::Center;
                        let g = ui.fonts_mut(|f| f.layout_job(job));
                        let gh = g.size().y;
                        ui.painter().galley(pos2(text_r.center().x, text_r.top()), g, pal.text);
                        {
                            let size = entry_size_text(&e, t, env);
                            let size = if size.is_empty() { "--".to_string() } else { size };
                            if gh < text_r.height() - 8.0 {
                                ui.painter().text(pos2(text_r.center().x, text_r.top() + gh + 1.0), Align2::CENTER_TOP, size, small.clone(), sub);
                            }
                        }
                    }
                    if env.cfg.highlight_recents && env.recents.contains(&e.path) {
                        ui.painter().circle_filled(pos2(r.right() - 9.0, r.top() + 9.0), 3.0, pal.accent.gamma_multiply(0.8));
                    }
                    item_checkbox(ui, Rect::from_min_size(r.min + vec2(7.0, 7.0), Vec2::splat(15.0)), idx, ui.rect_contains_pointer(r), t, env);
                }
                Mode::Details | Mode::Tree | Mode::Columns => {}
            }
        }
        if t.scroll_to_cursor {
            t.scroll_to_cursor = false;
            let target = cursor_rect.or_else(|| {
                let c = t.cursor?;
                let i = view.iter().position(|&x| x == c)?;
                Some(Rect::from_min_size(origin + pos_of(i), cell))
            });
            if let Some(r) = target {
                ui.scroll_to_rect(r, None);
            }
        }
    });
    t.current_scroll = if horizontal { 0.0 } else { out.state.offset.y };
}

// ------------------------------------------------------------------ columns

/// A column's width: room for about thirty characters of a name, growing with the font
/// size and the Spacing option.
pub fn column_width(cfg: &Config) -> f32 {
    (cfg.font_size * 16.0 + 56.0 * cfg.spacing).round()
}

/// The Columns view: the strip's folders side by side, the tab's own among them, scrolled
/// to keep it (and the folder under its cursor) in sight. Returns the tab's column, where
/// the empty and error states go.
fn columns(ui: &mut Ui, rect: Rect, t: &mut Tab, env: &mut Env, q: &fuzzy::Query) -> Rect {
    {
        let c = tab::ViewCtx { show_hidden: env.cfg.show_hidden, show_system: env.cfg.show_system, folders_first: env.cfg.folders_first, recents_on_top: env.cfg.recents_on_top, recents: env.recents, stats: &env.stats.map };
        crate::columns::sync(t, &c);
    }
    let Some(mut st) = t.strip.take() else { return rect };
    let w = column_width(env.cfg).min(rect.width().max(60.0));
    let focus = st.chain.len();
    let n = focus + 1 + usize::from(st.preview.is_some());
    let end = (n as f32 * w - rect.width()).max(0.0);
    // When the columns change, scroll as little as keeps the tab's column and the one
    // after it in sight (the tab's own wins in a narrow panel). In between, the wheel
    // moves freely along the strip.
    let key = Id::new((&t.path, &st.preview, (w * 4.0) as i32, (rect.width() * 4.0) as i32)).value();
    let changed = std::mem::replace(&mut st.laid_for, key) != key;
    if changed || !st.placed {
        // After a click the clicked column holds still: only its own must be in sight.
        let (lo, hi) = (focus as f32 * w, if st.held { (focus + 1) as f32 * w } else { n as f32 * w });
        let mut x = st.target;
        if hi - x > rect.width() {
            x = hi - rect.width();
        }
        if lo < x {
            x = lo;
        }
        st.target = if st.held { x.max(0.0) } else { x.clamp(0.0, end) };
    }
    if ui.rect_contains_pointer(rect) {
        let dx = ui.input(|i| i.smooth_scroll_delta.x);
        if dx != 0.0 {
            st.target = (st.target - dx).clamp(0.0, end.max(st.target));
            st.scroll = st.target;
        }
    }
    if !st.placed || !env.cfg.animations || (st.scroll - st.target).abs() < 0.5 {
        st.scroll = st.target;
        st.placed = true;
    } else {
        let dt = ui.input(|i| i.stable_dt).min(0.05);
        st.scroll += (st.target - st.scroll) * (dt * 20.0).min(1.0);
        ui.ctx().request_repaint();
    }
    t.hscroll = 0.0;
    let mut own = rect;
    for k in 0..n {
        let cr = Rect::from_min_size(pos2(rect.left() + k as f32 * w - st.scroll.round(), rect.top()), vec2(w, rect.height()));
        let vis = cr.intersect(rect);
        if !vis.is_positive() {
            continue;
        }
        let mut cui = ui.new_child(UiBuilder::new().max_rect(cr).id_salt(("col", t.id, k)));
        cui.set_clip_rect(vis.intersect(ui.clip_rect()));
        if k == focus {
            own = vis;
            own_column(&mut cui, cr, t, env, q, &mut st.own_target);
        } else {
            let path = if k < focus { st.chain[k].clone() } else { st.preview.clone().unwrap_or_default() };
            if let Some(i) = st.cols.iter().position(|c| c.tab.path == path) {
                let child = st.cols[i].child.filter(|_| k < focus);
                side_column(&mut cui, cr, &mut st.cols[i].tab, child, env);
            }
        }
        ui.painter().with_clip_rect(rect).line_segment([pos2(cr.right() - 0.5, rect.top()), pos2(cr.right() - 0.5, rect.bottom())], Stroke::new(1.0, env.pal.border));
    }
    t.strip = Some(st);
    own
}

/// What a column row shows: icon, name (the filter's letters marked) and, for a
/// folder, the chevron leading on. `icon` false when something else took its place.
#[allow(clippy::too_many_arguments)]
fn column_cells(ui: &Ui, r: Rect, ic: Rect, e: &Entry, env: &Env, q: &fuzzy::Query, selected: bool, faded: bool, icon: bool, git: Option<&crate::gitstatus::Repo>) {
    let pal = env.pal;
    let font = FontId::proportional(env.cfg.font_size);
    let dimc = |c: Color32| if faded { c.gamma_multiply(0.45) } else { c };
    if icon {
        let mut ip = ui.painter().clone();
        if faded {
            ip.multiply_opacity(0.45);
        }
        if !crate::icons::entry(&ip, ic, e) {
            theme::paint_icon(&ip, ic, theme::categorize(&e.ext(), e.is_dir, e.is_executable()), "", e.is_link);
        }
    }
    crate::gitstatus::paint(ui.painter(), ic, git, &e.path, pal);
    let (sub, acc, ring) = sel_colors(pal, selected);
    let chev = e.is_dir && tab::drive_dev(&e.path).is_none();
    let recent = env.cfg.highlight_recents && env.recents.contains(&e.path);
    // From the right: the chevron (as big as the text asks), the recents dot, the name.
    let cs = (env.cfg.font_size * 0.72).clamp(9.0, 15.0);
    let chev_w = if chev { cs + 10.0 } else { 6.0 };
    let dot_x = r.right() - chev_w - 4.0;
    let name_r = Rect::from_min_max(pos2(ic.right() + 7.0, r.top()), pos2(if recent { dot_x - 6.0 } else { r.right() - chev_w - 2.0 }, r.bottom()));
    let tags = row_tags(&e.path);
    let room = tags.as_ref().map_or(0.0, |tg| tags_width(ui, tg, &font));
    let rel = e.rel();
    let suffix = rel.as_deref().and_then(|r| std::path::Path::new(r).parent()).filter(|p| !p.as_os_str().is_empty()).map(|p| format!("› {}", p.display()));
    let job = name_job(&display_name(e, env.cfg), suffix.as_deref(), q, &font, dimc(pal.text), dimc(sub), acc, (name_r.width() - room).max(24.0), 1);
    let g = ui.fonts_mut(|f| f.layout_job(job));
    let gw = g.size().x;
    ui.painter().galley(pos2(name_r.left(), r.center().y - g.size().y / 2.0), g, pal.text);
    if let Some(tg) = &tags {
        paint_tags(ui, name_r.left() + gw, r.center().y, tg, &font, sub, ring);
    }
    if chev {
        let cr = Rect::from_center_size(pos2(r.right() - 6.0 - cs / 2.0, r.center().y), Vec2::splat(cs));
        theme::paint_glyph(ui.painter(), cr, theme::Glyph::ChevronRight, dimc(if selected { pal.on_sel_dim() } else { pal.text_dim }));
    }
    if recent {
        ui.painter().circle_filled(pos2(dot_x, r.center().y), 2.5, pal.accent.gamma_multiply(0.8));
    }
}

/// The tab's own column: everything a List view row does (selection, rubber band,
/// rename, drag and drop, menus), one folder tall.
fn own_column(ui: &mut Ui, cr: Rect, t: &mut Tab, env: &mut Env, q: &fuzzy::Query, eased: &mut Option<f32>) {
    let pal = *env.pal;
    let font = FontId::proportional(env.cfg.font_size);
    let row_h = row_height(env.cfg);
    let body = cr.shrink2(vec2(4.0, 4.0));
    t.page_items = ((body.height() / row_h).floor() as usize).max(1);
    t.nav_row_step = 1;
    t.nav_col_step = 0;
    let computer = tab::is_computer(&t.path);
    let git = crate::gitstatus::repo_for(&t.path);
    let dim_ignored = env.cfg.git_ignored == crate::config::IgnoredFiles::Dim && git.as_ref().is_some_and(|r| !crate::gitstatus::inside_ignored(r, &t.path));
    let mut sa = egui::ScrollArea::vertical().id_salt(("colv", t.id)).auto_shrink([false, false]);
    let view = t.view.clone();
    // The cursor kept in sight by an offset worked out here and eased toward, in the
    // views' motion: egui's own scroll-to works from a delta that goes stale when a second
    // request comes while the first still moves (End right after Home stopped short).
    if t.scroll_to_cursor {
        t.scroll_to_cursor = false;
        if let Some(i) = t.cursor.and_then(|c| view.iter().position(|&x| x == c)) {
            let (top, bottom, h) = (i as f32 * row_h, (i + 1) as f32 * row_h, body.height());
            let cur = eased.unwrap_or(t.current_scroll);
            let to = if top < cur { top } else if bottom > cur + h { bottom - h } else { cur };
            let to = to.clamp(0.0, (view.len() as f32 * row_h - h).max(0.0));
            if (to - t.current_scroll).abs() > 0.5 {
                *eased = Some(to);
            }
        }
    }
    if let Some(s) = t.restore_scroll.take() {
        *eased = None;
        sa = sa.vertical_scroll_offset(s);
    } else if let Some(to) = *eased {
        let cur = t.current_scroll;
        let next = if !env.cfg.animations || (to - cur).abs() < 1.0 {
            *eased = None;
            to
        } else {
            ui.ctx().request_repaint();
            cur + (to - cur) * (ui.input(|i| i.stable_dt).min(0.05) * 30.0).min(1.0)
        };
        sa = sa.vertical_scroll_offset(next);
    }
    let mut body_ui = ui.new_child(UiBuilder::new().max_rect(body));
    let out = sa.show_viewport(&mut body_ui, |ui, vp| {
        let content_h = (view.len() as f32 * row_h).max(vp.height() - 1.0);
        let (crect, bg) = ui.allocate_exact_size(vec2(body.width(), content_h), Sense::click_and_drag());
        let origin = crect.min;
        let viewport_screen = vp.translate(origin.to_vec2());
        background_clicks(ui, &bg, t, env);
        let view_ref = &view;
        let hit = |r: Rect| -> Vec<u32> {
            let a = (r.top() / row_h).floor().max(0.0) as usize;
            let b = ((r.bottom() / row_h).ceil() as usize).min(view_ref.len());
            view_ref.get(a.min(b)..b).map(|s| s.to_vec()).unwrap_or_default()
        };
        rubber_band(ui, &bg, origin, viewport_screen, t, env, &hit);
        let first = (vp.min.y / row_h).floor().max(0.0) as usize;
        let last = ((vp.max.y / row_h).ceil() as usize + 1).min(view.len());
        for (i, &idx) in view.iter().enumerate().take(last).skip(first) {
            let r = Rect::from_min_size(origin + vec2(0.0, i as f32 * row_h), vec2(body.width(), row_h));
            let e = t.entries[idx as usize].clone();
            // Rows are known by path, in whichever column: the second click of a
            // double-click lands on the row the first one made the tab's.
            let resp = ui.interact(r, Id::new(("colrow", env.panel_id, &e.path)), Sense::click_and_drag());
            let selected = t.selected.contains(&idx);
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &e.name));
            let is_cursor = t.cursor == Some(idx);
            if is_cursor {
                t.cursor_screen_pos = Some(pos2(r.left() + 40.0, r.bottom()));
            }
            let drop = drop_target(ui, r, &e, env);
            let faded = env.cut.contains(&e.path) || unmounted(t, &e) || dim_ignored && crate::gitstatus::ignored(git.as_deref(), &e.path, &t.path);
            let editing = rename_focus(ui, r, idx, t, env);
            paint_item_bg(ui.painter(), r.shrink2(vec2(2.0, 0.5)), selected && !editing, is_cursor && !editing, resp.hovered() && !editing, drop, env, 4);
            crate::compare::paint(ui.painter(), r, &e.path, &pal);
            handle_item(ui, &resp, idx, t, env);
            let ic = Rect::from_min_size(pos2(r.left() + 6.0, r.center().y - 8.0), Vec2::splat(16.0));
            let boxed = !computer && item_checkbox(ui, ic.shrink(1.0), idx, ui.rect_contains_pointer(r), t, env);
            if computer {
                paint_drive(ui, ic, t, &e, if faded { pal.text_dim.gamma_multiply(0.45) } else { pal.text_dim });
            }
            let name_r = Rect::from_min_max(pos2(ic.right() + 7.0, r.top()), pos2(r.right() - 8.0, r.bottom()));
            if !rename_field(ui, name_r, idx, t, env, &font) {
                column_cells(ui, r, ic, &e, env, q, selected && !editing, faded, !boxed && !computer, git.as_deref());
            }
        }
    });
    t.current_scroll = out.state.offset.y;
}

/// A column beside the tab's: an ancestor with the way on highlighted, or the folder
/// under the cursor. A click here makes its folder the tab's, with what was clicked
/// picked; a right-click does that and opens the menu; things dragged onto it land in it.
fn side_column(ui: &mut Ui, cr: Rect, col: &mut Tab, child: Option<u32>, env: &mut Env) {
    let row_h = row_height(env.cfg);
    let body = cr.shrink2(vec2(4.0, 4.0));
    let panel = env.panel_id;
    let dir = col.path.clone();
    let git = crate::gitstatus::repo_for(&dir);
    let none = fuzzy::Query::new("");
    let mut sa = egui::ScrollArea::vertical().id_salt(("colv", col.id)).auto_shrink([false, false]);
    if let Some(s) = col.restore_scroll.take() {
        sa = sa.vertical_scroll_offset(s);
    }
    let mut body_ui = ui.new_child(UiBuilder::new().max_rect(body));
    let mut go: Option<(Option<PathBuf>, Option<Pos2>)> = None;
    sa.show_viewport(&mut body_ui, |ui, vp| {
        let n = col.view.len();
        let content_h = (n as f32 * row_h).max(vp.height() - 1.0);
        let (crect, bg) = ui.allocate_exact_size(vec2(body.width(), content_h), Sense::click());
        let origin = crect.min;
        if bg.clicked() {
            go = Some((None, None));
        }
        if bg.secondary_clicked() {
            go = Some((None, bg.interact_pointer_pos()));
        }
        let first = (vp.min.y / row_h).floor().max(0.0) as usize;
        let last = ((vp.max.y / row_h).ceil() as usize + 1).min(n);
        for i in first..last {
            let idx = col.view[i];
            let Some(e) = col.entries.get(idx as usize) else { continue };
            let r = Rect::from_min_size(origin + vec2(0.0, i as f32 * row_h), vec2(body.width(), row_h));
            let resp = ui.interact(r, Id::new(("colrow", panel, &e.path)), Sense::click_and_drag());
            let on = child == Some(idx);
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, on, &e.name));
            let drop = drop_target(ui, r, e, env);
            // The way down is marked quieter than a selection: the picking happens in the
            // tab's own column.
            if on {
                ui.painter().rect_filled(r.shrink2(vec2(2.0, 0.5)), CornerRadius::same(4), env.pal.panel.lerp_to_gamma(env.pal.selected, 0.6));
            }
            paint_item_bg(ui.painter(), r.shrink2(vec2(2.0, 0.5)), false, false, resp.hovered() && !on, drop, env, 4);
            let ic = Rect::from_min_size(pos2(r.left() + 6.0, r.center().y - 8.0), Vec2::splat(16.0));
            let faded = env.cut.contains(&e.path);
            column_cells(ui, r, ic, e, env, &none, false, faded, true, git.as_deref());
            if resp.clicked() {
                LAST.store(Id::new(&e.path).value(), std::sync::atomic::Ordering::Relaxed);
                // Open-on-single-click goes into a folder at once.
                let into = env.cfg.single_click && e.is_dir && !ui.input(|i| i.modifiers.ctrl || i.modifiers.shift);
                go = Some(if into { (None, None) } else { (Some(e.path.clone()), None) });
                if into {
                    let p = e.path.clone();
                    env.actions.push(Action::Run(Box::new(move |app| crate::columns::focus(app, panel, p, None))));
                    go = None;
                }
            }
            if resp.secondary_clicked() {
                go = Some((Some(e.path.clone()), resp.interact_pointer_pos()));
            }
            if resp.middle_clicked() && e.is_dir {
                let path = e.path.clone();
                env.actions.push(match env.cfg.middle_click {
                    MiddleClick::OpenInSplit => Action::Split { panel, path, side: Side::Right, select: None },
                    MiddleClick::OpenInNewTab => Action::NewTab { panel, path, select: None, focus: false },
                });
            }
            let right = resp.drag_started_by(egui::PointerButton::Secondary);
            if (resp.drag_started_by(egui::PointerButton::Primary) || right) && env.drag.is_none() {
                *env.drag = Some(Drag::Files { paths: vec![e.path.clone()], from_panel: panel, right });
            }
        }
        if col.scroll_to_cursor {
            col.scroll_to_cursor = false;
            if let Some(i) = child.and_then(|c| col.view.iter().position(|&x| x == c)) {
                ui.scroll_to_rect(Rect::from_min_size(origin + vec2(0.0, i as f32 * row_h), vec2(body.width(), row_h)), None);
            }
        }
    });
    // Released over the column, not on a folder in it: into this column's folder.
    if let Some(Drag::Files { paths, .. }) = env.drag.as_ref() {
        let over = ui.ctx().pointer_latest_pos().is_some_and(|p| ui.clip_rect().contains(p));
        if over && ui.input(|i| i.pointer.any_released()) {
            if paths.iter().any(|p| p.parent() != Some(dir.as_path()) && *p != dir) {
                let m = ui.input(|i| i.modifiers);
                env.actions.push(Action::Drop { paths: paths.clone(), dest: dir.clone(), copy: m.ctrl && !m.shift, link: m.ctrl && m.shift });
            }
            *env.drag = None;
        }
    }
    if let Some((item, menu)) = go {
        env.actions.push(Action::Run(Box::new(move |app| {
            crate::columns::focus(app, panel, dir, item);
            if let Some(pos) = menu {
                let background = app.panels.get(&panel).is_none_or(|p| p.tab().selected.is_empty());
                app.popup = crate::ctxmenu::build(app, panel, pos, background);
            }
        })));
    }
    // Nothing to list: say why, small, in the column.
    if col.view.is_empty() && !col.is_loading() {
        let msg = match &col.error {
            Some(e) => format!("Can't open: {}", fsx::plain_error(e)),
            None if col.hidden_count > 0 => format!("{} hidden item{}", col.hidden_count, if col.hidden_count == 1 { "" } else { "s" }),
            None => "Empty".into(),
        };
        let g = elided(ui, &msg, FontId::proportional(env.cfg.font_size - 1.0), (cr.width() - 24.0).max(30.0), env.pal.text_dim);
        ui.painter().galley(pos2(cr.center().x - g.size().x / 2.0, cr.top() + row_height(env.cfg) * 1.5), g, env.pal.text_dim);
    }
}
