#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

#[test]
fn test_adapter_creation() {
    let adapter =
        GenericRestAdapter::new("http://localhost:6333", Duration::from_secs(30)).unwrap();
    assert_eq!(adapter.base_url(), "http://localhost:6333");
    assert_eq!(adapter.timeout(), Duration::from_secs(30));
}

#[test]
fn test_adapter_with_trailing_slash() {
    let adapter =
        GenericRestAdapter::new("http://localhost:6333/", Duration::from_secs(30)).unwrap();
    assert_eq!(adapter.base_url(), "http://localhost:6333");
}

#[test]
fn test_adapter_with_custom_paths() {
    let adapter = GenericRestAdapter::with_paths(
        "http://localhost:6333",
        "/api/search",
        "/api/health",
        Duration::from_secs(30),
    )
    .unwrap();
    assert_eq!(adapter.query_path, "/api/search");
    assert_eq!(adapter.health_path, "/api/health");
}

#[test]
fn test_upstream_error_display() {
    let err = UpstreamError::Network("connection refused".to_string());
    assert!(err.to_string().contains("Network error"));

    let err = UpstreamError::Status(500, "Internal Server Error".to_string());
    assert!(err.to_string().contains("500"));

    let err = UpstreamError::Timeout;
    assert!(err.to_string().contains("timed out"));

    let err = UpstreamError::NotConfigured;
    assert!(err.to_string().contains("not configured"));
}

#[test]
fn test_upstream_status_default() {
    assert_eq!(UpstreamStatus::default(), UpstreamStatus::Online);
}

#[test]
fn test_health_tracker_initial_state() {
    let tracker = HealthTracker::new();
    assert_eq!(tracker.status(), UpstreamStatus::Online);
    assert_eq!(tracker.consecutive_failures(), 0);
    assert_eq!(tracker.consecutive_successes(), 0);
}

#[test]
fn test_health_tracker_records_success() {
    let tracker = HealthTracker::new();

    tracker.record_success();
    assert_eq!(tracker.consecutive_successes(), 1);
    assert_eq!(tracker.consecutive_failures(), 0);
    assert_eq!(tracker.status(), UpstreamStatus::Online);

    tracker.record_success();
    assert_eq!(tracker.consecutive_successes(), 2);
}

#[test]
fn test_health_tracker_records_failure() {
    let tracker = HealthTracker::new();

    tracker.record_failure();
    assert_eq!(tracker.consecutive_failures(), 1);
    assert_eq!(tracker.consecutive_successes(), 0);
    assert_eq!(tracker.status(), UpstreamStatus::Online); // Not yet offline
}

#[test]
fn test_health_tracker_goes_offline() {
    let tracker = HealthTracker::new(); // Default threshold is 3

    tracker.record_failure();
    tracker.record_failure();
    assert_eq!(tracker.status(), UpstreamStatus::Online); // 2 failures, not offline yet

    tracker.record_failure();
    assert_eq!(tracker.status(), UpstreamStatus::Offline); // 3 failures = offline
}

#[test]
fn test_health_tracker_recovery() {
    let tracker = HealthTracker::new(); // Default recovery threshold is 2

    // Go offline
    tracker.record_failure();
    tracker.record_failure();
    tracker.record_failure();
    assert_eq!(tracker.status(), UpstreamStatus::Offline);

    // Start recovering
    tracker.record_success();
    assert_eq!(tracker.status(), UpstreamStatus::Offline); // 1 success, not recovered yet

    tracker.record_success();
    assert_eq!(tracker.status(), UpstreamStatus::Online); // 2 successes = recovered
}

#[test]
fn test_health_tracker_success_resets_failures() {
    let tracker = HealthTracker::new();

    tracker.record_failure();
    tracker.record_failure();
    assert_eq!(tracker.consecutive_failures(), 2);

    tracker.record_success();
    assert_eq!(tracker.consecutive_failures(), 0);
    assert_eq!(tracker.consecutive_successes(), 1);
}

#[test]
fn test_health_tracker_failure_resets_successes() {
    let tracker = HealthTracker::new();

    tracker.record_success();
    tracker.record_success();
    assert_eq!(tracker.consecutive_successes(), 2);

    tracker.record_failure();
    assert_eq!(tracker.consecutive_successes(), 0);
    assert_eq!(tracker.consecutive_failures(), 1);
}

#[test]
fn test_health_tracker_custom_thresholds() {
    let tracker = HealthTracker::with_thresholds(
        5,   // offline after 5 failures
        3,   // recover after 3 successes
        0.2, // degraded at 20% error rate
    );

    // 4 failures should still be online
    for _ in 0..4 {
        tracker.record_failure();
    }
    assert_eq!(tracker.status(), UpstreamStatus::Online);

    // 5th failure goes offline
    tracker.record_failure();
    assert_eq!(tracker.status(), UpstreamStatus::Offline);

    // Need 3 successes to recover
    tracker.record_success();
    tracker.record_success();
    assert_eq!(tracker.status(), UpstreamStatus::Offline);

    tracker.record_success();
    assert_eq!(tracker.status(), UpstreamStatus::Online);
}

#[test]
fn test_health_tracker_reset_window() {
    let tracker = HealthTracker::new();

    tracker.record_success();
    tracker.record_failure();

    tracker.reset_window();

    // Consecutive counters should remain, but window counters reset
    // (this is for error rate calculation reset, not for status determination)
    assert_eq!(tracker.status(), UpstreamStatus::Online);
}

#[test]
fn test_health_tracker_time_since_success() {
    let tracker = HealthTracker::new();

    // No success yet
    assert!(tracker.time_since_last_success().is_none());

    // Record success
    tracker.record_success();
    let time = tracker.time_since_last_success();
    assert!(time.is_some());
    assert!(time.unwrap() < Duration::from_secs(1));
}

#[test]
fn test_upstream_error_retryable_network() {
    let condition = RetryCondition::all();
    let err = UpstreamError::Network("connection refused".to_string());
    assert!(err.is_retryable(&condition));

    let no_network = RetryCondition {
        on_network_error: false,
        ..RetryCondition::all()
    };
    assert!(!err.is_retryable(&no_network));
}

#[test]
fn test_upstream_error_retryable_timeout() {
    let condition = RetryCondition::all();
    let err = UpstreamError::Timeout;
    assert!(err.is_retryable(&condition));

    let no_timeout = RetryCondition {
        on_timeout: false,
        ..RetryCondition::all()
    };
    assert!(!err.is_retryable(&no_timeout));
}

#[test]
fn test_upstream_error_retryable_status() {
    let condition = RetryCondition::all();

    // 5xx errors should be retryable
    let err_500 = UpstreamError::Status(500, "Internal Server Error".to_string());
    assert!(err_500.is_retryable(&condition));

    let err_503 = UpstreamError::Status(503, "Service Unavailable".to_string());
    assert!(err_503.is_retryable(&condition));

    // 429 (rate limited) should be retryable
    let err_429 = UpstreamError::Status(429, "Too Many Requests".to_string());
    assert!(err_429.is_retryable(&condition));

    // 4xx errors (except 429) should not be retryable
    let err_400 = UpstreamError::Status(400, "Bad Request".to_string());
    assert!(!err_400.is_retryable(&condition));

    let err_404 = UpstreamError::Status(404, "Not Found".to_string());
    assert!(!err_404.is_retryable(&condition));
}

#[test]
fn test_upstream_error_not_retryable() {
    let condition = RetryCondition::all();

    // Parse errors are never retryable (deterministic)
    let parse_err = UpstreamError::Parse("invalid json".to_string());
    assert!(!parse_err.is_retryable(&condition));

    // NotConfigured is never retryable (config issue)
    let not_configured = UpstreamError::NotConfigured;
    assert!(!not_configured.is_retryable(&condition));
}

#[test]
fn test_upstream_error_unavailable_retryable() {
    let condition = RetryCondition::all();
    let err = UpstreamError::Unavailable("all upstreams down".to_string());
    assert!(err.is_retryable(&condition));

    let no_server_error = RetryCondition {
        on_server_error: false,
        ..RetryCondition::all()
    };
    assert!(!err.is_retryable(&no_server_error));
}

// === QueryMode tests ===

#[test]
fn test_query_mode_supports_text() {
    assert!(QueryMode::TextNative.supports_text());
    assert!(QueryMode::Unknown.supports_text());
    assert!(!QueryMode::VectorOnly.supports_text());
}

#[test]
fn test_query_mode_requires_embedding() {
    assert!(QueryMode::VectorOnly.requires_embedding());
    assert!(!QueryMode::TextNative.requires_embedding());
    assert!(!QueryMode::Unknown.requires_embedding());
}

#[test]
fn test_query_mode_from_u8() {
    assert_eq!(QueryMode::from(0), QueryMode::Unknown);
    assert_eq!(QueryMode::from(1), QueryMode::TextNative);
    assert_eq!(QueryMode::from(2), QueryMode::VectorOnly);
    assert_eq!(QueryMode::from(255), QueryMode::Unknown); // invalid maps to Unknown
}

#[test]
fn test_query_mode_default() {
    assert_eq!(QueryMode::default(), QueryMode::Unknown);
}

// === GenericRestAdapter additional tests ===

#[test]
fn test_adapter_with_query_mode() {
    let adapter = GenericRestAdapter::with_query_mode(
        "http://localhost:6333",
        Duration::from_secs(10),
        QueryMode::TextNative,
    )
    .unwrap();
    assert_eq!(adapter.query_mode(), QueryMode::TextNative);
    assert_eq!(adapter.base_url(), "http://localhost:6333");
}

#[test]
fn test_adapter_set_query_mode() {
    let adapter =
        GenericRestAdapter::new("http://localhost:6333", Duration::from_secs(10)).unwrap();
    assert_eq!(adapter.query_mode(), QueryMode::Unknown);

    adapter.set_query_mode(QueryMode::VectorOnly);
    assert_eq!(adapter.query_mode(), QueryMode::VectorOnly);

    adapter.set_query_mode(QueryMode::TextNative);
    assert_eq!(adapter.query_mode(), QueryMode::TextNative);
}

#[test]
fn test_adapter_metadata() {
    let adapter =
        GenericRestAdapter::new("http://localhost:6333", Duration::from_secs(30)).unwrap();
    let metadata = adapter.metadata();
    assert_eq!(metadata.adapter_type, "generic");
    assert!(metadata.version.is_none());
    assert!(metadata.properties.is_empty());
}

#[test]
fn test_adapter_identifier() {
    let adapter =
        GenericRestAdapter::new("http://localhost:6333", Duration::from_secs(30)).unwrap();
    assert_eq!(
        UpstreamAdapter::identifier(&adapter),
        "http://localhost:6333"
    );
}

// === UpstreamError indicates_text_not_supported ===

#[test]
fn test_indicates_text_not_supported_unsupported_query_type() {
    let err = UpstreamError::UnsupportedQueryType("not supported".to_string());
    assert!(err.indicates_text_not_supported());
}

#[test]
fn test_indicates_text_not_supported_400_with_vector() {
    let err = UpstreamError::Status(400, "must provide a vector".to_string());
    assert!(err.indicates_text_not_supported());
}

#[test]
fn test_indicates_text_not_supported_400_with_embedding() {
    let err = UpstreamError::Status(400, "embedding required".to_string());
    assert!(err.indicates_text_not_supported());
}

#[test]
fn test_indicates_text_not_supported_400_with_dense() {
    let err = UpstreamError::Status(400, "expected dense vector".to_string());
    assert!(err.indicates_text_not_supported());
}

#[test]
fn test_indicates_text_not_supported_400_with_query_must_be() {
    let err = UpstreamError::Status(400, "query must be a vector".to_string());
    assert!(err.indicates_text_not_supported());
}

#[test]
fn test_indicates_text_not_supported_non_400() {
    let err = UpstreamError::Status(500, "must provide a vector".to_string());
    assert!(!err.indicates_text_not_supported());
}

#[test]
fn test_indicates_text_not_supported_400_generic() {
    let err = UpstreamError::Status(400, "bad request".to_string());
    assert!(!err.indicates_text_not_supported());
}

#[test]
fn test_indicates_text_not_supported_other_errors() {
    assert!(!UpstreamError::Network("err".into()).indicates_text_not_supported());
    assert!(!UpstreamError::Timeout.indicates_text_not_supported());
    assert!(!UpstreamError::NotConfigured.indicates_text_not_supported());
    assert!(!UpstreamError::Parse("err".into()).indicates_text_not_supported());
}

// === UpstreamError Display and Retryable for newer variants ===

#[test]
fn test_upstream_error_display_all_variants() {
    assert!(UpstreamError::Unavailable("test".into())
        .to_string()
        .contains("unavailable"));
    assert!(UpstreamError::UnsupportedQueryType("test".into())
        .to_string()
        .contains("not supported"));
    assert!(UpstreamError::EmbeddingRequired("test".into())
        .to_string()
        .contains("not available"));
    assert!(UpstreamError::EmbeddingFailed("test".into())
        .to_string()
        .contains("failed"));
    assert!(UpstreamError::Parse("test".into())
        .to_string()
        .contains("Parse error"));
}

#[test]
fn test_upstream_error_retryable_newer_variants() {
    let all = RetryCondition::all();
    // UnsupportedQueryType is never retryable
    assert!(!UpstreamError::UnsupportedQueryType("test".into()).is_retryable(&all));
    // EmbeddingRequired is never retryable
    assert!(!UpstreamError::EmbeddingRequired("test".into()).is_retryable(&all));
    // EmbeddingFailed follows on_server_error
    assert!(UpstreamError::EmbeddingFailed("test".into()).is_retryable(&all));
    let none = RetryCondition::none();
    assert!(!UpstreamError::EmbeddingFailed("test".into()).is_retryable(&none));
}

// === HealthTracker degraded status ===

#[test]
fn test_health_tracker_degraded_status() {
    let tracker = HealthTracker::with_thresholds(5, 2, 0.1);

    // Generate enough samples for degraded check (need >= 10 total, with > 10% errors)
    // Record 8 successes and 2 failures (20% error rate, > 10% threshold)
    for _ in 0..8 {
        tracker.record_success();
    }
    for _ in 0..2 {
        tracker.record_failure();
    }
    // After 2 consecutive failures, consecutive_failures > 0, so degraded check is skipped.
    // Need to record a success to clear consecutive_failures, keeping error rate high.
    tracker.record_success();
    // Now: total=11, failed=2, consecutive_failures=0
    // error_rate = 2/11 = 0.18 > 0.1 = degraded
    assert_eq!(tracker.status(), UpstreamStatus::Degraded);
}

#[test]
fn test_health_tracker_default_impl() {
    let tracker = HealthTracker::default();
    assert_eq!(tracker.status(), UpstreamStatus::Online);
    assert_eq!(tracker.consecutive_failures(), 0);
}

// === Default trait method coverage ===

/// Minimal adapter to exercise default trait methods on UpstreamAdapter.
struct MinimalAdapter;

#[async_trait::async_trait]
impl UpstreamAdapter for MinimalAdapter {
    async fn query(&self, _request: &QueryRequest) -> Result<QueryResponse, UpstreamError> {
        unimplemented!()
    }
    async fn health_check(&self) -> Result<bool, UpstreamError> {
        unimplemented!()
    }
    fn identifier(&self) -> &str {
        "minimal"
    }
    fn timeout(&self) -> Duration {
        Duration::from_secs(1)
    }
}

#[test]
fn test_adapter_default_metadata() {
    let adapter = MinimalAdapter;
    let meta = adapter.metadata();
    assert_eq!(meta.adapter_type, "");
    assert!(meta.version.is_none());
    assert!(meta.properties.is_empty());
}

#[test]
fn test_adapter_default_query_mode() {
    let adapter = MinimalAdapter;
    assert_eq!(adapter.query_mode(), QueryMode::Unknown);
}

#[test]
fn test_adapter_default_set_query_mode_noop() {
    let adapter = MinimalAdapter;
    // Default set_query_mode is a no-op
    adapter.set_query_mode(QueryMode::TextNative);
    // Still returns Unknown because set was a no-op
    assert_eq!(adapter.query_mode(), QueryMode::Unknown);
}

#[test]
fn test_adapter_default_query_vector_unsupported() {
    let adapter = MinimalAdapter;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let request = QueryRequest {
        query: "test".to_string(),
        top_k: None,
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };
    let result = rt.block_on(adapter.query_vector(&request, &[0.1, 0.2]));
    assert!(result.is_err());
    match result.unwrap_err() {
        UpstreamError::UnsupportedQueryType(msg) => {
            assert!(msg.contains("not implemented"));
        }
        other => panic!("Expected UnsupportedQueryType, got: {:?}", other),
    }
}

#[test]
fn test_adapter_default_discover_query_mode() {
    let adapter = MinimalAdapter;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mode = rt.block_on(adapter.discover_query_mode()).unwrap();
    assert_eq!(mode, QueryMode::Unknown);
}

#[test]
fn test_generic_adapter_metadata() {
    let adapter =
        GenericRestAdapter::new("http://localhost:6333", Duration::from_secs(30)).unwrap();
    let meta = adapter.metadata();
    assert_eq!(meta.adapter_type, "generic");
    assert!(meta.version.is_none());
}
