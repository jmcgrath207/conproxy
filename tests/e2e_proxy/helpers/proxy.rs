use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Manages a conproxy child process.
pub struct ProxyProcess {
    child: Option<Child>,
    listen: String,
    /// Background thread drains stderr into this buffer (non-blocking reads).
    stderr_buf: Arc<Mutex<String>>,
}

#[allow(dead_code)]
impl ProxyProcess {
    /// Start the proxy on the given listen address using the project's default config.
    pub fn start(listen: &str) -> Self {
        Self::start_inner(listen, &[], None)
    }

    /// Start the proxy with a specific config file.
    pub fn start_with_config(listen: &str, config_path: &str) -> Self {
        Self::start_inner(listen, &["--config", config_path], None)
    }

    /// Start the proxy and write full stderr logs to `log_path`.
    pub fn start_with_log(listen: &str, log_path: PathBuf) -> Self {
        Self::start_inner(listen, &[], Some(log_path))
    }

    fn start_inner(listen: &str, extra_args: &[&str], log_path: Option<PathBuf>) -> Self {
        let bin = conproxy_bin();
        let mut cmd = Command::new(&bin);
        cmd.args(["start", "--listen", listen]);
        cmd.args(extra_args);
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::piped());

        let mut child = cmd.spawn().expect("Failed to start proxy process");

        // Spawn a background thread to drain stderr without blocking.
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        if let Some(stderr) = child.stderr.take() {
            let buf = stderr_buf.clone();
            std::thread::spawn(move || {
                use std::io::{BufRead, BufReader, Write};
                let reader = BufReader::new(stderr);
                let mut log_file = log_path.and_then(|p| std::fs::File::create(p).ok());
                for line in reader.lines() {
                    match line {
                        Ok(l) => {
                            // Write to log file (unbounded)
                            if let Some(ref mut f) = log_file {
                                let _ = writeln!(f, "{}", l);
                            }
                            // Write to in-memory buffer (bounded, for error diagnostics)
                            let mut b = buf.lock().unwrap();
                            if b.len() < 8000 {
                                b.push_str(&l);
                                b.push('\n');
                            }
                        }
                        Err(_) => break,
                    }
                }
                // Flush log file on thread exit
                if let Some(ref mut f) = log_file {
                    let _ = f.flush();
                }
            });
        }

        Self {
            child: Some(child),
            listen: listen.to_string(),
            stderr_buf,
        }
    }

    /// Wait until `/health` returns 200, or timeout.
    ///
    /// Checks if the child process exited prematurely (crash) on each poll
    /// and includes stderr output in the error message for diagnostics.
    ///
    /// Note: The proxy serves HTTP REST on `listen_port + 1` (gRPC on the primary port).
    pub fn wait_healthy(&mut self, timeout: Duration) -> Result<(), String> {
        let http_addr = derive_http_addr(&self.listen);
        let url = format!("http://{}/health", http_addr);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| e.to_string())?;

        let start = Instant::now();
        while start.elapsed() < timeout {
            // Check if child exited (crashed)
            if let Some(ref mut child) = self.child {
                if let Ok(Some(status)) = child.try_wait() {
                    // Give the stderr thread a moment to flush
                    std::thread::sleep(Duration::from_millis(100));
                    let stderr = self.get_stderr();
                    return Err(format!(
                        "Proxy exited prematurely with {status}.\nStderr:\n{stderr}"
                    ));
                }
            }

            if let Ok(resp) = client.get(&url).send() {
                if resp.status().is_success() {
                    return Ok(());
                }
            }
            std::thread::sleep(Duration::from_millis(250));
        }

        // Timeout — grab whatever stderr the background thread has collected
        let stderr = self.get_stderr();
        let mut msg = format!(
            "Proxy at {} did not become healthy within {:.0}s",
            self.listen,
            timeout.as_secs_f64()
        );
        if !stderr.is_empty() {
            msg.push_str(&format!("\nProxy stderr:\n{stderr}"));
        }
        Err(msg)
    }

    /// Read stderr collected so far (non-blocking).
    pub fn get_stderr(&self) -> String {
        self.stderr_buf.lock().unwrap().clone()
    }

    /// Return the PID of the proxy child process (if running).
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(|c| c.id())
    }

    /// Stop the proxy process with SIGTERM and a grace period before SIGKILL.
    pub fn stop(&mut self) {
        if let Some(ref mut child) = self.child {
            #[cfg(unix)]
            unsafe {
                libc::kill(child.id() as i32, libc::SIGTERM);
            }

            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
        }
        self.child = None;
    }
}

impl Drop for ProxyProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Derive the HTTP REST address from the gRPC listen address (port + 1).
fn derive_http_addr(grpc_addr: &str) -> String {
    if let Some(colon) = grpc_addr.rfind(':') {
        let (host, port_str) = grpc_addr.split_at(colon);
        if let Ok(port) = port_str.trim_start_matches(':').parse::<u16>() {
            return format!("{}:{}", host, port + 1);
        }
    }
    grpc_addr.to_string()
}

/// Path to the conproxy binary (release build).
fn conproxy_bin() -> PathBuf {
    conproxy_bin_path()
}

/// Public accessor for the conproxy binary path.
pub fn conproxy_bin_path() -> PathBuf {
    // Check PROXY_BIN env var first (set by Makefile)
    if let Ok(bin) = std::env::var("PROXY_BIN") {
        return PathBuf::from(bin);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("release")
        .join("conproxy")
}
