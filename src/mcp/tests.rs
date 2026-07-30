use super::*;
use crate::config::ConfigFile;

fn empty_config() -> Config {
    Config {
        config: ConfigFile::default(),
        local_root: None,
    }
}

#[test]
fn test_server_info() {
    let server = ConproxyServer::new(empty_config());
    let info = server.get_info();

    assert_eq!(info.server_info.name, env!("CARGO_PKG_NAME"));
    assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
    assert!(info.instructions.is_some());
}

#[test]
fn test_search_tool_connection_error() {
    // With no proxy running, search should fail
    let server = ConproxyServer::new(empty_config());
    let params = ProxySearchParams {
        query: "test".to_string(),
        limit: 10,
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = rt.block_on(server.search(Parameters(params)));

    // Will try to connect to proxy (may fail without running proxy)
    let _ = result;
}

#[test]
fn test_proxy_search_params_defaults() {
    let json = r#"{"query": "hello"}"#;
    let params: ProxySearchParams = serde_json::from_str(json).unwrap();
    assert_eq!(params.query, "hello");
    assert_eq!(params.limit, 10);
}

#[test]
fn test_proxy_search_params_custom() {
    let json = r#"{"query": "rust async", "limit": 20}"#;
    let params: ProxySearchParams = serde_json::from_str(json).unwrap();
    assert_eq!(params.query, "rust async");
    assert_eq!(params.limit, 20);
}

#[test]
fn test_proxy_search_result_item_serialization() {
    let item = ProxySearchResultItem {
        id: "doc-1".to_string(),
        content: "Hello world".to_string(),
        score: 0.95,
        cache_status: "Hit".to_string(),
    };
    let json = serde_json::to_string(&item).unwrap();
    assert!(json.contains("doc-1"));
    assert!(json.contains("0.95"));
    assert!(json.contains("Hit"));
}

#[test]
fn test_server_info_fields() {
    let server = ConproxyServer::new(empty_config());
    let info = server.get_info();

    assert_eq!(info.protocol_version, ProtocolVersion::V_2024_11_05);
    let instructions = info.instructions.unwrap();
    assert!(instructions.contains("search"));
    assert!(instructions.contains("tune"));
}

// === Proxy search tests with mock gRPC ===

mod mock_grpc {
    use crate::proxy::grpc::proto;
    use crate::proxy::grpc::proto::search_service_server::SearchService;
    use tonic::{Request, Response, Status};

    pub struct MockSearchService {
        pub fail: bool,
    }

    #[tonic::async_trait]
    impl SearchService for MockSearchService {
        async fn query(
            &self,
            _request: Request<proto::QueryRequest>,
        ) -> Result<Response<proto::QueryResponse>, Status> {
            if self.fail {
                return Err(Status::internal("mock error"));
            }
            Ok(Response::new(proto::QueryResponse {
                results: vec![proto::SearchResult {
                    id: "doc-1".to_string(),
                    score: 0.95,
                    content: "Test result".to_string(),
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
            _request: Request<proto::BatchQueryRequest>,
        ) -> Result<Response<proto::BatchQueryResponse>, Status> {
            Err(Status::unimplemented("not needed for test"))
        }

        async fn federated_query(
            &self,
            _request: Request<proto::FederatedQueryRequest>,
        ) -> Result<Response<proto::FederatedQueryResponse>, Status> {
            Err(Status::unimplemented("not needed for test"))
        }

        type QueryStreamStream =
            tokio_stream::wrappers::ReceiverStream<Result<proto::QueryResponse, Status>>;

        async fn query_stream(
            &self,
            _request: Request<proto::QueryRequest>,
        ) -> Result<Response<Self::QueryStreamStream>, Status> {
            Err(Status::unimplemented("not needed for test"))
        }
    }
}

#[tokio::test]
async fn test_conproxy_search_with_mock_proxy() {
    use crate::proxy::grpc::proto::search_service_server::SearchServiceServer;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let svc = mock_grpc::MockSearchService { fail: false };
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(SearchServiceServer::new(svc))
            .serve_with_incoming(incoming)
            .await
            .ok();
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut config = empty_config();
    config.config.proxy.listen = Some(addr.to_string());

    let server = ConproxyServer::new(config);
    let params = ProxySearchParams {
        query: "test query".to_string(),
        limit: 10,
    };

    let result = server.search(Parameters(params)).await;
    assert!(result.is_ok());
    let call_result = result.unwrap();
    let text = &call_result.content[0];
    let t = text.as_text().expect("Expected text content");
    assert!(t.text.contains("doc-1"));
    assert!(t.text.contains("Test result"));
}

#[tokio::test]
async fn test_tune_session_scope_export_cookbook() {
    let server = ConproxyServer::new(empty_config());

    let open = server
        .tune_session_open(Parameters(TuneSessionOpenParams {
            agent_id: "agent-a".into(),
            context_id: "docs".into(),
            session_id: None,
        }))
        .await
        .expect("open");
    let open_text = open.content[0]
        .as_text()
        .expect("Expected text content")
        .text
        .clone();
    let open_json: serde_json::Value = serde_json::from_str(&open_text).unwrap();
    let session_id = open_json["session_id"].as_str().unwrap().to_string();

    let tune = server
        .scope_tune(Parameters(ScopeTuneToolParams {
            session_id: session_id.clone(),
            agent_id: Some("agent-a".into()),
            context_id: Some("docs".into()),
            hits: vec![
                McpHit {
                    id: "1".into(),
                    content: "Rust async Tokio".into(),
                    score: 0.9,
                },
                McpHit {
                    id: "2".into(),
                    content: "Python Django".into(),
                    score: 0.8,
                },
            ],
            weighted_phrases: vec![McpWeightedPhrase {
                text: "rust".into(),
                weight: 1.0,
                min_similarity: None,
            }],
            mode: Some("filter".into()),
            min_similarity: None,
            min_similarity_sweep: Some(vec![0.1, 0.5]),
            scope_weight: None,
            lexical_weight: None,
        }))
        .await
        .expect("scope_tune");
    let tune_text = tune.content[0]
        .as_text()
        .expect("Expected text content")
        .text
        .clone();
    assert!(tune_text.contains("scope_tune"));
    assert!(tune_text.contains("sweep"));

    let suggest = server
        .scope_suggest(Parameters(ScopeSuggestToolParams {
            session_id: session_id.clone(),
            agent_id: Some("agent-a".into()),
            context_id: Some("docs".into()),
            texts: vec!["Rust Tokio async".into(), "Tokio spawn".into()],
            max_phrases: 4,
        }))
        .await
        .expect("suggest");
    let suggest_text = suggest.content[0]
        .as_text()
        .expect("Expected text content")
        .text
        .clone();
    assert!(suggest_text.contains("phrases"));

    let export = server
        .tune_export(Parameters(TuneExportParams {
            session_id: session_id.clone(),
            agent_id: Some("agent-a".into()),
            context_id: Some("docs".into()),
        }))
        .await
        .expect("export");
    let export_text = export.content[0]
        .as_text()
        .expect("Expected text content")
        .text
        .clone();
    assert!(export_text.contains("contexts.docs.scope") || export_text.contains("toml"));

    // Isolation: other agent cannot export
    let denied = server
        .tune_export(Parameters(TuneExportParams {
            session_id: session_id.clone(),
            agent_id: Some("agent-b".into()),
            context_id: Some("docs".into()),
        }))
        .await;
    assert!(denied.is_err());

    let closed = server
        .tune_session_close(Parameters(TuneSessionCloseParams {
            session_id,
            agent_id: Some("agent-a".into()),
        }))
        .await
        .expect("close");
    let closed_text = closed.content[0]
        .as_text()
        .expect("Expected text content")
        .text
        .clone();
    assert!(closed_text.contains("\"closed\": true") || closed_text.contains("\"closed\":true"));
}

#[tokio::test]
async fn test_conproxy_search_proxy_error() {
    use crate::proxy::grpc::proto::search_service_server::SearchServiceServer;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let svc = mock_grpc::MockSearchService { fail: true };
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(SearchServiceServer::new(svc))
            .serve_with_incoming(incoming)
            .await
            .ok();
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut config = empty_config();
    config.config.proxy.listen = Some(addr.to_string());

    let server = ConproxyServer::new(config);
    let params = ProxySearchParams {
        query: "test".to_string(),
        limit: 10,
    };

    let result = server.search(Parameters(params)).await;
    assert!(result.is_err());
}

// === Composite workflow (open → search → scope_tune → apply/close) ===

#[tokio::test]
async fn test_tune_workflow_dry_run() {
    use crate::proxy::grpc::proto::search_service_server::SearchServiceServer;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let svc = mock_grpc::MockSearchService { fail: false };
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(SearchServiceServer::new(svc))
            .serve_with_incoming(incoming)
            .await
            .ok();
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut config = empty_config();
    config.config.proxy.listen = Some(addr.to_string());
    let server = ConproxyServer::new(config);

    let result = server
        .tune_workflow(Parameters(TuneWorkflowParams {
            agent_id: "agent-a".into(),
            context_id: "docs".into(),
            query: "rust async".into(),
            top_k: 10,
            weighted_phrases: vec![McpWeightedPhrase {
                text: "rust".into(),
                weight: 1.0,
                min_similarity: None,
            }],
            mode: Some("filter".into()),
            min_similarity: None,
            min_similarity_sweep: Some(vec![0.1, 0.5]),
            scope_weight: None,
            lexical_weight: None,
            apply: false,
            reload: true,
            config_path: None,
            close_session: true,
            session_id: None,
        }))
        .await
        .expect("workflow");
    let text = result.content[0].as_text().expect("text").text.clone();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(v["session_id"].as_str().is_some());
    assert_eq!(v["search"]["hit_count"], 1);
    assert!(v["tune"]["sweep"].as_array().unwrap().len() == 2);
    // dry-run: apply payload absent
    assert!(v["apply"].is_null());
    // close_session=true → ok
    assert_eq!(v["close"]["closed"], true);
    assert_eq!(v["close"]["reason"], "ok");
}

#[tokio::test]
async fn test_tune_workflow_empty_search_returns_clear_error() {
    // The mock returns one hit; build a custom mock that returns 0 hits.
    use crate::proxy::grpc::proto::search_service_server::SearchServiceServer;
    use tonic::{Request, Response, Status};

    struct EmptySearchService;
    #[tonic::async_trait]
    impl crate::proxy::grpc::proto::search_service_server::SearchService for EmptySearchService {
        async fn query(
            &self,
            _r: Request<crate::proxy::grpc::proto::QueryRequest>,
        ) -> Result<Response<crate::proxy::grpc::proto::QueryResponse>, Status> {
            Ok(Response::new(crate::proxy::grpc::proto::QueryResponse {
                results: vec![],
                cache_status: crate::proxy::grpc::proto::CacheStatus::Miss as i32,
                took_ms: 1,
                generated_at: 0,
            }))
        }
        async fn batch_query(
            &self,
            _r: Request<crate::proxy::grpc::proto::BatchQueryRequest>,
        ) -> Result<Response<crate::proxy::grpc::proto::BatchQueryResponse>, Status> {
            Err(Status::unimplemented("n/a"))
        }
        async fn federated_query(
            &self,
            _r: Request<crate::proxy::grpc::proto::FederatedQueryRequest>,
        ) -> Result<Response<crate::proxy::grpc::proto::FederatedQueryResponse>, Status> {
            Err(Status::unimplemented("n/a"))
        }
        type QueryStreamStream = tokio_stream::wrappers::ReceiverStream<
            Result<crate::proxy::grpc::proto::QueryResponse, Status>,
        >;
        async fn query_stream(
            &self,
            _r: Request<crate::proxy::grpc::proto::QueryRequest>,
        ) -> Result<Response<Self::QueryStreamStream>, Status> {
            Err(Status::unimplemented("n/a"))
        }
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(SearchServiceServer::new(EmptySearchService))
            .serve_with_incoming(incoming)
            .await
            .ok();
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut config = empty_config();
    config.config.proxy.listen = Some(addr.to_string());
    let server = ConproxyServer::new(config);

    let err = server
        .tune_workflow(Parameters(TuneWorkflowParams {
            agent_id: "a".into(),
            context_id: "ctx".into(),
            query: "no match query".into(),
            top_k: 5,
            weighted_phrases: vec![],
            mode: None,
            min_similarity: None,
            min_similarity_sweep: None,
            scope_weight: None,
            lexical_weight: None,
            apply: false,
            reload: false,
            config_path: None,
            close_session: false,
            session_id: None,
        }))
        .await
        .expect_err("should fail on empty hits");
    let msg = err.to_string();
    assert!(msg.contains("0 hits"), "msg was: {msg}");
    assert!(msg.contains("corpus"), "msg was: {msg}");
}

#[tokio::test]
async fn test_tune_session_close_reports_unknown_vs_mismatch() {
    let server = ConproxyServer::new(empty_config());

    // Unknown
    let r = server
        .tune_session_close(Parameters(TuneSessionCloseParams {
            session_id: "nope".into(),
            agent_id: None,
        }))
        .await
        .expect("close-ok-struct");
    let t = r.content[0].as_text().unwrap().text.clone();
    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
    assert_eq!(v["closed"], false);
    assert_eq!(v["reason"], "unknown_session");

    // Open + close wrong agent
    let open = server
        .tune_session_open(Parameters(TuneSessionOpenParams {
            agent_id: "alice".into(),
            context_id: "ctx".into(),
            session_id: None,
        }))
        .await
        .unwrap();
    let sid =
        serde_json::from_str::<serde_json::Value>(open.content[0].as_text().unwrap().text.as_str())
            .unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_string();

    let r = server
        .tune_session_close(Parameters(TuneSessionCloseParams {
            session_id: sid.clone(),
            agent_id: Some("bob".into()),
        }))
        .await
        .expect("close-ok-struct");
    let t = r.content[0].as_text().unwrap().text.clone();
    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
    assert_eq!(v["closed"], false);
    assert_eq!(v["reason"], "agent_mismatch");
    assert_eq!(v["expected_agent_id"], "alice");
    assert_eq!(v["got_agent_id"], "bob");

    // Owner closes
    let r = server
        .tune_session_close(Parameters(TuneSessionCloseParams {
            session_id: sid,
            agent_id: Some("alice".into()),
        }))
        .await
        .expect("close-ok");
    let t = r.content[0].as_text().unwrap().text.clone();
    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
    assert_eq!(v["closed"], true);
    assert_eq!(v["reason"], "ok");
}

// === MCP search sends proxy.api_key when configured ====================

mod auth_capture {
    use crate::proxy::grpc::proto;
    use crate::proxy::grpc::proto::search_service_server::SearchService;
    use std::sync::Arc;
    use std::sync::Mutex;
    use tonic::{Request, Response, Status};

    /// Captures the `x-api-key` (or absence) from each request so tests can
    /// assert that MCP wires the configured `proxy.api_key` into the gRPC
    /// search client.
    pub struct CapturingSearchService {
        pub last_key: Arc<Mutex<Option<Option<String>>>>,
    }

    #[tonic::async_trait]
    impl SearchService for CapturingSearchService {
        async fn query(
            &self,
            request: Request<proto::QueryRequest>,
        ) -> Result<Response<proto::QueryResponse>, Status> {
            let key = request
                .metadata()
                .get("x-api-key")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            *self.last_key.lock().unwrap() = Some(key);
            Ok(Response::new(proto::QueryResponse {
                results: vec![proto::SearchResult {
                    id: "doc-1".to_string(),
                    score: 0.95,
                    content: "Test result".to_string(),
                    metadata_json: vec![],
                    upstream_id: String::new(),
                }],
                cache_status: proto::CacheStatus::Miss as i32,
                took_ms: 1,
                generated_at: 0,
            }))
        }
        async fn batch_query(
            &self,
            _r: Request<proto::BatchQueryRequest>,
        ) -> Result<Response<proto::BatchQueryResponse>, Status> {
            Err(Status::unimplemented("n/a"))
        }
        async fn federated_query(
            &self,
            _r: Request<proto::FederatedQueryRequest>,
        ) -> Result<Response<proto::FederatedQueryResponse>, Status> {
            Err(Status::unimplemented("n/a"))
        }
        type QueryStreamStream =
            tokio_stream::wrappers::ReceiverStream<Result<proto::QueryResponse, Status>>;
        async fn query_stream(
            &self,
            _r: Request<proto::QueryRequest>,
        ) -> Result<Response<Self::QueryStreamStream>, Status> {
            Err(Status::unimplemented("n/a"))
        }
    }
}

#[tokio::test]
async fn test_conproxy_search_sends_proxy_api_key() {
    use crate::proxy::grpc::proto::search_service_server::SearchServiceServer;
    use std::sync::{Arc, Mutex};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let last_key = Arc::new(Mutex::new(None));
    let svc = auth_capture::CapturingSearchService {
        last_key: Arc::clone(&last_key),
    };
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(SearchServiceServer::new(svc))
            .serve_with_incoming(incoming)
            .await
            .ok();
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Config has api_key set → MCP should send x-api-key: sk-local
    let mut config = empty_config();
    config.config.proxy.listen = Some(addr.to_string());
    config.config.proxy.api_key = Some("sk-local".to_string());

    let server = ConproxyServer::new(config);
    let params = ProxySearchParams {
        query: "test".to_string(),
        limit: 5,
    };
    server.search(Parameters(params)).await.expect("search");

    let captured = last_key.lock().unwrap().clone();
    assert_eq!(
        captured,
        Some(Some("sk-local".to_string())),
        "expected x-api-key=sk-local, got {captured:?}"
    );
}

#[tokio::test]
async fn test_conproxy_search_omits_api_key_when_unset() {
    use crate::proxy::grpc::proto::search_service_server::SearchServiceServer;
    use std::sync::{Arc, Mutex};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let last_key = Arc::new(Mutex::new(None));
    let svc = auth_capture::CapturingSearchService {
        last_key: Arc::clone(&last_key),
    };
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(SearchServiceServer::new(svc))
            .serve_with_incoming(incoming)
            .await
            .ok();
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Config has no api_key → MCP should NOT send x-api-key
    let mut config = empty_config();
    config.config.proxy.listen = Some(addr.to_string());

    let server = ConproxyServer::new(config);
    let params = ProxySearchParams {
        query: "test".to_string(),
        limit: 5,
    };
    server.search(Parameters(params)).await.expect("search");

    let captured = last_key.lock().unwrap().clone();
    assert_eq!(
        captured,
        Some(None),
        "expected no x-api-key header, got {captured:?}"
    );
}
