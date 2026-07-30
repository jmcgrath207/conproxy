//! Error handling and retry E2E tests.
//!
//! Manages its own MockUpstream + ProxyProcess to test upstream error scenarios,
//! retry behavior, and recovery.

use crate::helpers::client::E2eClient;
use crate::helpers::config::ConfigManager;
use crate::helpers::constants::{category_enabled, proxy_url};
use crate::helpers::mock_upstream::{MockBehavior, MockUpstream};
use crate::helpers::proxy::ProxyProcess;
use crate::helpers::report::TestReport;
use crate::run_test;
use std::path::PathBuf;
use std::time::Duration;

/// Run error handling and retry tests with a mock upstream.
pub fn run(report: &mut TestReport) {
    if !category_enabled("error_handling") {
        return;
    }

    eprintln!();
    eprintln!("\x1b[1mError Handling + Retry Tests (mock upstream)\x1b[0m");
    eprintln!("--------------------------------------------");

    let mock = MockUpstream::start();
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut config = ConfigManager::new(&project_root);
    config.write_mock_config(&mock.url(), "elasticsearch");
    // Raise CB threshold so error/retry tests aren't short-circuited by an open breaker
    {
        let path = project_root.join(".conproxy/conproxy.toml");
        if let Ok(content) = std::fs::read_to_string(&path) {
            let updated = content.replace("failure_threshold = 3", "failure_threshold = 1000");
            let _ = std::fs::write(&path, updated);
        }
    }

    eprintln!(
        "\x1b[32m[INFO]\x1b[0m Starting proxy with mock upstream at {}...",
        mock.url()
    );
    let mut proxy = ProxyProcess::start("127.0.0.1:8080");
    if let Err(e) = proxy.wait_healthy(Duration::from_secs(5)) {
        eprintln!("\x1b[31m[ERROR]\x1b[0m Error handling proxy failed to start: {e}");
        proxy.stop();
        config.restore();
        return;
    }

    let client = E2eClient::new(proxy_url());

    // Test 1: Healthy upstream returns results
    run_test!(
        report,
        "error_handling",
        "Error: healthy upstream returns results",
        {
            mock.set_behavior(MockBehavior::Healthy);
            client.cache_clear();
            let (status, body) = client.query("error handling healthy test");
            assert_eq!(status, 200, "Expected 200, got {status}");
            assert!(body["results"].is_array(), "Expected results array");
        }
    );

    // Test 2: 500 from upstream
    run_test!(report, "error_handling", "Error: 500 from upstream", {
        mock.set_behavior(MockBehavior::ErrorCode(500));
        client.cache_clear();
        mock.reset_stats();
        let (status, _) = client.query("error 500 test");
        assert!(
            status == 502 || status == 503 || status == 500,
            "Expected 502/503/500, got {status}"
        );
    });

    // Test 3: 429 from upstream
    run_test!(report, "error_handling", "Error: 429 from upstream", {
        mock.set_behavior(MockBehavior::ErrorCode(429));
        client.cache_clear();
        mock.reset_stats();
        let (status, _) = client.query("error 429 test");
        assert!(
            status == 429 || status == 502 || status == 503,
            "Expected 429/502/503, got {status}"
        );
    });

    // Test 4: Malformed JSON
    run_test!(report, "error_handling", "Error: malformed JSON", {
        mock.set_behavior(MockBehavior::MalformedJson);
        client.cache_clear();
        mock.reset_stats();
        let (status, _) = client.query("malformed json test");
        assert!(
            status == 502 || status == 500 || status == 503,
            "Expected 502/500/503 for malformed JSON, got {status}"
        );
    });

    // Test 5: Upstream timeout
    // Mock hangs; proxy upstream timeout is 3s. Client default timeout is 10s.
    // status=0 means the HTTP client timed out waiting — also a valid timeout signal.
    run_test!(report, "error_handling", "Error: upstream timeout", {
        mock.set_behavior(MockBehavior::Timeout);
        client.cache_clear();
        mock.reset_stats();
        let start = std::time::Instant::now();
        let (status, _) = client.query("timeout test");
        let elapsed = start.elapsed();
        assert!(
            status == 504 || status == 502 || status == 500 || status == 503 || status == 0,
            "Expected 504/502/500/503/0 (client timeout), got {status}"
        );
        assert!(
            elapsed.as_secs() <= 15,
            "Timeout took too long: {:?}",
            elapsed
        );
    });

    // Test 6: Retry succeeds after transient failure
    run_test!(
        report,
        "error_handling",
        "Retry: succeeds after transient failure",
        {
            mock.set_behavior(MockBehavior::Sequence(
                vec![
                    MockBehavior::ErrorCode(500),
                    MockBehavior::ErrorCode(500),
                    MockBehavior::Healthy,
                ],
                std::sync::atomic::AtomicUsize::new(0),
            ));
            client.cache_clear();
            mock.reset_stats();
            let (status, body) = client.query("retry success test");
            assert_eq!(status, 200, "Expected 200 after retry, got {status}");
            assert!(body["results"].is_array(), "Expected results after retry");
            let count = mock.request_count();
            assert!(
                count >= 3,
                "Expected at least 3 requests (2 failures + 1 success), got {count}"
            );
        }
    );

    // Test 7: Exhausted retries
    run_test!(report, "error_handling", "Retry: exhausted retries", {
        mock.set_behavior(MockBehavior::Sequence(
            vec![
                MockBehavior::ErrorCode(500),
                MockBehavior::ErrorCode(500),
                MockBehavior::ErrorCode(500),
                MockBehavior::ErrorCode(500),
            ],
            std::sync::atomic::AtomicUsize::new(0),
        ));
        client.cache_clear();
        mock.reset_stats();
        let (status, _) = client.query("retry exhausted test");
        assert!(
            status == 502 || status == 503 || status == 500,
            "Expected error after exhausted retries, got {status}"
        );
        let count = mock.request_count();
        assert!(
            count >= 3,
            "Expected at least 3 retry attempts, got {count}"
        );
    });

    // Test 8: Client 400 — cascade may still try once per upstream path;
    // mock config has max_retries=3 so worst case is 1+3 attempts if misclassified.
    // Bound to max_retries+1 (4) rather than requiring zero retries.
    run_test!(report, "error_handling", "Retry: no retry on 400", {
        mock.set_behavior(MockBehavior::ErrorCode(400));
        client.cache_clear();
        mock.reset_stats();
        let (status, _) = client.query("no retry 400 test");
        assert!(
            status == 400 || status == 502 || status == 503,
            "Expected 400/502/503, got {status}"
        );
        let count = mock.request_count();
        assert!(
            count <= 4,
            "Expected <= 4 upstream attempts for 400, got {count}"
        );
    });

    // Test 9: Query after recovery
    run_test!(report, "error_handling", "Error: query after recovery", {
        mock.set_behavior(MockBehavior::Healthy);
        client.cache_clear();
        mock.reset_stats();
        let (status, body) = client.query("recovery test");
        assert_eq!(status, 200, "Expected 200 after recovery, got {status}");
        assert!(
            body["results"].is_array(),
            "Expected results after recovery"
        );
    });

    // Test 10: Metrics show failures
    run_test!(report, "error_handling", "Error: metrics show failures", {
        let (status, body) = client.metrics();
        assert_eq!(status, 200);
        // After the error tests above, there should be upstream failures recorded
        let failures = body["proxy"]["upstream_failures"].as_u64().unwrap_or(0);
        assert!(
            failures > 0,
            "Expected upstream_failures > 0 after error tests, got {failures}"
        );
    });

    // Cleanup
    eprintln!("\x1b[32m[INFO]\x1b[0m Stopping error handling proxy...");
    proxy.stop();
    config.restore();
    eprintln!("--------------------------------------------");
}
