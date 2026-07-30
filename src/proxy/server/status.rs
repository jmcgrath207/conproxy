//! Status, metrics, and monitoring handler functions for the cache proxy server.
//!
//! This module contains all read-only diagnostic endpoints:
//! - `GET /health` - Health check with upstream status
//! - `GET /stats` - Server and cache statistics
//! - `GET /metrics` - Detailed performance metrics (JSON)
//! - `GET /metrics/prometheus` - Prometheus/OpenMetrics text format
//! - `GET /stats/queries` - Query access statistics and hot queries
//! - `GET /audit` - Recent request audit log
//! - `GET /circuit` - Circuit breaker status
//! - `GET /queue` - Request queue status
//! - `GET /ready` - Readiness probe for Kubernetes
//! - `GET /pool` - Upstream pool status and statistics
//! - `GET /clients` - Active client connections

use super::*;
use tracing::{debug, instrument};

// ============================================================================
// Health Check
// ============================================================================

/// Health check response.
#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    uptime_secs: u64,
    cache_entries: usize,
    upstream_configured: bool,
    upstream_healthy: Option<bool>,
    /// Pool health summary (if pool is configured).
    pool_health: Option<PoolHealthSummary>,
    drift_alert: bool,
    error_rate: f64,
    /// Current degradation level of the proxy.
    degradation_level: DegradationLevel,
}

/// Pool health summary for health endpoint.
#[derive(Serialize)]
struct PoolHealthSummary {
    total: usize,
    healthy: usize,
    degraded: usize,
    offline: usize,
}

/// Handle GET /health requests.
#[instrument(skip(state))]
pub(super) async fn handle_health(State(state): State<AppState>) -> impl IntoResponse {
    debug!("Health check requested");
    // Check pool health if configured
    let pool = state.upstream_pool.load_full();
    let single = state.upstream.load_full();
    let (upstream_configured, upstream_healthy, pool_health) = if let Some(ref pool) = pool {
        let stats = pool.stats();

        // Pool is healthy if at least one upstream is healthy
        let pool_healthy = stats.healthy_upstreams > 0;

        (
            true,
            Some(pool_healthy),
            Some(PoolHealthSummary {
                total: stats.total_upstreams,
                healthy: stats.healthy_upstreams,
                degraded: stats.degraded_upstreams,
                offline: stats.offline_upstreams,
            }),
        )
    } else if let Some(ref upstream) = single {
        // Single upstream - do health check
        let healthy = match upstream.health_check().await {
            Ok(healthy) => Some(healthy),
            Err(_) => Some(false),
        };
        (true, healthy, None)
    } else {
        (false, None, None)
    };

    // Check for drift alerts
    let drift_alert = state
        .refresh_worker
        .as_ref()
        .map(|w| w.has_drift_alert())
        .unwrap_or(false);

    // Get error rate from metrics
    let snapshot = state.metrics.snapshot();
    let error_rate = snapshot.upstream_error_rate;

    // Determine overall status
    let status = if upstream_healthy == Some(false) {
        "degraded"
    } else if drift_alert {
        "warning"
    } else if error_rate > 0.1 {
        "degraded"
    } else {
        "ok"
    };

    // Read current degradation level
    let level_val = state
        .degradation_level
        .load(std::sync::atomic::Ordering::Relaxed);
    let degradation_level = match level_val {
        0 => DegradationLevel::Full,
        1 => DegradationLevel::StaleServing,
        2 => DegradationLevel::ReadOnly,
        3 => DegradationLevel::TextOnly,
        _ => DegradationLevel::StartupFailure,
    };

    let response = HealthResponse {
        status,
        uptime_secs: state.start_time.elapsed().as_secs(),
        cache_entries: state.cache.len(),
        upstream_configured,
        upstream_healthy,
        pool_health,
        drift_alert,
        error_rate,
        degradation_level,
    };

    (StatusCode::OK, Json(response))
}

// ============================================================================
// Stats
// ============================================================================

/// Stats response.
#[derive(Serialize)]
struct StatsResponse {
    uptime_secs: u64,
    cache: CacheStatsResponse,
    miss_reasons: MissReasonsResponse,
    upstream: Option<UpstreamStatsResponse>,
    coalescer: CoalescerStatsResponse,
    refresh_worker: Option<RefreshWorkerStatsResponse>,
    contexts: Vec<ContextStatsResponse>,
}

#[derive(Serialize)]
struct MissReasonsResponse {
    not_in_cache: u64,
    expired: u64,
    upstream_error: u64,
    circuit_open: u64,
}

#[derive(Serialize)]
struct ContextStatsResponse {
    id: String,
    hits: u64,
    misses: u64,
    queries: u64,
    hit_rate: f64,
}

#[derive(Serialize)]
struct CacheStatsResponse {
    total: usize,
    fresh: usize,
    stale: usize,
    expired: usize,
    max_entries: usize,
}

#[derive(Serialize)]
struct UpstreamStatsResponse {
    url: String,
    timeout_secs: u64,
}

#[derive(Serialize)]
struct CoalescerStatsResponse {
    in_flight_requests: usize,
}

#[derive(Serialize)]
struct RefreshWorkerStatsResponse {
    tracked_queries: usize,
    pending_refreshes: usize,
}

/// Handle GET /stats requests.
pub(super) async fn handle_stats(State(state): State<AppState>) -> impl IntoResponse {
    let cache_stats = state.cache.stats();

    let upstream = state.upstream.load_full().map(|u| UpstreamStatsResponse {
        url: u.base_url().to_string(),
        timeout_secs: u.timeout().as_secs(),
    });

    let coalescer = CoalescerStatsResponse {
        in_flight_requests: state.coalescer.in_flight_count(),
    };

    let refresh_worker = state
        .refresh_worker
        .as_ref()
        .map(|w| RefreshWorkerStatsResponse {
            tracked_queries: w.tracked_count(),
            pending_refreshes: w.pending_count(),
        });

    let contexts: Vec<ContextStatsResponse> = state
        .context_manager
        .all_context_stats()
        .into_iter()
        .map(|(id, snap, hit_rate)| ContextStatsResponse {
            id,
            hits: snap.hits,
            misses: snap.misses,
            queries: snap.queries,
            hit_rate,
        })
        .collect();

    let metrics_snap = state.metrics.snapshot();
    let response = StatsResponse {
        uptime_secs: state.start_time.elapsed().as_secs(),
        cache: CacheStatsResponse {
            total: cache_stats.total,
            fresh: cache_stats.fresh,
            stale: cache_stats.stale,
            expired: cache_stats.expired,
            max_entries: cache_stats.max_entries,
        },
        miss_reasons: MissReasonsResponse {
            not_in_cache: metrics_snap.miss_not_in_cache,
            expired: metrics_snap.miss_expired,
            upstream_error: metrics_snap.miss_upstream_error,
            circuit_open: metrics_snap.miss_circuit_open,
        },
        upstream,
        coalescer,
        refresh_worker,
        contexts,
    };

    (StatusCode::OK, Json(response))
}

// ============================================================================
// Metrics
// ============================================================================

/// Extended metrics response including adaptive timeout.
#[derive(Serialize)]
struct MetricsResponse {
    /// Core proxy metrics.
    proxy: super::super::metrics::MetricsSnapshot,
    /// All-request latency percentiles (cache hits + misses).
    latency_percentiles: super::super::metrics::LatencyPercentiles,
    /// Adaptive timeout statistics (full: p50, p95, p99, min, max, avg).
    adaptive_timeout: super::super::adaptive::AdaptiveTimeoutStats,
    /// Retry policy configuration.
    retry: RetryMetrics,
}

#[derive(Serialize)]
struct RetryMetrics {
    max_retries: u32,
    initial_delay_ms: u64,
    max_delay_ms: u64,
}

/// Handle GET /metrics requests.
///
/// Returns detailed performance metrics for monitoring.
pub(super) async fn handle_metrics(State(state): State<AppState>) -> impl IntoResponse {
    let snapshot = state.metrics.snapshot();
    let adaptive_stats = state.adaptive_timeout.stats();

    let latency_percentiles = state.metrics.latency_percentiles();

    let response = MetricsResponse {
        proxy: snapshot,
        latency_percentiles,
        adaptive_timeout: adaptive_stats,
        retry: RetryMetrics {
            max_retries: state.retry_policy.max_retries,
            initial_delay_ms: state.retry_policy.initial_delay.as_millis() as u64,
            max_delay_ms: state.retry_policy.max_delay.as_millis() as u64,
        },
    };

    (StatusCode::OK, Json(response))
}

/// Handle GET /metrics/prometheus requests.
///
/// Returns metrics in Prometheus/OpenMetrics text format for scraping.
/// This endpoint is suitable for Prometheus server to scrape directly.
pub(super) async fn handle_metrics_prometheus(State(state): State<AppState>) -> impl IntoResponse {
    let snapshot = state.metrics.snapshot();
    let cache_stats = state.cache.stats();

    // Get cache info from CacheStats
    let cache_entries = cache_stats.total as u64;
    let cache_memory = cache_stats.memory_bytes as u64;
    let cache_max = cache_stats.max_entries as u64;

    // Get pool stats if available
    let pool_guard = state.upstream_pool.load_full();
    let mut prometheus_text = if let Some(pool) = pool_guard.as_deref() {
        let pool_stats = pool.stats();
        let pool_params = super::super::metrics::PoolMetricsParams {
            total_upstreams: pool_stats.total_upstreams,
            healthy_upstreams: pool_stats.healthy_upstreams,
            fts_count: pool_stats.type_counts.fts,
            vector_db_count: pool_stats.type_counts.vector_db,
            hybrid_count: pool_stats.type_counts.hybrid,
            unknown_count: pool_stats.type_counts.unknown,
            active_connections: pool_stats.active_connections,
            pool_utilization: pool_stats.pool_utilization,
        };
        let base =
            snapshot.to_prometheus_with_pool(cache_entries, cache_memory, cache_max, &pool_params);

        // Gap 3: Per-upstream metrics
        let upstream_data: Vec<(String, u64, u64, f64, String)> = pool
            .all()
            .iter()
            .map(|u| {
                let status = match u.health.status() {
                    super::super::upstream::UpstreamStatus::Online => "online",
                    super::super::upstream::UpstreamStatus::Degraded => "degraded",
                    super::super::upstream::UpstreamStatus::Offline => "offline",
                };
                (
                    u.id.clone(),
                    u.request_count(),
                    u.failure_count(),
                    u.failure_rate(),
                    status.to_string(),
                )
            })
            .collect();
        let mut out = base;
        super::super::metrics::MetricsSnapshot::append_per_upstream_prometheus(
            &mut out,
            &upstream_data,
        );
        out
    } else {
        snapshot.to_prometheus_with_cache(cache_entries, cache_memory, cache_max)
    };

    // Gap 2: Adaptive timeout percentiles
    let adaptive_stats = state.adaptive_timeout.stats();
    super::super::metrics::MetricsSnapshot::append_adaptive_timeout_prometheus(
        &mut prometheus_text,
        &adaptive_stats,
    );

    // Gap 6: Circuit breaker state
    {
        use super::super::circuit::CircuitState;
        let circuit_state = state.circuit_breaker.state();
        let state_value = match circuit_state {
            CircuitState::Closed => 0u64,
            CircuitState::HalfOpen => 1,
            CircuitState::Open => 2,
        };
        super::super::metrics::MetricsSnapshot::append_circuit_breaker_prometheus(
            &mut prometheus_text,
            state_value,
            state.circuit_breaker.failure_count(),
            state.circuit_breaker.times_opened(),
            state.circuit_breaker.times_tripped(),
        );
    }

    // Gap 4: Request throughput
    let uptime_secs = state.start_time.elapsed().as_secs();
    super::super::metrics::MetricsSnapshot::append_throughput_prometheus(
        &mut prometheus_text,
        uptime_secs,
        snapshot.requests_total,
    );

    // Gap 5: All-request latency percentiles
    let latency_pcts = state.metrics.latency_percentiles();
    super::super::metrics::MetricsSnapshot::append_latency_percentiles_prometheus(
        &mut prometheus_text,
        &latency_pcts,
    );

    // Per-context cache metrics
    let context_entries: Vec<super::super::metrics::ContextMetricsEntry> = state
        .context_manager
        .all_context_stats()
        .into_iter()
        .map(
            |(id, snap, hit_rate)| super::super::metrics::ContextMetricsEntry {
                id,
                hits: snap.hits,
                misses: snap.misses,
                hit_rate,
            },
        )
        .collect();
    super::super::metrics::MetricsSnapshot::append_context_stats_prometheus(
        &mut prometheus_text,
        &context_entries,
    );

    // Per-agent metrics
    let registry_guard = state.agent_registry.load_full();
    if let Some(ref registry) = registry_guard {
        let agent_entries: Vec<super::super::metrics::AgentMetricsEntry> = registry
            .list_agents()
            .into_iter()
            .map(|info| super::super::metrics::AgentMetricsEntry {
                id: info.id,
                requests_total: info.quota.requests_total,
                cache_hits: info.quota.cache_hits,
                cache_misses: info.quota.cache_misses,
                rate_limited: info.quota.rate_limited,
                context_denied: info.quota.context_denied,
            })
            .collect();
        super::super::metrics::MetricsSnapshot::append_agent_metrics_prometheus(
            &mut prometheus_text,
            &agent_entries,
        );
    }

    // Queue metrics
    let queue_stats = state.request_queue.stats();
    super::super::metrics::MetricsSnapshot::append_queue_prometheus(
        &mut prometheus_text,
        &queue_stats,
    );

    // Client tracker metrics
    super::super::metrics::MetricsSnapshot::append_client_tracker_prometheus(
        &mut prometheus_text,
        state.client_tracker.active_count(),
        state
            .client_tracker
            .total_completed
            .load(std::sync::atomic::Ordering::Relaxed),
        state
            .client_tracker
            .total_rejected
            .load(std::sync::atomic::Ordering::Relaxed),
    );

    // Return with correct content type for Prometheus
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        prometheus_text,
    )
}

/// Handle `GET /debug/tokio` requests.
///
/// Returns a JSON snapshot of the current Tokio runtime's `RuntimeMetrics`:
/// alive task count, worker count, total busy time (sum across workers), global
/// queue depth. Always available (no `tokio_unstable` needed). The fields
/// `worker_poll_count`, `worker_mean_poll_time_ns`, and
/// `budget_forced_yield_count` are only included when built with
/// `RUSTFLAGS=--cfg tokio_unstable`. Per-task backtraces (with task names)
/// require `Handle::dump()` — see `/debug/tokio/dump` (unstable-gated).
pub(super) async fn handle_tokio_metrics(State(state): State<AppState>) -> impl IntoResponse {
    let Some(handle) = state.tokio_handle.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({
                "error": "tokio handle not available (test build?)"
            })),
        );
    };
    let m = handle.metrics();
    let nw = m.num_workers();
    // Sum stable worker_total_busy_duration across all workers.
    let total_busy_ns: u128 = (0..nw)
        .map(|i| m.worker_total_busy_duration(i).as_nanos())
        .sum();
    #[cfg_attr(not(tokio_unstable), allow(unused_mut))]
    let mut body = serde_json::json!({
        "schema_version": 1,
        "num_alive_tasks": m.num_alive_tasks(),
        "num_workers": nw,
        "worker_total_busy_duration_ns": total_busy_ns,
        "global_queue_depth": m.global_queue_depth(),
        "dropped_events_note": "Handle::dump() (per-task backtraces) at /debug/tokio/dump",
    });
    // Unstable fields — only included under RUSTFLAGS=--cfg tokio_unstable.
    // Arithmetic + expect() are only present in this cfg-gated block; the
    // allow suppresses them to keep clippy -D warnings clean.
    #[cfg(tokio_unstable)]
    #[allow(clippy::arithmetic_side_effects, clippy::expect_used)]
    {
        let total_poll_count: u64 = (0..nw).map(|i| m.worker_poll_count(i)).sum();
        let mean_poll_ns: u128 = if total_poll_count > 0 {
            total_busy_ns / u128::from(total_poll_count)
        } else {
            0
        };
        let obj = body.as_object_mut().expect("object");
        obj.insert(
            "worker_poll_count".into(),
            serde_json::json!(total_poll_count),
        );
        obj.insert(
            "worker_mean_poll_time_ns".into(),
            serde_json::json!(mean_poll_ns),
        );
        obj.insert(
            "budget_forced_yield_count".into(),
            serde_json::json!(m.budget_forced_yield_count()),
        );
    }
    (StatusCode::OK, Json(body))
}

/// Handle `GET /debug/tokio/dump` requests (one-shot).
///
/// Returns Tokio's full task backtrace dump in plain text. Requires
/// `RUSTFLAGS="--cfg tokio_unstable"` and the `taskdump` feature on the
/// `tokio` crate. Returns 503 if the runtime was not built with taskdump
/// support.
#[cfg_attr(not(all(tokio_unstable, feature = "tokio-taskdump")), allow(unused))]
pub(super) async fn handle_tokio_dump(State(state): State<AppState>) -> impl IntoResponse {
    #[cfg(all(tokio_unstable, feature = "tokio-taskdump"))]
    {
        let Some(handle) = state.tokio_handle.as_ref() else {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [(
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; charset=utf-8",
                )],
                "tokio handle not available (test build?)".to_string(),
            );
        };
        let dump = handle.dump().await;
        // Dump doesn't implement Display, only Debug. The output is verbose
        // but contains the full task set with names, IDs, and backtraces
        // (where available). The official tokio taskdump CLI shows a more
        // compact form; for now this is the machine-readable equivalent.
        let text = format!("{dump:#?}");
        (
            StatusCode::OK,
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )],
            text,
        )
    }
    #[cfg(not(all(tokio_unstable, feature = "tokio-taskdump")))]
    {
        let _ = state; // silence unused
        (
            StatusCode::SERVICE_UNAVAILABLE,
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )],
            "Task dump requires RUSTFLAGS=\"--cfg tokio_unstable\" + tokio feature \"taskdump\".\
             \n"
            .to_string(),
        )
    }
}

// ============================================================================
// Query Stats
// ============================================================================

/// Query stats response.
#[derive(Serialize)]
struct QueryStatsResponse {
    total_queries_tracked: usize,
    total_requests: u64,
    total_hits: u64,
    hit_rate: f64,
    avg_latency_ms: f64,
    hot_queries: Vec<HotQueryInfo>,
}

#[derive(Serialize)]
struct HotQueryInfo {
    query: String,
    count: u64,
    hits: u64,
    hit_rate: f64,
    avg_latency_ms: f64,
}

/// Handle GET /stats/queries requests.
///
/// Returns query access statistics including hot queries.
pub(super) async fn handle_query_stats(State(state): State<AppState>) -> impl IntoResponse {
    let aggregate = state.query_stats.aggregate();
    let hot_queries = state.query_stats.top_by_count(10);

    let hot_query_info: Vec<HotQueryInfo> = hot_queries
        .into_iter()
        .map(|stats| HotQueryInfo {
            query: stats.query,
            count: stats.count,
            hits: stats.hits,
            hit_rate: stats.hit_rate,
            avg_latency_ms: stats.avg_latency_ms,
        })
        .collect();

    let response = QueryStatsResponse {
        total_queries_tracked: aggregate.unique_queries,
        total_requests: aggregate.total_requests,
        total_hits: aggregate.total_hits,
        hit_rate: aggregate.hit_rate,
        avg_latency_ms: aggregate.avg_latency_ms,
        hot_queries: hot_query_info,
    };

    (StatusCode::OK, Json(response))
}

// ============================================================================
// Audit Log
// ============================================================================

/// Audit log response.
#[derive(Serialize)]
struct AuditResponse {
    total_logged: u64,
    entries_in_buffer: usize,
    success_rate: f64,
    cache_hit_rate: f64,
    avg_latency_ms: u64,
    recent_entries: Vec<AuditEntryResponse>,
}

#[derive(Serialize)]
struct AuditEntryResponse {
    request_id: u64,
    timestamp: u64,
    query: String,
    cache_status: String,
    latency_ms: u64,
    result_count: usize,
    success: bool,
    error: Option<String>,
}

/// Handle GET /audit requests.
///
/// Returns recent request audit log entries.
pub(super) async fn handle_audit(State(state): State<AppState>) -> impl IntoResponse {
    let stats = state.audit_log.stats();
    let entries = state.audit_log.recent(50);

    let recent_entries: Vec<AuditEntryResponse> = entries
        .into_iter()
        .map(|e| AuditEntryResponse {
            request_id: e.request_id,
            timestamp: e.timestamp,
            query: e.query.clone(),
            cache_status: format!("{:?}", e.cache_status),
            latency_ms: e.latency_ms,
            result_count: e.result_count,
            success: e.success,
            error: e.error.clone(),
        })
        .collect();

    let response = AuditResponse {
        total_logged: stats.total_logged,
        entries_in_buffer: stats.entries_in_buffer,
        success_rate: stats.success_rate,
        cache_hit_rate: stats.cache_hit_rate,
        avg_latency_ms: stats.avg_latency_ms,
        recent_entries,
    };

    (StatusCode::OK, Json(response))
}

// ============================================================================
// Circuit Breaker
// ============================================================================

/// Circuit breaker status response.
#[derive(Serialize)]
struct CircuitStatusResponse {
    state: String,
    failure_count: u32,
    times_opened: u64,
    times_tripped: u64,
}

/// Handle GET /circuit requests.
///
/// Returns circuit breaker status.
pub(super) async fn handle_circuit_status(State(state): State<AppState>) -> impl IntoResponse {
    use super::super::circuit::CircuitState;

    let circuit_state = state.circuit_breaker.state();
    let state_str = match circuit_state {
        CircuitState::Closed => "closed",
        CircuitState::Open => "open",
        CircuitState::HalfOpen => "half_open",
    };

    let response = CircuitStatusResponse {
        state: state_str.to_string(),
        failure_count: state.circuit_breaker.failure_count(),
        times_opened: state.circuit_breaker.times_opened(),
        times_tripped: state.circuit_breaker.times_tripped(),
    };

    (StatusCode::OK, Json(response))
}

// ============================================================================
// Queue Status
// ============================================================================

/// Queue status response.
#[derive(Serialize)]
struct QueueStatusResponse {
    /// Total items in queue.
    pub total: usize,
    /// Maximum queue size.
    pub max_size: usize,
    /// Items by priority level.
    pub by_priority: QueuePriorityBreakdown,
    /// Queue utilization (0.0 to 1.0).
    pub utilization: f64,
}

#[derive(Serialize)]
struct QueuePriorityBreakdown {
    low: usize,
    normal: usize,
    high: usize,
    critical: usize,
}

/// Handle GET /queue requests.
///
/// Returns request queue status and statistics.
pub(super) async fn handle_queue_status(State(state): State<AppState>) -> impl IntoResponse {
    let stats = state.request_queue.stats();

    let response = QueueStatusResponse {
        total: stats.total,
        max_size: stats.max_size,
        by_priority: QueuePriorityBreakdown {
            low: stats.low_priority,
            normal: stats.normal_priority,
            high: stats.high_priority,
            critical: stats.critical_priority,
        },
        utilization: stats.utilization(),
    };

    (StatusCode::OK, Json(response))
}

// ============================================================================
// Readiness Probe
// ============================================================================

/// Readiness response.
#[derive(Serialize)]
struct ReadyResponse {
    ready: bool,
    cache_populated: bool,
    upstream_reachable: Option<bool>,
    /// Number of healthy upstreams in pool (if pool configured).
    pool_healthy_count: Option<usize>,
    reason: Option<&'static str>,
}

/// Handle GET /ready requests.
///
/// Readiness probe for Kubernetes. Returns 200 if ready, 503 if not.
pub(super) async fn handle_ready(State(state): State<AppState>) -> impl IntoResponse {
    let cache_populated = !state.cache.is_empty();

    // Check pool or single upstream
    let pool = state.upstream_pool.load_full();
    let single = state.upstream.load_full();
    let (upstream_configured, upstream_reachable, pool_healthy_count) = if let Some(ref pool) = pool
    {
        // Pool configured - check if any upstream is healthy
        let stats = pool.stats();
        let has_healthy = stats.healthy_upstreams > 0;
        (true, Some(has_healthy), Some(stats.healthy_upstreams))
    } else if let Some(ref upstream) = single {
        // Single upstream - do health check
        let healthy = match upstream.health_check().await {
            Ok(healthy) => Some(healthy),
            Err(_) => Some(false),
        };
        (true, healthy, None)
    } else {
        (false, None, None)
    };

    // Check peer state: if peer is warming (snapshot in progress), not ready
    if state.peer_is_warming() {
        let response = ReadyResponse {
            ready: false,
            cache_populated,
            upstream_reachable,
            pool_healthy_count,
            reason: Some("Peer replication: warming (snapshot in progress)"),
        };
        return (StatusCode::SERVICE_UNAVAILABLE, Json(response));
    }

    // Determine readiness
    let (ready, reason) = match (upstream_configured, upstream_reachable, cache_populated) {
        // No upstream configured - ready if cache can serve
        (false, _, true) => (true, None),
        (false, _, false) => (false, Some("No upstream and cache is empty")),
        // Upstream configured and reachable
        (true, Some(true), _) => (true, None),
        // Upstream configured but not reachable, but we have cached data
        (true, Some(false), true) => (true, None), // Can serve from cache
        // Upstream configured but not reachable and no cache
        (true, Some(false), false) => (false, Some("Upstream unreachable and cache is empty")),
        (true, None, _) => (false, Some("Upstream health check failed")),
    };

    let response = ReadyResponse {
        ready,
        cache_populated,
        upstream_reachable,
        pool_healthy_count,
        reason,
    };

    if ready {
        (StatusCode::OK, Json(response))
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(response))
    }
}

// ============================================================================
// Pool Status
// ============================================================================

/// Pool status response.
#[derive(Serialize)]
struct PoolStatusResponse {
    /// Whether a pool is configured.
    pool_configured: bool,
    /// Pool statistics (if configured).
    stats: Option<PoolStats>,
    /// Per-upstream status.
    upstreams: Vec<UpstreamStatusInfo>,
    /// Load balancing strategy.
    strategy: Option<String>,
}

/// Per-upstream status information.
#[derive(Serialize)]
struct UpstreamStatusInfo {
    id: String,
    url: String,
    status: String,
    enabled: bool,
    weight: u32,
    priority: u32,
    requests: u64,
    failures: u64,
    failure_rate: f64,
}

/// Handle GET /pool requests.
///
/// Returns upstream pool status and statistics.
pub(super) async fn handle_pool_status(State(state): State<AppState>) -> impl IntoResponse {
    let pool = state.upstream_pool.load_full();
    let single = state.upstream.load_full();
    match pool.as_deref() {
        Some(pool) => {
            let stats = pool.stats();
            let upstreams: Vec<UpstreamStatusInfo> = pool
                .all()
                .iter()
                .map(|u| {
                    let status = match u.health.status() {
                        super::super::upstream::UpstreamStatus::Online => "online",
                        super::super::upstream::UpstreamStatus::Degraded => "degraded",
                        super::super::upstream::UpstreamStatus::Offline => "offline",
                    };
                    UpstreamStatusInfo {
                        id: u.id.clone(),
                        url: u.adapter.identifier().to_string(),
                        status: status.to_string(),
                        enabled: u.is_available(),
                        weight: u.weight,
                        priority: u.priority,
                        requests: u.request_count(),
                        failures: u.failure_count(),
                        failure_rate: u.failure_rate(),
                    }
                })
                .collect();

            let response = PoolStatusResponse {
                pool_configured: true,
                stats: Some(stats),
                upstreams,
                strategy: Some(pool.strategy().as_str().to_string()),
            };

            (StatusCode::OK, Json(response))
        }
        None => {
            // No pool configured - check for single upstream
            let upstreams = if let Some(ref upstream) = single {
                vec![UpstreamStatusInfo {
                    id: "default".to_string(),
                    url: upstream.base_url().to_string(),
                    status: "online".to_string(), // Assume online for single upstream
                    enabled: true,
                    weight: 1,
                    priority: 0,
                    requests: 0, // Single upstream doesn't track this
                    failures: 0,
                    failure_rate: 0.0,
                }]
            } else {
                vec![]
            };

            let response = PoolStatusResponse {
                pool_configured: false,
                stats: None,
                upstreams,
                strategy: None,
            };

            (StatusCode::OK, Json(response))
        }
    }
}

// ============================================================================
// Client Connections
// ============================================================================

/// Response for the GET /clients endpoint.
#[derive(Debug, Serialize)]
pub(super) struct ClientsResponse {
    active_requests: usize,
    total_completed: u64,
    total_rejected: u64,
    clients: Vec<ClientInfo>,
}

/// Return active client connections and aggregate request counters.
pub(super) async fn handle_clients(State(state): State<AppState>) -> impl IntoResponse {
    let clients = state.client_tracker.snapshot();
    Json(ClientsResponse {
        active_requests: clients.len(),
        total_completed: state
            .client_tracker
            .total_completed
            .load(std::sync::atomic::Ordering::Relaxed),
        total_rejected: state
            .client_tracker
            .total_rejected
            .load(std::sync::atomic::Ordering::Relaxed),
        clients,
    })
}
