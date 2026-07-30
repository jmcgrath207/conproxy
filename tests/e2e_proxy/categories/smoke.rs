//! Quick smoke test — validates all proxy endpoints respond.
//!
//! Replaces `tests/e2e/scripts/smoke_test.sh`.
//! Does not require Docker services or test data — only a running proxy.

use crate::helpers::client::E2eClient;
use crate::helpers::constants::category_enabled;
use crate::helpers::report::TestReport;
use crate::run_test;

pub fn run(client: &E2eClient, report: &mut TestReport) {
    if !category_enabled("smoke") {
        return;
    }

    // --- Health checks ---

    run_test!(report, "smoke", "GET /health (valid JSON)", {
        let (status, body) = client.health();
        assert_eq!(status, 200, "Health check failed");
        assert!(body["status"].is_string(), "Missing status field");
    });

    run_test!(report, "smoke", "GET /ready (responds)", {
        let (status, body) = client.ready();
        assert_eq!(status, 200, "Ready check failed");
        assert!(!body["ready"].is_null(), "Missing ready field");
    });

    run_test!(report, "smoke", "GET /pool (valid JSON)", {
        let (status, body) = client.pool();
        assert_eq!(status, 200, "Pool check failed");
        assert!(
            !body["pool_configured"].is_null(),
            "Missing pool_configured field"
        );
    });

    // --- Stats endpoints ---

    run_test!(report, "smoke", "GET /stats (cache fields)", {
        let (status, body) = client.stats();
        assert_eq!(status, 200, "Stats failed");
        let cache = &body["cache"];
        assert!(!cache["total"].is_null(), "Missing cache.total");
        assert!(!cache["fresh"].is_null(), "Missing cache.fresh");
        assert!(!cache["stale"].is_null(), "Missing cache.stale");
        assert!(!cache["expired"].is_null(), "Missing cache.expired");
    });

    run_test!(report, "smoke", "GET /metrics (proxy fields)", {
        let (status, body) = client.metrics();
        assert_eq!(status, 200, "Metrics failed");
        assert!(
            !body["proxy"]["requests_total"].is_null(),
            "Missing proxy.requests_total"
        );
    });

    run_test!(
        report,
        "smoke",
        "GET /metrics/prometheus (has conproxy_)",
        {
            let (status, body) = client.prometheus();
            assert_eq!(status, 200, "Prometheus failed");
            assert!(
                body.contains("conproxy_"),
                "Missing conproxy_ in prometheus output"
            );
        }
    );

    run_test!(report, "smoke", "GET /circuit (state field)", {
        let (status, body) = client.circuit();
        assert_eq!(status, 200, "Circuit failed");
        assert!(!body["state"].is_null(), "Missing state field");
    });

    run_test!(report, "smoke", "GET /queue (total field)", {
        let (status, body) = client.queue();
        assert_eq!(status, 200, "Queue failed");
        assert!(!body["total"].is_null(), "Missing total field");
    });

    // --- Query endpoints ---

    run_test!(report, "smoke", "POST /query (responds 200 or 502)", {
        let (status, _) = client.query("test");
        assert!(
            status == 200 || status == 502,
            "Expected 200 or 502, got {status}"
        );
    });

    run_test!(report, "smoke", "POST /batch (responds 200 or 502)", {
        let (status, _) = client.batch(&[("q1", "test1"), ("q2", "test2")]);
        assert!(
            status == 200 || status == 502,
            "Expected 200 or 502, got {status}"
        );
    });

    // --- Operational endpoints ---

    run_test!(report, "smoke", "GET /clients (JSON array)", {
        let (status, body) = client.clients();
        assert_eq!(status, 200, "Clients failed");
        assert!(body["clients"].is_array(), "Missing clients array");
    });

    run_test!(report, "smoke", "GET /audit (entries array)", {
        let (status, body) = client.audit();
        assert_eq!(status, 200, "Audit failed");
        assert!(
            body["recent_entries"].is_array(),
            "Missing recent_entries array"
        );
    });

    run_test!(report, "smoke", "GET /contexts (responds)", {
        let status = client.get_status("/contexts");
        assert!(
            status == 200 || status == 404,
            "Expected 200 or 404, got {status}"
        );
    });

    run_test!(report, "smoke", "POST /admin/reload (success)", {
        let (status, body) = client.admin_reload();
        assert_eq!(status, 200, "Reload failed");
        assert_eq!(
            body["success"].as_bool(),
            Some(true),
            "Reload did not return success=true"
        );
    });
}
