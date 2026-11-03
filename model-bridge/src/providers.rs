//! Documentation inference only. Callers retain captured work until results are validated and saved.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{env, time::Duration};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProposalBatch {
    pub proposals: Vec<Proposal>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    pub kind: String,
    pub summary: String,
    pub review: String,
    pub evidence: Vec<String>,
}

/// Rejects unsupported states and uncited claims; acceptance always belongs to human review.
pub fn validate_proposals(input: &str) -> Result<ProposalBatch, String> {
    let batch: ProposalBatch =
        serde_json::from_str(input).map_err(|e| format!("invalid proposal JSON: {e}"))?;
    for p in &batch.proposals {
        if !["requirement", "decision", "experiment", "question", "task"].contains(&p.kind.as_str())
            || p.review != "proposed"
            || p.summary.trim().is_empty()
            || p.evidence.is_empty()
            || p.evidence.iter().any(|e| e.trim().is_empty())
        {
            return Err("proposal must have a supported kind, nonempty summary and evidence, and proposed review state".into());
        }
    }
    Ok(batch)
}

pub fn proposal_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["proposals"],"properties":{"proposals":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["kind","summary","review","evidence"],"properties":{"kind":{"type":"string","enum":["requirement","decision","experiment","question","task"]},"summary":{"type":"string"},"review":{"type":"string","enum":["proposed"]},"evidence":{"type":"array","items":{"type":"string"}}}}}}})
}
pub const INSTRUCTION: &str = "Extract documentation proposals from the supplied material. Return only JSON matching the schema. Cite supplied evidence identifiers; never accept a decision.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKind {
    OpenAiCompatible,
    Anthropic,
}
/// Credentials are resolved from an environment variable only at call time and never serialized.
#[derive(Debug, Clone)]
pub struct ApiProvider {
    pub kind: ApiKind,
    pub base_url: String,
    pub api_key_env: String,
    pub models: Vec<String>,
}
impl ApiProvider {
    fn client(&self) -> Result<reqwest::blocking::Client, String> {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "failed to create HTTP client".into())
    }
    fn key(&self) -> Result<String, String> {
        env::var(&self.api_key_env)
            .ok()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("missing credential in {}", self.api_key_env))
    }
    fn url(&self, suffix: &str) -> Result<String, String> {
        let parsed = reqwest::Url::parse(&self.base_url).map_err(|_| "invalid provider URL")?;
        if parsed.scheme() != "https"
            && !(parsed.scheme() == "http"
                && matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "::1")))
        {
            return Err("provider URL must use HTTPS or loopback HTTP".into());
        }
        if !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err("provider URL must not contain credentials, query, or fragment".into());
        }
        Ok(format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            suffix
        ))
    }
    fn request(
        &self,
        method: reqwest::Method,
        suffix: &str,
    ) -> Result<reqwest::blocking::RequestBuilder, String> {
        let key = self.key()?;
        let req = self.client()?.request(method, self.url(suffix)?);
        Ok(match self.kind {
            ApiKind::OpenAiCompatible => req.bearer_auth(key),
            ApiKind::Anthropic => req
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01"),
        })
    }
    fn response(req: reqwest::blocking::RequestBuilder) -> Result<Value, String> {
        let response = req
            .send()
            .map_err(|_| "provider request failed; captured work remains pending")?;
        if !response.status().is_success() {
            return Err(format!(
                "provider returned HTTP {}; captured work remains pending",
                response.status().as_u16()
            ));
        }
        response
            .json()
            .map_err(|_| "invalid provider response; captured work remains pending".into())
    }
    /// Configured names are not proof of availability; this requests the provider's model list.
    pub fn discover(&self) -> Result<Vec<String>, String> {
        let v = Self::response(self.request(reqwest::Method::GET, "models")?)?;
        let data = v
            .get("data")
            .and_then(Value::as_array)
            .ok_or("invalid model list")?;
        data.iter()
            .map(|m| {
                m.get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| "invalid model identifier".to_string())
            })
            .collect()
    }
    pub fn extract(&self, model: &str, input: &str) -> Result<ProposalBatch, String> {
        if model.trim().is_empty() {
            return Err("model identifier is empty".into());
        }
        let (suffix, body) = match self.kind {
            ApiKind::OpenAiCompatible => (
                "chat/completions",
                json!({"model":model,"messages":[{"role":"system","content":INSTRUCTION},{"role":"user","content":input}],"response_format":{"type":"json_schema","json_schema":{"name":"proposals","strict":true,"schema":proposal_schema()}}}),
            ),
            ApiKind::Anthropic => (
                "messages",
                json!({"model":model,"max_tokens":4096,"system":format!("{} JSON schema: {}", INSTRUCTION, proposal_schema()),"messages":[{"role":"user","content":input}]}),
            ),
        };
        let v = Self::response(self.request(reqwest::Method::POST, suffix)?.json(&body))?;
        let text = match self.kind {
            ApiKind::OpenAiCompatible => v
                .pointer("/choices/0/message/content")
                .and_then(Value::as_str),
            ApiKind::Anthropic => v.pointer("/content/0/text").and_then(Value::as_str),
        }
        .ok_or("provider omitted response text")?;
        validate_proposals(text)
    }
}
