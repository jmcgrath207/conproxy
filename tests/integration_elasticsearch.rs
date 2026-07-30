//! Integration tests for Elasticsearch adapter against a real ES container.
//!
//! Requires `--features integration-tests` and a running Docker daemon.

#![cfg(feature = "integration-tests")]

mod test_infra;

use conproxy::proxy::elasticsearch::{ElasticsearchAdapter, ElasticsearchConfig};
use conproxy::proxy::types::QueryRequest;
use conproxy::proxy::upstream::UpstreamAdapter;
use std::time::Duration;

#[tokio::test]
async fn elasticsearch_query_returns_results() {
    test_infra::containers::docker_check();
    let inst = test_infra::containers::elasticsearch_container().await;

    // Create index with text mapping
    test_infra::containers::es_create_index(&inst.base_url, "test_docs", &["content", "title"])
        .await;

    // Index documents
    test_infra::containers::es_index_docs(
        &inst.base_url,
        "test_docs",
        vec![
            serde_json::json!({
                "content": "the quick brown fox jumps over the lazy dog",
                "title": "Fox and Dog"
            }),
            serde_json::json!({
                "content": "alpha bravo charlie delta echo foxtrot",
                "title": "NATO Alphabet"
            }),
        ],
    )
    .await;

    // Construct adapter
    let adapter = ElasticsearchAdapter::new(ElasticsearchConfig {
        base_url: inst.base_url.clone(),
        index: "test_docs".into(),
        timeout: Duration::from_secs(10),
        search_fields: vec!["content".into()],
        return_fields: vec![],
        api_key: None,
        score_threshold: None,
    })
    .expect("ElasticsearchAdapter::new");

    let req = QueryRequest {
        query: "fox".into(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };
    let resp = adapter.query(&req).await.expect("query should succeed");
    assert!(
        !resp.results.is_empty(),
        "should return at least one result"
    );
    // First result should have score ~1.0 (normalized BM25 max)
    assert!(
        (resp.results[0].score - 1.0).abs() < 0.01,
        "top score should be very close to 1.0 (normalized), got {:.4}",
        resp.results[0].score
    );
}

#[tokio::test]
async fn elasticsearch_health_check() {
    test_infra::containers::docker_check();
    let inst = test_infra::containers::elasticsearch_container().await;
    let adapter = ElasticsearchAdapter::new(ElasticsearchConfig {
        base_url: inst.base_url.clone(),
        index: "dummy".into(),
        timeout: Duration::from_secs(5),
        search_fields: vec!["content".into()],
        return_fields: vec![],
        api_key: None,
        score_threshold: None,
    })
    .expect("ElasticsearchAdapter::new");
    let healthy = adapter
        .health_check()
        .await
        .expect("health_check should succeed");
    assert!(healthy, "ES should report healthy");
}
