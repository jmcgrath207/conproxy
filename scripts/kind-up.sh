#!/usr/bin/env bash
# Bring up a kind cluster for conproxy dev + e2e work.
#
# Backends (qdrant, elastic, opensearch, meilisearch ×2, pgvector/postgres)
# run as plain host docker containers via `make e2e-services-up` — NOT inside
# kind. The kind cluster hosts ONLY the conproxy pod, which reaches host
# backends via host.docker.internal.
#
# Idempotent: if cluster already exists, skip create (use --recreate to force).
# Exports HOST_IP and KIND_NAME so the helm chart / Tiltfile can pick them up.
set -euo pipefail

CLUSTER_NAME="${KIND_NAME:-conproxy}"
CONFIG="${KIND_CONFIG:-$(dirname "$0")/../deploy/tilt/kind-config.yaml}"
RECREATE="${RECREATE:-0}"

# Detect the docker bridge gateway IP so the conproxy pod can reach host
# backends. On Docker Desktop (Mac/Windows) host.docker.internal works; on
# Linux kind, the container's gateway IP routes to the host.
detect_host_ip() {
  if command -v docker >/dev/null 2>&1; then
    # Try the default bridge network gateway first; fall back to the kind
    # network gateway if it already exists.
    local ip
    ip=$(docker network inspect bridge --format '{{(index .IPAM.Config 0).Gateway}}' 2>/dev/null || true)
    if [ -z "$ip" ] || [ "$ip" = "<no value>" ]; then
      ip=$(docker network inspect kind --format '{{(index .IPAM.Config 0).Gateway}}' 2>/dev/null || true)
    fi
    if [ -n "$ip" ] && [ "$ip" != "<no value>" ]; then
      echo "$ip"
      return 0
    fi
  fi
  # Fallback: guess 172.17.0.1 (default bridge gateway on Linux Docker)
  echo "172.17.0.1"
}

export HOST_IP
HOST_IP="$(detect_host_ip)"
export KIND_NAME="$CLUSTER_NAME"

if kind get clusters 2>/dev/null | grep -qx "$CLUSTER_NAME"; then
  if [ "$RECREATE" = "1" ]; then
    echo "kind: cluster '$CLUSTER_NAME' exists; recreating (RECREATE=1)"
    kind delete cluster --name "$CLUSTER_NAME"
  else
    echo "kind: cluster '$CLUSTER_NAME' already exists; skipping create"
    echo "  (export KUBECONFIG if needed; defaults to ~/.kube/config)"
    echo "  Set RECREATE=1 to force recreate."
    echo "  HOST_IP=$HOST_IP  KIND_NAME=$CLUSTER_NAME"
    exit 0
  fi
fi

echo "kind: creating cluster '$CLUSTER_NAME' with $CONFIG"
kind create cluster --name "$CLUSTER_NAME" --config "$CONFIG"

# Apply /etc/hosts hint so tools can resolve the cluster API server
# (kind already adds this via $KUBECONFIG context).
echo
echo "kind: cluster up."
echo "  kubeconfig context: kind-$CLUSTER_NAME"
echo "  HOST_IP=$HOST_IP  (use this in conproxy chart values for host backend URLs)"
echo "  Next: make backends-up && make helm-install"
echo "  Tear down: scripts/kind-down.sh"
