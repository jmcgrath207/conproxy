---
name: contributing
description: >
  conproxy contributor guide — how to build, test, and ship across all 14 test
  verticals (unit, integration, e2e, eval, load, uat, bench, coverage, security,
  fuzz, mutants, profiling, python sdk, full suite). PR conventions + code
  review checklist. Invoke when contributing, running tests, or preparing a PR
  in the conproxy repository.
license: MIT
metadata:
  repo: conproxy
  scope: testing + contributor workflow
---

# conproxy Contributing Guide

## When to Apply

Load this skill when:

- Adding a new feature, bug fix, or refactor that needs testing
- Preparing a pull request (run the relevant verticals first)
- Diagnosing a CI failure
- Adding tests in a new vertical
- Onboarding to the repository

If you only need a fast build/test/lint command, read the repo's `AGENTS.md` first; it links here for detail.

## Prerequisites

External tools by vertical. Install with cargo/cargo-binstall as noted.

| Tool | Vertical | Install | Notes |
|------|----------|---------|-------|
| Docker + docker compose | e2e, load, eval, uat | system pkg | Elasticsearch + Qdrant services |
| `cargo-audit` | security | `cargo install cargo-audit --locked` | RustSec advisory DB |
| `cargo-deny` | security | `cargo install cargo-deny --locked` | supply chain + license |
| `cargo-tarpaulin` | coverage | `cargo install cargo-tarpaulin --locked` | code coverage |
| `cargo-cyclonedx` | security (sbom) | `cargo install cargo-cyclonedx --locked` | SBOM generation |
| `cargo-geiger` | security | `cargo install cargo-geiger --locked` | unsafe dep scan |
| `cargo-mutants` | security | `cargo install cargo-mutants --locked` | mutation testing |
| `cargo-fuzz` | security | `cargo install cargo-fuzz` | requires nightly toolchain |
| `rlt` | load | via `cargo` test deps | gRPC/HTTP load framework |
| Ollama OR Claude API key | eval | system pkg / env | LLM vertical comparison |
| bpftrace + perf | profiling | system pkg (Linux) | syscall/lock/network profiles |
| maturin | python sdk | `pip install maturin` | PyO3 build |

**E2E/Load/Eval/Full suite also require** Docker services. Start with `docker compose -f tests/e2e/docker-compose.yml up -d` then `cargo run --bin test_runner -- wait all`. The default compose includes qdrant + elasticsearch + meilisearch (×2) + postgres. See `Known Gaps` below.

## Build

```bash
# Debug (default features: minimal)
cargo build

# Release — standard prod binary (mcp + persistence + pgvector + embed-api)
cargo build --release --features release

# Release with all standard features
cargo build --release --features release,embed,persistence,pgvector

# Feature-specific dev builds
cargo build --features embed           # local ONNX embedding (opt-in)
cargo build --features persistence     # disk-backed cache (redb)
cargo build --features pgvector        # pgvector adapter
cargo build --features linux-sandbox   # seccomp (Linux only, opt-in)

# Profiling build (release + debug symbols + dhat)
cargo build --profile profiling --features dhat-heap
```

Meta-features: `release` = `mcp` + `persistence` + `embed-api` + `pgvector` (ONNX `embed` and `linux-sandbox` stay opt-in). `test` = `release` + `load-test` + `dhat-heap` (full test infrastructure, used by `make test-all-prebuild`).

## Test Tiers

Four tiers — run the tier that matches your stage. Tiers 1 and 2 are authoritative in `AGENTS.md`; this file owns tiers 3 and 4.

| Tier | When | Duration | Source |
|------|------|----------|--------|
| 1. Smoke | every save | <60s | `AGENTS.md` §Tier 1 (4 steps: fmt, clippy, 2× lib test) |
| 2. Pre-PR Gate | before opening PR | ~8 min | `AGENTS.md` §Tier 2 (14 steps incl. feature-surface clippy + e2e check) |
| 3. Verticals | per `Feature Test Matrix` match below | varies | this file, per-vertical commands |
| 4. Full Suite | release / nightly | 20–40 min | `make test-all` |

Assert "all passed" — do not pin specific test counts. Counts drift; running counts serve only as quick diagnostic aids.


## Feature Test Matrix

MUST test the row that matches your change scope. Test counts drift; assert "all passed", not specific numbers.

| Change touches | Must also run |
|----------------|---------------|
| Anything (baseline) | `cargo test --lib` |
| `src/embedding/**`, `src/proxy/smart_embedder.rs` | `cargo test --features "embed-api" --lib` |
| `src/proxy/semantic_cache.rs`, `src/proxy/cache.rs` | `cargo test --features "embed-api" --lib` |
| `src/proxy/distill.rs`, `src/proxy/slug.rs`, `src/proxy/grpc/observability.rs` (distill handler), `src/config/mod.rs` (`DistillConfig`), `src/bin/conproxy/commands/proxy.rs` (`run_distill`), Python SDK `distill()` | `cargo test --lib` (also `--features "embed-api" --lib` if the distill handler's semantic join changed) |
| `src/proxy/cascade.rs` (RRF path) | `cargo test --features "embed-api" --lib` |
| New Cargo feature or `lints.clippy` change | full `cargo clippy -- -D warnings` + each feature surface |
| `src/proxy/persistence*`, `src/proxy/pgvector*` | `cargo test --features persistence --lib` and/or `--features pgvector --lib` |
| `src/proxy/sandbox.rs` | `cargo test --features linux-sandbox --lib` |
| CLI (`src/bin/conproxy/**`) | `make build-release` + `uat` vertical below |
| Python SDK (`sdk/python/**`) | maturin build + Python SDK vertical below |

## Test Verticals

### 1. Unit Tests

Inline `#[cfg(test)] mod tests` + dedicated `src/**/tests/*.rs` files.

```bash
# Default features
cargo test --lib

# With API providers
cargo test --features "embed-api" --lib

# With ONNX embedder
cargo build --features embed --lib  # hard gate; test may fail on linker on some systems
```

**Pass criteria:** all tests pass, no panics.

**Adding tests:** add to nearest `mod tests` block, or create `src/<module>/tests/<name>_tests.rs` (see `src/cache/tests/` for the pattern). Test modules need `#![allow(clippy::unwrap_used, clippy::expect_used)]` to satisfy `[lints.clippy]`.

### 2. Integration Tests

`tests/*.rs` at crate root: `proxy_test.rs`, `mcp_test.rs`, `seed_test.rs`, `proxy_cli_test.rs`, `e2e_sdk_python.rs`.

```bash
cargo test --tests
```

#### Testcontainers doctrine (backend proof)

Prefer **testcontainers** over bare `docker run` for single-backend proof. Multi-service e2e stays on compose (`tests/e2e/docker-compose.yml`).

| Proof type | Tool |
|------------|------|
| Single adapter | testcontainers (`integration_*`) |
| Dual-backend cascade | testcontainers multi-start |
| Full proxy soak / load | compose + `test_runner` |
| Peer 2-node | TC backend + 2 processes (`integration_peer`) |
| Cloud backends (Pinecone etc.) | mock default; live only under `#[ignore]` + secrets |

Default compose = qdrant + elasticsearch + meilisearch (×2) + postgres. OpenSearch via TC (`integration_opensearch`) or an explicit compose profile — do not assume `e2e-elastic` means Elasticsearch.

**Add a backend container**

1. Add `start_*` / `*_container()` helper in `tests/test_infra/containers.rs` (image pin, wait-for-HTTP/SQL, return `ContainerInstance` with `base_url`).
2. Add `tests/integration_<backend>.rs` using that helper.
3. Register `[[test]]` in `Cargo.toml` with `required-features = ["integration-tests"]` (and `pgvector` when needed).
4. Gate with `#![cfg(feature = "integration-tests")]` (already on existing binaries). Run via:

```bash
make test-integration
# equivalent:
cargo test --features integration-tests --test integration_qdrant
cargo test --features integration-tests --test integration_elasticsearch
cargo test --features integration-tests --test integration_meilisearch
cargo test --features "integration-tests,pgvector" --test integration_pgvector
```

**CI expectations:** nightly/scheduled job with Docker runs `make test-integration`. PR jobs stay unit/clippy unless labeled `run-integration`.

### 3. Feature-Gated Tests

Each feature surface compiles different modules. Test the surface your code uses:

```bash
cargo test --features embed-api --lib
cargo test --features persistence --lib
cargo test --features pgvector --lib
cargo test --features mcp --lib
cargo test --features release --lib
```

### 4. Lint

```bash
cargo fmt -- --check         # formatting
cargo clippy -- -D warnings  # lints, treat as errors
```

Lints configured in `Cargo.toml` under `[lints.clippy]`: `unwrap_used`, `expect_used`, `panic`, `indexing_slicing`, `arithmetic_side_effects`, `suspicious`, `style`, `complexity`, `perf` all `warn`. New `unwrap`/`expect` in non-test src is a lint violation; promote to `?` or `ok_or_else`.

**Security-focused lints** (run in `make test-all-security`): clippy with `unwrap_used`, `expect_used`, `panic`, `indexing_slicing`, `arithmetic_side_effects`. Surfaces all pre-existing unwraps in `#[cfg(test)] mod tests` blocks (each allowed by `#![allow(clippy::unwrap_used)]`). Make target: `make lint-security`. For the current count, run `make audit-known-gaps`.

### 5. Coverage

```bash
# HTML report
cargo tarpaulin --out Html

# Quick stdout, no clean rebuild
cargo tarpaulin --lib --skip-clean --out Stdout
```

Per-file threshold: 80% (`make test-coverage-check`). Fail the PR if a touched file drops below threshold.

### 6. Benchmarks

Criterion, two bench targets:

- `core_ops` (`benches/core_ops.rs`) — cache, slugify, serde, query hash
- `cascade_scope` (`benches/cascade_scope.rs`) — `fuse_rrf` + `ScopeFilter::best_sim`

```bash
# Run all benches (one or both targets)
cargo bench --bench core_ops
cargo bench --bench cascade_scope

# Save baseline for comparison
cargo bench --bench core_ops -- --save-baseline main
cargo bench --bench cascade_scope -- --save-baseline main

# Compare against baseline (Criterion's --baseline; panics if main/ missing)
cargo bench --bench core_ops -- --baseline main
cargo bench --bench cascade_scope -- --baseline main

# End-to-end perf diagnosis loop (CI-aware verdict from raw new/ + main/)
make perf-tuning-quick   # or `default` or `full`; see .opencode/commands/perf-tuning.md
```

Regression threshold: 15% slower than baseline → investigate before merging.

CI gate: `.github/workflows/perf.yml` — default-branch pushes refresh the
criterion baseline cache (`make bench-save`); PRs run
`make perf-tuning-quick` with `--fail-on-regression` (exit 2 on ≥15%
CI-bound regression). Treat CI failures as signals, reproduce locally
before acting (shared runners are noisy).

#### Hit-rate benchmark (bench-hitrate)

`hitrate_bench` measures cache hit rates on synthetic/real workloads —
the strategy-doc product number. Not a per-PR gate; run on demand.

```bash
make bench-hitrate        # exact hit rate, real CacheStore (fast, CI-safe)
make bench-hitrate-sem    # + semantic τ frontier, synthetic embedder (feature embed-api, ~6 min)
make bench-hitrate-onnx   # + live ONNX embedder (feature embed; needs ORT + model)
make bench-hitrate-live   # end-to-end wire test: docker qdrant + real proxy (embed build)
```

Key flags: `--ttl SECS` (virtual-clock expiry), `--mutation-rate P` +
`--cdc-delay SECS` (stale-hit model + what-if CDC), `--queries-file`
(MS MARCO/ORCAS text pool), `--embedder synthetic|onnx|api` +
`--embed-provider mock` (API wire path, no keys). Verdicts: FAIL-CORE
(exit 2, agentic exact < 40%), FAIL-TRUST (exit 3, no τ clears 1%
false-hit gate). See `docs/strategy-assessment.md` §3 for as-built
semantics and measured results.
`perf_summarize` (used by `/perf-tuning`) computes a 95% CI from independent-sample
SEs and only flags a regression when the CI lower bound exceeds the threshold —
fewer false positives than raw percent change.

### 7. E2E Proxy Tests

Categories live in `tests/e2e_proxy/categories/`. Requires Docker services (ES + Qdrant) + proxy running.

```bash
# Start Docker services
make e2e-services-up

# Wait + load test data
make e2e-wait
make e2e-load-data

# Run all E2E tests
make e2e-all

# Vertical subsets
make e2e-qdrant
make e2e-elastic
make e2e-mixed
make e2e-filter FILTER=cache

# Quick smoke (proxy must be running)
make e2e-smoke

# With perf/bpftrace profiling
make e2e-profile
```

**Suites** (env var `E2E_SUITE`): `qdrant`, `elastic`, `mixed`, `all` (default).
**Filter** (env var `E2E_FILTER`): comma-separated category names.
**Profile mode** (`E2E_PROFILE=1`): adds perf/bpftrace/cgroup profiling via `proc-monitor`.

**Pass criteria:** `tests/e2e/results/<timestamp>/results.json` written, all tests `status: pass`. View with `make e2e-results` (prints `jq` of latest) or `make e2e-report` (markdown + HTML report).

**Adding tests:** drop a `tests/e2e_proxy/categories/<name>.rs` module + add to `tests/e2e_proxy/categories/mod.rs`. Use `helpers::client::E2eClient`, `helpers::proxy::ProxyProcess`, `helpers::report::TestReport`. Mark with `#[ignore = "E2E: ..."]` (these tests are `--ignored` by design).

### 8. Load Tests

rlt-based gRPC + HTTP load.

```bash
# Build + run load
make e2e-bench

# Direct cargo invocation
PROXY_URL=http://localhost:8081 GRPC_URL=http://localhost:8080 \
  cargo test --test e2e_load --features load-test,e2e --release
```

Env vars: `PROXY_URL`, `GRPC_URL`, `DURATION` (seconds), `BENCH_OUTPUT_DIR`.

### 9. Eval Tests (LLM Vertical Comparison)

Compares conproxy with `no_context` / `mcp_tools` against Ollama or Claude.

```bash
# Full eval
make eval-all

# Quick (services already up)
make eval-quick

# Single vertical
make eval-vertical V=no_context

# Subset of queries
make eval-queries Q=q-001,q-003

# Cheap (2 verticals × 3 queries, for quick Ollama eval)
make eval-cheap
```

**Provider:** Ollama default (checks `http://localhost:11434/api/tags`). Override with `EVAL_PROVIDER=claude` and `ANTHROPIC_API_KEY=<key>`. **Skip:** eval is skipped if no provider is running (logged in results).

**Pass criteria:** `tests/e2e_eval/results/<timestamp>/eval_results.json` + HTML report. View with `make eval-results`.


- **eval_llamacpp** — `make eval-llamacpp`. Same eval harness as Ollama/Claude but with `EVAL_PROVIDER=llamacpp` (`OpenAiCompatRunner`). Hard-fail.
- **tilt** — `tilt up` / `tilt down` from repo root. kind cluster (`deploy/tilt/kind-config.yaml`) + conproxy pod. See `deploy/tilt/Tiltfile`.

### 10. UAT (CLI User Acceptance)

Tests CLI commands end-to-end against a running proxy.

```bash
make uat          # builds + runs
make uat-quick    # assumes binary built
```

Tests live in `tests/e2e_uat/main.rs` covering: start/stop, status, context.

### 11. Security

| Sub-vertical | Command | Frequency |
|--------------|---------|-----------|
| `audit` | `cargo audit` | every PR |
| `security-deny` | `cargo deny check` | every PR |
| `lint-security` | `cargo clippy -- -D warnings -W clippy::{unwrap_used,expect_used,panic,indexing_slicing,arithmetic_side_effects}` | every PR |
| `e2e-security` | auth bypass, rate limit, payload abuse, header injection | nightly |
| `sbom` | `cargo cyclonedx --format json --output-file conproxy-sbom.json` | on release |
| `unsafe-audit` | `cargo geiger --output-format ascii` | on release |
| `mutant-security` | `cargo mutants -F src/proxy/middleware.rs -F src/proxy/sandbox.rs -F src/proxy/agent.rs` | on release |
| `fuzz-query` / `fuzz-config` / `fuzz-all` | `cargo fuzz run <target> -- -max_len=4096 -max_total_time=60` | on release |

**Fast gate** (run every PR, < 2 min): `make security-quick` = `audit` + `security-deny` + `lint-security`.
**Full suite** (nightly/weekly): `make security-full` = `security-quick` + `sbom` + `unsafe-audit` + `fuzz-all` + `e2e-security`.

**Fuzz targets:** `fuzz_query_request` (QueryRequest deserialization), `fuzz_config_parse` (config TOML parsing). Requires nightly toolchain.

**Mutation targets:** `src/proxy/middleware.rs` (auth), `src/proxy/sandbox.rs` (seccomp), `src/proxy/agent.rs` (agent auth/routing). These are the most security-critical modules.

### 12. Profiling

DHAT heap, perf CPU, bpftrace syscall/lock/network, cgroup.

For performance **optimization workflow** (diagnose → measure → change → re-measure), see the `performance` skill.

```bash
# DHAT heap profile (quick)
make profile-dhat  # writes /tmp/conproxy-dhat/dhat-heap.json

# CPU flamegraph
make profile-flamegraph  # requires samply OR perf+inferno

# Full profiling build
make build-profiling  # target/profiling/conproxy

# E2E with profiling
make e2e-profile  # sets E2E_PROFILE=1

# Eval with profiling
make eval-profile
```

**DHAT viewer:** https://nnethercote.github.io/dh_view/dh_view.html (load the `.json`).

**bpftrace scripts** in `tests/e2e/profiling/`: `syscall_profile.bt`, `lock_profile.bt`, `net_profile.bt`. Require `bpftrace` + kernel BTF.

### 13. Python SDK

`sdk/python/` is a maturin project producing `conproxy_py` (Rust module) + pure-Python submodules (`conproxy_py.langchain`, `conproxy_py.llama_index`).

```bash
# Build (dev install, editable)
cd sdk/python
maturin develop --release

# Test
cargo test --test e2e_sdk_python

# Adapter syntax check (no framework install needed)
python3 -c "import ast; [ast.parse(open(f).read()) for f in ['src/langchain.py', 'src/llama_index.py', 'examples/langchain_rag.py', 'examples/llama_index_rag.py']]"

# Run LangChain RAG example
pip install -e .[langchain]
python3 examples/langchain_rag.py

# Run LlamaIndex RAG example
pip install -e .[llama-index]
python3 examples/llama_index_rag.py
```

**Optional deps** in `pyproject.toml`: `[langchain]` (langchain-core), `[llama-index]` (llama-index-core).

### 14. Full Suite (`make test-all`)

End-to-end pipeline. Results in `tests/results/<timestamp>/<profile>/`. HTML dashboard via `test_runner index`.

```bash
make test-all
```

Pipeline:

1. **`test-all-prebuild`** — release binary + dev targets + test binaries (dhat, e2e, load, eval, uat)
2. **`test-all-quality`** (sequential):
   - `test-all-lint` — fmt + clippy
   - `test-all-unit` — all lib + bin + integration tests
   - `test-all-coverage` — tarpaulin
   - `test-all-security` — audit + deny + lint-security
3. **`test-all-perf`** (sequential, services up the whole time):
   - `test-all-bench` — criterion
   - `test-all-e2e` — E2E proxy + load tests (Docker up, proxy started with dhat)
   - `test-all-eval` — LLM eval (skipped if no Ollama/Claude)
4. **`test_runner index <dir>`** — generates `index.html` dashboard, per-section `report.html`, `comparison.md` if 2+ runs

**Resource profile** (cgroup v2 scoping): `RESOURCE_PROFILE=1cpu_512mb make test-all` or `RESOURCE_PROFILE=4cpu_2gb make test-all`. Default = 1 CPU, 512 MB.

**Time:** 20–40 min depending on perf stages.

## Dashboard ↔ MCP Observability Parity

Conproxy exposes status / observability data through two surfaces that **must stay in lockstep**:

1. **Web dashboard** — vanilla SPA at `/dashboard` (`ui/`). Reads JSON endpoints from `src/proxy/middleware.rs::WEB_UI_ALLOWLIST` (+ `/health`, `/pool`, `/peer/status`, `/debug/tokio`).
2. **MCP server** — stdio MCP server (`conproxy mcp`, src/mcp/). Exposes a status tool per dashboard panel.

### Rule

Any PR that touches **any** of the following **must** add / update MCP status tools in the same PR:

- `ui/app.js`, `ui/index.html`, `ui/style.css` — dashboard panel fetcher / header change
- `src/proxy/middleware.rs::WEB_UI_ALLOWLIST` — new allowlisted status path
- `src/proxy/server/mod.rs` routes under `WEB_UI_ALLOWLIST` or new status routes
- `src/mcp/status.rs` or new MCP status tool definitions

### Mirror table

| Dashboard panel | MCP tool | Endpoints fetched |
|-----------------|----------|-------------------|
| Status dot | `conproxy_health` | `/health` |
| Overview | `conproxy_overview` | `/metrics`, `/stats`, `/circuit` |
| Cache | `conproxy_cache_status` | `/stats`, `/pool`, `/cache/integrity` |
| Connection Pool | `conproxy_pool_status` | `/pool` |
| Circuit / Queue | `conproxy_circuit_status` | `/circuit`, `/queue` |
| Metrics | `conproxy_metrics_status` | `/metrics`, `/pool`, `/stats/queries` |
| Contexts | `conproxy_contexts_status` | `/contexts`, `/contexts/current` |
| Peer | `conproxy_peer_status` | `/peer/status` |
| Tokio | `conproxy_tokio_status` | `/debug/tokio` |

Admin actions (toggle) → MCP tool, same PR: `conproxy_reload`, `conproxy_apply_tune` (write + reload), `conproxy_pause`, `conproxy_resume` (already in MCP as part of the tune suite / future work).

### Feature Test Matrix row

| Surface | Test | Command |
|---------|------|---------|
| `ui/**`, `src/proxy/server/web_ui*`, `src/proxy/middleware.rs`, `src/mcp/status.rs` | UI/MCP status parity | `cargo test --features mcp --lib -- mcp::status` + `cargo test --features mcp --lib -- proxy::middleware::tests` |

### PR checklist

Add to the PR template:

- [ ] If dashboard panel changed: matching MCP status tool updated
- [ ] If `WEB_UI_ALLOWLIST` changed: new path is wired into a panel tool (or `conproxy_status_get` for raw getters)
- [ ] `cargo test --features mcp --lib` passes
- [ ] `cargo clippy --features mcp --lib -- -D warnings` passes

### Long-term

When admin UI controls land (pause / resume / clear cache / warm), ship the MCP mutation tool in the same PR. Mirror table above is the source of truth — if the dashboard gains a control, MCP gains the mutation.

## Known Gaps

Known issues that affect testing. Document in PRs that touch them. Run `make audit-known-gaps` to surface the current pre-existing-warning inventory fresh — line refs and counts change as code moves; the make target is authoritative.

1. **Default compose** (`tests/e2e/docker-compose.yml`) includes qdrant + meilisearch + postgres, but no Elasticsearch. Prefer `make test-integration` (testcontainers) for single-backend proof. Compose + `test_runner wait all` still required for multi-service E2E/Load/UAT.
2. **`embed` feature linker issue** — `cargo test --features embed --lib` may fail on some systems due to ONNX/ort native dependency linking at the test link stage. `cargo build --features embed --lib` is the hard gate. Use `--features embed-api` for routine provider tests (no ONNX). Not a Tier-3 full-test blocker.
3. **`e2e_eval` external deps** — needs Ollama **or** `EVAL_PROVIDER=claude` API key. **Out of** Tier-3 release gate (non-blocking). Nightly optional.
4. **`linux-sandbox`** — Linux-only; not in `release` meta-feature (`mcp`+`persistence`+`embed-api`+`pgvector`).
5. **Peer mesh v1** — trusted network only (no peer mTLS/shared-secret); LWW by wall timestamp.
6. **Lint-security surfaces unwraps in tests** — every unwrap lives in a `#[cfg(test)]` block with `#![allow(clippy::unwrap_used)]`. Expected pattern; run `make audit-known-gaps` for live inventory.

OpenSearch ships via ES-compatible adapter + container proof (`integration_opensearch`). Pinecone/Milvus HTTP adapters shipped (Wave 3). Solr removed.

## Contributor Workflow

### Branch naming

`<type>/<short-kebab-description>` — e.g. `fix/cache-eviction`, `feat/semantic-cache`, `docs/feature-flags`. No long-lived personal branches.

### Commit messages

Use the `caveman-commit` skill for terse Conventional Commits. Subject ≤ 50 chars, body only when "why" isn't obvious. Examples:

```
feat: add semantic cache tier
fix: prevent cache stampede on cold start
docs: document embed-api provider config
```

Repo currently has all commits msg "test" on `init` branch — historical, not a convention. New work should use proper commits.

### Pull request checklist

Before opening a PR:

- [ ] Smoke + Pre-PR Gate pass (see `AGENTS.md` §Tiers 1 and 2)
- [ ] Feature matrix row for your change scope passes
- [ ] Docs updated if user-facing behavior changed (`docs/configuration.md`, `docs/architecture.md`, `docs/feature-flags.md`, `docs/api-reference.md`, etc.)
- [ ] Tests added/updated for the change
- [ ] No new `unwrap`/`expect` in non-test src (lint-security clean)
- [ ] PR description: problem, approach, test verticals run, screenshots if UI/HTML output

### Code review expectations

- **One concern per PR** — split unrelated refactors into separate PRs
- **No drive-by refactors** — keep diffs scoped to the change
- **Tests are not optional** — every new code path must have a test
- **Docs stay in sync with code** — if you change config, CLI, or API, update docs
- **Verify before claiming complete** — re-read changed files, run the vertical, confirm green

### Code style

Defer to the **`rust-skills`** skill for Rust style. Key local conventions:

- `#![deny(unsafe_code)]` in `src/lib.rs:1` — all `unsafe` must have `// SAFETY:` comment AND `#[allow(unsafe_code)]` per block
- Lints in `Cargo.toml [lints.clippy]` — no new `unwrap`/`expect`/`panic` in non-test src
- Feature-gated code: `#[cfg(feature = "...")]` consistently
- Public API: `# Errors` section on pub Result fns; doc comments on pub items
- `?` for error propagation, `expect("invariant: ...")` only for true bugs
- Locks never held across `.await` (rule `async-no-lock-await`)

## See Also

- `Makefile` — run `make help` for the full target list
- `make audit-known-gaps` — surfaces current pre-existing warning inventory (clippy unwraps, bench compile, unallowed warnings)
- `docs/` — user-facing reference docs
- `Cargo.toml` — features + lint config
- `rust-skills` skill — Rust style guide
- `caveman-commit` skill — commit message format
