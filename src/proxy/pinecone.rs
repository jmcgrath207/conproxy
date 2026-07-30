//! Pinecone adapter (HTTP query API).
//!
//! VectorOnly by default — proxy embeds then calls `query_vector`.
//! Scores from Pinecone cosine are already ~0–1.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::types::{CacheStatus, QueryRequest, QueryResponse, SearchResult};
use super::upstream::{
    read_body_with_timeout, read_json_with_timeout, AdapterMetadata, QueryMode, UpstreamAdapter,
    UpstreamError,
};

/// Configuration for Pinecone HTTP adapter.
#[derive(Debug, Clone)]
pub struct PineconeConfig {
    /// Index host URL (e.g. `https://index-xxx.svc.pinecone.io`).
    pub base_url: String,
    /// API key (sent as `Api-Key` header).
    pub api_key: Option<String>,
    /// Optional namespace.
    pub namespace: Option<String>,
    /// Request timeout.
    pub timeout: Duration,
    /// Expected embedding dim; fail-fast on mismatch when set.
    pub dimensions: Option<usize>,
    /// Include metadata in response.
    pub include_metadata: bool,
    /// Score threshold (min similarity).
    pub score_threshold: Option<f32>,
}

impl Default for PineconeConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:8080".to_string(),
            api_key: None,
            namespace: None,
            timeout: Duration::from_secs(30),
            dimensions: None,
            include_metadata: true,
            score_threshold: None,
        }
    }
}

/// Adapter for Pinecone vector index.
pub struct PineconeAdapter {
    client: reqwest::Client,
    config: PineconeConfig,
    query_mode: AtomicU8,
}

impl PineconeAdapter {
    /// Create adapter from config.
    pub fn new(config: PineconeConfig) -> Result<Self, reqwest::Error> {
        let mut builder = super::socket_tuning::create_tuned_client_default(config.timeout);

        if let Some(ref api_key) = config.api_key {
            match reqwest::header::HeaderValue::from_str(api_key) {
                Ok(value) => {
                    let mut headers = reqwest::header::HeaderMap::new();
                    headers.insert("Api-Key", value);
                    builder = builder.default_headers(headers);
                }
                Err(_) => {
                    tracing::error!(
                        "Pinecone API key contains invalid header characters; \
                         requests will be sent without authentication"
                    );
                }
            }
        }

        let client = builder.build()?;
        Ok(Self {
            client,
            config,
            query_mode: AtomicU8::new(QueryMode::VectorOnly as u8),
        })
    }

    /// Minimal constructor.
    pub fn simple(base_url: &str, timeout: Duration) -> Result<Self, reqwest::Error> {
        Self::new(PineconeConfig {
            base_url: base_url.to_string(),
            timeout,
            ..Default::default()
        })
    }

    fn query_url(&self) -> String {
        format!("{}/query", self.config.base_url.trim_end_matches('/'))
    }

    fn describe_url(&self) -> String {
        format!(
            "{}/describe_index_stats",
            self.config.base_url.trim_end_matches('/')
        )
    }

    fn map_matches(matches: Vec<PineconeMatch>, threshold: Option<f32>) -> Vec<SearchResult> {
        matches
            .into_iter()
            .filter_map(|m| {
                let score = m.score.unwrap_or(0.0).clamp(0.0, 1.0);
                if threshold.is_some_and(|t| score < t) {
                    return None;
                }
                let content = m
                    .metadata
                    .as_ref()
                    .and_then(|meta| {
                        meta.get("content")
                            .or_else(|| meta.get("text"))
                            .or_else(|| meta.get("body"))
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    })
                    .unwrap_or_default();
                let metadata = m.metadata.map(|meta| {
                    meta.as_object()
                        .map(|o| {
                            o.iter()
                                .filter(|(k, _)| !["content", "text", "body"].contains(&k.as_str()))
                                .map(|(k, v)| (k.clone(), v.to_string()))
                                .collect()
                        })
                        .unwrap_or_default()
                });
                Some(SearchResult {
                    id: m.id.unwrap_or_else(|| "unknown".to_string()),
                    score,
                    content,
                    metadata,
                    upstream_id: None,
                })
            })
            .collect()
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PineconeQueryRequest {
    vector: Vec<f32>,
    top_k: usize,
    include_metadata: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    namespace: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PineconeQueryResponse {
    #[serde(default)]
    matches: Vec<PineconeMatch>,
}

#[derive(Debug, Deserialize)]
struct PineconeMatch {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    score: Option<f32>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}

#[async_trait]
impl UpstreamAdapter for PineconeAdapter {
    async fn query(&self, _request: &QueryRequest) -> Result<QueryResponse, UpstreamError> {
        Err(UpstreamError::UnsupportedQueryType(
            "pinecone requires vector queries (use query_vector)".to_string(),
        ))
    }

    async fn query_vector(
        &self,
        request: &QueryRequest,
        vector: &[f32],
    ) -> Result<QueryResponse, UpstreamError> {
        if let Some(dims) = self.config.dimensions {
            if vector.len() != dims {
                return Err(UpstreamError::Network(format!(
                    "pinecone query vector dimension mismatch: expected {dims}, got {}",
                    vector.len()
                )));
            }
        }

        let start = std::time::Instant::now();
        let body = PineconeQueryRequest {
            vector: vector.to_vec(),
            top_k: request.top_k.unwrap_or(10),
            include_metadata: self.config.include_metadata,
            namespace: self.config.namespace.clone(),
        };

        let response = self
            .client
            .post(self.query_url())
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    UpstreamError::Timeout
                } else {
                    UpstreamError::Network(e.to_string())
                }
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = read_body_with_timeout(response, Duration::from_secs(5))
                .await
                .unwrap_or_default();
            return Err(UpstreamError::Status(status.as_u16(), body));
        }

        let pc_resp: PineconeQueryResponse =
            read_json_with_timeout(response, self.config.timeout).await?;
        let results = Self::map_matches(pc_resp.matches, self.config.score_threshold);

        Ok(QueryResponse {
            results,
            cache_status: CacheStatus::Miss,
            took_ms: start.elapsed().as_millis() as u64,
            generated_at: Some(QueryResponse::current_time_ms()),
            miss_reason: None,
        })
    }

    async fn health_check(&self) -> Result<bool, UpstreamError> {
        // describe_index_stats is lightweight and auth-checked
        let response = self
            .client
            .post(self.describe_url())
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|e| UpstreamError::Network(e.to_string()))?;
        Ok(response.status().is_success())
    }

    fn identifier(&self) -> &str {
        &self.config.base_url
    }

    fn timeout(&self) -> Duration {
        self.config.timeout
    }

    fn metadata(&self) -> AdapterMetadata {
        let mut properties = std::collections::HashMap::new();
        if let Some(ref ns) = self.config.namespace {
            properties.insert("namespace".to_string(), ns.clone());
        }
        AdapterMetadata {
            adapter_type: "pinecone".to_string(),
            version: None,
            properties,
        }
    }

    fn query_mode(&self) -> QueryMode {
        match self.query_mode.load(Ordering::Relaxed) {
            1 => QueryMode::TextNative,
            2 => QueryMode::VectorOnly,
            _ => QueryMode::Unknown,
        }
    }

    fn set_query_mode(&self, mode: QueryMode) {
        self.query_mode.store(mode as u8, Ordering::Relaxed);
    }

    async fn discover_query_mode(&self) -> Result<QueryMode, UpstreamError> {
        Ok(QueryMode::VectorOnly)
    }
}

#[cfg(test)]
#[path = "tests/pinecone_tests.rs"]
mod tests;
