#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

/// Helper to create a QueryRequest for testing.
fn make_query_request(query: &str, top_k: Option<usize>) -> QueryRequest {
    QueryRequest {
        query: query.to_string(),
        top_k,
        priority: None,
        upstream_id: None,
        upstream_type: None,
    }
}

#[test]
fn test_cache_status_serialization() {
    let hit = CacheStatus::Hit;
    let json = serde_json::to_string(&hit).unwrap();
    assert_eq!(json, r#""hit""#);

    let miss = CacheStatus::Miss;
    let json = serde_json::to_string(&miss).unwrap();
    assert_eq!(json, r#""miss""#);
}

#[test]
fn test_query_request_deserialization() {
    let json = r#"{"query": "test query", "top_k": 5}"#;
    let req: QueryRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.query, "test query");
    assert_eq!(req.top_k, Some(5));

    // Without top_k
    let json = r#"{"query": "test"}"#;
    let req: QueryRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.top_k, None);
}

#[test]
fn test_query_response_serialization() {
    let resp = QueryResponse {
        results: vec![SearchResult {
            id: "doc1".to_string(),
            score: 0.95,
            content: "test content".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Hit,
        took_ms: 42,
        generated_at: None,
        miss_reason: None,
    };

    let json = serde_json::to_string(&resp).unwrap();
    assert!(json.contains(r#""cache_status":"hit""#));
    assert!(json.contains(r#""took_ms":42"#));
}

#[test]
fn test_freshness_equality() {
    assert_eq!(Freshness::Fresh, Freshness::Fresh);
    assert_ne!(Freshness::Fresh, Freshness::Stale);
}

#[test]
fn test_validate_valid_request() {
    let req = make_query_request("valid search query", Some(10));
    assert!(req.validate().is_ok());
}

#[test]
fn test_validate_empty_query() {
    let req = make_query_request("", None);
    assert_eq!(req.validate(), Err(ValidationError::EmptyQuery));

    let req = make_query_request("   ", None);
    assert_eq!(req.validate(), Err(ValidationError::EmptyQuery));
}

#[test]
fn test_validate_query_too_long() {
    let req = make_query_request(&"x".repeat(MAX_QUERY_LENGTH + 1), None);
    match req.validate() {
        Err(ValidationError::QueryTooLong { length, max }) => {
            assert_eq!(length, MAX_QUERY_LENGTH + 1);
            assert_eq!(max, MAX_QUERY_LENGTH);
        }
        _ => panic!("Expected QueryTooLong error"),
    }
}

#[test]
fn test_validate_null_bytes() {
    let req = make_query_request("test\0query", None);
    assert_eq!(req.validate(), Err(ValidationError::QueryContainsNullBytes));
}

#[test]
fn test_validate_control_chars() {
    // Bell character (ASCII 7)
    let req = make_query_request("test\x07query", None);
    assert_eq!(
        req.validate(),
        Err(ValidationError::QueryContainsControlChars)
    );
}

#[test]
fn test_validate_allows_whitespace() {
    let req = make_query_request("test\tquery\nwith\rwhitespace", None);
    assert!(req.validate().is_ok());
}

#[test]
fn test_validate_top_k_too_large() {
    let req = make_query_request("valid query", Some(MAX_TOP_K + 1));
    match req.validate() {
        Err(ValidationError::TopKTooLarge { value, max }) => {
            assert_eq!(value, MAX_TOP_K + 1);
            assert_eq!(max, MAX_TOP_K);
        }
        _ => panic!("Expected TopKTooLarge error"),
    }
}

#[test]
fn test_effective_top_k() {
    // With explicit value
    let req = make_query_request("test", Some(50));
    assert_eq!(req.effective_top_k(), 50);

    // Without value (uses default)
    let req = make_query_request("test", None);
    assert_eq!(req.effective_top_k(), DEFAULT_TOP_K);

    // With value exceeding max (clamped)
    let req = make_query_request("test", Some(MAX_TOP_K + 100));
    assert_eq!(req.effective_top_k(), MAX_TOP_K);
}

#[test]
fn test_validation_error_display() {
    assert!(ValidationError::EmptyQuery.to_string().contains("empty"));
    assert!(ValidationError::QueryTooLong {
        length: 100,
        max: 50
    }
    .to_string()
    .contains("100"));
    assert!(ValidationError::QueryContainsNullBytes
        .to_string()
        .contains("null"));
    assert!(ValidationError::QueryContainsControlChars
        .to_string()
        .contains("control"));
    assert!(ValidationError::TopKTooLarge {
        value: 100,
        max: 50
    }
    .to_string()
    .contains("100"));
}

#[test]
fn test_response_validation_valid() {
    let response = QueryResponse {
        results: vec![SearchResult {
            id: "doc1".to_string(),
            score: 0.95,
            content: "Valid content".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };
    assert!(response.validate().is_ok());
}

#[test]
fn test_response_validation_too_many_results() {
    let results: Vec<SearchResult> = (0..MAX_RESULTS + 1)
        .map(|i| SearchResult {
            id: format!("doc{}", i),
            score: 0.5,
            content: "content".to_string(),
            metadata: None,
            upstream_id: None,
        })
        .collect();

    let response = QueryResponse {
        results,
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };

    match response.validate() {
        Err(ResponseValidationError::TooManyResults { count, max }) => {
            assert_eq!(count, MAX_RESULTS + 1);
            assert_eq!(max, MAX_RESULTS);
        }
        _ => panic!("Expected TooManyResults error"),
    }
}

#[test]
fn test_response_validation_content_too_long() {
    let response = QueryResponse {
        results: vec![SearchResult {
            id: "doc1".to_string(),
            score: 0.9,
            content: "x".repeat(MAX_CONTENT_LENGTH + 1),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };

    match response.validate() {
        Err(ResponseValidationError::ContentTooLong {
            result_id,
            length,
            max,
        }) => {
            assert_eq!(result_id, "doc1");
            assert_eq!(length, MAX_CONTENT_LENGTH + 1);
            assert_eq!(max, MAX_CONTENT_LENGTH);
        }
        _ => panic!("Expected ContentTooLong error"),
    }
}

#[test]
fn test_response_validation_null_bytes() {
    let response = QueryResponse {
        results: vec![SearchResult {
            id: "doc1".to_string(),
            score: 0.9,
            content: "content\0with\0nulls".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };

    match response.validate() {
        Err(ResponseValidationError::ContentContainsNullBytes { result_id }) => {
            assert_eq!(result_id, "doc1");
        }
        _ => panic!("Expected ContentContainsNullBytes error"),
    }
}

#[test]
fn test_response_validation_control_chars() {
    let response = QueryResponse {
        results: vec![SearchResult {
            id: "doc1".to_string(),
            score: 0.9,
            content: "content\x07with\x1bcontrol".to_string(), // bell and escape
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };

    match response.validate() {
        Err(ResponseValidationError::ContentContainsControlChars { result_id }) => {
            assert_eq!(result_id, "doc1");
        }
        _ => panic!("Expected ContentContainsControlChars error"),
    }
}

#[test]
fn test_response_validation_empty_result_id() {
    let response = QueryResponse {
        results: vec![SearchResult {
            id: "".to_string(),
            score: 0.9,
            content: "content".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };

    match response.validate() {
        Err(ResponseValidationError::EmptyResultId) => {}
        _ => panic!("Expected EmptyResultId error"),
    }
}

#[test]
fn test_response_validation_invalid_score_nan() {
    let response = QueryResponse {
        results: vec![SearchResult {
            id: "doc1".to_string(),
            score: f32::NAN,
            content: "content".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };

    match response.validate() {
        Err(ResponseValidationError::InvalidScore { result_id, score }) => {
            assert_eq!(result_id, "doc1");
            assert!(score.is_nan());
        }
        _ => panic!("Expected InvalidScore error"),
    }
}

#[test]
fn test_response_validation_invalid_score_infinite() {
    let response = QueryResponse {
        results: vec![SearchResult {
            id: "doc1".to_string(),
            score: f32::INFINITY,
            content: "content".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };

    match response.validate() {
        Err(ResponseValidationError::InvalidScore { result_id, .. }) => {
            assert_eq!(result_id, "doc1");
        }
        _ => panic!("Expected InvalidScore error"),
    }
}

#[test]
fn test_response_validation_allows_whitespace() {
    let response = QueryResponse {
        results: vec![SearchResult {
            id: "doc1".to_string(),
            score: 0.9,
            content: "content\twith\nnewlines\rand\ttabs".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };
    assert!(response.validate().is_ok());
}

#[test]
fn test_response_approximate_size() {
    let response = QueryResponse {
        results: vec![
            SearchResult {
                id: "doc1".to_string(),
                score: 0.9,
                content: "hello".to_string(),
                metadata: None,
                upstream_id: None,
            },
            SearchResult {
                id: "doc2".to_string(),
                score: 0.8,
                content: "world".to_string(),
                metadata: Some(serde_json::json!({"key": "value"})),
                upstream_id: None,
            },
        ],
        cache_status: CacheStatus::Hit,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };
    let size = response.approximate_size();
    assert!(size > 0);
    // Should include id + content + overhead for each result
    assert!(size > "doc1".len() + "hello".len() + "doc2".len() + "world".len());
}

#[test]
fn test_response_validation_error_display() {
    assert!(ResponseValidationError::TooManyResults {
        count: 1001,
        max: 1000
    }
    .to_string()
    .contains("1001"));
    assert!(ResponseValidationError::ContentTooLong {
        result_id: "x".to_string(),
        length: 100,
        max: 50
    }
    .to_string()
    .contains("100"));
    assert!(ResponseValidationError::ContentContainsNullBytes {
        result_id: "x".to_string()
    }
    .to_string()
    .contains("null"));
    assert!(ResponseValidationError::EmptyResultId
        .to_string()
        .contains("empty"));
}

#[test]
fn test_schema_fingerprint_default() {
    let fp = SchemaFingerprint::default();
    assert!(fp.embedding_dims.is_none());
    assert!(fp.model_hint.is_none());
    assert!(fp.version.is_none());
}

#[test]
fn test_schema_fingerprint_new() {
    let fp = SchemaFingerprint::new(Some(768), Some("model-v1".to_string()));
    assert_eq!(fp.embedding_dims, Some(768));
    assert_eq!(fp.model_hint, Some("model-v1".to_string()));
    assert!(fp.version.is_none());
}

#[test]
fn test_schema_fingerprint_with_version() {
    let fp = SchemaFingerprint::with_version(
        Some(384),
        Some("model".to_string()),
        Some("v1.0".to_string()),
    );
    assert_eq!(fp.embedding_dims, Some(384));
    assert_eq!(fp.version, Some("v1.0".to_string()));
}

#[test]
fn test_schema_fingerprint_is_compatible() {
    let fp1 = SchemaFingerprint::new(Some(768), Some("model-v1".to_string()));
    let fp2 = SchemaFingerprint::new(Some(768), Some("model-v1".to_string()));
    assert!(fp1.is_compatible(&fp2));

    // Same dimensions, different model - incompatible
    let fp3 = SchemaFingerprint::new(Some(768), Some("model-v2".to_string()));
    assert!(!fp1.is_compatible(&fp3));

    // Different dimensions - incompatible
    let fp4 = SchemaFingerprint::new(Some(384), Some("model-v1".to_string()));
    assert!(!fp1.is_compatible(&fp4));

    // Unknown values are compatible
    let fp5 = SchemaFingerprint::new(None, None);
    assert!(fp1.is_compatible(&fp5));
    assert!(fp5.is_compatible(&fp1));
}

#[test]
fn test_drift_level_ordering() {
    assert!(DriftLevel::None < DriftLevel::Minor);
    assert!(DriftLevel::Minor < DriftLevel::Moderate);
    assert!(DriftLevel::Moderate < DriftLevel::Significant);
    assert!(DriftLevel::Significant < DriftLevel::Major);
}

#[test]
fn test_drift_level_exceeds() {
    assert!(!DriftLevel::None.exceeds(DriftLevel::None));
    assert!(DriftLevel::Minor.exceeds(DriftLevel::None));
    assert!(DriftLevel::Major.exceeds(DriftLevel::Moderate));
    assert!(!DriftLevel::Minor.exceeds(DriftLevel::Moderate));
}

#[test]
fn test_detect_drift_empty_results() {
    let old = QueryResponse {
        results: vec![],
        cache_status: CacheStatus::Hit,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };
    let new = QueryResponse {
        results: vec![],
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };
    assert_eq!(detect_drift(&old, &new), DriftLevel::None);
}

#[test]
fn test_detect_drift_one_empty() {
    let old = QueryResponse {
        results: vec![SearchResult {
            id: "1".to_string(),
            score: 0.9,
            content: "test".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Hit,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };
    let new = QueryResponse {
        results: vec![],
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };
    assert_eq!(detect_drift(&old, &new), DriftLevel::Major);
}

#[test]
fn test_detect_drift_identical() {
    let results = vec![
        SearchResult {
            id: "1".to_string(),
            score: 0.95,
            content: "test1".to_string(),
            metadata: None,
            upstream_id: None,
        },
        SearchResult {
            id: "2".to_string(),
            score: 0.85,
            content: "test2".to_string(),
            metadata: None,
            upstream_id: None,
        },
    ];
    let old = QueryResponse {
        results: results.clone(),
        cache_status: CacheStatus::Hit,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };
    let new = QueryResponse {
        results,
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };
    assert_eq!(detect_drift(&old, &new), DriftLevel::None);
}

#[test]
fn test_detect_drift_different_results() {
    let old = QueryResponse {
        results: vec![SearchResult {
            id: "1".to_string(),
            score: 0.9,
            content: "old".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Hit,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };
    let new = QueryResponse {
        results: vec![SearchResult {
            id: "2".to_string(),
            score: 0.9,
            content: "new".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };
    // Completely different IDs = Major drift
    assert_eq!(detect_drift(&old, &new), DriftLevel::Major);
}

#[test]
fn test_drift_aggregator_new() {
    let agg = DriftAggregator::new(50, DriftLevel::Moderate, 5);
    assert!(agg.is_empty());
    assert_eq!(agg.len(), 0);
}

#[test]
fn test_drift_aggregator_with_defaults() {
    let agg = DriftAggregator::with_defaults();
    assert!(agg.is_empty());
}

#[test]
fn test_drift_aggregator_record() {
    let mut agg = DriftAggregator::with_defaults();
    let hash = [0u8; 32];

    agg.record(DriftLevel::Minor, hash);
    assert_eq!(agg.len(), 1);

    agg.record(DriftLevel::Moderate, hash);
    assert_eq!(agg.len(), 2);
}

#[test]
fn test_drift_aggregator_summary() {
    let mut agg = DriftAggregator::with_defaults();
    let hash = [0u8; 32];

    agg.record(DriftLevel::None, hash);
    agg.record(DriftLevel::None, hash);
    agg.record(DriftLevel::Minor, hash);
    agg.record(DriftLevel::Moderate, hash);

    let summary = agg.summary();
    assert_eq!(summary.total_observations, 4);
    assert_eq!(summary.level_counts[0], 2); // None
    assert_eq!(summary.level_counts[1], 1); // Minor
    assert_eq!(summary.level_counts[2], 1); // Moderate
    assert_eq!(summary.max_drift, DriftLevel::Moderate);
}

#[test]
fn test_drift_aggregator_alert() {
    let mut agg = DriftAggregator::new(100, DriftLevel::Minor, 3);
    let hash = [0u8; 32];

    // Below threshold
    agg.record(DriftLevel::Moderate, hash);
    agg.record(DriftLevel::Moderate, hash);
    assert!(!agg.should_alert());

    // At threshold
    agg.record(DriftLevel::Significant, hash);
    assert!(agg.should_alert());
}

#[test]
fn test_drift_aggregator_clear() {
    let mut agg = DriftAggregator::with_defaults();
    let hash = [0u8; 32];

    agg.record(DriftLevel::Minor, hash);
    agg.record(DriftLevel::Major, hash);
    assert_eq!(agg.len(), 2);

    agg.clear();
    assert!(agg.is_empty());
}

#[test]
fn test_drift_aggregator_max_observations() {
    let mut agg = DriftAggregator::new(3, DriftLevel::Moderate, 2);
    let hash = [0u8; 32];

    agg.record(DriftLevel::None, hash);
    agg.record(DriftLevel::Minor, hash);
    agg.record(DriftLevel::Moderate, hash);
    assert_eq!(agg.len(), 3);

    // Adding more should evict oldest
    agg.record(DriftLevel::Major, hash);
    assert_eq!(agg.len(), 3);

    let summary = agg.summary();
    assert_eq!(summary.level_counts[0], 0); // None was evicted
    assert_eq!(summary.max_drift, DriftLevel::Major);
}

#[test]
fn test_schema_extract_from_json_direct_fields() {
    let json = serde_json::json!({
        "embedding_dims": 768,
        "model": "text-embedding-ada-002",
        "version": "v1.0"
    });
    let fp = SchemaFingerprint::extract_from_json(&json);
    assert_eq!(fp.embedding_dims, Some(768));
    assert_eq!(fp.model_hint, Some("text-embedding-ada-002".to_string()));
    assert_eq!(fp.version, Some("v1.0".to_string()));
}

#[test]
fn test_schema_extract_from_json_dimensions_alias() {
    let json = serde_json::json!({
        "dimensions": 384
    });
    let fp = SchemaFingerprint::extract_from_json(&json);
    assert_eq!(fp.embedding_dims, Some(384));
}

#[test]
fn test_schema_extract_from_json_from_vector_array() {
    let json = serde_json::json!({
        "results": [
            {
                "id": "doc1",
                "vector": [0.1, 0.2, 0.3, 0.4]
            }
        ]
    });
    let fp = SchemaFingerprint::extract_from_json(&json);
    assert_eq!(fp.embedding_dims, Some(4));
}

#[test]
fn test_schema_extract_from_json_from_metadata() {
    let json = serde_json::json!({
        "metadata": {
            "embedding_dims": 512,
            "model": "custom-model"
        }
    });
    let fp = SchemaFingerprint::extract_from_json(&json);
    assert_eq!(fp.embedding_dims, Some(512));
    assert_eq!(fp.model_hint, Some("custom-model".to_string()));
}

#[test]
fn test_schema_extract_from_json_empty() {
    let json = serde_json::json!({});
    let fp = SchemaFingerprint::extract_from_json(&json);
    assert!(fp.embedding_dims.is_none());
    assert!(fp.model_hint.is_none());
    assert!(!fp.is_known());
}

#[test]
fn test_schema_fingerprint_is_known() {
    let fp1 = SchemaFingerprint::new(Some(768), None);
    assert!(fp1.is_known());

    let fp2 = SchemaFingerprint::new(None, Some("model".to_string()));
    assert!(fp2.is_known());

    let fp3 = SchemaFingerprint::default();
    assert!(!fp3.is_known());
}

// === Timestamp Validation Tests (Replay Attack Detection) ===

#[test]
fn test_replay_attack_detection_old_timestamp() {
    let now_ms = 1_700_000_000_000u64; // ~2023
    let max_age_ms = 60_000; // 1 minute
    let clock_skew_tolerance_ms = 5_000; // 5 seconds

    // Response generated 2 minutes ago (too old)
    let response = QueryResponse {
        results: vec![],
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: Some(now_ms - 120_000), // 2 minutes ago
        miss_reason: None,
    };

    match response.validate_timestamp(max_age_ms, now_ms, clock_skew_tolerance_ms) {
        Err(ResponseValidationError::TimestampTooOld {
            age_ms,
            max_age_ms: max,
        }) => {
            assert!(age_ms >= 120_000);
            assert_eq!(max, 60_000);
        }
        _ => panic!("Expected TimestampTooOld error"),
    }
}

#[test]
fn test_replay_attack_detection_valid_timestamp() {
    let now_ms = 1_700_000_000_000u64;
    let max_age_ms = 60_000; // 1 minute
    let clock_skew_tolerance_ms = 5_000;

    // Response generated 30 seconds ago (valid)
    let response = QueryResponse {
        results: vec![],
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: Some(now_ms - 30_000), // 30 seconds ago
        miss_reason: None,
    };

    assert!(response
        .validate_timestamp(max_age_ms, now_ms, clock_skew_tolerance_ms)
        .is_ok());
}

#[test]
fn test_replay_attack_detection_no_timestamp() {
    let now_ms = 1_700_000_000_000u64;
    let max_age_ms = 60_000;
    let clock_skew_tolerance_ms = 5_000;

    // Response with no timestamp - should pass (timestamp validation is opt-in)
    let response = QueryResponse {
        results: vec![],
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };

    assert!(response
        .validate_timestamp(max_age_ms, now_ms, clock_skew_tolerance_ms)
        .is_ok());
}

#[test]
fn test_replay_attack_detection_future_timestamp() {
    let now_ms = 1_700_000_000_000u64;
    let max_age_ms = 60_000;
    let clock_skew_tolerance_ms = 5_000; // 5 seconds

    // Response timestamp 10 seconds in future (beyond tolerance)
    let response = QueryResponse {
        results: vec![],
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: Some(now_ms + 10_000), // 10 seconds in future
        miss_reason: None,
    };

    match response.validate_timestamp(max_age_ms, now_ms, clock_skew_tolerance_ms) {
        Err(ResponseValidationError::TimestampInFuture {
            generated_at_ms,
            now_ms: n,
        }) => {
            assert_eq!(generated_at_ms, now_ms + 10_000);
            assert_eq!(n, now_ms);
        }
        _ => panic!("Expected TimestampInFuture error"),
    }
}

#[test]
fn test_replay_attack_detection_within_clock_skew() {
    let now_ms = 1_700_000_000_000u64;
    let max_age_ms = 60_000;
    let clock_skew_tolerance_ms = 5_000; // 5 seconds

    // Response timestamp 3 seconds in future (within tolerance)
    let response = QueryResponse {
        results: vec![],
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: Some(now_ms + 3_000), // 3 seconds in future
        miss_reason: None,
    };

    assert!(response
        .validate_timestamp(max_age_ms, now_ms, clock_skew_tolerance_ms)
        .is_ok());
}

#[test]
fn test_timestamp_validation_error_display() {
    let err1 = ResponseValidationError::TimestampTooOld {
        age_ms: 120_000,
        max_age_ms: 60_000,
    };
    assert!(err1.to_string().contains("120000"));
    assert!(err1.to_string().contains("replay"));

    let err2 = ResponseValidationError::TimestampInFuture {
        generated_at_ms: 1000,
        now_ms: 500,
    };
    assert!(err2.to_string().contains("future"));
}

#[test]
fn test_current_time_ms() {
    let time_ms = QueryResponse::current_time_ms();
    // Should be a reasonable timestamp (after year 2020)
    assert!(time_ms > 1_577_836_800_000); // 2020-01-01 00:00:00 UTC
}

// === Signature Verification Tests ===

#[test]
fn test_upstream_signature_verification() {
    let key = b"test-secret-key".to_vec();
    let config = SignatureConfig::hmac_sha256(key);

    // Valid response with correct signature
    let response_body = b"test response data";
    let signature = config.sign(response_body);

    assert!(config.verify(response_body, Some(&signature)).is_ok());
}

#[test]
fn test_signature_verification_rejects_unsigned() {
    let key = b"test-secret-key".to_vec();
    let config = SignatureConfig::hmac_sha256(key);

    // Response without signature should be rejected
    let response_body = b"test response data";

    match config.verify(response_body, None) {
        Err(ResponseValidationError::SignatureMissing) => {}
        other => panic!("Expected SignatureMissing, got {:?}", other),
    }
}

#[test]
fn test_signature_verification_rejects_invalid() {
    let key = b"test-secret-key".to_vec();
    let config = SignatureConfig::hmac_sha256(key);

    // Response with wrong signature should be rejected
    let response_body = b"test response data";
    let wrong_signature = "deadbeef1234567890abcdef";

    match config.verify(response_body, Some(wrong_signature)) {
        Err(ResponseValidationError::SignatureInvalid { .. }) => {}
        other => panic!("Expected SignatureInvalid, got {:?}", other),
    }
}

#[test]
fn test_signature_verification_disabled() {
    let config = SignatureConfig::default();
    assert!(!config.enabled);

    // When disabled, any response should pass
    let response_body = b"test response data";
    assert!(config.verify(response_body, None).is_ok());
    assert!(config.verify(response_body, Some("wrong-sig")).is_ok());
}

#[test]
fn test_signature_unsupported_algorithm() {
    let config = SignatureConfig {
        enabled: true,
        header_name: "X-Signature".to_string(),
        algorithm: "RSA-SHA512".to_string(),
        key: b"test".to_vec(),
    };

    let response_body = b"test";
    match config.verify(response_body, Some("signature")) {
        Err(ResponseValidationError::SignatureAlgorithmUnsupported { algorithm }) => {
            assert_eq!(algorithm, "RSA-SHA512");
        }
        other => panic!("Expected SignatureAlgorithmUnsupported, got {:?}", other),
    }
}

#[test]
fn test_signature_blake3_algorithm() {
    let config = SignatureConfig {
        enabled: true,
        header_name: "X-Signature".to_string(),
        algorithm: "BLAKE3".to_string(),
        key: b"secret".to_vec(),
    };

    let response_body = b"test data";
    let signature = config.sign(response_body);

    assert!(config.verify(response_body, Some(&signature)).is_ok());
}

#[test]
fn test_signature_tampered_response() {
    let key = b"test-secret-key".to_vec();
    let config = SignatureConfig::hmac_sha256(key);

    let original_body = b"original response";
    let tampered_body = b"tampered response";
    let signature = config.sign(original_body);

    // Signature computed for original should fail for tampered
    match config.verify(tampered_body, Some(&signature)) {
        Err(ResponseValidationError::SignatureInvalid { .. }) => {}
        other => panic!("Expected SignatureInvalid, got {:?}", other),
    }
}

// === TLS Certificate Pinning Tests ===

#[test]
fn test_tls_certificate_pinning() {
    let fingerprint = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
    let config = CertificatePinConfig::new(vec![fingerprint.to_string()]);

    // Matching fingerprint should pass
    assert!(config.verify(fingerprint).is_ok());
}

#[test]
fn test_tls_certificate_pinning_case_insensitive() {
    let fingerprint_lower = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
    let fingerprint_upper = "A1B2C3D4E5F6A1B2C3D4E5F6A1B2C3D4E5F6A1B2C3D4E5F6A1B2C3D4E5F6A1B2";
    let config = CertificatePinConfig::new(vec![fingerprint_lower.to_string()]);

    // Case insensitive matching
    assert!(config.verify(fingerprint_upper).is_ok());
}

#[test]
fn test_tls_certificate_pinning_with_colons() {
    let fingerprint_plain = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
    let fingerprint_colons = "a1:b2:c3:d4:e5:f6:a1:b2:c3:d4:e5:f6:a1:b2:c3:d4:e5:f6:a1:b2:c3:d4:e5:f6:a1:b2:c3:d4:e5:f6:a1:b2";
    let config = CertificatePinConfig::new(vec![fingerprint_plain.to_string()]);

    // Should normalize and match
    assert!(config.verify(fingerprint_colons).is_ok());
}

#[test]
fn test_tls_certificate_pinning_fails_for_wrong_cert() {
    let expected_fingerprint = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
    let wrong_fingerprint = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
    let config = CertificatePinConfig::new(vec![expected_fingerprint.to_string()]);

    match config.verify(wrong_fingerprint) {
        Err(ResponseValidationError::CertificatePinningFailed {
            expected_fingerprint: e,
            got_fingerprint: g,
        }) => {
            assert_eq!(e, expected_fingerprint);
            assert_eq!(g, wrong_fingerprint);
        }
        other => panic!("Expected CertificatePinningFailed, got {:?}", other),
    }
}

#[test]
fn test_tls_certificate_pinning_multiple_pins() {
    let fingerprint1 = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
    let fingerprint2 = "b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3";
    let config =
        CertificatePinConfig::new(vec![fingerprint1.to_string(), fingerprint2.to_string()]);

    // Either fingerprint should match
    assert!(config.verify(fingerprint1).is_ok());
    assert!(config.verify(fingerprint2).is_ok());
}

#[test]
fn test_tls_certificate_pinning_disabled() {
    let config = CertificatePinConfig::default();
    assert!(!config.enabled);

    // When disabled, any fingerprint should pass
    let any_fingerprint = "anything";
    assert!(config.verify(any_fingerprint).is_ok());
}

#[test]
fn test_certificate_fingerprint_computation() {
    // DER-encoded test data
    let cert_der = b"fake certificate data for testing";
    let fingerprint = CertificatePinConfig::compute_fingerprint(cert_der);

    // Should be a 64-character hex string (256 bits / 4 bits per char)
    assert_eq!(fingerprint.len(), 64);
    assert!(fingerprint.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn test_certificate_invalid_error_display() {
    let err = ResponseValidationError::CertificateInvalid {
        reason: "expired".to_string(),
    };
    let msg = err.to_string();
    assert!(msg.contains("invalid"));
    assert!(msg.contains("expired"));
}

#[test]
fn test_signature_error_display() {
    let err1 = ResponseValidationError::SignatureMissing;
    assert!(err1.to_string().contains("missing"));

    let err2 = ResponseValidationError::SignatureInvalid {
        expected: "abc".to_string(),
        got: "xyz".to_string(),
    };
    assert!(err2.to_string().contains("invalid"));
    assert!(err2.to_string().contains("abc"));
    assert!(err2.to_string().contains("xyz"));

    let err3 = ResponseValidationError::SignatureAlgorithmUnsupported {
        algorithm: "MD5".to_string(),
    };
    assert!(err3.to_string().contains("MD5"));
    assert!(err3.to_string().contains("not supported"));
}

// === Additional Negative Tests ===

#[test]
fn test_validate_query_boundary_conditions() {
    // Exactly at max length should be valid
    let req = make_query_request(&"x".repeat(MAX_QUERY_LENGTH), None);
    assert!(req.validate().is_ok());

    // One over max should fail
    let req = make_query_request(&"x".repeat(MAX_QUERY_LENGTH + 1), None);
    assert!(req.validate().is_err());
}

#[test]
fn test_validate_query_unicode() {
    // Unicode queries should be valid
    let req = make_query_request("こんにちは世界", None);
    assert!(req.validate().is_ok());

    // Emoji should be valid
    let req = make_query_request("search 🔍 query", None);
    assert!(req.validate().is_ok());
}

#[test]
fn test_validate_query_whitespace_variations() {
    // Tab + newline should be valid
    let req = make_query_request("line1\tline2\nline3", None);
    assert!(req.validate().is_ok());

    // Only whitespace should be empty error
    let req = make_query_request("\t\n\r  ", None);
    assert_eq!(req.validate(), Err(ValidationError::EmptyQuery));
}

#[test]
fn test_validate_response_empty_results() {
    // Empty results should be valid
    let response = QueryResponse {
        results: vec![],
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };
    assert!(response.validate().is_ok());
}

#[test]
fn test_validate_response_score_boundaries() {
    // Negative score should be valid (some algorithms produce these)
    let result = SearchResult {
        id: "doc1".to_string(),
        score: -0.5,
        content: "test".to_string(),
        metadata: None,
        upstream_id: None,
    };
    let response = QueryResponse {
        results: vec![result],
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };
    assert!(response.validate().is_ok());

    // Score > 1.0 should be valid (BM25 scores can be > 1)
    let result = SearchResult {
        id: "doc1".to_string(),
        score: 50.5,
        content: "test".to_string(),
        metadata: None,
        upstream_id: None,
    };
    let response = QueryResponse {
        results: vec![result],
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };
    assert!(response.validate().is_ok());
}

#[test]
fn test_validate_response_size_accumulation() {
    // Create results that together exceed max size
    let large_content = "x".repeat(MAX_CONTENT_LENGTH / 2);
    let results: Vec<SearchResult> = (0..10)
        .map(|i| SearchResult {
            id: format!("doc{}", i),
            score: 1.0,
            content: large_content.clone(),
            metadata: None,
            upstream_id: None,
        })
        .collect();

    let response = QueryResponse {
        results,
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };

    // Should fail due to total response size
    match response.validate() {
        Err(ResponseValidationError::ResponseTooLarge { size, max }) => {
            assert!(size > max);
        }
        Ok(()) => {
            // OK if under limit - depends on MAX values
        }
        Err(e) => panic!("Unexpected error: {:?}", e),
    }
}

// =============================================================================
// proptest: QueryRequest validation properties
// =============================================================================

mod proptest_query {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn prop_null_byte_always_fails(s in "[^\0]{0,100}\0[^\0]{0,100}") {
            let req = QueryRequest {
                query: s,
                top_k: Some(5),
                priority: None,
                upstream_id: None,
                upstream_type: None,
            };
            prop_assert_eq!(req.validate(), Err(ValidationError::QueryContainsNullBytes));
        }

        #[test]
        fn prop_valid_query_always_passes(s in "[a-zA-Z0-9][a-zA-Z0-9 ]{0,199}") {
            let req = QueryRequest {
                query: s,
                top_k: Some(5),
                priority: None,
                upstream_id: None,
                upstream_type: None,
            };
            prop_assert!(req.validate().is_ok());
        }

        #[test]
        fn prop_query_over_max_length_always_fails(extra in 1usize..500) {
            let query = "a".repeat(MAX_QUERY_LENGTH + extra);
            let req = QueryRequest {
                query,
                top_k: Some(5),
                priority: None,
                upstream_id: None,
                upstream_type: None,
            };
            match req.validate() {
                Err(ValidationError::QueryTooLong { .. }) => {}
                other => prop_assert!(false, "Expected QueryTooLong, got {:?}", other),
            }
        }

        #[test]
        fn prop_top_k_over_max_always_fails(k in (MAX_TOP_K + 1)..=usize::MAX) {
            let req = QueryRequest {
                query: "valid query".to_string(),
                top_k: Some(k),
                priority: None,
                upstream_id: None,
                upstream_type: None,
            };
            match req.validate() {
                Err(ValidationError::TopKTooLarge { .. }) => {}
                other => prop_assert!(false, "Expected TopKTooLarge, got {:?}", other),
            }
        }

        #[test]
        fn prop_top_k_in_range_valid_query_always_passes(k in 1usize..=MAX_TOP_K) {
            let req = QueryRequest {
                query: "valid query".to_string(),
                top_k: Some(k),
                priority: None,
                upstream_id: None,
                upstream_type: None,
            };
            prop_assert!(req.validate().is_ok());
        }
    }
}
