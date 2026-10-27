//! Fonts in the inspector: the font's own names and lines of sample text at several
//! sizes, drawn with the font itself on a worker (ab_glyph, which egui already carries;
//! egui's own font atlas is left alone). Install copies it into the user's fonts folder
//! and lets fontconfig know.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use ab_glyph::{Font, FontRef, ScaleFont};

pub fn is_font(ext: &str) -> bool {
    matches!(ext, "ttf" | "otf" | "ttc" | "otc" | "woff" | "woff2")
}

/// Fonts fontconfig takes as they are, so Install is offered for them.
pub fn installable(ext: &str) -> bool {
    matches!(ext, "ttf" | "otf" | "ttc" | "otc")
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Info {
    pub family: String,
    pub style: String,
    pub version: String,
    pub glyphs: u16,
}

fn be16(d: &[u8], i: usize) -> Option<u16> {
    d.get(i..i + 2).map(|s| u16::from_be_bytes([s[0], s[1]]))
}

fn be32(d: &[u8], i: usize) -> Option<u32> {
    d.get(i..i + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

/// The font as plain sfnt data: TTF and OTF as they are, the first face of a
/// collection, a WOFF unpacked. WOFF2 needs Brotli, which Maia doesn't carry.
pub fn sfnt(d: &[u8]) -> Result<Cow<'_, [u8]>, String> {
    match d.get(..4) {
        Some(b"wOF2") => Err("WOFF2 fonts can't be shown here".into()),
        Some(b"wOFF") => unwoff(d).map(Cow::Owned).ok_or_else(|| "This WOFF font is damaged".into()),
        Some(b"\0\x01\0\0" | b"OTTO" | b"true" | b"ttcf") => Ok(Cow::Borrowed(d)),
        _ => Err("Not a font".into()),
    }
}

/// A WOFF 1.0 font back as the sfnt it was made from: each table inflated (zlib) or
/// copied, behind a new table directory.
fn unwoff(d: &[u8]) -> Option<Vec<u8>> {
    let flavor = be32(d, 4)?;
    let n = be16(d, 12)? as usize;
    if n == 0 || n > 512 {
        return None;
    }
    let mut tables = vec![];
    for k in 0..n {
        let e = 44 + k * 20;
        let (tag, off, comp, orig, sum) = (be32(d, e)?, be32(d, e + 4)? as usize, be32(d, e + 8)? as usize, be32(d, e + 12)? as usize, be32(d, e + 16)?);
        let raw = d.get(off..off.checked_add(comp)?)?;
        let data = if comp < orig { fdeflate::decompress_to_vec_bounded(raw, orig).ok()? } else { raw.to_vec() };
        if data.len() != orig {
            return None;
        }
        tables.push((tag, sum, data));
    }
    let mut out = Vec::with_capacity(12 + n * 16 + tables.iter().map(|t| t.2.len() + 3).sum::<usize>());
    let pow = 1usize << (usize::BITS - 1 - n.leading_zeros());
    out.extend_from_slice(&flavor.to_be_bytes());
    for v in [n, pow * 16, pow.trailing_zeros() as usize, n * 16 - pow * 16] {
        out.extend_from_slice(&(v as u16).to_be_bytes());
    }
    let mut off = 12 + n * 16;
    for (tag, sum, data) in &tables {
        for v in [*tag, *sum, off as u32, data.len() as u32] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        off += (data.len() + 3) & !3;
    }
    for (_, _, data) in &tables {
        out.extend_from_slice(data);
        out.resize((out.len() + 3) & !3, 0);
    }
    Some(out)
}

/// Where the first face's tables start (a collection lists its faces first).
fn face_start(d: &[u8]) -> Option<usize> {
    if d.starts_with(b"ttcf") { be32(d, 12).map(|o| o as usize) } else { Some(0) }
}

/// The font's names (family, style, version) and its number of glyphs.
pub fn info(d: &[u8]) -> Info {
    let mut out = Info::default();
    let Some(base) = face_start(d) else { return out };
    let n = be16(d, base + 4).unwrap_or(0) as usize;
    let table = |want: &[u8; 4]| (0..n.min(512)).find_map(|k| {
        let e = base + 12 + k * 16;
        (d.get(e..e + 4)? == want).then(|| be32(d, e + 8).map(|o| o as usize)).flatten()
    });
    if let Some(m) = table(b"maxp") {
        out.glyphs = be16(d, m + 4).unwrap_or(0);
    }
    let Some(nt) = table(b"name") else { return out };
    let (count, strings) = (be16(d, nt + 2).unwrap_or(0) as usize, nt + be16(d, nt + 4).unwrap_or(0) as usize);
    // Typographic names (16, 17) say it best; then the family and style (1, 2).
    let mut best: [(u8, String); 4] = Default::default();
    for k in 0..count.min(2000) {
        let r = nt + 6 + k * 12;
        let (Some(platform), Some(enc), Some(lang), Some(id), Some(len), Some(off)) = (be16(d, r), be16(d, r + 2), be16(d, r + 4), be16(d, r + 6), be16(d, r + 8), be16(d, r + 10)) else { break };
        let Some(raw) = d.get(strings + off as usize..strings + off as usize + len as usize) else { continue };
        let text = match (platform, enc) {
            (3, 1 | 10) | (0, _) => String::from_utf16_lossy(&raw.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect::<Vec<_>>()),
            (1, 0) => raw.iter().map(|&b| b as char).collect(),
            _ => continue,
        };
        // English names first: Windows US English, then Mac Roman English, then any.
        let rank = match (platform, lang) {
            (3, 0x409) => 3,
            (1, 0) => 2,
            _ => 1,
        };
        let slot = match id {
            16 => 0,
            1 => 1,
            17 | 2 => 2,
            5 => 3,
            _ => continue,
        };
        let rank = if id == 17 { rank + 3 } else { rank };
        if rank > best[slot].0 && !text.trim().is_empty() {
            best[slot] = (rank, text.trim().to_string());
        }
    }
    let [typo, fam, style, ver] = best.map(|b| b.1);
    out.family = if typo.is_empty() { fam } else { typo };
    out.style = style;
    out.version = ver.trim_start_matches("Version ").to_string();
    out
}

pub const PANGRAM: &str = "The quick brown fox jumps over the lazy dog";

/// The sample lines: text, and its size in points.
pub fn samples() -> Vec<(&'static str, f32)> {
    vec![("ABCDEFGHIJKLMNOPQRSTUVWXYZ", 18.0), ("abcdefghijklmnopqrstuvwxyz", 18.0), ("0123456789 &@?!%(.,;:)", 18.0), (PANGRAM, 12.0), (PANGRAM, 16.0), (PANGRAM, 24.0), (PANGRAM, 36.0), (PANGRAM, 54.0)]
}

/// One line of `text` at `px` pixels, as coverage (0..255) in a tight width × height
/// buffer; None when the font draws nothing.
pub fn line(font: &FontRef, text: &str, px: f32) -> Option<(usize, usize, Vec<u8>)> {
    let f = font.as_scaled(px);
    let (asc, desc) = (f.ascent(), f.descent());
    let h = (asc - desc).ceil().max(1.0) as usize;
    let mut x = 0.0f32;
    let mut glyphs = vec![];
    let mut last = None;
    for c in text.chars() {
        let id = f.glyph_id(c);
        if let Some(prev) = last {
            x += f.kern(prev, id);
        }
        glyphs.push(id.with_scale_and_position(px, ab_glyph::point(x, asc)));
        x += f.h_advance(id);
        last = Some(id);
    }
    let w = (x.ceil() as usize + 2).min(8192);
    if w < 2 {
        return None;
    }
    let mut buf = vec![0u8; w * h];
    let mut any = false;
    for g in glyphs {
        let Some(o) = font.outline_glyph(g) else { continue };
        let b = o.px_bounds();
        o.draw(|gx, gy, c| {
            let (px_, py) = (b.min.x as i64 + gx as i64, b.min.y as i64 + gy as i64);
            if px_ >= 0 && py >= 0 && (px_ as usize) < w && (py as usize) < h {
                let v = &mut buf[py as usize * w + px_ as usize];
                *v = (*v).max((c.clamp(0.0, 1.0) * 255.0) as u8);
                any = true;
            }
        });
    }
    any.then_some((w, h, buf))
}

/// A font file's thumbnail: "Ag" in the font itself on a small paper card, `px` a side,
/// so a folder of fonts reads at a glance.
pub fn tile(path: &Path, px: u32) -> Option<crate::imgdec::Rgba> {
    let m = std::fs::metadata(path).ok()?;
    if !m.is_file() || m.len() > 64 << 20 {
        return None;
    }
    let raw = std::fs::read(path).ok()?;
    let d = sfnt(&raw).ok()?;
    let font = FontRef::try_from_slice_and_index(&d, 0).ok()?;
    let side = px.clamp(32, 512) as usize;
    let (w, h, cov) = line(&font, "Ag", side as f32 * 0.46)?;
    let mut out = vec![0u8; side * side * 4];
    let (paper, ink) = ([0xf4u8, 0xf1, 0xea], [0x24u8, 0x24, 0x26]);
    let rad = side as f32 * 0.12;
    let (ox, oy) = ((side as i64 - w as i64) / 2, (side as i64 - h as i64) / 2);
    for y in 0..side {
        for x in 0..side {
            // Rounded corners: the card's edge, a pixel soft.
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let cx = fx.clamp(rad, side as f32 - rad);
            let cy = fy.clamp(rad, side as f32 - rad);
            let inside = (rad - ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt() + 0.5).clamp(0.0, 1.0);
            let (gx, gy) = (x as i64 - ox, y as i64 - oy);
            let c = if gx >= 0 && gy >= 0 && (gx as usize) < w && (gy as usize) < h { cov[gy as usize * w + gx as usize] as f32 / 255.0 } else { 0.0 };
            let o = (y * side + x) * 4;
            for k in 0..3 {
                out[o + k] = (paper[k] as f32 + (ink[k] as f32 - paper[k] as f32) * c) as u8;
            }
            out[o + 3] = (inside * 255.0) as u8;
        }
    }
    Some(crate::imgdec::Rgba { w: side as u32, h: side as u32, px: out })
}

/// What the preview shows: the names, the sample lines drawn at `ppp` pixels per point
/// (size in points, then the picture), and whether this very file is installed.
pub struct Preview {
    pub info: Info,
    pub lines: Vec<(f32, egui::ColorImage)>,
    pub installed: bool,
}

/// The user's fonts folder, where Install puts a font.
pub fn fonts_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("fonts"))
}

pub fn read(path: &Path, ppp: f32, fonts: Option<&Path>) -> Result<Preview, String> {
    let m = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if !m.is_file() || m.len() > 64 << 20 {
        return Err("Too big to show".into());
    }
    let raw = std::fs::read(path).map_err(|e| e.to_string())?;
    let d = sfnt(&raw)?;
    let font = FontRef::try_from_slice_and_index(&d, 0).map_err(|_| "This font is damaged".to_string())?;
    let mut info = info(&d);
    if info.family.is_empty() {
        info.family = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    }
    let lines = samples()
        .into_iter()
        .filter_map(|(t, pt)| {
            let (w, h, cov) = line(&font, t, pt * ppp)?;
            let px: Vec<egui::Color32> = cov.iter().map(|&a| egui::Color32::from_white_alpha(a)).collect();
            Some((pt, egui::ColorImage { size: [w, h], pixels: px, source_size: egui::vec2(w as f32, h as f32) }))
        })
        .collect();
    let name = path.file_name().unwrap_or_default();
    let installed = fonts.is_some_and(|f| std::fs::metadata(f.join(name)).is_ok_and(|x| x.len() == m.len()) || path.starts_with(f));
    Ok(Preview { info, lines, installed })
}

/// Copy `path` into `fonts` (made when missing) under its own name, or a free one when
/// another font has it, and run fc-cache on the folder when there is one. Returns the
/// copy, and None when the same file is there already.
pub fn install(path: &Path, fonts: &Path, tools: Option<&crate::drives::Tools>) -> Result<Option<PathBuf>, String> {
    let name = path.file_name().ok_or("No name")?.to_string_lossy().into_owned();
    std::fs::create_dir_all(fonts).map_err(|e| e.to_string())?;
    let there = fonts.join(&name);
    if std::fs::read(&there).ok() == Some(std::fs::read(path).map_err(|e| e.to_string())?) {
        return Ok(None);
    }
    let dest = crate::fsx::unique_dest(fonts, &name);
    let part = fonts.join(format!(".{name}.part"));
    std::fs::copy(path, &part).and_then(|_| std::fs::rename(&part, &dest)).map_err(|e| {
        let _ = std::fs::remove_file(&part);
        e.to_string()
    })?;
    if let Some(t) = tools.filter(|t| t.has("fc-cache")) {
        let mut c = t.cmd("fc-cache");
        c.arg(fonts);
        // The font is in place either way; a slow cache only delays other apps seeing it.
        let _ = crate::errands::output(c, std::time::Duration::from_secs(60));
    }
    Ok(Some(dest))
}

/// Install from the font preview: copied on the errands thread, Undo taking the copy away.
pub fn start_install(app: &mut crate::app::App, path: PathBuf, family: String) {
    let Some(fonts) = fonts_dir() else {
        app.toast("There is no fonts folder to install into");
        return;
    };
    let tools = crate::drives::Tools::current();
    let ctx = app.ctx.clone();
    app.errands.run(&ctx, move || match install(&path, &fonts, tools.as_ref()) {
        Ok(Some(dest)) => crate::errands::Done {
            toast: format!("Installed {family}"),
            undo: Some(crate::app::UndoEntry { label: format!("Install {family}"), kind: crate::app::UndoKind::Created(vec![dest]) }),
            changed: vec![path],
            select: None,
        },
        Ok(None) => crate::errands::Done { toast: format!("{family} is installed already"), changed: vec![path], ..Default::default() },
        Err(e) => crate::errands::Done::say(format!("Couldn't install {family}: {e}")),
    });
}
