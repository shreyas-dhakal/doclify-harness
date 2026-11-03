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

/// Git locations for a working directory. Capture state uses `git_dir` so
/// linked worktrees do not consume each other’s events or proposal jobs.
#[derive(Debug, Clone)]
pub struct RepoInfo {
    pub worktree: PathBuf,
    pub common_dir: PathBuf,
    pub git_dir: PathBuf,
    pub branch: Option<String>,
    pub head: Option<String>,
}

impl RepoInfo {
    pub fn discover(cwd: &Path) -> Option<RepoInfo> {
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(cwd)
                .args(args)
                .output()
                .ok()
        };
        // One process for everything; falls back when HEAD is unborn.
        let full = git(&[
            "rev-parse",
            "--show-toplevel",
            "--git-common-dir",
            "--abbrev-ref",
            "HEAD",
            "HEAD",
        ])?;
        let out = if full.status.success() {
            full
        } else {
            git(&["rev-parse", "--show-toplevel", "--git-common-dir"])
                .filter(|o| o.status.success())?
        };
        let text = String::from_utf8(out.stdout).ok()?;
        let mut lines = text.lines();
        let worktree = PathBuf::from(lines.next()?);
        let common = PathBuf::from(lines.next()?);
        let common_dir = if common.is_absolute() {
            common
        } else {
            cwd.join(common)
        };
        let mut branch = lines.next().filter(|b| *b != "HEAD").map(str::to_owned);
        let head = lines.next().map(str::to_owned);
        if head.is_none() {
            // Unborn HEAD (no commits yet) still has a branch name.
            branch = git(&["symbolic-ref", "--short", "-q", "HEAD"])
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty());
        }
        let git_dir_out = git(&["rev-parse", "--absolute-git-dir"])?;
        if !git_dir_out.status.success() {
            return None;
        }
        let git_dir = PathBuf::from(String::from_utf8(git_dir_out.stdout).ok()?.trim());
        Some(RepoInfo {
            worktree,
            common_dir,
            git_dir,
            branch,
            head,
        })
    }

    /// Repository registration is shared through Git’s common directory.
    pub fn registered(&self) -> bool {
        self.common_dir.join("doclify/queue").is_dir()
            && !self.git_dir.join("doclify/disabled").exists()
    }

    /// Primary worktree, used as a fallback for reviewed repository context.
    pub fn primary_worktree(&self) -> Option<PathBuf> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.worktree)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8(out.stdout).ok()?;
        text.lines()
            .find_map(|line| line.strip_prefix("worktree ").map(PathBuf::from))
    }

    /// Capture queues remain local to each worktree for attribution and recovery.
    pub fn queue_dir(&self) -> PathBuf {
        self.git_dir.join("doclify").join("queue")
    }
}

pub mod queue;
