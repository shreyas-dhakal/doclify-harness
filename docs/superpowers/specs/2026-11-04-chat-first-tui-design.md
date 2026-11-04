# Chat first Doclify TUI design

## Intent

Make Doclify feel like a modern coding-agent CLI: a persistent chat input, readable conversation output, and a clear view of documentation moving from captured sessions to proposals, review, and published docs.

## Goals

- Replace the line-based no-argument prompt with a Ratatui full-screen terminal interface.
- Keep free-text input as a documentation request that produces proposed records.
- Keep slash commands as explicit workflow actions, with readable results rather than raw JSON in the TUI.
- Show the current repository, selected model/setup state, workflow stages, pending review count, and the next useful action using existing local data.
- Make the interface usable with keyboard navigation, scrolling history, and terminal resizing.
- Build and review the interface in small visible stages with the user.

## Non-goals

- Changing record generation, acceptance, or document-writing rules.
- Replacing the existing direct CLI commands or their JSON output; scripts keep working.
- Adding cloud sync, remote collaboration, or a new model provider.
- Copying the full behavior of Codex, OpenCode, or Claude Code.

## Approaches

- **Ratatui full-screen TUI (selected):** supports a persistent chat bar, panes, keyboard navigation, scrolling, and resize handling. It adds a terminal UI dependency and an event/render loop, which best fits the requested experience.
- **Lightweight ANSI interface:** avoids a larger UI dependency, but makes text editing, layout changes, and navigation harder to keep reliable.
- **Keep the existing line prompt and add styling:** smallest change, but cannot keep workflow context visible while the user chats.

## Interaction model

- Starting `doclify` opens the TUI. `doclify <command>` continues to run a direct, script-friendly command and print JSON.
- Free text in the chat bar requests a documentation proposal. The TUI shows the answer and a readable summary of proposed records; they remain proposed until explicitly reviewed.
- Slash commands retain their current meanings: `/sync`, `/review`, `/docs`, `/status`, `/handoff`, `/model`, `/help`, and `/quit`. Unknown commands show a concise hint.
- The header identifies the repository and branch and reports the model state. When no model is selected, the TUI explains how to choose one before accepting a documentation request.
- A compact workflow strip stays visible: **Capture → Propose → Review → Docs**. Counts and status come from the existing queue, records, and sync data; unavailable information is shown as unknown rather than guessed.
- The conversation area presents sync outcomes, proposal summaries, review actions, and errors in plain language. Detailed content remains reachable by scrolling or the existing `/docs` viewer.

## Constraints and safety

- Reuse existing command and domain operations where possible; do not duplicate record or sync rules in the view layer.
- Do not auto-accept proposals or write generated documentation without the existing review path.
- Keep the TUI isolated from capture hooks and background-worker output so agent hooks remain quiet and machine-readable.
- Use Ratatui with a terminal event/input backend compatible with the project's supported platforms.

## Implementation left to the builder

- Choose the Ratatui event/input crates and establish terminal restore behavior for normal exit, errors, and Ctrl-C.
- Define a small view-state/read-model boundary so workflow counts and statuses can be refreshed without blocking the input loop.
- Implement the shell first, then wire existing commands into it and refine the screen with the user between stages.
- Use Orca worktrees for independent implementation tasks only after the plan defines clean ownership boundaries; keep integration and the user-visible design decisions with the coordinator.
