//! Paste from earlier copies: the last nine sets copied or cut this session, kept in
//! memory only. The system clipboard still holds just the latest, as before; this list
//! is Maia's own. Picking a set pastes it here as Paste does, and a cut set, once pasted,
//! is gone from the list.

use std::path::PathBuf;

use egui::{CornerRadius, Event, FontId, Rect, Sense, Stroke, StrokeKind, Vec2, pos2, vec2};

use crate::app::App;
use crate::popups as pp;

/// Sets kept, newest first.
const KEEP: usize = 9;

#[derive(Clone)]
pub struct Set {
    pub paths: Vec<PathBuf>,
    pub cut: bool,
    /// How many of them are folders.
    pub dirs: usize,
    /// The folder they were copied from.
    pub from: PathBuf,
    /// When (seconds since the epoch).
    pub at: u64,
}

/// Note a copy or a cut: the newest first, the same set again moved up rather than kept
/// twice.
pub fn remember(list: &mut Vec<Set>, s: Set) {
    list.retain(|o| o.paths != s.paths);
    list.insert(0, s);
    list.truncate(KEEP);
}

/// A cut set is moved once pasted: there is nothing left to paste again.
pub fn pasted(list: &mut Vec<Set>, paths: &[PathBuf], cut: bool) {
    if cut {
        list.retain(|o| !(o.cut && o.paths == paths));
    }
}

/// "4 files from Pictures/Camera".
pub fn title(s: &Set) -> String {
    let n = s.paths.len();
    let what = match (s.dirs, n) {
        (0, 1) => "1 file".to_string(),
        (0, _) => format!("{n} files"),
        (d, _) if d == n => if n == 1 { "1 folder".to_string() } else { format!("{n} folders") },
        _ => format!("{n} items"),
    };
    let short = pp::home_short(&s.from);
    // The last two folders say where well enough; the whole path is in the tip.
    let parts: Vec<&str> = short.split('/').filter(|p| !p.is_empty()).collect();
    let from = if parts.len() > 2 { parts[parts.len() - 2..].join("/") } else { short.clone() };
    format!("{what} from {from}")
}

/// "IMG_2041.jpg, IMG_2042.jpg and 2 more".
pub fn names(s: &Set) -> String {
    let names: Vec<String> = s.paths.iter().take(2).map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).collect();
    let rest = s.paths.len().saturating_sub(2);
    if rest > 0 { format!("{} and {rest} more", names.join(", ")) } else { names.join(", ") }
}

pub struct State {
    sel: usize,
}

pub fn open(app: &mut App) {
    if app.pastes.is_empty() {
        app.toast("Nothing copied yet");
        return;
    }
    app.popup = pp::Popup::Pastes(State { sel: 0 });
}

/// A digit typed this frame, 1 to 9.
fn digit(ctx: &egui::Context) -> Option<usize> {
    ctx.input_mut(|i| {
        let mut got = None;
        i.events.retain(|e| match e {
            Event::Text(t) if got.is_none() => match t.chars().next().and_then(|c| c.to_digit(10)).filter(|d| (1..=9).contains(d)) {
                Some(d) => {
                    got = Some(d as usize - 1);
                    false
                }
                None => true,
            },
            _ => true,
        });
        got
    })
}

/// Paste set `k` where Paste would.
fn paste(app: &mut App, k: usize) {
    let Some(s) = app.pastes.get(k).cloned() else { return };
    let t = app.tab();
    let sel = t.target_entries();
    let dest = if t.selected.len() == 1 && sel.len() == 1 && sel[0].is_dir { sel[0].path.clone() } else { t.path.clone() };
    if crate::tab::is_place(&dest) {
        return;
    }
    if s.cut {
        pasted(&mut app.pastes, &s.paths, true);
        if app.clipboard.as_ref().is_some_and(|c| c.1 && c.0 == s.paths) {
            app.clipboard = None;
        }
    }
    app.begin_transfer(s.paths, dest, s.cut);
}

/// The popup. Returns false when it closes.
pub fn popup(app: &mut App, ctx: &egui::Context, st: &mut State) -> bool {
    let pal = app.pal;
    let k = pp::take_keys(ctx);
    if k.esc {
        return false;
    }
    let n = app.pastes.len();
    if n == 0 {
        return false;
    }
    pp::step(&mut st.sel, n, &k, |_| false);
    let mut pick = digit(ctx).filter(|&d| d < n).or((k.enter).then_some(st.sel));
    let fs = app.cfg.font_size;
    let small = FontId::proportional(fs - 1.5);
    let now = crate::jump::now() as i64;
    let row_h = (fs * 2.6 + 14.0).round();
    let (_, rect) = pp::centered(ctx, "paste from earlier", 560.0, &pal, |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        for (j, s) in app.pastes.iter().enumerate() {
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
            crate::ui::note_control(ui, r.shrink(1.0), "paste row");
            let on = j == st.sel;
            if on {
                ui.painter().rect_filled(r, CornerRadius::same(6), pal.selected);
            } else if resp.hovered() {
                ui.painter().rect_filled(r, CornerRadius::same(6), pal.hover);
            }
            let (tc, dc) = if on { (pal.on_sel(), pal.on_sel_dim()) } else { (pal.text, pal.text_dim) };
            // The digit in a key cap, as the marks show their letters.
            let kg = ui.painter().layout_no_wrap((j + 1).to_string(), FontId::monospace(fs - 1.5), tc);
            let kr = Rect::from_center_size(pos2(r.left() + 20.0, r.center().y), vec2(22.0, kg.size().y + 6.0));
            ui.painter().rect(kr, CornerRadius::same(4), pal.bg, Stroke::new(1.0, pal.border), StrokeKind::Inside);
            ui.painter().galley(kr.center() - kg.size() / 2.0, kg, tc);
            // A stack: the set is several things at once.
            let ic = Rect::from_center_size(pos2(kr.right() + 22.0, r.center().y), Vec2::splat(18.0));
            for i in 0..3 {
                let o = i as f32 * 3.0;
                let card = Rect::from_min_size(ic.min + vec2(o, 6.0 - o), vec2(11.0, 10.0));
                ui.painter().rect(card, CornerRadius::same(2), if on { pal.selected } else { pal.raised }, Stroke::new(1.2, if s.cut { pal.text_dim } else { pal.accent }), StrokeKind::Middle);
            }
            let x = ic.right() + 12.0;
            let age = crate::gitlog::ago(s.at as i64, now);
            let ag = ui.painter().layout_no_wrap(age, small.clone(), dc);
            let mut right = r.right() - 12.0 - ag.size().x;
            ui.painter().galley(pos2(right, r.top() + 8.0), ag, dc);
            if s.cut {
                let bg = ui.painter().layout_no_wrap("cut".into(), FontId::proportional(fs - 2.5), dc);
                let br = Rect::from_min_size(pos2(right - 10.0 - bg.size().x - 10.0, r.top() + 7.0), bg.size() + vec2(10.0, 4.0));
                ui.painter().rect(br, CornerRadius::same(4), egui::Color32::TRANSPARENT, Stroke::new(1.0, dc), StrokeKind::Inside);
                ui.painter().galley(br.min + vec2(5.0, 2.0), bg, dc);
                right = br.left();
            }
            let tg = crate::listing::elided(ui, &title(s), FontId::proportional(fs), right - 10.0 - x, tc);
            ui.painter().galley(pos2(x, r.top() + 6.0), tg, tc);
            let ng = crate::listing::elided(ui, &names(s), small.clone(), r.right() - 12.0 - x, dc);
            ui.painter().galley(pos2(x, r.bottom() - 6.0 - ng.size().y), ng, dc);
            if resp.on_hover_text(pp::home_short(&s.from)).clicked() {
                pick = Some(j);
            }
        }
        pp::hints(ui, &[("1–9", "paste"), ("Enter", "paste"), ("Esc", "close")], &pal);
    });
    if let Some(j) = pick {
        paste(app, j);
        return false;
    }
    !pp::clicked_outside(ctx, rect)
}
