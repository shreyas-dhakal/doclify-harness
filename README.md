# Doclify

Doclify captures supported Codex and Claude Code sessions in a Git worktree, proposes documentation records for review, and generates local documentation. It is an early implementation; transcript formats and live agent integrations may change.

## Install and set up

Install Rust and Cargo, then from this checkout build a release binary:

```sh
cargo build --release -p doclify
install -m 755 target/release/doclify /usr/local/bin/doclify
cd /path/to/your/git/repository
doclify init
doclify doctor
```

Choose another directory on your PATH if `/usr/local/bin` is not writable. Keep `doclify` on PATH for agent hooks. Release builds embed the checked-in `viewer/web/dist` assets; rebuilding those assets requires Node and `npm ci --prefix viewer/web && npm run build --prefix viewer/web` before building the binary. Debug builds read the asset directory at runtime and are not standalone packages.

`init` adds hooks to `.claude/settings.local.json` and `.codex/hooks.json`, preserving other entries. Registration applies to linked worktrees; capture and context work there when agent hooks are available. Project-local hook files are installed only in the current worktree, so linked worktrees may need hooks configured separately or inherited from agent-level configuration. Codex requires a trusted project and approval in its `/hooks` view. `doclify uninstall` removes Doclify hook entries in the current worktree and retains captured data. It does not revoke Codex trust.

`doctor` reports repository registration, whether the two hook configuration files exist, model capability information, and that transcript compatibility is checked during sync. File existence does **not** prove Doclify hooks are present or working. Codex approval and model authentication are unverified; inspect the agent settings and run a capture/sync to diagnose actual operation.

## Models and workflow

Configure one provider per repository, for example:

```sh
export OPENAI_API_KEY='your-key'
doclify model set openai https://api.openai.com/v1 OPENAI_API_KEY your-model-id
```

Use `anthropic` with its versioned API base URL, or `doclify model set claude` for the constrained Claude CLI bridge. The URL must use HTTPS, except for loopback HTTP, and cannot contain credentials, query parameters, or fragments. Credentials are read from the named environment variable, not stored in the configuration. The selected provider receives the relevant capture or documentation context. Selection and CLI capability checks do not prove authentication, availability, or actual CLI enforcement. Codex inference remains disabled because a tool-free mode has not been verified. Claude CLI authentication and effective restrictions need live verification; there is no automatic fallback.

Run `doclify` for the interactive prompt. `/model` offers a searchable picker and can discover models for a configured API provider. Ordinary messages request documentation answers and may create proposed records; they do not accept decisions. Use `/review` or `doclify review` to list proposals, then `doclify review <id> <version> accept|reject|supersede` to review explicitly. `/handoff [topic]` and `doclify handoff [topic]` return bounded local context; startup hooks can supply context to agents. Records currently lack path and branch scope, so accepted constraints are treated as repository-wide.

`doclify sync` reconciles hooks and supported transcripts, collects local Git and optional read-only `gh` evidence, extracts proposals with the selected model, and generates `docs/doclify/`. Failed extraction remains pending for retry. Hooks start a background sync worker. Init also registers a per-worktree launchd agent on macOS or user systemd timer on Linux, which scans on startup and retries every minute while the user scheduler is running; check the init result and status for registration errors. Linux requires an active user systemd instance (and lingering to run after logout); model credentials must be available to the user scheduler environment. Uninstall disables capture for the current worktree and removes its scheduler entry. `doclify status` shows queues, rejected events, pending reviews, GitHub status, and background errors. Unsupported transcript formats and unauthorized hook paths require investigation; session discovery relies on recognizable metadata and cannot recover every session. Captures and queues are worktree-local; linked worktrees fall back to reviewed records in the primary worktree when they have no local context. Older shared queues are not automatically reassigned.

`doclify docs` opens the loopback viewer; keep the process running. `/docs` keeps it available until the REPL exits. Saves use revision checks; generated-page conflicts are retained under `.doclify/conflicts/` rather than overwriting overlapping edits. Raw captures and local jobs may contain sensitive session content. Review generated records and documentation before sharing them through Git.

## Validation

`cargo test --workspace --no-fail-fast` runs the Rust suite. The end-to-end tests use temporary Git repositories, synthetic agent transcripts, and a loopback fake model provider; they do not establish compatibility with authenticated live agents or providers. Run `cargo test --release -p doclify --test e2e -- release_binary_embeds_viewer_assets` to exercise the release binary outside the source tree. Viewer frontend checks require `npm ci --prefix viewer/web` and `npm test --prefix viewer/web` plus the Playwright browser dependencies.
