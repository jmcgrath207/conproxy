use crate::helpers::client::E2eClient;
use crate::helpers::constants::{category_enabled, SHARED_QUERIES};
use crate::helpers::report::TestReport;
use crate::run_test;
use std::time::Duration;

pub fn run(client: &E2eClient, report: &mut TestReport) {
    if !category_enabled("efficiency") {
        return;
    }

    eprintln!("\x1b[32m[INFO]\x1b[0m Running cache efficiency tests...");

    // Wait for upstream pool to recover from resilience tests
    eprintln!("\x1b[32m[INFO]\x1b[0m Waiting for upstream pool recovery...");
    for _ in 0..30 {
        let (_, pool) = client.pool();
        let healthy = pool["stats"]["healthy_upstreams"].as_u64().unwrap_or(0);
        if healthy >= 1 {
            eprintln!("\x1b[32m[INFO]\x1b[0m Pool recovered ({healthy} healthy upstreams)");
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }

    // Send probe queries to close circuit breaker
    for _ in 0..3 {
        let (status, _) = client.query("circuit recovery probe");
        if status == 200 {
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }

    // Clear cache and reset metrics
    client.cache_clear();
    client.admin_metrics_reset();

    // Warm all 10 shared queries
    run_test!(report, "efficiency", "Efficiency: warmup shared queries", {
        let (status, body) = client.warmup_with_fetch(&SHARED_QUERIES);
        assert_eq!(status, 200, "Warmup failed: {}", body);
        let warmed = body["warmed"].as_u64().unwrap_or(0);
        assert!(warmed >= 8, "Expected >= 8 warmed queries, got {warmed}");
    });

    // Re-query first 3 — should be cache hits
    run_test!(report, "efficiency", "Efficiency: hit Q1 after warmup", {
        let (_, body) = client.query(SHARED_QUERIES[0]);
        assert_eq!(
            body["cache_status"].as_str(),
            Some("hit"),
            "Q1 not a cache hit"
        );
    });

    run_test!(report, "efficiency", "Efficiency: hit Q2 after warmup", {
        let (_, body) = client.query(SHARED_QUERIES[1]);
        assert_eq!(
            body["cache_status"].as_str(),
            Some("hit"),
            "Q2 not a cache hit"
        );
    });

    run_test!(report, "efficiency", "Efficiency: hit Q3 after warmup", {
        let (_, body) = client.query(SHARED_QUERIES[2]);
        assert_eq!(
            body["cache_status"].as_str(),
            Some("hit"),
            "Q3 not a cache hit"
        );
    });

    // Hit rate >= 80%
    run_test!(report, "efficiency", "Efficiency: hit rate >= 80%", {
        let (_, metrics) = client.metrics();
        let hit_rate = metrics["proxy"]["cache_hit_rate"].as_f64().unwrap_or(0.0);
        assert!(hit_rate >= 0.8, "Expected hit rate >= 0.8, got {hit_rate}");
    });

    // Warmup entries tracked
    run_test!(
        report,
        "efficiency",
        "Efficiency: warmup entries tracked",
        {
            let (_, metrics) = client.metrics();
            let entries = metrics["proxy"]["warmup_entries"].as_u64().unwrap_or(0);
            assert!(entries >= 8, "Expected >= 8 warmup entries, got {entries}");
        }
    );

    // Warmup request counted
    run_test!(
        report,
        "efficiency",
        "Efficiency: warmup request counted",
        {
            let (_, metrics) = client.metrics();
            let requests = metrics["proxy"]["warmup_requests"].as_u64().unwrap_or(0);
            assert!(
                requests >= 1,
                "Expected >= 1 warmup request, got {requests}"
            );
        }
    );
}
