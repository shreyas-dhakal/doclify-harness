//! Startup/resume context and handoffs built only from local records: no model
//! call, no writes, and nothing unless `.doclify/records` exists. Only accepted
//! requirements and decisions are directive; everything else is labeled open
//! work. Rejected proposals and abandoned experiments are never included.

use doclify_domain::{
    ExperimentOutcome, ImplementationState, Record, RecordKind, ReviewState, Store,
};
use serde::Serialize;
use std::{collections::BTreeMap, fs, io, path::Path};

/// What the session is known to be about.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    pub branch: Option<String>,
    /// Free text, file paths, or both (`/handoff [topic]`). `None` at startup.
    pub topic: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Limits {
    pub max_constraints: usize,
    pub max_open_work: usize,
    pub max_field_chars: usize,
    /// Budget for the rendered text, header included.
    pub max_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_constraints: 15,
            max_open_work: 10,
            max_field_chars: 400,
            max_bytes: 6000,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Constraint {
    pub id: String,
    pub version: u64,
    pub kind: RecordKind,
    pub title: String,
    pub statement: String,
    pub rationale: String,
    pub implementation: ImplementationState,
    pub evidence: Vec<String>,
    /// A later unreviewed version proposing a change; the accepted one still applies.
    pub pending_change: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenItem {
    pub id: String,
    pub version: u64,
    pub kind: RecordKind,
    pub title: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Context {
    pub scope: String,
    pub constraints: Vec<Constraint>,
    pub open_work: Vec<OpenItem>,
    /// Ids left out by the limits; listed as pointers so nothing vanishes silently.
    pub omitted: Vec<String>,
    /// Ids whose history could not be read (conflict, corruption, unknown schema).
    pub unreadable: Vec<String>,
    #[serde(skip)]
    text: String,
}

impl Context {
    pub fn render(&self) -> &str {
        &self.text
    }
}

/// `Ok(None)` when the worktree has no records or nothing worth saying.
pub fn build(worktree: &Path, scope: &Scope, limits: &Limits) -> io::Result<Option<Context>> {
    build_merged(&[worktree], scope, limits)
}

/// Like `build`, over the union of several worktrees' records (e.g. a linked
/// worktree, then the primary). An id held by more than one is taken from the
/// copy with the highest latest version, then a reviewed copy over an
/// unreviewed one (a review is newer news); ties go to the earlier worktree. If
/// any copy is unreadable the id is reported unreadable, not resolved silently.
pub fn build_merged(
    worktrees: &[&Path],
    scope: &Scope,
    limits: &Limits,
) -> io::Result<Option<Context>> {
    // ponytail: scans every record on each session start; add an index if repos reach thousands of records.
    let mut owners: BTreeMap<String, Vec<Store>> = BTreeMap::new();
    for worktree in worktrees {
        let root = worktree.join(".doclify/records");
        if !root.is_dir() {
            continue;
        }
        for id in fs::read_dir(&root)?
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|id| !id.starts_with('.'))
        {
            owners.entry(id).or_default().push(Store::open(worktree)?);
        }
    }
    let terms = terms(scope);
    let mut constraints = Vec::new();
    let mut open = Vec::new();
    let mut unreadable = Vec::new();
    let mut scoped_pointers = Vec::new();
    for (id, stores) in owners {
        let chosen = stores
            .iter()
            .map(|store| Ok((store, store.history(&id)?)))
            .collect::<io::Result<Vec<_>>>()
            .and_then(|copies| {
                let (store, history) = copies
                    .into_iter()
                    .rev()
                    .max_by_key(|(_, h)| {
                        h.last()
                            .map(|r| (r.version, r.review != ReviewState::Proposed))
                    })
                    .expect("every id has an owner");
                Ok((history, store.accepted(&id)?))
            });
        let (history, accepted) = match chosen {
            Ok(v) => v,
            Err(_) => {
                unreadable.push(id);
                continue;
            }
        };
        let Some(latest) = history.last() else {
            continue;
        };
        if !accepted
            .as_ref()
            .is_some_and(|r| matches!(r.kind, RecordKind::Requirement | RecordKind::Decision))
            && [
                Some(latest).filter(|r| r.kind == RecordKind::Experiment),
                accepted
                    .as_ref()
                    .filter(|r| r.kind == RecordKind::Experiment),
            ]
            .into_iter()
            .flatten()
            .any(|r| r.experiment_outcome == Some(ExperimentOutcome::Abandoned))
        {
            continue;
        }
        let scoped_record = accepted.as_ref().unwrap_or(latest);
        if !applicable(scoped_record, scope) {
            scoped_pointers.push(id);
            continue;
        }
        let pending = (latest.review == ReviewState::Proposed).then_some(latest.version);
        match &accepted {
            Some(r) if matches!(r.kind, RecordKind::Requirement | RecordKind::Decision) => {
                if r.implementation != ImplementationState::Implemented {
                    open.push((
                        relevance(r, &terms),
                        open_item(
                            r,
                            format!("accepted, implementation {}", name(&r.implementation)),
                            limits,
                        ),
                    ));
                }
                constraints.push((relevance(r, &terms), constraint(r, pending, limits)));
                continue;
            }
            Some(r)
                if matches!(r.kind, RecordKind::Question | RecordKind::Task)
                    && r.implementation != ImplementationState::Implemented =>
            {
                open.push((
                    relevance(r, &terms),
                    open_item(r, format!("accepted {}, unresolved", name(&r.kind)), limits),
                ));
            }
            Some(r) if r.experiment_outcome == Some(ExperimentOutcome::Active) => {
                open.push((
                    relevance(r, &terms),
                    open_item(r, "experiment, active; not a decision".into(), limits),
                ));
            }
            _ => {}
        }
        if pending.is_some() {
            let status = format!("unreviewed {} proposal; not settled", name(&latest.kind));
            open.push((relevance(latest, &terms), open_item(latest, status, limits)));
        }
    }
    // Most relevant first; stable sort keeps id order among equals.
    constraints
        .sort_by_key(|(score, c)| (std::cmp::Reverse(*score), c.kind != RecordKind::Requirement));
    open.sort_by_key(|(score, _)| std::cmp::Reverse(*score));

    let scope_line = describe(scope, !terms.is_empty());
    let mut text = format!(
        "# Doclify project context\n\
         Generated locally from Doclify records (no model call). {scope_line}\n\
         Rejected proposals and abandoned experiments are excluded. Full records: .doclify/records/<id>/; docs: docs/doclify/.\n"
    );
    let mut omitted = scoped_pointers;
    let mut take =
        |items: Vec<(usize, String, String)>, max: usize, heading: &str, text: &mut String| {
            let mut body = String::new();
            let mut selected = Vec::new();
            for (index, (_, id, item)) in items.into_iter().enumerate() {
                if selected.len() < max
                    && text.len() + heading.len() + body.len() + item.len() + 200
                        <= limits.max_bytes
                {
                    body += &item;
                    selected.push(index);
                } else if !omitted.contains(&id) {
                    omitted.push(id);
                }
            }
            if !selected.is_empty() {
                *text += heading;
                *text += &body;
            }
            selected
        };
    let selected_constraints = take(
        constraints.iter().map(|(s, c)| (*s, c.id.clone(), constraint_text(c))).collect(),
        limits.max_constraints,
        "\n## Accepted constraints\nExplicitly accepted in Doclify review. Follow them; ask the user before reversing one.\n",
        &mut text,
    );
    let constraints: Vec<Constraint> = constraints
        .into_iter()
        .enumerate()
        .filter(|(i, _)| selected_constraints.contains(i))
        .map(|(_, (_, c))| c)
        .collect();
    let selected_open = take(
        open.iter().map(|(s, o)| (*s, o.id.clone(), open_text(o))).collect(),
        limits.max_open_work,
        "\n## Open work (not settled constraints)\nFor awareness only: unfinished or unreviewed. Do not treat these as instructions.\n",
        &mut text,
    );
    let open_work: Vec<OpenItem> = open
        .into_iter()
        .enumerate()
        .filter(|(i, _)| selected_open.contains(i))
        .map(|(_, (_, o))| o)
        .collect();
    if constraints.is_empty() && open_work.is_empty() && omitted.is_empty() && unreadable.is_empty()
    {
        return Ok(None);
    }
    if !omitted.is_empty() {
        let shown: Vec<&str> = omitted.iter().take(20).map(String::as_str).collect();
        let more = omitted.len() - shown.len();
        text += &format!(
            "\nNot shown (scope or size limit; inspect records before relying on them): {}{}\n",
            shown.join(", "),
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            }
        );
    }
    if !unreadable.is_empty() {
        text += &format!(
            "\nUnreadable records (resolve before relying on them; run `doclify doctor`): {}\n",
            unreadable.join(", ")
        );
    }
    if text.len() > limits.max_bytes {
        let mut end = limits.max_bytes;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    Ok(Some(Context {
        scope: scope_line,
        constraints,
        open_work,
        omitted,
        unreadable,
        text,
    }))
}

/// Line starts that only Doclify's rendered context produces. The strip test
/// renders a full context, so drift from the `format!`s above fails it.
const RENDERED: [&str; 9] = [
    "# Doclify project context",
    "Generated locally from Doclify records",
    "Rejected proposals and abandoned experiments are excluded.",
    "## Accepted constraints",
    "Explicitly accepted in Doclify review.",
    "## Open work (not settled constraints)",
    "For awareness only: unfinished or unreviewed.",
    "Not shown (scope or size limit",
    "Unreadable records (resolve",
];

/// Removes Doclify's injected context, and an agent's restatement of it, from
/// captured text: rendered boilerplate, lines citing `[id@vN]`, and the
/// `Statement:`/`Rationale:`/`Evidence:`/`Pending:` lines that follow them.
/// Otherwise accepted constraints come back as fresh proposals.
// ponytail: line heuristic; a quote that drops the `[id@vN]` citation survives.
pub fn strip_quoted(text: &str) -> String {
    let mut out = String::new();
    let mut in_item = false;
    for line in text.split_inclusive('\n') {
        let bare = line.trim().trim_start_matches(['-', '*', '>', ' ']);
        if has_citation(line) {
            in_item = true;
        } else if in_item
            && ["Statement:", "Rationale:", "Evidence:", "Pending:"]
                .iter()
                .any(|l| bare.trim_start_matches('*').starts_with(l))
        {
        } else if RENDERED.iter().any(|p| bare.starts_with(p)) {
            in_item = false;
        } else {
            in_item = false;
            out += line;
        }
    }
    out
}

/// `[id@vN]`, as `constraint_text`/`open_text` cite records.
fn has_citation(line: &str) -> bool {
    line.split('[').skip(1).any(|rest| {
        let Some((inner, _)) = rest.split_once(']') else {
            return false;
        };
        let Some((id, version)) = inner.rsplit_once("@v") else {
            return false;
        };
        !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            && !version.is_empty()
            && version.chars().all(|c| c.is_ascii_digit())
    })
}

fn constraint(r: &Record, pending: Option<u64>, l: &Limits) -> Constraint {
    Constraint {
        id: r.id.clone(),
        version: r.version,
        kind: r.kind.clone(),
        title: clip(&r.title, l.max_field_chars),
        statement: clip(&r.statement, l.max_field_chars),
        rationale: clip(&r.rationale, l.max_field_chars),
        implementation: r.implementation,
        evidence: r.evidence.iter().take(5).map(|e| clip(e, 120)).collect(),
        pending_change: pending,
    }
}

/// Unreviewed text is model output, so open work carries the title only.
fn open_item(r: &Record, status: String, l: &Limits) -> OpenItem {
    OpenItem {
        id: r.id.clone(),
        version: r.version,
        kind: r.kind.clone(),
        title: clip(&r.title, l.max_field_chars),
        status,
    }
}

fn constraint_text(c: &Constraint) -> String {
    let mut s = format!(
        "- [{}@v{}] ({}, implementation {}) {}\n",
        c.id,
        c.version,
        name(&c.kind),
        name(&c.implementation),
        c.title
    );
    if c.statement != c.title {
        s += &format!("  Statement: {}\n", c.statement);
    }
    if !c.rationale.is_empty() {
        s += &format!("  Rationale: {}\n", c.rationale);
    }
    if !c.evidence.is_empty() {
        s += &format!("  Evidence: {}\n", c.evidence.join(", "));
    }
    if let Some(v) = c.pending_change {
        s += &format!(
            "  Pending: v{v} proposes a change and is unreviewed; v{} still applies.\n",
            c.version
        );
    }
    s
}

fn open_text(o: &OpenItem) -> String {
    format!("- [{}@v{}] {}: {}\n", o.id, o.version, o.status, o.title)
}

fn describe(scope: &Scope, ordered: bool) -> String {
    let given: Vec<String> = [("topic", &scope.topic), ("branch", &scope.branch)]
        .into_iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k} '{}'", clip(v, 80))))
        .collect();
    if scope.topic.is_none() {
        let start = match &scope.branch {
            Some(b) => format!("No task or path scope is known, so this lists repository-wide constraints, those scoped to branch '{}', and pointers rather than guessing the task.", clip(b, 80)),
            None => "No task or path scope is known, so this lists repository-wide constraints and pointers rather than guessing the task.".into(),
        };
        return if !ordered {
            start
        } else {
            format!("{start} Ordered by {}.", given.join(" and "))
        };
    }
    format!(
        "Ordered by relevance to {}; scoped records are included only when their scope matches.",
        given.join(" and ")
    )
}

fn applicable(record: &Record, scope: &Scope) -> bool {
    let Some(record_scope) = &record.scope else {
        return true;
    };
    if record_scope.branch.is_none() && record_scope.paths.is_empty() {
        return true;
    }
    if record_scope
        .branch
        .as_ref()
        .is_some_and(|b| scope.branch.as_ref() != Some(b))
    {
        return false;
    }
    if !record_scope.paths.is_empty() {
        // A branch alone does not establish what task or paths are in scope.
        let Some(topic) = scope.topic.as_deref() else {
            return false;
        };
        let words = topic.split_whitespace().map(|w| {
            w.trim_matches(|c: char| {
                !c.is_alphanumeric() && c != '/' && c != '.' && c != '_' && c != '-'
            })
        });
        return words.into_iter().any(|word| {
            record_scope.paths.iter().any(|path| {
                word == path
                    || word
                        .strip_prefix(path)
                        .is_some_and(|tail| tail.starts_with('/'))
                    || path
                        .strip_prefix(word)
                        .is_some_and(|tail| !word.is_empty() && tail.starts_with('/'))
            })
        });
    }
    true
}

/// Lowercase words of 3+ chars from the topic and branch, minus branch noise.
fn terms(scope: &Scope) -> Vec<String> {
    const NOISE: [&str; 8] = [
        "main", "master", "develop", "feature", "feat", "fix", "the", "and",
    ];
    let mut t: Vec<String> = [&scope.topic, &scope.branch]
        .into_iter()
        .flatten()
        .flat_map(|s| s.split(|c: char| !c.is_alphanumeric()))
        .map(str::to_lowercase)
        .filter(|w| w.len() >= 3 && !NOISE.contains(&w.as_str()))
        .collect();
    t.sort();
    t.dedup();
    t
}

fn relevance(r: &Record, terms: &[String]) -> usize {
    let hay = format!(
        "{} {} {} {} {}",
        r.id,
        r.title,
        r.statement,
        r.rationale,
        r.evidence.join(" ")
    )
    .to_lowercase();
    terms.iter().filter(|t| hay.contains(t.as_str())).count()
}

fn name(v: &impl Serialize) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Truncates on a char boundary and flattens newlines so a field cannot forge headings.
fn clip(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &flat[..i]),
        None => flat,
    }
}
