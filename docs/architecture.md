# Architecture

This document describes conproxy's internal design for contributors and operators who want to understand how the system works.

## High-Level Data Flow

```
                         ┌──────────────┐
                         │   Clients    │
                         │ (AI Agents)  │
                         └──────┬───────┘
                                │
                     HTTP/gRPC  │  X-Api-Key
                                ▼
                    ┌───────────────────────┐
                    │    Middleware Stack    │
                    │  Auth → Rate Limit →  │
                    │  Priority Queue       │
                    └───────────┬───────────┘
                                │
                                ▼
                    ┌───────────────────────┐
                    │     Query Handler     │
                    │  (request coalescing) │
                    └───────────┬───────────┘
                                │
                    ┌───────────┴───────────┐
                    │                       │
                    ▼                       ▼
            ┌──────────────┐       ┌──────────────┐
            │  Cache Store │       │   Cascade    │
            │  (in-memory) │       │  Executor    │
            └──────┬───────┘       └──────┬───────┘
                   │                      │
                   │               ┌──────┴──────┐
                   │               ▼             ▼
                   │       ┌────────────┐ ┌────────────┐
                   │       │ Upstream 1 │ │ Upstream 2 │ ...
                   │       │ (Qdrant)   │ │ (ES)       │
                   │       └────────────┘ └────────────┘
                    │
                    └──── CDC Events ────▶ Peers
```

**Request path:**

1. Client sends a query via HTTP POST `/query` or gRPC `SearchService.Query`
2. Middleware authenticates (API key), applies rate limits, and queues by priority
3. Query handler checks the cache — on hit, returns immediately
4. On miss, request coalescing deduplicates concurrent identical queries
5. **Cascade executor** orders legs by `priority` and tries them in turn:
   - Within a priority group, legs are load-balanced by `weight` (or fused with RRF when `fusion_method = "rrf"`)
   - Each leg's adapter is queried in the right mode — text-native or vector-only (auto-probed on first use via `discover_query_mode`)
   - Results from each leg are normalized to 0–1 and scored against `min_score_threshold` + `min_results`
6. Cascade stops on `ThresholdMet` / `MinResultsMet` / `MaxDepthReached` / `AllExhausted` / `Timeout`
7. With `merge_cascade_results = true`, results from all tried legs are combined (or the last successful leg only, by default)
8. Results are cached with TTL + jitter, then returned to the client
9. CDC events are broadcast for peer replication

**Federation path** is a different request type (`/federated`): the proxy starts with a local result set (cache or designated "local" leg), decides whether to query the remote leg based on `min_local_results` / `min_local_confidence`, and merges per the configured `merge_mode` (`local_only_fallback`, `local_priority`, `remote_priority`, `interleave`). Stats on the response body report `local_count`, `remote_queried`, `fallback_reason`, `merged_count`.

**Cascade vs. Federation** is a policy choice on the context, not a code path split — same executor, different config block. Cascade = ordered leg chain. Federation = parallel leg merge with confidence gate. Both share the same upstream pool and cache.

## Module Map

```
src/
├── lib.rs                    Module root, re-exports
├── error.rs                  ConproxyError, Result type
├── config/
│   └── mod.rs                All config structs, loading, merging, validation
├── cache/
│   └── ...                   Cache index, search, scoring
├── embedding/
│   ├── embedder.rs           ONNX embedding engine (feature: embed)
│   ├── provider.rs           EmbedderProvider trait + factory (feature: embed-api)
│   ├── openai.rs             OpenAI /v1/embeddings client (feature: embed-api)
│   ├── cohere.rs             Cohere /v1/embed client (feature: embed-api)
│   ├── huggingface.rs        HuggingFace inference client (feature: embed-api)
│   └── models.rs             Model management (download, list, info)
├── mcp/
│   └── mod.rs                MCP tools: search, plus tune suite (cache/scope/cascade/embed tuning)
├── proxy/
    ├── mod.rs                Re-exports all proxy types
    ├── types.rs              QueryRequest, QueryResponse, CacheEntry, SearchResult
    ├── server/
    │   ├── mod.rs            Axum router, AppState, startup
    │   ├── query.rs          handle_query (coalescing, cache, upstream)
    │   ├── batch.rs          handle_batch, handle_federated
    │   ├── cache.rs          Cache management endpoints
    │   ├── status.rs         Stats, metrics, health, prometheus
    │   ├── context.rs        Context CRUD
    │   └── admin.rs          Reload, pause/resume, agent management
    ├── adaptive.rs           P99-based adaptive timeout
    ├── agent.rs              Agent authentication and routing
    ├── audit.rs              Request audit log
    ├── cascade.rs            Priority-based cascade executor + RRF fusion
    ├── cdc/
    │   ├── mod.rs            CDC module root, event types
    │   ├── event.rs          CDC event definitions
    │   └── stream.rs         CDC event stream (broadcast/subscribe)
    ├── circuit.rs            Circuit breaker (Closed/Open/HalfOpen)
    ├── client.rs             ClientTracker for connection counting
    ├── coalesce.rs           Request coalescing (singleflight)
    ├── connection_pool.rs    Semaphore-based connection pool
    ├── context.rs            Multi-context cache isolation
    ├── elasticsearch.rs      Elasticsearch adapter
    ├── federated.rs          Local-first federated search
    ├── (fuzzy.rs removed in plan 02)
    ├── grpc.rs               gRPC service implementations
     ├── lifecycle.rs          PID file, daemon management
     ├── metrics.rs            Prometheus metrics formatting
    ├── middleware.rs          Auth, rate limit middleware
    ├── observability.rs      RequestId, CacheMutationLog, TraceBuilder
    ├── peer/
    │   ├── mod.rs            P2P module root, replication
    │   ├── receiver.rs       Peer CDC stream receiver
    │   ├── service.rs        Peer gRPC service
    │   ├── singleflight.rs   Distributed singleflight (CDC_FETCH_START)
    │   └── snapshot.rs       Full cache snapshot on join
    ├── persistence.rs        redb-based disk cache (feature: persistence)
    ├── pgvector.rs           pgvector adapter (feature: pgvector)
    ├── pool.rs               Upstream pool, health tracking
    ├── priority.rs           Priority queue for requests
    ├── qdrant.rs             Qdrant API client
    ├── query_stats.rs        Query access tracking, hot queries
    ├── refresh.rs            Background refresh worker
    ├── resilience.rs         3-state upstream health (Healthy/Degraded/Offline)
    ├── semantic_cache.rs     Semantic cache tier (embedding similarity)
    ├── retry.rs              Exponential backoff retry
    ├── scope.rs              Seed-based scope filtering
    ├── smart_embedder.rs     Embedding cache + coalescing (feature: embed)
    ├── socket_tuning.rs      TCP/socket options
    ├── upstream.rs           Upstream request execution
    └── workers.rs            WorkerScheduler, ScheduledTask
└── bin/
    ├── conproxy/
    │   ├── main.rs           CLI entry point, command dispatch
    │   └── commands/
    │       ├── mod.rs        Command trait, shared helpers
    │       ├── proxy.rs      Init/Start/Stop/Status/Peer/Cdc/Contexts
    │       └── seed.rs       Seed subcommands
    ├── generate_embeddings.rs Offline embedding generation tool
    ├── test_runner.rs        E2E test runner (separate binary)
    ├── perf_summarize.rs     Criterion results summarizer + ANALYSIS.md
    ├── hitrate_bench.rs      Cache hit-rate benchmark (Zipf, agentic, live)
    └── console_snap.rs       Headless tokio-console dump (CI-friendly)
```

## SDKs

Conproxy ships first-party gRPC clients in two languages plus framework adapters for the Python SDK.

```
sdk/
├── rust/                  Rust client crate (conproxy-sdk)
│   ├── src/
│   │   ├── client.rs      Tonic-based ConproxyClient
│   │   ├── config.rs      SdkConfig + TOML loader
│   │   └── proto.rs       Generated proto types
│   └── Cargo.toml
└── python/                Python SDK (conproxy, builds conproxy_py via maturin)
    ├── src/
    │   ├── lib.rs         PyO3 module entry
    │   ├── client.rs      ConproxyClient Python wrapper
    │   ├── types.rs       PyO3 wrappers for proto types
    │   ├── error.rs       SdkError → Python exceptions
    │   ├── langchain.py   ConproxyRetriever (BaseRetriever)
    │   └── llama_index.py ConproxyRetriever (BaseRetriever)
    ├── examples/
    │   ├── langchain_rag.py
    │   └── llama_index_rag.py
    ├── pyproject.toml
    └── Cargo.toml
```

See [Python SDK](sdk-python.md) for adapter usage and [API Reference](api-reference.md#generating-grpc-clients) for generating clients in other languages.

## Cache Lifecycle

Cache entries move through a lifecycle based on time:

```
MISS ──▶ FRESH ──▶ STALE ──▶ EXPIRED
           │          │
           │          └── Served while refreshing in background
           │              (stale-while-revalidate)
           │
           └── Served immediately (cache hit)

FROZEN: When upstream is offline, stale/expired entries are served
        as "frozen" — the last known good response.
```

**TTL stages:**

1. **Fresh** (`0` to `fresh_duration_secs`): Served as cache hit
2. **Stale** (`fresh_duration_secs` to `fresh + stale_duration_secs`): Served immediately, background refresh triggered
3. **Expired** (beyond stale): Entry evicted, next request is a cache miss

**TTL jittering:** Each entry's TTL is randomly adjusted by `±ttl_jitter_percent` (default 10%) to prevent thundering herd when many entries expire simultaneously.

**Background refresh:** A worker runs every `refresh_interval_secs` (default 60s), scanning for stale entries and re-fetching them from upstream. This keeps the cache warm without clients waiting.

**Request coalescing:** When multiple clients request the same query simultaneously, only one upstream request is made. All waiting clients receive the same response (singleflight pattern).

## Resilience Stack

Conproxy uses a layered resilience approach, from innermost (per-request) to outermost (system-wide):

### Layer 1: Retry with Backoff

Individual upstream requests are retried on transient failures with exponential backoff and jitter. Configurable per error type (network, timeout, 5xx, 429).

### Layer 2: Circuit Breaker

Per-upstream circuit breaker with three states:
- **Closed** — Normal operation, counting failures
- **Open** — Too many failures, rejecting requests for `open_duration_secs`
- **Half-Open** — Testing with a single request to see if upstream recovered

### Layer 3: Upstream State Machine

A 3-state machine that combines circuit breaker signals with rate limiting:

```
                 ┌─────────┐
        success  │         │  5 failures / 60s
    ┌───────────▶│ Healthy  ├──────────────┐
    │            │         │              │
    │            └─────────┘              ▼
    │                              ┌───────────┐
    │            3 successes       │           │  10 consecutive
    └──────────────────────────────┤ Degraded  ├──────────┐
                                   │           │          │
                                   └───────────┘          ▼
                                        ▲          ┌──────────┐
                                        │          │          │
                                   probe success   │ Offline  │
                                        └──────────┤          │
                                                   └──────────┘
                                                    probe after 30s
```

- **Healthy**: Full traffic, adaptive timeout based on P99
- **Degraded**: Rate-limited traffic (starts at 10 RPS, ramps up with successes), retries enabled
- **Offline**: No traffic, periodic probe attempts

### Layer 4: Connection Pool

Semaphore-based concurrency control (pgbouncer-style) limits the number of concurrent requests to each upstream. Excess requests queue with a configurable timeout.

### Layer 5: Degradation Ladder

System-wide PAUSE/RESUME control for graceful degradation during incidents. The admin can pause the proxy to drain in-flight requests, then resume when the issue is resolved.

## P2P Replication

When peer replication is enabled, cache mutations are broadcast as CDC events:

1. A cache INSERT on Node A emits a `CDC_INSERT` event
2. The event is broadcast on the local CDC channel
3. Peer nodes subscribe to each other's CDC streams via gRPC
4. Receiving peers apply the event with deduplication:
   - **Echo prevention**: Events with `origin_node_id` matching the receiver are skipped
   - **Last-write-wins**: If the local entry has a newer wall `timestamp_ms` / `cached_at_wall`, the remote event is dropped (equal → keep local)
5. New peers request a full snapshot on join (`snapshot_on_join = true`)
6. **Auth**: optional `[proxy.peer] shared_secret` — when set, peers must present `x-peer-secret` header on the gRPC stream. Default: off (trusted network). The process-level `api_key` still applies to non-peer gRPC when set. No mTLS — use NetworkPolicy or a mesh sidecar externally if needed.

Distributed singleflight: `CDC_FETCH_START` events prevent multiple peers from fetching the same cache miss simultaneously. If a peer is already fetching, others wait for the result.

## Connection Pooling

The connection pool uses a `tokio::sync::Semaphore` for concurrency control:

- Each upstream gets a pool with `max_connections` permits
- Requests acquire a permit before executing; on pool exhaustion, they queue
- Queued requests wait up to `queue_timeout_ms` before receiving a `QueueFull` error
- `PooledConnection` is an RAII guard that returns the permit on drop
- Fair queueing (FIFO) ensures requests are served in order

## Feature Flag Architecture

Optional modules are gated behind Cargo feature flags and compile out when disabled:

```
proxy (always)     → Core cache proxy, HTTP/gRPC server
├── mcp            → MCP server (rmcp, schemars)
├── embed-api      → EmbedderProvider trait + OpenAI/Cohere/HF clients
├── embed          → Local ONNX embedding (implies embed-api; ort, tokenizers, ndarray)
├── persistence    → Disk cache with redb
├── pgvector       → pgvector adapter (tokio-postgres)
├── linux-sandbox  → seccomp sandbox (caps, nix)
├── tokio-console  → Async task inspector (console-subscriber)
├── tokio-taskdump → Handle::dump() backtraces (RUSTFLAGS=--cfg tokio_unstable)
├── tokio-console-snap → console_snap headless dump bin (console-api)
├── integration-tests → testcontainers-based backend tests
├── e2e            → E2E proxy tests
├── load-test      → rlt-based load testing
└── dhat-heap      → DHAT heap profiling
```

Meta-features:
- `release` = `mcp` + `persistence` + `embed-api` + `pgvector` (production binary; ONNX + sandbox opt-in)
- `test` = `release` + `load-test` + `dhat-heap` (testing infrastructure)
