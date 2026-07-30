//! Qdrant-specific adapter for upstream queries.
//!
//! Implements the UpstreamAdapter trait for Qdrant vector database,
//! translating between the proxy's query format and Qdrant's API.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::types::{CacheStatus, QueryRequest, QueryResponse, SearchResult};
use super::upstream::{
    read_body_with_timeout, read_json_with_timeout, AdapterMetadata, QueryMode, UpstreamAdapter,
    UpstreamError,
};

/// Configuration for Qdrant adapter.
#[derive(Debug, Clone)]
pub struct QdrantConfig {
    /// Base URL for Qdrant (e.g., "http://localhost:6333").
    pub base_url: String,
    /// Collection name to search.
    pub collection_name: String,
    /// Request timeout.
    pub timeout: Duration,
    /// Whether to include payload in results.
    pub with_payload: bool,
    /// Whether to include vectors in results.
    pub with_vectors: bool,
    /// Score threshold (minimum similarity score).
    pub score_threshold: Option<f32>,
    /// API key for authentication (optional).
    pub api_key: Option<String>,
}

impl Default for QdrantConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:6333".to_string(),
            collection_name: "default".to_string(),
            timeout: Duration::from_secs(30),
            with_payload: true,
            with_vectors: false,
            score_threshold: None,
            api_key: None,
        }
    }
}

/// Adapter for Qdrant vector database.
pub struct QdrantAdapter {
    client: reqwest::Client,
    config: QdrantConfig,
    /// Cached query mode (0=Unknown, 1=TextNative, 2=VectorOnly).
    query_mode: AtomicU8,
}

impl QdrantAdapter {
    /// Create a new Qdrant adapter with the given configuration.
    pub fn new(config: QdrantConfig) -> Result<Self, reqwest::Error> {
        let mut builder = super::socket_tuning::create_tuned_client_default(config.timeout);

        // Add API key header if configured
        if let Some(ref api_key) = config.api_key {
            match reqwest::header::HeaderValue::from_str(api_key) {
                Ok(value) => {
                    let mut headers = reqwest::header::HeaderMap::new();
                    headers.insert("api-key", value);
                    builder = builder.default_headers(headers);
                }
                Err(_) => {
                    tracing::error!(
                        "Qdrant API key contains invalid header characters; \
                         requests will be sent without authentication"
                    );
                }
            }
        }

        let client = builder.build()?;

        Ok(Self {
            client,
            config,
            query_mode: AtomicU8::new(QueryMode::Unknown as u8),
        })
    }

    /// Create a simple Qdrant adapter with minimal configuration.
    pub fn simple(
        base_url: &str,
        collection_name: &str,
        timeout: Duration,
    ) -> Result<Self, reqwest::Error> {
        Self::new(QdrantConfig {
            base_url: base_url.to_string(),
            collection_name: collection_name.to_string(),
            timeout,
            ..Default::default()
        })
    }

    /// Get the search URL for the configured collection.
    fn search_url(&self) -> String {
        format!(
            "{}/collections/{}/points/search",
            self.config.base_url.trim_end_matches('/'),
            self.config.collection_name
        )
    }

    /// Get the health URL.
    fn health_url(&self) -> String {
        format!("{}/", self.config.base_url.trim_end_matches('/'))
    }

    /// Get the collection info URL.
    fn collection_url(&self) -> String {
        format!(
            "{}/collections/{}",
            self.config.base_url.trim_end_matches('/'),
            self.config.collection_name
        )
    }

    /// Get the query URL for text-based queries (FastEmbed).
    fn query_url(&self) -> String {
        format!(
            "{}/collections/{}/points/query",
            self.config.base_url.trim_end_matches('/'),
            self.config.collection_name
        )
    }
}

/// Qdrant search request body.
#[derive(Debug, Serialize)]
struct QdrantSearchRequest {
    /// The query vector (if using vector search).
    #[serde(skip_serializing_if = "Option::is_none")]
    vector: Option<Vec<f32>>,
    /// Text query (if using hybrid/text search).
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<String>,
    /// Number of results to return.
    limit: usize,
    /// Whether to include payload.
    with_payload: bool,
    /// Whether to include vectors.
    with_vector: bool,
    /// Score threshold.
    #[serde(skip_serializing_if = "Option::is_none")]
    score_threshold: Option<f32>,
}

/// Qdrant search response. Fields populated by serde deserialization.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct QdrantSearchResponse {
    result: Vec<QdrantScoredPoint>,
    time: Option<f64>,
}

/// A scored point from Qdrant search results. Fields populated by serde deserialization.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct QdrantScoredPoint {
    id: QdrantPointId,
    score: f32,
    #[serde(default)]
    payload: Option<serde_json::Value>,
    #[serde(default)]
    vector: Option<serde_json::Value>,
}

/// Qdrant point ID (can be UUID or integer).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum QdrantPointId {
    Uuid(String),
    Integer(u64),
}

impl std::fmt::Display for QdrantPointId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QdrantPointId::Uuid(s) => write!(f, "{}", s),
            QdrantPointId::Integer(n) => write!(f, "{}", n),
        }
    }
}

/// Qdrant health/root response. Fields populated by serde deserialization.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct QdrantHealthResponse {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    version: Option<String>,
}

/// Qdrant collection info response.
#[derive(Debug, Deserialize)]
struct QdrantCollectionResponse {
    result: QdrantCollectionInfo,
}

/// Collection info from Qdrant. Fields populated by serde deserialization.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct QdrantCollectionInfo {
    status: String,
    #[serde(default)]
    points_count: Option<u64>,
    #[serde(default)]
    vectors_count: Option<u64>,
}

#[async_trait]
impl UpstreamAdapter for QdrantAdapter {
    async fn query(&self, request: &QueryRequest) -> Result<QueryResponse, UpstreamError> {
        let start = std::time::Instant::now();

        // Build the Qdrant search request
        // Note: This uses text query; for vector search, you'd need an embedding
        let qdrant_request = QdrantSearchRequest {
            vector: None, // Would need embedding model to populate this
            query: Some(request.query.clone()),
            limit: request.top_k.unwrap_or(10),
            with_payload: self.config.with_payload,
            with_vector: self.config.with_vectors,
            score_threshold: self.config.score_threshold,
        };

        let response = self
            .client
            .post(self.search_url())
            .json(&qdrant_request)
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

        let qdrant_response: QdrantSearchResponse =
            read_json_with_timeout(response, self.config.timeout).await?;

        // Convert Qdrant results to our SearchResult format
        let results: Vec<SearchResult> = qdrant_response
            .result
            .into_iter()
            .map(|point| {
                // Extract content from payload
                let content = point
                    .payload
                    .as_ref()
                    .and_then(|p| {
                        // Try common content field names
                        p.get("content")
                            .or_else(|| p.get("text"))
                            .or_else(|| p.get("body"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    })
                    .unwrap_or_default();

                SearchResult {
                    id: point.id.to_string(),
                    score: point.score,
                    content,
                    metadata: point.payload.map(|p| {
                        p.as_object()
                            .map(|o| {
                                o.iter()
                                    .filter(|(k, _)| {
                                        !["content", "text", "body"].contains(&k.as_str())
                                    })
                                    .map(|(k, v)| (k.clone(), v.to_string()))
                                    .collect()
                            })
                            .unwrap_or_default()
                    }),
                    upstream_id: None,
                }
            })
            .collect();

        Ok(QueryResponse {
            results,
            cache_status: CacheStatus::Miss,
            took_ms: start.elapsed().as_millis() as u64,
            generated_at: Some(QueryResponse::current_time_ms()),
            miss_reason: None,
        })
    }

    async fn health_check(&self) -> Result<bool, UpstreamError> {
        // First check if Qdrant is reachable
        let response = self
            .client
            .get(self.health_url())
            .send()
            .await
            .map_err(|e| UpstreamError::Network(e.to_string()))?;

        if !response.status().is_success() {
            return Ok(false);
        }

        // Then check if the collection exists and is green
        let collection_response = self
            .client
            .get(self.collection_url())
            .send()
            .await
            .map_err(|e| UpstreamError::Network(e.to_string()))?;

        if !collection_response.status().is_success() {
            // Collection might not exist
            return Ok(false);
        }

        let collection_info: QdrantCollectionResponse =
            read_json_with_timeout(collection_response, self.config.timeout).await?;

        // Green status means healthy
        Ok(collection_info.result.status == "green")
    }

    fn identifier(&self) -> &str {
        &self.config.base_url
    }

    fn timeout(&self) -> Duration {
        self.config.timeout
    }

    fn metadata(&self) -> AdapterMetadata {
        let mut properties = std::collections::HashMap::new();
        properties.insert(
            "collection".to_string(),
            self.config.collection_name.clone(),
        );

        AdapterMetadata {
            adapter_type: "qdrant".to_string(),
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

    async fn query_vector(
        &self,
        request: &QueryRequest,
        vector: &[f32],
    ) -> Result<QueryResponse, UpstreamError> {
        let start = std::time::Instant::now();

        // Build vector search request
        let qdrant_request = QdrantSearchRequest {
            vector: Some(vector.to_vec()),
            query: None,
            limit: request.top_k.unwrap_or(10),
            with_payload: self.config.with_payload,
            with_vector: self.config.with_vectors,
            score_threshold: self.config.score_threshold,
        };

        let response = self
            .client
            .post(self.search_url())
            .json(&qdrant_request)
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

        let qdrant_response: QdrantSearchResponse =
            read_json_with_timeout(response, self.config.timeout).await?;

        // Convert results (same as query())
        let results: Vec<SearchResult> = qdrant_response
            .result
            .into_iter()
            .map(|point| {
                let content = point
                    .payload
                    .as_ref()
                    .and_then(|p| {
                        p.get("content")
                            .or_else(|| p.get("text"))
                            .or_else(|| p.get("body"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    })
                    .unwrap_or_default();

                SearchResult {
                    id: point.id.to_string(),
                    score: point.score,
                    content,
                    metadata: point.payload.map(|p| {
                        p.as_object()
                            .map(|o| {
                                o.iter()
                                    .filter(|(k, _)| {
                                        !["content", "text", "body"].contains(&k.as_str())
                                    })
                                    .map(|(k, v)| (k.clone(), v.to_string()))
                                    .collect()
                            })
                            .unwrap_or_default()
                    }),
                    upstream_id: None,
                }
            })
            .collect();

        Ok(QueryResponse {
            results,
            cache_status: CacheStatus::Miss,
            took_ms: start.elapsed().as_millis() as u64,
            generated_at: Some(QueryResponse::current_time_ms()),
            miss_reason: None,
        })
    }

    async fn discover_query_mode(&self) -> Result<QueryMode, UpstreamError> {
        // Try a text query to see if Qdrant supports it (FastEmbed/text embedding)
        // Qdrant's /query endpoint supports text queries when configured with FastEmbed
        let probe_request = serde_json::json!({
            "query": "test",
            "limit": 1,
            "with_payload": false
        });

        let response = self
            .client
            .post(self.query_url())
            .json(&probe_request)
            .send()
            .await
            .map_err(|e| UpstreamError::Network(e.to_string()))?;

        if response.status().is_success() {
            // Text query worked - upstream supports text natively
            Ok(QueryMode::TextNative)
        } else {
            let status = response.status().as_u16();
            let body = read_body_with_timeout(response, Duration::from_secs(5))
                .await
                .unwrap_or_default();

            // Check for specific error indicating text not supported
            if status == 400 || body.contains("vector") || body.contains("embedding") {
                // Upstream requires vectors
                Ok(QueryMode::VectorOnly)
            } else if status == 404 {
                // Collection might not exist or endpoint not available
                // Fall back to checking search endpoint
                Ok(QueryMode::VectorOnly)
            } else {
                // Unknown error, stay in Unknown mode
                Ok(QueryMode::Unknown)
            }
        }
    }
}

#[cfg(test)]
#[path = "tests/qdrant_tests.rs"]
mod tests;
