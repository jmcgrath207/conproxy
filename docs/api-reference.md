# API Reference

Conproxy exposes both HTTP and gRPC APIs. By default the proxy listens on a single address for gRPC; the HTTP API listens on the next port (or a separately configured `http_listen` address).

## Authentication

If `proxy.api_key` or `proxy.security.api_key` is set, protected endpoints require the header:

```
X-Api-Key: <your-key>
```

When agents are configured (`[[proxy.agents]]`), each agent uses its own key. The `X-Agent-Id` header identifies which agent is making the request.

---

## HTTP Endpoints

### Search

#### `POST /query`

Execute a single search query with caching.

**Request:**

```json
{
  "query": "error handling in rust",
  "top_k": 5,
  "priority": 1,
  "upstream_id": "qdrant-primary",
  "upstream_type": "qdrant"
}
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `query` | `string` | yes | — | Search query (max 10,000 chars) |
| `top_k` | `usize` | no | `10` | Max results (max 1,000) |
| `priority` | `u8` | no | `1` | 0=low, 1=normal, 2=high, 3=critical |
| `upstream_id` | `string` | no | — | Route to specific upstream (skip cascade) |
| `upstream_type` | `string` | no | — | Prefer upstreams of this type |

**Response:**

```json
{
  "results": [
    {
      "id": "doc-42",
      "score": 0.89,
      "content": "Rust uses the Result type for recoverable errors...",
      "metadata": {"source": "rust-book"},
      "upstream_id": "qdrant-primary"
    }
  ],
  "cache_status": "miss",
  "took_ms": 45,
  "generated_at": 1709827200000,
  "miss_reason": "not_found"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `results` | `SearchResult[]` | Search results |
| `cache_status` | `string` | `"hit"`, `"miss"`, `"stale"`, or `"frozen"` |
| `took_ms` | `u64` | Response time in milliseconds |
| `generated_at` | `u64?` | Unix epoch ms (when replay detection is enabled) |
| `miss_reason` | `string?` | Why the cache missed (only on miss) |

**SearchResult fields:**

| Field | Type | Description |
|-------|------|-------------|
| `id` | `string` | Document/chunk identifier |
| `score` | `f32` | Relevance score (higher is better) |
| `content` | `string` | Content text |
| `metadata` | `object?` | Optional metadata |
| `upstream_id` | `string?` | Which upstream produced this result |

```bash
curl -s http://127.0.0.1:9090/query \
  -H 'Content-Type: application/json' \
  -H 'X-Api-Key: my-secret' \
  -d '{"query": "error handling", "top_k": 5}'
```

#### `POST /batch`

Execute multiple queries in one request.

**Request:**

```json
{
  "queries": [
    {"query": "error handling", "top_k": 3},
    {"query": "async patterns", "top_k": 3}
  ]
}
```

**Response:**

```json
{
  "responses": [
    {"results": [...], "cache_status": "hit", "took_ms": 0},
    {"results": [...], "cache_status": "miss", "took_ms": 32}
  ],
  "total_took_ms": 32
}
```

#### `POST /federated`

Execute federated search combining local cache and remote upstream.

**Request:** Same as `/query`.

**Response:** Same as `/query`, with results merged according to the configured merge mode.

---

### Cache Management

#### `POST /cache/clear`

Clear all cache entries.

```bash
curl -X POST http://127.0.0.1:9090/cache/clear -H 'X-Api-Key: my-secret'
```

#### `POST /cache/warmup`

Pre-fetch queries to populate the cache.

**Request:**

```json
{
  "queries": ["error handling", "async patterns", "memory safety"]
}
```

#### `POST /cache/evict`

Selectively evict cache entries.

**Request:**

```json
{
  "pattern": "error*",
  "older_than_secs": 3600
}
```

#### `GET /cache/integrity`

Verify cache entry integrity using blake3 content hashes.

#### `GET /cache/upstreams`

Get cache statistics broken down by upstream.

---

### Context Management

#### `GET /contexts`

List all available contexts.

#### `GET /contexts/current`

Get current context metadata and cache stats.

#### `POST /contexts/switch`

Switch to a different context.

**Request:**

```json
{
  "context_id": "production"
}
```

#### `POST /contexts/create`

Create a new context.

**Request:**

```json
{
  "context_id": "staging",
  "upstream_url": "http://staging-qdrant:6333",
  "collection": "staging-docs"
}
```

#### `GET /contexts/:id/stats`

Get per-context cache statistics.

---

### Admin

#### `POST /admin/reload`

Hot-reload configuration from disk without restarting.

```bash
curl -X POST http://127.0.0.1:9090/admin/reload -H 'X-Api-Key: my-secret'
```

#### `POST /admin/pause`

Pause accepting new queries. In-flight requests drain.

#### `POST /admin/resume`

Resume accepting queries after a pause.

#### `POST /admin/metrics/reset`

Reset all metrics counters to zero.

#### `GET /admin/agents`

List all registered agents.

#### `POST /admin/agents`

Register a new agent.

**Request:**

```json
{
  "id": "new-agent",
  "api_key": "agent-key-123",
  "default_context": "default",
  "allowed_contexts": ["default", "production"]
}
```

#### `DELETE /admin/agents/:id`

Remove an agent.

#### `POST /admin/agents/:id/rotate-key`

Rotate an agent's API key.

---

### Observability

#### `GET /stats`

Server and cache statistics (JSON).

#### `GET /stats/queries`

Query access statistics and hot queries.

#### `GET /metrics`

Detailed performance metrics (JSON).

#### `GET /metrics/prometheus`

Metrics in Prometheus/OpenMetrics text format. **Public** (no auth required).

Key metrics exposed:

```
conproxy_pool_upstreams_total
conproxy_pool_upstreams_healthy
conproxy_pool_upstreams_by_type{type="fts|vector_db|hybrid|unknown"}
conproxy_pool_active_connections
conproxy_pool_utilization
```

#### `GET /audit`

Recent request audit log.

#### `GET /circuit`

Circuit breaker status per upstream.

#### `GET /queue`

Request queue status and statistics.

#### `GET /clients`

Active client connections.

---


### Public Endpoints

These endpoints require no authentication.

#### `GET /health`

Health check with upstream status. Returns 200 if the proxy is running.

```bash
curl http://127.0.0.1:9090/health
```

#### `GET /ready`

Readiness probe for load balancers and Kubernetes. Returns 200 when the proxy can serve traffic: a vector upstream is healthy, the cache is populated (cache-only mode), or one or more LLM providers are configured (LLM-only mode). 503 only when no vector upstream, no populated cache, and no LLM providers are available.

```bash
curl http://127.0.0.1:9090/ready
```

#### `GET /debug/tokio`

Runtime aggregates snapshot from `tokio::runtime::Handle::metrics()`. Returns JSON
with `num_alive_tasks`, `num_workers`, `worker_total_busy_duration_ns`,
`worker_poll_count`, `worker_mean_poll_time_ns`, `global_queue_depth`,
`budget_forced_yield_count`. Always available (no feature flag). Useful for
tracking runtime health under load.

```bash
curl http://127.0.0.1:9090/debug/tokio
```

#### `GET /debug/tokio/dump`

One-shot task backtraces via `tokio::runtime::Handle::dump()`. Returns
text/plain with all live task stacks — best for stuck-task diagnosis.
Requires building with **both** the `tokio-taskdump` feature and
`RUSTFLAGS=--cfg tokio_unstable`; otherwise returns 503 with a hint.

```bash
RUSTFLAGS=--cfg tokio_unstable \
  cargo build --profile profiling --features tokio-taskdump --bin conproxy
curl http://127.0.0.1:9090/debug/tokio/dump
```

#### `GET /pool`

Upstream pool status and statistics.

#### `GET /peer/status`

Peer replication status (CDC/P2P).

---

## Cache Status Values

| Status | Description |
|--------|-------------|
| `hit` | Response served from fresh cache |
| `miss` | Cache miss — fetched from upstream and cached |
| `stale` | Served stale response while refreshing in background |
| `frozen` | Upstream offline — serving last known good response |

---

## gRPC Services

Proto files are located at:
- `proto/conproxy/v1/search.proto` — Search, Admin, Context, Observability services
- `proto/conproxy/cdc/v1/cdc.proto` — CDC and Peer services

### SearchService

| RPC | Request | Response | Description |
|-----|---------|----------|-------------|
| `Query` | `QueryRequest` | `QueryResponse` | Single cached query |
| `BatchQuery` | `BatchQueryRequest` | `BatchQueryResponse` | Multiple queries |
| `FederatedQuery` | `FederatedQueryRequest` | `FederatedQueryResponse` | Federated search |
| `QueryStream` | `QueryRequest` | `stream QueryResponse` | Server-streaming results |

### AdminService

| RPC | Request | Response | Description |
|-----|---------|----------|-------------|
| `Reload` | `ReloadRequest` | `ReloadResponse` | Hot-reload config |
| `Pause` | `PauseRequest` | `PauseResponse` | Pause queries |
| `Resume` | `ResumeRequest` | `ResumeResponse` | Resume queries |
| `CacheClear` | `CacheClearRequest` | `CacheClearResponse` | Clear cache |
| `CacheWarmup` | `CacheWarmupRequest` | `CacheWarmupResponse` | Warm cache |
| `CacheEvict` | `CacheEvictRequest` | `CacheEvictResponse` | Evict entries |
| `CacheIntegrity` | `CacheIntegrityRequest` | `CacheIntegrityResponse` | Verify integrity |
| `MetricsReset` | `MetricsResetRequest` | `MetricsResetResponse` | Reset metrics |
| `ListAgents` | `ListAgentsRequest` | `ListAgentsResponse` | List agents |
| `CreateAgent` | `CreateAgentRequest` | `CreateAgentResponse` | Create agent |
| `DeleteAgent` | `DeleteAgentRequest` | `DeleteAgentResponse` | Delete agent |
| `RotateKey` | `RotateKeyRequest` | `RotateKeyResponse` | Rotate agent key |

### ContextService

| RPC | Request | Response | Description |
|-----|---------|----------|-------------|
| `ListContexts` | `ListContextsRequest` | `ListContextsResponse` | List contexts |
| `GetCurrentContext` | `GetCurrentContextRequest` | `ContextInfo` | Current context |
| `SwitchContext` | `SwitchContextRequest` | `SwitchContextResponse` | Switch context |
| `CreateContext` | `CreateContextRequest` | `CreateContextResponse` | Create context |
| `GetContextStats` | `GetContextStatsRequest` | `ContextStats` | Context stats |

### ObservabilityService

| RPC | Request | Response | Description |
|-----|---------|----------|-------------|
| `GetStats` | `GetStatsRequest` | `StatsResponse` | Server stats |
| `GetQueryStats` | `GetQueryStatsRequest` | `QueryStatsResponse` | Query stats |
| `GetAudit` | `GetAuditRequest` | `AuditResponse` | Audit log |
| `GetCircuitStatus` | `GetCircuitStatusRequest` | `CircuitStatusResponse` | Circuit breaker |
| `GetQueueStats` | `GetQueueStatsRequest` | `QueueStatsResponse` | Queue stats |
| `GetClients` | `GetClientsRequest` | `ClientsResponse` | Active clients |
| `GetPoolStatus` | `GetPoolStatusRequest` | `PoolStatusResponse` | Pool status |
| `GetCacheUpstreams` | `GetCacheUpstreamsRequest` | `CacheUpstreamsResponse` | Cache by upstream |
| `GetCacheDistill` | `DistillRequest` | `stream DistillEntry` | Stream cache entries for the `distill` command |

### CdcService

| RPC | Request | Response | Description |
|-----|---------|----------|-------------|
| `Subscribe` | `CdcSubscribeRequest` | `stream CdcEvent` | Subscribe to cache mutations |

**CdcEvent fields:**

| Field | Type | Description |
|-------|------|-------------|
| `sequence` | `uint64` | Per-node monotonic sequence |
| `timestamp_ms` | `uint64` | Wall-clock epoch ms |
| `event_type` | `enum` | `CDC_INSERT`, `CDC_REMOVE`, `CDC_FETCH_START`, `CDC_FETCH_CANCEL` |
| `query_key` | `string` | Cache key |
| `payload` | `bytes` | JSON-serialized QueryResponse (INSERT only) |
| `upstream_id` | `string` | Source upstream |
| `context_id` | `string` | Context isolation |
| `origin_node_id` | `string` | Originating node (echo prevention) |
| `absolute_expiry_ms` | `uint64` | Wall-clock expiry (0 = none) |

### Distill Messages

`GetCacheDistill` is a server-streaming RPC. The client sends a single
`DistillRequest` and the proxy streams zero or more `DistillEntry` messages
back. The stream closes naturally when all matching entries have been sent.
See [Distill](distill.md) for the workflow and the `conproxy distill` CLI.

**DistillRequest fields:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `context` | `string` | `""` | Filter to a single context ID (empty = all) |
| `tier` | `uint32` | `0` | `0` = primary, `1` = semantic, `2` = both |
| `limit` | `uint32` | `0` | Max entries to return (0 = unlimited) |
| `include_stale` | `bool` | `false` | Include entries past their fresh TTL |

**DistillEntry fields:**

| Field | Type | Description |
|-------|------|-------------|
| `query` | `string` | Original query text (empty for legacy entries) |
| `context_id` | `string` | Context the entry belongs to (`"default"` for pre-distill entries) |
| `upstream_id` | `string` | Upstream that produced the response |
| `cached_at_ms` | `uint64` | Insertion time (Unix epoch milliseconds) |
| `extended_count` | `uint32` | Number of TTL extensions applied |
| `response_json` | `bytes` | JSON-encoded `QueryResponse` payload |
| `hash_hex` | `string` | blake3 query hash (64-char lowercase hex) |
| `embedding` | `repeated float` | Embedding vector (semantic tier only; empty for primary) |

### PeerService

| RPC | Request | Response | Description |
|-----|---------|----------|-------------|
| `GetStatus` | `PeerStatusRequest` | `PeerStatusResponse` | Peer status |
| `Snapshot` | `SnapshotRequest` | `stream CdcEvent` | Full cache snapshot |

### Generating gRPC Clients

The proto files are compiled during `cargo build` via `tonic-build`. To generate clients in other languages, use the proto files directly:

```bash
# Go
protoc --go_out=. --go-grpc_out=. proto/conproxy/v1/search.proto

# Python
python -m grpc_tools.protoc -Iproto --python_out=. --grpc_python_out=. proto/conproxy/v1/search.proto
```
