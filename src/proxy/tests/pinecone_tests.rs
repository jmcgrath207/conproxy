#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;
use crate::proxy::types::QueryRequest;
use std::time::Duration;

#[test]
fn test_pinecone_config_default() {
    let config = PineconeConfig::default();
    assert_eq!(config.base_url, "http://localhost:8080");
    assert!(config.include_metadata);
}

#[test]
fn test_pinecone_adapter_creation() {
    let adapter =
        PineconeAdapter::simple("http://localhost:8080", Duration::from_secs(30)).unwrap();
    assert_eq!(adapter.identifier(), "http://localhost:8080");
    assert_eq!(adapter.timeout(), Duration::from_secs(30));
    assert_eq!(adapter.query_mode(), QueryMode::VectorOnly);
    assert_eq!(adapter.metadata().adapter_type, "pinecone");
}

#[tokio::test]
async fn test_pinecone_query_text_unsupported() {
    let adapter = PineconeAdapter::simple("http://localhost:8080", Duration::from_secs(5)).unwrap();
    let req = QueryRequest {
        query: "hello".into(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };
    let err = adapter.query(&req).await.unwrap_err();
    assert!(matches!(err, UpstreamError::UnsupportedQueryType(_)));
}

#[tokio::test]
async fn test_pinecone_dim_mismatch_fail_fast() {
    let adapter = PineconeAdapter::new(PineconeConfig {
        base_url: "http://localhost:8080".into(),
        timeout: Duration::from_secs(5),
        dimensions: Some(8),
        ..Default::default()
    })
    .unwrap();
    let req = QueryRequest {
        query: String::new(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };
    let err = adapter
        .query_vector(&req, &[0.1, 0.2, 0.3])
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("dimension mismatch"),
        "expected dim mismatch, got {msg}"
    );
}

#[tokio::test]
async fn test_pinecone_query_vector_mock() {
    use axum::routing::post;
    use axum::{Json, Router};
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    async fn handle_query(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
        assert!(body.get("vector").is_some());
        Json(serde_json::json!({
            "matches": [
                {
                    "id": "doc-1",
                    "score": 0.92,
                    "metadata": { "content": "pinecone hit", "tag": "a" }
                }
            ]
        }))
    }

    let app = Router::new().route("/query", post(handle_query));
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let base = format!("http://{addr}");
    let adapter = PineconeAdapter::new(PineconeConfig {
        base_url: base,
        timeout: Duration::from_secs(5),
        dimensions: Some(4),
        api_key: Some("test-key".into()),
        ..Default::default()
    })
    .unwrap();

    let req = QueryRequest {
        query: String::new(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };
    let resp = adapter
        .query_vector(&req, &[0.1, 0.2, 0.3, 0.4])
        .await
        .expect("query_vector");
    assert_eq!(resp.results.len(), 1);
    assert_eq!(resp.results[0].id, "doc-1");
    assert!((resp.results[0].score - 0.92).abs() < 0.001);
    assert_eq!(resp.results[0].content, "pinecone hit");
}
