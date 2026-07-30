#!/usr/bin/env bash
# DevEx session helpers — opencode session id for smoke + human handoff.
#
# The opencode session DB is in-container only (no host bind mount), so the
# sticky SID on the host is cleared on every container recreate. The SID file
# is still useful for multi-turn smoke + attach *within one container lifetime*.
#
# Usage:
#   scripts/devex-session.sh ensure          # create .conproxy/ if missing
#   scripts/devex-session.sh get             # echo SID (or empty)
#   scripts/devex-session.sh set <sid>       # write SID file
#   scripts/devex-session.sh clear           # remove SID file (next smoke mints new)
#   scripts/devex-session.sh path            # echo path to SID file
#   scripts/devex-session.sh attach-cmd      # print "docker exec -it opencode-test ..."
#   scripts/devex-session.sh banner          # print full dev-up/dev-restart banner
#   scripts/devex-session.sh status          # short status: SID + last smoke result path
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

CONPROXY_DIR="$REPO_ROOT/.conproxy"
SID_FILE="$CONPROXY_DIR/devex-session"
LAST_RESULT="$CONPROXY_DIR/devex-last.txt"

ensure() {
  mkdir -p "$CONPROXY_DIR"
}

get() {
  if [ -f "$SID_FILE" ]; then
    cat "$SID_FILE"
  fi
}

set_sid() {
  local sid="${1:-}"
  if [ -z "$sid" ]; then
    echo "devex-session: set requires a non-empty sid" >&2
    return 2
  fi
  ensure
  printf '%s\n' "$sid" > "$SID_FILE"
  echo "devex-session: wrote $sid → $SID_FILE"
}

clear_sid() {
  if [ -f "$SID_FILE" ]; then
    rm -f "$SID_FILE"
    echo "devex-session: cleared $SID_FILE (next smoke will mint a new session)"
  else
    echo "devex-session: nothing to clear"
  fi
}

path() {
  printf '%s\n' "$SID_FILE"
}

attach_cmd() {
  local sid
  sid="$(get || true)"
  if [ -z "$sid" ]; then
    echo "no DEVEX_SESSION yet — run 'make devex' or wait for Tilt 'devex-smoke' to mint one"
    return 0
  fi
  printf 'docker exec -it opencode-test opencode -s %q\n' "$sid"
}

banner() {
  local sid last
  sid="$(get || true)"
  last="(no smoke result recorded yet)"
  [ -f "$LAST_RESULT" ] && last="$(head -1 "$LAST_RESULT" 2>/dev/null || echo "$last")"
  cat <<EOF
════════════════════════════════════════
 DEVEX_SESSION=${sid:-<not yet minted — run: make devex>}
 Last smoke:  $last
 Resume TUI:  make devex-attach
 Or directly: $(attach_cmd)
 Attach web:  opencode attach http://127.0.0.1:14096
════════════════════════════════════════
EOF
}

status() {
  local sid
  sid="$(get || true)"
  if [ -z "$sid" ]; then
    echo "DEVEX_SESSION: <none>"
  else
    echo "DEVEX_SESSION: $sid"
  fi
  echo "SID file: $SID_FILE"
  if [ -f "$LAST_RESULT" ]; then
    echo "Last smoke: $LAST_RESULT"
    sed -n '1,5p' "$LAST_RESULT" 2>/dev/null || true
  else
    echo "Last smoke: (none)"
  fi
}

cmd="${1:-}"
shift || true
case "$cmd" in
  ensure)     ensure "$@" ;;
  get)        get "$@" ;;
  set)        set_sid "$@" ;;
  clear)      clear_sid "$@" ;;
  path)       path "$@" ;;
  attach-cmd) attach_cmd "$@" ;;
  banner)     banner "$@" ;;
  status)     status "$@" ;;
  ""|-h|--help|help)
    sed -n '2,15p' "$0"
    ;;
  *)
    echo "devex-session: unknown command: $cmd" >&2
    exit 2
    ;;
esac
