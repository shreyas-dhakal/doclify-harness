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
    assert!(Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(&dir)
        .status()
        .unwrap()
        .success());
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
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
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

    assert!(Command::new(BIN)
        .arg("install")
        .current_dir(&dir)
        .status()
        .unwrap()
        .success());
    let codex: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join(".codex/hooks.json")).unwrap()).unwrap();
    assert!(codex["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .contains("hook codex"));

    // Payload cwd (a subdirectory) wins over process cwd; branch is recorded.
    fs::create_dir_all(dir.join("sub")).unwrap();
    let payload = format!(
        r#"{{"hook_event_name":"UserPromptSubmit","session_id":"s1","cwd":"{}","prompt":"allow guest checkout"}}"#,
        dir.join("sub").display()
    );
    let out = run_hook(Path::new("/"), "codex", &payload, &[]);
    assert!(out.status.success() && out.stdout.is_empty());
    let events = hook::pending(&RepoInfo::discover(&dir).unwrap().queue_dir()).unwrap();
    assert_eq!(events.len(), 1);
    let e = &events[0].1;
    assert_eq!(e["agent"], "codex");
    assert_eq!(e["branch"], "main");
    assert_eq!(e["payload"]["prompt"], "allow guest checkout");

    // Doclify's own bridge sessions are never captured.
    run_hook(
        &dir,
        "claude",
        r#"{"hook_event_name":"Stop"}"#,
        &[(BRIDGE_ENV, "1")],
    );
    // Garbage input still exits 0 and is preserved rather than dropped.
    assert!(run_hook(&dir, "claude", "not json", &[]).status.success());
    assert_eq!(queued(&dir), 2);

    assert!(Command::new(BIN)
        .arg("uninstall")
        .current_dir(&dir)
        .status()
        .unwrap()
        .success());
    assert_eq!(
        fs::read_to_string(&claude)
            .unwrap()
            .split_whitespace()
            .collect::<String>(),
        r#"{"permissions":{"allow":["Bash"]},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"theirs"}]}]}}"#
    );
    assert_eq!(
        fs::read_to_string(dir.join(".codex/hooks.json"))
            .unwrap()
            .trim(),
        "{}"
    );
    fs::remove_dir_all(dir).unwrap();
}

fn add_guest_checkout_records(dir: &Path) {
    fn copy(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for e in fs::read_dir(from).unwrap().map(Result::unwrap) {
            if e.file_type().unwrap().is_dir() {
                copy(&e.path(), &to.join(e.file_name()));
            } else {
                fs::copy(e.path(), to.join(e.file_name())).unwrap();
            }
        }
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../doclify-domain/tests/fixtures/guest-checkout/.doclify");
    copy(&fixture, &dir.join(".doclify"));
}

#[test]
fn session_start_delivers_context_and_other_events_stay_silent() {
    let dir = git_repo("context");
    add_guest_checkout_records(&dir);
    assert!(Command::new(BIN)
        .arg("install")
        .current_dir(&dir)
        .status()
        .unwrap()
        .success());

    for source in ["startup", "resume", "compact"] {
        let payload = format!(
            r#"{{"hook_event_name":"SessionStart","source":"{source}","session_id":"s2","cwd":"{}"}}"#,
            dir.display()
        );
        for agent in ["claude", "codex"] {
            let out = run_hook(Path::new("/"), agent, &payload, &[]);
            assert!(out.status.success() && out.stderr.is_empty());
            let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
            let context = v["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap();
            assert!(context.contains("[requirement-guest-checkout@v2]"));
            assert!(context.contains("Rationale: Customers abandon checkout"));
            assert!(!context.contains("Bypass account"));
        }
    }
    // Capture still happened alongside context.
    assert_eq!(queued(&dir), 6);

    let out = run_hook(
        &dir,
        "claude",
        r#"{"hook_event_name":"UserPromptSubmit","prompt":"hi"}"#,
        &[],
    );
    assert!(out.stdout.is_empty());
    let out = run_hook(
        &dir,
        "claude",
        r#"{"hook_event_name":"SessionStart"}"#,
        &[(BRIDGE_ENV, "1")],
    );
    assert!(
        out.stdout.is_empty(),
        "Doclify's own bridge sessions get no context"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn session_start_fails_open_without_or_with_broken_records() {
    let dir = git_repo("context-broken");
    let start = r#"{"hook_event_name":"SessionStart","source":"startup"}"#;
    let out = run_hook(&dir, "claude", start, &[]);
    assert!(out.status.success() && out.stdout.is_empty() && out.stderr.is_empty());
    assert!(
        !dir.join(".doclify").exists(),
        "no records directory is created"
    );

    // Unreadable records directory: still exits 0 and prints nothing.
    fs::create_dir_all(dir.join(".doclify")).unwrap();
    fs::write(dir.join(".doclify/records"), "not a directory").unwrap();
    let out = run_hook(&dir, "claude", start, &[]);
    assert!(out.status.success() && out.stdout.is_empty() && out.stderr.is_empty());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn linked_worktree_inherits_registration_and_reviewed_context_without_leaking() {
    let dir = git_repo("linked");
    assert!(Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-qm",
            "initial"
        ])
        .current_dir(&dir)
        .status()
        .unwrap()
        .success());
    add_guest_checkout_records(&dir);
    assert!(Command::new(BIN)
        .arg("install")
        .current_dir(&dir)
        .status()
        .unwrap()
        .success());
    let wt = dir.with_extension("linked");
    let _ = fs::remove_dir_all(&wt);
    assert!(Command::new("git")
        .args(["worktree", "add", "-qb", "linked"])
        .arg(&wt)
        .current_dir(&dir)
        .status()
        .unwrap()
        .success());
    let payload = format!(
        r#"{{"hook_event_name":"SessionStart","cwd":"{}"}}"#,
        wt.display()
    );
    // Local records must not hide the primary's accepted decision.
    add_guest_checkout_records(&wt);
    fs::remove_dir_all(wt.join(".doclify/records/decision-guest-checkout")).unwrap();
    let out = run_hook(&wt, "claude", &payload, &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("requirement-guest-checkout@v2"), "{stdout}");
    assert!(stdout.contains("decision-guest-checkout@v2"), "{stdout}");
    let events = hook::pending(&RepoInfo::discover(&wt).unwrap().queue_dir()).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].1["worktree"], wt.to_str().unwrap());
    assert_eq!(queued(&dir), 0);
    let other = git_repo("other-linked-test");
    let out = run_hook(
        &other,
        "claude",
        r#"{"hook_event_name":"SessionStart"}"#,
        &[],
    );
    assert!(out.stdout.is_empty());
    assert_eq!(queued(&other), 0);
    fs::remove_dir_all(other).unwrap();
    fs::remove_dir_all(&dir).unwrap();
    fs::remove_dir_all(wt).unwrap();
}

#[test]
fn tool_hook_captures_dirty_worktree_state() {
    let dir = git_repo("dirty-state");
    let info = RepoInfo::discover(&dir).unwrap();
    doclify_capture::install::install(&info, "doclify-capture").unwrap();
    fs::write(dir.join("changed.rs"), "changed").unwrap();
    run_hook(
        &dir,
        "claude",
        r#"{"hook_event_name":"PostToolUse","tool_input":{"command":"cargo test"}}"#,
        &[],
    );
    let events = hook::pending(&info.queue_dir()).unwrap();
    assert_eq!(events[0].1["worktree_dirty"], true);
}

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new("git")
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap()
        .success());
}

fn capture(dir: &Path, cmd: &str) {
    assert!(Command::new(BIN)
        .arg(cmd)
        .current_dir(dir)
        .status()
        .unwrap()
        .success());
}

fn ours(path: &Path) -> usize {
    fs::read_to_string(path)
        .unwrap_or_default()
        .matches(doclify_capture::install::MARKER)
        .count()
}

#[test]
fn hooks_reach_existing_and_new_worktrees_without_touching_foreign_state() {
    use doclify_capture::{install::config_path, Agent};
    let dir = git_repo("reconcile");
    git(&dir, &["commit", "--allow-empty", "-qm", "initial"]);
    let wt = |name: &str| dir.with_extension(name);
    let add = |name: &str| {
        let _ = fs::remove_dir_all(wt(name));
        git(
            &dir,
            &["worktree", "add", "-qb", name, wt(name).to_str().unwrap()],
        );
    };
    let foreign = r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"theirs"}]}]}}"#;
    let codex = |name: &str| config_path(&wt(name), Agent::Codex);
    let claude = |name: &str| config_path(&wt(name), Agent::Claude);

    // Existing worktrees: one with a foreign Codex hook, one with a broken file,
    // one whose directory has gone (prunable).
    add("existing");
    fs::create_dir_all(codex("existing").parent().unwrap()).unwrap();
    fs::write(codex("existing"), foreign).unwrap();
    add("broken");
    fs::create_dir_all(claude("broken").parent().unwrap()).unwrap();
    fs::write(claude("broken"), "{ not json").unwrap();
    add("gone");
    fs::remove_dir_all(wt("gone")).unwrap();

    capture(&dir, "install");
    for name in ["existing", "broken"] {
        assert_eq!(ours(&codex(name)), 6, "{name}");
    }
    assert_eq!(ours(&claude("existing")), 6);
    assert!(fs::read_to_string(codex("existing"))
        .unwrap()
        .contains("theirs"));
    assert_eq!(fs::read_to_string(claude("broken")).unwrap(), "{ not json");
    assert!(!wt("gone").exists(), "missing worktrees are not recreated");

    // A worktree added later gets hooks when a session starts anywhere,
    // and that start stays silent although one worktree's file is broken.
    add("later");
    assert!(!claude("later").exists());
    let start = r#"{"hook_event_name":"SessionStart","source":"startup"}"#;
    let out = run_hook(&dir, "claude", start, &[]);
    assert!(out.status.success() && out.stdout.is_empty() && out.stderr.is_empty());
    assert_eq!((ours(&claude("later")), ours(&codex("later"))), (6, 6));
    let before = fs::read_to_string(claude("later")).unwrap();
    run_hook(&dir, "claude", start, &[]);
    assert_eq!(
        fs::read_to_string(claude("later")).unwrap(),
        before,
        "idempotent"
    );

    // Uninstall restores the foreign config and is not undone by reconciliation.
    capture(&wt("existing"), "uninstall");
    let restored = |p: PathBuf| {
        fs::read_to_string(p)
            .unwrap()
            .split_whitespace()
            .collect::<String>()
    };
    assert_eq!(restored(codex("existing")), foreign);
    run_hook(&wt("later"), "codex", start, &[]);
    assert_eq!(ours(&codex("existing")), 0);
    assert_eq!(ours(&codex("later")), 6, "other worktrees keep their hooks");
    capture(&wt("existing"), "install");
    assert_eq!(ours(&codex("existing")), 6, "install lifts the opt-out");

    git(&dir, &["worktree", "prune"]);
    for name in ["existing", "broken", "later"] {
        fs::remove_dir_all(wt(name)).unwrap();
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn unregistered_repo_worktrees_get_no_hooks() {
    let dir = git_repo("reconcile-unregistered");
    git(&dir, &["commit", "--allow-empty", "-qm", "initial"]);
    let wt = dir.with_extension("unreg-wt");
    let _ = fs::remove_dir_all(&wt);
    git(&dir, &["worktree", "add", "-qb", "x", wt.to_str().unwrap()]);
    let out = run_hook(&dir, "claude", r#"{"hook_event_name":"SessionStart"}"#, &[]);
    assert!(out.status.success() && out.stdout.is_empty());
    assert!(!wt.join(".claude").exists() && !dir.join(".claude").exists());
    fs::remove_dir_all(&wt).unwrap();
    fs::remove_dir_all(dir).unwrap();
}

/// Claude fires SessionStart/UserPromptSubmit before its transcript has a
/// `cwd` line, so ownership cannot be checked yet. Those hooks wait in
/// `deferred/` and rejoin the queue, in order, once the transcript attributes.
#[test]
fn early_unattributable_events_are_deferred_then_released_in_order() {
    let dir = git_repo("deferred");
    let info = RepoInfo::discover(&dir).unwrap();
    doclify_capture::install::install(&info, "doclify-capture").unwrap();
    let transcript = dir.with_extension("transcript.jsonl");
    let _ = fs::remove_file(&transcript);
    let hook_payload = |event: &str| {
        format!(
            r#"{{"hook_event_name":"{event}","session_id":"s","cwd":"{}","transcript_path":"{}"}}"#,
            dir.display(),
            transcript.display()
        )
    };
    // Not yet created, then only a cwd-less queue operation.
    run_hook(&dir, "claude", &hook_payload("SessionStart"), &[]);
    fs::write(&transcript, "{\"type\":\"queue-operation\"}\n").unwrap();
    run_hook(&dir, "claude", &hook_payload("UserPromptSubmit"), &[]);
    assert_eq!(queued(&dir), 0);
    let deferred = info.queue_dir().join("deferred");
    assert_eq!(fs::read_dir(&deferred).unwrap().count(), 2);

    fs::write(
        &transcript,
        format!(
            "{{\"type\":\"queue-operation\"}}\n{{\"type\":\"user\",\"sessionId\":\"s\",\"cwd\":\"{}\"}}\n",
            dir.display()
        ),
    )
    .unwrap();
    run_hook(&dir, "claude", &hook_payload("Stop"), &[]);
    let events: Vec<String> = hook::pending(&info.queue_dir())
        .unwrap()
        .into_iter()
        .map(|(_, e)| e["payload"]["hook_event_name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(events, ["SessionStart", "UserPromptSubmit", "Stop"]);
    assert_eq!(fs::read_dir(&deferred).unwrap().count(), 0);
    fs::remove_file(transcript).unwrap();
    fs::remove_dir_all(dir).unwrap();
}

/// A linked worktree with its own reviewed and unreviewed records still gets
/// every applicable primary constraint, and a primary acceptance wins over a
/// stale local proposal of the same version.
#[test]
fn linked_worktree_merges_local_and_primary_records() {
    use doclify_domain::*;
    let record = |id: &str, kind: RecordKind| Record {
        id: id.into(),
        version: 1,
        kind,
        title: id.into(),
        statement: id.into(),
        rationale: "Because.".into(),
        review: ReviewState::Proposed,
        implementation: ImplementationState::Planned,
        experiment_outcome: None,
        provenance: vec![],
        evidence: vec![],
        supersedes: None,
        scope: None,
        session: None,
    };
    let dir = git_repo("mixed");
    git(&dir, &["commit", "--allow-empty", "-qm", "initial"]);
    let info = RepoInfo::discover(&dir).unwrap();
    doclify_capture::install::install(&info, "doclify-capture").unwrap();
    let wt = dir.with_extension("mixed-linked");
    let _ = fs::remove_dir_all(&wt);
    git(
        &dir,
        &[
            "worktree",
            "add",
            "-qb",
            "mixed-linked",
            wt.to_str().unwrap(),
        ],
    );
    let (primary, local) = (Store::open(&dir).unwrap(), Store::open(&wt).unwrap());
    for (store, r, accept) in [
        (
            &primary,
            record("decision-primary", RecordKind::Decision),
            true,
        ),
        (
            &primary,
            record("decision-shared", RecordKind::Decision),
            true,
        ),
        (
            &local,
            record("decision-shared", RecordKind::Decision),
            false,
        ),
        (
            &local,
            Record {
                version: 2,
                ..record("decision-shared", RecordKind::Decision)
            },
            false,
        ),
        (&local, record("decision-local", RecordKind::Decision), true),
        (
            &local,
            record("task-local-proposal", RecordKind::Task),
            false,
        ),
    ] {
        store.propose(&r).unwrap();
        if accept {
            store.review(&r.id, 1, ReviewState::Accepted).unwrap();
        }
    }
    let out = run_hook(
        &wt,
        "claude",
        &format!(
            r#"{{"hook_event_name":"SessionStart","cwd":"{}"}}"#,
            wt.display()
        ),
        &[],
    );
    let stdout: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let text = stdout["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    let constraints = text.split("## Open work").next().unwrap();
    // Review appends a version, so accepted records are at v2.
    for id in [
        "decision-primary@v2",
        "decision-shared@v2",
        "decision-local@v2",
    ] {
        assert!(constraints.contains(id), "{text}");
    }
    assert!(!constraints.contains("task-local-proposal"), "{text}");
    assert!(
        text.contains("unreviewed task proposal; not settled: task-local-proposal"),
        "{text}"
    );
    fs::remove_dir_all(&dir).unwrap();
    fs::remove_dir_all(wt).unwrap();
}

/// Malformed hook structure fails install before anything is written; a
/// write failure part-way rolls back the files already changed. Either way
/// no registration, temp files, or created directories are left behind.
#[test]
fn install_is_validated_and_transactional() {
    let dir = git_repo("install-tx");
    let info = RepoInfo::discover(&dir).unwrap();
    let claude = dir.join(".claude/settings.local.json");
    fs::create_dir_all(claude.parent().unwrap()).unwrap();
    let original = r#"{"model":"x"}"#;
    fs::write(&claude, original).unwrap();
    let codex = dir.join(".codex/hooks.json");
    fs::create_dir_all(codex.parent().unwrap()).unwrap();
    let residue = |dir: &Path| {
        let mut names: Vec<String> = [".claude", ".codex"]
            .iter()
            .filter_map(|d| fs::read_dir(dir.join(d)).ok())
            .flatten()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    for bad in [r#"{"hooks":{"Stop":{}}}"#, r#"{"hooks":[]}"#] {
        fs::write(&codex, bad).unwrap();
        let err = doclify_capture::install::install(&info, "doclify-capture").unwrap_err();
        assert!(err.to_string().contains("hooks.json"), "{err}");
        assert_eq!(fs::read_to_string(&claude).unwrap(), original);
        assert_eq!(fs::read_to_string(&codex).unwrap(), bad);
        assert!(!info.registered());
        assert!(!info.common_dir.join("doclify").exists());
        assert_eq!(residue(&dir), ["hooks.json", "settings.local.json"]);
    }
    // The CLI reports the failure instead of printing "installed".
    let out = Command::new(BIN)
        .arg("install")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!String::from_utf8_lossy(&out.stdout).contains("installed"));

    // `.codex` is a file, so the second write fails after the first succeeded.
    fs::remove_dir_all(dir.join(".claude")).unwrap();
    fs::remove_dir_all(dir.join(".codex")).unwrap();
    fs::write(dir.join(".codex"), "").unwrap();
    assert!(doclify_capture::install::install(&info, "doclify-capture").is_err());
    assert!(!dir.join(".claude").exists(), "created config rolled back");
    assert!(!info.common_dir.join("doclify").exists());
    fs::remove_dir_all(dir).unwrap();
}

/// Deferral is only for transcripts still being started; a hook naming a file
/// that is not a transcript goes straight to the worker to be rejected.
#[test]
fn non_transcript_paths_are_not_deferred() {
    let dir = git_repo("not-deferred");
    let info = RepoInfo::discover(&dir).unwrap();
    doclify_capture::install::install(&info, "doclify-capture").unwrap();
    let secret = dir.join("secret.jsonl");
    fs::write(&secret, "secret\n").unwrap();
    run_hook(
        &dir,
        "claude",
        &format!(
            r#"{{"hook_event_name":"Stop","session_id":"s","transcript_path":"{}"}}"#,
            secret.display()
        ),
        &[],
    );
    assert_eq!(queued(&dir), 1);
    fs::remove_dir_all(dir).unwrap();
}
