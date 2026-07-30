#!/usr/bin/env bash
# Full dev stack restart: tear down → fresh kind → backends → seed → tilt up.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

cd "$REPO_ROOT"

echo "=== dev-restart: full stack restart ==="

# 0. Ensure DevEx dirs (sticky SID lives on host; cleared on container recreate)
./scripts/devex-session.sh ensure

# 1. Full teardown
./scripts/dev-down.sh

# 2. Fresh kind cluster
echo "  Creating fresh kind cluster..."
RECREATE=1 ./scripts/kind-up.sh

# 3. Backends on host
echo "  Starting host backends..."
docker compose -f tests/e2e/docker-compose.yml up -d

# 4. Wait for backends
./scripts/backends-wait.sh

# 5. Seed corpus (always — user decision)
echo "  Seeding corpus (--clear)..."
cargo run --bin corpus_seed --features embed,pgvector -- \
  --corpus all \
  --corpus-dir tests/corpus/data/ \
  --clear \
  --host http://localhost

# 6. Show corpus summary (product names, topics, sample queries)
./scripts/corpus-summary.sh

# 7. Print DevEx session status (Tilt will clear + remint when opencode-test recreates)
./scripts/devex-session.sh status
echo "  (Tilt resource 'devex-smoke' will run after opencode-test + corpus-seed)"

# 8. Tilt up (foreground)
echo "  Starting tilt (Ctrl+C to stop)..."
exec tilt up
