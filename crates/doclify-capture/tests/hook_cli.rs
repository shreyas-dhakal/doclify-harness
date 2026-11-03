//! End to end through the built binary and a real git repository.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use doclify_capture::{hook, RepoInfo, BRIDGE_ENV};

const BIN: &str = env!("CARGO_BIN_EXE_doclify-capture");

fn git_repo(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("doclify-e2e-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    assert!(Command::new("git").args(["init", "-q", "-b", "main"]).current_dir(&dir).status().unwrap().success());
    dir.canonicalize().unwrap()
}

fn run_hook(dir: &Path, agent: &str, stdin: &str, env: &[(&str, &str)]) -> std::process::Output {
    let mut child = Command::new(BIN)
        .args(["hook", agent])
        .current_dir(dir)
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

fn queued(dir: &Path) -> usize {
    hook::pending(&RepoInfo::discover(dir).unwrap().queue_dir()).map_or(0, |v| v.len())
}

#[test]
fn unregistered_repo_is_not_captured_and_hook_stays_silent() {
    let dir = git_repo("unregistered");
    let out = run_hook(&dir, "claude", r#"{"hook_event_name":"Stop"}"#, &[]);
    assert!(out.status.success() && out.stdout.is_empty() && out.stderr.is_empty());
    assert_eq!(queued(&dir), 0);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn install_capture_bridge_exclusion_and_uninstall() {
    let dir = git_repo("full");
    let claude = dir.join(".claude/settings.local.json");
    fs::create_dir_all(claude.parent().unwrap()).unwrap();
    fs::write(&claude, r#"{"permissions":{"allow":["Bash"]},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"theirs"}]}]}}"#).unwrap();

    assert!(Command::new(BIN).arg("install").current_dir(&dir).status().unwrap().success());
    let codex: serde_json::Value = serde_json::from_slice(&fs::read(dir.join(".codex/hooks.json")).unwrap()).unwrap();
    assert!(codex["hooks"]["SessionStart"][0]["hooks"][0]["command"].as_str().unwrap().contains("hook codex"));

    // Payload cwd (a subdirectory) wins over process cwd; branch is recorded.
    fs::create_dir_all(dir.join("sub")).unwrap();
    let payload = format!(r#"{{"hook_event_name":"UserPromptSubmit","session_id":"s1","cwd":"{}","prompt":"allow guest checkout"}}"#, dir.join("sub").display());
    let out = run_hook(Path::new("/"), "codex", &payload, &[]);
    assert!(out.status.success() && out.stdout.is_empty());
    let events = hook::pending(&RepoInfo::discover(&dir).unwrap().queue_dir()).unwrap();
    assert_eq!(events.len(), 1);
    let e = &events[0].1;
    assert_eq!(e["agent"], "codex");
    assert_eq!(e["branch"], "main");
    assert_eq!(e["payload"]["prompt"], "allow guest checkout");

    // Doclify's own bridge sessions are never captured.
    run_hook(&dir, "claude", r#"{"hook_event_name":"Stop"}"#, &[(BRIDGE_ENV, "1")]);
    // Garbage input still exits 0 and is preserved rather than dropped.
    assert!(run_hook(&dir, "claude", "not json", &[]).status.success());
    assert_eq!(queued(&dir), 2);

    assert!(Command::new(BIN).arg("uninstall").current_dir(&dir).status().unwrap().success());
    assert_eq!(
        fs::read_to_string(&claude).unwrap().split_whitespace().collect::<String>(),
        r#"{"permissions":{"allow":["Bash"]},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"theirs"}]}]}}"#
    );
    assert_eq!(fs::read_to_string(dir.join(".codex/hooks.json")).unwrap().trim(), "{}");
    fs::remove_dir_all(dir).unwrap();
}
