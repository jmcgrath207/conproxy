#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

//! UAT tests for proxy CLI subcommands and search
//!
//! Group A: No proxy needed — tests error paths when proxy is not running.
//! Group B: Proxy-dependent "not running" paths.
//! Group C: Proxy lifecycle (start/stop daemon) — uses `#[serial]`.

mod common;

use common::*;
use serial_test::serial;
use std::fs;

// ─── Group A: No proxy needed ───────────────────────────────────────────────

#[test]
#[serial]
fn test_proxy_status_not_running() {
    let dir = temp_dir();
    write_project_config(dir.path());
    let output = run_conproxy_in(dir.path(), &["status"]);
    // Should succeed even when proxy is not running (exits 0)
    let out = stdout(&output);
    assert!(
        out.contains("Proxy is not running"),
        "expected 'Proxy is not running' message, got: {}",
        out
    );
}

#[test]
#[serial]
fn test_proxy_status_json_not_running() {
    let dir = temp_dir();
    write_project_config(dir.path());
    let output = run_conproxy_in(dir.path(), &["status", "--json"]);
    assert_success(&output);

    let json = parse_json_output(&output);
    assert_eq!(json["running"], false);
}

#[test]
fn test_proxy_install_prints_instructions() {
    let dir = temp_dir();
    write_project_config(dir.path());
    let output = run_conproxy_in(dir.path(), &["install"]);
    assert_success(&output);

    let out = stdout(&output);
    // Linux → "systemd", macOS → "launchd", other → "not supported"
    assert!(
        out.contains("systemd") || out.contains("launchd") || out.contains("not supported"),
        "expected platform-specific install instructions, got: {}",
        out
    );
}

#[test]
fn test_proxy_uninstall_not_installed() {
    let dir = temp_dir();
    write_project_config(dir.path());
    let output = run_conproxy_in(dir.path(), &["uninstall"]);
    assert_success(&output);

    let out = stdout(&output);
    // Linux → "Service not installed." or uninstall instructions
    // macOS → "Service not installed."
    // Other → "not supported"
    assert!(
        out.contains("not installed") || out.contains("not supported") || out.contains("uninstall"),
        "expected service not found message, got: {}",
        out
    );
}

#[test]
fn test_search_no_proxy() {
    let dir = temp_dir();
    write_project_config(dir.path());
    let output = run_conproxy_in(dir.path(), &["search", "test query"]);
    // Search without proxy prints error to stderr
    let err = stderr(&output);
    assert!(
        err.contains("No proxy running") || err.contains("proxy"),
        "expected 'No proxy running' message, got stderr: {}",
        err
    );
}

// ─── Group B: Proxy-dependent "not running" paths ───────────────────────────

#[test]
fn test_proxy_contexts_not_running() {
    let dir = temp_dir();
    write_project_config(dir.path());
    let output = run_conproxy_in(dir.path(), &["contexts"]);
    let out = stdout(&output);
    let err = stderr(&output);
    assert!(
        out.contains("not running")
            || out.contains("Proxy is not running")
            || err.contains("transport")
            || err.contains("not running"),
        "expected 'not running' / 'transport' message, got: stdout={} stderr={}",
        out,
        err
    );
}

#[test]
fn test_proxy_peer_not_running() {
    let dir = temp_dir();
    write_project_config(dir.path());
    let output = run_conproxy_in(dir.path(), &["peer"]);
    let out = stdout(&output);
    assert!(
        out.contains("not running") || out.contains("Proxy is not running"),
        "expected 'not running' message, got: {}",
        out
    );
}

#[test]
fn test_proxy_cdc_not_running() {
    let dir = temp_dir();
    write_project_config(dir.path());
    let output = run_conproxy_in(dir.path(), &["cdc"]);
    let out = stdout(&output);
    assert!(
        out.contains("not running") || out.contains("Proxy is not running"),
        "expected 'not running' message, got: {}",
        out
    );
}

#[test]
fn test_seed_list_empty() {
    // `seed list` against a project config with no scope phrases should
    // report the empty state (no proxy needed).
    let dir = temp_dir();
    write_project_config(dir.path());
    let output = run_conproxy_in(dir.path(), &["seed", "list"]);
    assert_success(&output);
    let out = stdout(&output);
    assert!(
        out.contains("No scope phrases configured"),
        "expected 'No scope phrases configured' in seed list output, got: {}",
        out
    );
}

#[test]
fn test_seed_list_json_empty() {
    // `seed list --json` with no scope phrases should emit `[]`.
    let dir = temp_dir();
    write_project_config(dir.path());
    let output = run_conproxy_in(dir.path(), &["seed", "list", "--json"]);
    assert_success(&output);
    let out = stdout(&output).trim().to_string();
    assert_eq!(
        out, "[]",
        "expected '[]' from seed list --json, got: {}",
        out
    );
}

// ─── Group C: Proxy lifecycle (daemon start/stop) ───────────────────────────

#[test]
#[serial]
fn test_proxy_start_daemon_and_stop() {
    let dir = temp_dir();
    write_project_config(dir.path());
    let _guard = ProxyGuard::new(dir.path());

    let _ports = start_proxy_daemon_ephemeral(dir.path());

    // Verify running via text status
    let output = run_conproxy_in(dir.path(), &["status"]);
    assert_success(&output);
    assert!(
        stdout_contains(&output, "running"),
        "expected proxy to show as running"
    );

    // Stop
    let output = run_conproxy_in(dir.path(), &["stop"]);
    assert_success(&output);

    // Verify stopped
    // Give a moment for process to fully exit
    std::thread::sleep(std::time::Duration::from_millis(500));
    let output = run_conproxy_in(dir.path(), &["status"]);
    assert_success(&output);
    assert!(
        stdout_contains(&output, "not running"),
        "expected proxy to show as not running after stop"
    );
}

#[test]
#[serial]
fn test_proxy_start_daemon_json_status() {
    let dir = temp_dir();
    write_project_config(dir.path());
    let _guard = ProxyGuard::new(dir.path());

    let _ports = start_proxy_daemon_ephemeral(dir.path());

    let output = run_conproxy_in(dir.path(), &["status", "--json"]);
    assert_success(&output);

    let json = parse_json_output(&output);
    assert_eq!(json["running"], true, "expected running: true in JSON");
    assert!(
        json["pid"].is_number() || json["pid"].is_u64(),
        "expected pid in JSON"
    );
}

#[test]
#[serial]
fn test_proxy_start_already_running() {
    let dir = temp_dir();
    write_project_config(dir.path());
    let _guard = ProxyGuard::new(dir.path());

    let _ports = start_proxy_daemon_ephemeral(dir.path());

    // Try to start again
    let output = run_conproxy_in(
        dir.path(),
        &[
            "start",
            "--daemon",
            "--listen",
            &format!("127.0.0.1:{}", _ports.grpc_port),
        ],
    );
    assert_success(&output);
    let out = stdout(&output);
    assert!(
        out.contains("already running"),
        "expected 'already running' message, got: {}",
        out
    );
}

#[test]
#[serial]
#[ignore = "KNOWN BROKEN (2026-07-24): daemon reports started but subsequent CLI commands see 'Proxy is not running'. Likely config-injection or PID-file mismatch with the modern context-rooted config. Run `cargo test -- --ignored` to reproduce locally and diagnose before fixing."]
fn test_proxy_contexts_with_running_proxy() {
    let dir = temp_dir();
    write_project_config(dir.path());
    let _guard = ProxyGuard::new(dir.path());

    let _ports = start_proxy_daemon_ephemeral(dir.path());

    // Text output
    let output = run_conproxy_in(dir.path(), &["contexts"]);
    assert_success(&output);
    let out = stdout(&output);
    assert!(
        out.contains("context") || out.contains("Context") || out.contains("default"),
        "expected context list, got: {}",
        out
    );

    // JSON output
    let output = run_conproxy_in(dir.path(), &["contexts", "--json"]);
    assert_success(&output);
    let json = parse_json_output(&output);
    assert!(
        json["current"].is_string() || json["contexts"].is_array(),
        "expected current/contexts in JSON"
    );
}

#[test]
#[serial]
#[ignore = "KNOWN BROKEN (2026-07-24): daemon reports started but subsequent CLI commands see 'Proxy is not running'. Likely config-injection or PID-file mismatch with the modern context-rooted config. Run `cargo test -- --ignored` to reproduce locally and diagnose before fixing."]
fn test_proxy_peer_with_running_proxy() {
    let dir = temp_dir();
    write_project_config(dir.path());
    let _guard = ProxyGuard::new(dir.path());

    let _ports = start_proxy_daemon_ephemeral(dir.path());

    // Text output — peers disabled by default
    let output = run_conproxy_in(dir.path(), &["peer"]);
    assert_success(&output);
    let out = stdout(&output);
    assert!(
        out.contains("disabled") || out.contains("enabled"),
        "expected peer status, got: {}",
        out
    );

    // JSON output
    let output = run_conproxy_in(dir.path(), &["peer", "--json"]);
    assert_success(&output);
    // Should be valid JSON (even if it shows an error or status)
    let out = stdout(&output);
    assert!(
        out.starts_with('{') || out.starts_with('['),
        "expected JSON output, got: {}",
        out
    );
}

#[test]
#[serial]
#[ignore = "KNOWN BROKEN (2026-07-24): daemon reports started but subsequent CLI commands see 'Proxy is not running'. Likely config-injection or PID-file mismatch with the modern context-rooted config. Run `cargo test -- --ignored` to reproduce locally and diagnose before fixing."]
fn test_proxy_cdc_with_running_proxy() {
    let dir = temp_dir();
    write_project_config(dir.path());
    let _guard = ProxyGuard::new(dir.path());

    let _ports = start_proxy_daemon_ephemeral(dir.path());

    // Text output — CDC disabled by default
    let output = run_conproxy_in(dir.path(), &["cdc"]);
    assert_success(&output);
    let out = stdout(&output);
    assert!(
        out.contains("disabled") || out.contains("enabled"),
        "expected CDC status, got: {}",
        out
    );

    // JSON output
    let output = run_conproxy_in(dir.path(), &["cdc", "--json"]);
    assert_success(&output);
    let out = stdout(&output);
    assert!(out.starts_with('{'), "expected JSON object, got: {}", out);
}

#[test]
#[serial]
fn test_proxy_stop_removes_pid_file() {
    let dir = temp_dir();
    write_project_config(dir.path());
    let _guard = ProxyGuard::new(dir.path());
    let _ports = start_proxy_daemon_ephemeral(dir.path());

    let pid_file = test_pid_file_path(dir.path());
    assert!(
        pid_file.exists(),
        "PID file should exist while proxy is running"
    );

    let _ = run_conproxy_in(dir.path(), &["stop"]);
    // Give process a moment to exit and remove PID file
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(!pid_file.exists(), "PID file should be removed after stop");
}

#[test]
#[serial]
fn test_proxy_pid_file_isolation_concurrent() {
    let dir_a = temp_dir();
    let dir_b = temp_dir();
    write_project_config(dir_a.path());
    write_project_config(dir_b.path());
    let _guard_a = ProxyGuard::new(dir_a.path());
    let _guard_b = ProxyGuard::new(dir_b.path());

    let _ports_a = start_proxy_daemon_ephemeral(dir_a.path());
    let _ports_b = start_proxy_daemon_ephemeral(dir_b.path());

    // Each dir sees only its own daemon (different CONPROXY_PID_FILE)
    let out_a = run_conproxy_in(dir_a.path(), &["status", "--json"]);
    let json_a = parse_json_output(&out_a);
    assert_eq!(
        json_a["running"], true,
        "dirA should see its own daemon running"
    );

    let out_b = run_conproxy_in(dir_b.path(), &["status", "--json"]);
    let json_b = parse_json_output(&out_b);
    assert_eq!(
        json_b["running"], true,
        "dirB should see its own daemon running"
    );
}

// ─── Group D: CLI arg parsing (no proxy needed) ──────────────────────────

#[test]
fn test_cli_context_switch() {
    let dir = temp_dir();
    write_project_config(dir.path());
    let output = run_conproxy_in(dir.path(), &["context", "switch", "myctx"]);
    // Fails because no proxy running, but demonstrates arg parsing
    assert_failure(&output);
    let err = stderr(&output);
    assert!(
        err.contains("not running") || err.contains("proxy") || err.contains("connect"),
        "expected proxy error on context switch, got stderr: {}",
        err
    );
}

#[test]
fn test_cli_context_create() {
    let dir = temp_dir();
    write_project_config(dir.path());
    let output = run_conproxy_in(dir.path(), &["context", "create", "myctx"]);
    assert_failure(&output);
    let err = stderr(&output);
    assert!(
        err.contains("not running") || err.contains("proxy") || err.contains("connect"),
        "expected proxy error on context create, got stderr: {}",
        err
    );
}

#[test]
fn test_cli_logs_default() {
    let dir = temp_dir();
    write_project_config(dir.path());
    let output = run_conproxy_in(dir.path(), &["logs"]);
    assert_success(&output);
    let out = stdout(&output);
    assert!(
        out.contains("-- No entries --"),
        "expected '-- No entries --' in log output, got: {}",
        out
    );
}

#[test]
fn test_seed_list_with_phrases() {
    // `seed list` against a config that DOES define scope phrases should
    // render the count + the entries. Replaces the old `seed lookup` test
    // that called a removed subcommand.
    let dir = temp_dir();
    write_project_config(dir.path());
    let toml = r#"[upstreams.dummy]
url = "http://127.0.0.1:1"
type = "elasticsearch"
index = "test"

[contexts.default]
default = true
[[contexts.default.upstreams]]
ref = "dummy"
priority = 0

[contexts.default.scope]
seeds = ["alpha", "beta"]
"#;
    fs::write(dir.path().join(".conproxy/conproxy.toml"), toml).expect("write toml");
    let output = run_conproxy_in(dir.path(), &["seed", "list"]);
    assert_success(&output);
    let out = stdout(&output);
    assert!(
        out.contains("Scope phrases (2 configured)"),
        "expected 'Scope phrases (2 configured)' in seed list output, got: {}",
        out
    );
    assert!(
        out.contains("alpha") && out.contains("beta"),
        "expected both 'alpha' and 'beta' phrases listed, got: {}",
        out
    );
}
