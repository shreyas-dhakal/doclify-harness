//! Additive hook installation. Only entries carrying `MARKER` are ever added
//! or removed; everything else in the agent config is preserved byte-for-key.
//!
//! - Claude Code: `<worktree>/.claude/settings.local.json` (personal, not committed).
//! - Codex: `<worktree>/.codex/hooks.json`. Codex only runs project hooks for a
//!   trusted project after the user reviews them in `/hooks`; we never write
//!   trust state on the user's behalf.
//!
//! Registration is shared by linked worktrees, so `reconcile` copies the
//! installed command into worktrees that lack it, including ones created after
//! `install`. A worktree that ran `uninstall` opts out until `install` runs there.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::{Agent, RepoInfo};

pub const MARKER: &str = ": doclify-hook=1;";
/// Command the hooks invoke; resolved via PATH so the Codex file stays portable.
pub const DEFAULT_EXE: &str = "doclify-capture";
pub const EVENTS: [&str; 6] = [
    "SessionStart",
    "UserPromptSubmit",
    "Stop",
    "SubagentStop",
    "PostToolUse",
    "PostToolUseFailure",
];
const TIMEOUT_SECS: u64 = 5;
const AGENTS: [Agent; 2] = [Agent::Claude, Agent::Codex];
/// Per-worktree opt-out, under that worktree's own Git directory.
const OPT_OUT: &str = "doclify/disabled";

pub fn config_path(worktree: &Path, agent: Agent) -> PathBuf {
    match agent {
        Agent::Claude => worktree.join(".claude/settings.local.json"),
        Agent::Codex => worktree.join(".codex/hooks.json"),
    }
}

/// Fail open at the shell level too: a missing binary or a crash still drains
/// stdin and exits 0, and nothing reaches the agent's stdout/stderr.
pub fn hook_command(exe: &str, agent: Agent) -> String {
    format!(
        "{MARKER} '{}' hook {} 2>/dev/null || cat >/dev/null 2>&1; :",
        exe.replace('\'', r"'\''"),
        agent.name()
    )
}

/// Registers the repository for capture, installs both agents' hooks here, and
/// reconciles the repository's other worktrees. All-or-nothing: both configs
/// are validated before any write, and a later failure restores every file
/// and directory this call created or changed, registration included.
pub fn install(repo: &RepoInfo, exe: &str) -> io::Result<Vec<PathBuf>> {
    let plans = AGENTS
        .into_iter()
        .map(|agent| {
            plan(&config_path(&repo.worktree, agent), |root| {
                remove_ours(root);
                add_ours(root, &hook_command(exe, agent))
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    let mut paths: Vec<PathBuf> = plans.iter().map(|p| p.path.clone()).collect();
    commit(plans, |created| {
        for dir in [repo.common_dir.join("doclify/queue"), repo.queue_dir()] {
            create_dirs(&dir, created)?;
        }
        match fs::remove_file(repo.git_dir.join(OPT_OUT)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    })?;
    paths.extend(reconcile(repo));
    Ok(paths)
}

/// Adds Doclify's hooks to every worktree of a registered repository that has
/// none, using the command already installed in another worktree. Opted-out,
/// bare, and missing worktrees and unparseable files are skipped; existing
/// entries are never rewritten. Returns the files changed.
pub fn reconcile(repo: &RepoInfo) -> Vec<PathBuf> {
    if !repo.registered() {
        return Vec::new();
    }
    let worktrees: Vec<PathBuf> = repo
        .worktrees()
        .into_iter()
        .filter(|wt| git_dir(wt).is_some_and(|d| !d.join(OPT_OUT).exists()))
        .collect();
    let mut changed = Vec::new();
    for agent in AGENTS {
        let Some(command) = worktrees.iter().find_map(|wt| {
            let text = fs::read_to_string(config_path(wt, agent)).ok()?;
            let root: Map<String, Value> = serde_json::from_str(&text).ok()?;
            our_command(&root).map(str::to_owned)
        }) else {
            continue;
        };
        for wt in &worktrees {
            let path = config_path(wt, agent);
            let added = edit(&path, |root| {
                if our_command(root).is_none() {
                    add_ours(root, &command)?;
                }
                Ok(())
            });
            if added.unwrap_or(false) {
                changed.push(path);
            }
        }
    }
    changed
}

/// Removes only Doclify's entries in this worktree and opts it out of
/// `reconcile`. Captured data in the queue is left alone.
pub fn uninstall(repo: &RepoInfo) -> io::Result<Vec<PathBuf>> {
    let plans = AGENTS
        .into_iter()
        .map(|agent| config_path(&repo.worktree, agent))
        .filter(|path| path.exists())
        .map(|path| {
            plan(&path, |root| {
                remove_ours(root);
                Ok(())
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    let changed = plans
        .iter()
        .filter(|p| p.after.is_some())
        .map(|p| p.path.clone())
        .collect();
    commit(plans, |created| {
        if repo.registered() {
            let marker = repo.git_dir.join(OPT_OUT);
            create_dirs(marker.parent().unwrap(), created)?;
            fs::write(marker, "")?;
        }
        Ok(())
    })?;
    Ok(changed)
}

/// A worktree's Git directory: `.git` itself, or the `gitdir:` a linked
/// worktree's `.git` file points at. `None` for bare or missing worktrees.
fn git_dir(worktree: &Path) -> Option<PathBuf> {
    let dot_git = worktree.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let text = fs::read_to_string(&dot_git).ok()?;
    let dir = worktree.join(text.strip_prefix("gitdir:")?.trim());
    dir.is_dir().then_some(dir)
}

/// A validated config change: the file's prior text (`None` if absent) and
/// its replacement (`None` if unchanged).
struct Plan {
    path: PathBuf,
    before: Option<String>,
    after: Option<String>,
}

/// Parses and edits in memory, refusing unparseable files and unexpected
/// structure. Nothing is written.
fn plan(
    path: &Path,
    f: impl FnOnce(&mut Map<String, Value>) -> Result<(), String>,
) -> io::Result<Plan> {
    let before = match fs::read_to_string(path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let mut value: Value =
        serde_json::from_str(before.as_deref().unwrap_or("{}")).map_err(|e| invalid(path, e))?;
    let root = value
        .as_object_mut()
        .ok_or_else(|| invalid(path, "top level is not an object"))?;
    let original = root.clone();
    f(root).map_err(|e| invalid(path, e))?;
    let after = (*root != original)
        .then(|| serde_json::to_string_pretty(&value).map(|s| s + "\n"))
        .transpose()?;
    Ok(Plan {
        path: path.to_owned(),
        before,
        after,
    })
}

/// Writes every plan, then runs `then`. On any failure, restores the files
/// already written and removes directories this call created, newest first.
fn commit(
    plans: Vec<Plan>,
    then: impl FnOnce(&mut Vec<PathBuf>) -> io::Result<()>,
) -> io::Result<()> {
    let mut created = Vec::new();
    let mut written = Vec::new();
    let result = (|| {
        for p in &plans {
            let Some(after) = &p.after else { continue };
            if let Some(dir) = p.path.parent() {
                create_dirs(dir, &mut created)?;
            }
            write_atomic(&p.path, after)?;
            written.push(p);
        }
        then(&mut created)
    })();
    if result.is_err() {
        for p in written.into_iter().rev() {
            let _ = match &p.before {
                Some(text) => write_atomic(&p.path, text),
                None => fs::remove_file(&p.path),
            };
        }
        for dir in created.iter().rev() {
            let _ = fs::remove_dir(dir);
        }
    }
    result
}

/// `create_dir_all`, recording each directory it actually created.
fn create_dirs(dir: &Path, created: &mut Vec<PathBuf>) -> io::Result<()> {
    let missing: Vec<&Path> = dir.ancestors().take_while(|d| !d.exists()).collect();
    fs::create_dir_all(dir)?;
    created.extend(missing.into_iter().rev().map(Path::to_path_buf));
    Ok(())
}

fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let tmp = path.with_extension("json.doclify-tmp");
    let result = fs::write(&tmp, text).and_then(|()| fs::rename(&tmp, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Single-file `plan` + `commit`. Returns whether anything changed.
fn edit(
    path: &Path,
    f: impl FnOnce(&mut Map<String, Value>) -> Result<(), String>,
) -> io::Result<bool> {
    let plan = plan(path, f)?;
    let changed = plan.after.is_some();
    commit(vec![plan], |_| Ok(()))?;
    Ok(changed)
}

fn invalid(path: &Path, e: impl std::fmt::Display) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{}: {e}; not modified", path.display()),
    )
}

/// Fails, leaving `root` untouched, if `hooks` or any event we use is not the
/// shape both agents document, so install never reports a partial success.
fn add_ours(root: &mut Map<String, Value>, command: &str) -> Result<(), String> {
    if let Some(hooks) = root.get("hooks") {
        let hooks = hooks.as_object().ok_or("`hooks` is not an object")?;
        if let Some(event) = EVENTS
            .into_iter()
            .find(|e| hooks.get(*e).is_some_and(|v| !v.is_array()))
        {
            return Err(format!("`hooks.{event}` is not an array"));
        }
    }
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks.as_object_mut().expect("checked above");
    for event in EVENTS {
        let groups = hooks.entry(event).or_insert_with(|| json!([]));
        groups.as_array_mut().expect("checked above").push(json!({
            "hooks": [{ "type": "command", "command": command, "timeout": TIMEOUT_SECS }]
        }));
    }
    Ok(())
}

fn our_command(root: &Map<String, Value>) -> Option<&str> {
    root.get("hooks")?
        .as_object()?
        .values()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|group| group["hooks"].as_array())
        .flatten()
        .find(|h| is_ours(h))?["command"]
        .as_str()
}

fn is_ours(handler: &Value) -> bool {
    handler["command"]
        .as_str()
        .is_some_and(|c| c.starts_with(MARKER))
}

/// Drops our handlers, then any group/event/`hooks` object we emptied.
fn remove_ours(root: &mut Map<String, Value>) {
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };
    let mut emptied_any = false;
    hooks.retain(|_, groups| {
        let Some(groups) = groups.as_array_mut() else {
            return true;
        };
        let before = groups.len();
        groups.retain_mut(|group| {
            let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                return true;
            };
            let had_ours = handlers.iter().any(is_ours);
            handlers.retain(|h| !is_ours(h));
            !(had_ours && handlers.is_empty())
        });
        let emptied = before > 0 && groups.is_empty();
        emptied_any |= emptied;
        !emptied
    });
    if emptied_any && hooks.is_empty() {
        root.remove("hooks");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn existing() -> Map<String, Value> {
        serde_json::from_str(
            r#"{"model":"x","hooks":{"SessionStart":[{"matcher":"startup","hooks":[{"type":"command","command":"orca-hook"}]}],"PreCompact":[]}}"#,
        )
        .unwrap()
    }

    #[test]
    fn install_twice_then_uninstall_restores_original() {
        let original = existing();
        let mut root = original.clone();
        for _ in 0..2 {
            remove_ours(&mut root);
            add_ours(&mut root, &hook_command("doclify-capture", Agent::Claude)).unwrap();
        }
        let start = root["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(start.len(), 2, "existing group kept, ours added once");
        assert_eq!(start[0]["hooks"][0]["command"], "orca-hook");
        assert_eq!(root["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert_eq!(root["hooks"]["PostToolUse"].as_array().unwrap().len(), 1);
        assert_eq!(
            root["hooks"]["PostToolUseFailure"]
                .as_array()
                .unwrap()
                .len(),
            1
        );

        remove_ours(&mut root);
        // Pre-existing empty event arrays are user state and survive.
        assert_eq!(root, original);
    }

    #[test]
    fn uninstall_drops_hooks_key_we_created() {
        let mut root = Map::new();
        add_ours(&mut root, &hook_command("x", Agent::Codex)).unwrap();
        remove_ours(&mut root);
        assert!(root.is_empty());
    }

    #[test]
    fn mixed_group_keeps_foreign_handler() {
        let mut root: Map<String, Value> =
            serde_json::from_value(json!({"hooks":{"Stop":[{"hooks":[
                {"type":"command","command":"keep-me"},
                {"type":"command","command": hook_command("x", Agent::Claude)}
            ]}]}}))
            .unwrap();
        remove_ours(&mut root);
        assert_eq!(
            root["hooks"]["Stop"][0]["hooks"],
            json!([{"type":"command","command":"keep-me"}])
        );
    }

    #[test]
    fn command_quotes_exe_and_fails_open() {
        let c = hook_command("/opt/it's/doclify", Agent::Codex);
        assert!(c.starts_with(MARKER));
        assert!(c.contains(r"'/opt/it'\''s/doclify' hook codex"));
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(hook_command("/nonexistent/doclify", Agent::Codex))
            .output()
            .unwrap();
        assert!(out.status.success() && out.stdout.is_empty() && out.stderr.is_empty());
    }

    #[test]
    fn invalid_existing_file_is_not_overwritten() {
        let dir = std::env::temp_dir().join(format!("doclify-install-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hooks.json");
        fs::write(&path, "{ not json").unwrap();
        assert!(edit(&path, |r| add_ours(r, "x")).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json");
        fs::write(&path, r#"{"hooks":{"Stop":{"x":1}}}"#).unwrap();
        let err = edit(&path, |r| add_ours(r, "x")).unwrap_err().to_string();
        assert!(err.contains("hooks.Stop"), "{err}");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            r#"{"hooks":{"Stop":{"x":1}}}"#
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
