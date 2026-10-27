//! Thumbnail & preview image loading on worker threads, with a bounded texture cache.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use egui::{self, ColorImage, TextureHandle, TextureOptions};

pub fn is_image_ext(ext: &str) -> bool {
    matches!(ext, "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico")
}

/// Whether a thumbnail can be made for this item: built-in image formats, SVG (own
/// renderer), photos an installed tool decodes (HEIC, AVIF, TIFF, JPEG XL, RAW) or
/// anything an installed desktop thumbnailer handles (video, PDF, office documents,
/// audio covers…).
pub fn can_thumb(e: &crate::fsx::Entry) -> bool {
    if e.is_dir || e.special {
        return false;
    }
    let ext = e.ext();
    is_image_ext(ext) || ext == "svg" || crate::fontview::is_font(ext) || crate::drives::Tools::current().is_some_and(|t| crate::extpic::can(&t, ext)) || crate::thumbcache::thumbnailer_for(&crate::mime::mime_db().for_name(&e.name, false, false)).is_some()
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Key {
    path: PathBuf,
    size: u32,
    mtime: Option<SystemTime>,
}

pub struct Loaded {
    pub tex: TextureHandle,
    pub at: Instant,
    /// Original pixel dimensions.
    pub dims: [u32; 2],
    /// The picture's colour at a glance (`tone`).
    pub tone: egui::Color32,
    /// The colours it is mostly made of, with their shares (`colours::palette`).
    pub palette: Arc<[(egui::Color32, f32)]>,
    /// Its colours by place, for the glow behind it (`colours::glow`).
    pub glow: [egui::Color32; 16],
}

enum Slot {
    Pending,
    Failed,
    Ready(Loaded),
}

struct Job {
    key: Key,
}

type Colours = (egui::Color32, Arc<[(egui::Color32, f32)]>, [egui::Color32; 16]);

struct Done {
    key: Key,
    result: Option<(ColorImage, [u32; 2], Colours)>,
}

pub struct Thumbs {
    slots: HashMap<Key, Slot>,
    /// Latest ready key per path, for showing a different size while loading.
    ready_by_path: HashMap<PathBuf, Key>,
    failed: std::collections::HashSet<PathBuf>,
    order: VecDeque<Key>,
    queue: Arc<Mutex<VecDeque<Job>>>,
    rx: Receiver<Done>,
    wake: Sender<()>,
    pub budget_bytes: usize,
    used_bytes: usize,
}

impl Thumbs {
    pub fn new(ctx: egui::Context, workers: usize) -> Thumbs {
        let (tx, rx) = channel::<Done>();
        let queue: Arc<Mutex<VecDeque<Job>>> = Arc::new(Mutex::new(VecDeque::new()));
        let (wake_tx, wake_rx) = channel::<()>();
        let wake_rx = Arc::new(Mutex::new(wake_rx));
        // The tools photos are decoded with (fakes under test, set on this thread).
        let tools = crate::drives::Tools::current();
        for _ in 0..workers.max(1) {
            let q = queue.clone();
            let tx = tx.clone();
            let ctx = ctx.clone();
            let wake_rx = wake_rx.clone();
            let tools = tools.clone();
            std::thread::spawn(move || {
                loop {
                    // LIFO: the most recently requested (visible) items first.
                    let job = q.lock().unwrap().pop_back();
                    match job {
                        Some(j) => {
                            let result = decode(&j.key.path, j.key.size, j.key.mtime, tools.as_ref()).map(|(img, dims)| {
                                let colours = (tone(&img), crate::colours::palette(&img, 5).into(), crate::colours::glow(&img));
                                (img, dims, colours)
                            });
                            if tx.send(Done { key: j.key, result }).is_err() {
                                return;
                            }
                            ctx.request_repaint();
                        }
                        None => {
                            if wake_rx.lock().unwrap().recv().is_err() {
                                return;
                            }
                        }
                    }
                }
            });
        }
        Thumbs { slots: HashMap::new(), ready_by_path: HashMap::new(), failed: Default::default(), order: VecDeque::new(), queue, rx, wake: wake_tx, budget_bytes: 256 << 20, used_bytes: 0 }
    }

    /// Request an image at a size bucket. Returns it when ready.
    pub fn get(&mut self, path: &Path, mtime: Option<SystemTime>, want: f32) -> Option<&Loaded> {
        let size = bucket(want);
        let key = Key { path: path.to_path_buf(), size, mtime };
        if !self.slots.contains_key(&key) {
            self.slots.insert(key.clone(), Slot::Pending);
            let mut q = self.queue.lock().unwrap();
            q.push_back(Job { key: key.clone() });
            // Don't let stale requests pile up while scrolling fast.
            while q.len() > 512 {
                if let Some(old) = q.pop_front() {
                    self.slots.remove(&old.key);
                }
            }
            drop(q);
            let _ = self.wake.send(());
        }
        if let Some(Slot::Ready(_)) = self.slots.get(&key) {
            if let Some(Slot::Ready(l)) = self.slots.get(&key) {
                return Some(l);
            }
        }
        // Fall back to another ready size of the same file while loading.
        let alt = self.ready_by_path.get(path).filter(|k| k.mtime == mtime)?;
        match self.slots.get(alt) {
            Some(Slot::Ready(l)) => Some(l),
            _ => None,
        }
    }

    /// The size of a picture already decoded at some size.
    pub fn dims(&self, path: &Path) -> Option<[u32; 2]> {
        match self.slots.get(self.ready_by_path.get(path)?) {
            Some(Slot::Ready(l)) => Some(l.dims),
            _ => None,
        }
    }

    pub fn failed(&self, path: &Path) -> bool {
        self.failed.contains(path)
    }

    /// Upload finished decodes (a few per frame to keep frames smooth). True when any landed.
    pub fn pump(&mut self, ctx: &egui::Context) -> bool {
        let mut n = 0;
        while let Ok(d) = self.rx.try_recv() {
            match d.result {
                Some((img, dims, (tone, palette, glow))) => {
                    let bytes = img.width() * img.height() * 4;
                    let tex = ctx.load_texture(d.key.path.to_string_lossy(), img, TextureOptions::LINEAR);
                    self.used_bytes += bytes;
                    self.order.push_back(d.key.clone());
                    self.ready_by_path.insert(d.key.path.clone(), d.key.clone());
                    self.slots.insert(d.key, Slot::Ready(Loaded { tex, at: Instant::now(), dims, tone, palette, glow }));
                }
                None => {
                    self.failed.insert(d.key.path.clone());
                    self.slots.insert(d.key, Slot::Failed);
                }
            }
            n += 1;
            if n >= 24 {
                ctx.request_repaint();
                break;
            }
        }
        while self.used_bytes > self.budget_bytes {
            let Some(k) = self.order.pop_front() else { break };
            if self.ready_by_path.get(&k.path) == Some(&k) {
                self.ready_by_path.remove(&k.path);
            }
            if let Some(Slot::Ready(l)) = self.slots.remove(&k) {
                let [w, h] = l.tex.size();
                self.used_bytes = self.used_bytes.saturating_sub(w * h * 4);
            }
        }
        n > 0
    }

    pub fn invalidate(&mut self, path: &Path) {
        self.slots.retain(|k, _| k.path != path);
        self.ready_by_path.remove(path);
        self.failed.remove(path);
    }
}

pub fn bucket(want: f32) -> u32 {
    for b in [64, 128, 256, 512, 1024, 2048, 4096] {
        if want <= b as f32 {
            return b;
        }
    }
    4096
}

/// Thumbnail pipeline for one request: the shared cache, then the built-in decoders (and
/// SVG renderer), then an installed tool or thumbnailer. New thumbnails up to 1024 px go
/// back to the shared cache; failures are remembered (until the file changes) so nothing
/// is retried every frame.
fn decode(path: &Path, max: u32, mtime: Option<SystemTime>, tools: Option<&crate::drives::Tools>) -> Option<(ColorImage, [u32; 2])> {
    // A folder asked for has its cover (only the Stack view asks).
    if std::fs::metadata(path).is_ok_and(|m| m.is_dir()) {
        return folder_cover(path, max, tools).map(|(img, dims)| (to_color(img), dims));
    }
    make(path, max, mtime, tools).map(|(img, dims)| (to_color(img), dims))
}

/// A picture small (the shared cache's normal thumbnail when there is one), for working
/// out its colours off the thumbnail workers.
pub fn small(path: &Path, mtime: Option<SystemTime>, tools: Option<&crate::drives::Tools>) -> Option<ColorImage> {
    make(path, 128, mtime, tools).map(|(img, _)| to_color(img))
}

fn make(path: &Path, max: u32, mtime: Option<SystemTime>, tools: Option<&crate::drives::Tools>) -> Option<(crate::imgdec::Rgba, [u32; 2])> {
    use crate::thumbcache as tc;
    // A font draws its own: "Ag" in itself (quick, and not the desktop's way of showing it).
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if crate::fontview::is_font(&ext) {
        return crate::fontview::tile(path, max.min(512)).map(|img| {
            let d = [img.w, img.h];
            (img, d)
        });
    }
    let cacheable = max <= 1024;
    if cacheable {
        if let Some((img, dims)) = tc::lookup(path, mtime, max) {
            let dims = dims.unwrap_or([img.w, img.h]);
            return Some((img, dims));
        }
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let flavor_px = tc::flavor(max).1;
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let made: Option<(crate::imgdec::Rgba, [u32; 2])> = if is_image_ext(&ext) {
        crate::imgdec::load(path, if cacheable { flavor_px } else { max })
    } else if ext == "svg" {
        svg_thumb(path, if cacheable { flavor_px } else { max })
    } else if tc::has_failed(path, mtime) {
        None
    } else if let Some(art) = tools.filter(|t| crate::musictags::holds_cover(&ext) && t.has("ffmpeg")).and_then(|t| album_art(t, path, flavor_px)) {
        Some(art)
    } else {
        let mime = crate::mime::mime_db().for_name(&name, false, false);
        let thumbnailer = tc::thumbnailer_for(&mime);
        // A photo a tool decodes at the size shown; the small ones a desktop thumbnailer
        // makes, when there is one, are quicker.
        let tool = tools.filter(|t| crate::extpic::can(t, &ext) && (max > 256 || thumbnailer.is_none()));
        if tool.is_none() && thumbnailer.is_none() {
            return None;
        }
        let made = tool.and_then(|t| crate::extpic::load(t, path, if cacheable { flavor_px } else { max })).or_else(|| {
            let img = tc::run_external(thumbnailer?, path, flavor_px.min(1024), std::time::Duration::from_secs(12))?;
            let img = if img.w > flavor_px || img.h > flavor_px { crate::imgdec::downscale(&img, flavor_px) } else { img };
            let d = [img.w, img.h];
            Some((img, d))
        });
        if made.is_none() {
            tc::mark_failed(path, mtime);
        }
        made
    };
    let (img, dims) = made?;
    if cacheable {
        tc::store(path, mtime, max, &img, meta.len(), Some(dims));
    }
    // The cache flavour can be larger than this request: scale to what was asked.
    let img = if img.w > max || img.h > max { crate::imgdec::downscale(&img, max) } else { img };
    Some((img, dims))
}

/// A sound file's own cover, the picture stream ffmpeg finds attached to it.
fn album_art(t: &crate::drives::Tools, path: &Path, px: u32) -> Option<(crate::imgdec::Rgba, [u32; 2])> {
    let mut arg = std::ffi::OsString::from("file:");
    arg.push(path);
    let mut c = crate::media::tool_cmd(t.cmd("ffmpeg"));
    c.args(["-v", "error", "-i"]).arg(arg).args(["-map", "0:v:0", "-frames:v", "1", "-f", "image2pipe", "-c:v", "png", "-"]);
    let out = crate::media::output_capped(c, std::time::Duration::from_secs(8), 48 << 20, &|| false)?;
    let img = crate::imgdec::decode_bytes(&out)?;
    let dims = [img.w, img.h];
    let img = if img.w > px || img.h > px { crate::imgdec::downscale(&img, px) } else { img };
    Some((img, dims))
}

/// A folder's cover: the picture it keeps for one (cover, folder, front, album art), else
/// its first four pictures by name in a square of four, else its only few one alone.
/// Made here on the worker, kept in memory only: the shared cache is for files.
fn folder_cover(dir: &Path, max: u32, tools: Option<&crate::drives::Tools>) -> Option<(crate::imgdec::Rgba, [u32; 2])> {
    let mut own: Option<(u8, std::path::PathBuf)> = None;
    let mut pics: Vec<(String, std::path::PathBuf)> = vec![];
    // A folder of a hundred thousand files has its cover among the first few thousand or
    // none worth the wait.
    for e in std::fs::read_dir(dir).ok()?.flatten().take(4000) {
        let name = e.file_name().to_string_lossy().to_lowercase();
        let Some((stem, ext)) = name.rsplit_once('.') else { continue };
        if !matches!(ext, "jpg" | "jpeg" | "png" | "webp") || name.starts_with('.') || !e.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let rank = match stem {
            "cover" => 0,
            "folder" => 1,
            "front" => 2,
            "album" | "albumart" => 3,
            s if s.starts_with("albumart") => 4,
            _ => 9,
        };
        if rank < 9 {
            if own.as_ref().is_none_or(|o| rank < o.0) {
                own = Some((rank, e.path()));
            }
        } else {
            pics.push((name, e.path()));
        }
    }
    let mtime = |p: &Path| std::fs::metadata(p).ok().and_then(|m| m.modified().ok());
    if let Some((_, p)) = own {
        let (img, _) = make(&p, max, mtime(&p), tools)?;
        return Some((square(&img, max), [max, max]));
    }
    let mut ord: Vec<u32> = (0..pics.len() as u32).collect();
    crate::tab::merge_sort(&mut ord, &|a: &u32, b: &u32| crate::tab::natural_cmp(&pics[*a as usize].0, &pics[*b as usize].0));
    if pics.len() < 4 {
        let p = &pics[*ord.first()? as usize].1;
        let (img, _) = make(p, max, mtime(p), tools)?;
        return Some((square(&img, max), [max, max]));
    }
    // Four squares with a two-pixel seam between them, left clear: the card shows through.
    let gap = 2;
    let cell = (max.saturating_sub(gap) / 2).max(8);
    let side = cell * 2 + gap;
    let mut out = crate::imgdec::Rgba { w: side, h: side, px: vec![0; (side * side * 4) as usize] };
    let mut placed = 0;
    for p in ord.iter().take(8).map(|&k| &pics[k as usize].1) {
        if placed == 4 {
            break;
        }
        let Some((img, _)) = make(p, cell.max(64), mtime(p), tools) else { continue };
        let sq = square(&img, cell);
        let (ox, oy) = ((placed % 2) * (cell + gap), (placed / 2) * (cell + gap));
        for y in 0..cell {
            let from = (y * cell * 4) as usize;
            let to = (((oy + y) * side + ox) * 4) as usize;
            out.px[to..to + (cell * 4) as usize].copy_from_slice(&sq.px[from..from + (cell * 4) as usize]);
        }
        placed += 1;
    }
    (placed == 4).then_some((out, [side, side]))
}

/// The middle square of `img`, scaled to `side` × `side` (each pixel the average of the
/// ones it covers, or the nearest when growing).
fn square(img: &crate::imgdec::Rgba, side: u32) -> crate::imgdec::Rgba {
    let s = img.w.min(img.h).max(1);
    let (x0, y0) = ((img.w - s) / 2, (img.h - s) / 2);
    let mut px = vec![0u8; (side * side * 4) as usize];
    for ty in 0..side {
        let (sy0, sy1) = (y0 + ty * s / side, (y0 + ((ty + 1) * s / side).max(ty * s / side + 1)).min(y0 + s));
        for tx in 0..side {
            let (sx0, sx1) = (x0 + tx * s / side, (x0 + ((tx + 1) * s / side).max(tx * s / side + 1)).min(x0 + s));
            let mut acc = [0u32; 4];
            let mut n = 0;
            for y in sy0..sy1.max(sy0 + 1).min(img.h) {
                for x in sx0..sx1.max(sx0 + 1).min(img.w) {
                    let i = ((y * img.w + x) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += img.px[i + c] as u32;
                    }
                    n += 1;
                }
            }
            let o = ((ty * side + tx) * 4) as usize;
            for c in 0..4 {
                px[o + c] = (acc[c] / n.max(1)) as u8;
            }
        }
    }
    crate::imgdec::Rgba { w: side, h: side, px }
}

/// A picture's colour at a glance: its pixels averaged with the vivid ones counting
/// most, so a red flower on grey gives red and not brown, then a little brighter and more
/// saturated, the way a glow of it reads.
pub fn tone(img: &ColorImage) -> egui::Color32 {
    let [w, h] = img.size;
    if w == 0 || h == 0 {
        return egui::Color32::TRANSPARENT;
    }
    let (mut acc, mut total) = ([0f32; 3], 0f32);
    let steps = 24;
    for sy in 0..steps {
        for sx in 0..steps {
            let p = img.pixels[(sy * h / steps) * w + sx * w / steps];
            let [r, g, b, a] = p.to_srgba_unmultiplied().map(|c| c as f32 / 255.0);
            let (hi, lo) = (r.max(g).max(b), r.min(g).min(b));
            let sat = if hi > 0.0 { (hi - lo) / hi } else { 0.0 };
            let wgt = a * (0.08 + sat * sat * hi);
            acc[0] += r * wgt;
            acc[1] += g * wgt;
            acc[2] += b * wgt;
            total += wgt;
        }
    }
    if total <= 0.0 {
        return egui::Color32::TRANSPARENT;
    }
    let c = acc.map(|v| v / total);
    let grey = (c[0] + c[1] + c[2]) / 3.0;
    let c = c.map(|v| (grey + (v - grey) * 1.35).clamp(0.0, 1.0));
    let lift = 0.15 * (1.0 - c[0].max(c[1]).max(c[2]));
    egui::Color32::from_rgb(((c[0] + lift) * 255.0) as u8, ((c[1] + lift) * 255.0) as u8, ((c[2] + lift) * 255.0) as u8)
}

fn to_color(img: crate::imgdec::Rgba) -> ColorImage {
    ColorImage::from_rgba_unmultiplied([img.w as usize, img.h as usize], &img.px)
}

/// SVG through Maia's own renderer (premultiplied output, unpremultiplied here).
fn svg_thumb(path: &Path, px: u32) -> Option<(crate::imgdec::Rgba, [u32; 2])> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > 16 << 20 {
        return None;
    }
    let mut data = std::fs::read(path).ok()?;
    data.truncate(16 << 20);
    let (w, h, mut px_data) = crate::svg::render(&data, px)?;
    for p in px_data.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    // Its dimensions are the document's own (64 × 48), not the drawing's (2048 × 1536).
    let dims = crate::svg::size(&data).unwrap_or([w, h]);
    Some((crate::imgdec::Rgba { w, h, px: px_data }, dims))
}
