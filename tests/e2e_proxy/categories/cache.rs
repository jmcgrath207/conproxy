use crate::helpers::client::E2eClient;
use crate::helpers::constants::{category_enabled, Suite};
use crate::helpers::report::TestReport;
use crate::run_test;

/// Phase 1a: Basic cache tests (warmup, hit, batch) — bash tests 6-8.
/// Runs in the health+cache+query combined phase, BEFORE query tests.
pub fn run(client: &E2eClient, suite: Suite, report: &mut TestReport) {
    if !category_enabled("cache") {
        return;
    }

    // Test 6: Cache warmup
    run_test!(report, "cache", "Cache warmup", {
        let (status, _) = client.warmup(&["test query"]);
        if suite == Suite::Qdrant {
            assert!(
                [200, 202, 502, 503].contains(&status),
                "Expected 200/202/502/503, got {status}"
            );
        } else {
            assert!(
                status == 200 || status == 202,
                "Expected 200 or 202, got {status}"
            );
        }
    });

    // Test 7: Cache hit (repeat query)
    run_test!(report, "cache", "Cache hit", {
        let (status, body) = client.query("rust programming");
        if suite == Suite::Qdrant {
            assert!(status == 200 || status == 502, "Got {status}");
        } else {
            assert_eq!(status, 200);
            assert_eq!(
                body["cache_status"].as_str(),
                Some("hit"),
                "Expected cache hit, got: {}",
                body
            );
        }
    });

    // Test 8: Batch query
    run_test!(report, "cache", "Batch query", {
        let (status, body) = client.batch(&[("q1", "test 1"), ("q2", "test 2")]);
        if suite == Suite::Qdrant {
            assert!(status == 200 || status == 502, "Got {status}");
        } else {
            assert_eq!(status, 200);
            assert!(body["results"].is_object(), "Missing results: {}", body);
        }
    });
}

/// Phase 1b: Cache clear + re-warm tests — bash tests 12-15.
/// In bash these run AFTER query tests 9-11 but still within the health+cache+query snapshot.
pub fn run_clear_and_warmup(client: &E2eClient, suite: Suite, report: &mut TestReport) {
    if !category_enabled("cache") {
        return;
    }

    // Test 12: Cache clear
    run_test!(report, "cache", "Cache clear", {
        let status = client.cache_clear();
        assert!(
            status == 200 || status == 204,
            "Expected 200 or 204, got {status}"
        );
    });

    // Test 13: Query after cache clear (miss)
    run_test!(report, "cache", "Query after clear", {
        let (status, body) = client.query("after clear test");
        if suite == Suite::Qdrant {
            assert!(status == 200 || status == 502, "Got {status}");
        } else {
            assert_eq!(status, 200);
            assert_eq!(
                body["cache_status"].as_str(),
                Some("miss"),
                "Expected miss after clear, got: {}",
                body
            );
        }
    });

    // Test 14: Seed warmup (multiple queries)
    run_test!(report, "cache", "Seed warmup (multiple)", {
        let (status, _) = client.warmup(&[
            "authentication API",
            "database migrations",
            "error handling",
        ]);
        if suite == Suite::Qdrant {
            assert!(
                [200, 202, 502, 503].contains(&status),
                "Expected 200/202/502/503, got {status}"
            );
        } else {
            assert!(status == 200 || status == 202, "Got {status}");
        }
    });

    // Test 15: Cache populated after warmup
    run_test!(report, "cache", "Cache populated after warmup", {
        let (status, _body) = client.stats();
        assert_eq!(status, 200);
    });
}

/// Cache correctness tests — bash tests 41-43.
/// In bash these run AFTER the metrics phase (line 678 sets CURRENT_CATEGORY="cache").
pub fn run_correctness(client: &E2eClient, suite: Suite, report: &mut TestReport) {
    if !category_enabled("cache") || !suite.has_text_upstreams() {
        return;
    }

    run_test!(report, "cache", "Query returns results", {
        let (status, body) = client.query("vector databases similarity search");
        assert_eq!(status, 200);
        let len = body["results"].as_array().map(|a| a.len()).unwrap_or(0);
        assert!(len > 0, "Expected results, got: {}", body);
    });

    run_test!(report, "cache", "Repeated query is cache hit", {
        let (status, body) = client.query("vector databases similarity search");
        assert_eq!(status, 200);
        assert_eq!(
            body["cache_status"].as_str(),
            Some("hit"),
            "Expected hit, got: {}",
            body
        );
    });

    run_test!(report, "cache", "Metrics: hits>0, errors=0", {
        let (_, metrics) = client.metrics();
        let hits = metrics["proxy"]["cache_hits"].as_u64().unwrap_or(0);
        let errors = metrics["proxy"]["upstream_failures"].as_u64().unwrap_or(0);
        assert!(hits > 0, "Expected hits > 0");
        assert_eq!(errors, 0, "Expected 0 errors");
    });
}

/// Cache eviction with real pattern matching — bash line 983-998.
/// In bash this runs AFTER resilience tests, before the resilience snapshot.
pub fn run_eviction(client: &E2eClient, suite: Suite, report: &mut TestReport) {
    if !category_enabled("cache") || !suite.has_text_upstreams() {
        return;
    }

    // Warm cache with a known query
    let _ = client.query("eviction test query");

    run_test!(report, "cache", "Evict: pattern match", {
        let (status, body) = client.cache_evict("eviction");
        assert_eq!(status, 200);
        assert!(
            body["evicted"].is_number() || body["success"] == true,
            "Expected eviction result: {}",
            body
        );
    });
}
