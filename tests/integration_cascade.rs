//! Dual-backend cascade proof: primary Meili misses threshold → secondary serves.
//!
//! Requires `--features integration-tests` and Docker.

#![cfg(feature = "integration-tests")]

mod test_infra;

use conproxy::config::UpstreamEndpointConfig;
use conproxy::proxy::cascade::{CascadeConfig, CascadeExecutor, CascadeStopReason};
use conproxy::proxy::pool::{LoadBalanceStrategy, UpstreamPool};
use conproxy::proxy::types::QueryRequest;
use std::sync::Arc;
use std::time::Duration;
use test_infra::containers::MEILI_MASTER_KEY;

fn endpoint(id: &str, url: &str, index: &str, priority: u32) -> UpstreamEndpointConfig {
    UpstreamEndpointConfig {
        id: id.to_string(),
        url: url.to_string(),
        timeout_secs: Some(10),
        weight: Some(1),
        priority: Some(priority),
        max_concurrent: None,
        enabled: Some(true),
        version_endpoint: None,
        version_poll_interval_secs: None,
        upstream_type: Some("meilisearch".into()),
        query_mode: Some("text_native".into()),
        table: None,
        embedding_column: None,
        content_column: None,
        metadata_columns: vec![],
        distance_metric: None,
        dimensions: None,
        index: Some(index.into()),
        search_fields: vec!["content".into(), "title".into()],
        return_fields: vec![],
        api_key: Some(MEILI_MASTER_KEY.to_string()),
    }
}

#[tokio::test]
async fn cascade_dual_meili_falls_through_on_low_score() {
    test_infra::containers::docker_check();

    let primary = test_infra::containers::meilisearch_container().await;
    let secondary = test_infra::containers::meilisearch_container().await;

    // Primary: only feline docs — weak match for "rust async runtime"
    test_infra::containers::meili_create_index(&primary.base_url, "cascade_p", "id").await;
    test_infra::containers::meili_add_documents(
        &primary.base_url,
        "cascade_p",
        vec![serde_json::json!({
            "id": "cat-1",
            "title": "Cats",
            "content": "Fluffy cats sleep on warm windowsills all day."
        })],
    )
    .await;

    // Secondary: strong match for the query
    test_infra::containers::meili_create_index(&secondary.base_url, "cascade_s", "id").await;
    test_infra::containers::meili_add_documents(
        &secondary.base_url,
        "cascade_s",
        vec![serde_json::json!({
            "id": "rust-1",
            "title": "Rust async",
            "content": "Tokio is an async runtime for Rust systems programming."
        })],
    )
    .await;

    let configs = vec![
        endpoint("meili-primary", &primary.base_url, "cascade_p", 0),
        endpoint("meili-secondary", &secondary.base_url, "cascade_s", 1),
    ];
    let pool = Arc::new(UpstreamPool::new(&configs, LoadBalanceStrategy::Failover).expect("pool"));

    // High threshold so primary cat-doc cannot stop cascade.
    let cascade = CascadeExecutor::new(
        pool,
        CascadeConfig::new()
            .with_threshold(0.95)
            .with_min_results(1)
            .with_max_depth(3)
            .with_timeout(Duration::from_secs(30)),
    );

    let req = QueryRequest {
        query: "rust async runtime tokio".into(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };
    let result = cascade.query(&req).await;

    assert!(
        !result.results.is_empty(),
        "cascade must return results, stop={:?} tried={:?}",
        result.stop_reason,
        result.upstreams_tried
    );
    assert!(
        result.upstreams_tried.len() >= 2
            || result
                .upstreams_tried
                .iter()
                .any(|id| id == "meili-secondary")
            || result.final_upstream.as_deref() == Some("meili-secondary"),
        "expected secondary in cascade path: tried={:?} final={:?}",
        result.upstreams_tried,
        result.final_upstream
    );
    assert!(
        matches!(
            result.stop_reason,
            CascadeStopReason::ThresholdMet
                | CascadeStopReason::MinResultsMet
                | CascadeStopReason::MaxDepthReached
                | CascadeStopReason::AllExhausted
        ),
        "unexpected stop: {:?}",
        result.stop_reason
    );
    let top = &result.results[0];
    assert!(
        top.content.to_lowercase().contains("tokio")
            || top.content.to_lowercase().contains("rust")
            || top.id.contains("rust"),
        "top hit should be rust/tokio doc, got id={} content={}",
        top.id,
        top.content
    );
}
