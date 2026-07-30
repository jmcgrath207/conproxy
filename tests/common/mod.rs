#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

//! Test utilities for UAT tests
//!
//! Provides helper functions for running CLI commands and managing test environments.

#![allow(dead_code)]

use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

pub mod mock_upstream;

/// Read .conproxy/conproxy.toml as string
pub fn read_config(dir: &Path) -> String {
    fs::read_to_string(dir.join(".conproxy/conproxy.toml")).expect("Failed to read config")
}

/// Write `.conproxy/conproxy.toml` + subdirs + `.gitignore` directly.
///
/// Replaces the old `conproxy init` shim — `conproxy start` no longer
/// requires a prior init step, so tests can create the project layout
/// in-process instead of shelling out. Uses port `0` so tests that don't
/// start a real proxy never bind anything.
pub fn write_project_config(dir: &Path) {
    let conproxy_dir = dir.join(".conproxy");
    fs::create_dir_all(&conproxy_dir).expect("create .conproxy");
    fs::create_dir_all(conproxy_dir.join("cache")).expect("create cache");
    fs::create_dir_all(conproxy_dir.join("index")).expect("create index");
    fs::create_dir_all(conproxy_dir.join("packages")).expect("create packages");
    fs::create_dir_all(conproxy_dir.join("web")).expect("create web");

    // Minimal context-rooted config — one dummy upstream + default context.
    let toml = r#"[upstreams.dummy]
url = "http://127.0.0.1:1"
type = "elasticsearch"
index = "test"

[contexts.default]
default = true

[[contexts.default.upstreams]]
ref = "dummy"
priority = 0
"#;
    fs::write(conproxy_dir.join("conproxy.toml"), toml).expect("write toml");
    fs::write(conproxy_dir.join(".gitignore"), "cache/\n*.pid\n").expect("write gitignore");
}

/// Parse stdout as serde_json::Value
pub fn parse_json_output(output: &Output) -> serde_json::Value {
    let out = stdout(output);
    serde_json::from_str(&out).unwrap_or_else(|e| {
        panic!(
            "Failed to parse JSON output: {}\nstdout: {}\nstderr: {}",
            e,
            out,
            stderr(output)
        )
    })
}

/// Two ephemeral port numbers for gRPC and HTTP.
pub struct FreePorts {
    pub grpc_port: u16,
    pub http_port: u16,
}

/// Find two free ephemeral ports. Releases immediately — tiny TOCTOU window
/// in test environments (acceptable trade-off; the key benefit is writing
/// `http_listen` explicitly into config to match the discovered HTTP port).
pub fn find_free_ports() -> FreePorts {
    let grpc = TcpListener::bind("127.0.0.1:0").expect("bind grpc port 0");
    let grpc_port = grpc.local_addr().expect("local_addr grpc").port();
    drop(grpc);
    let http = TcpListener::bind("127.0.0.1:0").expect("bind http port 0");
    let http_port = http.local_addr().expect("local_addr http").port();
    drop(http);
    FreePorts {
        grpc_port,
        http_port,
    }
}

/// Bind to port 0 to find a free port, then release. Returns the ephemeral port.
/// Avoids hardcoded ports in tests → no port-conflict skip-on-failure silent passes.
///
/// Prefer `find_free_ports` which also reserves the derived HTTP port.
pub fn find_free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind to port 0 should succeed");
    let port = listener.local_addr().expect("local_addr").port();
    drop(listener);
    port
}

/// Start proxy in daemon mode on a given gRPC + HTTP port, poll for ready (5s timeout).
/// Returns true if the proxy started successfully.
///
/// Writes both `listen` and `http_listen` into the config so that all subsequent
/// CLI commands (`status`, `contexts`, etc.) connect to the correct ports.
pub fn start_proxy_daemon(dir: &Path, ports: &FreePorts) -> bool {
    let listen_addr = format!("127.0.0.1:{}", ports.grpc_port);
    let http_listen_addr = format!("127.0.0.1:{}", ports.http_port);

    // Write listen + http_listen into config so status/stop find the right ports
    let config_path = dir.join(".conproxy/conproxy.toml");
    if let Ok(config_content) = fs::read_to_string(&config_path) {
        let updated = if config_content.contains("[server]") {
            // Replace the [server] section header and inject both settings
            let section = format!(
                "[server]\nlisten = \"{listen_addr}\"\nhttp_listen = \"{http_listen_addr}\""
            );
            config_content.replace("[server]", &section)
        } else {
            format!(
                "[server]\nlisten = \"{listen_addr}\"\nhttp_listen = \"{http_listen_addr}\"\n\n{config_content}"
            )
        };
        let _ = fs::write(&config_path, updated);
    }

    let output = run_conproxy_in(dir, &["start", "--daemon", "--listen", &listen_addr]);
    if !output.status.success() {
        return false;
    }

    // Poll proxy status for up to 5 seconds
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(200));
        let status_output = run_conproxy_in(dir, &["status", "--json"]);
        if status_output.status.success() {
            let out = stdout(&status_output);
            if out.contains("\"running\": true") || out.contains("\"running\":true") {
                return true;
            }
        }
    }

    // Failure — attempt to read daemon log for diagnostics
    let daemon_log = dir.join(".conproxy/daemon.log");
    if let Ok(log) = fs::read_to_string(&daemon_log) {
        let mut tail: Vec<&str> = log.lines().rev().take(40).collect::<Vec<_>>();
        tail.reverse();
        eprintln!(
            "--- daemon.log (last 40 lines) ---\n{}\n---",
            tail.join("\n")
        );
    } else {
        eprintln!("--- daemon.log not found at {} ---", daemon_log.display());
    }
    false
}

/// Start proxy on ephemeral gRPC + HTTP ports. Returns the `FreePorts` guard.
/// Panics if the daemon fails to start — caller can rely on success.
pub fn start_proxy_daemon_ephemeral(dir: &Path) -> FreePorts {
    let ports = find_free_ports();
    let started = start_proxy_daemon(dir, &ports);
    assert!(
        started,
        "proxy daemon failed to start on ephemeral gRPC:{} HTTP:{}; tests must not silently skip",
        ports.grpc_port, ports.http_port,
    );
    ports
}

/// Stop proxy gracefully
pub fn stop_proxy(dir: &Path) {
    let _ = run_conproxy_in(dir, &["stop"]);
}

/// RAII guard — calls stop_proxy on drop (prevents leaked daemons on test panic)
pub struct ProxyGuard<'a> {
    pub dir: &'a Path,
}

impl<'a> ProxyGuard<'a> {
    pub fn new(dir: &'a Path) -> Self {
        ProxyGuard { dir }
    }
}

impl Drop for ProxyGuard<'_> {
    fn drop(&mut self) {
        stop_proxy(self.dir);
    }
}

/// Path to the conproxy binary built by cargo for the current test invocation.
///
/// Uses `CARGO_BIN_EXE_conproxy` (set by cargo at compile-time for integration tests)
/// which guarantees the binary matches the test's feature flags and profile.
pub fn conproxy_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_conproxy"))
}

/// Create a temporary directory for testing
pub fn temp_dir() -> TempDir {
    tempfile::tempdir().expect("Failed to create temp dir")
}

/// PID file env var — test-only override to isolate daemon PID files per fixture.
pub const CONPROXY_PID_ENV: &str = "CONPROXY_PID_FILE";

/// Test-scoped PID file path for the given test directory.
pub fn test_pid_file_path(dir: &Path) -> PathBuf {
    dir.join(".conproxy").join("test-proxy.pid")
}

/// Run conproxy CLI command in a specific directory.
///
/// Automatically sets `CONPROXY_PID_FILE=<dir>/.conproxy/test-proxy.pid`
/// so each temp dir has its own PID file — no cross-test contamination.
pub fn run_conproxy_in(dir: &Path, args: &[&str]) -> Output {
    let pid_file = test_pid_file_path(dir);
    let mut cmd = Command::new(conproxy_bin());
    cmd.current_dir(dir).args(args);
    cmd.env(CONPROXY_PID_ENV, pid_file);
    cmd.output()
        .unwrap_or_else(|e| panic!("Failed to run conproxy binary: {e}"))
}

/// Run conproxy CLI command with custom environment
pub fn run_conproxy_with_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(conproxy_bin());
    cmd.current_dir(dir).args(args);

    for (key, value) in env {
        cmd.env(key, value);
    }

    cmd.output().expect("Failed to run conproxy binary")
}

/// Assert that a command succeeded
pub fn assert_success(output: &Output) {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        panic!("Command failed!\nstdout: {}\nstderr: {}", stdout, stderr);
    }
}

/// Assert that a command failed
pub fn assert_failure(output: &Output) {
    assert!(
        !output.status.success(),
        "Expected command to fail but it succeeded"
    );
}

/// Get stdout as string
pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Get stderr as string
pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

/// Check if stdout contains a string
pub fn stdout_contains(output: &Output, needle: &str) -> bool {
    stdout(output).contains(needle)
}

/// Check if stderr contains a string
pub fn stderr_contains(output: &Output, needle: &str) -> bool {
    stderr(output).contains(needle)
}

/// Create a git repo at `parent/repo_name` with the given files, committed.
pub fn create_git_repo(parent: &Path, repo_name: &str, files: &[(&str, &str)]) -> PathBuf {
    let repo_path = parent.join(repo_name);
    fs::create_dir_all(&repo_path).unwrap();

    // Initialize git repo
    let output = Command::new("git")
        .current_dir(&repo_path)
        .args(["init"])
        .output()
        .expect("Failed to init git repo");
    assert!(output.status.success());

    // Configure git user for commits
    Command::new("git")
        .current_dir(&repo_path)
        .args(["config", "user.email", "test@test.com"])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(&repo_path)
        .args(["config", "user.name", "Test"])
        .output()
        .unwrap();

    // Write files
    for (name, content) in files {
        let path = repo_path.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    // Add and commit
    Command::new("git")
        .current_dir(&repo_path)
        .args(["add", "."])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(&repo_path)
        .args(["commit", "-m", "Initial commit"])
        .output()
        .unwrap();

    repo_path
}

/// Create a git repo with a tag.
pub fn create_git_repo_with_tag(
    parent: &Path,
    repo_name: &str,
    files: &[(&str, &str)],
    tag: &str,
) -> PathBuf {
    let repo_path = create_git_repo(parent, repo_name, files);

    Command::new("git")
        .current_dir(&repo_path)
        .args(["tag", tag])
        .output()
        .unwrap();

    repo_path
}

/// Run `conproxy init` + `conproxy install --git <repo_path>`, asserting both succeed.
pub fn init_and_install(work_dir: &Path, repo_path: &Path) -> Output {
    write_project_config(work_dir);
    let output = run_conproxy_in(work_dir, &["install", "--git", repo_path.to_str().unwrap()]);
    assert_success(&output);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_free_ports_returns_two_usable_ports() {
        let ports = find_free_ports();
        assert_ne!(
            ports.grpc_port, ports.http_port,
            "gRPC and HTTP ports must differ"
        );
        assert!(ports.grpc_port > 0, "gRPC port must be valid");
        assert!(ports.http_port > 0, "HTTP port must be valid");
        // Both ports should be immediately reusable (already released inside find_free_ports)
        let grpc = TcpListener::bind(format!("127.0.0.1:{}", ports.grpc_port));
        assert!(grpc.is_ok(), "gRPC port should be reusable immediately");
        let http = TcpListener::bind(format!("127.0.0.1:{}", ports.http_port));
        assert!(http.is_ok(), "HTTP port should be reusable immediately");
    }
}
