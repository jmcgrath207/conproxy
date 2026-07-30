---
description: Structured performance tuning — measure, analyze, plan (no auto-implement)
---

# /perf-tuning

Run the performance diagnosis pipeline for conproxy. Mode: `$ARGUMENTS` (default: `quick`).

## Modes

| Mode | Command | What happens |
|------|---------|--------------|
| `quick` | `make perf-tuning-quick` | Bench both targets (`--quick`) + summarize → results dir |
| `default` | `make perf-tuning-default` | Quick + `profile-metrics-snap` |
| `full` | `make perf-tuning-full` | Default + flamegraph + hitrate + heaptrack + tokio-console (all env-off-switchable) |
| `plan-only` | (no make target) | Analyze last run dir or user-provided data |

**Env off-switches for `full`** (all default ON except where noted):

| Switch | Default | Skips | Needs |
|--------|---------|-------|-------|
| `FLAMEGRAPH=0` | 1 | `make profile-flamegraph` | samply OR perf+inferno (graceful skip) |
| `DHAT=1` | 0 | (off by default) — adds `make profile-dhat` | Qdrant |
| `HITRATE=0` | 1 | `cargo run --bin hitrate_bench` | nothing |
| `HEAPTRACK=0` | 1 | `make profile-heaptrack` | heaptrack tool (graceful skip) |
| `TOKIO_CONSOLE=1` | 0 | (off by default) — builds instrumented proxy, sends curl load, runs `console_snap` 5s, fetches `/debug/tokio` (RuntimeMetrics) + `/debug/tokio/dump` (task backtraces) | `RUSTFLAGS=--cfg tokio_unstable` + tokio-console-snap feature |

Example: `make perf-tuning-full HITRATE=0 HEAPTRACK=0` (no backends needed).

**HTML report:** after any perf-tuning run, generate the dashboard:

```bash
cargo run --bin test_runner -- index tests/results/perf-tuning/<ts>/
# → <ts>/index.html with Benchmark Changes table wired to report_criterion.json
```

**Publish to git history** (optional, manual commit):

```bash
make perf-publish    # copies artifacts to perf-history/<ts>-<sha8>/, prunes to 100
git add perf-history/
git commit -m 'perf(history): publish <ts>-<sha8>'
```

`perf-history/` lives at the repo root, is **tracked** (contrasts with
`tests/results/` which is gitignored), and gives cross-clone trend visibility
for `ANALYSIS.md`'s "History" section. Pruned to most recent 100 runs
automatically. Never auto-commits — repo rule.

## Agent pipeline

### 1. DETECT

```bash
command -v samply && echo "samply: OK" || echo "samply: MISSING"
curl -sf http://localhost:6333/health >/dev/null && echo "qdrant: UP" || echo "qdrant: DOWN"
ls -lt tests/results/perf-tuning/ 2>/dev/null | head -3
# Baseline meta (fresh after make bench-save):
cat target/criterion/.baseline_meta.json 2>/dev/null || echo "NO BASELINE META"
git status --short | head -5
```

### 2. MEASURE

Run mode command. Output lands in `tests/results/perf-tuning/<YYYYMMDD-HHMMSS>/`:

```bash
make perf-tuning-quick   # or default or full
```

Produces:
- `summary.json` — machine-readable (agents parse this)
- `SUMMARY.md` — human scorecard (one screen)
- `report_criterion.json` — test_infra-compatible (index.html)
- `MANIFEST.md` — what each file is
- `criterion_*.txt` — raw Criterion output (collapsible detail)
- `metrics-prometheus.txt` — optional scrape
- `env.json` — git sha, mode, threshold

### 3. ANALYZE

Read `summary.json` via sandbox code (don't dump raw criterion text into chat):

```
<read summary.json>
```

**First: check baseline drift** (before investigating code):
- `baseline_status`: `stale_dirty` (uncommitted edits), `stale_age` (>24h), `stale_sha` (commits behind HEAD), `ok` (fresh)
- `baseline_age_hours`: how old the baseline is
- If **stale + REGRESSIONS** → drift is the first hypothesis. Refresh baseline (`make bench-save`) and re-run before investigating code.

Extract:
- **Regressions:** `bench[].status == "regression"` (95% CI lower bound ≥ +threshold)
- **Improvements:** `bench[].status == "improvement"` (CI upper bound ≤ −threshold)
- **Inconclusive:** `bench[].status == "inconclusive"` (raw change above threshold but CI crosses zero — noise, do not act on)
- **No-baseline:** `bench[].status == "no_main"` → warn user, treat as fresh run

### 4. RANK

For each regression, assign priority:
- **P0** — regression or hot path consuming >20% CPU
- **P1** — optimization opportunity >10% improvement potential
- **P2** — nice-to-have, low effort

Cite the relevant `rust-skills` rule (e.g., `perf-iter-over-index`, `mem-clone-from`).

### 5. PLAN

Write `PLAN.md` to the run dir (`tests/results/perf-tuning/<ts>/PLAN.md`).

**Frontmatter** (YAML, for agents):
```yaml
---
schema_version: 1
run_dir: tests/results/perf-tuning/<ts>
mode: quick
verdict: PASS|REGRESSIONS|NO_BASELINE
threshold_pct: 15
p0: 0
p1: 0
p2: 0
sources: [summary.json]
---
```

**Sections:**
1. **Evidence** — numbers from `summary.json` (cite file path)
2. **Hypotheses** — clearly labeled *inferred*, not facts
3. **Proposed changes** — file:symbol, rust-skills rule, verify command
4. **DoD** checkboxes
5. **Out of scope**

**Ban:** template examples that invent root causes without measurement.

### 6. STOP

Chat output (agents + humans):

```
Verdict: PASS (or REGRESSIONS, NO_BASELINE)
Run: tests/results/perf-tuning/<ts>/
Summary: <path>/SUMMARY.md
Analysis: <path>/ANALYSIS.md
Plan: <path>/PLAN.md    # after analyze
Regressions:
  - cache_insert +14.9%  (just under threshold, worth watching)
Hitrate (if full mode): PASS (agentic 95.6% / gate 40%)
Diagnostics: flamegraph=✅ heaptrack=❌ tokio=❌ tokio-metrics=✅ tokio-dump=❌
Tokio (if TOKIO_CONSOLE=1):
  - A: top task <name> consumed 42% of poll time
  - B: 14 alive tasks, 4 workers, 0.3ms mean poll
  - C: <handle::dump file path>

--- Analysis (from ANALYSIS.md) ---
[narrative: what ran, verdict meaning, baseline health, trend (vs prior run),
 per-group deep dive, top movers, regressions detail (with hot-path map +
 repro cmd + rule hints), cache effectiveness, metrics deltas, diagnostics,
 tokio runtime (A: top tasks, B: aggregates, C: handle::dump pointer),
 history (last 5 published), rule-based recommendations, noise note, next steps]

Stop. Say "implement" / pick P0 / "re-run default" / "publish".
```

**Always print ANALYSIS.md to stdout** after perf_summarize completes. 
The agent MUST emit the analysis block in its chat response — do not 
summarize, reproduce it faithfully.

Never auto-implement. After STOP, if user approves, optionally `make perf-publish` to commit evidence to `perf-history/`.

### 7. PUBLISH (optional)

After the user reviews the analysis, if they want a permanent record (especially useful for PRs):

```bash
make perf-publish         # copies artifacts → perf-history/<ts>-<sha8>/
# (prints the git add/commit commands — never auto-commits)
git add perf-history/
git commit -m "perf(history): publish <ts>-<sha8>"
```

Once at least one run is in `perf-history/`, subsequent `make perf-tuning-*`
runs will populate ANALYSIS.md's "History (last 5 published runs)" table with
verdict timeline and top-mover drift.

## Artifact layout

```
tests/results/perf-tuning/<ts>/
  env.json                    # git sha, mode, threshold, baseline_meta copy
  baseline_meta.json          # copy of target/criterion/.baseline_meta.json
  summary.json                # agent canonical parse target (schema v2)
  report_criterion.json       # test_infra-compatible (index.html)
  SUMMARY.md                  # human scorecard (drift warning when stale)
  ANALYSIS.md                 # narrative analysis (14 sections: verdict, baseline, trend,
                              #   per-group deep dive, hot-path regressions, hitrate,
                              #   metrics deltas, diagnostics, history, recommendations,
                              #   noise note, next steps)
  MANIFEST.md                 # orientation
  criterion_core_ops.txt      # raw output
  criterion_cascade_scope.txt # raw output
  metrics-prometheus.txt      # optional
  metrics-before.txt          # optional (counter deltas)
  hitrate/summary.json        # full mode: hit-rate bench result
  heaptrack.txt               # full mode: heaptrack log (if tool present)
  console-snap.json           # full mode + TOKIO_CONSOLE=1: per-task top-N (A)
  console-snap.txt            # same: human-readable text table
  tokio-metrics.json          # full mode + TOKIO_CONSOLE=1: RuntimeMetrics JSON (B)
  tokio-dump.txt              # full mode + TOKIO_CONSOLE=1: Handle::dump() text (C)
  flamegraph.svg              # full mode (if tool present)
  PLAN.md                     # written after analyze step
```

`summary.json` schema v2 adds: `hitrate` (object), `diagnostics` (flamegraph/heaptrack/tokio/tokio_metrics/tokio_dump/hitrate bools), `tokio` (A: per-task top-N), `tokio_metrics` (B: aggregates from Handle::metrics()), `tokio_dump` (C: Handle::dump() text), `trend` (vs prior run), `chronic_regressions` (flagged in 2+ prior runs), `history` (last 5 published runs).

## Hard rules

1. **Measure before optimizing** — never guess
2. **≥15% regression** (CI-aware) = investigate before merging
3. **Never auto-implement** — show plan, wait for approval
4. **Never run PGO** without explicit `--pgo` flag
5. **Never run full e2e** without confirming Docker services are up
6. **Agent reads `summary.json`** — don't dump raw criterion text into chat
7. **Evidence-first plans** — cite `summary.json` numbers, not guesses
8. **Single plan home** — always under the run dir, not `.opencode/plans/`
9. **CI-aware threshold** — `perf_summarize` uses 95% CI from independent-sample
   SEs. Raw percent change can be misleading on small benches; trust the CI
   flag, not the headline percent. `inconclusive` means noise, not "ignore".
10. **Baseline drift first** — on REGRESSIONS with stale baseline (dirty/sha/age),
    refresh baseline and re-run BEFORE investigating code changes. Never
    `make bench-save` after REGRESSIONS without explicit user OK.
11. **bench-save refuses dirty tree** — use `FORCE=1` only during active development.

## Tips

- `quick` first → see if anything obviously broke
- **False regression playbook:** REGRESSIONS + stale baseline? → refresh (`make bench-save`, verify clean tree) → re-run → if PASS, it was drift, not code
- Baseline age in `summary.json` → `baseline_age_hours` (>24h = stale)
- `baseline_status` = `ok` / `stale_dirty` / `stale_age` / `stale_sha` / `no_meta`
- For alloc issues → `make profile-dhat` separately (needs Qdrant)
- For CPU hot functions → `make profile-flamegraph` separately (needs samply or perf+inferno)
- For async hot spots → `make profile-tokio-console` (needs `tokio-console` CLI)
- Results accumulate in `tests/results/perf-tuning/` — compare across sessions
- Clean old runs: `make perf-tuning-clean`
- `report_criterion.json` plugs into `test_runner index` for HTML report (when wired)
