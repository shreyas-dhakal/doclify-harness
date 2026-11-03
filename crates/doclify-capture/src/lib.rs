//! Capture slice of Doclify: agent hooks enqueue events, transcript adapters
//! read agent sessions incrementally, and the installer adds/removes only
//! Doclify's hook entries.

use std::path::{Path, PathBuf};
use std::process::Command;

pub mod hook;
pub mod install;
pub mod transcript;

/// Set by Doclify on every model-bridge subprocess (`codex exec`, `claude -p`).
/// Hooks seeing it exit immediately so Doclify never captures its own sessions.
pub const BRIDGE_ENV: &str = "DOCLIFY_BRIDGE";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Codex,
    Claude,
}

impl Agent {
    pub fn parse(s: &str) -> Option<Agent> {
        match s {
            "codex" => Some(Agent::Codex),
            "claude" => Some(Agent::Claude),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Agent::Codex => "codex",
            Agent::Claude => "claude",
        }
    }
}

/// Git location of a working directory. `common_dir` is shared by all
/// worktrees, so local capture state lives there and is never tracked.
#[derive(Debug, Clone)]
pub struct RepoInfo {
    pub worktree: PathBuf,
    pub common_dir: PathBuf,
    pub branch: Option<String>,
    pub head: Option<String>,
}

impl RepoInfo {
    pub fn discover(cwd: &Path) -> Option<RepoInfo> {
        let git = |args: &[&str]| {
            Command::new("git").arg("-C").arg(cwd).args(args).output().ok()
        };
        // One process for everything; falls back when HEAD is unborn.
        let full = git(&["rev-parse", "--show-toplevel", "--git-common-dir", "--abbrev-ref", "HEAD", "HEAD"])?;
        let out = if full.status.success() {
            full
        } else {
            git(&["rev-parse", "--show-toplevel", "--git-common-dir"]).filter(|o| o.status.success())?
        };
        let text = String::from_utf8(out.stdout).ok()?;
        let mut lines = text.lines();
        let worktree = PathBuf::from(lines.next()?);
        let common = PathBuf::from(lines.next()?);
        let common_dir = if common.is_absolute() { common } else { cwd.join(common) };
        let mut branch = lines.next().filter(|b| *b != "HEAD").map(str::to_owned);
        let head = lines.next().map(str::to_owned);
        if head.is_none() {
            // Unborn HEAD (no commits yet) still has a branch name.
            branch = git(&["symbolic-ref", "--short", "-q", "HEAD"])
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty());
        }
        Some(RepoInfo { worktree, common_dir, branch, head })
    }

    /// Existence of this directory is what "registered repository" means for capture.
    pub fn queue_dir(&self) -> PathBuf {
        self.common_dir.join("doclify").join("queue")
    }
}

pub mod queue;
