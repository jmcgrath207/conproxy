#!/usr/bin/env bash
#
# Run e2e tests against an external conproxy + backends (the k8s/Tilt path).
#
# Required env:
#   PROXY_URL          (e.g. http://127.0.0.1:10000)
#   QDRANT_URL         (e.g. http://localhost:6333)
#   ELASTIC_URL        (e.g. http://localhost:9200)
#   OPENSEARCH_URL     (e.g. http://localhost:9201)
#   MEILI1_URL         (e.g. http://localhost:7700)
#   MEILI2_URL         (e.g. http://localhost:7701)
#   PGVECTOR_URL       (e.g. postgres://postgres:postgres@localhost:5432/conproxy_test)
#   E2E_EXTERNAL_PROXY=1
#
# Optional env:
#   E2E_OUTPUT_DIR     (default tests/results/e2e-tilt/<ts>)
#   E2E_SUITE          (default all)
#   E2E_FILTER         (test name filter, default empty)
#
# Writes: <output_dir>/{log,summary.json,index.html,...} via test_runner index.

set -euo pipefail

: "${PROXY_URL:?PROXY_URL required}"
: "${QDRANT_URL:?QDRANT_URL required}"
: "${ELASTIC_URL:?ELASTIC_URL required}"
: "${OPENSEARCH_URL:?OPENSEARCH_URL required}"
: "${MEILI1_URL:?MEILI1_URL required}"
: "${MEILI2_URL:?MEILI2_URL required}"
: "${PGVECTOR_URL:?PGVECTOR_URL required}"
: "${E2E_EXTERNAL_PROXY:?E2E_EXTERNAL_PROXY required (set to 1)}"

TS=$(date +%Y%m%d-%H%M%S)
PID=$$
OUT_DIR="${E2E_OUTPUT_DIR:-tests/results/e2e-tilt/${TS}-${PID}}"
mkdir -p "$OUT_DIR"

LOG="$OUT_DIR/e2e.log"
RESULTS_DIR="$OUT_DIR/test_results"
mkdir -p "$RESULTS_DIR"

# --features e2e is required (the e2e_proxy test target is feature-gated).
# -- --ignored runs the two `#[ignore]`d external-proxy tests in the suite.
# --test-threads=1: many e2e tests share the proxy on the same port; concurrent
#   execution flakes. The two ignored tests are the ones that exercise the
#   k8s-mode externality contract (manage-own-proxy categories skip themselves
#   when external_proxy() is true).
E2E_OUTPUT_DIR="$RESULTS_DIR" \
E2E_SUITE="${E2E_SUITE:-all}" \
E2E_FILTER="${E2E_FILTER:-}" \
  cargo test --features e2e --test e2e_proxy -- --ignored --test-threads=1 2>&1 | tee "$LOG"

# test_runner index to render index.html
cargo run --bin test_runner -- index "$OUT_DIR" >/dev/null 2>&1 || \
  echo "warning: test_runner index failed (see log); results in $OUT_DIR"

echo
echo "=== e2e-k8s complete ==="
echo "  results: $OUT_DIR"
echo "  index:   $OUT_DIR/index.html"
echo "  log:     $LOG"
