use crate::harness::{self, Effort, Event, Model, Process, Query};
use crate::{Result, Run};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;

#[derive(Deserialize)]
struct CatalogEntry {
    name: String,
    capabilities: Capabilities,
    #[serde(default)]
    variants: BTreeMap<String, Value>,
    #[serde(default)]
    release_date: String,
}

#[derive(Deserialize)]
struct Capabilities {
    toolcall: bool,
    input: Modalities,
}

#[derive(Deserialize)]
struct Modalities {
    image: bool,
}

fn cli_output(directory: &Path, args: &[&str]) -> Result<String> {
    let output = harness::command("opencode")
        .args(args)
        .current_dir(directory)
        .output()
        .map_err(|error| format!("Could not start OpenCode. Put opencode on PATH: {error}"))?;
    if !output.status.success() {
        return Err(format!("opencode {} exited with {}", args.join(" "), output.status).into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

pub fn resolve_model(
    directory: &Path,
    requested: Option<&str>,
    effort: Option<Effort>,
) -> Result<String> {
    let catalog = read_catalog(&cli_output(directory, &["models", "--verbose"])?)?;
    let config: Value = if requested.is_none() {
        serde_json::from_str(&cli_output(directory, &["debug", "config"])?)?
    } else {
        Value::Null
    };
    select_model(&catalog, requested, config["model"].as_str(), effort)
}

fn read_catalog(output: &str) -> Result<Vec<(Model, CatalogEntry)>> {
    let mut entries = Vec::new();
    let mut id = "";
    let mut metadata = String::new();
    // The verbose listing prints each qualified ID above its pretty-printed metadata.
    for line in output.lines() {
        if metadata.is_empty() && line != "{" {
            id = line.trim();
            continue;
        }
        metadata.push_str(line);
        metadata.push('\n');
        if line == "}" {
            if let Some((provider, model)) = id.split_once('/')
                && !provider.is_empty()
                && !model.is_empty()
            {
                let entry: CatalogEntry = serde_json::from_str(&metadata)
                    .map_err(|error| format!("Could not read OpenCode model {id}: {error}"))?;
                entries.push((
                    Model {
                        id: id.to_owned(),
                        display_name: entry.name.clone(),
                    },
                    entry,
                ));
            }
            metadata.clear();
        }
    }
    if entries.is_empty() {
        return Err(
            "OpenCode has no available models. Connect a provider in OpenCode first.".into(),
        );
    }
    Ok(entries)
}

fn select_model(
    catalog: &[(Model, CatalogEntry)],
    requested: Option<&str>,
    configured: Option<&str>,
    effort: Option<Effort>,
) -> Result<String> {
    let usable =
        |entry: &CatalogEntry| entry.capabilities.toolcall && entry.capabilities.input.image;
    let candidates = catalog.iter().filter(|(_, entry)| usable(entry));
    let selected = if let Some(requested) = requested {
        let models = candidates.clone().map(|(model, _)| model);
        let found = if requested.contains('/') {
            models.clone().find(|model| model.id == requested)
        } else {
            harness::match_model(models, requested)
        };
        found.and_then(|found| candidates.clone().find(|(model, _)| model.id == found.id))
    } else {
        let configured =
            configured.and_then(|id| candidates.clone().find(|(model, _)| model.id == id));
        configured.or_else(|| {
            candidates.clone().max_by_key(|(model, entry)| {
                (!model.id.starts_with("opencode/"), &entry.release_date)
            })
        })
    };
    let (model, entry) = selected.ok_or_else(|| {
        let names: Vec<_> = candidates.map(|(model, _)| model.id.as_str()).collect();
        let query = requested.map_or_else(String::new, |query| format!(" matching {query:?}"));
        if names.is_empty() {
            format!("No available OpenCode model{query} supports both images and tools. Connect a provider with a vision model in OpenCode.")
        } else {
            format!("No available OpenCode model{query} supports both images and tools. Choose --model from: {}", names.join(", "))
        }
    })?;
    if let Some(effort) = effort
        && !entry.variants.contains_key(effort.as_str())
    {
        let variants: Vec<_> = entry.variants.keys().map(String::as_str).collect();
        let detail = if variants.is_empty() {
            "This model has no effort variants. Omit --effort.".to_owned()
        } else {
            format!("OpenCode variants for this model: {}.", variants.join(", "))
        };
        return Err(format!(
            "{} does not support --effort {}. {detail}",
            model.id,
            effort.as_str()
        )
        .into());
    }
    Ok(model.id.clone())
}

pub fn start(run: &Run, address: SocketAddr, token: &str, prompt: &str) -> Result<Process> {
    let mcp = Some((address, token));
    let command = exec_command(
        run.model(),
        run.options.effort,
        run.reference.as_deref(),
        mcp,
    )?;
    spawn(command, prompt, false)
}

pub fn start_query(query: Query<'_>) -> Result<Process> {
    let command = exec_command(query.model, query.effort, query.image, None)?;
    let schema = std::fs::read_to_string(query.schema)?;
    let prompt = format!(
        "{}\nReturn one JSON object matching this schema. Include no Markdown or other text.\n{schema}",
        query.prompt
    );
    spawn(command, &prompt, true)
}

fn exec_command(
    model: &str,
    effort: Option<Effort>,
    image: Option<&Path>,
    mcp: Option<(SocketAddr, &str)>,
) -> Result<Command> {
    // Keep OpenCode in the invoking project so project-scoped providers are available.
    // Each command starts a fresh session, with only the file and tools we supply.
    let mut config = match std::env::var("OPENCODE_CONFIG_CONTENT") {
        Ok(content) if !content.is_empty() => serde_json::from_str(&content)
            .map_err(|error| format!("OPENCODE_CONFIG_CONTENT must be a JSON object: {error}"))?,
        _ => json!({}),
    };
    if !config.is_object() {
        return Err("OPENCODE_CONFIG_CONTENT must be a JSON object".into());
    }
    let mut permission = json!({"*": "deny"});
    if let Some((address, token)) = mcp {
        let executable = std::env::current_exe()?;
        let executable = executable
            .to_str()
            .ok_or("Auto Paint's executable path must be UTF-8")?;
        config["mcp"]["autopaint"] = json!({
            "type": "local",
            "command": [executable, "--mcp-stdio", address.to_string()],
            "environment": {"AUTO_PAINT_MCP_TOKEN": token},
            "enabled": true,
        });
        permission["autopaint_screenshot"] = json!("allow");
        permission["autopaint_act"] = json!("allow");
    }
    config["share"] = json!("disabled");
    config["autoupdate"] = json!(false);
    config["agent"]["auto-paint"] = json!({
        "description": "Draw and review pictures for Auto Paint.",
        "mode": "primary",
        "prompt": "You are the artist in Auto Paint. Follow the current request directly.",
        "permission": permission,
    });
    let mut command = harness::command("opencode");
    command
        .args([
            "run",
            "--format",
            "json",
            "--title",
            "Auto Paint",
            "--agent",
            "auto-paint",
            "--model",
            model,
        ])
        .env("OPENCODE_CONFIG_CONTENT", config.to_string());
    if let Some(effort) = effort {
        command.args(["--variant", effort.as_str()]);
    }
    if let Some(image) = image {
        command.arg("--file").arg(image);
    }
    Ok(command)
}

fn spawn(command: Command, prompt: &str, structured: bool) -> Result<Process> {
    let mut seen = std::collections::HashSet::new();
    let mut usage = Usage::default();
    Process::spawn(command, prompt.as_bytes(), move |value| {
        let part = &value["part"];
        let event = match value["type"].as_str() {
            Some("text") => part["text"].as_str().map(|text| {
                let text = text.trim();
                let text = if structured {
                    harness::json_reply(text)
                } else {
                    text
                };
                Event::Message(text.to_owned())
            }),
            Some("tool_use") => part["tool"].as_str().and_then(|tool| {
                Some(Event::Tool {
                    name: tool.strip_prefix("autopaint_")?.to_owned(),
                    status: match part["state"]["status"].as_str()? {
                        "completed" => "completed",
                        _ => "failed",
                    }
                    .to_owned(),
                })
            }),
            Some("step_finish")
                if part["id"]
                    .as_str()
                    .is_some_and(|id| seen.insert(id.to_owned())) =>
            {
                Some(Event::Usage(usage.add(part)))
            }
            Some("error") => Some(Event::Error(
                value["error"]["data"]["message"]
                    .as_str()
                    .or_else(|| value["error"]["name"].as_str())
                    .unwrap_or("OpenCode returned an error")
                    .to_owned(),
            )),
            _ => None,
        };
        event.into_iter().collect()
    })
}

#[derive(Default, serde::Serialize)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    reasoning_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    cost_usd: f64,
}

impl Usage {
    fn add(&mut self, step: &Value) -> Value {
        let tokens = &step["tokens"];
        self.input_tokens += tokens["input"].as_u64().unwrap_or_default();
        self.output_tokens += tokens["output"].as_u64().unwrap_or_default();
        self.reasoning_tokens += tokens["reasoning"].as_u64().unwrap_or_default();
        self.cache_read_tokens += tokens["cache"]["read"].as_u64().unwrap_or_default();
        self.cache_write_tokens += tokens["cache"]["write"].as_u64().unwrap_or_default();
        self.cost_usd += step["cost"].as_f64().unwrap_or_default();
        json!(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chooses_available_vision_models_and_checks_effort() -> Result<()> {
        let model = |id, image, tools, variants| {
            let metadata = json!({
                "name": id,
                "capabilities": {"input": {"image": image}, "toolcall": tools},
                "variants": variants,
            });
            format!("{id}\n{metadata:#}\n")
        };
        let low = json!({"low": {}});
        let listing = [
            model("router/vendor/sol-6.10", true, true, low.clone()),
            model("vendor/sol-6.2", true, true, low),
            model("vendor/sol-7", false, true, json!({})),
            model("vendor/sol-8", true, false, json!({})),
        ]
        .concat();
        let catalog = read_catalog(&listing)?;
        assert_eq!(
            select_model(&catalog, Some("sol"), None, Some(Effort::Low))?,
            "router/vendor/sol-6.10"
        );
        assert_eq!(
            select_model(&catalog, None, Some("vendor/sol-6.2"), None)?,
            "vendor/sol-6.2"
        );
        assert!(select_model(&catalog, Some("vendor/sol-8"), None, None).is_err());
        assert!(
            select_model(
                &catalog,
                Some("router/vendor/sol-6.10"),
                None,
                Some(Effort::High)
            )
            .unwrap_err()
            .to_string()
            .contains("OpenCode variants for this model: low")
        );
        Ok(())
    }
}
