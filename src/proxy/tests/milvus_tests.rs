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
fn test_milvus_config_default() {
    let config = MilvusConfig::default();
    assert_eq!(config.base_url, "http://localhost:9091");
    assert_eq!(config.collection_name, "default");
    assert_eq!(config.vector_field, "vector");
}

#[test]
fn test_milvus_adapter_creation() {
    let adapter =
        MilvusAdapter::simple("http://localhost:9091", "demo", Duration::from_secs(30)).unwrap();
    assert_eq!(adapter.identifier(), "http://localhost:9091");
    assert_eq!(adapter.timeout(), Duration::from_secs(30));
    assert_eq!(adapter.query_mode(), QueryMode::VectorOnly);
    assert_eq!(adapter.metadata().adapter_type, "milvus");
}

#[test]
fn test_normalize_milvus_score() {
    assert!((normalize_milvus_score(Some(0.0)) - 1.0).abs() < f32::EPSILON);
    assert!((normalize_milvus_score(Some(0.25)) - 0.75).abs() < f32::EPSILON);
    assert!((normalize_milvus_score(Some(2.0)) - 0.0).abs() < f32::EPSILON);
}

#[tokio::test]
async fn test_milvus_query_text_unsupported() {
    let adapter =
        MilvusAdapter::simple("http://localhost:9091", "demo", Duration::from_secs(5)).unwrap();
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
async fn test_milvus_dim_mismatch_fail_fast() {
    let adapter = MilvusAdapter::new(MilvusConfig {
        base_url: "http://localhost:9091".into(),
        collection_name: "demo".into(),
        timeout: Duration::from_secs(5),
        dimensions: Some(4),
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
    let err = adapter.query_vector(&req, &[0.1, 0.2]).await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("dimension mismatch"),
        "expected dim mismatch, got {msg}"
    );
}
