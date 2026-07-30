//! Integration tests for Qdrant adapter against a real Qdrant container.
//!
//! Requires `--features integration-tests` and a running Docker daemon.

#![cfg(feature = "integration-tests")]

mod test_infra;

use conproxy::proxy::qdrant::QdrantAdapter;
use conproxy::proxy::types::QueryRequest;
use conproxy::proxy::upstream::UpstreamAdapter;
use std::time::Duration;

#[tokio::test]
async fn qdrant_vector_query_returns_results() {
    test_infra::containers::docker_check();
    let inst = test_infra::containers::qdrant_container().await;

    // Create collection with 4-dim vectors
    test_infra::containers::qdrant_create_collection(&inst.base_url, "vec_test", 4).await;
    test_infra::containers::qdrant_insert_points(
        &inst.base_url,
        "vec_test",
        vec![
            serde_json::json!({
                "id": 1,
                "vector": [0.1, 0.2, 0.3, 0.4],
                "payload": { "content": "alpha" }
            }),
            serde_json::json!({
                "id": 2,
                "vector": [0.9, 0.8, 0.7, 0.6],
                "payload": { "content": "beta" }
            }),
        ],
    )
    .await;

    let adapter = QdrantAdapter::simple(&inst.base_url, "vec_test", Duration::from_secs(10))
        .expect("QdrantAdapter::simple");

    let req = QueryRequest {
        query: String::new(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };
    let vector = test_infra::containers::sample_vector(4);
    let resp = adapter
        .query_vector(&req, &vector)
        .await
        .expect("query_vector should succeed");
    assert!(
        !resp.results.is_empty(),
        "should return at least one result"
    );
    for r in &resp.results {
        assert!(
            (0.0..=1.1).contains(&r.score),
            "score {:.4} not in [0,1]",
            r.score
        );
    }
}

#[tokio::test]
async fn qdrant_health_check() {
    test_infra::containers::docker_check();
    let inst = test_infra::containers::qdrant_container().await;

    // health_check verifies collection exists + status green
    test_infra::containers::qdrant_create_collection(&inst.base_url, "health_test", 4).await;

    let adapter = QdrantAdapter::simple(&inst.base_url, "health_test", Duration::from_secs(5))
        .expect("QdrantAdapter::simple");
    let healthy = adapter
        .health_check()
        .await
        .expect("health_check should succeed");
    assert!(healthy, "Qdrant should report healthy");
}
