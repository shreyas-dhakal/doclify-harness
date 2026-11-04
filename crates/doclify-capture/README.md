# doclify-capture

Phase 2 capture slice: agent hooks → local queue, versioned transcript readers, additive hook install/uninstall, and SessionStart context delivery via `doclify-context`.

CLI versions inspected during development: Codex 0.166.0 and Claude Code 2.1.308 (2026-11-03). Live authenticated capture compatibility has not been established.

## How it works

- `doclify-capture install` (inside a repo) creates `<git-dir>/doclify/queue/` (this marks that worktree as registered and is never tracked) and adds hooks for `SessionStart`, `UserPromptSubmit`, `Stop`, `SubagentStop`, `PostToolUse`, `PostToolUseFailure` to:
  - Claude: `.claude/settings.local.json` (personal, not committed)
  - Codex: `.codex/hooks.json` (project hooks)
- Each hook runs `doclify-capture hook <agent>`, which writes one JSON file per event (`{schema, agent, received_at_ms, worktree, branch, head, worktree_dirty, payload}`) via write-then-rename. On `SessionStart` (startup, resume, clear, compact) the hook also prints `{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":…}}` built from `.doclify/records` by `doclify-context` (merged with primary worktree records for linked worktrees); every other event prints nothing. The exit status is always 0, and no model is called. A missing binary is absorbed by the shell wrapper.

- A hook whose transcript is missing or has not yet named its `cwd` (Claude's SessionStart/UserPromptSubmit fire before that line exists) is written to `queue/deferred/` instead of being rejected for unverifiable ownership. Every later hook releases deferred files, in their original order, once the transcript attributes (or after 24 h, so a never-attributed hook is still visibly rejected). `hook::release_deferred` exposes this for callers.
- Hooks exit immediately when `DOCLIFY_BRIDGE` is set. Doclify **must** set it on every `codex exec` / `claude -p` bridge process.
- `install` also reconciles the repository's other worktrees: any worktree lacking Doclify handlers gets the command already installed elsewhere. `SessionStart` repeats this silently, so worktrees added later are covered once any session starts in the repository. Bare/missing worktrees, unparseable files, and files that already contain Doclify handlers are left alone.
- `install` validates both config files before writing anything: unparseable JSON, a non-object `hooks`, or a non-array event entry fails with the file named, and nothing changes. If a later write or registration step fails, files already written are restored, and directories that the install created, registration included, are removed.
- `uninstall` removes only handlers whose command starts with `: doclify-hook=1;`, plus groups/events/`hooks` it emptied, and writes `<git-dir>/doclify/hooks-uninstalled` so reconciliation skips that worktree until `install` runs there again.
- `transcript::read(agent, path, offset)` returns entries (User/Assistant/ToolCall/ToolResult/SessionMeta with stable ids), the next offset, and an `unsupported` list (with byte offsets). Partial trailing lines are deferred. A file that shrank is re-read from 0; ids make the replay idempotent.
- Unsupported lines don't block the valid entries around them. Those entries are queued. The skipped lines go to `jobs/unsupported/<key>` (`Queue::unsupported()`), together with an actionable reason, the adapter format, and the original event. `Queue::replay_unsupported(roots)` re-reads those sessions from the stored event, even if discovery can no longer find them. A report written by another adapter version is re-read from offset 0 automatically on the next reconcile.
- Transcript jobs carry `context.payload.parent_session_id` for child sessions. The value comes from an explicit hook payload, a Codex `session_meta` `source.subagent.thread_spawn.parent_thread_id` (kept in the checkpoint for later reads), or a Claude sidechain line (`isSidechain` + `agentId`; the job's `session_id` becomes the agent id).
- Doclify's injected startup context, and agent text quoting it (`[id@vN]` citations and their Statement/Rationale/Evidence/Pending lines), is stripped from user/assistant jobs (`doclify_context::strip_quoted`). Entries that held nothing else are dropped.
- A transcript last modified more than an hour before the event worktree's `.git` was created is skipped. It belongs to a deleted repository whose path was reused. This check is skipped on filesystems without creation times.

## CLI integration

`doclify init`, `doclify uninstall`, and `doclify hook <agent>` use this library. Hooks enqueue events and the CLI starts a background sync worker. The pipeline reconciles recognized sessions under the configured agent roots and validates hook transcript ownership before import. Bridge subprocesses use capture-suppression markers.

## Limitations

- Codex runs project hooks only in a trusted project and only after the user approves them in Codex's `/hooks` view (hash-based trust in `~/.codex/config.toml`). Doclify does not write trust state. Until the user approves, Codex capture is inactive. `doctor` reports approval as unverified.
- Transcript formats are undocumented. Unknown line types are reported in `unsupported`, not dropped silently. Run the opt-in check against real files after agent upgrades:
  `DOCLIFY_REAL_CODEX=<rollout.jsonl> DOCLIFY_REAL_CLAUDE=<session.jsonl> cargo test -- --ignored`
- Reconciliation scans supported session roots but requires recognizable session metadata; parent-session IDs are derived as described above. Subagent transcript paths may appear in raw payloads.

Linked worktrees have separate hook queues, jobs, checkpoints, and extraction outputs under their own Git directories. A worktree created after `init` has no hooks until a session starts in another registered worktree or `init` runs there; a session started in it before then is not captured by hooks. Older shared queues and jobs remain in the common Git directory and are not automatically reassigned to linked worktrees; inspect them before manually recovering any pending work. Rejected hook files are retained in the worktree queue’s `rejected` directory and counted by `status` and `sync`.
