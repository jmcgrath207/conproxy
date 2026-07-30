//! UAT tests for conproxy CLI commands.
//!
//! Exercises `conproxy search` against a proxy + Docker backends.
//!
//! Run with: `cargo test --test e2e_uat --features e2e -- --ignored --nocapture`
//!
//! Prerequisites:
//!   - Docker services running with test data loaded
//!   - PROXY_BIN optional (defaults to target/release/conproxy)

use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

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

/// Shared fixture: temp config dir + running proxy on 8080/8081.
struct UatFixture {
    cwd: PathBuf,
    _child: Child,
}

fn fixture() -> &'static UatFixture {
    static FIX: OnceLock<UatFixture> = OnceLock::new();
    FIX.get_or_init(|| {
        let cwd = tempfile::tempdir().expect("tempdir").keep();
        let conproxy_dir = cwd.join(".conproxy");
        std::fs::create_dir_all(&conproxy_dir).expect("mkdir .conproxy");

        // Point CLI + server at meili-1 from e2e compose (load-data seeds conproxy_test)
        let toml = r#"[server]
listen = "127.0.0.1:8080"

[proxy]
listen = "127.0.0.1:8080"
max_entries = 1000

[upstreams.meili]
url = "http://localhost:7700"
type = "meilisearch"
index = "conproxy_test"
api_key = "conproxy_test_key"

[contexts.default]
default = true

[[contexts.default.upstreams]]
ref = "meili"
priority = 0
"#;
        let config_path = conproxy_dir.join("conproxy.toml");
        std::fs::write(&config_path, toml).expect("write config");

        let child = Command::new(conproxy_bin())
            .current_dir(&cwd)
            .args([
                "start",
                "--listen",
                "127.0.0.1:8080",
                "--config",
                config_path.to_str().expect("utf8 path"),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn conproxy");

        // Wait for HTTP health
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("http client");
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if Instant::now() > deadline {
                panic!("UAT proxy failed to become healthy on :8081");
            }
            if let Ok(resp) = client.get("http://127.0.0.1:8081/health").send() {
                if resp.status().is_success() {
                    break;
                }
            }
            thread::sleep(Duration::from_millis(200));
        }

        UatFixture { cwd, _child: child }
    })
}

fn run_search(query: &str, limit: usize, format: &str) -> Output {
    let fix = fixture();
    Command::new(conproxy_bin())
        .current_dir(&fix.cwd)
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

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
#[ignore = "E2E UAT: requires Docker services + proxy"]
fn search_returns_results() {
    let out = run_search("Tokio async runtime Rust", 5, "json");
    assert!(
        out.status.success(),
        "conproxy search failed: {}",
        stderr(&out)
    );

    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("Failed to parse search JSON output");

    let results = json["results"].as_array().expect("Missing results array");
    assert!(!results.is_empty(), "Expected at least one search result");

    let first = &results[0];
    assert!(first["id"].is_string(), "Result missing id");
    assert!(first["score"].is_number(), "Result missing score");
    assert!(first["content"].is_string(), "Result missing content");
}

#[test]
#[ignore = "E2E UAT: requires Docker services + proxy"]
fn search_respects_limit() {
    let out = run_search("Meilisearch full-text search", 2, "json");
    assert!(
        out.status.success(),
        "conproxy search failed: {}",
        stderr(&out)
    );

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
    let out = run_search("BM25 ranking algorithm", 3, "text");
    assert!(
        out.status.success(),
        "conproxy search failed: {}",
        stderr(&out)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Found") || stdout.contains("No results"),
        "Text output unexpected: {stdout}"
    );
    if stdout.contains("Found") {
        assert!(
            stdout.contains("score:"),
            "Text output should contain score"
        );
    }
}

#[test]
#[ignore = "E2E UAT: requires Docker services + proxy"]
fn search_cache_behavior() {
    let q = "gRPC Protocol Buffers uat-cache";
    let out1 = run_search(q, 3, "json");
    assert!(
        out1.status.success(),
        "first search failed: {}",
        stderr(&out1)
    );

    let out2 = run_search(q, 3, "json");
    assert!(
        out2.status.success(),
        "second search failed: {}",
        stderr(&out2)
    );

    let json2: serde_json::Value = serde_json::from_slice(&out2.stdout).unwrap();
    let cache_status = json2["cache_status"].as_str().unwrap_or("").to_lowercase();
    assert!(
        cache_status == "hit" || cache_status == "fresh",
        "Second identical search should be a cache hit, got {cache_status:?}"
    );
}
