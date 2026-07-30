//! Context manager for multi-context cache support.
//!
//! Provides isolation between different cache contexts (e.g., different
//! projects, upstreams, or collections). Each context maintains its own
//! cache namespace with independent entries and statistics.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::config::{ContextCacheConfig, FederatedConfig, ProxyScopeConfig, ResolvedContext};
use crate::proxy::cascade::CascadeConfig;

use super::scope::ScopeFilter;
use super::upstream::QueryMode;

/// Per-context runtime policy (plan 10 T2). Owned by context; no global fallback
/// when present.
#[derive(Clone)]
pub struct ContextPolicy {
    pub scope_filter: Arc<ScopeFilter>,
    pub scope: ProxyScopeConfig,
    pub cache: ContextCacheConfig,
    pub cascade: CascadeConfig,
    pub federated: FederatedConfig,
}

impl ContextPolicy {
    /// Build from a resolved config context.
    pub fn from_resolved(ctx: &ResolvedContext) -> Self {
        Self {
            scope_filter: Arc::new(ScopeFilter::from_config(&ctx.scope)),
            scope: ctx.scope.clone(),
            cache: ctx.cache.clone(),
            cascade: ctx.cascade.clone(),
            federated: ctx.federated.clone(),
        }
    }
}

/// Unique identifier for a cache context.
pub type ContextId = String;

/// Type of upstream backend.
///
/// This is orthogonal to `QueryMode` - it describes the backend technology,
/// not the query interface. For example, a VectorDB might accept text queries
/// (TextNative) if it has built-in embedding, or require vectors (VectorOnly).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UpstreamType {
    /// Full-text search engine (Elasticsearch, OpenSearch, Meilisearch, Typesense).
    /// Always TextNative. Scores are typically BM25-based (can be > 1.0).
    FullTextSearch,

    /// Vector database (Qdrant, Pinecone, Milvus, Weaviate, ChromaDB).
    /// Can be TextNative (with embedding) or VectorOnly.
    /// Scores are typically cosine similarity (0.0 - 1.0).
    VectorDatabase,

    /// Hybrid search (Elasticsearch with kNN, OpenSearch with neural search).
    /// Supports both FTS and vector search modes.
    Hybrid,

    /// Type not yet determined.
    #[default]
    Unknown,
}

impl UpstreamType {
    /// Check if this is a full-text search backend.
    pub fn is_fts(&self) -> bool {
        matches!(self, Self::FullTextSearch | Self::Hybrid)
    }

    /// Check if this is a vector database backend.
    pub fn is_vector_db(&self) -> bool {
        matches!(self, Self::VectorDatabase | Self::Hybrid)
    }

    /// Check if the type is known.
    pub fn is_known(&self) -> bool {
        !matches!(self, Self::Unknown)
    }

    /// Get the typical score range for this upstream type.
    ///
    /// Returns (min, max) for score normalization in federated search.
    pub fn score_range(&self) -> (f32, f32) {
        match self {
            Self::FullTextSearch => (0.0, 100.0), // BM25 scores can be high
            Self::VectorDatabase => (0.0, 1.0),   // Cosine similarity
            Self::Hybrid => (0.0, 1.0),           // Normalized
            Self::Unknown => (0.0, 1.0),          // Assume normalized
        }
    }

    /// Convert from `u8` representation.
    ///
    /// Used for `AtomicU8` storage in `PooledUpstream`.
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::FullTextSearch,
            1 => Self::VectorDatabase,
            2 => Self::Hybrid,
            _ => Self::Unknown,
        }
    }

    /// Convert to `u8` representation.
    ///
    /// Used for `AtomicU8` storage in `PooledUpstream`.
    pub fn to_u8(self) -> u8 {
        match self {
            Self::FullTextSearch => 0,
            Self::VectorDatabase => 1,
            Self::Hybrid => 2,
            Self::Unknown => 3,
        }
    }

    /// Get string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FullTextSearch => "fts",
            Self::VectorDatabase => "vector_db",
            Self::Hybrid => "hybrid",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for UpstreamType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Metadata for a cache context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextMetadata {
    /// Context identifier.
    pub id: ContextId,
    /// Upstream URL this context is associated with.
    pub upstream_url: String,
    /// Collection name (if applicable).
    pub collection: String,
    /// Optional description.
    pub description: Option<String>,
    /// Query mode for this context (TextNative vs VectorOnly).
    pub query_mode: QueryMode,
    /// Type of upstream backend (FTS vs VectorDB).
    pub upstream_type: UpstreamType,
    /// Creation timestamp (Unix epoch seconds).
    pub created_at: u64,
    /// Last access timestamp (Unix epoch seconds).
    pub last_accessed: u64,
    /// Number of cache entries.
    pub entry_count: u64,
}

impl ContextMetadata {
    /// Create new context metadata.
    pub fn new(id: &str, upstream_url: &str, collection: &str) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            id: id.to_string(),
            upstream_url: upstream_url.to_string(),
            collection: collection.to_string(),
            description: None,
            query_mode: QueryMode::Unknown,
            upstream_type: UpstreamType::Unknown,
            created_at: now,
            last_accessed: now,
            entry_count: 0,
        }
    }

    /// Create with description.
    pub fn with_description(mut self, desc: &str) -> Self {
        self.description = Some(desc.to_string());
        self
    }

    /// Create with upstream type.
    pub fn with_upstream_type(mut self, upstream_type: UpstreamType) -> Self {
        self.upstream_type = upstream_type;
        self
    }

    /// Create with query mode.
    pub fn with_query_mode(mut self, query_mode: QueryMode) -> Self {
        self.query_mode = query_mode;
        self
    }
}

/// Statistics for a context.
#[derive(Debug, Default)]
pub struct ContextStats {
    /// Cache hits.
    pub hits: AtomicU64,
    /// Cache misses.
    pub misses: AtomicU64,
    /// Total queries.
    pub queries: AtomicU64,
}

impl ContextStats {
    /// Create new stats.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a cache hit.
    pub fn record_hit(&self) {
        self.hits.fetch_add(1, Ordering::Relaxed);
        self.queries.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a cache miss.
    pub fn record_miss(&self) {
        self.misses.fetch_add(1, Ordering::Relaxed);
        self.queries.fetch_add(1, Ordering::Relaxed);
    }

    /// Get hit rate.
    pub fn hit_rate(&self) -> f64 {
        let hits = self.hits.load(Ordering::Relaxed);
        let total = self.queries.load(Ordering::Relaxed);
        if total == 0 {
            0.0
        } else {
            hits as f64 / total as f64
        }
    }

    /// Reset all counters to zero.
    pub fn reset(&self) {
        self.hits.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);
        self.queries.store(0, Ordering::Relaxed);
    }

    /// Get snapshot of stats.
    pub fn snapshot(&self) -> ContextStatsSnapshot {
        ContextStatsSnapshot {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            queries: self.queries.load(Ordering::Relaxed),
        }
    }
}

/// Snapshot of context statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextStatsSnapshot {
    /// Cache hits.
    pub hits: u64,
    /// Cache misses.
    pub misses: u64,
    /// Total queries.
    pub queries: u64,
}

impl ContextStatsSnapshot {
    /// Get hit rate.
    pub fn hit_rate(&self) -> f64 {
        if self.queries == 0 {
            0.0
        } else {
            self.hits as f64 / self.queries as f64
        }
    }
}

/// Configuration for context management.
#[derive(Debug, Clone)]
pub struct ContextConfig {
    /// Default context ID.
    pub default_context: ContextId,
    /// Maximum number of active contexts in memory.
    pub max_active_contexts: usize,
    /// Whether to auto-create contexts on first access.
    pub auto_create: bool,
    /// Timeout for inactive contexts before eviction.
    pub inactive_timeout: Duration,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            default_context: "default".to_string(),
            max_active_contexts: 10,
            auto_create: true,
            inactive_timeout: Duration::from_secs(3600), // 1 hour
        }
    }
}

/// In-memory context cache entry.
pub struct ContextCache {
    /// Context metadata.
    pub metadata: ContextMetadata,
    /// Context statistics.
    pub stats: ContextStats,
    /// Last access time (for LRU eviction).
    pub last_access: Instant,
    /// Per-context policy (scope/cache/cascade/federated). None = legacy global.
    pub policy: Option<Arc<ContextPolicy>>,
}

impl ContextCache {
    /// Create a new context cache.
    pub fn new(metadata: ContextMetadata) -> Self {
        Self {
            metadata,
            stats: ContextStats::new(),
            last_access: Instant::now(),
            policy: None,
        }
    }

    /// Touch the context (update last access time).
    pub fn touch(&mut self) {
        self.last_access = Instant::now();
    }
}

/// Context manager for multi-context cache support.
pub struct ContextManager {
    /// Active contexts in memory.
    active: DashMap<ContextId, ContextCache>,
    /// Current context.
    current: RwLock<ContextId>,
    /// Configuration.
    config: ContextConfig,
}

impl ContextManager {
    /// Create a new context manager.
    pub fn new(config: ContextConfig) -> Self {
        let default_id = config.default_context.clone();

        let manager = Self {
            active: DashMap::new(),
            current: RwLock::new(default_id.clone()),
            config,
        };

        // Create default context
        let metadata = ContextMetadata::new(&default_id, "", "");
        manager
            .active
            .insert(default_id, ContextCache::new(metadata));

        manager
    }

    /// Create with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(ContextConfig::default())
    }

    /// Get the current context ID.
    pub fn current(&self) -> ContextId {
        self.current.read().clone()
    }

    /// Switch to a different context.
    pub fn switch(&self, context_id: &str) -> Result<(), ContextError> {
        // Check if context exists or auto-create
        if !self.active.contains_key(context_id) {
            if self.config.auto_create {
                match self.create(context_id, "", "") {
                    Ok(()) | Err(ContextError::AlreadyExists(_)) => {}
                    Err(e) => return Err(e),
                }
            } else {
                return Err(ContextError::NotFound(context_id.to_string()));
            }
        }

        // Update last access
        if let Some(mut ctx) = self.active.get_mut(context_id) {
            ctx.touch();
        }

        // Switch current
        *self.current.write() = context_id.to_string();

        tracing::info!(context = %context_id, "Switched context");

        // Maybe evict old contexts
        self.maybe_evict();

        Ok(())
    }

    /// Create a new context.
    pub fn create(
        &self,
        id: &str,
        upstream_url: &str,
        collection: &str,
    ) -> Result<(), ContextError> {
        if self.active.contains_key(id) {
            return Err(ContextError::AlreadyExists(id.to_string()));
        }

        let metadata = ContextMetadata::new(id, upstream_url, collection);
        self.active
            .insert(id.to_string(), ContextCache::new(metadata));

        tracing::info!(context = %id, "Created context");

        Ok(())
    }

    /// Install policies from resolved contexts (plan 10 T2).
    ///
    /// Creates missing contexts, sets default current to the default context,
    /// and attaches `ContextPolicy` (scope/cache/cascade/federated) per id.
    pub fn install_resolved(&self, resolved: &[ResolvedContext]) {
        let default_id = resolved
            .iter()
            .find(|c| c.is_default)
            .map(|c| c.id.as_str())
            .unwrap_or("default");

        for ctx in resolved {
            let policy = Arc::new(ContextPolicy::from_resolved(ctx));
            let url = ctx
                .legs
                .first()
                .map(|l| l.endpoint.url.as_str())
                .unwrap_or("");
            let collection = ctx
                .legs
                .first()
                .and_then(|l| l.endpoint.index.as_deref())
                .unwrap_or("");

            if let Some(mut entry) = self.active.get_mut(&ctx.id) {
                entry.policy = Some(policy);
                if let Some(ref d) = ctx.description {
                    entry.metadata.description = Some(d.clone());
                }
                entry.touch();
            } else {
                let mut meta = ContextMetadata::new(&ctx.id, url, collection);
                meta.description = ctx.description.clone();
                let mut cache = ContextCache::new(meta);
                cache.policy = Some(policy);
                self.active.insert(ctx.id.clone(), cache);
            }
        }

        // Point default_context config + current at resolved default when present.
        if self.active.contains_key(default_id) {
            *self.current.write() = default_id.to_string();
        }
    }

    /// Scope filter for a context id, if policy installed.
    pub fn scope_filter_for(&self, id: &str) -> Option<Arc<ScopeFilter>> {
        self.active
            .get(id)
            .and_then(|c| c.policy.as_ref().map(|p| p.scope_filter.clone()))
    }

    /// Full policy for a context id.
    pub fn policy_for(&self, id: &str) -> Option<Arc<ContextPolicy>> {
        self.active.get(id).and_then(|c| c.policy.clone())
    }

    /// Build manager pre-loaded from resolved contexts.
    pub fn from_resolved(resolved: &[ResolvedContext]) -> Self {
        let default_id = resolved
            .iter()
            .find(|c| c.is_default)
            .map(|c| c.id.clone())
            .unwrap_or_else(|| "default".into());
        let mgr = Self::new(ContextConfig {
            default_context: default_id,
            ..ContextConfig::default()
        });
        mgr.install_resolved(resolved);
        mgr
    }

    /// Delete a context.
    pub fn delete(&self, id: &str) -> Result<(), ContextError> {
        if id == self.config.default_context {
            return Err(ContextError::CannotDeleteDefault);
        }

        if !self.active.contains_key(id) {
            return Err(ContextError::NotFound(id.to_string()));
        }

        // Switch away if this is the current context
        if *self.current.read() == id {
            *self.current.write() = self.config.default_context.clone();
        }

        self.active.remove(id);

        tracing::info!(context = %id, "Deleted context");

        Ok(())
    }

    /// Get context metadata.
    pub fn get(&self, id: &str) -> Option<ContextMetadata> {
        self.active.get(id).map(|c| c.metadata.clone())
    }

    /// Get current context metadata.
    pub fn get_current(&self) -> Option<ContextMetadata> {
        let id = self.current();
        self.get(&id)
    }

    /// List all context IDs.
    pub fn list(&self) -> Vec<ContextId> {
        self.active.iter().map(|e| e.key().clone()).collect()
    }

    /// List all context metadata.
    pub fn list_metadata(&self) -> Vec<ContextMetadata> {
        self.active.iter().map(|e| e.metadata.clone()).collect()
    }

    /// Get statistics for a context.
    pub fn stats(&self, id: &str) -> Option<ContextStatsSnapshot> {
        self.active.get(id).map(|c| c.stats.snapshot())
    }

    /// Record a cache hit for the current context.
    pub fn record_hit(&self) {
        let id = self.current();
        if let Some(ctx) = self.active.get(&id) {
            ctx.stats.record_hit();
        }
    }

    /// Record a cache miss for the current context.
    pub fn record_miss(&self) {
        let id = self.current();
        if let Some(ctx) = self.active.get(&id) {
            ctx.stats.record_miss();
        }
    }

    /// Record a cache hit for a specific context.
    pub fn record_hit_for(&self, ctx_id: &str) {
        if let Some(ctx) = self.active.get(ctx_id) {
            ctx.stats.record_hit();
        }
    }

    /// Record a cache miss for a specific context.
    pub fn record_miss_for(&self, ctx_id: &str) {
        if let Some(ctx) = self.active.get(ctx_id) {
            ctx.stats.record_miss();
        }
    }

    /// Reset stats for all active contexts.
    pub fn reset_all_stats(&self) {
        for ctx in self.active.iter() {
            ctx.stats.reset();
        }
    }

    /// Get stats snapshot for a specific context.
    pub fn get_context_stats(&self, id: &str) -> Option<(ContextStatsSnapshot, f64)> {
        self.active
            .get(id)
            .map(|ctx| (ctx.stats.snapshot(), ctx.stats.hit_rate()))
    }

    /// Get stats snapshots for all active contexts.
    pub fn all_context_stats(&self) -> Vec<(String, ContextStatsSnapshot, f64)> {
        self.active
            .iter()
            .map(|entry| {
                let id = entry.key().clone();
                let ctx = entry.value();
                (id, ctx.stats.snapshot(), ctx.stats.hit_rate())
            })
            .collect()
    }

    /// Update context metadata.
    pub fn update_metadata<F>(&self, id: &str, f: F) -> Result<(), ContextError>
    where
        F: FnOnce(&mut ContextMetadata),
    {
        if let Some(mut ctx) = self.active.get_mut(id) {
            f(&mut ctx.metadata);
            Ok(())
        } else {
            Err(ContextError::NotFound(id.to_string()))
        }
    }

    /// Set query mode for a context.
    pub fn set_query_mode(&self, id: &str, mode: QueryMode) -> Result<(), ContextError> {
        self.update_metadata(id, |m| m.query_mode = mode)
    }

    /// Get query mode for a context.
    pub fn query_mode(&self, id: &str) -> Option<QueryMode> {
        self.active.get(id).map(|c| c.metadata.query_mode)
    }

    /// Build a cache key prefixed with the context ID.
    pub fn cache_key(&self, query_hash: u64) -> String {
        let ctx = self.current();
        format!("ctx:{}:resp:{:016x}", ctx, query_hash)
    }

    /// Build an embedding cache key prefixed with the context ID.
    pub fn embedding_key(&self, text_hash: u64) -> String {
        let ctx = self.current();
        format!("ctx:{}:emb:{:016x}", ctx, text_hash)
    }

    /// Get number of active contexts.
    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// Evict inactive contexts if over limit.
    fn maybe_evict(&self) {
        if self.active.len() <= self.config.max_active_contexts {
            return;
        }

        // Find contexts to evict (oldest, not current, not default)
        let current = self.current();
        let default = &self.config.default_context;

        let mut candidates: Vec<_> = self
            .active
            .iter()
            .filter(|e| e.key() != &current && e.key() != default)
            .map(|e| (e.key().clone(), e.last_access))
            .collect();

        candidates.sort_by_key(|(_, last)| *last);

        let to_evict = self
            .active
            .len()
            .saturating_sub(self.config.max_active_contexts);
        for (id, _) in candidates.into_iter().take(to_evict) {
            self.active.remove(&id);
            tracing::debug!(context = %id, "Evicted inactive context");
        }
    }
}

impl Default for ContextManager {
    fn default() -> Self {
        Self::with_defaults()
    }
}

/// Errors from context operations.
#[derive(Debug, Clone)]
pub enum ContextError {
    /// Context not found.
    NotFound(String),
    /// Context already exists.
    AlreadyExists(String),
    /// Cannot delete the default context.
    CannotDeleteDefault,
}

impl std::fmt::Display for ContextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContextError::NotFound(id) => write!(f, "Context not found: {}", id),
            ContextError::AlreadyExists(id) => write!(f, "Context already exists: {}", id),
            ContextError::CannotDeleteDefault => write!(f, "Cannot delete the default context"),
        }
    }
}

impl std::error::Error for ContextError {}

#[cfg(test)]
#[path = "tests/context_tests.rs"]
mod tests;
