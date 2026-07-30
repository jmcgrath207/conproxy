use crate::helpers::client::E2eClient;
use crate::helpers::constants::{category_enabled, Suite};
use crate::helpers::report::TestReport;
use crate::run_test;
use std::path::PathBuf;
use std::process::Command;

pub fn run(client: &E2eClient, suite: Suite, report: &mut TestReport) {
    if !category_enabled("warmup") || !suite.has_text_upstreams() {
        return;
    }

    eprintln!();
    eprintln!("\x1b[1mSeed File Bulk Warmup Tests\x1b[0m");
    eprintln!("--------------------------------------------");

    // Clear cache before bulk warmup
    client.cache_clear();

    // Bulk warmup via /cache/warmup
    run_test!(report, "warmup", "Warmup: bulk from seeds", {
        let (status, body) = client.warmup(&[
            "rust programming language",
            "vector database embeddings",
            "elasticsearch full text search",
            "axum web framework rust",
            "cache ttl lru eviction",
        ]);
        assert_eq!(status, 200);
        let warmed = body["warmed"].as_u64().unwrap_or(0);
        // warmed is u64, always >= 0; just verify the request succeeded
        let _ = warmed;
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

    // CLI seed fetch with bulk file
    run_test!(report, "warmup", "Warmup: CLI bulk fetch from file", {
        let bin = conproxy_bin();
        let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let seeds_fts = project_root.join("tests/e2e/data/seeds_fts.txt");
        let out = Command::new(&bin)
            .current_dir(&project_root)
            .args(["seed", "fetch", "--bulk", seeds_fts.to_str().unwrap()])
            .output()
            .expect("Failed to run CLI seed fetch");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("queries fetched") || out.status.success(),
            "CLI bulk fetch failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    });

    // Stats show increased request count
    run_test!(report, "warmup", "Warmup: requests increased", {
        let (_, metrics) = client.metrics();
        let total = metrics["proxy"]["requests_total"].as_u64().unwrap_or(0);
        assert!(
            total >= 10,
            "Expected >= 10 requests after warmup, got {total}"
        );
    });

    eprintln!("--------------------------------------------");
}

fn conproxy_bin() -> PathBuf {
    if let Ok(bin) = std::env::var("PROXY_BIN") {
        return PathBuf::from(bin);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("release")
        .join("conproxy")
}
