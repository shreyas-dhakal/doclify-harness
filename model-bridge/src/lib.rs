//! Constrained, structured documentation inference via installed agent CLIs.
//! A successful capability check is not an authentication check.
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
    time::timeout,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capability {
    ConstrainedAuthenticationUnverified,
    Unavailable(String),
}
#[derive(Debug)]
pub enum BridgeError {
    Unsupported(String),
    Io(String),
    Timeout,
    Process {
        code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    Provider(Value),
    InvalidOutput(String),
    InvalidSchema(String),
}
#[derive(Debug, Clone)]
pub struct Request {
    pub prompt: String,
    pub schema: Value,
    pub model: Option<String>,
    pub timeout: Duration,
}
#[derive(Debug, Clone, Copy)]
enum Kind {
    Claude,
    Codex,
}
#[derive(Debug, Clone)]
pub struct Bridge {
    executable: PathBuf,
    kind: Kind,
}
impl Bridge {
    pub fn claude(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            kind: Kind::Claude,
        }
    }
    pub fn codex(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            kind: Kind::Codex,
        }
    }
    pub async fn capability(&self) -> Capability {
        if matches!(self.kind, Kind::Codex) {
            // Fail closed without spawning Codex. codex-cli 0.166.0 `exec --help` has no
            // tool-disabling option; `--sandbox` only governs shell commands. Even with
            // shell_tool, unified_exec, code_mode_host and sleep_tool disabled,
            // `codex debug prompt-input` still lists exec (nested apply_patch, view_image,
            // MCP resources) and collaboration.spawn_agent as model-visible tools.
            return Capability::Unavailable("Codex exec has no verifiable tool-free mode: --sandbox restricts only shell commands and feature disables leave exec/apply_patch/spawn_agent model-visible".into());
        }
        let output = match timeout(
            Duration::from_secs(5),
            Command::new(&self.executable)
                .arg("--help")
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        {
            Ok(Ok(output)) if output.status.success() => output,
            Ok(Ok(output)) => {
                return Capability::Unavailable(format!(
                    "help exited {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                ))
            }
            Ok(Err(err)) => return Capability::Unavailable(err.to_string()),
            Err(_) => return Capability::Unavailable("help timed out".into()),
        };
        let help = String::from_utf8_lossy(&output.stdout);
        for flag in [
            "--print",
            "--output-format",
            "--json-schema",
            "--verbose",
            "--tools",
            "--restricted",
            "--safe-mode",
            "--strict-mcp-config",
            "--no-session-persistence",
            "--disable-slash-commands",
            "--permission-mode",
            "--permission-prompts",
        ] {
            if !help.contains(flag) {
                return Capability::Unavailable(format!("missing required capability {flag}"));
            }
        }
        Capability::ConstrainedAuthenticationUnverified
    }
    pub async fn execute(&self, request: Request) -> Result<Value, BridgeError> {
        let validator = jsonschema::JSONSchema::compile(&request.schema)
            .map_err(|e| BridgeError::InvalidSchema(e.to_string()))?;
        if let Capability::Unavailable(reason) = self.capability().await {
            return Err(BridgeError::Unsupported(reason));
        }
        let mut cmd = Command::new(&self.executable);
        cmd.args([
            "--print",
            "--output-format",
            "stream-json",
            "--verbose",
            "--json-schema",
            &request.schema.to_string(),
            "--tools",
            "",
            "--restricted",
            "--safe-mode",
            "--strict-mcp-config",
            "--no-session-persistence",
            "--disable-slash-commands",
            "--permission-mode",
            "dontAsk",
            "--permission-prompts",
            "none",
        ]);
        if let Some(model) = &request.model {
            cmd.arg("--model").arg(model);
        }
        cmd.env("DOCLIFY_GENERATED_SESSION", "1")
            .env("DOCLIFY_BRIDGE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| BridgeError::Io(e.to_string()))?;
        let (mut stdin, stdout, mut stderr) =
            match (child.stdin.take(), child.stdout.take(), child.stderr.take()) {
                (Some(i), Some(o), Some(e)) => (i, o, e),
                _ => return Err(BridgeError::Io("missing stdio".into())),
            };
        let operation = async {
            let (write_result, stderr, events) = tokio::join!(
                async {
                    let result = stdin.write_all(request.prompt.as_bytes()).await;
                    drop(stdin);
                    result
                },
                async {
                    let mut buf = Vec::new();
                    let _ = stderr.read_to_end(&mut buf).await;
                    String::from_utf8_lossy(&buf).into_owned()
                },
                async {
                    // Enforce the restriction the CLI reports at runtime, not just its flags:
                    // the init event must precede model output and expose only StructuredOutput.
                    let mut lines = BufReader::new(stdout).lines();
                    let (mut raw, mut verified, mut result) = (String::new(), false, None);
                    while let Ok(Some(line)) = lines.next_line().await {
                        raw.push_str(&line);
                        raw.push('\n');
                        let Ok(event) = serde_json::from_str::<Value>(&line) else {
                            continue;
                        };
                        let violation = match (event["type"].as_str(), event["subtype"].as_str()) {
                            (Some("system"), Some("init")) => {
                                verified = true;
                                init_violation(&event)
                            }
                            (Some("assistant" | "user" | "result"), _) if !verified => {
                                Some("model output before the init event".into())
                            }
                            _ => None,
                        };
                        if let Some(reason) = violation {
                            let _ = child.start_kill();
                            return Err(BridgeError::Unsupported(format!(
                                "runtime tool constraint not verified: {reason}"
                            )));
                        }
                        if event["type"] == "result" {
                            result = Some(event);
                        }
                    }
                    Ok((raw, verified, result))
                }
            );
            let (stdout, verified, result) = events?;
            let status = child
                .wait()
                .await
                .map_err(|e| BridgeError::Io(e.to_string()))?;
            if !status.success() {
                return Err(BridgeError::Process {
                    code: status.code(),
                    stdout,
                    stderr,
                });
            }
            write_result.map_err(|e| BridgeError::Io(e.to_string()))?;
            if !verified {
                return Err(BridgeError::Unsupported(
                    "runtime tool constraint not verified: no init event".into(),
                ));
            }
            result.ok_or_else(|| BridgeError::InvalidOutput("missing result event".into()))
        };
        let envelope = timeout(request.timeout, operation)
            .await
            .map_err(|_| BridgeError::Timeout)??;
        if envelope.get("is_error").and_then(Value::as_bool) == Some(true) {
            return Err(BridgeError::Provider(envelope));
        }
        let value = envelope
            .get("structured_output")
            .ok_or_else(|| BridgeError::InvalidOutput("missing structured_output".into()))?;
        if let Err(errors) = validator.validate(value) {
            return Err(BridgeError::InvalidOutput(
                errors.map(|e| e.to_string()).collect::<Vec<_>>().join("; "),
            ));
        }
        Ok(value.clone())
    }
}

/// Reasons a Claude init event shows more than the structured-output tool or any MCP server.
fn init_violation(init: &Value) -> Option<String> {
    let tools = &init["tools"];
    if tools != &serde_json::json!(["StructuredOutput"]) {
        return Some(format!("model-visible tools {tools}"));
    }
    match init["mcp_servers"].as_array() {
        Some(servers) if servers.is_empty() => None,
        _ => Some(format!("MCP servers {}", init["mcp_servers"])),
    }
}

/// Diagnostic only, never used by [`Bridge::execute`]: the tools Codex would expose to the model,
/// as rendered by `codex debug prompt-input` with `extra_args` (e.g. `--disable shell_tool`) in an
/// empty `CODEX_HOME` and working directory, so no credentials are read and no inference runs.
/// Errors when the rendering has no `additional_tools` item: tools then travel outside the rendered
/// input (as with `--disable incremental_tools`), so an empty list would not be evidence.
pub async fn codex_visible_tools(
    executable: impl AsRef<Path>,
    extra_args: &[&str],
) -> Result<Vec<String>, BridgeError> {
    let home = tempfile::tempdir().map_err(|e| BridgeError::Io(e.to_string()))?;
    let output = timeout(
        Duration::from_secs(30),
        Command::new(executable.as_ref())
            .args(["debug", "prompt-input"])
            .args(extra_args)
            .env("CODEX_HOME", home.path())
            .current_dir(home.path())
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| BridgeError::Timeout)?
    .map_err(|e| BridgeError::Io(e.to_string()))?;
    if !output.status.success() {
        return Err(BridgeError::Process {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    let items: Vec<Value> = serde_json::from_slice(&output.stdout)
        .map_err(|e| BridgeError::InvalidOutput(format!("invalid prompt input: {e}")))?;
    fn walk(tools: &[Value], prefix: &str, out: &mut Vec<String>) {
        for tool in tools {
            let name = tool["name"].as_str().unwrap_or("?");
            match tool["tools"].as_array() {
                Some(nested) => walk(nested, &format!("{prefix}{name}."), out),
                None => out.push(format!("{prefix}{name}")),
            }
        }
    }
    let mut names = Vec::new();
    let mut rendered = false;
    for item in items.iter().filter(|i| i["type"] == "additional_tools") {
        rendered = true;
        walk(
            item["tools"].as_array().map_or(&[], Vec::as_slice),
            "",
            &mut names,
        );
    }
    if !rendered {
        return Err(BridgeError::InvalidOutput(
            "no additional_tools item: model-visible tools are not rendered".into(),
        ));
    }
    Ok(names)
}

/// Configured API providers and extraction proposal validation.
pub mod providers;

/// Structured documentation questions and proposed edits.
pub mod documentation;
