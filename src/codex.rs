use crate::harness::{self, Event, Model, ModelProbe, Process};
use crate::{Result, Run};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;

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
    let mut command = harness::command("codex");
    command
        .env_remove("OPENAI_API_KEY")
        .env_remove("CODEX_API_KEY");
    command
}

pub fn models(directory: &Path) -> Result<Vec<Model>> {
    let mut command = command();
    command
        .args(["app-server", "--stdio"])
        .current_dir(directory);
    let mut probe = ModelProbe::open(command)?;
    probe.send(json!({
        "id": 1,
        "method": "initialize",
        "params": {"clientInfo": {"name": "auto-paint", "version": env!("CARGO_PKG_VERSION")}}
    }))?;
    if probe.response(|value| value["id"] == 1)?["result"].is_null() {
        return Err("Codex could not initialize its model listing".into());
    }
    probe.send(json!({"method": "initialized"}))?;
    let mut cursor = Value::Null;
    let mut models = Vec::new();
    loop {
        probe.send(json!({
            "id": 2,
            "method": "model/list",
            "params": {"limit": 100, "cursor": cursor}
        }))?;
        let page = probe.response(|value| value["id"] == 2)?;
        let rows = page["result"]["data"]
            .as_array()
            .ok_or("Codex could not list models")?;
        models.extend(rows.iter().filter_map(|row| {
            if row["inputModalities"]
                .as_array()
                .is_some_and(|modes| !modes.iter().any(|mode| mode == "image"))
            {
                return None;
            }
            let id = row["model"].as_str()?;
            Some(Model {
                id: id.to_owned(),
                display_name: row["displayName"].as_str().unwrap_or(id).to_owned(),
            })
        }));
        cursor = page["result"]["nextCursor"].clone();
        if cursor.is_null() {
            return Ok(models);
        }
    }
}

fn event(value: &Value) -> Option<Event> {
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

pub fn start(run: &Run, address: SocketAddr, token: &str, prompt: &str) -> Result<Process> {
    let executable = std::env::current_exe()?;
    let executable = executable
        .to_str()
        .ok_or("Auto Paint's executable path must be UTF-8")?;
    let server = "mcp_servers.paintrs";
    let mut command = exec_command(&run.output, run.model());
    command
        .args([
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
        .env("AUTO_PAINT_MCP_TOKEN", token);
    if run.options.narrate {
        command.args([
            "-c",
            &format!("{server}.tools.review.approval_mode=\"approve\""),
        ]);
    }
    if let Some(path) = &run.reference {
        command.arg("--image").arg(path);
    }
    spawn(command, prompt)
}

fn exec_command(directory: &Path, model: &str) -> Command {
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
            model,
            "-c",
            "model_reasoning_effort=\"low\"",
        ])
        .current_dir(directory);
    command
}

pub fn start_selection(
    directory: &Path,
    model: &str,
    schema: &Path,
    prompt: &str,
) -> Result<Process> {
    let mut command = exec_command(directory, model);
    command.arg("--output-schema").arg(schema);
    spawn(command, prompt)
}

fn spawn(mut command: Command, prompt: &str) -> Result<Process> {
    command.args(["--", "-"]);
    Process::spawn(command, prompt.as_bytes(), |value| {
        event(value).into_iter().collect()
    })
}

fn toml_string(value: &str) -> String {
    // JSON's spaces, slashes and escaped quotes are also accepted in TOML basic strings.
    json!(value).to_string()
}
