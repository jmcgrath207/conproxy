//! Milvus adapter (HTTP REST v2).
//!
//! VectorOnly by default — proxy embeds then calls `query_vector`.
//! Score: cosine distance → similarity `1 - distance`, clamped to `[0, 1]`.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::types::{CacheStatus, QueryRequest, QueryResponse, SearchResult};
use super::upstream::{
    read_body_with_timeout, read_json_with_timeout, AdapterMetadata, QueryMode, UpstreamAdapter,
    UpstreamError,
};

/// Configuration for Milvus REST adapter.
#[derive(Debug, Clone)]
pub struct MilvusConfig {
    /// Base URL (e.g. `http://localhost:9091`).
    pub base_url: String,
    /// Collection name.
    pub collection_name: String,
    /// Request timeout.
    pub timeout: Duration,
    /// Optional bearer / token auth.
    pub api_key: Option<String>,
    /// Vector field name (default `vector`).
    pub vector_field: String,
    /// Output fields to request (default `content`).
    pub output_fields: Vec<String>,
    /// Expected embedding dim; fail-fast on mismatch when set.
    pub dimensions: Option<usize>,
    /// Score threshold (min similarity after normalize).
    pub score_threshold: Option<f32>,
}

impl Default for MilvusConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:9091".to_string(),
            collection_name: "default".to_string(),
            timeout: Duration::from_secs(30),
            api_key: None,
            vector_field: "vector".to_string(),
            output_fields: vec!["content".to_string()],
            dimensions: None,
            score_threshold: None,
        }
    }
}

/// Adapter for Milvus vector database (REST).
pub struct MilvusAdapter {
    client: reqwest::Client,
    config: MilvusConfig,
    query_mode: AtomicU8,
}

impl MilvusAdapter {
    /// Create adapter from config.
    pub fn new(config: MilvusConfig) -> Result<Self, reqwest::Error> {
        let mut builder = super::socket_tuning::create_tuned_client_default(config.timeout);

        if let Some(ref api_key) = config.api_key {
            match reqwest::header::HeaderValue::from_str(api_key) {
                Ok(value) => {
                    let mut headers = reqwest::header::HeaderMap::new();
                    headers.insert(reqwest::header::AUTHORIZATION, value);
                    builder = builder.default_headers(headers);
                }
                Err(_) => {
                    tracing::error!(
                        "Milvus API key contains invalid header characters; \
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
    pub fn simple(
        base_url: &str,
        collection_name: &str,
        timeout: Duration,
    ) -> Result<Self, reqwest::Error> {
        Self::new(MilvusConfig {
            base_url: base_url.to_string(),
            collection_name: collection_name.to_string(),
            timeout,
            ..Default::default()
        })
    }

    fn search_url(&self) -> String {
        format!(
            "{}/v2/vectordb/entities/search",
            self.config.base_url.trim_end_matches('/')
        )
    }

    fn list_collections_url(&self) -> String {
        format!(
            "{}/v2/vectordb/collections/list",
            self.config.base_url.trim_end_matches('/')
        )
    }

    fn map_hits(hits: Vec<MilvusHit>, threshold: Option<f32>) -> Vec<SearchResult> {
        hits.into_iter()
            .filter_map(|hit| {
                let score = normalize_milvus_score(hit.distance);
                if threshold.is_some_and(|t| score < t) {
                    return None;
                }
                let content = hit
                    .content
                    .clone()
                    .or_else(|| hit.entity.as_ref().and_then(extract_content))
                    .unwrap_or_default();
                let id = hit
                    .id
                    .map(|v| match v {
                        serde_json::Value::String(s) => s,
                        serde_json::Value::Number(n) => n.to_string(),
                        other => other.to_string(),
                    })
                    .unwrap_or_else(|| "unknown".to_string());
                Some(SearchResult {
                    id,
                    score,
                    content,
                    metadata: hit.entity.and_then(|e| {
                        e.as_object().map(|o| {
                            o.iter()
                                .filter(|(k, _)| {
                                    !["content", "text", "body", "vector", "id"]
                                        .contains(&k.as_str())
                                })
                                .map(|(k, v)| (k.clone(), v.to_string()))
                                .collect()
                        })
                    }),
                    upstream_id: None,
                })
            })
            .collect()
    }
}

/// Cosine distance → similarity in `[0, 1]`.
fn normalize_milvus_score(distance: Option<f32>) -> f32 {
    let d = distance.unwrap_or(0.0);
    (1.0 - d).clamp(0.0, 1.0)
}

fn extract_content(entity: &serde_json::Value) -> Option<String> {
    entity
        .get("content")
        .or_else(|| entity.get("text"))
        .or_else(|| entity.get("body"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MilvusSearchRequest {
    collection_name: String,
    data: Vec<Vec<f32>>,
    limit: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    output_fields: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    anns_field: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MilvusSearchResponse {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    data: Vec<MilvusHit>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MilvusHit {
    #[serde(default)]
    id: Option<serde_json::Value>,
    #[serde(default)]
    distance: Option<f32>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    entity: Option<serde_json::Value>,
}

#[async_trait]
impl UpstreamAdapter for MilvusAdapter {
    async fn query(&self, _request: &QueryRequest) -> Result<QueryResponse, UpstreamError> {
        Err(UpstreamError::UnsupportedQueryType(
            "milvus requires vector queries (use query_vector)".to_string(),
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
                    "milvus query vector dimension mismatch: expected {dims}, got {}",
                    vector.len()
                )));
            }
        }

        let start = std::time::Instant::now();
        let body = MilvusSearchRequest {
            collection_name: self.config.collection_name.clone(),
            data: vec![vector.to_vec()],
            limit: request.top_k.unwrap_or(10),
            output_fields: self.config.output_fields.clone(),
            anns_field: Some(self.config.vector_field.clone()),
        };

        let response = self
            .client
            .post(self.search_url())
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

        let milvus_resp: MilvusSearchResponse =
            read_json_with_timeout(response, self.config.timeout).await?;

        if milvus_resp.code != 0 {
            return Err(UpstreamError::Network(format!(
                "milvus search error code {}: {}",
                milvus_resp.code,
                milvus_resp.message.unwrap_or_default()
            )));
        }

        let results = Self::map_hits(milvus_resp.data, self.config.score_threshold);

        Ok(QueryResponse {
            results,
            cache_status: CacheStatus::Miss,
            took_ms: start.elapsed().as_millis() as u64,
            generated_at: Some(QueryResponse::current_time_ms()),
            miss_reason: None,
        })
    }

    async fn health_check(&self) -> Result<bool, UpstreamError> {
        // REST API port has no /healthz (that's metrics :9091); list is lightweight.
        let response = self
            .client
            .post(self.list_collections_url())
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
        properties.insert(
            "collection".to_string(),
            self.config.collection_name.clone(),
        );
        AdapterMetadata {
            adapter_type: "milvus".to_string(),
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
#[path = "tests/milvus_tests.rs"]
mod tests;
