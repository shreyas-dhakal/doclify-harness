//! Hook entry point. Runs inside the agent's hook process, so it must be fast,
//! never call a model, and never fail the agent. Stdout carries only
//! SessionStart context.

use std::fs;
use std::io::{self, Read, Write};
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
    let cwd = std::env::current_dir()?;
    let queued = enqueue(agent, &raw, &cwd);
    if let Some(out) = session_context(&raw, &cwd) {
        let _ = io::stdout().write_all(out.as_bytes());
        let _ = io::stdout().flush();
    }
    reconcile_on_start(&raw, &cwd);
    queued
}

/// Worktrees added after `install` get hooks the next time a session starts
/// anywhere in the repository. Silent and best-effort, like the rest of the hook.
fn reconcile_on_start(raw: &[u8], fallback_cwd: &Path) {
    let Ok(payload) = serde_json::from_slice::<Value>(raw) else {
        return;
    };
    if payload["hook_event_name"] != "SessionStart" {
        return;
    }
    let cwd = payload["cwd"].as_str().map_or(fallback_cwd, Path::new);
    if let Some(repo) = RepoInfo::discover(cwd) {
        crate::install::reconcile(&repo);
    }
}

/// SessionStart (startup, resume, clear, compact) gets bounded context from
/// reviewed local records in the `hookSpecificOutput` form both agents accept.
/// Delivery needs `.doclify/records`, not capture registration; any failure
/// yields `None` so the agent starts without context.
pub fn session_context(raw: &[u8], fallback_cwd: &Path) -> Option<String> {
    let payload: Value = serde_json::from_slice(raw).ok()?;
    if payload["hook_event_name"] != "SessionStart" {
        return None;
    }
    let cwd = payload["cwd"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| fallback_cwd.to_path_buf());
    // ponytail: second `git rev-parse` after enqueue's; share RepoInfo if hook latency matters.
    let repo = RepoInfo::discover(&cwd)?;
    let scope = doclify_context::Scope {
        branch: repo.branch.clone(),
        topic: None,
    };
    let context = doclify_context::build(&repo.worktree, &scope, &Default::default())
        .ok()?
        .or_else(|| {
            if !repo.registered() {
                return None;
            }
            let primary = repo.primary_worktree()?;
            doclify_context::build(&primary, &scope, &Default::default()).ok()?
        })?;
    let out = json!({
        "hookSpecificOutput": { "hookEventName": "SessionStart", "additionalContext": context.render() }
    });
    Some(out.to_string())
}

/// Writes one event file into the repository's queue. Returns `None` when the
/// working directory is not in a registered repository.
pub fn enqueue(agent: Agent, raw: &[u8], fallback_cwd: &Path) -> io::Result<Option<PathBuf>> {
    let payload: Value = serde_json::from_slice(raw)
        .unwrap_or_else(|_| json!({ "unparsed": String::from_utf8_lossy(raw) }));
    let cwd = payload["cwd"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| fallback_cwd.to_path_buf());
    let Some(repo) = RepoInfo::discover(&cwd) else {
        return Ok(None);
    };
    if !repo.registered() {
        return Ok(None);
    }
    let dir = repo.queue_dir();
    fs::create_dir_all(&dir)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let event = json!({
        "schema": SCHEMA,
        "agent": agent.name(),
        "received_at_ms": now.as_millis() as u64,
        "worktree": repo.worktree,
        "branch": repo.branch,
        "head": repo.head,
        "worktree_dirty": worktree_dirty(&repo.worktree),
        "payload": payload,
    });
    // Unique and time-sortable; write-then-rename so readers never see partial files.
    let name = format!(
        "{:020}-{}-{}",
        now.as_nanos(),
        std::process::id(),
        agent.name()
    );
    let tmp = dir.join(format!("{name}.tmp"));
    let path = dir.join(format!("{name}.json"));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(&tmp)?
        .write_all(&serde_json::to_vec(&event)?)?;
    fs::rename(&tmp, &path)?;
    Ok(Some(path))
}

/// Queued events in arrival order. The worker deletes each file only after it
/// has durably processed it, so a crash replays rather than loses events.
pub fn pending(queue_dir: &Path) -> io::Result<Vec<(PathBuf, Value)>> {
    let mut paths = fs::read_dir(queue_dir)?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<io::Result<Vec<_>>>()?;
    paths.retain(|p| p.extension().is_some_and(|x| x == "json"));
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let value = serde_json::from_slice(&fs::read(&p)?).map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", p.display()))
            })?;
            Ok((p, value))
        })
        .collect()
}

/// Whether relevant tracked or untracked files differ from HEAD. Unknown on Git failure.
pub fn worktree_dirty(repo: &Path) -> Option<bool> {
    let out = std::process::Command::new("git")
        .current_dir(repo)
        .args(["status", "--porcelain", "--untracked-files=all", "--", ".", ":(exclude)docs/doclify", ":(exclude).doclify", ":(exclude).claude/settings.local.json", ":(exclude).codex/hooks.json"])
        .output().ok()?;
    out.status.success().then(|| !out.stdout.is_empty())
}
