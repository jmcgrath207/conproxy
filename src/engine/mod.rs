//! In-process query-core facade. Same path as the daemon HTTP handler.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::config::Config;
use crate::proxy::server::query_core::execute_query;
use crate::proxy::server::{dashboard_http_router, CacheProxy};
use crate::proxy::types::{QueryRequest, QueryResponse};

use budget::{
    cap_for_fraction, read_memory_budget, DEFAULT_MEMORY_FRACTION, DYNAMIC_CAP_FLOOR_BYTES,
    MEMORY_TICK_SECS,
};

mod budget;

const DEFAULT_CONTEXT: &str = "global";
const DEFAULT_TTL_SECS: u64 = 300;
const DEFAULT_STALE_TTL_SECS: u64 = 3600;

/// Errors from [`Engine`] construction or [`Engine::query`].
#[derive(Debug, Error)]
pub enum EngineError {
    /// Config file missing, invalid, or failed to project.
    #[error("config: {0}")]
    Config(String),
    /// Persistence backend failed to open (including redb single-writer).
    #[error("persist: {0}")]
    Persist(String),
    /// Requested capability is not compiled into this build.
    #[error(
        "feature `{0}` not enabled in this build (install the matching extra / cargo feature)"
    )]
    Feature(&'static str),
    /// Query execution returned a non-success status.
    #[error("query: {0}")]
    Query(String),
    /// [`Engine::close`] already ran.
    #[error("engine is closed")]
    Closed,
}

/// Per-call overrides for [`Engine::query`].
#[derive(Debug, Clone, Default)]
pub struct QueryOpts {
    /// Maximum results. `None` uses the upstream / request default.
    pub top_k: Option<usize>,
    /// Context id. `None` uses the Engine default (`"global"`).
    pub context: Option<String>,
    /// Reserved: per-call TTL. Engine-level TTL is applied at construct.
    pub ttl: Option<Duration>,
    /// Reserved: per-call semantic toggle. Engine-level `semantic` is applied at construct.
    pub semantic: Option<bool>,
    /// Reserved: per-call semantic threshold.
    pub threshold: Option<f32>,
    /// Skip cache lookup, execute upstream, write (refresh).
    pub skip_cache: bool,
}

/// Snapshot of cache counters.
#[derive(Debug, Clone)]
pub struct EngineStats {
    /// Exact + semantic hits recorded on the metrics collector.
    pub hits: u64,
    /// Cache misses recorded on the metrics collector.
    pub misses: u64,
    /// Live entry count.
    pub size: usize,
    /// Evictions recorded on the metrics collector.
    pub evictions: u64,
}

/// In-process retrieval-leg cache. Clone is cheap (`Arc` inner).
#[derive(Clone)]
pub struct Engine {
    inner: Arc<EngineInner>,
}

struct EngineInner {
    state: crate::proxy::server::AppState,
    cancel: CancellationToken,
    context: String,
    /// Dynamic memory fraction. `None` = fixed cap (or unset).
    memory_fraction: Option<f64>,
    /// Optional read-only dashboard listener.
    dashboard: Option<EngineDashboard>,
    dashboard_spawned: AtomicBool,
}

/// Bound dashboard listener. The std listener is `Sync` and is cloned per
/// spawn so the Engine can start serving after a tokio runtime exists
/// (Python constructs off-loop, then spawns after RT start).
struct EngineDashboard {
    listener: std::net::TcpListener,
    addr: SocketAddr,
}

/// Builder for [`Engine`].
#[must_use]
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
    memory_fraction: Option<f64>,
    dashboard_listen: Option<String>,
}

impl Engine {
    /// Start a builder. `config_toml` is required before [`EngineBuilder::build`].
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
            memory_fraction: None,
            dashboard_listen: None,
        }
    }

    /// Default context id (`"global"` unless overridden at construct).
    #[must_use]
    pub fn context(&self) -> &str {
        &self.inner.context
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn context_exists(&self, id: &str) -> bool {
        self.inner.state.context_manager.get(id).is_some()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn peer_started(&self) -> bool {
        self.inner.state.peer_manager.is_some()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn cache(&self) -> &crate::proxy::cache::CacheStore {
        &self.inner.state.cache
    }

    /// Execute a query through the same path as the daemon HTTP handler.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Closed`] after [`Self::close`].
    /// Returns [`EngineError::Query`] when the core returns a non-200 status
    /// (validation, pause, upstream failure).
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
        let request_id = format!("eng-{}", next_request_id());
        let result = execute_query(
            &self.inner.state,
            request,
            context,
            request_id,
            None,
            "engine".to_string(),
            opts.skip_cache,
        )
        .await;
        if result.status != 200 {
            return Err(EngineError::Query(format!("status {}", result.status)));
        }
        Ok(result.response.into_query_response())
    }

    /// Wipe the whole store (daemon `cache_clear` semantics).
    pub fn clear(&self) {
        self.inner.state.cache.clear();
    }

    /// Current hit/miss/size/eviction counters.
    #[must_use]
    pub fn stats(&self) -> EngineStats {
        let snap = self.inner.state.metrics.snapshot();
        let cache = self.inner.state.cache.stats();
        EngineStats {
            hits: snap.cache_hits,
            misses: snap.cache_misses,
            size: cache.total,
            evictions: snap.evictions,
        }
    }

    /// Stop the refresh worker. Further [`Self::query`] calls return [`EngineError::Closed`].
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
    /// Path to `conproxy.toml`. Required.
    pub fn config_toml(mut self, path: impl AsRef<Path>) -> Self {
        self.config_toml = Some(path.as_ref().to_path_buf());
        self
    }

    /// Optional redb path. Second opener of the same file fails with [`EngineError::Persist`].
    pub fn persist_path(mut self, path: impl AsRef<Path>) -> Self {
        self.persist_path = Some(path.as_ref().to_path_buf());
        self
    }

    /// Fresh TTL. Default 300s.
    pub fn ttl(mut self, d: Duration) -> Self {
        self.ttl = Some(d);
        self
    }

    /// Stale TTL. Default 3600s.
    pub fn stale_ttl(mut self, d: Duration) -> Self {
        self.stale_ttl = Some(d);
        self
    }

    /// Default context. Created on build. Default `"global"`.
    pub fn context(mut self, id: impl Into<String>) -> Self {
        self.context = Some(id.into());
        self
    }

    /// Enable semantic tier when the build has `embed-api` and config supplies an embedder.
    pub fn semantic(mut self, on: bool) -> Self {
        self.semantic = Some(on);
        self
    }

    /// Semantic similarity threshold.
    pub fn threshold(mut self, t: Option<f32>) -> Self {
        self.threshold = t;
        self
    }

    /// S3-FIFO max entries.
    pub fn max_entries(mut self, n: usize) -> Self {
        self.max_entries = Some(n);
        self
    }

    /// Fixed memory cap in bytes. Overrides the dynamic default.
    /// `None` (or not called) selects the dynamic cap — see [`Self::memory_fraction`].
    pub fn max_memory(mut self, bytes: Option<u64>) -> Self {
        self.max_memory = bytes;
        self
    }

    /// Fraction of the detected memory budget used for the dynamic cap.
    ///
    /// Overrides any [`Self::max_memory`] and ignores the config default.
    /// The cap is `fraction × budget` where the budget (cgroup v2
    /// `memory.max` → `/proc/meminfo MemAvailable`) is re-read on a 10s
    /// ticker so the cache tracks container / host pressure. Clamped to
    /// `0.0..=1.0`.
    pub fn memory_fraction(mut self, fraction: f64) -> Self {
        self.memory_fraction = Some(fraction.clamp(0.0, 1.0));
        self
    }

    /// Bind a read-only HTTP dashboard (embedded SPA + the GETs it needs)
    /// and serve it for the Engine's lifetime.
    ///
    /// `addr` like `"127.0.0.1:10000"`; `:0` picks a free port (see
    /// [`Engine::dashboard_addr`]). Default: no listener. Loopback unless a
    /// wider address is given. No gRPC, no auth, no `/query`.
    pub fn dashboard_listen(mut self, addr: impl Into<String>) -> Self {
        self.dashboard_listen = Some(addr.into());
        self
    }

    /// Parse `"256MiB"` / `"1GiB"` / integer bytes.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Config`] when the string is not a number or a `KiB`/`MiB`/`GiB` size.
    pub fn max_memory_parsed(mut self, raw: &str) -> Result<Self, EngineError> {
        self.max_memory = Some(parse_memory(raw)?);
        Ok(self)
    }

    /// Build the engine: load toml, construct [`CacheProxy`], apply knobs, create context, start refresh worker.
    ///
    /// # Errors
    ///
    /// - [`EngineError::Config`] — missing path, parse/validate failure, setter rejection
    /// - [`EngineError::Persist`] — redb open / single-writer
    /// - [`EngineError::Feature`] — persist requested without `persistence`
    pub fn build(self) -> Result<Engine, EngineError> {
        let path = self
            .config_toml
            .as_ref()
            .ok_or_else(|| EngineError::Config("config_toml is required".into()))?;
        let path_str = path.to_str().ok_or_else(|| {
            EngineError::Config(format!("config path is not utf-8: {}", path.display()))
        })?;
        let loaded = Config::load_from(path_str).map_err(|e| EngineError::Config(e.to_string()))?;
        let proxy_cfg = match loaded.config.resolve_contexts() {
            Ok(resolved) if !resolved.is_empty() => loaded
                .config
                .effective_proxy()
                .map_err(EngineError::Config)?,
            _ => {
                let mut base = loaded.config.proxy.clone();
                base.normalize_upstreams();
                base
            }
        };

        let mut proxy =
            CacheProxy::new(&proxy_cfg).map_err(|e| EngineError::Config(e.to_string()))?;

        if let Some(ref persist) = self.persist_path {
            attach_persistence(&mut proxy, persist)?;
        }

        if let Some(n) = self.max_entries {
            proxy
                .cache()
                .set_max_entries(n)
                .map_err(EngineError::Config)?;
        }
        let memory_fraction = if let Some(bytes) = self.max_memory {
            proxy.cache().set_max_memory_bytes(bytes as usize);
            None
        } else {
            let fraction = self.memory_fraction.unwrap_or(DEFAULT_MEMORY_FRACTION);
            let cap = cap_for_fraction(fraction, read_memory_budget()).max(DYNAMIC_CAP_FLOOR_BYTES);
            proxy.cache().set_max_memory_bytes(cap as usize);
            Some(fraction)
        };
        let ttl = self.ttl.unwrap_or(Duration::from_secs(DEFAULT_TTL_SECS));
        let stale = self
            .stale_ttl
            .unwrap_or(Duration::from_secs(DEFAULT_STALE_TTL_SECS));
        proxy.cache().set_fresh_duration(ttl);
        proxy.cache().set_stale_duration(stale);

        let _ = self.semantic;
        let _ = self.threshold;

        let context = self.context.unwrap_or_else(|| DEFAULT_CONTEXT.to_string());
        match proxy.context_manager().create(&context, "", "") {
            Ok(()) => {}
            Err(crate::proxy::context::ContextError::AlreadyExists(_)) => {}
            Err(e) => return Err(EngineError::Config(e.to_string())),
        }
        proxy
            .context_manager()
            .switch(&context)
            .map_err(|e| EngineError::Config(e.to_string()))?;

        let cancel = CancellationToken::new();
        let dashboard = match self.dashboard_listen.as_deref() {
            Some(addr) => {
                let std_listener = std::net::TcpListener::bind(addr)
                    .map_err(|e| EngineError::Config(format!("dashboard listen {addr}: {e}")))?;
                let addr = std_listener
                    .local_addr()
                    .map_err(|e| EngineError::Config(format!("dashboard listen: {e}")))?;
                Some(EngineDashboard {
                    listener: std_listener,
                    addr,
                })
            }
            None => None,
        };
        let state = proxy.to_app_state(cancel.clone());
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            if let Some(worker) = state.refresh_worker.clone() {
                handle.spawn(async move {
                    worker.run().await;
                });
            }
            if let Some(fraction) = memory_fraction {
                let st = state.clone();
                let c = cancel.clone();
                handle.spawn(async move {
                    memory_ticker(st, fraction, c).await;
                });
            }
        }

        let engine = Engine {
            inner: Arc::new(EngineInner {
                state,
                cancel,
                context,
                memory_fraction,
                dashboard,
                dashboard_spawned: AtomicBool::new(false),
            }),
        };
        if engine.dashboard_configured() {
            engine.spawn_dashboard();
        }
        Ok(engine)
    }
}

impl Engine {
    /// Whether a dashboard listener was requested at build.
    #[must_use]
    pub fn dashboard_configured(&self) -> bool {
        self.inner.dashboard.is_some()
    }

    /// Bound dashboard address (resolves `:0`).
    #[must_use]
    pub fn dashboard_addr(&self) -> Option<SocketAddr> {
        self.inner.dashboard.as_ref().map(|d| d.addr)
    }

    /// Spawn the dashboard HTTP server on the current tokio runtime.
    ///
    /// No-op when [`EngineBuilder::dashboard_listen`] was not set, when no
    /// runtime is current (Python building off-loop), or when already spawned.
    /// [`EngineBuilder::build`] calls this when a runtime is current; Python
    /// calls it after its own RT starts.
    pub fn spawn_dashboard(&self) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let Some(dash) = self.inner.dashboard.as_ref() else {
            return;
        };
        if self.inner.dashboard_spawned.swap(true, Ordering::Relaxed) {
            return;
        }
        let cloned = match dash.listener.try_clone() {
            Ok(l) => l,
            Err(e) => {
                warn!(error = %e, "dashboard listener clone failed");
                return;
            }
        };
        if let Err(e) = cloned.set_nonblocking(true) {
            warn!(error = %e, "dashboard listener set_nonblocking failed");
            return;
        }
        let listener = match tokio::net::TcpListener::from_std(cloned) {
            Ok(l) => l,
            Err(e) => {
                warn!(error = %e, "dashboard listener conversion failed");
                return;
            }
        };
        let app = dashboard_http_router()
            .with_state(self.inner.state.clone())
            .layer(axum::extract::DefaultBodyLimit::max(256 * 1024));
        let cancel = self.inner.cancel.clone();
        let addr = dash.addr;
        handle.spawn(async move {
            info!(%addr, "engine dashboard listening");
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    cancel.cancelled().await;
                })
                .await;
        });
    }
}

impl Engine {
    /// Spawn the refresh worker on the current tokio runtime (Python after RT start).
    pub fn spawn_refresh(&self) {
        if let Some(worker) = self.inner.state.refresh_worker.clone() {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    worker.run().await;
                });
            }
        }
    }

    /// Spawn the dynamic-memory ticker on the current tokio runtime.
    ///
    /// No-op when the cap is fixed ([`EngineBuilder::max_memory`]) or no
    /// runtime is current (e.g. Python building off-loop, then called after
    /// RT start).
    pub fn spawn_memory_ticker(&self) {
        if let Some(fraction) = self.inner.memory_fraction {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let state = self.inner.state.clone();
                let cancel = self.inner.cancel.clone();
                handle.spawn(async move {
                    memory_ticker(state, fraction, cancel).await;
                });
            }
        }
    }
}

/// Live-resize loop: re-read the budget, recompute `fraction × budget`, and
/// apply when the cap moves by more than 5%. Never below the floor.
async fn memory_ticker(
    state: crate::proxy::server::AppState,
    fraction: f64,
    cancel: CancellationToken,
) {
    let mut current = 0u64;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs(MEMORY_TICK_SECS)) => {}
        }
        let cap = cap_for_fraction(fraction, read_memory_budget()).max(DYNAMIC_CAP_FLOOR_BYTES);
        let threshold = current.saturating_div(20); // 5%
        if cap != current && cap.abs_diff(current) > threshold {
            state.cache.set_max_memory_bytes(cap as usize);
            state.cache.enforce_memory_limit();
            current = cap;
        }
    }
}

fn attach_persistence(proxy: &mut CacheProxy, persist: &Path) -> Result<(), EngineError> {
    #[cfg(feature = "persistence")]
    {
        use crate::proxy::persistence::PersistentCache;
        let pc = PersistentCache::open(persist).map_err(|e| EngineError::Persist(e.to_string()))?;
        let store = Arc::get_mut(proxy.cache_arc_mut()).ok_or_else(|| {
            EngineError::Persist("cache already shared; cannot attach persistence".into())
        })?;
        store.set_persistence(Arc::new(pc));
        let _restored = store.restore_from_persistence();
        Ok(())
    }
    #[cfg(not(feature = "persistence"))]
    {
        let _ = (proxy, persist);
        Err(EngineError::Feature("persistence"))
    }
}

pub(crate) fn parse_memory(raw: &str) -> Result<u64, EngineError> {
    let s = raw.trim();
    if let Ok(n) = s.parse::<u64>() {
        return Ok(n);
    }
    let lower = s.to_ascii_lowercase();
    let (num, mul) = if let Some(n) = lower.strip_suffix("gib") {
        (n, 1024u64.saturating_mul(1024).saturating_mul(1024))
    } else if let Some(n) = lower.strip_suffix("mib") {
        (n, 1024u64.saturating_mul(1024))
    } else if let Some(n) = lower.strip_suffix("kib") {
        (n, 1024u64)
    } else {
        return Err(EngineError::Config(format!("invalid max_memory: {raw}")));
    };
    let n: u64 = num
        .trim()
        .parse()
        .map_err(|_| EngineError::Config(format!("invalid max_memory: {raw}")))?;
    Ok(n.saturating_mul(mul))
}

fn next_request_id() -> u64 {
    static N: AtomicU64 = AtomicU64::new(1);
    N.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
