use crate::{Result, codex};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::Path;

const CATEGORY_COUNT: usize = 10;
const IDEA_COUNT: usize = 8;
const SCHEMA: &str = r#"{
    "type": "object",
    "properties": {
        "options": { "type": "array", "items": { "type": "string" } }
    },
    "required": ["options"],
    "additionalProperties": false
}"#;

#[derive(Deserialize)]
struct Suggestions {
    options: Vec<String>,
}

struct Pick {
    candidates: Vec<String>,
    index: usize,
    usage: Option<Value>,
}

impl Pick {
    fn selected(&self) -> &str {
        &self.candidates[self.index]
    }

    fn report(&self) -> Value {
        json!({
            "candidates": self.candidates,
            "selected_index": self.index,
            "selected": self.selected(),
            "usage": self.usage,
        })
    }
}

pub fn choose(output: &Path, model: &str) -> Result<(String, String)> {
    let schema = output.join(".selection-schema.json");
    std::fs::write(&schema, SCHEMA)?;
    let picks = choose_with_schema(output, model, &schema);
    let cleanup = std::fs::remove_file(schema);
    let (category, idea) = picks?;
    cleanup?;

    let report = json!({
        "model": model,
        "category": category.report(),
        "idea": idea.report(),
    });
    let mut data = serde_json::to_vec_pretty(&report)?;
    data.push(b'\n');
    std::fs::write(output.join("selection.json"), data)?;
    println!("Drawing: {}", idea.selected());

    Ok((category.selected().to_owned(), idea.selected().to_owned()))
}

fn choose_with_schema(output: &Path, model: &str, schema: &Path) -> Result<(Pick, Pick)> {
    println!("Choosing a painting category");
    let category = pick(
        output,
        model,
        schema,
        CATEGORY_COUNT,
        &format!(
            "List {CATEGORY_COUNT} distinct, equally broad categories for colourful paintings              in a classic paint program. Cover different subject areas.              Return the category names as short noun phrases in the options array."
        ),
    )?;
    println!("Choosing an idea in {}", category.selected());
    let idea = pick(
        output,
        model,
        schema,
        IDEA_COUNT,
        &format!(
            "List {IDEA_COUNT} very different concrete painting ideas for the category {:?}.              Each should be a recognisable scene that fits a 720 by 480 canvas and can be              drawn with simple shapes and a few colours. Write each as a short noun phrase              in the options array, suitable to follow the words 'I picked'.",
            category.selected()
        ),
    )?;
    Ok((category, idea))
}

fn pick(directory: &Path, model: &str, schema: &Path, limit: usize, prompt: &str) -> Result<Pick> {
    let (text, usage) = codex::structured_reply(directory, model, schema, prompt)?;
    let suggestions: Suggestions = serde_json::from_str(&text)
        .map_err(|err| format!("Could not read Codex's suggestions: {err}"))?;
    let mut seen = HashSet::new();
    let candidates: Vec<String> = suggestions
        .options
        .into_iter()
        .map(|entry| entry.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|entry| !entry.is_empty() && seen.insert(entry.to_lowercase()))
        .take(limit)
        .collect();
    if candidates.len() < 2 {
        return Err("Codex returned fewer than two distinct suggestions".into());
    }
    let count = candidates.len() as u64;
    let range = u64::from(u32::MAX) + 1;
    // Sample a whole number of buckets so each candidate is equally likely.
    let usable = range - range % count;
    let index = loop {
        let value =
            u64::from(getrandom::u32().map_err(|err| format!("Choosing a subject: {err}"))?);
        if value < usable {
            break (value % count) as usize;
        }
    };
    Ok(Pick {
        candidates,
        index,
        usage,
    })
}
