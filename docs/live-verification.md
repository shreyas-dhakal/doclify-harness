# Live verification: guest-checkout handover (2026-11-03)

`scripts/live-guest-checkout.sh` runs the plan's acceptance scenario against authenticated, installed agents: codex-cli 0.166.0 (model gpt-6.1-terra) and Claude Code 2.1.308, on macOS. It uses the existing logins, copies no credentials, and works in a fresh `mktemp` repository. Codex project trust and hook approval are passed as per-invocation flags and never persisted. Extraction uses `doclify model set claude`, the constrained Claude CLI bridge. Each run costs real model usage.

## Result

The final run (`PASS`) showed:

1. A real `codex exec` session ran Doclify's project hooks (SessionStart, UserPromptSubmit, Stop), as the invocation log records.
2. `doclify sync` extracted 13 proposals through the live Claude bridge, with 0 pending and 0 rejected. Proposals included `decision: Allow guest checkout` and `requirement: Customers can check out without creating an account…`, each with the rationale "Customers abandon account creation." Receipt emails were proposed as open tasks.
3. The script explicitly accepted the requirements and decisions and left tasks unreviewed.
4. A real `claude -p` session on branch `auth-login` received the context through its SessionStart hook. Asked to quote that context, it listed the accepted guest-checkout constraints with rationale and `event:` evidence, then the receipt-email work under "Open work (not settled constraints)".
5. A second sync captured the Claude session's transcript (2 jobs).

Before it passed, the live runs found the following. Synthetic fixtures had not caught any of it.

## Fixed

- **Repeated prompt in every job.** Every transcript job embedded the triggering hook event as `context`, including the user's `prompt` (and Stop's `last_assistant_message`). The live model therefore re-proposed the user's request once for every tool call and tool result in the turn: 44 proposals came from one session. Jobs now carry only hook metadata (`crates/doclify-capture/src/queue.rs`). The e2e fake provider now reads the whole job, as a real model does. Before the fix, the guest-checkout e2e test failed with the duplication.
- **Claude Code transcripts rejected.** Claude Code 2.1.308 transcripts start with `queue-operation` lines that have no `cwd`. Both the hook ownership check and session discovery read only the first line, so every Claude hook was rejected as "does not belong to repository". Ownership now comes from the first line that carries a `cwd` (`transcript_owner` in `crates/doclify-pipeline/src/lib.rs`). A regression test covers this.

## Observed, not fixed

- **Codex hook trust.** Codex runs project `.codex/hooks.json` only with project trust *and* hook approval. Unattended, this needs `-c 'projects={"<repo>"={trust_level="trusted"}}'` (the dotted form `projects."<path>".trust_level` was silently ignored) plus `--dangerously-bypass-hook-trust`. Without both flags no Doclify hook ran, and `doctor` cannot detect that.
- **Early Claude hooks rejected.** Claude's SessionStart and UserPromptSubmit hooks fire before the transcript has a `cwd` line, so they are still rejected (2 per session). The Stop hook and discovery capture the full session, so no transcript content is lost, but `status.rejected_events` reports these as problems. Fixing this means deferring unattributable hooks to discovery instead of rejecting them.
- **Overlapping proposals.** Extraction runs per transcript item, so tool calls, tool results and the final answer restate the same facts as separate proposals (13 in the final run, several near-duplicates). Each item is also a separate CLI call, so one short session took about 90 s to sync. Session- or turn-level batching would address both.
- **Quoted context fed back in.** The Claude session's reply quoted the Doclify context, and that reply was captured and sent for extraction. Accepted constraints can therefore come back as new proposals.
- **Stale sessions picked up.** Sessions are attributed by `cwd`. Old Codex sessions from a deleted directory were imported into a new repository created at the same path.
- **Usage limit failures.** When the account hit its usage limit, the bridge failed with only "CLI extraction failed". Work stayed pending and a later `sync` completed it. The error does not say what went wrong.
