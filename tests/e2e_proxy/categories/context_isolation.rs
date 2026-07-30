//! Context isolation E2E tests.
//!
//! Runs against the main suite proxy. Verifies that different contexts
//! have isolated cache namespaces with independent hit/miss tracking.

use crate::helpers::client::E2eClient;
use crate::helpers::constants::{category_enabled, Suite};
use crate::helpers::report::TestReport;
use crate::run_test;

pub fn run(client: &E2eClient, _suite: Suite, report: &mut TestReport) {
    if !category_enabled("context_isolation") {
        return;
    }

    eprintln!();
    eprintln!("\x1b[32m[INFO]\x1b[0m Context isolation tests...");

    // Test 1: Create context A
    run_test!(report, "context_isolation", "Ctx: create context A", {
        let (status, body) = client.context_create(&serde_json::json!({"id": "e2e-ctx-a"}));
        assert!(
            status == 201 || status == 200,
            "Expected 201/200 creating context A, got {status}: {body}"
        );
    });

    // Test 2: Create context B
    run_test!(report, "context_isolation", "Ctx: create context B", {
        let (status, body) = client.context_create(&serde_json::json!({"id": "e2e-ctx-b"}));
        assert!(
            status == 201 || status == 200,
            "Expected 201/200 creating context B, got {status}: {body}"
        );
    });

    // Test 3: Query in A → miss (uses x-context header to scope)
    run_test!(report, "context_isolation", "Ctx: switch to A, query", {
        let (status, body) = client.query_with_context("e2e-ctx-a", "ctx isolation test");
        assert_eq!(status, 200);
        let cs = body["cache_status"].as_str().unwrap_or("");
        assert_eq!(cs, "miss", "Expected miss in fresh context A, got {cs}");
    });

    // Test 4: Same query in B → miss (not hit from A)
    run_test!(
        report,
        "context_isolation",
        "Ctx: switch to B, same query is miss",
        {
            let (status, body) = client.query_with_context("e2e-ctx-b", "ctx isolation test");
            assert_eq!(status, 200);
            let cs = body["cache_status"].as_str().unwrap_or("");
            assert_eq!(
                cs, "miss",
                "Expected miss in fresh context B (isolated from A), got {cs}"
            );
        }
    );

    // Test 5: A stats
    run_test!(report, "context_isolation", "Ctx: A stats", {
        let (status, body) = client.context_stats("e2e-ctx-a");
        assert_eq!(status, 200, "Expected 200 for context A stats");
        let misses = body["misses"].as_u64().unwrap_or(0);
        assert!(
            misses >= 1,
            "Expected at least 1 miss in ctx A, got {misses}"
        );
    });

    // Test 6: B stats
    run_test!(report, "context_isolation", "Ctx: B stats", {
        let (status, body) = client.context_stats("e2e-ctx-b");
        assert_eq!(status, 200, "Expected 200 for context B stats");
        let misses = body["misses"].as_u64().unwrap_or(0);
        assert!(
            misses >= 1,
            "Expected at least 1 miss in ctx B, got {misses}"
        );
    });

    // Test 7: Re-query A → hit
    run_test!(report, "context_isolation", "Ctx: re-query A is hit", {
        let (status, body) = client.query_with_context("e2e-ctx-a", "ctx isolation test");
        assert_eq!(status, 200);
        let cs = body["cache_status"].as_str().unwrap_or("");
        assert_eq!(cs, "hit", "Expected hit on re-query in context A, got {cs}");
    });

    // Restore to default context
    let _ = client.context_switch("default");
}
