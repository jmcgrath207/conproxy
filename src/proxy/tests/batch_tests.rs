#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

fn make_batch_query(id: &str, query: &str) -> BatchQuery {
    BatchQuery {
        id: id.to_string(),
        query: query.to_string(),
        top_k: Some(10),
        priority: None,
    }
}

#[test]
fn test_batch_request_creation() {
    let request = BatchRequest {
        queries: vec![
            make_batch_query("q1", "test query 1"),
            make_batch_query("q2", "test query 2"),
        ],
        fail_fast: false,
        timeout_ms: Some(5000),
    };

    assert_eq!(request.queries.len(), 2);
    assert!(!request.fail_fast);
}

#[test]
fn test_batch_query_result_success() {
    let response = QueryResponse {
        results: vec![],
        cache_status: CacheStatus::Hit,
        took_ms: 50,
        generated_at: None,
        miss_reason: None,
    };

    let result = BatchQueryResult::success(response);
    assert!(result.success);
    assert!(result.response.is_some());
    assert!(result.error.is_none());
    assert_eq!(result.took_ms, 50);
}

#[test]
fn test_batch_query_result_error() {
    let result = BatchQueryResult::error("Connection failed", 100);
    assert!(!result.success);
    assert!(result.response.is_none());
    assert_eq!(result.error, Some("Connection failed".to_string()));
}

#[test]
fn test_batch_processor_validation_empty() {
    let processor = BatchProcessor::default_config();
    let request = BatchRequest {
        queries: vec![],
        fail_fast: false,
        timeout_ms: None,
    };

    assert_eq!(processor.validate(&request), Err(BatchError::EmptyBatch));
}

#[test]
fn test_batch_processor_validation_too_many() {
    let config = BatchConfig {
        max_queries: 2,
        ..Default::default()
    };
    let processor = BatchProcessor::new(config);

    let request = BatchRequest {
        queries: vec![
            make_batch_query("q1", "query 1"),
            make_batch_query("q2", "query 2"),
            make_batch_query("q3", "query 3"),
        ],
        fail_fast: false,
        timeout_ms: None,
    };

    assert!(matches!(
        processor.validate(&request),
        Err(BatchError::TooManyQueries { count: 3, max: 2 })
    ));
}

#[test]
fn test_batch_processor_validation_duplicate_id() {
    let processor = BatchProcessor::default_config();
    let request = BatchRequest {
        queries: vec![
            make_batch_query("q1", "query 1"),
            make_batch_query("q1", "query 2"), // Duplicate ID
        ],
        fail_fast: false,
        timeout_ms: None,
    };

    assert!(matches!(
        processor.validate(&request),
        Err(BatchError::DuplicateId(_))
    ));
}

#[test]
fn test_batch_processor_validation_ok() {
    let processor = BatchProcessor::default_config();
    let request = BatchRequest {
        queries: vec![
            make_batch_query("q1", "query 1"),
            make_batch_query("q2", "query 2"),
        ],
        fail_fast: false,
        timeout_ms: None,
    };

    assert!(processor.validate(&request).is_ok());
}

#[tokio::test]
async fn test_batch_processor_success() {
    let config = BatchConfig {
        parallel: false,
        ..Default::default()
    };
    let processor = BatchProcessor::new(config);

    let request = BatchRequest {
        queries: vec![
            make_batch_query("q1", "query 1"),
            make_batch_query("q2", "query 2"),
        ],
        fail_fast: false,
        timeout_ms: None,
    };

    let response = processor
        .process(request, |_| async {
            Ok(QueryResponse {
                results: vec![],
                cache_status: CacheStatus::Miss,
                took_ms: 10,
                generated_at: None,
                miss_reason: None,
            })
        })
        .await
        .unwrap();

    assert_eq!(response.success_count, 2);
    assert_eq!(response.error_count, 0);
    assert!(response.complete);
    assert_eq!(response.results.len(), 2);
}

#[tokio::test]
async fn test_batch_processor_partial_failure() {
    let config = BatchConfig {
        parallel: false,
        ..Default::default()
    };
    let processor = BatchProcessor::new(config);

    let request = BatchRequest {
        queries: vec![
            make_batch_query("q1", "success"),
            make_batch_query("q2", "fail"),
        ],
        fail_fast: false,
        timeout_ms: None,
    };

    let response = processor
        .process(request, |req| async move {
            if req.query == "fail" {
                Err("Simulated failure".to_string())
            } else {
                Ok(QueryResponse {
                    results: vec![],
                    cache_status: CacheStatus::Miss,
                    took_ms: 10,
                    generated_at: None,
                    miss_reason: None,
                })
            }
        })
        .await
        .unwrap();

    assert_eq!(response.success_count, 1);
    assert_eq!(response.error_count, 1);
    assert!(response.complete);
}

#[tokio::test]
async fn test_batch_processor_fail_fast() {
    let config = BatchConfig {
        parallel: false,
        ..Default::default()
    };
    let processor = BatchProcessor::new(config);

    let request = BatchRequest {
        queries: vec![
            make_batch_query("q1", "fail"),
            make_batch_query("q2", "success"),
        ],
        fail_fast: true,
        timeout_ms: None,
    };

    let response = processor
        .process(request, |req| async move {
            if req.query == "fail" {
                Err("Simulated failure".to_string())
            } else {
                Ok(QueryResponse {
                    results: vec![],
                    cache_status: CacheStatus::Miss,
                    took_ms: 10,
                    generated_at: None,
                    miss_reason: None,
                })
            }
        })
        .await
        .unwrap();

    assert_eq!(response.error_count, 1);
    assert!(!response.complete); // Stopped early due to fail_fast
    assert_eq!(response.results.len(), 1); // Only first query processed
}

#[test]
fn test_batch_config_default() {
    let config = BatchConfig::default();
    assert_eq!(config.max_queries, 100);
    assert!(config.parallel);
}

#[test]
fn test_batch_error_display() {
    assert_eq!(BatchError::EmptyBatch.to_string(), "Batch cannot be empty");
    assert_eq!(
        BatchError::TooManyQueries { count: 5, max: 3 }.to_string(),
        "Too many queries (5) in batch, max is 3"
    );
    assert_eq!(
        BatchError::DuplicateId("q1".to_string()).to_string(),
        "Duplicate query ID: q1"
    );
}

#[test]
fn test_batch_query_to_query_request_conversion() {
    let bq = BatchQuery {
        id: "q1".to_string(),
        query: "test query".to_string(),
        top_k: Some(5),
        priority: Some(2),
    };
    let qr: QueryRequest = bq.into();
    assert_eq!(qr.query, "test query");
    assert_eq!(qr.top_k, Some(5));
    assert_eq!(qr.priority, Some(2));
    assert!(qr.upstream_id.is_none());
    assert!(qr.upstream_type.is_none());
}

#[test]
fn test_batch_query_to_query_request_defaults() {
    let bq = BatchQuery {
        id: "q1".to_string(),
        query: "minimal".to_string(),
        top_k: None,
        priority: None,
    };
    let qr: QueryRequest = bq.into();
    assert_eq!(qr.query, "minimal");
    assert!(qr.top_k.is_none());
    assert!(qr.priority.is_none());
}

#[test]
fn test_batch_processor_config_accessor() {
    let config = BatchConfig {
        max_queries: 50,
        max_total_results: 500,
        parallel: false,
        max_parallel: 5,
        ..Default::default()
    };
    let processor = BatchProcessor::new(config);
    assert_eq!(processor.config().max_queries, 50);
    assert_eq!(processor.config().max_total_results, 500);
    assert!(!processor.config().parallel);
    assert_eq!(processor.config().max_parallel, 5);
}

#[test]
fn test_batch_config_default_values() {
    let config = BatchConfig::default();
    assert_eq!(config.max_total_results, 1000);
    assert_eq!(config.default_query_timeout, Duration::from_secs(30));
    assert_eq!(config.max_batch_timeout, Duration::from_secs(300));
    assert_eq!(config.max_parallel, 10);
}

#[tokio::test]
async fn test_batch_processor_parallel_success() {
    let config = BatchConfig {
        parallel: true,
        max_parallel: 4,
        ..Default::default()
    };
    let processor = BatchProcessor::new(config);

    let request = BatchRequest {
        queries: vec![
            make_batch_query("q1", "query 1"),
            make_batch_query("q2", "query 2"),
            make_batch_query("q3", "query 3"),
        ],
        fail_fast: false,
        timeout_ms: None,
    };

    let response = processor
        .process(request, |_| async {
            Ok(QueryResponse {
                results: vec![],
                cache_status: CacheStatus::Miss,
                took_ms: 5,
                generated_at: None,
                miss_reason: None,
            })
        })
        .await
        .unwrap();

    assert_eq!(response.success_count, 3);
    assert_eq!(response.error_count, 0);
    assert!(response.complete);
    assert_eq!(response.results.len(), 3);
}

#[tokio::test]
async fn test_batch_processor_parallel_fail_fast() {
    let config = BatchConfig {
        parallel: true,
        max_parallel: 2,
        ..Default::default()
    };
    let processor = BatchProcessor::new(config);

    let request = BatchRequest {
        queries: vec![
            make_batch_query("q1", "fail"),
            make_batch_query("q2", "success"),
            make_batch_query("q3", "success"),
        ],
        fail_fast: true,
        timeout_ms: None,
    };

    let response = processor
        .process(request, |req| async move {
            if req.query == "fail" {
                Err("Simulated failure".to_string())
            } else {
                Ok(QueryResponse {
                    results: vec![],
                    cache_status: CacheStatus::Miss,
                    took_ms: 5,
                    generated_at: None,
                    miss_reason: None,
                })
            }
        })
        .await
        .unwrap();

    // At least one error should be recorded
    assert!(response.error_count >= 1);
}

#[test]
fn test_batch_request_serialization() {
    let request = BatchRequest {
        queries: vec![make_batch_query("q1", "test")],
        fail_fast: true,
        timeout_ms: Some(5000),
    };
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["queries"][0]["id"], "q1");
    assert_eq!(json["queries"][0]["query"], "test");
    assert_eq!(json["fail_fast"], true);
    assert_eq!(json["timeout_ms"], 5000);
}

#[test]
fn test_batch_request_deserialization() {
    let json = r#"{"queries":[{"id":"q1","query":"hello","top_k":5}]}"#;
    let request: BatchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(request.queries.len(), 1);
    assert_eq!(request.queries[0].query, "hello");
    assert_eq!(request.queries[0].top_k, Some(5));
    assert!(!request.fail_fast); // default
    assert!(request.timeout_ms.is_none()); // default
}
