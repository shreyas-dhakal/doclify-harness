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
/// Hooks whose transcript cannot be attributed yet wait here, under the queue.
pub const DEFERRED: &str = "deferred";
// ponytail: transcript prefix scanned for attribution; past it the worker decides.
const ATTRIBUTION_SCAN: u64 = 1 << 20;
/// Deferred hooks still unattributable after this go to the worker anyway, so
/// its rejection makes them visible instead of hiding them forever.
const MAX_DEFERRAL_MS: u128 = 24 * 60 * 60 * 1000;

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
    // Registered linked worktrees also see the primary's reviewed records;
    // local copies win ties (see `doclify_context::build_merged`).
    let primary = repo
        .registered()
        .then(|| repo.primary_worktree())
        .flatten()
        .filter(|p| *p != repo.worktree);
    let worktrees: Vec<&Path> = [Some(repo.worktree.as_path()), primary.as_deref()]
        .into_iter()
        .flatten()
        .collect();
    let context = doclify_context::build_merged(&worktrees, &scope, &Default::default()).ok()??;
    let _ = crate::metrics::increment(&repo, "handoff_deliveries", 1);
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
    // Claude's first hooks fire before its transcript names a `cwd`, so the
    // worker cannot verify ownership yet and would reject them.
    let dir = if attributable(agent, &event["payload"]) {
        dir
    } else {
        dir.join(DEFERRED)
    };
    fs::create_dir_all(&dir)?;
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
    let _ = release_deferred(&repo.queue_dir());
    Ok(Some(path))
}

/// Whether the worker can attribute this hook's transcript: Codex starts
/// with `session_meta`, Claude needs some line carrying `cwd`. Only a missing
/// transcript, or one of valid JSON lines that has not named its owner yet,
/// is unattributable; anything else (including non-transcripts) goes to the
/// worker, whose ownership check rejects it visibly.
fn attributable(agent: Agent, payload: &Value) -> bool {
    let Some(path) = payload["transcript_path"].as_str() else {
        return true;
    };
    let Ok(file) = fs::File::open(path) else {
        return false;
    };
    let mut bytes = Vec::new();
    if file
        .take(ATTRIBUTION_SCAN + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return false;
    }
    if bytes.len() as u64 > ATTRIBUTION_SCAN {
        return true;
    }
    // The trailing segment may still be being written.
    let complete = &bytes[..bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1)];
    let Ok(lines) = complete
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(serde_json::from_slice::<Value>)
        .collect::<Result<Vec<_>, _>>()
    else {
        return true;
    };
    match agent {
        Agent::Codex => lines.first().is_some_and(|v| v["type"] == "session_meta"),
        Agent::Claude => lines.iter().any(|v| v["cwd"].is_string()),
    }
}

/// Moves deferred hooks back into the queue once attributable (or too old to
/// wait), keeping their time-sorted names so the worker sees arrival order.
/// Runs on every enqueue, so a session's next hook releases its earlier ones.
pub fn release_deferred(queue_dir: &Path) -> io::Result<usize> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let mut released = 0;
    let deferred = match pending(&queue_dir.join(DEFERRED)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        deferred => deferred?,
    };
    for (path, event) in deferred {
        let agent = event["agent"].as_str().and_then(Agent::parse);
        let age = now.saturating_sub(event["received_at_ms"].as_u64().unwrap_or(0).into());
        if age > MAX_DEFERRAL_MS || agent.is_none_or(|a| attributable(a, &event["payload"])) {
            fs::rename(&path, queue_dir.join(path.file_name().unwrap()))?;
            released += 1;
        }
    }
    Ok(released)
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
        .args([
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--",
            ".",
            ":(exclude)docs/doclify",
            ":(exclude).doclify",
            ":(exclude).claude/settings.local.json",
            ":(exclude).codex/hooks.json",
        ])
        .output()
        .ok()?;
    out.status.success().then_some(!out.stdout.is_empty())
}
