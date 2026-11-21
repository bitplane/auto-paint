mod codex;
mod drawing;
mod mcp;
mod narration;
mod video;
mod watch;

use clap::Parser;
use std::error::Error;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const DEFAULT_MODEL: &str = "gpt-6.1-terra";
const WIDTH: u32 = 1000;
const HEIGHT: u32 = 700;
const FPS: u32 = 15;

#[derive(Parser)]
#[command(version, about)]
struct Options {
    /// What Terra should draw.
    #[arg(long)]
    prompt: Option<String>,
    /// Image to show Terra as a reference.
    #[arg(long, value_name = "IMAGE")]
    reference: Option<PathBuf>,
    /// New directory for the recording and final drawing.
    #[arg(long, value_name = "DIR")]
    output: Option<PathBuf>,
    /// Record with no visible window.
    #[arg(long)]
    no_watch: bool,
    /// Add an offline English voiceover to the recording.
    #[arg(long)]
    narrate: bool,
    /// Maximum number of screenshot and action batches.
    #[arg(long, default_value_t = 45, value_parser = clap::value_parser!(u32).range(1..=1000))]
    turns: u32,
    /// Codex model to use.
    #[arg(long, default_value = DEFAULT_MODEL)]
    model: String,
    /// Record a short local drawing without calling Codex.
    #[arg(long)]
    smoke_test: bool,
    #[arg(long, hide = true, value_name = "ADDRESS")]
    mcp_stdio: Option<SocketAddr>,
}

struct Run {
    options: Options,
    output: PathBuf,
    prompt: String,
    reference: Option<PathBuf>,
}

impl Run {
    fn new(options: Options) -> Result<Self> {
        let root = std::env::current_dir()?;
        let reference = options
            .reference
            .as_ref()
            .map(std::fs::canonicalize)
            .transpose()?;
        if let Some(path) = &reference {
            let supported = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    matches!(
                        ext.to_ascii_lowercase().as_str(),
                        "png" | "jpg" | "jpeg" | "webp" | "gif"
                    )
                });
            if !supported || !path.is_file() {
                return Err("--reference expects a PNG, JPEG, WebP, or static GIF file".into());
            }
        }
        if !options.smoke_test {
            codex::check_login()?;
        }

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
        let prompt = options
            .prompt
            .clone()
            .unwrap_or_else(|| default_prompt(options.output.as_deref(), reference.as_deref()));
        println!("Output: {}", output.display());
        Ok(Self {
            options,
            output,
            prompt,
            reference,
        })
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
        drawing::run(run, Arc::new(AtomicBool::new(false)), |_| {})
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
