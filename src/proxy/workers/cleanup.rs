//! Cleanup worker for periodic cache maintenance.
//!
//! Periodically removes truly expired entries and runs integrity checks.

use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::proxy::cache::CacheStore;
use crate::proxy::metrics::ProxyMetrics;

/// Configuration for the cleanup worker.
#[derive(Debug, Clone)]
pub struct CleanupConfig {
    /// Interval between cleanup runs.
    pub interval: Duration,
    /// Whether to run integrity checks during cleanup.
    pub check_integrity: bool,
}

impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(300), // 5 minutes
            check_integrity: false,
        }
    }
}

/// Background worker that periodically cleans up expired cache entries.
pub struct CleanupWorker {
    cache: Arc<CacheStore>,
    metrics: Option<Arc<ProxyMetrics>>,
    config: CleanupConfig,
    cancel: CancellationToken,
}

impl CleanupWorker {
    /// Create a new cleanup worker.
    pub fn new(cache: Arc<CacheStore>, config: CleanupConfig, cancel: CancellationToken) -> Self {
        Self {
            cache,
            metrics: None,
            config,
            cancel,
        }
    }

    /// Create a cleanup worker with metrics.
    pub fn with_metrics(
        cache: Arc<CacheStore>,
        metrics: Arc<ProxyMetrics>,
        config: CleanupConfig,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            cache,
            metrics: Some(metrics),
            config,
            cancel,
        }
    }

    /// Run the cleanup worker loop.
    pub async fn run(&self) {
        let mut interval = tokio::time::interval(self.config.interval);
        // Don't tick immediately
        interval.tick().await;

        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    break;
                }
                _ = interval.tick() => {
                    self.cleanup().await;
                }
            }
        }
    }

    /// Perform a cleanup cycle.
    async fn cleanup(&self) {
        // Evict truly expired entries
        let evicted = self.cache.evict_truly_expired();
        if evicted > 0 {
            if let Some(ref m) = self.metrics {
                for _ in 0..evicted {
                    m.record_eviction();
                }
            }
        }

        // Optionally run integrity check
        if self.config.check_integrity {
            let report = self.cache.verify_integrity();
            if report.invalid > 0 {
                if let Some(ref m) = self.metrics {
                    for _ in 0..report.invalid {
                        m.record_integrity_failure();
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "tests/cleanup_tests.rs"]
mod tests;
