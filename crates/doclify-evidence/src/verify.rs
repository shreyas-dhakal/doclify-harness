//! Test-run evidence and session-to-commit associations from captured events.
//! Accepts raw hook events, queued `{"source":"hook","event":..}` jobs, and
//! `{"source":"transcript",..}` jobs as written by `doclify-capture`.
use crate::{mentions, Basis, Commit};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::{BTreeMap, HashMap}, path::Path};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestSource {
    /// Doclify saw the command run through a tool hook.
    Observed,
    /// An agent said tests passed; nothing was observed.
    Claimed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestOutcome {
    Passed,
    Failed,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestRun {
    pub source: TestSource,
    pub outcome: TestOutcome,
    pub agent: String,
    pub session: String,
    /// HEAD when captured. Uncommitted working-tree changes are not captured.
    pub revision: Option<String>,
    pub observed_at_ms: u64,
    /// Exactly what was verified: the command run, or the claim's text.
    /// Never implies coverage beyond it.
    pub scope: String,
    /// Set when non-documentation files changed after `revision`, or the
    /// revision is unknown, so this result no longer describes HEAD.
    pub stale: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionLink {
    pub agent: String,
    pub session: String,
    pub commit: String,
    pub basis: Basis,
    pub reason: String,
}

// ponytail: fixed list of test runners; make configurable if repos use custom scripts.
const RUNNERS: [&str; 16] = [
    "cargo test", "cargo nextest", "npm test", "npm run test", "pnpm test", "pnpm run test",
    "yarn test", "bun test", "pytest", "go test", "make test", "vitest", "jest", "rspec",
    "mvn test", "gradle test",
];
const CLAIMS: [&str; 6] =
    ["tests pass", "tests passed", "tests are passing", "tests now pass", "test suite passes", "all green"];
const MAX_SCOPE: usize = 300;

/// `(envelope, transcript)` where the envelope is the capture hook event.
fn unwrap(v: &Value) -> (&Value, Option<&Value>) {
    match v["source"].as_str() {
        Some("hook") => (&v["event"], None),
        Some("transcript") => (&v["context"], Some(v)),
        _ => (v, None),
    }
}
fn session_of<'a>(env: &'a Value, job: Option<&'a Value>) -> &'a str {
    job.and_then(|j| j["session_id"].as_str())
        .or_else(|| env["payload"]["session_id"].as_str())
        .unwrap_or("unknown")
}

pub fn test_runs(captured: &[Value]) -> Vec<TestRun> {
    let mut runs: Vec<TestRun> = captured.iter().filter_map(test_run).collect();
    runs.sort_by(|a, b| (a.observed_at_ms, &a.session, &a.scope).cmp(&(b.observed_at_ms, &b.session, &b.scope)));
    runs.dedup();
    runs
}

fn test_run(v: &Value) -> Option<TestRun> {
    let (env, job) = unwrap(v);
    let p = &env["payload"];
    let (source, outcome, scope) = match job {
        Some(j) => {
            if j["kind"] != "Assistant" {
                return None;
            }
            let text = j["text"].as_str()?;
            let lower = text.to_ascii_lowercase();
            let at = CLAIMS.iter().filter_map(|c| lower.find(c)).min()?;
            (TestSource::Claimed, TestOutcome::Passed, excerpt(text, at))
        }
        None => {
            let event = p["hook_event_name"].as_str()?;
            if !event.starts_with("PostToolUse") {
                return None;
            }
            let command = match &p["tool_input"]["command"] {
                Value::String(s) => s.clone(),
                Value::Array(a) => a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "),
                _ => return None,
            };
            let normalized = command.split_whitespace().collect::<Vec<_>>().join(" ");
            if !RUNNERS.iter().any(|r| mentions(&normalized, r)) {
                return None;
            }
            (TestSource::Observed, outcome(env["agent"].as_str(), event, &p["tool_response"]), clip(&normalized))
        }
    };
    Some(TestRun {
        source,
        outcome,
        agent: env["agent"].as_str().unwrap_or("unknown").into(),
        session: session_of(env, job).into(),
        revision: env["head"].as_str().map(Into::into),
        observed_at_ms: env["received_at_ms"].as_u64().unwrap_or_default(),
        scope,
        stale: None,
    })
}

fn outcome(agent: Option<&str>, event: &str, response: &Value) -> TestOutcome {
    let code = ["exit_code", "exitCode"]
        .iter()
        .find_map(|k| response[k].as_i64())
        .or_else(|| response.as_str()?.strip_prefix("Exit code: ")?.lines().next()?.trim().parse().ok());
    match code {
        Some(0) => TestOutcome::Passed,
        Some(_) => TestOutcome::Failed,
        None if event == "PostToolUseFailure" || response["interrupted"] == true => TestOutcome::Failed,
        // Claude Code fires PostToolUse only for commands that succeeded.
        None if agent == Some("claude") && event == "PostToolUse" => TestOutcome::Passed,
        None => TestOutcome::Unknown,
    }
}

fn clip(s: &str) -> String {
    match s.char_indices().nth(MAX_SCOPE) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.into(),
    }
}
/// The sentence around a claim, so the scope is what the agent actually said.
fn excerpt(text: &str, at: usize) -> String {
    let start = text[..at].rfind(['.', '\n', '!', '?']).map_or(0, |i| i + 1);
    let end = text[at..].find(['.', '\n', '!', '?']).map_or(text.len(), |i| at + i);
    clip(text[start..end].trim())
}

/// Marks runs stale when anything but generated documentation changed between
/// their revision and `head`.
// ponytail: any non-doc change counts as relevant; map test scope to paths if this is too noisy.
pub fn mark_stale(repo: &Path, head: Option<&str>, runs: &mut [TestRun]) {
    let mut cache: HashMap<String, Option<String>> = HashMap::new();
    for run in runs {
        run.stale = match (&run.revision, head) {
            (None, _) => Some("revision not captured".into()),
            (Some(_), None) => Some("repository has no HEAD".into()),
            (Some(rev), Some(head)) => cache
                .entry(rev.clone())
                .or_insert_with(|| changed_since(repo, rev, head))
                .clone(),
        };
    }
}
fn changed_since(repo: &Path, rev: &str, head: &str) -> Option<String> {
    if rev.starts_with('-') {
        return Some("invalid revision".into());
    }
    match crate::git(repo, &["diff", "--name-only", rev, head, "--", ".", ":(exclude)docs/doclify", ":(exclude).doclify"]) {
        Err(_) => Some(format!("revision {} not found locally", short(rev))),
        Ok(out) => match out.lines().count() {
            0 => None,
            n => Some(format!("{n} file(s) changed since {}", short(rev))),
        },
    }
}
fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// Session-to-commit associations. A commit message naming the session id is
/// direct; HEAD moving to a commit during a session is only a suggestion,
/// since another process may have made it.
pub fn sessions(captured: &[Value], commits: &[Commit]) -> Vec<SessionLink> {
    // (agent, session) -> heads in arrival order.
    let mut seen: BTreeMap<(String, String), Vec<(u64, String)>> = BTreeMap::new();
    for v in captured {
        let (env, job) = unwrap(v);
        let session = session_of(env, job);
        if session == "unknown" {
            continue;
        }
        let key = (env["agent"].as_str().unwrap_or("unknown").to_owned(), session.to_owned());
        let heads = seen.entry(key).or_default();
        if let Some(head) = env["head"].as_str() {
            heads.push((env["received_at_ms"].as_u64().unwrap_or_default(), head.into()));
        }
    }
    let mut links = Vec::new();
    for ((agent, session), mut heads) in seen {
        heads.sort();
        for c in commits {
            let link = |basis, reason: &str| SessionLink {
                agent: agent.clone(),
                session: session.clone(),
                commit: c.sha.clone(),
                basis,
                reason: reason.into(),
            };
            if mentions(&c.subject, &session) || mentions(&c.body, &session) {
                links.push(link(Basis::Direct, "commit message cites session"));
            } else if heads.windows(2).any(|w| w[0].1 != c.sha && w[1].1 == c.sha) {
                links.push(link(Basis::Suggested, "HEAD moved to this commit during session"));
            }
        }
    }
    links.sort_by(|a, b| (&a.session, &a.commit).cmp(&(&b.session, &b.commit)));
    links
}
