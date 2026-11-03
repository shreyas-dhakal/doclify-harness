use doclify_domain::{ImplementationState, Provenance, Record, RecordKind, ReviewState, Store};
use doclify_evidence::{docs, sync, Basis, Github, SyncOptions};
use std::{fs, path::Path, process::Command};

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
        r#"[{"number":7,"title":"Require email for guest checkout","body":"Refs decision-guest-checkout","state":"MERGED","url":"https://github.com/o/r/pull/7","headRefName":"guest","baseRefName":"main","mergedAt":"2026-11-01T00:00:00Z","mergeCommit":{"oid":"abc"},"reviews":[{"author":{"login":"rev"},"state":"APPROVED","body":"lgtm"}],"comments":[{"body":"x"}]},
            {"number":8,"title":"Unrelated decision-guest-checkout-v2","body":"","state":"OPEN","url":"u","headRefName":"x","baseRefName":"main","mergedAt":null,"mergeCommit":null,"reviews":[],"comments":[]}]"#,
    );
    let e = sync(repo, &repo.join(".git/doclify"), &SyncOptions { gh: Some(&gh), ..Default::default() }).unwrap();
    let args = fs::read_to_string(bin.path().join("gh-args")).unwrap();
    assert!(args.starts_with("pr list --state all"), "{args}");
    let Github::Available { pull_requests } = &e.github else { panic!("{:?}", e.github) };
    assert_eq!(pull_requests[0].reviews[0].state, "APPROVED");
    assert_eq!(pull_requests[0].merge_commit.as_deref(), Some("abc"));
    assert_eq!(pull_requests[0].comment_count, 1);
    let pr_links: Vec<_> = e.links.iter().filter(|l| l.target.starts_with("pr:")).collect();
    assert_eq!(pr_links.len(), 1, "whole-token match only");
    assert_eq!(pr_links[0].basis, Basis::Suggested);

    docs::generate(repo, Some(&e)).unwrap();
    let decisions = fs::read_to_string(repo.join("docs/doclify/decisions.mdx")).unwrap();
    assert!(decisions.contains("Suggested link: `pr:7`"));
    // v3 remains a pending proposal despite the merged PR.
    assert!(decisions.contains("**Pending change, not accepted** (`decision-guest-checkout@v3`)"));
    assert!(fs::read_to_string(repo.join("docs/doclify/evidence.mdx")).unwrap().contains("#7 Require email"));
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
    assert!(accepted.contains("Source `codex:guest-checkout-session`: \"Customers abandon account creation during checkout.\""));
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
    let edited = fs::read_to_string(&page).unwrap() + "\nHuman note: guests need order lookup.\n";
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
