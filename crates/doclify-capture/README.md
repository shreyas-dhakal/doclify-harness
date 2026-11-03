# doclify-capture

Phase 2 capture slice: agent hooks → local queue, versioned transcript readers, additive hook install/uninstall, and SessionStart context delivery via `doclify-context`.

CLI versions inspected during development: Codex 0.166.0 and Claude Code 2.1.308 (2026-11-03). Live authenticated capture compatibility has not been established.

## How it works

- `doclify-capture install` (inside a repo) creates `<git-dir>/doclify/queue/` (this marks that worktree as registered and is never tracked) and adds hooks for `SessionStart`, `UserPromptSubmit`, `Stop`, `SubagentStop` to:
  - Claude: `.claude/settings.local.json` (personal, not committed)
  - Codex: `.codex/hooks.json` (project hooks)
- Each hook runs `doclify-capture hook <agent>`, which writes one JSON file per event (`{schema, agent, received_at_ms, worktree, branch, head, payload}`) via write-then-rename. On `SessionStart` (startup, resume, clear, compact) the hook also prints `{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":…}}` built from `.doclify/records` by `doclify-context`; every other event prints nothing. The exit status is always 0, and no model is called. A missing binary is absorbed by the shell wrapper.
- Hooks exit immediately when `DOCLIFY_BRIDGE` is set. Doclify **must** set it on every `codex exec` / `claude -p` bridge process.
- `uninstall` removes only handlers whose command starts with `: doclify-hook=1;`, plus groups/events/`hooks` it emptied.
- `transcript::read(agent, path, offset)` returns entries (User/Assistant/ToolCall/ToolResult/SessionMeta with stable ids), the next offset, and an `unsupported` list. Partial trailing lines are deferred. A file that shrank is re-read from 0; ids make the replay idempotent.

## CLI integration

`doclify init`, `doclify uninstall`, and `doclify hook <agent>` use this library. Hooks enqueue events and the CLI starts a background sync worker. The pipeline reconciles recognized sessions under the configured agent roots and validates hook transcript ownership before import. Bridge subprocesses use capture-suppression markers.

## Limitations

- Codex runs project hooks only in a trusted project and only after the user approves them in Codex's `/hooks` view (hash-based trust in `~/.codex/config.toml`). Doclify does not write trust state. Until the user approves, Codex capture is inactive. `doctor` reports approval as unverified.
- Transcript formats are undocumented. Unknown line types are reported in `unsupported`, not dropped silently. Run the opt-in check against real files after agent upgrades:
  `DOCLIFY_REAL_CODEX=<rollout.jsonl> DOCLIFY_REAL_CLAUDE=<session.jsonl> cargo test -- --ignored`
- Reconciliation scans supported session roots but requires recognizable session metadata; parent-session linkage is not implemented. Subagent transcript paths may appear in raw payloads.

Linked worktrees have separate hook queues, jobs, checkpoints, and extraction outputs under their own Git directories; run `doclify init` in each worktree. Older shared queues and jobs remain in the common Git directory and are not automatically reassigned to linked worktrees; inspect them before manually recovering any pending work. Rejected hook files are retained in the worktree queue’s `rejected` directory and counted by `status` and `sync`.
