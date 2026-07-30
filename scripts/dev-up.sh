#!/usr/bin/env bash
# Start the dev stack: kind → tilt up (foreground).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

cd "$REPO_ROOT"

echo "=== dev-up: starting dev stack ==="

# 0. Ensure DevEx dir exists (sticky SID lives on host; cleared on container recreate)
./scripts/devex-session.sh ensure

# 1. Kind cluster (idempotent; reuse if exists)
./scripts/kind-up.sh

# 2. Show corpus summary (so you know what to query)
./scripts/corpus-summary.sh

# 3. Print any saved DevEx SID (Tilt will clear it on first opencode-test recreate)
./scripts/devex-session.sh status

# 4. Tilt up (foreground)
echo "  Starting tilt (Ctrl+C to stop)..."
echo "  (Tilt resource 'devex-smoke' will run after opencode-test + corpus-seed)"
exec tilt up
