# Makefile for conproxy (cache proxy server)

# Use bash + pipefail so pipelines like `cargo bench | tee file` propagate
# cargo's real exit code. Without it, `tee` masks build/bench failures and the
# perf_summarize step reads stale data and reports a false PASS.
SHELL := /bin/bash
.SHELLFLAGS := -eu -o pipefail -c

.PHONY: all build build-release test test-unit test-integration test-integration-experimental lint fmt clean help build-embed build-persistence build-pgvector build-mcp build-all build-profiling bench bench-save bench-compare profile-dhat profile-pgo profile-pgo-clean profile-flamegraph profile-tokio-console profile-heaptrack profile-metrics-snap perf-tuning-quick perf-tuning-default perf-tuning-full e2e-all e2e-dirty e2e-qdrant e2e-elastic e2e-meili e2e-mixed e2e-filter e2e-smoke e2e-smoke-core e2e-cascade e2e-federated e2e-bench e2e-services-up e2e-services-down e2e-wait e2e-load-data e2e-results e2e-report e2e-bench-compare e2e-proxy-clean llm-server-check  eval-llamacpp eval-all eval-quick eval-vertical eval-queries eval-cheap eval-results eval-clean uat uat-quick e2e-profile eval-profile test-all test-all-prebuild test-all-lint test-all-unit test-all-bench test-all-coverage test-all-e2e test-all-eval test-all-quality test-all-perf test-all-security test-coverage-check proxy-start proxy-stop proxy-status audit security-deny lint-security audit-known-gaps e2e-security sbom unsafe-audit mutant-security fuzz-query fuzz-config fuzz-all security-quick security-full profile-pgo profile-pgo-clean cov-scope-tune proof-cascade sdk-smoke perf-tuning-clean bench-hitrate bench-hitrate-sem bench-hitrate-onnx bench-hitrate-live perf-publish fmt-check test-one test-verbose test-coverage-quick e2e-generate-embeddings e2e-k8s docker-build docker-push dev-up dev-down dev-restart devex devex-attach devex-status devex-new devex-banner t test-fast test-nextest test-slow test-filter target-prune nextest-install docker-buildx

# E2E infra directory (docker-compose lives here). Override on the command
# line, e.g. `make e2e-services-up E2E_PROXY_DIR=/path/to/compose`. The
# default assumes tests/e2e/docker-compose.yml next to the Makefile.
E2E_PROXY_DIR ?= $(CURDIR)/tests/e2e

# Default target
all: fmt lint test build

# ============================================================================
# Build
# ============================================================================

build:
	cargo build

build-release:
	cargo build --release --features release --bin conproxy

# Feature-specific builds
build-embed:
	cargo build --release --features embed

build-persistence:
	cargo build --features persistence

build-pgvector:
	cargo build --features pgvector

build-mcp:
	cargo build --release --features mcp

# Release build with all standard features (excludes embed, pgvector, persistence, linux-sandbox which need external deps)
build-all:
	cargo build --release

# Build with profiling profile (release + debug symbols for perf/bpftrace + dhat heap profiling)
build-profiling:
	cargo build --profile profiling --features dhat-heap

# Binary path for test-all pipeline (dev profile = debug symbols for perf + dhat-heap)
TEST_BIN = $(CURDIR)/target/debug/conproxy
TEST_RUNNER_BIN = $(CURDIR)/target/debug/test_runner

# Feature set for test-all pipeline. 'test' = load-test + dhat-heap, so all
# dev-profile targets share one feature set (no feature-switch recompilation between stages).
TEST_ALL_FEATURES ?=

# Resource profile for cgroup v2 scoping (default = 1 CPU, 512 MB)
RESOURCE_PROFILE ?= default
_PROFILE_NAME = $(or $(RESOURCE_PROFILE),default)

# ============================================================================
# Test
# ============================================================================

# Run all tests (unit)
test:
	cargo clippy -- -D warnings
	cargo test

# Local iteration loop (see docs/dev-loop.md).
#
# t            - run the unit tests; uses cargo-nextest with the `dev`
#                profile when installed, falls back to `cargo test --lib`
#                otherwise. Default features only. Designed for the
#                save-and-run loop. Warm wall ~3-4s on a 16-core host.
# test-fast    - same as t; alias used in some docs.
# test-nextest - force cargo-nextest with the dev profile (no fallback).
# test-slow    - list the top slow tests via nextest.
# test-filter  - run tests matching $PAT; uses nextest if available.
# target-prune - drop the conproxy-only build artifacts; safe to re-run.
# nextest-install - one-time install of cargo-nextest.
#
# Cargo.toml [profile.dev] already sets debug = "line-tables-only" and
# split-debuginfo = "unpacked" to shrink test binaries. .cargo/config.toml
# forces lld on Linux for faster links. See docs/dev-loop.md for the
# rationale + measurements.
NEXTEST ?= $(shell command -v cargo-nextest 2>/dev/null)

t:
	@if [ -n "$(NEXTEST)" ]; then \
		cargo nextest run --profile dev --lib 2>&1 | tail -n 30; \
	else \
		echo "cargo-nextest not found; falling back to cargo test --lib"; \
		echo "Tip: run 'make nextest-install' for the fast path"; \
		cargo test --lib -q 2>&1 | tail -n 30; \
	fi

test-fast: t

test-nextest:
	@if [ -z "$(NEXTEST)" ]; then \
		echo "cargo-nextest not installed. Run: make nextest-install" >&2; \
		exit 1; \
	fi
	cargo nextest run --profile dev --lib

test-slow:
	@if [ -z "$(NEXTEST)" ]; then \
		echo "cargo-nextest not installed. Run: make nextest-install" >&2; \
		exit 1; \
	fi
	# nextest 0.9 doesn't have --show-slowest N; status-level=slow + final summary
	# surfaces the slowest tests after the run.
	cargo nextest run --profile dev --lib --status-level slow --final-status-level slow 2>&1 | tail -n 40

# Usage: make test-filter PAT=foo
test-filter:
	@if [ -z "$(PAT)" ]; then \
		echo "Usage: make test-filter PAT=<substring>" >&2; \
		exit 1; \
	fi
	@if [ -n "$(NEXTEST)" ]; then \
		cargo nextest run --profile dev --lib --filter-expr 'test(=PAT)' 2>/dev/null \
			|| cargo nextest run --profile dev --lib "$(PAT)"; \
	else \
		cargo test --lib -- "$(PAT)"; \
	fi

target-prune:
	@echo "Pruning conproxy-only build artifacts..."
	@rm -rf target/debug/deps target/debug/incremental target/.rustc_info.json 2>/dev/null || true
	@find target/debug -maxdepth 1 -type f -name 'conproxy-*' -newer Cargo.toml -delete 2>/dev/null || true
	@echo "Done. Use 'cargo clean' for a full wipe."

nextest-install:
	@if [ -n "$(NEXTEST)" ]; then \
		echo "cargo-nextest already installed: $(NEXTEST)"; \
	else \
		cargo install cargo-nextest --locked --version '^0.9'; \
	fi

# Unit tests only (fast)
test-unit:
	cargo test --lib

# Backend integration tests (requires Docker daemon + testcontainers).
# Plan 09: full non-experimental matrix. PR: optional label run-integration;
# nightly: full target. Experimental backends: test-integration-experimental.
test-integration:
	cargo test --features integration-tests --test integration_qdrant
	cargo test --features integration-tests --test integration_elasticsearch
	cargo test --features integration-tests --test integration_opensearch
	cargo test --features integration-tests --test integration_meilisearch
	cargo test --features "integration-tests,pgvector" --test integration_pgvector
	cargo test --features integration-tests --test integration_cascade
	cargo test --features integration-tests --test integration_peer
	cargo test --features integration-tests --test integration_circuit
	cargo test --features integration-tests --test integration_batch
	cargo test --features integration-tests --test integration_metrics
	cargo test --features integration-tests --test integration_context_config
	cargo test --features integration-tests --test integration_singleflight

# Mock / non-live experimental backends (Pinecone, Milvus) — not in default gate.
test-integration-experimental:
	cargo test --features integration-tests --test integration_pinecone
	cargo test --features integration-tests --test integration_milvus

# Plan 09 T4: fast e2e smoke (Docker services + data + proxy). Health + query + cache hit.
e2e-smoke-core: build-release
	@echo "=== E2E smoke core ==="
	$(MAKE) e2e-services-up
	$(MAKE) e2e-wait
	$(MAKE) e2e-load-data
	E2E_SUITE=qdrant E2E_FILTER=smoke,health,query \
		E2E_OUTPUT_DIR="$(E2E_PROXY_DIR)/results/smoke-$(shell date +%Y%m%d-%H%M%S)" \
		$(E2E_CARGO_CMD) e2e_proxy_suite
	$(MAKE) e2e-services-down

# Plan 09 T4/T5: cascade-focused e2e (multi-upstream / cascade metrics).
e2e-cascade: build-release
	@echo "=== E2E cascade ==="
	$(MAKE) e2e-services-up
	$(MAKE) e2e-wait
	$(MAKE) e2e-load-data
	E2E_SUITE=mixed E2E_FILTER=cascade \
		E2E_OUTPUT_DIR="$(E2E_PROXY_DIR)/results/cascade-$(shell date +%Y%m%d-%H%M%S)" \
		$(E2E_CARGO_CMD) e2e_proxy_suite
	$(MAKE) e2e-services-down

# Plan 09 T4: federated category (self-hosts mock upstream inside suite).
e2e-federated: build-release
	@echo "=== E2E federated (mock upstream) ==="
	E2E_FILTER=federated_search \
		E2E_OUTPUT_DIR="$(E2E_PROXY_DIR)/results/federated-$(shell date +%Y%m%d-%H%M%S)" \
		$(E2E_CARGO_CMD) e2e_proxy_suite

test-verbose:
	cargo test -- --nocapture --test-threads=1

test-coverage:
	cargo tarpaulin --out Html

test-coverage-quick:
	cargo tarpaulin --lib --skip-clean --out Stdout

# Check per-file coverage threshold (requires tarpaulin JSON report)
test-coverage-check:
	@echo "=== Coverage Threshold Check (80% per file) ==="
	@echo "NOTE: check_coverage binary lives in parent repo. Run from parent: make test-coverage-check"

# Plan 05: coverage gate for scope + tune modules (≥80% line floor).
# Runs tarpaulin on the two modules and asserts both meet the threshold.
# Outputs per-file percentages + final pass/fail. Exit 0 = pass, 1 = fail.
# Wire into CI via `make cov-scope-tune` once baseline is met.
cov-scope-tune:
	@echo "=== Scope + Tune Coverage Gate (≥80% line) ==="
	@_FAIL=0; \
	for _pat in 'src/proxy/scope.rs' 'src/proxy/tune/*'; do \
		_out=$$(cargo tarpaulin --lib --skip-clean --include-files "$$_pat" --out Stdout 2>/dev/null | grep -E '^/.*: [0-9]+/[0-9]+|coverage, ' | tail -2); \
		echo ""; \
		echo "--- Pattern: $$_pat ---"; \
		echo "$$_out"; \
		_pct=$$(echo "$$_out" | grep -oE '[0-9]+\.[0-9]+% coverage' | tail -1 | sed 's/%.*//'); \
		if [ -n "$$_pct" ] && awk "BEGIN {exit !($${_pct} >= 80.0)}"; then \
			echo "  PASS: $${_pct}% ≥ 80%"; \
		else \
			echo "  FAIL: $${_pct:-<unknown>}% < 80%"; \
			_FAIL=1; \
		fi; \
	done; \
	echo ""; \
	if [ "$$_FAIL" = "0" ]; then \
		echo "cov-scope-tune: PASS"; \
	else \
		echo "cov-scope-tune: FAIL"; \
	fi; \
	exit $$_FAIL

# Plan 05: Python SDK smoke — build wheel via maturin and import the module.
# Requires maturin (pip install maturin) + python3-dev (header) for pyo3 build.
# Optional: also tests instantiation if CONPROXY_SDK_SMOKE_CLIENT=1 and a
# conproxy process is reachable; default is import-only to stay offline.

# Plan 08 W2: lead-story unit proofs (cascade + federated) — no Docker.
proof-cascade:
	@echo "=== Cascade + Federated unit proofs ==="
	cargo test --lib cascade -- --nocapture
	cargo test --lib federated -- --nocapture
	cargo test --lib test_handler_query_with_cascade -- --nocapture
	cargo test --lib test_grpc_federated -- --nocapture
	@echo "proof-cascade: PASS"

sdk-smoke:
	@echo "=== Python SDK Smoke (maturin build + import) ==="
	@if ! command -v maturin >/dev/null 2>&1; then \
		echo "FAIL: maturin not found. Install via: pip install maturin"; \
		exit 1; \
	fi
	@rm -rf /tmp/conproxy-sdk-smoke
	@cd sdk/python && maturin build --release 2>&1 | tail -3
	@_WHL=$$(ls -t target/wheels/conproxy-*.whl 2>/dev/null | head -1); \
	if [ -z "$$_WHL" ]; then echo "FAIL: no wheel found in target/wheels/"; exit 1; fi; \
	echo "Built wheel: $$_WHL"; \
	python3 -m venv /tmp/conproxy-sdk-smoke && \
	/tmp/conproxy-sdk-smoke/bin/pip install --quiet "$$_WHL" 2>&1 | tail -2 && \
	/tmp/conproxy-sdk-smoke/bin/python -c \
		"import conproxy; c = conproxy.ConproxyClient; print('import: OK'); print('class:', c)"
	@echo "sdk-smoke: PASS"

# Run specific test
test-one:
	cargo test $(TEST) -- --nocapture

# ============================================================================
# Lint & Format
# ============================================================================

fmt:
	cargo fmt

fmt-check:
	cargo fmt -- --check

# Build the conproxy Docker image (used by Tilt/kind dev loop).
#
# Local dev (default):   make docker-build                 -> conproxy:dev
# Versioned release:     make docker-build VERSION=0.1.0
#                        make docker-push  VERSION=0.1.0 REGISTRY=ghcr.io/me
# Multi-arch smoke:      make docker-buildx VERSION=0.1.0 PLATFORMS=linux/amd64,linux/arm64
VERSION ?=
REGISTRY ?=
IMAGE = $(if $(REGISTRY),$(REGISTRY)/conproxy,conproxy)
DOCKER_TAGS = $(if $(VERSION),-t $(IMAGE):$(VERSION) -t $(IMAGE):v$(VERSION),-t $(IMAGE):dev)

docker-build:
	docker build $(DOCKER_TAGS) .

docker-push: docker-build
	@if [ -z "$(VERSION)" ] || [ -z "$(REGISTRY)" ]; then \
		echo "VERSION and REGISTRY are required for docker-push (e.g. make docker-push VERSION=0.1.0 REGISTRY=ghcr.io/me)" >&2; \
		exit 1; \
	fi
	docker push $(IMAGE):$(VERSION)
	docker push $(IMAGE):v$(VERSION)

# Local multi-arch build via buildx (smoke; release job does this on GH).
docker-buildx:
	@command -v docker >/dev/null 2>&1 || { echo "docker required"; exit 1; }
	docker buildx build --platform $(PLATFORMS) $(DOCKER_TAGS) --load .

lint:
	cargo clippy -- -D warnings

lint-fix:
	cargo clippy --fix --allow-dirty

# ============================================================================
# Proxy Operations
# ============================================================================

proxy-start:
	./target/release/conproxy start

proxy-stop:
	./target/release/conproxy stop

proxy-status:
	./target/release/conproxy status

# ============================================================================
# Benchmarking
# ============================================================================

bench:
	cargo bench --bench core_ops
	cargo bench --bench cascade_scope

bench-save:
	@if [ -z "$(FORCE)" ] && [ -n "$$(git status --porcelain 2>/dev/null)" ]; then \
		echo "ERROR: git tree is dirty. Use FORCE=1 to override."; \
		exit 1; \
	fi
	cargo bench --bench core_ops -- --save-baseline main
	cargo bench --bench cascade_scope -- --save-baseline main
	@_N=$$(find target/criterion -path '*/main/estimates.json' 2>/dev/null | wc -l); \
	if [ "$$_N" -lt 10 ]; then \
		echo "ERROR: only $$_N baseline files found (expected ≥ 10)"; \
		exit 1; \
	fi; \
	_SHA=$$(git rev-parse --short HEAD 2>/dev/null || echo unknown); \
	_DIRTY=$$(git diff --quiet 2>/dev/null && echo false || echo true); \
	_RUSTC=$$(rustc --version 2>/dev/null || echo unknown); \
	_DATE=$$(date -u +%Y-%m-%dT%H:%M:%SZ); \
	printf '{\n  "schema_version": 1,\n  "saved_at": "%s",\n  "git_sha": "%s",\n  "git_dirty": %s,\n  "rustc": "%s",\n  "bench_count": %d\n}\n' \
		"$$_DATE" "$$_SHA" "$$_DIRTY" "$$_RUSTC" "$$_N" \
		> target/criterion/.baseline_meta.json; \
	echo "Baseline saved: $$_N files, sha=$$_SHA, dirty=$$_DIRTY, rustc=$$_RUSTC"

# Cache hit-rate benchmark (docs/strategy-assessment.md §3). Synthetic
# agentic + Zipf traces against the real CacheStore. Exit 2 (FAIL-CORE) if
# agentic exact hit rate misses the gate. Results: tests/results/hitrate/<ts>/
bench-hitrate:
	@_RD="tests/results/hitrate/$$(date +%Y%m%d-%H%M%S)-$$$$"; \
	mkdir -p "$$_RD"; \
	echo "=== Hit-Rate Benchmark → $$_RD ==="; \
	_RC=0; \
	cargo run --bin hitrate_bench -- --results-dir "$$_RD" || _RC=$$?; \
	cargo run --bin test_runner -- index "$$_RD" || echo "WARN: index generation failed"; \
	echo "  Index: $$_RD/index.html"; \
	exit $$_RC

# Semantic hit-rate mode (v2): real SemanticCache tier + synthetic orthogonal
# embedder. Requires embed-api feature. Sweeps τ, gates false-hit ≤ 1%.
# Exit 3 (FAIL-TRUST) if no τ clears the false-hit gate with uplift.
# Runtime ~6 min default suite (linear scan per lookup; scales with
# sem-cache-size). Trim with --sem-max-queries / --queries for iteration.
bench-hitrate-sem:
	@_RD="tests/results/hitrate/sem-$$(date +%Y%m%d-%H%M%S)-$$$$"; \
	mkdir -p "$$_RD"; \
	echo "=== Hit-Rate Benchmark (semantic) → $$_RD ==="; \
	_RC=0; \
	cargo run --features embed-api --bin hitrate_bench -- \
	  --semantic --results-dir "$$_RD" || _RC=$$?; \
	cargo run --bin test_runner -- index "$$_RD" || echo "WARN: index generation failed"; \
	echo "  Index: $$_RD/index.html"; \
	exit $$_RC

# Live ONNX embedder semantic mode (v3): real all-MiniLM-L6-v2 embeddings
# instead of the synthetic orthogonal embedder. Requires: embed feature
# (ONNX runtime system libs) + model installed under ~/.conproxy/models/.
# ort-sys links the system lib: point ORT_LIB_LOCATION at it and prefer
# dynamic linking (override both if your setup differs).
# Embeddings are memoized across τ/cache sizes/workloads, so the ONNX cost is
# paid once per unique query. Semantic lookup is a linear scan at 384 dims —
# default caps keep the suite in the ~10 min range; raise for full runs:
#   make bench-hitrate-onnx SEM_MAX=100000
bench-hitrate-onnx:
	@_RD="tests/results/hitrate/onnx-$$(date +%Y%m%d-%H%M%S)-$$$$"; \
	mkdir -p "$$_RD"; \
	echo "=== Hit-Rate Benchmark (semantic, ONNX embedder) → $$_RD ==="; \
	_RC=0; \
	ORT_LIB_LOCATION=$${ORT_LIB_LOCATION:-/usr/local/lib} \
	ORT_PREFER_DYNAMIC_LINK=$${ORT_PREFER_DYNAMIC_LINK:-1} \
	cargo run --features embed --bin hitrate_bench -- \
	  --semantic --embedder onnx --results-dir "$$_RD" \
	  --sem-max-queries $${SEM_MAX:-20000} || _RC=$$?; \
	cargo run --bin test_runner -- index "$$_RD" || echo "WARN: index generation failed"; \
	echo "  Index: $$_RD/index.html"; \
	exit $$_RC

# Live wire mode (v4): replay against a REAL proxy + qdrant (docker). Seeds
# qdrant with MiniLM-embedded docs, replays traces through the proxy HTTP API,
# measures real hit rate + wall latency. Optional mutation stream bumps doc
# versions in qdrant (stale-content accounting); LIVE_EVICT=1 adds a
# /cache/evict call after each mutation (simulated external CDC).
# Requires: docker, embed feature (ONNX runtime libs), model installed.
# Tunables: LIVE_QUERIES (zipf events), LIVE_TASKS (agentic tasks),
# LIVE_MUTATE (per-event mutation prob), LIVE_EVICT (0|1).
LIVE_QUERIES ?= 5000
LIVE_TASKS ?= 100
LIVE_MUTATE ?= 0.001
LIVE_EVICT ?= 0
bench-hitrate-live:
	@command -v docker >/dev/null 2>&1 || { echo "ERROR: docker required for bench-hitrate-live"; exit 1; }
	@_WD=/tmp/conproxy-hitrate-live; \
	_RD="tests/results/hitrate/live-$$(date +%Y%m%d-%H%M%S)-$$$$"; \
	mkdir -p "$$_WD" "$$_RD"; \
	export ORT_LIB_LOCATION=$${ORT_LIB_LOCATION:-/usr/local/lib} \
	  ORT_PREFER_DYNAMIC_LINK=$${ORT_PREFER_DYNAMIC_LINK:-1}; \
	cargo build --profile profiling --features embed --bin conproxy --bin hitrate_bench || exit 1; \
	docker rm -f conproxy-hitrate-qdrant >/dev/null 2>&1 || true; \
	docker run -d --name conproxy-hitrate-qdrant -p 16333:6333 qdrant/qdrant >/dev/null || exit 1; \
	_QOK=0; for _i in $$(seq 1 60); do \
	  curl -sf http://localhost:16333/healthz >/dev/null 2>&1 && { _QOK=1; break; }; sleep 1; done; \
	if [ $$_QOK != 1 ]; then echo "ERROR: qdrant not ready"; docker rm -f conproxy-hitrate-qdrant >/dev/null; exit 1; fi; \
	printf '[proxy]\nlisten = "127.0.0.1:8098"\n\n[proxy.embedding]\nprovider = "onnx"\n\n[upstreams.qdrant]\nurl = "http://localhost:16333"\ntype = "qdrant"\ncollection = "conproxy_hitrate"\n\n[contexts.default]\ndefault = true\n\n[[contexts.default.upstreams]]\nref = "qdrant"\npriority = 0\n' > "$$_WD/conproxy.toml"; \
	$(CURDIR)/target/profiling/conproxy start --config "$$_WD/conproxy.toml" >"$$_WD/proxy.log" 2>&1 & \
	_PID=$$!; \
	trap 'kill -INT $$_PID 2>/dev/null; docker rm -f conproxy-hitrate-qdrant >/dev/null 2>&1 || true' EXIT; \
	_READY=0; for _i in $$(seq 1 90); do \
	  kill -0 $$_PID 2>/dev/null || break; \
	  curl -sf http://127.0.0.1:8099/health >/dev/null 2>&1 && { _READY=1; break; }; sleep 1; done; \
	if [ $$_READY != 1 ]; then echo "ERROR: proxy not ready (see $$_WD/conproxy.toml)"; exit 1; fi; \
	echo "=== Hit-Rate Benchmark (LIVE) → $$_RD ==="; \
	_RC=0; \
	_EVICT=""; [ "$(LIVE_EVICT)" = "1" ] && _EVICT="--live-evict"; \
	$(CURDIR)/target/profiling/hitrate_bench \
	  --live http://127.0.0.1:8099 --live-seed http://localhost:16333 \
	  --queries $(LIVE_QUERIES) --tasks $(LIVE_TASKS) \
	  --live-mutate $(LIVE_MUTATE) $$_EVICT \
	  --results-dir "$(CURDIR)/$$_RD" || _RC=$$?; \
	cargo run --bin test_runner -- index "$$_RD" || echo "WARN: index generation failed"; \
	echo "  Index: $$_RD/index.html"; \
	exit $$_RC

bench-compare:
	cargo bench --bench core_ops --
	cargo bench --bench cascade_scope --

# ============================================================================
# Profiling
# ============================================================================

# DHAT heap profile (standalone — start proxy, run brief workload, collect dhat-heap.json)
profile-dhat:
	@echo "=== DHAT Heap Profile ==="
	@cargo build --profile profiling --features dhat-heap
	@mkdir -p /tmp/conproxy-dhat
	@printf '[proxy]\nlisten = "127.0.0.1:9096"\n\n[[proxy.upstreams]]\nid = "test"\nurl = "http://localhost:6333"\nupstream_type = "qdrant"\n' > /tmp/conproxy-dhat/conproxy.toml
	@echo "Starting proxy with dhat instrumentation on :9096/:9097..."
	@cd /tmp/conproxy-dhat && CONPROXY_DHAT=1 $(CURDIR)/target/profiling/conproxy start --config conproxy.toml & \
	_PID=$$!; \
	trap 'kill -INT $$_PID 2>/dev/null; wait $$_PID 2>/dev/null' EXIT; \
	_READY=0; \
	for _i in $$(seq 1 40); do \
		if ! kill -0 $$_PID 2>/dev/null; then break; fi; \
		if curl -sf http://127.0.0.1:9097/health >/dev/null 2>&1; then _READY=1; break; fi; \
		sleep 0.25; \
	done; \
	if [ "$$_READY" = "1" ]; then \
		echo "Proxy ready, running brief workload..."; \
		for _q in "rust async" "error handling" "database" "concurrency" "testing"; do \
			curl -sf -X POST http://127.0.0.1:9097/query -H 'Content-Type: application/json' \
				-d "{\"query\":\"$$_q\"}" >/dev/null 2>&1 || true; \
		done; \
		echo "Stopping proxy..."; \
	fi; \
	kill -INT $$_PID 2>/dev/null; wait $$_PID 2>/dev/null; trap - EXIT; \
	if [ -f /tmp/conproxy-dhat/dhat-heap.json ]; then \
		echo ""; \
		echo "DHAT profile written to: /tmp/conproxy-dhat/dhat-heap.json"; \
		ls -lh /tmp/conproxy-dhat/dhat-heap.json | awk '{print "  Size: " $$5}'; \
		echo "  View: https://nnethercote.github.io/dh_view/dh_view.html"; \
	else \
		echo "ERROR: dhat-heap.json not generated"; \
	fi

# PGO (Profile-Guided Optimization) build.
#
# PGO is a 2-pass optimization: first build an instrumented binary, run a
# representative workload to record branch coverage, then rebuild using
# that data for better optimization. Yields 5-20% speedups on hot paths.
#
# Requires `rustup component add llvm-tools-preview` for `llvm-profdata`.
# Requires Docker to be running for the Qdrant workload (uses existing
# `cqdrant` container or starts one).
profile-pgo:
	@rustup component add llvm-tools-preview >/dev/null 2>&1 || true
	@rm -rf /tmp/conproxy-pgo-profraw /tmp/conproxy-pgo-profdata /tmp/conproxy-pgo-wd
	@mkdir -p /tmp/conproxy-pgo-profraw /tmp/conproxy-pgo-wd
	@echo "=== PGO step 1: instrument build ==="
	@RUSTFLAGS="-Cprofile-generate=/tmp/conproxy-pgo-profraw" \
		cargo build --release --features release --bin conproxy
	@echo "=== PGO step 2: drive workload ==="
	@if curl -sf http://localhost:6333/readyz >/dev/null 2>&1; then \
		mkdir -p /tmp/conproxy-pgo-wd/.conproxy/cache; \
		printf '[proxy]\nlisten = "127.0.0.1:9090"\n\n[[proxy.upstreams]]\nid = "pgo-qdrant"\nurl = "http://localhost:6333"\nupstream_type = "qdrant"\n' \
			> /tmp/conproxy-pgo-wd/.conproxy/conproxy.toml; \
		cd /tmp/conproxy-pgo-wd && $(CURDIR)/target/release/conproxy start --daemon; \
		_PID=$$(lsof -ti:9090 2>/dev/null); \
		_READY=0; \
		for _i in $$(seq 1 40); do \
			kill -0 $$_PID 2>/dev/null || break; \
			if curl -sf http://127.0.0.1:9091/health >/dev/null 2>&1; then _READY=1; break; fi; \
			sleep 0.25; \
		done; \
		if [ "$$_READY" = "1" ]; then \
			for _q in "rust async" "error handling" "caching" "vector search" "tokio"; do \
				curl -sf -X POST http://127.0.0.1:9091/query \
					-H 'Content-Type: application/json' \
					-d "{\"query\":\"$$_q\",\"top_k\":5}" >/dev/null 2>&1 || true; \
			done; \
			cd /tmp/conproxy-pgo-wd && $(CURDIR)/target/release/conproxy stop; \
		else \
			echo "WARNING: proxy not ready on :9091; using fallback (cargo bench)"; \
			cargo bench --bench core_ops -- --quick >/dev/null 2>&1 || true; \
		fi; \
	else \
		echo "WARNING: Qdrant not reachable at localhost:6333; using fallback workload (cargo bench)"; \
		cargo bench --bench core_ops -- --quick >/dev/null 2>&1 || true; \
	fi
	@echo "=== PGO step 3: merge profraw ==="
	@LLVMPROFDATA=$$(rustc --print sysroot)/lib/rustlib/$$(rustc -vV | sed -n 's/host: //p')/bin/llvm-profdata; \
	if [ ! -x "$$LLVMPROFDATA" ]; then \
		echo "ERROR: llvm-profdata not found at $$LLVMPROFDATA"; \
		echo "  Install via: rustup component add llvm-tools-preview"; \
		exit 1; \
	fi; \
	$$LLVMPROFDATA merge -o /tmp/conproxy-pgo-profdata /tmp/conproxy-pgo-profraw/*.profraw
	@echo "=== PGO step 4: rebuild with profile data ==="
	@RUSTFLAGS="-Cprofile-use=/tmp/conproxy-pgo-profdata -Copt-level=2 -Clto=fat" \
		cargo build --release --features release --bin conproxy
	@echo ""
	@echo "PGO-optimized binary at: target/release/conproxy"
	@ls -lh target/release/conproxy | awk '{print "  Size: " $$5}'

profile-pgo-clean:
	@rm -rf /tmp/conproxy-pgo-profraw /tmp/conproxy-pgo-profdata /tmp/conproxy-pgo-wd

# CPU flamegraph under load. Prefers perf+inferno (real SVG, attach mode).
# samply also supported (emits Firefox Profiler profile.json.gz — open in
# https://profiler.firefox.com, not an SVG).
# Prerequisite: cargo install inferno-flamegraph samply
# (system `perf` required; perf_event_paranoid ≤ 1)
profile-flamegraph:
	@echo "=== CPU Flamegraph ==="
	@mkdir -p /tmp/conproxy-flame
	@printf '[proxy]\nlisten = "127.0.0.1:9090"\n\n[[proxy.upstreams]]\nid = "qdrant"\nurl = "http://localhost:6333"\nupstream_type = "qdrant"\n' > /tmp/conproxy-flame/conproxy.toml
	@cargo build --profile profiling --features release --bin conproxy
	@cd /tmp/conproxy-flame && $(CURDIR)/target/profiling/conproxy start --config conproxy.toml & \
	_PID=$$!; \
	trap 'kill -INT $$_PID 2>/dev/null; rm -rf /tmp/conproxy-flame' EXIT; \
	_READY=0; \
	for _i in $$(seq 1 40); do \
		kill -0 $$_PID 2>/dev/null || break; \
		if curl -sf http://127.0.0.1:9091/health >/dev/null 2>&1; then _READY=1; break; fi; \
		sleep 0.25; \
	done; \
	if [ "$$_READY" != "1" ]; then echo "ERROR: proxy not ready on :9091"; exit 1; fi; \
	if command -v perf >/dev/null 2>&1 && command -v inferno-flamegraph >/dev/null 2>&1; then \
		echo "Using perf + inferno (real SVG)..."; \
		perf record -g -F 99 -p $$_PID -o /tmp/conproxy-flame/perf.data -- sleep 12 & \
		_SAMP_PID=$$!; \
	elif command -v samply >/dev/null 2>&1; then \
		echo "Using samply (Firefox Profiler format — open in https://profiler.firefox.com)..."; \
		samply record -p $$_PID -o $(CURDIR)/profile.json.gz --save-only --duration 12 & \
		_SAMP_PID=$$!; \
	else \
		echo "ERROR: need (perf + inferno-flamegraph) OR samply."; \
		echo "Install with: cargo install inferno-flamegraph samply"; \
		exit 1; \
	fi; \
	for _n in $$(seq 1 30); do \
		for _q in "performance profiling sample" "rust async patterns" "vector search optimization"; do \
			curl -sf -X POST http://127.0.0.1:9091/query -H 'Content-Type: application/json' \
				-d "{\"query\":\"$$_q\",\"top_k\":5}" >/dev/null 2>&1 || true; \
		done; \
	done; \
	wait $$_SAMP_PID 2>/dev/null; \
	if [ -f /tmp/conproxy-flame/perf.data ]; then \
		perf script -i /tmp/conproxy-flame/perf.data | inferno-collapse-perf | \
			inferno-flamegraph > $(CURDIR)/flamegraph.svg; \
	fi; \
	if [ ! -s $(CURDIR)/flamegraph.svg ] && [ ! -s $(CURDIR)/profile.json.gz ]; then \
		echo "ERROR: empty flamegraph output"; exit 1; \
	fi; \
	if [ -s $(CURDIR)/flamegraph.svg ]; then \
		echo "Flamegraph output: $(CURDIR)/flamegraph.svg"; \
	else \
		echo "Profile output:  $(CURDIR)/profile.json.gz (open in profiler.firefox.com)"; \
	fi; \
	kill -INT $$_PID 2>/dev/null; wait $$_PID 2>/dev/null; \
	trap - EXIT; rm -rf /tmp/conproxy-flame

# ============================================================================
# Tokio Console + Metrics
# ============================================================================

# Tokio-console async runtime diagnosis (dev only).
# Requires: RUSTFLAGS="--cfg tokio_unstable" + tokio-console feature.
# Default bind: 127.0.0.1:6669 (overridable via TOKIO_CONSOLE_BIND env).
# Inspect with the `tokio-console` CLI client (not a browser):
#   cargo install tokio-console
#   tokio-console               # connects to default 127.0.0.1:6669
profile-tokio-console:
	@echo "=== Tokio Console (async runtime diagnosis) ==="
	@mkdir -p /tmp/conproxy-console
	@printf '[proxy]\nlisten = "127.0.0.1:8080"\n\n[[proxy.upstreams]]\nid = "qdrant"\nurl = "http://localhost:6333"\nupstream_type = "qdrant"\n' > /tmp/conproxy-console/conproxy.toml
	RUSTFLAGS="--cfg tokio_unstable" cargo build --profile profiling --features tokio-console --bin conproxy
	@echo "Starting proxy with tokio-console (default bind 127.0.0.1:6669) ..."
	@echo "In another terminal, run: tokio-console"
	cd /tmp/conproxy-console && $(CURDIR)/target/profiling/conproxy start --config conproxy.toml

# Prometheus metrics snapshot under load.
# Heap timeline profile (optional tool; deeper than DHAT for finding peak RSS leaks).
# Needs the `heaptrack` binary (https://github.com/KDE/heaptrack). Mirrors
# flamegraph's attach-under-load model: start proxy, attach heaptrack to the
# running PID, send curl load for ~12s, then summarize. The .gz file is
# portable; view with `heaptrack_print` or heaptrack_gui.
profile-heaptrack:
	@echo "=== Heaptrack heap timeline ==="
	@command -v heaptrack >/dev/null 2>&1 || { \
		echo "ERROR: 'heaptrack' not found."; \
		echo "Install: https://github.com/KDE/heaptrack (Debian/Ubuntu: apt install heaptrack)"; \
		exit 1; \
	}
	@mkdir -p /tmp/conproxy-heap
	@printf '[proxy]\nlisten = "127.0.0.1:9094"\n\n[[proxy.upstreams]]\nid = "qdrant"\nurl = "http://localhost:6333"\nupstream_type = "qdrant"\n' > /tmp/conproxy-heap/conproxy.toml
	cargo build --profile profiling --bin conproxy
	@cd /tmp/conproxy-heap && $(CURDIR)/target/profiling/conproxy start --config conproxy.toml & \
	_PID=$$!; \
	trap 'kill -INT $$_PID 2>/dev/null; wait $$_PID 2>/dev/null' EXIT; \
	_READY=0; \
	for _i in $$(seq 1 40); do \
		if ! kill -0 $$_PID 2>/dev/null; then break; fi; \
		if curl -sf http://127.0.0.1:9095/health >/dev/null 2>&1; then _READY=1; break; fi; \
		sleep 0.25; \
	done; \
	if [ "$$_READY" != "1" ]; then echo "ERROR: proxy not ready on :9095"; exit 1; fi; \
	echo "Attaching heaptrack to PID $$_PID (12s)..."; \
	heaptrack -o /tmp/conproxy-heap/conproxy.heap -p $$_PID &
	_HT_PID=$$!; \
	sleep 1; \
	for _n in $$(seq 1 30); do \
		for _q in "performance profiling sample" "rust async patterns" "vector search optimization"; do \
			curl -sf -X POST http://127.0.0.1:9095/query -H 'Content-Type: application/json' \
				-d "{\"query\":\"$$_q\",\"top_k\":5}" >/dev/null 2>&1 || true; \
		done; \
	done; \
	wait $$_HT_PID 2>/dev/null; \
	kill -INT $$_PID 2>/dev/null; wait $$_PID 2>/dev/null; \
	trap - EXIT; \
	if ls /tmp/conproxy-heap/conproxy.heap.*.gz >/dev/null 2>&1; then \
		cp /tmp/conproxy-heap/conproxy.heap.*.gz $(CURDIR)/heaptrack-$$(date +%Y%m%d-%H%M%S).gz; \
		ls -lh $(CURDIR)/heaptrack-*.gz | tail -1 | awk '{print "Heaptrack output: " $$9 " (" $$5 ")"}'; \
		echo "View: heaptrack_print <file> | heaptrack_gui <file>"; \
	else \
		echo "ERROR: heaptrack output not generated"; \
		exit 1; \
	fi; \
	rm -rf /tmp/conproxy-heap

# Prometheus metrics snapshot under load.
# Starts proxy on :9092/:9093, scrapes /metrics/prometheus BEFORE and AFTER a
# curl workload, writes both to /tmp/conproxy-metrics/. perf_summarize can
# compute counter deltas when both files are passed via --metrics-file
# (after) + --metrics-before-file (before).
profile-metrics-snap:
	@echo "=== Prometheus Metrics Snapshot ==="
	@mkdir -p /tmp/conproxy-metrics
	@printf '[proxy]\nlisten = "127.0.0.1:9092"\n\n[[proxy.upstreams]]\nid = "qdrant"\nurl = "http://localhost:6333"\nupstream_type = "qdrant"\n' > /tmp/conproxy-metrics/conproxy.toml
	@: > /tmp/conproxy-metrics/metrics-prometheus.txt  # ensure file exists; perf-tuning copies it
	@: > /tmp/conproxy-metrics/metrics-before.txt
	cargo build --profile profiling --bin conproxy
	@cd /tmp/conproxy-metrics && $(CURDIR)/target/profiling/conproxy start --config conproxy.toml & \
	_PID=$$!; \
	trap 'kill -INT $$_PID 2>/dev/null' EXIT; \
	_READY=0; \
	for _i in $$(seq 1 40); do \
		kill -0 $$_PID 2>/dev/null || break; \
		if curl -sf http://127.0.0.1:9093/health >/dev/null 2>&1; then _READY=1; break; fi; \
		sleep 0.25; \
	done; \
	if [ "$$_READY" != "1" ]; then echo "ERROR: proxy not ready on :9093"; exit 1; fi; \
	echo "Scraping BEFORE workload..."; \
	curl -sf http://127.0.0.1:9093/metrics/prometheus -o /tmp/conproxy-metrics/metrics-before.txt \
		|| echo "(no /metrics/prometheus endpoint)"; \
	echo "Sending load..."; \
	for _n in $$(seq 1 30); do \
		for _q in "performance profiling sample" "rust async patterns" "vector search optimization"; do \
			curl -sf -X POST http://127.0.0.1:9093/query -H 'Content-Type: application/json' \
				-d "{\"query\":\"$$_q\",\"top_k\":5}" >/dev/null 2>&1 || true; \
		done; \
	done; \
	echo ""; \
	echo "--- /metrics/prometheus (AFTER) ---"; \
	curl -sf http://127.0.0.1:9093/metrics/prometheus -o /tmp/conproxy-metrics/metrics-prometheus.txt && \
		head -80 /tmp/conproxy-metrics/metrics-prometheus.txt || echo "(no /metrics/prometheus endpoint)"; \
	echo ""; \
	echo "--- /metrics (JSON summary) ---"; \
	curl -sf http://127.0.0.1:9093/metrics 2>/dev/null | head -50 || echo "(no /metrics endpoint)"; \
	echo ""; \
	echo "--- /debug/vars ---"; \
	curl -sf http://127.0.0.1:9093/debug/vars 2>/dev/null | head -50 || echo "(no /debug/vars)"; \
	kill -INT $$_PID 2>/dev/null; wait $$_PID 2>/dev/null; \
	trap - EXIT

# ============================================================================
# Perf Tuning (structured measure → summarize → plan)
# ============================================================================

# Quick: run both bench targets with (CI-aware verdict),
# summarize, write to results dir. PIPESTATUS propagates real cargo exit.
# Touches .bench_start so perf_summarize --since filters stale bench data.
# After summarize, report_criterion.json is mirrored to bench/ so test_runner
# index can pick it up via build_bench_digest (looks for report_<name>.json).
perf-tuning-quick:
	@_RD="tests/results/perf-tuning/$$(date +%Y%m%d-%H%M%S)-$$$$"; \
	mkdir -p "$$_RD"; \
	touch "$$_RD/.bench_start"; \
	echo "=== Perf Tuning (quick) → $$_RD ==="; \
	echo "--- core_ops (vs main baseline) ---"; \
	cargo bench --bench core_ops -- --quick 2>&1 | tee "$$_RD/criterion_core_ops.txt"; \
	echo "--- cascade_scope (vs main baseline) ---"; \
	cargo bench --bench cascade_scope -- --quick 2>&1 | tee "$$_RD/criterion_cascade_scope.txt"; \
	echo "--- summarize ---"; \
	cargo run --bin perf_summarize -- --results-dir "$$_RD" --threshold 15 --mode quick \
		--since "$$_RD/.bench_start" --fail-on-regression; \
	mkdir -p "$$_RD/bench"; \
	cp "$$_RD/report_criterion.json" "$$_RD/bench/report_criterion.json"; \
	echo ""; \
	echo "=== Results: $$_RD ==="; \
	cat "$$_RD/SUMMARY.md"

# Default: quick + metrics-snap (scrapes /metrics/prometheus → metrics-prometheus.txt)
perf-tuning-default:
	@_RD="tests/results/perf-tuning/$$(date +%Y%m%d-%H%M%S)-$$$$"; \
	mkdir -p "$$_RD"; \
	touch "$$_RD/.bench_start"; \
	echo "=== Perf Tuning (default) → $$_RD ==="; \
	echo "--- core_ops (vs main baseline) ---"; \
	cargo bench --bench core_ops -- --quick 2>&1 | tee "$$_RD/criterion_core_ops.txt"; \
	echo "--- cascade_scope (vs main baseline) ---"; \
	cargo bench --bench cascade_scope -- --quick 2>&1 | tee "$$_RD/criterion_cascade_scope.txt"; \
	echo "--- metrics-snap (prom scrape) ---"; \
	$(MAKE) profile-metrics-snap 2>&1 | tee "$$_RD/metrics-snap.txt" || true; \
	cp /tmp/conproxy-metrics/metrics-prometheus.txt "$$_RD/" 2>/dev/null || true; \
	cp /tmp/conproxy-metrics/metrics-before.txt "$$_RD/" 2>/dev/null || true; \
	echo "--- summarize ---"; \
	cargo run --bin perf_summarize -- --results-dir "$$_RD" --threshold 15 --mode default \
		--since "$$_RD/.bench_start" --fail-on-regression \
		--metrics-file "$$_RD/metrics-prometheus.txt" \
		--metrics-before-file "$$_RD/metrics-before.txt"; \
	mkdir -p "$$_RD/bench"; \
	cp "$$_RD/report_criterion.json" "$$_RD/bench/report_criterion.json"; \
	echo ""; \
	echo "=== Results: $$_RD ==="; \
	cat "$$_RD/SUMMARY.md"

# Full: default + flamegraph (+ DHAT if DHAT=1). PGO is intentionally NOT included (separate workflow).
# FLAMEGRAPH=0 to skip flamegraph; DHAT=1 to additionally capture heap profile.
perf-tuning-full:
	@_RD="tests/results/perf-tuning/$$(date +%Y%m%d-%H%M%S)-$$$$"; \
	mkdir -p "$$_RD"; \
	touch "$$_RD/.bench_start"; \
	echo "=== Perf Tuning (full) → $$_RD ==="; \
	echo "--- core_ops (full) ---"; \
	cargo bench --bench core_ops -- 2>&1 | tee "$$_RD/criterion_core_ops.txt"; \
	echo "--- cascade_scope (full) ---"; \
	cargo bench --bench cascade_scope -- 2>&1 | tee "$$_RD/criterion_cascade_scope.txt"; \
	echo "--- metrics-snap (prom scrape) ---"; \
	$(MAKE) profile-metrics-snap 2>&1 | tee "$$_RD/metrics-snap.txt" || true; \
	cp /tmp/conproxy-metrics/metrics-prometheus.txt "$$_RD/" 2>/dev/null || true; \
	cp /tmp/conproxy-metrics/metrics-before.txt "$$_RD/" 2>/dev/null || true; \
	if [ "$${FLAMEGRAPH:-1}" != "0" ]; then \
		echo "--- flamegraph ---"; \
		$(MAKE) profile-flamegraph 2>&1 | tee "$$_RD/flamegraph.txt" || true; \
	else \
		echo "--- flamegraph: SKIPPED (FLAMEGRAPH=0) ---"; \
	fi; \
	if [ "$${DHAT:-0}" = "1" ]; then \
		echo "--- dhat (DHAT=1) ---"; \
		$(MAKE) profile-dhat 2>&1 | tee "$$_RD/dhat.txt" || true; \
		cp /tmp/conproxy-dhat/dhat-heap.json "$$_RD/" 2>/dev/null || true; \
	else \
		echo "--- dhat: SKIPPED (set DHAT=1 to enable; needs Qdrant) ---"; \
	fi; \
	if [ "$${HITRATE:-1}" != "0" ]; then \
		echo "--- hitrate (exact, default workloads) ---"; \
		mkdir -p "$$_RD/hitrate"; \
		cargo run --bin hitrate_bench -- --results-dir "$$_RD/hitrate" --no-fail 2>&1 | tee "$$_RD/hitrate.txt" || true; \
	else \
		echo "--- hitrate: SKIPPED (HITRATE=0) ---"; \
	fi; \
	if [ "$${HEAPTRACK:-1}" != "0" ]; then \
		echo "--- heaptrack (graceful if tool missing) ---"; \
		$(MAKE) profile-heaptrack 2>&1 | tee "$$_RD/heaptrack.txt" || echo "(heaptrack unavailable; skipped)" > "$$_RD/heaptrack.txt"; \
	else \
		echo "--- heaptrack: SKIPPED (HEAPTRACK=0) ---"; \
	fi; \
	if [ "$${TOKIO_CONSOLE:-0}" = "1" ]; then \
		echo "--- tokio-console (instrumented proxy + headless snap) ---"; \
		_WD=/tmp/conproxy-console-full; mkdir -p "$$_WD"; \
		printf '[proxy]\nlisten = "127.0.0.1:8084"\n[contexts.console]\ndefault = true\n' > "$$_WD/conproxy.toml"; \
		RUSTFLAGS="--cfg tokio_unstable" cargo build --profile profiling --features tokio-console,tokio-taskdump --bin conproxy 2>&1 | tee "$$_RD/console-build.txt"; \
		cargo build --profile profiling --features tokio-console-snap --bin console_snap 2>&1 | tee -a "$$_RD/console-build.txt" || true; \
		$(CURDIR)/target/profiling/conproxy start --config "$$_WD/conproxy.toml" >"$$_WD/proxy.log" 2>&1 & \
		_PID=$$!; \
		trap 'kill -INT $$_PID 2>/dev/null' EXIT; \
		_READY=0; \
		for _i in $$(seq 1 40); do \
		  kill -0 $$_PID 2>/dev/null || break; \
		  if curl -sf http://127.0.0.1:8085/health >/dev/null 2>&1; then _READY=1; break; fi; \
		  sleep 0.25; \
		done; \
		if [ "$$_READY" != "1" ]; then \
		  echo "(tokio-console: proxy not ready on :8085; skipping)"; \
		  kill -INT $$_PID 2>/dev/null; wait $$_PID 2>/dev/null; trap - EXIT; \
		else \
		  echo "Sending load (12s) ..."; \
		  for _n in $$(seq 1 20); do \
		    for _q in "performance profiling sample" "rust async patterns" "vector search optimization"; do \
		      curl -sf -X POST http://127.0.0.1:8085/query -H 'Content-Type: application/json' \
		        -d "{\"query\":\"$$_q\",\"top_k\":5}" >/dev/null 2>&1 || true; \
		    done; \
		  done >/dev/null 2>&1 & \
		  _LOAD_PID=$$!; \
		  echo "Sampling console (5s) ..."; \
		  $(CURDIR)/target/profiling/console_snap http://127.0.0.1:6669 5 --out "$$_RD" --top 10 \
		    2>&1 | tee "$$_RD/console-snap.txt" || true; \
		  wait $$_LOAD_PID 2>/dev/null || true; \
		  if [ ! -f "$$_RD/console-snap.json" ]; then touch "$$_RD/console-snap.json"; fi; \
		  echo "Fetching /debug/tokio (RuntimeMetrics) ..."; \
		  curl -sf http://127.0.0.1:8085/debug/tokio -o "$$_RD/tokio-metrics.json" 2>/dev/null || \
		    echo "(no /debug/tokio endpoint)" > "$$_RD/tokio-metrics.json"; \
		  echo "Fetching /debug/tokio/dump (Handle::dump) ..."; \
		  curl -sf http://127.0.0.1:8085/debug/tokio/dump -o "$$_RD/tokio-dump.txt" 2>/dev/null || \
		    echo "(no /debug/tokio/dump endpoint — needs RUSTFLAGS=--cfg tokio_unstable + tokio-console feature)" > "$$_RD/tokio-dump.txt"; \
		  kill -INT $$_PID 2>/dev/null; wait $$_PID 2>/dev/null; trap - EXIT; \
		fi; \
		rm -rf "$$_WD"; \
	else \
		echo "--- tokio-console: SKIPPED (set TOKIO_CONSOLE=1 to enable; needs RUSTFLAGS=--cfg tokio_unstable + console-snap built) ---"; \
	fi; \
	echo "--- summarize ---"; \
	_PS_ARGS="--results-dir $$_RD --threshold 15 --mode full \
		--since $$_RD/.bench_start --fail-on-regression \
		--metrics-file $$_RD/metrics-prometheus.txt \
		--metrics-before-file $$_RD/metrics-before.txt \
		--flamegraph flamegraph.svg \
		--history-dir perf-history"; \
	if [ -f "$$_RD/hitrate/summary.json" ]; then _PS_ARGS="$$_PS_ARGS --hitrate-summary $$_RD/hitrate/summary.json"; fi; \
	if [ -f "$$_RD/console-snap.json" ] && [ -s "$$_RD/console-snap.json" ]; then _PS_ARGS="$$_PS_ARGS --tokio-snap $$_RD/console-snap.json"; fi; \
	if [ -f "$$_RD/tokio-metrics.json" ] && [ -s "$$_RD/tokio-metrics.json" ]; then _PS_ARGS="$$_PS_ARGS --tokio-metrics $$_RD/tokio-metrics.json"; fi; \
	if [ -f "$$_RD/tokio-dump.txt" ] && [ -s "$$_RD/tokio-dump.txt" ]; then _PS_ARGS="$$_PS_ARGS --tokio-dump $$_RD/tokio-dump.txt"; fi; \
	cargo run --bin perf_summarize -- $$_PS_ARGS; \
	mkdir -p "$$_RD/bench"; \
	cp "$$_RD/report_criterion.json" "$$_RD/bench/report_criterion.json"; \
	echo ""; \
	echo "=== Results: $$_RD ==="; \
	cat "$$_RD/SUMMARY.md"

# Remove all perf-tuning run dirs (gitignored).
perf-tuning-clean:
	rm -rf tests/results/perf-tuning/

# perf-publish: copy artifacts from latest run → perf-history/<ts>-<sha8>/
# Prune to last 100. NEVER auto-commits — prints the git command for you.
# Use `RUN=<dir>` to publish a specific run instead of the latest.
perf-publish:
	@_SRC="$${RUN:-$$(ls -1t tests/results/perf-tuning/ 2>/dev/null | head -1)}"; \
	if [ -z "$$_SRC" ] || [ ! -d "tests/results/perf-tuning/$$_SRC" ]; then \
		echo "No perf run to publish (RUN= not set and tests/results/perf-tuning/ empty)."; \
		echo "  Run: make perf-tuning-full"; exit 1; \
	fi; \
	_SHA=$$(cd tests/results/perf-tuning/$$_SRC && git rev-parse --short HEAD 2>/dev/null || echo "unknown"); \
	_DST="perf-history/$$_SRC-$$_SHA"; \
	echo "Publishing: tests/results/perf-tuning/$$_SRC → $$_DST/"; \
	mkdir -p "$$_DST"; \
	for f in summary.json SUMMARY.md ANALYSIS.md report_criterion.json env.json baseline_meta.json; do \
		if [ -f "tests/results/perf-tuning/$$_SRC/$$f" ]; then \
			cp "tests/results/perf-tuning/$$_SRC/$$f" "$$_DST/"; \
		fi; \
	done; \
	if [ -f "tests/results/perf-tuning/$$_SRC/hitrate/summary.json" ]; then \
		mkdir -p "$$_DST/hitrate"; \
		cp "tests/results/perf-tuning/$$_SRC/hitrate/summary.json" "$$_DST/hitrate/"; \
	fi; \
	for f in console-snap.json console-snap.txt tokio-metrics.json tokio-dump.txt; do \
		if [ -f "tests/results/perf-tuning/$$_SRC/$$f" ]; then \
			cp "tests/results/perf-tuning/$$_SRC/$$f" "$$_DST/"; \
		fi; \
	done; \
	echo ""; \
	echo "Pruning perf-history/ to most recent 100 runs…"; \
	_KEEP=100; _REMOVED=0; \
	for d in $$(ls -1t perf-history/ 2>/dev/null | tail -n +$$((_KEEP + 1))); do \
		if [ -d "perf-history/$$d" ]; then rm -rf "perf-history/$$d"; _REMOVED=$$((_REMOVED + 1)); fi; \
	done; \
	echo "  pruned $$_REMOVED runs (kept last $$_KEEP)"; \
	echo ""; \
	echo "To commit (never auto-commits per repo rule):"; \
	echo "  git add perf-history/"; \
	echo "  git commit -m 'perf(history): publish $$_SRC-$$_SHA'"

# ============================================================================
# Full Suite (all tests + benchmarks)
# ============================================================================

# Pre-build all binaries and test targets needed by the full suite.
test-all-prebuild:
	@echo ""
	@echo "=== Pre-building all conproxy targets ==="
	@echo "--- [1/2] Release binary (--release --features release) ---"
	cargo build --release --features release
	@echo "--- [2/2] Test binary + all dev targets (--features test) ---"
	cargo clippy --features test -- -D warnings
	cargo test --no-run --features test -q
	cargo build --bin test_runner --features test -q
	cargo test --no-run --test e2e_proxy --test e2e_eval --test e2e_uat --test e2e_load --features test,e2e -q
	cargo fmt
	@echo "All conproxy targets pre-built."

# Run everything: lint, unit tests, coverage, benchmarks, E2E proxy tests, HTTP load tests, eval
test-all: test-all-prebuild
	$(eval _RD := tests/results/$(shell date +%Y%m%d-%H%M%S)/$(_PROFILE_NAME))
	@mkdir -p $(_RD)
	@echo "Results directory: $(_RD)"
	@_FAIL=0; \
	$(MAKE) test-all-quality RESULTS_DIR=$(_RD) TEST_ALL_FEATURES=test || _FAIL=1; \
	$(MAKE) test-all-perf RESULTS_DIR=$(_RD) TEST_ALL_FEATURES=test || _FAIL=1; \
	cargo run --bin test_runner --features test -- index $(_RD); \
	echo ""; \
	echo "=========================================="; \
	echo "Full test suite complete!"; \
	echo "Results: $(_RD)"; \
	echo "  Open $(_RD)/index.html for a summary"; \
	echo "=========================================="; \
	exit $$_FAIL

# Lint + format check (fast quality gate)
test-all-lint:
	@mkdir -p $(RESULTS_DIR)/lint
	@date +%s > $(RESULTS_DIR)/lint/.start_time
	@echo ""
	@echo "=== Lint & Format Check ==="
	@{ \
		echo "--- cargo fmt --check ---"; \
		cargo fmt -- --check 2>&1; \
		FMT_RC=$$?; \
		echo ""; \
		echo "--- cargo clippy (test features) ---"; \
		cargo clippy --features $(or $(TEST_ALL_FEATURES),default) -- -D warnings 2>&1; \
		CLIPPY_RC=$$?; \
		echo ""; \
		if [ $$FMT_RC -eq 0 ]; then echo "fmt: PASS"; else echo "fmt: FAIL"; fi; \
		if [ $$CLIPPY_RC -eq 0 ]; then echo "clippy: PASS"; else echo "clippy: FAIL"; fi; \
	} | tee $(RESULTS_DIR)/lint/output.txt; \
	date +%s > $(RESULTS_DIR)/lint/.end_time; \
	! grep -q ": FAIL" $(RESULTS_DIR)/lint/output.txt

# Unit + integration tests
test-all-unit:
	@mkdir -p $(RESULTS_DIR)/unit
	@date +%s > $(RESULTS_DIR)/unit/.start_time
	@echo ""
	@echo "=== Unit + Integration Tests ==="
	set -o pipefail; cargo test --features $(or $(TEST_ALL_FEATURES),default) \
		--lib --bins --tests \
		2>&1 | tee $(RESULTS_DIR)/unit/output.txt
	@date +%s > $(RESULTS_DIR)/unit/.end_time

# Code coverage (tarpaulin + per-file threshold check)
test-all-coverage:
	@mkdir -p $(RESULTS_DIR)/coverage
	@date +%s > $(RESULTS_DIR)/coverage/.start_time
	@echo ""
	@echo "=== Code Coverage (tarpaulin) ==="
	cargo tarpaulin --features $(or $(TEST_ALL_FEATURES),default) --skip-clean \
		--lib --bins --tests \
		--out Html --out Json --output-dir $(RESULTS_DIR)/coverage 2>&1 \
		| tee $(RESULTS_DIR)/coverage/output.txt
	@echo ""
	@echo "Coverage report at $(RESULTS_DIR)/coverage/tarpaulin-report.json"
	@date +%s > $(RESULTS_DIR)/coverage/.end_time

# Security checks (cargo-audit + cargo-deny + lint-security)
test-all-security:
	@mkdir -p $(RESULTS_DIR)/security
	@date +%s > $(RESULTS_DIR)/security/.start_time
	@echo ""
	@echo "=== Security Checks ==="
	@{ \
		echo "--- cargo audit ---"; \
		cargo audit 2>&1; \
		AUDIT_RC=$$?; \
		echo ""; \
		echo "--- cargo deny check ---"; \
		cargo deny check 2>&1; \
		DENY_RC=$$?; \
		echo ""; \
		echo "--- clippy security lints ---"; \
		cargo clippy --features $(or $(TEST_ALL_FEATURES),default) -- -D warnings \
		  -W clippy::unwrap_used \
		  -W clippy::expect_used \
		  -W clippy::panic \
		  -W clippy::indexing_slicing \
		  -W clippy::arithmetic_side_effects 2>&1; \
		CLIPPY_RC=$$?; \
		echo ""; \
		if [ $$AUDIT_RC -eq 0 ]; then echo "audit: PASS"; else echo "audit: FAIL"; fi; \
		if [ $$DENY_RC -eq 0 ]; then echo "deny: PASS"; else echo "deny: FAIL"; fi; \
		if [ $$CLIPPY_RC -eq 0 ]; then echo "lint-security: PASS"; else echo "lint-security: FAIL"; fi; \
	} | tee $(RESULTS_DIR)/security/output.txt; \
	date +%s > $(RESULTS_DIR)/security/.end_time; \
	! grep -q ": FAIL" $(RESULTS_DIR)/security/output.txt

# Quality gate: lint + unit + coverage + security
test-all-quality:
	@_FAIL=0; \
	$(MAKE) test-all-lint RESULTS_DIR=$(RESULTS_DIR) TEST_ALL_FEATURES=$(or $(TEST_ALL_FEATURES),test) || _FAIL=1; \
	$(MAKE) test-all-unit RESULTS_DIR=$(RESULTS_DIR) TEST_ALL_FEATURES=$(or $(TEST_ALL_FEATURES),test) || _FAIL=1; \
	$(MAKE) test-all-coverage RESULTS_DIR=$(RESULTS_DIR) TEST_ALL_FEATURES=$(or $(TEST_ALL_FEATURES),test) || _FAIL=1; \
	$(MAKE) test-all-security RESULTS_DIR=$(RESULTS_DIR) TEST_ALL_FEATURES=$(or $(TEST_ALL_FEATURES),test) || _FAIL=1; \
	exit $$_FAIL

# Performance/E2E suite: bench + e2e + load + eval
test-all-perf:
	@_FAIL=0; _E2E_OK=1; \
	$(MAKE) test-all-bench RESULTS_DIR=$(RESULTS_DIR) TEST_ALL_FEATURES=$(or $(TEST_ALL_FEATURES),test) || _FAIL=1; \
	$(MAKE) test-all-e2e RESULTS_DIR=$(RESULTS_DIR) TEST_ALL_FEATURES=$(or $(TEST_ALL_FEATURES),test) || { _FAIL=1; _E2E_OK=0; }; \
	if [ $$_E2E_OK -eq 1 ]; then \
		echo ""; \
		echo "=== E2E Eval Tests (e2e proxy tests passed) ==="; \
		echo "Waiting for port 8080 to be released..."; \
		for _i in $$(seq 1 10); do ss -tlnp 2>/dev/null | grep -q ':8080 ' || break; sleep 1; done; \
		$(MAKE) test-all-eval RESULTS_DIR=$(RESULTS_DIR) TEST_ALL_FEATURES=$(or $(TEST_ALL_FEATURES),test) || _FAIL=1; \
	else \
		echo ""; \
		echo "SKIP: eval tests — e2e proxy tests failed"; \
		mkdir -p $(RESULTS_DIR)/eval; \
		echo "SKIP: eval tests — e2e proxy tests failed" > $(RESULTS_DIR)/eval/output.txt; \
	fi; \
	$(MAKE) e2e-services-down 2>/dev/null; true; \
	exit $$_FAIL

# Criterion benchmarks — both targets. PIPESTATUS propagates cargo's real exit code
# through the pipe to make (without it, a build failure is masked by `tee`).
test-all-bench:
	@mkdir -p $(RESULTS_DIR)/bench
	@date +%s > $(RESULTS_DIR)/bench/.start_time
	@echo ""
	@echo "=== Criterion Benchmarks (core_ops) ==="
	cargo bench --bench core_ops 2>&1 | tee $(RESULTS_DIR)/bench/bench_core_ops.txt; 
	@echo "=== Criterion Benchmarks (cascade_scope) ==="
	cargo bench --bench cascade_scope 2>&1 | tee $(RESULTS_DIR)/bench/bench_cascade_scope.txt; 
	@date +%s > $(RESULTS_DIR)/bench/.end_time

# E2E tests (Docker services started here, kept alive for eval)
test-all-e2e:
	@mkdir -p $(RESULTS_DIR)/e2e $(RESULTS_DIR)/load
	@date +%s > $(RESULTS_DIR)/e2e/.start_time
	@echo ""
	@echo "=== E2E Tests (starting Docker services) ==="
	$(MAKE) e2e-services-up
	$(MAKE) e2e-wait TEST_ALL_FEATURES=$(TEST_ALL_FEATURES)
	@echo "Verifying backends are queryable..."
	@curl -sf http://localhost:9200/_cluster/health | grep -q '"status"' \
		|| { echo "ERROR: Elasticsearch at :9200 not responding"; exit 1; }
	@curl -sf http://localhost:6333/readyz >/dev/null \
		|| { echo "ERROR: Qdrant at :6333 not responding"; exit 1; }
	@echo "  Backends verified."
	$(MAKE) e2e-load-data TEST_ALL_FEATURES=$(TEST_ALL_FEATURES)
	@echo ""
	@echo "--- E2E Proxy Tests ---"
	@E2E_PROFILE=1 CONPROXY_DHAT=1 RUST_LOG=info E2E_SUITE=all E2E_OUTPUT_DIR=$(RESULTS_DIR)/e2e PROXY_BIN="$(TEST_BIN)" \
		cargo test --test e2e_proxy --features $(TEST_ALL_FEATURES),e2e -- --ignored --nocapture --test-threads=1 e2e_proxy_suite 2>&1 | tee $(RESULTS_DIR)/e2e/output.txt; \
	_E2E_RC=$$?; \
	exit $$_E2E_RC
	@date +%s > $(RESULTS_DIR)/e2e/.end_time
	@date +%s > $(RESULTS_DIR)/load/.start_time
	@echo ""
	@echo "--- Load Tests (rlt, gRPC + HTTP) ---"
	@mkdir -p .conproxy; \
	cp $(E2E_PROXY_DIR)/configs/single-elasticsearch.toml .conproxy/conproxy.toml; \
	_READY_FILE="$(CURDIR)/$(RESULTS_DIR)/load/.proc_monitor_ready"; \
	rm -f "$$_READY_FILE"; \
	CONPROXY_DHAT=1 RUST_LOG=info $(TEST_BIN) start --listen 127.0.0.1:8080 2>$(CURDIR)/$(RESULTS_DIR)/load/proxy_logs.txt & \
	_PID=$$!; \
	kill -STOP $$_PID 2>/dev/null; \
	$(TEST_RUNNER_BIN) proc-monitor \
		--pid $$_PID --output-dir $(CURDIR)/$(RESULTS_DIR)/load --perf --bpftrace --dhat --dhat-search-dir $(CURDIR) \
		$(if $(RESOURCE_PROFILE),--resource-profile $(RESOURCE_PROFILE)) \
		--ready-file "$$_READY_FILE" & \
	_MON_PID=$$!; \
	for _i in $$(seq 1 60); do \
		if [ -f "$$_READY_FILE" ]; then break; fi; \
		if ! kill -0 $$_MON_PID 2>/dev/null; then \
			echo "WARNING: proc-monitor exited before writing ready-file"; \
			break; \
		fi; \
		sleep 0.1; \
	done; \
	kill -CONT $$_PID 2>/dev/null; \
	_READY=0; \
	for _i in $$(seq 1 40); do \
		if ! kill -0 $$_PID 2>/dev/null; then \
			echo "ERROR: proxy exited before becoming healthy"; \
			break; \
		fi; \
		if curl -sf http://127.0.0.1:8081/health >/dev/null 2>&1; then \
			_READY=1; break; \
		fi; \
		sleep 0.25; \
	done; \
	if [ "$$_READY" = "1" ]; then \
		PROXY_URL=http://localhost:8081 GRPC_URL=http://localhost:8080 BENCH_OUTPUT_DIR=$(CURDIR)/$(RESULTS_DIR)/load DURATION=10 \
			cargo test --test e2e_load --features $(TEST_ALL_FEATURES),e2e 2>&1 \
			| tee $(CURDIR)/$(RESULTS_DIR)/load/output.txt; \
	else \
		echo "ERROR: proxy at 127.0.0.1:8081 did not become healthy within 10s" \
			| tee $(CURDIR)/$(RESULTS_DIR)/load/output.txt; \
	fi; \
	kill -INT $$_PID 2>/dev/null; wait $$_PID 2>/dev/null; \
	wait $$_MON_PID 2>/dev/null; \
	rm -f .conproxy/conproxy.toml "$$_READY_FILE"
	@date +%s > $(RESULTS_DIR)/load/.end_time

# E2E eval tests (Docker services must already be running)
test-all-eval:
	@mkdir -p $(RESULTS_DIR)/eval
	@date +%s > $(RESULTS_DIR)/eval/.start_time
	@echo ""
	@if ! curl -sf http://localhost:11434/api/tags >/dev/null 2>&1 && [ -z "$$EVAL_PROVIDER" ]; then \
		echo "SKIP: Ollama not running — skipping eval tests (set EVAL_PROVIDER=claude to use Claude)"; \
	else \
		E2E_PROFILE=1 CONPROXY_DHAT=1 EVAL_OUTPUT_DIR="$(RESULTS_DIR)/eval" PROXY_BIN="$(TEST_BIN)" \
			cargo test --test e2e_eval --features $(TEST_ALL_FEATURES),e2e -- --ignored --nocapture 2>&1 | tee $(RESULTS_DIR)/eval/output.txt; \
		_EVAL_RC=$$?; \
		exit $$_EVAL_RC; \
	fi
	@date +%s > $(RESULTS_DIR)/eval/.end_time

# ============================================================================
# E2E Proxy Testing
# ============================================================================

E2E_PROXY_DIR := tests/e2e

# Rust E2E test command (tests assume Docker services + proxy are up)
E2E_CARGO_CMD = PROXY_BIN="$(CURDIR)/target/release/conproxy" cargo test --test e2e_proxy --features e2e -- --ignored --nocapture

# Run all proxy E2E tests (requires Docker)
e2e-all: build-release
	@echo "Running all proxy E2E tests..."
	$(MAKE) e2e-services-up
	$(MAKE) e2e-wait
	$(MAKE) e2e-load-data
	E2E_SUITE=all E2E_OUTPUT_DIR="$(E2E_PROXY_DIR)/results/$(shell date +%Y%m%d-%H%M%S)" $(E2E_CARGO_CMD)
	$(MAKE) e2e-services-down

# Run e2e tests against the live k8s cluster (kind + helm-installed conproxy).
# Requires:
#   - kind cluster up (./scripts/kind-up.sh)
#   - conproxy deployed via helm to kind (make k8s-deploy or `tilt up`)
#   - backends up on host (docker compose -f tests/e2e/docker-compose.yml up -d)
#   - corpus seeded (cargo run --bin corpus_seed --features embed,pgvector -- --corpus all --host http://localhost)
#   - port-forward svc/conproxy (e.g. `kubectl port-forward svc/conproxy 10000:10000 &`)
# Then runs cargo test --test e2e_proxy with all backend URLs + E2E_EXTERNAL_PROXY=1.
e2e-k8s:
	@PROXY_URL="$${PROXY_URL:-http://127.0.0.1:10000}" \
	QDRANT_URL="$${QDRANT_URL:-http://localhost:6333}" \
	ELASTIC_URL="$${ELASTIC_URL:-http://localhost:9200}" \
	OPENSEARCH_URL="$${OPENSEARCH_URL:-http://localhost:9201}" \
	MEILI1_URL="$${MEILI1_URL:-http://localhost:7700}" \
	MEILI2_URL="$${MEILI2_URL:-http://localhost:7701}" \
	PGVECTOR_URL="$${PGVECTOR_URL:-postgres://postgres:postgres@localhost:5432/conproxy_test}" \
	E2E_EXTERNAL_PROXY=1 \
	E2E_SUITE="$${E2E_SUITE:-all}" \
	$(CURDIR)/scripts/e2e-k8s.sh

# ---------------------------------------------------------------------------
# Dev lifecycle (kind + backends + tilt + seed)
# ---------------------------------------------------------------------------

# Tear down the full dev stack (tilt, kind, backends, free ports)
dev-down:
	@echo "Tearing down dev stack..."
	$(CURDIR)/scripts/dev-down.sh

# Start dev stack (kind → tilt up). Backends are auto-started by Tilt.
dev-up:
	@echo "Starting dev stack..."
	$(CURDIR)/scripts/dev-up.sh

# Full restart: teardown → fresh kind → backends → seed → tilt up
dev-restart:
	@echo "Restarting dev stack..."
	$(CURDIR)/scripts/dev-restart.sh

# ---------------------------------------------------------------------------
# DevEx: opencode-test container + auto-smoke (fresh session per container)
# ---------------------------------------------------------------------------

# Run the DevEx auto-smoke against the live cluster + opencode-test container.
# Mints a new session if no SID is set; otherwise continues the sticky one
# for the current container lifetime. The opencode session DB is in-container
# only — every container recreate starts with an empty session list and the
# host sticky SID is cleared. Re-runnable. Default model is
# opencode/big-pickle (free built-in, no key). Override with
# DEVEX_MODEL=opencode/<other> to swap.
devex:
	@echo "Running DevEx auto-smoke against live cluster..."
	@./scripts/devex-session.sh ensure
	@DEVEX_OPENCODE_PORT=$${DEVEX_OPENCODE_PORT:-14096} \
	 DEVEX_MODEL=$${DEVEX_MODEL:-opencode/big-pickle} \
	 ./scripts/devex-smoke.sh

# Attach the human TUI to the current DevEx session (in-container only;
# fails after a container recreate until a new smoke mints one).
devex-attach:
	@./scripts/devex-session.sh ensure
	@./scripts/devex-session.sh attach-cmd | bash

# Print the current DEVEX_SESSION + the last smoke result.
devex-status:
	@./scripts/devex-session.sh status

# Mint a fresh DevEx session on the next smoke.
devex-new:
	@./scripts/devex-session.sh clear
	@echo "Next 'make devex' (or Tilt devex-smoke) will mint a new session."

# Print the DevEx attach banner (used by docs / shell startup).
devex-banner:
	@./scripts/devex-session.sh banner

# Run all proxy E2E tests in dirty mode (no cleanup, for manual inspection)
e2e-dirty: build-release
	@echo "Running all proxy E2E tests (dirty mode)..."
	$(MAKE) e2e-services-up
	$(MAKE) e2e-wait
	$(MAKE) e2e-load-data
	E2E_SUITE=all $(E2E_CARGO_CMD) || true
	@echo "Services still running. Use 'make e2e-services-down' to clean up."

# Run Qdrant-only E2E tests
e2e-qdrant: build-release
	@echo "Running Qdrant E2E tests..."
	$(MAKE) e2e-services-up
	$(MAKE) e2e-wait
	$(MAKE) e2e-load-data
	E2E_SUITE=qdrant $(E2E_CARGO_CMD)
	$(MAKE) e2e-services-down

# Run Meilisearch E2E tests (Suite::Elastic = Meilisearch fixtures, name kept for env compat)
e2e-elastic: build-release
	@echo "Running Meilisearch E2E tests..."
	$(MAKE) e2e-services-up
	$(MAKE) e2e-wait
	$(MAKE) e2e-load-data
	E2E_SUITE=elastic $(E2E_CARGO_CMD)
	$(MAKE) e2e-services-down

e2e-meili: e2e-elastic

# Run mixed upstream E2E tests
e2e-mixed: build-release
	@echo "Running mixed upstream E2E tests..."
	$(MAKE) e2e-services-up
	$(MAKE) e2e-wait
	$(MAKE) e2e-load-data
	E2E_SUITE=mixed $(E2E_CARGO_CMD)
	$(MAKE) e2e-services-down

# Run E2E tests filtered by category
e2e-filter: build-release
	@echo "Running proxy E2E tests (filter: $(FILTER))..."
	E2E_SUITE=all E2E_FILTER=$(FILTER) $(E2E_CARGO_CMD)

# Quick smoke test (requires running proxy)
e2e-smoke: build-release
	@echo "Running smoke test..."
	$(E2E_CARGO_CMD) e2e_smoke_test

# Run proxy load benchmarks (rlt, gRPC + HTTP)
e2e-bench: build-release
	@echo "Running proxy load benchmarks..."
	PROXY_URL=http://localhost:8081 GRPC_URL=http://localhost:8080 cargo test --test e2e_load --features load-test,e2e --release

# Start E2E test services
e2e-services-up:
	@echo "Starting E2E test services..."
	@if [ ! -d "$(E2E_PROXY_DIR)" ]; then echo "ERROR: E2E infra dir not found at $(E2E_PROXY_DIR)"; exit 1; fi
	cd $(E2E_PROXY_DIR) && docker compose up -d

# Stop E2E test services
e2e-services-down:
	@echo "Stopping E2E test services..."
	@if [ -d "$(E2E_PROXY_DIR)" ]; then cd $(E2E_PROXY_DIR) && docker compose down -v; fi

# Wait for E2E services to be ready
e2e-wait:
	cargo run --bin test_runner --features $(or $(TEST_ALL_FEATURES),default) -- wait all

# Load test data into services
e2e-load-data:
	@echo "Loading test data..."
	cargo run --bin test_runner --features $(or $(TEST_ALL_FEATURES),default) -- load-data

# Generate embeddings for test data
e2e-generate-embeddings:
	@echo "Generating embeddings..."
	cargo run --bin generate_embeddings --features embed

# View latest E2E results
e2e-results:
	@latest=$$(ls -td $(E2E_PROXY_DIR)/results/*/ 2>/dev/null | head -1); \
	if [ -z "$$latest" ]; then echo "No results found. Run 'make e2e-all' first."; exit 1; fi; \
	echo "Latest results: $$latest"; \
	echo ""; \
	jq '.' "$$latest/results.json"

# Clean proxy E2E artifacts
e2e-proxy-clean:
	cd $(E2E_PROXY_DIR) && docker compose down -v 2>/dev/null || true
	rm -rf $(E2E_PROXY_DIR)/data/embeddings.json
	rm -rf $(E2E_PROXY_DIR)/data/query_embeddings.json
	rm -rf $(E2E_PROXY_DIR)/data/*_embeddings.json
	rm -rf $(E2E_PROXY_DIR)/results

# Generate markdown report from latest E2E results
e2e-report:
	@latest=$$(ls -td $(E2E_PROXY_DIR)/results/*/ 2>/dev/null | head -1); \
	if [ -z "$$latest" ]; then echo "No results found. Run 'make e2e-all' first."; exit 1; fi; \
	echo "Generating report from: $$latest"; \
	cargo run --bin test_runner -- report \
		--input "$$latest/results.json" \
		--output "$$latest/report.md" \
		--html "$$latest/report.html"; \
	echo "Report written to: $$latest/report.md"

# Run E2E benchmark comparison against previous results
e2e-bench-compare:
	@results=($$(ls -td $(E2E_PROXY_DIR)/results/*/ 2>/dev/null)); \
	if [ $${#results[@]} -lt 2 ]; then \
		echo "Need at least 2 result sets to compare. Run 'make e2e-all' twice."; exit 1; \
	fi; \
	current=$${results[0]}; \
	previous=$${results[1]}; \
	echo "Comparing:"; \
	echo "  Current:  $$current"; \
	echo "  Previous: $$previous"; \
	cargo run --bin test_runner -- report \
		--input "$$current/results.json" \
		--compare "$$previous/results.json" \
		--output "$$current/comparison.md" \
		--html "$$current/comparison.html"; \
	echo ""; \
	echo "Comparison report written to: $$current/comparison.md"

# ============================================================================
# Eval helpers (llama-server direct — not passthrough)
# ============================================================================

# Check that llama-server is running and /v1/models responds (for eval-llamacpp).
llm-server-check:
	@echo "Checking llama-server at $$LLAMA_BASE_URL ..."; \
	url=$${LLAMA_BASE_URL:-http://127.0.0.1:8081}/v1/models; \
	if curl -sf "$$url" > /dev/null 2>&1; then \
		echo "  llama-server: OK"; \
	else \
		echo "  FAIL: llama-server not reachable at $$url"; \
		echo "  Ensure llama-server is running."; \
		echo "  Default: llama-server -m models/llm.gguf --port 8081 --host 127.0.0.1 -c 2048 --jinja"; \
		exit 1; \
	fi


# E2E Eval with llama.cpp provider (hard fail, no soft skip).
eval-llamacpp: build-release
	$(MAKE) llm-server-check
	$(MAKE) e2e-services-up
	$(MAKE) e2e-wait
	$(MAKE) e2e-load-data
	EVAL_PROVIDER=llamacpp \
	EVAL_OUTPUT_DIR="tests/e2e_eval/results/$$(date +%Y%m%d-%H%M%S)" \
	PROXY_BIN="$(CURDIR)/target/release/conproxy" \
	    cargo test --test e2e_eval --features e2e -- --ignored --nocapture
	$(MAKE) e2e-services-down

# ============================================================================
# E2E Eval Testing (Ollama/Claude LLM vertical comparison)
# ============================================================================

E2E_EVAL_CMD = PROXY_BIN="$(CURDIR)/target/release/conproxy" \
    cargo test --test e2e_eval --features e2e -- --ignored --nocapture

eval-all: build-release
	$(MAKE) e2e-services-up
	$(MAKE) e2e-wait
	$(MAKE) e2e-load-data
	EVAL_OUTPUT_DIR="tests/e2e_eval/results/$$(date +%Y%m%d-%H%M%S)" $(E2E_EVAL_CMD)
	$(MAKE) e2e-services-down

eval-quick: build-release
	EVAL_OUTPUT_DIR="tests/e2e_eval/results/$$(date +%Y%m%d-%H%M%S)" $(E2E_EVAL_CMD)

eval-vertical: build-release
	EVAL_VERTICALS=$(V) EVAL_OUTPUT_DIR="tests/e2e_eval/results/$$(date +%Y%m%d-%H%M%S)" $(E2E_EVAL_CMD)

eval-queries: build-release
	EVAL_QUERIES=$(Q) EVAL_OUTPUT_DIR="tests/e2e_eval/results/$$(date +%Y%m%d-%H%M%S)" $(E2E_EVAL_CMD)

eval-cheap: build-release
	EVAL_VERTICALS=no_context,mcp_tools EVAL_QUERIES=q-001,q-004,q-003 EVAL_OUTPUT_DIR="tests/e2e_eval/results/$$(date +%Y%m%d-%H%M%S)" $(E2E_EVAL_CMD)

eval-results:
	@latest=$$(ls -td tests/e2e_eval/results/*/ 2>/dev/null | head -1); \
	[ -z "$$latest" ] && echo "No results. Run 'make eval-all' first." && exit 1; \
	jq '.' "$$latest/eval_results.json"

eval-clean:
	rm -rf tests/e2e_eval/results /tmp/conproxy-eval

# ============================================================================
# UAT Testing (CLI user acceptance tests)
# ============================================================================

uat: build-release
	PROXY_BIN="$(CURDIR)/target/release/conproxy" \
	    cargo test --test e2e_uat --features e2e -- --ignored --nocapture

uat-quick:
	PROXY_BIN="$(CURDIR)/target/release/conproxy" \
	    cargo test --test e2e_uat --features e2e -- --ignored --nocapture

# ============================================================================
# Profiled E2E/Eval Runs (resource monitoring via /proc)
# ============================================================================

e2e-profile: build-release
	@echo "Running E2E proxy tests with resource profiling..."
	$(MAKE) e2e-services-up
	$(MAKE) e2e-wait
	$(MAKE) e2e-load-data
	E2E_SUITE=all E2E_PROFILE=1 E2E_OUTPUT_DIR="$(E2E_PROXY_DIR)/results/$(shell date +%Y%m%d-%H%M%S)" $(E2E_CARGO_CMD)
	$(MAKE) e2e-services-down

eval-profile: build-release
	$(MAKE) e2e-services-up
	$(MAKE) e2e-wait
	$(MAKE) e2e-load-data
	E2E_PROFILE=1 EVAL_OUTPUT_DIR="tests/e2e_eval/results/$$(date +%Y%m%d-%H%M%S)" $(E2E_EVAL_CMD)
	$(MAKE) e2e-services-down

# ============================================================================
# Security
# ============================================================================

# cargo-deny: supply chain + license + advisory DB checks
security-deny:
	cargo deny check

# cargo-audit: standalone RustSec advisory scanner
audit:
	cargo audit

# Clippy with security-focused lints (separate from regular lint)
lint-security:
	cargo clippy -- -D warnings \
	  -W clippy::unwrap_used \
	  -W clippy::expect_used \
	  -W clippy::panic \
	  -W clippy::indexing_slicing \
	  -W clippy::arithmetic_side_effects

# Surface current pre-existing warning inventory (for SKILL.md Known Gaps refresh)
audit-known-gaps:
	@echo "=== 1. Pre-existing clippy unwrap/expect warnings ==="
	@cargo clippy -- -W clippy::unwrap_used,clippy::expect_used 2>&1 \
		| grep -E "warning: .*(unwrap|expect|used)" | wc -l \
		| xargs -I{} echo "count: {}" || echo "count: 0"
	@echo
	@echo "=== 2. Unallowed clippy warnings (should be 0) ==="
	@cargo clippy -- -D warnings 2>&1 \
		| grep -E "^(warning|error)" || echo "clean"
	@echo
	@echo "=== 3. Bench compile (passes/fails?) ==="
	@cargo bench --no-run 2>&1 | tail -3
	@echo
	@echo "Run this target when Known Gaps section needs refreshing."
	@echo "Line refs and counts drift as code moves; this target is authoritative."

# Security-focused E2E tests (auth bypass, rate limiting, payload abuse, header injection)
e2e-security: build-release
	E2E_SUITE=security E2E_FILTER=security PROXY_BIN="$(CURDIR)/target/release/conproxy" \
		cargo test --test e2e_proxy --features e2e -- --ignored --nocapture

# SBOM generation (CycloneDX format)
sbom:
	cargo cyclonedx --format json --output-file conproxy-sbom.json

# cargo-geiger: scan dependency tree for unsafe usage
unsafe-audit:
	cargo geiger --output-format ascii 2>&1 | tee unsafe-report.txt

# cargo-mutants: mutation testing on security-critical modules
mutant-security:
	cargo mutants -F 'src/proxy/middleware.rs' -F 'src/proxy/sandbox.rs' -F 'src/proxy/agent.rs'

# Fuzz targets
fuzz-query:
	cargo fuzz run fuzz_query_request -- -max_len=4096 -max_total_time=60

fuzz-config:
	cargo fuzz run fuzz_config_parse -- -max_len=8192 -max_total_time=60

fuzz-all:
	@for target in $$(cargo fuzz list); do \
		echo "Fuzzing $$target for 60s..."; \
		timeout 60 cargo fuzz run $$target -- -max_len=4096 || true; \
	done

# Fast security gate (< 2 min, run on every PR)
security-quick: audit security-deny lint-security

# Full security suite (run nightly/weekly)
security-full: security-quick sbom unsafe-audit fuzz-all e2e-security

# ============================================================================
# Clean
# ============================================================================

clean:
	cargo clean

# ============================================================================
# Help
# ============================================================================

help:
	@echo "conproxy Makefile"
	@echo ""
	@echo "Build targets:"
	@echo "  build             - Debug build"
	@echo "  build-release     - Release build"
	@echo "  build-embed       - Release with embedding feature"
	@echo "  build-persistence - Build with persistence feature"
	@echo "  build-pgvector    - Build with pgvector feature"
	@echo "  build-mcp         - Release with MCP server"
	@echo "  build-all         - Release with all standard features"
	@echo "  build-profiling   - Profiling build (release + debug symbols + dhat)"
	@echo "  docker-build      - Build conproxy:dev (Tilt default)"
	@echo "  docker-push V= R= - Versioned push (V=0.1.0 R=ghcr.io/me)"
	@echo "  docker-buildx     - Local multi-arch build (PLATFORMS=...)"
	@echo ""
	@echo "Test targets:"
	@echo "  test              - Run all tests (unit)"
	@echo "  test-unit         - Run unit tests only"
	@echo "  t                 - Fast local loop (nextest if installed, else cargo test --lib)"
	@echo "  test-fast         - Alias for t"
	@echo "  test-nextest      - Force cargo-nextest (dev profile)"
	@echo "  test-slow         - Show top-25 slowest tests (nextest)"
	@echo "  test-filter PAT=  - Run tests matching PAT"
	@echo "  nextest-install   - One-time install of cargo-nextest"
	@echo "  target-prune      - Drop conproxy-only build artifacts"
	@echo "  test-verbose      - Run tests with output"
	@echo "  test-coverage     - Generate coverage report (HTML)"
	@echo "  test-coverage-quick - Quick coverage to stdout"
	@echo "  test-coverage-check - Check per-file coverage threshold (80%)"
	@echo "  test-one TEST=name - Run specific test"
	@echo ""
	@echo "Proxy targets:"
	@echo "  proxy-start       - Start the proxy server"
	@echo "  proxy-stop        - Stop the proxy server"
	@echo "  proxy-status      - Show proxy status"
	@echo ""
	@echo "Quality targets:"
	@echo "  fmt               - Format code"
	@echo "  fmt-check         - Check formatting"
	@echo "  lint              - Run clippy"
	@echo "  lint-fix          - Auto-fix lint issues"
	@echo ""
	@echo "Benchmark targets:"
	@echo "  bench             - Run benchmarks"
	@echo "  bench-save        - Save baseline for comparison"
	@echo "  bench-compare     - Compare against saved baseline + report"
	@echo "  bench-hitrate     - Cache hit-rate benchmark (agentic + Zipf traces)"
	@echo "  bench-hitrate-sem - Hit-rate + semantic τ frontier (embed-api; ~6 min)"
	@echo "  bench-hitrate-onnx - Semantic τ frontier with live ONNX embedder (embed)"
	@echo "  bench-hitrate-live - Live wire mode vs real proxy + qdrant (docker + embed)"
	@echo ""
	@echo "E2E Proxy Testing targets:"
	@echo "  e2e-all           - Run all proxy E2E tests (Docker required)"
	@echo "  e2e-dirty         - Run all tests, keep services running"
	@echo "  e2e-qdrant        - Run Qdrant-only tests"
	@echo "  e2e-elastic       - Run Meilisearch tests (historical name; config single-elasticsearch.toml)"
	@echo "  e2e-meili         - Alias for e2e-elastic"
	@echo "  e2e-mixed         - Run mixed upstream tests"
	@echo "  e2e-filter FILTER=cat - Run by category"
	@echo "  e2e-smoke         - Quick smoke test (proxy must be running)"
	@echo "  e2e-smoke-core    - Docker up + load + smoke/health/query filter"
	@echo "  e2e-cascade       - Docker up + load + cascade filter (mixed suite)"
	@echo "  e2e-federated     - Federated category (mock upstream, no compose)"
	@echo "  test-integration  - Full core testcontainers matrix (Docker)"
	@echo "  test-integration-experimental - pinecone/milvus mock tests"
	@echo "  e2e-bench         - Run proxy load benchmarks (rlt, gRPC + HTTP)"
	@echo "  e2e-services-up   - Start Docker test services"
	@echo "  e2e-services-down - Stop Docker test services"
	@echo "  e2e-load-data     - Load test data into services"
	@echo "  e2e-results       - View latest E2E test results"
	@echo "  e2e-report        - Generate markdown report from latest results"
	@echo "  e2e-bench-compare - Compare latest two E2E result sets"
	@echo "  e2e-proxy-clean   - Clean proxy E2E artifacts + results"
	@echo ""
	@echo "Eval helpers (direct llama.cpp — not passthrough):"
	@echo "  llm-server-check  - Verify llama-server is running (hard fail)"
	@echo "  eval-llamacpp     - Run eval suite with llama.cpp provider (hard fail)"
	@echo ""
	@echo "E2E Eval Testing targets (Ollama/Claude LLM vertical comparison):"
	@echo "  eval-all          - Full eval: services up, load data, all verticals, down"
	@echo "  eval-quick        - All verticals, assumes Docker services already running"
	@echo "  eval-vertical V=name - Single vertical (no_context, knowledge_only, mcp_tools)"
	@echo "  eval-queries Q=ids - Query subset (q-001,q-003,...)"
	@echo "  eval-cheap        - 2 verticals x 3 queries (quick Ollama eval)"
	@echo "  eval-results      - View latest JSON results"
	@echo "  eval-clean        - Remove eval results and /tmp/conproxy-eval/"
	@echo ""
	@echo "UAT Testing targets (CLI user acceptance tests):"
	@echo "  uat               - Run CLI UAT tests (builds release binary first)"
	@echo "  uat-quick         - Run CLI UAT tests (assumes binary already built)"
	@echo ""
	@echo "Profiled runs (resource monitoring via /proc):"
	@echo "  e2e-profile       - E2E proxy tests + process resource profiling"
	@echo "  eval-profile      - E2E eval tests + process resource profiling"
	@echo "  profile-dhat      - DHAT heap profile (requires Qdrant running)"
	@echo "  profile-pgo       - PGO build (release + instrumentation + workload)"
	@echo "  profile-flamegraph - CPU flamegraph (requires flamegraph or samply)"
	@echo "  profile-tokio-console - Tokio async task console (CLI client; default bind 127.0.0.1:6669)"
	@echo "  profile-heaptrack     - Heap timeline profile (optional heaptrack tool; needs apt install heaptrack)"
	@echo "  profile-metrics-snap  - Prometheus metrics snapshot under load (writes /tmp/conproxy-metrics/metrics-prometheus.txt)"
	@echo ""
	@echo "Perf tuning (structured measure → summarize → plan):"
	@echo "  perf-tuning-quick   - Bench both targets (--baseline main) + summarize (fast feedback)"
	@echo "  perf-tuning-default - Quick + metrics-snap (CI-aware verdict + parsed metrics)"
	@echo "  perf-tuning-full    - Default + flamegraph + hitrate + heaptrack + tokio-console (env off-switches: HITRATE=0 HEAPTRACK=0 TOKIO_CONSOLE=0 FLAMEGRAPH=0 DHAT=1)"
	@echo "  perf-tuning-clean   - Remove tests/results/perf-tuning/ (gitignored)"
	@echo "  perf-publish        - Copy latest run artifacts to perf-history/<ts>-<sha8>/ (tracked, max 100 runs; prints git add cmd)"
	@echo ""
	@echo "Full suite (results in tests/results/<timestamp>/<profile>/):"
	@echo "  test-all          - Run ALL: quality + perf (sequential)"
	@echo "  test-all-quality  - Lint + unit + coverage"
	@echo "  test-all-perf     - Bench + e2e + load + eval"
	@echo "  test-all-prebuild - Pre-build all binaries and test targets"
	@echo ""
	@echo "Security targets:"
	@echo "  audit             - Run cargo-audit (RustSec advisory scan)"
	@echo "  security-deny     - Run cargo-deny (supply chain + license checks)"
	@echo "  lint-security     - Clippy with security-focused lints"
	@echo "  audit-known-gaps  - Surface current pre-existing warning inventory (for SKILL.md refresh)"
	@echo "  e2e-security      - Security-focused E2E tests (auth, rate limit, payload)"
	@echo "  sbom              - Generate SBOM (CycloneDX JSON)"
	@echo "  unsafe-audit      - Scan dependency tree for unsafe usage (cargo-geiger)"
	@echo "  mutant-security   - Mutation testing on security-critical modules"
	@echo "  fuzz-query        - Fuzz QueryRequest deserialization (60s)"
	@echo "  fuzz-config       - Fuzz config TOML parsing (60s)"
	@echo "  fuzz-all          - Fuzz all targets (60s each)"
	@echo "  security-quick    - Fast security gate: audit + deny + lint-security"
	@echo "  security-full     - Full suite: quick + sbom + geiger + fuzz + e2e-security"
	@echo ""
	@echo "Dev lifecycle (kind + Tilt + backends):"
	@echo "  dev-up            - Start: kind → tilt up (foreground)"
	@echo "  dev-down          - Stop: tilt down → free ports → kind-down → compose down -v"
	@echo "  dev-restart       - Full restart: teardown → fresh kind → backends → seed → tilt up"
	@echo "  devex             - Run DevEx auto-smoke (opencode MCP smoke) against live cluster"
	@echo "  devex-attach      - Exec TUI into opencode-test container with current session"
	@echo "  devex-status      - Print DEVEX_SESSION + last smoke result"
	@echo "  devex-new         - Mint a fresh DevEx session on next smoke"
	@echo "  devex-banner      - Print the DevEx handoff banner"
	@echo ""
	@echo "Feature flags: test (load-test+dhat-heap), embed, persistence, pgvector, mcp, linux-sandbox"
	@echo ""
	@echo "Quality gates (plan 05):"
	@echo "  cov-scope-tune    - Coverage gate: scope + tune ≥80% line (tarpaulin)"
	@echo "  proof-cascade     - Cascade + federated unit proofs (no Docker)"
	@echo "  sdk-smoke         - Python SDK: maturin build + import conproxy"
	@echo ""
	@echo "Other targets:"
	@echo "  clean             - Clean build artifacts"
	@echo "  help              - Show this help"