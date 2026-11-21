use crate::codex::{self, Event, Process};
use crate::mcp::{Action, Batch, Button, Point, Server};
use crate::narration::Cue;
use crate::video::Video;
use crate::{FPS, HEIGHT, Result, Run, WIDTH};
use paintrs::automation::{Modifiers, NamedKey, Session};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use winit::keyboard::Key;

const FRAME_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / FPS as u64);
const RUN_LIMIT: Duration = Duration::from_secs(900);
const BATCH_LIMIT: Duration = Duration::from_secs(40);
const CLICK_HOLD: Duration = Duration::from_millis(35);

pub fn run(run: Run, cancelled: Arc<AtomicBool>, preview: impl FnMut(&[u32])) -> Result<()> {
    let mut drawing = Drawing::new(&run, &cancelled, preview)?;
    let result = if run.options.smoke_test {
        drawing.smoke_test()
    } else {
        drawing.run_codex(&run)
    };
    let output = drawing.finish(&run);
    if let Err(error) = &output {
        eprintln!("Could not finish recording: {error}");
    }
    result.and(output)?;
    println!(
        "Saved drawing.png and drawing.mp4 in {}",
        run.output.display()
    );
    Ok(())
}

struct Drawing<'a, P> {
    session: Session,
    video: Video,
    preview: P,
    cancelled: &'a AtomicBool,
    start: Instant,
    last_frame: Option<u64>,
    deadline: Instant,
    pointer: Point,
    cues: Vec<Cue>,
    narrate: bool,
}

impl<'a, P: FnMut(&[u32])> Drawing<'a, P> {
    fn new(run: &Run, cancelled: &'a AtomicBool, preview: P) -> Result<Self> {
        let video = Video::new(&run.output.join(".drawing.mp4"), run.options.narrate)?;
        let session = Session::new(WIDTH, HEIGHT)?;
        let now = Instant::now();
        let mut drawing = Self {
            session,
            video,
            preview,
            cancelled,
            start: now,
            last_frame: None,
            deadline: now + RUN_LIMIT,
            pointer: Point { x: -1, y: -1 },
            cues: Vec::new(),
            narrate: run.options.narrate,
        };
        drawing.render();
        drawing.sample()?;
        Ok(drawing)
    }

    fn render(&mut self) {
        (self.preview)(self.session.render());
    }

    fn sample(&mut self) -> Result<()> {
        let now = Instant::now();
        if self.session.animate(now) {
            self.render();
        }
        let frame = if self.last_frame.is_none() {
            0
        } else {
            (now.duration_since(self.start).as_nanos() * u128::from(FPS) / 1_000_000_000) as u64
        };
        if self.last_frame.is_none_or(|last| frame > last) {
            self.video.write(self.session.pixels(), frame)?;
            self.last_frame = Some(frame);
        }
        Ok(())
    }

    fn check_active(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Err("Drawing cancelled".into());
        }
        if Instant::now() >= self.deadline {
            return Err("Drawing exceeded its time limit".into());
        }
        Ok(())
    }

    fn wait(&mut self, delay: Duration) -> Result<()> {
        let stop = Instant::now() + delay;
        loop {
            self.check_active()?;
            self.sample()?;
            let remaining = stop.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            thread::sleep(remaining.min(FRAME_INTERVAL));
        }
    }

    fn run_codex(&mut self, run: &Run) -> Result<()> {
        let server = Server::bind()?;
        let (address, token) = (server.address, server.token.clone());
        let requests = server.start(run.options.turns)?;
        let mut codex = Process::start(run, address, &token)?;
        let mut usage = None;
        loop {
            self.check_active()?;
            self.sample()?;
            match requests.recv_timeout(Duration::from_millis(12)) {
                Ok(request) => {
                    let result = request.batch.map_or(Ok(()), |batch| self.batch(batch));
                    let result = result
                        .map_err(|err| err.to_string())
                        .and_then(|()| self.session.screenshot());
                    let _ = request.reply.send(result);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    thread::sleep(Duration::from_millis(12));
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
            let status = codex.status()?;
            for event in codex.events.try_iter() {
                match event {
                    Event::Message(text) => println!("{text}"),
                    Event::Tool { name, status } => println!("Paintrs {name} ({status})"),
                    Event::Usage(report) => usage = Some(report),
                    Event::Error(text) => eprintln!("{text}"),
                }
            }
            if let Some(status) = status {
                if let Some(usage) = usage {
                    codex::save_usage(&run.output.join("usage.json"), &run.options.model, usage)?;
                }
                if !status.success() {
                    return Err(format!("Codex exited with {status}").into());
                }
                return Ok(());
            }
        }
    }

    fn batch(&mut self, batch: Batch) -> Result<()> {
        if !(1..=128).contains(&batch.actions.len()) {
            return Err("An action batch needs 1 to 128 actions".into());
        }
        if let Some(line) = batch.narration.as_deref() {
            self.cue(line);
        }
        let deadline = self.deadline;
        self.deadline = deadline.min(Instant::now() + BATCH_LIMIT);
        let result = batch
            .actions
            .into_iter()
            .try_for_each(|action| self.act(action));
        self.deadline = deadline;
        result
    }

    fn cue(&mut self, line: &str) {
        let line = line.trim();
        if self.narrate && !line.is_empty() && self.cues.len() < 128 {
            self.cues.push(Cue {
                at: self.start.elapsed(),
                text: line.chars().take(200).collect(),
            });
        }
    }

    fn act(&mut self, action: Action) -> Result<()> {
        self.check_active()?;
        match action {
            Action::Move { x, y } => self.move_pointer(Point { x, y })?,
            Action::Click { x, y, button } => self.click(Point { x, y }, button)?,
            Action::DoubleClick { x, y, button } => {
                let point = Point { x, y };
                self.click(point, button)?;
                self.click(point, button)?;
            }
            Action::Drag { path, button } => self.drag(&path, button)?,
            Action::Scroll {
                x,
                y,
                scroll_x,
                scroll_y,
            } => {
                self.move_pointer(Point { x, y })?;
                self.session
                    .scroll(scroll_x.clamp(-10, 10), scroll_y.clamp(-10, 10));
                self.render();
            }
            Action::Keypress { keys } => {
                let (key, modifiers) = parse_keys(&keys)?;
                self.session.key(key, modifiers);
                self.render();
            }
            Action::Text { text } => {
                if text.len() > 4096 {
                    return Err("Text exceeds 4096 bytes".into());
                }
                self.session.text(&text);
                self.render();
            }
            Action::Wait { milliseconds } => {
                return self.wait(Duration::from_millis(u64::from(milliseconds.min(2000))));
            }
        }
        self.wait(Duration::from_millis(25))
    }

    fn move_pointer(&mut self, point: Point) -> Result<()> {
        if !on_screen(point) {
            return Err(format!(
                "Pointer ({}, {}) is outside the Paintrs window",
                point.x, point.y
            )
            .into());
        }
        self.pointer = point;
        self.session.move_pointer(point.x, point.y);
        self.render();
        Ok(())
    }

    fn click(&mut self, point: Point, button: Button) -> Result<()> {
        self.move_pointer(point)?;
        let right = matches!(button, Button::Right);
        self.session.button(right, true);
        self.render();
        let waited = self.wait(CLICK_HOLD);
        self.session.button(right, false);
        self.render();
        waited?;
        self.wait(CLICK_HOLD)
    }

    fn drag(&mut self, path: &[Point], button: Button) -> Result<()> {
        if !(2..=128).contains(&path.len()) || !path.iter().copied().all(on_screen) {
            return Err("A drag needs 2 to 128 points within the Paintrs window".into());
        }
        self.move_pointer(path[0])?;
        let right = matches!(button, Button::Right);
        self.session.button(right, true);
        self.render();
        let result = path[1..].iter().try_for_each(|&end| self.stroke_to(end));
        self.session.button(right, false);
        self.render();
        result
    }

    fn stroke_to(&mut self, end: Point) -> Result<()> {
        let start = self.pointer;
        let dx = end.x - start.x;
        let dy = end.y - start.y;
        let steps = (dx.unsigned_abs().max(dy.unsigned_abs()).div_ceil(20).max(1)) as i32;
        for step in 1..=steps {
            self.move_pointer(Point {
                x: start.x + dx * step / steps,
                y: start.y + dy * step / steps,
            })?;
            self.wait(Duration::from_millis(9))?;
        }
        Ok(())
    }

    fn smoke_test(&mut self) -> Result<()> {
        self.cue("I am drawing a line in Paintrs.");
        self.click(Point { x: 22, y: 151 }, Button::Left)?;
        self.drag(
            &[
                Point { x: 180, y: 145 },
                Point { x: 280, y: 100 },
                Point { x: 380, y: 145 },
                Point { x: 280, y: 195 },
                Point { x: 180, y: 145 },
            ],
            Button::Left,
        )
    }

    fn finish(mut self, run: &Run) -> Result<()> {
        self.session.finish(&run.output.join("drawing.png"))?;
        self.session.move_pointer(-1, -1);
        self.render();
        let video_duration = self.start.elapsed().as_secs_f64();
        let audio = crate::narration::synthesize(&self.cues)?;
        let mut frame = self.last_frame.unwrap_or(0) + 1;
        let tail =
            (video_duration.max(audio.duration()) * f64::from(FPS)).ceil() as u64 + u64::from(FPS);
        while frame <= tail {
            self.video.write(self.session.pixels(), frame)?;
            frame += 1;
        }
        self.video.finish(audio)?;
        std::fs::rename(
            run.output.join(".drawing.mp4"),
            run.output.join("drawing.mp4"),
        )?;
        Ok(())
    }
}

fn on_screen(point: Point) -> bool {
    (0..WIDTH as i32).contains(&point.x) && (0..HEIGHT as i32).contains(&point.y)
}

fn parse_keys(keys: &[String]) -> Result<(Key, Modifiers)> {
    let (key, modifiers) = keys.split_last().ok_or("A keypress requires a key")?;
    if modifiers.len() > 4 {
        return Err("A keypress can use at most four modifier keys".into());
    }
    let mut mask = Modifiers::empty();
    for modifier in modifiers {
        mask |= match modifier.to_ascii_uppercase().as_str() {
            "CTRL" | "CONTROL" => Modifiers::CONTROL,
            "SHIFT" => Modifiers::SHIFT,
            "ALT" | "OPTION" => Modifiers::ALT,
            "CMD" | "SUPER" | "META" => Modifiers::SUPER,
            _ => return Err(format!("Unknown modifier {modifier:?}").into()),
        };
    }
    let named = match key.to_ascii_uppercase().as_str() {
        "ESC" | "ESCAPE" => NamedKey::Escape,
        "RETURN" | "ENTER" => NamedKey::Enter,
        "BACKSPACE" => NamedKey::Backspace,
        "DEL" | "DELETE" => NamedKey::Delete,
        "TAB" => NamedKey::Tab,
        "SPACE" => NamedKey::Space,
        "UP" | "ARROWUP" => NamedKey::ArrowUp,
        "DOWN" | "ARROWDOWN" => NamedKey::ArrowDown,
        "LEFT" | "ARROWLEFT" => NamedKey::ArrowLeft,
        "RIGHT" | "ARROWRIGHT" => NamedKey::ArrowRight,
        "HOME" => NamedKey::Home,
        "END" => NamedKey::End,
        "PAGEUP" => NamedKey::PageUp,
        "PAGEDOWN" => NamedKey::PageDown,
        "F1" => NamedKey::F1,
        "F4" => NamedKey::F4,
        "F10" => NamedKey::F10,
        "F11" => NamedKey::F11,
        _ if key.chars().count() == 1 => return Ok((Key::Character(key.as_str().into()), mask)),
        _ => return Err(format!("Unsupported key {key:?}").into()),
    };
    Ok((Key::Named(named), mask))
}
