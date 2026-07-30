#!/usr/bin/env bash
#
# Wait for the e2e backends to be reachable on the host.
# Loops with backoff. Exits 0 when all healthy, 1 on timeout.
#
# Used by Tilt's `backends-wait` local_resource.

set -euo pipefail

# Endpoints: name, URL (single-space separated; bash `read` splits on IFS).
endpoints=(
  "qdrant http://localhost:6333/healthz"
  "qdrant-root http://localhost:6333/"
  "elastic http://localhost:9200/_cluster/health"
  "opensearch http://localhost:9201/_cluster/health"
  "meili-1 http://localhost:7700/health"
  "meili-2 http://localhost:7701/health"
  "pgvector tcp://localhost:5432"
)

TIMEOUT_SECS="${BACKENDS_WAIT_TIMEOUT:-180}"
INTERVAL_SECS="${BACKENDS_WAIT_INTERVAL:-2}"

# Elasticsearch/opensearch must be yellow+ (not just HTTP 200)
check_es_healthy() {
  local url="$1"
  local resp
  resp=$(curl --silent --max-time 2 "$url" 2>/dev/null) || return 1
  echo "$resp" | grep -qE '"status":"(yellow|green)"' 2>/dev/null
}

start_ts=$(date +%s)
declare -A ok=()
total=${#endpoints[@]}

echo "Waiting for $total backend endpoint(s) (timeout=${TIMEOUT_SECS}s)..."

while true; do
  now=$(date +%s)
  elapsed=$(( now - start_ts ))
  if (( elapsed > TIMEOUT_SECS )); then
    echo "TIMEOUT after ${elapsed}s. Still failing:"
    for i in "${!endpoints[@]}"; do
      line="${endpoints[$i]}"
      read -r name _ <<<"$line"
      if [[ -z "${ok[$name]:-}" ]]; then
        echo "  - $name"
      fi
    done
    exit 1
  fi

  ready=0
  for i in "${!endpoints[@]}"; do
    line="${endpoints[$i]}"
    read -r name url <<<"$line"
    if [[ -n "${ok[$name]:-}" ]]; then
      ready=$((ready + 1))
      continue
    fi
    if [[ "$url" == tcp://* ]]; then
      hostport="${url#tcp://}"
      host="${hostport%:*}"
      port="${hostport#*:}"
      # TCP connect-only check (do not read — pg doesn't reply to raw cat).
      if timeout 2 bash -c "</dev/tcp/${host}/${port}" 2>&1; then
        ok[$name]=1
        ready=$((ready + 1))
        echo "  ✓ $name ($url) [${elapsed}s]"
      fi
    else
      # Elasticsearch/opensearch must be yellow+ (disk watermark etc.)
      if [[ "$name" == "elastic" || "$name" == "opensearch" ]]; then
        if check_es_healthy "$url"; then
          ok[$name]=1
          ready=$((ready + 1))
          echo "  ✓ $name ($url) [${elapsed}s]"
        fi
      elif curl --silent --fail --max-time 2 "$url" >/dev/null 2>&1; then
        ok[$name]=1
        ready=$((ready + 1))
        echo "  ✓ $name ($url) [${elapsed}s]"
      fi
    fi
  done

  if (( ready == total )); then
    echo "All $total backends healthy (${elapsed}s)."
    exit 0
  fi

  sleep "$INTERVAL_SECS"
done
