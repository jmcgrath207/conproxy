//! HuggingFace inference API provider.
//!
//! Calls the HuggingFace inference endpoint (`/models/{model}`).
//! Supports any sentence-transformer or embedding model hosted on HuggingFace.

use super::provider::EmbedderProvider;
use crate::error::{ConproxyError, Result};
use async_trait::async_trait;
use parking_lot::RwLock;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://api-inference.huggingface.com/models";

/// HuggingFace embeddings provider.
pub struct HuggingFaceEmbedder {
    api_key: String,
    model: String,
    base_url: String,
    client: reqwest::Client,
    dimensions: RwLock<usize>,
}

impl HuggingFaceEmbedder {
    /// Create a new HuggingFace embedder.
    ///
    /// `base_url` overrides the default `https://api-inference.huggingface.com/models`.
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
        format!("{}/{}", self.base_url, self.model)
    }
}

#[async_trait]
impl EmbedderProvider for HuggingFaceEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let resp = self
            .client
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({
                "inputs": text,
                "options": {"wait_for_model": true},
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ConproxyError::Embedding(format!(
                "HuggingFace API error {status}: {body}"
            )));
        }

        // HuggingFace returns [[f32, ...]] for single-text embedding
        let parsed: Vec<Vec<f32>> = resp.json().await?;
        let embedding = parsed
            .into_iter()
            .next()
            .ok_or_else(|| ConproxyError::Embedding("HuggingFace returned no embeddings".into()))?;

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

        // HuggingFace batch: send array of strings as inputs
        let inputs: Vec<&str> = texts.to_vec();
        let resp = self
            .client
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({
                "inputs": inputs,
                "options": {"wait_for_model": true},
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ConproxyError::Embedding(format!(
                "HuggingFace API error {status}: {body}"
            )));
        }

        // HuggingFace returns [[f32, ...], [f32, ...], ...] for batch
        let embeddings: Vec<Vec<f32>> = resp.json().await?;

        if embeddings.len() != texts.len() {
            return Err(ConproxyError::Embedding(format!(
                "HuggingFace returned {} embeddings, expected {}",
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
        let embedder = HuggingFaceEmbedder::new(
            "test-key".to_string(),
            "sentence-transformers/all-MiniLM-L6-v2".to_string(),
            None,
            Duration::from_secs(5),
        )
        .expect("construct");
        // Default base URL = "https://api-inference.huggingface.com/models"
        // endpoint() appends /<model>
        assert_eq!(
            embedder.endpoint(),
            "https://api-inference.huggingface.com/models/sentence-transformers/all-MiniLM-L6-v2"
        );
    }

    #[test]
    fn test_new_with_custom_base_url() {
        let embedder = HuggingFaceEmbedder::new(
            "test-key".to_string(),
            "my-model".to_string(),
            Some("https://proxy.example.com/hf/models".to_string()),
            Duration::from_secs(5),
        )
        .expect("construct");
        assert_eq!(
            embedder.endpoint(),
            "https://proxy.example.com/hf/models/my-model"
        );
    }

    #[test]
    fn test_dimensions_starts_at_zero() {
        let embedder = HuggingFaceEmbedder::new(
            "test-key".to_string(),
            "sentence-transformers/all-MiniLM-L6-v2".to_string(),
            None,
            Duration::from_secs(5),
        )
        .expect("construct");
        assert_eq!(embedder.dimensions(), 0);
    }

    #[tokio::test]
    async fn test_embed_http_error_propagates() {
        // Port 1 → connection refused
        let embedder = HuggingFaceEmbedder::new(
            "test-key".to_string(),
            "my-model".to_string(),
            Some("http://127.0.0.1:1".to_string()),
            Duration::from_millis(500),
        )
        .expect("construct");
        let result = embedder.embed("hello").await;
        assert!(result.is_err(), "expected error for unreachable host");
    }
}
