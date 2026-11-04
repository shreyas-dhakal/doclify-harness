mod recovery;
mod tui;
use doclify_capture::queue::Queue;
use doclify_capture::{hook, install, Agent, RepoInfo};
use doclify_domain::{ImplementationState, Provenance, Record, RecordKind, ReviewState, Store};
use doclify_model_bridge::{Bridge, Capability};
use serde_json::{json, Value};
use std::{
    fs,
    io::{self, BufRead, IsTerminal, Write},
    process::ExitCode,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "hook") {
        if let Some(agent) = args.get(1).and_then(|a| Agent::parse(a)) {
            if let Ok(Some(path)) = hook::run(agent) {
                if let Ok(bytes) = fs::read(path) {
                    if let Ok(event) = serde_json::from_slice::<Value>(&bytes) {
                        if let Some(worktree) = event["worktree"].as_str() {
                            if let Ok(exe) = std::env::current_exe() {
                                let _ = std::process::Command::new(exe)
                                    .arg("background-worker")
                                    .current_dir(worktree)
                                    .stdin(std::process::Stdio::null())
                                    .stdout(std::process::Stdio::null())
                                    .stderr(std::process::Stdio::null())
                                    .spawn();
                            }
                        }
                    }
                }
            }
        }
        return ExitCode::SUCCESS;
    }
    if args.first().is_some_and(|a| a == "background-worker") {
        if !std::env::current_dir()
            .ok()
            .and_then(|cwd| RepoInfo::discover(&cwd))
            .is_some_and(|repo| repo.registered())
        {
            return ExitCode::SUCCESS;
        }
        // Contending hook workers retry so the last event is not stranded.
        for _ in 0..60 {
            match run(&["sync".into()]) {
                Ok(_) => {
                    if let Some(repo) = std::env::current_dir()
                        .ok()
                        .and_then(|cwd| RepoInfo::discover(&cwd))
                    {
                        let _ = fs::remove_file(repo.git_dir.join("doclify/worker-error"));
                    }
                    return ExitCode::SUCCESS;
                }
                Err(e) if e.to_string().contains("another sync is already running") => {
                    std::thread::sleep(std::time::Duration::from_secs(1))
                }
                Err(e) => {
                    if let Some(repo) = std::env::current_dir()
                        .ok()
                        .and_then(|cwd| RepoInfo::discover(&cwd))
                    {
                        let _ = fs::write(repo.git_dir.join("doclify/worker-error"), e.to_string());
                    }
                    return ExitCode::FAILURE;
                }
            }
        }
        return ExitCode::FAILURE;
    }
    if args.is_empty() {
        return if io::stdin().is_terminal() && io::stdout().is_terminal() {
            tui::run()
        } else {
            repl()
        };
    }
    if args.as_slice() == ["docs"] || args.as_slice() == ["docs", "--json"] {
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
        ["init"] => initialize(&repo, None),
        ["init", "--model", provider] if *provider == "claude" || *provider == "codex" => {
            initialize(&repo, Some(json!({"provider":provider})))
        }
        ["init", "--model", provider, url, key_env, model]
            if *provider == "openai" || *provider == "anthropic" =>
        {
            let selected = api_model_configuration(provider, url, key_env, model)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            initialize(&repo, Some(selected))
        }
        ["uninstall"] => {
            let removed = install::uninstall(&repo)?;
            recovery::uninstall(&repo)?;
            Ok(json!({"removed":removed}))
        }

        ["status"] => {
            let queue = Queue::open(repo.git_dir.join("doclify/jobs"))?;
            let count = if repo.queue_dir().is_dir() {
                hook::pending(&repo.queue_dir())?.len()
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
            let unsupported = queue.unsupported()?;
            let evidence = doclify_evidence::load(&repo.git_dir.join("doclify"))?;
            Ok(
                json!({"registered": repo.registered(), "pending_events":count, "rejected_events":rejected_count(&repo.queue_dir())?,
                "pending_extraction":queue.pending()?.len(), "rejected_details":rejected_events(&repo.queue_dir())?, "pending_review":pending,
                "unsupported_transcript_count":unsupported.len(), "unsupported_transcripts":unsupported,
                "github":evidence.map(|e| e.github), "selected_model":load_config(&repo)?, "background_error":fs::read_to_string(repo.git_dir.join("doclify/worker-error")).ok(),
                "recovery_error":fs::read_to_string(repo.git_dir.join("doclify/recovery-error")).ok(),
                "codex_hook_approval":{"status":"not_observable","required":"trust the repository and approve project hooks in /hooks"}}),
            )
        }
        ["rejected"] => Ok(json!({"events": rejected_events(&repo.queue_dir())?})),
        ["rejected", "replay", id] => {
            let dir = repo.queue_dir().join("rejected");
            if id.is_empty()
                || std::path::Path::new(id)
                    .file_name()
                    .and_then(|n| n.to_str())
                    != Some(id)
                || id.starts_with('.')
            {
                return Err("invalid rejected event id".into());
            }
            let path = dir.join(id);
            let value: Value = serde_json::from_slice(&fs::read(&path)?)?;
            let bytes = if let Some(raw) = value.get("event_bytes") {
                serde_json::from_value::<Vec<u8>>(raw.clone())?
            } else {
                serde_json::to_vec(value.get("event").unwrap_or(&value))?
            };
            let destination = repo.queue_dir().join(id);
            if destination.exists() {
                return Err("pending event already exists".into());
            }
            doclify_capture::queue::atomic_write(&destination, &bytes)?;
            fs::remove_file(path)?;
            Ok(json!({"replayed":id}))
        }
        ["unsupported"] => {
            let queue = Queue::open(repo.git_dir.join("doclify/jobs"))?;
            let transcripts = queue.unsupported()?;
            let count = transcripts.len();
            Ok(json!({"transcripts":transcripts,"count":count}))
        }
        ["unsupported", "replay"] => {
            let queue = Queue::open(repo.git_dir.join("doclify/jobs"))?;
            let roots = session_roots();
            let roots: Vec<_> = roots.iter().map(|(_, root)| root.as_path()).collect();
            let replayed = queue.replay_unsupported(&roots)?;
            let remaining = queue.unsupported()?;
            let remaining_count = remaining.len();
            Ok(json!({"replayed":replayed,"remaining":remaining,"remaining_count":remaining_count}))
        }
        ["sync"] => {
            if repo.registered() {
                fs::create_dir_all(repo.queue_dir())?;
            }
            let local = repo.git_dir.join("doclify");
            fs::create_dir_all(&local)?;
            let queue = Queue::open(local.join("jobs"))?;
            let _lock = queue.lock()?;
            let roots = session_roots();
            let sessions: Vec<_> = roots
                .iter()
                .map(|(agent, root)| (agent.as_str(), root.as_path()))
                .collect();
            let deferred_released = hook::release_deferred(&repo.queue_dir())?;
            let reconciliation = doclify_pipeline::reconcile_repository_stats_locked(
                &queue,
                &repo.queue_dir(),
                &repo.worktree,
                &sessions,
            )?;
            let drained = reconciliation.drained_events;
            let captured = captured_jobs(&local.join("jobs"))?;
            let captured_sessions = captured_session_count(&captured);
            let _ = doclify_capture::metrics::set_gauge(
                &repo,
                "last_sync_discovered_sessions",
                reconciliation.discovered_sessions as u64,
            );
            let _ = doclify_capture::metrics::set_gauge(
                &repo,
                "last_sync_captured_sessions",
                captured_sessions as u64,
            );
            let evidence = doclify_evidence::sync(
                &repo.worktree,
                &local,
                &doclify_evidence::SyncOptions {
                    captured: &captured,
                    ..Default::default()
                },
            )?;
            let processed = match selected_extractor(&repo)? {
                Some(mut extractor) => Some(doclify_pipeline::process_proposals_locked(
                    &queue,
                    &local.join("proposal-output"),
                    &Store::open(&repo.worktree)?,
                    &mut extractor,
                )?),
                None => None,
            };
            // Product measurements are opt-in and must never make capture or
            // documentation generation fail when the local metrics file is unavailable.
            let _ = doclify_capture::metrics::increment(&repo, "captured_events", drained as u64);
            let _ = doclify_capture::metrics::increment(
                &repo,
                "extracted_jobs",
                processed.unwrap_or_default() as u64,
            );
            let docs = doclify_evidence::docs::generate(&repo.worktree, Some(&evidence))?;
            Ok(
                json!({"reconciled_events":drained, "rejected_events":rejected_count(&repo.queue_dir())?, "processed":processed, "pending_extraction":queue.pending()?.len(),
                "deferred_released":deferred_released, "unsupported_transcript_count":queue.unsupported()?.len(),
                "commits":evidence.commits.len(), "github":evidence.github, "links":evidence.links, "docs":docs}),
            )
        }
        ["model"] | ["model", "list"] => model_list(&repo),
        ["metrics"] | ["metrics", "report"] => Ok(doclify_capture::metrics::report(&repo)?),
        ["metrics", "enable"] => Ok(doclify_capture::metrics::set_enabled(&repo, true)?),
        ["metrics", "disable"] => Ok(doclify_capture::metrics::set_enabled(&repo, false)?),
        ["metrics", "repeated-explanation"] => {
            doclify_capture::metrics::increment(&repo, "repeated_explanations", 1)?;
            Ok(doclify_capture::metrics::report(&repo)?)
        }
        ["model", "set", provider, url, key_env, model]
            if *provider == "openai" || *provider == "anthropic" =>
        {
            let selected = api_model_configuration(provider, url, key_env, model)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
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
                json!({"repository":repo.worktree,"registered":repo.registered(),
                "hooks":{"claude":hook_status(&repo.worktree, Agent::Claude),
                         "codex":hook_status(&repo.worktree, Agent::Codex),
                         "codex_approval":{"status":"not_observable","required":"trust the repository and approve project hooks in /hooks"}},"models":models,
                "versions":{"claude":agent_version("claude"),"codex":agent_version("codex")}, "capture_compatibility":"unverified; transcript formats are checked during sync"}),
            )
        }
        ["review", id, version, action] => {
            let state = match *action {
                "accept" => ReviewState::Accepted,
                "reject" => ReviewState::Rejected,
                "supersede" => ReviewState::Superseded,
                _ => return Err("review action must be accept, reject, or supersede".into()),
            };
            let reviewed = Store::open(&repo.worktree)?.review(id, version.parse()?, state)?;
            if matches!(state, ReviewState::Rejected | ReviewState::Superseded) {
                let _ = doclify_capture::metrics::increment(&repo, "review_corrections", 1);
            }
            Ok(serde_json::to_value(reviewed)?)
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
            json!({"commands":["init [--model claude|codex|openai|anthropic <url> <key-env> <model>]", "uninstall", "status", "sync", "doctor", "rejected [replay <id>]", "unsupported [replay]", "metrics [enable|disable|report|repeated-explanation]", "model [list|set claude|codex|set openai|anthropic <url> <key-env> <model>]", "review [<id> <version> <accept|reject|supersede>]", "docs", "handoff [topic]", "help", "quit"]}),
        ),
        _ => Err("unknown command; run doclify help".into()),
    }
}

fn initialize(
    repo: &RepoInfo,
    selected_model: Option<Value>,
) -> Result<Value, Box<dyn std::error::Error>> {
    let paths = install::install(repo, "doclify")?;
    Store::open(&repo.worktree)?;
    if let Some(model) = selected_model {
        save_config(repo, &model)?;
    }
    let recovery = match recovery::install(repo) {
        Ok(value) => {
            let _ = fs::remove_file(repo.git_dir.join("doclify/recovery-error"));
            value
        }
        Err(e) => {
            let message = e.to_string();
            fs::write(repo.git_dir.join("doclify/recovery-error"), &message)?;
            json!({"active":false,"error":message})
        }
    };
    let selected_model = load_config(repo)?;
    let model_setup_required = selected_model.is_none();
    Ok(json!({
        "installed": paths,
        "recovery": recovery,
        "selected_model": selected_model,
        "model_setup_required": model_setup_required,
        "notice": "Codex requires project trust and hook approval."
    }))
}

fn api_model_configuration(
    provider: &str,
    url: &str,
    key_env: &str,
    model: &str,
) -> Result<Value, String> {
    if model.trim().is_empty() || key_env.trim().is_empty() {
        return Err("model and credential environment variable are required".into());
    }
    let parsed = url::Url::parse(url).map_err(|_| "invalid provider URL")?;
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !(parsed.scheme() == "https"
            || (parsed.scheme() == "http"
                && matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "::1"))))
    {
        return Err(
            "provider URL must use HTTPS or loopback HTTP and contain no credentials, query, or fragment"
                .into(),
        );
    }
    Ok(json!({"provider":provider,"base_url":url,"api_key_env":key_env,"model":model}))
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
    let (stop, shutdown) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        while matches!(
            shutdown.recv_timeout(std::time::Duration::from_secs(60)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ) {
            if let Some(repo) = std::env::current_dir()
                .ok()
                .and_then(|cwd| RepoInfo::discover(&cwd))
            {
                if !repo.queue_dir().is_dir() {
                    continue;
                }
                let error_path = repo.git_dir.join("doclify/worker-error");
                match run(&["sync".into()]) {
                    Ok(_) => {
                        let _ = fs::remove_file(error_path);
                    }
                    Err(e) => {
                        let _ = fs::write(error_path, e.to_string());
                    }
                }
            }
        }
    });
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
            match documentation_request(line) {
                Ok(value) => println!("{}", serde_json::to_string_pretty(&value).unwrap()),
                Err(e) => eprintln!("doclify: {e}"),
            }
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
            let catalog = match run(&parts) {
                Ok(v) => {
                    println!("{}", serde_json::to_string_pretty(&v).unwrap());
                    v
                }
                Err(e) => {
                    eprintln!("doclify: {e}");
                    continue;
                }
            };
            println!("Search models by name; enter a number or exact name to select, or blank to cancel.");
            let repo = match std::env::current_dir()
                .ok()
                .and_then(|cwd| RepoInfo::discover(&cwd))
            {
                Some(repo) => repo,
                None => {
                    eprintln!("doclify: not inside a Git repository");
                    continue;
                }
            };
            let config = match load_config(&repo) {
                Ok(config) => config,
                Err(e) => {
                    eprintln!("doclify: {e}");
                    continue;
                }
            };
            let api_models = config
                .as_ref()
                .filter(|c| matches!(c["provider"].as_str(), Some("openai" | "anthropic")))
                .map(|c| {
                    let discovery = doclify::repl::discover_models(c).map_err(|e| {
                        eprintln!("doclify: model discovery failed: {e}");
                        e
                    });
                    doclify::repl::picker_models(Some(c), discovery)
                })
                .unwrap_or_default();
            let mut choices = vec!["claude".to_owned(), "codex".to_owned()];
            choices.extend(api_models.iter().map(|m| m.id.clone()));
            let mut shown = choices.clone();
            loop {
                for (i, choice) in shown.iter().enumerate() {
                    let status = if choice == "claude" || choice == "codex" {
                        let capability = &catalog["bridges"][choice]["capability"];
                        if capability == "unavailable" {
                            format!(
                                "unavailable: {}",
                                catalog["bridges"][choice]["reason"]
                                    .as_str()
                                    .unwrap_or("capability check failed")
                            )
                        } else {
                            "available; authentication unverified".to_owned()
                        }
                    } else {
                        match api_models
                            .iter()
                            .find(|model| model.id == *choice)
                            .map(|m| m.status)
                        {
                            Some(doclify::repl::ModelStatus::Discovered) => {
                                "discovered; authentication unverified".to_owned()
                            }
                            Some(doclify::repl::ModelStatus::ConfiguredOnly) => {
                                "not returned by discovery; availability unverified".to_owned()
                            }
                            Some(doclify::repl::ModelStatus::DiscoveryUnavailable) => {
                                "discovery failed; configured availability unknown".to_owned()
                            }
                            None => "availability unverified".to_owned(),
                        }
                    };
                    println!("{}: {} ({})", i + 1, choice, status);
                }
                print!("model search> ");
                let _ = stdout.flush();
                let mut query = String::new();
                if input.read_line(&mut query).is_err() {
                    return ExitCode::FAILURE;
                }
                let query = query.trim();
                if query.is_empty() {
                    break;
                }
                let selected = query
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| i.checked_sub(1))
                    .and_then(|i| shown.get(i).cloned())
                    .or_else(|| {
                        choices
                            .iter()
                            .find(|name| name.eq_ignore_ascii_case(query))
                            .cloned()
                    });
                if let Some(choice) = selected {
                    let result = if choice == "claude" || choice == "codex" {
                        run(&["model".into(), "set".into(), choice])
                    } else if let Some(mut selected) = config.clone() {
                        selected["model"] = json!(choice);
                        save_config(&repo, &selected)
                            .map(|_| json!({"selected":selected,"authentication":"unverified"}))
                            .map_err(Into::into)
                    } else {
                        Err("no API provider configured".into())
                    };
                    match result {
                        Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
                        Err(e) => eprintln!("doclify: {e}"),
                    }
                    break;
                }
                shown = doclify::repl::filter_models(&choices, query);
                if shown.is_empty() {
                    println!("No matches; enter another search.");
                }
            }
            continue;
        }
        match run(&parts) {
            Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
            Err(e) => eprintln!("doclify: {e}"),
        }
    }
    let _ = stop.send(());
    ExitCode::SUCCESS
}

fn documentation_request(question: &str) -> Result<Value, Box<dyn std::error::Error>> {
    let repo =
        RepoInfo::discover(&std::env::current_dir()?).ok_or("not inside a Git repository")?;
    let config = load_config(&repo)?.ok_or("select a model first with /model")?;
    let scope = doclify_context::Scope {
        branch: repo.branch.clone(),
        topic: Some(question.into()),
    };
    let context = doclify_context::build(&repo.worktree, &scope, &Default::default())?;
    let response = doclify::repl::request(
        &config,
        question,
        context.as_ref().map(|c| c.render()).unwrap_or(""),
    )?;
    let store = Store::open(&repo.worktree)?;
    let mut records = Vec::new();
    for proposal in response.proposals {
        let kind = match proposal.kind.as_str() {
            "requirement" => RecordKind::Requirement,
            "decision" => RecordKind::Decision,
            "experiment" => RecordKind::Experiment,
            "question" => RecordKind::Question,
            "task" => RecordKind::Task,
            _ => return Err("invalid proposal kind".into()),
        };
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let id = format!(
            "request-{}-{}-{}",
            timestamp,
            std::process::id(),
            NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let record = Record {
            id,
            version: 1,
            kind,
            title: proposal.summary.clone(),
            statement: proposal.summary,
            rationale: proposal.rationale.unwrap_or_default(),
            review: ReviewState::Proposed,
            implementation: match proposal.implementation.as_deref() {
                Some("planned") => ImplementationState::Planned,
                Some("partial") => ImplementationState::Partial,
                Some("implemented") => ImplementationState::Implemented,
                Some("reverted") => ImplementationState::Reverted,
                _ => ImplementationState::Unknown,
            },
            experiment_outcome: None,
            provenance: vec![Provenance {
                source: "interactive-documentation-request".into(),
                excerpt: proposal.excerpt.or_else(|| Some(question.into())),
            }],
            evidence: proposal.evidence,
            supersedes: None,
            scope: None,
            session: None,
        };
        store.propose(&record)?;
        records.push(record);
    }
    Ok(json!({"answer":response.answer,"proposals":records}))
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
    let context = doclify_context::build(&repo.worktree, &scope, &Default::default())?;
    if context
        .as_ref()
        .is_some_and(|c| !c.constraints.is_empty() || !c.open_work.is_empty())
    {
        let _ = doclify_capture::metrics::increment(repo, "handoff_deliveries", 1);
    }
    Ok(serde_json::to_value(context)?)
}
fn docs(background: bool) -> ExitCode {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let (url, worker) = start_docs_viewer()?;
        println!("{}", json!({"url":url}));
        io::stdout().flush()?;
        open_viewer(&url);
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

fn start_docs_viewer(
) -> Result<(String, std::thread::JoinHandle<()>), Box<dyn std::error::Error>> {
    let repo = RepoInfo::discover(&std::env::current_dir()?).ok_or("not inside a Git repository")?;
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
    Ok((url, worker))
}

fn open_viewer(url: &str) {
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(not(target_os = "macos"))]
    let opener = "xdg-open";
    let _ = std::process::Command::new(opener).arg(url).spawn();
}

fn hook_status(worktree: &std::path::Path, agent: Agent) -> Value {
    let path = install::config_path(worktree, agent);
    let parsed = fs::read(&path)
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice::<Value>(&b).map_err(|e| e.to_string()));
    let events = install::EVENTS;
    let missing: Vec<_> = events
        .into_iter()
        .filter(|event| {
            !parsed
                .as_ref()
                .ok()
                .and_then(|v| v["hooks"][*event].as_array())
                .is_some_and(|groups| {
                    groups.iter().any(|g| {
                        g["hooks"].as_array().is_some_and(|handlers| {
                            handlers.iter().any(|h| {
                                h["command"].as_str().is_some_and(|c| {
                                    c.starts_with(install::MARKER)
                                        && c.contains(&format!(" hook {} ", agent.name()))
                                })
                            })
                        })
                    })
                })
        })
        .collect();
    json!({"installed":missing.is_empty(), "missing_events":missing, "config":path, "error":parsed.err(), "execution":"unverified"})
}
fn agent_version(name: &str) -> Value {
    match std::process::Command::new(name).arg("--version").output() {
        Ok(out) if out.status.success() => {
            json!({"reported":String::from_utf8_lossy(&out.stdout).trim(),"compatibility":"unverified"})
        }
        Ok(out) => {
            json!({"reported":null,"error":format!("version command exited {}", out.status),"compatibility":"unverified"})
        }
        Err(e) => json!({"reported":null,"error":e.to_string(),"compatibility":"unverified"}),
    }
}
fn rejected_events(hook_dir: &std::path::Path) -> io::Result<Vec<Value>> {
    let entries = match fs::read_dir(hook_dir.join("rejected")) {
        Ok(v) => v,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e),
    };
    let mut events = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let value: Value = serde_json::from_slice(&fs::read(entry.path())?)?;
        events.push(json!({"id":entry.file_name().to_string_lossy(),"reason":value.get("reason").and_then(Value::as_str).unwrap_or("reason unavailable (rejected by an earlier version)")}));
    }
    events.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    Ok(events)
}
fn rejected_count(hook_dir: &std::path::Path) -> io::Result<usize> {
    match fs::read_dir(hook_dir.join("rejected")) {
        Ok(entries) => Ok(entries.count()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e),
    }
}

fn session_roots() -> Vec<(String, std::path::PathBuf)> {
    let Some(home) = std::env::var_os("HOME") else {
        return vec![];
    };
    let home = std::path::PathBuf::from(home);
    [
        ("claude", home.join(".claude/projects")),
        ("codex", home.join(".codex/sessions")),
    ]
    .into_iter()
    .filter(|(_, root)| root.is_dir())
    .map(|(name, root)| (name.to_owned(), root))
    .collect()
}
fn selected_extractor(repo: &RepoInfo) -> io::Result<Option<doclify_pipeline::SelectedExtractor>> {
    use doclify_model_bridge::providers::{ApiKind, ApiProvider};
    let Some(config) = load_config(repo)? else {
        return Ok(None);
    };
    let kind = match config["provider"].as_str() {
        Some("openai") => ApiKind::OpenAiCompatible,
        Some("anthropic") => ApiKind::Anthropic,
        Some(provider @ ("claude" | "codex")) => {
            return Ok(Some(doclify_pipeline::SelectedExtractor::Cli(
                doclify_pipeline::CliExtractor {
                    bridge: if provider == "claude" {
                        Bridge::claude("claude")
                    } else {
                        Bridge::codex("codex")
                    },
                    model: config["model"].as_str().map(str::to_owned),
                    timeout: std::time::Duration::from_secs(120),
                },
            )))
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid selected provider",
            ))
        }
    };
    let field = |name| {
        config[name]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "incomplete model configuration")
            })
    };
    let model = field("model")?;
    Ok(Some(doclify_pipeline::SelectedExtractor::Api(
        doclify_pipeline::ApiExtractor {
            provider: ApiProvider {
                kind,
                base_url: field("base_url")?,
                api_key_env: field("api_key_env")?,
                models: vec![model.clone()],
            },
            model,
        },
    )))
}

fn captured_jobs(root: &std::path::Path) -> io::Result<Vec<Value>> {
    let mut jobs = Vec::new();
    for name in ["pending", "done"] {
        for entry in fs::read_dir(root.join(name))? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            jobs.push(serde_json::from_slice(&fs::read(entry.path())?)?);
        }
    }
    match fs::read_dir(root.join("events")) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if entry.file_name().to_string_lossy().starts_with('.') {
                    continue;
                }
                let event: Value = serde_json::from_slice(&fs::read(entry.path())?)?;
                if event["payload"]["transcript_path"].is_string()
                    && event["payload"]["hook_event_name"]
                        .as_str()
                        .is_some_and(|name| name.starts_with("PostToolUse"))
                {
                    jobs.push(json!({"source":"hook", "event":event}));
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => (),
        Err(e) => return Err(e),
    }
    Ok(jobs)
}

fn captured_session_count(jobs: &[Value]) -> usize {
    let sessions: std::collections::HashSet<_> = jobs
        .iter()
        .filter_map(|job| {
            let event = job.get("event").unwrap_or(job);
            let agent = job["agent"].as_str().or_else(|| event["agent"].as_str())?;
            let session = job["session_id"]
                .as_str()
                .or_else(|| event["payload"]["session_id"].as_str())?;
            Some((agent.to_owned(), session.to_owned()))
        })
        .collect();
    sessions.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_reads_pending_and_completed_capture_jobs() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["pending", "done"] {
            fs::create_dir(dir.path().join(name)).unwrap();
            fs::write(dir.path().join(name).join("job"), br#"{"source":"hook"}"#).unwrap();
        }
        assert_eq!(captured_jobs(dir.path()).unwrap().len(), 2);
    }
}

#[cfg(test)]
mod audit_tests {
    use super::*;
    #[test]
    fn captured_jobs_include_raw_tool_hooks() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["pending", "done", "events"] {
            fs::create_dir(dir.path().join(name)).unwrap();
        }
        fs::write(dir.path().join("events/tool"), br#"{"agent":"claude","payload":{"hook_event_name":"PostToolUse","transcript_path":"/tmp/transcript","tool_input":{"command":"cargo test"}}}"#).unwrap();
        let jobs = captured_jobs(dir.path()).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(doclify_evidence::verify::test_runs(&jobs).len(), 1);
        assert_eq!(
            jobs[0]["event"]["payload"]["hook_event_name"],
            "PostToolUse"
        );
    }
    #[test]
    fn doctor_checks_hook_entries_not_file_existence() {
        let dir = tempfile::tempdir().unwrap();
        let path = install::config_path(dir.path(), Agent::Claude);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{}").unwrap();
        let status = hook_status(dir.path(), Agent::Claude);
        assert_eq!(status["installed"], false);
        assert_eq!(
            status["missing_events"].as_array().unwrap().len(),
            install::EVENTS.len()
        );
    }
}
