# Doclify: Rust documentation harness

Status: planning complete; implementation has not started.
Last updated: 2026-11-03.

## Purpose and success

Build a local-first documentation product for engineering teams using several coding agents. Doclify automatically captures Codex and Claude Code activity, connects it to Git and GitHub evidence, and maintains documentation that separates suggestions, accepted decisions, implementation, and abandoned experiments.

Users interact through a Rust REPL. `/model` selects the documentation model; `/docs` opens a localhost MDX viewer and WYSIWYG editor. External agents continue doing the coding. Doclify manages documentation rather than executing coding tasks.

The motivating acceptance scenario:

1. A user tells a coding agent to allow guest checkout because customers abandon account creation.
2. The agent explores alternatives, implements part of an approach, and leaves unfinished work.
3. Doclify captures the requirement, reason, proposed decision, evidence, experiments, and unresolved questions.
4. A user explicitly reviews and accepts the decision in Doclify.
5. A later authentication session in another agent receives the accepted constraint, rationale, evidence, and unresolved work before making changes.

The paid outcome to validate is less repeated explanation, fewer reversals of earlier decisions, and faster handovers. Capture volume and polished summaries alone do not establish value.

## Confirmed product decisions

- Rust application with an interactive documentation REPL.
- Automatic capture through session adapters and standard agent hooks; launching agents through Doclify is optional, not required.
- Codex and Claude Code supported in the first usable release.
- Installed Codex and Claude Code provide documentation inference using their existing logins through CLI bridges. Hooks handle capture separately.
- Additional models connect through API keys.
- `/model` presents an interactive model picker.
- Explicit review in Doclify is required before extracted decisions become accepted.
- Automatic startup context supplies accepted decisions and clearly labeled open work to external agents; a manual handoff command is also available.
- Standard documentation includes project overview, architecture, setup, feature requirements, decisions, evidence, and open work.
- Localhost MDX viewer and WYSIWYG editor use React and TypeScript alongside Rust.
- GitHub integration is read-only evidence collection in v1.

## Application and interfaces

Use a Cargo workspace with a shared domain library, capture adapters, model bridges, and one `doclify` executable. Proposed implementation defaults are Tokio, Clap, Ratatui/Crossterm, Axum, and SQLite.

Build the browser application with React, TypeScript, Vite, and MDXEditor. Embed production assets into the Rust binary so end users do not need Node.

Support macOS and Linux initially, one registered Git repository per workspace, including its worktrees. Share reviewed records and documentation through Git. These platform and packaging choices are implementation defaults, not separately requested product requirements.

Starting `doclify` opens the REPL. Provide matching noninteractive commands with machine-readable JSON output.

| Interface | Behavior |
| --- | --- |
| `doclify init` | Register the repository, configure capture and model access, and install additive agent hooks |
| `/model` | Open a searchable picker of configured documentation models and installed agent bridges |
| `/status` | Show capture health, pending extraction, review items, and integration failures |
| `/sync` | Reconcile captured sessions, Git history, and connected GitHub evidence |
| `/review` | Inspect proposed changes and explicitly accept, reject, or supersede records |
| `/docs` | Start the local viewer/editor and open the browser |
| `/handoff [topic]` | Generate cited context for another agent |
| `/help`, `/quit` | Command help and REPL exit |
| `doclify doctor` | Diagnose versions, hooks, authentication, and capture compatibility |

Ordinary REPL messages support documentation questions and proposed documentation edits. They do not authorize code changes or decision acceptance.

## Capture, evidence, and trust

### Capture pipeline

- Install repository-scoped Codex and Claude Code hooks while preserving existing configuration. Provide an uninstall command that removes only Doclify's entries.
- Capture lifecycle and tool events, then incrementally read associated transcripts through versioned adapters. Supplement hooks with reconciliation scans.
- Hooks enqueue local events quickly; a background worker performs extraction. Capture works while the REPL and browser are closed.
- Restrict collection to registered repositories. Preserve source events locally, deduplicate replay, recover after interruption, and report incomplete or unsupported formats.
- Tag Doclify-generated model sessions and exclude them from capture to prevent recursive documentation.
- Associate events with repository, worktree, branch, revision, agent, session, and parent session where available.
- Agent hooks must remain responsive and must not wait for an LLM request. Capture or context failures must not prevent coding-agent startup or work.

### Durable records

Maintain requirements, decisions, experiments, questions, and tasks with stable identifiers, rationale, provenance, and evidence links. Keep the following dimensions separate:

| Dimension | States |
| --- | --- |
| Review | proposed, accepted, rejected, superseded |
| Implementation | unknown, planned, partial, implemented, reverted |
| Experiment outcome | active, completed, abandoned |

A model may propose these facts; only an explicit Doclify review records acceptance. Later changes to an accepted claim create a new proposal. Preserve the earlier accepted version until reviewed. An implemented experiment is not automatically an accepted decision, and an accepted decision may remain unimplemented.

Distinguish observed test execution from an agent's claim that tests passed. Link test results to the revision or captured working-tree state; mark older evidence stale after relevant changes. Record the scope of verification without claiming broader coverage.

### Git and GitHub

- Read local commits, diffs, branches, and file history.
- Use the authenticated `gh` CLI for read-only PR, review, discussion, and merge evidence. Local functionality remains available without GitHub.
- Treat inferred session-to-commit associations as suggestions unless directly supported.
- Never infer acceptance of a requirement from a commit or merged PR alone.
- Do not create commits, branches, comments, or PRs in v1.

## Models and documentation

### Model connections

- Bridge installed Codex through noninteractive execution and Claude Code through print mode, using their existing authentication without reading or copying credentials.
- Run documentation requests with structured output and constrained tool access. Doclify validates results and owns all documentation writes.
- Support additional models through API-key-backed OpenAI-compatible endpoints and Anthropic's API.
- Populate `/model` from detected bridges, configured model identifiers, and provider discovery where supported. Show authentication and availability failures without silently changing providers or implying that an unverified model is available.
- Keep credentials outside the repository. Provider calls may transmit selected documentation and source excerpts; show the selected provider during setup.
- Make extraction incremental and checkpointed. Network, quota, or authentication failures preserve captured work for retry.
- Validate each CLI bridge's ability to constrain execution before enabling it. Existing agent authentication must not require copying tokens into Doclify.

### Storage and generated documentation

- Store portable structured records under `.doclify/records/` and documentation under `docs/doclify/`.
- Keep raw captures, queues, and the rebuildable SQLite search index in ignored local storage. Raw transcripts and credentials must not enter generated tracked documentation by default.
- Generate a standard template covering overview, architecture, setup, feature requirements, decisions and rationale, implementation evidence, and open work.
- Separate repository-observed facts from human intent. Missing knowledge stays explicitly unknown.
- Link documentation claims to record identifiers and source evidence. Preserve a local source excerpt when the original transcript is unavailable on another teammate's machine.
- Share documentation and reviewed records through the user's normal Git workflow.
- Preserve human edits and reviewed state when models are switched or records are regenerated.

## Local viewer and editor

- Provide documentation navigation and search, a central reading/editing pane, an evidence panel, and a review queue.
- Show review and implementation states separately. Open questions and abandoned experiments remain discoverable without appearing as instructions.
- Use a constrained MDX component set for requirements, decisions, evidence, and questions. Do not execute arbitrary JSX or imports.
- Support rich-text and source editing. Preserve unsupported MDX blocks verbatim.
- Save prose edits independently of decision acceptance. Regeneration proposes changes against the previous generated version; overlapping manual edits become reviewable conflicts.
- Use revision-checked saves to prevent one tab overwriting another.
- Bind the server to loopback and protect write endpoints with a per-launch session token and origin checks.
- Keep status and provenance legible, use keyboard-accessible controls, and make capture errors, missing credentials, empty documentation, and save conflicts actionable.

## Automatic handovers

Startup and supported resume/compaction hooks deliver bounded context from accepted, applicable records and clearly labeled open work. Exclude rejected proposals and abandoned experiments from directive context.

Use repository-wide constraints plus branch- and path-relevant records, with references to fuller documentation. When task or path scope is unavailable at startup, supply repository-wide constraints and pointers to narrower records rather than guessing the task.

Context delivery uses locally available data, requires no model call, and does not block coding-agent startup when Doclify is unavailable. An absent or unreviewed record must not be presented as a settled constraint.

## Delivery sequence

1. Domain records, durable storage, migrations, and the guest-checkout fixture.
2. Both capture adapters, hook installation, background processing, and recovery.
3. Model bridges, `/model`, extraction, review, and cited handovers.
4. Git/GitHub evidence and standard documentation generation.
5. Embedded viewer/editor, conflict handling, and packaging.

Build complete vertical slices in this order. Validate the cross-agent handover before expanding provider breadth or adding publishing features.

## Verification and acceptance

- A suggestion never becomes accepted automatically.
- Approved but unfinished work differs from implemented but unapproved experiments.
- Supersession and reversion retain history.
- Replay, concurrent sessions, worktrees, partial transcripts, and restarts do not lose or duplicate records.
- Unsupported transcript formats are visible rather than silently treated as complete capture.
- Existing hooks survive installation and removal; Doclify sessions do not recapture themselves.
- Model failures preserve pending work; provider switches do not erase review history.
- Manual MDX edits survive regeneration and conflicting saves.
- Source links, keyboard navigation, editor round trips, and localhost write protections work.
- A Codex session establishes the guest-checkout requirement, a user accepts it, and a later Claude Code authentication session receives it with its rationale and evidence.

Use Rust tests for domain and capture behavior, provider fixtures for deterministic extraction tests, and browser tests for editing and review. Keep authenticated live smoke tests opt-in. Test the packaged executable with embedded assets and no frontend development server.

Measure capture coverage, review corrections, handover delivery, and user-reported repeated explanation locally. Do not add hosted telemetry, billing, or collaboration services in v1.

## Exploration evidence and limitations

At planning time the repository was empty except for `.git`. Available local tools included Rust/Cargo, Node, Codex CLI 0.166.0, and Claude Code 2.1.308. These observations establish development feasibility, not permanent compatibility guarantees.

- [Official Codex hook documentation](https://learn.chatgpt.com/docs/hooks) describes lifecycle events, transcript paths, and startup context. It explicitly warns that transcript format is not a stable hook interface.
- [Claude Code hook documentation](https://code.claude.com/docs/en/hooks) describes lifecycle hooks and context delivery.
- [Claude Code programmatic execution](https://code.claude.com/docs/en/headless) describes print-mode execution.
- Local CLI help confirmed noninteractive execution and structured-output options for both agents. Authenticated inference, capture correctness, and hook installation have not yet been tested.

## Resume here

No product code, dependencies, hook configuration, or credentials have been created or changed. This plan is the saved handoff; implementation remains pending.

On resuming, read this file, check repository instructions and current tool versions, and convert the first delivery phase into actionable implementation tasks. Preserve the confirmed product decisions above; clearly label any new assumptions. Verify bridge constraints and hook/transcript compatibility against the installed versions before relying on them. Do not start by building a standalone coding agent or treating all captured session text as permanent instructions.
