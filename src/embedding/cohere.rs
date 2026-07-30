//! Cohere embeddings API provider.
//!
//! Calls the Cohere embeddings endpoint (`/v1/embed`).
//! Supports `embed-english-v3.0`, `embed-multilingual-v3.0`, etc.

use super::provider::EmbedderProvider;
use crate::error::{ConproxyError, Result};
use async_trait::async_trait;
use parking_lot::RwLock;
use serde::Deserialize;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://api.cohere.com/v1";

/// Cohere embeddings provider.
pub struct CohereEmbedder {
    api_key: String,
    model: String,
    base_url: String,
    client: reqwest::Client,
    dimensions: RwLock<usize>,
}

impl CohereEmbedder {
    /// Create a new Cohere embedder.
    ///
    /// `timeout` bounds each HTTP request.
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
        format!("{}/embed", self.base_url)
    }
}

#[derive(Deserialize)]
struct CohereResponse {
    embeddings: Vec<Vec<f32>>,
}

#[async_trait]
impl EmbedderProvider for CohereEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let resp = self
            .client
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({
                "model": self.model,
                "texts": [text],
                "input_type": "search_query",
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ConproxyError::Embedding(format!(
                "Cohere API error {status}: {body}"
            )));
        }

        let parsed: CohereResponse = resp.json().await?;
        let embedding = parsed
            .embeddings
            .into_iter()
            .next()
            .ok_or_else(|| ConproxyError::Embedding("Cohere returned no embeddings".into()))?;

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
                "texts": inputs,
                "input_type": "search_document",
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ConproxyError::Embedding(format!(
                "Cohere API error {status}: {body}"
            )));
        }

        let parsed: CohereResponse = resp.json().await?;
        let embeddings = parsed.embeddings;

        if embeddings.len() != texts.len() {
            return Err(ConproxyError::Embedding(format!(
                "Cohere returned {} embeddings, expected {}",
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
        let embedder = CohereEmbedder::new(
            "test-key".to_string(),
            "embed-english-v3.0".to_string(),
            None,
            Duration::from_secs(5),
        )
        .expect("construct");
        // Default base URL = "https://api.cohere.com/v1"
        assert_eq!(embedder.endpoint(), "https://api.cohere.com/v1/embed");
    }

    #[test]
    fn test_new_with_custom_base_url() {
        let embedder = CohereEmbedder::new(
            "test-key".to_string(),
            "embed-english-v3.0".to_string(),
            Some("https://proxy.example.com/cohere/v1".to_string()),
            Duration::from_secs(5),
        )
        .expect("construct");
        assert_eq!(
            embedder.endpoint(),
            "https://proxy.example.com/cohere/v1/embed"
        );
    }

    #[test]
    fn test_dimensions_starts_at_zero() {
        let embedder = CohereEmbedder::new(
            "test-key".to_string(),
            "embed-english-v3.0".to_string(),
            None,
            Duration::from_secs(5),
        )
        .expect("construct");
        assert_eq!(embedder.dimensions(), 0);
    }

    #[tokio::test]
    async fn test_embed_http_error_propagates() {
        // Port 1 is reserved/refused → connection refused
        let embedder = CohereEmbedder::new(
            "test-key".to_string(),
            "embed-english-v3.0".to_string(),
            Some("http://127.0.0.1:1".to_string()),
            Duration::from_millis(500),
        )
        .expect("construct");
        let result = embedder.embed("hello").await;
        assert!(result.is_err(), "expected error for unreachable host");
    }
}
