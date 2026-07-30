//! Recovery worker for gradual upstream recovery.
//!
//! When an upstream transitions from Offline to Online, this worker
//! manages the gradual recovery process with rate limiting to avoid
//! overwhelming the upstream with cached stale requests.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::proxy::cache::CacheStore;
use crate::proxy::upstream::{GenericRestAdapter, HealthTracker, UpstreamStatus};

/// Configuration for recovery behavior.
#[derive(Debug, Clone)]
pub struct RecoveryConfig {
    /// Initial requests per second during recovery.
    pub initial_rps: f32,
    /// Maximum requests per second at full recovery.
    pub max_rps: f32,
    /// How much to increase RPS each successful batch.
    pub rps_increment: f32,
    /// Batch size for recovery requests.
    pub batch_size: usize,
    /// Delay between batches.
    pub batch_delay: Duration,
    /// How long upstream must be stable before full recovery.
    pub stability_period: Duration,
}

impl Default for RecoveryConfig {
    fn default() -> Self {
        Self {
            initial_rps: 1.0,
            max_rps: 50.0,
            rps_increment: 5.0,
            batch_size: 5,
            batch_delay: Duration::from_millis(200),
            stability_period: Duration::from_secs(30),
        }
    }
}

/// State of the recovery process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryState {
    /// No recovery needed - upstream is online and stable.
    Stable,
    /// Recovery in progress with gradual ramp-up.
    Recovering,
    /// Recovery paused due to errors.
    Paused,
    /// Upstream is offline - no recovery possible.
    Offline,
}

/// Tracks recovery progress and rate limiting.
pub struct RecoveryTracker {
    /// Current recovery state.
    state: AtomicU32,
    /// Current requests per second limit.
    current_rps: std::sync::atomic::AtomicU32,
    /// Whether recovery is active.
    is_recovering: AtomicBool,
    /// Consecutive successful batches.
    successful_batches: AtomicU32,
    /// Required successful batches for stability.
    required_batches: u32,
}

impl RecoveryTracker {
    /// Create a new recovery tracker.
    pub fn new(config: &RecoveryConfig) -> Self {
        // Calculate required batches based on stability period
        let batch_interval_secs = config.batch_delay.as_secs_f32();
        let required = if batch_interval_secs > 0.0 {
            (config.stability_period.as_secs_f32() / batch_interval_secs) as u32
        } else {
            10
        };

        Self {
            state: AtomicU32::new(RecoveryState::Stable as u32),
            current_rps: AtomicU32::new((config.initial_rps * 100.0) as u32), // Store as fixed point
            is_recovering: AtomicBool::new(false),
            successful_batches: AtomicU32::new(0),
            required_batches: required.max(1),
        }
    }

    /// Get current recovery state.
    pub fn state(&self) -> RecoveryState {
        match self.state.load(Ordering::SeqCst) {
            0 => RecoveryState::Stable,
            1 => RecoveryState::Recovering,
            2 => RecoveryState::Paused,
            _ => RecoveryState::Offline,
        }
    }

    fn set_state(&self, state: RecoveryState) {
        self.state.store(state as u32, Ordering::SeqCst);
    }

    /// Get current RPS limit.
    pub fn current_rps(&self) -> f32 {
        self.current_rps.load(Ordering::SeqCst) as f32 / 100.0
    }

    fn set_rps(&self, rps: f32) {
        self.current_rps
            .store((rps * 100.0) as u32, Ordering::SeqCst);
    }

    /// Check if we're currently recovering.
    pub fn is_recovering(&self) -> bool {
        self.is_recovering.load(Ordering::SeqCst)
    }

    /// Start recovery process.
    pub fn start_recovery(&self, initial_rps: f32) {
        self.is_recovering.store(true, Ordering::SeqCst);
        self.set_state(RecoveryState::Recovering);
        self.set_rps(initial_rps);
        self.successful_batches.store(0, Ordering::SeqCst);
    }

    /// Record a successful batch.
    pub fn record_success(&self, rps_increment: f32, max_rps: f32) {
        let prev = self.successful_batches.fetch_add(1, Ordering::SeqCst);
        let batches = prev.saturating_add(1);

        // Increase RPS
        let current_rps = self.current_rps();
        let new_rps = (current_rps + rps_increment).min(max_rps);
        self.set_rps(new_rps);

        // Check if stable
        if batches >= self.required_batches {
            self.complete_recovery();
        }
    }

    /// Record a failed batch.
    pub fn record_failure(&self) {
        self.successful_batches.store(0, Ordering::SeqCst);
        self.set_state(RecoveryState::Paused);
    }

    /// Complete recovery and return to stable state.
    pub fn complete_recovery(&self) {
        self.is_recovering.store(false, Ordering::SeqCst);
        self.set_state(RecoveryState::Stable);
    }

    /// Transition to offline state.
    pub fn go_offline(&self) {
        self.is_recovering.store(false, Ordering::SeqCst);
        self.set_state(RecoveryState::Offline);
    }
}

/// Background worker that manages recovery from upstream failures.
pub struct RecoveryWorker {
    /// The cache to refresh during recovery.
    cache: Arc<CacheStore>,
    /// The upstream adapter.
    upstream: Arc<GenericRestAdapter>,
    /// Health tracker for upstream status.
    health: Arc<HealthTracker>,
    /// Recovery tracker for rate limiting.
    tracker: Arc<RecoveryTracker>,
    /// Configuration.
    config: RecoveryConfig,
    /// Query map for tracking original queries.
    query_map: Arc<dashmap::DashMap<[u8; 32], String>>,
    /// Cancellation token.
    cancel: CancellationToken,
}

impl RecoveryWorker {
    /// Create a new recovery worker.
    pub fn new(
        cache: Arc<CacheStore>,
        upstream: Arc<GenericRestAdapter>,
        health: Arc<HealthTracker>,
        query_map: Arc<dashmap::DashMap<[u8; 32], String>>,
        config: RecoveryConfig,
        cancel: CancellationToken,
    ) -> Self {
        let tracker = Arc::new(RecoveryTracker::new(&config));
        Self {
            cache,
            upstream,
            health,
            tracker,
            config,
            query_map,
            cancel,
        }
    }

    /// Create with default configuration.
    pub fn with_defaults(
        cache: Arc<CacheStore>,
        upstream: Arc<GenericRestAdapter>,
        health: Arc<HealthTracker>,
        query_map: Arc<dashmap::DashMap<[u8; 32], String>>,
        cancel: CancellationToken,
    ) -> Self {
        Self::new(
            cache,
            upstream,
            health,
            query_map,
            RecoveryConfig::default(),
            cancel,
        )
    }

    /// Get the recovery tracker.
    pub fn tracker(&self) -> Arc<RecoveryTracker> {
        self.tracker.clone()
    }

    /// Run the recovery worker loop.
    pub async fn run(&self) {
        let mut was_offline = false;

        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    break;
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    let status = self.health.status();

                    match status {
                        UpstreamStatus::Offline => {
                            was_offline = true;
                            self.tracker.go_offline();
                        }
                        UpstreamStatus::Online | UpstreamStatus::Degraded => {
                            // Start recovery if we were offline
                            if was_offline && !self.tracker.is_recovering() {
                                was_offline = false;
                                self.tracker.start_recovery(self.config.initial_rps);
                            }

                            // Process recovery if active
                            if self.tracker.is_recovering() {
                                self.process_recovery_batch().await;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Process a batch of recovery requests.
    async fn process_recovery_batch(&self) {
        // Get stale entries to refresh (limited to batch size)
        let all_stale = self.cache.get_stale_hashes();
        let stale_hashes: Vec<_> = all_stale.into_iter().take(self.config.batch_size).collect();
        if stale_hashes.is_empty() {
            // No stale entries - recovery complete
            self.tracker.complete_recovery();
            return;
        }

        let mut success_count: u32 = 0;
        let mut failure_count: u32 = 0;

        for hash in stale_hashes {
            // Look up original query
            let query = match self.query_map.get(&hash) {
                Some(q) => q.clone(),
                None => continue, // Skip if we don't have the original query
            };

            // Rate limit based on current RPS
            let delay = Duration::from_secs_f32(1.0 / self.tracker.current_rps());
            tokio::time::sleep(delay).await;

            // Refresh the entry
            match self.refresh_entry(&hash, &query).await {
                Ok(_) => {
                    success_count = success_count.saturating_add(1);
                    self.health.record_success();
                }
                Err(_) => {
                    failure_count = failure_count.saturating_add(1);
                    self.health.record_failure();
                }
            }
        }

        // Update recovery state based on batch results
        if failure_count > success_count {
            self.tracker.record_failure();
        } else {
            self.tracker
                .record_success(self.config.rps_increment, self.config.max_rps);
        }

        // Delay before next batch
        tokio::time::sleep(self.config.batch_delay).await;
    }

    /// Refresh a single cache entry.
    async fn refresh_entry(&self, hash: &[u8; 32], query: &str) -> Result<(), ()> {
        let request = crate::proxy::types::QueryRequest {
            query: query.to_string(),
            top_k: Some(10), // Default top_k for refresh
            priority: None,
            upstream_id: None,
            upstream_type: None,
        };
        match self.upstream.query(&request).await {
            Ok(response) => {
                self.cache.insert_by_hash(
                    *hash,
                    response,
                    self.upstream.base_url().to_string(),
                    Some(query),
                );
                Ok(())
            }
            Err(_) => Err(()),
        }
    }
}

#[cfg(test)]
#[path = "tests/recovery_tests.rs"]
mod tests;
