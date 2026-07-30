//! Metrics collection for the cache proxy.
//!
//! Tracks cache hit/miss rates, latencies, and other performance metrics
//! using lock-free atomic counters.

use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::types::{DriftLevel, MissReason};

/// Size of the latency ring buffer for percentile calculation.
const LATENCY_RING_SIZE: usize = 1000;

/// Metrics collector for the cache proxy.
#[derive(Debug)]
pub struct ProxyMetrics {
    /// Total number of requests processed.
    pub requests_total: AtomicU64,
    /// Number of cache hits (fresh).
    pub cache_hits: AtomicU64,
    /// Number of cache misses.
    pub cache_misses: AtomicU64,
    /// Cache misses: entry not in cache (cold miss).
    pub miss_not_in_cache: AtomicU64,
    /// Cache misses: entry expired, fetched from upstream.
    pub miss_expired: AtomicU64,
    /// Cache misses: upstream request failed, no frozen fallback.
    pub miss_upstream_error: AtomicU64,
    /// Cache misses: circuit breaker open, no frozen fallback.
    pub miss_circuit_open: AtomicU64,
    /// Number of stale-while-revalidate responses.
    pub cache_stale: AtomicU64,
    /// Number of frozen responses (upstream down).
    pub cache_frozen: AtomicU64,
    /// Number of coalesced requests (waited on leader).
    pub coalesced_requests: AtomicU64,
    /// Number of upstream requests made.
    pub upstream_requests: AtomicU64,
    /// Number of upstream request failures.
    pub upstream_failures: AtomicU64,
    /// Number of validation errors (bad requests).
    pub validation_errors: AtomicU64,
    /// Total latency in microseconds (for average calculation).
    latency_sum_us: AtomicU64,
    /// Maximum latency observed in microseconds.
    latency_max_us: AtomicU64,
    /// Minimum latency observed in microseconds (initialized to max).
    latency_min_us: AtomicU64,
    /// Number of refresh operations.
    pub refresh_operations: AtomicU64,
    /// Number of evictions.
    pub evictions: AtomicU64,
    /// Number of drift alerts triggered.
    pub drift_alerts: AtomicU64,
    /// Drift observations by level.
    pub drift_none: AtomicU64,
    pub drift_minor: AtomicU64,
    pub drift_moderate: AtomicU64,
    pub drift_significant: AtomicU64,
    pub drift_major: AtomicU64,
    /// Number of integrity check failures.
    pub integrity_failures: AtomicU64,
    /// Number of schema changes detected.
    pub schema_changes: AtomicU64,
    // Cascade metrics
    /// Number of successful cascade queries (threshold met).
    pub cascade_success: AtomicU64,
    /// Number of cascade queries that exhausted all upstreams.
    pub cascade_exhausted: AtomicU64,
    /// Number of cascade queries that timed out.
    pub cascade_timeout: AtomicU64,
    /// Sum of cascade depths (for averaging).
    cascade_depth_sum: AtomicU64,
    /// Number of cascade depth observations.
    cascade_depth_count: AtomicU64,
    // Warmup metrics
    /// Number of warmup requests (POST /cache/warmup calls).
    pub warmup_requests: AtomicU64,
    /// Number of individual entries warmed (successful cache inserts from warmup).
    pub warmup_entries: AtomicU64,
    /// Number of semantic cache hits (similarity above threshold).
    #[cfg(feature = "embed-api")]
    pub semantic_hits: AtomicU64,
    /// Number of semantic cache misses (no similar query above threshold).
    #[cfg(feature = "embed-api")]
    pub semantic_misses: AtomicU64,
    /// Pre-allocated lock-free ring buffer of recent latencies (microseconds).
    latency_ring: Box<[AtomicU64]>,
    /// Write position in the latency ring buffer (monotonically increasing).
    latency_ring_pos: AtomicU64,
}

impl Default for ProxyMetrics {
    fn default() -> Self {
        Self {
            requests_total: AtomicU64::new(0),
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
            miss_not_in_cache: AtomicU64::new(0),
            miss_expired: AtomicU64::new(0),
            miss_upstream_error: AtomicU64::new(0),
            miss_circuit_open: AtomicU64::new(0),
            cache_stale: AtomicU64::new(0),
            cache_frozen: AtomicU64::new(0),
            coalesced_requests: AtomicU64::new(0),
            upstream_requests: AtomicU64::new(0),
            upstream_failures: AtomicU64::new(0),
            validation_errors: AtomicU64::new(0),
            latency_sum_us: AtomicU64::new(0),
            latency_max_us: AtomicU64::new(0),
            latency_min_us: AtomicU64::new(u64::MAX),
            refresh_operations: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            drift_alerts: AtomicU64::new(0),
            drift_none: AtomicU64::new(0),
            drift_minor: AtomicU64::new(0),
            drift_moderate: AtomicU64::new(0),
            drift_significant: AtomicU64::new(0),
            drift_major: AtomicU64::new(0),
            integrity_failures: AtomicU64::new(0),
            schema_changes: AtomicU64::new(0),
            cascade_success: AtomicU64::new(0),
            cascade_exhausted: AtomicU64::new(0),
            cascade_timeout: AtomicU64::new(0),
            cascade_depth_sum: AtomicU64::new(0),
            cascade_depth_count: AtomicU64::new(0),
            warmup_requests: AtomicU64::new(0),
            warmup_entries: AtomicU64::new(0),
            #[cfg(feature = "embed-api")]
            semantic_hits: AtomicU64::new(0),
            #[cfg(feature = "embed-api")]
            semantic_misses: AtomicU64::new(0),
            latency_ring: (0..LATENCY_RING_SIZE)
                .map(|_| AtomicU64::new(0))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            latency_ring_pos: AtomicU64::new(0),
        }
    }
}

impl ProxyMetrics {
    /// Create a new metrics collector.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a cache hit.
    pub fn record_hit(&self) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a cache miss with reason.
    pub fn record_miss(&self, reason: MissReason) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
        match reason {
            MissReason::NotInCache => self.miss_not_in_cache.fetch_add(1, Ordering::Relaxed),
            MissReason::Expired => self.miss_expired.fetch_add(1, Ordering::Relaxed),
            MissReason::UpstreamError => self.miss_upstream_error.fetch_add(1, Ordering::Relaxed),
            MissReason::CircuitOpen => self.miss_circuit_open.fetch_add(1, Ordering::Relaxed),
        };
    }

    /// Record a stale response.
    pub fn record_stale(&self) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        self.cache_stale.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a frozen response.
    pub fn record_frozen(&self) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        self.cache_frozen.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a coalesced request (not leader).
    pub fn record_coalesced(&self) {
        self.coalesced_requests.fetch_add(1, Ordering::Relaxed);
    }

    /// Record an upstream request.
    pub fn record_upstream_request(&self) {
        self.upstream_requests.fetch_add(1, Ordering::Relaxed);
    }

    /// Record an upstream failure.
    pub fn record_upstream_failure(&self) {
        self.upstream_failures.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a validation error.
    pub fn record_validation_error(&self) {
        self.validation_errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a request latency.
    pub fn record_latency(&self, duration: Duration) {
        let us = duration.as_micros() as u64;
        self.latency_sum_us.fetch_add(us, Ordering::Relaxed);

        // Update max (compare-and-swap loop)
        let mut current_max = self.latency_max_us.load(Ordering::Relaxed);
        while us > current_max {
            match self.latency_max_us.compare_exchange_weak(
                current_max,
                us,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current_max = actual,
            }
        }

        // Update min
        let mut current_min = self.latency_min_us.load(Ordering::Relaxed);
        while us < current_min {
            match self.latency_min_us.compare_exchange_weak(
                current_min,
                us,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current_min = actual,
            }
        }

        // Record to lock-free ring buffer for percentile calculation
        let pos = self.latency_ring_pos.fetch_add(1, Ordering::Relaxed) as usize;
        let ring_idx = pos % LATENCY_RING_SIZE;
        if let Some(slot) = self.latency_ring.get(ring_idx) {
            slot.store(us, Ordering::Relaxed);
        }
    }

    /// Record a refresh operation.
    pub fn record_refresh(&self) {
        self.refresh_operations.fetch_add(1, Ordering::Relaxed);
    }

    /// Record an eviction.
    pub fn record_eviction(&self) {
        self.evictions.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a drift alert.
    pub fn record_drift_alert(&self) {
        self.drift_alerts.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a drift observation by level.
    pub fn record_drift(&self, level: DriftLevel) {
        match level {
            DriftLevel::None => self.drift_none.fetch_add(1, Ordering::Relaxed),
            DriftLevel::Minor => self.drift_minor.fetch_add(1, Ordering::Relaxed),
            DriftLevel::Moderate => self.drift_moderate.fetch_add(1, Ordering::Relaxed),
            DriftLevel::Significant => {
                self.drift_significant.fetch_add(1, Ordering::Relaxed);
                // Significant drift triggers an alert
                self.drift_alerts.fetch_add(1, Ordering::Relaxed);
                0
            }
            DriftLevel::Major => {
                self.drift_major.fetch_add(1, Ordering::Relaxed);
                // Major drift triggers an alert
                self.drift_alerts.fetch_add(1, Ordering::Relaxed);
                0
            }
        };
    }

    /// Record an integrity check failure.
    pub fn record_integrity_failure(&self) {
        self.integrity_failures.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a schema change detection.
    pub fn record_schema_change(&self) {
        self.schema_changes.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a successful cascade query.
    ///
    /// # Arguments
    /// * `depth` - The cascade depth (number of upstreams tried).
    pub fn record_cascade_success(&self, depth: usize) {
        self.cascade_success.fetch_add(1, Ordering::Relaxed);
        self.cascade_depth_sum
            .fetch_add(depth as u64, Ordering::Relaxed);
        self.cascade_depth_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a cascade query that exhausted all upstreams.
    pub fn record_cascade_exhausted(&self) {
        self.cascade_exhausted.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a cascade query that timed out.
    pub fn record_cascade_timeout(&self) {
        self.cascade_timeout.fetch_add(1, Ordering::Relaxed);
    }

    /// Record cascade depth (for non-success cases).
    pub fn record_cascade_depth(&self, depth: usize) {
        self.cascade_depth_sum
            .fetch_add(depth as u64, Ordering::Relaxed);
        self.cascade_depth_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a warmup request (one POST /cache/warmup call).
    pub fn record_warmup_request(&self) {
        self.warmup_requests.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a single warmup entry (one cache insert from warmup).
    pub fn record_warmup_entry(&self) {
        self.warmup_entries.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a semantic cache hit (cosine similarity above threshold).
    #[cfg(feature = "embed-api")]
    pub fn record_semantic_hit(&self) {
        self.semantic_hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a semantic cache miss (no similar query above threshold).
    #[cfg(feature = "embed-api")]
    pub fn record_semantic_miss(&self) {
        self.semantic_misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Get the current metrics snapshot.
    pub fn snapshot(&self) -> MetricsSnapshot {
        let total = self.requests_total.load(Ordering::Relaxed);
        let hits = self.cache_hits.load(Ordering::Relaxed);
        let misses = self.cache_misses.load(Ordering::Relaxed);
        let latency_sum = self.latency_sum_us.load(Ordering::Relaxed);
        let latency_max = self.latency_max_us.load(Ordering::Relaxed);
        let latency_min = self.latency_min_us.load(Ordering::Relaxed);

        MetricsSnapshot {
            requests_total: total,
            cache_hits: hits,
            cache_misses: misses,
            miss_not_in_cache: self.miss_not_in_cache.load(Ordering::Relaxed),
            miss_expired: self.miss_expired.load(Ordering::Relaxed),
            miss_upstream_error: self.miss_upstream_error.load(Ordering::Relaxed),
            miss_circuit_open: self.miss_circuit_open.load(Ordering::Relaxed),
            cache_stale: self.cache_stale.load(Ordering::Relaxed),
            cache_frozen: self.cache_frozen.load(Ordering::Relaxed),
            cache_hit_rate: if total > 0 {
                hits as f64 / total as f64
            } else {
                0.0
            },
            coalesced_requests: self.coalesced_requests.load(Ordering::Relaxed),
            upstream_requests: self.upstream_requests.load(Ordering::Relaxed),
            upstream_failures: self.upstream_failures.load(Ordering::Relaxed),
            upstream_error_rate: {
                let upstream = self.upstream_requests.load(Ordering::Relaxed);
                let failures = self.upstream_failures.load(Ordering::Relaxed);
                if upstream > 0 {
                    failures as f64 / upstream as f64
                } else {
                    0.0
                }
            },
            validation_errors: self.validation_errors.load(Ordering::Relaxed),
            latency_avg_ms: (latency_sum.checked_div(total).unwrap_or(0)) as f64 / 1000.0,
            latency_max_ms: latency_max as f64 / 1000.0,
            latency_min_ms: if latency_min == u64::MAX {
                0.0
            } else {
                latency_min as f64 / 1000.0
            },
            refresh_operations: self.refresh_operations.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            drift_alerts: self.drift_alerts.load(Ordering::Relaxed),
            drift_none: self.drift_none.load(Ordering::Relaxed),
            drift_minor: self.drift_minor.load(Ordering::Relaxed),
            drift_moderate: self.drift_moderate.load(Ordering::Relaxed),
            drift_significant: self.drift_significant.load(Ordering::Relaxed),
            drift_major: self.drift_major.load(Ordering::Relaxed),
            integrity_failures: self.integrity_failures.load(Ordering::Relaxed),
            schema_changes: self.schema_changes.load(Ordering::Relaxed),
            cascade_success: self.cascade_success.load(Ordering::Relaxed),
            cascade_exhausted: self.cascade_exhausted.load(Ordering::Relaxed),
            cascade_timeout: self.cascade_timeout.load(Ordering::Relaxed),
            cascade_avg_depth: {
                let count = self.cascade_depth_count.load(Ordering::Relaxed);
                if count > 0 {
                    self.cascade_depth_sum.load(Ordering::Relaxed) as f64 / count as f64
                } else {
                    0.0
                }
            },
            warmup_requests: self.warmup_requests.load(Ordering::Relaxed),
            warmup_entries: self.warmup_entries.load(Ordering::Relaxed),
            #[cfg(feature = "embed-api")]
            semantic_hits: self.semantic_hits.load(Ordering::Relaxed),
            #[cfg(feature = "embed-api")]
            semantic_misses: self.semantic_misses.load(Ordering::Relaxed),
        }
    }

    /// Reset all metrics.
    pub fn reset(&self) {
        self.requests_total.store(0, Ordering::Relaxed);
        self.cache_hits.store(0, Ordering::Relaxed);
        self.cache_misses.store(0, Ordering::Relaxed);
        self.miss_not_in_cache.store(0, Ordering::Relaxed);
        self.miss_expired.store(0, Ordering::Relaxed);
        self.miss_upstream_error.store(0, Ordering::Relaxed);
        self.miss_circuit_open.store(0, Ordering::Relaxed);
        self.cache_stale.store(0, Ordering::Relaxed);
        self.cache_frozen.store(0, Ordering::Relaxed);
        self.coalesced_requests.store(0, Ordering::Relaxed);
        self.upstream_requests.store(0, Ordering::Relaxed);
        self.upstream_failures.store(0, Ordering::Relaxed);
        self.validation_errors.store(0, Ordering::Relaxed);
        self.latency_sum_us.store(0, Ordering::Relaxed);
        self.latency_max_us.store(0, Ordering::Relaxed);
        self.latency_min_us.store(u64::MAX, Ordering::Relaxed);
        self.refresh_operations.store(0, Ordering::Relaxed);
        self.evictions.store(0, Ordering::Relaxed);
        self.drift_alerts.store(0, Ordering::Relaxed);
        self.drift_none.store(0, Ordering::Relaxed);
        self.drift_minor.store(0, Ordering::Relaxed);
        self.drift_moderate.store(0, Ordering::Relaxed);
        self.drift_significant.store(0, Ordering::Relaxed);
        self.drift_major.store(0, Ordering::Relaxed);
        self.integrity_failures.store(0, Ordering::Relaxed);
        self.schema_changes.store(0, Ordering::Relaxed);
        self.cascade_success.store(0, Ordering::Relaxed);
        self.cascade_exhausted.store(0, Ordering::Relaxed);
        self.cascade_timeout.store(0, Ordering::Relaxed);
        self.cascade_depth_sum.store(0, Ordering::Relaxed);
        self.cascade_depth_count.store(0, Ordering::Relaxed);
        self.warmup_requests.store(0, Ordering::Relaxed);
        self.warmup_entries.store(0, Ordering::Relaxed);
        #[cfg(feature = "embed-api")]
        {
            self.semantic_hits.store(0, Ordering::Relaxed);
            self.semantic_misses.store(0, Ordering::Relaxed);
        }
        for slot in self.latency_ring.iter() {
            slot.store(0, Ordering::Relaxed);
        }
        self.latency_ring_pos.store(0, Ordering::Relaxed);
    }

    /// Get latency percentiles from the ring buffer.
    ///
    /// Returns P50, P95, P99 computed from recent latency samples across all requests
    /// (cache hits and misses). Lock-free: reads atomic slots without any locks.
    pub fn latency_percentiles(&self) -> LatencyPercentiles {
        let total_written = self.latency_ring_pos.load(Ordering::Relaxed);
        if total_written == 0 {
            return LatencyPercentiles {
                sample_count: 0,
                p50_ms: 0.0,
                p95_ms: 0.0,
                p99_ms: 0.0,
            };
        }

        let len = (total_written as usize).min(LATENCY_RING_SIZE);
        let mut sorted: Vec<u64> = Vec::with_capacity(len);
        sorted.extend(
            (0..len).filter_map(|i| self.latency_ring.get(i).map(|a| a.load(Ordering::Relaxed))),
        );
        sorted.sort_unstable();

        let len_f64 = len as f64;
        let p50_idx = (len_f64 * 0.50).ceil() as usize;
        let p50_idx = p50_idx.saturating_sub(1);
        let p95_idx = (len_f64 * 0.95).ceil() as usize;
        let p95_idx = p95_idx.saturating_sub(1);
        let p99_idx = (len_f64 * 0.99).ceil() as usize;
        let p99_idx = p99_idx.saturating_sub(1);
        let last_idx = len.saturating_sub(1);

        let p50_val = sorted.get(p50_idx.min(last_idx)).copied().unwrap_or(0);
        let p95_val = sorted.get(p95_idx.min(last_idx)).copied().unwrap_or(0);
        let p99_val = sorted.get(p99_idx.min(last_idx)).copied().unwrap_or(0);

        LatencyPercentiles {
            sample_count: total_written,
            p50_ms: p50_val as f64 / 1000.0,
            p95_ms: p95_val as f64 / 1000.0,
            p99_ms: p99_val as f64 / 1000.0,
        }
    }
}

/// Latency percentiles computed from the ring buffer.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LatencyPercentiles {
    /// Total number of latency samples recorded (may exceed ring buffer size).
    pub sample_count: u64,
    /// 50th percentile latency in milliseconds.
    pub p50_ms: f64,
    /// 95th percentile latency in milliseconds.
    pub p95_ms: f64,
    /// 99th percentile latency in milliseconds.
    pub p99_ms: f64,
}

/// Point-in-time snapshot of metrics.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MetricsSnapshot {
    /// Total requests processed.
    pub requests_total: u64,
    /// Cache hits.
    pub cache_hits: u64,
    /// Cache misses.
    pub cache_misses: u64,
    /// Cache misses: entry not in cache (cold miss).
    pub miss_not_in_cache: u64,
    /// Cache misses: entry expired.
    pub miss_expired: u64,
    /// Cache misses: upstream error.
    pub miss_upstream_error: u64,
    /// Cache misses: circuit breaker open.
    pub miss_circuit_open: u64,
    /// Stale-while-revalidate responses.
    pub cache_stale: u64,
    /// Frozen responses.
    pub cache_frozen: u64,
    /// Cache hit rate (0.0 to 1.0).
    pub cache_hit_rate: f64,
    /// Coalesced requests.
    pub coalesced_requests: u64,
    /// Upstream requests made.
    pub upstream_requests: u64,
    /// Upstream failures.
    pub upstream_failures: u64,
    /// Upstream error rate (0.0 to 1.0).
    pub upstream_error_rate: f64,
    /// Validation errors.
    pub validation_errors: u64,
    /// Average latency in milliseconds.
    pub latency_avg_ms: f64,
    /// Maximum latency in milliseconds.
    pub latency_max_ms: f64,
    /// Minimum latency in milliseconds.
    pub latency_min_ms: f64,
    /// Total refresh operations.
    pub refresh_operations: u64,
    /// Total evictions.
    pub evictions: u64,
    /// Drift alerts triggered.
    pub drift_alerts: u64,
    /// Drift observations with no change.
    pub drift_none: u64,
    /// Minor drift observations.
    pub drift_minor: u64,
    /// Moderate drift observations.
    pub drift_moderate: u64,
    /// Significant drift observations.
    pub drift_significant: u64,
    /// Major drift observations.
    pub drift_major: u64,
    /// Integrity check failures.
    pub integrity_failures: u64,
    /// Schema changes detected.
    pub schema_changes: u64,
    /// Successful cascade queries.
    pub cascade_success: u64,
    /// Cascade queries that exhausted all upstreams.
    pub cascade_exhausted: u64,
    /// Cascade queries that timed out.
    pub cascade_timeout: u64,
    /// Average cascade depth.
    pub cascade_avg_depth: f64,
    /// Number of warmup requests.
    pub warmup_requests: u64,
    /// Number of warmup entries cached.
    pub warmup_entries: u64,
    /// Number of semantic cache hits.
    #[cfg(feature = "embed-api")]
    pub semantic_hits: u64,
    /// Number of semantic cache misses.
    #[cfg(feature = "embed-api")]
    pub semantic_misses: u64,
}

impl MetricsSnapshot {
    /// Format metrics in Prometheus/OpenMetrics text format.
    ///
    /// This produces metrics compatible with Prometheus scraping.
    /// Example output:
    /// ```text
    /// # HELP conproxy_requests_total Total number of requests processed.
    /// # TYPE conproxy_requests_total counter
    /// conproxy_requests_total 12345
    /// ```
    pub fn to_prometheus(&self) -> String {
        let mut out = String::with_capacity(4096);

        // Helper macro for counter metrics (uses writeln! to avoid format!() allocation)
        macro_rules! counter {
            ($name:expr, $help:expr, $value:expr) => {
                let _ = writeln!(
                    out,
                    "# HELP conproxy_{} {}\n# TYPE conproxy_{} counter\nconproxy_{} {}",
                    $name, $help, $name, $name, $value
                );
            };
        }

        // Helper macro for gauge metrics (uses writeln! to avoid format!() allocation)
        macro_rules! gauge {
            ($name:expr, $help:expr, $value:expr) => {
                let _ = writeln!(
                    out,
                    "# HELP conproxy_{} {}\n# TYPE conproxy_{} gauge\nconproxy_{} {}",
                    $name, $help, $name, $name, $value
                );
            };
        }

        // Request counters
        counter!(
            "requests_total",
            "Total number of requests processed",
            self.requests_total
        );
        counter!(
            "cache_hits_total",
            "Number of cache hits (fresh responses)",
            self.cache_hits
        );
        counter!(
            "cache_misses_total",
            "Number of cache misses",
            self.cache_misses
        );

        // Miss reasons by label (follows drift_observations_total pattern)
        out.push_str("# HELP conproxy_cache_miss_reasons_total Number of cache misses by reason\n");
        out.push_str("# TYPE conproxy_cache_miss_reasons_total counter\n");
        let _ = writeln!(
            out,
            "conproxy_cache_miss_reasons_total{{reason=\"not_in_cache\"}} {}",
            self.miss_not_in_cache
        );
        let _ = writeln!(
            out,
            "conproxy_cache_miss_reasons_total{{reason=\"expired\"}} {}",
            self.miss_expired
        );
        let _ = writeln!(
            out,
            "conproxy_cache_miss_reasons_total{{reason=\"upstream_error\"}} {}",
            self.miss_upstream_error
        );
        let _ = writeln!(
            out,
            "conproxy_cache_miss_reasons_total{{reason=\"circuit_open\"}} {}",
            self.miss_circuit_open
        );

        counter!(
            "cache_stale_total",
            "Number of stale-while-revalidate responses",
            self.cache_stale
        );
        counter!(
            "cache_frozen_total",
            "Number of frozen responses (upstream unavailable)",
            self.cache_frozen
        );
        counter!(
            "coalesced_requests_total",
            "Number of coalesced requests (waited on leader)",
            self.coalesced_requests
        );

        // Upstream counters
        counter!(
            "upstream_requests_total",
            "Number of upstream requests made",
            self.upstream_requests
        );
        counter!(
            "upstream_failures_total",
            "Number of upstream request failures",
            self.upstream_failures
        );

        // Validation and error counters
        counter!(
            "validation_errors_total",
            "Number of validation errors (bad requests)",
            self.validation_errors
        );
        counter!(
            "integrity_failures_total",
            "Number of integrity check failures",
            self.integrity_failures
        );
        counter!(
            "schema_changes_total",
            "Number of schema changes detected",
            self.schema_changes
        );

        // Operations counters
        counter!(
            "refresh_operations_total",
            "Number of refresh operations",
            self.refresh_operations
        );
        counter!(
            "evictions_total",
            "Number of cache evictions",
            self.evictions
        );

        // Drift counters
        counter!(
            "drift_alerts_total",
            "Number of drift alerts triggered",
            self.drift_alerts
        );

        // Drift by level using labels
        out.push_str(
            "# HELP conproxy_drift_observations_total Number of drift observations by level\n",
        );
        out.push_str("# TYPE conproxy_drift_observations_total counter\n");
        let _ = writeln!(
            out,
            "conproxy_drift_observations_total{{level=\"none\"}} {}",
            self.drift_none
        );
        let _ = writeln!(
            out,
            "conproxy_drift_observations_total{{level=\"minor\"}} {}",
            self.drift_minor
        );
        let _ = writeln!(
            out,
            "conproxy_drift_observations_total{{level=\"moderate\"}} {}",
            self.drift_moderate
        );
        let _ = writeln!(
            out,
            "conproxy_drift_observations_total{{level=\"significant\"}} {}",
            self.drift_significant
        );
        let _ = writeln!(
            out,
            "conproxy_drift_observations_total{{level=\"major\"}} {}",
            self.drift_major
        );

        // Cascade counters
        counter!(
            "cascade_success_total",
            "Number of successful cascade queries",
            self.cascade_success
        );
        counter!(
            "cascade_exhausted_total",
            "Number of cascade queries that exhausted all upstreams",
            self.cascade_exhausted
        );
        counter!(
            "cascade_timeout_total",
            "Number of cascade queries that timed out",
            self.cascade_timeout
        );
        gauge!(
            "cascade_avg_depth",
            "Average cascade depth (upstreams tried per query)",
            self.cascade_avg_depth
        );

        // Warmup counters
        counter!(
            "warmup_requests_total",
            "Number of warmup requests (POST /cache/warmup calls)",
            self.warmup_requests
        );
        counter!(
            "warmup_entries_total",
            "Number of entries warmed (cache inserts from warmup)",
            self.warmup_entries
        );

        // Semantic cache counters
        #[cfg(feature = "embed-api")]
        {
            counter!(
                "semantic_hits_total",
                "Number of semantic cache hits (cosine similarity above threshold)",
                self.semantic_hits
            );
            counter!(
                "semantic_misses_total",
                "Number of semantic cache misses (no similar query above threshold)",
                self.semantic_misses
            );
        }

        // Rate gauges
        gauge!(
            "cache_hit_rate",
            "Cache hit rate (0.0 to 1.0)",
            self.cache_hit_rate
        );
        gauge!(
            "upstream_error_rate",
            "Upstream error rate (0.0 to 1.0)",
            self.upstream_error_rate
        );

        // Latency gauges (in milliseconds, but Prometheus convention is seconds)
        gauge!(
            "latency_avg_seconds",
            "Average request latency in seconds",
            self.latency_avg_ms / 1000.0
        );
        gauge!(
            "latency_max_seconds",
            "Maximum request latency in seconds",
            self.latency_max_ms / 1000.0
        );
        gauge!(
            "latency_min_seconds",
            "Minimum request latency in seconds",
            self.latency_min_ms / 1000.0
        );

        out
    }

    /// Format metrics in Prometheus text format with optional cache info.
    ///
    /// # Arguments
    /// * `cache_entries` - Current number of entries in cache
    /// * `cache_memory_bytes` - Current memory usage of cache in bytes
    /// * `cache_max_entries` - Maximum allowed cache entries
    pub fn to_prometheus_with_cache(
        &self,
        cache_entries: u64,
        cache_memory_bytes: u64,
        cache_max_entries: u64,
    ) -> String {
        let mut out = self.to_prometheus();

        // Cache gauge metrics
        let _ = writeln!(
            out,
            "# HELP conproxy_cache_entries Current number of entries in cache\n\
             # TYPE conproxy_cache_entries gauge\n\
             conproxy_cache_entries {}",
            cache_entries
        );

        let _ = writeln!(
            out,
            "# HELP conproxy_cache_memory_bytes Current memory usage of cache in bytes\n\
             # TYPE conproxy_cache_memory_bytes gauge\n\
             conproxy_cache_memory_bytes {}",
            cache_memory_bytes
        );

        let _ = writeln!(
            out,
            "# HELP conproxy_cache_max_entries Maximum allowed cache entries\n\
             # TYPE conproxy_cache_max_entries gauge\n\
             conproxy_cache_max_entries {}",
            cache_max_entries
        );

        let utilization = if cache_max_entries > 0 {
            cache_entries as f64 / cache_max_entries as f64
        } else {
            0.0
        };
        let _ = writeln!(
            out,
            "# HELP conproxy_cache_utilization Cache utilization (0.0 to 1.0)\n\
             # TYPE conproxy_cache_utilization gauge\n\
             conproxy_cache_utilization {}",
            utilization
        );

        out
    }

    /// Format metrics in Prometheus text format with pool statistics.
    ///
    /// # Arguments
    /// * `cache_entries` - Current number of entries in cache
    /// * `cache_memory_bytes` - Current memory usage of cache in bytes
    /// * `cache_max_entries` - Maximum allowed cache entries
    /// * `pool_params` - Pool statistics including upstream type counts
    pub fn to_prometheus_with_pool(
        &self,
        cache_entries: u64,
        cache_memory_bytes: u64,
        cache_max_entries: u64,
        pool_params: &PoolMetricsParams,
    ) -> String {
        let mut out =
            self.to_prometheus_with_cache(cache_entries, cache_memory_bytes, cache_max_entries);

        // Pool gauge metrics
        let _ = writeln!(
            out,
            "# HELP conproxy_pool_upstreams_total Total number of configured upstreams\n\
             # TYPE conproxy_pool_upstreams_total gauge\n\
             conproxy_pool_upstreams_total {}",
            pool_params.total_upstreams
        );

        let _ = writeln!(
            out,
            "# HELP conproxy_pool_upstreams_healthy Number of healthy upstreams\n\
             # TYPE conproxy_pool_upstreams_healthy gauge\n\
             conproxy_pool_upstreams_healthy {}",
            pool_params.healthy_upstreams
        );

        // Upstream type counts with labels
        out.push_str(
            "# HELP conproxy_pool_upstreams_by_type Number of upstreams by backend type\n\
             # TYPE conproxy_pool_upstreams_by_type gauge\n",
        );
        let _ = writeln!(
            out,
            "conproxy_pool_upstreams_by_type{{type=\"fts\"}} {}",
            pool_params.fts_count
        );
        let _ = writeln!(
            out,
            "conproxy_pool_upstreams_by_type{{type=\"vector_db\"}} {}",
            pool_params.vector_db_count
        );
        let _ = writeln!(
            out,
            "conproxy_pool_upstreams_by_type{{type=\"hybrid\"}} {}",
            pool_params.hybrid_count
        );
        let _ = writeln!(
            out,
            "conproxy_pool_upstreams_by_type{{type=\"unknown\"}} {}",
            pool_params.unknown_count
        );

        // Connection pool metrics
        let _ = writeln!(
            out,
            "# HELP conproxy_pool_active_connections Number of active connections\n\
             # TYPE conproxy_pool_active_connections gauge\n\
             conproxy_pool_active_connections {}",
            pool_params.active_connections
        );

        let _ = writeln!(
            out,
            "# HELP conproxy_pool_utilization Connection pool utilization (0.0 to 1.0)\n\
             # TYPE conproxy_pool_utilization gauge\n\
             conproxy_pool_utilization {}",
            pool_params.pool_utilization
        );

        out
    }
}

/// Parameters for pool-related Prometheus metrics.
pub struct PoolMetricsParams {
    /// Total number of configured upstreams.
    pub total_upstreams: usize,
    /// Number of healthy upstreams.
    pub healthy_upstreams: usize,
    /// Number of full-text search upstreams.
    pub fts_count: usize,
    /// Number of vector database upstreams.
    pub vector_db_count: usize,
    /// Number of hybrid upstreams.
    pub hybrid_count: usize,
    /// Number of unknown-type upstreams.
    pub unknown_count: usize,
    /// Number of active connections.
    pub active_connections: usize,
    /// Connection pool utilization (0.0 to 1.0).
    pub pool_utilization: f64,
}

/// Per-context stats for Prometheus export.
pub struct ContextMetricsEntry {
    /// Context identifier.
    pub id: String,
    /// Cache hits for this context.
    pub hits: u64,
    /// Cache misses for this context.
    pub misses: u64,
    /// Hit rate (0.0 to 1.0).
    pub hit_rate: f64,
}

/// Per-agent stats for Prometheus export.
pub struct AgentMetricsEntry {
    /// Agent identifier.
    pub id: String,
    /// Total requests from this agent.
    pub requests_total: u64,
    /// Cache hits for this agent.
    pub cache_hits: u64,
    /// Cache misses for this agent.
    pub cache_misses: u64,
    /// Rate-limited requests for this agent.
    pub rate_limited: u64,
    /// Context-denied requests for this agent.
    pub context_denied: u64,
}

impl MetricsSnapshot {
    /// Append per-context cache metrics to a Prometheus output string.
    pub fn append_context_stats_prometheus(out: &mut String, contexts: &[ContextMetricsEntry]) {
        if contexts.is_empty() {
            return;
        }

        out.push_str(
            "# HELP conproxy_context_cache_hits_total Cache hits by context\n\
             # TYPE conproxy_context_cache_hits_total counter\n",
        );
        for ctx in contexts {
            let _ = writeln!(
                out,
                "conproxy_context_cache_hits_total{{context=\"{}\"}} {}",
                ctx.id, ctx.hits
            );
        }

        out.push_str(
            "# HELP conproxy_context_cache_misses_total Cache misses by context\n\
             # TYPE conproxy_context_cache_misses_total counter\n",
        );
        for ctx in contexts {
            let _ = writeln!(
                out,
                "conproxy_context_cache_misses_total{{context=\"{}\"}} {}",
                ctx.id, ctx.misses
            );
        }

        out.push_str(
            "# HELP conproxy_context_hit_rate Cache hit rate by context\n\
             # TYPE conproxy_context_hit_rate gauge\n",
        );
        for ctx in contexts {
            let _ = writeln!(
                out,
                "conproxy_context_hit_rate{{context=\"{}\"}} {}",
                ctx.id, ctx.hit_rate
            );
        }
    }

    /// Append per-agent metrics to an existing Prometheus output string.
    pub fn append_agent_metrics_prometheus(out: &mut String, agents: &[AgentMetricsEntry]) {
        if agents.is_empty() {
            return;
        }

        out.push_str(
            "# HELP conproxy_agent_requests_total Total requests per agent\n\
             # TYPE conproxy_agent_requests_total counter\n",
        );
        for agent in agents {
            let _ = writeln!(
                out,
                "conproxy_agent_requests_total{{agent=\"{}\"}} {}",
                agent.id, agent.requests_total
            );
        }

        out.push_str(
            "# HELP conproxy_agent_cache_hits_total Cache hits per agent\n\
             # TYPE conproxy_agent_cache_hits_total counter\n",
        );
        for agent in agents {
            let _ = writeln!(
                out,
                "conproxy_agent_cache_hits_total{{agent=\"{}\"}} {}",
                agent.id, agent.cache_hits
            );
        }

        out.push_str(
            "# HELP conproxy_agent_cache_misses_total Cache misses per agent\n\
             # TYPE conproxy_agent_cache_misses_total counter\n",
        );
        for agent in agents {
            let _ = writeln!(
                out,
                "conproxy_agent_cache_misses_total{{agent=\"{}\"}} {}",
                agent.id, agent.cache_misses
            );
        }

        out.push_str(
            "# HELP conproxy_agent_rate_limited_total Rate limited requests per agent\n\
             # TYPE conproxy_agent_rate_limited_total counter\n",
        );
        for agent in agents {
            let _ = writeln!(
                out,
                "conproxy_agent_rate_limited_total{{agent=\"{}\"}} {}",
                agent.id, agent.rate_limited
            );
        }

        out.push_str(
            "# HELP conproxy_agent_context_denied_total Context denied requests per agent\n\
             # TYPE conproxy_agent_context_denied_total counter\n",
        );
        for agent in agents {
            let _ = writeln!(
                out,
                "conproxy_agent_context_denied_total{{agent=\"{}\"}} {}",
                agent.id, agent.context_denied
            );
        }
    }

    /// Append adaptive timeout percentiles to an existing Prometheus output string.
    pub fn append_adaptive_timeout_prometheus(
        out: &mut String,
        stats: &super::adaptive::AdaptiveTimeoutStats,
    ) {
        out.push_str(
            "# HELP conproxy_adaptive_timeout_seconds Current adaptive timeout in seconds\n\
             # TYPE conproxy_adaptive_timeout_seconds gauge\n",
        );
        let _ = writeln!(
            out,
            "conproxy_adaptive_timeout_seconds {}",
            stats.current_timeout_ms / 1000.0
        );

        out.push_str(
            "# HELP conproxy_adaptive_timeout_samples Number of latency samples in the adaptive timeout ring buffer\n\
             # TYPE conproxy_adaptive_timeout_samples gauge\n",
        );
        let _ = writeln!(
            out,
            "conproxy_adaptive_timeout_samples {}",
            stats.sample_count
        );

        if stats.sample_count > 0 {
            out.push_str(
                "# HELP conproxy_upstream_latency_seconds Upstream latency percentiles from adaptive timeout\n\
                 # TYPE conproxy_upstream_latency_seconds gauge\n",
            );
            let _ = writeln!(
                out,
                "conproxy_upstream_latency_seconds{{quantile=\"0.5\"}} {}",
                stats.p50_latency_ms / 1000.0
            );
            let _ = writeln!(
                out,
                "conproxy_upstream_latency_seconds{{quantile=\"0.95\"}} {}",
                stats.p95_latency_ms / 1000.0
            );
            let _ = writeln!(
                out,
                "conproxy_upstream_latency_seconds{{quantile=\"0.99\"}} {}",
                stats.p99_latency_ms / 1000.0
            );
            let _ = writeln!(
                out,
                "conproxy_upstream_latency_seconds{{quantile=\"min\"}} {}",
                stats.min_latency_ms / 1000.0
            );
            let _ = writeln!(
                out,
                "conproxy_upstream_latency_seconds{{quantile=\"max\"}} {}",
                stats.max_latency_ms / 1000.0
            );
            let _ = writeln!(
                out,
                "conproxy_upstream_latency_seconds{{quantile=\"avg\"}} {}",
                stats.avg_latency_ms / 1000.0
            );
        }
    }

    /// Append circuit breaker metrics to an existing Prometheus output string.
    pub fn append_circuit_breaker_prometheus(
        out: &mut String,
        state_value: u64,
        failure_count: u32,
        times_opened: u64,
        times_tripped: u64,
    ) {
        out.push_str(
            "# HELP conproxy_circuit_state Circuit breaker state (0=closed, 1=half_open, 2=open)\n\
             # TYPE conproxy_circuit_state gauge\n",
        );
        let _ = writeln!(out, "conproxy_circuit_state {}", state_value);

        out.push_str(
            "# HELP conproxy_circuit_failure_count Current failure count in the circuit breaker window\n\
             # TYPE conproxy_circuit_failure_count gauge\n",
        );
        let _ = writeln!(out, "conproxy_circuit_failure_count {}", failure_count);

        out.push_str(
            "# HELP conproxy_circuit_times_opened_total Total times the circuit breaker has opened\n\
             # TYPE conproxy_circuit_times_opened_total counter\n",
        );
        let _ = writeln!(out, "conproxy_circuit_times_opened_total {}", times_opened);

        out.push_str(
            "# HELP conproxy_circuit_times_tripped_total Total requests rejected by open circuit\n\
             # TYPE conproxy_circuit_times_tripped_total counter\n",
        );
        let _ = writeln!(
            out,
            "conproxy_circuit_times_tripped_total {}",
            times_tripped
        );
    }

    /// Append per-upstream metrics to an existing Prometheus output string.
    pub fn append_per_upstream_prometheus(
        out: &mut String,
        upstreams: &[(String, u64, u64, f64, String)], // (id, requests, failures, failure_rate, status)
    ) {
        if upstreams.is_empty() {
            return;
        }

        out.push_str(
            "# HELP conproxy_upstream_requests_by_upstream Total requests per upstream\n\
             # TYPE conproxy_upstream_requests_by_upstream counter\n",
        );
        for (id, requests, _, _, _) in upstreams {
            let _ = writeln!(
                out,
                "conproxy_upstream_requests_by_upstream{{upstream=\"{}\"}} {}",
                id, requests
            );
        }

        out.push_str(
            "# HELP conproxy_upstream_failures_by_upstream Total failures per upstream\n\
             # TYPE conproxy_upstream_failures_by_upstream counter\n",
        );
        for (id, _, failures, _, _) in upstreams {
            let _ = writeln!(
                out,
                "conproxy_upstream_failures_by_upstream{{upstream=\"{}\"}} {}",
                id, failures
            );
        }

        out.push_str(
            "# HELP conproxy_upstream_failure_rate_by_upstream Failure rate per upstream (0.0 to 1.0)\n\
             # TYPE conproxy_upstream_failure_rate_by_upstream gauge\n",
        );
        for (id, _, _, rate, _) in upstreams {
            let _ = writeln!(
                out,
                "conproxy_upstream_failure_rate_by_upstream{{upstream=\"{}\"}} {}",
                id, rate
            );
        }

        out.push_str(
            "# HELP conproxy_upstream_status Per-upstream health status (0=offline, 1=degraded, 2=online)\n\
             # TYPE conproxy_upstream_status gauge\n",
        );
        for (id, _, _, _, status) in upstreams {
            let value = match status.as_str() {
                "online" => 2,
                "degraded" => 1,
                _ => 0,
            };
            let _ = writeln!(
                out,
                "conproxy_upstream_status{{upstream=\"{}\"}} {}",
                id, value
            );
        }
    }

    /// Append request throughput gauge to an existing Prometheus output string.
    pub fn append_throughput_prometheus(out: &mut String, uptime_secs: u64, requests_total: u64) {
        let rps = if uptime_secs > 0 {
            requests_total as f64 / uptime_secs as f64
        } else {
            0.0
        };

        out.push_str(
            "# HELP conproxy_requests_per_second Average request throughput (requests/second)\n\
             # TYPE conproxy_requests_per_second gauge\n",
        );
        let _ = writeln!(out, "conproxy_requests_per_second {:.2}", rps);
    }

    /// Append all-request latency percentiles to an existing Prometheus output string.
    pub fn append_latency_percentiles_prometheus(
        out: &mut String,
        percentiles: &LatencyPercentiles,
    ) {
        if percentiles.sample_count == 0 {
            return;
        }

        out.push_str(
            "# HELP conproxy_request_latency_seconds All-request latency percentiles (cache hits + misses)\n\
             # TYPE conproxy_request_latency_seconds gauge\n",
        );
        let _ = writeln!(
            out,
            "conproxy_request_latency_seconds{{quantile=\"0.5\"}} {}",
            percentiles.p50_ms / 1000.0
        );
        let _ = writeln!(
            out,
            "conproxy_request_latency_seconds{{quantile=\"0.95\"}} {}",
            percentiles.p95_ms / 1000.0
        );
        let _ = writeln!(
            out,
            "conproxy_request_latency_seconds{{quantile=\"0.99\"}} {}",
            percentiles.p99_ms / 1000.0
        );
    }

    /// Append request queue metrics to Prometheus output.
    pub fn append_queue_prometheus(out: &mut String, stats: &crate::proxy::priority::QueueStats) {
        let _ = writeln!(
            out,
            "# HELP conproxy_queue_depth Total items in request queue\n\
             # TYPE conproxy_queue_depth gauge\n\
             conproxy_queue_depth {}",
            stats.total
        );
        let _ = writeln!(
            out,
            "# HELP conproxy_queue_max_size Maximum request queue capacity\n\
             # TYPE conproxy_queue_max_size gauge\n\
             conproxy_queue_max_size {}",
            stats.max_size
        );
        out.push_str(
            "# HELP conproxy_queue_by_priority Queue depth by priority level\n\
             # TYPE conproxy_queue_by_priority gauge\n",
        );
        let _ = writeln!(
            out,
            "conproxy_queue_by_priority{{priority=\"low\"}} {}",
            stats.low_priority
        );
        let _ = writeln!(
            out,
            "conproxy_queue_by_priority{{priority=\"normal\"}} {}",
            stats.normal_priority
        );
        let _ = writeln!(
            out,
            "conproxy_queue_by_priority{{priority=\"high\"}} {}",
            stats.high_priority
        );
        let _ = writeln!(
            out,
            "conproxy_queue_by_priority{{priority=\"critical\"}} {}",
            stats.critical_priority
        );
    }

    /// Append client tracker metrics to Prometheus output.
    pub fn append_client_tracker_prometheus(
        out: &mut String,
        active: usize,
        completed: u64,
        rejected: u64,
    ) {
        let _ = writeln!(
            out,
            "# HELP conproxy_clients_active Number of active client requests\n\
             # TYPE conproxy_clients_active gauge\n\
             conproxy_clients_active {}",
            active
        );
        let _ = writeln!(
            out,
            "# HELP conproxy_clients_completed_total Total completed client requests\n\
             # TYPE conproxy_clients_completed_total counter\n\
             conproxy_clients_completed_total {}",
            completed
        );
        let _ = writeln!(
            out,
            "# HELP conproxy_clients_rejected_total Total rejected client requests\n\
             # TYPE conproxy_clients_rejected_total counter\n\
             conproxy_clients_rejected_total {}",
            rejected
        );
    }

    /// Append SmartEmbedder cache stats (feature `embed-api`).
    #[cfg(feature = "embed-api")]
    pub fn append_smart_embedder_prometheus(
        out: &mut String,
        stats: &crate::proxy::smart_embedder::SmartEmbedderStats,
    ) {
        let _ = writeln!(
            out,
            "# HELP conproxy_smart_embedder_cache_size Current embedding cache entries\n\
             # TYPE conproxy_smart_embedder_cache_size gauge\n\
             conproxy_smart_embedder_cache_size{{model=\"{}\"}} {}",
            stats.model_name, stats.cache_size
        );
        let _ = writeln!(
            out,
            "# HELP conproxy_smart_embedder_cache_hits_total Embedding cache hits\n\
             # TYPE conproxy_smart_embedder_cache_hits_total counter\n\
             conproxy_smart_embedder_cache_hits_total{{model=\"{}\"}} {}",
            stats.model_name, stats.cache_hits
        );
        let _ = writeln!(
            out,
            "# HELP conproxy_smart_embedder_cache_misses_total Embedding cache misses\n\
             # TYPE conproxy_smart_embedder_cache_misses_total counter\n\
             conproxy_smart_embedder_cache_misses_total{{model=\"{}\"}} {}",
            stats.model_name, stats.cache_misses
        );
        let _ = writeln!(
            out,
            "# HELP conproxy_smart_embedder_embeddings_computed_total Embeddings computed\n\
             # TYPE conproxy_smart_embedder_embeddings_computed_total counter\n\
             conproxy_smart_embedder_embeddings_computed_total{{model=\"{}\"}} {}",
            stats.model_name, stats.embeddings_computed
        );
        let _ = writeln!(
            out,
            "# HELP conproxy_smart_embedder_coalesced_total Coalesced embed requests\n\
             # TYPE conproxy_smart_embedder_coalesced_total counter\n\
             conproxy_smart_embedder_coalesced_total{{model=\"{}\"}} {}",
            stats.model_name, stats.coalesced_requests
        );
    }
}

/// Timer guard that records latency on drop.
pub struct LatencyTimer<'a> {
    metrics: &'a ProxyMetrics,
    start: Instant,
}

impl<'a> LatencyTimer<'a> {
    /// Create a new latency timer.
    pub fn new(metrics: &'a ProxyMetrics) -> Self {
        Self {
            metrics,
            start: Instant::now(),
        }
    }
}

impl Drop for LatencyTimer<'_> {
    fn drop(&mut self) {
        self.metrics.record_latency(self.start.elapsed());
    }
}

#[cfg(test)]
#[path = "tests/metrics_tests.rs"]
mod tests;
