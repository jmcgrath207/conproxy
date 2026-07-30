//! Proxy lifecycle management: start, stop, PID file, port checking.

use std::path::PathBuf;
use std::time::Duration;
use tracing::warn;

/// Default startup timeout in milliseconds.
pub const DEFAULT_STARTUP_TIMEOUT_MS: u64 = 5000;

/// Interval between startup checks in milliseconds.
pub const STARTUP_CHECK_INTERVAL_MS: u64 = 100;

/// Returns the PID file path for the proxy.
///
/// Test-only: if `CONPROXY_PID_FILE` env var is set, that path is used
/// directly, allowing per-test isolation without clobbering the global PID file.
pub fn pid_file_path() -> PathBuf {
    // Test override — keeps daemon PID files from leaking between test binaries.
    if let Ok(custom) = std::env::var("CONPROXY_PID_FILE") {
        return PathBuf::from(custom);
    }
    if cfg!(windows) {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("conproxy")
            .join("proxy.pid")
    } else if let Some(dir) = dirs::runtime_dir() {
        dir.join("conproxy.pid")
    } else {
        PathBuf::from("/tmp/conproxy.pid")
    }
}

/// Returns a per-project PID file path using a blake3 hash of the project root.
///
/// This allows multiple proxy instances for different projects to coexist.
/// Format: `conproxy-{hash8}.pid` where hash8 is the first 8 hex chars.
pub fn pid_file_path_for_project(project_root: &std::path::Path) -> PathBuf {
    let hash = blake3::hash(project_root.to_string_lossy().as_bytes());
    let hash8 = &hash.to_hex()[..8];
    let filename = format!("conproxy-{}.pid", hash8);

    if cfg!(windows) {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("conproxy")
            .join(filename)
    } else if let Some(dir) = dirs::runtime_dir() {
        dir.join(filename)
    } else {
        PathBuf::from("/tmp").join(filename)
    }
}

/// Write the proxy's PID to the PID file.
pub fn write_pid_file(pid: u32) {
    let path = pid_file_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&path, pid.to_string()) {
        warn!(path = %path.display(), error = %e, "Failed to write PID file");
        return;
    }

    // Set restrictive permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        if let Err(e) = std::fs::set_permissions(&path, perms) {
            warn!(error = %e, "Failed to set PID file permissions");
        }
    }
}

/// Remove the PID file.
pub fn remove_pid_file() {
    let path = pid_file_path();
    if let Err(e) = std::fs::remove_file(&path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            warn!(path = %path.display(), error = %e, "Failed to remove PID file");
        }
    }
}

/// Read the PID from the PID file.
pub fn read_pid() -> Option<u32> {
    let path = pid_file_path();
    if path.exists() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            return content.trim().parse().ok();
        }
    }
    None
}

/// Check if a process with the given PID is alive.
pub fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // Check via /proc on Linux — verify process identity to prevent
        // recycled-PID false positives.
        let proc_path = format!("/proc/{}", pid);
        if std::path::Path::new(&proc_path).exists() {
            let comm_path = format!("/proc/{}/comm", pid);
            if let Ok(comm) = std::fs::read_to_string(&comm_path) {
                return comm.trim().contains("conproxy");
            }
            // /proc exists but no comm readable — not our process
            return false;
        }
        // Fallback: use kill -0 via command (works on macOS)
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        // Use tasklist to check if PID exists
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
            .map(|o| {
                let out = String::from_utf8_lossy(&o.stdout);
                out.contains(&pid.to_string())
            })
            .unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

/// Check if the proxy is running by checking the PID file and process.
pub fn is_proxy_running() -> bool {
    if let Some(pid) = read_pid() {
        is_process_alive(pid)
    } else {
        false
    }
}

/// Check if a port is in use.
pub fn is_port_in_use(addr: &str) -> bool {
    use std::net::TcpListener;

    TcpListener::bind(addr).is_err()
}

/// Wait for the proxy to start listening on the given address.
///
/// # Errors
///
/// Returns [`ProxyError::StartupTimeout`] if the proxy is not listening on
/// `addr` within `timeout_ms` milliseconds.
pub async fn wait_for_proxy(addr: &str, timeout_ms: u64) -> Result<(), ProxyError> {
    let iterations = timeout_ms / STARTUP_CHECK_INTERVAL_MS;

    for _ in 0..iterations {
        if is_port_in_use(addr) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(STARTUP_CHECK_INTERVAL_MS)).await;
    }

    Err(ProxyError::StartupTimeout(format!(
        "Proxy not listening on {} after {}ms",
        addr, timeout_ms
    )))
}

/// Error types for proxy lifecycle operations.
#[derive(Debug)]
pub enum ProxyError {
    /// Startup timeout.
    StartupTimeout(String),
    /// HTTP client error.
    Http(String),
}

impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StartupTimeout(msg) => write!(f, "Startup timeout: {}", msg),
            Self::Http(msg) => write!(f, "HTTP error: {}", msg),
        }
    }
}

impl std::error::Error for ProxyError {}

/// Stop the proxy if it's running.
///
/// # Errors
///
/// Returns [`ProxyError::Http`] when the shutdown HTTP client fails to build.
/// A missing proxy (no PID file, or PID not alive) is not an error: the
/// function returns `Ok(())` after best-effort cleanup.
pub async fn stop_proxy(addr: &str) -> Result<(), ProxyError> {
    // First, try to send a graceful shutdown via HTTP
    let url = format!("http://{}/shutdown", addr);
    let client = super::socket_tuning::create_tuned_client_default(Duration::from_secs(5))
        .build()
        .map_err(|e| ProxyError::Http(e.to_string()))?;

    // Try to connect and send shutdown (may fail if not running)
    let _ = client.post(&url).send().await;

    // Check PID file and kill process if needed
    if let Some(pid) = read_pid() {
        if is_process_alive(pid) {
            #[cfg(unix)]
            {
                // Send SIGTERM
                let _ = std::process::Command::new("kill")
                    .args(["-TERM", &pid.to_string()])
                    .status();

                // Wait a bit
                tokio::time::sleep(Duration::from_millis(500)).await;

                // Check if still alive, send SIGKILL if so
                if is_process_alive(pid) {
                    let _ = std::process::Command::new("kill")
                        .args(["-KILL", &pid.to_string()])
                        .status();
                }
            }
            #[cfg(windows)]
            {
                let _ = std::process::Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/F"])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
            #[cfg(not(any(unix, windows)))]
            {
                let _ = pid;
            }
        }
        remove_pid_file();
    }

    Ok(())
}

/// Get the status of the proxy.
///
/// # Errors
///
/// Returns [`ProxyError::Http`] when the health check HTTP client fails to
/// build. Network-level errors against the proxy itself are swallowed and
/// reported as `running: false` in the returned [`ProxyStatus`].
pub async fn proxy_status(addr: &str) -> Result<ProxyStatus, ProxyError> {
    let pid = read_pid();
    let process_alive = pid.map(is_process_alive).unwrap_or(false);

    // Try to get health status
    let url = format!("http://{}/health", addr);
    let client = super::socket_tuning::create_tuned_client_default(Duration::from_secs(2))
        .build()
        .map_err(|e| ProxyError::Http(e.to_string()))?;

    let health: Option<serde_json::Value> = match client.get(&url).send().await {
        Ok(response) if response.status().is_success() => {
            tokio::time::timeout(Duration::from_secs(2), response.json())
                .await
                .ok()
                .and_then(|r| r.ok())
        }
        _ => None,
    };

    Ok(ProxyStatus {
        running: health.is_some() || process_alive,
        pid,
        health,
    })
}

/// Status information for the proxy.
#[derive(Debug)]
pub struct ProxyStatus {
    /// Whether the proxy is running.
    pub running: bool,
    /// The PID if available.
    pub pid: Option<u32>,
    /// Health check response if available.
    pub health: Option<serde_json::Value>,
}

#[cfg(test)]
#[path = "tests/lifecycle_tests.rs"]
mod tests;
