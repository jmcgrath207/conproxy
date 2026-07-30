#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

#[test]
fn test_recovery_config_default() {
    let config = RecoveryConfig::default();
    assert_eq!(config.initial_rps, 1.0);
    assert_eq!(config.max_rps, 50.0);
    assert_eq!(config.batch_size, 5);
}

#[test]
fn test_recovery_tracker_initial_state() {
    let config = RecoveryConfig::default();
    let tracker = RecoveryTracker::new(&config);

    assert_eq!(tracker.state(), RecoveryState::Stable);
    assert!(!tracker.is_recovering());
}

#[test]
fn test_recovery_tracker_start_recovery() {
    let config = RecoveryConfig::default();
    let tracker = RecoveryTracker::new(&config);

    tracker.start_recovery(config.initial_rps);

    assert_eq!(tracker.state(), RecoveryState::Recovering);
    assert!(tracker.is_recovering());
    assert_eq!(tracker.current_rps(), config.initial_rps);
}

#[test]
fn test_recovery_tracker_record_success() {
    let config = RecoveryConfig::default();
    let tracker = RecoveryTracker::new(&config);

    tracker.start_recovery(config.initial_rps);
    let initial_rps = tracker.current_rps();

    tracker.record_success(config.rps_increment, config.max_rps);

    assert!(tracker.current_rps() > initial_rps);
}

#[test]
fn test_recovery_tracker_record_failure() {
    let config = RecoveryConfig::default();
    let tracker = RecoveryTracker::new(&config);

    tracker.start_recovery(config.initial_rps);
    tracker.record_failure();

    assert_eq!(tracker.state(), RecoveryState::Paused);
}

#[test]
fn test_recovery_tracker_go_offline() {
    let config = RecoveryConfig::default();
    let tracker = RecoveryTracker::new(&config);

    tracker.start_recovery(config.initial_rps);
    tracker.go_offline();

    assert_eq!(tracker.state(), RecoveryState::Offline);
    assert!(!tracker.is_recovering());
}

#[test]
fn test_recovery_tracker_complete_recovery() {
    let config = RecoveryConfig::default();
    let tracker = RecoveryTracker::new(&config);

    tracker.start_recovery(config.initial_rps);
    tracker.complete_recovery();

    assert_eq!(tracker.state(), RecoveryState::Stable);
    assert!(!tracker.is_recovering());
}

#[test]
fn test_recovery_tracker_rps_capped_at_max() {
    let config = RecoveryConfig {
        initial_rps: 45.0,
        max_rps: 50.0,
        rps_increment: 10.0,
        ..Default::default()
    };
    let tracker = RecoveryTracker::new(&config);

    tracker.start_recovery(config.initial_rps);
    tracker.record_success(config.rps_increment, config.max_rps);

    assert_eq!(tracker.current_rps(), config.max_rps);
}

#[tokio::test]
async fn test_recovery_worker_creation() {
    let cache = Arc::new(CacheStore::new(
        Duration::from_secs(60),
        Duration::from_secs(120),
        100,
    ));
    let upstream = Arc::new(
        GenericRestAdapter::new("http://localhost:8080", Duration::from_secs(30)).unwrap(),
    );
    let health = Arc::new(HealthTracker::new());
    let query_map = Arc::new(dashmap::DashMap::new());
    let cancel = CancellationToken::new();

    let worker = RecoveryWorker::with_defaults(cache, upstream, health, query_map, cancel.clone());

    let tracker = worker.tracker();
    assert_eq!(tracker.state(), RecoveryState::Stable);
}

#[tokio::test]
async fn test_recovery_worker_cancellation() {
    let cache = Arc::new(CacheStore::new(
        Duration::from_secs(60),
        Duration::from_secs(120),
        100,
    ));
    let upstream = Arc::new(
        GenericRestAdapter::new("http://localhost:8080", Duration::from_secs(30)).unwrap(),
    );
    let health = Arc::new(HealthTracker::new());
    let query_map = Arc::new(dashmap::DashMap::new());
    let cancel = CancellationToken::new();

    let worker = Arc::new(RecoveryWorker::with_defaults(
        cache,
        upstream,
        health,
        query_map,
        cancel.clone(),
    ));

    let worker_clone = worker.clone();
    let handle = tokio::spawn(async move {
        worker_clone.run().await;
    });

    // Let it run briefly
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Cancel
    cancel.cancel();

    // Should stop within reasonable time
    let result = tokio::time::timeout(Duration::from_millis(200), handle).await;
    assert!(result.is_ok());
}

// === Additional RecoveryTracker tests ===

#[test]
fn test_recovery_tracker_all_states() {
    let config = RecoveryConfig::default();
    let tracker = RecoveryTracker::new(&config);

    // Initial: Stable
    assert_eq!(tracker.state(), RecoveryState::Stable);

    // Start recovery → Recovering
    tracker.start_recovery(1.0);
    assert_eq!(tracker.state(), RecoveryState::Recovering);

    // Record failure → Paused
    tracker.record_failure();
    assert_eq!(tracker.state(), RecoveryState::Paused);

    // Go offline → Offline
    tracker.go_offline();
    assert_eq!(tracker.state(), RecoveryState::Offline);

    // Complete recovery → Stable
    tracker.complete_recovery();
    assert_eq!(tracker.state(), RecoveryState::Stable);
}

#[test]
fn test_recovery_tracker_rps_fixed_point_precision() {
    let config = RecoveryConfig {
        initial_rps: 1.5,
        ..Default::default()
    };
    let tracker = RecoveryTracker::new(&config);

    tracker.start_recovery(1.5);
    // RPS stored as fixed-point (x100), so 1.5 → 150 → 1.5
    assert!((tracker.current_rps() - 1.5).abs() < 0.02);
}

#[test]
fn test_recovery_tracker_start_recovery_resets_batches() {
    let config = RecoveryConfig::default();
    let tracker = RecoveryTracker::new(&config);

    tracker.start_recovery(1.0);
    tracker.record_success(5.0, 50.0);
    tracker.record_success(5.0, 50.0);

    // Start again — should reset
    tracker.start_recovery(1.0);
    assert_eq!(tracker.current_rps(), 1.0);
    assert!(tracker.is_recovering());
}

#[test]
fn test_recovery_tracker_reaches_stability() {
    let config = RecoveryConfig {
        initial_rps: 1.0,
        max_rps: 50.0,
        rps_increment: 5.0,
        batch_delay: Duration::from_millis(100),
        stability_period: Duration::from_millis(300), // 3 batches needed
        ..Default::default()
    };
    let tracker = RecoveryTracker::new(&config);

    tracker.start_recovery(config.initial_rps);
    assert!(tracker.is_recovering());

    // Record enough successes to reach stability
    for _ in 0..10 {
        tracker.record_success(config.rps_increment, config.max_rps);
    }

    // Should have completed recovery
    assert_eq!(tracker.state(), RecoveryState::Stable);
    assert!(!tracker.is_recovering());
}

#[test]
fn test_recovery_config_custom() {
    let config = RecoveryConfig {
        initial_rps: 2.0,
        max_rps: 100.0,
        rps_increment: 10.0,
        batch_size: 10,
        batch_delay: Duration::from_millis(100),
        stability_period: Duration::from_secs(60),
    };
    assert_eq!(config.initial_rps, 2.0);
    assert_eq!(config.max_rps, 100.0);
    assert_eq!(config.batch_size, 10);
}

#[test]
fn test_recovery_worker_new_with_custom_config() {
    let cache = Arc::new(CacheStore::new(
        Duration::from_secs(60),
        Duration::from_secs(120),
        100,
    ));
    let upstream = Arc::new(
        GenericRestAdapter::new("http://localhost:8080", Duration::from_secs(30)).unwrap(),
    );
    let health = Arc::new(HealthTracker::new());
    let query_map = Arc::new(dashmap::DashMap::new());
    let cancel = CancellationToken::new();
    let config = RecoveryConfig {
        initial_rps: 5.0,
        max_rps: 100.0,
        ..Default::default()
    };

    let worker = RecoveryWorker::new(cache, upstream, health, query_map, config, cancel);
    let tracker = worker.tracker();
    assert_eq!(tracker.state(), RecoveryState::Stable);
}

// === Async mock-server tests for run() / process_recovery_batch() / refresh_entry() ===

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

fn mock_query_response() -> crate::proxy::types::QueryResponse {
    crate::proxy::types::QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "r1".to_string(),
            score: 0.9,
            content: "mock result".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: crate::proxy::types::CacheStatus::Miss,
        took_ms: 5,
        generated_at: None,
        miss_reason: None,
    }
}

#[tokio::test]
async fn test_recovery_batch_with_stale_entries() {
    use axum::{routing::post, Json, Router};

    let resp = mock_query_response();
    let app = Router::new().route(
        "/query",
        post(move || {
            let r = resp.clone();
            async move { Json(serde_json::to_value(&r).unwrap()) }
        }),
    );
    let (base_url, _handle) = start_mock_server(app).await;

    // Cache with very short fresh duration so entries become stale immediately
    let cache = Arc::new(CacheStore::new(
        Duration::from_millis(1),
        Duration::from_secs(60),
        100,
    ));
    let upstream = Arc::new(GenericRestAdapter::new(&base_url, Duration::from_secs(5)).unwrap());
    let health = Arc::new(HealthTracker::new());
    let query_map = Arc::new(dashmap::DashMap::new());

    // Insert an entry and record its hash in query_map
    let query = "test recovery query";
    let hash = CacheStore::hash_query(query);
    cache.insert(
        query,
        crate::proxy::types::QueryResponse {
            results: vec![],
            cache_status: crate::proxy::types::CacheStatus::Miss,
            took_ms: 1,
            generated_at: None,
            miss_reason: None,
        },
        "old-upstream".to_string(),
    );
    query_map.insert(hash, query.to_string());

    // Wait for entry to become stale
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!cache.get_stale_hashes().is_empty());

    let cancel = CancellationToken::new();
    let config = RecoveryConfig {
        batch_size: 10,
        batch_delay: Duration::from_millis(10),
        initial_rps: 100.0,
        max_rps: 100.0,
        rps_increment: 1.0,
        stability_period: Duration::from_millis(50),
    };

    let worker = Arc::new(RecoveryWorker::new(
        cache.clone(),
        upstream,
        health.clone(),
        query_map,
        config,
        cancel.clone(),
    ));

    // Simulate offline → online transition to trigger recovery
    // First go offline
    health.record_failure();
    health.record_failure();
    health.record_failure(); // exceeds default threshold → offline

    let worker_clone = worker.clone();
    let handle = tokio::spawn(async move {
        worker_clone.run().await;
    });

    // Let worker see offline state
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(worker.tracker().state(), RecoveryState::Offline);

    // Now recover: record enough successes
    health.record_success();
    health.record_success();

    // Let recovery process run
    tokio::time::sleep(Duration::from_millis(500)).await;

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(200), handle).await;

    // Entry should have been refreshed (has results now)
    let entry = cache.get(query);
    assert!(entry.is_some());
    let entry = entry.unwrap();
    assert!(
        !entry.response.results.is_empty(),
        "Entry should have been refreshed with mock results"
    );
}

#[tokio::test]
async fn test_recovery_batch_empty_stale() {
    use axum::{routing::post, Router};

    // Server that returns 200 but shouldn't be called (no stale entries)
    let app = Router::new().route("/query", post(|| async { "ok" }));
    let (base_url, _handle) = start_mock_server(app).await;

    // Cache with long fresh duration → no stale entries
    let cache = Arc::new(CacheStore::new(
        Duration::from_secs(3600),
        Duration::from_secs(7200),
        100,
    ));
    let upstream = Arc::new(GenericRestAdapter::new(&base_url, Duration::from_secs(5)).unwrap());
    let health = Arc::new(HealthTracker::new());
    let query_map = Arc::new(dashmap::DashMap::new());
    let cancel = CancellationToken::new();

    let config = RecoveryConfig {
        batch_delay: Duration::from_millis(10),
        stability_period: Duration::from_millis(50),
        ..Default::default()
    };

    let worker = Arc::new(RecoveryWorker::new(
        cache,
        upstream,
        health.clone(),
        query_map,
        config,
        cancel.clone(),
    ));

    // Go offline then online
    health.record_failure();
    health.record_failure();
    health.record_failure();

    let worker_clone = worker.clone();
    let handle = tokio::spawn(async move {
        worker_clone.run().await;
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Recover
    health.record_success();
    health.record_success();

    // Let process_recovery_batch run — no stale entries → complete_recovery()
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Tracker should be back to Stable (empty stale → complete_recovery)
    let state = worker.tracker().state();
    assert!(
        state == RecoveryState::Stable || !worker.tracker().is_recovering(),
        "Expected recovery complete with no stale entries, got {:?}",
        state
    );

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(200), handle).await;
}

#[tokio::test]
async fn test_recovery_batch_upstream_failure() {
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
    let health = Arc::new(HealthTracker::new());
    let query_map = Arc::new(dashmap::DashMap::new());

    // Insert entry that will go stale
    let query = "failing query";
    let hash = CacheStore::hash_query(query);
    cache.insert(
        query,
        crate::proxy::types::QueryResponse {
            results: vec![],
            cache_status: crate::proxy::types::CacheStatus::Miss,
            took_ms: 1,
            generated_at: None,
            miss_reason: None,
        },
        "old-upstream".to_string(),
    );
    query_map.insert(hash, query.to_string());

    tokio::time::sleep(Duration::from_millis(10)).await;

    let cancel = CancellationToken::new();
    let config = RecoveryConfig {
        batch_size: 10,
        batch_delay: Duration::from_millis(10),
        initial_rps: 100.0,
        max_rps: 100.0,
        rps_increment: 1.0,
        stability_period: Duration::from_millis(100),
    };

    let worker = Arc::new(RecoveryWorker::new(
        cache,
        upstream,
        health.clone(),
        query_map,
        config,
        cancel.clone(),
    ));

    // Go offline
    health.record_failure();
    health.record_failure();
    health.record_failure();

    let worker_clone = worker.clone();
    let handle = tokio::spawn(async move {
        worker_clone.run().await;
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Come back online
    health.record_success();
    health.record_success();

    // Let recovery attempt (will fail since upstream returns 500)
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Tracker should be Paused or Offline (failures feed back into health tracker,
    // which may push back to Offline before Paused state is observed)
    let state = worker.tracker().state();
    assert!(
        state == RecoveryState::Paused || state == RecoveryState::Offline,
        "Expected Paused or Offline after upstream failures, got {:?}",
        state
    );

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(200), handle).await;
}

#[tokio::test]
async fn test_recovery_run_offline_to_online() {
    use axum::{routing::post, Json, Router};

    let resp = mock_query_response();
    let app = Router::new().route(
        "/query",
        post(move || {
            let r = resp.clone();
            async move { Json(serde_json::to_value(&r).unwrap()) }
        }),
    );
    let (base_url, _handle) = start_mock_server(app).await;

    let cache = Arc::new(CacheStore::new(
        Duration::from_secs(3600),
        Duration::from_secs(7200),
        100,
    ));
    let upstream = Arc::new(GenericRestAdapter::new(&base_url, Duration::from_secs(5)).unwrap());
    let health = Arc::new(HealthTracker::new());
    let query_map = Arc::new(dashmap::DashMap::new());
    let cancel = CancellationToken::new();

    let worker = Arc::new(RecoveryWorker::new(
        cache,
        upstream,
        health.clone(),
        query_map,
        RecoveryConfig {
            batch_delay: Duration::from_millis(10),
            stability_period: Duration::from_millis(50),
            ..Default::default()
        },
        cancel.clone(),
    ));

    let worker_clone = worker.clone();
    let handle = tokio::spawn(async move {
        worker_clone.run().await;
    });

    // Initially stable
    assert_eq!(worker.tracker().state(), RecoveryState::Stable);

    // Go offline
    health.record_failure();
    health.record_failure();
    health.record_failure();

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(worker.tracker().state(), RecoveryState::Offline);

    // Come online
    health.record_success();
    health.record_success();

    tokio::time::sleep(Duration::from_millis(300)).await;

    // Should have started recovery (or completed it, since no stale entries)
    let state = worker.tracker().state();
    assert!(
        state == RecoveryState::Stable || state == RecoveryState::Recovering,
        "Expected Stable or Recovering after online transition, got {:?}",
        state
    );

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(200), handle).await;
}
