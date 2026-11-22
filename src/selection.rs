use crate::Result;
use crate::harness::{Effort, Harness, Query};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::Path;

const CATEGORY_COUNT: usize = 10;
const IDEA_COUNT: usize = 8;
const CATEGORY_SCHEMA: &str = r#"{
    "type": "object",
    "properties": {
        "options": { "type": "array", "items": { "type": "string" } }
    },
    "required": ["options"],
    "additionalProperties": false
}"#;
const IDEA_SCHEMA: &str = r#"{
    "type": "object",
    "properties": {
        "options": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "subject": { "type": "string" },
                    "opening": { "type": "string" }
                },
                "required": ["subject", "opening"],
                "additionalProperties": false
            }
        }
    },
    "required": ["options"],
    "additionalProperties": false
}"#;

#[derive(Deserialize)]
struct Suggestions {
    options: Vec<Suggestion>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Suggestion {
    Category(String),
    Idea { subject: String, opening: String },
}

impl Suggestion {
    fn subject(&self) -> &str {
        match self {
            Self::Category(subject) | Self::Idea { subject, .. } => subject,
        }
    }

    fn opening(&self) -> Option<&str> {
        match self {
            Self::Category(_) => None,
            Self::Idea { opening, .. } => Some(opening.trim()).filter(|line| !line.is_empty()),
        }
    }
}

struct Pick {
    candidates: Vec<Suggestion>,
    index: usize,
    usage: Option<Value>,
}

impl Pick {
    fn selected(&self) -> &Suggestion {
        &self.candidates[self.index]
    }

    fn report(&self) -> Value {
        let mut report = json!({
            "candidates": self.candidates.iter().map(Suggestion::subject).collect::<Vec<_>>(),
            "selected_index": self.index,
            "selected": self.selected().subject(),
            "usage": self.usage,
        });
        if let Some(opening) = self.selected().opening() {
            report["opening"] = json!(opening);
        }
        report
    }
}

pub fn choose(
    output: &Path,
    harness: Harness,
    model: &str,
    effort: Option<Effort>,
) -> Result<(String, Option<String>)> {
    let schema = output.join(".selection-schema.json");
    let picks = choose_with_schema(output, harness, model, effort, &schema);
    let cleanup = std::fs::remove_file(schema);
    let (category, idea) = picks?;
    cleanup?;

    let report = json!({
        "backend": harness.name(),
        "model": model,
        "category": category.report(),
        "idea": idea.report(),
    });
    let mut data = serde_json::to_vec_pretty(&report)?;
    data.push(b'\n');
    std::fs::write(output.join("selection.json"), data)?;
    let idea = idea.selected();
    println!("Drawing: {}", idea.subject());

    Ok((idea.subject().to_owned(), idea.opening().map(str::to_owned)))
}

fn choose_with_schema(
    output: &Path,
    harness: Harness,
    model: &str,
    effort: Option<Effort>,
    schema: &Path,
) -> Result<(Pick, Pick)> {
    std::fs::write(schema, CATEGORY_SCHEMA)?;
    println!("Choosing a painting category");
    let category = pick(
        output,
        harness,
        model,
        effort,
        schema,
        CATEGORY_COUNT,
        &format!(
            "List {CATEGORY_COUNT} distinct, equally broad categories for colourful paintings \
             in a classic paint program. Cover different subject areas. \
             Return the category names as short noun phrases in the options array."
        ),
    )?;
    let category_name = category.selected().subject();
    println!("Choosing an idea in {category_name}");
    std::fs::write(schema, IDEA_SCHEMA)?;
    let idea = pick(
        output,
        harness,
        model,
        effort,
        schema,
        IDEA_COUNT,
        &format!(
            "List {IDEA_COUNT} very different concrete painting ideas for the category {category_name:?}. \
             Each should be a recognisable scene that fits a 720 by 480 canvas and can be \
             drawn with simple shapes and a few colours. Write the subject as a short noun phrase. \
             For each idea, write one short line saying what you'll paint. Name the subject and \
             stop there. Use natural spoken wording and vary it across the ideas. Keep each opening \
             under 80 characters."
        ),
    )?;
    Ok((category, idea))
}

fn pick(
    directory: &Path,
    harness: Harness,
    model: &str,
    effort: Option<Effort>,
    schema: &Path,
    limit: usize,
    prompt: &str,
) -> Result<Pick> {
    let (text, usage) = harness.structured_reply(
        Query {
            directory,
            model,
            effort,
            schema,
            prompt,
            image: None,
        },
        None,
    )?;
    let suggestions: Suggestions = serde_json::from_str(&text)
        .map_err(|err| format!("Could not read {}'s suggestions: {err}", harness.name()))?;
    let mut seen = HashSet::new();
    let candidates: Vec<Suggestion> = suggestions
        .options
        .into_iter()
        .map(|mut entry| {
            let subject = match &mut entry {
                Suggestion::Category(subject) | Suggestion::Idea { subject, .. } => subject,
            };
            *subject = subject.split_whitespace().collect::<Vec<_>>().join(" ");
            entry
        })
        .filter(|entry| !entry.subject().is_empty() && seen.insert(entry.subject().to_lowercase()))
        .take(limit)
        .collect();
    if candidates.len() < 2 {
        return Err(format!(
            "{} returned fewer than two distinct suggestions",
            harness.name()
        )
        .into());
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
