#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

#[test]
fn test_qdrant_config_default() {
    let config = QdrantConfig::default();
    assert_eq!(config.base_url, "http://localhost:6333");
    assert_eq!(config.collection_name, "default");
    assert!(config.with_payload);
    assert!(!config.with_vectors);
}

#[test]
fn test_qdrant_adapter_creation() {
    let adapter = QdrantAdapter::simple(
        "http://localhost:6333",
        "test_collection",
        Duration::from_secs(30),
    )
    .unwrap();

    assert_eq!(adapter.identifier(), "http://localhost:6333");
    assert_eq!(adapter.timeout(), Duration::from_secs(30));
}

#[test]
fn test_qdrant_adapter_urls() {
    let adapter = QdrantAdapter::simple(
        "http://localhost:6333",
        "my_collection",
        Duration::from_secs(30),
    )
    .unwrap();

    assert_eq!(
        adapter.search_url(),
        "http://localhost:6333/collections/my_collection/points/search"
    );
    assert_eq!(adapter.health_url(), "http://localhost:6333/");
    assert_eq!(
        adapter.collection_url(),
        "http://localhost:6333/collections/my_collection"
    );
}

#[test]
fn test_qdrant_adapter_metadata() {
    let adapter = QdrantAdapter::simple(
        "http://localhost:6333",
        "test_collection",
        Duration::from_secs(30),
    )
    .unwrap();

    let metadata = adapter.metadata();
    assert_eq!(metadata.adapter_type, "qdrant");
    assert_eq!(
        metadata.properties.get("collection"),
        Some(&"test_collection".to_string())
    );
}

#[test]
fn test_qdrant_point_id_to_string() {
    let uuid_id = QdrantPointId::Uuid("abc-123".to_string());
    assert_eq!(uuid_id.to_string(), "abc-123");

    let int_id = QdrantPointId::Integer(42);
    assert_eq!(int_id.to_string(), "42");
}

#[test]
fn test_qdrant_config_with_api_key() {
    let config = QdrantConfig {
        api_key: Some("test-key".to_string()),
        ..Default::default()
    };

    let adapter = QdrantAdapter::new(config).unwrap();
    assert!(adapter.config.api_key.is_some());
}

#[test]
fn test_qdrant_query_url() {
    let adapter = QdrantAdapter::simple(
        "http://localhost:6333",
        "my_collection",
        Duration::from_secs(30),
    )
    .unwrap();

    assert_eq!(
        adapter.query_url(),
        "http://localhost:6333/collections/my_collection/points/query"
    );
}

#[test]
fn test_qdrant_query_mode_default() {
    let adapter = QdrantAdapter::simple(
        "http://localhost:6333",
        "test_collection",
        Duration::from_secs(30),
    )
    .unwrap();

    // Default should be Unknown
    assert_eq!(adapter.query_mode(), QueryMode::Unknown);
}

#[test]
fn test_qdrant_query_mode_set_and_get() {
    let adapter = QdrantAdapter::simple(
        "http://localhost:6333",
        "test_collection",
        Duration::from_secs(30),
    )
    .unwrap();

    // Set to TextNative
    adapter.set_query_mode(QueryMode::TextNative);
    assert_eq!(adapter.query_mode(), QueryMode::TextNative);

    // Set to VectorOnly
    adapter.set_query_mode(QueryMode::VectorOnly);
    assert_eq!(adapter.query_mode(), QueryMode::VectorOnly);

    // Set back to Unknown
    adapter.set_query_mode(QueryMode::Unknown);
    assert_eq!(adapter.query_mode(), QueryMode::Unknown);
}

#[test]
fn test_qdrant_adapter_urls_trailing_slash() {
    let adapter = QdrantAdapter::simple(
        "http://localhost:6333/",
        "my_collection",
        Duration::from_secs(30),
    )
    .unwrap();

    // Trailing slash should be stripped
    assert_eq!(
        adapter.search_url(),
        "http://localhost:6333/collections/my_collection/points/search"
    );
    assert_eq!(adapter.health_url(), "http://localhost:6333/");
    assert_eq!(
        adapter.collection_url(),
        "http://localhost:6333/collections/my_collection"
    );
    assert_eq!(
        adapter.query_url(),
        "http://localhost:6333/collections/my_collection/points/query"
    );
}

#[test]
fn test_qdrant_config_with_score_threshold() {
    let config = QdrantConfig {
        score_threshold: Some(0.8),
        ..Default::default()
    };
    assert_eq!(config.score_threshold, Some(0.8));
}

#[test]
fn test_qdrant_config_with_vectors_enabled() {
    let config = QdrantConfig {
        with_vectors: true,
        with_payload: false,
        ..Default::default()
    };
    assert!(config.with_vectors);
    assert!(!config.with_payload);
}

#[test]
fn test_qdrant_config_timeout() {
    let config = QdrantConfig {
        timeout: Duration::from_secs(60),
        ..Default::default()
    };
    let adapter = QdrantAdapter::new(config).unwrap();
    assert_eq!(adapter.timeout(), Duration::from_secs(60));
}

#[test]
fn test_qdrant_config_custom_collection() {
    let config = QdrantConfig {
        collection_name: "embeddings-v2".to_string(),
        ..Default::default()
    };
    let adapter = QdrantAdapter::new(config).unwrap();
    let meta = adapter.metadata();
    assert_eq!(
        meta.properties.get("collection"),
        Some(&"embeddings-v2".to_string())
    );
}

#[test]
fn test_qdrant_metadata_no_version() {
    let adapter =
        QdrantAdapter::simple("http://localhost:6333", "test", Duration::from_secs(30)).unwrap();
    let meta = adapter.metadata();
    assert!(meta.version.is_none());
}

#[test]
fn test_qdrant_point_id_uuid_display() {
    let id = QdrantPointId::Uuid("550e8400-e29b-41d4-a716-446655440000".to_string());
    assert_eq!(id.to_string(), "550e8400-e29b-41d4-a716-446655440000");
}

#[test]
fn test_qdrant_point_id_integer_display() {
    let id = QdrantPointId::Integer(0);
    assert_eq!(id.to_string(), "0");

    let id = QdrantPointId::Integer(u64::MAX);
    assert_eq!(id.to_string(), format!("{}", u64::MAX));
}

#[test]
fn test_qdrant_point_id_deserialization_uuid() {
    let json = serde_json::json!("abc-123");
    let id: QdrantPointId = serde_json::from_value(json).unwrap();
    assert_eq!(id.to_string(), "abc-123");
}

#[test]
fn test_qdrant_point_id_deserialization_integer() {
    let json = serde_json::json!(42);
    let id: QdrantPointId = serde_json::from_value(json).unwrap();
    assert_eq!(id.to_string(), "42");
}

#[test]
fn test_qdrant_search_response_parsing() {
    let json = serde_json::json!({
        "result": [
            {
                "id": "abc-123",
                "score": 0.95,
                "payload": {
                    "content": "Hello world",
                    "title": "Test"
                }
            },
            {
                "id": 42,
                "score": 0.8,
                "payload": {
                    "text": "Fallback text field",
                    "category": "docs"
                }
            }
        ],
        "time": 0.005
    });

    let response: QdrantSearchResponse = serde_json::from_value(json).unwrap();
    assert_eq!(response.result.len(), 2);
    assert_eq!(response.result[0].id.to_string(), "abc-123");
    assert!((response.result[0].score - 0.95).abs() < 0.001);
    assert!(response.result[0].payload.is_some());
}

#[test]
fn test_qdrant_search_response_no_payload() {
    let json = serde_json::json!({
        "result": [
            {
                "id": "doc-1",
                "score": 0.7
            }
        ]
    });

    let response: QdrantSearchResponse = serde_json::from_value(json).unwrap();
    assert_eq!(response.result.len(), 1);
    assert!(response.result[0].payload.is_none());
    assert!(response.time.is_none());
}

#[test]
fn test_qdrant_collection_response_parsing() {
    let json = serde_json::json!({
        "result": {
            "status": "green",
            "points_count": 1000,
            "vectors_count": 1000
        }
    });

    let response: QdrantCollectionResponse = serde_json::from_value(json).unwrap();
    assert_eq!(response.result.status, "green");
    assert_eq!(response.result.points_count, Some(1000));
}

#[test]
fn test_qdrant_health_response_parsing() {
    let json = serde_json::json!({
        "title": "qdrant",
        "version": "1.7.0"
    });

    let response: QdrantHealthResponse = serde_json::from_value(json).unwrap();
    assert_eq!(response.title, Some("qdrant".to_string()));
    assert_eq!(response.version, Some("1.7.0".to_string()));
}

#[test]
fn test_qdrant_health_response_minimal() {
    let json = serde_json::json!({});
    let response: QdrantHealthResponse = serde_json::from_value(json).unwrap();
    assert!(response.title.is_none());
    assert!(response.version.is_none());
}

#[test]
fn test_qdrant_query_mode_thread_safe() {
    use std::sync::Arc;
    use std::thread;

    let adapter = Arc::new(
        QdrantAdapter::simple(
            "http://localhost:6333",
            "test_collection",
            Duration::from_secs(30),
        )
        .unwrap(),
    );

    let adapter_clone = Arc::clone(&adapter);

    // Set from another thread
    let handle = thread::spawn(move || {
        adapter_clone.set_query_mode(QueryMode::TextNative);
    });

    handle.join().unwrap();

    // Should be visible from this thread
    assert_eq!(adapter.query_mode(), QueryMode::TextNative);
}

// === HTTP-based tests for query, health_check, query_vector, discover_query_mode ===

use crate::proxy::UpstreamAdapter;

/// Start a mock Qdrant server.
async fn mock_qdrant_server(routes: axum::Router) -> (tokio::task::JoinHandle<()>, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{}", addr);

    let handle = tokio::spawn(async move {
        axum::serve(listener, routes).await.ok();
    });

    tokio::time::sleep(Duration::from_millis(20)).await;
    (handle, url)
}

#[tokio::test]
async fn test_qdrant_query_success() {
    let app = axum::Router::new().fallback(|| async {
        axum::Json(serde_json::json!({
            "result": [
                {
                    "id": "abc-123",
                    "score": 0.95,
                    "payload": {
                        "content": "Hello world",
                        "title": "Test"
                    }
                }
            ],
            "time": 0.005
        }))
    });

    let (_handle, url) = mock_qdrant_server(app).await;
    let adapter = QdrantAdapter::simple(&url, "test_collection", Duration::from_secs(5)).unwrap();

    let request = crate::proxy::QueryRequest {
        query: "test query".to_string(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };

    let response = adapter.query(&request).await.unwrap();
    assert_eq!(response.results.len(), 1);
    assert_eq!(response.results[0].id, "abc-123");
    assert!((response.results[0].score - 0.95).abs() < 0.001);
    assert_eq!(response.results[0].content, "Hello world");
}

#[tokio::test]
async fn test_qdrant_query_error_status() {
    let app = axum::Router::new().fallback(|| async { axum::http::StatusCode::BAD_REQUEST });

    let (_handle, url) = mock_qdrant_server(app).await;
    let adapter = QdrantAdapter::simple(&url, "test_collection", Duration::from_secs(5)).unwrap();

    let request = crate::proxy::QueryRequest {
        query: "test".to_string(),
        top_k: None,
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };

    let result = adapter.query(&request).await;
    assert!(result.is_err());
    match result.unwrap_err() {
        crate::proxy::UpstreamError::Status(code, _) => assert_eq!(code, 400),
        other => panic!("Expected Status error, got: {:?}", other),
    }
}

#[tokio::test]
async fn test_qdrant_query_parse_error() {
    let app = axum::Router::new().fallback(|| async { "not json" });

    let (_handle, url) = mock_qdrant_server(app).await;
    let adapter = QdrantAdapter::simple(&url, "test_collection", Duration::from_secs(5)).unwrap();

    let request = crate::proxy::QueryRequest {
        query: "test".to_string(),
        top_k: None,
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };

    let result = adapter.query(&request).await;
    assert!(result.is_err());
    match result.unwrap_err() {
        crate::proxy::UpstreamError::Parse(_) => {}
        other => panic!("Expected Parse error, got: {:?}", other),
    }
}

#[tokio::test]
async fn test_qdrant_health_check_green() {
    let app = axum::Router::new()
        .route(
            "/",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({"title": "qdrant", "version": "1.7.0"}))
            }),
        )
        .route(
            "/collections/test_collection",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({
                    "result": {
                        "status": "green",
                        "points_count": 1000
                    }
                }))
            }),
        );

    let (_handle, url) = mock_qdrant_server(app).await;
    let adapter = QdrantAdapter::simple(&url, "test_collection", Duration::from_secs(5)).unwrap();

    let healthy = adapter.health_check().await.unwrap();
    assert!(healthy);
}

#[tokio::test]
async fn test_qdrant_health_check_red() {
    let app = axum::Router::new()
        .route(
            "/",
            axum::routing::get(|| async { axum::Json(serde_json::json!({"title": "qdrant"})) }),
        )
        .route(
            "/collections/test_collection",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({
                    "result": {
                        "status": "red",
                        "points_count": 0
                    }
                }))
            }),
        );

    let (_handle, url) = mock_qdrant_server(app).await;
    let adapter = QdrantAdapter::simple(&url, "test_collection", Duration::from_secs(5)).unwrap();

    let healthy = adapter.health_check().await.unwrap();
    assert!(!healthy);
}

#[tokio::test]
async fn test_qdrant_health_check_error() {
    let app =
        axum::Router::new().fallback(|| async { axum::http::StatusCode::SERVICE_UNAVAILABLE });

    let (_handle, url) = mock_qdrant_server(app).await;
    let adapter = QdrantAdapter::simple(&url, "test_collection", Duration::from_secs(5)).unwrap();

    let healthy = adapter.health_check().await.unwrap();
    assert!(!healthy);
}

#[tokio::test]
async fn test_qdrant_query_vector_success() {
    let app = axum::Router::new().fallback(|| async {
        axum::Json(serde_json::json!({
            "result": [
                {
                    "id": "vec-1",
                    "score": 0.88,
                    "payload": {
                        "text": "Vector result",
                        "category": "docs"
                    }
                }
            ],
            "time": 0.003
        }))
    });

    let (_handle, url) = mock_qdrant_server(app).await;
    let adapter = QdrantAdapter::simple(&url, "test_collection", Duration::from_secs(5)).unwrap();

    let request = crate::proxy::QueryRequest {
        query: "test".to_string(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };
    let vector = vec![0.1, 0.2, 0.3, 0.4, 0.5];

    let response = adapter.query_vector(&request, &vector).await.unwrap();
    assert_eq!(response.results.len(), 1);
    assert_eq!(response.results[0].id, "vec-1");
    assert_eq!(response.results[0].content, "Vector result");
}

#[tokio::test]
async fn test_qdrant_discover_query_mode_text_native() {
    let app = axum::Router::new().fallback(|| async {
        axum::Json(serde_json::json!({
            "result": [{"id": "1", "score": 0.9}],
            "time": 0.001
        }))
    });

    let (_handle, url) = mock_qdrant_server(app).await;
    let adapter = QdrantAdapter::simple(&url, "test_collection", Duration::from_secs(5)).unwrap();

    let mode = adapter.discover_query_mode().await.unwrap();
    assert_eq!(mode, QueryMode::TextNative);
}

#[tokio::test]
async fn test_qdrant_discover_query_mode_vector_only() {
    let app = axum::Router::new().fallback(|| async {
        (
            axum::http::StatusCode::BAD_REQUEST,
            "vector field required".to_string(),
        )
    });

    let (_handle, url) = mock_qdrant_server(app).await;
    let adapter = QdrantAdapter::simple(&url, "test_collection", Duration::from_secs(5)).unwrap();

    let mode = adapter.discover_query_mode().await.unwrap();
    assert_eq!(mode, QueryMode::VectorOnly);
}
