use doclify_domain::{ImplementationState, Provenance, Record, RecordKind, ReviewState, Store};
use doclify_evidence::{docs, sync, Basis, Github, SyncOptions, TestOutcome, TestSource};
use serde_json::{json, Value};
use std::{fs, path::Path, process::Command, time::{Duration, Instant}};

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=Test", "-c", "user.email=t@example.com"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A Git repository holding the domain's guest-checkout fixture records.
fn fixture_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q", "-b", "main"]);
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../doclify-domain/tests/fixtures/guest-checkout"),
        repo,
    );
    fs::write(repo.join("Cargo.toml"), "[package]\nname = \"shop\"\n").unwrap();
    fs::write(repo.join("README.md"), "# Shop\n").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "Initial shop"]);
    fs::write(repo.join("checkout.rs"), "fn guest() {}\n").unwrap();
    git(repo, &["add", "checkout.rs"]);
    git(
        repo,
        &["commit", "-q", "-m", "Start guest checkout", "-m", "Implements decision-guest-checkout partially."],
    );
    dir
}

/// A fake `gh` that records its arguments and prints canned PR JSON.
fn fake_gh(dir: &Path, json: &str) -> String {
    let path = dir.join("gh");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\necho \"$@\" > \"{}\"\ncat <<'EOF'\n{json}\nEOF\n",
            dir.join("gh-args").display()
        ),
    )
    .unwrap();
    Command::new("chmod").arg("+x").arg(&path).status().unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
fn git_evidence_without_github() {
    let dir = fixture_repo();
    let repo = dir.path();
    let local = repo.join(".git/doclify");
    let e = sync(repo, &local, &SyncOptions { gh: Some("doclify-missing-gh"), ..Default::default() }).unwrap();
    assert!(matches!(e.github, Github::Unavailable { .. }));
    assert_eq!(e.commits.len(), 2);
    assert_eq!(e.branches[0].name, "main");
    assert_eq!(e.head.as_deref(), Some(e.commits[0].sha.as_str()));
    let c = &e.commits[0];
    assert_eq!(c.subject, "Start guest checkout");
    assert_eq!(c.files[0].path, "checkout.rs");
    assert_eq!(c.files[0].added, Some(1));
    // A message mention is only a suggestion.
    assert_eq!(e.links.len(), 1);
    assert_eq!(e.links[0].record, "decision-guest-checkout");
    assert_eq!(e.links[0].basis, Basis::Suggested);
    assert_eq!(doclify_evidence::load(&local).unwrap(), Some(e.clone()));
    assert_eq!(doclify_evidence::file_history(repo, "checkout.rs").unwrap(), vec![c.sha.clone()]);
    assert!(doclify_evidence::diff(repo, &c.sha).unwrap().contains("+fn guest() {}"));
    assert!(doclify_evidence::diff(repo, "--output=x").is_err());
    // Sync never changes review state.
    let store = Store::open(repo).unwrap();
    assert_eq!(store.history("decision-guest-checkout").unwrap().len(), 3);
}

#[test]
fn github_is_read_only_and_merged_pr_does_not_accept() {
    let dir = fixture_repo();
    let repo = dir.path();
    let bin = tempfile::tempdir().unwrap();
    let gh = fake_gh(
        bin.path(),
        r#"[{"number":7,"title":"Require email for guest checkout","body":"Refs decision-guest-checkout","state":"MERGED","url":"https://github.com/o/r/pull/7","headRefName":"guest","baseRefName":"main","mergedAt":"2026-11-01T00:00:00Z","mergeCommit":{"oid":"abc"},"mergedBy":{"login":"maint"},"reviewDecision":"APPROVED","reviews":[{"author":{"login":"rev"},"state":"APPROVED","body":"lgtm","submittedAt":"2026-10-31T00:00:00Z"}],"comments":[{"author":{"login":"ann"},"body":"x","createdAt":"2026-10-30T00:00:00Z"}]},
            {"number":8,"title":"Unrelated decision-guest-checkout-v2","body":"","state":"OPEN","url":"u","headRefName":"x","baseRefName":"main","mergedAt":null,"mergeCommit":null,"reviews":[],"comments":[]}]"#,
    );
    let e = sync(repo, &repo.join(".git/doclify"), &SyncOptions { gh: Some(&gh), ..Default::default() }).unwrap();
    let args = fs::read_to_string(bin.path().join("gh-args")).unwrap();
    assert!(args.starts_with("pr list --state all"), "{args}");
    let Github::Available { pull_requests } = &e.github else { panic!("{:?}", e.github) };
    assert_eq!(pull_requests[0].reviews[0].state, "APPROVED");
    assert_eq!(pull_requests[0].merge_commit.as_deref(), Some("abc"));
    assert_eq!(pull_requests[0].comment_count, 1);
    assert_eq!(pull_requests[0].merged_by.as_deref(), Some("maint"));
    assert_eq!(pull_requests[0].review_decision.as_deref(), Some("APPROVED"));
    assert_eq!(pull_requests[0].reviews[0].body, "lgtm");
    assert_eq!(pull_requests[0].comments[0].author, "ann");
    assert_eq!(pull_requests[1].merged_by, None);
    let pr_links: Vec<_> = e.links.iter().filter(|l| l.target.starts_with("pr:")).collect();
    assert_eq!(pr_links.len(), 1, "whole-token match only");
    assert_eq!(pr_links[0].basis, Basis::Suggested);

    docs::generate(repo, Some(&e)).unwrap();
    let decisions = fs::read_to_string(repo.join("docs/doclify/decisions.mdx")).unwrap();
    assert!(decisions.contains("Suggested link: `pr:7`"));
    // v3 remains a pending proposal despite the merged PR.
    assert!(decisions.contains("**Pending change, not accepted** (`decision-guest-checkout@v3`)"));
    let page = fs::read_to_string(repo.join("docs/doclify/evidence.mdx")).unwrap();
    assert!(page.contains("#7 Require email"));
    assert!(page.contains("merged 2026-11-01T00:00:00Z by maint as `abc`; review decision: APPROVED"), "{page}");
}

#[test]
fn gh_failure_is_reported_not_fatal() {
    let dir = fixture_repo();
    let bin = tempfile::tempdir().unwrap();
    let gh = bin.path().join("gh");
    fs::write(&gh, "#!/bin/sh\necho 'gh: not logged in' >&2\nexit 4\n").unwrap();
    Command::new("chmod").arg("+x").arg(&gh).status().unwrap();
    let e = sync(dir.path(), &dir.path().join(".git/doclify"), &SyncOptions { gh: Some(gh.to_str().unwrap()), ..Default::default() }).unwrap();
    assert_eq!(e.github, Github::Unavailable { reason: "gh: not logged in".into() });
    docs::generate(dir.path(), Some(&e)).unwrap();
    assert!(fs::read_to_string(dir.path().join("docs/doclify/evidence.mdx"))
        .unwrap()
        .contains("GitHub evidence unavailable: gh: not logged in"));
}

#[test]
fn standard_docs_separate_states_and_keep_unknowns() {
    let dir = fixture_repo();
    let repo = dir.path();
    let report = docs::generate(repo, None).unwrap();
    assert_eq!(report.written.len(), docs::PAGES.len());
    let read = |p: &str| fs::read_to_string(repo.join("docs/doclify").join(format!("{p}.mdx"))).unwrap();

    let decisions = read("decisions");
    let accepted = decisions.split("## Proposed").next().unwrap();
    assert!(accepted.contains("`decision-guest-checkout@v2`"));
    assert!(accepted.contains("Review: accepted | Implementation: planned"));
    assert!(accepted.contains("Rationale: Customers abandon checkout"));
    assert!(accepted.contains("Source [`codex:guest-checkout-session`](../../.doclify/records/decision-guest-checkout/00000000000000000002.json): \"Customers abandon account creation during checkout.\""));
    assert!(read("overview").contains("Unknown: no reviewed record"));
    assert!(read("architecture").contains("`Cargo.toml`"));
    assert!(read("setup").contains("`README.md`"));
    assert!(read("evidence").contains("not been synced"));

    let open = read("open-work");
    let abandoned = open.split("## Abandoned experiments").nth(1).unwrap();
    assert!(abandoned.contains("Do not treat these as instructions"));
    assert!(abandoned.contains("experiment-account-bypass"));
    let unapproved = open.split("## Implemented, not accepted").nth(1).unwrap().split("##").next().unwrap();
    assert!(unapproved.contains("experiment-account-bypass"));
    let unfinished = open.split("## Accepted, not yet implemented").nth(1).unwrap().split("##").next().unwrap();
    assert!(unfinished.contains("decision-guest-checkout@v2"));

    // Deterministic: regenerating unchanged input rewrites nothing.
    let again = docs::generate(repo, None).unwrap();
    assert_eq!(again.unchanged.len(), docs::PAGES.len());
}

#[test]
fn human_edits_survive_regeneration() {
    let dir = fixture_repo();
    let repo = dir.path();
    docs::generate(repo, None).unwrap();
    let page = repo.join("docs/doclify/requirements.mdx");
    // Edits the same lines the next regeneration changes, so the edits overlap.
    let edited = fs::read_to_string(&page).unwrap().replace("suggestions only.\n\nNone.\n", "suggestions only.\n\nHuman note: guests need order lookup.\n");
    assert!(edited.contains("Human note"));
    fs::write(&page, &edited).unwrap();

    // Unchanged generation keeps the human page silently.
    let r = docs::generate(repo, None).unwrap();
    assert_eq!(r.kept_human, vec!["requirements"]);
    assert_eq!(fs::read_to_string(&page).unwrap(), edited);

    // A changed generation becomes a reviewable conflict, not an overwrite.
    let store = Store::open(repo).unwrap();
    store
        .propose(&Record {
            id: "requirement-order-lookup".into(),
            version: 1,
            kind: RecordKind::Requirement,
            title: "Guests can <look up> {orders}".into(),
            statement: "Guests look up orders by email.".into(),
            rationale: String::new(),
            review: ReviewState::Proposed,
            implementation: ImplementationState::Unknown,
            experiment_outcome: None,
            provenance: vec![Provenance { source: "claude:s2".into(), excerpt: None }],
            evidence: vec![],
            supersedes: None,
        })
        .unwrap();
    let r = docs::generate(repo, None).unwrap();
    assert_eq!(r.conflicts, vec!["requirements"]);
    assert_eq!(fs::read_to_string(&page).unwrap(), edited);
    let conflict = fs::read_to_string(repo.join(".doclify/conflicts/requirements.mdx")).unwrap();
    assert!(conflict.contains(r"Guests can \<look up\> \{orders\}"), "MDX-escaped");
    assert!(conflict.contains("Rationale: Unknown."));

    // After review, the human page stays and the conflict is not raised again.
    docs::resolve_conflict(repo, "requirements").unwrap();
    let r = docs::generate(repo, None).unwrap();
    assert_eq!(r.kept_human, vec!["requirements"]);
    assert!(!repo.join(".doclify/conflicts/requirements.mdx").exists());
    assert!(docs::resolve_conflict(repo, "../x").is_err());
}

#[test]
fn preexisting_human_page_is_not_overwritten() {
    let dir = fixture_repo();
    let repo = dir.path();
    fs::create_dir_all(repo.join("docs/doclify")).unwrap();
    fs::write(repo.join("docs/doclify/setup.mdx"), "# Setup\n\nRun make.\n").unwrap();
    let r = docs::generate(repo, None).unwrap();
    assert_eq!(r.conflicts, vec!["setup"]);
    assert_eq!(fs::read_to_string(repo.join("docs/doclify/setup.mdx")).unwrap(), "# Setup\n\nRun make.\n");
}

/// A capture hook envelope as `doclify-capture` writes it.
fn event(agent: &str, session: &str, head: &str, at: u64, payload: Value) -> Value {
    let mut payload = payload;
    payload["session_id"] = json!(session);
    json!({"schema": 1, "agent": agent, "received_at_ms": at, "head": head, "payload": payload})
}
fn tool(command: Value, response: Value) -> Value {
    json!({"hook_event_name": "PostToolUse", "tool_name": "Bash", "tool_input": {"command": command}, "tool_response": response})
}

#[test]
fn observed_tests_differ_from_claims_and_go_stale() {
    let dir = fixture_repo();
    let repo = dir.path();
    let shas: Vec<String> = git(repo, &["rev-list", "HEAD"]).lines().map(Into::into).collect();
    let (head, older) = (&shas[0], &shas[1]);
    let old_run = event("claude", "s1", older, 1, tool(json!("cargo   test -p shop"), json!({"stdout": "ok", "interrupted": false})));
    let captured = vec![
        old_run.clone(),
        json!({"source": "hook", "event": old_run}), // replayed job
        event("codex", "s2", head, 2, tool(json!(["bash", "-lc", "npm test"]), json!({"exit_code": 1}))),
        event("claude", "s1", head, 3, tool(json!("ls"), json!({}))),
        json!({"source": "transcript", "kind": "Assistant", "session_id": "s2", "text": "Fixed it. All tests pass now! Done.",
               "context": event("codex", "s2", head, 4, json!({"hook_event_name": "Stop"}))}),
        json!({"source": "transcript", "kind": "User", "session_id": "s2", "text": "make sure tests pass",
               "context": event("codex", "s2", head, 5, json!({}))}),
    ];
    let e = sync(repo, &repo.join(".git/doclify"), &SyncOptions { gh: None, captured: &captured, ..Default::default() }).unwrap();
    assert_eq!(e.tests.len(), 3, "{:#?}", e.tests);
    let [old, failed, claim] = &e.tests[..] else { unreachable!() };
    assert_eq!((old.source, old.outcome, old.scope.as_str()), (TestSource::Observed, TestOutcome::Passed, "cargo test -p shop"));
    assert_eq!(old.revision.as_deref(), Some(older.as_str()));
    assert!(old.stale.as_deref().unwrap().contains("1 file(s) changed"), "{:?}", old.stale);
    assert_eq!((failed.outcome, failed.stale.as_deref()), (TestOutcome::Failed, None));
    assert_eq!((claim.source, claim.scope.as_str(), claim.stale.as_deref()), (TestSource::Claimed, "All tests pass now", None));

    docs::generate(repo, Some(&e)).unwrap();
    let page = fs::read_to_string(repo.join("docs/doclify/evidence.mdx")).unwrap();
    assert!(page.contains("Agent claim, not observed: `All tests pass now` (passed; codex session `s2`)"), "{page}");
    assert!(page.contains("Observed run: `bash -lc npm test` (failed"), "{page}");
    assert!(page.contains("**Stale:** 1 file(s) changed since"), "{page}");

    // Doclify's own generated docs are not a relevant change.
    git(repo, &["add", "docs", ".doclify"]);
    git(repo, &["commit", "-q", "-m", "Docs"]);
    let e = sync(repo, &repo.join(".git/doclify"), &SyncOptions { gh: None, captured: &captured, ..Default::default() }).unwrap();
    assert_eq!(e.tests[1].stale, None);
    assert!(e.tests[1].revision.as_deref() != e.head.as_deref());
}

#[test]
fn session_commit_associations_are_suggestions_unless_cited() {
    let dir = fixture_repo();
    let repo = dir.path();
    let shas: Vec<String> = git(repo, &["rev-list", "HEAD"]).lines().map(Into::into).collect();
    let captured = vec![
        event("claude", "sess-a", &shas[1], 1, json!({"hook_event_name": "SessionStart"})),
        event("claude", "sess-a", &shas[0], 2, json!({"hook_event_name": "Stop"})),
        event("codex", "sess-b", &shas[0], 3, json!({"hook_event_name": "SessionStart"})),
    ];
    let e = sync(repo, &repo.join(".git/doclify"), &SyncOptions { gh: None, captured: &captured, ..Default::default() }).unwrap();
    assert_eq!(e.sessions.len(), 1, "{:#?}", e.sessions);
    assert_eq!((e.sessions[0].session.as_str(), e.sessions[0].basis), ("sess-a", Basis::Suggested));
    assert_eq!(e.sessions[0].commit, shas[0]);

    fs::write(repo.join("x.rs"), "").unwrap();
    git(repo, &["add", "x.rs"]);
    git(repo, &["commit", "-q", "-m", "Add x", "-m", "Session: sess-b"]);
    let e = sync(repo, &repo.join(".git/doclify"), &SyncOptions { gh: None, captured: &captured, ..Default::default() }).unwrap();
    let b: Vec<_> = e.sessions.iter().filter(|l| l.session == "sess-b").collect();
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].basis, Basis::Direct);
    docs::generate(repo, Some(&e)).unwrap();
    let page = fs::read_to_string(repo.join("docs/doclify/evidence.mdx")).unwrap();
    assert!(page.contains("Suggested link: claude session `sess-a`"), "{page}");
    assert!(page.contains("Linked: codex session `sess-b`"), "{page}");
}

#[test]
fn hung_gh_is_bounded() {
    let dir = fixture_repo();
    let bin = tempfile::tempdir().unwrap();
    let gh = bin.path().join("gh");
    fs::write(&gh, "#!/bin/sh\nsleep 5\n").unwrap();
    Command::new("chmod").arg("+x").arg(&gh).status().unwrap();
    let start = Instant::now();
    let options = SyncOptions { gh: Some(gh.to_str().unwrap()), gh_timeout: Duration::from_millis(300), ..Default::default() };
    let e = sync(dir.path(), &dir.path().join(".git/doclify"), &options).unwrap();
    assert!(start.elapsed() < Duration::from_secs(3), "{:?}", start.elapsed());
    let Github::Unavailable { reason } = &e.github else { panic!() };
    assert!(reason.contains("timed out"), "{reason}");
    assert_eq!(e.commits.len(), 2, "local evidence still syncs");
}

#[test]
fn non_overlapping_human_edits_merge_with_regeneration() {
    let dir = fixture_repo();
    let repo = dir.path();
    docs::generate(repo, None).unwrap();
    let page = repo.join("docs/doclify/requirements.mdx");
    let original = fs::read_to_string(&page).unwrap();
    let edited = original.replacen("## Accepted\n", "Human intro.\n\n## Accepted\n", 1);
    assert_ne!(edited, original);
    fs::write(&page, &edited).unwrap();
    Store::open(repo)
        .unwrap()
        .propose(&Record {
            id: "requirement-receipts".into(),
            version: 1,
            kind: RecordKind::Requirement,
            title: "Guests get receipts".into(),
            statement: "Email a receipt.".into(),
            rationale: String::new(),
            review: ReviewState::Proposed,
            implementation: ImplementationState::Unknown,
            experiment_outcome: None,
            provenance: vec![],
            evidence: vec![],
            supersedes: None,
        })
        .unwrap();
    let r = docs::generate(repo, None).unwrap();
    assert_eq!(r.merged, vec!["requirements"]);
    let merged = fs::read_to_string(&page).unwrap();
    assert!(merged.contains("Human intro.") && merged.contains("Guests get receipts"), "{merged}");
    assert!(!repo.join(".doclify/conflicts/requirements.mdx").exists());
    assert_eq!(fs::read_dir(repo.join(".doclify/conflicts")).unwrap().count(), 0, "temp files removed");
    // The merged page is stable on the next run.
    assert_eq!(docs::generate(repo, None).unwrap().kept_human, vec!["requirements"]);
}

#[test]
fn generation_waits_for_document_lock_and_keeps_intervening_edit() {
    let dir = fixture_repo();
    let repo = dir.path();
    docs::generate(repo, None).unwrap();
    let lock = fs::File::open(repo.join("docs/doclify")).unwrap();
    lock.lock().unwrap();
    let path = repo.to_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || { tx.send(docs::generate(&path, None).unwrap()).unwrap(); });
    assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "generator bypassed viewer lock");
    fs::write(repo.join("docs/doclify/overview.mdx"), "human edit\n").unwrap();
    drop(lock);
    let report = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    worker.join().unwrap();
    assert!(report.kept_human.contains(&"overview".to_string()));
    assert_eq!(fs::read_to_string(repo.join("docs/doclify/overview.mdx")).unwrap(), "human edit\n");
}

#[test]
fn provenance_links_to_portable_record_with_excerpt_in_clone() {
    let dir = fixture_repo();
    docs::generate(dir.path(), None).unwrap();
    let page = fs::read_to_string(dir.path().join("docs/doclify/decisions.mdx")).unwrap();
    let link = "../../.doclify/records/decision-guest-checkout/00000000000000000002.json";
    assert!(page.contains(&format!("]({link})")), "{page}");
    let clone = tempfile::tempdir().unwrap();
    copy_dir(dir.path(), clone.path());
    let source = fs::read_to_string(clone.path().join("docs/doclify").join(link)).unwrap();
    assert!(source.contains("Customers abandon account creation"));
}
