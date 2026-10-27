//! Transfers: the status bar's chip while jobs run ("3 jobs · 64%") and the list it opens
//! above itself. Jobs that move data onto one disk take turns (ops::Queue); here the
//! waiting ones are put in another order, by dragging or with Alt and the arrows, the
//! running ones paused, and a name already taken is answered inside its job.

use egui::{Align2, Color32, CornerRadius, FontId, Id, Key, Modifiers, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

use crate::app::App;
use crate::ops::{Clash, Conflict, Job};
use crate::popups as pp;
use crate::theme::Palette;

pub struct State {
    /// The chip it opens above.
    pub anchor: Rect,
    /// The waiting job the keys move, by id.
    sel: Option<u64>,
    /// The waiting job being dragged to another place.
    drag: Option<u64>,
}

impl State {
    pub fn new(anchor: Rect) -> State {
        State { anchor, sel: None, drag: None }
    }
}

/// Jobs not finished yet: the chip shows while there are any.
pub fn live(app: &App) -> usize {
    app.jobs.iter().filter(|j| j.finished_at.is_none() && !j.progress.lock().is_ok_and(|p| p.finished)).count()
}

/// The chip's text and how far along the running jobs are together, 0 to 1. None when
/// nothing is running or waiting.
pub fn summary(jobs: &[Job]) -> Option<(String, f32)> {
    let (mut n, mut done, mut total, mut items, mut items_done) = (0usize, 0u64, 0u64, 0f32, 0f32);
    for j in jobs {
        let Ok(p) = j.progress.lock() else { continue };
        if p.finished || j.finished_at.is_some() {
            continue;
        }
        n += 1;
        if p.waiting.is_some() {
            continue;
        }
        if p.total_bytes > 0 {
            done += p.done_bytes;
            total += p.total_bytes;
        } else if p.total_items > 0 {
            items += 1.0;
            items_done += (p.done_items as f32 + p.within.unwrap_or(0.0)) / p.total_items as f32;
        }
    }
    if n == 0 {
        return None;
    }
    // Bytes where they are counted, else each job's share of its items.
    let frac = if total > 0 {
        let b = done as f32 / total as f32;
        if items > 0.0 { (b + items_done) / (1.0 + items) } else { b }
    } else if items > 0.0 {
        items_done / items
    } else {
        0.0
    };
    let frac = frac.clamp(0.0, 1.0);
    Some((format!("{n} job{} · {:.0}%", if n == 1 { "" } else { "s" }, frac * 100.0), frac))
}

/// "2.1 of 3.3 GB": the unit once when both share it.
pub fn of_size(done: u64, total: u64) -> String {
    let (a, b) = (crate::fsx::fmt_size(done), crate::fsx::fmt_size(total));
    match (a.rsplit_once(' '), b.rsplit_once(' ')) {
        (Some((n, u)), Some((_, v))) if u == v => format!("{n} of {b}"),
        _ => format!("{a} of {b}"),
    }
}

/// What a running job has done, its speed, and its name at hand.
struct Snap {
    id: u64,
    label: String,
    frac: f32,
    line: String,
    speed: String,
    current: String,
    paused: bool,
    waiting: bool,
    clash: Option<Clash>,
    same: bool,
}

fn snap(j: &Job) -> Option<Snap> {
    let p = j.progress.lock().ok()?;
    if p.finished || j.finished_at.is_some() {
        return None;
    }
    let paused = j.pause.load(std::sync::atomic::Ordering::Relaxed);
    let frac = match p.within {
        Some(w) if p.total_items > 0 => ((p.done_items as f32 + w) / p.total_items as f32).min(1.0),
        _ if p.total_bytes > 0 => p.done_bytes as f32 / p.total_bytes as f32,
        _ if p.total_items > 0 => p.done_items as f32 / p.total_items as f32,
        _ => 0.0,
    };
    let el = p.since.unwrap_or(j.started).elapsed().saturating_sub(p.held).as_secs_f64().max(0.001);
    let mut parts = vec![];
    let mut speed = String::new();
    if p.total_bytes > 0 {
        parts.push(of_size(p.done_bytes, p.total_bytes));
    }
    if p.total_files > 1 {
        parts.push(format!("{} of {} files", crate::table::fmt_count(p.done_files), crate::table::fmt_count(p.total_files)));
    } else if p.total_items > 1 {
        parts.push(format!("{} of {} items", p.done_items, p.total_items));
    }
    if paused {
        parts.push("paused".into());
    } else if p.clash.is_some() {
        parts.push("waiting for you".into());
    } else if p.total_bytes > 0 && p.done_bytes > 0 {
        let s = p.done_bytes as f64 / el;
        speed = format!("{}/s", crate::fsx::fmt_size(s as u64));
        parts.push(format!("{} left", crate::ui::time_left(p.total_bytes.saturating_sub(p.done_bytes) as f64 / s)));
    } else if p.within.is_some() {
        parts.push(format!("{:.0}%", frac * 100.0));
    }
    Some(Snap { id: j.id, label: j.label.clone(), frac, line: parts.join(" · "), speed, current: p.current.clone(), paused, waiting: p.waiting.is_some(), clash: p.clash.clone(), same: p.same })
}

/// A name already taken, asked inside its job: Keep both, Replace, Skip (Merge for two
/// folders), and the same for the others it has. True when answered.
pub fn clash_box(ui: &mut Ui, job: &Job, clash: &Clash, same: bool, pal: &Palette) -> bool {
    let mut answer = None;
    let mut flip = false;
    pp::card(ui, pal, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        let font = egui::TextStyle::Body.resolve(ui.style());
        let mut job_text = egui::text::LayoutJob::default();
        job_text.append(&clash.name, 0.0, egui::TextFormat { font_id: crate::theme::semibold(font.size), color: pal.text, ..Default::default() });
        let rest = if clash.note.is_empty() { " is already there".to_string() } else { format!(" is already there, {}", clash.note) };
        job_text.append(&rest, 0.0, egui::TextFormat { font_id: font, color: pal.text_dim, ..Default::default() });
        job_text.wrap.max_width = ui.available_width();
        ui.add(egui::Label::new(job_text).wrap());
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let b = ui.add(egui::Button::new(egui::RichText::new("Keep both").color(pal.on_accent())).fill(pal.accent).small());
            crate::ui::note_control(ui, b.rect, "clash keep both");
            if b.clicked() {
                answer = Some(Conflict::KeepBoth);
            }
            let r = ui.add(egui::Button::new("Replace").small()).on_hover_text("The one there goes to the trash; Undo brings it back");
            crate::ui::note_control(ui, r.rect, "clash replace");
            if r.clicked() {
                answer = Some(Conflict::Overwrite);
            }
            if clash.dirs {
                let m = ui.add(egui::Button::new("Merge").small()).on_hover_text("Copy into the folder there, replacing files");
                crate::ui::note_control(ui, m.rect, "clash merge");
                if m.clicked() {
                    answer = Some(Conflict::Merge);
                }
            }
            let s = ui.add(egui::Button::new("Skip").small());
            crate::ui::note_control(ui, s.rect, "clash skip");
            if s.clicked() {
                answer = Some(Conflict::Skip);
            }
        });
        if clash.others > 0 {
            let label = format!("Same for the other {}", clash.others);
            let g = ui.painter().layout_no_wrap(label, egui::TextStyle::Small.resolve(ui.style()), pal.text_dim);
            let (r, resp) = ui.allocate_exact_size(vec2(20.0 + g.size().x, g.size().y.max(16.0)), Sense::click());
            crate::ui::note_control(ui, r, "clash same");
            pp::checkbox_glyph(ui.painter(), Rect::from_center_size(pos2(r.left() + 7.0, r.center().y), Vec2::splat(13.0)), same, pal);
            ui.painter().galley(pos2(r.left() + 20.0, r.center().y - g.size().y / 2.0), g, if resp.hovered() { pal.text } else { pal.text_dim });
            flip = resp.clicked();
        }
    });
    let Ok(mut p) = job.progress.lock() else { return false };
    if flip {
        p.same = !p.same;
    }
    match answer {
        Some(c) => {
            let all = p.same;
            p.answer = Some((c, all));
            true
        }
        None => false,
    }
}

/// The list above the chip. Returns false when it closes.
pub fn popup(app: &mut App, ctx: &egui::Context, st: &mut State) -> bool {
    let pal = app.pal;
    let order = app.queue.waiting();
    let snaps: Vec<Snap> = app.jobs.iter().filter_map(snap).collect();
    if snaps.is_empty() {
        return false;
    }
    let running: Vec<&Snap> = snaps.iter().filter(|s| !s.waiting).collect();
    // The waiting in the order they will go; one not in line yet goes last.
    let mut waiting: Vec<&Snap> = snaps.iter().filter(|s| s.waiting).collect();
    waiting.sort_by_key(|s| order.iter().position(|&o| o == s.id).unwrap_or(usize::MAX));
    let ids: Vec<u64> = waiting.iter().map(|s| s.id).collect();
    if st.sel.is_some_and(|s| !ids.contains(&s)) {
        st.sel = None;
    }
    let take = |m: Modifiers, k: Key| crate::devtools::take(ctx, m, k);
    if take(Modifiers::NONE, Key::Escape) {
        return false;
    }
    // Alt first: a plain key's check would take Alt and the key as well.
    let (alt_up, alt_down) = (take(Modifiers::ALT, Key::ArrowUp), take(Modifiers::ALT, Key::ArrowDown));
    let (up, down) = (take(Modifiers::NONE, Key::ArrowUp), take(Modifiers::NONE, Key::ArrowDown));
    let (space, del, enter) = (take(Modifiers::NONE, Key::Space), take(Modifiers::NONE, Key::Delete), take(Modifiers::NONE, Key::Enter));
    let at = st.sel.and_then(|s| ids.iter().position(|&x| x == s));
    if down {
        st.sel = ids.get(at.map_or(0, |a| (a + 1).min(ids.len().saturating_sub(1)))).copied();
    }
    if up {
        st.sel = at.and_then(|a| a.checked_sub(1)).and_then(|a| ids.get(a)).copied().or(st.sel);
    }
    if let (Some(a), Some(id)) = (at, st.sel) {
        if alt_up && a > 0 {
            app.queue.move_to(id, Some(ids[a - 1]));
        }
        if alt_down && a + 1 < ids.len() {
            app.queue.move_to(id, ids.get(a + 2).copied());
        }
    }
    // Space pauses or resumes what runs; Enter starts the waiting one picked now, Delete
    // cancels it.
    if space {
        let pause = running.iter().any(|s| !s.paused);
        for j in app.jobs.iter().filter(|j| running.iter().any(|s| s.id == j.id)) {
            j.pause.store(pause, std::sync::atomic::Ordering::Relaxed);
        }
    }
    if let Some(id) = st.sel.filter(|_| enter || del) {
        if let Some(j) = app.jobs.iter().find(|j| j.id == id) {
            if del {
                j.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            } else if let Ok(mut p) = j.progress.lock() {
                p.go = true;
            }
            app.queue.poke();
        }
    }
    let screen = ctx.content_rect();
    let width = 400.0f32.min(screen.width() - 16.0);
    let x = st.anchor.right().min(screen.right() - 6.0).max(screen.left() + width + 6.0);
    let fs = app.cfg.font_size;
    let small = FontId::proportional(fs - 1.5);
    let mut pause_flip = None;
    let mut cancel = None;
    let mut start_now = None;
    let mut moved: Option<(u64, Option<u64>)> = None;
    let room = (st.anchor.top() - screen.top() - 16.0).max(80.0);
    let r = egui::Area::new(Id::unique("transfers")).order(egui::Order::Foreground).pivot(Align2::RIGHT_BOTTOM).fixed_pos(pos2(x, st.anchor.top() - 6.0)).constrain_to(screen.shrink(6.0)).show(ctx, |ui| {
        crate::frost::behind(ui, 10);
        pp::popup_frame(&pal).inner_margin(egui::Margin::same(12)).show(ui, |ui| {
            ui.set_width(width - 24.0);
            egui::ScrollArea::vertical().max_height(room).auto_shrink([false, true]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                for (n, s) in running.iter().enumerate() {
                    if n > 0 {
                        ui.add_space(8.0);
                    }
                    let h = ui.text_style_height(&egui::TextStyle::Body).max(22.0);
                    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
                    // The buttons at the right, the title in what they leave, cut with "…".
                    let x_r = Rect::from_center_size(pos2(row.right() - 11.0, row.center().y), Vec2::splat(22.0));
                    let p_r = Rect::from_center_size(pos2(x_r.left() - 13.0, row.center().y), Vec2::splat(22.0));
                    if icon_button(ui, x_r, "x", "Cancel", "transfer cancel", &pal) {
                        cancel = Some(s.id);
                    }
                    if icon_button(ui, p_r, if s.paused { "play" } else { "pause" }, if s.paused { "Resume" } else { "Pause (Space)" }, "transfer pause", &pal) {
                        pause_flip = Some(s.id);
                    }
                    let title = crate::listing::elided(ui, &s.label, crate::theme::semibold(fs), p_r.left() - 8.0 - row.left(), pal.text);
                    ui.painter().galley(pos2(row.left(), row.center().y - title.size().y / 2.0), title, pal.text);
                    ui.add(egui::ProgressBar::new(s.frac).desired_height(5.0).fill(if s.paused { pal.text_dim } else { pal.accent }));
                    let (lr, _) = ui.allocate_exact_size(vec2(ui.available_width(), fs + 4.0), Sense::hover());
                    let sg = ui.painter().layout_no_wrap(s.speed.clone(), small.clone(), pal.text_dim);
                    let sw = sg.size().x;
                    ui.painter().galley(pos2(lr.right() - sw, lr.center().y - sg.size().y / 2.0), sg, pal.text_dim);
                    let lg = crate::listing::elided(ui, &s.line, small.clone(), lr.width() - sw - 10.0, pal.text_dim);
                    ui.painter().galley(pos2(lr.left(), lr.center().y - lg.size().y / 2.0), lg, pal.text_dim);
                    if !s.current.is_empty() && s.clash.is_none() {
                        ui.add(egui::Label::new(egui::RichText::new(&s.current).font(small.clone()).color(pal.text_dim)).truncate());
                    }
                    if let Some(c) = &s.clash {
                        ui.add_space(2.0);
                        if let Some(j) = app.jobs.iter().find(|j| j.id == s.id) {
                            clash_box(ui, j, c, s.same, &pal);
                        }
                    }
                }
                if !waiting.is_empty() {
                    if !running.is_empty() {
                        ui.add_space(8.0);
                        let y = ui.cursor().top();
                        ui.painter().line_segment([pos2(ui.min_rect().left(), y), pos2(ui.min_rect().right(), y)], Stroke::new(1.0, pal.border));
                        ui.add_space(6.0);
                    }
                    let row_h = (fs * 1.35 + 8.0).round();
                    let mut rects: Vec<(u64, Rect)> = vec![];
                    for s in &waiting {
                        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click_and_drag());
                        rects.push((s.id, r));
                        let on = st.sel == Some(s.id);
                        let dragged = st.drag == Some(s.id);
                        if on || dragged {
                            ui.painter().rect_filled(r, CornerRadius::same(6), pal.selected);
                        } else if resp.hovered() {
                            ui.painter().rect_filled(r, CornerRadius::same(6), pal.hover);
                        }
                        let (tc, dc) = if on || dragged { (pal.on_sel(), pal.on_sel_dim()) } else { (pal.text, pal.text_dim) };
                        grip(ui.painter(), pos2(r.left() + 10.0, r.center().y), dc);
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                        }
                        if resp.drag_started() {
                            st.drag = Some(s.id);
                            st.sel = Some(s.id);
                        }
                        if resp.clicked() {
                            st.sel = Some(s.id);
                        }
                        let x_r = Rect::from_center_size(pos2(r.right() - 13.0, r.center().y), Vec2::splat(22.0));
                        if icon_button(ui, x_r, "x", "Cancel", "transfer cancel", &pal) {
                            cancel = Some(s.id);
                        }
                        let g = ui.painter().layout_no_wrap("Start now".into(), small.clone(), dc);
                        let b = Rect::from_min_max(pos2(x_r.left() - 8.0 - g.size().x - 12.0, r.center().y - 10.0), pos2(x_r.left() - 6.0, r.center().y + 10.0));
                        let br = ui.interact(b, Id::unique(("start now", s.id)), Sense::click());
                        crate::ui::note_control(ui, b, "transfer start now");
                        if br.hovered() {
                            ui.painter().rect(b, CornerRadius::same(5), pal.hover, Stroke::new(1.0, pal.card_line()), StrokeKind::Inside);
                        }
                        ui.painter().galley(pos2(b.center().x - g.size().x / 2.0, b.center().y - g.size().y / 2.0), g, if br.hovered() { tc } else { dc });
                        if br.on_hover_text("Start without waiting its turn (Enter)").clicked() {
                            start_now = Some(s.id);
                        }
                        // The row's own part, where it is picked up: its buttons are theirs.
                        crate::ui::note_control(ui, Rect::from_min_max(r.min, pos2(b.left() - 4.0, r.max.y)), "transfer waiting");
                        let lg = crate::listing::elided(ui, &s.label, FontId::proportional(fs), b.left() - 8.0 - (r.left() + 24.0), tc);
                        ui.painter().galley(pos2(r.left() + 24.0, r.center().y - lg.size().y / 2.0), lg, tc);
                    }
                    // A drag: a line where it would go, and the move once let go.
                    if let Some(d) = st.drag {
                        let at = ctx.pointer_latest_pos().map(|p| p.y);
                        let before = at.and_then(|y| rects.iter().find(|(_, r)| y < r.center().y).map(|(id, _)| *id));
                        let line_y = match before {
                            Some(b) => rects.iter().find(|x| x.0 == b).map(|x| x.1.top()),
                            None => rects.last().map(|x| x.1.bottom()),
                        };
                        if let (Some(y), Some(first)) = (line_y, rects.first()) {
                            ui.painter().line_segment([pos2(first.1.left() + 4.0, y), pos2(first.1.right() - 4.0, y)], Stroke::new(2.0, pal.accent));
                        }
                        if ctx.input(|i| i.pointer.any_released()) {
                            st.drag = None;
                            if before != Some(d) {
                                moved = Some((d, before));
                            }
                        }
                    }
                    ui.add_space(4.0);
                    pp::hint_chips(ui, &[("Alt+↑↓", "reorder"), ("Enter", "start now"), ("Space", "pause"), ("Esc", "close")], &pal);
                }
            });
        });
    });
    crate::ui::note_popup(r.response.rect);
    if let Some((id, before)) = moved {
        app.queue.move_to(id, before);
    }
    for j in &app.jobs {
        if pause_flip == Some(j.id) {
            let now = j.pause.load(std::sync::atomic::Ordering::Relaxed);
            j.pause.store(!now, std::sync::atomic::Ordering::Relaxed);
        }
        if cancel == Some(j.id) {
            j.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            app.queue.poke();
        }
        if start_now == Some(j.id) {
            if let Ok(mut p) = j.progress.lock() {
                p.go = true;
            }
            app.queue.poke();
        }
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(250));
    let outside = pp::clicked_outside(ctx, r.response.rect) && !st.anchor.contains(ctx.pointer_interact_pos().unwrap_or(Pos2::ZERO));
    st.drag.is_some() || !outside
}

/// A small square button with an icon, lit while pointed at.
fn icon_button(ui: &mut Ui, r: Rect, icon: &str, tip: &str, what: &'static str, pal: &Palette) -> bool {
    let resp = ui.interact(r, Id::unique((what, r.min.x as i32, r.min.y as i32)), Sense::click());
    crate::ui::note_control(ui, r, what);
    if resp.hovered() {
        ui.painter().rect_filled(r, CornerRadius::same(5), pal.hover);
    }
    crate::theme::paint_symbol(ui.painter(), r.shrink(5.0), icon, if resp.hovered() { pal.text } else { pal.text_dim });
    resp.on_hover_text(tip).clicked()
}

/// Six dots: a row that can be picked up.
fn grip(p: &egui::Painter, c: Pos2, col: Color32) {
    for dx in [-2.0, 2.0] {
        for dy in [-4.0, 0.0, 4.0] {
            p.circle_filled(c + vec2(dx, dy), 1.1, col);
        }
    }
}

/// The chip in the status bar: the text, and a fill along its foot as far as the jobs
/// have got. Returns its response.
pub fn chip(ui: &mut Ui, text: &str, frac: f32, open: bool, font: FontId, pal: &Palette) -> Option<egui::Response> {
    let g = ui.painter().layout_no_wrap(text.to_string(), font, pal.text_dim);
    let w = g.size().x + 12.0;
    if w > ui.available_width() {
        return None;
    }
    let (r, resp) = ui.allocate_exact_size(vec2(w, 20.0), Sense::click());
    crate::ui::note_control(ui, r, "transfers chip");
    if resp.hovered() || open {
        ui.painter().rect_filled(r, CornerRadius::same(5), if open { pal.accent.gamma_multiply(0.2) } else { pal.hover });
    }
    ui.painter().galley(pos2(r.left() + 6.0, r.center().y - g.size().y / 2.0), g, pal.text_dim);
    let bar = Rect::from_min_max(pos2(r.left() + 5.0, r.bottom() - 4.0), pos2(r.right() - 5.0, r.bottom() - 2.5));
    ui.painter().rect_filled(bar, CornerRadius::same(1), pal.border);
    ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * frac, bar.height())), CornerRadius::same(1), pal.accent);
    Some(resp.on_hover_text("Copies and moves under way: pause them, put them in another order"))
}
