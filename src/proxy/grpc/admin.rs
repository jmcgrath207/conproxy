//! AdminService gRPC implementation.
//!
//! Handles config reload, pause/resume, cache operations, and agent CRUD.

use tonic::{Request, Response, Status};

use super::proto;
use super::proto::admin_service_server::AdminService;
use crate::proxy::server::AppState;

/// gRPC AdminService implementation.
pub struct AdminServiceImpl {
    pub(crate) state: AppState,
}

impl AdminServiceImpl {
    pub(crate) fn new(state: AppState) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl AdminService for AdminServiceImpl {
    async fn reload(
        &self,
        _request: Request<proto::ReloadRequest>,
    ) -> Result<Response<proto::ReloadResponse>, Status> {
        match self.state.apply_reload() {
            Ok(summary) => {
                let count = summary.sections.len();
                let message = if summary.restart_required.is_empty() {
                    format!(
                        "Reloaded {} section(s): {}",
                        count,
                        summary.sections.join(", ")
                    )
                } else {
                    format!(
                        "Reloaded {} section(s): {}. Restart required for: {}",
                        count,
                        summary.sections.join(", "),
                        summary.restart_required.join("; ")
                    )
                };
                Ok(Response::new(proto::ReloadResponse {
                    success: true,
                    reloaded: summary.sections,
                    error: String::new(),
                    message,
                    restart_required: summary.restart_required,
                }))
            }
            Err(e) => Ok(Response::new(proto::ReloadResponse {
                success: false,
                reloaded: vec![],
                error: e.clone(),
                message: format!("Config reload failed: {e}"),
                restart_required: vec![],
            })),
        }
    }

    async fn pause(
        &self,
        _request: Request<proto::PauseRequest>,
    ) -> Result<Response<proto::PauseResponse>, Status> {
        self.state
            .paused
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let active = self.state.client_tracker.active_count();
        Ok(Response::new(proto::PauseResponse {
            status: "paused".to_string(),
            active_requests: active as u32,
            message: "Proxy is paused. New queries will be rejected.".to_string(),
        }))
    }

    async fn resume(
        &self,
        _request: Request<proto::ResumeRequest>,
    ) -> Result<Response<proto::ResumeResponse>, Status> {
        self.state
            .paused
            .store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(Response::new(proto::ResumeResponse {
            status: "resumed".to_string(),
            message: "Proxy is accepting queries again.".to_string(),
        }))
    }

    async fn cache_clear(
        &self,
        _request: Request<proto::CacheClearRequest>,
    ) -> Result<Response<proto::CacheClearResponse>, Status> {
        let count = self.state.cache.len();
        self.state.cache.clear();
        Ok(Response::new(proto::CacheClearResponse {
            cleared_entries: count as u64,
            message: "Cache cleared successfully".to_string(),
        }))
    }

    async fn cache_warmup(
        &self,
        request: Request<proto::CacheWarmupRequest>,
    ) -> Result<Response<proto::CacheWarmupResponse>, Status> {
        let req = request.into_inner();
        let start = std::time::Instant::now();

        self.state.metrics.record_warmup_request();

        if !req.fetch_from_upstream {
            return Ok(Response::new(proto::CacheWarmupResponse {
                warmed: 0,
                failed: 0,
                took_ms: start.elapsed().as_millis() as u64,
            }));
        }

        let has_upstream = self.state.upstream_pool.load_full().is_some()
            || self.state.upstream.load_full().is_some();
        if !has_upstream {
            return Ok(Response::new(proto::CacheWarmupResponse {
                warmed: 0,
                failed: req.queries.len() as u32,
                took_ms: start.elapsed().as_millis() as u64,
            }));
        }

        let mut warmed = 0u32;
        let mut failed = 0u32;
        let ctx_id = self.state.context_manager.current();

        for query in req.queries {
            let ctx_q = format!("ctx:{}:{}", ctx_id, query);
            if let Some(crate::proxy::types::Freshness::Fresh) =
                self.state.cache.check_freshness(&ctx_q)
            {
                warmed = warmed.saturating_add(1);
                continue;
            }

            let query_req = crate::proxy::types::QueryRequest {
                query: query.clone(),
                top_k: None,
                priority: None,
                upstream_id: None,
                upstream_type: None,
            };

            let pool_opt = self.state.upstream_pool.load_full();
            let upstream_opt = self.state.upstream.load_full();
            let result = if let Some(ref pool) = pool_opt {
                pool.query(&query_req).await
            } else if let Some(ref upstream) = upstream_opt {
                upstream.query(&query_req).await
            } else {
                unreachable!("has_upstream guard ensures pool or upstream is Some")
            };

            match result {
                Ok(mut response) => {
                    let scope_filter = self.state.scope_filter_for(&ctx_id);
                    response.results = crate::proxy::server::query_core::apply_scope_filter(
                        &scope_filter,
                        response.results,
                        #[cfg(feature = "embed-api")]
                        self.state.smart_embedder.as_deref(),
                    )
                    .await;
                    if response.validate().is_ok() {
                        self.state
                            .cache
                            .insert(&ctx_q, response, self.state.upstream_id.clone());
                        self.state.metrics.record_warmup_entry();
                        warmed = warmed.saturating_add(1);
                    } else {
                        failed = failed.saturating_add(1);
                    }
                }
                Err(_) => failed = failed.saturating_add(1),
            }
        }

        Ok(Response::new(proto::CacheWarmupResponse {
            warmed,
            failed,
            took_ms: start.elapsed().as_millis() as u64,
        }))
    }

    async fn cache_evict(
        &self,
        request: Request<proto::CacheEvictRequest>,
    ) -> Result<Response<proto::CacheEvictResponse>, Status> {
        let req = request.into_inner();

        let evicted = if req.expired_only {
            self.state.cache.evict_truly_expired()
        } else if !req.upstream_id.is_empty() {
            if req.max_entries > 0 {
                let current = self.state.cache.count_for_upstream(&req.upstream_id);
                self.state.cache.enforce_per_upstream_limit(
                    &req.upstream_id,
                    current.saturating_sub(req.max_entries as usize),
                )
            } else {
                let count = self.state.cache.count_for_upstream(&req.upstream_id);
                self.state
                    .cache
                    .evict_from_upstream(&req.upstream_id, count)
            }
        } else {
            self.state.cache.evict_truly_expired()
        };

        Ok(Response::new(proto::CacheEvictResponse {
            evicted: evicted as u64,
            remaining: self.state.cache.len() as u64,
        }))
    }

    async fn cache_integrity(
        &self,
        _request: Request<proto::CacheIntegrityRequest>,
    ) -> Result<Response<proto::CacheIntegrityResponse>, Status> {
        let report = self.state.cache.verify_integrity();
        if report.invalid > 0 {
            for _ in 0..report.invalid {
                self.state.metrics.record_integrity_failure();
            }
        }
        Ok(Response::new(proto::CacheIntegrityResponse {
            total: report.total as u64,
            valid: report.valid as u64,
            invalid: report.invalid as u64,
            no_hash: report.missing_hash as u64,
        }))
    }

    async fn metrics_reset(
        &self,
        _request: Request<proto::MetricsResetRequest>,
    ) -> Result<Response<proto::MetricsResetResponse>, Status> {
        self.state.metrics.reset();
        self.state.context_manager.reset_all_stats();
        Ok(Response::new(proto::MetricsResetResponse {
            status: "reset".to_string(),
            message: "All metrics counters reset to zero.".to_string(),
        }))
    }

    async fn list_agents(
        &self,
        _request: Request<proto::ListAgentsRequest>,
    ) -> Result<Response<proto::ListAgentsResponse>, Status> {
        let registry = self.state.agent_registry.load_full();
        let agents = match registry.as_deref() {
            Some(registry) => registry
                .list_agents()
                .into_iter()
                .map(|a| proto::AgentInfo {
                    id: a.id,
                    default_context: String::new(),
                    allowed_contexts: a.allowed_contexts,
                    priority_class: a.priority_class,
                    rate_limit_rps: a.rate_limit_rps.unwrap_or(0),
                    enabled: a.enabled,
                })
                .collect(),
            None => vec![],
        };
        let total = agents.len() as u32;
        Ok(Response::new(proto::ListAgentsResponse { agents, total }))
    }

    async fn create_agent(
        &self,
        request: Request<proto::CreateAgentRequest>,
    ) -> Result<Response<proto::CreateAgentResponse>, Status> {
        let Some(ref registry) = self.state.agent_registry.load_full() else {
            return Err(Status::failed_precondition("Agent registry not configured"));
        };

        let req = request.into_inner();
        let config = crate::config::AgentConfig {
            id: req.id.clone(),
            api_key: req.api_key,
            default_context: None,
            allowed_contexts: req.allowed_contexts,
            priority_class: if req.priority_class > 0 {
                Some(req.priority_class)
            } else {
                None
            },
            rate_limit_rps: if req.rate_limit_rps > 0 {
                Some(req.rate_limit_rps)
            } else {
                None
            },
            enabled: true,
        };

        registry.register(&config);
        Ok(Response::new(proto::CreateAgentResponse {
            status: "created".to_string(),
            agent_id: req.id,
        }))
    }

    async fn delete_agent(
        &self,
        request: Request<proto::DeleteAgentRequest>,
    ) -> Result<Response<proto::DeleteAgentResponse>, Status> {
        let Some(ref registry) = self.state.agent_registry.load_full() else {
            return Err(Status::failed_precondition("Agent registry not configured"));
        };

        let req = request.into_inner();
        if registry.remove_by_id(&req.agent_id) {
            Ok(Response::new(proto::DeleteAgentResponse {
                status: "deleted".to_string(),
                agent_id: req.agent_id,
            }))
        } else {
            Err(Status::not_found("Agent not found"))
        }
    }

    async fn rotate_key(
        &self,
        request: Request<proto::RotateKeyRequest>,
    ) -> Result<Response<proto::RotateKeyResponse>, Status> {
        let Some(ref registry) = self.state.agent_registry.load_full() else {
            return Err(Status::failed_precondition("Agent registry not configured"));
        };

        let req = request.into_inner();
        if registry.rotate_key(&req.agent_id, req.new_api_key) {
            Ok(Response::new(proto::RotateKeyResponse {
                status: "rotated".to_string(),
                agent_id: req.agent_id,
            }))
        } else {
            Err(Status::not_found("Agent not found"))
        }
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

    fn make_admin_service() -> AdminServiceImpl {
        AdminServiceImpl::new(make_test_app_state())
    }

    #[tokio::test]
    async fn test_grpc_pause() {
        let svc = make_admin_service();
        let resp = svc
            .pause(Request::new(proto::PauseRequest {}))
            .await
            .unwrap();
        assert_eq!(resp.into_inner().status, "paused");
    }

    #[tokio::test]
    async fn test_grpc_resume() {
        let svc = make_admin_service();
        svc.pause(Request::new(proto::PauseRequest {}))
            .await
            .unwrap();
        let resp = svc
            .resume(Request::new(proto::ResumeRequest {}))
            .await
            .unwrap();
        assert_eq!(resp.into_inner().status, "resumed");
    }

    #[tokio::test]
    async fn test_grpc_cache_clear() {
        let svc = make_admin_service();
        let resp = svc
            .cache_clear(Request::new(proto::CacheClearRequest {}))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert_eq!(inner.cleared_entries, 0);
    }

    #[tokio::test]
    async fn test_grpc_cache_warmup_no_fetch() {
        let svc = make_admin_service();
        let resp = svc
            .cache_warmup(Request::new(proto::CacheWarmupRequest {
                queries: vec!["q1".to_string()],
                fetch_from_upstream: false,
            }))
            .await
            .unwrap();
        assert_eq!(resp.into_inner().warmed, 0);
    }

    #[tokio::test]
    async fn test_grpc_cache_warmup_no_upstream() {
        let svc = make_admin_service();
        let resp = svc
            .cache_warmup(Request::new(proto::CacheWarmupRequest {
                queries: vec!["q1".to_string()],
                fetch_from_upstream: true,
            }))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert_eq!(inner.failed, 1);
    }

    #[tokio::test]
    async fn test_grpc_cache_evict_expired() {
        let svc = make_admin_service();
        let resp = svc
            .cache_evict(Request::new(proto::CacheEvictRequest {
                expired_only: true,
                upstream_id: String::new(),
                max_entries: 0,
            }))
            .await
            .unwrap();
        assert_eq!(resp.into_inner().evicted, 0);
    }

    #[tokio::test]
    async fn test_grpc_cache_evict_by_upstream() {
        let svc = make_admin_service();
        let resp = svc
            .cache_evict(Request::new(proto::CacheEvictRequest {
                expired_only: false,
                upstream_id: "up-1".to_string(),
                max_entries: 0,
            }))
            .await
            .unwrap();
        assert_eq!(resp.into_inner().evicted, 0);
    }

    #[tokio::test]
    async fn test_grpc_cache_evict_with_max_entries() {
        let svc = make_admin_service();
        let resp = svc
            .cache_evict(Request::new(proto::CacheEvictRequest {
                expired_only: false,
                upstream_id: "up-1".to_string(),
                max_entries: 10,
            }))
            .await
            .unwrap();
        assert_eq!(resp.into_inner().evicted, 0);
    }

    #[tokio::test]
    async fn test_grpc_cache_integrity() {
        let svc = make_admin_service();
        let resp = svc
            .cache_integrity(Request::new(proto::CacheIntegrityRequest {}))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert_eq!(inner.total, 0);
        assert_eq!(inner.invalid, 0);
    }

    #[tokio::test]
    async fn test_grpc_metrics_reset() {
        let svc = make_admin_service();
        let resp = svc
            .metrics_reset(Request::new(proto::MetricsResetRequest {}))
            .await
            .unwrap();
        assert_eq!(resp.into_inner().status, "reset");
    }

    #[tokio::test]
    async fn test_grpc_reload() {
        let svc = make_admin_service();
        let result = svc.reload(Request::new(proto::ReloadRequest {})).await;
        // Reload is now wired to apply_reload — either succeeds or returns an
        // Ok response with success=false (no reload_source in the test app
        // state is fine; the RPC layer must always return Ok).
        assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());
        let resp = result.unwrap().into_inner();
        // Either succeeded with empty reloaded list, or failed because no
        // reload_source is configured. Both are valid outcomes.
        assert!(resp.success || !resp.error.is_empty());
    }

    #[tokio::test]
    async fn test_grpc_list_agents_empty() {
        let svc = make_admin_service();
        let resp = svc
            .list_agents(Request::new(proto::ListAgentsRequest {}))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert_eq!(inner.total, 0);
        assert!(inner.agents.is_empty());
    }

    #[tokio::test]
    async fn test_grpc_list_agents_with_registry() {
        let state = make_test_app_state();
        let registry = crate::proxy::agent::AgentRegistry::new();
        registry.register(&crate::config::AgentConfig {
            id: "a1".to_string(),
            api_key: "k1".to_string(),
            default_context: None,
            allowed_contexts: vec!["ctx".to_string()],
            priority_class: Some(5),
            rate_limit_rps: Some(100),
            enabled: true,
        });
        state
            .agent_registry
            .store(Some(std::sync::Arc::new(registry)));
        let svc = AdminServiceImpl::new(state);

        let resp = svc
            .list_agents(Request::new(proto::ListAgentsRequest {}))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert_eq!(inner.total, 1);
        assert_eq!(inner.agents[0].id, "a1");
        assert_eq!(inner.agents[0].priority_class, 5);
    }

    #[tokio::test]
    async fn test_grpc_create_agent_no_registry() {
        let svc = make_admin_service();
        let result = svc
            .create_agent(Request::new(proto::CreateAgentRequest {
                id: "a".to_string(),
                api_key: "k".to_string(),
                allowed_contexts: vec![],
                priority_class: 0,
                rate_limit_rps: 0,
            }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_grpc_create_agent_success() {
        let state = make_test_app_state();
        state.agent_registry.store(Some(std::sync::Arc::new(
            crate::proxy::agent::AgentRegistry::new(),
        )));
        let svc = AdminServiceImpl::new(state);

        let resp = svc
            .create_agent(Request::new(proto::CreateAgentRequest {
                id: "new-agent".to_string(),
                api_key: "key".to_string(),
                allowed_contexts: vec![],
                priority_class: 0,
                rate_limit_rps: 0,
            }))
            .await
            .unwrap();
        assert_eq!(resp.into_inner().status, "created");
    }

    #[tokio::test]
    async fn test_grpc_delete_agent_no_registry() {
        let svc = make_admin_service();
        let result = svc
            .delete_agent(Request::new(proto::DeleteAgentRequest {
                agent_id: "any".to_string(),
            }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_grpc_delete_agent_not_found() {
        let state = make_test_app_state();
        state.agent_registry.store(Some(std::sync::Arc::new(
            crate::proxy::agent::AgentRegistry::new(),
        )));
        let svc = AdminServiceImpl::new(state);

        let result = svc
            .delete_agent(Request::new(proto::DeleteAgentRequest {
                agent_id: "nope".to_string(),
            }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_grpc_rotate_key_no_registry() {
        let svc = make_admin_service();
        let result = svc
            .rotate_key(Request::new(proto::RotateKeyRequest {
                agent_id: "any".to_string(),
                new_api_key: "k".to_string(),
            }))
            .await;
        assert!(result.is_err());
    }
}
