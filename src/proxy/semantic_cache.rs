//! Semantic cache tier — embedding-based similarity matching.
//!
//! Provides a second-tier cache that matches queries by embedding cosine
//! similarity rather than exact/normalized text. When a query misses the
//! primary cache, the proxy computes its embedding and scans this structure
//! for a similar past query whose response can be reused.
//!
//! Disabled state is a `None` `Arc`; all methods on the unconfigured path
//! short-circuit with zero overhead.
//!
//! Concurrency: `DashMap` for lock-free reads, with an `AtomicU64` sequence
//! counter for LRU eviction. Embeddings are stored behind `Arc<Vec<f32>>` so
//! so scans don't clone the underlying vector. Each entry also stores
//! its insertion sequence for true LRU eviction.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use serde::{Deserialize, Serialize};

use crate::proxy::types::QueryHash;

/// Default cosine similarity threshold for a semantic match.
pub const DEFAULT_SIMILARITY_THRESHOLD: f32 = 0.92;

/// Default maximum entries before LRU eviction kicks in.
pub const DEFAULT_MAX_ENTRIES: usize = 10_000;

/// Semantic cache tier. Maps every cached query hash to its embedding so
/// future queries can be matched by cosine similarity.
///
/// `threshold` is set on construction and used by `lookup`. `max_entries`
/// caps memory usage; LRU eviction triggers when `len()` exceeds the cap.
#[derive(Debug)]
pub struct SemanticCache {
    /// Query hash → (insertion sequence, stored embedding).
    embeddings: DashMap<QueryHash, Arc<(u64, Vec<f32>)>>,
    /// Monotonic counter for LRU ordering.
    sequence: AtomicU64,
    /// Cosine similarity threshold (0.0–1.0). Higher = stricter.
    threshold: f32,
    /// Max entries before LRU eviction.
    max_entries: usize,
    /// Cache hit counter.
    hits: AtomicU64,
    /// Cache miss counter.
    misses: AtomicU64,
    /// Eviction counter.
    evictions: AtomicU64,
}

impl SemanticCache {
    /// Create a new semantic cache tier.
    ///
    /// # Arguments
    /// * `threshold` - Cosine similarity in [0.0, 1.0]. `0.92` is a good default.
    /// * `max_entries` - Cap on entries. `0` disables the cache.
    pub fn new(threshold: f32, max_entries: usize) -> Self {
        Self {
            embeddings: DashMap::new(),
            sequence: AtomicU64::new(0),
            threshold: threshold.clamp(0.0, 1.0),
            // Preserve `0` as a disable sentinel — `insert()` short-circuits
            // when `max_entries == 0`. Do NOT coerce to `1`.
            max_entries,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
        }
    }

    /// Get the configured similarity threshold.
    pub fn threshold(&self) -> f32 {
        self.threshold
    }

    /// Get the configured max entries.
    pub fn max_entries(&self) -> usize {
        self.max_entries
    }

    /// Current number of stored embeddings.
    pub fn len(&self) -> usize {
        self.embeddings.len()
    }

    /// True if the cache has no entries.
    pub fn is_empty(&self) -> bool {
        self.embeddings.is_empty()
    }

    /// Number of semantic hits served.
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    /// Number of semantic misses (lookups with no match).
    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }

    /// Number of LRU evictions performed.
    pub fn evictions(&self) -> u64 {
        self.evictions.load(Ordering::Relaxed)
    }

    /// Look up the cache for a semantically similar past query.
    ///
    /// Returns the hash of the best matching entry if cosine similarity is
    /// `>= threshold`. Dimensions must match; if they don't, returns `None`
    /// and records a miss.
    pub fn lookup(&self, query_embedding: &[f32]) -> Option<QueryHash> {
        if self.embeddings.is_empty() {
            self.misses.fetch_add(1, Ordering::Relaxed);
            return None;
        }

        let mut best_hash: Option<QueryHash> = None;
        let mut best_sim = f32::NEG_INFINITY;
        let mut dim_mismatch = false;

        for entry in self.embeddings.iter() {
            let stored = &entry.value().1;
            if stored.len() != query_embedding.len() {
                dim_mismatch = true;
                continue;
            }
            let sim = cosine_similarity(query_embedding, stored);
            if sim > best_sim {
                best_sim = sim;
                best_hash = Some(*entry.key());
            }
        }

        if dim_mismatch && best_hash.is_none() {
            // No usable entry — dimensions don't line up at all
            self.misses.fetch_add(1, Ordering::Relaxed);
            return None;
        }

        if best_sim >= self.threshold {
            self.hits.fetch_add(1, Ordering::Relaxed);
            best_hash
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    /// Insert an embedding for a query hash.
    ///
    /// Triggers LRU eviction if the cache exceeds `max_entries`. Does nothing
    /// if the cache is disabled (`max_entries == 0`).
    ///
    /// Disabling via `max_entries = 0` is a sentinel; the constructor preserves
    /// it as `0` (does not coerce to `1`).
    pub fn insert(&self, hash: QueryHash, embedding: Vec<f32>) {
        if self.max_entries == 0 {
            return;
        }

        // Bump sequence for LRU ordering
        let seq = self.sequence.fetch_add(1, Ordering::Relaxed);

        // Insert the new entry first so the cache always holds the most
        // recent embedding for a hash, even if we evict something else.
        self.embeddings.insert(hash, Arc::new((seq, embedding)));

        // Evict if over capacity. Pick the entry with the lowest sequence
        // number (oldest insertion).
        if self.embeddings.len() > self.max_entries {
            self.evict_lru_one();
        }
    }

    /// Remove all entries.
    pub fn clear(&self) {
        self.embeddings.clear();
    }

    /// Evict the oldest entry by insertion sequence (true LRU).
    fn evict_lru_one(&self) {
        // Collect a victim key first, then drop the iterator before
        // mutating the map (DashMap's Ref must be released before remove).
        let victim = self
            .embeddings
            .iter()
            .min_by_key(|e| e.value().0)
            .map(|entry| *entry.key());
        if let Some(key) = victim {
            self.embeddings.remove(&key);
            self.evictions.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Snapshot all stored embeddings for the `distill` feature.
    ///
    /// Returns `(query_hash, embedding, insertion_seq)` tuples. The hash is
    /// used to join with `CacheStore::snapshot_entries_rich` output so the
    /// distill consumer can attach the embedding to the matching primary
    /// entry. Insertion sequence is preserved for caller-side ordering.
    pub fn snapshot(&self) -> Vec<(QueryHash, Vec<f32>, u64)> {
        self.embeddings
            .iter()
            .map(|entry| {
                let (seq, emb) = &**entry.value();
                (*entry.key(), emb.clone(), *seq)
            })
            .collect()
    }
}

/// Compute cosine similarity between two embedding vectors.
///
/// Returns 0.0 if either vector is zero-magnitude (no direction).
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "cosine_similarity: dim mismatch");
    let mut dot = 0.0_f64;
    let mut mag_a = 0.0_f64;
    let mut mag_b = 0.0_f64;
    for (x, y) in a.iter().zip(b.iter()) {
        let x = *x as f64;
        let y = *y as f64;
        dot += x * y;
        mag_a += x * x;
        mag_b += y * y;
    }
    let denom = (mag_a * mag_b).sqrt();
    if denom == 0.0 {
        0.0
    } else {
        (dot / denom) as f32
    }
}

/// Configuration for the semantic cache tier, deserialized from
/// `[proxy.cache.semantic]` in `conproxy.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SemanticCacheSettings {
    /// Enable semantic matching (default: false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Cosine similarity threshold for a match (default: 0.92).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub similarity_threshold: Option<f32>,
    /// Maximum number of stored embeddings (default: 10000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_entries: Option<usize>,
}

impl SemanticCacheSettings {
    /// `true` when semantic matching is enabled.
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(false)
    }

    /// Cosine similarity threshold (default: 0.92).
    pub fn similarity_threshold(&self) -> f32 {
        self.similarity_threshold
            .unwrap_or(DEFAULT_SIMILARITY_THRESHOLD)
    }

    /// Maximum stored embeddings (default: 10000).
    pub fn max_entries(&self) -> usize {
        self.max_entries.unwrap_or(DEFAULT_MAX_ENTRIES)
    }

    /// Merge with a base (global) config, local fields winning when set.
    pub fn merge_with(&self, base: &Self) -> Self {
        Self {
            enabled: self.enabled.or(base.enabled),
            similarity_threshold: self.similarity_threshold.or(base.similarity_threshold),
            max_entries: self.max_entries.or(base.max_entries),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_embedding(values: &[f32]) -> Vec<f32> {
        values.to_vec()
    }

    #[test]
    fn empty_cache_always_misses() {
        let cache = SemanticCache::new(0.9, 100);
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
        assert!(cache.lookup(&make_embedding(&[1.0, 0.0])).is_none());
        assert_eq!(cache.misses(), 1);
        assert_eq!(cache.hits(), 0);
    }

    #[test]
    fn exact_match_returns_hash() {
        let cache = SemanticCache::new(0.9, 100);
        let hash = [1u8; 32];
        let emb = make_embedding(&[1.0, 0.0, 0.0]);
        cache.insert(hash, emb.clone());
        let found = cache.lookup(&emb);
        assert_eq!(found, Some(hash));
        assert_eq!(cache.hits(), 1);
    }

    #[test]
    fn below_threshold_misses() {
        let cache = SemanticCache::new(0.99, 100);
        let hash = [1u8; 32];
        // Stored vector points along +X
        cache.insert(hash, make_embedding(&[1.0, 0.0]));
        // Query vector points along +Y (orthogonal → cosine 0.0)
        let result = cache.lookup(&make_embedding(&[0.0, 1.0]));
        assert!(result.is_none());
        assert_eq!(cache.misses(), 1);
    }

    #[test]
    fn near_orthogonal_above_low_threshold_hits() {
        let cache = SemanticCache::new(0.5, 100);
        let hash = [1u8; 32];
        cache.insert(hash, make_embedding(&[1.0, 0.0]));
        // 45° angle, cosine ~0.707 > 0.5
        let result = cache.lookup(&make_embedding(&[0.707, 0.707]));
        assert_eq!(result, Some(hash));
    }

    #[test]
    fn dimension_mismatch_returns_none() {
        let cache = SemanticCache::new(0.5, 100);
        cache.insert([1u8; 32], make_embedding(&[1.0, 0.0, 0.0]));
        // 2D query vs 3D stored — no usable candidate
        let result = cache.lookup(&make_embedding(&[1.0, 0.0]));
        assert!(result.is_none());
    }

    #[test]
    fn lru_eviction_keeps_capacity() {
        let cache = SemanticCache::new(0.5, 3);
        for i in 0..5u8 {
            cache.insert([i; 32], make_embedding(&[i as f32, 0.0]));
        }
        assert!(cache.len() <= 3, "len={} should be <= 3", cache.len());
        assert!(cache.evictions() >= 2);
    }

    #[test]
    fn zero_magnitude_returns_zero() {
        let sim = cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]);
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn identical_vectors_have_cosine_one() {
        let sim = cosine_similarity(&[1.0, 2.0, 3.0], &[1.0, 2.0, 3.0]);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn orthogonal_vectors_have_cosine_zero() {
        let sim = cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]);
        assert!(sim.abs() < 1e-6);
    }

    #[test]
    fn clear_empties_cache() {
        let cache = SemanticCache::new(0.5, 100);
        cache.insert([1u8; 32], make_embedding(&[1.0, 0.0]));
        assert!(!cache.is_empty());
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn settings_defaults() {
        let s = SemanticCacheSettings::default();
        assert!(!s.enabled());
        assert!((s.similarity_threshold() - 0.92).abs() < 1e-6);
        assert_eq!(s.max_entries(), 10_000);
    }

    #[test]
    fn settings_merge_local_wins() {
        let base = SemanticCacheSettings {
            enabled: Some(false),
            similarity_threshold: Some(0.8),
            max_entries: Some(5000),
        };
        let local = SemanticCacheSettings {
            enabled: Some(true),
            similarity_threshold: Some(0.95),
            max_entries: None,
        };
        let merged = local.merge_with(&base);
        assert!(merged.enabled());
        assert!((merged.similarity_threshold() - 0.95).abs() < 1e-6);
        assert_eq!(merged.max_entries(), 5000);
    }
}
