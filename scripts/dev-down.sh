#!/usr/bin/env bash
# Tear down the full dev stack: tilt → ports → kind → backends.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

cd "$REPO_ROOT"

echo "=== dev-down: tearing down dev stack ==="

# 1. Tilt down (ignore if not running)
if command -v tilt >/dev/null 2>&1; then
  echo "  Stopping tilt..."
  tilt down 2>/dev/null || true
fi

# 2. Free proxy ports
for port in 9999 10000; do
  if ss -ltnp "sport = :$port" 2>/dev/null | grep -q .; then
    echo "  Freeing port $port..."
    fuser -k "${port}/tcp" 2>/dev/null || true
  fi
done

# 3. Delete kind cluster
CLUSTER_NAME="${KIND_NAME:-conproxy}"
if kind get clusters 2>/dev/null | grep -qx "$CLUSTER_NAME"; then
  echo "  Deleting kind cluster '$CLUSTER_NAME'..."
  kind delete cluster --name "$CLUSTER_NAME"
fi

# 4. Tear down host backends (with volumes for clean state)
echo "  Stopping host backends..."
docker compose -f tests/e2e/docker-compose.yml down -v 2>/dev/null || true

echo "=== dev-down: done ==="
