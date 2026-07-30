//! Integration tests for Pinecone adapter against an in-process mock.
//!
//! No secrets required. Live Pinecone stays `#[ignore]`.

#![cfg(feature = "integration-tests")]

use axum::routing::post;
use axum::{Json, Router};
use conproxy::proxy::pinecone::{PineconeAdapter, PineconeConfig};
use conproxy::proxy::types::QueryRequest;
use conproxy::proxy::upstream::UpstreamAdapter;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpListener;

async fn spawn_pinecone_mock() -> String {
    async fn query(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
        assert!(body.get("vector").is_some());
        let top_k = body.get("topK").and_then(|v| v.as_u64()).unwrap_or(10);
        assert!(top_k >= 1);
        Json(serde_json::json!({
            "matches": [
                {
                    "id": "pc-1",
                    "score": 0.88,
                    "metadata": { "content": "mock pinecone doc", "src": "test" }
                },
                {
                    "id": "pc-2",
                    "score": 0.41,
                    "metadata": { "content": "other" }
                }
            ]
        }))
    }

    async fn describe() -> Json<serde_json::Value> {
        Json(serde_json::json!({
            "namespaces": { "": { "vectorCount": 2 } },
            "dimension": 4,
            "indexFullness": 0.0,
            "totalVectorCount": 2
        }))
    }

    let app = Router::new()
        .route("/query", post(query))
        .route("/describe_index_stats", post(describe));
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn pinecone_mock_query_and_health() {
    let base = spawn_pinecone_mock().await;
    let adapter = PineconeAdapter::new(PineconeConfig {
        base_url: base,
        api_key: Some("test-key".into()),
        timeout: Duration::from_secs(5),
        dimensions: Some(4),
        ..Default::default()
    })
    .expect("adapter");

    assert!(adapter.health_check().await.expect("health"));

    let req = QueryRequest {
        query: String::new(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };
    let resp = adapter
        .query_vector(&req, &[0.25, 0.5, 0.75, 1.0])
        .await
        .expect("query_vector");
    assert_eq!(resp.results.len(), 2);
    assert_eq!(resp.results[0].id, "pc-1");
    assert!((resp.results[0].score - 0.88).abs() < 0.001);
    assert_eq!(resp.results[0].content, "mock pinecone doc");
}

#[tokio::test]
async fn pinecone_mock_dim_mismatch() {
    let base = spawn_pinecone_mock().await;
    let adapter = PineconeAdapter::new(PineconeConfig {
        base_url: base,
        timeout: Duration::from_secs(5),
        dimensions: Some(4),
        ..Default::default()
    })
    .expect("adapter");
    let req = QueryRequest {
        query: String::new(),
        top_k: Some(1),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };
    let err = adapter
        .query_vector(&req, &[1.0, 2.0])
        .await
        .expect_err("dim");
    assert!(err.to_string().contains("dimension mismatch"));
}

#[tokio::test]
#[ignore = "live Pinecone: set PINECONE_API_KEY + PINECONE_HOST"]
async fn pinecone_live_query() {
    let key = std::env::var("PINECONE_API_KEY").expect("PINECONE_API_KEY");
    let host = std::env::var("PINECONE_HOST").expect("PINECONE_HOST");
    let adapter = PineconeAdapter::new(PineconeConfig {
        base_url: host,
        api_key: Some(key),
        timeout: Duration::from_secs(30),
        dimensions: None,
        ..Default::default()
    })
    .expect("adapter");
    let req = QueryRequest {
        query: String::new(),
        top_k: Some(3),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };
    // 4-dim probe — may fail if index dims differ; live test is manual.
    let _ = adapter.query_vector(&req, &[0.1, 0.2, 0.3, 0.4]).await;
}
