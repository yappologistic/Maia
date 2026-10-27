//! Inspector: previews the item under the cursor (images, text, folders, volumes).

use std::path::{Path, PathBuf};

use egui::{self, Align2, Color32, CornerRadius, FontId, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder, Vec2, pos2, vec2};

use crate::config::InspectorLayout;
use crate::fsx::{self, Entry};
use crate::listing::Env;
use crate::panel::Panel;
use crate::tab;
use crate::theme::{self, Glyph, paint_glyph};
use crate::thumbs;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    Fit,
    Actual,
    Fill,
}

enum Content {
    None,
    /// Folder listing or text being read in the background.
    Loading,
    /// `spans`: syntax highlighting per line (code and markup only). `prose`: plain text
    /// or Markdown, which wrap by default where code does not. `rich`: what Markdown,
    /// data, tables and `.env` files show instead of their text. `end`: the byte after
    /// what was read; `from_end`: the file's last megabyte was read (a long log).
    Text { lines: Vec<String>, truncated: bool, spans: Option<Vec<Vec<(usize, usize, crate::syntax::Kind)>>>, prose: bool, rich: Option<Box<Rich>>, end: u64, from_end: bool },
    Sqlite(Box<crate::sqlite::View>),
    /// Why there is nothing to show, under the file's icon and name.
    Note(String),
    /// Text too big to read whole, or anything that isn't text (as hex): read a window
    /// at a time.
    Big(Box<crate::bigfile::Pager>),
    Image,
    Folder(Vec<Entry>),
    Volume,
    Pdf(Box<PdfView>),
    /// An office document LibreOffice is turning into a PDF to show.
    Office(Box<crate::office::Conversion>),
    Media(Box<MediaView>),
    /// A font's names and lines of sample text drawn with it.
    Font(Box<FontView>),
    Other,
}

/// What a rendered preview shows instead of its text (the text stays, for Source).
pub enum Rich {
    Markdown(Box<crate::markdown::View>),
    Data(Box<crate::datatree::Tree>),
    Table(Box<crate::table::TableView>),
    /// A data file that would not parse: why, above its source.
    Broken(String),
    Env(Box<crate::envfile::View>),
    /// An EPUB's text as Markdown, which has no source worth switching to.
    Book(Box<crate::markdown::View>),
}

/// The previews with a Rendered / Source choice, remembered apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RichKind {
    Markdown,
    Data,
    Table,
}

impl Rich {
    fn kind(&self) -> Option<RichKind> {
        match self {
            Rich::Markdown(_) => Some(RichKind::Markdown),
            Rich::Data(_) | Rich::Broken(_) => Some(RichKind::Data),
            Rich::Table(_) => Some(RichKind::Table),
            Rich::Env(_) | Rich::Book(_) => None,
        }
    }
}

/// A blame of the file shown, read on a worker, for the file as it was when asked.
struct BlameJob {
    key: (PathBuf, Option<std::time::SystemTime>),
    rx: Option<std::sync::mpsc::Receiver<Result<crate::blame::Blame, String>>>,
    got: Option<Result<crate::blame::Blame, String>>,
}

/// Lines a followed file keeps; past it the oldest go.
pub const FOLLOW_LINES: usize = 100_000;

pub struct PdfView {
    path: PathBuf,
    /// A comic's pages: the pictures inside it, in order (a PDF has none).
    comic: Option<std::sync::Arc<Vec<PathBuf>>>,
    comic_rx: Option<std::sync::mpsc::Receiver<Result<Vec<PathBuf>, String>>>,
    pages: Option<usize>,
    pages_rx: Option<std::sync::mpsc::Receiver<Option<usize>>>,
    /// 1-based page shown.
    pub page: usize,
    tex: Option<egui::TextureHandle>,
    /// (page, pixel size) of `tex`.
    shown: Option<(usize, u32)>,
    job: Option<std::sync::mpsc::Receiver<(usize, u32, Option<egui::ColorImage>)>>,
    failed: bool,
}

pub struct FontView {
    path: PathBuf,
    rx: Option<std::sync::mpsc::Receiver<Result<crate::fontview::Preview, String>>>,
    got: Option<Result<(crate::fontview::Info, bool), String>>,
    /// The sample lines' pictures, and the scale they were drawn for.
    tex: Vec<(f32, egui::TextureHandle)>,
    ppp: f32,
}

pub struct MediaView {
    path: PathBuf,
    pub probe: Option<crate::media::Probe>,
    probe_rx: Option<std::sync::mpsc::Receiver<Option<crate::media::Probe>>>,
    pub player: Option<crate::media::Player>,
    tex: Option<egui::TextureHandle>,
    seq: u64,
    failed: bool,
}

pub struct Inspector {
    pub layout: Option<InspectorLayout>,
    pub line_numbers: bool,
    /// Wrap long lines: remembered apart for highlighted code and for everything else.
    pub wrap_code: bool,
    pub wrap_prose: bool,
    /// Where each wrapped row starts, for the text shown at the width it was made for.
    wrapped: Option<Wrapped>,
    pub minimap: bool,
    pub pixel_grid: bool,
    pub folder_path: Option<PathBuf>,
    current: Option<(PathBuf, Option<std::time::SystemTime>)>,
    content: Content,
    fit: Fit,
    /// Image scale relative to "fit" (1.0 = fit) and pan in screen points.
    zoom: f32,
    pan: Vec2,
    text_size: f32,
    show_meta: bool,
    /// Background load of the current item's content (folder listing / text).
    pending: Option<std::sync::mpsc::Receiver<Content>>,
    /// Metadata card shows every known field, not just the basics.
    extra_meta: bool,
    /// A file inside an archive being extracted to preview (where it landed, and the
    /// entry it stands for).
    extracting: Option<(std::sync::mpsc::Receiver<Result<PathBuf, String>>, Entry)>,
    /// The archive it is in wants a password the preview won't ask for.
    locked: bool,
    /// The commit history, shown instead of the preview while open.
    pub git: Option<Box<crate::gitlog::History>>,
    /// A line (from 1) to show marked in this file's text, and whether to scroll to it.
    pub goto: Option<(PathBuf, u32, bool)>,
    /// Who last changed each line (the toolbar's Blame), for files in a work tree.
    pub blame_on: bool,
    blame: Option<BlameJob>,
    /// Keep a growing file's preview at its end (the toolbar's Follow).
    pub follow: bool,
    tail: Option<crate::tail::Tail>,
    /// A long log, shown from its end, paged whole while it is found in.
    paged_log: bool,
    /// Bytes after the last line break that arrived, waiting for the rest of their line.
    carry: Vec<u8>,
    /// The file grew while it was shown: Follow is offered for it.
    grew: bool,
    /// Size of the file when its preview was read.
    shown_size: u64,
    /// The number of the first line kept (0: unknown, after reading a file's end).
    first_line: usize,
    /// Scroll to the end on the next frame.
    to_end: bool,
    /// A log preview's filter, and the lines it lets through: (query, lines looked at,
    /// lines dropped from the front when made, matches).
    pub filter: String,
    filtered: Option<(String, u64, usize, Vec<String>)>,
    /// Counts every new text and every cut to the front of one (the filter starts over).
    era: u64,
    /// A `.env` preview shows every value.
    pub env_all: bool,
    /// Two files compared, shown instead of the preview while one of them is selected.
    pub compare: Option<Box<crate::filediff::Compare>>,
    /// A crop being drawn on the picture shown.
    pub crop: Option<crate::picfix::Crop>,
    /// Earlier versions of a file from the disk's snapshots, listed above the preview.
    pub versions: Option<Box<crate::versions::Page>>,
    /// A photo's EXIF for its details (read on a worker once the card is open).
    exif: Option<(PathBuf, Option<std::sync::mpsc::Receiver<Option<crate::exif::Exif>>>, Option<crate::exif::Exif>)>,
    /// Find in the text shown, while its bar is open.
    pub find: Option<crate::findbar::Finder>,
    /// The preview was clicked last: Ctrl+F finds in it rather than filtering the list.
    pub keys: bool,
    /// A text file being changed in the preview (it stays on it while the cursor moves).
    pub edit: Option<Box<crate::quickedit::Edit>>,
    /// The picture shown, playing, when it moves.
    anim: Option<crate::anim::Player>,
    /// Text selected in a text preview (lines as shown, filtered or not), and what it
    /// was made on: (text, the filter it went through).
    tsel: Option<crate::textsel::Sel>,
    tsel_for: (u64, String),
    /// The view drawn last, for Ctrl+C and Ctrl+A.
    showing: Showing,
    /// A copy out of a big file, being read on a worker.
    copying: Option<std::sync::mpsc::Receiver<(String, bool)>>,
}

/// Which view of the content was drawn last.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Showing {
    Other,
    /// Lines as text; whether through the log's filter.
    Text(bool),
    Env,
    Markdown,
    Tree,
    Table,
    Sqlite,
    Big,
}

/// Test-only views of the preview state (simulations check behaviour, not pixels).
#[cfg(test)]
impl Inspector {
    /// (page, pages, rendered (page, px)) for a PDF preview.
    pub fn pdf_state(&self) -> Option<(usize, Option<usize>, Option<(usize, u32)>)> {
        match &self.content {
            Content::Pdf(v) => Some((v.page, v.pages, v.shown)),
            _ => None,
        }
    }
    /// A comic's pages, once listed.
    pub fn comic_pages(&self) -> Option<usize> {
        match &self.content {
            Content::Pdf(v) => v.comic.as_ref().map(|c| c.len()),
            _ => None,
        }
    }
    /// A font preview: (family, style, sample lines drawn, installed), once read; the
    /// reason when it can't be shown.
    pub fn font_state(&self) -> Option<Result<(String, String, usize, bool), String>> {
        match &self.content {
            Content::Font(v) => v.got.as_ref().map(|g| g.clone().map(|(i, inst)| (i.family, i.style, v.tex.len(), inst))),
            _ => None,
        }
    }
    /// The EXIF read for the details card.
    pub fn exif_state(&self) -> Option<&crate::exif::Exif> {
        self.exif.as_ref().and_then(|e| e.2.as_ref())
    }
    /// An office document waiting for LibreOffice.
    pub fn office_pending(&self) -> bool {
        matches!(self.content, Content::Office(_))
    }
    /// The file a PDF preview shows (an office document's converted copy).
    pub fn pdf_path(&self) -> Option<&Path> {
        match &self.content {
            Content::Pdf(v) => Some(&v.path),
            _ => None,
        }
    }
    /// (playing, position, duration, decoder pids, frames received) for a media preview.
    pub fn media_state(&self) -> Option<(bool, f64, f64, Vec<u32>, u64)> {
        match &self.content {
            Content::Media(v) => v.player.as_ref().map(|p| (p.playing(), p.position(), p.duration, p.pids(), v.seq)),
            _ => None,
        }
    }
    /// (frame shown, frames held, bytes they hold, decoded again each time round,
    /// playing) for an animated picture.
    pub fn anim_state(&self) -> Option<(usize, usize, usize, bool, bool)> {
        self.anim.as_ref().filter(|a| a.moving()).map(|a| {
            let (at, n, bytes, streaming, _) = a.state();
            (at, n, bytes, streaming, a.playing)
        })
    }
    pub fn media_probe(&self) -> Option<crate::media::Probe> {
        match &self.content {
            Content::Media(v) => v.probe.clone(),
            _ => None,
        }
    }
    /// Rows the text preview is drawn in (wrapped rows when wrapping).
    pub fn wrapped_rows(&self) -> Option<usize> {
        self.wrapped.as_ref().filter(|_| self.wraps()).map(|w| w.rows.len())
    }
    /// What a rich preview holds: (kind, rows or pieces drawn). Markdown: laid-out text
    /// pieces; data: visible tree rows; table: rows; env: lines.
    pub fn rich_state(&mut self) -> Option<(&'static str, usize)> {
        let Content::Text { rich: Some(r), .. } = &mut self.content else { return None };
        Some(match &mut **r {
            Rich::Markdown(v) => ("markdown", v.counts().map_or(0, |c| c.0)),
            Rich::Data(t) => ("tree", t.rows().len()),
            Rich::Table(t) => ("table", t.t.rows.len()),
            Rich::Broken(_) => ("broken", 0),
            Rich::Env(v) => ("env", v.kinds.len()),
            Rich::Book(v) => ("book", v.counts().map_or(0, |c| c.0)),
        })
    }
    pub fn rich_name(&self) -> Option<&'static str> {
        let Content::Text { rich: Some(r), .. } = &self.content else { return None };
        Some(match &**r {
            Rich::Markdown(_) => "markdown",
            Rich::Data(_) => "tree",
            Rich::Table(_) => "table",
            Rich::Broken(_) => "broken",
            Rich::Env(_) => "env",
            Rich::Book(_) => "book",
        })
    }
    pub fn markdown(&self) -> Option<&crate::markdown::View> {
        match &self.content {
            Content::Text { rich: Some(r), .. } => match &**r {
                Rich::Markdown(v) | Rich::Book(v) => Some(v),
                _ => None,
            },
            _ => None,
        }
    }
    pub fn tree(&mut self) -> Option<&mut crate::datatree::Tree> {
        match &mut self.content {
            Content::Text { rich: Some(r), .. } => match &mut **r {
                Rich::Data(t) => Some(t),
                _ => None,
            },
            _ => None,
        }
    }
    pub fn table(&self) -> Option<&crate::table::Table> {
        match &self.content {
            Content::Text { rich: Some(r), .. } => match &**r {
                Rich::Table(t) => Some(&t.t),
                _ => None,
            },
            Content::Sqlite(v) => v.rows.as_ref().and_then(|r| r.1.as_ref().ok()).map(|t| &t.t),
            _ => None,
        }
    }
    pub fn sqlite_state(&self) -> Option<(Option<usize>, usize, Option<(usize, usize)>)> {
        match &self.content {
            Content::Sqlite(v) => Some(v.state()),
            _ => None,
        }
    }
    pub fn note(&self) -> Option<&str> {
        match &self.content {
            Content::Note(n) => Some(n),
            _ => None,
        }
    }
    /// Lines of a text preview, and whether a worker is following the file.
    pub fn text_lines(&self) -> Option<(&[String], bool)> {
        match &self.content {
            Content::Text { lines, .. } => Some((lines, self.tail.is_some())),
            _ => None,
        }
    }
    /// The blame read for the file shown: (lines, commits).
    pub fn blame_state(&self) -> Option<(usize, usize)> {
        self.blame.as_ref().and_then(|b| b.got.as_ref()).and_then(|g| g.as_ref().ok()).map(|b| (b.lines.len(), b.commits.len()))
    }
    pub fn filtered_lines(&self) -> Option<usize> {
        self.filtered.as_ref().filter(|f| !f.0.is_empty()).map(|f| f.3.len())
    }
    /// A paged file: (rows, all counted, as hex), and its search: (matches, done, current).
    pub fn big_state(&self) -> Option<((u64, bool, bool), Option<(usize, bool, usize)>)> {
        match &self.content {
            Content::Big(v) => Some(((v.rows().0, v.rows().1, v.hex), v.find.as_ref().map(|f| (f.hits.lock().unwrap().len(), f.done(), f.cur)))),
            _ => None,
        }
    }
    /// Highlighted spans in a text preview (None: plain text or not text).
    pub fn text_spans(&self) -> Option<usize> {
        match &self.content {
            Content::Text { spans, .. } => spans.as_ref().map(|s| s.iter().map(|l| l.len()).sum()),
            _ => None,
        }
    }
}

impl Inspector {
    /// Page Up / Page Down turn pages while the pointer is over this preview.
    pub fn wants_page_keys(&self) -> bool {
        matches!(self.content, Content::Pdf(_))
    }

    /// The Rendered / Source choice that applies to what is shown.
    pub fn rich_kind(&self) -> Option<RichKind> {
        match &self.content {
            Content::Text { rich: Some(r), .. } => r.kind(),
            _ => None,
        }
    }

    /// Keys a clicked data tree or database list takes while it has the keyboard; a
    /// crop takes Enter and Escape.
    pub fn takes(&self, k: egui::Key) -> bool {
        if self.crop.is_some() && matches!(k, egui::Key::Enter | egui::Key::Escape) {
            return true;
        }
        if let Some(v) = &self.versions {
            return v.takes(k);
        }
        match &self.content {
            Content::Text { rich: Some(r), .. } => matches!(&**r, Rich::Data(t) if t.takes(k)),
            Content::Sqlite(v) => v.takes(k),
            _ => false,
        }
    }

    pub fn key(&mut self, k: egui::Key) {
        if let Some(v) = self.versions.as_mut() {
            v.key(k);
            return;
        }
        if let Some(c) = self.crop.as_mut() {
            match k {
                egui::Key::Enter => c.save = c.sel.is_some(),
                egui::Key::Escape => self.crop = None,
                _ => {}
            }
            return;
        }
        match &mut self.content {
            Content::Text { rich: Some(r), .. } => {
                if let Rich::Data(t) = &mut **r {
                    t.key(k);
                }
            }
            Content::Sqlite(v) => v.key(k),
            _ => {}
        }
    }

    pub fn toggle_follow(&mut self) {
        self.follow = !self.follow;
        if !self.follow {
            self.tail = None;
        } else {
            self.to_end = true;
        }
    }
}

impl Inspector {
    /// Something is selected in the view shown.
    pub fn has_selection(&self) -> bool {
        use crate::textsel::some;
        match (self.showing, &self.content) {
            (Showing::Text(_) | Showing::Env, Content::Text { .. }) => some(&self.tsel),
            (Showing::Markdown, Content::Text { rich: Some(r), .. }) => matches!(&**r, Rich::Markdown(v) | Rich::Book(v) if some(&v.sel)),
            (Showing::Tree, Content::Text { rich: Some(r), .. }) => matches!(&**r, Rich::Data(t) if some(&t.tsel)),
            (Showing::Table, Content::Text { rich: Some(r), .. }) => matches!(&**r, Rich::Table(t) if t.sel.is_some()),
            (Showing::Sqlite, Content::Sqlite(v)) => matches!(&v.rows, Some((_, Ok(t))) if t.sel.is_some()),
            (Showing::Big, Content::Big(v)) => if v.hex { v.hsel.is_some() } else { some(&v.sel) },
            _ => false,
        }
    }

    /// Ctrl+A: all of the view shown. False when it has nothing to select.
    pub fn select_all(&mut self) -> bool {
        use crate::textsel::{END, Sel};
        match (self.showing, &mut self.content) {
            (Showing::Text(_) | Showing::Env, Content::Text { .. }) => {
                self.tsel = Some(Sel::new((0, 0), END));
                true
            }
            (Showing::Markdown | Showing::Tree | Showing::Table, Content::Text { rich: Some(r), .. }) => match &mut **r {
                Rich::Markdown(v) | Rich::Book(v) => {
                    v.sel = Some(Sel::new((0, 0), END));
                    true
                }
                Rich::Data(t) => {
                    t.tsel = Some(Sel::new((0, 0), END));
                    true
                }
                Rich::Table(t) => t.select_all(),
                _ => false,
            },
            (Showing::Sqlite, Content::Sqlite(v)) => match &mut v.rows {
                Some((_, Ok(t))) => t.select_all(),
                _ => false,
            },
            (Showing::Big, Content::Big(v)) => {
                if v.hex {
                    v.hsel = (v.size > 0).then(|| (Sel::new(0, v.size - 1), false));
                } else {
                    v.sel = Some(Sel::new((0, 0), END));
                }
                true
            }
            _ => false,
        }
    }

    /// The selection's text for the clipboard. A big file's is read on a worker and
    /// lands later.
    pub fn copy_selection(&mut self, ctx: &egui::Context) -> crate::textsel::Copied {
        use crate::textsel::{Copied, lines_text};
        use std::borrow::Cow;
        let sel = self.tsel.filter(|s| !s.is_empty());
        let text = match (self.showing, &mut self.content) {
            (Showing::Text(filtered), Content::Text { lines, .. }) => {
                let Some(s) = sel else { return Copied::Nothing };
                let shown: &[String] = match &self.filtered {
                    Some(f) if filtered => &f.3,
                    _ => lines,
                };
                lines_text(&s, shown.len(), |i| shown.get(i).map(|l| Cow::Borrowed(l.as_str())))
            }
            (Showing::Env, Content::Text { lines, rich: Some(r), .. }) => {
                let (Some(s), Rich::Env(v)) = (sel, &**r) else { return Copied::Nothing };
                // Copied as shown: a value still hidden stays hidden.
                lines_text(&s, lines.len(), |i| lines.get(i).map(|l| Cow::Owned(crate::envfile::shown(l, v.kinds.get(i).copied().unwrap_or(crate::envfile::Line::Hidden), self.env_all || v.open.contains(&i)))))
            }
            (Showing::Markdown | Showing::Tree | Showing::Table, Content::Text { rich: Some(r), .. }) => match &**r {
                Rich::Markdown(v) | Rich::Book(v) => v.copy_text(),
                Rich::Data(t) => t.copy_text(),
                Rich::Table(t) => t.copy_text(),
                _ => String::new(),
            },
            (Showing::Sqlite, Content::Sqlite(v)) => match &v.rows {
                Some((_, Ok(t))) => t.copy_text(),
                _ => String::new(),
            },
            (Showing::Big, Content::Big(v)) => {
                let rx = if v.hex { v.hsel.map(|(s, text)| v.copy_bytes(s, text, ctx)) } else { v.sel.filter(|s| !s.is_empty()).map(|s| v.copy_lines(s, ctx)) };
                let Some(rx) = rx else { return Copied::Nothing };
                self.copying = Some(rx);
                return Copied::Later;
            }
            _ => String::new(),
        };
        if text.is_empty() { Copied::Nothing } else { Copied::Now(text) }
    }

    /// A copy read on a worker, once it is: onto the clipboard, and said.
    fn pump_copy(&mut self, ctx: &egui::Context, actions: &mut Vec<crate::app::Action>) {
        let Some(rx) = &self.copying else { return };
        match rx.try_recv() {
            Ok((text, cut)) => {
                self.copying = None;
                if text.is_empty() {
                    return;
                }
                let say = if cut { format!("Copied the first {}", fsx::fmt_size(text.len() as u64)) } else { crate::textsel::said(&text) };
                ctx.copy_text(text);
                actions.push(crate::app::Action::Toast(say));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.copying = None,
            Err(_) => {}
        }
    }

    /// The selected text of a text preview, for tests: (from, to), in order.
    #[cfg(test)]
    pub fn text_selection(&self) -> Option<(crate::textsel::At, crate::textsel::At)> {
        self.tsel.filter(|s| !s.is_empty()).map(|s| s.span())
    }

    #[cfg(test)]
    pub fn big_selection(&self) -> Option<(crate::textsel::At, crate::textsel::At)> {
        match &self.content {
            Content::Big(v) => v.sel.filter(|s| !s.is_empty()).map(|s| s.span()),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn hex_selection(&self) -> Option<((u64, u64), bool)> {
        match &self.content {
            Content::Big(v) => v.hsel.map(|(s, t)| (s.span(), t)),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn copying(&self) -> bool {
        self.copying.is_some()
    }
}

impl Default for Inspector {
    fn default() -> Self {
        Inspector {
            layout: None,
            line_numbers: true,
            wrap_code: false,
            wrap_prose: true,
            wrapped: None,
            minimap: true,
            pixel_grid: true,
            folder_path: None,
            current: None,
            content: Content::None,
            fit: Fit::Fit,
            zoom: 1.0,
            pan: Vec2::ZERO,
            text_size: 0.0,
            show_meta: false,
            pending: None,
            extra_meta: false,
            extracting: None,
            locked: false,
            git: None,
            goto: None,
            blame_on: false,
            blame: None,
            follow: false,
            tail: None,
            paged_log: false,
            carry: vec![],
            grew: false,
            shown_size: 0,
            first_line: 1,
            to_end: false,
            filter: String::new(),
            filtered: None,
            era: 0,
            env_all: false,
            compare: None,
            crop: None,
            versions: None,
            exif: None,
            find: None,
            keys: false,
            edit: None,
            anim: None,
            tsel: None,
            tsel_for: (0, String::new()),
            showing: Showing::Other,
            copying: None,
        }
    }
}

impl Inspector {
    /// Show `c`. A big text, tree or table goes away on a thread of its own: freeing a
    /// few hundred thousand strings is a stall a frame should not wait for.
    fn set(&mut self, c: Content) {
        self.paged_log = false;
        self.tsel = None;
        let old = std::mem::replace(&mut self.content, c);
        if let Content::Text { lines, rich, spans, .. } = old {
            if lines.len() > 2000 || rich.is_some() {
                std::thread::spawn(move || drop((lines, rich, spans)));
            }
        }
    }

    /// Whether the text shown now wraps.
    pub fn wraps(&self) -> bool {
        match &self.content {
            Content::Text { prose: false, .. } => self.wrap_code,
            _ => self.wrap_prose,
        }
    }

    pub fn toggle_wrap(&mut self) {
        if matches!(self.content, Content::Text { prose: false, .. }) {
            self.wrap_code = !self.wrap_code;
        } else {
            self.wrap_prose = !self.wrap_prose;
        }
    }

    pub fn invalidate(&mut self) {
        self.current = None;
    }

    /// Show the history; the preview (a playing video included) stops until it closes.
    pub fn show_history(&mut self, h: crate::gitlog::History) {
        self.set(Content::None);
        self.current = None;
        self.pending = None;
        self.git = Some(Box::new(h));
        self.compare = None;
        // Just opened, it has the keyboard: Ctrl+F searches it.
        self.keys = true;
    }

    /// Start showing `e`. Anything that touches the disk (folder listings, file contents)
    /// runs on a worker thread, so a huge folder, a slow or hung network mount, or a FIFO
    /// can never stall the UI.
    fn load(&mut self, e: &Entry, computer: bool, ctx: &egui::Context) {
        self.folder_path = None;
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
        self.fit = Fit::Fit;
        self.pending = None;
        self.anim = None;
        if computer {
            self.set(Content::Volume);
            return;
        }
        self.extracting = None;
        self.locked = false;
        // Inside an archive: a file is extracted to the cache and previewed from there.
        if !e.path.exists() {
            if let Some((archive, inner)) = crate::archive::split(&e.path) {
                self.set(Content::Other);
                if !e.is_dir && e.size <= 64 << 20 {
                    let (tx, rx) = std::sync::mpsc::channel();
                    let ctx = ctx.clone();
                    std::thread::spawn(move || {
                        let _ = tx.send(crate::archive::extract_for_open(&archive, &inner));
                        ctx.request_repaint();
                    });
                    self.extracting = Some((rx, e.clone()));
                }
                return;
            }
        }
        // A lone compressed file (notes.txt.gz) previews as the file in it.
        if !e.is_dir && e.size <= 16 << 20 && crate::archive::is_archive(&e.name) && crate::archive::single(&e.path) {
            self.set(Content::Other);
            let (tx, rx) = std::sync::mpsc::channel();
            let (archive, ctx) = (e.path.clone(), ctx.clone());
            let mut inner = e.clone();
            inner.name = crate::archive::stem(&e.name).to_string();
            let name = inner.name.clone();
            std::thread::spawn(move || {
                let _ = tx.send(crate::archive::extract_for_open(&archive, Path::new(&name)));
                ctx.request_repaint();
            });
            self.extracting = Some((rx, inner));
            return;
        }
        if e.special {
            // Sockets, FIFOs and device nodes: opening or reading them can block forever.
            self.set(Content::Other);
            return;
        }
        if !e.is_dir && crate::archive::available() && (crate::books::is_book(e.ext()) || crate::books::is_comic(e.ext())) {
            if crate::books::is_comic(e.ext()) {
                self.set(comic_content(e.path.clone(), ctx));
                return;
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let (path, ctx) = (e.path.clone(), ctx.clone());
            std::thread::spawn(move || {
                let c = match crate::books::read_book(&path) {
                    Ok(md) => {
                        let lines: Vec<String> = md.lines().map(String::from).collect();
                        let view = crate::markdown::View::new(crate::markdown::parse(&lines, path.parent()));
                        Content::Text { lines, truncated: false, spans: None, prose: true, rich: Some(Box::new(Rich::Book(Box::new(view)))), end: 0, from_end: false }
                    }
                    Err(why) => Content::Note(why),
                };
                let _ = tx.send(c);
                ctx.request_repaint();
            });
            self.pending = Some(rx);
            self.set(Content::Loading);
            return;
        }
        if !e.is_dir && crate::fontview::is_font(e.ext()) {
            self.set(font_content(e.path.clone(), ctx));
            return;
        }
        if !e.is_dir {
            let mime = crate::mime::mime_db().for_name(&e.name, false, false);
            if (e.ext() == "pdf" || mime == "application/pdf") && crate::media::have("pdftoppm") {
                self.set(pdf_content(e.path.clone(), ctx));
                return;
            }
            // Before thumbnails: a desktop thumbnailer can picture the first page, but
            // the PDF shows every one.
            if crate::office::is_office(e.ext()) && crate::media::have("pdftoppm") {
                if let Some((t, prog)) = crate::drives::Tools::current().and_then(|t| crate::office::program(&t).map(|p| (t, p))) {
                    self.set(Content::Office(Box::new(crate::office::Conversion::start(t, prog, e.path.clone(), e.size, e.modified, ctx))));
                    return;
                }
            }
            if (mime.starts_with("video/") || mime.starts_with("audio/")) && crate::media::have("ffprobe") {
                let (tx, rx) = std::sync::mpsc::channel();
                let (path, ctx) = (e.path.clone(), ctx.clone());
                std::thread::spawn(move || {
                    let _ = tx.send(crate::media::probe(&path));
                    ctx.request_repaint();
                });
                self.set(Content::Media(Box::new(MediaView { path: e.path.clone(), probe: None, probe_rx: Some(rx), player: None, tex: None, seq: 0, failed: false })));
                return;
            }
        }
        if !e.is_dir && thumbs::can_thumb(e) {
            self.set(Content::Image);
            return;
        }
        // A photo nothing installed reads: what would.
        if let Some(n) = crate::extpic::install_note(e.ext()).filter(|_| !e.is_dir) {
            self.set(Content::Note(n.into()));
            return;
        }
        // A database: its tables through sqlite3, once its first bytes say it is one.
        if !e.is_dir && crate::sqlite::is_db_name(&e.name) {
            let tools = crate::drives::Tools::current().filter(|t| t.has("sqlite3"));
            let (tx, rx) = std::sync::mpsc::channel();
            let (path, ctx) = (e.path.clone(), ctx.clone());
            std::thread::spawn(move || {
                use std::io::Read;
                let mut head = [0u8; 16];
                let db = std::fs::metadata(&path).is_ok_and(|m| m.is_file()) && std::fs::File::open(&path).and_then(|mut f| f.read_exact(&mut head)).is_ok() && crate::sqlite::is_db_header(&head);
                let c = match (db, tools) {
                    (true, Some(t)) => Content::Sqlite(Box::new(crate::sqlite::View::start(path, t, &ctx))),
                    (true, None) => Content::Note("Install sqlite3 to see the tables in this database".into()),
                    _ => read_text(&path, &ctx),
                };
                let _ = tx.send(c);
                ctx.request_repaint();
            });
            self.pending = Some(rx);
            self.set(Content::Loading);
            return;
        }
        if e.is_dir {
            self.folder_path = Some(e.path.clone());
        }
        // An archive shows what is in it, as a folder does (its hex says nothing). Big
        // compressed tarballs are read whole to be listed: those keep the hex.
        let archive = !e.is_dir && e.size <= 64 << 20 && crate::archive::is_archive(&e.name) && crate::archive::available();
        let (tx, rx) = std::sync::mpsc::channel();
        let (path, is_dir, ctx) = (e.path.clone(), e.is_dir, ctx.clone());
        std::thread::spawn(move || {
            let sorted = |mut v: Vec<Entry>| {
                v.retain(|x| !x.hidden);
                v.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| tab::natural_cmp(&a.lname(), &b.lname())));
                v.truncate(5000);
                Content::Folder(v)
            };
            let c = if is_dir {
                sorted(fsx::list_dir(&path).unwrap_or_default())
            } else if archive {
                crate::archive::entries(&path, Path::new("")).map(sorted).unwrap_or_else(|_| read_text(&path, &ctx))
            } else {
                read_text(&path, &ctx)
            };
            let _ = tx.send(c);
            ctx.request_repaint();
        });
        self.pending = Some(rx);
        self.set(Content::Loading);
    }

    fn poll(&mut self) {
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok(c) => {
                    if let Content::Text { from_end, .. } = &c {
                        self.first_line = if *from_end { 0 } else { 1 };
                        // A log opens at its end, where the newest lines are.
                        self.to_end = self.current.as_ref().is_some_and(|(p, _)| p.file_name().is_some_and(|n| is_log(&n.to_string_lossy())));
                    }
                    self.era += 1;
                    self.tail = None;
                    self.set(c);
                    self.pending = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.set(Content::Other);
                    self.pending = None;
                }
                Err(_) => {}
            }
        }
    }
}

/// A PDF's preview, its pages counted on a worker.
fn pdf_content(path: PathBuf, ctx: &egui::Context) -> Content {
    let (tx, rx) = std::sync::mpsc::channel();
    let (p, ctx) = (path.clone(), ctx.clone());
    std::thread::spawn(move || {
        let _ = tx.send(crate::media::pdf_pages(&p));
        ctx.request_repaint();
    });
    Content::Pdf(Box::new(PdfView { path, comic: None, comic_rx: None, pages: None, pages_rx: Some(rx), page: 1, tex: None, shown: None, job: None, failed: false }))
}

/// A comic's preview: its pictures listed on a worker, then shown a page at a time.
fn comic_content(path: PathBuf, ctx: &egui::Context) -> Content {
    let (tx, rx) = std::sync::mpsc::channel();
    let (p, ctx) = (path.clone(), ctx.clone());
    std::thread::spawn(move || {
        let _ = tx.send(crate::books::comic_pages(&p));
        ctx.request_repaint();
    });
    Content::Pdf(Box::new(PdfView { path, comic: None, comic_rx: Some(rx), pages: None, pages_rx: None, page: 1, tex: None, shown: None, job: None, failed: false }))
}

/// A font's preview, read and drawn on a worker at the screen's scale.
fn font_content(path: PathBuf, ctx: &egui::Context) -> Content {
    let (tx, rx) = std::sync::mpsc::channel();
    let ppp = ctx.pixels_per_point();
    let (p, ctx) = (path.clone(), ctx.clone());
    std::thread::spawn(move || {
        let _ = tx.send(crate::fontview::read(&p, ppp, crate::fontview::fonts_dir().as_deref()));
        ctx.request_repaint();
    });
    Content::Font(Box::new(FontView { path, rx: Some(rx), got: None, tex: vec![], ppp }))
}

/// What a text file's preview becomes, by its name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Plain,
    Markdown,
    Json,
    JsonLines,
    Toml,
    Yaml,
    Table,
    Env,
    Log,
}

fn kind_for(name: &str) -> Kind {
    let l = name.to_ascii_lowercase();
    let ext = l.rsplit_once('.').map_or("", |x| x.1);
    if crate::envfile::is_env_name(&l) {
        return Kind::Env;
    }
    match ext {
        "md" | "markdown" | "mdown" | "mkd" | "mkdn" => Kind::Markdown,
        "json" | "jsonc" | "geojson" | "webmanifest" => Kind::Json,
        "jsonl" | "ndjson" => Kind::JsonLines,
        "toml" => Kind::Toml,
        "yaml" | "yml" => Kind::Yaml,
        "csv" | "tsv" | "tab" | "psv" => Kind::Table,
        "log" => Kind::Log,
        _ if l == "cargo.lock" || l == "pipfile" || l == "poetry.lock" => Kind::Toml,
        _ => Kind::Plain,
    }
}

/// Text read for its preview: up to a megabyte as lines (a log: its last), and up to
/// 8 MB for a data file or table, which is parsed whole, on this worker. Anything bigger,
/// and anything that isn't text, is paged.
fn read_text(p: &Path, ctx: &egui::Context) -> Content {
    use std::io::{Read, Seek};
    // Only regular files (a symlink may point at a FIFO or device).
    let Ok(meta) = std::fs::metadata(p) else { return Content::Other };
    if !meta.is_file() {
        return Content::Other;
    }
    let Ok(mut f) = std::fs::File::open(p) else { return Content::Other };
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let kind = kind_for(&name);
    let limit = 1 << 20;
    let parse_limit: usize = if matches!(kind, Kind::Json | Kind::JsonLines | Kind::Toml | Kind::Yaml | Kind::Table) { 8 << 20 } else { limit };
    // A long log is read from its end: the newest lines are what it is opened for.
    let from_end = kind == Kind::Log && meta.len() > limit as u64;
    if from_end && f.seek(std::io::SeekFrom::Start(meta.len() - limit as u64)).is_err() {
        return Content::Other;
    }
    let mut buf = Vec::with_capacity(64 << 10);
    if f.by_ref().take(parse_limit as u64 + 1).read_to_end(&mut buf).is_err() {
        return Content::Other;
    }
    if from_end {
        let cut = buf.iter().position(|&b| b == b'\n').map_or(0, |i| i + 1);
        buf.drain(..cut);
    }
    let end = if from_end { meta.len() } else { buf.len().min(limit) as u64 };
    if buf.is_empty() {
        return Content::Text { lines: vec![], truncated: false, spans: None, prose: true, rich: None, end, from_end };
    }
    let cut = buf.len() > parse_limit;
    buf.truncate(parse_limit);
    let truncated = !from_end && buf.len() > limit;
    let big = |hex| crate::bigfile::Pager::open(p, meta.len(), hex, ctx).map_or(Content::Other, |v| Content::Big(Box::new(v)));
    let Some(lines) = text_lines(&buf[..buf.len().min(limit)]) else {
        return big(true);
    };
    // Too long to keep whole, and not a data file read whole for its tree or table.
    if !from_end && (cut || truncated && !matches!(kind, Kind::Json | Kind::JsonLines | Kind::Toml | Kind::Yaml | Kind::Table)) {
        return big(false);
    }
    let lang = crate::syntax::lang_for(&name);
    let prose = lang.is_none_or(crate::syntax::is_markdown);
    let spans = lang.map(|lang| crate::syntax::highlight(lang, &lines, 20_000));
    let whole = || String::from_utf8_lossy(&buf);
    let tree = |r: Result<crate::datatree::V, String>, what: &str| -> Rich {
        match r {
            // Cut off mid-way, a file is not what it says: the tree would be wrong.
            Ok(_) if cut && kind != Kind::JsonLines => Rich::Broken(format!("Over 8 MB: shown as {what} text")),
            Ok(v) => {
                let mut t = crate::datatree::flatten(&v, None);
                t.note = (t.nodes.len() >= crate::datatree::MAX_NODES - 1).then(|| format!("First {} values", crate::table::fmt_count(t.nodes.len())));
                Rich::Data(Box::new(t))
            }
            Err(e) => Rich::Broken(format!("Can't read this as {what}: {e}")),
        }
    };
    let rich = match kind {
        Kind::Markdown => Some(Rich::Markdown(Box::new(crate::markdown::View::new(crate::markdown::parse(&lines, p.parent()))))),
        Kind::Json if cut => Some(Rich::Broken("Over 8 MB: shown as JSON text".into())),
        Kind::Json => Some(tree(crate::datatree::parse_json(&whole()), "JSON")),
        Kind::JsonLines => Some(tree(crate::datatree::parse_jsonl(&whole()), "JSON Lines")),
        Kind::Toml => Some(tree(crate::datatree::parse_toml(&whole()), "TOML")),
        Kind::Yaml => Some(tree(crate::datatree::parse_yaml(&whole()), "YAML")),
        Kind::Table => Some(Rich::Table(Box::new(crate::table::TableView::new(crate::table::Table::from_text(&whole(), &name, cut))))),
        Kind::Env => Some(Rich::Env(Box::new(crate::envfile::View::new(&lines)))),
        Kind::Plain | Kind::Log => None,
    };
    Content::Text { lines, truncated, spans, prose, rich: rich.map(Box::new), end, from_end }
}

/// A log, by its name: offered Follow and a filter from the start.
pub fn is_log(name: &str) -> bool {
    kind_for(name) == Kind::Log
}

/// Text as preview lines (tabs as spaces, very long lines cut), or None when it looks
/// binary.
pub(crate) fn text_lines(buf: &[u8]) -> Option<Vec<String>> {
    let sniff = &buf[..buf.len().min(8192)];
    if sniff.contains(&0) || std::str::from_utf8(sniff).is_err() && sniff.iter().filter(|b| **b < 9).count() > 4 {
        return None;
    }
    let s = String::from_utf8_lossy(buf);
    // Normalize CRLF and lone CR.
    let s = s.replace("\r\n", "\n").replace('\r', "\n");
    Some(s.lines().map(|l| if l.len() > 2000 { format!("{}…", &l[..l.floor_char_boundary(2000)]) } else { l.replace('\t', "    ") }).collect())
}

/// One icon button of a toolbar segment; `paint` draws the icon in the given color.
pub(crate) fn seg_button(ui: &mut Ui, on: bool, tip: &'static str, pal: &theme::Palette, paint: impl FnOnce(&egui::Painter, Rect, Color32)) -> bool {
    // Narrow inspector: leave out what doesn't fit rather than spill into the next panel.
    if crate::ui::no_room(ui, 24.0) {
        return false;
    }
    let (r, resp) = ui.allocate_exact_size(vec2(24.0, 20.0), Sense::click());
    crate::ui::note_control(ui, r, tip);
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, tip));
    if on {
        ui.painter().rect_filled(r, CornerRadius::same(4), pal.accent.gamma_multiply(0.85));
    } else if resp.hovered() {
        ui.painter().rect_filled(r, CornerRadius::same(4), pal.hover);
    }
    paint(ui.painter(), Rect::from_center_size(r.center(), Vec2::splat(14.0)), if on { Color32::WHITE } else { pal.text_dim });
    resp.on_hover_text(tip).clicked()
}

/// A quiet rounded segment behind a group of toolbar buttons, as Options draws its groups.
pub(crate) fn seg_frame(ui: &mut Ui, pal: &theme::Palette, add: impl FnOnce(&mut Ui)) {
    // No room for even one button: no empty outline either.
    if crate::ui::no_room(ui, 30.0) {
        return;
    }
    // The buttons go straight into the toolbar row, so they centre on it like the close
    // button does; the segment is a fixed 24 px around that centre (buttons are 20 high),
    // painted under them once their width is known.
    let bg = ui.painter().add(egui::Shape::Noop);
    let x0 = ui.cursor().min.x;
    let gap = ui.spacing().item_spacing.x;
    ui.spacing_mut().item_spacing.x = 1.0;
    ui.add_space(2.0);
    add(ui);
    ui.add_space(2.0);
    ui.spacing_mut().item_spacing.x = gap;
    let cy = ui.max_rect().center().y;
    let r = Rect::from_min_max(pos2(x0, cy - 12.0), pos2(ui.cursor().min.x, cy + 12.0));
    ui.painter().set(bg, egui::Shape::rect_filled(r, CornerRadius::same(6), pal.card()));
    ui.painter().rect_stroke(r, CornerRadius::same(6), egui::Stroke::new(1.0, pal.card_line()), egui::StrokeKind::Inside);
    ui.add_space(6.0);
}

/// Window outline with a highlighted region (layout icons).
fn layout_icon(p: &egui::Painter, r: Rect, which: InspectorLayout, col: Color32) {
    let b = r.shrink2(vec2(0.5, 1.5));
    p.rect_stroke(b, CornerRadius::same(2), Stroke::new(1.2, col), StrokeKind::Middle);
    match which {
        InspectorLayout::Auto => {
            p.line_segment([pos2(b.left() + b.width() * 0.38, b.top()), pos2(b.left() + b.width() * 0.38, b.bottom())], Stroke::new(1.2, col));
            p.line_segment([pos2(b.left() + 2.5, b.top() + 3.5), pos2(b.left() + b.width() * 0.38 - 2.0, b.top() + 3.5)], Stroke::new(1.2, col));
        }
        InspectorLayout::Right => {
            let x = b.left() + b.width() * 0.58;
            p.rect_filled(Rect::from_min_max(pos2(x, b.top()), b.max).shrink(1.5), CornerRadius::same(1), col);
        }
        InspectorLayout::Bottom => {
            let y = b.top() + b.height() * 0.55;
            p.rect_filled(Rect::from_min_max(pos2(b.left(), y), b.max).shrink(1.5), CornerRadius::same(1), col);
        }
    }
}

#[allow(dead_code)]
fn tool_button(ui: &mut Ui, label: &str, on: bool, tip: &str, pal: &theme::Palette) -> bool {
    let txt = egui::RichText::new(label).size(11.5).color(if on { pal.accent } else { pal.text_dim });
    let b = egui::Button::new(txt).fill(if on { pal.accent.gamma_multiply(0.18) } else { Color32::TRANSPARENT }).corner_radius(4.0).min_size(vec2(22.0, 20.0));
    ui.add(b).on_hover_text(tip).clicked()
}

pub fn inspector(ui: &mut Ui, rect: Rect, panel: &mut Panel, env: &mut Env) {
    let pal = *env.pal;
    let computer = tab::is_place(&panel.tab().path);
    let target: Option<Entry> = {
        let t = panel.tab();
        let idx = t.cursor.or_else(|| if t.selected.len() == 1 { t.selected.iter().next().copied() } else { None });
        idx.and_then(|i| t.entries.get(i as usize).cloned())
    };
    let vol = target.as_ref().and_then(|e| panel.tab().vols.get(&e.path).cloned());
    let here = panel.tab().path.clone();
    let Some(ins) = panel.inspector.as_mut() else { return };
    // Too short to hold even its toolbar (a squeezed panel): draw nothing rather than
    // spill over the status bar.
    if rect.height() < 34.0 || rect.width() < 40.0 {
        return;
    }
    if let Some(h) = ins.git.as_mut() {
        // The changes follow the status marks; a branch switched elsewhere shows.
        h.follow(crate::gitstatus::repo_for(&here).as_ref(), ui.ctx());
        // A search starts in the folder shown; Ctrl+F is the history's once it is clicked.
        if let Ok(rel) = here.strip_prefix(&h.root) {
            if Path::new(&h.here) != rel {
                h.here = rel.to_string_lossy().into_owned();
            }
        }
        if ui.input(|i| i.pointer.any_pressed()) {
            if let Some(pp) = ui.ctx().pointer_interact_pos() {
                ins.keys = rect.contains(pp);
            }
        }
        // Leaving the work tree closes its history. Diffs wrap as code previews do.
        if !here.starts_with(&h.root) || !crate::gitlog::view(ui, rect, h, &mut ins.wrap_code, env) {
            close_history(panel);
        }
        return;
    }
    if let Some(c) = ins.compare.as_mut() {
        // Moving to another item leaves the comparison for its preview.
        let on = target.as_ref().is_none_or(|e| e.path == c.a || e.path == c.b);
        if on && crate::filediff::view(ui, rect, c, &mut ins.wrap_code, env) {
            return;
        }
        ins.compare = None;
    }
    if ins.edit.is_some() {
        edit_view(ui, rect, panel, env);
        return;
    }
    // Previous versions: the list across the top, the version picked previewed below.
    let (mut rect, mut target) = (rect, target);
    if let Some(v) = ins.versions.as_mut() {
        let band = if target.as_ref().is_some_and(|e| e.path == v.file) { crate::versions::band(ui, rect, v, env) } else { crate::versions::Band::Close };
        match band {
            crate::versions::Band::Close => {
                if v.opened {
                    panel.inspector = None;
                    return;
                }
                ins.versions = None;
            }
            crate::versions::Band::Whole => return,
            crate::versions::Band::Preview(r, e) => {
                rect = r;
                target = Some(e);
            }
        }
    }
    let Some(ins) = panel.inspector.as_mut() else { return };
    ui.painter().rect_filled(rect, CornerRadius::ZERO, pal.bg);
    // A crop belongs to the picture it was started on.
    if ins.crop.as_ref().is_some_and(|c| target.as_ref().map(|e| &e.path) != Some(&c.path)) {
        ins.crop = None;
    }
    let key = target.as_ref().map(|e| (e.path.clone(), e.modified));
    if key != ins.current {
        let same = ins.current.as_ref().map(|c| &c.0) == key.as_ref().map(|k| &k.0) && key.is_some();
        if same && ins.tail.is_some() {
            // Followed: the worker reads what was added, nothing is read again.
            ins.current = key;
        } else {
            ins.current = key;
            if !same {
                ins.tail = None;
                ins.grew = false;
                ins.filtered = None;
            }
            match &target {
                Some(e) => {
                    // The same file again, bigger: it is being written to.
                    ins.grew |= same && e.size > ins.shown_size && matches!(ins.content, Content::Text { .. });
                    ins.shown_size = e.size;
                    ins.load(e, computer, ui.ctx());
                }
                None => {
                    ins.set(Content::None);
                    ins.folder_path = None;
                }
            }
        }
    }
    ins.poll();
    if let Content::Office(c) = &ins.content {
        match c.rx.try_recv() {
            Ok(Ok(pdf)) => ins.set(pdf_content(pdf, ui.ctx())),
            Ok(Err(why)) => ins.set(Content::Note(why)),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => ins.set(Content::Note("Can't preview this document".into())),
            Err(_) => {}
        }
    }
    ins.pump_tail(target.as_ref(), ui.ctx());
    if let Some((rx, e)) = &ins.extracting {
        if let Ok(got) = rx.try_recv() {
            let mut e = e.clone();
            ins.extracting = None;
            match got {
                Ok(path) => {
                    e.path = path;
                    let key = ins.current.clone();
                    ins.load(&e, false, ui.ctx());
                    ins.current = key;
                }
                Err(err) => ins.locked = crate::archive::wants_password(&err),
            }
        }
    }
    // The file is committed in a work tree (blame has something to say), or a log.
    let blame_repo = target.as_ref().filter(|e| !e.is_dir).and_then(|e| {
        let r = e.path.parent().and_then(crate::gitstatus::repo_for).or_else(|| crate::gitstatus::repo_for(&here))?;
        let untracked = e.path.ancestors().take_while(|a| a.starts_with(&r.root) && *a != r.root).any(|a| r.marks.get(a) == Some(&crate::gitstatus::Mark::Untracked));
        (e.path.starts_with(&r.root) && !untracked).then_some(r)
    });
    let blame_ok = blame_repo.is_some();
    let log = target.as_ref().is_some_and(|e| is_log(&e.name));
    // ---------------------------------------------------------- header
    let hdr = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + 30.0));
    ui.painter().line_segment([hdr.left_bottom(), hdr.right_bottom()], Stroke::new(1.0, pal.border));
    let mut close = false;
    let mut hui = ui.new_child(UiBuilder::new().max_rect(hdr.shrink2(vec2(6.0, 3.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    hui.set_clip_rect(hdr.intersect(ui.clip_rect()));
    hui.spacing_mut().item_spacing.x = 2.0;
    if crate::ui::glyph_button(&mut hui, Glyph::Close, "Close inspector (Space)", true, &pal).clicked() {
        close = true;
    }
    hui.add_space(4.0);
    // Toolbar segments, each in a dashed outline: layout | fit, 1:1, fill | grid, minimap.
    let cur_layout = ins.layout.unwrap_or(env.cfg.inspector_layout);
    seg_frame(&mut hui, &pal, |ui| {
        for (l, tip) in [(InspectorLayout::Auto, "Place automatically"), (InspectorLayout::Right, "Dock right"), (InspectorLayout::Bottom, "Dock bottom")] {
            if seg_button(ui, cur_layout == l, tip, &pal, |p, r, c| layout_icon(p, r, l, c)) {
                ins.layout = Some(l);
            }
        }
    });
    match ins.content {
        Content::Image => {
            if let Some(a) = ins.anim.as_mut().filter(|a| a.moving()) {
                seg_frame(&mut hui, &pal, |ui| {
                    let (tip, icon) = if a.playing { ("Pause", "pause") } else { ("Play", "play") };
                    if seg_button(ui, false, tip, &pal, |p, r, c| {
                        theme::paint_symbol(p, r, icon, c);
                    }) {
                        a.toggle();
                    }
                });
            }
            seg_frame(&mut hui, &pal, |ui| {
                for (f, tip) in [(Fit::Fit, "Zoom to fit"), (Fit::Actual, "Actual pixels (1:1)"), (Fit::Fill, "Fill the inspector")] {
                    let on = ins.fit == f && ins.zoom == 1.0;
                    let clicked = seg_button(ui, on, tip, &pal, |p, r, c| match f {
                        Fit::Fit => {
                            theme::paint_symbol(p, r, "minimize-2", c);
                        }
                        Fit::Actual => {
                            p.text(r.center(), Align2::CENTER_CENTER, "1:1", FontId::proportional(10.5), c);
                        }
                        Fit::Fill => {
                            theme::paint_symbol(p, r, "maximize-2", c);
                        }
                    });
                    if clicked {
                        ins.fit = f;
                        ins.zoom = 1.0;
                        ins.pan = Vec2::ZERO;
                    }
                }
            });
            if seg_button(&mut hui, ins.pixel_grid, "Pixel grid at high zoom", &pal, |p, r, c| {
                theme::paint_symbol(p, r, "grid-3x3", c);
            }) {
                ins.pixel_grid = !ins.pixel_grid;
            }
            if seg_button(&mut hui, ins.minimap, "Minimap while zoomed", &pal, |p, r, c| {
                theme::paint_symbol(p, r, "diamond", c);
            }) {
                ins.minimap = !ins.minimap;
            }
        }
        Content::Pdf(ref v) => {
            let (page, pages) = (v.page, v.pages.unwrap_or(0));
            let mut go = None;
            seg_frame(&mut hui, &pal, |ui| {
                if seg_button(ui, false, "Previous page", &pal, |p, r, c| {
                    theme::paint_symbol(p, r, "chevron-left", c);
                }) && page > 1
                {
                    go = Some(page - 1);
                }
                if seg_button(ui, false, "Next page", &pal, |p, r, c| {
                    theme::paint_symbol(p, r, "chevron-right", c);
                }) && page < pages
                {
                    go = Some(page + 1);
                }
                // The page count belongs to the arrows, inside their outline.
                if pages > 0 && !crate::ui::no_room(ui, 40.0) {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(format!("{page} / {pages}")).size(11.5).color(pal.text_dim));
                    ui.add_space(3.0);
                }
            });
            if let (Some(p), Content::Pdf(v)) = (go, &mut ins.content) {
                v.page = p;
            }
        }
        Content::Text { ref rich, .. } => {
            let kind = rich.as_ref().and_then(|r| r.kind());
            let source = kind.is_some_and(|k| env.cfg.source_views.get(k));
            let env_file = matches!(rich.as_deref(), Some(Rich::Env(_)));
            if let Some(k) = kind {
                // Rendered or as written: remembered for each kind of file.
                let (tip, glyph) = match k {
                    RichKind::Markdown => ("Rendered", Glyph::Eye),
                    RichKind::Data => ("Tree", Glyph::ListTree),
                    RichKind::Table => ("Table", Glyph::Table),
                };
                seg_frame(&mut hui, &pal, |ui| {
                    if seg_button(ui, !source, tip, &pal, |p, r, c| paint_glyph(p, r, glyph, c)) && source {
                        env.actions.push(crate::app::Action::SourceView(k, false));
                    }
                    if seg_button(ui, source, "Source", &pal, |p, r, c| paint_glyph(p, r, Glyph::Code, c)) && !source {
                        env.actions.push(crate::app::Action::SourceView(k, true));
                    }
                });
            }
            // A data file that would not parse shows its text either way.
            let book = matches!(rich.as_deref(), Some(Rich::Book(_)));
            let as_text = !book && (kind.is_none() || source || matches!(rich.as_deref(), Some(Rich::Broken(_))));
            seg_frame(&mut hui, &pal, |ui| {
                if as_text && seg_button(ui, ins.line_numbers, "Line numbers", &pal, |p, r, c| {
                    theme::paint_symbol(p, r, "list-ordered", c);
                }) {
                    ins.line_numbers = !ins.line_numbers;
                }
                if as_text && !env_file && seg_button(ui, ins.wraps(), "Wrap lines", &pal, |p, r, c| {
                    paint_glyph(p, r, Glyph::Wrap, c);
                }) {
                    ins.toggle_wrap();
                }
                if seg_button(ui, false, "Smaller text", &pal, |p, r, c| {
                    theme::paint_symbol(p, r, "a-arrow-down", c);
                }) {
                    ins.text_size = (ins.text_size - 1.0).max(-6.0);
                }
                if seg_button(ui, false, "Larger text", &pal, |p, r, c| {
                    theme::paint_symbol(p, r, "a-arrow-up", c);
                }) {
                    ins.text_size = (ins.text_size + 1.0).min(16.0);
                }
            });
            if env_file && seg_button(&mut hui, ins.env_all, if ins.env_all { "Hide values" } else { "Show values" }, &pal, |p, r, c| paint_glyph(p, r, if ins.env_all { Glyph::LockOpen } else { Glyph::Lock }, c)) {
                ins.env_all = !ins.env_all;
            }
            let editable = !book && target.as_ref().is_some_and(|e| !e.is_dir && e.size <= crate::quickedit::MAX && e.path.exists());
            if editable && seg_button(&mut hui, false, "Edit (Shift+F4)", &pal, |p, r, c| {
                theme::paint_symbol(p, r, "pencil", c);
            }) {
                env.actions.push(crate::app::Action::Cmd(crate::commands::Cmd::EditInPreview));
            }
            if as_text && !env_file && blame_ok && seg_button(&mut hui, ins.blame_on, "Blame", &pal, |p, r, c| paint_glyph(p, r, Glyph::Blame, c)) {
                ins.blame_on = !ins.blame_on;
            }
            if as_text && !env_file && (log || ins.grew || ins.follow) && seg_button(&mut hui, ins.follow, "Follow", &pal, |p, r, c| paint_glyph(p, r, Glyph::Follow, c)) {
                ins.toggle_follow();
            }
        }
        Content::Big(ref v) => {
            let hex = v.hex;
            seg_frame(&mut hui, &pal, |ui| {
                if !hex && seg_button(ui, ins.line_numbers, "Line numbers", &pal, |p, r, c| {
                    theme::paint_symbol(p, r, "list-ordered", c);
                }) {
                    ins.line_numbers = !ins.line_numbers;
                }
                if seg_button(ui, false, "Smaller text", &pal, |p, r, c| {
                    theme::paint_symbol(p, r, "a-arrow-down", c);
                }) {
                    ins.text_size = (ins.text_size - 1.0).max(-6.0);
                }
                if seg_button(ui, false, "Larger text", &pal, |p, r, c| {
                    theme::paint_symbol(p, r, "a-arrow-up", c);
                }) {
                    ins.text_size = (ins.text_size + 1.0).min(16.0);
                }
            });
        }
        Content::Sqlite(_) => {
            seg_frame(&mut hui, &pal, |ui| {
                if seg_button(ui, false, "Smaller text", &pal, |p, r, c| {
                    theme::paint_symbol(p, r, "a-arrow-down", c);
                }) {
                    ins.text_size = (ins.text_size - 1.0).max(-6.0);
                }
                if seg_button(ui, false, "Larger text", &pal, |p, r, c| {
                    theme::paint_symbol(p, r, "a-arrow-up", c);
                }) {
                    ins.text_size = (ins.text_size + 1.0).min(16.0);
                }
            });
        }
        _ => {}
    }
    let findable = match &ins.content {
        Content::Text { rich, .. } => !matches!(rich.as_deref(), Some(Rich::Env(_) | Rich::Book(_))),
        Content::Big(_) => true,
        _ => false,
    };
    if findable && seg_button(&mut hui, ins.find.is_some(), "Find (Ctrl+F)", &pal, |p, r, c| paint_glyph(p, r, Glyph::Search, c)) {
        if ins.find.take().is_none() {
            ins.find = Some(crate::findbar::Finder::new(String::new()));
        }
    }
    // File name as a tinted pill; its chevron opens the metadata card.
    if let Some(e) = &target {
        hui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Fit the space the toolbar buttons leave: elide the name, show just the
            // chevron when no name fits, and nothing at all when even that doesn't.
            let room = ui.available_width() - 6.0;
            if room < 24.0 {
                return;
            }
            let name_w = (room - 34.0).min(260.0);
            let g = (name_w >= 24.0).then(|| crate::listing::elided(ui, &e.name, FontId::proportional(12.0), name_w, pal.text));
            let w = g.as_ref().map(|g| g.size().x + 34.0).unwrap_or(24.0);
            let (r, resp) = ui.allocate_exact_size(vec2(w, 22.0), Sense::click());
            let fill = if ins.show_meta { pal.accent.gamma_multiply(0.45) } else if resp.hovered() { pal.accent.gamma_multiply(0.35) } else { pal.accent.gamma_multiply(0.22) };
            ui.painter().rect_filled(r, CornerRadius::same(11), fill);
            if let Some(g) = g {
                ui.painter().galley(pos2(r.left() + 10.0, r.center().y - g.size().y / 2.0), g, pal.text);
            }
            paint_glyph(ui.painter(), Rect::from_center_size(pos2(r.right() - 12.0, r.center().y), Vec2::splat(11.0)), if ins.show_meta { Glyph::ChevronDown } else { Glyph::ChevronRight }, pal.text);
            if resp.on_hover_text("Details").clicked() {
                ins.show_meta = !ins.show_meta;
            }
        });
    }
    if close {
        panel.inspector = None;
        return;
    }
    let body = Rect::from_min_max(pos2(rect.left(), hdr.bottom() + 1.0), rect.max);
    // Clicked, the preview takes Ctrl+F; a click elsewhere gives it back to the list.
    if ui.input(|i| i.pointer.any_pressed()) {
        if let Some(pp) = ui.ctx().pointer_interact_pos() {
            ins.keys = rect.contains(pp);
        }
    }
    let mut bui = ui.new_child(UiBuilder::new().max_rect(body).id_salt(("insp", env.panel_id)));
    bui.set_clip_rect(body.intersect(ui.clip_rect()));
    ins.showing = Showing::Other;
    ins.pump_copy(ui.ctx(), &mut env.actions);
    crate::textsel::forget_painted();
    let Some(e) = target else {
        bui.painter().text(body.center(), Align2::CENTER_CENTER, "Nothing to preview", FontId::proportional(env.cfg.font_size), pal.text_dim);
        return;
    };
    // A long log shows its end; finding in it pages the whole file instead, and closing
    // the bar goes back to the end.
    let finding = ins.find.as_ref().is_some_and(|f| !f.query.is_empty());
    if finding && matches!(ins.content, Content::Text { from_end: true, .. }) {
        if let Some(v) = std::fs::metadata(&e.path).ok().and_then(|m| crate::bigfile::Pager::open(&e.path, m.len(), false, ui.ctx())) {
            ins.set(Content::Big(Box::new(v)));
            ins.tail = None;
            ins.paged_log = true;
        }
    } else if ins.paged_log && ins.find.is_none() {
        ins.paged_log = false;
        ins.invalidate();
    }
    match &mut ins.content {
        Content::Pdf(v) => {
            pdf_view(&mut bui, body, v, &pal);
            return;
        }
        Content::Office(_) => {
            bui.painter().text(body.center(), Align2::CENTER_CENTER, "Preparing preview…", FontId::proportional(12.5), pal.text_dim);
            return;
        }
        // Drawn, then the Details card over it as for a picture: its length, size and codecs.
        Content::Media(v) => media_view(&mut bui, body, &e, v, env),
        Content::Font(v) => {
            font_view(&mut bui, body, v, env);
            return;
        }
        _ => {}
    }
    if matches!(ins.content, Content::Text { .. } | Content::Sqlite(_)) {
        ins.pump_blame(&e, blame_repo.as_deref(), &env.cfg.source_views, ui.ctx());
        if let Some(hash) = text_body(&mut bui, body, ins, &e, env) {
            // A blame line's commit, opened in the history; closing it comes back here.
            if let Some(r) = &blame_repo {
                let mut h = crate::gitlog::History::new(r.root.clone(), r.branch.clone(), false);
                h.open_hash(hash);
                ins.show_history(h);
                return;
            }
        }
    }
    if matches!(ins.content, Content::Big(_)) {
        big_body(&mut bui, body, ins, &e, env);
    }
    // A right-click on what is selected offers to copy it.
    if let Some(pp) = ui.input(|i| i.pointer.interact_pos().filter(|_| i.pointer.button_clicked(egui::PointerButton::Secondary))) {
        if body.contains(pp) && crate::textsel::on_painted(pp) && ins.has_selection() {
            ins.keys = true;
            env.actions.push(crate::app::Action::Open(crate::ctxmenu::build_preview(env.panel_id, pp)));
        }
    }
    match &ins.content {
        Content::Pdf(_) | Content::Office(_) | Content::Media(_) | Content::Font(_) | Content::Text { .. } | Content::Sqlite(_) | Content::Big(_) => {}
        Content::Image => image_view(&mut bui, body, &e, ins, env),
        Content::Folder(entries) => folder_view(&mut bui, body, entries, env),
        Content::Volume => volume_view(&mut bui, body, &e, vol.as_ref(), &pal),
        Content::Loading => {}
        Content::Other | Content::None | Content::Note(_) => {
            let ic = Rect::from_center_size(body.center() - vec2(0.0, 30.0), Vec2::splat(96.0));
            if !crate::icons::entry(bui.painter(), ic, &e) {
                theme::paint_icon(bui.painter(), ic, theme::categorize(&e.ext(), e.is_dir, e.is_executable()), &e.ext(), e.is_link);
            }
            bui.painter().text(pos2(body.center().x, ic.bottom() + 14.0), Align2::CENTER_TOP, &e.name, FontId::proportional(env.cfg.font_size), pal.text);
            let sub = match &ins.content {
                Content::Note(n) => n.clone(),
                _ if ins.locked => "Password-protected".to_string(),
                _ => fsx::fmt_size(e.size),
            };
            bui.painter().text(pos2(body.center().x, ic.bottom() + 34.0), Align2::CENTER_TOP, sub, FontId::proportional(env.cfg.font_size - 1.5), pal.text_dim);
        }
    }
    // A photo's date and place, for the card; read off this thread once it is open.
    let photo = ins.show_meta && matches!(e.ext(), "jpg" | "jpeg" | "png" | "webp" | "tif" | "tiff");
    if !photo {
        ins.exif = None;
    } else if ins.exif.as_ref().is_none_or(|x| x.0 != e.path) {
        let (tx, rx) = std::sync::mpsc::channel();
        let (path, ctx) = (e.path.clone(), ui.ctx().clone());
        std::thread::spawn(move || {
            let _ = tx.send(crate::exif::read(&path));
            ctx.request_repaint();
        });
        ins.exif = Some((e.path.clone(), Some(rx), None));
    }
    if let Some((_, rx @ Some(_), got)) = ins.exif.as_mut() {
        if let Some(Ok(x)) = rx.as_ref().map(|r| r.try_recv()) {
            *got = x;
            *rx = None;
        }
    }
    let exif = ins.exif.as_ref().and_then(|x| x.2.clone());
    // Metadata dropdown overlays the body.
    if ins.show_meta {
        let w = 360.0f32.min(body.width() - 16.0);
        let area = Rect::from_min_size(pos2(body.right() - w - 8.0, body.top() + 6.0), vec2(w, 10.0));
        let mut toggle_extra = false;
        egui::Area::new(Id::new(("meta", env.panel_id))).order(egui::Order::Foreground).fixed_pos(area.min).show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).fill(pal.raised).stroke(Stroke::new(1.0, pal.border)).corner_radius(8.0).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
                ui.set_width(w - 20.0);
                // Card header: the name, the extra-details toggle and the fold chevron.
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&e.name).size(12.5).color(pal.text));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (r, resp) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::click());
                        paint_glyph(ui.painter(), Rect::from_center_size(r.center(), Vec2::splat(11.0)), Glyph::ChevronDown, pal.text_dim);
                        if resp.clicked() {
                            ins.show_meta = false;
                        }
                        let (r, resp) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::click());
                        if ins.extra_meta {
                            ui.painter().rect_filled(r, CornerRadius::same(4), pal.accent.gamma_multiply(0.85));
                        } else if resp.hovered() {
                            ui.painter().rect_filled(r, CornerRadius::same(4), pal.hover);
                        }
                        theme::paint_symbol(ui.painter(), r.shrink(3.0), "info", if ins.extra_meta { Color32::WHITE } else { pal.text_dim });
                        if resp.on_hover_text("Toggle extra details").clicked() {
                            toggle_extra = true;
                        }
                    });
                });
                ui.add_space(4.0);
                let extra = ins.extra_meta;
                let probe = match &ins.content {
                    Content::Media(v) => v.probe.clone(),
                    _ => None,
                };
                egui::Grid::new(("metagrid", env.panel_id)).num_columns(2).spacing(vec2(14.0, 3.0)).show(ui, |ui| {
                    let mut row = |k: &str, v: String| {
                        ui.label(egui::RichText::new(k).color(pal.text_dim).size(11.5));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add(egui::Label::new(egui::RichText::new(v).size(11.5)).truncate());
                        });
                        ui.end_row();
                    };
                    if extra {
                        use std::os::unix::fs::MetadataExt;
                        if let Ok(m) = std::fs::symlink_metadata(&e.path) {
                            row("Accessed", fsx::fmt_time(m.accessed().ok()));
                            row("Changed", fsx::fmt_time(Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(m.ctime().max(0) as u64))));
                            row("Owner", format!("{} ({})", user_name(m.uid()), m.uid()));
                            row("Group", format!("{}", m.gid()));
                            row("Links", m.nlink().to_string());
                            row("Inode", m.ino().to_string());
                            row("Device", format!("{}:{}", libc::major(m.dev()), libc::minor(m.dev())));
                            row("Blocks", format!("{} ({})", m.blocks(), fsx::fmt_size(m.blocks() * 512)));
                        }
                    }
                    row("Path", e.path.display().to_string());
                    let db = crate::mime::mime_db();
                    let m = db.for_name(&e.name, e.is_dir, e.is_executable());
                    row("Type", format!("{} ({m})", db.comment(&m)));
                    let size = if e.is_dir { env.stats.map.get(&e.path).map(|s| format!("{} · {} files · {} folders", fsx::fmt_size(s.size), s.files, s.folders)).unwrap_or_else(|| "—".into()) } else { format!("{} ({} bytes)", fsx::fmt_size(e.size), e.size) };
                    row("Size", size);
                    row("Created", fsx::fmt_time(e.created));
                    row("Modified", fsx::fmt_time(e.modified));
                    row("Permissions", format!("{} ({:o})", fsx::fmt_mode(e.mode, e.is_dir), e.mode & 0o7777));
                    if e.is_link {
                        row("Link target", std::fs::read_link(&e.path).map(|p| p.display().to_string()).unwrap_or_default());
                    }
                    if thumbs::is_image_ext(&e.ext()) {
                        if let Some(l) = env.thumbs.get(&e.path, e.modified, 64.0) {
                            row("Dimensions", format!("{} × {}", l.dims[0], l.dims[1]));
                            if extra {
                                let mp = l.dims[0] as f64 * l.dims[1] as f64 / 1e6;
                                row("Megapixels", format!("{mp:.2}"));
                            }
                        }
                    }
                    if let Some(p) = &probe {
                        if p.duration > 0.0 {
                            row("Duration", crate::media::fmt_time(p.duration));
                        }
                        match p.video {
                            Some(d) => {
                                row("Resolution", crate::facts::fmt_dims(d));
                                row("Codecs", [p.codec.as_str(), p.acodec.as_str()].iter().filter(|c| !c.is_empty()).copied().collect::<Vec<_>>().join(", "));
                            }
                            None if !p.codec.is_empty() => row("Codec", p.codec.clone()),
                            None => {}
                        }
                    }
                    if let Some([y, mo, d, h, mi, _]) = exif.as_ref().and_then(|x| x.taken) {
                        row("Taken", format!("{y}-{mo:02}-{d:02} {h:02}:{mi:02}"));
                    }
                    if let Some((lat, lon)) = exif.as_ref().and_then(|x| x.gps) {
                        ui.label(egui::RichText::new("Location").color(pal.text_dim).size(11.5));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let b = ui.add(egui::Button::new(egui::RichText::new("Open map").size(11.5)).small());
                            crate::ui::note_control(ui, b.rect, "open map");
                            if b.on_hover_text("Show this place on OpenStreetMap, in your browser").clicked() {
                                if let Err(err) = crate::mime::open_uri(&crate::exif::map_url(lat, lon)) {
                                    env.actions.push(crate::app::Action::Toast(format!("Couldn't open the map: {err}")));
                                }
                            }
                            ui.add(egui::Label::new(egui::RichText::new(crate::exif::place(lat, lon)).size(11.5)).truncate());
                        });
                        ui.end_row();
                        // Whether Send To Phone and Email take the place along.
                        ui.label(egui::RichText::new("Shared").color(pal.text_dim).size(11.5));
                        ui.horizontal(|ui| {
                            let mut on = env.cfg.share_without_place;
                            if crate::popups::switch(ui, &mut on, &pal) {
                                env.actions.push(crate::app::Action::Cmd(crate::commands::Cmd::ToggleSharePlace));
                            }
                            ui.label(egui::RichText::new(if on { "without place and camera" } else { "with place and camera" }).size(11.5).color(pal.text_dim));
                        });
                        ui.end_row();
                    }
                    // A download: the address it came from and the page that linked to it,
                    // each opening in the browser.
                    if let Some(o) = (!e.is_dir).then(|| crate::origin::get(&e.path).flatten()).flatten() {
                        let page = o.page.as_deref().filter(|p| Some(*p) != o.url.as_deref());
                        for (k, url) in [("From", o.url.as_deref()), ("Page", page)] {
                            let Some(url) = url else { continue };
                            ui.label(egui::RichText::new(k).color(pal.text_dim).size(11.5));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let open = crate::origin::openable(url);
                                let text = egui::RichText::new(crate::origin::short(url)).size(11.5).color(if open { pal.accent } else { pal.text });
                                let l = ui.add(egui::Label::new(text).truncate().sense(if open { Sense::click() } else { Sense::hover() }));
                                crate::ui::note_control(ui, l.rect, "origin link");
                                if open && l.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                }
                                if l.on_hover_text(url).clicked() {
                                    if let Err(err) = crate::mime::open_uri(url) {
                                        env.actions.push(crate::app::Action::Toast(format!("Couldn't open the address: {err}")));
                                    }
                                }
                            });
                            ui.end_row();
                        }
                    }
                    // A project's code: how many lines, and in what, most first.
                    if let Some(c) = e.is_dir.then(|| crate::linecount::get(&e.path, ui.ctx())).flatten() {
                        code_rows(ui, &c, &pal);
                    }
                });
            });
        });
        if toggle_extra {
            ins.extra_meta = !ins.extra_meta;
        }
    }
}

impl Inspector {
    /// Follow mode: start the worker for the file shown, and take in what it read.
    fn pump_tail(&mut self, e: Option<&Entry>, ctx: &egui::Context) {
        if !self.follow {
            self.tail = None;
            return;
        }
        let Some(e) = e else { return };
        let Content::Text { lines, end, truncated, spans, .. } = &mut self.content else { return };
        if self.tail.is_none() {
            // From where the read stopped; a file read only in part is read from its end.
            let from = (!*truncated).then_some(*end);
            self.tail = crate::tail::Tail::start(e.path.clone(), from, ctx.clone());
            self.carry.clear();
            self.to_end = true;
        }
        let Some((reset, data)) = self.tail.as_ref().and_then(|t| t.take()) else { return };
        if reset {
            lines.clear();
            self.carry.clear();
            self.first_line = 0;
            self.era += 1;
        }
        *truncated = false;
        self.carry.extend_from_slice(&data);
        if let Some(cut) = self.carry.iter().rposition(|&b| b == b'\n') {
            let rest = self.carry.split_off(cut + 1);
            let done = std::mem::replace(&mut self.carry, rest);
            if let Some(new) = text_lines(&done) {
                lines.extend(new);
                self.to_end = true;
                // Appended lines are not highlighted; a log has no colours to lose.
                *spans = None;
            }
        }
        if lines.len() > FOLLOW_LINES {
            let n = lines.len() - FOLLOW_LINES * 3 / 4;
            lines.drain(..n);
            if self.first_line > 0 {
                self.first_line += n;
            }
            self.era += 1;
        }
    }

    /// Blame the file shown when asked to, once per version of it.
    fn pump_blame(&mut self, e: &Entry, repo: Option<&crate::gitstatus::Repo>, source: &crate::config::SourceViews, ctx: &egui::Context) {
        let shows_text = match &self.content {
            Content::Text { rich: None, .. } => true,
            Content::Text { rich: Some(r), .. } => r.kind().is_some_and(|k| source.get(k)),
            _ => false,
        };
        let Some(repo) = repo.filter(|_| self.blame_on && shows_text) else { return };
        let key = (e.path.clone(), e.modified);
        if self.blame.as_ref().map(|b| &b.key) != Some(&key) {
            let (tx, rx) = std::sync::mpsc::channel();
            let (root, path, ctx) = (repo.root.clone(), e.path.clone(), ctx.clone());
            std::thread::spawn(move || {
                let _ = tx.send(crate::blame::run(&root, &path));
                ctx.request_repaint();
            });
            self.blame = Some(BlameJob { key, rx: Some(rx), got: None });
        }
        if let Some(b) = &mut self.blame {
            if let Some(Ok(r)) = b.rx.as_ref().map(|rx| rx.try_recv()) {
                b.got = Some(r);
                b.rx = None;
            }
        }
    }
}

/// Lines holding `q`, ignoring case (ASCII letters; others as they are).
fn has_ci(hay: &str, q: &str) -> bool {
    let (h, n) = (hay.as_bytes(), q.as_bytes());
    n.is_empty() || h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
}

/// Text, code, and the rich previews; returns a blame line's commit that was clicked.
fn text_body(ui: &mut Ui, body: Rect, ins: &mut Inspector, e: &Entry, env: &mut Env) -> Option<String> {
    let mut body = body;
    let pal = *env.pal;
    let size = env.cfg.inspector_font_size + ins.text_size;
    let spacing = env.cfg.preview_line_spacing;
    let id = env.panel_id;
    if let Content::Sqlite(v) = &mut ins.content {
        ins.showing = Showing::Sqlite;
        crate::sqlite::view(ui, body, v, size, &pal, spacing, id);
        return None;
    }
    // A search result's line is in the source, so a rendered file shows its source for it.
    let marking = ins.goto.as_ref().is_some_and(|g| g.1 > 0 && ins.current.as_ref().is_some_and(|c| c.0 == g.0));
    let Content::Text { lines, truncated, spans, prose, rich, .. } = &mut ins.content else { return None };
    if let Some(f) = ins.find.as_mut().filter(|_| !matches!(rich.as_deref(), Some(Rich::Env(_)))) {
        let count = f.count();
        let (out, rest) = crate::findbar::bar(ui, body, f, &count, !f.hits.is_empty(), id, env.cfg.animations, &pal);
        body = rest;
        match out {
            crate::findbar::Out::Next => f.step(1),
            crate::findbar::Out::Prev => f.step(-1),
            crate::findbar::Out::Close => {
                ins.find = None;
                ins.keys = true;
            }
            _ => {}
        }
    }
    let kind = rich.as_ref().and_then(|r| r.kind());
    // While finding, a rendered file shows its source, where the matches are.
    let finding = ins.find.as_ref().is_some_and(|f| !f.query.is_empty());
    let source = marking || finding || kind.is_some_and(|k| env.cfg.source_views.get(k));
    if !source {
        match rich.as_deref_mut() {
            Some(Rich::Markdown(v) | Rich::Book(v)) => {
                ins.showing = Showing::Markdown;
                match crate::markdown::view(ui, body, v, size, &pal, spacing, env.thumbs, id) {
                    Some(crate::markdown::Follow::Open(p)) => env.actions.push(crate::app::Action::OpenFile(p)),
                    Some(crate::markdown::Follow::Folder(p)) => env.actions.push(crate::app::Action::Navigate { panel: id, path: p }),
                    Some(crate::markdown::Follow::Url(u)) if crate::markdown::is_web(&u) => {
                        if let Err(err) = crate::mime::open_uri(&u) {
                            env.actions.push(crate::app::Action::Toast(format!("Couldn't open: {err}")));
                        }
                    }
                    _ => {}
                }
                return None;
            }
            Some(Rich::Data(t)) => {
                ins.showing = Showing::Tree;
                if let Some(path) = crate::datatree::view(ui, body, t, size, &pal, spacing, id) {
                    env.actions.push(crate::app::Action::Toast(format!("Copied {path}")));
                }
                return None;
            }
            Some(Rich::Table(t)) => {
                ins.showing = Showing::Table;
                crate::table::view(ui, body, t, size, &pal, spacing, id);
                return None;
            }
            _ => {}
        }
    }
    if let Some(Rich::Env(v)) = rich.as_deref_mut() {
        ins.showing = Showing::Env;
        crate::envfile::view(ui, body, lines, v, ins.env_all, ins.line_numbers, size, &pal, spacing, &mut ins.tsel);
        return None;
    }
    // A data file that would not parse says why, above its text.
    if let (false, Some(Rich::Broken(msg))) = (source, rich.as_deref()) {
        let bar = Rect::from_min_max(body.min, pos2(body.right(), body.top() + 28.0));
        let g = crate::listing::elided(ui, msg, FontId::proportional(12.0), bar.width() - 30.0, pal.text_dim);
        ui.painter().circle_filled(pos2(bar.left() + 12.0, bar.center().y), 3.0, pal.danger);
        ui.painter().galley(pos2(bar.left() + 22.0, bar.center().y - g.size().y / 2.0), g, pal.text_dim);
        ui.painter().line_segment([bar.left_bottom(), bar.right_bottom()], Stroke::new(1.0, pal.border));
        body.min.y = bar.bottom() + 1.0;
    }
    // A log, or a file that grew while shown: a filter along the bottom.
    let mut shown: &[String] = lines;
    let filtering = is_log(&e.name) || ins.follow || ins.grew;
    if filtering && body.height() > 90.0 {
        let bar = Rect::from_min_max(pos2(body.left(), body.bottom() - 30.0), body.max);
        body.max.y = bar.top() - 1.0;
        ui.painter().line_segment([pos2(bar.left(), bar.top() - 0.5), pos2(bar.right(), bar.top() - 0.5)], Stroke::new(1.0, pal.border));
        let q = ins.filter.trim().to_string();
        // Matches, kept up as lines arrive; made again for a new query or text.
        let stale = ins.filtered.as_ref().is_none_or(|f| f.0 != q || f.1 != ins.era || f.2 > lines.len());
        if stale {
            ins.filtered = Some((q.clone(), ins.era, 0, vec![]));
        }
        if let Some(f) = &mut ins.filtered {
            if !q.is_empty() {
                f.3.extend(lines[f.2..].iter().filter(|l| has_ci(l, &q)).cloned());
            }
            f.2 = lines.len();
        }
        let count = if q.is_empty() { String::new() } else { format!("{} of {}", crate::table::fmt_count(ins.filtered.as_ref().map_or(0, |f| f.3.len())), crate::table::fmt_count(lines.len())) };
        let cg = ui.painter().layout_no_wrap(count, FontId::proportional(11.5), pal.text_dim);
        let field = Rect::from_min_max(pos2(bar.left() + 28.0, bar.top() + 4.0), pos2(bar.right() - cg.size().x - 16.0, bar.bottom() - 4.0));
        paint_glyph(ui.painter(), Rect::from_center_size(pos2(bar.left() + 14.0, bar.center().y), Vec2::splat(13.0)), Glyph::Search, pal.text_dim);
        ui.painter().galley(pos2(bar.right() - 8.0 - cg.size().x, bar.center().y - cg.size().y / 2.0), cg, pal.text_dim);
        if field.width() > 30.0 {
            let mut fui = ui.new_child(UiBuilder::new().max_rect(field).layout(egui::Layout::left_to_right(egui::Align::Center)));
            // The bar's height, whatever the Spacing: a field as tall as a control would be
            // runs out of it.
            fui.spacing_mut().interact_size.y = field.height();
            let te = egui::TextEdit::singleline(&mut ins.filter).id_salt(("log-filter", id)).hint_text(egui::RichText::new("Filter lines").color(pal.text_dim)).frame(crate::ui::bare_edit()).desired_width(field.width()).font(FontId::proportional(12.5));
            let r = fui.add(te);
            crate::ui::note_control(ui, r.rect, "line filter");
        }
        if let Some(f) = ins.filtered.as_ref().filter(|f| !f.0.is_empty()) {
            shown = &f.3;
        }
    }
    let filtered = !std::ptr::eq(shown, lines.as_slice());
    // A selection belongs to the lines it was made on: another text, or another filter,
    // starts without one.
    let made_on = (ins.era, if filtered { ins.filter.trim().to_string() } else { String::new() });
    if ins.tsel_for != made_on {
        ins.tsel = None;
        ins.tsel_for = made_on;
    }
    ins.showing = Showing::Text(filtered);
    // The blame of this file (one left from another file says nothing here).
    let job = ins.blame.as_ref().filter(|b| ins.blame_on && !filtered && b.key.0 == e.path);
    let blame = job.map(|b| match &b.got {
        Some(Ok(b)) => Gutter::Lines(b),
        Some(Err(m)) => Gutter::Note(m),
        None => Gutter::Waiting,
    });
    let wrap = if *prose { ins.wrap_prose } else { ins.wrap_code };
    let wrapped = if wrap { Some(&mut ins.wrapped) } else { None };
    // A search result's line, counted from the text's first kept line.
    let here = ins.current.as_ref().map(|c| &c.0);
    let skip = ins.first_line.max(1) - 1;
    let mark = ins.goto.as_mut().filter(|g| !filtered && Some(&g.0) == here && g.1 as usize > skip).map(|g| (g.1 as usize - 1 - skip, &mut g.2));
    let mut reveal = None;
    let hits = match ins.find.as_mut() {
        Some(f) if !f.query.is_empty() => {
            f.update(shown, ins.era);
            if std::mem::take(&mut f.reveal) {
                reveal = f.hits.get(f.cur).map(|h| (h.0 as usize, h.1 as usize));
            }
            Some((&f.hits[..], Some(f.cur)))
        }
        _ => None,
    };
    let opts = TextOpts { blame, first: if filtered { 0 } else { ins.first_line }, stick: ins.follow, to_end: std::mem::take(&mut ins.to_end), mark, hits, reveal, sel: Some(&mut ins.tsel), ..Default::default() };
    let spans = if filtered { None } else { spans.as_deref() };
    text_view_ex(ui, body, shown, *truncated && !filtered, spans, ins.line_numbers, wrapped, size, &pal, is_patch(&e.name), spacing, opts)
}

/// The preview while its file is edited: the editor under a toolbar of its own, with find
/// and replace.
fn edit_view(ui: &mut Ui, rect: Rect, panel: &mut Panel, env: &mut Env) {
    use crate::app::Action;
    use crate::findbar::Out;
    let pal = *env.pal;
    let id = env.panel_id;
    let Some(ins) = panel.inspector.as_mut() else { return };
    let Some(ed) = ins.edit.as_mut() else { return };
    ui.painter().rect_filled(rect, CornerRadius::ZERO, pal.bg);
    let hdr = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + 30.0));
    ui.painter().line_segment([hdr.left_bottom(), hdr.right_bottom()], Stroke::new(1.0, pal.border));
    let mut hui = ui.new_child(UiBuilder::new().max_rect(hdr.shrink2(vec2(6.0, 3.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    hui.set_clip_rect(hdr.intersect(ui.clip_rect()));
    hui.spacing_mut().item_spacing.x = 2.0;
    if crate::ui::glyph_button(&mut hui, Glyph::Close, "Close inspector (Space)", true, &pal).clicked() {
        env.actions.push(Action::Run(Box::new(move |app| app.leave_edit(id, true))));
    }
    hui.add_space(4.0);
    let dirty = ed.dirty();
    seg_frame(&mut hui, &pal, |ui| {
        if seg_button(ui, true, "Stop editing (Esc)", &pal, |p, r, c| {
            theme::paint_symbol(p, r, "pencil", c);
        }) {
            env.actions.push(Action::Run(Box::new(move |app| app.leave_edit(id, false))));
        }
        // Lit while there is something to save.
        if seg_button(ui, dirty, "Save (Ctrl+S)", &pal, |p, r, c| {
            theme::paint_symbol(p, r, "save", c);
        }) {
            env.actions.push(Action::Run(Box::new(move |app| {
                app.save_edit(id, false);
            })));
        }
    });
    seg_frame(&mut hui, &pal, |ui| {
        if seg_button(ui, ins.line_numbers, "Line numbers", &pal, |p, r, c| {
            theme::paint_symbol(p, r, "list-ordered", c);
        }) {
            ins.line_numbers = !ins.line_numbers;
        }
        if seg_button(ui, ins.wrap_code, "Wrap lines", &pal, |p, r, c| paint_glyph(p, r, Glyph::Wrap, c)) {
            ins.wrap_code = !ins.wrap_code;
        }
        if seg_button(ui, false, "Smaller text", &pal, |p, r, c| {
            theme::paint_symbol(p, r, "a-arrow-down", c);
        }) {
            ins.text_size = (ins.text_size - 1.0).max(-6.0);
        }
        if seg_button(ui, false, "Larger text", &pal, |p, r, c| {
            theme::paint_symbol(p, r, "a-arrow-up", c);
        }) {
            ins.text_size = (ins.text_size + 1.0).min(16.0);
        }
    });
    if seg_button(&mut hui, ins.find.is_some(), "Find and replace (Ctrl+F)", &pal, |p, r, c| paint_glyph(p, r, Glyph::Search, c)) {
        match ins.find.take() {
            Some(_) => ui.memory_mut(|m| m.request_focus(egui::Id::new(("insp-edit", id)))),
            None => ins.find = Some(crate::findbar::Finder::new(String::new())),
        }
    }
    // The file's name, a dot before it while it has changes.
    let name = ed.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    hui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let room = ui.available_width() - 6.0;
        if room < 40.0 {
            return;
        }
        let label = if dirty { format!("● {name}") } else { name };
        let g = crate::listing::elided(ui, &label, FontId::proportional(12.0), (room - 20.0).min(260.0), pal.text);
        let (r, _) = ui.allocate_exact_size(vec2(g.size().x + 20.0, 22.0), Sense::hover());
        ui.painter().rect_filled(r, CornerRadius::same(11), pal.accent.gamma_multiply(0.22));
        ui.painter().galley(pos2(r.left() + 10.0, r.center().y - g.size().y / 2.0), g, pal.text);
    });
    let mut body = Rect::from_min_max(pos2(rect.left(), hdr.bottom() + 1.0), rect.max);
    let mut bui = ui.new_child(UiBuilder::new().max_rect(body).id_salt(("insp-edit-body", id)));
    bui.set_clip_rect(body.intersect(ui.clip_rect()));
    if let Some(f) = ins.find.as_mut() {
        f.replace.get_or_insert_with(String::new);
        let count = f.count();
        let (out, rest) = crate::findbar::bar(&mut bui, body, f, &count, !f.hits.is_empty(), id, env.cfg.animations, &pal);
        body = rest;
        match out {
            Out::Next => f.step(1),
            Out::Prev => f.step(-1),
            Out::Replace => {
                ed.replace(f, false);
            }
            Out::ReplaceAll => {
                let n = ed.replace(f, true);
                env.actions.push(Action::Toast(format!("Replaced {n}")));
            }
            Out::Close => {
                ins.find = None;
                ui.memory_mut(|m| m.request_focus(egui::Id::new(("insp-edit", id))));
            }
            Out::None => {}
        }
    }
    let size = env.cfg.inspector_font_size + ins.text_size;
    crate::quickedit::view(&mut bui, body, ed, ins.find.as_mut(), ins.line_numbers, ins.wrap_code, size, env.cfg.preview_line_spacing, &pal, id);
}

/// A big file: its text a window at a time, or its bytes as hex; found in on a worker.
fn big_body(ui: &mut Ui, body: Rect, ins: &mut Inspector, e: &Entry, env: &mut Env) {
    use crate::findbar::Out;
    let pal = *env.pal;
    let (size, spacing, id) = (env.cfg.inspector_font_size + ins.text_size, env.cfg.preview_line_spacing, env.panel_id);
    let Content::Big(v) = &mut ins.content else { return };
    ins.showing = Showing::Big;
    let mut body = body;
    // The match to scroll to: a line, or a byte offset in hex.
    let mut target = None;
    let mut close = false;
    if let Some(f) = ins.find.as_mut() {
        v.search(&f.query, ui.ctx());
        let (n, done, part) = v.find.as_ref().map_or((0, true, 1.0), |b| (b.hits.lock().unwrap().len(), b.done(), b.seen.load(std::sync::atomic::Ordering::Relaxed) as f32 / v.size.max(1) as f32));
        let cur = v.find.as_ref().map_or(0, |b| b.cur);
        let count = match n {
            _ if f.query.is_empty() => String::new(),
            0 if done => "No matches".into(),
            0 => format!("Searching… {:.0}%", part * 100.0),
            n => format!("{} of {}{}", cur + 1, crate::table::fmt_count(n), if !done { "…" } else if n >= crate::bigfile::MAX_HITS { "+" } else { "" }),
        };
        let (out, rest) = crate::findbar::bar(ui, body, f, &count, n > 0, id, env.cfg.animations, &pal);
        body = rest;
        close = out == Out::Close;
        if let Some(b) = v.find.as_mut().filter(|_| n > 0) {
            let d = match out {
                Out::Next => 1,
                Out::Prev => -1,
                _ => 0,
            };
            if d != 0 || !b.shown {
                b.cur = (b.cur as isize + d).rem_euclid(n as isize) as usize;
                b.shown = true;
                target = b.hits.lock().unwrap().get(b.cur).copied();
            }
        }
    }
    if close {
        ins.find = None;
        v.find = None;
        ins.keys = true;
    }
    let query = ins.find.as_ref().map(|f| f.query.as_str()).filter(|q| !q.is_empty());
    let cur = v.find.as_ref().and_then(|b| b.hits.lock().unwrap().get(b.cur).copied());
    if v.hex {
        hex_view(ui, body, v, size, spacing, query, cur, target, &pal);
    } else {
        let (seen0, seen1) = v.seen;
        v.want(seen0 as u64, seen1 as u64 + 1);
        let (rows, _) = v.rows();
        // Highlighted a window at a time (a block comment begun above it shows plain).
        let held = v.window().map(|(f, l)| (f, l.len()));
        if held.is_some() && v.spans.as_ref().map(|s| (s.0, s.1)) != held {
            v.spans = v.window().and_then(|(f, l)| crate::syntax::lang_for(&e.name).map(|lang| (f, l.len(), crate::syntax::highlight(lang, l, l.len()))));
        }
        let mut pending = target.or(v.reveal);
        let (first, lines): (u64, &[String]) = v.window().unwrap_or((0, &[]));
        let mut hits = vec![];
        if let Some(q) = query {
            let case = q.chars().any(char::is_uppercase);
            let mut at = vec![];
            for (k, l) in lines.iter().enumerate() {
                at.clear();
                crate::findbar::find_in(l, q, case, &mut at);
                hits.extend(at.iter().map(|&(a, b)| ((first + k as u64) as u32, a, b)));
            }
        }
        let cur_hit = cur.and_then(|line| hits.iter().position(|h| h.0 as u64 == line));
        // Down to its line at once; sideways to the match once the line is read.
        let reveal = pending.map(|line| (line as usize, cur_hit.map_or(0, |i| hits[i].1 as usize)));
        if cur_hit.is_some() {
            pending = None;
        }
        let mut seen = v.seen;
        let mut sel = v.sel;
        let opts = TextOpts { window: Some((rows as usize, first as usize, v.estimate() as usize)), hits: query.map(|_| (&hits[..], cur_hit)), reveal, seen: Some(&mut seen), sel: Some(&mut sel), ..Default::default() };
        let spans = v.spans.as_ref().map(|s| &s.2[..]);
        text_view_ex(ui, body, lines, false, spans, ins.line_numbers, None, size, &pal, false, spacing, opts);
        v.sel = sel;
        v.reveal = pending;
        if seen != v.seen {
            v.seen = seen;
            v.want(seen.0 as u64, seen.1 as u64 + 1);
        }
    }
    // Lines are counted on a worker: how far it has got, until it is done.
    let left = 1.0 - v.indexed();
    let shown = ui.ctx().animate_bool_with_time(egui::Id::new(("big-count", id)), left > 0.0, 0.25);
    if shown > 0.0 {
        let txt = if left > 0.0 { format!("Counting lines… {:.0}%", (1.0 - left) * 100.0) } else { format!("{} lines", crate::table::fmt_count(v.rows().0 as usize)) };
        let g = ui.painter().layout_no_wrap(txt, FontId::proportional(11.0), pal.text_dim);
        let r = Rect::from_min_size(pos2(body.right() - g.size().x - 30.0, body.bottom() - g.size().y - 18.0), g.size() + vec2(14.0, 8.0));
        let p = ui.painter().with_clip_rect(body);
        p.rect(r, CornerRadius::same(10), pal.raised.gamma_multiply(shown), Stroke::new(1.0, pal.border.gamma_multiply(shown)), StrokeKind::Inside);
        p.galley(r.min + vec2(7.0, 4.0), g, pal.text_dim.gamma_multiply(shown));
        if left > 0.0 {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
    }
}

/// Bytes as hex beside their text, a row at a time as they are read. The byte under the
/// pointer is marked in both columns; matches of a search are tinted.
#[allow(clippy::too_many_arguments)]
fn hex_view(ui: &mut Ui, body: Rect, v: &mut crate::bigfile::Pager, size: f32, spacing: f32, query: Option<&str>, cur: Option<u64>, target: Option<u64>, pal: &theme::Palette) {
    let font = FontId::monospace(size);
    let cw = ui.fonts_mut(|f| f.glyph_width(&font, 'M'));
    let row_h = row_height(ui, size, spacing);
    let lift = ((row_h - ui.fonts_mut(|f| f.row_height(&font))) / 2.0).floor().max(0.0);
    // Sixteen bytes a row where they fit with their text, else eight.
    let cols = |n: f32| (10.0 + n * 3.0 + (n / 8.0).ceil() + 1.0 + n) * cw + 24.0;
    v.bpr = if body.width() >= cols(16.0) { 16 } else { 8 };
    let bpr = v.bpr as usize;
    let (rows, _) = v.rows();
    let (s0, s1) = v.seen;
    v.want(s0 as u64, s1 as u64 + 1);
    let needle = query.map(crate::bigfile::hex_query).unwrap_or_default();
    let x_hex = 10.0 * cw;
    let x_of = |i: usize| x_hex + (i * 3 + i / 8) as f32 * cw;
    let x_txt = x_of(bpr) + cw;
    let mut inner = ui.new_child(UiBuilder::new().max_rect(body.shrink2(vec2(8.0, 4.0))));
    let sid = inner.make_persistent_id("insp_hex");
    let mut area = egui::ScrollArea::both().id_salt("insp_hex").auto_shrink([false, false]);
    if let Some(off) = target {
        let at = egui::scroll_area::State::load(ui.ctx(), sid).map_or(0.0, |s| s.offset().y);
        let y = (off / v.bpr) as f32 * row_h;
        if y < at || y + row_h > at + inner.max_rect().height() {
            area = area.vertical_scroll_offset((y - inner.max_rect().height() / 3.0).max(0.0));
        }
    }
    let mut seen = v.seen;
    let mut hsel = v.hsel;
    area.show_viewport(&mut inner, |ui, vp| {
        let (r, resp) = ui.allocate_exact_size(vec2((x_txt + bpr as f32 * cw + 8.0).max(vp.width()), rows as f32 * row_h + 8.0), Sense::click_and_drag());
        // Bytes under the mouse, both ends taken: a press picks one (in the column it is
        // in), Shift or a drag stretches to another, a triple-click takes its row.
        if v.size > 0 {
            let at = |pp: Pos2| -> (u64, bool) {
                let row = ((pp.y - r.top()) / row_h).max(0.0) as u64;
                let x = pp.x - r.left();
                let text = x >= x_txt - cw * 0.5;
                let col = if text { ((x - x_txt) / cw).max(0.0) as usize } else { (0..bpr).rev().find(|&i| x >= x_of(i) - cw * 0.5).unwrap_or(0) };
                ((row * v.bpr + col.min(bpr - 1) as u64).min(v.size - 1), text)
            };
            let (pressed, down, shift, origin, pos) = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.modifiers.shift, i.pointer.press_origin(), i.pointer.interact_pos()));
            let on = resp.is_pointer_button_down_on();
            if let Some(o) = origin.filter(|_| pressed && on) {
                let (o, text) = at(o);
                match hsel.as_mut() {
                    Some((s, _)) if shift => s.head = o,
                    _ => hsel = Some((crate::textsel::Sel::at(o), text)),
                }
            } else if let (true, true, Some(pp), Some((s, _))) = (on, down, pos, hsel.as_mut()) {
                s.head = at(pp).0;
                crate::textsel::autoscroll(ui, pp);
            }
            if let (true, Some(pp), Some((s, _))) = (resp.triple_clicked(), pos, hsel.as_mut()) {
                let row = at(pp).0 / v.bpr * v.bpr;
                *s = crate::textsel::Sel::new(row, (row + v.bpr - 1).min(v.size - 1));
            }
        }
        let first = (vp.min.y / row_h).floor().max(0.0) as usize;
        let last = ((vp.max.y / row_h).ceil() as usize + 1).min(rows as usize);
        seen = (first, last);
        let p = ui.painter();
        // The byte under the pointer, by its offset.
        let hover = resp.hover_pos().and_then(|hp| {
            let row = ((hp.y - r.top()) / row_h) as u64;
            let x = hp.x - r.left();
            let col = if x >= x_txt { ((x - x_txt) / cw) as usize } else { (0..bpr).find(|&i| x >= x_of(i) - cw * 0.5 && x < x_of(i) + cw * 2.5)? };
            (col < bpr).then_some(row * v.bpr + col as u64).filter(|&o| o < v.size)
        });
        let mark = |p: &egui::Painter, top: f32, i: usize, col: Color32| {
            p.rect_filled(Rect::from_min_size(pos2(r.left() + x_of(i) - 1.0, top + 1.0), vec2(cw * 2.0 + 2.0, row_h - 2.0)), CornerRadius::same(2), col);
            p.rect_filled(Rect::from_min_size(pos2(r.left() + x_txt + i as f32 * cw, top + 1.0), vec2(cw, row_h - 2.0)), CornerRadius::same(2), col);
        };
        // Matches on screen, by where they start (one may run in from the row above).
        let n = needle.len() as u64;
        let found: Vec<u64> = if n == 0 { vec![] } else { ((first as u64 * v.bpr).saturating_sub(n - 1)..last as u64 * v.bpr).filter(|&s| hit_at(v, s, &needle)).collect() };
        for row in first..last {
            let top = r.top() + row as f32 * row_h;
            let y = top + lift;
            let off = row as u64 * v.bpr;
            p.text(pos2(r.left(), y), Align2::LEFT_TOP, format!("{off:08x}"), font.clone(), pal.text_dim.gamma_multiply(0.7));
            let Some(bytes) = v.row_bytes(row as u64) else {
                p.text(pos2(r.left() + x_hex, y), Align2::LEFT_TOP, "·", font.clone(), pal.text_dim.gamma_multiply(0.5));
                continue;
            };
            // The selection: full in the column it was made in (the one Copy takes),
            // faint in the other.
            if let Some(((lo, hi), text)) = hsel.map(|(s, t)| (s.span(), t)).filter(|_| !bytes.is_empty()) {
                let (a, b) = (lo.max(off), hi.min(off + bytes.len() as u64 - 1));
                if a <= b {
                    let (i0, i1) = ((a - off) as usize, (b - off) as usize);
                    let hex_r = Rect::from_min_max(pos2(r.left() + x_of(i0) - cw * 0.5, top), pos2(r.left() + x_of(i1) + cw * 2.5, top + row_h));
                    let txt_r = Rect::from_min_max(pos2(r.left() + x_txt + i0 as f32 * cw, top), pos2(r.left() + x_txt + (i1 + 1) as f32 * cw, top + row_h));
                    let (main, other) = if text { (txt_r, hex_r) } else { (hex_r, txt_r) };
                    crate::textsel::paint(p, main, pal);
                    p.rect_filled(other, CornerRadius::ZERO, pal.selected.gamma_multiply(0.4));
                }
            }
            for i in 0..bytes.len() {
                let o = off + i as u64;
                let k = found.partition_point(|&s| s + n <= o);
                if let Some(&s) = found.get(k).filter(|&&s| s <= o) {
                    mark(p, top, i, if Some(s) == cur { pal.accent.gamma_multiply(0.55) } else { pal.accent.gamma_multiply(0.25) });
                }
            }
            if let Some(h) = hover.filter(|h| h / v.bpr == row as u64) {
                mark(p, top, (h % v.bpr) as usize, pal.hover);
            }
            let mut job = egui::text::LayoutJob::default();
            let fmt = |c: Color32| egui::TextFormat { font_id: font.clone(), color: c, ..Default::default() };
            for (i, &b) in bytes.iter().enumerate() {
                let c = match b {
                    0 => pal.text_dim.gamma_multiply(0.55),
                    32..127 => pal.text,
                    _ => pal.syntax.number,
                };
                let gap = if i + 1 == bpr { "" } else if i % 8 == 7 { "  " } else { " " };
                job.append(&format!("{b:02x}{gap}"), 0.0, fmt(c));
            }
            p.galley(pos2(r.left() + x_hex, y), ui.fonts_mut(|f| f.layout_job(job)), pal.text);
            let mut job = egui::text::LayoutJob::default();
            for &b in bytes {
                let (ch, c) = if (32..127).contains(&b) { (b as char, pal.text) } else { ('.', pal.text_dim.gamma_multiply(0.6)) };
                job.append(ch.encode_utf8(&mut [0; 4]), 0.0, fmt(c));
            }
            p.galley(pos2(r.left() + x_txt, y), ui.fonts_mut(|f| f.layout_job(job)), pal.text);
        }
        if let Some(h) = hover {
            let b = v.row_bytes(h / v.bpr).and_then(|bs| bs.get((h % v.bpr) as usize)).copied();
            if let Some(b) = b {
                resp.on_hover_text(format!("Offset {h} (0x{h:x})\nByte {b} (0x{b:02x}){}", if (32..127).contains(&b) { format!(" '{}'", b as char) } else { String::new() }));
            }
        }
    });
    v.hsel = hsel;
    if seen != v.seen {
        v.seen = seen;
        v.want(seen.0 as u64, seen.1 as u64 + 1);
    }
}

/// Whether the bytes held from `at` are `needle`.
fn hit_at(v: &crate::bigfile::Pager, at: u64, needle: &[u8]) -> bool {
    needle.iter().enumerate().all(|(k, &n)| {
        let o = at + k as u64;
        v.row_bytes(o / v.bpr).and_then(|b| b.get((o % v.bpr) as usize)) == Some(&n)
    })
}

/// A diff or patch file, shown tinted like the history's diffs.
pub(crate) fn is_patch(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(stem, x)| !stem.is_empty() && (x.eq_ignore_ascii_case("diff") || x.eq_ignore_ascii_case("patch")))
}

/// Close the history, and the inspector with it when it was opened for the history.
pub fn close_history(panel: &mut Panel) {
    if panel.inspector.as_ref().and_then(|i| i.git.as_ref()).is_some_and(|h| h.opened) {
        panel.inspector = None;
    } else if let Some(i) = panel.inspector.as_mut() {
        i.git = None;
    }
}

pub(crate) fn checkerboard(p: &egui::Painter, r: Rect, pal: &theme::Palette) {
    let s = 10.0;
    let (a, b) = if pal.dark { (Color32::from_gray(48), Color32::from_gray(64)) } else { (Color32::from_gray(225), Color32::from_gray(245)) };
    p.rect_filled(r, CornerRadius::ZERO, a);
    let nx = (r.width() / s).ceil() as i32;
    let ny = (r.height() / s).ceil() as i32;
    if nx * ny > 40_000 {
        return;
    }
    for y in 0..ny {
        for x in 0..nx {
            if (x + y) % 2 == 0 {
                let cell = Rect::from_min_size(r.min + vec2(x as f32 * s, y as f32 * s), Vec2::splat(s)).intersect(r);
                p.rect_filled(cell, CornerRadius::ZERO, b);
            }
        }
    }
}

fn image_view(ui: &mut Ui, body: Rect, e: &Entry, ins: &mut Inspector, env: &mut Env) {
    let pal = *env.pal;
    let ppp = ui.ctx().pixels_per_point();
    let area = body.shrink(10.0);
    let fit_px = area.width().max(area.height()) * ppp;
    let want = (fit_px * ins.zoom.max(1.0)).min(4096.0);
    // An animated picture plays (not under the viewer); its first frame shows meanwhile.
    if !crate::anim::may_animate(e.ext()) {
        ins.anim = None;
    } else if ins.anim.as_ref().is_none_or(|a| !a.is_for(&e.path, e.modified, fit_px)) {
        ins.anim = Some(crate::anim::Player::start(&e.path, e.modified, fit_px, env.thumbs.budget_bytes, ui.ctx()));
    }
    let live = !crate::viewer::covers(ui.ctx());
    let moving = ins.anim.as_mut().and_then(|a| a.frame(live));
    let Some((tex, dims)) = moving.or_else(|| env.thumbs.get(&e.path, e.modified, want).map(|l| (l.tex.id(), l.dims))) else {
        let msg = if env.thumbs.failed(&e.path) { "Can't decode this image" } else { "Loading…" };
        ui.painter().text(body.center(), Align2::CENTER_CENTER, msg, FontId::proportional(13.0), pal.text_dim);
        return;
    };
    let [ow, oh] = [dims[0] as f32, dims[1] as f32];
    let base = match ins.fit {
        // A drawing scales without losing anything: it fills the preview however small
        // it says it is. A picture stops at 8 pixels for one.
        Fit::Fit => (area.width() / ow).min(area.height() / oh).min(if e.ext() == "svg" { f32::INFINITY } else { 1.0 / ppp * 8.0 }).max(0.0001),
        Fit::Fill => (area.width() / ow).max(area.height() / oh),
        Fit::Actual => 1.0 / ppp,
    };
    let fit_scale = (area.width() / ow).min(area.height() / oh);
    let scale = base * ins.zoom;
    let size = vec2(ow * scale, oh * scale);
    let resp = ui.interact(body, Id::new(("imgview", env.panel_id)), Sense::click_and_drag());
    // Wheel zoom around the pointer.
    if resp.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        let zd = ui.input(|i| i.zoom_delta());
        let factor = if (zd - 1.0).abs() > 1e-4 { zd } else if scroll != 0.0 { (scroll / 300.0).exp() } else { 1.0 };
        if factor != 1.0 {
            let old = ins.zoom;
            ins.zoom = (ins.zoom * factor).clamp(0.05, 200.0 / base.max(1e-4));
            if let Some(pp) = ui.ctx().pointer_hover_pos() {
                let c = area.center() + ins.pan;
                let rel = pp - c;
                ins.pan += rel - rel * (ins.zoom / old);
            }
        }
    }
    // While cropping, a drag draws the crop instead of moving the picture.
    if resp.dragged() && ins.crop.is_none() {
        ins.pan += resp.drag_delta();
    }
    if resp.double_clicked() {
        ins.zoom = 1.0;
        ins.pan = Vec2::ZERO;
    }
    let img_rect = Rect::from_center_size(area.center() + ins.pan, size);
    let clip = body;
    let painter = ui.painter().with_clip_rect(clip);
    // The picture's own colours, softly around it, and the ones it is made of.
    let colours = env.thumbs.get(&e.path, e.modified, want).map(|l| (l.glow, l.palette.clone()));
    if let Some((glow, _)) = &colours {
        crate::colours::paint_glow(&painter, img_rect, glow, crate::colours::glow_strength(pal.panel));
    }
    checkerboard(&painter, img_rect.intersect(clip), &pal);
    painter.image(tex, img_rect, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
    // Pixel grid at high magnification.
    let px = scale * ppp;
    if ins.pixel_grid && px >= 12.0 {
        let vis = img_rect.intersect(clip);
        let step = scale;
        let x0 = ((vis.left() - img_rect.left()) / step).floor() as i32;
        let x1 = ((vis.right() - img_rect.left()) / step).ceil() as i32;
        let y0 = ((vis.top() - img_rect.top()) / step).floor() as i32;
        let y1 = ((vis.bottom() - img_rect.top()) / step).ceil() as i32;
        if (x1 - x0) < 400 && (y1 - y0) < 400 {
            let st = Stroke::new(1.0, Color32::from_black_alpha(60));
            for x in x0..=x1 {
                let xx = img_rect.left() + x as f32 * step;
                painter.line_segment([pos2(xx, vis.top()), pos2(xx, vis.bottom())], st);
            }
            for y in y0..=y1 {
                let yy = img_rect.top() + y as f32 * step;
                painter.line_segment([pos2(vis.left(), yy), pos2(vis.right(), yy)], st);
            }
        }
    }
    // Minimap
    if ins.minimap && (size.x > area.width() + 1.0 || size.y > area.height() + 1.0) {
        let mw = 120.0;
        let mh = mw * oh / ow;
        let mr = Rect::from_min_size(pos2(body.right() - mw - 12.0, body.bottom() - mh - 30.0), vec2(mw, mh));
        painter.rect_filled(mr.expand(3.0), CornerRadius::same(4), pal.raised);
        painter.image(tex, mr, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE.gamma_multiply(0.85));
        let vis = clip.intersect(img_rect);
        let to_m = |p: Pos2| pos2(mr.left() + (p.x - img_rect.left()) / size.x * mw, mr.top() + (p.y - img_rect.top()) / size.y * mh);
        painter.rect_stroke(Rect::from_min_max(to_m(vis.min), to_m(vis.max)), CornerRadius::ZERO, Stroke::new(1.5, pal.accent), StrokeKind::Middle);
    }
    if let Some(c) = ins.crop.as_mut() {
        c.ui(ui, &resp, img_rect, clip, &pal);
    }
    // Footer zoom %
    let pct = scale * ppp * 100.0;
    let label = match &ins.crop {
        Some(c) => c.hint(dims),
        None => format!("{} × {}  ·  {:.0}%", dims[0], dims[1], pct),
    };
    let g = ui.painter().layout_no_wrap(label, FontId::proportional(11.5), pal.text_dim);
    let r = Rect::from_min_size(pos2(body.right() - g.size().x - 18.0, body.bottom() - 24.0), g.size() + vec2(10.0, 6.0));
    ui.painter().rect_filled(r, CornerRadius::same(4), pal.raised.gamma_multiply(0.9));
    ui.painter().galley(r.min + vec2(5.0, 3.0), g, pal.text_dim);
    // Its colours in a pill of their own on the left, the same height: one a click copies.
    if let Some((_, palette)) = colours.filter(|(_, p)| !p.is_empty() && ins.crop.is_none()) {
        let d = (r.height() - 8.0).max(8.0);
        let gap = 4.0;
        let n = palette.len();
        let pill = Rect::from_min_size(pos2(body.left() + 8.0, r.top()), vec2(n as f32 * (d + gap) - gap + 8.0, r.height()));
        if pill.right() + 8.0 < r.left() {
            ui.painter().rect_filled(pill, CornerRadius::same(4), pal.raised.gamma_multiply(0.9));
            for (i, (c, share)) in palette.iter().enumerate() {
                let sr = Rect::from_min_size(pos2(pill.left() + 4.0 + i as f32 * (d + gap), pill.center().y - d / 2.0), vec2(d, d));
                let resp = ui.interact(sr.expand(1.5), Id::new(("swatch", env.panel_id, i)), Sense::click());
                crate::ui::note_control(ui, sr, "picture colour");
                let hovered = resp.hovered();
                ui.painter().rect_filled(if hovered { sr.expand(1.5) } else { sr }, CornerRadius::same(3), *c);
                ui.painter().rect_stroke(sr, CornerRadius::same(3), Stroke::new(1.0, pal.border.gamma_multiply(0.6)), StrokeKind::Inside);
                let hex = crate::colours::hex(*c);
                let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(format!("{hex} · {}% of the picture · click to copy", (share * 100.0).round().max(1.0)));
                if resp.clicked() {
                    ui.ctx().copy_text(hex.clone());
                    env.actions.push(crate::app::Action::Toast(format!("Copied {hex}")));
                }
            }
        }
    }
    let _ = fit_scale;
}

/// Draw `tex` fitted (aspect kept) and centred in `area`; returns where it went.
fn fit_texture(ui: &Ui, area: Rect, tex: &egui::TextureHandle) -> Rect {
    let [w, h] = tex.size();
    let k = (area.width() / w.max(1) as f32).min(area.height() / h.max(1) as f32);
    let r = Rect::from_center_size(area.center(), vec2(w as f32 * k, h as f32 * k));
    ui.painter().image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    r
}

fn pdf_view(ui: &mut Ui, body: Rect, v: &mut PdfView, pal: &theme::Palette) {
    if let Some(rx) = &v.comic_rx {
        if let Ok(r) = rx.try_recv() {
            v.comic_rx = None;
            match r {
                Ok(list) if !list.is_empty() => {
                    v.pages = Some(list.len());
                    v.comic = Some(std::sync::Arc::new(list));
                }
                _ => v.failed = true,
            }
        }
    }
    if let Some(rx) = &v.pages_rx {
        if let Ok(n) = rx.try_recv() {
            v.pages = n;
            v.pages_rx = None;
            v.failed = n.is_none();
            v.page = v.page.clamp(1, n.unwrap_or(1).max(1));
        }
    }
    let ctx = ui.ctx().clone();
    if let Some(rx) = &v.job {
        if let Ok((pg, px, img)) = rx.try_recv() {
            v.job = None;
            match img {
                Some(ci) => {
                    v.tex = Some(ctx.load_texture("pdf-page", ci, egui::TextureOptions::LINEAR));
                    v.shown = Some((pg, px));
                }
                None => v.failed = true,
            }
        }
    }
    let area = body.shrink(14.0);
    // Render at the size the page is shown (in 128 px steps, so resizing doesn't
    // re-render constantly); the previous page stays up until the new one is ready.
    let want = ((area.width().max(area.height() * 0.71) * ctx.pixels_per_point() / 128.0).ceil() * 128.0).clamp(256.0, 2048.0) as u32;
    if v.job.is_none() && !v.failed && v.pages.is_some() && v.shown != Some((v.page, want)) {
        let (tx, rx) = std::sync::mpsc::channel();
        let (path, page, c2, comic) = (v.path.clone(), v.page, ctx.clone(), v.comic.clone());
        std::thread::spawn(move || {
            let pic = match &comic {
                Some(list) => list.get(page - 1).and_then(|m| crate::books::comic_page(&path, m, want)),
                None => crate::media::pdf_page(&path, page, want),
            };
            let img = pic.map(|i| egui::ColorImage::from_rgba_unmultiplied([i.w as usize, i.h as usize], &i.px));
            let _ = tx.send((page, want, img));
            c2.request_repaint();
        });
        v.job = Some(rx);
    }
    let font = FontId::proportional(12.5);
    if v.failed {
        let why = if v.comic.is_some() || v.comic_rx.is_some() || v.pages_rx.is_none() { "Can't show this comic" } else { "Can't render this PDF" };
        ui.painter().text(body.center(), Align2::CENTER_CENTER, why, font, pal.text_dim);
        return;
    }
    match &v.tex {
        Some(tex) => {
            let r = fit_texture(ui, area, tex);
            ui.painter().rect_stroke(r, CornerRadius::same(2), Stroke::new(1.0, pal.border), StrokeKind::Outside);
        }
        None => {
            ui.painter().text(body.center(), Align2::CENTER_CENTER, if v.pages_rx.is_none() { "Opening…" } else { "Rendering page…" }, font, pal.text_dim);
        }
    }
    // Wheel / Page keys over the page turn pages.
    if ui.rect_contains_pointer(body) {
        let pages = v.pages.unwrap_or(1);
        let (pd, pu) = ui.input(|i| (i.key_pressed(egui::Key::PageDown), i.key_pressed(egui::Key::PageUp)));
        if pd && v.page < pages {
            v.page += 1;
        }
        if pu && v.page > 1 {
            v.page -= 1;
        }
    }
}

/// A font: its name, style and version, Install, and the sample lines drawn with it.
fn font_view(ui: &mut Ui, body: Rect, v: &mut FontView, env: &mut Env) {
    let pal = *env.pal;
    let ctx = ui.ctx().clone();
    if let Some(rx) = &v.rx {
        if let Ok(r) = rx.try_recv() {
            v.rx = None;
            match r {
                Ok(p) => {
                    v.tex = p.lines.into_iter().enumerate().map(|(i, (pt, img))| (pt, ctx.load_texture(format!("font-sample-{i}"), img, egui::TextureOptions::LINEAR))).collect();
                    v.got = Some(Ok((p.info, p.installed)));
                }
                Err(e) => v.got = Some(Err(e)),
            }
        }
    }
    // Moved to a screen of another scale: drawn again for it.
    if v.rx.is_none() && v.got.as_ref().is_some_and(|g| g.is_ok()) && (ctx.pixels_per_point() - v.ppp).abs() > 0.01 {
        if let Content::Font(f) = font_content(v.path.clone(), &ctx) {
            v.rx = f.rx;
            v.ppp = f.ppp;
        }
    }
    let fs = env.cfg.font_size;
    let (info, installed) = match &v.got {
        None => {
            ui.painter().text(body.center(), Align2::CENTER_CENTER, "Reading the font…", FontId::proportional(12.5), pal.text_dim);
            return;
        }
        Some(Err(why)) => {
            ui.painter().text(body.center(), Align2::CENTER_CENTER, why, FontId::proportional(12.5), pal.text_dim);
            return;
        }
        Some(Ok((i, inst))) => (i.clone(), *inst),
    };
    let ext = v.path.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
    let inner = body.shrink2(vec2(16.0, 12.0));
    let mut cui = ui.new_child(UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)));
    egui::ScrollArea::vertical().id_salt(("font view", env.panel_id)).auto_shrink([false, false]).show(&mut cui, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.add(egui::Label::new(egui::RichText::new(&info.family).size(fs + 6.0).color(pal.text)).truncate());
                let mut sub = vec![];
                if !info.style.is_empty() {
                    sub.push(info.style.clone());
                }
                if !info.version.is_empty() {
                    sub.push(format!("version {}", info.version));
                }
                if info.glyphs > 0 {
                    sub.push(format!("{} glyphs", info.glyphs));
                }
                ui.add(egui::Label::new(egui::RichText::new(sub.join("  ·  ")).size(fs - 1.5).color(pal.text_dim)).truncate());
            });
        });
        if crate::fontview::installable(&ext) {
            ui.add_space(6.0);
            if installed {
                ui.label(egui::RichText::new("Installed").size(fs - 1.5).color(pal.text_dim));
            } else {
                let b = crate::popups::plain_button(ui, "Install", &pal);
                crate::ui::note_control(ui, b.rect, "font install");
                if b.on_hover_text("Copy it into your fonts folder, for every app").clicked() {
                    let (path, family) = (v.path.clone(), info.family.clone());
                    env.actions.push(crate::app::Action::Run(Box::new(move |app: &mut crate::app::App| crate::fontview::start_install(app, path, family))));
                }
            }
        }
        ui.add_space(12.0);
        let ppp = v.ppp.max(0.5);
        for (pt, tex) in &v.tex {
            ui.label(egui::RichText::new(format!("{pt:.0} pt")).size(fs - 2.5).color(pal.text_dim));
            let [w, h] = tex.size();
            let size = vec2(w as f32 / ppp, h as f32 / ppp);
            // Lines wider than the preview are cut at its edge, as a sample is.
            let (r, _) = ui.allocate_exact_size(vec2(size.x.min(ui.available_width()), size.y), Sense::hover());
            let uv = Rect::from_min_max(Pos2::ZERO, pos2(r.width() / size.x, 1.0));
            ui.painter().image(tex.id(), Rect::from_min_size(r.min, vec2(r.width(), size.y)), uv, pal.text);
            ui.add_space(8.0);
        }
    });
}

fn media_view(ui: &mut Ui, body: Rect, e: &Entry, v: &mut MediaView, env: &mut Env) {
    let pal = *env.pal;
    let ctx = ui.ctx().clone();
    if let Some(rx) = &v.probe_rx {
        if let Ok(p) = rx.try_recv() {
            v.probe_rx = None;
            match p {
                Some(p) => {
                    let max_w = ((body.width() * ctx.pixels_per_point()) as u32).clamp(320, 1280);
                    v.player = Some(crate::media::Player::new(&v.path, &p, max_w, &ctx));
                    v.probe = Some(p);
                }
                None => v.failed = true,
            }
        }
    }
    let font = FontId::proportional(12.5);
    if v.failed {
        ui.painter().text(body.center(), Align2::CENTER_CENTER, "Can't play this file", font, pal.text_dim);
        return;
    }
    let (Some(p), Some(pl)) = (v.probe.clone(), v.player.as_mut()) else {
        ui.painter().text(body.center(), Align2::CENTER_CENTER, "Loading…", font, pal.text_dim);
        return;
    };
    if let Some(px) = pl.tick(&mut v.seq) {
        if let Some((w, h)) = pl.size {
            let ci = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &px);
            match &mut v.tex {
                Some(t) => t.set(ci, egui::TextureOptions::LINEAR),
                None => v.tex = Some(ctx.load_texture("media-frame", ci, egui::TextureOptions::LINEAR)),
            }
        }
    }
    let bar_h = 40.0;
    let stage = Rect::from_min_max(body.min + vec2(10.0, 10.0), pos2(body.right() - 10.0, body.bottom() - bar_h - 6.0));
    let ppp = ctx.pixels_per_point();
    if p.video.is_some() {
        // The live frame once playback has started; the poster frame before.
        let started = pl.playing() || pl.position() > 0.0;
        match (&v.tex, started) {
            (Some(t), true) => {
                fit_texture(ui, stage, t);
            }
            _ => {
                if let Some(l) = env.thumbs.get(&e.path, e.modified, stage.width().max(stage.height()) * ppp) {
                    fit_texture(ui, stage, &l.tex);
                }
            }
        }
    } else {
        // Audio: cover art and tags.
        let side = (stage.width().min(stage.height() - 60.0)).max(48.0).min(320.0);
        let cover = Rect::from_center_size(pos2(stage.center().x, stage.top() + side / 2.0 + 8.0), Vec2::splat(side));
        match env.thumbs.get(&e.path, e.modified, side * ppp) {
            Some(l) => {
                fit_texture(ui, cover, &l.tex);
            }
            None => {
                ui.painter().rect_filled(cover.shrink(side * 0.2), CornerRadius::same(12), pal.raised);
                theme::paint_symbol(ui.painter(), Rect::from_center_size(cover.center(), Vec2::splat(side * 0.3)), "music", pal.text_dim);
            }
        }
        let mut y = cover.bottom() + 14.0;
        let lines = [(p.title.clone().unwrap_or_else(|| e.name.clone()), 14.0, pal.text), (p.artist.clone().unwrap_or_default(), 12.5, pal.text_dim), (p.album.clone().unwrap_or_default(), 12.0, pal.text_dim)];
        for (t, size, col) in lines {
            if t.is_empty() {
                continue;
            }
            let g = crate::listing::elided(ui, &t, FontId::proportional(size), stage.width() - 8.0, col);
            ui.painter().galley(pos2(stage.center().x - g.size().x / 2.0, y), g.clone(), col);
            y += g.size().y + 4.0;
        }
    }
    // Clicking the picture plays / pauses.
    let stage_resp = ui.interact(stage, egui::Id::new(("media-stage", &v.path)), Sense::click());
    if stage_resp.clicked() {
        pl.toggle();
    }
    // Control bar: play/pause, mute, time, seek.
    let bar = Rect::from_min_max(pos2(body.left() + 8.0, body.bottom() - bar_h), pos2(body.right() - 8.0, body.bottom() - 4.0));
    ui.painter().rect_filled(bar, CornerRadius::same(8), pal.raised);
    let btn = |ui: &mut Ui, r: Rect, icon: &str, tip: &str, id: &str| -> bool {
        let resp = ui.interact(r, egui::Id::new((id, &v.path)), Sense::click());
        crate::ui::note_control(ui, r, "media button");
        if resp.hovered() {
            ui.painter().rect_filled(r, CornerRadius::same(6), pal.hover);
        }
        theme::paint_symbol(ui.painter(), r.shrink(5.0), icon, pal.text);
        resp.on_hover_text(tip).clicked()
    };
    let play_r = Rect::from_min_size(pos2(bar.left() + 4.0, bar.center().y - 14.0), vec2(28.0, 28.0));
    if btn(ui, play_r, if pl.playing() { "pause" } else { "play" }, if pl.playing() { "Pause" } else { "Play" }, "media-play") {
        pl.toggle();
    }
    let mute_r = Rect::from_min_size(pos2(play_r.right() + 2.0, play_r.top()), vec2(28.0, 28.0));
    if p.audio && btn(ui, mute_r, if pl.muted { "volume-x" } else { "volume-2" }, if pl.muted { "Unmute" } else { "Mute" }, "media-mute") {
        let m = !pl.muted;
        pl.set_muted(m);
    }
    let time = format!("{} / {}", crate::media::fmt_time(pl.position()), crate::media::fmt_time(pl.duration));
    let tg = ui.painter().layout_no_wrap(time, FontId::proportional(11.5), pal.text_dim);
    let tx = bar.right() - tg.size().x - 10.0;
    ui.painter().galley(pos2(tx, bar.center().y - tg.size().y / 2.0), tg, pal.text_dim);
    let track = Rect::from_min_max(pos2(mute_r.right() + 10.0, bar.center().y - 2.0), pos2(tx - 12.0, bar.center().y + 2.0));
    if track.width() > 20.0 {
        let frac = if pl.duration > 0.0 { (pl.position() / pl.duration).clamp(0.0, 1.0) as f32 } else { 0.0 };
        ui.painter().rect_filled(track, CornerRadius::same(2), pal.border);
        ui.painter().rect_filled(Rect::from_min_size(track.min, vec2(track.width() * frac, track.height())), CornerRadius::same(2), pal.accent);
        ui.painter().circle_filled(pos2(track.left() + track.width() * frac, track.center().y), 5.0, pal.accent);
        let hit = track.expand2(vec2(6.0, 10.0));
        let resp = ui.interact(hit, egui::Id::new(("media-seek", &v.path)), Sense::click_and_drag());
        crate::ui::note_control(ui, hit, "seek bar");
        if resp.clicked() || resp.drag_stopped() {
            if let Some(pp) = resp.interact_pointer_pos() {
                let t = ((pp.x - track.left()) / track.width()).clamp(0.0, 1.0) as f64 * pl.duration;
                pl.seek(t);
            }
        }
    }
    if pl.playing() {
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }
}

/// Highlight colours: the theme's own, or the classic set (`theme::Syntax`).
pub(crate) fn syntax_color(k: crate::syntax::Kind, pal: &theme::Palette) -> Color32 {
    use crate::syntax::Kind;
    let s = &pal.syntax;
    match k {
        Kind::Plain => pal.text,
        Kind::Keyword => s.keyword,
        Kind::Type => s.kind,
        Kind::Str => s.string,
        Kind::Comment => s.comment,
        Kind::Number | Kind::Constant => s.number,
        Kind::Function => s.function,
        Kind::Property => s.property,
        Kind::Meta => s.meta,
    }
}

/// Rows of wrapped text: each is (line, byte where the row starts in that line).
pub(crate) struct Wrapped {
    /// Columns, and which text (its line count, where its lines live, and its first and
    /// last line's own text) they are for.
    key: (usize, usize, usize, (usize, usize), (usize, usize)),
    pub(crate) rows: Vec<(u32, u32)>,
}

/// Columns a character takes in a monospace font: wide East Asian scripts and emoji
/// take two.
pub fn char_cols(c: char) -> usize {
    let u = c as u32;
    if u < 0x1100 {
        return 1;
    }
    let wide = matches!(u, 0x1100..=0x115F | 0x2E80..=0x303E | 0x3041..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x1F300..=0x1FAFF | 0x20000..=0x3FFFD);
    if wide { 2 } else { 1 }
}

/// Where the rows of `line` start when it is wrapped at `cols` columns: after the last
/// space that fits, or mid-word when a word is longer than a row.
pub fn wrap_line(line: &str, cols: usize, out: &mut Vec<u32>) {
    out.push(0);
    let (mut start, mut width, mut space) = (0usize, 0usize, None);
    for (i, c) in line.char_indices() {
        let w = char_cols(c);
        if width + w > cols && i > start {
            // Break after the last space on this row, else right here.
            let at = space.filter(|&s| s > start).unwrap_or(i);
            out.push(at as u32);
            width = line[at..i].chars().map(char_cols).sum();
            start = at;
            space = None;
        }
        width += w;
        if c == ' ' {
            space = Some(i + 1);
        }
    }
}

/// `diff`: lines of a unified diff, tinted green where added and red where removed.
#[allow(clippy::too_many_arguments)]
/// A text preview's row: the font's own line height times the line spacing setting.
pub(crate) fn row_height(ui: &Ui, size: f32, spacing: f32) -> f32 {
    ui.fonts_mut(|f| (f.row_height(&FontId::monospace(size)) * spacing.clamp(1.0, 2.0)).round().max(1.0))
}

pub(crate) fn text_view(ui: &mut Ui, body: Rect, lines: &[String], truncated: bool, spans: Option<&[Vec<(usize, usize, crate::syntax::Kind)>]>, numbers: bool, wrapped: Option<&mut Option<Wrapped>>, size: f32, pal: &theme::Palette, diff: bool, spacing: f32) {
    text_view_ex(ui, body, lines, truncated, spans, numbers, wrapped, size, pal, diff, spacing, TextOpts::default());
}

/// What a blame gutter shows: the lines' commits, why there are none, or nothing yet.
pub(crate) enum Gutter<'a> {
    Lines(&'a crate::blame::Blame),
    Note(&'a str),
    Waiting,
}

/// A text preview's extras: a blame gutter, line numbers that start past 1 (a followed
/// file whose start was dropped; 0: not known), staying at the end as lines arrive, and
/// one line (from 0) marked, scrolled to once while its flag is set.
pub(crate) struct TextOpts<'a> {
    pub blame: Option<Gutter<'a>>,
    pub first: usize,
    pub stick: bool,
    pub to_end: bool,
    pub mark: Option<(usize, &'a mut bool)>,
    /// A big file held a window at a time: (its lines so far, the line `lines[0]` is, the
    /// lines it will have). Lines not read yet show as a dot.
    pub window: Option<(usize, usize, usize)>,
    /// Matches to mark, and which one is current.
    pub hits: Option<(&'a [crate::findbar::Hit], Option<usize>)>,
    /// A line and a byte in it to scroll into view, when out of view.
    pub reveal: Option<(usize, usize)>,
    /// Given the rows shown.
    pub seen: Option<&'a mut (usize, usize)>,
    /// A diff whose changed lines can be picked: those picked, and where the one
    /// clicked this frame goes (with whether Shift was held).
    pub pick: Option<(&'a [bool], &'a mut Option<(usize, bool)>)>,
    /// Text can be selected: what is, by line (counted as `window`'s are) and byte.
    pub sel: Option<&'a mut Option<crate::textsel::Sel>>,
}

impl Default for TextOpts<'_> {
    fn default() -> Self {
        TextOpts { blame: None, first: 1, stick: false, to_end: false, mark: None, window: None, hits: None, reveal: None, seen: None, pick: None, sel: None }
    }
}

/// The text preview; returns the commit of a blame line that was clicked.
#[allow(clippy::too_many_arguments)]
pub(crate) fn text_view_ex(ui: &mut Ui, body: Rect, lines: &[String], truncated: bool, spans: Option<&[Vec<(usize, usize, crate::syntax::Kind)>]>, numbers: bool, wrapped: Option<&mut Option<Wrapped>>, size: f32, pal: &theme::Palette, diff: bool, spacing: f32, opts: TextOpts) -> Option<String> {
    let font = FontId::monospace(size);
    let (line_h, cw) = ui.fonts_mut(|f| (f.row_height(&font), f.glyph_width(&font, 'M')));
    let row_h = row_height(ui, size, spacing);
    // Text sits in the middle of its (taller) row.
    let lift = ((row_h - line_h) / 2.0).floor().max(0.0);
    let numbers = numbers && opts.first > 0;
    let base = opts.first.max(1);
    let (all, off, widest) = opts.window.unwrap_or((lines.len(), 0, lines.len()));
    let gutter = if numbers { ((widest.max(all) + base).max(1).to_string().len() as f32) * size * 0.62 + 16.0 } else { 6.0 };
    // The blame column: author and age where a commit's lines begin; only the age when
    // the preview is narrow.
    let narrow = body.width() < 360.0;
    let blame_w = if opts.blame.is_some() { ((if narrow { 5.0 } else { 16.0 }) * cw + 14.0).round() } else { 0.0 };
    let gutter = gutter + blame_w;
    let mut clicked = None;
    let mut inner = ui.new_child(UiBuilder::new().max_rect(body.shrink2(vec2(4.0, 4.0))));
    // Wrapping: rows are (line, start byte), made again only when the width or the text
    // changes. The scroll bar's room is left free so a row never runs under it.
    let rows: Option<&[(u32, u32)]> = match wrapped {
        Some(slot) => {
            let cols = (((inner.max_rect().width() - gutter - 22.0) / cw.max(1.0)) as usize).max(8);
            // The lines' own buffers too: a text read again can get the old list's address.
            let ends = |l: Option<&String>| l.map_or((0, 0), |l| (l.as_ptr() as usize, l.len()));
            let key = (cols, lines.len(), lines.as_ptr() as usize, ends(lines.first()), ends(lines.last()));
            if slot.as_ref().map(|w| w.key) != Some(key) {
                let mut rows = Vec::with_capacity(lines.len());
                let mut starts = vec![];
                for (i, l) in lines.iter().enumerate() {
                    starts.clear();
                    wrap_line(l, cols, &mut starts);
                    rows.extend(starts.iter().map(|&b| (i as u32, b)));
                }
                *slot = Some(Wrapped { key, rows });
            }
            slot.as_ref().map(|w| w.rows.as_slice())
        }
        None => None,
    };
    let total = rows.map_or(all, |r| r.len());
    let n = total + if truncated { 1 } else { 0 };
    let mut area = if rows.is_some() { egui::ScrollArea::vertical() } else { egui::ScrollArea::both() };
    let mut marked = None;
    if let Some((line, scroll)) = opts.mark {
        if *scroll {
            *scroll = false;
            let row = rows.map_or(line, |r| r.partition_point(|x| (x.0 as usize) < line));
            // A third of the way down, with what leads up to it above.
            area = area.vertical_scroll_offset((row as f32 * row_h - inner.max_rect().height() / 3.0).max(0.0));
        }
        marked = Some(line);
    }
    if let Some((line, byte)) = opts.reveal {
        // Only when out of view: stepping through matches on screen shouldn't jump.
        let at = egui::scroll_area::State::load(ui.ctx(), inner.make_persistent_id("insp_text")).map_or(Vec2::ZERO, |s| s.offset());
        let view = inner.max_rect().size();
        // Wrapped: the last row starting at or before the byte.
        let row = rows.map_or(line, |r| r.partition_point(|x| (x.0 as usize, x.1 as usize) <= (line, byte)).saturating_sub(1));
        let y = row as f32 * row_h;
        if y < at.y || y + row_h > at.y + view.y {
            area = area.vertical_scroll_offset((y - view.y / 3.0).max(0.0));
        }
        if rows.is_none() {
            let x = line.checked_sub(off).and_then(|k| lines.get(k)).map_or(0.0, |l| crate::findbar::hit_x(l, byte, cw));
            if x < at.x || x + gutter + 40.0 > at.x + view.x {
                area = area.horizontal_scroll_offset((x + gutter - view.x / 2.0).max(0.0));
            }
        }
    }
    let now = crate::blame::now();
    let mut pick = opts.pick;
    let picked: &[bool] = pick.as_ref().map_or(&[], |p| p.0);
    let mut sel = opts.sel;
    // This row's line and the bytes of it shown.
    let row_of = |i: usize| -> (usize, usize, usize) {
        match rows {
            Some(rows) => {
                let (li, a) = (rows[i].0 as usize, rows[i].1 as usize);
                (li, a, rows.get(i + 1).filter(|n| n.0 as usize == li).map_or(usize::MAX, |n| n.1 as usize))
            }
            None => (i, 0, usize::MAX),
        }
    };
    let line_of = |li: usize| li.checked_sub(off).and_then(|k| lines.get(k));
    // In a short panel the text gets only the room left (above a find bar, say), not the
    // least height a scroll area would otherwise keep.
    let room = inner.max_rect().height().max(0.0);
    area.auto_shrink([false, false]).min_scrolled_height(room.min(64.0)).max_height(room).stick_to_bottom(opts.stick).id_salt("insp_text").show_viewport(&mut inner, |ui, vp| {
        let w = if rows.is_some() { vp.width() } else { (2000.0 * size * 0.62).min(lines.iter().map(|l| l.len()).max().unwrap_or(0) as f32 * size * 0.62 + gutter + 20.0).max(vp.width()) };
        let sense = if pick.is_some() { Sense::click() } else if sel.is_some() { Sense::click_and_drag() } else { Sense::hover() };
        let (r, resp) = ui.allocate_exact_size(vec2(w, n as f32 * row_h + 8.0), sense);
        if let Some(sel) = sel.as_deref_mut().filter(|_| total > 0) {
            // The place under a point: its row, then the column in it, past the gutter.
            let hit = |pp: Pos2| -> crate::textsel::At {
                let y = pp.y - r.top();
                if y < 0.0 {
                    let (li, a, _) = row_of(0);
                    return (li, a);
                }
                let i = (y / row_h) as usize;
                if i >= total {
                    let (li, ..) = row_of(total - 1);
                    return (li, line_of(li).map_or(0, |l| l.len()));
                }
                let (li, a, b) = row_of(i);
                let Some(l) = line_of(li) else { return (li, 0) };
                let b = b.min(l.len());
                let part = l.get(a..b).unwrap_or("");
                (li, a + crate::textsel::byte_at_col(part, (pp.x - r.left() - gutter) / cw.max(1.0)))
            };
            let word = |(li, b): crate::textsel::At| {
                let (x, y) = line_of(li).map_or((0, 0), |l| crate::textsel::word(l, b));
                ((li, x), (li, y))
            };
            let line = |(li, _): crate::textsel::At| {
                if li + 1 < all { ((li, 0), (li + 1, 0)) } else { ((li, 0), (li, line_of(li).map_or(0, |l| l.len()))) }
            };
            crate::textsel::track(ui, &resp, sel, hit, word, line);
        }
        let sel = sel.as_deref().copied().flatten().filter(|s| !s.is_empty());
        // The changed line under the pointer, when lines can be picked: a click picks it.
        let hover_line = resp.hover_pos().filter(|_| pick.is_some()).map(|hp| ((hp.y - r.top()).max(0.0) / row_h) as usize).filter(|&i| i < total).map(|i| rows.map_or(i, |rw| rw[i].0 as usize)).filter(|&li| matches!(lines[li].as_bytes().first(), Some(b'+' | b'-')));
        if let (Some(li), Some((_, slot))) = (hover_line, pick.as_mut()) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            if resp.clicked() {
                **slot = Some((li, ui.input(|i| i.modifiers.shift)));
            }
        }
        if opts.to_end && n > 0 {
            // At once: a followed log jumps to its newest line, as a terminal does.
            ui.scroll_to_rect_animation(Rect::from_min_size(pos2(r.left(), r.bottom() - row_h), vec2(1.0, row_h)), Some(egui::Align::BOTTOM), egui::style::ScrollAnimation::none());
        }
        let first = (vp.min.y / row_h).floor().max(0.0) as usize;
        let last = ((vp.max.y / row_h).ceil() as usize + 1).min(n);
        if let Some(seen) = opts.seen {
            *seen = (first, last);
        }
        let p = ui.painter().clone();
        let p = &p;
        // The blame column scrolls sideways with the line numbers and the text.
        let bx = r.left();
        let col = Rect::from_min_max(pos2(bx, r.top() + vp.min.y), pos2(bx + blame_w - 6.0, r.top() + vp.max.y));
        let blame_hit = (blame_w > 0.0).then(|| ui.interact(col, ui.id().with("blame"), Sense::click()));
        if let Some(h) = &blame_hit {
            crate::ui::note_control(ui, col, "blame gutter");
            let _ = h;
        }
        let mut hovered_line = None;
        for i in first..last {
            let top = r.top() + i as f32 * row_h;
            let y = top + lift;
            if let (Some(Gutter::Note(t)), 0) = (&opts.blame, i) {
                p.text(pos2(bx + 4.0, y), Align2::LEFT_TOP, *t, FontId::proportional((size - 1.5).max(8.0)), pal.text_dim);
            }
            if i == total {
                p.text(pos2(r.left() + gutter, y), Align2::LEFT_TOP, "… (preview truncated at 1 MB)", font.clone(), pal.text_dim);
                continue;
            }
            let (li, a, b) = row_of(i);
            let Some(line) = line_of(li) else {
                p.text(pos2(r.left() + gutter, y), Align2::LEFT_TOP, "·", font.clone(), pal.text_dim.gamma_multiply(0.5));
                continue;
            };
            let b = b.min(line.len());
            if a > b || !line.is_char_boundary(a) || !line.is_char_boundary(b) {
                continue;
            }
            if marked == Some(li) {
                p.rect_filled(Rect::from_min_size(pos2(r.left(), top), vec2(r.width(), row_h)), CornerRadius::ZERO, pal.accent.gamma_multiply(0.18));
            }
            if numbers && a == 0 {
                p.text(pos2(r.left() + gutter - 10.0, y), Align2::RIGHT_TOP, (li + base).to_string(), font.clone(), pal.text_dim.gamma_multiply(0.7));
            }
            if let Some(Gutter::Lines(b)) = &opts.blame {
                if let Some(c) = b.lines.get(li).and_then(|&k| b.commits.get(k as usize)) {
                    let ac = if c.committed() { crate::blame::age_color(c.time, now, pal) } else { pal.text_dim.gamma_multiply(0.6) };
                    // A strip down every row of a commit's lines; its name where they begin.
                    p.rect_filled(Rect::from_min_size(pos2(bx + 1.0, top), vec2(2.0, row_h)), CornerRadius::ZERO, ac.gamma_multiply(0.8));
                    let starts = a == 0 && (li == 0 || b.lines.get(li - 1) != b.lines.get(li));
                    if starts {
                        let small = FontId::proportional((size - 1.5).max(8.0));
                        let age = if c.committed() { crate::blame::short_ago(c.time, now) } else { "new".into() };
                        let ag = ui.painter().layout_no_wrap(age, small.clone(), ac);
                        let ax = bx + blame_w - 10.0 - ag.size().x;
                        p.galley(pos2(ax, top + (row_h - ag.size().y) / 2.0), ag, ac);
                        if !narrow {
                            let who = if c.committed() { crate::blame::short_author(&c.author).to_string() } else { "Not committed".into() };
                            let g = crate::listing::elided(ui, &who, small, (ax - bx - 14.0).max(8.0), pal.text_dim);
                            p.galley(pos2(bx + 8.0, top + (row_h - g.size().y) / 2.0), g, pal.text_dim);
                        }
                    }
                    if blame_hit.as_ref().and_then(|h| h.hover_pos()).is_some_and(|hp| hp.y >= top && hp.y < top + row_h) {
                        hovered_line = Some(li);
                    }
                }
            }
            // A diff: added and removed lines tinted (every row of them when wrapped),
            // hunk headers dimmed, and the file names (a "---" line with its "+++"
            // after it) dimmed with them rather than read as a line taken out and one put in.
            let k = li - off;
            let names = diff && ((line.starts_with("--- ") && lines.get(k + 1).is_some_and(|n| n.starts_with("+++ "))) || (line.starts_with("+++ ") && k > 0 && lines[k - 1].starts_with("--- ")));
            let mark = if names { Some(b'@') } else if diff { line.as_bytes().first().copied() } else { None };
            let tint = match mark {
                Some(b'+') => crate::gitstatus::GREEN,
                Some(b'-') => pal.danger,
                _ => Color32::TRANSPARENT,
            };
            if tint != Color32::TRANSPARENT {
                p.rect_filled(Rect::from_min_size(pos2(r.left(), top), vec2(r.width(), row_h)), CornerRadius::ZERO, tint.gamma_multiply(if pal.dark { 0.13 } else { 0.11 }));
            }
            // The selection behind the text; a selected line break as a sliver past
            // the line's end.
            if let Some((sa, sb, nl)) = sel.and_then(|s| s.on_line(li, line.len())) {
                let (sa, sb) = (sa.max(a), sb.min(b));
                let tail = if nl && b == line.len() { cw * 0.6 } else { 0.0 };
                if sa <= sb {
                    let x0 = r.left() + gutter + crate::textsel::cols(line, a, sa) * cw;
                    let x1 = r.left() + gutter + crate::textsel::cols(line, a, sb) * cw + tail;
                    if x1 > x0 {
                        crate::textsel::paint(p, Rect::from_min_max(pos2(x0, top), pos2(x1, top + row_h)), pal);
                    }
                }
            }
            if let Some((hits, cur)) = opts.hits {
                // The current match's line, faintly, to find it by at a glance.
                if a == 0 && cur.and_then(|c| hits.get(c)).is_some_and(|h| h.0 as usize == li) {
                    p.rect_filled(Rect::from_min_size(pos2(r.left(), top), vec2(r.width(), row_h)), CornerRadius::ZERO, pal.accent.gamma_multiply(0.07));
                }
                crate::findbar::paint_hits(p, hits, cur, li as u32, a, b, line, r.left() + gutter, top, row_h, cw, pal);
            }
            // A picked line: the accent over its tint, and a bar at its edge.
            if picked.get(li).copied().unwrap_or(false) {
                p.rect_filled(Rect::from_min_size(pos2(r.left(), top), vec2(r.width(), row_h)), CornerRadius::ZERO, pal.accent.gamma_multiply(0.2));
                p.rect_filled(Rect::from_min_size(pos2(r.left(), top), vec2(3.0, row_h)), CornerRadius::ZERO, pal.accent);
            } else if hover_line == Some(li) {
                p.rect_filled(Rect::from_min_size(pos2(r.left(), top), vec2(r.width(), row_h)), CornerRadius::ZERO, pal.hover);
            }
            if mark == Some(b'@') || mark == Some(b'\\') {
                p.text(pos2(r.left() + gutter, y), Align2::LEFT_TOP, &line[a..b], font.clone(), pal.text_dim);
                continue;
            }
            let line_spans: &[(usize, usize, crate::syntax::Kind)] = spans.and_then(|s| s.get(k)).map_or(&[][..], |s| &s[..]);
            match Some(line_spans).filter(|s| !s.is_empty() || tint != Color32::TRANSPARENT) {
                Some(sp) => {
                    // Coloured sections between the spans; plain text in the gaps. A diff's
                    // +/- takes its line's colour.
                    let mut job = egui::text::LayoutJob::default();
                    let fmt = |c: Color32| egui::TextFormat { font_id: font.clone(), color: c, ..Default::default() };
                    let mut at = a;
                    if tint != Color32::TRANSPARENT && a == 0 && b > 0 {
                        job.append(&line[..1], 0.0, fmt(tint));
                        at = 1;
                    }
                    for &(sa, sb, k) in sp {
                        let (sa, sb) = (sa.max(a), sb.min(b));
                        if sa < at || sb > line.len() || sa >= sb {
                            continue;
                        }
                        if sa > at {
                            job.append(&line[at..sa], 0.0, fmt(pal.text));
                        }
                        job.append(&line[sa..sb], 0.0, fmt(syntax_color(k, pal)));
                        at = sb;
                    }
                    if at < b {
                        job.append(&line[at..b], 0.0, fmt(pal.text));
                    }
                    let g = ui.fonts_mut(|f| f.layout_job(job));
                    p.galley(pos2(r.left() + gutter, y), g, pal.text);
                }
                None => {
                    p.text(pos2(r.left() + gutter, y), Align2::LEFT_TOP, &line[a..b], font.clone(), pal.text);
                }
            }
        }
        // A blame line: its commit's summary on hover; a click opens it in the history.
        if let (Some(Gutter::Lines(b)), Some(li), Some(h)) = (&opts.blame, hovered_line, blame_hit) {
            if let Some(c) = b.lines.get(li).and_then(|&k| b.commits.get(k as usize)) {
                if c.committed() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    let tip = format!("{}\n{} · {} · {}", c.summary, c.author, crate::gitlog::ago(c.time, now), &c.hash[..c.hash.len().min(10)]);
                    if h.clicked() {
                        clicked = Some(c.hash.clone());
                    }
                    h.on_hover_text(tip);
                } else {
                    h.on_hover_text("Changed here and not committed yet");
                }
            }
        }
    });
    clicked
}

fn folder_view(ui: &mut Ui, body: Rect, entries: &[Entry], env: &mut Env) {
    let pal = *env.pal;
    let row_h = (env.cfg.font_size + 8.0).round();
    let font = FontId::proportional(env.cfg.font_size - 0.5);
    let small = FontId::proportional(env.cfg.font_size - 2.0);
    if entries.is_empty() {
        ui.painter().text(body.center(), Align2::CENTER_CENTER, "Empty folder", font, pal.text_dim);
        return;
    }
    let mut inner = ui.new_child(UiBuilder::new().max_rect(body.shrink2(vec2(6.0, 4.0))));
    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("insp_folder").show_rows(&mut inner, row_h, entries.len(), |ui, range| {
        for i in range {
            let e = &entries[i];
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
            if resp.hovered() {
                ui.painter().rect_filled(r, CornerRadius::same(4), pal.hover);
            }
            let ic = Rect::from_min_size(pos2(r.left() + 4.0, r.center().y - 7.0), Vec2::splat(14.0));
            if !crate::icons::entry(ui.painter(), ic, &e) {
                theme::paint_icon(ui.painter(), ic, theme::categorize(&e.ext(), e.is_dir, e.is_executable()), "", e.is_link);
            }
            let g = crate::listing::elided(ui, &e.name, font.clone(), r.width() - 110.0, pal.text);
            ui.painter().galley(pos2(ic.right() + 6.0, r.center().y - g.size().y / 2.0), g, pal.text);
            if !e.is_dir {
                ui.painter().text(pos2(r.right() - 4.0, r.center().y), Align2::RIGHT_CENTER, fsx::fmt_size(e.size), small.clone(), pal.text_dim);
            }
        }
    });
}

fn volume_view(ui: &mut Ui, body: Rect, e: &Entry, v: Option<&tab::VolInfo>, pal: &theme::Palette) {
    let c = body.center();
    let mounted = v.is_none_or(|v| v.mounted);
    let crypt = v.map_or(&crate::drives::Crypt::Plain, |v| &v.crypt);
    crate::ui::paint_drive(ui.painter(), Rect::from_center_size(c - vec2(0.0, 60.0), Vec2::splat(64.0)), v.is_some_and(|v| v.removable), crypt, if mounted { pal.text_dim } else { pal.text_dim.gamma_multiply(0.55) }, pal.bg);
    ui.painter().text(c - vec2(0.0, 14.0), Align2::CENTER_CENTER, &e.name, FontId::proportional(16.0), pal.text);
    if let Some(v) = v {
        let lines = if v.mounted {
            let used = v.total.saturating_sub(v.free);
            let frac = if v.total > 0 { used as f32 / v.total as f32 } else { 0.0 };
            let bar = Rect::from_center_size(c + vec2(0.0, 12.0), vec2(body.width().min(320.0) - 40.0, 8.0));
            ui.painter().rect_filled(bar, CornerRadius::same(4), pal.raised);
            ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * frac, 8.0)), CornerRadius::same(4), if frac > 0.9 { pal.danger } else { pal.accent });
            [format!("{} free of {}", fsx::fmt_size(v.free), fsx::fmt_size(v.total)), format!("{} · {}", v.fstype, v.device), e.path.display().to_string()]
        } else if *crypt == crate::drives::Crypt::Locked {
            [format!("{} · Locked", fsx::fmt_size(v.total)), format!("Encrypted · {}", v.device), "Open it to unlock".into()]
        } else {
            [format!("{} · Not mounted", fsx::fmt_size(v.total)), format!("{} · {}", v.fstype, v.device), "Open it to mount".into()]
        };
        for (i, l) in lines.iter().enumerate() {
            let g = crate::listing::elided(ui, l, FontId::proportional(12.5), body.width() - 16.0, pal.text_dim);
            ui.painter().galley(c + vec2(-g.size().x / 2.0, 34.0 + i as f32 * 18.0 - g.size().y / 2.0), g, pal.text_dim);
        }
    }
}

/// Login name for a uid (from /etc/passwd), or the number.
pub(crate) fn user_name(uid: u32) -> String {
    std::fs::read_to_string("/etc/passwd")
        .ok()
        .and_then(|s| s.lines().find_map(|l| {
            let mut f = l.split(':');
            let name = f.next()?;
            f.next();
            (f.next()?.parse::<u32>().ok()? == uid).then(|| name.to_string())
        }))
        .unwrap_or_else(|| uid.to_string())
}

/// A project's lines of code in the details grid: the total, a bar of the languages'
/// shares, then each language (the smallest together as Other).
fn code_rows(ui: &mut Ui, c: &crate::linecount::Count, pal: &crate::theme::Palette) {
    let small = |s: String| egui::RichText::new(s).size(11.5);
    let total = c.lines.max(1) as f32;
    ui.label(small("Code".into()).color(pal.text_dim));
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let what = format!("{} lines{}", crate::ui::fmt_count(c.lines as usize), if c.cut { " in the part read" } else { "" });
        ui.add(egui::Label::new(small(what)).truncate());
    });
    ui.end_row();
    ui.label("");
    let w = ui.available_width().max(40.0);
    let (r, _) = ui.allocate_exact_size(vec2(w, 8.0), Sense::hover());
    let mut x = r.left();
    for l in &c.langs {
        let lw = r.width() * l.lines as f32 / total;
        if lw >= 0.5 {
            ui.painter().rect_filled(Rect::from_min_max(pos2(x, r.top()), pos2((x + lw).min(r.right()), r.bottom())), CornerRadius::ZERO, l.color);
        }
        x += lw;
    }
    // Round ends without a seam between the shares: the bar's own rounding drawn over.
    ui.painter().rect_stroke(r.expand(0.5), CornerRadius::same(4), egui::Stroke::new(1.5, pal.panel), egui::StrokeKind::Outside);
    ui.end_row();
    let shown = if c.langs.len() > 6 { 5 } else { c.langs.len() };
    let other: u64 = c.langs.iter().skip(shown).map(|l| l.lines).sum();
    let rows = c.langs.iter().take(shown).map(|l| (l.name, Some(l.color), l.lines)).chain((other > 0).then_some(("Other", None, other)));
    for (name, col, lines) in rows {
        ui.horizontal(|ui| {
            let (d, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
            ui.painter().rect_filled(d, CornerRadius::same(2), col.unwrap_or(pal.text_dim));
            ui.label(small(name.into()).color(pal.text_dim));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let pct = lines as f32 / total * 100.0;
            let p = if pct < 1.0 { format!("{pct:.1}%") } else { format!("{pct:.0}%") };
            ui.add(egui::Label::new(small(format!("{} · {p}", crate::ui::fmt_count(lines as usize)))).truncate());
        });
        ui.end_row();
    }
}
