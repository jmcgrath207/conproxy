use crate::helpers::client::E2eClient;
use crate::helpers::config::ConfigManager;
use crate::helpers::constants::{category_enabled, proxy_url, Suite};
use crate::helpers::metrics::snapshot_and_reset;
use crate::helpers::proxy::ProxyProcess;
use crate::helpers::report::TestReport;
use crate::{assert_section, run_test};
use std::path::PathBuf;
use std::time::Duration;

/// Advanced tests require a proxy restart with specialized config (auth + rate_limit + short TTL).
/// This function manages its own ProxyProcess and snapshots metrics per sub-section.
pub fn run(suite: Suite, report: &mut TestReport) {
    if !category_enabled("advanced") || !suite.is_all() {
        return;
    }

    eprintln!();
    eprintln!("\x1b[1mAdvanced Feature Tests (proxy restart)\x1b[0m");
    eprintln!("--------------------------------------------");

    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut config = ConfigManager::new(&project_root);

    // Write advanced config
    config.write_advanced_config();

    // Start proxy with advanced config
    eprintln!("\x1b[32m[INFO]\x1b[0m Starting proxy with rate limit + auth + short TTL...");
    let mut proxy = ProxyProcess::start("127.0.0.1:8080");
    if let Err(e) = proxy.wait_healthy(Duration::from_secs(5)) {
        eprintln!("\x1b[31m[ERROR]\x1b[0m Advanced proxy failed to start: {e}");
        proxy.stop();
        config.restore();
        return;
    }

    // Client WITHOUT auth (for rejection tests)
    let client_no_auth = E2eClient::new(proxy_url());
    // Client WITH auth
    let client_auth = E2eClient::new(proxy_url()).with_api_key("e2e-test-key");

    // --- Auth Tests ---
    // ~0% hit rate: 2 public endpoint checks (not counted), 2 rejected (401), 2 valid queries
    run_auth(&client_no_auth, &client_auth, report);
    let s = snapshot_and_reset(&client_auth, "auth", report);
    assert_section!(report, s, "auth", max_errors: 0);

    // --- Rate Limiting Tests ---
    // ~0% hit rate: burst of unique queries + recovery, each query string is unique
    run_rate_limit(&client_auth, report);
    let s = snapshot_and_reset(&client_auth, "rate-limit", report);
    assert_section!(report, s, "rate-limit", max_errors: 0);

    // Let the token bucket fully refill before TTL tests (refill=10/s, burst=5)
    std::thread::sleep(Duration::from_secs(1));

    // --- TTL Expiration Tests ---
    // ~25% hit rate: 4 queries (1 miss, 1 hit, 1 stale/miss, 1 miss after full expiry)
    run_ttl(&client_auth, report);
    let s = snapshot_and_reset(&client_auth, "ttl", report);
    assert_section!(report, s, "ttl", min_hits: 1, max_errors: 0);

    // Cleanup: stop advanced proxy, restore config
    eprintln!("\x1b[32m[INFO]\x1b[0m Stopping advanced proxy...");
    proxy.stop();

    // Restore original config (suite config will be re-injected by caller)
    config.restore();

    eprintln!("--------------------------------------------");
}

fn run_auth(client_no_auth: &E2eClient, client_auth: &E2eClient, report: &mut TestReport) {
    eprintln!();
    eprintln!("\x1b[32m[INFO]\x1b[0m Auth tests...");

    // Health accessible without key (public)
    run_test!(report, "auth", "Auth: health without key", {
        let (status, body) = client_no_auth.health();
        assert_eq!(status, 200);
        assert!(body["status"].is_string());
    });

    // Ready accessible without key (public)
    run_test!(report, "auth", "Auth: ready without key", {
        let (status, body) = client_no_auth.ready();
        assert_eq!(status, 200);
        assert!(!body["ready"].is_null());
    });

    // Query without key → 401
    run_test!(report, "auth", "Auth: query without key (401)", {
        let (status, _) = client_no_auth.query_no_auth("auth test");
        assert_eq!(status, 401, "Expected 401 without key, got {status}");
    });

    // Query with wrong key → 401
    run_test!(report, "auth", "Auth: wrong key (401)", {
        let (status, _) = client_no_auth.query_with_key("auth test", "wrong-key");
        assert_eq!(status, 401, "Expected 401 with wrong key, got {status}");
    });

    // Query with valid X-API-Key → 200
    run_test!(report, "auth", "Auth: valid X-API-Key (200)", {
        let (status, body) = client_auth.query("auth test");
        assert_eq!(status, 200);
        assert!(body["results"].is_array(), "Missing results with valid key");
    });

    // Query with valid Bearer token → 200
    run_test!(report, "auth", "Auth: valid Bearer (200)", {
        let (status, body) = client_no_auth.query_with_bearer("auth bearer test", "e2e-test-key");
        assert_eq!(status, 200);
        assert!(body["results"].is_array());
    });
}

fn run_rate_limit(client_auth: &E2eClient, report: &mut TestReport) {
    eprintln!();
    eprintln!("\x1b[32m[INFO]\x1b[0m Rate limiting tests (burst_size=5, refill=10/s)...");

    // Send rapid burst to exhaust tokens
    let mut rate_limit_hit = false;
    for i in 0..8 {
        let (status, _) = client_auth.query(&format!("rate limit burst {i}"));
        if status == 429 {
            rate_limit_hit = true;
            break;
        }
    }

    run_test!(report, "rate-limit", "Rate limit: 429 on burst", {
        assert!(rate_limit_hit, "Expected 429 rate limit hit during burst");
    });

    // Wait for refill
    std::thread::sleep(Duration::from_secs(1));

    // Should work after refill
    run_test!(report, "rate-limit", "Rate limit: works after refill", {
        let (status, body) = client_auth.query("rate limit recovered");
        assert_eq!(status, 200);
        assert!(body["results"].is_array());
    });
}

fn run_ttl(client_auth: &E2eClient, report: &mut TestReport) {
    eprintln!();
    eprintln!("\x1b[32m[INFO]\x1b[0m TTL expiration tests (fresh=5s, stale=10s)...");

    // Clear cache
    client_auth.cache_clear();

    // Initial query: cache miss
    run_test!(report, "ttl", "TTL: initial miss", {
        let (status, body) = client_auth.query("ttl expiration test");
        assert_eq!(status, 200);
        assert_eq!(body["cache_status"].as_str(), Some("miss"));
    });

    // Immediate re-query: cache hit
    run_test!(report, "ttl", "TTL: fresh hit", {
        let (status, body) = client_auth.query("ttl expiration test");
        assert_eq!(status, 200);
        assert_eq!(body["cache_status"].as_str(), Some("hit"));
    });

    // Wait for fresh → stale transition (5s + jitter margin)
    eprintln!("\x1b[32m[INFO]\x1b[0m Waiting 7s for TTL fresh→stale transition...");
    std::thread::sleep(Duration::from_secs(7));

    run_test!(report, "ttl", "TTL: stale after fresh expires", {
        let (status, body) = client_auth.query("ttl expiration test");
        assert_eq!(status, 200);
        let cs = body["cache_status"].as_str().unwrap_or("");
        assert!(
            cs == "stale" || cs == "miss",
            "Expected stale or miss, got {cs}"
        );
    });

    // Wait for stale → expired (10s more)
    eprintln!("\x1b[32m[INFO]\x1b[0m Waiting 12s for TTL stale→expired transition...");
    std::thread::sleep(Duration::from_secs(12));

    run_test!(report, "ttl", "TTL: miss after full expiry", {
        let (status, body) = client_auth.query("ttl expiration test");
        assert_eq!(status, 200);
        assert_eq!(body["cache_status"].as_str(), Some("miss"));
    });
}
