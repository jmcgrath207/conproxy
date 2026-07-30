//! Connection pooling for upstream HTTP connections.
//!
//! Provides pgbouncer-style connection management for HTTP upstreams:
//! - Configurable max concurrent connections per upstream
//! - Request queueing when pool is exhausted
//! - Metrics for pool utilization and queue depth
//! - Connection recycling and health tracking
//!
//! # Pooling Mode
//!
//! - **Request**: Connection returned after each request (default, only supported mode)

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, Semaphore, SemaphorePermit};

/// Connection pool configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionPoolConfig {
    /// Maximum concurrent connections to this upstream.
    /// Default: 100
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,

    /// Maximum requests waiting in queue when pool is exhausted.
    /// Default: 500
    #[serde(default = "default_max_queue_size")]
    pub max_queue_size: usize,

    /// How long a request can wait in queue before timing out.
    /// Default: 30 seconds
    #[serde(default = "default_queue_timeout_ms")]
    pub queue_timeout_ms: u64,

    /// Connection idle timeout for recycling.
    /// Default: 90 seconds
    #[serde(default = "default_idle_timeout_ms")]
    pub idle_timeout_ms: u64,

    /// Enable fair queueing (FIFO) vs allowing some priority bypass.
    #[serde(default = "default_fair_queueing")]
    pub fair_queueing: bool,
}

fn default_max_connections() -> usize {
    100
}
fn default_max_queue_size() -> usize {
    500
}
fn default_queue_timeout_ms() -> u64 {
    30000
}
fn default_idle_timeout_ms() -> u64 {
    90000
}
fn default_fair_queueing() -> bool {
    true
}

impl Default for ConnectionPoolConfig {
    fn default() -> Self {
        Self {
            max_connections: default_max_connections(),
            max_queue_size: default_max_queue_size(),
            queue_timeout_ms: default_queue_timeout_ms(),
            idle_timeout_ms: default_idle_timeout_ms(),
            fair_queueing: default_fair_queueing(),
        }
    }
}

impl ConnectionPoolConfig {
    /// Create a new pool config with specified max connections.
    pub fn new(max_connections: usize) -> Self {
        Self {
            max_connections,
            ..Self::default()
        }
    }

    /// Set the queue timeout.
    pub fn with_queue_timeout(mut self, timeout: Duration) -> Self {
        self.queue_timeout_ms = timeout.as_millis() as u64;
        self
    }

    /// Set the max queue size.
    pub fn with_max_queue_size(mut self, size: usize) -> Self {
        self.max_queue_size = size;
        self
    }

    /// Get queue timeout as Duration.
    pub fn queue_timeout(&self) -> Duration {
        Duration::from_millis(self.queue_timeout_ms)
    }

    /// Get idle timeout as Duration.
    pub fn idle_timeout(&self) -> Duration {
        Duration::from_millis(self.idle_timeout_ms)
    }
}

/// Pooling mode for connection management.
///
/// Only `Request` mode is supported: connection returned after each request
/// (like pgbouncer transaction mode). Most efficient for stateless REST APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolingMode {
    /// Return connection after each request.
    #[default]
    Request,
}

impl PoolingMode {
    /// Get the mode as a string.
    pub fn as_str(&self) -> &'static str {
        "request"
    }
}

/// A waiting request in the queue.
struct WaitingRequest {
    /// Channel to signal when permit is available.
    ready_tx: oneshot::Sender<()>,
    /// When this request started waiting.
    enqueued_at: Instant,
}

/// Connection pool for a single upstream.
///
/// Manages concurrent connections to an upstream service with:
/// - Semaphore-based concurrency limiting
/// - Request queueing when pool is exhausted
/// - Metrics for monitoring
pub struct ConnectionPool {
    /// Semaphore to limit concurrent connections.
    permits: Arc<Semaphore>,

    /// Waiting queue for requests when pool is exhausted.
    waiting: Mutex<VecDeque<WaitingRequest>>,

    /// Pool configuration.
    config: ConnectionPoolConfig,

    /// Metrics.
    stats: ConnectionPoolStats,
}

/// Connection pool statistics.
#[derive(Debug, Default)]
pub struct ConnectionPoolStats {
    /// Current active connections.
    pub active: AtomicUsize,

    /// Current queue depth.
    pub queued: AtomicUsize,

    /// Total requests processed.
    pub total_requests: AtomicU64,

    /// Requests that acquired a permit immediately.
    pub immediate_permits: AtomicU64,

    /// Requests that had to wait in queue.
    pub queued_requests: AtomicU64,

    /// Requests that timed out waiting.
    pub queue_timeouts: AtomicU64,

    /// Requests rejected due to queue full.
    pub queue_rejections: AtomicU64,

    /// Total wait time in microseconds (for averaging).
    wait_time_us: AtomicU64,

    /// Max wait time observed in microseconds.
    max_wait_time_us: AtomicU64,
}

impl ConnectionPoolStats {
    /// Get a snapshot of the stats.
    pub fn snapshot(&self) -> ConnectionPoolSnapshot {
        let total = self.total_requests.load(Ordering::Relaxed);
        let queued = self.queued_requests.load(Ordering::Relaxed);
        let wait_sum = self.wait_time_us.load(Ordering::Relaxed);

        ConnectionPoolSnapshot {
            active_connections: self.active.load(Ordering::Relaxed),
            queue_depth: self.queued.load(Ordering::Relaxed),
            total_requests: total,
            immediate_permits: self.immediate_permits.load(Ordering::Relaxed),
            queued_requests: queued,
            queue_timeouts: self.queue_timeouts.load(Ordering::Relaxed),
            queue_rejections: self.queue_rejections.load(Ordering::Relaxed),
            avg_wait_time_ms: (wait_sum.checked_div(queued).unwrap_or(0)) as f64 / 1000.0,
            max_wait_time_ms: self.max_wait_time_us.load(Ordering::Relaxed) as f64 / 1000.0,
            utilization: if total > 0 {
                1.0 - (self.immediate_permits.load(Ordering::Relaxed) as f64 / total as f64)
            } else {
                0.0
            },
        }
    }

    fn record_wait_time(&self, duration: Duration) {
        let us = duration.as_micros() as u64;
        self.wait_time_us.fetch_add(us, Ordering::Relaxed);

        // Update max (compare-and-swap loop)
        let mut current_max = self.max_wait_time_us.load(Ordering::Relaxed);
        while us > current_max {
            match self.max_wait_time_us.compare_exchange_weak(
                current_max,
                us,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current_max = actual,
            }
        }
    }
}

/// Point-in-time snapshot of pool stats.
#[derive(Debug, Clone, Serialize)]
pub struct ConnectionPoolSnapshot {
    /// Current active connections.
    pub active_connections: usize,

    /// Current queue depth.
    pub queue_depth: usize,

    /// Total requests processed.
    pub total_requests: u64,

    /// Requests that acquired permit immediately.
    pub immediate_permits: u64,

    /// Requests that had to wait in queue.
    pub queued_requests: u64,

    /// Requests that timed out waiting.
    pub queue_timeouts: u64,

    /// Requests rejected due to queue full.
    pub queue_rejections: u64,

    /// Average wait time in milliseconds.
    pub avg_wait_time_ms: f64,

    /// Maximum wait time in milliseconds.
    pub max_wait_time_ms: f64,

    /// Pool utilization (0.0 = always available, 1.0 = always queued).
    pub utilization: f64,
}

/// Error acquiring a connection from the pool.
#[derive(Debug, Clone)]
pub enum PoolError {
    /// Queue is full, request rejected.
    QueueFull,

    /// Timed out waiting for a connection.
    Timeout,

    /// Pool is shutting down.
    Shutdown,
}

impl std::fmt::Display for PoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QueueFull => write!(f, "Connection pool queue is full"),
            Self::Timeout => write!(f, "Timed out waiting for connection"),
            Self::Shutdown => write!(f, "Connection pool is shutting down"),
        }
    }
}

impl std::error::Error for PoolError {}

/// Guard that returns the permit to the pool when dropped.
pub struct PooledConnection<'a> {
    _permit: SemaphorePermit<'a>,
    pool: &'a ConnectionPool,
    acquired_at: Instant,
}

impl Drop for PooledConnection<'_> {
    fn drop(&mut self) {
        // Decrement active count
        self.pool.stats.active.fetch_sub(1, Ordering::Relaxed);

        // Wake next waiter if any
        self.pool.wake_next_waiter();
    }
}

impl<'a> PooledConnection<'a> {
    /// Get how long this connection has been held.
    pub fn held_duration(&self) -> Duration {
        self.acquired_at.elapsed()
    }
}

impl ConnectionPool {
    /// Create a new connection pool with the given configuration.
    pub fn new(config: ConnectionPoolConfig) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(config.max_connections)),
            waiting: Mutex::new(VecDeque::new()),
            config,
            stats: ConnectionPoolStats::default(),
        }
    }

    /// Create a pool with default configuration.
    pub fn with_max_connections(max: usize) -> Self {
        Self::new(ConnectionPoolConfig::new(max))
    }

    /// Get the pool configuration.
    pub fn config(&self) -> &ConnectionPoolConfig {
        &self.config
    }

    /// Get current pool statistics.
    pub fn stats(&self) -> ConnectionPoolSnapshot {
        self.stats.snapshot()
    }

    /// Get current active connection count.
    pub fn active_connections(&self) -> usize {
        self.stats.active.load(Ordering::Relaxed)
    }

    /// Get current queue depth.
    pub fn queue_depth(&self) -> usize {
        self.stats.queued.load(Ordering::Relaxed)
    }

    /// Check if the pool has available capacity.
    pub fn has_capacity(&self) -> bool {
        self.permits.available_permits() > 0
    }

    /// Acquire a connection from the pool.
    ///
    /// If a permit is available, returns immediately.
    /// Otherwise, queues the request and waits up to `queue_timeout`.
    ///
    /// # Errors
    ///
    /// Returns `PoolError::QueueFull` if the queue is at capacity.
    /// Returns `PoolError::Timeout` if waiting times out.
    pub async fn acquire(&self) -> Result<PooledConnection<'_>, PoolError> {
        self.stats.total_requests.fetch_add(1, Ordering::Relaxed);

        // Try to acquire immediately
        match self.permits.try_acquire() {
            Ok(permit) => {
                self.stats.immediate_permits.fetch_add(1, Ordering::Relaxed);
                self.stats.active.fetch_add(1, Ordering::Relaxed);
                return Ok(PooledConnection {
                    _permit: permit,
                    pool: self,
                    acquired_at: Instant::now(),
                });
            }
            Err(_) => {
                // Pool exhausted, need to queue
            }
        }

        // Check queue capacity and enqueue in a single lock acquisition
        let (tx, rx) = oneshot::channel();
        let enqueued_at = Instant::now();
        {
            let mut queue = self.waiting.lock();
            if queue.len() >= self.config.max_queue_size {
                self.stats.queue_rejections.fetch_add(1, Ordering::Relaxed);
                return Err(PoolError::QueueFull);
            }
            queue.push_back(WaitingRequest {
                ready_tx: tx,
                enqueued_at,
            });
            self.stats.queued.fetch_add(1, Ordering::Relaxed);
        }

        self.stats.queued_requests.fetch_add(1, Ordering::Relaxed);

        // Wait for signal or timeout
        let timeout = self.config.queue_timeout();
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(())) => {
                // Signaled, try to acquire
                let wait_time = enqueued_at.elapsed();
                self.stats.record_wait_time(wait_time);

                match self.permits.try_acquire() {
                    Ok(permit) => {
                        self.stats.active.fetch_add(1, Ordering::Relaxed);
                        Ok(PooledConnection {
                            _permit: permit,
                            pool: self,
                            acquired_at: Instant::now(),
                        })
                    }
                    Err(_) => {
                        // Shouldn't happen if signaling is correct
                        self.stats.queue_timeouts.fetch_add(1, Ordering::Relaxed);
                        Err(PoolError::Timeout)
                    }
                }
            }
            Ok(Err(_)) => {
                // Channel closed, pool shutting down
                self.stats.queued.fetch_sub(1, Ordering::Relaxed);
                Err(PoolError::Shutdown)
            }
            Err(_) => {
                // Timeout — stale entry will be skipped in wake_next_waiter()
                // when the closed channel is detected (R12: removed useless lock)
                self.stats.queue_timeouts.fetch_add(1, Ordering::Relaxed);
                self.stats.queued.fetch_sub(1, Ordering::Relaxed);

                Err(PoolError::Timeout)
            }
        }
    }

    /// Wake the next waiter in the queue.
    fn wake_next_waiter(&self) {
        let mut queue = self.waiting.lock();
        let timeout = self.config.queue_timeout();

        while let Some(waiter) = queue.pop_front() {
            self.stats.queued.fetch_sub(1, Ordering::Relaxed);

            // Check if waiter has already timed out
            if waiter.enqueued_at.elapsed() > timeout {
                // Skip stale waiters
                continue;
            }

            // Try to signal
            if waiter.ready_tx.send(()).is_ok() {
                // Successfully signaled
                return;
            }
            // Receiver dropped (timed out), try next
        }
    }

    /// Drain the queue, rejecting all waiters.
    ///
    /// Called during shutdown.
    pub fn drain_queue(&self) {
        let mut queue = self.waiting.lock();
        while queue.pop_front().is_some() {
            self.stats.queued.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Get max connections setting.
    pub fn max_connections(&self) -> usize {
        self.config.max_connections
    }

    /// Get available permits.
    pub fn available_permits(&self) -> usize {
        self.permits.available_permits()
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
#[path = "tests/connection_pool_tests.rs"]
mod tests;
