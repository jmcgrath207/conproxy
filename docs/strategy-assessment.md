# conproxy Strategy Assessment

Date: 2026-07-24. Status: internal strategy doc, opinionated.

> Snapshot document. Product numbers (test counts, tool counts, feature
> counts) reflect the codebase **as of 2026-07-24** and may drift in
> later releases. Verify against `cargo test --lib` (test count),
> `rg -c '#\[tool' src/mcp/mod.rs` (MCP tool count), and `Cargo.toml`
> `[features]` (feature count) before quoting externally.

Three parts:

1. **Assessment** — is conproxy helpful or misguided?
2. **Hit-rate math** — the models that decide whether the product works.
3. **Agentic hit-rate benchmark design** — how to prove/disprove the core bet.

---

## 1. Assessment

### 1.1 What conproxy is

Cache proxy in front of heterogeneous RAG/vector backends (Elasticsearch,
OpenSearch, Qdrant, pgvector, Meilisearch, Pinecone, Milvus). Exact+semantic
query cache, multi-backend cascade with RRF fusion, scope filter, coalesce,
CDC, peer mesh. MCP server with a dry-run tune suite (search + 10 tune
tools + 10 dashboard status tools — see README "One endpoint" list).
Rust, ~1800 tests, 14 test verticals, CI-aware perf toolchain.

### 1.2 Market facts (verified live 2026-07-24)

- **GPTCache** (zilliztech, ~8.1k stars): semantic cache for **LLM responses**
  (question→answer). Feature list is 2023-era (llama.cpp, dolly, minigpt4;
  Anthropic still unchecked). Caches the generation leg, not retrieval.
- **RedisVL SemanticCache**: same scope — "reduces requests and tokens sent
  to the LLM." LLM-response cache again.
- **Nobody dominant caches the retrieval leg.** Vector-DB query-result
  caching as a standalone proxy is a genuinely thin spot. The core thesis —
  "cache the retrieval leg" — targets an unoccupied shelf.

(Remaining landscape reads are training knowledge, not live data.)

### 1.3 Strengths

1. **Real gap, right layer.** LLM caches are crowded; retrieval caches are
   not.
2. **Embedding + managed-backend cost story.** Remote embedders charge per
   token; serverless vector DBs charge per read unit. Cache hits avoid both.
3. **MCP-native distribution.** One search tool for any backend,
   agent-consumable. Per-backend MCP servers are fragmented; MCP is the hot
   channel.
4. **Agentic workloads fix the hit-rate problem.** The classic objection to
   query caching is that natural-language query space is huge → low
   exact-match hit rates. But LLM agents issue repeated and near-duplicate
   retrieval constantly: retry loops, multi-agent fanout, tool-call storms,
   multi-turn re-grounding. High hit rates + coalesce collapses concurrent
   duplicates. This is the killer use case.
5. **Federation/RRF is real.** Migrations, org mergers, lexical+vector
   hybrid. Nobody does this as a proxy.
6. **Tune suite is differentiated.** "Measure, don't guess" — empirical
   scope/cascade/cache tuning is consulting-grade tooling nobody ships.
7. **Engineering rigor.** Tests, CI-aware benching, profiling verticals —
   enterprise-trust grade.

### 1.4 Weaknesses / misguided risks

1. **Latency leverage is small in classic RAG.** Retrieval = 5–50 ms;
   generation = seconds. If the headline is "faster RAG," the math is weak
   for chat UX. (It is NOT weak for agents — see §2.4.)
2. **Proxy tax.** Extra hop on every miss. Need
   `hit_rate × saved_cost > hop + ops_burden`. In-process caches don't pay
   that tax.
3. **Cache correctness = trust cliff.** Stale results = silently wrong
   answers. TTL+CDC+LWW-peer invalidation must be bulletproof and provable.
4. **Backend heterogeneity is niche.** Most prod teams standardized on one
   store. Multi-backend = migrations + big orgs. Real but narrow.
5. **Feature sprawl before PMF.** 13 features (incl. meta + dev/test
   groups), ~20 MCP tools, peer mesh, CDC, ~321K-line test_runner, ~130
   Makefile targets. Large surface relative to validated product signal.
6. **Semantic cache is double-edged.** Threshold caching raises hit rate but
   risks semantically-wrong hits. Scope filter + embed band is the right
   mitigation and the hardest thing to explain to a skeptical buyer.

### 1.5 Verdict

**Not misguided — but the pitch is probably wrong.**

- Wrong pitch: "faster vector search." Retrieval latency doesn't matter
  enough in classic RAG.
- Right pitch: "cut embedding + managed-backend cost for agentic RAG, and
  give agents one MCP search endpoint across any backend — with measured
  hit rates, not vibes."

Helpful for developers **if** it leads with cost + agent workloads + MCP
distribution, and **if** hit-rate claims get published benchmarks on
realistic agentic traces. Becomes misguided only if it keeps selling the
latency story and keeps adding infra features before the core bet —
"agents re-query enough that caching pays" — is proven with numbers.

### 1.6 Recommendations

| # | Action | Why |
|---|--------|-----|
| 1 | Re-headline: cost + agents, not latency | Latency leg too small to sell in classic RAG |
| 2 | Publish hit-rate benchmark on agentic traces | The one number that decides if the product works (§3) |
| 3 | Tune suite as the wedge: "measure YOUR hit rate in 10 min" | Converts skeptics; already built |
| 4 | Freeze peer-mesh/CDC-class features until PMF signal | Complexity budget already high |
| 5 | Write invalidation/correctness doc like a compliance artifact | Trust cliff is the #1 enterprise objection |

---

## 2. Hit-rate math

The product lives or dies on one inequality:

```
benefit = H × saved_per_hit  >  overhead_per_request + ops_cost
```

H = cache hit rate. Everything below estimates H and the break-evens.

### 2.1 Model 1 — exact-match cache, Zipf popularity

Natural query popularity follows Zipf: the i-th most popular query has
probability `p_i = i^(-s) / H_{N,s}` over N unique queries, exponent s.
A cache holding the top-k queries gets hit rate:

```
H(k) = H_{k,s} / H_{N,s}      (H_{n,s} = generalized harmonic number)
```

Asymptotics: for s = 1, H ≈ ln(k)/ln(N); for s < 1, H ≈ (k/N)^(1-s).

Worked table — N = 10,000 unique queries, cache k = 1,000 hottest:

| Zipf s | Workload shape | Hit rate |
|--------|----------------|----------|
| 1.2 | Spiky FAQ/support | ~78% |
| 1.0 | Classic FAQ | ~75% |
| 0.8 | Doc search, repeat-ish | ~63% |
| 0.6 | Open search, diverse | ~40% |
| 0.4 | Near-flat, adversarial | ~25% |

Takeaway: even moderately flat distributions give viable hit rates with a
reasonably sized cache. The FAQ/support/doc-search pattern is squarely in
the 60–80% band.

### 2.2 Model 2 — agentic repetition (the core bet)

Agents re-query. Two sub-models:

**Within-task repetition.** A task issues M retrieval calls, U unique
(U ≤ M). With a warm cache and TTL longer than the task:

```
H = 1 − U/M
```

Example: agent does 20 retrieval calls per task, 8 unique → H = 60%.

**Multi-agent fanout.** A agents each issue m queries drawn from a shared
sub-query pool of size P (uniform). Expected unique queries after all
agents:

```
E[unique] = P × (1 − (1 − 1/P)^(A·m))
H = 1 − E[unique] / (A·m)
```

Example: P = 50, m = 10, A = 4 → E[unique] ≈ 22.5, total 40 → H ≈ 44%.

Combined with within-task repetition, agentic workloads plausibly sit in
the **40–70%** exact-match band — before semantic matching adds anything.
This is the number the benchmark (§3) must measure, not assert.

**Coalesce bonus.** Concurrent duplicate queries collapse to one backend
call. Under fanout bursts, effective backend load drops by a further
dedup factor not captured in H.

### 2.3 Model 3 — semantic threshold cache

Queries arrive in C semantic clusters; within-cluster pairs exceed
similarity threshold τ. First query of each cluster misses, rest hit:

```
H ≈ 1 − C/Q        (Q = total queries)
```

Cluster count C grows sub-linearly with Q for bounded domains (support
topics, product docs). The knob is τ:

- τ too low → false hits (wrong-cluster matches) → correctness violation.
- τ too high → H collapses toward exact-match.

The interesting product surface is the **H vs false-hit-rate frontier** as
τ sweeps. Scope filter + embed band is conproxy's mechanism for pushing
that frontier outward. Benchmark must plot it, not pick a single τ.

### 2.4 Break-evens

**Latency (classic RAG).** Let t_lookup ≈ 1 ms, t_embed, t_backend:

```
H* = t_lookup / (t_embed + t_backend)
```

- Remote embedder (30 ms) + typical backend (20 ms): H* ≈ 2%. Trivial.
- Local embedder (1 ms) + fast backend (5 ms): H* ≈ 17%. Still easy.

**Latency (agentic amplification).** Agents retrieve M times, often
serially. Saved task time:

```
ΔT = M × H × (t_embed + t_backend)
```

M = 20, H = 60%, 40 ms saved per hit → ~480 ms off every task. Agents run
thousands of tasks/hour; this is the throughput story that classic-RAG
latency math misses.

**Cost.** Honest math (approximate public pricing, verify before
publishing):

| Saved item | Rough unit cost | 10M queries/mo, H = 50% | 1B queries/mo, H = 50% |
|------------|-----------------|--------------------------|-------------------------|
| Embedding (API, ~15 tok/query) | ~$0.13/1M tok | ~$10/mo | ~$1,000/mo |
| Managed vector DB read units (serverless) | ~$16/1M RU (order-of-magnitude) | ~$80/mo | ~$8,000/mo |
| Reranker API (if in path) | ~$1/1k searches | ~$5,000/mo | ~$500,000/mo |
| Self-hosted backend node-hours | capacity-dependent | tail-latency headroom | real capacity savings |

Conclusion: pure embedding savings are pocket change at small scale. The
cost story needs **scale, expensive read units, or rerankers in the path**.
The agentic amplification story (§2.4) works at any scale.

### 2.5 What decides the product

1. Agentic H ≥ ~40% on realistic traces → product works.
2. False-hit rate ≤ ~1% at that operating point → product is trustworthy.
3. Net saved (cost + task time) > ops burden → product is adoptable.

All three are measurable. That is the benchmark.

---

## 3. Agentic hit-rate benchmark design

Working name: `bench-hitrate`. Purpose: produce the publishable number —
measured hit rates on realistic agentic traces, with correctness gates.

**Status: v5 implemented** (`src/bin/hitrate_bench.rs`,
`make bench-hitrate` / `bench-hitrate-sem` / `bench-hitrate-onnx` /
`bench-hitrate-live`). As-built semantics:

- **Measured:** exact hit rate against the real `CacheStore` (with
  virtual-clock TTL sweep, `--ttl`); semantic hit rate + false-hit rate
  against the real `SemanticCache` tier (feature `embed-api`), driven by
  three embedders (`--embedder`): synthetic near-orthogonal (default),
  **onnx** (live all-MiniLM-L6-v2 via prod `Embedder`, feature `embed`),
  or **api** (openai/cohere/huggingface via prod providers). Latency/cost/
  task-time are parameterized models, not measurements.
- **Measured v2 result (seed 42, synthetic embedder):** agentic trace —
  best valid τ = 0.90, combined HR 90.8%, +14.1pp uplift over exact-only,
  false-hit 0.53% ≤ 1% gate; τ = 0.95 → 0.00% false. Zipf trace — best
  valid τ = 0.95, +4.2pp. The frontier shows the predicted trust cliff:
  τ ≤ 0.85 → 34–88% false-hit. **Prod default τ = 0.92 sits inside the
  valid band** — independent confirmation of the shipped default.
- **Measured v3 result (ONNX all-MiniLM-L6-v2, SEM_MAX=20k):** agentic —
  combined HR 96.5–99.7% across τ, but **no τ ≤ 0.95 clears the 1%
  false-hit gate** (τ = 0.95 → 6.8% false). Root cause: the synthetic
  50-word vocabulary makes neighbor space artificially dense — word-salad
  queries are pathological for real embedders. This is a *workload realism*
  finding, not a prod τ finding: de-risking path is `replay` mode with real
  query logs (MS MARCO/ORCAS adapter, still deferred).
- **PROD BUG FOUND + FIXED (v3):** prod ONNX `Embedder` bound
  `session.run` inputs positionally as (input_ids, token_type_ids,
  attention_mask) while the model declares (input_ids, attention_mask,
  token_type_ids) — attention_mask became all zeros → every embedding
  collapsed (all-pairs cosine ≈ 0.93–1.0). Existing tests checked dims +
  batch consistency only, so it shipped. Fixed both `embed` and
  `embed_batch`; added `test_embed_semantic_geometry` regression (model-
  gated) asserting paraphrase_sim > unrelated_sim + 0.3. Verified against
  Python onnxruntime ground truth on the same model file. Host note: link
  via `ORT_LIB_LOCATION=/usr/local/lib ORT_PREFER_DYNAMIC_LINK=1` (wired
  into `make bench-hitrate-onnx`).
- **Real dynamic surfaced (v2):** semantic hits suppress exact-key
  insertion — exact HR drops inside semantic runs; combined HR still nets
  positive. Worth documenting for capacity planning.
- **Replay adapter + stale model (v4):** `--queries-file` feeds real query
  texts (MS MARCO / ORCAS) into the zipf/agentic generators in place of
  synthetic salads — one query per line, TSV rows take the second field
  (`qid\tquery[\t...]`), pool wraps modulo. Conversion recipes:
  MS MARCO `queries.dev.tsv` works as-is; ORCAS `cut -f2 orcas.tsv > q.txt`.
  `--mutation-rate P` adds document mutation: a random already-seen cluster
  mutates per event with prob P; hits on entries inserted before their
  cluster's last mutation count as **stale hits** (entry heals only at TTL
  expiry — no-CDC worst case; the TTL grid shows how TTL bounds staleness).
  Verified: zipf 50k, cache 500, mutation 5e-4 — TTL 600s → 1,906 stale,
  TTL 3600s → 11,044 stale. `--cdc-delay SECS` adds the what-if CDC model:
  mutations enqueue an invalidation that fires after the delay; healed
  entries miss once and re-cache. Verified: same run, TTL 3600s — 6,735
  stale → **218 with 30s CDC (96.8% reduction)**, exact HR unchanged.
  `--embed-provider mock` live-tests the API provider wire path (local
  OpenAI-compatible mock server, no keys needed).
- **Live mode (v5):** `--live URL` replays workloads against a real
  running proxy over HTTP (`POST /query`, real `cache_status`), with
  `--live-seed` seeding a docker qdrant via the bench's ONNX embedder,
  `--live-mutate P` driving payload version bumps, and `--live-evict`
  calling the evict API (simulated external CDC). Orchestrated by
  `make bench-hitrate-live` (docker qdrant on 16333, profiling-profile
  proxy with `[proxy.embedding] provider="onnx"` + context-rooted config,
  automatic teardown). **Measured (seed 42, twice deterministic):** zipf
  5k → **66.6% hit HR** (synthetic model: 68.5%); agentic 8k → **89.5%
  hit HR** (synthetic: 95.6%); hit p50 **0.1ms** / p99 0.3ms vs miss p50
  **13.8ms** (~138× ratio on the real wire); stale-content detection works
  end-to-end (35 caught under mutation); errors 0; verdict PASS.
  The synthetic harness predictions hold up against the real wire.
- **Deferred:** CDC-live stale invalidation against a real external change
  stream (live mode currently simulates CDC via explicit evict calls);
  hosted-API embedder runs with real keys (mock covers the wire path).
- Verdicts: `FAIL-CORE` (exit 2) if agentic exact hit rate misses the gate
  (default 40%); `FAIL-TRUST` (exit 3) if semantic mode runs and no τ
  clears the false-hit gate (default 1%) with positive uplift; `--no-fail`
  for report-only. Deterministic via `--seed`. Results render into
  `index.html` via `test_runner index`.

The subsections below are the full design; remaining roadmap items marked
accordingly.

### 3.1 Trace generators

| Generator | Parameters | What it models |
|-----------|-----------|----------------|
| `zipf` | N unique, s, length | FAQ / doc-search popularity curves (§2.1) |
| `agentic` | tasks, M calls/task, requery prob r, agents A, pool P | Within-task repetition + multi-agent fanout (§2.2) |
| `paraphrase` | wraps any trace, transform rate ρ | Near-duplicates for semantic mode (§2.3): synonym swap, clause reorder, question→statement |
| `replay` | JSONL file | Real traces: MS MARCO / ORCAS query logs, or captured production logs |

Trace record format (JSONL):

```json
{"ts": 0.0, "query": "reset password", "session_id": "s-42", "agent_id": "a-1", "cluster_id": "c-7"}
```

`cluster_id` is ground truth for false-hit measurement: a semantic hit is
correct only if the matched cached entry shares the cluster.

### 3.2 Harness

New small binary `src/bin/hitrate_bench.rs` (or test_runner subcommand —
decide at implementation):

1. Boot proxy with mock upstream (deterministic latency simulator: embed
   X ms, backend Y ms, pure Rust, no Docker) → CI-fast mode.
2. Live-upstream mode (qdrant testcontainer) → nightly validation mode.
3. Replay trace at configurable QPS; record per-request: hit/miss,
   hit kind (exact/semantic), latency, matched cluster.
4. Sweep grid: cache max entries × TTL × τ × scope on/off.
5. Emit `tests/results/hitrate/<ts>/summary.json` + `SUMMARY.md` +
   `frontier.json` (H vs false-hit vs τ), consumed by
   `test_runner index` like perf-tuning's report_criterion.

Make target: `make bench-hitrate` (mock mode), `make bench-hitrate-live`.

### 3.3 Metrics

| Metric | Definition | Gate (initial) |
|--------|-----------|----------------|
| Exact hit rate | exact hits / total | agentic ≥ 40% |
| Semantic hit rate | semantic hits / total (per τ) | report frontier |
| **False-hit rate** | semantic hits with wrong cluster / semantic hits | ≤ 1% at chosen τ |
| Stale rate | hits served past backend-truth change (CDC ground truth) | ≤ 0.1% with CDC on |
| p50/p99 latency | hit vs miss path | hit p99 ≤ 5 ms mock |
| Cost saved | $/1k queries (embed + RU model, §2.4) | report |
| Task time saved | M × H × saved-per-hit, per agentic trace | ≥ 20% of retrieval time |
| Coalesce dedup | collapsed concurrent duplicates | report |

### 3.4 Verdict logic (like perf_summarize)

- `PASS`: all gates met on agentic + zipf traces.
- `FAIL-CORE`: agentic H < 40% → core bet wrong, pivot pitch.
- `FAIL-TRUST`: false-hit > 1% at every viable τ → semantic mode needs
  rework (scope band tightening) before marketing it.
- Exit codes + JSON mirror perf_summarize conventions (`--fail-on-*`).

### 3.5 Deliverables

1. Trace generators + JSONL replay (synthetic first; MS MARCO/ORCAS
   adapter second).
2. Mock upstream with deterministic latency.
3. Sweep runner + summary/frontier JSON + index.html integration.
4. Published numbers doc: hit rates per workload, frontier plot, break-even
   table — the marketing artifact this strategy needs.

### 3.6 Non-goals / risks

- Not a load test (e2e_load owns throughput); this measures cache
  effectiveness, not RPS.
- Synthetic traces are hypotheses; replay mode against real query logs is
  the follow-up that de-risks them.
- False-hit ground truth depends on cluster labeling quality — paraphrase
  generator must be conservative (only transforms known to preserve
  meaning).

---

## Appendix — positioning one-liner

> conproxy: the retrieval cache for agentic RAG. One MCP endpoint, any
> backend, measured hit rates. Agents re-query constantly — stop paying
> for it twice.
