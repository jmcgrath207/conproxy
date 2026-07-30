//! Unified resilience state machine for upstream health management.
//!
//! Combines the responsibilities of circuit breaker, health tracking,
//! recovery worker, retry policy, and adaptive timeout into a single
//! coherent state machine with three states:
//!
//! - **Healthy**: Normal operation. All requests allowed.
//! - **Degraded**: Partial failures detected. Rate-limited traffic with retries.
//! - **Offline**: Upstream unreachable. No traffic; awaiting probe success.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use serde::Serialize;

/// Unified upstream state combining circuit breaker, health tracking, and recovery.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamState {
    /// Normal operation. Adaptive timeout based on P99.
    #[default]
    Healthy,
    /// Failures detected. Retry with backoff. Rate-limited traffic.
    Degraded,
    /// Consecutive failures exceeded threshold. No traffic. Awaiting probe.
    Offline,
}

/// Configuration for the unified resilience state machine.
#[derive(Debug, Clone)]
pub struct ResilienceConfig {
    /// Failures within `failure_window` to transition Healthy → Degraded.
    pub degraded_after_failures: u32,
    /// Consecutive failures in Degraded to transition → Offline.
    pub offline_after_consecutive: u32,
    /// Consecutive successes to transition Degraded → Healthy.
    pub healthy_after_successes: u32,
    /// Rolling window for counting failures.
    pub failure_window: Duration,
    /// Duration in Offline before attempting probe.
    pub probe_interval: Duration,
    /// Initial RPS limit in Degraded state.
    pub degraded_rps_limit: f32,
    /// Max RPS as recovery progresses.
    pub max_rps: f32,
    /// RPS increment per successful batch in Degraded.
    pub rps_increment: f32,
    /// Max retry attempts in Degraded state.
    pub max_retries: u32,
    /// Base delay for retry backoff.
    pub retry_base_delay: Duration,
    /// Max delay for retry backoff.
    pub retry_max_delay: Duration,
}

impl Default for ResilienceConfig {
    fn default() -> Self {
        Self {
            degraded_after_failures: 5,
            offline_after_consecutive: 10,
            healthy_after_successes: 3,
            failure_window: Duration::from_secs(60),
            probe_interval: Duration::from_secs(30),
            degraded_rps_limit: 10.0,
            max_rps: 1000.0,
            rps_increment: 5.0,
            max_retries: 3,
            retry_base_delay: Duration::from_millis(100),
            retry_max_delay: Duration::from_secs(10),
        }
    }
}

/// Unified state manager for an upstream.
pub struct UpstreamStateManager {
    config: ResilienceConfig,
    state: RwLock<UpstreamState>,
    failure_count: AtomicU32,
    consecutive_failures: AtomicU32,
    consecutive_successes: AtomicU32,
    window_start: RwLock<Instant>,
    current_rps: AtomicU32, // fixed-point * 100
    transitions: AtomicU64,
    total_failures: AtomicU64,
    total_successes: AtomicU64,
    last_transition: RwLock<Instant>,
}

impl UpstreamStateManager {
    /// Create a new state manager with the given configuration.
    pub fn new(config: ResilienceConfig) -> Self {
        let initial_rps = (config.degraded_rps_limit * 100.0) as u32;
        Self {
            config,
            state: RwLock::new(UpstreamState::Healthy),
            failure_count: AtomicU32::new(0),
            consecutive_failures: AtomicU32::new(0),
            consecutive_successes: AtomicU32::new(0),
            window_start: RwLock::new(Instant::now()),
            current_rps: AtomicU32::new(initial_rps),
            transitions: AtomicU64::new(0),
            total_failures: AtomicU64::new(0),
            total_successes: AtomicU64::new(0),
            last_transition: RwLock::new(Instant::now()),
        }
    }

    /// Create with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(ResilienceConfig::default())
    }

    /// Get current state.
    pub fn state(&self) -> UpstreamState {
        *self.state.read()
    }

    /// Record a successful request/probe. May transition state.
    pub fn record_success(&self) {
        self.total_successes.fetch_add(1, Ordering::Relaxed);
        self.consecutive_failures.store(0, Ordering::Relaxed);
        let prev = self.consecutive_successes.fetch_add(1, Ordering::Relaxed);

        let state = *self.state.read();
        match state {
            UpstreamState::Healthy => {
                self.failure_count.store(0, Ordering::Relaxed);
            }
            UpstreamState::Degraded => {
                // Increase RPS limit
                let inc = (self.config.rps_increment * 100.0) as u32;
                let max = (self.config.max_rps * 100.0) as u32;
                let _ =
                    self.current_rps
                        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                            Some(cur.saturating_add(inc).min(max))
                        });
                if prev.saturating_add(1) >= self.config.healthy_after_successes {
                    self.transition_to(UpstreamState::Healthy);
                }
            }
            UpstreamState::Offline => {
                // Probe success → Degraded (must go through Degraded first)
                self.transition_to(UpstreamState::Degraded);
            }
        }
    }

    /// Record a failed request/probe. May transition state.
    pub fn record_failure(&self) {
        self.total_failures.fetch_add(1, Ordering::Relaxed);
        self.consecutive_successes.store(0, Ordering::Relaxed);
        self.consecutive_failures.fetch_add(1, Ordering::Relaxed);

        let state = *self.state.read();
        match state {
            UpstreamState::Healthy => {
                self.maybe_reset_window();
                let prev = self.failure_count.fetch_add(1, Ordering::Relaxed);
                let failures = prev.saturating_add(1);
                if failures >= self.config.degraded_after_failures {
                    self.transition_to(UpstreamState::Degraded);
                }
            }
            UpstreamState::Degraded => {
                let consecutive = self.consecutive_failures.load(Ordering::Relaxed);
                if consecutive >= self.config.offline_after_consecutive {
                    self.transition_to(UpstreamState::Offline);
                }
            }
            UpstreamState::Offline => {}
        }
    }

    /// Whether requests should be allowed through.
    pub fn allow_request(&self) -> bool {
        match *self.state.read() {
            UpstreamState::Healthy | UpstreamState::Degraded => true,
            UpstreamState::Offline => false,
        }
    }

    /// Get current RPS limit (f32::MAX in Healthy, 0 in Offline).
    pub fn current_rps_limit(&self) -> f32 {
        match *self.state.read() {
            UpstreamState::Healthy => f32::MAX,
            UpstreamState::Degraded => self.current_rps.load(Ordering::Relaxed) as f32 / 100.0,
            UpstreamState::Offline => 0.0,
        }
    }

    /// Get retry delay for nth attempt in Degraded state (exponential backoff).
    /// Returns None in Healthy/Offline or if attempt >= max_retries.
    pub fn retry_delay(&self, attempt: u32) -> Option<Duration> {
        if *self.state.read() != UpstreamState::Degraded {
            return None;
        }
        if attempt >= self.config.max_retries {
            return None;
        }
        let base_ms = self.config.retry_base_delay.as_millis() as u64;
        let multiplier = 2u64.saturating_pow(attempt);
        let delay_ms = base_ms.saturating_mul(multiplier);
        let max_ms = self.config.retry_max_delay.as_millis() as u64;
        Some(Duration::from_millis(delay_ms.min(max_ms)))
    }

    /// Force transition to Offline.
    pub fn force_offline(&self) {
        self.transition_to(UpstreamState::Offline);
    }

    /// Force reset to Healthy.
    pub fn force_healthy(&self) {
        self.transition_to(UpstreamState::Healthy);
    }

    /// Get snapshot of resilience metrics.
    pub fn snapshot(&self) -> ResilienceSnapshot {
        let state = *self.state.read();
        let last = *self.last_transition.read();
        ResilienceSnapshot {
            state,
            transitions: self.transitions.load(Ordering::Relaxed),
            total_failures: self.total_failures.load(Ordering::Relaxed),
            total_successes: self.total_successes.load(Ordering::Relaxed),
            current_rps_limit: self.current_rps_limit(),
            time_in_current_state_ms: last.elapsed().as_millis() as u64,
        }
    }

    fn transition_to(&self, new_state: UpstreamState) {
        let mut state = self.state.write();
        if *state == new_state {
            return;
        }
        *state = new_state;
        self.transitions.fetch_add(1, Ordering::Relaxed);
        *self.last_transition.write() = Instant::now();

        match new_state {
            UpstreamState::Healthy => {
                self.failure_count.store(0, Ordering::Relaxed);
                self.consecutive_failures.store(0, Ordering::Relaxed);
                self.consecutive_successes.store(0, Ordering::Relaxed);
                *self.window_start.write() = Instant::now();
                let initial = (self.config.degraded_rps_limit * 100.0) as u32;
                self.current_rps.store(initial, Ordering::Relaxed);
            }
            UpstreamState::Degraded => {
                self.consecutive_successes.store(0, Ordering::Relaxed);
                self.failure_count.store(0, Ordering::Relaxed);
                let initial = (self.config.degraded_rps_limit * 100.0) as u32;
                self.current_rps.store(initial, Ordering::Relaxed);
            }
            UpstreamState::Offline => {
                self.consecutive_successes.store(0, Ordering::Relaxed);
            }
        }
    }

    fn maybe_reset_window(&self) {
        let ws = *self.window_start.read();
        if ws.elapsed() >= self.config.failure_window {
            *self.window_start.write() = Instant::now();
            self.failure_count.store(0, Ordering::Relaxed);
        }
    }
}

/// Metrics snapshot for observability.
#[derive(Debug, Clone, Serialize)]
pub struct ResilienceSnapshot {
    pub state: UpstreamState,
    pub transitions: u64,
    pub total_failures: u64,
    pub total_successes: u64,
    pub current_rps_limit: f32,
    pub time_in_current_state_ms: u64,
}

#[cfg(test)]
#[path = "tests/resilience_tests.rs"]
mod tests;
