# Performance Analysis — PASS

**Run:** full mode | Git `cfff440` | Threshold ≥15%

**No CI-bound regressions detected.** All measured changes are within the noise band (CI crosses zero) or below the threshold. This code change is safe from a performance perspective.

## Baseline Health

- Status: ✅ `ok`
- Saved: 2026-07-25T00:13:18Z (sha `cfff440`)
- Benchmarks: 21
- Age: 0.0h

## core_ops benches (full)

| Bench | Mean | Δ | CI | Status |
|-------|-----:|--:|----|--------|
| response serialize json | 1.6µs | +9.2% | [+7.7%, +10.8%] | inconclusive |
| slugify | 28ns | +9.0% | [+6.7%, +11.4%] | inconclusive |
| cache lookup hit | 178ns | +8.1% | [+6.6%, +9.7%] | inconclusive |
| cache lookup miss | 234ns | +7.3% | [+5.3%, +9.3%] | inconclusive |
| normalize query | 61ns | +6.7% | [+5.4%, +8.0%] | inconclusive |
| cache throughput/10000 | 26.7ms | +3.3% | [+2.0%, +4.7%] | inconclusive |
| cache insert | 4.4µs | +3.1% | [-0.6%, +6.7%] | inconclusive |
| response deserialize json | 2.3µs | +2.4% | [+0.2%, +4.7%] | inconclusive |
| cache eviction pressure | 3.2µs | +2.0% | [+0.8%, +3.3%] | inconclusive |
| query hash | 140ns | -2.3% | [-3.4%, -1.1%] | inconclusive |
| query deserialize json | 146ns | -2.9% | [-5.1%, -0.6%] | inconclusive |
| cache throughput/100 | 276.5µs | -3.7% | [-4.7%, -2.7%] | inconclusive |
| cache throughput/1000 | 2.9ms | -4.3% | [-5.4%, -3.2%] | inconclusive |
| query serialize json | 91ns | -5.2% | [-7.0%, -3.3%] | inconclusive |

## cascade_scope benches (full)

| Bench | Mean | Δ | CI | Status |
|-------|-----:|--:|----|--------|
| fuse rrf/dedup 2upstreams | 3.6µs | +6.2% | [-63.4%, +75.8%] | inconclusive |
| fuse rrf/3upstreams k60 | 13.2µs | +2.8% | [-38.3%, +43.8%] | inconclusive |
| scope best sim short content | 654ns | -3.2% | [-4.1%, -2.3%] | inconclusive |
| fuse rrf/2upstreams 50each | 36.1µs | -3.6% | [-4.4%, -2.8%] | inconclusive |
| scope best sim long content | 3.0µs | -3.7% | [-4.9%, -2.4%] | inconclusive |
| scope best sim no match | 362ns | -4.5% | [-5.5%, -3.4%] | inconclusive |
| scope filter results 20items | 13.4µs | -7.4% | [-8.6%, -6.2%] | inconclusive |

## Cache effectiveness (hit-rate benchmark)

- Verdict: ✅ `PASS`
- Agentic exact HR: ✅ 95.6% (gate 40%)

| Workload | Best exact | Best combined (τ) | False-hit |
|----------|-----------:|-------------------:|----------:|
| zipf | 91.3% | — | — |
| agentic | 95.6% | — | — |

## Diagnostics

- Flamegraph: ❌ not produced
- Heaptrack: ✅ present
- Tokio console: ❌ not produced (run perf-tuning-full with TOKIO_CONSOLE=1)
- Hitrate bench: ✅ present

**Note:** 21 benches showed raw change above threshold but the 95% CI crosses zero — this is statistical noise, not a real regression. Do not act on inconclusive results; re-run with more iterations if the signal persists.

## Next Steps

1. ✅ Safe to merge — no performance regressions
2. Optional: run `make perf-tuning-default` or `full` for deeper profiling
3. Publish: `make perf-publish` (commits evidence to `perf-history/`)
