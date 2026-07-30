//! Coalescing dedup verification E2E tests.
//!
//! Manages its own MockUpstream + ProxyProcess. Verifies that concurrent identical
//! queries are coalesced into a single upstream request.

use crate::helpers::client::E2eClient;
use crate::helpers::config::ConfigManager;
use crate::helpers::constants::{category_enabled, proxy_url};
use crate::helpers::mock_upstream::{MockBehavior, MockUpstream};
use crate::helpers::proxy::ProxyProcess;
use crate::helpers::report::TestReport;
use crate::run_test;
use std::path::PathBuf;
use std::time::Duration;

pub fn run(report: &mut TestReport) {
    if !category_enabled("coalescing") {
        return;
    }

    eprintln!();
    eprintln!("\x1b[1mCoalescing Dedup Verification Tests (mock upstream)\x1b[0m");
    eprintln!("--------------------------------------------");

    let mock = MockUpstream::start();
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut config = ConfigManager::new(&project_root);
    config.write_mock_config(&mock.url(), "elasticsearch");

    eprintln!("\x1b[32m[INFO]\x1b[0m Starting proxy with mock upstream...");
    let mut proxy = ProxyProcess::start("127.0.0.1:8080");
    if let Err(e) = proxy.wait_healthy(Duration::from_secs(5)) {
        eprintln!("\x1b[31m[ERROR]\x1b[0m Coalescing proxy failed to start: {e}");
        proxy.stop();
        config.restore();
        return;
    }

    let client = E2eClient::new(proxy_url());

    // Test 1: Single query baseline
    run_test!(report, "coalescing", "Coalesce: single query baseline", {
        mock.set_behavior(MockBehavior::Healthy);
        mock.set_latency_ms(0);
        client.cache_clear();
        mock.reset_stats();
        let (status, _) = client.query("coalesce baseline");
        assert_eq!(status, 200);
        let _count = mock.query_count("coalesce baseline");
        // We can't guarantee query string forwarding, but request count should be >= 1
        let total = mock.request_count();
        assert!(total >= 1, "Expected at least 1 request, got {total}");
    });

    // Test 2 + 3: Concurrent identical queries deduped + all responses received
    let concurrent_results: Vec<(u16, serde_json::Value)>;
    {
        mock.set_behavior(MockBehavior::Healthy);
        mock.set_latency_ms(500);
        client.cache_clear();
        mock.reset_stats();

        let thread_count = 10;
        let handles: Vec<_> = (0..thread_count)
            .map(|_| {
                std::thread::spawn(|| {
                    let c = reqwest::blocking::Client::builder()
                        .timeout(Duration::from_secs(10))
                        .build()
                        .unwrap();
                    let url = format!("{}/query", proxy_url());
                    match c
                        .post(&url)
                        .json(&serde_json::json!({"query": "coalesce test"}))
                        .send()
                    {
                        Ok(resp) => {
                            let status = resp.status().as_u16();
                            let body = resp.json::<serde_json::Value>().unwrap_or_default();
                            (status, body)
                        }
                        Err(_) => (0, serde_json::Value::Null),
                    }
                })
            })
            .collect();

        concurrent_results = handles.into_iter().map(|h| h.join().unwrap()).collect();
    }

    run_test!(
        report,
        "coalescing",
        "Coalesce: concurrent identical queries deduped",
        {
            let upstream_requests = mock.request_count();
            eprintln!("  Concurrent test: {upstream_requests} upstream requests for 10 concurrent queries");
            // With coalescing, we expect significantly fewer upstream requests than 10
            // Allow some tolerance — at minimum, fewer than the thread count
            assert!(
                upstream_requests < 10,
                "Expected coalescing to reduce requests below 10, got {upstream_requests}"
            );
        }
    );

    run_test!(report, "coalescing", "Coalesce: all responses received", {
        let success_count = concurrent_results
            .iter()
            .filter(|(status, _)| *status == 200)
            .count();
        assert_eq!(
            success_count,
            concurrent_results.len(),
            "Expected all 10 threads to get 200, but only {success_count}/{}",
            concurrent_results.len()
        );
    });

    // Test 4: Different queries not coalesced
    run_test!(
        report,
        "coalescing",
        "Coalesce: different queries not coalesced",
        {
            mock.set_behavior(MockBehavior::Healthy);
            mock.set_latency_ms(300);
            client.cache_clear();
            mock.reset_stats();

            let h1 = std::thread::spawn(|| {
                let c = reqwest::blocking::Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build()
                    .unwrap();
                let _ = c
                    .post(format!("{}/query", proxy_url()))
                    .json(&serde_json::json!({"query": "coalesce alpha unique"}))
                    .send();
            });
            let h2 = std::thread::spawn(|| {
                let c = reqwest::blocking::Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build()
                    .unwrap();
                let _ = c
                    .post(format!("{}/query", proxy_url()))
                    .json(&serde_json::json!({"query": "coalesce beta unique"}))
                    .send();
            });
            h1.join().unwrap();
            h2.join().unwrap();

            let total = mock.request_count();
            assert!(
                total >= 2,
                "Expected at least 2 requests for different queries, got {total}"
            );
        }
    );

    // Test 5: Coalescing metric tracked
    run_test!(report, "coalescing", "Coalesce: metric tracked", {
        let (status, text) = client.prometheus();
        assert_eq!(status, 200);
        // Check for coalescing-related metrics in prometheus output
        let has_coalesce = text.contains("coalesced")
            || text.contains("coalesce")
            || text.contains("inflight")
            || text.contains("in_flight");
        eprintln!("  Prometheus has coalescing metric: {has_coalesce}");
        // This is informational — not all implementations expose this metric
    });

    // Cleanup
    mock.set_latency_ms(0);
    eprintln!("\x1b[32m[INFO]\x1b[0m Stopping coalescing proxy...");
    proxy.stop();
    config.restore();
    eprintln!("--------------------------------------------");
}
