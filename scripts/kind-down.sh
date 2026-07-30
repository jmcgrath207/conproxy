#!/usr/bin/env bash
# Tear down the kind cluster created by kind-up.sh.
# Does NOT touch host docker backend containers — use `make e2e-services-down`
# for those.
set -euo pipefail

CLUSTER_NAME="${KIND_NAME:-conproxy}"

if ! kind get clusters 2>/dev/null | grep -qx "$CLUSTER_NAME"; then
  echo "kind: cluster '$CLUSTER_NAME' not present; nothing to do"
  exit 0
fi

echo "kind: deleting cluster '$CLUSTER_NAME'"
kind delete cluster --name "$CLUSTER_NAME"
unset HOST_IP KIND_NAME
