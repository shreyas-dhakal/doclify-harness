use std::{env, error::Error};

use doclify_capture::RepoInfo;
use serde_json::{json, Value};

#[derive(Clone, Debug)]
pub struct ModelChoice {
    pub id: String,
    pub status: String,
}

pub enum ActionResult {
    Message(String),
    ModelChoices(Vec<ModelChoice>),
    Quit,
}

pub fn execute(input: &str) -> ActionResult {
    if !input.starts_with('/') {
        return match crate::documentation_request(input) {
            Ok(response) => ActionResult::Message(format_documentation(&response)),
            Err(error) => ActionResult::Message(format!("Couldn't create a draft: {error}")),
        };
    }

    let args: Vec<String> = input[1..].split_whitespace().map(str::to_owned).collect();
    let Some(command) = args.first().map(String::as_str) else {
        return ActionResult::Message(
            "Type a command after `/`, or tell Doclify what the docs should explain.".into(),
        );
    };

    match command {
        "quit" | "exit" => ActionResult::Quit,
        "help" => ActionResult::Message(help_text()),
        "model" if args.len() == 1 => match model_choices() {
            Ok(choices) => ActionResult::ModelChoices(choices),
            Err(error) => ActionResult::Message(format!("Couldn't load AI choices: {error}")),
        },
        "docs" => match crate::start_docs_viewer() {
            Ok((url, _server)) => {
                crate::open_viewer(&url);
                ActionResult::Message(format!("Opened the docs in your browser: {url}"))
            }
            Err(error) => ActionResult::Message(format!("Couldn't open the docs: {error}")),
        },
        _ => match crate::run(&args) {
            Ok(value) => ActionResult::Message(format_command(command, &value)),
            Err(error) => ActionResult::Message(format!("Couldn't finish that command: {error}")),
        },
    }
}

pub fn choose_model(choice: &str) -> Result<String, String> {
    if matches!(choice, "claude" | "codex") {
        let args = ["model".to_owned(), "set".to_owned(), choice.to_owned()];
        crate::run(&args).map_err(|error| error.to_string())?;
        return Ok(format!(
            "AI selected: {choice}. Sign-in is checked when you use it."
        ));
    }

    let cwd = env::current_dir().map_err(|error| error.to_string())?;
    let repo = RepoInfo::discover(&cwd).ok_or("Open Doclify from inside a Git project first.")?;
    let mut config = crate::load_config(&repo)
        .map_err(|error| error.to_string())?
        .ok_or("Set up that AI first with `doclify init`.")?;
    config["model"] = json!(choice);
    crate::save_config(&repo, &config).map_err(|error| error.to_string())?;
    Ok(format!(
        "AI selected: {choice}. Sign-in is checked when you use it."
    ))
}

fn model_choices() -> Result<Vec<ModelChoice>, Box<dyn Error>> {
    let cwd = env::current_dir()?;
    let repo = RepoInfo::discover(&cwd).ok_or("Open Doclify from inside a Git project first.")?;
    let catalog = crate::run(&["model".into()])?;
    let mut choices = Vec::new();
    for provider in ["claude", "codex"] {
        let bridge = &catalog["bridges"][provider];
        let status = if bridge["capability"] == "unavailable" {
            "Not available on this setup".into()
        } else {
            "Ready · sign-in checked when used".into()
        };
        choices.push(ModelChoice {
            id: provider.into(),
            status,
        });
    }

    let config = crate::load_config(&repo)?;
    if let Some(config) =
        config.filter(|config| matches!(config["provider"].as_str(), Some("openai" | "anthropic")))
    {
        let discovery = doclify::repl::discover_models(&config);
        let api_models = doclify::repl::picker_models(Some(&config), discovery);
        choices.extend(api_models.into_iter().map(|model| ModelChoice {
            id: model.id,
            status: match model.status {
                doclify::repl::ModelStatus::Discovered => {
                    "Found · sign-in checked when used".into()
                }
                doclify::repl::ModelStatus::ConfiguredOnly => "Saved · not found online".into(),
                doclify::repl::ModelStatus::DiscoveryUnavailable => {
                    "Couldn't check · saved choice kept".into()
                }
            },
        }));
    }
    Ok(choices)
}

fn format_documentation(response: &Value) -> String {
    let answer = response["answer"].as_str().unwrap_or("Draft complete.");
    let proposals = response["proposals"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if proposals.is_empty() {
        return answer.to_owned();
    }

    let mut message = format!(
        "{answer}\n\nDrafted {}:",
        quantity(proposals.len() as u64, "suggestion", "suggestions")
    );
    for proposal in proposals {
        let title = proposal["title"].as_str().unwrap_or("Untitled suggestion");
        let id = proposal["id"].as_str().unwrap_or("unknown");
        let version = proposal["version"].as_u64().unwrap_or(1);
        message.push_str(&format!("\n• {title}  ({id} v{version})"));
    }
    message.push_str("\nUse /review to check them. Doclify won't add a suggestion to the docs until you approve it.");
    message
}

fn format_command(command: &str, value: &Value) -> String {
    match command {
        "sync" => {
            let events = value["reconciled_events"].as_u64().unwrap_or(0);
            let pending = value["pending_extraction"].as_u64().unwrap_or(0);
            let unsupported = value["unsupported_transcript_count"].as_u64().unwrap_or(0);
            format!(
                "Session update complete · {} saved · {} waiting to become drafts · {} couldn't be read",
                quantity(events, "new record", "new records"),
                quantity(pending, "session", "sessions"),
                quantity(unsupported, "session file", "session files")
            )
        }
        "review" if value["pending_review"].is_array() => {
            let pending = value["pending_review"].as_array().unwrap();
            if pending.is_empty() {
                return "No suggestions need your review.".into();
            }
            let mut message = format!(
                "{} need your review:",
                quantity(pending.len() as u64, "suggestion", "suggestions")
            );
            for record in pending {
                message.push_str(&format!(
                    "\n• {}  ({} v{})",
                    record["title"].as_str().unwrap_or("Untitled suggestion"),
                    record["id"].as_str().unwrap_or("unknown"),
                    record["version"].as_u64().unwrap_or(1)
                ));
            }
            message.push_str("\nDecide with `/review <id> <version> accept`, `reject`, or `supersede` (replace an older suggestion).");
            message
        }
        "review" => review_result(value["review"].as_str().unwrap_or("saved")),
        "status" => format_status(value),
        "handoff" => format_handoff(value),
        "model" => {
            let selected = value["selected"]
                .get("model")
                .or_else(|| value["selected"].get("provider"));
            format!(
                "AI selected: {} · sign-in is checked when used",
                selected.and_then(Value::as_str).unwrap_or("unknown")
            )
        }
        _ => serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
    }
}

fn format_status(value: &Value) -> String {
    let selected = value
        .get("selected_model")
        .and_then(|model| model.get("model").or_else(|| model.get("provider")))
        .and_then(Value::as_str)
        .unwrap_or("none selected");
    let pending_review = value["pending_review"].as_array().map_or(0, Vec::len);
    let unsupported = value["unsupported_transcript_count"].as_u64().unwrap_or(0);
    let capture = if value["registered"].as_bool().unwrap_or(false) {
        "Session capture is on"
    } else {
        "Session capture is off · run `doclify init` here to turn it on"
    };
    format!(
        "{capture}\nAI: {selected}\n{} to import · {} waiting for drafts · {} to review\n{} couldn't be read",
        quantity(value["pending_events"].as_u64().unwrap_or(0), "saved item", "saved items"),
        quantity(value["pending_extraction"].as_u64().unwrap_or(0), "session", "sessions"),
        quantity(pending_review as u64, "suggestion", "suggestions"),
        quantity(unsupported, "session file", "session files")
    )
}

fn format_handoff(value: &Value) -> String {
    if value.is_null() {
        return "No approved project notes found for this topic yet.".into();
    }
    let mut message = format!(
        "Project notes for {}",
        value["scope"].as_str().unwrap_or("this project")
    );
    if let Some(items) = value["constraints"].as_array() {
        if !items.is_empty() {
            message.push_str("\n\nApproved notes:");
            for item in items {
                message.push_str(&format!(
                    "\n• {} — {}",
                    item["title"].as_str().unwrap_or("Untitled"),
                    item["statement"].as_str().unwrap_or("")
                ));
            }
        }
    }
    if let Some(items) = value["open_work"].as_array() {
        if !items.is_empty() {
            message.push_str("\n\nUnfinished work (for context):");
            for item in items {
                message.push_str(&format!(
                    "\n• {} · {}",
                    item["title"].as_str().unwrap_or("Untitled"),
                    item["status"].as_str().unwrap_or("unknown")
                ));
            }
        }
    }
    message
}

fn help_text() -> String {
    "Tell Doclify what to write. Approve each draft before it reaches the docs.\n/sync · import saved sessions and refresh docs\n/review · see and decide on suggestions\n/docs · open docs in your browser\n/status · see setup and pending work\n/handoff [topic] · prepare project notes\n/model · choose an AI\n/help · commands · /quit · exit".into()
}

fn quantity(count: u64, singular: &str, plural: &str) -> String {
    let noun = if count == 1 { singular } else { plural };
    if noun.is_empty() {
        count.to_string()
    } else {
        format!("{count} {noun}")
    }
}

fn review_result(result: &str) -> String {
    match result {
        "accepted" => "Suggestion approved.".into(),
        "rejected" => "Suggestion rejected.".into(),
        "superseded" => "Suggestion replaced.".into(),
        _ => "Suggestion review saved.".into(),
    }
}
