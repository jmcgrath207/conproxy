---
name: performance
description: >
  conproxy performance optimization guide — diagnose, measure, and optimize
  hot paths using the repo's profiling tools. Covers flamegraphs, DHAT,
  Criterion benches, load tests, and PGO. Invoke when latency, memory, or
  throughput is unsatisfactory.
license: MIT
metadata:
  repo: conproxy
  scope: performance optimization
---

# Performance Optimization Guide

## When to Apply

Load this skill when:

- Latency or throughput is unexpectedly high/low
- Memory usage is growing or churn is suspected
- A PR changes hot code paths (cache, cascade, scope, serde)
- You need to identify CPU hot functions before optimization
- Preparing a release build with PGO

## Decision Tree: Symptom → Tool

| Symptom | Tool | Command |
|---------|------|---------|
| High latency under load | rlt load test | `make e2e-bench` |
| Suspected heap churn | DHAT heap | `make profile-dhat` |
| Peak RSS over time | heaptrack (optional) | `make profile-heaptrack` |
| CPU hot function unknown | Flamegraph | `make profile-flamegraph` |
| Micro regression | Criterion | `cargo bench --bench core_ops` |
| Cascade/scope regression | Criterion | `cargo bench --bench cascade_scope` |
| Ship faster binary | PGO build | `make profile-pgo` |
| Syscall/lock/network | perf + bpftrace | `make e2e-profile` |
| Async contention | tokio-console | `make profile-tokio-console` (dev only) |
| Metrics baseline | Prometheus | `make profile-metrics-snap` |

## Workflow Loop

1. **Reproduce** — narrow down the scenario
2. **Measure baseline** — use appropriate tool above
3. **Isolate** — microbench vs e2e vs system-wide
4. **Change** — cite rust-skills rules (`perf-*`, `mem-*`, `opt-pgo`)
5. **Re-measure** — ensure improvement
6. **Merge gate** — if bench shows regression ≥ 15% vs baseline → investigate before merging

## Toolchain Commands

### Micro-benchmarks (pure Rust, no Docker)

```bash
# Run all benches
cargo bench --bench core_ops

# Quick check
cargo bench --bench core_ops -- --quick

# Save baseline
cargo bench --bench core_ops -- --save-baseline main

# Compare against baseline
cargo bench --bench core_ops -- --baseline main
```

**Regression threshold:** 15% slower than baseline → investigate before merging.

### CPU Flamegraph

```bash
# Requires: cargo install samply (preferred) OR
#           perf + inferno-flamegraph (fallback)
#           cargo flamegraph is NOT supported (cold-start, port collision)
make profile-flamegraph
# Output (sibling of Makefile):
#   - flamegraph.svg   (perf + inferno)
#   - profile.json.gz  (samply → https://profiler.firefox.com)
```

**Note:** Recipe attaches to the running proxy PID and samples under curl load.
Requires the binary be built with debug symbols (the `profiling` profile used here
inherits `release` + `debug = 2`). Run `cargo install samply` once; or
`cargo install inferno-flamegraph` + Linux `perf` for the fallback path.

### DHAT Heap Profile

```bash
make profile-dhat
# Produces /tmp/conproxy-dhat/dhat-heap.json
# View: https://nnethercote.github.io/dh_view/dh_view.html
```

### Heap Timeline (heaptrack, optional)

```bash
make profile-heaptrack
# Requires: apt install heaptrack  (or build from https://github.com/KDE/heaptrack)
# Produces: ./heaptrack-<ts>.gz
# View:     heaptrack_print <file>    # text summary
#           heaptrack_gui   <file>    # interactive (KDE) or https://mdklinux.github.io/heaptrack-web/
```

**Note:** Heavier than DHAT — attaches to PID, samples every alloc/free.
Use for **leak suspects** and **peak RSS over time** rather than alloc hotspots.

### PGO Build (runtime perf)

```bash
make profile-pgo
# Instrumented build → workload → optimized binary
# Yields 5-20% speedup on hot paths
```

### Tokio Console (async diagnosis)

**Interactive (dev):**
```bash
make profile-tokio-console
# Builds with: RUSTFLAGS="--cfg tokio_unstable" --features tokio-console
# Default console server: 127.0.0.1:6669 (override: TOKIO_CONSOLE_BIND=host:port)
# Inspect: tokio-console http://127.0.0.1:6669   (CLI client, NOT a browser)
```

**Headless (CI / scripted, no CLI install required):**

`make perf-tuning-full TOKIO_CONSOLE=1` runs three in one go under load:

| Output | Source | Contents |
|--------|--------|----------|
| `console-snap.json` + `.txt` | `console_snap` bin (connects to console port 6669) | **A**: per-task top-N by poll time |
| `tokio-metrics.json` | `GET /debug/tokio` on proxy | **B**: `Handle::current().metrics()` aggregates (always-on) |
| `tokio-dump.txt` | `GET /debug/tokio/dump` on proxy | **C**: `Handle::dump()` task backtraces (one-shot, stuck-task debug) |

`summary.json` gains `tokio_metrics` + `tokio_dump` fields; ANALYSIS.md renders
all three sections. B and C require the proxy to be built with the same
`RUSTFLAGS=--cfg tokio_unstable` + `--features tokio-console` (the Makefile
step handles both).

### Prometheus Metrics Snapshot

```bash
make profile-metrics-snap
# Starts proxy on :9092 (gRPC) / :9093 (HTTP), sends curl load, scrapes:
#   /metrics/prometheus  (Prom text — canonical)
#   /metrics             (JSON)
#   /debug/vars          (Go-style runtime metrics)
# Copy of the scrape: tests/results/perf-tuning/<ts>/metrics-prometheus.txt
```

**Note:** Always available — no extra features. The `/metrics/prometheus` endpoint
is canonical; `/metrics` is the legacy JSON view.

### Cascade + Scope Benchmarks

```bash
# Run cascade/scope microbenchmarks
cargo bench --bench cascade_scope

# Quick check
cargo bench --bench cascade_scope -- --quick

# Save baseline
cargo bench --bench cascade_scope -- --save-baseline main

# Compare against baseline
cargo bench --bench cascade_scope -- --baseline main
```

**Regression threshold:** 15% slower than baseline → investigate before merging.

### Port Map (profiling recipes)

Each profiling Make target binds to its own port pair so they can be run
sequentially without cleanup. Override `listen` in the toml if you need to.

| Recipe | gRPC | HTTP | Notes |
|--------|------|------|-------|
| `profile-dhat` | 9096 | 9097 | Heap profile under brief load |
| `profile-flamegraph` | 9090 | 9091 | CPU flamegraph under curl load |
| `profile-pgo` | 9090 | 9091 | PGO instrumented run |
| `profile-heaptrack` | 9094 | 9095 | Heap timeline (optional tool) |
| `profile-metrics-snap` | 9092 | 9093 | Prometheus scrape under load |
| `profile-tokio-console` | 8080 | 8081 | Plus console server on 127.0.0.1:6669 |

## Load Test (e2e, needs Docker)

```bash
# Start services
make e2e-services-up

# Load test data
make e2e-load-data

# Run load benchmarks
make e2e-bench

# With system profiling
make e2e-profile
```

## conproxy Hot Modules

| Module | Why hot | Bench/Profile |
|--------|---------|---------------|
| `CacheStore` (insert) | Every query caches | `cache_insert` |
| `CacheStore` (lookup) | Cache hit/miss paths | `cache_lookup_hit`, `cache_lookup_miss` |
| `slugify` | File naming (embed caching) | `slugify` |
| `serde_json` | Request/response serialization | `query_serialize_json`, `response_serialize_json` |
| `QueryRequest::hash` | Cache key | `query_hash` |
| Query normalization | Cache key stability | `normalize_query` |
| `fuse_rrf` (cascade) | Cross-backend dedup + RRF | `fuse_rrf_*` (cascade_scope bench) |
| `ScopeFilter` | Lexical Jaccard scoring | `scope_best_sim_*`, `scope_filter_*` (cascade_scope bench) |

## Pointers to rust-skills

- **Memory:** `mem-arrayvec`, `mem-clone-from`, `mem-write-over-format`, `mem-arena-allocator`
- **CPU:** `opt-inline-always-rare`, `opt-cold-unlikely`, `opt-codegen-units`, `opt-pgo-profile`, `opt-target-cpu`
- **Performance patterns:** `perf-iter-over-index`, `perf-collect-once`, `perf-entry-api`, `perf-drain-reuse`, `perf-extend-batch`, `perf-chain-avoid`, `perf-collect-into`
- **Testing:** `test-criterion-bench`

## What Not to Do

- PGO before profiling (measure first)
- `target-cpu=native` in portable release artifacts
- Optimize without benchmark numbers
- Hold locks across `.await` (repo rule violation)
- Use `unwrap`/`expect` in hot paths (clippy deny)

## Prerequisites

| Tool | Install | For |
|------|---------|-----|
| `samply` | `cargo install samply` | `profile-flamegraph` (preferred) → `profile.json.gz` |
| `perf` + `inferno-flamegraph` | system pkg + `cargo install inferno-flamegraph` | `profile-flamegraph` fallback → `flamegraph.svg` |
| `tokio-console` | `cargo install tokio-console` | `profile-tokio-console` (CLI client) |
| `bpftrace` | system pkg | `e2e-profile` |
| `rlt` | cargo crate (via `load-test` feature) | `e2e-bench` |
| `cargo flamegraph` | NOT supported | port collision + cold-start; not in toolchain |

## Next Steps

1. Identify bottleneck with `profile-flamegraph` or `profile-dhat`
2. Add targeted microbench if missing (see `benches/core_ops.rs`, `benches/cascade_scope.rs`)
3. Apply rust-skills rules
4. Verify with `bench-compare` or `e2e-bench`
5. Generate HTML report: `cargo run --bin test_runner -- index tests/results/perf-tuning/<ts>/`
6. Repeat on same machine — **pin with `taskset -c 0-3 cargo bench ...`** to reduce laptop noise

## Noise reduction (laptop runs)

- **Pin CPUs:** `taskset -c 0-3 cargo bench --bench cascade_scope` keeps the
  bench on one core/CCX to avoid scheduler migration between runs.
- **Drop turbo:** `sudo intel-undervolting ...` (n/a) — instead disable turbo
  in BIOS or use `thermald`'s `balance-performace`. Otherwise adjacent runs
  drift ±10%.
- **Drain background:** `systemctl stop thermald snapd` etc. before long runs.
- **CI runners** don't need this — they're isolated.

## `/perf-tuning` Command

Run structured performance diagnosis loop: measure → analyze → plan.

```bash
/perf-tuning [mode]
```

| Mode | Command | What happens |
|------|---------|--------------|
| `quick` (default) | `make perf-tuning-quick` | Bench both targets (`--quick`) + summarize |
| `default` | `make perf-tuning-default` | Quick + `profile-metrics-snap` |
| `full` | `make perf-tuning-full` | Default + flamegraph + hitrate + heaptrack + (optional) tokio-console |
| `plan-only` | (no make target) | Analyze last run dir or user-provided data |

**`full` env off-switches:** `FLAMEGRAPH=0`, `HITRATE=0`, `HEAPTRACK=0`, `TOKIO_CONSOLE=1` (off by default), `DHAT=1` (off by default). All graceful-skip if the tool is missing.

**Output:** `tests/results/perf-tuning/<ts>/` containing `summary.json` (schema v2), `SUMMARY.md`, `ANALYSIS.md` (14 sections: verdict, baseline, trend, per-group deep dive, hot-path regressions, hitrate, metrics, diagnostics, **tokio runtime (A: per-task top-N, B: RuntimeMetrics aggregates, C: Handle::dump pointer)**, history, recommendations, noise, next steps), `report_criterion.json`, `MANIFEST.md`, `env.json`, `baseline_meta.json`, plus raw `criterion_*.txt`, optional `hitrate/summary.json`, `heaptrack.txt`, `console-snap.json`+`.txt` (A), `tokio-metrics.json` (B), `tokio-dump.txt` (C), `flamegraph.svg` (or `profile.json.gz`).

**Publish to git history** (tracked, never auto-commits): `make perf-publish` copies artifacts to `perf-history/<ts>-<sha8>/`, prunes to 100 most recent. Subsequent runs populate ANALYSIS's "History" section.

**Verdict logic (in `perf_summarize`):** 95% CI from raw `new/` + `main/`
estimates (independent-sample SE). Bench is **regression** if CI lower bound ≥
`+threshold%`, **improvement** if CI upper bound ≤ `-threshold%`, else
**inconclusive**. Threshold default 15% (override via `--threshold`).

**Agent workflow:**
1. Run mode command (writes `summary.json` + `MANIFEST.md` + `ANALYSIS.md`).
2. Read `summary.json` (sandbox only — do not dump raw Criterion text into chat).
3. **Print ANALYSIS.md to stdout** faithfully — the agent MUST emit the narrative block.
4. For each `regression`, look up the bench source in `benches/core_ops.rs` /
   `benches/cascade_scope.rs` AND consult the hot-path map in ANALYSIS.md.
5. Write `PLAN.md` to the run dir.
6. Stop and prompt user.

**Hard rules:** measure first; ≥15% gate (CI-aware); never auto-implement;
evidence-first plans; single plan home under run dir; `make bench-compare`
still uses `--baseline main` (raw CI fallback in `perf_summarize` covers the
case where main/ doesn't exist yet); on REGRESSIONS check baseline drift first
(stale baseline = refresh + re-run before code investigation).

**Optional flags** (advanced):
- `make perf-tuning-full FLAMEGRAPH=0 HITRATE=0 HEAPTRACK=0` — no backends needed
- `cargo run --bin perf_summarize -- --since .bench_start --fail-on-regression --metrics-file metrics-prometheus.txt --hitrate-summary <path> --history-dir perf-history target/criterion/`
