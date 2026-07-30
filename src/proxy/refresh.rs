//! Background refresh worker for proactive cache maintenance.
//!
//! Periodically scans for stale entries and refreshes them in the background
//! before they expire, ensuring fresh data is available for clients.

use dashmap::DashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use super::cache::CacheStore;
use super::metrics::ProxyMetrics;
use super::types::{detect_drift, DriftAggregator, QueryHash, QueryRequest, SchemaFingerprint};
use super::upstream::GenericRestAdapter;
use parking_lot::Mutex;

/// Background worker that proactively refreshes stale cache entries.
pub struct RefreshWorker {
    /// Reference to the cache store.
    cache: Arc<CacheStore>,
    /// Reference to the upstream adapter.
    upstream: Arc<GenericRestAdapter>,
    /// Unique identifier for refreshed entries.
    upstream_id: String,
    /// Interval between refresh cycles.
    refresh_interval: Duration,
    /// Tracks hashes currently being refreshed (deduplication).
    pending: DashMap<QueryHash, ()>,
    /// Cancellation token for graceful shutdown.
    cancel: CancellationToken,
}

impl RefreshWorker {
    /// Create a new refresh worker.
    pub fn new(
        cache: Arc<CacheStore>,
        upstream: Arc<GenericRestAdapter>,
        upstream_id: String,
        refresh_interval: Duration,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            cache,
            upstream,
            upstream_id,
            refresh_interval,
            pending: DashMap::new(),
            cancel,
        }
    }

    /// Run the refresh worker loop.
    ///
    /// This runs until the cancellation token is triggered.
    pub async fn run(&self) {
        let mut interval = tokio::time::interval(self.refresh_interval);
        // Don't tick immediately on start
        interval.tick().await;

        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    break;
                }
                _ = interval.tick() => {
                    self.refresh_stale_entries().await;
                }
            }
        }
    }

    /// Refresh all stale entries.
    async fn refresh_stale_entries(&self) {
        let stale_hashes = self.cache.get_stale_hashes();

        for hash in stale_hashes {
            // Skip if already being refreshed
            if self.pending.contains_key(&hash) {
                continue;
            }

            // Mark as pending
            self.pending.insert(hash, ());

            // Spawn refresh task
            let cache = self.cache.clone();
            let upstream = self.upstream.clone();
            let upstream_id = self.upstream_id.clone();
            let pending = self.pending.clone();

            tokio::spawn(async move {
                // Get the cached entry to extract the original query
                if let Some(entry) = cache.get_by_hash(&hash) {
                    if let Some(query_text) = &entry.query_text {
                        // Build request from cached query
                        let request = QueryRequest {
                            query: query_text.clone(),
                            top_k: Some(entry.response.results.len().max(10)),
                            priority: None,
                            upstream_id: None,
                            upstream_type: None,
                        };

                        // Try to refresh from upstream
                        match upstream.query(&request).await {
                            Ok(response) => {
                                // Update cache with fresh response
                                cache.insert_by_hash(
                                    hash,
                                    response,
                                    upstream_id,
                                    Some(query_text.as_str()),
                                );
                            }
                            Err(e) => {
                                // Log error but keep stale entry
                                tracing::warn!("Failed to refresh entry: {}", e);
                            }
                        }
                    }
                }

                // Remove from pending regardless of outcome
                pending.remove(&hash);
            });
        }
    }

    /// Get the number of pending refresh operations.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Check if the worker is running (not cancelled).
    pub fn is_running(&self) -> bool {
        !self.cancel.is_cancelled()
    }
}

/// Extended cache entry that includes the original query for refresh support.
#[derive(Clone)]
pub struct RefreshableEntry {
    /// The query hash.
    pub hash: QueryHash,
    /// The original query string (for refresh).
    pub query: String,
}

/// Refresh worker with query tracking.
///
/// This variant stores query strings to enable full refresh support.
pub struct QueryTrackingRefreshWorker {
    /// Reference to the cache store.
    cache: Arc<CacheStore>,
    /// Reference to the upstream adapter.
    upstream: Arc<GenericRestAdapter>,
    /// Unique identifier for refreshed entries.
    upstream_id: String,
    /// Interval between refresh cycles.
    refresh_interval: Duration,
    /// Maps hashes to their original queries.
    query_map: DashMap<QueryHash, String>,
    /// Tracks hashes currently being refreshed.
    pending: DashMap<QueryHash, ()>,
    /// Aggregates drift metrics across refreshes.
    drift_aggregator: Arc<Mutex<DriftAggregator>>,
    /// Last known schema fingerprint from upstream.
    last_schema: Arc<Mutex<Option<SchemaFingerprint>>>,
    /// Optional metrics collector.
    metrics: Option<Arc<ProxyMetrics>>,
    /// Cancellation token for graceful shutdown.
    cancel: CancellationToken,
}

impl QueryTrackingRefreshWorker {
    /// Create a new query-tracking refresh worker.
    pub fn new(
        cache: Arc<CacheStore>,
        upstream: Arc<GenericRestAdapter>,
        upstream_id: String,
        refresh_interval: Duration,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            cache,
            upstream,
            upstream_id,
            refresh_interval,
            query_map: DashMap::new(),
            pending: DashMap::new(),
            drift_aggregator: Arc::new(Mutex::new(DriftAggregator::with_defaults())),
            last_schema: Arc::new(Mutex::new(None)),
            metrics: None,
            cancel,
        }
    }

    /// Create with metrics collector.
    pub fn with_metrics(
        cache: Arc<CacheStore>,
        upstream: Arc<GenericRestAdapter>,
        upstream_id: String,
        refresh_interval: Duration,
        metrics: Arc<ProxyMetrics>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            cache,
            upstream,
            upstream_id,
            refresh_interval,
            query_map: DashMap::new(),
            pending: DashMap::new(),
            drift_aggregator: Arc::new(Mutex::new(DriftAggregator::with_defaults())),
            last_schema: Arc::new(Mutex::new(None)),
            metrics: Some(metrics),
            cancel,
        }
    }

    /// Create with custom drift aggregator settings.
    pub fn with_drift_settings(
        cache: Arc<CacheStore>,
        upstream: Arc<GenericRestAdapter>,
        upstream_id: String,
        refresh_interval: Duration,
        drift_aggregator: DriftAggregator,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            cache,
            upstream,
            upstream_id,
            refresh_interval,
            query_map: DashMap::new(),
            pending: DashMap::new(),
            drift_aggregator: Arc::new(Mutex::new(drift_aggregator)),
            last_schema: Arc::new(Mutex::new(None)),
            metrics: None,
            cancel,
        }
    }

    /// Register a query for potential refresh.
    ///
    /// Call this when caching a new response to enable background refresh.
    pub fn register_query(&self, query: &str) {
        let hash = CacheStore::hash_query(query);
        self.query_map.insert(hash, query.to_string());
    }

    /// Unregister a query (e.g., when evicted from cache).
    pub fn unregister_query(&self, query: &str) {
        let hash = CacheStore::hash_query(query);
        self.query_map.remove(&hash);
    }

    /// Run the refresh worker loop.
    pub async fn run(&self) {
        let mut interval = tokio::time::interval(self.refresh_interval);
        interval.tick().await;

        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    break;
                }
                _ = interval.tick() => {
                    self.refresh_stale_entries().await;
                }
            }
        }
    }

    /// Refresh all stale entries.
    async fn refresh_stale_entries(&self) {
        let stale_hashes = self.cache.get_stale_hashes();

        for hash in stale_hashes {
            // Skip if already being refreshed
            if self.pending.contains_key(&hash) {
                continue;
            }

            // Get the original query
            let query = match self.query_map.get(&hash) {
                Some(q) => q.clone(),
                None => continue, // Query not tracked, skip
            };

            // Mark as pending
            self.pending.insert(hash, ());

            // Spawn refresh task
            let cache = self.cache.clone();
            let upstream = self.upstream.clone();
            let upstream_id = self.upstream_id.clone();
            let pending = self.pending.clone();
            let drift_aggregator = self.drift_aggregator.clone();
            let metrics = self.metrics.clone();

            tokio::spawn(async move {
                let request = QueryRequest {
                    query: query.clone(),
                    top_k: None,
                    priority: None,
                    upstream_id: None,
                    upstream_type: None,
                };

                // Get old entry for drift comparison
                let old_entry = cache.get(&query);

                match upstream.query(&request).await {
                    Ok(new_response) => {
                        // Detect drift if we have an old entry
                        if let Some(old) = old_entry {
                            let drift = detect_drift(&old.response, &new_response);

                            // Record drift in aggregator
                            {
                                let mut agg = drift_aggregator.lock();
                                agg.record(drift, hash);
                            }

                            // Record drift in metrics
                            if let Some(ref m) = metrics {
                                m.record_drift(drift);
                                m.record_refresh();
                            }
                        } else if let Some(ref m) = metrics {
                            // No old entry, just record refresh
                            m.record_refresh();
                        }

                        cache.insert(&query, new_response, upstream_id);
                    }
                    Err(_) => {
                        // Failed to refresh, entry will remain stale
                        // It can still be served as stale-while-revalidate
                    }
                }

                pending.remove(&hash);
            });
        }
    }

    /// Get the number of tracked queries.
    pub fn tracked_count(&self) -> usize {
        self.query_map.len()
    }

    /// Get the number of pending refresh operations.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Get drift summary.
    pub fn drift_summary(&self) -> super::types::DriftSummary {
        self.drift_aggregator.lock().summary()
    }

    /// Check if drift alert is triggered.
    pub fn has_drift_alert(&self) -> bool {
        self.drift_aggregator.lock().should_alert()
    }

    /// Clear drift observations.
    pub fn clear_drift(&self) {
        self.drift_aggregator.lock().clear();
    }

    /// Get the last known schema fingerprint.
    pub fn last_schema(&self) -> Option<SchemaFingerprint> {
        self.last_schema.lock().clone()
    }

    /// Update the last known schema fingerprint.
    pub fn update_schema(&self, schema: SchemaFingerprint) {
        let mut guard = self.last_schema.lock();
        // Check for schema change
        if let Some(ref old) = *guard {
            if !old.is_compatible(&schema) {
                if let Some(ref m) = self.metrics {
                    m.record_schema_change();
                }
            }
        }
        *guard = Some(schema);
    }
}

#[cfg(test)]
#[path = "tests/refresh_tests.rs"]
mod tests;
