# Contributing to conproxy

Thanks for your interest in conproxy! This guide covers building, testing, and contributing to the project.

> **For AI agents**: see [`AGENTS.md`](AGENTS.md) and the [`contributing` skill](.opencode/skills/contributing/SKILL.md) for agent-specific guidance. This file is for human contributors.

## Getting Started

### Prerequisites

**Required:**

- Rust 1.75+ (edition 2021)
- Docker + Docker Compose (for E2E tests)

**By test vertical:**

- **Security:** `cargo-audit`, `cargo-deny`, `cargo-tarpaulin`, `cargo-cyclonedx`, `cargo-geiger`, `cargo-mutants`, `cargo-fuzz` (nightly)
- **Eval:** Ollama running locally, OR Claude API key
- **Profiling (Linux):** `bpftrace`, `perf`
- **Python SDK:** Python 3.9+, `maturin`

Install cargo tools:

```bash
cargo install cargo-audit cargo-deny cargo-tarpaulin cargo-cyclonedx cargo-geiger --locked
cargo install cargo-mutants --locked
cargo install cargo-fuzz  # nightly
```

### Build

```bash
# Debug
cargo build

# Release — standard production binary (mcp + persistence + embed-api + pgvector)
cargo build --release --features release

# With embedding
cargo build --release --features release,embed

# Full production (release + ONNX embed + Linux seccomp sandbox)
cargo build --release --features release,embed,linux-sandbox
```

## Testing

### Test Tiers

See [`AGENTS.md`](AGENTS.md) for the full tier ladder (Smoke + Pre-PR Gate).

- **Tier 1 — Smoke** (< 60 s, every save): fmt, clippy, `cargo test --lib`, `cargo test --features "embed-api" --lib`
- **Tier 2 — Pre-PR Gate** (~8 min cold): 14 commands including all feature-surface lints, workspace build, e2e check, and binary build


### Feature Test Matrix

| Change touches | Must also run |
|----------------|---------------|
| Anything (baseline) | `cargo test --lib` |
| Embedder, semantic cache, LLM, cascade, smart_embedder | `cargo test --features "embed-api" --lib` |
| Persistence, pgvector, sandbox, MCP | `cargo test --features <feature> --lib` |
| CLI changes | `make build-release` + `make uat` |
| Python SDK | `maturin develop` + `cargo test --test e2e_sdk_python` |

### Test Verticals

#### 1. Unit Tests

Inline `#[cfg(test)] mod tests` + dedicated `src/**/tests/*.rs` files.

```bash
cargo test --lib
cargo test --features "embed-api" --lib
```

#### 2. Integration Tests

`tests/*.rs` at crate root.

```bash
cargo test --tests
```

#### 3. Coverage

```bash
cargo tarpaulin --out Html
```

Per-file threshold: 80% (`make test-coverage-check`).

#### 4. Benchmarks

```bash
cargo bench --bench core_ops
cargo bench --bench core_ops -- --save-baseline main
cargo bench --bench core_ops -- --baseline main
```

Regression threshold: 10% slower than baseline → investigate.

#### 5. E2E Proxy Tests

Categories live in `tests/e2e_proxy/categories/`. Requires Docker services.

```bash
make e2e-services-up
make e2e-wait
make e2e-load-data
make e2e-all                  # all suites
make e2e-qdrant               # Qdrant only
make e2e-elastic              # Elasticsearch only
make e2e-mixed                # mixed upstreams
make e2e-filter FILTER=cache  # category filter
make e2e-smoke                # quick smoke (proxy must be running)
make e2e-profile              # with perf/bpftrace profiling
```

Suites (`E2E_SUITE` env var): `qdrant`, `elastic`, `mixed`, `all` (default).
Filter (`E2E_FILTER` env var): comma-separated category names.

#### 6. Load Tests

```bash
make e2e-bench
```

Env vars: `PROXY_URL`, `GRPC_URL`, `DURATION` (seconds), `BENCH_OUTPUT_DIR`.

#### 7. Eval Tests (LLM Vertical Comparison)

```bash
make eval-all
make eval-vertical V=no_context
make eval-queries Q=q-001,q-003
make eval-cheap
```

Provider: Ollama default (checks `http://localhost:11434/api/tags`). Override with `EVAL_PROVIDER=claude` + `ANTHROPIC_API_KEY`.



```bash
# Prereqs: llama-server on PATH, models/llm.gguf present
make llm-server-check   # verify /v1/models responds
```

`make eval-llamacpp` runs the eval suite with `EVAL_PROVIDER=llamacpp` (an OpenAI-compatible provider that talks to llama-server via the `OpenAiCompatRunner`). Same hard-fail semantics. Requires the e2e services (qdrant, meilisearch, postgres) to be up.

```bash
make eval-llamacpp
```



#### 8. UAT (CLI User Acceptance)

```bash
make uat
make uat-quick  # assumes binary built
```

#### 9. Security

| Sub-vertical | Command | When |
|--------------|---------|------|
| Audit | `cargo audit` | Every PR |
| Deny | `cargo deny check` | Every PR |
| Lint security | `make lint-security` | Every PR |
| E2E security | `make e2e-security` | Nightly |
| SBOM | `make sbom` | Release |
| Unsafe audit | `make unsafe-audit` | Release |
| Mutation | `make mutant-security` | Release |
| Fuzz | `make fuzz-all` | Release |

Fast gate: `make security-quick`. Full suite: `make security-full`.

#### 10. Profiling

```bash
make profile-dhat        # DHAT heap profile
make build-profiling     # profiling build
make e2e-profile         # E2E with profiling
make eval-profile        # Eval with profiling
```

DHAT viewer: <https://nnethercote.github.io/dh_view/dh_view.html>

#### 11. Python SDK

```bash
cd sdk/python
maturin develop --release
cargo test --test e2e_sdk_python
```

Optional dependencies for adapters:

```bash
pip install -e .[langchain]    # LangChain adapter
pip install -e .[llama-index]  # LlamaIndex adapter
```

#### 12. Full Suite

```bash
make test-all
```

Pipeline: prebuild → quality (lint + unit + coverage + security) → perf (bench + e2e + load + eval) → HTML dashboard. Results in `tests/results/<timestamp>/<profile>/`. Time: 20–40 min.

Resource profile: `RESOURCE_PROFILE=1cpu_512mb make test-all`.

## Code Style

We follow the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/) plus local conventions:

- **`#![deny(unsafe_code)]`** — every `unsafe` block must have a `// SAFETY:` comment AND `#[allow(unsafe_code)]` per block
- **No `unwrap`/`expect`/`panic` in non-test src** — enforced by `[lints.clippy]` in `Cargo.toml`. Use `?` for propagation, `ok_or_else` for recoverable errors, `expect("invariant: ...")` only for true bugs
- **Locks never held across `.await`** — use `std::sync::Mutex`/`RwLock` with block-scoping, or `tokio::sync` primitives
- **Public API documentation** — `# Errors` section on pub Result fns, doc comments on pub items
- **Feature-gated code** — `#[cfg(feature = "...")]` consistently; new features need both gate + doc
- **Commit messages** — Conventional Commits, subject ≤ 50 chars, body only when "why" isn't obvious

## Pull Request Process

### Branch naming

`<type>/<short-kebab-description>` — e.g. `fix/cache-eviction`, `feat/semantic-cache`.

### Before Opening a PR

- [ ] Smoke + Pre-PR Gate pass (see `AGENTS.md` §Tiers 1 and 2)
- [ ] Feature matrix row for your change scope passes
- [ ] Tests added/updated for the change
- [ ] No new `unwrap`/`expect` in non-test src
- [ ] Docs updated if user-facing behavior changed
- [ ] PR description: problem, approach, test verticals run, screenshots if applicable

### Review Process

1. Open a PR against the main branch
2. CI runs the full test suite
3. Maintainer review
4. Address feedback
5. Squash-merge

### Required CI status checks (PR gate)

`main` / default branch is protected. PRs must show green from the
six `ci.yml` jobs:

- `ci / unit` — fmt, clippy (all feature surfaces + bins), lib
  tests (default / embed-api / mcp / release), mcp_test, build
  (workspace + embed), release binary smoke + install-sim.
- `ci / integration` — `make test-integration` (testcontainers
  real-backend matrix: qdrant, ES, OS, meili, pgvector, cascade,
  peer, circuit, batch, metrics, context_config, singleflight).
- `ci / integration-experimental` — `make test-integration-experimental`
  (pinecone + milvus mocks, ~2 min, no real backends).
- `ci / security` — `cargo audit` (RustSec), `cargo deny check`
  (supply chain + license), clippy with security lints
  (unwrap/expect/panic/indexing_slicing/arithmetic_side_effects).
- `ci / fuzz` — `cargo fuzz run` on all 5 targets, 60s each, best
  effort. Crash repros uploaded as `fuzz-artifacts` artifact.
- `ci / e2e` — `needs: [unit, integration]`. Builds release binary,
  `docker compose pull` (GHA cache), brings up the e2e compose,
  loads data, runs the **full** ignored `e2e_proxy_suite` (all
  phases: smoke/health/query, cascade, load, reload, observability,
  efficiency, advanced, security) + UAT (`e2e_uat`) + security-focused
  e2e filter. Results uploaded as `e2e-results` artifact. Tears
  down on completion.

`unit`, `integration`, `integration-experimental`, `security`, and
`fuzz` start in parallel; `e2e` is gated on `unit` + `integration` so
the heavy compose + load + ignored suite never runs if the cheaper
gates would block the merge.

**Out of the PR gate** (lives in `.github/workflows/coverage.yml`,
weekly cron + workflow_dispatch):
- `coverage / coverage` — `cargo tarpaulin --lib --bins` + per-file
  80% line gate via `make test-coverage-check`. Report uploaded as
  `coverage-results` artifact. Tarpaulin takes ~10 min on cold cache
  and is non-deterministic across runners, so we keep it off the PR
  path.

`release.yml` (`v*` tag / `workflow_dispatch`) **publishes artifacts
only** — no test execution. By the time a tag lands, every commit
on the default branch has already passed the full `ci.yml` gate.
Release steps: cross-compile (x86_64-musl + aarch64-gnu), aarch64
qemu smoke, multi-arch container (amd64 + arm64) to GHCR, multi-arch
manifest + `:latest` tag, OCI Helm chart to GHCR, GitHub Release
with cross binaries + chart `.tgz` attached.

## Code Review Expectations

- **One concern per PR** — split unrelated refactors
- **No drive-by refactors** — keep diffs scoped
- **Tests are not optional** — every new code path needs a test
- **Docs stay in sync with code** — config/CLI/API changes need doc updates
- **Verify before claiming complete** — re-read files, run the vertical, confirm green

## Known Gaps

1. **Default compose** (`tests/e2e/docker-compose.yml`) includes qdrant + Elasticsearch 8.13 + meilisearch×2 + postgres. Run `docker compose -f tests/e2e/docker-compose.yml up -d` then `cargo run --bin test_runner -- wait all` before E2E/Load/Eval/Full suite.
2. **`embed` feature linker issue** — pre-existing ONNX/ort linking problem on some systems. Workaround: test with `--features embed-api`.
3. **Lint-security surfaces pre-existing unwraps** — every unwrap lives in a `#[cfg(test)] mod tests` block, each annotated `#![allow(clippy::unwrap_used)]`. Not a real violation; expected pattern. Run `make audit-known-gaps` for the current count.

## See also

- [`AGENTS.md`](AGENTS.md) — always-loaded agent rules + tier ladder
- [`.opencode/skills/contributing/SKILL.md`](.opencode/skills/contributing/SKILL.md) — 14 vertical deep dives
- `make audit-known-gaps` — current pre-existing-warning inventory
- `make help` — full Make target list

## License

MIT. By contributing, you agree your contributions will be licensed under MIT.
