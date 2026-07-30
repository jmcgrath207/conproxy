use super::*;

#[test]
fn test_initial_state_healthy() {
    let mgr = UpstreamStateManager::with_defaults();
    assert_eq!(mgr.state(), UpstreamState::Healthy);
    assert!(mgr.allow_request());
    assert_eq!(mgr.current_rps_limit(), f32::MAX);
    assert!(mgr.retry_delay(0).is_none());
}

#[test]
fn test_transitions_to_degraded_after_failures() {
    let config = ResilienceConfig {
        degraded_after_failures: 3,
        ..Default::default()
    };
    let mgr = UpstreamStateManager::new(config);

    mgr.record_failure();
    assert_eq!(mgr.state(), UpstreamState::Healthy);
    mgr.record_failure();
    assert_eq!(mgr.state(), UpstreamState::Healthy);
    mgr.record_failure();
    assert_eq!(mgr.state(), UpstreamState::Degraded);
    assert!(mgr.allow_request());
    assert_ne!(mgr.current_rps_limit(), f32::MAX);
}

#[test]
fn test_transitions_to_offline_after_consecutive() {
    let config = ResilienceConfig {
        degraded_after_failures: 2,
        offline_after_consecutive: 3,
        ..Default::default()
    };
    let mgr = UpstreamStateManager::new(config);

    mgr.record_failure();
    mgr.record_failure();
    assert_eq!(mgr.state(), UpstreamState::Degraded);

    mgr.record_failure();
    assert_eq!(mgr.state(), UpstreamState::Offline);
    assert!(!mgr.allow_request());
    assert_eq!(mgr.current_rps_limit(), 0.0);
}

#[test]
fn test_degrades_then_recovers_to_healthy() {
    let config = ResilienceConfig {
        degraded_after_failures: 2,
        healthy_after_successes: 3,
        ..Default::default()
    };
    let mgr = UpstreamStateManager::new(config);

    mgr.record_failure();
    mgr.record_failure();
    assert_eq!(mgr.state(), UpstreamState::Degraded);

    mgr.record_success();
    mgr.record_success();
    assert_eq!(mgr.state(), UpstreamState::Degraded);
    mgr.record_success();
    assert_eq!(mgr.state(), UpstreamState::Healthy);
    assert_eq!(mgr.current_rps_limit(), f32::MAX);
}

#[test]
fn test_offline_recovers_through_degraded() {
    let config = ResilienceConfig {
        degraded_after_failures: 1,
        offline_after_consecutive: 2,
        healthy_after_successes: 2,
        ..Default::default()
    };
    let mgr = UpstreamStateManager::new(config);

    mgr.record_failure();
    assert_eq!(mgr.state(), UpstreamState::Degraded);
    mgr.record_failure();
    assert_eq!(mgr.state(), UpstreamState::Offline);

    // Probe success → Degraded (not directly Healthy)
    mgr.record_success();
    assert_eq!(mgr.state(), UpstreamState::Degraded);

    mgr.record_success();
    mgr.record_success();
    assert_eq!(mgr.state(), UpstreamState::Healthy);
}

#[test]
fn test_allow_request_per_state() {
    let config = ResilienceConfig {
        degraded_after_failures: 1,
        offline_after_consecutive: 2,
        ..Default::default()
    };
    let mgr = UpstreamStateManager::new(config);

    assert!(mgr.allow_request());
    mgr.record_failure();
    assert!(mgr.allow_request()); // Degraded
    mgr.record_failure();
    assert!(!mgr.allow_request()); // Offline
}

#[test]
fn test_rps_limit_increases_with_success() {
    let config = ResilienceConfig {
        degraded_after_failures: 1,
        healthy_after_successes: 100,
        degraded_rps_limit: 10.0,
        rps_increment: 5.0,
        ..Default::default()
    };
    let mgr = UpstreamStateManager::new(config);

    mgr.record_failure();
    assert!((mgr.current_rps_limit() - 10.0).abs() < 0.01);

    mgr.record_success();
    assert!((mgr.current_rps_limit() - 15.0).abs() < 0.01);

    mgr.record_success();
    assert!((mgr.current_rps_limit() - 20.0).abs() < 0.01);
}

#[test]
fn test_retry_delay_exponential_backoff() {
    let config = ResilienceConfig {
        degraded_after_failures: 1,
        max_retries: 4,
        retry_base_delay: Duration::from_millis(100),
        retry_max_delay: Duration::from_secs(10),
        ..Default::default()
    };
    let mgr = UpstreamStateManager::new(config);

    assert!(mgr.retry_delay(0).is_none()); // Healthy

    mgr.record_failure();
    assert_eq!(mgr.retry_delay(0), Some(Duration::from_millis(100)));
    assert_eq!(mgr.retry_delay(1), Some(Duration::from_millis(200)));
    assert_eq!(mgr.retry_delay(2), Some(Duration::from_millis(400)));
    assert_eq!(mgr.retry_delay(3), Some(Duration::from_millis(800)));
    assert!(mgr.retry_delay(4).is_none()); // Beyond max_retries
}

#[test]
fn test_force_offline_and_healthy() {
    let mgr = UpstreamStateManager::with_defaults();

    mgr.force_offline();
    assert_eq!(mgr.state(), UpstreamState::Offline);
    assert!(!mgr.allow_request());

    mgr.force_healthy();
    assert_eq!(mgr.state(), UpstreamState::Healthy);
    assert!(mgr.allow_request());
}

#[test]
fn test_failure_window_reset() {
    let config = ResilienceConfig {
        degraded_after_failures: 3,
        failure_window: Duration::from_millis(50),
        ..Default::default()
    };
    let mgr = UpstreamStateManager::new(config);

    mgr.record_failure();
    mgr.record_failure();
    assert_eq!(mgr.state(), UpstreamState::Healthy);

    std::thread::sleep(Duration::from_millis(60));

    mgr.record_failure(); // New window, count=1
    assert_eq!(mgr.state(), UpstreamState::Healthy);
    mgr.record_failure();
    assert_eq!(mgr.state(), UpstreamState::Healthy);
    mgr.record_failure();
    assert_eq!(mgr.state(), UpstreamState::Degraded);
}

#[test]
fn test_snapshot_metrics() {
    let config = ResilienceConfig {
        degraded_after_failures: 2,
        ..Default::default()
    };
    let mgr = UpstreamStateManager::new(config);

    let snap = mgr.snapshot();
    assert_eq!(snap.state, UpstreamState::Healthy);
    assert_eq!(snap.transitions, 0);
    assert_eq!(snap.total_failures, 0);
    assert_eq!(snap.total_successes, 0);

    mgr.record_success();
    mgr.record_failure();
    mgr.record_failure();

    let snap = mgr.snapshot();
    assert_eq!(snap.state, UpstreamState::Degraded);
    assert_eq!(snap.transitions, 1);
    assert_eq!(snap.total_failures, 2);
    assert_eq!(snap.total_successes, 1);
}

#[test]
fn test_resilience_config_default() {
    let config = ResilienceConfig::default();
    assert_eq!(config.degraded_after_failures, 5);
    assert_eq!(config.offline_after_consecutive, 10);
    assert_eq!(config.healthy_after_successes, 3);
    assert_eq!(config.max_retries, 3);
    assert_eq!(config.retry_base_delay, Duration::from_millis(100));
}
