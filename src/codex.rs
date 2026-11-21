use crate::{Result, Run};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;

pub fn check_login() -> Result<()> {
    let output = command()
        .args(["login", "status"])
        .output()
        .map_err(|err| {
            format!("Could not run Codex CLI. Install it and run codex login. ({err})")
        })?;
    let logged_in = [&output.stdout, &output.stderr].iter().any(|stream| {
        String::from_utf8_lossy(stream)
            .to_ascii_lowercase()
            .contains("logged in using chatgpt")
    });
    if !output.status.success() || !logged_in {
        return Err("Run codex login and sign in with ChatGPT before starting Auto Paint.".into());
    }
    Ok(())
}

fn command() -> Command {
    let mut command = Command::new(executable());
    command
        .env_remove("OPENAI_API_KEY")
        .env_remove("CODEX_API_KEY");
    command
}

#[cfg(not(windows))]
fn executable() -> &'static str {
    "codex"
}

#[cfg(windows)]
fn executable() -> std::path::PathBuf {
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in ["codex.exe", "codex.cmd"] {
                let path = dir.join(name);
                if path.is_file() {
                    return path;
                }
            }
        }
    }
    "codex.exe".into()
}

pub enum Event {
    Message(String),
    Usage(Value),
    Tool { name: String, status: String },
    Error(String),
}

fn event(line: &str) -> Option<Event> {
    let value = serde_json::from_str::<Value>(line).ok()?;
    let kind = value["type"].as_str()?;
    let item = &value["item"];
    match kind {
        "turn.completed" => Some(Event::Usage(value.get("usage")?.clone())),
        "item.completed" if item["type"] == "agent_message" => {
            Some(Event::Message(item["text"].as_str()?.to_owned()))
        }
        "item.started" | "item.completed" if item["type"] == "mcp_tool_call" => Some(Event::Tool {
            name: item["tool"].as_str()?.to_owned(),
            status: item["status"].as_str().unwrap_or("started").to_owned(),
        }),
        "turn.failed" => Some(Event::Error(value["error"]["message"].as_str()?.to_owned())),
        "error" => Some(Event::Error(value["message"].as_str()?.to_owned())),
        _ => None,
    }
}

pub struct Process {
    child: Child,
    pub events: Receiver<Event>,
    reader: Option<thread::JoinHandle<()>>,
}

impl Process {
    pub fn start(run: &Run, address: std::net::SocketAddr, token: &str) -> Result<Self> {
        let executable = std::env::current_exe()?;
        let executable = executable
            .to_str()
            .ok_or("Auto Paint's executable path must be UTF-8")?;
        let server = "mcp_servers.paintrs";
        let mut command = command();
        command
            .args([
                "-a",
                "never",
                "exec",
                "--json",
                "--sandbox",
                "read-only",
                "--disable",
                "shell_tool",
                "--ignore-user-config",
                "--skip-git-repo-check",
                "--ephemeral",
                "--color",
                "never",
                "-m",
                &run.options.model,
                "-c",
                "model_reasoning_effort=\"low\"",
                "-c",
                &format!("{server}.command={}", toml_string(executable)),
                "-c",
                &format!(
                    "{server}.args=[\"--mcp-stdio\",{}]",
                    toml_string(&address.to_string()),
                ),
                "-c",
                &format!("{server}.env_vars=[\"AUTO_PAINT_MCP_TOKEN\"]"),
                "-c",
                &format!("{server}.required=true"),
                "-c",
                &format!("{server}.tools.act.approval_mode=\"approve\""),
            ])
            .env("AUTO_PAINT_MCP_TOKEN", token)
            .current_dir(&run.output)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        if let Some(path) = &run.reference {
            command.arg("--image").arg(path);
        }
        command.args(["--", "-"]);

        let mut child = command.spawn()?;
        let stdout = child.stdout.take().expect("piped stdout");
        let (send, events) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout)
                .lines()
                .map_while(std::result::Result::ok)
            {
                if let Some(event) = event(&line)
                    && send.send(event).is_err()
                {
                    break;
                }
            }
        });
        let mut process = Self {
            child,
            events,
            reader: Some(reader),
        };
        let mut stdin = process.child.stdin.take().expect("piped stdin");
        stdin.write_all(instructions(run).as_bytes())?;
        Ok(process)
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

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn toml_string(value: &str) -> String {
    // JSON's spaces, slashes and escaped quotes are also accepted in TOML basic strings.
    json!(value).to_string()
}

fn instructions(run: &Run) -> String {
    let narration = if run.options.narrate {
        " At each major drawing step, include an optional one-sentence narration in the act call describing what the viewer is about to see."
    } else {
        ""
    };
    format!(
        "Draw a picture using only the Paintrs screenshot and act tools. Start with a screenshot. \
         Click and drag in the actual editor to create the image. Treat screenshots as window-local coordinates. \
         Leave the finished picture visible. The app saves it automatically when you finish.{narration}\n\n\
         Drawing request: {}\n",
        run.prompt,
    )
}

pub fn save_usage(path: &Path, model: &str, usage: Value) -> Result<()> {
    let report = json!({"backend": "codex", "model": model, "usage": usage});
    let mut data = serde_json::to_vec_pretty(&report)?;
    data.push(b'\n');
    std::fs::write(path, data)?;
    Ok(())
}
