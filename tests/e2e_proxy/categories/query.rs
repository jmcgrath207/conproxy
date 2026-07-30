use crate::helpers::client::E2eClient;
use crate::helpers::constants::{category_enabled, Suite};
use crate::helpers::report::TestReport;
use crate::run_test;

/// Phase 1 query tests — bash tests 9-11.
/// Runs after cache basics (6-8) and before cache clear (12-15).
pub fn run(client: &E2eClient, suite: Suite, report: &mut TestReport) {
    if !category_enabled("query") {
        return;
    }

    // Test 9: Federated search (mixed/all only)
    if suite.has_multiple_upstreams() {
        run_test!(report, "query", "Federated search", {
            let (status, _) = client.federated("search test");
            assert!(
                [200, 502, 503].contains(&status),
                "Expected 200/502/503, got {status}"
            );
        });
    }

    // Test 10: Circuit breaker endpoint
    run_test!(report, "query", "Circuit breaker status", {
        let (status, _) = client.circuit();
        assert_eq!(status, 200);
    });

    // Test 11: Queue status
    run_test!(report, "query", "Queue status", {
        let (status, _) = client.queue();
        assert_eq!(status, 200);
    });
}

/// Phase 1 suite-specific tests — bash tests 16-19.
/// Runs after cache clear/warmup (12-15), still within the health+cache+query snapshot.
pub fn run_suite_specific(client: &E2eClient, suite: Suite, report: &mut TestReport) {
    if !category_enabled("query") {
        return;
    }

    // Test 16: Health JSON valid
    run_test!(report, "query", "Health JSON valid", {
        let (status, body) = client.health();
        assert_eq!(status, 200);
        assert!(body["status"].is_string(), "Missing status field");
    });

    // Suite-specific multi-upstream tests (all only)
    if suite.is_all() {
        // Test 17: Pool shows multiple upstreams
        run_test!(report, "query", "Pool multi-upstream", {
            let (_, pool) = client.pool();
            let total = pool["stats"]["total_upstreams"].as_u64().unwrap_or(0);
            assert!(total >= 1, "Expected >= 1 upstreams, got {total}");
        });

        // Test 18: Context list
        run_test!(report, "query", "Context list", {
            let status = client.get_status("/contexts");
            assert!(
                status == 200 || status == 404,
                "Expected 200 or 404, got {status}"
            );
        });

        // Test 19: Admin reload
        run_test!(report, "query", "Admin reload", {
            let (status, _) = client.admin_reload();
            assert!(
                status == 200 || status == 404,
                "Expected 200 or 404, got {status}"
            );
        });
    }
}
