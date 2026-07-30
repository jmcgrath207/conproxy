#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

#[test]
fn test_cascade_config_default() {
    let config = CascadeConfig::default();
    assert!(config.enabled);
    assert!((config.min_score_threshold - 0.7).abs() < 0.001);
    assert_eq!(config.min_results, 1);
    assert_eq!(config.max_cascade_depth, 3);
    assert!(!config.merge_cascade_results);
    assert_eq!(config.cascade_timeout_ms, 30000);
}

#[test]
fn test_cascade_config_builder() {
    let config = CascadeConfig::new()
        .with_threshold(0.8)
        .with_min_results(5)
        .with_max_depth(5)
        .with_merge(true)
        .with_timeout(Duration::from_secs(60));

    assert!((config.min_score_threshold - 0.8).abs() < 0.001);
    assert_eq!(config.min_results, 5);
    assert_eq!(config.max_cascade_depth, 5);
    assert!(config.merge_cascade_results);
    assert_eq!(config.cascade_timeout_ms, 60000);
}

#[test]
fn test_cascade_config_disabled() {
    let config = CascadeConfig::disabled();
    assert!(!config.enabled);
}

#[test]
fn test_cascade_stop_reason_as_str() {
    assert_eq!(CascadeStopReason::ThresholdMet.as_str(), "threshold_met");
    assert_eq!(CascadeStopReason::MinResultsMet.as_str(), "min_results_met");
    assert_eq!(
        CascadeStopReason::MaxDepthReached.as_str(),
        "max_depth_reached"
    );
    assert_eq!(CascadeStopReason::AllExhausted.as_str(), "all_exhausted");
    assert_eq!(CascadeStopReason::Timeout.as_str(), "timeout");
    assert_eq!(CascadeStopReason::Disabled.as_str(), "disabled");
    assert_eq!(CascadeStopReason::NoUpstreams.as_str(), "no_upstreams");
}

#[test]
fn test_cascade_result_is_success() {
    let mut result = CascadeResult {
        results: Vec::new(),
        upstreams_tried: Vec::new(),
        final_upstream: None,
        stop_reason: CascadeStopReason::ThresholdMet,
        upstream_scores: Vec::new(),
        cascade_time_ms: 0,
        cascade_depth: 0,
    };

    assert!(result.is_success());

    result.stop_reason = CascadeStopReason::MinResultsMet;
    assert!(result.is_success());

    result.stop_reason = CascadeStopReason::MaxDepthReached;
    assert!(!result.is_success());

    result.stop_reason = CascadeStopReason::AllExhausted;
    assert!(!result.is_success());
}

#[test]
fn test_cascade_result_max_score() {
    let result = CascadeResult {
        results: vec![
            SearchResult {
                id: "1".to_string(),
                content: "a".to_string(),
                score: 0.8,
                metadata: None,
                upstream_id: None,
            },
            SearchResult {
                id: "2".to_string(),
                content: "b".to_string(),
                score: 0.95,
                metadata: None,
                upstream_id: None,
            },
            SearchResult {
                id: "3".to_string(),
                content: "c".to_string(),
                score: 0.7,
                metadata: None,
                upstream_id: None,
            },
        ],
        upstreams_tried: Vec::new(),
        final_upstream: None,
        stop_reason: CascadeStopReason::ThresholdMet,
        upstream_scores: Vec::new(),
        cascade_time_ms: 0,
        cascade_depth: 0,
    };

    assert!((result.max_score().unwrap() - 0.95).abs() < 0.001);
}

#[test]
fn test_upstream_cascade_config() {
    let config = UpstreamCascadeConfig::new(1).with_threshold(0.85).skip();

    assert_eq!(config.cascade_priority, 1);
    assert!((config.min_score_threshold.unwrap() - 0.85).abs() < 0.001);
    assert!(config.skip_in_cascade);
}

#[test]
fn test_cascade_error_display() {
    let err = CascadeError::NoUpstreamsAvailable;
    assert_eq!(err.to_string(), "No upstreams available");

    let err = CascadeError::AllUpstreamsFailed(vec!["a".to_string(), "b".to_string()]);
    assert_eq!(err.to_string(), "All upstreams failed: a, b");

    let err = CascadeError::Timeout;
    assert_eq!(err.to_string(), "Cascade timeout reached");

    let err = CascadeError::Disabled;
    assert_eq!(err.to_string(), "Cascade is disabled");
}

// === Score Normalization Tests (BM25 vs Cosine) ===

#[test]
fn test_upstream_type_score_range_fts() {
    // FTS/BM25 scores can exceed 1.0 (typical range 0-100+)
    let (min, max) = UpstreamType::FullTextSearch.score_range();
    assert_eq!(min, 0.0);
    assert!(max > 1.0); // BM25 scores are unbounded, typically up to 100
}

#[test]
fn test_upstream_type_score_range_vector_db() {
    // VectorDB cosine similarity is always 0-1
    let (min, max) = UpstreamType::VectorDatabase.score_range();
    assert_eq!(min, 0.0);
    assert_eq!(max, 1.0);
}

#[test]
fn test_upstream_type_score_range_hybrid() {
    // Hybrid can have either score type, defaults to 0-1
    let (min, max) = UpstreamType::Hybrid.score_range();
    assert_eq!(min, 0.0);
    assert!(max >= 1.0);
}

#[test]
fn test_upstream_type_score_range_unknown() {
    // Unknown defaults to 0-1 (normalized)
    let (min, max) = UpstreamType::Unknown.score_range();
    assert_eq!(min, 0.0);
    assert_eq!(max, 1.0);
}

#[test]
fn test_score_normalization_vector_db() {
    // VectorDB cosine scores are already 0-1, should pass through
    let executor = CascadeExecutor::new(Arc::new(UpstreamPool::empty()), CascadeConfig::default());

    // Cosine similarity of 0.85 stays 0.85
    let normalized = executor.normalize_score(0.85, UpstreamType::VectorDatabase);
    assert!((normalized - 0.85).abs() < 0.001);

    // Edge cases
    let normalized = executor.normalize_score(0.0, UpstreamType::VectorDatabase);
    assert!((normalized - 0.0).abs() < 0.001);

    let normalized = executor.normalize_score(1.0, UpstreamType::VectorDatabase);
    assert!((normalized - 1.0).abs() < 0.001);
}

#[test]
fn test_score_normalization_fts_bm25() {
    // FTS BM25 scores need normalization (e.g., 0-100 -> 0-1)
    let executor = CascadeExecutor::new(Arc::new(UpstreamPool::empty()), CascadeConfig::default());

    // BM25 score of 50 (out of 100) should normalize to ~0.5
    let normalized = executor.normalize_score(50.0, UpstreamType::FullTextSearch);
    assert!((normalized - 0.5).abs() < 0.01);

    // BM25 score of 0 normalizes to 0
    let normalized = executor.normalize_score(0.0, UpstreamType::FullTextSearch);
    assert!((normalized - 0.0).abs() < 0.001);

    // BM25 score at max normalizes to 1.0
    let (_, max) = UpstreamType::FullTextSearch.score_range();
    let normalized = executor.normalize_score(max, UpstreamType::FullTextSearch);
    assert!((normalized - 1.0).abs() < 0.001);
}

#[test]
fn test_score_normalization_clamps() {
    // Scores outside expected range should be clamped to 0-1
    let executor = CascadeExecutor::new(Arc::new(UpstreamPool::empty()), CascadeConfig::default());

    // Score above max should clamp to 1.0
    let normalized = executor.normalize_score(200.0, UpstreamType::FullTextSearch);
    assert!((normalized - 1.0).abs() < 0.001);

    // Negative score should clamp to 0.0
    let normalized = executor.normalize_score(-0.5, UpstreamType::VectorDatabase);
    assert!((normalized - 0.0).abs() < 0.001);
}

#[test]
fn test_score_normalization_hybrid() {
    // Hybrid upstreams use the same normalization as FTS (conservative)
    let executor = CascadeExecutor::new(Arc::new(UpstreamPool::empty()), CascadeConfig::default());

    let normalized = executor.normalize_score(0.9, UpstreamType::Hybrid);
    // Should be valid normalized score
    assert!((0.0..=1.0).contains(&normalized));
}

#[test]
fn test_score_normalization_unknown() {
    // Unknown type assumes 0-1 range (pass-through with clamping)
    let executor = CascadeExecutor::new(Arc::new(UpstreamPool::empty()), CascadeConfig::default());

    let normalized = executor.normalize_score(0.75, UpstreamType::Unknown);
    assert!((normalized - 0.75).abs() < 0.001);
}

// === Upstream Type Detection Tests ===

#[test]
fn test_upstream_type_is_fts() {
    assert!(UpstreamType::FullTextSearch.is_fts());
    assert!(UpstreamType::Hybrid.is_fts());
    assert!(!UpstreamType::VectorDatabase.is_fts());
    assert!(!UpstreamType::Unknown.is_fts());
}

#[test]
fn test_upstream_type_is_vector_db() {
    assert!(UpstreamType::VectorDatabase.is_vector_db());
    assert!(UpstreamType::Hybrid.is_vector_db());
    assert!(!UpstreamType::FullTextSearch.is_vector_db());
    assert!(!UpstreamType::Unknown.is_vector_db());
}

#[test]
fn test_upstream_type_is_known() {
    assert!(UpstreamType::FullTextSearch.is_known());
    assert!(UpstreamType::VectorDatabase.is_known());
    assert!(UpstreamType::Hybrid.is_known());
    assert!(!UpstreamType::Unknown.is_known());
}

#[test]
fn test_upstream_type_as_str() {
    assert_eq!(UpstreamType::FullTextSearch.as_str(), "fts");
    assert_eq!(UpstreamType::VectorDatabase.as_str(), "vector_db");
    assert_eq!(UpstreamType::Hybrid.as_str(), "hybrid");
    assert_eq!(UpstreamType::Unknown.as_str(), "unknown");
}

// === Additional cascade tests for coverage ===

#[test]
fn test_cascade_result_max_score_empty() {
    let result = CascadeResult {
        results: Vec::new(),
        upstreams_tried: Vec::new(),
        final_upstream: None,
        stop_reason: CascadeStopReason::AllExhausted,
        upstream_scores: Vec::new(),
        cascade_time_ms: 0,
        cascade_depth: 0,
    };
    assert!(result.max_score().is_none());
}

#[test]
fn test_cascade_result_result_count() {
    let result = CascadeResult {
        results: vec![
            SearchResult {
                id: "1".to_string(),
                content: "a".to_string(),
                score: 0.9,
                metadata: None,
                upstream_id: None,
            },
            SearchResult {
                id: "2".to_string(),
                content: "b".to_string(),
                score: 0.8,
                metadata: None,
                upstream_id: None,
            },
        ],
        upstreams_tried: Vec::new(),
        final_upstream: None,
        stop_reason: CascadeStopReason::ThresholdMet,
        upstream_scores: Vec::new(),
        cascade_time_ms: 0,
        cascade_depth: 0,
    };
    assert_eq!(result.result_count(), 2);
}

#[test]
fn test_cascade_config_timeout_method() {
    let config = CascadeConfig::new().with_timeout(Duration::from_secs(45));
    assert_eq!(config.timeout(), Duration::from_secs(45));
}

#[test]
fn test_normalize_score_max_equals_min() {
    let executor = CascadeExecutor::new(Arc::new(UpstreamPool::empty()), CascadeConfig::default());
    // When max <= min (degenerate range), should clamp to 0-1
    // UpstreamType::VectorDatabase has range (0.0, 1.0) — normal
    // But if we could somehow get max<=min... The code checks upstream_type.score_range()
    // which returns fixed values. So we test the Unknown type which returns (0.0, 1.0).
    // The max<=min branch is hit when score_range returns (x, x) or (x, y) where y <= x.
    // None of the current UpstreamType variants produce that. But we test the clamping:
    let normalized = executor.normalize_score(1.5, UpstreamType::Unknown);
    assert_eq!(normalized, 1.0); // Clamped
    let normalized = executor.normalize_score(-0.5, UpstreamType::Unknown);
    assert_eq!(normalized, 0.0); // Clamped
}

#[test]
fn test_cascade_config_serialization() {
    let config = CascadeConfig::default();
    let json = serde_json::to_value(&config).unwrap();
    assert_eq!(json["enabled"], true);
    assert!((json["min_score_threshold"].as_f64().unwrap() - 0.7).abs() < 0.001);
    assert_eq!(json["max_cascade_depth"], 3);
}

#[test]
fn test_upstream_cascade_config_default() {
    let config = UpstreamCascadeConfig::default();
    assert!(config.min_score_threshold.is_none());
    assert_eq!(config.cascade_priority, 0);
    assert!(!config.skip_in_cascade);
}

#[test]
fn test_cascade_stop_reason_serialization() {
    let json = serde_json::to_value(CascadeStopReason::ThresholdMet).unwrap();
    assert_eq!(json, "ThresholdMet");
}

// === CascadeExecutor unit tests ===

fn make_pool_config(id: &str, url: &str, priority: u32) -> crate::config::UpstreamEndpointConfig {
    crate::config::UpstreamEndpointConfig {
        id: id.to_string(),
        url: url.to_string(),
        timeout_secs: Some(5),
        weight: Some(1),
        priority: Some(priority),
        max_concurrent: None,
        enabled: Some(true),
        version_endpoint: None,
        version_poll_interval_secs: None,
        upstream_type: None,
        query_mode: None,
        table: None,
        embedding_column: None,
        content_column: None,
        metadata_columns: vec![],
        distance_metric: None,
        dimensions: None,
        index: None,
        search_fields: vec![],
        return_fields: vec![],
        api_key: None,
    }
}

#[test]
fn test_cascade_executor_creation() {
    let configs = vec![make_pool_config("a", "http://a.example.com", 0)];
    let pool = Arc::new(
        crate::proxy::pool::UpstreamPool::new(
            &configs,
            crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
        )
        .unwrap(),
    );
    let config = CascadeConfig::default();
    let executor = CascadeExecutor::new(pool, config);
    assert!(executor.config().enabled);
}

#[test]
fn test_cascade_executor_with_metrics() {
    let configs = vec![make_pool_config("a", "http://a.example.com", 0)];
    let pool = Arc::new(
        crate::proxy::pool::UpstreamPool::new(
            &configs,
            crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
        )
        .unwrap(),
    );
    let metrics = Arc::new(crate::proxy::metrics::ProxyMetrics::new());
    let config = CascadeConfig::default();
    let executor = CascadeExecutor::with_metrics(pool, config, metrics);
    assert!(executor.config().enabled);
}

#[test]
fn test_cascade_order() {
    let configs = vec![
        make_pool_config("low", "http://low.example.com", 0),
        make_pool_config("high", "http://high.example.com", 10),
        make_pool_config("mid", "http://mid.example.com", 5),
    ];
    let pool = Arc::new(
        crate::proxy::pool::UpstreamPool::new(
            &configs,
            crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
        )
        .unwrap(),
    );
    let executor = CascadeExecutor::new(pool, CascadeConfig::default());
    let order = executor.cascade_order();
    assert_eq!(order.len(), 3);
    assert_eq!(order[0].id, "low");
    assert_eq!(order[1].id, "mid");
    assert_eq!(order[2].id, "high");
}

#[test]
fn test_cascade_order_with_preference_none() {
    let configs = vec![
        make_pool_config("a", "http://a.example.com", 0),
        make_pool_config("b", "http://b.example.com", 1),
    ];
    let pool = Arc::new(
        crate::proxy::pool::UpstreamPool::new(
            &configs,
            crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
        )
        .unwrap(),
    );
    let executor = CascadeExecutor::new(pool, CascadeConfig::default());
    // No preference → same as cascade_order
    let order = executor.cascade_order_with_preference(None);
    assert_eq!(order[0].id, "a");
    assert_eq!(order[1].id, "b");
}

#[test]
fn test_cascade_order_with_preference_type() {
    let mut es_config = make_pool_config("es-fts", "http://es.example.com", 10);
    es_config.upstream_type = Some("elasticsearch".to_string());
    let qdrant_config = make_pool_config("qdrant-vec", "http://qdrant.example.com", 0);

    let configs = vec![qdrant_config, es_config];
    let pool = Arc::new(
        crate::proxy::pool::UpstreamPool::new(
            &configs,
            crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
        )
        .unwrap(),
    );
    let executor = CascadeExecutor::new(pool, CascadeConfig::default());

    // With fts preference, ES should come first despite higher priority number
    let order = executor.cascade_order_with_preference(Some("fts"));
    assert_eq!(order[0].id, "es-fts");
}

#[test]
fn test_cascade_normalize_score_vector_db() {
    let configs = vec![make_pool_config("a", "http://a.example.com", 0)];
    let pool = Arc::new(
        crate::proxy::pool::UpstreamPool::new(
            &configs,
            crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
        )
        .unwrap(),
    );
    let executor = CascadeExecutor::new(pool, CascadeConfig::default());

    // VectorDB scores are already 0-1, so normalization should roughly preserve
    let normalized = executor.normalize_score(0.95, UpstreamType::VectorDatabase);
    assert!(normalized > 0.9 && normalized <= 1.0);
}

#[test]
fn test_cascade_normalize_score_fts() {
    let configs = vec![make_pool_config("a", "http://a.example.com", 0)];
    let pool = Arc::new(
        crate::proxy::pool::UpstreamPool::new(
            &configs,
            crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
        )
        .unwrap(),
    );
    let executor = CascadeExecutor::new(pool, CascadeConfig::default());

    // FTS scores can be > 1.0 (BM25), normalization should bring to 0-1
    let normalized = executor.normalize_score(50.0, UpstreamType::FullTextSearch);
    assert!(normalized >= 0.0 && normalized <= 1.0);
}

#[test]
fn test_cascade_normalize_score_unknown_type() {
    let configs = vec![make_pool_config("a", "http://a.example.com", 0)];
    let pool = Arc::new(
        crate::proxy::pool::UpstreamPool::new(
            &configs,
            crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
        )
        .unwrap(),
    );
    let executor = CascadeExecutor::new(pool, CascadeConfig::default());

    // Unknown type has score_range (0, 1) so max <= min check kicks in
    let normalized = executor.normalize_score(0.5, UpstreamType::Unknown);
    assert!(normalized >= 0.0 && normalized <= 1.0);
}

// === CascadeError ===

#[test]
fn test_cascade_error_display_no_upstreams() {
    let err = CascadeError::NoUpstreamsAvailable;
    assert_eq!(err.to_string(), "No upstreams available");
}

#[test]
fn test_cascade_error_display_all_failed() {
    let err = CascadeError::AllUpstreamsFailed(vec!["a".to_string(), "b".to_string()]);
    assert_eq!(err.to_string(), "All upstreams failed: a, b");
}

#[test]
fn test_cascade_error_display_timeout() {
    assert_eq!(CascadeError::Timeout.to_string(), "Cascade timeout reached");
}

#[test]
fn test_cascade_error_display_disabled() {
    assert_eq!(CascadeError::Disabled.to_string(), "Cascade is disabled");
}

#[test]
fn test_cascade_error_is_std_error() {
    let err: Box<dyn std::error::Error> = Box::new(CascadeError::Timeout);
    assert!(err.to_string().contains("timeout"));
}

// === CascadeConfig builder ===

#[test]
fn test_cascade_config_with_timeout() {
    let config = CascadeConfig::new().with_timeout(Duration::from_secs(10));
    assert_eq!(config.timeout(), Duration::from_secs(10));
}

#[test]
fn test_cascade_config_with_merge() {
    let config = CascadeConfig::new().with_merge(true);
    assert!(config.merge_cascade_results);
}

// === UpstreamScore serialization ===

#[test]
fn test_upstream_score_serialization() {
    let score = UpstreamScore {
        upstream_id: "qdrant-1".to_string(),
        upstream_type: UpstreamType::VectorDatabase,
        query_mode: QueryMode::TextNative,
        max_score: 0.95,
        normalized_score: 0.95,
        result_count: 5,
        latency_ms: 42,
        met_threshold: true,
        error: None,
    };
    let json = serde_json::to_string(&score).unwrap();
    assert!(json.contains("qdrant-1"));
    assert!(json.contains("0.95"));
    assert!(!json.contains("error")); // None skipped
}

#[test]
fn test_upstream_score_with_error() {
    let score = UpstreamScore {
        upstream_id: "es-1".to_string(),
        upstream_type: UpstreamType::FullTextSearch,
        query_mode: QueryMode::Unknown,
        max_score: 0.0,
        normalized_score: 0.0,
        result_count: 0,
        latency_ms: 5000,
        met_threshold: false,
        error: Some("connection refused".to_string()),
    };
    let json = serde_json::to_string(&score).unwrap();
    assert!(json.contains("connection refused"));
}

// === Async cascade query tests ===

/// Start a mock upstream that returns QueryResponse-format search results with the given score.
async fn start_mock_upstream(score: f32) -> (tokio::task::JoinHandle<()>, String) {
    let app = axum::Router::new().fallback(move || async move {
        axum::Json(serde_json::json!({
            "results": [
                {
                    "id": "doc-1",
                    "score": score,
                    "content": "Test result"
                }
            ],
            "cache_status": "Miss",
            "took_ms": 1
        }))
    });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{}", addr);

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    tokio::time::sleep(Duration::from_millis(20)).await;
    (handle, url)
}

/// Start a mock upstream that returns 500 errors.
async fn start_failing_upstream() -> (tokio::task::JoinHandle<()>, String) {
    let app =
        axum::Router::new().fallback(|| async { axum::http::StatusCode::INTERNAL_SERVER_ERROR });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{}", addr);

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    tokio::time::sleep(Duration::from_millis(20)).await;
    (handle, url)
}

#[tokio::test]
async fn test_cascade_query_disabled() {
    let pool = Arc::new(crate::proxy::pool::UpstreamPool::empty());
    let config = CascadeConfig::disabled();
    let executor = CascadeExecutor::new(pool, config);

    let request = crate::proxy::types::QueryRequest {
        query: "test".to_string(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };

    let result = executor.query(&request).await;
    assert_eq!(result.stop_reason, CascadeStopReason::Disabled);
    assert!(result.results.is_empty());
}

#[tokio::test]
async fn test_cascade_query_no_upstreams() {
    let pool = Arc::new(crate::proxy::pool::UpstreamPool::empty());
    let config = CascadeConfig::default();
    let executor = CascadeExecutor::new(pool, config);

    let request = crate::proxy::types::QueryRequest {
        query: "test".to_string(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };

    let result = executor.query(&request).await;
    assert_eq!(result.stop_reason, CascadeStopReason::NoUpstreams);
}

#[tokio::test]
async fn test_cascade_query_threshold_met() {
    let (_handle, url) = start_mock_upstream(0.95).await;

    let configs = vec![make_pool_config("mock", &url, 0)];
    let pool = Arc::new(
        crate::proxy::pool::UpstreamPool::new(
            &configs,
            crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
        )
        .unwrap(),
    );
    let config = CascadeConfig::new().with_threshold(0.5);
    let executor = CascadeExecutor::new(pool, config);

    let request = crate::proxy::types::QueryRequest {
        query: "test".to_string(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };

    let result = executor.query(&request).await;
    assert_eq!(result.stop_reason, CascadeStopReason::ThresholdMet);
    assert!(!result.results.is_empty());
    assert_eq!(result.upstreams_tried, vec!["mock"]);
}

#[tokio::test]
async fn test_cascade_query_all_exhausted() {
    let (_handle, url) = start_failing_upstream().await;

    let configs = vec![make_pool_config("failing", &url, 0)];
    let pool = Arc::new(
        crate::proxy::pool::UpstreamPool::new(
            &configs,
            crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
        )
        .unwrap(),
    );
    let config = CascadeConfig::default();
    let executor = CascadeExecutor::new(pool, config);

    let request = crate::proxy::types::QueryRequest {
        query: "test".to_string(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };

    let result = executor.query(&request).await;
    assert_eq!(result.stop_reason, CascadeStopReason::AllExhausted);
    assert!(result.results.is_empty());
}

#[tokio::test]
async fn test_cascade_query_max_depth_reached() {
    let (_handle1, url1) = start_mock_upstream(0.1).await;
    let (_handle2, url2) = start_mock_upstream(0.1).await;
    let (_handle3, url3) = start_mock_upstream(0.1).await;

    let configs = vec![
        make_pool_config("a", &url1, 0),
        make_pool_config("b", &url2, 1),
        make_pool_config("c", &url3, 2),
    ];
    let pool = Arc::new(
        crate::proxy::pool::UpstreamPool::new(
            &configs,
            crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
        )
        .unwrap(),
    );
    // Threshold very high so nothing meets it; max_depth = 1
    let config = CascadeConfig::new().with_threshold(0.99).with_max_depth(1);
    let executor = CascadeExecutor::new(pool, config);

    let request = crate::proxy::types::QueryRequest {
        query: "test".to_string(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };

    let result = executor.query(&request).await;
    assert_eq!(result.stop_reason, CascadeStopReason::MaxDepthReached);
    assert_eq!(result.cascade_depth, 1);
}

#[tokio::test]
async fn test_cascade_query_targeted_upstream() {
    let (_handle, url) = start_mock_upstream(0.9).await;

    let configs = vec![make_pool_config("target", &url, 0)];
    let pool = Arc::new(
        crate::proxy::pool::UpstreamPool::new(
            &configs,
            crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
        )
        .unwrap(),
    );
    let config = CascadeConfig::default();
    let executor = CascadeExecutor::new(pool, config);

    let request = crate::proxy::types::QueryRequest {
        query: "test".to_string(),
        top_k: Some(5),
        priority: None,
        upstream_id: Some("target".to_string()),
        upstream_type: None,
    };

    let result = executor.query(&request).await;
    // Targeted upstream should be queried and return results
    assert!(!result.results.is_empty());
    assert_eq!(result.upstreams_tried, vec!["target"]);
}

#[tokio::test]
async fn test_cascade_query_targeted_fallback() {
    let (_handle1, url1) = start_failing_upstream().await;
    let (_handle2, url2) = start_mock_upstream(0.9).await;

    let configs = vec![
        make_pool_config("failing-target", &url1, 0),
        make_pool_config("fallback", &url2, 1),
    ];
    let pool = Arc::new(
        crate::proxy::pool::UpstreamPool::new(
            &configs,
            crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
        )
        .unwrap(),
    );
    let config = CascadeConfig::new().with_threshold(0.5);
    let executor = CascadeExecutor::new(pool, config);

    let request = crate::proxy::types::QueryRequest {
        query: "test".to_string(),
        top_k: Some(5),
        priority: None,
        upstream_id: Some("failing-target".to_string()),
        upstream_type: None,
    };

    let result = executor.query(&request).await;
    // Targeted upstream failed, should cascade to fallback
    assert!(!result.results.is_empty());
}

// === fuse_rrf tests ===

fn make_result(id: &str, content: &str, score: f32) -> SearchResult {
    SearchResult {
        id: id.to_string(),
        score,
        content: content.to_string(),
        metadata: None,
        upstream_id: None,
    }
}

#[test]
fn test_fuse_rrf_empty() {
    let fused = fuse_rrf(vec![], 60, 100);
    assert!(fused.is_empty());
}

#[test]
fn test_fuse_rrf_single_list() {
    let lists = vec![(
        "u1".to_string(),
        vec![
            make_result("a", "content a", 0.9),
            make_result("b", "content b", 0.5),
        ],
    )];
    let fused = fuse_rrf(lists, 60, 100);
    assert_eq!(fused.len(), 2);
    // Higher-ranked result comes first
    assert_eq!(fused[0].id, "a");
    assert_eq!(fused[1].id, "b");
}

#[test]
fn test_fuse_rrf_dedup_by_content() {
    // Same content from two upstreams should merge into one entry
    let lists = vec![
        (
            "u1".to_string(),
            vec![make_result("a-from-u1", "shared content", 0.8)],
        ),
        (
            "u2".to_string(),
            vec![make_result("a-from-u2", "shared content", 0.9)],
        ),
    ];
    let fused = fuse_rrf(lists, 60, 100);
    assert_eq!(fused.len(), 1, "duplicate content should merge");
    // Higher-scoring instance wins as representative
    assert_eq!(fused[0].id, "a-from-u2");
}

#[test]
fn test_fuse_rrf_ranks_higher_for_appearing_in_multiple_lists() {
    // "shared" appears in both lists; "only1" and "only2" appear in one each.
    // "shared" should rank highest.
    let lists = vec![
        (
            "u1".to_string(),
            vec![
                make_result("only1", "content only1", 0.9),
                make_result("shared", "content shared", 0.5),
            ],
        ),
        (
            "u2".to_string(),
            vec![
                make_result("only2", "content only2", 0.9),
                make_result("shared", "content shared", 0.5),
            ],
        ),
    ];
    let fused = fuse_rrf(lists, 60, 100);
    assert_eq!(fused.len(), 3);
    assert_eq!(fused[0].id, "shared", "content in 2 lists ranks highest");
}

#[test]
fn test_fuse_rrf_truncates_to_max_results() {
    let lists = vec![(
        "u1".to_string(),
        vec![
            make_result("a", "ca", 0.9),
            make_result("b", "cb", 0.8),
            make_result("c", "cc", 0.7),
        ],
    )];
    let fused = fuse_rrf(lists, 60, 2);
    assert_eq!(fused.len(), 2);
    assert_eq!(fused[0].id, "a");
    assert_eq!(fused[1].id, "b");
}

#[test]
fn test_fuse_rrf_k_affects_scores() {
    // Different k values should produce different score distributions,
    // but rank order should remain the same for the same input.
    let lists = vec![(
        "u1".to_string(),
        vec![make_result("a", "ca", 0.9), make_result("b", "cb", 0.5)],
    )];
    let fused_low_k = fuse_rrf(lists.clone(), 1, 100);
    let fused_high_k = fuse_rrf(lists, 1000, 100);
    assert_eq!(fused_low_k[0].id, fused_high_k[0].id);
    assert_eq!(fused_low_k[1].id, fused_high_k[1].id);
}

// === FusionMethod config tests ===

#[test]
fn test_fusion_method_default() {
    let config = CascadeConfig::default();
    assert_eq!(config.fusion_method, FusionMethod::None);
    assert_eq!(config.rrf_k, 60);
}

#[test]
fn test_fusion_method_rrf_serde() {
    let toml = r#"
        enabled = true
        fusion_method = "rrf"
        rrf_k = 42
    "#;
    let config: CascadeConfig = toml::from_str(toml).unwrap();
    assert_eq!(config.fusion_method, FusionMethod::Rrf);
    assert_eq!(config.rrf_k, 42);
}

#[test]
fn test_fusion_method_with_builder() {
    let config = CascadeConfig::new()
        .with_fusion(FusionMethod::Rrf)
        .with_rrf_k(45);
    assert_eq!(config.fusion_method, FusionMethod::Rrf);
    assert_eq!(config.rrf_k, 45);
}

#[test]
fn test_fusion_method_none_serde() {
    let toml = r#"
        enabled = true
        fusion_method = "none"
    "#;
    let config: CascadeConfig = toml::from_str(toml).unwrap();
    assert_eq!(config.fusion_method, FusionMethod::None);
    assert_eq!(config.rrf_k, 60);
}
