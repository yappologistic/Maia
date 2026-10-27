//! Quick Open: any file in the project the folder is in, by a few letters of its path.
//! The project's files come from git (tracked and new, not ignored) or, outside a
//! repository, from a walk that leaves out hidden folders and build output. Listing and
//! matching both run on threads, so a project of a hundred thousand files never holds up
//! a frame; with nothing typed, the project's files opened last come first.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use egui::{CornerRadius, FontId, Sense, pos2, vec2};

use crate::app::{Action, App};
use crate::popups::{self as pp, Popup};

/// Files listed at most: past this a "project" is a home folder or a disk.
const MAX_FILES: usize = 200_000;
/// Rows kept for a query.
const SHOWN: usize = 200;

pub struct State {
    pub root: PathBuf,
    pub query: String,
    focus: bool,
    pub sel: usize,
    files: Arc<Vec<String>>,
    list_rx: Option<mpsc::Receiver<Vec<String>>>,
    pub listed: bool,
    /// Rows for `hits_for`: indexes into `files`.
    pub hits: Vec<u32>,
    hits_for: Option<String>,
    hit_rx: mpsc::Receiver<(String, Vec<u32>)>,
    hit_tx: mpsc::Sender<(String, Vec<u32>)>,
    /// Bumped for each query; a match still running for an older one gives up.
    generation: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    /// Files of this project opened lately, newest first (relative paths).
    recent: Vec<String>,
}

impl Drop for State {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        self.generation.fetch_add(1, Relaxed);
    }
}

/// The project's files, as paths from its top: git's when it is a repository.
pub fn list(root: &Path, stop: &AtomicBool) -> Vec<String> {
    if crate::project::git_root(root).as_deref() == Some(root) && crate::media::have("git") {
        let mut c = crate::media::tool("git");
        c.arg("--no-optional-locks").args(crate::gitstatus::guard(root)).arg("-C").arg(root).args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"]);
        if let Some(out) = crate::media::output_capped(c, Duration::from_secs(30), 64 << 20, &|| stop.load(Relaxed)) {
            // Tracked files, then untracked ones, each in order: one order for both.
            let mut v = sorted(out.split(|b| *b == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned()).take(MAX_FILES).collect());
            // A file in conflict is listed once for each of its stages.
            v.dedup();
            return v;
        }
    }
    walk(root, stop)
}

/// Every file under `root` but in hidden folders, build output and other file systems.
fn walk(root: &Path, stop: &AtomicBool) -> Vec<String> {
    use std::os::unix::fs::MetadataExt;
    let mut out = vec![];
    let Ok(dev) = std::fs::metadata(root).map(|m| m.dev()) else { return out };
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        if stop.load(Relaxed) || out.len() >= MAX_FILES {
            break;
        }
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let kids: Vec<(String, std::fs::FileType, PathBuf)> = rd.flatten().filter_map(|e| Some((e.file_name().to_string_lossy().into_owned(), e.file_type().ok()?, e.path()))).collect();
        let has = |n: &str| kids.iter().any(|k| k.0 == n);
        for (name, ft, path) in &kids {
            if name.starts_with('.') {
                continue;
            }
            if ft.is_dir() {
                if crate::buildclean::kind_by_name(name, &has).is_none() && std::fs::symlink_metadata(path).is_ok_and(|m| m.dev() == dev) {
                    stack.push(path.clone());
                }
            } else {
                out.push(crate::project::relative(path, root));
            }
        }
    }
    sorted(out)
}

/// In order, so a file is found by a binary search; sorted as indexes, which the sort
/// moves cheaply.
fn sorted(mut v: Vec<String>) -> Vec<String> {
    let mut order: Vec<u32> = (0..v.len() as u32).collect();
    crate::tab::merge_sort(&mut order, &|a: &u32, b: &u32| v[*a as usize].cmp(&v[*b as usize]));
    order.into_iter().map(|i| std::mem::take(&mut v[i as usize])).collect()
}

/// The best matches for `query` among `files`, best first: letters in the file's own
/// name count for more than letters in its folders. None when a newer query took over.
pub fn rank(files: &[String], query: &str, generation: &AtomicU64, g: u64) -> Option<Vec<u32>> {
    let q = crate::fuzzy::Query::new(query);
    let mut scored: Vec<(i32, u32)> = vec![];
    for (i, f) in files.iter().enumerate() {
        if i % 4096 == 0 && generation.load(Relaxed) != g {
            return None;
        }
        let Some(whole) = crate::fuzzy::score_ci(&q, f) else { continue };
        let name = f.rsplit('/').next().unwrap_or(f);
        let own = crate::fuzzy::score_ci(&q, name).map_or(0, |s| 1000 + s);
        // Shallower paths first among equals.
        let depth = f.matches('/').count() as i32;
        scored.push((whole.max(own) - depth, i as u32));
    }
    if scored.len() > SHOWN {
        scored.select_nth_unstable_by(SHOWN, |a, b| b.0.cmp(&a.0));
        scored.truncate(SHOWN);
    }
    crate::tab::merge_sort(&mut scored, &|a: &(i32, u32), b: &(i32, u32)| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    Some(scored.into_iter().map(|s| s.1).collect())
}

pub fn open(app: &mut App, ctx: &egui::Context) -> Option<Popup> {
    let here = app.tab().path.clone();
    if crate::tab::is_place(&here) || app.tab().archive.is_some() {
        return None;
    }
    let root = crate::project::root(&here);
    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let (r, st, c) = (root.clone(), stop.clone(), ctx.clone());
    let _ = std::thread::Builder::new().name("quick open".into()).spawn(move || {
        let v = list(&r, &st);
        let _ = tx.send(v);
        c.request_repaint();
    });
    let recent = app.ud.recents.iter().filter(|p| p.starts_with(&root) && p != &&root).map(|p| crate::project::relative(p, &root)).take(SHOWN).collect();
    let (hit_tx, hit_rx) = mpsc::channel();
    Some(Popup::QuickOpen(Box::new(State { root, query: String::new(), focus: true, sel: 0, files: Arc::new(vec![]), list_rx: Some(rx), listed: false, hits: vec![], hits_for: None, hit_rx, hit_tx, generation: Arc::new(AtomicU64::new(0)), stop, recent })))
}

impl State {
    /// The relative paths on show, in order.
    pub fn rows(&self) -> Vec<&str> {
        if self.query.trim().is_empty() {
            // Recent files that still exist in the list, then the list from its top.
            let mut v: Vec<&str> = self.recent.iter().filter(|r| self.files.binary_search(r).is_ok() || !self.listed).map(String::as_str).collect();
            for f in self.files.iter() {
                if v.len() >= SHOWN {
                    break;
                }
                if !v.contains(&f.as_str()) {
                    v.push(f);
                }
            }
            return v;
        }
        self.hits.iter().filter_map(|&i| self.files.get(i as usize)).map(String::as_str).collect()
    }

    /// Start matching the query, unless it is matched already or being matched.
    fn ask(&mut self, ctx: &egui::Context) {
        let q = self.query.trim().to_string();
        if q.is_empty() || !self.listed || self.hits_for.as_deref() == Some(&q) {
            return;
        }
        self.hits_for = Some(q.clone());
        let g = self.generation.fetch_add(1, Relaxed) + 1;
        let (files, generation, tx, ctx) = (self.files.clone(), self.generation.clone(), self.hit_tx.clone(), ctx.clone());
        let _ = std::thread::Builder::new().name("quick open match".into()).spawn(move || {
            if let Some(hits) = rank(&files, &q, &generation, g) {
                let _ = tx.send((q, hits));
                ctx.request_repaint();
            }
        });
    }
}

/// Enter shows the file in its folder, Ctrl+Enter in a new tab, Shift+Enter opens it in
/// the editor.
pub fn popup(app: &mut App, ctx: &egui::Context, st: &mut State) -> bool {
    let pal = app.pal;
    let shift_enter = ctx.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::Enter));
    let k = pp::take_keys(ctx);
    if k.esc {
        return false;
    }
    if let Some(rx) = &st.list_rx {
        if let Ok(v) = rx.try_recv() {
            st.files = Arc::new(v);
            st.listed = true;
            st.list_rx = None;
        }
    }
    while let Ok((q, hits)) = st.hit_rx.try_recv() {
        if Some(&q) == st.hits_for.as_ref() {
            st.hits = hits;
        }
    }
    st.ask(ctx);
    let rows: Vec<String> = st.rows().into_iter().map(str::to_string).collect();
    pp::step(&mut st.sel, rows.len(), &k, |_| false);
    let fs = app.cfg.font_size;
    let small = fs - 1.5;
    let mods = ctx.input(|i| i.modifiers);
    // (relative path, new tab, editor)
    let mut go: Option<(String, bool, bool)> = rows.get(st.sel).filter(|_| k.enter || k.ctrl_enter || shift_enter).map(|r| (r.clone(), k.ctrl_enter, shift_enter));
    let list_max = pp::fit_list(ctx, 460.0, 110.0);
    let mut reset_sel = false;
    let project = st.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| st.root.display().to_string());
    let q = crate::fuzzy::Query::new(&st.query);
    pp::centered(ctx, "quick open", 680.0, &pal, |ui| {
        let r = pp::search_field(ui, &mut st.query, &format!("Open a file in {project}…"), &mut st.focus, &pal, crate::theme::Glyph::Search);
        crate::ui::note_control(ui, r.rect, "quick open field");
        reset_sel = r.changed();
        let row_h = (fs * 1.35 + 12.0).round();
        let n = rows.len();
        if n > 0 {
            let mut area = egui::ScrollArea::vertical().id_salt("quick open list").max_height(list_max).min_scrolled_height((n as f32 * row_h).min(list_max)).auto_shrink([false, true]);
            if k.up || k.down || k.page_up || k.page_down {
                let most = (n as f32 * row_h - list_max).max(0.0);
                area = area.vertical_scroll_offset((st.sel as f32 * row_h - list_max / 2.0 + row_h / 2.0).clamp(0.0, most));
            }
            area.show_rows(ui, row_h, n, |ui, range| {
                for j in range {
                    let rel = &rows[j];
                    let on = j == st.sel;
                    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                    if on {
                        ui.painter().rect_filled(r, CornerRadius::same(6), pal.selected);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(r, CornerRadius::same(6), pal.hover);
                    }
                    let (tc, dc, acc) = if on { (pal.on_sel(), pal.on_sel_dim(), pal.on_sel()) } else { (pal.text, pal.text_dim, pal.accent) };
                    let (dir, name) = match rel.rsplit_once('/') {
                        Some((d, n)) => (d, n),
                        None => ("", rel.as_str()),
                    };
                    let ic = egui::Rect::from_center_size(pos2(r.left() + 20.0, r.center().y), egui::Vec2::splat(16.0));
                    crate::icons::file(ui.painter(), ic, name);
                    let left = ic.right() + 10.0;
                    let room = r.right() - 12.0 - left;
                    let mut job = pp::highlight_job(name, &q, FontId::proportional(fs), tc, acc);
                    job.wrap = egui::text::TextWrapping { max_width: room * 0.6, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
                    let g = ui.fonts_mut(|f| f.layout_job(job));
                    let nw = g.size().x;
                    ui.painter().galley(pos2(left, r.center().y - g.size().y / 2.0), g, tc);
                    if !dir.is_empty() && room - nw - 12.0 > 30.0 {
                        let dg = crate::listing::elided_tail(ui, dir, FontId::proportional(small), room - nw - 12.0, dc);
                        ui.painter().galley(pos2(left + nw + 12.0, r.center().y - dg.size().y / 2.0), dg, dc);
                    }
                    crate::ui::note_control(ui, r.shrink(1.0), "quick open row");
                    if resp.clicked() {
                        go = Some((rel.clone(), mods.command, mods.shift));
                    }
                }
            });
        } else {
            let msg = if !st.listed {
                "Listing the project's files…"
            } else if st.files.is_empty() {
                "No files in this project"
            } else {
                "No file by that name"
            };
            ui.add_space(4.0);
            ui.label(egui::RichText::new(msg).color(pal.text_dim));
            ui.add_space(4.0);
        }
        if st.listed && !st.files.is_empty() {
            let count = st.files.len();
            let more = if count >= MAX_FILES { "+" } else { "" };
            ui.label(egui::RichText::new(format!("{count}{more} files in {}", pp::home_short(&st.root))).size(small).color(pal.text_dim));
        }
        pp::hints(ui, &[("Enter", "show"), ("Ctrl+Enter", "new tab"), ("Shift+Enter", "edit"), ("Esc", "close")], &pal);
    });
    if reset_sel {
        st.sel = 0;
    }
    if let Some((rel, new_tab, editor)) = go {
        let path = st.root.join(&rel);
        let panel = app.active;
        if editor {
            app.open_in_editor(&path, None);
        } else if new_tab {
            let parent = path.parent().map(Path::to_path_buf).unwrap_or_else(|| st.root.clone());
            app.actions.push(Action::NewTab { panel, path: parent, select: Some(path), focus: true });
        } else {
            app.actions.push(Action::Navigate { panel, path });
        }
        return false;
    }
    true
}
