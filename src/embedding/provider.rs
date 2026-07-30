//! Embedder provider trait and factory.
//!
//! Abstracts embedding backends so the proxy can use ONNX (local),
//! OpenAI, Cohere, or HuggingFace API providers interchangeably.

use crate::error::{ConproxyError, Result};
use async_trait::async_trait;
use std::sync::Arc;

/// Trait for text embedding providers.
///
/// Implementations include local ONNX models ([`crate::embedding::embedder::Embedder`])
/// and remote API providers (OpenAI, Cohere, HuggingFace).
#[async_trait]
pub trait EmbedderProvider: Send + Sync {
    /// Embed a single text string into a vector.
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Embed a batch of texts into vectors.
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;

    /// Returns the dimensionality of the embedding vectors.
    fn dimensions(&self) -> usize;
}

/// Provider type selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderType {
    /// Local ONNX model (requires `embed` feature).
    Onnx,
    /// OpenAI embeddings API.
    OpenAi,
    /// Cohere embeddings API.
    Cohere,
    /// HuggingFace inference API.
    HuggingFace,
}

impl ProviderType {
    /// Parse from a string.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "onnx" => Ok(Self::Onnx),
            "openai" => Ok(Self::OpenAi),
            "cohere" => Ok(Self::Cohere),
            "huggingface" | "hf" => Ok(Self::HuggingFace),
            other => Err(ConproxyError::InvalidConfig(format!(
                "Unknown embedding provider: '{other}'. Expected: onnx, openai, cohere, huggingface"
            ))),
        }
    }
}

/// Resolve a `${VAR}` env-var reference, falling back to the literal string.
fn resolve_env_ref(s: &str) -> String {
    if let Some(var) = s.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        std::env::var(var).unwrap_or_else(|_| {
            tracing::warn!("env var '{var}' not set, using empty string for embedding api_key");
            String::new()
        })
    } else {
        s.to_string()
    }
}

/// Configuration for creating an embedder provider.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    /// Provider type.
    pub provider: ProviderType,
    /// Model name (ONNX model name or API model identifier).
    pub model_name: String,
    /// API key (for API providers). Supports `${VAR}` env-var references.
    pub api_key: Option<String>,
    /// Optional base URL override (for API providers).
    pub base_url: Option<String>,
    /// Per-request HTTP timeout for API providers. Ignored for ONNX.
    pub request_timeout: std::time::Duration,
}

/// Create an embedder provider from configuration.
///
/// For ONNX, requires the `embed` feature and the model to be installed.
/// For API providers, constructs an HTTP client targeting the provider's endpoint.
///
/// # Errors
///
/// Returns [`ConproxyError::ModelNotInstalled`] if the ONNX model or
/// tokenizer is missing. Returns [`ConproxyError::EmbedderInit`] if the
/// ONNX session cannot be built. Returns [`ConproxyError::Config`] for
/// unsupported provider types or malformed API provider config. Returns
/// [`ConproxyError::HttpClient`] if the HTTP client fails to construct.
pub fn create_provider(config: &ProviderConfig) -> Result<Arc<dyn EmbedderProvider>> {
    match config.provider {
        ProviderType::Onnx => {
            #[cfg(feature = "embed")]
            {
                use crate::embedding::embedder::Embedder;
                use crate::embedding::models::ModelManager;

                if !ModelManager::is_installed(&config.model_name) {
                    return Err(ConproxyError::ModelNotInstalled);
                }
                let model_path = ModelManager::model_path(&config.model_name);
                let tokenizer_path = ModelManager::tokenizer_path(&config.model_name);
                let embedder = Embedder::new(&model_path, &tokenizer_path)?;
                Ok(Arc::new(embedder))
            }
            #[cfg(not(feature = "embed"))]
            {
                Err(ConproxyError::InvalidConfig(
                    "ONNX provider requires the 'embed' feature".to_string(),
                ))
            }
        }
        ProviderType::OpenAi => {
            let api_key = config
                .api_key
                .as_ref()
                .map(|k| resolve_env_ref(k))
                .ok_or_else(|| {
                    ConproxyError::InvalidConfig("OpenAI provider requires api_key".to_string())
                })?;
            let provider = crate::embedding::openai::OpenAiEmbedder::new(
                api_key,
                config.model_name.clone(),
                config.base_url.clone(),
                config.request_timeout,
            )?;
            Ok(Arc::new(provider))
        }
        ProviderType::Cohere => {
            let api_key = config
                .api_key
                .as_ref()
                .map(|k| resolve_env_ref(k))
                .ok_or_else(|| {
                    ConproxyError::InvalidConfig("Cohere provider requires api_key".to_string())
                })?;
            let provider = crate::embedding::cohere::CohereEmbedder::new(
                api_key,
                config.model_name.clone(),
                config.base_url.clone(),
                config.request_timeout,
            )?;
            Ok(Arc::new(provider))
        }
        ProviderType::HuggingFace => {
            let api_key = config
                .api_key
                .as_ref()
                .map(|k| resolve_env_ref(k))
                .ok_or_else(|| {
                    ConproxyError::InvalidConfig(
                        "HuggingFace provider requires api_key".to_string(),
                    )
                })?;
            let provider = crate::embedding::huggingface::HuggingFaceEmbedder::new(
                api_key,
                config.model_name.clone(),
                config.base_url.clone(),
                config.request_timeout,
            )?;
            Ok(Arc::new(provider))
        }
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
    fn test_provider_type_from_str_onnx() {
        assert_eq!(ProviderType::from_str("onnx").unwrap(), ProviderType::Onnx);
    }

    #[test]
    fn test_provider_type_from_str_openai() {
        assert_eq!(
            ProviderType::from_str("openai").unwrap(),
            ProviderType::OpenAi
        );
    }

    #[test]
    fn test_provider_type_from_str_cohere() {
        assert_eq!(
            ProviderType::from_str("cohere").unwrap(),
            ProviderType::Cohere
        );
    }

    #[test]
    fn test_provider_type_from_str_huggingface_aliases() {
        // "huggingface" and "hf" both map to HuggingFace
        assert_eq!(
            ProviderType::from_str("huggingface").unwrap(),
            ProviderType::HuggingFace
        );
        assert_eq!(
            ProviderType::from_str("hf").unwrap(),
            ProviderType::HuggingFace
        );
    }

    #[test]
    fn test_provider_type_from_str_case_insensitive() {
        assert_eq!(ProviderType::from_str("ONNX").unwrap(), ProviderType::Onnx);
        assert_eq!(
            ProviderType::from_str("OpenAI").unwrap(),
            ProviderType::OpenAi
        );
        assert_eq!(
            ProviderType::from_str("COHERE").unwrap(),
            ProviderType::Cohere
        );
    }

    #[test]
    fn test_provider_type_from_str_unknown() {
        let err = ProviderType::from_str("bogus").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("Unknown embedding provider"),
            "error should mention unknown provider, got: {msg}"
        );
        assert!(msg.contains("bogus"), "error should echo input, got: {msg}");
    }

    #[test]
    fn test_resolve_env_ref_passthrough() {
        // No ${} wrapper → return as-is
        assert_eq!(resolve_env_ref("plain-string"), "plain-string");
    }

    #[test]
    fn test_resolve_env_ref_unset_var() {
        // Unset env var → empty string + warning
        let var = "CONPROXY_TEST_NONEXISTENT_VAR_42";
        std::env::remove_var(var);
        let result = resolve_env_ref(&format!("${{{var}}}"));
        assert_eq!(result, "", "unset env var should fall back to empty string");
    }

    #[test]
    fn test_resolve_env_ref_set_var() {
        let var = "CONPROXY_TEST_RESOLVE_VAR";
        std::env::set_var(var, "secret-value");
        let result = resolve_env_ref(&format!("${{{var}}}"));
        assert_eq!(result, "secret-value");
        std::env::remove_var(var);
    }

    #[test]
    fn test_create_provider_openai_missing_api_key() {
        let cfg = ProviderConfig {
            provider: ProviderType::OpenAi,
            model_name: "text-embedding-3-small".to_string(),
            api_key: None,
            base_url: None,
            request_timeout: std::time::Duration::from_secs(5),
        };
        let result = create_provider(&cfg);
        match result {
            Err(e) => {
                let msg = format!("{e}");
                assert!(
                    msg.contains("api_key"),
                    "error should mention api_key, got: {msg}"
                );
            }
            Ok(_) => panic!("expected error for missing api_key, got success"),
        }
    }

    #[test]
    fn test_create_provider_cohere_missing_api_key() {
        let cfg = ProviderConfig {
            provider: ProviderType::Cohere,
            model_name: "embed-english-v3.0".to_string(),
            api_key: None,
            base_url: None,
            request_timeout: std::time::Duration::from_secs(5),
        };
        let result = create_provider(&cfg);
        match result {
            Err(e) => {
                let msg = format!("{e}");
                assert!(
                    msg.contains("api_key"),
                    "error should mention api_key, got: {msg}"
                );
            }
            Ok(_) => panic!("expected error for missing api_key, got success"),
        }
    }

    #[test]
    fn test_create_provider_huggingface_missing_api_key() {
        let cfg = ProviderConfig {
            provider: ProviderType::HuggingFace,
            model_name: "sentence-transformers/all-MiniLM-L6-v2".to_string(),
            api_key: None,
            base_url: None,
            request_timeout: std::time::Duration::from_secs(5),
        };
        let result = create_provider(&cfg);
        match result {
            Err(e) => {
                let msg = format!("{e}");
                assert!(
                    msg.contains("api_key"),
                    "error should mention api_key, got: {msg}"
                );
            }
            Ok(_) => panic!("expected error for missing api_key, got success"),
        }
    }

    #[test]
    fn test_create_provider_openai_with_api_key() {
        // Doesn't make any network call — just verifies construction succeeds
        // and returns something implementing EmbedderProvider.
        let cfg = ProviderConfig {
            provider: ProviderType::OpenAi,
            model_name: "text-embedding-3-small".to_string(),
            api_key: Some("test-key".to_string()),
            base_url: Some("https://api.example.com".to_string()),
            request_timeout: std::time::Duration::from_secs(5),
        };
        let provider = create_provider(&cfg).expect("openai with api_key should construct");
        // Dimensions are unknown until first embed() call → starts at 0
        assert_eq!(
            provider.dimensions(),
            0,
            "dimensions should be 0 before first embed (lazy probe)"
        );
    }
}
