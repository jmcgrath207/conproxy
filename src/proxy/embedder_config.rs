//! Configuration for the proxy smart embedder.
//!
//! Provides configuration options for embedding in the proxy, including
//! model selection, caching, batching, and performance tuning.

use std::time::Duration;

/// Configuration for the proxy smart embedder.
#[derive(Debug, Clone)]
pub struct EmbedderConfig {
    /// Name of the embedding model to use.
    /// Must match a model in the catalog (e.g., "all-MiniLM-L6-v2").
    pub model_name: String,

    /// Maximum number of embeddings to cache.
    pub cache_max_entries: usize,

    /// TTL for cached embeddings.
    pub cache_ttl: Duration,

    /// Maximum batch size for embedding requests.
    pub max_batch_size: usize,

    /// Whether to warmup the model on startup.
    pub warmup_on_start: bool,

    /// Number of warmup iterations to run.
    pub warmup_iterations: usize,

    /// Timeout for individual embedding requests.
    pub request_timeout: Duration,

    /// Embedding provider type: "onnx", "openai", "cohere", "huggingface".
    pub provider: String,

    /// API key for remote providers. Supports `${VAR}` env-var references.
    pub api_key: Option<String>,

    /// Base URL override for remote providers.
    pub base_url: Option<String>,
}

impl Default for EmbedderConfig {
    fn default() -> Self {
        Self {
            model_name: "all-MiniLM-L6-v2".to_string(),
            cache_max_entries: 10_000,
            cache_ttl: Duration::from_secs(3600), // 1 hour
            max_batch_size: 32,
            warmup_on_start: true,
            warmup_iterations: 3,
            request_timeout: Duration::from_secs(30),
            provider: "onnx".to_string(),
            api_key: None,
            base_url: None,
        }
    }
}

impl EmbedderConfig {
    /// Create a new config with the specified model name.
    pub fn new(model_name: &str) -> Self {
        Self {
            model_name: model_name.to_string(),
            ..Default::default()
        }
    }

    /// Set the cache max entries.
    pub fn with_cache_max_entries(mut self, max_entries: usize) -> Self {
        self.cache_max_entries = max_entries;
        self
    }

    /// Set the cache TTL.
    pub fn with_cache_ttl(mut self, ttl: Duration) -> Self {
        self.cache_ttl = ttl;
        self
    }

    /// Set the max batch size.
    pub fn with_max_batch_size(mut self, size: usize) -> Self {
        self.max_batch_size = size;
        self
    }

    /// Set whether to warmup on start.
    pub fn with_warmup(mut self, warmup: bool) -> Self {
        self.warmup_on_start = warmup;
        self
    }

    /// Set the request timeout.
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Disable caching.
    pub fn without_cache(mut self) -> Self {
        self.cache_max_entries = 0;
        self
    }
}

#[cfg(test)]
#[path = "tests/embedder_config_tests.rs"]
mod tests;
