use super::*;
use tracing::{debug, instrument, warn};

/// Handle POST /batch requests.
///
/// Process multiple queries in a single request for efficiency.
#[instrument(skip(state, headers, agent_identity), fields(query_count = request.queries.len(), fail_fast = request.fail_fast))]
pub(super) async fn handle_batch(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    agent_identity: Option<axum::Extension<AgentIdentity>>,
    Json(request): Json<BatchRequest>,
) -> impl IntoResponse {
    let start = Instant::now();
    debug!("Processing batch request");

    // Validate batch request
    if let Err(_e) = state.batch_processor.validate(&request) {
        warn!("Batch validation failed");
        return (
            StatusCode::BAD_REQUEST,
            Json(BatchResponse {
                results: std::collections::HashMap::new(),
                took_ms: start.elapsed().as_millis() as u64,
                success_count: 0,
                error_count: 0,
                complete: false,
            }),
        );
    }

    let batch_ctx_id = resolve_context(&headers, agent_identity.as_deref());

    // Ensure context exists (auto-creates if configured)
    if state.context_manager.get(&batch_ctx_id).is_none() {
        let _ = state.context_manager.switch(&batch_ctx_id);
    }

    // Enforce context binding for agents with restricted contexts
    if let Some(ref agent) = agent_identity {
        if !agent.can_access_context(&batch_ctx_id) {
            return (
                StatusCode::FORBIDDEN,
                Json(BatchResponse {
                    results: std::collections::HashMap::new(),
                    took_ms: start.elapsed().as_millis() as u64,
                    success_count: 0,
                    error_count: 0,
                    complete: false,
                }),
            );
        }
    }

    // Bundle shared state into Arc to avoid per-query Arc cloning
    struct BatchContext {
        cache: Arc<CacheStore>,
        cascade: Option<Arc<CascadeExecutor>>,
        pool: Option<Arc<UpstreamPool>>,
        upstream: Option<Arc<GenericRestAdapter>>,
        scope_filter: Arc<ScopeFilter>,
        metrics: Arc<ProxyMetrics>,
        query_stats: Arc<QueryStatsTracker>,
        coalescer: Arc<RequestCoalescer>,
        refresh_worker: Option<Arc<QueryTrackingRefreshWorker>>,
        upstream_id: String,
        context_manager: Arc<ContextManager>,
        batch_ctx_id: String,
        global_concurrency: Arc<tokio::sync::Semaphore>,
        #[cfg(feature = "embed-api")]
        smart_embedder: Option<Arc<crate::proxy::smart_embedder::SmartEmbedder>>,
    }

    let ctx = Arc::new(BatchContext {
        cache: state.cache.clone(),
        cascade: state.cascade_executor.load_full(),
        pool: state.upstream_pool.load_full(),
        upstream: state.upstream.load_full(),
        scope_filter: state.scope_filter_for(&batch_ctx_id),
        metrics: state.metrics.clone(),
        query_stats: state.query_stats.clone(),
        coalescer: state.coalescer.clone(),
        refresh_worker: state.refresh_worker.clone(),
        upstream_id: state.upstream_id.clone(),
        context_manager: state.context_manager.clone(),
        batch_ctx_id,
        global_concurrency: state.global_concurrency.clone(),
        #[cfg(feature = "embed-api")]
        smart_embedder: state.smart_embedder.clone(),
    });

    let result = state
        .batch_processor
        .process(request, move |req| {
            let ctx = ctx.clone();

            async move {
                let query_start = Instant::now();
                let ctx_q = context_query(&ctx.batch_ctx_id, &req.query);
                let query_hash = CacheStore::hash_query(&ctx_q);

                // Check cache first
                let mut miss_reason = MissReason::NotInCache;
                if let Some(freshness) = ctx.cache.check_freshness_by_hash(&query_hash) {
                    match freshness {
                        Freshness::Fresh | Freshness::Stale => {
                            if let Some(entry) = ctx.cache.get_by_hash(&query_hash) {
                                ctx.metrics.record_hit();
                                ctx.context_manager.record_hit_for(&ctx.batch_ctx_id);
                                let elapsed = query_start.elapsed();
                                ctx.metrics.record_latency(elapsed);
                                ctx.query_stats.record(&req.query, true, elapsed);
                                // PERF(R2): clone required — batch mutates cache_status/took_ms
                                let mut response = entry.response.clone();
                                response.cache_status = if matches!(freshness, Freshness::Fresh) {
                                    CacheStatus::Hit
                                } else {
                                    CacheStatus::Stale
                                };
                                response.took_ms = elapsed.as_millis() as u64;
                                return Ok(response);
                            }
                        }
                        _ => {
                            miss_reason = MissReason::Expired;
                        }
                    }
                }

                // Cache miss - fetch from upstream (cascade > pool > single)
                let has_upstream =
                    ctx.cascade.is_some() || ctx.pool.is_some() || ctx.upstream.is_some();
                if has_upstream {
                    // (query_hash already computed above — R8: single blake3 call per query)

                    match ctx.coalescer.get_or_insert(query_hash) {
                        CoalesceAction::Leader => {
                            ctx.metrics.record_upstream_request();
                            let query_result = super::query_core::dispatch_upstream_query(
                                &ctx.cascade,
                                &ctx.pool,
                                &ctx.upstream,
                                &req,
                                #[cfg(feature = "embed-api")]
                                &None,
                                &ctx.global_concurrency,
                            )
                            .await;
                            match query_result {
                                Ok(mut response) => {
                                    ctx.metrics.record_miss(miss_reason);
                                    ctx.context_manager.record_miss_for(&ctx.batch_ctx_id);

                                    // Apply scope filtering (hybrid when embed-api)
                                    response.results = super::query_core::apply_scope_filter(
                                        &ctx.scope_filter,
                                        response.results,
                                        #[cfg(feature = "embed-api")]
                                        ctx.smart_embedder.as_deref(),
                                    )
                                    .await;

                                    response.cache_status = CacheStatus::Miss;
                                    response.miss_reason = Some(miss_reason);
                                    // Cache if valid (context-isolated key)
                                    // R8: use insert_by_hash to skip redundant blake3 re-hash of ctx_q
                                    if response.validate().is_ok() {
                                        ctx.cache.insert_by_hash(
                                            query_hash,
                                            response.clone(),
                                            ctx.upstream_id.clone(),
                                            Some(&ctx_q),
                                        );
                                        if let Some(ref worker) = ctx.refresh_worker {
                                            worker.register_query(&ctx_q);
                                        }
                                    }

                                    let elapsed = query_start.elapsed();
                                    ctx.metrics.record_latency(elapsed);
                                    response.took_ms = elapsed.as_millis() as u64;
                                    let response_arc = Arc::new(response);
                                    ctx.coalescer
                                        .complete(&query_hash, Ok(Arc::clone(&response_arc)));
                                    ctx.query_stats.record(&req.query, false, elapsed);
                                    Ok(Arc::try_unwrap(response_arc)
                                        .unwrap_or_else(|a| (*a).clone()))
                                }
                                Err(e) => {
                                    ctx.metrics.record_miss(MissReason::UpstreamError);
                                    ctx.metrics.record_upstream_failure();
                                    // Try frozen cache
                                    if let Some(entry) = ctx.cache.get_by_hash(&query_hash) {
                                        let elapsed = query_start.elapsed();
                                        ctx.metrics.record_latency(elapsed);
                                        let took_ms = elapsed.as_millis() as u64;
                                        // R2: build owned response once, wrap in Arc, try_unwrap to avoid double clone
                                        let mut response = entry.response.clone();
                                        response.cache_status = CacheStatus::Frozen;
                                        response.took_ms = took_ms;
                                        let response_arc = Arc::new(response);
                                        ctx.coalescer
                                            .complete(&query_hash, Ok(Arc::clone(&response_arc)));
                                        return Ok(Arc::try_unwrap(response_arc)
                                            .unwrap_or_else(|a| (*a).clone()));
                                    }
                                    let error = Arc::new(ProxyError::Http(e.to_string()));
                                    ctx.coalescer.complete(&query_hash, Err(error));
                                    Err(e.to_string())
                                }
                            }
                        }
                        CoalesceAction::Waiter(mut receiver) => {
                            ctx.metrics.record_coalesced();
                            match receiver.recv().await {
                                Ok(Ok(response_arc)) => {
                                    let elapsed = query_start.elapsed();
                                    ctx.metrics.record_latency(elapsed);
                                    let mut response = Arc::try_unwrap(response_arc)
                                        .unwrap_or_else(|a| (*a).clone());
                                    response.took_ms = elapsed.as_millis() as u64;
                                    Ok(response)
                                }
                                _ => {
                                    // Try cache as fallback
                                    if let Some(entry) = ctx.cache.get_by_hash(&query_hash) {
                                        // PERF(R2): clone required — batch mutates cache_status/took_ms
                                        let mut response = entry.response.clone();
                                        response.cache_status = CacheStatus::Frozen;
                                        let elapsed = query_start.elapsed();
                                        ctx.metrics.record_latency(elapsed);
                                        response.took_ms = elapsed.as_millis() as u64;
                                        return Ok(response);
                                    }
                                    // PERF(R6): deferred — batch error type needs &'static str variant
                                    Err("Upstream request failed".to_string())
                                }
                            }
                        }
                    }
                } else {
                    // No upstream - check cache only
                    if let Some(entry) = ctx.cache.get_by_hash(&query_hash) {
                        // PERF(R2): clone required — batch mutates cache_status/took_ms
                        let mut response = entry.response.clone();
                        response.cache_status = CacheStatus::Hit;
                        let elapsed = query_start.elapsed();
                        ctx.metrics.record_latency(elapsed);
                        response.took_ms = elapsed.as_millis() as u64;
                        Ok(response)
                    } else {
                        // PERF(R6): deferred — batch error type needs &'static str variant
                        Err("No upstream configured and query not in cache".to_string())
                    }
                }
            }
        })
        .await;

    match result {
        Ok(response) => (StatusCode::OK, Json(response)),
        Err(_) => (
            StatusCode::BAD_REQUEST,
            Json(BatchResponse {
                results: std::collections::HashMap::new(),
                took_ms: start.elapsed().as_millis() as u64,
                success_count: 0,
                error_count: 0,
                complete: false,
            }),
        ),
    }
}

/// Request body for federated search.
#[derive(Debug, Clone, serde::Deserialize)]
pub(super) struct FederatedRequest {
    /// The query text.
    pub query: String,
    /// Local search results (if already computed).
    #[serde(default)]
    pub local_results: Option<Vec<super::super::types::SearchResult>>,
    /// Number of results to return.
    #[serde(default)]
    pub top_k: Option<usize>,
}

/// Response for federated search.
#[derive(Debug, Clone, Serialize)]
pub(super) struct FederatedSearchResponse {
    /// Combined results.
    pub results: Vec<super::super::types::SearchResult>,
    /// Cache status.
    pub cache_status: CacheStatus,
    /// Total time in milliseconds.
    pub took_ms: u64,
    /// Federated search statistics.
    pub stats: FederatedStats,
}

/// Handle POST /federated requests.
///
/// Performs federated search combining local results with remote upstream results.
/// If local_results are provided in the request, uses those; otherwise queries remote only.
/// Decision to query remote is based on federated search configuration (min_local_results,
/// min_local_confidence, merge_mode, etc.).
#[instrument(skip(state, headers, agent_identity), fields(query_len = request.query.len(), has_local = request.local_results.is_some()))]
pub(super) async fn handle_federated(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    agent_identity: Option<axum::Extension<AgentIdentity>>,
    Json(request): Json<FederatedRequest>,
) -> impl IntoResponse {
    let start = Instant::now();
    debug!("Processing federated search request");

    // Resolve per-request context (header > agent default > "default")
    let fed_ctx_id = resolve_context(&headers, agent_identity.as_deref());

    // Ensure context exists (auto-creates if configured)
    if state.context_manager.get(&fed_ctx_id).is_none() {
        if let Err(e) = state.context_manager.switch(&fed_ctx_id) {
            tracing::warn!(error = %e, context = %fed_ctx_id, "Failed to auto-create context in federated search");
        }
    }

    // Enforce context binding for agents with restricted contexts
    if let Some(ref agent) = agent_identity {
        if !agent.can_access_context(&fed_ctx_id) {
            return (
                StatusCode::FORBIDDEN,
                Json(FederatedSearchResponse {
                    results: vec![],
                    cache_status: CacheStatus::Miss,
                    took_ms: start.elapsed().as_millis() as u64,
                    stats: FederatedStats::default(),
                }),
            );
        }
    }

    // Snapshot federated search once for the whole handler — avoids
    // three separate ArcSwap loads across the same async path.
    let fed = state.federated_search.load_full();

    // If federated search is disabled, just do a normal query
    if !fed.is_enabled() {
        debug!("Federated search disabled, using normal query");
        // Delegate to normal query handling
        let query_request = QueryRequest {
            query: request.query,
            top_k: request.top_k,
            priority: None,
            upstream_id: None,
            upstream_type: None,
        };

        // Use the cache/upstream path
        let fed_ctx_query = context_query(&fed_ctx_id, &query_request.query);
        if let Some(entry) = state.cache.get(&fed_ctx_query) {
            state.metrics.record_hit();
            state.context_manager.record_hit_for(&fed_ctx_id);
            state.metrics.record_latency(start.elapsed());
            let mut response = entry.response.clone();
            response.cache_status = CacheStatus::Hit;
            response.took_ms = start.elapsed().as_millis() as u64;

            return (
                StatusCode::OK,
                Json(FederatedSearchResponse {
                    results: response.results,
                    cache_status: response.cache_status,
                    took_ms: response.took_ms,
                    stats: FederatedStats::default(),
                }),
            );
        }

        // No cache hit - return error (federated disabled, no local results)
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(FederatedSearchResponse {
                results: vec![],
                cache_status: CacheStatus::Miss,
                took_ms: start.elapsed().as_millis() as u64,
                stats: FederatedStats::default(),
            }),
        );
    }

    // Get local results (either from request or empty)
    let local_results = request.local_results.unwrap_or_default();
    let local_latency = 0u64; // Local results are pre-computed or empty

    // Decide if we should query remote
    let fallback_decision = fed.should_query_remote(&local_results);

    let (remote_response, remote_latency) = match fallback_decision {
        FallbackDecision::LocalSufficient => (None, None),
        _ => {
            // Query remote/upstream
            let remote_start = Instant::now();
            let has_upstream =
                state.upstream_pool.load_full().is_some() || state.upstream.load_full().is_some();

            if !has_upstream {
                (None, None)
            } else {
                let query_request = QueryRequest {
                    query: request.query.clone(),
                    top_k: request.top_k,
                    priority: None,
                    upstream_id: None,
                    upstream_type: None,
                };
                let remote_ctx_query = context_query(&fed_ctx_id, &query_request.query);

                // Check cache first
                if let Some(entry) = state.cache.get(&remote_ctx_query) {
                    let latency = remote_start.elapsed().as_millis() as u64;
                    (Some(entry.response.clone()), Some(latency))
                } else {
                    // Query upstream
                    let pool = state.upstream_pool.load_full();
                    let upstream = state.upstream.load_full();
                    let result = if let Some(ref pool) = pool {
                        pool.query(&query_request).await
                    } else if let Some(ref upstream) = upstream {
                        upstream.query(&query_request).await
                    } else {
                        Err(super::super::upstream::UpstreamError::NotConfigured)
                    };

                    match result {
                        Ok(response) => {
                            let latency = remote_start.elapsed().as_millis() as u64;
                            // Cache the result (context-isolated key)
                            state.cache.insert_with_context(
                                &remote_ctx_query,
                                response.clone(),
                                state.upstream_id.clone(),
                                &fed_ctx_id,
                            );
                            (Some(response), Some(latency))
                        }
                        Err(_) => {
                            state.metrics.record_upstream_failure();
                            (None, None)
                        }
                    }
                }
            }
        }
    };

    // Build local response for merging
    let local_response = QueryResponse {
        results: local_results,
        cache_status: CacheStatus::Hit, // Local results are considered "hit"
        took_ms: local_latency,
        generated_at: None,
        miss_reason: None,
    };

    // Merge results
    let (merged_response, mut stats) = fed.merge_responses(local_response, remote_response);

    // Update stats with timing and fallback reason
    stats.local_latency_ms = local_latency;
    stats.remote_latency_ms = remote_latency;
    stats.fallback_reason = Some(match fallback_decision {
        FallbackDecision::LocalSufficient => "local_sufficient".to_string(),
        FallbackDecision::EmptyLocal => "empty_local".to_string(),
        FallbackDecision::BelowMinResults => "below_min_results".to_string(),
        FallbackDecision::LowConfidence => "low_confidence".to_string(),
        FallbackDecision::AlwaysRemote => "always_remote".to_string(),
    });

    state.metrics.record_latency(start.elapsed());

    let response = FederatedSearchResponse {
        results: merged_response.results,
        cache_status: merged_response.cache_status,
        took_ms: start.elapsed().as_millis() as u64,
        stats,
    };

    (StatusCode::OK, Json(response))
}
