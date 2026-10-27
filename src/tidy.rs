//! Tidy Folder: a folder's loose files sorted into folders by kind (Pictures, Documents,
//! Music, Videos, Archives, Installers), or its photos and videos into a folder a month
//! by the date they were taken. Every move is listed before anything happens; the moves
//! then run as one job, which Undo takes back whole, the folders it made included.
//! Folders, hidden files and files of no known kind stay where they are.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::SystemTime;

use egui::{CornerRadius, FontId, Sense, pos2, vec2};

use crate::app::App;
use crate::popups::{self as pp, Popup};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Kind,
    Month,
}

/// The folder a file of this name goes to when sorted by kind.
pub fn kind_folder(name: &str) -> Option<&'static str> {
    let l = name.to_lowercase();
    let ext = l.rsplit_once('.').map(|(s, e)| if s.is_empty() { "" } else { e }).unwrap_or("");
    // Packages first: a .deb is an archive too, and an Arch package ends in .tar.zst.
    if matches!(ext, "deb" | "rpm" | "appimage" | "flatpakref" | "flatpakrepo" | "flatpak" | "snap" | "exe" | "msi" | "dmg" | "pkg") || l.contains(".pkg.tar.") {
        return Some("Installers");
    }
    // A comic is a zip or a rar, but it is read, not unpacked.
    if matches!(ext, "cbz" | "cbr" | "epub" | "mobi" | "azw3" | "fb2") {
        return Some("Documents");
    }
    if crate::archive::is_archive(&l) || matches!(ext, "gz" | "xz" | "bz2" | "zst") {
        return Some("Archives");
    }
    Some(match ext {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "heif" | "avif" | "bmp" | "tif" | "tiff" | "svg" | "raw" | "dng" | "cr2" | "cr3" | "nef" | "arw" | "orf" | "rw2" => "Pictures",
        "pdf" | "doc" | "docx" | "odt" | "rtf" | "txt" | "md" | "xls" | "xlsx" | "ods" | "csv" | "ppt" | "pptx" | "odp" | "epub" | "pages" | "numbers" | "key" | "djvu" => "Documents",
        "mp3" | "flac" | "ogg" | "oga" | "opus" | "m4a" | "aac" | "wav" | "wma" | "aiff" | "aif" | "mid" | "midi" => "Music",
        "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" | "wmv" | "flv" | "mpg" | "mpeg" | "3gp" | "mts" => "Videos",
        _ => return None,
    })
}

/// A photo or video, the files sorted by month.
pub fn dated(name: &str) -> bool {
    matches!(kind_folder(name), Some("Pictures" | "Videos"))
}

/// "2024/2024-06": the year, then the month named with its year, so a month's folder
/// still says which one it is when it turns up on its own.
pub fn month_folder(year: u32, month: u32) -> String {
    format!("{year}/{year}-{month:02}")
}

/// One file to move: where it is, the folder it goes into (relative to the tidied
/// folder), and whether the date came from the file's own record of when it was taken.
#[derive(Clone, Debug, PartialEq)]
pub struct Move {
    pub src: PathBuf,
    pub to: String,
    pub taken: bool,
}

/// The moves for the files directly in `dir` (name, is a folder, modified).
pub fn plan(dir: &Path, files: &[(String, bool, Option<SystemTime>)], mode: Mode, taken: &dyn Fn(&Path) -> Option<[u32; 6]>) -> Vec<Move> {
    // A folder already named for a kind keeps that kind's files.
    let here = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut out = vec![];
    for (name, is_dir, modified) in files {
        if *is_dir || name.starts_with('.') {
            continue;
        }
        let src = dir.join(name);
        match mode {
            Mode::Kind => {
                if let Some(k) = kind_folder(name).filter(|k| !here.eq_ignore_ascii_case(k)) {
                    out.push(Move { src, to: k.to_string(), taken: false });
                }
            }
            Mode::Month if dated(name) => {
                let (ymd, from_photo) = match taken(&src) {
                    Some(t) => ((t[0], t[1]), true),
                    None => match modified {
                        Some(m) => {
                            let tm = crate::timefmt::local(*m);
                            ((tm.tm_year as u32 + 1900, tm.tm_mon as u32 + 1), false)
                        }
                        None => continue,
                    },
                };
                out.push(Move { src, to: month_folder(ymd.0, ymd.1), taken: from_photo });
            }
            Mode::Month => {}
        }
    }
    out
}

/// The folder's files and both plans, read on a worker (the photos' dates are on disk).
pub struct Plans {
    pub kind: Vec<Move>,
    pub month: Vec<Move>,
}

pub fn read_plans(dir: &Path) -> Result<Plans, String> {
    let files: Vec<(String, bool, Option<SystemTime>)> = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| {
            let m = e.metadata().ok();
            (e.file_name().to_string_lossy().into_owned(), m.as_ref().is_none_or(|m| m.is_dir()), m.and_then(|m| m.modified().ok()))
        })
        .collect();
    // In the order the folder lists them (the release build's own sort is slow).
    let mut order: Vec<u32> = (0..files.len() as u32).collect();
    crate::tab::merge_sort(&mut order, &|a: &u32, b: &u32| crate::tab::natural_cmp(&files[*a as usize].0, &files[*b as usize].0));
    let files: Vec<(String, bool, Option<SystemTime>)> = order.iter().map(|&i| files[i as usize].clone()).collect();
    let taken = |p: &Path| crate::exif::read(p).and_then(|x| x.taken);
    Ok(Plans { kind: plan(dir, &files, Mode::Kind, &taken), month: plan(dir, &files, Mode::Month, &taken) })
}

pub struct State {
    pub dir: PathBuf,
    pub mode: Mode,
    pub plans: Option<Result<Plans, String>>,
    rx: Option<Receiver<Result<Plans, String>>>,
}

impl State {
    pub fn moves(&self) -> &[Move] {
        match &self.plans {
            Some(Ok(p)) => if self.mode == Mode::Kind { &p.kind } else { &p.month },
            _ => &[],
        }
    }
}

pub fn open(app: &mut App) {
    let t = app.tab();
    if t.archive.is_some() || crate::tab::is_place(&t.path) || t.path.starts_with(crate::sidebar::trash_dir()) {
        app.toast("Tidy Folder works in a folder of your own");
        return;
    }
    let dir = t.path.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let (d, ctx) = (dir.clone(), app.ctx.clone());
    std::thread::spawn(move || {
        let _ = tx.send(read_plans(&d));
        ctx.request_repaint();
    });
    app.popup = Popup::Tidy(State { dir, mode: Mode::Kind, plans: None, rx: Some(rx) });
}

/// The dialog: by kind or photos by month, every file beside the folder it goes into;
/// Tidy runs the moves.
pub fn popup(app: &mut App, ctx: &egui::Context, st: &mut State) -> bool {
    if let Some(rx) = &st.rx {
        if let Ok(p) = rx.try_recv() {
            st.plans = Some(p);
            st.rx = None;
        }
    }
    let pal = app.pal;
    let k = pp::take_keys(ctx);
    if k.esc {
        return false;
    }
    let fs = app.cfg.font_size;
    let small = fs - 1.5;
    let moves = st.moves().to_vec();
    let ok = !moves.is_empty();
    let mut go = k.enter && ok;
    let mut cancel = false;
    let list_max = pp::fit_list(ctx, 360.0, 260.0);
    let name = st.dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| st.dir.display().to_string());
    pp::centered(ctx, "tidy folder", 600.0, &pal, |ui| {
        pp::dialog_title(ui, &format!("Tidy {name}"), None, &pal);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            for (m, l, what) in [(Mode::Kind, "By kind", "tidy by kind"), (Mode::Month, "Photos by month", "tidy by month")] {
                if crate::devtools::chip(ui, l, st.mode == m, what, l, &pal, false) {
                    st.mode = m;
                }
            }
        });
        ui.add_space(8.0);
        let row_h = (fs * 1.35 + 10.0).round();
        let n = moves.len();
        egui::ScrollArea::vertical().id_salt("tidy list").max_height(list_max).min_scrolled_height((n as f32 * row_h).min(list_max)).auto_shrink([false, true]).show_rows(ui, row_h, n, |ui, range| {
            for i in range {
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
                if i % 2 == 1 {
                    ui.painter().rect_filled(r, CornerRadius::same(4), pal.card());
                }
                let m = &moves[i];
                let file = m.src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let half = (r.width() - 40.0) / 2.0;
                let font = FontId::proportional(fs);
                let g = crate::listing::elided(ui, &file, font.clone(), half - 8.0, pal.text);
                ui.painter().galley(pos2(r.left() + 8.0, r.center().y - g.size().y / 2.0), g, pal.text);
                ui.painter().text(pos2(r.left() + 8.0 + half + 12.0, r.center().y), egui::Align2::CENTER_CENTER, "→", font.clone(), pal.text_dim);
                let x = r.left() + half + 34.0;
                // A month worked out from when the file was last changed is a guess: said so.
                let to = if st.mode == Mode::Month && !m.taken { format!("{}  (file date)", m.to) } else { m.to.clone() };
                let g = crate::listing::elided(ui, &to, font, r.right() - 8.0 - x, pal.text_dim);
                ui.painter().galley(pos2(x, r.center().y - g.size().y / 2.0), g, pal.text_dim);
            }
        });
        ui.add_space(6.0);
        let folders = {
            let mut f: Vec<&str> = moves.iter().map(|m| m.to.as_str()).collect();
            f.sort_unstable();
            f.dedup();
            f.len()
        };
        let (status, col) = match &st.plans {
            None => ("Looking at the files…".to_string(), pal.text_dim),
            Some(Err(e)) => (format!("Can't read this folder: {e}"), pal.danger),
            Some(Ok(_)) if n == 0 && st.mode == Mode::Kind => ("Nothing here to sort by kind".to_string(), pal.text_dim),
            Some(Ok(_)) if n == 0 => ("No photos or videos here".to_string(), pal.text_dim),
            Some(Ok(_)) => (format!("{n} {} into {folders} {}", if n == 1 { "file" } else { "files" }, if folders == 1 { "folder" } else { "folders" }), pal.text_dim),
        };
        ui.label(egui::RichText::new(status).size(small).color(col));
        pp::footer(ui, &[("Enter", "tidy"), ("Esc", "close")], &pal, |ui| {
            let b = pp::primary_button(ui, "Tidy", ok, &pal);
            crate::ui::note_control(ui, b.rect, "tidy apply");
            go |= b.clicked() && ok;
            let c = pp::plain_button(ui, "Cancel", &pal);
            crate::ui::note_control(ui, c.rect, "tidy cancel");
            cancel = c.clicked();
        });
    });
    if cancel {
        return false;
    }
    if go {
        let moves: Vec<(PathBuf, PathBuf)> = moves.iter().map(|m| (m.src.clone(), st.dir.join(&m.to))).collect();
        app.actions.push(crate::app::Action::StartJob(crate::ops::OpKind::Sort { moves, label: format!("Tidying {name}") }));
        return false;
    }
    true
}
