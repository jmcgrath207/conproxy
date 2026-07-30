//! Adaptive timeout based on latency patterns.
//!
//! Adjusts request timeouts based on observed latency to avoid
//! premature timeouts during slow periods or wasteful waits during
//! fast periods.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::RwLock;

/// Configuration for adaptive timeout.
#[derive(Debug, Clone)]
pub struct AdaptiveTimeoutConfig {
    /// Minimum timeout (floor).
    pub min_timeout: Duration,
    /// Maximum timeout (ceiling).
    pub max_timeout: Duration,
    /// Multiplier for p99 latency to calculate timeout.
    pub latency_multiplier: f64,
    /// Number of samples to use for percentile calculation.
    pub sample_size: usize,
    /// Initial timeout before we have samples.
    pub initial_timeout: Duration,
}

impl Default for AdaptiveTimeoutConfig {
    fn default() -> Self {
        Self {
            min_timeout: Duration::from_millis(100),
            max_timeout: Duration::from_secs(30),
            latency_multiplier: 2.0, // 2x p99
            sample_size: 100,
            initial_timeout: Duration::from_secs(5),
        }
    }
}

/// Adaptive timeout calculator.
///
/// Tracks latency samples and calculates appropriate timeouts
/// based on recent performance.
pub struct AdaptiveTimeout {
    config: AdaptiveTimeoutConfig,
    /// Ring buffer of latency samples (in microseconds).
    samples: RwLock<Vec<u64>>,
    /// Write position in the ring buffer.
    write_pos: AtomicU64,
    /// Number of samples recorded.
    sample_count: AtomicU64,
    /// Cached p99 latency (in microseconds).
    cached_p99: AtomicU64,
    /// Current calculated timeout (in microseconds).
    current_timeout: AtomicU64,
}

impl AdaptiveTimeout {
    /// Create a new adaptive timeout calculator.
    pub fn new(config: AdaptiveTimeoutConfig) -> Self {
        let initial_timeout_us = config.initial_timeout.as_micros() as u64;
        Self {
            samples: RwLock::new(Vec::with_capacity(config.sample_size)),
            write_pos: AtomicU64::new(0),
            sample_count: AtomicU64::new(0),
            cached_p99: AtomicU64::new(0),
            current_timeout: AtomicU64::new(initial_timeout_us),
            config,
        }
    }

    /// Create with default configuration.
    pub fn default_config() -> Self {
        Self::new(AdaptiveTimeoutConfig::default())
    }

    /// Record a latency sample.
    pub fn record(&self, latency: Duration) {
        let latency_us = latency.as_micros() as u64;

        {
            let mut samples = self.samples.write();
            let pos = self.write_pos.fetch_add(1, Ordering::Relaxed) as usize;

            if samples.len() < self.config.sample_size {
                samples.push(latency_us);
            } else {
                let idx = pos.checked_rem(self.config.sample_size).unwrap_or(0);
                if let Some(slot) = samples.get_mut(idx) {
                    *slot = latency_us;
                }
            }
        }

        self.sample_count.fetch_add(1, Ordering::Relaxed);

        // Recalculate timeout periodically
        if self.sample_count.load(Ordering::Relaxed).is_multiple_of(10) {
            self.recalculate();
        }
    }

    /// Get the current recommended timeout.
    pub fn timeout(&self) -> Duration {
        Duration::from_micros(self.current_timeout.load(Ordering::Relaxed))
    }

    /// Get the current p99 latency.
    pub fn p99_latency(&self) -> Duration {
        Duration::from_micros(self.cached_p99.load(Ordering::Relaxed))
    }

    /// Get the number of samples recorded.
    pub fn sample_count(&self) -> u64 {
        self.sample_count.load(Ordering::Relaxed)
    }

    /// Force recalculation of the timeout.
    pub fn recalculate(&self) {
        let samples = self.samples.read();
        if samples.is_empty() {
            return;
        }

        let mut sorted: Vec<_> = samples.clone();
        sorted.sort_unstable();

        // Calculate p99
        let p99_idx = ((sorted.len() as f64) * 0.99).ceil() as usize;
        let p99_idx = p99_idx
            .saturating_sub(1)
            .min(sorted.len().saturating_sub(1));
        let p99_us = sorted.get(p99_idx).copied().unwrap_or(0);

        self.cached_p99.store(p99_us, Ordering::Relaxed);

        // Calculate timeout
        let p99_f64 = p99_us as f64;
        let timeout_us = (p99_f64 * self.config.latency_multiplier) as u64;
        let timeout_us = timeout_us.clamp(
            self.config.min_timeout.as_micros() as u64,
            self.config.max_timeout.as_micros() as u64,
        );

        self.current_timeout.store(timeout_us, Ordering::Relaxed);
    }

    /// Get statistics about the adaptive timeout.
    pub fn stats(&self) -> AdaptiveTimeoutStats {
        let samples = self.samples.read();
        if samples.is_empty() {
            return AdaptiveTimeoutStats {
                sample_count: 0,
                min_latency_ms: 0.0,
                max_latency_ms: 0.0,
                avg_latency_ms: 0.0,
                p50_latency_ms: 0.0,
                p95_latency_ms: 0.0,
                p99_latency_ms: 0.0,
                current_timeout_ms: self.current_timeout.load(Ordering::Relaxed) as f64 / 1000.0,
            };
        }

        let mut sorted: Vec<_> = samples.clone();
        sorted.sort_unstable();

        let len = sorted.len();
        let sum: u64 = sorted.iter().sum();

        let len_f64 = len as f64;
        let p50_idx = (len_f64 * 0.50).ceil() as usize;
        let p50_idx = p50_idx.saturating_sub(1);
        let p95_idx = (len_f64 * 0.95).ceil() as usize;
        let p95_idx = p95_idx.saturating_sub(1);
        let p99_idx = (len_f64 * 0.99).ceil() as usize;
        let p99_idx = p99_idx.saturating_sub(1);
        let last_idx = len.saturating_sub(1);

        let min_val = sorted.first().copied().unwrap_or(0);
        let max_val = sorted.get(last_idx).copied().unwrap_or(0);
        #[allow(clippy::arithmetic_side_effects)] // guarded by len > 0
        let avg_val = if len > 0 { sum / len as u64 } else { 0 };
        let p50_val = sorted.get(p50_idx.min(last_idx)).copied().unwrap_or(0);
        let p95_val = sorted.get(p95_idx.min(last_idx)).copied().unwrap_or(0);
        let p99_val = sorted.get(p99_idx.min(last_idx)).copied().unwrap_or(0);

        AdaptiveTimeoutStats {
            sample_count: self.sample_count.load(Ordering::Relaxed),
            min_latency_ms: min_val as f64 / 1000.0,
            max_latency_ms: max_val as f64 / 1000.0,
            avg_latency_ms: avg_val as f64 / 1000.0,
            p50_latency_ms: p50_val as f64 / 1000.0,
            p95_latency_ms: p95_val as f64 / 1000.0,
            p99_latency_ms: p99_val as f64 / 1000.0,
            current_timeout_ms: self.current_timeout.load(Ordering::Relaxed) as f64 / 1000.0,
        }
    }

    /// Reset all samples.
    pub fn reset(&self) {
        let mut samples = self.samples.write();
        samples.clear();
        self.write_pos.store(0, Ordering::Relaxed);
        self.sample_count.store(0, Ordering::Relaxed);
        self.cached_p99.store(0, Ordering::Relaxed);
        self.current_timeout.store(
            self.config.initial_timeout.as_micros() as u64,
            Ordering::Relaxed,
        );
    }
}

/// Statistics from adaptive timeout.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AdaptiveTimeoutStats {
    /// Number of samples recorded.
    pub sample_count: u64,
    /// Minimum observed latency.
    pub min_latency_ms: f64,
    /// Maximum observed latency.
    pub max_latency_ms: f64,
    /// Average latency.
    pub avg_latency_ms: f64,
    /// 50th percentile latency.
    pub p50_latency_ms: f64,
    /// 95th percentile latency.
    pub p95_latency_ms: f64,
    /// 99th percentile latency.
    pub p99_latency_ms: f64,
    /// Current calculated timeout.
    pub current_timeout_ms: f64,
}

/// Timeout budget for tracking remaining time in a request.
pub struct TimeoutBudget {
    /// Deadline when the request should timeout.
    deadline: std::time::Instant,
    /// Original timeout duration.
    original: Duration,
}

impl TimeoutBudget {
    /// Create a new timeout budget.
    pub fn new(timeout: Duration) -> Self {
        #[allow(clippy::arithmetic_side_effects)]
        let deadline = std::time::Instant::now() + timeout;
        Self {
            deadline,
            original: timeout,
        }
    }

    /// Create a timeout budget from an adaptive timeout.
    pub fn from_adaptive(adaptive: &AdaptiveTimeout) -> Self {
        Self::new(adaptive.timeout())
    }

    /// Get remaining time until timeout.
    pub fn remaining(&self) -> Duration {
        self.deadline
            .saturating_duration_since(std::time::Instant::now())
    }

    /// Check if the budget has expired.
    pub fn is_expired(&self) -> bool {
        std::time::Instant::now() >= self.deadline
    }

    /// Get the original timeout.
    pub fn original(&self) -> Duration {
        self.original
    }

    /// Get elapsed time since budget creation.
    pub fn elapsed(&self) -> Duration {
        let remaining = self.remaining();
        self.original.saturating_sub(remaining)
    }

    /// Get fraction of budget remaining (0.0 to 1.0).
    pub fn remaining_fraction(&self) -> f64 {
        if self.original.is_zero() {
            return 0.0;
        }
        self.remaining().as_secs_f64() / self.original.as_secs_f64()
    }
}

#[cfg(test)]
#[path = "tests/adaptive_tests.rs"]
mod tests;
