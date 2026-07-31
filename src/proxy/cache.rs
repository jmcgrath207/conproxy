//! Cache storage with papaya lock-free concurrent access and S3-FIFO eviction.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tracing::{debug, instrument, trace, warn};

use super::cdc::{CdcEventBuilder, CdcEventType, SharedEventSender};
use super::types::{CacheEntry, Freshness, QueryHash, QueryResponse, SchemaFingerprint};

#[cfg(feature = "persistence")]
use super::persistence::{current_time_ms, PersistedEntry, PersistedResponse, PersistentCache};

/// Default maximum frozen duration (24 hours).
pub const DEFAULT_MAX_FROZEN_DURATION: Duration = Duration::from_secs(24 * 60 * 60);

/// Indicates which cache tier provided a hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheHitType {
    /// Exact hash match (fastest - no normalization needed).
    Exact,
    /// Normalized hash match (query was normalized before matching).
    Normalized,
}

// --- S3-FIFO data structures ---

struct S3FifoMeta {
    hash: QueryHash,
}

struct S3FifoState {
    small: VecDeque<S3FifoMeta>,
    main: VecDeque<S3FifoMeta>,
    ghost: VecDeque<QueryHash>,
    ghost_set: HashSet<QueryHash>,
    small_capacity: usize,
    main_capacity: usize,
    ghost_capacity: usize,
}

impl S3FifoState {
    fn new(max_entries: usize) -> Self {
        let small_capacity = (max_entries / 10).max(1);
        let main_capacity = max_entries.saturating_sub(small_capacity);
        let ghost_capacity = max_entries.max(1);
        Self {
            small: VecDeque::with_capacity(small_capacity),
            main: VecDeque::with_capacity(main_capacity),
            ghost: VecDeque::with_capacity(ghost_capacity),
            ghost_set: HashSet::with_capacity(ghost_capacity),
            small_capacity,
            main_capacity,
            ghost_capacity,
        }
    }

    /// Number of entries in the ghost set.
    #[allow(dead_code)]
    pub(crate) fn ghost_set_size(&self) -> usize {
        self.ghost_set.len()
    }

    /// Number of entries in the small queue.
    #[allow(dead_code)]
    pub(crate) fn small_queue_size(&self) -> usize {
        self.small.len()
    }

    /// Number of entries in the main queue.
    #[allow(dead_code)]
    pub(crate) fn main_queue_size(&self) -> usize {
        self.main.len()
    }

    /// Check if a hash is in the ghost set.
    #[allow(dead_code)]
    pub(crate) fn is_in_ghost(&self, hash: &QueryHash) -> bool {
        self.ghost_set.contains(hash)
    }
}

/// Which S3-FIFO queue an eviction came from.
enum EvictionSource {
    Small,
    Main,
}

/// Rich snapshot of a cache entry, produced by
/// [`CacheStore::snapshot_entries_rich`] for the `distill` feature.
///
/// Unlike [`CacheStore::snapshot_entries`] which only returns
/// `(query, response_json, upstream_id)`, this carries the entry's hash,
/// context tag, wall-clock insertion time, and TTL-extension count so the
/// distill consumer can sort, filter, and join with the semantic tier.
#[derive(Debug, Clone)]
pub struct DistillSnapshot {
    /// blake3 query hash, the same key used by the primary cache.
    pub hash: QueryHash,
    /// Context ID the entry belongs to. `"default"` for entries that pre-date
    /// the per-entry context tagging introduced with the distill feature.
    pub context_id: String,
    /// Original query text, or empty string for legacy entries.
    pub query_text: String,
    /// Wall-clock insertion time (set when the entry was created or restored).
    /// Entries with `None` are skipped by `snapshot_entries_rich`.
    pub cached_at_wall: SystemTime,
    /// `serde_json` bytes of the `QueryResponse` payload.
    pub response_json: Vec<u8>,
    /// Upstream that produced the cached response.
    pub upstream_id: String,
    /// Number of TTL extensions applied to the entry.
    pub extended_count: u32,
}

/// Thread-safe cache store with lock-free reads (papaya) and S3-FIFO eviction.
pub struct CacheStore {
    /// The actual cache storage, keyed by normalized hash.
    /// Entries are wrapped in Arc for zero-copy cache hits.
    entries: papaya::HashMap<QueryHash, Arc<CacheEntry>>,
    /// Exact hash to normalized hash mapping for fast lookups.
    /// Avoids re-normalizing repeated identical queries.
    exact_to_normalized: papaya::HashMap<QueryHash, QueryHash>,
    /// Duration before an entry becomes stale. Behind a Mutex for hot-reload.
    fresh_duration: parking_lot::Mutex<Duration>,
    /// Duration before a stale entry expires completely. Behind a Mutex for hot-reload.
    stale_duration: parking_lot::Mutex<Duration>,
    /// Maximum age for frozen entries (after which they cannot be served even as frozen).
    max_frozen_duration: Duration,
    /// Maximum number of entries (for eviction). Atomic for hot-reload.
    max_entries: std::sync::atomic::AtomicUsize,
    /// Maximum memory in bytes (0 = no limit).
    max_memory_bytes: usize,
    /// TTL jitter percentage (0.0 to 1.0).
    ttl_jitter_percent: f32,
    /// Fingerprint of relevant config (for cache invalidation on config change).
    config_fingerprint: AtomicU64,
    /// Eviction statistics.
    eviction_stats: parking_lot::Mutex<EvictionStats>,
    /// Maximum entries per upstream (None = no limit).
    per_upstream_limit: Option<usize>,
    /// Whether normalized matching is enabled (two-tier cache).
    /// When false, only exact hash matching is used (faster, less memory).
    normalized_matching: bool,
    /// Counter for exact hash hits.
    exact_hits: AtomicU64,
    /// Counter for normalized hash hits.
    normalized_hits: AtomicU64,
    /// Running total of approximate memory usage (updated on insert/remove).
    total_memory_bytes: AtomicUsize,
    /// Optional persistent cache for disk-backed storage.
    #[cfg(feature = "persistence")]
    persistence: Option<Arc<PersistentCache>>,
    /// Optional CDC event sender for cache mutation broadcast.
    cdc_sender: Option<SharedEventSender>,
    /// S3-FIFO eviction state (small queue, main queue, ghost set).
    s3fifo: parking_lot::Mutex<S3FifoState>,
    /// Optional semantic cache tier for embedding-similarity matching.
    /// When `None`, all semantic operations short-circuit (zero overhead).
    #[cfg(feature = "embed-api")]
    semantic: Option<Arc<super::semantic_cache::SemanticCache>>,
}

impl CacheStore {
    /// Create a new cache store with the given configuration.
    pub fn new(fresh_duration: Duration, stale_duration: Duration, max_entries: usize) -> Self {
        Self {
            entries: papaya::HashMap::new(),
            exact_to_normalized: papaya::HashMap::new(),
            fresh_duration: parking_lot::Mutex::new(fresh_duration),
            stale_duration: parking_lot::Mutex::new(stale_duration),
            max_frozen_duration: DEFAULT_MAX_FROZEN_DURATION,
            max_entries: std::sync::atomic::AtomicUsize::new(max_entries),
            max_memory_bytes: 0,     // No limit
            ttl_jitter_percent: 0.1, // Default 10% jitter
            config_fingerprint: AtomicU64::new(0),
            eviction_stats: parking_lot::Mutex::new(EvictionStats::default()),
            per_upstream_limit: None,
            normalized_matching: false,
            exact_hits: AtomicU64::new(0),
            normalized_hits: AtomicU64::new(0),
            total_memory_bytes: AtomicUsize::new(0),
            #[cfg(feature = "persistence")]
            persistence: None,
            cdc_sender: None,
            s3fifo: parking_lot::Mutex::new(S3FifoState::new(max_entries)),
            #[cfg(feature = "embed-api")]
            semantic: None,
        }
    }

    /// Create a new cache store with TTL jittering enabled.
    pub fn with_jitter(
        fresh_duration: Duration,
        stale_duration: Duration,
        max_entries: usize,
        ttl_jitter_percent: f32,
    ) -> Self {
        Self {
            entries: papaya::HashMap::new(),
            exact_to_normalized: papaya::HashMap::new(),
            fresh_duration: parking_lot::Mutex::new(fresh_duration),
            stale_duration: parking_lot::Mutex::new(stale_duration),
            max_frozen_duration: DEFAULT_MAX_FROZEN_DURATION,
            max_entries: std::sync::atomic::AtomicUsize::new(max_entries),
            max_memory_bytes: 0,
            ttl_jitter_percent: ttl_jitter_percent.clamp(0.0, 1.0),
            config_fingerprint: AtomicU64::new(0),
            eviction_stats: parking_lot::Mutex::new(EvictionStats::default()),
            per_upstream_limit: None,
            normalized_matching: false,
            exact_hits: AtomicU64::new(0),
            normalized_hits: AtomicU64::new(0),
            total_memory_bytes: AtomicUsize::new(0),
            #[cfg(feature = "persistence")]
            persistence: None,
            cdc_sender: None,
            s3fifo: parking_lot::Mutex::new(S3FifoState::new(max_entries)),
            #[cfg(feature = "embed-api")]
            semantic: None,
        }
    }

    /// Create a cache store with memory limit.
    pub fn with_memory_limit(
        fresh_duration: Duration,
        stale_duration: Duration,
        max_entries: usize,
        max_memory_bytes: usize,
    ) -> Self {
        Self {
            entries: papaya::HashMap::new(),
            exact_to_normalized: papaya::HashMap::new(),
            fresh_duration: parking_lot::Mutex::new(fresh_duration),
            stale_duration: parking_lot::Mutex::new(stale_duration),
            max_frozen_duration: DEFAULT_MAX_FROZEN_DURATION,
            max_entries: std::sync::atomic::AtomicUsize::new(max_entries),
            max_memory_bytes,
            ttl_jitter_percent: 0.1,
            config_fingerprint: AtomicU64::new(0),
            eviction_stats: parking_lot::Mutex::new(EvictionStats::default()),
            per_upstream_limit: None,
            normalized_matching: false,
            exact_hits: AtomicU64::new(0),
            normalized_hits: AtomicU64::new(0),
            total_memory_bytes: AtomicUsize::new(0),
            #[cfg(feature = "persistence")]
            persistence: None,
            cdc_sender: None,
            s3fifo: parking_lot::Mutex::new(S3FifoState::new(max_entries)),
            #[cfg(feature = "embed-api")]
            semantic: None,
        }
    }

    /// Set the per-upstream entry limit.
    ///
    /// When set, prevents any single upstream from having more than this many
    /// entries in the cache. Oldest entries for the upstream will be evicted.
    pub fn set_per_upstream_limit(&mut self, limit: Option<usize>) {
        self.per_upstream_limit = limit;
    }

    /// Get the per-upstream limit if set.
    pub fn per_upstream_limit(&self) -> Option<usize> {
        self.per_upstream_limit
    }

    /// Atomically set the maximum number of entries (for hot-reload).
    ///
    /// # Errors
    /// Returns `Err` if `max_entries == 0`.
    pub fn set_max_entries(&self, max_entries: usize) -> Result<(), String> {
        if max_entries == 0 {
            return Err("cache.max_entries must be > 0".to_string());
        }
        self.max_entries
            .store(max_entries, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// Get the current maximum entries (atomic read).
    pub fn max_entries(&self) -> usize {
        self.max_entries.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Enable or disable normalized matching (two-tier cache).
    ///
    /// When enabled, queries are normalized (lowercased, whitespace collapsed) and
    /// the exact→normalized mapping is maintained. When disabled, only exact hash
    /// matching is used, saving memory and CPU.
    pub fn with_normalized_matching(mut self, enabled: bool) -> Self {
        self.normalized_matching = enabled;
        self
    }

    /// Check if normalized matching is enabled.
    pub fn normalized_matching(&self) -> bool {
        self.normalized_matching
    }

    /// Set the CDC event sender for broadcasting cache mutations.
    ///
    /// When set, insert/remove/clear operations publish CDC events.
    /// Broadcast send is non-blocking; no receivers = no-op.
    pub fn with_cdc_sender(mut self, sender: SharedEventSender) -> Self {
        self.cdc_sender = Some(sender);
        self
    }

    /// Attach a semantic cache tier for embedding-similarity lookups.
    ///
    /// When set, `lookup_semantic` performs a cosine-similarity scan
    /// across stored embeddings, and inserts record the new embedding
    /// alongside the cache entry.
    #[cfg(feature = "embed-api")]
    pub fn with_semantic_cache(
        mut self,
        semantic: Arc<super::semantic_cache::SemanticCache>,
    ) -> Self {
        self.semantic = Some(semantic);
        self
    }

    /// Get a reference to the semantic cache tier (if configured).
    #[cfg(feature = "embed-api")]
    pub fn semantic_cache(&self) -> Option<&Arc<super::semantic_cache::SemanticCache>> {
        self.semantic.as_ref()
    }

    /// Get the CDC sender (if configured).
    pub fn cdc_sender(&self) -> Option<&SharedEventSender> {
        self.cdc_sender.as_ref()
    }

    /// Set the maximum frozen duration.
    pub fn set_max_frozen_duration(&mut self, duration: Duration) {
        self.max_frozen_duration = duration;
    }

    /// Set the maximum memory in bytes for cache storage.
    ///
    /// When non-zero, the S3-FIFO admission path will evict entries
    /// until total memory usage drops below this limit.
    pub fn set_max_memory_bytes(&mut self, bytes: usize) {
        self.max_memory_bytes = bytes;
    }

    // --- Memory-tracking wrappers (take a papaya Guard) ---

    /// Insert an entry into the map and update the memory tracker.
    fn tracked_insert(&self, hash: QueryHash, entry: Arc<CacheEntry>, guard: &impl papaya::Guard) {
        let new_size = entry.approximate_size();
        let old = self.entries.insert(hash, entry, guard);
        if let Some(old_entry) = old {
            let old_size = old_entry.approximate_size();
            if new_size >= old_size {
                self.total_memory_bytes
                    .fetch_add(new_size.saturating_sub(old_size), Ordering::Relaxed);
            } else {
                self.total_memory_bytes
                    .fetch_sub(old_size.saturating_sub(new_size), Ordering::Relaxed);
            }
        } else {
            self.total_memory_bytes
                .fetch_add(new_size, Ordering::Relaxed);
        }
    }

    /// Remove an entry from the map and update the memory tracker.
    ///
    /// When normalized matching is enabled, also cleans up the exact→normalized
    /// hash mapping to prevent memory leaks from stale mappings after eviction.
    fn tracked_remove(
        &self,
        hash: &QueryHash,
        guard: &impl papaya::Guard,
    ) -> Option<Arc<CacheEntry>> {
        if let Some(old) = self.entries.remove(hash, guard) {
            let size = old.approximate_size();
            self.total_memory_bytes.fetch_sub(size, Ordering::Relaxed);

            // P-001/P-002: Clean up exact→normalized mapping to prevent memory leak.
            // Without this, evicted entries leave stale mappings that grow unboundedly.
            if self.normalized_matching {
                if let Some(ref query_text) = old.query_text {
                    let exact_hash = Self::hash_query_exact(query_text);
                    let e2n_guard = self.exact_to_normalized.guard();
                    self.exact_to_normalized.remove(&exact_hash, &e2n_guard);
                }
            }

            Some(Arc::clone(old))
        } else {
            None
        }
    }

    // --- S3-FIFO eviction methods ---

    /// Admit a newly inserted entry into the S3-FIFO queues.
    ///
    /// If the entry's hash is in the ghost set (previously evicted), it goes
    /// directly to the Main queue. Otherwise it enters the Small queue.
    /// Eviction is triggered if total queue size exceeds capacity.
    fn s3fifo_admit(&self, hash: QueryHash, guard: &impl papaya::Guard) {
        let mut state = self.s3fifo.lock();
        let was_ghost = state.ghost_set.remove(&hash);
        if was_ghost {
            state.ghost.retain(|h| *h != hash);
            state.main.push_back(S3FifoMeta { hash });
        } else {
            state.small.push_back(S3FifoMeta { hash });
        }

        let mut small_evictions = 0usize;
        let mut main_evictions = 0usize;

        while state.small.len().saturating_add(state.main.len())
            > state.small_capacity.saturating_add(state.main_capacity)
        {
            match self.s3fifo_evict_one(&mut state, guard) {
                Some(EvictionSource::Small) => small_evictions = small_evictions.saturating_add(1),
                Some(EvictionSource::Main) => main_evictions = main_evictions.saturating_add(1),
                None => break,
            }
        }

        // Memory-pressure eviction: keep evicting until under the memory limit
        if self.max_memory_bytes > 0 {
            while self.total_memory_bytes.load(Ordering::Relaxed) > self.max_memory_bytes {
                match self.s3fifo_evict_one(&mut state, guard) {
                    Some(EvictionSource::Small) => {
                        small_evictions = small_evictions.saturating_add(1)
                    }
                    Some(EvictionSource::Main) => main_evictions = main_evictions.saturating_add(1),
                    None => break,
                }
            }
        }

        // Update stats (still under s3fifo lock — consistent ordering: s3fifo → eviction_stats)
        if was_ghost || small_evictions > 0 || main_evictions > 0 {
            let mut stats = self.eviction_stats.lock();
            if was_ghost {
                stats.ghost_hits = stats.ghost_hits.saturating_add(1);
            }
            stats.small_evictions = stats.small_evictions.saturating_add(small_evictions);
            stats.main_evictions = stats.main_evictions.saturating_add(main_evictions);
            stats.total = stats
                .total
                .saturating_add(small_evictions.saturating_add(main_evictions));
        }
    }

    /// Try to evict one entry from the S3-FIFO queues.
    /// Prefers evicting from Small; falls back to Main.
    fn s3fifo_evict_one(
        &self,
        state: &mut S3FifoState,
        guard: &impl papaya::Guard,
    ) -> Option<EvictionSource> {
        if !state.small.is_empty() {
            if let Some(src) = self.s3fifo_evict_from_small(state, guard) {
                return Some(src);
            }
        }
        if !state.main.is_empty() {
            return self.s3fifo_evict_from_main(state, guard);
        }
        None
    }

    /// Try to evict one entry from the Small queue.
    ///
    /// Pop entries from the head. If freq == 0, evict from the map and add
    /// to ghost. If freq > 0, promote to Main (counts as a promotion, not
    /// an eviction — keep trying).
    fn s3fifo_evict_from_small(
        &self,
        state: &mut S3FifoState,
        guard: &impl papaya::Guard,
    ) -> Option<EvictionSource> {
        let max_attempts = state.small.len();
        let mut promotions = 0usize;
        for _ in 0..max_attempts {
            let meta = state.small.pop_front()?;
            let freq = self.read_and_reset_freq(&meta.hash, guard);
            if freq == 0 {
                // Cold entry: evict from map, add to ghost
                if self.tracked_remove(&meta.hash, guard).is_some() {
                    self.s3fifo_add_ghost(state, meta.hash);
                    if promotions > 0 {
                        let mut stats = self.eviction_stats.lock();
                        stats.promotions = stats.promotions.saturating_add(promotions);
                    }
                    return Some(EvictionSource::Small);
                }
                // Entry was already gone (TTL/explicit remove) — ghost it and keep trying
                self.s3fifo_add_ghost(state, meta.hash);
            } else {
                // Hot entry: promote to Main
                state.main.push_back(S3FifoMeta { hash: meta.hash });
                promotions = promotions.saturating_add(1);
            }
        }
        if promotions > 0 {
            let mut stats = self.eviction_stats.lock();
            stats.promotions = stats.promotions.saturating_add(promotions);
        }
        None
    }

    /// Try to evict one entry from the Main queue.
    ///
    /// Pop entries from the head. If freq == 0, evict. If freq > 0,
    /// re-enqueue at the tail (second chance).
    fn s3fifo_evict_from_main(
        &self,
        state: &mut S3FifoState,
        guard: &impl papaya::Guard,
    ) -> Option<EvictionSource> {
        let max_attempts = state.main.len();
        for _ in 0..max_attempts {
            let meta = state.main.pop_front()?;
            let freq = self.read_and_reset_freq(&meta.hash, guard);
            if freq == 0 {
                // Cold entry: evict
                if self.tracked_remove(&meta.hash, guard).is_some() {
                    return Some(EvictionSource::Main);
                }
                // Entry was already gone — keep trying
            } else {
                // Hot entry: re-enqueue at tail
                state.main.push_back(S3FifoMeta { hash: meta.hash });
            }
        }
        None
    }

    /// Read the freq counter for an entry and reset it to 0.
    fn read_and_reset_freq(&self, hash: &QueryHash, guard: &impl papaya::Guard) -> u8 {
        if let Some(entry) = self.entries.get(hash, guard) {
            entry.freq.swap(0, Ordering::Relaxed)
        } else {
            0
        }
    }

    /// Add a hash to the bounded ghost queue.
    fn s3fifo_add_ghost(&self, state: &mut S3FifoState, hash: QueryHash) {
        while state.ghost.len() >= state.ghost_capacity {
            if let Some(old) = state.ghost.pop_front() {
                state.ghost_set.remove(&old);
            }
        }
        state.ghost.push_back(hash);
        state.ghost_set.insert(hash);
    }

    /// Bump the frequency counter on access (capped at 3).
    #[inline]
    fn bump_freq(entry: &CacheEntry) {
        entry
            .freq
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |f| {
                Some(f.saturating_add(1).min(3))
            })
            .ok();
    }

    // --- Hash / normalization helpers ---

    /// Compute a jittered TTL to prevent thundering herd.
    ///
    /// Uses the query hash for deterministic jitter (same query always gets same jitter).
    pub fn jittered_ttl(&self, base_ttl: Duration, query_hash: &QueryHash) -> Duration {
        if self.ttl_jitter_percent <= 0.0 {
            return base_ttl;
        }

        // Use first byte of hash for deterministic jitter factor (0.0 to 1.0)
        let hash_byte = query_hash.first().copied().unwrap_or(0);
        let jitter_factor = (hash_byte as f32 / 255.0) * self.ttl_jitter_percent;
        let jitter_secs = (base_ttl.as_secs_f32() * jitter_factor) as u64;

        #[allow(clippy::arithmetic_side_effects)]
        let result = base_ttl + Duration::from_secs(jitter_secs);
        result
    }

    /// Compute a configuration fingerprint for cache invalidation.
    pub fn compute_config_fingerprint(upstream_url: Option<&str>, scope_seeds: &[String]) -> u64 {
        let mut hasher = blake3::Hasher::new();

        if let Some(url) = upstream_url {
            hasher.update(url.as_bytes());
        }
        hasher.update(b"\0");

        for seed in scope_seeds {
            hasher.update(seed.as_bytes());
            hasher.update(b"\0");
        }

        let hash = hasher.finalize();
        u64::from_le_bytes(hash.as_bytes()[..8].try_into().unwrap_or([0u8; 8]))
    }

    /// Update the config fingerprint and invalidate cache if it changed.
    ///
    /// Returns `true` if the cache was invalidated.
    pub fn update_config_fingerprint(
        &self,
        upstream_url: Option<&str>,
        scope_seeds: &[String],
    ) -> bool {
        let new_fingerprint = Self::compute_config_fingerprint(upstream_url, scope_seeds);
        let old = self
            .config_fingerprint
            .swap(new_fingerprint, Ordering::SeqCst);

        if old != 0 && old != new_fingerprint {
            let guard = self.entries.guard();
            self.entries.clear(&guard);
            let e2n_guard = self.exact_to_normalized.guard();
            self.exact_to_normalized.clear(&e2n_guard);
            self.total_memory_bytes.store(0, Ordering::Relaxed);

            // Clear S3-FIFO state
            let mut state = self.s3fifo.lock();
            state.small.clear();
            state.main.clear();
            state.ghost.clear();
            state.ghost_set.clear();

            true
        } else {
            false
        }
    }

    /// Get the current config fingerprint.
    pub fn config_fingerprint(&self) -> u64 {
        self.config_fingerprint.load(Ordering::SeqCst)
    }

    /// Get the TTL jitter percentage.
    pub fn ttl_jitter_percent(&self) -> f32 {
        self.ttl_jitter_percent
    }

    /// Normalize a query string for consistent cache keys.
    ///
    /// - Converts to lowercase
    /// - Trims leading/trailing whitespace
    /// - Collapses multiple whitespace to single spaces
    pub fn normalize_query(query: &str) -> String {
        let trimmed = query.trim().to_lowercase();
        let mut result = String::with_capacity(trimmed.len());
        let mut prev_was_space = false;

        for c in trimmed.chars() {
            if c.is_whitespace() {
                if !prev_was_space {
                    result.push(' ');
                    prev_was_space = true;
                }
            } else {
                result.push(c);
                prev_was_space = false;
            }
        }

        result
    }

    /// Compute a blake3 hash for the normalized query.
    pub fn hash_query(query: &str) -> QueryHash {
        let normalized = Self::normalize_query(query);
        *blake3::hash(normalized.as_bytes()).as_bytes()
    }

    /// Compute a blake3 hash for the exact (non-normalized) query.
    ///
    /// Used for the two-tier cache: first check exact hash, then normalized.
    pub fn hash_query_exact(query: &str) -> QueryHash {
        *blake3::hash(query.as_bytes()).as_bytes()
    }

    /// Get the number of exact cache hits (no normalization needed).
    pub fn exact_hits(&self) -> u64 {
        self.exact_hits.load(Ordering::Relaxed)
    }

    /// Get the number of normalized cache hits (required normalization).
    pub fn normalized_hits(&self) -> u64 {
        self.normalized_hits.load(Ordering::Relaxed)
    }

    // --- Lookup methods ---

    /// Get an entry from the cache if it exists.
    ///
    /// Uses two-tier lookup when normalized matching is enabled:
    /// 1. First checks exact hash to see if we've seen this exact query before
    /// 2. If not, computes normalized hash and checks entries
    /// 3. Stores exact→normalized mapping on hit for future fast lookups
    #[instrument(skip(self), fields(query_len = query.len()))]
    pub fn get(&self, query: &str) -> Option<Arc<CacheEntry>> {
        if self.normalized_matching {
            return self.get_two_tier(query);
        }

        // Direct lookup by normalized hash only
        let normalized_hash = Self::hash_query(query);
        let guard = self.entries.guard();
        self.entries.get(&normalized_hash, &guard).map(|entry| {
            Self::bump_freq(entry);
            self.exact_hits.fetch_add(1, Ordering::Relaxed);
            trace!(hit_type = "direct", "Cache lookup");
            Arc::clone(entry)
        })
    }

    /// Two-tier lookup: exact hash → normalized hash → entry.
    fn get_two_tier(&self, query: &str) -> Option<Arc<CacheEntry>> {
        let exact_hash = Self::hash_query_exact(query);
        let guard = self.entries.guard();
        let e2n_guard = self.exact_to_normalized.guard();

        // Tier 1: Check if we have a cached exact→normalized mapping
        if let Some(&normalized_hash) = self.exact_to_normalized.get(&exact_hash, &e2n_guard) {
            if let Some(entry) = self.entries.get(&normalized_hash, &guard) {
                Self::bump_freq(entry);
                self.exact_hits.fetch_add(1, Ordering::Relaxed);
                trace!(hit_type = "exact", "Cache lookup");
                return Some(Arc::clone(entry));
            }
        }

        // Tier 2: Compute normalized hash and check entries
        let normalized_hash = Self::hash_query(query);
        let result = self.entries.get(&normalized_hash, &guard).map(|entry| {
            // Store exact→normalized mapping for future fast lookups
            self.exact_to_normalized
                .insert(exact_hash, normalized_hash, &e2n_guard);
            Self::bump_freq(entry);
            self.normalized_hits.fetch_add(1, Ordering::Relaxed);
            trace!(hit_type = "normalized", "Cache lookup");
            Arc::clone(entry)
        });

        if result.is_none() {
            trace!(hit_type = "miss", "Cache lookup");
        }
        result
    }

    /// Get an entry from the cache with the hit type (Exact or Normalized).
    ///
    /// Returns `None` if no entry was found, or `Some((entry, hit_type))`.
    /// Useful for diagnostics to understand cache behavior.
    #[instrument(skip(self), fields(query_len = query.len()))]
    pub fn get_with_hit_type(&self, query: &str) -> Option<(Arc<CacheEntry>, CacheHitType)> {
        let guard = self.entries.guard();

        if !self.normalized_matching {
            // Direct lookup only
            let normalized_hash = Self::hash_query(query);
            return self.entries.get(&normalized_hash, &guard).map(|entry| {
                Self::bump_freq(entry);
                self.exact_hits.fetch_add(1, Ordering::Relaxed);
                (Arc::clone(entry), CacheHitType::Exact)
            });
        }

        let exact_hash = Self::hash_query_exact(query);
        let e2n_guard = self.exact_to_normalized.guard();

        // Tier 1: Check if we have a cached exact→normalized mapping
        if let Some(&normalized_hash) = self.exact_to_normalized.get(&exact_hash, &e2n_guard) {
            if let Some(entry) = self.entries.get(&normalized_hash, &guard) {
                Self::bump_freq(entry);
                self.exact_hits.fetch_add(1, Ordering::Relaxed);
                return Some((Arc::clone(entry), CacheHitType::Exact));
            }
        }

        // Tier 2: Compute normalized hash and check entries
        let normalized_hash = Self::hash_query(query);
        self.entries.get(&normalized_hash, &guard).map(|entry| {
            // Store exact→normalized mapping for future fast lookups
            self.exact_to_normalized
                .insert(exact_hash, normalized_hash, &e2n_guard);
            Self::bump_freq(entry);
            self.normalized_hits.fetch_add(1, Ordering::Relaxed);
            (Arc::clone(entry), CacheHitType::Normalized)
        })
    }

    /// Get an entry with integrity verification.
    ///
    /// If the entry has a content hash and it doesn't match, the entry
    /// is evicted and `None` is returned.
    #[instrument(skip(self), fields(query_len = query.len()))]
    pub fn get_verified(&self, query: &str) -> Option<Arc<CacheEntry>> {
        let hash = Self::hash_query(query);
        let guard = self.entries.guard();

        if let Some(entry) = self.entries.get(&hash, &guard) {
            if let Some(stored_hash) = entry.content_hash {
                let computed_hash = CacheEntry::compute_content_hash(&entry.response);
                if stored_hash != computed_hash {
                    // Integrity check failed - evict the entry
                    warn!("Cache integrity check failed, evicting entry");
                    self.tracked_remove(&hash, &guard);
                    let mut stats = self.eviction_stats.lock();
                    stats.integrity_failures = stats.integrity_failures.saturating_add(1);
                    return None;
                }
            }

            Self::bump_freq(entry);
            trace!("Cache hit with integrity verification");
            return Some(Arc::clone(entry));
        }

        trace!("Cache miss");
        None
    }

    /// Get an entry by hash.
    pub fn get_by_hash(&self, hash: &QueryHash) -> Option<Arc<CacheEntry>> {
        let guard = self.entries.guard();
        self.entries.get(hash, &guard).map(|entry| {
            Self::bump_freq(entry);
            Arc::clone(entry)
        })
    }

    // --- Insert methods ---

    /// Insert a new entry into the cache.
    ///
    /// Also stores the exact→normalized hash mapping for two-tier lookups.
    /// Convenience wrapper that stores `context_id` as `"default"`.
    /// Prefer [`insert_with_context`] when the context is known.
    #[instrument(skip(self, response), fields(query_len = query.len(), upstream = %upstream_id))]
    pub fn insert(&self, query: &str, response: QueryResponse, upstream_id: String) {
        self.insert_impl(query, response, upstream_id, "default");
    }

    /// Insert a new entry with explicit context_id.
    ///
    /// Same as [`insert`] but records the actual context for metadata/CDC filtering.
    #[instrument(skip(self, response), fields(query_len = query.len(), upstream = %upstream_id, context = %context_id))]
    pub fn insert_with_context(
        &self,
        query: &str,
        response: QueryResponse,
        upstream_id: String,
        context_id: &str,
    ) {
        self.insert_impl(query, response, upstream_id, context_id);
    }

    /// Internal insert implementation shared by `insert` and `insert_with_context`.
    fn insert_impl(
        &self,
        query: &str,
        response: QueryResponse,
        upstream_id: String,
        context_id: &str,
    ) {
        let normalized_hash = Self::hash_query(query);
        // Skip exact hash computation when normalized matching is disabled
        let exact_hash = if self.normalized_matching {
            Self::hash_query_exact(query)
        } else {
            [0u8; 32] // unused sentinel — no two-tier lookup
        };
        let guard = self.entries.guard();

        // Check per-upstream limit before inserting
        if let Some(limit) = self.per_upstream_limit {
            let upstream_count = self.count_for_upstream_inner(&upstream_id, &guard);
            if upstream_count >= limit {
                debug!(upstream = %upstream_id, "Per-upstream limit reached, evicting");
                self.evict_from_upstream_inner(&upstream_id, 1, &guard);
                let mut stats = self.eviction_stats.lock();
                stats.per_upstream = stats.per_upstream.saturating_add(1);
            }
        }

        // Check if this is a genuinely new entry (not an update of existing key)
        let is_new = !self.entries.contains_key(&normalized_hash, &guard);

        // Serialize response for CDC before moving into CacheEntry
        let (cdc_payload, cdc_upstream_id) = if self.cdc_sender.is_some() {
            (
                serde_json::to_vec(&response).ok(),
                Some(upstream_id.clone()),
            )
        } else {
            (None, None)
        };

        let content_hash = Some(CacheEntry::compute_content_hash(&response));
        let entry = CacheEntry {
            response,
            upstream_id,
            cached_at: Instant::now(),
            cached_at_wall: Some(SystemTime::now()),
            extended_count: 0,
            schema: SchemaFingerprint::default(),
            content_hash,
            query_text: Some(query.to_string()),
            context_id: context_id.to_string(),
            freq: AtomicU8::new(0),
        };

        // Persist to disk if configured
        #[cfg(feature = "persistence")]
        self.persist_entry(&normalized_hash, &entry);

        self.tracked_insert(normalized_hash, Arc::new(entry), &guard);

        // Store exact→normalized mapping for two-tier lookups (only when enabled)
        if self.normalized_matching {
            let e2n_guard = self.exact_to_normalized.guard();
            self.exact_to_normalized
                .insert(exact_hash, normalized_hash, &e2n_guard);
        }

        // Admit to S3-FIFO (handles eviction automatically)
        if is_new {
            self.s3fifo_admit(normalized_hash, &guard);
        }

        // Publish CDC INSERT event
        if let Some(ref sender) = self.cdc_sender {
            if let Some(payload) = cdc_payload {
                // R2: cdc_upstream_id only cloned when CDC is enabled
                sender.send(
                    CdcEventType::Insert,
                    CdcEventBuilder::insert(
                        query.to_string(),
                        payload,
                        cdc_upstream_id.unwrap_or_default(),
                    ),
                );
            }
        }

        // Semantic tier: store the normalized hash → embedding mapping
        // (query text isn't passed here; semantic stores keyed by hash only
        // and accepts the embedding as a separate call from the caller).
        // This is intentionally a no-op for `insert` — see
        // `insert_by_hash_with_embedding` for the hash-keyed path.
    }

    /// Insert an entry by hash.
    /// Insert by precomputed hash. When `query_key` is set, store it on the
    /// entry and emit a CDC INSERT (required for peer replication — peers
    /// re-hash `query_key` on apply).
    pub fn insert_by_hash(
        &self,
        hash: QueryHash,
        response: QueryResponse,
        upstream_id: String,
        query_key: Option<&str>,
    ) {
        let guard = self.entries.guard();

        // Check per-upstream limit before inserting
        if let Some(limit) = self.per_upstream_limit {
            let upstream_count = self.count_for_upstream_inner(&upstream_id, &guard);
            if upstream_count >= limit {
                self.evict_from_upstream_inner(&upstream_id, 1, &guard);
                let mut stats = self.eviction_stats.lock();
                stats.per_upstream = stats.per_upstream.saturating_add(1);
            }
        }

        let is_new = !self.entries.contains_key(&hash, &guard);

        // Serialize for CDC before moving response into the entry
        let cdc_payload = if self.cdc_sender.is_some() && query_key.is_some() {
            serde_json::to_vec(&response).ok()
        } else {
            None
        };
        let cdc_upstream = if cdc_payload.is_some() {
            Some(upstream_id.clone())
        } else {
            None
        };

        let content_hash = Some(CacheEntry::compute_content_hash(&response));
        let entry = CacheEntry {
            response,
            upstream_id,
            cached_at: Instant::now(),
            cached_at_wall: Some(SystemTime::now()),
            extended_count: 0,
            schema: SchemaFingerprint::default(),
            content_hash,
            query_text: query_key.map(str::to_string),
            context_id: "default".to_string(),
            freq: AtomicU8::new(0),
        };

        // Persist to disk if configured
        #[cfg(feature = "persistence")]
        self.persist_entry(&hash, &entry);

        self.tracked_insert(hash, Arc::new(entry), &guard);

        if is_new {
            self.s3fifo_admit(hash, &guard);
        }

        // Peer CDC: same path as insert_impl — key must be original query text
        if let (Some(ref sender), Some(payload), Some(key)) =
            (&self.cdc_sender, cdc_payload, query_key)
        {
            sender.send(
                CdcEventType::Insert,
                CdcEventBuilder::insert(key.to_string(), payload, cdc_upstream.unwrap_or_default()),
            );
        }
    }

    /// Insert an entry by hash, plus an embedding for the semantic cache tier.
    ///
    /// When the semantic cache is configured and `embedding` is `Some`, the
    /// embedding is stored under `hash` for later similarity lookups.
    /// When the semantic cache is disabled, this is equivalent to
    /// `insert_by_hash` and the embedding is dropped.
    #[cfg(feature = "embed-api")]
    pub fn insert_by_hash_with_embedding(
        &self,
        hash: QueryHash,
        response: QueryResponse,
        upstream_id: String,
        embedding: Option<Vec<f32>>,
        query_key: Option<&str>,
    ) {
        self.insert_by_hash(hash, response, upstream_id, query_key);
        if let (Some(emb), Some(semantic)) = (embedding, self.semantic.as_ref()) {
            semantic.insert(hash, emb);
        }
    }

    /// Batch insert multiple entries for cache warmup.
    ///
    /// Returns the number of entries successfully inserted.
    /// Stops when the cache reaches max_entries to avoid wasteful insert-then-evict cycles.
    pub fn warmup(&self, entries: Vec<(String, QueryResponse)>, upstream_id: String) -> usize {
        let guard = self.entries.guard();
        let mut inserted: usize = 0;

        for (query, response) in entries.into_iter() {
            // Skip if response is invalid
            if response.validate().is_err() {
                continue;
            }

            // Respect max_entries — no point warming past capacity
            if self.entries.len() >= self.max_entries.load(Ordering::Relaxed) {
                break;
            }

            let hash = Self::hash_query(&query);
            let is_new = !self.entries.contains_key(&hash, &guard);
            let content_hash = Some(CacheEntry::compute_content_hash(&response));

            self.tracked_insert(
                hash,
                Arc::new(CacheEntry {
                    response,
                    upstream_id: upstream_id.clone(),
                    cached_at: Instant::now(),
                    cached_at_wall: Some(SystemTime::now()),
                    extended_count: 0,
                    schema: SchemaFingerprint::default(),
                    content_hash,
                    query_text: Some(query),
                    context_id: "default".to_string(),
                    freq: AtomicU8::new(0),
                }),
                &guard,
            );

            if is_new {
                self.s3fifo_admit(hash, &guard);
            }
            inserted = inserted.saturating_add(1);
        }

        inserted
    }

    // --- Freshness / TTL ---

    /// Check the freshness of a cached entry.
    pub fn check_freshness(&self, query: &str) -> Option<Freshness> {
        let hash = Self::hash_query(query);
        self.check_freshness_by_hash(&hash)
    }

    /// Check the freshness of a cached entry by hash.
    pub fn check_freshness_by_hash(&self, hash: &QueryHash) -> Option<Freshness> {
        let guard = self.entries.guard();
        self.entries.get(hash, &guard).map(|entry| {
            let age = entry.cached_at.elapsed();

            let fresh = *self.fresh_duration.lock();
            let stale = *self.stale_duration.lock();
            let jittered_fresh = self.jittered_ttl(fresh, hash);
            #[allow(clippy::arithmetic_side_effects)]
            let jittered_total = jittered_fresh + stale;

            if age < jittered_fresh {
                Freshness::Fresh
            } else if age < jittered_total {
                Freshness::Stale
            } else if age < self.max_frozen_duration {
                Freshness::Frozen
            } else {
                Freshness::Expired
            }
        })
    }

    /// Extend the TTL of an entry (for stale-while-revalidate pattern).
    pub fn extend_ttl(&self, query: &str) {
        let hash = Self::hash_query(query);
        let guard = self.entries.guard();
        if let Some(old) = self.entries.get(&hash, &guard) {
            let extended = Arc::new(CacheEntry {
                response: old.response.clone(),
                upstream_id: old.upstream_id.clone(),
                cached_at: Instant::now(),
                cached_at_wall: old.cached_at_wall,
                extended_count: old.extended_count.saturating_add(1),
                schema: old.schema.clone(),
                content_hash: old.content_hash,
                query_text: old.query_text.clone(),
                context_id: old.context_id.clone(),
                freq: AtomicU8::new(1), // Mark as recently accessed
            });
            self.tracked_insert(hash, extended, &guard);
        }
    }

    /// Remove an entry from the cache.
    ///
    /// S3-FIFO cleanup is lazy: the queue entry will be skipped during eviction.
    pub fn remove(&self, query: &str) {
        let exact_hash = Self::hash_query_exact(query);
        let normalized_hash = Self::hash_query(query);
        let guard = self.entries.guard();
        self.tracked_remove(&normalized_hash, &guard);
        let e2n_guard = self.exact_to_normalized.guard();
        self.exact_to_normalized.remove(&exact_hash, &e2n_guard);

        // Remove from persistence if configured
        #[cfg(feature = "persistence")]
        self.unpersist_entry(&normalized_hash);

        // Publish CDC REMOVE event
        if let Some(ref sender) = self.cdc_sender {
            sender.send(
                CdcEventType::Remove,
                CdcEventBuilder::remove(query.to_string()),
            );
        }
    }

    // --- Size / clear ---

    /// Get the number of entries in the cache.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Clear all entries from the cache.
    pub fn clear(&self) {
        let guard = self.entries.guard();
        self.entries.clear(&guard);
        let e2n_guard = self.exact_to_normalized.guard();
        self.exact_to_normalized.clear(&e2n_guard);
        self.total_memory_bytes.store(0, Ordering::Relaxed);

        // Clear S3-FIFO state
        {
            let mut state = self.s3fifo.lock();
            state.small.clear();
            state.main.clear();
            state.ghost.clear();
            state.ghost_set.clear();
        }

        // Clear persistence if configured
        #[cfg(feature = "persistence")]
        self.clear_persistence();

        // Publish CDC REMOVE event for clear (empty query_key = clear all)
        if let Some(ref sender) = self.cdc_sender {
            sender.send(CdcEventType::Remove, CdcEventBuilder::remove(String::new()));
        }
    }

    // --- Iteration / snapshot helpers ---

    /// Get all stale entry hashes (for background refresh).
    ///
    /// Returns hashes of entries that are stale but not expired,
    /// which are candidates for proactive refresh.
    pub fn get_stale_hashes(&self) -> Vec<QueryHash> {
        let guard = self.entries.guard();
        self.entries
            .iter(&guard)
            .filter_map(|(hash, entry)| {
                let age = entry.cached_at.elapsed();
                let fresh = *self.fresh_duration.lock();
                let stale = *self.stale_duration.lock();
                let jittered_fresh = self.jittered_ttl(fresh, hash);
                #[allow(clippy::arithmetic_side_effects)]
                let jittered_total = jittered_fresh + stale;
                if age >= jittered_fresh && age < jittered_total {
                    Some(*hash)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Iterate all cache entries (for snapshot streaming).
    ///
    /// Returns (query_text, response_json, upstream_id) tuples.
    /// Entries that fail to serialize are skipped.
    pub fn snapshot_entries(&self) -> Vec<(String, Vec<u8>, String)> {
        let guard = self.entries.guard();
        self.entries
            .iter(&guard)
            .filter_map(|(_, entry)| {
                let query_text = entry.query_text.clone().unwrap_or_default();
                let json = serde_json::to_vec(&entry.response).ok()?;
                Some((query_text, json, entry.upstream_id.clone()))
            })
            .collect()
    }

    /// Lightweight per-entry summary for observability endpoints.
    ///
    /// Returns metadata for every entry: hash, query text (truncated), upstream,
    /// context, and freshness classification. Avoids cloning the full response
    /// payload so the endpoint stays cheap even at high cache cardinality.
    pub fn entries_summary(&self) -> Vec<CacheEntrySummary> {
        let guard = self.entries.guard();
        let fresh_duration = *self.fresh_duration.lock();
        let stale_duration = *self.stale_duration.lock();
        let now = Instant::now();
        let mut out = Vec::with_capacity(self.entries.len());
        for (hash, entry) in self.entries.iter(&guard) {
            let age = now.saturating_duration_since(entry.cached_at);
            let freshness = if age < fresh_duration {
                EntryFreshness::Fresh
            } else if age < fresh_duration.saturating_add(stale_duration) {
                EntryFreshness::Stale
            } else {
                EntryFreshness::Expired
            };
            let query_text = entry
                .query_text
                .clone()
                .unwrap_or_default()
                .chars()
                .take(120)
                .collect::<String>();
            out.push(CacheEntrySummary {
                hash: query_hash_hex(hash),
                query_text,
                upstream_id: entry.upstream_id.clone(),
                context_id: entry.context_id.clone(),
                cached_at_unix_ms: entry
                    .cached_at_wall
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as u64),
                result_count: entry.response.results.len(),
                freshness,
            });
        }
        out
    }

    /// Iterate all cache entries as rich snapshots for the `distill` feature.
    ///
    /// Returns one `DistillSnapshot` per entry, plus a parallel `Vec<u64>` of
    /// cached_at wall-clock times in milliseconds since UNIX epoch.
    ///
    /// Entries without a recorded `cached_at_wall` (legacy entries from before
    /// the wall-clock field was added) are **skipped** — they cannot be
    /// ordered by insertion time and have no human-meaningful timestamp.
    ///
    /// Entries whose response fails to serialize are also skipped.
    pub fn snapshot_entries_rich(&self) -> Vec<DistillSnapshot> {
        let guard = self.entries.guard();
        self.entries
            .iter(&guard)
            .filter_map(|(hash, entry)| {
                let cached_at_wall = entry.cached_at_wall?;
                let json = serde_json::to_vec(&entry.response).ok()?;
                Some(DistillSnapshot {
                    hash: *hash,
                    context_id: entry.context_id.clone(),
                    query_text: entry.query_text.clone().unwrap_or_default(),
                    cached_at_wall,
                    response_json: json,
                    upstream_id: entry.upstream_id.clone(),
                    extended_count: entry.extended_count,
                })
            })
            .collect()
    }

    /// Get the fresh duration (for computing absolute expiry in CDC events).
    pub fn fresh_duration(&self) -> Duration {
        *self.fresh_duration.lock()
    }

    /// Get the stale duration.
    pub fn stale_duration(&self) -> Duration {
        *self.stale_duration.lock()
    }

    /// Get the fresh duration in seconds.
    pub fn fresh_duration_secs(&self) -> Option<u64> {
        Some(self.fresh_duration().as_secs())
    }

    /// Get the stale duration in seconds.
    pub fn stale_duration_secs(&self) -> Option<u64> {
        Some(self.stale_duration().as_secs())
    }

    /// Set the fresh duration (for hot-reload).
    pub fn set_fresh_duration(&self, d: Duration) {
        *self.fresh_duration.lock() = d;
    }

    /// Set the stale duration (for hot-reload).
    pub fn set_stale_duration(&self, d: Duration) {
        *self.stale_duration.lock() = d;
    }

    /// Get the max frozen duration (absolute expiry cap, regardless of jitter).
    pub fn max_frozen_duration(&self) -> Duration {
        self.max_frozen_duration
    }

    // --- Eviction ---

    /// Evict expired entries first, then use S3-FIFO.
    pub fn evict_smart(&self, target_count: usize) {
        let guard = self.entries.guard();
        let mut evicted = 0usize;
        #[allow(clippy::arithmetic_side_effects)]
        let total_ttl = *self.fresh_duration.lock() + *self.stale_duration.lock();

        // Phase 1: Evict expired entries
        let expired_hashes: Vec<QueryHash> = self
            .entries
            .iter(&guard)
            .filter(|(_, entry)| entry.cached_at.elapsed() > total_ttl)
            .map(|(hash, _)| *hash)
            .collect();

        // R12: batch eviction_stats updates — acquire lock once after loop, not per entry
        let mut expired_count = 0usize;
        for hash in expired_hashes {
            if evicted >= target_count {
                break;
            }
            if self.tracked_remove(&hash, &guard).is_some() {
                evicted = evicted.saturating_add(1);
                expired_count = expired_count.saturating_add(1);
            }
        }

        // Phase 2: If still need more, use S3-FIFO eviction
        if evicted < target_count {
            let remaining = target_count.saturating_sub(evicted);
            let mut state = self.s3fifo.lock();
            let mut s3fifo_evicted = 0usize;
            for _ in 0..remaining {
                if state.small.is_empty() && state.main.is_empty() {
                    break;
                }
                if self.s3fifo_evict_one(&mut state, &guard).is_some() {
                    s3fifo_evicted = s3fifo_evicted.saturating_add(1);
                }
            }
            evicted = evicted.saturating_add(s3fifo_evicted);
        }

        // R12: single lock acquisition for all eviction stats
        if evicted > 0 {
            let mut stats = self.eviction_stats.lock();
            stats.expired = stats.expired.saturating_add(expired_count);
            stats.total = stats.total.saturating_add(evicted);
        }
    }

    /// Evict entries that are truly expired (past max_frozen_duration).
    ///
    /// These entries should not be served even as frozen fallbacks.
    /// Returns the number of entries evicted.
    pub fn evict_truly_expired(&self) -> usize {
        let guard = self.entries.guard();
        let truly_expired_hashes: Vec<QueryHash> = self
            .entries
            .iter(&guard)
            .filter(|(_, entry)| entry.cached_at.elapsed() > self.max_frozen_duration)
            .map(|(hash, _)| *hash)
            .collect();

        let mut count = 0usize;
        for hash in truly_expired_hashes {
            if self.tracked_remove(&hash, &guard).is_some() {
                count = count.saturating_add(1);
            }
        }

        if count > 0 {
            let mut stats = self.eviction_stats.lock();
            stats.expired = stats.expired.saturating_add(count);
            stats.total = stats.total.saturating_add(count);
        }

        count
    }

    /// Count entries for a specific upstream (with a provided guard).
    fn count_for_upstream_inner(&self, upstream_id: &str, guard: &impl papaya::Guard) -> usize {
        self.entries
            .iter(guard)
            .filter(|(_, e)| e.upstream_id == upstream_id)
            .count()
    }

    /// Count entries for a specific upstream.
    pub fn count_for_upstream(&self, upstream_id: &str) -> usize {
        let guard = self.entries.guard();
        self.count_for_upstream_inner(upstream_id, &guard)
    }

    /// Evict oldest entries from a specific upstream (with a provided guard).
    fn evict_from_upstream_inner(
        &self,
        upstream_id: &str,
        count: usize,
        guard: &impl papaya::Guard,
    ) -> usize {
        if count == 0 {
            return 0;
        }

        let mut upstream_entries: Vec<(QueryHash, Duration)> = self
            .entries
            .iter(guard)
            .filter(|(_, e)| e.upstream_id == upstream_id)
            .map(|(hash, e)| (*hash, e.cached_at.elapsed()))
            .collect();

        upstream_entries.sort_by_key(|v| std::cmp::Reverse(v.1));

        let mut evicted = 0usize;
        let mut removed_queries: Vec<String> = Vec::new();
        for (hash, _) in upstream_entries.into_iter().take(count) {
            if let Some(old) = self.tracked_remove(&hash, guard) {
                evicted = evicted.saturating_add(1);
                // Collect query text for CDC REMOVE broadcast (peer invalidation).
                // Entries without query_text (legacy) cannot be targeted remotely;
                // peers drop them at TTL expiry.
                if self.cdc_sender.is_some() {
                    if let Some(ref q) = old.query_text {
                        removed_queries.push(q.clone());
                    }
                }
            }
        }

        // Publish CDC REMOVE events outside the removal loop so peers
        // invalidate their copies (closes the evict-API broadcast gap).
        if let Some(ref sender) = self.cdc_sender {
            for q in removed_queries {
                sender.send(CdcEventType::Remove, CdcEventBuilder::remove(q));
            }
        }

        if evicted > 0 {
            let mut stats = self.eviction_stats.lock();
            stats.oldest = stats.oldest.saturating_add(evicted);
            stats.total = stats.total.saturating_add(evicted);
        }

        evicted
    }

    /// Evict oldest entries from a specific upstream.
    ///
    /// Returns the number of entries evicted.
    pub fn evict_from_upstream(&self, upstream_id: &str, count: usize) -> usize {
        let guard = self.entries.guard();
        self.evict_from_upstream_inner(upstream_id, count, &guard)
    }

    /// Count entries by upstream ID.
    ///
    /// Returns a map of upstream_id to entry count.
    pub fn entries_by_upstream(&self) -> std::collections::HashMap<String, usize> {
        let guard = self.entries.guard();
        let mut counts = std::collections::HashMap::new();
        for (_, entry) in self.entries.iter(&guard) {
            let counter = counts.entry(entry.upstream_id.clone()).or_insert(0usize);
            *counter = counter.saturating_add(1);
        }
        counts
    }

    /// Enforce a per-upstream entry limit.
    ///
    /// If the upstream has more than `max_entries` entries, evict the oldest.
    /// Returns the number of entries evicted.
    pub fn enforce_per_upstream_limit(&self, upstream_id: &str, max_entries: usize) -> usize {
        let current = self.count_for_upstream(upstream_id);
        if current <= max_entries {
            return 0;
        }
        self.evict_from_upstream(upstream_id, current.saturating_sub(max_entries))
    }

    /// Filtered bulk eviction for `/cache/evict` (admin invalidation).
    ///
    /// All provided criteria are AND-combined. None = no constraint on that
    /// field. `query_text_prefix` is a case-insensitive prefix match on the
    /// stored query text (entries with no recorded text are skipped for this
    /// filter). `older_than_secs` is wall-clock age from `cached_at_wall`.
    ///
    /// Returns the number of entries removed. A single CDC REMOVE event is
    /// emitted with the count for downstream consumers (peer mesh).
    #[allow(clippy::too_many_arguments)]
    pub fn evict_filter(
        &self,
        context_id: Option<&str>,
        query_text_prefix: Option<&str>,
        older_than_secs: Option<u64>,
        max_entries: Option<usize>,
    ) -> usize {
        let guard = self.entries.guard();
        let prefix_lower = query_text_prefix.map(|s| s.to_lowercase());

        let wall_threshold = older_than_secs.map(|secs| {
            SystemTime::now()
                .checked_sub(Duration::from_secs(secs))
                .unwrap_or(SystemTime::UNIX_EPOCH)
        });

        let mut matches: Vec<QueryHash> = Vec::new();
        for (hash, entry) in self.entries.iter(&guard) {
            if let Some(cid) = context_id {
                if entry.context_id != cid {
                    continue;
                }
            }
            if let Some(ref p) = prefix_lower {
                match entry.query_text.as_deref() {
                    Some(t) if t.to_lowercase().starts_with(p) => {}
                    _ => continue,
                }
            }
            if let Some(threshold) = wall_threshold {
                match entry.cached_at_wall {
                    Some(w) if w <= threshold => {}
                    _ => continue,
                }
            }
            matches.push(*hash);
        }

        if let Some(max) = max_entries {
            if matches.len() > max {
                matches.truncate(max);
            }
        }

        let mut removed = 0usize;
        for hash in matches {
            if self.tracked_remove(&hash, &guard).is_some() {
                removed = removed.saturating_add(1);
            }
        }

        if removed > 0 {
            let mut stats = self.eviction_stats.lock();
            stats.total = stats.total.saturating_add(removed);

            if let Some(ref sender) = self.cdc_sender {
                sender.send(
                    CdcEventType::Remove,
                    CdcEventBuilder::remove(format!("bulk:{removed}")),
                );
            }
        }

        removed
    }

    // --- Stats ---

    /// Get cache statistics.
    pub fn stats(&self) -> CacheStats {
        let guard = self.entries.guard();
        let mut fresh_count = 0usize;
        let mut stale_count = 0usize;
        let mut expired = 0usize;

        let fresh_ttl = *self.fresh_duration.lock();
        let stale_ttl = *self.stale_duration.lock();
        #[allow(clippy::arithmetic_side_effects)]
        let total_fresh_stale = fresh_ttl + stale_ttl;
        for (_, entry) in self.entries.iter(&guard) {
            let age = entry.cached_at.elapsed();
            if age < fresh_ttl {
                fresh_count = fresh_count.saturating_add(1);
            } else if age < total_fresh_stale {
                stale_count = stale_count.saturating_add(1);
            } else {
                expired = expired.saturating_add(1);
            }
        }

        let total = self.entries.len();
        let max_entries = self.max_entries.load(Ordering::Relaxed);
        let utilization = if max_entries > 0 {
            let total_f = total as f32;
            let max_f = max_entries as f32;
            total_f / max_f
        } else {
            0.0
        };

        CacheStats {
            total,
            fresh: fresh_count,
            stale: stale_count,
            expired,
            max_entries,
            memory_bytes: self.total_memory_bytes.load(Ordering::Relaxed),
            utilization,
        }
    }

    /// Get eviction statistics.
    pub fn eviction_stats(&self) -> EvictionStats {
        self.eviction_stats.lock().clone()
    }

    /// Get statistics by upstream.
    pub fn stats_by_upstream(&self) -> std::collections::HashMap<String, UpstreamCacheStats> {
        let guard = self.entries.guard();
        let mut stats: std::collections::HashMap<String, UpstreamCacheStats> =
            std::collections::HashMap::new();

        for (_, entry) in self.entries.iter(&guard) {
            let upstream_stats = stats.entry(entry.upstream_id.clone()).or_default();

            upstream_stats.total = upstream_stats.total.saturating_add(1);
            upstream_stats.memory_bytes = upstream_stats
                .memory_bytes
                .saturating_add(entry.approximate_size());

            let age = entry.cached_at.elapsed();
            let fresh_ttl = *self.fresh_duration.lock();
            let stale_ttl = *self.stale_duration.lock();
            #[allow(clippy::arithmetic_side_effects)]
            let fresh_stale = fresh_ttl + stale_ttl;
            if age < fresh_ttl {
                upstream_stats.fresh = upstream_stats.fresh.saturating_add(1);
            } else if age < fresh_stale {
                upstream_stats.stale = upstream_stats.stale.saturating_add(1);
            } else {
                upstream_stats.expired = upstream_stats.expired.saturating_add(1);
            }
        }

        stats
    }

    /// Calculate total approximate memory usage.
    pub fn memory_usage(&self) -> usize {
        let guard = self.entries.guard();
        self.entries
            .iter(&guard)
            .map(|(_, e)| e.approximate_size())
            .sum()
    }

    // --- Integrity verification ---

    /// Verify integrity of all cache entries.
    ///
    /// Returns the number of entries that failed integrity check.
    pub fn verify_integrity(&self) -> IntegrityReport {
        let guard = self.entries.guard();
        let mut valid = 0usize;
        let mut invalid = 0usize;
        let mut missing_hash = 0usize;

        for (_, entry) in self.entries.iter(&guard) {
            match entry.content_hash {
                Some(stored_hash) => {
                    let computed = CacheEntry::compute_content_hash(&entry.response);
                    if stored_hash == computed {
                        valid = valid.saturating_add(1);
                    } else {
                        invalid = invalid.saturating_add(1);
                    }
                }
                None => {
                    missing_hash = missing_hash.saturating_add(1);
                }
            }
        }

        IntegrityReport {
            total: self.entries.len(),
            valid,
            invalid,
            missing_hash,
        }
    }

    /// Remove entries that fail integrity verification.
    ///
    /// Returns the number of entries removed.
    pub fn remove_corrupted(&self) -> usize {
        let guard = self.entries.guard();
        let mut corrupted_hashes = Vec::new();

        for (hash, entry) in self.entries.iter(&guard) {
            if let Some(stored_hash) = entry.content_hash {
                let computed = CacheEntry::compute_content_hash(&entry.response);
                if stored_hash != computed {
                    corrupted_hashes.push(*hash);
                }
            }
        }

        let mut count = 0usize;
        for hash in corrupted_hashes {
            if self.tracked_remove(&hash, &guard).is_some() {
                count = count.saturating_add(1);
            }
        }
        count
    }

    // === Persistence Methods (feature-gated) ===

    /// Set the persistent cache backend.
    ///
    /// When set, cache operations will automatically sync to disk.
    #[cfg(feature = "persistence")]
    pub fn set_persistence(&mut self, persistence: Arc<PersistentCache>) {
        self.persistence = Some(persistence);
    }

    /// Get the persistent cache if configured.
    #[cfg(feature = "persistence")]
    pub fn persistence(&self) -> Option<&Arc<PersistentCache>> {
        self.persistence.as_ref()
    }

    /// Persist a single entry to disk.
    ///
    /// Called automatically by insert() when persistence is configured.
    #[cfg(feature = "persistence")]
    fn persist_entry(&self, query_hash: &QueryHash, entry: &CacheEntry) {
        if let Some(ref persistence) = self.persistence {
            let persisted = PersistedEntry {
                query: entry.query_text.clone().unwrap_or_default(),
                upstream_id: entry.upstream_id.clone(),
                response: PersistedResponse::from(&entry.response),
                cached_at_ms: current_time_ms(),
                extended_count: entry.extended_count,
                content_hash: entry.content_hash.unwrap_or([0u8; 32]),
            };

            if let Err(e) = persistence.store_entry(query_hash, &persisted) {
                warn!("Failed to persist cache entry: {}", e);
            }
        }
    }

    /// Remove an entry from persistent storage.
    ///
    /// Called automatically by remove() when persistence is configured.
    #[cfg(feature = "persistence")]
    fn unpersist_entry(&self, query_hash: &QueryHash) {
        if let Some(ref persistence) = self.persistence {
            if let Err(e) = persistence.remove_entry(query_hash) {
                warn!("Failed to remove persisted entry: {}", e);
            }
        }
    }

    /// Clear all entries from persistent storage.
    ///
    /// Called automatically by clear() when persistence is configured.
    #[cfg(feature = "persistence")]
    fn clear_persistence(&self) {
        if let Some(ref persistence) = self.persistence {
            if let Err(e) = persistence.clear_entries() {
                warn!("Failed to clear persisted entries: {}", e);
            }
        }
    }

    /// Restore cache from persistent storage.
    ///
    /// Loads all entries from disk into memory. Call this on startup
    /// to restore cached entries from the previous session.
    ///
    /// Returns the number of entries restored.
    #[cfg(feature = "persistence")]
    pub fn restore_from_persistence(&self) -> usize {
        let persistence = match &self.persistence {
            Some(p) => p,
            None => return 0,
        };

        let hashes = match persistence.all_entry_hashes() {
            Ok(h) => h,
            Err(e) => {
                warn!("Failed to get persisted entry hashes: {}", e);
                return 0;
            }
        };

        let guard = self.entries.guard();
        let mut restored: usize = 0;
        for hash in hashes {
            // Respect max_entries limit
            if self.entries.len() >= self.max_entries.load(Ordering::Relaxed) {
                break;
            }

            let persisted = match persistence.load_entry(&hash) {
                Ok(Some(p)) => p,
                Ok(None) => continue,
                Err(e) => {
                    warn!("Failed to load persisted entry: {}", e);
                    continue;
                }
            };

            // Convert persisted entry to in-memory entry
            #[allow(clippy::arithmetic_side_effects)]
            let cached_at_wall =
                SystemTime::UNIX_EPOCH + Duration::from_millis(persisted.cached_at_ms);
            let entry = CacheEntry {
                response: (&persisted.response).into(),
                upstream_id: persisted.upstream_id,
                cached_at: Instant::now(), // Reset freshness on restore
                cached_at_wall: Some(cached_at_wall),
                extended_count: persisted.extended_count,
                schema: SchemaFingerprint::default(),
                content_hash: Some(persisted.content_hash),
                query_text: if persisted.query.is_empty() {
                    None
                } else {
                    Some(persisted.query)
                },
                context_id: "default".to_string(),
                freq: AtomicU8::new(0),
            };

            self.tracked_insert(hash, Arc::new(entry), &guard);
            self.s3fifo_admit(hash, &guard);
            restored = restored.saturating_add(1);
        }

        debug!("Restored {} entries from persistence", restored);
        restored
    }

    /// Flush all in-memory entries to persistent storage.
    ///
    /// Use this for graceful shutdown to ensure all entries are persisted.
    ///
    /// Returns the number of entries flushed.
    #[cfg(feature = "persistence")]
    pub fn flush_to_persistence(&self) -> usize {
        let persistence = match &self.persistence {
            Some(p) => p,
            None => return 0,
        };

        let guard = self.entries.guard();
        let mut flushed: usize = 0;
        for (hash, entry) in self.entries.iter(&guard) {
            self.persist_entry(hash, entry);
            flushed = flushed.saturating_add(1);
        }

        // Ensure data is written to disk
        if let Err(e) = persistence.flush() {
            warn!("Failed to flush persistence: {}", e);
        }

        debug!("Flushed {} entries to persistence", flushed);
        flushed
    }

    /// Sync persistence - flush to disk.
    #[cfg(feature = "persistence")]
    pub fn sync_persistence(&self) -> bool {
        match &self.persistence {
            Some(p) => p.flush().is_ok(),
            None => true,
        }
    }

    /// Get persistence statistics if available.
    #[cfg(feature = "persistence")]
    pub fn persistence_stats(&self) -> Option<super::persistence::PersistenceStats> {
        self.persistence.as_ref().map(|p| p.stats())
    }
}

/// Report from cache integrity verification.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct IntegrityReport {
    /// Total entries checked.
    pub total: usize,
    /// Entries with valid hash.
    pub valid: usize,
    /// Entries with invalid hash (corrupted).
    pub invalid: usize,
    /// Entries without hash (legacy).
    pub missing_hash: usize,
}

/// Freshness classification of a cache entry at a point in time.
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryFreshness {
    /// Age < fresh_duration.
    Fresh,
    /// Age < fresh_duration + stale_duration.
    Stale,
    /// Past fresh + stale window; should be evicted.
    Expired,
}

/// Render a 32-byte blake3 query hash as 64-char lowercase hex.
fn query_hash_hex(hash: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for b in hash {
        out.push(HEX.get((b >> 4) as usize).copied().unwrap_or(b'0') as char);
        out.push(HEX.get((b & 0x0f) as usize).copied().unwrap_or(b'0') as char);
    }
    out
}

/// Lightweight per-entry summary for observability endpoints.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CacheEntrySummary {
    /// blake3 query hash, rendered as 64-char lowercase hex.
    pub hash: String,
    /// Original query text, truncated to 120 chars for compactness.
    pub query_text: String,
    /// Upstream that produced the cached response.
    pub upstream_id: String,
    /// Context id the entry belongs to.
    pub context_id: String,
    /// Wall-clock insertion time in milliseconds since UNIX epoch (None for legacy).
    pub cached_at_unix_ms: Option<u64>,
    /// Number of results in the cached response.
    pub result_count: usize,
    /// Fresh/stale/expired relative to current cache TTLs.
    pub freshness: EntryFreshness,
}

/// Statistics about the cache state.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CacheStats {
    /// Total number of entries.
    pub total: usize,
    /// Number of fresh entries.
    pub fresh: usize,
    /// Number of stale entries.
    pub stale: usize,
    /// Number of expired entries.
    pub expired: usize,
    /// Maximum allowed entries.
    pub max_entries: usize,
    /// Approximate memory usage in bytes.
    pub memory_bytes: usize,
    /// Cache utilization (0.0 to 1.0).
    pub utilization: f32,
}

/// Cache statistics for a specific upstream.
#[derive(Debug, Clone, Default)]
pub struct UpstreamCacheStats {
    /// Total entries for this upstream.
    pub total: usize,
    /// Fresh entries.
    pub fresh: usize,
    /// Stale entries.
    pub stale: usize,
    /// Expired entries.
    pub expired: usize,
    /// Approximate memory usage in bytes.
    pub memory_bytes: usize,
}

impl CacheEntry {
    /// Calculate approximate size of this entry in bytes.
    pub fn approximate_size(&self) -> usize {
        let mut size = std::mem::size_of::<Self>();

        // Add response size
        for result in &self.response.results {
            size = size.saturating_add(result.id.len());
            size = size.saturating_add(result.content.len());
            size = size.saturating_add(std::mem::size_of::<f32>()); // score
            if let Some(meta) = &result.metadata {
                size = size.saturating_add(meta.to_string().len());
            }
        }

        // Add upstream_id
        size = size.saturating_add(self.upstream_id.len());

        // Add schema size
        if let Some(model) = &self.schema.model_hint {
            size = size.saturating_add(model.len());
        }
        if let Some(version) = &self.schema.version {
            size = size.saturating_add(version.len());
        }

        // Add query_text size
        if let Some(query) = &self.query_text {
            size = size.saturating_add(query.len());
        }

        size
    }
}

/// Eviction reason for tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvictionReason {
    /// Entry was expired.
    Expired,
    /// Entry had low similarity to seeds.
    LowSimilarity,
    /// Entry was oldest (LRU).
    OldestEntry,
    /// Entry exceeded size limit.
    SizeLimit,
    /// Manual eviction.
    Manual,
}

/// Eviction statistics.
#[derive(Debug, Clone, Default)]
pub struct EvictionStats {
    /// Total evictions.
    pub total: usize,
    /// Evictions by reason.
    pub expired: usize,
    pub low_similarity: usize,
    pub oldest: usize,
    pub size_limit: usize,
    pub manual: usize,
    /// Evictions due to integrity check failure.
    pub integrity_failures: usize,
    /// Evictions due to per-upstream limit.
    pub per_upstream: usize,
    /// S3-FIFO: re-insertions found in ghost set (admitted directly to Main).
    pub ghost_hits: usize,
    /// S3-FIFO: entries evicted from the Small queue.
    pub small_evictions: usize,
    /// S3-FIFO: entries evicted from the Main queue.
    pub main_evictions: usize,
    /// S3-FIFO: entries promoted from Small to Main (accessed while in Small).
    pub promotions: usize,
}

#[cfg(test)]
#[path = "tests/cache_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/stress_tests.rs"]
mod stress_tests;
