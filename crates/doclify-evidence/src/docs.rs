//! Standard documentation under `docs/doclify/`. Each page is regenerated
//! against the last generated copy in `.doclify/generated/`. Human edits are
//! three-way merged with the regeneration (`git merge-file`); edits that
//! overlap it are never overwritten, and the regeneration is written to
//! `.doclify/conflicts/` for review instead.
use crate::{load_records, write_atomic, Basis, Evidence, Github, TestOutcome, TestSource};
use doclify_domain::{ExperimentOutcome, ImplementationState, Record, RecordKind, ReviewState, Store};
use serde::Serialize;
use std::{fmt::Write, fs, io, path::Path};

pub const PAGES: [&str; 7] = [
    "overview",
    "architecture",
    "setup",
    "requirements",
    "decisions",
    "evidence",
    "open-work",
];
const UNKNOWN: &str = "Unknown: no reviewed record documents this yet.";

#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct Report {
    pub written: Vec<String>,
    pub unchanged: Vec<String>,
    /// Human-edited pages whose generated content did not change.
    pub kept_human: Vec<String>,
    /// Human-edited pages the regeneration merged into without overlap.
    #[serde(default)]
    pub merged: Vec<String>,
    /// Human-edited pages whose regeneration waits in `.doclify/conflicts/`.
    pub conflicts: Vec<String>,
}

/// One record id's state: the accepted version (if any) and a newer pending proposal.
struct Entry {
    accepted: Option<Record>,
    pending: Option<Record>,
    latest: Record,
}

pub fn generate(repo: &Path, evidence: Option<&Evidence>) -> io::Result<Report> {
    let _lock = lock_docs(repo)?;
    let store = Store::open(repo)?;
    let mut entries = Vec::new();
    for history in load_records(repo)? {
        let latest = history.last().cloned().expect("load_records skips empty histories");
        let accepted = store.accepted(&latest.id)?;
        let pending = (latest.review == ReviewState::Proposed).then(|| latest.clone());
        entries.push(Entry { accepted, pending, latest });
    }
    let files = git_files(repo);
    let mut report = Report::default();
    for name in PAGES {
        let body = match name {
            "overview" => overview(&entries, &files, evidence),
            "architecture" => architecture(&files),
            "setup" => setup(&files),
            "requirements" => records(&entries, RecordKind::Requirement, evidence),
            "decisions" => records(&entries, RecordKind::Decision, evidence),
            "evidence" => evidence_page(evidence),
            _ => open_work(&entries),
        };
        reconcile(repo, name, &page(name, &body), &mut report)?;
    }
    Ok(report)
}

/// Marks a conflict reviewed: the human page stays as edited and the pending
/// regeneration becomes the new baseline, so it is not proposed again.
pub fn resolve_conflict(repo: &Path, name: &str) -> io::Result<()> {
    if !PAGES.contains(&name) {
        return Err(crate::invalid("unknown documentation page"));
    }
    let _lock = lock_docs(repo)?;
    let file = format!("{name}.mdx");
    fs::rename(
        repo.join(".doclify/conflicts").join(&file),
        repo.join(".doclify/generated").join(&file),
    )
}

// Shared with the viewer: keep the directory descriptor locked for the whole
// read/merge/write transaction, including baseline and conflict updates.
fn lock_docs(repo: &Path) -> io::Result<fs::File> {
    let dir = repo.join("docs/doclify");
    fs::create_dir_all(&dir)?;
    let lock = fs::File::open(dir)?;
    lock.lock()?;
    Ok(lock)
}

fn reconcile(repo: &Path, name: &str, new: &str, report: &mut Report) -> io::Result<()> {
    let file = format!("{name}.mdx");
    let doc = repo.join("docs/doclify").join(&file);
    let base = repo.join(".doclify/generated").join(&file);
    let conflict = repo.join(".doclify/conflicts").join(&file);
    for dir in ["docs/doclify", ".doclify/generated", ".doclify/conflicts"] {
        fs::create_dir_all(repo.join(dir))?;
    }
    let (current, baseline) = (read_opt(&doc)?, read_opt(&base)?);
    let name = name.to_owned();
    if current == baseline {
        if current.as_deref() == Some(new) {
            report.unchanged.push(name);
        } else {
            write_atomic(&doc, new.as_bytes())?;
            write_atomic(&base, new.as_bytes())?;
            report.written.push(name);
        }
    } else if current.as_deref() == Some(new) {
        // The human edit already matches the regeneration.
        write_atomic(&base, new.as_bytes())?;
        report.unchanged.push(name);
    } else if baseline.as_deref() == Some(new) {
        report.kept_human.push(name);
    } else if let Some(merged) = merge(repo, current.as_deref(), baseline.as_deref(), new)? {
        write_atomic(&doc, merged.as_bytes())?;
        write_atomic(&base, new.as_bytes())?;
        report.merged.push(name);
    } else {
        write_atomic(&conflict, new.as_bytes())?;
        report.conflicts.push(name);
        return Ok(());
    }
    // Any stale conflict is obsolete once the page is consistent again.
    match fs::remove_file(conflict) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Line-based three-way merge. `None` when edits overlap or there is no
/// baseline (a page that predates Doclify is all human edit).
fn merge(repo: &Path, current: Option<&str>, baseline: Option<&str>, new: &str) -> io::Result<Option<String>> {
    let (Some(current), Some(baseline)) = (current, baseline) else { return Ok(None) };
    let dir = repo.join(".doclify/conflicts");
    let tmp = |tag: &str, text: &str| -> io::Result<std::path::PathBuf> {
        let path = dir.join(format!(".merge-{}-{tag}.tmp", std::process::id()));
        fs::write(&path, text)?;
        Ok(path)
    };
    let files = [tmp("current", current)?, tmp("base", baseline)?, tmp("new", new)?];
    let out = std::process::Command::new("git")
        .arg("merge-file")
        .arg("-p")
        .args(&files)
        .output();
    for f in &files {
        let _ = fs::remove_file(f);
    }
    let out = out?;
    match out.status.code() {
        Some(0) => String::from_utf8(out.stdout).map(Some).map_err(|e| crate::invalid(&e.to_string())),
        Some(1..=127) => Ok(None),
        _ => Err(io::Error::other(format!("git merge-file: {}", String::from_utf8_lossy(&out.stderr).trim()))),
    }
}

fn read_opt(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn page(name: &str, body: &str) -> String {
    let title = match name {
        "overview" => "Project overview",
        "architecture" => "Architecture",
        "setup" => "Setup",
        "requirements" => "Feature requirements",
        "decisions" => "Decisions and rationale",
        "evidence" => "Implementation evidence",
        _ => "Open work",
    };
    format!(
        "---\ntitle: {title}\ngenerated_by: doclify\n---\n\n# {title}\n\n\
         {{/* Generated by Doclify. Your edits are kept; later regenerations become reviewable conflicts. */}}\n\n{body}"
    )
}

fn git_files(repo: &Path) -> Vec<String> {
    crate::git(repo, &["ls-files"])
        .map(|s| s.lines().map(Into::into).collect())
        .unwrap_or_default()
}

fn overview(entries: &[Entry], files: &[String], evidence: Option<&Evidence>) -> String {
    let mut s = String::from("## Purpose\n\n");
    let _ = writeln!(s, "{UNKNOWN}\n");
    s.push_str("## Repository-observed facts\n\n");
    let mut top: Vec<&str> = files.iter().map(|f| f.split('/').next().unwrap_or(f)).collect();
    top.dedup();
    let _ = writeln!(s, "- Tracked files: {}", files.len());
    let _ = writeln!(
        s,
        "- Top-level entries: {}",
        if top.is_empty() { "none".into() } else { top.iter().map(|t| code(t)).collect::<Vec<_>>().join(", ") }
    );
    match evidence {
        Some(e) => {
            let _ = writeln!(s, "- Local branches: {}", e.branches.len());
            let _ = writeln!(s, "- Commits read: {}", e.commits.len());
        }
        None => s.push_str("- Git evidence: not synced yet.\n"),
    }
    let count = |k: RecordKind| entries.iter().filter(|e| e.latest.kind == k && e.accepted.is_some()).count();
    s.push_str("\n## Reviewed intent\n\n");
    let _ = writeln!(s, "- Accepted requirements: {}", count(RecordKind::Requirement));
    let _ = writeln!(s, "- Accepted decisions: {}", count(RecordKind::Decision));
    let _ = writeln!(s, "- Records awaiting review: {}", entries.iter().filter(|e| e.pending.is_some()).count());
    s
}

const MANIFESTS: [&str; 7] = [
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "requirements.txt",
    "Gemfile",
    "pom.xml",
];
fn manifests(files: &[String]) -> Vec<&String> {
    files
        .iter()
        .filter(|f| MANIFESTS.iter().any(|m| f.rsplit('/').next() == Some(*m)))
        .collect()
}

fn architecture(files: &[String]) -> String {
    let mut s = String::from("## Design intent\n\n");
    let _ = writeln!(s, "{UNKNOWN}\n");
    s.push_str("## Repository-observed components\n\nBuild manifests found in tracked files:\n\n");
    list(&mut s, manifests(files).iter().map(|m| code(m)));
    s
}

fn setup(files: &[String]) -> String {
    let mut s = String::from("## Setup steps\n\n");
    let _ = writeln!(s, "{UNKNOWN}\n");
    s.push_str("## Repository-observed facts\n\n");
    let readmes = files.iter().filter(|f| f.rsplit('/').next().is_some_and(|n| n.to_ascii_lowercase().starts_with("readme")));
    s.push_str("Readme files:\n\n");
    list(&mut s, readmes.map(|r| code(r)));
    s.push_str("\nBuild manifests:\n\n");
    list(&mut s, manifests(files).iter().map(|m| code(m)));
    s
}

fn list(s: &mut String, items: impl Iterator<Item = String>) {
    let before = s.len();
    for item in items {
        let _ = writeln!(s, "- {item}");
    }
    if s.len() == before {
        s.push_str("- None found.\n");
    }
}

fn records(entries: &[Entry], kind: RecordKind, evidence: Option<&Evidence>) -> String {
    let mine: Vec<&Entry> = entries.iter().filter(|e| e.latest.kind == kind).collect();
    let mut s = String::from("## Accepted\n\n");
    let accepted: Vec<&Entry> = mine.iter().copied().filter(|e| e.accepted.is_some()).collect();
    if accepted.is_empty() {
        s.push_str("None accepted.\n\n");
    }
    for e in &accepted {
        let r = e.accepted.as_ref().unwrap();
        record(&mut s, r, evidence);
        if let Some(p) = &e.pending {
            let _ = writeln!(
                s,
                "> **Pending change, not accepted** ({}): {}\n",
                cite(p),
                esc(&p.statement)
            );
        }
    }
    s.push_str("## Proposed, not accepted\n\nThese are suggestions only.\n\n");
    let proposed: Vec<&Entry> = mine.iter().copied().filter(|e| e.accepted.is_none() && e.pending.is_some()).collect();
    if proposed.is_empty() {
        s.push_str("None.\n\n");
    }
    for e in proposed {
        record(&mut s, e.pending.as_ref().unwrap(), evidence);
    }
    s
}

fn record(s: &mut String, r: &Record, evidence: Option<&Evidence>) {
    let _ = writeln!(s, "### {} ({})\n", esc(&r.title), cite(r));
    let _ = writeln!(
        s,
        "- Review: {} | Implementation: {}",
        review(r.review),
        implementation(r.implementation)
    );
    let _ = writeln!(s, "- Statement: {}", esc(&r.statement));
    let _ = writeln!(
        s,
        "- Rationale: {}",
        if r.rationale.trim().is_empty() { "Unknown.".into() } else { esc(&r.rationale) }
    );
    let source_link = format!("../../.doclify/records/{}/{:020}.json", r.id, r.version);
    for p in &r.provenance {
        match &p.excerpt {
            Some(x) => { let _ = writeln!(s, "- Source [{}]({}): \"{}\"", code(&p.source), source_link, esc(x)); }
            None => { let _ = writeln!(s, "- Source [{}]({})", code(&p.source), source_link); }
        }
    }
    for e in &r.evidence {
        let _ = writeln!(s, "- Evidence: {}", code(e));
    }
    for l in evidence.into_iter().flat_map(|e| &e.links).filter(|l| l.record == r.id) {
        let _ = writeln!(s, "- {}: {} ({})", basis(l.basis), code(&short(&l.target)), esc(&l.reason));
    }
    s.push('\n');
}

fn evidence_page(evidence: Option<&Evidence>) -> String {
    let Some(e) = evidence else {
        return "Evidence has not been synced. Run `/sync`.\n".into();
    };
    let mut s = String::from(
        "Commits and pull requests are observations. A commit or merged pull request never accepts a record.\n\n## Record links\n\n",
    );
    list(
        &mut s,
        e.links.iter().map(|l| format!("{}: {} to {} ({})", basis(l.basis), code(&l.record), code(&short(&l.target)), esc(&l.reason))),
    );
    s.push_str(
        "\n## Test evidence\n\nObserved runs were seen through tool hooks; claims are what an agent said and were not observed. \
         Each result covers only its command at its revision; uncommitted changes were not captured.\n\n",
    );
    list(
        &mut s,
        e.tests.iter().map(|t| {
            format!(
                "{}: {} ({}; {} session {}) at {}{}",
                match t.source {
                    TestSource::Observed => "Observed run",
                    TestSource::Claimed => "Agent claim, not observed",
                },
                code(&t.scope),
                match t.outcome {
                    TestOutcome::Passed => "passed",
                    TestOutcome::Failed => "failed",
                    TestOutcome::Unknown => "outcome unknown",
                },
                esc(&t.agent),
                code(&t.session),
                t.revision.as_deref().map_or("unknown revision".into(), |r| code(&r[..r.len().min(12)])),
                t.stale.as_ref().map_or(String::new(), |why| format!(" **Stale:** {}", esc(why))),
            )
        }),
    );
    s.push_str("\n## Session associations\n\nSuggested associations are inferred and unconfirmed.\n\n");
    list(
        &mut s,
        e.sessions.iter().map(|l| {
            format!(
                "{}: {} session {} to {} ({})",
                basis(l.basis),
                esc(&l.agent),
                code(&l.session),
                code(&short(&format!("commit:{}", l.commit))),
                esc(&l.reason)
            )
        }),
    );
    s.push_str("\n## Branches\n\n");
    list(&mut s, e.branches.iter().map(|b| format!("{} at {}", code(&b.name), code(&b.head[..b.head.len().min(12)]))));
    s.push_str("\n## Recent commits\n\n");
    // ponytail: fixed cap keeps the tracked page small; the full set stays in local evidence.json.
    list(&mut s, e.commits.iter().take(50).map(|c| format!("{} {} ({} files)", code(&c.sha[..c.sha.len().min(12)]), esc(&c.subject), c.files.len())));
    s.push_str("\n## Pull requests\n\n");
    match &e.github {
        Github::Unavailable { reason } => {
            let _ = writeln!(s, "GitHub evidence unavailable: {}", esc(reason));
        }
        Github::Available { pull_requests } => list(
            &mut s,
            pull_requests.iter().map(|p| {
                let reviews: Vec<String> = p.reviews.iter().map(|r| format!("{} {}", esc(&r.author), esc(&r.state))).collect();
                let merged = match (&p.merged_at, &p.merged_by, &p.merge_commit) {
                    (Some(at), by, commit) => format!(
                        ", merged {}{}{}",
                        esc(at),
                        by.as_ref().map_or(String::new(), |b| format!(" by {}", esc(b))),
                        commit.as_ref().map_or(String::new(), |c| format!(" as {}", code(&c[..c.len().min(12)]))),
                    ),
                    _ => String::new(),
                };
                format!(
                    "#{} {} ({}{}; review decision: {}; reviews: {}; comments: {}) {}",
                    p.number,
                    esc(&p.title),
                    esc(&p.state),
                    merged,
                    p.review_decision.as_deref().map_or("none".into(), esc),
                    if reviews.is_empty() { "none".into() } else { reviews.join(", ") },
                    p.comment_count,
                    esc(&p.url)
                )
            }),
        ),
    }
    s
}

fn open_work(entries: &[Entry]) -> String {
    let mut s = String::from("## Awaiting review\n\nSuggestions only; not settled constraints.\n\n");
    list(&mut s, entries.iter().filter_map(|e| e.pending.as_ref()).map(line));
    s.push_str("\n## Accepted, not yet implemented\n\n");
    list(
        &mut s,
        entries
            .iter()
            .filter_map(|e| e.accepted.as_ref())
            .filter(|r| r.implementation != ImplementationState::Implemented)
            .map(line),
    );
    s.push_str("\n## Implemented, not accepted\n\nThese exist in code without an accepted decision.\n\n");
    list(
        &mut s,
        entries
            .iter()
            .filter(|e| e.accepted.is_none() && e.latest.implementation == ImplementationState::Implemented)
            .map(|e| line(&e.latest)),
    );
    s.push_str("\n## Open questions and tasks\n\n");
    list(
        &mut s,
        entries
            .iter()
            .map(|e| e.accepted.as_ref().unwrap_or(&e.latest))
            .filter(|r| matches!(r.kind, RecordKind::Question | RecordKind::Task))
            .filter(|r| r.review != ReviewState::Rejected && r.implementation != ImplementationState::Implemented)
            .map(line),
    );
    s.push_str("\n## Active experiments\n\n");
    list(&mut s, experiments(entries, ExperimentOutcome::Active));
    s.push_str("\n## Abandoned experiments\n\nHistory only. Do not treat these as instructions.\n\n");
    list(&mut s, experiments(entries, ExperimentOutcome::Abandoned));
    s
}

fn experiments(entries: &[Entry], outcome: ExperimentOutcome) -> impl Iterator<Item = String> + '_ {
    entries
        .iter()
        .map(|e| &e.latest)
        .filter(move |r| r.kind == RecordKind::Experiment && r.experiment_outcome == Some(outcome))
        .map(line)
}

fn line(r: &Record) -> String {
    format!(
        "{} ({}; review: {}; implementation: {})",
        esc(&r.title),
        cite(r),
        review(r.review),
        implementation(r.implementation)
    )
}

fn cite(r: &Record) -> String {
    code(&format!("{}@v{}", r.id, r.version))
}
fn short(target: &str) -> String {
    match target.strip_prefix("commit:") {
        Some(sha) => format!("commit:{}", &sha[..sha.len().min(12)]),
        None => target.into(),
    }
}
fn basis(b: Basis) -> &'static str {
    match b {
        Basis::Direct => "Linked",
        Basis::Suggested => "Suggested link",
    }
}
fn review(r: ReviewState) -> &'static str {
    match r {
        ReviewState::Proposed => "proposed",
        ReviewState::Accepted => "accepted",
        ReviewState::Rejected => "rejected",
        ReviewState::Superseded => "superseded",
    }
}
fn implementation(i: ImplementationState) -> &'static str {
    match i {
        ImplementationState::Unknown => "unknown",
        ImplementationState::Planned => "planned",
        ImplementationState::Partial => "partial",
        ImplementationState::Implemented => "implemented",
        ImplementationState::Reverted => "reverted",
    }
}

/// Escapes text so MDX renders it literally (no JSX, expressions, or markup).
fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' | '\r' => out.push(' '),
            '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '<' | '>' | '#' | '|' | '!' | '~' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}
/// Inline code is literal in MDX; backticks inside are replaced.
fn code(text: &str) -> String {
    format!("`{}`", text.replace(['`', '\n', '\r'], "'"))
}
