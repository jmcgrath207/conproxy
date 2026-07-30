/// Default proxy URL for E2E tests (HTTP REST port = gRPC port + 1).
pub const PROXY_URL_DEFAULT: &str = "http://127.0.0.1:8081";

/// Qdrant URL for E2E tests.
pub const QDRANT_URL_DEFAULT: &str = "http://localhost:6333";

/// Elasticsearch URL for E2E tests.
pub const ELASTIC_URL_DEFAULT: &str = "http://localhost:9200";

/// OpenSearch URL for E2E tests.
pub const OPENSEARCH_URL_DEFAULT: &str = "http://localhost:9201";

/// Meilisearch URL #1 for E2E tests.
pub const MEILI1_URL_DEFAULT: &str = "http://localhost:7700";

/// Meilisearch URL #2 for E2E tests.
pub const MEILI2_URL_DEFAULT: &str = "http://localhost:7701";

/// Proxy URL — overridable via `PROXY_URL` env var (used for k8s mode).
#[allow(dead_code)]
pub fn proxy_url() -> &'static str {
    static OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    OVERRIDE
        .get_or_init(|| {
            std::env::var("PROXY_URL").unwrap_or_else(|_| PROXY_URL_DEFAULT.to_string())
        })
        .as_str()
}

#[allow(dead_code)]
pub fn qdrant_url() -> &'static str {
    static OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    OVERRIDE
        .get_or_init(|| {
            std::env::var("QDRANT_URL").unwrap_or_else(|_| QDRANT_URL_DEFAULT.to_string())
        })
        .as_str()
}

#[allow(dead_code)]
pub fn elastic_url() -> &'static str {
    static OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    OVERRIDE
        .get_or_init(|| {
            std::env::var("ELASTIC_URL").unwrap_or_else(|_| ELASTIC_URL_DEFAULT.to_string())
        })
        .as_str()
}

#[allow(dead_code)]
pub fn opensearch_url() -> &'static str {
    static OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    OVERRIDE
        .get_or_init(|| {
            std::env::var("OPENSEARCH_URL").unwrap_or_else(|_| OPENSEARCH_URL_DEFAULT.to_string())
        })
        .as_str()
}

#[allow(dead_code)]
pub fn meili1_url() -> &'static str {
    static OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    OVERRIDE
        .get_or_init(|| {
            std::env::var("MEILI1_URL").unwrap_or_else(|_| MEILI1_URL_DEFAULT.to_string())
        })
        .as_str()
}

#[allow(dead_code)]
pub fn meili2_url() -> &'static str {
    static OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    OVERRIDE
        .get_or_init(|| {
            std::env::var("MEILI2_URL").unwrap_or_else(|_| MEILI2_URL_DEFAULT.to_string())
        })
        .as_str()
}

/// True when tests should NOT spawn a local proxy and instead connect to the
/// URL given by `PROXY_URL` (k8s mode: proxy runs in a kind cluster, port-forwarded).
#[allow(dead_code)]
pub fn external_proxy() -> bool {
    std::env::var("E2E_EXTERNAL_PROXY").ok().as_deref() == Some("1")
}

/// Shared query strings for cache reuse across test sections.
pub const SHARED_QUERIES: [&str; 10] = [
    "rust programming language",
    "elasticsearch full text search",
    "vector databases similarity search",
    "cache ttl lru eviction",
    "circuit breaker pattern distributed systems",
    "load balancing failover",
    "axum web framework rust",
    "BM25 scoring algorithm",
    "database migrations versioning",
    "error handling patterns",
];

/// E2E test suite type, determined by `E2E_SUITE` env var.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suite {
    Qdrant,
    /// Meilisearch fixtures (name `Elastic` kept for env var `E2E_SUITE=elastic` compat).
    Elastic,
    Mixed,
    All,
}

impl Suite {
    /// Read from `E2E_SUITE` environment variable, defaulting to `All`.
    pub fn from_env() -> Self {
        match std::env::var("E2E_SUITE").as_deref() {
            Ok("qdrant") => Suite::Qdrant,
            Ok("elastic") => Suite::Elastic,
            Ok("mixed") => Suite::Mixed,
            _ => Suite::All,
        }
    }

    pub fn has_multiple_upstreams(self) -> bool {
        matches!(self, Suite::Mixed | Suite::All)
    }

    pub fn is_all(self) -> bool {
        self == Suite::All
    }

    /// Is NOT qdrant-only (i.e. has text-capable upstreams like ES).
    pub fn has_text_upstreams(self) -> bool {
        self != Suite::Qdrant
    }

    /// Config file name in `tests/e2e/configs/`.
    pub fn config_name(self) -> &'static str {
        match self {
            Suite::Qdrant => "single-qdrant.toml",
            Suite::Elastic => "single-elasticsearch.toml",
            Suite::Mixed => "cascade-mixed.toml",
            Suite::All => "multi-elasticsearch.toml",
        }
    }
}

impl std::fmt::Display for Suite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Suite::Qdrant => write!(f, "qdrant"),
            Suite::Elastic => write!(f, "elastic"),
            Suite::Mixed => write!(f, "mixed"),
            Suite::All => write!(f, "all"),
        }
    }
}

/// Check if a test category is enabled by `E2E_FILTER` env var.
/// If `E2E_FILTER` is unset or empty, all categories are enabled.
pub fn category_enabled(name: &str) -> bool {
    match std::env::var("E2E_FILTER") {
        Ok(filter) if !filter.is_empty() => filter.split(',').any(|c| c.trim() == name),
        _ => true,
    }
}
