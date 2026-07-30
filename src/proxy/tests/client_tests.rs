#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

#[test]
fn test_client_config_default() {
    let config = ClientConfig::default();
    assert_eq!(config.base_url, "http://127.0.0.1:9999");
    assert_eq!(config.timeout, Duration::from_secs(30));
    assert!(config.api_key.is_none());
}

#[test]
fn test_client_config_new() {
    let config = ClientConfig::new("http://localhost:9999");
    assert_eq!(config.base_url, "http://localhost:9999");
}

#[test]
fn test_client_config_builder() {
    let config = ClientConfig::new("http://localhost:8080")
        .with_timeout(Duration::from_secs(10))
        .with_api_key("secret");

    assert_eq!(config.base_url, "http://localhost:8080");
    assert_eq!(config.timeout, Duration::from_secs(10));
    assert_eq!(config.api_key, Some("secret".to_string()));
}

#[test]
fn test_client_error_display() {
    assert_eq!(
        ClientError::Connection("refused".to_string()).to_string(),
        "Connection error: refused"
    );
    assert_eq!(
        ClientError::Request(500, "internal error".to_string()).to_string(),
        "Request error (500): internal error"
    );
    assert_eq!(
        ClientError::Parse("invalid json".to_string()).to_string(),
        "Parse error: invalid json"
    );
    assert_eq!(ClientError::Timeout.to_string(), "Request timed out");
}

#[tokio::test]
async fn test_proxy_client_creation() {
    let client = ProxyClient::local();
    assert!(client.is_ok());
}

#[tokio::test]
async fn test_proxy_client_connect() {
    let client = ProxyClient::connect("http://localhost:9999");
    assert!(client.is_ok());
}

#[tokio::test]
async fn test_proxy_client_with_api_key() {
    let config = ClientConfig::new("http://localhost:8080").with_api_key("my-secret-key");
    let client = ProxyClient::new(config);
    assert!(client.is_ok());
}

#[test]
fn test_client_error_is_std_error() {
    let err: Box<dyn std::error::Error> = Box::new(ClientError::Connection("test".to_string()));
    assert!(err.to_string().contains("Connection error"));
}

#[test]
fn test_client_error_debug() {
    let err = ClientError::Timeout;
    let debug_str = format!("{:?}", err);
    assert!(debug_str.contains("Timeout"));
}

#[test]
fn test_client_config_with_timeout_only() {
    let config =
        ClientConfig::new("http://example.com:9999").with_timeout(Duration::from_millis(500));
    assert_eq!(config.timeout, Duration::from_millis(500));
    assert!(config.api_key.is_none());
}

#[test]
fn test_client_config_builder_chain() {
    let config = ClientConfig::new("http://host:9000")
        .with_timeout(Duration::from_secs(5))
        .with_api_key("key123");
    assert_eq!(config.base_url, "http://host:9000");
    assert_eq!(config.timeout, Duration::from_secs(5));
    assert_eq!(config.api_key.as_deref(), Some("key123"));
}

#[test]
fn test_client_error_display_all_variants() {
    let errors = vec![
        (
            ClientError::Connection("conn refused".to_string()),
            "Connection error: conn refused",
        ),
        (
            ClientError::Request(404, "not found".to_string()),
            "Request error (404): not found",
        ),
        (
            ClientError::Request(429, "rate limited".to_string()),
            "Request error (429): rate limited",
        ),
        (
            ClientError::Parse("unexpected EOF".to_string()),
            "Parse error: unexpected EOF",
        ),
        (ClientError::Timeout, "Request timed out"),
    ];
    for (err, expected) in errors {
        assert_eq!(err.to_string(), expected);
    }
}

#[test]
fn test_client_config_clone() {
    let config = ClientConfig::new("http://localhost:3000").with_api_key("key");
    let cloned = config.clone();
    assert_eq!(config.base_url, cloned.base_url);
    assert_eq!(config.timeout, cloned.timeout);
    assert_eq!(config.api_key, cloned.api_key);
}

#[test]
fn test_client_config_debug() {
    let config = ClientConfig::default();
    let debug_str = format!("{:?}", config);
    assert!(debug_str.contains("127.0.0.1:9999"));
}

#[test]
fn test_client_config_http_url_default() {
    let config = ClientConfig::new("http://127.0.0.1:9999");
    assert_eq!(config.http_base_url(), "http://127.0.0.1:10000");
}

#[test]
fn test_client_config_http_url_explicit() {
    let config = ClientConfig::new("http://127.0.0.1:9999").with_http_url("http://127.0.0.1:3001");
    assert_eq!(config.http_base_url(), "http://127.0.0.1:3001");
}

#[test]
fn test_tonic_status_code_mapping() {
    assert_eq!(tonic_status_to_http(tonic::Code::Ok), 200);
    assert_eq!(tonic_status_to_http(tonic::Code::InvalidArgument), 400);
    assert_eq!(tonic_status_to_http(tonic::Code::Unauthenticated), 401);
    assert_eq!(tonic_status_to_http(tonic::Code::PermissionDenied), 403);
    assert_eq!(tonic_status_to_http(tonic::Code::NotFound), 404);
    assert_eq!(tonic_status_to_http(tonic::Code::ResourceExhausted), 429);
    assert_eq!(tonic_status_to_http(tonic::Code::Internal), 500);
    assert_eq!(tonic_status_to_http(tonic::Code::Unavailable), 503);
    assert_eq!(tonic_status_to_http(tonic::Code::DeadlineExceeded), 504);
}

// === gRPC-based tests for ProxyClient async methods ===

/// Start a mock gRPC server with SearchService, AdminService, and ObservabilityService.
async fn start_mock_grpc_server() -> (tokio::task::JoinHandle<()>, String) {
    use crate::proxy::grpc::proto;
    use crate::proxy::grpc::proto::admin_service_server::{AdminService, AdminServiceServer};
    use crate::proxy::grpc::proto::observability_service_server::{
        ObservabilityService, ObservabilityServiceServer,
    };
    use crate::proxy::grpc::proto::search_service_server::{SearchService, SearchServiceServer};

    // Minimal mock implementations
    struct MockSearch;
    struct MockAdmin;
    struct MockObs;

    #[tonic::async_trait]
    impl SearchService for MockSearch {
        async fn query(
            &self,
            _request: tonic::Request<proto::QueryRequest>,
        ) -> Result<tonic::Response<proto::QueryResponse>, tonic::Status> {
            Ok(tonic::Response::new(proto::QueryResponse {
                results: vec![proto::SearchResult {
                    id: "doc-1".to_string(),
                    content: "Hello world".to_string(),
                    score: 0.95,
                    metadata_json: vec![],
                    upstream_id: String::new(),
                }],
                cache_status: proto::CacheStatus::Miss as i32,
                took_ms: 5,
                generated_at: 0,
            }))
        }

        async fn batch_query(
            &self,
            _request: tonic::Request<proto::BatchQueryRequest>,
        ) -> Result<tonic::Response<proto::BatchQueryResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented("Not implemented"))
        }

        async fn federated_query(
            &self,
            _request: tonic::Request<proto::FederatedQueryRequest>,
        ) -> Result<tonic::Response<proto::FederatedQueryResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented("Not implemented"))
        }

        type QueryStreamStream = std::pin::Pin<
            Box<
                dyn tokio_stream::Stream<Item = Result<proto::QueryResponse, tonic::Status>> + Send,
            >,
        >;

        async fn query_stream(
            &self,
            _request: tonic::Request<proto::QueryRequest>,
        ) -> Result<tonic::Response<Self::QueryStreamStream>, tonic::Status> {
            Err(tonic::Status::unimplemented("Not implemented"))
        }
    }

    #[tonic::async_trait]
    impl AdminService for MockAdmin {
        async fn reload(
            &self,
            _: tonic::Request<proto::ReloadRequest>,
        ) -> Result<tonic::Response<proto::ReloadResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented(""))
        }
        async fn pause(
            &self,
            _: tonic::Request<proto::PauseRequest>,
        ) -> Result<tonic::Response<proto::PauseResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented(""))
        }
        async fn resume(
            &self,
            _: tonic::Request<proto::ResumeRequest>,
        ) -> Result<tonic::Response<proto::ResumeResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented(""))
        }
        async fn cache_clear(
            &self,
            _: tonic::Request<proto::CacheClearRequest>,
        ) -> Result<tonic::Response<proto::CacheClearResponse>, tonic::Status> {
            Ok(tonic::Response::new(proto::CacheClearResponse {
                cleared_entries: 0,
                message: "Cache cleared".to_string(),
            }))
        }
        async fn cache_warmup(
            &self,
            _: tonic::Request<proto::CacheWarmupRequest>,
        ) -> Result<tonic::Response<proto::CacheWarmupResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented(""))
        }
        async fn cache_evict(
            &self,
            _: tonic::Request<proto::CacheEvictRequest>,
        ) -> Result<tonic::Response<proto::CacheEvictResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented(""))
        }
        async fn cache_integrity(
            &self,
            _: tonic::Request<proto::CacheIntegrityRequest>,
        ) -> Result<tonic::Response<proto::CacheIntegrityResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented(""))
        }
        async fn metrics_reset(
            &self,
            _: tonic::Request<proto::MetricsResetRequest>,
        ) -> Result<tonic::Response<proto::MetricsResetResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented(""))
        }
        async fn list_agents(
            &self,
            _: tonic::Request<proto::ListAgentsRequest>,
        ) -> Result<tonic::Response<proto::ListAgentsResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented(""))
        }
        async fn create_agent(
            &self,
            _: tonic::Request<proto::CreateAgentRequest>,
        ) -> Result<tonic::Response<proto::CreateAgentResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented(""))
        }
        async fn delete_agent(
            &self,
            _: tonic::Request<proto::DeleteAgentRequest>,
        ) -> Result<tonic::Response<proto::DeleteAgentResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented(""))
        }
        async fn rotate_key(
            &self,
            _: tonic::Request<proto::RotateKeyRequest>,
        ) -> Result<tonic::Response<proto::RotateKeyResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented(""))
        }
    }

    #[tonic::async_trait]
    impl ObservabilityService for MockObs {
        async fn get_stats(
            &self,
            _: tonic::Request<proto::GetStatsRequest>,
        ) -> Result<tonic::Response<proto::StatsResponse>, tonic::Status> {
            Ok(tonic::Response::new(proto::StatsResponse {
                uptime_secs: 42,
                cache_entries: 100,
                total_hits: 80,
                total_misses: 20,
                hit_rate: 0.8,
                upstream_requests: 20,
                upstream_failures: 0,
                upstream_error_rate: 0.0,
                degradation_level: "full".to_string(),
                paused: false,
            }))
        }
        async fn get_query_stats(
            &self,
            _: tonic::Request<proto::GetQueryStatsRequest>,
        ) -> Result<tonic::Response<proto::QueryStatsResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented(""))
        }
        async fn get_audit(
            &self,
            _: tonic::Request<proto::GetAuditRequest>,
        ) -> Result<tonic::Response<proto::AuditResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented(""))
        }
        async fn get_circuit_status(
            &self,
            _: tonic::Request<proto::GetCircuitStatusRequest>,
        ) -> Result<tonic::Response<proto::CircuitStatusResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented(""))
        }
        async fn get_queue_stats(
            &self,
            _: tonic::Request<proto::GetQueueStatsRequest>,
        ) -> Result<tonic::Response<proto::QueueStatsResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented(""))
        }
        async fn get_clients(
            &self,
            _: tonic::Request<proto::GetClientsRequest>,
        ) -> Result<tonic::Response<proto::ClientsResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented(""))
        }
        async fn get_pool_status(
            &self,
            _: tonic::Request<proto::GetPoolStatusRequest>,
        ) -> Result<tonic::Response<proto::PoolStatusResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented(""))
        }
        async fn get_cache_upstreams(
            &self,
            _: tonic::Request<proto::GetCacheUpstreamsRequest>,
        ) -> Result<tonic::Response<proto::CacheUpstreamsResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented(""))
        }
        type GetCacheDistillStream = std::pin::Pin<
            Box<dyn tokio_stream::Stream<Item = Result<proto::DistillEntry, tonic::Status>> + Send>,
        >;
        async fn get_cache_distill(
            &self,
            _: tonic::Request<proto::DistillRequest>,
        ) -> Result<tonic::Response<Self::GetCacheDistillStream>, tonic::Status> {
            let stream = tokio_stream::iter(std::iter::empty::<
                Result<proto::DistillEntry, tonic::Status>,
            >());
            Ok(tonic::Response::new(Box::pin(stream)))
        }
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{}", addr);

    let handle = tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        tonic::transport::Server::builder()
            .add_service(SearchServiceServer::new(MockSearch))
            .add_service(AdminServiceServer::new(MockAdmin))
            .add_service(ObservabilityServiceServer::new(MockObs))
            .serve_with_incoming(incoming)
            .await
            .ok();
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (handle, url)
}

/// Start a mock HTTP server for health/ready endpoints.
async fn start_mock_http_server() -> (tokio::task::JoinHandle<()>, String) {
    use axum::routing::get;

    let app = axum::Router::new()
        .route("/health", get(|| async { axum::http::StatusCode::OK }))
        .route("/ready", get(|| async { axum::http::StatusCode::OK }));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{}", addr);

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    (handle, url)
}

#[tokio::test]
async fn test_query_success() {
    let (_handle, grpc_url) = start_mock_grpc_server().await;
    let client = ProxyClient::connect(&grpc_url).unwrap();
    let request = crate::proxy::types::QueryRequest {
        query: "test".to_string(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };
    let response = client.query(&request).await.unwrap();
    assert_eq!(response.results.len(), 1);
    assert_eq!(response.results[0].id, "doc-1");
}

#[tokio::test]
async fn test_search_success() {
    let (_handle, grpc_url) = start_mock_grpc_server().await;
    let client = ProxyClient::connect(&grpc_url).unwrap();
    let response = client.search("test", 5).await.unwrap();
    assert_eq!(response.results.len(), 1);
}

#[tokio::test]
async fn test_health_success() {
    let (_handle, http_url) = start_mock_http_server().await;
    let config = ClientConfig::new("http://127.0.0.1:19999") // gRPC won't be used
        .with_http_url(&http_url);
    let client = ProxyClient::new(config).unwrap();
    let healthy = client.health().await.unwrap();
    assert!(healthy);
}

#[tokio::test]
async fn test_ready_success() {
    let (_handle, http_url) = start_mock_http_server().await;
    let config = ClientConfig::new("http://127.0.0.1:19999").with_http_url(&http_url);
    let client = ProxyClient::new(config).unwrap();
    let ready = client.ready().await.unwrap();
    assert!(ready);
}

#[tokio::test]
async fn test_stats_success() {
    let (_handle, grpc_url) = start_mock_grpc_server().await;
    let client = ProxyClient::connect(&grpc_url).unwrap();
    let stats = client.stats().await.unwrap();
    assert_eq!(stats["uptime_secs"], 42);
    assert_eq!(stats["cache_entries"], 100);
}

#[tokio::test]
async fn test_clear_cache_success() {
    let (_handle, grpc_url) = start_mock_grpc_server().await;
    let client = ProxyClient::connect(&grpc_url).unwrap();
    let result = client.clear_cache().await;
    assert!(result.is_ok());
}
