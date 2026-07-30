# Synthetic Corpus

Topic-clustered, synthetic corpus for conproxy backend seeding and benchmarking.

## Structure

```
data/
├── docs.jsonl      # 100 synthetic docs (20 topics × 5 docs each)
├── tickets.jsonl   # 100 synthetic tickets (20 issue types × 5 each)
├── code.jsonl      # 100 code snippets (20 patterns × 5 each)
└── queries.jsonl   # 60 known-good queries (20 per corpus)
```

## Why synthetic?

Content uses **invented product names** (ZephyrDB, Lumen Cache, Quark Mesh, Nimbus Index, Vortex Queue, etc.) so it is:
- **Novel** — not on the internet, not in any LLM training set
- **Semantically coherent** — generated around fixed topic clusters, so embeddings work and search returns meaningful results
- **Bigger** — 2000-5000 chars per doc (vs. ~50-100 chars in the original inline templates)

## Why topic-clustered?

Each corpus has **20 fixed topics** (5 docs per topic). The `queries.jsonl` file has one known-good query per topic, so you always know:
- What to query (see `queries.jsonl`)
- What results to expect (each query has an `expected_topic` and `expected_min_results`)
- How to benchmark ("did tuning improve results for this query?")

## How to regenerate

The generator is at `src/bin/corpus_gen.rs` (uses `fake-rs` for seeded RNG).

```bash
cargo run --bin corpus_gen
```

This rewrites all 4 JSONL files. The seed is fixed (`[42u8; 32]`) so the output is deterministic.

## How to seed backends

```bash
cargo run --bin corpus_seed --features embed,pgvector -- \
    --corpus all \
    --corpus-dir tests/corpus/data/
```

The seed binary reads the JSONL files and loads them into qdrant/elastic/opensearch/meili/pgvector.

## How to benchmark

Use the queries from `queries.jsonl` with `search` or `benchmark`:

```bash
# Via grpcurl
grpcurl -plaintext 127.0.0.1:9999 conproxy.v1.SearchService/Query \
    -d '{"query":"cache ttl tuning","top_k":5}'

# Via MCP (in opencode)
mcp_conproxy_benchmark --session_id=... --agent_id=... \
    --context_id=... --query="cache ttl tuning" --top_k=5
```

## Topics

### Docs (20 topics)

`cascade_configuration`, `federation_vs_cascade`, `cache_ttl_tuning`, `semantic_cache_threshold`, `embedding_provider_config`, `mcp_server_integration`, `peer_replication`, `scope_filtering`, `coalesce_dedup`, `circuit_breaker`, `connection_pool`, `rate_limiting`, `context_management`, `audit_logging`, `metrics_observability`, `warmup_strategy`, `drift_detection`, `adaptive_timeout`, `federated_search`, `deployment_k8s`

### Tickets (20 issue types)

`connection_timeout`, `data_loss`, `auth_failure`, `performance_regression`, `memory_leak`, `cache_inconsistency`, `replication_divergence`, `circuit_tripping`, `rate_limit_exceeded`, `config_reload_failure`, `embedding_mismatch`, `search_quality_degradation`, `peer_sync_stall`, `health_check_failure`, `upstream_drain`, `scope_filter_regression`, `coalesce_deadlock`, `metrics_drift`, `warmup_timeout`, `cascade_exhaustion`

### Code (20 patterns)

`embed_query`, `cache_lookup`, `fuse_rrf`, `scope_filter`, `circuit_check`, `rate_limit_check`, `peer_sync`, `health_probe`, `config_reload`, `coalesce_dedup_fn`, `warmup_seed`, `drift_check`, `adaptive_timeout_fn`, `federated_merge`, `audit_log`, `pool_select`, `context_switch`, `mcp_tool_register`, `cascade_execute`, `cache_evict`

## JSONL entry format

```json
{
  "id": "docs-000",
  "title": "Configuring ZephyrDB cascade mode",
  "content": "...2000-5000 chars of generated content around the topic...",
  "category": "guides",
  "tags": ["cascade", "configuration", "priority"],
  "topic": "cascade_configuration",
  "overlap": true
}
```

```json
{
  "query": "cache ttl tuning",
  "corpus": "docs",
  "expected_topic": "cache_ttl_tuning",
  "expected_min_results": 3
}
```

The `overlap: true` flag marks the first 10 entries of each corpus — these are seeded to ALL backends for cross-backend testing. Remaining entries are unique per backend.