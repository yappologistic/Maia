//! A picture's colours: the five it is mostly made of, with their shares (the preview's
//! chips), a four by four grid of its colours by place (the glow behind it), and one
//! number to sort pictures by colour, red through violet, then the greys.
//!
//! The thumbnail workers work out the first two with every picture they decode. The sort
//! needs a colour for every picture in a folder, shown or not, so it has workers of its
//! own that read each picture's small thumbnail (the shared cache's, when there is one)
//! and keep the answer on disk by path, size and time changed.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, RwLock};
use std::time::UNIX_EPOCH;

use egui::{Color32, ColorImage, Rect, pos2};

use crate::fsx::Entry;

// ------------------------------------------------------------------ reading a picture

fn samples(img: &ColorImage, steps: usize) -> Vec<[f32; 3]> {
    let [w, h] = img.size;
    let mut v = Vec::with_capacity(steps * steps);
    if w == 0 || h == 0 {
        return v;
    }
    for sy in 0..steps {
        for sx in 0..steps {
            let p = img.pixels[(sy * h / steps).min(h - 1) * w + (sx * w / steps).min(w - 1)];
            let [r, g, b, a] = p.to_srgba_unmultiplied();
            if a >= 128 {
                v.push([r as f32, g as f32, b as f32]);
            }
        }
    }
    v
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    // Green counts most and blue least, roughly as the eye weighs them.
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    2.0 * d[0] * d[0] + 4.0 * d[1] * d[1] + 3.0 * d[2] * d[2]
}

/// The colours a picture is mostly made of, largest share first (at most `k`; fewer when
/// some are all but the same).
pub fn palette(img: &ColorImage, k: usize) -> Vec<(Color32, f32)> {
    let px = samples(img, 40);
    if px.is_empty() || k == 0 {
        return vec![];
    }
    // Seeds spread out: the first pixel, then each time the one farthest from all chosen.
    let mut centres = vec![px[px.len() / 2]];
    while centres.len() < k {
        let far = px.iter().copied().max_by(|a, b| {
            let da = centres.iter().map(|c| dist(*a, *c)).fold(f32::MAX, f32::min);
            let db = centres.iter().map(|c| dist(*b, *c)).fold(f32::MAX, f32::min);
            da.total_cmp(&db)
        });
        match far {
            Some(f) if centres.iter().all(|c| dist(f, *c) > 1.0) => centres.push(f),
            _ => break,
        }
    }
    let mut counts = vec![0usize; centres.len()];
    for _ in 0..8 {
        let mut sums = vec![[0f32; 3]; centres.len()];
        counts.iter_mut().for_each(|c| *c = 0);
        for p in &px {
            let i = (0..centres.len()).min_by(|&a, &b| dist(*p, centres[a]).total_cmp(&dist(*p, centres[b]))).unwrap_or(0);
            counts[i] += 1;
            for c in 0..3 {
                sums[i][c] += p[c];
            }
        }
        for (i, s) in sums.iter().enumerate() {
            if counts[i] > 0 {
                centres[i] = s.map(|v| v / counts[i] as f32);
            }
        }
    }
    let total = px.len() as f32;
    let mut out: Vec<([f32; 3], f32)> = vec![];
    for (c, n) in centres.into_iter().zip(counts) {
        if n == 0 {
            continue;
        }
        // Two that look alike are one.
        match out.iter_mut().find(|(o, _)| dist(*o, c) < 9.0 * 144.0) {
            Some(o) => o.1 += n as f32 / total,
            None => out.push((c, n as f32 / total)),
        }
    }
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out.into_iter().map(|(c, s)| (Color32::from_rgb(c[0] as u8, c[1] as u8, c[2] as u8), s)).collect()
}

/// Sixteen colours by place, four rows of four, a little brighter and more saturated:
/// what the glow behind a picture is drawn from.
pub fn glow(img: &ColorImage) -> [Color32; 16] {
    let [w, h] = img.size;
    let mut out = [Color32::TRANSPARENT; 16];
    if w == 0 || h == 0 {
        return out;
    }
    for gy in 0..4 {
        for gx in 0..4 {
            let (mut acc, mut n) = ([0f32; 3], 0f32);
            for sy in 0..6 {
                for sx in 0..6 {
                    let x = ((gx * 6 + sx) * w / 24).min(w - 1);
                    let y = ((gy * 6 + sy) * h / 24).min(h - 1);
                    let [r, g, b, a] = img.pixels[y * w + x].to_srgba_unmultiplied();
                    if a >= 64 {
                        acc[0] += r as f32;
                        acc[1] += g as f32;
                        acc[2] += b as f32;
                        n += 1.0;
                    }
                }
            }
            if n > 0.0 {
                let c = acc.map(|v| v / n / 255.0);
                let grey = (c[0] + c[1] + c[2]) / 3.0;
                let c = c.map(|v| (grey + (v - grey) * 1.3).clamp(0.0, 1.0));
                out[gy * 4 + gx] = Color32::from_rgb((c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8);
            }
        }
    }
    out
}

/// Hue (degrees), saturation and lightness, each 0..1 but the hue.
pub fn hsl(c: Color32) -> (f32, f32, f32) {
    let [r, g, b] = [c.r(), c.g(), c.b()].map(|v| v as f32 / 255.0);
    let (hi, lo) = (r.max(g).max(b), r.min(g).min(b));
    let l = (hi + lo) / 2.0;
    if hi - lo < 1e-4 {
        return (0.0, 0.0, l);
    }
    let d = hi - lo;
    let s = d / (1.0 - (2.0 * l - 1.0).abs()).max(1e-4);
    let h = if hi == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if hi == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, s.min(1.0), l)
}

/// The number a picture sorts by: its most colourful large colour's hue, red first (the
/// reds just short of 360° with the others), lighter before darker within a hue; a picture
/// with no colour to speak of after every coloured one, light to dark.
pub fn sort_number(palette: &[(Color32, f32)]) -> u64 {
    let best = palette.iter().map(|&(c, share)| (c, share, hsl(c))).filter(|(_, share, _)| *share >= 0.08).max_by(|a, b| {
        let score = |x: &(Color32, f32, (f32, f32, f32))| x.1.sqrt() * (x.2.1 * (1.0 - (x.2.2 - 0.5).abs() * 1.4)).max(0.0);
        score(a).total_cmp(&score(b))
    });
    match best {
        Some((_, _, (h, s, l))) if s * (1.0 - (l - 0.5).abs() * 1.4) >= 0.14 => {
            let hue = (h + 15.0).rem_euclid(360.0);
            (hue * 10.0) as u64 * 1000 + ((1.0 - l) * 999.0) as u64
        }
        _ => {
            let l = palette.first().map_or(0.5, |p| hsl(p.0).2);
            (1u64 << 32) + ((1.0 - l) * 999.0) as u64
        }
    }
}

/// "#E9C46A".
pub fn hex(c: Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
}

// ------------------------------------------------------------------ the glow

/// A soft light of the picture's own colours around `pic`: its sixteen colours spread
/// over a larger rectangle and faded to nothing at the edge. One mesh, no texture.
pub fn paint_glow(p: &egui::Painter, pic: Rect, cells: &[Color32; 16], strength: f32) {
    if cells.iter().all(|c| c.a() == 0) || strength <= 0.0 || !pic.is_finite() || pic.width() < 2.0 || pic.height() < 2.0 {
        return;
    }
    let spread = (pic.width().min(pic.height()) * 0.35).clamp(24.0, 220.0);
    let outer = pic.expand(spread);
    // Six by six points: the outer ring clear, the inner four by four the picture's cells.
    let xs = [outer.left(), pic.left(), pic.left() + pic.width() / 3.0, pic.right() - pic.width() / 3.0, pic.right(), outer.right()];
    let ys = [outer.top(), pic.top(), pic.top() + pic.height() / 3.0, pic.bottom() - pic.height() / 3.0, pic.bottom(), outer.bottom()];
    let mut mesh = egui::Mesh::default();
    for (yi, &y) in ys.iter().enumerate() {
        for (xi, &x) in xs.iter().enumerate() {
            let inner = (1..=4).contains(&xi) && (1..=4).contains(&yi);
            let c = if inner { cells[(yi - 1) * 4 + (xi - 1)] } else { Color32::TRANSPARENT };
            let a = if inner { (strength * 255.0) as u8 } else { 0 };
            mesh.colored_vertex(pos2(x, y), Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a));
        }
    }
    for yi in 0..5u32 {
        for xi in 0..5u32 {
            let i = yi * 6 + xi;
            mesh.add_triangle(i, i + 1, i + 7);
            mesh.add_triangle(i, i + 7, i + 6);
        }
    }
    p.add(mesh);
}

/// How strong the glow is on a background: quieter on light ones, where colour shows more.
pub fn glow_strength(bg: Color32) -> f32 {
    let l = (bg.r() as f32 * 0.3 + bg.g() as f32 * 0.59 + bg.b() as f32 * 0.11) / 255.0;
    if l > 0.5 { 0.28 } else { 0.5 }
}

// ------------------------------------------------------------------ sorting by colour

/// Size and time changed: a kept answer holds while both do.
type Stamp = (u64, i64, u32);

struct Store {
    map: HashMap<PathBuf, u64>,
    pending: HashSet<PathBuf>,
    generation: u64,
}

static STORE: RwLock<Option<Store>> = RwLock::new(None);
static QUEUE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static FOCUS: RwLock<Vec<PathBuf>> = RwLock::new(Vec::new());

struct Work {
    todo: VecDeque<PathBuf>,
    tools: Option<crate::drives::Tools>,
    ctx: Option<egui::Context>,
    started: bool,
}

static WORK: Mutex<Work> = Mutex::new(Work { todo: VecDeque::new(), tools: None, ctx: None, started: false });
static WAKE: Condvar = Condvar::new();
/// Past this many answers in memory the store starts again.
const MOST: usize = 200_000;

/// What can have a colour to sort by: pictures Maia or an installed tool decodes.
pub fn sortable(e: &Entry) -> bool {
    !e.is_dir && !e.special && (crate::thumbs::is_image_ext(e.ext()) || crate::drives::Tools::current().is_some_and(|t| crate::extpic::can(&t, e.ext())))
}

/// The number `e` sorts by, None while it is read (it is then asked for) or when it is
/// no picture; either way it goes last.
pub fn sort_key(e: &Entry) -> Option<u64> {
    if !sortable(e) {
        return None;
    }
    {
        let g = STORE.read().unwrap_or_else(|x| x.into_inner());
        if let Some(s) = g.as_ref() {
            if let Some(k) = s.map.get(&e.path) {
                return (*k != u64::MAX).then_some(*k);
            }
            if s.pending.contains(&e.path) {
                return None;
            }
        }
    }
    let mut g = STORE.write().unwrap_or_else(|x| x.into_inner());
    g.get_or_insert_with(|| Store { map: HashMap::new(), pending: HashSet::new(), generation: 0 }).pending.insert(e.path.clone());
    QUEUE.lock().unwrap_or_else(|x| x.into_inner()).push(e.path.clone());
    None
}

pub fn generation() -> u64 {
    STORE.read().unwrap_or_else(|x| x.into_inner()).as_ref().map_or(0, |s| s.generation)
}

/// Whether any answers landed since the frame last asked.
pub fn changed() -> bool {
    static SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let g = generation();
    SEEN.swap(g, std::sync::atomic::Ordering::Relaxed) != g
}

pub fn invalidate(p: &Path) {
    if let Some(s) = STORE.write().unwrap_or_else(|x| x.into_inner()).as_mut() {
        if s.map.remove(p).is_some() {
            s.generation += 1;
        }
    }
}

fn wanted(p: &Path) -> bool {
    let f = FOCUS.read().unwrap_or_else(|x| x.into_inner());
    f.is_empty() || f.iter().any(|d| p.starts_with(d))
}

/// The folders shown: work for pictures elsewhere is dropped (asked again if shown again).
pub fn focus(dirs: &[PathBuf]) {
    if FOCUS.read().unwrap_or_else(|x| x.into_inner()).as_slice() == dirs {
        return;
    }
    *FOCUS.write().unwrap_or_else(|x| x.into_inner()) = dirs.to_vec();
    let mut dropped = vec![];
    WORK.lock().unwrap_or_else(|x| x.into_inner()).todo.retain(|p| {
        wanted(p) || {
            dropped.push(p.clone());
            false
        }
    });
    if let Some(s) = STORE.write().unwrap_or_else(|x| x.into_inner()).as_mut() {
        for p in &dropped {
            s.pending.remove(p);
        }
    }
}

/// What rows asked for this frame, to the workers (started on first use).
pub fn flush(ctx: &egui::Context) {
    let batch = std::mem::take(&mut *QUEUE.lock().unwrap_or_else(|x| x.into_inner()));
    if batch.is_empty() {
        return;
    }
    let mut w = WORK.lock().unwrap_or_else(|x| x.into_inner());
    w.tools = crate::drives::Tools::current();
    w.ctx = Some(ctx.clone());
    w.todo.extend(batch);
    if !w.started {
        w.started = true;
        for _ in 0..2 {
            let _ = std::thread::Builder::new().name("picture colours".into()).spawn(worker);
        }
    }
    WAKE.notify_all();
}

fn stamp(p: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(p).ok().filter(|m| m.is_file())?;
    let t = m.modified().ok()?;
    let (s, n) = match t.duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
        Err(e) => (-(e.duration().as_secs() as i64), 0),
    };
    Some((m.len(), s, n))
}

fn worker() {
    loop {
        let (p, tools, ctx) = {
            let mut w = WORK.lock().unwrap_or_else(|x| x.into_inner());
            loop {
                if let Some(p) = w.todo.pop_front() {
                    break (p, w.tools.clone(), w.ctx.clone());
                }
                w = WAKE.wait(w).unwrap_or_else(|x| x.into_inner());
            }
        };
        if !wanted(&p) {
            if let Some(s) = STORE.write().unwrap_or_else(|x| x.into_inner()).as_mut() {
                s.pending.remove(&p);
            }
            continue;
        }
        let st = stamp(&p);
        let key = st.and_then(|st| kept(&p, st)).or_else(|| {
            let mtime = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
            let k = crate::thumbs::small(&p, mtime, tools.as_ref()).map(|img| sort_number(&palette(&img, 5))).unwrap_or(u64::MAX);
            if let Some(st) = st {
                keep(&p, st, k);
            }
            Some(k)
        });
        {
            let mut g = STORE.write().unwrap_or_else(|x| x.into_inner());
            let s = g.get_or_insert_with(|| Store { map: HashMap::new(), pending: HashSet::new(), generation: 0 });
            if s.map.len() >= MOST {
                s.map.clear();
            }
            s.pending.remove(&p);
            s.map.insert(p, key.unwrap_or(u64::MAX));
            s.generation += 1;
        }
        if let Some(c) = ctx {
            c.request_repaint();
        }
    }
}

struct Kept {
    loaded: bool,
    map: HashMap<PathBuf, (Stamp, u64)>,
}

static KEPT: Mutex<Option<Kept>> = Mutex::new(None);

fn kept_path() -> PathBuf {
    #[cfg(test)]
    return std::env::temp_dir().join(format!("maia-test-colours-{}", std::process::id()));
    #[cfg(not(test))]
    dirs::cache_dir().unwrap_or_else(|| crate::fsx::home().join(".cache")).join("maia/picture-colours")
}

/// One line a picture: size, time changed (seconds, nanoseconds), its number, the path.
fn kept(p: &Path, s: Stamp) -> Option<u64> {
    let mut g = KEPT.lock().unwrap_or_else(|x| x.into_inner());
    let k = g.get_or_insert_with(|| Kept { loaded: false, map: HashMap::new() });
    if !k.loaded {
        k.loaded = true;
        let path = kept_path();
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 8 << 20) {
            let _ = std::fs::remove_file(&path);
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            for l in text.lines() {
                let v: Vec<&str> = l.splitn(5, '\t').collect();
                if let [size, sec, nsec, key, path] = v.as_slice() {
                    if let (Ok(a), Ok(b), Ok(c), Ok(n)) = (size.parse(), sec.parse(), nsec.parse(), key.parse()) {
                        k.map.insert(PathBuf::from(path), ((a, b, c), n));
                    }
                }
            }
        }
    }
    k.map.get(p).filter(|(st, _)| *st == s).map(|(_, n)| *n)
}

fn keep(p: &Path, s: Stamp, n: u64) {
    let Some(path) = p.to_str().filter(|x| !x.contains('\n')) else { return };
    if let Some(k) = KEPT.lock().unwrap_or_else(|x| x.into_inner()).as_mut() {
        k.map.insert(p.to_path_buf(), (s, n));
    }
    let file = kept_path();
    if let Some(d) = file.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&file) {
        let _ = f.write_all(format!("{}\t{}\t{}\t{n}\t{path}\n", s.0, s.1, s.2).as_bytes());
    }
}
