use doclify_capture::{hook, install, Agent, RepoInfo};
use doclify_domain::{ExperimentOutcome, RecordKind, ReviewState, Store};
use serde_json::{json, Value};
use std::{
    fs,
    io::{self, BufRead, Write},
    process::ExitCode,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "hook") {
        if let Some(agent) = args.get(1).and_then(|a| Agent::parse(a)) {
            let _ = hook::run(agent);
        }
        return ExitCode::SUCCESS;
    }
    if args.is_empty() {
        return repl();
    }
    match run(&args) {
        Ok(v) => {
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("doclify: {e}");
            ExitCode::FAILURE
        }
    }
}
fn run(args: &[String]) -> Result<Value, Box<dyn std::error::Error>> {
    let repo =
        RepoInfo::discover(&std::env::current_dir()?).ok_or("not inside a Git repository")?;
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["init"] => {
            let paths = install::install(&repo, "doclify")?;
            Store::open(&repo.worktree)?;
            Ok(
                json!({"installed": paths, "notice":"Codex requires project trust and hook approval; capture processing is not yet implemented."}),
            )
        }
        ["uninstall"] => Ok(json!({"removed":install::uninstall(&repo.worktree)?})),
        ["status"] => {
            let queue = repo.queue_dir();
            let count = if queue.is_dir() {
                fs::read_dir(queue)?
                    .filter_map(Result::ok)
                    .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
                    .count()
            } else {
                0
            };
            let store = Store::open(&repo.worktree)?;
            let mut pending = Vec::new();
            for id in ids(&repo)? {
                if let Some(r) = store.history(&id)?.last() {
                    if r.review == ReviewState::Proposed {
                        pending.push(r.clone());
                    }
                }
            }
            Ok(
                json!({"registered":repo.queue_dir().is_dir(), "pending_events":count,"pending_review":pending,"capture_processing":"not implemented", "codex_hook_approval":"not verified"}),
            )
        }
        ["review", id, version, action] => {
            let state = match *action {
                "accept" => ReviewState::Accepted,
                "reject" => ReviewState::Rejected,
                "supersede" => ReviewState::Superseded,
                _ => return Err("review action must be accept, reject, or supersede".into()),
            };
            Ok(serde_json::to_value(Store::open(&repo.worktree)?.review(
                id,
                version.parse()?,
                state,
            )?)?)
        }
        ["handoff"] => {
            let store = Store::open(&repo.worktree)?;
            let mut accepted = Vec::new();
            let mut open_work = Vec::new();
            for id in ids(&repo)? {
                if let Some(r) = store.accepted(&id)? {
                    if r.experiment_outcome == Some(ExperimentOutcome::Abandoned) {
                        continue;
                    }
                    if matches!(r.kind, RecordKind::Requirement | RecordKind::Decision) {
                        accepted.push(r);
                    } else if matches!(r.kind, RecordKind::Question | RecordKind::Task) {
                        open_work.push(r);
                    }
                }
            }
            Ok(
                json!({"accepted_constraints":accepted,"open_work":open_work,"scope":"repository-wide; relevance is not inferred"}),
            )
        }
        ["help"] => Ok(
            json!({"commands":["init", "uninstall", "status", "review <id> <version> <accept|reject|supersede>", "handoff", "help", "quit"]}),
        ),
        _ => Err("unknown command; run doclify help".into()),
    }
}
fn ids(repo: &RepoInfo) -> io::Result<Vec<String>> {
    let path = repo.worktree.join(".doclify/records");
    if !path.exists() {
        return Ok(vec![]);
    }
    let mut ids = Vec::new();
    for e in fs::read_dir(path)? {
        let e = e?;
        if e.file_type()?.is_dir() {
            ids.push(
                e.file_name()
                    .into_string()
                    .map_err(|_| io::Error::other("invalid record directory"))?,
            );
        }
    }
    ids.sort();
    Ok(ids)
}
fn repl() -> ExitCode {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    println!("Doclify: /help for commands");
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            return ExitCode::FAILURE;
        };
        let parts: Vec<String> = line
            .split_whitespace()
            .map(|s| s.trim_start_matches('/').to_owned())
            .collect();
        if parts.is_empty() {
            continue;
        }
        if parts[0] == "quit" {
            break;
        }
        match run(&parts) {
            Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
            Err(e) => eprintln!("doclify: {e}"),
        }
        let _ = stdout.flush();
    }
    ExitCode::SUCCESS
}
