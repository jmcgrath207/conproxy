//! Integration tests for pgvector adapter against a real pgvector container.
//!
//! Requires `--features integration-tests,pgvector` and a running Docker daemon.

#![cfg(feature = "integration-tests")]

mod test_infra;

use conproxy::proxy::pgvector::{DistanceMetric, PgvectorAdapter, PgvectorConfig};
use conproxy::proxy::types::QueryRequest;
use conproxy::proxy::upstream::UpstreamAdapter;

#[tokio::test]
async fn pgvector_vector_query_returns_results() {
    test_infra::containers::docker_check();
    let inst = test_infra::containers::pgvector_container().await;

    // Setup: CREATE EXTENSION + TABLE + INSERT DATA
    test_infra::containers::pgv_execute(&inst.base_url, "CREATE EXTENSION IF NOT EXISTS vector")
        .await;
    test_infra::containers::pgv_execute(
        &inst.base_url,
        "CREATE TABLE IF NOT EXISTS test_docs (
            id SERIAL PRIMARY KEY,
            content TEXT NOT NULL,
            embedding vector(4)
        )",
    )
    .await;

    // Insert test data
    test_infra::containers::pgv_insert(
        &inst.base_url,
        "test_docs",
        "alpha bravo charlie",
        &[0.1, 0.2, 0.3, 0.4],
    )
    .await;
    test_infra::containers::pgv_insert(
        &inst.base_url,
        "test_docs",
        "delta echo foxtrot",
        &[0.9, 0.8, 0.7, 0.6],
    )
    .await;

    // Construct adapter (connects and validates dims against existing data)
    let config = PgvectorConfig {
        url: format!("postgresql://postgres:test@{}/test", inst.base_url),
        table: "test_docs".into(),
        embedding_column: "embedding".into(),
        content_column: "content".into(),
        title_column: None,
        metadata_columns: vec![],
        distance_metric: DistanceMetric::Cosine,
        dimensions: Some(4),
        timeout_secs: 10,
    };
    let adapter = PgvectorAdapter::connect(config)
        .await
        .expect("PgvectorAdapter::connect");

    // query() (TextNative) should fail — pgvector is VectorOnly
    let req = QueryRequest {
        query: "alpha".into(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };
    let text_result = adapter.query(&req).await;
    assert!(
        text_result.is_err(),
        "query() should fail for VectorOnly adapter"
    );

    // query_vector() should succeed
    let vector = test_infra::containers::sample_vector(4); // [0.25, 0.5, 0.75, 1.0]
    let vec_result = adapter
        .query_vector(&req, &vector)
        .await
        .expect("query_vector should succeed");
    assert!(
        !vec_result.results.is_empty(),
        "query_vector should return results"
    );
    for r in &vec_result.results {
        assert!(
            (0.0..=1.1).contains(&r.score),
            "score {:.4} not in [0,1]",
            r.score
        );
    }
}

#[tokio::test]
async fn pgvector_health_check() {
    test_infra::containers::docker_check();
    let inst = test_infra::containers::pgvector_container().await;

    test_infra::containers::pgv_execute(&inst.base_url, "CREATE EXTENSION IF NOT EXISTS vector")
        .await;
    test_infra::containers::pgv_execute(
        &inst.base_url,
        "CREATE TABLE IF NOT EXISTS health_test (
            id SERIAL PRIMARY KEY,
            content TEXT NOT NULL,
            embedding vector(4)
        )",
    )
    .await;
    test_infra::containers::pgv_insert(
        &inst.base_url,
        "health_test",
        "test",
        &[0.1, 0.2, 0.3, 0.4],
    )
    .await;

    let config = PgvectorConfig {
        url: format!("postgresql://postgres:test@{}/test", inst.base_url),
        table: "health_test".into(),
        embedding_column: "embedding".into(),
        content_column: "content".into(),
        title_column: None,
        metadata_columns: vec![],
        distance_metric: DistanceMetric::Cosine,
        dimensions: Some(4),
        timeout_secs: 5,
    };
    let adapter = PgvectorAdapter::connect(config)
        .await
        .expect("PgvectorAdapter::connect");
    let healthy = adapter
        .health_check()
        .await
        .expect("health_check should succeed");
    assert!(healthy, "pgvector should report healthy");
}

#[tokio::test]
async fn pgvector_query_vector_dim_mismatch_fails_fast() {
    test_infra::containers::docker_check();
    let inst = test_infra::containers::pgvector_container().await;

    test_infra::containers::pgv_execute(&inst.base_url, "CREATE EXTENSION IF NOT EXISTS vector")
        .await;
    test_infra::containers::pgv_execute(
        &inst.base_url,
        "CREATE TABLE IF NOT EXISTS dim_test (
            id SERIAL PRIMARY KEY,
            content TEXT NOT NULL,
            embedding vector(4)
        )",
    )
    .await;
    test_infra::containers::pgv_insert(
        &inst.base_url,
        "dim_test",
        "dim probe",
        &[0.1, 0.2, 0.3, 0.4],
    )
    .await;

    let config = PgvectorConfig {
        url: format!("postgresql://postgres:test@{}/test", inst.base_url),
        table: "dim_test".into(),
        embedding_column: "embedding".into(),
        content_column: "content".into(),
        title_column: None,
        metadata_columns: vec![],
        distance_metric: DistanceMetric::Cosine,
        dimensions: Some(4),
        timeout_secs: 5,
    };
    let adapter = PgvectorAdapter::connect(config)
        .await
        .expect("PgvectorAdapter::connect");

    let req = QueryRequest {
        query: "dim".into(),
        top_k: Some(3),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };
    let bad = adapter.query_vector(&req, &[0.1, 0.2, 0.3]).await;
    assert!(bad.is_err(), "wrong dim must fail before SQL");
    let msg = bad.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        msg.contains("dimension mismatch"),
        "error should mention dimension mismatch, got: {msg}"
    );

    let ok = adapter
        .query_vector(&req, &[0.1, 0.2, 0.3, 0.4])
        .await
        .expect("correct dim query");
    assert!(!ok.results.is_empty());
}
