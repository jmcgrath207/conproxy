//! UAT tests for conproxy CLI commands.
//!
//! Exercises `conproxy search` and other user-facing CLI commands against
//! a running proxy with Docker backends.
//!
//! Run with: `cargo test --test e2e_uat --features e2e -- --ignored --nocapture`
//!
//! Prerequisites:
//!   - Docker services running (Qdrant + Elasticsearch)
//!   - Test data loaded
//!   - Proxy running on 127.0.0.1:8080

use std::path::PathBuf;
use std::process::{Command, Output};

fn conproxy_bin() -> PathBuf {
    std::env::var("PROXY_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join("release")
                .join("conproxy")
        })
}

fn proxy_listen() -> String {
    std::env::var("UAT_PROXY_LISTEN").unwrap_or_else(|_| "127.0.0.1:8080".into())
}

fn run_search(query: &str, limit: usize, format: &str) -> Output {
    Command::new(conproxy_bin())
        .args([
            "search",
            query,
            "--limit",
            &limit.to_string(),
            "--format",
            format,
        ])
        .output()
        .expect("Failed to run conproxy search")
}

#[test]
#[ignore = "E2E UAT: requires Docker services + proxy"]
fn search_returns_results() {
    let _ = proxy_listen(); // ensure proxy is expected
    let out = run_search("test query", 5, "json");
    assert!(
        out.status.success(),
        "conproxy search failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("Failed to parse search JSON output");

    let results = json["results"].as_array().expect("Missing results array");
    assert!(!results.is_empty(), "Expected at least one search result");

    // Verify result structure
    let first = &results[0];
    assert!(first["id"].is_string(), "Result missing id");
    assert!(first["score"].is_number(), "Result missing score");
    assert!(first["content"].is_string(), "Result missing content");
}

#[test]
#[ignore = "E2E UAT: requires Docker services + proxy"]
fn search_respects_limit() {
    let out = run_search("test", 2, "json");
    assert!(out.status.success());

    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let results = json["results"].as_array().unwrap();
    assert!(
        results.len() <= 2,
        "Expected at most 2 results, got {}",
        results.len()
    );
}

#[test]
#[ignore = "E2E UAT: requires Docker services + proxy"]
fn search_text_format() {
    let out = run_search("test query", 3, "text");
    assert!(out.status.success());

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Found"),
        "Text output should contain 'Found' header"
    );
    assert!(
        stdout.contains("score:"),
        "Text output should contain score"
    );
}

#[test]
#[ignore = "E2E UAT: requires Docker services + proxy"]
fn search_cache_behavior() {
    // First search: may be a miss
    let out1 = run_search("cache test query", 3, "json");
    assert!(out1.status.success());

    // Second identical search: should be a cache hit
    let out2 = run_search("cache test query", 3, "json");
    assert!(out2.status.success());

    let json2: serde_json::Value = serde_json::from_slice(&out2.stdout).unwrap();
    let cache_status = json2["cache_status"].as_str().unwrap_or("");
    assert_eq!(
        cache_status, "Hit",
        "Second identical search should be a cache hit"
    );
}
