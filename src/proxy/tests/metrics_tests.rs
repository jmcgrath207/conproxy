use super::*;

#[test]
fn test_metrics_new() {
    let m = ProxyMetrics::new();
    let snap = m.snapshot();
    assert_eq!(snap.requests_total, 0);
    assert_eq!(snap.cache_hits, 0);
}

#[test]
fn test_metrics_record_hit() {
    let m = ProxyMetrics::new();
    m.record_hit();
    m.record_hit();
    let snap = m.snapshot();
    assert_eq!(snap.requests_total, 2);
    assert_eq!(snap.cache_hits, 2);
    assert_eq!(snap.cache_hit_rate, 1.0);
}

#[test]
fn test_metrics_record_miss() {
    let m = ProxyMetrics::new();
    m.record_miss(MissReason::NotInCache);
    let snap = m.snapshot();
    assert_eq!(snap.requests_total, 1);
    assert_eq!(snap.cache_misses, 1);
    assert_eq!(snap.cache_hit_rate, 0.0);
}

#[test]
fn test_metrics_hit_rate() {
    let m = ProxyMetrics::new();
    m.record_hit();
    m.record_hit();
    m.record_miss(MissReason::NotInCache);
    let snap = m.snapshot();
    assert_eq!(snap.requests_total, 3);
    assert!((snap.cache_hit_rate - 0.666).abs() < 0.01);
}

#[test]
fn test_metrics_latency() {
    let m = ProxyMetrics::new();
    m.record_latency(Duration::from_millis(10));
    m.record_latency(Duration::from_millis(20));
    m.record_hit();
    m.record_hit();
    let snap = m.snapshot();
    assert_eq!(snap.latency_max_ms, 20.0);
    assert_eq!(snap.latency_min_ms, 10.0);
    assert_eq!(snap.latency_avg_ms, 15.0);
}

#[test]
fn test_metrics_reset() {
    let m = ProxyMetrics::new();
    m.record_hit();
    m.record_miss(MissReason::NotInCache);
    m.reset();
    let snap = m.snapshot();
    assert_eq!(snap.requests_total, 0);
    assert_eq!(snap.cache_hits, 0);
}

#[test]
fn test_miss_reason_tracking() {
    let m = ProxyMetrics::new();
    m.record_miss(MissReason::NotInCache);
    m.record_miss(MissReason::NotInCache);
    m.record_miss(MissReason::Expired);
    m.record_miss(MissReason::UpstreamError);
    m.record_miss(MissReason::CircuitOpen);

    let snap = m.snapshot();
    assert_eq!(snap.cache_misses, 5);
    assert_eq!(snap.miss_not_in_cache, 2);
    assert_eq!(snap.miss_expired, 1);
    assert_eq!(snap.miss_upstream_error, 1);
    assert_eq!(snap.miss_circuit_open, 1);
}

#[test]
fn test_miss_reason_reset() {
    let m = ProxyMetrics::new();
    m.record_miss(MissReason::NotInCache);
    m.record_miss(MissReason::Expired);
    m.reset();

    let snap = m.snapshot();
    assert_eq!(snap.cache_misses, 0);
    assert_eq!(snap.miss_not_in_cache, 0);
    assert_eq!(snap.miss_expired, 0);
    assert_eq!(snap.miss_upstream_error, 0);
    assert_eq!(snap.miss_circuit_open, 0);
}

#[test]
fn test_miss_reason_prometheus() {
    let m = ProxyMetrics::new();
    m.record_miss(MissReason::NotInCache);
    m.record_miss(MissReason::NotInCache);
    m.record_miss(MissReason::Expired);

    let snap = m.snapshot();
    let prom = snap.to_prometheus();
    assert!(prom.contains("conproxy_cache_miss_reasons_total{reason=\"not_in_cache\"} 2"));
    assert!(prom.contains("conproxy_cache_miss_reasons_total{reason=\"expired\"} 1"));
    assert!(prom.contains("conproxy_cache_miss_reasons_total{reason=\"upstream_error\"} 0"));
    assert!(prom.contains("conproxy_cache_miss_reasons_total{reason=\"circuit_open\"} 0"));
}

#[test]
fn test_latency_timer() {
    let m = ProxyMetrics::new();
    {
        let _timer = LatencyTimer::new(&m);
        std::thread::sleep(Duration::from_millis(5));
    }
    // Latency should have been recorded
    let snap = m.snapshot();
    assert!(snap.latency_max_ms >= 5.0);
}

#[test]
fn test_metrics_stale_frozen() {
    let m = ProxyMetrics::new();
    m.record_stale();
    m.record_frozen();
    let snap = m.snapshot();
    assert_eq!(snap.cache_stale, 1);
    assert_eq!(snap.cache_frozen, 1);
}

#[test]
fn test_metrics_upstream() {
    let m = ProxyMetrics::new();
    m.record_upstream_request();
    m.record_upstream_request();
    m.record_upstream_failure();
    let snap = m.snapshot();
    assert_eq!(snap.upstream_requests, 2);
    assert_eq!(snap.upstream_failures, 1);
    assert_eq!(snap.upstream_error_rate, 0.5);
}

#[test]
fn test_metrics_drift_levels() {
    let m = ProxyMetrics::new();
    m.record_drift(DriftLevel::None);
    m.record_drift(DriftLevel::Minor);
    m.record_drift(DriftLevel::Minor);
    m.record_drift(DriftLevel::Moderate);
    m.record_drift(DriftLevel::Significant);
    m.record_drift(DriftLevel::Major);

    let snap = m.snapshot();
    assert_eq!(snap.drift_none, 1);
    assert_eq!(snap.drift_minor, 2);
    assert_eq!(snap.drift_moderate, 1);
    assert_eq!(snap.drift_significant, 1);
    assert_eq!(snap.drift_major, 1);
    // Significant and Major drift trigger alerts
    assert_eq!(snap.drift_alerts, 2);
}

#[test]
fn test_metrics_integrity_and_schema() {
    let m = ProxyMetrics::new();
    m.record_integrity_failure();
    m.record_integrity_failure();
    m.record_schema_change();

    let snap = m.snapshot();
    assert_eq!(snap.integrity_failures, 2);
    assert_eq!(snap.schema_changes, 1);
}

#[test]
fn test_prometheus_format_basic() {
    let m = ProxyMetrics::new();
    m.record_hit();
    m.record_hit();
    m.record_miss(MissReason::NotInCache);
    m.record_upstream_request();
    m.record_latency(Duration::from_millis(50));

    let snap = m.snapshot();
    let prom = snap.to_prometheus();

    // Check format
    assert!(prom.contains("# HELP conproxy_requests_total"));
    assert!(prom.contains("# TYPE conproxy_requests_total counter"));
    assert!(prom.contains("conproxy_requests_total 3"));

    assert!(prom.contains("conproxy_cache_hits_total 2"));
    assert!(prom.contains("conproxy_cache_misses_total 1"));
    assert!(prom.contains("conproxy_upstream_requests_total 1"));

    // Check latency (in seconds)
    assert!(prom.contains("conproxy_latency_max_seconds 0.05"));

    // Check hit rate
    assert!(prom.contains("conproxy_cache_hit_rate"));
}

#[test]
fn test_prometheus_format_drift_labels() {
    let m = ProxyMetrics::new();
    m.record_drift(DriftLevel::Minor);
    m.record_drift(DriftLevel::Minor);
    m.record_drift(DriftLevel::Significant);

    let snap = m.snapshot();
    let prom = snap.to_prometheus();

    // Check drift observations with labels
    assert!(prom.contains("conproxy_drift_observations_total{level=\"none\"} 0"));
    assert!(prom.contains("conproxy_drift_observations_total{level=\"minor\"} 2"));
    assert!(prom.contains("conproxy_drift_observations_total{level=\"significant\"} 1"));
}

#[test]
fn test_prometheus_format_with_cache() {
    let m = ProxyMetrics::new();
    m.record_hit();
    let snap = m.snapshot();

    let prom = snap.to_prometheus_with_cache(150, 1024 * 1024, 1000);

    // Check cache metrics
    assert!(prom.contains("conproxy_cache_entries 150"));
    assert!(prom.contains("conproxy_cache_memory_bytes 1048576"));
    assert!(prom.contains("conproxy_cache_max_entries 1000"));
    assert!(prom.contains("conproxy_cache_utilization 0.15"));
}

#[test]
fn test_prometheus_format_empty_metrics() {
    let m = ProxyMetrics::new();
    let snap = m.snapshot();
    let prom = snap.to_prometheus();

    // Should have valid format even with all zeros
    assert!(prom.contains("conproxy_requests_total 0"));
    assert!(prom.contains("conproxy_cache_hit_rate 0"));
    // Min latency should be 0 when no requests
    assert!(prom.contains("conproxy_latency_min_seconds 0"));
}

#[test]
fn test_prometheus_format_no_newline_issues() {
    let m = ProxyMetrics::new();
    m.record_hit();
    let snap = m.snapshot();
    let prom = snap.to_prometheus();

    // Each metric line should end with a value, not a newline character in the value
    for line in prom.lines() {
        if line.starts_with("conproxy_") && !line.starts_with("# ") {
            // Should have a space followed by a number
            let parts: Vec<&str> = line.split(' ').collect();
            assert!(
                parts.len() >= 2,
                "Line should have metric and value: {}",
                line
            );
        }
    }
}

#[test]
fn test_prometheus_format_with_pool() {
    let m = ProxyMetrics::new();
    m.record_hit();
    m.record_upstream_request();
    let snap = m.snapshot();

    let pool_params = PoolMetricsParams {
        total_upstreams: 5,
        healthy_upstreams: 4,
        fts_count: 2,
        vector_db_count: 2,
        hybrid_count: 1,
        unknown_count: 0,
        active_connections: 10,
        pool_utilization: 0.25,
    };
    let prom = snap.to_prometheus_with_pool(
        100,        // cache_entries
        512 * 1024, // cache_memory_bytes
        1000,       // cache_max_entries
        &pool_params,
    );

    // Check pool metrics
    assert!(prom.contains("conproxy_pool_upstreams_total 5"));
    assert!(prom.contains("conproxy_pool_upstreams_healthy 4"));

    // Check type labels
    assert!(prom.contains("conproxy_pool_upstreams_by_type{type=\"fts\"} 2"));
    assert!(prom.contains("conproxy_pool_upstreams_by_type{type=\"vector_db\"} 2"));
    assert!(prom.contains("conproxy_pool_upstreams_by_type{type=\"hybrid\"} 1"));
    assert!(prom.contains("conproxy_pool_upstreams_by_type{type=\"unknown\"} 0"));

    // Check connection pool metrics
    assert!(prom.contains("conproxy_pool_active_connections 10"));
    assert!(prom.contains("conproxy_pool_utilization 0.25"));

    // Should also include cache metrics from parent method
    assert!(prom.contains("conproxy_cache_entries 100"));
}

// ========================================================================
// Latency percentile ring buffer tests
// ========================================================================

#[test]
fn test_latency_percentiles_empty() {
    let m = ProxyMetrics::new();
    let pcts = m.latency_percentiles();
    assert_eq!(pcts.sample_count, 0);
    assert_eq!(pcts.p50_ms, 0.0);
    assert_eq!(pcts.p95_ms, 0.0);
    assert_eq!(pcts.p99_ms, 0.0);
}

#[test]
fn test_latency_percentiles_single_sample() {
    let m = ProxyMetrics::new();
    m.record_hit();
    m.record_latency(Duration::from_millis(42));
    let pcts = m.latency_percentiles();
    assert_eq!(pcts.sample_count, 1);
    assert!((pcts.p50_ms - 42.0).abs() < 0.1);
    assert!((pcts.p99_ms - 42.0).abs() < 0.1);
}

#[test]
fn test_latency_percentiles_multiple_samples() {
    let m = ProxyMetrics::new();
    // Record 100 samples from 1ms to 100ms
    for i in 1..=100 {
        m.record_hit();
        m.record_latency(Duration::from_millis(i));
    }
    let pcts = m.latency_percentiles();
    assert_eq!(pcts.sample_count, 100);
    // P50 should be ~50ms
    assert!((pcts.p50_ms - 50.0).abs() < 2.0);
    // P95 should be ~95ms
    assert!((pcts.p95_ms - 95.0).abs() < 2.0);
    // P99 should be ~99ms or 100ms
    assert!(pcts.p99_ms >= 99.0);
}

#[test]
fn test_latency_percentiles_ring_buffer_overflow() {
    let m = ProxyMetrics::new();
    // Write more than LATENCY_RING_SIZE samples
    for i in 0..2000 {
        m.record_hit();
        m.record_latency(Duration::from_millis(i + 1));
    }
    let pcts = m.latency_percentiles();
    // sample_count should be 2000 (total ever), ring has last 1000
    assert_eq!(pcts.sample_count, 2000);
    // The ring buffer should contain samples 1001..=2000
    // P50 of 1001..=2000 ≈ 1500ms
    assert!(pcts.p50_ms > 1000.0);
}

#[test]
fn test_latency_percentiles_reset() {
    let m = ProxyMetrics::new();
    m.record_hit();
    m.record_latency(Duration::from_millis(10));
    assert_eq!(m.latency_percentiles().sample_count, 1);

    m.reset();
    let pcts = m.latency_percentiles();
    assert_eq!(pcts.sample_count, 0);
    assert_eq!(pcts.p50_ms, 0.0);
}

// ========================================================================
// Prometheus append method tests
// ========================================================================

#[test]
fn test_append_adaptive_timeout_prometheus_with_samples() {
    let stats = super::super::adaptive::AdaptiveTimeoutStats {
        sample_count: 50,
        min_latency_ms: 5.0,
        max_latency_ms: 200.0,
        avg_latency_ms: 50.0,
        p50_latency_ms: 40.0,
        p95_latency_ms: 150.0,
        p99_latency_ms: 190.0,
        current_timeout_ms: 400.0,
    };
    let mut out = String::new();
    MetricsSnapshot::append_adaptive_timeout_prometheus(&mut out, &stats);

    assert!(out.contains("conproxy_adaptive_timeout_seconds 0.4"));
    assert!(out.contains("conproxy_adaptive_timeout_samples 50"));
    assert!(out.contains("conproxy_upstream_latency_seconds{quantile=\"0.5\"} 0.04"));
    assert!(out.contains("conproxy_upstream_latency_seconds{quantile=\"0.95\"} 0.15"));
    assert!(out.contains("conproxy_upstream_latency_seconds{quantile=\"0.99\"} 0.19"));
}

#[test]
fn test_append_adaptive_timeout_prometheus_no_samples() {
    let stats = super::super::adaptive::AdaptiveTimeoutStats {
        sample_count: 0,
        min_latency_ms: 0.0,
        max_latency_ms: 0.0,
        avg_latency_ms: 0.0,
        p50_latency_ms: 0.0,
        p95_latency_ms: 0.0,
        p99_latency_ms: 0.0,
        current_timeout_ms: 5000.0,
    };
    let mut out = String::new();
    MetricsSnapshot::append_adaptive_timeout_prometheus(&mut out, &stats);

    // Should still have timeout and sample count, but no quantiles
    assert!(out.contains("conproxy_adaptive_timeout_seconds 5"));
    assert!(out.contains("conproxy_adaptive_timeout_samples 0"));
    assert!(!out.contains("quantile"));
}

#[test]
fn test_append_circuit_breaker_prometheus() {
    let mut out = String::new();
    MetricsSnapshot::append_circuit_breaker_prometheus(&mut out, 0, 2, 1, 5);

    assert!(out.contains("conproxy_circuit_state 0"));
    assert!(out.contains("conproxy_circuit_failure_count 2"));
    assert!(out.contains("conproxy_circuit_times_opened_total 1"));
    assert!(out.contains("conproxy_circuit_times_tripped_total 5"));
}

#[test]
fn test_append_per_upstream_prometheus() {
    let upstreams = vec![
        (
            "qdrant-1".to_string(),
            100u64,
            5u64,
            0.05,
            "online".to_string(),
        ),
        (
            "es-1".to_string(),
            200u64,
            20u64,
            0.10,
            "degraded".to_string(),
        ),
    ];
    let mut out = String::new();
    MetricsSnapshot::append_per_upstream_prometheus(&mut out, &upstreams);

    assert!(out.contains("conproxy_upstream_requests_by_upstream{upstream=\"qdrant-1\"} 100"));
    assert!(out.contains("conproxy_upstream_requests_by_upstream{upstream=\"es-1\"} 200"));
    assert!(out.contains("conproxy_upstream_failures_by_upstream{upstream=\"qdrant-1\"} 5"));
    assert!(out.contains("conproxy_upstream_failures_by_upstream{upstream=\"es-1\"} 20"));
    assert!(out.contains("conproxy_upstream_status{upstream=\"qdrant-1\"} 2")); // online=2
    assert!(out.contains("conproxy_upstream_status{upstream=\"es-1\"} 1"));
    // degraded=1
}

#[test]
fn test_append_per_upstream_prometheus_empty() {
    let mut out = String::new();
    MetricsSnapshot::append_per_upstream_prometheus(&mut out, &[]);
    assert!(out.is_empty());
}

#[test]
fn test_append_throughput_prometheus() {
    let mut out = String::new();
    MetricsSnapshot::append_throughput_prometheus(&mut out, 100, 500);

    assert!(out.contains("conproxy_requests_per_second 5.00"));
}

#[test]
fn test_append_throughput_prometheus_zero_uptime() {
    let mut out = String::new();
    MetricsSnapshot::append_throughput_prometheus(&mut out, 0, 500);

    assert!(out.contains("conproxy_requests_per_second 0.00"));
}

#[test]
fn test_append_latency_percentiles_prometheus() {
    let pcts = LatencyPercentiles {
        sample_count: 100,
        p50_ms: 10.0,
        p95_ms: 50.0,
        p99_ms: 95.0,
    };
    let mut out = String::new();
    MetricsSnapshot::append_latency_percentiles_prometheus(&mut out, &pcts);

    assert!(out.contains("conproxy_request_latency_seconds{quantile=\"0.5\"} 0.01"));
    assert!(out.contains("conproxy_request_latency_seconds{quantile=\"0.95\"} 0.05"));
    assert!(out.contains("conproxy_request_latency_seconds{quantile=\"0.99\"} 0.095"));
}

#[test]
fn test_append_latency_percentiles_prometheus_empty() {
    let pcts = LatencyPercentiles {
        sample_count: 0,
        p50_ms: 0.0,
        p95_ms: 0.0,
        p99_ms: 0.0,
    };
    let mut out = String::new();
    MetricsSnapshot::append_latency_percentiles_prometheus(&mut out, &pcts);
    assert!(out.is_empty());
}

#[test]
fn test_warmup_counters_increment() {
    let metrics = ProxyMetrics::new();

    assert_eq!(metrics.warmup_requests.load(Ordering::Relaxed), 0);
    assert_eq!(metrics.warmup_entries.load(Ordering::Relaxed), 0);

    metrics.record_warmup_request();
    metrics.record_warmup_entry();
    metrics.record_warmup_entry();
    metrics.record_warmup_entry();

    assert_eq!(metrics.warmup_requests.load(Ordering::Relaxed), 1);
    assert_eq!(metrics.warmup_entries.load(Ordering::Relaxed), 3);
}

#[test]
fn test_warmup_counters_in_snapshot() {
    let metrics = ProxyMetrics::new();
    metrics.record_warmup_request();
    metrics.record_warmup_request();
    metrics.record_warmup_entry();
    metrics.record_warmup_entry();
    metrics.record_warmup_entry();
    metrics.record_warmup_entry();
    metrics.record_warmup_entry();

    let snap = metrics.snapshot();
    assert_eq!(snap.warmup_requests, 2);
    assert_eq!(snap.warmup_entries, 5);
}

#[test]
fn test_warmup_counters_reset() {
    let metrics = ProxyMetrics::new();
    metrics.record_warmup_request();
    metrics.record_warmup_entry();
    metrics.record_warmup_entry();

    assert_eq!(metrics.warmup_requests.load(Ordering::Relaxed), 1);
    assert_eq!(metrics.warmup_entries.load(Ordering::Relaxed), 2);

    metrics.reset();

    assert_eq!(metrics.warmup_requests.load(Ordering::Relaxed), 0);
    assert_eq!(metrics.warmup_entries.load(Ordering::Relaxed), 0);

    let snap = metrics.snapshot();
    assert_eq!(snap.warmup_requests, 0);
    assert_eq!(snap.warmup_entries, 0);
}

#[test]
fn test_warmup_counters_in_prometheus() {
    let metrics = ProxyMetrics::new();
    metrics.record_warmup_request();
    metrics.record_warmup_entry();
    metrics.record_warmup_entry();

    let snap = metrics.snapshot();
    let prom = snap.to_prometheus();

    assert!(prom.contains("conproxy_warmup_requests_total 1"));
    assert!(prom.contains("conproxy_warmup_entries_total 2"));
}

#[test]
fn test_context_stats_prometheus_output() {
    let contexts = vec![
        ContextMetricsEntry {
            id: "default".to_string(),
            hits: 42,
            misses: 10,
            hit_rate: 0.8077,
        },
        ContextMetricsEntry {
            id: "project-b".to_string(),
            hits: 5,
            misses: 15,
            hit_rate: 0.25,
        },
    ];

    let mut out = String::new();
    MetricsSnapshot::append_context_stats_prometheus(&mut out, &contexts);

    assert!(out.contains("conproxy_context_cache_hits_total{context=\"default\"} 42"));
    assert!(out.contains("conproxy_context_cache_hits_total{context=\"project-b\"} 5"));
    assert!(out.contains("conproxy_context_cache_misses_total{context=\"default\"} 10"));
    assert!(out.contains("conproxy_context_cache_misses_total{context=\"project-b\"} 15"));
    assert!(out.contains("conproxy_context_hit_rate{context=\"default\"} 0.8077"));
    assert!(out.contains("conproxy_context_hit_rate{context=\"project-b\"} 0.25"));
}

#[test]
fn test_context_stats_prometheus_empty() {
    let mut out = String::new();
    MetricsSnapshot::append_context_stats_prometheus(&mut out, &[]);
    assert!(out.is_empty());
}

#[test]
fn test_warmup_counters_zero_when_no_warmup() {
    let metrics = ProxyMetrics::new();
    // Record some normal requests but no warmup
    metrics.record_hit();
    metrics.record_miss(MissReason::NotInCache);

    let snap = metrics.snapshot();
    assert_eq!(snap.warmup_requests, 0);
    assert_eq!(snap.warmup_entries, 0);

    let prom = snap.to_prometheus();
    assert!(prom.contains("conproxy_warmup_requests_total 0"));
    assert!(prom.contains("conproxy_warmup_entries_total 0"));
}

#[test]
fn test_append_agent_metrics_prometheus() {
    let agents = vec![
        AgentMetricsEntry {
            id: "agent-a".to_string(),
            requests_total: 100,
            cache_hits: 80,
            cache_misses: 20,
            rate_limited: 5,
            context_denied: 2,
        },
        AgentMetricsEntry {
            id: "agent-b".to_string(),
            requests_total: 50,
            cache_hits: 30,
            cache_misses: 20,
            rate_limited: 0,
            context_denied: 0,
        },
    ];

    let mut out = String::new();
    MetricsSnapshot::append_agent_metrics_prometheus(&mut out, &agents);

    assert!(out.contains("conproxy_agent_requests_total{agent=\"agent-a\"} 100"));
    assert!(out.contains("conproxy_agent_requests_total{agent=\"agent-b\"} 50"));
    assert!(out.contains("conproxy_agent_cache_hits_total{agent=\"agent-a\"} 80"));
    assert!(out.contains("conproxy_agent_rate_limited_total{agent=\"agent-a\"} 5"));
    assert!(out.contains("conproxy_agent_context_denied_total{agent=\"agent-b\"} 0"));
}

#[test]
fn test_append_agent_metrics_empty() {
    let mut out = String::new();
    MetricsSnapshot::append_agent_metrics_prometheus(&mut out, &[]);
    assert!(out.is_empty());
}

#[test]
fn test_append_queue_prometheus() {
    use crate::proxy::priority::QueueStats;
    let stats = QueueStats {
        total: 15,
        max_size: 500,
        low_priority: 2,
        normal_priority: 8,
        high_priority: 3,
        critical_priority: 2,
    };

    let mut out = String::new();
    MetricsSnapshot::append_queue_prometheus(&mut out, &stats);

    assert!(out.contains("conproxy_queue_depth 15"));
    assert!(out.contains("conproxy_queue_max_size 500"));
    assert!(out.contains("conproxy_queue_by_priority{priority=\"low\"} 2"));
    assert!(out.contains("conproxy_queue_by_priority{priority=\"normal\"} 8"));
    assert!(out.contains("conproxy_queue_by_priority{priority=\"high\"} 3"));
    assert!(out.contains("conproxy_queue_by_priority{priority=\"critical\"} 2"));
    assert!(out.contains("# TYPE conproxy_queue_depth gauge"));
    assert!(out.contains("# TYPE conproxy_queue_max_size gauge"));
    assert!(out.contains("# TYPE conproxy_queue_by_priority gauge"));
}

#[test]
fn test_append_queue_prometheus_empty() {
    use crate::proxy::priority::QueueStats;
    let stats = QueueStats::default();

    let mut out = String::new();
    MetricsSnapshot::append_queue_prometheus(&mut out, &stats);

    assert!(out.contains("conproxy_queue_depth 0"));
    assert!(out.contains("conproxy_queue_by_priority{priority=\"low\"} 0"));
}

#[test]
fn test_append_client_tracker_prometheus() {
    let mut out = String::new();
    MetricsSnapshot::append_client_tracker_prometheus(&mut out, 5, 1000, 42);

    assert!(out.contains("conproxy_clients_active 5"));
    assert!(out.contains("conproxy_clients_completed_total 1000"));
    assert!(out.contains("conproxy_clients_rejected_total 42"));
    assert!(out.contains("# TYPE conproxy_clients_active gauge"));
    assert!(out.contains("# TYPE conproxy_clients_completed_total counter"));
    assert!(out.contains("# TYPE conproxy_clients_rejected_total counter"));
}

#[test]
fn test_append_client_tracker_prometheus_zeros() {
    let mut out = String::new();
    MetricsSnapshot::append_client_tracker_prometheus(&mut out, 0, 0, 0);

    assert!(out.contains("conproxy_clients_active 0"));
    assert!(out.contains("conproxy_clients_completed_total 0"));
    assert!(out.contains("conproxy_clients_rejected_total 0"));
}
