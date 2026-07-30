#!/usr/bin/env bash
# DevEx auto-smoke — drives the running opencode-test container with
# MCP-only prompts. Picks a random product from the corpus, mints or
# continues a session, asserts no 401/UNAUTHENTICATED, prints the SID.
#
# Idempotent + safe to re-run: reuses .conproxy/devex-session when present.
#
# Env:
#   DEVEX_OPENCODE_PORT  (default 14096) — opencode serve listen port
#   DEVEX_OPENCODE_URL   (default http://127.0.0.1:$PORT) — attach URL
#   DEVEX_MODEL          (default opencode/big-pickle) — free built-in model
#                        other free options: opencode/deepseek-v4-flash-free,
#                        opencode/laguna-s-2.1-free, opencode/ling-3.0-flash-free,
#                        opencode/mimo-v2.5-free, opencode/nemotron-3-ultra-free,
#                        opencode/north-mini-code-free
#   DEVEX_TIMEOUT        (default 90) per-run seconds
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DATA_DIR="$REPO_ROOT/tests/corpus/data"

DEVEX_OPENCODE_PORT="${DEVEX_OPENCODE_PORT:-14096}"
DEVEX_OPENCODE_URL="${DEVEX_OPENCODE_URL:-http://127.0.0.1:${DEVEX_OPENCODE_PORT}}"
DEVEX_MODEL="${DEVEX_MODEL:-opencode/big-pickle}"
DEVEX_TIMEOUT="${DEVEX_TIMEOUT:-90}"

cd "$REPO_ROOT"

log() { printf '[devex-smoke] %s\n' "$*"; }
die() { printf '[devex-smoke] ERROR: %s\n' "$*" >&2; exit 1; }

# ----- 0. ensure dirs + helper -----
"$SCRIPT_DIR/devex-session.sh" ensure

LAST_RESULT="$REPO_ROOT/.conproxy/devex-last.txt"
SID_FILE="$REPO_ROOT/.conproxy/devex-session"
: > "$LAST_RESULT"
log "results → $LAST_RESULT"
log "session file → $SID_FILE"

# ----- 1. wait for proxy + opencode -----
wait_tcp() {
  local host="$1" port="$2" name="$3" tries=60
  while ! (echo > "/dev/tcp/${host}/${port}") 2>/dev/null; do
    tries=$((tries - 1))
    if [ "$tries" -le 0 ]; then
      die "$name not reachable on $host:$port after 60s"
    fi
    sleep 1
  done
  log "$name up: $host:$port"
}

# proxy HTTP /health on the Tilt port-forward
wait_tcp 127.0.0.1 10000 "conproxy HTTP"
# opencode serve
wait_tcp 127.0.0.1 "$DEVEX_OPENCODE_PORT" "opencode serve"

# ----- 2. no creds needed: default model is opencode/big-pickle (free built-in) -----
# Other free models (no key required): opencode/deepseek-v4-flash-free,
# opencode/laguna-s-2.1-free, opencode/ling-3.0-flash-free, opencode/mimo-v2.5-free,
# opencode/nemotron-3-ultra-free, opencode/north-mini-code-free
# Override with DEVEX_MODEL=opencode/<other-free> to swap.
log "model: $DEVEX_MODEL"

# ----- 3. pick a random product + detail from the corpus -----
if [ ! -f "$DATA_DIR/docs.jsonl" ]; then
  die "no corpus at $DATA_DIR (run 'make dev-restart' to seed)"
fi

pick_json=$(python3 - <<'PY'
import json, random, sys
from pathlib import Path
DATA = Path('tests/corpus/data')
random_pick = None
for corpus in ('docs', 'tickets', 'code'):
    p = DATA / f"{corpus}.jsonl"
    if not p.exists():
        continue
    rows = [json.loads(l) for l in p.read_text().splitlines() if l.strip()]
    if rows:
        random_pick = (corpus, random.choice(rows))
        break
if not random_pick:
    print("ERROR=no rows in corpus", file=sys.stderr)
    sys.exit(2)
corpus, d = random_pick
title = d.get('title', '')
content = d.get('content', '')
# pull first ~160 chars of content as detail
detail = content[:160].replace('\n', ' ').strip()
# try a query from queries.jsonl for the same corpus if present
qp = DATA / 'queries.jsonl'
sample_q = ''
if qp.exists():
    qs = [json.loads(l) for l in qp.read_text().splitlines() if l.strip() and json.loads(l).get('corpus') == corpus]
    if qs:
        sample_q = random.choice(qs).get('query', '')
out = {
    'corpus': corpus,
    'id': d.get('id', ''),
    'title': title,
    'detail': detail,
    'sample_query': sample_q,
}
print(json.dumps(out))
PY
)
if [ -z "$pick_json" ]; then die "could not pick a random corpus entry"; fi
log "random pick: $(echo "$pick_json" | python3 -c "import sys,json; d=json.loads(sys.stdin.read()); print(f\"[{d['corpus']}] {d['id']} — {d['title']}\")")"

CORPUS=$(echo "$pick_json" | python3 -c "import sys,json; print(json.loads(sys.stdin.read())['corpus'])")
PROD_TITLE=$(echo "$pick_json" | python3 -c "import sys,json; print(json.loads(sys.stdin.read())['title'])")
PROD_DETAIL=$(echo "$pick_json" | python3 -c "import sys,json; print(json.loads(sys.stdin.read())['detail'])")
SAMPLE_Q=$(echo "$pick_json" | python3 -c "import sys,json; print(json.loads(sys.stdin.read())['sample_query'])")

# Use sample query if present, else build from title
if [ -n "$SAMPLE_Q" ]; then
  QUERY="$SAMPLE_Q"
else
  QUERY="$PROD_TITLE"
fi
log "query: $QUERY"

# ----- 4. session: continue or mint -----
SID="$(cat "$SID_FILE" 2>/dev/null || true)"
MINT_NEEDED=0
if [ -n "$SID" ]; then
  # confirm session still exists in opencode
  if docker exec opencode-test opencode session list --format json 2>/dev/null | grep -q "\"$SID\""; then
    log "continuing session: $SID"
  else
    log "saved session $SID not present inside container — minting new"
    MINT_NEEDED=1
    SID=""
  fi
else
  MINT_NEEDED=1
fi

# ----- 5. shared turn runner -----
# Usage: run_turn "title" "prompt_text"
# Streams --format json events to stderr; captures them to /tmp/devex-turn.json.
run_turn() {
  local label="$1" prompt="$2"
  local out="/tmp/devex-turn-${label// /_}.json"
  local args=(run --attach "$DEVEX_OPENCODE_URL" --format json --auto --model "$DEVEX_MODEL")
  if [ -n "$SID" ]; then
    args+=( -s "$SID" )
  else
    args+=( --title "conproxy-devex" )
  fi
  # We must run inside the container (model + storage there) but the
  # smoke script itself runs on the host. Use docker exec + `timeout` so
  # a stuck model turn doesn't block the whole smoke. (opencode run has
  # no --timeout flag; only the TUI root command does.)
  if ! timeout "$DEVEX_TIMEOUT" docker exec \
      -e DEVEX_OPENCODE_URL="$DEVEX_OPENCODE_URL" \
      -e DEVEX_TIMEOUT="$DEVEX_TIMEOUT" \
      opencode-test opencode "${args[@]}" "$prompt" \
      > "$out" 2>/tmp/devex-turn.err; then
    local rc=$?
    log "turn '$label' failed (rc=$rc; see $out + /tmp/devex-turn.err)"
    cat /tmp/devex-turn.err | tail -20 >&2
    return 1
  fi
  cat "$out"
  return 0
}

# ----- 6. turns -----
TURN_LOG=/tmp/devex-turns.jsonl
: > "$TURN_LOG"

# Turn 1 — establish the session (always mints if no SID yet)
if [ "$MINT_NEEDED" -eq 1 ]; then
  log "turn 1: open session + status call"
  T1_OUT="$(run_turn "status" "Use the conproxy MCP tools only. Call health and overview and report the status. Do not run any other tools.")" || die "turn 1 failed"
  # Extract session id from the json stream: first event with .sessionID
  NEW_SID="$(printf '%s\n' "$T1_OUT" | python3 -c "
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        d = json.loads(line)
    except Exception:
        continue
    sid = d.get('sessionID') or d.get('session_id')
    if not sid and isinstance(d.get('session'), dict):
        sid = d['session'].get('id')
    if sid:
        print(sid)
        break
")"
  if [ -z "$NEW_SID" ]; then
    log "could not extract session id from turn 1; tail of stream:"
    printf '%s' "$T1_OUT" | tail -5 >&2
    die "no session id in turn 1 output"
  fi
  SID="$NEW_SID"
  "$SCRIPT_DIR/devex-session.sh" set "$SID" >/dev/null
  log "minted session: $SID"
fi
# T1_OUT only set when MINT_NEEDED=1; for continue mode we skip that path.
# Initialize here for `set -u` safety + the log append.
: "${T1_OUT:=}"
printf '%s\n' "$T1_OUT" >> "$TURN_LOG" || true

# Helper: assert no auth errors in a turn output
assert_no_auth() {
  local out="$1" label="$2"
  if printf '%s' "$out" | grep -Eq 'API key required|UNAUTHENTICATED|Invalid API key'; then
    log "FAIL: auth error in '$label' — proxy rejected our request"
    return 1
  fi
  return 0
}

# Turn 2 — search using the random product
log "turn 2: search random product via MCP"
T2_PROMPT="Use the conproxy MCP tools only. Call search with query=\"${QUERY}\" and limit=5. Also call cache_status. Report the top hit ids and the cache hit rate. Do not call any other tools."
T2_OUT="$(run_turn "search" "$T2_PROMPT")" || die "turn 2 failed"
assert_no_auth "$T2_OUT" "search" || die "auth error in search turn"
printf '%s\n' "$T2_OUT" >> "$TURN_LOG"

# Turn 3 — full tune_workflow dry-run
log "turn 3: tune_workflow dry-run"
T3_PROMPT="Use the conproxy MCP tools only. Call tune_workflow with agent_id=\"devex-test\", context_id=\"default\", query=\"${QUERY}\", top_k=5, apply=false, close_session=true. Report whether the workflow returned hits and the mode/primary_kept summary. Do not call any other tools."
T3_OUT="$(run_turn "tune_workflow" "$T3_PROMPT")" || die "turn 3 failed"
assert_no_auth "$T3_OUT" "tune_workflow" || die "auth error in tune_workflow turn"
printf '%s\n' "$T3_OUT" >> "$TURN_LOG"

# Turn 4 — cache entries listing (final status)
log "turn 4: cache_entries"
T4_PROMPT="Use the conproxy MCP tools only. Call cache_entries. Summarize the entry count and any freshness buckets you see. Do not call any other tools."
T4_OUT="$(run_turn "cache_entries" "$T4_PROMPT")" || die "turn 4 failed"
assert_no_auth "$T4_OUT" "cache_entries" || die "auth error in cache_entries turn"
printf '%s\n' "$T4_OUT" >> "$TURN_LOG"

# ----- 7. persist + export -----
log "all 4 turns OK against session $SID"
echo "session=$SID pick=[$CORPUS] query=$QUERY turns=4 result=ok" >> "$LAST_RESULT"

# Optional: export the session as JSON for later debugging
EXPORT_FILE="$REPO_ROOT/.conproxy/devex-export.json"
if docker exec opencode-test opencode export "$SID" --sanitize > "$EXPORT_FILE" 2>/dev/null; then
  log "exported session → $EXPORT_FILE"
fi

# ----- 8. banner -----
"$SCRIPT_DIR/devex-session.sh" banner
log "smoke complete"
