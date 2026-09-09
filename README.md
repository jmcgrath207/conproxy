# conproxy

[![CI](https://github.com/jmcgrath207/conproxy/actions/workflows/ci.yml/badge.svg)](https://github.com/jmcgrath207/conproxy/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/conproxy.svg)](https://crates.io/crates/conproxy)
[![PyPI](https://img.shields.io/pypi/v/conproxy.svg)](https://pypi.org/project/conproxy/)
[![GHCR](https://img.shields.io/badge/GHCR-conproxy-black?logo=github)](https://github.com/jmcgrath207/conproxy/pkgs/container/conproxy)
[![Helm](https://img.shields.io/badge/Helm-OCI-0F1689?logo=helm)](https://github.com/jmcgrath207/conproxy/pkgs/container/charts%2Fconproxy)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

> Retrieval cache for agentic RAG — lower cost, faster search.

Agents re-query. You pay embed + vector search again. conproxy caches the retrieval leg — hits skip embed and upstream, so agentic loops (retries, fanout, tool-call storms) stop paying twice.

One MCP / HTTP / gRPC endpoint. Any backend. Measured hit rates, not vibes.

**Proof** — ~89.5% exact hit rate on an 8 000-query agentic trace; hit p50 ~0.1 ms vs miss ~13.8 ms. [Benchmarks](docs/benchmarks.md) · reproduce with `make bench-hitrate`.

**When to use**

- LLM agents re-querying the same corpora (retry loops, multi-agent fanout, tool-call storms)
- Multi-backend retrieval (cascading, federating, or migrating across stores)
- Hitting embed / managed-vector cost or latency under agentic load

**When *not* to use**

- A one-off script with no TTL / semantic / multi-backend needs — the in-process [`Engine`](docs/engine.md) is the lightweight path
- LLM-response caching (that's GPTCache or RedisVL SemanticCache territory)
- Cross-org mTLS peer replication (not planned; use a mesh sidecar)

**vs alternatives**

| Need | Prefer |
|------|--------|
| Cache **LLM answers** | GPTCache / RedisVL SemanticCache |
| Cache **search/retrieval** under agents | **conproxy** |
| One process, no daemon, single MCP server | [`Engine`](docs/engine.md) (in-process query core) |
| Multi-backend cascade / MCP tune / dry-run scope | **conproxy** |
| LLM-side semantic cache for prompts | LangChain cache / provider-level caching |

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

## Use it

Five ways in. One copy-paste each; [docs](#documentation) for the rest.

### Python Engine

In-process query core — no daemon, no gRPC. Same request path as the daemon. [docs/engine.md](docs/engine.md) · [docs/sdk-python.md](docs/sdk-python.md).

```bash
pip install conproxy
```

```python
from conproxy import Engine

engine = Engine(config="conproxy.toml")
result = await engine.query("how does X work", top_k=10)
# result.cache_status: 1=hit, 2=miss, 3=stale, 4=frozen
```

Always `await query()`. Talk to a running daemon instead: `ConproxyClient(grpc_url="http://localhost:9999")`.

### Rust Engine

Same Engine, in-process. [docs/engine.md](docs/engine.md).

```bash
cargo add conproxy
```

```rust
use conproxy::{Engine, QueryOpts};

let engine = Engine::builder()
    .config_toml("conproxy.toml")
    .build()?;
let result = engine
    .query("how does X work", QueryOpts { top_k: Some(10), ..Default::default() })
    .await?;
```

gRPC client crate: `cargo add conproxy-sdk`.

### Docker daemon

Shared cache in front of a backend — the default when multiple agents share one cache. [Full walkthrough](docs/quickstart.md).

```bash
docker pull ghcr.io/jmcgrath207/conproxy:0.2.1
docker run -d --name conproxy -p 9999:9999 -p 10000:10000 \
  -v "$PWD/conproxy.toml:/etc/conproxy/conproxy.toml:ro" \
  ghcr.io/jmcgrath207/conproxy:0.2.1
curl -s http://127.0.0.1:10000/health
```

Local binary (no Docker):

```bash
cargo install conproxy --locked --features release
docker run -d -p 6333:6333 qdrant/qdrant
conproxy start --config examples/qdrant-quickstart.toml --daemon
curl -s http://127.0.0.1:9090/query \
  -H 'Content-Type: application/json' \
  -d '{"query": "how to handle errors in rust", "top_k": 5}'
```

Compose (proxy + Meilisearch): `examples/docker-compose/` · [docs/docker-compose.md](docs/docker-compose.md).

Helm: `helm install conproxy oci://ghcr.io/jmcgrath207/charts/conproxy --version 0.2.1`

`release` = `mcp` + `persistence` + `embed-api` + `pgvector`. Flags: [docs/feature-flags.md](docs/feature-flags.md).

### MCP

`release` already includes the MCP server. Point Claude Desktop or opencode at `conproxy mcp`. [docs/mcp-integration.md](docs/mcp-integration.md).

```json
{ "mcpServers": { "conproxy": { "command": "conproxy", "args": ["mcp"] } } }
```

opencode (`~/.config/opencode/opencode.jsonc`):

```jsonc
{ "mcp": { "conproxy": { "type": "local", "command": ["conproxy", "mcp"], "enabled": true } } }
```

Start a daemon first (`conproxy start --config … --daemon`) so `search` has an upstream.

### gRPC / HTTP

Any language. Default container ports: gRPC `:9999`, HTTP `:10000`. [docs/api-reference.md](docs/api-reference.md).

```bash
curl -s http://127.0.0.1:10000/query \
  -H 'Content-Type: application/json' \
  -d '{"query": "how to handle errors in rust", "top_k": 5}'
```

## Features

**In-process Engine**

- Same `execute_query` path as the daemon — no gRPC, no peer, no daemon ([docs/engine.md](docs/engine.md))
- `pip install conproxy` (Python) / `cargo add conproxy` (Rust); the wheel links the query core, not a thin client
- Memory budget: `max_memory` / `memory_fraction` bounds the cache in-process
- Optional read-only dashboard (`dashboard_listen`) — health, stats, metrics, cache, contexts
- Use the daemon instead when multiple agents should share one cache

**Agentic cache**

- In-memory cache with TTL, jitter, and background refresh; S3-FIFO eviction
- Hit path skips embed + upstream — **cost and latency** win on every hit; coalesce collapses concurrent duplicates
- Semantic tier with τ-frontier and measured false-hit rate (≤1% gate)
- Request coalescing (singleflight) to collapse concurrent duplicates
- Negative caching for errors; serve-stale-while-refresh

**One endpoint, any backend**

- MCP server (stdio): `search`, dry-run **tune** suite, dashboard-parity **status tools** (health, overview, cache_status, pool_status, circuit_status, metrics_status, contexts_status, peer_status, tokio_status, cache_entries)
- 7 upstream adapters: Elasticsearch, OpenSearch, Qdrant, pgvector, Meilisearch, Pinecone, Milvus
- Runtime query-mode probe (TextNative vs VectorOnly) per adapter
- Context-rooted multi-tenancy (`[contexts.<id>]`): per-agent API keys, rate limits, isolated cache, scope phrases

**Tune**

- **Scope loop** — dry-run Score C filter/boost/rerank + `min_similarity` sweeps on supplied or live hits; phrase suggest; run compare/select
- **What-if probes** — cache TTL hit/stale/miss, cascade leg selection, federated merge weights, embed batch shape, rate-limit allow/deny, warm-plan ETA — session-scoped, no backend write
- **Build local — ship prod** — one call: `tune_workflow` (open → search → tune → optional `apply_tune` + hot-reload); or export `contexts.<id>.scope` TOML/JSON to paste by hand

**Cascade & federation**

- **Cascade** — priority-ordered fallback chain; quality gate (`min_score_threshold`, `min_results`); optional RRF fusion for equal-priority legs
- **Federation** — local-first with confidence-gated remote fallback; configurable merge modes

**Correctness you can gate**

- `make bench-hitrate` family (exact / sem / onnx / live) with PASS / FAIL-CORE / FAIL-TRUST verdicts
- τ-frontier + false-hit rate per workload (only public frontier in the space, as far as we know)
- TTL sweep + what-if CDC model — measures stale rate vs healing options
- MCP `benchmark` — live query vs tuned scope diff; improved / degraded / unchanged (pairs with Tune)

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
| [Docker Compose](docs/docker-compose.md) | Side-by-side conproxy + backend stack ([example](examples/docker-compose/)) |
| [Feature Flags](docs/feature-flags.md) | Compile-time features |
| [Python SDK](docs/sdk-python.md) | Native client + LangChain/LlamaIndex adapters |
| [Engine](docs/engine.md) | In-process query core (Rust + Python) |

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

## License

MIT
