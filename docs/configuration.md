# Configuration Reference


Conproxy uses TOML configuration files. A global config at `~/.conproxy/conproxy.toml` provides defaults; a local `.conproxy/conproxy.toml` in your project overrides them. Fields from the local config take precedence, with some sections (like registries) being merged additively.

## Full Example

```toml
[proxy]
listen = "127.0.0.1:9090"
http_listen = "127.0.0.1:9091"
fresh_duration_secs = 300
stale_duration_secs = 600
max_entries = 10000
ttl_jitter_percent = 0.1
refresh_interval_secs = 60
api_key = "my-secret-key"

[[proxy.upstreams]]
id = "qdrant-primary"
url = "http://localhost:6333"
upstream_type = "qdrant"
timeout_secs = 30
weight = 2
priority = 0
max_concurrent = 50
enabled = true

[[proxy.upstreams]]
id = "es-fallback"
url = "http://localhost:9200"
upstream_type = "elasticsearch"
index = "documents"
search_fields = ["title", "content"]
return_fields = ["title", "content", "source"]
timeout_secs = 15
priority = 1

[proxy.cascade]
enabled = true
min_score_threshold = 0.7
min_results = 1
max_cascade_depth = 3
merge_cascade_results = false
cascade_timeout_ms = 30000
fusion_method = "rrf"
rrf_k = 60

[proxy.pool]
max_connections = 100
max_queue_size = 500
queue_timeout_ms = 30000
idle_timeout_ms = 90000
fair_queueing = true

[proxy.circuit_breaker]
failure_threshold = 25
success_threshold = 2
open_duration_secs = 30
failure_window_secs = 60

[proxy.retry]
enabled = true
max_retries = 3
initial_delay_ms = 100
max_delay_ms = 10000
backoff_multiplier = 2.0

[proxy.rate_limit]
enabled = true
requests_per_second = 100
burst_size = 50

[proxy.scope]
seeds = ["error handling", "async patterns"]
mode = "filter"
min_seed_similarity = 0.25

[proxy.cache]
max_memory_mb = 256
max_entry_size_kb = 512
eviction_policy = "lru"
normalized_matching = false

[proxy.cache.semantic]
enabled = false
similarity_threshold = 0.92
max_entries = 10000

[proxy.distill]
output_dir = "/var/lib/conproxy/distill"
format = "md"
include_stale = false

[proxy.security]
api_key = "my-secret-key"

[proxy.security.rate_limit]
enabled = true
requests_per_second = 100

[proxy.security.advanced]
enabled = false

[[proxy.agents]]
id = "code-review-agent"
api_key = "crv-xxxxxxxx"
default_context = "codebase-rust"
allowed_contexts = ["codebase-rust", "codebase-python"]
priority_class = 2
rate_limit_rps = 50

[proxy.cdc]
enabled = true
buffer_size = 10000

[proxy.peer]
enabled = true
node_id = "pod-a"
peers = ["pod-b.svc:9090"]

[proxy.socket_tuning]
tcp_nodelay = true
reuse_port = true
listen_backlog = 4096

[proxy.federated]
enabled = false

[search]
default_limit = 10

[embedding]
batch_size = 32
provider = "onnx"
api_key = "${OPENAI_API_KEY}"
base_url = "https://api.openai.com"

[context]
paths = ["packages/**/*.md"]
warm_interval = 300
warm_limit = 1000

[web]
auto_index = false
content_dir = "web"

[packages.my-pkg]
git = "https://github.com/user/repo"
tag = "v1.0"

[registries]
myregistry = "https://registry.example.com"

id = "anthropic"
provider = "anthropic"
base_url = "https://api.anthropic.com"
api_key = "${ANTHROPIC_API_KEY}"
path_prefix = "/v1"
strip_headers = ["x-api-key", "anthropic-version"]
```

---

## `[proxy]`

Top-level proxy server configuration.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `listen` | `string` | `"127.0.0.1:9999"` | gRPC listen address |
| `http_listen` | `string` | gRPC port + 1 | HTTP API listen address |
| `fresh_duration_secs` | `u64` | — | Seconds before a cached entry becomes stale |
| `stale_duration_secs` | `u64` | — | Seconds before a stale entry expires completely |
| `max_entries` | `usize` | — | Maximum number of cache entries |
| `upstream_url` | `string` | — | **Deprecated.** Use `[[proxy.upstreams]]` instead |
| `upstream_timeout_secs` | `u64` | — | Timeout for upstream requests (deprecated) |
| `ttl_jitter_percent` | `f32` | `0.1` | TTL jitter to prevent thundering herd (0.0–1.0) |
| `refresh_interval_secs` | `u64` | `60` | Background refresh worker interval |
| `api_key` | `string` | — | API key for client authentication |
| `shutdown_timeout_secs` | `u64` | `30` | Graceful shutdown timeout for in-flight requests |
| `max_global_connections` | `usize` | `1000` | Max concurrent upstream connections across all upstreams (fail-fast 503) |

## `[[proxy.upstreams]]`

Configure one or more upstream search backends. Repeat the section for multiple upstreams.

### Common fields

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `id` | `string` | **required** | Unique identifier |
| `url` | `string` | **required** | Upstream service URL |
| `upstream_type` | `string` | — | Backend type (see below) |
| `query_mode` | `string` | — | `"text_native"`, `"vector_only"`, or `"unknown"` |
| `timeout_secs` | `u64` | `30` | Request timeout |
| `weight` | `u32` | `1` | Load balancing weight (higher = more traffic) |
| `priority` | `u32` | `0` | Cascade priority (lower = tried first) |
| `max_concurrent` | `usize` | — | Max concurrent requests to this upstream |
| `enabled` | `bool` | `true` | Whether this upstream is active |
| `version_endpoint` | `string` | — | URL path for version polling (e.g., `"/v1/version"`) |
| `version_poll_interval_secs` | `u64` | `60` | Interval between version polls |
| `api_key` | `string` | — | API key for authenticated backends. Supports `${ENV_VAR}` interpolation for secrets. Meilisearch: Bearer master key. Elasticsearch: ApiKey value. Qdrant: sent as `api-key` header. |

**Valid `upstream_type` values:** `elasticsearch`, `opensearch`, `qdrant`, `pinecone`, `milvus`, `pgvector`, `meilisearch`

> **Status:** Elasticsearch, OpenSearch, Qdrant, Meilisearch, pgvector, Pinecone, and Milvus are shipped. Solr is removed.

**Valid `query_mode` values:**
- `text_native` — Upstream handles text queries natively (FTS engines, Qdrant)
- `vector_only` — Proxy must embed the query before sending (pgvector, Milvus)
- `unknown` — Auto-detected on first request

### Elasticsearch/OpenSearch fields

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `index` | `string` | — | Index name to query |
| `search_fields` | `string[]` | `[]` | Fields to search in |
| `return_fields` | `string[]` | `[]` | Fields to include in results |

### pgvector fields

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `table` | `string` | **required** | Table name |
| `embedding_column` | `string` | — | Column containing vectors |
| `content_column` | `string` | — | Column containing document text |
| `metadata_columns` | `string[]` | `[]` | Additional columns to return |
| `distance_metric` | `string` | `"cosine"` | `"cosine"`, `"l2"`, or `"inner_product"` |
| `dimensions` | `usize` | — | Vector dimensions |

## `[proxy.cascade]`

Priority-based cascade queries upstreams in order until results meet the quality threshold.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `true` | Enable cascade fallback |
| `min_score_threshold` | `f32` | `0.7` | Minimum normalized score (0–1) before cascading |
| `min_results` | `usize` | `1` | Minimum results before cascading |
| `max_cascade_depth` | `usize` | `3` | Maximum upstreams to try |
| `merge_cascade_results` | `bool` | `false` | Merge results from multiple upstreams |
| `cascade_timeout_ms` | `u64` | `30000` | Timeout for the entire cascade |
| `fusion_method` | `string` | `"none"` | Result fusion method for equal-priority upstream groups: `"none"` (first threshold-meeting upstream wins) or `"rrf"` (Reciprocal Rank Fusion) |
| `rrf_k` | `u32` | `60` | RRF constant: `score(d) = sum(1.0 / (k + rank))`. Only used when `fusion_method = "rrf"` |

## `[proxy.pool]`

pgbouncer-style connection pooling with semaphore-based concurrency control.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max_connections` | `usize` | `100` | Maximum concurrent connections |
| `max_queue_size` | `usize` | `500` | Maximum requests waiting in queue |
| `queue_timeout_ms` | `u64` | `30000` | Queue wait timeout |
| `idle_timeout_ms` | `u64` | `90000` | Idle connection recycling timeout |
| `fair_queueing` | `bool` | `true` | FIFO ordering (vs. priority bypass) |

## `[proxy.circuit_breaker]`

Stops sending requests to a failing upstream (opens) and resumes after recovery (closes).

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `failure_threshold` | `u32` | `25` | Failures within window to open circuit |
| `success_threshold` | `u32` | `2` | Successes in half-open to close circuit |
| `open_duration_secs` | `u64` | `30` | Seconds before trying half-open |
| `failure_window_secs` | `u64` | `60` | Rolling window for counting failures |

## `[proxy.retry]`

Retry policy for failed upstream requests with exponential backoff.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `true` | Enable retries |
| `max_retries` | `u32` | `3` | Maximum retry attempts |
| `initial_delay_ms` | `u64` | `100` | Delay before first retry |
| `max_delay_ms` | `u64` | `10000` | Maximum retry delay |
| `backoff_multiplier` | `f64` | `2.0` | Exponential backoff factor |
| `on_network_error` | `bool` | `true` | Retry on network errors |
| `on_timeout` | `bool` | `true` | Retry on timeouts |
| `on_server_error` | `bool` | `true` | Retry on 5xx responses |
| `on_rate_limited` | `bool` | `true` | Retry on 429 responses |

## `[proxy.rate_limit]`

Token bucket rate limiting for client requests.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Enable rate limiting |
| `requests_per_second` | `u32` | `100` | Sustained request rate |
| `burst_size` | `u32` | `50` | Burst capacity |

## `[proxy.scope]`

Seed-based scope filtering for relevance control.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `seeds` | `string[]` | `[]` | Seed phrases defining project scope |
| `mode` | `string` | `"filter"` | `"filter"`, `"rerank"`, or `"boost"` |
| `min_seed_similarity` | `f32` | `0.25` | Minimum similarity to seeds (filter mode) |
| `seed_weight` | `f32` | `0.3` | Seed weight (rerank mode) |
| `query_prefix` | `string` | — | Prefix prepended to all queries |

## `[proxy.cache]`

Cache limits and eviction policies.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max_memory_mb` | `usize` | `256` | Maximum memory usage |
| `max_entry_size_kb` | `usize` | `512` | Maximum size per entry |
| `eviction_policy` | `string` | `"lru"` | `"lru"`, `"lfu"`, or `"ttl_first"` |
| `error_ttl_5xx_secs` | `u64` | `30` | Negative cache TTL for 5xx errors |
| `error_ttl_timeout_secs` | `u64` | `10` | Negative cache TTL for timeouts |
| `error_ttl_connection_secs` | `u64` | `5` | Negative cache TTL for connection errors |
| `normalized_matching` | `bool` | `false` | Enable two-tier exact+normalized cache |

### `[proxy.cache.per_upstream]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Enable per-upstream limits |
| `max_entries_per_upstream` | `usize` | `500` | Max entries per upstream |

### `[proxy.cache.semantic]`

Second-tier cache that matches queries by embedding cosine similarity. When a query misses the primary cache, the proxy computes its embedding (via the configured `EmbedderProvider`) and scans this tier for a similar past query whose response can be reused. Requires the `embed-api` feature and a configured embedder.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Enable semantic matching tier |
| `similarity_threshold` | `f32` | `0.92` | Cosine similarity required for a match (0.0–1.0) |
| `max_entries` | `usize` | `10000` | Cap on stored embeddings (LRU eviction beyond this) |

**Trade-offs:**

- Latency: adds one embedding inference + one linear scan on the exact-miss path. For ≤10k entries the scan is sub-millisecond; for larger deployments consider sharding or switching to a vector index.
- Cost: the embedder charges per inference (ONNX is local; API providers bill per request). The first miss for a given query pays this cost; subsequent semantic hits are free.
- False positives: a high `similarity_threshold` reduces the chance of returning semantically unrelated results. `0.92` is a reasonable starting point; tune up if you see false hits.

## `[proxy.distill]`

Defaults for the `conproxy distill` command. All flags on the CLI override the
values here. See [Distill](distill.md) for the full workflow.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `output_dir` | `string` | `distill/` | Where to write per-entry files + index when `--output-dir` is not passed |
| `post_process_cmd` | `string` | — | Command to run after a dump completes (whitespace-split, no shell) |
| `format` | `string` | `"md"` | `"md"`, `"json"`, or `"both"` (validated at load) |
| `include_stale` | `bool` | `false` | Default for `--include-stale` |

## `[proxy.security]`

Unified security configuration.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `api_key` | `string` | — | API key for authentication |

### `[proxy.security.rate_limit]`

Same fields as `[proxy.rate_limit]`.

### `[proxy.security.advanced]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Enable advanced security |
| `signature_algorithm` | `string` | — | `"hmac-sha256"` or `"blake3"` |
| `tls_pinning` | `bool` | `false` | TLS certificate pinning |
| `replay_detection` | `bool` | `false` | Timestamp-based replay detection |
| `replay_window_seconds` | `u64` | `300` | Replay detection window |

## `[[proxy.agents]]`

Per-agent multi-tenancy configuration. Repeat for each agent.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `id` | `string` | **required** | Unique agent identifier |
| `api_key` | `string` | **required** | Agent-specific API key |
| `default_context` | `string` | — | Default context when no header sent |
| `allowed_contexts` | `string[]` | `[]` | Allowed contexts (empty = all) |
| `priority_class` | `u32` | `0` | Priority (lower = higher priority) |
| `rate_limit_rps` | `u32` | — | Per-agent rate limit |
| `enabled` | `bool` | `true` | Whether agent is active |

### Hot-reload behavior

File is authoritative on config reload. When the config file is reloaded (via `SIGHUP` or file change), the agent registry is rebuilt from `[[proxy.agents]]` in the file:

- **File-declared agents** are loaded fresh — `api_key`, `rate_limit_rps`, `default_context`, `allowed_contexts`, `enabled` all update.
- **API-created agents** (via `POST /admin/agents`) that are not in the file are dropped on the next reload. API mutations survive only until the next file reload.
- **In-flight requests** using the old `api_key` complete with the old key; new requests use the new registry immediately after the swap.
- **Agent removal**: remove the `[[proxy.agents]]` block from the file and reload — the agent disappears.

## `[proxy.cdc]`

Change Data Capture event stream for cache mutations.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Enable CDC (auto-enabled with peer replication) |
| `buffer_size` | `usize` | `10000` | Broadcast channel capacity |

## `[proxy.peer]`

Peer-to-peer cache replication over gRPC CDC streams.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Enable P2P replication |
| `node_id` | `string` | hostname | Unique node identifier |
| `peers` | `string[]` | `[]` | Peer gRPC addresses (`host:port`) |
| `reconnect_interval_ms` | `u64` | `5000` | Reconnect interval on disconnect |
| `fetch_wait_timeout_ms` | `u64` | `5000` | Distributed singleflight wait |
| `snapshot_on_join` | `bool` | `true` | Request snapshot on startup |
| `ready_threshold` | `f64` | `0.8` | Fraction of cache needed for readiness |
| `snapshot_batch_size` | `usize` | `100` | Entries per snapshot message |
| `shared_secret` | `string` | — | Optional peer gRPC shared secret (`x-peer-secret`). Supports `${ENV}`. Default off. |

Example (all peers share one secret via env):

```toml
[proxy.peer]
enabled = true
node_id = "pod-a"
peers = ["pod-b.conproxy-svc:9090"]
shared_secret = "${PEER_SECRET}"
```


### Conflict policy (LWW)

Receivers apply INSERT events with **last-write-wins** by wall-clock `timestamp_ms` / `cached_at_wall`. Equal timestamps keep the local entry (stale skip). Echo prevention drops events whose `origin_node_id` matches this node.

### Auth (current)

**Peer auth:** optional `shared_secret` (plan 07). When set, CDC subscribe + Peer snapshot/status require `x-peer-secret` (or `x-api-key`) matching the secret. When unset, trusted-network only. **mTLS is not supported and not planned** — put NetworkPolicy or a mesh sidecar in front if you need transport identity. Non-peer gRPC still uses process `api_key` / agents when configured. Do not expose peer gRPC on the public internet.

## `[proxy.socket_tuning]`

OS-level TCP options for server and upstream connections.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `tcp_nodelay` | `bool` | `true` | Disable Nagle's algorithm |
| `tcp_keepalive_secs` | `u64` | `60` | Keepalive idle time |
| `tcp_keepalive_interval` | `u64` | `15` | Keepalive probe interval |
| `tcp_keepalive_probes` | `u32` | `5` | Probes before declaring dead |
| `listen_backlog` | `u32` | `4096` | Listen queue depth |
| `send_buffer_size` | `usize` | OS default | Send buffer (omit for autotuning) |
| `recv_buffer_size` | `usize` | OS default | Receive buffer (omit for autotuning) |
| `defer_accept_secs` | `i32` | `5` | TCP_DEFER_ACCEPT (Linux only) |
| `user_timeout_ms` | `u32` | `30000` | TCP_USER_TIMEOUT (Linux only) |
| `reuse_port` | `bool` | `true` | SO_REUSEPORT for kernel load balancing |
| `upstream_pool_idle_timeout_secs` | `u64` | `90` | Upstream idle connection timeout |
| `upstream_pool_max_idle` | `usize` | `32` | Max idle connections per host |

## `[proxy.federated]`

Federated search with local-first results and remote fallback.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Enable federated search |
| `min_local_results` | `usize` | `3` | Minimum local results before fallback |
| `min_local_confidence` | `f32` | `0.7` | Minimum score for local results |
| `fallback_on_empty` | `bool` | `true` | Fallback on zero local results |
| `fallback_on_low_confidence` | `bool` | `true` | Fallback on low scores |
| `merge_mode` | `string` | `"local_only_fallback"` | Merge strategy (see below) |
| `max_merged_results` | `usize` | `10` | Max results after merging |

**Merge modes:**
- `local_only_fallback` — Use local results; only query remote if local is insufficient
- `local_priority` — Merge with local results ranked first
- `remote_priority` — Merge with remote results ranked first
- `interleave` — Interleave local and remote results

---

## `[search]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `use_hybrid` | `bool` | `false` | Enable hybrid search |
| `default_limit` | `usize` | `10` | Default result limit |
| `auto_index_on_install` | `bool` | `false` | Auto-index on package install |

### `[search.federated]`

Same fields as `[proxy.federated]`.

## `[embedding]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `model_path` | `path` | `~/.conproxy/models/all-MiniLM-L6-v2/model.onnx` | ONNX model path (used when `provider = "onnx"`) |
| `tokenizer_path` | `path` | `~/.conproxy/models/all-MiniLM-L6-v2/tokenizer.json` | Tokenizer path (used when `provider = "onnx"`) |
| `batch_size` | `usize` | `32` | Embedding batch size |
| `provider` | `string` | `"onnx"` | Embedder provider: `onnx`, `openai`, `cohere`, or `huggingface`. `onnx` requires the `embed` feature; the others require `embed-api`. |
| `api_key` | `string` | none | API key for the selected provider. Supports `${ENV_VAR}` references that are resolved at startup (e.g. `${OPENAI_API_KEY}`). Not required for `onnx`. |
| `base_url` | `string` | provider default | Override the API base URL. Defaults: OpenAI `https://api.openai.com`, Cohere `https://api.cohere.com`, HuggingFace `https://api-inference.huggingface.co`. |

## `[context]`

OS page cache warming configuration.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `paths` | `string[]` | `["packages/**/*.md"]` | Glob patterns for files to warm |
| `warm_interval` | `u64` | `300` | Seconds between warming cycles |
| `warm_limit` | `usize` | `1000` | Max files to warm |

## `[web]`

Web content serving configuration. Not consumed by the proxy runtime; used by external tooling that reads the conproxy config directory.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `auto_index` | `bool` | `false` | Enable directory auto-indexing for web content |
| `content_dir` | `string` | `"web"` | Web content directory name (resolved under the conproxy config dir) |

## `[packages]`

External package sources. Each entry is a `[packages.<name>]` table. When the local config defines a non-empty packages map, it replaces the global one; otherwise the global map is used.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `git` | `string` | required | Git repository URL |
| `tag` | `string` | none | Git tag or ref to check out |

Example:

```toml
[packages.my-pkg]
git = "https://github.com/user/repo"
tag = "v1.0"
```

## `[registries]`

Package registry URLs. A flat map of `name = "url"` entries. Merged additively: local entries are added to global entries, with local values winning on key conflicts.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `<name>` | `string` | none | Registry URL indexed by name |

Example:

```toml
[registries]
myregistry = "https://registry.example.com"
```



## Environment Variable Overrides

The proxy supports environment-based configuration overrides via `apply_env_overrides()` on `ProxyConfig`. This allows production deployments to adjust settings without modifying config files. Environment variables take precedence over both global and local config values.

## Config File Locations

| Path | Purpose |
|------|---------|
| `~/.conproxy/conproxy.toml` | Global defaults |
| `.conproxy/conproxy.toml` | Project-local overrides |

Local config is merged on top of global config. Local takes precedence for all fields except registries (which merge additively).
