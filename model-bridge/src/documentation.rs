//! Structured answers and reviewable documentation proposals. This module never writes records.
use crate::providers::{self, Proposal, ProposalBatch};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const INSTRUCTION: &str = "Answer the user's documentation request using the supplied context. Return only JSON matching the schema. If changes are requested, suggest proposals with evidence identifiers from the supplied context. Mark every proposal proposed; never accept a decision. Do not invent evidence. If context is insufficient, say so in the answer.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DocumentationResponse {
    pub answer: String,
    pub proposals: Vec<Proposal>,
}

pub fn schema() -> Value {
    let proposals = providers::proposal_schema()["properties"]["proposals"].clone();
    json!({"type":"object","additionalProperties":false,"required":["answer","proposals"],"properties":{"answer":{"type":"string"},"proposals":proposals}})
}

pub fn validate_response(input: &str) -> Result<DocumentationResponse, String> {
    let result: DocumentationResponse =
        serde_json::from_str(input).map_err(|e| format!("invalid documentation response: {e}"))?;
    if result.answer.trim().is_empty() {
        return Err("documentation answer is empty".into());
    }
    providers::validate_proposals(
        &serde_json::to_string(&ProposalBatch {
            proposals: result.proposals.clone(),
        })
        .map_err(|e| e.to_string())?,
    )?;
    Ok(result)
}

/// Context and request are encoded as data, not interpolated into the instruction.
pub fn prompt(question: &str, context: &str) -> Result<String, String> {
    if question.trim().is_empty() {
        return Err("documentation request is empty".into());
    }
    Ok(format!(
        "{}\n\n{}",
        INSTRUCTION,
        json!({"request":question,"context":context})
    ))
}
