//! OpenAI embeddings API provider.
//!
//! Calls the OpenAI embeddings endpoint (`/v1/embeddings`).
//! Supports `text-embedding-3-small`, `text-embedding-3-large`, `text-embedding-ada-002`, etc.

use super::provider::EmbedderProvider;
use crate::error::{ConproxyError, Result};
use async_trait::async_trait;
use parking_lot::RwLock;
use serde::Deserialize;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// OpenAI embeddings provider.
pub struct OpenAiEmbedder {
    api_key: String,
    model: String,
    base_url: String,
    client: reqwest::Client,
    dimensions: RwLock<usize>,
}

impl OpenAiEmbedder {
    /// Create a new OpenAI embedder.
    ///
    /// `base_url` overrides the default `https://api.openai.com/v1` (useful for
    /// Azure OpenAI or compatible APIs). `timeout` bounds each HTTP request;
    /// a slow or hung provider fails fast instead of blocking the request path.
    pub fn new(
        api_key: String,
        model: String,
        base_url: Option<String>,
        timeout: Duration,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| ConproxyError::Embedding(format!("failed to build HTTP client: {e}")))?;
        Ok(Self {
            api_key,
            model,
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            client,
            dimensions: RwLock::new(0),
        })
    }

    fn endpoint(&self) -> String {
        format!("{}/embeddings", self.base_url)
    }
}

#[derive(Deserialize)]
struct OpenAiResponse {
    data: Vec<OpenAiEmbedding>,
}

#[derive(Deserialize)]
struct OpenAiEmbedding {
    /// OpenAI returns this so clients can reorder if the API returns
    /// out-of-order data (the spec permits this for batch requests).
    index: usize,
    embedding: Vec<f32>,
}

#[async_trait]
impl EmbedderProvider for OpenAiEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let resp = self
            .client
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({
                "model": self.model,
                "input": text,
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ConproxyError::Embedding(format!(
                "OpenAI API error {status}: {body}"
            )));
        }

        let parsed: OpenAiResponse = resp.json().await?;
        let embedding = parsed
            .data
            .into_iter()
            .next()
            .ok_or_else(|| ConproxyError::Embedding("OpenAI returned no embeddings".into()))?
            .embedding;

        let dim = embedding.len();
        let mut guard = self.dimensions.write();
        if *guard == 0 {
            *guard = dim;
        }

        Ok(embedding)
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let inputs: Vec<&str> = texts.to_vec();
        let resp = self
            .client
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({
                "model": self.model,
                "input": inputs,
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ConproxyError::Embedding(format!(
                "OpenAI API error {status}: {body}"
            )));
        }

        let parsed: OpenAiResponse = resp.json().await?;
        // Sort by the API-provided `index` to defend against out-of-order
        // batch responses (OpenAI spec allows it).
        let mut data = parsed.data;
        data.sort_by_key(|d| d.index);
        let embeddings: Vec<Vec<f32>> = data.into_iter().map(|d| d.embedding).collect();

        if embeddings.len() != texts.len() {
            return Err(ConproxyError::Embedding(format!(
                "OpenAI returned {} embeddings, expected {}",
                embeddings.len(),
                texts.len()
            )));
        }

        let dim = embeddings.first().map(|v| v.len()).unwrap_or(0);
        let mut guard = self.dimensions.write();
        if *guard == 0 {
            *guard = dim;
        }

        Ok(embeddings)
    }

    fn dimensions(&self) -> usize {
        *self.dimensions.read()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
mod tests {
    use super::*;

    #[test]
    fn test_new_with_default_base_url() {
        let embedder = OpenAiEmbedder::new(
            "test-key".to_string(),
            "text-embedding-3-small".to_string(),
            None,
            Duration::from_secs(5),
        )
        .expect("construct");
        // Default base URL = "https://api.openai.com/v1"
        assert_eq!(embedder.endpoint(), "https://api.openai.com/v1/embeddings");
    }

    #[test]
    fn test_new_with_custom_base_url() {
        let embedder = OpenAiEmbedder::new(
            "test-key".to_string(),
            "text-embedding-3-small".to_string(),
            Some("https://custom.example.com/openai/v1".to_string()),
            Duration::from_secs(5),
        )
        .expect("construct");
        // Custom base URL is used verbatim, /embeddings appended
        assert_eq!(
            embedder.endpoint(),
            "https://custom.example.com/openai/v1/embeddings"
        );
    }

    #[test]
    fn test_dimensions_starts_at_zero() {
        let embedder = OpenAiEmbedder::new(
            "test-key".to_string(),
            "text-embedding-3-small".to_string(),
            None,
            Duration::from_secs(5),
        )
        .expect("construct");
        // Before first embed(), dimensions is unknown (0)
        assert_eq!(embedder.dimensions(), 0);
    }

    #[test]
    fn test_dimensions_remains_zero_without_embed_call() {
        // embed() is what populates the dimension cache. Verifying the
        // initial state guards against accidentally initializing the
        // RwLock with a non-zero value (e.g., a stale model default).
        let embedder = OpenAiEmbedder::new(
            "test-key".to_string(),
            "text-embedding-3-small".to_string(),
            None,
            Duration::from_secs(5),
        )
        .expect("construct");
        assert_eq!(embedder.dimensions(), 0);
    }

    #[tokio::test]
    async fn test_embed_http_error_propagates() {
        // Use a non-routable address to force a connection error.
        // Port 1 on localhost typically refuses connection immediately.
        let embedder = OpenAiEmbedder::new(
            "test-key".to_string(),
            "text-embedding-3-small".to_string(),
            Some("http://127.0.0.1:1".to_string()),
            Duration::from_millis(500),
        )
        .expect("construct");
        let result = embedder.embed("hello").await;
        assert!(result.is_err(), "expected error for unreachable host");
    }

    #[tokio::test]
    async fn test_embed_mock_success_and_non_2xx() {
        use axum::extract::State;
        use axum::http::StatusCode;
        use axum::routing::post;
        use axum::{Json, Router};
        use std::net::SocketAddr;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use tokio::net::TcpListener;

        #[derive(Clone)]
        struct MockState {
            hits: Arc<AtomicUsize>,
            fail_once: Arc<AtomicUsize>,
        }

        async fn embeddings(
            State(st): State<MockState>,
            body: axum::body::Bytes,
        ) -> std::result::Result<Json<serde_json::Value>, StatusCode> {
            st.hits.fetch_add(1, Ordering::Relaxed);
            let _ = body;
            if st.fail_once.fetch_add(1, Ordering::Relaxed) == 0 {
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
            Ok(Json(serde_json::json!({
                "data": [{
                    "embedding": [0.1, 0.2, 0.3],
                    "index": 0,
                    "object": "embedding"
                }],
                "model": "text-embedding-3-small",
                "object": "list"
            })))
        }

        let st = MockState {
            hits: Arc::new(AtomicUsize::new(0)),
            fail_once: Arc::new(AtomicUsize::new(0)),
        };
        let app = Router::new()
            .route("/v1/embeddings", post(embeddings))
            .with_state(st.clone());
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });

        let base = format!("http://{addr}/v1");
        let embedder = OpenAiEmbedder::new(
            "test-key".to_string(),
            "text-embedding-3-small".to_string(),
            Some(base),
            Duration::from_secs(5),
        )
        .expect("construct");

        let err = embedder.embed("hello").await;
        assert!(err.is_err(), "first call should surface non-2xx");

        let vec = embedder.embed("hello").await.expect("second call ok");
        assert_eq!(vec, vec![0.1, 0.2, 0.3]);
        assert_eq!(embedder.dimensions(), 3);
        assert!(st.hits.load(Ordering::Relaxed) >= 2);
    }
}
