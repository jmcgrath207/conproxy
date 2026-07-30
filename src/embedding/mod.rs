//! Embedding and model management module.
//!
//! ONNX model path helpers (always available).
//! Embedder provider trait + API providers require `embed-api`.
//! ONNX embedder requires `embed` (which includes `embed-api`).

pub mod models;

#[cfg(feature = "embed-api")]
pub mod provider;

#[cfg(feature = "embed-api")]
pub mod openai;

#[cfg(feature = "embed-api")]
pub mod cohere;

#[cfg(feature = "embed-api")]
pub mod huggingface;

#[cfg(feature = "embed")]
pub mod embedder;
