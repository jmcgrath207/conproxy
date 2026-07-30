use crate::helpers::client::E2eClient;
use crate::helpers::constants::{category_enabled, Suite};
use crate::helpers::report::TestReport;
use crate::run_test;

pub fn run(client: &E2eClient, suite: Suite, report: &mut TestReport) {
    if !category_enabled("operational") {
        return;
    }

    // Test 20: Readiness probe
    run_test!(report, "operational", "Readiness probe", {
        let (status, body) = client.ready();
        assert_eq!(status, 200);
        assert!(!body["ready"].is_null(), "Missing ready field");
    });

    // Test 21: Cache integrity
    run_test!(report, "operational", "Cache integrity", {
        let status = client.get_status("/cache/integrity");
        assert!(
            status == 200 || status == 404,
            "Expected 200 or 404, got {status}"
        );
    });

    // Test 22: Cache upstreams
    run_test!(report, "operational", "Cache upstreams", {
        let status = client.get_status("/cache/upstreams");
        assert!(
            status == 200 || status == 404,
            "Expected 200 or 404, got {status}"
        );
    });

    // Test 23: Cache evict (nonexistent pattern)
    run_test!(report, "operational", "Cache evict", {
        let (status, _) = client.cache_evict("nonexistent");
        assert!(
            [200, 204, 400, 404].contains(&status),
            "Expected 200/204/400/404, got {status}"
        );
    });

    // Test 24: Query stats
    run_test!(report, "operational", "Query stats", {
        let status = client.get_status("/stats/queries");
        assert!(
            status == 200 || status == 404,
            "Expected 200 or 404, got {status}"
        );
    });

    // Test 25: Current context
    run_test!(report, "operational", "Current context", {
        let status = client.get_status("/contexts/current");
        assert!(
            status == 200 || status == 404,
            "Expected 200 or 404, got {status}"
        );
    });

    // Test 27: Admin pause
    run_test!(report, "operational", "Admin pause", {
        let (status, body) = client.admin_pause();
        assert_eq!(status, 200);
        assert_eq!(body["status"].as_str(), Some("paused"));
    });

    // Verify queries rejected while paused
    run_test!(report, "operational", "Pause: query rejected (503)", {
        let (status, _) = client.query("paused test");
        assert_eq!(status, 503, "Expected 503 while paused, got {status}");
    });

    // Health still works while paused
    run_test!(report, "operational", "Pause: health still up", {
        let (status, _) = client.health();
        assert_eq!(status, 200);
    });

    // Resume
    run_test!(report, "operational", "Admin resume", {
        let (status, body) = client.admin_resume();
        assert_eq!(status, 200);
        assert_eq!(body["status"].as_str(), Some("resumed"));
    });

    // Verify queries work after resume (not qdrant — text upstreams only)
    if suite.has_text_upstreams() {
        run_test!(report, "operational", "Resume: query works", {
            let (status, body) = client.query("resume test");
            assert_eq!(status, 200);
            assert!(body["results"].is_array(), "Missing results after resume");
        });
    }

    // Test 28: Client tracking
    run_test!(report, "operational", "Client list", {
        let status = client.get_status("/clients");
        assert!(
            status == 200 || status == 404,
            "Expected 200 or 404, got {status}"
        );
    });

    // Test 29: Audit log
    run_test!(report, "operational", "Audit log", {
        let status = client.get_status("/audit");
        assert!(
            status == 200 || status == 404,
            "Expected 200 or 404, got {status}"
        );
    });
}
