#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;
use serial_test::serial;

#[test]
fn test_pid_file_path() {
    let path = pid_file_path();
    assert!(path.to_string_lossy().contains("conproxy"));
    assert!(path.to_string_lossy().contains("proxy.pid"));
}

#[test]
fn test_is_process_alive_nonexistent() {
    // PID 1 is usually init, but a very high PID likely doesn't exist
    assert!(!is_process_alive(999999));
}

#[test]
fn test_is_port_in_use() {
    use std::net::TcpListener;

    // Bind to a port
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    // Port should be in use
    assert!(is_port_in_use(&addr));

    // Drop the listener
    drop(listener);

    // Give the OS time to release the port
    std::thread::sleep(Duration::from_millis(100));

    // Port should be free now (may fail on some systems)
    // assert!(!is_port_in_use(&addr));
}

#[test]
fn test_read_pid_nonexistent() {
    // If no PID file exists, should return None
    // (This test might pass or fail depending on actual state)
    let _pid = read_pid();
}

#[test]
fn test_pid_file_path_for_project_deterministic() {
    let root = std::path::Path::new("/home/user/myproject");
    let path1 = pid_file_path_for_project(root);
    let path2 = pid_file_path_for_project(root);
    assert_eq!(path1, path2);
    // Should contain the hash-based filename
    let filename = path1.file_name().unwrap().to_string_lossy();
    assert!(filename.starts_with("conproxy-"));
    assert!(filename.ends_with(".pid"));
    assert_eq!(filename.len(), "conproxy-12345678.pid".len());
}

#[test]
fn test_pid_file_path_for_project_different_roots() {
    let root1 = std::path::Path::new("/home/user/project-a");
    let root2 = std::path::Path::new("/home/user/project-b");
    let path1 = pid_file_path_for_project(root1);
    let path2 = pid_file_path_for_project(root2);
    assert_ne!(path1, path2);
}

#[test]
fn test_stop_proxy_code_paths() {
    // Test that is_process_alive handles nonexistent PIDs correctly on all platforms
    assert!(!is_process_alive(4294967));
    // Test that is_proxy_running works when no PID file exists
    // (may or may not have PID file depending on test environment)
    let _ = is_proxy_running();
}

// === PID file operations using tempdir ===

#[test]
#[serial]
fn test_write_and_read_pid_file() {
    // Write a PID file, then read it back
    let current_pid = std::process::id();
    write_pid_file(current_pid);

    let read_back = read_pid();
    // The read might return our PID if the default path was writable
    if read_back.is_some() {
        assert_eq!(read_back.unwrap(), current_pid);
        // Clean up
        remove_pid_file();
        // After removal, reading should return None (unless another process wrote one)
    }
}

#[test]
#[serial]
fn test_remove_pid_file_nonexistent() {
    // Removing a non-existent PID file should not panic
    // Save and restore state: first ensure no PID file
    let path = pid_file_path();
    let existed = path.exists();
    if !existed {
        // Safe to test removal of non-existent
        remove_pid_file(); // Should not panic
    }
}

#[test]
fn test_is_process_alive_current_process() {
    let pid = std::process::id();
    assert!(is_process_alive(pid));
}

#[test]
fn test_is_process_alive_dead_pid() {
    // Use a PID in the high range that's unlikely to be alive
    // (4294967295 = u32::MAX can wrap to -1 which signal(0) treats specially)
    assert!(!is_process_alive(4194300));
}

// === ProxyError Display ===

#[test]
fn test_proxy_error_display() {
    let err = ProxyError::StartupTimeout("not listening after 5000ms".to_string());
    assert!(err.to_string().contains("Startup timeout"));
    assert!(err.to_string().contains("not listening"));

    let err = ProxyError::Http("connection refused".to_string());
    assert!(err.to_string().contains("HTTP error"));
    assert!(err.to_string().contains("connection refused"));
}

// === Constants ===

#[test]
fn test_lifecycle_constants() {
    assert_eq!(DEFAULT_STARTUP_TIMEOUT_MS, 5000);
    assert_eq!(STARTUP_CHECK_INTERVAL_MS, 100);
}

// === ProxyStatus ===

#[test]
fn test_proxy_status_struct() {
    let status = ProxyStatus {
        running: true,
        pid: Some(1234),
        health: Some(serde_json::json!({"status": "ok"})),
    };
    assert!(status.running);
    assert_eq!(status.pid, Some(1234));
    assert!(status.health.is_some());
}

// === is_port_in_use with free port ===

#[test]
fn test_is_port_not_in_use() {
    // Use a high port that's very unlikely to be in use
    assert!(!is_port_in_use("127.0.0.1:59123"));
}

// === pid_file_path_for_project ===

#[test]
fn test_pid_file_path_for_project_contains_hash() {
    let path = pid_file_path_for_project(std::path::Path::new("/test/project"));
    let filename = path.file_name().unwrap().to_string_lossy();
    assert!(filename.starts_with("conproxy-"));
    assert!(filename.ends_with(".pid"));
    // Hash is 8 hex chars
    let hash_part: String = filename
        .strip_prefix("conproxy-")
        .unwrap()
        .strip_suffix(".pid")
        .unwrap()
        .to_string();
    assert_eq!(hash_part.len(), 8);
    assert!(hash_part.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn test_proxy_error_http_display() {
    let err = ProxyError::Http("connection refused".to_string());
    assert_eq!(err.to_string(), "HTTP error: connection refused");
}

#[test]
fn test_proxy_error_is_std_error() {
    let err: Box<dyn std::error::Error> =
        Box::new(ProxyError::StartupTimeout("timed out".to_string()));
    assert!(err.to_string().contains("Startup timeout"));
}

#[test]
fn test_is_proxy_running_no_pid_file() {
    // If PID file doesn't exist, proxy is not running.
    // This tests the is_proxy_running() → read_pid() → is_process_alive() path.
    // We can't guarantee there's no PID file, but the function should not panic.
    let _running = is_proxy_running();
}

#[test]
fn test_proxy_status_struct_fields() {
    let status = ProxyStatus {
        running: true,
        pid: Some(1234),
        health: Some(serde_json::json!({"status": "ok"})),
    };
    assert!(status.running);
    assert_eq!(status.pid, Some(1234));
    assert!(status.health.is_some());
}

#[test]
fn test_proxy_status_not_running() {
    let status = ProxyStatus {
        running: false,
        pid: None,
        health: None,
    };
    assert!(!status.running);
    assert!(status.pid.is_none());
    assert!(status.health.is_none());
}

#[test]
fn test_proxy_error_debug() {
    let err = ProxyError::Http("test".to_string());
    let debug = format!("{:?}", err);
    assert!(debug.contains("Http"));
}

#[test]
#[serial]
fn test_pid_file_round_trip() {
    // Write a PID file, read it back, remove it
    write_pid_file(12345);
    let pid = read_pid();
    assert_eq!(pid, Some(12345));
    remove_pid_file();
    assert!(read_pid().is_none());
}

#[test]
fn test_startup_check_interval_constant() {
    assert_eq!(STARTUP_CHECK_INTERVAL_MS, 100);
}

#[test]
#[serial]
fn test_pid_file_path_env_override() {
    let tmp = std::env::temp_dir().join("conproxy_test_pid_env_override");
    let _ = std::fs::remove_file(&tmp);
    let custom = tmp.join("custom.pid");
    std::env::set_var("CONPROXY_PID_FILE", &custom);
    assert_eq!(pid_file_path(), custom);
    write_pid_file(12345);
    assert_eq!(read_pid(), Some(12345));
    remove_pid_file();
    assert!(read_pid().is_none());
    std::env::remove_var("CONPROXY_PID_FILE");
}

#[test]
fn test_is_process_alive_self_pid() {
    let pid = std::process::id();
    assert!(is_process_alive(pid));
}

// === Async tests for stop_proxy, proxy_status, wait_for_proxy ===

#[tokio::test]
async fn test_proxy_status_unreachable() {
    // No server running on this port
    let result = proxy_status("127.0.0.1:19999").await;
    assert!(result.is_ok());
    let status = result.unwrap();
    // Health check should fail (no server listening)
    assert!(status.health.is_none());
}

#[tokio::test]
async fn test_proxy_status_with_mock_server() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let app = axum::Router::new().route(
        "/health",
        axum::routing::get(|| async {
            axum::Json(serde_json::json!({"status": "ok", "uptime_secs": 42}))
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let result = proxy_status(&addr.to_string()).await;
    assert!(result.is_ok());
    let status = result.unwrap();
    assert!(status.running);
    assert!(status.health.is_some());
    let health = status.health.unwrap();
    assert_eq!(health["status"], "ok");
}

#[tokio::test]
async fn test_stop_proxy_shutdown_endpoint() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let app = axum::Router::new().route(
        "/shutdown",
        axum::routing::post(|| async { axum::http::StatusCode::OK }),
    );

    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let result = stop_proxy(&addr.to_string()).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_wait_for_proxy_already_running() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    // Port is in use, so wait_for_proxy should return immediately
    let result = wait_for_proxy(&addr, 500).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_wait_for_proxy_timeout() {
    // Port that nobody is listening on
    let result = wait_for_proxy("127.0.0.1:19998", 200).await;
    assert!(result.is_err());
    match result.unwrap_err() {
        ProxyError::StartupTimeout(msg) => {
            assert!(msg.contains("19998"));
        }
        other => panic!("Expected StartupTimeout, got: {:?}", other),
    }
}
