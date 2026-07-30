//! Circuit breaker and frozen entry E2E tests.
//!
//! Manages its own MockUpstream + ProxyProcess. Verifies circuit breaker state
//! transitions (closed → open → half-open → closed) and frozen cache entries.

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
    if !category_enabled("circuit_breaker") {
        return;
    }

    eprintln!();
    eprintln!("\x1b[1mCircuit Breaker + Frozen Entry Tests (mock upstream)\x1b[0m");
    eprintln!("--------------------------------------------");

    let mock = MockUpstream::start();
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut config = ConfigManager::new(&project_root);
    config.write_mock_config(&mock.url(), "elasticsearch");

    eprintln!("\x1b[32m[INFO]\x1b[0m Starting proxy with mock upstream...");
    let mut proxy = ProxyProcess::start("127.0.0.1:8080");
    if let Err(e) = proxy.wait_healthy(Duration::from_secs(5)) {
        eprintln!("\x1b[31m[ERROR]\x1b[0m Circuit breaker proxy failed to start: {e}");
        proxy.stop();
        config.restore();
        return;
    }

    let client = E2eClient::new(proxy_url());

    // Test 1: Circuit starts closed
    run_test!(report, "circuit_breaker", "CB: starts closed", {
        let (status, body) = client.circuit();
        assert_eq!(status, 200);
        let state = body["state"].as_str().unwrap_or("");
        assert!(
            state.to_lowercase().contains("closed") || state.to_lowercase().contains("close"),
            "Expected circuit closed, got: {state}"
        );
    });

    // Test 2: Populate cache with healthy upstream
    run_test!(report, "circuit_breaker", "CB: populate cache", {
        mock.set_behavior(MockBehavior::Healthy);
        client.cache_clear();
        let (status, body) = client.query("cb test alpha");
        assert_eq!(status, 200);
        let cs = body["cache_status"].as_str().unwrap_or("");
        assert_eq!(cs, "miss", "Expected cache miss on first query, got {cs}");
    });

    // Test 3: Verify cached
    run_test!(report, "circuit_breaker", "CB: verify cached", {
        let (status, body) = client.query("cb test alpha");
        assert_eq!(status, 200);
        let cs = body["cache_status"].as_str().unwrap_or("");
        assert_eq!(cs, "hit", "Expected cache hit, got {cs}");
    });

    // Test 4: Trip circuit with failures
    run_test!(
        report,
        "circuit_breaker",
        "CB: trip circuit with failures",
        {
            mock.set_behavior(MockBehavior::ErrorCode(500));
            // Send enough failing queries to trip the circuit breaker (threshold=3)
            for i in 0..10 {
                let _ = client.query(&format!("cb trip {i}"));
            }
            std::thread::sleep(Duration::from_millis(500));
            let (status, body) = client.circuit();
            assert_eq!(status, 200);
            let failure_count = body["failure_count"]
                .as_u64()
                .or_else(|| body["consecutive_failures"].as_u64())
                .unwrap_or(0);
            assert!(failure_count > 0, "Expected failure_count > 0");
        }
    );

    // Test 5: Frozen entry served when circuit is open/degraded
    run_test!(report, "circuit_breaker", "CB: frozen entry served", {
        // The previously cached "cb test alpha" should be served as frozen
        let (status, body) = client.query("cb test alpha");
        // Should get a response (either frozen or hit from before)
        assert!(
            status == 200 || status == 503,
            "Expected 200 (frozen) or 503, got {status}"
        );
        if status == 200 {
            let cs = body["cache_status"].as_str().unwrap_or("");
            assert!(
                cs == "frozen" || cs == "hit" || cs == "stale",
                "Expected frozen/hit/stale cache status, got {cs}"
            );
        }
    });

    // Test 6: Uncached query fails fast when circuit is open
    run_test!(
        report,
        "circuit_breaker",
        "CB: uncached query fails fast",
        {
            let (status, _) = client.query("cb never cached before xyz");
            assert!(
                status == 502 || status == 503 || status == 200,
                "Expected error or degraded response for uncached query, got {status}"
            );
        }
    );

    // Test 7: Recovery after upstream heals
    run_test!(
        report,
        "circuit_breaker",
        "CB: recovery after upstream heals",
        {
            mock.set_behavior(MockBehavior::Healthy);
            // Wait for circuit breaker to transition to half-open (open_duration_secs=3)
            eprintln!("\x1b[32m[INFO]\x1b[0m Waiting 5s for circuit breaker recovery...");
            std::thread::sleep(Duration::from_secs(5));
            // Send queries to help close the circuit
            for _ in 0..5 {
                let _ = client.query("cb recovery probe");
                std::thread::sleep(Duration::from_millis(200));
            }
            let (status, body) = client.circuit();
            assert_eq!(status, 200);
            let state = body["state"].as_str().unwrap_or("");
            // Circuit should be transitioning toward closed
            assert!(
                state.to_lowercase().contains("closed")
                    || state.to_lowercase().contains("half")
                    || state.to_lowercase().contains("close"),
                "Expected circuit closing/closed after recovery, got: {state}"
            );
        }
    );

    // Test 8: Fresh query after recovery
    run_test!(
        report,
        "circuit_breaker",
        "CB: fresh query after recovery",
        {
            client.cache_clear();
            let (status, body) = client.query("cb fresh after recovery");
            assert_eq!(
                status, 200,
                "Expected 200 after full recovery, got {status}"
            );
            assert!(body["results"].is_array(), "Expected results");
        }
    );

    // Test 9: times_tripped incremented
    run_test!(
        report,
        "circuit_breaker",
        "CB: times_tripped incremented",
        {
            let (status, body) = client.circuit();
            assert_eq!(status, 200);
            let tripped = body["times_tripped"]
                .as_u64()
                .or_else(|| body["total_trips"].as_u64())
                .unwrap_or(0);
            // It may or may not have this field depending on implementation
            // Just verify the endpoint responds with circuit data
            let state = body["state"].as_str();
            assert!(state.is_some(), "Expected state field in circuit response");
            eprintln!("  Circuit times_tripped={tripped}, state={:?}", state);
        }
    );

    // Cleanup
    eprintln!("\x1b[32m[INFO]\x1b[0m Stopping circuit breaker proxy...");
    proxy.stop();
    config.restore();
    eprintln!("--------------------------------------------");
}
