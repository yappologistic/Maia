//! A copy's speed over time, drawn in its job card under the figures: a stalled drive
//! or a slow run of small files shows as a dip at once. Samples are taken as the card is
//! drawn (twice a second at most), so nothing runs for it between frames.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use egui::{Color32, Pos2, Rect, Shape, Stroke, Ui, pos2};

use crate::theme::Palette;

/// Samples kept: about half a minute.
const KEEP: usize = 60;
const EVERY: f64 = 0.5;

struct Line {
    last: (f64, u64),
    samples: VecDeque<f32>,
}

thread_local! {
    static LINES: RefCell<HashMap<u64, Line>> = RefCell::new(HashMap::new());
}

/// Records job `id`'s bytes done at `now` and returns its speeds so far (bytes a second).
pub fn sample(id: u64, done: u64, now: f64) -> Vec<f32> {
    LINES.with(|l| {
        let mut l = l.borrow_mut();
        let line = l.entry(id).or_insert_with(|| Line { last: (now, done), samples: VecDeque::new() });
        let dt = now - line.last.0;
        if dt >= EVERY {
            let speed = done.saturating_sub(line.last.1) as f64 / dt;
            line.samples.push_back(speed as f32);
            if line.samples.len() > KEEP {
                line.samples.pop_front();
            }
            line.last = (now, done);
        }
        line.samples.iter().copied().collect()
    })
}

/// Forget the jobs no longer shown.
pub fn keep_only(ids: &[u64]) {
    LINES.with(|l| l.borrow_mut().retain(|id, _| ids.contains(id)));
}

/// The line in `r`: filled faintly under it, the newest at the right.
pub fn paint(ui: &Ui, r: Rect, samples: &[f32], pal: &Palette) {
    if samples.len() < 2 || !r.is_finite() {
        return;
    }
    let most = samples.iter().copied().fold(1.0f32, f32::max);
    let n = KEEP.max(samples.len());
    let step = r.width() / (n - 1) as f32;
    let x0 = r.right() - step * (samples.len() - 1) as f32;
    let pts: Vec<Pos2> = samples.iter().enumerate().map(|(i, s)| pos2(x0 + i as f32 * step, r.bottom() - (s / most).clamp(0.0, 1.0) * (r.height() - 2.0) - 1.0)).collect();
    // The fill: a strip of quads down to the base, clear at the bottom.
    let mut mesh = egui::Mesh::default();
    let top = pal.accent.gamma_multiply(0.28);
    for (i, p) in pts.iter().enumerate() {
        mesh.colored_vertex(*p, top);
        mesh.colored_vertex(pos2(p.x, r.bottom()), Color32::TRANSPARENT);
        if i > 0 {
            let k = (i * 2) as u32;
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k - 1, k + 1, k);
        }
    }
    ui.painter().add(mesh);
    ui.painter().add(Shape::line(pts, Stroke::new(1.4, pal.accent.gamma_multiply(0.9))));
}
