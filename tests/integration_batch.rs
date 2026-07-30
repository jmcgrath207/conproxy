//! Batch partial-failure semantics against Meilisearch (plan 05 Wave4).
//!
//! Default: per-item errors (`fail_fast=false`) → mixed success/error, `complete=true`.
//! `fail_fast=true` (sequential path via unit tests) stops early.
//!
//! Requires `--features integration-tests` and Docker.

#![cfg(feature = "integration-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod test_infra;

use conproxy::proxy::batch::{BatchConfig, BatchProcessor, BatchQuery, BatchRequest};
use conproxy::proxy::meilisearch::{MeilisearchAdapter, MeilisearchConfig};
use conproxy::proxy::types::QueryRequest;
use conproxy::proxy::upstream::UpstreamAdapter;
use std::sync::Arc;
use std::time::Duration;
use test_infra::containers::MEILI_MASTER_KEY;

fn adapter(base_url: &str, index: &str) -> MeilisearchAdapter {
    MeilisearchAdapter::new(MeilisearchConfig {
        base_url: base_url.to_string(),
        index: index.to_string(),
        timeout: Duration::from_secs(10),
        search_attributes: vec!["content".to_string(), "title".to_string()],
        displayed_attributes: vec![],
        api_key: Some(MEILI_MASTER_KEY.to_string()),
        score_threshold: None,
    })
    .expect("MeilisearchAdapter::new")
}

#[tokio::test]
async fn batch_partial_failure_per_item_against_meili() {
    test_infra::containers::docker_check();
    let inst = test_infra::containers::meilisearch_container().await;

    test_infra::containers::meili_create_index(&inst.base_url, "batch_docs", "id").await;
    test_infra::containers::meili_add_documents(
        &inst.base_url,
        "batch_docs",
        vec![serde_json::json!({
            "id": "d1",
            "title": "Rust",
            "content": "Rust systems programming language"
        })],
    )
    .await;

    let good = Arc::new(adapter(&inst.base_url, "batch_docs"));
    // Bad index → upstream error for that item only
    let bad = Arc::new(adapter(&inst.base_url, "does_not_exist_idx"));

    let processor = BatchProcessor::new(BatchConfig {
        parallel: true,
        max_parallel: 4,
        ..Default::default()
    });

    let request = BatchRequest {
        queries: vec![
            BatchQuery {
                id: "ok".into(),
                query: "rust".into(),
                top_k: Some(5),
                priority: None,
            },
            BatchQuery {
                id: "fail".into(),
                query: "anything".into(),
                top_k: Some(5),
                priority: None,
            },
            BatchQuery {
                id: "ok2".into(),
                query: "systems".into(),
                top_k: Some(5),
                priority: None,
            },
        ],
        fail_fast: false,
        timeout_ms: Some(30_000),
    };

    let good_c = good.clone();
    let bad_c = bad.clone();
    let response = processor
        .process(request, move |req: QueryRequest| {
            let good = good_c.clone();
            let bad = bad_c.clone();
            async move {
                // Route by query text: "anything" → bad index
                if req.query == "anything" {
                    bad.query(&req).await.map_err(|e| e.to_string())
                } else {
                    good.query(&req).await.map_err(|e| e.to_string())
                }
            }
        })
        .await
        .expect("batch process");

    assert!(response.complete, "fail_fast=false should complete");
    assert_eq!(response.success_count, 2, "two ok items: {response:?}");
    assert_eq!(response.error_count, 1, "one fail item: {response:?}");
    assert!(response.results["ok"].success);
    assert!(!response.results["fail"].success);
    assert!(response.results["ok2"].success);
    assert!(
        response.results["fail"].error.is_some(),
        "fail item needs error string"
    );
}

#[tokio::test]
async fn batch_all_success_meili() {
    test_infra::containers::docker_check();
    let inst = test_infra::containers::meilisearch_container().await;

    test_infra::containers::meili_create_index(&inst.base_url, "batch_ok", "id").await;
    test_infra::containers::meili_add_documents(
        &inst.base_url,
        "batch_ok",
        vec![serde_json::json!({
            "id": "d1",
            "title": "Python",
            "content": "asyncio event loop"
        })],
    )
    .await;

    let up = Arc::new(adapter(&inst.base_url, "batch_ok"));
    let processor = BatchProcessor::default_config();
    let request = BatchRequest {
        queries: vec![
            BatchQuery {
                id: "q1".into(),
                query: "asyncio".into(),
                top_k: Some(3),
                priority: None,
            },
            BatchQuery {
                id: "q2".into(),
                query: "python".into(),
                top_k: Some(3),
                priority: None,
            },
        ],
        fail_fast: false,
        timeout_ms: None,
    };

    let up_c = up.clone();
    let response = processor
        .process(request, move |req| {
            let up = up_c.clone();
            async move { up.query(&req).await.map_err(|e| e.to_string()) }
        })
        .await
        .unwrap();

    assert!(response.complete);
    assert_eq!(response.success_count, 2);
    assert_eq!(response.error_count, 0);
}
