#![deny(unsafe_code)]
#![doc = "Cache proxy server for RAG/vector search with LLM passthrough."]
#![doc = ""]
#![doc = "Provides caching, circuit-breaking, load balancing, and query federation"]
#![doc = "for search backends (Elasticsearch, Qdrant, pgvector, and generic REST)."]

pub mod cache;
pub mod config;
pub mod embedding;
pub mod error;
pub mod proxy;

#[cfg(feature = "mcp")]
pub mod mcp;

// Re-exports
pub use config::{Config, ProxyConfig};
pub use embedding::models::ModelManager;
pub use error::{ConproxyError, Result};

#[cfg(feature = "embed-api")]
pub use embedding::provider::{EmbedderProvider, ProviderConfig, ProviderType};

#[cfg(feature = "embed")]
pub use embedding::embedder::Embedder;
