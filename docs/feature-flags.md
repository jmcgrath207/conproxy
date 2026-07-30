# Feature Flags

Conproxy uses Cargo feature flags to control which optional modules are compiled in. This keeps the default binary small and dependency-light while letting you opt into features as needed.

## Available Features

| Feature | What it enables | External dependencies | Binary size impact |
|---------|----------------|----------------------|-------------------|
| `mcp` | MCP server for stdio MCP clients (Claude Desktop, opencode) | rmcp, schemars | ~2 MB |
| `embed-api` | Embedder provider trait + OpenAI/Cohere/HuggingFace API clients (no ONNX) | — | ~0.5 MB |
| `embed` | Local ONNX embedding for VectorOnly upstreams (implies `embed-api`) | ort, tokenizers, ndarray | ~15 MB |
| `persistence` | Disk-backed cache using redb (survives restarts) | redb | ~1 MB |
| `pgvector` | pgvector adapter for PostgreSQL vector search | tokio-postgres | ~1 MB |
| `linux-sandbox` | seccomp sandbox (Linux only) | caps, nix | ~0.5 MB |
| `e2e` | E2E proxy tests (requires running instance) | — | test only |
| `load-test` | Load testing infrastructure | rlt, rand, rand_distr, hdrhistogram | test only |
| `integration-tests` | Integration tests against real backends (requires Docker) | testcontainers | test only |
| `dhat-heap` | DHAT heap profiling | dhat | dev only |
| `tokio-console` | Async runtime diagnosis (tokio-console) | console-subscriber | dev only |
| `tokio-taskdump` | `Handle::dump()` task backtraces (`GET /debug/tokio/dump`) — requires `RUSTFLAGS=--cfg tokio_unstable` | — | dev only |
| `tokio-console-snap` | `console_snap` headless dump bin (CI-friendly alternative to tokio-console TUI) | console-api | dev only |

## Meta-Features

| Feature | Includes | Purpose |
|---------|----------|---------|
| `release` | `mcp`, `persistence`, `embed-api`, `pgvector` | Standard production binary (MCP + durable cache + remote embed + pgvector; ONNX/sandbox opt-in) |
| `test` | `release`, `load-test`, `dhat-heap` | Full test infrastructure |

**Plan 10:** `release` = `mcp` + `persistence` + `embed-api` + `pgvector`.  
**Out of `release` (opt-in only):**
- **`embed`** (ONNX / `ort`) — dynamic link pain for precompiled binaries. Add `,embed` to opt in.
- **`linux-sandbox`** — Linux-only. Add `,linux-sandbox` on Linux.

`embed-api` is in `release` so remote embedders (OpenAI/Cohere/HF) work out of the box. The ONNX local runtime stays opt-in due to the dynamic-link issue for shipped binaries.

## Recommended Combinations

### AI agent use (MCP)

```bash
cargo install --path . --release --features release
```

Includes: MCP server. This is the recommended build for most users.

### With local embedding

For pgvector or Milvus upstreams that need the proxy to embed queries:

```bash
cargo install --path . --release --features release,embed
```

### With disk persistence

`release` already includes `persistence`. Explicit extra flag is redundant but harmless:

```bash
cargo install --path . --release --features release
# equivalent historical form:
cargo install --path . --release --features release,persistence
```

### With pgvector

Direct pgvector adapter (generates SQL queries):

```bash
cargo install --path . --release --features release,pgvector
```

### Full production

`release` already includes `persistence` and `pgvector`. Add `embed` for
local ONNX embedding and `linux-sandbox` for the seccomp sandbox (Linux
only):

```bash
cargo install --path . --release --locked --features release,embed,linux-sandbox
```

### Minimal (no optional features)

Just the HTTP/gRPC cache proxy with no optional dependencies:

```bash
cargo install --path . --release
```

## Building with Multiple Features

Features are comma-separated:

```bash
cargo build --release --features mcp,embed,persistence
```

Or in Cargo.toml for a downstream crate:

```toml
[dependencies]
conproxy = { path = "../conproxy", features = ["mcp", "persistence"] }
```

## Feature Details

### `mcp`

Adds the `conproxy mcp` command which starts an MCP server over stdio. stdio MCP clients (Claude Desktop, opencode) launch this process and communicate using the Model Context Protocol to call search tools. See [MCP Integration](mcp-integration.md).

### `embed-api`

Enables the `EmbedderProvider` trait and the API-based embedder implementations (OpenAI, Cohere, HuggingFace). These use `reqwest` and are lightweight — no ONNX runtime required. Useful for hosted embedding models where the local ONNX dependency is undesirable.

Use the `[embedding]` config section to select a provider (`onnx`, `openai`, `cohere`, `huggingface`) and supply an `api_key` (supports `${ENV_VAR}` references).

### `embed`

Adds local ONNX-based embedding using the `ort` runtime. Implies `embed-api`, so all API providers and the embedder trait are also available. Required when upstreams are configured with `query_mode = "vector_only"` (pgvector, Milvus) and you want local embedding. The proxy converts text queries to vectors before forwarding.

**Hard gate:** `cargo build --features embed --lib`.  
`cargo test --features embed --lib` may fail to link on some hosts — use `embed-api` for routine tests.

Also enables:
- `generate_embeddings` binary for batch embedding
- Smart embedder with embedding cache and request coalescing
- ONNX path helpers (`ModelManager`) — place `model.onnx` + `tokenizer.json` under `~/.conproxy/models/<name>/`


### `persistence`

Replaces the ephemeral in-memory cache with a redb-backed store. Cache entries survive process restarts. The redb database is stored in `.conproxy/cache/`.

### `pgvector`

Adds a native pgvector adapter that generates SQL queries against PostgreSQL tables with the pgvector extension. Configure pgvector upstreams with `upstream_type = "pgvector"` and provide table/column details.

**Gating rationale**: `pgvector` is feature-gated because it pulls in `tokio-postgres` (a native PostgreSQL client). Other backends (`elasticsearch`, `opensearch`, `qdrant`, `pinecone`, `milvus`, `meilisearch`) are HTTP-only and stay always-on — they use `reqwest` with no native deps. Rule of thumb: native-dep adapters get a feature flag, HTTP-only adapters don't. (Solr removed; Pinecone/Milvus shipped Wave 3.)

### `linux-sandbox`

Applies a seccomp sandbox on Linux that restricts the process to a minimal set of system calls. Useful for security-sensitive deployments.

**When to enable**: Enable when running conproxy as a **bare binary** started as root (e.g., to bind to ports <1024, or via systemd with `User=root`). The sandbox drops to an unprivileged user/group, sets `PR_SET_NO_NEW_PRIVS`, and drops Linux capabilities after binding — so a parser/FFI/RCE vulnerability in the proxy lands as an unprivileged user instead of full root.

**When to skip**: When running inside a **container** (Docker, Kubernetes, Podman), the container runtime already provides seccomp filtering, capability bounding, and `no-new-privileges` via `--cap-drop=ALL --security-opt=no-new-privileges` or PodSecurityStandards. Adding the feature is harmless but redundant. Omit it to keep the binary and build smaller.

On non-Linux targets, the feature compiles to nothing (no effect).

### `e2e`

Enables integration tests that require a running conproxy instance and real upstream services. Used with Docker Compose for CI/CD testing. See `tests/e2e/` for infrastructure.

### `load-test`

Enables the load testing framework (rlt-based). Builds the `e2e_load` test binary for benchmarking proxy throughput and latency under load.

### `dhat-heap`

Enables heap profiling via DHAT. Set `CONPROXY_DHAT=1` to activate the profiler at runtime. Produces a `dhat-heap.json` file for analysis.

### `tokio-console`

Enables [tokio-console](https://github.com/tokio-rs/console) for async runtime diagnosis. Build with `RUSTFLAGS="--cfg tokio_unstable"` (set automatically by `make profile-tokio-console`). Provides real-time visibility into async task states, poll durations, and waker chains.

**Server:** gRPC on `127.0.0.1:6669` by default (override with `TOKIO_CONSOLE_BIND=host:port`).
**Client:** NOT a browser — use the `tokio-console` CLI:
```bash
cargo install tokio-console
tokio-console http://127.0.0.1:6669
```

Dev/diagnosis only — not for production builds.

### `integration-tests`

Enables the testcontainers-based integration test suite. Hits real backends
(Qdrant, Elasticsearch, Meilisearch, Postgres) spun up in Docker. Use
`make test-integration` to run; requires a Docker daemon. Not in the
default `test` meta-feature — opt in explicitly to keep CI builds light.

### `tokio-taskdump`

Enables `tokio::runtime::Handle::dump()` for one-shot task backtraces
(stuck-task debug). Exposed via `GET /debug/tokio/dump` on the HTTP admin
port. Requires `RUSTFLAGS=--cfg tokio_unstable` at build time; the
endpoint returns 503 with a hint if the binary wasn't built that way.

### `tokio-console-snap`

Enables the `console_snap` binary — a headless alternative to the
tokio-console TUI. Connects to the same gRPC endpoint (port 6669),
samples for a fixed duration, and writes `console-snap.json` +
`console-snap.txt` with top tasks by total poll time. Used by
`make perf-tuning-full` when `TOKIO_CONSOLE=1` for CI-friendly tokio
introspection.
