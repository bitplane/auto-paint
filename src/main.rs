mod agy;
mod claude;
mod codex;
mod drawing;
mod evaluation;
mod grok;
mod harness;
mod mcp;
mod muse;
mod narration;
mod opencode;
mod overlay;
mod playback;
mod selection;
mod video;
mod watch;

use clap::Parser;
use harness::{Effort, Harness};
use std::error::Error;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const WIDTH: u32 = 1000;
const HEIGHT: u32 = 700;
const FPS: u32 = 15;

#[derive(Parser)]
#[command(version, about)]
struct Options {
    /// What to draw.
    #[arg(long)]
    prompt: Option<String>,
    /// Image to use as a reference.
    #[arg(long, value_name = "IMAGE")]
    reference: Option<PathBuf>,
    /// New directory for the recording and final drawing.
    #[arg(long, value_name = "DIR")]
    output: Option<PathBuf>,
    /// Record with no visible window.
    #[arg(long)]
    no_watch: bool,
    /// Shorten silent waits for the model in the saved video.
    #[arg(long)]
    trim_waits: bool,
    /// Record without voiceover or subtitles.
    #[arg(long = "no-narrate", action = clap::ArgAction::SetFalse)]
    narrate: bool,
    // Accept the former opt-in flag for existing scripts.
    #[arg(long = "narrate", hide = true, conflicts_with = "narrate")]
    _legacy_narrate: bool,
    /// Maximum number of screenshot and action batches.
    #[arg(long, default_value_t = 45, value_parser = clap::value_parser!(u32).range(1..=1000))]
    turns: u32,
    /// CLI to drive the drawing.
    #[arg(long, value_enum, default_value = "codex")]
    harness: Harness,
    /// Model name or family. Defaults depend on the harness.
    #[arg(long)]
    model: Option<String>,
    /// Reasoning effort for the selected model.
    #[arg(long, value_enum)]
    effort: Option<Effort>,
    /// Record a short local drawing without calling a model.
    #[arg(long)]
    smoke_test: bool,
    #[arg(long, hide = true, value_name = "ADDRESS")]
    mcp_stdio: Option<SocketAddr>,
}

struct Run {
    options: Options,
    model: String,
    output: PathBuf,
    prompt: String,
    reference: Option<PathBuf>,
    opening: Option<String>,
}

impl Run {
    fn new(options: Options) -> Result<Self> {
        let root = std::env::current_dir()?;
        let reference = options
            .reference
            .as_ref()
            .map(std::fs::canonicalize)
            .transpose()?;
        if let Some(path) = &reference
            && (reference_media_type(path).is_none() || !path.is_file())
        {
            return Err("--reference expects a PNG, JPEG, WebP, or static GIF file".into());
        }
        let model = if options.smoke_test {
            String::new()
        } else {
            options.harness.check_login()?;
            options
                .harness
                .resolve_model(&root, options.model.as_deref(), options.effort)?
        };

        let output = match &options.output {
            Some(path) => {
                let path = root.join(path);
                std::fs::create_dir_all(path.parent().ok_or("Output needs a parent directory")?)?;
                std::fs::create_dir(&path)
                    .map_err(|err| format!("Creating {}: {err}", path.display()))?;
                path
            }
            None => unique_output(&root)?,
        };
        println!("Output: {}", output.display());
        let random_subject = !options.smoke_test
            && options.prompt.is_none()
            && options.output.is_none()
            && reference.is_none();
        let (prompt, opening) = if random_subject {
            let (subject, opening) =
                selection::choose(&output, options.harness, &model, options.effort)?;
            (
                format!("Draw {subject}. Use bold shapes and a clear composition."),
                opening,
            )
        } else {
            (
                options.prompt.clone().unwrap_or_else(|| {
                    default_prompt(options.output.as_deref(), reference.as_deref())
                }),
                None,
            )
        };
        Ok(Self {
            options,
            model,
            output,
            prompt,
            reference,
            opening,
        })
    }

    fn model(&self) -> &str {
        &self.model
    }
}

fn reference_media_type(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

fn unique_output(root: &Path) -> Result<PathBuf> {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    for suffix in 0..100 {
        let name = if suffix == 0 {
            format!("auto-paint-{timestamp}")
        } else {
            format!("auto-paint-{timestamp}-{suffix}")
        };
        let path = root.join(name);
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err.into()),
        }
    }
    Err("Could not choose an unused output directory".into())
}

fn default_prompt(output: Option<&Path>, reference: Option<&Path>) -> String {
    let hint = |path: &Path| {
        path.file_stem()
            .map(|name| name.to_string_lossy().into_owned())
    };
    let output = output.and_then(hint);
    let reference_name = reference.and_then(hint);
    let subject = match (output, reference_name) {
        (Some(output), Some(reference)) => {
            format!(" The output name is {output:?} and the reference name is {reference:?}.")
        }
        (Some(output), None) => format!(" The output name is {output:?}."),
        (None, Some(reference)) => format!(" The reference name is {reference:?}."),
        (None, None) => String::new(),
    };
    if reference.is_some() {
        format!(
            "Redraw the attached reference in Paintrs as a clean, stylized drawing. Preserve its main subject, composition, and recognizable colors.{subject}"
        )
    } else {
        format!(
            "Choose a subject that fits the output name if it suggests one. Otherwise, pick a colorful, recognizable subject yourself and draw it in Paintrs.{subject}"
        )
    }
}

fn run() -> Result<()> {
    let options = Options::parse();
    if let Some(address) = options.mcp_stdio {
        return mcp::relay(address);
    }
    let run = Run::new(options)?;
    if run.options.no_watch {
        drawing::run(run, Arc::new(AtomicBool::new(false)), None)
    } else {
        watch::run(run)
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("auto-paint: {error}");
            ExitCode::FAILURE
        }
    }
}
