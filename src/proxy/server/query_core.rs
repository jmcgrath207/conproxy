//! Protocol-agnostic query execution core.
//!
//! Extracts the business logic from HTTP handlers so both HTTP (axum) and gRPC (tonic)
//! transports can share the same code path. Each function takes plain Rust types and
//! returns `Result<T, QueryError>`, leaving serialization to the transport layer.

use std::sync::Arc;
use std::time::Instant;

use super::{context_query, AppState};
use crate::proxy::agent::AgentIdentity;
use crate::proxy::cache::CacheStore;
use crate::proxy::coalesce::CoalesceAction;
use crate::proxy::lifecycle::ProxyError;
use crate::proxy::types::{
    CacheStatus, CachedResponse, Freshness, MissReason, QueryRequest, QueryResponse,
};

#[cfg(feature = "embed-api")]
use super::query::query_with_mode;

use crate::proxy::cascade::CascadeExecutor;
use crate::proxy::pool::UpstreamPool;
use crate::proxy::scope::ScopeFilter;
use crate::proxy::types::SearchResult;
use crate::proxy::upstream::GenericRestAdapter;

/// Apply scope filter; with `embed-api` + SmartEmbedder, hybrid score when possible.
pub(crate) async fn apply_scope_filter(
    scope_filter: &ScopeFilter,
    results: Vec<SearchResult>,
    #[cfg(feature = "embed-api")] embedder: Option<&crate::proxy::smart_embedder::SmartEmbedder>,
) -> Vec<SearchResult> {
    if !scope_filter.is_enabled() {
        return results;
    }

    #[cfg(feature = "embed-api")]
    if let Some(emb) = embedder {
        let phrases = scope_filter.phrases();
        if !phrases.is_empty() && !results.is_empty() {
            let phrase_refs: Vec<&str> = phrases.iter().map(String::as_str).collect();
            let content_refs: Vec<&str> = results.iter().map(|r| r.content.as_str()).collect();
            let (phrase_res, content_res) = tokio::join!(
                emb.embed_batch(&phrase_refs),
                emb.embed_batch(&content_refs),
            );
            if let (Ok(phrase_embs), Ok(content_embs)) = (phrase_res, content_res) {
                let hybrid = scope_filter.clone().with_phrase_embeddings(phrase_embs);
                return hybrid.filter_results_hybrid(results, Some(&content_embs));
            }
        }
    }

    scope_filter.filter_results(results)
}

/// Dispatch a query to the appropriate upstream (cascade > pool > single).
///
/// Extracted as a standalone function so tarpaulin can instrument the logic
/// (async closures passed to retry executors and batch processors are opaque to tarpaulin).
/// Used by both `execute_query` (via RetryExecutor) and `handle_batch`.
pub(crate) async fn dispatch_upstream_query(
    cascade: &Option<Arc<CascadeExecutor>>,
    pool: &Option<Arc<UpstreamPool>>,
    upstream: &Option<Arc<GenericRestAdapter>>,
    request: &QueryRequest,
    #[cfg(feature = "embed-api")] smart_embedder: &Option<
        Arc<crate::proxy::smart_embedder::SmartEmbedder>,
    >,
    global_concurrency: &tokio::sync::Semaphore,
) -> Result<QueryResponse, crate::proxy::upstream::UpstreamError> {
    // Acquire global concurrency permit (fail-fast if exhausted)
    let _permit = global_concurrency.try_acquire().map_err(|_| {
        crate::proxy::upstream::UpstreamError::Unavailable(
            "Global connection limit reached".to_string(),
        )
    })?;

    if let Some(ref cascade) = cascade {
        let cr = cascade.query(request).await;
        // Classify cascade exhaustion:
        // - Timeout / NoUpstreams → hard upstream failure → 502
        // - AllExhausted with every upstream errored → upstream failure → 502
        //   (the cascade treated each error as "try next upstream", but every
        //    upstream failed — this is not a valid empty miss)
        // - AllExhausted with at least one upstream returning Ok (empty
        //   results) → legitimate empty miss → 200 with empty results
        let all_errored = !cr.upstream_scores.is_empty()
            && cr.upstream_scores.iter().all(|s| s.error.is_some());
        let upstream_failure = matches!(
            cr.stop_reason,
            crate::proxy::cascade::CascadeStopReason::Timeout
                | crate::proxy::cascade::CascadeStopReason::NoUpstreams
        ) || (matches!(
            cr.stop_reason,
            crate::proxy::cascade::CascadeStopReason::AllExhausted
        ) && all_errored);
        if upstream_failure {
            Err(crate::proxy::upstream::UpstreamError::Unavailable(
                "All upstreams failed in cascade".to_string(),
            ))
        } else {
            Ok(QueryResponse {
                results: cr.results,
                cache_status: CacheStatus::Miss,
                took_ms: cr.cascade_time_ms,
                generated_at: None,
                miss_reason: None,
            })
        }
    } else if let Some(ref p) = pool {
        p.query(request).await
    } else if let Some(ref u) = upstream {
        let mut resp = {
            #[cfg(feature = "embed-api")]
            {
                query_with_mode(u, request, smart_embedder).await
            }
            #[cfg(not(feature = "embed-api"))]
            {
                u.query(request).await
            }
        };
        if let Ok(ref mut response) = resp {
            for r in &mut response.results {
                if r.upstream_id.is_none() {
                    r.upstream_id = Some("default".into());
                }
            }
        }
        resp
    } else {
        Err(crate::proxy::upstream::UpstreamError::NotConfigured)
    }
}

/// The result of a query execution.
pub(crate) struct QueryResult {
    pub response: CachedResponse,
    /// HTTP-equivalent status: 200, 502, 503, etc.
    pub status: u16,
}

impl QueryResult {
    fn ok(response: CachedResponse) -> Self {
        Self {
            response,
            status: 200,
        }
    }

    fn with_status(response: CachedResponse, status: u16) -> Self {
        Self { response, status }
    }
}

/// Construct an empty miss response for error/rejection paths.
///
/// `vec![]` with zero capacity does not allocate, so this is cheap.
fn empty_miss_response(took_ms: u64, miss_reason: Option<MissReason>) -> QueryResponse {
    QueryResponse {
        results: vec![],
        cache_status: CacheStatus::Miss,
        took_ms,
        generated_at: None,
        miss_reason,
    }
}

/// Execute a single query against the cache/upstream pipeline.
///
/// This is the protocol-agnostic core shared by HTTP and gRPC handlers.
pub(crate) async fn execute_query(
    state: &AppState,
    request: QueryRequest,
    context_id: String,
    request_id: String,
    agent: Option<&AgentIdentity>,
    source: String,
) -> QueryResult {
    let start = Instant::now();

    // Reject queries when proxy is paused (pgbouncer PAUSE parity)
    if state.paused.load(std::sync::atomic::Ordering::Relaxed) {
        state.client_tracker.reject();
        // Cold path: elapsed() call acceptable
        return QueryResult::with_status(
            CachedResponse::Fresh(empty_miss_response(
                start.elapsed().as_millis() as u64,
                None,
            )),
            503,
        );
    }

    // Validate request first
    if request.validate().is_err() {
        state.metrics.record_validation_error();
        // Cold path: elapsed() call acceptable
        return QueryResult::with_status(
            CachedResponse::Fresh(empty_miss_response(
                start.elapsed().as_millis() as u64,
                None,
            )),
            400,
        );
    }

    // Track active client request
    state
        .client_tracker
        .track(request_id.clone(), request.query.clone(), source);

    // Ensure context exists (auto-creates if configured)
    if state.context_manager.get(&context_id).is_none() {
        if let Err(e) = state.context_manager.switch(&context_id) {
            tracing::warn!(error = %e, context = %context_id, "Failed to auto-create context");
        }
    }

    // Enforce context binding for agents with restricted contexts
    if let Some(agent) = agent {
        if !agent.can_access_context(&context_id) {
            state.client_tracker.complete(&request_id);
            // Cold path: elapsed() call acceptable
            return QueryResult::with_status(
                CachedResponse::Fresh(empty_miss_response(
                    start.elapsed().as_millis() as u64,
                    None,
                )),
                403,
            );
        }
    }

    let ctx_query = context_query(&context_id, &request.query);
    let query_hash = CacheStore::hash_query(&ctx_query);
    let mut miss_reason = MissReason::NotInCache;

    // Check cache first
    if let Some(freshness) = state.cache.check_freshness_by_hash(&query_hash) {
        match freshness {
            Freshness::Fresh => {
                if let Some(entry) = state.cache.get_by_hash(&query_hash) {
                    state.metrics.record_hit();
                    state.context_manager.record_hit_for(&context_id);
                    let elapsed = start.elapsed();
                    state.metrics.record_latency(elapsed);
                    state.query_stats.record(&request.query, true, elapsed);
                    let took_ms = elapsed.as_millis() as u64;
                    state.client_tracker.complete(&request_id);
                    return QueryResult::ok(CachedResponse::from_cache(
                        entry,
                        CacheStatus::Hit,
                        took_ms,
                    ));
                }
            }
            Freshness::Stale => {
                if let Some(entry) = state.cache.get_by_hash(&query_hash) {
                    state.metrics.record_stale();
                    // PERF(R2): required for tokio::spawn 'static bound
                    let cache = state.cache.clone();
                    let upstream = state.upstream.load_full();
                    let query = request.query.clone();
                    let ctx_id_bg = context_id.clone();
                    let upstream_id = state.upstream_id.clone();
                    let refresh_worker = state.refresh_worker.clone();
                    let scope_filter = state.scope_filter_for(&context_id);
                    #[cfg(feature = "embed-api")]
                    let smart_embedder_bg = state.smart_embedder.clone();

                    tokio::spawn(async move {
                        if let Some(upstream) = upstream {
                            if let Ok(mut new_response) = upstream
                                .query(&QueryRequest {
                                    query: query.clone(),
                                    top_k: None,
                                    priority: None,
                                    upstream_id: None,
                                    upstream_type: None,
                                })
                                .await
                            {
                                new_response.results = apply_scope_filter(
                                    &scope_filter,
                                    new_response.results,
                                    #[cfg(feature = "embed-api")]
                                    smart_embedder_bg.as_deref(),
                                )
                                .await;
                                if new_response.validate().is_ok() {
                                    let bg_ctx_query = context_query(&ctx_id_bg, &query);
                                    cache.insert_with_context(
                                        &bg_ctx_query,
                                        new_response,
                                        upstream_id,
                                        &ctx_id_bg,
                                    );
                                    if let Some(ref worker) = refresh_worker {
                                        worker.register_query(&bg_ctx_query);
                                    }
                                }
                            }
                        }
                    });

                    let elapsed = start.elapsed();
                    state.metrics.record_latency(elapsed);
                    state.query_stats.record(&request.query, true, elapsed);
                    let took_ms = elapsed.as_millis() as u64;
                    state.client_tracker.complete(&request_id);
                    return QueryResult::ok(CachedResponse::from_cache(
                        entry,
                        CacheStatus::Stale,
                        took_ms,
                    ));
                }
            }
            Freshness::Expired | Freshness::Frozen => {
                miss_reason = MissReason::Expired;
            }
        }
    }

    // Semantic cache tier lookup — only on exact miss, requires embedder.
    // Compute embedding once and scan the tier for a similar past query.
    // Freshness is checked before serving: Expired entries are skipped.
    // The embedding is stashed in `query_embedding` so the leader branch below
    // can reuse it for the semantic insert (avoids a second inference call).
    #[cfg(feature = "embed-api")]
    let mut query_embedding: Option<Vec<f32>> = None;

    #[cfg(feature = "embed-api")]
    if let (Some(semantic), Some(embedder)) =
        (state.semantic_cache.as_ref(), state.smart_embedder.as_ref())
    {
        match embedder.embed(&ctx_query).await {
            Ok(embedding) => {
                if let Some(matched_hash) = semantic.lookup(&embedding) {
                    // Check freshness before serving — skip expired entries
                    let fresh = state.cache.check_freshness_by_hash(&matched_hash);
                    if matches!(
                        fresh,
                        Some(Freshness::Fresh) | Some(Freshness::Stale) | Some(Freshness::Frozen)
                    ) {
                        if let Some(entry) = state.cache.get_by_hash(&matched_hash) {
                            state.metrics.record_semantic_hit();
                            state.context_manager.record_hit_for(&context_id);
                            let elapsed = start.elapsed();
                            state.metrics.record_latency(elapsed);
                            state.query_stats.record(&request.query, true, elapsed);
                            let took_ms = elapsed.as_millis() as u64;
                            state.client_tracker.complete(&request_id);
                            return QueryResult::ok(CachedResponse::from_cache(
                                entry,
                                CacheStatus::Hit,
                                took_ms,
                            ));
                        }
                    }
                    // Expired or missing — record miss and continue to upstream
                    state.metrics.record_semantic_miss();
                } else {
                    state.metrics.record_semantic_miss();
                }
                // Stash for reuse in the leader branch below (avoids 2nd embed call).
                query_embedding = Some(embedding);
            }
            Err(_) => {
                // Embedding failed — record as miss so metrics reflect no semantic coverage
                state.metrics.record_semantic_miss();
            }
        }
    }

    // Cache miss or expired - use coalescer to deduplicate concurrent requests
    // (query_hash already computed above — R8: single blake3 call per request)

    // Check if we have any upstream (cascade > pool > single)
    let has_upstream = state.cascade_executor.load_full().is_some()
        || state.upstream_pool.load_full().is_some()
        || state.upstream.load_full().is_some();

    let result = if has_upstream {
        // Check circuit breaker before making upstream request
        if !state.circuit_breaker.allow_request() {
            if let Some(entry) = state.cache.get_by_hash(&query_hash) {
                state.metrics.record_frozen();
                let took_ms = start.elapsed().as_millis() as u64;
                state.client_tracker.complete(&request_id);
                return QueryResult::ok(CachedResponse::from_cache(
                    entry,
                    CacheStatus::Frozen,
                    took_ms,
                ));
            }
            state.metrics.record_miss(MissReason::CircuitOpen);
            state.client_tracker.complete(&request_id);
            // Cold path: elapsed() call acceptable
            return QueryResult::with_status(
                CachedResponse::Fresh(empty_miss_response(
                    start.elapsed().as_millis() as u64,
                    Some(MissReason::CircuitOpen),
                )),
                503,
            );
        }

        match state.coalescer.get_or_insert(query_hash) {
            CoalesceAction::Leader => {
                state.metrics.record_upstream_request();

                let executor =
                    crate::proxy::retry::RetryExecutor::new((*state.retry_policy).clone());
                let cascade_clone = state.cascade_executor.load_full();
                let pool_clone = state.upstream_pool.load_full();
                let upstream_clone = state.upstream.load_full();
                let request_clone = request.clone();
                let global_concurrency_clone = state.global_concurrency.clone();

                #[cfg(feature = "embed-api")]
                let smart_embedder_clone = state.smart_embedder.clone();

                let retry_result = executor
                    .execute(|_attempt| {
                        let cascade = cascade_clone.clone();
                        let pool = pool_clone.clone();
                        let upstream = upstream_clone.clone();
                        let r = request_clone.clone();
                        let gc = global_concurrency_clone.clone();
                        #[cfg(feature = "embed-api")]
                        let embedder = smart_embedder_clone.clone();

                        async move {
                            dispatch_upstream_query(
                                &cascade,
                                &pool,
                                &upstream,
                                &r,
                                #[cfg(feature = "embed-api")]
                                &embedder,
                                &gc,
                            )
                            .await
                        }
                    })
                    .await;

                if retry_result.is_success() {
                    state.adaptive_timeout.record(retry_result.total_duration);
                }
                if retry_result.retried() {
                    state.metrics.record_upstream_request();
                }

                let result = retry_result.into_result();

                match result {
                    Ok(mut response) => {
                        state.metrics.record_miss(miss_reason);
                        state.context_manager.record_miss_for(&context_id);
                        state.circuit_breaker.record_success();

                        let scope_filter = state.scope_filter_for(&context_id);
                        response.results = apply_scope_filter(
                            &scope_filter,
                            response.results,
                            #[cfg(feature = "embed-api")]
                            state.smart_embedder.as_deref(),
                        )
                        .await;

                        response.cache_status = CacheStatus::Miss;
                        response.miss_reason = Some(miss_reason);
                        if response.validate().is_err() {
                            state.metrics.record_validation_error();
                            let elapsed = start.elapsed();
                            response.took_ms = elapsed.as_millis() as u64;
                            let response_arc = Arc::new(response);
                            state
                                .coalescer
                                .complete(&query_hash, Ok(Arc::clone(&response_arc)));
                            state.metrics.record_latency(elapsed);
                            state.client_tracker.complete(&request_id);
                            return QueryResult::ok(CachedResponse::SharedFresh {
                                response: response_arc,
                                took_ms: elapsed.as_millis() as u64,
                            });
                        }

                        // R8: use insert_by_hash to skip redundant blake3 re-hash of ctx_query
                        // R2: response.clone() still required — cache takes ownership,
                        //     response used below for Arc broadcast.
                        //     Systemic fix (Arc<QueryResponse> in CacheEntry) deferred: >5 callers.
                        // Reuse the embedding stashed from the semantic lookup above
                        // (compute only if we didn't already — single inference per miss).
                        #[cfg(feature = "embed-api")]
                        let query_embedding: Option<Vec<f32>> = if query_embedding.is_some() {
                            query_embedding
                        } else if state.semantic_cache.is_some() {
                            if let Some(embedder) = state.smart_embedder.as_ref() {
                                embedder.embed(&ctx_query).await.ok()
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        #[cfg(not(feature = "embed-api"))]
                        let _query_embedding: Option<Vec<f32>> = None;

                        #[cfg(feature = "embed-api")]
                        {
                            state.cache.insert_by_hash_with_embedding(
                                query_hash,
                                response.clone(),
                                state.upstream_id.clone(),
                                query_embedding,
                                Some(&ctx_query),
                            );
                        }
                        #[cfg(not(feature = "embed-api"))]
                        {
                            state.cache.insert_by_hash(
                                query_hash,
                                response.clone(),
                                state.upstream_id.clone(),
                                Some(&ctx_query),
                            );
                        }

                        if let Some(ref worker) = state.refresh_worker {
                            worker.register_query(&ctx_query);
                        }

                        let elapsed = start.elapsed();
                        response.took_ms = elapsed.as_millis() as u64;
                        let response_arc = Arc::new(response);
                        state
                            .coalescer
                            .complete(&query_hash, Ok(Arc::clone(&response_arc)));
                        state.metrics.record_latency(elapsed);
                        state.query_stats.record(&request.query, false, elapsed);
                        QueryResult::ok(CachedResponse::SharedFresh {
                            response: response_arc,
                            took_ms: elapsed.as_millis() as u64,
                        })
                    }
                    Err(e) => {
                        state.metrics.record_miss(MissReason::UpstreamError);
                        state.metrics.record_upstream_failure();
                        state.circuit_breaker.record_failure();

                        if let Some(entry) = state.cache.get_by_hash(&query_hash) {
                            state.metrics.record_frozen();
                            let elapsed = start.elapsed();
                            let took_ms = elapsed.as_millis() as u64;

                            let broadcast = Arc::new(QueryResponse {
                                results: entry.response.results.clone(),
                                cache_status: CacheStatus::Frozen,
                                took_ms,
                                generated_at: entry.response.generated_at,
                                miss_reason: entry.response.miss_reason,
                            });
                            state.coalescer.complete(&query_hash, Ok(broadcast));

                            state.metrics.record_latency(elapsed);
                            state.client_tracker.complete(&request_id);
                            return QueryResult::ok(CachedResponse::from_cache(
                                entry,
                                CacheStatus::Frozen,
                                took_ms,
                            ));
                        }

                        let error = Arc::new(ProxyError::Http(e.to_string()));
                        state.coalescer.complete(&query_hash, Err(error));
                        let elapsed = start.elapsed();
                        state.metrics.record_latency(elapsed);
                        QueryResult::with_status(
                            CachedResponse::Fresh(empty_miss_response(
                                elapsed.as_millis() as u64,
                                Some(MissReason::UpstreamError),
                            )),
                            502,
                        )
                    }
                }
            }
            CoalesceAction::Waiter(mut receiver) => {
                state.metrics.record_coalesced();
                match receiver.recv().await {
                    Ok(Ok(response_arc)) => {
                        let elapsed = start.elapsed();
                        let took_ms = elapsed.as_millis() as u64;
                        state.metrics.record_latency(elapsed);
                        QueryResult::ok(CachedResponse::SharedFresh {
                            response: response_arc,
                            took_ms,
                        })
                    }
                    Ok(Err(_)) | Err(_) => {
                        if let Some(entry) = state.cache.get_by_hash(&query_hash) {
                            state.metrics.record_frozen();
                            let elapsed = start.elapsed();
                            let took_ms = elapsed.as_millis() as u64;
                            state.metrics.record_latency(elapsed);
                            state.client_tracker.complete(&request_id);
                            return QueryResult::ok(CachedResponse::from_cache(
                                entry,
                                CacheStatus::Frozen,
                                took_ms,
                            ));
                        }
                        let elapsed = start.elapsed();
                        state.metrics.record_latency(elapsed);
                        QueryResult::with_status(
                            CachedResponse::Fresh(empty_miss_response(
                                elapsed.as_millis() as u64,
                                Some(MissReason::UpstreamError),
                            )),
                            502,
                        )
                    }
                }
            }
        }
    } else {
        // No upstream configured
        if let Some(entry) = state.cache.get_by_hash(&query_hash) {
            let took_ms = start.elapsed().as_millis() as u64;
            QueryResult::ok(CachedResponse::from_cache(entry, CacheStatus::Hit, took_ms))
        } else {
            state.metrics.record_miss(miss_reason);
            QueryResult::with_status(
                CachedResponse::Fresh(empty_miss_response(
                    start.elapsed().as_millis() as u64,
                    Some(miss_reason),
                )),
                503,
            )
        }
    };

    // Complete client tracking
    state.client_tracker.complete(&request_id);

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that empty_miss_response produces a correctly shaped response.
    #[test]
    fn test_empty_miss_response_no_reason() {
        let resp = empty_miss_response(42, None);
        assert!(resp.results.is_empty());
        assert_eq!(resp.cache_status, CacheStatus::Miss);
        assert_eq!(resp.took_ms, 42);
        assert!(resp.miss_reason.is_none());
    }

    /// Test that empty_miss_response includes the provided miss_reason.
    #[test]
    fn test_empty_miss_response_with_reason() {
        let resp = empty_miss_response(99, Some(MissReason::NotInCache));
        assert!(resp.results.is_empty());
        assert_eq!(resp.took_ms, 99);
        assert_eq!(resp.miss_reason, Some(MissReason::NotInCache));
    }

    /// Test QueryResult::ok creates a 200 response with Fresh variant.
    #[test]
    fn test_query_result_ok() {
        let response = QueryResponse {
            results: vec![],
            cache_status: CacheStatus::Hit,
            took_ms: 10,
            generated_at: None,
            miss_reason: None,
        };
        let result = QueryResult::ok(CachedResponse::Fresh(response));
        assert_eq!(result.status, 200);
    }

    /// Test QueryResult::with_status sets the provided status.
    #[test]
    fn test_query_result_with_status() {
        let response = QueryResponse {
            results: vec![],
            cache_status: CacheStatus::Hit,
            took_ms: 10,
            generated_at: None,
            miss_reason: None,
        };
        let result = QueryResult::with_status(CachedResponse::Fresh(response), 503);
        assert_eq!(result.status, 503);
    }

    /// Test QueryResult public field access.
    #[test]
    fn test_query_result_fields() {
        let response = QueryResponse {
            results: vec![],
            cache_status: CacheStatus::Hit,
            took_ms: 10,
            generated_at: None,
            miss_reason: None,
        };
        let result = QueryResult::ok(CachedResponse::Fresh(response));
        assert_eq!(result.status, 200);
    }
}
