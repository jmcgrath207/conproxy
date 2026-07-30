use super::*;
use crate::proxy::types::{CacheStatus, QueryResponse, SearchResult};

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
fn test_cleanup_config_default() {
    let config = CleanupConfig::default();
    assert_eq!(config.interval, Duration::from_secs(300));
    assert!(!config.check_integrity);
}

#[test]
fn test_cleanup_worker_creation() {
    let cache = Arc::new(CacheStore::new(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        1000,
    ));
    let cancel = CancellationToken::new();
    let config = CleanupConfig::default();

    let _worker = CleanupWorker::new(cache, config, cancel);
}

#[tokio::test]
async fn test_cleanup_worker_cancellation() {
    let mut cache = CacheStore::new(Duration::from_millis(10), Duration::from_millis(20), 1000);
    cache.set_max_frozen_duration(Duration::from_millis(30));
    let cache = Arc::new(cache);
    let cancel = CancellationToken::new();
    let config = CleanupConfig {
        interval: Duration::from_millis(50),
        check_integrity: false,
    };

    cache.insert("test", make_response("content"), "upstream".to_string());

    let worker = Arc::new(CleanupWorker::new(cache.clone(), config, cancel.clone()));

    // Start worker
    let worker_clone = worker.clone();
    let handle = tokio::spawn(async move {
        worker_clone.run().await;
    });

    // Wait for a cleanup cycle
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Cancel
    cancel.cancel();

    // Worker should stop
    let _ = tokio::time::timeout(Duration::from_millis(200), handle).await;
}

#[test]
fn test_cleanup_worker_with_metrics() {
    let cache = Arc::new(CacheStore::new(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        1000,
    ));
    let metrics = Arc::new(ProxyMetrics::new());
    let cancel = CancellationToken::new();
    let config = CleanupConfig::default();

    let worker = CleanupWorker::with_metrics(cache, metrics, config, cancel);
    // Verify the worker was created with metrics (it has the metrics field set)
    // We can't directly inspect private fields, but we can verify no panic
    drop(worker);
}

#[test]
fn test_cleanup_config_custom() {
    let config = CleanupConfig {
        interval: Duration::from_secs(60),
        check_integrity: true,
    };
    assert_eq!(config.interval, Duration::from_secs(60));
    assert!(config.check_integrity);
}

#[tokio::test]
async fn test_cleanup_worker_with_integrity_check() {
    let mut cache = CacheStore::new(Duration::from_millis(10), Duration::from_millis(20), 1000);
    cache.set_max_frozen_duration(Duration::from_millis(30));
    let cache = Arc::new(cache);
    let metrics = Arc::new(ProxyMetrics::new());
    let cancel = CancellationToken::new();
    let config = CleanupConfig {
        interval: Duration::from_millis(50),
        check_integrity: true,
    };

    cache.insert("test", make_response("content"), "upstream".to_string());

    let worker = Arc::new(CleanupWorker::with_metrics(
        cache.clone(),
        metrics,
        config,
        cancel.clone(),
    ));

    let worker_clone = worker.clone();
    let handle = tokio::spawn(async move {
        worker_clone.run().await;
    });

    // Wait for cleanup + integrity check cycle
    tokio::time::sleep(Duration::from_millis(120)).await;

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(200), handle).await;
}

#[tokio::test]
async fn test_cleanup_evicts_expired_entries() {
    let mut cache = CacheStore::new(Duration::from_millis(10), Duration::from_millis(20), 1000);
    cache.set_max_frozen_duration(Duration::from_millis(30));
    let cache = Arc::new(cache);
    let cancel = CancellationToken::new();
    let config = CleanupConfig {
        interval: Duration::from_millis(50),
        check_integrity: false,
    };

    // Insert entries that will expire quickly
    cache.insert("test1", make_response("content1"), "upstream".to_string());
    cache.insert("test2", make_response("content2"), "upstream".to_string());
    assert_eq!(cache.len(), 2);

    let worker = Arc::new(CleanupWorker::new(cache.clone(), config, cancel.clone()));

    let worker_clone = worker.clone();
    let handle = tokio::spawn(async move {
        worker_clone.run().await;
    });

    // Wait for entries to expire and cleanup to run
    tokio::time::sleep(Duration::from_millis(200)).await;

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(200), handle).await;

    // Expired entries should have been cleaned up
    assert_eq!(cache.len(), 0);
}
