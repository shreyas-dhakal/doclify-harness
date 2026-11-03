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
pub struct ExistingRecord {
    pub id: String,
    pub version: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    #[serde(default)]
    pub existing_record: Option<ExistingRecord>,
    #[serde(default)]
    pub experiment_outcome: Option<String>,
    pub kind: String,
    pub summary: String,
    pub review: String,
    pub evidence: Vec<String>,
    #[serde(default)]
    pub rationale: Option<String>,
    #[serde(default)]
    pub excerpt: Option<String>,
    #[serde(default)]
    pub implementation: Option<String>,
}

/// Rejects unsupported states and uncited claims; acceptance always belongs to human review.
pub fn validate_proposals(input: &str) -> Result<ProposalBatch, String> {
    let batch: ProposalBatch =
        serde_json::from_str(input).map_err(|e| format!("invalid proposal JSON: {e}"))?;
    for p in &batch.proposals {
        if !["requirement", "decision", "experiment", "question", "task"].contains(&p.kind.as_str())
            || p.review != "proposed"
            || p.summary.trim().is_empty()
            || p.implementation.as_deref().is_some_and(|s| {
                !["unknown", "planned", "partial", "implemented", "reverted"].contains(&s)
            })
            || p.experiment_outcome.as_deref().is_some_and(|s| {
                p.kind != "experiment" || !["active", "completed", "abandoned"].contains(&s)
            })
            || p.existing_record
                .as_ref()
                .is_some_and(|r| r.id.is_empty() || r.version == 0)
            || p.evidence.is_empty()
            || p.evidence.iter().any(|e| e.trim().is_empty())
        {
            return Err("proposal must have a supported kind, nonempty summary and evidence, and proposed review state".into());
        }
    }
    Ok(batch)
}

pub fn proposal_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["proposals"],"properties":{"proposals":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["kind","summary","review","evidence","rationale","excerpt","implementation","existing_record","experiment_outcome"],"properties":{"existing_record":{"anyOf":[{"type":"null"},{"type":"object","additionalProperties":false,"required":["id","version"],"properties":{"id":{"type":"string"},"version":{"type":"integer","minimum":1}}}]},"experiment_outcome":{"type":["string","null"],"enum":["active","completed","abandoned",null]},"kind":{"type":"string","enum":["requirement","decision","experiment","question","task"]},"summary":{"type":"string"},"review":{"type":"string","enum":["proposed"]},"evidence":{"type":"array","items":{"type":"string"}},"rationale":{"type":["string","null"]},"excerpt":{"type":["string","null"]},"implementation":{"type":["string","null"],"enum":["unknown","planned","partial","implemented","reverted",null]}}}}}})
}
pub const INSTRUCTION: &str = "Extract documentation proposals from the supplied material. Return only JSON matching the schema. Cite supplied evidence identifiers; never accept a decision. Preserve supported rationale, source excerpts, and implementation status; use null when unknown. For a revision, set existing_record to the exact id and latest version supplied in existing_records; otherwise use null. Do not merge unrelated claims. Set experiment_outcome only for experiments. Excerpts must be verbatim from the supplied event.";

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
    /// Answer a documentation request; returned proposals remain unaccepted and unwritten.
    pub fn documentation(
        &self,
        model: &str,
        question: &str,
        context: &str,
    ) -> Result<crate::documentation::DocumentationResponse, String> {
        if model.trim().is_empty() {
            return Err("model identifier is empty".into());
        }
        let prompt = crate::documentation::prompt(question, context)?;
        let schema = crate::documentation::schema();
        let (suffix, body) = match self.kind {
            ApiKind::OpenAiCompatible => (
                "chat/completions",
                json!({"model":model,"messages":[{"role":"system","content":crate::documentation::INSTRUCTION},{"role":"user","content":prompt}],"response_format":{"type":"json_schema","json_schema":{"name":"documentation_response","strict":true,"schema":schema}}}),
            ),
            ApiKind::Anthropic => (
                "messages",
                json!({"model":model,"max_tokens":4096,"system":format!("{} JSON schema: {}", crate::documentation::INSTRUCTION, schema),"messages":[{"role":"user","content":prompt}]}),
            ),
        };
        let value = Self::response(self.request(reqwest::Method::POST, suffix)?.json(&body))?;
        let text = match self.kind {
            ApiKind::OpenAiCompatible => value
                .pointer("/choices/0/message/content")
                .and_then(Value::as_str),
            ApiKind::Anthropic => value.pointer("/content/0/text").and_then(Value::as_str),
        }
        .ok_or("provider omitted response text")?;
        crate::documentation::validate_response(text)
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
