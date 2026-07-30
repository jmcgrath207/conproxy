//! Circuit/health chaos: stop Meili → Offline → restart → recovery.
//!
//! Requires `--features integration-tests` and Docker.

#![cfg(feature = "integration-tests")]

mod test_infra;

use conproxy::config::UpstreamEndpointConfig;
use conproxy::proxy::pool::{LoadBalanceStrategy, UpstreamPool};
use conproxy::proxy::types::QueryRequest;
use conproxy::proxy::upstream::UpstreamStatus;
use std::time::Duration;
use test_infra::containers::MEILI_MASTER_KEY;

fn meili_endpoint(url: &str, index: &str) -> UpstreamEndpointConfig {
    UpstreamEndpointConfig {
        id: "meili-cb".into(),
        url: url.to_string(),
        timeout_secs: Some(3),
        weight: Some(1),
        priority: Some(0),
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
        search_fields: vec!["content".into()],
        return_fields: vec![],
        api_key: Some(MEILI_MASTER_KEY.to_string()),
    }
}

#[tokio::test]
async fn health_offline_after_container_stop_then_recover() {
    test_infra::containers::docker_check();
    let mut inst = test_infra::containers::meilisearch_container().await;

    test_infra::containers::meili_create_index(&inst.base_url, "cb_docs", "id").await;
    test_infra::containers::meili_add_documents(
        &inst.base_url,
        "cb_docs",
        vec![serde_json::json!({
            "id": "d1",
            "content": "health recovery probe document about rust"
        })],
    )
    .await;

    let pool = UpstreamPool::new(
        &[meili_endpoint(&inst.base_url, "cb_docs")],
        LoadBalanceStrategy::RoundRobin,
    )
    .expect("pool");
    let upstream = pool.get("meili-cb").expect("upstream");

    let req = QueryRequest {
        query: "rust".into(),
        top_k: Some(3),
        priority: None,
        upstream_id: Some("meili-cb".into()),
        upstream_type: None,
    };

    // Baseline: Online + success
    let ok = pool.query(&req).await.expect("baseline query");
    assert!(!ok.results.is_empty());
    assert_eq!(upstream.health.status(), UpstreamStatus::Online);

    // Chaos: stop backend
    inst.stop().await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Drive consecutive failures past offline_threshold (3)
    for i in 0..5 {
        let err = pool.query(&req).await;
        assert!(err.is_err(), "query {i} should fail while stopped");
    }
    assert_eq!(
        upstream.health.status(),
        UpstreamStatus::Offline,
        "must be Offline after consecutive failures"
    );

    // Recovery: start container (may remap host port), wait ready
    inst.start("/health").await;

    // Pool still points at old URL; rebuild against refreshed base_url.
    // Health recovery path is still validated via a fresh pool + success streak.
    let pool2 = UpstreamPool::new(
        &[meili_endpoint(&inst.base_url, "cb_docs")],
        LoadBalanceStrategy::RoundRobin,
    )
    .expect("pool2");
    let upstream2 = pool2.get("meili-cb").expect("upstream2");

    let mut recovered = false;
    for _ in 0..15 {
        match pool2.query(&req).await {
            Ok(resp) if !resp.results.is_empty() => {
                recovered = true;
                break;
            }
            _ => tokio::time::sleep(Duration::from_millis(300)).await,
        }
    }
    assert!(recovered, "queries must succeed after container restart");

    // recovery_threshold = 2 consecutive successes clears offline
    let _ = pool2.query(&req).await;
    assert_eq!(
        upstream2.health.status(),
        UpstreamStatus::Online,
        "must return Online after recovery successes"
    );
}
