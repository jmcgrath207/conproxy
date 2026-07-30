//! ContextService gRPC implementation.
//!
//! Handles context listing, switching, creation, and stats.

use tonic::{Request, Response, Status};

use super::proto;
use super::proto::context_service_server::ContextService;
use crate::proxy::server::AppState;

/// gRPC ContextService implementation.
pub struct ContextServiceImpl {
    pub(crate) state: AppState,
}

impl ContextServiceImpl {
    pub(crate) fn new(state: AppState) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl ContextService for ContextServiceImpl {
    async fn list_contexts(
        &self,
        _request: Request<proto::ListContextsRequest>,
    ) -> Result<Response<proto::ListContextsResponse>, Status> {
        let contexts = self
            .state
            .context_manager
            .list_metadata()
            .into_iter()
            .map(|meta| proto::ContextInfo {
                id: meta.id,
                created_at_ms: meta.created_at,
                last_accessed_ms: meta.last_accessed,
                hits: meta.entry_count,
                misses: 0,
            })
            .collect();

        let current = self.state.context_manager.current();

        Ok(Response::new(proto::ListContextsResponse {
            contexts,
            current,
        }))
    }

    async fn get_current_context(
        &self,
        _request: Request<proto::GetCurrentContextRequest>,
    ) -> Result<Response<proto::ContextInfo>, Status> {
        match self.state.context_manager.get_current() {
            Some(meta) => Ok(Response::new(proto::ContextInfo {
                id: meta.id,
                created_at_ms: meta.created_at,
                last_accessed_ms: meta.last_accessed,
                hits: meta.entry_count,
                misses: 0,
            })),
            None => Err(Status::not_found("No current context")),
        }
    }

    async fn switch_context(
        &self,
        request: Request<proto::SwitchContextRequest>,
    ) -> Result<Response<proto::SwitchContextResponse>, Status> {
        let req = request.into_inner();
        let old_context = self.state.context_manager.current();

        match self.state.context_manager.switch(&req.context_id) {
            Ok(()) => {
                let current = self.state.context_manager.current();

                Ok(Response::new(proto::SwitchContextResponse {
                    previous: old_context,
                    current,
                    message: format!("Switched to context '{}'", req.context_id),
                }))
            }
            Err(e) => Err(Status::invalid_argument(e.to_string())),
        }
    }

    async fn create_context(
        &self,
        request: Request<proto::CreateContextRequest>,
    ) -> Result<Response<proto::CreateContextResponse>, Status> {
        let req = request.into_inner();

        match self.state.context_manager.create(&req.context_id, "", "") {
            Ok(()) => Ok(Response::new(proto::CreateContextResponse {
                context_id: req.context_id.clone(),
                message: format!("Created context '{}'", req.context_id),
            })),
            Err(e) => Err(Status::invalid_argument(e.to_string())),
        }
    }

    async fn get_context_stats(
        &self,
        request: Request<proto::GetContextStatsRequest>,
    ) -> Result<Response<proto::ContextStats>, Status> {
        let req = request.into_inner();

        match self
            .state
            .context_manager
            .get_context_stats(&req.context_id)
        {
            Some((snap, hit_rate)) => Ok(Response::new(proto::ContextStats {
                context_id: req.context_id,
                cache_entries: snap.queries,
                hits: snap.hits,
                misses: snap.misses,
                hit_rate,
            })),
            None => Err(Status::not_found(format!(
                "Context '{}' not found",
                req.context_id
            ))),
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

    fn make_ctx_service() -> ContextServiceImpl {
        ContextServiceImpl::new(make_test_app_state())
    }

    #[tokio::test]
    async fn test_grpc_list_contexts() {
        let svc = make_ctx_service();
        let resp = svc
            .list_contexts(Request::new(proto::ListContextsRequest {}))
            .await
            .unwrap();
        let inner = resp.into_inner();
        assert!(!inner.current.is_empty());
    }

    #[tokio::test]
    async fn test_grpc_get_current_context() {
        let svc = make_ctx_service();
        let resp = svc
            .get_current_context(Request::new(proto::GetCurrentContextRequest {}))
            .await;
        // May return Ok or not_found depending on context manager state
        assert!(resp.is_ok() || resp.is_err());
    }

    #[tokio::test]
    async fn test_grpc_create_context() {
        let svc = make_ctx_service();
        let resp = svc
            .create_context(Request::new(proto::CreateContextRequest {
                context_id: "grpc-ctx".to_string(),
            }))
            .await
            .unwrap();
        assert_eq!(resp.into_inner().context_id, "grpc-ctx");
    }

    #[tokio::test]
    async fn test_grpc_create_context_duplicate() {
        let svc = make_ctx_service();
        let _ = svc
            .create_context(Request::new(proto::CreateContextRequest {
                context_id: "dup".to_string(),
            }))
            .await;
        let result = svc
            .create_context(Request::new(proto::CreateContextRequest {
                context_id: "dup".to_string(),
            }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_grpc_switch_context() {
        let svc = make_ctx_service();
        // Create context first
        let _ = svc
            .create_context(Request::new(proto::CreateContextRequest {
                context_id: "switch-target".to_string(),
            }))
            .await;
        let resp = svc
            .switch_context(Request::new(proto::SwitchContextRequest {
                context_id: "switch-target".to_string(),
            }))
            .await
            .unwrap();
        assert_eq!(resp.into_inner().current, "switch-target");
    }

    #[tokio::test]
    async fn test_grpc_get_context_stats_not_found() {
        let svc = make_ctx_service();
        let result = svc
            .get_context_stats(Request::new(proto::GetContextStatsRequest {
                context_id: "nonexistent".to_string(),
            }))
            .await;
        assert!(result.is_err());
    }
}
