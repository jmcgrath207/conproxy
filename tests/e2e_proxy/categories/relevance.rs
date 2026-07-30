use crate::helpers::client::E2eClient;
use crate::helpers::constants::{category_enabled, Suite};
use crate::helpers::report::TestReport;
use crate::run_test;

pub fn run(client: &E2eClient, suite: Suite, report: &mut TestReport) {
    if !category_enabled("relevance") || !suite.has_text_upstreams() {
        return;
    }

    eprintln!();
    eprintln!("\x1b[1mQuery Relevance Tests\x1b[0m");
    eprintln!("--------------------------------------------");

    // Clear cache for clean relevance testing
    client.cache_clear();

    // Batch of keyword queries all return results
    run_test!(
        report,
        "relevance",
        "Relevance: keyword queries return results",
        {
            let (status, body) = client.batch(&[
                ("q1", "rust programming language"),
                ("q2", "elasticsearch full text search"),
                ("q3", "load balancing failover"),
                ("q4", "BM25 scoring algorithm"),
                ("q5", "circuit breaker pattern distributed systems"),
            ]);
            assert_eq!(status, 200);
            let len = body["results"].as_object().map(|o| o.len()).unwrap_or(0);
            assert_eq!(len, 5, "Expected 5 batch results, got {len}");
        }
    );

    // q-001 (rust programming) returns doc-001
    run_test!(report, "relevance", "Relevance: q-001 finds doc-001", {
        let (status, body) = client.query("rust programming language");
        assert_eq!(status, 200);
        let has_doc = body["results"]
            .as_array()
            .map(|arr| arr.iter().any(|r| r["id"] == "doc-001"))
            .unwrap_or(false);
        assert!(
            has_doc,
            "Expected doc-001 in results for 'rust programming language'"
        );
    });

    // q-003 (elasticsearch) returns doc-003
    run_test!(report, "relevance", "Relevance: q-003 finds doc-003", {
        let (status, body) = client.query("elasticsearch full text search");
        assert_eq!(status, 200);
        let has_doc = body["results"]
            .as_array()
            .map(|arr| arr.iter().any(|r| r["id"] == "doc-003"))
            .unwrap_or(false);
        assert!(
            has_doc,
            "Expected doc-003 in results for 'elasticsearch full text search'"
        );
    });

    // q-009 (circuit breaker) returns doc-008
    run_test!(report, "relevance", "Relevance: q-009 finds doc-008", {
        let (status, body) = client.query("circuit breaker pattern distributed systems");
        assert_eq!(status, 200);
        let has_doc = body["results"]
            .as_array()
            .map(|arr| arr.iter().any(|r| r["id"] == "doc-008"))
            .unwrap_or(false);
        assert!(
            has_doc,
            "Expected doc-008 in results for 'circuit breaker pattern'"
        );
    });

    // Cache populated after relevance queries
    run_test!(report, "relevance", "Relevance: cache populated", {
        let (_, stats) = client.stats();
        let total = stats["cache"]["total"].as_u64().unwrap_or(0);
        assert!(total >= 3, "Expected >= 3 cache entries, got {total}");
    });

    eprintln!("--------------------------------------------");
}
