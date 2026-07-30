//! Observability primitives beyond Prometheus metrics.
//!
//! Provides request-level tracing, cache mutation audit logging,
//! and request ID propagation for debugging and compliance.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use parking_lot::RwLock;
use serde::Serialize;

// ---------------------------------------------------------------------------
// Request ID
// ---------------------------------------------------------------------------

/// Unique identifier for request correlation across log lines and responses.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct RequestId(String);

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

impl RequestId {
    /// Generate a new request ID (monotonic counter + process start prefix).
    pub fn generate() -> Self {
        let seq = REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        Self(format!("req-{seq:016x}"))
    }

    /// Create from an incoming header value (trusted, no validation beyond length).
    pub fn from_header(value: &str) -> Option<Self> {
        if value.is_empty() || value.len() > 128 {
            return None;
        }
        Some(Self(value.to_string()))
    }

    /// Get the inner string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Cache Mutation Audit
// ---------------------------------------------------------------------------

/// Type of cache mutation for audit logging.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheMutation {
    Insert,
    Evict,
    Clear,
    Warmup,
    Invalidate,
}

/// Reason for an eviction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationEvictionReason {
    Ttl,
    Capacity,
    Manual,
    VersionDrift,
}

/// Scope of a clear operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClearScope {
    All,
    Context(String),
    Pattern(String),
}

/// Single audit entry for a cache mutation.
#[derive(Debug, Clone, Serialize)]
pub struct MutationAuditEntry {
    pub timestamp_ms: u64,
    pub mutation: CacheMutation,
    pub key: String,
    pub context: Option<String>,
    pub eviction_reason: Option<MutationEvictionReason>,
    pub clear_scope: Option<ClearScope>,
    pub request_id: Option<String>,
    pub entries_affected: u32,
}

impl MutationAuditEntry {
    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    /// Create an insert entry.
    pub fn insert(
        key: impl Into<String>,
        context: Option<String>,
        request_id: Option<&RequestId>,
    ) -> Self {
        Self {
            timestamp_ms: Self::now_ms(),
            mutation: CacheMutation::Insert,
            key: key.into(),
            context,
            eviction_reason: None,
            clear_scope: None,
            request_id: request_id.map(|r| r.as_str().to_string()),
            entries_affected: 1,
        }
    }

    /// Create an eviction entry.
    pub fn evict(key: impl Into<String>, reason: MutationEvictionReason) -> Self {
        Self {
            timestamp_ms: Self::now_ms(),
            mutation: CacheMutation::Evict,
            key: key.into(),
            context: None,
            eviction_reason: Some(reason),
            clear_scope: None,
            request_id: None,
            entries_affected: 1,
        }
    }

    /// Create a clear entry.
    pub fn clear(scope: ClearScope, entries_affected: u32) -> Self {
        Self {
            timestamp_ms: Self::now_ms(),
            mutation: CacheMutation::Clear,
            key: String::new(),
            context: None,
            eviction_reason: None,
            clear_scope: Some(scope),
            request_id: None,
            entries_affected,
        }
    }
}

/// Bounded ring buffer for cache mutation audit entries.
pub struct CacheMutationLog {
    entries: RwLock<VecDeque<MutationAuditEntry>>,
    capacity: usize,
    total_logged: AtomicU64,
}

impl CacheMutationLog {
    /// Create a new log with the given capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: RwLock::new(VecDeque::with_capacity(capacity.min(10_000))),
            capacity,
            total_logged: AtomicU64::new(0),
        }
    }

    /// Record a mutation. Drops oldest entry when at capacity.
    pub fn record(&self, entry: MutationAuditEntry) {
        self.total_logged.fetch_add(1, Ordering::Relaxed);
        let mut entries = self.entries.write();
        if entries.len() >= self.capacity {
            entries.pop_front();
        }
        entries.push_back(entry);
    }

    /// Get recent entries (newest first).
    pub fn recent(&self, limit: usize) -> Vec<MutationAuditEntry> {
        let entries = self.entries.read();
        entries.iter().rev().take(limit).cloned().collect()
    }

    /// Total entries ever logged (including evicted from ring buffer).
    pub fn total_logged(&self) -> u64 {
        self.total_logged.load(Ordering::Relaxed)
    }

    /// Current number of entries in the buffer.
    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.read().is_empty()
    }
}

// ---------------------------------------------------------------------------
// Request Trace
// ---------------------------------------------------------------------------

/// A named timing stage within a request trace.
#[derive(Debug, Clone, Serialize)]
pub struct TraceStage {
    pub name: String,
    pub duration_us: u64,
}

/// Completed trace for a single request.
#[derive(Debug, Clone, Serialize)]
pub struct RequestTrace {
    pub request_id: String,
    pub total_duration_us: u64,
    pub stages: Vec<TraceStage>,
    pub cache_hit: bool,
    pub upstream_name: Option<String>,
}

/// Builder for constructing per-request traces.
pub struct TraceBuilder {
    request_id: RequestId,
    start: Instant,
    stages: Vec<(String, Instant, Option<Duration>)>,
    cache_hit: bool,
    upstream_name: Option<String>,
}

impl TraceBuilder {
    /// Start a new trace.
    pub fn new(request_id: RequestId) -> Self {
        Self {
            request_id,
            start: Instant::now(),
            stages: Vec::new(),
            cache_hit: false,
            upstream_name: None,
        }
    }

    /// Begin a named stage. Returns the stage index.
    pub fn begin_stage(&mut self, name: impl Into<String>) -> usize {
        let idx = self.stages.len();
        self.stages.push((name.into(), Instant::now(), None));
        idx
    }

    /// End a stage by index.
    pub fn end_stage(&mut self, idx: usize) {
        if let Some(stage) = self.stages.get_mut(idx) {
            stage.2 = Some(stage.1.elapsed());
        }
    }

    /// Mark whether the request was a cache hit.
    pub fn set_cache_hit(&mut self, hit: bool) {
        self.cache_hit = hit;
    }

    /// Set the upstream name that served the request.
    pub fn set_upstream(&mut self, name: impl Into<String>) {
        self.upstream_name = Some(name.into());
    }

    /// Finalize the trace.
    pub fn finish(self) -> RequestTrace {
        let total = self.start.elapsed();
        let stages = self
            .stages
            .into_iter()
            .map(|(name, start, dur)| TraceStage {
                name,
                duration_us: dur.unwrap_or_else(|| start.elapsed()).as_micros() as u64,
            })
            .collect();

        RequestTrace {
            request_id: self.request_id.to_string(),
            total_duration_us: total.as_micros() as u64,
            stages,
            cache_hit: self.cache_hit,
            upstream_name: self.upstream_name,
        }
    }
}

#[cfg(test)]
#[path = "tests/observability_tests.rs"]
mod tests;
