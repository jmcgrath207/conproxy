//! Smart embedder with caching and coalescing for the proxy.
//!
//! Wraps the base ONNX embedder with:
//! - Embedding cache (text → vector)
//! - Request coalescing for identical concurrent requests
//! - Lazy model loading with warmup
//! - Batching support

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use parking_lot::RwLock;
use tokio::sync::broadcast;

use super::embedder_config::EmbedderConfig;
use crate::embedding::provider::{create_provider, EmbedderProvider, ProviderConfig, ProviderType};
use crate::error::{ConproxyError, Result};
use tracing::debug;

/// Cached embedding entry.
struct CachedEmbedding {
    vector: Vec<f32>,
    created_at: Instant,
}

/// Action to take for a coalesced request.
enum CoalesceAction {
    /// This request is the leader, should compute the embedding.
    Leader(broadcast::Sender<Vec<f32>>),
    /// This request should wait for the leader.
    Waiter(broadcast::Receiver<Vec<f32>>),
}

/// Smart embedder with caching and request coalescing.
pub struct SmartEmbedder {
    /// Configuration.
    config: EmbedderConfig,

    /// The underlying embedder provider (lazy loaded).
    embedder: RwLock<Option<Arc<dyn EmbedderProvider>>>,

    /// Embedding cache: model-aware key → vector.
    cache: DashMap<u64, CachedEmbedding>,

    /// In-flight requests for coalescing.
    in_flight: DashMap<u64, broadcast::Sender<Vec<f32>>>,

    /// Whether the model has been loaded.
    loaded: AtomicBool,

    /// Statistics: cache hits.
    cache_hits: AtomicU64,

    /// Statistics: cache misses.
    cache_misses: AtomicU64,

    /// Statistics: coalesced requests.
    coalesced: AtomicU64,

    /// Statistics: total embeddings computed.
    embeddings_computed: AtomicU64,
}

impl SmartEmbedder {
    /// Create a new smart embedder with the given configuration.
    pub fn new(config: EmbedderConfig) -> Self {
        Self {
            config,
            embedder: RwLock::new(None),
            cache: DashMap::new(),
            in_flight: DashMap::new(),
            loaded: AtomicBool::new(false),
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
            coalesced: AtomicU64::new(0),
            embeddings_computed: AtomicU64::new(0),
        }
    }

    /// Create a smart embedder with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(EmbedderConfig::default())
    }

    /// Check if the model is loaded.
    pub fn is_loaded(&self) -> bool {
        self.loaded.load(Ordering::Relaxed)
    }

    /// Get the embedding dimensions (returns None if not loaded).
    pub fn dimensions(&self) -> Option<usize> {
        self.embedder.read().as_ref().map(|e| e.dimensions())
    }

    /// Ensure the model is loaded, loading it if necessary.
    pub fn ensure_loaded(&self) -> Result<()> {
        if self.is_loaded() {
            return Ok(());
        }

        let mut guard = self.embedder.write();
        if guard.is_some() {
            return Ok(());
        }

        let provider_type = ProviderType::from_str(&self.config.provider)?;

        let provider_config = ProviderConfig {
            provider: provider_type,
            model_name: self.config.model_name.clone(),
            api_key: self.config.api_key.clone(),
            base_url: self.config.base_url.clone(),
            request_timeout: self.config.request_timeout,
        };

        let embedder = create_provider(&provider_config)?;

        *guard = Some(embedder);
        self.loaded.store(true, Ordering::Release);

        Ok(())
    }

    /// Warmup the model by running a few probe embeddings.
    pub async fn warmup(&self) -> Result<()> {
        self.ensure_loaded()?;

        let embedder = {
            let guard = self.embedder.read();
            guard
                .as_ref()
                .ok_or(ConproxyError::ModelNotInstalled)?
                .clone()
        };

        for i in 0..self.config.warmup_iterations {
            let probe_text = format!("warmup probe iteration {}", i);
            embedder.embed(&probe_text).await?;
        }

        tracing::info!(
            model = %self.config.model_name,
            iterations = self.config.warmup_iterations,
            "Embedder warmup complete"
        );

        Ok(())
    }

    /// Cache key: provider + model + text (avoids cross-model collisions).
    ///
    /// Changing provider/model intentionally cold-starts the cache for that identity.
    fn cache_key(&self, text: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.config.provider.hash(&mut hasher);
        self.config.model_name.hash(&mut hasher);
        text.hash(&mut hasher);
        hasher.finish()
    }

    /// Test/helper: hash without a live instance.
    #[cfg(test)]
    fn text_hash(provider: &str, model_name: &str, text: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        provider.hash(&mut hasher);
        model_name.hash(&mut hasher);
        text.hash(&mut hasher);
        hasher.finish()
    }

    /// Get or insert into coalescing map.
    fn get_or_insert_coalesce(&self, hash: u64) -> CoalesceAction {
        // Try to get existing sender
        if let Some(sender) = self.in_flight.get(&hash) {
            self.coalesced.fetch_add(1, Ordering::Relaxed);
            return CoalesceAction::Waiter(sender.subscribe());
        }

        // Create new sender for this request
        let (tx, _rx) = broadcast::channel(1);
        match self.in_flight.entry(hash) {
            dashmap::mapref::entry::Entry::Occupied(entry) => {
                // Someone else got there first
                self.coalesced.fetch_add(1, Ordering::Relaxed);
                CoalesceAction::Waiter(entry.get().subscribe())
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(tx.clone());
                CoalesceAction::Leader(tx)
            }
        }
    }

    /// Embed a single text with caching and coalescing.
    pub async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let hash = self.cache_key(text);

        // Check cache first
        if let Some(entry) = self.cache.get(&hash) {
            if entry.created_at.elapsed() < self.config.cache_ttl {
                self.cache_hits.fetch_add(1, Ordering::Relaxed);
                return Ok(entry.vector.clone());
            }
        }

        self.cache_misses.fetch_add(1, Ordering::Relaxed);

        // Check for coalescing
        match self.get_or_insert_coalesce(hash) {
            CoalesceAction::Leader(sender) => {
                // We're the leader, compute the embedding
                let result = self.compute_embedding(text).await;

                // Remove from in-flight
                self.in_flight.remove(&hash);

                match &result {
                    Ok(vector) => {
                        // Broadcast to waiters
                        let _ = sender.send(vector.clone());

                        // Cache the result
                        if self.config.cache_max_entries > 0 {
                            self.maybe_evict();
                            self.cache.insert(
                                hash,
                                CachedEmbedding {
                                    vector: vector.clone(),
                                    created_at: Instant::now(),
                                },
                            );
                        }
                    }
                    Err(e) => {
                        // Don't broadcast errors (the channel carries Result-of-Vec<f32>;
                        // waiters would get a different type). Log so production can observe
                        // the failure mode, and drop the sender so waiters see a closed
                        // channel and bubble up "Coalesced request failed" themselves.
                        debug!(error = %e, "smart_embedder: leader request failed");
                    }
                }

                result
            }
            CoalesceAction::Waiter(mut receiver) => {
                // Wait for the leader
                receiver
                    .recv()
                    .await
                    .map_err(|_| ConproxyError::Embedding("Coalesced request failed".to_string()))
            }
        }
    }

    /// Compute embedding without caching (internal).
    async fn compute_embedding(&self, text: &str) -> Result<Vec<f32>> {
        self.ensure_loaded()?;

        let embedder = {
            let guard = self.embedder.read();
            guard
                .as_ref()
                .ok_or(ConproxyError::ModelNotInstalled)?
                .clone()
        };

        let result = embedder.embed(text).await?;
        self.embeddings_computed.fetch_add(1, Ordering::Relaxed);

        Ok(result)
    }

    /// Embed a batch of texts.
    #[allow(clippy::indexing_slicing)]
    pub async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        self.ensure_loaded()?;

        let embedder = {
            let guard = self.embedder.read();
            guard
                .as_ref()
                .ok_or(ConproxyError::ModelNotInstalled)?
                .clone()
        };

        // Check cache for each text
        let mut results = vec![None; texts.len()];
        let mut to_compute: Vec<(usize, &str)> = Vec::new();

        for (i, text) in texts.iter().enumerate() {
            let hash = self.cache_key(text);
            if let Some(entry) = self.cache.get(&hash) {
                if entry.created_at.elapsed() < self.config.cache_ttl {
                    self.cache_hits.fetch_add(1, Ordering::Relaxed);
                    results[i] = Some(entry.vector.clone());
                    continue;
                }
            }
            self.cache_misses.fetch_add(1, Ordering::Relaxed);
            to_compute.push((i, text));
        }

        // Compute missing embeddings in batches
        if !to_compute.is_empty() {
            // Guard against misconfiguration: `slice::chunks(0)` panics.
            let batch_size = self.config.max_batch_size.max(1);
            for chunk in to_compute.chunks(batch_size) {
                let chunk_texts: Vec<&str> = chunk.iter().map(|(_, t)| *t).collect();
                let computed: Vec<Vec<f32>> = embedder.embed_batch(&chunk_texts).await?;
                self.embeddings_computed
                    .fetch_add(computed.len() as u64, Ordering::Relaxed);

                for (i, vector) in computed.into_iter().enumerate() {
                    let (orig_idx, text) = chunk[i];

                    // Cache the result
                    if self.config.cache_max_entries > 0 {
                        let hash = self.cache_key(text);
                        let cached = CachedEmbedding {
                            vector: vector.clone(),
                            created_at: Instant::now(),
                        };
                        self.cache.insert(hash, cached);
                    }

                    results[orig_idx] = Some(vector);
                }

                // Evict old entries to keep cache within budget. Called once
                // per batch (not per insert) to amortize the O(n) scan cost.
                self.maybe_evict();
            }
        }

        // Collect all results
        results
            .into_iter()
            .enumerate()
            .map(|(i, r)| {
                r.ok_or_else(|| {
                    ConproxyError::Embedding(format!("Missing embedding for text at index {}", i))
                })
            })
            .collect()
    }

    /// Evict old entries if over capacity.
    fn maybe_evict(&self) {
        if self.cache.len() <= self.config.cache_max_entries {
            return;
        }

        // Evict oldest 10%
        let to_evict = self.config.cache_max_entries / 10;
        let mut entries: Vec<_> = self
            .cache
            .iter()
            .map(|e| (*e.key(), e.created_at))
            .collect();
        entries.sort_by_key(|(_, created)| *created);

        for (key, _) in entries.into_iter().take(to_evict) {
            self.cache.remove(&key);
        }
    }

    /// Clear the embedding cache.
    pub fn clear_cache(&self) {
        self.cache.clear();
    }

    /// Get cache statistics.
    pub fn stats(&self) -> SmartEmbedderStats {
        SmartEmbedderStats {
            cache_size: self.cache.len(),
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            cache_misses: self.cache_misses.load(Ordering::Relaxed),
            coalesced_requests: self.coalesced.load(Ordering::Relaxed),
            embeddings_computed: self.embeddings_computed.load(Ordering::Relaxed),
            model_loaded: self.is_loaded(),
            model_name: self.config.model_name.clone(),
            dimensions: self.dimensions(),
        }
    }
}

/// Statistics for the smart embedder.
#[derive(Debug, Clone)]
pub struct SmartEmbedderStats {
    /// Current cache size.
    pub cache_size: usize,
    /// Total cache hits.
    pub cache_hits: u64,
    /// Total cache misses.
    pub cache_misses: u64,
    /// Total coalesced requests.
    pub coalesced_requests: u64,
    /// Total embeddings computed.
    pub embeddings_computed: u64,
    /// Whether the model is loaded.
    pub model_loaded: bool,
    /// Model name.
    pub model_name: String,
    /// Embedding dimensions (if loaded).
    pub dimensions: Option<usize>,
}

impl SmartEmbedderStats {
    /// Calculate cache hit rate.
    #[allow(clippy::arithmetic_side_effects)]
    pub fn hit_rate(&self) -> f64 {
        let total = self.cache_hits + self.cache_misses;
        if total == 0 {
            0.0
        } else {
            self.cache_hits as f64 / total as f64
        }
    }
}

#[cfg(test)]
#[path = "tests/smart_embedder_tests.rs"]
mod tests;
