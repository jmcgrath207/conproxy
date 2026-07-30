//! Elasticsearch adapter for upstream full-text search queries.
//!
//! Implements the `UpstreamAdapter` trait for Elasticsearch,
//! translating between the proxy's query format and the Elasticsearch
//! `_search` API using `multi_match` query DSL.
//!
//! ## Score Normalization
//!
//! Elasticsearch uses BM25 scoring which produces unbounded scores
//! (typically 0-100+). This adapter normalizes scores to the 0-1 range
//! by dividing each hit's score by the `max_score` from the response,
//! so the top result always gets a normalized score of 1.0.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::types::{CacheStatus, QueryRequest, QueryResponse, SearchResult};
use super::upstream::{
    read_body_with_timeout, read_json_with_timeout, AdapterMetadata, QueryMode, UpstreamAdapter,
    UpstreamError,
};

/// Configuration for the Elasticsearch adapter.
#[derive(Debug, Clone)]
pub struct ElasticsearchConfig {
    /// Base URL for Elasticsearch (e.g., "http://localhost:9200").
    pub base_url: String,
    /// Index name or pattern to search (e.g., "docs", "docs-*").
    pub index: String,
    /// Request timeout.
    pub timeout: Duration,
    /// Fields to search with multi_match (default: ["content"]).
    pub search_fields: Vec<String>,
    /// Fields to return in `_source` (default: empty = all fields).
    pub return_fields: Vec<String>,
    /// API key for Elasticsearch Cloud authentication (optional).
    pub api_key: Option<String>,
    /// Minimum score threshold for results (optional).
    pub score_threshold: Option<f32>,
}

impl Default for ElasticsearchConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:9200".to_string(),
            index: "documents".to_string(),
            timeout: Duration::from_secs(30),
            search_fields: vec!["content".to_string()],
            return_fields: Vec::new(),
            api_key: None,
            score_threshold: None,
        }
    }
}

/// Adapter for Elasticsearch full-text search.
///
/// Sends `multi_match` queries to the Elasticsearch `_search` API
/// and normalizes BM25 scores to the 0-1 range for cascade compatibility.
pub struct ElasticsearchAdapter {
    client: reqwest::Client,
    config: ElasticsearchConfig,
    /// Cached query mode (always TextNative for Elasticsearch).
    query_mode: AtomicU8,
}

impl ElasticsearchAdapter {
    /// Create a new Elasticsearch adapter with the given configuration.
    pub fn new(config: ElasticsearchConfig) -> Result<Self, reqwest::Error> {
        let mut builder = super::socket_tuning::create_tuned_client_default(config.timeout);

        // Add API key header if configured (ES Cloud uses Authorization: ApiKey <key>)
        if let Some(ref api_key) = config.api_key {
            let mut headers = reqwest::header::HeaderMap::new();
            let auth_value = format!("ApiKey {}", api_key);
            if let Ok(value) = reqwest::header::HeaderValue::from_str(&auth_value) {
                headers.insert(reqwest::header::AUTHORIZATION, value);
                builder = builder.default_headers(headers);
            }
        }

        let client = builder.build()?;

        Ok(Self {
            client,
            config,
            // ES always handles text natively via BM25
            query_mode: AtomicU8::new(QueryMode::TextNative as u8),
        })
    }

    /// Create a simple Elasticsearch adapter with minimal configuration.
    pub fn simple(base_url: &str, index: &str, timeout: Duration) -> Result<Self, reqwest::Error> {
        Self::new(ElasticsearchConfig {
            base_url: base_url.to_string(),
            index: index.to_string(),
            timeout,
            ..Default::default()
        })
    }

    /// Get the search URL for the configured index.
    fn search_url(&self) -> String {
        format!(
            "{}/{}/_search",
            self.config.base_url.trim_end_matches('/'),
            self.config.index
        )
    }

    /// Get the cluster health URL.
    fn health_url(&self) -> String {
        format!(
            "{}/_cluster/health?timeout=5s",
            self.config.base_url.trim_end_matches('/')
        )
    }

    /// Build the Elasticsearch query body for a multi_match search.
    fn build_query_body(&self, query: &str, size: usize) -> serde_json::Value {
        let mut body = serde_json::json!({
            "query": {
                "multi_match": {
                    "query": query,
                    "fields": self.config.search_fields,
                    "type": "best_fields"
                }
            },
            "size": size
        });

        // Add _source filtering if return_fields is specified
        if !self.config.return_fields.is_empty() {
            if let Some(obj) = body.as_object_mut() {
                obj.insert(
                    "_source".to_string(),
                    serde_json::json!(self.config.return_fields),
                );
            }
        }

        // Add min_score if score_threshold is set
        if let Some(threshold) = self.config.score_threshold {
            if let Some(obj) = body.as_object_mut() {
                obj.insert("min_score".to_string(), serde_json::json!(threshold));
            }
        }

        body
    }

    /// Normalize a BM25 score to the 0-1 range using max_score.
    ///
    /// If `max_score` is zero or negative, returns 0.0 to avoid division by zero.
    fn normalize_score(score: f32, max_score: f32) -> f32 {
        if max_score <= 0.0 {
            0.0
        } else {
            (score / max_score).clamp(0.0, 1.0)
        }
    }

    /// Parse an Elasticsearch `_search` response into a list of `SearchResult`.
    fn parse_hits(response: &EsSearchResponse) -> Vec<SearchResult> {
        let max_score = response.hits.max_score.unwrap_or(0.0);

        response
            .hits
            .hits
            .iter()
            .map(|hit| {
                // Extract content from _source, trying common field names
                let content = hit
                    .source
                    .as_ref()
                    .and_then(|src| {
                        src.get("content")
                            .or_else(|| src.get("text"))
                            .or_else(|| src.get("body"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    })
                    .unwrap_or_default();

                // Build metadata from remaining _source fields
                let metadata = hit.source.as_ref().map(|src| {
                    let mut filtered = src.clone();
                    // Remove content fields from metadata to avoid duplication
                    if let Some(obj) = filtered.as_object_mut() {
                        obj.remove("content");
                        obj.remove("text");
                        obj.remove("body");
                    }
                    filtered
                });

                SearchResult {
                    id: hit.id.clone(),
                    score: Self::normalize_score(hit.score.unwrap_or(0.0), max_score),
                    content,
                    metadata,
                    upstream_id: None,
                }
            })
            .collect()
    }
}

/// Elasticsearch `_search` request body. Constructed for serialization, not read back.
#[derive(Debug, Serialize)]
#[allow(dead_code)]
struct EsSearchRequest {
    query: serde_json::Value,
    size: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    _source: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    min_score: Option<f32>,
}

/// Elasticsearch `_search` response.
#[derive(Debug, Deserialize)]
struct EsSearchResponse {
    hits: EsHitsContainer,
}

/// Container for Elasticsearch hits. Fields populated by serde deserialization.
#[derive(Debug, Deserialize)]
struct EsHitsContainer {
    #[allow(dead_code)] // Populated by serde deserialization
    total: EsHitsTotal,
    max_score: Option<f32>,
    hits: Vec<EsHit>,
}

/// Total hit count from Elasticsearch. Variants populated by serde deserialization.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
#[allow(dead_code)]
enum EsHitsTotal {
    /// Object format: { "value": N, "relation": "eq" }
    Object { value: u64 },
    /// Legacy integer format (ES < 7.0)
    Integer(u64),
}

/// A single hit from Elasticsearch search results.
#[derive(Debug, Deserialize)]
struct EsHit {
    #[serde(rename = "_id")]
    id: String,
    #[serde(rename = "_score")]
    score: Option<f32>,
    #[serde(rename = "_source")]
    source: Option<serde_json::Value>,
}

/// Elasticsearch cluster health response. Fields populated by serde deserialization.
#[derive(Debug, Deserialize)]
struct EsClusterHealthResponse {
    status: String,
    #[allow(dead_code)] // Populated by serde deserialization
    cluster_name: Option<String>,
}

#[async_trait]
impl UpstreamAdapter for ElasticsearchAdapter {
    async fn query(&self, request: &QueryRequest) -> Result<QueryResponse, UpstreamError> {
        let start = std::time::Instant::now();
        let size = request.top_k.unwrap_or(10);
        let query_body = self.build_query_body(&request.query, size);

        debug!(
            upstream = %self.config.base_url,
            index = %self.config.index,
            query_len = request.query.len(),
            "Sending multi_match query to Elasticsearch"
        );

        let response = self
            .client
            .post(self.search_url())
            .json(&query_body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    warn!("Elasticsearch request timed out");
                    UpstreamError::Timeout
                } else {
                    warn!(error = %e, "Elasticsearch network error");
                    UpstreamError::Network(e.to_string())
                }
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = read_body_with_timeout(response, Duration::from_secs(5))
                .await
                .unwrap_or_default();
            warn!(status = %status, "Elasticsearch returned error status");
            return Err(UpstreamError::Status(status.as_u16(), body));
        }

        let es_response: EsSearchResponse =
            read_json_with_timeout(response, self.config.timeout).await?;

        let results = Self::parse_hits(&es_response);

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
            "Checking Elasticsearch cluster health"
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

        let health: EsClusterHealthResponse =
            read_json_with_timeout(response, self.config.timeout).await?;

        // Green and yellow are considered healthy; red is not
        Ok(health.status == "green" || health.status == "yellow")
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
            "search_fields".to_string(),
            self.config.search_fields.join(", "),
        );

        AdapterMetadata {
            adapter_type: "elasticsearch".to_string(),
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
        // Elasticsearch always handles text natively via BM25
        Ok(QueryMode::TextNative)
    }
}

#[cfg(test)]
#[path = "tests/elasticsearch_tests.rs"]
mod tests;
