//! Transfers: the status bar's chip while jobs run ("3 jobs · 64%") and the list it opens
//! above itself. Jobs that move data onto one disk take turns (ops::Queue); here the
//! waiting ones are put in another order, by dragging or with Alt and the arrows, the
//! running ones paused, and a name already taken is answered inside its job.

use egui::{Align2, Color32, CornerRadius, FontId, Id, Key, Modifiers, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

use crate::app::App;
use crate::ops::{Clash, Conflict, Job};
use crate::popups as pp;
use crate::theme::Palette;

/// Where the card's text begins, inside its rows and outside them: the job's name, what
/// it is doing, the queue's heading and the names waiting all start on one line.
const TEXT_X: f32 = 12.0;

pub struct State {
    /// The chip it opens above.
    pub anchor: Rect,
    /// The waiting job the keys move, by id.
    sel: Option<u64>,
    /// The waiting job being dragged to another place.
    drag: Option<u64>,
    /// The waiting ones are folded away under their heading.
    fold: bool,
}

impl State {
    pub fn new(anchor: Rect) -> State {
        State { anchor, sel: None, drag: None, fold: false }
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
    /// How much is done, in the job's own row: "15.6 of 45.8 MB", else "3 of 12 items".
    amount: String,
    /// Under it, on the left: when it will be done and how fast it goes ("2 min left ·
    /// 38 MB/s"), or why it isn't going ("paused").
    pace: String,
    /// And on the right: "3 of 12 files", when the amount was in bytes.
    count: String,
    /// What a waiting one will move, when known.
    size: String,
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
    let files = if p.total_files > 1 {
        format!("{} of {} files", crate::table::fmt_count(p.done_files), crate::table::fmt_count(p.total_files))
    } else if p.total_items > 1 {
        format!("{} of {} items", p.done_items, p.total_items)
    } else {
        String::new()
    };
    // Bytes where they are counted, the files beneath; else the items in the row.
    let (amount, count) = if p.total_bytes > 0 { (of_size(p.done_bytes, p.total_bytes), files) } else { (files, String::new()) };
    let pace = if paused {
        "paused".into()
    } else if p.clash.is_some() {
        "waiting for you".into()
    } else if p.total_bytes > 0 && p.done_bytes > 0 {
        let s = p.done_bytes as f64 / el;
        format!("{} left · {}/s", crate::ui::time_left(p.total_bytes.saturating_sub(p.done_bytes) as f64 / s), crate::fsx::fmt_size(s as u64))
    } else if p.within.is_some() {
        format!("{:.0}%", frac * 100.0)
    } else {
        String::new()
    };
    let size = if p.total_bytes > 0 { crate::fsx::fmt_size(p.total_bytes) } else { String::new() };
    Some(Snap { id: j.id, label: j.label.clone(), frac, amount, pace, count, size, current: p.current.clone(), paused, waiting: p.waiting.is_some(), clash: p.clash.clone(), same: p.same })
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
            let b = crate::kit::small_main_button(ui, "Keep both", pal);
            crate::ui::note_control(ui, b.rect, "clash keep both");
            if b.clicked() {
                answer = Some(Conflict::KeepBoth);
            }
            let r = crate::kit::small_button(ui, "Replace", pal).on_hover_text("The one there goes to the trash; Undo brings it back");
            crate::ui::note_control(ui, r.rect, "clash replace");
            if r.clicked() {
                answer = Some(Conflict::Overwrite);
            }
            if clash.dirs {
                let m = crate::kit::small_button(ui, "Merge", pal).on_hover_text("Copy into the folder there, replacing files");
                crate::ui::note_control(ui, m.rect, "clash merge");
                if m.clicked() {
                    answer = Some(Conflict::Merge);
                }
            }
            let s = crate::kit::small_button(ui, "Skip", pal);
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
    // The keys walk the queue: folded, it opens for them.
    if up || down || alt_up || alt_down {
        st.fold = false;
    }
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
            // The key hints stay under the list, outside its scrolling: inside, the last
            // row of them was cut in half at larger Spacing.
            let hints = !waiting.is_empty();
            let hint_h = if hints { 30.0 * app.cfg.spacing.max(1.0) + 8.0 } else { 0.0 };
            egui::ScrollArea::vertical().max_height((room - hint_h).max(60.0)).auto_shrink([false, true]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                for (n, s) in running.iter().enumerate() {
                    if n > 0 {
                        ui.add_space(10.0);
                    }
                    // The job is a row that fills as it goes, its name on the left and how
                    // much is done on the right; when it will be done goes underneath.
                    let h = (ui.text_style_height(&egui::TextStyle::Body) + 14.0).max(30.0).round();
                    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
                    // Pause and cancel show while the job is pointed at; a paused one keeps
                    // its play button, the way back.
                    let block = Rect::from_min_max(row.min, pos2(row.right(), row.bottom() + 22.0));
                    let near = ui.rect_contains_pointer(block);
                    let p = ui.painter();
                    p.rect(row, CornerRadius::same(7), pal.card().lerp_to_gamma(pal.text, if pal.dark { 0.05 } else { 0.045 }), Stroke::new(1.0, pal.card_line()), StrokeKind::Inside);
                    // Eased between the worker's reports, a quarter second apart.
                    let shown = crate::kit::slide(ui.ctx(), Id::new(("transfer fill", s.id)), s.frac.clamp(0.0, 1.0));
                    let tone = if s.paused { pal.text_dim } else { pal.accent };
                    if shown > 0.002 {
                        let fx = row.left() + 1.0 + shown * (row.width() - 2.0);
                        let fill = Rect::from_min_max(row.min, pos2(fx, row.bottom())).shrink(1.0);
                        ui.painter().rect_filled(fill, CornerRadius::same(6), tone.gamma_multiply(if pal.dark { 0.28 } else { 0.2 }));
                        if shown < 0.998 {
                            ui.painter().line_segment([pos2(fx - 1.0, row.top() + 1.0), pos2(fx - 1.0, row.bottom() - 1.0)], Stroke::new(2.0, tone));
                        }
                    }
                    let mut right = row.right() - 10.0;
                    if near || s.paused {
                        let x_r = Rect::from_center_size(pos2(row.right() - 4.0 - 11.0, row.center().y), Vec2::splat(22.0));
                        // Alone (paused, not pointed at), the play button takes the end.
                        let p_r = if near { Rect::from_center_size(pos2(x_r.left() - 13.0, row.center().y), Vec2::splat(22.0)) } else { x_r };
                        if near && icon_button(ui, x_r, "x", "Cancel", "transfer cancel", &pal) {
                            cancel = Some(s.id);
                        }
                        if icon_button(ui, p_r, if s.paused { "play" } else { "pause" }, if s.paused { "Resume (Space)" } else { "Pause (Space)" }, "transfer pause", &pal) {
                            pause_flip = Some(s.id);
                        }
                        right = p_r.left() - 6.0;
                    }
                    let mut aw = 0.0;
                    if !s.amount.is_empty() {
                        let ag = crate::listing::tabular(ui, &s.amount, small.clone(), (right - row.left()) * 0.5, pal.text_dim);
                        aw = ag.size().x + 12.0;
                        ui.painter().galley(pos2(right - ag.size().x, (row.center().y - ag.size().y / 2.0).round()), ag, pal.text_dim);
                    }
                    let title = crate::listing::elided(ui, &s.label, crate::theme::semibold(fs), (right - aw - row.left() - TEXT_X).max(20.0), pal.text);
                    ui.painter().galley(pos2(row.left() + TEXT_X, (row.center().y - title.size().y / 2.0).round()), title, pal.text);
                    if !s.pace.is_empty() || !s.count.is_empty() {
                        ui.add_space(2.0);
                        let lh = ui.painter().layout_no_wrap("0".into(), small.clone(), pal.text_dim).size().y;
                        let (lr, _) = ui.allocate_exact_size(vec2(ui.available_width(), lh), Sense::hover());
                        let mut cw = 0.0;
                        if !s.count.is_empty() {
                            let cg = crate::listing::tabular(ui, &s.count, small.clone(), lr.width() * 0.5, pal.text_dim);
                            cw = cg.size().x + 12.0;
                            ui.painter().galley(pos2(lr.right() - 10.0 - cg.size().x, lr.top()), cg, pal.text_dim);
                        }
                        let pg = crate::listing::tabular(ui, &s.pace, small.clone(), (lr.width() - cw - 22.0).max(20.0), pal.text_dim);
                        ui.painter().galley(pos2(lr.left() + TEXT_X, lr.top()), pg, pal.text_dim);
                    }
                    // The file at hand, when the title does not already name it.
                    if !s.current.is_empty() && s.clash.is_none() && !s.label.contains(s.current.as_str()) {
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
                    // A dashed rule: the queue is part of the same work, not another list.
                    if !running.is_empty() {
                        ui.add_space(8.0);
                        let y = ui.cursor().top().round() + 0.5;
                        let (l, r) = (ui.min_rect().left(), ui.min_rect().left() + ui.available_width());
                        crate::kit::dashed_rule(ui.painter(), pos2(l, y), pos2(r, y), &pal);
                        ui.add_space(8.0);
                    }
                    // Its heading folds it away, and says how many wait.
                    let lf = crate::theme::semibold((fs * 0.8).round());
                    let (lr, lresp) = ui.allocate_exact_size(vec2(ui.available_width(), lf.size + 8.0), Sense::click());
                    crate::ui::note_control(ui, lr, "transfer waiting heading");
                    let hc = if lresp.hovered() { pal.text } else { pal.text_dim };
                    ui.painter().text(pos2(lr.left() + TEXT_X, lr.center().y), Align2::LEFT_CENTER, format!("Waiting · {}", waiting.len()), lf, hc);
                    let turn = crate::kit::slide(ui.ctx(), Id::new("transfers fold turn"), if st.fold { 0.0 } else { 1.0 });
                    crate::kit::chevron(ui.painter(), pos2(lr.right() - 14.0, lr.center().y), turn, hc);
                    if lresp.clicked() {
                        st.fold = !st.fold;
                    }
                    let row_h = (fs * 1.35 + 8.0).round();
                    let mut rects: Vec<(u64, Rect)> = vec![];
                    for s in waiting.iter().filter(|_| !st.fold) {
                        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click_and_drag());
                        rects.push((s.id, r));
                        let on = st.sel == Some(s.id);
                        let dragged = st.drag == Some(s.id);
                        // A plain list: the handle and the row's buttons only on the row
                        // pointed at or under the keyboard's cursor.
                        let hot = ui.rect_contains_pointer(r) && st.drag.is_none();
                        let shown = on || dragged || hot;
                        if shown {
                            ui.painter().rect_filled(r, CornerRadius::same(6), pal.hover);
                        }
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
                        let mut text_right = r.right() - 8.0;
                        if shown {
                            grip(ui.painter(), pos2(r.left() + 5.0, r.center().y), pal.text_dim);
                            let x_r = Rect::from_center_size(pos2(r.right() - 13.0, r.center().y), Vec2::splat(22.0));
                            if icon_button(ui, x_r, "x", "Cancel", "transfer cancel", &pal) {
                                cancel = Some(s.id);
                            }
                            let g = ui.painter().layout_no_wrap("Start now".into(), small.clone(), pal.text_dim);
                            let b = Rect::from_min_max(pos2(x_r.left() - 8.0 - g.size().x - 12.0, r.center().y - 10.0), pos2(x_r.left() - 6.0, r.center().y + 10.0));
                            let br = ui.interact(b, Id::unique(("start now", s.id)), Sense::click());
                            crate::ui::note_control(ui, b, "transfer start now");
                            if br.hovered() {
                                ui.painter().rect(b, CornerRadius::same(5), pal.raised, Stroke::new(1.0, pal.card_line()), StrokeKind::Inside);
                            }
                            ui.painter().galley(pos2(b.center().x - g.size().x / 2.0, b.center().y - g.size().y / 2.0), g, if br.hovered() { pal.text } else { pal.text_dim });
                            if br.on_hover_text("Start without waiting its turn (Enter)").clicked() {
                                start_now = Some(s.id);
                            }
                            text_right = b.left() - 8.0;
                        }
                        // The row's own part, where it is picked up: its buttons are theirs.
                        crate::ui::note_control(ui, Rect::from_min_max(r.min, pos2(text_right + 4.0, r.max.y)), "transfer waiting");
                        // What it will move, while its buttons don't need the room.
                        if !shown && !s.size.is_empty() {
                            let sg = crate::listing::tabular(ui, &s.size, small.clone(), r.width() * 0.3, pal.text_dim);
                            let sw = sg.size().x;
                            ui.painter().galley(pos2(r.right() - 8.0 - sw, (r.center().y - sg.size().y / 2.0).round()), sg, pal.text_dim);
                            text_right = r.right() - 8.0 - sw - 12.0;
                        }
                        let lg = crate::listing::elided(ui, &s.label, FontId::proportional(fs), text_right - (r.left() + TEXT_X), pal.text);
                        ui.painter().galley(pos2(r.left() + TEXT_X, (r.center().y - lg.size().y / 2.0).round()), lg, pal.text);
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
                }
            });
            if hints {
                ui.add_space(8.0);
                pp::hint_chips(ui, &[("Alt+↑↓", "reorder"), ("Enter", "start now"), ("Space", "pause"), ("Esc", "close")], &pal);
            }
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

/// The chip in the status bar: a pill that fills from the left as far as the jobs have
/// got, its figures over the fill; lifted while the list of them is open. Returns its
/// response.
pub fn chip(ui: &mut Ui, text: &str, frac: f32, open: bool, font: FontId, round: bool, pal: &Palette) -> Option<egui::Response> {
    let g = crate::listing::figures(ui, text, font, 400.0, pal.text, crate::kit::words_of(pal.text_dim));
    let w = g.size().x + 20.0;
    if w > ui.available_width() {
        return None;
    }
    let (r, resp) = ui.allocate_exact_size(vec2(w, crate::kit::CHIP_H), Sense::click());
    crate::ui::note_control(ui, r, "transfers chip");
    crate::kit::chip_face(ui.painter(), r, resp.hovered(), open, Some(frac), round, pal);
    if open {
        ui.painter().galley_with_override_text_color(pos2(r.left() + 10.0, r.center().y - g.size().y / 2.0), g, crate::kit::on_lift(pal));
    } else {
        ui.painter().galley(pos2(r.left() + 10.0, r.center().y - g.size().y / 2.0), g, pal.text);
    }
    Some(resp.on_hover_text("Copies and moves under way: pause them, put them in another order"))
}
