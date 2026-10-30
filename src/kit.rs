//! Controls the dialogs share: a highlight that slides to what was picked, a segmented
//! choice, keys drawn as keycaps, a slider whose fill is its value, a dashed rule.
//! Every colour comes from the theme's palette, so they sit as well on a light theme as
//! on a dark one, and every motion stops at once with Animations off.

use egui::{Align2, Color32, CornerRadius, FontId, Id, Key, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2};

use crate::theme::Palette;

/// How long a highlight takes to reach what was picked.
const SLIDE: f64 = 0.3;

#[derive(Clone, Copy)]
struct Slide {
    from: f32,
    to: f32,
    t0: f64,
}

/// Fast at first, settling softly: the last stretch is where the eye lands.
fn ease(t: f64) -> f32 {
    (1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(4)) as f32
}

/// Motion is on: Animations off sets the style's animation time to zero.
pub fn moving(ctx: &egui::Context) -> bool {
    ctx.global_style().animation_time > 0.0
}

/// `target`, reached by easing from wherever the value was when the target last
/// changed. The first time an id is seen it starts where it should be: a dialog opens
/// still. Repaints only while it is on its way.
pub fn slide(ctx: &egui::Context, id: Id, target: f32) -> f32 {
    let now = ctx.input(|i| i.time);
    let at = |s: &Slide| if now - s.t0 >= SLIDE { s.to } else { s.from + (s.to - s.from) * ease((now - s.t0) / SLIDE) };
    let mut s = ctx.data(|d| d.get_temp::<Slide>(id)).unwrap_or(Slide { from: target, to: target, t0: f64::MIN });
    if !moving(ctx) {
        s = Slide { from: target, to: target, t0: f64::MIN };
    } else if (s.to - target).abs() > 0.01 {
        s = Slide { from: at(&s), to: target, t0: now };
    }
    ctx.data_mut(|d| d.insert_temp(id, s));
    if now - s.t0 < SLIDE {
        ctx.request_repaint();
    }
    at(&s)
}

/// Time since `key` last changed, for rows that come in one after another; `key` is
/// what they show (a search, a category). Infinite with Animations off.
pub fn since_change(ctx: &egui::Context, id: Id, key: u64) -> f64 {
    let now = ctx.input(|i| i.time);
    let prev = ctx.data(|d| d.get_temp::<(u64, f64)>(id));
    let t0 = match prev {
        Some((k, t)) if k == key => t,
        // The first sight of the list is no change: it is simply there.
        None => f64::MIN,
        Some(_) => now,
    };
    ctx.data_mut(|d| d.insert_temp(id, (key, t0)));
    if moving(ctx) { now - t0 } else { f64::INFINITY }
}

/// The `i`th row's way in, `t` seconds after the change: its opacity, from a fifth to
/// whole. Rows follow 45 ms apart, the twelfth and after all together. None starts
/// unseen: a row is there to be read and clicked from the first frame, only fainter.
pub fn arrive(ctx: &egui::Context, t: f64, i: usize) -> f32 {
    let t = t - 0.045 * i.min(12) as f64;
    if t < 0.32 {
        ctx.request_repaint();
    }
    0.2 + 0.8 * ease(t / 0.32)
}

/// What is chosen, quietly: a step from the card towards the text colour. It reads on
/// every theme, and leaves the accent for the one mark beside it.
pub fn lift(pal: &Palette) -> Color32 {
    if pal.strong {
        return pal.selected;
    }
    pal.card().lerp_to_gamma(pal.text, if pal.dark { 0.11 } else { 0.08 })
}

/// A pointer over a row, a step short of `lift`.
pub fn touch(pal: &Palette) -> Color32 {
    pal.card().lerp_to_gamma(pal.text, if pal.dark { 0.05 } else { 0.04 })
}

/// Text on `lift`: the theme's own, except a high-contrast theme's saturated selection.
pub fn on_lift(pal: &Palette) -> Color32 {
    if pal.strong { pal.on_sel() } else { pal.text }
}

/// The chosen row's highlight: `lift`, and a short accent bar at its left edge.
pub fn chosen(p: &egui::Painter, r: Rect, radius: u8, pal: &Palette) {
    p.rect_filled(r, CornerRadius::same(radius), lift(pal));
    if !pal.strong {
        let h = (r.height() - 16.0).clamp(8.0, 18.0);
        p.rect_filled(Rect::from_center_size(pos2(r.left() + 3.5, r.center().y), vec2(3.0, h)), CornerRadius::same(2), pal.accent);
    }
}

/// A choice of a few, side by side, with a pill that slides to the one picked. Labels
/// at `size`; `h` tall; `notes` name each part for the simulated user and `tips` say
/// what it shows (either may be empty). Returns the one clicked.
#[allow(clippy::too_many_arguments)]
pub fn segmented(ui: &mut Ui, id: Id, labels: &[&str], notes: &[&'static str], tips: &[&str], cur: usize, size: f32, h: f32, pal: &Palette) -> Option<usize> {
    let font = FontId::proportional(size);
    let ws: Vec<f32> = labels.iter().map(|l| ui.painter().layout_no_wrap(l.to_string(), font.clone(), pal.text).size().x + 18.0).collect();
    let (r, _) = ui.allocate_exact_size(vec2(ws.iter().sum::<f32>() + 4.0, h), Sense::hover());
    segmented_in(ui, id, r, labels, notes, tips, &ws, cur, size, pal)
}

/// Its width at `size`, for a caller that lays it out itself.
pub fn segmented_width(ui: &Ui, labels: &[&str], size: f32) -> f32 {
    labels.iter().map(|l| ui.painter().layout_no_wrap(l.to_string(), FontId::proportional(size), Color32::WHITE).size().x + 18.0).sum::<f32>() + 4.0
}

#[allow(clippy::too_many_arguments)]
fn segmented_in(ui: &mut Ui, id: Id, r: Rect, labels: &[&str], notes: &[&'static str], tips: &[&str], ws: &[f32], cur: usize, size: f32, pal: &Palette) -> Option<usize> {
    let p = ui.painter();
    p.rect(r, CornerRadius::same(7), pal.card(), Stroke::new(1.0, pal.card_line()), StrokeKind::Inside);
    let inner = r.shrink(2.0);
    let mut x = inner.left();
    let mut cells = Vec::with_capacity(labels.len());
    for w in ws {
        cells.push(Rect::from_min_max(pos2(x, inner.top()), pos2(x + w, inner.bottom())));
        x += w;
    }
    let mut out = None;
    if let Some(c) = cells.get(cur.min(cells.len().saturating_sub(1))) {
        let l = slide(ui.ctx(), id.with("l"), c.left() - r.left()) + r.left();
        let w = slide(ui.ctx(), id.with("w"), c.width());
        let pill = Rect::from_min_size(pos2(l, inner.top()), vec2(w, inner.height()));
        ui.painter().rect(pill, CornerRadius::same(5), lift(pal), Stroke::new(1.0, pal.card_line()), StrokeKind::Inside);
    }
    for (k, (cell, label)) in cells.iter().zip(labels).enumerate() {
        let resp = ui.interact(*cell, id.with(k), Sense::click());
        if let Some(n) = notes.get(k) {
            crate::ui::note_control(ui, *cell, n);
        }
        let on = k == cur;
        if !on && resp.hovered() {
            ui.painter().rect_filled(cell.shrink(1.0), CornerRadius::same(5), touch(pal));
        }
        let c = if on { on_lift(pal) } else if resp.hovered() { pal.text } else { pal.text_dim };
        let c = if ui.is_enabled() { c } else { c.gamma_multiply(0.5) };
        ui.painter().text(cell.center(), Align2::CENTER_CENTER, *label, FontId::proportional(size), c);
        let resp = match tips.get(k) {
            Some(t) => resp.on_hover_text(*t),
            None => resp,
        };
        if resp.clicked() && !on {
            out = Some(k);
        }
    }
    out
}

/// A key written in a hint ("Ctrl+D", "Alt+↑↓", "Ctrl PgUp PgDn"), as the caps it takes.
pub fn caps(k: &str) -> Vec<&str> {
    let mut out = vec![];
    for chord in k.split(' ').filter(|w| !w.is_empty()) {
        let mut rest = chord;
        // Modifiers come first, each with its plus; what is left is one key ("Ctrl++" is
        // Ctrl and the plus key).
        loop {
            let Some((m, tail)) = rest.split_once('+') else { break };
            if tail.is_empty() || !matches!(m, "Ctrl" | "Alt" | "Shift" | "Super") {
                break;
            }
            out.push(m);
            rest = tail;
        }
        // Arrows written together are a cap each.
        if rest.chars().count() > 1 && rest.chars().all(|c| matches!(c, '↑' | '↓' | '←' | '→')) {
            out.extend(rest.char_indices().map(|(i, c)| &rest[i..i + c.len_utf8()]));
        } else {
            out.push(rest);
        }
    }
    out
}

/// The cap is down: its key is held now.
pub fn held(ctx: &egui::Context, cap: &str) -> bool {
    ctx.input(|i| {
        let m = i.modifiers;
        let key = match cap {
            "Ctrl" => return m.ctrl,
            "Alt" => return m.alt,
            "Shift" => return m.shift,
            "↑" => Some(Key::ArrowUp),
            "↓" => Some(Key::ArrowDown),
            "←" => Some(Key::ArrowLeft),
            "→" => Some(Key::ArrowRight),
            "Esc" => Some(Key::Escape),
            "Del" => Some(Key::Delete),
            "PgUp" => Some(Key::PageUp),
            "PgDn" => Some(Key::PageDown),
            _ => Key::from_name(cap),
        };
        key.is_some_and(|k| i.key_down(k))
    })
}

/// One keycap, `text` at `font` in `r`: a face a step off the background, lit along its
/// top edge, standing on a one-pixel shadow it sinks onto while `down`.
pub fn keycap(p: &egui::Painter, r: Rect, text: &str, font: FontId, down: bool, pal: &Palette) {
    let face = if pal.dark { pal.raised.lerp_to_gamma(pal.text, 0.08) } else { pal.raised.lerp_to_gamma(Color32::WHITE, 0.7) };
    let shade = if pal.dark { Color32::from_black_alpha(140) } else { pal.border.lerp_to_gamma(pal.text, 0.15) };
    let rim = if pal.strong { pal.border } else if pal.dark { pal.text.gamma_multiply(0.14) } else { pal.border };
    let shine = if pal.dark { pal.text.gamma_multiply(0.12) } else { Color32::from_white_alpha(200) };
    let r = if down { r.translate(vec2(0.0, 1.0)) } else { r };
    if !down {
        p.rect_filled(r.translate(vec2(0.0, 1.0)), CornerRadius::same(4), shade);
    }
    p.rect(r, CornerRadius::same(4), if down { face.lerp_to_gamma(pal.text, 0.06) } else { face }, Stroke::new(1.0, rim), StrokeKind::Inside);
    p.line_segment([pos2(r.left() + 3.0, r.top() + 1.5), pos2(r.right() - 3.0, r.top() + 1.5)], Stroke::new(1.0, shine));
    let c = if down { pal.text } else { pal.text_dim };
    p.text(pos2(r.center().x, r.center().y - 0.5), Align2::CENTER_CENTER, text, font, c);
}

/// The caps of `k` laid out from `left` around `cy`: their rects, and the width taken.
pub fn caps_layout(p: &egui::Painter, k: &str, left: f32, cy: f32, font: &FontId, pal: &Palette) -> (Vec<(Rect, String)>, f32) {
    let h = (font.size * 1.55).round().max(16.0);
    let mut x = left;
    let mut out = vec![];
    for (n, c) in caps(k).into_iter().enumerate() {
        if n > 0 {
            x += 3.0;
        }
        let w = (p.layout_no_wrap(c.to_string(), font.clone(), pal.text_dim).size().x + 10.0).max(h);
        out.push((Rect::from_min_size(pos2(x, (cy - h / 2.0).round()), vec2(w, h)), c.to_string()));
        x += w;
    }
    (out, x - left)
}

/// Draws `k` as caps from `left`, pressed while held when `live`; returns the width.
pub fn paint_caps(ui: &Ui, k: &str, left: f32, cy: f32, font: &FontId, live: bool, pal: &Palette) -> f32 {
    let (rects, w) = caps_layout(ui.painter(), k, left, cy, font, pal);
    for (r, c) in rects {
        let down = live && held(ui.ctx(), &c);
        keycap(ui.painter(), r, &c, font.clone(), down, pal);
        crate::ui::note_control(ui, r, "key hint");
    }
    w
}

/// A hotkey ("Ctrl+Shift+N", "G, H") as caps ending at `right`; returns the width.
pub fn hotkey(p: &egui::Painter, right: Pos2, text: &str, pal: &Palette) -> f32 {
    let font = FontId::proportional(10.5);
    let parts: Vec<&str> = text.split(", ").collect();
    let mut all: Vec<(Vec<(Rect, String)>, f32)> = parts.iter().map(|k| caps_layout(p, k, 0.0, right.y, &font, pal)).collect();
    let comma = p.layout_no_wrap(",".into(), font.clone(), pal.text_dim).size().x + 4.0;
    let total: f32 = all.iter().map(|a| a.1).sum::<f32>() + comma * (all.len().saturating_sub(1)) as f32;
    let mut x = right.x - total;
    let n = all.len();
    for (i, (rects, w)) in all.iter_mut().enumerate() {
        for (r, c) in rects.iter() {
            keycap(p, r.translate(vec2(x, 0.0)), c, font.clone(), false, pal);
        }
        x += *w;
        if i + 1 < n {
            p.text(pos2(x + 1.0, right.y), Align2::LEFT_CENTER, ",", font.clone(), pal.text_dim);
            x += comma;
        }
    }
    total
}

/// A rule of short dashes: a division inside one thing, not between two.
pub fn dashed_rule(p: &egui::Painter, a: Pos2, b: Pos2, pal: &Palette) {
    p.extend(Shape::dashed_line(&[a, b], Stroke::new(1.0, pal.border), 4.0, 3.0));
}

/// A slider whose fill is its value: `w` wide, `h` tall, `label` at its left end (or
/// none) and the value at its right end as `fmt` makes it. Hashmarks show while it is
/// pointed at or dragged: at `marks` (values) when given, else at each step. The arrow
/// keys move it a step when `keys` (its row has the keyboard). A logarithmic one spreads
/// a range of sizes evenly. Returns whether the value changed.
#[allow(clippy::too_many_arguments)]
pub fn fill_slider(ui: &mut Ui, id: Id, v: &mut f32, min: f32, max: f32, step: f32, log: bool, w: f32, h: f32, keys: bool, label: &str, marks: &[f32], fmt: impl Fn(f32) -> String, pal: &Palette) -> bool {
    let (r, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click_and_drag());
    crate::ui::note_control(ui, r, "slider");
    let enabled = ui.is_enabled();
    let to_t = |x: f32| if log { (x.max(min).ln() - min.ln()) / (max.ln() - min.ln()) } else { (x - min) / (max - min) };
    let from_t = |t: f32| if log { (min.ln() + t * (max.ln() - min.ln())).exp() } else { min + t * (max - min) };
    let snap = |x: f32| ((x / step).round() * step).clamp(min, max);
    let before = *v;
    if enabled && (resp.dragged() || resp.clicked()) {
        if let Some(p) = resp.interact_pointer_pos() {
            *v = snap(from_t(((p.x - r.left() - 4.0) / (r.width() - 8.0)).clamp(0.0, 1.0)));
        }
    }
    if enabled && keys {
        let (l, rt) = ui.input_mut(|i| (i.consume_key(egui::Modifiers::NONE, Key::ArrowLeft), i.consume_key(egui::Modifiers::NONE, Key::ArrowRight)));
        // A logarithmic one steps by a tenth of its range, else by one step.
        let by = |t: f32, d: f32| if log { snap(from_t((to_t(t) + d * 0.1).clamp(0.0, 1.0))) } else { snap(t + d * step) };
        if l {
            *v = by(*v, -1.0);
        }
        if rt {
            *v = by(*v, 1.0);
        }
    }
    let hot = enabled && (resp.hovered() || resp.dragged());
    let p = ui.painter();
    let track = pal.card().lerp_to_gamma(pal.text, if pal.dark { 0.05 } else { 0.045 });
    // The fill is the accent's, as a switch that is on: faint enough to read the value on.
    let filled = track.lerp_to_gamma(pal.accent, if pal.dark { 0.34 } else { 0.26 } + if hot { 0.06 } else { 0.0 });
    p.rect(r, CornerRadius::same(6), track, Stroke::new(1.0, pal.card_line()), StrokeKind::Inside);
    let t = to_t(*v).clamp(0.0, 1.0);
    // The fill eases to a value set by a click or a key, and follows a drag at once.
    let shown = if resp.dragged() { slide(ui.ctx(), id, t); t } else { slide(ui.ctx(), id, t) };
    let x = r.left() + 4.0 + shown * (r.width() - 8.0);
    let fill = Rect::from_min_max(r.min, pos2(x.max(r.left() + 6.0), r.bottom())).shrink(1.0);
    p.rect_filled(fill, CornerRadius::same(5), if enabled { filled } else { track });
    if hot && !marks.is_empty() {
        for &m in marks.iter().filter(|&&m| m > min && m < max) {
            let hx = r.left() + 4.0 + to_t(m) * (r.width() - 8.0);
            p.line_segment([pos2(hx.round() + 0.5, r.center().y - 3.5), pos2(hx.round() + 0.5, r.center().y + 3.5)], Stroke::new(1.0, pal.text.gamma_multiply(0.18)));
        }
    } else if hot && !log {
        let n = ((max - min) / step).round() as usize;
        let every = n.div_ceil(24).max(1);
        for k in (every..n).step_by(every) {
            let hx = r.left() + 4.0 + k as f32 / n as f32 * (r.width() - 8.0);
            p.line_segment([pos2(hx.round() + 0.5, r.center().y - 3.5), pos2(hx.round() + 0.5, r.center().y + 3.5)], Stroke::new(1.0, pal.text.gamma_multiply(0.18)));
        }
    }
    let knob = if enabled { if hot { pal.text } else { pal.text.gamma_multiply(0.85) } } else { pal.text_dim.gamma_multiply(0.5) };
    let kx = x.clamp(r.left() + 5.0, r.right() - 5.0);
    p.rect_filled(Rect::from_center_size(pos2(kx, r.center().y), vec2(3.0, (h - 10.0).max(8.0))), CornerRadius::same(2), knob);
    let vc = if !enabled { pal.text_dim.gamma_multiply(0.6) } else if hot { pal.text } else { pal.text_dim };
    let vg = crate::listing::tabular(ui, &fmt(*v), FontId::proportional(12.0), r.width() * 0.5, vc);
    // The value sits at the right end, and steps to the handle's left when the handle
    // comes up to it, so the two never cross.
    if !label.is_empty() {
        let lc = if enabled { if hot { pal.text } else { pal.text_dim } } else { pal.text_dim.gamma_multiply(0.6) };
        let lg = ui.painter().layout_no_wrap(label.to_string(), FontId::proportional(12.0), lc);
        // Like the value, it steps aside when the handle comes to it.
        let lx = if kx < r.left() + 9.0 + lg.size().x + 4.0 { (kx + 7.0).max(r.left() + 9.0) } else { r.left() + 9.0 };
        ui.painter().galley(pos2(lx, (r.center().y - lg.size().y / 2.0).round()), lg, lc);
    }
    let vw = vg.size().x;
    let vx = if kx + 6.0 > r.right() - 8.0 - vw { kx - 8.0 - vw } else { r.right() - 8.0 - vw };
    ui.painter().galley(pos2(vx, (r.center().y - vg.size().y / 2.0).round()), vg, vc);
    if hot {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    *v != before
}

/// A chevron at `c` turning from pointing right (`open` 0, folded) to down (1).
pub fn chevron(p: &egui::Painter, c: Pos2, open: f32, col: Color32) {
    let a = open.clamp(0.0, 1.0) * std::f32::consts::FRAC_PI_2;
    let d = vec2(a.cos(), a.sin());
    let n = vec2(-d.y, d.x);
    p.add(Shape::line(vec![c - d * 1.5 + n * 3.5, c + d * 2.0, c - d * 1.5 - n * 3.5], Stroke::new(1.3, col)));
}

/// A quiet button whose keys stand in it after its label, as caps: "Undo [Ctrl] [Z]".
pub fn key_button(ui: &mut Ui, label: &str, keys: &str, pal: &Palette) -> egui::Response {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let kfont = FontId::proportional(10.5);
    let g = ui.painter().layout_no_wrap(label.to_string(), font, pal.text);
    let kw = if keys.is_empty() { 0.0 } else { caps_layout(ui.painter(), keys, 0.0, 0.0, &kfont, pal).1 + 8.0 };
    let h = (g.size().y + 10.0).max(ui.spacing().interact_size.y).max(24.0);
    let (r, resp) = ui.allocate_exact_size(vec2(g.size().x + kw + 20.0, h), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    let hot = resp.hovered() || resp.has_focus();
    ui.painter().rect(r, CornerRadius::same(7), if hot { lift(pal) } else { touch(pal) }, Stroke::new(1.0, pal.card_line()), StrokeKind::Inside);
    let tc = if hot { pal.text } else { pal.text.gamma_multiply(0.9) };
    ui.painter().galley(pos2(r.left() + 10.0, (r.center().y - g.size().y / 2.0).round()), g.clone(), tc);
    // The caps are part of the button, not controls of their own.
    if !keys.is_empty() {
        let (rects, _) = caps_layout(ui.painter(), keys, r.left() + 10.0 + g.size().x + 8.0, r.center().y - 0.5, &kfont, pal);
        for (cr, c) in rects {
            keycap(ui.painter(), cr, &c, kfont.clone(), held(ui.ctx(), &c), pal);
        }
    }
    resp
}

/// A line along `r`'s foot, as long as the time left is of `life`: how long something
/// stays before it goes by itself.
pub fn time_left_line(p: &egui::Painter, r: Rect, left: f32, life: f32, pal: &Palette) {
    let t = (left / life).clamp(0.0, 1.0);
    if t <= 0.0 {
        return;
    }
    let y = r.bottom() - 1.5;
    let x0 = r.left() + 8.0;
    let x1 = x0 + (r.width() - 16.0) * t;
    p.line_segment([pos2(x0, y), pos2(x1, y)], Stroke::new(2.0, pal.text_dim.gamma_multiply(0.55)));
}

/// A switch drawn as a small word ("Aa", ".*"): bare while off, lit in the accent's tint
/// with a hairline while on. Returns whether it was clicked.
pub fn toggle(ui: &mut Ui, label: &str, on: bool, mono: bool, note: &'static str, tip: &str, pal: &Palette) -> bool {
    let font = if mono { FontId::monospace(12.0) } else { FontId::proportional(12.0) };
    let g = ui.painter().layout_no_wrap(label.to_string(), font.clone(), pal.text);
    let (r, resp) = ui.allocate_exact_size(vec2((g.size().x + 14.0).max(26.0), 22.0), Sense::click());
    crate::ui::note_control(ui, r, note);
    let t = slide(ui.ctx(), Id::new(("toggle", note)), if on { 1.0 } else { 0.0 });
    if t > 0.01 {
        ui.painter().rect(r, CornerRadius::same(5), pal.accent.gamma_multiply((if pal.dark { 0.24 } else { 0.16 }) * t), Stroke::new(1.0, pal.accent.gamma_multiply(0.45 * t)), StrokeKind::Inside);
    } else {
        // Off, it keeps a field's faint edge: a control, not a stray word.
        ui.painter().rect(r, CornerRadius::same(5), if resp.hovered() { touch(pal) } else { Color32::TRANSPARENT }, Stroke::new(1.0, pal.card_line()), StrokeKind::Inside);
    }
    let c = if on || resp.hovered() { pal.text } else { pal.text_dim };
    ui.painter().text(r.center(), Align2::CENTER_CENTER, label, font, c);
    resp.on_hover_text(tip).clicked()
}

/// A quiet button at a popup's own size (12.5), whatever the window's font: the card's
/// fill and edge, a step up while pointed at.
pub fn small_button(ui: &mut Ui, label: &str, pal: &Palette) -> egui::Response {
    let font = FontId::proportional(12.5);
    let h = (ui.painter().layout_no_wrap("A".into(), font, pal.text).size().y + 10.0).max(24.0);
    button_in(ui, label, h, false, pal)
}

/// A quiet button `h` tall; `news` puts an accent dot before its label (something new to
/// act on: an update) where a solid accent fill would outshout the rest of the row.
pub fn button_in(ui: &mut Ui, label: &str, h: f32, news: bool, pal: &Palette) -> egui::Response {
    let font = FontId::proportional(12.5);
    let g = ui.painter().layout_no_wrap(label.to_string(), font, pal.text);
    let dot = if news { 12.0 } else { 0.0 };
    let (r, resp) = ui.allocate_exact_size(vec2(g.size().x + 18.0 + dot, h), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), label));
    let hot = ui.is_enabled() && (resp.hovered() || resp.has_focus());
    ui.painter().rect(r, CornerRadius::same(6), if hot { lift(pal) } else { pal.card() }, Stroke::new(1.0, if resp.has_focus() { pal.accent.gamma_multiply(0.7) } else { pal.card_line() }), StrokeKind::Inside);
    if news {
        ui.painter().circle_filled(pos2(r.left() + 12.0, r.center().y), 3.0, pal.accent);
    }
    let c = if !ui.is_enabled() { pal.text_dim.gamma_multiply(0.6) } else if hot { pal.text } else { pal.text.gamma_multiply(0.9) };
    ui.painter().galley(pos2(r.left() + 9.0 + dot, (r.center().y - g.size().y / 2.0).round()), g, c);
    resp
}
