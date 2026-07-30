use super::*;

#[test]
fn test_adaptive_timeout_creation() {
    let adaptive = AdaptiveTimeout::default_config();
    assert_eq!(adaptive.timeout(), Duration::from_secs(5));
    assert_eq!(adaptive.sample_count(), 0);
}

#[test]
fn test_adaptive_timeout_record() {
    let adaptive = AdaptiveTimeout::new(AdaptiveTimeoutConfig {
        sample_size: 10,
        ..Default::default()
    });

    for _ in 0..5 {
        adaptive.record(Duration::from_millis(100));
    }

    assert_eq!(adaptive.sample_count(), 5);
}

#[test]
fn test_adaptive_timeout_calculation() {
    let config = AdaptiveTimeoutConfig {
        min_timeout: Duration::from_millis(50),
        max_timeout: Duration::from_secs(10),
        latency_multiplier: 2.0,
        sample_size: 10,
        initial_timeout: Duration::from_secs(1),
    };
    let adaptive = AdaptiveTimeout::new(config);

    // Record consistent latencies
    for _ in 0..10 {
        adaptive.record(Duration::from_millis(100));
    }

    adaptive.recalculate();

    // p99 ≈ 100ms, timeout should be ~200ms (2x multiplier)
    let timeout = adaptive.timeout();
    assert!(timeout >= Duration::from_millis(150));
    assert!(timeout <= Duration::from_millis(250));
}

#[test]
fn test_adaptive_timeout_respects_bounds() {
    let config = AdaptiveTimeoutConfig {
        min_timeout: Duration::from_millis(500),
        max_timeout: Duration::from_secs(2),
        latency_multiplier: 2.0,
        sample_size: 10,
        initial_timeout: Duration::from_secs(1),
    };
    let adaptive = AdaptiveTimeout::new(config);

    // Record very fast latencies
    for _ in 0..10 {
        adaptive.record(Duration::from_millis(10));
    }
    adaptive.recalculate();

    // Should be clamped to min
    assert_eq!(adaptive.timeout(), Duration::from_millis(500));

    // Record very slow latencies
    for _ in 0..10 {
        adaptive.record(Duration::from_secs(5));
    }
    adaptive.recalculate();

    // Should be clamped to max
    assert_eq!(adaptive.timeout(), Duration::from_secs(2));
}

#[test]
fn test_adaptive_timeout_stats() {
    let config = AdaptiveTimeoutConfig {
        sample_size: 10,
        ..Default::default()
    };
    let adaptive = AdaptiveTimeout::new(config);

    for i in 1..=10 {
        adaptive.record(Duration::from_millis(i * 10));
    }

    let stats = adaptive.stats();
    assert_eq!(stats.sample_count, 10);
    assert!((stats.min_latency_ms - 10.0).abs() < 0.1);
    assert!((stats.max_latency_ms - 100.0).abs() < 0.1);
    assert!((stats.avg_latency_ms - 55.0).abs() < 1.0);
}

#[test]
fn test_adaptive_timeout_reset() {
    let adaptive = AdaptiveTimeout::default_config();

    for _ in 0..5 {
        adaptive.record(Duration::from_millis(100));
    }
    assert_eq!(adaptive.sample_count(), 5);

    adaptive.reset();
    assert_eq!(adaptive.sample_count(), 0);
    assert_eq!(adaptive.timeout(), Duration::from_secs(5)); // Initial timeout
}

#[test]
fn test_timeout_budget_creation() {
    let budget = TimeoutBudget::new(Duration::from_secs(5));
    assert!(!budget.is_expired());
    assert!(budget.remaining() <= Duration::from_secs(5));
    assert_eq!(budget.original(), Duration::from_secs(5));
}

#[test]
fn test_timeout_budget_expiration() {
    let budget = TimeoutBudget::new(Duration::from_millis(10));
    assert!(!budget.is_expired());

    std::thread::sleep(Duration::from_millis(15));
    assert!(budget.is_expired());
    assert_eq!(budget.remaining(), Duration::ZERO);
}

#[test]
fn test_timeout_budget_remaining_fraction() {
    let budget = TimeoutBudget::new(Duration::from_millis(100));
    let fraction = budget.remaining_fraction();
    assert!(fraction > 0.9);
    assert!(fraction <= 1.0);
}

#[test]
fn test_timeout_budget_from_adaptive() {
    let adaptive = AdaptiveTimeout::new(AdaptiveTimeoutConfig {
        initial_timeout: Duration::from_secs(3),
        ..Default::default()
    });

    let budget = TimeoutBudget::from_adaptive(&adaptive);
    assert_eq!(budget.original(), Duration::from_secs(3));
}

#[test]
fn test_adaptive_timeout_ring_buffer() {
    let config = AdaptiveTimeoutConfig {
        sample_size: 5,
        ..Default::default()
    };
    let adaptive = AdaptiveTimeout::new(config);

    // Fill beyond capacity
    for i in 0..10 {
        adaptive.record(Duration::from_millis(i * 10 + 10));
    }

    let _stats = adaptive.stats();
    // Should only have 5 samples
    let samples = adaptive.samples.read();
    assert_eq!(samples.len(), 5);
}
