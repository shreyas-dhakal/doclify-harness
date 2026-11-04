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
            Err(error) => ActionResult::Message(format!("Request failed: {error}")),
        };
    }

    let args: Vec<String> = input[1..]
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let Some(command) = args.first().map(String::as_str) else {
        return ActionResult::Message("Enter a command after /, or type a documentation request.".into());
    };

    match command {
        "quit" | "exit" => ActionResult::Quit,
        "help" => ActionResult::Message(help_text()),
        "model" if args.len() == 1 => match model_choices() {
            Ok(choices) => ActionResult::ModelChoices(choices),
            Err(error) => ActionResult::Message(format!("Could not load model choices: {error}")),
        },
        "docs" => match crate::start_docs_viewer() {
            Ok((url, _server)) => {
                crate::open_viewer(&url);
                ActionResult::Message(format!("Documentation viewer opened: {url}"))
            }
            Err(error) => ActionResult::Message(format!("Could not open documentation viewer: {error}")),
        },
        _ => match crate::run(&args) {
            Ok(value) => ActionResult::Message(format_command(command, &value)),
            Err(error) => ActionResult::Message(format!("Command failed: {error}")),
        },
    }
}

pub fn choose_model(choice: &str) -> Result<String, String> {
    if matches!(choice, "claude" | "codex") {
        let args = ["model".to_owned(), "set".to_owned(), choice.to_owned()];
        crate::run(&args).map_err(|error| error.to_string())?;
        return Ok(format!("Selected {choice}. Authentication is checked when it is used."));
    }

    let cwd = env::current_dir().map_err(|error| error.to_string())?;
    let repo = RepoInfo::discover(&cwd).ok_or("not inside a Git repository")?;
    let mut config = crate::load_config(&repo)
        .map_err(|error| error.to_string())?
        .ok_or("configure an API provider before selecting an API model")?;
    config["model"] = json!(choice);
    crate::save_config(&repo, &config).map_err(|error| error.to_string())?;
    Ok(format!("Selected model {choice}. Authentication is checked when it is used."))
}

fn model_choices() -> Result<Vec<ModelChoice>, Box<dyn Error>> {
    let cwd = env::current_dir()?;
    let repo = RepoInfo::discover(&cwd).ok_or("not inside a Git repository")?;
    let catalog = crate::run(&["model".into()])?;
    let mut choices = Vec::new();
    for provider in ["claude", "codex"] {
        let bridge = &catalog["bridges"][provider];
        let status = if bridge["capability"] == "unavailable" {
            format!(
                "unavailable: {}",
                bridge["reason"].as_str().unwrap_or("capability check failed")
            )
        } else {
            "available · authentication unverified".into()
        };
        choices.push(ModelChoice {
            id: provider.into(),
            status,
        });
    }

    let config = crate::load_config(&repo)?;
    if let Some(config) = config.filter(|config| {
        matches!(config["provider"].as_str(), Some("openai" | "anthropic"))
    }) {
        let discovery = doclify::repl::discover_models(&config);
        let api_models = doclify::repl::picker_models(Some(&config), discovery);
        choices.extend(api_models.into_iter().map(|model| ModelChoice {
            id: model.id,
            status: match model.status {
                doclify::repl::ModelStatus::Discovered => {
                    "discovered · authentication unverified".into()
                }
                doclify::repl::ModelStatus::ConfiguredOnly => {
                    "not returned by discovery · availability unverified".into()
                }
                doclify::repl::ModelStatus::DiscoveryUnavailable => {
                    "discovery failed · configured availability unknown".into()
                }
            },
        }));
    }
    Ok(choices)
}

fn format_documentation(response: &Value) -> String {
    let answer = response["answer"].as_str().unwrap_or("Draft complete.");
    let proposals = response["proposals"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if proposals.is_empty() {
        return answer.to_owned();
    }

    let mut message = format!("{answer}\n\nDrafted {} proposal(s):", proposals.len());
    for proposal in proposals {
        let title = proposal["title"].as_str().unwrap_or("Untitled proposal");
        let id = proposal["id"].as_str().unwrap_or("unknown");
        let version = proposal["version"].as_u64().unwrap_or(1);
        message.push_str(&format!("\n• {title}  ({id} v{version})"));
    }
    message.push_str("\nReview them with /review; nothing is accepted automatically.");
    message
}

fn format_command(command: &str, value: &Value) -> String {
    match command {
        "sync" => {
            let events = value["reconciled_events"].as_u64().unwrap_or(0);
            let pending = value["pending_extraction"].as_u64().unwrap_or(0);
            let unsupported = value["unsupported_transcript_count"].as_u64().unwrap_or(0);
            format!("Sync complete · {events} events captured · {pending} extraction jobs pending · {unsupported} unsupported transcripts")
        }
        "review" if value["pending_review"].is_array() => {
            let pending = value["pending_review"].as_array().unwrap();
            if pending.is_empty() {
                return "No proposals are waiting for review.".into();
            }
            let mut message = format!("{} proposal(s) waiting for review:", pending.len());
            for record in pending {
                message.push_str(&format!(
                    "\n• {}  ({} v{})",
                    record["title"].as_str().unwrap_or("Untitled proposal"),
                    record["id"].as_str().unwrap_or("unknown"),
                    record["version"].as_u64().unwrap_or(1)
                ));
            }
            message.push_str("\nUse /review <id> <version> accept|reject|supersede to decide.");
            message
        }
        "review" => format!(
            "Proposal review updated · {}",
            value["review"].as_str().unwrap_or("saved")
        ),
        "status" => format_status(value),
        "handoff" => format_handoff(value),
        "model" => {
            let selected = value["selected"].get("model").or_else(|| value["selected"].get("provider"));
            format!(
                "Selected model {} · authentication unverified",
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
        .unwrap_or("no model selected");
    let pending_review = value["pending_review"].as_array().map_or(0, Vec::len);
    let unsupported = value["unsupported_transcript_count"].as_u64().unwrap_or(0);
    format!(
        "{} · model {selected} · {} captured events queued · {} extraction jobs · {pending_review} proposals to review · {unsupported} unsupported transcripts",
        if value["registered"].as_bool().unwrap_or(false) {
            "Hooks installed"
        } else {
            "Hooks not installed"
        },
        value["pending_events"].as_u64().unwrap_or(0),
        value["pending_extraction"].as_u64().unwrap_or(0)
    )
}

fn format_handoff(value: &Value) -> String {
    if value.is_null() {
        return "No reviewed context is available for this topic yet.".into();
    }
    let mut message = format!("Handoff for {}", value["scope"].as_str().unwrap_or("this repository"));
    if let Some(items) = value["constraints"].as_array() {
        if !items.is_empty() {
            message.push_str("\n\nAccepted context:");
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
            message.push_str("\n\nOpen work (context, not instructions):");
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
    "Ask in plain language to draft documentation proposals.\n\nCommands:\n/sync · capture sessions and update docs\n/review · see proposals waiting for a decision\n/docs · open the documentation viewer\n/status · inspect the local workflow\n/handoff [topic] · prepare reviewed context\n/model · choose a model\n/help · show this help\n/quit · exit\n\nProposals stay unaccepted until you review them.".into()
}
