//! Files turned into other formats by the desktop's own tools: pictures through
//! ImageMagick (HEIC through libheif when ImageMagick can't read it), sound and video
//! through ffmpeg, PDFs merged and split by poppler. A picture PDF is written here: its
//! pages are JPEGs ImageMagick made upright, because ImageMagick's own PDF writer is
//! often switched off by its security policy.
//!
//! Every result lands beside its source under the same name with the new extension,
//! written to a hidden part file first and renamed once whole, so a failed or cancelled
//! conversion leaves nothing that looks finished.

use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::commands::Cmd;
use crate::drives::Tools;
use crate::fsx::unique_dest;
use crate::ops::{Gate, Progress};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Picture,
    Audio,
    Video,
    Pdf,
}

pub fn kind(ext: &str) -> Option<Kind> {
    Some(match ext {
        "jpg" | "jpeg" | "png" | "webp" | "heic" | "heif" | "avif" | "tif" | "tiff" | "bmp" | "gif" => Kind::Picture,
        "mp3" | "flac" | "ogg" | "oga" | "opus" | "m4a" | "aac" | "wav" | "wma" | "aiff" | "aif" => Kind::Audio,
        "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" | "wmv" | "flv" | "mpg" | "mpeg" | "ts" | "3gp" => Kind::Video,
        "pdf" => Kind::Pdf,
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Jpg,
    Png,
    Webp,
    /// The pictures as pages of one PDF.
    PicturePdf,
    Mp3,
    Opus,
    Flac,
    Mp4,
    Webm,
    /// A video's sound as MP3.
    SoundMp3,
    MergePdf,
    SplitPdf,
}

impl Target {
    pub const ALL: [Target; 12] = [Target::Jpg, Target::Png, Target::Webp, Target::PicturePdf, Target::Mp3, Target::Opus, Target::Flac, Target::Mp4, Target::Webm, Target::SoundMp3, Target::MergePdf, Target::SplitPdf];

    pub fn ext(self) -> &'static str {
        match self {
            Target::Jpg => "jpg",
            Target::Png => "png",
            Target::Webp => "webp",
            Target::PicturePdf | Target::MergePdf => "pdf",
            Target::Mp3 | Target::SoundMp3 => "mp3",
            Target::Opus => "opus",
            Target::Flac => "flac",
            Target::Mp4 => "mp4",
            Target::Webm => "webm",
            Target::SplitPdf => "",
        }
    }

    /// The format's name, as job cards and toasts say it.
    pub fn name(self) -> &'static str {
        match self {
            Target::Jpg => "JPG",
            Target::Png => "PNG",
            Target::Webp => "WebP",
            Target::PicturePdf | Target::MergePdf | Target::SplitPdf => "PDF",
            Target::Mp3 | Target::SoundMp3 => "MP3",
            Target::Opus => "Opus",
            Target::Flac => "FLAC",
            Target::Mp4 => "MP4",
            Target::Webm => "WebM",
        }
    }

    /// What the Convert menu calls it.
    pub fn label(self) -> &'static str {
        match self {
            Target::PicturePdf => "PDF…",
            Target::SoundMp3 => "Sound only (MP3)",
            Target::MergePdf => "Merge into one PDF…",
            Target::SplitPdf => "Split into pages",
            t => t.name(),
        }
    }

    fn of_kind(self) -> Kind {
        match self {
            Target::Jpg | Target::Png | Target::Webp | Target::PicturePdf => Kind::Picture,
            Target::Mp3 | Target::Opus | Target::Flac => Kind::Audio,
            Target::Mp4 | Target::Webm | Target::SoundMp3 => Kind::Video,
            Target::MergePdf | Target::SplitPdf => Kind::Pdf,
        }
    }

    /// Whether a file with extension `ext` is already in this format.
    fn same(self, ext: &str) -> bool {
        match self {
            Target::Jpg => matches!(ext, "jpg" | "jpeg"),
            Target::Png | Target::Webp | Target::Mp3 | Target::Opus | Target::Flac | Target::Mp4 | Target::Webm => ext == self.ext(),
            _ => false,
        }
    }

    /// The name is asked for first: one file is made from several.
    pub fn asks_name(self) -> bool {
        matches!(self, Target::PicturePdf | Target::MergePdf)
    }

    pub fn cmd(self) -> Cmd {
        match self {
            Target::Jpg => Cmd::ConvertJpg,
            Target::Png => Cmd::ConvertPng,
            Target::Webp => Cmd::ConvertWebp,
            Target::PicturePdf => Cmd::ConvertPdf,
            Target::Mp3 => Cmd::ConvertMp3,
            Target::Opus => Cmd::ConvertOpus,
            Target::Flac => Cmd::ConvertFlac,
            Target::Mp4 => Cmd::ConvertMp4,
            Target::Webm => Cmd::ConvertWebm,
            Target::SoundMp3 => Cmd::ConvertSoundMp3,
            Target::MergePdf => Cmd::MergePdfs,
            Target::SplitPdf => Cmd::SplitPdf,
        }
    }

    pub fn of(c: Cmd) -> Option<Target> {
        Target::ALL.into_iter().find(|t| t.cmd() == c)
    }
}

/// The one kind every file in a selection is, from their extensions (lower case).
pub fn kind_of(exts: &[&str]) -> Option<Kind> {
    let k = kind(exts.first()?)?;
    exts.iter().all(|e| kind(e) == Some(k)).then_some(k)
}

/// What a selection of files (by extension) can become with the tools installed.
pub fn offered(exts: &[&str], t: &Tools) -> Vec<Target> {
    let Some(k) = kind_of(exts) else { return vec![] };
    let have = match k {
        Kind::Picture => crate::picfix::magick(t).is_some(),
        Kind::Audio | Kind::Video => t.has("ffmpeg"),
        Kind::Pdf => true,
    };
    if !have {
        return vec![];
    }
    Target::ALL
        .into_iter()
        .filter(|g| g.of_kind() == k && !exts.iter().all(|e| g.same(e)))
        .filter(|g| match g {
            Target::MergePdf => exts.len() >= 2 && t.has("pdfunite"),
            Target::SplitPdf => exts.len() == 1 && t.has("pdfseparate"),
            _ => true,
        })
        .collect()
}

/// What to install when a selection of this kind can't be converted.
pub fn needs(k: Kind) -> &'static str {
    match k {
        Kind::Picture => "Converting pictures needs ImageMagick",
        Kind::Audio | Kind::Video => "Converting sound and video needs ffmpeg",
        Kind::Pdf => "Merging and splitting PDFs needs poppler",
    }
}

/// The job card's title: "Converting song.flac to MP3", "Merging 3 PDFs".
pub fn label(srcs: &[PathBuf], to: Target) -> String {
    let what = match srcs {
        [one] => name_of(one),
        many => format!("{} files", many.len()),
    };
    match to {
        Target::MergePdf => format!("Merging {} PDFs", srcs.len()),
        Target::SplitPdf => format!("Splitting {what} into pages"),
        Target::SoundMp3 => format!("Converting {what} to sound only"),
        t => format!("Converting {what} to {}", t.name()),
    }
}

/// What Undo says it takes back: "Convert song.flac to MP3".
pub fn undo_label(srcs: &[PathBuf], to: Target) -> String {
    let l = label(srcs, to);
    for (now, verb) in [("Converting", "Convert"), ("Merging", "Merge"), ("Splitting", "Split")] {
        if let Some(rest) = l.strip_prefix(now) {
            return format!("{verb}{rest}");
        }
    }
    l
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn ext_of(p: &Path) -> String {
    p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default()
}

fn stem_of(p: &Path) -> String {
    p.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The hidden file a result is written to before it is renamed into place.
fn part_of(dest: &Path) -> PathBuf {
    dest.with_file_name(format!(".{}.part", name_of(dest)))
}

fn lock(p: &Mutex<Progress>) -> std::sync::MutexGuard<'_, Progress> {
    p.lock().unwrap_or_else(|e| e.into_inner())
}

/// Run a conversion job: every source in turn (or all of them into `out`).
pub fn run(srcs: &[PathBuf], to: Target, out: Option<&Path>, t: &Tools, p: &Mutex<Progress>, gate: &Gate, ctx: &egui::Context) {
    let fail = |src: &Path, e: String| lock(p).errors.push(format!("{}: {e}", name_of(src)));
    match to {
        Target::PicturePdf | Target::MergePdf => {
            {
                let mut g = lock(p);
                g.total_items = 1;
                g.within = Some(0.0);
            }
            let Some(dir) = srcs.first().and_then(|s| s.parent()) else { return };
            let dest = out.map(Path::to_path_buf).unwrap_or_else(|| unique_dest(dir, &format!("{}.pdf", if to == Target::MergePdf { "Merged" } else { "Pictures" })));
            lock(p).current = name_of(&dest);
            let r = if to == Target::MergePdf { merge(t, srcs, &dest) } else { picture_pdf(t, srcs, &dest, p, gate, ctx) };
            match r {
                Ok(()) => {
                    let mut g = lock(p);
                    g.produced.push((srcs[0].clone(), dest));
                    g.done_items = 1;
                }
                Err(e) if e == CANCELLED => {}
                Err(e) => lock(p).errors.push(format!("{}: {e}", name_of(&dest))),
            }
        }
        _ => {
            lock(p).total_items = srcs.len();
            for s in srcs {
                if gate.stop() {
                    break;
                }
                {
                    let mut g = lock(p);
                    g.current = name_of(s);
                    g.within = Some(0.0);
                }
                ctx.request_repaint();
                let Some(dir) = s.parent() else { continue };
                let r = match to {
                    Target::SplitPdf => split(t, s, dir),
                    Target::Jpg | Target::Png | Target::Webp => {
                        let dest = unique_dest(dir, &format!("{}.{}", stem_of(s), to.ext()));
                        picture(t, s, to, &dest).map(|_| dest)
                    }
                    _ => {
                        let dest = unique_dest(dir, &format!("{}.{}", stem_of(s), to.ext()));
                        media(t, s, to, &dest, p, gate, ctx).map(|_| dest)
                    }
                };
                match r {
                    Ok(dest) => lock(p).produced.push((s.clone(), dest)),
                    Err(e) if e == CANCELLED => break,
                    Err(e) => fail(s, e),
                }
                lock(p).done_items += 1;
                ctx.request_repaint();
            }
        }
    }
}

const CANCELLED: &str = "Cancelled";

/// Past this a picture is given up on; sound and video take as long as they take.
const PICTURE_TIMEOUT: Duration = Duration::from_secs(180);

/// ImageMagick's arguments for a picture in `to`, after reading it.
fn picture_args(to: Target) -> Vec<&'static str> {
    let mut a = vec!["-auto-orient"];
    match to {
        // JPEG has no transparency: what was see-through becomes white, not black.
        Target::Jpg => a.extend(["-background", "white", "-alpha", "remove", "-alpha", "off", "-quality", "90"]),
        Target::Webp => a.extend(["-quality", "90"]),
        // A 16-bit HEIC would make a PNG twice the size for nothing anyone sees.
        Target::Png => a.extend(["-depth", "8"]),
        _ => {}
    }
    a
}

fn im_format(to: Target) -> &'static str {
    match to {
        Target::Png => "PNG",
        Target::Webp => "WEBP",
        _ => "JPEG",
    }
}

/// ImageMagick reading `src` on its standard input (a name with brackets or a colon is
/// never read as one of its own), first frame only (an animated GIF would give one
/// picture per frame), writing `fmt` to `out` or returning it.
fn magick(t: &Tools, src: &Path, args: &[&str], fmt: &str, out: Option<&Path>) -> Result<Vec<u8>, String> {
    let prog = crate::picfix::magick(t).ok_or("Converting pictures needs ImageMagick")?;
    let input = std::fs::File::open(src).map_err(|e| e.to_string())?;
    let mut c = t.cmd(prog);
    c.arg("-[0]").args(args).arg(format!("{fmt}:-"));
    let file = match out {
        Some(p) => Some(std::fs::File::create(p).map_err(|e| e.to_string())?),
        None => None,
    };
    crate::errands::output_io(c, PICTURE_TIMEOUT, Some(input), file)
}

/// A HEIC that ImageMagick can't read, decoded by libheif's own tool into a PNG beside
/// `near` (removed again by the caller).
fn heif_png(t: &Tools, src: &Path, near: &Path) -> Result<PathBuf, String> {
    let tool = ["heif-dec", "heif-convert"].into_iter().find(|n| t.has(n)).ok_or("This picture needs libheif's heif-dec")?;
    let png = near.with_file_name(format!(".{}.heif.png", name_of(near)));
    let mut c = t.cmd(tool);
    c.arg(src).arg(&png);
    let r = crate::errands::output(c, PICTURE_TIMEOUT);
    match r {
        Ok(_) if png.is_file() => Ok(png),
        Ok(_) => Err("libheif made nothing of it".into()),
        Err(e) => {
            let _ = std::fs::remove_file(&png);
            Err(e)
        }
    }
}

/// `src` through ImageMagick, falling back to libheif for a HEIC it can't read.
fn magick_any(t: &Tools, src: &Path, args: &[&str], fmt: &str, out: Option<&Path>, near: &Path) -> Result<Vec<u8>, String> {
    match magick(t, src, args, fmt, out) {
        Err(e) if matches!(ext_of(src).as_str(), "heic" | "heif") => {
            let Ok(png) = heif_png(t, src, near) else { return Err(e) };
            let r = magick(t, &png, args, fmt, out);
            let _ = std::fs::remove_file(&png);
            r
        }
        r => r,
    }
}

fn picture(t: &Tools, src: &Path, to: Target, dest: &Path) -> Result<(), String> {
    let part = part_of(dest);
    let r = magick_any(t, src, &picture_args(to), im_format(to), Some(&part), dest).and_then(|_| finish(&part, dest));
    if r.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    r
}

/// The part file renamed into place, once it holds something.
fn finish(part: &Path, dest: &Path) -> Result<(), String> {
    if std::fs::metadata(part).map(|m| m.len()).unwrap_or(0) == 0 {
        return Err("Nothing came out".into());
    }
    std::fs::rename(part, dest).map_err(|e| e.to_string())
}

// ------------------------------------------------------------------ pictures into a PDF

/// Width, height and colour components of a JPEG, from its frame header.
pub fn jpeg_size(b: &[u8]) -> Option<(u32, u32, u8)> {
    if b.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut i = 2;
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            return None;
        }
        let m = b[i + 1];
        // Fill bytes and markers without a length.
        if m == 0xFF {
            i += 1;
            continue;
        }
        if m == 0x01 || (0xD0..=0xD7).contains(&m) {
            i += 2;
            continue;
        }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        // Start of frame: every SOFn but the huffman, arithmetic and extension markers.
        if (0xC0..=0xCF).contains(&m) && !matches!(m, 0xC4 | 0xC8 | 0xCC) {
            let h = u16::from_be_bytes([*b.get(i + 5)?, *b.get(i + 6)?]) as u32;
            let w = u16::from_be_bytes([*b.get(i + 7)?, *b.get(i + 8)?]) as u32;
            let c = *b.get(i + 9)?;
            return (w > 0 && h > 0).then_some((w, h, c));
        }
        i += 2 + len;
    }
    None
}

/// A4 in points.
const A4: (f32, f32) = (595.28, 841.89);
/// Around each picture on its page.
const MARGIN: f32 = 28.0;

/// Where a `w`×`h` picture sits on its page: page size, then x, y, width, height, all in
/// points. The page turns to landscape for a wide picture.
pub fn page_for(w: u32, h: u32) -> ((f32, f32), [f32; 4]) {
    let page = if w > h { (A4.1, A4.0) } else { A4 };
    let (bw, bh) = (page.0 - 2.0 * MARGIN, page.1 - 2.0 * MARGIN);
    let k = (bw / w as f32).min(bh / h as f32);
    let (dw, dh) = (w as f32 * k, h as f32 * k);
    (page, [(page.0 - dw) / 2.0, (page.1 - dh) / 2.0, dw, dh])
}

/// A PDF of JPEG pages, one picture on each.
pub fn pdf_of_jpegs(jpegs: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offs: Vec<usize> = vec![];
    let n = jpegs.len();
    fn obj(out: &mut Vec<u8>, offs: &mut Vec<usize>, body: &[u8]) {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", offs.len()).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    obj(&mut out, &mut offs, b"<< /Type /Catalog /Pages 2 0 R >>");
    let kids: Vec<String> = (0..n).map(|k| format!("{} 0 R", 3 + 3 * k)).collect();
    obj(&mut out, &mut offs, format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")).as_bytes());
    for (k, j) in jpegs.iter().enumerate() {
        let (w, h, comps) = jpeg_size(j).ok_or("Not a picture ImageMagick made")?;
        let space = match comps {
            1 => "/DeviceGray",
            4 => "/DeviceCMYK",
            _ => "/DeviceRGB",
        };
        let ((pw, ph), [x, y, dw, dh]) = page_for(w, h);
        let (content, image) = (4 + 3 * k, 5 + 3 * k);
        obj(&mut out, &mut offs, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw:.2} {ph:.2}] /Resources << /XObject << /Im0 {image} 0 R >> >> /Contents {content} 0 R >>").as_bytes());
        let draw = format!("q {dw:.2} 0 0 {dh:.2} {x:.2} {y:.2} cm /Im0 Do Q");
        obj(&mut out, &mut offs, format!("<< /Length {} >>\nstream\n{draw}\nendstream", draw.len()).as_bytes());
        let mut body = format!("<< /Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace {space} /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>\nstream\n", j.len()).into_bytes();
        body.extend_from_slice(j);
        body.extend_from_slice(b"\nendstream");
        obj(&mut out, &mut offs, &body);
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offs.len() + 1).as_bytes());
    for o in &offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", offs.len() + 1).as_bytes());
    Ok(out)
}

fn picture_pdf(t: &Tools, srcs: &[PathBuf], dest: &Path, p: &Mutex<Progress>, gate: &Gate, ctx: &egui::Context) -> Result<(), String> {
    // Upright, in sRGB with three channels (a grey or CMYK JPEG would need its own
    // colour space and decode in the PDF), and flattened on white.
    let args = ["-auto-orient", "-background", "white", "-alpha", "remove", "-alpha", "off", "-colorspace", "sRGB", "-type", "TrueColor", "-quality", "90"];
    let mut pages = Vec::with_capacity(srcs.len());
    for (i, s) in srcs.iter().enumerate() {
        if gate.stop() {
            return Err(CANCELLED.into());
        }
        lock(p).within = Some(i as f32 / srcs.len() as f32);
        ctx.request_repaint();
        let j = magick_any(t, s, &args, "JPEG", None, dest).map_err(|e| format!("{}: {e}", name_of(s)))?;
        pages.push(j);
    }
    let pdf = pdf_of_jpegs(&pages)?;
    let part = part_of(dest);
    let r = std::fs::write(&part, pdf).map_err(|e| e.to_string()).and_then(|_| finish(&part, dest));
    if r.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    r
}

// ------------------------------------------------------------------ PDFs

fn merge(t: &Tools, srcs: &[PathBuf], dest: &Path) -> Result<(), String> {
    let part = part_of(dest);
    let mut c = t.cmd("pdfunite");
    c.args(srcs).arg(&part);
    let r = crate::errands::output(c, Duration::from_secs(600)).and_then(|_| finish(&part, dest));
    if r.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    r
}

/// One PDF a page a file, in a folder "<name> pages" beside it: made in a hidden folder,
/// renamed into place once every page is there.
fn split(t: &Tools, src: &Path, dir: &Path) -> Result<PathBuf, String> {
    let stem = stem_of(src);
    let stage = dir.join(format!(".maia-split-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir(&stage).map_err(|e| e.to_string())?;
    let r = (|| {
        let mut c = t.cmd("pdfseparate");
        // Pages named in the folder itself: a % anywhere in the path would be read as
        // part of the pattern.
        c.current_dir(&stage).arg(src).arg("p-%d.pdf");
        crate::errands::output(c, Duration::from_secs(600))?;
        let mut n = 0;
        while stage.join(format!("p-{}.pdf", n + 1)).is_file() {
            n += 1;
        }
        if n == 0 {
            return Err("It has no pages".to_string());
        }
        for k in 1..=n {
            std::fs::rename(stage.join(format!("p-{k}.pdf")), stage.join(format!("{stem} {k}.pdf"))).map_err(|e| e.to_string())?;
        }
        let dest = unique_dest(dir, &format!("{stem} pages"));
        std::fs::rename(&stage, &dest).map_err(|e| e.to_string())?;
        Ok(dest)
    })();
    if r.is_err() {
        let _ = std::fs::remove_dir_all(&stage);
    }
    r
}

// ------------------------------------------------------------------ sound and video

/// The encoders an ffmpeg build has, from `ffmpeg -encoders`: each line's second word.
pub fn encoder_names(listing: &str) -> Vec<String> {
    // The legend before the dashes is not a list of encoders.
    listing.lines().skip_while(|l| l.trim() != "------").skip(1).filter_map(|l| {
        let mut w = l.split_whitespace();
        let flags = w.next()?;
        // " V....D libx264  libx264 H.264 ...": six flag letters, then the name.
        (flags.len() == 6 && flags.chars().next().is_some_and(|c| "VAS".contains(c))).then(|| w.next().map(str::to_string)).flatten()
    }).collect()
}

/// Asked of ffmpeg once per run, on the job's thread.
fn encoders(t: &Tools) -> Vec<String> {
    static HAVE: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    HAVE.get_or_init(|| {
        let mut c = t.cmd("ffmpeg");
        c.args(["-hide_banner", "-encoders"]);
        crate::errands::output(c, Duration::from_secs(20)).map(|o| encoder_names(&String::from_utf8_lossy(&o))).unwrap_or_default()
    }).clone()
}

/// ffmpeg's arguments after the input for `to`, with or without the cover picture, made
/// with the best encoder this build has: distributions leave different ones out (one
/// without x264 still has OpenH264 or MPEG-4 for an MP4). An empty `have` (not known)
/// asks for the usual ones.
pub fn media_args(to: Target, cover: bool, have: &[String]) -> Result<Vec<&'static str>, String> {
    let has = |n: &str| have.is_empty() || have.iter().any(|h| h == n);
    let keep_cover = ["-map", "0:a:0", "-map", "0:v:0?", "-c:v", "copy", "-disposition:v:0", "attached_pic"];
    let mut a: Vec<&'static str> = vec![];
    match to {
        Target::Mp3 | Target::Flac if cover => a.extend(keep_cover),
        Target::Mp3 | Target::Flac | Target::Opus | Target::SoundMp3 => a.extend(["-map", "0:a:0", "-vn"]),
        _ => a.extend(["-map", "0:v:0", "-map", "0:a:0?"]),
    }
    let opus = |a: &mut Vec<&'static str>| -> Result<(), String> {
        if has("libopus") {
            a.extend(["-c:a", "libopus", "-b:a", "128k"]);
        } else if has("opus") {
            // ffmpeg's own, still marked experimental.
            a.extend(["-c:a", "opus", "-strict", "-2", "-b:a", "128k"]);
        } else {
            return Err("This ffmpeg can't write Opus sound".into());
        }
        Ok(())
    };
    match to {
        Target::Mp3 | Target::SoundMp3 if has("libmp3lame") => a.extend(["-c:a", "libmp3lame", "-q:a", "2", "-id3v2_version", "3", "-f", "mp3"]),
        Target::Mp3 | Target::SoundMp3 => return Err("This ffmpeg can't write MP3 (it has no LAME)".into()),
        Target::Opus => {
            opus(&mut a)?;
            a.extend(["-f", "opus"]);
        }
        Target::Flac => a.extend(["-c:a", "flac", "-f", "flac"]),
        Target::Mp4 => {
            // Even sides: 4:2:0 video can't have an odd width or height.
            a.extend(["-vf", "scale=trunc(iw/2)*2:trunc(ih/2)*2", "-pix_fmt", "yuv420p"]);
            if has("libx264") {
                a.extend(["-c:v", "libx264", "-crf", "23", "-preset", "medium"]);
            } else if has("libopenh264") {
                a.extend(["-c:v", "libopenh264", "-b:v", "6M"]);
            } else if has("mpeg4") {
                a.extend(["-c:v", "mpeg4", "-q:v", "3"]);
            } else {
                return Err("This ffmpeg can't write MP4 video".into());
            }
            a.extend(["-c:a", "aac", "-b:a", "160k", "-movflags", "+faststart", "-f", "mp4"]);
        }
        Target::Webm => {
            if has("libvpx-vp9") {
                a.extend(["-c:v", "libvpx-vp9", "-crf", "32", "-b:v", "0", "-row-mt", "1", "-cpu-used", "4"]);
            } else if has("libvpx") {
                a.extend(["-c:v", "libvpx", "-crf", "10", "-b:v", "4M"]);
            } else {
                return Err("This ffmpeg can't write WebM video".into());
            }
            opus(&mut a)?;
            a.extend(["-f", "webm"]);
        }
        _ => {}
    }
    Ok(a)
}

/// ffmpeg's way of naming a file: never read as a protocol or an option.
fn ff_path(p: &Path) -> std::ffi::OsString {
    let mut s = std::ffi::OsString::from("file:");
    s.push(p);
    s
}

/// How long `src` plays, from ffprobe; None without it.
pub fn duration(t: &Tools, src: &Path) -> Option<f64> {
    if !t.has("ffprobe") {
        return None;
    }
    let mut c = t.cmd("ffprobe");
    c.args(["-v", "error", "-show_entries", "format=duration", "-of", "default=noprint_wrappers=1:nokey=1"]).arg(ff_path(src));
    let out = crate::errands::output(c, Duration::from_secs(20)).ok()?;
    String::from_utf8_lossy(&out).trim().parse().ok().filter(|d: &f64| d.is_finite() && *d > 0.0)
}

fn media(t: &Tools, src: &Path, to: Target, dest: &Path, p: &Mutex<Progress>, gate: &Gate, ctx: &egui::Context) -> Result<(), String> {
    if !t.has("ffmpeg") {
        return Err("Converting sound and video needs ffmpeg".into());
    }
    let dur = duration(t, src);
    let part = part_of(dest);
    let cover = matches!(to, Target::Mp3 | Target::Flac);
    let have = encoders(t);
    let mut r = media_args(to, cover, &have).and_then(|a| ffmpeg(t, src, &a, &part, dur, p, gate, ctx));
    // A cover ffmpeg can't put in this format: the sound alone.
    if cover && r.as_ref().is_err_and(|e| e != CANCELLED) {
        r = media_args(to, false, &have).and_then(|a| ffmpeg(t, src, &a, &part, dur, p, gate, ctx));
    }
    let r = r.and_then(|_| finish(&part, dest));
    if r.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    r
}

/// The microseconds ffmpeg's `-progress` lines say it has written.
pub fn progress_us(line: &str) -> Option<u64> {
    // out_time_ms is microseconds too, despite its name.
    let v = line.strip_prefix("out_time_us=").or_else(|| line.strip_prefix("out_time_ms="))?;
    v.trim().parse().ok()
}

#[allow(clippy::too_many_arguments)]
fn ffmpeg(t: &Tools, src: &Path, args: &[&str], part: &Path, dur: Option<f64>, p: &Mutex<Progress>, gate: &Gate, ctx: &egui::Context) -> Result<(), String> {
    let mut c = crate::media::tool_cmd(t.cmd("ffmpeg"));
    c.args(["-nostdin", "-hide_banner", "-nostats", "-loglevel", "error", "-progress", "pipe:1", "-y", "-i"]).arg(ff_path(src)).args(args).arg(ff_path(part));
    c.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| e.to_string())?;
    let done_us = std::sync::Arc::new(AtomicU64::new(0));
    let out = child.stdout.take();
    let d2 = done_us.clone();
    let reader = std::thread::spawn(move || {
        let Some(o) = out else { return };
        for line in std::io::BufReader::new(o).lines().map_while(Result::ok) {
            if let Some(us) = progress_us(&line) {
                d2.store(us, Ordering::Relaxed);
            }
        }
    });
    let err = child.stderr.take();
    let errs = std::thread::spawn(move || {
        let mut v = vec![];
        if let Some(mut e) = err {
            let _ = e.read_to_end(&mut v);
        }
        v
    });
    let pid = child.id() as libc::pid_t;
    let mut stopped = false;
    let mut shown = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Ok(st),
            Ok(None) => {}
            Err(e) => break Err(e.to_string()),
        }
        if gate.cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            break Err(CANCELLED.to_string());
        }
        // Paused: ffmpeg is stopped where it is and carries on when resumed.
        if gate.paused() != stopped {
            stopped = !stopped;
            unsafe { libc::kill(pid, if stopped { libc::SIGSTOP } else { libc::SIGCONT }) };
        }
        if let Some(d) = dur {
            let f = (done_us.load(Ordering::Relaxed) as f64 / 1e6 / d).clamp(0.0, 1.0) as f32;
            lock(p).within = Some(f);
            if shown.elapsed() > Duration::from_millis(250) {
                ctx.request_repaint();
                shown = Instant::now();
            }
        }
        std::thread::sleep(Duration::from_millis(40));
    };
    let _ = reader.join();
    let said = errs.join().unwrap_or_default();
    let st = status?;
    if st.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&said);
    // The first complaint names the cause; "Conversion failed!" comes last. A
    // "[libx264 @ 0x…]" prefix says nothing to a person.
    let line = said.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let line = match line.strip_prefix('[').and_then(|l| l.split_once("] ")) {
        Some((_, rest)) => rest,
        None => line,
    };
    Err(if line.is_empty() { format!("ffmpeg stopped with {st}") } else { line.to_string() })
}

// ------------------------------------------------------------------ from the app

/// The files a conversion of the active tab's selection would take, and their
/// extensions: none in an archive or the trash, or with a folder among them.
fn selection(app: &crate::app::App) -> Option<(Vec<PathBuf>, Vec<String>)> {
    let p = app.panels.get(&app.active)?;
    let t = p.tab();
    if t.archive.is_some() || t.path.starts_with(crate::sidebar::trash_dir()) || crate::tab::is_place(&t.path) {
        return None;
    }
    let ents = t.target_entries();
    if ents.is_empty() || ents.iter().any(|e| e.is_dir) {
        return None;
    }
    Some((ents.iter().map(|e| e.path.clone()).collect(), ents.iter().map(|e| e.ext().to_string()).collect()))
}

/// What the active selection can be converted to (the palette offers only these).
pub fn applicable(app: &crate::app::App, to: Target) -> bool {
    let Some((_, exts)) = selection(app) else { return false };
    let exts: Vec<&str> = exts.iter().map(String::as_str).collect();
    Tools::current().is_some_and(|t| offered(&exts, &t).contains(&to))
}

/// Start converting the selection, asking for the name first when several files make one.
pub fn start(app: &mut crate::app::App, to: Target) {
    let Some((srcs, exts)) = selection(app) else {
        app.toast("Select files to convert");
        return;
    };
    let exts: Vec<&str> = exts.iter().map(String::as_str).collect();
    let Some(k) = kind_of(&exts) else {
        app.toast("Select files of one kind");
        return;
    };
    let Some(tools) = Tools::current() else {
        app.toast(needs(k));
        return;
    };
    let can = offered(&exts, &tools);
    if !can.contains(&to) {
        app.toast(if can.is_empty() || to.of_kind() == k { needs(k) } else { "These files can't become that" });
        return;
    }
    if !to.asks_name() {
        app.actions.push(crate::app::Action::StartJob(crate::ops::OpKind::Convert { srcs, to, out: None, tools }));
        return;
    }
    let Some(dir) = srcs[0].parent().map(Path::to_path_buf) else { return };
    let base = match (to, srcs.as_slice()) {
        (Target::PicturePdf, [one]) => format!("{}.pdf", stem_of(one)),
        (Target::PicturePdf, _) => "Pictures.pdf".to_string(),
        _ => "Merged.pdf".to_string(),
    };
    let start = name_of(&unique_dest(&dir, &base));
    let title = if to == Target::MergePdf { "Name of the merged PDF" } else { "Name of the PDF" };
    app.popup = crate::popups::Popup::prompt(
        title,
        start,
        Box::new(move |app: &mut crate::app::App, text: String| {
            let name = text.trim();
            if name.is_empty() || name.contains('/') || name == "." || name == ".." {
                app.toast("Type a name for the PDF");
                return;
            }
            let name = if name.to_lowercase().ends_with(".pdf") { name.to_string() } else { format!("{name}.pdf") };
            let out = unique_dest(&dir, &name);
            app.actions.push(crate::app::Action::StartJob(crate::ops::OpKind::Convert { srcs, to, out: Some(out), tools }));
        }),
    );
}

/// The Convert submenu for files with these extensions, when there is anything to offer.
pub fn menu(exts: &[&str]) -> Option<Vec<(Cmd, &'static str)>> {
    let t = Tools::current()?;
    let v: Vec<(Cmd, &'static str)> = offered(exts, &t).into_iter().map(|g| (g.cmd(), g.label())).collect();
    (!v.is_empty()).then_some(v)
}
