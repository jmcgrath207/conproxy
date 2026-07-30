use crate::auth::make_request;
use crate::client::ConproxyClient;
use crate::error::{from_tonic_status, SdkError};
use crate::proto;

impl ConproxyClient {
    /// List all available contexts.
    pub async fn list_contexts(&self) -> Result<proto::ListContextsResponse, SdkError> {
        let grpc_req = make_request(proto::ListContextsRequest {}, &self.config.api_key);
        self.ctx
            .clone()
            .list_contexts(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Get current context metadata.
    pub async fn current_context(&self) -> Result<proto::ContextInfo, SdkError> {
        let grpc_req = make_request(proto::GetCurrentContextRequest {}, &self.config.api_key);
        self.ctx
            .clone()
            .get_current_context(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Switch to a different context.
    pub async fn switch_context(
        &self,
        context_id: &str,
    ) -> Result<proto::SwitchContextResponse, SdkError> {
        let req = proto::SwitchContextRequest {
            context_id: context_id.to_string(),
        };
        let grpc_req = make_request(req, &self.config.api_key);
        self.ctx
            .clone()
            .switch_context(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Create a new context.
    pub async fn create_context(
        &self,
        context_id: &str,
    ) -> Result<proto::CreateContextResponse, SdkError> {
        let req = proto::CreateContextRequest {
            context_id: context_id.to_string(),
        };
        let grpc_req = make_request(req, &self.config.api_key);
        self.ctx
            .clone()
            .create_context(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Get per-context cache statistics.
    pub async fn context_stats(&self, context_id: &str) -> Result<proto::ContextStats, SdkError> {
        let req = proto::GetContextStatsRequest {
            context_id: context_id.to_string(),
        };
        let grpc_req = make_request(req, &self.config.api_key);
        self.ctx
            .clone()
            .get_context_stats(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }
}
