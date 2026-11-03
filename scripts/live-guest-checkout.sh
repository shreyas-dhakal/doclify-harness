#!/bin/sh
# Opt-in live smoke test of the guest-checkout handover: a real Codex session
# establishes the requirement, the Claude CLI bridge extracts proposals, the
# script reviews them explicitly, and a real Claude Code session on another
# branch is asked what it was told at startup.
#
# Uses the caller's existing Codex and Claude Code logins; it copies no
# credentials and writes only inside a fresh mktemp directory (plus the
# session logs both agents keep under ~/.codex and ~/.claude). Codex project
# trust and hook approval are supplied as per-invocation flags, never
# persisted. Costs real model usage.
#
#   cargo build -p doclify && scripts/live-guest-checkout.sh
set -eu

DOCLIFY_BIN=${DOCLIFY_BIN:-$(cd "$(dirname "$0")/.." && pwd)/target/debug/doclify}
[ -x "$DOCLIFY_BIN" ] || { echo "build doclify first: $DOCLIFY_BIN" >&2; exit 2; }
for c in codex claude timeout python3; do
  command -v $c >/dev/null || { echo "$c must be on PATH" >&2; exit 2; }
done

W=$(cd "$(mktemp -d "${TMPDIR:-/tmp}/doclify-live.XXXXXX")" && pwd -P)
OUT=$W/out
mkdir -p "$W/bin" "$OUT"
# Shim logs every invocation so hook delivery is observed, not assumed.
cat > "$W/bin/doclify" <<EOF
#!/bin/sh
echo "\$(date +%T) \$*" >> "$OUT/invocations.log"
exec "$DOCLIFY_BIN" "\$@"
EOF
chmod +x "$W/bin/doclify"
# Nested agents must not think they run inside the caller's agent session.
for v in $(env | grep -E '^(CLAUDECODE|CLAUDE_CODE_[A-Z_]*|CLAUDE_PID|CLAUDE_EFFORT|CODEX_THREAD_ID|CODEX_SANDBOX[A-Z_]*)=' | cut -d= -f1); do
  unset "$v"
done
export PATH="$W/bin:$PATH"
json() { s=$1; shift; python3 -c "import json,sys; d=json.load(sys.stdin); $s" "$@"; }
fail() { echo "FAIL: $*" >&2; echo "artifacts: $OUT" >&2; exit 1; }

R=$W/shop
git init -q -b main "$R" && cd "$R"
printf 'fn checkout() {}\n' > checkout.rs
git add . && git -c user.name=live -c user.email=live@example.invalid commit -qm "Initial checkout"
doclify init > "$OUT/init.json"
doclify model set claude > "$OUT/model.json"
doclify doctor > "$OUT/doctor.json"
codex --version > "$OUT/versions.txt"; claude --version >> "$OUT/versions.txt"

echo "== Codex session"
timeout 600 codex exec --dangerously-bypass-hook-trust -s workspace-write \
  -c "projects={\"$R\"={trust_level=\"trusted\"}}" \
  "We will allow guest checkout because customers abandon account creation. Requirement: customers can check out without creating an account; offer an optional account after purchase. Add a minimal guest_checkout(email) function to checkout.rs. Do not implement receipt emails for guests yet; that remains unfinished follow-up work. Keep the change tiny." \
  < /dev/null > "$OUT/codex.out" 2> "$OUT/codex.err" || fail "codex exec exited $?"
n=$(grep -c ' hook codex$' "$OUT/invocations.log" || true)
[ "$n" -ge 3 ] || fail "expected Codex SessionStart/UserPromptSubmit/Stop hooks, saw $n"

echo "== Sync (Claude CLI bridge extraction)"
doclify sync > "$OUT/sync1.json" 2> "$OUT/sync1.err" || fail "sync: $(cat "$OUT/sync1.err")"
doclify status > "$OUT/status1.json"
json 'assert d["pending_events"]==0 and d["rejected_events"]==0 and d["pending_extraction"]==0, d' < "$OUT/status1.json" \
  || fail "capture or extraction incomplete: $(cat "$OUT/status1.json")"
doclify review > "$OUT/proposals.json"

echo "== Explicit review: accept requirements/decisions, leave tasks open"
json '[print(r["id"], r["version"], r["kind"]) for r in d["pending_review"]]' < "$OUT/proposals.json" |
  while read -r id version kind; do
    case $kind in requirement|decision) doclify review "$id" "$version" accept >> "$OUT/reviews.json" ;; esac
  done
[ -s "$OUT/reviews.json" ] || fail "no requirement or decision proposed: $(cat "$OUT/proposals.json")"
doclify handoff > "$OUT/handoff.json"

echo "== Claude Code session on another branch"
git checkout -q -b auth-login
# Prompt on stdin: the variadic --tools would swallow a positional prompt.
echo "We are starting work on authentication and login. Before anything else, quote verbatim every accepted constraint and every open work item that Doclify supplied to you as session-start context, including any rationale. If you received no Doclify context, reply exactly NO_DOCLIFY_CONTEXT." |
  timeout 600 claude -p --output-format json --tools "" > "$OUT/claude.json" 2> "$OUT/claude.err" || fail "claude -p exited $?"
grep -q ' hook claude$' "$OUT/invocations.log" || fail "Claude Code never ran the Doclify hook"
json 'print(d["result"]); print(d["session_id"], file=sys.stderr)' < "$OUT/claude.json" > "$OUT/claude-reply.txt" 2> "$OUT/claude-session.txt"
grep -qi 'guest' "$OUT/claude-reply.txt" || fail "Claude reply lacks the guest-checkout constraint: $(cat "$OUT/claude-reply.txt")"

echo "== Re-sync captures the Claude session"
doclify sync > "$OUT/sync2.json" 2> "$OUT/sync2.err" || fail "second sync: $(cat "$OUT/sync2.err")"
doclify status > "$OUT/status2.json"
json 'assert d["pending_extraction"]==0, d' < "$OUT/status2.json" || fail "Claude extraction pending"
grep -l '"agent":"claude"' .git/doclify/jobs/done/* > "$OUT/claude-jobs.txt" || fail "Claude transcript never captured"
# Known limitation: Claude's SessionStart/UserPromptSubmit hooks can fire before
# its transcript records a cwd; they are rejected while Stop captures the session.
json 'print("claude transcript jobs:", sys.argv[1], "rejected hook events:", d["rejected_events"])' "$(wc -l < "$OUT/claude-jobs.txt")" < "$OUT/status2.json"

echo "PASS; artifacts in $OUT"
cat "$OUT/claude-reply.txt"
