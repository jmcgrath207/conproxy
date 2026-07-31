# conproxy

> Search retrieval cache for agentic RAG.

conproxy is a caching proxy for search backends. LLM caches skip re-generating answers. conproxy skips re-running the search — embed, rerank, backend — when agents ask the same thing twice.

**When to use**

- LLM agents re-querying the same corpora (retry loops, multi-agent fanout, tool-call storms)
- Multi-backend retrieval (cascading, federating, or migrating across stores)
- Hitting embed / managed-vector cost or latency under agentic load

**When *not* to use**

- Single small backend where an in-process cache suffices
- LLM-response caching (that's GPTCache or RedisVL SemanticCache territory)
- Cross-org mTLS peer replication (not planned; use a mesh sidecar)

One MCP endpoint, any backend, semantic tier with false-hit gating. Benchmarks reproducible.

```
agent ──► MCP / HTTP / gRPC ──► conproxy ──► backends
                                │  cache (exact + semantic, coalesce)
                                │  cascade (priority fallback + RRF)
                                │  federation (local-first confidence merge)
                                │  scope (lexical Jaccard + embed band)
                                 └─ metrics · audit · distill
```

Works with Elasticsearch, OpenSearch, Qdrant, pgvector, Meilisearch, Pinecone, Milvus.

[→ Benchmarks](docs/benchmarks.md) · [Python SDK + LangChain / LlamaIndex](docs/sdk-python.md) · [CONTRIBUTING](CONTRIBUTING.md)

## Features

**Agentic cache**

- In-memory cache with TTL, jitter, and background refresh; S3-FIFO eviction
- Semantic tier with τ-frontier and measured false-hit rate (≤1% gate)
- Request coalescing (singleflight) to collapse concurrent duplicates
- Negative caching for errors; serve-stale-while-refresh

**One endpoint, any backend**

- MCP server (stdio): `search`, dry-run **tune** suite, dashboard-parity **status tools** (health, overview, cache_status, pool_status, circuit_status, metrics_status, contexts_status, peer_status, tokio_status, cache_entries)
- 7 upstream adapters: Elasticsearch, OpenSearch, Qdrant, pgvector, Meilisearch, Pinecone, Milvus
- Runtime query-mode probe (TextNative vs VectorOnly) per adapter
- Context-rooted multi-tenancy (`[contexts.<id>]`): per-agent API keys, rate limits, isolated cache, scope phrases

**Tune**

- **Scope loop** — dry-run Score C filter/boost/rerank + `min_similarity` sweeps on supplied or live hits; phrase suggest; run compare/select; `benchmark` verdict (improved / degraded / unchanged)
- **What-if probes** — cache TTL hit/stale/miss, cascade leg selection, federated merge weights, embed batch shape, rate-limit allow/deny, warm-plan ETA — session-scoped, no backend write
- **Build local — ship prod** — one call: `tune_workflow` (open → search → tune → optional `apply_tune` + hot-reload); or export `contexts.<id>.scope` TOML/JSON to paste by hand

**Cascade & federation**

- **Cascade** — priority-ordered fallback chain; quality gate (`min_score_threshold`, `min_results`); optional RRF fusion for equal-priority legs
- **Federation** — local-first with confidence-gated remote fallback; configurable merge modes

**Correctness you can gate**

- `make bench-hitrate` family (exact / sem / onnx / live) with PASS / FAIL-CORE / FAIL-TRUST verdicts
- τ-frontier + false-hit rate per workload (only public frontier in the space, as far as we know)
- TTL sweep + what-if CDC model — measures stale rate vs healing options

**Resilience**

- 3-state upstream health tracking (Healthy / Degraded / Offline)
- Circuit breaker with configurable thresholds
- Exponential backoff with jitter; adaptive timeout from P99
- Connection pooling (pgbouncer-style semaphore); degradation ladder (PAUSE / RESUME)

**Export & observability**

- `conproxy distill` — dump cache to Markdown (+ optional JSON sidecar) for LLM ingestion; per-context filtering, tier selection, post-process hook
- Prometheus metrics endpoint, audit log, per-upstream + per-context stats
- TokIO runtime introspection: `GET /debug/tokio` (aggregates) and `GET /debug/tokio/dump` (task backtraces; requires `RUSTFLAGS=--cfg tokio_unstable`)
- Grafana dashboard

**Integrations**

- gRPC + HTTP APIs; MCP server for stdio clients (Claude Desktop, opencode)
- Python SDK with LangChain / LlamaIndex adapters ([docs/sdk-python.md](docs/sdk-python.md))
- systemd service management
- *Experimental:* P2P cache replication via CDC (LWW by wall timestamp; optional `shared_secret`, no mTLS — see feature flags before relying on it for production fan-out)

## Install

**Binary (cargo):**
```bash
# From a tagged release (recommended)
cargo install --git https://github.com/jmcgrath207/conproxy \
    --tag v0.1.0 --locked --features release

# From the default branch
cargo install --git https://github.com/jmcgrath207/conproxy --locked --features release

# From a local checkout
cargo install --path . --locked --features release
```

**Docker (multi-arch amd64 + arm64):**
```bash
docker pull ghcr.io/jmcgrath207/conproxy:0.1.0

# Container default: gRPC :9999, HTTP :10000 (matches Dockerfile EXPOSE
# + Helm values). Mount your conproxy.toml read-only at /etc/conproxy.
docker run -d --name conproxy -p 9999:9999 -p 10000:10000 \
  -v "$PWD/conproxy.toml:/etc/conproxy/conproxy.toml:ro" \
  ghcr.io/jmcgrath207/conproxy:0.1.0
```

**Helm (Kubernetes):**
```bash
helm install conproxy oci://ghcr.io/jmcgrath207/charts/conproxy \
  --version 0.1.0
```

**Features:** `release` = `mcp` + `persistence` + `embed-api` + `pgvector`
(ONNX + sandbox opt-in). For MCP-only: `--features mcp`. For full
options see `docs/feature-flags.md`.

## Quick Start

```bash
# Verify install
conproxy --version

# Bring up a search backend
docker run -d -p 6333:6333 qdrant/qdrant

# Start the proxy with an example config (no `init` step needed).
# The example config listens on 127.0.0.1:9090 (gRPC) — different from
# the container/Helm default of :9999/:10000 so local dev doesn't
# fight for a privileged port.
conproxy start --config examples/qdrant-quickstart.toml --daemon

# Query through the proxy
curl -s http://127.0.0.1:9090/query \
  -H 'Content-Type: application/json' \
  -d '{"query": "how to handle errors in rust", "top_k": 5}'

# Check status
conproxy status

# Stop
conproxy stop
```

## Supported Upstreams

| Backend | Type | Query Mode | Score Range | Status |
|---------|------|------------|-------------|--------|
| Elasticsearch | `elasticsearch` | `text_native` | 0–100+ (normalized to 0–1) | Shipped |
| OpenSearch | `opensearch` | `text_native` | 0–100+ (normalized to 0–1) | Shipped (ES adapter; container proof Wave 1) |
| Qdrant | `qdrant` | `text_native` | 0–1 | Shipped |
| Meilisearch | `meilisearch` | `text_native` | 0–1 (`_rankingScore`) | Shipped |
| pgvector | `pgvector` | `vector_only` | 0–1 | Shipped (`pgvector` feature) |
| Pinecone | `pinecone` | `vector_only` | 0–1 | Experimental (less e2e proof) |
| Milvus | `milvus` | `vector_only` | 0–1 | Experimental (less e2e proof) |

## Connect via MCP

The `release` build already includes the MCP server (it's a default
component of the production binary, not an add-on). Build with:

```bash
# 1. Install (release build includes MCP + persistence + embed-api + pgvector)
cargo install --path . --locked --features release

# MCP-only minimal build (smaller binary, no persistence/pgvector):
# cargo install --path . --locked --features mcp

# 2. Bring up a backend + start the proxy daemon
docker run -d -p 6333:6333 qdrant/qdrant
conproxy start --config examples/qdrant-quickstart.toml --daemon
```

**Claude Desktop** — add to `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) or `~/.config/Claude/claude_desktop_config.json` (Linux):

```json
{
  "mcpServers": {
    "conproxy": { "command": "conproxy", "args": ["mcp"] }
  }
}
```

Restart Claude Desktop.

**opencode** — add to `~/.config/opencode/opencode.jsonc`:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "conproxy": { "type": "local", "command": ["conproxy", "mcp"], "enabled": true }
  }
}
```

Restart opencode and include `use conproxy` in your prompts.

See [MCP Integration](docs/mcp-integration.md) for full configuration details.


## Export cache for LLM ingestion

Dump cached entries to structured Markdown (with optional JSON sidecars) for bootstrapping LLM knowledge.

```bash
conproxy distill --output-dir ./knowledge-base
```

See [Distill](docs/distill.md) for tier selection, stale handling, and post-process hooks.

## Minimal Configuration

Context-rooted (canonical). Cache, scope, and routing live on the context, not on a global `[proxy]`:

```toml
# .conproxy/conproxy.toml

[server]
listen = "127.0.0.1:9090"

[upstreams.qdrant]
url = "http://localhost:6333"
type = "qdrant"
timeout_secs = 30

[contexts.default]
default = true

[[contexts.default.upstreams]]
ref = "qdrant"

[contexts.default.cache]
fresh_secs = 300      # 5 min fresh
stale_secs = 600      # 10 min stale (serves while refreshing)
max_entries = 10000
```

Multi-leg cascade and federated variants: see [`examples/multi-upstream-cascade.toml`](examples/multi-upstream-cascade.toml) and [`examples/federated-search.toml`](examples/federated-search.toml).

## Documentation

| Document | Description |
|----------|-------------|
| [Tour](docs/tour.md) | Feature walkthrough with cascade/federation emphasis |
| [Quickstart](docs/quickstart.md) | 60-second install + first cached query (Docker required) |
| [Configuration](docs/configuration.md) | Full context-rooted TOML config reference |
| [Multi-Upstream](docs/multi-upstream.md) | Cascade, federation, RRF, score normalization |
| [Architecture](docs/architecture.md) | Internal design and data flow |
| [CLI Reference](docs/cli-reference.md) | All commands and flags |
| [API Reference](docs/api-reference.md) | HTTP + gRPC admin + distill endpoints |
| [MCP Integration](docs/mcp-integration.md) | Setup for Claude Desktop, opencode, and other stdio clients; tune tools |
| [Distill](docs/distill.md) | Cache export for LLM ingestion |
| [Deployment](docs/deployment.md) | Production setup and monitoring |
| [Feature Flags](docs/feature-flags.md) | Compile-time features |
| [Python SDK](docs/sdk-python.md) | Native client + LangChain/LlamaIndex adapters |

## Feature Flags

| Flag | What it enables | External deps |
|------|-----------------|---------------|
| `mcp` | MCP server (stdio transport — Claude Desktop, opencode) | rmcp, schemars |
| `embed-api` | Embedder trait + OpenAI/Cohere/HuggingFace APIs (no ONNX) | — |
| `embed` | Local ONNX embedding (implies `embed-api`) | ort, tokenizers, ndarray |
| `persistence` | Disk-backed cache (redb) | redb |
| `pgvector` | pgvector adapter | tokio-postgres |
| `linux-sandbox` | seccomp sandbox (Linux only) | caps, nix |
| `e2e` | E2E proxy tests (requires running instance) | — |
| `load-test` | Load testing infra | rlt, rand, rand_distr, hdrhistogram |
| `integration-tests` | Integration tests against real backends (Docker) | testcontainers |
| `dhat-heap` | DHAT heap profiling | dhat |
| `tokio-console` | tokio-console (async task inspector, TUI) | console-subscriber |
| `tokio-taskdump` | `Handle::dump()` task backtraces (requires `RUSTFLAGS=--cfg tokio_unstable`) | — |
| `tokio-console-snap` | `console_snap` headless dump bin (CI-friendly) | console-api |

Meta-features: `release` = `mcp` + `persistence` + `embed-api` + `pgvector` (ONNX + sandbox opt-in). `test` = `release` + `load-test` + `dhat-heap`.

See [Feature Flags](docs/feature-flags.md) for recommended combinations and build instructions.

---

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

## License

MIT
