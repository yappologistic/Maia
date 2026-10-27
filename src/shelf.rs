//! The shelf: files gathered from any number of folders, to move or copy together into
//! one. It lives for the session, as a strip along the bottom of the window shown only
//! while it holds something (and, while files are dragged, as a place to drop them).
//! What it holds is looked at again on a thread from time to time, never on a timer of
//! its own: a file that went away leaves the shelf.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use egui::{Color32, CornerRadius, FontId, Id, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

use crate::app::{Action, App, Drag};
use crate::fsx::Entry;
use crate::theme::{Glyph, Palette};

pub struct Item {
    pub path: PathBuf,
    /// Its details for the icon, once read (straight from the listing when added there).
    pub entry: Option<Entry>,
}

#[derive(Default)]
pub struct Shelf {
    pub items: Vec<Item>,
    /// A look at what is still there, running.
    check: Option<mpsc::Receiver<Vec<(PathBuf, Option<Entry>)>>>,
    checked: Option<Instant>,
    /// The strip's height as drawn (it slides in and out).
    pub shown_h: f32,
}

impl Shelf {
    pub fn has(&self, p: &Path) -> bool {
        self.items.iter().any(|i| i.path == p)
    }

    /// Add what isn't there yet; returns how many were added.
    pub fn add(&mut self, items: Vec<(PathBuf, Option<Entry>)>) -> usize {
        let mut n = 0;
        for (path, entry) in items {
            if !self.has(&path) {
                self.items.push(Item { path, entry });
                n += 1;
            }
        }
        if n > 0 {
            self.checked = None;
        }
        n
    }

    pub fn remove(&mut self, p: &Path) {
        self.items.retain(|i| i.path != p);
    }

    pub fn paths(&self) -> Vec<PathBuf> {
        self.items.iter().map(|i| i.path.clone()).collect()
    }

    /// Start a look at the items when the last one is a second old; take in one that
    /// finished. Called on frames that happen anyway.
    pub fn pump(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.check {
            match rx.try_recv() {
                Ok(seen) => {
                    self.check = None;
                    for (path, entry) in seen {
                        match entry {
                            Some(e) => {
                                if let Some(it) = self.items.iter_mut().find(|i| i.path == path) {
                                    it.entry = Some(e);
                                }
                            }
                            None => self.remove(&path),
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => self.check = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.items.is_empty() || self.check.is_some() || self.checked.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            return;
        }
        self.checked = Some(Instant::now());
        let paths = self.paths();
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let started = std::thread::Builder::new().name("shelf".into()).spawn(move || {
            let seen = paths.into_iter().map(|p| {
                let e = Entry::from_path(&p);
                (p, e)
            });
            let _ = tx.send(seen.collect());
            ctx.request_repaint();
        });
        if started.is_ok() {
            self.check = Some(rx);
        }
    }
}

/// The selection, onto the shelf.
pub fn add_selection(app: &mut App) {
    let t = app.tab();
    if crate::tab::is_place(&t.path) || t.archive.is_some() {
        app.toast("Only files and folders go on the shelf");
        return;
    }
    let items: Vec<(PathBuf, Option<Entry>)> = t.target_entries().iter().map(|e| (e.path.clone(), Some((*e).clone()))).collect();
    if items.is_empty() {
        app.toast("Select what to put on the shelf");
        return;
    }
    let n = app.shelf.add(items);
    if n == 0 {
        app.toast("Already on the shelf");
    }
}

/// Everything on the shelf into the active panel's folder, through the copy and move
/// jobs (their conflicts asked, their progress shown, undone as they are). A move
/// empties the shelf; a copy keeps it for the next folder.
pub fn put_here(app: &mut App, mv: bool) {
    let dest = app.panel().path();
    if crate::tab::is_place(&dest) || app.tab().archive.is_some() {
        app.toast("The shelf goes into a folder");
        return;
    }
    let srcs: Vec<PathBuf> = app.shelf.paths().into_iter().filter(|p| p.parent() != Some(dest.as_path()) || !mv).collect();
    if srcs.is_empty() {
        app.toast(if app.shelf.items.is_empty() { "The shelf is empty" } else { "Everything on the shelf is here already" });
        return;
    }
    if srcs.iter().any(|s| dest.starts_with(s)) {
        app.toast("A folder can't go inside itself");
        return;
    }
    if mv {
        app.shelf.items.clear();
    }
    app.begin_transfer(srcs, dest, mv);
}

/// The shelf's mark: a tray.
pub fn paint_tray(p: &egui::Painter, r: Rect, col: Color32) {
    let st = Stroke::new(1.3f32, col);
    let (l, rt, t, b) = (r.left() + 1.0, r.right() - 1.0, r.top() + r.height() * 0.3, r.bottom() - 1.0);
    let mid = t + (b - t) * 0.45;
    p.add(egui::Shape::line(vec![pos2(l, t), pos2(l, b), pos2(rt, b), pos2(rt, t)], st));
    let w = rt - l;
    p.add(egui::Shape::line(vec![pos2(l, mid), pos2(l + w * 0.3, mid), pos2(l + w * 0.38, mid + (b - mid) * 0.45), pos2(rt - w * 0.38, mid + (b - mid) * 0.45), pos2(rt - w * 0.3, mid), pos2(rt, mid)], st));
}

/// The strip's height for the font and Spacing in use.
pub fn height(app: &App) -> f32 {
    (app.cfg.font_size * 1.35 + 14.0 * app.cfg.spacing + 8.0).round()
}

/// Files being dragged over the window, from Maia or another app.
fn dragged(app: &App) -> Option<Vec<PathBuf>> {
    match &app.drag {
        Some(Drag::Files { paths, .. }) => Some(paths.clone()),
        _ => None,
    }
}

/// The strip along the bottom: the count, the items, and Copy Here / Move Here.
pub fn strip(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    // Laid out at its full height while it slides in or out, and cut to what shows.
    let shown = ui.max_rect();
    let rect = Rect::from_min_size(shown.min, vec2(shown.width(), height(app)));
    ui.set_clip_rect(shown.intersect(ui.clip_rect()));
    ui.painter().rect_filled(shown, CornerRadius::ZERO, pal.chrome);
    ui.painter().line_segment([rect.left_top(), rect.right_top()], Stroke::new(1.0f32, pal.border));
    let fs = app.cfg.font_size;
    let pointer = ui.ctx().pointer_hover_pos();
    let released = ui.input(|i| i.pointer.any_released());
    // Files dropped on it go on it.
    if let Some(paths) = dragged(app) {
        if pointer.is_some_and(|p| rect.contains(p)) {
            ui.painter().rect_stroke(rect.shrink(2.0), CornerRadius::same(6), Stroke::new(1.5f32, pal.accent), StrokeKind::Inside);
            if released {
                let shown = known_entries(app, &paths);
                app.shelf.add(shown);
            }
        }
    }
    let n = app.shelf.items.len();
    let inner = rect.shrink2(vec2(10.0, 0.0));
    let cy = inner.center().y;
    let chip_h = (fs * 1.35 + 8.0 * app.cfg.spacing).round().min(rect.height() - 6.0);
    // ---------------- left: the tray and the count, a handle for dragging everything
    let tray = Rect::from_center_size(pos2(inner.left() + 9.0, cy), Vec2::splat(16.0));
    let count = ui.painter().layout_no_wrap(if n == 1 { "1 item".into() } else { format!("{n} items") }, FontId::proportional(fs - 1.0), pal.text_dim);
    let head = Rect::from_min_max(pos2(inner.left() - 4.0, cy - chip_h / 2.0), pos2(tray.right() + 8.0 + count.size().x + 6.0, cy + chip_h / 2.0));
    let hr = ui.interact(head, Id::new("shelf all"), Sense::click_and_drag());
    crate::ui::note_control(ui, head, "shelf all");
    if hr.hovered() && n > 0 {
        ui.painter().rect_filled(head, CornerRadius::same(5), pal.hover);
    }
    paint_tray(ui.painter(), tray, if n > 0 { pal.text_dim } else { pal.accent });
    let cg = count.size();
    ui.painter().galley(pos2(tray.right() + 8.0, cy - cg.y / 2.0), count, pal.text_dim);
    if hr.drag_started() && n > 0 && app.drag.is_none() {
        app.drag = Some(Drag::Files { paths: app.shelf.paths(), from_panel: 0, right: false });
    }
    let hr = hr.on_hover_text("Drag everything on the shelf somewhere");
    let _ = hr;
    if n == 0 {
        return;
    }
    // ---------------- right: Clear, Copy Here, Move Here
    let mut actions = vec![];
    let mut right = inner.right();
    let button = |ui: &mut Ui, label: &str, accent: bool, what: &'static str, right: &mut f32| -> bool {
        let g = ui.painter().layout_no_wrap(label.to_string(), FontId::proportional(fs - 0.5), if accent { Color32::WHITE } else { pal.text });
        let r = Rect::from_min_max(pos2(*right - g.size().x - 22.0, cy - chip_h / 2.0), pos2(*right, cy + chip_h / 2.0));
        if r.left() < head.right() + 8.0 {
            return false;
        }
        *right = r.left() - 6.0;
        let resp = ui.interact(r, Id::new(("shelf button", what)), Sense::click());
        crate::ui::note_control(ui, r, what);
        let fill = if accent { if resp.hovered() { pal.accent.gamma_multiply(0.85) } else { pal.accent } } else if resp.hovered() { pal.hover } else { pal.card() };
        ui.painter().rect(r, CornerRadius::same(6), fill, if accent { Stroke::NONE } else { Stroke::new(1.0f32, pal.card_line()) }, StrokeKind::Inside);
        ui.painter().galley(pos2(r.center().x - g.size().x / 2.0, cy - g.size().y / 2.0), g, pal.text);
        resp.clicked()
    };
    let clear = Rect::from_center_size(pos2(right - 12.0, cy), Vec2::splat(22.0));
    right = clear.left() - 6.0;
    let cr = ui.interact(clear, Id::new("shelf clear"), Sense::click());
    crate::ui::note_control(ui, clear, "shelf clear");
    if cr.hovered() {
        ui.painter().rect_filled(clear, CornerRadius::same(5), pal.hover);
    }
    crate::theme::paint_glyph(ui.painter(), clear.shrink(5.0), Glyph::Close, if cr.hovered() { pal.text } else { pal.text_dim });
    if cr.on_hover_text("Clear the shelf").clicked() {
        actions.push(Action::Cmd(crate::commands::Cmd::ClearShelf));
    }
    let here = app.panels.get(&app.active).map(|p| p.path()).is_some_and(|p| !crate::tab::is_place(&p));
    if here {
        if button(ui, "Move Here", true, "shelf move", &mut right) {
            actions.push(Action::Cmd(crate::commands::Cmd::ShelfMoveHere));
        }
        if button(ui, "Copy Here", false, "shelf copy", &mut right) {
            actions.push(Action::Cmd(crate::commands::Cmd::ShelfCopyHere));
        }
    }
    // ---------------- the items, as chips that scroll sideways
    let lane = Rect::from_min_max(pos2(head.right() + 8.0, rect.top()), pos2(right - 4.0, rect.bottom()));
    if lane.width() > 40.0 {
        let mut lui = ui.new_child(egui::UiBuilder::new().max_rect(lane).layout(egui::Layout::left_to_right(egui::Align::Center)));
        lui.set_clip_rect(lane.intersect(ui.clip_rect()));
        egui::ScrollArea::horizontal().id_salt("shelf lane").scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).auto_shrink([false, false]).show(&mut lui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for it in &app.shelf.items {
                    let resp = chip(ui, it, chip_h, fs, &pal);
                    let x = Rect::from_center_size(pos2(resp.rect.right() - 12.0, cy), Vec2::splat(16.0));
                    let over_x = pointer.is_some_and(|p| x.contains(p));
                    if resp.hovered() {
                        ui.painter().rect_filled(x, CornerRadius::same(4), if over_x { pal.hover } else { Color32::TRANSPARENT });
                        crate::theme::paint_glyph(ui.painter(), x.shrink(4.0), Glyph::Close, if over_x { pal.text } else { pal.text_dim });
                    }
                    crate::ui::note_control(ui, resp.rect, "shelf item");
                    if resp.clicked() && over_x {
                        actions.push(Action::Run(remove_one(it.path.clone())));
                    } else if resp.clicked() {
                        actions.push(Action::Run(show_one(it.path.clone())));
                    }
                    if resp.drag_started() {
                        actions.push(Action::Run(drag_one(it.path.clone())));
                    }
                    let tip = it.path.display().to_string();
                    let _ = resp.on_hover_text(tip);
                }
            });
        });
    }
    app.actions.extend(actions);
}

fn remove_one(p: PathBuf) -> Box<dyn FnOnce(&mut App)> {
    Box::new(move |app: &mut App| app.shelf.remove(&p))
}

/// The item selected in its folder, in the active panel.
fn show_one(p: PathBuf) -> Box<dyn FnOnce(&mut App)> {
    Box::new(move |app: &mut App| {
        let Some(dir) = p.parent().map(Path::to_path_buf) else { return };
        let panel = app.active;
        app.navigate(panel, dir.clone());
        let t = app.tab();
        t.pending_restore = Some(crate::tab::HistState { path: dir, cursor: Some(p.clone()), selected: vec![p], scroll: 0.0 });
        t.scroll_to_cursor = true;
    })
}

fn drag_one(p: PathBuf) -> Box<dyn FnOnce(&mut App)> {
    Box::new(move |app: &mut App| {
        if app.drag.is_none() {
            app.drag = Some(Drag::Files { paths: vec![p], from_panel: 0, right: false });
        }
    })
}

/// One item: its icon and name, room at the end for its ×.
fn chip(ui: &mut Ui, it: &Item, h: f32, fs: f32, pal: &Palette) -> egui::Response {
    let name = it.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| it.path.display().to_string());
    let g = crate::listing::elided(ui, &name, FontId::proportional(fs - 0.5), 180.0, pal.text);
    let w = 8.0 + 16.0 + 6.0 + g.size().x + 26.0;
    let (r, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click_and_drag());
    let fill = if resp.hovered() { pal.hover } else { pal.card() };
    ui.painter().rect(r, CornerRadius::same(6), fill, Stroke::new(1.0f32, pal.card_line()), StrokeKind::Inside);
    let ic = Rect::from_center_size(pos2(r.left() + 16.0, r.center().y), Vec2::splat(16.0));
    match &it.entry {
        Some(e) => {
            if !crate::icons::entry(ui.painter(), ic, e) {
                crate::theme::paint_icon(ui.painter(), ic, crate::theme::categorize(e.ext(), e.is_dir, e.is_executable()), e.ext(), e.is_link);
            }
        }
        None => crate::theme::paint_icon(ui.painter(), ic, crate::theme::FileCat::Other, "", false),
    }
    ui.painter().galley(pos2(ic.right() + 6.0, r.center().y - g.size().y / 2.0), g, pal.text);
    resp
}

/// Entries for dropped paths, from a listing that shows them (the shelf's own look reads
/// the rest on its thread).
fn known_entries(app: &App, paths: &[PathBuf]) -> Vec<(PathBuf, Option<Entry>)> {
    paths
        .iter()
        .map(|p| {
            let e = app.panels.values().flat_map(|pn| pn.tabs.iter()).find_map(|t| t.entries.iter().find(|e| e.path == *p).cloned());
            (p.clone(), e)
        })
        .collect()
}

/// While files are dragged and the shelf is empty: a small place to drop them, over the
/// middle of the bottom edge.
pub fn drop_spot(app: &mut App, ctx: &egui::Context) {
    let Some(paths) = dragged(app).filter(|_| app.shelf.items.is_empty()) else { return };
    let pal = app.pal;
    let fs = app.cfg.font_size;
    let screen = ctx.content_rect();
    let g = ctx.fonts_mut(|f| f.layout_no_wrap("Shelf".into(), FontId::proportional(fs - 0.5), pal.text));
    let size = vec2(g.size().x + 16.0 + 30.0, height(app) - 8.0);
    let r = Rect::from_center_size(pos2(screen.center().x, screen.bottom() - 10.0 - size.y / 2.0), size);
    let over = ctx.pointer_hover_pos().is_some_and(|p| r.contains(p));
    let released = ctx.input(|i| i.pointer.any_released());
    egui::Area::new(Id::new("shelf drop")).order(egui::Order::Foreground).fixed_pos(r.min).interactable(false).show(ctx, |ui| {
        let p = ui.painter();
        p.rect(r, CornerRadius::same((size.y / 2.0) as u8), if over { pal.selected } else { pal.raised }, Stroke::new(1.5f32, if over { pal.accent } else { pal.border }), StrokeKind::Inside);
        paint_tray(p, Rect::from_center_size(pos2(r.left() + 20.0, r.center().y), Vec2::splat(15.0)), if over { pal.accent } else { pal.text_dim });
        p.galley(pos2(r.left() + 34.0, r.center().y - g.size().y / 2.0), g, pal.text);
    });
    if over && released {
        let shown = known_entries(app, &paths);
        app.shelf.add(shown);
        app.drag = None;
    }
}
