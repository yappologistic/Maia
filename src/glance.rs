//! A folder at a glance: a small ring in the status bar of what kinds of file fill the
//! folder shown, by count, in the icons' own colours; pointing at it names the shares.
//! Worked out once per order of the list (a big one is sampled), never per frame.

use egui::{Pos2, Rect, Stroke, Ui};

use crate::tab::Tab;
use crate::theme::{FileCat, Palette};

/// Items looked at, at most: past this the shares come from an even sample.
const SAMPLE: usize = 20_000;

/// The shares worked out for a tab: (category, share), largest first.
#[derive(Default)]
pub struct Glance {
    key: Option<(u64, usize)>,
    shares: Vec<(FileCat, f32)>,
}

fn name(c: FileCat) -> &'static str {
    match c {
        FileCat::Folder => "Folders",
        FileCat::Image => "Pictures",
        FileCat::Video => "Videos",
        FileCat::Audio => "Music and sound",
        FileCat::Archive => "Archives",
        FileCat::Code => "Code",
        FileCat::Doc => "Documents",
        FileCat::Text => "Text",
        FileCat::Sheet => "Spreadsheets",
        FileCat::Slides => "Presentations",
        FileCat::Pdf => "PDFs",
        FileCat::Font => "Fonts",
        FileCat::Disk => "Disk images",
        FileCat::Book => "Books",
        FileCat::Exec => "Programs",
        FileCat::Other => "Other",
    }
}

const ALL: [FileCat; 16] = [FileCat::Folder, FileCat::Image, FileCat::Video, FileCat::Audio, FileCat::Archive, FileCat::Code, FileCat::Doc, FileCat::Text, FileCat::Sheet, FileCat::Slides, FileCat::Pdf, FileCat::Font, FileCat::Disk, FileCat::Book, FileCat::Exec, FileCat::Other];

/// The shares of the tab's shown items (worked out again when the list changed).
pub fn shares(t: &mut Tab) -> Vec<(FileCat, f32)> {
    // Computer's drives and the GitHub page are no folder of files.
    if crate::tab::is_place(&t.path) {
        return vec![];
    }
    let n = t.view.len();
    let key = (t.view_gen, n);
    if t.glance.key != Some(key) && (!t.is_loading() || t.glance.key.is_none_or(|k| k.1 * 2 < n)) {
        t.glance.key = Some(key);
        let step = n.div_ceil(SAMPLE).max(1);
        let mut counts = [0u32; 16];
        let mut seen = 0u32;
        for &i in t.view.iter().step_by(step) {
            let Some(e) = t.entries.get(i as usize) else { continue };
            let c = crate::theme::categorize(&e.ext(), e.is_dir, e.is_executable());
            if let Some(k) = ALL.iter().position(|x| *x == c) {
                counts[k] += 1;
                seen += 1;
            }
        }
        let mut v: Vec<(FileCat, f32)> = ALL.iter().zip(counts).filter(|(_, n)| *n > 0).map(|(c, n)| (*c, n as f32 / seen.max(1) as f32)).collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1));
        t.glance.shares = v;
    }
    t.glance.shares.clone()
}

/// The ring, in `r`: one arc per kind of file, the largest first from the top.
pub fn paint_ring(ui: &Ui, r: Rect, shares: &[(FileCat, f32)], pal: &Palette) {
    let c = r.center();
    let rad = r.width().min(r.height()) / 2.0 - 1.0;
    let width = (rad * 0.42).max(2.0);
    let mid = rad - width / 2.0;
    let mut a0 = -std::f32::consts::FRAC_PI_2;
    let gap = if shares.len() > 1 { 0.08 } else { 0.0 };
    for (cat, share) in shares {
        let sweep = share * std::f32::consts::TAU;
        let a1 = a0 + sweep;
        let (s, e) = (a0 + gap / 2.0, (a1 - gap / 2.0).max(a0 + gap / 2.0 + 0.01));
        let steps = ((e - s) / 0.15).ceil().max(2.0) as usize;
        let pts: Vec<Pos2> = (0..=steps).map(|k| {
            let a = s + (e - s) * k as f32 / steps as f32;
            c + egui::vec2(a.cos(), a.sin()) * mid
        }).collect();
        ui.painter().add(egui::Shape::line(pts, Stroke::new(width, crate::theme::cat_color(*cat))));
        a0 = a1;
    }
    if shares.is_empty() {
        ui.painter().circle_stroke(c, mid, Stroke::new(width, pal.border));
    }
}

/// What pointing at the ring says: each kind of file and its share.
pub fn describe(shares: &[(FileCat, f32)]) -> String {
    shares.iter().take(8).map(|(c, s)| {
        let pct = (s * 100.0).round();
        let pct = if pct < 1.0 { "under 1".to_string() } else { format!("{pct}") };
        format!("{} {pct}%", name(*c))
    }).collect::<Vec<_>>().join("\n")
}
