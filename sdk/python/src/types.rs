use pyo3::prelude::*;

// -- Search types --

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PySearchResult {
    #[pyo3(get)]
    pub id: String,
    #[pyo3(get)]
    pub score: f32,
    #[pyo3(get)]
    pub content: String,
    #[pyo3(get)]
    pub metadata_json: Option<String>,
    #[pyo3(get)]
    pub upstream_id: String,
}

impl From<conproxy_sdk::proto::SearchResult> for PySearchResult {
    fn from(r: conproxy_sdk::proto::SearchResult) -> Self {
        let metadata_json = if r.metadata_json.is_empty() {
            None
        } else {
            String::from_utf8(r.metadata_json).ok()
        };
        Self {
            id: r.id,
            score: r.score,
            content: r.content,
            metadata_json,
            upstream_id: r.upstream_id,
        }
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyQueryResponse {
    #[pyo3(get)]
    pub results: Vec<PySearchResult>,
    #[pyo3(get)]
    pub cache_status: i32,
    #[pyo3(get)]
    pub took_ms: u64,
    #[pyo3(get)]
    pub generated_at: u64,
}

impl From<conproxy_sdk::proto::QueryResponse> for PyQueryResponse {
    fn from(r: conproxy_sdk::proto::QueryResponse) -> Self {
        Self {
            results: r.results.into_iter().map(PySearchResult::from).collect(),
            cache_status: r.cache_status,
            took_ms: r.took_ms,
            generated_at: r.generated_at,
        }
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyBatchQueryResponse {
    #[pyo3(get)]
    pub took_ms: u64,
    #[pyo3(get)]
    pub success_count: u32,
    #[pyo3(get)]
    pub error_count: u32,
    #[pyo3(get)]
    pub complete: bool,
}

impl From<conproxy_sdk::proto::BatchQueryResponse> for PyBatchQueryResponse {
    fn from(r: conproxy_sdk::proto::BatchQueryResponse) -> Self {
        Self {
            took_ms: r.took_ms,
            success_count: r.success_count,
            error_count: r.error_count,
            complete: r.complete,
        }
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyFederatedStats {
    #[pyo3(get)]
    pub local_count: u32,
    #[pyo3(get)]
    pub remote_count: u32,
    #[pyo3(get)]
    pub merged_count: u32,
    #[pyo3(get)]
    pub local_latency_ms: u64,
    #[pyo3(get)]
    pub remote_latency_ms: u64,
    #[pyo3(get)]
    pub fallback_reason: String,
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyFederatedQueryResponse {
    #[pyo3(get)]
    pub results: Vec<PySearchResult>,
    #[pyo3(get)]
    pub cache_status: i32,
    #[pyo3(get)]
    pub took_ms: u64,
    #[pyo3(get)]
    pub stats: Option<PyFederatedStats>,
}

impl From<conproxy_sdk::proto::FederatedQueryResponse> for PyFederatedQueryResponse {
    fn from(r: conproxy_sdk::proto::FederatedQueryResponse) -> Self {
        Self {
            results: r.results.into_iter().map(PySearchResult::from).collect(),
            cache_status: r.cache_status,
            took_ms: r.took_ms,
            stats: r.stats.map(|s| PyFederatedStats {
                local_count: s.local_count,
                remote_count: s.remote_count,
                merged_count: s.merged_count,
                local_latency_ms: s.local_latency_ms,
                remote_latency_ms: s.remote_latency_ms,
                fallback_reason: s.fallback_reason,
            }),
        }
    }
}

// -- Stats/Observability types --

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyStatsResponse {
    #[pyo3(get)]
    pub uptime_secs: u64,
    #[pyo3(get)]
    pub cache_entries: u64,
    #[pyo3(get)]
    pub total_hits: u64,
    #[pyo3(get)]
    pub total_misses: u64,
    #[pyo3(get)]
    pub hit_rate: f64,
    #[pyo3(get)]
    pub upstream_requests: u64,
    #[pyo3(get)]
    pub upstream_failures: u64,
    #[pyo3(get)]
    pub upstream_error_rate: f64,
    #[pyo3(get)]
    pub degradation_level: String,
    #[pyo3(get)]
    pub paused: bool,
}

impl From<conproxy_sdk::proto::StatsResponse> for PyStatsResponse {
    fn from(r: conproxy_sdk::proto::StatsResponse) -> Self {
        Self {
            uptime_secs: r.uptime_secs,
            cache_entries: r.cache_entries,
            total_hits: r.total_hits,
            total_misses: r.total_misses,
            hit_rate: r.hit_rate,
            upstream_requests: r.upstream_requests,
            upstream_failures: r.upstream_failures,
            upstream_error_rate: r.upstream_error_rate,
            degradation_level: r.degradation_level,
            paused: r.paused,
        }
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyCircuitStatusResponse {
    #[pyo3(get)]
    pub state: String,
    #[pyo3(get)]
    pub failure_count: u64,
    #[pyo3(get)]
    pub success_count: u64,
    #[pyo3(get)]
    pub consecutive_failures: u64,
}

impl From<conproxy_sdk::proto::CircuitStatusResponse> for PyCircuitStatusResponse {
    fn from(r: conproxy_sdk::proto::CircuitStatusResponse) -> Self {
        Self {
            state: r.state,
            failure_count: r.failure_count,
            success_count: r.success_count,
            consecutive_failures: r.consecutive_failures,
        }
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyQueueStatsResponse {
    #[pyo3(get)]
    pub pending: u64,
    #[pyo3(get)]
    pub capacity: u64,
    #[pyo3(get)]
    pub utilization: f64,
}

impl From<conproxy_sdk::proto::QueueStatsResponse> for PyQueueStatsResponse {
    fn from(r: conproxy_sdk::proto::QueueStatsResponse) -> Self {
        Self {
            pending: r.pending,
            capacity: r.capacity,
            utilization: r.utilization,
        }
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyClientInfo {
    #[pyo3(get)]
    pub request_id: String,
    #[pyo3(get)]
    pub started_at_ms: u64,
    #[pyo3(get)]
    pub query: String,
    #[pyo3(get)]
    pub source: String,
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyClientsResponse {
    #[pyo3(get)]
    pub clients: Vec<PyClientInfo>,
    #[pyo3(get)]
    pub total_completed: u64,
    #[pyo3(get)]
    pub total_rejected: u64,
}

impl From<conproxy_sdk::proto::ClientsResponse> for PyClientsResponse {
    fn from(r: conproxy_sdk::proto::ClientsResponse) -> Self {
        Self {
            clients: r
                .clients
                .into_iter()
                .map(|c| PyClientInfo {
                    request_id: c.request_id,
                    started_at_ms: c.started_at_ms,
                    query: c.query,
                    source: c.source,
                })
                .collect(),
            total_completed: r.total_completed,
            total_rejected: r.total_rejected,
        }
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyUpstreamInfo {
    #[pyo3(get)]
    pub id: String,
    #[pyo3(get)]
    pub url: String,
    #[pyo3(get)]
    pub health_status: String,
    #[pyo3(get)]
    pub weight: u32,
    #[pyo3(get)]
    pub priority: u32,
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyPoolStatusResponse {
    #[pyo3(get)]
    pub total_upstreams: u32,
    #[pyo3(get)]
    pub healthy_upstreams: u32,
    #[pyo3(get)]
    pub degraded_upstreams: u32,
    #[pyo3(get)]
    pub offline_upstreams: u32,
    #[pyo3(get)]
    pub upstreams: Vec<PyUpstreamInfo>,
}

impl From<conproxy_sdk::proto::PoolStatusResponse> for PyPoolStatusResponse {
    fn from(r: conproxy_sdk::proto::PoolStatusResponse) -> Self {
        Self {
            total_upstreams: r.total_upstreams,
            healthy_upstreams: r.healthy_upstreams,
            degraded_upstreams: r.degraded_upstreams,
            offline_upstreams: r.offline_upstreams,
            upstreams: r
                .upstreams
                .into_iter()
                .map(|u| PyUpstreamInfo {
                    id: u.id,
                    url: u.url,
                    health_status: u.health_status,
                    weight: u.weight,
                    priority: u.priority,
                })
                .collect(),
        }
    }
}

// -- Admin types --

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyReloadResponse {
    #[pyo3(get)]
    pub success: bool,
    #[pyo3(get)]
    pub reloaded: Vec<String>,
    #[pyo3(get)]
    pub error: String,
    #[pyo3(get)]
    pub message: String,
}

impl From<conproxy_sdk::proto::ReloadResponse> for PyReloadResponse {
    fn from(r: conproxy_sdk::proto::ReloadResponse) -> Self {
        Self {
            success: r.success,
            reloaded: r.reloaded,
            error: r.error,
            message: r.message,
        }
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyCacheClearResponse {
    #[pyo3(get)]
    pub cleared_entries: u64,
    #[pyo3(get)]
    pub message: String,
}

impl From<conproxy_sdk::proto::CacheClearResponse> for PyCacheClearResponse {
    fn from(r: conproxy_sdk::proto::CacheClearResponse) -> Self {
        Self {
            cleared_entries: r.cleared_entries,
            message: r.message,
        }
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyCacheWarmupResponse {
    #[pyo3(get)]
    pub warmed: u32,
    #[pyo3(get)]
    pub failed: u32,
    #[pyo3(get)]
    pub took_ms: u64,
}

impl From<conproxy_sdk::proto::CacheWarmupResponse> for PyCacheWarmupResponse {
    fn from(r: conproxy_sdk::proto::CacheWarmupResponse) -> Self {
        Self {
            warmed: r.warmed,
            failed: r.failed,
            took_ms: r.took_ms,
        }
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyCacheEvictResponse {
    #[pyo3(get)]
    pub evicted: u64,
    #[pyo3(get)]
    pub remaining: u64,
}

impl From<conproxy_sdk::proto::CacheEvictResponse> for PyCacheEvictResponse {
    fn from(r: conproxy_sdk::proto::CacheEvictResponse) -> Self {
        Self {
            evicted: r.evicted,
            remaining: r.remaining,
        }
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyCacheIntegrityResponse {
    #[pyo3(get)]
    pub total: u64,
    #[pyo3(get)]
    pub valid: u64,
    #[pyo3(get)]
    pub invalid: u64,
    #[pyo3(get)]
    pub no_hash: u64,
}

impl From<conproxy_sdk::proto::CacheIntegrityResponse> for PyCacheIntegrityResponse {
    fn from(r: conproxy_sdk::proto::CacheIntegrityResponse) -> Self {
        Self {
            total: r.total,
            valid: r.valid,
            invalid: r.invalid,
            no_hash: r.no_hash,
        }
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyAgentInfo {
    #[pyo3(get)]
    pub id: String,
    #[pyo3(get)]
    pub default_context: String,
    #[pyo3(get)]
    pub allowed_contexts: Vec<String>,
    #[pyo3(get)]
    pub priority_class: u32,
    #[pyo3(get)]
    pub rate_limit_rps: u32,
    #[pyo3(get)]
    pub enabled: bool,
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyListAgentsResponse {
    #[pyo3(get)]
    pub agents: Vec<PyAgentInfo>,
    #[pyo3(get)]
    pub total: u32,
}

impl From<conproxy_sdk::proto::ListAgentsResponse> for PyListAgentsResponse {
    fn from(r: conproxy_sdk::proto::ListAgentsResponse) -> Self {
        Self {
            agents: r
                .agents
                .into_iter()
                .map(|a| PyAgentInfo {
                    id: a.id,
                    default_context: a.default_context,
                    allowed_contexts: a.allowed_contexts,
                    priority_class: a.priority_class,
                    rate_limit_rps: a.rate_limit_rps,
                    enabled: a.enabled,
                })
                .collect(),
            total: r.total,
        }
    }
}

// -- Context types --

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyContextInfo {
    #[pyo3(get)]
    pub id: String,
    #[pyo3(get)]
    pub created_at_ms: u64,
    #[pyo3(get)]
    pub last_accessed_ms: u64,
    #[pyo3(get)]
    pub hits: u64,
    #[pyo3(get)]
    pub misses: u64,
}

impl From<conproxy_sdk::proto::ContextInfo> for PyContextInfo {
    fn from(r: conproxy_sdk::proto::ContextInfo) -> Self {
        Self {
            id: r.id,
            created_at_ms: r.created_at_ms,
            last_accessed_ms: r.last_accessed_ms,
            hits: r.hits,
            misses: r.misses,
        }
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyListContextsResponse {
    #[pyo3(get)]
    pub contexts: Vec<PyContextInfo>,
    #[pyo3(get)]
    pub current: String,
}

impl From<conproxy_sdk::proto::ListContextsResponse> for PyListContextsResponse {
    fn from(r: conproxy_sdk::proto::ListContextsResponse) -> Self {
        Self {
            contexts: r.contexts.into_iter().map(PyContextInfo::from).collect(),
            current: r.current,
        }
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PySwitchContextResponse {
    #[pyo3(get)]
    pub previous: String,
    #[pyo3(get)]
    pub current: String,
    #[pyo3(get)]
    pub message: String,
}

impl From<conproxy_sdk::proto::SwitchContextResponse> for PySwitchContextResponse {
    fn from(r: conproxy_sdk::proto::SwitchContextResponse) -> Self {
        Self {
            previous: r.previous,
            current: r.current,
            message: r.message,
        }
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyCreateContextResponse {
    #[pyo3(get)]
    pub context_id: String,
    #[pyo3(get)]
    pub message: String,
}

impl From<conproxy_sdk::proto::CreateContextResponse> for PyCreateContextResponse {
    fn from(r: conproxy_sdk::proto::CreateContextResponse) -> Self {
        Self {
            context_id: r.context_id,
            message: r.message,
        }
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyContextStats {
    #[pyo3(get)]
    pub context_id: String,
    #[pyo3(get)]
    pub cache_entries: u64,
    #[pyo3(get)]
    pub hits: u64,
    #[pyo3(get)]
    pub misses: u64,
    #[pyo3(get)]
    pub hit_rate: f64,
}

impl From<conproxy_sdk::proto::ContextStats> for PyContextStats {
    fn from(r: conproxy_sdk::proto::ContextStats) -> Self {
        Self {
            context_id: r.context_id,
            cache_entries: r.cache_entries,
            hits: r.hits,
            misses: r.misses,
            hit_rate: r.hit_rate,
        }
    }
}

// -- Config type --

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyDistillEntry {
    #[pyo3(get)]
    pub query: String,
    #[pyo3(get)]
    pub context_id: String,
    #[pyo3(get)]
    pub upstream_id: String,
    #[pyo3(get)]
    pub cached_at_ms: u64,
    #[pyo3(get)]
    pub extended_count: u32,
    #[pyo3(get)]
    pub response_json: Vec<u8>,
    #[pyo3(get)]
    pub hash_hex: String,
    #[pyo3(get)]
    pub embedding: Vec<f32>,
}

impl From<conproxy_sdk::proto::DistillEntry> for PyDistillEntry {
    fn from(r: conproxy_sdk::proto::DistillEntry) -> Self {
        Self {
            query: r.query,
            context_id: r.context_id,
            upstream_id: r.upstream_id,
            cached_at_ms: r.cached_at_ms,
            extended_count: r.extended_count,
            response_json: r.response_json,
            hash_hex: r.hash_hex,
            embedding: r.embedding,
        }
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PySdkConfig {
    #[pyo3(get, set)]
    pub grpc_url: String,
    #[pyo3(get, set)]
    pub http_url: String,
    #[pyo3(get, set)]
    pub api_key: Option<String>,
}
