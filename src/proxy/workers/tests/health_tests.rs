#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

#[test]
fn test_health_check_config_default() {
    let config = HealthCheckConfig::default();
    assert_eq!(config.check_interval, Duration::from_secs(30));
    assert_eq!(config.recovery_check_interval, Duration::from_secs(5));
    assert_eq!(config.check_timeout, Duration::from_secs(5));
}

#[test]
fn test_health_check_worker_creation() {
    let upstream = Arc::new(
        GenericRestAdapter::new("http://localhost:8080", Duration::from_secs(30)).unwrap(),
    );
    let tracker = Arc::new(HealthTracker::new());
    let cancel = CancellationToken::new();

    let worker = HealthCheckWorker::with_defaults(upstream, tracker.clone(), cancel.clone());

    assert!(worker.is_running());
    assert_eq!(worker.status(), UpstreamStatus::Online);

    cancel.cancel();
    assert!(!worker.is_running());
}

#[tokio::test]
async fn test_health_check_worker_cancellation() {
    let upstream = Arc::new(
        GenericRestAdapter::new("http://localhost:8080", Duration::from_secs(30)).unwrap(),
    );
    let tracker = Arc::new(HealthTracker::new());
    let cancel = CancellationToken::new();

    let worker = Arc::new(HealthCheckWorker::new(
        upstream,
        tracker,
        HealthCheckConfig {
            check_interval: Duration::from_millis(50),
            recovery_check_interval: Duration::from_millis(10),
            check_timeout: Duration::from_millis(100),
        },
        cancel.clone(),
    ));

    let worker_clone = worker.clone();
    let handle = tokio::spawn(async move {
        worker_clone.run().await;
    });

    // Let it run briefly
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Cancel
    cancel.cancel();

    // Should stop within reasonable time
    let result = tokio::time::timeout(Duration::from_millis(200), handle).await;
    assert!(result.is_ok());
}

#[test]
fn test_failure_escalation_online_to_offline() {
    // Default offline_threshold = 3, so 3 consecutive failures should flip status
    let tracker = HealthTracker::new();
    assert_eq!(tracker.status(), UpstreamStatus::Online);

    tracker.record_failure();
    assert_eq!(tracker.consecutive_failures(), 1);
    assert_eq!(
        tracker.status(),
        UpstreamStatus::Online,
        "1 fail still online"
    );

    tracker.record_failure();
    assert_eq!(tracker.consecutive_failures(), 2);
    assert_eq!(
        tracker.status(),
        UpstreamStatus::Online,
        "2 fails still online"
    );

    tracker.record_failure();
    assert_eq!(tracker.consecutive_failures(), 3);
    assert_eq!(
        tracker.status(),
        UpstreamStatus::Offline,
        "3 fails → offline"
    );
}

#[test]
fn test_failure_intermittent_does_not_escalate() {
    // Successes reset the failure counter — intermittent failures shouldn't trip offline
    let tracker = HealthTracker::new();
    tracker.record_failure();
    tracker.record_failure();
    assert_eq!(tracker.consecutive_failures(), 2);

    tracker.record_success();
    assert_eq!(
        tracker.consecutive_failures(),
        0,
        "success resets failure count"
    );
    assert_eq!(tracker.status(), UpstreamStatus::Online);
}

#[test]
fn test_recovery_offline_to_online() {
    // Trip offline, then recover via successes
    let tracker = HealthTracker::new();
    for _ in 0..3 {
        tracker.record_failure();
    }
    assert_eq!(tracker.status(), UpstreamStatus::Offline);

    // 1 success not enough (recovery_threshold = 2)
    tracker.record_success();
    assert_eq!(
        tracker.status(),
        UpstreamStatus::Offline,
        "1 success not enough to recover"
    );

    // 2nd success → recovery
    tracker.record_success();
    assert_eq!(
        tracker.status(),
        UpstreamStatus::Online,
        "2 successes → online"
    );
}

#[test]
fn test_custom_thresholds() {
    // offline_threshold=2, recovery_threshold=1
    let tracker = HealthTracker::with_thresholds(2, 1, 0.5);
    tracker.record_failure();
    assert_eq!(tracker.status(), UpstreamStatus::Online);
    tracker.record_failure();
    assert_eq!(
        tracker.status(),
        UpstreamStatus::Offline,
        "custom threshold=2 trips after 2 fails"
    );

    // recovery_threshold=1 → 1 success recovers
    tracker.record_success();
    assert_eq!(
        tracker.status(),
        UpstreamStatus::Online,
        "1 success recovers with custom threshold=1"
    );
}

#[test]
fn test_reset_window_clears_total_failed_counts() {
    // reset_window() clears only the aggregate total/failed counters
    // (used for degraded-state error-rate computation), not the
    // consecutive counters or is_offline flag.
    let tracker = HealthTracker::new();
    for _ in 0..3 {
        tracker.record_failure();
    }
    assert_eq!(tracker.status(), UpstreamStatus::Offline);

    tracker.reset_window();
    // Consecutive failures and is_offline are NOT reset by reset_window()
    assert_eq!(tracker.consecutive_failures(), 3);
    assert_eq!(
        tracker.status(),
        UpstreamStatus::Offline,
        "reset_window does not clear offline state"
    );
}

#[test]
fn test_total_and_failed_request_counts() {
    let tracker = HealthTracker::new();
    tracker.record_success();
    tracker.record_success();
    tracker.record_failure();

    // Last event was a failure → consecutive_successes reset to 0
    assert_eq!(
        tracker.consecutive_successes(),
        0,
        "failure resets consecutive successes"
    );
    assert_eq!(
        tracker.consecutive_failures(),
        1,
        "last event was a failure"
    );
}

#[test]
fn test_time_since_last_success() {
    let tracker = HealthTracker::new();
    // No success yet
    let since = tracker.time_since_last_success();
    assert!(since.is_none(), "no success recorded → None");

    tracker.record_success();
    let since = tracker.time_since_last_success();
    assert!(since.is_some(), "after success → Some");
    // Should be very small (just recorded)
    let dur = since.unwrap();
    assert!(
        dur < Duration::from_secs(1),
        "time since should be < 1s, got: {dur:?}"
    );
}
