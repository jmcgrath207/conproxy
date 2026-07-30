//! Shared ProcMonitor for test crates (e2e_proxy, e2e_eval).
//!
//! Samples process metrics from /proc on Linux. Non-Linux platforms
//! compile as no-ops.
//!
//! NOTE: A copy of this logic also lives in `src/bin/test_runner.rs` for the
//! standalone profiling binary. Keep them in sync when making changes.

// Each test binary includes this module via `path = "../test_infra/mod.rs"`
// (or mod.rs declaration) but most only use a subset of the symbols
// (e.g. `containers` for integration tests, `proc_monitor` for e2e_proxy).
// Allow dead code at module level so unused symbols don't fail under
// `RUSTFLAGS=-D warnings` in CI.
#![allow(dead_code)]

/// Single /proc sample for a running process.
#[cfg(target_os = "linux")]
pub struct ProcSnapshot {
    pub timestamp_ms: u64,
    pub rss_bytes: u64,
    pub vsize_bytes: u64,
    pub utime_ticks: u64,
    pub stime_ticks: u64,
    pub num_threads: u32,
    pub fd_count: u32,
    pub voluntary_ctxt_switches: u64,
    pub nonvoluntary_ctxt_switches: u64,
}

/// Aggregated summary of process resource usage.
#[cfg(target_os = "linux")]
pub struct ProcSummary {
    pub peak_rss_bytes: u64,
    pub avg_rss_bytes: u64,
    pub final_rss_bytes: u64,
    pub cpu_percent: f64,
    pub peak_threads: u32,
    pub peak_fds: u32,
    pub total_voluntary_ctxt_switches: u64,
    pub total_nonvoluntary_ctxt_switches: u64,
}

/// Monitors a process via /proc reads.
#[cfg(target_os = "linux")]
pub struct ProcMonitor {
    pid: u32,
    interval: std::time::Duration,
    snapshots: Vec<ProcSnapshot>,
    clock_ticks_per_sec: u64,
}

#[cfg(target_os = "linux")]
impl ProcMonitor {
    pub fn new(pid: u32) -> Self {
        let clock_ticks_per_sec = unsafe { libc::sysconf(libc::_SC_CLK_TCK) as u64 };
        Self {
            pid,
            interval: std::time::Duration::from_secs(1),
            snapshots: Vec::new(),
            clock_ticks_per_sec,
        }
    }

    /// Take a single sample from /proc. Returns false if the process is gone.
    pub fn sample(&mut self) -> bool {
        let stat_path = format!("/proc/{}/stat", self.pid);
        let stat_content = match std::fs::read_to_string(&stat_path) {
            Ok(c) => c,
            Err(_) => return false,
        };

        let after_comm = match stat_content.rfind(')') {
            Some(pos) => &stat_content[pos + 2..],
            None => return false,
        };
        let fields: Vec<&str> = after_comm.split_whitespace().collect();
        if fields.len() < 22 {
            return false;
        }

        let utime_ticks: u64 = fields[11].parse().unwrap_or(0);
        let stime_ticks: u64 = fields[12].parse().unwrap_or(0);
        let num_threads: u32 = fields[17].parse().unwrap_or(0);
        let vsize_bytes: u64 = fields[20].parse().unwrap_or(0);
        let rss_pages: u64 = fields[21].parse().unwrap_or(0);
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as u64 };
        let rss_bytes = rss_pages * page_size;

        let mut voluntary_ctxt_switches = 0u64;
        let mut nonvoluntary_ctxt_switches = 0u64;
        let status_path = format!("/proc/{}/status", self.pid);
        if let Ok(status) = std::fs::read_to_string(&status_path) {
            for line in status.lines() {
                if let Some(v) = line.strip_prefix("voluntary_ctxt_switches:") {
                    voluntary_ctxt_switches = v.trim().parse().unwrap_or(0);
                } else if let Some(v) = line.strip_prefix("nonvoluntary_ctxt_switches:") {
                    nonvoluntary_ctxt_switches = v.trim().parse().unwrap_or(0);
                }
            }
        }

        let fd_dir = format!("/proc/{}/fd", self.pid);
        let fd_count = std::fs::read_dir(&fd_dir)
            .map(|entries| entries.count() as u32)
            .unwrap_or(0);

        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        self.snapshots.push(ProcSnapshot {
            timestamp_ms,
            rss_bytes,
            vsize_bytes,
            utime_ticks,
            stime_ticks,
            num_threads,
            fd_count,
            voluntary_ctxt_switches,
            nonvoluntary_ctxt_switches,
        });

        true
    }

    /// Spawn a background sampling thread.
    pub fn spawn_background(pid: u32, interval: std::time::Duration) -> ProcMonitorHandle {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_clone = stop.clone();
        let handle = std::thread::spawn(move || {
            let mut monitor = ProcMonitor::new(pid);
            monitor.interval = interval;
            while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
                if !monitor.sample() {
                    break;
                }
                std::thread::sleep(interval);
            }
            monitor
        });
        ProcMonitorHandle { handle, stop }
    }

    /// Convert all snapshots + summary to JSON.
    pub fn to_json(&self) -> serde_json::Value {
        let snapshots: Vec<serde_json::Value> = self
            .snapshots
            .iter()
            .map(|s| {
                serde_json::json!({
                    "timestamp_ms": s.timestamp_ms,
                    "rss_bytes": s.rss_bytes,
                    "vsize_bytes": s.vsize_bytes,
                    "utime_ticks": s.utime_ticks,
                    "stime_ticks": s.stime_ticks,
                    "num_threads": s.num_threads,
                    "fd_count": s.fd_count,
                    "voluntary_ctxt_switches": s.voluntary_ctxt_switches,
                    "nonvoluntary_ctxt_switches": s.nonvoluntary_ctxt_switches,
                })
            })
            .collect();

        let summary = self.summary();
        serde_json::json!({
            "snapshots": snapshots,
            "summary": {
                "peak_rss_bytes": summary.peak_rss_bytes,
                "avg_rss_bytes": summary.avg_rss_bytes,
                "final_rss_bytes": summary.final_rss_bytes,
                "cpu_percent": summary.cpu_percent,
                "peak_threads": summary.peak_threads,
                "peak_fds": summary.peak_fds,
                "total_voluntary_ctxt_switches": summary.total_voluntary_ctxt_switches,
                "total_nonvoluntary_ctxt_switches": summary.total_nonvoluntary_ctxt_switches,
            }
        })
    }

    /// Compute aggregated summary from snapshots.
    pub fn summary(&self) -> ProcSummary {
        if self.snapshots.is_empty() {
            return ProcSummary {
                peak_rss_bytes: 0,
                avg_rss_bytes: 0,
                final_rss_bytes: 0,
                cpu_percent: 0.0,
                peak_threads: 0,
                peak_fds: 0,
                total_voluntary_ctxt_switches: 0,
                total_nonvoluntary_ctxt_switches: 0,
            };
        }

        let peak_rss = self
            .snapshots
            .iter()
            .map(|s| s.rss_bytes)
            .max()
            .unwrap_or(0);
        let avg_rss =
            self.snapshots.iter().map(|s| s.rss_bytes).sum::<u64>() / self.snapshots.len() as u64;
        let final_snap = self.snapshots.last().unwrap();
        let first_snap = self.snapshots.first().unwrap();

        let delta_ticks = (final_snap.utime_ticks + final_snap.stime_ticks)
            .saturating_sub(first_snap.utime_ticks + first_snap.stime_ticks);
        let delta_wall_ms = final_snap
            .timestamp_ms
            .saturating_sub(first_snap.timestamp_ms);
        let cpu_percent = if delta_wall_ms > 0 && self.clock_ticks_per_sec > 0 {
            (delta_ticks as f64 / (delta_wall_ms as f64 / 1000.0 * self.clock_ticks_per_sec as f64))
                * 100.0
        } else {
            0.0
        };

        ProcSummary {
            peak_rss_bytes: peak_rss,
            avg_rss_bytes: avg_rss,
            final_rss_bytes: final_snap.rss_bytes,
            cpu_percent,
            peak_threads: self
                .snapshots
                .iter()
                .map(|s| s.num_threads)
                .max()
                .unwrap_or(0),
            peak_fds: self.snapshots.iter().map(|s| s.fd_count).max().unwrap_or(0),
            total_voluntary_ctxt_switches: final_snap.voluntary_ctxt_switches,
            total_nonvoluntary_ctxt_switches: final_snap.nonvoluntary_ctxt_switches,
        }
    }
}

/// Handle to a background ProcMonitor thread.
#[cfg(target_os = "linux")]
pub struct ProcMonitorHandle {
    handle: std::thread::JoinHandle<ProcMonitor>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(target_os = "linux")]
impl ProcMonitorHandle {
    pub fn stop(self) -> ProcMonitor {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        self.handle.join().expect("ProcMonitor thread panicked")
    }
}
