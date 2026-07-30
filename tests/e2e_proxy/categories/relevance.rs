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

    // q-003: Meilisearch FTS for caching doc (doc-003 title/content)
    run_test!(report, "relevance", "Relevance: q-003 finds doc-003", {
        let (status, body) = client.query("Caching strategies in distributed systems");
        assert_eq!(status, 200);
        let results = body["results"].as_array();
        assert!(
            results.map(|a| !a.is_empty()).unwrap_or(false),
            "Expected non-empty results for caching strategies query: {body}"
        );
        let has_doc = results
            .map(|arr| {
                arr.iter().any(|r| {
                    let id = r["id"].as_str().unwrap_or("");
                    id == "doc-003"
                        || id.contains("003")
                        || r["content"]
                            .as_str()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains("cache")
                        || r["title"]
                            .as_str()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains("caching")
                })
            })
            .unwrap_or(false);
        assert!(
            has_doc,
            "Expected caching-related doc in results, got: {body}"
        );
    });

    // q-009: gRPC / protobuf doc
    run_test!(report, "relevance", "Relevance: q-009 finds doc-008", {
        let (status, body) = client.query("gRPC Protocol Buffers");
        assert_eq!(status, 200);
        let results = body["results"].as_array();
        assert!(
            results.map(|a| !a.is_empty()).unwrap_or(false),
            "Expected non-empty results for gRPC query: {body}"
        );
        let has_doc = results
            .map(|arr| {
                arr.iter().any(|r| {
                    let id = r["id"].as_str().unwrap_or("");
                    id == "doc-008"
                        || id.contains("008")
                        || r["content"]
                            .as_str()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains("grpc")
                        || r["title"]
                            .as_str()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains("grpc")
                })
            })
            .unwrap_or(false);
        assert!(has_doc, "Expected gRPC-related doc in results, got: {body}");
    });

    // Cache populated after relevance queries
    run_test!(report, "relevance", "Relevance: cache populated", {
        let (_, stats) = client.stats();
        let total = stats["cache"]["total"].as_u64().unwrap_or(0);
        assert!(total >= 2, "Expected >= 2 cache entries, got {total}");
    });

    eprintln!("--------------------------------------------");
}
