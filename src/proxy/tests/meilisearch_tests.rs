#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;
use crate::proxy::upstream::QueryMode;

#[test]
fn test_meilisearch_config_default() {
    let config = MeilisearchConfig::default();
    assert_eq!(config.base_url, "http://localhost:7700");
    assert_eq!(config.index, "documents");
    assert_eq!(config.timeout, Duration::from_secs(30));
    assert_eq!(config.search_attributes, Vec::<String>::new());
    assert!(config.displayed_attributes.is_empty());
    assert!(config.api_key.is_none());
    assert!(config.score_threshold.is_none());
}

#[test]
fn test_meilisearch_adapter_creation() {
    let adapter = MeilisearchAdapter::simple(
        "http://localhost:7700",
        "test_index",
        Duration::from_secs(15),
    )
    .unwrap();

    assert_eq!(adapter.identifier(), "http://localhost:7700");
    assert_eq!(adapter.timeout(), Duration::from_secs(15));
}

#[test]
fn test_meilisearch_adapter_urls() {
    let adapter =
        MeilisearchAdapter::simple("http://localhost:7700", "my_docs", Duration::from_secs(30))
            .unwrap();

    assert_eq!(
        adapter.search_url(),
        "http://localhost:7700/indexes/my_docs/search"
    );
    assert_eq!(adapter.health_url(), "http://localhost:7700/health");
}

#[test]
fn test_meilisearch_adapter_urls_trailing_slash() {
    let adapter =
        MeilisearchAdapter::simple("http://localhost:7700/", "my_docs", Duration::from_secs(30))
            .unwrap();

    assert_eq!(
        adapter.search_url(),
        "http://localhost:7700/indexes/my_docs/search"
    );
    assert_eq!(adapter.health_url(), "http://localhost:7700/health");
}

#[test]
fn test_meilisearch_adapter_metadata() {
    let config = MeilisearchConfig {
        index: "docs-2026".to_string(),
        search_attributes: vec!["content".to_string(), "title".to_string()],
        ..Default::default()
    };
    let adapter = MeilisearchAdapter::new(config).unwrap();

    let metadata = adapter.metadata();
    assert_eq!(metadata.adapter_type, "meilisearch");
    assert_eq!(
        metadata.properties.get("index"),
        Some(&"docs-2026".to_string())
    );
    assert_eq!(
        metadata.properties.get("search_attributes"),
        Some(&"content, title".to_string())
    );
}

#[test]
fn test_meilisearch_adapter_query_mode() {
    let adapter =
        MeilisearchAdapter::simple("http://localhost:7700", "test", Duration::from_secs(30))
            .unwrap();

    // Meilisearch adapter should always be TextNative.
    assert_eq!(adapter.query_mode(), QueryMode::TextNative);
}

#[test]
fn test_meilisearch_score_normalization() {
    // Normal case: rankingScore already in [0, 1].
    assert_eq!(MeilisearchAdapter::normalize_score(Some(0.85)), 0.85);
    assert_eq!(MeilisearchAdapter::normalize_score(Some(0.0)), 0.0);
    assert_eq!(MeilisearchAdapter::normalize_score(Some(1.0)), 1.0);

    // None → 0.0
    assert_eq!(MeilisearchAdapter::normalize_score(None), 0.0);

    // Defensive clamping for out-of-range values.
    assert_eq!(MeilisearchAdapter::normalize_score(Some(1.5)), 1.0);
    assert_eq!(MeilisearchAdapter::normalize_score(Some(-0.5)), 0.0);
}

#[test]
fn test_meilisearch_response_parsing_basic() {
    let meili_json = serde_json::json!({
        "hits": [
            {
                "id": "doc-001",
                "title": "Rust async tokio",
                "content": "Tokio is an async runtime for Rust.",
                "category": "rust",
                "_rankingScore": 0.95
            },
            {
                "id": "doc-002",
                "title": "Python asyncio",
                "content": "asyncio is async for Python.",
                "category": "python",
                "_rankingScore": 0.42
            }
        ],
        "estimatedTotalHits": 2,
        "processingTimeMs": 1
    });
    let parsed: MeiliSearchResponse = serde_json::from_value(meili_json).unwrap();
    let results = MeilisearchAdapter::parse_hits(&parsed);

    assert_eq!(results.len(), 2);

    assert_eq!(results[0].id, "doc-001");
    assert!((results[0].score - 0.95).abs() < 0.001);
    assert_eq!(results[0].content, "Tokio is an async runtime for Rust.");

    assert_eq!(results[1].id, "doc-002");
    assert!((results[1].score - 0.42).abs() < 0.001);
}

#[test]
fn test_meilisearch_response_parsing_no_ranking_score() {
    // Older indexes without showRankingScore enabled omit _rankingScore.
    let meili_json = serde_json::json!({
        "hits": [
            {
                "id": "doc-001",
                "content": "Some content."
            }
        ]
    });
    let parsed: MeiliSearchResponse = serde_json::from_value(meili_json).unwrap();
    let results = MeilisearchAdapter::parse_hits(&parsed);

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "doc-001");
    assert_eq!(results[0].score, 0.0);
    assert_eq!(results[0].content, "Some content.");
}

#[test]
fn test_meilisearch_response_parsing_empty_hits() {
    let meili_json = serde_json::json!({
        "hits": [],
        "estimatedTotalHits": 0,
        "processingTimeMs": 0
    });
    let parsed: MeiliSearchResponse = serde_json::from_value(meili_json).unwrap();
    let results = MeilisearchAdapter::parse_hits(&parsed);
    assert!(results.is_empty());
}

#[test]
fn test_meilisearch_response_parsing_alternate_content_fields() {
    let meili_json = serde_json::json!({
        "hits": [
            {
                "id": "doc-001",
                "text": "content via text field"
            },
            {
                "id": "doc-002",
                "body": "content via body field"
            }
        ]
    });
    let parsed: MeiliSearchResponse = serde_json::from_value(meili_json).unwrap();
    let results = MeilisearchAdapter::parse_hits(&parsed);

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].content, "content via text field");
    assert_eq!(results[1].content, "content via body field");
}

#[test]
fn test_meilisearch_build_query_body() {
    let adapter =
        MeilisearchAdapter::simple("http://localhost:7700", "docs", Duration::from_secs(30))
            .unwrap();
    let body = adapter.build_query_body("rust async", 5);

    assert_eq!(body["q"], "rust async");
    assert_eq!(body["limit"], 5);
    assert_eq!(body["showRankingScore"], true);
    // Default search_attributes is empty → no attributesToSearchOn sent.
    assert!(body.get("attributesToSearchOn").is_none());
}

#[test]
fn test_meilisearch_build_query_body_with_displayed_attrs() {
    let config = MeilisearchConfig {
        displayed_attributes: vec!["title".to_string(), "content".to_string()],
        ..Default::default()
    };
    let adapter = MeilisearchAdapter::new(config).unwrap();
    let body = adapter.build_query_body("test", 10);

    assert_eq!(body["attributesToRetrieve"][0], "title");
    assert_eq!(body["attributesToRetrieve"][1], "content");
}

#[test]
fn test_meilisearch_health_response_parsing() {
    let json = serde_json::json!({"status": "available"});
    let health: MeiliHealthResponse = serde_json::from_value(json).unwrap();
    assert_eq!(health.status, "available");

    let json = serde_json::json!({"status": "unavailable"});
    let health: MeiliHealthResponse = serde_json::from_value(json).unwrap();
    assert_eq!(health.status, "unavailable");
}

#[test]
fn test_meilisearch_version_response_parsing() {
    let json = serde_json::json!({
        "pkgVersion": "1.8.0",
        "commitSha": "abc123",
        "commitDate": "2024-01-15"
    });
    let v: MeiliVersionResponse = serde_json::from_value(json).unwrap();
    assert_eq!(v.pkg_version, "1.8.0");
}

#[test]
fn test_meilisearch_helpers_extract_id() {
    let v = serde_json::json!({"id": "doc-1", "content": "x"});
    assert_eq!(hit_id(&v), "doc-1");

    let v = serde_json::json!({"uid": "doc-2", "content": "y"});
    assert_eq!(hit_id(&v), "doc-2");

    let v = serde_json::json!({"content": "no id"});
    assert_eq!(hit_id(&v), "");
}

#[test]
fn test_meilisearch_helpers_extract_id_numeric() {
    // Meili returns integer primary keys as JSON numbers.
    let v = serde_json::json!({"id": 1, "body": "x"});
    assert_eq!(hit_id(&v), "1");

    let v = serde_json::json!({"id": 42, "body": "y"});
    assert_eq!(hit_id(&v), "42");

    // String ids still work.
    let v = serde_json::json!({"id": "doc-1", "body": "z"});
    assert_eq!(hit_id(&v), "doc-1");

    // Missing id → empty.
    let v = serde_json::json!({"body": "no id"});
    assert_eq!(hit_id(&v), "");
}

#[test]
fn test_meilisearch_parse_hit_with_numeric_id() {
    // Full parse: numeric id + body content → hit kept.
    let meili_json = serde_json::json!({
        "hits": [{"id": 1, "body": "rust errors", "_rankingScore": 0.9}],
        "estimatedTotalHits": 1,
        "processingTimeMs": 1
    });
    let parsed: MeiliSearchResponse = serde_json::from_value(meili_json).unwrap();
    let results = MeilisearchAdapter::parse_hits(&parsed);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "1");
    assert_eq!(results[0].content, "rust errors");
    assert!((results[0].score - 0.9).abs() < 0.001);
}

#[test]
fn test_meilisearch_helpers_ranking_score() {
    let v = serde_json::json!({"_rankingScore": 0.75});
    assert!((v.ranking_score().unwrap() - 0.75).abs() < 0.001);

    let v = serde_json::json!({"content": "x"});
    assert!(v.ranking_score().is_none());
}

#[test]
fn test_meilisearch_metadata_strips_internal_fields() {
    let meili_json = serde_json::json!({
        "hits": [{
            "id": "doc-001",
            "title": "T",
            "content": "C",
            "_rankingScore": 0.9,
            "_formatted": {"title": "<em>T</em>"}
        }]
    });
    let parsed: MeiliSearchResponse = serde_json::from_value(meili_json).unwrap();
    let results = MeilisearchAdapter::parse_hits(&parsed);
    assert_eq!(results.len(), 1);

    // Metadata should contain only user fields (title — id/content/_internal stripped).
    let meta = results[0].metadata.as_ref().unwrap();
    let obj = meta.as_object().unwrap();
    assert!(!obj.contains_key("content"));
    assert!(!obj.contains_key("id"));
    assert!(!obj.contains_key("_rankingScore"));
    assert!(!obj.contains_key("_formatted"));
    assert!(obj.contains_key("title"));
}
