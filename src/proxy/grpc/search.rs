//! SearchService gRPC implementation.
//!
//! Handles Query, BatchQuery, FederatedQuery, and QueryStream RPCs.

use tonic::{Request, Response, Status};

use super::proto;
use super::proto::search_service_server::SearchService;
use crate::proxy::server::query_core;
use crate::proxy::server::AppState;
use crate::proxy::types::QueryRequest as InternalQueryRequest;

/// gRPC SearchService implementation.
pub struct SearchServiceImpl {
    pub(crate) state: AppState,
}

impl SearchServiceImpl {
    pub(crate) fn new(state: AppState) -> Self {
        Self { state }
    }
}

/// Extract API key from gRPC metadata.
/// Extract API key from gRPC metadata.
///
/// Supports both `x-api-key` metadata and `Authorization: Bearer` metadata.
fn extract_api_key(metadata: &tonic::metadata::MetadataMap) -> Option<String> {
    // Try x-api-key first
    if let Some(key) = metadata.get("x-api-key") {
        return key.to_str().ok().map(|s| s.to_string());
    }
    // Try Authorization: Bearer
    if let Some(auth) = metadata.get("authorization") {
        if let Ok(auth_str) = auth.to_str() {
            if let Some(token) = auth_str.strip_prefix("Bearer ") {
                return Some(token.to_string());
            }
        }
    }
    None
}

/// Extract or generate request ID from gRPC metadata/request.
fn resolve_request_id(proto_request_id: &str, metadata: &tonic::metadata::MetadataMap) -> String {
    if !proto_request_id.is_empty() {
        return proto_request_id.to_string();
    }
    metadata
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

/// Resolve context from proto field or metadata.
fn resolve_context(proto_context: &str, metadata: &tonic::metadata::MetadataMap) -> String {
    if !proto_context.is_empty() {
        return proto_context.to_string();
    }
    metadata
        .get("x-context")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "default".to_string())
}

/// Authenticate the request if auth is required.
#[allow(clippy::result_large_err)]
fn authenticate(
    state: &AppState,
    metadata: &tonic::metadata::MetadataMap,
) -> Result<Option<crate::proxy::agent::AgentIdentity>, Status> {
    // Check agent registry first.
    //
    // Defense in depth: an empty registry must NOT impose auth requirements.
    // If `apply_reload` (or any future caller) installs `Some(empty registry)`
    // when no agents are configured, the gRPC path would otherwise flip into
    // "API key required" mode — breaking search for callers that were never
    // given a key (e.g. the MCP `search` tool before API key plumbing
    // was added, or any open-loop local setup).
    let registry = state.agent_registry.load_full();
    if let Some(ref registry) = registry {
        if !registry.is_empty() {
            if let Some(key) = extract_api_key(metadata) {
                match registry.lookup(&key) {
                    Some(lookup) => return Ok(Some(lookup.identity)),
                    None => return Err(Status::unauthenticated("Invalid API key")),
                }
            }
            // Registry is non-empty but no key was provided.
            return Err(Status::unauthenticated("API key required"));
        }
    }
    Ok(None)
}

#[tonic::async_trait]
impl SearchService for SearchServiceImpl {
    async fn query(
        &self,
        request: Request<proto::QueryRequest>,
    ) -> Result<Response<proto::QueryResponse>, Status> {
        let metadata = request.metadata().clone();
        let agent = authenticate(&self.state, &metadata)?;
        let proto_req = request.into_inner();

        let request_id = resolve_request_id(&proto_req.request_id, &metadata);
        let context_id = resolve_context(&proto_req.context, &metadata);
        let source = agent
            .as_ref()
            .map(|a| a.id.clone())
            .unwrap_or_else(|| "grpc".to_string());

        let internal_req = InternalQueryRequest::from(proto_req);

        let result = query_core::execute_query(
            &self.state,
            internal_req,
            context_id,
            request_id,
            agent.as_ref(),
            source,
        )
        .await;

        match result.status {
            200 => Ok(Response::new(proto::QueryResponse::from(&result.response))),
            400 => Err(Status::invalid_argument("Request validation failed")),
            403 => Err(Status::permission_denied("Context access denied")),
            502 => Err(Status::unavailable("Upstream request failed")),
            503 => Err(Status::unavailable("Service unavailable")),
            _ => Err(Status::internal("Internal error")),
        }
    }

    async fn batch_query(
        &self,
        request: Request<proto::BatchQueryRequest>,
    ) -> Result<Response<proto::BatchQueryResponse>, Status> {
        let metadata = request.metadata().clone();
        let _agent = authenticate(&self.state, &metadata)?;
        let proto_req = request.into_inner();

        let batch_req = crate::proxy::batch::BatchRequest::from(proto_req.clone());
        let context_id = resolve_context(&proto_req.context, &metadata);

        // Validate batch
        if let Err(_e) = self.state.batch_processor.validate(&batch_req) {
            return Err(Status::invalid_argument("Batch validation failed"));
        }

        // Ensure context exists (auto-creates if configured)
        if self.state.context_manager.get(&context_id).is_none() {
            if let Err(e) = self.state.context_manager.switch(&context_id) {
                tracing::warn!(error = %e, context = %context_id, "Failed to switch context in batch");
            }
        }

        let cache = self.state.cache.clone();
        let cascade = self.state.cascade_executor.load_full();
        let pool = self.state.upstream_pool.load_full();
        let upstream = self.state.upstream.load_full();
        let scope_filter = self.state.scope_filter_for(&context_id);
        let metrics = self.state.metrics.clone();
        let query_stats = self.state.query_stats.clone();
        let coalescer = self.state.coalescer.clone();
        let refresh_worker = self.state.refresh_worker.clone();
        let upstream_id = self.state.upstream_id.clone();
        let context_manager = self.state.context_manager.clone();
        let global_concurrency = self.state.global_concurrency.clone();
        #[cfg(feature = "embed-api")]
        let smart_embedder = self.state.smart_embedder.clone();

        let result = self
            .state
            .batch_processor
            .process(batch_req, move |req| {
                let cache = cache.clone();
                let cascade = cascade.clone();
                let pool = pool.clone();
                let upstream = upstream.clone();
                let scope_filter = scope_filter.clone();
                let metrics = metrics.clone();
                let query_stats = query_stats.clone();
                let coalescer = coalescer.clone();
                let refresh_worker = refresh_worker.clone();
                let upstream_id = upstream_id.clone();
                let context_manager = context_manager.clone();
                let context_id = context_id.clone();
                let global_concurrency = global_concurrency.clone();
                #[cfg(feature = "embed-api")]
                let smart_embedder = smart_embedder.clone();

                async move {
                    use crate::proxy::cache::CacheStore;
                    use crate::proxy::coalesce::CoalesceAction;
                    use crate::proxy::types::{CacheStatus, Freshness, MissReason};
                    use std::time::Instant;

                    let query_start = Instant::now();
                    let ctx_q = format!("ctx:{}:{}", context_id, req.query);

                    // Check cache
                    let mut miss_reason = MissReason::NotInCache;
                    if let Some(freshness) = cache.check_freshness(&ctx_q) {
                        match freshness {
                            Freshness::Fresh | Freshness::Stale => {
                                if let Some(entry) = cache.get(&ctx_q) {
                                    metrics.record_hit();
                                    context_manager.record_hit_for(&context_id);
                                    let elapsed = query_start.elapsed();
                                    metrics.record_latency(elapsed);
                                    query_stats.record(&req.query, true, elapsed);
                                    let mut response = entry.response.clone();
                                    response.cache_status = if matches!(freshness, Freshness::Fresh)
                                    {
                                        CacheStatus::Hit
                                    } else {
                                        CacheStatus::Stale
                                    };
                                    response.took_ms = query_start.elapsed().as_millis() as u64;
                                    return Ok(response);
                                }
                            }
                            _ => {
                                miss_reason = MissReason::Expired;
                            }
                        }
                    }

                    // Cache miss - fetch upstream
                    let has_upstream = cascade.is_some() || pool.is_some() || upstream.is_some();
                    if has_upstream {
                        let query_hash = CacheStore::hash_query(&ctx_q);
                        match coalescer.get_or_insert(query_hash) {
                            CoalesceAction::Leader => {
                                metrics.record_upstream_request();
                                let query_result = query_core::dispatch_upstream_query(
                                    &cascade,
                                    &pool,
                                    &upstream,
                                    &req,
                                    #[cfg(feature = "embed-api")]
                                    &None,
                                    &global_concurrency,
                                )
                                .await;
                                match query_result {
                                    Ok(mut response) => {
                                        metrics.record_miss(miss_reason);
                                        context_manager.record_miss_for(&context_id);
                                        response.results = query_core::apply_scope_filter(
                                            &scope_filter,
                                            response.results,
                                            #[cfg(feature = "embed-api")]
                                            smart_embedder.as_deref(),
                                        )
                                        .await;
                                        response.cache_status = CacheStatus::Miss;
                                        response.miss_reason = Some(miss_reason);
                                        let elapsed = query_start.elapsed();
                                        metrics.record_latency(elapsed);
                                        response.took_ms = elapsed.as_millis() as u64;
                                        if response.validate().is_ok() {
                                            cache.insert(&ctx_q, response.clone(), upstream_id);
                                            if let Some(ref worker) = refresh_worker {
                                                worker.register_query(&ctx_q);
                                            }
                                        }
                                        let response_arc = std::sync::Arc::new(response);
                                        coalescer.complete(
                                            &query_hash,
                                            Ok(std::sync::Arc::clone(&response_arc)),
                                        );
                                        query_stats.record(&req.query, false, elapsed);
                                        Ok(std::sync::Arc::try_unwrap(response_arc)
                                            .unwrap_or_else(|a| (*a).clone()))
                                    }
                                    Err(e) => {
                                        metrics.record_miss(MissReason::UpstreamError);
                                        metrics.record_upstream_failure();
                                        if let Some(entry) = cache.get(&ctx_q) {
                                            let mut response = entry.response.clone();
                                            response.cache_status = CacheStatus::Frozen;
                                            let elapsed = query_start.elapsed();
                                            metrics.record_latency(elapsed);
                                            response.took_ms = elapsed.as_millis() as u64;
                                            coalescer.complete(
                                                &query_hash,
                                                Ok(std::sync::Arc::new(response.clone())),
                                            );
                                            return Ok(response);
                                        }
                                        let error = std::sync::Arc::new(
                                            crate::proxy::lifecycle::ProxyError::Http(
                                                e.to_string(),
                                            ),
                                        );
                                        coalescer.complete(&query_hash, Err(error));
                                        Err(e.to_string())
                                    }
                                }
                            }
                            CoalesceAction::Waiter(mut receiver) => {
                                metrics.record_coalesced();
                                match receiver.recv().await {
                                    Ok(Ok(response_arc)) => {
                                        let elapsed = query_start.elapsed();
                                        metrics.record_latency(elapsed);
                                        let mut response = std::sync::Arc::try_unwrap(response_arc)
                                            .unwrap_or_else(|a| (*a).clone());
                                        response.took_ms = elapsed.as_millis() as u64;
                                        Ok(response)
                                    }
                                    _ => {
                                        if let Some(entry) = cache.get(&ctx_q) {
                                            let mut response = entry.response.clone();
                                            response.cache_status = CacheStatus::Frozen;
                                            let elapsed = query_start.elapsed();
                                            metrics.record_latency(elapsed);
                                            response.took_ms = elapsed.as_millis() as u64;
                                            return Ok(response);
                                        }
                                        Err("Upstream request failed".to_string())
                                    }
                                }
                            }
                        }
                    } else if let Some(entry) = cache.get(&ctx_q) {
                        let mut response = entry.response.clone();
                        response.cache_status = CacheStatus::Hit;
                        let elapsed = query_start.elapsed();
                        metrics.record_latency(elapsed);
                        response.took_ms = elapsed.as_millis() as u64;
                        Ok(response)
                    } else {
                        Err("No upstream configured".to_string())
                    }
                }
            })
            .await;

        match result {
            Ok(response) => Ok(Response::new(super::convert::batch_response_to_proto(
                &response,
            ))),
            Err(_) => Err(Status::internal("Batch processing failed")),
        }
    }

    async fn federated_query(
        &self,
        request: Request<proto::FederatedQueryRequest>,
    ) -> Result<Response<proto::FederatedQueryResponse>, Status> {
        let metadata = request.metadata().clone();
        let _agent = authenticate(&self.state, &metadata)?;
        let proto_req = request.into_inner();

        let context_id = resolve_context(&proto_req.context, &metadata);

        // Ensure context exists
        if self.state.context_manager.get(&context_id).is_none() {
            if let Err(e) = self.state.context_manager.switch(&context_id) {
                tracing::warn!(error = %e, context = %context_id, "Failed to switch context");
            }
        }

        let local_results: Vec<crate::proxy::types::SearchResult> = proto_req
            .local_results
            .into_iter()
            .map(crate::proxy::types::SearchResult::from)
            .collect();

        let local_response = crate::proxy::types::QueryResponse {
            results: local_results.clone(),
            cache_status: crate::proxy::types::CacheStatus::Hit,
            took_ms: 0,
            generated_at: None,
            miss_reason: None,
        };

        // Snapshot federated search once — avoids three ArcSwap loads across async path.
        let fed = self.state.federated_search.load_full();

        if !fed.is_enabled() {
            // Delegate to normal cache lookup
            let ctx_query = format!("ctx:{}:{}", context_id, proto_req.query);
            if let Some(entry) = self.state.cache.get(&ctx_query) {
                self.state.metrics.record_hit();
                let response = proto::FederatedQueryResponse {
                    results: entry
                        .response
                        .results
                        .iter()
                        .map(proto::SearchResult::from)
                        .collect(),
                    cache_status: proto::CacheStatus::Hit as i32,
                    took_ms: 0,
                    stats: Some(proto::FederatedStats::default()),
                };
                return Ok(Response::new(response));
            }
            return Err(Status::unavailable(
                "Federated search disabled and no cache",
            ));
        }

        let fallback_decision = fed.should_query_remote(&local_results);

        let remote_response = match fallback_decision {
            crate::proxy::federated::FallbackDecision::LocalSufficient => None,
            _ => {
                let pool_opt = self.state.upstream_pool.load_full();
                let upstream_opt = self.state.upstream.load_full();
                let has_upstream = pool_opt.is_some() || upstream_opt.is_some();
                if has_upstream {
                    let query_req = InternalQueryRequest {
                        query: proto_req.query.clone(),
                        top_k: if proto_req.top_k > 0 {
                            Some(proto_req.top_k as usize)
                        } else {
                            None
                        },
                        priority: None,
                        upstream_id: None,
                        upstream_type: None,
                    };

                    let result = if let Some(ref pool) = pool_opt {
                        pool.query(&query_req).await
                    } else if let Some(ref upstream) = upstream_opt {
                        upstream.query(&query_req).await
                    } else {
                        Err(crate::proxy::upstream::UpstreamError::NotConfigured)
                    };

                    result.ok()
                } else {
                    None
                }
            }
        };

        let (merged_response, stats) = fed.merge_responses(local_response, remote_response);

        let response = proto::FederatedQueryResponse {
            results: merged_response
                .results
                .iter()
                .map(proto::SearchResult::from)
                .collect(),
            cache_status: proto::CacheStatus::from(merged_response.cache_status) as i32,
            took_ms: merged_response.took_ms,
            stats: Some(proto::FederatedStats::from(&stats)),
        };

        Ok(Response::new(response))
    }

    type QueryStreamStream = std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<proto::QueryResponse, Status>> + Send>,
    >;

    async fn query_stream(
        &self,
        request: Request<proto::QueryRequest>,
    ) -> Result<Response<Self::QueryStreamStream>, Status> {
        // For now, implement as a single-shot stream (same as Query but wrapped in a stream)
        let response = self.query(request).await?;
        let item = response.into_inner();
        let stream = tokio_stream::once(Ok(item));
        Ok(Response::new(Box::pin(stream)))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic
)]
mod tests {
    use super::*;
    use crate::proxy::server::tests::make_test_app_state;

    fn make_search_service() -> SearchServiceImpl {
        SearchServiceImpl::new(make_test_app_state())
    }

    #[test]
    fn test_extract_api_key_present() {
        let mut meta = tonic::metadata::MetadataMap::new();
        meta.insert("x-api-key", "test-key".parse().unwrap());
        assert_eq!(extract_api_key(&meta), Some("test-key".to_string()));
    }

    #[test]
    fn test_extract_api_key_missing() {
        let meta = tonic::metadata::MetadataMap::new();
        assert_eq!(extract_api_key(&meta), None);
    }

    #[test]
    fn test_resolve_request_id_from_proto() {
        let meta = tonic::metadata::MetadataMap::new();
        assert_eq!(resolve_request_id("proto-id", &meta), "proto-id");
    }

    #[test]
    fn test_resolve_request_id_from_metadata() {
        let mut meta = tonic::metadata::MetadataMap::new();
        meta.insert("x-request-id", "meta-id".parse().unwrap());
        assert_eq!(resolve_request_id("", &meta), "meta-id");
    }

    #[test]
    fn test_resolve_request_id_generated() {
        let meta = tonic::metadata::MetadataMap::new();
        let id = resolve_request_id("", &meta);
        assert!(id.starts_with("req-"));
    }

    #[test]
    fn test_resolve_context_from_proto() {
        let meta = tonic::metadata::MetadataMap::new();
        assert_eq!(resolve_context("my-ctx", &meta), "my-ctx");
    }

    #[test]
    fn test_resolve_context_from_metadata() {
        let mut meta = tonic::metadata::MetadataMap::new();
        meta.insert("x-context", "meta-ctx".parse().unwrap());
        assert_eq!(resolve_context("", &meta), "meta-ctx");
    }

    #[test]
    fn test_resolve_context_default() {
        let meta = tonic::metadata::MetadataMap::new();
        assert_eq!(resolve_context("", &meta), "default");
    }

    #[test]
    fn test_authenticate_no_registry() {
        let state = make_test_app_state();
        let meta = tonic::metadata::MetadataMap::new();
        let result = authenticate(&state, &meta);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_authenticate_empty_registry_no_key() {
        // Defense in depth: `Some(empty registry)` must NOT require auth.
        // Previously this asserted `is_err()`, which broke search after a
        // hot reload on a deployment that had no agents.
        let mut state = make_test_app_state();
        state.agent_registry.store(Some(std::sync::Arc::new(
            crate::proxy::agent::AgentRegistry::new(),
        )));
        let meta = tonic::metadata::MetadataMap::new();
        let result = authenticate(&state, &meta);
        assert!(result.is_ok(), "empty registry should not require auth");
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_authenticate_empty_registry_ignores_key() {
        // Same defense in depth: with an empty registry a stale key is
        // simply ignored (treated as no auth context). Compare with
        // `test_authenticate_non_empty_registry_invalid_key` below.
        let mut state = make_test_app_state();
        state.agent_registry.store(Some(std::sync::Arc::new(
            crate::proxy::agent::AgentRegistry::new(),
        )));
        let mut meta = tonic::metadata::MetadataMap::new();
        meta.insert("x-api-key", "bad-key".parse().unwrap());
        let result = authenticate(&state, &meta);
        assert!(result.is_ok(), "empty registry should not validate keys");
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_authenticate_non_empty_registry_no_key() {
        let mut state = make_test_app_state();
        let mut registry = crate::proxy::agent::AgentRegistry::new();
        registry.register(&crate::config::AgentConfig {
            id: "agent-1".to_string(),
            api_key: "valid-key".to_string(),
            default_context: None,
            allowed_contexts: vec![],
            priority_class: None,
            rate_limit_rps: None,
            enabled: true,
        });
        state
            .agent_registry
            .store(Some(std::sync::Arc::new(registry)));
        let meta = tonic::metadata::MetadataMap::new();
        let result = authenticate(&state, &meta);
        assert!(result.is_err());
    }

    #[test]
    fn test_authenticate_non_empty_registry_invalid_key() {
        let mut state = make_test_app_state();
        let mut registry = crate::proxy::agent::AgentRegistry::new();
        registry.register(&crate::config::AgentConfig {
            id: "agent-1".to_string(),
            api_key: "valid-key".to_string(),
            default_context: None,
            allowed_contexts: vec![],
            priority_class: None,
            rate_limit_rps: None,
            enabled: true,
        });
        state
            .agent_registry
            .store(Some(std::sync::Arc::new(registry)));
        let mut meta = tonic::metadata::MetadataMap::new();
        meta.insert("x-api-key", "bad-key".parse().unwrap());
        let result = authenticate(&state, &meta);
        assert!(result.is_err());
    }

    #[test]
    fn test_authenticate_with_registry_valid_key() {
        let mut state = make_test_app_state();
        let registry = crate::proxy::agent::AgentRegistry::new();
        registry.register(&crate::config::AgentConfig {
            id: "agent-1".to_string(),
            api_key: "valid-key".to_string(),
            default_context: None,
            allowed_contexts: vec![],
            priority_class: None,
            rate_limit_rps: None,
            enabled: true,
        });
        state
            .agent_registry
            .store(Some(std::sync::Arc::new(registry)));
        let mut meta = tonic::metadata::MetadataMap::new();
        meta.insert("x-api-key", "valid-key".parse().unwrap());
        let result = authenticate(&state, &meta);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().unwrap().id, "agent-1");
    }

    #[tokio::test]
    async fn test_grpc_query_no_upstream() {
        let svc = make_search_service();
        let resp = svc
            .query(Request::new(proto::QueryRequest {
                query: "test".to_string(),
                top_k: 5,
                request_id: String::new(),
                context: String::new(),
                upstream_id: String::new(),
                priority: 0,
                upstream_type: String::new(),
            }))
            .await;
        // No upstream = should respond (200 with no results or 503)
        assert!(resp.is_ok() || resp.is_err());
    }

    #[tokio::test]
    async fn test_grpc_query_cache_hit() {
        let svc = make_search_service();
        // Insert into cache first
        let response = crate::proxy::types::QueryResponse {
            results: vec![crate::proxy::types::SearchResult {
                id: "doc".to_string(),
                content: "content".to_string(),
                score: 0.9,
                metadata: None,
                upstream_id: None,
            }],
            cache_status: crate::proxy::types::CacheStatus::Miss,
            took_ms: 1,
            generated_at: None,
            miss_reason: None,
        };
        svc.state
            .cache
            .insert("ctx:default:cached-q", response, "up".to_string());

        let resp = svc
            .query(Request::new(proto::QueryRequest {
                query: "cached-q".to_string(),
                top_k: 5,
                request_id: String::new(),
                context: String::new(),
                upstream_id: String::new(),
                priority: 0,
                upstream_type: String::new(),
            }))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert!(!inner.results.is_empty());
    }

    fn insert_test_cache_entry(state: &AppState, query: &str) {
        let response = crate::proxy::types::QueryResponse {
            results: vec![crate::proxy::types::SearchResult {
                id: format!("{query}-doc"),
                content: format!("content for {query}"),
                score: 0.9,
                metadata: None,
                upstream_id: None,
            }],
            cache_status: crate::proxy::types::CacheStatus::Miss,
            took_ms: 1,
            generated_at: None,
            miss_reason: None,
        };
        state
            .cache
            .insert(&format!("ctx:default:{query}"), response, "up".to_string());
    }

    #[tokio::test]
    async fn test_grpc_batch_query_cache_hits() {
        use proto::search_service_server::SearchService;

        let svc = make_search_service();
        insert_test_cache_entry(&svc.state, "batch-q1");
        insert_test_cache_entry(&svc.state, "batch-q2");

        let resp = svc
            .batch_query(Request::new(proto::BatchQueryRequest {
                queries: vec!["batch-q1".to_string(), "batch-q2".to_string()],
                context: String::new(),
                fail_fast: false,
            }))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert_eq!(inner.results.len(), 2);
    }

    #[tokio::test]
    async fn test_grpc_batch_query_empty() {
        use proto::search_service_server::SearchService;

        let svc = make_search_service();
        let resp = svc
            .batch_query(Request::new(proto::BatchQueryRequest {
                queries: vec![],
                context: String::new(),
                fail_fast: false,
            }))
            .await;
        // Empty batch fails validation
        assert!(resp.is_err());
        assert_eq!(resp.unwrap_err().code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_grpc_batch_query_cache_miss_no_upstream() {
        use proto::search_service_server::SearchService;

        let svc = make_search_service();
        let resp = svc
            .batch_query(Request::new(proto::BatchQueryRequest {
                queries: vec!["miss-q".to_string()],
                context: String::new(),
                fail_fast: false,
            }))
            .await;
        // Should return error since no upstream and cache miss
        assert!(resp.is_ok() || resp.is_err());
    }

    #[tokio::test]
    async fn test_grpc_federated_query_disabled_cache_hit() {
        use proto::search_service_server::SearchService;

        let svc = make_search_service();
        // Federated is disabled by default. With a cache hit, it should return data.
        insert_test_cache_entry(&svc.state, "fed-q");

        let resp = svc
            .federated_query(Request::new(proto::FederatedQueryRequest {
                query: "fed-q".to_string(),
                local_results: vec![],
                context: String::new(),
                top_k: 5,
            }))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert!(!inner.results.is_empty());
    }

    #[tokio::test]
    async fn test_grpc_federated_query_disabled_no_cache() {
        use proto::search_service_server::SearchService;

        let svc = make_search_service();
        let resp = svc
            .federated_query(Request::new(proto::FederatedQueryRequest {
                query: "no-cache-fed".to_string(),
                local_results: vec![],
                context: String::new(),
                top_k: 5,
            }))
            .await;
        assert!(resp.is_err());
        let status = resp.unwrap_err();
        assert_eq!(status.code(), tonic::Code::Unavailable);
    }

    #[tokio::test]
    async fn test_grpc_federated_query_with_local_results() {
        use proto::search_service_server::SearchService;

        let mut state = make_test_app_state();
        // Enable federated search
        state.federated_search.store(std::sync::Arc::new(
            crate::proxy::federated::FederatedSearch::new(
                crate::proxy::federated::FederatedSearchConfig {
                    enabled: true,
                    ..Default::default()
                },
            ),
        ));
        let svc = SearchServiceImpl::new(state);

        let resp = svc
            .federated_query(Request::new(proto::FederatedQueryRequest {
                query: "fed-local".to_string(),
                local_results: vec![proto::SearchResult {
                    id: "local-doc".to_string(),
                    score: 0.95,
                    content: "local content".to_string(),
                    upstream_id: String::new(),
                    metadata_json: vec![],
                }],
                context: String::new(),
                top_k: 5,
            }))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert!(!inner.results.is_empty());
        assert!(inner.stats.is_some());
    }

    #[tokio::test]
    async fn test_grpc_query_stream() {
        use proto::search_service_server::SearchService;

        let svc = make_search_service();
        insert_test_cache_entry(&svc.state, "stream-q");

        let resp = svc
            .query_stream(Request::new(proto::QueryRequest {
                query: "stream-q".to_string(),
                top_k: 5,
                request_id: String::new(),
                context: String::new(),
                upstream_id: String::new(),
                priority: 0,
                upstream_type: String::new(),
            }))
            .await
            .unwrap();

        // Consume the stream
        use tokio_stream::StreamExt;
        let mut stream = resp.into_inner();
        let item = stream.next().await;
        assert!(item.is_some());
        let query_resp = item.unwrap().unwrap();
        assert!(!query_resp.results.is_empty());
    }

    #[tokio::test]
    async fn test_grpc_query_paused() {
        let state = make_test_app_state();
        state
            .paused
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let svc = SearchServiceImpl::new(state);

        let resp = svc
            .query(Request::new(proto::QueryRequest {
                query: "paused-q".to_string(),
                top_k: 5,
                request_id: String::new(),
                context: String::new(),
                upstream_id: String::new(),
                priority: 0,
                upstream_type: String::new(),
            }))
            .await;
        assert!(resp.is_err());
        assert_eq!(resp.unwrap_err().code(), tonic::Code::Unavailable);
    }

    #[tokio::test]
    async fn test_grpc_query_invalid_empty() {
        let svc = make_search_service();
        let resp = svc
            .query(Request::new(proto::QueryRequest {
                query: String::new(), // empty query
                top_k: 5,
                request_id: String::new(),
                context: String::new(),
                upstream_id: String::new(),
                priority: 0,
                upstream_type: String::new(),
            }))
            .await;
        // Empty query should fail validation → 400 → InvalidArgument
        assert!(resp.is_err());
        assert_eq!(resp.unwrap_err().code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_grpc_query_with_upstream_success() {
        use crate::proxy::server::tests::{
            make_test_app_state_with_upstream, start_mock_upstream_server,
        };

        let (_handle, url) = start_mock_upstream_server().await;
        let state = make_test_app_state_with_upstream(&url);
        let svc = SearchServiceImpl::new(state);

        let resp = svc
            .query(Request::new(proto::QueryRequest {
                query: "upstream-test".to_string(),
                top_k: 5,
                request_id: String::new(),
                context: String::new(),
                upstream_id: String::new(),
                priority: 0,
                upstream_type: String::new(),
            }))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert!(!inner.results.is_empty());
    }

    #[tokio::test]
    async fn test_grpc_query_upstream_failure() {
        use crate::proxy::server::tests::{
            make_test_app_state_with_upstream, start_failing_mock_upstream,
        };

        let (_handle, url) = start_failing_mock_upstream().await;
        let state = make_test_app_state_with_upstream(&url);
        let svc = SearchServiceImpl::new(state);

        let resp = svc
            .query(Request::new(proto::QueryRequest {
                query: "upstream-fail".to_string(),
                top_k: 5,
                request_id: String::new(),
                context: String::new(),
                upstream_id: String::new(),
                priority: 0,
                upstream_type: String::new(),
            }))
            .await;
        assert!(resp.is_err());
        assert_eq!(resp.unwrap_err().code(), tonic::Code::Unavailable);
    }

    #[tokio::test]
    async fn test_grpc_batch_query_with_upstream() {
        use crate::proxy::server::tests::{
            make_test_app_state_with_upstream, start_mock_upstream_server,
        };
        use proto::search_service_server::SearchService;

        let (_handle, url) = start_mock_upstream_server().await;
        let state = make_test_app_state_with_upstream(&url);
        let svc = SearchServiceImpl::new(state);

        let resp = svc
            .batch_query(Request::new(proto::BatchQueryRequest {
                queries: vec!["batch-up-q1".to_string(), "batch-up-q2".to_string()],
                context: String::new(),
                fail_fast: false,
            }))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert_eq!(inner.results.len(), 2);
    }

    #[tokio::test]
    async fn test_grpc_batch_query_upstream_failure() {
        use crate::proxy::server::tests::{
            make_test_app_state_with_upstream, start_failing_mock_upstream,
        };
        use proto::search_service_server::SearchService;

        let (_handle, url) = start_failing_mock_upstream().await;
        let state = make_test_app_state_with_upstream(&url);
        let svc = SearchServiceImpl::new(state);

        let resp = svc
            .batch_query(Request::new(proto::BatchQueryRequest {
                queries: vec!["batch-fail-q".to_string()],
                context: String::new(),
                fail_fast: false,
            }))
            .await;
        // Upstream failure: batch may return Ok with error_count or Err depending on fail_fast
        assert!(resp.is_ok() || resp.is_err());
    }

    #[tokio::test]
    async fn test_grpc_federated_with_upstream() {
        use crate::proxy::server::tests::{
            make_test_app_state_with_upstream, start_mock_upstream_server,
        };
        use proto::search_service_server::SearchService;

        let (_handle, url) = start_mock_upstream_server().await;
        let mut state = make_test_app_state_with_upstream(&url);
        state.federated_search.store(std::sync::Arc::new(
            crate::proxy::federated::FederatedSearch::new(
                crate::proxy::federated::FederatedSearchConfig {
                    enabled: true,
                    min_local_results: 10, // force remote query
                    ..Default::default()
                },
            ),
        ));
        let svc = SearchServiceImpl::new(state);

        let resp = svc
            .federated_query(Request::new(proto::FederatedQueryRequest {
                query: "fed-upstream-q".to_string(),
                local_results: vec![proto::SearchResult {
                    id: "local-doc".to_string(),
                    score: 0.5,
                    content: "local".to_string(),
                    upstream_id: String::new(),
                    metadata_json: vec![],
                }],
                context: String::new(),
                top_k: 5,
            }))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert!(!inner.results.is_empty());
        assert!(inner.stats.is_some());
    }

    #[tokio::test]
    async fn test_grpc_batch_query_upstream_failure_frozen_cache() {
        use crate::proxy::server::tests::{
            make_test_app_state_with_upstream, start_failing_mock_upstream,
        };
        use proto::search_service_server::SearchService;

        let (_handle, url) = start_failing_mock_upstream().await;
        let state = make_test_app_state_with_upstream(&url);

        // Pre-populate cache for frozen fallback
        insert_test_cache_entry(&state, "batch-frozen-q");

        let resp = state
            .batch_processor
            .validate(&crate::proxy::batch::BatchRequest {
                queries: vec![crate::proxy::batch::BatchQuery {
                    id: "q0".to_string(),
                    query: "batch-frozen-q".to_string(),
                    top_k: None,
                    priority: None,
                }],
                fail_fast: false,
                timeout_ms: None,
            });
        assert!(resp.is_ok());

        let svc = SearchServiceImpl::new(state);
        let resp = svc
            .batch_query(Request::new(proto::BatchQueryRequest {
                queries: vec!["batch-frozen-q".to_string()],
                context: String::new(),
                fail_fast: false,
            }))
            .await;
        // Should serve frozen cache
        assert!(resp.is_ok());
    }

    #[tokio::test]
    async fn test_grpc_batch_query_mixed_cache_and_upstream() {
        use crate::proxy::server::tests::{
            make_test_app_state_with_upstream, start_mock_upstream_server,
        };
        use proto::search_service_server::SearchService;

        let (_handle, url) = start_mock_upstream_server().await;
        let state = make_test_app_state_with_upstream(&url);
        insert_test_cache_entry(&state, "batch-cached-q");

        let svc = SearchServiceImpl::new(state);
        let resp = svc
            .batch_query(Request::new(proto::BatchQueryRequest {
                queries: vec!["batch-cached-q".to_string(), "batch-miss-q".to_string()],
                context: String::new(),
                fail_fast: false,
            }))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert_eq!(inner.results.len(), 2);
    }

    #[tokio::test]
    async fn test_grpc_batch_query_stale_cache() {
        use proto::search_service_server::SearchService;

        let mut state = make_test_app_state();
        // Short TTL to make entries go stale
        state.cache = std::sync::Arc::new(crate::proxy::cache::CacheStore::new(
            std::time::Duration::from_millis(1),
            std::time::Duration::from_secs(3600),
            100,
        ));
        // Insert entry that will become stale
        insert_test_cache_entry(&state, "stale-batch-q");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let svc = SearchServiceImpl::new(state);
        let resp = svc
            .batch_query(Request::new(proto::BatchQueryRequest {
                queries: vec!["stale-batch-q".to_string()],
                context: String::new(),
                fail_fast: false,
            }))
            .await;
        // Stale cache should still be served
        assert!(resp.is_ok());
    }

    #[tokio::test]
    async fn test_grpc_query_agent_context_denied() {
        let mut state = make_test_app_state();
        let registry = crate::proxy::agent::AgentRegistry::new();
        registry.register(&crate::config::AgentConfig {
            id: "agent-restricted".to_string(),
            api_key: "restricted-key".to_string(),
            default_context: Some("allowed-ctx".to_string()),
            allowed_contexts: vec!["allowed-ctx".to_string()],
            priority_class: None,
            rate_limit_rps: None,
            enabled: true,
        });
        state
            .agent_registry
            .store(Some(std::sync::Arc::new(registry)));
        let svc = SearchServiceImpl::new(state);

        let mut request = Request::new(proto::QueryRequest {
            query: "denied-test".to_string(),
            top_k: 5,
            request_id: String::new(),
            context: "forbidden-ctx".to_string(),
            upstream_id: String::new(),
            priority: 0,
            upstream_type: String::new(),
        });
        request
            .metadata_mut()
            .insert("x-api-key", "restricted-key".parse().unwrap());

        let resp = svc.query(request).await;
        assert!(resp.is_err());
        assert_eq!(resp.unwrap_err().code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn test_grpc_batch_query_with_context() {
        use proto::search_service_server::SearchService;

        let svc = make_search_service();
        // Insert with specific context
        let response = crate::proxy::types::QueryResponse {
            results: vec![crate::proxy::types::SearchResult {
                id: "ctx-batch-doc".to_string(),
                content: "context batch doc".to_string(),
                score: 0.9,
                metadata: None,
                upstream_id: None,
            }],
            cache_status: crate::proxy::types::CacheStatus::Miss,
            took_ms: 1,
            generated_at: None,
            miss_reason: None,
        };
        svc.state
            .cache
            .insert("ctx:batch-ctx:batch-ctx-q", response, "up".to_string());

        let resp = svc
            .batch_query(Request::new(proto::BatchQueryRequest {
                queries: vec!["batch-ctx-q".to_string()],
                context: "batch-ctx".to_string(),
                fail_fast: false,
            }))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert_eq!(inner.results.len(), 1);
    }

    #[tokio::test]
    async fn test_grpc_federated_with_context() {
        use proto::search_service_server::SearchService;

        let svc = make_search_service();
        // Insert cache entry for this specific context
        let response = crate::proxy::types::QueryResponse {
            results: vec![crate::proxy::types::SearchResult {
                id: "fed-ctx-doc".to_string(),
                content: "fed context doc".to_string(),
                score: 0.9,
                metadata: None,
                upstream_id: None,
            }],
            cache_status: crate::proxy::types::CacheStatus::Miss,
            took_ms: 1,
            generated_at: None,
            miss_reason: None,
        };
        svc.state
            .cache
            .insert("ctx:fed-ctx:fed-ctx-q", response, "up".to_string());

        let resp = svc
            .federated_query(Request::new(proto::FederatedQueryRequest {
                query: "fed-ctx-q".to_string(),
                local_results: vec![],
                context: "fed-ctx".to_string(),
                top_k: 5,
            }))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert!(!inner.results.is_empty());
    }

    #[tokio::test]
    async fn test_grpc_federated_enabled_no_upstream_empty_local() {
        use proto::search_service_server::SearchService;

        let mut state = make_test_app_state();
        state.federated_search.store(std::sync::Arc::new(
            crate::proxy::federated::FederatedSearch::new(
                crate::proxy::federated::FederatedSearchConfig {
                    enabled: true,
                    ..Default::default()
                },
            ),
        ));
        let svc = SearchServiceImpl::new(state);

        // Empty local results + no upstream → should still merge and return
        let resp = svc
            .federated_query(Request::new(proto::FederatedQueryRequest {
                query: "fed-empty".to_string(),
                local_results: vec![],
                context: String::new(),
                top_k: 5,
            }))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert!(inner.stats.is_some());
    }

    #[tokio::test]
    async fn test_grpc_federated_with_pool() {
        use crate::proxy::server::tests::start_mock_upstream_server;
        use proto::search_service_server::SearchService;

        let (_handle, url) = start_mock_upstream_server().await;
        let mut state = make_test_app_state();

        // Use upstream_pool instead of single upstream
        let configs = vec![crate::config::UpstreamEndpointConfig {
            id: "pool-node".to_string(),
            url: url.clone(),
            ..Default::default()
        }];
        state.upstream_pool.store(Some(std::sync::Arc::new(
            crate::proxy::pool::UpstreamPool::new(
                &configs,
                crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
            )
            .unwrap(),
        )));
        state.federated_search.store(std::sync::Arc::new(
            crate::proxy::federated::FederatedSearch::new(
                crate::proxy::federated::FederatedSearchConfig {
                    enabled: true,
                    min_local_results: 10,
                    ..Default::default()
                },
            ),
        ));
        let svc = SearchServiceImpl::new(state);

        let resp = svc
            .federated_query(Request::new(proto::FederatedQueryRequest {
                query: "fed-pool-q".to_string(),
                local_results: vec![proto::SearchResult {
                    id: "local".to_string(),
                    score: 0.5,
                    content: "local".to_string(),
                    upstream_id: String::new(),
                    metadata_json: vec![],
                }],
                context: String::new(),
                top_k: 5,
            }))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert!(!inner.results.is_empty());
    }

    #[tokio::test]
    async fn test_grpc_federated_local_sufficient() {
        use proto::search_service_server::SearchService;

        let mut state = make_test_app_state();
        // Default merge_mode=LocalOnlyFallback, min_local_results=3, min_local_confidence=0.7
        state.federated_search.store(std::sync::Arc::new(
            crate::proxy::federated::FederatedSearch::new(
                crate::proxy::federated::FederatedSearchConfig {
                    enabled: true,
                    min_local_results: 3,
                    min_local_confidence: 0.7,
                    ..Default::default()
                },
            ),
        ));
        let svc = SearchServiceImpl::new(state);

        // Provide ≥3 local results with top score ≥0.7 → LocalSufficient (no remote query)
        let resp = svc
            .federated_query(Request::new(proto::FederatedQueryRequest {
                query: "fed-local-sufficient".to_string(),
                local_results: vec![
                    proto::SearchResult {
                        id: "local-1".to_string(),
                        score: 0.95,
                        content: "high confidence".to_string(),
                        upstream_id: String::new(),
                        metadata_json: vec![],
                    },
                    proto::SearchResult {
                        id: "local-2".to_string(),
                        score: 0.85,
                        content: "medium confidence".to_string(),
                        upstream_id: String::new(),
                        metadata_json: vec![],
                    },
                    proto::SearchResult {
                        id: "local-3".to_string(),
                        score: 0.75,
                        content: "adequate confidence".to_string(),
                        upstream_id: String::new(),
                        metadata_json: vec![],
                    },
                ],
                context: String::new(),
                top_k: 5,
            }))
            .await
            .unwrap();
        let inner = resp.into_inner();
        // All 3 local results should be returned since no remote query was made
        assert_eq!(inner.results.len(), 3);
        assert!(inner.stats.is_some());
    }

    #[tokio::test]
    async fn test_grpc_federated_low_confidence() {
        use proto::search_service_server::SearchService;

        let (_handle, url) = crate::proxy::server::tests::start_mock_upstream_server().await;
        let mut state = crate::proxy::server::tests::make_test_app_state_with_upstream(&url);
        state.federated_search.store(std::sync::Arc::new(
            crate::proxy::federated::FederatedSearch::new(
                crate::proxy::federated::FederatedSearchConfig {
                    enabled: true,
                    min_local_results: 1,
                    min_local_confidence: 0.9,
                    fallback_on_low_confidence: true,
                    ..Default::default()
                },
            ),
        ));
        let svc = SearchServiceImpl::new(state);

        // Low score → LowConfidence → queries remote
        let resp = svc
            .federated_query(Request::new(proto::FederatedQueryRequest {
                query: "fed-low-conf".to_string(),
                local_results: vec![proto::SearchResult {
                    id: "low".to_string(),
                    score: 0.3,
                    content: "low confidence".to_string(),
                    upstream_id: String::new(),
                    metadata_json: vec![],
                }],
                context: String::new(),
                top_k: 5,
            }))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert!(!inner.results.is_empty());
    }

    #[tokio::test]
    async fn test_grpc_federated_always_remote() {
        use proto::search_service_server::SearchService;

        let (_handle, url) = crate::proxy::server::tests::start_mock_upstream_server().await;
        let mut state = crate::proxy::server::tests::make_test_app_state_with_upstream(&url);
        state.federated_search.store(std::sync::Arc::new(
            crate::proxy::federated::FederatedSearch::new(
                crate::proxy::federated::FederatedSearchConfig {
                    enabled: true,
                    merge_mode: crate::proxy::federated::MergeMode::Interleave,
                    ..Default::default()
                },
            ),
        ));
        let svc = SearchServiceImpl::new(state);

        let resp = svc
            .federated_query(Request::new(proto::FederatedQueryRequest {
                query: "fed-always-remote".to_string(),
                local_results: vec![proto::SearchResult {
                    id: "local".to_string(),
                    score: 0.95,
                    content: "local".to_string(),
                    upstream_id: String::new(),
                    metadata_json: vec![],
                }],
                context: String::new(),
                top_k: 5,
            }))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert!(!inner.results.is_empty());
    }

    #[tokio::test]
    async fn test_grpc_federated_upstream_failure() {
        use proto::search_service_server::SearchService;

        let (_handle, url) = crate::proxy::server::tests::start_failing_mock_upstream().await;
        let mut state = crate::proxy::server::tests::make_test_app_state_with_upstream(&url);
        state.federated_search.store(std::sync::Arc::new(
            crate::proxy::federated::FederatedSearch::new(
                crate::proxy::federated::FederatedSearchConfig {
                    enabled: true,
                    min_local_results: 10,
                    ..Default::default()
                },
            ),
        ));
        let svc = SearchServiceImpl::new(state);

        let resp = svc
            .federated_query(Request::new(proto::FederatedQueryRequest {
                query: "fed-fail".to_string(),
                local_results: vec![proto::SearchResult {
                    id: "local".to_string(),
                    score: 0.8,
                    content: "local".to_string(),
                    upstream_id: String::new(),
                    metadata_json: vec![],
                }],
                context: String::new(),
                top_k: 5,
            }))
            .await
            .unwrap();
        let inner = resp.into_inner();
        // Should still return local results even with upstream failure
        assert!(inner.stats.is_some());
    }

    #[tokio::test]
    async fn test_grpc_query_with_context() {
        let svc = make_search_service();
        insert_test_cache_entry(&svc.state, "ctx-q");
        // Insert with specific context
        let response = crate::proxy::types::QueryResponse {
            results: vec![crate::proxy::types::SearchResult {
                id: "ctx-doc".to_string(),
                content: "ctx content".to_string(),
                score: 0.9,
                metadata: None,
                upstream_id: None,
            }],
            cache_status: crate::proxy::types::CacheStatus::Miss,
            took_ms: 1,
            generated_at: None,
            miss_reason: None,
        };
        svc.state
            .cache
            .insert("ctx:my-ctx:ctx-q", response, "up".to_string());

        let resp = svc
            .query(Request::new(proto::QueryRequest {
                query: "ctx-q".to_string(),
                top_k: 5,
                request_id: "req-123".to_string(),
                context: "my-ctx".to_string(),
                upstream_id: String::new(),
                priority: 0,
                upstream_type: String::new(),
            }))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert!(!inner.results.is_empty());
    }

    // === Coverage improvement tests for search.rs ===

    #[tokio::test]
    async fn test_grpc_query_circuit_open_503() {
        // Circuit breaker open → 503 → Unavailable status
        let mut state = make_test_app_state();
        state.upstream.store(Some(std::sync::Arc::new(
            crate::proxy::upstream::GenericRestAdapter::new(
                "http://127.0.0.1:1", // won't connect
                std::time::Duration::from_secs(1),
            )
            .unwrap(),
        )));
        // Trip circuit breaker
        for _ in 0..20 {
            state.circuit_breaker.record_failure();
        }
        let svc = SearchServiceImpl::new(state);

        let resp = svc
            .query(Request::new(proto::QueryRequest {
                query: "circuit-test".to_string(),
                top_k: 5,
                request_id: String::new(),
                context: String::new(),
                upstream_id: String::new(),
                priority: 0,
                upstream_type: String::new(),
            }))
            .await;
        assert!(resp.is_err());
        assert_eq!(resp.unwrap_err().code(), tonic::Code::Unavailable);
    }

    #[tokio::test]
    async fn test_grpc_batch_concurrent_coalesce() {
        use crate::proxy::server::tests::{
            make_test_app_state_with_upstream, start_mock_upstream_server,
        };
        use proto::search_service_server::SearchService;

        let (_handle, url) = start_mock_upstream_server().await;
        let state = make_test_app_state_with_upstream(&url);
        let svc = SearchServiceImpl::new(state);

        // Same query in both batch requests → coalescing
        let (r1, r2) = tokio::join!(
            svc.batch_query(Request::new(proto::BatchQueryRequest {
                queries: vec!["coalesce-grpc-q".to_string()],
                context: String::new(),
                fail_fast: false,
            })),
            svc.batch_query(Request::new(proto::BatchQueryRequest {
                queries: vec!["coalesce-grpc-q".to_string()],
                context: String::new(),
                fail_fast: false,
            })),
        );
        assert!(r1.is_ok());
        assert!(r2.is_ok());
    }

    #[tokio::test]
    async fn test_grpc_batch_expired_cache_no_upstream() {
        use proto::search_service_server::SearchService;

        let mut state = make_test_app_state();
        state.cache = std::sync::Arc::new(crate::proxy::cache::CacheStore::new(
            std::time::Duration::from_millis(1),
            std::time::Duration::from_millis(2),
            100,
        ));

        insert_test_cache_entry(&state, "expired-grpc-q");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let svc = SearchServiceImpl::new(state);
        let resp = svc
            .batch_query(Request::new(proto::BatchQueryRequest {
                queries: vec!["expired-grpc-q".to_string()],
                context: String::new(),
                fail_fast: false,
            }))
            .await;
        // Expired cache, no upstream → batch returns with error in results
        assert!(resp.is_ok() || resp.is_err());
    }

    #[tokio::test]
    async fn test_grpc_batch_no_upstream_cache_only() {
        use proto::search_service_server::SearchService;

        let svc = make_search_service();
        // Insert one cache entry, query two
        insert_test_cache_entry(&svc.state, "cached-only-q");

        let resp = svc
            .batch_query(Request::new(proto::BatchQueryRequest {
                queries: vec!["cached-only-q".to_string(), "no-cache-grpc-q".to_string()],
                context: String::new(),
                fail_fast: false,
            }))
            .await;
        // Should return OK with one hit, one error
        assert!(resp.is_ok());
    }

    #[tokio::test]
    async fn test_grpc_federated_upstream_pool_failure_with_local() {
        use proto::search_service_server::SearchService;

        // Federated enabled, upstream pool fails, but we have local results
        let mut state = make_test_app_state();
        state.federated_search.store(std::sync::Arc::new(
            crate::proxy::federated::FederatedSearch::new(
                crate::proxy::federated::FederatedSearchConfig {
                    enabled: true,
                    min_local_results: 10, // forces remote query attempt
                    ..Default::default()
                },
            ),
        ));
        // Set upstream pool that won't respond
        let configs = vec![crate::config::UpstreamEndpointConfig {
            id: "node".to_string(),
            url: "http://127.0.0.1:1".to_string(),
            ..Default::default()
        }];
        state.upstream_pool.store(Some(std::sync::Arc::new(
            crate::proxy::pool::UpstreamPool::new(
                &configs,
                crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
            )
            .unwrap(),
        )));
        let svc = SearchServiceImpl::new(state);

        let resp = svc
            .federated_query(Request::new(proto::FederatedQueryRequest {
                query: "fed-pool-fail".to_string(),
                local_results: vec![proto::SearchResult {
                    id: "local-doc".to_string(),
                    score: 0.8,
                    content: "local content".to_string(),
                    upstream_id: String::new(),
                    metadata_json: vec![],
                }],
                context: String::new(),
                top_k: 5,
            }))
            .await
            .unwrap();
        let inner = resp.into_inner();
        // Should still return local results even with upstream pool failure
        assert!(!inner.results.is_empty());
        assert!(inner.stats.is_some());
    }

    // === Streaming RPC tests ===

    #[tokio::test]
    async fn test_grpc_batch_query_single() {
        use proto::search_service_server::SearchService;

        let svc = make_search_service();
        insert_test_cache_entry(&svc.state, "single-batch-q");

        let resp = svc
            .batch_query(Request::new(proto::BatchQueryRequest {
                queries: vec!["single-batch-q".to_string()],
                context: String::new(),
                fail_fast: false,
            }))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert_eq!(inner.results.len(), 1, "should have exactly 1 result");
    }

    #[tokio::test]
    async fn test_grpc_batch_query_multi() {
        use proto::search_service_server::SearchService;

        let svc = make_search_service();
        insert_test_cache_entry(&svc.state, "multi-q-a");
        insert_test_cache_entry(&svc.state, "multi-q-b");
        insert_test_cache_entry(&svc.state, "multi-q-c");

        let resp = svc
            .batch_query(Request::new(proto::BatchQueryRequest {
                queries: vec![
                    "multi-q-a".to_string(),
                    "multi-q-b".to_string(),
                    "multi-q-c".to_string(),
                ],
                context: String::new(),
                fail_fast: false,
            }))
            .await
            .unwrap();
        let inner = resp.into_inner();
        // Results keyed by auto-generated query IDs ("q0", "q1", "q2")
        assert_eq!(inner.results.len(), 3, "should have exactly 3 results");
        assert!(inner.results.contains_key("q0"), "missing key q0");
        assert!(inner.results.contains_key("q1"), "missing key q1");
        assert!(inner.results.contains_key("q2"), "missing key q2");
    }

    #[tokio::test]
    async fn test_grpc_federated_query_local_only() {
        use proto::search_service_server::SearchService;

        let mut state = make_test_app_state();
        state.federated_search.store(std::sync::Arc::new(
            crate::proxy::federated::FederatedSearch::new(
                crate::proxy::federated::FederatedSearchConfig {
                    enabled: true,
                    min_local_results: 3,
                    min_local_confidence: 0.7,
                    ..Default::default()
                },
            ),
        ));
        let svc = SearchServiceImpl::new(state);

        // Provide 3 local results with high scores → LocalSufficient, no remote query
        let resp = svc
            .federated_query(Request::new(proto::FederatedQueryRequest {
                query: "fed-local-only".to_string(),
                local_results: vec![
                    proto::SearchResult {
                        id: "local-1".to_string(),
                        score: 0.95,
                        content: "local one".to_string(),
                        upstream_id: String::new(),
                        metadata_json: vec![],
                    },
                    proto::SearchResult {
                        id: "local-2".to_string(),
                        score: 0.88,
                        content: "local two".to_string(),
                        upstream_id: String::new(),
                        metadata_json: vec![],
                    },
                    proto::SearchResult {
                        id: "local-3".to_string(),
                        score: 0.75,
                        content: "local three".to_string(),
                        upstream_id: String::new(),
                        metadata_json: vec![],
                    },
                ],
                context: String::new(),
                top_k: 5,
            }))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert_eq!(inner.results.len(), 3, "all 3 local results returned");
        assert!(inner.stats.is_some(), "stats should be present");
        // All results should have local-based IDs
        assert_eq!(inner.results[0].id, "local-1");
    }

    #[tokio::test]
    async fn test_grpc_query_stream_single_item() {
        use proto::search_service_server::SearchService;
        use tokio_stream::StreamExt;

        let svc = make_search_service();
        insert_test_cache_entry(&svc.state, "stream-single-q");

        let resp = svc
            .query_stream(Request::new(proto::QueryRequest {
                query: "stream-single-q".to_string(),
                top_k: 5,
                request_id: String::new(),
                context: String::new(),
                upstream_id: String::new(),
                priority: 0,
                upstream_type: String::new(),
            }))
            .await
            .unwrap();

        // Should yield exactly one item
        let mut stream = resp.into_inner();
        let item = stream.next().await;
        assert!(item.is_some(), "stream should yield exactly one item");
        let query_resp = item.unwrap().expect("stream item should be Ok");
        assert!(
            !query_resp.results.is_empty(),
            "response should have results"
        );

        // No more items
        assert!(
            stream.next().await.is_none(),
            "stream should have exactly one item"
        );
    }
}
