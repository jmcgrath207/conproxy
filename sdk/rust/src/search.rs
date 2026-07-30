use crate::auth::make_request;
use crate::client::ConproxyClient;
use crate::error::{from_tonic_status, SdkError};
use crate::proto;

impl ConproxyClient {
    /// Execute a simple text query.
    pub async fn query(&self, q: &str, top_k: u32) -> Result<proto::QueryResponse, SdkError> {
        let req = proto::QueryRequest {
            query: q.to_string(),
            top_k,
            ..Default::default()
        };
        self.query_full(req).await
    }

    /// Execute a query with full request control.
    pub async fn query_full(
        &self,
        req: proto::QueryRequest,
    ) -> Result<proto::QueryResponse, SdkError> {
        let grpc_req = make_request(req, &self.config.api_key);
        self.search
            .clone()
            .query(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Execute multiple queries in a single request.
    pub async fn batch_query(
        &self,
        queries: &[&str],
    ) -> Result<proto::BatchQueryResponse, SdkError> {
        let req = proto::BatchQueryRequest {
            queries: queries.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        let grpc_req = make_request(req, &self.config.api_key);
        self.search
            .clone()
            .batch_query(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Execute federated search combining local and remote results.
    pub async fn federated_query(
        &self,
        q: &str,
        local_results: Vec<proto::SearchResult>,
        top_k: u32,
    ) -> Result<proto::FederatedQueryResponse, SdkError> {
        let req = proto::FederatedQueryRequest {
            query: q.to_string(),
            local_results,
            top_k,
            ..Default::default()
        };
        let grpc_req = make_request(req, &self.config.api_key);
        self.search
            .clone()
            .federated_query(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }

    /// Stream results as they arrive from upstream (server-streaming).
    pub async fn query_stream(
        &self,
        q: &str,
        top_k: u32,
    ) -> Result<tonic::Streaming<proto::QueryResponse>, SdkError> {
        let req = proto::QueryRequest {
            query: q.to_string(),
            top_k,
            ..Default::default()
        };
        let grpc_req = make_request(req, &self.config.api_key);
        self.search
            .clone()
            .query_stream(grpc_req)
            .await
            .map(|r| r.into_inner())
            .map_err(from_tonic_status)
    }
}
