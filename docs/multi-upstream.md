# Multi-Upstream Guide

Conproxy can route queries across multiple search backends simultaneously. This guide covers upstream types, cascade queries, federated search, and the discovery tool.

## Upstream Types

Each upstream has a type that determines how conproxy communicates with it and how scores are normalized:

| Type | Examples | Score Range | Query Mode | Status |
|------|----------|-------------|------------|--------|
| **Full-Text Search** | Elasticsearch, OpenSearch, Meilisearch | 0–1 (`_rankingScore` for Meili, BM25 for ES/OS) | `text_native` | Shipped |
| **Vector Database** | Qdrant | 0–1 (cosine) | `text_native` or `vector_only` | Shipped |
| **Vector Database** | Pinecone, Milvus | 0–1 (cosine) | `vector_only` | Shipped |
| **pgvector** | PostgreSQL + pgvector | 0–1 (distance) | `vector_only` | Shipped (`pgvector` feature) |
| **Hybrid** | ES with kNN | Mixed | varies | Shipped (via ES adapter) |

Solr is **removed** (no adapter; `upstream_type = "solr"` is rejected at config validation).

Set the type explicitly for best results:

```toml
[upstreams.qdrant-primary]
url = "http://localhost:6333"
type = "qdrant"
query_mode = "text_native"

[upstreams.es-secondary]
url = "http://localhost:9200"
type = "elasticsearch"
query_mode = "text_native"
index = "documents"
search_fields = ["title", "content"]

[contexts.default]
default = true

[[contexts.default.upstreams]]
ref = "qdrant-primary"

[[contexts.default.upstreams]]
ref = "es-secondary"
```

### Query modes

- **`text_native`** — The upstream accepts text queries directly (most engines)
- **`vector_only`** — The proxy must embed the query into a vector before sending (pgvector, Milvus). Requires the `embed` feature
- **`unknown`** — Auto-detected on first request

## Configuring Multiple Upstreams

### Weight and priority

Each leg has a `weight` (for load balancing within a priority group) and a `priority` (for cascade ordering). The resource is shared; per-context overrides decide how this context uses it:

```toml
[upstreams.qdrant-fast]
url = "http://qdrant-1:6333"
type = "qdrant"

[upstreams.qdrant-warm]
url = "http://qdrant-2:6333"
type = "qdrant"

[upstreams.es-fallback]
url = "http://es:9200"
type = "elasticsearch"

[contexts.default]
default = true

[[contexts.default.upstreams]]
ref = "qdrant-fast"
weight = 3        # Gets 3x the traffic
priority = 0      # Tried first in cascade

[[contexts.default.upstreams]]
ref = "qdrant-warm"
weight = 1        # Gets 1x the traffic
priority = 0      # Same priority = load balanced

[[contexts.default.upstreams]]
ref = "es-fallback"
weight = 1
priority = 1      # Tried second (cascade fallback)
```

- Upstreams with the same priority are load-balanced by weight
- Lower priority number = tried first in cascade

### Concurrency limits

Protect upstreams from overload (set on the resource, applies to all contexts using it):

```toml
[upstreams.small-instance]
url = "http://localhost:6333"
type = "qdrant"
max_concurrent = 20   # Max 20 simultaneous requests
```

### Version polling

Detect upstream changes by polling a version endpoint:

```toml
[upstreams.my-qdrant]
url = "http://localhost:6333"
type = "qdrant"
version_endpoint = "/v1/version"
version_poll_interval_secs = 60
```

## Priority-Based Cascade

The cascade executor tries upstreams in priority order. If the first upstream's results don't meet the quality threshold, the next upstream is tried. Cascade lives on the context:

```toml
[contexts.default]
default = true

[contexts.default.cascade]
enabled = true
min_score_threshold = 0.7   # Normalized 0-1 score threshold
min_results = 1             # Minimum results needed
max_cascade_depth = 3       # Max upstreams to try
merge_cascade_results = false  # Merge or replace
cascade_timeout_ms = 30000  # Total cascade timeout
fusion_method = "rrf"       # "none" (default) or "rrf" for equal-priority fusion
rrf_k = 60                  # RRF constant (only used when fusion_method = "rrf")
```

### How cascade works

1. Query goes to the highest-priority (lowest number) upstream
2. If results meet the threshold (`min_score_threshold` and `min_results`), return them
3. Otherwise, try the next upstream in priority order
4. Continue until threshold is met, max depth is reached, or all upstreams are exhausted

### Stop reasons

The cascade stops when:
- **ThresholdMet** — Results meet the score threshold
- **MinResultsMet** — Enough results returned
- **MaxDepthReached** — Tried `max_cascade_depth` upstreams
- **AllExhausted** — No more upstreams to try
- **Timeout** — `cascade_timeout_ms` exceeded

### Result merging

With `merge_cascade_results = true`, results from all tried upstreams are combined and sorted by normalized score. With `false` (default), only the results from the last successful upstream are returned.

### Reciprocal Rank Fusion (RRF)

When multiple upstreams share the same priority, set `fusion_method = "rrf"` to query them in parallel and merge results with [Reciprocal Rank Fusion](https://en.wikipedia.org/wiki/Reciprocal_rank_fusion):

```toml
[contexts.default.cascade]
fusion_method = "rrf"
rrf_k = 60   # Standard k value; higher = more weight to top results
```

RRF score for document `d` is: `sum_l 1.0 / (k + rank_l(d))` where `rank_l(d)` is the 1-based position in list `l`. Documents are deduplicated by `blake3(content)` so the same content from different upstreams merges into one entry.

**Behavior:**

- **Within a priority group**: All upstreams run concurrently; their results are fused by RRF score.
- **Across priority groups**: The cascade still proceeds to lower-priority groups if the RRF-merged result doesn't meet `min_score_threshold` and `min_results`.
- **Single-upstream groups**: RRF is a no-op (nothing to merge).
- **No upstream meets the threshold**: Returns the best RRF-merged result from the highest-priority group (or empty if all errored).

This is useful when you have multiple backends of the same type (e.g. two Qdrant clusters, or ES + OpenSearch) and want the best result from any of them, weighted by rank position.

## Score Normalization

Different upstream types return scores in different ranges. Conproxy normalizes all scores to the 0–1 range for fair comparison during cascade:

| Upstream Type | Raw Range | Normalization |
|---------------|-----------|---------------|
| Full-Text Search (BM25) | 0–100+ | Divided by max observed score |
| Vector Database (cosine) | 0–1 | Already normalized |
| Hybrid | Mixed | Per-source normalization |

This ensures a cascade from Elasticsearch (BM25 scores in the hundreds) to Qdrant (cosine similarity 0–1) works correctly.

## Federated Search

Federated search combines local cache results with remote upstream results. Unlike cascade (which tries upstreams sequentially), federated search merges results from multiple sources. Federated policy is on the context:

```toml
[contexts.default.federated]
enabled = true
min_local_results = 3          # Local results needed before "good enough"
min_local_confidence = 0.7     # Minimum local score
fallback_on_empty = true       # Query remote on zero local results
fallback_on_low_confidence = true
merge_mode = "local_priority"  # How to combine results
max_merged_results = 10
```

### Merge modes

| Mode | Behavior |
|------|----------|
| `local_only_fallback` | Use local; only query remote if local is insufficient |
| `local_priority` | Merge with local results ranked first |
| `remote_priority` | Merge with remote results ranked first |
| `interleave` | Alternate local and remote results |

### When to use federated vs cascade

- **Cascade**: When you have a preferred upstream and want a fallback chain
- **Federated**: When you want to combine results from cache and live upstream, or merge local and remote search systems

## Discovery

The `conproxy discover` CLI was removed in plan 02 (periphery cut). To choose upstream
shape, use the context-rooted examples and dry-run knobs with the MCP **tune** suite
(`cascade_tune`, `federated_tune`) — that is the supported way to score leg/parameter
choices for an existing context. Runtime query-mode probing (`discover_query_mode` on
each adapter) is unchanged.

## Load Balancing and Failover

Within a priority group, requests are distributed by weight:

```
Priority 0: qdrant-1 (weight=3), qdrant-2 (weight=1)
  → qdrant-1 gets 75% of traffic, qdrant-2 gets 25%

Priority 1: es-fallback (weight=1)
  → Only used if all priority-0 upstreams fail cascade threshold
```

### Automatic failover

When an upstream becomes unhealthy (via the resilience state machine), it is automatically removed from the active pool:

- **Healthy** → **Degraded**: Rate-limited traffic, retries enabled
- **Degraded** → **Offline**: No traffic, periodic probes
- **Offline** → **Degraded**: Probe succeeds, gradually restore traffic

Traffic shifts to remaining healthy upstreams. When the upstream recovers, it is gradually reintroduced with rate-limited traffic.

### Targeting specific upstreams

Clients can bypass cascade and route to a specific upstream:

```json
{
  "query": "my search",
  "upstream_id": "qdrant-primary"
}
```

Or prefer a type:

```json
{
  "query": "my search",
  "upstream_type": "elasticsearch"
}
```

## Observability

Scrapeable on `/metrics` (Prometheus). The cascade series most operators want to
alert on:

| Series | What it tells you |
|--------|-------------------|
| `proxy_cascade_success_total` | Queries that met the cascade threshold and returned |
| `proxy_cascade_exhausted_total` | Queries that ran all legs and still didn't meet threshold |
| `proxy_cascade_timeout_total` | Queries dropped by `cascade_timeout_ms` |
| `proxy_cascade_avg_depth` | Average legs tried per query (rising = first leg quality drop) |
| `proxy_per_upstream_*` (per leg) | Latency, errors, success rate per leg |

Rule of thumb: a stable `cascade_avg_depth` near `1.0` means the priority-0 leg is
doing all the work. A rising `cascade_exhausted_total` rate means either the threshold
is too aggressive or the backend is degraded — check `proxy_per_upstream_*` for the
offending leg.

Federated search currently exposes per-query stats on the response body
(`stats.local_count`, `stats.remote_queried`, `stats.fallback_reason`) rather than
aggregate Prometheus counters. If you need cross-query aggregation, fold
`proxy_federated_*` into a follow-up plan; do not assume the series exist.
