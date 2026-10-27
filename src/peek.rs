//! Peek into a folder: in the icon views, resting the pointer on a folder fans its first
//! few pictures out above it like a hand of cards, and they fold back when it moves on.
//! The folder is read on a thread, once a session, only the start of it; the pictures
//! come through the thumbnail cache like any other.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use egui::{Color32, Id, LayerId, Order, Pos2, Rect, emath::Rot2, pos2, vec2};

/// The pointer rests this long before the cards come out.
const DELAY: f64 = 0.45;
/// Coming out, or folding back.
const MOVE: f32 = 0.22;
/// Cards in the fan.
const CARDS: usize = 3;
/// Entries looked at in a folder, at most: a huge folder is not read through for this.
const LOOK: usize = 600;

type Pics = Arc<Vec<(PathBuf, Option<SystemTime>)>>;

#[derive(Default)]
struct Known {
    map: HashMap<PathBuf, Option<Pics>>,
}

static KNOWN: Mutex<Option<Known>> = Mutex::new(None);

/// The first pictures in `dir` by name, None while it is read.
fn pictures(dir: &Path, ctx: &egui::Context) -> Option<Pics> {
    let mut g = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    let k = g.get_or_insert_with(Known::default);
    if let Some(v) = k.map.get(dir) {
        return v.clone();
    }
    k.map.insert(dir.to_path_buf(), None);
    let (d, c) = (dir.to_path_buf(), ctx.clone());
    std::thread::spawn(move || {
        let pics = first_pictures(&d);
        if let Some(k) = KNOWN.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            k.map.insert(d, Some(Arc::new(pics)));
        }
        c.request_repaint();
    });
    None
}

/// The first pictures in `d` by name (looking at the first `LOOK` entries), with their
/// times changed: what the cards show.
pub(crate) fn first_pictures(d: &Path) -> Vec<(PathBuf, Option<SystemTime>)> {
    let mut found: Vec<(String, PathBuf)> = vec![];
    if let Ok(rd) = std::fs::read_dir(d) {
        for e in rd.flatten().take(LOOK) {
            let name = e.file_name().to_string_lossy().into_owned();
            let ext = Path::new(&name).extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
            if !name.starts_with('.') && crate::thumbs::is_image_ext(&ext) && e.file_type().is_ok_and(|t| t.is_file()) {
                found.push((name.to_lowercase(), e.path()));
            }
        }
    }
    found.sort_by(|a, b| crate::tab::natural_cmp(&a.0, &b.0));
    found.into_iter().take(CARDS).map(|(_, p)| {
        let m = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
        (p, m)
    }).collect()
}

/// Forget what was read of `dir` (it changed).
pub fn invalidate(dir: &Path) {
    if let Some(k) = KNOWN.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        k.map.remove(dir);
    }
}

/// A tab's fan: the folder under the pointer and since when, and the one shown (it
/// stays while it folds back).
#[derive(Default)]
pub struct Peek {
    at: Option<(PathBuf, Rect)>,
    since: f64,
    shown: Option<(PathBuf, Rect)>,
    open: f32,
    /// What the cards covered last frame (for the simulations).
    #[cfg(test)]
    fan: Option<(Rect, Rect)>,
}

impl Peek {
    /// The folder whose cards are out, and how far (for the simulations).
    #[cfg(test)]
    pub fn showing(&self) -> Option<(&Path, f32)> {
        self.shown.as_ref().map(|(p, _)| (p.as_path(), self.open))
    }

    /// The cards' rect and the view's.
    #[cfg(test)]
    pub fn fan(&self) -> Option<(Rect, Rect)> {
        self.fan
    }

    /// Each frame of an icon view: the folder icon under the pointer, if any.
    /// `area` is the view's own: the fan stays inside it, never over the toolbar, the
    /// sidebar or the other panel.
    pub fn frame(&mut self, ctx: &egui::Context, hovered: Option<(PathBuf, Rect)>, area: Rect, thumbs: &mut crate::thumbs::Thumbs, animations: bool, tab_id: u64) {
        let now = ctx.input(|i| i.time);
        // A drag or a held button is no time to show anything.
        let busy = ctx.input(|i| i.pointer.any_down()) || ctx.dragged_id().is_some();
        let hovered = hovered.filter(|_| !busy);
        if hovered.as_ref().map(|h| &h.0) != self.at.as_ref().map(|a| &a.0) {
            self.since = now;
        }
        self.at = hovered;
        let pics = self.at.as_ref().and_then(|(p, _)| pictures(p, ctx)).filter(|v| !v.is_empty());
        let rested = now - self.since >= DELAY;
        let want = self.at.is_some() && rested && pics.is_some();
        if want {
            self.shown = self.at.clone();
        } else if self.at.is_some() && !rested {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(DELAY - (now - self.since) + 0.01));
        }
        let dt = ctx.input(|i| i.stable_dt).min(0.05);
        let target = if want { 1.0 } else { 0.0 };
        self.open = if !animations { target } else if target > self.open { (self.open + dt / MOVE).min(1.0) } else { (self.open - dt / (MOVE * 0.7)).max(0.0) };
        if self.open != target {
            ctx.request_repaint();
        }
        if self.open <= 0.0 {
            self.shown = None;
            return;
        }
        let Some((dir, icon)) = self.shown.clone() else { return };
        let Some(pics) = pictures(&dir, ctx) else { return };
        let _fan = paint(ctx, &pics, icon, area, self.open, thumbs, tab_id);
        #[cfg(test)]
        {
            self.fan = Some((_fan, area));
        }
    }
}

fn paint(ctx: &egui::Context, pics: &Pics, icon: Rect, area: Rect, open: f32, thumbs: &mut crate::thumbs::Thumbs, tab_id: u64) -> Rect {
    let mut fan = Rect::NOTHING;
    // A spring that overshoots a little on the way out.
    let k = {
        let t = open.clamp(0.0, 1.0);
        let c = 1.7;
        1.0 + (c + 1.0) * (t - 1.0).powi(3) + c * (t - 1.0).powi(2)
    };
    let p = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::new(("peek", tab_id)))).with_clip_rect(area);
    let ppp = ctx.pixels_per_point();
    let side = (icon.width() * 0.78).clamp(36.0, 150.0);
    let n = pics.len().min(CARDS);
    // The cards stand on a point just inside the folder, turning out from it.
    let base = pos2(icon.center().x, icon.top() + icon.height() * 0.45);
    // A folder in the top row or at a side would push the fan out of the view: the whole
    // hand moves in instead, down over its folder or along the row.
    let reach = vec2(side * (0.42 * k + 0.65), side * (0.62 * k + 0.6));
    let fit = |lo: f32, hi: f32, at: f32, r: f32| if hi - lo < 2.0 * r { (lo + hi) / 2.0 } else { at.clamp(lo + r, hi - r) };
    let base = pos2(
        fit(area.left() + 4.0, area.right() - 4.0, base.x, reach.x),
        base.y.max(area.top() + 4.0 + reach.y),
    );
    for (i, (path, mtime)) in pics.iter().take(n).enumerate() {
        let spread = if n == 1 { 0.0 } else { i as f32 / (n - 1) as f32 * 2.0 - 1.0 };
        let angle = spread * 0.26 * k;
        let lift = side * 0.62 * k;
        let Some(l) = thumbs.get(path, *mtime, side * ppp) else { continue };
        let [tw, th] = l.tex.size();
        let s = (side / tw.max(1) as f32).min(side / th.max(1) as f32);
        let size = vec2(tw as f32 * s, th as f32 * s);
        let centre = base + vec2(spread * side * 0.42 * k, -lift);
        let r = Rect::from_center_size(centre, size);
        let rot = Rot2::from_angle(angle);
        fan = fan.union(r);
        let pivot = pos2(centre.x, centre.y + size.y * 0.9);
        let alpha = open.clamp(0.0, 1.0);
        // A soft shadow under each card, then the card with a thin light edge.
        let mut shadow = egui::Mesh::default();
        shadow.add_colored_rect(r.translate(vec2(0.0, 4.0)).expand(2.0), Color32::from_black_alpha((90.0 * alpha) as u8));
        shadow.rotate(rot, pivot);
        p.add(shadow);
        let mut edge = egui::Mesh::default();
        edge.add_colored_rect(r.expand(1.5), Color32::from_white_alpha((200.0 * alpha) as u8));
        edge.rotate(rot, pivot);
        p.add(edge);
        let mut card = egui::Mesh::with_texture(l.tex.id());
        card.add_rect_with_uv(r, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE.gamma_multiply(alpha));
        card.rotate(rot, pivot);
        p.add(card);
    }
    fan
}
