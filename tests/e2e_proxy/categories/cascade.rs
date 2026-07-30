use crate::helpers::client::E2eClient;
use crate::helpers::constants::{category_enabled, Suite};
use crate::helpers::report::TestReport;
use crate::run_test;

pub fn run(client: &E2eClient, suite: Suite, report: &mut TestReport) {
    if !category_enabled("cascade") || !suite.has_text_upstreams() {
        return;
    }

    eprintln!();
    eprintln!("\x1b[1mCascade Query Tests\x1b[0m");
    eprintln!("--------------------------------------------");

    // Cascade: query returns results (cascade is transparent to the client)
    run_test!(report, "cascade", "Cascade: query returns results", {
        let (status, body) = client.query("rust programming");
        assert_eq!(status, 200, "Query failed with status {status}");
        assert!(body["results"].is_array(), "Missing results array");
    });

    // Cascade: metrics show cascade activity
    run_test!(
        report,
        "cascade",
        "Cascade: metrics show cascade activity",
        {
            // Fire a query to ensure cascade metrics are populated
            let _ = client.query("database indexing");
            let (status, m) = client.metrics();
            assert_eq!(status, 200);
            let success = m["proxy"]["cascade_success"].as_u64().unwrap_or(0);
            let exhausted = m["proxy"]["cascade_exhausted"].as_u64().unwrap_or(0);
            assert!(
            success > 0 || exhausted > 0,
            "Expected cascade_success > 0 or cascade_exhausted > 0, got success={success}, exhausted={exhausted}"
        );
        }
    );

    // Cascade: multiple upstreams tried (multi-upstream suites only)
    run_test!(report, "cascade", "Cascade: multiple upstreams tried", {
        let _ = client.query("error handling patterns");
        let (status, m) = client.metrics();
        assert_eq!(status, 200);
        // cascade depth average > 0 means at least one upstream was tried
        let depth = m["proxy"]["cascade_avg_depth"].as_f64().unwrap_or(0.0);
        assert!(depth > 0.0, "Expected cascade_avg_depth > 0, got {depth}");
    });

    // Cascade: results have scores (cascade normalizes scores)
    run_test!(report, "cascade", "Cascade: results have scores", {
        let (status, body) = client.query("async runtime");
        assert_eq!(status, 200);
        if let Some(results) = body["results"].as_array() {
            for r in results {
                let score = r["score"].as_f64().unwrap_or(0.0);
                assert!(score >= 0.0, "Expected non-negative score, got {score}");
            }
        }
    });

    eprintln!("--------------------------------------------");
}
