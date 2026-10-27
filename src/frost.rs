//! Frosted popups: menus, the palette and dialogs show the window behind them blurred,
//! under their own colour a little thinned, instead of a flat card.
//!
//! The blur is made on the graphics card while the frame is painted: a callback placed
//! first in a popup's layer takes what is drawn so far (everything under the popup),
//! halves it three times through small framebuffers (each halving averages four pixels,
//! so the eighth-size result is soft), and the popup draws that, stretched back up, before
//! its own background. Nothing comes back to the processor. Where the card or the driver
//! can't (no framebuffer blits), popups stay as they were.

use std::sync::{Arc, Mutex, OnceLock};

use egui::{Color32, CornerRadius, Rect, Ui, pos2};
use egui_glow::glow::{self, HasContext as _};

struct Gl {
    fbos: [glow::Framebuffer; 3],
    texs: [glow::Texture; 3],
    size: [u32; 2],
    failed: bool,
}

static GL: Mutex<Option<Gl>> = Mutex::new(None);
static TEX: OnceLock<egui::TextureId> = OnceLock::new();

/// At start, with the painter: the framebuffers and the texture egui draws the blur from.
pub fn setup(gl: &glow::Context, painter: &mut egui_glow::Painter) {
    unsafe {
        let mut texs = vec![];
        let mut fbos = vec![];
        for _ in 0..3 {
            let (Ok(t), Ok(f)) = (gl.create_texture(), gl.create_framebuffer()) else { return };
            texs.push(t);
            fbos.push(f);
        }
        let (Ok(texs), Ok(fbos)) = (<[glow::Texture; 3]>::try_from(texs), <[glow::Framebuffer; 3]>::try_from(fbos)) else { return };
        let id = painter.register_native_texture(texs[2]);
        let _ = TEX.set(id);
        *GL.lock().unwrap_or_else(|e| e.into_inner()) = Some(Gl { fbos, texs, size: [0, 0], failed: false });
    }
}

fn ready() -> Option<egui::TextureId> {
    let g = GL.lock().unwrap_or_else(|e| e.into_inner());
    g.as_ref().filter(|g| !g.failed)?;
    TEX.get().copied()
}

/// A popup's fill: thinned when the blur shows through it.
pub fn fill(c: Color32) -> Color32 {
    // Thin enough that the window behind shows as a tint, solid enough that text over a
    // busy folder stays as easy to read as on a flat card.
    if ready().is_some() { Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 218) } else { c }
}

/// The blur, made from the framebuffer as it is at this point of the frame.
fn blur(info: &egui::PaintCallbackInfo, painter: &egui_glow::Painter) {
    let gl = painter.gl();
    let mut g = GL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(st) = g.as_mut().filter(|s| !s.failed) else { return };
    let [w, h] = info.screen_size_px;
    if w < 8 || h < 8 {
        return;
    }
    let sizes: Vec<[i32; 2]> = (1..=3).map(|k| [(w >> k).max(1) as i32, (h >> k).max(1) as i32]).collect();
    unsafe {
        while gl.get_error() != glow::NO_ERROR {}
        if st.size != [w, h] {
            for (i, s) in sizes.iter().enumerate() {
                gl.bind_texture(glow::TEXTURE_2D, Some(st.texs[i]));
                gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, s[0], s[1], 0, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(None));
                for (p, v) in [(glow::TEXTURE_MIN_FILTER, glow::LINEAR), (glow::TEXTURE_MAG_FILTER, glow::LINEAR), (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE), (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE)] {
                    gl.tex_parameter_i32(glow::TEXTURE_2D, p, v as i32);
                }
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(st.fbos[i]));
                gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(st.texs[i]), 0);
            }
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.bind_texture(glow::TEXTURE_2D, None);
            st.size = [w, h];
        }
        // Blits ignore the viewport but not the scissor; egui sets it again after.
        gl.disable(glow::SCISSOR_TEST);
        let mut from: (Option<glow::Framebuffer>, [i32; 2]) = (None, [w as i32, h as i32]);
        for (i, s) in sizes.iter().enumerate() {
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, from.0);
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(st.fbos[i]));
            gl.blit_framebuffer(0, 0, from.1[0], from.1[1], 0, 0, s[0], s[1], glow::COLOR_BUFFER_BIT, glow::LINEAR);
            from = (Some(st.fbos[i]), *s);
        }
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        if gl.get_error() != glow::NO_ERROR {
            st.failed = true;
        }
    }
}

/// First thing inside a popup's area: the window behind it, blurred, where the popup was
/// last frame (an area keeps its size), with `radius` corners. Nothing on the very first
/// frame, before the popup has a size.
pub fn behind(ui: &Ui, radius: u8) {
    let Some(tex) = ready() else { return };
    let ctx = ui.ctx();
    let Some(rect) = ctx.memory(|m| m.area_rect(ui.layer_id().id)) else { return };
    let screen = ctx.content_rect();
    if !rect.is_positive() || !screen.is_positive() {
        return;
    }
    let ppp = ctx.pixels_per_point();
    let (w, h) = ((screen.width() * ppp).round().max(1.0), (screen.height() * ppp).round().max(1.0));
    let cb = egui::PaintCallback { rect: screen, callback: Arc::new(egui_glow::CallbackFn::new(|info, painter| blur(&info, painter))) };
    let p = ui.painter().with_clip_rect(screen);
    p.add(egui::Shape::Callback(cb));
    // The texture is the framebuffer's, bottom row first.
    let uv = Rect::from_min_max(pos2((rect.left() - screen.left()) * ppp / w, 1.0 - (rect.top() - screen.top()) * ppp / h), pos2((rect.right() - screen.left()) * ppp / w, 1.0 - (rect.bottom() - screen.top()) * ppp / h));
    p.add(egui::epaint::RectShape::filled(rect, CornerRadius::same(radius), Color32::WHITE).with_texture(tex, uv));
}
