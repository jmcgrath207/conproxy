//! Health check worker for monitoring upstream services.
//!
//! Periodically checks upstream health and updates the health tracker,
//! enabling the proxy to make informed decisions about serving stale data.

use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::proxy::upstream::{GenericRestAdapter, HealthTracker, UpstreamStatus};

/// Configuration for the health check worker.
#[derive(Debug, Clone)]
pub struct HealthCheckConfig {
    /// Interval between health checks when upstream is online.
    pub check_interval: Duration,
    /// Faster interval when upstream is offline (for quicker recovery detection).
    pub recovery_check_interval: Duration,
    /// Timeout for individual health check requests.
    pub check_timeout: Duration,
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_secs(30),
            recovery_check_interval: Duration::from_secs(5),
            check_timeout: Duration::from_secs(5),
        }
    }
}

/// Background worker that periodically checks upstream health.
pub struct HealthCheckWorker {
    /// The upstream adapter to check.
    upstream: Arc<GenericRestAdapter>,
    /// Health tracker to update with results.
    tracker: Arc<HealthTracker>,
    /// Configuration for health checks.
    config: HealthCheckConfig,
    /// Cancellation token for graceful shutdown.
    cancel: CancellationToken,
}

impl HealthCheckWorker {
    /// Create a new health check worker.
    pub fn new(
        upstream: Arc<GenericRestAdapter>,
        tracker: Arc<HealthTracker>,
        config: HealthCheckConfig,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            upstream,
            tracker,
            config,
            cancel,
        }
    }

    /// Create a health check worker with default configuration.
    pub fn with_defaults(
        upstream: Arc<GenericRestAdapter>,
        tracker: Arc<HealthTracker>,
        cancel: CancellationToken,
    ) -> Self {
        Self::new(upstream, tracker, HealthCheckConfig::default(), cancel)
    }

    /// Run the health check worker loop.
    ///
    /// This runs until the cancellation token is triggered.
    pub async fn run(&self) {
        loop {
            // Determine check interval based on current status
            let interval = match self.tracker.status() {
                UpstreamStatus::Online => self.config.check_interval,
                #[allow(clippy::arithmetic_side_effects)]
                UpstreamStatus::Degraded => self.config.check_interval / 2,
                UpstreamStatus::Offline => self.config.recovery_check_interval,
            };

            tokio::select! {
                _ = self.cancel.cancelled() => {
                    break;
                }
                _ = tokio::time::sleep(interval) => {
                    self.perform_health_check().await;
                }
            }
        }
    }

    /// Perform a single health check.
    async fn perform_health_check(&self) {
        match self.upstream.health_check().await {
            Ok(true) => {
                self.tracker.record_success();
            }
            Ok(false) | Err(_) => {
                self.tracker.record_failure();
            }
        }
    }

    /// Get the current upstream status.
    pub fn status(&self) -> UpstreamStatus {
        self.tracker.status()
    }

    /// Check if the worker is running (not cancelled).
    pub fn is_running(&self) -> bool {
        !self.cancel.is_cancelled()
    }
}

#[cfg(test)]
#[path = "tests/health_tests.rs"]
mod tests;
