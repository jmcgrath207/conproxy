//! Cache Proxy module for remote RAG upstreams.
//!
//! Provides a pull-through cache proxy that sits between clients and
//! remote RAG/vector search services. Features include:
//!
//! - Query normalization and blake3 hashing for cache keys
//! - TTL-based freshness with fresh/stale/expired states
//! - Stale-while-revalidate pattern for low-latency responses
//! - Request coalescing (singleflight) for concurrent identical requests
//! - TTL jittering to prevent thundering herd on cache expiration
//! - Background refresh worker for proactive cache maintenance
//! - Config change detection for automatic cache invalidation
//! - Graceful degradation when upstream is unavailable
//!
//! # Architecture
//!
//! ```text
//! Client -> CacheProxy -> RequestCoalescer -> CacheStore (DashMap)
//!                    \-> GenericRestAdapter -> Upstream
//!                    \-> RefreshWorker (background)
//! ```

pub mod adaptive;
pub mod agent;
pub mod audit;
pub mod batch;
pub mod cache;
pub mod cascade;
pub mod cdc;
pub mod circuit;
pub mod client;
pub mod coalesce;
pub mod connection_pool;
pub mod context;
pub mod distill;
pub mod elasticsearch;
pub mod embedder_config;
pub mod federated;
pub mod grpc;
pub mod lifecycle;
pub mod meilisearch;
pub mod metrics;
pub mod middleware;
pub mod milvus;
pub mod observability;
pub mod peer;
#[cfg(feature = "persistence")]
pub mod persistence;
#[cfg(feature = "pgvector")]
pub mod pgvector;
pub mod pinecone;
pub mod pool;
pub mod priority;
pub mod qdrant;
pub mod query_stats;
pub mod refresh;
pub mod resilience;
pub mod retry;
#[cfg(all(feature = "linux-sandbox", target_os = "linux"))]
pub mod sandbox;
pub mod scope;
#[cfg(feature = "embed-api")]
pub mod semantic_cache;
pub mod server;
pub mod slug;
#[cfg(feature = "embed-api")]
pub mod smart_embedder;
pub mod socket_tuning;
pub mod tune;
pub mod types;
pub mod upstream;
pub mod workers;

pub use crate::config::ProxyConfig;
pub use adaptive::{AdaptiveTimeout, AdaptiveTimeoutConfig, AdaptiveTimeoutStats, TimeoutBudget};
pub use agent::{AgentEntry, AgentIdentity, AgentInfo, AgentLookup, AgentQuota, AgentRegistry};
pub use audit::{AuditBuilder, AuditEntry, AuditLog, AuditStats};
pub use batch::{
    BatchConfig, BatchError, BatchProcessor, BatchQuery, BatchQueryResult, BatchRequest,
    BatchResponse,
};
pub use cache::{
    CacheEntrySummary, CacheStats, CacheStore, DistillSnapshot, EntryFreshness, EvictionReason,
    EvictionStats, IntegrityReport, UpstreamCacheStats,
};
pub use cascade::{
    CascadeConfig, CascadeError, CascadeExecutor, CascadeResult, CascadeStopReason,
    UpstreamCascadeConfig, UpstreamScore,
};
pub use cdc::{
    CdcEvent, CdcEventBuilder, CdcEventType, CdcManager, CdcServiceImpl, EventSender,
    SharedEventSender,
};
pub use circuit::{CircuitBreaker, CircuitBreakerConfig, CircuitResult, CircuitState};
pub use client::{ClientConfig, ClientError, ProxyClient};
pub use coalesce::{CoalesceAction, RequestCoalescer};
pub use connection_pool::{
    ConnectionPool, ConnectionPoolConfig, ConnectionPoolSnapshot, PoolError, PooledConnection,
    PoolingMode,
};
pub use context::{
    ContextCache, ContextConfig, ContextError, ContextId, ContextManager, ContextMetadata,
    ContextPolicy, ContextStats, ContextStatsSnapshot, UpstreamType,
};
pub use distill::render_entry_md;
pub use elasticsearch::{ElasticsearchAdapter, ElasticsearchConfig};
pub use embedder_config::EmbedderConfig;
pub use federated::{
    FallbackDecision, FederatedResponse, FederatedResult, FederatedSearch, FederatedSearchConfig,
    FederatedStats, MergeMode, ResultSource,
};
pub use lifecycle::ProxyError;
pub use metrics::{LatencyTimer, MetricsSnapshot, ProxyMetrics};
pub use middleware::{
    AuthConfig, AuthErrorResponse, RateLimitConfig, RateLimitResponse, RateLimiter,
};
pub use milvus::{MilvusAdapter, MilvusConfig};
pub use observability::{
    CacheMutation, CacheMutationLog, ClearScope, MutationAuditEntry, MutationEvictionReason,
    RequestId, RequestTrace, TraceBuilder, TraceStage,
};
pub use peer::{
    CoalesceDecision, DeduplicateResult, DistributedCoalescer, PeerManager, PeerReceiver,
    PeerReplicationStats, PeerReplicationStatsSnapshot, PeerServiceImpl, PeerState,
};
#[cfg(feature = "persistence")]
pub use persistence::{
    PersistedEntry, PersistedResponse, PersistedResult, PersistedUpstreamHealth, PersistenceError,
    PersistenceStats, PersistentCache,
};
#[cfg(feature = "pgvector")]
pub use pgvector::{DistanceMetric, PgvectorAdapter, PgvectorConfig};
pub use pinecone::{PineconeAdapter, PineconeConfig};
pub use pool::{
    LoadBalanceStrategy, PoolStats, PooledUpstream, QueryModeCounts, UpstreamPool,
    UpstreamTypeCounts,
};
pub use priority::{PrioritizedRequest, Priority, PriorityQueue, QueueStats};
pub use qdrant::{QdrantAdapter, QdrantConfig};
pub use query_stats::{AggregateStats, QueryStats, QueryStatsSnapshot, QueryStatsTracker};
pub use refresh::{QueryTrackingRefreshWorker, RefreshWorker};
pub use resilience::{ResilienceConfig, ResilienceSnapshot, UpstreamState, UpstreamStateManager};
pub use retry::{
    RetryCondition, RetryError, RetryExecutor, RetryPolicy, RetryResult, RetryableError,
};
#[cfg(all(feature = "linux-sandbox", target_os = "linux"))]
pub use sandbox::{
    apply_sandbox, current_gid, current_uid, get_status, is_no_new_privs_set, is_root,
    needs_sandbox, SandboxConfig, SandboxError, SandboxStatus,
};
pub use scope::{DiscardReason, DiscardedResult, FilterMode, FilterStats, ScopeFilter};
pub use server::CacheProxy;
pub use slug::slugify;
#[cfg(feature = "embed-api")]
pub use smart_embedder::{SmartEmbedder, SmartEmbedderStats};
pub use types::{
    detect_drift, CacheEntry, CacheStatus, DriftAggregator, DriftLevel, DriftObservation,
    DriftSummary, Freshness, QueryHash, QueryRequest, QueryResponse, ResponseValidationError,
    SchemaFingerprint, SearchResult, ValidationError, DEFAULT_TOP_K, MAX_CONTENT_LENGTH,
    MAX_QUERY_LENGTH, MAX_RESPONSE_SIZE, MAX_RESULTS, MAX_TOP_K,
};
pub use upstream::{
    AdapterMetadata, GenericRestAdapter, HealthTracker, QueryMode, UpstreamAdapter, UpstreamError,
    UpstreamStatus,
};
pub use workers::{
    CleanupConfig, CleanupWorker, HealthCheckConfig, HealthCheckWorker, RecoveryConfig,
    RecoveryState, RecoveryTracker, RecoveryWorker, UpstreamVersion, VersionCheckConfig,
    VersionCheckWorker,
};
