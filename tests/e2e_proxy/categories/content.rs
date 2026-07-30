use crate::helpers::client::E2eClient;
use crate::helpers::constants::{category_enabled, Suite};
use crate::helpers::report::TestReport;
use crate::run_test;

pub fn run(client: &E2eClient, suite: Suite, report: &mut TestReport) {
    if !category_enabled("content") {
        return;
    }

    // Batch: returns results with correct count (text upstreams)
    if suite.has_text_upstreams() {
        run_test!(report, "content", "Content: batch results", {
            let (status, body) = client.batch(&[("q1", "rust"), ("q2", "python")]);
            assert_eq!(status, 200);
            let len = body["results"].as_object().map(|o| o.len()).unwrap_or(0);
            assert_eq!(len, 2, "Expected 2 batch results, got {len}");
        });

        // Federated returns valid response
        run_test!(report, "content", "Content: federated results", {
            let (_, body) = client.federated("programming");
            assert!(
                body["results"].is_array(),
                "Expected results array in federated response"
            );
        });
    }

    // Stats: cache section has expected fields
    run_test!(report, "content", "Content: stats cache fields", {
        let (_, stats) = client.stats();
        let cache = &stats["cache"];
        assert!(!cache["total"].is_null(), "Missing cache.total");
        assert!(!cache["fresh"].is_null(), "Missing cache.fresh");
        assert!(!cache["stale"].is_null(), "Missing cache.stale");
        assert!(!cache["expired"].is_null(), "Missing cache.expired");
    });

    // Audit: returns recent_entries array
    run_test!(report, "content", "Content: audit entries", {
        let (_, audit) = client.audit();
        assert!(
            audit["recent_entries"].is_array(),
            "Expected recent_entries array"
        );
    });

    // Clients: returns clients array
    run_test!(report, "content", "Content: clients list", {
        let (_, body) = client.clients();
        assert!(body["clients"].is_array(), "Expected clients array");
    });

    // Grafana dashboard: valid JSON with panels
    run_test!(report, "content", "Content: grafana dashboard valid", {
        let dashboard_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/e2e/data/grafana-dashboard.json");
        let content =
            std::fs::read_to_string(&dashboard_path).expect("Failed to read grafana dashboard");
        let dashboard: serde_json::Value =
            serde_json::from_str(&content).expect("Invalid JSON in dashboard");
        let panels = dashboard["panels"].as_array().expect("Missing panels");
        assert!(!panels.is_empty(), "Dashboard has no panels");
    });
}
