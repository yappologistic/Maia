//! Minimal native runner: winit window + glutin GL context + egui-winit input + egui_glow painter.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_winit::winit;
use glutin::context::NotCurrentGlContext as _;
use glutin::display::{GetGlDisplay as _, GlDisplay as _};
use glutin::prelude::GlSurface as _;
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::raw_window_handle::HasWindowHandle as _;
use winit::window::{Window, WindowId};

use crate::app::App;
use crate::signals::OrDie as _;

pub struct Options {
    pub title: &'static str,
    pub app_id: &'static str,
    pub size: [f32; 2],
    pub min_size: [f32; 2],
    pub icon: (Vec<u8>, u32, u32),
    /// A picker window: the app's request it answers (no single-instance socket, no
    /// "Show in folder" name; those are the main window's).
    pub picker: Option<Box<crate::picker::Picker>>,
    /// A request the window answers as soon as it is up (`--properties`, which the
    /// file-manager service starts a window with).
    pub first: Option<crate::single::Request>,
}

enum UserEvent {
    Repaint(Duration),
    /// SIGTERM/SIGHUP/SIGINT: close like the window's close button (session is saved).
    Quit,
    /// A screen reader asking for what the window shows, or for an action on it.
    #[cfg(feature = "screen-reader")]
    Access(egui_winit::accesskit_winit::Event),
}

#[cfg(feature = "screen-reader")]
impl From<egui_winit::accesskit_winit::Event> for UserEvent {
    fn from(e: egui_winit::accesskit_winit::Event) -> Self {
        UserEvent::Access(e)
    }
}

/// GL state. Field order is drop order: the EGL surface and context must go before the
/// window, or the driver talks to an already-destroyed Wayland surface (crash on close).
struct Gl {
    /// Wayland: frames are paced by the compositor's frame callbacks, not by blocking
    /// in the buffer swap.
    wayland: bool,
    glow: Arc<egui_glow::glow::Context>,
    surface: glutin::surface::Surface<glutin::surface::WindowSurface>,
    context: glutin::context::PossiblyCurrentContext,
    window: Window,
}

struct Runner {
    opts: Options,
    paths: Option<Vec<PathBuf>>,
    proxy: EventLoopProxy<UserEvent>,
    gl: Option<Gl>,
    ctx: egui::Context,
    state: Option<egui_winit::State>,
    painter: Option<egui_glow::Painter>,
    viewport: egui::ViewportInfo,
    app: Option<App>,
    repaint_at: Option<Instant>,
    shown: bool,
    mods: winit::keyboard::ModifiersState,
    /// The single-instance socket this window answers on (removed on exit).
    listener: Option<(std::os::unix::net::UnixListener, PathBuf)>,
    sock: Option<PathBuf>,
    #[cfg(feature = "testkit")]
    tk: Option<crate::testkit::Driver>,
}

pub fn run(opts: Options, paths: Vec<PathBuf>, listener: Option<(std::os::unix::net::UnixListener, PathBuf)>) {
    let event_loop = EventLoop::<UserEvent>::with_user_event().build().or_die("No display connection (is a Wayland or X11 session running?)");
    let proxy = event_loop.create_proxy();
    let q = proxy.clone();
    crate::signals::install(move || {
        let _ = q.send_event(UserEvent::Quit);
    });
    let mut r = Runner {
        opts,
        paths: Some(paths),
        proxy,
        gl: None,
        ctx: egui::Context::default(),
        state: None,
        painter: None,
        viewport: Default::default(),
        app: None,
        repaint_at: None,
        shown: false,
        mods: Default::default(),
        listener,
        sock: None,
        #[cfg(feature = "testkit")]
        tk: None,
    };
    if let Err(e) = event_loop.run_app(&mut r) {
        crate::signals::log(&format!("event loop stopped: {e}"));
    }
}

fn create_gl(el: &ActiveEventLoop, o: &Options) -> Gl {
    let mut attrs = Window::default_attributes()
        .with_title(o.title)
        .with_inner_size(winit::dpi::LogicalSize::new(o.size[0], o.size[1]))
        .with_min_inner_size(winit::dpi::LogicalSize::new(o.min_size[0], o.min_size[1]))
        .with_decorations(false)
        .with_visible(false)
        .with_window_icon(winit::window::Icon::from_rgba(o.icon.0.clone(), o.icon.1, o.icon.2).ok());
    {
        attrs = winit::platform::wayland::WindowAttributesExtWayland::with_name(attrs, o.app_id, o.app_id);
    }
    {
        attrs = winit::platform::x11::WindowAttributesExtX11::with_name(attrs, o.app_id, o.app_id);
    }
    let template = glutin::config::ConfigTemplateBuilder::new().prefer_hardware_accelerated(None).with_depth_size(0).with_stencil_size(0).with_transparency(false);
    let (window, config) = glutin_winit::DisplayBuilder::new()
        .with_preference(glutin_winit::ApiPreference::FallbackEgl)
        .with_window_attributes(Some(attrs.clone()))
        .build(el, template, |mut it| it.next().or_die("No usable OpenGL configuration"))
        .or_die("Could not create a GL display");
    let display = config.display();
    let raw = window.as_ref().and_then(|w| w.window_handle().ok()).map(|h| h.as_raw());
    let ctx_attrs = glutin::context::ContextAttributesBuilder::new().build(raw);
    let gles_attrs = glutin::context::ContextAttributesBuilder::new().with_context_api(glutin::context::ContextApi::Gles(None)).build(raw);
    let not_current = unsafe { display.create_context(&config, &ctx_attrs).or_else(|_| display.create_context(&config, &gles_attrs)).or_die("Could not create a GL context") };
    let window = window.unwrap_or_else(|| glutin_winit::finalize_window(el, attrs, &config).or_die("Could not create the window"));
    let (w, h): (u32, u32) = window.inner_size().into();
    let surf_attrs = glutin::surface::SurfaceAttributesBuilder::<glutin::surface::WindowSurface>::new().build(
        window.window_handle().or_die("Window handle").as_raw(),
        NonZeroU32::new(w).unwrap_or(NonZeroU32::MIN),
        NonZeroU32::new(h).unwrap_or(NonZeroU32::MIN),
    );
    let surface = unsafe { display.create_window_surface(&config, &surf_attrs).or_die("Could not create a GL surface") };
    let context = not_current.make_current(&surface).or_die("Could not make the GL context current");
    // A hidden window (another workspace, minimised) gets no frame callbacks, and on
    // Wayland a vsynced swap waits for one with no timeout: the window froze and stopped
    // answering the compositor, which then offered to kill it. There the swap never waits;
    // winit holds redraws until the next frame callback instead (pre_present_notify).
    use winit::raw_window_handle::{HasDisplayHandle as _, RawDisplayHandle};
    let wayland = window.display_handle().is_ok_and(|h| matches!(h.as_raw(), RawDisplayHandle::Wayland(_)));
    let interval = if wayland { glutin::surface::SwapInterval::DontWait } else { glutin::surface::SwapInterval::Wait(NonZeroU32::MIN) };
    let _ = surface.set_swap_interval(&context, interval);
    let glow = unsafe { egui_glow::glow::Context::from_loader_function_cstr(|s| display.get_proc_address(s)) };
    Gl { wayland, window, context, surface, glow: Arc::new(glow) }
}

impl Runner {
    fn redraw(&mut self, el: &ActiveEventLoop) {
        let (Some(gl), Some(state), Some(painter), Some(app)) = (self.gl.as_ref(), self.state.as_mut(), self.painter.as_mut(), self.app.as_mut()) else { return };
        let window = &gl.window;
        egui_winit::update_viewport_info(&mut self.viewport, &self.ctx, window, false);
        let mut input = state.take_egui_input(window);
        input.viewports.insert(egui::ViewportId::ROOT, self.viewport.clone());
        for ev in crate::clipboard::take_dnd() {
            app.on_dnd(ev);
        }
        input.events.append(&mut app.inject);
        #[cfg(feature = "testkit")]
        let shot = match self.tk.as_mut() {
            Some(tk) => {
                input.events.extend(tk.take());
                // egui hears of the modifier keys as an event of its own, sent when they
                // change (their release too).
                static LAST: std::sync::Mutex<egui::Modifiers> = std::sync::Mutex::new(egui::Modifiers::NONE);
                let held = crate::testkit::held();
                let m = input
                    .events
                    .iter()
                    .rev()
                    .find_map(|e| match e {
                        egui::Event::PointerButton { modifiers, .. } | egui::Event::Key { modifiers, .. } => Some(*modifiers),
                        _ => None,
                    })
                    .unwrap_or(held);
                let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
                if *last != m {
                    input.events.insert(0, egui::Event::ModifiersChanged(m));
                    *last = m;
                }
                if tk.pending() {
                    window.request_redraw();
                }
                tk.take_shot()
            }
            None => None,
        };
        let mut out = self.ctx.run_pass(input, |ctx| app.frame(ctx));
        for (_, vo) in out.viewport_output {
            let mut actions = Default::default();
            egui_winit::process_viewport_commands(&self.ctx, &mut self.viewport, vo.commands, window, &mut actions);
        }
        for cmd in &out.platform_output.commands {
            if let egui::OutputCommand::CopyText(t) = cmd {
                crate::clipboard::set_text(&t);
            }
        }
        state.handle_platform_output(window, out.platform_output);
        let close = self.viewport.events.iter().any(|e| matches!(e, egui::ViewportEvent::Close)) || app.quit;
        self.viewport.events.clear();
        // Paint
        let bg = app.pal.bg;
        unsafe {
            use egui_glow::glow::HasContext as _;
            gl.glow.clear_color(bg.r() as f32 / 255.0, bg.g() as f32 / 255.0, bg.b() as f32 / 255.0, 1.0);
            gl.glow.clear(egui_glow::glow::COLOR_BUFFER_BIT);
        }
        for (id, deltas) in &out.textures_delta.set {
            for delta in deltas {
                painter.set_texture(*id, delta);
            }
        }
        let prims = self.ctx.tessellate(out.shapes, out.pixels_per_point);
        let size: [u32; 2] = window.inner_size().into();
        painter.paint_primitives(size, out.pixels_per_point, &prims);
        for id in &out.textures_delta.free {
            painter.free_texture(*id);
        }
        out.textures_delta.clear();
        #[cfg(feature = "testkit")]
        if let Some(path) = shot {
            crate::testkit::save_frame(&gl.glow, size, &path);
        }
        if gl.wayland {
            window.pre_present_notify();
        }
        let _ = gl.surface.swap_buffers(&gl.context);
        if !self.shown {
            window.set_visible(true);
            self.shown = true;
        }
        if close {
            self.exit(el);
        }
    }

    fn exit(&mut self, el: &ActiveEventLoop) {
        if let Some(mut app) = self.app.take() {
            app.on_exit();
        }
        if let Some(s) = self.sock.take() {
            let _ = std::fs::remove_file(s);
        }
        // Tear down in dependency order: GL objects (needs the context current), then the
        // EGL surface and context, and only then the window they render into.
        if let Some(mut p) = self.painter.take() {
            p.destroy();
        }
        self.state = None;
        if let Some(gl) = self.gl.take() {
            let Gl { glow, surface, context, window, .. } = gl;
            drop(glow);
            drop(surface);
            drop(context);
            window.set_visible(false);
            drop(window);
        }
        el.exit();
    }

    fn schedule(&mut self, el: &ActiveEventLoop) {
        el.set_control_flow(match self.repaint_at {
            Some(t) => ControlFlow::WaitUntil(t),
            None => {
                // Nothing due: a moment to hand freed memory back.
                crate::memory::idle();
                ControlFlow::Wait
            }
        });
    }
}

impl ApplicationHandler<UserEvent> for Runner {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gl.is_some() {
            return;
        }
        let gl = create_gl(el, &self.opts);
        if let Some(p) = &self.opts.picker {
            crate::picker::set_parent(&gl.window, &p.pick.parent);
        }
        let painter = egui_glow::Painter::new(gl.glow.clone(), "", None, true).or_die("Could not create the painter");
        #[allow(unused_mut)]
        let mut state = egui_winit::State::new(self.ctx.clone(), egui::ViewportId::ROOT, el, Some(gl.window.scale_factor() as f32), el.system_theme(), Some(painter.max_texture_side()));
        // Before the window is shown, as the accessibility bus wants. The tree is only
        // built once a screen reader asks for it.
        #[cfg(feature = "screen-reader")]
        state.init_accesskit(el, &gl.window, self.proxy.clone());
        let proxy = self.proxy.clone();
        self.ctx.set_request_repaint_callback(move |info| {
            let _ = proxy.send_event(UserEvent::Repaint(info.delay));
        });
        egui_winit::update_viewport_info(&mut self.viewport, &self.ctx, &gl.window, true);
        crate::clipboard::init(&gl.window);
        let p = self.proxy.clone();
        crate::clipboard::set_dnd_waker(move || {
            let _ = p.send_event(UserEvent::Repaint(Duration::ZERO));
        });
        #[cfg(feature = "testkit")]
        {
            let p = self.proxy.clone();
            self.tk = crate::testkit::Driver::start(move || {
                let _ = p.send_event(UserEvent::Repaint(std::time::Duration::ZERO));
            });
        }
        let mut app = App::new(&self.ctx, self.paths.take().unwrap_or_default());
        if let Some(p) = self.opts.picker.take() {
            app.picker = Some(p);
            self.app = Some(app);
            gl.window.request_redraw();
            self.gl = Some(gl);
            self.state = Some(state);
            self.painter = Some(painter);
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        if let Some((l, sock)) = self.listener.take() {
            let p = self.proxy.clone();
            crate::single::serve(l, tx.clone(), move || {
                let _ = p.send_event(UserEvent::Repaint(Duration::ZERO));
            });
            self.sock = Some(sock);
        }
        if let Some(r) = self.opts.first.take() {
            let _ = tx.send(r);
        }
        // "Show in folder" from other apps, over the session bus.
        let p = self.proxy.clone();
        crate::fm1::serve(tx, move || {
            let _ = p.send_event(UserEvent::Repaint(Duration::ZERO));
        });
        app.requests = Some(rx);
        self.app = Some(app);
        gl.window.request_redraw();
        self.gl = Some(gl);
        self.state = Some(state);
        self.painter = Some(painter);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, mut event: WindowEvent) {
        let Some(gl) = self.gl.as_ref() else { return };
        match &event {
            WindowEvent::CloseRequested => {
                // Edits not saved are asked about first; the window stays until then.
                if self.app.as_mut().is_some_and(|a| a.ask_unsaved()) {
                    if let Some(gl) = &self.gl {
                        gl.window.request_redraw();
                    }
                    return;
                }
                self.exit(el);
                return;
            }
            WindowEvent::RedrawRequested => {
                self.repaint_at = None;
                self.redraw(el);
                self.schedule(el);
                return;
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::KeyboardInput { event: k, .. } if k.state.is_pressed() => {
                // Hand the system clipboard to egui right before it handles a paste.
                use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
                let v = matches!(k.physical_key, PhysicalKey::Code(KeyCode::KeyV)) || matches!(&k.logical_key, Key::Character(c) if c.eq_ignore_ascii_case("v"));
                let paste = (self.mods.control_key() && v) || matches!(k.logical_key, Key::Named(NamedKey::Paste));
                if paste {
                    if let (Some(state), Some(text)) = (self.state.as_mut(), crate::clipboard::get_text()) {
                        state.set_clipboard_text(text);
                    }
                }
            }
            WindowEvent::Resized(s) => {
                gl.surface.resize(&gl.context, NonZeroU32::new(s.width).unwrap_or(NonZeroU32::MIN), NonZeroU32::new(s.height).unwrap_or(NonZeroU32::MIN));
                gl.window.request_redraw();
            }
            _ => {}
        }
        // "Reverse scroll direction" option.
        if let (WindowEvent::MouseWheel { delta, .. }, Some(app)) = (&mut event, self.app.as_ref()) {
            if app.cfg.reverse_scroll {
                *delta = match *delta {
                    winit::event::MouseScrollDelta::LineDelta(x, y) => winit::event::MouseScrollDelta::LineDelta(-x, -y),
                    winit::event::MouseScrollDelta::PixelDelta(p) => winit::event::MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(-p.x, -p.y)),
                };
            }
        }
        if let Some(state) = self.state.as_mut() {
            let r = state.on_window_event(&gl.window, &event);
            if r.repaint {
                gl.window.request_redraw();
            }
        }
    }

    fn user_event(&mut self, el: &ActiveEventLoop, ev: UserEvent) {
        let delay = match ev {
            UserEvent::Repaint(d) => d,
            UserEvent::Quit => return self.exit(el),
            #[cfg(feature = "screen-reader")]
            UserEvent::Access(e) => {
                use egui_winit::accesskit_winit::WindowEvent as A;
                match e.window_event {
                    A::InitialTreeRequested => self.ctx.enable_accesskit(),
                    A::ActionRequested(r) => {
                        if let Some(s) = self.state.as_mut() {
                            s.on_accesskit_action_request(r);
                        }
                    }
                    A::AccessibilityDeactivated => self.ctx.disable_accesskit(),
                }
                Duration::ZERO
            }
        };
        if delay.is_zero() {
            if let Some(gl) = self.gl.as_ref() {
                gl.window.request_redraw();
            }
        } else if let Some(t) = Instant::now().checked_add(delay) {
            self.repaint_at = Some(self.repaint_at.map_or(t, |c| c.min(t)));
            self.schedule(el);
        }
    }

    fn new_events(&mut self, el: &ActiveEventLoop, cause: StartCause) {
        if let StartCause::ResumeTimeReached { .. } = cause {
            self.repaint_at = None;
            if let Some(gl) = self.gl.as_ref() {
                gl.window.request_redraw();
            }
            self.schedule(el);
        }
    }
}
