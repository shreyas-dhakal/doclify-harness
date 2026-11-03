//! Constrained, structured documentation inference via installed agent CLIs.
//! A successful capability check is not an authentication check.
use serde_json::Value;
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command, time::timeout};

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
            return Capability::Unavailable("Codex read-only sandbox does not disable model tool execution; no verified tool-free mode".into());
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
            "json",
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
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| BridgeError::Io("missing stdin".into()))?;
        let operation = async {
            let write_result = stdin.write_all(request.prompt.as_bytes()).await;
            drop(stdin);
            let output = child
                .wait_with_output()
                .await
                .map_err(|e| BridgeError::Io(e.to_string()))?;
            if output.status.success() {
                write_result.map_err(|e| BridgeError::Io(e.to_string()))?;
            }
            Ok::<_, BridgeError>(output)
        };
        let output = timeout(request.timeout, operation)
            .await
            .map_err(|_| BridgeError::Timeout)??;
        if !output.status.success() {
            return Err(BridgeError::Process {
                code: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        let envelope: Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| BridgeError::InvalidOutput(format!("invalid JSON envelope: {e}")))?;
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

/// Configured API providers and extraction proposal validation.
pub mod providers;
