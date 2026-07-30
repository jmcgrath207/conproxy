use crate::auth::make_request;
use crate::client::ConproxyClient;
use crate::error::{from_tonic_status, SdkError};
use crate::proto;
use tokio_stream::StreamExt;

impl ConproxyClient {
    /// Get server and cache statistics.
    pub async fn stats(&self) -> Result<proto::StatsResponse, SdkError> {
        let grpc_req = make_request(proto::GetStatsRequest {}, &self.config.api_key);
        self.obs
            .clone()
            .get_stats(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Get query access statistics and hot queries.
    pub async fn query_stats(&self) -> Result<proto::QueryStatsResponse, SdkError> {
        let grpc_req = make_request(proto::GetQueryStatsRequest {}, &self.config.api_key);
        self.obs
            .clone()
            .get_query_stats(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Get recent request audit log.
    pub async fn audit(&self) -> Result<proto::AuditResponse, SdkError> {
        let grpc_req = make_request(proto::GetAuditRequest {}, &self.config.api_key);
        self.obs
            .clone()
            .get_audit(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Get circuit breaker status.
    pub async fn circuit_status(&self) -> Result<proto::CircuitStatusResponse, SdkError> {
        let grpc_req = make_request(proto::GetCircuitStatusRequest {}, &self.config.api_key);
        self.obs
            .clone()
            .get_circuit_status(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Get request queue status.
    pub async fn queue_stats(&self) -> Result<proto::QueueStatsResponse, SdkError> {
        let grpc_req = make_request(proto::GetQueueStatsRequest {}, &self.config.api_key);
        self.obs
            .clone()
            .get_queue_stats(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Get active client connections.
    pub async fn clients(&self) -> Result<proto::ClientsResponse, SdkError> {
        let grpc_req = make_request(proto::GetClientsRequest {}, &self.config.api_key);
        self.obs
            .clone()
            .get_clients(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Get upstream pool status.
    pub async fn pool_status(&self) -> Result<proto::PoolStatusResponse, SdkError> {
        let grpc_req = make_request(proto::GetPoolStatusRequest {}, &self.config.api_key);
        self.obs
            .clone()
            .get_pool_status(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Get cache statistics by upstream.
    pub async fn cache_upstreams(&self) -> Result<proto::CacheUpstreamsResponse, SdkError> {
        let grpc_req = make_request(proto::GetCacheUpstreamsRequest {}, &self.config.api_key);
        self.obs
            .clone()
            .get_cache_upstreams(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Stream cache entries for offline dump / LLM ingestion.
    ///
    /// Returns the full response as a `Vec` (the server stream is small enough
    /// that a Python SDK caller rarely needs incremental iteration). Pass an
    /// empty `context` string to include entries from all contexts; `tier=0`
    /// is the primary cache, `tier=1` is the semantic tier, `tier=2` is both;
    /// `limit=0` means unlimited; `include_stale=false` drops entries past the
    /// stale TTL (the default).
    pub async fn distill(
        &self,
        context: String,
        tier: u32,
        limit: u32,
        include_stale: bool,
    ) -> Result<Vec<proto::DistillEntry>, SdkError> {
        let req = proto::DistillRequest {
            context,
            tier,
            limit,
            include_stale,
        };
        let grpc_req = make_request(req, &self.config.api_key);
        let mut stream = self
            .obs
            .clone()
            .get_cache_distill(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)?;
        let mut out = Vec::new();
        while let Some(item) = stream.next().await {
            out.push(item.map_err(from_tonic_status)?);
        }
        Ok(out)
    }
}
