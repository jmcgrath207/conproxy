//! Integration tests for the Meilisearch adapter against a real Meilisearch v1.8 container.
//!
//! Requires `--features integration-tests` and a running Docker daemon.

#![cfg(feature = "integration-tests")]

mod test_infra;

use conproxy::proxy::meilisearch::{MeilisearchAdapter, MeilisearchConfig};
use conproxy::proxy::types::QueryRequest;
use conproxy::proxy::upstream::UpstreamAdapter;
use std::time::Duration;
use test_infra::containers::MEILI_MASTER_KEY;

fn adapter(base_url: &str, index: &str) -> MeilisearchAdapter {
    MeilisearchAdapter::new(MeilisearchConfig {
        base_url: base_url.to_string(),
        index: index.to_string(),
        timeout: Duration::from_secs(10),
        search_attributes: vec!["content".to_string()],
        displayed_attributes: vec![],
        api_key: Some(MEILI_MASTER_KEY.to_string()),
        score_threshold: None,
    })
    .expect("MeilisearchAdapter::new")
}

#[tokio::test]
async fn meilisearch_text_query_returns_results() {
    test_infra::containers::docker_check();
    let inst = test_infra::containers::meilisearch_container().await;

    test_infra::containers::meili_create_index(&inst.base_url, "fts_test", "id").await;
    test_infra::containers::meili_add_documents(
        &inst.base_url,
        "fts_test",
        vec![
            serde_json::json!({
                "id": "doc-001",
                "title": "Rust async tokio runtime",
                "content": "Tokio is an async runtime for the Rust programming language."
            }),
            serde_json::json!({
                "id": "doc-002",
                "title": "Python asyncio",
                "content": "asyncio is the async library in Python."
            }),
            serde_json::json!({
                "id": "doc-003",
                "title": "Distributed cache patterns",
                "content": "Read-through and write-behind caching strategies."
            }),
        ],
    )
    .await;

    let adapter = adapter(&inst.base_url, "fts_test");

    let req = QueryRequest {
        query: "rust async".to_string(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };
    let resp = adapter.query(&req).await.expect("query should succeed");
    assert!(
        !resp.results.is_empty(),
        "should return at least one result for 'rust async' in 'fts_test' index"
    );
    for r in &resp.results {
        assert!(
            (0.0..=1.0).contains(&r.score),
            "score {:.4} not in [0, 1] (Meilisearch _rankingScore)",
            r.score
        );
    }
}

#[tokio::test]
async fn meilisearch_health_check() {
    test_infra::containers::docker_check();
    let inst = test_infra::containers::meilisearch_container().await;

    test_infra::containers::meili_create_index(&inst.base_url, "health_test", "id").await;

    let adapter = adapter(&inst.base_url, "health_test");
    let healthy = adapter
        .health_check()
        .await
        .expect("health_check should succeed");
    assert!(healthy, "Meilisearch should report healthy");
}

#[tokio::test]
async fn meilisearch_metadata_reports_type() {
    test_infra::containers::docker_check();
    let inst = test_infra::containers::meilisearch_container().await;

    let adapter = adapter(&inst.base_url, "any_idx");
    let metadata = adapter.metadata();
    assert_eq!(metadata.adapter_type, "meilisearch");
    assert_eq!(
        metadata.properties.get("index"),
        Some(&"any_idx".to_string())
    );
}
