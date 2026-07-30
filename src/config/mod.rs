//! Configuration loading and management
//!
//! Handles global (~/.conproxy/) and local (.conproxy/) configuration with merge logic.

mod context_rooted;

pub use context_rooted::{
    pool_resource_ids, project_to_proxy, resolve_all_contexts, resolve_embedder, resolve_leg,
    validate_context_rooted, ContextCacheConfig, ContextEmbedderConfig, ContextLegConfig,
    EmbedderResourceConfig, NamedContextConfig, ResolvedContext, ResolvedEmbedder,
    ResolvedUpstreamLeg, ServerConfig, UpstreamResourceConfig,
};

use crate::error::{ConproxyError, Result};

use crate::proxy::cascade::CascadeConfig;

use crate::proxy::circuit::CircuitBreakerConfig;

use crate::proxy::connection_pool::ConnectionPoolConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use std::time::Duration;

/// Serde helper for fields that default to `true`.
fn default_true() -> bool {
    true
}

/// Main configuration holder
#[derive(Debug, Clone)]
pub struct Config {
    /// The merged configuration
    pub config: ConfigFile,
    /// Path to local .conproxy directory (if exists)
    pub local_root: Option<PathBuf>,
}

/// Config file structure (used for both global and local)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigFile {
    /// REMOVED (plan 01): presence of `[llm]` is a hard error.
    #[serde(default, skip_serializing)]
    llm: Option<toml::Value>,

    #[serde(default)]
    pub packages: HashMap<String, PackageEntry>,

    #[serde(default)]
    pub registries: HashMap<String, String>,

    #[serde(default)]
    pub search: SearchConfig,

    #[serde(default)]
    pub embedding: EmbeddingConfig,

    #[serde(default)]
    pub web: WebConfig,

    #[serde(default)]
    pub proxy: ProxyConfig,

    #[serde(default)]
    pub context: ContextConfig,

    // --- Context-rooted config (plan 10) ---
    /// Process listen addresses (`[server]`). Optional; falls back to `[proxy].listen`.
    #[serde(default)]
    pub server: ServerConfig,

    /// Named upstream resources (`[upstreams.name]`).
    #[serde(default)]
    pub upstreams: HashMap<String, UpstreamResourceConfig>,

    /// Named embedder resources (`[embedders.name]`).
    #[serde(default)]
    pub embedders: HashMap<String, EmbedderResourceConfig>,

    /// Context policy units (`[contexts.name]`).
    #[serde(default)]
    pub contexts: HashMap<String, NamedContextConfig>,

    /// Top-level agents (`[[agents]]`). Merged with `proxy.agents` when both set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<AgentConfig>,
}

impl ConfigFile {
    /// Merge two configs (other overrides self)
    pub fn merge_with(&self, other: &ConfigFile) -> ConfigFile {
        ConfigFile {
            llm: None,
            // Packages: only from local (not merged)
            packages: if other.packages.is_empty() {
                self.packages.clone()
            } else {
                other.packages.clone()
            },

            // Registries: merged (local can add to global)
            registries: {
                let mut merged = self.registries.clone();
                merged.extend(other.registries.clone());
                merged
            },

            // Search: local overrides global
            search: other.search.merge_with(&self.search),

            // Embedding: local overrides global
            embedding: other.embedding.merge_with(&self.embedding),

            // Web: local overrides global
            web: other.web.merge_with(&self.web),

            // Proxy: local overrides global
            proxy: other.proxy.merge_with(&self.proxy),

            // Context: local overrides global
            context: other.context.merge_with(&self.context),

            server: other.server.merge_with(&self.server),
            upstreams: {
                let mut m = self.upstreams.clone();
                m.extend(other.upstreams.clone());
                m
            },
            embedders: {
                let mut m = self.embedders.clone();
                m.extend(other.embedders.clone());
                m
            },
            contexts: if other.contexts.is_empty() {
                self.contexts.clone()
            } else {
                other.contexts.clone()
            },
            agents: if other.agents.is_empty() {
                self.agents.clone()
            } else {
                other.agents.clone()
            },
        }
    }

    /// Create a default global config
    pub fn default_global() -> Self {
        ConfigFile {
            llm: None,
            packages: HashMap::new(),
            registries: HashMap::new(),
            search: SearchConfig::default(),
            embedding: EmbeddingConfig::default(),
            web: WebConfig::default(),
            proxy: ProxyConfig::default(),
            context: ContextConfig::default(),
            server: ServerConfig::default(),
            upstreams: HashMap::new(),
            embedders: HashMap::new(),
            contexts: HashMap::new(),
            agents: Vec::new(),
        }
    }

    /// Create a default local config
    pub fn default_local() -> Self {
        ConfigFile {
            llm: None,
            packages: HashMap::new(),
            registries: HashMap::new(),
            search: SearchConfig::default(),
            embedding: EmbeddingConfig::default(),
            web: WebConfig::default(),
            proxy: ProxyConfig::default(),
            context: ContextConfig::default(),
            server: ServerConfig::default(),
            upstreams: HashMap::new(),
            embedders: HashMap::new(),
            contexts: HashMap::new(),
            agents: Vec::new(),
        }
    }

    /// Validate the entire config. Called after deserialization or before writing.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.llm.is_some() {
            return Err(
                "[llm] / [[llm.providers]] removed (plan 01). Use an external gateway (e.g. LiteLLM) for provider proxy/routing. conproxy owns search cache and distill only.".into(),
            );
        }

        let proxy = &self.proxy;

        // Validate upstream endpoints
        for upstream in &proxy.upstreams {
            upstream.validate()?;

            // Validate upstream URL is parseable
            if !upstream.url.starts_with("http://")
                && !upstream.url.starts_with("https://")
                && !upstream.url.starts_with("postgres://")
                && !upstream.url.starts_with("postgresql://")
            {
                return Err(format!(
                    "upstream '{}': URL must start with http(s):// or postgres(ql)://, got '{}'",
                    upstream.id, upstream.url
                ));
            }

            // Validate upstream timeout
            if let Some(t) = upstream.timeout_secs {
                if t == 0 || t > 300 {
                    return Err(format!(
                        "upstream '{}': timeout_secs must be 1..=300, got {}",
                        upstream.id, t
                    ));
                }
            }
        }

        // Validate legacy upstream_url if set
        if let Some(ref url_str) = proxy.upstream_url {
            if !url_str.starts_with("http://") && !url_str.starts_with("https://") {
                return Err(format!(
                    "proxy.upstream_url: must start with http(s)://, got '{}'",
                    url_str
                ));
            }
        }

        // Validate upstream_timeout_secs
        if let Some(t) = proxy.upstream_timeout_secs {
            if t == 0 || t > 300 {
                return Err(format!(
                    "proxy.upstream_timeout_secs must be 1..=300, got {}",
                    t
                ));
            }
        }

        // Validate cache max_entries
        if let Some(max) = proxy.max_entries {
            if max == 0 {
                return Err("proxy.max_entries must be > 0".to_string());
            }
        }

        // Validate pool max_connections
        if proxy.pool.max_connections == 0 || proxy.pool.max_connections > 10_000 {
            return Err(format!(
                "proxy.pool.max_connections must be 1..=10000, got {}",
                proxy.pool.max_connections
            ));
        }

        // Validate listen backlog
        if proxy.socket_tuning.listen_backlog > 65535 {
            return Err(format!(
                "proxy.socket_tuning.listen_backlog must be <= 65535, got {}",
                proxy.socket_tuning.listen_backlog
            ));
        }

        // Validate TTL / duration values
        if let Some(secs) = proxy.fresh_duration_secs {
            if secs == 0 {
                return Err("proxy.fresh_duration_secs must be > 0".to_string());
            }
        }
        if let Some(secs) = proxy.stale_duration_secs {
            if secs == 0 {
                return Err("proxy.stale_duration_secs must be > 0".to_string());
            }
        }
        if let Some(secs) = proxy.refresh_interval_secs {
            if secs == 0 {
                return Err("proxy.refresh_interval_secs must be > 0".to_string());
            }
        }

        // Validate distill sub-config
        proxy.distill.validate()?;

        // Validate federated search config
        proxy.federated.validate()?;

        // Validate scope config
        proxy.scope.validate()?;

        // Validate rate limit config
        proxy.rate_limit.validate()?;

        // Validate retry config
        proxy.retry.validate()?;

        // Validate cache config (per-upstream limits)
        proxy.cache.validate()?;

        // Validate security config
        proxy.security.validate()?;

        // Validate circuit breaker config
        proxy.circuit_breaker.validate()?;

        // Context-rooted tables (plan 10); no-op when [contexts] empty
        let agents_for_ctx: Vec<AgentConfig> = if self.agents.is_empty() {
            proxy.agents.clone()
        } else {
            self.agents.clone()
        };
        validate_context_rooted(
            &self.upstreams,
            &self.embedders,
            &self.contexts,
            &agents_for_ctx,
        )?;

        Ok(())
    }

    /// Whether this file uses context-rooted tables (`[contexts]` non-empty).
    pub fn is_context_rooted(&self) -> bool {
        !self.contexts.is_empty()
    }

    /// Scope phrases from the default context (context-rooted) or legacy proxy scope.
    #[must_use]
    pub fn effective_scope_seeds(&self) -> Vec<String> {
        if let Some(default_ctx) = self.contexts.values().find(|c| c.default == Some(true)) {
            default_ctx.scope.phrase_texts()
        } else {
            self.proxy.scope.phrase_texts()
        }
    }

    /// Resolve all contexts (empty if legacy-only config).
    ///
    /// # Errors
    /// Propagates `resolve_all_contexts` / missing-ref errors.
    pub fn resolve_contexts(&self) -> std::result::Result<Vec<ResolvedContext>, String> {
        resolve_all_contexts(&self.upstreams, &self.embedders, &self.contexts)
    }

    /// Runtime `ProxyConfig`: project default context from context-rooted config.
    ///
    /// # Errors
    /// Resolve / project failures.
    pub fn effective_proxy(&self) -> std::result::Result<ProxyConfig, String> {
        let mut base = self.proxy.clone();
        base.normalize_upstreams();

        // Overlay [server] listen onto proxy listen fields.
        if let Some(ref l) = self.server.listen {
            base.listen = Some(l.clone());
        }
        if let Some(ref h) = self.server.http_listen {
            base.http_listen = Some(h.clone());
        }

        let resolved = self.resolve_contexts()?;
        let mut projected = project_to_proxy(&base, &resolved)?;
        // Top-level agents win over proxy.agents when set.
        if !self.agents.is_empty() {
            projected.agents = self.agents.clone();
        }
        Ok(projected)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageEntry {
    pub git: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_hybrid: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_limit: Option<usize>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_index_on_install: Option<bool>,

    /// Federated search configuration (local-first with remote fallback).
    #[serde(default)]
    pub federated: FederatedConfig,
}

/// Configuration for federated search (local-first with remote fallback).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FederatedConfig {
    /// Enable federated search mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    /// Minimum local results before considering "good enough" (default: 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_local_results: Option<usize>,

    /// Minimum confidence score for local results (default: 0.7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_local_confidence: Option<f32>,

    /// Always fallback if zero local results (default: true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_on_empty: Option<bool>,

    /// Fallback if below min_local_confidence (default: true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_on_low_confidence: Option<bool>,

    /// Merge mode: "local_only_fallback", "local_priority", "remote_priority", "interleave".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_mode: Option<String>,

    /// Maximum results after merging (default: 10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_merged_results: Option<usize>,
}

impl FederatedConfig {
    fn merge_with(&self, base: &Self) -> Self {
        Self {
            enabled: self.enabled.or(base.enabled),
            min_local_results: self.min_local_results.or(base.min_local_results),
            min_local_confidence: self.min_local_confidence.or(base.min_local_confidence),
            fallback_on_empty: self.fallback_on_empty.or(base.fallback_on_empty),
            fallback_on_low_confidence: self
                .fallback_on_low_confidence
                .or(base.fallback_on_low_confidence),
            merge_mode: self.merge_mode.clone().or_else(|| base.merge_mode.clone()),
            max_merged_results: self.max_merged_results.or(base.max_merged_results),
        }
    }

    /// Check if federated search is enabled.
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(false)
    }

    /// Get the minimum local results threshold (default: 3).
    pub fn min_local_results(&self) -> usize {
        self.min_local_results.unwrap_or(3)
    }

    /// Get the minimum local confidence (default: 0.7).
    pub fn min_local_confidence(&self) -> f32 {
        self.min_local_confidence.unwrap_or(0.7)
    }

    /// Get the merge mode (default: "local_only_fallback").
    pub fn merge_mode(&self) -> &str {
        self.merge_mode.as_deref().unwrap_or("local_only_fallback")
    }

    /// Get max merged results (default: 10).
    pub fn max_merged_results(&self) -> usize {
        self.max_merged_results.unwrap_or(10)
    }

    /// Validate federated search configuration.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if let Some(ref val) = self.min_local_confidence {
            if !(0.0..=1.0).contains(val) {
                return Err("federated.min_local_confidence must be between 0.0 and 1.0".into());
            }
        }
        if let Some(ref mode) = self.merge_mode {
            let lower = mode.to_lowercase();
            let valid = [
                "local_only_fallback",
                "local_priority",
                "remote_priority",
                "interleave",
            ];
            if !valid.contains(&lower.as_str()) {
                return Err(format!(
                    "federated.merge_mode '{}' invalid, expected one of: {}",
                    mode,
                    valid.join(", ")
                ));
            }
        }
        if let Some(ref val) = self.max_merged_results {
            if *val == 0 {
                return Err("federated.max_merged_results must be > 0".into());
            }
        }
        if let Some(ref val) = self.min_local_results {
            if *val == 0 {
                return Err("federated.min_local_results must be > 0".into());
            }
        }
        Ok(())
    }
}

impl SearchConfig {
    fn merge_with(&self, base: &Self) -> Self {
        Self {
            use_hybrid: self.use_hybrid.or(base.use_hybrid),
            default_limit: self.default_limit.or(base.default_limit),
            auto_index_on_install: self.auto_index_on_install.or(base.auto_index_on_install),
            federated: self.federated.merge_with(&base.federated),
        }
    }

    pub fn use_hybrid(&self) -> bool {
        self.use_hybrid.unwrap_or(false)
    }

    pub fn default_limit(&self) -> usize {
        self.default_limit.unwrap_or(10)
    }

    pub fn auto_index_on_install(&self) -> bool {
        self.auto_index_on_install.unwrap_or(false)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokenizer_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_size: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl EmbeddingConfig {
    fn merge_with(&self, base: &Self) -> Self {
        Self {
            model_path: self.model_path.clone().or(base.model_path.clone()),
            tokenizer_path: self.tokenizer_path.clone().or(base.tokenizer_path.clone()),
            batch_size: self.batch_size.or(base.batch_size),
            provider: self.provider.clone().or(base.provider.clone()),
            api_key: self.api_key.clone().or(base.api_key.clone()),
            base_url: self.base_url.clone().or(base.base_url.clone()),
        }
    }

    pub fn model_path(&self) -> PathBuf {
        self.model_path.clone().unwrap_or_else(|| {
            Config::global_models_dir()
                .join("all-MiniLM-L6-v2")
                .join("model.onnx")
        })
    }
    pub fn tokenizer_path(&self) -> PathBuf {
        self.tokenizer_path.clone().unwrap_or_else(|| {
            Config::global_models_dir()
                .join("all-MiniLM-L6-v2")
                .join("tokenizer.json")
        })
    }
    pub fn batch_size(&self) -> usize {
        self.batch_size.unwrap_or(32)
    }
    pub fn provider(&self) -> &str {
        self.provider.as_deref().unwrap_or("onnx")
    }
    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }
    pub fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_index: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_dir: Option<String>,
}

/// Proxy server configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// Listen address for gRPC (e.g., "127.0.0.1:9999").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen: Option<String>,

    /// Listen address for HTTP health/prometheus endpoints (e.g., "127.0.0.1:9998").
    /// If not set, defaults to the gRPC listen port + 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_listen: Option<String>,

    /// Duration in seconds before a cached entry becomes stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_duration_secs: Option<u64>,

    /// Duration in seconds before a stale entry expires completely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_duration_secs: Option<u64>,

    /// Maximum number of entries in the cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_entries: Option<usize>,

    /// URL of the upstream RAG service.
    ///
    /// **Deprecated**: Use `[[proxy.upstreams]]` with `id` and `url` instead.
    /// This field is auto-converted via `normalize_upstreams()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_url: Option<String>,

    /// Timeout for upstream requests in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_timeout_secs: Option<u64>,

    /// TTL jitter percentage to prevent thundering herd (0.0 to 1.0, default: 0.1 = 10%).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_jitter_percent: Option<f32>,

    /// Interval for background refresh worker in seconds (default: 60).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_interval_secs: Option<u64>,

    /// Scope configuration for seed phrase filtering.
    #[serde(default)]
    pub scope: ProxyScopeConfig,

    /// Cache configuration with limits and eviction.
    #[serde(default)]
    pub cache: ProxyCacheConfig,

    /// Distill feature configuration (output dir, post-process, format).
    #[serde(default)]
    pub distill: DistillConfig,

    /// API key for authenticating clients (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,

    /// Rate limiting configuration.
    #[serde(default)]
    pub rate_limit: ProxyRateLimitConfig,

    /// Retry policy configuration for upstream requests.
    #[serde(default)]
    pub retry: ProxyRetryConfig,

    /// Multiple upstream endpoints (alternative to single upstream_url).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub upstreams: Vec<UpstreamEndpointConfig>,

    /// Federated search configuration for local+remote merging.
    #[serde(default)]
    pub federated: FederatedConfig,

    /// Unified security configuration (API key, rate limiting, advanced).
    #[serde(default)]
    pub security: SecurityConfig,

    /// Cascade query configuration (priority-based upstream fallback).

    #[serde(default)]
    pub cascade: CascadeConfig,

    /// Connection pool configuration (pgbouncer-style concurrency control).

    #[serde(default)]
    pub pool: ConnectionPoolConfig,

    /// Circuit breaker configuration (upstream failure protection).

    #[serde(default)]
    pub circuit_breaker: ProxyCircuitBreakerConfig,

    /// Per-agent multi-tenancy configuration.
    ///
    /// When non-empty, each agent gets its own API key, rate limit,
    /// and context restrictions. When empty, behavior is identical
    /// to the single-key `api_key` field (backward-compatible).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<AgentConfig>,

    /// CDC (Change Data Capture) event stream configuration.
    #[serde(default)]
    pub cdc: CdcConfig,

    /// Peer-to-peer cache replication configuration.
    #[serde(default)]
    pub peer: PeerConfig,

    /// Socket tuning configuration for listeners and upstream clients.
    #[serde(default)]
    pub socket_tuning: SocketTuningConfig,

    /// Graceful shutdown timeout in seconds (default: 30).
    /// After receiving a shutdown signal, the proxy will wait this long
    /// for in-flight requests to complete before forcing exit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shutdown_timeout_secs: Option<u64>,

    /// Maximum global concurrent upstream connections across all upstreams (default: 1000).
    /// Uses try_acquire for fail-fast 503 instead of queueing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_global_connections: Option<usize>,

    /// Web UI configuration (read-only status dashboard).
    #[serde(default)]
    pub web_ui: WebUiConfig,
}

/// Web UI configuration.
///
/// When enabled, serves a read-only status dashboard at `/ui`.
/// GET status endpoints are accessible without authentication.
/// No admin/mutation endpoints are exposed through the UI.
///
/// # TOML example
///
/// ```toml
/// [proxy.web_ui]
/// enabled = true
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct WebUiConfig {
    /// Enable the read-only web UI (default: false).
    #[serde(default)]
    pub enabled: bool,
}

impl WebUiConfig {
    fn merge_with(&self, base: &Self) -> Self {
        Self {
            enabled: self.enabled || base.enabled,
        }
    }
}

/// Configuration for a single agent (multi-tenancy).
///
/// Each agent gets its own API key, optional context restrictions,
/// priority class, and rate limit. When no agents are configured,
/// behavior is identical to the global `api_key` field.
///
/// # TOML example
///
/// ```toml
/// [[proxy.agents]]
/// id = "code-review-agent"
/// api_key = "crv-xxxxxxxx"
/// default_context = "codebase-rust"
/// allowed_contexts = ["codebase-rust", "codebase-python"]
/// priority_class = 2
/// rate_limit_rps = 50
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Unique identifier for this agent.
    pub id: String,

    /// API key for authenticating this agent.
    pub api_key: String,

    /// Default context for this agent (used when no X-Context header is sent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_context: Option<String>,

    /// Contexts this agent is allowed to query.
    /// Empty list means all contexts are allowed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_contexts: Vec<String>,

    /// Priority class for request ordering (lower = higher priority, default: 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority_class: Option<u32>,

    /// Per-agent rate limit in requests per second.
    /// If not set, the global rate limit applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_rps: Option<u32>,

    /// Whether this agent is enabled (default: true).
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl AgentConfig {
    /// Check if this agent is allowed to access the given context.
    ///
    /// Returns `true` if `allowed_contexts` is empty (unrestricted)
    /// or if the context is in the allowed list.
    pub fn can_access_context(&self, context: &str) -> bool {
        self.allowed_contexts.is_empty() || self.allowed_contexts.iter().any(|c| c == context)
    }

    /// Get the priority class (default: 0).
    pub fn priority_class(&self) -> u32 {
        self.priority_class.unwrap_or(0)
    }
}

/// Configuration for a single upstream endpoint.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpstreamEndpointConfig {
    /// Unique identifier for this upstream.
    pub id: String,

    /// URL of the upstream RAG service.
    pub url: String,

    /// Timeout for requests in seconds (default: 30).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,

    /// Weight for load balancing (default: 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<u32>,

    /// Priority for failover (lower = higher priority, default: 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u32>,

    /// Maximum concurrent requests to this upstream (default: unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<usize>,

    /// Whether this upstream is enabled (default: true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    /// Optional endpoint for version/metadata polling (e.g., "/v1/version").
    /// If set, the proxy will periodically poll this endpoint to detect upstream changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_endpoint: Option<String>,

    /// Poll interval for version endpoint in seconds (default: 60).
    /// Only used if version_endpoint is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_poll_interval_secs: Option<u64>,

    /// Upstream type: "elasticsearch", "opensearch", "qdrant", "pinecone", "milvus", "pgvector", "meilisearch".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_type: Option<String>,

    /// Query mode: "text_native", "vector_only", "unknown".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_mode: Option<String>,

    // --- pgvector-specific fields ---
    /// Table name for pgvector upstream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,

    /// Embedding column name for pgvector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_column: Option<String>,

    /// Content column name for pgvector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_column: Option<String>,

    /// Metadata columns to return for pgvector.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metadata_columns: Vec<String>,

    /// Distance metric for pgvector: "cosine", "l2", "inner_product".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance_metric: Option<String>,

    /// Vector dimensions for pgvector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<usize>,

    // --- Elasticsearch/OpenSearch-specific fields ---
    /// Index name for ES/OpenSearch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,

    /// Fields to search in ES/OpenSearch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub search_fields: Vec<String>,

    /// Fields to return in ES/OpenSearch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub return_fields: Vec<String>,

    /// Optional API key for the upstream (supports `${ENV_VAR}` interpolation).
    /// Meilisearch: Bearer master key. Elasticsearch: ApiKey value. Qdrant: raw `api-key` header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

impl UpstreamEndpointConfig {
    /// Get the timeout in seconds (default: 30).
    pub fn timeout_secs(&self) -> u64 {
        self.timeout_secs.unwrap_or(30)
    }

    /// Get the weight for load balancing (default: 1).
    pub fn weight(&self) -> u32 {
        self.weight.unwrap_or(1)
    }

    /// Get the priority for failover (default: 0).
    pub fn priority(&self) -> u32 {
        self.priority.unwrap_or(0)
    }

    /// Check if this upstream is enabled (default: true).
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    /// Get the version endpoint if configured.
    pub fn version_endpoint(&self) -> Option<&str> {
        self.version_endpoint.as_deref()
    }

    /// Get the version poll interval in seconds (default: 60).
    pub fn version_poll_interval_secs(&self) -> u64 {
        self.version_poll_interval_secs.unwrap_or(60)
    }

    /// Check if version polling is enabled for this upstream.
    pub fn has_version_polling(&self) -> bool {
        self.version_endpoint.is_some()
    }

    /// Get the upstream type (e.g., "elasticsearch", "qdrant", "pgvector").
    pub fn upstream_type(&self) -> Option<&str> {
        self.upstream_type.as_deref()
    }

    /// Get the query mode (e.g., "text_native", "vector_only").
    pub fn query_mode(&self) -> Option<&str> {
        self.query_mode.as_deref()
    }

    /// Check if this is a pgvector upstream.
    pub fn is_pgvector(&self) -> bool {
        self.upstream_type.as_deref() == Some("pgvector")
    }

    /// Get the distance metric (default: "cosine").
    pub fn distance_metric(&self) -> &str {
        self.distance_metric.as_deref().unwrap_or("cosine")
    }

    /// Validate upstream-specific configuration.
    pub fn validate(&self) -> std::result::Result<(), String> {
        // Validate upstream_type values
        if let Some(ref ut) = self.upstream_type {
            let valid = [
                "elasticsearch",
                "opensearch",
                "qdrant",
                "pinecone",
                "milvus",
                "pgvector",
                "meilisearch",
            ];
            if !valid.contains(&ut.as_str()) {
                return Err(format!(
                    "upstream '{}': invalid upstream_type '{}', expected one of: {}",
                    self.id,
                    ut,
                    valid.join(", ")
                ));
            }
        }

        // Validate query_mode values
        if let Some(ref qm) = self.query_mode {
            let valid = ["text_native", "vector_only", "unknown"];
            if !valid.contains(&qm.as_str()) {
                return Err(format!(
                    "upstream '{}': invalid query_mode '{}', expected one of: {}",
                    self.id,
                    qm,
                    valid.join(", ")
                ));
            }
        }

        // pgvector requires table
        if self.is_pgvector() && self.table.is_none() {
            return Err(format!(
                "upstream '{}': pgvector upstream requires 'table' field",
                self.id
            ));
        }

        // Validate distance_metric
        if let Some(ref dm) = self.distance_metric {
            let valid = ["cosine", "l2", "inner_product"];
            if !valid.contains(&dm.as_str()) {
                return Err(format!(
                    "upstream '{}': invalid distance_metric '{}', expected one of: {}",
                    self.id,
                    dm,
                    valid.join(", ")
                ));
            }
        }

        Ok(())
    }

    /// Resolve optional upstream API key (`${ENV_VAR}` expanded).
    ///
    /// # Errors
    /// Returns error if value is `${NAME}` and `NAME` is not set in the environment.
    pub fn resolve_api_key(&self) -> Result<Option<String>> {
        match &self.api_key {
            None => Ok(None),
            Some(raw) => {
                let resolved = resolve_env_ref(raw).ok_or_else(|| {
                    ConproxyError::ConfigValidation(format!(
                        "upstream '{}': api_key references undefined env var: {raw}",
                        self.id
                    ))
                })?;
                if resolved.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(resolved))
                }
            }
        }
    }
}

/// Advanced security configuration for non-localhost deployments.
///
/// These features (signature verification, TLS pinning, replay detection) are
/// disabled by default for localhost use. Enable them when exposing the proxy
/// to untrusted networks.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AdvancedSecurityConfig {
    /// Enable advanced security features (default: false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    /// Signature algorithm: "hmac-sha256" or "blake3" (default: none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_algorithm: Option<String>,

    /// Enable TLS certificate pinning for upstream connections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_pinning: Option<bool>,

    /// Enable replay attack detection (timestamp validation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_detection: Option<bool>,

    /// Replay detection window in seconds (default: 300).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_window_seconds: Option<u64>,
}

impl AdvancedSecurityConfig {
    fn merge_with(&self, base: &Self) -> Self {
        Self {
            enabled: self.enabled.or(base.enabled),
            signature_algorithm: self
                .signature_algorithm
                .clone()
                .or_else(|| base.signature_algorithm.clone()),
            tls_pinning: self.tls_pinning.or(base.tls_pinning),
            replay_detection: self.replay_detection.or(base.replay_detection),
            replay_window_seconds: self.replay_window_seconds.or(base.replay_window_seconds),
        }
    }

    /// Check if advanced security is enabled (default: false).
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(false)
    }

    /// Check if TLS pinning is enabled (default: false).
    pub fn tls_pinning(&self) -> bool {
        self.tls_pinning.unwrap_or(false)
    }

    /// Check if replay detection is enabled (default: false).
    pub fn replay_detection(&self) -> bool {
        self.replay_detection.unwrap_or(false)
    }

    /// Get replay window in seconds (default: 300).
    pub fn replay_window_seconds(&self) -> u64 {
        self.replay_window_seconds.unwrap_or(300)
    }

    /// Validate advanced security configuration.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if let Some(ref sig) = self.signature_algorithm {
            let valid = ["hmac-sha256", "blake3"];
            if !valid.contains(&sig.as_str()) {
                return Err(format!(
                    "advanced.signature_algorithm '{}' invalid, expected one of: {}",
                    sig,
                    valid.join(", ")
                ));
            }
        }
        if let Some(ref val) = self.replay_window_seconds {
            if *val == 0 {
                return Err("advanced.replay_window_seconds must be > 0".into());
            }
        }
        Ok(())
    }
}

/// Security configuration for the proxy.
///
/// Groups API key auth, rate limiting, and advanced security features.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// API key for authenticating clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,

    /// Rate limiting configuration.
    #[serde(default)]
    pub rate_limit: ProxyRateLimitConfig,

    /// Advanced security features (disabled by default for localhost).
    #[serde(default)]
    pub advanced: AdvancedSecurityConfig,
}

impl SecurityConfig {
    fn merge_with(&self, base: &Self) -> Self {
        Self {
            api_key: self.api_key.clone().or_else(|| base.api_key.clone()),
            rate_limit: self.rate_limit.merge_with(&base.rate_limit),
            advanced: self.advanced.merge_with(&base.advanced),
        }
    }

    /// Validate security configuration.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if let Some(ref key) = self.api_key {
            if key.is_empty() {
                return Err("security.api_key must not be empty".into());
            }
        }
        self.advanced.validate()?;
        Ok(())
    }
}

/// CDC (Change Data Capture) configuration for the cache proxy.
///
/// When enabled, cache mutations are published to a broadcast channel.
/// External consumers can subscribe via gRPC `CdcService.Subscribe`.
/// Automatically enabled when `peer.enabled = true`.
///
/// # TOML example
///
/// ```toml
/// [proxy.cdc]
/// enabled = true
/// buffer_size = 10000
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CdcConfig {
    /// Enable CDC event stream (default: false).
    /// Automatically enabled when peer replication is enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    /// Broadcast channel capacity (default: 10000).
    /// When the buffer is full, slow receivers get `Lagged` errors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buffer_size: Option<usize>,
}

impl CdcConfig {
    fn merge_with(&self, base: &Self) -> Self {
        Self {
            enabled: self.enabled.or(base.enabled),
            buffer_size: self.buffer_size.or(base.buffer_size),
        }
    }

    /// Check if CDC is enabled (default: false).
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(false)
    }

    /// Get the broadcast buffer size (default: 10000).
    pub fn buffer_size(&self) -> usize {
        self.buffer_size.unwrap_or(10_000)
    }
}

/// Peer-to-peer cache replication configuration.
///
/// When enabled, cache mutations are replicated between peers via CDC events
/// over gRPC. Each peer subscribes to the others' CDC streams and applies
/// received events with dedup (origin_node_id echo prevention, timestamp-based
/// last-write-wins).
///
/// # TOML example
///
/// ```toml
/// [proxy.peer]
/// enabled = true
/// node_id = "pod-a"
/// peers = ["pod-b.conproxy-svc:9090"]
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PeerConfig {
    /// Enable peer-to-peer cache replication (default: false).
    /// When enabled, CDC is automatically enabled regardless of `cdc.enabled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    /// Unique node identifier for this peer.
    /// If not set, auto-detected from hostname.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,

    /// Addresses of other peers (e.g., ["pod-b.conproxy-svc:9090"]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub peers: Vec<String>,

    /// Reconnect interval when a peer connection drops (ms, default: 5000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconnect_interval_ms: Option<u64>,

    /// Timeout for distributed singleflight wait (ms, default: 5000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_wait_timeout_ms: Option<u64>,

    /// Request a cache snapshot from a live peer on startup (default: true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_on_join: Option<bool>,

    /// Fraction of peer cache size to reach before marking ready (default: 0.8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_threshold: Option<f64>,

    /// Number of cache entries per gRPC snapshot message (default: 100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_batch_size: Option<usize>,

    /// Optional shared secret for peer gRPC auth (plan 07).
    /// When set, peer CDC subscribe + snapshot/status require `x-peer-secret`
    /// (or `x-api-key`) matching this value. Default off (trusted-network v1).
    /// Supports `${ENV_VAR}` expansion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_secret: Option<String>,
}

impl PeerConfig {
    fn merge_with(&self, base: &Self) -> Self {
        Self {
            enabled: self.enabled.or(base.enabled),
            node_id: self.node_id.clone().or_else(|| base.node_id.clone()),
            peers: if self.peers.is_empty() {
                base.peers.clone()
            } else {
                self.peers.clone()
            },
            reconnect_interval_ms: self.reconnect_interval_ms.or(base.reconnect_interval_ms),
            fetch_wait_timeout_ms: self.fetch_wait_timeout_ms.or(base.fetch_wait_timeout_ms),
            snapshot_on_join: self.snapshot_on_join.or(base.snapshot_on_join),
            ready_threshold: self.ready_threshold.or(base.ready_threshold),
            snapshot_batch_size: self.snapshot_batch_size.or(base.snapshot_batch_size),
            shared_secret: self
                .shared_secret
                .clone()
                .or_else(|| base.shared_secret.clone()),
        }
    }

    /// Check if peer replication is enabled (default: false).
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(false)
    }

    /// Get the node ID (falls back to `HOSTNAME` env var or PID if not set).
    pub fn node_id(&self) -> String {
        self.node_id.clone().unwrap_or_else(|| {
            std::env::var("HOSTNAME").unwrap_or_else(|_| format!("node-{}", std::process::id()))
        })
    }

    /// Get the peer addresses.
    pub fn peers(&self) -> &[String] {
        &self.peers
    }

    /// Get the reconnect interval (default: 5000ms).
    pub fn reconnect_interval_ms(&self) -> u64 {
        self.reconnect_interval_ms.unwrap_or(5000)
    }

    /// Get the distributed singleflight wait timeout (default: 5000ms).
    pub fn fetch_wait_timeout_ms(&self) -> u64 {
        self.fetch_wait_timeout_ms.unwrap_or(5000)
    }

    /// Whether to request a snapshot on join (default: true).
    pub fn snapshot_on_join(&self) -> bool {
        self.snapshot_on_join.unwrap_or(true)
    }

    /// Fraction of peer cache size needed for readiness (default: 0.8).
    pub fn ready_threshold(&self) -> f64 {
        self.ready_threshold.unwrap_or(0.8)
    }

    /// Entries per gRPC snapshot message (default: 100).
    pub fn snapshot_batch_size(&self) -> usize {
        self.snapshot_batch_size.unwrap_or(100)
    }

    /// Optional peer shared secret (raw config value, may be `${ENV}`).
    pub fn shared_secret_raw(&self) -> Option<&str> {
        self.shared_secret.as_deref()
    }

    /// Resolved peer shared secret (env expanded). `None` if unset.
    ///
    /// # Errors
    ///
    /// Returns error if value uses `${ENV}` syntax but the variable is unset/empty.
    pub fn resolve_shared_secret(&self) -> std::result::Result<Option<String>, String> {
        match &self.shared_secret {
            None => Ok(None),
            Some(raw) => {
                let raw = raw.trim();
                if raw.is_empty() {
                    return Err("peer.shared_secret must not be empty when set".to_string());
                }
                match resolve_env_ref(raw) {
                    Some(v) => Ok(Some(v)),
                    None => Err(format!(
                        "peer.shared_secret references undefined env var: {raw}"
                    )),
                }
            }
        }
    }
}

/// Circuit breaker configuration for upstream failure protection.
///
/// Controls when the proxy stops sending requests to a failing upstream
/// (circuit opens) and when it resumes (circuit closes). The default
/// `failure_threshold` of 25 is tuned for production load — low enough
/// to catch genuine outages, high enough to tolerate warm-up blips.
///
/// # TOML example
///
/// ```toml
/// [proxy.circuit_breaker]
/// failure_threshold = 25
/// success_threshold = 2
/// open_duration_secs = 30
/// failure_window_secs = 60
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyCircuitBreakerConfig {
    /// Number of failures within the window before opening the circuit (default: 25).
    #[serde(default = "default_cb_failure_threshold")]
    pub failure_threshold: u32,
    /// Number of successes in half-open state to close the circuit (default: 2).
    #[serde(default = "default_cb_success_threshold")]
    pub success_threshold: u32,
    /// Seconds to wait before transitioning open → half-open (default: 30).
    #[serde(default = "default_cb_open_duration_secs")]
    pub open_duration_secs: u64,
    /// Failure window in seconds — failures outside this window don't count (default: 60).
    #[serde(default = "default_cb_failure_window_secs")]
    pub failure_window_secs: u64,
}

fn default_cb_failure_threshold() -> u32 {
    25
}
fn default_cb_success_threshold() -> u32 {
    2
}
fn default_cb_open_duration_secs() -> u64 {
    30
}
fn default_cb_failure_window_secs() -> u64 {
    60
}

impl Default for ProxyCircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: default_cb_failure_threshold(),
            success_threshold: default_cb_success_threshold(),
            open_duration_secs: default_cb_open_duration_secs(),
            failure_window_secs: default_cb_failure_window_secs(),
        }
    }
}

impl ProxyCircuitBreakerConfig {
    /// Convert to the internal `CircuitBreakerConfig` used by `CircuitBreaker`.
    pub fn to_circuit_breaker_config(&self) -> CircuitBreakerConfig {
        CircuitBreakerConfig {
            failure_threshold: self.failure_threshold,
            success_threshold: self.success_threshold,
            open_duration: Duration::from_secs(self.open_duration_secs),
            failure_window: Duration::from_secs(self.failure_window_secs),
        }
    }

    /// Validate circuit breaker configuration.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.failure_threshold == 0 {
            return Err("circuit_breaker.failure_threshold must be > 0".into());
        }
        if self.success_threshold == 0 {
            return Err("circuit_breaker.success_threshold must be > 0".into());
        }
        if self.open_duration_secs == 0 {
            return Err("circuit_breaker.open_duration_secs must be > 0".into());
        }
        if self.failure_window_secs == 0 {
            return Err("circuit_breaker.failure_window_secs must be > 0".into());
        }
        Ok(())
    }
}

/// Socket tuning configuration for proxy listeners and upstream clients.
///
/// Controls OS-level TCP options for both the server (listener) and client
/// (upstream) sides. All fields have production-ready defaults — operators
/// only need to override when their environment differs.
///
/// # TOML example
///
/// ```toml
/// [proxy.socket_tuning]
/// tcp_nodelay = true
/// reuse_port = true
/// listen_backlog = 4096
/// send_buffer_size = 262144    # 256 KB, omit for OS autotuning
/// recv_buffer_size = 262144
/// upstream_pool_max_idle = 32
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketTuningConfig {
    /// Disable Nagle's algorithm on listener and upstream sockets (default: true).
    #[serde(default = "default_true")]
    pub tcp_nodelay: bool,

    /// TCP keepalive idle time in seconds (default: 60).
    #[serde(default = "SocketTuningConfig::default_keepalive_secs")]
    pub tcp_keepalive_secs: u64,

    /// TCP keepalive probe interval in seconds (default: 15).
    #[serde(default = "SocketTuningConfig::default_keepalive_interval")]
    pub tcp_keepalive_interval: u64,

    /// TCP keepalive probe count before declaring dead (default: 5).
    #[serde(default = "SocketTuningConfig::default_keepalive_probes")]
    pub tcp_keepalive_probes: u32,

    /// Listen backlog (default: 4096).
    #[serde(default = "SocketTuningConfig::default_listen_backlog")]
    pub listen_backlog: u32,

    /// Send buffer size in bytes. None = OS autotuning (recommended).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_buffer_size: Option<usize>,

    /// Receive buffer size in bytes. None = OS autotuning (recommended).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recv_buffer_size: Option<usize>,

    /// TCP_DEFER_ACCEPT timeout in seconds (Linux only, default: 5).
    #[serde(default = "SocketTuningConfig::default_defer_accept_secs")]
    pub defer_accept_secs: i32,

    /// TCP_USER_TIMEOUT in milliseconds (Linux only, default: 30000).
    #[serde(default = "SocketTuningConfig::default_user_timeout_ms")]
    pub user_timeout_ms: u32,

    /// Enable SO_REUSEPORT for kernel-level load balancing (default: true).
    #[serde(default = "default_true")]
    pub reuse_port: bool,

    /// Upstream connection pool idle timeout in seconds (default: 90).
    #[serde(default = "SocketTuningConfig::default_upstream_pool_idle_timeout")]
    pub upstream_pool_idle_timeout_secs: u64,

    /// Maximum idle connections per upstream host (default: 32).
    #[serde(default = "SocketTuningConfig::default_upstream_pool_max_idle")]
    pub upstream_pool_max_idle: usize,
}

impl Default for SocketTuningConfig {
    fn default() -> Self {
        Self {
            tcp_nodelay: true,
            tcp_keepalive_secs: 60,
            tcp_keepalive_interval: 15,
            tcp_keepalive_probes: 5,
            listen_backlog: 4096,
            send_buffer_size: None,
            recv_buffer_size: None,
            defer_accept_secs: 5,
            user_timeout_ms: 30_000,
            reuse_port: true,
            upstream_pool_idle_timeout_secs: 90,
            upstream_pool_max_idle: 32,
        }
    }
}

impl SocketTuningConfig {
    fn default_keepalive_secs() -> u64 {
        60
    }
    fn default_keepalive_interval() -> u64 {
        15
    }
    fn default_keepalive_probes() -> u32 {
        5
    }
    fn default_listen_backlog() -> u32 {
        4096
    }
    fn default_defer_accept_secs() -> i32 {
        5
    }
    fn default_user_timeout_ms() -> u32 {
        30_000
    }
    fn default_upstream_pool_idle_timeout() -> u64 {
        90
    }
    fn default_upstream_pool_max_idle() -> usize {
        32
    }

    fn merge_with(&self, _base: &Self) -> Self {
        // Socket tuning: local config fully overrides base (no partial merge)
        self.clone()
    }
}

/// Rate limiting configuration for the proxy.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProxyRateLimitConfig {
    /// Enable rate limiting (default: false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    /// Maximum requests per second (default: 100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_per_second: Option<u32>,

    /// Burst capacity for token bucket (default: 50).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burst_size: Option<u32>,
}

/// Retry policy configuration for upstream requests.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProxyRetryConfig {
    /// Enable retry policy (default: true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    /// Maximum number of retry attempts (default: 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,

    /// Initial delay before first retry in milliseconds (default: 100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_delay_ms: Option<u64>,

    /// Maximum delay between retries in milliseconds (default: 10000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_delay_ms: Option<u64>,

    /// Backoff multiplier for exponential backoff (default: 2.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff_multiplier: Option<f64>,

    /// Retry on network errors (default: true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_network_error: Option<bool>,

    /// Retry on timeout (default: true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_timeout: Option<bool>,

    /// Retry on 5xx status codes (default: true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_server_error: Option<bool>,

    /// Retry on 429 rate limited (default: true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_rate_limited: Option<bool>,
}

impl ProxyRateLimitConfig {
    fn merge_with(&self, base: &Self) -> Self {
        Self {
            enabled: self.enabled.or(base.enabled),
            requests_per_second: self.requests_per_second.or(base.requests_per_second),
            burst_size: self.burst_size.or(base.burst_size),
        }
    }

    /// Check if rate limiting is enabled (default: false).
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(false)
    }

    /// Get requests per second (default: 100).
    pub fn requests_per_second(&self) -> u32 {
        self.requests_per_second.unwrap_or(100)
    }

    /// Get burst size (default: 50).
    pub fn burst_size(&self) -> u32 {
        self.burst_size.unwrap_or(50)
    }

    /// Validate rate limit configuration.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if let Some(ref val) = self.requests_per_second {
            if *val == 0 {
                return Err("rate_limit.requests_per_second must be > 0".into());
            }
        }
        if let Some(ref val) = self.burst_size {
            if *val == 0 {
                return Err("rate_limit.burst_size must be > 0".into());
            }
        }
        Ok(())
    }
}

impl ProxyRetryConfig {
    fn merge_with(&self, base: &Self) -> Self {
        Self {
            enabled: self.enabled.or(base.enabled),
            max_retries: self.max_retries.or(base.max_retries),
            initial_delay_ms: self.initial_delay_ms.or(base.initial_delay_ms),
            max_delay_ms: self.max_delay_ms.or(base.max_delay_ms),
            backoff_multiplier: self.backoff_multiplier.or(base.backoff_multiplier),
            on_network_error: self.on_network_error.or(base.on_network_error),
            on_timeout: self.on_timeout.or(base.on_timeout),
            on_server_error: self.on_server_error.or(base.on_server_error),
            on_rate_limited: self.on_rate_limited.or(base.on_rate_limited),
        }
    }

    /// Check if retry is enabled (default: true).
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    /// Get max retries (default: 3).
    pub fn max_retries(&self) -> u32 {
        self.max_retries.unwrap_or(3)
    }

    /// Get initial delay in milliseconds (default: 100).
    pub fn initial_delay_ms(&self) -> u64 {
        self.initial_delay_ms.unwrap_or(100)
    }

    /// Get max delay in milliseconds (default: 10000).
    pub fn max_delay_ms(&self) -> u64 {
        self.max_delay_ms.unwrap_or(10000)
    }

    /// Get backoff multiplier (default: 2.0).
    pub fn backoff_multiplier(&self) -> f64 {
        self.backoff_multiplier.unwrap_or(2.0)
    }

    /// Check if network errors should be retried (default: true).
    pub fn on_network_error(&self) -> bool {
        self.on_network_error.unwrap_or(true)
    }

    /// Check if timeouts should be retried (default: true).
    pub fn on_timeout(&self) -> bool {
        self.on_timeout.unwrap_or(true)
    }

    /// Check if server errors should be retried (default: true).
    pub fn on_server_error(&self) -> bool {
        self.on_server_error.unwrap_or(true)
    }

    /// Check if rate limited errors should be retried (default: true).
    pub fn on_rate_limited(&self) -> bool {
        self.on_rate_limited.unwrap_or(true)
    }

    /// Validate retry configuration.
    pub fn validate(&self) -> std::result::Result<(), String> {
        // 0 = no retries (valid), 0 = immediate retry (valid)
        if let Some(ref max) = self.max_delay_ms {
            if *max == 0 {
                return Err("retry.max_delay_ms must be > 0".into());
            }
            if let Some(ref initial) = self.initial_delay_ms {
                if *max < *initial {
                    return Err("retry.max_delay_ms must be >= retry.initial_delay_ms".into());
                }
            }
        }
        if let Some(ref val) = self.backoff_multiplier {
            if *val < 1.0 {
                return Err("retry.backoff_multiplier must be >= 1.0".into());
            }
        }
        Ok(())
    }
}

/// One weighted scope phrase (preferred config form: array-of-tables).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WeightedPhrase {
    /// Phrase text matched against result content.
    pub text: String,
    /// Boost/rerank weight only (ignored by filter). Default 1.0.
    #[serde(default = "default_phrase_weight")]
    pub weight: f32,
    /// Optional per-entry similarity floor; inherits context `min_similarity` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_similarity: Option<f32>,
}

fn default_phrase_weight() -> f32 {
    1.0
}

impl WeightedPhrase {
    /// Build phrase with default weight 1.0 and no entry min.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            weight: 1.0,
            min_similarity: None,
        }
    }
}

/// Scope configuration for filtering upstream results by relevance.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProxyScopeConfig {
    /// Preferred: weighted phrases (array-of-tables in TOML).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub weighted_phrases: Vec<WeightedPhrase>,

    /// Deprecated bare string list (`seeds` / `phrases`). Expanded when `weighted_phrases` empty.
    #[serde(default, alias = "phrases", skip_serializing_if = "Vec::is_empty")]
    pub seeds: Vec<String>,

    /// How phrases are used: "filter", "rerank", or "boost".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,

    /// Minimum similarity for filter mode / default entry floor (default: 0.25).
    #[serde(
        default,
        alias = "min_similarity",
        skip_serializing_if = "Option::is_none"
    )]
    pub min_seed_similarity: Option<f32>,

    /// Weight for rerank/boost mode blend (default: 0.3). Alias: `scope_weight`.
    #[serde(
        default,
        alias = "scope_weight",
        skip_serializing_if = "Option::is_none"
    )]
    pub seed_weight: Option<f32>,

    /// Optional query prefix (alternative to embedding-based filtering).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_prefix: Option<String>,

    /// Blend weight for lexical vs semantic when hybrid embed is active (default: 0.5).
    /// `sim = lexical_weight * lexical + (1 - lexical_weight) * semantic` inside embed_band.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lexical_weight: Option<f32>,

    /// Lexical similarity band `[lo, hi]` where embed hybrid applies (default: `[0.1, 0.55]`).
    /// Below `lo` or at/above `hi`: lexical-only (v1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_band: Option<[f32; 2]>,
}

impl ProxyScopeConfig {
    fn merge_with(&self, base: &Self) -> Self {
        let weighted_phrases = if self.weighted_phrases.is_empty() {
            base.weighted_phrases.clone()
        } else {
            self.weighted_phrases.clone()
        };
        let seeds = if self.seeds.is_empty() {
            base.seeds.clone()
        } else {
            self.seeds.clone()
        };
        Self {
            weighted_phrases,
            seeds,
            mode: self.mode.clone().or_else(|| base.mode.clone()),
            min_seed_similarity: self.min_seed_similarity.or(base.min_seed_similarity),
            seed_weight: self.seed_weight.or(base.seed_weight),
            query_prefix: self
                .query_prefix
                .clone()
                .or_else(|| base.query_prefix.clone()),
            lexical_weight: self.lexical_weight.or(base.lexical_weight),
            embed_band: self.embed_band.or(base.embed_band),
        }
    }

    /// Effective weighted phrases: prefer `weighted_phrases`, else expand `seeds`/`phrases`.
    #[must_use]
    pub fn effective_phrases(&self) -> Vec<WeightedPhrase> {
        if !self.weighted_phrases.is_empty() {
            return self.weighted_phrases.clone();
        }
        self.seeds
            .iter()
            .filter(|s| !s.is_empty())
            .map(WeightedPhrase::new)
            .collect()
    }

    /// Phrase texts for fingerprint / list display.
    #[must_use]
    pub fn phrase_texts(&self) -> Vec<String> {
        self.effective_phrases()
            .into_iter()
            .map(|p| p.text)
            .collect()
    }

    /// Get the scope mode (default: "filter").
    pub fn mode(&self) -> &str {
        self.mode.as_deref().unwrap_or("filter")
    }

    /// Get the minimum seed similarity (default: 0.25).
    pub fn min_seed_similarity(&self) -> f32 {
        self.min_seed_similarity.unwrap_or(0.25)
    }

    /// Alias for [`Self::min_seed_similarity`].
    #[must_use]
    pub fn min_similarity(&self) -> f32 {
        self.min_seed_similarity()
    }

    /// Get the seed weight for rerank/boost mode (default: 0.3).
    pub fn seed_weight(&self) -> f32 {
        self.seed_weight.unwrap_or(0.3)
    }

    /// Alias for [`Self::seed_weight`].
    #[must_use]
    pub fn scope_weight(&self) -> f32 {
        self.seed_weight()
    }

    /// Lexical blend weight for hybrid embed (default: 0.5).
    #[must_use]
    pub fn lexical_weight(&self) -> f32 {
        self.lexical_weight.unwrap_or(0.5)
    }

    /// Embed hybrid band `[lo, hi]` (default: `[0.1, 0.55]`).
    #[must_use]
    pub fn embed_band(&self) -> [f32; 2] {
        self.embed_band.unwrap_or([0.1, 0.55])
    }

    /// Validate scope configuration.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if let Some(ref mode) = self.mode {
            let lower = mode.to_lowercase();
            let valid = ["filter", "rerank", "boost"];
            if !valid.contains(&lower.as_str()) {
                return Err(format!(
                    "scope.mode '{}' invalid, expected one of: {}",
                    mode,
                    valid.join(", ")
                ));
            }
        }
        if let Some(ref val) = self.min_seed_similarity {
            if !(0.0..=1.0).contains(val) {
                return Err(
                    "scope.min_similarity / min_seed_similarity must be between 0.0 and 1.0".into(),
                );
            }
        }
        if let Some(ref val) = self.seed_weight {
            if !(0.0..=1.0).contains(val) {
                return Err("scope.scope_weight / seed_weight must be between 0.0 and 1.0".into());
            }
        }
        if let Some(ref val) = self.lexical_weight {
            if !(0.0..=1.0).contains(val) {
                return Err("scope.lexical_weight must be between 0.0 and 1.0".into());
            }
        }
        if let Some([lo, hi]) = self.embed_band {
            if !(0.0..=1.0).contains(&lo) || !(0.0..=1.0).contains(&hi) {
                return Err("scope.embed_band values must be between 0.0 and 1.0".into());
            }
            if lo > hi {
                return Err("scope.embed_band lo must be <= hi".into());
            }
        }
        for (i, p) in self.weighted_phrases.iter().enumerate() {
            if p.text.trim().is_empty() {
                return Err(format!(
                    "scope.weighted_phrases[{i}].text must be non-empty"
                ));
            }
            if !p.weight.is_finite() || p.weight < 0.0 {
                return Err(format!(
                    "scope.weighted_phrases[{i}].weight must be finite and >= 0"
                ));
            }
            if let Some(ms) = p.min_similarity {
                if !(0.0..=1.0).contains(&ms) {
                    return Err(format!(
                        "scope.weighted_phrases[{i}].min_similarity must be between 0.0 and 1.0"
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Cache configuration with limits and eviction policies.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProxyCacheConfig {
    /// Maximum memory usage in MB (default: 256).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_memory_mb: Option<usize>,

    /// Maximum size per cached entry in KB (default: 512).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_entry_size_kb: Option<usize>,

    /// Eviction policy: "lru", "lfu", or "ttl_first" (default: "lru").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eviction_policy: Option<String>,

    /// Error TTL for 5xx errors in seconds (default: 30).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_ttl_5xx_secs: Option<u64>,

    /// Error TTL for timeouts in seconds (default: 10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_ttl_timeout_secs: Option<u64>,

    /// Error TTL for connection errors in seconds (default: 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_ttl_connection_secs: Option<u64>,

    /// Enable normalized matching (two-tier cache with exact→normalized mapping).
    /// Default: false (exact matching only, saves memory).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalized_matching: Option<bool>,

    /// Per-upstream cache limits configuration.
    #[serde(default)]
    pub per_upstream: PerUpstreamCacheConfig,

    /// Semantic cache tier configuration (embedding-similarity matching).
    #[serde(default)]
    pub semantic: SemanticCacheSettingsConfig,
}

/// Semantic cache tier configuration. Deserialized from
/// `[proxy.cache.semantic]` in `conproxy.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SemanticCacheSettingsConfig {
    /// Enable semantic matching (default: false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Cosine similarity threshold for a match (default: 0.92).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub similarity_threshold: Option<f32>,
    /// Maximum stored embeddings before LRU eviction (default: 10000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_entries: Option<usize>,
}

impl SemanticCacheSettingsConfig {
    /// `true` when semantic matching is enabled (default: false).
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(false)
    }

    /// Cosine similarity threshold (default: 0.92).
    pub fn similarity_threshold(&self) -> f32 {
        self.similarity_threshold.unwrap_or(0.92)
    }

    /// Maximum stored embeddings (default: 10000).
    pub fn max_entries(&self) -> usize {
        self.max_entries.unwrap_or(10_000)
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

/// Distill feature configuration. Controls the dump-to-disk behavior of the
/// `conproxy distill` CLI command. Deserialized from `[proxy.distill]` in
/// `conproxy.toml`.
///
/// The values are only consulted at CLI invocation time; the proxy itself
/// does not auto-distill. Setting `output_dir` is the only way to enable
/// post-process invocation — if it is `None`, the CLI still writes files but
/// does not run the post-process command.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DistillConfig {
    /// Default output directory written by `conproxy distill` (no auto-trigger).
    /// When `None`, the CLI requires `--output-dir` to be passed explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,

    /// Default post-process command (cross-platform). Split on whitespace;
    /// `parts[0]` is the executable, `parts[1..]` are arguments.
    /// When `Some`, run after files are written. When `None`, no post-process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_process_cmd: Option<String>,

    /// Default output format: `"md"`, `"json"`, or `"both"`. Default: `"md"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,

    /// Default behavior for stale TTL entries: `true` to include, `false` to skip.
    /// Default: `false` (only fresh + within-stale-window entries).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_stale: Option<bool>,
}

impl DistillConfig {
    /// Output format with default fallback.
    pub fn format(&self) -> &str {
        self.format.as_deref().unwrap_or("md")
    }

    /// Include-stale flag with default fallback.
    pub fn include_stale(&self) -> bool {
        self.include_stale.unwrap_or(false)
    }

    /// Validate that `format` is one of the allowed values.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if let Some(ref fmt) = self.format {
            if !matches!(fmt.as_str(), "md" | "json" | "both") {
                return Err(format!(
                    "proxy.distill.format must be 'md', 'json', or 'both', got '{}'",
                    fmt
                ));
            }
        }
        Ok(())
    }

    /// Merge with a base (global) config, local fields winning when set.
    pub fn merge_with(&self, base: &Self) -> Self {
        Self {
            output_dir: self.output_dir.clone().or_else(|| base.output_dir.clone()),
            post_process_cmd: self
                .post_process_cmd
                .clone()
                .or_else(|| base.post_process_cmd.clone()),
            format: self.format.clone().or_else(|| base.format.clone()),
            include_stale: self.include_stale.or(base.include_stale),
        }
    }
}

/// Per-upstream cache limits configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PerUpstreamCacheConfig {
    /// Enable per-upstream limits (default: false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    /// Maximum entries per upstream to prevent one upstream from dominating (default: 500).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_entries_per_upstream: Option<usize>,
}

impl PerUpstreamCacheConfig {
    /// Check if per-upstream limits are enabled (default: false).
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(false)
    }

    /// Get the maximum entries per upstream (default: 500).
    pub fn max_entries_per_upstream(&self) -> usize {
        self.max_entries_per_upstream.unwrap_or(500)
    }

    /// Validate per-upstream cache configuration.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if let Some(ref val) = self.max_entries_per_upstream {
            if *val == 0 {
                return Err("cache.per_upstream.max_entries_per_upstream must be > 0".into());
            }
        }
        Ok(())
    }
}

impl ProxyCacheConfig {
    fn merge_with(&self, base: &Self) -> Self {
        Self {
            max_memory_mb: self.max_memory_mb.or(base.max_memory_mb),
            max_entry_size_kb: self.max_entry_size_kb.or(base.max_entry_size_kb),
            eviction_policy: self
                .eviction_policy
                .clone()
                .or_else(|| base.eviction_policy.clone()),
            error_ttl_5xx_secs: self.error_ttl_5xx_secs.or(base.error_ttl_5xx_secs),
            error_ttl_timeout_secs: self.error_ttl_timeout_secs.or(base.error_ttl_timeout_secs),
            error_ttl_connection_secs: self
                .error_ttl_connection_secs
                .or(base.error_ttl_connection_secs),
            normalized_matching: self.normalized_matching.or(base.normalized_matching),
            per_upstream: PerUpstreamCacheConfig {
                enabled: self.per_upstream.enabled.or(base.per_upstream.enabled),
                max_entries_per_upstream: self
                    .per_upstream
                    .max_entries_per_upstream
                    .or(base.per_upstream.max_entries_per_upstream),
            },
            semantic: self.semantic.merge_with(&base.semantic),
        }
    }

    /// Get the maximum memory in MB (default: 256).
    pub fn max_memory_mb(&self) -> usize {
        self.max_memory_mb.unwrap_or(256)
    }

    /// Get the maximum entry size in KB (default: 512).
    pub fn max_entry_size_kb(&self) -> usize {
        self.max_entry_size_kb.unwrap_or(512)
    }

    /// Get the eviction policy (default: "lru").
    pub fn eviction_policy(&self) -> &str {
        self.eviction_policy.as_deref().unwrap_or("lru")
    }

    /// Check if normalized matching is enabled (default: false).
    pub fn normalized_matching(&self) -> bool {
        self.normalized_matching.unwrap_or(false)
    }

    /// Get the per-upstream cache configuration.
    pub fn per_upstream(&self) -> &PerUpstreamCacheConfig {
        &self.per_upstream
    }

    /// Get the semantic cache tier configuration.
    pub fn semantic(&self) -> &SemanticCacheSettingsConfig {
        &self.semantic
    }

    /// Validate cache configuration.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if let Some(ref policy) = self.eviction_policy {
            let valid = ["lru", "lfu", "ttl_first"];
            if !valid.contains(&policy.as_str()) {
                return Err(format!(
                    "cache.eviction_policy '{}' invalid, expected one of: {}",
                    policy,
                    valid.join(", ")
                ));
            }
        }
        if let Some(ref val) = self.max_entry_size_kb {
            if *val == 0 {
                return Err("cache.max_entry_size_kb must be > 0".into());
            }
        }
        self.per_upstream.validate()?;
        Ok(())
    }
}

impl WebConfig {
    fn merge_with(&self, base: &Self) -> Self {
        Self {
            auto_index: self.auto_index.or(base.auto_index),
            content_dir: self.content_dir.clone().or(base.content_dir.clone()),
        }
    }

    pub fn auto_index(&self) -> bool {
        self.auto_index.unwrap_or(false)
    }
    pub fn content_dir(&self) -> &str {
        self.content_dir.as_deref().unwrap_or("web")
    }
}

/// Configuration for OS page cache warming (Phase 18).
///
/// Controls which files the daemon should prefetch into the OS page cache.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextConfig {
    /// Glob patterns for files to keep warm in cache.
    /// Default: ["packages/**/*.md"]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<String>>,

    /// Interval between cache warming cycles (seconds).
    /// Default: 300 (5 minutes)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warm_interval: Option<u64>,

    /// Maximum files to warm.
    /// Default: 1000
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warm_limit: Option<usize>,
}

impl ContextConfig {
    fn merge_with(&self, base: &Self) -> Self {
        Self {
            paths: self.paths.clone().or_else(|| base.paths.clone()),
            warm_interval: self.warm_interval.or(base.warm_interval),
            warm_limit: self.warm_limit.or(base.warm_limit),
        }
    }

    /// Get the glob patterns for files to warm.
    pub fn paths(&self) -> Vec<String> {
        self.paths
            .clone()
            .unwrap_or_else(|| vec!["packages/**/*.md".to_string()])
    }

    /// Get the warming interval in seconds.
    pub fn warm_interval(&self) -> u64 {
        self.warm_interval.unwrap_or(300)
    }

    /// Get the maximum number of files to warm.
    pub fn warm_limit(&self) -> usize {
        self.warm_limit.unwrap_or(1000)
    }
}

impl ProxyConfig {
    fn merge_with(&self, base: &Self) -> Self {
        Self {
            listen: self.listen.clone().or_else(|| base.listen.clone()),
            fresh_duration_secs: self.fresh_duration_secs.or(base.fresh_duration_secs),
            stale_duration_secs: self.stale_duration_secs.or(base.stale_duration_secs),
            max_entries: self.max_entries.or(base.max_entries),
            upstream_url: self
                .upstream_url
                .clone()
                .or_else(|| base.upstream_url.clone()),
            upstream_timeout_secs: self.upstream_timeout_secs.or(base.upstream_timeout_secs),
            ttl_jitter_percent: self.ttl_jitter_percent.or(base.ttl_jitter_percent),
            refresh_interval_secs: self.refresh_interval_secs.or(base.refresh_interval_secs),
            scope: self.scope.merge_with(&base.scope),
            cache: self.cache.merge_with(&base.cache),
            distill: self.distill.merge_with(&base.distill),
            api_key: self.api_key.clone().or_else(|| base.api_key.clone()),
            rate_limit: self.rate_limit.merge_with(&base.rate_limit),
            retry: self.retry.merge_with(&base.retry),
            upstreams: if self.upstreams.is_empty() {
                base.upstreams.clone()
            } else {
                self.upstreams.clone()
            },
            federated: self.federated.merge_with(&base.federated),
            security: self.security.merge_with(&base.security),

            cascade: self.cascade.clone(),

            pool: self.pool.clone(),

            circuit_breaker: self.circuit_breaker.clone(),
            agents: if self.agents.is_empty() {
                base.agents.clone()
            } else {
                self.agents.clone()
            },
            http_listen: self
                .http_listen
                .clone()
                .or_else(|| base.http_listen.clone()),
            cdc: self.cdc.merge_with(&base.cdc),
            peer: self.peer.merge_with(&base.peer),
            socket_tuning: self.socket_tuning.merge_with(&base.socket_tuning),
            shutdown_timeout_secs: self.shutdown_timeout_secs.or(base.shutdown_timeout_secs),
            max_global_connections: self.max_global_connections.or(base.max_global_connections),
            web_ui: self.web_ui.merge_with(&base.web_ui),
        }
    }

    /// Get the listen address with default fallback.
    pub fn listen(&self) -> &str {
        self.listen.as_deref().unwrap_or("127.0.0.1:9999")
    }

    /// Get the HTTP listen address for health/prometheus (defaults to gRPC port + 1).
    pub fn http_listen_addr(&self) -> String {
        if let Some(ref addr) = self.http_listen {
            return addr.clone();
        }
        // Derive from gRPC listen addr by incrementing port
        let grpc_addr = self.listen.as_deref().unwrap_or("127.0.0.1:9999");
        if let Ok(addr) = grpc_addr.parse::<std::net::SocketAddr>() {
            format!("{}:{}", addr.ip(), addr.port().saturating_add(1))
        } else {
            "127.0.0.1:10000".to_string()
        }
    }

    /// Get the fresh duration in seconds.
    pub fn fresh_duration_secs(&self) -> u64 {
        self.fresh_duration_secs.unwrap_or(300)
    }

    /// Get the stale duration in seconds.
    pub fn stale_duration_secs(&self) -> u64 {
        self.stale_duration_secs.unwrap_or(3600)
    }

    /// Get the maximum number of cache entries.
    pub fn max_entries(&self) -> usize {
        self.max_entries.unwrap_or(10000)
    }

    /// Get the upstream URL (if configured).
    pub fn upstream_url(&self) -> Option<&str> {
        self.upstream_url.as_deref()
    }

    /// Get the upstream timeout in seconds.
    pub fn upstream_timeout_secs(&self) -> u64 {
        self.upstream_timeout_secs.unwrap_or(30)
    }

    /// Get the TTL jitter percentage (default: 0.1 = 10%).
    pub fn ttl_jitter_percent(&self) -> f32 {
        self.ttl_jitter_percent.unwrap_or(0.1)
    }

    /// Get the refresh interval in seconds (default: 60).
    pub fn refresh_interval_secs(&self) -> u64 {
        self.refresh_interval_secs.unwrap_or(60)
    }

    /// Phrase texts for scope filtering (from `weighted_phrases` or legacy `seeds`).
    #[must_use]
    pub fn scope_seeds(&self) -> Vec<String> {
        self.scope.phrase_texts()
    }

    /// Mutable legacy `seeds` list (CLI mutate path). Prefer editing `weighted_phrases` in config.
    pub fn scope_seeds_mut(&mut self) -> &mut Vec<String> {
        // Mutating bare seeds; clear weighted so effective_phrases sees seeds.
        if !self.scope.weighted_phrases.is_empty() {
            let texts: Vec<String> = self
                .scope
                .weighted_phrases
                .drain(..)
                .map(|p| p.text)
                .collect();
            if self.scope.seeds.is_empty() {
                self.scope.seeds = texts;
            }
        }
        &mut self.scope.seeds
    }

    /// Get the API key (if configured).
    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    /// Check if authentication is required.
    pub fn requires_auth(&self) -> bool {
        self.api_key.is_some()
    }

    /// Get the rate limit configuration.
    pub fn rate_limit(&self) -> &ProxyRateLimitConfig {
        &self.rate_limit
    }

    /// Get the retry policy configuration.
    pub fn retry(&self) -> &ProxyRetryConfig {
        &self.retry
    }

    /// Get the configured upstreams.
    pub fn upstreams(&self) -> &[UpstreamEndpointConfig] {
        &self.upstreams
    }

    /// Get enabled upstreams only.
    pub fn enabled_upstreams(&self) -> Vec<&UpstreamEndpointConfig> {
        self.upstreams.iter().filter(|u| u.enabled()).collect()
    }

    /// Check if multiple upstreams are configured.
    pub fn has_multiple_upstreams(&self) -> bool {
        self.upstreams.len() > 1
    }

    /// Get the configured agents.
    pub fn agents(&self) -> &[AgentConfig] {
        &self.agents
    }

    /// Check if multi-tenancy is enabled (at least one agent configured).
    pub fn has_agents(&self) -> bool {
        !self.agents.is_empty()
    }

    /// Get enabled agents only.
    pub fn enabled_agents(&self) -> Vec<&AgentConfig> {
        self.agents.iter().filter(|a| a.enabled).collect()
    }

    /// Normalize legacy `upstream_url` into the `upstreams` array.
    ///
    /// If `upstream_url` is set and `upstreams` is empty, creates a single upstream
    /// entry with id "default". Returns `true` if a conversion was performed.
    pub fn normalize_upstreams(&mut self) -> bool {
        if self.upstreams.is_empty() {
            if let Some(ref url) = self.upstream_url {
                tracing::warn!(
                    "proxy.upstream_url is deprecated; \
                     use [[proxy.upstreams]] with id and url instead"
                );
                self.upstreams.push(UpstreamEndpointConfig {
                    id: "default".to_string(),
                    url: url.clone(),
                    timeout_secs: self.upstream_timeout_secs,
                    weight: None,
                    priority: None,
                    max_concurrent: None,
                    enabled: None,

                    version_endpoint: None,
                    version_poll_interval_secs: None,
                    upstream_type: None,
                    query_mode: None,
                    table: None,
                    embedding_column: None,
                    content_column: None,
                    metadata_columns: Vec::new(),
                    distance_metric: None,
                    dimensions: None,
                    index: None,
                    search_fields: Vec::new(),
                    return_fields: Vec::new(),
                    api_key: None,
                });
                return true;
            }
        }
        false
    }

    /// Apply environment variable overrides for deployment.
    ///
    /// Supported env vars:
    /// - `CONPROXY_HOST` — Override listen host
    /// - `CONPROXY_PORT` — Override listen port
    /// - `CONPROXY_API_KEY` — Override API key
    /// - `CONPROXY_CACHE_MAX_ENTRIES` — Override max cache entries
    /// - `CONPROXY_UPSTREAM_{ID}_URL` — Override URL for upstream with matching id (case-insensitive)
    ///
    /// Returns the number of overrides applied.
    pub fn apply_env_overrides(&mut self) -> usize {
        let mut count: usize = 0;

        // Listen address override
        let host = std::env::var("CONPROXY_HOST").ok();
        let port = std::env::var("CONPROXY_PORT").ok();
        if host.is_some() || port.is_some() {
            let current = self
                .listen
                .clone()
                .unwrap_or_else(|| "127.0.0.1:3000".to_string());
            let parts: Vec<&str> = current.rsplitn(2, ':').collect();
            let cur_port = parts.first().unwrap_or(&"3000");
            let cur_host = if let Some(host_part) = parts.get(1) {
                host_part
            } else {
                "127.0.0.1"
            };
            let new_host = host.as_deref().unwrap_or(cur_host);
            let new_port = port.as_deref().unwrap_or(cur_port);
            self.listen = Some(format!("{}:{}", new_host, new_port));
            count = count.saturating_add(1);
        }

        // API key override
        if let Ok(key) = std::env::var("CONPROXY_API_KEY") {
            self.api_key = Some(key.clone());
            self.security.api_key = Some(key);
            count = count.saturating_add(1);
        }

        // Cache max entries override
        if let Ok(val) = std::env::var("CONPROXY_CACHE_MAX_ENTRIES") {
            if let Ok(n) = val.parse::<usize>() {
                self.max_entries = Some(n);
                count = count.saturating_add(1);
            }
        }

        // Per-upstream URL overrides: CONPROXY_UPSTREAM_{ID}_URL
        for upstream in &mut self.upstreams {
            let env_key = format!(
                "CONPROXY_UPSTREAM_{}_URL",
                upstream.id.to_uppercase().replace('-', "_")
            );
            if let Ok(url) = std::env::var(&env_key) {
                upstream.url = url;
                count = count.saturating_add(1);
            }
        }

        count
    }

    /// Get the CDC configuration.
    pub fn cdc(&self) -> &CdcConfig {
        &self.cdc
    }

    /// Get the peer replication configuration.
    pub fn peer(&self) -> &PeerConfig {
        &self.peer
    }

    /// Check if CDC is effectively enabled (explicit or via peer replication).
    pub fn cdc_enabled(&self) -> bool {
        self.cdc.enabled() || self.peer.enabled()
    }

    /// Get the graceful shutdown timeout in seconds (default: 30).
    pub fn shutdown_timeout_secs(&self) -> u64 {
        self.shutdown_timeout_secs.unwrap_or(30)
    }

    /// Get the maximum global concurrent connections (default: 1000).
    pub fn max_global_connections(&self) -> usize {
        self.max_global_connections.unwrap_or(1000)
    }
}

impl Config {
    /// Load config with global + local merge.
    ///
    /// When neither a global (`~/.conproxy/`) nor a local (`.conproxy/`)
    /// configuration exists, falls back to a default in-memory local config.
    /// The proxy can therefore run on first invocation without any prior
    /// `init` step. Use [`Config::save`] to persist changes.
    ///
    /// # Errors
    ///
    /// Returns IO errors on file read failure, TOML parse errors on invalid
    /// syntax, and [`ConproxyError::ConfigValidation`] when the merged config
    /// fails validation.
    pub fn load() -> Result<Self> {
        let global = Self::load_global()?;
        let local = Self::load_local()?;

        // Merge: global defaults, local overrides. If both are absent, fall
        // back to a default local config so the proxy can run on first use.
        let merged = match (&global, &local) {
            (Some(g), Some(l)) => g.merge_with(l),
            (Some(g), None) => g.clone(),
            (None, Some(l)) => l.clone(),
            (None, None) => ConfigFile::default_local(),
        };

        // Validate merged config — fail hard on invalid configs
        merged.validate().map_err(ConproxyError::ConfigValidation)?;

        // Find local root if it exists
        let local_root = if local.is_some() {
            Some(Self::find_local_root()?)
        } else {
            None
        };

        Ok(Self {
            config: merged,
            local_root,
        })
    }

    /// Re-read configuration from disk (for hot-reload).
    ///
    /// Re-parses `conproxy.toml` using the same merge logic as `load()`.
    /// Returns a fresh `Config` without modifying the current instance.
    ///
    /// # Errors
    ///
    /// Propagates the same errors as [`Config::load`]: IO, TOML parse, and
    /// merged-config validation failures.
    pub fn reload() -> Result<Self> {
        Self::load()
    }

    /// Load from specific path (no merge).
    ///
    /// # Errors
    ///
    /// Returns IO errors when the file cannot be read, and TOML parse errors
    /// when the file content is not valid TOML or does not match the config schema.
    pub fn load_from(path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: ConfigFile = toml::from_str(&content)?;
        config.validate().map_err(ConproxyError::ConfigValidation)?;
        Ok(Self {
            config,
            local_root: PathBuf::from(path).parent().map(|p| p.to_path_buf()),
        })
    }

    /// Load only global config.
    ///
    /// Returns `None` when the global config file does not exist.
    ///
    /// # Errors
    ///
    /// Returns IO errors when the file exists but cannot be read, and TOML
    /// parse errors when the content is not valid.
    pub fn load_global() -> Result<Option<ConfigFile>> {
        let path = Self::global_config_path();
        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            let config: ConfigFile = toml::from_str(&content)?;
            Ok(Some(config))
        } else {
            Ok(None)
        }
    }

    /// Load only local config.
    ///
    /// Returns `None` when the local config file does not exist.
    ///
    /// # Errors
    ///
    /// Returns IO errors when the file exists but cannot be read, and TOML
    /// parse errors when the content is not valid.
    pub fn load_local() -> Result<Option<ConfigFile>> {
        let path = Self::local_config_path();
        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            let config: ConfigFile = toml::from_str(&content)?;
            Ok(Some(config))
        } else {
            Ok(None)
        }
    }

    /// Find the local .conproxy root directory
    fn find_local_root() -> Result<PathBuf> {
        let cwd = std::env::current_dir()?;
        let local_dir = cwd.join(".conproxy");
        if local_dir.exists() {
            Ok(local_dir)
        } else {
            Err(ConproxyError::NotInitialized)
        }
    }

    /// Save to local config.
    ///
    /// Auto-creates `.conproxy/` + subdirs + `.gitignore` on first write
    /// so callers don't need a separate init step.
    ///
    /// # Errors
    ///
    /// Returns TOML serialization errors if the config cannot be serialized,
    /// and IO errors when writing to the local config file fails.
    pub fn save(&self) -> Result<()> {
        Self::ensure_local_dirs()?;
        let path = Self::local_config_path();
        let content = toml::to_string_pretty(&self.config)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Save to global config.
    ///
    /// # Errors
    ///
    /// Returns TOML serialization errors if the config cannot be serialized,
    /// and IO errors when writing to the global config file fails.
    pub fn save_global(&self) -> Result<()> {
        let path = Self::global_config_path();
        let content = toml::to_string_pretty(&self.config)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    // === Paths ===

    /// Global conproxy directory (~/.conproxy/)
    ///
    /// Tries `dirs::home_dir()` first, falls back to `$HOME` env var.
    /// Panics if neither is available (extremely rare on normal systems).
    pub fn global_dir() -> PathBuf {
        let home = dirs::home_dir().or_else(|| std::env::var("HOME").ok().map(PathBuf::from));
        home.unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".conproxy")
    }

    /// Global config file path (~/.conproxy/conproxy.toml)
    pub fn global_config_path() -> PathBuf {
        Self::global_dir().join("conproxy.toml")
    }

    /// Global models directory (~/.conproxy/models/)
    pub fn global_models_dir() -> PathBuf {
        Self::global_dir().join("models")
    }

    /// Local conproxy directory (.conproxy/)
    pub fn local_dir() -> PathBuf {
        PathBuf::from(".conproxy")
    }

    /// Local config file path (.conproxy/conproxy.toml)
    pub fn local_config_path() -> PathBuf {
        Self::local_dir().join("conproxy.toml")
    }

    // === Convenience accessors ===

    pub fn conproxy_dir(&self) -> PathBuf {
        self.local_root
            .clone()
            .unwrap_or_else(|| PathBuf::from(".conproxy"))
    }

    pub fn index_dir(&self) -> PathBuf {
        self.conproxy_dir().join("index")
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.conproxy_dir().join("cache")
    }

    pub fn packages_dir(&self) -> PathBuf {
        self.conproxy_dir().join("packages")
    }

    pub fn web_dir(&self) -> PathBuf {
        self.conproxy_dir().join("web")
    }

    pub fn models_dir(&self) -> PathBuf {
        Self::global_models_dir()
    }

    /// Ensure the local `.conproxy/` directory structure exists.
    ///
    /// Creates `.conproxy/` + subdirs (`packages`, `index`, `cache`, `web`)
    /// and a `.gitignore` (only on first write) on demand. Called automatically
    /// by [`Config::save`] so callers do not need a separate init step.
    ///
    /// # Errors
    ///
    /// Returns IO errors when any of the directory or file operations fail.
    pub fn ensure_local_dirs() -> Result<()> {
        let local_dir = Self::local_dir();
        std::fs::create_dir_all(&local_dir)?;
        std::fs::create_dir_all(local_dir.join("packages"))?;
        std::fs::create_dir_all(local_dir.join("index"))?;
        std::fs::create_dir_all(local_dir.join("cache"))?;
        std::fs::create_dir_all(local_dir.join("web"))?;

        let gitignore_path = local_dir.join(".gitignore");
        if !gitignore_path.exists() {
            std::fs::write(&gitignore_path, "cache/\n*.pid\n")?;
        }
        Ok(())
    }
}

/// Resolve `${ENV_VAR}` references in a string.
///
/// Returns `None` if the input uses env-var syntax but the variable is unset,
/// `Some(s)` otherwise (the original string if no env-var syntax).
pub(crate) fn resolve_env_ref(s: &str) -> Option<String> {
    if s.starts_with("${") && s.ends_with('}') {
        #[allow(clippy::arithmetic_side_effects)]
        let var = &s[2..s.len() - 1];
        match std::env::var(var) {
            Ok(v) if !v.is_empty() => Some(v),
            _ => None,
        }
    } else {
        Some(s.to_string())
    }
}

#[cfg(test)]
mod tests;
