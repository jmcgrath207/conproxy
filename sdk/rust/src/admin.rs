use crate::auth::make_request;
use crate::client::ConproxyClient;
use crate::error::{from_tonic_status, SdkError};
use crate::proto;

impl ConproxyClient {
    /// Hot-reload configuration from disk.
    pub async fn reload(&self) -> Result<proto::ReloadResponse, SdkError> {
        let grpc_req = make_request(proto::ReloadRequest {}, &self.config.api_key);
        self.admin
            .clone()
            .reload(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Pause the proxy (reject new queries, drain in-flight).
    pub async fn pause(&self) -> Result<proto::PauseResponse, SdkError> {
        let grpc_req = make_request(proto::PauseRequest {}, &self.config.api_key);
        self.admin
            .clone()
            .pause(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Resume accepting queries.
    pub async fn resume(&self) -> Result<proto::ResumeResponse, SdkError> {
        let grpc_req = make_request(proto::ResumeRequest {}, &self.config.api_key);
        self.admin
            .clone()
            .resume(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Clear all cache entries.
    pub async fn cache_clear(&self) -> Result<proto::CacheClearResponse, SdkError> {
        let grpc_req = make_request(proto::CacheClearRequest {}, &self.config.api_key);
        self.admin
            .clone()
            .cache_clear(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Pre-fetch queries to populate cache.
    pub async fn cache_warmup(
        &self,
        queries: &[&str],
    ) -> Result<proto::CacheWarmupResponse, SdkError> {
        let req = proto::CacheWarmupRequest {
            queries: queries.iter().map(|s| s.to_string()).collect(),
            fetch_from_upstream: true,
        };
        let grpc_req = make_request(req, &self.config.api_key);
        self.admin
            .clone()
            .cache_warmup(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Selectively evict cache entries.
    pub async fn cache_evict(
        &self,
        upstream_id: &str,
        max_entries: u32,
        expired_only: bool,
    ) -> Result<proto::CacheEvictResponse, SdkError> {
        let req = proto::CacheEvictRequest {
            upstream_id: upstream_id.to_string(),
            max_entries,
            expired_only,
        };
        let grpc_req = make_request(req, &self.config.api_key);
        self.admin
            .clone()
            .cache_evict(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Verify cache entry integrity.
    pub async fn cache_integrity(&self) -> Result<proto::CacheIntegrityResponse, SdkError> {
        let grpc_req = make_request(proto::CacheIntegrityRequest {}, &self.config.api_key);
        self.admin
            .clone()
            .cache_integrity(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Reset all metrics counters.
    pub async fn metrics_reset(&self) -> Result<proto::MetricsResetResponse, SdkError> {
        let grpc_req = make_request(proto::MetricsResetRequest {}, &self.config.api_key);
        self.admin
            .clone()
            .metrics_reset(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// List all registered agents.
    pub async fn list_agents(&self) -> Result<proto::ListAgentsResponse, SdkError> {
        let grpc_req = make_request(proto::ListAgentsRequest {}, &self.config.api_key);
        self.admin
            .clone()
            .list_agents(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Register a new agent.
    pub async fn create_agent(
        &self,
        req: proto::CreateAgentRequest,
    ) -> Result<proto::CreateAgentResponse, SdkError> {
        let grpc_req = make_request(req, &self.config.api_key);
        self.admin
            .clone()
            .create_agent(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Remove an agent.
    pub async fn delete_agent(
        &self,
        agent_id: &str,
    ) -> Result<proto::DeleteAgentResponse, SdkError> {
        let req = proto::DeleteAgentRequest {
            agent_id: agent_id.to_string(),
        };
        let grpc_req = make_request(req, &self.config.api_key);
        self.admin
            .clone()
            .delete_agent(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Rotate an agent's API key.
    pub async fn rotate_key(
        &self,
        agent_id: &str,
        new_api_key: &str,
    ) -> Result<proto::RotateKeyResponse, SdkError> {
        let req = proto::RotateKeyRequest {
            agent_id: agent_id.to_string(),
            new_api_key: new_api_key.to_string(),
        };
        let grpc_req = make_request(req, &self.config.api_key);
        self.admin
            .clone()
            .rotate_key(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }
}
