use doclify_capture::queue::Queue;
use doclify_capture::{hook, install, Agent, RepoInfo};
use doclify_domain::{ReviewState, Store};
use doclify_model_bridge::{Bridge, Capability};
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
    if args.first().is_some_and(|a| a == "docs") {
        return docs(false);
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
                json!({"installed": paths, "notice":"Codex requires project trust and hook approval."}),
            )
        }
        ["uninstall"] => Ok(json!({"removed":install::uninstall(&repo.worktree)?})),
        ["status"] => {
            let queue = Queue::open(repo.common_dir.join("doclify/jobs"))?;
            let count = hook::pending(&repo.queue_dir())?.len();
            let store = Store::open(&repo.worktree)?;
            let mut pending = Vec::new();
            for id in ids(&repo)? {
                if let Some(r) = store.history(&id)?.last() {
                    if r.review == ReviewState::Proposed {
                        pending.push(r.clone());
                    }
                }
            }
            let evidence = doclify_evidence::load(&repo.common_dir.join("doclify"))?;
            Ok(
                json!({"registered": repo.queue_dir().is_dir(), "pending_events":count,
                "pending_extraction":queue.pending()?.len(), "pending_review":pending,
                "github":evidence.map(|e| e.github), "selected_model":load_config(&repo)?,
                "codex_hook_approval":"not verified"}),
            )
        }
        ["sync"] => {
            let queue = Queue::open(repo.common_dir.join("doclify/jobs"))?;
            let drained = queue.drain_hooks(&repo.queue_dir())?;
            let evidence = doclify_evidence::sync(
                &repo.worktree,
                &repo.common_dir.join("doclify"),
                &Default::default(),
            )?;
            let docs = doclify_evidence::docs::generate(&repo.worktree, Some(&evidence))?;
            Ok(
                json!({"reconciled_events":drained, "pending_extraction":queue.pending()?.len(),
                "commits":evidence.commits.len(), "github":evidence.github, "links":evidence.links, "docs":docs}),
            )
        }
        ["model"] | ["model", "list"] => model_list(&repo),
        ["model", "set", provider, url, key_env, model]
            if *provider == "openai" || *provider == "anthropic" =>
        {
            if model.trim().is_empty() || key_env.trim().is_empty() {
                return Err("model and credential environment variable are required".into());
            }
            let selected =
                json!({"provider":provider,"base_url":url,"api_key_env":key_env,"model":model});
            save_config(&repo, &selected)?;
            Ok(json!({"selected":selected,"authentication":"unverified"}))
        }
        ["model", "set", provider] if *provider == "claude" || *provider == "codex" => {
            let selected = json!({"provider":provider});
            save_config(&repo, &selected)?;
            Ok(json!({"selected":selected,"authentication":"unverified"}))
        }
        ["doctor"] => {
            let models = model_list(&repo)?;
            Ok(
                json!({"repository":repo.worktree,"registered":repo.queue_dir().is_dir(),
                "hooks":{"claude":install::config_path(&repo.worktree, Agent::Claude).exists(),
                         "codex":install::config_path(&repo.worktree, Agent::Codex).exists(),
                         "codex_approval":"unverified"},"models":models,
                "capture_compatibility":"transcript formats are checked during sync"}),
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
        ["handoff"] => handoff(&repo, None),
        ["handoff", topic @ ..] => handoff(&repo, Some(topic.join(" "))),
        ["review"] => {
            let store = Store::open(&repo.worktree)?;
            let mut pending = Vec::new();
            for id in ids(&repo)? {
                if let Some(r) = store.history(&id)?.last() {
                    if r.review == ReviewState::Proposed {
                        pending.push(r.clone());
                    }
                }
            }
            Ok(json!({"pending_review":pending}))
        }
        ["help"] => Ok(
            json!({"commands":["init", "uninstall", "status", "sync", "doctor", "model [list|set claude|codex|set openai|anthropic <url> <key-env> <model>]", "review [<id> <version> <accept|reject|supersede>]", "docs", "handoff [topic]", "help", "quit"]}),
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
    let mut input = stdin.lock();
    let mut stdout = io::stdout();
    println!("Doclify: /help for commands; /quit to exit");
    loop {
        print!("doclify> ");
        if stdout.flush().is_err() {
            return ExitCode::FAILURE;
        }
        let mut line = String::new();
        match input.read_line(&mut line) {
            Ok(0) => break,
            Err(_) => return ExitCode::FAILURE,
            _ => {}
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !line.starts_with('/') {
            eprintln!("Documentation questions and edits require an inference integration; use /help for available commands.");
            continue;
        }
        let parts: Vec<String> = line[1..].split_whitespace().map(str::to_owned).collect();
        if parts.is_empty() {
            continue;
        }
        if parts[0] == "quit" || parts[0] == "exit" {
            break;
        }
        if parts[0] == "docs" {
            docs(true);
            continue;
        }
        if parts.len() == 1 && parts[0] == "model" {
            match run(&parts) {
                Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
                Err(e) => {
                    eprintln!("doclify: {e}");
                    continue;
                }
            }
            println!("Choose claude or codex, or press Enter to cancel. For an API model use /model set openai|anthropic <url> <key-env> <model>.");
            print!("model> ");
            let _ = stdout.flush();
            let mut choice = String::new();
            if input.read_line(&mut choice).is_err() {
                return ExitCode::FAILURE;
            }
            let choice = choice.trim();
            if choice.is_empty() {
                continue;
            }
            match run(&["model".into(), "set".into(), choice.into()]) {
                Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
                Err(e) => eprintln!("doclify: {e}"),
            }
            continue;
        }
        match run(&parts) {
            Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
            Err(e) => eprintln!("doclify: {e}"),
        }
    }
    ExitCode::SUCCESS
}

fn config_path(repo: &RepoInfo) -> std::path::PathBuf {
    repo.common_dir.join("doclify/model.json")
}
fn load_config(repo: &RepoInfo) -> io::Result<Option<Value>> {
    match fs::read(config_path(repo)) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
fn save_config(repo: &RepoInfo, value: &Value) -> io::Result<()> {
    let path = config_path(repo);
    fs::create_dir_all(path.parent().unwrap())?;
    doclify_capture::queue::atomic_write(&path, &serde_json::to_vec_pretty(value)?)
}
fn model_list(repo: &RepoInfo) -> Result<Value, Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Runtime::new()?;
    let claude_bridge = Bridge::claude("claude");
    let codex_bridge = Bridge::codex("codex");
    let (claude, codex) = runtime
        .block_on(async { tokio::join!(claude_bridge.capability(), codex_bridge.capability()) });
    fn availability(capability: Capability) -> Value {
        match capability {
            Capability::ConstrainedAuthenticationUnverified => {
                json!({"capability":"available", "authentication":"unverified"})
            }
            Capability::Unavailable(reason) => json!({"capability":"unavailable", "reason":reason}),
        }
    }
    Ok(
        json!({"selected":load_config(repo)?,"bridges":{"claude":availability(claude),"codex":availability(codex)}}),
    )
}
fn handoff(repo: &RepoInfo, topic: Option<String>) -> Result<Value, Box<dyn std::error::Error>> {
    let scope = doclify_context::Scope {
        branch: repo.branch.clone(),
        topic,
    };
    Ok(serde_json::to_value(doclify_context::build(
        &repo.worktree,
        &scope,
        &Default::default(),
    )?)?)
}
fn docs(background: bool) -> ExitCode {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let repo =
            RepoInfo::discover(&std::env::current_dir()?).ok_or("not inside a Git repository")?;
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let result = (|| -> io::Result<()> {
                let rt = tokio::runtime::Runtime::new()?;
                rt.block_on(async {
                    let viewer = doclify_viewer::Viewer::new(repo.worktree)?;
                    let (url, task) = viewer.serve().await?;
                    let _ = tx.send(url);
                    task.await.map_err(io::Error::other)?
                })
            })();
            if let Err(e) = result {
                eprintln!("doclify viewer: {e}");
            }
        });
        let url = rx.recv().map_err(|_| "viewer failed to start")?;
        println!("{}", json!({"url":url}));
        io::stdout().flush()?;
        #[cfg(target_os = "macos")]
        let opener = "open";
        #[cfg(not(target_os = "macos"))]
        let opener = "xdg-open";
        let _ = std::process::Command::new(opener).arg(&url).spawn();
        if !background {
            let _ = worker.join();
        }
        Ok(())
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("doclify: {e}");
            ExitCode::FAILURE
        }
    }
}
