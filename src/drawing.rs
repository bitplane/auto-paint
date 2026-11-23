use crate::harness::{Event, Harness};
use crate::mcp::{Action, Batch, Button, Command, Point, Server};
use crate::narration::Narration;
use crate::overlay::Overlays;
use crate::video::Video;
use crate::{FPS, HEIGHT, Result, Run, WIDTH, evaluation};
use paintrs::automation::{Modifiers, NamedKey, Session};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use winit::keyboard::Key;

const FRAME_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / FPS as u64);
const BATCH_LIMIT: Duration = Duration::from_secs(40);
const CLICK_HOLD: Duration = Duration::from_millis(35);
const WAIT_TAIL: Duration = Duration::from_millis(500);

pub enum Preview<'a> {
    Frame(&'a [u32]),
    Speech(&'a [i16]),
}

pub fn run(
    run: Run,
    cancelled: Arc<AtomicBool>,
    preview: Option<&mut dyn FnMut(Preview<'_>)>,
) -> Result<()> {
    let mut drawing = Drawing::new(&run, cancelled, preview)?;
    let result = if run.options.smoke_test {
        drawing.smoke_test()
    } else {
        drawing.run_harness(&run)
    };
    let output = drawing.finish(&run, result.is_ok());
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

struct Drawing<'a> {
    session: Session,
    video: Video,
    preview: Option<&'a mut dyn FnMut(Preview<'_>)>,
    cancelled: Arc<AtomicBool>,
    start: Option<Instant>,
    trim_waits: bool,
    wait_cutoff: Option<Instant>,
    last_frame: Option<u64>,
    batch_deadline: Option<Instant>,
    pointer: Point,
    narration: Option<Narration>,
    subject: Option<String>,
    overlays: Option<Overlays>,
    composited: Vec<u32>,
}

impl<'a> Drawing<'a> {
    fn new(
        run: &Run,
        cancelled: Arc<AtomicBool>,
        preview: Option<&'a mut dyn FnMut(Preview<'_>)>,
    ) -> Result<Self> {
        let video = Video::new(&run.output.join(".drawing.mp4"), run.options.narrate)?;
        let session = Session::new(WIDTH, HEIGHT)?;
        let narration = run.options.narrate.then(Narration::new).transpose()?;
        let (model, effort) = run.overlay_model(run.model());
        let overlays = (run.options.narrate || !run.model().is_empty())
            .then(|| Overlays::new(model, effort))
            .transpose()?;
        let mut drawing = Self {
            session,
            video,
            preview,
            cancelled,
            start: None,
            trim_waits: !run.options.real_time,
            wait_cutoff: None,
            last_frame: None,
            batch_deadline: None,
            pointer: Point { x: -1, y: -1 },
            narration,
            subject: None,
            overlays,
            composited: Vec::new(),
        };
        drawing.render();
        Ok(drawing)
    }

    fn render(&mut self) {
        let pixels = self.session.render();
        if (self.start.is_none()
            || self
                .wait_cutoff
                .is_some_and(|cutoff| Instant::now() >= cutoff))
            && let Some(preview) = &mut self.preview
        {
            let pixels = match &mut self.overlays {
                Some(overlays) => overlays.compose(&mut self.composited, pixels, None),
                None => pixels,
            };
            preview(Preview::Frame(pixels));
        }
    }

    fn start_recording(&mut self) -> Result<Instant> {
        if let Some(start) = self.start {
            return Ok(start);
        }
        let start = Instant::now();
        self.start = Some(start);
        self.sample()?;
        Ok(start)
    }

    fn sample(&mut self) -> Result<()> {
        let now = Instant::now();
        if self.session.animate(now) {
            self.render();
        }
        let Some(start) = self.start else {
            return Ok(());
        };
        let recorded_now = self.wait_cutoff.map_or(now, |cutoff| now.min(cutoff));
        let frame = if self.last_frame.is_none() {
            0
        } else {
            (recorded_now.duration_since(start).as_nanos() * u128::from(FPS) / 1_000_000_000) as u64
        };
        if self.last_frame.is_none_or(|last| frame > last) {
            self.write_frame(frame)?;
            self.last_frame = Some(frame);
        }
        Ok(())
    }

    fn check_active(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Err("Drawing cancelled".into());
        }
        if self
            .batch_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err("Action batch exceeded its time limit".into());
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

    fn run_harness(&mut self, run: &Run) -> Result<()> {
        let server = Server::bind()?;
        let (address, token) = (server.address, server.token.clone());
        let harness = run.options.harness;
        let reference = match harness {
            Harness::Agy => run.reference.clone(),
            _ => None,
        };
        let requests = server.start(run.options.turns, reference)?;
        let mut process = harness.start(run, address, &token)?;
        let mut usage = None;
        loop {
            self.check_active()?;
            self.sample()?;
            match requests.recv_timeout(Duration::from_millis(12)) {
                Ok(request) => {
                    let _ = request
                        .reply
                        .send(self.command(request.command, run.opening.as_deref()));
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    thread::sleep(Duration::from_millis(12));
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
            let status = process.status()?;
            for event in process.events.try_iter() {
                match event {
                    Event::Message(text) => println!("{text}"),
                    Event::Tool { name, status } => println!("Paintrs {name} ({status})"),
                    Event::Usage(report) => usage = Some(report),
                    Event::Error(text) => eprintln!("{text}"),
                    Event::Model(model) => {
                        if let Some(overlays) = &mut self.overlays {
                            let (model, effort) = run.overlay_model(&model);
                            overlays.set_model(model, effort);
                            self.render();
                        }
                    }
                }
            }
            if let Some(status) = status {
                if let Some(usage) = usage {
                    harness.save_usage(&run.output.join("usage.json"), run.model(), usage)?;
                }
                if !status.success() {
                    return Err(format!("{} exited with {status}", harness.name()).into());
                }
                return Ok(());
            }
        }
    }

    fn command(
        &mut self,
        command: Command,
        opening: Option<&str>,
    ) -> std::result::Result<Vec<u8>, String> {
        match command {
            Command::Screenshot => self.session.screenshot(),
            Command::Act(batch) => {
                self.end_model_wait(Instant::now());
                let response = self
                    .batch(batch, opening)
                    .map_err(|error| error.to_string())
                    .and_then(|()| self.session.screenshot());
                self.begin_model_wait(Instant::now());
                response
            }
        }
    }

    fn begin_model_wait(&mut self, now: Instant) {
        if self.trim_waits
            && let Some(start) = self.start
        {
            let speech_end = self
                .narration
                .as_ref()
                .map_or(start, |narration| start + narration.subtitled_duration());
            self.wait_cutoff = Some(now.max(speech_end) + WAIT_TAIL);
        }
    }

    fn end_model_wait(&mut self, now: Instant) {
        if let Some(cutoff) = self.wait_cutoff.take()
            && let Some(start) = &mut self.start
        {
            *start += now.saturating_duration_since(cutoff);
        }
    }

    fn batch(&mut self, batch: Batch, opening: Option<&str>) -> Result<()> {
        if !(1..=128).contains(&batch.actions.len()) {
            return Err("An action batch needs 1 to 128 actions".into());
        }
        let line = if self.start.is_none() {
            self.subject = batch
                .subject
                .as_deref()
                .map(str::trim)
                .filter(|subject| !subject.is_empty())
                .map(|subject| subject.chars().take(200).collect());
            opening.or(batch.narration.as_deref())
        } else {
            batch.narration.as_deref()
        };
        self.start_recording()?;
        if let Some(line) = line {
            self.cue(line);
        }
        self.batch_deadline = Some(Instant::now() + BATCH_LIMIT);
        let result = batch
            .actions
            .into_iter()
            .try_for_each(|action| self.act(action));
        self.batch_deadline = None;
        result
    }

    fn cue(&mut self, line: &str) {
        if let (Some(start), Some(narration)) = (self.start, &mut self.narration)
            && let Some(samples) = narration.add(start.elapsed(), line)
            && let Some(preview) = &mut self.preview
        {
            preview(Preview::Speech(samples));
        }
    }

    fn write_frame(&mut self, frame: u64) -> Result<()> {
        let subtitle = self
            .narration
            .as_ref()
            .and_then(|voice| voice.subtitle_at(frame));
        let pixels = self.session.pixels();
        let pixels = match &mut self.overlays {
            Some(overlays) => overlays.compose(&mut self.composited, pixels, subtitle),
            None => pixels,
        };
        if self.video.write(pixels, frame)?
            && let Some(preview) = &mut self.preview
        {
            preview(Preview::Frame(pixels));
        }
        Ok(())
    }

    fn act(&mut self, action: Action) -> Result<()> {
        self.check_active()?;
        match action {
            Action::Move { x, y } => self.move_pointer(Point { x, y })?,
            Action::Click { x, y, button } => self.click(Point { x, y }, button, false)?,
            Action::DoubleClick { x, y, button } => self.click(Point { x, y }, button, true)?,
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

    fn click(&mut self, point: Point, button: Button, double: bool) -> Result<()> {
        self.move_pointer(point)?;
        let right = matches!(button, Button::Right);
        self.session.press(right, double);
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
        self.session.press(right, false);
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
        self.start_recording()?;
        self.cue("I am drawing a line in Paintrs.");
        self.click(Point { x: 22, y: 151 }, Button::Left, false)?;
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

    fn finish(mut self, run: &Run, completed: bool) -> Result<()> {
        self.end_model_wait(Instant::now());
        let picture = run.output.join("drawing.png");
        self.session.finish(&picture)?;
        self.session.move_pointer(-1, -1);
        self.render();
        let review = if completed
            && self.narration.is_some()
            && self.start.is_some()
            && !run.options.smoke_test
            && !self.cancelled.load(Ordering::Relaxed)
        {
            self.sample()?;
            let paused = Instant::now();
            let review =
                evaluation::review(run, self.subject.as_deref(), &picture, &self.cancelled);
            self.start = self.start.map(|start| start + paused.elapsed());
            match review {
                Ok(line) => {
                    println!("Final review {line:?}");
                    Some(line)
                }
                Err(error) => {
                    eprintln!("Could not evaluate the drawing: {error}");
                    None
                }
            }
        } else {
            None
        };
        let start = self.start_recording()?;
        if let (Some(review), Some(narration)) = (review.as_deref(), &mut self.narration)
            && let Some(samples) = narration.add_review(start.elapsed(), review)
            && let Some(preview) = &mut self.preview
        {
            preview(Preview::Speech(samples));
        }
        let audio_duration = self.narration.as_ref().map_or(0.0, Narration::duration);
        if self.preview.is_some() && audio_duration > 0.0 {
            // Keep the window and output device alive while the final cue plays out.
            let stop = start + Duration::from_secs_f64(audio_duration) + FRAME_INTERVAL * 2;
            loop {
                let remaining = stop.saturating_duration_since(Instant::now());
                if remaining.is_zero() || self.cancelled.load(Ordering::Relaxed) {
                    break;
                }
                self.sample()?;
                thread::sleep(remaining.min(FRAME_INTERVAL));
            }
        }
        let video_duration = start.elapsed().as_secs_f64();
        let mut frame = self.last_frame.unwrap_or(0) + 1;
        let tail =
            (video_duration.max(audio_duration) * f64::from(FPS)).ceil() as u64 + u64::from(FPS);
        while frame <= tail {
            self.write_frame(frame)?;
            frame += 1;
        }
        self.video.finish(self.narration)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn a_model_wait_keeps_speech_and_a_short_pause() -> Result<()> {
        let workspace = crate::harness::Workspace::new()?;
        let run = Run {
            options: crate::Options::parse_from(["auto-paint"]),
            model: String::new(),
            output: workspace.0.clone(),
            prompt: String::new(),
            reference: None,
            opening: None,
        };
        let mut drawing = Drawing::new(&run, Arc::new(AtomicBool::new(false)), None)?;
        let start = Instant::now();
        drawing.start = Some(start);
        let narration = drawing.narration.as_mut().unwrap();
        assert!(
            narration
                .add(Duration::ZERO, "I'm painting the first shape.")
                .is_some()
        );
        let speech_end = start + narration.subtitled_duration();

        drawing.begin_model_wait(start);
        assert_eq!(drawing.wait_cutoff, Some(speech_end + WAIT_TAIL));
        drawing.end_model_wait(speech_end);
        assert_eq!(drawing.start, Some(start));

        drawing.begin_model_wait(start);
        let discarded = Duration::from_secs(10);
        drawing.end_model_wait(speech_end + WAIT_TAIL + discarded);
        assert_eq!(drawing.start, Some(start + discarded));
        assert_eq!(drawing.wait_cutoff, None);
        Ok(())
    }

    #[test]
    fn polygon_clicks_keep_their_intended_click_count() -> Result<()> {
        let output = crate::unique_output(&std::env::temp_dir())?;
        let run = Run {
            options: crate::Options::parse_from(["auto-paint", "--smoke-test", "--no-narrate"]),
            model: String::new(),
            output: output.clone(),
            prompt: String::new(),
            reference: None,
            opening: None,
        };
        let pixels = (|| -> Result<[u32; 3]> {
            let mut drawing = Drawing::new(&run, Arc::new(AtomicBool::new(false)), None)?;
            let point = |x, y| Point { x, y };
            let left = Button::Left;
            drawing.click(point(52, 235), left, false)?;
            drawing.click(point(35, 343), left, false)?;
            drawing.drag(&[point(170, 110), point(280, 110)], left)?;
            drawing.click(point(225, 209), left, false)?;
            drawing.click(point(225, 209), left, true)?;
            drawing.drag(&[point(340, 110), point(450, 110)], left)?;
            drawing.click(point(395, 209), left, true)?;
            drawing.drag(&[point(510, 110), point(620, 110)], left)?;
            drawing.click(point(620, 210), left, false)?;
            drawing.click(point(617, 212), left, false)?;
            drawing.click(point(510, 210), left, false)?;
            drawing.act(Action::Keypress {
                keys: vec!["ENTER".into()],
            })?;
            Ok([(225, 150), (395, 150), (530, 185)]
                .map(|(x, y)| drawing.session.pixels()[y * WIDTH as usize + x]))
        })();
        std::fs::remove_dir_all(output)?;
        assert_eq!(pixels?, [0; 3]);
        Ok(())
    }
}
