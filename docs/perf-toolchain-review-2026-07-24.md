# Perf Toolchain Review — 2026-07-24

Scope: `/perf-tuning` command, `perf_summarize` bin, Makefile profile/bench targets,
performance skill, benches. Method: static read + live runs + crate-source verification
(console-subscriber 0.2.0) + upstream docs (criterion.rs book, samply, tokio-console, bencher).

Every finding below was reproduced or verified against source. Confidence is stated.

---

## P0 — Truth bugs (toolchain lies to the user)

### B1. tokio-console port documented wrong: 6969 vs actual 6669
- `Makefile:401,407,408` and `.opencode/skills/performance/SKILL.md:102` say `http://localhost:6969`.
- console-subscriber 0.2.0 `builder.rs`: default `TOKIO_CONSOLE_BIND` = `127.0.0.1:6669`
  (`Server::DEFAULT_PORT` via `ServerAddr::Tcp` default). Verified in local crate source.
- Impact: `tokio-console http://localhost:6969` never connects. Feature is DOA as documented.
- Fix: s/6969/6669/ everywhere; document `TOKIO_CONSOLE_BIND` override.

### B2. samply branch writes a fake `flamegraph.svg`
- `Makefile:360`: `samply record -p $PID -o flamegraph.svg --save-only`.
- samply only emits Firefox Profiler format (gzipped JSON). It cannot produce SVG.
  The `-o` name does not change the format. Result: `flamegraph.svg` is gzip JSON.
- `[ -s flamegraph.svg ]` passes, `perf-tuning-full` copies it into the run dir,
  `perf_summarize` may copy it again. Users open an "SVG" that isn't.
- Fix: `-o profile.json.gz` + print "open at https://profiler.firefox.com".
  Only the perf+inferno branch produces a real SVG. Adjust file-existence check:
  accept `flamegraph.svg` (inferno) OR `profile.json.gz` (samply).

### B3. cargo-flamegraph branch profiles a crashing second proxy
- `Makefile:366-371`: proxy #1 is already bound to :9090/:9091 (started line 348).
  `cargo flamegraph ... -- --config conproxy.toml` spawns proxy #2 with the same config
  → bind failure → flamegraph captures a dying process, or the recipe hangs.
  Also cold-start: no workload runs against it.
- Fix: delete the cargo-flamegraph branch entirely (it cannot attach to a PID).
  samply + perf/inferno cover all cases. Remove from prereqs in skill/Makefile help.

### B4. Bench failures masked by `tee` (no pipefail)
- `Makefile:459,461` (perf-tuning-quick), `477,479` (default), `495,497` (full),
  `test-all-bench:655`: `cargo bench ... | tee file`. make uses `/bin/sh -c`;
  pipeline exit = tee's exit = 0. A bench that fails to build or panics is invisible,
  and the summarizer then reads STALE `target/criterion` data → false PASS.
- Fix: add `SHELL := /bin/bash` + `.SHELLFLAGS := -eo pipefail -c` at top of Makefile
  (audit other recipes first), or capture `${PIPESTATUS[0]}` per recipe.

### B5. perf-tuning verdict compares against the LAST RUN, not the `main` baseline
- Criterion semantics (criterion.rs book, verified): default comparison is new-vs-previous-run.
  `--baseline main` compares against the named baseline without overwriting.
- perf-tuning runs plain `cargo bench --quick`, so `change/estimates.json` = delta vs whatever
  ran last (often the previous perf-tuning). The documented gate is "15% vs baseline `main`".
  Verdicts are therefore drift-detection, not branch-vs-main regression detection.
- Fix (cheap): add `--baseline main` to perf-tuning bench invocations
  (criterion warns, not errors, when `main` is missing).
- Fix (better): perf_summarize diffs `new/estimates.json` vs `main/estimates.json` itself
  (both exist on disk; `main/` confirmed present for saved benches), reports both deltas:
  vs-main (gate) and vs-last-run (noise indicator).

---

## P1 — Correctness / freshness

### B6. Summarizer has no freshness model
- Reads ALL of `target/criterion/*`. Benches that failed, were deleted, or were never run
  in this session still appear in `summary.json` with current timestamps.
- Fix: Makefile `touch "$RUN_DIR/.bench_start"` before benching; summarizer gains
  `--since <file>`: only include benches whose `new/estimates.json` mtime ≥ marker.
  Refuse (or mark `STALE`) otherwise.

### B7. Stale `flamegraph.svg` copied into every run dir
- `perf_summarize.rs:310-313`: copies root `flamegraph.svg` regardless of age.
  A week-old flamegraph lands in today's run. Fix: only copy when mtime is within the
  run window (see B6 marker), or only when `--flamegraph <path>` is passed explicitly.

### B8. Metrics artifact is an empty file; `summary.metrics` always null
- `perf-tuning-quick` curls `127.0.0.1:9093` where nothing is listening;
  shell `>` creates a 0-byte `metrics-prometheus.txt` (confirmed: 0 bytes in
  `tests/results/perf-tuning/20260724-013203/`). `summary.json` → `"metrics": null`
  (confirmed live). Nothing ever populates the metrics field.
- Fix: quick mode → skip scrape (or curl `-o` so failure leaves no file);
  default/full → capture `/metrics/prometheus` from the metrics-snap proxy run INTO the
  run dir, and parse headline counters (queries, cache_hits, hit_rate, upstream_latency)
  into `summary.metrics`.

### B9. Parameterized benches silently dropped
- Summarizer iterates one level: `target/criterion/<name>/new/estimates.json`.
  Criterion nests parameterized benches as `<group>/<id>/new/...`. First `bench_with_input`
  added to the repo vanishes from reports. Latent — no such bench today.
- Fix: recursive walk for `new/estimates.json`; name = path relative to criterion root.

### B10. perf_summarize output bugs (verified live)
- `render_manifest` flamegraph check: `format!("{}/flamegraph.svg", s.artifacts.summary_md)`
  → checks literal path `SUMMARY.md/flamegraph.svg`. Always false. Manifest can never
  list a flamegraph. Needs the run dir threaded through.
- MANIFEST schema pointer says `benches/perf_summarize.rs`; actual file is
  `src/bin/perf_summarize.rs`.
- Line 187 skip-list contains `" פרופיל"` (Hebrew "profile") — hallucinated junk string.
  Harmless, embarrassing; remove. Real internal dirs to skip: `report`.

### B11. `test-all-bench` only runs `core_ops`
- `Makefile:655`: `cargo bench --bench core_ops` only. `cascade_scope` is absent from the
  full suite even though `make bench` runs both. Fix: add second line (with B4's pipefail fix).

### B12. `perf-tuning-full` help text lies
- `Makefile:1156`: "Default + flamegraph + optional e2e-bench" — recipe has no e2e-bench step.
  Fix help, or add a guarded `curl -sf localhost:6333 && $(MAKE) e2e-bench` step.

---

## P2 — Rigor / CI-readiness

### G1. No statistical gating — point estimates only
- `change/estimates.json` carries 95% CI (`lower_bound`, `upper_bound`) + `standard_error`
  (verified on disk). Verdict uses `point_estimate` alone → noise (±5% observed between runs)
  can flip PASS/REGRESSIONS.
- Fix: regression iff `lower_bound ≥ +threshold`; improvement iff `upper_bound ≤ −threshold`;
  otherwise `inconclusive`. Surface `ci: [lo, hi]` per bench in summary.json.
  This is the single highest-leverage analysis upgrade.

### G2. No exit code / no CI gate
- perf-tuning always exits 0. Add `--fail-on-regression` (exit 1 on REGRESSIONS) so CI can
  gate. Also emit `--output-format bencher`-compatible results (criterion supports
  `--output-format bencher`; bencher.dev pattern: track `main` baseline in CI, alert on
  threshold breach) or keep `report_criterion.json` as the contract and add a CI consumer.
- Design sketch (GitHub Actions): bench job on PR → `make bench-save main` on trunk artifacts
  restored → `cargo bench --baseline main` → `perf_summarize --fail-on-regression` →
  upload run dir as artifact.

### G3. clippy doesn't cover bins
- `cargo clippy --bin perf_summarize` → 6 warnings (e.g. `arithmetic_side_effects` on `i += 1`).
  Tier gates run `--lib` only. Fix warnings; add `cargo clippy --bins` (or `--all-targets`)
  to Tier 1/2.

### G4. Port map undocumented
- 8080/8081 dev+DHAT, 9090/9091 flamegraph+PGO, 9092/9093 metrics, 6669 console.
  DHAT and console share :8080 with a normal dev proxy → collision. Add one table
  (performance skill + feature-flags.md) and pick non-default ports for dhat/console.

### G5. contributing skill stale
- `.opencode/skills/contributing/SKILL.md:203`: "single bench target `core_ops`".
  Now two targets + perf-tuning pipeline. Update section 6.

### G6. Misc polish
- `profile-tokio-console` comment still says "Opens ... in browser" (wrong port too);
  `CONPROXY_DHAT=0` there is pointless.
- `env.json` (Makefile-written) lacks rustc/dirty-flag/host; summary.json has git+rustc.
  Consolidate: summarizer writes the only env record.
- Run dir timestamp is 1-second granularity — same-second runs collide. Append `-$$`.
- `--quick` tradeoff undocumented (fewer iterations → wider CIs → more "inconclusive"
  under G1). Document: quick = smoke, default/full = gate.

---

## P3 — Brainstorm (bigger swings, optional)

1. **Noise-immune CI benches**: iai (instruction counts via Callgrind) or divan for
   deterministic regression detection; keep Criterion for wall-clock. iai variance <1%
   on CI runners vs Criterion's ±5-15%.
2. **cargo-criterion**: JSON-lines machine-readable output replaces our estimates.json
   scraping; long-term cleaner contract.
3. **Baseline lifecycle**: CI on trunk refreshes `main` baseline artifacts; PR job downloads
   them → true branch-vs-trunk gating without trusting local baselines.
4. **metrics-snap deltas**: scrape before+after workload, emit counter deltas as JSON
   (queries, hits, evictions) into summary.metrics — turns the snap into a signal, not a dump.
5. **DHAT in perf-tuning-full**: copy `dhat-heap.json` into run dir; summarizer lists it
   in MANIFEST (with fixed path logic).
6. **Bench hygiene**: `iter_batched` for `lists.clone()` setup isolation (today clone cost
   is inside the measurement — realistic but undocumented); `Throughput` on fuse_rrf for
   elements/s reporting; taskset pinning guidance for laptop runs.
7. **`perf-tuning-clean` target** + retention note (results dir is gitignored, unbounded).
8. **report_criterion.json → test_runner index HTML** (planned in b5 notes, not built).
9. **tokio-console Cargo comment**: tokio/tracing arrives via feature unification from
   console-subscriber's own dep — works but invisible; comment it, or add explicit
   `"tokio/tracing"` to the `tokio-console` feature for robustness against dep-graph changes.
   Also warn: setting RUSTFLAGS rebuilds the whole dep graph (build-cache churn).

---

## Suggested fix order

| Batch | Items | Effort |
|-------|-------|--------|
| 1 (truth) | B1 port, B2 samply output, B3 delete cargo-flamegraph, B10 manifest/strings, B12 help, G5/G6 docs | ~1h |
| 2 (freshness) | B4 pipefail, B5 baseline semantics, B6 --since, B7 flamegraph mtime, B8 metrics capture, B11 test-all bench | ~2h |
| 3 (rigor) | G1 CI-based verdict, G2 exit code, G3 clippy bins, B9 recursive walk | ~2h |
| 4 (optional) | P3 items, pick per appetite | varies |
