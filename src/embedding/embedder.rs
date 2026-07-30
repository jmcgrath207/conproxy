//! ONNX-based text embedding using sentence transformers
//!
//! Provides the `Embedder` struct for producing vector embeddings from text,
//! plus standalone `mean_pool` and `normalize` functions for testability.

use crate::error::{ConproxyError, Result};
use ndarray::{Array2, ArrayView3};
use ort::session::Session;
use ort::value::TensorRef;
use std::path::Path;
use std::sync::Mutex;
use tokenizers::Tokenizer;

use super::provider::EmbedderProvider;
use async_trait::async_trait;

/// Produces vector embeddings from text using an ONNX sentence-transformer model.
pub struct Embedder {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    dimensions: usize,
}

impl Embedder {
    /// Load an ONNX model and tokenizer from disk.
    ///
    /// Infers the embedding dimensions by running a probe through the model.
    ///
    /// # Errors
    ///
    /// Returns [`ConproxyError::Embedding`] on ONNX session construction
    /// failure, tokenizer load failure, or when the initial probe embed fails.
    pub fn new(model_path: impl AsRef<Path>, tokenizer_path: impl AsRef<Path>) -> Result<Self> {
        let session = Session::builder()
            .and_then(|mut b| b.commit_from_file(model_path.as_ref()))
            .map_err(|e| ConproxyError::Embedding(format!("Failed to load ONNX model: {}", e)))?;

        let tokenizer = Tokenizer::from_file(tokenizer_path.as_ref())
            .map_err(|e| ConproxyError::Embedding(format!("Failed to load tokenizer: {}", e)))?;

        // Probe to infer dimensions
        let mut embedder = Self {
            session: Mutex::new(session),
            tokenizer,
            dimensions: 0,
        };

        let probe = embedder.embed("hello")?;
        embedder.dimensions = probe.len();

        Ok(embedder)
    }

    /// Returns the dimensionality of the embedding vectors.
    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// Embed a single text string into a vector.
    ///
    /// # Errors
    ///
    /// Returns [`ConproxyError::Embedding`] on tokenization failure, tensor
    /// construction failure, or ONNX inference failure.
    pub fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| ConproxyError::Embedding(format!("Tokenization failed: {}", e)))?;

        let ids = encoding.get_ids();
        let mask = encoding.get_attention_mask();
        let seq_len = ids.len();

        let ids_i64: Vec<i64> = ids.iter().map(|&v| v as i64).collect();
        let mask_i64: Vec<i64> = mask.iter().map(|&v| v as i64).collect();
        let type_ids_i64: Vec<i64> = vec![0i64; seq_len];

        // Use tuple (shape, &[T]) API to avoid ndarray version conflicts with ort
        let input_ids_tensor = TensorRef::from_array_view(([1usize, seq_len], ids_i64.as_slice()))
            .map_err(|e| ConproxyError::Embedding(format!("Input tensor error: {}", e)))?;
        let attention_mask_tensor =
            TensorRef::from_array_view(([1usize, seq_len], mask_i64.as_slice()))
                .map_err(|e| ConproxyError::Embedding(format!("Input tensor error: {}", e)))?;
        let token_type_ids_tensor =
            TensorRef::from_array_view(([1usize, seq_len], type_ids_i64.as_slice()))
                .map_err(|e| ConproxyError::Embedding(format!("Input tensor error: {}", e)))?;

        // Run inference and extract data while session lock is held
        let (shape_dims, output_data) = {
            let mut session = self
                .session
                .lock()
                .map_err(|e| ConproxyError::Embedding(format!("Session lock failed: {}", e)))?;
            let outputs = session
                .run(ort::inputs![
                    input_ids_tensor,
                    attention_mask_tensor,
                    token_type_ids_tensor
                ])
                .map_err(|e| ConproxyError::Embedding(format!("Inference failed: {}", e)))?;

            let (shape, data) = outputs[0].try_extract_tensor::<f32>().map_err(|e| {
                ConproxyError::Embedding(format!("Output extraction failed: {}", e))
            })?;

            let dims: Vec<usize> = shape.iter().map(|&d| d as usize).collect();
            let owned_data: Vec<f32> = data.to_vec();
            (dims, owned_data)
        };

        // Output shape: (1, seq_len, hidden_dim)
        let view3 = ndarray::ArrayView3::from_shape(
            (shape_dims[0], shape_dims[1], shape_dims[2]),
            &output_data,
        )
        .map_err(|e| ConproxyError::Embedding(format!("Reshape error: {}", e)))?;

        let attention_mask_nd = Array2::from_shape_vec((1, seq_len), mask_i64)
            .map_err(|e| ConproxyError::Embedding(format!("Shape error: {}", e)))?;

        let pooled = mean_pool(&view3, &attention_mask_nd);
        let mut embedding = pooled.row(0).to_vec();
        normalize(&mut embedding);

        Ok(embedding)
    }

    /// Embed a batch of texts into vectors.
    ///
    /// # Errors
    ///
    /// Returns [`ConproxyError::Embedding`] on tokenization, tensor, or
    /// inference failure (any single-item error short-circuits the batch).
    pub fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        if texts.len() == 1 {
            return Ok(vec![self.embed(texts[0])?]);
        }

        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| ConproxyError::Embedding(format!("Batch tokenization failed: {}", e)))?;

        let max_len = encodings
            .iter()
            .map(|e| e.get_ids().len())
            .max()
            .unwrap_or(0);
        let batch_size = encodings.len();

        // Build padded input_ids, attention_mask, and token_type_ids
        let mut ids_flat = Vec::with_capacity(batch_size * max_len);
        let mut mask_flat = Vec::with_capacity(batch_size * max_len);

        for enc in &encodings {
            let ids = enc.get_ids();
            let mask = enc.get_attention_mask();

            ids_flat.extend(ids.iter().map(|&id| id as i64));
            ids_flat.extend(std::iter::repeat_n(0i64, max_len - ids.len()));

            mask_flat.extend(mask.iter().map(|&m| m as i64));
            mask_flat.extend(std::iter::repeat_n(0i64, max_len - mask.len()));
        }

        let type_ids_flat: Vec<i64> = vec![0i64; batch_size * max_len];

        let input_ids_tensor =
            TensorRef::from_array_view(([batch_size, max_len], ids_flat.as_slice()))
                .map_err(|e| ConproxyError::Embedding(format!("Input tensor error: {}", e)))?;
        let attention_mask_tensor =
            TensorRef::from_array_view(([batch_size, max_len], mask_flat.as_slice()))
                .map_err(|e| ConproxyError::Embedding(format!("Input tensor error: {}", e)))?;
        let token_type_ids_tensor =
            TensorRef::from_array_view(([batch_size, max_len], type_ids_flat.as_slice()))
                .map_err(|e| ConproxyError::Embedding(format!("Input tensor error: {}", e)))?;

        let (shape_dims, output_data) = {
            let mut session = self
                .session
                .lock()
                .map_err(|e| ConproxyError::Embedding(format!("Session lock failed: {}", e)))?;
            let outputs = session
                .run(ort::inputs![
                    input_ids_tensor,
                    attention_mask_tensor,
                    token_type_ids_tensor
                ])
                .map_err(|e| ConproxyError::Embedding(format!("Inference failed: {}", e)))?;

            let (shape, data) = outputs[0].try_extract_tensor::<f32>().map_err(|e| {
                ConproxyError::Embedding(format!("Output extraction failed: {}", e))
            })?;

            let dims: Vec<usize> = shape.iter().map(|&d| d as usize).collect();
            let owned_data: Vec<f32> = data.to_vec();
            (dims, owned_data)
        };

        let view3 = ndarray::ArrayView3::from_shape(
            (shape_dims[0], shape_dims[1], shape_dims[2]),
            &output_data,
        )
        .map_err(|e| ConproxyError::Embedding(format!("Reshape error: {}", e)))?;

        let attention_mask_nd = Array2::from_shape_vec((batch_size, max_len), mask_flat)
            .map_err(|e| ConproxyError::Embedding(format!("Shape error: {}", e)))?;

        let pooled = mean_pool(&view3, &attention_mask_nd);

        let mut results = Vec::with_capacity(batch_size);
        for i in 0..batch_size {
            let mut embedding = pooled.row(i).to_vec();
            normalize(&mut embedding);
            results.push(embedding);
        }

        Ok(results)
    }
}

#[async_trait]
impl EmbedderProvider for Embedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        Embedder::embed(self, text)
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        Embedder::embed_batch(self, texts)
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}

/// Mean pooling over the sequence dimension, respecting the attention mask.
///
/// # Arguments
///
/// * `token_embeddings` - Shape (batch_size, seq_len, hidden_dim)
/// * `attention_mask` - Shape (batch_size, seq_len), values 0 or 1
///
/// # Returns
///
/// Array2 of shape (batch_size, hidden_dim) with mean-pooled embeddings.
pub fn mean_pool(token_embeddings: &ArrayView3<f32>, attention_mask: &Array2<i64>) -> Array2<f32> {
    let batch_size = token_embeddings.shape()[0];
    let hidden_dim = token_embeddings.shape()[2];

    let mut result = Array2::<f32>::zeros((batch_size, hidden_dim));

    for b in 0..batch_size {
        let mut sum = vec![0.0f32; hidden_dim];
        let mut count = 0.0f32;

        for s in 0..token_embeddings.shape()[1] {
            let mask_val = attention_mask[[b, s]] as f32;
            if mask_val > 0.0 {
                for d in 0..hidden_dim {
                    sum[d] += token_embeddings[[b, s, d]] * mask_val;
                }
                count += mask_val;
            }
        }

        if count > 0.0 {
            for d in 0..hidden_dim {
                result[[b, d]] = sum[d] / count;
            }
        }
    }

    result
}

/// L2-normalize an embedding vector in-place.
///
/// If the vector is all zeros, it stays all zeros (no NaN).
pub fn normalize(embedding: &mut [f32]) {
    let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in embedding.iter_mut() {
            *x /= norm;
        }
    }
}

#[cfg(test)]
#[path = "tests/embedder_tests.rs"]
mod tests;
