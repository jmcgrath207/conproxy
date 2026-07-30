//! Cache integrity and upstream observability E2E tests.
//!
//! Runs against the main suite proxy. Tests the /cache/integrity and
//! /cache/upstreams endpoints.

use crate::helpers::client::E2eClient;
use crate::helpers::constants::{category_enabled, Suite};
use crate::helpers::report::TestReport;
use crate::run_test;

pub fn run(client: &E2eClient, _suite: Suite, report: &mut TestReport) {
    if !category_enabled("cache_observability") {
        return;
    }

    eprintln!();
    eprintln!("\x1b[32m[INFO]\x1b[0m Cache observability tests...");

    // Test 1: Integrity on empty cache
    run_test!(report, "cache_observability", "Integrity: empty cache", {
        client.cache_clear();
        let (status, body) = client.cache_integrity();
        assert_eq!(status, 200, "Expected 200 for cache integrity");
        let total = body["total"].as_u64().unwrap_or(u64::MAX);
        assert_eq!(total, 0, "Expected total=0 after clear, got {total}");
        let valid = body["valid"].as_u64().unwrap_or(u64::MAX);
        assert_eq!(valid, 0, "Expected valid=0 after clear, got {valid}");
        let invalid = body["invalid"].as_u64().unwrap_or(u64::MAX);
        assert_eq!(invalid, 0, "Expected invalid=0 after clear, got {invalid}");
    });

    // Test 2: Integrity after populating cache
    run_test!(report, "cache_observability", "Integrity: populated", {
        // Query 3 items to populate
        let _ = client.query("cache obs alpha");
        let _ = client.query("cache obs beta");
        let _ = client.query("cache obs gamma");

        let (status, body) = client.cache_integrity();
        assert_eq!(status, 200);
        let total = body["total"].as_u64().unwrap_or(0);
        assert!(total > 0, "Expected total > 0 after queries, got {total}");
        let valid = body["valid"].as_u64().unwrap_or(0);
        assert_eq!(
            valid, total,
            "Expected valid == total, got valid={valid}, total={total}"
        );
        let invalid = body["invalid"].as_u64().unwrap_or(u64::MAX);
        assert_eq!(invalid, 0, "Expected invalid=0, got {invalid}");
    });

    // Test 3: Integrity response shape
    run_test!(
        report,
        "cache_observability",
        "Integrity: response shape",
        {
            let (status, body) = client.cache_integrity();
            assert_eq!(status, 200);
            assert!(body.get("total").is_some(), "Missing 'total' field");
            assert!(body.get("valid").is_some(), "Missing 'valid' field");
            assert!(body.get("invalid").is_some(), "Missing 'invalid' field");
            // no_hash or missing_hash field
            let has_hash_field =
                body.get("no_hash").is_some() || body.get("missing_hash").is_some();
            assert!(has_hash_field, "Missing 'no_hash' or 'missing_hash' field");
        }
    );

    // Test 4: Upstreams on empty cache
    run_test!(report, "cache_observability", "Upstreams: empty", {
        client.cache_clear();
        let (status, _body) = client.cache_upstreams();
        assert_eq!(status, 200, "Expected 200 for cache upstreams");
    });

    // Test 5: Upstreams after populating
    run_test!(report, "cache_observability", "Upstreams: populated", {
        let _ = client.query("cache ups alpha");
        let _ = client.query("cache ups beta");
        let (status, body) = client.cache_upstreams();
        assert_eq!(status, 200);
        // The response is a map of upstream_id → stats
        let obj = body.as_object();
        if let Some(map) = obj {
            let has_entries = map
                .values()
                .any(|v| v.get("total").and_then(|t| t.as_u64()).unwrap_or(0) > 0);
            assert!(has_entries, "Expected at least one upstream with total > 0");
        }
    });

    // Test 6: Upstreams totals match integrity
    run_test!(report, "cache_observability", "Upstreams: totals match", {
        let (_, integrity) = client.cache_integrity();
        let integrity_total = integrity["total"].as_u64().unwrap_or(0);

        let (_, upstreams) = client.cache_upstreams();
        let upstream_total: u64 = upstreams
            .as_object()
            .map(|map| map.values().map(|v| v["total"].as_u64().unwrap_or(0)).sum())
            .unwrap_or(0);

        assert_eq!(
            integrity_total, upstream_total,
            "Integrity total ({integrity_total}) != sum of upstream totals ({upstream_total})"
        );
    });
}
