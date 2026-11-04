# Chat first Doclify TUI Implementation Plan

> **For agentic workers:** Follow this plan task by task. Keep the workflow snapshot and terminal UI in separate files with the interfaces below. The coordinator owns entry-point integration and the user-visible design decisions.

**Goal:** Replace Doclify's line prompt with a chat-first Ratatui interface that makes documentation progress and next actions clear.

**Architecture:** Keep direct command handling in `main.rs` unchanged for JSON output. Add a TUI module for terminal lifecycle, chat state, and drawing, plus a read-only workflow snapshot module that uses existing local capture, record, model, and docs data. The TUI calls existing handlers for requests and slash commands and renders their results as readable conversation messages.

**Tech Stack:** Rust 2021, Ratatui, Crossterm, existing Doclify domain and capture crates.

**Spec:** `docs/superpowers/specs/2026-11-04-chat-first-tui-design.md`

## Global Constraints

- Starting `doclify` opens the TUI. `doclify <command>` continues to run a direct, script-friendly command and print JSON.
- Free text in the chat bar requests a documentation proposal. The TUI shows the answer and a readable summary of proposed records; they remain proposed until explicitly reviewed.
- Slash commands retain their current meanings: `/sync`, `/review`, `/docs`, `/status`, `/handoff`, `/model`, `/help`, and `/quit`.
- A compact workflow strip stays visible: **Capture → Propose → Review → Docs**.
- Counts and status come from existing local data; unavailable information is shown as unknown rather than guessed.
- Do not auto-accept proposals or write generated documentation without the existing review path.
- Keep capture hooks and background-worker output quiet and machine-readable.
- Use Ratatui with a terminal event/input backend compatible with the project's supported platforms.

## Review Focus

- **Model not selected:** show how to choose a model and keep the TUI usable for slash commands.
- **Slow model or sync operation:** keep the screen responsive and show a working state while the action runs.
- **Terminal resize or very small terminal:** preserve the input and workflow indicators; avoid panics or corrupt output.
- **Exit through `/quit`, Ctrl-C, EOF, or an error:** restore the terminal to its original mode.
- **Direct command invocation:** preserve JSON output and avoid entering the TUI for `doclify status`, `doclify sync`, and other commands.

---

## File Map

- Modify `crates/doclify/Cargo.toml`: add Ratatui and the Crossterm backend.
- Modify `Cargo.lock`: lock the chosen compatible dependency versions.
- Modify `crates/doclify/src/main.rs`: declare the TUI module and route only the no-argument path to it; retain direct command handlers and JSON behavior. Add a viewer-start helper that returns its URL for TUI use while keeping the existing direct `docs` behavior.
- Create `crates/doclify/src/tui/workflow.rs`: read-only status projection used by the persistent workflow strip.
- Create `crates/doclify/src/tui/app.rs`: input, transcript, scroll position, loading state, and action/result handling.
- Create `crates/doclify/src/tui/ui.rs`: draw header, conversation, workflow strip, input bar, and help/model overlays.
- Create `crates/doclify/src/tui/terminal.rs`: terminal setup and guaranteed restoration.
- Create `crates/doclify/src/tui/mod.rs`: event loop, command dispatch, background action channel, and public `run()` entry point.
- Modify `README.md`: document the chat-first interaction and workflow controls.

## Task 1: Scaffold the TUI contract (coordinator worktree)

**Files:**
- Modify `crates/doclify/Cargo.toml` and `Cargo.lock`
- Modify `crates/doclify/src/main.rs`
- Create `crates/doclify/src/tui/mod.rs`
- Create `crates/doclify/src/tui/workflow.rs`

**Interfaces:**
- Consumes `doclify_capture::RepoInfo`, `doclify_capture::queue::Queue`, `doclify_domain::Store`, and the existing model configuration file.
- Produces the shared `workflow` contract for parallel implementation:

```rust
pub struct WorkflowSnapshot {
    pub repository: String,
    pub branch: Option<String>,
    // Provider name when selected; configured API model ID when applicable.
    pub model: Option<String>,
    pub registration: RegistrationState,
    pub pending_capture_events: Option<usize>,
    pub pending_extraction: Option<usize>,
    pub pending_review: Option<usize>,
    pub documentation_files: Option<usize>,
    pub background_error: Option<String>,
    pub recovery_error: Option<String>,
}

pub enum RegistrationState { Registered, NotRegistered }

pub fn load(repo: &RepoInfo) -> io::Result<WorkflowSnapshot>
```

- `documentation_files` counts `.md` and `.mdx` files below `docs/doclify`; a missing directory means zero.
- Count the latest version of each record whose review state is `Proposed`.
- Treat absent worker/recovery error files and an absent model config as `None`; propagate malformed configuration and other I/O errors.

- [ ] Add Ratatui and Crossterm dependencies and declare `mod tui;` in `main.rs`.
- [ ] Add a minimal `tui::run() -> ExitCode` placeholder and route only the no-argument path to it; keep hook and direct-command paths unchanged.
- [ ] Add the `WorkflowSnapshot`, `RegistrationState`, and `load(&RepoInfo)` interface above in `workflow.rs`; initialize unavailable counts to `None` until Task 2 fills them in.
- [ ] Run `cargo check -p doclify` to confirm the shared contract compiles.
- [ ] Commit the scaffold so the workflow and UI tasks can start from the same interface.

## Task 2: Build the workflow snapshot (Orca worktree)

**Files:**
- Modify `crates/doclify/src/tui/workflow.rs`

**Interfaces:**
- Consumes the contract scaffolded in Task 1.
- Produces a read-only `load(repo: &RepoInfo) -> io::Result<WorkflowSnapshot>` implementation.

- [ ] Implement the snapshot from existing local sources without writing or accepting records.
- [ ] Use `.md` and `.mdx` files below `docs/doclify`; a missing directory means zero.
- [ ] Run `cargo check -p doclify` and report the changed file and check result to the coordinator.
- [ ] Commit the isolated workflow snapshot and report the commit to the coordinator.

## Task 3: Implement the Ratatui chat shell (coordinator worktree)

**Files:**
- Modify `crates/doclify/src/tui/mod.rs`
- Create `crates/doclify/src/tui/app.rs`
- Create `crates/doclify/src/tui/ui.rs`
- Create `crates/doclify/src/tui/terminal.rs`

**Interfaces:**
- Consumes `tui::workflow::{load, WorkflowSnapshot}` from Task 2.
- Produces `pub fn tui::run() -> ExitCode`.
- `TerminalSession::enter()` enables raw mode and the alternate screen; its `Drop` restores both.
- App state contains a single-line input, scrollable transcript, current workflow snapshot, action-in-progress state, and optional overlay.

- [ ] Replace the placeholder `tui::run()` with terminal setup, event handling, redraw, and workflow refresh.
- [ ] Render the selected A layout: repository/model header, scrollable chat, persistent Capture → Propose → Review → Docs strip, and bottom chat bar with `/` and keyboard hints.
- [ ] Add keyboard editing and navigation: character input, Backspace, Delete, Left/Right, Home/End, Enter, Up/Down history scrolling, and Ctrl-C exit.
- [ ] Poll terminal events without blocking redraws; recompute layout after resize and handle small terminal dimensions without panicking.
- [ ] Keep slow actions off the render loop using a worker thread and result channel; show a working state while one runs.
- [ ] Build with `cargo check -p doclify` and inspect the screen at normal and narrow terminal sizes.

## Task 4: Wire chat requests and workflow actions

**Files:**
- Modify `crates/doclify/src/main.rs`
- Modify `crates/doclify/src/tui/app.rs` and `crates/doclify/src/tui/mod.rs`

**Interfaces:**
- `main.rs` declares `mod tui;` and routes `args.is_empty()` to `tui::run()`.
- The TUI reuses the existing `run`, `documentation_request`, and model configuration operations; it does not duplicate domain logic.
- Slash parsing keeps the existing command names and argument behavior.

- [ ] Route only the no-argument invocation to the TUI; leave hook, background-worker, and direct command paths unchanged.
- [ ] Send free text to `documentation_request`, then append its answer and proposed-record summaries to the transcript; display errors as readable messages.
- [ ] Wire `/sync`, `/review`, `/status`, and `/handoff` to existing handlers and format their returned data in the transcript.
- [ ] Implement `/model` as a keyboard-selectable picker that preserves the existing available choices and setup guidance; selecting a choice uses the existing model configuration path.
- [ ] Implement `/help` and `/quit`; unknown commands show a concise hint.
- [ ] Route `/docs` through a viewer-start helper that returns the URL to the transcript without printing JSON into the alternate screen.
- [ ] Keep direct invocations such as `doclify status` on the JSON path.
- [ ] Refresh the workflow snapshot after each completed action and on a short timer; surface worker errors without hiding the chat.
- [ ] Run `cargo check -p doclify` and manually walk through model selection, a proposal request, a sync, review, `/docs`, terminal resize, and clean exit.

## Task 5: Document the workflow and polish the first-run state

**Files:**
- Modify `README.md`
- Modify `crates/doclify/src/tui/ui.rs` as needed

- [ ] Show a first-run welcome message when no model is selected, including `/model` and a brief explanation that chat requests create proposals for review.
- [ ] Make the workflow strip distinguish pending work, ready stages, and unknown values without inventing counts.
- [ ] Document free-text requests, slash commands, keyboard shortcuts, and the fact that proposals require review.
- [ ] Run `cargo check -p doclify` and review the README against the implemented commands.

## Sequencing and collaboration

After Task 1 is committed, Task 2 (workflow data) and Task 3 (terminal shell) can run in separate Orca worktrees against the shared interface. The coordinator owns `main.rs`, crate dependency integration, and final integration. Task 4 follows both workstreams; Task 5 follows the integrated UI. Keep each worktree's edits within its file ownership above, and integrate through the coordinator's branch.
