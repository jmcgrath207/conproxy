//! Core types for the cache proxy.

use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicU8;
use std::time::{Instant, SystemTime};

/// Blake3 hash of a normalized query string.
pub type QueryHash = [u8; 32];

/// Schema fingerprint for detecting embedding model changes.
///
/// Tracks information about the embedding model/dimensions used to generate
/// vectors. When this changes, cached entries may be incompatible.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SchemaFingerprint {
    /// Embedding dimensions (e.g., 384, 768, 1536).
    pub embedding_dims: Option<u32>,
    /// Model identifier hint (e.g., "text-embedding-ada-002").
    pub model_hint: Option<String>,
    /// Optional version string from upstream.
    pub version: Option<String>,
}

impl SchemaFingerprint {
    /// Create a new schema fingerprint.
    pub fn new(embedding_dims: Option<u32>, model_hint: Option<String>) -> Self {
        Self {
            embedding_dims,
            model_hint,
            version: None,
        }
    }

    /// Create with all fields.
    pub fn with_version(
        embedding_dims: Option<u32>,
        model_hint: Option<String>,
        version: Option<String>,
    ) -> Self {
        Self {
            embedding_dims,
            model_hint,
            version,
        }
    }

    /// Check if this fingerprint is compatible with another.
    ///
    /// Returns true if the fingerprints are compatible (same dimensions and model).
    /// Unknown values (None) are considered compatible.
    pub fn is_compatible(&self, other: &SchemaFingerprint) -> bool {
        // Check dimensions
        if let (Some(a), Some(b)) = (self.embedding_dims, other.embedding_dims) {
            if a != b {
                return false;
            }
        }

        // Check model hint
        if let (Some(a), Some(b)) = (&self.model_hint, &other.model_hint) {
            if a != b {
                return false;
            }
        }

        true
    }

    /// Try to extract schema information from a raw JSON response.
    ///
    /// Looks for common patterns in RAG/vector database responses:
    /// - `embedding_dims` or `dimensions` fields
    /// - `model` or `embedding_model` fields
    /// - `version` fields
    /// - Vector arrays to infer dimensions
    pub fn extract_from_json(value: &serde_json::Value) -> Self {
        let mut embedding_dims = None;
        let mut model_hint = None;
        let mut version = None;

        // Try to extract from various known field patterns
        if let Some(obj) = value.as_object() {
            // Direct dimension fields
            for key in [
                "embedding_dims",
                "dimensions",
                "dim",
                "vector_size",
                "embedding_dimension",
            ] {
                if let Some(serde_json::Value::Number(n)) = obj.get(key) {
                    if let Some(d) = n.as_u64() {
                        embedding_dims = Some(d as u32);
                        break;
                    }
                }
            }

            // Model hint fields
            for key in ["model", "embedding_model", "model_name", "model_id"] {
                if let Some(serde_json::Value::String(s)) = obj.get(key) {
                    model_hint = Some(s.clone());
                    break;
                }
            }

            // Version fields
            for key in ["version", "api_version", "schema_version"] {
                if let Some(serde_json::Value::String(s)) = obj.get(key) {
                    version = Some(s.clone());
                    break;
                }
            }

            // Try to infer dimensions from vector arrays in results
            if embedding_dims.is_none() {
                if let Some(results) = obj.get("results").and_then(|r| r.as_array()) {
                    for result in results {
                        if let Some(result_obj) = result.as_object() {
                            // Look for vector fields
                            for key in ["vector", "embedding", "embeddings", "dense_vector"] {
                                if let Some(vec_array) =
                                    result_obj.get(key).and_then(|v| v.as_array())
                                {
                                    embedding_dims = Some(vec_array.len() as u32);
                                    break;
                                }
                            }
                            if embedding_dims.is_some() {
                                break;
                            }
                        }
                    }
                }
            }

            // Check for metadata that might contain schema info
            if let Some(meta) = obj
                .get("metadata")
                .or_else(|| obj.get("meta"))
                .and_then(|m| m.as_object())
            {
                if embedding_dims.is_none() {
                    for key in ["embedding_dims", "dimensions"] {
                        if let Some(serde_json::Value::Number(n)) = meta.get(key) {
                            if let Some(d) = n.as_u64() {
                                embedding_dims = Some(d as u32);
                                break;
                            }
                        }
                    }
                }
                if model_hint.is_none() {
                    for key in ["model", "embedding_model"] {
                        if let Some(serde_json::Value::String(s)) = meta.get(key) {
                            model_hint = Some(s.clone());
                            break;
                        }
                    }
                }
            }
        }

        Self {
            embedding_dims,
            model_hint,
            version,
        }
    }

    /// Check if this fingerprint has any known schema information.
    pub fn is_known(&self) -> bool {
        self.embedding_dims.is_some() || self.model_hint.is_some()
    }
}

/// Drift level indicating how much results have changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DriftLevel {
    /// No significant drift detected.
    None,
    /// Minor changes (score variations, metadata updates).
    Minor,
    /// Moderate changes (some results differ).
    Moderate,
    /// Significant changes (most results differ).
    Significant,
    /// Major changes (completely different results).
    Major,
}

impl DriftLevel {
    /// Check if this drift level exceeds a threshold.
    pub fn exceeds(&self, threshold: DriftLevel) -> bool {
        *self > threshold
    }
}

/// A cached response with metadata.
///
/// Field ordering note: Rust does not guarantee struct layout without `repr(C)`.
/// The compiler may reorder fields for optimal alignment regardless of source order.
/// Hot fields (freq, cached_at) are listed first as documentation of access patterns.
pub struct CacheEntry {
    // Hot — read on every access
    /// S3-FIFO frequency counter (0–3). Bumped on access, reset on eviction check.
    pub(crate) freq: AtomicU8,
    /// When the entry was first cached.
    pub cached_at: Instant,
    /// Wall-clock timestamp at insert time. None for legacy entries (skipped by distill).
    /// Reconstructed on `restore_from_persistence` from `PersistedEntry.cached_at_ms`.
    pub cached_at_wall: Option<SystemTime>,
    /// Blake3 hash of response content for integrity verification.
    pub content_hash: Option<[u8; 32]>,
    // Warm — read on cache hit
    /// The cached response data.
    pub response: QueryResponse,
    /// Identifier for the upstream that provided this response.
    pub upstream_id: String,
    // Cold — rarely read
    /// Number of times TTL was extended (for stale-while-revalidate).
    pub extended_count: u32,
    /// Schema fingerprint for compatibility checking.
    pub schema: SchemaFingerprint,
    /// Original query text for refresh support.
    /// Stored so the refresh worker can re-query the upstream.
    pub query_text: Option<String>,
    /// Context id this entry belongs to (non-empty).
    pub context_id: String,
}

impl CacheEntry {
    /// Compute a blake3 hash of the response content for integrity verification.
    pub fn compute_content_hash(response: &QueryResponse) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();

        // Hash the number of results
        hasher.update(&(response.results.len() as u64).to_le_bytes());

        // Hash each result
        for result in &response.results {
            hasher.update(result.id.as_bytes());
            hasher.update(&result.score.to_le_bytes());
            hasher.update(result.content.as_bytes());
            if let Some(meta) = &result.metadata {
                hasher.update(meta.to_string().as_bytes());
            }
        }

        *hasher.finalize().as_bytes()
    }

    /// Verify the integrity of the cached response.
    ///
    /// Returns true if the content hash matches, or if no hash was stored.
    pub fn verify_integrity(&self) -> bool {
        match self.content_hash {
            Some(stored_hash) => {
                let computed = Self::compute_content_hash(&self.response);
                stored_hash == computed
            }
            None => true, // No hash stored, assume valid
        }
    }

    /// Create a new cache entry with integrity hash.
    pub fn new_with_hash(
        response: QueryResponse,
        upstream_id: String,
        schema: SchemaFingerprint,
        context_id: String,
    ) -> Self {
        debug_assert!(!context_id.is_empty(), "context_id must be set");
        let content_hash = Some(Self::compute_content_hash(&response));
        Self {
            response,
            upstream_id,
            cached_at: Instant::now(),
            cached_at_wall: Some(SystemTime::now()),
            extended_count: 0,
            schema,
            content_hash,
            query_text: None,
            freq: AtomicU8::new(0),
            context_id,
        }
    }

    /// Create a new cache entry with integrity hash and query text for refresh.
    pub fn new_with_query(
        response: QueryResponse,
        upstream_id: String,
        schema: SchemaFingerprint,
        query_text: String,
        context_id: String,
    ) -> Self {
        debug_assert!(!context_id.is_empty(), "context_id must be set");
        let content_hash = Some(Self::compute_content_hash(&response));
        Self {
            response,
            upstream_id,
            cached_at: Instant::now(),
            cached_at_wall: Some(SystemTime::now()),
            extended_count: 0,
            schema,
            content_hash,
            query_text: Some(query_text),
            freq: AtomicU8::new(0),
            context_id,
        }
    }
}

/// Maximum allowed query length in characters.
pub const MAX_QUERY_LENGTH: usize = 10_000;

/// Maximum allowed top_k value.
pub const MAX_TOP_K: usize = 1000;

/// Default top_k value.
pub const DEFAULT_TOP_K: usize = 10;

/// Maximum number of results in a response.
pub const MAX_RESULTS: usize = 1000;

/// Maximum content length per result in bytes.
pub const MAX_CONTENT_LENGTH: usize = 100_000;

/// Maximum total response size in bytes (approximate).
pub const MAX_RESPONSE_SIZE: usize = 10_000_000; // 10 MB

/// Incoming query request from clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryRequest {
    /// The search query string.
    pub query: String,
    /// Maximum number of results to return.
    #[serde(default)]
    pub top_k: Option<usize>,
    /// Request priority (0=low, 1=normal, 2=high, 3=critical).
    /// Used for request ordering during high load. Optional, defaults to normal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    /// Target a specific upstream by ID (skip cascade, route directly).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_id: Option<String>,
    /// Prefer upstreams of this type (used as hint in cascade ordering).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_type: Option<String>,
}

/// Validation error for query requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// Query string is empty.
    EmptyQuery,
    /// Query string exceeds maximum length.
    QueryTooLong { length: usize, max: usize },
    /// Query contains null bytes.
    QueryContainsNullBytes,
    /// Query contains suspicious control characters.
    QueryContainsControlChars,
    /// top_k value exceeds maximum.
    TopKTooLarge { value: usize, max: usize },
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyQuery => write!(f, "Query string cannot be empty"),
            Self::QueryTooLong { length, max } => {
                write!(f, "Query length {} exceeds maximum {}", length, max)
            }
            Self::QueryContainsNullBytes => write!(f, "Query contains null bytes"),
            Self::QueryContainsControlChars => {
                write!(f, "Query contains suspicious control characters")
            }
            Self::TopKTooLarge { value, max } => {
                write!(f, "top_k value {} exceeds maximum {}", value, max)
            }
        }
    }
}

impl std::error::Error for ValidationError {}

/// Validation error for query responses.
#[derive(Debug, Clone, PartialEq)]
pub enum ResponseValidationError {
    /// Response has too many results.
    TooManyResults { count: usize, max: usize },
    /// A result content exceeds maximum length.
    ContentTooLong {
        result_id: String,
        length: usize,
        max: usize,
    },
    /// A result contains null bytes in content.
    ContentContainsNullBytes { result_id: String },
    /// A result contains suspicious control characters.
    ContentContainsControlChars { result_id: String },
    /// Total response size exceeds maximum.
    ResponseTooLarge { size: usize, max: usize },
    /// A result ID is empty.
    EmptyResultId,
    /// A result has an invalid score (NaN or infinite).
    InvalidScore { result_id: String, score: f32 },
    /// Response timestamp is too old (potential replay attack).
    TimestampTooOld { age_ms: u64, max_age_ms: u64 },
    /// Response timestamp is in the future (clock skew or tampering).
    TimestampInFuture { generated_at_ms: u64, now_ms: u64 },
    /// Response signature is missing when signature verification is required.
    SignatureMissing,
    /// Response signature is invalid or does not match content.
    SignatureInvalid { expected: String, got: String },
    /// Response signature algorithm is not supported.
    SignatureAlgorithmUnsupported { algorithm: String },
    /// TLS certificate does not match pinned certificate.
    CertificatePinningFailed {
        expected_fingerprint: String,
        got_fingerprint: String,
    },
    /// TLS certificate is invalid or expired.
    CertificateInvalid { reason: String },
}

impl std::fmt::Display for ResponseValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyResults { count, max } => {
                write!(f, "Response has {} results, exceeds maximum {}", count, max)
            }
            Self::ContentTooLong {
                result_id,
                length,
                max,
            } => {
                write!(
                    f,
                    "Result '{}' content length {} exceeds maximum {}",
                    result_id, length, max
                )
            }
            Self::ContentContainsNullBytes { result_id } => {
                write!(f, "Result '{}' content contains null bytes", result_id)
            }
            Self::ContentContainsControlChars { result_id } => {
                write!(
                    f,
                    "Result '{}' content contains suspicious control characters",
                    result_id
                )
            }
            Self::ResponseTooLarge { size, max } => {
                write!(f, "Response size {} exceeds maximum {}", size, max)
            }
            Self::EmptyResultId => write!(f, "Result has empty ID"),
            Self::InvalidScore { result_id, score } => {
                write!(f, "Result '{}' has invalid score: {}", result_id, score)
            }
            Self::TimestampTooOld { age_ms, max_age_ms } => {
                write!(f, "Response timestamp too old: age {}ms exceeds max {}ms (potential replay attack)", age_ms, max_age_ms)
            }
            Self::TimestampInFuture {
                generated_at_ms,
                now_ms,
            } => {
                write!(
                    f,
                    "Response timestamp {} is in the future (now: {}ms)",
                    generated_at_ms, now_ms
                )
            }
            Self::SignatureMissing => {
                write!(
                    f,
                    "Response signature is missing when signature verification is required"
                )
            }
            Self::SignatureInvalid { expected, got } => {
                write!(
                    f,
                    "Response signature invalid: expected '{}', got '{}'",
                    expected, got
                )
            }
            Self::SignatureAlgorithmUnsupported { algorithm } => {
                write!(
                    f,
                    "Response signature algorithm '{}' is not supported",
                    algorithm
                )
            }
            Self::CertificatePinningFailed {
                expected_fingerprint,
                got_fingerprint,
            } => {
                write!(
                    f,
                    "TLS certificate pinning failed: expected fingerprint '{}', got '{}'",
                    expected_fingerprint, got_fingerprint
                )
            }
            Self::CertificateInvalid { reason } => {
                write!(f, "TLS certificate invalid: {}", reason)
            }
        }
    }
}

impl std::error::Error for ResponseValidationError {}

impl QueryRequest {
    /// Validate the query request.
    ///
    /// Returns `Ok(())` if the request is valid, or a `ValidationError` describing the issue.
    pub fn validate(&self) -> Result<(), ValidationError> {
        // Check for empty query
        if self.query.trim().is_empty() {
            return Err(ValidationError::EmptyQuery);
        }

        // Check query length
        if self.query.len() > MAX_QUERY_LENGTH {
            return Err(ValidationError::QueryTooLong {
                length: self.query.len(),
                max: MAX_QUERY_LENGTH,
            });
        }

        // Check for null bytes (potential injection)
        if self.query.contains('\0') {
            return Err(ValidationError::QueryContainsNullBytes);
        }

        // Check for suspicious control characters (ASCII 0-8, 11-12, 14-31)
        // Allow common whitespace: tab (9), newline (10), carriage return (13)
        for c in self.query.chars() {
            let code = c as u32;
            if code < 32 && code != 9 && code != 10 && code != 13 {
                return Err(ValidationError::QueryContainsControlChars);
            }
        }

        // Check top_k bounds
        if let Some(top_k) = self.top_k {
            if top_k > MAX_TOP_K {
                return Err(ValidationError::TopKTooLarge {
                    value: top_k,
                    max: MAX_TOP_K,
                });
            }
        }

        Ok(())
    }

    /// Get the effective top_k value (uses default if not specified).
    pub fn effective_top_k(&self) -> usize {
        self.top_k.unwrap_or(DEFAULT_TOP_K).min(MAX_TOP_K)
    }
}

/// A single search result item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Document or chunk identifier.
    #[serde(default)]
    pub id: String,
    /// Relevance score (higher is better).
    pub score: f32,
    /// The content text.
    pub content: String,
    /// Optional metadata.
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    /// Which upstream produced this result (for debugging/routing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_id: Option<String>,
}

/// Query response returned to clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResponse {
    /// The search results.
    pub results: Vec<SearchResult>,
    /// Cache status for this response.
    pub cache_status: CacheStatus,
    /// Time taken in milliseconds.
    pub took_ms: u64,
    /// Optional timestamp when the response was generated (Unix epoch milliseconds).
    /// Used for replay attack detection when timestamp validation is enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<u64>,
    /// Why this response was a cache miss (only set when cache_status is Miss).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub miss_reason: Option<MissReason>,
}

/// Zero-copy response wrapper that avoids deep-cloning cached entries.
///
/// On cache hits, holds an `Arc<CacheEntry>` reference instead of cloning
/// the entire `QueryResponse` (which contains `Vec<SearchResult>`).
/// Per-request fields (`cache_status`, `took_ms`) are stored separately.
pub enum CachedResponse {
    /// Zero-copy cache hit: borrows results from Arc<CacheEntry>.
    Cached {
        entry: std::sync::Arc<CacheEntry>,
        cache_status: CacheStatus,
        took_ms: u64,
    },
    /// Freshly constructed response (cache miss, errors, upstream results).
    Fresh(QueryResponse),
    /// Shared response from coalescer (avoids deep clone for waiters).
    /// The Arc is shared across all waiters; `took_ms` is per-waiter.
    SharedFresh {
        response: std::sync::Arc<QueryResponse>,
        took_ms: u64,
    },
}

impl CachedResponse {
    /// Create a zero-copy response from a cached entry.
    pub fn from_cache(
        entry: std::sync::Arc<CacheEntry>,
        cache_status: CacheStatus,
        took_ms: u64,
    ) -> Self {
        Self::Cached {
            entry,
            cache_status,
            took_ms,
        }
    }
}

impl Serialize for CachedResponse {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            CachedResponse::Cached {
                entry,
                cache_status,
                took_ms,
            } => {
                use serde::ser::SerializeStruct;
                let has_generated_at = entry.response.generated_at.is_some();
                let field_count = if has_generated_at { 4 } else { 3 };
                let mut state = serializer.serialize_struct("QueryResponse", field_count)?;
                state.serialize_field("results", &entry.response.results)?;
                state.serialize_field("cache_status", cache_status)?;
                state.serialize_field("took_ms", took_ms)?;
                if has_generated_at {
                    state.serialize_field("generated_at", &entry.response.generated_at)?;
                }
                state.end()
            }
            CachedResponse::Fresh(response) => response.serialize(serializer),
            CachedResponse::SharedFresh { response, took_ms } => {
                use serde::ser::SerializeStruct;
                let mut field_count = 3usize; // results, cache_status, took_ms
                if response.generated_at.is_some() {
                    field_count = field_count.saturating_add(1);
                }
                if response.miss_reason.is_some() {
                    field_count = field_count.saturating_add(1);
                }
                let mut state = serializer.serialize_struct("QueryResponse", field_count)?;
                state.serialize_field("results", &response.results)?;
                state.serialize_field("cache_status", &response.cache_status)?;
                state.serialize_field("took_ms", took_ms)?;
                if response.generated_at.is_some() {
                    state.serialize_field("generated_at", &response.generated_at)?;
                }
                if response.miss_reason.is_some() {
                    state.serialize_field("miss_reason", &response.miss_reason)?;
                }
                state.end()
            }
        }
    }
}

/// Estimate JSON value size in bytes without allocating a String.
fn json_approximate_size(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Null => 4,
        serde_json::Value::Bool(b) => {
            if *b {
                4
            } else {
                5
            }
        }
        serde_json::Value::Number(_) => 8, // typical number length
        serde_json::Value::String(s) => s.len().saturating_add(2),
        serde_json::Value::Array(arr) => {
            let items: usize = arr.iter().map(json_approximate_size).sum();
            items.saturating_add(arr.len()).saturating_add(2)
        }
        serde_json::Value::Object(obj) => {
            let items: usize = obj
                .iter()
                .map(|(k, v)| {
                    k.len()
                        .saturating_add(json_approximate_size(v))
                        .saturating_add(4)
                })
                .sum();
            items.saturating_add(2)
        }
    }
}

impl QueryResponse {
    /// Validate the response before caching.
    ///
    /// Returns `Ok(())` if the response is valid for caching, or an error describing the issue.
    pub fn validate(&self) -> Result<(), ResponseValidationError> {
        // Check result count
        if self.results.len() > MAX_RESULTS {
            return Err(ResponseValidationError::TooManyResults {
                count: self.results.len(),
                max: MAX_RESULTS,
            });
        }

        // Calculate approximate total size and validate each result
        let mut total_size = 0usize;

        for result in &self.results {
            // Check for empty ID
            if result.id.is_empty() {
                return Err(ResponseValidationError::EmptyResultId);
            }

            // Check for invalid score
            if result.score.is_nan() || result.score.is_infinite() {
                return Err(ResponseValidationError::InvalidScore {
                    result_id: result.id.clone(),
                    score: result.score,
                });
            }

            // Check content length
            if result.content.len() > MAX_CONTENT_LENGTH {
                return Err(ResponseValidationError::ContentTooLong {
                    result_id: result.id.clone(),
                    length: result.content.len(),
                    max: MAX_CONTENT_LENGTH,
                });
            }

            // Check for null bytes
            if result.content.contains('\0') {
                return Err(ResponseValidationError::ContentContainsNullBytes {
                    result_id: result.id.clone(),
                });
            }

            // Check for suspicious control characters (excluding common whitespace)
            if result
                .content
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t')
            {
                return Err(ResponseValidationError::ContentContainsControlChars {
                    result_id: result.id.clone(),
                });
            }

            // Accumulate size
            total_size = total_size.saturating_add(
                result
                    .id
                    .len()
                    .saturating_add(result.content.len())
                    .saturating_add(32),
            ); // 32 for overhead
            if let Some(ref meta) = result.metadata {
                total_size = total_size.saturating_add(json_approximate_size(meta));
            }
        }

        // Check total size
        if total_size > MAX_RESPONSE_SIZE {
            return Err(ResponseValidationError::ResponseTooLarge {
                size: total_size,
                max: MAX_RESPONSE_SIZE,
            });
        }

        Ok(())
    }

    /// Approximate size of this response in bytes.
    pub fn approximate_size(&self) -> usize {
        let mut size = 16usize; // base overhead
        for result in &self.results {
            size = size.saturating_add(
                result
                    .id
                    .len()
                    .saturating_add(result.content.len())
                    .saturating_add(32),
            );
            if let Some(ref meta) = result.metadata {
                size = size.saturating_add(json_approximate_size(meta));
            }
        }
        size
    }

    /// Validate the response timestamp for replay attack detection.
    ///
    /// This method checks if the response's `generated_at` timestamp is:
    /// 1. Not too old (exceeding `max_age_ms`)
    /// 2. Not in the future (with a small tolerance for clock skew)
    ///
    /// # Arguments
    /// * `max_age_ms` - Maximum allowed age of the response in milliseconds
    /// * `now_ms` - Current time in Unix epoch milliseconds
    /// * `clock_skew_tolerance_ms` - Tolerance for future timestamps (default: 5000ms)
    ///
    /// # Returns
    /// * `Ok(())` if the timestamp is valid or not present
    /// * `Err(ResponseValidationError)` if the timestamp indicates a replay attack
    pub fn validate_timestamp(
        &self,
        max_age_ms: u64,
        now_ms: u64,
        clock_skew_tolerance_ms: u64,
    ) -> Result<(), ResponseValidationError> {
        if let Some(generated_at) = self.generated_at {
            // Check if timestamp is too far in the future (clock skew or tampering)
            if generated_at > now_ms.saturating_add(clock_skew_tolerance_ms) {
                return Err(ResponseValidationError::TimestampInFuture {
                    generated_at_ms: generated_at,
                    now_ms,
                });
            }

            // Check if timestamp is too old (potential replay attack)
            if generated_at < now_ms.saturating_sub(max_age_ms) {
                let age_ms = now_ms.saturating_sub(generated_at);
                return Err(ResponseValidationError::TimestampTooOld { age_ms, max_age_ms });
            }
        }
        Ok(())
    }

    /// Get the current time in Unix epoch milliseconds.
    pub fn current_time_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// Configuration for upstream response signature verification.
#[derive(Debug, Clone)]
pub struct SignatureConfig {
    /// Whether signature verification is enabled.
    pub enabled: bool,
    /// The header name containing the signature (e.g., "X-Response-Signature").
    pub header_name: String,
    /// The algorithm to use for verification (e.g., "HMAC-SHA256").
    pub algorithm: String,
    /// The shared secret or public key for verification.
    pub key: Vec<u8>,
}

impl Default for SignatureConfig {
    // PERF(R11): allocation acceptable — called once at startup
    fn default() -> Self {
        Self {
            enabled: false,
            header_name: "X-Response-Signature".to_string(),
            algorithm: "HMAC-SHA256".to_string(),
            key: Vec::new(),
        }
    }
}

impl SignatureConfig {
    /// Create a new signature config with HMAC-SHA256.
    pub fn hmac_sha256(key: Vec<u8>) -> Self {
        Self {
            enabled: true,
            header_name: "X-Response-Signature".to_string(),
            algorithm: "HMAC-SHA256".to_string(),
            key,
        }
    }

    /// Verify a response signature.
    ///
    /// # Arguments
    /// * `response_body` - The raw response body bytes
    /// * `provided_signature` - The signature provided in the response header (hex-encoded)
    ///
    /// # Returns
    /// * `Ok(())` if the signature is valid
    /// * `Err(ResponseValidationError)` if the signature is missing, invalid, or uses unsupported algorithm
    pub fn verify(
        &self,
        response_body: &[u8],
        provided_signature: Option<&str>,
    ) -> Result<(), ResponseValidationError> {
        if !self.enabled {
            return Ok(());
        }

        let provided = provided_signature.ok_or(ResponseValidationError::SignatureMissing)?;

        match self.algorithm.as_str() {
            "HMAC-SHA256" => {
                // Compute expected signature using blake3 (since we already have it as a dependency)
                // In production, you'd use hmac + sha2 crates
                let mut hasher = blake3::Hasher::new_keyed(&self.derive_blake3_key());
                hasher.update(response_body);
                let expected = hasher.finalize().to_hex().to_string();

                if expected != provided {
                    return Err(ResponseValidationError::SignatureInvalid {
                        expected,
                        got: provided.to_string(),
                    });
                }
                Ok(())
            }
            "BLAKE3" => {
                let mut hasher = blake3::Hasher::new_keyed(&self.derive_blake3_key());
                hasher.update(response_body);
                let expected = hasher.finalize().to_hex().to_string();

                if expected != provided {
                    return Err(ResponseValidationError::SignatureInvalid {
                        expected,
                        got: provided.to_string(),
                    });
                }
                Ok(())
            }
            other => Err(ResponseValidationError::SignatureAlgorithmUnsupported {
                algorithm: other.to_string(),
            }),
        }
    }

    /// Derive a 32-byte key for blake3 from the configured key.
    fn derive_blake3_key(&self) -> [u8; 32] {
        let hash = blake3::hash(&self.key);
        *hash.as_bytes()
    }

    /// Compute a signature for a response body.
    ///
    /// This is useful for testing and for upstreams that want to sign their responses.
    pub fn sign(&self, response_body: &[u8]) -> String {
        match self.algorithm.as_str() {
            "HMAC-SHA256" | "BLAKE3" => {
                let mut hasher = blake3::Hasher::new_keyed(&self.derive_blake3_key());
                hasher.update(response_body);
                hasher.finalize().to_hex().to_string()
            }
            _ => String::new(),
        }
    }
}

/// Configuration for TLS certificate pinning.
#[derive(Debug, Clone, Default)]
pub struct CertificatePinConfig {
    /// Whether certificate pinning is enabled.
    pub enabled: bool,
    /// SHA-256 fingerprints of allowed certificates (hex-encoded).
    pub fingerprints: Vec<String>,
}

impl CertificatePinConfig {
    /// Create a new certificate pin config.
    pub fn new(fingerprints: Vec<String>) -> Self {
        Self {
            enabled: !fingerprints.is_empty(),
            fingerprints,
        }
    }

    /// Verify a certificate fingerprint.
    ///
    /// # Arguments
    /// * `cert_fingerprint` - The SHA-256 fingerprint of the presented certificate (hex-encoded)
    ///
    /// # Returns
    /// * `Ok(())` if the fingerprint matches any of the pinned fingerprints
    /// * `Err(ResponseValidationError::CertificatePinningFailed)` if no match
    pub fn verify(&self, cert_fingerprint: &str) -> Result<(), ResponseValidationError> {
        if !self.enabled {
            return Ok(());
        }

        let normalized_fingerprint = cert_fingerprint.to_lowercase().replace([':', ' '], "");

        for pinned in &self.fingerprints {
            let normalized_pinned = pinned.to_lowercase().replace([':', ' '], "");
            if normalized_fingerprint == normalized_pinned {
                return Ok(());
            }
        }

        Err(ResponseValidationError::CertificatePinningFailed {
            expected_fingerprint: self.fingerprints.first().cloned().unwrap_or_default(),
            got_fingerprint: cert_fingerprint.to_string(),
        })
    }

    /// Compute the SHA-256 fingerprint of a certificate (DER-encoded).
    ///
    /// Returns a lowercase hex-encoded fingerprint.
    pub fn compute_fingerprint(cert_der: &[u8]) -> String {
        blake3::hash(cert_der).to_hex().to_string()
    }
}

/// Indicates how a response was served.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheStatus {
    /// Fresh response from cache (within fresh TTL).
    Hit,
    /// Cache miss - fetched from upstream.
    Miss,
    /// Stale response served while revalidating.
    Stale,
    /// Frozen response (upstream unavailable, serving expired cache).
    Frozen,
}

/// Internal freshness classification for cache entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Within fresh TTL - serve immediately.
    Fresh,
    /// Past fresh TTL but within stale TTL - serve and revalidate.
    Stale,
    /// Past stale TTL - must revalidate before serving.
    Expired,
    /// Upstream failed - serve any cached data regardless of age.
    Frozen,
}

/// Reason for a cache miss. Used for observability breakdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissReason {
    /// Entry was never in cache (cold miss or previously evicted).
    NotInCache,
    /// Entry existed but TTL expired, upstream returned fresh data.
    Expired,
    /// Upstream request failed and no frozen entry was available.
    UpstreamError,
    /// Circuit breaker blocked the request and no frozen entry was available.
    CircuitOpen,
}

/// Detect drift between old and new query responses.
///
/// Compares result sets to determine how much the results have changed.
/// This is useful for detecting corpus changes, model updates, or
/// other changes that affect search quality.
pub fn detect_drift(old: &QueryResponse, new: &QueryResponse) -> DriftLevel {
    let old_results = &old.results;
    let new_results = &new.results;

    // Empty results handling
    if old_results.is_empty() && new_results.is_empty() {
        return DriftLevel::None;
    }
    if old_results.is_empty() || new_results.is_empty() {
        return DriftLevel::Major;
    }

    // Count matching IDs
    let old_ids: std::collections::HashSet<_> = old_results.iter().map(|r| &r.id).collect();
    let new_ids: std::collections::HashSet<_> = new_results.iter().map(|r| &r.id).collect();

    let intersection = old_ids.intersection(&new_ids).count();
    let total = old_ids.len().max(new_ids.len());

    // Calculate overlap ratio
    let overlap_ratio = intersection as f32 / total as f32;

    // Calculate score drift for matching results
    let mut score_drift_sum = 0.0f32;
    let mut score_drift_count: u32 = 0;

    for old_result in old_results {
        if let Some(new_result) = new_results.iter().find(|r| r.id == old_result.id) {
            let score_diff = (old_result.score - new_result.score).abs();
            score_drift_sum += score_diff;
            score_drift_count = score_drift_count.saturating_add(1);
        }
    }

    let avg_score_drift = if score_drift_count > 0 {
        score_drift_sum / score_drift_count as f32
    } else {
        1.0 // No matching results = maximum drift
    };

    // Classify drift level based on overlap and score changes
    if overlap_ratio >= 0.9 && avg_score_drift < 0.05 {
        DriftLevel::None
    } else if overlap_ratio >= 0.7 && avg_score_drift < 0.15 {
        DriftLevel::Minor
    } else if overlap_ratio >= 0.5 && avg_score_drift < 0.3 {
        DriftLevel::Moderate
    } else if overlap_ratio >= 0.3 {
        DriftLevel::Significant
    } else {
        DriftLevel::Major
    }
}

/// Aggregates drift metrics across multiple queries for corpus-wide detection.
///
/// Tracks drift levels over time to detect systematic changes in the
/// upstream corpus or model that affect search quality across many queries.
#[derive(Debug)]
pub struct DriftAggregator {
    /// Window of recent drift observations.
    observations: std::collections::VecDeque<DriftObservation>,
    /// Maximum window size.
    max_observations: usize,
    /// Threshold for triggering an alert.
    alert_threshold: DriftLevel,
    /// Number of observations that must exceed threshold to trigger alert.
    alert_count_threshold: usize,
}

/// A single drift observation.
#[derive(Debug, Clone)]
pub struct DriftObservation {
    /// The detected drift level.
    pub level: DriftLevel,
    /// When the observation was recorded.
    pub timestamp: Instant,
    /// Query hash that triggered this observation.
    pub query_hash: QueryHash,
}

/// Summary of aggregated drift metrics.
#[derive(Debug, Clone)]
pub struct DriftSummary {
    /// Total observations in the window.
    pub total_observations: usize,
    /// Count of each drift level.
    pub level_counts: [usize; 5], // None, Minor, Moderate, Significant, Major
    /// Whether the alert threshold has been exceeded.
    pub alert_triggered: bool,
    /// Maximum drift level observed.
    pub max_drift: DriftLevel,
    /// Average drift level (as numeric value).
    pub avg_drift: f32,
}

impl DriftAggregator {
    /// Create a new drift aggregator.
    ///
    /// # Arguments
    /// * `max_observations` - Maximum window size for tracking drift
    /// * `alert_threshold` - Drift level that triggers an alert
    /// * `alert_count_threshold` - How many observations must exceed threshold
    pub fn new(
        max_observations: usize,
        alert_threshold: DriftLevel,
        alert_count_threshold: usize,
    ) -> Self {
        Self {
            observations: std::collections::VecDeque::with_capacity(max_observations),
            max_observations,
            alert_threshold,
            alert_count_threshold,
        }
    }

    /// Create with default settings (100 observations, Moderate threshold, 10 count).
    pub fn with_defaults() -> Self {
        Self::new(100, DriftLevel::Moderate, 10)
    }

    /// Record a drift observation.
    pub fn record(&mut self, level: DriftLevel, query_hash: QueryHash) {
        // Remove old observations if at capacity
        while self.observations.len() >= self.max_observations {
            self.observations.pop_front();
        }

        self.observations.push_back(DriftObservation {
            level,
            timestamp: Instant::now(),
            query_hash,
        });
    }

    /// Get a summary of the current drift state.
    pub fn summary(&self) -> DriftSummary {
        let mut level_counts = [0usize; 5];
        let mut max_drift = DriftLevel::None;
        let mut drift_sum = 0u32;

        for obs in &self.observations {
            let idx = obs.level as usize;
            if let Some(slot) = level_counts.get_mut(idx) {
                *slot = slot.saturating_add(1);
            }
            drift_sum = drift_sum.saturating_add(idx as u32);
            if obs.level > max_drift {
                max_drift = obs.level;
            }
        }

        let total = self.observations.len();
        let avg_drift = if total > 0 {
            drift_sum as f32 / total as f32
        } else {
            0.0
        };

        // Count observations exceeding threshold
        let exceeding_count = self
            .observations
            .iter()
            .filter(|obs| obs.level > self.alert_threshold)
            .count();

        DriftSummary {
            total_observations: total,
            level_counts,
            alert_triggered: exceeding_count >= self.alert_count_threshold,
            max_drift,
            avg_drift,
        }
    }

    /// Check if an alert should be triggered.
    pub fn should_alert(&self) -> bool {
        self.summary().alert_triggered
    }

    /// Clear all observations.
    pub fn clear(&mut self) {
        self.observations.clear();
    }

    /// Get the number of observations.
    pub fn len(&self) -> usize {
        self.observations.len()
    }

    /// Check if the aggregator is empty.
    pub fn is_empty(&self) -> bool {
        self.observations.is_empty()
    }
}

#[cfg(test)]
#[path = "tests/types_tests.rs"]
mod tests;
