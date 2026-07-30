# Local dev loop — fast iteration on cache + unit tests

The unit-test harness itself is fast (~3.6 s for 1738 tests with
nextest). The iteration drag is **compile, link, and disk bloat**, not
the tests.

Measured on a 16-core / 30 GB Linux host with the changes in this repo
(2026-07-28):

| Loop                                    | Before  | After  |
|-----------------------------------------|---------|--------|
| `make t` (warm)                         | ~22 s   | ~4.3 s |
| Touch one src file → test               | ~5 s    | ~8 s*  |
| Fresh test binary size                  | 443 MB  | 106 MB |
| `target/` size (debug)                  | 227 GB  | shrinking** |
| Switch to `--features embed-api` (warm) | ~36 s   | ~9 s |

\* First link with lld is roughly the same as bfd; the win is link time on
**large** rebuilds (e.g. after a feature switch), and in dev-loop overhead
between saves. The 4.3 s warm wall is dominated by nextest startup + the
single slowest test (`test_freshness` at 3.2 s of pure sleeps).

\** Run `make target-prune` to drop the conproxy-only artifacts; full
prune is `cargo clean` (also drops dependent crate artifacts).

## What changed

- **`.cargo/config.toml`** (new) — Linux-only: links via `clang -fuse-ld=lld`
  with `--no-keep-memory`. Drops the heavy test-bin link from bfd.
- **`Cargo.toml [profile.dev]`** — `debug = "line-tables-only"` and
  `split-debuginfo = "unpacked"`. Test binaries shrink ~4×, link is faster.
- **`.config/nextest.toml`** (new) — `dev` profile: `num-cpus` processes,
  fail-fast, terse per-test output. Used by `make test-nextest` and `make t`.
- **`Makefile`** — new `t` / `test-nextest` / `test-fast` / `test-filter`
  / `test-slow` / `target-prune` / `nextest-install` targets.
- **`AGENTS.md`** — Tier 1 "every save" recommendation narrowed to
  `make t`; full Tier 1 gate stays pre-PR.

## Parallelization — `cargo-nextest`

`make t` uses **cargo-nextest** (process-level parallelism) by default.
Install with `make nextest-install` (one-time, ~2 min).

Why nextest over `cargo test`:
- **Per-test process isolation** — no shared state, no test interference.
- **Per-test timing in output** — every `PASS` line shows the test's
  duration, so the slowest tests jump out (see `make test-slow`).
- **Real-time stdout per test** — failing tests dump output as they
  fail; no need to re-run with `--nocapture`.
- **Test filtering** — `make test-filter PAT=foo` matches by name.

When nextest is missing, `make t` falls back to `cargo test --lib -q`
with no other change to the loop.

## Save-loop rules of thumb

1. **`make t` on every save.** Default features only, no clippy, no
   second feature surface. ~3.5 s warm, ~5–10 s after a small edit.
2. **Stay on one feature surface per session.** Switching between
   `default`, `embed-api`, `mcp`, `pgvector`, `release` triggers a
   full crate recompile (~30–45 s on first switch). If you must work
   on a feature-gated path, leave it on until you push.
3. **Don't `cargo build --workspace` in the save loop.** It pulls
   `sdk/python` (pyo3). Use it for release/PR gates only.
4. **Pre-commit / pre-PR: full Tier 1.** `cargo fmt -- --check`,
   `cargo clippy -- -D warnings`, `cargo test --lib`,
   `cargo test --features "embed-api" --lib`. See `AGENTS.md` § "Fast
   Feedback Tiers".
5. **`target/` bloat.** If you haven't pruned in a while:
   `make target-prune` (conproxy-only) or `cargo clean` (full). The
   incremental cache alone is ~130 GB after a few feature surfaces.
6. **Find slow tests.** `make test-slow` (uses nextest) shows the top
   20 slowest tests. The wall is bound by the slowest one; shaving
   that one test directly trims the loop.

## Optional: sccache

If you frequently switch between feature surfaces or branches:

```sh
cargo install sccache --locked
export RUSTC_WRAPPER=sccache
```

Then `cargo clean` and a rebuild is much cheaper on the second pass.
Not required — incremental is already fast for a single surface.

## Future work (P3 / P4)

- **The wall is bound by `test_freshness` (3.2 s of serial sleeps).**
  Replacing that test (and ~250 similar `tokio::time::sleep` /
  `thread::sleep` sites in lib tests) with `tokio::time::pause` +
  manual clock advance, OR injecting a fake clock into `CacheStore`,
  would drop the wall to <1 s. Defer until the compile/link wins are
  exhausted — high-risk change touching timing-sensitive code.
- Structural: reduce feature-gated API surface in hot modules so
  default vs `embed-api` shares more codegen. Larger refactor; not
  on the per-save hot path.
