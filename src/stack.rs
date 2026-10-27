//! The Stack view: the folder's items standing in a row like cards in a box, seen from
//! the front and a little above. The item at the cursor is drawn out of the row to face
//! you, its name over the row and a scrubber under it. The arrows, the wheel, a click on
//! a card or on the scrubber move the cursor; the row eases after it.
//!
//! The faces are drawn flat to the screen and the row recedes at an angle (an oblique
//! projection), so thumbnails, icons and names stay crisp: nothing is skewed or resampled.
//! Only the cards in sight are drawn, and nothing repaints once the row has settled.

use egui::{Color32, CornerRadius, FontId, Id, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

use crate::listing::{self, Env};
use crate::tab::{self, Tab};
use crate::theme;

/// The row's direction into the box, on screen: up and to the right.
const ANGLE: f32 = 0.74;
/// From one card to the next along the row, and a card's thickness, as shares of its side.
const PITCH: f32 = 0.075;
const DEPTH: f32 = 0.03;
/// How far the card at the cursor comes out: along its face, and out of the box.
const SLIDE: f32 = 0.6;
const PULL: f32 = 0.05;
/// The room the cards in front of it step back to leave, and the room behind it.
const FRONT_GAP: f32 = 0.46;
const BACK_GAP: f32 = 0.06;
/// A jump further than this many items eases over this many: Home in a folder of ten
/// thousand would otherwise flip past every one of them.
const MAX_FLIP: f32 = 24.0;

/// The row's direction on screen, a unit vector.
fn into_box() -> Vec2 {
    vec2(ANGLE.cos(), -ANGLE.sin())
}

fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Where a card's face sits for its place `p` in the row (0 at the cursor, negative in
/// front), relative to where the card at the cursor would stand before it comes out.
fn offset(p: f32, side: f32) -> Vec2 {
    let r = into_box();
    let out = smooth(1.0 - p.abs());
    let along = p * PITCH + if p < 0.0 { -FRONT_GAP * smooth(-p) } else { BACK_GAP * smooth(p) };
    r * along * side + (vec2(SLIDE, 0.0) - r * PULL) * out * side
}

/// How far a card at place `p` has sunk into the background, 0 to 1: the ones behind the
/// cursor over a long way, the few in front of it soon, so they never pile up over it.
fn depth_fade(p: f32) -> f32 {
    if p > 0.0 { smooth((p - 10.0) / 16.0) } else { smooth((-p - 2.0) / 6.0) }
}

/// One card: its face, its top and its right side.
struct Card {
    face: Rect,
    top: [Pos2; 4],
    side: [Pos2; 4],
}

fn card(anchor: Pos2, side: f32) -> Card {
    let face = Rect::from_min_size(pos2(anchor.x, anchor.y - side), Vec2::splat(side));
    let d = into_box() * DEPTH * side;
    Card {
        face,
        top: [face.left_top(), face.right_top(), face.right_top() + d, face.left_top() + d],
        side: [face.right_top(), face.right_bottom(), face.right_bottom() + d, face.right_top() + d],
    }
}

impl Card {
    fn bounds(&self) -> Rect {
        Rect::from_points(&[self.face.min, self.face.max, self.top[2], self.top[3], self.side[2]])
    }
}

pub fn draw(ui: &mut Ui, rect: Rect, t: &mut Tab, env: &mut Env) {
    let n = t.view.len();
    // Every arrow moves one item; a page is ten.
    t.nav_row_step = 1;
    t.nav_col_step = 1;
    t.page_items = 10;
    if n == 0 {
        return;
    }
    let pal = *env.pal;
    let fs = env.cfg.font_size;
    let anim = env.cfg.animations;
    let ctx = ui.ctx().clone();
    let bg = ui.interact(rect, Id::new(("stack bg", t.id)), Sense::click());
    listing::background_clicks(ui, &bg, t, env);

    // The wheel, either way, flips one item per notch; a touchpad's glide adds up to items.
    // From the wheel's own events, not the smoothed scroll: the row eases already, and
    // easing twice lags.
    if ui.rect_contains_pointer(rect) && !ui.input(|i| i.modifiers.ctrl) {
        let notch = ctx.options(|o| o.input_options.line_scroll_speed).max(1.0);
        let d: f32 = ui.input(|i| {
            i.events
                .iter()
                .map(|e| match e {
                    egui::Event::MouseWheel { unit, delta, .. } => {
                        let s = delta.x + delta.y;
                        match unit {
                            egui::MouseWheelUnit::Line => s,
                            egui::MouseWheelUnit::Page => s * 10.0,
                            egui::MouseWheelUnit::Point => s / notch,
                        }
                    }
                    _ => 0.0,
                })
                .sum()
        });
        t.stack_acc -= d;
        let whole = t.stack_acc.trunc();
        if whole != 0.0 {
            t.stack_acc -= whole;
            t.move_cursor(whole as isize, false, false);
        }
    } else {
        t.stack_acc = 0.0;
    }
    let cur = t.cursor.and_then(|c| t.view_pos(c)).unwrap_or(0);
    let target = cur as f32;
    if t.stack_pos < 0.0 || !anim {
        t.stack_pos = target;
    } else {
        if (target - t.stack_pos).abs() > MAX_FLIP {
            t.stack_pos = target - MAX_FLIP * (target - t.stack_pos).signum();
        }
        let dt = ui.input(|i| i.stable_dt).min(0.05);
        t.stack_pos += (target - t.stack_pos) * (1.0 - (-dt * 16.0).exp());
        if (target - t.stack_pos).abs() < 0.001 {
            t.stack_pos = target;
        } else {
            ctx.request_repaint();
        }
    }
    let pos = t.stack_pos;

    // Name and details over the row, the scrubber under it, the row between.
    let edge = (16.0 * env.cfg.spacing.max(0.7)).round();
    let title_font = crate::theme::semibold((fs * 1.55).round());
    let meta_font = FontId::proportional(fs - 1.0);
    let title_h = ui.fonts_mut(|f| f.row_height(&title_font));
    let meta_h = ui.fonts_mut(|f| f.row_height(&meta_font));
    let head = Rect::from_min_size(pos2(rect.left() + edge, rect.top() + edge + 4.0), vec2(rect.width() - 2.0 * edge, title_h + 4.0 + meta_h));
    // Ticks and pin, and a line for the marks under them.
    let scrub_h = 48.0;
    let scrub = Rect::from_min_max(pos2(rect.left() + edge, rect.bottom() - edge - scrub_h), pos2(rect.right() - edge, rect.bottom() - edge));
    let stage = Rect::from_min_max(pos2(rect.left() + edge, head.bottom() + 12.0), pos2(rect.right() - edge, scrub.top() - 8.0));

    // A card's side: as tall as the stage allows with the row's rise and the card's step
    // out of it, and narrow enough that the row behind it shows.
    let side = (stage.height() * 0.5).min(stage.width() * 0.36).clamp(56.0, 380.0).round();
    // The card at the cursor, once out, stands a little right of and below the middle.
    let lead = offset(0.0, side);
    let anchor0 = stage.center() + vec2(-side * 0.5, side * 0.5) - lead + vec2(side * 0.12, side * 0.08);

    // What is in sight: walk out from the cursor both ways until the cards leave the stage.
    let in_sight = |i: isize| -> Option<Card> {
        let c = card(anchor0 + offset(i as f32 - pos, side), side);
        (c.bounds().intersects(stage) && depth_fade(i as f32 - pos) < 0.995).then_some(c)
    };
    // The row eases toward the cursor, so just after the list shrinks it can still stand
    // past the end: the cards start from the last one there is.
    let base = (pos.round() as isize).clamp(0, n as isize - 1);
    let mut last = base;
    while last + 1 < n as isize && last - base < 200 && in_sight(last + 1).is_some() {
        last += 1;
    }
    let mut first = base;
    while first > 0 && base - first < 200 && in_sight(first - 1).is_some() {
        first -= 1;
    }

    let thumbs_here = t.archive.is_none() && env.cfg.thumbs_on(crate::fsx::drive_kind_cached(&t.path, &ctx));
    let ppp = ctx.pixels_per_point();
    let now = std::time::Instant::now();
    let round = if env.cfg.round_corners { 3 } else { 0 };
    let painter = ui.painter().with_clip_rect(stage.intersect(ui.clip_rect()));
    // The case's edges: a lighter top, a darker side, neutral in every theme.
    let top_col = pal.raised.lerp_to_gamma(pal.text, if pal.dark { 0.3 } else { 0.18 });
    let side_col = pal.raised.lerp_to_gamma(pal.text, if pal.dark { 0.14 } else { 0.08 });
    let fade_to = pal.panel;

    // The glow: the colour of the picture at the cursor, behind the card where it comes to
    // rest, easing from one picture's to the next's as the row flips and gone for an item
    // with no picture.
    let tone = t.view.get(cur).map(|&idx| &t.entries[idx as usize]).filter(|e| thumbs_here && pictured(e, t)).and_then(|e| env.thumbs.get(&e.path, e.modified, side * ppp).map(|l| l.tone)).filter(|c| c.a() > 0);
    {
        let g = &mut t.stack_glow;
        let k = if anim { 1.0 - (-ui.input(|i| i.stable_dt).min(0.05) * 9.0).exp() } else { 1.0 };
        let mut moving = false;
        let mut ease = |x: &mut f32, to: f32| {
            if (to - *x).abs() < 0.002 {
                *x = to;
            } else {
                *x += (to - *x) * k;
                moving = true;
            }
        };
        match tone {
            Some(c) => {
                // From nothing it comes up in its own colour, not out of the last one's.
                if g[3] < 0.02 {
                    *g = [c.r() as f32, c.g() as f32, c.b() as f32, g[3]];
                }
                for (i, v) in [c.r(), c.g(), c.b()].into_iter().enumerate() {
                    ease(&mut g[i], v as f32);
                }
                ease(&mut g[3], 1.0);
            }
            None => ease(&mut g[3], 0.0),
        }
        if moving {
            ctx.request_repaint();
        }
        if g[3] > 0.002 {
            let rest = card(anchor0 + offset(0.0, side), side).face.center();
            let strength = g[3] * if pal.dark { 0.3 } else { 0.2 };
            glow(&painter, rest, side * 1.5, Color32::from_rgb(g[0] as u8, g[1] as u8, g[2] as u8), strength);
        }
    }

    // Back to front, so each card covers the ones behind it.
    for i in (first..=last).rev() {
        let idx = t.view[i as usize];
        let p = i as f32 - pos;
        let mut c = card(anchor0 + offset(p, side), side);
        let id = Id::new(("stack card", t.id, idx));
        let resp = ui.interact(c.face, id, Sense::click_and_drag());
        // A card pointed at rises a little out of the row.
        let lift = ctx.animate_bool_with_time(id, resp.hovered() && p.abs() > 0.5, if anim { 0.12 } else { 0.0 });
        if lift > 0.0 {
            let up = vec2(0.0, -side * 0.06 * lift);
            c.face = c.face.translate(up);
            c.top = c.top.map(|q| q + up);
            c.side = c.side.map(|q| q + up);
        }
        let e = t.entries[idx as usize].clone();
        let selected = t.selected.contains(&idx);
        let drop = listing::drop_target(ui, c.face, &e, env);
        listing::handle_item(ui, &resp, idx, t, env);
        if p.abs() < 0.5 {
            t.cursor_screen_pos = Some(c.face.center_bottom());
        }
        // The card coming out casts a soft shadow on the row behind it.
        let out = smooth(1.0 - p.abs());
        if out > 0.01 {
            let shadow = egui::epaint::Shadow { offset: [0, (side * 0.04) as i8], blur: (side * 0.12).min(60.0) as u8, spread: 0, color: Color32::from_black_alpha(((if pal.dark { 110.0 } else { 45.0 }) * out) as u8) };
            painter.add(shadow.as_shape(c.face, CornerRadius::same(round)));
        }
        painter.add(Shape::convex_polygon(c.side.to_vec(), side_col, Stroke::NONE));
        painter.add(Shape::convex_polygon(c.top.to_vec(), top_col, Stroke::NONE));
        face(&painter, c.face, &e, t, env, thumbs_here, side * ppp, now, round, &pal);
        // The card at the cursor is out of the row already: the outline marks the others
        // in a selection of several.
        let marked = selected && (t.selected.len() > 1 || p.abs() > 0.5);
        if marked || drop {
            painter.rect_stroke(c.face.expand(1.0), CornerRadius::same(round + 1), Stroke::new(2.0, pal.accent), StrokeKind::Outside);
        }
        // Cards further from the cursor sink into the background; cut items and drives not
        // mounted stay faded.
        let mut fade = depth_fade(p);
        if env.cut.contains(&e.path) || listing::unmounted(t, &e) {
            fade = fade.max(0.5);
        }
        if fade > 0.004 {
            let veil = fade_to.gamma_multiply(fade.min(1.0));
            painter.add(Shape::convex_polygon(c.side.to_vec(), veil, Stroke::NONE));
            painter.add(Shape::convex_polygon(c.top.to_vec(), veil, Stroke::NONE));
            painter.rect_filled(c.face.expand(if marked || drop { 3.0 } else { 0.0 }), CornerRadius::same(round), veil);
        }
    }
    t.scroll_to_cursor = false;
    // The row dissolves at the stage's edges instead of being cut off by them.
    vignette(&painter, stage, (side * 0.22).clamp(16.0, 56.0), fade_to);

    // The name, or the rename field over it, and what it is.
    if let Some(&idx) = t.view.get(cur) {
        let e = t.entries[idx as usize].clone();
        let width = head.width().min(720.0);
        let title_r = Rect::from_center_size(pos2(head.center().x, head.top() + title_h / 2.0), vec2(width, title_h));
        if !listing::rename_field(ui, title_r, idx, t, env, &title_font) {
            let g = listing::elided(ui, &listing::display_name(&e, env.cfg), title_font.clone(), width, pal.text);
            ui.painter().galley(pos2(head.center().x - g.size().x / 2.0, head.top()), g, pal.text);
        }
        let mut meta = vec![listing::type_text(&e, t)];
        let size = listing::entry_size_text(&e, t, env);
        if !size.is_empty() && size != "--" {
            meta.push(size);
        }
        let date = listing::fmt_date(e.modified, env.cfg);
        if !date.is_empty() {
            meta.push(date);
        }
        let g = listing::elided(ui, &meta.join("  ·  "), meta_font, width, pal.text_dim);
        ui.painter().galley(pos2(head.center().x - g.size().x / 2.0, head.top() + title_h + 4.0), g, pal.text_dim);
    }

    scrubber(ui, scrub, t, env, pos, thumbs_here, &pal);
}

/// A soft disc of `col` round `c`, `strength` at the middle and clear at `radius`.
///
/// A faint gradient over a large area has only a few dozen steps of 8-bit colour to go
/// through and shows them as rings. The disc is a texture whose coverage carries both the
/// falloff and a few levels of noise; the colour is its tint. The blend is worked in full
/// precision and rounded once, after the noise is in, so each step dissolves into grain
/// too fine to see. Noise added in a second pass would come after that rounding and leave
/// the steps where they were.
fn glow(p: &egui::Painter, c: Pos2, radius: f32, col: Color32, strength: f32) {
    let Some(tex) = glow_texture(p.ctx()) else { return };
    let tint = Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), (strength * 255.0).round() as u8);
    p.image(tex, Rect::from_center_size(c, Vec2::splat(radius * 2.0)), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), tint);
}

/// The glow's disc: coverage falling off smoothly from the middle, with interleaved
/// gradient noise of about ±4 levels in it (enough for half a level of the colour at the
/// glow's strength), fading out at the rim so no grain shows past it. Drawn texel for
/// texel (nearest), which keeps the grain sharp. A million texels are made once, on a
/// thread of their own and not in a frame; until then there is no glow, which eases in
/// from nothing anyway.
fn glow_texture(ctx: &egui::Context) -> Option<egui::TextureId> {
    static IMAGE: std::sync::OnceLock<egui::ColorImage> = std::sync::OnceLock::new();
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let id = Id::new("stack glow");
    if let Some(t) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        return Some(t.id());
    }
    if let Some(img) = IMAGE.get() {
        let t = ctx.load_texture("stack glow", img.clone(), egui::TextureOptions::NEAREST);
        let tid = t.id();
        ctx.data_mut(|d| d.insert_temp(id, t));
        return Some(tid);
    }
    if !STARTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        let ctx = ctx.clone();
        let _ = std::thread::Builder::new().name("stack glow".into()).spawn(move || {
            let side = 1024usize;
            let half = side as f32 / 2.0;
            let mut px = Vec::with_capacity(side * side * 4);
            for y in 0..side {
                for x in 0..side {
                    let f = ((x as f32 + 0.5 - half).hypot(y as f32 + 0.5 - half) / half).min(1.0);
                    let k = 1.0 - f;
                    let shape = k * k * (3.0 - 2.0 * k);
                    let n = (52.982_918 * (0.067_110_56 * x as f32 + 0.005_837_15 * y as f32).fract()).fract() - 0.5;
                    let a = (shape + n * (8.0 / 255.0) * (shape / 0.03).min(1.0)).clamp(0.0, 1.0);
                    let a = (a * 255.0).round() as u8;
                    px.extend_from_slice(&[a, a, a, a]);
                }
            }
            let _ = IMAGE.set(egui::ColorImage::from_rgba_premultiplied([side, side], &px));
            ctx.request_repaint();
        });
    }
    None
}

/// Whether a card can show a picture: what the thumbnailer makes, a folder's cover, a
/// song's own.
fn pictured(e: &crate::fsx::Entry, t: &Tab) -> bool {
    crate::thumbs::can_thumb(e) || e.is_dir && !e.is_link && !tab::is_place(&t.path) || !e.is_dir && crate::musictags::holds_cover(e.ext())
}

/// Bands of `col` along the inside of `r`'s edges, solid at the edge and clear `w` in.
fn vignette(p: &egui::Painter, r: Rect, w: f32, col: Color32) {
    let mut m = egui::Mesh::default();
    let clear = Color32::TRANSPARENT;
    let mut band = |a: Pos2, b: Pos2, c: Pos2, d: Pos2| {
        // a-b on the edge, d-c inside it.
        let k = m.vertices.len() as u32;
        m.colored_vertex(a, col);
        m.colored_vertex(b, col);
        m.colored_vertex(c, clear);
        m.colored_vertex(d, clear);
        m.add_triangle(k, k + 1, k + 2);
        m.add_triangle(k, k + 2, k + 3);
    };
    band(r.left_top(), r.right_top(), pos2(r.right(), r.top() + w), pos2(r.left(), r.top() + w));
    band(r.left_bottom(), r.right_bottom(), pos2(r.right(), r.bottom() - w), pos2(r.left(), r.bottom() - w));
    band(r.left_top(), r.left_bottom(), pos2(r.left() + w, r.bottom()), pos2(r.left() + w, r.top()));
    band(r.right_top(), r.right_bottom(), pos2(r.right() - w, r.bottom()), pos2(r.right() - w, r.top()));
    p.add(Shape::mesh(m));
}

/// A card's face: the thumbnail filling it when there is one, else the item's icon on a
/// plain face.
#[allow(clippy::too_many_arguments)]
fn face(p: &egui::Painter, r: Rect, e: &crate::fsx::Entry, t: &Tab, env: &mut Env, thumbs_here: bool, px: f32, now: std::time::Instant, round: u8, pal: &theme::Palette) {
    let cat = theme::categorize(e.ext(), e.is_dir, e.is_executable());
    // A plain case, a step lighter than the dialogs' fill: the icon carries the colour.
    let blank = if pal.dark { pal.raised.lerp_to_gamma(pal.text, 0.07) } else { pal.raised.lerp_to_gamma(Color32::WHITE, 0.6) };
    p.rect_filled(r, CornerRadius::same(round), blank);
    if thumbs_here && pictured(e, t) {
        if let Some(l) = env.thumbs.get(&e.path, e.modified, px) {
            let [tw, th] = l.tex.size();
            let (tw, th) = (tw as f32, th as f32);
            // Cropped to the square, as a cover fills its case.
            let s = tw.min(th);
            let (u0, v0) = ((tw - s) / 2.0 / tw, (th - s) / 2.0 / th);
            let alpha = if env.cfg.animations { (now.duration_since(l.at).as_secs_f32() / 0.18).min(1.0) } else { 1.0 };
            if alpha < 1.0 {
                p.ctx().request_repaint();
            }
            let uv = Rect::from_min_max(pos2(u0, v0), pos2(1.0 - u0, 1.0 - v0));
            p.add(egui::epaint::RectShape::filled(r, CornerRadius::same(round), Color32::WHITE.gamma_multiply(alpha)).with_texture(l.tex.id(), uv));
            p.rect_stroke(r, CornerRadius::same(round), Stroke::new(1.0, Color32::from_black_alpha(40)), StrokeKind::Inside);
            // A folder showing its cover still says it is a folder, in its corner.
            if e.is_dir && alpha > 0.0 {
                let s = (r.width() * 0.15).clamp(14.0, 34.0).round();
                let plate = Rect::from_min_size(pos2(r.left() + s * 0.35, r.bottom() - s * 1.35), Vec2::splat(s));
                p.rect_filled(plate, CornerRadius::same((s * 0.22) as u8), Color32::from_black_alpha((120.0 * alpha) as u8));
                let ic = plate.shrink(s * 0.17);
                crate::listing::paint_entry_icon(p, ic, e, theme::FileCat::Folder, "");
            }
            return;
        }
    }
    let ic = Rect::from_center_size(r.center(), Vec2::splat((r.width() * 0.42).round()));
    if tab::is_place(&t.path) {
        crate::ui::paint_drive(p, ic.shrink(ic.width() * 0.1), false, &crate::drives::Crypt::Plain, pal.text_dim, pal.raised);
    } else {
        crate::listing::paint_entry_icon(p, ic, e, cat, e.ext());
    }
    p.rect_stroke(r, CornerRadius::same(round), Stroke::new(1.0, pal.border), StrokeKind::Inside);
}

/// Where each run of the current sort begins: letters by name, months by date, sizes by
/// tens, kinds by type. Made once per listing and sort (the signature), not per frame: a
/// flattened /usr is hundreds of thousands of items.
fn marks(t: &mut Tab) -> std::rc::Rc<Vec<(usize, String)>> {
    use crate::config::SortKey;
    use std::hash::{Hash, Hasher};
    let n = t.view.len();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (n, t.entries.len(), t.settings.sort as u8, t.settings.desc).hash(&mut h);
    for i in [0, n / 4, n / 2, 3 * n / 4, n.saturating_sub(1)] {
        t.view.get(i).hash(&mut h);
    }
    let sig = h.finish();
    if let Some((s, m)) = &t.stack_marks {
        if *s == sig {
            return m.clone();
        }
    }
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let key = |e: &crate::fsx::Entry| -> Option<String> {
        match t.settings.sort {
            SortKey::Name => {
                let c = e.name.chars().find(|c| !c.is_whitespace())?;
                Some(if c.is_alphabetic() { c.to_uppercase().collect() } else { "#".into() })
            }
            SortKey::Modified | SortKey::Created | SortKey::Accessed => {
                let tm = crate::timefmt::local(match t.settings.sort {
                    SortKey::Created => e.created,
                    SortKey::Accessed => e.accessed,
                    _ => e.modified,
                }?);
                Some(format!("{} {}", tm.tm_year + 1900, MONTHS[tm.tm_mon.clamp(0, 11) as usize]))
            }
            SortKey::Size if e.is_dir => Some("Folders".into()),
            SortKey::Size => Some(
                match (e.size.max(1) as f64).log10().floor() as i32 {
                    ..=2 => "Bytes",
                    3 => "1 KB",
                    4 => "10 KB",
                    5 => "100 KB",
                    6 => "1 MB",
                    7 => "10 MB",
                    8 => "100 MB",
                    9 => "1 GB",
                    _ => "10 GB",
                }
                .into(),
            ),
            SortKey::Type if e.is_dir => Some("Folders".into()),
            SortKey::Type => Some(if e.ext().is_empty() { "None".into() } else { e.ext().to_uppercase() }),
            _ => None,
        }
    };
    let mut out: Vec<(usize, String)> = vec![];
    let mut last: Option<String> = None;
    let dated = matches!(t.settings.sort, SortKey::Modified | SortKey::Created | SortKey::Accessed);
    for (i, &idx) in t.view.iter().enumerate() {
        let Some(k) = key(&t.entries[idx as usize]) else { continue };
        if last.as_ref() != Some(&k) {
            // A date says its year where the year turns and its month otherwise.
            let label = if dated {
                let (y, m) = k.split_once(' ').unwrap_or((&k, ""));
                if last.as_ref().and_then(|l| l.split_once(' ')).is_some_and(|(ly, _)| ly == y) { m.to_string() } else { y.to_string() }
            } else {
                k.clone()
            };
            out.push((i, label));
            last = Some(k);
        }
    }
    // One run is no mark at all.
    if out.len() < 2 {
        out.clear();
    }
    let m = std::rc::Rc::new(out);
    t.stack_marks = Some((sig, m.clone()));
    m
}

/// Ticks for the items under the row, swelling round the cursor; a pin marks it. A click
/// or a drag moves the cursor to the item under the pointer.
fn scrubber(ui: &mut Ui, r: Rect, t: &mut Tab, env: &mut Env, pos: f32, thumbs_here: bool, pal: &theme::Palette) {
    let n = t.view.len();
    let pitch = 5.0;
    let width = r.width().min(760.0);
    let room = (width / pitch).floor().max(2.0) as usize;
    let ticks = n.min(room);
    let span = (ticks.saturating_sub(1)) as f32 * pitch;
    let x0 = (r.center().x - span / 2.0).round();
    let mark_font = FontId::proportional(10.5);
    let base = r.bottom() - 16.0;
    // The item at a tick, and the tick an item (or a place between two) is at.
    let item_at = |k: usize| if ticks <= 1 { 0 } else { (k * (n - 1) + (ticks - 1) / 2) / (ticks - 1) };
    let x_of = |f: f32| if n <= 1 { x0 } else { x0 + f / (n - 1) as f32 * span };
    let fx = x_of(pos);
    let (low, high) = (5.0, 18.0);
    let hit = Rect::from_min_max(pos2(x0 - pitch, base - high - 12.0), pos2(x0 + span + pitch, r.bottom()));
    let resp = ui.interact(hit, Id::new(("stack scrub", t.id)), Sense::click_and_drag());
    crate::ui::note_control(ui, hit, "stack scrubber");
    let tick_at = |x: f32| if span <= 0.0 { 0 } else { (((x - x0) / pitch).round().max(0.0) as usize).min(ticks - 1) };
    let hover = resp.hover_pos().filter(|_| !resp.dragged()).map(|pp| tick_at(pp.x));
    let p = ui.painter().clone();
    for k in 0..ticks {
        let x = x0 + k as f32 * pitch;
        let near = (-((x - fx) / (pitch * 3.5)).powi(2)).exp();
        let h = low + (high - low) * near;
        let col = if hover == Some(k) { pal.text } else { pal.text_dim.gamma_multiply(0.35 + 0.65 * near) };
        p.line_segment([pos2(x, base), pos2(x, base - h)], Stroke::new(1.0, col));
    }
    // The pin: a line taller than any tick and a dot on it.
    p.line_segment([pos2(fx, base), pos2(fx, base - high - 6.0)], Stroke::new(1.5, pal.text));
    p.circle_filled(pos2(fx, base - high - 8.0), 2.5, pal.text);
    // Where each run of the sort begins, under its first tick. Where they would crowd, the
    // longest runs keep theirs: the letter for forty photos matters more than one file's.
    let ms = marks(t);
    let mut order: Vec<usize> = (0..ms.len()).collect();
    let run = |k: usize| ms.get(k + 1).map_or(n, |m| m.0) - ms[k].0;
    order.sort_by_key(|&k| (std::cmp::Reverse(run(k)), k));
    let mut taken: Vec<(f32, f32)> = vec![];
    for k in order {
        let (i, label) = &ms[k];
        let g = p.layout_no_wrap(label.clone(), mark_font.clone(), pal.text_dim);
        let x = x_of(*i as f32).round();
        let (a, b) = (x - 1.0, x + g.size().x + 8.0);
        if b > x0 + span + 24.0 || taken.iter().any(|&(ta, tb)| a < tb && ta < b) {
            continue;
        }
        taken.push((a, b));
        p.line_segment([pos2(x, base + 2.0), pos2(x, base + 4.0)], Stroke::new(1.0, pal.text_dim.gamma_multiply(0.6)));
        p.galley(pos2(x - 1.0, base + 4.0), g, pal.text_dim);
    }
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    // Pointed at: what is there, before it is clicked.
    if let Some(k) = hover {
        let i = item_at(k).min(n - 1);
        let e = t.entries[t.view[i] as usize].clone();
        let fs = env.cfg.font_size;
        let thumb = (fs * 2.4).round();
        let pad = 6.0;
        let name = listing::elided(ui, &listing::display_name(&e, env.cfg), FontId::proportional(fs - 0.5), 260.0, pal.text);
        let w = pad + thumb + 9.0 + name.size().x + pad + 4.0;
        let h = thumb + 2.0 * pad;
        let x = (x0 + k as f32 * pitch - w / 2.0).clamp(r.left(), r.right() - w);
        let card = Rect::from_min_size(pos2(x, base - high - 18.0 - h), vec2(w, h));
        let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::new(("stack peek", t.id))));
        painter.add(egui::epaint::Shadow { offset: [0, 4], blur: 14, spread: 0, color: Color32::from_black_alpha(if pal.dark { 90 } else { 40 }) }.as_shape(card, CornerRadius::same(8)));
        painter.rect(card, CornerRadius::same(8), pal.raised, Stroke::new(1.0, pal.border), StrokeKind::Inside);
        let tr = Rect::from_min_size(card.min + vec2(pad, pad), Vec2::splat(thumb));
        let ppp = ui.ctx().pixels_per_point();
        let pic = (thumbs_here && pictured(&e, t)).then(|| env.thumbs.get(&e.path, e.modified, thumb * ppp)).flatten();
        match pic {
            Some(l) => {
                let [tw, th] = l.tex.size();
                let (tw, th) = (tw as f32, th as f32);
                let s = tw.min(th);
                let (u0, v0) = ((tw - s) / 2.0 / tw, (th - s) / 2.0 / th);
                painter.add(egui::epaint::RectShape::filled(tr, CornerRadius::same(4), Color32::WHITE).with_texture(l.tex.id(), Rect::from_min_max(pos2(u0, v0), pos2(1.0 - u0, 1.0 - v0))));
            }
            None => {
                let ic = tr.shrink(thumb * 0.12);
                crate::listing::paint_entry_icon(&painter, ic, &e, theme::categorize(e.ext(), e.is_dir, e.is_executable()), e.ext());
            }
        }
        painter.galley(pos2(tr.right() + 9.0, card.center().y - name.size().y / 2.0), name, pal.text);
        crate::ui::note_popup(card);
    }
    if resp.is_pointer_button_down_on() || resp.clicked() {
        if let Some(pp) = resp.interact_pointer_pos() {
            let i = item_at(tick_at(pp.x)).min(n - 1);
            let idx = t.view[i];
            if t.cursor != Some(idx) {
                t.select_only(idx);
            }
        }
    }
}
