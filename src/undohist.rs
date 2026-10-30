//! Undo History: the changes Undo can take back, newest first, under those Redo can do
//! again. Taking one back takes back everything after it first, in order, the way
//! pressing Undo that many times would: a change undone out of order could put a file
//! back where a later change expects it gone. Doing one again does the ones before it
//! first, for the same reason.

use std::time::SystemTime;

use egui::{CornerRadius, FontId, Rect, Sense, Shape, pos2, vec2};

use crate::app::{App, UndoKind, UndoStack};
use crate::popups as pp;

pub struct State {
    /// The row chosen, from the top: what Redo would do last, down to what it would do
    /// next, then the newest change down to the oldest.
    pub sel: usize,
    /// The chosen row not yet scrolled to (it opens on the newest change).
    pub reveal: bool,
}

impl State {
    pub fn new(u: &UndoStack) -> State {
        let r = u.redos().len();
        State { sel: if u.is_empty() { r.saturating_sub(1) } else { r }, reveal: true }
    }
}

/// A few words for what a change was, under its own label.
fn what(k: &UndoKind) -> &'static str {
    match k {
        UndoKind::Rename(_) => "Renamed",
        UndoKind::Move(_) => "Moved",
        UndoKind::Created(_) => "Made",
        UndoKind::Trashed(_) => "Moved to the trash",
        UndoKind::Replaced(_) => "Changed in place",
        UndoKind::Sorted(..) => "Sorted into folders",
        UndoKind::Copied(..) => "Copied, replacing",
        UndoKind::MovedOver(..) => "Moved, replacing",
        UndoKind::Synced(..) => "Synced",
        UndoKind::Tagged(..) => "Tags changed",
    }
}

/// When a change was made, as the listing says it, the day left out for today's: every
/// row would say "Today" otherwise.
fn clock(t: SystemTime, pattern: &str) -> String {
    let now = SystemTime::now();
    if pattern.is_empty() && crate::timefmt::local_day(t) == crate::timefmt::local_day(now) {
        let tm = crate::timefmt::local(t);
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    } else {
        crate::timefmt::date(t, pattern)
    }
}

/// The band over what a press of the button takes back or does again: `kit::lift`, with
/// the accent's bar down its left edge.
fn band_shape(r: Rect, pal: &crate::theme::Palette) -> Shape {
    if r.height() < 1.0 {
        return Shape::Noop;
    }
    let mut v = vec![Shape::rect_filled(r, CornerRadius::same(6), crate::kit::lift(pal))];
    if !pal.strong {
        v.push(Shape::rect_filled(Rect::from_min_max(pos2(r.left() + 2.0, r.top() + 8.0), pos2(r.left() + 5.0, (r.bottom() - 8.0).max(r.top() + 9.0))), CornerRadius::same(2), pal.accent));
    }
    Shape::Vec(v)
}

pub fn popup(app: &mut App, ctx: &egui::Context, st: &mut State) -> bool {
    let pal = app.pal;
    let k = pp::take_keys(ctx);
    if k.esc {
        return false;
    }
    let n = app.undo.len();
    // Rows above the newest change: those Redo can do again.
    let rn = app.undo.redos().len();
    let rows = n + rn;
    pp::step(&mut st.sel, rows, &k, |_| false);
    let redoing = st.sel < rn;
    let fs = app.cfg.font_size;
    let small = fs - 1.5;
    let mut take: Option<usize> = (k.enter && rows > 0).then_some(st.sel);
    let mut close = false;
    let list_max = pp::fit_list(ctx, 420.0, 170.0);
    let (_, rect) = pp::centered(ctx, "undo history", 600.0, &pal, |ui| {
        pp::dialog_title(ui, "Undo history", Some("Taking a change back takes back the ones after it first."), &pal);
        ui.add_space(8.0);
        if rows == 0 {
            ui.add_space(6.0);
            ui.label(egui::RichText::new("Nothing to undo yet").color(pal.text_dim));
            ui.add_space(6.0);
        } else {
            let row_h = (fs * 1.35 + small * 1.35 + 14.0).round();
            let mut area = egui::ScrollArea::vertical().id_salt("undo history list").max_height(list_max).min_scrolled_height((rows as f32 * row_h).min(list_max)).auto_shrink([false, true]);
            if k.up || k.down || k.page_up || k.page_down || st.reveal {
                st.reveal = false;
                let most = (rows as f32 * row_h - list_max).max(0.0);
                area = area.vertical_scroll_offset((st.sel as f32 * row_h - list_max / 2.0 + row_h / 2.0).clamp(0.0, most));
            }
            area.show_rows(ui, row_h, rows, |ui, range| {
                // Everything from now to the chosen row goes with it: one band, drawn under
                // the rows, that grows and shrinks to the row picked.
                let band = ui.painter().add(egui::Shape::Noop);
                let first = range.start;
                let (lo, hi) = if redoing { (st.sel, rn.saturating_sub(1)) } else { (rn, st.sel) };
                let (left, width) = (ui.cursor().left(), ui.available_width());
                // Where the rows land, measured as they are laid out: the band reaches
                // rows scrolled out of sight by the same step.
                let mut tops: Vec<f32> = vec![];
                for j in range {
                    // Row j < rn is Redo's, rn - j steps away; row j >= rn the change
                    // j - rn steps back from the latest.
                    let (e, when, undone) = match j.checked_sub(rn) {
                        Some(back) => (&app.undo[n - 1 - back], app.undo.when(n - 1 - back), false),
                        None => match app.undo.redos().nth(j) {
                            Some(e) => (e, app.undo.undone_when(j), true),
                            None => continue,
                        },
                    };
                    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                    tops.push(r.top());
                    let going = j >= lo && j <= hi;
                    if !going && resp.hovered() {
                        ui.painter().rect_filled(r, CornerRadius::same(6), crate::kit::touch(&pal));
                    }
                    // Now: between what can be done again and what can be taken back.
                    if j == rn && rn > 0 {
                        ui.painter().hline(r.x_range().shrink(6.0), r.top(), egui::Stroke::new(1.0, pal.border));
                    }
                    let loud = going && pal.strong;
                    let (tc, dc) = if loud { (pal.on_sel(), pal.on_sel_dim()) } else if undone && !going { (pal.text_dim, pal.text_dim) } else { (pal.text, pal.text_dim) };
                    let when = when.map(|t| clock(t, &app.cfg.date_format)).unwrap_or_default();
                    let wg = crate::listing::tabular(ui, &when, FontId::proportional(small), 120.0, dc);
                    let ww = wg.size().x;
                    ui.painter().galley(pos2(r.right() - 12.0 - ww, r.top() + 7.0 + (fs * 1.35 - wg.size().y) / 2.0), wg, dc);
                    let x = r.left() + 12.0;
                    let room = r.width() - 36.0 - ww;
                    let g = crate::listing::elided(ui, &e.label, FontId::proportional(fs), room, tc);
                    ui.painter().galley(pos2(x, r.top() + 6.0), g, tc);
                    let c = e.kind.count();
                    let sub = format!("{}{} · {c} {}", if undone { "Undone · " } else { "" }, what(&e.kind), if c == 1 { "item" } else { "items" });
                    let g = crate::listing::elided(ui, &sub, FontId::proportional(small), room, dc);
                    ui.painter().galley(pos2(x, r.top() + 8.0 + fs * 1.35), g, dc);
                    crate::ui::note_control(ui, r.shrink(1.0), "undo row");
                    if resp.clicked() {
                        st.sel = j;
                    }
                    if resp.double_clicked() {
                        take = Some(j);
                    }
                }
                if let Some(&t0) = tops.first() {
                    let pitch = if tops.len() > 1 { tops[1] - t0 } else { row_h + ui.spacing().item_spacing.y };
                    let origin = t0 - first as f32 * pitch;
                    let top = crate::kit::slide(ui.ctx(), egui::Id::new("undo band top"), lo as f32 * pitch);
                    let bottom = crate::kit::slide(ui.ctx(), egui::Id::new("undo band bottom"), hi as f32 * pitch + row_h);
                    let band_r = Rect::from_min_max(pos2(left, origin + top), pos2(left + width, origin + bottom));
                    ui.painter().set(band, band_shape(band_r, &pal));
                }
            });
        }
        pp::footer(ui, &[("↑↓", "choose"), ("Enter", if redoing { "do again" } else { "take back" }), ("Esc", "close")], &pal, |ui| {
            let (verb, steps) = if redoing { ("Redo", rn - st.sel) } else { ("Undo", st.sel.saturating_sub(rn) + 1) };
            let label = if steps > 1 && rows > 0 { format!("{verb} {steps} changes") } else { verb.to_string() };
            let b = pp::primary_button(ui, &label, rows > 0, &pal);
            crate::ui::note_control(ui, b.rect, "undo back");
            if b.clicked() {
                take = Some(st.sel);
            }
            let c = pp::plain_button(ui, "Close", &pal);
            crate::ui::note_control(ui, c.rect, "undo close");
            close = c.clicked();
        });
    });
    if let Some(j) = take {
        let redo = j < rn;
        let steps = if redo { rn - j } else { (j - rn + 1).min(n) };
        let (mut whole, mut done) = (true, 0);
        for _ in 0..steps {
            // A move across disks runs as a job: the rest waits for it, in order.
            if !app.job_steps.is_empty() {
                break;
            }
            whole &= if redo { app.redo_last() } else { app.undo_last() };
            done += 1;
        }
        let verb = if redo { "Redid" } else { "Undid" };
        if done < steps {
            app.toast(format!("{verb} {done} of {steps} changes: the next is a move under way"));
        } else if steps > 1 {
            app.toast(if whole { format!("{verb} {steps} changes") } else { format!("{verb} {steps} changes; some files couldn't be moved") });
        }
        return false;
    }
    !(close || pp::clicked_outside(ctx, rect))
}
