//! A list you can watch change: when the filter, the sort or the grouping changes what
//! is shown, the rows on screen glide from where they were to where they now are, and
//! the ones that newly appear fade in. Only the rows on screen are remembered (their
//! place last frame), so a list of any length costs the same; a folder still being read,
//! or another folder, glides nothing.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use egui::Vec2;

/// How long a row takes to reach its new place.
const TIME: f64 = 0.22;
/// Farther than this many screens, a row jumps instead (it came from far off screen).
const FAR: f32 = 1.5;

#[derive(Default)]
pub struct Glide {
    /// Where each row on screen was drawn last frame, in the list's own coordinates, by
    /// its path (an index means another file once the folder is read again).
    last: HashMap<u64, Vec2>,
    next: HashMap<u64, Vec2>,
    /// Rows on their way: how far from their place they started.
    moves: HashMap<u64, Vec2>,
    fresh: HashSet<u64>,
    t0: f64,
    path: PathBuf,
    /// The view drawing: another view's places are no start for a glide.
    view: u8,
    generation: u64,
    /// This frame may start glides (the order changed since last frame).
    starting: bool,
    /// The last frame was drawn while the folder was being read: what it showed is no
    /// start for a glide (often the folder before, still on screen).
    was_loading: bool,
    now: f64,
}

fn ease(t: f64) -> f32 {
    (1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)) as f32
}

impl Glide {
    /// Before the rows are drawn: the folder, the view (Details, icons…), the order's
    /// generation (`Tab::view_gen`), whether the folder is still coming in (or the view
    /// is otherwise moving on its own, a zoom easing), and whether motion is on.
    pub fn begin(&mut self, path: &std::path::Path, view: u8, generation: u64, loading: bool, animations: bool, now: f64) {
        self.now = now;
        let same = self.path == path && self.view == view;
        self.view = view;
        self.starting = animations && same && !loading && !self.was_loading && generation != self.generation && !self.last.is_empty();
        self.was_loading = loading;
        if !same {
            self.path = path.to_path_buf();
            self.moves.clear();
            self.fresh.clear();
        }
        self.generation = generation;
        if self.starting {
            self.t0 = now;
            self.fresh.clear();
        }
        self.last = std::mem::take(&mut self.next);
    }

    /// Each row on screen, at `at` (list coordinates) on a screen `height` tall: how far
    /// to draw it from there, and how opaque.
    pub fn row(&mut self, path: &std::path::Path, at: Vec2, height: f32) -> (Vec2, f32) {
        let idx = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            path.hash(&mut h);
            h.finish()
        };
        self.next.insert(idx, at);
        if self.starting {
            match self.last.get(&idx) {
                Some(&old) if (old - at).length() > 0.5 && (old - at).length() < height * FAR => {
                    // From where it is drawn now, which may be part way through a glide.
                    let cur = self.offset(idx);
                    self.moves.insert(idx, old + cur - at);
                }
                Some(_) => {
                    self.moves.remove(&idx);
                }
                None => {
                    self.fresh.insert(idx);
                }
            }
        }
        let k = ease((self.now - self.t0) / TIME);
        let alpha = if self.fresh.contains(&idx) { k } else { 1.0 };
        (self.offset(idx), alpha)
    }

    fn offset(&self, idx: u64) -> Vec2 {
        let k = ease((self.now - self.t0) / TIME);
        self.moves.get(&idx).map_or(Vec2::ZERO, |m| *m * (1.0 - k))
    }

    /// Rows on their way or fading in (for the simulations).
    #[cfg(test)]
    pub fn moving(&self) -> usize {
        if self.now - self.t0 < TIME { self.moves.len() + self.fresh.len() } else { 0 }
    }

    /// After the rows: keeps the frames coming while anything moves.
    pub fn end(&mut self, ctx: &egui::Context) {
        if self.now - self.t0 < TIME {
            if !self.moves.is_empty() || !self.fresh.is_empty() {
                ctx.request_repaint();
            }
        } else {
            self.moves.clear();
            self.fresh.clear();
        }
    }
}
