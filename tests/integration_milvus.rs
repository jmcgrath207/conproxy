//! Integration tests for Milvus adapter against a real Milvus container.
//!
//! Requires `--features integration-tests` and a running Docker daemon.

#![cfg(feature = "integration-tests")]

mod test_infra;

use conproxy::proxy::milvus::{MilvusAdapter, MilvusConfig};
use conproxy::proxy::types::QueryRequest;
use conproxy::proxy::upstream::UpstreamAdapter;
use std::time::Duration;

#[tokio::test]
async fn milvus_vector_query_returns_results() {
    test_infra::containers::docker_check();
    let inst = test_infra::containers::milvus_container().await;

    test_infra::containers::milvus_create_collection(&inst.base_url, "vec_test", 4).await;
    test_infra::containers::milvus_insert(
        &inst.base_url,
        "vec_test",
        vec![
            (1, vec![0.1, 0.2, 0.3, 0.4], "alpha".into()),
            (2, vec![0.9, 0.8, 0.7, 0.6], "beta".into()),
        ],
    )
    .await;

    let adapter = MilvusAdapter::new(MilvusConfig {
        base_url: inst.base_url.clone(),
        collection_name: "vec_test".into(),
        timeout: Duration::from_secs(30),
        dimensions: Some(4),
        ..Default::default()
    })
    .expect("MilvusAdapter::new");

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
async fn milvus_health_check() {
    test_infra::containers::docker_check();
    let inst = test_infra::containers::milvus_container().await;

    let adapter =
        MilvusAdapter::simple(&inst.base_url, "any", Duration::from_secs(10)).expect("adapter");
    let healthy = adapter
        .health_check()
        .await
        .expect("health_check should succeed");
    assert!(healthy, "Milvus should report healthy");
}

#[tokio::test]
async fn milvus_dim_mismatch_fails_fast() {
    test_infra::containers::docker_check();
    let inst = test_infra::containers::milvus_container().await;
    test_infra::containers::milvus_create_collection(&inst.base_url, "dim_test", 4).await;

    let adapter = MilvusAdapter::new(MilvusConfig {
        base_url: inst.base_url.clone(),
        collection_name: "dim_test".into(),
        timeout: Duration::from_secs(10),
        dimensions: Some(4),
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
    let err = adapter
        .query_vector(&req, &[0.1, 0.2])
        .await
        .expect_err("dim mismatch");
    assert!(err.to_string().contains("dimension mismatch"), "got {err}");
}
