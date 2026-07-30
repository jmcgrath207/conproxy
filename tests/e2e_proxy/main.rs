//! Rust E2E integration tests for the conproxy cache proxy.
//!
//! Replaces the bash script `tests/e2e/scripts/run_e2e_tests.sh` with ~120 tests
//! across 15 categories. The bash script is kept as a read-only reference.
//!
//! Run with: `cargo test --test e2e_proxy --features proxy -- --ignored --nocapture`
//!
//! Prerequisites:
//!   - Docker services running (`make e2e-services-up`)
//!   - Test data loaded (`make e2e-load-data`)
//!   - Proxy running on 127.0.0.1:8080 with suite-appropriate config
//!
//! Environment variables:
//!   E2E_SUITE      - qdrant | elastic | mixed | all (default: all)
//!   E2E_FILTER     - comma-separated category filter (default: all)
//!   E2E_OUTPUT_DIR - directory to write JSON results
//!   PROXY_BIN      - path to conproxy binary (default: target/release/conproxy)

mod categories;
mod helpers;

#[path = "../test_infra/mod.rs"]
mod test_infra;

use helpers::client::E2eClient;
use helpers::config::ConfigManager;
#[allow(unused_imports)]
use helpers::constants::{
    category_enabled, elastic_url, external_proxy, meili1_url, meili2_url, opensearch_url,
    proxy_url, qdrant_url, Suite,
};
use helpers::metrics::snapshot_and_reset;
use helpers::proxy::ProxyProcess;
use helpers::report::TestReport;
use std::path::PathBuf;
use std::time::Duration;

#[test]
#[ignore = "E2E: requires Docker services + proxy"]
fn e2e_proxy_suite() {
    let suite = Suite::from_env();
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let client = E2eClient::new(proxy_url());
    let mut config = ConfigManager::new(&project_root);
    let mut report = TestReport::new(&suite.to_string());

    // Check if proxy is already running (started externally by Makefile)
    let proxy_external = client.is_up();

    // E2E_EXTERNAL_PROXY=1 forces external-proxy mode (k8s) even if the proxy
    // happens to be reachable on localhost:8080. Categories that manage their
    // own proxy are skipped in that mode (they assume a local 127.0.0.1:8080
    // proxy they can restart; in k8s mode the proxy lives in a port-forward
    // and the test should target it directly).
    let ext_proxy = proxy_external || external_proxy();

    // If proxy is not running, inject config and start it
    let mut _proxy_process: Option<ProxyProcess> = None;
    if !ext_proxy {
        {
            use std::io::Write;
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(
                stderr,
                "\x1b[32m[INFO]\x1b[0m Proxy not running, starting it..."
            );
            let _ = stderr.flush();
        }
        config.inject_suite_config(suite);

        let log_path = std::env::var("E2E_OUTPUT_DIR")
            .ok()
            .map(|d| PathBuf::from(d).join("proxy_logs.txt"));

        let mut proxy = if let Some(path) = log_path {
            ProxyProcess::start_with_log("127.0.0.1:8080", path)
        } else {
            ProxyProcess::start("127.0.0.1:8080")
        };
        if let Err(e) = proxy.wait_healthy(Duration::from_secs(10)) {
            panic!("Proxy failed to start: {e}");
        }
        _proxy_process = Some(proxy);
    }

    assert!(client.is_up(), "Proxy not responding at {}", proxy_url());

    // Optional resource profiling when E2E_PROFILE=1
    #[cfg(target_os = "linux")]
    let (_proc_monitor_handle, mut _full_profiler) = {
        let do_profile = std::env::var("E2E_PROFILE").unwrap_or_default() == "1";
        if do_profile {
            if let Some(ref proxy) = _proxy_process {
                if let Some(pid) = proxy.pid() {
                    {
                        use std::io::Write;
                        let mut stderr = std::io::stderr().lock();
                        let _ = writeln!(
                            stderr,
                            "\x1b[36m[PROFILE]\x1b[0m Starting process monitor (PID: {pid})..."
                        );
                        let _ = stderr.flush();
                    }
                    let lightweight =
                        Some(test_infra::proc_monitor::ProcMonitor::spawn_background(
                            pid,
                            std::time::Duration::from_secs(2),
                        ));
                    let output_dir = std::env::var("E2E_OUTPUT_DIR").ok().map(PathBuf::from);
                    let conproxy_bin = helpers::proxy::conproxy_bin_path();
                    let full = spawn_full_profiler(pid, &conproxy_bin, output_dir.as_deref());
                    (lightweight, full)
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            }
        } else {
            (None, None)
        }
    };
    #[cfg(not(target_os = "linux"))]
    let mut _full_profiler: Option<std::process::Child> = None;

    {
        use std::io::Write;
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr);
        let _ = writeln!(stderr, "\x1b[1mE2E Proxy Tests (suite: {suite})\x1b[0m");
        let _ = writeln!(stderr, "============================================");
        let _ = stderr.flush();
    }

    // Ordering matches bash script exactly:
    // health(1-5) → cache-basics(6-8) → query(9-11) → cache-clear(12-15)
    //   → health-json+multi-upstream(16-19) → snapshot("health+cache+query")
    // → operational(20-29) → snapshot("operational")
    // → metrics(30-40) → cache-correctness(41-43) → snapshot("metrics")
    // → cli → snapshot("cli")
    // → content → snapshot("content")
    // → relevance → snapshot("relevance")
    // → warmup → snapshot("warmup")
    // → resilience(failure+coalescing) → cache-eviction → snapshot("resilience")
    // → reload → snapshot("reload")
    // → cascade → snapshot("cascade")
    // → efficiency → snapshot("efficiency")
    // → advanced → snapshot("advanced")

    // Note on per-section hit rates: metrics are reset between sections so cache
    // entries from earlier phases don't inflate later counters. Most sections use
    // unique queries or clear cache explicitly, so low hit rates are expected.
    // The "efficiency" section is the true cache effectiveness measurement (target >= 80%).
    //
    // Each section asserts its expected cache behavior via `assert_section!`.

    // --- Phase 0.5: gRPC compression ---
    // No cache impact: tests gRPC compression negotiation (gzip, zstd, uncompressed)
    categories::compression::run(&mut report);

    // --- Phase 0.7: socket tuning verification ---
    // No cache impact: inspects OS socket options via ss command
    categories::socket_tuning::run(&client, &mut report);

    // --- Phase 1: health + cache + query ---
    // ~20% hit rate: 1 deliberate repeat ("rust programming" → hit), rest are first-time queries
    categories::health::run(&client, suite, &mut report);
    categories::cache::run(&client, suite, &mut report);
    categories::query::run(&client, suite, &mut report);
    categories::cache::run_clear_and_warmup(&client, suite, &mut report);
    categories::query::run_suite_specific(&client, suite, &mut report);
    let s = snapshot_and_reset(&client, "health+cache+query", &mut report);
    assert_section!(report, s, "health+cache+query", min_hits: 1, max_errors: 0);

    // --- Phase 2: operational ---
    // ~0% hit rate: pause/resume test uses a fresh query after metrics reset
    categories::operational::run(&client, suite, &mut report);
    let s = snapshot_and_reset(&client, "operational", &mut report);
    assert_section!(report, s, "operational", max_errors: 0);

    // --- Phase 3: metrics + cache correctness ---
    // ~33% hit rate: probe query + "vector databases" queried twice (1 deliberate hit)
    categories::metrics_cat::run(&client, suite, &mut report);
    categories::cache::run_correctness(&client, suite, &mut report);
    let s = snapshot_and_reset(&client, "metrics", &mut report);
    assert_section!(report, s, "metrics", min_hits: 1, max_errors: 0);

    // --- Phase 4: cli ---
    // ~20% hit rate: most CLI queries are unique strings, 1 repeat
    categories::cli::run(suite, &mut report);
    let s = snapshot_and_reset(&client, "cli", &mut report);
    assert_section!(report, s, "cli", min_hits: 1, max_errors: 0);

    // --- Phase 5: content ---
    // ~0% hit rate: batch + federated with new query strings, no repeats
    categories::content::run(&client, suite, &mut report);
    let s = snapshot_and_reset(&client, "content", &mut report);
    assert_section!(report, s, "content", max_errors: 0);

    // --- Phase 6: relevance ---
    // ~38% hit rate: cache cleared, batch of 5 (misses), then 3 individual re-queries (hits)
    categories::relevance::run(&client, suite, &mut report);
    let s = snapshot_and_reset(&client, "relevance", &mut report);
    assert_section!(report, s, "relevance", min_hits: 3, min_hit_rate: 0.30, max_errors: 0);

    // --- Phase 7: warmup ---
    // ~22% hit rate: cache cleared, API warmup (5), CLI bulk seeds (~20), few overlaps
    categories::warmup::run(&client, suite, &mut report);
    let s = snapshot_and_reset(&client, "warmup", &mut report);
    assert_section!(report, s, "warmup", min_hits: 3, max_errors: 0);

    // --- Phase 8: resilience + cache eviction ---
    // ~0% hit rate: Docker stop/start, cache cleared for coalescing, errors expected
    categories::resilience::run(&client, suite, &mut report);
    categories::cache::run_eviction(&client, suite, &mut report);
    let _s = snapshot_and_reset(&client, "resilience", &mut report);
    // No hit rate or error assertions — errors are intentional (failure injection)

    // --- Phase 9: reload ---
    // 0 requests: no queries, only /admin/reload calls
    categories::reload::run(&client, suite, &config, &mut report);
    let _s = snapshot_and_reset(&client, "reload", &mut report);

    // --- Phase 10: cascade ---
    // ~0% hit rate: runs after resilience cleared cache, 4 unique queries
    categories::cascade::run(&client, suite, &mut report);
    let s = snapshot_and_reset(&client, "cascade", &mut report);
    assert_section!(report, s, "cascade", max_errors: 0);

    // --- Phase 11: efficiency (the real cache benchmark) ---
    // 100% hit rate: warms 10 queries, re-queries 3 — this is the cache effectiveness test
    categories::efficiency::run(&client, &mut report);
    let s = snapshot_and_reset(&client, "efficiency", &mut report);
    assert_section!(report, s, "efficiency", min_hits: 3, min_hit_rate: 0.80, max_errors: 0);

    // --- Phase 12: advanced (manages own proxy) ---
    // Starts its own proxy with auth + rate_limit + short TTL config.
    // Snapshots metrics internally for 3 sub-sections: auth, rate-limit, ttl.
    // SKIP in external-proxy mode (k8s): can't restart the port-forwarded proxy.
    if !ext_proxy {
        if let Some(ref mut proxy) = _proxy_process {
            proxy.stop();
        }
        categories::advanced::run(suite, &mut report);

        // Restart normal proxy if we manage it and there are remaining phases
        if _proxy_process.is_some() {
            config.inject_suite_config(suite);
            let mut proxy = ProxyProcess::start("127.0.0.1:8080");
            let _ = proxy.wait_healthy(Duration::from_secs(10));
            _proxy_process = Some(proxy);
        }
    } else {
        eprintln!("[SKIP] advanced: external-proxy mode (k8s)");
        report.skip_category("advanced");
    }

    // --- Phase 14: cache observability ---
    // Tests /cache/integrity and /cache/upstreams endpoints
    categories::cache_observability::run(&client, suite, &mut report);
    let _s = snapshot_and_reset(&client, "cache_observability", &mut report);

    // --- Phase 15: context isolation ---
    // Tests cache isolation between different contexts
    categories::context_isolation::run(&client, suite, &mut report);
    let _s = snapshot_and_reset(&client, "context_isolation", &mut report);

    // --- Phase 15b: context-rooted config (own proxy + mock) ---
    if !ext_proxy {
        if let Some(ref mut proxy) = _proxy_process {
            proxy.stop();
        }
        categories::context_rooted::run(&mut report);
        // Restart suite proxy for remaining phases
        if _proxy_process.is_some() {
            config.inject_suite_config(suite);
            let mut proxy = ProxyProcess::start("127.0.0.1:8080");
            let _ = proxy.wait_healthy(Duration::from_secs(10));
            _proxy_process = Some(proxy);
        }
    } else {
        eprintln!("[SKIP] context_rooted: external-proxy mode (k8s)");
        report.skip_category("context_rooted");
    }

    // --- Phase 16: gRPC parity ---
    // Tests all gRPC service methods against the main proxy
    categories::grpc_parity::run(&mut report);
    let _s = snapshot_and_reset(&client, "grpc_parity", &mut report);

    // --- Phase 16b: SDK parity ---
    // Tests all gRPC service methods via the conproxy-sdk crate
    categories::sdk_parity::run(&mut report);
    let _s = snapshot_and_reset(&client, "sdk_parity", &mut report);

    // --- Phases 17-22: manage-own-proxy categories ---
    // SKIP in external-proxy mode (k8s): each of these starts its own proxy
    // on 127.0.0.1:8080 with specific configs; in k8s mode the proxy is in
    // a port-forward and can't be restarted.
    if !ext_proxy {
        // --- Phase 17: agent management (manages own proxy + mock) ---
        if let Some(ref mut proxy) = _proxy_process {
            proxy.stop();
        }
        categories::agent_mgmt::run(&mut report);

        // --- Phase 18: error handling (manages own proxy + mock) ---
        categories::error_handling::run(&mut report);

        // --- Phase 19: circuit breaker (manages own proxy + mock) ---
        categories::circuit_breaker::run(&mut report);

        // --- Phase 20: coalescing (manages own proxy + mock) ---
        categories::coalescing::run(&mut report);

        // --- Phase 21: federated search (manages own proxy + mock) ---
        categories::federated_search::run(&mut report);

        // --- Phase 22: security (manages own proxy + mock) ---
        categories::security::run(&mut report);

        // Restart normal proxy for performance measurement
        if _proxy_process.is_some() {
            config.inject_suite_config(suite);
            let mut proxy = ProxyProcess::start("127.0.0.1:8080");
            let _ = proxy.wait_healthy(Duration::from_secs(10));
            _proxy_process = Some(proxy);
        }
    } else {
        for cat in &[
            "agent_mgmt",
            "error_handling",
            "circuit_breaker",
            "coalescing",
            "federated_search",
            "security",
        ] {
            eprintln!("[SKIP] {cat}: external-proxy mode (k8s)");
            report.skip_category(cat);
        }
    }

    // --- Performance measurement (matches bash lines 1423-1435) ---
    if client.is_up() {
        measure_cache_hit_latency(&client);
    }

    // Drop the proxy explicitly so dhat-heap.json is written before collection
    if let Some(mut proxy) = _proxy_process.take() {
        proxy.stop();
    }

    // Wait for full profiler to finish (watches the PID and exits when it dies)
    if let Some(ref mut child) = _full_profiler {
        eprintln!("\x1b[36m[PROFILE]\x1b[0m Waiting for full profiler to finish...");
        let _ = child.wait();
    }

    // --- Stop ProcMonitor and collect resource data ---
    #[cfg(target_os = "linux")]
    let resource_data: Option<serde_json::Value> = _proc_monitor_handle.map(|h| {
        eprintln!("\x1b[36m[PROFILE]\x1b[0m Stopping process monitor...");
        let monitor = h.stop();
        let data = monitor.to_json();
        eprintln!(
            "\x1b[36m[PROFILE]\x1b[0m Collected {} snapshots",
            data["snapshots"].as_array().map(|a| a.len()).unwrap_or(0)
        );
        data
    });
    #[cfg(not(target_os = "linux"))]
    let resource_data: Option<serde_json::Value> = None;

    // --- Summary ---
    report.print_summary();

    if let Ok(dir) = std::env::var("E2E_OUTPUT_DIR") {
        let output_dir = PathBuf::from(&dir);

        // Read full profiler data (CPU top functions, DHAT, etc.) if available
        let profile_data: Option<serde_json::Value> =
            std::fs::read_to_string(output_dir.join("profile_results.json"))
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok());

        report.write_json(&output_dir.join("results.json"));
        report.write_html(
            &output_dir.join("results.html"),
            resource_data.as_ref(),
            profile_data.as_ref(),
        );
        write_endpoint_snapshots(&client, &output_dir);

        // Write resource profile if profiling was enabled
        if let Some(ref data) = resource_data {
            let resource_path = output_dir.join("resource_profile.json");
            if let Ok(json_str) = serde_json::to_string_pretty(data) {
                let _ = std::fs::write(&resource_path, json_str);
                eprintln!("Resource profile written to {}", resource_path.display());
            }
        }
    }

    report.assert_all_passed();
}

/// Standalone smoke test — validates all proxy endpoints respond without Docker.
///
/// Run with: `cargo test --test e2e_proxy --features proxy -- --ignored e2e_smoke_test --nocapture`
#[test]
#[ignore = "SMOKE: requires running proxy (no Docker)"]
fn e2e_smoke_test() {
    let client = E2eClient::new(proxy_url());
    assert!(
        client.is_up(),
        "Proxy not responding at {}. Start it first.",
        proxy_url()
    );

    let mut report = TestReport::new("smoke");
    categories::smoke::run(&client, &mut report);
    report.print_summary();

    if let Ok(dir) = std::env::var("E2E_OUTPUT_DIR") {
        let output_dir = PathBuf::from(&dir);
        report.write_json(&output_dir.join("smoke_results.json"));
        report.write_html(&output_dir.join("smoke_results.html"), None, None);
    }

    report.assert_all_passed();
}

/// Measure cache hit latency over 10 requests (matches bash performance check).
fn measure_cache_hit_latency(client: &E2eClient) {
    eprintln!();
    eprintln!("\x1b[36mPerformance Check:\x1b[0m");

    // Warm a query
    let _ = client.query("latency test");

    // Measure 10 cache hits
    let iterations = 10;
    let start = std::time::Instant::now();
    for _ in 0..iterations {
        let _ = client.query("latency test");
    }
    let elapsed = start.elapsed();
    let avg_ms = elapsed.as_secs_f64() * 1000.0 / iterations as f64;
    eprintln!("  Cache hit avg latency: {avg_ms:.2}ms (over {iterations} requests)");
}

/// Spawn the full profiler (`test_runner proc-monitor`) targeting the proxy PID.
///
/// The test_runner binary is in the same target directory as the conproxy binary.
/// Returns `None` if the binary is not found or spawn fails (graceful degradation).
fn spawn_full_profiler(
    proxy_pid: u32,
    conproxy_bin: &std::path::Path,
    output_dir: Option<&std::path::Path>,
) -> Option<std::process::Child> {
    let output_dir = output_dir?;

    // Derive test_runner binary path from conproxy binary path (same target dir)
    let target_dir = conproxy_bin.parent()?;
    let test_runner = target_dir.join("test_runner");

    if !test_runner.exists() {
        eprintln!(
            "\x1b[33m[PROFILE]\x1b[0m test_runner not found at {} — skipping full profiler",
            test_runner.display()
        );
        return None;
    }

    let mut cmd = std::process::Command::new(&test_runner);
    cmd.args([
        "proc-monitor",
        "--pid",
        &proxy_pid.to_string(),
        "--output-dir",
        &output_dir.to_string_lossy(),
        "--perf",
        "--bpftrace",
        "--dhat",
    ]);
    cmd.stdout(std::process::Stdio::null());
    let log_file = std::fs::File::create(output_dir.join("profiler.log"));
    match log_file {
        Ok(file) => {
            cmd.stderr(std::process::Stdio::from(file));
        }
        Err(_) => {
            cmd.stderr(std::process::Stdio::null());
        }
    };

    match cmd.spawn() {
        Ok(child) => {
            eprintln!(
                "\x1b[36m[PROFILE]\x1b[0m Full profiler started (PID: {}, targeting proxy PID: {proxy_pid})",
                child.id()
            );
            Some(child)
        }
        Err(e) => {
            eprintln!(
                "\x1b[33m[PROFILE]\x1b[0m Failed to spawn full profiler: {e} — falling back to lightweight monitor only"
            );
            None
        }
    }
}

/// Write endpoint snapshots to the output directory (matches bash write_output).
fn write_endpoint_snapshots(client: &E2eClient, dir: &std::path::Path) {
    let _ = std::fs::create_dir_all(dir);

    let endpoints: &[(&str, &str)] = &[
        ("/stats", "stats.json"),
        ("/metrics", "metrics.json"),
        ("/pool", "pool.json"),
        ("/circuit", "circuit.json"),
        ("/health", "health.json"),
        ("/ready", "ready.json"),
        ("/clients", "clients.json"),
        ("/audit", "audit.json"),
    ];

    for (path, filename) in endpoints {
        let (status, body) = client.get_json(path);
        let json = if status == 200 {
            serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".to_string())
        } else {
            "{}".to_string()
        };
        let _ = std::fs::write(dir.join(filename), json);
    }

    // Prometheus is text, not JSON
    let (status, text) = client.prometheus();
    let prom = if status == 200 { text } else { String::new() };
    let _ = std::fs::write(dir.join("prometheus.txt"), prom);

    eprintln!("Endpoint snapshots written to {}", dir.display());
}
