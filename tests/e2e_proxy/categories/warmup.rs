use crate::helpers::client::E2eClient;
use crate::helpers::constants::{category_enabled, Suite};
use crate::helpers::report::TestReport;
use crate::run_test;
use std::path::PathBuf;

pub fn run(client: &E2eClient, suite: Suite, report: &mut TestReport) {
    if !category_enabled("warmup") || !suite.has_text_upstreams() {
        return;
    }

    eprintln!();
    eprintln!("\x1b[1mSeed File Bulk Warmup Tests\x1b[0m");
    eprintln!("--------------------------------------------");

    // Clear cache before bulk warmup
    client.cache_clear();

    // Bulk warmup via /cache/warmup (must fetch_from_upstream to populate cache)
    run_test!(report, "warmup", "Warmup: bulk from seeds", {
        let (status, body) = client.warmup_with_fetch(&[
            "rust programming language",
            "vector database embeddings",
            "elasticsearch full text search",
            "axum web framework rust",
            "cache ttl lru eviction",
        ]);
        assert_eq!(status, 200);
        let warmed = body["warmed"].as_u64().unwrap_or(0);
        assert!(warmed > 0, "Expected > 0 warmed entries, got {warmed}");
    });

    // Cache has entries after bulk warmup
    run_test!(report, "warmup", "Warmup: cache populated after bulk", {
        let (_, stats) = client.stats();
        let total = stats["cache"]["total"].as_u64().unwrap_or(0);
        assert!(
            total >= 3,
            "Expected >= 3 cache entries after warmup, got {total}"
        );
    });

    // Bulk warmup from file via REST API
    run_test!(report, "warmup", "Warmup: bulk fetch from file", {
        let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let seeds_fts = project_root.join("tests/e2e/data/seeds_fts.txt");
        let content = std::fs::read_to_string(&seeds_fts).unwrap_or_default();
        let queries: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        assert!(!queries.is_empty(), "seeds_fts.txt is empty or missing");
        let owned: Vec<String> = queries.iter().map(|s| s.to_string()).collect();
        let refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
        let (status, body) = client.warmup_with_fetch(&refs);
        assert_eq!(status, 200, "Bulk warmup failed: {body}");
        let warmed = body["warmed"].as_u64().unwrap_or(0);
        assert!(warmed > 0, "Expected > 0 warmed entries, got {warmed}");
    });

    // Cache still populated after file warmup
    run_test!(report, "warmup", "Warmup: requests increased", {
        let (_, stats) = client.stats();
        let total = stats["cache"]["total"].as_u64().unwrap_or(0);
        assert!(
            total >= 5,
            "Expected >= 5 cache entries after warmup, got {total}"
        );
    });

    eprintln!("--------------------------------------------");
}
