//! Window chrome: title bar, panels (tabs, path header, footer), dividers, overlays.

use std::path::{Path, PathBuf};

use egui::{self, Align2, Color32, CornerRadius, FontId, Id, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder, Vec2, pos2, vec2};

use crate::app::{Action, App, Drag};
use crate::commands::Cmd;
use crate::fsx;
use crate::listing::{self, Env};
use crate::panel::Side;
use crate::popups::Popup;
use crate::tab;
use crate::theme::{Glyph, Palette, paint_glyph};

pub const TITLE_H: f32 = 36.0;
pub const TAB_H: f32 = 30.0;
/// A pinned tab: its icon with room either side.
const PIN_TAB_W: f32 = 36.0;
pub const HEADER_H: f32 = 34.0;
pub const FOOTER_H: f32 = 30.0;

/// A text field drawn without a frame of its own, keeping the padding it had when that
/// was a switch: 4 by 2.
pub fn bare_edit() -> egui::Frame {
    egui::Frame::NONE.inner_margin(egui::Margin::symmetric(4, 2))
}

/// Small square button with a painted glyph.
/// Test builds: every toolbar/status control drawn this frame, so the user simulations can
/// catch two controls drawn on top of each other. Compiles to nothing otherwise.
#[cfg(test)]
thread_local! {
    pub static CONTROLS: std::cell::RefCell<Vec<(egui::LayerId, Rect, &'static str)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Test builds: popup frames drawn this frame (text inside must stay inside).
#[cfg(test)]
thread_local! {
    pub static POPUPS: std::cell::RefCell<Vec<Rect>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[inline(always)]
pub fn note_popup(r: Rect) {
    #[cfg(test)]
    POPUPS.with(|p| p.borrow_mut().push(r));
    #[cfg(not(test))]
    let _ = r;
}

#[inline(always)]
pub fn note_control(ui: &Ui, r: Rect, what: &'static str) {
    #[cfg(test)]
    if ui.is_visible() && r.width() > 0.5 && r.height() > 0.5 {
        CONTROLS.with(|c| c.borrow_mut().push((ui.layer_id(), r.intersect(ui.clip_rect()), what)));
    }
    #[cfg(not(test))]
    let _ = (ui, r, what);
}

/// In a single-row (non-wrapping) horizontal layout with no room left for `w` points:
/// egui would stack the item on top of its neighbour at the edge, so callers skip it.
/// Seconds left as a person says them: "40s", "3 min", "1 h 5 min".
pub fn time_left(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{} min", s.div_ceil(60)),
        _ => format!("{} h {} min", s / 3600, (s % 3600) / 60),
    }
}

pub fn no_room(ui: &Ui, w: f32) -> bool {
    let l = ui.layout();
    l.main_dir().is_horizontal() && !l.main_wrap && ui.available_width() < w
}

pub fn glyph_button(ui: &mut Ui, g: Glyph, tip: &str, enabled: bool, pal: &Palette) -> egui::Response {
    glyph_button_as(ui, g, tip, enabled, pal, "toolbar button")
}

/// A toolbar glyph button the simulations find by its own name.
pub fn glyph_button_as(ui: &mut Ui, g: Glyph, tip: &str, enabled: bool, pal: &Palette, what: &'static str) -> egui::Response {
    if no_room(ui, 26.0) {
        return ui.allocate_exact_size(Vec2::ZERO, Sense::hover()).1;
    }
    let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 24.0), if enabled { Sense::click() } else { Sense::hover() });
    note_control(ui, rect, what);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, tip));
    if resp.hovered() && enabled {
        // Toolbar buttons light up with a tint of the accent.
        ui.painter().rect_filled(rect, CornerRadius::same(5), pal.row_hover);
    }
    let col = if enabled { if resp.hovered() { pal.text } else { pal.text_dim } } else { pal.text_dim.gamma_multiply(0.4) };
    paint_glyph(ui.painter(), Rect::from_center_size(rect.center(), Vec2::splat(15.0)), g, col);
    if !tip.is_empty() {
        resp.on_hover_text(tip)
    } else {
        resp
    }
}

/// Command tooltip: an optional target line (e.g. the folder a button
/// goes to), "Cmd: <command>", and the command's hotkeys as chips, all centred.
pub fn cmd_tip(resp: egui::Response, target: Option<String>, cmd: crate::commands::Cmd, keymap: &crate::commands::Keymap, pal: &Palette) -> egui::Response {
    let keys: Vec<String> = keymap.bindings(cmd).iter().map(crate::commands::binding_to_string).collect();
    let title = cmd.info().title;
    let pal = *pal;
    resp.on_hover_ui(move |ui| {
        // One line per part, each centred; long targets keep their tail ("…/parent/folder").
        let p = ui.painter();
        let target_g = target.as_ref().map(|t| {
            let mut t = t.clone();
            let max_chars = 64;
            if t.chars().count() > max_chars {
                let tail: String = t.chars().rev().take(max_chars - 1).collect::<Vec<_>>().into_iter().rev().collect();
                t = format!("…{tail}");
            }
            p.layout_no_wrap(t, FontId::proportional(12.5), pal.text)
        });
        let mut job = egui::text::LayoutJob::default();
        job.append("Cmd: ", 0.0, egui::TextFormat { font_id: FontId::proportional(12.0), color: pal.text_dim, ..Default::default() });
        job.append(title, 0.0, egui::TextFormat { font_id: FontId::proportional(12.0), color: pal.text_dim.gamma_multiply(0.7), ..Default::default() });
        let cmd_g = ui.fonts_mut(|f| f.layout_job(job));
        let chips: Vec<_> = keys.iter().map(|k| ui.painter().layout_no_wrap(k.clone(), FontId::proportional(11.0), pal.text_dim)).collect();
        let chips_w: f32 = chips.iter().map(|g| g.size().x + 10.0).sum::<f32>() + 4.0 * chips.len().saturating_sub(1) as f32;
        let w = target_g.as_ref().map(|g| g.size().x).unwrap_or(0.0).max(cmd_g.size().x).max(chips_w);
        let gap = 5.0;
        let h = target_g.as_ref().map(|g| g.size().y + gap).unwrap_or(0.0) + cmd_g.size().y + if chips.is_empty() { 0.0 } else { gap + chips[0].size().y + 6.0 };
        let (r, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
        let mut y = r.top();
        let p = ui.painter();
        if let Some(g) = target_g {
            let gh = g.size().y;
            p.galley(pos2(r.center().x - g.size().x / 2.0, y), g, pal.text);
            y += gh + gap;
        }
        let ch = cmd_g.size().y;
        p.galley(pos2(r.center().x - cmd_g.size().x / 2.0, y), cmd_g, pal.text_dim);
        y += ch + gap;
        let mut x = r.center().x - chips_w / 2.0;
        for g in chips {
            let cr = Rect::from_min_size(pos2(x, y), g.size() + vec2(10.0, 6.0));
            p.rect(cr, CornerRadius::same(4), pal.bg, Stroke::new(1.0, pal.border), StrokeKind::Inside);
            x += cr.width() + 4.0;
            p.galley(cr.min + vec2(5.0, 3.0), g, pal.text_dim);
        }
    })
}

/// Hotkey chip ("Ctrl+P") painted at `pos` (right-aligned). Returns its width.
pub fn paint_chip(p: &egui::Painter, right: egui::Pos2, text: &str, pal: &Palette) -> f32 {
    let font = FontId::proportional(11.0);
    let g = p.layout_no_wrap(text.to_string(), font, pal.text_dim);
    let w = g.size().x + 10.0;
    let r = Rect::from_min_size(pos2(right.x - w, right.y - 9.0), vec2(w, 18.0));
    p.rect(r, CornerRadius::same(4), pal.raised, Stroke::new(1.0, pal.border), StrokeKind::Inside);
    p.galley(pos2(r.left() + 5.0, r.center().y - g.size().y / 2.0), g, pal.text_dim);
    w
}

/// What follows the pointer when files are dragged out to other apps: a card with a
/// file or folder glyph and the name (or "3 items"), in the interface's own font.
pub fn drag_picture(ctx: &egui::Context, paths: &[std::path::PathBuf], pal: &crate::theme::Palette) -> crate::wlclip::Picture {
    let ppp = ctx.pixels_per_point();
    let s = ppp.ceil().clamp(1.0, 3.0);
    let name = |p: &std::path::PathBuf| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string());
    let mut label = if paths.len() == 1 { name(&paths[0]) } else { format!("{} items", paths.len()) };
    let font = FontId::proportional(13.0);
    // Long names are cut to 240 points with an ellipsis.
    let mut galley = ctx.fonts_mut(|f| f.layout_no_wrap(label.clone(), font.clone(), Color32::WHITE));
    while galley.size().x > 240.0 && label.chars().count() > 2 {
        let keep = label.chars().count() * 9 / 10;
        label = label.chars().take(keep.saturating_sub(1)).collect::<String>().trim_end().to_string() + "…";
        galley = ctx.fonts_mut(|f| f.layout_no_wrap(label.clone(), font.clone(), Color32::WHITE));
    }
    let atlas = ctx.fonts(|f| f.image());
    let (tw, th) = (galley.size().x, galley.size().y);
    let (wp, hp) = (10.0 + 16.0 + 8.0 + tw + 12.0, 30.0f32);
    let (w, h) = ((wp * s).ceil() as usize, (hp * s) as usize);
    let text_at = egui::vec2(34.0, (hp - th) / 2.0);
    let folders = paths.iter().all(|p| p.is_dir());
    // Signed distances (in pixels) to a rounded box and to a segment.
    let rbox = |x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32| {
        let (cx, cy, hx, hy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0, (x1 - x0) / 2.0 - r, (y1 - y0) / 2.0 - r);
        let (qx, qy) = ((x - cx).abs() - hx, (y - cy).abs() - hy);
        qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r
    };
    let seg = |x: f32, y: f32, (ax, ay): (f32, f32), (bx, by): (f32, f32)| {
        let (dx, dy) = (bx - ax, by - ay);
        let t = (((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy).max(1e-6)).clamp(0.0, 1.0);
        (x - ax - t * dx).hypot(y - ay - t * dy)
    };
    // Glyph outline in points: a page with a folded corner, or a folder with a tab.
    let (g0, g1, gy0, gy1) = (11.0f32, 24.0f32, 7.5f32, 22.5f32);
    let lines: Vec<((f32, f32), (f32, f32))> = if folders {
        let t = gy0 + 3.0;
        vec![((g0, t), (g1, t)), ((g1, t), (g1, gy1)), ((g1, gy1), (g0, gy1)), ((g0, gy1), (g0, gy0)), ((g0, gy0), (g0 + 5.0, gy0)), ((g0 + 5.0, gy0), (g0 + 7.0, t))]
    } else {
        let f = 4.5;
        vec![((g0, gy0), (g1 - f, gy0)), ((g1 - f, gy0), (g1, gy0 + f)), ((g1, gy0 + f), (g1, gy1)), ((g1, gy1), (g0, gy1)), ((g0, gy1), (g0, gy0)), ((g1 - f, gy0), (g1 - f, gy0 + f)), ((g1 - f, gy0 + f), (g1, gy0 + f))]
    };
    let lines: Vec<((f32, f32), (f32, f32))> = lines.into_iter().map(|(a, b)| ((a.0 * s, a.1 * s), (b.0 * s, b.1 * s))).collect();
    // Text coverage from the font atlas, sampled bilinearly at this scale.
    let mut cover = vec![0f32; w * h];
    let [aw, ah] = atlas.size;
    let alpha = |x: i64, y: i64| if x < 0 || y < 0 || x >= aw as i64 || y >= ah as i64 { 0.0 } else { atlas.pixels[y as usize * aw + x as usize].a() as f32 / 255.0 };
    for pr in &galley.rows {
        for g in &pr.glyphs {
            let uv = g.uv_rect;
            if uv.is_nothing() {
                continue;
            }
            let lt = text_at + (pr.pos + g.pos.to_vec2() + uv.offset).to_vec2();
            let (x0, y0) = ((lt.x * s).floor() as i64, (lt.y * s).floor() as i64);
            let (x1, y1) = (((lt.x + uv.size.x) * s).ceil() as i64, ((lt.y + uv.size.y) * s).ceil() as i64);
            let kx = (uv.max[0] - uv.min[0]) as f32 / uv.size.x.max(1e-3);
            let ky = (uv.max[1] - uv.min[1]) as f32 / uv.size.y.max(1e-3);
            for y in y0.max(0)..y1.min(h as i64) {
                for x in x0.max(0)..x1.min(w as i64) {
                    let tx = uv.min[0] as f32 + ((x as f32 + 0.5) / s - lt.x) * kx - 0.5;
                    let ty = uv.min[1] as f32 + ((y as f32 + 0.5) / s - lt.y) * ky - 0.5;
                    if tx < uv.min[0] as f32 - 1.0 || ty < uv.min[1] as f32 - 1.0 || tx > uv.max[0] as f32 || ty > uv.max[1] as f32 {
                        continue;
                    }
                    let (fx, fy) = (tx.floor(), ty.floor());
                    let (ax, ay) = (tx - fx, ty - fy);
                    let (ix, iy) = (fx as i64, fy as i64);
                    let v = alpha(ix, iy) * (1.0 - ax) * (1.0 - ay) + alpha(ix + 1, iy) * ax * (1.0 - ay) + alpha(ix, iy + 1) * (1.0 - ax) * ay + alpha(ix + 1, iy + 1) * ax * ay;
                    let c = &mut cover[y as usize * w + x as usize];
                    *c = (*c + v).min(1.0);
                }
            }
        }
    }
    let rgb = |c: Color32| [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0];
    let (fill, edge, ink, dim) = (rgb(pal.raised), rgb(pal.border), rgb(pal.text), rgb(pal.text_dim));
    let mut px = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let sd = rbox(fx, fy, 0.5, 0.5, w as f32 - 0.5, h as f32 - 0.5, 8.0 * s);
            let card = (0.5 - sd).clamp(0.0, 1.0);
            if card <= 0.0 {
                continue;
            }
            // Layers, top last: card, border, glyph, text.
            let mut c = [fill[0] * 0.96, fill[1] * 0.96, fill[2] * 0.96, 0.96f32];
            let mut over = |col: [f32; 3], a: f32| {
                for i in 0..3 {
                    c[i] = col[i] * a + c[i] * (1.0 - a);
                }
                c[3] = a + c[3] * (1.0 - a);
            };
            over(edge, (0.5 + 0.5 * s - (sd + 0.5 * s).abs()).clamp(0.0, 1.0));
            let d = lines.iter().map(|&(a, b)| seg(fx, fy, a, b)).fold(f32::MAX, f32::min);
            over(dim, (0.6 * s + 0.5 - d).clamp(0.0, 1.0));
            over(ink, cover[y * w + x]);
            let i = (y * w + x) * 4;
            let to8 = |v: f32| (v * card * 255.0).round().clamp(0.0, 255.0) as u8;
            px[i..i + 4].copy_from_slice(&[to8(c[2]), to8(c[1]), to8(c[0]), to8(c[3])]);
        }
    }
    crate::wlclip::Picture { w, h, scale: s as i32, px }
}

/// A job's label once it is done: "Copying 3 items" becomes "Copied 3 items".
pub fn finished_label(label: &str) -> String {
    job_title(label, false)
}

/// The card's title when nothing in the job succeeded: "Couldn't copy 1 item".
pub fn failed_label(label: &str) -> String {
    job_title(label, true)
}

/// The card's title once Cancel stopped the job: "Cancelled converting clip.mkv to MP4".
pub fn cancelled_label(label: &str) -> String {
    let mut c = label.chars();
    match c.next() {
        Some(f) => format!("Cancelled {}{}", f.to_lowercase(), c.as_str()),
        None => "Cancelled".into(),
    }
}

fn job_title(label: &str, failed: bool) -> String {
    for (now, done, verb) in [("Copying", "Copied", "copy"), ("Moving", "Moved", "move"), ("Deleting", "Deleted", "delete"), ("Extracting", "Extracted", "extract"), ("Compressing", "Compressed", "compress"), ("Converting", "Converted", "convert"), ("Merging", "Merged", "merge"), ("Splitting", "Split", "split"), ("Tidying", "Tidied", "tidy"), ("Renaming", "Renamed", "rename"), ("Creating", "Created", "create")] {
        if let Some(rest) = label.strip_prefix(now) {
            return if failed { format!("Couldn't {verb}{rest}") } else { format!("{done}{rest}") };
        }
    }
    label.to_string()
}

/// What Undo takes back, from the job's title: "Moving notes.txt to the trash" is
/// "Move notes.txt to the trash".
pub fn job_undo_label(label: &str) -> String {
    for (now, verb) in [("Copying", "Copy"), ("Moving", "Move"), ("Deleting", "Delete"), ("Extracting", "Extract"), ("Compressing", "Compress"), ("Converting", "Convert"), ("Merging", "Merge"), ("Splitting", "Split"), ("Tidying", "Tidy"), ("Renaming", "Rename"), ("Creating", "Create")] {
        if let Some(rest) = label.strip_prefix(now) {
            return format!("{verb}{rest}");
        }
    }
    label.to_string()
}

pub fn spinner(p: &egui::Painter, c: egui::Pos2, r: f32, col: Color32, t: f64) {
    let a0 = (t * 6.0) as f32;
    let pts: Vec<egui::Pos2> = (0..=16).map(|i| {
        let a = a0 + i as f32 / 16.0 * 4.2;
        c + vec2(a.cos(), a.sin()) * r
    }).collect();
    p.add(egui::Shape::line(pts, Stroke::new(1.6, col)));
}

/// A drive's icon: a padlock while it is encrypted and locked, else a USB stick for
/// removable media or a disk, which wears a small open padlock (ringed in `bg`) once
/// unlocked.
pub fn paint_drive(p: &egui::Painter, r: Rect, removable: bool, crypt: &crate::drives::Crypt, col: Color32, bg: Color32) {
    use crate::drives::Crypt;
    let g = match crypt {
        Crypt::Locked => Glyph::Lock,
        _ if removable => Glyph::Usb,
        _ => Glyph::Drive,
    };
    paint_glyph(p, r, g, col);
    if matches!(crypt, Crypt::Open(_)) {
        // On the corner, so the drive still reads as one.
        let s = (r.width() * 0.5).clamp(9.0, 24.0);
        let mark = Rect::from_center_size(r.right_bottom(), Vec2::splat(s));
        p.circle_filled(mark.center(), s * 0.55, bg);
        paint_glyph(p, mark, Glyph::LockOpen, col);
    }
}

impl App {
    pub fn draw(&mut self, ctx: &egui::Context) {
        // Whether the files being dragged were picked up with the right button (their drop
        // then asks Move / Copy / Link). Read before drawing: the drop target that takes
        // the release ends the drag during this frame.
        self.drop_right = matches!(self.drag, Some(Drag::Files { right: true, .. }));
        let ctx = ctx.clone();
        let pal = self.pal;
        // ------------------------------------------------ the picker's bar (Open and Save)
        // The window's whole width, under the sidebar too: a dialog's row of answers.
        if self.picker.is_some() {
            ctx.root_ui(|root| {
                egui::Panel::bottom("picker")
                    .exact_size(crate::picker::height(self))
                    .show_separator_line(false)
                    .frame(egui::Frame::NONE.fill(pal.chrome))
                    .show(root, |ui| crate::picker::bar(self, ui))
            });
        }
        // ------------------------------------------------ sidebar (full height)
        if self.sidebar {
            // A narrow window keeps room beside it for the title bar's window buttons and
            // tab list; a wide sidebar left them on top of each other.
            let most = 420f32.min(ctx.content_rect().width() - 230.0).max(150.0);
            let r = ctx.root_ui(|root| {
                egui::Panel::left("sidebar")
                    .resizable(true)
                    .show_separator_line(false)
                    .default_size(self.sidebar_width)
                    .size_range(150.0..=most)
                    .frame(egui::Frame::NONE.fill(pal.sidebar))
                    .show(root, |ui| crate::sidebar::sidebar(self, ui))
            });
            self.sidebar_width = r.response.rect.width();
        }
        // ------------------------------------------------ title bar
        ctx.root_ui(|root| {
            egui::Panel::top("title")
                .exact_size(TITLE_H)
                .show_separator_line(false)
                .frame(egui::Frame::NONE.fill(pal.chrome))
                .show(root, |ui| self.title_bar(ui))
        });
        // ------------------------------------------------ shelf (only while it holds something)
        let full = !self.shelf.items.is_empty();
        let open = if self.cfg.animations { ctx.animate_bool_with_time(Id::new("shelf open"), full, 0.13) } else if full { 1.0 } else { 0.0 };
        self.shelf.shown_h = (crate::shelf::height(self) * open).round();
        if self.shelf.shown_h >= 1.0 {
            ctx.root_ui(|root| {
                egui::Panel::bottom("shelf")
                    .exact_size(self.shelf.shown_h)
                    .show_separator_line(false)
                    .frame(egui::Frame::NONE.fill(pal.chrome))
                    .show(root, |ui| crate::shelf::strip(self, ui))
            });
        }
        // ------------------------------------------------ panels
        ctx.root_ui(|root| egui::CentralPanel::default().frame(egui::Frame::NONE.fill(pal.bg)).show(root, |ui| {
            let full = ui.max_rect();
            let mut leaves = vec![];
            let mut dividers = vec![];
            let panels = Rect::from_min_max(full.min, pos2(full.right(), full.bottom() - crate::terminal::room(self, full)));
            self.tree.layout(panels, 3.0, &mut leaves, &mut dividers, &mut vec![]);
            // Panels still too small to lay out while they grow in: background only.
            let mut growing = std::collections::HashSet::new();
            // Ease each panel's rectangle toward its layout slot. A new panel grows out of
            // the edge it was split from; panels that remain slide to their new size.
            let targets = leaves.clone();
            if self.cfg.animations {
                let dt = ctx.input(|i| i.stable_dt).min(0.05);
                let k = (dt * 20.0).min(1.0);
                let mut moving = false;
                let prev = std::mem::take(&mut self.shown_rects);
                for (id, target) in leaves.iter_mut() {
                    let start = prev.get(id).copied().unwrap_or_else(|| {
                        // A new panel slides in from its outer edge: from the right when it was
                        // split off to the right of an existing panel, otherwise from below.
                        if prev.is_empty() {
                            return *target; // first frame: no entrance animation
                        }
                        let right_split = prev.values().any(|r| r.intersects(*target) && r.left() < target.left() - 1.0);
                        if right_split {
                            Rect::from_min_max(pos2(target.right(), target.top()), target.max)
                        } else {
                            Rect::from_min_max(pos2(target.left(), target.bottom()), target.max)
                        }
                    });
                    let lerp = |a: f32, b: f32| if (a - b).abs() < 0.5 { b } else { a + (b - a) * k };
                    let shown = Rect::from_min_max(pos2(lerp(start.left(), target.left()), lerp(start.top(), target.top())), pos2(lerp(start.right(), target.right()), lerp(start.bottom(), target.bottom())));
                    if shown != *target {
                        moving = true;
                    }
                    self.shown_rects.insert(*id, shown);
                    // Always draw at the eased rect: jumping to the final rect while still
                    // tiny put a new panel on top of the neighbour that hasn't shrunk yet.
                    if shown != *target && (shown.width() < 60.0 || shown.height() < 60.0) {
                        growing.insert(*id);
                    }
                    *target = shown;
                }
                // Two motions at once (a split while a resize still eases) can carry one
                // panel over another: then show the final layout rather than a pile-up.
                let overlap = leaves.iter().enumerate().any(|(i, (_, a))| leaves[i + 1..].iter().any(|(_, b)| a.shrink(1.0).intersects(b.shrink(1.0))));
                if overlap {
                    leaves = targets;
                    growing.clear();
                    self.shown_rects = leaves.iter().copied().collect();
                } else if moving {
                    ctx.request_repaint();
                }
            } else {
                self.shown_rects = leaves.iter().copied().collect();
            }
            let in_title = self.title_tab_panels.clone();
            // The transfers chip goes in the bar nearest the job cards: the bottom right panel's.
            if let Some((id, _)) = leaves.iter().max_by(|a, b| a.1.bottom().total_cmp(&b.1.bottom()).then(a.1.right().total_cmp(&b.1.right()))) {
                self.jobs_panel = *id;
            }
            for (id, rect) in leaves.clone() {
                if growing.contains(&id) {
                    ui.painter().rect_filled(rect, CornerRadius::same(if self.cfg.round_corners { 8 } else { 0 }), pal.panel);
                    continue;
                }
                let tabs_in_title = if in_title.is_empty() { id == self.tree.first_leaf() } else { in_title.contains(&id) };
                self.panel_ui(ui, id, rect, tabs_in_title);
            }
            // Dividers
            for (path, handle, vertical, parent) in dividers {
                let resp = ui.interact(handle, Id::new(("divider", &path)), Sense::click_and_drag());
                if resp.hovered() || resp.dragged() {
                    ctx.set_cursor_icon(if vertical { egui::CursorIcon::ResizeHorizontal } else { egui::CursorIcon::ResizeVertical });
                    ui.painter().rect_filled(handle.shrink(2.0), CornerRadius::same(1), pal.accent.gamma_multiply(0.6));
                }
                if let Some(node) = self.tree.at_path(&path) {
                    if let crate::panel::Node::Split { ratio, .. } = node {
                        if resp.dragged() {
                            if let Some(pp) = resp.interact_pointer_pos() {
                                *ratio = if vertical { (pp.x - parent.left()) / parent.width() } else { (pp.y - parent.top()) / parent.height() }.clamp(0.08, 0.92);
                            }
                        }
                        if resp.double_clicked() {
                            *ratio = 0.5;
                        }
                    }
                }
            }
            // The terminal below, its top following the panels' eased edge.
            let above = leaves.iter().map(|(_, r)| r.bottom()).fold(full.top(), f32::max);
            crate::terminal::pane(self, ui, full, above);
            // Tab drag: dock compass over the hovered panel.
            self.drag_overlay(ui, &leaves);
        }));
        // ------------------------------------------------ overlays
        self.jobs_overlay(&ctx);
        crate::shelf::drop_spot(self, &ctx);
        crate::popups::draw(self, &ctx);
        crate::hints::draw(self, &ctx);
        self.window_edges(&ctx);
        // Files dragged past the window's edge carry on as a system drag, into other
        // apps (the button is still held, so the system can take over the pointer).
        if let Some(Drag::Files { paths, .. }) = &self.drag {
            let out = ctx.input(|i| i.pointer.any_down() && i.pointer.latest_pos().is_some_and(|p| !i.content_rect().contains(p)));
            if out && self.ext_drag.is_none() && crate::clipboard::can_drag_out() {
                let pic = drag_picture(&ctx, paths, &self.pal);
                crate::clipboard::start_drag(paths.clone(), Some(pic), ctx.native_pixels_per_point().unwrap_or(1.0));
                self.drag = None;
                // The release goes to the drop target, never to us: end the press here.
                let pos = ctx.input(|i| i.pointer.latest_pos()).unwrap_or_default();
                let modifiers = ctx.input(|i| i.modifiers);
                for button in [egui::PointerButton::Primary, egui::PointerButton::Secondary] {
                    if ctx.input(|i| i.pointer.button_down(button)) {
                        self.inject.push(egui::Event::PointerButton { pos, button, pressed: false, modifiers });
                    }
                }
                self.inject.push(egui::Event::PointerGone);
            }
        }
        // Releasing outside any target ends drags.
        if ctx.input(|i| i.pointer.any_released()) {
            self.drag = None;
        }
    }

    fn title_bar(&mut self, ui: &mut Ui) {
        let pal = self.pal;
        let rect = ui.max_rect();
        // Drag / maximize on empty space (registered first so widgets win).
        let bg = ui.interact(rect, Id::new("titlebar_bg"), Sense::click_and_drag());
        if bg.drag_started() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if bg.double_clicked() {
            let m = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!m));
        }
        let mut ui = ui.new_child(UiBuilder::new().max_rect(rect.shrink2(vec2(6.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
        // Menu and sidebar toggle live in the sidebar header while it is shown.
        if !self.sidebar {
            let r = glyph_button(&mut ui, Glyph::Menu, "", true, &pal);
            if cmd_tip(r, None, Cmd::MainMenu, &self.keymap, &pal).clicked() {
                self.popup = if matches!(self.popup, Popup::MainMenu) { Popup::None } else { Popup::MainMenu };
            }
            let (r, resp) = ui.allocate_exact_size(vec2(28.0, 26.0), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(r, CornerRadius::same(5), pal.hover);
            }
            let ir = Rect::from_center_size(r.center(), vec2(15.0, 12.0));
            ui.painter().rect_stroke(ir, CornerRadius::same(2), Stroke::new(1.4, pal.text_dim), StrokeKind::Middle);
            ui.painter().line_segment([pos2(ir.left() + 5.0, ir.top()), pos2(ir.left() + 5.0, ir.bottom())], Stroke::new(1.4, pal.text_dim));
            if cmd_tip(resp, Some("Show sidebar".into()), Cmd::ToggleSidebar, &self.keymap, &pal).clicked() {
                self.sidebar = true;
            }
        }
        ui.add_space(6.0);
        // Every panel touching the top edge draws its tab strip here, above its own column
        // (left/right splits get one strip each); the window buttons stay at the far right.
        // Window buttons (3 × 40), a gap, the tab-list chevron (26) and the spacing between
        // those five items, plus a small margin before the last tab strip.
        let right_reserved = 3.0 * 40.0 + 4.0 + 26.0 + 4.0 * ui.spacing().item_spacing.x + 6.0;
        let mut tops: Vec<(u64, Rect)> = {
            let mut leaves = vec![];
            self.tree.leaves(&mut leaves);
            let rects: Vec<(u64, Rect)> = leaves.iter().filter_map(|id| self.shown_rects.get(id).map(|r| (*id, *r))).collect();
            let top = rects.iter().map(|(_, r)| r.top()).fold(f32::INFINITY, f32::min);
            rects.into_iter().filter(|(_, r)| (r.top() - top).abs() < 2.0).collect()
        };
        tops.sort_by(|a, b| a.1.left().total_cmp(&b.1.left()));
        if tops.is_empty() {
            tops.push((self.tree.first_leaf(), rect));
        }
        self.title_tab_panels = tops.iter().map(|t| t.0).collect();
        let strips_left = ui.cursor().min.x;
        let n_tops = tops.len();
        for (i, (pid, pr)) in tops.into_iter().enumerate() {
            let x0 = if i == 0 { strips_left } else { pr.left() + 4.0 };
            // No strip may reach into the window buttons (at narrow widths the last
            // column can be too small for a strip of its own).
            let x1 = if i + 1 == n_tops { rect.right() - right_reserved } else { (pr.right() - 4.0).min(rect.right() - right_reserved) };
            if x1 - x0 < 40.0 {
                continue;
            }
            if i > 0 {
                // A faint seam above the divider between the panels.
                ui.painter().line_segment([pos2(pr.left() - 1.5, rect.top() + 8.0), pos2(pr.left() - 1.5, rect.bottom() - 6.0)], Stroke::new(1.0, pal.border));
            }
            let sr = Rect::from_min_max(pos2(x0, rect.top()), pos2(x1, rect.bottom()));
            // Bottom-aligned like a panel's own strip, so the active tab joins its panel
            // (centred, it left a strip of title bar showing between tab and panel).
            let mut sui = ui.new_child(UiBuilder::new().max_rect(sr).layout(egui::Layout::left_to_right(egui::Align::Max)));
            // Too many tabs for the strip: they clip at its edge instead of running under
            // the next strip or the window buttons.
            sui.set_clip_rect(sr.intersect(ui.clip_rect()));
            self.tab_strip(&mut sui, pid, sr.width());
        }
        ui.allocate_space(vec2((rect.right() - right_reserved - ui.cursor().min.x).max(0.0), 1.0));
        // Tab search chevron + window buttons on the right.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for (kind, tip) in [(2, "Close"), (1, "Maximize"), (0, "Minimize")] {
                let (r, resp) = ui.allocate_exact_size(vec2(40.0, TITLE_H), Sense::click());
                note_control(ui, r, "window button");
                let hovered = resp.hovered();
                if hovered {
                    let (hr, rad) = if self.cfg.rect_buttons { (r, 0) } else { (r.shrink2(vec2(5.0, 5.0)), 6) };
                    ui.painter().rect_filled(hr, CornerRadius::same(rad), if kind == 2 { Color32::from_rgb(0xc4, 0x2b, 0x1c) } else { pal.hover });
                }
                let c = r.center();
                let col = if hovered && kind == 2 { Color32::WHITE } else { pal.text_dim };
                let st = Stroke::new(1.2, col);
                match kind {
                    0 => {
                        ui.painter().line_segment([c + vec2(-5.0, 0.0), c + vec2(5.0, 0.0)], st);
                    }
                    1 => {
                        ui.painter().rect_stroke(Rect::from_center_size(c, Vec2::splat(9.0)), CornerRadius::same(if self.cfg.round_corners { 2 } else { 0 }), st, StrokeKind::Middle);
                    }
                    _ => {
                        ui.painter().line_segment([c + vec2(-5.0, -5.0), c + vec2(5.0, 5.0)], st);
                        ui.painter().line_segment([c + vec2(-5.0, 5.0), c + vec2(5.0, -5.0)], st);
                    }
                }
                if resp.on_hover_text(tip).clicked() {
                    let ctx = ui.ctx();
                    match kind {
                        0 => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                        1 => {
                            let m = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!m));
                        }
                        _ => self.quit = true,
                    }
                }
            }
            ui.add_space(4.0);
            let (r, resp) = ui.allocate_exact_size(vec2(26.0, 24.0), Sense::click());
            note_control(ui, r, "tab list");
            if resp.hovered() {
                ui.painter().rect_filled(r, CornerRadius::same(5), pal.hover);
            }
            ui.painter().add(egui::Shape::line(vec![r.center() + vec2(-5.0, -2.0), r.center() + vec2(0.0, 3.0), r.center() + vec2(5.0, -2.0)], Stroke::new(1.5, pal.text_dim)));
            if cmd_tip(resp, None, Cmd::TabSearch, &self.keymap, &pal).clicked() {
                self.popup = Popup::tab_search();
            }
        });
    }

    /// Tab strip for a panel within the current horizontal layout.
    pub fn tab_strip(&mut self, ui: &mut Ui, panel_id: u64, max_w: f32) {
        let pal = self.pal;
        // Folders a running job copies, moves or unpacks into, and how far it is: their
        // tabs show a ring in place of the folder icon.
        let work: Vec<(PathBuf, f32)> = self.jobs.iter().filter_map(|j| {
            let dest = match &j.kind {
                crate::ops::OpKind::Copy { dest, .. } | crate::ops::OpKind::Move { dest, .. } | crate::ops::OpKind::Extract { dest, .. } => dest.clone(),
                _ => return None,
            };
            let p = j.progress.lock().ok()?;
            if p.finished {
                return None;
            }
            let f = if p.total_bytes > 0 { p.done_bytes as f32 / p.total_bytes as f32 } else if p.total_items > 0 { p.done_items as f32 / p.total_items as f32 } else { 0.0 };
            Some((dest, f.clamp(0.0, 1.0)))
        }).collect();
        let Some(p) = self.panels.get(&panel_id) else { return };
        let n = p.tabs.len();
        let plus_w = 28.0;
        // Tabs share what's left after the + button, the pinned tabs (an icon each) and
        // the gaps between items.
        let gap = ui.spacing().item_spacing.x;
        let pins = p.tabs.iter().filter(|t| t.pinned).count();
        let free = n - pins;
        // A point of slack per tab: placing them rounds to pixels, and a strip filled to
        // the exact width lost its + to the rounding (at other font sizes and Spacings).
        let avail = max_w - plus_w - (gap + 1.0) * n as f32 - PIN_TAB_W * pins as f32;
        let per = (avail / free.max(1) as f32).clamp(56.0, 220.0);
        // Squeezed tabs read "D…", "Q…": the active one keeps room for its name, as the
        // one being looked at.
        // The others never get narrower than their least for it (the strip would overflow).
        let active_free = p.tabs.get(p.active).is_some_and(|t| !t.pinned);
        let active_w = if free > 1 && active_free && per < 120.0 { (avail * 0.4).min(160.0).min(avail - 56.0 * (free - 1) as f32).max(per) } else { per };
        let rest = if active_w > per { ((avail - active_w) / (free - 1) as f32).clamp(56.0, 220.0) } else { per };
        let strip_start = ui.cursor().min;
        let col_x = self.shown_rects.get(&panel_id).map(|r| r.x_range());
        let dragging_tab = matches!(self.drag, Some(Drag::Tab { .. }));
        let dragging_files = matches!(self.drag, Some(Drag::Files { .. }));
        let pointer = ui.ctx().pointer_hover_pos();
        let released = ui.input(|i| i.pointer.any_released());
        let ctrl = ui.input(|i| i.modifiers.ctrl);
        let mut actions = vec![];
        let mut insert_marker: Option<f32> = None;
        let mut drop_index: Option<usize> = None;
        let active_panel = panel_id == self.active;
        let now = ui.input(|i| i.time);
        // Closed tabs leave a slot that collapses over 80 ms.
        const GHOST: f32 = 0.08;
        self.tab_ghosts.retain(|g| g.3.elapsed().as_secs_f32() < GHOST);
        let ghosts: Vec<(usize, f32, f32)> = self.tab_ghosts.iter().filter(|g| g.0 == panel_id).map(|g| (g.1, g.2, g.3.elapsed().as_secs_f32() / GHOST)).collect();
        if !ghosts.is_empty() {
            ui.ctx().request_repaint();
        }
        let ghost_slot = |ui: &mut Ui, i: usize| {
            for &(gi, gw, gt) in &ghosts {
                if gi == i {
                    let e = 1.0 - (1.0 - gt).powi(2);
                    ui.allocate_exact_size(vec2((gw * (1.0 - e)).max(0.0), TAB_H - 4.0), Sense::hover());
                }
            }
        };
        let mut widths: Vec<f32> = vec![];
        for (i, t) in p.tabs.iter().enumerate() {
            ghost_slot(ui, i);
            let grow = if self.cfg.animations { (t.born.elapsed().as_secs_f32() / 0.11).min(1.0) } else { 1.0 };
            if grow < 1.0 {
                ui.ctx().request_repaint();
            }
            let w = if t.pinned { PIN_TAB_W } else if i == p.active { active_w } else { rest } * (0.3 + 0.7 * grow);
            widths.push(w);
            let (r, resp) = ui.allocate_exact_size(vec2(w, TAB_H - 4.0), Sense::click_and_drag());
            let is_active = i == p.active;
            let hovered = resp.hovered();
            let fill = if is_active { pal.panel } else if hovered { pal.hover } else { Color32::TRANSPARENT };
            let rad = if self.cfg.round_corners { 7 } else { 0 };
            if is_active {
                // The active tab is cut from the content below it: rounded top corners and
                // small flared feet that curve out into the strip.
                let full = Rect::from_min_max(r.min, pos2(r.right(), r.bottom() + 4.0));
                ui.painter().rect_filled(full, CornerRadius { nw: rad, ne: rad, sw: 0, se: 0 }, fill);
                if rad > 0 {
                    let f = rad as f32;
                    let strip = pal.chrome;
                    // The feet stay over their own panel: a first tab sits closer to the
                    // panel's edge than a foot is wide, and the strip's colour drawn past it
                    // left a bump on the divider.
                    let feet = match col_x {
                        Some(xr) => ui.painter().with_clip_rect(Rect::from_x_y_ranges(xr, ui.clip_rect().y_range()).intersect(ui.clip_rect())),
                        None => ui.painter().clone(),
                    };
                    for (x, dir) in [(full.left(), -1.0f32), (full.right(), 1.0)] {
                        let foot = Rect::from_min_max(pos2(x.min(x + dir * f), full.bottom() - f), pos2(x.max(x + dir * f), full.bottom()));
                        feet.rect_filled(foot, CornerRadius::ZERO, fill);
                        feet.circle_filled(pos2(x + dir * f, full.bottom() - f), f, strip);
                    }
                }
            } else {
                ui.painter().rect_filled(r.shrink2(vec2(2.0, 3.0)), CornerRadius::same(rad.min(6)), fill);
            }
            let _ = active_panel;
            // Icon or spinner; a pinned tab is its icon alone.
            let ic = Rect::from_center_size(pos2(if t.pinned { r.center().x } else { r.left() + 15.0 }, r.center().y), Vec2::splat(14.0));
            if t.is_loading() {
                spinner(ui.painter(), ic.center(), 5.5, pal.accent, now);
                ui.ctx().request_repaint();
            } else if let Some((_, f)) = work.iter().find(|(d, _)| *d == t.path) {
                // A job's progress into this folder, as a ring.
                note_control(ui, ic, "tab progress");
                let c = ic.center();
                let rad = ic.width() * 0.42;
                ui.painter().circle_stroke(c, rad, Stroke::new(2.2, pal.border));
                let steps = ((f * 40.0).ceil() as usize).max(1);
                let pts: Vec<egui::Pos2> = (0..=steps).map(|k| {
                    let a = -std::f32::consts::FRAC_PI_2 + f * std::f32::consts::TAU * k as f32 / steps as f32;
                    c + vec2(a.cos(), a.sin()) * rad
                }).collect();
                if *f > 0.0 {
                    ui.painter().add(egui::Shape::line(pts, Stroke::new(2.2, pal.accent)));
                }
            } else if tab::is_computer(&t.path) {
                paint_glyph(ui.painter(), ic, Glyph::Drive, pal.text_dim);
            } else if tab::is_github(&t.path) {
                crate::theme::paint_symbol(ui.painter(), ic, "user", pal.text_dim);
            } else if !crate::icons::dir(ui.painter(), ic, &t.path) {
                crate::theme::paint_icon(ui.painter(), ic, crate::theme::FileCat::Folder, "", false);
            }
            // The close button waits until the tab (growing in as it opens) has room beside
            // the icon.
            // A pinned tab has none: it closes from its menu, on purpose.
            let close_w = if (hovered || is_active) && r.width() >= 52.0 && !t.pinned { 18.0 } else { 0.0 };
            let text_rect = Rect::from_min_max(pos2(r.left() + 27.0, r.top()), pos2(r.right() - 6.0 - close_w, r.bottom()));
            let title = t.title();
            if !t.pinned {
                let g = listing::elided(ui, &title, FontId::proportional(self.cfg.font_size - 0.5), text_rect.width(), if is_active { pal.text } else { pal.text_dim });
                ui.painter().galley(pos2(text_rect.left(), r.center().y - g.size().y / 2.0), g, pal.text);
            }
            // Close button
            if close_w > 0.0 {
                let cr = Rect::from_center_size(pos2(r.right() - 14.0, r.center().y), Vec2::splat(16.0));
                let ch = pointer.map(|pp| cr.contains(pp)).unwrap_or(false);
                if ch {
                    ui.painter().rect_filled(cr, CornerRadius::same(4), pal.raised);
                }
                paint_glyph(ui.painter(), cr.shrink(3.0), Glyph::Close, if ch { pal.text } else { pal.text_dim });
                if ch && resp.clicked() {
                    actions.push(Action::CloseTab { panel: panel_id, idx: i });
                    continue;
                }
            }
            let resp = resp.on_hover_text(t.path.display().to_string());
            if resp.clicked() {
                actions.push(Action::ActivateTab { panel: panel_id, idx: i });
            }
            if resp.middle_clicked() && !t.pinned {
                actions.push(Action::CloseTab { panel: panel_id, idx: i });
            }
            if resp.double_clicked() && !tab::is_place(&t.path) {
                actions.push(Action::Open(Popup::properties(vec![t.path.clone()])));
            }
            if resp.secondary_clicked() {
                let pos = resp.interact_pointer_pos().unwrap_or(r.left_bottom());
                actions.push(Action::Open(Popup::TabMenu { panel: panel_id, idx: i, pos }));
            }
            if resp.drag_started() && !dragging_tab {
                self.drag = Some(Drag::Tab { panel: panel_id, idx: i, title: title.clone() });
            }
            // Drop targets while dragging.
            if let Some(pp) = pointer {
                if r.contains(pp) {
                    if dragging_tab {
                        let before = pp.x < r.center().x;
                        insert_marker = Some(if before { r.left() } else { r.right() });
                        drop_index = Some(if before { i } else { i + 1 });
                    }
                    if dragging_files {
                        ui.painter().rect_stroke(r, CornerRadius::same(6), Stroke::new(1.5, pal.accent), StrokeKind::Inside);
                        // Spring-load: hovering a tab with files activates it.
                        if !is_active && ui.input(|i| i.pointer.time_since_last_movement()) > 0.6 {
                            actions.push(Action::ActivateTab { panel: panel_id, idx: i });
                        }
                        if released {
                            if let Some(Drag::Files { paths, .. }) = &self.drag {
                                actions.push(Action::Drop { paths: paths.clone(), dest: t.path.clone(), copy: ctrl, link: false });
                            }
                        }
                    }
                }
            }
        }
        ghost_slot(ui, n);
        if let Some(pm) = self.panels.get_mut(&panel_id) {
            for (t, w) in pm.tabs.iter_mut().zip(widths) {
                t.shown_tab_w = w;
            }
        }
        // New tab (+)
        // No room left (more tabs than fit): no + rather than one drawn over the last tab.
        let room = !no_room(ui, plus_w);
        let (r, resp) = ui.allocate_exact_size(vec2(if room { plus_w } else { 0.0 }, TAB_H - 6.0), Sense::click());
        note_control(ui, r, "new tab");
        if room && resp.hovered() {
            ui.painter().rect_filled(r, CornerRadius::same(5), pal.hover);
        }
        if room {
            paint_glyph(ui.painter(), Rect::from_center_size(r.center(), Vec2::splat(13.0)), Glyph::Plus, pal.text_dim);
        }
        let resp = cmd_tip(resp, None, Cmd::NewTab, &self.keymap, &pal);
        if room && (resp.clicked() || resp.middle_clicked()) {
            actions.push(Action::FocusPanel(panel_id));
            actions.push(Action::Cmd(Cmd::NewTab));
        }
        let strip = Rect::from_min_max(strip_start, pos2(r.right(), strip_start.y + TAB_H));
        // Right-clicking empty strip space opens the tabs options.
        if ui.input(|i| i.pointer.secondary_clicked()) {
            if let Some(pp) = pointer {
                let empty = Rect::from_min_max(pos2(r.right(), strip.top()), pos2(strip.left() + max_w, strip.bottom()));
                if empty.contains(pp) {
                    actions.push(Action::Open(Popup::TabsOptions { panel: panel_id, pos: pp }));
                }
            }
        }
        // Empty-strip right click → tab options
        if let Some(pp) = pointer {
            if dragging_tab && strip.contains(pp) && drop_index.is_none() {
                drop_index = Some(n);
                insert_marker = Some(r.left());
            }
        }
        if let Some(x) = insert_marker {
            ui.painter().line_segment([pos2(x, strip.top() + 4.0), pos2(x, strip.bottom() - 6.0)], Stroke::new(2.0, pal.accent));
            if released {
                if let (Some(Drag::Tab { panel, idx, .. }), Some(to)) = (&self.drag, drop_index) {
                    let (from, idx) = (*panel, *idx);
                    let to_idx = if from == panel_id && to > idx && !ctrl { to - 1 } else { to };
                    if !(from == panel_id && to_idx == idx && !ctrl) {
                        actions.push(Action::MoveTab { from, idx, to: panel_id, to_idx: Some(to_idx), side: None, copy: ctrl });
                    }
                    self.drag = None;
                }
            }
        }
        self.actions.extend(actions);
    }

    fn panel_ui(&mut self, ui: &mut Ui, id: u64, rect: Rect, tabs_in_title: bool) {
        let pal = self.pal;
        let is_active = id == self.active;
        // Focus on any press inside, but not one on a job card or anything else floating
        // over it.
        if ui.input(|i| i.pointer.any_pressed()) {
            if let Some(pp) = ui.ctx().pointer_interact_pos() {
                if rect.contains(pp) && !is_active && matches!(self.popup, Popup::None) && ui.ctx().layer_id_at(pp).is_none_or(|l| l == ui.layer_id()) {
                    self.actions.push(Action::FocusPanel(id));
                }
            }
        }
        let rad = if self.cfg.round_corners { 8 } else { 0 };
        ui.painter().rect_filled(rect, CornerRadius::same(rad), pal.panel);
        let mut y = rect.top();
        // A split squeezed shorter than its tab strip shows none: the strip would be
        // drawn over the panel below it.
        if !tabs_in_title && rect.height() >= TAB_H {
            let r = Rect::from_min_max(pos2(rect.left(), y), pos2(rect.right(), y + TAB_H));
            ui.painter().rect_filled(r, CornerRadius { nw: rad, ne: rad, sw: 0, se: 0 }, pal.chrome);
            let mut child = ui.new_child(UiBuilder::new().max_rect(r.shrink2(vec2(4.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Max)));
            self.tab_strip(&mut child, id, r.width() - 8.0);
            y += TAB_H;
        }
        // Too short for everything (tiny window): drop the status bar, then the toolbar,
        // rather than drawing them on top of each other.
        let room = rect.bottom() - y;
        let show_header = room >= HEADER_H;
        let show_footer = room >= HEADER_H + FOOTER_H;
        let header = Rect::from_min_max(pos2(rect.left(), y), pos2(rect.right(), y + if show_header { HEADER_H } else { 0.0 }));
        if show_header {
            self.header_ui(ui, id, header);
            y += HEADER_H;
        }
        let footer = Rect::from_min_max(pos2(rect.left(), rect.bottom() - if show_footer { FOOTER_H } else { 0.0 }), rect.max);
        let body = Rect::from_min_max(pos2(rect.left(), y), pos2(rect.right(), footer.top()));
        ui.painter().line_segment([body.left_top(), body.right_top()], Stroke::new(1.0, pal.border));
        // Inspector split.
        // The inspector slides open and closed: its share of the body animates.
        let insp_t = {
            let p = &self.panels[&id];
            let open = p.inspector.is_some();
            if self.cfg.animations { ui.ctx().animate_bool_with_time(Id::new(("insp_open", id)), open, 0.13) } else if open { 1.0 } else { 0.0 }
        };
        let ease = |t: f32| 1.0 - (1.0 - t).powi(3);
        let (list_rect, insp_rect) = {
            let p = &self.panels[&id];
            let right_for = |layout: Option<crate::config::InspectorLayout>| match layout.unwrap_or(self.cfg.inspector_layout) {
                crate::config::InspectorLayout::Right => true,
                crate::config::InspectorLayout::Bottom => false,
                crate::config::InspectorLayout::Auto => body.width() > body.height() * 1.15,
            };
            match &p.inspector {
                None if insp_t > 0.001 => {
                    // Closing: the list grows back over the space the inspector left.
                    let r = p.inspector_ratio * ease(insp_t);
                    let right = right_for(None);
                    let gap = if right { Rect::from_min_max(pos2(body.right() - body.width() * r, body.top()), body.max) } else { Rect::from_min_max(pos2(body.left(), body.bottom() - body.height() * r), body.max) };
                    ui.painter().rect_filled(gap, CornerRadius::ZERO, pal.panel);
                    (if right { Rect::from_min_max(body.min, pos2(gap.left() - 1.0, body.bottom())) } else { Rect::from_min_max(body.min, pos2(body.right(), gap.top() - 1.0)) }, None)
                }
                None => (body, None),
                Some(ins) => {
                    let right = right_for(ins.layout);
                    let r = p.inspector_ratio * ease(insp_t);
                    if right {
                        let x = body.right() - body.width() * r;
                        (Rect::from_min_max(body.min, pos2(x - 1.0, body.bottom())), Some((Rect::from_min_max(pos2(x + 1.0, body.top()), body.max), true)))
                    } else {
                        let yy = body.bottom() - body.height() * r;
                        (Rect::from_min_max(body.min, pos2(body.right(), yy - 1.0)), Some((Rect::from_min_max(pos2(body.left(), yy + 1.0), body.max), false)))
                    }
                }
            }
        };
        // The folder's README over the list: its start, or all of it, sliding to its height.
        let list_rect = {
            let size = self.cfg.font_size;
            let (show, hidden) = (self.cfg.show_readme, &self.ud.readme_hidden);
            let p = self.panels.get_mut(&id).expect("drawn panel");
            let t = p.tab_mut();
            let fits = show && !tab::is_place(&t.path) && t.archive.is_none() && !t.flatten && t.smart.is_none() && !t.content.on && t.filter.is_empty() && !t.settings.column_view && !t.settings.stack_view && !hidden.contains(&t.path) && !t.path.starts_with(crate::sidebar::trash_dir());
            let chosen = self.cfg.readme_height;
            let room = list_rect.height();
            let want = if fits {
                crate::readme::sync(t, ui.ctx());
                t.readme.as_deref_mut().map_or(0.0, |c| crate::readme::height(c, size, room, chosen))
            } else {
                0.0
            };
            // Dragged, the card follows the pointer at once; otherwise it slides.
            let grip_id = Id::new(("readme grip", id));
            let dragging = ui.ctx().is_being_dragged(grip_id);
            let h = if self.cfg.animations { ui.ctx().animate_value_with_time(Id::new(("readme h", id)), want, if dragging { 0.0 } else { 0.16 }) } else { want };
            if h >= 1.0 && fits {
                let card = Rect::from_min_max(list_rect.min, pos2(list_rect.right(), list_rect.top() + h));
                let spacing = self.cfg.spacing;
                let pal = self.pal;
                let shut = crate::readme::shut_height(size, chosen, room);
                let act = {
                    let App { panels, thumbs, .. } = self;
                    let t = panels.get_mut(&id).expect("drawn panel").tab_mut();
                    let mut cu = ui.new_child(UiBuilder::new().max_rect(card));
                    cu.set_clip_rect(card.intersect(ui.clip_rect()));
                    t.readme.as_deref_mut().and_then(|c| crate::readme::draw(&mut cu, card, c, size, shut, spacing, &pal, thumbs, id))
                };
                // Its bottom edge drags to the height wanted, kept for every folder; a
                // double click gives the start's own height back.
                let edge = card.bottom() - 6.0;
                let grip = Rect::from_min_max(pos2(card.left() + 8.0, edge - 3.0), pos2(card.right() - 8.0, card.bottom() + 2.0));
                let g = ui.interact(grip, grip_id, if crate::readme::settled(h) || dragging { Sense::click_and_drag() } else { Sense::hover() });
                if crate::readme::settled(h) {
                    note_control(ui, grip, "readme edge");
                }
                if (g.hovered() && crate::readme::settled(h)) || g.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                    let c = pos2(card.center().x, edge);
                    ui.painter().rect_filled(Rect::from_center_size(c, vec2(36.0, 4.0)), CornerRadius::same(2), if g.dragged() { pal.accent } else { pal.text_dim });
                }
                if g.dragged() {
                    if let Some(pp) = g.interact_pointer_pos() {
                        let to = (pp.y - card.top() + 6.0).clamp(crate::readme::min_height(size), crate::readme::max_height(room)).round();
                        if to != self.cfg.readme_height {
                            self.cfg.readme_height = to;
                            self.cfg_dirty = true;
                        }
                        if let Some(c) = self.panels.get_mut(&id).and_then(|p| p.tab_mut().readme.as_deref_mut()) {
                            c.open = false;
                        }
                    }
                }
                if g.double_clicked() {
                    self.cfg.readme_height = 0.0;
                    self.cfg_dirty = true;
                }
                match act {
                    Some(crate::readme::Act::Hide(dir)) => {
                        self.ud.readme_hidden.push(dir);
                        self.ud_dirty = true;
                    }
                    Some(crate::readme::Act::Follow(crate::markdown::Follow::Open(f))) => self.actions.push(Action::OpenFile(f)),
                    Some(crate::readme::Act::Follow(crate::markdown::Follow::Folder(f))) => self.actions.push(Action::Navigate { panel: id, path: f }),
                    Some(crate::readme::Act::Follow(crate::markdown::Follow::Url(u))) if crate::markdown::is_web(&u) => {
                        if let Err(e) = crate::mime::open_uri(&u) {
                            self.toast(format!("Couldn't open: {e}"));
                        }
                    }
                    _ => {}
                }
                Rect::from_min_max(pos2(list_rect.left(), card.bottom()), list_rect.max)
            } else {
                list_rect
            }
        };
        let github = self.panels.get(&id).is_some_and(|p| tab::is_github(&p.tab().path));
        if github {
            let App { panels, cfg, actions, github: page, .. } = self;
            if let Some(panel) = panels.get_mut(&id) {
                panel.rect = rect;
                panel.list_rect = list_rect;
                panel.inspector_rect = None;
            }
            let mut cu = ui.new_child(UiBuilder::new().max_rect(list_rect));
            cu.set_clip_rect(list_rect.intersect(ui.clip_rect()));
            crate::github::page(&mut cu, list_rect, page, cfg, &pal, actions, id);
        }
        if !github {
            let App { panels, cfg, thumbs, stats, recents_set, pinned_set, actions, keymap, drag, clipboard, ud, avatars, .. } = self;
            let panel = panels.get_mut(&id).unwrap();
            panel.rect = rect;
            panel.list_rect = list_rect;
            let cut: std::collections::HashSet<PathBuf> = match clipboard {
                Some((p, true)) => p.iter().cloned().collect(),
                _ => Default::default(),
            };
            let mut env = Env {
                cfg,
                pal: &pal,
                thumbs,
                stats,
                recents: recents_set,
                pinned: pinned_set,
                cut: &cut,
                actions,
                keymap,
                drag,
                panel_id: id,
                active: is_active,
                bookmarks: &ud.bookmarks,
                avatars,
            };
            listing::listing(ui, list_rect, panel, &mut env);
            panel.inspector_rect = None;
            if let Some((ir, right)) = insp_rect {
                // Divider
                let handle = if right { Rect::from_center_size(pos2(ir.left() - 1.0, ir.center().y), vec2(6.0, ir.height())) } else { Rect::from_center_size(pos2(ir.center().x, ir.top() - 1.0), vec2(ir.width(), 6.0)) };
                let resp = ui.interact(handle, Id::new(("insp_div", id)), Sense::drag());
                ui.painter().line_segment(if right { [ir.left_top(), ir.left_bottom()] } else { [ir.left_top(), ir.right_top()] }, Stroke::new(1.0, pal.border));
                if resp.hovered() || resp.dragged() {
                    ui.ctx().set_cursor_icon(if right { egui::CursorIcon::ResizeHorizontal } else { egui::CursorIcon::ResizeVertical });
                }
                if resp.dragged() {
                    if let Some(pp) = resp.interact_pointer_pos() {
                        panel.inspector_ratio = if right { (body.right() - pp.x) / body.width() } else { (body.bottom() - pp.y) / body.height() }.clamp(0.15, 0.85);
                    }
                }
                panel.inspector_rect = Some(ir);
                crate::inspector::inspector(ui, ir, panel, &mut env);
            }
        }
        if show_footer {
            self.footer_ui(ui, id, footer);
        }
        // Inactive panels are dimmed.
        if !is_active && self.panels.len() > 1 {
            let a = ((1.0 - self.cfg.inactive_opacity).clamp(0.0, 0.8) * 255.0) as u8;
            let c = pal.panel;
            ui.painter().rect_filled(rect.shrink2(vec2(0.0, 0.0)), CornerRadius::same(rad), Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a));
        }
    }

    fn header_ui(&mut self, ui: &mut Ui, id: u64, rect: Rect) {
        let pal = self.pal;
        let mut child = ui.new_child(UiBuilder::new().max_rect(rect.shrink2(vec2(6.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
        let ui = &mut child;
        ui.spacing_mut().item_spacing.x = 2.0;
        let (can_back, can_fwd, path, flatten, saved) = {
            let t = self.panels[&id].tab();
            (t.can_back() || t.flatten, t.can_forward(), t.path.clone(), t.flatten || t.expand_all_on, t.saved_scope.is_some())
        };
        let mut cmd = None;
        // Back / Forward / Up tooltips name the folder they lead to.
        let (back_to, fwd_to) = {
            let t = self.panels[&id].tab();
            let show = |p: &Path| if tab::is_computer(p) { "Computer".to_string() } else { p.display().to_string() };
            (t.hist_pos.checked_sub(1).and_then(|i| t.history.get(i)).map(|h| show(&h.path)), t.history.get(t.hist_pos + 1).map(|h| show(&h.path)))
        };
        let up_to = if tab::is_computer(&path) { None } else if tab::is_github(&path) { Some("Computer".into()) } else { Some(path.parent().map(|p| p.display().to_string()).unwrap_or_else(|| "Computer".into())) };
        let r = glyph_button(ui, Glyph::Back, "", can_back, &pal);
        if cmd_tip(r, back_to, Cmd::Back, &self.keymap, &pal).clicked() {
            cmd = Some(Cmd::Back);
        }
        let r = glyph_button(ui, Glyph::Forward, "", can_fwd, &pal);
        if cmd_tip(r, fwd_to, Cmd::Forward, &self.keymap, &pal).clicked() {
            cmd = Some(Cmd::Forward);
        }
        let r = glyph_button(ui, Glyph::Up, "", !tab::is_computer(&path), &pal);
        if cmd_tip(r, up_to, Cmd::Up, &self.keymap, &pal).clicked() {
            cmd = Some(Cmd::Up);
        }
        let hr = glyph_button(ui, Glyph::ChevronDown, "Recent locations", true, &pal);
        if hr.clicked() {
            self.popup = Popup::history(id, hr.rect.left_bottom());
        }
        toolbar_sep(ui, &pal);
        // Lock (saved view settings): open when this folder follows the general view.
        let lr = glyph_button_col(ui, if saved { Glyph::Lock } else { Glyph::LockOpen }, if saved { "View settings saved for this folder" } else { "Save view settings for this folder" }, if saved { pal.accent } else { pal.text_dim }, &pal);
        if lr.clicked() {
            self.popup = Popup::saved_view(id);
        }
        // Bookmark star
        let bookmarked = self.ud.bookmarks.contains(&path);
        let (r, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(r, CornerRadius::same(5), pal.hover);
        }
        paint_bookmark(ui.painter(), Rect::from_center_size(r.center(), Vec2::splat(15.0)), bookmarked, if bookmarked { pal.text } else { pal.text_dim });
        if cmd_tip(resp, Some(if bookmarked { "Remove bookmark".into() } else { "Bookmark this folder".into() }), Cmd::ToggleBookmark, &self.keymap, &pal).clicked() && !tab::is_place(&path) {
            self.actions.push(Action::FocusPanel(id));
            if bookmarked {
                self.actions.push(Action::BookmarkRemove(path.clone()));
            } else {
                self.actions.push(Action::BookmarkAdd(path.clone()));
            }
        }
        toolbar_sep(ui, &pal);
        // ⋮ menu reserved at the right
        let menu_w = 28.0;
        let crumbs_w = ui.available_width() - menu_w - 4.0;
        let crumbs_rect = Rect::from_min_size(ui.cursor().min, vec2(crumbs_w, 26.0));
        let crumbs_rect = Rect::from_center_size(pos2(crumbs_rect.center().x, rect.center().y), crumbs_rect.size());
        ui.allocate_rect(crumbs_rect, Sense::hover());
        let smart = self.panels[&id].tab().smart.as_ref().map(|s| s.name.clone());
        self.breadcrumbs(ui, id, crumbs_rect, &path, flatten, smart);
        let (r, resp) = ui.allocate_exact_size(vec2(menu_w, 24.0), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(r, CornerRadius::same(5), pal.hover);
        }
        for dy in [-4.5, 0.0, 4.5] {
            ui.painter().circle_filled(r.center() + vec2(0.0, dy), 1.5, pal.text_dim);
        }
        if resp.on_hover_text("Panel menu").clicked() {
            self.actions.push(Action::FocusPanel(id));
            self.popup = Popup::panel_menu(id, r.left_bottom());
        }
        if let Some(c) = cmd {
            self.actions.push(Action::FocusPanel(id));
            self.actions.push(Action::Cmd(c));
        }
    }

    fn breadcrumbs(&mut self, ui: &mut Ui, id: u64, rect: Rect, path: &Path, flatten: bool, smart: Option<String>) {
        let pal = self.pal;
        let font = FontId::proportional(self.cfg.font_size);
        // Empty space → GoTo with the current path.
        let bg = ui.interact(rect, Id::new(("crumbs_bg", id)), Sense::click());
        if bg.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(6), pal.raised.gamma_multiply(0.5));
        }
        if bg.clicked() {
            self.actions.push(Action::FocusPanel(id));
            self.actions.push(Action::Cmd(Cmd::AddressBar));
        }
        if bg.secondary_clicked() {
            self.popup = Popup::path_menu(id, path.to_path_buf(), ui.ctx().pointer_interact_pos().unwrap_or(rect.left_bottom()));
        }
        if !self.cfg.breadcrumbs {
            let mut s = if tab::is_computer(path) { "Computer".into() } else if tab::is_github(path) { "Computer › GitHub".into() } else { path.display().to_string() };
            if let Some(n) = &smart {
                s = format!("{s} › {n}");
            }
            let g = listing::elided(ui, &s, font, rect.width() - 12.0, pal.text);
            ui.painter().galley(pos2(rect.left() + 6.0, rect.center().y - g.size().y / 2.0), g, pal.text);
            return;
        }
        // Segments: (label, path)
        let mut segs: Vec<(String, PathBuf)> = vec![];
        if tab::is_computer(path) {
            segs.push(("Computer".into(), path.to_path_buf()));
        } else if tab::is_github(path) {
            segs.push(("Computer".into(), PathBuf::from(tab::COMPUTER)));
            segs.push(("GitHub".into(), path.to_path_buf()));
        } else {
            let home = fsx::home();
            let mut acc = PathBuf::new();
            let compact_home = self.cfg.compact_paths && path.starts_with(&home);
            if compact_home {
                acc = home.clone();
                segs.push(("~".into(), home.clone()));
                for c in path.strip_prefix(&home).unwrap().components() {
                    acc.push(c);
                    segs.push((c.as_os_str().to_string_lossy().into_owned(), acc.clone()));
                }
            } else {
                for c in path.components() {
                    acc.push(c);
                    let label = match c {
                        std::path::Component::RootDir => "/".to_string(),
                        _ => c.as_os_str().to_string_lossy().into_owned(),
                    };
                    segs.push((label, acc.clone()));
                }
            }
            // A network place reads as its name, not as GVFS's folder for it.
            if let Some((root, name)) = crate::sidebar::net_root(path) {
                segs.clear();
                segs.push((name, root.clone()));
                let mut acc = root.clone();
                for c in path.strip_prefix(&root).map(|r| r.components()).into_iter().flatten() {
                    acc.push(c);
                    segs.push((c.as_os_str().to_string_lossy().into_owned(), acc.clone()));
                }
            }
            segs.insert(0, ("Computer".into(), PathBuf::from(tab::COMPUTER)));
        }
        // A smart folder reads as one more step below the folder it searches.
        if let Some(name) = smart {
            segs.push((name, path.to_path_buf()));
        } else if flatten {
            if let Some(last) = segs.last_mut() {
                last.0.push_str(" […]");
            }
        }
        let sep_w = 16.0;
        let mut widths: Vec<f32> = segs.iter().map(|(l, _)| ui.painter().layout_no_wrap(l.clone(), font.clone(), pal.text).size().x + 12.0).collect();
        // Narrow panels: shrink every segment (eliding with "…") before dropping any.
        let avail = rect.width() - 8.0 - sep_w * widths.len() as f32;
        let over = widths.iter().sum::<f32>() - avail;
        if over > 0.0 && avail > 0.0 {
            // The folders above give way; the current one keeps its whole name, and only
            // shrinks below when it alone is wider than the bar.
            let (min_w, n) = (44.0, widths.len());
            let shrinkable: f32 = widths[..n - 1].iter().map(|w| (w - min_w).max(0.0)).sum();
            if shrinkable > 0.0 {
                let k = (over / shrinkable).min(1.0);
                for w in widths[..n - 1].iter_mut() {
                    *w -= (*w - min_w).max(0.0) * k;
                }
            }
        }
        let mut start = 0;
        let total = |s: usize| widths[s..].iter().sum::<f32>() + sep_w * (widths.len() - s) as f32 + if s > 0 { 24.0 } else { 0.0 };
        while start + 1 < segs.len() && total(start) > rect.width() - 8.0 {
            start += 1;
        }
        let mut x = rect.left() + 2.0;
        if start > 0 {
            ui.painter().text(pos2(x + 8.0, rect.center().y), Align2::CENTER_CENTER, "…", font.clone(), pal.text_dim);
            x += 20.0;
        }
        let n = segs.len();
        // Going into a folder, the new names rise into the path; an accent line moves
        // under the folder shown. Kept per panel: the path drawn and since when.
        let now = ui.input(|i| i.time);
        let anim = self.cfg.animations;
        let key = Id::new(("crumb moves", id));
        let (since, kept) = ui.ctx().data_mut(|d| {
            let (old, t0, common) = d.get_temp::<(PathBuf, f64, usize)>(key).unwrap_or((path.to_path_buf(), -10.0, n));
            if old == path {
                (t0, common)
            } else {
                let same = old.components().zip(path.components()).take_while(|(a, b)| a == b).count();
                // Computer and GitHub share their first segment with nothing on disk.
                let same = if tab::is_place(&old) != tab::is_place(path) { 0 } else { same };
                let common = if tab::is_computer(path) { 0 } else { segs.iter().take_while(|(_, sp)| sp.components().count() <= same && path.starts_with(sp)).count() };
                d.insert_temp(key, (path.to_path_buf(), now, common));
                (now, common)
            }
        });
        let rise = if anim { (((now - since) / 0.22).clamp(0.0, 1.0)) as f32 } else { 1.0 };
        let rise = 1.0 - (1.0 - rise).powi(3);
        if rise < 1.0 {
            ui.ctx().request_repaint();
        }
        let mut under: Option<Rect> = None;
        for (i, (label, p)) in segs.iter().enumerate().skip(start) {
            let last = i + 1 == n;
            let fresh = i >= kept && rise < 1.0;
            let room = widths[i].min(rect.right() - x - 4.0).max(20.0);
            let dropped_here = matches!(&self.popup, Popup::PathDropdown(st) if st.panel == id && st.dir == *p);
            let g = listing::elided(ui, label, font.clone(), room - 10.0, if dropped_here { Color32::WHITE } else if last { pal.text } else { pal.text_dim });
            // Shortened segments take only the width of their (elided) text.
            let w = (g.size().x + 12.0).min(room);
            let r = Rect::from_min_size(pos2(x, rect.top() + 1.0), vec2(w, rect.height() - 2.0));
            let resp = ui.interact(r, Id::new(("crumb", id, i)), Sense::click());
            // The segment whose dropdown is open becomes an accent pill.
            let dropped = matches!(&self.popup, Popup::PathDropdown(st) if st.panel == id && st.dir == *p);
            if dropped {
                ui.painter().rect_filled(Rect::from_min_max(r.min, pos2(r.right() + sep_w, r.bottom())), CornerRadius::same(5), pal.accent);
            } else if resp.hovered() {
                ui.painter().rect_filled(r, CornerRadius::same(5), pal.hover);
            }
            let (dy, a) = if fresh { (6.0 * (1.0 - rise), rise) } else { (0.0, 1.0) };
            ui.painter().galley(pos2(r.left() + 6.0, r.center().y - g.size().y / 2.0 + dy), g, pal.text.gamma_multiply(a));
            if last {
                under = Some(Rect::from_min_max(pos2(r.left() + 6.0, r.bottom() - 3.0), pos2(r.left() + 6.0 + (w - 12.0).max(4.0), r.bottom() - 1.0)));
            }
            if resp.clicked() {
                self.actions.push(Action::Navigate { panel: id, path: p.clone() });
            }
            if resp.middle_clicked() {
                self.actions.push(Action::NewTab { panel: id, path: p.clone(), select: None, focus: false });
            }
            if resp.secondary_clicked() {
                self.popup = Popup::path_menu(id, p.clone(), resp.rect.left_bottom());
            }
            x += w;
            // Separator chevron opens the subfolder dropdown for this segment.
            if !tab::is_place(p) || !last {
                let sr = Rect::from_min_size(pos2(x, rect.top() + 3.0), vec2(sep_w, rect.height() - 6.0));
                let sresp = ui.interact(sr, Id::new(("crumb_sep", id, i)), Sense::click());
                if sresp.hovered() {
                    ui.painter().rect_filled(sr, CornerRadius::same(4), pal.hover);
                }
                // After the last segment the chevron only shows on hover.
                if dropped_here {
                    paint_glyph(ui.painter(), Rect::from_center_size(sr.center(), Vec2::splat(10.0)), Glyph::ChevronDown, Color32::WHITE);
                } else if !last || sresp.hovered() || ui.rect_contains_pointer(rect) {
                    paint_glyph(ui.painter(), Rect::from_center_size(sr.center(), Vec2::splat(10.0)), Glyph::ChevronRight, pal.text_dim.gamma_multiply(0.8));
                }
                if sresp.clicked() {
                    let next = segs.get(i + 1).map(|s| s.1.clone());
                    self.popup = Popup::path_dropdown(id, p.clone(), next);
                    if let Popup::PathDropdown(st) = &mut self.popup {
                        st.anchor = Some(sr.left_bottom());
                    }
                }
                x += sep_w;
            }
            if x > rect.right() - 10.0 {
                break;
            }
        }
        // The line under the folder shown slides to it from the last one.
        if let Some(u) = under {
            let t = if anim { 0.22 } else { 0.0 };
            let l = ui.ctx().animate_value_with_time(Id::new(("crumb line l", id)), u.left(), t);
            let r = ui.ctx().animate_value_with_time(Id::new(("crumb line r", id)), u.right(), t);
            let line = Rect::from_min_max(pos2(l.min(r - 4.0), u.top()), pos2(r, u.bottom()));
            if line.is_finite() && rect.contains(line.center()) {
                ui.painter().rect_filled(line, CornerRadius::same(1), pal.accent.gamma_multiply(0.85));
            }
        }
    }

    fn footer_ui(&mut self, ui: &mut Ui, id: u64, rect: Rect) {
        let pal = self.pal;
        ui.painter().line_segment([rect.left_top(), rect.right_top()], Stroke::new(1.0, pal.border));
        // The GitHub page has no files to filter or count: only when its answers are from.
        if self.panels.get(&id).is_some_and(|p| tab::is_github(&p.tab().path)) {
            let s = self.github.status_line();
            let g = listing::elided(ui, &s, FontId::proportional(self.cfg.font_size - 1.5), rect.width() - 24.0, pal.text_dim);
            ui.painter().galley(pos2(rect.left() + 12.0, rect.center().y - g.size().y / 2.0), g, pal.text_dim);
            return;
        }
        let mut child = ui.new_child(UiBuilder::new().max_rect(rect.shrink2(vec2(8.0, 2.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
        let ui = &mut child;
        ui.spacing_mut().item_spacing.x = 4.0;
        let font_small = FontId::proportional(self.cfg.font_size - 1.5);
        let p = self.panels.get_mut(&id).unwrap();
        let focus_req = std::mem::take(&mut p.focus_filter);
        let history = p.inspector.as_ref().is_some_and(|i| i.git.is_some());
        let t = p.tab_mut();
        let (vf, tf, vfi, tfi) = t.counts;
        let glance = crate::glance::shares(t);
        let types_on = !t.settings.only_types.is_empty();
        let flatten = t.flatten || t.expand_all_on;
        let flat_only = t.flatten;
        // Flattened: how long the recursive scan took (live while it runs).
        let walk_label = if t.flatten {
            if t.walk_truncated() {
                Some(format!("first {} items", fmt_count(crate::fsx::WALK_LIMIT)))
            } else {
                t.walk_secs.map(|s| format!("{s:.2} sec")).or_else(|| t.is_loading().then(|| "…".to_string()))
            }
        } else {
            None
        };
        let loading = t.is_loading();
        let branch = crate::gitstatus::repo_for(&t.path).map(|r| (r.branch.clone(), r.ahead, r.behind)).filter(|b| !b.0.is_empty());
        let syncing = crate::gitstatus::repo_for(&t.path).and_then(|r| crate::gitwork::busy(&r.root));
        let mut branch_menu = None;
        let mut chip_at = None;
        let nsel = t.selected.len();
        let sel_size: u64 = if nsel > 1 && nsel < 200_000 {
            let stats = &self.stats.map;
            t.selected.iter().filter_map(|&i| t.entries.get(i as usize)).map(|e| if e.is_dir { stats.get(&e.path).map(|s| s.size).unwrap_or(0) } else { e.size }).sum()
        } else {
            0
        };
        let zoom = t.settings.zoom;
        let view_name = listing::view_label(&t.settings);
        let view_mode = listing::mode_of(&t.settings);
        let error = t.error.clone();
        let (show_folders, show_files) = (t.settings.show_folders, t.settings.show_files);
        let mut toggle_folders = false;
        let mut toggle_files = false;
        let jobs = if id == self.jobs_panel { crate::transfers::summary(&self.jobs) } else { None };
        let jobs_open = matches!(self.popup, Popup::Transfers(_));
        let mut jobs_at = None;
        // The right-hand group (counts, view, zoom…) is drawn twice: first invisibly to
        // measure it (no painting, no clicks), then for real once the filter box is sized.
        let mut right = |ui: &mut Ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            // A small text chip with an optional painted icon; returns its response.
            let chip = |ui: &mut Ui, icon: Option<&dyn Fn(&egui::Painter, Rect)>, text: &str, col: Color32, active: bool, tip: &str| -> egui::Response {
                let g = ui.painter().layout_no_wrap(text.to_string(), font_small.clone(), col);
                let iw = if icon.is_some() { 16.0 } else { 0.0 };
                // Narrow panel: a chip that doesn't fit is left out rather than drawn on
                // top of its neighbour.
                if g.size().x + iw + 12.0 > ui.available_width() {
                    return ui.allocate_exact_size(Vec2::ZERO, Sense::hover()).1;
                }
                let (r, resp) = ui.allocate_exact_size(vec2(g.size().x + iw + 12.0, 20.0), Sense::click());
                note_control(ui, r, "status chip");
                if resp.hovered() || active {
                    ui.painter().rect_filled(r, CornerRadius::same(5), if active { pal.accent.gamma_multiply(0.2) } else { pal.hover });
                }
                if let Some(f) = icon {
                    f(ui.painter(), Rect::from_center_size(pos2(r.left() + 6.0 + 6.0, r.center().y), Vec2::splat(12.0)));
                }
                ui.painter().galley(pos2(r.left() + 6.0 + iw, r.center().y - g.size().y / 2.0), g, col);
                if tip.is_empty() { resp } else { resp.on_hover_text(tip) }
            };
            // While jobs run: how many and how far, opening the list of them.
            if let Some((text, frac)) = &jobs {
                if let Some(r) = crate::transfers::chip(ui, text, *frac, jobs_open, font_small.clone(), &pal) {
                    jobs_at = Some(r.rect);
                    if r.clicked() && ui.is_visible() {
                        self.popup = if jobs_open { Popup::None } else { Popup::Transfers(crate::transfers::State::new(r.rect)) };
                    }
                }
            }
            // Zoom chip with a fill bar proportional to the zoom (none in Columns, which has
            // no zoom).
            let zr = if matches!(view_mode, listing::Mode::Columns | listing::Mode::Stack) { ui.allocate_exact_size(Vec2::ZERO, Sense::hover()).1 } else { chip(ui, None, &format!("{}%", (zoom * 100.0 + 1e-3).floor() as i32), pal.text_dim, false, "View size (Ctrl+wheel)") };
            if zr.rect.width() > 10.0 {
                let bar = Rect::from_min_max(pos2(zr.rect.left() + 5.0, zr.rect.bottom() - 4.0), pos2(zr.rect.right() - 5.0, zr.rect.bottom() - 2.5));
                ui.painter().rect_filled(bar, CornerRadius::same(1), pal.border);
                ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * zoom, bar.height())), CornerRadius::same(1), pal.accent);
            }
            if zr.clicked() {
                self.popup = Popup::zoom(id, zr.rect.right_top());
            }
            let view_icon = move |p: &egui::Painter, r: Rect| listing::paint_view_glyph(p, r, view_mode, pal.text_dim);
            let vr = chip(ui, Some(&view_icon), view_name, pal.text_dim, false, "Change view");
            if vr.clicked() {
                self.popup = Popup::zoom(id, vr.rect.right_top());
            }
            // Folders-first / files-first toggle (a box split by a line; the filled half
            // is the group listed first).
            let ff = self.cfg.folders_first;
            let ff_icon = move |p: &egui::Painter, r: Rect| {
                let b = Rect::from_center_size(r.center(), vec2(12.0, 11.0));
                p.rect_stroke(b, CornerRadius::same(2), Stroke::new(1.2, pal.text_dim), StrokeKind::Middle);
                let half = if ff { Rect::from_min_max(b.min, pos2(b.right(), b.center().y)) } else { Rect::from_min_max(pos2(b.left(), b.center().y), b.max) };
                p.rect_filled(half.shrink(2.0), CornerRadius::same(1), pal.text_dim);
            };
            let fr2 = chip(ui, Some(&ff_icon), "", pal.text_dim, false, if ff { "Folders first · click for files first" } else { "Files first · click for folders first" });
            if fr2.clicked() {
                self.actions.push(Action::Cmd(Cmd::ToggleFoldersFirst));
            }
            // An active type filter shows as a chip (the funnel in the Type header opens it).
            if types_on {
                let fun = move |p: &egui::Painter, r: Rect| funnel(p, r.center(), pal.accent);
                let tr = chip(ui, Some(&fun), "Types", pal.accent, true, "Showing only some file types (Alt+T)");
                if tr.clicked() {
                    self.actions.push(Action::FocusPanel(id));
                    self.popup = Popup::types(id);
                }
            }
            // What kinds of file fill the folder, as a ring; pointing at it names them.
            if !glance.is_empty() && ui.available_width() >= 24.0 {
                let (r, resp) = ui.allocate_exact_size(vec2(24.0, 20.0), Sense::hover());
                note_control(ui, r, "folder at a glance");
                if resp.hovered() {
                    ui.painter().rect_filled(r, CornerRadius::same(5), pal.hover);
                }
                crate::glance::paint_ring(ui, Rect::from_center_size(r.center(), Vec2::splat(13.0)), &glance, &pal);
                resp.on_hover_text(crate::glance::describe(&glance));
            }
            let count = |v: usize, t: usize| if v == t { fmt_count(t) } else { format!("{}/{}", fmt_count(v), fmt_count(t)) };
            let file_icon = move |p: &egui::Painter, r: Rect| {
                crate::theme::paint_symbol(p, r.expand(1.0), "file", pal.text_dim);
            };
            let fr = chip(ui, Some(&file_icon), &count(vfi, tfi), if show_files { pal.text_dim } else { pal.text_dim.gamma_multiply(0.45) }, !show_files, &format!("{vfi} of {tfi} files shown{} · click to {} files", if tfi > vfi { " (hidden, system or filtered items excluded)" } else { "" }, if show_files { "hide" } else { "show" }));
            if fr.clicked() {
                toggle_files = true;
            }
            if !flat_only {
                let dir_icon = move |p: &egui::Painter, r: Rect| {
                    crate::theme::paint_symbol(p, r.expand(1.0), "folder", pal.text_dim);
                };
                let dr = chip(ui, Some(&dir_icon), &count(vf, tf), if show_folders { pal.text_dim } else { pal.text_dim.gamma_multiply(0.45) }, !show_folders, &format!("{vf} of {tf} folders shown · click to {} folders", if show_folders { "hide" } else { "show" }));
                if dr.clicked() {
                    toggle_folders = true;
                }
            }
            if let Some(label) = &walk_label {
                let clock = move |p: &egui::Painter, r: Rect| paint_glyph(p, r, Glyph::Clock, pal.text_dim);
                chip(ui, Some(&clock), label, pal.text_dim, false, "Time taken to list every subfolder");
            }
            if let Some((b, ahead, behind)) = &branch {
                // The branch, then what is not yet pushed (up) and not yet pulled (down).
                let counts: Vec<(bool, String)> = [(true, *ahead), (false, *behind)].into_iter().filter(|c| c.1 > 0).map(|(up, n)| (up, n.to_string())).collect();
                let name = ui.painter().layout_no_wrap(b.clone(), font_small.clone(), pal.text_dim);
                let nums: Vec<(bool, std::sync::Arc<egui::Galley>)> = counts.iter().map(|(up, n)| (*up, ui.painter().layout_no_wrap(n.clone(), font_small.clone(), pal.text_dim))).collect();
                let arrow = (font_small.size * 0.7).round();
                let w = 16.0 + name.size().x + nums.iter().map(|(_, g)| arrow + 2.0 + g.size().x + 6.0).sum::<f32>() + 12.0;
                if w <= ui.available_width() {
                    let (r, resp) = ui.allocate_exact_size(vec2(w, 20.0), Sense::click());
                    note_control(ui, r, "status chip");
                    chip_at = Some(r);
                    if resp.hovered() || history {
                        ui.painter().rect_filled(r, CornerRadius::same(5), if history { pal.accent.gamma_multiply(0.2) } else { pal.hover });
                    }
                    let p = ui.painter();
                    crate::theme::paint_symbol(p, Rect::from_center_size(pos2(r.left() + 12.0, r.center().y), Vec2::splat(13.0)), "git-branch", pal.text_dim);
                    let mut x = r.left() + 22.0;
                    p.galley(pos2(x, r.center().y - name.size().y / 2.0), name.clone(), pal.text_dim);
                    x += name.size().x + 6.0;
                    for (up, g) in &nums {
                        // A small drawn arrow: the bundled fonts have no arrow glyphs.
                        let (cx, cy, h) = (x + arrow / 2.0, r.center().y, arrow / 2.0);
                        let st = Stroke::new(1.3, pal.text_dim);
                        let (tip, tail) = if *up { (cy - h, cy + h) } else { (cy + h, cy - h) };
                        p.line_segment([pos2(cx, tail), pos2(cx, tip)], st);
                        let back = if *up { tip + h * 0.7 } else { tip - h * 0.7 };
                        p.line_segment([pos2(cx - h * 0.7, back), pos2(cx, tip)], st);
                        p.line_segment([pos2(cx + h * 0.7, back), pos2(cx, tip)], st);
                        x += arrow + 2.0;
                        p.galley(pos2(x, r.center().y - g.size().y / 2.0), g.clone(), pal.text_dim);
                        x += g.size().x + 6.0;
                    }
                    let tip = match (*ahead, *behind) {
                        (0, 0) => "Git branch · click for history and branches".to_string(),
                        (a, 0) => format!("{a} commit{} to push · click for history and branches", if a == 1 { "" } else { "s" }),
                        (0, d) => format!("{d} commit{} to pull · click for history and branches", if d == 1 { "" } else { "s" }),
                        (a, d) => format!("{a} to push, {d} to pull · click for history and branches"),
                    };
                    if resp.on_hover_text(tip).clicked() {
                        branch_menu = Some(r);
                    }
                }
            }
            // Beside the branch while it is being pulled or pushed; the answer is a toast.
            if let Some(what) = syncing.filter(|_| branch.is_some()) {
                chip(ui, None, what, pal.text_dim, false, "git is talking to the remote");
            }
            if nsel > 1 {
                let layers = move |p: &egui::Painter, r: Rect| {
                    let st = Stroke::new(1.2, pal.accent);
                    for k in 0..3 {
                        let o = k as f32 * 2.5;
                        p.rect_stroke(Rect::from_min_size(r.min + vec2(o, 5.0 - o), vec2(7.0, 6.0)), CornerRadius::same(1), st, StrokeKind::Middle);
                    }
                };
                chip(ui, Some(&layers), &format!("{} · {}", fmt_count(nsel), fsx::fmt_size(sel_size)), pal.accent, false, "Selected items and their total size");
            }
            if loading && ui.available_width() >= 16.0 {
                let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                spinner(ui.painter(), r.center(), 5.0, pal.accent, ui.input(|i| i.time));
            }
            if let Some(e) = &error {
                if ui.available_width() > 24.0 {
                    ui.add(egui::Label::new(egui::RichText::new(fsx::plain_error(e)).font(font_small.clone()).color(pal.danger)).truncate());
                }
            }
            if !self.seq.is_empty() && ui.available_width() > 24.0 {
                let s: Vec<String> = self.seq.iter().map(|c| c.to_string()).collect();
                ui.add(egui::Label::new(egui::RichText::new(format!("{} …", s.join(", "))).font(font_small.clone()).color(pal.accent)).truncate());
            }
        };
        let right_w = {
            let mut m = ui.new_child(UiBuilder::new().max_rect(rect.shrink2(vec2(8.0, 2.0))).layout(egui::Layout::right_to_left(egui::Align::Center)).invisible());
            right(&mut m);
            m.min_rect().width()
        };
        // Filter box
        // Never wider than the space the right-hand group leaves (narrow split panels).
        let room = rect.right() - right_w - 10.0 - ui.cursor().min.x;
        let fw = (rect.width() * 0.36).clamp(140.0, 380.0).min(room).max(64.0);
        // Tall enough for the text at larger font sizes: a taller line than the box hung
        // below its middle.
        let fh = (self.cfg.font_size * 1.3 + 6.0).clamp(22.0, FOOTER_H - 2.0);
        let frame_rect = Rect::from_min_size(ui.cursor().min, vec2(fw, fh));
        let frame_rect = Rect::from_center_size(pos2(frame_rect.center().x, rect.center().y), frame_rect.size());
        ui.allocate_rect(frame_rect, Sense::hover());
        ui.painter().rect(frame_rect, CornerRadius::same(6), pal.bg, Stroke::new(1.0, if p_filter_active(t) { pal.accent.gamma_multiply(0.7) } else { pal.border }), StrokeKind::Inside);
        paint_glyph(ui.painter(), Rect::from_center_size(pos2(frame_rect.left() + 12.0, frame_rect.center().y), Vec2::splat(12.0)), Glyph::Search, pal.text_dim);
        // A narrow box keeps its room for typing; the palette still switches contents mode.
        let cont_btn = fw >= 110.0;
        let btns = 1.0 + if cont_btn { 1.0 } else { 0.0 } + if t.filter.is_empty() { 0.0 } else { 1.0 } + if types_on { 1.0 } else { 0.0 };
        let nudge = cap_nudge(ui, &egui::TextStyle::Body.resolve(ui.style()));
        let edit_rect = Rect::from_min_max(pos2(frame_rect.left() + 22.0, frame_rect.top() + 1.0 + nudge), pos2(frame_rect.right() - 22.0 * btns - 2.0, frame_rect.bottom() - 1.0 + nudge));
        let shown = vf + vfi;
        // Files when only files are listed; an empty folder has items, not files.
        let changed = if t.git_only.is_some() { " changed" } else { "" };
        let hint = format!("Filter {}{changed} {}…", fmt_count(shown), if shown > 0 && (t.flatten || vf == 0) { if shown == 1 { "file" } else { "files" } } else if shown == 1 { "item" } else { "items" });
        // Short form when the full hint wouldn't fit.
        let hint = if fw < 150.0 { "Filter…".to_string() } else { hint };
        let hint = if t.content.on { if fw < 150.0 { "Find…".into() } else { "Find in files…".into() } } else { hint };
        let before = t.filter.clone();
        let mut fui = ui.new_child(UiBuilder::new().max_rect(edit_rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
        // A field is at least a control's height, which grows with Spacing: taller than the
        // box, it would start at the box's top and centre its text below the box's middle.
        fui.spacing_mut().interact_size.y = edit_rect.height();
        // Conditions and operators the filter understands (size>10M, -draft, a | b) show
        // in the accent, a broken pattern in the danger colour.
        let (accent, text, danger) = (pal.accent, pal.text, pal.danger);
        // One line that scrolls, as egui lays out a single-line field itself: a long
        // query wrapped onto a second line below the box.
        let mut conds = |ui: &Ui, buf: &dyn egui::TextBuffer, _wrap: f32| {
            let job = crate::query::job(buf.as_str(), egui::TextStyle::Body.resolve(ui.style()), text, accent, danger);
            ui.fonts_mut(|f| f.layout_job(job))
        };
        let mut edit = egui::TextEdit::singleline(&mut t.filter).hint_text(egui::RichText::new(hint).color(pal.text_dim)).frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(4, 0))).desired_width(edit_rect.width()).font(egui::TextStyle::Body).align(egui::Align2::LEFT_CENTER);
        if crate::query::marked(&before) {
            edit = edit.layouter(&mut conds);
        }
        let resp = fui.add(edit);
        if focus_req {
            resp.request_focus();
            if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), resp.id) {
                let c = egui::text::CCursor::new(t.filter.chars().count());
                st.cursor.set_char_range(Some(egui::text::CCursorRange::one(c)));
                st.store(ui.ctx(), resp.id);
            }
        }
        let has_focus = resp.has_focus();
        if t.filter != before {
            t.dirty = true;
        }
        let mut bx = frame_rect.right() - 12.0;
        let br = Rect::from_center_size(pos2(bx, frame_rect.center().y), Vec2::splat(18.0));
        let fresp = ui.interact(br, Id::new(("flat", id)), Sense::click()).on_hover_text("Expand: list every file under this folder (Ctrl+E)");
        if fresp.hovered() || flatten {
            ui.painter().rect_filled(br, CornerRadius::same(4), if flatten { pal.accent.gamma_multiply(0.35) } else { pal.hover });
        }
        let c = br.center();
        let st = Stroke::new(1.3, if flatten { pal.accent } else { pal.text_dim });
        // Three rows evenly apart, as far above the middle as below it.
        ui.painter().line_segment([c + vec2(-5.0, -4.5), c + vec2(-5.0, 4.5)], st);
        for dy in [0.0, 4.5] {
            ui.painter().line_segment([c + vec2(-5.0, dy), c + vec2(0.0, dy)], st);
            ui.painter().line_segment([c + vec2(2.0, dy), c + vec2(6.0, dy)], st);
        }
        ui.painter().line_segment([c + vec2(-5.0, -4.5), c + vec2(6.0, -4.5)], st);
        if fresp.clicked() {
            // In Tree view the same button unfolds everything instead of flattening.
            if t.tree_active() || t.expand_all_on {
                t.toggle_expand_all();
            } else {
                t.set_flatten(!flatten);
            }
        }
        bx -= 22.0;
        // Contents mode: the filter looks inside files instead of at their names.
        if cont_btn {
            let on = t.content.on;
            let cr = Rect::from_center_size(pos2(bx, frame_rect.center().y), Vec2::splat(18.0));
            note_control(ui, cr, "contents toggle");
            let cresp = ui.interact(cr, Id::new(("fcont", id)), Sense::click()).on_hover_text("Search inside files (Ctrl+Shift+F)");
            if cresp.hovered() || on {
                ui.painter().rect_filled(cr, CornerRadius::same(4), if on { pal.accent.gamma_multiply(0.35) } else { pal.hover });
            }
            crate::theme::paint_symbol(ui.painter(), cr.shrink(3.0), "text-search", if on { pal.accent } else { pal.text_dim });
            if cresp.clicked() {
                t.content.on = !on;
                t.dirty = true;
                // Back to the text, ready to type, as when switched from the palette.
                ui.memory_mut(|m| m.request_focus(resp.id));
            }
            bx -= 22.0;
        }
        if !t.filter.is_empty() {
            let cr = Rect::from_center_size(pos2(bx, frame_rect.center().y), Vec2::splat(18.0));
            let cresp = ui.interact(cr, Id::new(("fclr", id)), Sense::click()).on_hover_text("Clear filter (Esc)");
            if cresp.hovered() {
                ui.painter().rect_filled(cr, CornerRadius::same(4), pal.hover);
            }
            paint_glyph(ui.painter(), cr.shrink(4.0), Glyph::Close, pal.text_dim);
            if cresp.clicked() {
                t.filter.clear();
                t.dirty = true;
            }
            bx -= 22.0;
        }
        if types_on {
            let cr = Rect::from_center_size(pos2(bx, frame_rect.center().y), Vec2::splat(18.0));
            let cresp = ui.interact(cr, Id::new(("tclr", id)), Sense::click()).on_hover_text("Clear type filters");
            ui.painter().rect_filled(cr, CornerRadius::same(4), if cresp.hovered() { pal.hover } else { pal.accent.gamma_multiply(0.25) });
            funnel(ui.painter(), cr.center(), pal.accent);
            ui.painter().line_segment([cr.left_bottom() + vec2(4.0, -4.0), cr.right_top() + vec2(-4.0, 4.0)], Stroke::new(1.3, pal.accent));
            if cresp.clicked() {
                t.settings.only_types.clear();
                t.dirty = true;
            }
        }
        p.filter_has_focus = has_focus;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| right(ui));
        if let Some(p) = self.panels.get_mut(&id) {
            p.branch_chip = chip_at;
        }
        if id == self.jobs_panel {
            self.jobs_chip = jobs_at;
        }
        if let Some(r) = branch_menu {
            self.actions.push(Action::FocusPanel(id));
            self.actions.push(Action::Cmd(Cmd::GitBranches));
            let _ = r;
        }
        if toggle_folders || toggle_files {
            let t = self.panels.get_mut(&id).unwrap().tab_mut();
            if toggle_folders {
                t.settings.show_folders = !t.settings.show_folders;
            }
            if toggle_files {
                t.settings.show_files = !t.settings.show_files;
            }
            t.dirty = true;
        }
    }

    fn drag_overlay(&mut self, ui: &mut Ui, leaves: &[(u64, Rect)]) {
        let pal = self.pal;
        let Some(pp) = ui.ctx().pointer_hover_pos() else { return };
        let released = ui.input(|i| i.pointer.any_released());
        let ctrl = ui.input(|i| i.modifiers.ctrl);
        match &self.drag {
            Some(Drag::Tab { panel, idx, title }) => {
                let (from, idx, title) = (*panel, *idx, title.clone());
                // Ghost: a miniature of the tab, with the action as a pill below it.
                let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::new("tabghost")));
                let tg = painter.layout_no_wrap(title.clone(), FontId::proportional(12.0), pal.text);
                let ghost = Rect::from_min_size(pp + vec2(10.0, 8.0), vec2((tg.size().x + 56.0).clamp(90.0, 220.0), 24.0));
                painter.add(egui::epaint::Shadow { offset: [0, 3], blur: 10, spread: 0, color: Color32::from_black_alpha(90) }.as_shape(ghost, CornerRadius::same(6)));
                painter.rect(ghost, CornerRadius::same(6), pal.panel, Stroke::new(1.0, pal.border), StrokeKind::Inside);
                let ic = Rect::from_center_size(pos2(ghost.left() + 13.0, ghost.center().y), Vec2::splat(13.0));
                if !crate::icons::dir(&painter, ic, &self.panels.get(&from).and_then(|p| p.tabs.get(idx)).map(|t| t.path.clone()).unwrap_or_default()) {
                    crate::theme::paint_icon(&painter, ic, crate::theme::FileCat::Folder, "", false);
                }
                let tw = ghost.width() - 46.0;
                painter.galley(pos2(ghost.left() + 26.0, ghost.center().y - tg.size().y / 2.0), listing::elided(ui, &title, FontId::proportional(12.0), tw, pal.text), pal.text);
                paint_glyph(&painter, Rect::from_center_size(pos2(ghost.right() - 11.0, ghost.center().y), Vec2::splat(9.0)), Glyph::Close, pal.text_dim);
                let hint = painter.layout_no_wrap(if ctrl { "Copy".into() } else { "Move (Ctrl to copy)".into() }, FontId::proportional(10.5), pal.text_dim);
                let hr = Rect::from_min_size(pos2(ghost.left() + 18.0, ghost.bottom() + 3.0), hint.size() + vec2(12.0, 6.0));
                painter.rect(hr, CornerRadius::same(4), pal.raised, Stroke::new(1.0, pal.border), StrokeKind::Inside);
                painter.galley(hr.min + vec2(6.0, 3.0), hint, pal.text_dim);
                for &(pid, r) in leaves {
                    // Targets live over the file list (between the path bar and the footer).
                    let body = self.panels.get(&pid).map(|p| p.list_rect).filter(|lr| lr.width() > 40.0).unwrap_or(r);
                    if !body.contains(pp) {
                        continue;
                    }
                    // Dock compass: a row of three cards in the middle (narrow
                    // left / square centre / narrow right) and flat cards pinned to the top and
                    // bottom edges. Each card shows where the tab would go as a tinted shape.
                    let c = body.center();
                    let targets = [
                        (None, Rect::from_center_size(c, vec2(36.0, 36.0))),
                        (Some(Side::Left), Rect::from_center_size(c - vec2(36.0, 0.0), vec2(18.0, 36.0))),
                        (Some(Side::Right), Rect::from_center_size(c + vec2(36.0, 0.0), vec2(18.0, 36.0))),
                        (Some(Side::Top), Rect::from_center_size(pos2(c.x, body.top() + 40.0), vec2(42.0, 18.0))),
                        (Some(Side::Bottom), Rect::from_center_size(pos2(c.x, body.bottom() - 14.0), vec2(42.0, 18.0))),
                    ];
                    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::new("dock")));
                    let hit = targets.iter().find(|(_, tr)| tr.expand(5.0).contains(pp)).map(|t| t.0);
                    if let Some(side) = hit {
                        let preview = match side {
                            None => body,
                            Some(Side::Left) => Rect::from_min_max(body.min, pos2(body.center().x, body.bottom())),
                            Some(Side::Right) => Rect::from_min_max(pos2(body.center().x, body.top()), body.max),
                            Some(Side::Top) => Rect::from_min_max(body.min, pos2(body.right(), body.center().y)),
                            Some(Side::Bottom) => Rect::from_min_max(pos2(body.left(), body.center().y), body.max),
                        };
                        painter.rect_filled(preview.shrink(2.0), CornerRadius::same(6), pal.accent.gamma_multiply(0.18));
                        painter.rect_stroke(preview.shrink(2.0), CornerRadius::same(6), Stroke::new(1.0, pal.accent.gamma_multiply(0.45)), StrokeKind::Inside);
                    }
                    for (side, tr) in targets {
                        let h = hit == Some(side);
                        painter.add(egui::epaint::Shadow { offset: [0, 2], blur: 8, spread: 0, color: Color32::from_black_alpha(80) }.as_shape(tr, CornerRadius::same(5)));
                        painter.rect(tr, CornerRadius::same(5), if h { pal.accent } else { pal.raised }, Stroke::new(1.0, if h { pal.accent } else { pal.border }), StrokeKind::Inside);
                        let inner = tr.shrink(4.0);
                        let fill = match side {
                            None => inner,
                            Some(Side::Left) | Some(Side::Right) => inner,
                            Some(Side::Top) => Rect::from_min_max(inner.min, pos2(inner.right(), inner.center().y + 1.0)),
                            Some(Side::Bottom) => Rect::from_min_max(pos2(inner.left(), inner.center().y - 1.0), inner.max),
                        };
                        painter.rect_filled(fill, CornerRadius::same(3), if h { Color32::from_white_alpha(200) } else { pal.accent.gamma_multiply(0.55) });
                    }
                    if let Some(side) = hit {
                        if released {
                            if !(side.is_none() && pid == from) {
                                self.actions.push(Action::MoveTab { from, idx, to: pid, to_idx: None, side, copy: ctrl });
                            }
                            self.drag = None;
                        }
                    }
                }
            }
            Some(Drag::Files { paths, .. }) => {
                let n = paths.len();
                let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::new("fileghost")));
                let shift = ui.input(|i| i.modifiers.shift);
                // Files from other apps are copied unless Shift is held.
                let foreign = self.ext_drag == Some(false);
                let txt = format!("{} {} item{}", if ctrl && shift { "Link" } else if ctrl || (foreign && !shift) { "Copy" } else { "Move" }, n, if n == 1 { "" } else { "s" });
                let g = painter.layout_no_wrap(txt, FontId::proportional(12.0), pal.text);
                let r = Rect::from_min_size(pp + vec2(14.0, 12.0), g.size() + vec2(16.0, 10.0));
                painter.rect(r, CornerRadius::same(6), pal.raised, Stroke::new(1.0, pal.border), StrokeKind::Inside);
                painter.galley(r.min + vec2(8.0, 5.0), g, pal.text);
            }
            _ => {}
        }
    }

    fn jobs_overlay(&mut self, ctx: &egui::Context) {
        let pal = self.pal;
        let has_toast = self.toast.as_ref().map(|t| t.1.elapsed().as_secs_f32() < 3.0).unwrap_or(false);
        // The transfers list shows the same jobs, above the chip.
        let listed = matches!(self.popup, Popup::Transfers(_));
        if (self.jobs.is_empty() || listed) && !has_toast {
            return;
        }
        let screen = ctx.content_rect();
        egui::Area::new(Id::new("jobs"))
            .order(egui::Order::Foreground)
            .anchor(Align2::RIGHT_BOTTOM, vec2(-14.0, -38.0 - self.shelf.shown_h))
            .show(ctx, |ui| {
                ui.set_max_width(340.0f32.min(screen.width() - 40.0));
                let mut remove = None;
                for (k, j) in self.jobs.iter_mut().enumerate().filter(|_| !listed) {
                    egui::Frame::popup(ui.style()).fill(pal.raised).corner_radius(CornerRadius::same(10)).inner_margin(10.0).show(ui, |ui| {
                        ui.set_width(320.0);
                        let mut p = j.progress.lock().unwrap();
                        let done = p.finished;
                        // Its disk is busy with another job: it starts when that one ends.
                        let waiting = if done { None } else { p.waiting.clone() };
                        let failed = done && !p.errors.is_empty() && p.errors.len() >= p.total_items.max(1);
                        let cancelled = j.cancel.load(std::sync::atomic::Ordering::Relaxed);
                        // The title gets what the buttons leave and ends in "…" past it: a
                        // file's name in it can be any length.
                        let h = ui.text_style_height(&egui::TextStyle::Body).max(ui.spacing().interact_size.y);
                        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
                        let mut buttons = ui.new_child(egui::UiBuilder::new().max_rect(row).layout(egui::Layout::right_to_left(egui::Align::Center)));
                        {
                            let ui = &mut buttons;
                            if done {
                                if ui.small_button("×").clicked() {
                                    remove = Some(k);
                                }
                            } else if waiting.is_some() {
                                let c = ui.small_button("Cancel");
                                note_control(ui, c.rect, "job cancel");
                                if c.clicked() {
                                    j.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                                    self.queue.poke();
                                }
                                let b = ui.small_button("Start now");
                                note_control(ui, b.rect, "job start now");
                                if b.clicked() {
                                    p.go = true;
                                    self.queue.poke();
                                }
                            } else {
                                if ui.small_button("Cancel").clicked() {
                                    j.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                                }
                                let paused = j.pause.load(std::sync::atomic::Ordering::Relaxed);
                                if ui.small_button(if paused { "Resume" } else { "Pause" }).clicked() {
                                    j.pause.store(!paused, std::sync::atomic::Ordering::Relaxed);
                                }
                            }
                        }
                        let left = Rect::from_min_max(row.min, pos2((buttons.min_rect().left() - 8.0).max(row.left()), row.max.y));
                        let title = if done && cancelled { cancelled_label(&j.label) } else if failed { failed_label(&j.label) } else if done { finished_label(&j.label) } else { j.label.clone() };
                        ui.new_child(egui::UiBuilder::new().max_rect(left).layout(egui::Layout::left_to_right(egui::Align::Center))).add(egui::Label::new(egui::RichText::new(title).strong()).truncate());
                        let frac = match p.within {
                            // Items done and the share of the one at hand.
                            Some(w) if p.total_items > 0 => ((p.done_items as f32 + w) / p.total_items as f32).min(1.0),
                            _ if p.total_bytes > 0 => p.done_bytes as f32 / p.total_bytes as f32,
                            _ if p.total_items > 0 => p.done_items as f32 / p.total_items as f32,
                            _ => 0.0,
                        };
                        ui.add(egui::ProgressBar::new(if done && !cancelled { 1.0 } else { frac }).desired_height(5.0).fill(if p.errors.is_empty() { pal.accent } else { pal.danger }));
                        let el = p.since.unwrap_or(j.started).elapsed().saturating_sub(p.held).as_secs_f64().max(0.001);
                        let mut line = format!("{} / {} item{}", p.done_items, p.total_items, if p.total_items == 1 { "" } else { "s" });
                        if let Some(w) = &waiting {
                            line = format!("Waiting for “{w}”");
                        } else if p.within.is_some() && !done {
                            line.push_str(&format!(" · {:.0}%", frac * 100.0));
                        } else if p.total_bytes > 0 {
                            let speed = p.done_bytes as f64 / el;
                            line = format!("{} of {} · {}/s", fsx::fmt_size(p.done_bytes), fsx::fmt_size(p.total_bytes), fsx::fmt_size(speed as u64));
                            if !done && speed > 0.0 {
                                let eta = (p.total_bytes.saturating_sub(p.done_bytes)) as f64 / speed;
                                line.push_str(&format!(" · {} left", time_left(eta)));
                            }
                        }
                        if waiting.is_some() {
                            // The other job's title can be any length: one line, cut with "…".
                            ui.add(egui::Label::new(egui::RichText::new(line).small().color(pal.text_dim)).truncate());
                        } else {
                            ui.label(egui::RichText::new(line).small().color(pal.text_dim));
                        }
                        // Bytes copied: the speed over time, under the figures.
                        if p.total_bytes > 0 && p.within.is_none() && waiting.is_none() {
                            let samples = crate::speedline::sample(j.id, p.done_bytes, ui.input(|i| i.time));
                            if samples.len() >= 2 {
                                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
                                note_control(ui, r, "job speed");
                                crate::speedline::paint(ui, r, &samples, &pal);
                            }
                            if !done {
                                ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
                            }
                        }
                        if !done && !p.current.is_empty() && p.clash.is_none() {
                            // One line, cut with "…": a long name wrapped and the card grew and
                            // shrank from file to file.
                            ui.add(egui::Label::new(egui::RichText::new(&p.current).small().color(pal.text_dim)).truncate());
                        }
                        // A name already taken, answered here or in the transfers list.
                        if let Some(c) = p.clash.clone().filter(|_| !done) {
                            let same = p.same;
                            drop(p);
                            crate::transfers::clash_box(ui, j, &c, same, &pal);
                            p = j.progress.lock().unwrap();
                        }
                        if !p.errors.is_empty() {
                            let n = p.errors.len();
                            let errs = p.errors.clone();
                            let denied = p.denied.clone();
                            let busy = p.busy.clone();
                            drop(p);
                            if done && !busy.is_empty() {
                                let b = ui.add(egui::Button::new(egui::RichText::new("What's Using It").small()));
                                note_control(ui, b.rect, "job holders");
                                if b.clicked() {
                                    self.actions.push(Action::Open(crate::holders::open(ctx, crate::holders::What::Paths(busy), None)));
                                }
                            }
                            // Refused for lack of permission: offer the same again as administrator.
                            let task = match &j.kind {
                                crate::ops::OpKind::Copy { dest, .. } if done && !denied.is_empty() => Some(crate::admin::Task::Copy { srcs: denied, dest: dest.clone() }),
                                crate::ops::OpKind::Move { dest, .. } if done && !denied.is_empty() => Some(crate::admin::Task::Move { srcs: denied, dest: dest.clone() }),
                                crate::ops::OpKind::Delete { .. } if done && !denied.is_empty() => Some(crate::admin::Task::Delete { paths: denied }),
                                crate::ops::OpKind::Perms { change, deep, .. } if done && !denied.is_empty() => change.admin(denied, *deep),
                                _ => None,
                            };
                            if let (Some(task), Some(tools)) = (task, crate::drives::Tools::current().filter(|t| t.has("pkexec"))) {
                                if ui.add(egui::Button::new(egui::RichText::new("Retry as administrator").small())).clicked() {
                                    self.actions.push(Action::StartJob(crate::ops::OpKind::Admin { task, tools }));
                                    remove = Some(k);
                                }
                            }
                            if ui.add(egui::Label::new(egui::RichText::new(format!("{n} error{} · {}", if n == 1 { "" } else { "s" }, if j.show_errors { "hide" } else { "show" })).small().color(pal.danger)).sense(Sense::click())).clicked() {
                                j.show_errors = !j.show_errors;
                            }
                            if j.show_errors {
                                egui::ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                                    for e in errs {
                                        ui.label(egui::RichText::new(fsx::plain_error(&e)).small());
                                    }
                                });
                            }
                        }
                    });
                    ui.add_space(6.0);
                }
                if let Some(k) = remove {
                    self.jobs.remove(k);
                }
                crate::speedline::keep_only(&self.jobs.iter().map(|j| j.id).collect::<Vec<_>>());
                if let Some((msg, at)) = &self.toast {
                    let el = at.elapsed().as_secs_f32();
                    if el < 3.0 {
                        let a = if el > 2.6 { (3.0 - el) / 0.4 } else { 1.0 };
                        egui::Frame::popup(ui.style()).fill(pal.raised.gamma_multiply(a)).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
                            ui.label(egui::RichText::new(msg).color(pal.text.gamma_multiply(a)));
                        });
                        ctx.request_repaint();
                    }
                }
            });
    }

    /// Invisible resize handles at the window edges (we draw our own decorations).
    fn window_edges(&self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().maximized.unwrap_or(false) || i.viewport().fullscreen.unwrap_or(false)) {
            return;
        }
        let r = ctx.content_rect();
        let m = 4.0;
        use egui::ResizeDirection as D;
        let zones = [
            (Rect::from_min_max(r.min, pos2(r.left() + m * 2.0, r.top() + m * 2.0)), D::NorthWest, egui::CursorIcon::ResizeNorthWest),
            (Rect::from_min_max(pos2(r.right() - m * 2.0, r.top()), pos2(r.right(), r.top() + m * 2.0)), D::NorthEast, egui::CursorIcon::ResizeNorthEast),
            (Rect::from_min_max(pos2(r.left(), r.bottom() - m * 2.0), pos2(r.left() + m * 2.0, r.bottom())), D::SouthWest, egui::CursorIcon::ResizeSouthWest),
            (Rect::from_min_max(pos2(r.right() - m * 2.0, r.bottom() - m * 2.0), r.max), D::SouthEast, egui::CursorIcon::ResizeSouthEast),
            (Rect::from_min_max(r.min, pos2(r.right(), r.top() + m)), D::North, egui::CursorIcon::ResizeVertical),
            (Rect::from_min_max(pos2(r.left(), r.bottom() - m), r.max), D::South, egui::CursorIcon::ResizeVertical),
            (Rect::from_min_max(r.min, pos2(r.left() + m, r.bottom())), D::West, egui::CursorIcon::ResizeHorizontal),
            (Rect::from_min_max(pos2(r.right() - m, r.top()), r.max), D::East, egui::CursorIcon::ResizeHorizontal),
        ];
        let Some(pp) = ctx.pointer_hover_pos() else { return };
        for (z, dir, cur) in zones {
            if z.contains(pp) {
                ctx.set_cursor_icon(cur);
                if ctx.input(|i| i.pointer.primary_pressed()) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
                }
                break;
            }
        }
    }
}

fn p_filter_active(t: &tab::Tab) -> bool {
    !t.filter.is_empty() || t.flatten || t.git_only.is_some()
}

/// How far down text in `font` must go for its capitals to stand in the middle of the
/// line egui centres: the line keeps more room below the baseline than above the
/// capitals, so centred text otherwise sits high. Whole pixels, so it stays crisp.
pub fn cap_nudge(ui: &Ui, font: &FontId) -> f32 {
    let g = ui.fonts_mut(|f| f.layout_no_wrap("H".into(), font.clone(), Color32::WHITE));
    let Some((row, gl)) = g.rows.first().and_then(|r| r.glyphs.first().map(|gl| (r, gl))) else { return 0.0 };
    if gl.uv_rect.is_nothing() {
        return 0.0;
    }
    let top = row.pos.y + gl.pos.y + gl.uv_rect.offset.y;
    let mid = top + gl.uv_rect.size.y / 2.0;
    let ppp = ui.ctx().pixels_per_point();
    // Half a pixel or less stays put: moved, it would be as far off the other way.
    ((g.size().y / 2.0 - mid) * ppp + 0.49).floor() / ppp
}

pub fn fmt_count(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

pub fn star(p: &egui::Painter, c: egui::Pos2, r: f32, col: Color32, filled: bool) {
    let pts: Vec<egui::Pos2> = (0..10)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
            let rr = if i % 2 == 0 { r } else { r * 0.45 };
            c + vec2(a.cos() * rr, a.sin() * rr)
        })
        .collect();
    if filled {
        // Fan from the center (star is concave).
        for i in 0..10 {
            p.add(egui::Shape::convex_polygon(vec![c, pts[i], pts[(i + 1) % 10]], col, Stroke::NONE));
        }
    } else {
        p.add(egui::Shape::closed_line(pts, Stroke::new(1.2, col)));
    }
}

pub fn funnel(p: &egui::Painter, c: egui::Pos2, col: Color32) {
    p.add(egui::Shape::convex_polygon(vec![c + vec2(-5.0, -4.0), c + vec2(5.0, -4.0), c + vec2(1.0, 1.0), c + vec2(-1.0, 1.0)], col, Stroke::NONE));
    p.rect_filled(Rect::from_min_max(c + vec2(-1.0, 1.0), c + vec2(1.0, 5.0)), CornerRadius::ZERO, col);
}

/// Thin vertical divider between toolbar groups.
fn toolbar_sep(ui: &mut Ui, pal: &Palette) {
    if no_room(ui, 9.0) {
        return;
    }
    let (r, _) = ui.allocate_exact_size(vec2(9.0, 20.0), Sense::hover());
    ui.painter().line_segment([pos2(r.center().x, r.top() + 1.0), pos2(r.center().x, r.bottom() - 1.0)], Stroke::new(1.0, pal.border));
}

/// Toolbar icon button with an explicit icon color.
fn glyph_button_col(ui: &mut Ui, g: Glyph, tip: &str, col: Color32, pal: &Palette) -> egui::Response {
    if no_room(ui, 26.0) {
        return ui.allocate_exact_size(Vec2::ZERO, Sense::hover()).1;
    }
    let (r, resp) = ui.allocate_exact_size(vec2(26.0, 24.0), Sense::click());
    note_control(ui, r, "toolbar button");
    if resp.hovered() {
        ui.painter().rect_filled(r, CornerRadius::same(5), pal.hover);
    }
    paint_glyph(ui.painter(), Rect::from_center_size(r.center(), Vec2::splat(15.0)), g, col);
    resp.on_hover_text(tip)
}

/// Bookmark ribbon: outlined, or solid when the folder is bookmarked.
pub fn paint_bookmark(p: &egui::Painter, r: Rect, filled: bool, col: Color32) {
    crate::theme::paint_symbol(p, r, if filled { "bookmark@bold" } else { "bookmark" }, col);
}
