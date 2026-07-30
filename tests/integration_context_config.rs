//! Integration tests for context-rooted config (plan 10 T5).
//!
//! Shared Meilisearch resource, per-context index overrides, isolated scope
//! filters, and cache keys namespaced by `ctx:<id>:<query>`.
//!
//! Requires `--features integration-tests` and a running Docker daemon.

#![cfg(feature = "integration-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod test_infra;

use conproxy::config::{
    resolve_all_contexts, ContextCacheConfig, ContextLegConfig, NamedContextConfig,
    ProxyScopeConfig, UpstreamResourceConfig, WeightedPhrase,
};
use conproxy::proxy::cache::CacheStore;
use conproxy::proxy::context::ContextManager;
use conproxy::proxy::meilisearch::{MeilisearchAdapter, MeilisearchConfig};
use conproxy::proxy::types::{CacheStatus, QueryRequest, QueryResponse, SearchResult};
use conproxy::proxy::upstream::UpstreamAdapter;
use std::collections::HashMap;
use std::time::Duration;
use test_infra::containers::MEILI_MASTER_KEY;

fn meili_adapter(base_url: &str, index: &str) -> MeilisearchAdapter {
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

fn two_context_resolve(base_url: &str) -> Vec<conproxy::config::ResolvedContext> {
    let mut upstreams = HashMap::new();
    upstreams.insert(
        "meili".into(),
        UpstreamResourceConfig {
            url: Some(base_url.into()),
            upstream_type: Some("meilisearch".into()),
            api_key: Some(MEILI_MASTER_KEY.into()),
            timeout_secs: Some(30),
            ..Default::default()
        },
    );

    let mut contexts = HashMap::new();
    contexts.insert(
        "docs".into(),
        NamedContextConfig {
            default: Some(true),
            description: Some("docs".into()),
            upstreams: vec![ContextLegConfig {
                resource_ref: "meili".into(),
                index: Some("docs".into()),
                timeout_secs: Some(5),
                ..Default::default()
            }],
            cache: ContextCacheConfig {
                fresh_secs: Some(300),
                max_entries: Some(1000),
                ..Default::default()
            },
            scope: ProxyScopeConfig {
                mode: Some("filter".into()),
                weighted_phrases: vec![WeightedPhrase {
                    text: "rust async".into(),
                    weight: 1.0,
                    min_similarity: None,
                }],
                min_seed_similarity: Some(0.2),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    contexts.insert(
        "support".into(),
        NamedContextConfig {
            default: Some(false),
            upstreams: vec![ContextLegConfig {
                resource_ref: "meili".into(),
                index: Some("support".into()),
                timeout_secs: Some(15),
                ..Default::default()
            }],
            cache: ContextCacheConfig {
                fresh_secs: Some(60),
                max_entries: Some(500),
                ..Default::default()
            },
            scope: ProxyScopeConfig {
                mode: Some("filter".into()),
                weighted_phrases: vec![WeightedPhrase {
                    text: "billing invoice".into(),
                    weight: 1.2,
                    min_similarity: None,
                }],
                min_seed_similarity: Some(0.2),
                ..Default::default()
            },
            ..Default::default()
        },
    );

    resolve_all_contexts(&upstreams, &HashMap::new(), &contexts).expect("resolve")
}

/// Prefix used by the proxy for per-context cache keys.
fn context_query(context_id: &str, query: &str) -> String {
    format!("ctx:{context_id}:{query}")
}

#[tokio::test]
async fn shared_ref_two_contexts_query_ok() {
    test_infra::containers::docker_check();
    let inst = test_infra::containers::meilisearch_container().await;

    test_infra::containers::meili_create_index(&inst.base_url, "docs", "id").await;
    test_infra::containers::meili_create_index(&inst.base_url, "support", "id").await;
    test_infra::containers::meili_add_documents(
        &inst.base_url,
        "docs",
        vec![serde_json::json!({
            "id": "d1",
            "title": "Rust async guide",
            "content": "Tokio is an async runtime for Rust."
        })],
    )
    .await;
    test_infra::containers::meili_add_documents(
        &inst.base_url,
        "support",
        vec![serde_json::json!({
            "id": "s1",
            "title": "Billing FAQ",
            "content": "How to pay an invoice and refund policy."
        })],
    )
    .await;

    let resolved = two_context_resolve(&inst.base_url);
    assert_eq!(resolved.len(), 2);

    let docs = resolved.iter().find(|c| c.id == "docs").unwrap();
    let support = resolved.iter().find(|c| c.id == "support").unwrap();

    // Same resource id
    assert_eq!(docs.legs[0].resource_id, "meili");
    assert_eq!(support.legs[0].resource_id, "meili");
    // Overrides
    assert_eq!(docs.legs[0].endpoint.index.as_deref(), Some("docs"));
    assert_eq!(support.legs[0].endpoint.index.as_deref(), Some("support"));
    assert_eq!(docs.legs[0].endpoint.timeout_secs, Some(5));
    assert_eq!(support.legs[0].endpoint.timeout_secs, Some(15));

    let docs_ad = meili_adapter(&inst.base_url, "docs");
    let sup_ad = meili_adapter(&inst.base_url, "support");

    let docs_resp = docs_ad
        .query(&QueryRequest {
            query: "async rust".into(),
            top_k: Some(5),
            priority: None,
            upstream_id: None,
            upstream_type: None,
        })
        .await
        .expect("docs query");
    assert!(
        !docs_resp.results.is_empty(),
        "docs index should return hits"
    );

    let sup_resp = sup_ad
        .query(&QueryRequest {
            query: "invoice".into(),
            top_k: Some(5),
            priority: None,
            upstream_id: None,
            upstream_type: None,
        })
        .await
        .expect("support query");
    assert!(
        !sup_resp.results.is_empty(),
        "support index should return hits"
    );
}

#[tokio::test]
async fn isolated_scope_filters_differ() {
    test_infra::containers::docker_check();
    let inst = test_infra::containers::meilisearch_container().await;
    // container only needed for resolve URL validity path; scope is pure
    let _ = &inst.base_url;

    let resolved = two_context_resolve(&inst.base_url);
    let mgr = ContextManager::from_resolved(&resolved);

    let docs_sf = mgr.scope_filter_for("docs").expect("docs scope");
    let sup_sf = mgr.scope_filter_for("support").expect("support scope");
    assert!(docs_sf.is_enabled());
    assert!(sup_sf.is_enabled());

    let mixed = vec![
        SearchResult {
            id: "1".into(),
            score: 0.9,
            content: "Tokio is an async runtime for Rust programming.".into(),
            metadata: None,
            upstream_id: None,
        },
        SearchResult {
            id: "2".into(),
            score: 0.8,
            content: "Customer billing invoice and refund window.".into(),
            metadata: None,
            upstream_id: None,
        },
    ];

    let docs_out = docs_sf.filter_results(mixed.clone());
    let sup_out = sup_sf.filter_results(mixed);

    let docs_ids: Vec<_> = docs_out.iter().map(|r| r.id.as_str()).collect();
    let sup_ids: Vec<_> = sup_out.iter().map(|r| r.id.as_str()).collect();

    assert!(
        docs_ids.contains(&"1"),
        "docs scope should keep rust/async hit, got {docs_ids:?}"
    );
    assert!(
        !docs_ids.contains(&"2"),
        "docs scope should drop billing hit, got {docs_ids:?}"
    );
    assert!(
        sup_ids.contains(&"2"),
        "support scope should keep billing hit, got {sup_ids:?}"
    );
    assert!(
        !sup_ids.contains(&"1"),
        "support scope should drop rust hit, got {sup_ids:?}"
    );

    // Independent cache policy objects
    assert_eq!(mgr.policy_for("docs").unwrap().cache.fresh_secs, Some(300));
    assert_eq!(
        mgr.policy_for("support").unwrap().cache.fresh_secs,
        Some(60)
    );
}

#[tokio::test]
async fn isolated_cache_keys_by_context() {
    test_infra::containers::docker_check();
    let inst = test_infra::containers::meilisearch_container().await;
    let _ = &inst; // docker gate only; cache is in-process

    let cache = CacheStore::new(Duration::from_secs(300), Duration::from_secs(600), 1000);

    let query = "shared query text";
    let key_a = context_query("docs", query);
    let key_b = context_query("support", query);

    let response = QueryResponse {
        results: vec![SearchResult {
            id: "only-docs".into(),
            score: 1.0,
            content: "docs-only payload".into(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };

    cache.insert_with_context(&key_a, response, "meili".into(), "docs");

    assert!(cache.get(&key_a).is_some(), "docs key must hit");
    assert!(cache.get(&key_b).is_none(), "support key must miss");
    assert!(cache.get(query).is_none(), "raw query must miss");
}
