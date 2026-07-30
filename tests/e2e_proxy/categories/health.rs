use crate::helpers::client::E2eClient;
use crate::helpers::constants::{category_enabled, Suite};
use crate::helpers::report::TestReport;
use crate::run_test;

pub fn run(client: &E2eClient, suite: Suite, report: &mut TestReport) {
    if !category_enabled("health") {
        return;
    }

    // Test 1: Health check
    run_test!(report, "health", "Proxy health check", {
        let (status, _) = client.health();
        assert_eq!(status, 200);
    });

    // Test 2: Basic query
    run_test!(report, "health", "Basic query", {
        let (status, body) = client.query("rust programming");
        if suite == Suite::Qdrant {
            assert!(
                status == 200 || status == 502,
                "Expected 200 or 502, got {status}"
            );
        } else {
            assert_eq!(status, 200, "Query failed: {}", body);
            assert!(body["results"].is_array(), "Missing results array");
        }
    });

    // Test 3: Cache stats
    run_test!(report, "health", "Cache stats", {
        let (status, _) = client.stats();
        assert_eq!(status, 200);
    });

    // Test 4: Prometheus metrics
    run_test!(report, "health", "Prometheus metrics", {
        let (status, body) = client.prometheus();
        assert_eq!(status, 200);
        assert!(body.contains("conproxy_"), "Missing conproxy_ metrics");
    });

    // Test 5: Pool status
    run_test!(report, "health", "Pool status", {
        let (status, _) = client.pool();
        assert_eq!(status, 200);
    });
}
