use crate::harness::{self, Event, Model, ModelProbe, Process};
use crate::{Result, Run};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;

pub fn check_login() -> Result<()> {
    let output = harness::command("claude")
        .args(["auth", "status", "--json"])
        .output()
        .map_err(|err| {
            format!("Could not run Claude Code. Install it and run claude auth login. ({err})")
        })?;
    let status: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
    if !output.status.success() || status["loggedIn"] != true {
        return Err("Run claude auth login before starting Auto Paint with Claude.".into());
    }
    Ok(())
}

pub fn models(directory: &Path) -> Result<Vec<Model>> {
    let mut probe = ModelProbe::open(exec_command(directory, "haiku"))?;
    // Claude Code returns the model picker entries in the SDK initialize response.
    probe.send(json!({
        "type": "control_request",
        "request_id": "models",
        "request": {"subtype": "initialize"}
    }))?;
    let reply = probe.response(|value| value["response"]["request_id"] == "models")?;
    let rows = reply["response"]["response"]["models"]
        .as_array()
        .ok_or("Claude Code could not list models")?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            let id = row["value"].as_str()?;
            Some(Model {
                id: id.to_owned(),
                display_name: row["displayName"].as_str().unwrap_or(id).to_owned(),
            })
        })
        .collect())
}

pub fn start(run: &Run, address: SocketAddr, token: &str, prompt: &str) -> Result<Process> {
    let executable = std::env::current_exe()?;
    let executable = executable
        .to_str()
        .ok_or("Auto Paint's executable path must be UTF-8")?;
    let config = json!({
        "mcpServers": {
            "paintrs": {
                "type": "stdio",
                "command": executable,
                "args": ["--mcp-stdio", address.to_string()],
                "env": {"AUTO_PAINT_MCP_TOKEN": "${AUTO_PAINT_MCP_TOKEN}"}
            }
        }
    });
    let mut command = exec_command(&run.output, run.model());
    command
        .arg("--mcp-config")
        .arg(config.to_string())
        .args([
            "--allowedTools",
            "mcp__paintrs__screenshot",
            "mcp__paintrs__act",
        ])
        .env("AUTO_PAINT_MCP_TOKEN", token);
    if run.options.narrate {
        command.arg("mcp__paintrs__review");
    }
    spawn(command, prompt, run.reference.as_deref(), drawing_events)
}

pub fn start_selection(
    directory: &Path,
    model: &str,
    schema: &Path,
    prompt: &str,
) -> Result<Process> {
    let mut command = exec_command(directory, model);
    command
        .arg("--json-schema")
        .arg(std::fs::read_to_string(schema)?);
    spawn(command, prompt, None, result_events)
}

fn exec_command(directory: &Path, model: &str) -> Command {
    let mut command = harness::command("claude");
    command
        .args([
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--restricted",
            "--strict-mcp-config",
            "--no-chrome",
            "--no-session-persistence",
            "--disable-slash-commands",
            "--permission-mode",
            "dontAsk",
            "--permission-prompts",
            "none",
            "--tools",
            "",
            "--model",
            model,
            "--system-prompt",
            "You are the artist in Auto Paint. Follow the drawing or subject selection request.",
        ])
        .current_dir(directory);
    command
}

fn spawn(
    command: Command,
    prompt: &str,
    reference: Option<&Path>,
    events: fn(&Value) -> Vec<Event>,
) -> Result<Process> {
    let mut content = vec![json!({"type": "text", "text": prompt})];
    if let Some(path) = reference {
        let media_type = crate::reference_media_type(path).ok_or("Unsupported reference image")?;
        let data = STANDARD.encode(std::fs::read(path)?);
        content.push(json!({
            "type": "image",
            "source": {"type": "base64", "media_type": media_type, "data": data}
        }));
    }
    let message = json!({
        "type": "user",
        "message": {"role": "user", "content": content},
        "parent_tool_use_id": null
    });
    let mut input = serde_json::to_vec(&message)?;
    input.push(b'\n');
    Process::spawn(command, &input, events)
}

fn drawing_events(value: &Value) -> Vec<Event> {
    if value["type"] == "system"
        && value["subtype"] == "init"
        && let Some(model) = value["model"].as_str()
    {
        return vec![Event::Model(model.to_owned())];
    }
    if value["type"] == "assistant"
        && let Some(content) = value["message"]["content"].as_array()
    {
        return content
            .iter()
            .filter_map(|block| match block["type"].as_str()? {
                "text" => Some(Event::Message(block["text"].as_str()?.to_owned())),
                "tool_use" => Some(Event::Tool {
                    name: block["name"]
                        .as_str()?
                        .strip_prefix("mcp__paintrs__")?
                        .to_owned(),
                    status: "started".to_owned(),
                }),
                _ => None,
            })
            .collect();
    }
    result_events(value)
}

fn result_events(value: &Value) -> Vec<Event> {
    if value["type"] != "result" {
        return Vec::new();
    }
    let mut events = Vec::new();
    if let Some(mut usage) = value["usage"].as_object().cloned() {
        if let Some(cost) = value.get("total_cost_usd") {
            usage.insert("estimated_cost_usd".to_owned(), cost.clone());
        }
        if let Some(models) = value.get("modelUsage") {
            usage.insert("model_usage".to_owned(), models.clone());
        }
        events.push(Event::Usage(Value::Object(usage)));
    }
    if value["is_error"] == true {
        let errors = value["errors"].as_array().map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        });
        let message = errors
            .as_deref()
            .filter(|text| !text.is_empty())
            .or_else(|| value["result"].as_str())
            .or_else(|| value["subtype"].as_str())
            .unwrap_or("Claude Code reported a failed session");
        events.push(Event::Error(message.to_owned()));
    } else if let Some(result) = value.get("structured_output") {
        events.push(Event::Message(result.to_string()));
    }
    events
}
