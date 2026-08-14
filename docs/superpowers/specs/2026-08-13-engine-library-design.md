# In-process Engine library

**Date:** 2026-08-13
**Status:** Draft — awaiting review
**Product:** conproxy query core, embeddable in developer-owned MCP servers (Rust + Python)

## 1. Problem

Developers who write their own MCP servers want conproxy’s retrieval-leg cache **inside their process**. Today they must run the daemon and talk to it over gRPC/HTTP (`conproxy mcp`, `ConproxyClient`).

This is not an LLM-answer cache and not a generic tool-result cache. It is the daemon’s **request path** as a library.

## 2. Goal

Ship `Engine`: a thin facade over the **same** `AppState` + `query_core::execute_query` the HTTP handler already calls. Do not reimplement the stack.

- Rust: `conproxy::engine::Engine` (`src/engine/`).
- Python: `from conproxy import Engine` next to remote `ConproxyClient`.
- MCP tools `await engine.query(...)`.
- No gRPC listen, no daemon required. Optional read-only HTTP dashboard (`dashboard_listen`, default off).

## 3. Parity cut

**In — query core**

Same modules as a daemon request: `CacheStore` (papaya + S3-FIFO), TTL/jitter, semantic, persist, all adapters, cascade/federated, pool, circuit, singleflight, serve-stale (**refresh worker must be started**), contexts, scope/embed-band, negative cache, retry.

API: `query` (async), `clear`, `stats`, `close`. Response uses existing `QueryResponse.cache_status` (`Hit` / `Miss` / `Stale` / `Frozen`).

**Out — process / ops**

HTTP/gRPC listen, the daemon's full REST (mutating) API, `conproxy mcp`, tune, peer/CDC (`peer.start()` is not called), file-watch reload, pause/resume, inbound agent API keys.

**Dashboard carve-out** (supersedes the "no dashboard" lock above): Engine may bind a **read-only** HTTP server on an opt-in `dashboard_listen` — the embedded SPA plus exactly the GETs it needs (`/health`, `/stats`, `/metrics`, `/circuit`, `/queue`, `/pool`, `/cache/integrity`, `/cache/upstreams`, `/cache/entries`, `/contexts`, `/contexts/current`, `/contexts/{id}/stats`, `/debug/tokio`, `/peer/status`). No `/query`, no admin/cache mutations, no auth (loopback unless a wider address is passed). Daemon router unchanged.

**Out — parked**

- `@cached` / `wrap` / `get` / `put`
- `top_k` in the cache key (daemon hashes the query string only)
- LFU-on-main — `docs/superpowers/specs/2026-08-13-s3fifo-lfu-main-experiment.md`
- Read-through redb on live miss
- Builder-only upstreams without toml
- Bundling ONNX models; a `conproxy-engine` crate
- Sync Python `query()` that `block_on`s (deadlock)

## 4. Architecture

```
Host
  │  stdio MCP
  ▼
Developer MCP server
  await engine.query(q, top_k=10)
          │
          ▼
   Engine (Arc<AppState>)
     query_core::execute_query(...)
     refresh worker spawned (serve-stale)
     peer.start() NOT called
     agent = None (no inbound auth)
          │ miss
          ▼
     vector DB
```

**Implementation rule:** `Engine` constructs `AppState` (existing `AppState::new` + Engine overrides), creates/switches context `"global"`, starts the refresh worker the HTTP bind path starts today, then every `query` is `execute_query`. No second cache/upstream stack.

**Cache pressures (existing `CacheStore`)**

```
query()
  └─ papaya map
       ├─ S3-FIFO     max_entries — small / main / ghost
       ├─ memory cap  max_memory  — same evict loop, values only
       │              dynamic by default (memory_fraction × budget,
       │              live-resized by 10s ticker)
       └─ redb        persist_path — write-through + restore on open
                      get() does not read disk on miss
```

- **small** = probation. Cold → evict value, hash → **ghost**. Hit → **main**.
- **main** = FIFO + second chance. No demotion. Main evict does not ghost.
- **ghost** = hashes only. Re-insert admits to **main**. Not in `max_memory`.
- **TTL** is independent. Expiry does not ghost.
- S3-FIFO is eviction, not hit-path speed.

**Daemon vs library**

| | Daemon | Engine |
|---|---|---|
| Process | `conproxy` binary | Inside the MCP process |
| Transport | gRPC / HTTP / `conproxy mcp` | Function call |
| Request path | `handle_query` → `execute_query` | `Engine.query` → `execute_query` |
| Default context | `"default"` | `"global"` (must be created on build) |
| Refresh worker | Started with HTTP server | Started with `Engine` |
| Listen / tune / peer | Yes | No |

**Crate graph**

- `src/engine/` is the public surface. `AppState` / `query_core` stay `pub(crate)`.
- `conproxy-sdk` stays a gRPC client.
- `sdk/python`: `ConproxyClient` on `conproxy-sdk`; `Engine` wraps `conproxy::engine::Engine`.
- Linking Engine pulls most of the proxy. The default Python wheel is **not** thin. Docs must say so.

## 5. Public API

### 5.1 Construction

**Rust** — sync `build()` (caller already on tokio).

```rust
let engine = Engine::builder()
    .config_toml("conproxy.toml")
    .persist_path("/var/lib/my-mcp/cache")
    .embedder(EmbedderSpec::Onnx { model: "all-MiniLM-L6-v2" })
    .ttl(Duration::from_secs(300))
    .stale_ttl(Duration::from_secs(3600))
    .context("global")
    .semantic(true)
    .threshold(None)
    .max_entries(10_000)
    .max_memory(Some(256 * 1024 * 1024))
    .build()?;
```

**Memory cap — fixed or dynamic (default dynamic).**

- `max_memory="256MiB"` / int → **fixed** cap, no resizing.
- `memory_fraction=0.7` (**default**) → **dynamic** cap = `fraction × budget`.
  - Budget = cgroup v2 `memory.max` (walk up if `max`), else `/proc/meminfo MemAvailable`, else 256 MiB.
  - A 10s ticker re-reads the budget and applies when the cap moves by >5% (floor 16 MiB).
  - Shrinks run the S3-FIFO eviction loop immediately (`enforce_memory_limit`).
- `max_memory` wins over `memory_fraction` if both are passed.
- Rationale: a fixed default is wrong in containers — 70% of the budget tracks cgroup/host pressure instead of a guess.

After `AppState::new(&config)`: apply kwargs via existing setters; **create/switch** `ContextManager` to `"global"` (or the `context=` override). `ContextConfig::default()` is `"default"` — do not leave it there.

Start the refresh worker. Do not call `peer.start()`. Bind sockets only when `dashboard_listen` is set (read-only dashboard, default off).

**Python — both constructors, one object**

```python
engine = Engine(config="conproxy.toml", ...)            # FastMCP-style; may block on toml/redb/ONNX
engine = await Engine.create(config="conproxy.toml", ...)  # same build, off the event loop
```

`create()` is not a second implementation. It runs the same construct off-loop so startup I/O does not freeze asyncio.

Sync construct owns a tokio multi-thread runtime (same pattern as `ConproxyClient`) and starts the refresh worker there.

`Engine` is `Clone` (`Arc` inner).

Constructor kwargs override toml, then builtins (`ttl=300`, `stale_ttl=3600`, `context="global"`, `semantic=True`).

Library does **not** install a tracing subscriber. `fork()` after construct is undefined.

### 5.2 `query` (async only in Python)

Key = `CacheStore::hash_query` (query string only).

```python
result = await engine.query(
    "how does X work",
    top_k=10,
    context=None,       # → Engine.context ("global")
    ttl=None,
    semantic=None,
    threshold=None,
    skip_cache=False,
)
# result.cache_status in {hit, miss, stale, frozen}
```

```rust
engine.query("how does X work", QueryOpts { ... }).await?;
```

Return existing `QueryResponse` (already has `cache_status`, `took_ms`, `miss_reason`). Do not add a parallel status field.

`skip_cache=True`: skip lookup, execute upstream, write (refresh).

```python
@mcp.tool()
async def search_docs(query: str, limit: int = 10):
    return await engine.query(query, top_k=limit)
```

**No sync Python `query()` in v1.** A `block_on` under a running loop deadlocks.

**GIL:** PyO3 must `allow_threads` / `pyo3_async_runtimes` around the entire Rust `execute_query`. Hash, papaya, ONNX, HTTP, coalesce, circuit run with the GIL released. GIL is only held to convert args in and `QueryResponse` out.

**Auth:** `execute_query(..., agent=None)`. Ignore toml `[[agents]]` / process `api_key` / inbound rate-limit-as-auth. Still honor circuit, coalesce, retry, pool.

### 5.3 `clear` / `stats` / `close`

- `clear()` — same as daemon `cache_clear`: wipe the store. Not per-context in v1.
- `stats()` — existing daemon counters (hits, misses, size, evictions).
- `close()` — stop refresh worker, flush redb, drop persist handle. `Drop` / `async with` also close.

### 5.4 Precedence

1. Per-call `query()` kwargs
2. `Engine(...)` defaults
3. `conproxy.toml`
4. Builtins

## 6. Contexts

Default **`"global"`**. Daemon stays **`"default"`**. Engine **creates** `"global"` at build (ContextManager does not have it by default).

Shared redb with a daemon → pass `context="default"` or you will not see those hits.

## 7. Embedders

| `embedder` | Behavior | Feature |
|---|---|---|
| `None` | Exact only. `semantic=True` ignored. | always |
| `"api"` | embed-api from `config` | `embed-api` |
| `"onnx"` | `ModelManager` path or download. Not in the wheel. | `embed` |

Missing feature → construct error.

## 8. Persistence

Optional. Memory-only if `persist_path` is unset.

- Feature `persistence` / extra `conproxy[persist]`.
- Open: `restore_from_persistence`. Insert: write-through.
- Live `get()` does not promote from disk.
- **Lock:** do not invent flock. Surface **redb’s native single-writer** error if a second `Engine` or daemon opens the same file.
- Eviction may leave disk rows (`tracked_remove` today). Unchanged.

## 9. Errors

| Situation | Behavior |
|---|---|
| No `config` / no upstreams | Construct or `query` error |
| persist/onnx/api without feature | Construct error, names extra |
| Second opener of `persist_path` | Construct error (redb single-writer) |
| Invalid `max_memory` string | Construct error |
| Sync `query()` | Does not exist in v1 |
| `skip_cache=True` | Refresh; not an error |
| `fork()` after construct | Undefined; documented |

## 10. Packaging

No new Rust meta-feature. Existing: `embed-api`, `embed`, `persistence`, `pgvector`. `release` unchanged.

| Python extra | Meaning |
|---|---|
| (none) | `ConproxyClient` + `Engine` (full query core — **not a thin wheel**) |
| `[onnx]` | Documents ONNX; wrong wheel → construct error |
| `[persist]` | Same for redb |
| `langchain` / `llama-index` | Unchanged |

Sync `Engine()` owns a tokio RT. `await query()` bridges asyncio → that RT. Prefer one RT if `ConproxyClient` is also in-process.

## 11. Testing

| Change | Must run |
|---|---|
| `src/engine/**` | `cargo test --lib` |
| semantic | `cargo test --features "embed-api" --lib` |
| persist + second open | `cargo test --features persistence --lib` |
| Python | `make sdk-smoke` / `cargo test --test e2e_sdk_python` |
| clippy | touched feature surfaces |

**Rust:** `execute_query` hit / miss / stale / frozen / `skip_cache`; `"global"` created and isolated from `"default"`; refresh worker running (stale refresh path); `semantic=False`; knobs applied via setters; second persist opener fails; `clear` wipes all; `stats`; no config errors; construct errors without features; `peer.start` not invoked.

**Python:** `await Engine.create` and `Engine()` both work; `await query` result shape + `cache_status`; kwargs stick; GIL released (at least: other asyncio tasks progress during a slow miss — documented test if feasible); `async with` / `close`; no sync `query`.

Do not pin test counts.

## 12. Docs and examples

- README: Engine is first-class; per-process cache; daemon for shared/multi-agent; wheel is not thin.
- `docs/engine.md`: both constructors, async `query`, `"global"`, knobs, redb single-writer, GIL, extras. S3-FIFO = eviction. redb ≠ read-through.
- `docs/sdk-python.md`: `Engine` next to `ConproxyClient`.
- Examples: one Rust `rmcp` tool and one Python FastMCP-style tool, `await engine.query` only.

## 13. Rollout

1. Rust `Engine` around `AppState` + `execute_query` + refresh worker + `"global"` + unit tests.
2. Persist (redb error) + embedder + kwargs setters.
3. `clear` / `stats` / `close`.
4. Python: sync `Engine()` + `create()` + async `query` + GIL release + sdk-smoke.
5. Examples + docs.

Daemon unchanged.

## 14. Decisions

| Decision | Choice |
|---|---|
| Implementation | Wrap `AppState` + `execute_query`; start refresh worker; no `peer.start()` |
| Parity | Query core only |
| Tune / peer | Out |
| Tool cache | Out |
| Cache key | Query string only |
| Default context | `"global"` — **create it** on build |
| `cache_status` | Existing enum including `Frozen` |
| Persist lock | redb native single-writer, not a new flock |
| `clear()` | Daemon `cache_clear` (whole store) |
| Auth | `agent=None`; ignore toml agents/api_key |
| Python | Asyncio `query`; both `Engine()` and `await Engine.create()` |
| GIL | Released for all of `execute_query` |
| Eviction | Stock S3-FIFO |
