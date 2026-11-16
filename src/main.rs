//! EagleView: a SysML v2 editor and viewer with a code pane and a diagram, rendered with
//! Vulkan through the `gpu` crate.
//!
//! Usage: `eagleview [--config settings.toml] [--export-png diagram.png] [model.sysml]`
//!
//! Without a model the bundled sample opens. Settings come from `--config`,
//! else `eagleview.toml` in the working directory or next to the executable;
//! both are reloaded when another program changes them. Edits in the code
//! pane update the diagram as you type; Ctrl+S saves them. `--export-png`
//! writes the diagram to a PNG file and exits (status 1 if that failed).

mod backdrop;
mod clipboard;
mod code_view;
mod config;
mod diagram;
mod diagram_view;
mod dialog;
mod docs;
mod document;
mod editor;
mod export;
mod ibd;
mod ibd_view;
mod menu;
mod model_edit;
mod requirements;
mod allocations;
mod checks;
mod shapes;
mod source;
mod states;
mod state_view;
mod text;
mod theme;
mod tree_view;
mod ui;

use gpu::{Attachment, Device, DeviceDesc, Error, RenderDesc};
use backdrop::Backdrop;
use shapes::ShapeRenderer;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use text::TextRenderer;
use code_view::Mods;
use ui::Ui;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

const SAMPLE_MODEL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/sysml/tests/data/drone.sysml");
const CONFIG_NAME: &str = "eagleview.toml";

struct Renderer {
    // Field order is drop order: the swapchain waits for the GPU and must go
    // before the resources it used and before the window.
    swapchain: gpu::Swapchain,
    backdrop: Backdrop,
    shapes: ShapeRenderer,
    text: TextRenderer,
    device: Arc<Device>,
    ui: Ui,
    window: Arc<Window>,
    title: String,
    cursor_icon: winit::window::CursorIcon,
    last_frame: Instant,
}

impl Renderer {
    fn new(window: Arc<Window>, ui: Ui) -> gpu::Result<Self> {
        // Shapes are fetched in the vertex shader through buffer device addresses.
        let desc = |validation| DeviceDesc {
            window: Some(&*window),
            validation,
            buffer_device_address: true,
            ..Default::default()
        };
        let device = Device::new(desc(cfg!(debug_assertions))).or_else(|e| match e {
            // The validation layer ships with the Vulkan SDK; run without it otherwise.
            Error::LayerUnavailable => Device::new(desc(false)),
            e => Err(e),
        })?;
        let size = window.inner_size();
        let swapchain = device.create_swapchain(&*window, size.width, size.height)?;
        let backdrop = Backdrop::new(&device, swapchain.format(), swapchain.extent())?;
        let shapes = ShapeRenderer::new(&device, swapchain.format(), &backdrop)?;
        let text = TextRenderer::new(&device, swapchain.format())?;
        Ok(Self {
            swapchain,
            backdrop,
            shapes,
            text,
            device,
            ui,
            window,
            title: String::new(),
            cursor_icon: Default::default(),
            last_frame: Instant::now(),
        })
    }

    /// Carries out a pending File > Export Diagram as PNG, between frames, and
    /// returns the status message (`Err` if it failed). `None` if none was pending.
    fn export_if_requested(&mut self) -> Option<Result<String, String>> {
        let path = self.ui.take_export_request()?;
        let max_side = export::max_side(&self.device);
        let result = match self.ui.draw_export(&mut self.shapes, &mut self.text, max_side) {
            None => None,
            Some(size) => {
                let (format, clear) = (self.swapchain.format(), self.ui.clear_color());
                let written = export::write_png(&self.device, &mut self.shapes, &mut self.text, format, size, clear, &path);
                Some(written.map(|()| size).map_err(|e| e.to_string()))
            }
        };
        let ok = matches!(result, Some(Ok(_)));
        let message = self.ui.export_finished(&path, result);
        Some(if ok { Ok(message) } else { Err(message) })
    }

    /// Runs the requested export now (for `--export-png`).
    fn export_message(&mut self) -> Result<String, String> {
        self.export_if_requested().unwrap_or_else(|| Err("no export was requested".into()))
    }

    fn resize(&mut self) {
        let size = self.window.inner_size();
        match self.swapchain.resize(size.width, size.height) {
            Ok(()) | Err(Error::OutOfDate) => {} // minimized: retry on the next resize
            Err(e) => panic!("swapchain resize failed: {e}"),
        }
    }

    fn render(&mut self) -> gpu::Result<()> {
        let now = Instant::now();
        let ms = now.duration_since(self.last_frame).as_secs_f32() * 1000.0;
        self.ui.frame_ms += (ms - self.ui.frame_ms) * 0.05;
        self.last_frame = now;

        // Before this frame queues anything: the export uses the same renderers.
        self.export_if_requested();
        self.ui.poll_files();
        let title = self.ui.title();
        if title != self.title {
            self.window.set_title(&title);
            self.title = title;
        }
        let icon = self.ui.cursor_icon();
        if icon != self.cursor_icon {
            self.window.set_cursor(icon);
            self.cursor_icon = icon;
        }

        let extent = self.swapchain.extent();
        let dpi = self.window.scale_factor() as f32;
        self.ui.draw(&mut self.shapes, &mut self.text, extent.width as f32, extent.height as f32, dpi);
        let frame = match self.swapchain.begin_frame() {
            Ok(frame) => frame,
            Err(Error::OutOfDate) => {
                self.shapes.discard();
                self.text.discard();
                self.resize();
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        // After begin_frame: the frame slot's previous GPU work has finished.
        self.shapes.prepare()?;
        self.text.prepare()?;
        let (cmd, target) = (frame.command, frame.target);
        if self.backdrop.canvas().extent() != target.extent() {
            self.backdrop.resize(target.extent())?;
            self.shapes.set_backdrop(&self.backdrop)?;
        }
        // The canvas renders offscreen, so the glass panels can show it blurred.
        let canvas = self.backdrop.canvas();
        cmd.begin_render(&RenderDesc {
            area: canvas.rect(),
            colors: &[Attachment::clear_color(canvas, self.ui.clear_color())],
            ..Default::default()
        })?;
        self.shapes.record(cmd, canvas.extent(), shapes::PASS_CANVAS)?;
        self.text.record(cmd, canvas.extent(), shapes::PASS_CANVAS)?;
        cmd.end_render()?;
        self.backdrop.record_blur(cmd, self.ui.backdrop_blur(dpi))?;

        // The window: the canvas, then the floating panels, then menus; each
        // pass covers the shapes and text of the ones before it.
        cmd.begin_render(&RenderDesc {
            area: target.rect(),
            colors: &[Attachment { load: gpu::vk::AttachmentLoadOp::DONT_CARE, ..Attachment::load(target) }],
            ..Default::default()
        })?;
        self.backdrop.record_composite(cmd)?;
        for pass in shapes::PASS_CHROME..shapes::PASSES {
            self.shapes.record(cmd, target.extent(), pass)?;
            self.text.record(cmd, target.extent(), pass)?;
        }
        cmd.end_render()?;
        match self.swapchain.end_frame() {
            Err(Error::OutOfDate) => self.resize(),
            other => other?,
        }
        Ok(())
    }
}

struct App {
    model: PathBuf,
    config: Option<PathBuf>,
    /// `--export-png`: write the diagram here and quit without showing the window.
    export: Option<PathBuf>,
    export_failed: bool,
    renderer: Option<Renderer>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("EagleView")
            .with_inner_size(winit::dpi::LogicalSize::new(1400.0, 860.0))
            .with_visible(self.export.is_none());
        let window = Arc::new(event_loop.create_window(attributes).expect("create window"));
        let mut ui = Ui::new(self.model.clone(), self.config.clone());
        ui.owner = native_handle(&window);
        let mut renderer = Renderer::new(window, ui).expect("initialize renderer");
        if let Some(path) = self.export.take() {
            renderer.ui.request_export(path);
            let message = renderer.export_message();
            self.export_failed = message.is_err();
            match message {
                Ok(m) => println!("{m}"),
                Err(m) => eprintln!("{m}"),
            }
            drop(renderer);
            event_loop.exit();
            return;
        }
        self.renderer = Some(renderer);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(renderer) = &mut self.renderer else { return };
        let ui = &mut renderer.ui;
        match event {
            WindowEvent::CloseRequested if !ui.close_requested() => {}
            WindowEvent::CloseRequested => {
                self.renderer = None;
                event_loop.exit();
            }
            WindowEvent::Resized(_) => renderer.resize(),
            WindowEvent::ModifiersChanged(m) => {
                let s = m.state();
                ui.set_modifiers(Mods { ctrl: s.control_key(), shift: s.shift_key(), alt: s.alt_key() });
            }
            WindowEvent::DroppedFile(path) => ui.open(path),
            WindowEvent::CursorMoved { position, .. } => ui.cursor_moved([position.x as f32, position.y as f32]),
            WindowEvent::CursorLeft { .. } => ui.cursor_left(),
            WindowEvent::MouseInput { state, button, .. } => ui.mouse_button(button, state == ElementState::Pressed),
            WindowEvent::MouseWheel { delta, .. } => ui.wheel(match delta {
                MouseScrollDelta::LineDelta(_, y) => y,
                MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40.0,
            }),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                ui.key(&event.logical_key, event.text.as_ref().map(|t| t.as_str()))
            }
            WindowEvent::RedrawRequested => {
                renderer.render().expect("render frame");
                renderer.window.request_redraw();
            }
            _ => {}
        }
        // File > Exit, after any unsaved changes were dealt with.
        if self.renderer.as_ref().is_some_and(|r| r.ui.exit_requested) {
            self.renderer = None;
            event_loop.exit();
        }
    }
}

/// The window's HWND on Windows, the owner of native dialogs (0 elsewhere).
fn native_handle(window: &Window) -> isize {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
        _ => 0,
    }
}

/// `--config <path>`, else `eagleview.toml` in the working directory, next to
/// the executable, or in the source tree (for `cargo run`).
fn find_config(explicit: Option<PathBuf>) -> Option<PathBuf> {
    if explicit.is_some() {
        return explicit;
    }
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(CONFIG_NAME)));
    [Some(PathBuf::from(CONFIG_NAME)), exe_dir, Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(CONFIG_NAME))]
        .into_iter()
        .flatten()
        .find(|p| p.is_file())
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    let (mut model, mut config, mut export) = (None, None, None);
    while let Some(arg) = args.next() {
        if arg == "--config" {
            config = args.next().map(PathBuf::from);
        } else if arg == "--export-png" {
            export = args.next().map(PathBuf::from);
        } else {
            model = Some(PathBuf::from(arg));
        }
    }
    let model = model.unwrap_or_else(|| SAMPLE_MODEL.into());
    let config = find_config(config);
    let event_loop = EventLoop::new().expect("create event loop");
    let mut app = App { model, config, export, export_failed: false, renderer: None };
    event_loop.run_app(&mut app).expect("run event loop");
    if app.export_failed {
        std::process::exit(1);
    }
}
