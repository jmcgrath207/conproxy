use crate::helpers::client::E2eClient;
use crate::helpers::constants::{category_enabled, Suite};
use crate::helpers::report::TestReport;
use crate::run_test;

pub fn run(client: &E2eClient, suite: Suite, report: &mut TestReport) {
    if !category_enabled("metrics") {
        return;
    }

    // Probe query to populate latency data after operational reset
    if suite.has_text_upstreams() {
        let _ = client.query("metrics latency probe");
    }

    // Test 30: Metrics has latency_percentiles
    run_test!(report, "metrics", "Metrics: latency percentiles", {
        let (_, m) = client.metrics();
        assert!(
            !m["latency_percentiles"]["sample_count"].is_null(),
            "Missing latency_percentiles.sample_count"
        );
    });

    // Test 31: Adaptive timeout p50/p95
    run_test!(report, "metrics", "Metrics: adaptive timeout p50/p95", {
        let (_, m) = client.metrics();
        assert!(
            !m["adaptive_timeout"]["p50_latency_ms"].is_null()
                && !m["adaptive_timeout"]["p95_latency_ms"].is_null(),
            "Missing adaptive_timeout fields"
        );
    });

    // Test 32: Circuit breaker JSON structure
    run_test!(report, "metrics", "Circuit breaker JSON", {
        let (_, cb) = client.circuit();
        assert!(cb["state"].is_string(), "Missing circuit state");
        assert!(!cb["failure_count"].is_null(), "Missing failure_count");
        assert!(!cb["times_opened"].is_null(), "Missing times_opened");
    });

    // Test 33: Prometheus circuit breaker metric
    run_test!(report, "metrics", "Prometheus: circuit breaker", {
        let (_, prom) = client.prometheus();
        assert!(
            prom.contains("conproxy_circuit_state"),
            "Missing circuit_state metric"
        );
    });

    // Test 34: Prometheus per-upstream metrics
    run_test!(report, "metrics", "Prometheus: per-upstream", {
        let (_, prom) = client.prometheus();
        assert!(
            prom.contains("conproxy_upstream_requests_by_upstream"),
            "Missing per-upstream metric"
        );
    });

    // Test 35: Prometheus throughput gauge
    run_test!(report, "metrics", "Prometheus: throughput", {
        let (_, prom) = client.prometheus();
        assert!(
            prom.contains("conproxy_requests_per_second"),
            "Missing throughput metric"
        );
    });

    // Test 36: Prometheus adaptive timeout
    run_test!(report, "metrics", "Prometheus: adaptive timeout", {
        let (_, prom) = client.prometheus();
        assert!(
            prom.contains("conproxy_adaptive_timeout_seconds"),
            "Missing adaptive_timeout metric"
        );
    });

    // Test 37: Prometheus latency percentiles
    run_test!(report, "metrics", "Prometheus: latency percentiles", {
        let (_, prom) = client.prometheus();
        assert!(
            prom.contains("conproxy_request_latency_seconds"),
            "Missing latency percentiles metric"
        );
    });

    // Test 38: Prometheus HELP annotations (at least 10)
    run_test!(report, "metrics", "Prometheus: HELP annotations", {
        let (_, prom) = client.prometheus();
        let help_count = prom.lines().filter(|l| l.starts_with("# HELP")).count();
        assert!(
            help_count >= 10,
            "Expected >= 10 HELP annotations, got {help_count}"
        );
    });

    // Test 39: Health has degradation_level
    run_test!(report, "metrics", "Health: degradation level", {
        let (_, h) = client.health();
        assert!(
            !h["degradation_level"].is_null(),
            "Missing degradation_level"
        );
    });

    // Test 40: Pool has per-upstream failure_rate
    run_test!(report, "metrics", "Pool: upstream failure rate", {
        let (_, pool) = client.pool();
        if let Some(upstreams) = pool["upstreams"].as_array() {
            if !upstreams.is_empty() {
                assert!(
                    !upstreams[0]["failure_rate"].is_null(),
                    "Missing failure_rate on first upstream"
                );
            }
        }
    });
}
