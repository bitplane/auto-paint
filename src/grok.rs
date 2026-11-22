use crate::harness::{self, Effort, Event, Model, Process, Query, Workspace};
use crate::{Result, Run};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs;
use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;

pub fn resolve_model(directory: &Path, requested: Option<&str>) -> Result<String> {
    let output = harness::command("grok")
        .args(["--no-auto-update", "models"])
        .current_dir(directory)
        .output()
        .map_err(|error| format!("Could not start Grok Build. Put grok on PATH: {error}"))?;
    if !output.status.success() {
        return Err(format!("grok models exited with {}", output.status).into());
    }
    let output = String::from_utf8(output.stdout)?;
    let (default, models) = model_catalogue(&output);
    if output
        .lines()
        .any(|line| line == "You are not authenticated.")
        && requested.is_none()
    {
        return Err("Grok Build is signed out. Run grok login or set XAI_API_KEY.".into());
    }
    match requested {
        None => default
            .map(str::to_owned)
            .ok_or_else(|| "Grok Build did not report a default model. Check grok models.".into()),
        Some(query) => harness::match_model(models.iter(), query)
            .map(|model| model.id.clone())
            .ok_or_else(|| {
                let ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
                format!(
                    "No available Grok model matches {query:?}. Choose --model from: {}",
                    ids.join(", ")
                )
                .into()
            }),
    }
}

fn model_catalogue(output: &str) -> (Option<&str>, Vec<Model>) {
    let default = output
        .lines()
        .find_map(|line| line.strip_prefix("Default model: "));
    let models = output
        .lines()
        .skip_while(|line| *line != "Available models:")
        .filter_map(|line| {
            let line = line.trim();
            line.strip_prefix("- ")
                .or_else(|| line.strip_prefix("* ")?.strip_suffix(" (default)"))
        })
        .map(|id| Model {
            id: id.to_owned(),
            display_name: id.to_owned(),
        })
        .collect();
    (default, models)
}

pub fn start(run: &Run, address: SocketAddr, token: &str, prompt: &str) -> Result<Process> {
    let workspace = Workspace::new()?;
    let mut command = command(
        &workspace.0,
        run.model(),
        run.options.effort,
        Some((address, token)),
    )?;
    let prompt = format!(
        "{prompt}\nLook up the Paintrs tools with search_tool before calling them with use_tool."
    );
    set_prompt(
        &mut command,
        &workspace.0,
        &prompt,
        run.reference.as_deref(),
    )?;
    command.args(["--allow", "MCPTool(paintrs__*)"]);
    spawn(command, workspace, false)
}

pub fn start_query(query: Query<'_>) -> Result<Process> {
    let workspace = Workspace::new()?;
    let mut command = command(&workspace.0, query.model, query.effort, None)?;
    let schema = fs::read_to_string(query.schema)?;
    let prompt = format!(
        "{}\nReturn one JSON object matching this schema. Include no Markdown or other text.\n{schema}",
        query.prompt
    );
    set_prompt(&mut command, &workspace.0, &prompt, query.image)?;
    command.args(["--disallowed-tools", "search_tool,use_tool"]);
    spawn(command, workspace, true)
}

fn command(
    directory: &Path,
    model: &str,
    effort: Option<Effort>,
    mcp: Option<(SocketAddr, &str)>,
) -> Result<Command> {
    let mut profile = json!({
        "name": "auto-paint",
        "description": "Draw and review pictures for Auto Paint",
        "promptMode": "full",
        "agentsMd": false,
        "injectDefaultTools": false,
        "mcpInheritance": "none",
        "tools": ["search_tool"],
    });
    if let Some((address, token)) = mcp {
        let executable = std::env::current_exe()?;
        let executable = executable
            .to_str()
            .ok_or("Auto Paint's executable path must be UTF-8")?;
        profile["tools"] = json!(["search_tool", "use_tool"]);
        profile["mcpInheritance"] = json!({"named": ["paintrs"]});
        profile["mcpServers"] = json!([{"paintrs": {
            "command": executable,
            "args": ["--mcp-stdio", address.to_string()],
            "env": {"AUTO_PAINT_MCP_TOKEN": token},
        }}]);
    }
    let agent = directory.join("auto-paint.md");
    fs::write(
        &agent,
        format!(
            "---\n{profile}\n---\n\nYou are the artist in Auto Paint. Follow the current request directly.\n"
        ),
    )?;
    let mut command = harness::command("grok");
    command
        .current_dir(directory)
        .arg("--agent")
        .arg(agent)
        .args([
            "--no-auto-update",
            "--no-memory",
            "--no-plan",
            "--no-subagents",
            "--disable-web-search",
            "--verbatim",
            "--permission-mode",
            "dontAsk",
            "--model",
            model,
        ]);
    if let Some(effort) = effort {
        command.args(["--reasoning-effort", effort.as_str()]);
    }
    Ok(command)
}

fn set_prompt(
    command: &mut Command,
    directory: &Path,
    prompt: &str,
    image: Option<&Path>,
) -> Result<()> {
    if let Some(image) = image {
        let mime = crate::reference_media_type(image).ok_or("Unsupported Grok image")?;
        let blocks = json!([
            {"type": "text", "text": prompt},
            {"type": "image", "mimeType": mime, "data": STANDARD.encode(fs::read(image)?)},
        ]);
        let path = directory.join("prompt.json");
        fs::write(&path, serde_json::to_vec(&blocks)?)?;
        command.arg("--prompt-file").arg(path);
    } else {
        command.arg("--single").arg(prompt);
    }
    Ok(())
}

fn spawn(mut command: Command, workspace: Workspace, structured: bool) -> Result<Process> {
    command.args(["--output-format", "streaming-json"]);
    let mut calls = HashMap::new();
    let mut text = String::new();
    Ok(Process::spawn(command, &[], move |event| {
        stream_events(event, &mut calls, &mut text, structured)
    })?
    .with_workspace(workspace))
}

fn stream_events(
    event: &Value,
    calls: &mut HashMap<String, String>,
    text: &mut String,
    structured: bool,
) -> Vec<Event> {
    match event["type"].as_str() {
        Some("text") => {
            text.push_str(event["data"].as_str().unwrap_or_default());
            Vec::new()
        }
        Some("tool_call") => {
            let tool = match event["toolName"].as_str() {
                Some("use_tool") => event["rawInput"]["tool_name"].as_str(),
                tool => tool,
            }
            .and_then(|name| name.strip_prefix("paintrs__"));
            if let (Some(tool), Some(id)) = (tool, event["toolCallId"].as_str()) {
                calls.insert(id.to_owned(), tool.to_owned());
                vec![Event::Tool {
                    name: tool.to_owned(),
                    status: "started".to_owned(),
                }]
            } else {
                Vec::new()
            }
        }
        Some("tool_call_update") => {
            if let (Some(id), Some(status @ ("completed" | "failed"))) =
                (event["toolCallId"].as_str(), event["status"].as_str())
                && let Some(name) = calls.remove(id)
            {
                vec![Event::Tool {
                    name,
                    status: status.to_owned(),
                }]
            } else {
                Vec::new()
            }
        }
        Some("end") => {
            let mut events: Vec<_> = usage(event).into_iter().collect();
            let text = text.trim();
            if !text.is_empty() {
                let line = if structured {
                    harness::json_reply(text)
                } else {
                    text
                };
                events.push(Event::Message(line.to_owned()));
            }
            events
        }
        Some("error") => {
            let mut events: Vec<_> = usage(event).into_iter().collect();
            if let Some(message) = event["message"].as_str() {
                events.push(Event::Error(message.to_owned()));
            }
            events
        }
        _ => Vec::new(),
    }
}

fn usage(value: &Value) -> Option<Event> {
    let mut usage = value.get("usage")?.as_object()?.clone();
    if let Some(cost) = value.get("total_cost_usd") {
        usage.insert("cost_usd".into(), cost.clone());
    }
    Some(Event::Usage(Value::Object(usage)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_catalogue_default_and_matches_a_model_family() {
        let (default, models) = model_catalogue(
            "Default model: grok-build\n\nAvailable models:\n  - grok-4.6\n  - grok-4.10\n  * grok-build (default)\n",
        );
        assert_eq!(default, Some("grok-build"));
        assert_eq!(
            harness::match_model(models.iter(), "grok-4").unwrap().id,
            "grok-4.10"
        );
    }

    #[test]
    fn reconstructs_chunked_json_and_preserves_reported_usage() {
        let mut calls = HashMap::new();
        let mut text = String::new();
        for data in ["```json\n", "{\"score\":", "8}\n```"] {
            assert!(
                stream_events(
                    &json!({"type": "text", "data": data}),
                    &mut calls,
                    &mut text,
                    true
                )
                .is_empty()
            );
        }
        let events = stream_events(
            &json!({"type": "end", "usage": {"input_tokens": 10}, "total_cost_usd": 0.01}),
            &mut calls,
            &mut text,
            true,
        );
        assert!(matches!(&events[0], Event::Usage(value) if value["cost_usd"] == 0.01));
        assert!(matches!(&events[1], Event::Message(message) if message == r#"{"score":8}"#));
    }
}
