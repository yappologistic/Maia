//! The picture viewer: the pictures of a folder full window over Maia, in the listing's
//! order. Pictures come through the thumbnail cache (decoded on its workers, the right
//! way up, within the asset cache's budget), the neighbours asked for ahead. Nothing
//! repaints while it is still: the slideshow's timer runs only while it plays, an
//! animated picture's only until its next frame (K pauses it), and the name fades away
//! once the pointer rests.
//!
//! Pictures can be sorted while flipping through them: Delete sends one to the trash
//! (Ctrl+Z, here or after, brings it back), R and L turn it through ImageMagick, Ctrl+1
//! to Ctrl+7 toggle the colour labels and T opens a strip of every tag. C pins the
//! picture shown to the left and steps on beside it; both share one zoom and pan, each
//! relative to its own fit, so two shots of the same size line up pixel for pixel.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use egui::{Align2, Color32, CornerRadius, FontId, Id, Key, Modifiers, Pos2, Rect, Sense, Stroke, Vec2, pos2, vec2};

use crate::app::App;
use crate::popups::Popup;
use crate::theme::Glyph;

/// How long each picture stays in a slideshow.
pub const SLIDE: f64 = 4.0;
/// The fade from one picture to the next.
const FADE: f64 = 0.35;
/// The name and position stay this long after the pointer last moved, then fade out.
const SHOW: f64 = 2.0;
const HIDE: f64 = 0.4;
/// How long the app's own message shows over the viewer: as long as its toast does.
const NOTE: f32 = 3.0;

pub fn is_picture(ext: &str) -> bool {
    crate::thumbs::is_image_ext(ext) || ext == "svg" || crate::extpic::is_ext(ext) && crate::drives::Tools::current().is_some_and(|t| crate::extpic::can(&t, ext))
}

/// Whether the viewer covers the window (what is under it needn't move).
pub fn covers(ctx: &egui::Context) -> bool {
    ctx.memory(|m| m.areas().visible_last_frame(&egui::LayerId::new(egui::Order::Foreground, Id::new("picture viewer"))))
}

#[derive(Clone)]
pub struct Pic {
    pub path: PathBuf,
    pub name: String,
    pub modified: Option<SystemTime>,
}

/// The tag strip: the chip picked, and a new tag's name while one is typed.
pub struct Strip {
    pub at: usize,
    pub typing: Option<String>,
}

pub struct State {
    pub panel: u64,
    pub pics: Vec<Pic>,
    pub at: usize,
    /// Scale relative to fitting the window (1: fit) and the offset from the middle.
    pub zoom: f32,
    pub pan: Vec2,
    pub playing: bool,
    /// The picture shown, playing, when it moves.
    pub anim: Option<crate::anim::Player>,
    /// When the slideshow moves on.
    next_at: f64,
    /// The picture fading out, and when it started to.
    from: Option<(usize, f64)>,
    /// When the pointer last moved or a key was pressed (the name shows meanwhile).
    stirred: f64,
    last_pointer: Option<Pos2>,
    /// Pictures sent to the trash from here, the latest last, with where each was.
    trashed: Vec<(usize, Pic)>,
    /// The changes Undo held when the viewer opened: Ctrl+Z here takes back only later
    /// ones.
    undo_mark: usize,
    /// Pictures whose original is in the trash already, kept by this viewer's turns (a
    /// second turn keeps no second copy), shared with the jobs turning them.
    kept: Arc<Mutex<HashSet<PathBuf>>>,
    /// Turns asked for and not done.
    pub turning: Vec<PathBuf>,
    /// A changed picture's new modified time, from the worker that changed it.
    stat: (Sender<(PathBuf, Option<SystemTime>)>, Receiver<(PathBuf, Option<SystemTime>)>),
    pub strip: Option<Strip>,
    /// The picture pinned to the left to compare against.
    pub pin: Option<Pic>,
    /// When it opened: messages from before are not its own.
    opened: Instant,
    /// Where the picture grows from as the viewer opens (its place in the listing), and
    /// when it started to.
    pub grow: Option<(Rect, f64)>,
    /// Closing since: the picture goes back into its place, the window comes back.
    pub shrink: Option<f64>,
    /// Comparing over one another instead of side by side: where the line between the
    /// pinned picture (left of it) and this one is, 0..1 across.
    pub wipe: Option<f32>,
    wiping: bool,
    /// Across what the line moves: the two pictures' width together as drawn last frame,
    /// so a small picture in a big window does not leave the line out in the dark.
    wipe_span: (f32, f32),
    /// The filmstrip along the bottom, how far up (0..1).
    film: f32,
}

impl State {
    pub fn new(panel: u64, pics: Vec<Pic>, at: usize, now: f64) -> State {
        State {
            panel,
            pics,
            at,
            zoom: 1.0,
            pan: Vec2::ZERO,
            playing: false,
            anim: None,
            next_at: 0.0,
            from: None,
            stirred: now,
            last_pointer: None,
            trashed: vec![],
            undo_mark: 0,
            kept: Default::default(),
            turning: vec![],
            stat: channel(),
            strip: None,
            pin: None,
            opened: Instant::now(),
            grow: None,
            shrink: None,
            wipe: None,
            wiping: false,
            wipe_span: (0.0, 0.0),
            film: 0.0,
        }
    }

    /// The picture fading out, while one is.
    #[cfg(test)]
    pub fn fading_from(&self) -> Option<usize> {
        self.from.map(|f| f.0)
    }

    /// Where the wipe's line is and the span it moves across, as last drawn.
    #[cfg(test)]
    pub fn wipe_line(&self) -> Option<(f32, (f32, f32))> {
        let (l, r) = self.wipe_span;
        self.wipe.filter(|_| r > l).map(|f| (l + f * (r - l), (l, r)))
    }

    pub fn current(&self) -> Option<&Pic> {
        self.pics.get(self.at)
    }

    /// The name and position's opacity at `now`.
    pub fn label_alpha(&self, now: f64, animations: bool) -> f32 {
        let idle = now - self.stirred;
        if idle < SHOW {
            1.0
        } else if animations && idle < SHOW + HIDE {
            (1.0 - (idle - SHOW) / HIDE) as f32
        } else {
            0.0
        }
    }

    pub fn go(&mut self, to: usize, now: f64, animations: bool) {
        if to == self.at || to >= self.pics.len() {
            return;
        }
        self.from = animations.then_some((self.at, now));
        self.at = to;
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
    }
}

/// The pictures in the active panel's listing, starting from `start` (or the one under
/// the cursor). None when there are none.
pub fn open_at(app: &mut App, start: Option<PathBuf>) -> bool {
    let panel = app.active;
    let t = app.tab();
    let pics: Vec<Pic> = t.view.iter().map(|&i| &t.entries[i as usize]).filter(|e| !e.is_dir && is_picture(e.ext())).map(|e| Pic { path: e.path.clone(), name: e.name.clone(), modified: e.modified }).collect();
    if pics.is_empty() {
        return false;
    }
    let want = start.or_else(|| t.cursor.and_then(|c| t.entries.get(c as usize)).map(|e| e.path.clone()));
    let at = want.and_then(|w| pics.iter().position(|p| p.path == w)).unwrap_or(0);
    let now = app.ctx.input(|i| i.time);
    let mut st = State::new(panel, pics, at, now);
    st.undo_mark = app.undo.len();
    if app.cfg.animations {
        st.grow = place_of(app, panel, &st.pics[at].path).map(|r| (r, now));
    }
    app.popup = Popup::Viewer(Box::new(st));
    true
}

/// View Pictures: from the picture under the cursor, else the folder's first.
pub fn open(app: &mut App) {
    if !open_at(app, None) {
        app.toast("No pictures here");
    }
}

/// Opening and closing take this long.
const GROW: f64 = 0.28;

fn ease(t: f64) -> f32 {
    (1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)) as f32
}

/// Where `path` was drawn in its panel's listing last frame, when it was on screen.
fn place_of(app: &App, panel: u64, path: &std::path::Path) -> Option<Rect> {
    let t = app.panels.get(&panel)?.tab();
    let idx = t.entries.iter().position(|e| e.path == path)? as u32;
    t.item_rects.iter().find(|(i, _)| *i == idx).map(|(_, r)| *r).filter(|r| r.is_finite() && r.width() > 1.0)
}

/// `a` to `b`, `k` of the way.
fn lerp_rect(a: Rect, b: Rect, k: f32) -> Rect {
    Rect::from_min_max(a.min + (b.min - a.min) * k, a.max + (b.max - a.max) * k)
}

/// The closing: the picture shrinks back into its place (or fades where it has none on
/// screen) as the dark lifts. False once done.
fn closing(app: &mut App, ctx: &egui::Context, st: &mut State, t0: f64) -> bool {
    let now = ctx.input(|i| i.time);
    let k = ease((now - t0) / (GROW * 0.85));
    if k >= 1.0 {
        return false;
    }
    let screen = ctx.content_rect();
    let area = screen.shrink(12.0);
    let Some(cur) = st.current().cloned() else { return false };
    let target = place_of(app, st.panel, &cur.path);
    let shown = app.thumbs.get(&cur.path, cur.modified, area.width().max(area.height())).map(|l| (l.tex.id(), l.dims));
    egui::Area::new(Id::new("picture viewer")).order(egui::Order::Foreground).fixed_pos(screen.min).interactable(false).show(ctx, |ui| {
        let p = ui.painter();
        p.rect_filled(screen, CornerRadius::ZERO, Color32::from_rgb(14, 14, 15).gamma_multiply(1.0 - k));
        if let Some((tex, dims)) = shown {
            let size = vec2(dims[0] as f32, dims[1] as f32) * fit_scale(area, dims) * st.zoom;
            let from = Rect::from_center_size(area.center() + st.pan, size);
            let (to, a) = match target {
                Some(t) => (t, 1.0),
                None => (Rect::from_center_size(from.center(), from.size() * 0.92), 1.0 - k),
            };
            p.image(tex, lerp_rect(from, to, k), Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE.gamma_multiply(a));
        }
    });
    ctx.request_repaint();
    true
}

/// The scale that fits `dims` into `area`, never past one point per pixel.
pub fn fit_scale(area: Rect, dims: [u32; 2]) -> f32 {
    (area.width() / dims[0].max(1) as f32).min(area.height() / dims[1].max(1) as f32).min(1.0)
}

/// The viewer. Returns false when it closes.
pub fn draw(app: &mut App, ctx: &egui::Context, st: &mut State) -> bool {
    let animations = app.cfg.animations;
    let (now, ppp, pointer) = ctx.input(|i| (i.time, i.pixels_per_point(), i.pointer.latest_pos()));
    let screen = ctx.content_rect();
    take_stats(app, st);
    let n = st.pics.len();
    if n == 0 {
        return false;
    }
    st.at = st.at.min(n - 1);
    if let Some(t0) = st.shrink {
        return closing(app, ctx, st, t0);
    }
    // A new tag being typed has the keys; otherwise the strip, while open, takes the
    // arrows, Enter and Space.
    let typing = st.strip.as_ref().is_some_and(|s| s.typing.is_some());
    let key = |k: Key| !typing && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, k));
    let ctrl_key = |k: Key| !typing && ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, k));
    let shift_key = |k: Key| !typing && ctx.input_mut(|i| i.consume_key(Modifiers::SHIFT, k));
    let undo = ctrl_key(Key::Z);
    let label = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7].iter().position(|k| ctrl_key(*k));
    let (esc, l_key, r_key, space, home, end) = (key(Key::Escape), key(Key::ArrowLeft), key(Key::ArrowRight), key(Key::Space), key(Key::Home), key(Key::End));
    let zoom_in = key(Key::Plus) || key(Key::Equals) || shift_key(Key::Equals) || shift_key(Key::Plus);
    let (zoom_out, fit, actual, pause) = (key(Key::Minus), key(Key::Num0), key(Key::Num1), key(Key::K));
    let (delete, turn_r, turn_l, tags_key, compare, enter) = (key(Key::Delete), key(Key::R), key(Key::L), key(Key::T), key(Key::C), key(Key::Enter));
    let wipe_key = key(Key::W);
    let strip_keys = st.strip.is_some() && !typing;
    let (left, right) = if strip_keys { (false, false) } else { (l_key, r_key) };
    if esc {
        // One step back at a time: the strip, then the comparison, then the viewer.
        if st.strip.take().is_none() && {
            st.wipe = None;
            st.pin.take().is_none()
        } {
            close(app, st);
            if animations {
                st.shrink = Some(now);
                ctx.request_repaint();
                return true;
            }
            return false;
        }
        st.pan = Vec2::ZERO;
    }
    if left || right || space || home || end || zoom_in || zoom_out || fit || actual || delete || turn_r || turn_l || tags_key || compare || undo || label.is_some() || pointer != st.last_pointer {
        st.stirred = now;
    }
    st.last_pointer = pointer;
    if space && !strip_keys {
        st.playing = !st.playing;
        st.next_at = now + SLIDE;
    }
    let mut to = None;
    if right && st.at + 1 < n {
        to = Some(st.at + 1);
    }
    if left && st.at > 0 {
        to = Some(st.at - 1);
    }
    if home {
        to = Some(0);
    }
    if end {
        to = Some(n - 1);
    }
    if st.playing && now >= st.next_at {
        to = Some((st.at + 1) % n);
        st.next_at = now + SLIDE;
    }
    if let Some(i) = to {
        st.go(i, now, animations);
        if st.playing {
            st.next_at = now + SLIDE;
        }
    }
    // What the keys ask of the picture shown.
    let mut act = None;
    if delete {
        act = Some(Do::Trash);
    } else if turn_r || turn_l {
        act = Some(Do::Turn(turn_r));
    } else if let Some(k) = label {
        act = Some(Do::Tag(crate::tags::COLORS[k].0.to_string()));
    } else if undo {
        act = Some(Do::Undo);
    } else if tags_key {
        act = Some(Do::Strip);
    } else if compare {
        act = Some(Do::Pin);
    } else if wipe_key {
        act = Some(Do::Wipe);
    }
    let names = tag_names(st);
    if let Some(sp) = st.strip.as_mut().filter(|_| strip_keys) {
        let k = names.len() + 1;
        if l_key {
            sp.at = (sp.at + k - 1) % k;
        }
        if r_key {
            sp.at = (sp.at + 1) % k;
        }
        if enter || space {
            act = Some(match names.get(sp.at) {
                Some(t) => Do::Tag(t.clone()),
                None => Do::NewTag,
            });
        }
    }
    if let Some(a) = act.take() {
        if !run(app, ctx, st, a, now) {
            return false;
        }
    }
    let n = st.pics.len();
    let area = screen.shrink(12.0);
    // Comparing: the pinned picture on the left half, the one stepped to on the right.
    let (cur_area, pin_area) = match &st.pin {
        Some(_) if area.width() > 240.0 && st.wipe.is_none() => {
            let w = (area.width() - 12.0) / 2.0;
            (Rect::from_min_max(pos2(area.right() - w, area.top()), area.max), Some(Rect::from_min_max(area.min, pos2(area.left() + w, area.bottom()))))
        }
        _ => (area, None),
    };
    // No bigger than the graphics card takes in one texture.
    let most_side = ctx.input(|i| i.max_texture_side).min(4096) as f32;
    let want = |zoom: f32| (area.width().max(area.height()) * ppp * zoom.max(1.0)).min(most_side);
    // Ask for the neighbours first: the cache's workers take the newest request first,
    // so the picture shown is decoded before them.
    for d in [st.at + 1, st.at.wrapping_sub(1)] {
        if let Some(p) = st.pics.get(d) {
            let _ = app.thumbs.get(&p.path, p.modified, want(1.0));
        }
    }
    let cur = st.pics[st.at].clone();
    let ext = cur.path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if !crate::anim::may_animate(&ext) {
        st.anim = None;
    } else if st.anim.as_ref().is_none_or(|a| !a.is_for(&cur.path, cur.modified, want(1.0))) {
        st.anim = Some(crate::anim::Player::start(&cur.path, cur.modified, want(1.0), app.thumbs.budget_bytes, ctx));
    }
    if let Some(a) = st.anim.as_mut().filter(|_| pause) {
        a.toggle();
    }
    let moving = st.anim.as_mut().and_then(|a| a.frame(true));
    let shown = moving.or_else(|| app.thumbs.get(&cur.path, cur.modified, want(st.zoom)).map(|l| (l.tex.id(), l.dims)));
    let failed = shown.is_none() && app.thumbs.failed(&cur.path);
    let pinned = st.pin.clone().map(|p| {
        let got = app.thumbs.get(&p.path, p.modified, want(st.zoom)).map(|l| (l.tex.id(), l.dims));
        let failed = got.is_none() && app.thumbs.failed(&p.path);
        (p, got, failed)
    });
    // Zoom by keys, around the middle.
    if let Some((_, dims)) = shown {
        let base = fit_scale(cur_area, dims);
        let most = (16.0 / ppp) / base.max(1e-4);
        if zoom_in {
            st.zoom = (st.zoom * 1.25).min(most.max(1.0));
        }
        if zoom_out {
            st.zoom = (st.zoom / 1.25).max(0.1);
        }
        if actual {
            st.zoom = (1.0 / ppp) / base.max(1e-4);
        }
        if fit {
            st.zoom = 1.0;
            st.pan = Vec2::ZERO;
        }
    }
    let pal = app.pal;
    let mut close_now = false;
    // While the strip is open its picture's name stays, to show the tags it changes.
    let label_a = if st.strip.is_some() { 1.0 } else { st.label_alpha(now, animations) };
    let fs = app.cfg.font_size;
    let cur_tags = crate::tags::get(&cur.path, |t| t.to_vec()).unwrap_or_default();
    let note = app.toast.as_ref().filter(|t| t.1 >= st.opened).map(|t| (t.0.clone(), t.1.elapsed().as_secs_f32())).filter(|t| t.1 < NOTE);
    // Over the job cards and toasts, which show here as its own note: a card that comes
    // up after the viewer opened would otherwise be put above it.
    ctx.move_to_top(egui::LayerId::new(egui::Order::Foreground, Id::new("picture viewer")));
    egui::Area::new(Id::new("picture viewer")).order(egui::Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
        let (r, resp) = ui.allocate_exact_size(screen.size(), Sense::click_and_drag());
        let p = ui.painter();
        let grown = match st.grow {
            Some((_, t0)) => ease((now - t0) / GROW),
            None => 1.0,
        };
        if grown >= 1.0 {
            st.grow = None;
        } else {
            ctx.request_repaint();
        }
        p.rect_filled(r, CornerRadius::ZERO, Color32::from_rgb(14, 14, 15).gamma_multiply(grown));
        if let Some((tex, dims)) = shown {
            let base = fit_scale(cur_area, dims);
            // The wheel zooms around the pointer, in whichever picture it is over.
            if resp.hovered() {
                let (scroll, zd) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
                let factor = if (zd - 1.0).abs() > 1e-4 { zd } else if scroll != 0.0 { (scroll / 300.0).exp() } else { 1.0 };
                if factor != 1.0 {
                    let old = st.zoom;
                    st.zoom = (st.zoom * factor).clamp(0.1, ((16.0 / ppp) / base.max(1e-4)).max(1.0));
                    if let Some(pp) = pointer {
                        let mid = pin_area.filter(|a| a.expand(6.0).contains(pp)).unwrap_or(cur_area).center();
                        let rel = pp - (mid + st.pan);
                        st.pan += rel - rel * (st.zoom / old);
                    }
                    st.stirred = now;
                }
            }
            // Over one another, a drag that starts on the line between them moves it.
            let span = if st.wipe_span.1 > st.wipe_span.0 + 1.0 { st.wipe_span } else { (cur_area.left(), cur_area.right()) };
            let line_x = st.wipe.map(|f| span.0 + f * (span.1 - span.0));
            // Judged where the press was: the pointer has moved on by the time it is a drag.
            if resp.drag_started() {
                let pressed = ui.input(|i| i.pointer.press_origin());
                st.wiping = matches!((line_x, pressed), (Some(x), Some(pp)) if (pp.x - x).abs() < 14.0);
            }
            if resp.dragged() {
                match (st.wiping, resp.interact_pointer_pos()) {
                    (true, Some(pp)) => st.wipe = Some(((pp.x - span.0) / (span.1 - span.0).max(1.0)).clamp(0.0, 1.0)),
                    _ => st.pan += resp.drag_delta(),
                }
            }
            if resp.drag_stopped() {
                st.wiping = false;
            }
            if let Some(x) = line_x.filter(|x| pointer.is_some_and(|pp| (pp.x - x).abs() < 14.0 && cur_area.contains(pp))) {
                let _ = x;
                ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            if resp.double_clicked() {
                // Between fitting the window and one pixel for one.
                let one = (1.0 / ppp) / base.max(1e-4);
                st.zoom = if (st.zoom - 1.0).abs() < 1e-3 { one } else { 1.0 };
                st.pan = Vec2::ZERO;
            }
            let size = vec2(dims[0] as f32, dims[1] as f32) * base * st.zoom;
            // Moved only as far as there is picture to see.
            let room = ((size - cur_area.size()) / 2.0).max(Vec2::ZERO);
            st.pan = st.pan.clamp(-room, room);
            let clip = if pin_area.is_some() { cur_area.expand(6.0) } else { r };
            let fading = st.from.map(|(_, t0)| ((now - t0) / FADE).clamp(0.0, 1.0) as f32).filter(|t| *t < 1.0);
            if let (Some(t), Some((i, _))) = (fading, st.from) {
                if let Some(old) = st.pics.get(i) {
                    if let Some(l) = app.thumbs.get(&old.path, old.modified, want(1.0)) {
                        let b = fit_scale(cur_area, l.dims);
                        let r0 = Rect::from_center_size(cur_area.center(), vec2(l.dims[0] as f32, l.dims[1] as f32) * b);
                        p.with_clip_rect(clip).image(l.tex.id(), r0, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE.gamma_multiply(1.0 - t));
                    }
                }
            }
            let img = Rect::from_center_size(cur_area.center() + st.pan, size);
            let img = match st.grow {
                Some((from, _)) => lerp_rect(from, img, grown),
                None => img,
            };
            let p = ui.painter().with_clip_rect(clip);
            if let Some(glow) = app.thumbs.get(&cur.path, cur.modified, want(st.zoom)).map(|l| l.glow) {
                crate::colours::paint_glow(&p, img, &glow, 0.42 * fading.unwrap_or(1.0) * grown);
            }
            match (st.wipe, pinned.as_ref().and_then(|(_, got, _)| *got)) {
                // The pinned picture left of the line, this one right of it, the same zoom
                // and pan over the same place, each from its own fit.
                (Some(f), Some((ptex, pdims))) => {
                    let psize = vec2(pdims[0] as f32, pdims[1] as f32) * fit_scale(cur_area, pdims) * st.zoom;
                    let pimg = Rect::from_center_size(cur_area.center() + st.pan, psize);
                    let span = (img.left().min(pimg.left()).max(cur_area.left()), img.right().max(pimg.right()).min(cur_area.right()));
                    st.wipe_span = span;
                    let x = span.0 + f * (span.1 - span.0);
                    let left = Rect::from_min_max(r.min, pos2(x, r.bottom()));
                    let right = Rect::from_min_max(pos2(x, r.top()), r.max);
                    ui.painter().with_clip_rect(left).image(ptex, pimg, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
                    ui.painter().with_clip_rect(right).image(tex, img, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE.gamma_multiply(fading.unwrap_or(1.0)));
                    let top = img.top().min(pimg.top()).max(r.top());
                    let bottom = img.bottom().max(pimg.bottom()).min(r.bottom());
                    let lp = ui.painter();
                    lp.line_segment([pos2(x, top), pos2(x, bottom)], Stroke::new(2.0, Color32::WHITE));
                    let knob = pos2(x, (top + bottom) / 2.0);
                    lp.circle_filled(knob, 11.0, Color32::WHITE);
                    lp.circle_stroke(knob, 11.0, Stroke::new(1.0, Color32::from_black_alpha(80)));
                    for d in [-1.0f32, 1.0] {
                        let c = knob + vec2(d * 4.5, 0.0);
                        lp.add(egui::Shape::convex_polygon(vec![c + vec2(d * 2.5, 0.0), c + vec2(-d * 1.5, -3.5), c + vec2(-d * 1.5, 3.5)], Color32::from_gray(60), Stroke::NONE));
                    }
                    // Which is which, small, at the top on either side of the line.
                    let small = FontId::proportional((fs - 2.0).max(9.0));
                    let names = [(st.pin.as_ref().map(|p| p.name.clone()).unwrap_or_default(), true), (cur.name.clone(), false)];
                    for (name, is_left) in names {
                        let g = crate::listing::elided(ui, &name, small.clone(), (if is_left { x - r.left() } else { r.right() - x } - 30.0).max(30.0), Color32::from_gray(230));
                        let w = g.size().x + 14.0;
                        // Above the pictures when they leave room there, so a small one is not hidden.
                        let h = g.size().y + 6.0;
                        let y = if top - r.top() > h + 70.0 { top - h - 8.0 } else { top + 10.0 };
                        let at = if is_left { pos2(x - 10.0 - w, y) } else { pos2(x + 10.0, y) };
                        let pill = Rect::from_min_size(at, vec2(w, h));
                        lp.rect_filled(pill, CornerRadius::same(6), Color32::from_black_alpha(150));
                        lp.galley(pill.min + vec2(7.0, 3.0), g, Color32::WHITE);
                    }
                }
                _ => {
                    p.image(tex, img, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE.gamma_multiply(fading.unwrap_or(1.0)));
                }
            }
            if fading.is_some() {
                ctx.request_repaint();
            } else {
                st.from = None;
            }
        } else if failed {
            p.text(cur_area.center(), Align2::CENTER_CENTER, "Can't show this picture", FontId::proportional(fs), Color32::from_gray(170));
        } else {
            crate::ui::spinner(p, cur_area.center(), 9.0, pal.accent, now);
            ctx.request_repaint_after(Duration::from_millis(33));
        }
        // The pinned picture, at the same zoom and pan from its own fit.
        if let (Some(pa), Some((_, got, failed))) = (pin_area, &pinned) {
            let p = ui.painter().with_clip_rect(pa.expand(6.0));
            match got {
                Some((tex, dims)) => {
                    let size = vec2(dims[0] as f32, dims[1] as f32) * fit_scale(pa, *dims) * st.zoom;
                    p.image(*tex, Rect::from_center_size(pa.center() + st.pan, size), Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
                }
                None if *failed => {
                    p.text(pa.center(), Align2::CENTER_CENTER, "Can't show this picture", FontId::proportional(fs), Color32::from_gray(170));
                }
                None => {
                    crate::ui::spinner(&p, pa.center(), 9.0, pal.accent, now);
                    ctx.request_repaint_after(Duration::from_millis(33));
                }
            }
            p.line_segment([pos2(pa.right() + 6.0, pa.top() + 8.0), pos2(pa.right() + 6.0, pa.bottom() - 8.0)], Stroke::new(1.0, Color32::from_white_alpha(28)));
        }
        // The filmstrip: the pointer near the bottom raises the folder's pictures there,
        // the ones nearest it a little larger; a click goes to one.
        let base = (fs * 3.6).round();
        let near = pointer.is_some_and(|pp| r.contains(pp) && pp.y > r.bottom() - base * 2.3) && n > 1 && st.strip.is_none() && r.width() > 300.0;
        let dt = ctx.input(|i| i.stable_dt).min(0.05);
        let target = if near { 1.0 } else { 0.0 };
        st.film = if !animations { target } else if target > st.film { (st.film + dt / 0.18).min(1.0) } else { (st.film - dt / 0.25).max(0.0) };
        if st.film != target {
            ctx.request_repaint();
        }
        let film_h = (base + 30.0) * st.film;
        if st.film > 0.0 {
            let ease = 1.0 - (1.0 - st.film).powi(3);
            let band = Rect::from_min_max(pos2(r.left(), r.bottom() - (base * 1.6 + 30.0) * ease), r.max);
            let mut shade = egui::Mesh::default();
            shade.colored_vertex(band.left_top(), Color32::TRANSPARENT);
            shade.colored_vertex(band.right_top(), Color32::TRANSPARENT);
            shade.colored_vertex(band.left_bottom(), Color32::from_black_alpha((170.0 * ease) as u8));
            shade.colored_vertex(band.right_bottom(), Color32::from_black_alpha((170.0 * ease) as u8));
            shade.add_triangle(0, 1, 2);
            shade.add_triangle(1, 3, 2);
            ui.painter().add(shade);
            let gap = 6.0;
            let cy = r.bottom() - 14.0 - base / 2.0 + (1.0 - ease) * (base + 30.0);
            let px = pointer.map_or(f32::NAN, |p| p.x);
            let grow = |x: f32| if px.is_nan() { 1.0 } else { 1.0 + 0.5 * (-((x - px) / (base * 1.3)).powi(2)).exp() };
            // Laid out from the picture shown, outwards both ways, as far as fits.
            let mut slots: Vec<(usize, f32, f32)> = vec![(st.at, r.center().x, base * grow(r.center().x))];
            for dir in [-1i64, 1] {
                let (mut x, mut w) = (r.center().x, slots[0].2);
                let mut i = st.at as i64;
                loop {
                    i += dir;
                    if i < 0 || i as usize >= n {
                        break;
                    }
                    let guess = x + dir as f32 * (w / 2.0 + gap + base / 2.0);
                    let nw = base * grow(guess);
                    x += dir as f32 * (w / 2.0 + gap + nw / 2.0);
                    w = nw;
                    if x - w / 2.0 < r.left() + 8.0 || x + w / 2.0 > r.right() - 8.0 {
                        break;
                    }
                    slots.push((i as usize, x, w));
                }
            }
            for (i, x, w) in slots {
                let cell = Rect::from_center_size(pos2(x, cy - (w - base) / 2.0), vec2(w, w));
                let resp = ui.interact(cell, Id::new(("filmstrip", i)), Sense::click());
                crate::ui::note_control(ui, cell, "viewer filmstrip");
                if resp.clicked() {
                    to = Some(i);
                }
                let pic = &st.pics[i];
                let p = ui.painter();
                match app.thumbs.get(&pic.path, pic.modified, base * 1.6 * ppp) {
                    Some(l) => {
                        let [tw, th] = l.tex.size();
                        let (tw, th) = (tw.max(1) as f32, th.max(1) as f32);
                        let side = tw.min(th);
                        let uv = Rect::from_min_max(pos2((tw - side) / 2.0 / tw, (th - side) / 2.0 / th), pos2(1.0 - (tw - side) / 2.0 / tw, 1.0 - (th - side) / 2.0 / th));
                        p.image(l.tex.id(), cell, uv, Color32::WHITE.gamma_multiply(ease));
                    }
                    None => {
                        p.rect_filled(cell, CornerRadius::same(4), Color32::from_white_alpha((30.0 * ease) as u8));
                    }
                }
                if i == st.at {
                    p.rect_stroke(cell.expand(1.5), CornerRadius::same(3), Stroke::new(2.0, Color32::WHITE.gamma_multiply(ease)), egui::StrokeKind::Outside);
                }
            }
        }
        let mut pill_top = r.bottom();
        // The name, its tags and where it is in the folder, quietly, while the pointer
        // moves; the pinned picture's name under it, with a pin.
        if label_a > 0.0 {
            let mid = if pin_area.is_some() { cur_area.center().x } else { r.center().x };
            let room = if pin_area.is_some() { cur_area.width() } else { r.width() };
            let pos = format!("{} / {}", st.at + 1, n);
            let pill = name_pill(ui, mid, r.bottom() - 18.0 - film_h, room, &cur.name, &cur_tags, Some(&pos), false, st.playing, fs, label_a);
            pill_top = pill.top();
            if let (Some(pa), Some((pp, ..))) = (pin_area, &pinned) {
                let tags = crate::tags::get(&pp.path, |t| t.to_vec()).unwrap_or_default();
                name_pill(ui, pa.center().x, r.bottom() - 18.0 - film_h, pa.width(), &pp.name, &tags, None, true, false, fs, label_a);
            }
        }
        // The tag strip, above the name: every colour and word, those it has lit.
        if st.strip.is_some() {
            let (top, clicked) = strip(ui, st, &names, &cur_tags, r, pill_top - 10.0, fs, &pal);
            pill_top = top;
            if let Some(i) = clicked {
                if let Some(sp) = st.strip.as_mut() {
                    sp.at = i;
                }
                act = Some(match names.get(i) {
                    Some(t) => Do::Tag(t.clone()),
                    None => Do::NewTag,
                });
            }
            if let Some(sp) = st.strip.as_mut() {
                if let Some(t) = sp.typing.as_mut() {
                    let (out, typed) = tag_field(ui, t, r, pill_top - 8.0, fs, &pal);
                    pill_top -= typed;
                    match out {
                        Field::Done => {
                            let name = t.replace(',', " ").trim().to_string();
                            sp.typing = None;
                            if !name.is_empty() {
                                act = Some(Do::Tag(name));
                            }
                        }
                        Field::Cancel => sp.typing = None,
                        Field::Typing => {}
                    }
                }
            }
        }
        // What the app says while the viewer covers it (a picture that would not turn,
        // ImageMagick missing), above the rest; it fades as its toast does.
        if let Some((msg, age)) = &note {
            let a = if *age > NOTE - 0.4 { ((NOTE - age) / 0.4).clamp(0.0, 1.0) } else { 1.0 };
            let g = crate::listing::elided(ui, msg, FontId::proportional(fs - 1.0), (r.width() - 80.0).max(60.0), Color32::from_gray(235).gamma_multiply(a));
            let h = g.size().y + 12.0;
            let nr = Rect::from_center_size(pos2(r.center().x, pill_top - 10.0 - h / 2.0), vec2(g.size().x + 28.0, h));
            ui.painter().rect_filled(nr, CornerRadius::same((h / 2.0) as u8), Color32::from_black_alpha((170.0 * a) as u8));
            ui.painter().galley(pos2(nr.left() + 14.0, nr.center().y - g.size().y / 2.0), g, Color32::WHITE);
            ctx.request_repaint_after(Duration::from_secs_f32(if *age > NOTE - 0.4 { 0.016 } else { NOTE - 0.4 - age + 0.01 }));
        }
        if label_a > 0.0 {
            // Close at the corner with what can be done to the picture beside it;
            // previous and next at the sides.
            let button = |rect: Rect, what: &'static str, tip: &str, enabled: bool, on: bool, icon: &dyn Fn(&egui::Painter, Rect, Color32)| -> bool {
                let b = ui.interact(rect, Id::new(("viewer button", what)), Sense::click());
                crate::ui::note_control(ui, rect, what);
                let fill = if b.hovered() { 120.0 } else { 70.0 };
                let bg = if on { pal.accent.gamma_multiply(0.85 * label_a) } else { Color32::from_black_alpha((fill * label_a) as u8) };
                ui.painter().rect_filled(rect, CornerRadius::same((rect.width() / 2.0) as u8), bg);
                let col = Color32::from_gray(if enabled { 230 } else { 110 }).gamma_multiply(label_a);
                icon(ui.painter(), Rect::from_center_size(rect.center(), Vec2::splat(rect.width() * 0.45)), col);
                let b = if tip.is_empty() { b } else { b.on_hover_text(tip) };
                enabled && b.clicked()
            };
            let glyph = |g: Glyph| move |p: &egui::Painter, r: Rect, c: Color32| crate::theme::paint_glyph(p, r, g, c);
            let symbol = |name: &'static str| move |p: &egui::Painter, r: Rect, c: Color32| {
                crate::theme::paint_symbol(p, r, name, c);
            };
            let corner = |k: f32| Rect::from_center_size(pos2(r.right() - 30.0 - k * 42.0, r.top() + 30.0), Vec2::splat(34.0));
            if button(corner(0.0), "viewer close", "", true, false, &glyph(Glyph::Close)) {
                close_now = true;
            }
            // Only where they fit beside the close button and clear of the arrows.
            if r.width() > 470.0 {
                let tools: [(&'static str, &str, bool, Do, &dyn Fn(&egui::Painter, Rect, Color32)); 6] = [
                    ("viewer trash", "Move to the trash (Delete)", false, Do::Trash, &symbol("trash-2")),
                    ("viewer rotate right", "Turn right (R)", false, Do::Turn(true), &symbol("turn")),
                    ("viewer rotate left", "Turn left (L)", false, Do::Turn(false), &|p: &egui::Painter, r: Rect, c: Color32| {
                        crate::symbols::paint_mirrored(p, r, "turn", c);
                    }),
                    ("viewer tags", "Tags (T)", st.strip.is_some(), Do::Strip, &symbol("tag")),
                    ("viewer compare", "Compare beside this one (C)", st.pin.is_some() && st.wipe.is_none(), Do::Pin, &symbol("columns-2")),
                    ("viewer wipe", "Compare over one another, a line to drag between them (W)", st.wipe.is_some(), Do::Wipe, &symbol("view-columns")),
                ];
                for (k, (what, tip, on, d, icon)) in tools.into_iter().enumerate() {
                    if button(corner(k as f32 + 1.0), what, tip, true, on, icon) {
                        act = Some(d);
                    }
                }
            }
            if n > 1 && r.width() > 200.0 {
                if button(Rect::from_center_size(pos2(r.left() + 34.0, r.center().y), Vec2::splat(40.0)), "viewer previous", "", st.at > 0, false, &glyph(Glyph::Back)) {
                    to = Some(st.at - 1);
                }
                if button(Rect::from_center_size(pos2(r.right() - 34.0, r.center().y), Vec2::splat(40.0)), "viewer next", "", st.at + 1 < n, false, &glyph(Glyph::Forward)) {
                    to = Some(st.at + 1);
                }
            }
            if st.strip.is_some() {
            } else if label_a < 1.0 {
                ctx.request_repaint();
            } else {
                ctx.request_repaint_after(Duration::from_secs_f64((SHOW - (now - st.stirred)).max(0.0) + 0.01));
            }
        }
    });
    if let Some(a) = act {
        st.stirred = now;
        if !run(app, ctx, st, a, now) {
            return false;
        }
        ctx.request_repaint();
    }
    if let Some(i) = to.filter(|i| *i != st.at) {
        st.go(i, now, animations);
        st.stirred = now;
        if st.playing {
            st.next_at = now + SLIDE;
        }
        ctx.request_repaint();
    }
    if st.playing {
        ctx.request_repaint_after(Duration::from_secs_f64((st.next_at - now).max(0.0)));
    }
    if close_now {
        close(app, st);
        if animations {
            st.shrink = Some(now);
            ctx.request_repaint();
            return true;
        }
        return false;
    }
    true
}

/// What a key or a button asks of the picture shown.
enum Do {
    Trash,
    /// A quarter turn, to the right when true.
    Turn(bool),
    /// Add the tag, or take it off when the picture has it.
    Tag(String),
    NewTag,
    Undo,
    Strip,
    Pin,
    Wipe,
}

/// Carry out `a`; false when the viewer has nothing left to show.
fn run(app: &mut App, ctx: &egui::Context, st: &mut State, a: Do, now: f64) -> bool {
    let Some(cur) = st.current().cloned() else { return false };
    // Pictures inside an archive or the trash are only looked at.
    let (archive, trash) = app.panels.get(&st.panel).map(|p| (p.tab().archive.is_some(), p.tab().path.starts_with(crate::sidebar::trash_dir()))).unwrap_or((false, false));
    let fixed = || if archive { "Pictures inside an archive can't be changed" } else { "Pictures in the trash can't be changed" };
    match a {
        Do::Trash | Do::Turn(_) | Do::Tag(_) | Do::NewTag if archive || trash => app.toast(fixed()),
        Do::Trash => {
            app.start_job(crate::ops::OpKind::Trash { paths: vec![cur.path.clone()] });
            let at = st.at;
            st.trashed.push((at, st.pics.remove(at)));
            if st.pin.as_ref().is_some_and(|p| p.path == cur.path) {
                st.pin = None;
            }
            if st.pics.is_empty() {
                return false;
            }
            st.at = at.min(st.pics.len() - 1);
            st.from = None;
            st.zoom = 1.0;
            st.pan = Vec2::ZERO;
        }
        Do::Turn(right) => turn(app, ctx, st, &cur, right),
        Do::Tag(tag) => {
            let path = cur.path.clone();
            app.errands.run(ctx, move || match toggle_tag(&path, &tag) {
                Ok(()) => crate::errands::Done::default(),
                Err(e) => crate::errands::Done::say(format!("Couldn't tag {e}")),
            });
        }
        Do::NewTag => {
            let sp = st.strip.get_or_insert(Strip { at: 0, typing: None });
            sp.typing = Some(String::new());
        }
        Do::Undo => undo(app, st, now),
        Do::Strip => {
            st.strip = match st.strip.take() {
                Some(_) => None,
                None => Some(Strip { at: 0, typing: None }),
            }
        }
        Do::Pin => {
            st.pin = match st.pin.take() {
                Some(_) => None,
                None => Some(cur),
            };
            st.wipe = None;
            st.zoom = 1.0;
            st.pan = Vec2::ZERO;
        }
        Do::Wipe => {
            // Over one another: pins this one first when nothing is pinned.
            if st.pin.is_none() {
                st.pin = Some(cur);
            }
            st.wipe = if st.wipe.is_some() { None } else { Some(0.5) };
            st.zoom = 1.0;
            st.pan = Vec2::ZERO;
        }
    }
    true
}

/// A quarter turn of `cur` through ImageMagick, on the errands' thread: the original to
/// the trash on the first turn (Undo puts it back), the picture shown again once its new
/// time is read.
fn turn(app: &mut App, ctx: &egui::Context, st: &mut State, cur: &Pic, right: bool) {
    let ext = cur.path.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
    if !crate::picfix::is_picture(&ext) {
        app.toast("Only JPEG, PNG, WebP, BMP, GIF and TIFF pictures turn");
        return;
    }
    let Some((tools, prog)) = crate::drives::Tools::current().and_then(|t| crate::picfix::magick(&t).map(|p| (t, p))) else {
        app.toast("Turning pictures needs ImageMagick");
        return;
    };
    // A turn of a picture just turned: its original is in the trash already, and one
    // Undo goes back to it.
    if matches!(app.undo.last(), Some(crate::app::UndoEntry { kind: crate::app::UndoKind::Replaced(p), .. }) if *p == [cur.path.clone()]) {
        st.kept.lock().unwrap_or_else(|e| e.into_inner()).insert(cur.path.clone());
    }
    let (path, kept, tx) = (cur.path.clone(), st.kept.clone(), st.stat.0.clone());
    st.turning.push(path.clone());
    app.errands.run(ctx, move || {
        let (r, again) = turn_once(&tools, prog, &path, right, &kept);
        let _ = tx.send((path.clone(), std::fs::metadata(&path).and_then(|m| m.modified()).ok()));
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match r {
            Ok(()) => crate::errands::Done {
                undo: (!again).then(|| crate::app::UndoEntry { label: format!("Rotate {name}"), kind: crate::app::UndoKind::Replaced(vec![path.clone()]) }),
                changed: vec![path],
                ..Default::default()
            },
            Err(e) => crate::errands::Done::say(format!("Couldn't turn {name}: {e}")),
        }
    });
}

/// One quarter turn of `path`; the original goes to the trash only when `kept` has none
/// of it there yet. Returns how it went, and whether one was there already.
pub fn turn_once(tools: &crate::drives::Tools, prog: &str, path: &std::path::Path, right: bool, kept: &Mutex<HashSet<PathBuf>>) -> (Result<(), String>, bool) {
    let again = kept.lock().unwrap_or_else(|e| e.into_inner()).contains(path);
    let r = crate::picfix::rotate(tools, prog, path, right, again);
    if r.is_ok() {
        kept.lock().unwrap_or_else(|e| e.into_inner()).insert(path.to_path_buf());
    }
    (r, again)
}

/// Toggle `tag` on `path` by what the file itself has (the tag cache may not have
/// caught up with it yet).
pub fn toggle_tag(path: &std::path::Path, tag: &str) -> Result<(), String> {
    let has = crate::tags::read(path).iter().any(|x| x.eq_ignore_ascii_case(tag));
    crate::tags::toggle(&[path.to_path_buf()], tag, !has)
}

/// Ctrl+Z in the viewer: the latest change made from it taken back. A picture sent to
/// the trash comes back where it was and shows; a turned one shows as it was.
fn undo(app: &mut App, st: &mut State, now: f64) {
    if app.undo.len() <= st.undo_mark {
        app.toast("Nothing to undo here");
        return;
    }
    let (paths, trashed) = match app.undo.last().map(|u| &u.kind) {
        Some(crate::app::UndoKind::Trashed(p)) => (p.clone(), true),
        Some(crate::app::UndoKind::Replaced(p)) => (p.clone(), false),
        _ => (vec![], false),
    };
    app.undo_last();
    for p in paths {
        if trashed {
            if let Some(k) = st.trashed.iter().rposition(|(_, x)| x.path == p) {
                let (i, pic) = st.trashed.remove(k);
                let i = i.min(st.pics.len());
                st.pics.insert(i, pic);
                st.at = i;
                st.from = None;
                st.zoom = 1.0;
                st.pan = Vec2::ZERO;
                st.stirred = now;
            }
        } else {
            // The original is back from the trash: its own time, read off this thread.
            st.kept.lock().unwrap_or_else(|e| e.into_inner()).remove(&p);
            let tx = st.stat.0.clone();
            std::thread::spawn(move || {
                let m = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
                let _ = tx.send((p, m));
            });
        }
    }
}

/// Take in the new times of pictures changed: shown again from their new contents.
fn take_stats(app: &mut App, st: &mut State) {
    while let Ok((path, m)) = st.stat.1.try_recv() {
        app.thumbs.invalidate(&path);
        for p in st.pics.iter_mut().chain(st.pin.iter_mut()).filter(|p| p.path == path) {
            p.modified = m;
        }
        if let Some(i) = st.turning.iter().position(|t| *t == path) {
            st.turning.remove(i);
        }
        st.anim = None;
    }
}

/// The strip's tags: the colours, then the words in use and the shown picture's own.
fn tag_names(st: &State) -> Vec<String> {
    let mut names: Vec<String> = crate::tags::COLORS.iter().map(|(n, _)| n.to_string()).collect();
    if st.strip.is_none() {
        return names;
    }
    let mut words = crate::tags::known();
    if let Some(cur) = st.current() {
        for t in crate::tags::get(&cur.path, |t| t.to_vec()).unwrap_or_default() {
            if crate::tags::color_of(&t).is_none() && !words.iter().any(|w| w.eq_ignore_ascii_case(&t)) {
                words.push(t);
            }
        }
    }
    words.sort_by_key(|w| w.to_lowercase());
    names.extend(words);
    names
}

/// The name pill: a pin before a pinned picture's name, the colour labels as dots and the
/// other tags after it, then where it is in the folder; centred on `mid` above `bottom`.
#[allow(clippy::too_many_arguments)]
fn name_pill(ui: &egui::Ui, mid: f32, bottom: f32, room: f32, name: &str, tags: &[String], pos: Option<&str>, pinned: bool, playing: bool, fs: f32, a: f32) -> Rect {
    let dim = Color32::from_gray(160).gamma_multiply(a);
    let dots: Vec<Color32> = tags.iter().filter_map(|t| crate::tags::color_of(t)).collect();
    let words: Vec<&str> = tags.iter().filter(|t| crate::tags::color_of(t).is_none()).map(String::as_str).collect();
    let words = (!words.is_empty()).then(|| crate::listing::elided(ui, &words.join(", "), FontId::proportional(fs - 1.5), (room * 0.3).max(40.0), dim));
    let pos = pos.map(|p| ui.painter().layout_no_wrap(p.to_string(), FontId::proportional(fs - 1.5), dim));
    let extra = if dots.is_empty() { 0.0 } else { dots.len() as f32 * 12.0 + 8.0 } + words.as_ref().map_or(0.0, |w| w.size().x + 12.0) + pos.as_ref().map_or(0.0, |p| p.size().x + 16.0) + if pinned { 20.0 } else { 0.0 };
    let name = crate::listing::elided(ui, name, FontId::proportional(fs), (room - 100.0 - extra).max(60.0), Color32::from_gray(235).gamma_multiply(a));
    let w = name.size().x + extra;
    let h = name.size().y.max(pos.as_ref().map_or(0.0, |p| p.size().y)) + 12.0;
    let pill = Rect::from_center_size(pos2(mid, bottom - h / 2.0), vec2(w + 28.0, h));
    let p = ui.painter();
    p.rect_filled(pill, CornerRadius::same((h / 2.0) as u8), Color32::from_black_alpha((150.0 * a) as u8));
    let mut x = pill.left() + 14.0;
    let cy = pill.center().y;
    if pinned {
        crate::theme::paint_glyph(p, Rect::from_center_size(pos2(x + 6.0, cy), Vec2::splat(12.0)), Glyph::Pin, dim);
        x += 20.0;
    }
    let nw = name.size().x;
    p.galley(pos2(x, cy - name.size().y / 2.0), name, Color32::WHITE);
    x += nw;
    if !dots.is_empty() {
        x += 8.0;
        for c in &dots {
            p.circle_filled(pos2(x + 6.0, cy), 4.5, c.gamma_multiply(a));
            x += 12.0;
        }
    }
    if let Some(g) = words {
        x += 12.0;
        let gw = g.size().x;
        p.galley(pos2(x, cy - g.size().y / 2.0), g, Color32::WHITE);
        x += gw;
    }
    if let Some(g) = pos {
        x += 16.0;
        p.galley(pos2(x, cy - g.size().y / 2.0), g, Color32::WHITE);
    }
    if playing {
        // A small play mark: the slideshow is on.
        let c = pos2(pill.right() + 14.0, cy);
        p.add(egui::Shape::convex_polygon(vec![c + vec2(-4.0, -5.0), c + vec2(5.0, 0.0), c + vec2(-4.0, 5.0)], Color32::from_gray(200).gamma_multiply(a), Stroke::NONE));
    }
    pill
}

/// The tag strip above `bottom`: a chip for every tag, lit where the picture has it, the
/// one picked by the arrows outlined, New Tag last; rows as many as the width needs.
/// Returns its top, and a chip clicked.
#[allow(clippy::too_many_arguments)]
fn strip(ui: &egui::Ui, st: &State, names: &[String], have: &[String], r: Rect, bottom: f32, fs: f32, pal: &crate::theme::Palette) -> (f32, Option<usize>) {
    let at = st.strip.as_ref().map_or(0, |s| s.at);
    let font = FontId::proportional(fs - 1.0);
    let h = (fs + 12.0).round();
    let chips: Vec<(Option<std::sync::Arc<egui::Galley>>, Option<Color32>, f32)> = names
        .iter()
        .map(|n| match crate::tags::color_of(n) {
            Some(c) => (None, Some(c), h),
            None => {
                let g = crate::listing::elided(ui, n, font.clone(), 180.0, Color32::from_gray(235));
                let w = g.size().x + 24.0;
                (Some(g), None, w)
            }
        })
        .chain(std::iter::once({
            let g = ui.painter().layout_no_wrap("New Tag…".into(), font.clone(), Color32::from_gray(200));
            let w = g.size().x + 24.0;
            (Some(g), None, w)
        }))
        .collect();
    // Rows from the bottom up, each centred.
    let most = (r.width() - 60.0).max(h * 2.0);
    let mut rows: Vec<Vec<usize>> = vec![vec![]];
    let mut used = 0.0;
    for (i, c) in chips.iter().enumerate() {
        if used + c.2 > most && !rows.last().is_some_and(|x| x.is_empty()) {
            rows.push(vec![]);
            used = 0.0;
        }
        used += c.2 + 6.0;
        if let Some(row) = rows.last_mut() {
            row.push(i);
        }
    }
    let mut clicked = None;
    let n = rows.len();
    let top = bottom - n as f32 * (h + 6.0) + 6.0;
    for (k, row) in rows.iter().enumerate() {
        let y = top + k as f32 * (h + 6.0);
        let w: f32 = row.iter().map(|&i| chips[i].2 + 6.0).sum::<f32>() - 6.0;
        let mut x = r.center().x - w / 2.0;
        for &i in row {
            let (g, col, cw) = &chips[i];
            let cr = Rect::from_min_size(pos2(x, y), vec2(*cw, h));
            x += cw + 6.0;
            let on = names.get(i).is_some_and(|n| have.iter().any(|t| t.eq_ignore_ascii_case(n)));
            let resp = ui.interact(cr, Id::new(("viewer tag", i)), Sense::click());
            crate::ui::note_control(ui, cr, "viewer tag");
            let fill = if on { pal.accent.gamma_multiply(0.85) } else if resp.hovered() { Color32::from_black_alpha(140) } else { Color32::from_black_alpha(110) };
            let p = ui.painter();
            p.rect_filled(cr, CornerRadius::same((h / 2.0) as u8), fill);
            if i == at {
                p.rect_stroke(cr, CornerRadius::same((h / 2.0) as u8), Stroke::new(1.5, Color32::from_gray(235)), egui::StrokeKind::Inside);
            }
            match (g, col) {
                (_, Some(c)) => {
                    p.circle_filled(cr.center(), h * 0.22, *c);
                    if on {
                        p.circle_stroke(cr.center(), h * 0.22 + 2.0, Stroke::new(1.5, Color32::WHITE));
                    }
                }
                (Some(g), None) => {
                    p.galley(pos2(cr.left() + 12.0, cr.center().y - g.size().y / 2.0), g.clone(), Color32::WHITE);
                }
                _ => {}
            }
            let tip = names.get(i).map_or("New tag", String::as_str);
            if resp.on_hover_text(tip).clicked() {
                clicked = Some(i);
            }
        }
    }
    (top, clicked)
}

enum Field {
    Typing,
    Done,
    Cancel,
}

/// The new tag's name, typed in a field over the strip. Returns what became of it and
/// the height it took.
fn tag_field(ui: &mut egui::Ui, text: &mut String, r: Rect, bottom: f32, fs: f32, pal: &crate::theme::Palette) -> (Field, f32) {
    let h = (fs + 14.0).round();
    let fr = Rect::from_center_size(pos2(r.center().x, bottom - h / 2.0), vec2(260.0f32.min(r.width() - 40.0), h));
    ui.painter().rect_filled(fr, CornerRadius::same((h / 2.0) as u8), Color32::from_black_alpha(170));
    let mut fui = ui.new_child(egui::UiBuilder::new().max_rect(fr.shrink2(vec2(14.0, 3.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    let te = egui::TextEdit::singleline(text).id(Id::new("viewer new tag")).hint_text(egui::RichText::new("New tag").color(Color32::from_gray(150))).text_color(Color32::WHITE).frame(crate::ui::bare_edit()).desired_width(fr.width() - 28.0).font(FontId::proportional(fs));
    let resp = fui.add(te);
    crate::ui::note_control(ui, fr, "viewer new tag");
    let _ = pal;
    let (enter, esc) = ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
    if !resp.has_focus() && !resp.lost_focus() {
        resp.request_focus();
    }
    let out = if resp.lost_focus() && enter {
        Field::Done
    } else if esc || resp.lost_focus() {
        Field::Cancel
    } else {
        Field::Typing
    };
    (out, h + 8.0)
}


/// Test views of the viewer's state.
#[cfg(test)]
impl State {
    pub fn trashed_here(&self) -> usize {
        self.trashed.len()
    }
}

/// Leave the listing's cursor on the picture last shown.
fn close(app: &mut App, st: &State) {
    let Some(p) = st.current().map(|p| p.path.clone()) else { return };
    if let Some(t) = app.panels.get_mut(&st.panel).map(|p| p.tab_mut()) {
        if let Some(i) = t.entries.iter().position(|e| e.path == p) {
            t.select_only(i as u32);
            t.scroll_to_cursor = true;
        }
    }
}
