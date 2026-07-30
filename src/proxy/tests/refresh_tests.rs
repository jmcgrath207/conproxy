#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;
use crate::proxy::metrics::ProxyMetrics;
use crate::proxy::types::{CacheStatus, QueryResponse, SchemaFingerprint, SearchResult};
use std::time::Duration;

fn make_response(content: &str) -> QueryResponse {
    QueryResponse {
        results: vec![SearchResult {
            id: "test".to_string(),
            score: 1.0,
            content: content.to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    }
}

#[test]
fn test_refresh_worker_creation() {
    let cache = Arc::new(CacheStore::new(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        1000,
    ));
    let upstream = Arc::new(
        GenericRestAdapter::new("http://localhost:8080", Duration::from_secs(30)).unwrap(),
    );
    let cancel = CancellationToken::new();

    let worker = RefreshWorker::new(
        cache,
        upstream,
        "test-worker".to_string(),
        Duration::from_secs(60),
        cancel.clone(),
    );

    assert_eq!(worker.pending_count(), 0);
    assert!(worker.is_running());

    cancel.cancel();
    assert!(!worker.is_running());
}

#[test]
fn test_query_tracking_worker() {
    let cache = Arc::new(CacheStore::new(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        1000,
    ));
    let upstream = Arc::new(
        GenericRestAdapter::new("http://localhost:8080", Duration::from_secs(30)).unwrap(),
    );
    let cancel = CancellationToken::new();

    let worker = QueryTrackingRefreshWorker::new(
        cache.clone(),
        upstream,
        "test-worker".to_string(),
        Duration::from_secs(60),
        cancel,
    );

    // Register a query
    worker.register_query("test query");
    assert_eq!(worker.tracked_count(), 1);

    // Register same query again (should not duplicate due to hash)
    worker.register_query("test query");
    assert_eq!(worker.tracked_count(), 1);

    // Register different query
    worker.register_query("another query");
    assert_eq!(worker.tracked_count(), 2);

    // Unregister
    worker.unregister_query("test query");
    assert_eq!(worker.tracked_count(), 1);
}

#[tokio::test]
async fn test_refresh_worker_cancellation() {
    let cache = Arc::new(CacheStore::new(
        Duration::from_millis(10),  // Very short fresh duration
        Duration::from_millis(100), // Short stale duration
        1000,
    ));
    let upstream = Arc::new(
        GenericRestAdapter::new("http://localhost:8080", Duration::from_secs(30)).unwrap(),
    );
    let cancel = CancellationToken::new();

    let worker = Arc::new(RefreshWorker::new(
        cache.clone(),
        upstream,
        "test-worker".to_string(),
        Duration::from_millis(50),
        cancel.clone(),
    ));

    // Add an entry
    cache.insert(
        "test query",
        make_response("result"),
        "upstream".to_string(),
    );

    // Start worker in background
    let worker_clone = worker.clone();
    let handle = tokio::spawn(async move {
        worker_clone.run().await;
    });

    // Wait a bit
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Cancel
    cancel.cancel();

    // Worker should stop
    let _ = tokio::time::timeout(Duration::from_millis(200), handle).await;
}

#[test]
fn test_worker_with_metrics() {
    let cache = Arc::new(CacheStore::new(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        1000,
    ));
    let upstream = Arc::new(
        GenericRestAdapter::new("http://localhost:8080", Duration::from_secs(30)).unwrap(),
    );
    let metrics = Arc::new(ProxyMetrics::new());
    let cancel = CancellationToken::new();

    let worker = QueryTrackingRefreshWorker::with_metrics(
        cache,
        upstream,
        "test-worker".to_string(),
        Duration::from_secs(60),
        metrics.clone(),
        cancel,
    );

    assert_eq!(worker.tracked_count(), 0);
}

#[test]
fn test_schema_tracking() {
    let cache = Arc::new(CacheStore::new(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        1000,
    ));
    let upstream = Arc::new(
        GenericRestAdapter::new("http://localhost:8080", Duration::from_secs(30)).unwrap(),
    );
    let metrics = Arc::new(ProxyMetrics::new());
    let cancel = CancellationToken::new();

    let worker = QueryTrackingRefreshWorker::with_metrics(
        cache,
        upstream,
        "test-worker".to_string(),
        Duration::from_secs(60),
        metrics.clone(),
        cancel,
    );

    // Initially no schema
    assert!(worker.last_schema().is_none());

    // Update schema
    let schema1 = SchemaFingerprint::new(Some(768), Some("model-v1".to_string()));
    worker.update_schema(schema1.clone());
    assert_eq!(worker.last_schema(), Some(schema1));

    // Update with same schema (compatible)
    let schema2 = SchemaFingerprint::new(Some(768), Some("model-v1".to_string()));
    worker.update_schema(schema2);
    assert_eq!(metrics.snapshot().schema_changes, 0);

    // Update with different schema (incompatible)
    let schema3 = SchemaFingerprint::new(Some(384), Some("model-v2".to_string()));
    worker.update_schema(schema3);
    assert_eq!(metrics.snapshot().schema_changes, 1);
}

#[test]
fn test_worker_with_drift_settings() {
    let cache = Arc::new(CacheStore::new(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        1000,
    ));
    let upstream = Arc::new(
        GenericRestAdapter::new("http://localhost:8080", Duration::from_secs(30)).unwrap(),
    );
    let cancel = CancellationToken::new();
    let drift_agg = DriftAggregator::with_defaults();

    let worker = QueryTrackingRefreshWorker::with_drift_settings(
        cache,
        upstream,
        "test-worker".to_string(),
        Duration::from_secs(60),
        drift_agg,
        cancel,
    );

    assert_eq!(worker.tracked_count(), 0);
    assert_eq!(worker.pending_count(), 0);
}

#[test]
fn test_drift_summary_and_alert() {
    let cache = Arc::new(CacheStore::new(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        1000,
    ));
    let upstream = Arc::new(
        GenericRestAdapter::new("http://localhost:8080", Duration::from_secs(30)).unwrap(),
    );
    let cancel = CancellationToken::new();

    let worker = QueryTrackingRefreshWorker::new(
        cache,
        upstream,
        "test-worker".to_string(),
        Duration::from_secs(60),
        cancel,
    );

    // Initially no drift
    let summary = worker.drift_summary();
    assert_eq!(summary.total_observations, 0);
    assert!(!worker.has_drift_alert());

    // Clear drift should not panic on empty
    worker.clear_drift();
    assert_eq!(worker.drift_summary().total_observations, 0);
}

#[test]
fn test_register_unregister_queries() {
    let cache = Arc::new(CacheStore::new(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        1000,
    ));
    let upstream = Arc::new(
        GenericRestAdapter::new("http://localhost:8080", Duration::from_secs(30)).unwrap(),
    );
    let cancel = CancellationToken::new();

    let worker = QueryTrackingRefreshWorker::new(
        cache,
        upstream,
        "test-worker".to_string(),
        Duration::from_secs(60),
        cancel,
    );

    worker.register_query("q1");
    worker.register_query("q2");
    worker.register_query("q3");
    assert_eq!(worker.tracked_count(), 3);

    worker.unregister_query("q2");
    assert_eq!(worker.tracked_count(), 2);

    // Unregister non-existent query — no panic
    worker.unregister_query("non-existent");
    assert_eq!(worker.tracked_count(), 2);
}

#[test]
fn test_last_schema_initially_none() {
    let cache = Arc::new(CacheStore::new(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        1000,
    ));
    let upstream = Arc::new(
        GenericRestAdapter::new("http://localhost:8080", Duration::from_secs(30)).unwrap(),
    );
    let cancel = CancellationToken::new();

    let worker = QueryTrackingRefreshWorker::new(
        cache,
        upstream,
        "test-worker".to_string(),
        Duration::from_secs(60),
        cancel,
    );

    assert!(worker.last_schema().is_none());
}

#[test]
fn test_refreshable_entry() {
    let entry = RefreshableEntry {
        hash: [0u8; 32],
        query: "test query".to_string(),
    };
    assert_eq!(entry.query, "test query");
    assert_eq!(entry.hash, [0u8; 32]);
}

// === Async mock-server tests for refresh_stale_entries() ===

/// Start a lightweight axum test server on a random port.
async fn start_mock_server(router: axum::Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{}", addr);
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (base_url, handle)
}

fn mock_response_with_content(content: &str) -> QueryResponse {
    QueryResponse {
        results: vec![SearchResult {
            id: "refreshed".to_string(),
            score: 0.95,
            content: content.to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 5,
        generated_at: None,
        miss_reason: None,
    }
}

#[tokio::test]
async fn test_refresh_worker_refreshes_stale_entries() {
    use crate::proxy::cache::CacheStore;
    use axum::{routing::post, Json, Router};

    let fresh_content = mock_response_with_content("refreshed content");
    let app = Router::new().route(
        "/query",
        post(move || {
            let r = fresh_content.clone();
            async move { Json(serde_json::to_value(&r).unwrap()) }
        }),
    );
    let (base_url, _handle) = start_mock_server(app).await;

    // Cache with 1ms fresh duration → entries go stale immediately
    let cache = Arc::new(CacheStore::new(
        Duration::from_millis(1),
        Duration::from_secs(60),
        100,
    ));
    let upstream = Arc::new(GenericRestAdapter::new(&base_url, Duration::from_secs(5)).unwrap());
    let cancel = CancellationToken::new();

    // Insert an entry that will become stale
    cache.insert(
        "stale query",
        make_response("old content"),
        "upstream-1".to_string(),
    );

    // Wait for staleness
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(
        !cache.get_stale_hashes().is_empty(),
        "Entry should be stale"
    );

    let worker = Arc::new(RefreshWorker::new(
        cache.clone(),
        upstream,
        "test-refresh".to_string(),
        Duration::from_millis(50), // fast interval
        cancel.clone(),
    ));

    let worker_clone = worker.clone();
    let handle = tokio::spawn(async move {
        worker_clone.run().await;
    });

    // Let the worker run a few cycles to pick up and refresh the stale entry
    tokio::time::sleep(Duration::from_millis(300)).await;

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(200), handle).await;

    // The entry should now have the refreshed content
    let entry = cache.get("stale query");
    assert!(entry.is_some());
    let entry = entry.unwrap();
    assert_eq!(entry.response.results[0].content, "refreshed content");
    assert_eq!(entry.response.results[0].id, "refreshed");
}

#[tokio::test]
async fn test_query_tracking_refresh_with_drift() {
    use crate::proxy::cache::CacheStore;
    use axum::{routing::post, Json, Router};

    // Return slightly different results to trigger drift
    let fresh = QueryResponse {
        results: vec![SearchResult {
            id: "new-doc".to_string(),
            score: 0.8,
            content: "different result".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 3,
        generated_at: None,
        miss_reason: None,
    };

    let app = Router::new().route(
        "/query",
        post(move || {
            let r = fresh.clone();
            async move { Json(serde_json::to_value(&r).unwrap()) }
        }),
    );
    let (base_url, _handle) = start_mock_server(app).await;

    let cache = Arc::new(CacheStore::new(
        Duration::from_millis(1),
        Duration::from_secs(60),
        100,
    ));
    let upstream = Arc::new(GenericRestAdapter::new(&base_url, Duration::from_secs(5)).unwrap());
    let metrics = Arc::new(ProxyMetrics::new());
    let cancel = CancellationToken::new();

    // Insert original entry
    cache.insert(
        "drift query",
        make_response("original content"),
        "upstream-1".to_string(),
    );

    tokio::time::sleep(Duration::from_millis(10)).await;

    let worker = Arc::new(QueryTrackingRefreshWorker::with_metrics(
        cache.clone(),
        upstream,
        "test-drift".to_string(),
        Duration::from_millis(50),
        metrics.clone(),
        cancel.clone(),
    ));

    // Register the query so the worker knows how to refresh it
    worker.register_query("drift query");

    let worker_clone = worker.clone();
    let handle = tokio::spawn(async move {
        worker_clone.run().await;
    });

    // Let the worker run to detect and record drift
    tokio::time::sleep(Duration::from_millis(300)).await;

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(200), handle).await;

    // Drift should have been recorded
    let summary = worker.drift_summary();
    assert!(
        summary.total_observations > 0,
        "Expected drift observations, got 0"
    );

    // Metrics should record the refresh
    let snap = metrics.snapshot();
    assert!(
        snap.refresh_operations > 0,
        "Expected refresh operations recorded"
    );
}

#[tokio::test]
async fn test_query_tracking_refresh_upstream_error() {
    use crate::proxy::cache::CacheStore;
    use axum::{http::StatusCode, routing::post, Router};

    let app = Router::new().route(
        "/query",
        post(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
    );
    let (base_url, _handle) = start_mock_server(app).await;

    let cache = Arc::new(CacheStore::new(
        Duration::from_millis(1),
        Duration::from_secs(60),
        100,
    ));
    let upstream = Arc::new(GenericRestAdapter::new(&base_url, Duration::from_secs(5)).unwrap());
    let cancel = CancellationToken::new();

    // Insert original entry
    cache.insert(
        "error query",
        make_response("keep this"),
        "upstream-1".to_string(),
    );

    tokio::time::sleep(Duration::from_millis(10)).await;

    let worker = Arc::new(QueryTrackingRefreshWorker::new(
        cache.clone(),
        upstream,
        "test-error".to_string(),
        Duration::from_millis(50),
        cancel.clone(),
    ));

    worker.register_query("error query");

    let worker_clone = worker.clone();
    let handle = tokio::spawn(async move {
        worker_clone.run().await;
    });

    // Let the worker attempt refresh (will fail)
    tokio::time::sleep(Duration::from_millis(300)).await;

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(200), handle).await;

    // Entry should still exist with original content (stale-while-revalidate)
    let entry = cache.get("error query");
    assert!(entry.is_some());
    assert_eq!(entry.unwrap().response.results[0].content, "keep this");
}
