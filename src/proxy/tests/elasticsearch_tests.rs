#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

#[test]
fn test_elasticsearch_config_default() {
    let config = ElasticsearchConfig::default();
    assert_eq!(config.base_url, "http://localhost:9200");
    assert_eq!(config.index, "documents");
    assert_eq!(config.timeout, Duration::from_secs(30));
    assert_eq!(config.search_fields, vec!["content".to_string()]);
    assert!(config.return_fields.is_empty());
    assert!(config.api_key.is_none());
    assert!(config.score_threshold.is_none());
}

#[test]
fn test_elasticsearch_adapter_creation() {
    let adapter = ElasticsearchAdapter::simple(
        "http://localhost:9200",
        "test_index",
        Duration::from_secs(15),
    )
    .unwrap();

    assert_eq!(adapter.identifier(), "http://localhost:9200");
    assert_eq!(adapter.timeout(), Duration::from_secs(15));
}

#[test]
fn test_elasticsearch_adapter_urls() {
    let adapter =
        ElasticsearchAdapter::simple("http://localhost:9200", "my_docs", Duration::from_secs(30))
            .unwrap();

    assert_eq!(
        adapter.search_url(),
        "http://localhost:9200/my_docs/_search"
    );
    assert_eq!(
        adapter.health_url(),
        "http://localhost:9200/_cluster/health?timeout=5s"
    );
}

#[test]
fn test_elasticsearch_adapter_urls_trailing_slash() {
    let adapter =
        ElasticsearchAdapter::simple("http://localhost:9200/", "my_docs", Duration::from_secs(30))
            .unwrap();

    assert_eq!(
        adapter.search_url(),
        "http://localhost:9200/my_docs/_search"
    );
    assert_eq!(
        adapter.health_url(),
        "http://localhost:9200/_cluster/health?timeout=5s"
    );
}

#[test]
fn test_elasticsearch_adapter_metadata() {
    let config = ElasticsearchConfig {
        index: "docs-2026".to_string(),
        search_fields: vec!["content".to_string(), "title".to_string()],
        ..Default::default()
    };
    let adapter = ElasticsearchAdapter::new(config).unwrap();

    let metadata = adapter.metadata();
    assert_eq!(metadata.adapter_type, "elasticsearch");
    assert_eq!(
        metadata.properties.get("index"),
        Some(&"docs-2026".to_string())
    );
    assert_eq!(
        metadata.properties.get("search_fields"),
        Some(&"content, title".to_string())
    );
}

#[test]
fn test_elasticsearch_adapter_query_mode() {
    let adapter =
        ElasticsearchAdapter::simple("http://localhost:9200", "test", Duration::from_secs(30))
            .unwrap();

    // ES adapter should always be TextNative
    assert_eq!(adapter.query_mode(), QueryMode::TextNative);
}

#[test]
fn test_elasticsearch_score_normalization() {
    // Normal case: score divided by max_score
    assert_eq!(ElasticsearchAdapter::normalize_score(15.7, 15.7), 1.0);
    assert!((ElasticsearchAdapter::normalize_score(7.85, 15.7) - 0.5).abs() < 0.001);
    assert_eq!(ElasticsearchAdapter::normalize_score(0.0, 15.7), 0.0);

    // Edge case: max_score is zero
    assert_eq!(ElasticsearchAdapter::normalize_score(5.0, 0.0), 0.0);

    // Edge case: max_score is negative
    assert_eq!(ElasticsearchAdapter::normalize_score(5.0, -1.0), 0.0);

    // Clamping: score > max_score should clamp to 1.0
    assert_eq!(ElasticsearchAdapter::normalize_score(20.0, 15.0), 1.0);
}

#[test]
fn test_elasticsearch_response_parsing() {
    let es_json = serde_json::json!({
        "hits": {
            "total": {"value": 2},
            "max_score": 15.7,
            "hits": [
                {
                    "_id": "doc1",
                    "_score": 15.7,
                    "_source": {
                        "content": "Elasticsearch is a search engine",
                        "title": "ES Guide",
                        "author": "John"
                    }
                },
                {
                    "_id": "doc2",
                    "_score": 8.3,
                    "_source": {
                        "content": "Full-text search with BM25 scoring",
                        "title": "BM25 Primer"
                    }
                }
            ]
        }
    });

    let es_response: EsSearchResponse = serde_json::from_value(es_json).unwrap();
    let results = ElasticsearchAdapter::parse_hits(&es_response);

    assert_eq!(results.len(), 2);

    // First result should have normalized score 1.0 (it has max_score)
    assert_eq!(results[0].id, "doc1");
    assert!((results[0].score - 1.0).abs() < 0.001);
    assert_eq!(results[0].content, "Elasticsearch is a search engine");

    // Check metadata has title and author but not content
    let meta = results[0].metadata.as_ref().unwrap();
    assert_eq!(meta.get("title").and_then(|v| v.as_str()), Some("ES Guide"));
    assert_eq!(meta.get("author").and_then(|v| v.as_str()), Some("John"));
    assert!(meta.get("content").is_none());

    // Second result should have normalized score ~0.5287
    assert_eq!(results[1].id, "doc2");
    assert!((results[1].score - 8.3 / 15.7).abs() < 0.001);
    assert_eq!(results[1].content, "Full-text search with BM25 scoring");
}

#[test]
fn test_elasticsearch_response_parsing_empty_hits() {
    let es_json = serde_json::json!({
        "hits": {
            "total": {"value": 0},
            "max_score": null,
            "hits": []
        }
    });

    let es_response: EsSearchResponse = serde_json::from_value(es_json).unwrap();
    let results = ElasticsearchAdapter::parse_hits(&es_response);

    assert!(results.is_empty());
}

#[test]
fn test_elasticsearch_response_parsing_no_source() {
    let es_json = serde_json::json!({
        "hits": {
            "total": {"value": 1},
            "max_score": 5.0,
            "hits": [
                {
                    "_id": "doc1",
                    "_score": 5.0
                }
            ]
        }
    });

    let es_response: EsSearchResponse = serde_json::from_value(es_json).unwrap();
    let results = ElasticsearchAdapter::parse_hits(&es_response);

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "doc1");
    assert_eq!(results[0].content, "");
    assert!(results[0].metadata.is_none());
}

#[test]
fn test_elasticsearch_config_with_api_key() {
    let config = ElasticsearchConfig {
        api_key: Some("base64-encoded-key".to_string()),
        ..Default::default()
    };

    let adapter = ElasticsearchAdapter::new(config).unwrap();
    assert!(adapter.config.api_key.is_some());
    assert_eq!(
        adapter.config.api_key.as_deref(),
        Some("base64-encoded-key")
    );
}

#[test]
fn test_elasticsearch_build_query_body_basic() {
    let adapter =
        ElasticsearchAdapter::simple("http://localhost:9200", "docs", Duration::from_secs(30))
            .unwrap();

    let body = adapter.build_query_body("rust programming", 5);

    assert_eq!(body["query"]["multi_match"]["query"], "rust programming");
    assert_eq!(body["query"]["multi_match"]["type"], "best_fields");
    assert_eq!(body["size"], 5);
    // Default config has no _source filter
    assert!(body.get("_source").is_none());
    // Default config has no min_score
    assert!(body.get("min_score").is_none());
}

#[test]
fn test_elasticsearch_build_query_body_with_source_filter() {
    let config = ElasticsearchConfig {
        return_fields: vec!["content".to_string(), "title".to_string()],
        ..Default::default()
    };
    let adapter = ElasticsearchAdapter::new(config).unwrap();

    let body = adapter.build_query_body("test", 10);

    let source = body.get("_source").unwrap();
    assert!(source.is_array());
    let fields: Vec<&str> = source
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(fields, vec!["content", "title"]);
}

#[test]
fn test_elasticsearch_build_query_body_with_score_threshold() {
    let config = ElasticsearchConfig {
        score_threshold: Some(5.0),
        ..Default::default()
    };
    let adapter = ElasticsearchAdapter::new(config).unwrap();

    let body = adapter.build_query_body("test", 10);

    assert_eq!(body.get("min_score").and_then(|v| v.as_f64()), Some(5.0));
}

#[test]
fn test_elasticsearch_build_query_body_with_multiple_search_fields() {
    let config = ElasticsearchConfig {
        search_fields: vec![
            "content".to_string(),
            "title".to_string(),
            "summary".to_string(),
        ],
        ..Default::default()
    };
    let adapter = ElasticsearchAdapter::new(config).unwrap();

    let body = adapter.build_query_body("query", 10);

    let fields = body["query"]["multi_match"]["fields"].as_array().unwrap();
    assert_eq!(fields.len(), 3);
    assert_eq!(fields[0], "content");
    assert_eq!(fields[1], "title");
    assert_eq!(fields[2], "summary");
}

#[test]
fn test_elasticsearch_query_mode_set_and_get() {
    let adapter =
        ElasticsearchAdapter::simple("http://localhost:9200", "test", Duration::from_secs(30))
            .unwrap();

    // Initially TextNative
    assert_eq!(adapter.query_mode(), QueryMode::TextNative);

    // Can be overridden via set_query_mode (trait method)
    adapter.set_query_mode(QueryMode::VectorOnly);
    assert_eq!(adapter.query_mode(), QueryMode::VectorOnly);

    // Can be set back
    adapter.set_query_mode(QueryMode::TextNative);
    assert_eq!(adapter.query_mode(), QueryMode::TextNative);
}

#[test]
fn test_elasticsearch_hits_total_object_format() {
    let json = serde_json::json!({
        "hits": {
            "total": {"value": 42},
            "max_score": 10.0,
            "hits": []
        }
    });

    let response: EsSearchResponse = serde_json::from_value(json).unwrap();
    match response.hits.total {
        EsHitsTotal::Object { value } => assert_eq!(value, 42),
        _ => panic!("Expected Object variant"),
    }
}

#[test]
fn test_elasticsearch_hits_total_integer_format() {
    let json = serde_json::json!({
        "hits": {
            "total": 100,
            "max_score": 10.0,
            "hits": []
        }
    });

    let response: EsSearchResponse = serde_json::from_value(json).unwrap();
    match response.hits.total {
        EsHitsTotal::Integer(value) => assert_eq!(value, 100),
        _ => panic!("Expected Integer variant"),
    }
}

#[test]
fn test_elasticsearch_cluster_health_parsing() {
    let json = serde_json::json!({
        "cluster_name": "my-cluster",
        "status": "green"
    });

    let health: EsClusterHealthResponse = serde_json::from_value(json).unwrap();
    assert_eq!(health.status, "green");
}

#[test]
fn test_elasticsearch_cluster_health_yellow() {
    let json = serde_json::json!({
        "cluster_name": "my-cluster",
        "status": "yellow"
    });

    let health: EsClusterHealthResponse = serde_json::from_value(json).unwrap();
    // Yellow is still considered healthy
    assert!(health.status == "green" || health.status == "yellow");
}

#[test]
fn test_elasticsearch_response_parsing_text_field_fallback() {
    let es_json = serde_json::json!({
        "hits": {
            "total": {"value": 1},
            "max_score": 10.0,
            "hits": [
                {
                    "_id": "doc1",
                    "_score": 10.0,
                    "_source": {
                        "text": "This uses the text field instead of content",
                        "title": "Test"
                    }
                }
            ]
        }
    });

    let es_response: EsSearchResponse = serde_json::from_value(es_json).unwrap();
    let results = ElasticsearchAdapter::parse_hits(&es_response);

    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].content,
        "This uses the text field instead of content"
    );
    // text field should be removed from metadata
    let meta = results[0].metadata.as_ref().unwrap();
    assert!(meta.get("text").is_none());
    assert_eq!(meta.get("title").and_then(|v| v.as_str()), Some("Test"));
}

#[test]
fn test_elasticsearch_response_parsing_body_field_fallback() {
    let es_json = serde_json::json!({
        "hits": {
            "total": {"value": 1},
            "max_score": 10.0,
            "hits": [
                {
                    "_id": "doc1",
                    "_score": 10.0,
                    "_source": {
                        "body": "This uses the body field",
                        "category": "test"
                    }
                }
            ]
        }
    });

    let es_response: EsSearchResponse = serde_json::from_value(es_json).unwrap();
    let results = ElasticsearchAdapter::parse_hits(&es_response);

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].content, "This uses the body field");
    let meta = results[0].metadata.as_ref().unwrap();
    assert!(meta.get("body").is_none());
    assert_eq!(meta.get("category").and_then(|v| v.as_str()), Some("test"));
}

#[test]
fn test_elasticsearch_response_parsing_no_score() {
    let es_json = serde_json::json!({
        "hits": {
            "total": {"value": 1},
            "max_score": 10.0,
            "hits": [
                {
                    "_id": "doc1",
                    "_source": {
                        "content": "No score field"
                    }
                }
            ]
        }
    });

    let es_response: EsSearchResponse = serde_json::from_value(es_json).unwrap();
    let results = ElasticsearchAdapter::parse_hits(&es_response);

    assert_eq!(results.len(), 1);
    // No _score field → defaults to 0.0, normalized against max_score 10.0 → 0.0
    assert_eq!(results[0].score, 0.0);
}

#[tokio::test]
async fn test_elasticsearch_discover_query_mode() {
    let adapter =
        ElasticsearchAdapter::simple("http://localhost:9200", "test", Duration::from_secs(30))
            .unwrap();
    let mode = adapter.discover_query_mode().await.unwrap();
    // Elasticsearch always discovers as TextNative
    assert_eq!(mode, QueryMode::TextNative);
}

#[test]
fn test_elasticsearch_response_parsing_no_content_fields() {
    let es_json = serde_json::json!({
        "hits": {
            "total": {"value": 1},
            "max_score": 5.0,
            "hits": [
                {
                    "_id": "doc1",
                    "_score": 5.0,
                    "_source": {
                        "title": "No content/text/body field",
                        "category": "misc"
                    }
                }
            ]
        }
    });

    let es_response: EsSearchResponse = serde_json::from_value(es_json).unwrap();
    let results = ElasticsearchAdapter::parse_hits(&es_response);

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].content, ""); // Falls through all field options
}

// === Async mock-server tests for query() and health_check() ===

/// Start a lightweight axum test server on a random port.
/// Returns the base URL (e.g. "http://127.0.0.1:12345") and the JoinHandle.
async fn start_mock_server(router: axum::Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{}", addr);
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (base_url, handle)
}

/// Build a valid ES _search JSON response body.
fn es_search_response_json(hits: Vec<serde_json::Value>, max_score: f32) -> serde_json::Value {
    serde_json::json!({
        "hits": {
            "total": { "value": hits.len() },
            "max_score": max_score,
            "hits": hits
        }
    })
}

#[tokio::test]
async fn test_elasticsearch_query_success() {
    use axum::{routing::post, Json, Router};

    let body = es_search_response_json(
        vec![
            serde_json::json!({
                "_id": "doc1",
                "_score": 12.5,
                "_source": { "content": "Rust is fast", "title": "Perf" }
            }),
            serde_json::json!({
                "_id": "doc2",
                "_score": 6.0,
                "_source": { "content": "Go is concurrent", "title": "Conc" }
            }),
        ],
        12.5,
    );

    let app = Router::new().route(
        "/test_idx/_search",
        post(move || async move { Json(body.clone()) }),
    );

    let (base_url, _handle) = start_mock_server(app).await;

    let adapter =
        ElasticsearchAdapter::simple(&base_url, "test_idx", Duration::from_secs(5)).unwrap();
    let request = QueryRequest {
        query: "rust".to_string(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };

    let response = adapter.query(&request).await.unwrap();
    assert_eq!(response.results.len(), 2);
    assert_eq!(response.results[0].id, "doc1");
    assert!((response.results[0].score - 1.0).abs() < 0.001);
    assert_eq!(response.results[0].content, "Rust is fast");
    assert_eq!(response.results[1].id, "doc2");
    assert_eq!(response.cache_status, CacheStatus::Miss);
    assert!(response.took_ms < 5000);
}

#[tokio::test]
async fn test_elasticsearch_query_error_status() {
    use axum::{http::StatusCode, routing::post, Router};

    let app = Router::new().route(
        "/test_idx/_search",
        post(|| async { (StatusCode::BAD_REQUEST, "index_not_found_exception") }),
    );

    let (base_url, _handle) = start_mock_server(app).await;

    let adapter =
        ElasticsearchAdapter::simple(&base_url, "test_idx", Duration::from_secs(5)).unwrap();
    let request = QueryRequest {
        query: "test".to_string(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };

    let err = adapter.query(&request).await.unwrap_err();
    match err {
        UpstreamError::Status(code, body) => {
            assert_eq!(code, 400);
            assert!(body.contains("index_not_found"));
        }
        other => panic!("Expected Status error, got: {:?}", other),
    }
}

#[tokio::test]
async fn test_elasticsearch_query_parse_error() {
    use axum::{routing::post, Router};

    let app = Router::new().route(
        "/test_idx/_search",
        post(|| async { "this is not valid json for ES response" }),
    );

    let (base_url, _handle) = start_mock_server(app).await;

    let adapter =
        ElasticsearchAdapter::simple(&base_url, "test_idx", Duration::from_secs(5)).unwrap();
    let request = QueryRequest {
        query: "test".to_string(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };

    let err = adapter.query(&request).await.unwrap_err();
    assert!(matches!(err, UpstreamError::Parse(_)));
}

#[tokio::test]
async fn test_elasticsearch_health_check_green() {
    use axum::{routing::get, Json, Router};

    let app = Router::new().route(
        "/_cluster/health",
        get(|| async {
            Json(serde_json::json!({
                "status": "green",
                "cluster_name": "test-cluster"
            }))
        }),
    );

    let (base_url, _handle) = start_mock_server(app).await;

    let adapter =
        ElasticsearchAdapter::simple(&base_url, "test_idx", Duration::from_secs(5)).unwrap();
    let healthy = adapter.health_check().await.unwrap();
    assert!(healthy);
}

#[tokio::test]
async fn test_elasticsearch_health_check_red() {
    use axum::{routing::get, Json, Router};

    let app = Router::new().route(
        "/_cluster/health",
        get(|| async {
            Json(serde_json::json!({
                "status": "red",
                "cluster_name": "test-cluster"
            }))
        }),
    );

    let (base_url, _handle) = start_mock_server(app).await;

    let adapter =
        ElasticsearchAdapter::simple(&base_url, "test_idx", Duration::from_secs(5)).unwrap();
    let healthy = adapter.health_check().await.unwrap();
    assert!(!healthy);
}

#[tokio::test]
async fn test_elasticsearch_health_check_error_status() {
    use axum::{http::StatusCode, routing::get, Router};

    let app = Router::new().route(
        "/_cluster/health",
        get(|| async { StatusCode::SERVICE_UNAVAILABLE }),
    );

    let (base_url, _handle) = start_mock_server(app).await;

    let adapter =
        ElasticsearchAdapter::simple(&base_url, "test_idx", Duration::from_secs(5)).unwrap();
    let healthy = adapter.health_check().await.unwrap();
    assert!(!healthy);
}
