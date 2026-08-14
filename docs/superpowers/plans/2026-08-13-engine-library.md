# In-process Engine Library Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-subagent-driven-development (recommended) or superpowers-executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `conproxy::engine::Engine` (Rust) and `from conproxy import Engine` (Python) as a thin facade over existing `CacheProxy` → `AppState` + `query_core::execute_query`, with a refresh worker and no listen/peer.

**Architecture:** Extract `CacheProxy::into_app_state(cancel)` from `run()` so Engine and the daemon share one AppState constructor. Engine applies library overrides (`"global"` context, persist, knobs), starts the refresh worker, never calls `PeerManager::start`. Python wraps the same type; `query` is asyncio-only with the GIL released around `execute_query`.

**Tech Stack:** Rust 2021, existing `AppState` / `execute_query` / `CacheStore` / redb, PyO3 0.29 + `pyo3-async-runtimes` + tokio.

**Spec:** `docs/superpowers/specs/2026-08-13-engine-library-design.md`

---

## File map

| File | Role |
|---|---|
| Create `src/engine/mod.rs` | Public `Engine`, `EngineBuilder`, `QueryOpts`, `EngineError`, `EngineStats` |
| Create `src/engine/tests.rs` | Unit tests (included from mod.rs) |
| Modify `src/lib.rs` | `pub mod engine` + re-exports |
| Modify `src/proxy/server/mod.rs` | Extract `CacheProxy::into_app_state`; `run()` calls it |
| Modify `src/proxy/server/query_core.rs` | Add `skip_cache: bool` to `execute_query` |
| Modify `src/proxy/server/query.rs` | Pass `skip_cache=false` |
| Modify `src/proxy/types.rs` | `CachedResponse::into_query_response` |
| Modify `src/proxy/server/tests/mod_tests.rs` | Update existing `execute_query(` call sites with new arg |
| Create `sdk/python/src/engine.rs` | `PyEngine` |
| Modify `sdk/python/src/lib.rs` | Register `Engine` |
| Modify `sdk/python/src/types.rs` | Engine-facing `PyQueryResponse` from crate types (or convert) |
| Modify `sdk/python/Cargo.toml` | Depend on `conproxy` crate |
| Modify `sdk/python/pyproject.toml` | extras `[onnx]`, `[persist]` |
| Create `docs/engine.md` | User docs |
| Modify `README.md`, `docs/sdk-python.md` | Point at Engine |
| Create `examples/engine-rmcp.rs` (or `examples/engine_tool.rs`) | Rust example |
| Create `sdk/python/examples/engine_fastmcp.py` | Python example |

Do **not** invent flock, LFU-main, `@cached`, listen, or a second cache stack.

---

### Task 1: `CachedResponse::into_query_response`

**Files:**
- Modify: `src/proxy/types.rs` (`impl CachedResponse`)
- Test: add to existing `src/proxy/tests/types_tests.rs` (or nearest `mod tests` in types.rs)

- [x] **Step 1: Write the failing test**

Add to the types test module:

```rust
#[test]
fn cached_response_into_query_response_preserves_status() {
    use crate::proxy::types::{CacheStatus, CachedResponse, QueryResponse};

    let fresh = CachedResponse::Fresh(QueryResponse {
        results: vec![],
        cache_status: CacheStatus::Miss,
        took_ms: 7,
        generated_at: None,
        miss_reason: None,
    });
    let out = fresh.into_query_response();
    assert_eq!(out.cache_status, CacheStatus::Miss);
    assert_eq!(out.took_ms, 7);
}
```

- [x] **Step 2: Run test to verify it fails**

Run: `cargo test --lib cached_response_into_query_response -- --nocapture`

Expected: FAIL — `into_query_response` not found.

- [x] **Step 3: Implement**

In `src/proxy/types.rs` on `impl CachedResponse`, after `from_cache`:

```rust
    /// Materialize a owned `QueryResponse` (Engine / non-HTTP callers).
    pub fn into_query_response(self) -> QueryResponse {
        match self {
            Self::Cached {
                entry,
                cache_status,
                took_ms,
            } => QueryResponse {
                results: entry.response.results.clone(),
                cache_status,
                took_ms,
                generated_at: entry.response.generated_at,
                miss_reason: None,
            },
            Self::Fresh(response) => response,
            Self::SharedFresh { response, took_ms } => {
                let mut owned = (*response).clone();
                owned.took_ms = took_ms;
                owned
            }
        }
    }
```

- [x] **Step 4: Run test to verify it passes**

Run: `cargo test --lib cached_response_into_query_response`

Expected: PASS

---

### Task 2: Extract `CacheProxy::into_app_state`

**Files:**
- Modify: `src/proxy/server/mod.rs` (`impl CacheProxy`, around `run()` ~1090–1160)
- Test: `src/engine/tests.rs` will cover this in Task 4; here add a focused test next to existing server tests **or** wait for Task 4. Prefer a small test in `src/proxy/server/tests/mod_tests.rs`.

`run()` currently builds `AppState { ... peer_manager: if cdc+peer_cfg { PeerManager::new } ...}` then binds sockets and later starts the peer. Extract the struct build.

- [x] **Step 1: Add `into_app_state` and make `run()` call it**

Add on `impl CacheProxy` (pub(crate) is enough; Engine is in the same crate):

```rust
    /// Build runtime `AppState` without binding sockets.
    ///
    /// Starts a `QueryTrackingRefreshWorker` when a single upstream exists.
    /// Does **not** construct `PeerManager` — Engine must not start peer.
    /// Daemon `run()` constructs peer itself after this returns.
    pub(crate) fn into_app_state(self, cancel: CancellationToken) -> AppState {
        let refresh_worker = self.upstream.as_ref().map(|upstream| {
            Arc::new(QueryTrackingRefreshWorker::new(
                self.cache.clone(),
                upstream.clone(),
                self.upstream_id.clone(),
                self.refresh_interval,
                cancel,
            ))
        });

        let query_stats = Arc::new(QueryStatsTracker::new(10000));
        let batch_processor = Arc::new(BatchProcessor::new(BatchConfig::default()));
        let global_concurrency = Arc::new(tokio::sync::Semaphore::new(self.max_global_connections));

        AppState {
            cache: self.cache.clone(),
            upstream: Arc::new(ArcSwapOption::new(self.upstream.clone())),
            upstream_pool: Arc::new(ArcSwapOption::new(self.upstream_pool.clone())),
            coalescer: self.coalescer.clone(),
            refresh_worker,
            scope_filter: self.scope_filter.clone(),
            metrics: self.metrics.clone(),
            circuit_breaker: self.circuit_breaker.clone(),
            audit_log: self.audit_log.clone(),
            retry_policy: Arc::new(self.retry_policy.clone()),
            adaptive_timeout: self.adaptive_timeout.clone(),
            query_stats,
            batch_processor,
            federated_search: Arc::new(ArcSwap::new(self.federated_search.clone())),
            request_queue: self.request_queue.clone(),
            upstream_id: self.upstream_id.clone(),
            start_time: self.start_time,
            context_manager: self.context_manager.clone(),
            #[cfg(feature = "embed-api")]
            smart_embedder: self.smart_embedder.clone(),
            #[cfg(feature = "embed-api")]
            semantic_cache: self.semantic_cache.clone(),
            degradation_level: Arc::new(std::sync::atomic::AtomicU8::new(0)),
            paused: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            client_tracker: Arc::new(ClientTracker::new()),
            cascade_executor: Arc::new(ArcSwapOption::new(self.cascade_executor.clone())),
            agent_registry: Arc::new(ArcSwapOption::new(self.agent_registry.clone())),
            cdc_manager: self.cdc_manager.clone(),
            global_concurrency,
            reload_source: self.reload_source.clone(),
            peer_manager: None,
            tokio_handle: tokio::runtime::Handle::try_current().ok(),
        }
    }
```

Then change `run()` to:

1. Keep `peer_config` / `cdc_manager` clones **before** `into_app_state` if needed.
2. Call `let mut state = self.into_app_state(cancel.clone());`
3. If daemon still needs peer: construct `PeerManager` here and assign `state.peer_manager = Some(...)`.

`AppState` is `Clone` (all Arcs). If `run()` needs `self` fields after the move, clone what peer construction needs first (`cache`, `cdc_manager`, `peer_config`).

`into_app_state` **always** sets `peer_manager: None`. Daemon `run()` attaches peer after. Engine never attaches peer.

- [x] **Step 2: Compile**

Run: `cargo test --lib --no-run`

Expected: compiles. Existing `run()` tests still exist.

- [x] **Step 3: Add a unit test that into_app_state has no peer**

In `src/proxy/server/tests/mod_tests.rs`:

```rust
#[test]
fn into_app_state_does_not_start_peer() {
    let cfg = crate::config::ProxyConfig::default();
    let proxy = crate::proxy::server::CacheProxy::new(&cfg).expect("proxy");
    let cancel = tokio_util::sync::CancellationToken::new();
    let state = proxy.into_app_state(cancel);
    assert!(state.peer_manager.is_none());
    assert!(state.context_manager.get("default").is_some());
}
```

If `CacheProxy` is not `pub` enough from tests, the test lives in `src/engine/tests.rs` after Task 4. Do not fight visibility — Engine tests are the real gate.

Run: `cargo test --lib into_app_state_does_not_start_peer`

---

### Task 3: `skip_cache` on `execute_query`

**Files:**
- Modify: `src/proxy/server/query_core.rs` (`execute_query` signature + cache-check block ~line 174 and ~241)
- Modify: `src/proxy/server/query.rs` (the one `execute_query(` call)
- Modify: every `execute_query(` in `src/proxy/server/tests/mod_tests.rs` and any gRPC handler

- [x] **Step 1: Find every call site**

Run: `rg -n "execute_query\(" src --glob '*.rs'`

Every site must pass the new last-but-compatible arg. Add `skip_cache: bool` **after** `source` (or before — pick one and use it everywhere):

```rust
pub(crate) async fn execute_query(
    state: &AppState,
    request: QueryRequest,
    context_id: String,
    request_id: String,
    agent: Option<&AgentIdentity>,
    source: String,
    skip_cache: bool,
) -> QueryResult {
```

HTTP / gRPC pass `false`.

- [x] **Step 2: Skip the freshness lookup when `skip_cache`**

Change:

```rust
    // Check cache first
    if let Some(freshness) = state.cache.check_freshness_by_hash(&query_hash) {
```

to:

```rust
    // Check cache first (Engine skip_cache=true forces refresh + write)
    if !skip_cache {
        if let Some(freshness) = state.cache.check_freshness_by_hash(&query_hash) {
            // existing match Fresh / Stale / Expired / Frozen unchanged
        }
    }
```

Close the extra brace after the existing cache-check `if let`. Do not skip the later insert-on-miss path.

- [x] **Step 3: Update HTTP**

`src/proxy/server/query.rs`:

```rust
    let result = super::query_core::execute_query(
        &state,
        request,
        ctx_id,
        request_id,
        agent_identity.as_deref(),
        source,
        false,
    )
    .await;
```

Same for gRPC if it calls `execute_query`.

- [x] **Step 4: Update tests — add `, false` to every existing call**

Do not change assertions. Then add one new test (can live in engine tests Task 5) that `skip_cache=true` after a hit still goes upstream.

- [x] **Step 5: Compile + existing query_core tests**

Run: `cargo test --lib execute_query`

Expected: all existing execute_query tests PASS.

---

### Task 4: Rust `Engine` construct + `"global"` context

**Files:**
- Create: `src/engine/mod.rs`
- Create: `src/engine/tests.rs`
- Modify: `src/lib.rs`

- [x] **Step 1: Write failing tests first** (`src/engine/tests.rs`)

```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use std::time::Duration;

fn minimal_toml(dir: &std::path::Path) -> std::path::PathBuf {
    let p = dir.join("conproxy.toml");
    std::fs::write(
        &p,
        r#"
[proxy]
upstream_url = "http://127.0.0.1:9"
"#,
    )
    .unwrap();
    p
}

#[tokio::test]
async fn engine_creates_global_context() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::builder()
        .config_toml(minimal_toml(dir.path()))
        .build()
        .unwrap();
    assert_eq!(engine.context(), "global");
    assert!(engine.context_exists("global"));
}

#[tokio::test]
async fn engine_does_not_start_peer() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::builder()
        .config_toml(minimal_toml(dir.path()))
        .build()
        .unwrap();
    assert!(!engine.peer_started());
}

#[test]
fn engine_missing_config_errors() {
    let err = Engine::builder()
        .config_toml("/no/such/conproxy.toml")
        .build()
        .unwrap_err();
    assert!(matches!(err, EngineError::Config(_)));
}
```

If `tempfile` is not a crate dep, use `std::env::temp_dir()` + a unique name, or the repo's existing temp-dir helper. Check `Cargo.toml` `[dev-dependencies]` first — this repo already uses `tempfile` in persistence tests.

- [x] **Step 2: Run tests — expect FAIL**

Run: `cargo test --lib engine::`

Expected: FAIL — `engine` module missing.

- [x] **Step 3: Implement `src/engine/mod.rs`**

```rust
//! In-process query-core facade. Same path as the daemon HTTP handler.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::config::{Config, ProxyConfig};
use crate::proxy::server::query_core::execute_query;
use crate::proxy::server::{CacheProxy, AppState};
use crate::proxy::types::{QueryRequest, QueryResponse};

const DEFAULT_CONTEXT: &str = "global";
const DEFAULT_TTL_SECS: u64 = 300;
const DEFAULT_STALE_TTL_SECS: u64 = 3600;

#[derive(Debug)]
pub enum EngineError {
    Config(String),
    Persist(String),
    Feature(&'static str),
    Query(String),
    Closed,
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(m) => write!(f, "config: {m}"),
            Self::Persist(m) => write!(f, "persist: {m}"),
            Self::Feature(name) => write!(
                f,
                "feature `{name}` not enabled in this build (install the matching extra / cargo feature)"
            ),
            Self::Query(m) => write!(f, "query: {m}"),
            Self::Closed => write!(f, "engine is closed"),
        }
    }
}

impl std::error::Error for EngineError {}

#[derive(Debug, Clone, Default)]
pub struct QueryOpts {
    pub top_k: Option<usize>,
    pub context: Option<String>,
    pub ttl: Option<Duration>,
    pub semantic: Option<bool>,
    pub threshold: Option<f32>,
    pub skip_cache: bool,
}

#[derive(Debug, Clone)]
pub struct EngineStats {
    pub hits: u64,
    pub misses: u64,
    pub size: usize,
    pub evictions: u64,
}

#[derive(Clone)]
pub struct Engine {
    inner: Arc<EngineInner>,
}

struct EngineInner {
    state: AppState,
    cancel: CancellationToken,
    context: String,
    semantic: bool,
    threshold: Option<f32>,
    ttl: Duration,
}

pub struct EngineBuilder {
    config_toml: Option<PathBuf>,
    persist_path: Option<PathBuf>,
    ttl: Option<Duration>,
    stale_ttl: Option<Duration>,
    context: Option<String>,
    semantic: Option<bool>,
    threshold: Option<f32>,
    max_entries: Option<usize>,
    max_memory: Option<u64>,
}

impl Engine {
    pub fn builder() -> EngineBuilder {
        EngineBuilder {
            config_toml: None,
            persist_path: None,
            ttl: None,
            stale_ttl: None,
            context: None,
            semantic: None,
            threshold: None,
            max_entries: None,
            max_memory: None,
        }
    }

    pub fn context(&self) -> &str {
        &self.inner.context
    }

    pub(crate) fn context_exists(&self, id: &str) -> bool {
        self.inner.state.context_manager.get(id).is_some()
    }

    pub(crate) fn peer_started(&self) -> bool {
        self.inner.state.peer_manager.is_some()
    }

    pub async fn query(&self, q: &str, opts: QueryOpts) -> Result<QueryResponse, EngineError> {
        if self.inner.cancel.is_cancelled() {
            return Err(EngineError::Closed);
        }
        let context = opts
            .context
            .as_deref()
            .unwrap_or(self.inner.context.as_str())
            .to_string();
        let request = QueryRequest {
            query: q.to_string(),
            top_k: opts.top_k,
            priority: None,
            upstream_id: None,
            upstream_type: None,
        };
        let request_id = format!("eng-{}", uuid_or_counter());
        let result = execute_query(
            &self.inner.state,
            request,
            context,
            request_id,
            None, // ignore toml agents / api_key
            "engine".to_string(),
            opts.skip_cache,
        )
        .await;
        if result.status != 200 {
            return Err(EngineError::Query(format!("status {}", result.status)));
        }
        Ok(result.response.into_query_response())
    }

    pub fn clear(&self) {
        self.inner.state.cache.clear();
    }

    pub fn stats(&self) -> EngineStats {
        let s = self.inner.state.cache.stats();
        EngineStats {
            hits: s.hits,
            misses: s.misses,
            size: s.size,
            evictions: s.evictions,
        }
    }

    pub fn close(&self) {
        self.inner.cancel.cancel();
    }
}

impl Drop for EngineInner {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl EngineBuilder {
    pub fn config_toml(mut self, path: impl AsRef<Path>) -> Self {
        self.config_toml = Some(path.as_ref().to_path_buf());
        self
    }

    pub fn persist_path(mut self, path: impl AsRef<Path>) -> Self {
        self.persist_path = Some(path.as_ref().to_path_buf());
        self
    }

    pub fn ttl(mut self, d: Duration) -> Self {
        self.ttl = Some(d);
        self
    }

    pub fn stale_ttl(mut self, d: Duration) -> Self {
        self.stale_ttl = Some(d);
        self
    }

    pub fn context(mut self, id: impl Into<String>) -> Self {
        self.context = Some(id.into());
        self
    }

    pub fn semantic(mut self, on: bool) -> Self {
        self.semantic = Some(on);
        self
    }

    pub fn threshold(mut self, t: Option<f32>) -> Self {
        self.threshold = t;
        self
    }

    pub fn max_entries(mut self, n: usize) -> Self {
        self.max_entries = Some(n);
        self
    }

    pub fn max_memory(mut self, bytes: Option<u64>) -> Self {
        self.max_memory = bytes;
        self
    }

    /// Parse `"256MiB"` / `"1GiB"` / integer bytes. Used by Python.
    pub fn max_memory_parsed(mut self, raw: &str) -> Result<Self, EngineError> {
        self.max_memory = Some(parse_memory(raw)?);
        Ok(self)
    }

    pub fn build(self) -> Result<Engine, EngineError> {
        let path = self
            .config_toml
            .as_ref()
            .ok_or_else(|| EngineError::Config("config_toml is required".into()))?;
        let loaded = Config::load_from(path.to_str().ok_or_else(|| {
            EngineError::Config(format!("config path is not utf-8: {}", path.display()))
        })?)
        .map_err(|e| EngineError::Config(e.to_string()))?;
        let proxy_cfg: ProxyConfig = loaded
            .config
            .effective_proxy()
            .map_err(|e| EngineError::Config(e.to_string()))?;

        let mut proxy = CacheProxy::new(&proxy_cfg)
            .map_err(|e| EngineError::Config(e.to_string()))?;

        // persist
        if let Some(ref persist) = self.persist_path {
            #[cfg(feature = "persistence")]
            {
                use crate::proxy::persistence::PersistentCache;
                use std::sync::Arc as StdArc;
                let pc = PersistentCache::open(persist)
                    .map_err(|e| EngineError::Persist(e.to_string()))?;
                // CacheStore::set_persistence takes &mut — CacheProxy.cache is Arc.
                // Use existing CacheProxy / CacheStore API: if only `&self` insert
                // path exists, add a `CacheProxy::attach_persistence` helper that
                // reaches the inner store. See Task 6 if `cache` is private.
                proxy.attach_persistence(StdArc::new(pc))?;
            }
            #[cfg(not(feature = "persistence"))]
            {
                return Err(EngineError::Feature("persistence"));
            }
        }

        if let Some(n) = self.max_entries {
            proxy
                .cache()
                .set_max_entries(n)
                .map_err(EngineError::Config)?;
        }
        if let Some(bytes) = self.max_memory {
            proxy.cache_mut().set_max_memory_bytes(bytes as usize);
        }
        let ttl = self.ttl.unwrap_or(Duration::from_secs(DEFAULT_TTL_SECS));
        let stale = self
            .stale_ttl
            .unwrap_or(Duration::from_secs(DEFAULT_STALE_TTL_SECS));
        proxy.cache().set_fresh_duration(ttl);
        proxy.cache().set_stale_duration(stale);

        let context = self
            .context
            .unwrap_or_else(|| DEFAULT_CONTEXT.to_string());
        // ContextManager starts with "default". Create + switch to "global".
        let _ = proxy.context_manager().create(&context, "", "");
        proxy
            .context_manager()
            .switch(&context)
            .map_err(|e| EngineError::Config(e.to_string()))?;

        let cancel = CancellationToken::new();
        let state = proxy.into_app_state(cancel.clone());

        Ok(Engine {
            inner: Arc::new(EngineInner {
                state,
                cancel,
                context,
                semantic: self.semantic.unwrap_or(true),
                threshold: self.threshold,
                ttl,
            }),
        })
    }
}

fn parse_memory(raw: &str) -> Result<u64, EngineError> {
    let s = raw.trim();
    if let Ok(n) = s.parse::<u64>() {
        return Ok(n);
    }
    let lower = s.to_ascii_lowercase();
    let (num, mul) = if let Some(n) = lower.strip_suffix("gib") {
        (n, 1024u64 * 1024 * 1024)
    } else if let Some(n) = lower.strip_suffix("mib") {
        (n, 1024 * 1024)
    } else if let Some(n) = lower.strip_suffix("kib") {
        (n, 1024)
    } else {
        return Err(EngineError::Config(format!("invalid max_memory: {raw}")));
    };
    let n: u64 = num
        .trim()
        .parse()
        .map_err(|_| EngineError::Config(format!("invalid max_memory: {raw}")))?;
    Ok(n.saturating_mul(mul))
}

fn uuid_or_counter() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1);
    N.fetch_add(1, Ordering::Relaxed)
}
```

`CacheProxy.cache` / `context_manager` are **private**. Add the smallest accessors on `impl CacheProxy`:

```rust
    pub(crate) fn cache(&self) -> &CacheStore {
        &self.cache
    }

    pub(crate) fn cache_arc(&self) -> Arc<CacheStore> {
        self.cache.clone()
    }

    pub(crate) fn context_manager(&self) -> &crate::proxy::context::ContextManager {
        &self.context_manager
    }

    #[cfg(feature = "persistence")]
    pub(crate) fn attach_persistence(
        &mut self,
        persistence: Arc<crate::proxy::persistence::PersistentCache>,
    ) -> Result<(), EngineError> {
        // CacheStore::set_persistence is &mut self. cache is Arc<CacheStore>.
        // If set_persistence cannot run through Arc, add CacheStore::set_persistence_arc
        // that uses interior mutability already on the store, OR
        // Arc::get_mut(&mut self.cache) before any clone.
        let store = Arc::get_mut(&mut self.cache).ok_or_else(|| {
            EngineError::Persist("cache already shared; cannot attach persistence".into())
        })?;
        store.set_persistence(persistence);
        let _ = store.restore_from_persistence();
        Ok(())
    }
```

`set_max_memory_bytes` is `&mut self` — same `Arc::get_mut` pattern. If that is too brittle, add `&self` setters on `CacheStore` using existing interior mutability (check `cache.rs` — `set_max_entries` is already `&self`). Prefer promoting `set_max_memory_bytes` / `set_fresh_duration` to `&self` **only if** the fields are already behind a lock/atomic. Do not add a new Mutex if one exists.

- [x] **Step 4: Export**

`src/lib.rs`:

```rust
pub mod engine;
pub use engine::{Engine, EngineBuilder, EngineError, EngineStats, QueryOpts};
```

`src/engine/mod.rs` footer:

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
```

- [x] **Step 5: Run tests**

Run: `cargo test --lib engine::`

Expected: PASS (`engine_creates_global_context`, `engine_does_not_start_peer`, `engine_missing_config_errors`).

---

### Task 5: Engine `query` hit / miss / skip_cache / isolation

**Files:**
- Modify: `src/engine/tests.rs`
- Wiretap: `src/proxy/server/tests/mod_tests.rs` `make_test_app_state_with_upstream` if Engine tests need a live HTTP upstream. Prefer a `wiremock` / existing test adapter.

Look at how `test_execute_query_upstream_success_refresh_worker` builds an upstream (around `src/proxy/server/tests/mod_tests.rs:5215`). Reuse that pattern: start a tiny axum/httptest upstream **or** insert directly into the cache then `query` for hits.

Minimum tests:

```rust
#[tokio::test]
async fn query_miss_then_hit_without_upstream_via_insert() {
    // If Engine has no test hook to insert, use skip_cache against a mock
    // upstream. Otherwise: build engine, insert via cache test hook, query.
}

#[tokio::test]
async fn global_context_isolated_from_default() {
    // insert under "default", query with default context "global" → miss
}

#[tokio::test]
async fn skip_cache_refreshes() {
    // hit, then skip_cache=true → status Miss (or new upstream body)
}

#[tokio::test]
async fn clear_wipes_store() {
    // insert, clear, query → miss
}

#[tokio::test]
async fn stats_counters_move() {
    // miss then hit → hits>=1, misses>=1
}
```

If a mock upstream is required, copy the helper from `make_test_app_state_with_upstream` / existing wiremock in `mod_tests.rs`. Do not stand up Docker.

For isolation: `execute_query` keys as `context_query(&context_id, &request.query)`. Insert via `cache.insert_with_context(query, resp, "test", "default")`, then `engine.query(query, QueryOpts::default())` must miss.

- [x] **Step 1: Write failing tests**
- [x] **Step 2: Run — FAIL**
- [x] **Step 3: Add any missing Engine hooks (`pub(crate)` cache insert for tests only if needed)**
- [x] **Step 4: PASS** `cargo test --lib engine::`

---

### Task 6: Persist path + second opener + knobs

**Files:**
- Modify: `src/engine/mod.rs` (`attach_persistence`)
- Modify: `src/engine/tests.rs`
- Feature: `persistence`

- [x] **Step 1: Tests**

```rust
#[cfg(feature = "persistence")]
#[test]
fn persist_second_opener_errors() {
    let dir = tempfile::tempdir().unwrap();
    let toml = minimal_toml(dir.path());
    let db = dir.path().join("cache.redb");
    let _a = Engine::builder()
        .config_toml(&toml)
        .persist_path(&db)
        .build()
        .unwrap();
    let err = Engine::builder()
        .config_toml(&toml)
        .persist_path(&db)
        .build()
        .unwrap_err();
    assert!(matches!(err, EngineError::Persist(_)));
}

#[test]
fn persist_without_feature_errors() {
    // Only compile this assertion on not(feature = "persistence")
}

#[test]
fn max_memory_parse_mib() {
    assert_eq!(super::parse_memory("256MiB").unwrap(), 256 * 1024 * 1024);
}

#[test]
fn invalid_max_memory_errors() {
    assert!(super::parse_memory("nope").is_err());
}
```

Make `parse_memory` `pub(crate)` so tests can call it.

- [x] **Step 2: FAIL then implement attach (Task 4 stub) + PASS**

Run: `cargo test --features persistence --lib persist_second_opener`

Expected: PASS. redb native lock — do not add flock.

---

### Task 6b: Dynamic memory cap (added post-plan, per user)

**Decision (approved):** default `memory_fraction=0.7`; `max_memory` (fixed) wins if both passed; live resize via 10s ticker, >5% change threshold, 16 MiB floor; shrink runs S3-FIFO evict immediately.

- [x] `CacheStore::max_memory_bytes` → `AtomicUsize` + `&self` setter + getter + `enforce_memory_limit()` (shrink eviction)
- [x] `src/engine/budget.rs` — cgroup v2 `memory.max` walk-up → `MemAvailable` → 256 MiB fallback; `cap_for_fraction` clamped; floor
- [x] `EngineBuilder::memory_fraction(f64)` + dynamic default in `build()`; `spawn_memory_ticker()` (Python path) + `memory_ticker` task on shared cancel token
- [x] Python `memory_fraction=` kwarg on `Engine()` / `Engine.create()`; `wrap_engine` spawns ticker
- [x] Tests: fixed cap, 70% default, fraction override, fraction-vs-max precedence, shrink evicts, floor
- [x] Docs: `docs/engine.md` memory-cap section + spec §cache pressures / builder
- [x] Gate: clippy default + persistence + embed-api + python; `cargo test --lib` (1773), persistence engine (23)

### Task 6c: Read-only HTTP dashboard (added post-plan, per user)

**Decision (approved):** opt-in `dashboard_listen` (default off), option **B** — dashboard SPA + the read GETs it needs only; no `/query`, no admin/cache mutations, no auth. Loopback unless a wider address is passed. Daemon REST router unchanged.

- [x] `src/proxy/server/mod.rs`: extract `pub(crate) fn dashboard_http_router() -> Router<AppState>` — public status, observability GETs, cache reads, context reads, `/dashboard` SPA
- [x] `EngineBuilder::dashboard_listen(addr)` + bind in `build()` (std listener, fail-fast on conflict, `:0` supported); `EngineDashboard { listener, addr }` on `EngineInner`
- [x] `Engine::spawn_dashboard()` (idempotent via `dashboard_spawned` AtomicBool; no-op without a current runtime), `dashboard_configured()`, `dashboard_addr()`; `close()` shuts it down via cancel token
- [x] Python: `dashboard_listen=` kwarg on `Engine()` / `Engine.create()`; `wrap_engine` calls `inner.spawn_dashboard()`; `Engine.dashboard_addr()` method
- [x] Tests: off by default; `:0` serves `/health` `/dashboard` `/stats` `/contexts` 200; `POST /query` + `POST /cache/clear` 404; `close()` stops serving; listen conflict errors
- [x] Docs: `docs/engine.md` Dashboard section; spec lock superseded (parity cut + build steps)
- [x] Gate: clippy default + persistence + embed-api + python; `cargo test --lib` (1778), persistence (1823), embed-api (1832)

---

### Task 7: Python `Engine` + async `query` + GIL release
**Files:**
- Create: `sdk/python/src/engine.rs`
- Modify: `sdk/python/src/lib.rs`
- Modify: `sdk/python/Cargo.toml` — add `conproxy = { path = "../..", default-features = false }`
- Modify: `sdk/python/pyproject.toml` — extras
- Test: `tests/e2e_sdk_python.rs` or a new `sdk/python` pytest if one exists. Prefer extending `tests/e2e_sdk_python.rs` **only** if it already covers client construct without a live daemon. Otherwise add `src/engine` rust tests as the gate and a small Python snippet under `sdk/python/examples/` run by `make sdk-smoke` if that target exists.

`sdk/python` today depends only on `conproxy-sdk`. Adding `conproxy` makes the wheel **not thin** (spec). Default features off; document extras:

```toml
# sdk/python/Cargo.toml
conproxy = { path = "../..", default-features = false }
```

```toml
# pyproject.toml
[project.optional-dependencies]
onnx = []          # documents ONNX; construct errors if `embed` not compiled in
persist = []       # documents redb; construct errors if `persistence` not compiled in
langchain = ["langchain-core>=0.1,<1.0"]
llama-index = ["llama-index-core>=0.10,<1.0"]
```

- [x] **Step 1: `sdk/python/src/engine.rs`**

```rust
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Runtime;

use conproxy::{Engine as RustEngine, QueryOpts};

use crate::error::to_py_err;
use crate::types::PyQueryResponse;

#[pyclass(name = "Engine")]
pub struct PyEngine {
    inner: Arc<RustEngine>,
    rt: Arc<Runtime>,
}

fn build_rust(
    config: Option<String>,
    persist_path: Option<String>,
    ttl: Option<u64>,
    stale_ttl: Option<u64>,
    context: Option<String>,
    semantic: Option<bool>,
    threshold: Option<f32>,
    max_entries: Option<usize>,
    max_memory: Option<Bound<'_, PyAny>>,
) -> PyResult<RustEngine> {
    let mut b = RustEngine::builder();
    if let Some(c) = config {
        b = b.config_toml(c);
    }
    if let Some(p) = persist_path {
        b = b.persist_path(p);
    }
    if let Some(t) = ttl {
        b = b.ttl(Duration::from_secs(t));
    }
    if let Some(t) = stale_ttl {
        b = b.stale_ttl(Duration::from_secs(t));
    }
    if let Some(c) = context {
        b = b.context(c);
    }
    if let Some(s) = semantic {
        b = b.semantic(s);
    }
    b = b.threshold(threshold);
    if let Some(n) = max_entries {
        b = b.max_entries(n);
    }
    if let Some(mm) = max_memory {
        if let Ok(n) = mm.extract::<u64>() {
            b = b.max_memory(Some(n));
        } else if let Ok(s) = mm.extract::<String>() {
            b = b.max_memory_parsed(&s).map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(e.to_string())
            })?;
        } else {
            return Err(pyo3::exceptions::PyTypeError::new_err(
                "max_memory must be int bytes or a string like '256MiB'",
            ));
        }
    }
    b.build().map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
}

#[pymethods]
impl PyEngine {
    #[new]
    #[pyo3(signature = (config=None, persist_path=None, ttl=None, stale_ttl=None, context=None, semantic=None, threshold=None, max_entries=None, max_memory=None))]
    fn new(
        config: Option<String>,
        persist_path: Option<String>,
        ttl: Option<u64>,
        stale_ttl: Option<u64>,
        context: Option<String>,
        semantic: Option<bool>,
        threshold: Option<f32>,
        max_entries: Option<usize>,
        max_memory: Option<Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        let inner = build_rust(
            config, persist_path, ttl, stale_ttl, context, semantic, threshold, max_entries, max_memory,
        )?;
        let rt = Runtime::new().map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(e.to_string())
        })?;
        Ok(Self {
            inner: Arc::new(inner),
            rt: Arc::new(rt),
        })
    }

    #[classmethod]
    #[pyo3(signature = (config=None, persist_path=None, ttl=None, stale_ttl=None, context=None, semantic=None, threshold=None, max_entries=None, max_memory=None))]
    fn create<'py>(
        _cls: &Bound<'py, pyo3::types::PyType>,
        py: Python<'py>,
        config: Option<String>,
        persist_path: Option<String>,
        ttl: Option<u64>,
        stale_ttl: Option<u64>,
        context: Option<String>,
        semantic: Option<bool>,
        threshold: Option<f32>,
        max_entries: Option<usize>,
        max_memory: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        // Off-loop: same build_rust, then wrap. Use pyo3_async_runtimes to return a coroutine.
        // Convert max_memory to an owned enum before leaving the GIL.
        let mm_owned: Option<MaxMem> = match max_memory {
            None => None,
            Some(v) if v.extract::<u64>().is_ok() => Some(MaxMem::Bytes(v.extract()?)),
            Some(v) => Some(MaxMem::Str(v.extract()?)),
        };
        let fut = async move {
            // build is sync/blocking — run in spawn_blocking
            tokio::task::spawn_blocking(move || {
                // rebuild kwargs from owned values; call build_rust via a non-py path
                unimplemented!("see implementation: owned-kwargs build")
            })
            .await
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?
        };
        pyo3_async_runtimes::tokio::future_into_py(py, fut)
    }

    #[pyo3(signature = (q, top_k=None, context=None, ttl=None, semantic=None, threshold=None, skip_cache=false))]
    fn query<'py>(
        slf: Bound<'py, Self>,
        py: Python<'py>,
        q: String,
        top_k: Option<usize>,
        context: Option<String>,
        ttl: Option<u64>,
        semantic: Option<bool>,
        threshold: Option<f32>,
        skip_cache: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let engine = slf.borrow().inner.clone();
        let rt = slf.borrow().rt.clone();
        let opts = QueryOpts {
            top_k,
            context,
            ttl: ttl.map(Duration::from_secs),
            semantic,
            threshold,
            skip_cache,
        };
        // GIL released for the whole execute_query.
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let engine = engine;
            let opts = opts;
            let q = q;
            let resp = tokio::task::spawn_blocking(move || {
                rt.block_on(async {
                    engine.query(&q, opts).await
                })
            })
            .await
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
            Ok(PyQueryResponse::from_engine(resp))
        })
    }

    fn clear(&self) {
        self.inner.clear();
    }

    fn stats(&self) -> PyResult<PyObject> {
        // return a small dict or reuse PyStatsResponse
        unimplemented!("map EngineStats")
    }

    fn close(&self) {
        self.inner.close();
    }

    fn __aenter__<'py>(slf: Bound<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let slf = slf.unbind();
        pyo3_async_runtimes::tokio::future_into_py(py, async move { Ok(slf) })
    }

    fn __aexit__<'py>(
        slf: Bound<'py, Self>,
        py: Python<'py>,
        _exc_type: Option<Bound<'py, PyAny>>,
        _exc: Option<Bound<'py, PyAny>>,
        _tb: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        slf.borrow().close();
        pyo3_async_runtimes::tokio::future_into_py(py, async move { Ok(false) })
    }
}

enum MaxMem {
    Bytes(u64),
    Str(String),
}
```

`PyQueryResponse` today is `From<conproxy_sdk::proto::QueryResponse>`. Add:

```rust
impl PyQueryResponse {
    pub fn from_engine(r: conproxy::proxy::types::QueryResponse) -> Self {
        Self {
            results: r.results.into_iter().map(|s| PySearchResult {
                id: s.id,
                score: s.score,
                content: s.content,
                metadata_json: s.metadata.map(|m| m.to_string()),
                upstream_id: s.upstream_id.unwrap_or_default(),
            }).collect(),
            cache_status: match r.cache_status {
                conproxy::proxy::types::CacheStatus::Hit => 0,
                conproxy::proxy::types::CacheStatus::Miss => 1,
                conproxy::proxy::types::CacheStatus::Stale => 2,
                conproxy::proxy::types::CacheStatus::Frozen => 3,
            },
            took_ms: r.took_ms,
            generated_at: r.generated_at.unwrap_or(0),
        }
    }
}
```

Check proto enum ints against existing `ConproxyClient` so hit/miss stay consistent. If proto uses different numbers, **match the proto ints**, not 0/1/2/3 invented here.

`types::QueryResponse` / `SearchResult` / `CacheStatus` may not be `pub` from `conproxy::proxy`. If `proxy` module items are public (`src/proxy/mod.rs` already `pub use`s many types), re-export `QueryResponse` + `CacheStatus` + `SearchResult` from `src/lib.rs` or `src/engine/mod.rs` so Python does not reach into `pub(crate)` paths.

- [x] **Step 2: Register**

`sdk/python/src/lib.rs`:

```rust
mod engine;
// ...
m.add_class::<engine::PyEngine>()?;
```

- [x] **Step 3: `make sdk-smoke` or `cargo test --test e2e_sdk_python`**

If sdk-smoke cannot import Engine without maturin rebuild:

```bash
cd sdk/python && maturin develop
python3 -c "from conproxy import Engine; print(Engine)"
```

Expected: class exists. Full query test can stay Rust-side if no upstream.

GIL test (optional, only if feasible without flakiness): start Engine, `await query` against a slow mock, concurrently increment an asyncio counter — counter must move. Skip if no mock; document in test name `#[ignore]`.

---

### Task 8: Docs + examples

**Files:**
- Create: `docs/engine.md`
- Create: `examples/engine_tool.rs` (or a markdown snippet in `docs/engine.md` if a new bin is too heavy — prefer a `[[example]]` only if the workspace already has examples as bins)
- Create: `sdk/python/examples/engine_fastmcp.py`
- Modify: `README.md` — short “in-process Engine” bullet under consider/skip
- Modify: `docs/sdk-python.md` — `Engine` next to `ConproxyClient`

`docs/engine.md` must say:

- Both `Engine()` and `await Engine.create()`; always `await query()`
- Default context `"global"`; daemon is `"default"`
- Wheel is not thin
- redb single-writer (no flock)
- GIL released for `execute_query`
- S3-FIFO is eviction; redb is not live read-through
- Most users should keep the daemon (shared cache, stdio dies)

Python example — FastMCP-style, `await` only:

```python
from conproxy import Engine

engine = Engine(config="conproxy.toml")

async def search_docs(query: str, limit: int = 10):
    return await engine.query(query, top_k=limit)
```

Rust example — `Engine::builder().config_toml(...).build()?` then `engine.query(..., QueryOpts { top_k: Some(10), ..Default::default() }).await?`.

---

### Task 9: Lint + typecheck gate

- [x] **Step 1: fmt**

Run: `cargo fmt -- --check`

- [x] **Step 2: clippy default + embed-api + persistence**

```bash
cargo clippy -- -D warnings
cargo clippy --features "embed-api" --lib -- -D warnings
cargo clippy --features persistence --lib -- -D warnings
```

- [x] **Step 3: tests**

```bash
cargo test --lib
cargo test --features "embed-api" --lib
cargo test --features persistence --lib engine::
```

- [ ] **Step 4: workspace build** (Python cdylib)

Run: `cargo build --workspace`

Fix any proto / feature leakage. Do not enable `embed` in the default Python dep.

---

## Self-review

**Spec coverage**

| Spec | Task |
|---|---|
| Wrap AppState + execute_query | 2, 4 |
| Start refresh worker | 2 (`into_app_state`) |
| No `peer.start()` | 2, 4 test |
| `"global"` created | 4 |
| `cache_status` incl Frozen | 1 + existing enum |
| `skip_cache` | 3, 5 |
| query kwargs | 4 `QueryOpts` |
| `clear` = whole store | 4, 5 |
| `stats` / `close` / Drop | 4 |
| `agent=None` | 4 `execute_query(..., None, ...)` |
| persist redb single-writer | 6 |
| max_entries / max_memory / persist_path | 4, 6 |
| Python both constructors | 7 |
| async query only | 7 |
| GIL released | 7 `spawn_blocking` / allow_threads |
| Docs + examples | 8 |
| Wheel not thin | 7 Cargo.toml + 8 docs |
| Out: listen, tune, peer, @cached, LFU, read-through | not in file map |

**Placeholders:** Task 7 `create()` / `from_engine` proto ints / `stats()` mapping must be filled from live `PyQueryResponse` / proto enums while implementing — do not ship `unimplemented!`.

**Types:** `Engine`, `EngineBuilder`, `EngineError`, `EngineStats`, `QueryOpts`, `skip_cache: bool` on `execute_query`, `into_app_state`, `into_query_response` — used consistently.

---

## Execution

Plan complete. Implement task-by-task in this session (inline). Do not commit unless asked.
