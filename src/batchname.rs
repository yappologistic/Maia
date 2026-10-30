//! Batch Rename: new names for several items at once from a pattern, a find and replace
//! and a change of case, every one shown beside the old before anything is renamed.
//! Extensions stay as they are.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use egui::{CornerRadius, FontId, Id, Sense, pos2, vec2};

use crate::app::App;
use crate::popups::{self as pp, Popup};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Case {
    #[default]
    Keep,
    Lower,
    Upper,
    Title,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rules {
    /// The new name: `#` a number (`###` gives 001), `{name}` the old one. Empty keeps
    /// the old names.
    pub name: String,
    pub start: String,
    pub find: String,
    pub replace: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub case: Case,
}

/// A name and its extension: "photo.jpg" is ("photo", ".jpg"); a folder, a dotfile or a
/// name without a dot is all name.
pub fn split(name: &str, is_dir: bool) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if !is_dir && i > 0 && i + 1 < name.len() => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// The new names of `items` (name, is a folder), in their order, or why the rules
/// can't be used (a broken expression).
pub fn names(items: &[(String, bool)], r: &Rules) -> Result<Vec<String>, String> {
    names_with(items, r, &[])
}

/// The tags a pattern can name: {artist}, {title}, {track}, {album}.
pub const TAG_WORDS: [&str; 4] = ["{artist}", "{title}", "{track}", "{album}"];

pub fn uses_tags(pattern: &str) -> bool {
    TAG_WORDS.iter().any(|w| pattern.contains(w))
}

/// `names`, with each item's music tags (by position) for the words that name them.
pub fn names_with(items: &[(String, bool)], r: &Rules, tags: &[Option<crate::musictags::Tags>]) -> Result<Vec<String>, String> {
    let start: u64 = r.start.trim().parse().unwrap_or(1);
    let find = if r.find.is_empty() {
        None
    } else {
        let pat = if r.regex { r.find.clone() } else { regex_lite::escape(&r.find) };
        let pat = if r.case_sensitive { pat } else { format!("(?i){pat}") };
        Some(regex_lite::Regex::new(&pat).map_err(|e| e.to_string().lines().last().unwrap_or("Not an expression").trim().to_string())?)
    };
    Ok(items
        .iter()
        .enumerate()
        .map(|(i, (old, is_dir))| {
            let (stem, ext) = split(old, *is_dir);
            let mut base = if r.name.trim().is_empty() { stem.to_string() } else { expand(&r.name, stem, start + i as u64, tags.get(i).and_then(Option::as_ref)) };
            // A tag a file lacks leaves a gap: no doubled or leading spaces for it.
            if uses_tags(&r.name) {
                base = base.split_whitespace().collect::<Vec<_>>().join(" ");
            }
            if let Some(re) = &find {
                // Plain text is replaced as typed; an expression's $1 means its first group.
                base = if r.regex { re.replace_all(&base, r.replace.as_str()).into_owned() } else { re.replace_all(&base, regex_lite::NoExpand(&r.replace)).into_owned() };
            }
            base = match r.case {
                Case::Keep => base,
                Case::Lower => base.to_lowercase(),
                Case::Upper => base.to_uppercase(),
                Case::Title => title(&base),
            };
            format!("{base}{ext}")
        })
        .collect())
}

/// `pattern` for one item: each run of `#` its number, padded to the run's length, and
/// its tags where they are named (a slash in one would make a folder: it becomes a dash).
fn expand(pattern: &str, stem: &str, n: u64, tags: Option<&crate::musictags::Tags>) -> String {
    let mut pattern = pattern.to_string();
    for (w, key) in TAG_WORDS.iter().zip(["artist", "title", "track", "album"]) {
        if pattern.contains(w) {
            let v = tags.map(|t| t.get(key)).unwrap_or("");
            let v = if key == "track" { crate::musictags::track_name(v) } else { v.to_string() };
            // Kept apart from the # and {name} handling below.
            pattern = pattern.replace(w, &v.replace('/', "-").replace('#', "\u{2}"));
        }
    }
    let p = pattern.replace("{name}", "\u{1}");
    let mut out = String::new();
    let mut chars = p.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '#' => {
                let mut w = 1;
                while chars.peek() == Some(&'#') {
                    chars.next();
                    w += 1;
                }
                out += &format!("{n:0w$}");
            }
            '\u{1}' => out += stem,
            '\u{2}' => out.push('#'),
            c => out.push(c),
        }
    }
    out
}

fn title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut start = true;
    for c in s.chars() {
        if start {
            out.extend(c.to_uppercase());
        } else {
            out.extend(c.to_lowercase());
        }
        start = !c.is_alphanumeric() && c != '\'';
    }
    out
}

/// What is wrong with each new name, if anything: empty, a slash, the same as another
/// new one, or the name of something else in the folder (`others`).
pub fn problems(old: &[(String, bool)], new: &[String], others: &HashSet<String>) -> Vec<Option<&'static str>> {
    let mut count: HashMap<&str, usize> = HashMap::new();
    for n in new {
        *count.entry(n.as_str()).or_default() += 1;
    }
    let renamed: HashSet<&str> = old.iter().map(|o| o.0.as_str()).collect();
    new.iter()
        .map(|n| {
            if n.trim().is_empty() || n == "." || n == ".." {
                Some("no name")
            } else if n.contains('/') || n.contains('\0') {
                Some("can't have /")
            } else if count[n.as_str()] > 1 {
                Some("same as another")
            } else if others.contains(n) && !renamed.contains(n.as_str()) {
                Some("already there")
            } else {
                None
            }
        })
        .collect()
}

pub struct State {
    pub panel: u64,
    /// Path and whether it is a folder, in the listing's order.
    pub items: Vec<(PathBuf, bool)>,
    pub rules: Rules,
    /// What the names were worked out for, and what they came to.
    made: Option<Rules>,
    pub new: Result<Vec<String>, String>,
    pub bad: Vec<Option<&'static str>>,
    /// Names in the folder that aren't being renamed (from the listing, not the disk).
    others: HashSet<String>,
    focus: bool,
    /// The items' music tags, read once a pattern names one.
    tags: Vec<Option<crate::musictags::Tags>>,
    tags_rx: Option<std::sync::mpsc::Receiver<Vec<Option<crate::musictags::Tags>>>>,
    asked_tags: bool,
}

pub fn open(app: &App, panel: u64) -> Option<Popup> {
    let t = app.panels.get(&panel)?.tab();
    let items: Vec<(PathBuf, bool)> = t.target_entries().iter().map(|e| (e.path.clone(), e.is_dir)).collect();
    if items.is_empty() {
        return None;
    }
    let others = t.entries.iter().map(|e| e.name.clone()).collect();
    Some(Popup::BatchRename(State { panel, items, rules: Rules { start: "1".into(), ..Default::default() }, made: None, new: Ok(vec![]), bad: vec![], others, focus: true, tags: vec![], tags_rx: None, asked_tags: false }))
}

impl State {
    fn olds(&self) -> Vec<(String, bool)> {
        self.items.iter().map(|(p, d)| (p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), *d)).collect()
    }

    /// The renames to make: the items whose name changes, with their new names.
    pub fn plan(&self) -> Vec<(PathBuf, String)> {
        let Ok(new) = &self.new else { return vec![] };
        self.items.iter().zip(new).filter(|((p, _), n)| p.file_name().is_some_and(|o| o.to_string_lossy() != n.as_str())).map(|((p, _), n)| (p.clone(), n.clone())).collect()
    }

    /// Whether the music tags a pattern names are still being read.
    pub fn reading_tags(&self) -> bool {
        self.tags_rx.is_some()
    }

    pub fn refresh(&mut self, ctx: &egui::Context) {
        if uses_tags(&self.rules.name) && !self.asked_tags {
            self.asked_tags = true;
            if let Some(t) = crate::drives::Tools::current().filter(|t| t.has("ffprobe")) {
                let (tx, rx) = std::sync::mpsc::channel();
                let (files, ctx) = (self.items.iter().map(|i| i.0.clone()).collect::<Vec<_>>(), ctx.clone());
                std::thread::spawn(move || {
                    let _ = tx.send(crate::musictags::for_rename(&t, &files));
                    ctx.request_repaint();
                });
                self.tags_rx = Some(rx);
            }
        }
        if let Some(rx) = &self.tags_rx {
            if let Ok(t) = rx.try_recv() {
                self.tags = t;
                self.tags_rx = None;
                self.made = None;
            }
        }
        if self.made.as_ref() == Some(&self.rules) {
            return;
        }
        let olds = self.olds();
        self.new = names_with(&olds, &self.rules, &self.tags);
        self.bad = match &self.new {
            Ok(n) => problems(&olds, n, &self.others),
            Err(_) => vec![],
        };
        self.made = Some(self.rules.clone());
    }
}

/// The dialog: the rules on top, every old name beside its new one below, a clash
/// shown on its row; Rename once nothing clashes and something changes.
pub fn popup(app: &mut App, ctx: &egui::Context, st: &mut State) -> bool {
    let pal = app.pal;
    let k = pp::take_keys(ctx);
    if k.esc {
        return false;
    }
    st.refresh(ctx);
    let fs = app.cfg.font_size;
    let small = fs - 1.5;
    let plan = st.plan();
    let clashes = st.bad.iter().filter(|b| b.is_some()).count();
    let ok = st.new.is_ok() && clashes == 0 && !plan.is_empty();
    let mut go = k.enter && ok;
    let mut cancel = false;
    let list_max = pp::fit_list(ctx, 360.0, 300.0);
    let n = st.items.len();
    pp::centered(ctx, "batch rename", 680.0, &pal, |ui| {
        pp::dialog_title(ui, &format!("Rename {n} {}", if n == 1 { "item" } else { "items" }), None, &pal);
        ui.add_space(8.0);
        let field = |ui: &mut egui::Ui, text: &mut String, id: &'static str, hint: &str, w: f32| {
            let r = ui.add(egui::TextEdit::singleline(text).id(Id::new(("batch rename", id))).hint_text(egui::RichText::new(hint).color(pal.text_dim)).desired_width(w).margin(egui::Margin::symmetric(6, 3)));
            crate::ui::note_control(ui, r.rect, id);
            r
        };
        let label_w = (fs * 6.2).round();
        let label = |ui: &mut egui::Ui, t: &str| {
            let (r, _) = ui.allocate_exact_size(vec2(label_w, fs * 1.6), Sense::hover());
            ui.painter().text(pos2(r.left(), r.center().y), egui::Align2::LEFT_CENTER, t, FontId::proportional(fs - 0.5), pal.text_dim);
        };
        ui.horizontal(|ui| {
            label(ui, "New name");
            let w = (ui.available_width() - label_w - 70.0).max(120.0);
            let r = field(ui, &mut st.rules.name, "rename name", "Keep the names, or e.g. Trip ###, {name} 2026 or {track} {title}", w);
            if st.focus {
                r.request_focus();
                st.focus = false;
            }
            ui.label(egui::RichText::new("from").size(small).color(pal.text_dim));
            field(ui, &mut st.rules.start, "rename start", "1", 40.0);
        });
        ui.horizontal(|ui| {
            label(ui, "Find");
            let w = ((ui.available_width() - label_w - 80.0) / 2.0).max(90.0);
            field(ui, &mut st.rules.find, "rename find", "", w);
            ui.label(egui::RichText::new("with").size(small).color(pal.text_dim));
            field(ui, &mut st.rules.replace, "rename replace", "", w);
            // Switches, lit in the accent's tint while on, not buttons.
            if crate::kit::toggle(ui, "Aa", st.rules.case_sensitive, false, "rename match case", "Match case", &pal) {
                st.rules.case_sensitive = !st.rules.case_sensitive;
            }
            if crate::kit::toggle(ui, ".*", st.rules.regex, true, "rename regex", "Regular expression; $1 in the replacement is its first group", &pal) {
                st.rules.regex = !st.rules.regex;
            }
        });
        ui.horizontal(|ui| {
            label(ui, "Case");
            // One choice of four, under a pill that slides to it.
            let cases = [Case::Keep, Case::Lower, Case::Upper, Case::Title];
            let cur = cases.iter().position(|c| *c == st.rules.case).unwrap_or(0);
            if let Some(k) = crate::kit::segmented(ui, Id::new("rename case"), &["As is", "lower", "UPPER", "Title"], &["rename case keep", "rename case lower", "rename case upper", "rename case title"], &[], cur, 12.0, 22.0, &pal) {
                st.rules.case = cases[k];
            }
        });
        ui.add_space(8.0);
        // Old beside new: the new name in the theme's text, a clash in red with why.
        let row_h = (fs * 1.35 + 10.0).round();
        let news = st.new.as_ref().ok();
        // What the new names don't share, lit in the accent: the counter, or the part taken
        // from each file. Worked out on the names as they stand, in characters.
        let (pre, suf) = news.map(|v| varying(v)).unwrap_or((usize::MAX, 0));
        egui::ScrollArea::vertical().id_salt("batch rename list").max_height(list_max).min_scrolled_height((n as f32 * row_h).min(list_max)).auto_shrink([false, true]).show_rows(ui, row_h, n, |ui, range| {
            for i in range {
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
                if i % 2 == 1 {
                    ui.painter().rect_filled(r, CornerRadius::same(4), pal.card());
                }
                let old = st.items[i].0.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let new = news.and_then(|v| v.get(i)).cloned().unwrap_or_default();
                let bad = st.bad.get(i).copied().flatten();
                let half = (r.width() - 40.0) / 2.0;
                let font = FontId::proportional(fs);
                let g = crate::listing::elided(ui, &old, font.clone(), half - 8.0, pal.text_dim);
                ui.painter().galley(pos2(r.left() + 8.0, r.center().y - g.size().y / 2.0), g, pal.text_dim);
                ui.painter().text(pos2(r.left() + 8.0 + half + 12.0, r.center().y), egui::Align2::CENTER_CENTER, "→", font.clone(), pal.text_dim);
                let x = r.left() + half + 34.0;
                let room = r.right() - 8.0 - x;
                let g = match bad {
                    Some(why) => crate::listing::elided(ui, &format!("{new}  ({why})"), font, room, pal.danger),
                    None if new == old => crate::listing::elided(ui, &new, font, room, pal.text_dim),
                    None => {
                        let chars: Vec<char> = new.chars().collect();
                        let (a, b) = (pre.min(chars.len()), chars.len().saturating_sub(suf).max(pre.min(chars.len())));
                        let mut job = egui::text::LayoutJob::default();
                        for (t, c) in [(&chars[..a], pal.text), (&chars[a..b], pal.accent), (&chars[b..], pal.text)] {
                            job.append(&t.iter().collect::<String>(), 0.0, egui::TextFormat { font_id: font.clone(), color: c, ..Default::default() });
                        }
                        job.wrap = egui::text::TextWrapping { max_width: room, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
                        ui.fonts_mut(|f| f.layout_job(job))
                    }
                };
                ui.painter().galley(pos2(x, r.center().y - g.size().y / 2.0), g, pal.text);
            }
        });
        ui.add_space(6.0);
        let (status, col) = match &st.new {
            _ if st.reading_tags() => ("Reading the music tags…".to_string(), pal.text_dim),
            Err(e) => (format!("The expression won't work: {e}"), pal.danger),
            Ok(_) if clashes > 0 => (format!("{clashes} {} can't be used", if clashes == 1 { "name" } else { "names" }), pal.danger),
            Ok(_) if plan.is_empty() => ("Nothing changes yet".to_string(), pal.text_dim),
            Ok(_) => (format!("{} to rename", plan.len()), pal.text_dim),
        };
        ui.label(egui::RichText::new(status).size(small).color(col));
        pp::footer(ui, &[("Enter", "rename"), ("Esc", "close")], &pal, |ui| {
            let b = pp::primary_button(ui, "Rename", ok, &pal);
            crate::ui::note_control(ui, b.rect, "rename apply");
            go |= b.clicked() && ok;
            let c = pp::plain_button(ui, "Cancel", &pal);
            crate::ui::note_control(ui, c.rect, "rename cancel");
            cancel = c.clicked();
        });
    });
    if cancel {
        return false;
    }
    if go {
        app.rename_batch(st.panel, plan);
        return false;
    }
    true
}

/// How many characters every name in `names` starts with, and ends with, alike: what lies
/// between is what tells them apart. A single name has nothing to tell apart from.
fn varying(names: &[String]) -> (usize, usize) {
    if names.len() < 2 {
        return (usize::MAX, 0);
    }
    let cs: Vec<Vec<char>> = names.iter().map(|n| n.chars().collect()).collect();
    let short = cs.iter().map(|c| c.len()).min().unwrap_or(0);
    let pre = (0..short).take_while(|&i| cs.iter().all(|c| c[i] == cs[0][i])).count();
    let suf = (0..short - pre).take_while(|&i| cs.iter().all(|c| c[c.len() - 1 - i] == cs[0][cs[0].len() - 1 - i])).count();
    (pre, suf)
}
