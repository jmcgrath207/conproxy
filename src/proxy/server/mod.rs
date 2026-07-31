//! Axum HTTP server for the cache proxy.
//!
//! # API Endpoints
//!
//! ## Protected Endpoints (require API key if configured)
//!
//! - `POST /query` - Execute a single query with caching
//! - `POST /batch` - Execute multiple queries in a single request
//! - `POST /federated` - Execute federated search with local results and remote fallback
//! - `GET /stats` - Get server and cache statistics
//! - `GET /stats/queries` - Get query access statistics and hot queries
//! - `GET /metrics` - Get detailed performance metrics (JSON)
//! - `GET /metrics/prometheus` - Get metrics in Prometheus/OpenMetrics text format
//! - `POST /cache/clear` - Clear all cache entries
//! - `GET /cache/integrity` - Verify cache entry integrity
//! - `GET /cache/upstreams` - Get cache statistics by upstream
//! - `POST /cache/warmup` - Pre-fetch queries to populate cache
//! - `POST /cache/evict` - Selectively evict cache entries
//! - `GET /audit` - Get recent request audit log
//! - `GET /circuit` - Get circuit breaker status
//! - `GET /queue` - Get request queue status and statistics
//! - `GET /contexts` - List all available contexts
//! - `GET /contexts/current` - Get current context metadata and stats
//! - `POST /contexts/switch` - Switch to a different context
//! - `POST /contexts/create` - Create a new context
//! - `GET /contexts/:id/stats` - Per-context cache statistics
//! - `POST /admin/reload` - Re-read configuration from disk (hot-reload)
//! - `POST /admin/pause` - Pause accepting queries (drain in-flight)
//! - `POST /admin/resume` - Resume accepting queries
//! - `POST /admin/metrics/reset` - Reset all metrics counters to zero
//! - `GET /admin/agents` - List all registered agents
//! - `POST /admin/agents` - Register a new agent
//! - `DELETE /admin/agents/:id` - Remove an agent
//! - `POST /admin/agents/:id/rotate-key` - Rotate an agent's API key
//! - `GET /clients` - Active client connections
//!
//! ## Public Endpoints (no authentication)
//!
//! - `GET /health` - Health check with upstream status
//! - `GET /ready` - Readiness probe for load balancers
//! - `GET /pool` - Upstream pool status and statistics

use tracing::{debug, info, instrument, warn};

#[allow(unused_imports)]
use axum::{
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{any, delete, get, post},
    Json, Router,
};
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use super::adaptive::AdaptiveTimeout;
use super::agent::{AgentIdentity, AgentRegistry};
use super::audit::AuditLog;
use super::batch::{BatchConfig, BatchProcessor, BatchRequest, BatchResponse};
use super::cache::CacheStore;
use super::cascade::CascadeExecutor;
use super::circuit::CircuitBreaker;
use super::coalesce::{CoalesceAction, RequestCoalescer};
use super::context::{ContextConfig, ContextManager, ContextMetadata};
use super::federated::{FallbackDecision, FederatedSearch, FederatedStats};
use super::lifecycle::ProxyError;
use super::metrics::ProxyMetrics;
#[allow(unused_imports)]
use super::middleware::{
    auth_middleware, rate_limit_middleware, AuthConfig, RateLimitConfig, RateLimiter,
};
use super::pool::{LoadBalanceStrategy, PoolStats, UpstreamPool};
use super::priority::PriorityQueue;
use super::query_stats::QueryStatsTracker;
use super::refresh::QueryTrackingRefreshWorker;
#[allow(unused_imports)]
use super::retry::{RetryCondition, RetryExecutor, RetryPolicy};
use super::scope::ScopeFilter;
#[cfg(feature = "embed-api")]
use super::semantic_cache::SemanticCache;
#[cfg(feature = "embed-api")]
use super::smart_embedder::SmartEmbedder;
#[allow(unused_imports)]
use super::types::{
    CacheStatus, CachedResponse, Freshness, MissReason, QueryRequest, QueryResponse,
};
use super::upstream::GenericRestAdapter;
#[cfg(feature = "embed-api")]
use super::upstream::{QueryMode, UpstreamAdapter};
use crate::config::ProxyConfig;

use super::cdc::CdcManager;
use arc_swap::{ArcSwap, ArcSwapOption};

/// Alias for always-present reloadable slot (e.g. FederatedSearch).
type ReloadArc<T> = Arc<ArcSwap<T>>;
/// Alias for optional reloadable slot (e.g. UpstreamPool, agent_registry).
type ReloadOpt<T> = Arc<ArcSwapOption<T>>;
use super::peer::PeerManager;

// HTTP handlers — served on the HTTP port alongside gRPC on the primary port.
mod admin;
mod batch;
mod cache;
mod context;
mod query;
pub(crate) mod query_core;

mod status;

/// Embedded web UI (dashboard).
mod web_ui;

/// Structured error response for proxy endpoints.
#[derive(Debug, Clone, Serialize)]
pub struct ProxyErrorResponse {
    /// Error category: "upstream_error", "timeout", "rate_limited", "validation_error", "internal"
    pub error_type: String,
    /// Human-readable error description
    pub message: String,
    /// Request ID (propagated from X-Request-Id header or auto-generated)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Trail of upstream attempts (for cascade queries)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cascade_trail: Vec<CascadeStep>,
}

/// A single step in the cascade query trail.
#[derive(Debug, Clone, Serialize)]
pub struct CascadeStep {
    /// Upstream identifier
    pub upstream: String,
    /// Outcome: "timeout", "error", "circuit_open", "success"
    pub status: String,
    /// Time spent on this upstream in milliseconds
    pub latency_ms: u64,
}

impl ProxyErrorResponse {
    /// Create a simple error response without cascade trail.
    pub fn new(error_type: &str, message: impl Into<String>) -> Self {
        Self {
            error_type: error_type.to_string(),
            message: message.into(),
            request_id: None,
            cascade_trail: vec![],
        }
    }

    /// Set the request ID.
    pub fn with_request_id(mut self, id: Option<String>) -> Self {
        self.request_id = id;
        self
    }
}

/// Extract X-Request-Id from headers, or generate a new one from timestamp.
pub(super) fn extract_request_id(headers: &axum::http::HeaderMap) -> String {
    headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            use std::time::SystemTime;
            let ts = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            format!("req-{:x}", ts)
        })
}

/// Resolve the context for this request.
///
/// Priority: X-Context header > agent default_context > "default"
fn resolve_context(headers: &axum::http::HeaderMap, agent: Option<&AgentIdentity>) -> String {
    // 1. Explicit header
    if let Some(val) = headers.get("x-context") {
        if let Ok(ctx) = val.to_str() {
            if !ctx.is_empty() {
                return ctx.to_string();
            }
        }
    }
    // 2. Agent default
    if let Some(agent) = agent {
        if let Some(ref ctx) = agent.default_context {
            return ctx.clone();
        }
    }
    // 3. Global fallback
    "default".to_string()
}

/// Build a context-isolated cache key string.
/// By prefixing the context ID, the same query in different contexts
/// produces different blake3 hashes, achieving cache isolation.
fn context_query(context_id: &str, query: &str) -> String {
    format!("ctx:{}:{}", context_id, query)
}

/// Tracks active client connections for pgbouncer-style monitoring.
pub(crate) struct ClientTracker {
    /// Currently active requests (request_id -> client info)
    active: dashmap::DashMap<String, ClientInfo>,
    /// Total completed requests
    pub(crate) total_completed: std::sync::atomic::AtomicU64,
    /// Total requests that were rejected (paused, rate limited, etc.)
    pub(crate) total_rejected: std::sync::atomic::AtomicU64,
}

/// Information about an active client request.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ClientInfo {
    /// Request ID
    pub(crate) request_id: String,
    /// Request start time (as epoch millis)
    pub(crate) started_at_ms: u64,
    /// Query being executed
    pub(crate) query: String,
    /// Client source (from X-Forwarded-For or remote addr)
    pub(crate) source: String,
}

impl ClientTracker {
    fn new() -> Self {
        Self {
            active: dashmap::DashMap::new(),
            total_completed: std::sync::atomic::AtomicU64::new(0),
            total_rejected: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn track(&self, request_id: String, query: String, source: String) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.active.insert(
            request_id.clone(),
            ClientInfo {
                request_id,
                started_at_ms: now_ms,
                query,
                source,
            },
        );
    }

    fn complete(&self, request_id: &str) {
        self.active.remove(request_id);
        self.total_completed
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    fn reject(&self) {
        self.total_rejected
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn active_count(&self) -> usize {
        self.active.len()
    }

    pub(crate) fn snapshot(&self) -> Vec<ClientInfo> {
        self.active.iter().map(|r| r.value().clone()).collect()
    }
}

/// The cache proxy server.
pub struct CacheProxy {
    /// The cache storage.
    cache: Arc<CacheStore>,
    /// The upstream adapter (if single upstream configured).
    upstream: Option<Arc<GenericRestAdapter>>,
    /// Upstream pool for multiple upstreams with load balancing.
    upstream_pool: Option<Arc<UpstreamPool>>,
    /// Request coalescer for deduplicating concurrent requests.
    coalescer: Arc<RequestCoalescer>,
    /// Scope filter for filtering upstream results.
    scope_filter: Arc<ScopeFilter>,
    /// Metrics collector.
    metrics: Arc<ProxyMetrics>,
    /// Circuit breaker for upstream protection.
    circuit_breaker: Arc<CircuitBreaker>,
    /// Audit log for request tracking.
    audit_log: Arc<AuditLog>,
    /// Retry policy for upstream requests.
    retry_policy: RetryPolicy,
    /// Adaptive timeout calculator.
    adaptive_timeout: Arc<AdaptiveTimeout>,
    /// Unique identifier for this proxy instance.
    upstream_id: String,
    /// Server start time.
    start_time: Instant,
    /// Refresh interval for background worker.
    refresh_interval: Duration,
    /// API key for authentication (if configured).
    api_key: Option<String>,
    /// Rate limiter (if configured).
    rate_limiter: Option<Arc<RateLimiter>>,
    /// Federated search for local+remote merging.
    federated_search: Arc<FederatedSearch>,
    /// Priority queue for request ordering under load.
    request_queue: Arc<PriorityQueue<QueryRequest>>,
    /// Context manager for multi-context cache support.
    context_manager: Arc<ContextManager>,
    /// Smart embedder for VectorOnly upstreams (optional, requires proxy-embed feature).
    #[cfg(feature = "embed-api")]
    smart_embedder: Option<Arc<SmartEmbedder>>,
    /// Run embedder warmup at server startup.
    #[cfg(feature = "embed-api")]
    warmup_on_start: bool,
    /// Semantic cache tier for embedding-similarity matching (optional).
    #[cfg(feature = "embed-api")]
    semantic_cache: Option<Arc<SemanticCache>>,
    /// Cascade executor for priority-based upstream querying.
    cascade_executor: Option<Arc<CascadeExecutor>>,
    /// Agent registry for multi-tenancy (optional).
    agent_registry: Option<Arc<AgentRegistry>>,
    /// CDC manager for cache mutation event streaming (optional).
    cdc_manager: Option<Arc<CdcManager>>,
    /// Peer replication configuration (PeerManager created in run() with CancellationToken).
    peer_config: Option<crate::config::PeerConfig>,
    /// Socket tuning configuration for listeners and upstream clients.
    socket_tuning: crate::config::SocketTuningConfig,
    /// Graceful shutdown timeout.
    shutdown_timeout: Duration,
    /// Maximum global concurrent upstream connections.
    max_global_connections: usize,
    /// Path to the config file the proxy was started with (for hot-reload).
    /// `None` means the proxy loaded via `Config::load()` (default-merge).
    reload_source: Option<std::path::PathBuf>,
    /// Enable web UI auth bypass for GET status paths.
    web_ui_enabled: bool,
}

/// Shared state for Axum and gRPC handlers.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) cache: Arc<CacheStore>,
    /// Single-upstream adapter — hot-reloadable via ArcSwap.
    pub(crate) upstream: ReloadOpt<GenericRestAdapter>,
    /// Upstream pool — hot-reloadable via ArcSwap.
    pub(crate) upstream_pool: ReloadOpt<UpstreamPool>,
    pub(crate) coalescer: Arc<RequestCoalescer>,
    pub(crate) refresh_worker: Option<Arc<QueryTrackingRefreshWorker>>,
    pub(crate) scope_filter: Arc<ScopeFilter>,
    pub(crate) metrics: Arc<ProxyMetrics>,
    pub(crate) circuit_breaker: Arc<CircuitBreaker>,
    pub(crate) audit_log: Arc<AuditLog>,
    pub(crate) retry_policy: Arc<RetryPolicy>,
    pub(crate) adaptive_timeout: Arc<AdaptiveTimeout>,
    pub(crate) query_stats: Arc<QueryStatsTracker>,
    pub(crate) batch_processor: Arc<BatchProcessor>,
    /// Federated search — hot-reloadable via ArcSwap.
    pub(crate) federated_search: ReloadArc<FederatedSearch>,
    pub(crate) request_queue: Arc<PriorityQueue<QueryRequest>>,
    pub(crate) upstream_id: String,
    pub(crate) start_time: Instant,
    pub(crate) context_manager: Arc<ContextManager>,
    #[cfg(feature = "embed-api")]
    pub(crate) smart_embedder: Option<Arc<SmartEmbedder>>,
    /// Semantic cache tier for embedding-similarity matching (optional).
    #[cfg(feature = "embed-api")]
    pub(crate) semantic_cache: Option<Arc<SemanticCache>>,
    pub(crate) degradation_level: Arc<std::sync::atomic::AtomicU8>,
    /// Whether the proxy is paused (pgbouncer PAUSE/RESUME parity).
    pub(crate) paused: Arc<std::sync::atomic::AtomicBool>,
    /// Client connection tracker for pgbouncer-style monitoring.
    pub(crate) client_tracker: Arc<ClientTracker>,
    /// Cascade executor — hot-reloadable via ArcSwap.
    pub(crate) cascade_executor: ReloadOpt<CascadeExecutor>,
    /// Agent registry — hot-reloadable via ArcSwap.
    pub(crate) agent_registry: ReloadOpt<AgentRegistry>,
    /// CDC manager for cache mutation events.
    pub(crate) cdc_manager: Option<Arc<CdcManager>>,
    /// Peer manager for peer-to-peer replication.
    pub(crate) peer_manager: Option<Arc<PeerManager>>,
    /// Global concurrency semaphore to cap total upstream connections.
    pub(crate) global_concurrency: Arc<tokio::sync::Semaphore>,
    /// Path to the config file the proxy was started with (for hot-reload).
    /// `None` means the proxy loaded via `Config::load()` (default-merge).
    pub(crate) reload_source: Option<std::path::PathBuf>,
    /// Handle to the current Tokio runtime. Used by the `/debug/tokio`
    /// endpoint to snapshot `Handle::metrics()` and (with `tokio_unstable`)
    /// `Handle::dump()`. Captured at server construction. `None` in test
    /// helpers that don't run inside a tokio runtime.
    pub(crate) tokio_handle: Option<tokio::runtime::Handle>,
}

impl AppState {
    /// Check if the peer manager is in warming state (not ready for traffic).
    pub(crate) fn peer_is_warming(&self) -> bool {
        self.peer_manager
            .as_ref()
            .map(|pm| !pm.is_ready())
            .unwrap_or(false)
    }

    /// Scope filter for a context id (plan 10 T2).
    ///
    /// Prefers per-context policy when installed; falls back to process-level
    /// `scope_filter` (legacy / default-context projection).
    pub(crate) fn scope_filter_for(&self, context_id: &str) -> Arc<ScopeFilter> {
        self.context_manager
            .scope_filter_for(context_id)
            .unwrap_or_else(|| self.scope_filter.clone())
    }

    /// Re-read the config from the path the proxy was started with (or from
    /// `Config::load()` default-merge), then atomically apply the reloadable
    /// fields to the live state.
    ///
    /// # Reloads (applied to live state)
    ///
    /// - `cache.max_entries`, `cache.fresh_duration_secs`, `cache.stale_duration_secs`
    /// - `proxy.federated` — swapped atomically via ArcSwap
    /// - `proxy.upstreams` / `proxy.cascade` — rebuilt then committed together;
    ///   on build failure, no upstream/cascade/federated stores are committed
    /// - `proxy.circuit_breaker` — thresholds updated in place (trip state preserved)
    /// - `[[agents]]` (Phase B, plan 04) — registry rebuilt from file and committed
    ///   via ArcSwap. **File is authoritative on reload**: any runtime API mutations
    ///   (create / delete / rotate-key) on agents NOT in the file are dropped; agents
    ///   in the file are reloaded exactly as declared. To preserve API-only agents,
    ///   add them to the file too.
    ///
    /// # Restart required
    ///
    /// - `proxy.peer` — peer manager started once
    /// - `proxy.cdc` — CDC manager started once
    /// - `proxy.api_key` — auth middleware captures key at startup
    /// - `proxy.rate_limit` — built into middleware at startup
    /// - `proxy.listen` / `proxy.http_listen` — listener address
    ///
    /// # Errors
    ///
    /// Returns `Err` (as a `String` message) when:
    /// - the config file path is not valid UTF-8
    /// - the config file fails to parse
    /// - the upstream pool or single adapter cannot be built
    /// - `cache.max_entries` rejects the new value
    pub(crate) fn apply_reload(&self) -> Result<ReloadSummary, String> {
        use crate::config::Config;

        let new_config = match &self.reload_source {
            Some(path) => Config::load_from(
                path.to_str()
                    .ok_or_else(|| format!("config path is not valid UTF-8: {}", path.display()))?,
            )
            .map_err(|e| format!("Failed to parse config at {}: {}", path.display(), e))?,
            None => Config::load()
                .map_err(|e| format!("Failed to load config from default locations: {}", e))?,
        };

        let mut sections: Vec<String> = Vec::new();
        let mut restart_required: Vec<String> = Vec::new();
        // Plan 10: project context-rooted tables into ProxyConfig shape
        let projected = new_config
            .config
            .effective_proxy()
            .map_err(|e| format!("config project: {e}"))?;
        let proxy_cfg = &projected;
        // Refresh per-context policies when contexts present
        if let Ok(resolved) = new_config.config.resolve_contexts() {
            if !resolved.is_empty() {
                self.context_manager.install_resolved(&resolved);
                sections.push(format!("contexts={}", resolved.len()));
            }
        }
        let old_max = self.cache.max_entries();

        // --- cache.max_entries ---
        let new_max = proxy_cfg.max_entries();
        if new_max != old_max {
            self.cache
                .set_max_entries(new_max)
                .map_err(|e| format!("cache.max_entries: {}", e))?;
            sections.push(format!("cache.max_entries={}", new_max));
        }

        // --- cache.fresh_duration_secs ---
        let new_fresh = proxy_cfg.fresh_duration_secs();
        if let Some(orig_fresh) = self.cache.fresh_duration_secs() {
            if new_fresh != orig_fresh {
                self.cache
                    .set_fresh_duration(std::time::Duration::from_secs(new_fresh));
                sections.push(format!("cache.fresh_duration_secs={}", new_fresh));
            }
        }

        // --- cache.stale_duration_secs ---
        let new_stale = proxy_cfg.stale_duration_secs();
        if let Some(orig_stale) = self.cache.stale_duration_secs() {
            if new_stale != orig_stale {
                self.cache
                    .set_stale_duration(std::time::Duration::from_secs(new_stale));
                sections.push(format!("cache.stale_duration_secs={}", new_stale));
            }
        }

        // --- Build phase: construct all fallible values before any ArcSwap store ---
        // This ensures that a pool-adapter build failure does not leave live
        // state in a partially-updated condition.

        // Federated search is infallible to build.
        let new_federated = Arc::new(FederatedSearch::from_config(&proxy_cfg.federated));

        // Upstream pool / single adapter / cascade — build first, commit later.
        enum UpstreamBuild {
            Pool {
                pool: Arc<UpstreamPool>,
                cascade: Option<Arc<CascadeExecutor>>,
                count: usize,
            },
            Single {
                adapter: Arc<crate::proxy::upstream::GenericRestAdapter>,
                url: String,
            },
            None,
        }

        let upstream_build = if !proxy_cfg.upstreams().is_empty() {
            let strategy = crate::proxy::pool::LoadBalanceStrategy::RoundRobin;
            let pool = UpstreamPool::new(proxy_cfg.upstreams(), strategy)
                .map_err(|e| format!("proxy.upstreams: failed to build pool: {}", e))?;
            let pool = Arc::new(pool);
            let cascade = if proxy_cfg.cascade.enabled {
                Some(Arc::new(CascadeExecutor::with_metrics(
                    pool.clone(),
                    proxy_cfg.cascade.clone(),
                    self.metrics.clone(),
                )))
            } else {
                None
            };
            UpstreamBuild::Pool {
                pool,
                cascade,
                count: proxy_cfg.upstreams().len(),
            }
        } else if let Some(url) = proxy_cfg.upstream_url() {
            let adapter = crate::proxy::upstream::GenericRestAdapter::new(
                url,
                std::time::Duration::from_secs(proxy_cfg.upstream_timeout_secs()),
            )
            .map_err(|e| format!("proxy.upstreams: failed to build adapter: {}", e))?;
            UpstreamBuild::Single {
                adapter: Arc::new(adapter),
                url: url.to_string(),
            }
        } else {
            UpstreamBuild::None
        };

        // --- Commit phase: apply all ArcSwap stores together ---
        self.federated_search.store(new_federated);
        sections.push(format!(
            "proxy.federated (enabled={})",
            proxy_cfg.federated.enabled()
        ));

        match upstream_build {
            UpstreamBuild::Pool {
                pool,
                cascade,
                count,
            } => {
                self.upstream_pool.store(Some(pool));
                self.upstream.store(None);
                sections.push(format!("proxy.upstreams ({} in pool)", count));

                if cascade.is_some() {
                    self.cascade_executor.store(cascade);
                    sections.push("proxy.cascade".to_string());
                } else {
                    self.cascade_executor.store(None);
                }
            }
            UpstreamBuild::Single { adapter, url } => {
                self.upstream.store(Some(adapter));
                self.upstream_pool.store(None);
                self.cascade_executor.store(None);
                sections.push(format!("proxy.upstreams (single: {})", url));
            }
            UpstreamBuild::None => {
                self.upstream_pool.store(None);
                self.cascade_executor.store(None);
                self.upstream.store(None);
                sections.push("proxy.upstreams (none)".to_string());
            }
        }

        // --- proxy.circuit_breaker config update ---
        // set_config updates thresholds in place; keeps trip state.
        self.circuit_breaker.set_config(
            new_config
                .config
                .proxy
                .circuit_breaker
                .to_circuit_breaker_config(),
        );
        sections.push("proxy.circuit_breaker".to_string());

        // --- [[agents]] (Phase B, plan 04) ---
        // Rebuild the agent registry from the new file config and commit via
        // ArcSwap. File is authoritative: API-only agents (created via
        // POST /admin/agents) are dropped on reload. Per-agent rate_limit
        // and api_key are picked up on the next request after the swap.
        //
        // IMPORTANT: when the merged agent list is empty, store `None` to match
        // startup semantics. Previously we stored `Some(empty registry)`, which
        // flipped the gRPC auth path into "API key required" mode even when no
        // agents or global key were configured (reload regression, plan 04).
        let new_agents: Vec<crate::config::AgentConfig> = if !new_config.config.agents.is_empty() {
            new_config.config.agents.clone()
        } else {
            proxy_cfg.agents().to_vec()
        };
        if new_agents.is_empty() {
            self.agent_registry.store(None);
            sections.push("agents (none; auth disabled)".to_string());
        } else {
            let new_agent_registry = Arc::new(AgentRegistry::from_config(&new_agents));
            self.agent_registry.store(Some(new_agent_registry.clone()));
            sections.push(format!("agents ({} registered)", new_agent_registry.len()));
        }

        // --- Sections that truly require restart ---
        if proxy_cfg.peer().enabled() {
            restart_required.push("proxy.peer (restart to apply)".to_string());
        }
        if let Some(ref api_key) = proxy_cfg.api_key {
            if !api_key.is_empty() {
                restart_required.push("proxy.api_key (auth key; restart to apply)".to_string());
            }
        }
        if proxy_cfg.rate_limit().enabled() {
            restart_required.push("proxy.rate_limit (restart to apply)".to_string());
        }

        if sections.is_empty() {
            sections.push("config.parsed (no reloadable changes detected)".to_string());
        }

        Ok(ReloadSummary {
            sections,
            restart_required,
        })
    }
}

/// Summary of what changed during a `/admin/reload` invocation.
#[derive(Debug)]
#[must_use = "reload result should be checked for restart_required sections"]
pub(crate) struct ReloadSummary {
    pub(crate) sections: Vec<String>,
    /// Sections that were read but require a process restart to take effect.
    pub(crate) restart_required: Vec<String>,
}

/// Degradation level indicating the proxy's operational capability.
///
/// Each level represents reduced functionality compared to the previous level.
/// The proxy determines its level at startup and updates it as conditions change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DegradationLevel {
    /// Level 0: Full operation - all upstreams healthy, persistence working, embedder loaded
    Full = 0,
    /// Level 1: Stale serving - upstreams degraded, serving from cache only
    StaleServing = 1,
    /// Level 2: Read-only - persistence failed, in-memory cache only, no writes
    ReadOnly = 2,
    /// Level 3: Text-only - embedder failed, VectorOnly upstreams unavailable, TextNative only
    TextOnly = 3,
    /// Level 4: Startup failure - no upstreams reachable, no cache, should exit
    StartupFailure = 4,
}

impl DegradationLevel {
    /// Human-readable description of this degradation level.
    pub fn description(&self) -> &'static str {
        match self {
            Self::Full => "Full operation",
            Self::StaleServing => "Serving from cache (upstreams degraded)",
            Self::ReadOnly => "Read-only (persistence unavailable)",
            Self::TextOnly => "Text-only (embedding unavailable)",
            Self::StartupFailure => "Startup failure (no upstreams reachable)",
        }
    }

    /// Whether this level is considered healthy for readiness probes.
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Full | Self::StaleServing)
    }
}

impl std::fmt::Display for DegradationLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Level {} ({})", *self as u8, self.description())
    }
}

/// Determine the current degradation level based on system state.
fn determine_degradation_level(state: &AppState) -> DegradationLevel {
    // Snapshot reloadable fields
    let has_upstream =
        state.upstream.load_full().is_some() || state.upstream_pool.load_full().is_some();
    if !has_upstream {
        return DegradationLevel::StartupFailure;
    }

    // Check pool health
    let pool = state.upstream_pool.load_full();
    if let Some(ref pool) = pool {
        let stats = pool.stats();
        if stats.healthy_upstreams == 0 {
            if !state.cache.is_empty() {
                return DegradationLevel::StaleServing;
            }
            return DegradationLevel::StartupFailure;
        }
    }

    DegradationLevel::Full
}

impl CacheProxy {
    /// Build cache store and optional CDC manager from config.
    fn build_cache_store(config: &ProxyConfig) -> (Arc<CacheStore>, Option<Arc<CdcManager>>) {
        let cdc_manager = if config.cdc_enabled() {
            let node_id = config.peer().node_id();
            Some(Arc::new(CdcManager::new(config.cdc(), node_id)))
        } else {
            None
        };

        let mut cache_store = CacheStore::with_jitter(
            Duration::from_secs(config.fresh_duration_secs()),
            Duration::from_secs(config.stale_duration_secs()),
            config.max_entries(),
            config.ttl_jitter_percent(),
        )
        .with_normalized_matching(config.cache.normalized_matching());

        let max_memory_bytes = config
            .cache
            .max_memory_mb()
            .saturating_mul(1024)
            .saturating_mul(1024);
        if max_memory_bytes > 0 {
            cache_store.set_max_memory_bytes(max_memory_bytes);
        }

        if let Some(ref cdc) = cdc_manager {
            cache_store = cache_store.with_cdc_sender(cdc.event_sender());
        }

        // Attach semantic cache tier if enabled
        #[cfg(feature = "embed-api")]
        if config.cache.semantic.enabled() {
            let semantic = Arc::new(SemanticCache::new(
                config.cache.semantic.similarity_threshold(),
                config.cache.semantic.max_entries(),
            ));
            cache_store = cache_store.with_semantic_cache(semantic);
        }

        let cache = Arc::new(cache_store);
        let scope_texts = config.scope_seeds();
        cache.update_config_fingerprint(config.upstream_url(), &scope_texts);

        (cache, cdc_manager)
    }

    /// Build upstream adapter(s) from config — pool or single.
    #[allow(clippy::type_complexity)]
    fn build_upstreams(
        config: &ProxyConfig,
    ) -> anyhow::Result<(Option<Arc<GenericRestAdapter>>, Option<Arc<UpstreamPool>>)> {
        if !config.upstreams().is_empty() {
            let pool = Arc::new(
                UpstreamPool::new(config.upstreams(), LoadBalanceStrategy::RoundRobin)
                    .map_err(|e| anyhow::anyhow!("Failed to create upstream pool: {}", e))?,
            );
            Ok((None, Some(pool)))
        } else {
            let single = config
                .upstream_url()
                .map(|url| {
                    GenericRestAdapter::new(
                        url,
                        Duration::from_secs(config.upstream_timeout_secs()),
                    )
                })
                .transpose()
                .map_err(|e| anyhow::anyhow!("Failed to create upstream adapter: {}", e))?
                .map(Arc::new);
            Ok((single, None))
        }
    }

    /// Build rate limiter and retry policy from config.
    fn build_resilience(config: &ProxyConfig) -> (Option<Arc<RateLimiter>>, RetryPolicy) {
        let rate_limiter = if config.rate_limit().enabled() {
            Some(Arc::new(RateLimiter::new(
                config.rate_limit().requests_per_second(),
                config.rate_limit().burst_size(),
            )))
        } else {
            None
        };

        let retry_config = config.retry();
        let retry_policy = if retry_config.enabled() {
            RetryPolicy {
                max_retries: retry_config.max_retries(),
                initial_delay: Duration::from_millis(retry_config.initial_delay_ms()),
                max_delay: Duration::from_millis(retry_config.max_delay_ms()),
                backoff_multiplier: retry_config.backoff_multiplier(),
                jitter_factor: 0.1,
                retry_on: RetryCondition {
                    on_network_error: retry_config.on_network_error(),
                    on_timeout: retry_config.on_timeout(),
                    on_server_error: retry_config.on_server_error(),
                    on_rate_limited: retry_config.on_rate_limited(),
                    on_status_codes: vec![],
                },
            }
        } else {
            RetryPolicy::no_retry()
        };

        (rate_limiter, retry_policy)
    }

    /// Create a new cache proxy with the given configuration.
    pub fn new(config: &ProxyConfig) -> anyhow::Result<Self> {
        let (cache, cdc_manager) = Self::build_cache_store(config);
        let (upstream, upstream_pool) = Self::build_upstreams(config)?;
        let (rate_limiter, retry_policy) = Self::build_resilience(config);

        let scope_filter = Arc::new(ScopeFilter::from_config(&config.scope));
        let upstream_id = format!("proxy-{}", std::process::id());

        // Honest surfacing of experimental upstream types at startup.
        for u in config.upstreams.iter() {
            if u.is_experimental() {
                warn!(
                    upstream = %u.id,
                    upstream_type = ?u.upstream_type(),
                    "experimental upstream configured — lighter e2e coverage, \
                     API may drift. See README 'Supported Upstreams'."
                );
            }
        }

        // Create SmartEmbedder eagerly (shared between cascade and single-upstream paths)
        #[cfg(feature = "embed-api")]
        let smart_embedder = Some(Arc::new(SmartEmbedder::with_defaults()));

        // Create cascade executor if enabled and pool is available
        let metrics = Arc::new(ProxyMetrics::new());
        let cascade_executor = if config.cascade.enabled {
            upstream_pool.as_ref().map(|pool| {
                let executor = CascadeExecutor::with_metrics(
                    pool.clone(),
                    config.cascade.clone(),
                    metrics.clone(),
                );
                #[cfg(feature = "embed-api")]
                let executor = if let Some(ref embedder) = smart_embedder {
                    executor.with_embedder(embedder.clone())
                } else {
                    executor
                };
                Arc::new(executor)
            })
        } else {
            None
        };

        let peer_config = if config.peer().enabled() {
            let secret_configured = matches!(config.peer().resolve_shared_secret(), Ok(Some(_)));
            if secret_configured {
                info!(
                    peers = config.peer().peers().len(),
                    "peer replication enabled with shared_secret auth (plan 07)"
                );
            } else {
                // Plan 06/07: surface trusted-network limit when secret not set.
                warn!(
                    peers = config.peer().peers().len(),
                    "peer replication enabled without shared_secret: \
                     no peer-specific auth (no mTLS). Peers must be on a trusted network. \
                     Set [proxy.peer] shared_secret to require x-peer-secret on peer gRPC. \
                     Do not expose peer gRPC on the public internet."
                );
            }
            Some(config.peer().clone())
        } else {
            None
        };

        Ok(Self {
            cache,
            upstream,
            upstream_pool,
            coalescer: Arc::new(RequestCoalescer::new()),
            scope_filter,
            metrics,
            circuit_breaker: Arc::new(CircuitBreaker::new(
                config.circuit_breaker.to_circuit_breaker_config(),
            )),
            audit_log: Arc::new(AuditLog::new(1000)),
            retry_policy,
            adaptive_timeout: Arc::new(AdaptiveTimeout::default_config()),
            upstream_id,
            start_time: Instant::now(),
            refresh_interval: Duration::from_secs(config.refresh_interval_secs()),
            api_key: config.api_key().map(|s| s.to_string()),
            rate_limiter,
            federated_search: Arc::new(FederatedSearch::from_config(&config.federated)),
            request_queue: Arc::new(PriorityQueue::new(1000)),
            context_manager: Arc::new(ContextManager::new(ContextConfig::default())),
            #[cfg(feature = "embed-api")]
            smart_embedder: None, // Initialized lazily when VectorOnly upstream detected
            #[cfg(feature = "embed-api")]
            warmup_on_start: false,
            #[cfg(feature = "embed-api")]
            semantic_cache: None,
            cascade_executor,
            agent_registry: if config.has_agents() {
                Some(Arc::new(AgentRegistry::from_config(config.agents())))
            } else {
                None
            },
            cdc_manager,
            peer_config,
            socket_tuning: config.socket_tuning.clone(),
            shutdown_timeout: Duration::from_secs(config.shutdown_timeout_secs()),
            max_global_connections: config.max_global_connections(),
            reload_source: None,
            web_ui_enabled: config.web_ui.enabled,
        })
    }

    /// Create a new cache proxy with a custom upstream adapter.
    pub fn with_upstream(config: &ProxyConfig, upstream: GenericRestAdapter) -> Self {
        let cache = Arc::new(
            CacheStore::with_jitter(
                Duration::from_secs(config.fresh_duration_secs()),
                Duration::from_secs(config.stale_duration_secs()),
                config.max_entries(),
                config.ttl_jitter_percent(),
            )
            .with_normalized_matching(config.cache.normalized_matching()),
        );

        // Update config fingerprint for cache invalidation
        let scope_texts = config.scope_seeds();
        cache.update_config_fingerprint(config.upstream_url(), &scope_texts);

        let scope_filter = Arc::new(ScopeFilter::from_config(&config.scope));
        let upstream_id = format!("proxy-{}", std::process::id());

        let (rate_limiter, retry_policy) = Self::build_resilience(config);

        // Create federated search from config
        let federated_search = Arc::new(FederatedSearch::from_config(&config.federated));

        // Create request queue for load shedding
        let request_queue = Arc::new(PriorityQueue::new(1000));

        // Create context manager for multi-context cache support
        let context_manager = Arc::new(ContextManager::new(ContextConfig::default()));

        Self {
            cache,
            upstream: Some(Arc::new(upstream)),
            upstream_pool: None, // Single upstream, no pool
            coalescer: Arc::new(RequestCoalescer::new()),
            scope_filter,
            metrics: Arc::new(ProxyMetrics::new()),
            circuit_breaker: Arc::new(CircuitBreaker::new(
                config.circuit_breaker.to_circuit_breaker_config(),
            )),
            audit_log: Arc::new(AuditLog::new(1000)), // Keep last 1000 requests
            retry_policy,
            adaptive_timeout: Arc::new(AdaptiveTimeout::default_config()),
            upstream_id,
            start_time: Instant::now(),
            refresh_interval: Duration::from_secs(config.refresh_interval_secs()),
            api_key: config.api_key().map(|s| s.to_string()),
            rate_limiter,
            federated_search,
            request_queue,
            context_manager,
            #[cfg(feature = "embed-api")]
            smart_embedder: None, // Initialized lazily when VectorOnly upstream detected
            #[cfg(feature = "embed-api")]
            warmup_on_start: false,
            #[cfg(feature = "embed-api")]
            semantic_cache: None,
            cascade_executor: None, // Single upstream, no cascade
            agent_registry: if config.has_agents() {
                Some(Arc::new(AgentRegistry::from_config(config.agents())))
            } else {
                None
            },
            cdc_manager: None,
            peer_config: None,
            socket_tuning: config.socket_tuning.clone(),
            shutdown_timeout: Duration::from_secs(config.shutdown_timeout_secs()),
            max_global_connections: config.max_global_connections(),
            reload_source: None,
            web_ui_enabled: config.web_ui.enabled,
        }
    }

    /// Configure the smart embedder from `[embedding]` config.
    ///
    /// Call after `new()` to override the default ONNX provider with an API
    /// provider (OpenAI, Cohere, HuggingFace) when `provider` is set in config.
    #[cfg(feature = "embed-api")]
    #[allow(clippy::field_reassign_with_default)]
    pub fn with_embedding_config(mut self, embedding: &crate::config::EmbeddingConfig) -> Self {
        use crate::proxy::embedder_config::EmbedderConfig;

        let mut ec = EmbedderConfig::default();
        ec.provider = embedding.provider().to_string();
        ec.api_key = embedding.api_key().map(|s| s.to_string());
        ec.base_url = embedding.base_url().map(|s| s.to_string());
        if let Some(bs) = embedding.batch_size {
            ec.max_batch_size = bs;
        }

        // Honor warmup_on_start from EmbedderConfig (now actually wired).
        self.warmup_on_start = ec.warmup_on_start;
        self.smart_embedder = Some(Arc::new(SmartEmbedder::new(ec)));
        self
    }

    /// Configure the semantic cache tier from `[proxy.cache.semantic]` config.
    ///
    /// Returns `self` unchanged when semantic matching is disabled.
    #[cfg(feature = "embed-api")]
    pub fn with_semantic_cache(
        mut self,
        cache_cfg: &crate::config::SemanticCacheSettingsConfig,
    ) -> Self {
        if !cache_cfg.enabled() {
            return self;
        }
        let semantic = Arc::new(SemanticCache::new(
            cache_cfg.similarity_threshold(),
            cache_cfg.max_entries(),
        ));
        // Mirror the cache_store attachment so inserts go through the tier.
        self.semantic_cache = Some(semantic);
        self
    }

    /// Install per-context policies from resolved context-rooted config (plan 10).
    pub fn with_resolved_contexts(self, resolved: &[crate::config::ResolvedContext]) -> Self {
        if !resolved.is_empty() {
            self.context_manager.install_resolved(resolved);
        }
        self
    }

    /// Set the path to the config file used at startup.
    ///
    /// Stored on the resulting `AppState` so `/admin/reload` can re-read from
    /// the same source after the proxy is running. Pass `None` to clear.
    pub fn with_reload_source(mut self, path: Option<std::path::PathBuf>) -> Self {
        self.reload_source = path;
        self
    }

    /// Run the proxy server with gRPC on primary port and HTTP on health port.
    ///
    /// - `listen_addr`: Primary address for gRPC (e.g., "127.0.0.1:9999")
    /// - `http_listen_addr`: Address for HTTP health/prometheus (e.g., "127.0.0.1:10000")
    /// - `cancel`: Cancellation token for graceful shutdown
    pub async fn run(
        self,
        listen_addr: &str,
        http_listen_addr: &str,
        cancel: CancellationToken,
    ) -> anyhow::Result<()> {
        // Create refresh worker if upstream is configured
        let refresh_worker = self.upstream.as_ref().map(|upstream| {
            Arc::new(QueryTrackingRefreshWorker::new(
                self.cache.clone(),
                upstream.clone(),
                self.upstream_id.clone(),
                self.refresh_interval,
                cancel.clone(),
            ))
        });

        // Create query stats tracker and batch processor
        let query_stats = Arc::new(QueryStatsTracker::new(10000)); // Track up to 10k queries
        let batch_processor = Arc::new(BatchProcessor::new(BatchConfig::default()));

        let global_concurrency = Arc::new(tokio::sync::Semaphore::new(self.max_global_connections));

        let state = AppState {
            cache: self.cache.clone(),
            upstream: Arc::new(ArcSwapOption::new(self.upstream.clone())),
            upstream_pool: Arc::new(ArcSwapOption::new(self.upstream_pool.clone())),
            coalescer: self.coalescer.clone(),
            refresh_worker: refresh_worker.clone(),
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
            global_concurrency: global_concurrency.clone(),
            reload_source: self.reload_source.clone(),
            peer_manager: if let (Some(ref cdc), Some(ref peer_cfg)) =
                (&self.cdc_manager, &self.peer_config)
            {
                Some(Arc::new(PeerManager::new(
                    peer_cfg.clone(),
                    self.cache.clone(),
                    cdc.event_sender(),
                    cancel.clone(),
                )))
            } else {
                None
            },
            tokio_handle: Some(tokio::runtime::Handle::current()),
        };

        // Determine initial degradation level
        let initial_level = determine_degradation_level(&state);
        state
            .degradation_level
            .store(initial_level as u8, std::sync::atomic::Ordering::Relaxed);

        // Create middleware configs
        let auth_config = {
            let base = match &self.api_key {
                Some(key) => AuthConfig::with_key(key.clone()),
                None => AuthConfig::disabled(),
            };
            let base = match &self.agent_registry {
                Some(registry) => base.with_registry(registry.clone()),
                None => base,
            };
            if self.web_ui_enabled {
                base.with_web_ui()
            } else {
                base
            }
        };

        let rate_limit_config = match &self.rate_limiter {
            Some(limiter) => RateLimitConfig::with_limiter(limiter.clone()),
            None => RateLimitConfig::disabled(),
        };

        // =====================================================================
        // gRPC server (primary port) — all operational endpoints
        // =====================================================================
        use crate::proxy::grpc::proto::admin_service_server::AdminServiceServer;
        use crate::proxy::grpc::proto::context_service_server::ContextServiceServer;
        use crate::proxy::grpc::proto::observability_service_server::ObservabilityServiceServer;
        use crate::proxy::grpc::proto::search_service_server::SearchServiceServer;

        let search_svc = crate::proxy::grpc::search::SearchServiceImpl::new(state.clone());
        let admin_svc = crate::proxy::grpc::admin::AdminServiceImpl::new(state.clone());
        let context_svc = crate::proxy::grpc::context::ContextServiceImpl::new(state.clone());
        let obs_svc =
            crate::proxy::grpc::observability::ObservabilityServiceImpl::new(state.clone());

        let grpc_addr: SocketAddr = listen_addr.parse()?;

        // =====================================================================
        // HTTP server (secondary port) — full REST API
        // =====================================================================

        // Protected routes: auth + rate limiting middleware applied
        use axum::middleware as axum_middleware;
        let protected = Router::new()
            // Query endpoints
            .route("/query", post(query::handle_query))
            .route("/batch", post(batch::handle_batch))
            .route("/federated", post(batch::handle_federated))
            // Observability
            .route("/stats", get(status::handle_stats))
            .route("/stats/queries", get(status::handle_query_stats))
            .route("/metrics", get(status::handle_metrics))
            .route("/audit", get(status::handle_audit))
            .route("/circuit", get(status::handle_circuit_status))
            .route("/queue", get(status::handle_queue_status))
            .route("/clients", get(status::handle_clients))
            // Cache management
            .route("/cache/clear", post(cache::handle_cache_clear))
            .route("/cache/warmup", post(cache::handle_cache_warmup))
            .route("/cache/evict", post(cache::handle_cache_evict))
            .route("/cache/integrity", get(cache::handle_cache_integrity))
            .route("/cache/upstreams", get(cache::handle_cache_upstreams))
            .route("/cache/entries", get(cache::handle_cache_entries))
            // Admin
            .route("/admin/reload", post(admin::handle_admin_reload))
            .route("/admin/pause", post(admin::handle_admin_pause))
            .route("/admin/resume", post(admin::handle_admin_resume))
            .route(
                "/admin/metrics/reset",
                post(admin::handle_admin_metrics_reset),
            )
            .route("/admin/agents", get(admin::handle_admin_agents_list))
            .route("/admin/agents", post(admin::handle_admin_agents_create))
            .route(
                "/admin/agents/{id}",
                delete(admin::handle_admin_agents_delete),
            )
            .route(
                "/admin/agents/{id}/rotate-key",
                post(admin::handle_admin_agents_rotate_key),
            )
            // Context management
            .route("/contexts", get(context::handle_contexts_list))
            .route("/contexts/current", get(context::handle_context_current))
            .route("/contexts/switch", post(context::handle_context_switch))
            .route("/contexts/create", post(context::handle_context_create))
            .route("/contexts/{id}/stats", get(context::handle_context_stats));

        // Apply auth + rate-limit to protected routes
        let protected = protected
            .route_layer(axum_middleware::from_fn_with_state(
                rate_limit_config,
                rate_limit_middleware,
            ))
            .route_layer(axum_middleware::from_fn_with_state(
                auth_config,
                auth_middleware,
            ));

        // Public routes: no auth required
        let http_app = Router::new()
            .route("/health", get(status::handle_health))
            .route("/ready", get(status::handle_ready))
            .route("/pool", get(status::handle_pool_status))
            .route(
                "/metrics/prometheus",
                get(status::handle_metrics_prometheus),
            )
            .route("/peer/status", get(handle_peer_status))
            .route("/debug/tokio", get(status::handle_tokio_metrics))
            .route("/debug/tokio/dump", get(status::handle_tokio_dump))
            .merge(protected);

        // Web UI dashboard (read-only, no auth)
        let http_app = if self.web_ui_enabled {
            http_app
                .route("/dashboard", get(web_ui::handle_dashboard))
                .route("/dashboard/{*path}", get(web_ui::handle_dashboard))
        } else {
            http_app
        };

        let http_app = http_app
            .with_state(state.clone())
            .layer(DefaultBodyLimit::max(256 * 1024)); // 256 KB default body limit

        // Check FD limits and sysctl recommendations before binding
        super::socket_tuning::check_fd_limit();
        super::socket_tuning::check_sysctl_recommendations();

        let http_addr: SocketAddr = http_listen_addr.parse()?;
        let http_listener =
            super::socket_tuning::create_tuned_listener(http_addr, &self.socket_tuning)?;

        // Apply sandbox after binding (ports are already open, safe to drop privileges)
        #[cfg(all(feature = "linux-sandbox", target_os = "linux"))]
        {
            if super::sandbox::needs_sandbox() {
                let sandbox_config = super::sandbox::SandboxConfig {
                    // Don't change user/group — let the deployment handle that
                    user: None,
                    group: None,
                    drop_capabilities: true,
                    allow_net_bind: grpc_addr.port() < 1024 || http_addr.port() < 1024,
                };
                match super::sandbox::apply_sandbox(&sandbox_config) {
                    Ok(()) => info!("Sandbox: capabilities dropped"),
                    Err(e) => warn!(error = %e, "Sandbox: failed to apply, continuing without"),
                }
            } else {
                info!("Sandbox: not needed (no excess capabilities)");
            }
        }

        info!(%grpc_addr, "gRPC server listening");
        info!(%http_addr, "HTTP REST server listening");
        if let Some(ref pool) = self.upstream_pool {
            info!(endpoints = pool.len(), "Upstream pool configured");
            for upstream in pool.all() {
                info!(
                    upstream = %upstream.adapter.identifier(),
                    weight = upstream.weight,
                    priority = upstream.priority,
                    "Upstream endpoint"
                );
            }
        } else if let Some(ref upstream) = self.upstream {
            info!(url = %upstream.base_url(), "Upstream configured");
        } else {
            warn!("No upstream configured (cache-only mode)");
        }

        // Log security configuration
        if self.api_key.is_some() {
            info!("Authentication: enabled (API key required)");
        }
        if let Some(ref registry) = self.agent_registry {
            info!(agents = registry.len(), "Multi-tenancy: enabled");
        }
        if let Some(ref limiter) = self.rate_limiter {
            info!(
                rate = limiter.refill_rate(),
                burst = limiter.capacity(),
                "Rate limiting: enabled"
            );
        }

        // Start refresh worker if configured
        if let Some(ref worker) = refresh_worker {
            let worker = worker.clone();
            tokio::spawn(async move {
                worker.run().await;
            });
            info!("Background refresh worker started");
        }

        // Start peer manager if configured
        if let Some(ref pm) = state.peer_manager {
            pm.start().await;
            info!(
                node_id = %pm.node_id(),
                peers = self.peer_config
                    .as_ref()
                    .map(|c| c.peers().len())
                    .unwrap_or(0),
                "Peer replication: enabled"
            );
        }

        // Log CDC status
        if let Some(ref cdc) = self.cdc_manager {
            info!(node_id = %cdc.node_id(), "CDC event stream: enabled");
        }

        // Run both servers concurrently with shared shutdown
        let cancel_grpc = cancel.clone();
        let cancel_http = cancel.clone();

        use tonic::codec::CompressionEncoding;
        use tonic::service::interceptor::InterceptedService;

        // Create gRPC auth + rate-limit interceptor (mirrors HTTP middleware config)
        let grpc_interceptor_config = crate::proxy::grpc::middleware::GrpcInterceptorConfig::new(
            self.agent_registry.clone(),
            self.rate_limiter.clone(),
            self.api_key.clone(),
        );
        let grpc_interceptor =
            crate::proxy::grpc::middleware::make_interceptor(grpc_interceptor_config);

        // Plan 07: optional peer shared-secret interceptor for CDC + Peer services.
        let peer_shared_secret = self
            .peer_config
            .as_ref()
            .and_then(|c| c.resolve_shared_secret().ok().flatten());

        // Create tuned gRPC listener with OS-level socket options
        let grpc_listener =
            super::socket_tuning::create_tuned_listener(grpc_addr, &self.socket_tuning)?;
        let grpc_incoming = tokio_stream::wrappers::TcpListenerStream::new(grpc_listener);

        let mut grpc_server_builder = tonic::transport::Server::builder()
            .tcp_keepalive(Some(std::time::Duration::from_secs(60)))
            .tcp_nodelay(true);

        let mut grpc_router = grpc_server_builder
            .add_service(InterceptedService::new(
                SearchServiceServer::new(search_svc)
                    .accept_compressed(CompressionEncoding::Gzip)
                    .accept_compressed(CompressionEncoding::Zstd)
                    .send_compressed(CompressionEncoding::Gzip)
                    .send_compressed(CompressionEncoding::Zstd),
                grpc_interceptor.clone(),
            ))
            .add_service(InterceptedService::new(
                AdminServiceServer::new(admin_svc)
                    .accept_compressed(CompressionEncoding::Gzip)
                    .accept_compressed(CompressionEncoding::Zstd)
                    .send_compressed(CompressionEncoding::Gzip)
                    .send_compressed(CompressionEncoding::Zstd),
                grpc_interceptor.clone(),
            ))
            .add_service(InterceptedService::new(
                ContextServiceServer::new(context_svc)
                    .accept_compressed(CompressionEncoding::Gzip)
                    .accept_compressed(CompressionEncoding::Zstd)
                    .send_compressed(CompressionEncoding::Gzip)
                    .send_compressed(CompressionEncoding::Zstd),
                grpc_interceptor.clone(),
            ))
            .add_service(InterceptedService::new(
                ObservabilityServiceServer::new(obs_svc)
                    .accept_compressed(CompressionEncoding::Gzip)
                    .accept_compressed(CompressionEncoding::Zstd)
                    .send_compressed(CompressionEncoding::Gzip)
                    .send_compressed(CompressionEncoding::Zstd),
                grpc_interceptor.clone(),
            ));

        // Register CDC gRPC service if enabled
        if let Some(ref cdc) = self.cdc_manager {
            use super::cdc::proto::cdc_service_server::CdcServiceServer;
            let cdc_svc = CdcServiceServer::new(cdc.grpc_service())
                .accept_compressed(CompressionEncoding::Gzip)
                .accept_compressed(CompressionEncoding::Zstd)
                .send_compressed(CompressionEncoding::Gzip)
                .send_compressed(CompressionEncoding::Zstd);
            grpc_router = if let Some(ref secret) = peer_shared_secret {
                let peer_ix =
                    crate::proxy::grpc::middleware::make_peer_secret_interceptor(secret.clone());
                grpc_router.add_service(InterceptedService::new(cdc_svc, peer_ix))
            } else {
                grpc_router.add_service(InterceptedService::new(cdc_svc, grpc_interceptor.clone()))
            };
        }

        // Register Peer gRPC service if enabled
        if let Some(ref pm) = state.peer_manager {
            use super::cdc::proto::peer_service_server::PeerServiceServer;
            let peer_svc = PeerServiceServer::new(pm.grpc_service())
                .accept_compressed(CompressionEncoding::Gzip)
                .accept_compressed(CompressionEncoding::Zstd)
                .send_compressed(CompressionEncoding::Gzip)
                .send_compressed(CompressionEncoding::Zstd);
            grpc_router = if let Some(ref secret) = peer_shared_secret {
                let peer_ix =
                    crate::proxy::grpc::middleware::make_peer_secret_interceptor(secret.clone());
                grpc_router.add_service(InterceptedService::new(peer_svc, peer_ix))
            } else {
                grpc_router.add_service(InterceptedService::new(peer_svc, grpc_interceptor.clone()))
            };
        }

        let grpc_server = grpc_router.serve_with_incoming_shutdown(grpc_incoming, async move {
            cancel_grpc.cancelled().await;
            info!("Shutting down gRPC server...");
        });

        let http_server = axum::serve(http_listener, http_app).with_graceful_shutdown(async move {
            cancel_http.cancelled().await;
            info!("Shutting down HTTP server...");
        });

        let shutdown_timeout = self.shutdown_timeout;

        // Run embedder warmup on startup (opt-in via EmbedderConfig). Skipped
        // when disabled in config, or when the embedder hasn't been configured.
        #[cfg(feature = "embed-api")]
        if let (Some(embedder), true) = (&self.smart_embedder, self.warmup_on_start) {
            if let Err(e) = embedder.warmup().await {
                warn!(error = %e, "smart_embedder warmup failed; continuing without");
            }
        }

        tokio::select! {
            result = grpc_server => {
                if let Err(e) = result {
                    warn!(error = %e, "gRPC server error");
                }
            }
            result = http_server => {
                if let Err(e) = result {
                    warn!(error = %e, "HTTP server error");
                }
            }
        }

        // --- Graceful shutdown: drain in-flight requests with deadline ---
        info!(
            timeout_secs = shutdown_timeout.as_secs(),
            "Servers stopped, draining in-flight requests"
        );
        #[allow(clippy::arithmetic_side_effects)]
        let deadline = tokio::time::Instant::now() + shutdown_timeout;

        // Wait for active client requests to complete
        loop {
            let active = state.client_tracker.active_count();
            if active == 0 {
                info!("All in-flight requests completed");
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                warn!(
                    "Shutdown timeout reached with {} active request(s), forcing exit",
                    active
                );
                break;
            }
            debug!(active, "Waiting for in-flight requests to complete");
            #[allow(clippy::arithmetic_side_effects)]
            let sleep_until = std::cmp::min(
                deadline,
                tokio::time::Instant::now() + Duration::from_millis(250),
            );
            tokio::time::sleep_until(sleep_until).await;
        }

        info!("Shutdown complete");
        Ok(())
    }

    /// Get the cache store.
    pub fn cache(&self) -> &Arc<CacheStore> {
        &self.cache
    }

    /// Get the upstream adapter.
    pub fn upstream(&self) -> Option<&Arc<GenericRestAdapter>> {
        self.upstream.as_ref()
    }

    /// Get the request coalescer.
    pub fn coalescer(&self) -> &Arc<RequestCoalescer> {
        &self.coalescer
    }

    /// Get the rate limiter (if configured).
    pub fn rate_limiter(&self) -> Option<&Arc<RateLimiter>> {
        self.rate_limiter.as_ref()
    }

    /// Check if authentication is required.
    pub fn requires_auth(&self) -> bool {
        self.api_key.is_some()
    }

    /// Get the circuit breaker.
    pub fn circuit_breaker(&self) -> &Arc<CircuitBreaker> {
        &self.circuit_breaker
    }

    /// Get the audit log.
    pub fn audit_log(&self) -> &Arc<AuditLog> {
        &self.audit_log
    }

    /// Get the retry policy.
    pub fn retry_policy(&self) -> &RetryPolicy {
        &self.retry_policy
    }

    /// Get the adaptive timeout calculator.
    pub fn adaptive_timeout(&self) -> &Arc<AdaptiveTimeout> {
        &self.adaptive_timeout
    }

    /// Get the upstream pool (if configured).
    pub fn upstream_pool(&self) -> Option<&Arc<UpstreamPool>> {
        self.upstream_pool.as_ref()
    }

    /// Check if any upstream is configured (pool or single).
    pub fn has_upstream(&self) -> bool {
        self.upstream_pool.is_some() || self.upstream.is_some()
    }

    /// Get the federated search handler.
    pub fn federated_search(&self) -> &Arc<FederatedSearch> {
        &self.federated_search
    }

    /// Get the request queue for priority-based ordering.
    pub fn request_queue(&self) -> &Arc<PriorityQueue<QueryRequest>> {
        &self.request_queue
    }

    /// Get the agent registry (if multi-tenancy is configured).
    pub fn agent_registry(&self) -> Option<&Arc<AgentRegistry>> {
        self.agent_registry.as_ref()
    }
}

/// Handle GET /peer/status requests.
async fn handle_peer_status(State(state): State<AppState>) -> impl IntoResponse {
    #[derive(Serialize)]
    struct PeerStatusResponse {
        enabled: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        node_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        state: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_entry_count: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cdc_subscribers: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cdc_sequence: Option<u64>,
    }

    if let Some(ref pm) = state.peer_manager {
        let cdc_info = state
            .cdc_manager
            .as_ref()
            .map(|cdc| (cdc.subscriber_count(), cdc.current_sequence()));

        Json(PeerStatusResponse {
            enabled: true,
            node_id: Some(pm.node_id()),
            state: Some(pm.state().as_str().to_string()),
            cache_entry_count: Some(state.cache.len()),
            cdc_subscribers: cdc_info.map(|(s, _)| s),
            cdc_sequence: cdc_info.map(|(_, seq)| seq),
        })
    } else {
        Json(PeerStatusResponse {
            enabled: false,
            node_id: None,
            state: None,
            cache_entry_count: None,
            cdc_subscribers: None,
            cdc_sequence: None,
        })
    }
}

#[cfg(test)]
#[path = "tests/mod_tests.rs"]
pub(crate) mod tests;
