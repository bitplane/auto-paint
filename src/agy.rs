use crate::harness::{self, Effort, Event, Model, Process};
use crate::{Result, Run};
use serde_json::{Value, json};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

pub fn models(directory: &Path) -> Result<Vec<Model>> {
    let mut command = harness::command("agy");
    command.arg("models").current_dir(directory);
    let mut process = Process::open_lines(command, |line| parse_model(line).into_iter().collect())
        .map_err(|error| {
            format!("Could not start Antigravity CLI. Install agy and sign in. ({error})")
        })?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = process.status()? {
            let models: Vec<_> = process.events.try_iter().collect();
            if status.success() && !models.is_empty() {
                return Ok(models);
            }
            return Err("Antigravity did not list any available models. Run agy to sign in and check agy models.".into());
        }
        if Instant::now() >= deadline {
            return Err("Timed out waiting for agy models".into());
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn parse_model(line: &str) -> Option<Model> {
    let line = line.trim();
    let (id, display_name) = line.split_once('\t').or_else(|| line.split_once("  "))?;
    let id = id.trim();
    let display_name = display_name.trim();
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.:/".contains(c))
        || display_name.is_empty()
    {
        return None;
    }
    Some(Model {
        id: id.to_owned(),
        display_name: display_name.to_owned(),
    })
}

pub fn start(run: &Run, address: SocketAddr, token: &str, prompt: &str) -> Result<Process> {
    let config = WorkspaceConfig::new(&run.output, true)?;
    config.write_mcp(run, address)?;
    let mut command = exec_command(&run.output, run.model(), run.options.effort, true);
    command.env("AUTO_PAINT_MCP_TOKEN", token);
    let prompt = if run.reference.is_some() {
        format!(
            "{prompt}\nCall the Paintrs reference tool to see the reference image before drawing."
        )
    } else {
        prompt.to_owned()
    };
    command.arg("--print").arg(prompt);
    Ok(Process::spawn(command, &[], events)?.with_agy_config(config))
}

pub fn start_selection(
    directory: &Path,
    model: &str,
    effort: Option<Effort>,
    schema: &Path,
    prompt: &str,
) -> Result<Process> {
    let config = WorkspaceConfig::new(directory, false)?;
    let mut command = exec_command(directory, model, effort, false);
    let schema = fs::read_to_string(schema)?;
    let prompt = format!(
        "{prompt}\nReturn one JSON object matching the schema below. Include no Markdown or other text.\n{schema}"
    );
    command.arg("--print").arg(prompt);
    Ok(Process::spawn(command, &[], selection_events)?.with_agy_config(config))
}

fn exec_command(directory: &Path, model: &str, effort: Option<Effort>, drawing: bool) -> Command {
    let mut deny = vec![
        "command(*)",
        "read_file(*)",
        "write_file(*)",
        "read_url(*)",
        "execute_url(*)",
    ];
    let allow = if drawing {
        vec!["mcp(paintrs/*)"]
    } else {
        deny.push("mcp(*)");
        Vec::new()
    };
    let settings = json!({
        "agentMode": "",
        "toolPermission": "request-review",
        "permissions": {"allow": allow, "deny": deny}
    });
    let mut command = harness::command("agy");
    command
        .args([
            "--output-format",
            "stream-json",
            "--disable-slash-commands",
            "--agent",
            "auto-paint",
            "--model",
            model,
        ])
        .arg("--settings")
        .arg(settings.to_string())
        .current_dir(directory);
    if let Some(effort) = effort {
        command.args(["--effort", effort.as_str()]);
    }
    command
}

pub struct WorkspaceConfig {
    directory: PathBuf,
}

impl WorkspaceConfig {
    fn new(directory: &Path, drawing: bool) -> Result<Self> {
        let directory = directory.join(".agents");
        fs::create_dir(&directory)?;
        let config = Self { directory };
        let agents = config.directory.join("agents");
        fs::create_dir(&agents)?;
        let agent = format!(
            "---\nname: auto-paint\ndescription: Draw pictures in Paintrs and suggest painting subjects.\n\
             mainAgent: true\nsubagent: false\ninheritMcp: {drawing}\nexcludeDefaultComponents: true\n\
             commandExecutionPolicy: 'off'\n---\n\n\
             You are the artist in Auto Paint. Follow the drawing or subject selection request directly."
        );
        fs::write(agents.join("auto-paint.md"), agent)?;
        Ok(config)
    }

    fn write_mcp(&self, run: &Run, address: SocketAddr) -> Result<()> {
        let executable = std::env::current_exe()?;
        let executable = executable
            .to_str()
            .ok_or("Auto Paint's executable path must be UTF-8")?;
        let mut tools = json!({"screenshot": {"eager": true}, "act": {"eager": true}});
        if run.options.narrate {
            tools["review"] = json!({"eager": true});
        }
        if run.reference.is_some() {
            tools["reference"] = json!({"eager": true});
        }
        let server = json!({"mcpServers": {"paintrs": {
            "command": executable,
            "args": ["--mcp-stdio", address.to_string()],
            "tools": tools
        }}});
        fs::write(
            self.directory.join("mcp_config.json"),
            serde_json::to_vec(&server)?,
        )?;
        Ok(())
    }
}

impl Drop for WorkspaceConfig {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.directory.join("mcp_config.json"));
        let agents = self.directory.join("agents");
        let _ = fs::remove_file(agents.join("auto-paint.md"));
        let _ = fs::remove_dir(agents);
        let _ = fs::remove_dir(&self.directory);
    }
}

fn events(value: &Value) -> Vec<Event> {
    match value["event"].as_str() {
        Some("step_update") => tool_event(&value["step_update"]).into_iter().collect(),
        Some("result") => {
            let result = &value["result"];
            let mut events = Vec::new();
            if let Some(usage) = result.get("usage") {
                events.push(Event::Usage(usage.clone()));
            }
            if result["status"] == "SUCCESS" {
                if let Some(output) = result
                    .get("structured_output")
                    .filter(|output| !output.is_null())
                {
                    events.push(Event::Message(
                        output
                            .as_str()
                            .map_or_else(|| output.to_string(), str::to_owned),
                    ));
                } else if let Some(message) = result["response"]
                    .as_str()
                    .filter(|text| !text.trim().is_empty())
                {
                    events.push(Event::Message(message.trim().to_owned()));
                }
            } else {
                let message = result["error"]
                    .as_str()
                    .or_else(|| result["status"].as_str())
                    .unwrap_or("Antigravity did not return a successful result");
                events.push(Event::Error(message.to_owned()));
            }
            events
        }
        _ => Vec::new(),
    }
}

fn tool_event(step: &Value) -> Option<Event> {
    let tool = step["tool_name"].as_str()?;
    let name = tool
        .strip_prefix("mcp__paintrs__")
        .or_else(|| tool.strip_prefix("mcp_paintrs_"))?;
    let status = match step["state"].as_str()? {
        "ACTIVE" => "started",
        "DONE" if step["tool_info"]["error"].is_null() => "completed",
        "DONE" => "failed",
        _ => return None,
    };
    Some(Event::Tool {
        name: name.to_owned(),
        status: status.to_owned(),
    })
}

fn selection_events(value: &Value) -> Vec<Event> {
    let mut events = events(value);
    for event in &mut events {
        if let Event::Message(message) = event {
            *message = json_reply(message).to_owned();
        }
    }
    events
}

fn json_reply(message: &str) -> &str {
    if serde_json::from_str::<Value>(message).is_ok() {
        return message;
    }
    message
        .split("```")
        .skip(1)
        .step_by(2)
        .map(|block| {
            let block = block.trim();
            block
                .strip_prefix("json")
                .or_else(|| block.strip_prefix("JSON"))
                .unwrap_or(block)
                .trim()
        })
        .filter(|block| serde_json::from_str::<Value>(block).is_ok())
        .last()
        .unwrap_or(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_model_rows_and_skips_status_messages() {
        let model = parse_model("gemini-3.8-flash-medium\tGemini 3.8 Flash (Medium)").unwrap();
        assert_eq!(model.id, "gemini-3.8-flash-medium");
        assert_eq!(model.display_name, "Gemini 3.8 Flash (Medium)");
        assert_eq!(parse_model("custom  Custom Model").unwrap().id, "custom");
        assert!(parse_model("Fetching available models...").is_none());
    }

    #[test]
    fn accepts_json_even_when_the_agent_wraps_it_in_markdown() {
        let json = r#"{"options":["Landscapes","Still lifes"]}"#;
        let fenced = format!("```json\n{json}\n```\n```json\n{json}\n```");
        assert_eq!(json_reply(&fenced), json);
        assert_eq!(json_reply(json), json);
    }
}
