//! Circuit breaker for protecting against cascading failures.
//!
//! Implements the circuit breaker pattern to prevent repeated calls to
//! failing upstreams. States:
//! - Closed: Normal operation, requests pass through
//! - Open: Failures exceeded threshold, requests fail fast
//! - HalfOpen: Testing if upstream has recovered

use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::time::{Duration, Instant};

use parking_lot::RwLock;

/// Circuit breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation - requests pass through.
    Closed,
    /// Circuit is open - requests fail fast.
    Open,
    /// Testing if upstream has recovered.
    HalfOpen,
}

impl CircuitState {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Closed,
            1 => Self::Open,
            2 => Self::HalfOpen,
            _ => Self::Closed,
        }
    }

    pub fn to_u8(self) -> u8 {
        match self {
            Self::Closed => 0,
            Self::Open => 1,
            Self::HalfOpen => 2,
        }
    }
}

/// Configuration for the circuit breaker.
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of failures before opening the circuit.
    pub failure_threshold: u32,
    /// Number of successes in half-open state to close the circuit.
    pub success_threshold: u32,
    /// Duration to wait before transitioning from open to half-open.
    pub open_duration: Duration,
    /// Duration for the failure window (failures outside this window don't count).
    pub failure_window: Duration,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            success_threshold: 2,
            open_duration: Duration::from_secs(30),
            failure_window: Duration::from_secs(60),
        }
    }
}

/// Circuit breaker for protecting upstream calls.
pub struct CircuitBreaker {
    /// Configuration (behind RwLock for hot-reload).
    config: RwLock<CircuitBreakerConfig>,
    /// Current state.
    state: AtomicU8,
    /// Failure count in current window.
    failure_count: AtomicU32,
    /// Success count in half-open state.
    half_open_successes: AtomicU32,
    /// When the circuit was opened.
    opened_at: RwLock<Option<Instant>>,
    /// When the failure window started.
    window_start: RwLock<Instant>,
    /// Total times circuit has opened (for metrics).
    times_opened: AtomicU64,
    /// Total times circuit has tripped (failed when open).
    times_tripped: AtomicU64,
}

impl CircuitBreaker {
    /// Create a new circuit breaker with the given configuration.
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            config: RwLock::new(config),
            state: AtomicU8::new(CircuitState::Closed.to_u8()),
            failure_count: AtomicU32::new(0),
            half_open_successes: AtomicU32::new(0),
            opened_at: RwLock::new(None),
            window_start: RwLock::new(Instant::now()),
            times_opened: AtomicU64::new(0),
            times_tripped: AtomicU64::new(0),
        }
    }

    /// Create a circuit breaker with default configuration.
    pub fn default_config() -> Self {
        Self::new(CircuitBreakerConfig::default())
    }

    /// Get the current state of the circuit.
    pub fn state(&self) -> CircuitState {
        self.maybe_transition();
        CircuitState::from_u8(self.state.load(Ordering::Acquire))
    }

    /// Check if a request should be allowed through.
    ///
    /// Returns true if the circuit allows the request, false if it should fail fast.
    pub fn allow_request(&self) -> bool {
        self.maybe_transition();

        let state = CircuitState::from_u8(self.state.load(Ordering::Acquire));
        match state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                self.times_tripped.fetch_add(1, Ordering::Relaxed);
                false
            }
            CircuitState::HalfOpen => {
                // Allow limited requests in half-open state
                true
            }
        }
    }

    /// Record a successful request.
    pub fn record_success(&self) {
        let state = CircuitState::from_u8(self.state.load(Ordering::Acquire));

        match state {
            CircuitState::Closed => {
                // Reset failure count on success
                self.failure_count.store(0, Ordering::Relaxed);
            }
            CircuitState::HalfOpen => {
                let prev = self.half_open_successes.fetch_add(1, Ordering::Relaxed);
                let successes = prev.saturating_add(1);
                let config = self.config.read();
                if successes >= config.success_threshold {
                    self.close();
                }
            }
            CircuitState::Open => {
                // Should not happen, but handle gracefully
            }
        }
    }

    /// Record a failed request.
    pub fn record_failure(&self) {
        let state = CircuitState::from_u8(self.state.load(Ordering::Acquire));

        match state {
            CircuitState::Closed => {
                self.maybe_reset_window();
                let prev = self.failure_count.fetch_add(1, Ordering::Relaxed);
                let failures = prev.saturating_add(1);
                let config = self.config.read();
                if failures >= config.failure_threshold {
                    self.open();
                }
            }
            CircuitState::HalfOpen => {
                // Any failure in half-open state reopens the circuit
                self.open();
            }
            CircuitState::Open => {
                // Already open, nothing to do
            }
        }
    }

    /// Force the circuit to open.
    pub fn trip(&self) {
        self.open();
    }

    /// Force the circuit to close.
    pub fn reset(&self) {
        self.close();
    }

    /// Get the number of times the circuit has opened.
    pub fn times_opened(&self) -> u64 {
        self.times_opened.load(Ordering::Relaxed)
    }

    /// Get the number of times requests were rejected when circuit was open.
    pub fn times_tripped(&self) -> u64 {
        self.times_tripped.load(Ordering::Relaxed)
    }

    /// Get the current failure count.
    pub fn failure_count(&self) -> u32 {
        self.failure_count.load(Ordering::Relaxed)
    }

    /// Update the circuit breaker configuration at runtime.
    ///
    /// Does NOT reset current trip/open state — thresholds apply to future
    /// state transitions only. Use `trip()` / `reset()` explicitly if needed.
    ///
    /// This is the hot-reload entry point for circuit breaker settings.
    pub fn set_config(&self, new_config: CircuitBreakerConfig) {
        *self.config.write() = new_config;
    }

    /// Get time until circuit might transition to half-open.
    pub fn time_until_half_open(&self) -> Option<Duration> {
        if CircuitState::from_u8(self.state.load(Ordering::Acquire)) != CircuitState::Open {
            return None;
        }

        let opened_at = self.opened_at.read();
        if let Some(opened) = *opened_at {
            let elapsed = opened.elapsed();
            let config = self.config.read();
            if elapsed < config.open_duration {
                return Some(config.open_duration.saturating_sub(elapsed));
            }
        }
        Some(Duration::ZERO)
    }

    fn open(&self) {
        let current = self.state.load(Ordering::Acquire);
        if current != CircuitState::Open.to_u8()
            && self
                .state
                .compare_exchange(
                    current,
                    CircuitState::Open.to_u8(),
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                )
                .is_ok()
        {
            *self.opened_at.write() = Some(Instant::now());
            self.times_opened.fetch_add(1, Ordering::Relaxed);
            self.half_open_successes.store(0, Ordering::Relaxed);
        }
    }

    fn close(&self) {
        self.state
            .store(CircuitState::Closed.to_u8(), Ordering::Release);
        *self.opened_at.write() = None;
        self.failure_count.store(0, Ordering::Relaxed);
        self.half_open_successes.store(0, Ordering::Relaxed);
        *self.window_start.write() = Instant::now();
    }

    fn half_open(&self) {
        if self
            .state
            .compare_exchange(
                CircuitState::Open.to_u8(),
                CircuitState::HalfOpen.to_u8(),
                Ordering::AcqRel,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            self.half_open_successes.store(0, Ordering::Relaxed);
        }
    }

    fn maybe_transition(&self) {
        let state = CircuitState::from_u8(self.state.load(Ordering::Acquire));

        if state == CircuitState::Open {
            let opened_at = self.opened_at.read();
            if let Some(opened) = *opened_at {
                let config = self.config.read();
                if opened.elapsed() >= config.open_duration {
                    drop(opened_at); // Release read lock before write
                    drop(config);
                    self.half_open();
                }
            }
        }
    }

    fn maybe_reset_window(&self) {
        let mut window_start = self.window_start.write();
        let config = self.config.read();
        if window_start.elapsed() >= config.failure_window {
            *window_start = Instant::now();
            drop(window_start);
            self.failure_count.store(0, Ordering::Relaxed);
        }
    }
}

/// Result wrapper for circuit breaker operations.
#[derive(Debug)]
pub enum CircuitResult<T, E> {
    /// Request succeeded.
    Success(T),
    /// Request failed.
    Failure(E),
    /// Circuit is open, request was not attempted.
    CircuitOpen,
}

impl<T, E> CircuitResult<T, E> {
    /// Check if the result is a success.
    pub fn is_success(&self) -> bool {
        matches!(self, CircuitResult::Success(_))
    }

    /// Check if the circuit was open.
    pub fn is_circuit_open(&self) -> bool {
        matches!(self, CircuitResult::CircuitOpen)
    }

    /// Convert to Option, returning None if circuit was open.
    pub fn ok(self) -> Option<Result<T, E>> {
        match self {
            CircuitResult::Success(v) => Some(Ok(v)),
            CircuitResult::Failure(e) => Some(Err(e)),
            CircuitResult::CircuitOpen => None,
        }
    }
}

#[cfg(test)]
#[path = "tests/circuit_tests.rs"]
mod tests;
