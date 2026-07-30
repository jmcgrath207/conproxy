//! Query statistics tracking for understanding access patterns.
//!
//! Tracks query frequency, latency distribution, and identifies
//! hot queries for optimization.

use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use serde::Serialize;

/// Statistics for a single query.
#[derive(Debug)]
pub struct QueryStats {
    /// Number of times this query was requested.
    pub count: AtomicU64,
    /// Number of cache hits.
    pub hits: AtomicU64,
    /// Number of cache misses.
    pub misses: AtomicU64,
    /// Total latency in microseconds.
    pub total_latency_us: AtomicU64,
    /// Maximum latency in microseconds.
    pub max_latency_us: AtomicU64,
    /// Minimum latency in microseconds.
    pub min_latency_us: AtomicU64,
    /// First seen timestamp (unix seconds).
    pub first_seen: u64,
    /// Last seen timestamp (unix seconds).
    pub last_seen: AtomicU64,
}

impl QueryStats {
    /// Create new query stats.
    pub fn new(timestamp: u64) -> Self {
        Self {
            count: AtomicU64::new(1),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(1),
            total_latency_us: AtomicU64::new(0),
            max_latency_us: AtomicU64::new(0),
            min_latency_us: AtomicU64::new(u64::MAX),
            first_seen: timestamp,
            last_seen: AtomicU64::new(timestamp),
        }
    }

    /// Record a request.
    pub fn record(&self, is_hit: bool, latency: Duration, timestamp: u64) {
        self.count.fetch_add(1, Ordering::Relaxed);

        if is_hit {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }

        let latency_us = latency.as_micros() as u64;
        self.total_latency_us
            .fetch_add(latency_us, Ordering::Relaxed);

        // Update max latency
        loop {
            let current = self.max_latency_us.load(Ordering::Relaxed);
            if latency_us <= current {
                break;
            }
            if self
                .max_latency_us
                .compare_exchange(current, latency_us, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
        }

        // Update min latency
        loop {
            let current = self.min_latency_us.load(Ordering::Relaxed);
            if latency_us >= current {
                break;
            }
            if self
                .min_latency_us
                .compare_exchange(current, latency_us, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
        }

        self.last_seen.store(timestamp, Ordering::Relaxed);
    }

    /// Get the hit rate.
    pub fn hit_rate(&self) -> f64 {
        let count = self.count.load(Ordering::Relaxed);
        if count == 0 {
            return 0.0;
        }
        self.hits.load(Ordering::Relaxed) as f64 / count as f64
    }

    /// Get the average latency.
    pub fn avg_latency(&self) -> Duration {
        let count = self.count.load(Ordering::Relaxed);
        if count == 0 {
            return Duration::ZERO;
        }
        #[allow(clippy::arithmetic_side_effects)]
        let avg_us = self.total_latency_us.load(Ordering::Relaxed) / count;
        Duration::from_micros(avg_us)
    }

    /// Get the request count.
    pub fn request_count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }
}

/// Serializable query stats snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct QueryStatsSnapshot {
    /// Query text (or hash if configured).
    pub query: String,
    /// Number of requests.
    pub count: u64,
    /// Number of cache hits.
    pub hits: u64,
    /// Number of cache misses.
    pub misses: u64,
    /// Hit rate (0.0 to 1.0).
    pub hit_rate: f64,
    /// Average latency in milliseconds.
    pub avg_latency_ms: f64,
    /// Maximum latency in milliseconds.
    pub max_latency_ms: f64,
    /// Minimum latency in milliseconds.
    pub min_latency_ms: f64,
    /// First seen timestamp.
    pub first_seen: u64,
    /// Last seen timestamp.
    pub last_seen: u64,
}

impl QueryStatsSnapshot {
    fn from_stats(query: &str, stats: &QueryStats) -> Self {
        let count = stats.count.load(Ordering::Relaxed);
        let hits = stats.hits.load(Ordering::Relaxed);
        let misses = stats.misses.load(Ordering::Relaxed);
        let total_latency_us = stats.total_latency_us.load(Ordering::Relaxed);
        let max_us = stats.max_latency_us.load(Ordering::Relaxed);
        let min_us = stats.min_latency_us.load(Ordering::Relaxed);

        let avg_us = total_latency_us.checked_div(count).unwrap_or(0);
        let min_us = if min_us == u64::MAX { 0 } else { min_us };

        Self {
            query: query.to_string(),
            count,
            hits,
            misses,
            hit_rate: if count > 0 {
                hits as f64 / count as f64
            } else {
                0.0
            },
            avg_latency_ms: avg_us as f64 / 1000.0,
            max_latency_ms: max_us as f64 / 1000.0,
            min_latency_ms: min_us as f64 / 1000.0,
            first_seen: stats.first_seen,
            last_seen: stats.last_seen.load(Ordering::Relaxed),
        }
    }
}

/// Tracker for query access patterns.
pub struct QueryStatsTracker {
    /// Stats by query.
    stats: DashMap<String, QueryStats>,
    /// Maximum queries to track (LRU eviction when exceeded).
    max_queries: usize,
    /// Whether to store full query text (false = hash only).
    store_query_text: bool,
}

impl QueryStatsTracker {
    /// Create a new query stats tracker.
    pub fn new(max_queries: usize) -> Self {
        Self {
            stats: DashMap::new(),
            max_queries,
            store_query_text: true,
        }
    }

    /// Create a tracker that stores only query hashes (for privacy).
    pub fn hash_only(max_queries: usize) -> Self {
        Self {
            stats: DashMap::new(),
            max_queries,
            store_query_text: false,
        }
    }

    /// Record a query request.
    pub fn record(&self, query: &str, is_hit: bool, latency: Duration) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let key = self.query_key(query);

        if let Some(stats) = self.stats.get(&key) {
            stats.record(is_hit, latency, timestamp);
        } else {
            // Check capacity before inserting
            if self.stats.len() >= self.max_queries {
                self.evict_oldest();
            }

            let stats = QueryStats::new(timestamp);
            if !is_hit {
                stats.misses.store(1, Ordering::Relaxed);
            } else {
                stats.hits.store(1, Ordering::Relaxed);
                stats.misses.store(0, Ordering::Relaxed);
            }
            stats
                .total_latency_us
                .store(latency.as_micros() as u64, Ordering::Relaxed);
            stats
                .max_latency_us
                .store(latency.as_micros() as u64, Ordering::Relaxed);
            stats
                .min_latency_us
                .store(latency.as_micros() as u64, Ordering::Relaxed);

            self.stats.insert(key, stats);
        }
    }

    /// Get stats for a specific query.
    pub fn get(&self, query: &str) -> Option<QueryStatsSnapshot> {
        let key = self.query_key(query);
        self.stats
            .get(&key)
            .map(|s| QueryStatsSnapshot::from_stats(&key, &s))
    }

    /// Get the top N queries by request count.
    pub fn top_by_count(&self, n: usize) -> Vec<QueryStatsSnapshot> {
        let mut entries: Vec<_> = self
            .stats
            .iter()
            .map(|r| QueryStatsSnapshot::from_stats(r.key(), r.value()))
            .collect();

        entries.sort_by_key(|e| std::cmp::Reverse(e.count));
        entries.truncate(n);
        entries
    }

    /// Get the top N queries by average latency.
    pub fn top_by_latency(&self, n: usize) -> Vec<QueryStatsSnapshot> {
        let mut entries: Vec<_> = self
            .stats
            .iter()
            .map(|r| QueryStatsSnapshot::from_stats(r.key(), r.value()))
            .collect();

        entries.sort_by(|a, b| {
            b.avg_latency_ms
                .partial_cmp(&a.avg_latency_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        entries.truncate(n);
        entries
    }

    /// Get queries with low hit rates.
    pub fn low_hit_rate(&self, threshold: f64, limit: usize) -> Vec<QueryStatsSnapshot> {
        let mut entries: Vec<_> = self
            .stats
            .iter()
            .filter(|r| {
                let count = r.count.load(Ordering::Relaxed);
                count > 1 // At least 2 requests to have meaningful rate
            })
            .map(|r| QueryStatsSnapshot::from_stats(r.key(), r.value()))
            .filter(|s| s.hit_rate < threshold)
            .collect();

        entries.sort_by(|a, b| {
            a.hit_rate
                .partial_cmp(&b.hit_rate)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        entries.truncate(limit);
        entries
    }

    /// Get total number of tracked queries.
    pub fn len(&self) -> usize {
        self.stats.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.stats.is_empty()
    }

    /// Get aggregate statistics.
    pub fn aggregate(&self) -> AggregateStats {
        let mut total_requests = 0u64;
        let mut total_hits = 0u64;
        let mut total_latency_us = 0u64;

        for entry in self.stats.iter() {
            total_requests = total_requests.saturating_add(entry.count.load(Ordering::Relaxed));
            total_hits = total_hits.saturating_add(entry.hits.load(Ordering::Relaxed));
            total_latency_us =
                total_latency_us.saturating_add(entry.total_latency_us.load(Ordering::Relaxed));
        }

        AggregateStats {
            unique_queries: self.stats.len(),
            total_requests,
            total_hits,
            hit_rate: if total_requests > 0 {
                total_hits as f64 / total_requests as f64
            } else {
                0.0
            },
            avg_latency_ms: (total_latency_us.checked_div(total_requests).unwrap_or(0)) as f64
                / 1000.0,
        }
    }

    /// Clear all statistics.
    pub fn clear(&self) {
        self.stats.clear();
    }

    fn query_key(&self, query: &str) -> String {
        if self.store_query_text {
            query.to_string()
        } else {
            // Use hash for privacy
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            query.hash(&mut hasher);
            format!("hash:{:x}", hasher.finish())
        }
    }

    fn evict_oldest(&self) {
        // Find and remove the entry with the oldest last_seen
        let mut oldest_key = None;
        let mut oldest_time = u64::MAX;

        for entry in self.stats.iter() {
            let last_seen = entry.last_seen.load(Ordering::Relaxed);
            if last_seen < oldest_time {
                oldest_time = last_seen;
                oldest_key = Some(entry.key().clone());
            }
        }

        if let Some(key) = oldest_key {
            self.stats.remove(&key);
        }
    }
}

/// Aggregate statistics across all queries.
#[derive(Debug, Clone, Serialize)]
pub struct AggregateStats {
    /// Number of unique queries tracked.
    pub unique_queries: usize,
    /// Total requests across all queries.
    pub total_requests: u64,
    /// Total cache hits.
    pub total_hits: u64,
    /// Overall hit rate.
    pub hit_rate: f64,
    /// Average latency in milliseconds.
    pub avg_latency_ms: f64,
}

#[cfg(test)]
#[path = "tests/query_stats_tests.rs"]
mod tests;
