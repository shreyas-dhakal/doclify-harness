//! Deterministic end-to-end tests through the built `doclify` binary: real Git
//! repositories, a temporary HOME holding agent session trees, a PATH without
//! `gh` or a browser, and a loopback fake of an OpenAI-compatible provider.
//!
//! The fake provider returns one proposal for every `[[kind|summary]]` marker in
//! the job it is given, citing that job's source id, so extraction output is a
//! pure function of captured content.
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};

const KEY_ENV: &str = "DOCLIFY_E2E_KEY";

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
    bin: PathBuf,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (repo, home, bin) = (root.join("shop"), root.join("home"), root.join("bin"));
        for d in [
            &repo,
            &home.join(".codex/sessions"),
            &home.join(".claude/projects"),
            &bin,
        ] {
            fs::create_dir_all(d).unwrap();
        }
        // Browser openers record their argument instead of launching anything.
        for opener in ["open", "xdg-open"] {
            let path = bin.join(opener);
            fs::write(
                &path,
                format!(
                    "#!/bin/sh\necho \"$1\" >> '{}'\n",
                    root.join("opened").display()
                ),
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let env = Env {
            _tmp: tmp,
            repo,
            home,
            bin,
        };
        env.git(&env.repo, &["init", "-q", "-b", "main"]);
        fs::write(env.repo.join("checkout.rs"), "fn checkout() {}\n").unwrap();
        env.git(&env.repo, &["add", "."]);
        env.git(&env.repo, &["commit", "-q", "-m", "Initial checkout"]);
        assert!(env.run(&env.repo, &["init"]).status.success());
        env
    }
    fn command(&self, dir: &Path, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut c = Command::new(program);
        c.current_dir(dir)
            .env_clear()
            .env("HOME", &self.home)
            // No `gh`, no real browser: GitHub evidence is reported unavailable.
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()))
            .env("GIT_AUTHOR_NAME", "e2e")
            .env("GIT_AUTHOR_EMAIL", "e2e@example.com")
            .env("GIT_COMMITTER_NAME", "e2e")
            .env("GIT_COMMITTER_EMAIL", "e2e@example.com")
            .env(KEY_ENV, "fixture");
        c
    }
    fn git(&self, dir: &Path, args: &[&str]) {
        let out = self.command(dir, "git").args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    fn run(&self, dir: &Path, args: &[&str]) -> Output {
        self.command(dir, env!("CARGO_BIN_EXE_doclify"))
            .args(args)
            .output()
            .unwrap()
    }
    fn json(&self, dir: &Path, args: &[&str]) -> Value {
        let out = self.run(dir, args);
        assert!(
            out.status.success(),
            "doclify {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
    fn hook(&self, dir: &Path, agent: &str, payload: &Value) -> Output {
        let mut child = self
            .command(dir, env!("CARGO_BIN_EXE_doclify"))
            .args(["hook", agent])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success() && out.stderr.is_empty(),
            "hooks must fail open silently"
        );
        out
    }
    fn use_provider(&self, dir: &Path, provider: &Provider) {
        self.json(
            dir,
            &["model", "set", "openai", &provider.url, KEY_ENV, "fixture"],
        );
    }
    /// Writes a Codex 0.166 rollout whose session_meta cwd is `cwd`.
    fn codex_rollout(&self, session: &str, cwd: &Path, lines: &[Value]) -> PathBuf {
        let path = self.home.join(format!(
            ".codex/sessions/2026/11/03/rollout-{session}.jsonl"
        ));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut body = json!({"ordinal":0,"type":"session_meta","payload":{"id":session,"cwd":cwd,"cli_version":"0.166.0"}}).to_string() + "\n";
        for line in lines {
            body += &(line.to_string() + "\n");
        }
        fs::write(&path, body).unwrap();
        path
    }
    fn pending_review(&self, dir: &Path) -> Vec<Value> {
        self.json(dir, &["review"])["pending_review"]
            .as_array()
            .unwrap()
            .clone()
    }
    fn records(&self, dir: &Path) -> usize {
        fs::read_dir(dir.join(".doclify/records")).map_or(0, |d| d.count())
    }
}

fn codex_message(id: &str, role: &str, text: &str) -> Value {
    let kind = if role == "user" {
        "input_text"
    } else {
        "output_text"
    };
    json!({"type":"response_item","payload":{"type":"message","id":id,"role":role,"content":[{"type":kind,"text":text}]}})
}

/// Loopback OpenAI-compatible fake. Leaked for the test process lifetime.
struct Provider {
    url: String,
    requests: Arc<AtomicUsize>,
    fail: Arc<AtomicBool>,
    hang: Arc<AtomicBool>,
}
impl Provider {
    fn start() -> Provider {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (requests, fail, hang) = Default::default();
        let p = Provider {
            url,
            requests,
            fail,
            hang,
        };
        let (requests, fail, hang) = (p.requests.clone(), p.fail.clone(), p.hang.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let Some(body) = read_request(&mut stream) else {
                    continue;
                };
                requests.fetch_add(1, Ordering::SeqCst);
                if hang.load(Ordering::SeqCst) {
                    std::mem::forget(stream);
                    continue;
                }
                let (status, response) = if fail.load(Ordering::SeqCst) {
                    ("500 Internal Server Error", "{}".to_string())
                } else {
                    let request: Value = serde_json::from_slice(&body).unwrap();
                    let input = request["messages"][1]["content"].as_str().unwrap();
                    let input: Value = serde_json::from_str(input).unwrap();
                    // Only the job's own content: transcript text, or a hook's prompt.
                    let job = &input["event"];
                    let own = job["text"]
                        .as_str()
                        .or(job["event"]["payload"]["prompt"].as_str())
                        .unwrap_or("");
                    let source = input["source_id"].clone();
                    let content = json!({"proposals": markers(own).into_iter().map(|(kind, summary)| {
                        json!({"kind":kind,"summary":summary,"review":"proposed","evidence":[source]})
                    }).collect::<Vec<_>>()});
                    (
                        "200 OK",
                        json!({"choices":[{"message":{"content":content.to_string()}}]})
                            .to_string(),
                    )
                };
                let _ = write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len());
            }
        });
        p
    }
    fn count(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}
fn read_request(stream: &mut std::net::TcpStream) -> Option<Vec<u8>> {
    let mut data = Vec::new();
    let mut buf = [0; 8192];
    let end = loop {
        let n = stream.read(&mut buf).ok().filter(|n| *n > 0)?;
        data.extend_from_slice(&buf[..n]);
        if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let length: usize = String::from_utf8_lossy(&data[..end])
        .lines()
        .find_map(|l| {
            l.split_once(':')
                .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.trim().parse().ok())
        })?;
    while data.len() < end + length {
        let n = stream.read(&mut buf).ok().filter(|n| *n > 0)?;
        data.extend_from_slice(&buf[..n]);
    }
    Some(data[end..end + length].to_vec())
}
/// Distinct `[[kind|summary]]` markers, in order of first appearance.
fn markers(input: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for part in input.split("[[").skip(1) {
        let Some((marker, _)) = part.split_once("]]") else {
            continue;
        };
        let Some((kind, summary)) = marker.split_once('|') else {
            continue;
        };
        let item = (kind.to_owned(), summary.to_owned());
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

fn find<'a>(records: &'a [Value], statement: &str) -> &'a Value {
    records
        .iter()
        .find(|r| r["statement"] == statement)
        .unwrap_or_else(|| panic!("no proposal '{statement}' in {records:#?}"))
}
fn sync(env: &Env, dir: &Path) -> Value {
    let out = env.run(dir, &["sync"]);
    assert!(
        out.status.success(),
        "sync failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// Plan acceptance scenario: a Codex session establishes guest checkout and
/// leaves unfinished work; the user explicitly reviews; a later Claude Code
/// authentication session on another branch receives the accepted constraint
/// with rationale and evidence, and open work labeled as not settled.
#[test]
fn guest_checkout_handover_from_codex_to_claude() {
    let env = Env::new();
    let provider = Provider::start();
    env.use_provider(&env.repo, &provider);
    let session = "019a-guest-checkout";
    let prompt = "Allow guest checkout because customers abandon account creation. [[requirement|Customers can check out without creating an account]]";
    let rollout = env.codex_rollout(session, &env.repo, &[
        codex_message("m1", "user", prompt),
        codex_message("m2", "assistant", "Options: social login, or guest checkout with an optional account after purchase. [[decision|Offer guest checkout; defer account creation until after purchase]] [[experiment|Social login instead of guest checkout]]"),
        json!({"type":"response_item","payload":{"type":"function_call","id":"fc1","name":"apply_patch","arguments":"{\"patch\":\"guest flow\"}"}}),
        json!({"type":"response_item","payload":{"type":"function_call_output","id":"fo1","output":"applied"}}),
        codex_message("m3", "assistant", "Guest checkout is partly implemented; receipt emails for guests are unfinished. [[task|Send order receipts to guest email addresses]]"),
    ]);
    // Real Codex hooks always carry transcript_path and session_id.
    let hook = json!({"hook_event_name":"UserPromptSubmit","session_id":session,"cwd":env.repo,"transcript_path":rollout,"prompt":prompt});
    assert!(env.hook(&env.repo, "codex", &hook).stdout.is_empty());
    env.hook(&env.repo, "codex", &json!({"hook_event_name":"Stop","session_id":session,"cwd":env.repo,"transcript_path":rollout}));
    assert_eq!(env.json(&env.repo, &["status"])["pending_events"], 2);

    let synced = sync(&env, &env.repo);
    assert_eq!(synced["pending_extraction"], 0, "{synced:#}");

    // Nothing is accepted by extraction.
    let pending = env.pending_review(&env.repo);
    assert!(pending.iter().all(|r| r["review"] == "proposed"));
    let requirement = find(
        &pending,
        "Customers can check out without creating an account",
    )
    .clone();
    let decision = find(
        &pending,
        "Offer guest checkout; defer account creation until after purchase",
    )
    .clone();
    let experiment = find(&pending, "Social login instead of guest checkout").clone();
    find(&pending, "Send order receipts to guest email addresses");
    assert_eq!(pending.len(), 4, "each proposal exactly once: {pending:#?}");
    let before = env.json(&env.repo, &["handoff"]);
    assert!(
        !before.to_string().contains("Accepted constraints"),
        "unreviewed records must not be constraints: {before:#}"
    );

    for (record, action) in [
        (&requirement, "accept"),
        (&decision, "accept"),
        (&experiment, "reject"),
    ] {
        let id = record["id"].as_str().unwrap();
        env.json(&env.repo, &["review", id, "1", action]);
    }
    // Re-syncing must not resurrect or duplicate anything.
    sync(&env, &env.repo);
    assert_eq!(env.pending_review(&env.repo).len(), 1);
    assert_eq!(env.records(&env.repo), 4);

    // Later Claude Code authentication session on another branch.
    env.git(&env.repo, &["checkout", "-q", "-b", "auth-login"]);
    let out = env.hook(&env.repo, "claude", &json!({"hook_event_name":"SessionStart","source":"startup","session_id":"claude-auth-1","cwd":env.repo}));
    let delivered: Value =
        serde_json::from_slice(&out.stdout).expect("SessionStart must deliver context");
    assert_eq!(
        delivered["hookSpecificOutput"]["hookEventName"],
        "SessionStart"
    );
    let context = delivered["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    let (settled, open) = context
        .split_once("## Open work")
        .expect("open work section");
    assert!(settled.contains("## Accepted constraints"), "{context}");
    assert!(
        settled.contains("Customers can check out without creating an account"),
        "{context}"
    );
    assert!(
        settled.contains("Offer guest checkout; defer account creation"),
        "{context}"
    );
    let evidence = decision["evidence"][0].as_str().unwrap();
    assert!(
        settled.contains(evidence),
        "evidence citation missing: {context}"
    );
    assert!(
        open.contains("Send order receipts to guest email addresses")
            && open.contains("not settled"),
        "{context}"
    );
    assert!(
        !context.contains("Social login"),
        "rejected experiment leaked into context: {context}"
    );
    // The user's stated reason must travel with the constraint.
    assert!(
        settled.contains("abandon account creation"),
        "accepted constraint carries no rationale: {context}"
    );
}

/// A session in a linked worktree is captured into the shared queue once, and
/// its proposal is reviewable where the session ran, whichever worktree syncs.
#[test]
fn linked_worktree_capture_is_shared_and_not_duplicated() {
    let env = Env::new();
    let provider = Provider::start();
    env.use_provider(&env.repo, &provider);
    let wt = env.repo.parent().unwrap().join("shop-auth");
    env.git(
        &env.repo,
        &["worktree", "add", "-q", "-b", "auth", wt.to_str().unwrap()],
    );
    assert_eq!(
        env.json(&wt, &["status"])["registered"],
        true,
        "worktrees share registration"
    );
    let out = env.hook(&wt, "claude", &json!({"hook_event_name":"UserPromptSubmit","session_id":"wt-1","cwd":wt,"prompt":"[[decision|Sessions expire after 30 minutes]]"}));
    assert!(out.stdout.is_empty());
    assert_eq!(
        env.json(&env.repo, &["status"])["pending_events"],
        1,
        "queue lives in the common dir"
    );
    sync(&env, &env.repo);
    sync(&env, &wt);
    let all: Vec<Value> = [&env.repo, &wt]
        .iter()
        .flat_map(|d| env.pending_review(d))
        .collect();
    assert_eq!(
        all.iter()
            .filter(|r| r["statement"] == "Sessions expire after 30 minutes")
            .count(),
        1,
        "{all:#?}"
    );
    find(&env.pending_review(&wt), "Sessions expire after 30 minutes");
}

/// Provider failure, a killed sync, and restarts neither lose nor duplicate work,
/// and completed work is never sent to the provider again.
#[test]
fn restart_after_provider_failure_and_kill_is_lossless() {
    let env = Env::new();
    let provider = Provider::start();
    env.use_provider(&env.repo, &provider);
    let rollout = env.codex_rollout(
        "restart-1",
        &env.repo,
        &[codex_message(
            "m1",
            "user",
            "[[requirement|Guest carts persist for 7 days]]",
        )],
    );
    env.hook(&env.repo, "codex", &json!({"hook_event_name":"UserPromptSubmit","session_id":"restart-1","cwd":env.repo,"prompt":"[[decision|Guest emails are verified at checkout]]"}));

    provider.fail.store(true, Ordering::SeqCst);
    let out = env.run(&env.repo, &["sync"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("remains pending"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        env.json(&env.repo, &["status"])["pending_extraction"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(env.records(&env.repo), 0);

    provider.fail.store(false, Ordering::SeqCst);
    provider.hang.store(true, Ordering::SeqCst);
    let mut child: Child = env
        .command(&env.repo, env!("CARGO_BIN_EXE_doclify"))
        .arg("sync")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let seen = provider.count();
    while provider.count() == seen {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    child.kill().unwrap();
    child.wait().unwrap();

    provider.hang.store(false, Ordering::SeqCst);
    sync(&env, &env.repo);
    let pending = env.pending_review(&env.repo);
    find(&pending, "Guest carts persist for 7 days");
    find(&pending, "Guest emails are verified at checkout");
    assert_eq!(pending.len(), 2, "{pending:#?}");

    let calls = provider.count();
    sync(&env, &env.repo);
    assert_eq!(provider.count(), calls, "completed work was re-extracted");
    assert_eq!(env.records(&env.repo), 2);

    // An appended transcript line is extracted alone.
    let mut f = fs::OpenOptions::new().append(true).open(&rollout).unwrap();
    writeln!(
        f,
        "{}",
        codex_message(
            "m2",
            "user",
            "[[question|Should guest orders merge into a later account?]]"
        )
    )
    .unwrap();
    sync(&env, &env.repo);
    assert_eq!(provider.count(), calls + 1);
    assert_eq!(env.pending_review(&env.repo).len(), 3);
}

/// Concurrent hooks lose nothing; concurrent syncs serialize, and every event is
/// extracted exactly once.
#[test]
fn concurrent_hooks_and_syncs_extract_each_event_once() {
    let env = Env::new();
    let provider = Provider::start();
    env.use_provider(&env.repo, &provider);
    const HOOKS: usize = 16;
    std::thread::scope(|s| {
        for i in 0..HOOKS {
            let env = &env;
            s.spawn(move || {
                let agent = if i % 2 == 0 { "codex" } else { "claude" };
                env.hook(&env.repo, agent, &json!({"hook_event_name":"UserPromptSubmit","session_id":format!("c{i}"),"cwd":env.repo,"prompt":format!("[[task|Concurrent task {i}]]")}));
            });
        }
    });
    assert_eq!(env.json(&env.repo, &["status"])["pending_events"], HOOKS);

    let outputs: Vec<Output> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..4)
            .map(|_| s.spawn(|| env.run(&env.repo, &["sync"])))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for out in &outputs {
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success() || stderr.contains("another sync is already running"),
            "{stderr}"
        );
    }
    assert!(outputs.iter().any(|o| o.status.success()));
    sync(&env, &env.repo);
    let pending = env.pending_review(&env.repo);
    assert_eq!(pending.len(), HOOKS, "{pending:#?}");
    for i in 0..HOOKS {
        find(&pending, &format!("Concurrent task {i}"));
    }
    assert_eq!(provider.count(), HOOKS, "each event extracted exactly once");
}

/// Copies the built executable outside the source tree and runs `doclify docs`
/// against it. With `--release` this exercises the embedded assets exactly as
/// shipped; debug builds read `viewer/web/dist` from disk (rust-embed default).
fn packaged_viewer(env: &Env) -> (Child, String, String) {
    let exe = env.bin.join("doclify");
    fs::copy(env!("CARGO_BIN_EXE_doclify"), &exe).unwrap();
    let mut child = env
        .command(&env.repo, &exe)
        .arg("docs")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufRead::read_line(
        &mut std::io::BufReader::new(child.stdout.take().unwrap()),
        &mut line,
    )
    .unwrap();
    let url = serde_json::from_str::<Value>(&line).unwrap()["url"]
        .as_str()
        .unwrap()
        .to_owned();
    let (origin, token) = url.split_once("/#").unwrap();
    (child, origin.to_owned(), token.to_owned())
}

#[test]
fn packaged_viewer_serves_assets_docs_and_protected_writes() {
    let env = Env::new();
    let provider = Provider::start();
    env.use_provider(&env.repo, &provider);
    env.hook(&env.repo, "claude", &json!({"hook_event_name":"UserPromptSubmit","session_id":"v1","cwd":env.repo,"prompt":"[[decision|Guest checkout is allowed]]"}));
    sync(&env, &env.repo);
    let (child, origin, token) = packaged_viewer(&env);
    let _viewer = KillOnDrop(child);
    let http = reqwest::blocking::Client::new();
    {
        assert!(
            origin.starts_with("http://127.0.0.1:"),
            "viewer must bind loopback: {origin}"
        );
        // The opener is spawned after the URL is printed.
        let opened_path = env.home.parent().unwrap().join("opened");
        let mut opened = String::new();
        for _ in 0..500 {
            opened = fs::read_to_string(&opened_path).unwrap_or_default();
            if opened.ends_with('\n') {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            opened.trim(),
            format!("{origin}/#{token}"),
            "browser opened with the tokenized URL"
        );

        let index = http.get(&origin).send().unwrap();
        assert_eq!(index.status(), 200);
        assert!(index.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("script-src 'self'"));
        let html = index.text().unwrap();
        let script = html
            .split("src=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .expect("index references a bundle");
        let asset = http.get(format!("{origin}{script}")).send().unwrap();
        assert_eq!(asset.status(), 200);
        assert!(asset.headers()["content-type"]
            .to_str()
            .unwrap()
            .contains("javascript"));
        let shipped = fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../viewer/web/dist")
                .join(script.trim_start_matches('/')),
        )
        .unwrap();
        assert_eq!(asset.bytes().unwrap().as_ref(), shipped.as_slice());

        let docs: Vec<String> = http
            .get(format!("{origin}/api/documents"))
            .send()
            .unwrap()
            .json()
            .unwrap();
        assert!(docs.contains(&"decisions.mdx".to_owned()), "{docs:?}");
        let doc: Value = http
            .get(format!("{origin}/api/document?path=decisions.mdx"))
            .send()
            .unwrap()
            .json()
            .unwrap();
        assert!(doc["content"]
            .as_str()
            .unwrap()
            .contains("Guest checkout is allowed"));
        let save = |headers: &[(&str, &str)], revision: &Value| {
            let mut req = http
                .put(format!("{origin}/api/document"))
                .json(&json!({"path":"decisions.mdx","content":"# Edited\n","revision":revision}));
            for (k, v) in headers {
                req = req.header(*k, *v);
            }
            req.send().unwrap().status().as_u16()
        };
        assert_eq!(
            save(&[("origin", &origin)], &doc["revision"]),
            403,
            "write without token"
        );
        assert_eq!(
            save(
                &[
                    ("origin", "http://evil.example"),
                    ("x-doclify-token", &token)
                ],
                &doc["revision"]
            ),
            403,
            "cross-origin write"
        );
        assert_eq!(
            save(
                &[("origin", &origin), ("x-doclify-token", &token)],
                &doc["revision"]
            ),
            200
        );
        assert_eq!(
            save(
                &[("origin", &origin), ("x-doclify-token", &token)],
                &doc["revision"]
            ),
            409,
            "stale revision"
        );
        assert_eq!(
            fs::read_to_string(env.repo.join("docs/doclify/decisions.mdx")).unwrap(),
            "# Edited\n"
        );

        // Review in the viewer is the same explicit review the CLI and hooks see.
        let records: Vec<Value> = http
            .get(format!("{origin}/api/records"))
            .send()
            .unwrap()
            .json()
            .unwrap();
        let id = find(&records, "Guest checkout is allowed")["id"].clone();
        let review = |token: &str| {
            http.post(format!("{origin}/api/review"))
                .header("origin", &origin)
                .header("x-doclify-token", token)
                .json(&json!({"id":id,"version":1,"action":"accept"}))
                .send()
                .unwrap()
                .status()
                .as_u16()
        };
        assert_eq!(review("wrong"), 403);
        assert_eq!(review(&token), 200);
        assert!(env
            .json(&env.repo, &["handoff"])
            .to_string()
            .contains("Guest checkout is allowed"));
        // Regeneration must keep the human edit.
        sync(&env, &env.repo);
        assert!(
            fs::read_to_string(env.repo.join("docs/doclify/decisions.mdx"))
                .unwrap()
                .contains("# Edited")
        );
    }
}
struct KillOnDrop(Child);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The shipped binary must not depend on `viewer/web/dist` existing at runtime.
#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "debug builds read viewer assets from disk; run with --release"
)]
fn release_binary_embeds_viewer_assets() {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../viewer/web/dist/assets");
    let binary = fs::read(env!("CARGO_BIN_EXE_doclify")).unwrap();
    for entry in fs::read_dir(dist).unwrap() {
        let bytes = fs::read(entry.unwrap().path()).unwrap();
        let probe = &bytes[bytes.len() / 2..bytes.len() / 2 + 256];
        assert!(
            binary.windows(probe.len()).any(|w| w == probe),
            "asset not embedded"
        );
    }
}
