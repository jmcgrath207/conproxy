//! Audit logging for proxy requests.
//!
//! Provides a ring buffer for recent request history and structured
//! audit entries for compliance and debugging.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use parking_lot::RwLock;
use serde::Serialize;

use super::types::{CacheStatus, MissReason};

/// Audit entry for a single request.
#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    /// Unique request ID.
    pub request_id: u64,
    /// Timestamp of the request.
    pub timestamp: u64,
    /// Query text (truncated if too long).
    pub query: String,
    /// Number of results requested.
    pub top_k: Option<usize>,
    /// Cache status (hit, miss, stale, etc.).
    pub cache_status: CacheStatus,
    /// Latency in milliseconds.
    pub latency_ms: u64,
    /// Number of results returned.
    pub result_count: usize,
    /// Whether the request was successful.
    pub success: bool,
    /// Error message if failed.
    pub error: Option<String>,
    /// Client identifier (if authenticated).
    pub client_id: Option<String>,
    /// Upstream used (if applicable).
    pub upstream_id: Option<String>,
    /// Reason for cache miss (if applicable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub miss_reason: Option<MissReason>,
}

impl AuditEntry {
    /// Create a new audit entry for a request.
    pub fn new(request_id: u64, query: &str, top_k: Option<usize>) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Truncate query if too long
        let query = if query.len() > 200 {
            format!("{}...", &query[..200])
        } else {
            query.to_string()
        };

        Self {
            request_id,
            timestamp,
            query,
            top_k,
            cache_status: CacheStatus::Miss,
            latency_ms: 0,
            result_count: 0,
            success: false,
            error: None,
            client_id: None,
            upstream_id: None,
            miss_reason: None,
        }
    }

    /// Mark the request as successful.
    pub fn with_success(mut self, result_count: usize) -> Self {
        self.success = true;
        self.result_count = result_count;
        self
    }

    /// Mark the request as failed.
    pub fn with_error(mut self, error: &str) -> Self {
        self.success = false;
        self.error = Some(error.to_string());
        self
    }

    /// Set the cache status.
    pub fn with_cache_status(mut self, status: CacheStatus) -> Self {
        self.cache_status = status;
        self
    }

    /// Set the latency.
    pub fn with_latency(mut self, latency: Duration) -> Self {
        self.latency_ms = latency.as_millis() as u64;
        self
    }

    /// Set the client ID.
    pub fn with_client_id(mut self, client_id: &str) -> Self {
        self.client_id = Some(client_id.to_string());
        self
    }

    /// Set the upstream ID.
    pub fn with_upstream_id(mut self, upstream_id: &str) -> Self {
        self.upstream_id = Some(upstream_id.to_string());
        self
    }

    /// Set the miss reason.
    pub fn with_miss_reason(mut self, reason: MissReason) -> Self {
        self.miss_reason = Some(reason);
        self
    }
}

/// Builder for creating audit entries with timing.
pub struct AuditBuilder {
    entry: AuditEntry,
    start: Instant,
}

impl AuditBuilder {
    /// Start building an audit entry.
    pub fn new(request_id: u64, query: &str, top_k: Option<usize>) -> Self {
        Self {
            entry: AuditEntry::new(request_id, query, top_k),
            start: Instant::now(),
        }
    }

    /// Set the cache status.
    pub fn cache_status(mut self, status: CacheStatus) -> Self {
        self.entry.cache_status = status;
        self
    }

    /// Set the client ID.
    pub fn client_id(mut self, client_id: &str) -> Self {
        self.entry.client_id = Some(client_id.to_string());
        self
    }

    /// Set the upstream ID.
    pub fn upstream_id(mut self, upstream_id: &str) -> Self {
        self.entry.upstream_id = Some(upstream_id.to_string());
        self
    }

    /// Finish with success.
    pub fn success(mut self, result_count: usize) -> AuditEntry {
        self.entry.latency_ms = self.start.elapsed().as_millis() as u64;
        self.entry.success = true;
        self.entry.result_count = result_count;
        self.entry
    }

    /// Finish with error.
    pub fn error(mut self, error: &str) -> AuditEntry {
        self.entry.latency_ms = self.start.elapsed().as_millis() as u64;
        self.entry.success = false;
        self.entry.error = Some(error.to_string());
        self.entry
    }
}

/// Ring buffer for storing recent audit entries.
pub struct AuditLog {
    /// Ring buffer of entries.
    entries: RwLock<Vec<AuditEntry>>,
    /// Maximum number of entries to keep.
    capacity: usize,
    /// Write position in the ring buffer.
    write_pos: AtomicU64,
    /// Total entries logged (for statistics).
    total_logged: AtomicU64,
    /// Next request ID.
    next_request_id: AtomicU64,
}

impl AuditLog {
    /// Create a new audit log with the given capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: RwLock::new(Vec::with_capacity(capacity)),
            capacity,
            write_pos: AtomicU64::new(0),
            total_logged: AtomicU64::new(0),
            next_request_id: AtomicU64::new(1),
        }
    }

    /// Get the next request ID.
    pub fn next_request_id(&self) -> u64 {
        self.next_request_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Start building an audit entry with automatic request ID.
    pub fn start(&self, query: &str, top_k: Option<usize>) -> AuditBuilder {
        AuditBuilder::new(self.next_request_id(), query, top_k)
    }

    /// Log an audit entry.
    pub fn log(&self, entry: AuditEntry) {
        let mut entries = self.entries.write();
        let pos = self.write_pos.fetch_add(1, Ordering::Relaxed) as usize;

        if entries.len() < self.capacity {
            entries.push(entry);
        } else {
            #[allow(clippy::arithmetic_side_effects)]
            let idx = pos % self.capacity;
            if let Some(slot) = entries.get_mut(idx) {
                *slot = entry;
            }
        }

        self.total_logged.fetch_add(1, Ordering::Relaxed);
    }

    /// Get recent entries (newest first).
    pub fn recent(&self, limit: usize) -> Vec<AuditEntry> {
        let entries = self.entries.read();
        let len = entries.len().min(limit);

        let mut result = Vec::with_capacity(len);
        let total = self.write_pos.load(Ordering::Relaxed) as usize;

        for i in 0..len {
            #[allow(clippy::arithmetic_side_effects)]
            let idx = if total > 0 && total >= entries.len() {
                // Ring buffer has wrapped
                total.saturating_sub(1).saturating_sub(i) % entries.len()
            } else {
                // Ring buffer hasn't filled yet
                entries.len().saturating_sub(1_usize.saturating_add(i))
            };

            if let Some(entry) = entries.get(idx) {
                result.push(entry.clone());
            }
        }

        result
    }

    /// Get entries matching a filter.
    pub fn filter<F>(&self, predicate: F, limit: usize) -> Vec<AuditEntry>
    where
        F: Fn(&AuditEntry) -> bool,
    {
        let entries = self.entries.read();
        entries
            .iter()
            .filter(|e| predicate(e))
            .take(limit)
            .cloned()
            .collect()
    }

    /// Get entries for a specific client.
    pub fn for_client(&self, client_id: &str, limit: usize) -> Vec<AuditEntry> {
        self.filter(|e| e.client_id.as_deref() == Some(client_id), limit)
    }

    /// Get failed entries.
    pub fn failures(&self, limit: usize) -> Vec<AuditEntry> {
        self.filter(|e| !e.success, limit)
    }

    /// Get slow entries (above threshold).
    pub fn slow(&self, threshold_ms: u64, limit: usize) -> Vec<AuditEntry> {
        self.filter(|e| e.latency_ms > threshold_ms, limit)
    }

    /// Get statistics about the audit log.
    pub fn stats(&self) -> AuditStats {
        let entries = self.entries.read();
        let total = entries.len();

        if total == 0 {
            return AuditStats::default();
        }

        let mut success_count = 0u64;
        let mut cache_hit_count = 0u64;
        let mut total_latency = 0u64;

        for entry in entries.iter() {
            if entry.success {
                success_count = success_count.saturating_add(1);
            }
            if entry.cache_status == CacheStatus::Hit {
                cache_hit_count = cache_hit_count.saturating_add(1);
            }
            total_latency = total_latency.saturating_add(entry.latency_ms);
        }

        let total_u64 = total as u64;
        let avg_latency_ms = total_latency.checked_div(total_u64).unwrap_or(0);
        AuditStats {
            total_logged: self.total_logged.load(Ordering::Relaxed),
            entries_in_buffer: total,
            success_rate: success_count as f64 / total as f64,
            cache_hit_rate: cache_hit_count as f64 / total as f64,
            avg_latency_ms,
        }
    }

    /// Clear the audit log.
    pub fn clear(&self) {
        let mut entries = self.entries.write();
        entries.clear();
        self.write_pos.store(0, Ordering::Relaxed);
    }
}

/// Statistics from the audit log.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AuditStats {
    /// Total entries ever logged.
    pub total_logged: u64,
    /// Current entries in buffer.
    pub entries_in_buffer: usize,
    /// Success rate (0.0 to 1.0).
    pub success_rate: f64,
    /// Cache hit rate (0.0 to 1.0).
    pub cache_hit_rate: f64,
    /// Average latency in milliseconds.
    pub avg_latency_ms: u64,
}

#[cfg(test)]
#[path = "tests/audit_tests.rs"]
mod tests;
