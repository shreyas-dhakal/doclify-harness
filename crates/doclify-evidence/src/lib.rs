//! Read-only evidence from local Git and (optionally) GitHub, plus standard
//! documentation generation. Nothing here writes to Git or GitHub, and no
//! evidence ever changes a record's review state.
use doclify_domain::{Record, Store};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

pub mod docs;
pub mod verify;
pub use verify::{SessionLink, TestOutcome, TestRun, TestSource};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Branch {
    pub name: String,
    pub head: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    /// `None` for binary files.
    pub added: Option<u64>,
    pub removed: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    pub sha: String,
    pub parents: Vec<String>,
    pub author: String,
    pub date: String,
    pub subject: String,
    pub body: String,
    pub files: Vec<FileChange>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub author: String,
    pub state: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub submitted_at: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub author: String,
    /// Truncated to `MAX_DISCUSSION` characters.
    pub body: String,
    pub created_at: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub state: String,
    pub url: String,
    pub head: String,
    pub base: String,
    pub merged_at: Option<String>,
    pub merge_commit: Option<String>,
    #[serde(default)]
    pub merged_by: Option<String>,
    /// GitHub's aggregate, e.g. `APPROVED` or `CHANGES_REQUESTED`.
    #[serde(default)]
    pub review_decision: Option<String>,
    pub reviews: Vec<Review>,
    pub comment_count: usize,
    #[serde(default)]
    pub comments: Vec<Comment>,
}
/// GitHub is optional: absence or failure is recorded, never fatal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Github {
    Available { pull_requests: Vec<PullRequest> },
    Unavailable { reason: String },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    /// The record itself cites this evidence.
    Direct,
    /// Inferred from text; a suggestion until a reviewer links it.
    Suggested,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub record: String,
    /// `commit:<sha>` or `pr:<number>`.
    pub target: String,
    pub basis: Basis,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub head: Option<String>,
    pub branches: Vec<Branch>,
    pub commits: Vec<Commit>,
    pub github: Github,
    pub links: Vec<Link>,
    #[serde(default)]
    pub tests: Vec<TestRun>,
    #[serde(default)]
    pub sessions: Vec<SessionLink>,
}

pub struct SyncOptions<'a> {
    /// Commits read across all local branches, newest first.
    pub max_commits: usize,
    pub max_pull_requests: usize,
    /// `None` skips GitHub entirely; otherwise the `gh` executable to run.
    pub gh: Option<&'a str>,
    /// `gh` is killed after this; its output is capped at `MAX_GH_OUTPUT`.
    pub gh_timeout: Duration,
    /// Captured hook events and transcript jobs (see `verify`).
    pub captured: &'a [serde_json::Value],
}
impl Default for SyncOptions<'_> {
    fn default() -> Self {
        Self {
            max_commits: 500,
            max_pull_requests: 100,
            gh: Some("gh"),
            gh_timeout: Duration::from_secs(30),
            captured: &[],
        }
    }
}
pub const MAX_GH_OUTPUT: usize = 32 << 20;
pub const MAX_DISCUSSION: usize = 1000;

/// Reads evidence and stores the snapshot at `local_dir/evidence.json`
/// (rebuildable, machine-local: pass an ignored directory).
// ponytail: full rescan bounded by max_commits each sync; checkpoint by last sha if repos get large.
pub fn sync(repo: &Path, local_dir: &Path, options: &SyncOptions) -> io::Result<Evidence> {
    let records = load_records(repo)?;
    let head = git(repo, &["rev-parse", "--verify", "-q", "HEAD"])
        .ok()
        .map(|s| s.trim().to_owned());
    let branches = branches(repo)?;
    let commits = if branches.is_empty() {
        vec![]
    } else {
        commits(repo, options.max_commits)?
    };
    let github = match options.gh {
        Some(gh) => pull_requests(repo, gh, options.max_pull_requests, options.gh_timeout),
        None => Github::Unavailable {
            reason: "GitHub sync disabled".into(),
        },
    };
    let links = link(&records, &commits, &github);
    let mut tests = verify::test_runs(options.captured);
    verify::mark_stale(repo, head.as_deref(), &mut tests);
    let sessions = verify::sessions(options.captured, &commits);
    let evidence = Evidence {
        head,
        branches,
        commits,
        github,
        links,
        tests,
        sessions,
    };
    fs::create_dir_all(local_dir)?;
    write_atomic(
        &local_dir.join("evidence.json"),
        &serde_json::to_vec_pretty(&evidence).map_err(data)?,
    )?;
    Ok(evidence)
}

pub fn load(local_dir: &Path) -> io::Result<Option<Evidence>> {
    match fs::read(local_dir.join("evidence.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(data),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Full history of every record under `.doclify/records`, sorted by id.
pub fn load_records(repo: &Path) -> io::Result<Vec<Vec<Record>>> {
    let store = Store::open(repo)?;
    let mut ids: Vec<String> = fs::read_dir(repo.join(".doclify/records"))?
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    ids.sort();
    ids.iter()
        .map(|id| store.history(id))
        .filter(|h| !matches!(h, Ok(h) if h.is_empty()))
        .collect()
}

pub fn branches(repo: &Path) -> io::Result<Vec<Branch>> {
    Ok(git(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname:short)%1f%(objectname)",
            "refs/heads",
        ],
    )?
    .lines()
    .filter_map(|l| l.split_once('\x1f'))
    .map(|(name, head)| Branch {
        name: name.into(),
        head: head.into(),
    })
    .collect())
}

pub fn commits(repo: &Path, max: usize) -> io::Result<Vec<Commit>> {
    let out = git(
        repo,
        &[
            "log",
            "--branches",
            &format!("--max-count={max}"),
            "--no-renames",
            "--numstat",
            "--format=%x1e%H%x1f%P%x1f%an%x1f%aI%x1f%s%x1f%b%x1f",
        ],
    )?;
    Ok(out.split('\x1e').filter_map(parse_commit).collect())
}

fn parse_commit(chunk: &str) -> Option<Commit> {
    let f: Vec<&str> = chunk.splitn(7, '\x1f').collect();
    if f.len() != 7 {
        return None;
    }
    let files = f[6]
        .lines()
        .filter_map(|l| {
            let mut p = l.splitn(3, '\t');
            let (a, r, path) = (p.next()?, p.next()?, p.next()?);
            Some(FileChange {
                path: path.into(),
                added: a.parse().ok(),
                removed: r.parse().ok(),
            })
        })
        .collect();
    Some(Commit {
        sha: f[0].into(),
        parents: f[1].split_whitespace().map(Into::into).collect(),
        author: f[2].into(),
        date: f[3].into(),
        subject: f[4].into(),
        body: f[5].trim().into(),
        files,
    })
}

/// Commits touching `path`, newest first, following renames.
pub fn file_history(repo: &Path, path: &str) -> io::Result<Vec<String>> {
    Ok(git(repo, &["log", "--follow", "--format=%H", "--", path])?
        .lines()
        .map(Into::into)
        .collect())
}

/// Patch for one commit. Read on demand; never copied into tracked docs.
pub fn diff(repo: &Path, sha: &str) -> io::Result<String> {
    if sha.starts_with('-') {
        return Err(invalid("revision must not start with '-'"));
    }
    git(repo, &["show", "--format=", "--patch", sha, "--"])
}

/// One read-only `gh pr list` call, bounded by `max`, `timeout`, and `MAX_GH_OUTPUT`.
fn pull_requests(repo: &Path, gh: &str, max: usize, timeout: Duration) -> Github {
    let unavailable = |reason: String| Github::Unavailable { reason };
    let mut cmd = Command::new(gh);
    cmd.current_dir(repo)
        .env("GH_PROMPT_DISABLED", "1")
        .args([
            "pr",
            "list",
            "--state",
            "all",
            "--limit",
            &max.to_string(),
            "--json",
            "number,title,body,state,url,headRefName,baseRefName,mergedAt,mergeCommit,mergedBy,reviewDecision,reviews,comments",
        ]);
    let (ok, stdout, stderr) = match run_bounded(cmd, timeout) {
        Ok(out) => out,
        Err(e) => return unavailable(format!("{gh}: {e}")),
    };
    if !ok {
        let stderr = String::from_utf8_lossy(&stderr);
        return unavailable(
            stderr
                .lines()
                .next()
                .unwrap_or("gh failed")
                .trim()
                .to_owned(),
        );
    }
    let prs: Vec<serde_json::Value> = match serde_json::from_slice(&stdout) {
        Ok(v) => v,
        Err(e) => return unavailable(format!("unexpected gh output: {e}")),
    };
    let s = |v: &serde_json::Value, k: &str| v[k].as_str().unwrap_or_default().to_owned();
    let opt = |v: &serde_json::Value| v.as_str().filter(|m| !m.is_empty()).map(String::from);
    let clip = |v: &serde_json::Value| {
        v.as_str()
            .unwrap_or_default()
            .chars()
            .take(MAX_DISCUSSION)
            .collect::<String>()
    };
    let pull_requests = prs
        .iter()
        .map(|p| PullRequest {
            number: p["number"].as_u64().unwrap_or_default(),
            title: s(p, "title"),
            body: s(p, "body"),
            state: s(p, "state"),
            url: s(p, "url"),
            head: s(p, "headRefName"),
            base: s(p, "baseRefName"),
            merged_at: opt(&p["mergedAt"]),
            merge_commit: opt(&p["mergeCommit"]["oid"]),
            merged_by: opt(&p["mergedBy"]["login"]),
            review_decision: opt(&p["reviewDecision"]),
            reviews: p["reviews"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| Review {
                    author: s(&r["author"], "login"),
                    state: s(r, "state"),
                    body: clip(&r["body"]),
                    submitted_at: opt(&r["submittedAt"]),
                })
                .collect(),
            comment_count: p["comments"].as_array().map_or(0, Vec::len),
            comments: p["comments"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| Comment {
                    author: s(&c["author"], "login"),
                    body: clip(&c["body"]),
                    created_at: opt(&c["createdAt"]),
                })
                .collect(),
        })
        .collect();
    Github::Available { pull_requests }
}

/// Links records to commits and PRs. Only evidence the record itself cites is
/// direct; text mentions are suggestions. Links never alter review state.
pub fn link(records: &[Vec<Record>], commits: &[Commit], github: &Github) -> Vec<Link> {
    let prs: &[PullRequest] = match github {
        Github::Available { pull_requests } => pull_requests,
        Github::Unavailable { .. } => &[],
    };
    let mut links = Vec::new();
    for history in records {
        let Some(id) = history.first().map(|r| r.id.as_str()) else {
            continue;
        };
        let cited: Vec<&str> = history
            .iter()
            .flat_map(|r| &r.evidence)
            .map(String::as_str)
            .collect();
        for c in commits {
            let direct = cited.iter().any(|e| {
                e.strip_prefix("commit:")
                    .is_some_and(|p| p.len() >= 7 && c.sha.starts_with(p))
            });
            if direct {
                links.push(Link {
                    record: id.into(),
                    target: format!("commit:{}", c.sha),
                    basis: Basis::Direct,
                    reason: "cited by record".into(),
                });
            } else if mentions(&c.subject, id) || mentions(&c.body, id) {
                links.push(Link {
                    record: id.into(),
                    target: format!("commit:{}", c.sha),
                    basis: Basis::Suggested,
                    reason: "commit message mentions record id".into(),
                });
            }
        }
        for p in prs {
            let target = format!("pr:{}", p.number);
            if cited.contains(&target.as_str()) {
                links.push(Link {
                    record: id.into(),
                    target,
                    basis: Basis::Direct,
                    reason: "cited by record".into(),
                });
            } else if [&p.title, &p.body, &p.head]
                .into_iter()
                .chain(p.comments.iter().map(|c| &c.body))
                .chain(p.reviews.iter().map(|r| &r.body))
                .any(|t| mentions(t, id))
            {
                links.push(Link {
                    record: id.into(),
                    target,
                    basis: Basis::Suggested,
                    reason: "pull request mentions record id".into(),
                });
            }
        }
    }
    links
}

/// Whole-token match, so `decision-a` does not match `decision-ab`.
pub(crate) fn mentions(text: &str, id: &str) -> bool {
    let token = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    text.match_indices(id).any(|(i, _)| {
        !text[..i].chars().next_back().is_some_and(token)
            && !text[i + id.len()..].chars().next().is_some_and(token)
    })
}

/// Runs `cmd` with a deadline and capped stdout. Returns (success, stdout, stderr).
/// Readers report over channels so a grandchild holding a pipe cannot hang sync.
fn run_bounded(mut cmd: Command, timeout: Duration) -> Result<(bool, Vec<u8>, Vec<u8>), String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run: {e}"))?;
    let pipe = |r: Box<dyn Read + Send>, cap: usize| {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = r.take(cap as u64 + 1).read_to_end(&mut buf);
            let _ = tx.send(buf);
        });
        rx
    };
    let stdout = pipe(Box::new(child.stdout.take().unwrap()), MAX_GH_OUTPUT);
    let stderr = pipe(Box::new(child.stderr.take().unwrap()), 64 << 10);
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {}s", timeout.as_secs_f32()));
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    let left = deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_millis(100));
    let out = stdout
        .recv_timeout(left)
        .map_err(|_| "output did not close".to_owned())?;
    if out.len() > MAX_GH_OUTPUT {
        return Err(format!("output exceeded {MAX_GH_OUTPUT} bytes"));
    }
    Ok((
        status.success(),
        out,
        stderr
            .recv_timeout(Duration::from_millis(100))
            .unwrap_or_default(),
    ))
}

fn git(repo: &Path, args: &[&str]) -> io::Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "git {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    String::from_utf8(out.stdout).map_err(data)
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temp = PathBuf::from(path);
    temp.as_mut_os_string()
        .push(format!(".{}.tmp", std::process::id()));
    fs::write(&temp, bytes)?;
    fs::rename(&temp, path)
}
pub(crate) fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn data(e: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}
