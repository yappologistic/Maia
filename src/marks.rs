//! Marks: a letter for a place, set with Ctrl+M and a letter, gone back to with Ctrl+'
//! and the letter. A mark holds the file under the cursor (going back selects it in
//! its folder) or the folder. Kept with the user's data, so they last.

use std::path::PathBuf;

use egui::{CornerRadius, Event, FontId, Sense, pos2, vec2};

use crate::app::{Action, App};
use crate::popups as pp;

pub struct State {
    /// Setting a mark (else going to one).
    pub set: bool,
    pub sel: usize,
}

/// What a mark set now would hold: the file under the cursor, else the folder.
pub fn here(app: &mut App) -> Option<PathBuf> {
    let t = app.tab();
    if crate::tab::is_place(&t.path) || t.archive.is_some() {
        return None;
    }
    match t.cursor_path() {
        Some(p) if !p.is_dir() => Some(p),
        _ => Some(t.path.clone()),
    }
}

/// The letter typed this frame, if any (other keys are left alone).
fn letter(ctx: &egui::Context) -> Option<char> {
    ctx.input_mut(|i| {
        let mut got = None;
        i.events.retain(|e| match e {
            Event::Text(t) if got.is_none() => match t.chars().next().filter(|c| c.is_ascii_alphabetic()) {
                Some(c) => {
                    got = Some(c.to_ascii_lowercase());
                    false
                }
                None => true,
            },
            _ => true,
        });
        got
    })
}

pub fn popup(app: &mut App, ctx: &egui::Context, st: &mut State) -> bool {
    let pal = app.pal;
    let k = pp::take_keys(ctx);
    if k.esc {
        return false;
    }
    let del = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Delete));
    let marks: Vec<(String, PathBuf)> = app.ud.marks.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    pp::step(&mut st.sel, marks.len(), &k, |_| false);
    if let Some(c) = letter(ctx) {
        if st.set {
            let Some(p) = here(app) else {
                app.toast("Nothing here to mark");
                return false;
            };
            app.toast(format!("Marked {} as {c}", pp::home_short(&p)));
            app.ud.marks.insert(c.to_string(), p);
            app.ud_dirty = true;
            return false;
        }
        match app.ud.marks.get(&c.to_string()).cloned() {
            Some(p) => {
                go(app, p);
                return false;
            }
            None => app.toast(format!("No mark {c}")),
        }
    }
    if del && !st.set {
        if let Some((key, _)) = marks.get(st.sel) {
            app.ud.marks.remove(key);
            app.ud_dirty = true;
        }
    }
    if k.enter && !st.set {
        if let Some((_, p)) = marks.get(st.sel) {
            go(app, p.clone());
            return false;
        }
    }
    let fs = app.cfg.font_size;
    let small = fs - 1.5;
    let mut chosen: Option<PathBuf> = None;
    let mut close = false;
    let target = if st.set { here(app) } else { None };
    let (_, rect) = pp::centered(ctx, "marks", 520.0, &pal, |ui| {
        let (title, sub) = if st.set {
            ("Set a mark", target.as_ref().map(|p| format!("Press a letter for {}.", pp::home_short(p))))
        } else {
            ("Go to a mark", Some("Press its letter.".to_string()))
        };
        pp::dialog_title(ui, title, sub.as_deref(), &pal);
        ui.add_space(8.0);
        if marks.is_empty() {
            ui.label(egui::RichText::new(if st.set { "No marks yet" } else { "No marks yet: Ctrl+M and a letter sets one" }).color(pal.text_dim));
        }
        let row_h = (fs * 1.35 + 10.0).round();
        for (j, (key, p)) in marks.iter().enumerate() {
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
            let on = j == st.sel && !st.set;
            if on {
                ui.painter().rect_filled(r, CornerRadius::same(6), pal.selected);
            } else if resp.hovered() {
                ui.painter().rect_filled(r, CornerRadius::same(6), pal.hover);
            }
            let (tc, dc) = if on { (pal.on_sel(), pal.on_sel_dim()) } else { (pal.text, pal.text_dim) };
            // The letter in a key cap, as the hints show keys.
            let kg = ui.painter().layout_no_wrap(key.to_uppercase(), FontId::monospace(small), tc);
            let kr = egui::Rect::from_center_size(pos2(r.left() + 20.0, r.center().y), vec2(22.0, kg.size().y + 6.0));
            ui.painter().rect(kr, CornerRadius::same(4), pal.bg, egui::Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
            ui.painter().galley(kr.center() - kg.size() / 2.0, kg, tc);
            let gone = !p.exists();
            let text = pp::home_short(p);
            let g = crate::listing::elided_tail(ui, &text, FontId::proportional(fs), r.right() - kr.right() - 24.0, if gone { dc } else { tc });
            ui.painter().galley(pos2(kr.right() + 12.0, r.center().y - g.size().y / 2.0), g, tc);
            crate::ui::note_control(ui, r.shrink(1.0), "mark row");
            if resp.clicked() && !st.set {
                chosen = Some(p.clone());
            }
        }
        let keys: &[(&str, &str)] = if st.set { &[("a–z", "mark"), ("Esc", "close")] } else { &[("a–z", "go"), ("Enter", "go"), ("Delete", "forget"), ("Esc", "close")] };
        pp::hints(ui, keys, &pal);
        let _ = &mut close;
    });
    if let Some(p) = chosen {
        go(app, p);
        return false;
    }
    !(close || pp::clicked_outside(ctx, rect))
}

/// Go to a mark: a file opens its folder with it selected.
fn go(app: &mut App, p: PathBuf) {
    if !p.exists() {
        app.toast(format!("{} is gone", pp::home_short(&p)));
        return;
    }
    let panel = app.active;
    app.actions.push(Action::Navigate { panel, path: p });
}
