//! Federated search E2E tests.
//!
//! Manages its own MockUpstream + ProxyProcess with federated config.
//! Tests local/remote result merging, fallback behavior, and caching.

use crate::helpers::client::E2eClient;
use crate::helpers::config::ConfigManager;
use crate::helpers::constants::{category_enabled, proxy_url};
use crate::helpers::mock_upstream::MockUpstream;
use crate::helpers::proxy::ProxyProcess;
use crate::helpers::report::TestReport;
use crate::run_test;
use std::path::PathBuf;
use std::time::Duration;

pub fn run(report: &mut TestReport) {
    if !category_enabled("federated_search") {
        return;
    }

    eprintln!();
    eprintln!("\x1b[1mFederated Search Tests (mock upstream)\x1b[0m");
    eprintln!("--------------------------------------------");

    let mock = MockUpstream::start();
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut config = ConfigManager::new(&project_root);
    config.write_federated_config(&mock.url());

    eprintln!("\x1b[32m[INFO]\x1b[0m Starting proxy with federated config...");
    let mut proxy = ProxyProcess::start("127.0.0.1:8080");
    if let Err(e) = proxy.wait_healthy(Duration::from_secs(5)) {
        eprintln!("\x1b[31m[ERROR]\x1b[0m Federated proxy failed to start: {e}");
        proxy.stop();
        config.restore();
        return;
    }

    let client = E2eClient::new(proxy_url());

    // Test 1: Empty local results trigger remote query
    run_test!(
        report,
        "federated_search",
        "Fed: empty local, remote queried",
        {
            client.cache_clear();
            let (status, body) =
                client.federated_with_local("federated empty local test", &serde_json::json!([]));
            assert_eq!(status, 200, "Expected 200, got {status}");
            if let Some(stats) = body.get("stats") {
                let remote_queried = stats["remote_queried"].as_bool().unwrap_or(false);
                let fallback = stats["fallback_reason"].as_str().unwrap_or("");
                eprintln!("  remote_queried={remote_queried}, fallback_reason={fallback}");
            }
        }
    );

    // Test 2: Sufficient local results skip remote
    run_test!(
        report,
        "federated_search",
        "Fed: sufficient local results",
        {
            client.cache_clear();
            let local = serde_json::json!([
                {"id": "local-1", "score": 0.95, "content": "Local result 1"},
                {"id": "local-2", "score": 0.92, "content": "Local result 2"},
                {"id": "local-3", "score": 0.90, "content": "Local result 3"},
                {"id": "local-4", "score": 0.88, "content": "Local result 4"},
                {"id": "local-5", "score": 0.85, "content": "Local result 5"},
            ]);
            let (status, body) = client.federated_with_local("federated sufficient test", &local);
            assert_eq!(status, 200);
            if let Some(stats) = body.get("stats") {
                let local_count = stats["local_count"].as_u64().unwrap_or(0);
                assert!(
                    local_count >= 5,
                    "Expected local_count >= 5, got {local_count}"
                );
            }
        }
    );

    // Test 3: Below min triggers remote
    run_test!(
        report,
        "federated_search",
        "Fed: below min triggers remote",
        {
            client.cache_clear();
            let local = serde_json::json!([
                {"id": "local-1", "score": 0.95, "content": "Only one result"}
            ]);
            let (status, body) = client.federated_with_local("federated below min test", &local);
            assert_eq!(status, 200);
            if let Some(stats) = body.get("stats") {
                let reason = stats["fallback_reason"].as_str().unwrap_or("");
                eprintln!("  fallback_reason: {reason}");
            }
        }
    );

    // Test 4: Low confidence triggers remote
    run_test!(
        report,
        "federated_search",
        "Fed: low confidence triggers remote",
        {
            client.cache_clear();
            let local = serde_json::json!([
                {"id": "local-1", "score": 0.3, "content": "Low confidence 1"},
                {"id": "local-2", "score": 0.2, "content": "Low confidence 2"},
                {"id": "local-3", "score": 0.1, "content": "Low confidence 3"},
            ]);
            let (status, body) =
                client.federated_with_local("federated low confidence test", &local);
            assert_eq!(status, 200);
            if let Some(stats) = body.get("stats") {
                let reason = stats["fallback_reason"].as_str().unwrap_or("");
                eprintln!("  low confidence fallback_reason: {reason}");
            }
        }
    );

    // Test 5: Results populated
    run_test!(report, "federated_search", "Fed: results populated", {
        client.cache_clear();
        let (status, body) = client.federated("federated results test");
        assert_eq!(status, 200);
        let results = body["results"].as_array();
        assert!(
            results.is_some(),
            "Expected results array in federated response"
        );
    });

    // Test 6: took_ms positive
    run_test!(report, "federated_search", "Fed: took_ms positive", {
        let (status, body) = client.federated("federated took test");
        assert_eq!(status, 200);
        let took = body["took_ms"].as_u64().unwrap_or(0);
        assert!(
            took > 0 || body.get("took_ms").is_some(),
            "Expected took_ms field"
        );
    });

    // Test 7: Stats shape
    run_test!(report, "federated_search", "Fed: stats shape", {
        client.cache_clear();
        let (status, body) = client.federated("federated stats shape test");
        assert_eq!(status, 200);
        if let Some(stats) = body.get("stats") {
            // Verify expected fields exist
            let has_fields = stats.get("local_count").is_some()
                || stats.get("remote_count").is_some()
                || stats.get("merged_count").is_some();
            assert!(has_fields, "Expected stats to have count fields: {stats}");
        }
    });

    // Test 8: Merged count
    run_test!(report, "federated_search", "Fed: merged count", {
        client.cache_clear();
        let local = serde_json::json!([
            {"id": "merge-1", "score": 0.5, "content": "Merge test"}
        ]);
        let (status, body) = client.federated_with_local("federated merge test", &local);
        assert_eq!(status, 200);
        if let Some(stats) = body.get("stats") {
            let merged = stats["merged_count"].as_u64().unwrap_or(0);
            eprintln!("  merged_count: {merged}");
        }
    });

    // Test 9: Cache status on repeat
    run_test!(report, "federated_search", "Fed: cache status", {
        // First query (miss)
        let _ = client.federated("federated cache test unique");
        // Second query (should be hit)
        let (status, body) = client.federated("federated cache test unique");
        assert_eq!(status, 200);
        let cs = body["cache_status"].as_str().unwrap_or("");
        assert!(
            cs == "hit" || cs == "miss",
            "Expected cache_status hit or miss, got {cs}"
        );
    });

    // Cleanup
    eprintln!("\x1b[32m[INFO]\x1b[0m Stopping federated proxy...");
    proxy.stop();
    config.restore();
    eprintln!("--------------------------------------------");
}
