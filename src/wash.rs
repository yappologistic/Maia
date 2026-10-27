//! Themes that wash in: a new theme spreads over the window in a circle from where the
//! pointer is (the card picked or hovered in the theme picker), over the old one.
//!
//! The old window is kept on the graphics card: the frame the change is asked in is still
//! drawn in the old theme, and the runner copies it into a texture before showing it.
//! The next frame draws in the new theme with that copy on top, a hole in it growing from
//! the pointer until nothing of it is left. Nothing comes back to the processor.

use egui::{Color32, Id, LayerId, Order, Pos2, pos2, vec2};

/// From the first frame of the new theme to the last of the old.
const TIME: f64 = 0.55;

#[derive(Default)]
pub enum Wash {
    #[default]
    Idle,
    /// The theme changed: this frame keeps the old one, for the runner to copy.
    Asked(Pos2),
    /// The copy, ready: the next frame shows the new theme under it.
    Ready(Pos2, egui::TextureId, [u32; 2]),
    Running { at: Pos2, tex: egui::TextureId, size: [u32; 2], t0: f64 },
    /// The runner could not copy: the change goes ahead without the wash.
    Skip,
}

impl Wash {
    /// At the start of a frame whose theme differs from the one drawn: whether to apply
    /// it now. False keeps the old theme one more frame while the window is copied.
    pub fn theme_changing(&mut self, ctx: &egui::Context, animations: bool) -> bool {
        let at = ctx.input(|i| i.pointer.latest_pos()).unwrap_or_else(|| ctx.content_rect().center());
        match std::mem::take(self) {
            Wash::Ready(at, tex, size) => {
                *self = Wash::Running { at, tex, size, t0: ctx.input(|i| i.time) };
                ctx.request_repaint();
                true
            }
            // A copy asked for and not made (no runner of ours draws this frame), or one
            // that failed: no wash this time.
            Wash::Asked(_) | Wash::Skip => true,
            Wash::Idle | Wash::Running { .. } if animations => {
                *self = Wash::Asked(at);
                ctx.request_repaint();
                false
            }
            _ => true,
        }
    }

    /// For the runner, after painting: where to copy the frame for, if it is asked.
    pub fn wanted(&self) -> bool {
        matches!(self, Wash::Asked(_))
    }

    /// From the runner: the copy made (or not).
    pub fn copied(&mut self, got: Option<(egui::TextureId, [u32; 2])>) {
        if let Wash::Asked(at) = *self {
            *self = match got {
                Some((tex, size)) => Wash::Ready(at, tex, size),
                None => Wash::Skip,
            };
        }
    }

    /// At the end of a frame: the old window over everything, a growing hole in it.
    pub fn paint(&mut self, ctx: &egui::Context) {
        let Wash::Running { at, tex, size, t0 } = *self else { return };
        let now = ctx.input(|i| i.time);
        let t = ((now - t0) / TIME).clamp(0.0, 1.0);
        if t >= 1.0 {
            *self = Wash::Idle;
            return;
        }
        // Slow at the start and the end, as a drop spreading.
        let k = (t * t * (3.0 - 2.0 * t)) as f32;
        let screen = ctx.content_rect();
        let far = [screen.left_top(), screen.right_top(), screen.left_bottom(), screen.right_bottom()].iter().map(|c| (*c - at).length()).fold(0.0, f32::max) + 4.0;
        let r = far * k;
        let ppp = ctx.pixels_per_point();
        let (w, h) = (size[0].max(1) as f32, size[1].max(1) as f32);
        // The copy is the framebuffer's, bottom row first.
        let uv = |p: Pos2| pos2(p.x * ppp / w, 1.0 - p.y * ppp / h);
        let mut mesh = egui::Mesh::with_texture(tex);
        let segs = 96u32;
        let outer = far * 1.5;
        for i in 0..segs {
            let a = i as f32 / segs as f32 * std::f32::consts::TAU;
            let d = vec2(a.cos(), a.sin());
            let pin = at + d * r;
            let pout = at + d * outer;
            mesh.vertices.push(egui::epaint::Vertex { pos: pin, uv: uv(pin), color: Color32::WHITE });
            mesh.vertices.push(egui::epaint::Vertex { pos: pout, uv: uv(pout), color: Color32::WHITE });
        }
        for i in 0..segs {
            let (a, b) = (i * 2, ((i + 1) % segs) * 2);
            mesh.add_triangle(a, a + 1, b);
            mesh.add_triangle(a + 1, b + 1, b);
        }
        let p = ctx.layer_painter(LayerId::new(Order::Debug, Id::new("theme wash"))).with_clip_rect(screen);
        p.add(mesh);
        // A faint edge where the two meet.
        if r > 1.0 {
            p.circle_stroke(at, r, egui::Stroke::new(1.0, Color32::from_white_alpha((40.0 * (1.0 - k)) as u8)));
        }
        ctx.request_repaint();
    }
}
