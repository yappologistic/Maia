//! The date ribbon: a list sorted by a date has the years marked on its scrollbar, small
//! ticks where each starts. Pointing at the scrollbar names them; dragging it shows the
//! month passing beside the thumb. The marks are worked out once per order of the list
//! (plain arithmetic on the times, no calendar calls), never per frame.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use egui::{Color32, CornerRadius, FontId, Rect, Stroke, Ui, pos2, vec2};

use crate::config::SortKey;
use crate::fsx::Entry;
use crate::theme::Palette;

/// Width of the strip along the right edge the scrollbar takes.
const STRIP: f32 = 14.0;
/// Lists shorter than this many screens have no ribbon: there is little to travel.
const SCREENS: f32 = 2.5;

#[derive(Default)]
pub struct Ribbon {
    key: Option<(usize, SortKey, bool, u64, usize)>,
    /// Where each year starts, as a share of the list, and the year.
    pub(crate) marks: Vec<(f32, i32)>,
    worked: Option<Instant>,
    shown: f32,
}

fn dated(k: SortKey) -> bool {
    matches!(k, SortKey::Modified | SortKey::Created | SortKey::Accessed | SortKey::Taken)
}

fn time_of(e: &Entry, k: SortKey) -> Option<SystemTime> {
    match k {
        SortKey::Modified => e.modified,
        SortKey::Created => e.created,
        SortKey::Accessed => e.accessed,
        SortKey::Taken => crate::facts::get(e).and_then(|f| f.taken),
        _ => None,
    }
}

/// The year and month (1..12) of `t`, `off` seconds east of UTC.
pub(crate) fn year_month(t: SystemTime, off: i64) -> Option<(i32, u32)> {
    let s = t.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64 + off;
    let z = s.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (yoe + era * 400 + i64::from(m <= 2)) as i32;
    Some((y, m))
}

pub(crate) fn offset() -> i64 {
    crate::timefmt::local(SystemTime::now()).tm_gmtoff as i64
}

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// Each frame of a list after it is drawn: `n` rows top to bottom, `order` the item in
/// each (None for a group's header), `scroll` and `content` the scroll area's offset
/// and height. `order` is only walked when the list's order changed.
#[allow(clippy::too_many_arguments)]
pub fn paint(ui: &Ui, body: Rect, rb: &mut Ribbon, n: usize, order: &dyn Fn(usize) -> Option<u32>, entries: &[Entry], sort: SortKey, desc: bool, loading: bool, scroll: f32, content: f32, fs: f32, pal: &Palette, animations: bool) {
    if !dated(sort) || n < 2 || content < body.height() * SCREENS || !body.is_finite() {
        rb.shown = 0.0;
        return;
    }
    let fg = if sort == SortKey::Taken { crate::facts::generation() } else { 0 };
    let ends = order(0).unwrap_or(0) as usize ^ ((order(n - 1).unwrap_or(0) as usize) << 1);
    let key = (n, sort, desc, fg, ends);
    // At most a few times a second (dates taken land one by one), and twice a second
    // while a big folder streams in.
    let wait = if loading { 0.5 } else { 0.2 };
    let due = rb.worked.is_none_or(|w| w.elapsed().as_secs_f32() > wait);
    if rb.key != Some(key) && !due {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f32(wait));
    }
    if rb.key != Some(key) && due {
        rb.key = Some(key);
        rb.worked = Some(Instant::now());
        rb.marks.clear();
        let off = offset();
        let mut last = None;
        for i in 0..n {
            let Some(idx) = order(i) else { continue };
            let Some(y) = entries.get(idx as usize).and_then(|e| time_of(e, sort)).and_then(|t| year_month(t, off)).map(|x| x.0) else { continue };
            if last != Some(y) {
                last = Some(y);
                rb.marks.push((i as f32 / n as f32, y));
            }
        }
    }
    if rb.marks.len() < 2 {
        return;
    }
    let strip = Rect::from_min_max(pos2(body.right() - STRIP, body.top()), body.max);
    let (pointer, down, origin) = ui.input(|i| (i.pointer.hover_pos(), i.pointer.primary_down(), i.pointer.press_origin()));
    let over = pointer.is_some_and(|p| strip.expand2(vec2(6.0, 0.0)).contains(p));
    let dragging = down && origin.is_some_and(|o| strip.contains(o));
    let want = if over || dragging { 1.0 } else { 0.0 };
    let dt = ui.input(|i| i.stable_dt).min(0.05);
    rb.shown = if animations { if want > rb.shown { (rb.shown + dt / 0.15).min(1.0) } else { (rb.shown - dt / 0.3).max(0.0) } } else { want };
    if rb.shown != want {
        ui.ctx().request_repaint();
    }
    let p = ui.painter().with_clip_rect(body.intersect(ui.clip_rect()));
    let track = body.shrink2(vec2(0.0, 4.0));
    let small = FontId::proportional((fs - 3.0).max(8.0));
    let mut last_label = f32::NEG_INFINITY;
    for &(f, year) in &rb.marks {
        let y = track.top() + f * track.height();
        // A tick across the scrollbar's track, always.
        p.line_segment([pos2(strip.left() + 3.0, y), pos2(strip.right() - 3.0, y)], Stroke::new(1.0, pal.text_dim.gamma_multiply(0.45)));
        if rb.shown > 0.0 && y - last_label >= small.size * 1.4 + 6.0 && y < track.bottom() - small.size * 0.5 {
            last_label = y;
            let g = p.layout_no_wrap(year.to_string(), small.clone(), pal.text_dim);
            let r = Rect::from_min_size(pos2(strip.left() - g.size().x - 10.0, y - g.size().y / 2.0), g.size() + vec2(6.0, 0.0));
            // Wide enough that a row's text beside it does not show through at the ends.
            p.rect_filled(r.expand2(vec2(3.0, 2.0)), CornerRadius::same(4), pal.panel.gamma_multiply(0.96 * rb.shown));
            p.galley(r.min + vec2(3.0, 0.0), g, pal.text_dim.gamma_multiply(rb.shown));
        }
    }
    // Dragging: the month at the middle of the screen, beside the thumb.
    if dragging {
        let span = (content - body.height()).max(1.0);
        let at = ((scroll + body.height() / 2.0) / content * n as f32) as usize;
        let thumb_h = (track.height() * body.height() / content).max(18.0);
        let ty = track.top() + (scroll / span).clamp(0.0, 1.0) * (track.height() - thumb_h) + thumb_h / 2.0;
        let near = (0..12).flat_map(|d| [at.saturating_sub(d), (at + d).min(n - 1)]).find_map(order);
        let label = near.and_then(|i| entries.get(i as usize)).and_then(|e| time_of(e, sort)).and_then(|t| year_month(t, offset())).map(|(y, m)| format!("{} {y}", MONTHS[(m as usize).clamp(1, 12) - 1]));
        if let Some(l) = label {
            let font = crate::theme::semibold(fs - 0.5);
            let g = p.layout_no_wrap(l, font, pal.text);
            let r = Rect::from_min_size(pos2(strip.left() - g.size().x - 30.0, ty - g.size().y / 2.0 - 5.0), g.size() + vec2(18.0, 10.0));
            p.rect_filled(r.translate(vec2(0.0, 2.0)), CornerRadius::same(7), Color32::from_black_alpha(50));
            p.rect_filled(r, CornerRadius::same(7), pal.raised);
            p.rect_stroke(r, CornerRadius::same(7), Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
            p.galley(r.min + vec2(9.0, 5.0), g, pal.text);
            // A short line from the bubble to the thumb.
            p.line_segment([pos2(r.right(), ty), pos2(strip.left() - 2.0, ty)], Stroke::new(1.0, pal.border));
        }
    }
}
