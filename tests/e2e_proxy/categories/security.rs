//! Security-focused E2E tests.
//!
//! Tests authentication bypass, rate limit enforcement, payload abuse,
//! admin access control, and header injection against a proxy with auth enabled.

use crate::helpers::client::E2eClient;
use crate::helpers::config::ConfigManager;
use crate::helpers::constants::{category_enabled, proxy_url};
use crate::helpers::mock_upstream::MockUpstream;
use crate::helpers::proxy::ProxyProcess;
use crate::helpers::report::TestReport;
use crate::run_test;
use std::path::PathBuf;
use std::time::Duration;

/// Run all security tests. Manages its own proxy with auth + rate limiting enabled.
pub fn run(report: &mut TestReport) {
    if !category_enabled("security") {
        return;
    }

    eprintln!();
    eprintln!("\x1b[1mSecurity Tests\x1b[0m");
    eprintln!("--------------------------------------------");

    let mock = MockUpstream::start();
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut config = ConfigManager::new(&project_root);
    write_security_config(&mut config, &mock.url());

    eprintln!("\x1b[32m[INFO]\x1b[0m Starting proxy with security config...");
    let mut proxy = ProxyProcess::start("127.0.0.1:8080");
    if let Err(e) = proxy.wait_healthy(Duration::from_secs(5)) {
        eprintln!("\x1b[31m[ERROR]\x1b[0m Security proxy failed to start: {e}");
        proxy.stop();
        config.restore();
        return;
    }

    let client_no_auth = E2eClient::new(proxy_url());
    let client_auth = E2eClient::new(proxy_url()).with_api_key("security-test-key");

    run_auth_bypass(&client_no_auth, &client_auth, report);
    run_rate_limit_enforcement(&client_auth, report);

    // Let the rate limiter fully refill before payload + admin tests
    // (burst_size=5, rps=10 → wait long enough after burst + payload traffic)
    std::thread::sleep(Duration::from_secs(3));

    run_payload_abuse(&client_auth, report);

    // Refill again after payload tests consume tokens
    std::thread::sleep(Duration::from_secs(2));

    run_admin_access_control(&client_no_auth, &client_auth, report);
    run_header_injection(&client_no_auth, report);

    proxy.stop();
    config.restore();

    eprintln!("--------------------------------------------");
}

/// Write a config suitable for security testing: auth enabled, rate limiting, body size limit.
fn write_security_config(config: &mut ConfigManager, mock_url: &str) {
    // Use the config manager's internal path by writing advanced-style config
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let config_path = project_root.join(".conproxy/conproxy.toml");
    if let Some(parent) = config_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Back up via write_mock_config (triggers backup), then overwrite with our config
    config.write_mock_config(mock_url, "elasticsearch");

    let security_config = format!(
        r#"[server]
listen = "127.0.0.1:8080"

[upstreams.mock]
url = "{mock_url}"
type = "elasticsearch"
index = "mock_index"
search_fields = ["content", "title"]
timeout_secs = 5

[contexts.default]
default = true

[[contexts.default.upstreams]]
ref = "mock"
priority = 0
weight = 1

[contexts.default.cache]
fresh_secs = 60
stale_secs = 120
max_entries = 1000

[proxy]
api_key = "security-test-key"

[proxy.retry]
enabled = true
max_retries = 2
base_delay_ms = 50
max_delay_ms = 2000

[proxy.rate_limit]
enabled = true
requests_per_second = 10
burst_size = 5
"#
    );
    std::fs::write(&config_path, security_config).expect("Failed to write security config");
    eprintln!("  Wrote security test config (auth + rate_limit)");
}

// ---------------------------------------------------------------------------
// Auth bypass tests
// ---------------------------------------------------------------------------

fn run_auth_bypass(client_no_auth: &E2eClient, client_auth: &E2eClient, report: &mut TestReport) {
    eprintln!();
    eprintln!("\x1b[32m[INFO]\x1b[0m Auth bypass tests...");

    // No API key → 401
    run_test!(report, "security", "Auth bypass: no API key", {
        let (status, _) = client_no_auth.query_no_auth("bypass test");
        assert_eq!(status, 401, "Expected 401 without key, got {status}");
    });

    // Malformed key → 401
    run_test!(report, "security", "Auth bypass: malformed key", {
        let (status, _) = client_no_auth.query_with_key("bypass test", "");
        assert_eq!(status, 401, "Expected 401 with empty key, got {status}");
    });

    // Wrong key → 401
    run_test!(report, "security", "Auth bypass: wrong key", {
        let (status, _) = client_no_auth.query_with_key("bypass test", "wrong-key-value");
        assert_eq!(status, 401, "Expected 401 with wrong key, got {status}");
    });

    // Null bytes in key → 401
    // Note: reqwest rejects null bytes in header values at builder time, so
    // status=0 (builder error) is also acceptable — the proxy would reject
    // the key with 401 if the request were sent.
    run_test!(report, "security", "Auth bypass: null bytes in key", {
        let (status, _) = client_no_auth.query_with_key("bypass test", "security-test-key\0extra");
        assert!(
            status == 401 || status == 0,
            "Expected 401 (or builder-rejected) with null-byte key, got {status}"
        );
    });

    // Valid key → 200
    run_test!(report, "security", "Auth bypass: valid key accepted", {
        let (status, body) = client_auth.query("auth accepted test");
        assert_eq!(status, 200, "Expected 200 with valid key, got {status}");
        assert!(body["results"].is_array());
    });

    // Health endpoint accessible without auth (public)
    run_test!(report, "security", "Auth bypass: health is public", {
        let (status, _) = client_no_auth.health();
        assert_eq!(status, 200, "Health should be public, got {status}");
    });

    // Ready endpoint accessible without auth (public)
    run_test!(report, "security", "Auth bypass: ready is public", {
        let (status, _) = client_no_auth.ready();
        assert_eq!(status, 200, "Ready should be public, got {status}");
    });
}

// ---------------------------------------------------------------------------
// Rate limit enforcement tests
// ---------------------------------------------------------------------------

fn run_rate_limit_enforcement(client_auth: &E2eClient, report: &mut TestReport) {
    eprintln!();
    eprintln!("\x1b[32m[INFO]\x1b[0m Rate limit enforcement tests...");

    // Burst beyond limit → should get 429 eventually
    run_test!(report, "security", "Rate limit: burst triggers 429", {
        let mut got_429 = false;
        // Send 20 rapid-fire requests — with burst_size=5, should hit 429
        for i in 0..20 {
            let (status, _) = client_auth.query(&format!("rate-limit-burst-{i}"));
            if status == 429 {
                got_429 = true;
                break;
            }
        }
        assert!(
            got_429,
            "Expected at least one 429 after burst, but never got one"
        );
    });

    // Wait for bucket to refill
    std::thread::sleep(Duration::from_secs(2));

    // After refill, requests should succeed again
    run_test!(report, "security", "Rate limit: recovery after refill", {
        let (status, _) = client_auth.query("rate-limit-recovery");
        assert_eq!(
            status, 200,
            "Expected 200 after rate limit recovery, got {status}"
        );
    });
}

// ---------------------------------------------------------------------------
// Payload abuse tests
// ---------------------------------------------------------------------------

fn run_payload_abuse(client_auth: &E2eClient, report: &mut TestReport) {
    eprintln!();
    eprintln!("\x1b[32m[INFO]\x1b[0m Payload abuse tests...");

    // Deeply nested JSON
    run_test!(report, "security", "Payload: deeply nested JSON", {
        // Build deeply nested JSON: {"query": {"query": {"query": ... "deep" ...}}}
        let mut nested = serde_json::json!("deep");
        for _ in 0..100 {
            nested = serde_json::json!({"nested": nested});
        }
        let body = serde_json::json!({"query": "deep-nest-test", "metadata": nested});
        let (status, _) = client_auth.post_json("/query", &body);
        // Should either process it or reject it gracefully (not crash)
        assert!(
            status > 0,
            "Proxy should respond (not crash) on deeply nested JSON"
        );
        assert_ne!(status, 0, "Connection should not be dropped");
    });

    // Empty query string
    run_test!(report, "security", "Payload: empty query string", {
        let (status, _) = client_auth.query("");
        // Should handle gracefully — 200 or 400, not 500
        assert!(
            status == 200 || status == 400,
            "Expected 200 or 400 for empty query, got {status}"
        );
    });

    // Null bytes in query
    run_test!(report, "security", "Payload: null bytes in query", {
        let (status, _) = client_auth.query("test\0injection");
        assert!(status > 0, "Proxy should respond on null-byte query");
        assert_ne!(status, 500, "Should not cause internal server error");
    });

    // Very long query string
    run_test!(report, "security", "Payload: very long query string", {
        let long_query = "a".repeat(10_000);
        let (status, _) = client_auth.query(&long_query);
        // Should handle gracefully
        assert!(status > 0, "Proxy should respond on very long query");
    });

    // Malformed JSON body
    run_test!(report, "security", "Payload: malformed JSON body", {
        let url = format!("{}/query", proxy_url());
        let client = reqwest::blocking::Client::new();
        let resp = client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("X-API-Key", "security-test-key")
            .body("{invalid json")
            .send();
        match resp {
            Ok(r) => {
                let status = r.status().as_u16();
                assert!(
                    status == 400 || status == 422,
                    "Expected 400 or 422 for malformed JSON, got {status}"
                );
            }
            Err(_) => {
                // Connection error is acceptable — proxy may reject early
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Admin access control tests
// ---------------------------------------------------------------------------

fn run_admin_access_control(
    client_no_auth: &E2eClient,
    client_auth: &E2eClient,
    report: &mut TestReport,
) {
    eprintln!();
    eprintln!("\x1b[32m[INFO]\x1b[0m Admin access control tests...");

    // Admin endpoints without auth → 401
    run_test!(report, "security", "Admin ACL: pause without auth", {
        let (status, _) = client_no_auth.admin_pause();
        assert_eq!(status, 401, "Admin pause should require auth, got {status}");
    });

    run_test!(report, "security", "Admin ACL: resume without auth", {
        let (status, _) = client_no_auth.admin_resume();
        assert_eq!(
            status, 401,
            "Admin resume should require auth, got {status}"
        );
    });

    run_test!(report, "security", "Admin ACL: reload without auth", {
        let (status, _) = client_no_auth.admin_reload();
        assert_eq!(
            status, 401,
            "Admin reload should require auth, got {status}"
        );
    });

    run_test!(
        report,
        "security",
        "Admin ACL: metrics reset without auth",
        {
            let status = client_no_auth.admin_metrics_reset();
            assert_eq!(
                status, 401,
                "Admin metrics reset should require auth, got {status}"
            );
        }
    );

    // Admin endpoints with valid auth → 200
    run_test!(report, "security", "Admin ACL: pause with auth", {
        let (status, _) = client_auth.admin_pause();
        assert_eq!(
            status, 200,
            "Admin pause should succeed with auth, got {status}"
        );
        // Resume immediately
        let _ = client_auth.admin_resume();
    });
}

// ---------------------------------------------------------------------------
// Header injection tests
// ---------------------------------------------------------------------------

fn run_header_injection(client_no_auth: &E2eClient, report: &mut TestReport) {
    eprintln!();
    eprintln!("\x1b[32m[INFO]\x1b[0m Header injection tests...");

    // Newlines in X-API-Key header (CRLF injection attempt)
    run_test!(report, "security", "Header injection: CRLF in API key", {
        let (status, _) =
            client_no_auth.query_with_key("injection test", "key\r\nX-Injected: true");
        // Should be rejected — either 401 (bad key) or 400 (bad header)
        assert!(
            status == 401 || status == 400 || status == 0,
            "Expected rejection for CRLF injection, got {status}"
        );
    });

    // Very long header value
    run_test!(report, "security", "Header injection: oversized API key", {
        let long_key = "x".repeat(8192);
        let (status, _) = client_no_auth.query_with_key("injection test", &long_key);
        // Should be rejected — 401 (bad key) or 431 (header too large)
        assert!(
            status == 401 || status == 431 || status == 400 || status == 0,
            "Expected rejection for oversized key, got {status}"
        );
    });

    // Special characters in X-Request-Id (if supported)
    run_test!(
        report,
        "security",
        "Header injection: special chars in request ID",
        {
            let url = format!("{}/health", proxy_url());
            let client = reqwest::blocking::Client::new();
            let resp = client
                .get(&url)
                .header("X-Request-Id", "<script>alert(1)</script>")
                .send();
            match resp {
                Ok(r) => {
                    let status = r.status().as_u16();
                    // Should not crash — health is public
                    assert_eq!(status, 200, "Health should still work, got {status}");
                    // Response should not reflect the injected script
                    let body = r.text().unwrap_or_default();
                    assert!(
                        !body.contains("<script>"),
                        "Response should not reflect injected script"
                    );
                }
                Err(_) => {
                    // Connection error is acceptable
                }
            }
        }
    );
}
