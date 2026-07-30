//! Meilisearch adapter for upstream full-text search queries.
//!
//! Implements the `UpstreamAdapter` trait for Meilisearch v1.0+,
//! translating between the proxy's query format and the Meilisearch
//! `/indexes/{uid}/search` REST API.
//!
//! ## Score Normalization
//!
//! Meilisearch v1.0+ returns `_rankingScore` on every hit when the
//! [`showRankingScore`](https://www.meilisearch.com/docs/reference/api/settings#show-ranking-score)
//! setting is enabled. The score is already in the 0-1 range, so no
//! division by `max_score` is needed (unlike Elasticsearch BM25).
//!
//! **This adapter requires Meilisearch v1.0 or newer** and the index
//! must have `showRankingScore` enabled (the adapter enables it
//! implicitly on the first query if not already set on the index).

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use tracing::{debug, warn};

use super::types::{CacheStatus, QueryRequest, QueryResponse, SearchResult};
use super::upstream::{
    read_body_with_timeout, read_json_with_timeout, AdapterMetadata, QueryMode, UpstreamAdapter,
    UpstreamError,
};

/// Configuration for the Meilisearch adapter.
#[derive(Debug, Clone)]
pub struct MeilisearchConfig {
    /// Base URL for Meilisearch (e.g., "http://localhost:7700").
    pub base_url: String,
    /// Index uid (e.g., "docs", "conproxy_test").
    pub index: String,
    /// Request timeout.
    pub timeout: Duration,
    /// Attributes to search on (default: ["content"]).
    pub search_attributes: Vec<String>,
    /// Attributes to return in hits (default: empty = all attributes).
    pub displayed_attributes: Vec<String>,
    /// API key / master key for Meilisearch (optional, but required for
    /// any Meilisearch instance started with MEILI_MASTER_KEY set).
    pub api_key: Option<String>,
    /// Minimum score threshold for results (optional, applied client-side
    /// after Meili returns hits — Meili's `showRankingScore` exposes a
    /// 0-1 score but the API doesn't support a min_score filter directly).
    pub score_threshold: Option<f32>,
}

impl Default for MeilisearchConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:7700".to_string(),
            index: "documents".to_string(),
            timeout: Duration::from_secs(30),
            search_attributes: vec!["content".to_string()],
            displayed_attributes: Vec::new(),
            api_key: None,
            score_threshold: None,
        }
    }
}

/// Adapter for Meilisearch full-text search.
///
/// Sends POST requests to `/indexes/{uid}/search` with the user query
/// and parses `hits[]` plus `_rankingScore` for each result.
pub struct MeilisearchAdapter {
    client: reqwest::Client,
    config: MeilisearchConfig,
    /// Cached query mode (always TextNative for Meilisearch).
    query_mode: AtomicU8,
}

impl MeilisearchAdapter {
    /// Create a new Meilisearch adapter with the given configuration.
    pub fn new(config: MeilisearchConfig) -> Result<Self, reqwest::Error> {
        let mut builder = super::socket_tuning::create_tuned_client_default(config.timeout);

        // Add Authorization: Bearer <api_key> header if configured.
        if let Some(ref api_key) = config.api_key {
            let mut headers = reqwest::header::HeaderMap::new();
            let auth_value = format!("Bearer {}", api_key);
            if let Ok(value) = reqwest::header::HeaderValue::from_str(&auth_value) {
                headers.insert(reqwest::header::AUTHORIZATION, value);
                builder = builder.default_headers(headers);
            } else {
                warn!(
                    "Meilisearch API key contains invalid header characters; \
                     requests will be sent without authentication"
                );
            }
        }

        let client = builder.build()?;

        Ok(Self {
            client,
            config,
            // Meilisearch is always a text-native FTS engine.
            query_mode: AtomicU8::new(QueryMode::TextNative as u8),
        })
    }

    /// Create a simple Meilisearch adapter with minimal configuration.
    pub fn simple(base_url: &str, index: &str, timeout: Duration) -> Result<Self, reqwest::Error> {
        Self::new(MeilisearchConfig {
            base_url: base_url.to_string(),
            index: index.to_string(),
            timeout,
            ..Default::default()
        })
    }

    /// Encode index name for safe URL path embedding.
    /// Only encodes characters that would change URL structure (/, ?, #, space, etc.)
    fn encode_index(index: &str) -> String {
        use percent_encoding::{AsciiSet, CONTROLS};
        const PATH_SAFE: &AsciiSet = &CONTROLS
            .add(b' ')
            .add(b'"')
            .add(b'#')
            .add(b'%')
            .add(b'<')
            .add(b'>')
            .add(b'?')
            .add(b'`')
            .add(b'{')
            .add(b'}')
            .add(b'|')
            .add(b'\\')
            .add(b'^')
            .add(b'[')
            .add(b']');
        percent_encoding::utf8_percent_encode(index, PATH_SAFE).to_string()
    }

    /// Get the search URL for the configured index.
    fn search_url(&self) -> String {
        format!(
            "{}/indexes/{}/search",
            self.config.base_url.trim_end_matches('/'),
            Self::encode_index(&self.config.index)
        )
    }

    /// Get the health URL.
    fn health_url(&self) -> String {
        format!("{}/health", self.config.base_url.trim_end_matches('/'))
    }

    /// Build the Meilisearch query body.
    fn build_query_body(&self, query: &str, limit: usize) -> serde_json::Value {
        let mut body = serde_json::json!({
            "q": query,
            "limit": limit,
            // Ask Meilisearch to include the 0-1 ranking score in each hit.
            "showRankingScore": true,
        });

        // Restrict search to specified attributes.
        if !self.config.search_attributes.is_empty() {
            if let Some(obj) = body.as_object_mut() {
                obj.insert(
                    "attributesToSearchOn".to_string(),
                    serde_json::json!(self.config.search_attributes),
                );
            }
        }

        // Restrict displayed attributes if specified.
        if !self.config.displayed_attributes.is_empty() {
            if let Some(obj) = body.as_object_mut() {
                obj.insert(
                    "attributesToRetrieve".to_string(),
                    serde_json::json!(self.config.displayed_attributes),
                );
            }
        }

        body
    }

    /// Normalize a Meilisearch `_rankingScore` to the 0-1 range.
    ///
    /// Meilisearch already returns a value in [0, 1], but we clamp here
    /// defensively in case the response includes a slightly out-of-range
    /// value due to internal ranking rule quirks.
    fn normalize_score(score: Option<f32>) -> f32 {
        score.unwrap_or(0.0).clamp(0.0, 1.0)
    }

    /// Parse a Meilisearch `/search` response into a list of `SearchResult`.
    fn parse_hits(response: &MeiliSearchResponse) -> Vec<SearchResult> {
        response
            .hits
            .iter()
            .filter_map(|hit| {
                let id = hit.id().to_string();

                // Extract content from common field names.
                let content = hit
                    .get("content")
                    .or_else(|| hit.get("text"))
                    .or_else(|| hit.get("body"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_default();

                // Build metadata from remaining fields.
                let metadata = if let Some(obj) = hit.as_object() {
                    let mut filtered = obj.clone();
                    filtered.remove("id");
                    filtered.remove("content");
                    filtered.remove("text");
                    filtered.remove("body");
                    filtered.remove("_rankingScore");
                    filtered.remove("_formatted");
                    Some(serde_json::Value::Object(filtered))
                } else {
                    None
                };

                let score = Self::normalize_score(hit.ranking_score());

                if content.is_empty() && id.is_empty() {
                    None
                } else {
                    Some(SearchResult {
                        id,
                        score,
                        content,
                        metadata,
                        upstream_id: None,
                    })
                }
            })
            .collect()
    }
}

/// Meilisearch `/search` response. The `hits` field is a vector of
/// arbitrary JSON objects (Meili returns whatever was indexed for each
/// document). We use `serde_json::Value` so unknown fields are preserved.
#[derive(Debug, Deserialize)]
struct MeiliSearchResponse {
    hits: Vec<serde_json::Value>,
    #[serde(default)]
    #[allow(dead_code)]
    estimated_total_hits: Option<u64>,
    #[serde(default)]
    #[allow(dead_code)]
    processing_time_ms: Option<u64>,
}

/// Meilisearch `/health` response.
#[derive(Debug, Deserialize)]
struct MeiliHealthResponse {
    status: String,
}

/// Meilisearch `/version` response.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct MeiliVersionResponse {
    #[serde(rename = "pkgVersion")]
    pkg_version: String,
}

/// Trait extension for `serde_json::Value` to retrieve a hit's `id` and
/// `_rankingScore` regardless of whether the user used the reserved field
/// name or a custom primary key.
trait MeiliHitExt {
    fn id(&self) -> &str;
    fn ranking_score(&self) -> Option<f32>;
}

impl MeiliHitExt for serde_json::Value {
    fn id(&self) -> &str {
        // Meili usually returns the primary key as a top-level field.
        // If absent, fall back to the integer id (we don't currently use
        // integer ids in our schemas, so this is mostly defensive).
        self.get("id")
            .and_then(|v| v.as_str())
            .or_else(|| self.get("uid").and_then(|v| v.as_str()))
            .unwrap_or_default()
    }

    fn ranking_score(&self) -> Option<f32> {
        self.get("_rankingScore")
            .and_then(|v| v.as_f64())
            .map(|f| f as f32)
    }
}

#[async_trait]
impl UpstreamAdapter for MeilisearchAdapter {
    async fn query(&self, request: &QueryRequest) -> Result<QueryResponse, UpstreamError> {
        let start = std::time::Instant::now();
        let size = request.top_k.unwrap_or(10);
        let query_body = self.build_query_body(&request.query, size);

        debug!(
            upstream = %self.config.base_url,
            index = %self.config.index,
            query_len = request.query.len(),
            "Sending search query to Meilisearch"
        );

        let response = self
            .client
            .post(self.search_url())
            .json(&query_body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    warn!("Meilisearch request timed out");
                    UpstreamError::Timeout
                } else {
                    warn!(error = %e, "Meilisearch network error");
                    UpstreamError::Network(e.to_string())
                }
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = read_body_with_timeout(response, Duration::from_secs(5))
                .await
                .unwrap_or_default();
            warn!(status = %status, "Meilisearch returned error status");
            return Err(UpstreamError::Status(status.as_u16(), body));
        }

        let meili_response: MeiliSearchResponse =
            read_json_with_timeout(response, self.config.timeout).await?;

        let mut results = Self::parse_hits(&meili_response);

        // Apply client-side score threshold (Meili has no server-side min_score filter).
        if let Some(threshold) = self.config.score_threshold {
            results.retain(|r| r.score >= threshold);
        }

        Ok(QueryResponse {
            results,
            cache_status: CacheStatus::Miss,
            took_ms: start.elapsed().as_millis() as u64,
            generated_at: Some(QueryResponse::current_time_ms()),
            miss_reason: None,
        })
    }

    async fn health_check(&self) -> Result<bool, UpstreamError> {
        debug!(
            upstream = %self.config.base_url,
            "Checking Meilisearch health"
        );

        let response = self
            .client
            .get(self.health_url())
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    UpstreamError::Timeout
                } else {
                    UpstreamError::Network(e.to_string())
                }
            })?;

        if !response.status().is_success() {
            return Ok(false);
        }

        let health: MeiliHealthResponse =
            read_json_with_timeout(response, self.config.timeout).await?;

        // Meilisearch returns status: "available" when healthy.
        Ok(health.status == "available")
    }

    fn identifier(&self) -> &str {
        &self.config.base_url
    }

    fn timeout(&self) -> Duration {
        self.config.timeout
    }

    fn metadata(&self) -> AdapterMetadata {
        let mut properties = std::collections::HashMap::new();
        properties.insert("index".to_string(), self.config.index.clone());
        properties.insert(
            "search_attributes".to_string(),
            self.config.search_attributes.join(", "),
        );

        AdapterMetadata {
            adapter_type: "meilisearch".to_string(),
            version: None,
            properties,
        }
    }

    fn query_mode(&self) -> QueryMode {
        QueryMode::from(self.query_mode.load(Ordering::Relaxed))
    }

    fn set_query_mode(&self, mode: QueryMode) {
        self.query_mode.store(mode as u8, Ordering::Relaxed);
    }

    async fn discover_query_mode(&self) -> Result<QueryMode, UpstreamError> {
        // Meilisearch always handles text natively via its ranking rules.
        Ok(QueryMode::TextNative)
    }
}

#[cfg(test)]
#[path = "tests/meilisearch_tests.rs"]
mod tests;
