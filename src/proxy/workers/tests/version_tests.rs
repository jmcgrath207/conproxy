#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

#[test]
fn test_upstream_version_default() {
    let v = UpstreamVersion::default();
    assert!(v.is_empty());
}

#[test]
fn test_upstream_version_new() {
    let v = UpstreamVersion::new("v1.0".to_string());
    assert!(!v.is_empty());
    assert_eq!(v.version, Some("v1.0".to_string()));
}

#[test]
fn test_upstream_version_differs_version() {
    let v1 = UpstreamVersion::new("v1.0".to_string());
    let v2 = UpstreamVersion::new("v1.1".to_string());
    assert!(v1.differs_from(&v2));

    let v3 = UpstreamVersion::new("v1.0".to_string());
    assert!(!v1.differs_from(&v3));
}

#[test]
fn test_upstream_version_differs_hash() {
    let v1 = UpstreamVersion::full(None, Some("abc123".to_string()), None, None);
    let v2 = UpstreamVersion::full(None, Some("def456".to_string()), None, None);
    assert!(v1.differs_from(&v2));
}

#[test]
fn test_upstream_version_differs_doc_count() {
    // Small change (within 1%) - should not differ
    let v1 = UpstreamVersion::full(None, None, Some(1000), None);
    let v2 = UpstreamVersion::full(None, None, Some(1005), None);
    assert!(!v1.differs_from(&v2));

    // Large change - should differ
    let v3 = UpstreamVersion::full(None, None, Some(1100), None);
    assert!(v1.differs_from(&v3));
}

#[test]
fn test_upstream_version_differs_timestamp() {
    let v1 = UpstreamVersion::full(None, None, None, Some(1000000));
    let v2 = UpstreamVersion::full(None, None, None, Some(1000001));
    assert!(v1.differs_from(&v2));
}

#[test]
fn test_version_check_config_default() {
    let config = VersionCheckConfig::default();
    assert!(config.version_endpoint.is_empty());
    assert_eq!(config.poll_interval, Duration::from_secs(300));
    assert!(config.invalidate_on_change);
}

#[tokio::test]
async fn test_version_worker_no_endpoint() {
    let cache = Arc::new(CacheStore::new(
        Duration::from_secs(60),
        Duration::from_secs(120),
        100,
    ));
    let config = VersionCheckConfig::default();
    let cancel = CancellationToken::new();

    let worker = VersionCheckWorker::new(cache, config, cancel.clone());

    // Should exit immediately since no endpoint configured
    let handle = tokio::spawn(async move {
        worker.run().await;
    });

    // Should complete quickly
    let result = tokio::time::timeout(Duration::from_millis(100), handle).await;
    assert!(result.is_ok());
}

#[test]
fn test_version_worker_creation() {
    let cache = Arc::new(CacheStore::new(
        Duration::from_secs(60),
        Duration::from_secs(120),
        100,
    ));
    let config = VersionCheckConfig {
        version_endpoint: "http://localhost/version".to_string(),
        ..Default::default()
    };
    let cancel = CancellationToken::new();

    let worker = VersionCheckWorker::new(cache, config, cancel);

    assert_eq!(worker.changes_detected(), 0);
    assert!(worker.current_version().is_empty());
}

// === differs_from edge cases ===

#[test]
fn test_differs_from_one_version_none() {
    // When one side has version and other doesn't, the (Some, None) branch won't match
    let v1 = UpstreamVersion::new("v1.0".to_string());
    let v2 = UpstreamVersion::default(); // all None
    assert!(!v1.differs_from(&v2)); // no matching fields to compare
}

#[test]
fn test_differs_from_one_hash_none() {
    let v1 = UpstreamVersion::full(None, Some("abc".to_string()), None, None);
    let v2 = UpstreamVersion::full(None, None, None, None);
    assert!(!v1.differs_from(&v2)); // no matching fields
}

#[test]
fn test_differs_from_doc_count_exact_1_percent_boundary() {
    // 1% of 1000 = 10. Diff of exactly 10 should NOT differ (threshold is >)
    let v1 = UpstreamVersion::full(None, None, Some(1000), None);
    let v2 = UpstreamVersion::full(None, None, Some(1010), None);
    assert!(!v1.differs_from(&v2));

    // Diff of 11 should differ
    let v3 = UpstreamVersion::full(None, None, Some(1011), None);
    assert!(v1.differs_from(&v3));
}

#[test]
fn test_differs_from_doc_count_zero() {
    // Edge: both doc counts are 0 — no difference
    let v1 = UpstreamVersion::full(None, None, Some(0), None);
    let v2 = UpstreamVersion::full(None, None, Some(0), None);
    assert!(!v1.differs_from(&v2));
}

#[test]
fn test_differs_from_doc_count_small_values() {
    // With small doc counts, 1% threshold is 0, so any diff triggers
    let v1 = UpstreamVersion::full(None, None, Some(10), None);
    let v2 = UpstreamVersion::full(None, None, Some(11), None);
    assert!(v1.differs_from(&v2));
}

#[test]
fn test_differs_from_one_doc_count_none() {
    let v1 = UpstreamVersion::full(None, None, Some(1000), None);
    let v2 = UpstreamVersion::full(None, None, None, None);
    assert!(!v1.differs_from(&v2));
}

#[test]
fn test_differs_from_one_timestamp_none() {
    let v1 = UpstreamVersion::full(None, None, None, Some(100));
    let v2 = UpstreamVersion::full(None, None, None, None);
    assert!(!v1.differs_from(&v2));
}

#[test]
fn test_differs_from_same_timestamps() {
    let v1 = UpstreamVersion::full(None, None, None, Some(1000000));
    let v2 = UpstreamVersion::full(None, None, None, Some(1000000));
    assert!(!v1.differs_from(&v2));
}

#[test]
fn test_differs_from_multiple_fields_agree() {
    // All fields match — no difference
    let v1 = UpstreamVersion::full(
        Some("v1".into()),
        Some("hash1".into()),
        Some(1000),
        Some(100),
    );
    let v2 = UpstreamVersion::full(
        Some("v1".into()),
        Some("hash1".into()),
        Some(1000),
        Some(100),
    );
    assert!(!v1.differs_from(&v2));
}

#[test]
fn test_differs_from_version_matches_but_hash_differs() {
    let v1 = UpstreamVersion::full(Some("v1".into()), Some("aaa".into()), None, None);
    let v2 = UpstreamVersion::full(Some("v1".into()), Some("bbb".into()), None, None);
    assert!(v1.differs_from(&v2));
}

// === is_empty combos ===

#[test]
fn test_is_empty_only_version() {
    let v = UpstreamVersion {
        version: Some("v1".into()),
        ..Default::default()
    };
    assert!(!v.is_empty());
}

#[test]
fn test_is_empty_only_hash() {
    let v = UpstreamVersion {
        corpus_hash: Some("abc".into()),
        ..Default::default()
    };
    assert!(!v.is_empty());
}

#[test]
fn test_is_empty_only_doc_count() {
    let v = UpstreamVersion {
        doc_count: Some(42),
        ..Default::default()
    };
    assert!(!v.is_empty());
}

#[test]
fn test_is_empty_only_last_modified() {
    let v = UpstreamVersion {
        last_modified: Some(1000),
        ..Default::default()
    };
    assert!(!v.is_empty());
}

// === VersionCheckConfig ===

#[test]
fn test_version_check_config_custom() {
    let config = VersionCheckConfig {
        version_endpoint: "http://example.com/ver".to_string(),
        poll_interval: Duration::from_secs(60),
        timeout: Duration::from_secs(5),
        invalidate_on_change: false,
    };
    assert_eq!(config.version_endpoint, "http://example.com/ver");
    assert_eq!(config.poll_interval, Duration::from_secs(60));
    assert_eq!(config.timeout, Duration::from_secs(5));
    assert!(!config.invalidate_on_change);
}

// === HTTP-based tests for fetch_version / check_version / run ===

/// Start a mock server that returns the given status code and body for all requests.
async fn mock_version_server(
    status: u16,
    body: &'static str,
) -> (tokio::task::JoinHandle<()>, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{}", addr);

    let app = axum::Router::new().fallback(move || async move {
        (
            axum::http::StatusCode::from_u16(status).unwrap(),
            body.to_string(),
        )
    });

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    // Give server time to start
    tokio::time::sleep(Duration::from_millis(20)).await;

    (handle, url)
}

#[tokio::test]
async fn test_fetch_version_json() {
    let (_handle, url) = mock_version_server(
        200,
        r#"{"version":"v2","corpus_hash":"abc","doc_count":500,"last_modified":12345}"#,
    )
    .await;

    let cache = Arc::new(CacheStore::new(
        Duration::from_secs(60),
        Duration::from_secs(120),
        100,
    ));
    let config = VersionCheckConfig {
        version_endpoint: format!("{}/version", url),
        poll_interval: Duration::from_secs(300),
        timeout: Duration::from_secs(5),
        invalidate_on_change: true,
    };
    let cancel = CancellationToken::new();
    let worker = VersionCheckWorker::new(cache, config, cancel);

    let version = worker.fetch_version().await.unwrap();
    assert_eq!(version.version, Some("v2".to_string()));
    assert_eq!(version.corpus_hash, Some("abc".to_string()));
    assert_eq!(version.doc_count, Some(500));
    assert_eq!(version.last_modified, Some(12345));
}

#[tokio::test]
async fn test_fetch_version_plain_text() {
    let (_handle, url) = mock_version_server(200, "v1.2.3").await;

    let cache = Arc::new(CacheStore::new(
        Duration::from_secs(60),
        Duration::from_secs(120),
        100,
    ));
    let config = VersionCheckConfig {
        version_endpoint: format!("{}/version", url),
        timeout: Duration::from_secs(5),
        ..Default::default()
    };
    let cancel = CancellationToken::new();
    let worker = VersionCheckWorker::new(cache, config, cancel);

    let version = worker.fetch_version().await.unwrap();
    assert_eq!(version.version, Some("v1.2.3".to_string()));
}

#[tokio::test]
async fn test_fetch_version_error_status() {
    let (_handle, url) = mock_version_server(500, "Internal Server Error").await;

    let cache = Arc::new(CacheStore::new(
        Duration::from_secs(60),
        Duration::from_secs(120),
        100,
    ));
    let config = VersionCheckConfig {
        version_endpoint: format!("{}/version", url),
        timeout: Duration::from_secs(5),
        ..Default::default()
    };
    let cancel = CancellationToken::new();
    let worker = VersionCheckWorker::new(cache, config, cancel);

    assert!(worker.fetch_version().await.is_err());
}

#[tokio::test]
async fn test_fetch_version_empty_body() {
    let (_handle, url) = mock_version_server(200, "").await;

    let cache = Arc::new(CacheStore::new(
        Duration::from_secs(60),
        Duration::from_secs(120),
        100,
    ));
    let config = VersionCheckConfig {
        version_endpoint: format!("{}/version", url),
        timeout: Duration::from_secs(5),
        ..Default::default()
    };
    let cancel = CancellationToken::new();
    let worker = VersionCheckWorker::new(cache, config, cancel);

    assert!(worker.fetch_version().await.is_err());
}

#[tokio::test]
async fn test_check_version_detects_change() {
    // Server that always returns the same version
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{}/version", addr);

    let call_count = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let call_count_clone = call_count.clone();

    let app = axum::Router::new().fallback(move || {
        let cc = call_count_clone.clone();
        async move {
            let n = cc.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let body = if n == 0 {
                r#"{"version":"v1"}"#
            } else {
                r#"{"version":"v2"}"#
            };
            (axum::http::StatusCode::OK, body.to_string())
        }
    });

    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let cache = Arc::new(CacheStore::new(
        Duration::from_secs(60),
        Duration::from_secs(120),
        100,
    ));
    let config = VersionCheckConfig {
        version_endpoint: url,
        poll_interval: Duration::from_millis(50),
        timeout: Duration::from_secs(5),
        invalidate_on_change: true,
    };
    let cancel = CancellationToken::new();
    let worker = Arc::new(VersionCheckWorker::new(cache, config, cancel.clone()));

    // First call sets the initial version
    worker.check_version().await;
    assert_eq!(worker.current_version().version, Some("v1".to_string()));
    assert_eq!(worker.changes_detected(), 0);

    // Second call detects the change
    worker.check_version().await;
    assert_eq!(worker.current_version().version, Some("v2".to_string()));
    assert_eq!(worker.changes_detected(), 1);
}

#[tokio::test]
async fn test_check_version_no_change() {
    let (_handle, url) = mock_version_server(200, r#"{"version":"v1"}"#).await;

    let cache = Arc::new(CacheStore::new(
        Duration::from_secs(60),
        Duration::from_secs(120),
        100,
    ));
    let config = VersionCheckConfig {
        version_endpoint: format!("{}/version", url),
        timeout: Duration::from_secs(5),
        ..Default::default()
    };
    let cancel = CancellationToken::new();
    let worker = VersionCheckWorker::new(cache, config, cancel);

    // First call sets the version
    worker.check_version().await;
    // Second call: same version → no change
    worker.check_version().await;
    assert_eq!(worker.changes_detected(), 0);
}

#[tokio::test]
async fn test_version_worker_run_with_endpoint() {
    let (_handle, url) = mock_version_server(200, r#"{"version":"v1"}"#).await;

    let cache = Arc::new(CacheStore::new(
        Duration::from_secs(60),
        Duration::from_secs(120),
        100,
    ));
    let config = VersionCheckConfig {
        version_endpoint: format!("{}/version", url),
        poll_interval: Duration::from_millis(50),
        timeout: Duration::from_secs(5),
        invalidate_on_change: true,
    };
    let cancel = CancellationToken::new();
    let worker = Arc::new(VersionCheckWorker::new(cache, config, cancel.clone()));

    let worker_clone = worker.clone();
    let handle = tokio::spawn(async move {
        worker_clone.run().await;
    });

    // Let it run briefly then cancel
    tokio::time::sleep(Duration::from_millis(150)).await;
    cancel.cancel();

    let result = tokio::time::timeout(Duration::from_millis(500), handle).await;
    assert!(result.is_ok());

    // Should have fetched the version
    assert!(!worker.current_version().is_empty());
}
