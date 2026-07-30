# Perf Run 20260724-201244 — PASS

| Field | Value |
|-------|-------|
| Mode | full |
| Git | `cfff440` |
| Rustc | rustc 1.97.0 (2d8144b78 2026-07-07) |
| Threshold | ≥15% (CI-based) |
| Benches | 21 total, 0 regression(s), 0 improvement(s), 21 inconclusive, 0 no-main |

## Baseline

| Field | Value |
|-------|-------|
| Status | ✅ ok |
| Saved at | 2026-07-25T00:13:18Z |
| SHA | `cfff440` |
| Rustc | rustc 1.97.0 (2d8144b78 2026-07-07) |
| Bench count | 21 |
| Age | 0.0h |
| Commits behind | 0 |

## Inconclusive (CI crosses threshold — needs more data)

| Bench | Δ | CI |
|-------|---|----|
| cache eviction pressure | +2.0% | [+0.8%, +3.3%] |
| cache insert | +3.1% | [-0.6%, +6.7%] |
| cache lookup hit | +8.1% | [+6.6%, +9.7%] |
| cache lookup miss | +7.3% | [+5.3%, +9.3%] |
| cache throughput/100 | -3.7% | [-4.7%, -2.7%] |
| cache throughput/1000 | -4.3% | [-5.4%, -3.2%] |
| cache throughput/10000 | +3.3% | [+2.0%, +4.7%] |
| fuse rrf/2upstreams 50each | -3.6% | [-4.4%, -2.8%] |
| fuse rrf/3upstreams k60 | +2.8% | [-38.3%, +43.8%] |
| fuse rrf/dedup 2upstreams | +6.2% | [-63.4%, +75.8%] |
| normalize query | +6.7% | [+5.4%, +8.0%] |
| query deserialize json | -2.9% | [-5.1%, -0.6%] |
| query hash | -2.3% | [-3.4%, -1.1%] |
| query serialize json | -5.2% | [-7.0%, -3.3%] |
| response deserialize json | +2.4% | [+0.2%, +4.7%] |
| response serialize json | +9.2% | [+7.7%, +10.8%] |
| scope best sim long content | -3.7% | [-4.9%, -2.4%] |
| scope best sim no match | -4.5% | [-5.5%, -3.4%] |
| scope best sim short content | -3.2% | [-4.1%, -2.3%] |
| scope filter results 20items | -7.4% | [-8.6%, -6.2%] |
| slugify | +9.0% | [+6.7%, +11.4%] |

## Artifacts

- `summary.json` — machine-readable (agents parse this)
- `report_criterion.json` — test_infra-compatible (index.html)
- `SUMMARY.md` — this file (human)
- `ANALYSIS.md` — narrative: what happened + next steps
- `MANIFEST.md` — what each file is
- `PLAN.md` — optimization plan (written by agent after analyze)

- `env.json` — run context (git SHA, mode, baseline meta copy)
- `baseline_meta.json` — copy of `target/criterion/.baseline_meta.json`
**No regressions detected.** Ready to merge.
