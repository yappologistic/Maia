//! What's New's ground: three soft lights in the accent's tones circling behind its card,
//! drawn as characters that grow denser where a light is brighter, and that scramble
//! under the pointer. The lights are worked out on a thread of their own, about two
//! dozen times a second and only while What's New is open; the window draws only the
//! cells the card leaves uncovered. Turning a page slides the lights along a little.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Color32, FontId, Rect, Ui, pos2, vec2};

/// Characters from faint to dense.
pub(crate) const RAMP: [&str; 10] = [" ", ".", ":", "-", "=", "+", "*", "#", "%", "@"];
/// Frames a second while it moves.
const FPS: u64 = 24;
/// How far from the pointer the characters scramble, in points.
const STIR: f32 = 46.0;

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Ask {
    pub cols: usize,
    pub rows: usize,
    /// Where the lights are slid to, a fraction of the width: one step a page.
    pub shift: f32,
    pub moving: bool,
}

/// A frame: each cell's place on the ramp, and how much of the second tone it takes.
#[derive(Default)]
pub(crate) struct Grid {
    pub cols: usize,
    pub rows: usize,
    pub level: Vec<u8>,
    pub warm: Vec<u8>,
}

#[derive(Default)]
struct Shared {
    ask: Option<Ask>,
    frame: Option<Grid>,
}

pub struct Glow {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    grid: Grid,
}

impl Drop for Glow {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
    }
}

impl Glow {
    pub fn new(ctx: &egui::Context) -> Glow {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (s2, st2, c2) = (shared.clone(), stop.clone(), ctx.clone());
        let _ = std::thread::Builder::new().name("glow".into()).spawn(move || worker(s2, st2, c2));
        Glow { shared, stop, grid: Grid::default() }
    }

    /// Draw it over `r`, leaving out every cell that reaches into one of `clear`; `page`
    /// of `pages` slides the lights. Still when `moving` is off.
    #[allow(clippy::too_many_arguments)]
    pub fn paint(&mut self, ui: &Ui, r: Rect, clear: &[Rect], page: usize, pages: usize, accent: Color32, bg: Color32, moving: bool) {
        let font = FontId::monospace(11.0);
        let painter = ui.painter();
        let glyphs: Vec<std::sync::Arc<egui::Galley>> = RAMP.iter().map(|c| painter.layout_no_wrap(c.to_string(), font.clone(), Color32::WHITE)).collect();
        let cell = vec2(glyphs[9].size().x.max(4.0), (glyphs[9].size().y * 0.86).max(6.0));
        let cols = (r.width() / cell.x).ceil().max(1.0) as usize;
        let rows = (r.height() / cell.y).ceil().max(1.0) as usize;
        let shift = if pages > 1 { (page as f32 / (pages - 1) as f32 - 0.5) * 0.3 } else { 0.0 };
        let ask = Ask { cols, rows, shift, moving };
        {
            let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            if s.ask != Some(ask) {
                s.ask = Some(ask);
            }
            if let Some(g) = s.frame.take() {
                self.grid = g;
            }
        }
        let g = &self.grid;
        if g.cols != cols || g.rows != rows {
            return;
        }
        // The second tone is the accent turned a little towards red.
        let warm = {
            let mut h = egui::ecolor::Hsva::from(accent);
            h.h = (h.h - 0.035).rem_euclid(1.0);
            Color32::from(h)
        };
        let pointer = ui.ctx().pointer_hover_pos().filter(|p| r.contains(*p) && !clear.iter().any(|c| c.contains(*p)));
        let tick = (ui.input(|i| i.time) * 18.0) as u32;
        for j in 0..rows {
            for i in 0..cols {
                let at = pos2(r.left() + i as f32 * cell.x, r.top() + j as f32 * cell.y);
                let c = Rect::from_min_size(at, cell);
                if clear.iter().any(|k| k.intersects(c)) {
                    continue;
                }
                let k = j * cols + i;
                let mut level = g.level[k] as usize;
                if let Some(p) = pointer {
                    let d = p.distance(c.center());
                    if d < STIR && (hash(i as u32, j as u32, tick) as f32 / 255.0) < 0.85 * (1.0 - d / STIR) {
                        level = 2 + hash(j as u32, i as u32, tick ^ 0x5bd1) as usize % 8;
                    }
                }
                if level == 0 {
                    continue;
                }
                let tone = accent.lerp_to_gamma(warm, g.warm[k] as f32 / 255.0);
                let color = bg.lerp_to_gamma(tone, 0.12 + 0.5 * level as f32 / 9.0);
                painter.galley_with_override_text_color(at, glyphs[level].clone(), color);
            }
        }
    }
}

fn worker(shared: Arc<Mutex<Shared>>, stop: Arc<AtomicBool>, ctx: egui::Context) {
    let t0 = Instant::now();
    let mut last = Instant::now();
    let mut done: Option<Ask> = None;
    let mut shift: Option<f32> = None;
    while !stop.load(Relaxed) {
        let tick = Instant::now();
        let ask = shared.lock().unwrap_or_else(|e| e.into_inner()).ask;
        let Some(a) = ask else {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        };
        // The slide to a new page's place eases in over about a second.
        let dt = last.elapsed().as_secs_f32();
        last = Instant::now();
        let s = shift.get_or_insert(a.shift);
        let settled = (*s - a.shift).abs() < 0.0005;
        *s = if a.moving { *s + (a.shift - *s) * (1.0 - (-dt * 3.0).exp()) } else { a.shift };
        // Still, once drawn it is drawn.
        if !a.moving && done == Some(a) && settled {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        let t = if a.moving { t0.elapsed().as_secs_f32() } else { 6.0 };
        let grid = frame(&a, t, *s);
        shared.lock().unwrap_or_else(|e| e.into_inner()).frame = Some(grid);
        done = Some(a);
        ctx.request_repaint();
        if let Some(rest) = Duration::from_millis(1000 / FPS).checked_sub(tick.elapsed()) {
            std::thread::sleep(rest);
        }
    }
}

pub(crate) fn frame(a: &Ask, t: f32, shift: f32) -> Grid {
    let (cols, rows) = (a.cols, a.rows);
    // Lengths in cell widths; a cell is about twice as tall as it is wide.
    let (w, h) = (cols as f32, rows as f32 * 1.9);
    let size = w.max(h);
    // Three lights on slow loops around the middle, wide enough to pass through the
    // margins the card leaves: two of the accent, one warmer.
    let at = |rx: f32, ry: f32, sx: f32, sy: f32, ph: f32| {
        (w * (0.5 + shift + rx * (t * sx + ph).sin()), h * (0.5 + ry * (t * sy + ph * 1.7).cos()))
    };
    let lights = [
        (at(0.44, 0.42, 0.071, 0.053, 0.0), size * 0.2, 0.9, false),
        (at(0.40, 0.46, 0.047, 0.067, 2.1), size * 0.17, 0.75, true),
        (at(0.46, 0.40, 0.059, 0.043, 4.2), size * 0.15, 0.6, false),
    ];
    let mut level = vec![0u8; cols * rows];
    let mut warm = vec![0u8; cols * rows];
    for j in 0..rows {
        let y = j as f32 * 1.9;
        for i in 0..cols {
            let x = i as f32;
            let (mut sum, mut hot) = (0.0f32, 0.0f32);
            for &((cx, cy), r, gain, is_warm) in &lights {
                let (dx, dy) = ((x - cx) / r, (y - cy) / r);
                let v = (-(dx * dx + dy * dy)).exp() * gain;
                sum += v;
                if is_warm {
                    hot += v;
                }
            }
            // A faint ripple so even ground has a little in it.
            let ripple = 0.05 * (x * 0.9 + t * 1.3).sin() * (y * 0.55 - t * 0.9).cos();
            let v = (1.0 - (-sum * 1.8).exp() + ripple).clamp(0.0, 1.0);
            let k = j * cols + i;
            level[k] = (v * 9.99) as u8;
            warm[k] = ((hot / (sum + 1e-6)).min(1.0) * 255.0) as u8;
        }
    }
    Grid { cols, rows, level, warm }
}

/// A byte from a cell and a moment, for the scramble under the pointer.
fn hash(x: u32, y: u32, t: u32) -> u8 {
    let mut h = x.wrapping_mul(0x8da6_b343) ^ y.wrapping_mul(0xd816_3841) ^ t.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 13;
    h = h.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 16;
    (h & 0xff) as u8
}
