//! Generic REST adapter for upstream RAG services.
//!
//! This module provides a trait-based abstraction for upstream adapters,
//! allowing support for different vector database backends.
//!
//! ## Query Modes
//!
//! Upstreams can operate in different query modes:
//! - `TextNative`: Upstream handles text embedding internally (e.g., Qdrant FastEmbed, Weaviate vectorizer)
//! - `VectorOnly`: Upstream only accepts vectors - proxy must embed locally
//! - `Unknown`: Mode not yet determined - will probe on first request

use async_trait::async_trait;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::time::Duration;
use tracing::{debug, instrument, warn};

use super::retry::{RetryCondition, RetryableError};
use super::types::{CacheStatus, QueryRequest, QueryResponse, SearchResult};

/// Query mode supported by an upstream.
///
/// Determines how the proxy should send queries to the upstream:
/// - `TextNative`: Send text queries directly (upstream handles embedding)
/// - `VectorOnly`: Embed locally and send vectors
/// - `Unknown`: Mode not yet determined
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum QueryMode {
    /// Upstream handles text embedding internally.
    ///
    /// Examples: Qdrant with FastEmbed, Weaviate with vectorizer module.
    TextNative = 1,

    /// Upstream only accepts vectors - proxy must embed locally.
    ///
    /// Requires the `proxy-embed` feature for embedding support.
    VectorOnly = 2,

    /// Not yet determined - will probe on first request.
    #[default]
    Unknown = 0,
}

impl QueryMode {
    /// Check if this mode supports text queries directly.
    pub fn supports_text(&self) -> bool {
        matches!(self, Self::TextNative | Self::Unknown)
    }

    /// Check if this mode requires local embedding.
    pub fn requires_embedding(&self) -> bool {
        matches!(self, Self::VectorOnly)
    }
}

impl From<u8> for QueryMode {
    fn from(value: u8) -> Self {
        match value {
            1 => Self::TextNative,
            2 => Self::VectorOnly,
            _ => Self::Unknown,
        }
    }
}

/// Trait for upstream RAG/vector search adapters.
///
/// Implement this trait to add support for new upstream backends.
#[async_trait]
pub trait UpstreamAdapter: Send + Sync {
    /// Execute a query against the upstream service.
    async fn query(&self, request: &QueryRequest) -> Result<QueryResponse, UpstreamError>;

    /// Perform a health check on the upstream.
    ///
    /// Returns `Ok(true)` if healthy, `Ok(false)` if unhealthy but reachable,
    /// or `Err` if the upstream cannot be reached.
    async fn health_check(&self) -> Result<bool, UpstreamError>;

    /// Get the adapter's base URL or identifier.
    fn identifier(&self) -> &str;

    /// Get the adapter's timeout duration.
    fn timeout(&self) -> Duration;

    /// Get adapter-specific metadata (e.g., type, version).
    fn metadata(&self) -> AdapterMetadata {
        AdapterMetadata::default()
    }

    /// Get the current query mode for this upstream.
    ///
    /// Returns the detected or configured query mode. Default implementation
    /// returns `Unknown` to indicate capability detection is needed.
    fn query_mode(&self) -> QueryMode {
        QueryMode::Unknown
    }

    /// Set the query mode after capability detection.
    ///
    /// Called by the proxy after probing to cache the detected mode.
    /// Default implementation is a no-op (stateless adapters).
    fn set_query_mode(&self, _mode: QueryMode) {
        // Default: no-op for stateless adapters
    }

    /// Execute a vector query against the upstream.
    ///
    /// Only called when `query_mode()` is `VectorOnly`. The proxy will
    /// handle embedding the text query before calling this method.
    ///
    /// Default implementation returns `UnsupportedQueryType`.
    async fn query_vector(
        &self,
        _request: &QueryRequest,
        _vector: &[f32],
    ) -> Result<QueryResponse, UpstreamError> {
        Err(UpstreamError::UnsupportedQueryType(
            "Vector queries not implemented for this adapter".to_string(),
        ))
    }

    /// Probe the upstream to discover its query mode.
    ///
    /// Default implementation returns `Unknown`. Specialized adapters
    /// can override this to probe the upstream API.
    async fn discover_query_mode(&self) -> Result<QueryMode, UpstreamError> {
        Ok(QueryMode::Unknown)
    }
}

/// Metadata about an upstream adapter.
#[derive(Debug, Clone, Default)]
pub struct AdapterMetadata {
    /// Adapter type (e.g., "generic", "qdrant", "weaviate").
    pub adapter_type: String,
    /// Upstream service version (if known).
    pub version: Option<String>,
    /// Additional properties.
    pub properties: std::collections::HashMap<String, String>,
}

/// Upstream service health status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UpstreamStatus {
    /// Upstream is healthy and responding normally.
    #[default]
    Online,
    /// Upstream is responding but with elevated error rates or latency.
    Degraded,
    /// Upstream is not responding or returning errors.
    Offline,
}

/// Tracks upstream health metrics for status determination.
pub struct HealthTracker {
    /// Count of consecutive failures.
    consecutive_failures: AtomicU32,
    /// Count of consecutive successes (for recovery tracking).
    consecutive_successes: AtomicU32,
    /// Whether the upstream is currently marked offline (1 = offline, 0 = not offline).
    is_offline: AtomicU32,
    /// Total requests in the current window.
    total_requests: AtomicU32,
    /// Failed requests in the current window.
    failed_requests: AtomicU32,
    /// Timestamp of last successful request (as millis since Unix epoch).
    last_success_ms: AtomicU64,
    /// Timestamp of last failed request (as millis since Unix epoch).
    last_failure_ms: AtomicU64,
    /// Threshold for consecutive failures to mark as offline.
    offline_threshold: u32,
    /// Threshold for consecutive successes to recover from offline.
    recovery_threshold: u32,
    /// Error rate threshold to mark as degraded (0.0 to 1.0).
    degraded_threshold: f32,
}

impl HealthTracker {
    /// Create a new health tracker with default thresholds.
    pub fn new() -> Self {
        Self {
            consecutive_failures: AtomicU32::new(0),
            consecutive_successes: AtomicU32::new(0),
            is_offline: AtomicU32::new(0),
            total_requests: AtomicU32::new(0),
            failed_requests: AtomicU32::new(0),
            last_success_ms: AtomicU64::new(0),
            last_failure_ms: AtomicU64::new(0),
            offline_threshold: 3,
            recovery_threshold: 2,
            degraded_threshold: 0.1, // 10% error rate
        }
    }

    /// Create a health tracker with custom thresholds.
    pub fn with_thresholds(
        offline_threshold: u32,
        recovery_threshold: u32,
        degraded_threshold: f32,
    ) -> Self {
        Self {
            consecutive_failures: AtomicU32::new(0),
            consecutive_successes: AtomicU32::new(0),
            is_offline: AtomicU32::new(0),
            total_requests: AtomicU32::new(0),
            failed_requests: AtomicU32::new(0),
            last_success_ms: AtomicU64::new(0),
            last_failure_ms: AtomicU64::new(0),
            offline_threshold,
            recovery_threshold,
            degraded_threshold,
        }
    }

    /// Record a successful request.
    pub fn record_success(&self) {
        self.consecutive_failures.store(0, Ordering::SeqCst);
        let prev = self.consecutive_successes.fetch_add(1, Ordering::SeqCst);
        let successes = prev.saturating_add(1);
        self.total_requests.fetch_add(1, Ordering::SeqCst);

        // Check if we've recovered from offline
        if self.is_offline.load(Ordering::SeqCst) == 1 && successes >= self.recovery_threshold {
            self.is_offline.store(0, Ordering::SeqCst);
        }

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.last_success_ms.store(now_ms, Ordering::SeqCst);
    }

    /// Record a failed request.
    pub fn record_failure(&self) {
        self.consecutive_successes.store(0, Ordering::SeqCst);
        let prev = self.consecutive_failures.fetch_add(1, Ordering::SeqCst);
        let failures = prev.saturating_add(1);
        self.total_requests.fetch_add(1, Ordering::SeqCst);
        self.failed_requests.fetch_add(1, Ordering::SeqCst);

        // Check if we've hit the offline threshold
        if failures >= self.offline_threshold {
            self.is_offline.store(1, Ordering::SeqCst);
        }

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.last_failure_ms.store(now_ms, Ordering::SeqCst);
    }

    /// Get the current upstream status based on tracked metrics.
    pub fn status(&self) -> UpstreamStatus {
        let is_offline = self.is_offline.load(Ordering::SeqCst) == 1;
        let total = self.total_requests.load(Ordering::SeqCst);
        let failed = self.failed_requests.load(Ordering::SeqCst);
        let consecutive_failures = self.consecutive_failures.load(Ordering::SeqCst);

        // Check for offline state
        if is_offline {
            return UpstreamStatus::Offline;
        }

        // Check for degraded (error rate above threshold)
        // Only apply degraded check when we have enough samples (at least 10)
        // and we're not in a consecutive failure streak
        if total >= 10 && consecutive_failures == 0 {
            let error_rate = failed as f32 / total as f32;
            if error_rate > self.degraded_threshold {
                return UpstreamStatus::Degraded;
            }
        }

        UpstreamStatus::Online
    }

    /// Get the number of consecutive failures.
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures.load(Ordering::SeqCst)
    }

    /// Get the number of consecutive successes.
    pub fn consecutive_successes(&self) -> u32 {
        self.consecutive_successes.load(Ordering::SeqCst)
    }

    /// Reset the tracking window (call periodically to avoid unbounded counters).
    pub fn reset_window(&self) {
        self.total_requests.store(0, Ordering::SeqCst);
        self.failed_requests.store(0, Ordering::SeqCst);
    }

    /// Get time since last successful request.
    pub fn time_since_last_success(&self) -> Option<Duration> {
        let last_ms = self.last_success_ms.load(Ordering::SeqCst);
        if last_ms == 0 {
            return None;
        }

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        if now_ms > last_ms {
            Some(Duration::from_millis(now_ms.saturating_sub(last_ms)))
        } else {
            Some(Duration::ZERO)
        }
    }
}

impl Default for HealthTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Error types for upstream operations.
#[derive(Debug)]
pub enum UpstreamError {
    /// Network or connection error.
    Network(String),
    /// Upstream returned an error status.
    Status(u16, String),
    /// Failed to parse response.
    Parse(String),
    /// Request timed out.
    Timeout,
    /// Upstream is not configured.
    NotConfigured,
    /// No upstreams available (all offline or disabled).
    Unavailable(String),
    /// Query type not supported by upstream.
    ///
    /// Returned when a text query is sent to a VectorOnly upstream,
    /// or when capability detection fails.
    UnsupportedQueryType(String),
    /// Embedding required but not available.
    ///
    /// Returned when the upstream is VectorOnly but the proxy
    /// doesn't have embedding support enabled.
    EmbeddingRequired(String),
    /// Embedding operation failed.
    ///
    /// Returned when the proxy fails to embed a query for a VectorOnly upstream.
    EmbeddingFailed(String),
}

impl UpstreamError {
    /// Check if this error indicates text queries are not supported.
    ///
    /// Used for capability auto-detection during fallback.
    pub fn indicates_text_not_supported(&self) -> bool {
        match self {
            Self::UnsupportedQueryType(_) => true,
            Self::Status(code, msg) => {
                // Common error patterns for text-not-supported
                *code == 400
                    && (msg.contains("vector")
                        || msg.contains("embedding")
                        || msg.contains("dense")
                        || msg.contains("query must be"))
            }
            _ => false,
        }
    }
}

impl std::fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(msg) => write!(f, "Network error: {}", msg),
            Self::Status(code, msg) => write!(f, "Upstream error {}: {}", code, msg),
            Self::Parse(msg) => write!(f, "Parse error: {}", msg),
            Self::Timeout => write!(f, "Request timed out"),
            Self::NotConfigured => write!(f, "Upstream not configured"),
            Self::Unavailable(msg) => write!(f, "Upstream unavailable: {}", msg),
            Self::UnsupportedQueryType(msg) => write!(f, "Query type not supported: {}", msg),
            Self::EmbeddingRequired(msg) => {
                write!(f, "Embedding required but not available: {}", msg)
            }
            Self::EmbeddingFailed(msg) => write!(f, "Embedding failed: {}", msg),
        }
    }
}

impl std::error::Error for UpstreamError {}

impl RetryableError for UpstreamError {
    fn is_retryable(&self, condition: &RetryCondition) -> bool {
        match self {
            // Network errors are typically transient
            UpstreamError::Network(_) => condition.on_network_error,
            // Timeout errors are typically transient
            UpstreamError::Timeout => condition.on_timeout,
            // Status errors depend on the status code
            UpstreamError::Status(code, _) => condition.should_retry_status(*code),
            // Parse errors are deterministic, not retryable
            UpstreamError::Parse(_) => false,
            // Not configured is a config issue, not transient
            UpstreamError::NotConfigured => false,
            // Unavailable could be transient (all upstreams down)
            UpstreamError::Unavailable(_) => condition.on_server_error,
            // Query type issues are deterministic, not retryable
            UpstreamError::UnsupportedQueryType(_) => false,
            // Embedding required is a config/feature issue, not transient
            UpstreamError::EmbeddingRequired(_) => false,
            // Embedding failures could be transient (model loading, etc.)
            UpstreamError::EmbeddingFailed(_) => condition.on_server_error,
        }
    }
}

/// Read a response body as text with a timeout guard.
///
/// Protects against stalled upstreams that trickle bytes indefinitely
/// after the HTTP headers have already arrived.
pub(crate) async fn read_body_with_timeout(
    response: reqwest::Response,
    timeout: Duration,
) -> Result<String, UpstreamError> {
    tokio::time::timeout(timeout, response.text())
        .await
        .map_err(|_| UpstreamError::Timeout)?
        .map_err(|e| UpstreamError::Network(e.to_string()))
}

/// Read and deserialize a JSON response body with a timeout guard.
pub(crate) async fn read_json_with_timeout<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
    timeout: Duration,
) -> Result<T, UpstreamError> {
    tokio::time::timeout(timeout, response.json::<T>())
        .await
        .map_err(|_| UpstreamError::Timeout)?
        .map_err(|e| UpstreamError::Parse(e.to_string()))
}

/// Generic REST adapter for querying upstream RAG services.
pub struct GenericRestAdapter {
    client: reqwest::Client,
    base_url: String,
    query_path: String,
    health_path: String,
    timeout: Duration,
    /// Detected or configured query mode.
    query_mode: AtomicU8,
}

impl GenericRestAdapter {
    /// Create a new adapter with the given configuration.
    pub fn new(base_url: &str, timeout: Duration) -> Result<Self, reqwest::Error> {
        let client = super::socket_tuning::create_tuned_client_default(timeout).build()?;

        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            query_path: "/query".to_string(),
            health_path: "/health".to_string(),
            timeout,
            query_mode: AtomicU8::new(QueryMode::Unknown as u8),
        })
    }

    /// Create a new adapter with custom paths.
    pub fn with_paths(
        base_url: &str,
        query_path: &str,
        health_path: &str,
        timeout: Duration,
    ) -> Result<Self, reqwest::Error> {
        let client = super::socket_tuning::create_tuned_client_default(timeout).build()?;

        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            query_path: query_path.to_string(),
            health_path: health_path.to_string(),
            timeout,
            query_mode: AtomicU8::new(QueryMode::Unknown as u8),
        })
    }

    /// Create a new adapter with a specific query mode.
    pub fn with_query_mode(
        base_url: &str,
        timeout: Duration,
        mode: QueryMode,
    ) -> Result<Self, reqwest::Error> {
        let adapter = Self::new(base_url, timeout)?;
        adapter.query_mode.store(mode as u8, Ordering::SeqCst);
        Ok(adapter)
    }

    /// Get the base URL.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Get the timeout duration.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Execute a query against the upstream service.
    #[instrument(skip(self, request), fields(upstream = %self.base_url, query_len = request.query.len()))]
    pub async fn query(&self, request: &QueryRequest) -> Result<QueryResponse, UpstreamError> {
        let url = format!("{}{}", self.base_url, self.query_path);
        let start = std::time::Instant::now();
        debug!("Sending query to upstream");

        let response: reqwest::Response = self
            .client
            .post(&url)
            .json(request)
            .send()
            .await
            .map_err(|e: reqwest::Error| {
                if e.is_timeout() {
                    warn!("Upstream request timed out");
                    UpstreamError::Timeout
                } else {
                    warn!(error = %e, "Upstream network error");
                    UpstreamError::Network(e.to_string())
                }
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = read_body_with_timeout(response, Duration::from_secs(5))
                .await
                .unwrap_or_default();
            warn!(status = %status, "Upstream returned error status");
            return Err(UpstreamError::Status(status.as_u16(), body));
        }

        // Try to parse the response
        let body = read_body_with_timeout(response, self.timeout).await?;

        // Try parsing as our expected format first
        if let Ok(mut resp) = serde_json::from_str::<QueryResponse>(&body) {
            resp.cache_status = CacheStatus::Miss;
            resp.took_ms = start.elapsed().as_millis() as u64;
            return Ok(resp);
        }

        // Try parsing as a generic results array
        if let Ok(results) = serde_json::from_str::<Vec<SearchResult>>(&body) {
            return Ok(QueryResponse {
                results,
                cache_status: CacheStatus::Miss,
                took_ms: start.elapsed().as_millis() as u64,
                generated_at: Some(QueryResponse::current_time_ms()),
                miss_reason: None,
            });
        }

        // Try parsing as a wrapper object with a "results" field
        #[derive(serde::Deserialize)]
        struct Wrapper {
            results: Vec<SearchResult>,
        }
        if let Ok(wrapper) = serde_json::from_str::<Wrapper>(&body) {
            return Ok(QueryResponse {
                results: wrapper.results,
                cache_status: CacheStatus::Miss,
                took_ms: start.elapsed().as_millis() as u64,
                generated_at: Some(QueryResponse::current_time_ms()),
                miss_reason: None,
            });
        }

        Err(UpstreamError::Parse(format!(
            "Failed to parse upstream response: {}",
            if body.len() > 200 {
                &body[..200]
            } else {
                &body
            }
        )))
    }

    /// Check if the upstream service is healthy.
    pub async fn health_check(&self) -> Result<bool, UpstreamError> {
        let url = format!("{}{}", self.base_url, self.health_path);

        let response: reqwest::Response =
            self.client
                .get(&url)
                .send()
                .await
                .map_err(|e: reqwest::Error| {
                    if e.is_timeout() {
                        UpstreamError::Timeout
                    } else {
                        UpstreamError::Network(e.to_string())
                    }
                })?;

        Ok(response.status().is_success())
    }
}

#[async_trait]
impl UpstreamAdapter for GenericRestAdapter {
    async fn query(&self, request: &QueryRequest) -> Result<QueryResponse, UpstreamError> {
        // Delegate to the inherent method
        GenericRestAdapter::query(self, request).await
    }

    async fn health_check(&self) -> Result<bool, UpstreamError> {
        // Delegate to the inherent method
        GenericRestAdapter::health_check(self).await
    }

    fn identifier(&self) -> &str {
        &self.base_url
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }

    fn metadata(&self) -> AdapterMetadata {
        AdapterMetadata {
            adapter_type: "generic".to_string(),
            version: None,
            properties: std::collections::HashMap::new(),
        }
    }

    fn query_mode(&self) -> QueryMode {
        QueryMode::from(self.query_mode.load(Ordering::SeqCst))
    }

    fn set_query_mode(&self, mode: QueryMode) {
        self.query_mode.store(mode as u8, Ordering::SeqCst);
    }
}

#[cfg(test)]
#[path = "tests/upstream_tests.rs"]
mod tests;
