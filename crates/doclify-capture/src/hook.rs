//! Hook entry point. Runs inside the agent's hook process, so it must be fast,
//! never call a model, never print to stdout, and never fail the agent.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::{Agent, RepoInfo, BRIDGE_ENV};

pub const SCHEMA: u64 = 1;
// ponytail: hard cap on hook payload; raise if real payloads get truncated.
const MAX_PAYLOAD: u64 = 16 << 20;

/// Reads the hook payload from stdin and enqueues it. Errors are swallowed by
/// the caller; this returns them only so tests can see what happened.
pub fn run(agent: Agent) -> io::Result<Option<PathBuf>> {
    if std::env::var_os(BRIDGE_ENV).is_some() {
        io::copy(&mut io::stdin(), &mut io::sink())?;
        return Ok(None);
    }
    let mut raw = Vec::new();
    io::stdin().take(MAX_PAYLOAD).read_to_end(&mut raw)?;
    io::copy(&mut io::stdin(), &mut io::sink())?;
    enqueue(agent, &raw, &std::env::current_dir()?)
}

/// Writes one event file into the repository's queue. Returns `None` when the
/// working directory is not in a registered repository.
pub fn enqueue(agent: Agent, raw: &[u8], fallback_cwd: &Path) -> io::Result<Option<PathBuf>> {
    let payload: Value = serde_json::from_slice(raw)
        .unwrap_or_else(|_| json!({ "unparsed": String::from_utf8_lossy(raw) }));
    let cwd = payload["cwd"].as_str().map(PathBuf::from).unwrap_or_else(|| fallback_cwd.to_path_buf());
    let Some(repo) = RepoInfo::discover(&cwd) else { return Ok(None) };
    let dir = repo.queue_dir();
    if !dir.is_dir() {
        return Ok(None);
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let event = json!({
        "schema": SCHEMA,
        "agent": agent.name(),
        "received_at_ms": now.as_millis() as u64,
        "worktree": repo.worktree,
        "branch": repo.branch,
        "head": repo.head,
        "payload": payload,
    });
    // Unique and time-sortable; write-then-rename so readers never see partial files.
    let name = format!("{:020}-{}-{}", now.as_nanos(), std::process::id(), agent.name());
    let tmp = dir.join(format!("{name}.tmp"));
    let path = dir.join(format!("{name}.json"));
    fs::write(&tmp, serde_json::to_vec(&event)?)?;
    fs::rename(&tmp, &path)?;
    Ok(Some(path))
}

/// Queued events in arrival order. The worker deletes each file only after it
/// has durably processed it, so a crash replays rather than loses events.
pub fn pending(queue_dir: &Path) -> io::Result<Vec<(PathBuf, Value)>> {
    let mut paths: Vec<PathBuf> = fs::read_dir(queue_dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    Ok(paths
        .into_iter()
        .filter_map(|p| {
            let v = serde_json::from_slice(&fs::read(&p).ok()?).ok()?;
            Some((p, v))
        })
        .collect())
}
