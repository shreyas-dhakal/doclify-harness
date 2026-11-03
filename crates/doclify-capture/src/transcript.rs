//! Versioned, incremental transcript readers. Transcript formats are not a
//! stable agent interface, so anything unrecognised is reported, never
//! silently treated as complete capture.
//!
//! Verified against Codex CLI 0.166.0 rollouts and Claude Code 2.1.308 sessions.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

use crate::Agent;

pub const CODEX_FORMAT: &str = "codex-rollout/0.166";
pub const CLAUDE_FORMAT: &str = "claude-session/2.1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    SessionMeta,
    User,
    Assistant,
    ToolCall,
    ToolResult,
}

#[derive(Debug, Clone)]
pub struct Entry {
    /// Stable per-line id from the agent (Claude `uuid`, Codex item id or
    /// ordinal), used downstream to deduplicate replays.
    pub id: Option<String>,
    pub offset: u64,
    pub kind: Kind,
    pub text: String,
    pub raw: Value,
}

#[derive(Debug, Default)]
pub struct ReadResult {
    pub entries: Vec<Entry>,
    /// Byte offset to resume from. Never points inside a line.
    pub next_offset: u64,
    /// Line types not understood by this adapter version, and unparseable lines.
    pub unsupported: Vec<String>,
}

/// Reads complete lines from `offset`. A trailing line without `\n` is still
/// being written and is left for the next call. If the file shrank (rewritten
/// or rotated), reading restarts at 0; entry ids make the replay idempotent.
pub fn read(agent: Agent, path: &Path, offset: u64) -> io::Result<ReadResult> {
    let file = File::open(path)?;
    read_from(agent, file, offset)
}

/// Parse an immutable snapshot so offsets and hashes describe the same bytes.
pub fn read_from(agent: Agent, mut file: impl Read + Seek, offset: u64) -> io::Result<ReadResult> {
    let offset = if file.seek(SeekFrom::End(0))? < offset {
        0
    } else {
        offset
    };
    file.seek(SeekFrom::Start(offset))?;
    let mut reader = BufReader::new(file);
    let mut out = ReadResult {
        next_offset: offset,
        ..Default::default()
    };
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line)?;
        if n == 0 || line.last() != Some(&b'\n') {
            break;
        }
        let line_offset = out.next_offset;
        out.next_offset += n as u64;
        let before = out.entries.len();
        let Ok(v) = serde_json::from_slice::<Value>(&line) else {
            out.unsupported.push("unparseable line".into());
            continue;
        };
        match agent {
            Agent::Codex => codex(v, &mut out),
            Agent::Claude => claude(v, &mut out),
        }
        for entry in &mut out.entries[before..] {
            entry.offset = line_offset;
        }
    }
    Ok(out)
}

fn id_of(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::to_owned)
        .or_else(|| v.as_u64().map(|n| n.to_string()))
}

/// Joins `text` fields of a content-block array, or returns a plain string.
fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b["text"].as_str().or_else(|| b["content"].as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn codex(v: Value, out: &mut ReadResult) {
    let p = &v["payload"];
    let ptype = p["type"].as_str().unwrap_or("");
    let id = id_of(&p["id"]).or_else(|| id_of(&v["ordinal"]));
    let (kind, text) = match (v["type"].as_str().unwrap_or(""), ptype) {
        ("session_meta", _) => (Kind::SessionMeta, String::new()),
        ("response_item", "message") => match p["role"].as_str() {
            Some("user") => (Kind::User, text_of(&p["content"])),
            Some("assistant") => (Kind::Assistant, text_of(&p["content"])),
            // Developer messages are Codex's own instructions, not session content.
            _ => return,
        },
        ("response_item", "function_call") => (
            Kind::ToolCall,
            format!(
                "{} {}",
                p["name"].as_str().unwrap_or(""),
                p["arguments"].as_str().unwrap_or("")
            ),
        ),
        ("response_item", "custom_tool_call") => (
            Kind::ToolCall,
            format!(
                "{} {}",
                p["name"].as_str().unwrap_or(""),
                p["input"].as_str().unwrap_or("")
            ),
        ),
        ("response_item", "function_call_output" | "custom_tool_call_output") => {
            (Kind::ToolResult, text_of(&p["output"]))
        }
        // Answers to Codex's clarifying questions: often the user's actual decisions.
        ("retained_context", "verified_answer") => (
            Kind::User,
            p["questions"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|q| {
                    format!(
                        "Q: {}\nA: {}",
                        q["question"].as_str().unwrap_or(""),
                        q["answer"].as_str().unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        // Known and intentionally skipped: duplicated UI events, usage, encrypted reasoning, config.
        ("event_msg" | "token_usage_record" | "turn_context" | "world_state" | "compacted", _)
        | ("response_item", "reasoning" | "additional_tools" | "configuration_update") => return,
        (t, pt) => return out.unsupported.push(format!("codex {t}/{pt}")),
    };
    out.entries.push(Entry {
        id,
        offset: 0,
        kind,
        text,
        raw: v,
    });
}

fn claude(v: Value, out: &mut ReadResult) {
    let content = &v["message"]["content"];
    let first = content.get(0).and_then(|b| b["type"].as_str());
    let (kind, text) = match (v["type"].as_str().unwrap_or(""), first) {
        ("user", Some("tool_result")) => (
            Kind::ToolResult,
            content
                .as_array()
                .into_iter()
                .flatten()
                .map(|b| text_of(&b["content"]))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        ("user", _) => (Kind::User, text_of(content)),
        ("assistant", Some("tool_use")) => (
            Kind::ToolCall,
            content
                .as_array()
                .into_iter()
                .flatten()
                .map(|b| format!("{} {}", b["name"].as_str().unwrap_or(""), b["input"]))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        // Thinking blocks are signed/redacted; nothing to document.
        ("assistant", Some("thinking" | "redacted_thinking")) => return,
        ("assistant", _) => (Kind::Assistant, text_of(content)),
        (
            "attachment"
            | "api-request"
            | "api-request-blob"
            | "api-request-shape"
            | "last-prompt"
            | "mode"
            | "permission-mode"
            | "atis-latch"
            | "ai-title"
            | "file-history-snapshot"
            | "file-history-delta"
            | "summary"
            | "system"
            | "custom-title"
            | "cost-state"
            | "queue-operation",
            _,
        ) => return,
        (t, _) => return out.unsupported.push(format!("claude {t}")),
    };
    out.entries.push(Entry {
        id: id_of(&v["uuid"]),
        offset: 0,
        kind,
        text,
        raw: v,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn file(name: &str, body: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("doclify-{}-{name}", std::process::id()));
        fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn codex_incremental_with_partial_line() {
        let p = file(
            "codex.jsonl",
            concat!(
                r#"{"ordinal":0,"type":"session_meta","payload":{"id":"s1","cwd":"/r","cli_version":"0.166.0"}}"#,
                "\n",
                r#"{"ordinal":1,"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"sys"}]}}"#,
                "\n",
                r#"{"ordinal":2,"type":"response_item","payload":{"type":"message","id":"m1","role":"user","content":[{"type":"input_text","text":"allow guest checkout"}]}}"#,
                "\n",
                r#"{"ordinal":3,"type":"response_item","payload":{"type":"reasoning","encrypted_content":"x"}}"#,
                "\n",
                r#"{"ordinal":4,"type":"brand_new","payload":{}}"#,
                "\n",
                r#"{"ordinal":6,"type":"retained_context","payload":{"type":"verified_answer","questions":[{"question":"Guest checkout?","answer":"Yes"}]}}"#,
                "\n",
                r#"{"ordinal":5,"type":"response_item","payload":{"type":"function_call","name":"sh","arguments":"{\"cmd\":\"ls\"}"}}"#
            ),
        );
        let r = read(Agent::Codex, &p, 0).unwrap();
        let kinds: Vec<_> = r.entries.iter().map(|e| e.kind.clone()).collect();
        assert_eq!(kinds, [Kind::SessionMeta, Kind::User, Kind::User]);
        assert_eq!(r.entries[2].text, "Q: Guest checkout?\nA: Yes");
        assert_eq!(r.entries[1].text, "allow guest checkout");
        assert_eq!(r.entries[1].id.as_deref(), Some("m1"));
        assert_eq!(r.entries[0].id.as_deref(), Some("s1"));
        assert_eq!(r.unsupported, ["codex brand_new/"]);

        fs::OpenOptions::new()
            .append(true)
            .open(&p)
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        let r2 = read(Agent::Codex, &p, r.next_offset).unwrap();
        assert_eq!(r2.entries.len(), 1);
        assert_eq!(r2.entries[0].kind, Kind::ToolCall);
        assert_eq!(r2.entries[0].id.as_deref(), Some("5"));
        assert_eq!(
            read(Agent::Codex, &p, r2.next_offset)
                .unwrap()
                .entries
                .len(),
            0
        );

        fs::write(&p, "{}\n").unwrap(); // truncated: restart from 0
        assert_eq!(
            read(Agent::Codex, &p, r2.next_offset).unwrap().next_offset,
            3
        );
        fs::remove_file(p).unwrap();
    }

    #[test]
    fn claude_kinds() {
        let p = file(
            "claude.jsonl",
            concat!(
                r#"{"type":"user","uuid":"u1","message":{"role":"user","content":"why guest checkout?"}}"#,
                "\n",
                r#"{"type":"assistant","uuid":"a0","message":{"content":[{"type":"thinking","thinking":""}]}}"#,
                "\n",
                r#"{"type":"assistant","uuid":"a1","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#,
                "\n",
                r#"{"type":"user","uuid":"u2","message":{"content":[{"type":"tool_result","content":"src"}]}}"#,
                "\n",
                r#"{"type":"assistant","uuid":"a2","message":{"content":[{"type":"text","text":"done"}]}}"#,
                "\n",
                r#"{"type":"attachment"}"#,
                "\n",
                "not json\n",
            ),
        );
        let r = read(Agent::Claude, &p, 0).unwrap();
        let got: Vec<_> = r
            .entries
            .iter()
            .map(|e| (e.id.clone().unwrap(), e.kind.clone(), e.text.clone()))
            .collect();
        assert_eq!(
            got,
            [
                ("u1".into(), Kind::User, "why guest checkout?".into()),
                (
                    "a1".into(),
                    Kind::ToolCall,
                    r#"Bash {"command":"ls"}"#.into()
                ),
                ("u2".into(), Kind::ToolResult, "src".into()),
                ("a2".into(), Kind::Assistant, "done".into()),
            ]
        );
        assert_eq!(r.unsupported, ["unparseable line"]);
        fs::remove_file(p).unwrap();
    }

    /// Opt-in check against real transcripts on this machine:
    /// `DOCLIFY_REAL_CODEX=<rollout.jsonl> DOCLIFY_REAL_CLAUDE=<session.jsonl> cargo test -- --ignored`
    #[test]
    #[ignore]
    fn real_transcripts_have_no_unsupported_lines() {
        for (agent, var) in [
            (Agent::Codex, "DOCLIFY_REAL_CODEX"),
            (Agent::Claude, "DOCLIFY_REAL_CLAUDE"),
        ] {
            let Some(path) = std::env::var_os(var) else {
                continue;
            };
            let r = read(agent, Path::new(&path), 0).unwrap();
            assert!(!r.entries.is_empty(), "{var}: no entries");
            assert!(r.unsupported.is_empty(), "{var}: {:?}", r.unsupported);
        }
    }
}
