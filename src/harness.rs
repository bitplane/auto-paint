use crate::{Result, Run, agy, claude, codex};
use clap::ValueEnum;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, ValueEnum)]
pub enum Harness {
    Codex,
    Claude,
    Agy,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

impl Harness {
    pub fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Agy => "agy",
        }
    }

    pub fn resolve_model(
        self,
        directory: &Path,
        requested: Option<&str>,
        effort: Option<Effort>,
    ) -> Result<String> {
        let (family, fallback, models) = match self {
            Self::Codex => ("terra", "gpt-6.1-terra", codex::models(directory)),
            Self::Claude => ("haiku", "haiku", claude::models(directory)),
            Self::Agy => {
                let models = agy::models(directory)?;
                let model = select_agy_model(&models, requested, effort)?;
                println!("Model: {} (agy)", model.id);
                return Ok(model.id.clone());
            }
        };
        let query = requested.unwrap_or(family);
        let matched = match models {
            Ok(models) => match_model(models.iter(), query).map(|model| model.id.clone()),
            Err(error) => {
                eprintln!("Could not list {} models: {error}", self.name());
                None
            }
        };
        let selected = matched.unwrap_or_else(|| requested.unwrap_or(fallback).to_owned());
        println!("Model: {selected} ({})", self.name());
        Ok(selected)
    }

    pub fn check_login(self) -> Result<()> {
        match self {
            Self::Codex => codex::check_login(),
            Self::Claude => claude::check_login(),
            Self::Agy => Ok(()),
        }
    }

    pub fn start(self, run: &Run, address: SocketAddr, token: &str) -> Result<Process> {
        let prompt = instructions(run);
        match self {
            Self::Codex => codex::start(run, address, token, &prompt),
            Self::Claude => claude::start(run, address, token, &prompt),
            Self::Agy => agy::start(run, address, token, &prompt),
        }
    }

    pub fn structured_reply(
        self,
        directory: &Path,
        model: &str,
        effort: Option<Effort>,
        schema: &Path,
        prompt: &str,
    ) -> Result<(String, Option<Value>)> {
        let mut process = match self {
            Self::Codex => codex::start_selection(directory, model, effort, schema, prompt),
            Self::Claude => claude::start_selection(directory, model, effort, schema, prompt),
            Self::Agy => agy::start_selection(directory, model, effort, schema, prompt),
        }?;
        let deadline = Instant::now() + Duration::from_secs(90);
        let mut message = None;
        let mut usage = None;
        let mut error = None;
        loop {
            let status = process.status()?;
            for event in process.events.try_iter() {
                match event {
                    Event::Message(text) => message = Some(text),
                    Event::Usage(value) => usage = Some(value),
                    Event::Error(text) => error = Some(text),
                    Event::Tool { .. } | Event::Model(_) => {}
                }
            }
            if let Some(status) = status {
                if !status.success() {
                    return Err(error
                        .unwrap_or_else(|| format!("{} exited with {status}", self.name()))
                        .into());
                }
                return Ok((
                    message.ok_or("The harness returned no subject suggestions")?,
                    usage,
                ));
            }
            if Instant::now() >= deadline {
                return Err("Timed out waiting for subject suggestions".into());
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn save_usage(self, path: &Path, model: &str, usage: Value) -> Result<()> {
        let report = json!({"backend": self.name(), "model": model, "usage": usage});
        let mut data = serde_json::to_vec_pretty(&report)?;
        data.push(b'\n');
        std::fs::write(path, data)?;
        Ok(())
    }
}

pub struct Model {
    pub id: String,
    pub display_name: String,
}

fn match_model<'a>(
    models: impl DoubleEndedIterator<Item = &'a Model> + Clone,
    query: &str,
) -> Option<&'a Model> {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return None;
    }
    models
        .clone()
        .find(|model| model.id.eq_ignore_ascii_case(&query))
        .or_else(|| {
            let query = search_text(&query);
            if query.is_empty() {
                return None;
            }
            models
                .rev()
                .filter(|model| {
                    search_text(&model.id).contains(&query)
                        || search_text(&model.display_name).contains(&query)
                })
                .max_by_key(|model| {
                    version_numbers(&model.id).or_else(|| version_numbers(&model.display_name))
                })
        })
}

fn search_text(text: &str) -> String {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn select_agy_model<'a>(
    models: &'a [Model],
    requested: Option<&str>,
    effort: Option<Effort>,
) -> Result<&'a Model> {
    let eligible = models.iter().filter(|model| {
        effort.is_none_or(|effort| {
            let (_, suffix) = split_model_effort(&model.id);
            suffix.is_none_or(|suffix| suffix == effort.as_str())
        })
    });
    let choice = match requested {
        Some(query) => {
            let normalized = search_text(query);
            let (family, suffix) = split_model_effort(&normalized);
            let query = if effort.is_some() && suffix.is_some() {
                family
            } else {
                query
            };
            match_model(eligible, query)
        }
        None => match_model(eligible.clone(), "gemini-3.8-flash-medium")
            .or_else(|| match_model(eligible.clone(), "gemini-3.8-flash"))
            .or_else(|| {
                eligible
                    .rev()
                    .filter(|model| {
                        model.id.starts_with("gemini-")
                            && model.id.split('-').any(|word| word == "flash")
                    })
                    .max_by_key(|model| version_numbers(&model.id))
            }),
    };
    choice.ok_or_else(|| {
        let names: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
        let query = requested.unwrap_or("Gemini Flash");
        let effort = effort.map_or_else(String::new, |effort| {
            format!(" with {} effort", effort.as_str())
        });
        format!(
            "No Antigravity model matching {query:?} is available{effort}. Choose --model from: {}",
            names.join(", ")
        )
        .into()
    })
}

fn split_model_effort(model: &str) -> (&str, Option<&str>) {
    if let Some((family, suffix)) = model.rsplit_once(['-', ' '])
        && Effort::from_str(suffix, false).is_ok()
    {
        return (family, Some(suffix));
    }
    (model, None)
}

fn version_numbers(name: &str) -> Option<Vec<u64>> {
    let numbers: Vec<_> = name
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|number| number.parse().ok())
        .collect();
    (!numbers.is_empty()).then_some(numbers)
}

pub struct ModelProbe {
    process: Process<Value>,
    deadline: Instant,
}

impl ModelProbe {
    pub fn open(mut command: Command) -> Result<Self> {
        command.stderr(Stdio::null());
        Ok(Self {
            process: Process::open(command, |value| vec![value.clone()])?,
            deadline: Instant::now() + Duration::from_secs(10),
        })
    }

    pub fn send(&mut self, request: Value) -> Result<()> {
        let stdin = self.process.child.stdin.as_mut().expect("piped stdin");
        serde_json::to_writer(&mut *stdin, &request)?;
        stdin.write_all(b"\n")?;
        Ok(())
    }

    pub fn response(&self, matches: impl Fn(&Value) -> bool) -> Result<Value> {
        loop {
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("Timed out listing available models".into());
            }
            let value = self.process.events.recv_timeout(remaining)?;
            if matches(&value) {
                return Ok(value);
            }
        }
    }
}

pub fn command(name: &str) -> Command {
    #[cfg(windows)]
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for ext in ["exe", "cmd"] {
                let candidate = dir.join(format!("{name}.{ext}"));
                if candidate.is_file() {
                    return Command::new(candidate);
                }
            }
        }
    }
    Command::new(name)
}

pub enum Event {
    Message(String),
    Model(String),
    Usage(Value),
    Tool { name: String, status: String },
    Error(String),
}

pub struct Process<E = Event> {
    child: Child,
    pub events: Receiver<E>,
    reader: Option<thread::JoinHandle<()>>,
    agy_config: Option<agy::WorkspaceConfig>,
}

impl<E: Send + 'static> Process<E> {
    pub fn spawn(command: Command, input: &[u8], decode: fn(&Value) -> Vec<E>) -> Result<Self> {
        let mut process = Self::open(command, decode)?;
        let mut stdin = process.child.stdin.take().expect("piped stdin");
        stdin.write_all(input)?;
        Ok(process)
    }

    fn open(command: Command, decode: fn(&Value) -> Vec<E>) -> Result<Self> {
        Self::open_lines(command, move |line| {
            serde_json::from_str(line).map_or_else(|_| Vec::new(), |value| decode(&value))
        })
    }

    pub fn open_lines(
        mut command: Command,
        decode: impl Fn(&str) -> Vec<E> + Send + 'static,
    ) -> Result<Self> {
        command.stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut child = command.spawn()?;
        let stdout = child.stdout.take().expect("piped stdout");
        let (send, events) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout)
                .lines()
                .map_while(std::result::Result::ok)
            {
                for event in decode(&line) {
                    if send.send(event).is_err() {
                        return;
                    }
                }
            }
        });
        Ok(Self {
            child,
            events,
            reader: Some(reader),
            agy_config: None,
        })
    }

    pub fn status(&mut self) -> Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        if status.is_some()
            && let Some(reader) = self.reader.take()
        {
            let _ = reader.join();
        }
        Ok(status)
    }
}

impl Process {
    pub fn with_agy_config(mut self, config: agy::WorkspaceConfig) -> Self {
        self.agy_config = Some(config);
        self
    }
}

impl<E> Drop for Process<E> {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn instructions(run: &Run) -> String {
    let narration = if run.options.narrate {
        " At major drawing steps, use the act call's narration field for one short first-person line \
         about what you're doing in Paintrs. Use the latest screenshot. When a screenshot or tool result \
         shows a mistake, say what you got wrong and how you're fixing it. Skip routine clicks. \
         Once the picture is finished, look at the final screenshot and call the Paintrs review tool \
         exactly once. Deliver one short, mercilessly funny sentence about a specific flaw you can \
         point to in your own drawing. Make the drawing the butt of the joke. End the turn after the review."
    } else {
        ""
    };
    format!(
        "Draw a picture using only the Paintrs tools. Start with a screenshot. \
         Click and drag in the actual editor to create the image. Treat screenshots as window-local coordinates. \
         Leave the finished picture visible. The app saves it automatically when you finish.{narration}\n\n\
         Drawing request: {}\n",
        run.prompt,
    )
}

#[cfg(test)]
mod tests {
    use super::{Effort, Model, match_model, select_agy_model};

    #[test]
    fn matches_exact_ids_and_compares_family_versions_numerically() {
        let models = [
            ("gpt-6.2-sol", "GPT-6.2 Sol"),
            ("gpt-6.10-sol", "GPT-6.10 Sol"),
            ("gpt-7-terra", "GPT-7 Terra"),
            ("claude-haiku-4-5-20251001", "Haiku 4.5"),
            ("haiku", "Haiku 5.5"),
        ]
        .map(|(id, display_name)| Model {
            id: id.to_owned(),
            display_name: display_name.to_owned(),
        });
        let matches = |query| match_model(models.iter(), query).map(|model| model.id.as_str());
        assert_eq!(matches("sol"), Some("gpt-6.10-sol"));
        assert_eq!(matches("gpt-6.2-sol"), Some("gpt-6.2-sol"));
        assert_eq!(matches("terra"), Some("gpt-7-terra"));
        assert_eq!(matches("Haiku 5"), Some("haiku"));
        assert_eq!(matches("missing"), None);
    }

    #[test]
    fn agy_chooses_only_from_the_account_catalogue() {
        let models = [
            "gemini-3.8-flash-high",
            "gemini-3.8-flash-medium",
            "gemini-3.8-flash-low",
            "gemini-3.7-flash-medium",
            "claude-sonnet-4-6",
        ]
        .map(|id| Model {
            id: id.to_owned(),
            display_name: id.to_owned(),
        });
        let select =
            |account: &[Model], query| select_agy_model(account, query, None).unwrap().id.clone();
        assert_eq!(select(&models, None), "gemini-3.8-flash-medium");
        assert_eq!(select(&models, Some("flash")), "gemini-3.8-flash-high");
        assert_eq!(
            select(&models, Some("Gemini 3.8 Flash Medium")),
            "gemini-3.8-flash-medium"
        );
        assert_eq!(select(&models[3..], None), "gemini-3.7-flash-medium");
        assert!(select_agy_model(&models[3..], Some("gemini-3.8-flash-medium"), None).is_err());
        assert_eq!(
            select_agy_model(&models, Some("Gemini 3.8 Flash (High)"), Some(Effort::Low))
                .unwrap()
                .id,
            "gemini-3.8-flash-low"
        );
        assert!(select_agy_model(&models, Some("flash"), Some(Effort::Max)).is_err());
    }
}
