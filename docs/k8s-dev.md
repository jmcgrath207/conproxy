# Kubernetes dev loop (kind + Helm + Tilt)

This doc covers the local dev loop for running conproxy on a kind cluster
with backends on the host, and the e2e tests that exercise the full stack.

## Prerequisites

- `kind` (v0.24+)
- `helm` (v4+)
- `kubectl`
- `docker` (with compose)
- `tilt` (optional, for the live UI)
- Local ONNX model: `~/.conproxy/models/all-MiniLM-L6-v2/{model.onnx,tokenizer.json}`. Download from https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/onnx/model.onnx (and `tokenizer.json` from the repo root) into that directory.

## Quick start

The fastest way to get a full dev stack running is via the make targets:

```bash
# Full restart: teardown → fresh kind → backends → seed corpus → tilt up
make dev-restart
```

This tears down any existing cluster, creates a fresh one, starts host
backends (qdrant, elastic, opensearch, meilisearch ×2, pgvector), seeds
them with the synthetic corpus (`--clear`), and launches `tilt up`.

To stop everything (including backends):
```bash
make dev-down
```

To start without re-seeding (faster if backends already have data):
```bash
make dev-up
```

### Manual equivalent of `make dev-restart`

If you prefer running steps individually:

```bash
# 1. Bring up the kind cluster
./scripts/kind-up.sh

# 2. Start backends on the host
docker compose -f tests/e2e/docker-compose.yml up -d
./scripts/backends-wait.sh

# 3. Seed the corpora (3 overlapping corpora into 6 backends)
cargo run --bin corpus_seed --features embed,pgvector -- --corpus all --host http://localhost --clear

# 4. Build the conproxy image and load it into kind (Helm references conproxy:dev)
docker build -t conproxy:dev .
kind load docker-image conproxy:dev --name conproxy

# 5. Deploy conproxy via Helm (set hostIP to your kind-network gateway —
#    the bridge gateway is typically 172.18.0.1; `docker network inspect kind`
#    confirms the real value)
helm install conproxy deploy/helm/conproxy/ --set hostIP=$(docker network inspect kind --format '{{(index .IPAM.Config 0).Gateway}}')

# 6. Open the dashboard at http://127.0.0.1:10000/dashboard (requires
#    [proxy.web_ui] enabled = true, which Tilt sets automatically)
make e2e-k8s
```

Or skip steps 4–6 by running `tilt up` and clicking the `corpus-seed` and
`e2e-k8s` local resources in the Tilt UI.

### Tilt workflow

The simplest path is `make dev-restart` which runs all the above and leaves
`tilt up` in the foreground. Open http://127.0.0.1:10000/dashboard in your
browser. Trigger `corpus-seed` (or `opencode-test`) from the Tilt UI.

## DevEx auto-smoke (opencode-test + sticky session)

Tilt also manages an `opencode-test` container (host network, port `14096`)
and a `devex-smoke` local resource that drives it against the live conproxy
with **MCP-only prompts**, using a **random product / title detail** from
the generated corpus. Both are auto-init by default.

| Make target | What it does |
|-------------|--------------|
| `make devex` | Run smoke once (re-runnable; continues the sticky session for the current container) |
| `make devex-attach` | `docker exec -it opencode-test opencode -s $SID` |
| `make devex-status` | Print `DEVEX_SESSION` + last smoke result path |
| `make devex-new` | Discard the saved SID (next smoke mints a new one) |
| `make devex-banner` | Print the full handoff banner |

State on the host (cleared on `dev-restart` / container recreate):

- `.conproxy/devex-session` — current `ses_…` id (within current container)
- `.conproxy/devex-last.txt` — last smoke result
- `.conproxy/devex-export.json` — last sanitized session export

### Fresh session on every container restart

The opencode session DB lives **inside the container only** (no host bind
mount). When Tilt (or any container recreate) brings up a new
`opencode-test`, the DB is empty, the host sticky SID is cleared, and the
next smoke mints a fresh session. No multi-session cruft accumulates from
prior runs.

The sticky SID is still useful **within one container lifetime** — re-running
`make devex` (or `devex-attach`) on the same container continues the same
session, so multi-turn smoke + TUI handoff still work.

To force a fresh session without recreating the container:
`make devex-new` (or `rm .conproxy/devex-session`).

### Model credentials (no host auth-file mount)

The container does **not** read `~/.local/share/opencode/auth.json` from
the host.

**Default model is `opencode/big-pickle` — free, no API key required.**
It "just works" out of the box:

```bash
make dev-up                    # or dev-restart
make devex                     # runs the smoke with big-pickle
```

Other free models (no key required) — set `DEVEX_MODEL=…`:

| Model |
|-------|
| `opencode/big-pickle` (default) |
| `opencode/deepseek-v4-flash-free` |
| `opencode/laguna-s-2.1-free` |
| `opencode/ling-3.0-flash-free` |
| `opencode/mimo-v2.5-free` |
| `opencode/nemotron-3-ultra-free` |
| `opencode/north-mini-code-free` |

For a paid model, export a key on the host and `DEVEX_MODEL=…` (the key
is forwarded into the container by Tilt):

```bash
export OPENAI_API_KEY=sk-...   # or ANTHROPIC_API_KEY / GOOGLE_API_KEY / ...
DEVEX_MODEL=anthropic/claude-sonnet-4-5 make dev-up
make devex
```

### Disable / re-enable auto

Both `opencode-test` and `devex-smoke` auto-start on `tilt up` by default.
Toggle with env before `tilt up`:

- `DEVEX_OPENCODE_AUTO=0` — start `opencode-test` as manual
- `DEVEX_SMOKE_AUTO=0` — start `devex-smoke` as manual
- `DEVEX_OPENCODE_PORT=14096` — change the opencode serve port (host network)
- `DEVEX_MODEL=opencode/<model>` — change the smoke model (default `opencode/big-pickle`, free)

### Manual flow (auto off)

```bash
make dev-up                    # or dev-restart
# in Tilt UI: trigger opencode-test → devex-smoke
make devex-status              # read the SID
make devex-attach              # resume the TUI in the same session
```

## Components

### `scripts/kind-up.sh` / `scripts/kind-down.sh`
- `kind-up.sh` creates (or reuses) a kind cluster named `conproxy`, exports
  `HOST_IP` and `KIND_NAME`, and wires the `kind-config.yaml` for the
  conproxy pod (Tilt owns port-forwards 9999/10000; kind has no host port
  mappings). `RECREATE=1` destroys the existing cluster first.
- `kind-down.sh` deletes the cluster. Safe to re-run; no-op if missing.

### `deploy/helm/conproxy/`
- Conproxy-only chart. Backends are NOT bundled — they run on the host.
- `values.yaml` defaults: 6 backends wired (qdrant, elastic, opensearch,
  meili-1, meili-2, pgvector), context-rooted conproxy.toml, non-root pod,
  NodePort service, on-host embeddings via ONNX.
- Override `hostIP` to your machine's docker bridge IP if the default
  `172.17.0.1` is wrong.

### `tests/e2e/docker-compose.yml`
- 6 backends on the host: qdrant (6333), elasticsearch (9200), opensearch
  (9201), meilisearch (7700, 7701), postgres+pgvector (5432).

### `src/bin/corpus_seed.rs`
- 3 corpora: docs (60), tickets (50), code (50), 160 docs total.
- Overlap matrix: 10 docs per corpus (30 total) seeded to ALL 6 backends
  (tests cascade/federated). Remaining docs go to their assigned
  backends only.
- Real ONNX MiniLM embeddings (384-dim). The embedder is loaded once
  and batch-embedded.
- Parallel loads via `tokio::join!`. Per-backend: qdrant (u64 hash of id),
  ES/OpenSearch (`_bulk` NDJSON; ES gets vectors, OpenSearch doesn't),
  Meilisearch (text-only, master key from `MEILI_MASTER_KEY` env or
  default `conproxy_test_key`), pgvector (SQL `INSERT` with `CAST` on
  vector literal).

### `tests/e2e_proxy/`
- 14 categories, all use `proxy_url()` (env-overridable via `PROXY_URL`)
  and per-backend URL accessors (`qdrant_url`, `elastic_url`, etc.).
- `E2E_EXTERNAL_PROXY=1` tells the harness to skip spawning a local proxy
  and connect to whatever's at `$PROXY_URL`.
- Hard-coded localhost defaults preserved — running e2e_proxy against the
  local Tilt dev loop requires the env vars below.

### `make e2e-k8s`
- Brings up nothing itself; assumes the cluster + backends + corpus are
  already up and conproxy is reachable at `$PROXY_URL` (default
  `http://127.0.0.1:10000`).
- Writes to `tests/results/e2e-tilt/<ts>-<pid>/` and runs
  `test_runner index` to produce `index.html`.

## Environment variables

| Var | Default | Purpose |
|-----|---------|---------|
| `PROXY_URL` | `http://127.0.0.1:10000` | Conproxy HTTP endpoint (port-forwarded from kind) |
| `QDRANT_URL` | `http://localhost:6333` | Host-side qdrant |
| `ELASTIC_URL` | `http://localhost:9200` | Host-side ES |
| `OPENSEARCH_URL` | `http://localhost:9201` | Host-side OpenSearch |
| `MEILI1_URL` | `http://localhost:7700` | Host-side meili-1 |
| `MEILI2_URL` | `http://localhost:7701` | Host-side meili-2 |
| `E2E_EXTERNAL_PROXY` | (unset) | Set to `1` to skip local proxy spawn |
| `E2E_SUITE` | `all` | Filter to one e2e category |
| `E2E_OUTPUT_DIR` | (per-suite) | Override output dir |

## Tilt

The Tiltfile wires the same flow as `make e2e-k8s` but with live resource
updates. Tilt UI shows:
- `backends-up` — docker compose on the host
- `backends-wait` — health checks
- `corpus-seed` — manual, runs after backends are healthy
- `conproxy` — kind Deployment via Helm, auto-rebuilds on src change
- `e2e-k8s` — manual, runs after corpus-seed

Port-forwards `9999:9999` (gRPC) and `10000:10000` (HTTP) are wired
through Tilt's `k8s_resource` block. The read-only dashboard is at
[http://127.0.0.1:10000/dashboard](http://127.0.0.1:10000/dashboard)
(with a clickable link in the Tilt UI).

### Port strategy

Tilt owns host ports `9999` and `10000` via `k8s_resource` port-forwards.
The `deploy/tilt/kind-config.yaml` deliberately does **not** map those
ports at the kind node level. **Mixing both** (kind `extraPortMappings`
+ Tilt `port_forwards`) is the most common `tilt up` failure mode — you
get `Error port-forwarding conproxy (10000 -> 10000): address already in
use` forever, with no obvious hint that the kind container is the holder.

Rule: **Tilt owns 9999/10000; kind owns nothing on the host except 6443
(API server).**

If you need to change this, the kind cluster must be recreated — kind
cannot drop `extraPortMappings` in place:

```bash
./scripts/kind-down.sh
./scripts/kind-up.sh
```

## Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| `address already in use` on 9999/10000 in Tilt UI | kind was created with `extraPortMappings` for 9999/10000 (stale config) | `kind delete cluster --name conproxy && kind create cluster --name conproxy --config deploy/tilt/kind-config.yaml` |
| Tilt UI/API up on `:10350` but no app traffic on 9999/10000 | same as above | same as above |
| `helm template` ignores `hostIP` in upstream URLs | upstream `url` was hardcoded in `values.yaml`; configmap now templates `{{ .Values.hostIP }}` | ensure `values.yaml` upstream entries use `"http://{{ .Values.hostIP }}:PORT"` |
| pod crash-loops with embed / ORT errors | chart had `proxy.embedding.provider = "onnx"` but image has no model/ORT libs | v1: embed runs on the host via `corpus_seed`; in-cluster embed is off (drop the `[proxy.embedding]` block from values) |
| `kubectl get cm conproxy-llm-config` exists | stale deploy from before the Helm chart | `kubectl delete cm conproxy-llm-config deploy conproxy svc conproxy --ignore-not-found` |
| `make e2e-k8s` hangs spawning proxy | `E2E_EXTERNAL_PROXY=1` not set | set it in the env table below |

## Cleanup

```bash
make e2e-services-down      # stop host backends
./scripts/kind-down.sh      # destroy kind cluster
```
