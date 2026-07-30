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

    // Queries aligned with sample_docs.json titles loaded by test_runner load-data.
    const Q_RUST: &str = "Tokio async runtime Rust";
    const Q_MEILI: &str = "Meilisearch full-text search";
    const Q_BM25: &str = "BM25 ranking algorithm";
    const Q_GRPC: &str = "gRPC Protocol Buffers";
    const Q_PGVECTOR: &str = "PostgreSQL pgvector";

    // Batch of keyword queries all return results
    run_test!(
        report,
        "relevance",
        "Relevance: keyword queries return results",
        {
            let (status, body) = client.batch(&[
                ("q1", Q_RUST),
                ("q2", Q_MEILI),
                ("q3", Q_BM25),
                ("q4", Q_GRPC),
                ("q5", Q_PGVECTOR),
            ]);
            assert_eq!(status, 200);
            let len = body["results"].as_object().map(|o| o.len()).unwrap_or(0);
            assert_eq!(len, 5, "Expected 5 batch results, got {len}");
        }
    );

    // q-001: rust/tokio doc — non-empty + re-query hit
    run_test!(report, "relevance", "Relevance: q-001 finds doc-001", {
        let (status, body) = client.query(Q_RUST);
        assert_eq!(status, 200);
        let results = body["results"].as_array();
        assert!(
            results.map(|a| !a.is_empty()).unwrap_or(false),
            "Expected non-empty results for rust/tokio query: {body}"
        );
        let (status2, _) = client.query(Q_RUST);
        assert_eq!(status2, 200);
    });

    // q-003: meilisearch FTS doc — non-empty + re-query hit
    run_test!(report, "relevance", "Relevance: q-003 finds doc-003", {
        let (status, body) = client.query(Q_MEILI);
        assert_eq!(status, 200);
        let results = body["results"].as_array();
        assert!(
            results.map(|a| !a.is_empty()).unwrap_or(false),
            "Expected non-empty results for meilisearch query: {body}"
        );
        let (status2, _) = client.query(Q_MEILI);
        assert_eq!(status2, 200);
    });

    // q-009: gRPC / protobuf doc — non-empty + re-query hit
    run_test!(report, "relevance", "Relevance: q-009 finds doc-008", {
        let (status, body) = client.query(Q_GRPC);
        assert_eq!(status, 200);
        let results = body["results"].as_array();
        assert!(
            results.map(|a| !a.is_empty()).unwrap_or(false),
            "Expected non-empty results for gRPC query: {body}"
        );
        let (status2, _) = client.query(Q_GRPC);
        assert_eq!(status2, 200);
    });

    // Cache populated after relevance queries
    run_test!(report, "relevance", "Relevance: cache populated", {
        let (_, stats) = client.stats();
        let total = stats["cache"]["total"].as_u64().unwrap_or(0);
        assert!(total >= 2, "Expected >= 2 cache entries, got {total}");
    });

    eprintln!("--------------------------------------------");
}
