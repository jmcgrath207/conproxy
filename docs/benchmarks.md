# Benchmarks

Measured numbers from `hitrate_bench`, `perf_summarize`, and live runs. All
results are reproducible via the `make bench-*` targets listed in each
section. Methodology + caveats live in
[`docs/strategy-assessment.md`](strategy-assessment.md) §3.

## Cache effectiveness

Agentic workload (100 tasks × 4 agents × 20 retrieval calls = 8 000 queries)
against a real running proxy + docker qdrant:

| Metric | Value | Notes |
|--------|-------|-------|
| Exact hit rate | **89.5%** | matches synthetic model (95.6%) — harness validated |
| Hit p50 latency | **0.1 ms** | cache lookup, in-process |
| Miss p50 latency | **13.8 ms** | ONNX embed + qdrant search |
| Hit/miss ratio | **~138×** | at the median |

Zipf workload (10 000 unique, exponent s=1.0, 100 000 queries):

| Metric | Value |
|--------|-------|
| Exact hit rate at cache 1 000 | **68.5%** |
| Exact hit rate at cache 10 000 | **87.3%** |

**Reproduce:**

```bash
make bench-hitrate          # exact tier (default)
make bench-hitrate-live     # real proxy + docker qdrant
make bench-hitrate-onnx     # real ONNX embedder (MiniLM)
```

## Semantic cache (τ-frontier + false-hit)

Synthetic near-orthogonal embedder (validated decision machinery, not
production embedder quality — see strategy doc §3 for the ONNX result with
real MiniLM vectors):

| Workload | Best valid τ | Combined HR | False-hit | Uplift over exact |
|----------|--------------|-------------|-----------|-------------------|
| Agentic | 0.90 | 90.8% | 0.53% | +14.1 pp |
| Zipf (diverse) | 0.95 | +4.2 pp | 0.00% | +4.2 pp |

The trust cliff: τ ≤ 0.85 → 34–88% false-hit. Shipped default `τ = 0.92`
sits inside the valid band — the only published-frontier confirmation
of that default we are aware of.

**Reproduce:**

```bash
make bench-hitrate-sem
```

## Cache correctness

No live CDC against an external change stream yet. What the harness does
measure is the *no-CDC* worst case, with a what-if CDC model:

| Setup | Stale hits | Stale rate | Notes |
|-------|-----------:|-----------:|-------|
| TTL 600 s, mutation 5e-4 | 1 906 | — | heals only at TTL expiry |
| TTL 3 600 s, mutation 5e-4 | 11 044 | — | longer freshness window = more stale |
| Same + `--cdc-delay 30` | **218** | **−96.8%** | what-if CDC invalidation |
| Same + `--cdc-delay 30` exact HR | unchanged | — | healed entries re-cache |

**Reproduce:**

```bash
make bench-hitrate
# pass --ttl 600 --mutation-rate 0.0005 etc.
```

## Microbenchmarks (Criterion)

`cargo bench --bench core_ops` (cache, query, serde, slugify) and
`--bench cascade_scope` (RRF fusion, scope scoring) — baseline drift
gated at 15% with a 95% CI from independent-sample SE, chronic-regression
detector across published runs, trend line per run.

**Reproduce + gate:**

```bash
make perf-tuning-quick       # benches + metrics + ANALYSIS
make bench-save              # refresh baseline on clean HEAD
make perf-publish            # archive to perf-history/<ts>-<sha8>/
```

## What the numbers don't claim

- **No cost numbers.** Pricing for managed vector DB read units, remote
  embedder tokens, and reranker APIs changes too fast to publish. Cost
  breakevens in the strategy doc are approximate, not measured.
- **No latency vs no-cache comparison on production traffic.** The 138×
  ratio is the proxy cache lookup vs. ONNX-embed + qdrant search; your
  real backend may differ.
- **No multi-tenant isolation numbers.** Context isolation adds bookkeeping
  cost; not yet benchmarked.
