use crate::drawing::{self, Preview as Update};
use crate::playback::Playback;
use crate::{HEIGHT, Result, Run, WIDTH};
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

enum Event {
    Frame,
    Done,
}

struct Preview {
    window: Rc<Window>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
}

impl Preview {
    fn new(event_loop: &ActiveEventLoop) -> Result<Self> {
        let window = Rc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("Auto Paint")
                    .with_inner_size(LogicalSize::new(WIDTH, HEIGHT))
                    .with_decorations(false)
                    .with_window_level(winit::window::WindowLevel::Normal),
            )?,
        );
        window.set_cursor_visible(false);
        let context = softbuffer::Context::new(window.clone()).map_err(|err| err.to_string())?;
        let surface =
            softbuffer::Surface::new(&context, window.clone()).map_err(|err| err.to_string())?;
        Ok(Self { window, surface })
    }

    fn render(&mut self, frame: &[u32]) -> Result<()> {
        let window = self.window.inner_size();
        let (Some(width), Some(height)) = (
            NonZeroU32::new(window.width),
            NonZeroU32::new(window.height),
        ) else {
            return Ok(());
        };
        self.surface
            .resize(width, height)
            .map_err(|err| err.to_string())?;
        let mut dest = self.surface.buffer_mut().map_err(|err| err.to_string())?;
        let ratio = (f64::from(window.width) / f64::from(WIDTH))
            .min(f64::from(window.height) / f64::from(HEIGHT));
        let scaled_width = (f64::from(WIDTH) * ratio).round() as usize;
        let scaled_height = (f64::from(HEIGHT) * ratio).round() as usize;
        let left = (window.width as usize - scaled_width) / 2;
        let top = (window.height as usize - scaled_height) / 2;
        dest.fill(0x202020);
        for (y, row) in dest
            .chunks_exact_mut(window.width as usize)
            .skip(top)
            .take(scaled_height)
            .enumerate()
        {
            let from = y * HEIGHT as usize / scaled_height * WIDTH as usize;
            for (x, pixel) in row[left..left + scaled_width].iter_mut().enumerate() {
                *pixel = frame[from + x * WIDTH as usize / scaled_width];
            }
        }
        self.window.pre_present_notify();
        dest.present().map_err(|err| err.to_string())?;
        Ok(())
    }
}

#[derive(Default)]
struct Frames {
    latest: Option<Arc<[u32]>>,
    pending: bool,
}

struct Watch {
    frames: Arc<Mutex<Frames>>,
    preview: Option<Preview>,
    error: Option<String>,
}

impl ApplicationHandler<Event> for Watch {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.preview.is_none() {
            match Preview::new(event_loop) {
                Ok(preview) => self.preview = Some(preview),
                Err(err) => {
                    self.error = Some(err.to_string());
                    event_loop.exit();
                }
            }
        }
        if let Some(preview) = &self.preview {
            preview.window.request_redraw();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Event) {
        match event {
            Event::Done => event_loop.exit(),
            Event::Frame => {
                self.frames.lock().expect("frame lock").pending = false;
                if let Some(preview) = &self.preview {
                    preview.window.request_redraw();
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(preview) = &mut self.preview else {
            return;
        };
        if id != preview.window.id() {
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Escape) =>
            {
                event_loop.exit();
            }
            WindowEvent::Resized(_) => preview.window.request_redraw(),
            WindowEvent::RedrawRequested => {
                let latest = self.frames.lock().expect("frame lock").latest.clone();
                if let Some(frame) = latest
                    && let Err(err) = preview.render(&frame)
                {
                    self.error = Some(err.to_string());
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }
}

pub fn run(run: Run) -> Result<()> {
    let event_loop = EventLoop::<Event>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let frames = Arc::new(Mutex::new(Frames::default()));
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker = {
        let frames = frames.clone();
        let cancelled = cancelled.clone();
        thread::spawn(move || {
            let playback = if run.options.narrate {
                match Playback::open() {
                    Ok(playback) => Some(playback),
                    Err(error) => {
                        eprintln!("Live audio unavailable: {error}");
                        None
                    }
                }
            } else {
                None
            };
            let mut update = |update: Update<'_>| match update {
                Update::Frame(pixels) => {
                    let mut frames = frames.lock().expect("frame lock");
                    frames.latest = Some(Arc::from(pixels));
                    if !frames.pending {
                        frames.pending = true;
                        let _ = proxy.send_event(Event::Frame);
                    }
                }
                Update::Speech(samples) => {
                    if let Some(playback) = &playback {
                        playback.play(samples);
                    }
                }
            };
            let result = drawing::run(run, cancelled, Some(&mut update));
            let _ = proxy.send_event(Event::Done);
            result
        })
    };
    let mut watch = Watch {
        frames,
        preview: None,
        error: None,
    };
    let window_result = event_loop.run_app(&mut watch);
    cancelled.store(true, Ordering::Relaxed);
    let result = worker.join().map_err(|_| "The drawing worker panicked")?;
    window_result?;
    if let Some(error) = watch.error {
        return Err(error.into());
    }
    result
}
