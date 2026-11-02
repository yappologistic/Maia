//! The welcome tour, shown once on a first start, and What's New, shown once after an
//! update. Both fill the window: a few pages, each a feature with a short film of it at
//! work; the tour's first pages hold the choices most people make on arrival, What's
//! New's last one the rest of what the version added. Both close with Esc or Skip at any
//! page, and come back only from the palette or Options.
//!
//! The films (assets/tour) are made by tools/tour-media.sh from the simulator, in the
//! Graphite theme with Google Sans Flex and Google Sans Code, a few states each: the
//! frames in between cost nearly a whole picture apiece, which the binary has no room
//! for. They are decoded on a thread of their own, a frame ahead, and only while shown.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};

use egui::{Align2, CornerRadius, FontId, Id, Key, Modifiers, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

use crate::app::App;
use crate::commands::Cmd;
use crate::popups::{self as pp, Popup};
use crate::theme::{Palette, ThemeName};

/// The version What's New describes. A start that has seen an older one shows it.
pub const NEWS_VERSION: &str = "0.7.0";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Welcome,
    News,
}

/// What a page shows besides its words.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Body {
    /// The welcome's opening: its words beside the moving dither and the app's icon.
    Hero,
    /// A film of the feature.
    Film(&'static [u8]),
    /// The choices made on arrival, a few at a time: the look, the text's size, how
    /// things open, what is shown, and the extras.
    Look,
    Size,
    Clicks,
    Show,
    Extras,
    /// Everything else the version added, in groups.
    List,
}

pub struct Page {
    pub title: &'static str,
    pub text: &'static str,
    pub body: Body,
    /// The commands that reach it, shown with their keys as the user has them.
    pub keys: &'static [Cmd],
}

const GOTO: &[u8] = include_bytes!("../assets/tour/goto.webp");
const PALETTE: &[u8] = include_bytes!("../assets/tour/palette.webp");
const SPLIT: &[u8] = include_bytes!("../assets/tour/split.webp");
const PREVIEW: &[u8] = include_bytes!("../assets/tour/preview.webp");
const GIT: &[u8] = include_bytes!("../assets/tour/git.webp");
const GITHUB: &[u8] = include_bytes!("../assets/tour/github.webp");
const THEMES_FILM: &[u8] = include_bytes!("../assets/tour/themes.webp");
const SCRIPTS: &[u8] = include_bytes!("../assets/tour/scripts.webp");
const COPYPATH: &[u8] = include_bytes!("../assets/tour/copypath.webp");

pub const WELCOME: &[Page] = &[
    Page {
        title: "Welcome to Maia",
        text: "A file manager to drive from the keyboard, with the mouse there when you want it. A few choices, then a short look around.",
        body: Body::Hero,
        keys: &[],
    },
    Page {
        title: "Pick a look",
        text: "Point at a theme to see the whole window in it. Every one is in Choose Theme, beside its code colours.",
        body: Body::Look,
        keys: &[Cmd::ChooseTheme],
    },
    Page {
        title: "A comfortable size",
        text: "Text and the room around it, together. The window follows as you pick, and the keys change it a step at a time later.",
        body: Body::Size,
        keys: &[Cmd::FontBigger],
    },
    Page {
        title: "Opening things",
        text: "Two clicks open a file, as on most desktops, or one does, as on the web. Enter always opens what the cursor is on.",
        body: Body::Clicks,
        keys: &[],
    },
    Page {
        title: "What you see",
        text: "Hidden files are the ones whose names start with a dot, settings mostly. Pictures can show what they hold where you look at them.",
        body: Body::Show,
        keys: &[Cmd::ToggleHidden],
    },
    Page {
        title: "Two extras",
        text: "Folders other apps open can open here, and files can be found by what they are about. Both wait in Options when you want them later.",
        body: Body::Extras,
        keys: &[Cmd::Options],
    },
    Page {
        title: "Go anywhere by typing",
        text: "Go To finds a folder by a few letters of its name, from the places you have been, or takes a path and completes it. Typing in a folder filters it.",
        body: Body::Film(GOTO),
        keys: &[Cmd::GoTo, Cmd::AddressBar],
    },
    Page {
        title: "Every command by its name",
        text: "The palette lists every command with its keys. Run one from there, star it for the Quick menu, or give it keys of your own.",
        body: Body::Film(PALETTE),
        keys: &[Cmd::Palette],
    },
    Page {
        title: "Two folders side by side",
        text: "Split the window and copy or move between the panels. Copies run in the background, and Undo takes them back.",
        body: Body::Film(SPLIT),
        keys: &[Cmd::SplitRight, Cmd::FocusNextPanel, Cmd::Undo],
    },
    Page {
        title: "Look inside without opening",
        text: "The inspector shows the file under the cursor beside the list: Markdown, tables, code, pictures, PDFs, sound, video and archives.",
        body: Body::Film(PREVIEW),
        keys: &[Cmd::ToggleInspector],
    },
    Page {
        title: "Git where your files are",
        text: "In a repository, the branch in the status bar opens the history, branches and stash, with pull and push. The changed files can be listed alone.",
        body: Body::Film(GIT),
        keys: &[Cmd::OnlyChanged, Cmd::RecentProjects],
    },
];

pub const NEWS: &[Page] = &[
    Page {
        title: "Maia 0.7.0",
        text: "A calmer look throughout, twenty more themes, your GitHub profile beside your files, and names in every script. The next pages show them at work.",
        body: Body::Hero,
        keys: &[],
    },
    Page {
        title: "Your GitHub beside your files",
        text: "GitHub Profile shows the account gh is signed in to: followers, achievements, the year's contributions and what you did lately. The arrows open a commit's files, Enter its history in your clone, O the commit on GitHub.",
        body: Body::Film(GITHUB),
        keys: &[Cmd::GitHubProfile],
    },
    Page {
        title: "Twenty more themes",
        text: "Choose Theme has twenty more, dark and light, from Tokyo Night Moon to Pencil Light. Type a few letters of a name, or of what it looks like, and Enter wears it.",
        body: Body::Film(THEMES_FILM),
        keys: &[Cmd::ChooseTheme],
    },
    Page {
        title: "Names in every script",
        text: "Persian, Arabic, Hebrew, Hindi, Thai, Greek, Japanese and Korean names are drawn whole, joined as they should be, where they showed as boxes. The font set in Options leads; the letters it lacks come from your system's.",
        body: Body::Film(SCRIPTS),
        keys: &[Cmd::Options],
    },
    Page {
        title: "Copy a path in one step",
        text: "Right-click a file or folder for Copy Path, its full path as it is. Ctrl+Shift+C copies it quoted for a terminal, and in Properties a click on the location copies the folder's.",
        body: Body::Film(COPYPATH),
        keys: &[Cmd::CopyPath],
    },
    Page { title: "Also new", text: "The rest of 0.7.0, by who will use it most.", body: Body::List, keys: &[] },
];

/// Everything else 0.7.0 added: (group, [(icon, name, one line)]).
pub const ALSO: &[(&str, &[(&str, &str, &str)])] = &[
    (
        "A calmer window",
        &[
            ("diamond", "A calmer look", "Less chrome and colour, one set of controls in every bar, menu and dialog, and words where icons said too little."),
            ("scan-eye", "Peek into a folder", "Point at a folder and the first pictures in it fan out."),
            ("search", "A filter you can watch", "Rows glide as the filter narrows them, the letters that matched lit."),
            ("image", "Pictures in their own light", "A preview glows in its picture's colours, its five main colours copy with a click, and Sort By Colour orders a folder by them."),
            ("clock", "A ribbon of dates", "Sorted by date, the scrollbar marks the years, and names the month while you drag it."),
            ("columns-2", "Wipe to compare", "The viewer lays one picture over another with a line to drag between them, and a filmstrip runs along its foot."),
            ("tag", "Coloured folders", "A folder's icon takes the colour of its label."),
        ],
    ),
    (
        "Steadier",
        &[
            ("hard-drive", "A stalled drive holds nothing up", "Properties, the path's folder list and the preview read the disk away from the window, so a network drive that stops answering no longer freezes it."),
            ("list", "Big folders stay quick", "The path's folder list and the Timeline stay smooth with tens of thousands of files."),
            ("file", "Every name opens", "Paths whose names are not UTF-8 open from the command line and from other apps."),
        ],
    ),
    ("Around the desktop", &[("monitor", "No window at login", "A dock that asks Maia about itself at login no longer opens a window.")]),
];

pub fn pages(kind: Kind) -> &'static [Page] {
    match kind {
        Kind::Welcome => WELCOME,
        Kind::News => NEWS,
    }
}

pub struct State {
    pub kind: Kind,
    pub page: usize,
    /// Shown by itself at start: closing it says where it can be found again.
    pub auto: bool,
    film: Option<Film>,
    /// When the page was turned (for its fade in).
    turned: Option<f64>,
    /// When it opened: the welcome fills the window with a fade.
    opened: Option<f64>,
    /// The opening page's moving dither, while that page is shown.
    dither: Option<crate::dither::Dither>,
    /// What's New's lights behind the card, on every page.
    glow: Option<crate::glow::Glow>,
    /// The app's icon, decoded on a thread the first time it is shown.
    icon: Option<egui::TextureHandle>,
    icon_rx: Option<Receiver<egui::ColorImage>>,
    /// How tall the page's words stood last frame (with the button, on the opening
    /// page): they are centred in their column by it.
    block_h: f32,
}

pub fn open(kind: Kind, auto: bool) -> Popup {
    Popup::Tour(Box::new(State { kind, page: 0, auto, film: None, turned: None, opened: None, dither: None, glow: None, icon: None, icon_rx: None, block_h: 0.0 }))
}

impl State {
    pub fn pages(&self) -> &'static [Page] {
        pages(self.kind)
    }
    /// The film on screen has played to its end (or shows its last state, with
    /// animations off).
    pub fn film_done(&self) -> bool {
        self.film.as_ref().is_some_and(|f| f.done && f.tex.is_some())
    }
    /// The film's picture is on screen.
    pub fn film_shown(&self) -> bool {
        self.film.as_ref().is_some_and(|f| f.tex.is_some())
    }
}

// ------------------------------------------------------------------ first start

#[cfg(test)]
thread_local! {
    /// Test builds show neither by themselves unless a test asks: every simulation
    /// starts in a fresh home, which would otherwise be a first start each time.
    pub static AUTO: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A version as numbers ("0.3.0" → [0, 3, 0]); missing parts are 0, anything after a
/// number (a "-rc1") is ignored.
pub fn version(v: &str) -> [u32; 3] {
    let mut out = [0; 3];
    for (i, p) in v.trim().trim_start_matches('v').split('.').take(3).enumerate() {
        out[i] = p.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0);
    }
    out
}

/// What a start shows: the tour when nothing of Maia's is on disk yet, What's New when
/// the newest seen is older than the notes (a 0.2 home had no record of it: older), and
/// nothing otherwise.
pub fn at_start_kind(first: bool, seen: &str, news: &str) -> Option<Kind> {
    if first {
        Some(Kind::Welcome)
    } else if seen.is_empty() || version(seen) < version(news) {
        Some(Kind::News)
    } else {
        None
    }
}

/// On starting: show the tour or What's New when due, and remember this version as
/// seen, written at once so that neither comes back by itself.
pub fn at_start(app: &mut App, first: bool) {
    #[cfg(test)]
    if !AUTO.with(|a| a.get()) {
        return;
    }
    let now = env!("CARGO_PKG_VERSION");
    if version(&app.ud.seen_version) >= version(now) && !first {
        return;
    }
    let kind = at_start_kind(first, &app.ud.seen_version, NEWS_VERSION);
    app.ud.seen_version = now.to_string();
    app.ud_dirty = true;
    app.save_userdata();
    if let Some(k) = kind {
        app.popup = open(k, true);
    }
}

// ------------------------------------------------------------------ films

/// One frame of a film, decoded.
pub(crate) struct Frame {
    pub(crate) img: egui::ColorImage,
    pub(crate) ms: u32,
}

/// A film playing: its frames come from a thread one ahead, held for their time.
struct Film {
    bytes: &'static [u8],
    rx: Receiver<Frame>,
    /// Set while the frame due has not come yet: the thread wakes the window with it.
    waiting: Arc<AtomicBool>,
    tex: Option<egui::TextureHandle>,
    /// Width over height, once known.
    aspect: f32,
    /// When the frame on screen is over.
    until: f64,
    done: bool,
}

/// How many times a film plays before it rests on its last state.
pub(crate) const PLAYS: u32 = 3;

impl Film {
    fn start(bytes: &'static [u8], still: bool, ctx: &egui::Context) -> Film {
        let (tx, rx) = sync_channel(1);
        let waiting = Arc::new(AtomicBool::new(true));
        let (w, c) = (waiting.clone(), ctx.clone());
        std::thread::spawn(move || play(bytes, still, tx, w, c));
        Film { bytes, rx, waiting, tex: None, aspect: 1.6, until: 0.0, done: false }
    }

    /// Show the next frame when the one on screen is over.
    fn pump(&mut self, ctx: &egui::Context) {
        if self.done {
            return;
        }
        let now = ctx.input(|i| i.time);
        if self.tex.is_some() && now < self.until {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(self.until - now));
            return;
        }
        let f = match self.rx.try_recv() {
            Ok(f) => f,
            Err(TryRecvError::Empty) => {
                // Asked for before looking again: a frame landing in between still wakes us.
                self.waiting.store(true, Ordering::Release);
                match self.rx.try_recv() {
                    Ok(f) => f,
                    Err(TryRecvError::Empty) => return,
                    Err(TryRecvError::Disconnected) => {
                        self.done = true;
                        return;
                    }
                }
            }
            Err(TryRecvError::Disconnected) => {
                self.done = true;
                return;
            }
        };
        self.aspect = f.img.size[0] as f32 / f.img.size[1].max(1) as f32;
        let opts = egui::TextureOptions { mipmap_mode: Some(egui::TextureFilter::Linear), ..egui::TextureOptions::LINEAR };
        match &mut self.tex {
            Some(t) => t.set(f.img, opts),
            None => self.tex = Some(ctx.load_texture("tour film", f.img, opts)),
        }
        self.until = now + f.ms as f64 / 1000.0;
        if f.ms > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(f.ms as u64));
        }
    }
}

/// Decode `bytes` frame by frame, `PLAYS` times over (only the last frame when `still`),
/// handing each over when the one before has been taken.
pub(crate) fn play(bytes: &'static [u8], still: bool, tx: SyncSender<Frame>, waiting: Arc<AtomicBool>, ctx: egui::Context) {
    let Ok(mut d) = image_webp::WebPDecoder::new(std::io::Cursor::new(bytes)) else { return };
    let (w, h) = d.dimensions();
    let (w, h) = (w as usize, h as usize);
    let alpha = d.has_alpha();
    let Some(len) = d.output_buffer_size() else { return };
    let mut buf = vec![0u8; len];
    let n = if d.is_animated() { d.num_frames().max(1) } else { 1 };
    for round in 0..PLAYS {
        for i in 0..n {
            let ms = if d.is_animated() {
                match d.read_frame(&mut buf) {
                    Ok(ms) => ms,
                    Err(_) => return,
                }
            } else {
                if d.read_image(&mut buf).is_err() {
                    return;
                }
                0
            };
            if still && i + 1 < n {
                continue;
            }
            let img = if alpha { egui::ColorImage::from_rgba_unmultiplied([w, h], &buf) } else { egui::ColorImage::from_rgb([w, h], &buf) };
            // The last state of the last play stays: no time of its own.
            let last = still || (round + 1 == PLAYS && i + 1 == n);
            if tx.send(Frame { img, ms: if last { 0 } else { ms } }).is_err() {
                return;
            }
            if waiting.swap(false, Ordering::AcqRel) {
                ctx.request_repaint();
            }
        }
        if still || n == 1 {
            return;
        }
        d.reset_animation();
    }
}

// ------------------------------------------------------------------ drawing

/// The themes offered on arrival: dark and light ones people know.
const THEMES: [ThemeName; 6] = [ThemeName::Midnight, ThemeName::Graphite, ThemeName::TokyoNight, ThemeName::Mocha, ThemeName::Paper, ThemeName::Latte];

/// Closing: the first time by itself, a word on where to find it again.
fn closed(app: &mut App, st: &State) -> bool {
    if st.auto {
        app.toast(match st.kind {
            Kind::Welcome => "The tour is in the command palette: Welcome Tour",
            Kind::News => "This is in the command palette: What's New in Maia",
        });
    }
    false
}

/// The pages as dots along the foot's left end; a click goes to one. Returns their width.
fn dots(ui: &mut Ui, foot: Rect, n: usize, page: usize, pal: &Palette, go: &mut Option<usize>) -> f32 {
    let step = 14.0;
    for i in 0..n {
        let c = pos2(foot.left() + 8.0 + i as f32 * step, foot.center().y);
        let r = Rect::from_center_size(c, Vec2::splat(step));
        let resp = ui.interact(r, Id::new(("tour dot", i)), Sense::click());
        crate::ui::note_control(ui, r, "tour dot");
        let col = if i == page { pal.accent } else if resp.hovered() { pal.text_dim } else { pal.border.lerp_to_gamma(pal.text_dim, 0.35) };
        ui.painter().circle_filled(c, if i == page { 4.0 } else { 3.0 }, col);
        if resp.clicked() {
            *go = Some(i);
        }
    }
    n as f32 * step
}

/// The commands that reach a page, as key chips with what they do; a command with no
/// keys is a chip of its name, with the palette's magnifier, as the palette finds it.
fn keys_row(ui: &mut Ui, app: &App, keys: &[Cmd], pal: &Palette) {
    if keys.is_empty() {
        return;
    }
    let fs = ui.style().text_styles.get(&egui::TextStyle::Body).map_or(13.0, |f| f.size);
    let chip_fs = (fs - 2.0).max(10.5);
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        for &c in keys {
            let what = c.info().title.trim_end_matches('…');
            let key = app.keymap.label(c);
            let g = ui.painter().layout_no_wrap(key.clone().unwrap_or_else(|| what.to_string()), FontId::proportional(chip_fs), pal.text_dim);
            let icon = if key.is_none() { (chip_fs * 0.85).round() } else { 0.0 };
            let lead = if icon > 0.0 { icon + 5.0 } else { 0.0 };
            let (r, _) = ui.allocate_exact_size(g.size() + vec2(16.0 + lead, 8.0), Sense::hover());
            ui.painter().rect(r, CornerRadius::same(5), pal.bg, Stroke::new(1.0, pal.border), StrokeKind::Inside);
            if icon > 0.0 {
                crate::theme::paint_symbol(ui.painter(), Rect::from_center_size(pos2(r.left() + 8.0 + icon / 2.0, r.center().y), Vec2::splat(icon)), "search", pal.text_dim);
            }
            ui.painter().galley(pos2(r.left() + 8.0 + lead, r.center().y - g.size().y / 2.0), g, pal.text_dim);
            crate::ui::note_control(ui, r, "key hint");
            if key.is_some() {
                ui.label(egui::RichText::new(what).size(fs - 1.0).color(pal.text_dim));
            }
            ui.add_space(10.0);
        }
    });
}

// ------------------------------------------------------------------ the whole window

/// The welcome or What's New over the whole window: a card with the page's words on one
/// side and what it is about on the other (the dither and the icon, the choices, a film,
/// the rest of a version's additions), the pages as dots below it. Esc, Skip or the last
/// page's button closes it.
pub fn draw(app: &mut App, ctx: &egui::Context, st: &mut State) -> bool {
    let pal = app.pal;
    let pages = st.pages();
    let n = pages.len();
    st.page = st.page.min(n - 1);
    let typing = ctx.memory(|m| m.focused()).is_some_and(|f| egui::TextEdit::load_state(ctx, f).is_some());
    let (esc, left, right, enter) = ctx.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::Escape),
            !typing && i.consume_key(Modifiers::NONE, Key::ArrowLeft),
            !typing && i.consume_key(Modifiers::NONE, Key::ArrowRight),
            !typing && i.consume_key(Modifiers::NONE, Key::Enter),
        )
    });
    if esc {
        return closed(app, st);
    }
    let mut go: Option<usize> = None;
    let mut close = false;
    if left && st.page > 0 {
        go = Some(st.page - 1);
    }
    if right || enter {
        if st.page + 1 < n {
            go = Some(st.page + 1);
        } else if enter {
            close = true;
        }
    }
    let page = &pages[st.page];
    let now = ctx.input(|i| i.time);
    let anim = app.cfg.animations;
    let opened = *st.opened.get_or_insert(now);
    let turned = *st.turned.get_or_insert(now);
    // The window fills with it over a quarter second; each page fades in after.
    let ease = |t: f32| 1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3);
    let appear = if anim { ease(((now - opened) / 0.28) as f32) } else { 1.0 };
    let fade = if anim { ease(((now - turned) / 0.22) as f32) } else { 1.0 };
    if appear < 1.0 || fade < 1.0 {
        ctx.request_repaint();
    }
    // What the page shows besides its words: started with it, stopped when it goes.
    if let Body::Film(bytes) = page.body {
        if st.film.as_ref().is_none_or(|f| !std::ptr::eq(f.bytes, bytes)) {
            st.film = Some(Film::start(bytes, !anim, ctx));
        }
    } else {
        st.film = None;
    }
    if let Some(f) = &mut st.film {
        f.pump(ctx);
    }
    if st.kind == Kind::News && st.glow.is_none() {
        st.glow = Some(crate::glow::Glow::new(ctx));
    }
    if page.body == Body::Hero {
        if st.dither.is_none() {
            st.dither = Some(crate::dither::Dither::new(ctx));
        }
        if st.icon.is_none() && st.icon_rx.is_none() {
            let (tx, rx) = sync_channel(1);
            let c = ctx.clone();
            let _ = std::thread::Builder::new().name("welcome icon".into()).spawn(move || {
                if let Some(i) = crate::imgdec::decode_fit(crate::install::ICON_PNG, 256) {
                    let _ = tx.send(egui::ColorImage::from_rgba_unmultiplied([i.w as usize, i.h as usize], &i.px));
                    c.request_repaint();
                }
            });
            st.icon_rx = Some(rx);
        }
    } else {
        st.dither = None;
    }
    if let Some(img) = st.icon_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
        st.icon_rx = None;
        st.icon = Some(ctx.load_texture("welcome icon", img, egui::TextureOptions::LINEAR));
    }
    let screen = ctx.content_rect();
    let fs = app.cfg.font_size;
    pp::note_layer(Id::new("welcome"));
    let mut replay = false;
    egui::Area::new(Id::new("welcome")).order(egui::Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
        let (scr, _) = ui.allocate_exact_size(screen.size(), Sense::click());
        ui.multiply_opacity(appear);
        let p = ui.painter().clone();
        p.rect_filled(scr, CornerRadius::ZERO, pal.bg);
        // The strip along the top moves the window, as the title bar it covers does.
        let strip = Rect::from_min_size(scr.min, vec2(scr.width(), 44.0));
        let drag = ui.interact(strip, Id::new("welcome drag"), Sense::click_and_drag());
        if drag.drag_started() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        // Skip, top right, on every page but the last.
        if st.page + 1 < n {
            let sr = Rect::from_min_size(pos2(scr.right() - 92.0, scr.top() + 10.0), vec2(80.0, 26.0));
            let mut sui = ui.new_child(egui::UiBuilder::new().max_rect(sr).layout(egui::Layout::right_to_left(egui::Align::Center)));
            let b = pp::plain_button(&mut sui, "Skip", &pal);
            crate::ui::note_control(&sui, b.rect, "welcome skip");
            let tip = match st.kind {
                Kind::Welcome => "Close the welcome (Esc)",
                Kind::News => "Close What's New (Esc)",
            };
            if b.on_hover_text(tip).clicked() {
                close = true;
            }
        }
        // The card, as big as the window allows up to a comfortable reading size.
        let dots_h = 40.0;
        let cw = (scr.width() - 48.0).min(1120.0).max(280.0).round();
        let ch = (scr.height() - 56.0 - dots_h - 16.0).min(700.0).max(240.0).round();
        let card = Rect::from_min_size(pos2((scr.center().x - cw / 2.0).round(), (scr.top() + 52.0 + ((scr.height() - 52.0 - dots_h - ch) / 2.0 - 8.0).max(0.0)).round()), vec2(cw, ch));
        let rad = if app.cfg.round_corners { 16 } else { 0 };
        if let Some(g) = &mut st.glow {
            // Clear of the card, the page dots under it and Skip, so they read on it.
            let dw = n as f32 * 14.0;
            let dots = Rect::from_min_size(pos2((scr.center().x - dw / 2.0).round(), card.bottom() + 12.0), vec2(dw, 24.0)).expand2(vec2(18.0, 4.0));
            let skip = Rect::from_min_size(pos2(scr.right() - 92.0, scr.top() + 10.0), vec2(80.0, 26.0)).expand(8.0);
            g.paint(ui, scr, &[card.expand(2.0), dots, skip], st.page, n, pal.accent, pal.bg, anim);
        }
        p.rect(card, CornerRadius::same(rad), pal.panel, Stroke::new(1.0, pal.border), StrokeKind::Inside);
        // Side by side, or the picture over the words in a narrow window.
        let stacked = cw < 760.0;
        let pad = (24.0 + (cw - 760.0).max(0.0) * 0.04).round().min(40.0);
        let foot_h = 34.0;
        let (words_r, visual) = if stacked {
            // The choices and the list need the room; a film or the opening picture less.
            // The words keep what they took last frame, whole, with the way on below.
            let choose = matches!(page.body, Body::Look | Body::Size | Body::Clicks | Body::Show | Body::Extras | Body::List);
            let need = st.block_h + 12.0 + foot_h + 2.0 * pad;
            let vh = (ch - need).min(ch * if choose { 0.62 } else { 0.44 }).max(ch * 0.3).round();
            (Rect::from_min_max(pos2(card.left() + pad, card.top() + vh + pad), pos2(card.right() - pad, card.bottom() - pad)), Rect::from_min_size(card.min + vec2(10.0, 10.0), vec2(cw - 20.0, vh - 10.0)))
        } else {
            // The words keep the same margin from the picture as from the card's edge.
            let lw = (cw * 0.42).clamp(340.0, 480.0).round();
            (Rect::from_min_max(card.min + vec2(pad, pad), pos2(card.left() + lw - pad, card.bottom() - pad)), Rect::from_min_max(pos2(card.left() + lw, card.top() + 10.0), card.max - vec2(10.0, 10.0)))
        };
        let vrad = if app.cfg.round_corners { 12 } else { 0 };
        p.rect_filled(visual, CornerRadius::same(vrad), pal.bg);
        // The page's words, the way on at their foot.
        let shift = (1.0 - fade) * 10.0;
        let foot = Rect::from_min_max(pos2(words_r.left(), words_r.bottom() - foot_h), words_r.max);
        let hero = page.body == Body::Hero;
        // The words stand in the middle of their column, level with the picture beside
        // them (a little above, where the eye expects the middle); the opening page's
        // button stands under its words and is centred with them, the others' keep to
        // the foot.
        let hero_gap = 28.0;
        let room = if hero { words_r.height() } else { foot.top() - 12.0 - words_r.top() };
        let top = if stacked { 0.0 } else { ((room - st.block_h) * 0.46).max(0.0).round() };
        let hero_foot: f32;
        let words_h: f32;
        {
            let body = Rect::from_min_max(words_r.min + vec2(shift, 0.0), pos2(words_r.right() + shift, foot.top() - 12.0));
            let mut wui = ui.new_child(egui::UiBuilder::new().max_rect(body).layout(egui::Layout::top_down(egui::Align::Min)));
            wui.set_clip_rect(body.expand(2.0).intersect(ui.clip_rect()));
            wui.multiply_opacity(fade);
            wui.add_space(top);
            wui.spacing_mut().item_spacing.y = 0.0;
            wui.label(egui::RichText::new(format!("{} of {n}", st.page + 1)).size((fs - 2.0).max(10.5)).color(pal.text_dim));
            wui.add_space(10.0);
            let title = (if hero { fs * 2.3 } else { fs * 1.8 }).round();
            wui.add(egui::Label::new(egui::RichText::new(page.title).strong().size(title).color(pal.text).line_height(Some((title * 1.2).round()))).wrap());
            wui.add_space(if hero { 16.0 } else { 12.0 });
            // A comfortable measure: a line much longer than this is hard to follow back.
            let text = fs + 0.5;
            let measure = wui.available_width().min((text * 34.0).round());
            wui.allocate_ui(vec2(measure, 0.0), |ui| {
                ui.add(egui::Label::new(egui::RichText::new(page.text).size(text).color(pal.text_dim).line_height(Some((text * 1.5).round()))).wrap());
            });
            if !page.keys.is_empty() {
                wui.add_space(10.0);
                keys_row(&mut wui, app, page.keys, &pal);
            }
            hero_foot = (wui.min_rect().bottom() + hero_gap).round();
            words_h = (wui.min_rect().bottom() - words_r.top() - top).round();
        }
        // The opening page's button stands under its words; the others' at the foot.
        let foot = if hero { Rect::from_min_size(pos2(foot.left(), hero_foot.min(foot.top())), foot.size()) } else { foot };
        let mut fui = ui.new_child(egui::UiBuilder::new().max_rect(foot).layout(egui::Layout::left_to_right(egui::Align::Center)));
        fui.spacing_mut().item_spacing.x = 8.0;
        let last = st.page + 1 == n;
        let label = match (st.kind, st.page, last) {
            (Kind::Welcome, 0, _) => "Get started",
            (Kind::Welcome, _, true) => "Start using Maia",
            (Kind::News, 0, _) => "See what's new",
            (Kind::News, _, true) => "Done",
            _ => "Continue",
        };
        // Back first, the way on after it.
        if st.page > 0 {
            let b = pp::plain_button(&mut fui, "Back", &pal);
            crate::ui::note_control(&fui, b.rect, "welcome back");
            if b.clicked() {
                go = Some(st.page - 1);
            }
        }
        let b = pp::primary_button(&mut fui, label, true, &pal);
        crate::ui::note_control(&fui, b.rect, "welcome next");
        {
            let h = if hero && !stacked { (b.rect.bottom() - words_r.top() - top).round() } else { words_h };
            if (h - st.block_h).abs() > 0.5 {
                st.block_h = h;
                ctx.request_repaint();
            }
        }
        if b.clicked() {
            if last {
                close = true;
            } else {
                go = Some(st.page + 1);
            }
        }
        // What the page is about, in the other half.
        let mut vui = ui.new_child(egui::UiBuilder::new().max_rect(visual).layout(egui::Layout::top_down(egui::Align::Min)));
        vui.set_clip_rect(visual.intersect(ui.clip_rect()));
        vui.multiply_opacity(fade);
        match page.body {
            Body::Hero => {
                if let Some(d) = &mut st.dither {
                    d.paint(&vui, visual, pal.accent, pal.bg, anim, vrad, fade);
                }
                // The icon on a tile of the card's colour, in the middle, standing a little
                // off the dither.
                let side = (visual.width().min(visual.height()) * 0.24).clamp(72.0, 132.0).round();
                let tile = Rect::from_center_size(visual.center(), Vec2::splat(side));
                let trad = CornerRadius::same((side * 0.24) as u8);
                for k in 1..=4 {
                    let s = k as f32 * 3.0;
                    vui.painter().rect_filled(tile.translate(vec2(0.0, s * 0.6)).expand(s), CornerRadius::same((side * 0.24 + s) as u8), egui::Color32::from_black_alpha((14.0 / k as f32) as u8));
                }
                vui.painter().rect(tile, trad, pal.panel, Stroke::new(1.0, pal.border), StrokeKind::Inside);
                if let Some(t) = &st.icon {
                    let ir = tile.shrink((side * 0.14).round());
                    egui::Image::new((t.id(), ir.size())).paint_at(&vui, ir);
                }
                // What's New says which version under the icon, where there is room.
                let g = vui.painter().layout_no_wrap(NEWS_VERSION.to_string(), FontId::proportional(fs), pal.text);
                let size = g.size() + vec2(24.0, 10.0);
                let chip = Rect::from_min_size(pos2((tile.center().x - size.x / 2.0).round(), (tile.bottom() + 22.0).round()), size);
                if st.kind == Kind::News && chip.bottom() < visual.bottom() - 10.0 {
                    vui.painter().rect(chip, CornerRadius::same((chip.height() / 2.0) as u8), pal.panel, Stroke::new(1.0, pal.border), StrokeKind::Inside);
                    vui.painter().galley(chip.center() - g.size() / 2.0, g, pal.text);
                }
            }
            Body::Look | Body::Size | Body::Clicks | Body::Show | Body::Extras => {
                let pad = (visual.width() * 0.045).clamp(18.0, 36.0).round();
                let inner = visual.shrink(pad);
                let changed = match page.body {
                    Body::Look => step_look(&mut vui, app, inner),
                    Body::Size => step_size(&mut vui, app, inner),
                    Body::Clicks => step_clicks(&mut vui, app, inner),
                    Body::Show => step_show(&mut vui, app, inner),
                    _ => step_extras(&mut vui, app, inner),
                };
                if changed {
                    let ctx = vui.ctx().clone();
                    pp::option_changed(app, &ctx);
                }
            }
            Body::Film(_) => {
                let inner = visual.shrink(28.0);
                let aspect = st.film.as_ref().map_or(1.6, |f| f.aspect);
                let fw = inner.width().min(inner.height() * aspect).floor();
                let fr = Rect::from_center_size(inner.center(), vec2(fw, (fw / aspect).round()));
                let resp = vui.interact(fr, Id::new("welcome film"), Sense::click());
                crate::ui::note_control(&vui, fr, "tour film");
                let round = CornerRadius::same(8);
                match st.film.as_ref().and_then(|f| f.tex.as_ref()) {
                    Some(t) => {
                        egui::Image::new((t.id(), fr.size())).corner_radius(round).paint_at(&vui, fr);
                    }
                    None => {
                        vui.painter().rect_filled(fr, round, pal.card());
                    }
                }
                vui.painter().rect_stroke(fr, round, Stroke::new(1.0, pal.card_line()), StrokeKind::Outside);
                replay = resp.clicked() && st.film_done();
            }
            Body::List => also(&mut vui, app, visual),
        }
        // The pages as dots, centred under the card.
        let dw = n as f32 * 14.0;
        let dr = Rect::from_min_size(pos2((scr.center().x - dw / 2.0).round(), card.bottom() + 12.0), vec2(dw, 24.0));
        dots(ui, dr, n, st.page, &pal, &mut go);
    });
    if replay {
        st.film = None;
    }
    if let Some(p) = go.filter(|p| *p != st.page && *p < n) {
        st.page = p;
        st.turned = Some(now);
        st.film = None;
        ctx.request_repaint();
    }
    if close {
        return closed(app, st);
    }
    true
}

/// The rest of what the version added, in its groups: a card for each, its icon beside
/// its name and a line on it, two to a row where there is room for that.
fn also(ui: &mut Ui, app: &App, visual: Rect) {
    let pal = app.pal;
    let fs = app.cfg.font_size;
    let pad = (visual.width() * 0.045).clamp(18.0, 36.0).round();
    let area = visual.shrink(pad);
    let mut sui = ui.new_child(egui::UiBuilder::new().max_rect(area).layout(egui::Layout::top_down(egui::Align::Min)));
    egui::ScrollArea::vertical().id_salt("news also").max_height(area.height()).auto_shrink([false, false]).show(&mut sui, |ui| {
        let gap = 12.0;
        // Room kept at the right for the scroll bar.
        let w = (ui.available_width() - 12.0).floor();
        let cols = if w >= 520.0 { 2 } else { 1 };
        let cw = ((w - gap * (cols as f32 - 1.0)) / cols as f32).floor();
        let icon = 36.0;
        let text_w = cw - 14.0 - icon - 12.0 - 14.0;
        ui.spacing_mut().item_spacing.y = gap;
        for (gi, (group, items)) in ALSO.iter().enumerate() {
            if gi > 0 {
                ui.add_space(8.0);
            }
            ui.label(egui::RichText::new(*group).size(fs - 1.5).color(pal.text_dim));
            for row in items.chunks(cols) {
                let lay: Vec<_> = row
                    .iter()
                    .map(|(_, name, line)| {
                        let n = ui.painter().layout(name.to_string(), FontId::proportional(fs), pal.text, text_w);
                        let l = ui.painter().layout(line.to_string(), FontId::proportional(fs - 1.5), pal.text_dim, text_w);
                        (n, l)
                    })
                    .collect();
                let h = lay.iter().map(|(n, l)| n.size().y + 3.0 + l.size().y).fold(icon, f32::max) + 28.0;
                let (r, _) = ui.allocate_exact_size(vec2(w, h.ceil()), Sense::hover());
                for (k, ((sym, _, _), (n, l))) in row.iter().zip(lay).enumerate() {
                    let c = Rect::from_min_size(pos2(r.left() + k as f32 * (cw + gap), r.top()), vec2(cw, r.height()));
                    ui.painter().rect(c, CornerRadius::same(12), pal.panel, Stroke::new(1.0, pal.border), StrokeKind::Inside);
                    let ir = Rect::from_min_size(c.min + vec2(14.0, 14.0), Vec2::splat(icon));
                    ui.painter().rect_filled(ir, CornerRadius::same(9), pal.accent.gamma_multiply(0.16));
                    crate::theme::paint_symbol(ui.painter(), ir.shrink(9.0), sym, pal.accent);
                    let at = pos2(ir.right() + 12.0, c.top() + 14.0);
                    let nh = n.size().y;
                    ui.painter().galley(at, n, pal.text);
                    ui.painter().galley(at + vec2(0.0, nh + 3.0), l, pal.text_dim);
                }
            }
        }
    });
}

// ------------------------------------------------------------------ the first choices

/// Text on the accent: dark or light, whichever reads.
fn on_accent(pal: &Palette) -> egui::Color32 {
    let a = pal.accent;
    if 0.2126 * a.r() as f32 + 0.7152 * a.g() as f32 + 0.0722 * a.b() as f32 > 150.0 { egui::Color32::from_gray(20) } else { egui::Color32::WHITE }
}

/// A card to choose in `r`: it lifts under the pointer, and when chosen it is ringed in
/// the accent with a check in its corner, both easing in.
fn card(ui: &mut Ui, r: Rect, on: bool, id: Id, what: &'static str, pal: &Palette) -> egui::Response {
    let resp = ui.interact(r, id, Sense::click());
    // A row around a switch is the switch's to note: the two are one control.
    if !what.is_empty() {
        crate::ui::note_control(ui, r, what);
    }
    let t = ui.ctx().animate_bool_with_time(id.with("on"), on, 0.14);
    let h = ui.ctx().animate_bool_with_time(id.with("hover"), resp.hovered(), 0.1);
    let rad = CornerRadius::same(12);
    ui.painter().rect_filled(r, rad, pal.panel.lerp_to_gamma(pal.hover, h * 0.55));
    let line = pal.border.lerp_to_gamma(pal.text_dim, h * 0.45).lerp_to_gamma(pal.accent, t);
    ui.painter().rect_stroke(r, rad, Stroke::new(1.0 + t, line), StrokeKind::Inside);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// The check in a chosen card's corner, drawn over what the card shows; it grows in as
/// the card is chosen.
fn mark(ui: &Ui, r: Rect, id: Id, pal: &Palette) {
    let t = ui.ctx().animate_bool_with_time(id.with("on"), true, 0.14);
    let c = pos2(r.right() - 18.0, r.top() + 18.0);
    ui.painter().circle_filled(c, 10.5 * t, pal.panel);
    ui.painter().circle_filled(c, 8.5 * t, pal.accent);
    let s = Stroke::new(1.6, on_accent(pal).gamma_multiply(t));
    ui.painter().add(egui::Shape::line(vec![c + vec2(-3.6, 0.2), c + vec2(-1.1, 2.7), c + vec2(3.8, -2.4)], s));
}

/// `area` cut to `h` tall, in its middle: a step's cards stand in the middle of the
/// panel, not at its top.
fn middle(area: Rect, h: f32) -> Rect {
    let h = h.min(area.height());
    Rect::from_min_size(pos2(area.left(), (area.top() + (area.height() - h) / 2.0).round()), vec2(area.width(), h))
}

/// A card's name and a dim line under it, from `at`.
fn card_words(ui: &Ui, at: egui::Pos2, w: f32, name: &str, detail: &str, pal: &Palette) {
    let fs = ui.style().text_styles.get(&egui::TextStyle::Body).map_or(13.0, |f| f.size);
    let g = crate::listing::elided(ui, name, FontId::proportional(fs), w, pal.text);
    let gh = g.size().y;
    ui.painter().galley(at, g, pal.text);
    if !detail.is_empty() {
        let d = crate::listing::elided(ui, detail, FontId::proportional(fs - 1.5), w, pal.text_dim);
        ui.painter().galley(at + vec2(0.0, gh + 2.0), d, pal.text_dim);
    }
}

/// How tall `card_words` is.
fn words_h(ui: &Ui, detail: bool) -> f32 {
    let fs = ui.style().text_styles.get(&egui::TextStyle::Body).map_or(13.0, |f| f.size);
    (fs * 1.3 + if detail { fs * 1.2 + 2.0 } else { 0.0 }).round()
}

/// A wide row card with a switch at its end: its name and what it does. The whole row
/// flips it. Returns whether it was flipped.
fn switch_row(ui: &mut Ui, r: Rect, on: &mut bool, name: &str, detail: &str, what: &'static str, pal: &Palette) -> bool {
    let resp = card(ui, r, false, Id::new(("welcome row", what)), "", pal);
    let sw = Rect::from_center_size(pos2(r.right() - 16.0 - 16.0, r.center().y), vec2(32.0, 16.0));
    let mut sui = ui.new_child(egui::UiBuilder::new().max_rect(sw).layout(egui::Layout::left_to_right(egui::Align::Center)));
    let flipped = pp::switch(&mut sui, on, pal);
    let wh = words_h(ui, !detail.is_empty());
    card_words(ui, pos2(r.left() + 16.0, r.center().y - wh / 2.0), r.width() - 90.0, name, detail, pal);
    if !flipped && resp.clicked() {
        *on = !*on;
        return true;
    }
    flipped
}

/// Cards in a row across `r`, or down it when too narrow for that; each `h` tall.
fn layout(r: Rect, n: usize, h: f32, min_w: f32) -> Vec<Rect> {
    let gap = 14.0;
    let across = (r.width() - gap * (n as f32 - 1.0)) / n as f32 >= min_w;
    (0..n)
        .map(|i| {
            if across {
                let w = ((r.width() - gap * (n as f32 - 1.0)) / n as f32).floor();
                Rect::from_min_size(pos2(r.left() + i as f32 * (w + gap), r.top()), vec2(w, h))
            } else {
                let hh = ((r.height() - gap * (n as f32 - 1.0)) / n as f32).min(h).floor();
                Rect::from_min_size(pos2(r.left(), r.top() + i as f32 * (hh + gap)), vec2(r.width(), hh))
            }
        })
        .collect()
}

/// The look: the desktop's light and dark followed or not, and the themes as small
/// windows; pointing at one shows the whole window in it.
fn step_look(ui: &mut Ui, app: &mut App, area: Rect) -> bool {
    let pal = app.pal;
    let mut changed = false;
    // The cards' size first, then the whole stood in the panel's middle.
    let cols = if area.width() >= 540.0 { 3 } else { 2 };
    let rows = THEMES.len().div_ceil(cols);
    let gap = 14.0;
    let cw = ((area.width() - gap * (cols as f32 - 1.0)) / cols as f32).floor();
    let wh = words_h(ui, false);
    let ch = (cw * 0.62 + wh + 26.0).min(((area.height() - 78.0 - gap * (rows as f32 - 1.0)) / rows as f32).floor()).max(wh + 22.0);
    let area = middle(area, 78.0 + rows as f32 * ch + (rows as f32 - 1.0) * gap);
    let row = Rect::from_min_size(area.min, vec2(area.width(), 60.0));
    let mut follow = app.cfg.follow_system_theme;
    if switch_row(ui, row, &mut follow, "Follow the desktop", "Light or dark as the desktop is, now and when it changes", "welcome follow", &pal) {
        app.cfg.follow_system_theme = follow;
        changed = true;
    }
    let grid = Rect::from_min_max(pos2(area.left(), row.bottom() + 18.0), area.max);
    for (i, t) in THEMES.iter().enumerate() {
        let r = Rect::from_min_size(pos2(grid.left() + (i % cols) as f32 * (cw + gap), grid.top() + (i / cols) as f32 * (ch + gap)), vec2(cw, ch));
        let on = !app.cfg.follow_system_theme && app.cfg.theme == *t;
        let resp = card(ui, r, on, Id::new(("welcome theme", i)), "tour theme", &pal);
        // Too short for a picture of the window that reads, the card is its name alone.
        let pic = Rect::from_min_max(r.min + vec2(10.0, 10.0), pos2(r.right() - 10.0, r.bottom() - wh - 16.0));
        if pic.height() >= 36.0 {
            pp::theme_card(ui.painter(), pic, crate::theme::palette(*t), 7);
        }
        card_words(ui, pos2(r.left() + 12.0, r.bottom() - wh - 9.0), r.width() - 24.0, t.label(), "", &pal);
        if on {
            mark(ui, r, Id::new(("welcome theme", i)), &pal);
        }
        if resp.hovered() {
            app.preview_theme = Some(*t);
        }
        if resp.clicked() && !on {
            app.cfg.theme = *t;
            // A theme picked by hand is the one wanted, whatever the desktop's.
            app.cfg.follow_system_theme = false;
            changed = true;
        }
    }
    changed
}

/// Text size and spacing, together: (name, size, spacing).
const SIZES: [(&str, f32, f32); 3] = [("Compact", 12.0, 1.0), ("Comfortable", 14.5, 1.3), ("Roomy", 16.5, 1.6)];

/// The text's size, as a few rows of a folder at each; and animations.
fn step_size(ui: &mut Ui, app: &mut App, area: Rect) -> bool {
    let pal = app.pal;
    let mut changed = false;
    let motion_h = 60.0;
    let wh = words_h(ui, true);
    let ch = (area.height() - motion_h - 18.0).min(320.0);
    let area = middle(area, ch + 18.0 + motion_h);
    let cards = Rect::from_min_max(area.min, pos2(area.right(), area.bottom() - motion_h - 18.0));
    for (i, r) in layout(cards, SIZES.len(), ch, 170.0).into_iter().enumerate() {
        let (name, fs, sp) = SIZES[i];
        let on = (app.cfg.font_size - fs).abs() < 0.01 && (app.cfg.spacing - sp).abs() < 0.01;
        let resp = card(ui, r, on, Id::new(("welcome size", i)), "welcome size", &pal);
        // A folder's first rows, at this size, as the list draws them.
        let pic = Rect::from_min_max(r.min + vec2(10.0, 10.0), pos2(r.right() - 10.0, r.bottom() - wh - 18.0));
        ui.painter().rect_filled(pic, CornerRadius::same(7), pal.bg);
        let p = ui.painter().with_clip_rect(pic.intersect(ui.clip_rect()));
        let row_h = (fs * 1.35 + 8.0 * sp).round();
        let names = ["Documents", "Photos", "Projects", "notes.md", "budget.ods", "trip.mkv"];
        for (k, n) in names.iter().enumerate() {
            let y = pic.top() + 6.0 + k as f32 * row_h;
            if y > pic.bottom() {
                break;
            }
            let rr = Rect::from_min_size(pos2(pic.left() + 4.0, y), vec2(pic.width() - 8.0, row_h));
            if k == 1 {
                p.rect_filled(rr, CornerRadius::same(5), pal.selected);
            }
            let side = (fs * 1.15).round();
            let ic = Rect::from_center_size(pos2(rr.left() + 8.0 + side / 2.0, rr.center().y), Vec2::splat(side));
            let cat = if k < 3 { crate::theme::FileCat::Folder } else { crate::theme::categorize(n.rsplit('.').next().unwrap_or(""), false, false) };
            crate::theme::paint_icon(&p, ic, cat, "", false);
            let col = if k == 1 { pal.on_sel() } else { pal.text };
            p.text(pos2(ic.right() + 8.0, rr.center().y), Align2::LEFT_CENTER, *n, FontId::proportional(fs), col);
        }
        card_words(ui, pos2(r.left() + 12.0, r.bottom() - wh - 10.0), r.width() - 24.0, name, &format!("Text {fs}, spacing {sp}"), &pal);
        if on {
            mark(ui, r, Id::new(("welcome size", i)), &pal);
        }
        if resp.clicked() && !on {
            app.cfg.font_size = fs;
            app.cfg.spacing = sp;
            changed = true;
        }
    }
    let row = Rect::from_min_size(pos2(area.left(), area.bottom() - motion_h), vec2(area.width(), motion_h));
    let mut anim = app.cfg.animations;
    if switch_row(ui, row, &mut anim, "Animations", "Pages fade, panels slide open, lists ease to their place", "welcome motion", &pal) {
        app.cfg.animations = anim;
        changed = true;
    }
    changed
}

/// A pointer's arrow with its tip at `tip`, `s` points tall.
fn pointer(p: &egui::Painter, tip: egui::Pos2, s: f32, fill: egui::Color32, line: egui::Color32) {
    let k = s / 18.0;
    let pts: Vec<egui::Pos2> = [(0.0, 0.0), (0.0, 15.0), (4.0, 11.5), (7.0, 18.0), (9.5, 17.0), (6.5, 10.5), (11.5, 10.5)].iter().map(|(x, y)| tip + vec2(x * k, y * k)).collect();
    p.add(egui::Shape::convex_polygon(vec![pts[0], pts[1], pts[2], pts[5], pts[6]], fill, Stroke::NONE));
    p.add(egui::Shape::convex_polygon(vec![pts[2], pts[3], pts[4], pts[5]], fill, Stroke::NONE));
    p.add(egui::Shape::closed_line(pts, Stroke::new(1.2, line)));
}

/// Opening with two clicks or one, each drawn as a row being clicked.
fn step_clicks(ui: &mut Ui, app: &mut App, area: Rect) -> bool {
    let pal = app.pal;
    let mut changed = false;
    let single = app.cfg.single_click;
    let extra_h = if single { 60.0 + 18.0 } else { 0.0 };
    let wh = words_h(ui, true);
    let ch = (area.height() - extra_h).min(280.0);
    let area = middle(area, ch + extra_h);
    let cards = Rect::from_min_max(area.min, pos2(area.right(), area.bottom() - extra_h));
    let opts = [(false, "Double click", "Opens a file, as on most desktops"), (true, "Single click", "Opens it at once, as on the web")];
    let now = ui.input(|i| i.time);
    for (i, r) in layout(cards, 2, ch, 200.0).into_iter().enumerate() {
        let (one, name, detail) = opts[i];
        let on = single == one;
        let resp = card(ui, r, on, Id::new(("welcome click", i)), "welcome click", &pal);
        let pic = Rect::from_min_max(r.min + vec2(10.0, 10.0), pos2(r.right() - 10.0, r.bottom() - wh - 18.0));
        let p = ui.painter().with_clip_rect(pic.intersect(ui.clip_rect()));
        p.rect_filled(pic, CornerRadius::same(7), pal.bg);
        // A file's row with the pointer on it, and a ring for each click.
        let fs = 13.0;
        let rr = Rect::from_center_size(pic.center() - vec2(0.0, 6.0), vec2((pic.width() * 0.7).min(220.0), 30.0));
        p.rect_filled(rr, CornerRadius::same(6), if on { pal.selected } else { pal.hover });
        let ic = Rect::from_center_size(pos2(rr.left() + 18.0, rr.center().y), Vec2::splat(16.0));
        crate::theme::paint_icon(&p, ic, crate::theme::FileCat::Folder, "", false);
        p.text(pos2(ic.right() + 8.0, rr.center().y), Align2::LEFT_CENTER, "Holiday photos", FontId::proportional(fs), if on { pal.on_sel() } else { pal.text });
        let tip = pos2(rr.left() + rr.width() * 0.62, rr.center().y + 2.0);
        // The rings swell and fade, one after the other, while the card is chosen.
        let n = if one { 1 } else { 2 };
        for k in 0..n {
            let phase = if on && app.cfg.animations { ((now * 0.8 - k as f64 * 0.18).rem_euclid(1.0)) as f32 } else { 0.45 + k as f32 * 0.25 };
            let rad = 6.0 + phase * 14.0;
            p.circle_stroke(tip, rad, Stroke::new(1.6, pal.accent.gamma_multiply((1.0 - phase) * 0.9)));
        }
        if on && app.cfg.animations {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
        }
        pointer(&p, tip, 20.0, egui::Color32::WHITE, egui::Color32::from_gray(20));
        card_words(ui, pos2(r.left() + 12.0, r.bottom() - wh - 10.0), r.width() - 24.0, name, detail, &pal);
        if on {
            mark(ui, r, Id::new(("welcome click", i)), &pal);
        }
        if resp.clicked() && !on {
            app.cfg.single_click = one;
            changed = true;
        }
    }
    if single {
        let row = Rect::from_min_size(pos2(area.left(), area.bottom() - 60.0), vec2(area.width(), 60.0));
        let mut only = app.cfg.single_click_folders_only;
        if switch_row(ui, row, &mut only, "Folders only", "Files still open with two clicks, so one selects them", "welcome folders only", &pal) {
            app.cfg.single_click_folders_only = only;
            changed = true;
        }
    }
    changed
}

/// Hidden files shown or not, each drawn as a folder's list; and where pictures show
/// what they hold.
fn step_show(ui: &mut Ui, app: &mut App, area: Rect) -> bool {
    let pal = app.pal;
    let mut changed = false;
    let thumbs_h = 92.0;
    let wh = words_h(ui, true);
    let ch = (area.height() - thumbs_h - 18.0).min(280.0);
    let area = middle(area, ch + 18.0 + thumbs_h);
    let cards = Rect::from_min_max(area.min, pos2(area.right(), area.bottom() - thumbs_h - 18.0));
    let opts = [(false, "Leave them out", "The folder as you made it"), (true, "Show them", "Settings and caches among the rest")];
    for (i, r) in layout(cards, 2, ch, 200.0).into_iter().enumerate() {
        let (shown, name, detail) = opts[i];
        let on = app.cfg.show_hidden == shown;
        let resp = card(ui, r, on, Id::new(("welcome hidden", i)), "welcome hidden", &pal);
        let pic = Rect::from_min_max(r.min + vec2(10.0, 10.0), pos2(r.right() - 10.0, r.bottom() - wh - 18.0));
        let p = ui.painter().with_clip_rect(pic.intersect(ui.clip_rect()));
        p.rect_filled(pic, CornerRadius::same(7), pal.bg);
        let rows: Vec<(&str, bool)> = [(".cache", true), (".config", true), ("Documents", false), ("Music", false), (".local", true), ("Pictures", false)].into_iter().filter(|(_, dot)| shown || !dot).collect();
        let row_h = 24.0;
        for (k, (n, dot)) in rows.iter().enumerate() {
            let y = pic.top() + 8.0 + k as f32 * row_h;
            let ic = Rect::from_center_size(pos2(pic.left() + 20.0, y + row_h / 2.0), Vec2::splat(15.0));
            let faint = if *dot { 0.5 } else { 1.0 };
            let mut ip = p.clone();
            ip.multiply_opacity(faint);
            crate::theme::paint_icon(&ip, ic, crate::theme::FileCat::Folder, "", false);
            p.text(pos2(ic.right() + 8.0, y + row_h / 2.0), Align2::LEFT_CENTER, *n, FontId::proportional(13.0), pal.text.gamma_multiply(faint));
        }
        card_words(ui, pos2(r.left() + 12.0, r.bottom() - wh - 10.0), r.width() - 24.0, name, detail, &pal);
        if on {
            mark(ui, r, Id::new(("welcome hidden", i)), &pal);
        }
        if resp.clicked() && !on {
            app.cfg.show_hidden = shown;
            changed = true;
        }
    }
    // Where pictures get their previews: three choices in a row card.
    let row = Rect::from_min_size(pos2(area.left(), area.bottom() - thumbs_h), vec2(area.width(), thumbs_h));
    card(ui, row, false, Id::new("welcome thumbs"), "", &pal);
    card_words(ui, row.min + vec2(16.0, 14.0), row.width() - 32.0, "Picture previews", "Where pictures and films show what they hold", &pal);
    let chips = Rect::from_min_max(pos2(row.left() + 14.0, row.bottom() - 34.0), pos2(row.right() - 14.0, row.bottom() - 10.0));
    let mut cu = ui.new_child(egui::UiBuilder::new().max_rect(chips).layout(egui::Layout::left_to_right(egui::Align::Center)));
    cu.spacing_mut().item_spacing.x = 6.0;
    use crate::config::ThumbMode;
    for (m, label) in [(ThumbMode::LocalDisks, "On this computer's disks"), (ThumbMode::AllDrives, "Everywhere"), (ThumbMode::Never, "Nowhere")] {
        if crate::devtools::chip(&mut cu, label, app.cfg.thumbnails == m, "welcome thumbs choice", "", &pal, false) && app.cfg.thumbnails != m {
            app.cfg.thumbnails = m;
            changed = true;
        }
    }
    changed
}

/// Maia for the folders other apps open, and search by meaning, each a card to switch.
fn step_extras(ui: &mut Ui, app: &mut App, area: Rect) -> bool {
    let pal = app.pal;
    let mut changed = false;
    let h = 96.0;
    let gap = 14.0;
    let area = middle(area, h * 2.0 + gap);
    let fm = Rect::from_min_size(area.min, vec2(area.width(), h));
    let meaning = Rect::from_min_size(pos2(area.left(), fm.bottom() + gap), vec2(area.width(), h));
    let mut default = app.default_fm() == Some(true);
    let icon = |ui: &Ui, r: Rect, sym: &str| {
        let ir = Rect::from_center_size(pos2(r.left() + 36.0, r.center().y), Vec2::splat(40.0));
        ui.painter().rect_filled(ir, CornerRadius::same(10), pal.accent.gamma_multiply(0.16));
        crate::theme::paint_symbol(ui.painter(), ir.shrink(10.0), sym, pal.accent);
    };
    let row = |ui: &mut Ui, r: Rect, on: &mut bool, name: &str, detail: &str, what: &'static str, sym: &str| -> bool {
        let inner = Rect::from_min_max(pos2(r.left() + 56.0, r.top()), r.max);
        let resp = card(ui, r, false, Id::new(("welcome extra", what)), "", &pal);
        icon(ui, r, sym);
        let sw = Rect::from_center_size(pos2(r.right() - 32.0, r.center().y), vec2(32.0, 16.0));
        let mut sui = ui.new_child(egui::UiBuilder::new().max_rect(sw).layout(egui::Layout::left_to_right(egui::Align::Center)));
        let flipped = pp::switch(&mut sui, on, &pal);
        let wh = words_h(ui, true);
        card_words(ui, pos2(inner.left() + 12.0, r.center().y - wh / 2.0), inner.width() - 80.0, name, detail, &pal);
        if !flipped && resp.clicked() {
            *on = !*on;
            return true;
        }
        flipped
    };
    if row(ui, fm, &mut default, "Open folders in Maia", "When another app or the desktop opens one", "welcome default", "folder") {
        app.set_default_fm(default);
    }
    let mut sem = app.cfg.semantic_search;
    if row(ui, meaning, &mut sem, "Search by meaning", "Find files by what they are about, with a local Ollama model", "welcome meaning", "search") {
        app.cfg.semantic_search = sem;
        changed = true;
    }
    changed
}
