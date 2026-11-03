# doclify-capture

Phase 2 capture slice: agent hooks → local queue, versioned transcript readers, additive hook install/uninstall. No dependency on the domain crate.

Verified against Codex CLI 0.166.0 and Claude Code 2.1.308 (2026-11-03).

## How it works

- `doclify-capture install` (inside a repo) creates `<git-common-dir>/doclify/queue/` (this marks the repo as registered, is shared by all worktrees, and is never tracked) and adds hooks for `SessionStart`, `UserPromptSubmit`, `Stop`, `SubagentStop` to:
  - Claude: `.claude/settings.local.json` (personal, not committed)
  - Codex: `.codex/hooks.json` (project hooks)
- Each hook runs `doclify-capture hook <agent>`, which writes one JSON file per event (`{schema, agent, received_at_ms, worktree, branch, head, payload}`) via write-then-rename. Nothing goes to stdout, the exit status is always 0, and no model is called. A missing binary is absorbed by the shell wrapper.
- Hooks exit immediately when `DOCLIFY_BRIDGE` is set. Doclify **must** set it on every `codex exec` / `claude -p` bridge process.
- `uninstall` removes only handlers whose command starts with `: doclify-hook=1;`, plus groups/events/`hooks` it emptied.
- `transcript::read(agent, path, offset)` returns entries (User/Assistant/ToolCall/ToolResult/SessionMeta with stable ids), the next offset, and an `unsupported` list. Partial trailing lines are deferred. A file that shrank is re-read from 0; ids make the replay idempotent.

## Integrating into `doclify`

1. Add `crates/doclify-capture` to the workspace members.
2. Route `doclify hook <agent>` → `doclify_capture::hook::run`, and `doclify init` / uninstall → `install::install(&repo, "doclify")` / `install::uninstall`. Then change `install::DEFAULT_EXE` to `"doclify"` and drop `src/main.rs`.
3. Worker: `hook::pending(queue_dir)` → for each event, read `payload.transcript_path` from its stored cursor with `transcript::read` → persist entries keyed by `(agent, session_id, entry.id)` → store the cursor → delete the event file. Delete only after persisting, so a crash replays events instead of losing them. Surface `unsupported` in `/status` and `doctor`.
4. Set `DOCLIFY_BRIDGE=1` in the env of every model-bridge subprocess.

## Limitations

- Codex runs project hooks only in a trusted project and only after the user approves them in Codex's `/hooks` view (hash-based trust in `~/.codex/config.toml`). Doclify does not write trust state. Until the user approves, Codex capture is inactive, so `doctor` should report it.
- Transcript formats are undocumented. Unknown line types are reported in `unsupported`, not dropped silently. Run the opt-in check against real files after agent upgrades:
  `DOCLIFY_REAL_CODEX=<rollout.jsonl> DOCLIFY_REAL_CLAUDE=<session.jsonl> cargo test -- --ignored`
- Not yet implemented: the reconciliation scan of `~/.codex/sessions` and `~/.claude/projects` for sessions whose hooks never fired, SessionStart context injection (phase 3), and parent-session linkage. Subagent transcripts are captured only through `agent_transcript_path` in the raw payload.
