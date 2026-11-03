# doclify-context

`build(worktree, &Scope, &Limits) -> io::Result<Option<Context>>` turns `.doclify/records` into bounded agent context. It reads local files only: no model call, no writes, and `None` when there are no records or nothing to say. `Context` serializes to JSON (constraints, open_work, omitted, unreadable, scope); `render()` gives the Markdown injected into agents.

- **Accepted constraints**: the accepted version of requirements and decisions, with rationale, evidence ids, implementation state, and a note when a later unreviewed change is pending.
- **Open work** (labeled as not settled): unfinished accepted requirements/decisions, accepted questions/tasks, active experiments, and unreviewed proposals (title only, since proposal text is unreviewed model output).
- **Excluded**: rejected proposals and abandoned experiments, everywhere.
- **Scope**: records have no path/branch fields yet, so every accepted constraint is repository-wide. Topic and branch words only order records by relevance; they never hide one. At startup there is no topic, and the text says so instead of guessing the task.
- **Limits**: item counts, per-field length (newlines flattened), and a total byte budget. Items that do not fit are listed by id as pointers; unreadable histories (e.g. merge conflicts) are reported, not dropped.

`build_merged(&[worktree, …], …)` builds one context over several worktrees' records. A record id held by more than one takes the copy with the highest latest version, ties going to the earlier worktree; if any copy is unreadable the id is reported unreadable.

The capture hook calls this on every `SessionStart` (startup, resume, clear, compact) and prints `hookSpecificOutput.additionalContext`. For `/handoff [topic]`, call `build(&repo.worktree, &Scope { branch: repo.branch, topic }, &Limits::default())` and print the `Context` JSON or `render()`.
