//! Compare Files: how two files differ, as a unified diff shown in the inspector in place
//! of the preview. git's diff does the comparing, on a thread; it takes any two files,
//! in a repository or not.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use egui::{Align, CornerRadius, FontId, Layout, Rect, Sense, Stroke, Ui, UiBuilder, Vec2, pos2, vec2};

use crate::listing::Env;
use crate::theme::Glyph;

/// More than this many lines of diff are not shown.
const MAX_LINES: usize = 50_000;

#[derive(Debug, Default, PartialEq)]
pub struct Diff {
    /// From the first hunk on; empty when the files are the same.
    pub lines: Vec<String>,
    pub added: usize,
    pub removed: usize,
    pub truncated: bool,
}

/// git's output for two files, from its first hunk: what a person reads. Its header
/// names the files, which the inspector's title already does.
pub fn parse(out: &str) -> Result<Diff, String> {
    if out.lines().any(|l| l.starts_with("Binary files ")) {
        return Err("They're binary files, and they differ".into());
    }
    let mut d = Diff::default();
    for l in out.lines().skip_while(|l| !l.starts_with("@@")) {
        if d.lines.len() == MAX_LINES {
            d.truncated = true;
            d.lines.push(format!("… the diff goes on past {} lines", crate::table::fmt_count(MAX_LINES)));
            break;
        }
        match l.as_bytes().first() {
            Some(b'+') => d.added += 1,
            Some(b'-') => d.removed += 1,
            _ => {}
        }
        d.lines.push(l.to_string());
    }
    Ok(d)
}

/// Compare `a` with `b`: lines of `b` that `a` lacks are added.
pub fn diff(a: &Path, b: &Path) -> Result<Diff, String> {
    let o = crate::media::tool("git")
        .args(["-c", "core.quotePath=false", "diff", "--no-index", "--no-color", "--no-ext-diff", "--"])
        .arg(a)
        .arg(b)
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|_| "Comparing files needs git".to_string())?;
    // 0: the same, 1: they differ, anything else went wrong. 1 is also what git says
    // when it can't read one of them, then with nothing on its output.
    let err = String::from_utf8_lossy(&o.stderr);
    match o.status.code() {
        Some(0) | Some(1) if !o.stdout.is_empty() || err.trim().is_empty() => parse(&String::from_utf8_lossy(&o.stdout)),
        _ => Err(err.lines().next().unwrap_or("git couldn't compare them").trim_start_matches("error: ").to_string()),
    }
}

pub struct Compare {
    pub a: PathBuf,
    pub b: PathBuf,
    rx: Option<mpsc::Receiver<Result<Diff, String>>>,
    pub got: Option<Result<Diff, String>>,
    wrapped: Option<crate::inspector::Wrapped>,
    side: crate::sidediff::View,
}

impl Compare {
    pub fn start(a: PathBuf, b: PathBuf, ctx: &egui::Context) -> Compare {
        let (tx, rx) = mpsc::channel();
        let (pa, pb, c) = (a.clone(), b.clone(), ctx.clone());
        let started = std::thread::Builder::new().name("compare".into()).spawn(move || {
            let _ = tx.send(diff(&pa, &pb));
            c.request_repaint();
        });
        let got = started.is_err().then(|| Err("Couldn't start comparing".to_string()));
        Compare { a, b, rx: Some(rx), got, wrapped: None, side: Default::default() }
    }

    fn poll(&mut self) {
        if let Some(r) = self.rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.got = Some(r);
            self.rx = None;
            self.wrapped = None;
        }
    }

    /// The names in the title: just the files', or with their folders when both have
    /// the same name.
    pub fn title(&self) -> String {
        let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let (na, nb) = (name(&self.a), name(&self.b));
        if na != nb {
            return format!("{na} → {nb}");
        }
        let with_dir = |p: &Path| p.parent().and_then(|d| d.file_name()).map_or(name(p), |d| format!("{}/{}", d.to_string_lossy(), name(p)));
        format!("{} → {}", with_dir(&self.a), with_dir(&self.b))
    }
}

/// Draw the comparison; false when it was closed.
pub fn view(ui: &mut Ui, rect: Rect, c: &mut Compare, wrap: &mut bool, env: &mut Env) -> bool {
    let pal = *env.pal;
    c.poll();
    ui.painter().rect_filled(rect, CornerRadius::ZERO, pal.bg);
    let hdr = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + 30.0));
    ui.painter().line_segment([hdr.left_bottom(), hdr.right_bottom()], Stroke::new(1.0, pal.border));
    let mut hui = ui.new_child(UiBuilder::new().max_rect(hdr.shrink2(vec2(6.0, 3.0))).layout(Layout::left_to_right(Align::Center)));
    hui.set_clip_rect(hdr.intersect(ui.clip_rect()));
    hui.spacing_mut().item_spacing.x = 2.0;
    if crate::ui::glyph_button(&mut hui, Glyph::Close, "Close the comparison", true, &pal).clicked() {
        return false;
    }
    hui.add_space(4.0);
    let side = env.cfg.diff_side;
    crate::inspector::seg_frame(&mut hui, &pal, |ui| {
        if diff_layout(ui, side, &pal) {
            env.actions.push(crate::app::Action::Run(Box::new(|app| toggle_side(app))));
        }
        if !side && crate::inspector::seg_button(ui, *wrap, "Wrap lines", &pal, |p, r, col| crate::theme::paint_glyph(p, r, Glyph::Wrap, col)) {
            *wrap = !*wrap;
            c.wrapped = None;
        }
    });
    let counts = match &c.got {
        Some(Ok(d)) if !d.lines.is_empty() => Some((d.added, d.removed)),
        _ => None,
    };
    let title = c.title();
    hui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let room = ui.available_width() - 6.0 - 36.0;
        if room < 24.0 {
            return;
        }
        let g = crate::listing::elided(ui, &title, FontId::proportional(12.0), room.min(320.0), pal.text);
        let (r, _) = ui.allocate_exact_size(vec2(g.size().x + 36.0, crate::kit::CHIP_H), Sense::hover());
        crate::kit::chip_face(ui.painter(), r, false, false, None, crate::kit::round(), &pal);
        crate::theme::paint_symbol(ui.painter(), Rect::from_center_size(pos2(r.left() + 16.0, r.center().y), Vec2::splat(12.0)), "git-compare", pal.text);
        ui.painter().galley(pos2(r.left() + 26.0, r.center().y - g.size().y / 2.0), g, pal.text);
        if let Some((add, del)) = counts.filter(|_| ui.available_width() > 90.0) {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(format!("−{del}")).size(11.5).color(pal.danger));
            ui.label(egui::RichText::new(format!("+{add}")).size(11.5).color(crate::gitstatus::GREEN));
        }
    });
    let body = Rect::from_min_max(pos2(rect.left(), hdr.bottom() + 1.0), rect.max);
    let mut bui = ui.new_child(UiBuilder::new().max_rect(body).id_salt(("compare", env.panel_id)));
    bui.set_clip_rect(body.intersect(ui.clip_rect()));
    let note = match &c.got {
        None => Some("Comparing…".to_string()),
        Some(Err(e)) => Some(e.clone()),
        Some(Ok(d)) if d.lines.is_empty() => Some("The files are the same".to_string()),
        Some(Ok(_)) => None,
    };
    if let Some(n) = note {
        bui.painter().text(body.center(), egui::Align2::CENTER_CENTER, n, FontId::proportional(env.cfg.font_size - 1.0), pal.text_dim);
        return true;
    }
    if let Some(Ok(d)) = &c.got {
        if side {
            crate::sidediff::view(&mut bui, body, &d.lines, &mut c.side, env.cfg.inspector_font_size, env.cfg.preview_line_spacing, &pal);
        } else {
            let wrapped = if *wrap { Some(&mut c.wrapped) } else { None };
            crate::inspector::text_view(&mut bui, body, &d.lines, false, None, false, wrapped, env.cfg.inspector_font_size, &pal, true, env.cfg.preview_line_spacing);
        }
    }
    true
}

/// The toolbar button that puts a diff's sides beside each other; true when clicked.
pub fn diff_layout(ui: &mut Ui, side: bool, pal: &crate::theme::Palette) -> bool {
    crate::inspector::seg_button(ui, side, "Side by side", pal, |p, r, col| {
        crate::theme::paint_symbol(p, r, "columns-2", col);
    })
}

/// Diffs side by side, or one side under the other again: remembered.
pub fn toggle_side(app: &mut crate::app::App) {
    app.cfg.diff_side = !app.cfg.diff_side;
    app.cfg_dirty = true;
}
