//! Cache management handlers for the proxy server.
//!
//! Handles cache clear, integrity, eviction, warmup, and upstream stats endpoints.

use super::*;

/// Cache clear response.
#[derive(Serialize)]
struct CacheClearResponse {
    cleared_entries: usize,
    message: &'static str,
}

/// Handle POST /cache/clear requests.
///
/// Clears all entries from the cache. Use with caution in production.
pub(super) async fn handle_cache_clear(State(state): State<AppState>) -> impl IntoResponse {
    let count = state.cache.len();
    state.cache.clear();

    let response = CacheClearResponse {
        cleared_entries: count,
        message: "Cache cleared successfully",
    };

    (StatusCode::OK, Json(response))
}

/// Handle GET /cache/integrity requests.
///
/// Runs integrity verification on all cache entries.
pub(super) async fn handle_cache_integrity(State(state): State<AppState>) -> impl IntoResponse {
    let report = state.cache.verify_integrity();

    // Record any integrity failures in metrics
    if report.invalid > 0 {
        for _ in 0..report.invalid {
            state.metrics.record_integrity_failure();
        }
    }

    (StatusCode::OK, Json(report))
}

/// Request body for selective cache eviction.
#[derive(Debug, Clone, serde::Deserialize)]
pub(super) struct EvictRequest {
    /// Upstream ID to evict entries from (optional).
    #[serde(default)]
    pub(super) upstream_id: Option<String>,
    /// Maximum number of entries to evict (optional).
    #[serde(default)]
    pub(super) max_entries: Option<usize>,
    /// Evict only expired entries.
    #[serde(default)]
    pub(super) expired_only: bool,
}

/// Response for cache eviction.
#[derive(Serialize)]
struct EvictResponse {
    /// Number of entries evicted.
    evicted: usize,
    /// Entries remaining in cache.
    remaining: usize,
}

/// Handle POST /cache/evict requests.
///
/// Selectively evicts cache entries based on criteria.
pub(super) async fn handle_cache_evict(
    State(state): State<AppState>,
    Json(request): Json<EvictRequest>,
) -> impl IntoResponse {
    let evicted = if request.expired_only {
        // Evict only truly expired entries
        state.cache.evict_truly_expired()
    } else if let Some(upstream_id) = &request.upstream_id {
        // Evict entries from a specific upstream
        if let Some(max) = request.max_entries {
            // Enforce a limit on this upstream
            state.cache.enforce_per_upstream_limit(
                upstream_id,
                state
                    .cache
                    .count_for_upstream(upstream_id)
                    .saturating_sub(max),
            )
        } else {
            // Evict all from this upstream
            let count = state.cache.count_for_upstream(upstream_id);
            state.cache.evict_from_upstream(upstream_id, count)
        }
    } else {
        // No specific criteria - only evict expired
        state.cache.evict_truly_expired()
    };

    (
        StatusCode::OK,
        Json(EvictResponse {
            evicted,
            remaining: state.cache.len(),
        }),
    )
}

/// Request body for cache warmup.
#[derive(Debug, Clone, serde::Deserialize)]
pub(super) struct WarmupRequest {
    /// Queries to pre-fetch and cache.
    pub(super) queries: Vec<String>,
    /// Whether to fetch from upstream (default: true).
    #[serde(default = "default_true")]
    pub(super) fetch_from_upstream: bool,
}

fn default_true() -> bool {
    true
}

/// Response for cache warmup.
#[derive(Serialize)]
struct WarmupResponse {
    /// Number of queries warmed.
    warmed: usize,
    /// Number of queries that failed.
    failed: usize,
    /// Total time in milliseconds.
    took_ms: u64,
}

/// Handle POST /cache/warmup requests.
///
/// Pre-fetches queries and populates the cache.
pub(super) async fn handle_cache_warmup(
    State(state): State<AppState>,
    Json(request): Json<WarmupRequest>,
) -> impl IntoResponse {
    let start = Instant::now();
    state.metrics.record_warmup_request();

    if !request.fetch_from_upstream {
        // Just acknowledge - nothing to warm without upstream fetch
        return (
            StatusCode::OK,
            Json(WarmupResponse {
                warmed: 0,
                failed: 0,
                took_ms: start.elapsed().as_millis() as u64,
            }),
        );
    }

    // Need at least one upstream source (pool or single)
    let has_upstream =
        state.upstream_pool.load_full().is_some() || state.upstream.load_full().is_some();
    if !has_upstream {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(WarmupResponse {
                warmed: 0,
                failed: request.queries.len(),
                took_ms: start.elapsed().as_millis() as u64,
            }),
        );
    }

    let mut warmed: usize = 0;
    let mut failed: usize = 0;
    let warmup_ctx_id = state.context_manager.current();

    for query in request.queries {
        let warmup_ctx_q = context_query(&warmup_ctx_id, &query);
        // Skip if already in cache and fresh
        if let Some(Freshness::Fresh) = state.cache.check_freshness(&warmup_ctx_q) {
            warmed = warmed.saturating_add(1);
            continue;
        }

        // Fetch from upstream
        let req = QueryRequest {
            query: query.clone(),
            top_k: None,
            priority: None,
            upstream_id: None,
            upstream_type: None,
        };

        // Use pool if available, otherwise single upstream
        let pool = state.upstream_pool.load_full();
        let upstream = state.upstream.load_full();
        let result = if let Some(ref pool) = pool {
            pool.query(&req).await
        } else if let Some(ref upstream) = upstream {
            upstream.query(&req).await
        } else {
            unreachable!("checked has_upstream above")
        };

        match result {
            Ok(mut response) => {
                // Apply scope filtering (per-context policy)
                let scope_filter = state.scope_filter_for(&warmup_ctx_id);
                response.results = super::query_core::apply_scope_filter(
                    &scope_filter,
                    response.results,
                    #[cfg(feature = "embed-api")]
                    state.smart_embedder.as_deref(),
                )
                .await;

                // Cache if valid (context-isolated key)
                if response.validate().is_ok() {
                    state.cache.insert_with_context(
                        &warmup_ctx_q,
                        response,
                        state.upstream_id.clone(),
                        &warmup_ctx_id,
                    );
                    state.metrics.record_warmup_entry();
                    warmed = warmed.saturating_add(1);
                } else {
                    failed = failed.saturating_add(1);
                }
            }
            Err(_) => {
                failed = failed.saturating_add(1);
            }
        }
    }

    (
        StatusCode::OK,
        Json(WarmupResponse {
            warmed,
            failed,
            took_ms: start.elapsed().as_millis() as u64,
        }),
    )
}

/// Handle GET /cache/upstreams requests.
///
/// Returns cache statistics broken down by upstream.
pub(super) async fn handle_cache_upstreams(State(state): State<AppState>) -> impl IntoResponse {
    let stats_by_upstream = state.cache.stats_by_upstream();

    #[derive(Serialize)]
    struct UpstreamStats {
        total: usize,
        fresh: usize,
        stale: usize,
        expired: usize,
        memory_bytes: usize,
    }

    let response: std::collections::HashMap<String, UpstreamStats> = stats_by_upstream
        .into_iter()
        .map(|(id, stats)| {
            (
                id,
                UpstreamStats {
                    total: stats.total,
                    fresh: stats.fresh,
                    stale: stats.stale,
                    expired: stats.expired,
                    memory_bytes: stats.memory_bytes,
                },
            )
        })
        .collect();

    (StatusCode::OK, Json(response))
}

/// Response wrapper for `GET /cache/entries`.
#[derive(Serialize)]
struct CacheEntriesResponse {
    total: usize,
    entries: Vec<crate::proxy::cache::CacheEntrySummary>,
}

/// Handle GET /cache/entries requests.
///
/// Returns a lightweight per-entry summary so the agent can list what's in the
/// cache. Excludes the full response payload to keep the payload compact.
pub(super) async fn handle_cache_entries(State(state): State<AppState>) -> impl IntoResponse {
    let entries = state.cache.entries_summary();
    let total = entries.len();
    (
        StatusCode::OK,
        Json(CacheEntriesResponse { total, entries }),
    )
}
