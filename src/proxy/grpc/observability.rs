//! ObservabilityService gRPC implementation.
//!
//! Stats, audit, circuit breaker, queue, clients, pool status.

use std::collections::HashMap;
use std::pin::Pin;
use std::time::Duration;

use tonic::{Request, Response, Status};

use super::proto;
use super::proto::observability_service_server::ObservabilityService;
use crate::proxy::server::AppState;

/// gRPC ObservabilityService implementation.
pub struct ObservabilityServiceImpl {
    pub(crate) state: AppState,
}

impl ObservabilityServiceImpl {
    pub(crate) fn new(state: AppState) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl ObservabilityService for ObservabilityServiceImpl {
    async fn get_stats(
        &self,
        _request: Request<proto::GetStatsRequest>,
    ) -> Result<Response<proto::StatsResponse>, Status> {
        let snapshot = self.state.metrics.snapshot();
        let paused = self.state.paused.load(std::sync::atomic::Ordering::Relaxed);
        let level_u8 = self
            .state
            .degradation_level
            .load(std::sync::atomic::Ordering::Relaxed);

        let level_str = match level_u8 {
            0 => "full",
            1 => "stale_serving",
            2 => "read_only",
            3 => "text_only",
            4 => "startup_failure",
            _ => "unknown",
        };

        Ok(Response::new(proto::StatsResponse {
            uptime_secs: self.state.start_time.elapsed().as_secs(),
            cache_entries: self.state.cache.len() as u64,
            total_hits: snapshot.cache_hits,
            total_misses: snapshot.cache_misses,
            hit_rate: snapshot.cache_hit_rate,
            upstream_requests: snapshot.upstream_requests,
            upstream_failures: snapshot.upstream_failures,
            upstream_error_rate: snapshot.upstream_error_rate,
            degradation_level: level_str.to_string(),
            paused,
        }))
    }

    async fn get_query_stats(
        &self,
        _request: Request<proto::GetQueryStatsRequest>,
    ) -> Result<Response<proto::QueryStatsResponse>, Status> {
        let top_queries = self.state.query_stats.top_by_count(100);
        let json_bytes = serde_json::to_vec(&top_queries).unwrap_or_default();
        Ok(Response::new(proto::QueryStatsResponse {
            stats_json: json_bytes,
        }))
    }

    async fn get_audit(
        &self,
        _request: Request<proto::GetAuditRequest>,
    ) -> Result<Response<proto::AuditResponse>, Status> {
        let entries = self.state.audit_log.recent(100);
        let json_bytes = serde_json::to_vec(&entries).unwrap_or_default();
        Ok(Response::new(proto::AuditResponse {
            entries_json: json_bytes,
        }))
    }

    async fn get_circuit_status(
        &self,
        _request: Request<proto::GetCircuitStatusRequest>,
    ) -> Result<Response<proto::CircuitStatusResponse>, Status> {
        let state = self.state.circuit_breaker.state();
        let failure_count = self.state.circuit_breaker.failure_count();
        let times_opened = self.state.circuit_breaker.times_opened();

        Ok(Response::new(proto::CircuitStatusResponse {
            state: format!("{:?}", state),
            failure_count: failure_count as u64,
            success_count: times_opened, // using times_opened as a proxy
            consecutive_failures: failure_count as u64,
        }))
    }

    async fn get_queue_stats(
        &self,
        _request: Request<proto::GetQueueStatsRequest>,
    ) -> Result<Response<proto::QueueStatsResponse>, Status> {
        let stats = self.state.request_queue.stats();
        let total = stats.total as u64;
        let max = stats.max_size as u64;
        let util = if max > 0 {
            total as f64 / max as f64
        } else {
            0.0
        };

        Ok(Response::new(proto::QueueStatsResponse {
            pending: total,
            capacity: max,
            utilization: util,
        }))
    }

    async fn get_clients(
        &self,
        _request: Request<proto::GetClientsRequest>,
    ) -> Result<Response<proto::ClientsResponse>, Status> {
        let clients = self
            .state
            .client_tracker
            .snapshot()
            .into_iter()
            .map(|c| proto::ClientInfo {
                request_id: c.request_id,
                started_at_ms: c.started_at_ms,
                query: c.query,
                source: c.source,
            })
            .collect();

        let total_completed = self
            .state
            .client_tracker
            .total_completed
            .load(std::sync::atomic::Ordering::Relaxed);
        let total_rejected = self
            .state
            .client_tracker
            .total_rejected
            .load(std::sync::atomic::Ordering::Relaxed);

        Ok(Response::new(proto::ClientsResponse {
            clients,
            total_completed,
            total_rejected,
        }))
    }
    async fn get_pool_status(
        &self,
        _request: Request<proto::GetPoolStatusRequest>,
    ) -> Result<Response<proto::PoolStatusResponse>, Status> {
        let pool = self.state.upstream_pool.load_full();
        if let Some(ref pool) = pool {
            let stats: crate::proxy::pool::PoolStats = pool.stats();
            let upstreams = pool
                .all()
                .iter()
                .map(|u| proto::UpstreamInfo {
                    id: u.id.clone(),
                    url: String::new(), // adapter trait doesn't expose base_url generically
                    health_status: format!("{:?}", u.health.status()),
                    weight: u.weight,
                    priority: u.priority,
                })
                .collect();

            Ok(Response::new(proto::PoolStatusResponse {
                total_upstreams: stats.total_upstreams as u32,
                healthy_upstreams: stats.healthy_upstreams as u32,
                degraded_upstreams: stats.degraded_upstreams as u32,
                offline_upstreams: stats.offline_upstreams as u32,
                upstreams,
            }))
        } else {
            Ok(Response::new(proto::PoolStatusResponse {
                total_upstreams: if self.state.upstream.load_full().is_some() {
                    1
                } else {
                    0
                },
                healthy_upstreams: if self.state.upstream.load_full().is_some() {
                    1
                } else {
                    0
                },
                degraded_upstreams: 0,
                offline_upstreams: 0,
                upstreams: vec![],
            }))
        }
    }

    async fn get_cache_upstreams(
        &self,
        _request: Request<proto::GetCacheUpstreamsRequest>,
    ) -> Result<Response<proto::CacheUpstreamsResponse>, Status> {
        let stats_by_upstream = self.state.cache.stats_by_upstream();
        let upstreams = stats_by_upstream
            .into_iter()
            .map(|(id, stats)| {
                (
                    id,
                    proto::UpstreamCacheStats {
                        total: stats.total as u64,
                        fresh: stats.fresh as u64,
                        stale: stats.stale as u64,
                        expired: stats.expired as u64,
                        memory_bytes: stats.memory_bytes as u64,
                    },
                )
            })
            .collect();

        Ok(Response::new(proto::CacheUpstreamsResponse { upstreams }))
    }

    type GetCacheDistillStream =
        Pin<Box<dyn tokio_stream::Stream<Item = Result<proto::DistillEntry, Status>> + Send>>;

    async fn get_cache_distill(
        &self,
        request: Request<proto::DistillRequest>,
    ) -> Result<Response<Self::GetCacheDistillStream>, Status> {
        let req = request.into_inner();
        let cache: &crate::proxy::cache::CacheStore = &self.state.cache;
        let max_frozen = cache.max_frozen_duration();

        // Pull all rich snapshots, then filter, sort, and truncate in-memory.
        // For the typical cache size this is cheap; the streaming wrapper
        // exists so callers can consume large dumps lazily.
        let mut entries = cache.snapshot_entries_rich();

        // 1. Context filter (empty string = all contexts).
        if !req.context.is_empty() {
            entries.retain(|e| e.context_id == req.context);
        }

        // 2. TTL gate: drop frozen entries (past max_frozen) and, unless
        //    include_stale, drop entries that have gone stale.
        let fresh_dur = cache.fresh_duration();
        entries.retain(|e| {
            let elapsed = e.cached_at_wall.elapsed().unwrap_or(Duration::ZERO);
            if elapsed > max_frozen {
                return false;
            }
            if !req.include_stale {
                let jittered = cache.jittered_ttl(fresh_dur, &e.hash);
                if elapsed > jittered {
                    return false;
                }
            }
            true
        });

        // 3. Sort by insertion time ascending (oldest first).
        entries.sort_by_key(|e| e.cached_at_wall);

        // 4. Truncate.
        if req.limit > 0 && entries.len() > req.limit as usize {
            entries.truncate(req.limit as usize);
        }

        // 5. Optional semantic join (tier 1 or 2) — only when a semantic
        //    cache is configured. tier=0 means primary only (no embedding).
        //    The `semantic_cache()` accessor is gated on `embed-api`; without
        //    that feature the index is always empty (tier>0 silently degrades
        //    to primary-only behavior, which is the safe default).
        #[cfg(feature = "embed-api")]
        let semantic_index: HashMap<[u8; 32], Vec<f32>> = if req.tier != 0 {
            cache
                .semantic_cache()
                .map(|sc| {
                    sc.snapshot()
                        .into_iter()
                        .map(|(hash, emb, _seq)| (hash, emb))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            HashMap::new()
        };
        #[cfg(not(feature = "embed-api"))]
        let semantic_index: HashMap<[u8; 32], Vec<f32>> = HashMap::new();

        // 6. Map to proto entries.
        let include_embedding = req.tier != 0;
        let proto_entries: Vec<Result<proto::DistillEntry, Status>> = entries
            .into_iter()
            .map(|e| {
                let cached_at_ms = e
                    .cached_at_wall
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let hash_hex: String = e.hash.iter().map(|b| format!("{:02x}", b)).collect();
                let embedding = if include_embedding {
                    semantic_index.get(&e.hash).cloned().unwrap_or_default()
                } else {
                    Vec::new()
                };
                Ok(proto::DistillEntry {
                    query: e.query_text,
                    context_id: e.context_id,
                    upstream_id: e.upstream_id,
                    cached_at_ms,
                    extended_count: e.extended_count,
                    response_json: e.response_json,
                    hash_hex,
                    embedding,
                })
            })
            .collect();

        let stream = tokio_stream::iter(proto_entries);
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

    fn make_obs_service() -> ObservabilityServiceImpl {
        ObservabilityServiceImpl::new(make_test_app_state())
    }

    #[tokio::test]
    async fn test_grpc_get_stats() {
        let svc = make_obs_service();
        let resp = svc
            .get_stats(Request::new(proto::GetStatsRequest {}))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert_eq!(inner.cache_entries, 0);
        assert!(!inner.paused);
    }

    #[tokio::test]
    async fn test_grpc_get_query_stats() {
        let svc = make_obs_service();
        let resp = svc
            .get_query_stats(Request::new(proto::GetQueryStatsRequest {}))
            .await
            .unwrap();
        let _inner = resp.into_inner();
    }

    #[tokio::test]
    async fn test_grpc_get_audit() {
        let svc = make_obs_service();
        let resp = svc
            .get_audit(Request::new(proto::GetAuditRequest {}))
            .await
            .unwrap();
        let _inner = resp.into_inner();
    }

    #[tokio::test]
    async fn test_grpc_get_circuit_status() {
        let svc = make_obs_service();
        let resp = svc
            .get_circuit_status(Request::new(proto::GetCircuitStatusRequest {}))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert!(inner.state.contains("Closed"));
    }

    #[tokio::test]
    async fn test_grpc_get_queue_stats() {
        let svc = make_obs_service();
        let resp = svc
            .get_queue_stats(Request::new(proto::GetQueueStatsRequest {}))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert_eq!(inner.pending, 0);
    }

    #[tokio::test]
    async fn test_grpc_get_clients() {
        let svc = make_obs_service();
        let resp = svc
            .get_clients(Request::new(proto::GetClientsRequest {}))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert!(inner.clients.is_empty());
    }

    #[tokio::test]
    async fn test_grpc_get_pool_status_no_pool() {
        let svc = make_obs_service();
        let resp = svc
            .get_pool_status(Request::new(proto::GetPoolStatusRequest {}))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert_eq!(inner.total_upstreams, 0);
    }

    #[tokio::test]
    async fn test_grpc_get_cache_upstreams() {
        let svc = make_obs_service();
        let resp = svc
            .get_cache_upstreams(Request::new(proto::GetCacheUpstreamsRequest {}))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert!(inner.upstreams.is_empty());
    }

    #[tokio::test]
    async fn test_grpc_get_stats_degradation_stale_serving() {
        let state = make_test_app_state();
        state
            .degradation_level
            .store(1, std::sync::atomic::Ordering::Relaxed);
        let svc = ObservabilityServiceImpl::new(state);
        let resp = svc
            .get_stats(Request::new(proto::GetStatsRequest {}))
            .await
            .unwrap();
        assert_eq!(resp.into_inner().degradation_level, "stale_serving");
    }

    #[tokio::test]
    async fn test_grpc_get_stats_degradation_read_only() {
        let state = make_test_app_state();
        state
            .degradation_level
            .store(2, std::sync::atomic::Ordering::Relaxed);
        let svc = ObservabilityServiceImpl::new(state);
        let resp = svc
            .get_stats(Request::new(proto::GetStatsRequest {}))
            .await
            .unwrap();
        assert_eq!(resp.into_inner().degradation_level, "read_only");
    }

    #[tokio::test]
    async fn test_grpc_get_stats_degradation_text_only() {
        let state = make_test_app_state();
        state
            .degradation_level
            .store(3, std::sync::atomic::Ordering::Relaxed);
        let svc = ObservabilityServiceImpl::new(state);
        let resp = svc
            .get_stats(Request::new(proto::GetStatsRequest {}))
            .await
            .unwrap();
        assert_eq!(resp.into_inner().degradation_level, "text_only");
    }

    #[tokio::test]
    async fn test_grpc_get_stats_degradation_startup_failure() {
        let state = make_test_app_state();
        state
            .degradation_level
            .store(4, std::sync::atomic::Ordering::Relaxed);
        let svc = ObservabilityServiceImpl::new(state);
        let resp = svc
            .get_stats(Request::new(proto::GetStatsRequest {}))
            .await
            .unwrap();
        assert_eq!(resp.into_inner().degradation_level, "startup_failure");
    }

    #[tokio::test]
    async fn test_grpc_get_stats_degradation_unknown() {
        let state = make_test_app_state();
        state
            .degradation_level
            .store(255, std::sync::atomic::Ordering::Relaxed);
        let svc = ObservabilityServiceImpl::new(state);
        let resp = svc
            .get_stats(Request::new(proto::GetStatsRequest {}))
            .await
            .unwrap();
        assert_eq!(resp.into_inner().degradation_level, "unknown");
    }

    #[tokio::test]
    async fn test_grpc_get_stats_paused() {
        let state = make_test_app_state();
        state
            .paused
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let svc = ObservabilityServiceImpl::new(state);
        let resp = svc
            .get_stats(Request::new(proto::GetStatsRequest {}))
            .await
            .unwrap();
        assert!(resp.into_inner().paused);
    }

    #[tokio::test]
    async fn test_grpc_get_pool_status_with_upstream() {
        let state = make_test_app_state();
        state.upstream.store(Some(std::sync::Arc::new(
            crate::proxy::upstream::GenericRestAdapter::new(
                "http://localhost:9999",
                std::time::Duration::from_secs(5),
            )
            .unwrap(),
        )));
        let svc = ObservabilityServiceImpl::new(state);
        let resp = svc
            .get_pool_status(Request::new(proto::GetPoolStatusRequest {}))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert_eq!(inner.total_upstreams, 1);
        assert_eq!(inner.healthy_upstreams, 1);
    }

    #[tokio::test]
    async fn test_grpc_get_pool_status_with_pool() {
        let state = make_test_app_state();
        let configs = vec![crate::config::UpstreamEndpointConfig {
            id: "pool-node".to_string(),
            url: "http://localhost:9999".to_string(),
            ..Default::default()
        }];
        state.upstream_pool.store(Some(std::sync::Arc::new(
            crate::proxy::pool::UpstreamPool::new(
                &configs,
                crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
            )
            .unwrap(),
        )));
        let svc = ObservabilityServiceImpl::new(state);
        let resp = svc
            .get_pool_status(Request::new(proto::GetPoolStatusRequest {}))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert_eq!(inner.total_upstreams, 1);
        assert_eq!(inner.upstreams.len(), 1);
        assert_eq!(inner.upstreams[0].id, "pool-node");
    }

    #[tokio::test]
    async fn test_grpc_get_cache_upstreams_with_data() {
        let state = make_test_app_state();
        // Insert cache entries so stats_by_upstream has data
        let response = crate::proxy::types::QueryResponse {
            results: vec![crate::proxy::types::SearchResult {
                id: "doc".to_string(),
                content: "data".to_string(),
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
            .insert("ctx:default:test", response, "up-1".to_string());

        let svc = ObservabilityServiceImpl::new(state);
        let resp = svc
            .get_cache_upstreams(Request::new(proto::GetCacheUpstreamsRequest {}))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert!(!inner.upstreams.is_empty());
    }

    #[tokio::test]
    async fn test_grpc_get_queue_stats_with_items() {
        let state = make_test_app_state();
        // Push items into the queue
        state.request_queue.push(
            crate::proxy::types::QueryRequest {
                query: "q1".to_string(),
                top_k: None,
                priority: None,
                upstream_id: None,
                upstream_type: None,
            },
            crate::proxy::Priority::Normal,
        );
        let svc = ObservabilityServiceImpl::new(state);
        let resp = svc
            .get_queue_stats(Request::new(proto::GetQueueStatsRequest {}))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert_eq!(inner.pending, 1);
        assert!(inner.utilization > 0.0);
    }

    // --- get_cache_distill tests ---

    /// Build a test `QueryResponse` with one result, deterministic enough for assertions.
    fn make_distill_response(content: &str) -> crate::proxy::types::QueryResponse {
        crate::proxy::types::QueryResponse {
            results: vec![crate::proxy::types::SearchResult {
                id: "doc-1".to_string(),
                score: 0.9,
                content: content.to_string(),
                metadata: None,
                upstream_id: None,
            }],
            cache_status: crate::proxy::types::CacheStatus::Hit,
            took_ms: 1,
            generated_at: None,
            miss_reason: None,
        }
    }

    #[tokio::test]
    async fn test_grpc_get_cache_distill_empty() {
        use proto::observability_service_server::ObservabilityService;
        let svc = make_obs_service();
        let resp = svc
            .get_cache_distill(Request::new(proto::DistillRequest {
                context: String::new(),
                tier: 0,
                limit: 0,
                include_stale: false,
            }))
            .await
            .unwrap();
        use tokio_stream::StreamExt;
        let mut stream = resp.into_inner();
        let item = stream.next().await;
        assert!(item.is_none(), "empty cache should yield no stream items");
    }

    #[tokio::test]
    async fn test_grpc_get_cache_distill_single_entry() {
        use proto::observability_service_server::ObservabilityService;
        let svc = make_obs_service();
        svc.state.cache.insert(
            "ctx:default:single-q",
            make_distill_response("hello world"),
            "up-a".to_string(),
        );

        let resp = svc
            .get_cache_distill(Request::new(proto::DistillRequest {
                context: String::new(),
                tier: 0,
                limit: 0,
                include_stale: false,
            }))
            .await
            .unwrap();
        use tokio_stream::StreamExt;
        let mut stream = resp.into_inner();
        let item = stream.next().await.expect("expected one item");
        let entry = item.unwrap();
        assert_eq!(entry.upstream_id, "up-a");
        assert_eq!(entry.context_id, "default");
        assert!(!entry.hash_hex.is_empty());
        assert_eq!(entry.hash_hex.len(), 64); // 32 bytes * 2 hex chars
        assert!(entry.cached_at_ms > 0);
        // No more items after the first.
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_grpc_get_cache_distill_sort_order() {
        use proto::observability_service_server::ObservabilityService;
        let svc = make_obs_service();
        // Insert in chronological order: alpha, beta, gamma, delta.
        // The handler sorts by cached_at_wall ascending, so the order
        // in the stream should match insertion order.
        for q in &["alpha", "beta", "gamma", "delta"] {
            svc.state.cache.insert(
                &format!("ctx:default:{}", q),
                make_distill_response(q),
                "up".to_string(),
            );
            // 1ms gap so the wall-clock timestamps are strictly ordered
            // even on coarse-resolution clocks.
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }

        let resp = svc
            .get_cache_distill(Request::new(proto::DistillRequest {
                context: String::new(),
                tier: 0,
                limit: 0,
                include_stale: false,
            }))
            .await
            .unwrap();
        use tokio_stream::StreamExt;
        let mut stream = resp.into_inner();
        let mut queries = Vec::new();
        while let Some(item) = stream.next().await {
            queries.push(item.unwrap().query);
        }
        assert_eq!(queries.len(), 4);
        assert!(queries[0].ends_with("alpha"));
        assert!(queries[1].ends_with("beta"));
        assert!(queries[2].ends_with("gamma"));
        assert!(queries[3].ends_with("delta"));
    }

    #[tokio::test]
    async fn test_grpc_get_cache_distill_context_filter() {
        use proto::observability_service_server::ObservabilityService;
        let svc = make_obs_service();
        // Inserts use the public API which hardcodes context_id="default".
        svc.state.cache.insert(
            "ctx:default:q1",
            make_distill_response("q1"),
            "up".to_string(),
        );
        svc.state.cache.insert(
            "ctx:default:q2",
            make_distill_response("q2"),
            "up".to_string(),
        );

        // Filter to "default" -> all 2 entries pass.
        let resp = svc
            .get_cache_distill(Request::new(proto::DistillRequest {
                context: "default".to_string(),
                tier: 0,
                limit: 0,
                include_stale: false,
            }))
            .await
            .unwrap();
        use tokio_stream::StreamExt;
        let mut stream = resp.into_inner();
        let mut count = 0;
        while stream.next().await.is_some() {
            count += 1;
        }
        assert_eq!(count, 2);

        // Filter to a different context -> 0 entries.
        let resp = svc
            .get_cache_distill(Request::new(proto::DistillRequest {
                context: "other".to_string(),
                tier: 0,
                limit: 0,
                include_stale: false,
            }))
            .await
            .unwrap();
        let mut stream = resp.into_inner();
        assert!(stream.next().await.is_none());

        // Empty context = no filter -> all 2 entries.
        let resp = svc
            .get_cache_distill(Request::new(proto::DistillRequest {
                context: String::new(),
                tier: 0,
                limit: 0,
                include_stale: false,
            }))
            .await
            .unwrap();
        let mut stream = resp.into_inner();
        let mut count = 0;
        while stream.next().await.is_some() {
            count += 1;
        }
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn test_grpc_get_cache_distill_limit_truncate() {
        use proto::observability_service_server::ObservabilityService;
        let svc = make_obs_service();
        for i in 0..5 {
            svc.state.cache.insert(
                &format!("ctx:default:q{}", i),
                make_distill_response("x"),
                "up".to_string(),
            );
        }

        let resp = svc
            .get_cache_distill(Request::new(proto::DistillRequest {
                context: String::new(),
                tier: 0,
                limit: 2,
                include_stale: false,
            }))
            .await
            .unwrap();
        use tokio_stream::StreamExt;
        let mut stream = resp.into_inner();
        let mut count = 0;
        while stream.next().await.is_some() {
            count += 1;
        }
        assert_eq!(count, 2, "limit=2 should truncate 5 entries to 2");
    }

    #[tokio::test]
    async fn test_grpc_get_cache_distill_stale_gate() {
        use proto::observability_service_server::ObservabilityService;
        // Build a state with a tiny fresh_duration so we can age entries
        // without sleeping for hours.
        let mut state = make_test_app_state();
        state.cache = std::sync::Arc::new(crate::proxy::cache::CacheStore::new(
            std::time::Duration::from_millis(20),
            std::time::Duration::from_secs(3600),
            100,
        ));
        state.cache.insert(
            "ctx:default:stale-q",
            make_distill_response("stale"),
            "up".to_string(),
        );
        // Wait past the fresh window (with margin for jitter).
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;

        let svc = ObservabilityServiceImpl::new(state);

        // include_stale=false -> entry is past fresh_duration, so it's dropped.
        let resp = svc
            .get_cache_distill(Request::new(proto::DistillRequest {
                context: String::new(),
                tier: 0,
                limit: 0,
                include_stale: false,
            }))
            .await
            .unwrap();
        use tokio_stream::StreamExt;
        let mut stream = resp.into_inner();
        assert!(
            stream.next().await.is_none(),
            "stale entry should be filtered out when include_stale=false"
        );

        // include_stale=true -> entry is returned even though it's past fresh.
        let resp = svc
            .get_cache_distill(Request::new(proto::DistillRequest {
                context: String::new(),
                tier: 0,
                limit: 0,
                include_stale: true,
            }))
            .await
            .unwrap();
        let mut stream = resp.into_inner();
        let item = stream.next().await;
        assert!(
            item.is_some(),
            "stale entry should pass when include_stale=true"
        );
    }

    #[cfg(feature = "embed-api")]
    #[tokio::test]
    async fn test_grpc_get_cache_distill_with_semantic_embedding() {
        use proto::observability_service_server::ObservabilityService;
        use std::sync::Arc;
        use std::time::Duration;
        use tokio_stream::StreamExt;

        let semantic = Arc::new(crate::proxy::semantic_cache::SemanticCache::new(0.92, 1000));
        let cache = Arc::new(
            crate::proxy::cache::CacheStore::new(
                Duration::from_secs(300),
                Duration::from_secs(3600),
                100,
            )
            .with_semantic_cache(Arc::clone(&semantic)),
        );

        let mut state = make_test_app_state();
        state.cache = cache;
        #[cfg(feature = "embed-api")]
        {
            state.semantic_cache = Some(semantic.clone());
        }

        // Insert cache entry for "sem-emb-q"
        let query = "sem-emb-q";
        let response = crate::proxy::types::QueryResponse {
            results: vec![crate::proxy::types::SearchResult {
                id: "sem-doc".to_string(),
                content: "semantic embedding test".to_string(),
                score: 0.9,
                metadata: None,
                upstream_id: None,
            }],
            cache_status: crate::proxy::types::CacheStatus::Hit,
            took_ms: 1,
            generated_at: None,
            miss_reason: None,
        };
        state.cache.insert(
            &format!("ctx:default:{query}"),
            response,
            "up-sem".to_string(),
        );

        // Compute the same hash (cache stores entries keyed by "ctx:default:{query}")
        let cache_key = format!("ctx:default:{query}");
        let hash = crate::proxy::cache::CacheStore::hash_query(&cache_key);
        let embedding: Vec<f32> = vec![0.1, 0.2, 0.3, 0.4, 0.5];
        semantic.insert(hash, embedding);

        let svc = ObservabilityServiceImpl::new(state);
        let resp = svc
            .get_cache_distill(Request::new(proto::DistillRequest {
                context: String::new(),
                tier: 1,
                limit: 0,
                include_stale: false,
            }))
            .await
            .unwrap();
        let mut stream = resp.into_inner();
        let item = stream.next().await.expect("should have one entry");
        let entry = item.unwrap();
        assert!(
            !entry.embedding.is_empty(),
            "embedding should be populated when tier=1 and semantic cache has matching hash"
        );
        assert_eq!(entry.embedding.len(), 5);
        assert!((entry.embedding[0] - 0.1).abs() < 1e-6);
    }

    #[cfg(feature = "embed-api")]
    #[tokio::test]
    async fn test_grpc_orphan_embedding_not_emitted() {
        use proto::observability_service_server::ObservabilityService;
        use std::sync::Arc;
        use std::time::Duration;
        use tokio_stream::StreamExt;

        let semantic = Arc::new(crate::proxy::semantic_cache::SemanticCache::new(0.92, 1000));
        let cache = Arc::new(
            crate::proxy::cache::CacheStore::new(
                Duration::from_secs(300),
                Duration::from_secs(3600),
                100,
            )
            .with_semantic_cache(Arc::clone(&semantic)),
        );

        let mut state = make_test_app_state();
        state.cache = cache;
        #[cfg(feature = "embed-api")]
        {
            state.semantic_cache = Some(Arc::clone(&semantic));
        }

        // Insert a cache entry for "real-q"
        let response = crate::proxy::types::QueryResponse {
            results: vec![crate::proxy::types::SearchResult {
                id: "real-doc".to_string(),
                content: "real content".to_string(),
                score: 0.9,
                metadata: None,
                upstream_id: None,
            }],
            cache_status: crate::proxy::types::CacheStatus::Hit,
            took_ms: 1,
            generated_at: None,
            miss_reason: None,
        };
        state
            .cache
            .insert("ctx:default:real-q", response, "up-real".to_string());

        // Insert a semantic entry with a hash that does NOT match any cache entry
        let orphan_hash = [0xdeu8; 32];
        semantic.insert(orphan_hash, vec![0.9, 0.8, 0.7]);

        let svc = ObservabilityServiceImpl::new(state);
        let resp = svc
            .get_cache_distill(Request::new(proto::DistillRequest {
                context: String::new(),
                tier: 1,
                limit: 0,
                include_stale: false,
            }))
            .await
            .unwrap();

        let mut stream = resp.into_inner();
        // Only "real-q" should be emitted; orphan hash entry has no matching cache entry
        let item = stream.next().await.expect("real-q should be emitted");
        let entry = item.unwrap();
        assert_eq!(
            entry.query, "ctx:default:real-q",
            "real-q should be present"
        );
        // Orphan embedding not associated with any cache entry → embedding should be empty
        assert!(
            entry.embedding.is_empty(),
            "orphan hash should not produce embedding"
        );

        // No more entries
        assert!(
            stream.next().await.is_none(),
            "only one entry should be emitted"
        );
    }
}
