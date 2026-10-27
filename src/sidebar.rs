//! Quick access sidebar: header (menu, collapse), filter, then Recents, Bookmarks, Smart
//! Folders, Tags, Storage (with fill bars), Network and Places, each collapsible.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{Align2, Color32, CornerRadius, FontId, Id, Rect, Sense, Stroke, Ui, Vec2, pos2, vec2};

use crate::app::{Action, App, Drag};
use crate::fsx;
use crate::popups::Popup;
use crate::tab;
use crate::theme::{Glyph, paint_glyph};

struct Cache {
    at: Option<Instant>,
    mounts: Vec<(PathBuf, String, u64, u64)>,
    recents: Vec<PathBuf>,
    places: Vec<(String, PathBuf, Glyph)>,
    /// Bookmark status from the last refresh: (exists, is a symlink).
    marks: std::collections::HashMap<PathBuf, (bool, bool)>,
    /// Sidebar paths that are files (for hiding extensions).
    files: std::collections::HashSet<PathBuf>,
    filter: String,
    /// Refresh running on a worker thread (anything touching the disk lives there: a
    /// dead network mount must never stall drawing).
    job: Option<std::sync::mpsc::Receiver<Snapshot>>,
    /// Hash of the last refresh's inputs (recents, bookmarks); a change triggers a new one.
    inputs: u64,
    /// Bumped whenever a refresh lands, so dependents (GoTo) know to rebuild.
    generation: u64,
    /// Block devices (lsblk), and a counter bumped whenever they change (plugged in,
    /// mounted, ejected) so Computer views reload.
    drives: Vec<crate::drives::Drive>,
    drives_gen: u64,
    /// Mounted network shares and attached phones (gio).
    net: Vec<crate::network::Place>,
}

struct Snapshot {
    mounts: Vec<(PathBuf, String, u64, u64)>,
    recents: Vec<PathBuf>,
    places: Vec<(String, PathBuf, Glyph)>,
    marks: std::collections::HashMap<PathBuf, (bool, bool)>,
    files: std::collections::HashSet<PathBuf>,
    drives: Vec<crate::drives::Drive>,
    net: Vec<crate::network::Place>,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache { at: None, mounts: vec![], recents: vec![], places: vec![], marks: Default::default(), files: Default::default(), filter: String::new(), job: None, inputs: 0, generation: 0, drives: vec![], drives_gen: 0, net: vec![] });
}

/// Drives as of the last refresh.
pub fn drives() -> Vec<crate::drives::Drive> {
    CACHE.with(|c| c.borrow().drives.clone())
}

pub fn net_places() -> Vec<crate::network::Place> {
    CACHE.with(|c| c.borrow().net.clone())
}

/// The mounted network place `path` is in: its folder and its name.
pub fn net_root(path: &Path) -> Option<(PathBuf, String)> {
    CACHE.with(|c| c.borrow().net.iter().find_map(|n| n.path.as_ref().filter(|r| path.starts_with(r)).map(|r| (r.clone(), n.name.clone()))))
}

pub fn drives_generation() -> u64 {
    CACHE.with(|c| c.borrow().drives_gen)
}

/// Refresh soon (after mounting or ejecting), not on the next 4 s tick.
pub fn invalidate() {
    CACHE.with(|c| c.borrow_mut().at = None);
}

pub fn trash_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| fsx::home().join(".local/share")).join("Trash/files")
}

/// Places in display order (shared with GoTo).
pub fn places() -> Vec<(String, PathBuf, Glyph)> {
    let mut v = vec![("Computer".to_string(), PathBuf::from(tab::COMPUTER), Glyph::Monitor), ("Home".to_string(), fsx::home(), Glyph::Home)];
    let mut add = |name: &str, p: Option<PathBuf>, g: Glyph| {
        if let Some(p) = p {
            if p.is_dir() && p != fsx::home() {
                v.push((name.to_string(), p, g));
            }
        }
    };
    add("Desktop", dirs::desktop_dir(), Glyph::Desktop);
    add("Downloads", dirs::download_dir(), Glyph::Down);
    add("Documents", dirs::document_dir(), Glyph::Docs);
    add("Music", dirs::audio_dir(), Glyph::Music);
    add("Pictures", dirs::picture_dir(), Glyph::Pics);
    add("Videos", dirs::video_dir(), Glyph::Video);
    let td = trash_dir();
    let _ = std::fs::create_dir_all(&td);
    v.push(("Trash".into(), td, Glyph::Trash));
    v.push(("Filesystem".into(), PathBuf::from("/"), Glyph::Root));
    v
}

/// What the last background refresh found, for views that must not touch the disk while
/// drawing (GoTo). `dirs` holds every path known to be an existing folder.
pub struct Known {
    pub generation: u64,
    pub mounts: Vec<(PathBuf, String, u64, u64)>,
    pub recents: Vec<PathBuf>,
    pub places: Vec<(String, PathBuf, Glyph)>,
    pub dirs: std::collections::HashSet<PathBuf>,
}

pub fn known(app: &App, ctx: &egui::Context) -> Known {
    refresh(app, ctx);
    CACHE.with(|c| {
        let c = c.borrow();
        let mut dirs: std::collections::HashSet<PathBuf> = c.recents.iter().cloned().collect();
        dirs.extend(c.places.iter().map(|p| p.1.clone()));
        dirs.extend(c.mounts.iter().map(|m| m.0.clone()));
        dirs.extend(c.marks.iter().filter(|(p, (exists, _))| *exists && !c.files.contains(*p)).map(|(p, _)| p.clone()));
        Known { generation: c.generation, mounts: c.mounts.clone(), recents: c.recents.clone(), places: c.places.clone(), dirs }
    })
}

/// Where the mounted drives are, as of the last refresh.
pub fn mount_points() -> Vec<PathBuf> {
    CACHE.with(|c| c.borrow().mounts.iter().map(|m| m.0.clone()).collect())
}

/// Generation of the cached refresh (cheap; no copies).
pub fn generation() -> u64 {
    CACHE.with(|c| c.borrow().generation)
}

pub fn refresh(app: &App, ctx: &egui::Context) {
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        // Pick up a finished refresh.
        if let Some(rx) = &c.job {
            match rx.try_recv() {
                Ok(s) => {
                    c.mounts = s.mounts;
                    c.recents = s.recents;
                    c.places = s.places;
                    c.marks = s.marks;
                    c.files = s.files;
                    if c.drives != s.drives || c.net != s.net {
                        c.drives = s.drives;
                        c.net = s.net;
                        c.drives_gen += 1;
                    }
                    c.job = None;
                    c.generation += 1;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => c.job = None,
                Err(_) => return,
            }
        }
        let hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            app.ud.recents.hash(&mut h);
            app.ud.bookmarks.hash(&mut h);
            h.finish()
        };
        let stale = c.at.map(|t| t.elapsed() >= Duration::from_secs(4)).unwrap_or(true);
        if c.job.is_some() || !(stale || hash != c.inputs) {
            return;
        }
        c.at = Some(Instant::now());
        c.inputs = hash;
        let inputs = (app.ud.recents.clone(), app.ud.bookmarks.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        let (table, tools) = (crate::drives::mounts_table(), crate::drives::Tools::current());
        std::thread::spawn(move || {
            let (recent_in, marks_in) = inputs;
            let drives = tools.as_ref().map(crate::drives::list).unwrap_or_default();
            let net = tools.as_ref().filter(|t| t.has("gio")).map(crate::network::places).unwrap_or_default();
            let mounts = fsx::mounts_in(&table)
                .into_iter()
                .map(|m| {
                    let (t, f) = fsx::disk_space(&m.path).unwrap_or((0, 0));
                    (m.path.clone(), tab::volume_label(&m.device, &m.path), t, f)
                })
                .collect();
            // Every recent that still exists (the sidebar shows 10; GoTo searches them all).
            let recents: Vec<PathBuf> = recent_in.into_iter().filter(|p| p.is_dir()).collect();
            let mut files = std::collections::HashSet::new();
            let mut marks = std::collections::HashMap::new();
            for b in &marks_in {
                let m = std::fs::symlink_metadata(b);
                let exists = tab::is_place(b) || m.is_ok();
                let link = m.as_ref().map(|m| m.file_type().is_symlink()).unwrap_or(false);
                if b.is_file() {
                    files.insert(b.clone());
                }
                marks.insert(b.clone(), (exists, link));
            }
            let _ = tx.send(Snapshot { mounts, recents, places: places(), marks, files, drives, net });
            ctx.request_repaint();
        });
        c.job = Some(rx);
    });
}

pub fn sidebar(app: &mut App, ui: &mut Ui) {
    refresh(app, ui.ctx());
    let pal = app.pal;
    let current = app.panels.get(&app.active).map(|p| p.path()).unwrap_or_default();
    let font = FontId::proportional(app.cfg.font_size);
    let row_h = (app.cfg.font_size + 12.0 * app.cfg.spacing).round();
    let mut actions: Vec<Action> = vec![];
    let released = ui.input(|i| i.pointer.any_released());
    let pointer = ui.ctx().pointer_hover_pos();
    let mods = ui.input(|i| i.modifiers);
    let active = app.active;
    // ---------------- header: menu, drag area, collapse
    let full = ui.max_rect();
    let hdr = Rect::from_min_size(full.min, vec2(full.width(), crate::ui::TITLE_H));
    let drag = ui.interact(hdr, Id::new("side_hdr_drag"), Sense::click_and_drag());
    if drag.drag_started() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if drag.double_clicked() {
        let m = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!m));
    }
    let mut hui = ui.new_child(egui::UiBuilder::new().max_rect(hdr.shrink2(vec2(8.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    let r = crate::ui::glyph_button(&mut hui, Glyph::Menu, "", true, &pal);
    if crate::ui::cmd_tip(r, None, crate::commands::Cmd::MainMenu, &app.keymap, &pal).clicked() {
        app.popup = if matches!(app.popup, Popup::MainMenu) { Popup::None } else { Popup::MainMenu };
    }
    hui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let r = crate::ui::glyph_button(ui, Glyph::CollapseLeft, "", true, &pal);
        if crate::ui::cmd_tip(r, Some("Hide sidebar".into()), crate::commands::Cmd::ToggleSidebar, &app.keymap, &pal).clicked() {
            app.sidebar = false;
        }
    });
    ui.allocate_rect(hdr, Sense::hover());
    // ---------------- filter
    let mut filter = CACHE.with(|c| c.borrow().filter.clone());
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        let w = ui.available_width() - 8.0;
        // Borderless filter row (icon + text) with a divider under it.
        // As tall as its text needs at larger font sizes, 26 points at least.
        let row_h = (ui.text_style_height(&egui::TextStyle::Body) + 8.0).max(26.0).round();
        let (r, _) = ui.allocate_exact_size(vec2(w, row_h), Sense::hover());
        paint_glyph(ui.painter(), Rect::from_center_size(pos2(r.left() + 10.0, r.center().y), Vec2::splat(13.0)), Glyph::Search, if filter.is_empty() { pal.text_dim } else { pal.accent });
        // The text's capitals on the icon's middle (see `cap_nudge`).
        let nudge = crate::ui::cap_nudge(ui, &egui::TextStyle::Body.resolve(ui.style()));
        let er = Rect::from_min_max(pos2(r.left() + 24.0, r.top() + 2.0 + nudge), pos2(r.right() - 6.0, r.bottom() - 2.0 + nudge));
        let mut cu = ui.new_child(egui::UiBuilder::new().max_rect(er).layout(egui::Layout::left_to_right(egui::Align::Center)));
        cu.spacing_mut().interact_size.y = er.height();
        let resp = cu.add(egui::TextEdit::singleline(&mut filter).hint_text(egui::RichText::new("Filter sidebar…").color(pal.text_dim)).frame(crate::ui::bare_edit()).desired_width(er.width()).align(egui::Align2::LEFT_CENTER));
        if resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            filter.clear();
        }
    });
    {
        let y = ui.cursor().min.y + 3.0;
        let x = ui.max_rect().x_range();
        ui.painter().line_segment([pos2(x.min, y), pos2(x.max, y)], Stroke::new(1.0, pal.border));
        ui.add_space(6.0);
    }
    CACHE.with(|c| c.borrow_mut().filter = filter.clone());
    let fq = crate::fuzzy::Query::new(&filter);
    let matches = |name: &str| fq.is_empty() || crate::fuzzy::score(&fq, &name.to_lowercase()).is_some();
    ui.add_space(4.0);

    egui::ScrollArea::vertical().auto_shrink([false, false]).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded).show(ui, |ui| {
        let width = ui.available_width();
        let section = |ui: &mut Ui, app: &mut App, key: &str, title: &str, _glyph: Glyph| -> bool {
            let collapsed = app.ud.sidebar_collapsed.iter().any(|s| s == key) && fq.is_empty();
            // A small dim label over its places, a step of air above it: a header, not
            // another row. Its left edge is the places' icons'.
            let fs = (app.cfg.font_size - 1.5).max(10.0);
            let (r, resp) = ui.allocate_exact_size(vec2(width, (fs + 18.0).round()), Sense::click());
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::DisclosureTriangle, true, title));
            let col = if resp.hovered() { pal.text } else { pal.text_dim };
            let text_y = r.bottom() - 6.0 - fs * 0.6;
            ui.painter().text(pos2(r.left() + 28.0, text_y), Align2::LEFT_CENTER, title, crate::theme::semibold(fs), col);
            // The chevron shows the section can fold: always when folded, else when pointed at.
            if collapsed || resp.hovered() {
                let c = Rect::from_center_size(pos2(r.right() - 16.0, text_y), Vec2::splat(11.0));
                paint_glyph(ui.painter(), c, if collapsed { Glyph::ChevronRight } else { Glyph::ChevronDown }, col);
            }
            if resp.clicked() {
                if app.ud.sidebar_collapsed.iter().any(|s| s == key) {
                    app.ud.sidebar_collapsed.retain(|s| s != key);
                } else {
                    app.ud.sidebar_collapsed.push(key.to_string());
                }
                app.ud_dirty = true;
            }
            !collapsed
        };
        // Row painter shared by all sections. Returns the response.
        // The place whose context menu is open stays highlighted.
        let ctx_target: Option<PathBuf> = match &app.popup {
            Popup::Ctx(st) => st.sidebar.clone(),
            _ => None,
        };
        let item = |ui: &mut Ui, label: &str, glyph: Option<Glyph>, path: &Path, dim: bool, link: bool| -> egui::Response {
            let (r, resp) = ui.allocate_exact_size(vec2(width, row_h), Sense::click_and_drag());
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
            // The place you are in wears a quiet pill, a step off the sidebar, so it is found
            // at a glance without competing with the list's selection.
            let here = path == current.as_path();
            let r2 = r.shrink2(vec2(6.0, 1.0));
            if ctx_target.as_deref() == Some(path) {
                ui.painter().rect_filled(r2, CornerRadius::same(6), pal.selected);
            } else if resp.hovered() {
                ui.painter().rect_filled(r2, CornerRadius::same(6), pal.row_hover);
            } else if here {
                ui.painter().rect_filled(r2, CornerRadius::same(6), pal.sidebar.lerp_to_gamma(pal.text, if pal.dark { 0.08 } else { 0.06 }));
            }
            let ic = Rect::from_center_size(pos2(r.left() + 36.0, r.center().y), Vec2::splat(15.0));
            match glyph {
                Some(g) => paint_glyph(ui.painter(), ic, g, if here { pal.text } else { pal.text_dim }),
                None => {
                    // A folder with a colour label is drawn in it, as in the listing.
                    let label = crate::tags::get(path, |t| t.iter().find_map(|x| crate::tags::color_of(x))).flatten();
                    if let Some(c) = label {
                        crate::theme::paint_folder(ui.painter(), ic, c);
                    } else if !crate::icons::dir(ui.painter(), ic, path) {
                        crate::theme::paint_icon(ui.painter(), ic, crate::theme::FileCat::Folder, "", link);
                    }
                }
            }
            let g = crate::listing::elided(ui, label, font.clone(), r.width() - 62.0, if dim { pal.text_dim.gamma_multiply(0.6) } else { pal.text });
            ui.painter().galley(pos2(r.left() + 52.0, r.center().y - g.size().y / 2.0), g, pal.text);
            let what = if tab::is_computer(path) {
                "Computer".into()
            } else if path == crate::ctxmenu::smart_key(crate::smart::TIMELINE) {
                "Files changed in the last month".into()
            } else {
                path.display().to_string()
            };
            resp.on_hover_text(format!("{what}\nCtrl+click: open in a new tab"))
        };
        let nav = |actions: &mut Vec<Action>, resp: &egui::Response, path: &Path| {
            if resp.clicked() {
                if mods.ctrl {
                    actions.push(Action::NewTab { panel: active, path: path.to_path_buf(), select: None, focus: true });
                } else {
                    actions.push(Action::Navigate { panel: active, path: path.to_path_buf() });
                }
            }
            if resp.middle_clicked() {
                actions.push(Action::NewTab { panel: active, path: path.to_path_buf(), select: None, focus: false });
            }
        };
        let drop_files = |ui: &Ui, drag: &Option<Drag>, actions: &mut Vec<Action>, resp: &egui::Response, path: &Path| {
            if let Some(Drag::Files { paths, .. }) = drag {
                if pointer.map(|p| resp.rect.contains(p)).unwrap_or(false) && path.is_dir() {
                    ui.painter().rect_stroke(resp.rect.shrink2(vec2(6.0, 1.0)), CornerRadius::same(6), Stroke::new(1.5, pal.accent), egui::StrokeKind::Inside);
                    if released {
                        actions.push(Action::Drop { paths: paths.clone(), dest: path.to_path_buf(), copy: mods.ctrl, link: false });
                    }
                }
            }
        };
        // Right-click opens the same searchable command menu as the file list.
        let drives = drives();
        let net = net_places();
        let ctx_menu = |resp: &egui::Response, actions: &mut Vec<Action>, path: &Path, bookmark: bool| {
            if resp.secondary_clicked() {
                let pos = resp.interact_pointer_pos().unwrap_or(resp.rect.left_bottom());
                let drive = match tab::drive_dev(path) {
                    Some(dev) => drives.iter().find(|d| d.dev == dev).cloned(),
                    None => crate::drives::at_mount(&drives, path).cloned(),
                };
                let network = net.iter().find(|n| n.path.as_deref() == Some(path)).map(|n| n.uri.clone());
                actions.push(Action::Open(crate::ctxmenu::build_place(active, path.to_path_buf(), bookmark, drive, network, pos)));
            }
        };
        let show_ext = app.cfg.show_ext;
        let (files, marks) = CACHE.with(|c| {
            let c = c.borrow();
            (c.files.clone(), c.marks.clone())
        });
        let name_of = |p: &Path| -> String {
            if tab::is_computer(p) {
                return "Computer".into();
            }
            if !show_ext && files.contains(p) {
                if let Some(stem) = p.file_stem() {
                    return stem.to_string_lossy().into_owned();
                }
            }
            p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
        };

        // ---------------- recents
        let recents: Vec<PathBuf> = CACHE.with(|c| c.borrow().recents.iter().take(10).cloned().collect());
        if section(ui, app, "recents", "Recents", Glyph::Clock) {
            for p in recents.iter() {
                let name = name_of(p);
                if !matches(&name) {
                    continue;
                }
                let resp = item(ui, &name, None, p, false, false);
                nav(&mut actions, &resp, p);
                drop_files(ui, &app.drag, &mut actions, &resp, p);
                ctx_menu(&resp, &mut actions, p, false);
            }
        }
        ui.add_space(6.0);
        // ---------------- bookmarks
        let hdr_top = ui.cursor().top();
        let open = section(ui, app, "bookmarks", "Bookmarks", Glyph::Bookmark);
        let hdr = Rect::from_min_size(pos2(ui.min_rect().left(), hdr_top), vec2(width, 26.0));
        // Dropping folders on the header bookmarks them.
        if let Some(Drag::Files { paths, .. }) = &app.drag {
            if pointer.map(|p| hdr.contains(p)).unwrap_or(false) {
                ui.painter().rect_stroke(hdr.shrink2(vec2(6.0, 1.0)), CornerRadius::same(6), Stroke::new(1.5, pal.accent), egui::StrokeKind::Inside);
                if released {
                    for p in paths.iter().filter(|p| p.is_dir()) {
                        actions.push(Action::BookmarkAdd(p.clone()));
                    }
                }
            }
        }
        if open {
            let bms = app.ud.bookmarks.clone();
            let mut drop_line: Option<(usize, f32)> = None;
            for (i, b) in bms.iter().enumerate() {
                let name = name_of(b);
                if !matches(&name) {
                    continue;
                }
                // Status comes from the background refresh (unknown yet = assume fine).
                let (exists, is_link) = marks.get(b).copied().unwrap_or((true, false));
                let resp = item(ui, &name, None, b, !exists, is_link);
                nav(&mut actions, &resp, b);
                drop_files(ui, &app.drag, &mut actions, &resp, b);
                ctx_menu(&resp, &mut actions, b, true);
                if resp.drag_started() && app.drag.is_none() {
                    app.drag = Some(Drag::Bookmark { idx: i });
                }
                if let (Some(Drag::Bookmark { idx }), Some(p)) = (&app.drag, pointer) {
                    if resp.rect.contains(p) && *idx != i {
                        let before = p.y < resp.rect.center().y;
                        drop_line = Some((if before { i } else { i + 1 }, if before { resp.rect.top() } else { resp.rect.bottom() }));
                    }
                }
            }
            if let Some((to, y)) = drop_line {
                ui.painter().line_segment([pos2(12.0, y), pos2(width - 8.0, y)], Stroke::new(2.0, pal.accent));
                if released {
                    if let Some(Drag::Bookmark { idx }) = app.drag {
                        let to = if to > idx { to - 1 } else { to };
                        actions.push(Action::BookmarkMove { from: idx, to });
                    }
                }
            }
        }
        ui.add_space(6.0);
        // ---------------- smart folders, once there are any
        if !app.ud.smart.is_empty() && section(ui, app, "smart", "Smart Folders", Glyph::Search) {
            let list: Vec<(u64, String)> = app.ud.smart.iter().map(|s| (s.id, s.name.clone())).collect();
            for (id, name) in list {
                if !matches(&name) {
                    continue;
                }
                let key = crate::ctxmenu::smart_key(id);
                let (r, resp) = ui.allocate_exact_size(vec2(width, row_h), Sense::click());
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &name));
                let r2 = r.shrink2(vec2(6.0, 1.0));
                if ctx_target.as_deref() == Some(key.as_path()) {
                    ui.painter().rect_filled(r2, CornerRadius::same(5), pal.selected);
                } else if resp.hovered() {
                    ui.painter().rect_filled(r2, CornerRadius::same(5), pal.row_hover);
                }
                let ic = Rect::from_center_size(pos2(r.left() + 36.0, r.center().y), Vec2::splat(15.0));
                crate::theme::paint_symbol(ui.painter(), ic, "file-search", pal.text_dim);
                let g = crate::listing::elided(ui, &name, font.clone(), r.width() - 62.0, pal.text);
                ui.painter().galley(pos2(r.left() + 52.0, r.center().y - g.size().y / 2.0), g, pal.text);
                // What it looks for, put in words only while the pointer is on it.
                let summary = resp.hovered().then(|| app.ud.smart.iter().find(|s| s.id == id).map(|s| s.summary())).flatten();
                let resp = match summary {
                    Some(sum) => resp.on_hover_text(format!("{sum}\nCtrl+click: open in a new tab")),
                    None => resp,
                };
                if resp.clicked() {
                    actions.push(Action::OpenSmart { panel: active, id, new_tab: mods.ctrl, focus: true });
                }
                if resp.middle_clicked() {
                    actions.push(Action::OpenSmart { panel: active, id, new_tab: true, focus: false });
                }
                if resp.secondary_clicked() {
                    let pos = resp.interact_pointer_pos().unwrap_or(r.left_bottom());
                    actions.push(Action::Open(crate::ctxmenu::build_smart(active, id, pos)));
                }
            }
            ui.add_space(6.0);
        } else if !app.ud.smart.is_empty() {
            ui.add_space(6.0);
        }
        // ---------------- tags, once some file has one
        if !app.ud.tags.is_empty() && section(ui, app, "tags", "Tags", Glyph::Tag) {
            for tag in crate::tags::ordered(&app.ud.tags) {
                if !matches(&tag) {
                    continue;
                }
                let id = crate::smart::tag_id(&tag);
                let key = crate::ctxmenu::smart_key(id);
                let (r, resp) = ui.allocate_exact_size(vec2(width, row_h), Sense::click());
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &tag));
                let r2 = r.shrink2(vec2(6.0, 1.0));
                if ctx_target.as_deref() == Some(key.as_path()) {
                    ui.painter().rect_filled(r2, CornerRadius::same(5), pal.selected);
                } else if resp.hovered() {
                    ui.painter().rect_filled(r2, CornerRadius::same(5), pal.row_hover);
                }
                let ic = Rect::from_center_size(pos2(r.left() + 36.0, r.center().y), Vec2::splat(15.0));
                match crate::tags::color_of(&tag) {
                    Some(c) => {
                        ui.painter().circle_filled(ic.center(), 5.0, c);
                    }
                    None => paint_glyph(ui.painter(), ic.shrink(1.0), Glyph::Tag, pal.text_dim),
                }
                let g = crate::listing::elided(ui, &tag, font.clone(), r.width() - 62.0, pal.text);
                ui.painter().galley(pos2(r.left() + 52.0, r.center().y - g.size().y / 2.0), g, pal.text);
                let resp = resp.on_hover_text(format!("Everything tagged “{tag}” in your home folder\nCtrl+click: open in a new tab"));
                if resp.clicked() {
                    actions.push(Action::OpenSmart { panel: active, id, new_tab: mods.ctrl, focus: true });
                }
                if resp.middle_clicked() {
                    actions.push(Action::OpenSmart { panel: active, id, new_tab: true, focus: false });
                }
                if resp.secondary_clicked() {
                    let pos = resp.interact_pointer_pos().unwrap_or(r.left_bottom());
                    actions.push(Action::Open(crate::ctxmenu::build_tag(active, &tag, pos)));
                }
            }
            ui.add_space(6.0);
        } else if !app.ud.tags.is_empty() {
            ui.add_space(6.0);
        }
        // ---------------- storage
        if section(ui, app, "storage", "Storage", Glyph::Drive) {
            let mounts = CACHE.with(|c| c.borrow().mounts.clone());
            // Mounted volumes, then removable media that isn't mounted yet (dimmed; a click
            // mounts and opens it).
            let unmounted: Vec<(PathBuf, String, u64, u64)> = drives.iter().filter(|d| d.removable && d.offer_mount()).map(|d| (PathBuf::from(format!("{}{}", tab::DRIVE, d.dev)), d.title(), d.size, d.size)).collect();
            for (p, label, total, free) in mounts.iter().chain(unmounted.iter()) {
                if !matches(label) {
                    continue;
                }
                let drive = match tab::drive_dev(p) {
                    Some(dev) => drives.iter().find(|d| d.dev == dev),
                    None => crate::drives::at_mount(&drives, p),
                };
                let mounted = tab::drive_dev(p).is_none();
                let busy = drive.is_some_and(|d| crate::drives::busy(&d.dev));
                // Removable media ejects; other user mounts (a second internal disk) unmount.
                let eject = drive.filter(|d| mounted && (d.removable || crate::drives::can_unmount(d))).map(|d| if d.removable { crate::drives::Op::Eject } else { crate::drives::Op::Unmount });
                let (r, resp) = ui.allocate_exact_size(vec2(width, row_h + 8.0), Sense::click_and_drag());
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
                let sel = *p == current;
                let r2 = r.shrink2(vec2(6.0, 1.0));
                // Hover covers the eject button too (it sits on top of the row).
                let row_hover = ui.rect_contains_pointer(r) && app.drag.is_none();
                if sel {
                    ui.painter().rect_filled(r2, CornerRadius::same(6), pal.selected);
                } else if row_hover || ctx_target.as_deref() == Some(p.as_path()) {
                    ui.painter().rect_filled(r2, CornerRadius::same(6), pal.row_hover);
                }
                // A drive that is not mounted has a second line: the name moves up a little
                // so the two lines don't touch.
                let ic = Rect::from_center_size(pos2(r.left() + 36.0, r.top() + row_h / 2.0 + if mounted { 1.0 } else { -1.0 }), Vec2::splat(15.0));
                let crypt = drive.map_or(&crate::drives::Crypt::Plain, |d| &d.crypt);
                let icol = if sel { pal.on_sel() } else if mounted { pal.text_dim } else { pal.text_dim.gamma_multiply(0.6) };
                let ring = if sel { pal.selected } else if row_hover || ctx_target.as_deref() == Some(p.as_path()) { pal.row_hover } else { pal.sidebar };
                crate::ui::paint_drive(ui.painter(), ic, drive.is_some_and(|d| d.removable), crypt, icol, ring);
                // Room on the right for the eject button (or the spinner while busy).
                let btn = Rect::from_center_size(pos2(r.right() - 20.0, ic.center().y), Vec2::splat(20.0));
                let reserve = if eject.is_some() || busy { 26.0 } else { 0.0 };
                let tcol = if mounted { pal.text } else { pal.text_dim.gamma_multiply(0.75) };
                let lg = crate::listing::elided(ui, label, font.clone(), r.width() - 60.0 - reserve, tcol);
                ui.painter().galley(pos2(r.left() + 52.0, ic.center().y - lg.size().y / 2.0), lg, tcol);
                if !mounted {
                    let state = if *crypt == crate::drives::Crypt::Locked { "Locked" } else { "Not mounted" };
                    let sub = crate::listing::elided(ui, &format!("{} · {state}", fsx::fmt_size(*total)), FontId::proportional(app.cfg.font_size - 3.0), r.width() - 60.0 - reserve, pal.text_dim);
                    ui.painter().galley(pos2(r.left() + 52.0, r.bottom() - 3.0 - sub.size().y), sub, pal.text_dim);
                } else if *total > 0 && row_hover {
                    // Pointed at, the bar says it in words.
                    let words = format!("{} free of {}", fsx::fmt_size(*free), fsx::fmt_size(*total));
                    let sub = crate::listing::elided(ui, &words, FontId::proportional(app.cfg.font_size - 3.0), r.width() - 60.0 - reserve, pal.text_dim);
                    ui.painter().galley(pos2(r.left() + 52.0, r.bottom() - 4.0 - sub.size().y), sub, pal.text_dim);
                } else if *total > 0 {
                    let frac = (total - free) as f32 / *total as f32;
                    // Every drive's bar the same length, eject button or not; a thin line, the
                    // name above it the thing read first.
                    let bar = Rect::from_min_size(pos2(r.left() + 52.0, r.bottom() - 7.5), vec2((r.width() - 96.0).clamp(0.0, 110.0), 3.0));
                    let track = if sel { pal.on_sel().gamma_multiply(0.25) } else { pal.sidebar.lerp_to_gamma(pal.text, if pal.dark { 0.12 } else { 0.1 }) };
                    ui.painter().rect_filled(bar, CornerRadius::same(2), track);
                    let col = if frac > 0.9 { pal.danger } else if frac > 0.75 { Color32::from_rgb(0xe8, 0xa3, 0x3d) } else { pal.accent };
                    ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * frac, 3.0)), CornerRadius::same(2), col);
                }
                if busy {
                    crate::ui::spinner(ui.painter(), btn.center(), 5.5, pal.accent, ui.input(|i| i.time));
                    ui.ctx().request_repaint();
                } else if let (Some(op), Some(d)) = (eject, drive) {
                    // Shown while the row is hovered (or the button itself is).
                    let b = ui.interact(btn, Id::new(("eject", &d.dev)), Sense::click());
                    if row_hover {
                        if b.hovered() {
                            ui.painter().rect_filled(btn, CornerRadius::same(4), pal.row_hover);
                        }
                        paint_glyph(ui.painter(), btn.shrink(4.0), Glyph::Eject, if b.hovered() { pal.text } else { pal.text_dim });
                        crate::ui::note_control(ui, btn, if op == crate::drives::Op::Eject { "eject button" } else { "unmount button" });
                    }
                    if b.clicked() {
                        app.drive_op(op, &d.dev, None);
                    }
                    let _ = b.on_hover_text(if op == crate::drives::Op::Eject { format!("Eject {}", d.title()) } else { format!("Unmount {}", d.title()) });
                }
                let tip = if mounted { format!("{}\n{} free of {}", p.display(), fsx::fmt_size(*free), fsx::fmt_size(*total)) } else { format!("{}\nClick to {}", drive.map(|d| d.dev.clone()).unwrap_or_default(), if *crypt == crate::drives::Crypt::Locked { "unlock" } else { "mount" }) };
                let resp = resp.on_hover_text(tip);
                nav(&mut actions, &resp, p);
                if mounted {
                    drop_files(ui, &app.drag, &mut actions, &resp, p);
                }
                ctx_menu(&resp, &mut actions, p, false);
            }
        }
        ui.add_space(6.0);
        // ---------------- network: mounted shares and attached phones
        if !net.is_empty() && section(ui, app, "network", "Network", Glyph::Globe) {
            for place in net.iter() {
                let name = if place.device { place.name.clone() } else { crate::network::label(&place.uri) };
                if !matches(&name) {
                    continue;
                }
                let (r, resp) = ui.allocate_exact_size(vec2(width, row_h), Sense::click());
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &name));
                let row_hover = ui.rect_contains_pointer(r) && app.drag.is_none();
                if row_hover || (ctx_target.is_some() && ctx_target == place.path) {
                    ui.painter().rect_filled(r.shrink2(vec2(6.0, 1.0)), CornerRadius::same(5), pal.row_hover);
                }
                let ic = Rect::from_center_size(pos2(r.left() + 36.0, r.center().y), Vec2::splat(15.0));
                let busy = app.connecting(&place.uri);
                let col = if place.mounted { pal.text_dim } else { pal.text_dim.gamma_multiply(0.6) };
                paint_glyph(ui.painter(), ic, if place.device { Glyph::Phone } else { Glyph::Globe }, col);
                let btn = Rect::from_center_size(pos2(r.right() - 20.0, r.center().y), Vec2::splat(20.0));
                let reserve = if place.mounted || busy { 26.0 } else { 0.0 };
                let tcol = if place.mounted { pal.text } else { pal.text_dim.gamma_multiply(0.75) };
                let g = crate::listing::elided(ui, &name, font.clone(), r.width() - 60.0 - reserve, tcol);
                ui.painter().galley(pos2(r.left() + 52.0, r.center().y - g.size().y / 2.0), g, tcol);
                if busy {
                    crate::ui::spinner(ui.painter(), btn.center(), 5.5, pal.accent, ui.input(|i| i.time));
                    ui.ctx().request_repaint();
                } else if place.mounted {
                    let b = ui.interact(btn, Id::new(("disconnect", &place.uri)), Sense::click());
                    if row_hover {
                        if b.hovered() {
                            ui.painter().rect_filled(btn, CornerRadius::same(4), pal.row_hover);
                        }
                        paint_glyph(ui.painter(), btn.shrink(4.0), Glyph::Eject, if b.hovered() { pal.text } else { pal.text_dim });
                        crate::ui::note_control(ui, btn, "disconnect button");
                    }
                    if b.clicked() {
                        app.disconnect(place.uri.clone(), place.path.clone());
                    }
                    let _ = b.on_hover_text(format!("Disconnect {name}"));
                }
                let resp = resp.on_hover_text(match &place.path {
                    Some(p) => format!("{}\n{}", place.uri, p.display()),
                    None => format!("{}\nClick to connect", place.uri),
                });
                match (&place.path, resp.clicked() || resp.middle_clicked()) {
                    (Some(p), _) => nav(&mut actions, &resp, p),
                    (None, true) => app.connect(place.uri.clone(), crate::network::Login { user: String::new(), password: String::new(), anonymous: false }, Some((active, mods.ctrl || resp.middle_clicked()))),
                    _ => {}
                }
                if let Some(p) = &place.path {
                    drop_files(ui, &app.drag, &mut actions, &resp, p);
                    ctx_menu(&resp, &mut actions, p, false);
                }
            }
            ui.add_space(6.0);
        }
        // ---------------- places
        if section(ui, app, "places", "Places", Glyph::Folder) {
            let places = CACHE.with(|c| c.borrow().places.clone());
            for (name, p, g) in places.iter() {
                if !matches(name) {
                    continue;
                }
                let resp = item(ui, name, Some(*g), p, false, false);
                nav(&mut actions, &resp, p);
                drop_files(ui, &app.drag, &mut actions, &resp, p);
                ctx_menu(&resp, &mut actions, p, false);
                // What changed lately, all over the home folder, right under it.
                if *g == Glyph::Home && matches("Timeline") {
                    let key = crate::ctxmenu::smart_key(crate::smart::TIMELINE);
                    let resp = item(ui, "Timeline", Some(Glyph::Clock), &key, false, false);
                    let id = crate::smart::TIMELINE;
                    if resp.clicked() {
                        actions.push(Action::OpenSmart { panel: active, id, new_tab: mods.ctrl, focus: true });
                    }
                    if resp.middle_clicked() {
                        actions.push(Action::OpenSmart { panel: active, id, new_tab: true, focus: false });
                    }
                }
            }
        }
        ui.add_space(12.0);
    });
    app.actions.extend(actions);
}
