//! Upstream pool for managing multiple upstream endpoints.
//!
//! Provides load balancing and failover across multiple upstream RAG services.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use serde::Serialize;

use super::connection_pool::{
    ConnectionPool, ConnectionPoolConfig, ConnectionPoolSnapshot, PoolError,
};
use super::context::UpstreamType;
use super::elasticsearch::{ElasticsearchAdapter, ElasticsearchConfig};
use super::meilisearch::{MeilisearchAdapter, MeilisearchConfig};
use super::milvus::{MilvusAdapter, MilvusConfig};
#[cfg(feature = "pgvector")]
use super::pgvector::{PgvectorAdapter, PgvectorConfig};
use super::pinecone::{PineconeAdapter, PineconeConfig};
use super::qdrant::{QdrantAdapter, QdrantConfig};
use super::types::{QueryRequest, QueryResponse};
use super::upstream::{
    GenericRestAdapter, HealthTracker, QueryMode, UpstreamAdapter, UpstreamError, UpstreamStatus,
};
use crate::config::UpstreamEndpointConfig;

/// Load balancing strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoadBalanceStrategy {
    /// Round-robin across all healthy upstreams.
    #[default]
    RoundRobin,
    /// Weighted round-robin based on configured weights.
    Weighted,
    /// Random selection.
    Random,
    /// Failover: use primary, fall back to secondary on failure.
    Failover,
}

impl LoadBalanceStrategy {
    /// Get the strategy name as a string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RoundRobin => "round_robin",
            Self::Weighted => "weighted",
            Self::Random => "random",
            Self::Failover => "failover",
        }
    }
}

/// A single upstream in the pool.
pub struct PooledUpstream {
    /// Unique identifier.
    pub id: String,
    /// The upstream adapter.
    pub adapter: Arc<dyn UpstreamAdapter>,
    /// Health tracker for this upstream.
    pub health: Arc<HealthTracker>,
    /// Connection pool for this upstream (pgbouncer-style).
    connection_pool: ConnectionPool,
    /// Weight for load balancing.
    pub weight: u32,
    /// Priority for failover (lower = higher priority).
    pub priority: u32,
    /// Request count for metrics.
    requests: AtomicU64,
    /// Failure count for metrics.
    failures: AtomicU64,
    /// Whether this upstream is enabled.
    enabled: AtomicBool,
    /// Type of upstream backend (FTS vs VectorDB).
    upstream_type: AtomicU8,
}

/// Create a specialized adapter based on the upstream type in config.
///
/// Matches on `config.upstream_type` to select the right adapter:
/// - `"elasticsearch"` / `"opensearch"` → `ElasticsearchAdapter`
/// - `"qdrant"` → `QdrantAdapter`
/// - `"meilisearch"` → `MeilisearchAdapter`
/// - `"pinecone"` → `PineconeAdapter`
/// - `"milvus"` → `MilvusAdapter`
/// - anything else → `GenericRestAdapter` (backward compatible)
fn create_adapter(config: &UpstreamEndpointConfig) -> Result<Arc<dyn UpstreamAdapter>, String> {
    let timeout = Duration::from_secs(config.timeout_secs());

    // Resolve upstream API key (supports `${ENV_VAR}` via resolve_env_ref).
    // If the env var is unset, silently use None (fallback to unauthenticated).
    let api_key = config
        .api_key
        .as_ref()
        .and_then(|k| crate::config::resolve_env_ref(k));

    match config.upstream_type.as_deref() {
        Some("elasticsearch" | "opensearch") => {
            let search_fields = if config.search_fields.is_empty() {
                vec!["content".to_string()]
            } else {
                config.search_fields.clone()
            };
            Ok(Arc::new(
                ElasticsearchAdapter::new(ElasticsearchConfig {
                    base_url: config.url.clone(),
                    index: config
                        .index
                        .clone()
                        .unwrap_or_else(|| "documents".to_string()),
                    timeout,
                    search_fields,
                    return_fields: config.return_fields.clone(),
                    api_key,
                    score_threshold: None,
                })
                .map_err(|e| e.to_string())?,
            ))
        }
        Some("qdrant") => Ok(Arc::new(
            QdrantAdapter::new(QdrantConfig {
                base_url: config.url.clone(),
                collection_name: config
                    .index
                    .clone()
                    .unwrap_or_else(|| "default".to_string()),
                timeout,
                api_key,
                with_payload: true,
                with_vectors: false,
                score_threshold: None,
            })
            .map_err(|e| e.to_string())?,
        )),
        Some("meilisearch") => Ok(Arc::new(
            MeilisearchAdapter::new(MeilisearchConfig {
                base_url: config.url.clone(),
                index: config
                    .index
                    .clone()
                    .unwrap_or_else(|| "documents".to_string()),
                timeout,
                search_attributes: if config.search_fields.is_empty() {
                    Vec::new()
                } else {
                    config.search_fields.clone()
                },
                displayed_attributes: config.return_fields.clone(),
                api_key,
                score_threshold: None,
            })
            .map_err(|e| e.to_string())?,
        )),
        Some("milvus") => Ok(Arc::new(
            MilvusAdapter::new(MilvusConfig {
                base_url: config.url.clone(),
                collection_name: config
                    .index
                    .clone()
                    .unwrap_or_else(|| "default".to_string()),
                timeout,
                api_key,
                vector_field: "vector".to_string(),
                output_fields: if config.return_fields.is_empty() {
                    vec!["content".to_string()]
                } else {
                    config.return_fields.clone()
                },
                dimensions: config.dimensions,
                score_threshold: None,
            })
            .map_err(|e| e.to_string())?,
        )),
        Some("pinecone") => Ok(Arc::new(
            PineconeAdapter::new(PineconeConfig {
                base_url: config.url.clone(),
                api_key,
                namespace: None,
                timeout,
                dimensions: config.dimensions,
                include_metadata: true,
                score_threshold: None,
            })
            .map_err(|e| e.to_string())?,
        )),
        #[cfg(feature = "pgvector")]
        Some("pgvector") => {
            // PgvectorAdapter::connect is async; create_adapter is sync.
            // Use block_in_place + Handle::block_on so this works whether
            // called from sync startup code or from within a tokio task.
            let table = config
                .table
                .clone()
                .or_else(|| config.index.clone())
                .ok_or_else(|| {
                    "pgvector: 'table' (or 'collection') field is required".to_string()
                })?;
            let embedding_column = config
                .embedding_column
                .clone()
                .unwrap_or_else(|| "vector".to_string());
            let content_column = config
                .content_column
                .clone()
                .unwrap_or_else(|| "content".to_string());
            let pg_config = PgvectorConfig {
                url: config.url.clone(),
                table,
                embedding_column,
                content_column,
                title_column: None,
                metadata_columns: config.metadata_columns.clone(),
                distance_metric: Default::default(),
                dimensions: config.dimensions,
                timeout_secs: config.timeout_secs(),
            };
            let adapter = match tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(PgvectorAdapter::connect(pg_config))
            }) {
                Ok(a) => a,
                Err(e) => {
                    return Err(format!("pgvector connect ({}): {e}", config.url));
                }
            };
            Ok(Arc::new(adapter))
        }
        _ => {
            // Generic fallback — backward compatible
            Ok(Arc::new(
                GenericRestAdapter::new(&config.url, timeout).map_err(|e| e.to_string())?,
            ))
        }
    }
}

impl PooledUpstream {
    /// Create a new pooled upstream.
    pub fn new(config: &UpstreamEndpointConfig) -> Result<Self, String> {
        // Configure connection pool based on max_concurrent setting
        let pool_config = if let Some(max) = config.max_concurrent {
            ConnectionPoolConfig::new(max)
        } else {
            ConnectionPoolConfig::default()
        };

        // Determine upstream type from config
        let upstream_type = match config.upstream_type.as_deref() {
            Some("elasticsearch" | "opensearch" | "meilisearch") => UpstreamType::FullTextSearch,
            Some("qdrant" | "pinecone" | "milvus" | "pgvector") => UpstreamType::VectorDatabase,
            _ => UpstreamType::Unknown,
        };

        let adapter = create_adapter(config)?;

        // Apply config-declared query mode (skip probing)
        if let Some(ref qm) = config.query_mode {
            let mode = match qm.as_str() {
                "vector_only" => QueryMode::VectorOnly,
                "text_native" => QueryMode::TextNative,
                _ => QueryMode::Unknown,
            };
            if mode != QueryMode::Unknown {
                adapter.set_query_mode(mode);
            }
        }

        Ok(Self {
            id: config.id.clone(),
            adapter,
            health: Arc::new(HealthTracker::new()),
            connection_pool: ConnectionPool::new(pool_config),
            weight: config.weight(),
            priority: config.priority(),
            requests: AtomicU64::new(0),
            failures: AtomicU64::new(0),
            enabled: AtomicBool::new(config.enabled()),
            upstream_type: AtomicU8::new(upstream_type.to_u8()),
        })
    }

    /// Check if this upstream is healthy.
    pub fn is_healthy(&self) -> bool {
        self.enabled.load(Ordering::Relaxed) && self.health.status() == UpstreamStatus::Online
    }

    /// Check if this upstream is available (healthy or degraded).
    pub fn is_available(&self) -> bool {
        self.enabled.load(Ordering::Relaxed) && self.health.status() != UpstreamStatus::Offline
    }

    /// Enable this upstream.
    pub fn enable(&self) {
        self.enabled.store(true, Ordering::Relaxed);
    }

    /// Disable this upstream.
    pub fn disable(&self) {
        self.enabled.store(false, Ordering::Relaxed);
    }

    /// Record a successful request.
    pub fn record_success(&self) {
        self.requests.fetch_add(1, Ordering::Relaxed);
        self.health.record_success();
    }

    /// Record a failed request.
    pub fn record_failure(&self) {
        self.requests.fetch_add(1, Ordering::Relaxed);
        self.failures.fetch_add(1, Ordering::Relaxed);
        self.health.record_failure();
    }

    /// Get request count.
    pub fn request_count(&self) -> u64 {
        self.requests.load(Ordering::Relaxed)
    }

    /// Get failure count.
    pub fn failure_count(&self) -> u64 {
        self.failures.load(Ordering::Relaxed)
    }

    /// Get failure rate.
    pub fn failure_rate(&self) -> f64 {
        let requests = self.request_count();
        if requests == 0 {
            return 0.0;
        }
        self.failure_count() as f64 / requests as f64
    }

    // === QueryMode Support ===

    /// Get the query mode for this upstream.
    ///
    /// Returns the discovered or configured QueryMode for this upstream.
    pub fn query_mode(&self) -> QueryMode {
        self.adapter.query_mode()
    }

    /// Set the query mode for this upstream.
    ///
    /// Used after mode discovery to cache the result.
    pub fn set_query_mode(&self, mode: QueryMode) {
        self.adapter.set_query_mode(mode);
    }

    /// Check if this upstream supports text queries natively.
    ///
    /// Returns true for TextNative upstreams.
    pub fn supports_text(&self) -> bool {
        self.query_mode().supports_text()
    }

    /// Check if this upstream requires embedding.
    ///
    /// Returns true for VectorOnly upstreams.
    pub fn requires_embedding(&self) -> bool {
        self.query_mode().requires_embedding()
    }

    /// Check if the query mode has been discovered.
    ///
    /// Returns false if the mode is still Unknown.
    pub fn is_mode_known(&self) -> bool {
        self.query_mode() != QueryMode::Unknown
    }

    // === UpstreamType Support ===

    /// Get the upstream type (FTS vs VectorDB).
    pub fn upstream_type(&self) -> UpstreamType {
        UpstreamType::from_u8(self.upstream_type.load(Ordering::Relaxed))
    }

    /// Set the upstream type.
    ///
    /// Used after type discovery or from configuration.
    pub fn set_upstream_type(&self, upstream_type: UpstreamType) {
        self.upstream_type
            .store(upstream_type.to_u8(), Ordering::Relaxed);
    }

    /// Check if this is a full-text search backend.
    pub fn is_fts(&self) -> bool {
        self.upstream_type().is_fts()
    }

    /// Check if this is a vector database backend.
    pub fn is_vector_db(&self) -> bool {
        self.upstream_type().is_vector_db()
    }

    /// Check if the upstream type is known.
    pub fn is_type_known(&self) -> bool {
        self.upstream_type().is_known()
    }

    /// Get the typical score range for this upstream.
    pub fn score_range(&self) -> (f32, f32) {
        self.upstream_type().score_range()
    }

    // === Connection Pool Support ===

    /// Execute a query with connection pool enforcement.
    ///
    /// This method:
    /// 1. Acquires a connection permit from the pool (waits if exhausted)
    /// 2. Executes the query
    /// 3. Returns the permit when done
    ///
    /// Use this instead of `adapter.query()` to respect `max_concurrent` limits.
    pub async fn query_pooled(
        &self,
        request: &QueryRequest,
    ) -> Result<QueryResponse, UpstreamError> {
        // Acquire connection from pool
        // PERF(R6): deferred — UpstreamError::Unavailable needs Cow<'static, str>
        // to avoid .to_string() allocation on error paths
        let _conn = self.connection_pool.acquire().await.map_err(|e| match e {
            PoolError::QueueFull => UpstreamError::Unavailable("connection queue full".to_string()),
            PoolError::Timeout => UpstreamError::Timeout,
            PoolError::Shutdown => {
                UpstreamError::Unavailable("connection pool shutdown".to_string())
            }
        })?;

        // Execute query (connection is held until response is received)
        self.adapter.query(request).await
    }

    /// Execute a vector query with connection pool enforcement.
    ///
    /// Same as `query_pooled()` but sends a pre-embedded vector to the upstream
    /// via `adapter.query_vector()`. Used for VectorOnly upstreams where the
    /// proxy embeds the query text before sending.
    pub async fn query_vector_pooled(
        &self,
        request: &QueryRequest,
        vector: &[f32],
    ) -> Result<QueryResponse, UpstreamError> {
        // Acquire connection from pool
        // PERF(R6): deferred — UpstreamError::Unavailable needs Cow<'static, str>
        let _conn = self.connection_pool.acquire().await.map_err(|e| match e {
            PoolError::QueueFull => UpstreamError::Unavailable("connection queue full".to_string()),
            PoolError::Timeout => UpstreamError::Timeout,
            PoolError::Shutdown => {
                UpstreamError::Unavailable("connection pool shutdown".to_string())
            }
        })?;

        // Execute vector query (connection is held until response is received)
        self.adapter.query_vector(request, vector).await
    }

    /// Get connection pool statistics.
    pub fn pool_stats(&self) -> ConnectionPoolSnapshot {
        self.connection_pool.stats()
    }

    /// Get the number of active connections.
    pub fn active_connections(&self) -> usize {
        self.connection_pool.active_connections()
    }

    /// Get the connection queue depth.
    pub fn queue_depth(&self) -> usize {
        self.connection_pool.queue_depth()
    }

    /// Check if the connection pool has available capacity.
    pub fn has_pool_capacity(&self) -> bool {
        self.connection_pool.has_capacity()
    }

    /// Get the max connections for this upstream.
    pub fn max_connections(&self) -> usize {
        self.connection_pool.max_connections()
    }

    /// Get available connection permits.
    pub fn available_connections(&self) -> usize {
        self.connection_pool.available_permits()
    }
}

/// Pool of upstream endpoints with load balancing.
pub struct UpstreamPool {
    /// All upstreams in the pool.
    upstreams: Vec<Arc<PooledUpstream>>,
    /// Upstreams pre-sorted by priority (for failover strategy).
    upstreams_by_priority: Vec<Arc<PooledUpstream>>,
    /// Index lookup by ID.
    by_id: DashMap<String, usize>,
    /// Load balancing strategy.
    strategy: LoadBalanceStrategy,
    /// Current index for round-robin.
    current_index: AtomicUsize,
}

impl UpstreamPool {
    /// Create a new upstream pool from configuration.
    pub fn new(
        configs: &[UpstreamEndpointConfig],
        strategy: LoadBalanceStrategy,
    ) -> Result<Self, String> {
        let upstreams: Vec<Arc<PooledUpstream>> = configs
            .iter()
            .map(|c| PooledUpstream::new(c).map(Arc::new))
            .collect::<Result<Vec<_>, _>>()?;

        let by_id = DashMap::new();
        for (i, upstream) in upstreams.iter().enumerate() {
            by_id.insert(upstream.id.clone(), i);
        }

        let mut upstreams_by_priority = upstreams.clone();
        upstreams_by_priority.sort_by_key(|u| u.priority);

        Ok(Self {
            upstreams,
            upstreams_by_priority,
            by_id,
            strategy,
            current_index: AtomicUsize::new(0),
        })
    }

    /// Create an empty pool.
    pub fn empty() -> Self {
        Self {
            upstreams: Vec::new(),
            upstreams_by_priority: Vec::new(),
            by_id: DashMap::new(),
            strategy: LoadBalanceStrategy::RoundRobin,
            current_index: AtomicUsize::new(0),
        }
    }

    /// Check if the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.upstreams.is_empty()
    }

    /// Get the number of upstreams.
    pub fn len(&self) -> usize {
        self.upstreams.len()
    }

    /// Get an upstream by ID.
    pub fn get(&self, id: &str) -> Option<Arc<PooledUpstream>> {
        self.by_id
            .get(id)
            .and_then(|i| self.upstreams.get(*i).cloned())
    }

    /// Get all upstreams.
    pub fn all(&self) -> &[Arc<PooledUpstream>] {
        &self.upstreams
    }

    /// Get all healthy upstreams.
    pub fn healthy(&self) -> Vec<Arc<PooledUpstream>> {
        self.upstreams
            .iter()
            .filter(|u| u.is_healthy())
            .cloned()
            .collect()
    }

    /// Get the load balancing strategy.
    pub fn strategy(&self) -> LoadBalanceStrategy {
        self.strategy
    }

    /// Get all available upstreams (healthy or degraded).
    pub fn available(&self) -> Vec<Arc<PooledUpstream>> {
        self.upstreams
            .iter()
            .filter(|u| u.is_available())
            .cloned()
            .collect()
    }

    /// Select the next upstream based on the load balancing strategy.
    pub fn select(&self) -> Option<Arc<PooledUpstream>> {
        let available = self.available();
        self.select_from_available(&available)
    }

    /// Select an upstream matching a predicate, using the configured strategy.
    ///
    /// Single-pass: iterates `self.upstreams` once, collecting items that are
    /// both available and match `predicate`, then delegates to
    /// `select_from_available` for strategy dispatch.
    fn select_where(
        &self,
        predicate: impl Fn(&PooledUpstream) -> bool,
    ) -> Option<Arc<PooledUpstream>> {
        let filtered: Vec<_> = self
            .upstreams
            .iter()
            .filter(|u| u.is_available() && predicate(u))
            .cloned()
            .collect();
        self.select_from_available(&filtered)
    }

    /// Select from a pre-computed available list using the configured strategy.
    fn select_from_available(
        &self,
        available: &[Arc<PooledUpstream>],
    ) -> Option<Arc<PooledUpstream>> {
        if available.is_empty() {
            return None;
        }

        match self.strategy {
            LoadBalanceStrategy::RoundRobin => self.select_round_robin(available),
            LoadBalanceStrategy::Weighted => self.select_weighted(available),
            LoadBalanceStrategy::Random => self.select_random(available),
            LoadBalanceStrategy::Failover => self.select_failover(available),
        }
    }

    fn select_round_robin(&self, available: &[Arc<PooledUpstream>]) -> Option<Arc<PooledUpstream>> {
        if available.is_empty() {
            return None;
        }
        let counter = self.current_index.fetch_add(1, Ordering::Relaxed);
        let len = available.len();
        // INVARIANT: len > 0 guaranteed by is_empty check above — checked_rem always returns Some
        let index = counter.checked_rem(len).unwrap_or(0);
        available.get(index).cloned()
    }

    fn select_weighted(&self, available: &[Arc<PooledUpstream>]) -> Option<Arc<PooledUpstream>> {
        if available.is_empty() {
            return None;
        }

        let total_weight: u32 = available
            .iter()
            .map(|u| u.weight)
            .fold(0u32, |a, b| a.saturating_add(b));
        if total_weight == 0 {
            return self.select_round_robin(available);
        }

        // Use counter to distribute based on weights
        let counter = self.current_index.fetch_add(1, Ordering::Relaxed) as u32;
        // INVARIANT: total_weight > 0 guaranteed by check above — checked_rem always returns Some
        let index = counter.checked_rem(total_weight).unwrap_or(0);
        let mut accumulated = 0u32;

        for upstream in available {
            accumulated = accumulated.saturating_add(upstream.weight);
            if index < accumulated {
                return Some(upstream.clone());
            }
        }

        // Fallback to last
        available.last().cloned()
    }

    fn select_random(&self, available: &[Arc<PooledUpstream>]) -> Option<Arc<PooledUpstream>> {
        if available.is_empty() {
            return None;
        }

        // Simple pseudo-random using counter
        let seed = self.current_index.fetch_add(1, Ordering::Relaxed);
        let hash = seed.wrapping_mul(2654435761);
        // INVARIANT: available.len() > 0 guaranteed by is_empty check above — checked_rem always returns Some
        let index = hash.checked_rem(available.len()).unwrap_or(0);
        available.get(index).cloned()
    }

    fn select_failover(&self, _available: &[Arc<PooledUpstream>]) -> Option<Arc<PooledUpstream>> {
        // Scan pre-sorted list: first available+healthy, else first available, else first in list.
        self.upstreams_by_priority
            .iter()
            .find(|u| u.is_available() && u.is_healthy())
            .cloned()
            .or_else(|| {
                self.upstreams_by_priority
                    .iter()
                    .find(|u| u.is_available())
                    .cloned()
            })
    }

    /// Execute a query with automatic failover and connection pooling.
    ///
    /// Tries the selected upstream first, then falls back to other available upstreams.
    /// Respects `max_concurrent` limits via connection pool (pgbouncer-style).
    pub async fn query(&self, request: &QueryRequest) -> Result<QueryResponse, UpstreamError> {
        let available = self.available();

        // When all upstreams are offline, probe them as a last resort for recovery.
        // Without this fallback, offline upstreams can never recover since no queries
        // reach them and there's no periodic health check at the pool level.
        let candidates: Vec<Arc<PooledUpstream>> = if available.is_empty() {
            if self.upstreams.is_empty() {
                // PERF(R6): deferred — needs Cow<'static, str>
                return Err(UpstreamError::Unavailable(
                    "No upstreams configured".to_string(),
                ));
            }
            self.upstreams.to_vec()
        } else {
            available
        };

        // If upstream_id is specified, route directly to that upstream
        if let Some(ref target_id) = request.upstream_id {
            if let Some(target) = self.get(target_id) {
                match target.query_pooled(request).await {
                    Ok(mut response) => {
                        for r in &mut response.results {
                            r.upstream_id = Some(target.id.clone());
                        }
                        target.record_success();
                        return Ok(response);
                    }
                    Err(e) => {
                        target.record_failure();
                        return Err(e);
                    }
                }
            }
            // Target not found — fall through to normal selection
        }

        // Try primary selection first (reuse candidates to avoid double available() call)
        if let Some(primary) = self.select_from_available(&candidates) {
            match primary.query_pooled(request).await {
                Ok(mut response) => {
                    for r in &mut response.results {
                        r.upstream_id = Some(primary.id.clone());
                    }
                    primary.record_success();
                    return Ok(response);
                }
                Err(e) => {
                    primary.record_failure();
                    // Continue to try other upstreams
                    if candidates.len() == 1 {
                        return Err(e);
                    }
                }
            }
        }

        // Try remaining upstreams (using pooled query)
        for upstream in candidates.iter() {
            match upstream.query_pooled(request).await {
                Ok(mut response) => {
                    for r in &mut response.results {
                        r.upstream_id = Some(upstream.id.clone());
                    }
                    upstream.record_success();
                    return Ok(response);
                }
                Err(_) => {
                    upstream.record_failure();
                    continue;
                }
            }
        }

        // PERF(R6): deferred — needs Cow<'static, str>
        Err(UpstreamError::Unavailable(
            "All upstreams failed".to_string(),
        ))
    }

    /// Get pool statistics including connection pool metrics.
    ///
    /// Single-pass aggregation over all upstreams.
    pub fn stats(&self) -> PoolStats {
        let total = self.upstreams.len();
        let mut healthy = 0usize;
        let mut degraded = 0usize;
        let mut offline = 0usize;
        let mut total_requests = 0u64;
        let mut total_failures = 0u64;
        let mut active_connections = 0usize;
        let mut total_queue_depth = 0usize;
        let mut total_max_connections = 0usize;
        let mut fts = 0usize;
        let mut vector_db = 0usize;
        let mut hybrid = 0usize;
        let mut type_unknown = 0usize;

        for u in &self.upstreams {
            match u.health.status() {
                UpstreamStatus::Online => healthy = healthy.saturating_add(1),
                UpstreamStatus::Degraded => degraded = degraded.saturating_add(1),
                UpstreamStatus::Offline => offline = offline.saturating_add(1),
            }
            total_requests = total_requests.saturating_add(u.request_count());
            total_failures = total_failures.saturating_add(u.failure_count());
            active_connections = active_connections.saturating_add(u.active_connections());
            total_queue_depth = total_queue_depth.saturating_add(u.queue_depth());
            total_max_connections = total_max_connections.saturating_add(u.max_connections());
            match u.upstream_type() {
                UpstreamType::FullTextSearch => fts = fts.saturating_add(1),
                UpstreamType::VectorDatabase => vector_db = vector_db.saturating_add(1),
                UpstreamType::Hybrid => hybrid = hybrid.saturating_add(1),
                UpstreamType::Unknown => type_unknown = type_unknown.saturating_add(1),
            }
        }

        PoolStats {
            total_upstreams: total,
            healthy_upstreams: healthy,
            degraded_upstreams: degraded,
            offline_upstreams: offline,
            total_requests,
            total_failures,
            failure_rate: if total_requests > 0 {
                total_failures as f64 / total_requests as f64
            } else {
                0.0
            },
            active_connections,
            total_queue_depth,
            total_max_connections,
            pool_utilization: if total_max_connections > 0 {
                active_connections as f64 / total_max_connections as f64
            } else {
                0.0
            },
            type_counts: UpstreamTypeCounts {
                fts,
                vector_db,
                hybrid,
                unknown: type_unknown,
            },
        }
    }

    // === QueryMode Support ===

    /// Get all upstreams that support text queries natively.
    ///
    /// Returns upstreams with QueryMode::TextNative.
    pub fn text_native(&self) -> Vec<Arc<PooledUpstream>> {
        self.upstreams
            .iter()
            .filter(|u| u.query_mode() == QueryMode::TextNative)
            .cloned()
            .collect()
    }

    /// Get all upstreams that require embedding.
    ///
    /// Returns upstreams with QueryMode::VectorOnly.
    pub fn vector_only(&self) -> Vec<Arc<PooledUpstream>> {
        self.upstreams
            .iter()
            .filter(|u| u.query_mode() == QueryMode::VectorOnly)
            .cloned()
            .collect()
    }

    /// Get all upstreams with unknown query mode.
    ///
    /// These need to be probed to discover their capability.
    pub fn unknown_mode(&self) -> Vec<Arc<PooledUpstream>> {
        self.upstreams
            .iter()
            .filter(|u| u.query_mode() == QueryMode::Unknown)
            .cloned()
            .collect()
    }

    /// Get all upstreams with a specific query mode.
    pub fn with_mode(&self, mode: QueryMode) -> Vec<Arc<PooledUpstream>> {
        self.upstreams
            .iter()
            .filter(|u| u.query_mode() == mode)
            .cloned()
            .collect()
    }

    /// Select an available upstream that supports text queries.
    pub fn select_text_native(&self) -> Option<Arc<PooledUpstream>> {
        self.select_where(|u| u.supports_text())
    }

    /// Select an available upstream that requires embedding.
    pub fn select_vector_only(&self) -> Option<Arc<PooledUpstream>> {
        self.select_where(|u| u.requires_embedding())
    }

    /// Get count of upstreams by query mode.
    pub fn mode_counts(&self) -> QueryModeCounts {
        let mut text_native = 0usize;
        let mut vector_only = 0usize;
        let mut unknown = 0usize;

        for upstream in &self.upstreams {
            match upstream.query_mode() {
                QueryMode::TextNative => text_native = text_native.saturating_add(1),
                QueryMode::VectorOnly => vector_only = vector_only.saturating_add(1),
                QueryMode::Unknown => unknown = unknown.saturating_add(1),
            }
        }

        QueryModeCounts {
            text_native,
            vector_only,
            unknown,
        }
    }

    // === UpstreamType Support ===

    /// Get all FTS (full-text search) upstreams.
    pub fn fts_upstreams(&self) -> Vec<Arc<PooledUpstream>> {
        self.upstreams
            .iter()
            .filter(|u| u.is_fts())
            .cloned()
            .collect()
    }

    /// Get all VectorDB upstreams.
    pub fn vector_db_upstreams(&self) -> Vec<Arc<PooledUpstream>> {
        self.upstreams
            .iter()
            .filter(|u| u.is_vector_db())
            .cloned()
            .collect()
    }

    /// Get all upstreams with unknown type.
    pub fn unknown_type_upstreams(&self) -> Vec<Arc<PooledUpstream>> {
        self.upstreams
            .iter()
            .filter(|u| !u.is_type_known())
            .cloned()
            .collect()
    }

    /// Get all upstreams of a specific type.
    pub fn with_type(&self, upstream_type: UpstreamType) -> Vec<Arc<PooledUpstream>> {
        self.upstreams
            .iter()
            .filter(|u| u.upstream_type() == upstream_type)
            .cloned()
            .collect()
    }

    /// Select an available FTS upstream using the configured strategy.
    pub fn select_fts(&self) -> Option<Arc<PooledUpstream>> {
        self.select_where(|u| u.is_fts())
    }

    /// Select an available VectorDB upstream using the configured strategy.
    pub fn select_vector_db(&self) -> Option<Arc<PooledUpstream>> {
        self.select_where(|u| u.is_vector_db())
    }

    /// Get count of upstreams by type.
    pub fn type_counts(&self) -> UpstreamTypeCounts {
        let mut fts = 0usize;
        let mut vector_db = 0usize;
        let mut hybrid = 0usize;
        let mut unknown = 0usize;

        for upstream in &self.upstreams {
            match upstream.upstream_type() {
                UpstreamType::FullTextSearch => fts = fts.saturating_add(1),
                UpstreamType::VectorDatabase => vector_db = vector_db.saturating_add(1),
                UpstreamType::Hybrid => hybrid = hybrid.saturating_add(1),
                UpstreamType::Unknown => unknown = unknown.saturating_add(1),
            }
        }

        UpstreamTypeCounts {
            fts,
            vector_db,
            hybrid,
            unknown,
        }
    }
}

/// Counts of upstreams by query mode.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct QueryModeCounts {
    /// Upstreams that support text queries natively.
    pub text_native: usize,
    /// Upstreams that require embedding.
    pub vector_only: usize,
    /// Upstreams with unknown capability.
    pub unknown: usize,
}

/// Counts of upstreams by backend type.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct UpstreamTypeCounts {
    /// Full-text search upstreams (Elasticsearch, OpenSearch, etc.).
    pub fts: usize,
    /// Vector database upstreams (Qdrant, Pinecone, etc.).
    pub vector_db: usize,
    /// Hybrid upstreams (supports both FTS and vector).
    pub hybrid: usize,
    /// Upstreams with unknown type.
    pub unknown: usize,
}

/// Statistics for the upstream pool.
#[derive(Debug, Clone, Serialize)]
pub struct PoolStats {
    /// Total number of upstreams.
    pub total_upstreams: usize,
    /// Number of healthy upstreams.
    pub healthy_upstreams: usize,
    /// Number of degraded upstreams.
    pub degraded_upstreams: usize,
    /// Number of offline upstreams.
    pub offline_upstreams: usize,
    /// Total requests across all upstreams.
    pub total_requests: u64,
    /// Total failures across all upstreams.
    pub total_failures: u64,
    /// Overall failure rate.
    pub failure_rate: f64,
    // Connection pool aggregate stats
    /// Total active connections across all upstreams.
    pub active_connections: usize,
    /// Total queue depth across all upstreams.
    pub total_queue_depth: usize,
    /// Total max connections capacity.
    pub total_max_connections: usize,
    /// Connection pool utilization (active / max).
    pub pool_utilization: f64,
    /// Counts by upstream type (FTS, VectorDB, Hybrid, Unknown).
    pub type_counts: UpstreamTypeCounts,
}

#[cfg(test)]
#[path = "tests/pool_tests.rs"]
mod tests;
