# Doclify evidence and documentation

Read-only evidence sync and standard documentation generation (delivery phase 4).

- `sync(repo, local_dir, &SyncOptions)` reads local branches, commits (with numstat) across `--branches`, and, if `gh` is installed and authenticated, PRs with review states and comment counts via `gh pr list` only. A missing, unauthenticated, or failing `gh` is recorded as `Github::Unavailable { reason }`; local evidence still syncs. `SyncOptions { gh: None, .. }` skips GitHub. The snapshot is written to `local_dir/evidence.json`, which is rebuildable and must be in ignored local storage.
- `file_history` and `diff` read on demand; patches are never copied into tracked docs.
- Links connect records to commits and PRs. Only evidence a record cites (`commit:<sha prefix of 7 or more characters>`, `pr:<n>`) is `Direct`; whole-token mentions of a record id in commit messages or PR title, body, or branch are `Suggested`. Nothing here changes review state, so a commit or merged PR never accepts a record.
- `docs::generate(repo, Option<&Evidence>)` writes `docs/doclify/{overview,architecture,setup,requirements,decisions,evidence,open-work}.mdx`. Accepted versions, pending proposals, and implementation state are shown separately, with record citations (`id@vN`), provenance sources and excerpts, and evidence links. Purpose, design intent, and setup steps stay explicitly `Unknown` until a reviewed record documents them; repository-observed facts are labeled as such. Abandoned experiments are listed as history, not instructions. All record text is MDX-escaped. Output is deterministic.
- Human edits are kept. The last generated copy of each page is stored in `.doclify/generated/`. A page that differs from that copy is never overwritten: if regeneration changed, the new version goes to `.doclify/conflicts/<page>.mdx` and is reported in `Report::conflicts`. `docs::resolve_conflict(repo, page)` marks it reviewed by making it the new baseline. A page that existed before Doclify first generated it counts as a human edit. Commit `.doclify/generated/` and `.doclify/conflicts/` with the docs so teammates share the baseline.

Limits: merging is per page, not per section. Each sync rescans up to `max_commits` (default 500) with no checkpoint. `gh` has no timeout. Session-to-commit association and test-run evidence and staleness are not implemented.

## Integration

1. Add `"crates/doclify-evidence"` to `members` in the root `Cargo.toml` and run `cargo test -p doclify-evidence`. The crate needs no new dependencies outside the lockfile.
2. In `crates/doclify`, add `doclify-evidence = { path = "../doclify-evidence" }` and wire `/sync` (`doclify sync`) as:
   ```rust
   let local = repo.common_dir.join("doclify"); // shared by worktrees, never tracked
   let evidence = doclify_evidence::sync(&repo.worktree, &local, &Default::default())?;
   let report = doclify_evidence::docs::generate(&repo.worktree, Some(&evidence))?;
   Ok(json!({"commits": evidence.commits.len(), "github": evidence.github, "links": evidence.links, "docs": report}))
   ```
   Report `Github::Unavailable` in `/status` as an integration failure, not as an error.
