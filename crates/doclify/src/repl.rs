//! Integration helpers for interactive documentation requests. No record writes occur here.
use doclify_model_bridge::{
    documentation::{self, DocumentationResponse},
    providers::{ApiKind, ApiProvider},
    Bridge, Request,
};
use serde_json::Value;
use std::time::Duration;

/// Availability information for the API model catalog shown by the picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelStatus {
    Discovered,
    ConfiguredOnly,
    DiscoveryUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerModel {
    pub id: String,
    pub status: ModelStatus,
}

/// Build picker choices without dropping the saved model when the provider is
/// offline. Discovery failure describes the catalog request, not the saved
/// model's actual availability.
pub fn picker_models(
    config: Option<&Value>,
    discovery: Result<Vec<String>, String>,
) -> Vec<PickerModel> {
    let configured = config
        .and_then(|c| c.get("model"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty());
    let mut models = Vec::new();
    match discovery {
        Ok(ids) => {
            for id in ids {
                if models.iter().any(|m: &PickerModel| m.id == id) {
                    continue;
                }
                models.push(PickerModel {
                    id,
                    status: ModelStatus::Discovered,
                });
            }
            if let Some(id) = configured.filter(|id| !models.iter().any(|m| m.id == *id)) {
                models.push(PickerModel {
                    id: id.into(),
                    status: ModelStatus::ConfiguredOnly,
                });
            }
        }
        Err(_) => {
            if let Some(id) = configured {
                models.push(PickerModel {
                    id: id.into(),
                    status: ModelStatus::DiscoveryUnavailable,
                });
            }
        }
    }
    models
}

fn field<'a>(config: &'a Value, name: &str) -> Result<&'a str, String> {
    config
        .get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("missing model configuration: {name}"))
}
fn api(config: &Value) -> Result<ApiProvider, String> {
    let kind = match field(config, "provider")? {
        "openai" => ApiKind::OpenAiCompatible,
        "anthropic" => ApiKind::Anthropic,
        _ => return Err("model discovery requires a configured API provider".into()),
    };
    Ok(ApiProvider {
        kind,
        base_url: field(config, "base_url")?.into(),
        api_key_env: field(config, "api_key_env")?.into(),
        models: vec![],
    })
}
/// Discover actual API model IDs. A failed request is an error, never a fabricated list.
pub fn discover_models(config: &Value) -> Result<Vec<String>, String> {
    api(config)?.discover()
}

/// Case-insensitive substring search over the discovered IDs.
pub fn filter_models(models: &[String], query: &str) -> Vec<String> {
    let query = query.to_lowercase();
    models
        .iter()
        .filter(|name| name.to_lowercase().contains(&query))
        .cloned()
        .collect()
}

/// Request a structured answer and proposed edits. Callers own context selection and review.
pub fn request(
    config: &Value,
    question: &str,
    context: &str,
) -> Result<DocumentationResponse, String> {
    let result = match field(config, "provider")? {
        "openai" | "anthropic" => {
            api(config)?.documentation(field(config, "model")?, question, context)?
        }
        provider @ ("claude" | "codex") => {
            let bridge = if provider == "claude" {
                Bridge::claude("claude")
            } else {
                Bridge::codex("codex")
            };
            let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
            let value = runtime
                .block_on(
                    bridge.execute(Request {
                        prompt: documentation::prompt(question, context)?,
                        schema: documentation::schema(),
                        model: config
                            .get("model")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        timeout: Duration::from_secs(120),
                    }),
                )
                .map_err(|e| format!("documentation request failed: {e:?}"))?;
            documentation::validate_response(&value.to_string())?
        }
        _ => return Err("invalid selected provider".into()),
    };
    // A citation must identify material actually sent to the model.
    if result
        .proposals
        .iter()
        .flat_map(|p| &p.evidence)
        .any(|e| !context.contains(e))
    {
        return Err("proposal cites evidence absent from supplied context".into());
    }
    Ok(result)
}
