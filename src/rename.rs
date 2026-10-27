//! Inline (multi-cursor) rename: every field is edited at once, relative to its own caret.

use std::collections::HashSet;
use std::path::PathBuf;

use egui::{Event, Key, Modifiers};

#[derive(Clone, Debug)]
pub struct Field {
    pub idx: u32,
    pub path: PathBuf,
    pub orig: String,
    pub text: Vec<char>,
    pub anchor: usize,
    pub caret: usize,
}

impl Field {
    fn sel(&self) -> (usize, usize) {
        (self.anchor.min(self.caret), self.anchor.max(self.caret))
    }
    pub fn text_string(&self) -> String {
        self.text.iter().collect()
    }
    pub fn selection(&self) -> (usize, usize) {
        self.sel()
    }
    fn delete_sel(&mut self) -> bool {
        let (a, b) = self.sel();
        if a == b {
            return false;
        }
        self.text.drain(a..b);
        self.anchor = a;
        self.caret = a;
        true
    }
    fn insert(&mut self, s: &str) {
        self.delete_sel();
        let chars: Vec<char> = s.chars().filter(|c| *c != '\n' && *c != '\r' && *c != '/').collect();
        let n = chars.len();
        self.text.splice(self.caret..self.caret, chars);
        self.caret += n;
        self.anchor = self.caret;
    }
    fn stem_end(&self) -> usize {
        // Exclude the extension unless the name is a dotfile with no other dot.
        match self.text.iter().rposition(|&c| c == '.') {
            Some(0) | None => self.text.len(),
            Some(p) => p,
        }
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric()
}

fn word_left(t: &[char], mut i: usize) -> usize {
    while i > 0 && !is_word(t[i - 1]) {
        i -= 1;
    }
    while i > 0 && is_word(t[i - 1]) {
        i -= 1;
    }
    i
}

fn word_right(t: &[char], mut i: usize) -> usize {
    while i < t.len() && !is_word(t[i]) {
        i += 1;
    }
    while i < t.len() && is_word(t[i]) {
        i += 1;
    }
    i
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Continue,
    Commit,
    Cancel,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Case {
    Lower,
    Upper,
    Title,
    Sentence,
}

pub struct Rename {
    pub fields: Vec<Field>,
    /// The item was just created (New folder/file): cancel deletes nothing, it simply keeps the default name.
    pub created: bool,
    f2_cycle: u8,
    pub find: String,
    pub replace: String,
    pub show_find: bool,
    pub error: Option<String>,
    /// When a refused name last shook the field (the frame's time).
    pub shake: f64,
    /// `conflicts()` for the names it was last computed from (it stats every target; the
    /// list asks once per visible field per frame).
    conflict_cache: std::cell::RefCell<Option<(u64, std::rc::Rc<HashSet<usize>>)>>,
}

impl Rename {
    pub fn new(items: Vec<(u32, PathBuf, String)>, created: bool) -> Rename {
        let fields = items
            .into_iter()
            .map(|(idx, path, name)| {
                let mut f = Field { idx, path, text: name.chars().collect(), orig: name, anchor: 0, caret: 0 };
                f.caret = f.stem_end();
                f
            })
            .collect();
        Rename { fields, created, f2_cycle: 0, find: String::new(), replace: String::new(), show_find: false, error: None,
            shake: -10.0, conflict_cache: Default::default() }
    }

    pub fn is_batch(&self) -> bool {
        self.fields.len() > 1
    }

    pub fn field_for(&self, idx: u32) -> Option<&Field> {
        self.fields.iter().find(|f| f.idx == idx)
    }

    // -------------------------------------------------------- mirrored edits

    pub fn insert_all(&mut self, s: &str) {
        for f in &mut self.fields {
            f.insert(s);
        }
    }

    /// Insert a per-field string at each caret.
    /// Insert a per-field string at each caret; the inserted text stays selected.
    pub fn insert_each(&mut self, mut make: impl FnMut(usize, &Field) -> String) {
        for i in 0..self.fields.len() {
            let s = make(i, &self.fields[i]);
            let f = &mut self.fields[i];
            f.insert(&s);
            f.anchor = f.caret - s.chars().filter(|c| *c != '\n' && *c != '\r' && *c != '/').count();
        }
    }

    pub fn insert_incremental_id(&mut self) {
        let width = self.fields.len().to_string().len();
        self.insert_each(|i, _| format!("{:0width$}", i + 1));
    }

    pub fn insert_date(&mut self, which: DateKind) {
        let today = crate::timefmt::format(std::time::SystemTime::now(), "%Y-%m-%d");
        self.insert_each(|_, f| {
            let t = match which {
                DateKind::Today => return today.clone(),
                DateKind::Created => std::fs::metadata(&f.path).and_then(|m| m.created()).ok(),
                DateKind::Modified => std::fs::metadata(&f.path).and_then(|m| m.modified()).ok(),
            };
            t.map(|t| crate::timefmt::format(t, "%Y-%m-%d")).unwrap_or_default()
        });
    }

    pub fn insert_parent_name(&mut self) {
        self.insert_each(|_, f| f.path.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    }

    pub fn insert_unique_id(&mut self) {
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0) as u64;
        let mut x = seed | 1;
        self.insert_each(|_, _| {
            // xorshift → 6 base-36 chars
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let mut v = x;
            let mut s = String::new();
            for _ in 0..6 {
                let d = (v % 36) as u32;
                s.push(std::char::from_digit(d, 36).unwrap());
                v /= 36;
            }
            s
        });
    }

    pub fn insert_original(&mut self) {
        self.insert_each(|_, f| f.orig.clone());
    }

    pub fn change_case(&mut self, case: Case) {
        for f in &mut self.fields {
            let (mut a, mut b) = f.sel();
            if a == b {
                a = 0;
                b = f.stem_end();
            }
            let s: String = f.text[a..b].iter().collect();
            let out = match case {
                Case::Lower => s.to_lowercase(),
                Case::Upper => s.to_uppercase(),
                Case::Title => s
                    .split_inclusive(|c: char| !c.is_alphanumeric())
                    .map(|w| {
                        let mut cs = w.chars();
                        match cs.next() {
                            Some(c) => c.to_uppercase().collect::<String>() + &cs.as_str().to_lowercase(),
                            None => String::new(),
                        }
                    })
                    .collect(),
                Case::Sentence => {
                    let l = s.to_lowercase();
                    let mut cs = l.chars();
                    match cs.next() {
                        Some(c) => c.to_uppercase().collect::<String>() + cs.as_str(),
                        None => String::new(),
                    }
                }
            };
            let chars: Vec<char> = out.chars().collect();
            let n = chars.len();
            f.text.splice(a..b, chars);
            f.anchor = a;
            f.caret = a + n;
        }
    }

    /// Find & replace in every name (plain text, case-sensitive).
    pub fn find_replace(&mut self) {
        if self.find.is_empty() {
            return;
        }
        for f in &mut self.fields {
            let s: String = f.text.iter().collect();
            let r = s.replace(&self.find, &self.replace);
            f.text = r.chars().collect();
            f.caret = f.stem_end().min(f.text.len());
            f.anchor = f.caret;
        }
    }

    pub fn click_at(&mut self, offset: usize, shift: bool) {
        for f in &mut self.fields {
            f.caret = offset.min(f.text.len());
            if !shift {
                f.anchor = f.caret;
            }
        }
    }

    pub fn select_all(&mut self) {
        for f in &mut self.fields {
            f.anchor = 0;
            f.caret = f.text.len();
        }
    }

    pub fn copy_text(&self) -> String {
        self.fields
            .iter()
            .map(|f| {
                let (a, b) = f.sel();
                if a == b { f.text_string() } else { f.text[a..b].iter().collect() }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn paste(&mut self, s: &str) {
        let lines: Vec<&str> = s.lines().collect();
        if lines.len() <= 1 {
            self.insert_all(s.trim_end_matches(['\n', '\r']));
        } else {
            for (f, l) in self.fields.iter_mut().zip(lines.iter()) {
                f.insert(l);
            }
            if lines.len() != self.fields.len() {
                self.error = Some(format!("Pasted {} lines into {} names", lines.len(), self.fields.len()));
            }
        }
    }

    fn each(&mut self, mut op: impl FnMut(&mut Field)) {
        for f in &mut self.fields {
            op(f);
        }
    }

    /// Feed this frame's input events. Returns the outcome.
    pub fn handle(&mut self, events: &[Event], clip_out: &mut Option<String>) -> Outcome {
        for ev in events {
            match ev {
                Event::Text(t) => {
                    self.error = None;
                    self.insert_all(t);
                }
                Event::Ime(egui::ImeEvent::Commit(t)) => self.insert_all(t),
                Event::Paste(t) => self.paste(t),
                Event::Copy => *clip_out = Some(self.copy_text()),
                Event::Cut => {
                    *clip_out = Some(self.copy_text());
                    self.each(|f| {
                        f.delete_sel();
                    });
                }
                Event::Key { key, pressed: true, modifiers, .. } => {
                    if let Some(o) = self.key(*key, *modifiers) {
                        return o;
                    }
                }
                _ => {}
            }
        }
        Outcome::Continue
    }

    fn key(&mut self, key: Key, m: Modifiers) -> Option<Outcome> {
        let word = m.ctrl || m.command;
        match key {
            Key::Enter => return Some(Outcome::Commit),
            Key::Escape => return Some(Outcome::Cancel),
            Key::Backspace => self.each(|f| {
                if !f.delete_sel() && f.caret > 0 {
                    let to = if word { word_left(&f.text, f.caret) } else { f.caret - 1 };
                    f.text.drain(to..f.caret);
                    f.caret = to;
                    f.anchor = to;
                }
            }),
            Key::Delete => self.each(|f| {
                if !f.delete_sel() && f.caret < f.text.len() {
                    let to = if word { word_right(&f.text, f.caret) } else { f.caret + 1 };
                    f.text.drain(f.caret..to);
                    f.anchor = f.caret;
                }
            }),
            Key::ArrowLeft => self.each(|f| {
                let (a, _) = f.sel();
                f.caret = if word {
                    word_left(&f.text, f.caret)
                } else if f.anchor != f.caret && !m.shift {
                    a
                } else {
                    f.caret.saturating_sub(1)
                };
                if !m.shift {
                    f.anchor = f.caret;
                }
            }),
            Key::ArrowRight => self.each(|f| {
                let (_, b) = f.sel();
                f.caret = if word {
                    word_right(&f.text, f.caret)
                } else if f.anchor != f.caret && !m.shift {
                    b
                } else {
                    (f.caret + 1).min(f.text.len())
                };
                if !m.shift {
                    f.anchor = f.caret;
                }
            }),
            Key::Home => self.each(|f| {
                f.caret = 0;
                if !m.shift {
                    f.anchor = 0;
                }
            }),
            Key::End => self.each(|f| {
                f.caret = f.text.len();
                if !m.shift {
                    f.anchor = f.caret;
                }
            }),
            Key::A if word => self.select_all(),
            Key::F2 => {
                // Cycle: stem → whole name → extension.
                self.f2_cycle = (self.f2_cycle + 1) % 3;
                let c = self.f2_cycle;
                self.each(|f| {
                    let se = f.stem_end();
                    match c {
                        1 => {
                            f.anchor = 0;
                            f.caret = f.text.len();
                        }
                        2 => {
                            f.anchor = (se + 1).min(f.text.len());
                            f.caret = f.text.len();
                        }
                        _ => {
                            f.anchor = 0;
                            f.caret = se;
                        }
                    }
                });
            }
            _ => {}
        }
        None
    }

    /// Planned renames (path, new name) with trimming; unchanged names are skipped.
    pub fn plan(&self) -> Vec<(PathBuf, String)> {
        self.fields
            .iter()
            .map(|f| (f.path.clone(), f.text_string().trim().to_string()))
            .filter(|(p, n)| p.file_name().map(|o| o.to_string_lossy() != n.as_str()).unwrap_or(true))
            .collect()
    }

    /// Indices of fields whose result is invalid or collides.
    /// Fields whose new name is invalid, duplicated or would overwrite another file.
    /// Cached until a name changes.
    pub fn conflicts(&self) -> std::rc::Rc<HashSet<usize>> {
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            for f in &self.fields {
                f.text.hash(&mut h);
                f.path.hash(&mut h);
            }
            h.finish()
        };
        if let Some((k, v)) = &*self.conflict_cache.borrow() {
            if *k == key {
                return v.clone();
            }
        }
        let v = std::rc::Rc::new(self.compute_conflicts());
        *self.conflict_cache.borrow_mut() = Some((key, v.clone()));
        v
    }

    /// Uncached (the commit re-checks the disk right before renaming).
    /// Why the one name being typed can't be used, in words.
    pub fn reason(&self) -> Option<String> {
        let f = self.fields.first()?;
        let name = f.text_string().trim().to_string();
        if name.is_empty() {
            return Some("A name can't be empty".into());
        }
        if name.contains('/') {
            return Some("A name can't have “/” in it".into());
        }
        if name == "." || name == ".." {
            return Some(format!("“{name}” is the folder itself"));
        }
        let to = f.path.with_file_name(&name);
        (to != f.path && std::fs::symlink_metadata(&to).is_ok()).then(|| format!("“{name}” is already here"))
    }

    pub fn compute_conflicts(&self) -> HashSet<usize> {
        let mut bad = HashSet::new();
        let mut seen: std::collections::HashMap<(PathBuf, String), usize> = std::collections::HashMap::new();
        let renaming: HashSet<&PathBuf> = self.fields.iter().map(|f| &f.path).collect();
        for (i, f) in self.fields.iter().enumerate() {
            let name = f.text_string().trim().to_string();
            if name.is_empty() || name == "." || name == ".." || name.contains('/') {
                bad.insert(i);
                continue;
            }
            let dir = f.path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
            if let Some(&j) = seen.get(&(dir.clone(), name.clone())) {
                bad.insert(i);
                bad.insert(j);
            }
            seen.insert((dir.clone(), name.clone()), i);
            let target = dir.join(&name);
            if target != f.path && std::fs::symlink_metadata(&target).is_ok() && !renaming.contains(&target) {
                bad.insert(i);
            }
        }
        bad
    }
}

#[derive(Clone, Copy)]
pub enum DateKind {
    Today,
    Created,
    Modified,
}

/// Apply renames safely even when names swap (two-phase via temp names).
pub fn apply(plan: &[(PathBuf, String)]) -> Vec<String> {
    let mut errors = vec![];
    let mut temps = vec![];
    for (i, (p, _)) in plan.iter().enumerate() {
        let tmp = p.with_file_name(format!(".maia-rename-{}-{i}", std::process::id()));
        match std::fs::rename(p, &tmp) {
            Ok(()) => temps.push(Some(tmp)),
            Err(e) => {
                errors.push(format!("{}: {e}", p.display()));
                temps.push(None);
            }
        }
    }
    for ((p, name), tmp) in plan.iter().zip(temps) {
        let Some(tmp) = tmp else { continue };
        let dest = p.with_file_name(name);
        if std::fs::symlink_metadata(&dest).is_ok() {
            errors.push(format!("{} already exists", dest.display()));
            let _ = std::fs::rename(&tmp, p);
            continue;
        }
        if let Err(e) = crate::fsx::rename_new(&tmp, &dest) {
            errors.push(if e.kind() == std::io::ErrorKind::AlreadyExists { format!("{} already exists", dest.display()) } else { format!("{}: {e}", dest.display()) });
            let _ = std::fs::rename(&tmp, p);
        }
    }
    errors
}
