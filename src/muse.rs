use crate::harness::{self, Effort, Event, Model, Process, Query, Workspace};
use crate::{Result, Run};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::process::{Command, Stdio};

pub fn resolve_model(directory: &Path, requested: Option<&str>) -> Result<String> {
    let output = harness::command("muse")
        .args(["exec", "--list-models"])
        .current_dir(directory)
        .output()
        .map_err(|error| format!("Could not start Muse Code. Put muse on PATH: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("missing meta credentials") {
            return Err("Muse Code is signed out. Run muse login or set META_API_KEY.".into());
        }
        let detail = stderr
            .lines()
            .last()
            .unwrap_or("No diagnostic from Muse Code");
        return Err(format!(
            "muse exec --list-models exited with {}: {detail}",
            output.status
        )
        .into());
    }
    let models = model_catalogue(&String::from_utf8(output.stdout)?);
    let selected = if let Some(query) = requested {
        harness::match_model(models.iter(), query)
    } else {
        harness::match_model(
            models
                .iter()
                .filter(|model| !model.id.ends_with("-contributor")),
            "muse-spark",
        )
        .or_else(|| harness::match_model(models.iter(), "muse-spark"))
        .or_else(|| models.first())
    };
    selected.map(|model| model.id.clone()).ok_or_else(|| {
        let ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
        if let Some(query) = requested {
            format!(
                "No available Muse Code model matches {query:?}. Choose --model from: {}",
                ids.join(", ")
            )
            .into()
        } else {
            "Muse Code did not report any available models.".into()
        }
    })
}

fn model_catalogue(output: &str) -> Vec<Model> {
    output
        .lines()
        .map(str::trim)
        .filter(|id| {
            !id.is_empty()
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-._/".contains(&byte))
        })
        .map(|id| Model {
            id: id.to_owned(),
            display_name: id.to_owned(),
        })
        .collect()
}

pub fn start(run: &Run, address: SocketAddr, token: &str, prompt: &str) -> Result<Process> {
    let workspace = Workspace::new()?;
    let mut command = command(
        &workspace.0,
        run.model(),
        run.options.effort,
        run.reference.as_deref(),
    );
    let executable = std::env::current_exe()?;
    let executable = executable
        .to_str()
        .ok_or("Auto Paint's executable path must be UTF-8")?;
    let server = json!({
        "type": "stdio",
        "framing": "line_delimited_json",
        "required": true,
        "command": executable,
        "args": ["--mcp-stdio", address.to_string()],
        "env": {"AUTO_PAINT_MCP_TOKEN": "${AUTO_PAINT_MCP_TOKEN}"},
    });
    let settings = json!({"mcpServers": {"paintrs": server}});
    std::fs::write(
        workspace.0.join(".mcp.json"),
        serde_json::to_vec(&settings)?,
    )?;
    // Trust the generated workspace so Muse loads its MCP file for this run.
    command
        .arg("--trust-workspace")
        .args([
            "-c",
            r#"run.toolset=["mcp__paintrs__screenshot","mcp__paintrs__act"]"#,
        ])
        .arg("--disable-approval")
        .arg("--no-parallel-tool-calls")
        .env("AUTO_PAINT_MCP_TOKEN", token);
    spawn(command, workspace, prompt, true)
}

pub fn start_query(query: Query<'_>) -> Result<Process> {
    let workspace = Workspace::new()?;
    let mut command = command(&workspace.0, query.model, query.effort, query.image);
    command
        .args(["-c", "run.toolset=[]"])
        .arg("--output-schema")
        .arg(query.schema);
    spawn(command, workspace, query.prompt, false)
}

fn command(directory: &Path, model: &str, effort: Option<Effort>, image: Option<&Path>) -> Command {
    let mut command = harness::command("muse");
    command
        .current_dir(directory)
        .args([
            "exec",
            "--json",
            "--no-session-log",
            "--disable-shell",
            "--disable-write",
            "--disable-web-tools",
            "--disable-reminders",
            "--disable-request-user-input",
            "--no-foreign-personal-context",
            "--model",
            model,
        ])
        .arg("--workspace")
        .arg(directory);
    if let Some(effort) = effort {
        command.args(["--reasoning-effort", effort.as_str()]);
    }
    if let Some(image) = image {
        command.arg("--image").arg(image);
    }
    command
}

fn spawn(
    mut command: Command,
    workspace: Workspace,
    prompt: &str,
    drawing: bool,
) -> Result<Process> {
    command.args(["--", prompt]).stderr(Stdio::piped());
    let mut tools = HashMap::new();
    Ok(
        Process::spawn(command, &[], move |value| events(value, &mut tools))?
            .with_stderr_filter(move |line| show_stderr(line, drawing))
            .with_workspace(workspace),
    )
}

fn show_stderr(line: &str, drawing: bool) -> bool {
    match line {
        "muse: local session messaging disabled: session logging is required"
        | "muse: workspace trust: trusted source=run-flag"
        | "muse: Agent delegation: auto unavailable: child tool 'mcp__paintrs__screenshot' is unavailable."
        | "muse: Agent delegation: auto unavailable: child tool 'mcp__paintrs__act' is unavailable." => {
            false
        }
        // Queries use an empty toolset. Drawing needs the project MCP server.
        "muse: Agent delegation: auto unavailable: workspace is untrusted."
        | "muse: warning: project MCP servers from .mcp.json, if any, are not loaded because the workspace is untrusted; restart with --trust-workspace to load them" => {
            drawing
        }
        _ => !line.starts_with("muse: workspace root: "),
    }
}

fn events(value: &Value, tools: &mut HashMap<String, String>) -> Vec<Event> {
    let payload = &value["payload"];
    let event = match value["payload_type"].as_str() {
        Some("run.model.configured") => payload["model_id"]
            .as_str()
            .map(|id| Event::Model(id.to_owned())),
        Some("run.terminal.completed") => payload["text"]
            .as_str()
            .filter(|text| !text.trim().is_empty())
            .map(|text| Event::Message(text.trim().to_owned())),
        Some("run.usage") => Some(Event::Usage(payload.clone())),
        Some("run.terminal.failed") => payload["reason"]
            .as_str()
            .map(|reason| Event::Error(reason.to_owned())),
        Some(kind) => tool_event(kind, payload, tools),
        _ => None,
    };
    event.into_iter().collect()
}

fn tool_event(kind: &str, payload: &Value, tools: &mut HashMap<String, String>) -> Option<Event> {
    let id = payload["task_id"].as_str()?;
    let (name, status) = match kind {
        "task.lifecycle.proposed" => {
            let name = payload["event"]["task_kind"]
                .as_str()?
                .strip_prefix("tool.mcp__paintrs__")?;
            tools.insert(id.to_owned(), name.to_owned());
            return None;
        }
        "task.lifecycle.started" => (tools.get(id)?.clone(), "started"),
        "task.lifecycle.completed" => (tools.remove(id)?, "completed"),
        "task.lifecycle.failed" | "task.lifecycle.rejected" | "task.lifecycle.cancelled" => {
            (tools.remove(id)?, "failed")
        }
        _ => return None,
    };
    Some(Event::Tool {
        name,
        status: status.to_owned(),
    })
}
