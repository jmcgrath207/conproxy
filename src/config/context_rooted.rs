//! Context-rooted config (plan 10).
//!
//! Named upstream/embedder resources + per-context legs (`ref` + full overrides).
//! T0–T1: schema + resolve. T2–T3: project-to-proxy for runtime.

use super::{
    AgentConfig, CascadeConfig, FederatedConfig, ProxyCacheConfig, ProxyConfig, ProxyScopeConfig,
    UpstreamEndpointConfig,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Process-level listen addresses (`[server]`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_listen: Option<String>,
}

impl ServerConfig {
    pub fn listen(&self) -> &str {
        self.listen.as_deref().unwrap_or("127.0.0.1:9999")
    }

    pub fn merge_with(&self, base: &Self) -> Self {
        Self {
            listen: self.listen.clone().or_else(|| base.listen.clone()),
            http_listen: self
                .http_listen
                .clone()
                .or_else(|| base.http_listen.clone()),
        }
    }
}

/// Named upstream resource (`[upstreams.name]`).
///
/// Map key is the resource id. Field `r#type` accepts TOML key `type`
/// (alias of `upstream_type`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UpstreamResourceConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// Backend type. TOML: `type` or `upstream_type`.
    #[serde(
        default,
        rename = "type",
        alias = "upstream_type",
        skip_serializing_if = "Option::is_none"
    )]
    pub upstream_type: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<usize>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_endpoint: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_poll_interval_secs: Option<u64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_mode: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_column: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_column: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metadata_columns: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance_metric: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<usize>,

    /// ES/OpenSearch/Meili index; also accepts `collection` alias (Qdrant).
    #[serde(default, alias = "collection", skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub search_fields: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub return_fields: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

/// Context leg: required `ref` + optional full overrides of the resource.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ContextLegConfig {
    /// Resource id in `[upstreams]`.
    #[serde(rename = "ref")]
    pub resource_ref: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    #[serde(
        default,
        rename = "type",
        alias = "upstream_type",
        skip_serializing_if = "Option::is_none"
    )]
    pub upstream_type: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<usize>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_endpoint: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_poll_interval_secs: Option<u64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_mode: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_column: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_column: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_columns: Option<Vec<String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance_metric: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<usize>,

    #[serde(default, alias = "collection", skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_fields: Option<Vec<String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_fields: Option<Vec<String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

/// Embedder resource (`[embedders.name]`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EmbedderResourceConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_size: Option<usize>,
}

/// Context embedder attachment: `ref` + optional overrides.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ContextEmbedderConfig {
    #[serde(rename = "ref")]
    pub resource_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_size: Option<usize>,
}

/// Per-context cache (owned; never shared across contexts).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ContextCacheConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_entries: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_memory_mb: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eviction_policy: Option<String>,
}

impl ContextCacheConfig {
    /// Convert to legacy `ProxyCacheConfig` shape (limits only; TTLs stay separate).
    pub fn to_proxy_cache(&self) -> ProxyCacheConfig {
        ProxyCacheConfig {
            max_memory_mb: self.max_memory_mb,
            eviction_policy: self.eviction_policy.clone(),
            ..Default::default()
        }
    }
}

/// One named context (`[contexts.name]`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NamedContextConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub upstreams: Vec<ContextLegConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedder: Option<ContextEmbedderConfig>,

    #[serde(default)]
    pub cache: ContextCacheConfig,

    #[serde(default)]
    pub scope: ProxyScopeConfig,

    #[serde(default)]
    pub cascade: CascadeConfig,

    #[serde(default)]
    pub federated: FederatedConfig,
}

/// Resolved leg after `merge(resource, leg overrides)`.
#[derive(Debug, Clone)]
pub struct ResolvedUpstreamLeg {
    /// Pool key = resource id (`upstreams.name`).
    pub resource_id: String,
    /// Fully merged endpoint (id = resource_id for pool lookup).
    pub endpoint: UpstreamEndpointConfig,
}

/// Resolved embedder after merge.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedEmbedder {
    pub resource_id: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub batch_size: Option<usize>,
}

/// Fully resolved context (policy unit).
#[derive(Debug, Clone)]
pub struct ResolvedContext {
    pub id: String,
    pub is_default: bool,
    pub description: Option<String>,
    pub legs: Vec<ResolvedUpstreamLeg>,
    pub embedder: Option<ResolvedEmbedder>,
    pub cache: ContextCacheConfig,
    pub scope: ProxyScopeConfig,
    pub cascade: CascadeConfig,
    pub federated: FederatedConfig,
}

/// Merge resource + leg overrides into an `UpstreamEndpointConfig`.
///
/// Leg fields win when set; omitted fields keep the resource value.
/// Endpoint `id` is always the **resource id** (pool-by-resource-id).
///
/// # Errors
/// Returns error if resource has no `url` after merge.
pub fn resolve_leg(
    resource_id: &str,
    resource: &UpstreamResourceConfig,
    leg: &ContextLegConfig,
) -> Result<ResolvedUpstreamLeg, String> {
    let url = leg
        .url
        .clone()
        .or_else(|| resource.url.clone())
        .ok_or_else(|| {
            format!("upstream resource '{resource_id}': url required after leg merge")
        })?;

    let metadata_columns = leg
        .metadata_columns
        .clone()
        .unwrap_or_else(|| resource.metadata_columns.clone());
    let search_fields = leg
        .search_fields
        .clone()
        .unwrap_or_else(|| resource.search_fields.clone());
    let return_fields = leg
        .return_fields
        .clone()
        .unwrap_or_else(|| resource.return_fields.clone());

    let endpoint = UpstreamEndpointConfig {
        id: resource_id.to_string(),
        url,
        timeout_secs: leg.timeout_secs.or(resource.timeout_secs),
        weight: leg.weight.or(resource.weight),
        priority: leg.priority.or(resource.priority),
        max_concurrent: leg.max_concurrent.or(resource.max_concurrent),
        enabled: leg.enabled.or(resource.enabled),
        version_endpoint: leg
            .version_endpoint
            .clone()
            .or_else(|| resource.version_endpoint.clone()),
        version_poll_interval_secs: leg
            .version_poll_interval_secs
            .or(resource.version_poll_interval_secs),
        upstream_type: leg
            .upstream_type
            .clone()
            .or_else(|| resource.upstream_type.clone()),
        query_mode: leg
            .query_mode
            .clone()
            .or_else(|| resource.query_mode.clone()),
        table: leg.table.clone().or_else(|| resource.table.clone()),
        embedding_column: leg
            .embedding_column
            .clone()
            .or_else(|| resource.embedding_column.clone()),
        content_column: leg
            .content_column
            .clone()
            .or_else(|| resource.content_column.clone()),
        metadata_columns,
        distance_metric: leg
            .distance_metric
            .clone()
            .or_else(|| resource.distance_metric.clone()),
        dimensions: leg.dimensions.or(resource.dimensions),
        index: leg.index.clone().or_else(|| resource.index.clone()),
        search_fields,
        return_fields,
        api_key: leg.api_key.clone().or_else(|| resource.api_key.clone()),
    };

    endpoint.validate()?;

    Ok(ResolvedUpstreamLeg {
        resource_id: resource_id.to_string(),
        endpoint,
    })
}

/// Merge embedder resource + context overrides.
pub fn resolve_embedder(
    resource_id: &str,
    resource: &EmbedderResourceConfig,
    attach: &ContextEmbedderConfig,
) -> ResolvedEmbedder {
    ResolvedEmbedder {
        resource_id: resource_id.to_string(),
        provider: attach
            .provider
            .clone()
            .or_else(|| resource.provider.clone()),
        model: attach.model.clone().or_else(|| resource.model.clone()),
        api_key: attach.api_key.clone().or_else(|| resource.api_key.clone()),
        base_url: attach
            .base_url
            .clone()
            .or_else(|| resource.base_url.clone()),
        batch_size: attach.batch_size.or(resource.batch_size),
    }
}

/// Resolve all contexts from resource maps.
///
/// # Errors
/// Missing ref, bad default count, empty contexts, leg/url validation.
pub fn resolve_all_contexts(
    upstreams: &HashMap<String, UpstreamResourceConfig>,
    embedders: &HashMap<String, EmbedderResourceConfig>,
    contexts: &HashMap<String, NamedContextConfig>,
) -> Result<Vec<ResolvedContext>, String> {
    if contexts.is_empty() {
        return Ok(Vec::new());
    }

    let mut resolved = Vec::with_capacity(contexts.len());
    for (id, ctx) in contexts {
        let mut legs = Vec::with_capacity(ctx.upstreams.len());
        for leg in &ctx.upstreams {
            let resource = upstreams.get(&leg.resource_ref).ok_or_else(|| {
                format!(
                    "context '{id}': upstream ref '{}' not found in [upstreams]",
                    leg.resource_ref
                )
            })?;
            legs.push(resolve_leg(&leg.resource_ref, resource, leg)?);
        }

        let embedder = if let Some(ref att) = ctx.embedder {
            let resource = embedders.get(&att.resource_ref).ok_or_else(|| {
                format!(
                    "context '{id}': embedder ref '{}' not found in [embedders]",
                    att.resource_ref
                )
            })?;
            Some(resolve_embedder(&att.resource_ref, resource, att))
        } else {
            None
        };

        resolved.push(ResolvedContext {
            id: id.clone(),
            is_default: ctx.default.unwrap_or(false),
            description: ctx.description.clone(),
            legs,
            embedder,
            cache: ctx.cache.clone(),
            scope: ctx.scope.clone(),
            cascade: ctx.cascade.clone(),
            federated: ctx.federated.clone(),
        });
    }

    Ok(resolved)
}

/// Validate context-rooted tables when any are present.
///
/// # Errors
/// Returns human-readable validation errors.
pub fn validate_context_rooted(
    upstreams: &HashMap<String, UpstreamResourceConfig>,
    embedders: &HashMap<String, EmbedderResourceConfig>,
    contexts: &HashMap<String, NamedContextConfig>,
    agents: &[AgentConfig],
) -> Result<(), String> {
    if contexts.is_empty() {
        // Legacy [proxy]-only configs skip this path.
        return Ok(());
    }

    if upstreams.is_empty() {
        return Err("contexts defined but [upstreams] is empty".into());
    }

    let default_count = contexts
        .values()
        .filter(|c| c.default.unwrap_or(false))
        .count();
    if default_count == 0 {
        return Err("contexts: exactly one context must have default = true".into());
    }
    if default_count > 1 {
        return Err("contexts: only one context may have default = true".into());
    }

    for (name, res) in upstreams {
        if let Some(ref url) = res.url {
            validate_url(name, url)?;
        }
        // Build a synthetic endpoint for type validation when type set.
        if res.upstream_type.is_some() || res.url.is_some() {
            let stub = ContextLegConfig {
                resource_ref: name.clone(),
                ..Default::default()
            };
            // URL may be missing until leg override — only validate type fields.
            if let Some(ref ut) = res.upstream_type {
                let tmp = UpstreamEndpointConfig {
                    id: name.clone(),
                    url: res
                        .url
                        .clone()
                        .unwrap_or_else(|| "http://placeholder".into()),
                    upstream_type: Some(ut.clone()),
                    query_mode: res.query_mode.clone(),
                    table: res.table.clone(),
                    distance_metric: res.distance_metric.clone(),
                    ..Default::default()
                };
                tmp.validate()
                    .map_err(|e| format!("upstreams.{name}: {e}"))?;
            }
            let _ = stub;
        }
    }

    for (name, emb) in embedders {
        if let Some(ref p) = emb.provider {
            let valid = ["onnx", "openai", "voyage", "cohere", "google", "ollama"];
            if !valid.contains(&p.as_str()) {
                // Keep open — unknown providers may be planned; only warn-level via reject empty.
                if p.is_empty() {
                    return Err(format!("embedders.{name}: provider must not be empty"));
                }
            }
        }
    }

    let resolved = resolve_all_contexts(upstreams, embedders, contexts)?;
    for ctx in &resolved {
        if ctx.legs.is_empty() {
            return Err(format!(
                "context '{}': at least one upstream leg required",
                ctx.id
            ));
        }
        for leg in &ctx.legs {
            validate_url(&leg.resource_id, &leg.endpoint.url)?;
            if let Some(t) = leg.endpoint.timeout_secs {
                if t == 0 || t > 300 {
                    return Err(format!(
                        "context '{}': leg '{}': timeout_secs must be 1..=300, got {t}",
                        ctx.id, leg.resource_id
                    ));
                }
            }
        }
        ctx.scope
            .validate()
            .map_err(|e| format!("context '{}': scope: {e}", ctx.id))?;
        if let Some(max) = ctx.cache.max_entries {
            if max == 0 {
                return Err(format!(
                    "context '{}': cache.max_entries must be > 0",
                    ctx.id
                ));
            }
        }
    }

    for agent in agents {
        if agent.id.is_empty() {
            return Err("agent id must not be empty".into());
        }
        for c in &agent.allowed_contexts {
            if !contexts.contains_key(c) {
                return Err(format!(
                    "agent '{}': allowed_contexts entry '{c}' not found in [contexts]",
                    agent.id
                ));
            }
        }
        if let Some(ref dc) = agent.default_context {
            if !contexts.contains_key(dc) {
                return Err(format!(
                    "agent '{}': default_context '{dc}' not found in [contexts]",
                    agent.id
                ));
            }
        }
    }

    Ok(())
}

fn validate_url(id: &str, url: &str) -> Result<(), String> {
    if !(url.starts_with("http://")
        || url.starts_with("https://")
        || url.starts_with("postgres://")
        || url.starts_with("postgresql://"))
    {
        return Err(format!(
            "upstream '{id}': URL must start with http(s):// or postgres(ql)://, got '{url}'"
        ));
    }
    Ok(())
}

/// Unique resource ids used across all resolved legs (pool keys).
pub fn pool_resource_ids(resolved: &[ResolvedContext]) -> Vec<String> {
    let mut ids: Vec<String> = resolved
        .iter()
        .flat_map(|c| c.legs.iter().map(|l| l.resource_id.clone()))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// Project context-rooted tables into a `ProxyConfig` for existing runtime builders.
///
/// Uses the default context's policy (scope/cache/cascade/federated) and pools
/// unique resource endpoints (first-seen wins). Preserves process-level fields
/// from `base` (security, retry, peer, agents, listen, …).
///
/// # Errors
/// Missing default context or resolve failures.
pub fn project_to_proxy(
    base: &ProxyConfig,
    resolved: &[ResolvedContext],
) -> Result<ProxyConfig, String> {
    let default_ctx = resolved
        .iter()
        .find(|c| c.is_default)
        .or_else(|| resolved.first())
        .ok_or_else(|| "project_to_proxy: no resolved contexts".to_string())?;

    let mut out = base.clone();
    out.scope = default_ctx.scope.clone();
    out.cascade = default_ctx.cascade.clone();
    out.federated = default_ctx.federated.clone();

    if let Some(f) = default_ctx.cache.fresh_secs {
        out.fresh_duration_secs = Some(f);
    }
    if let Some(s) = default_ctx.cache.stale_secs {
        out.stale_duration_secs = Some(s);
    }
    if let Some(m) = default_ctx.cache.max_entries {
        out.max_entries = Some(m);
    }
    if default_ctx.cache.max_memory_mb.is_some() || default_ctx.cache.eviction_policy.is_some() {
        out.cache = ProxyCacheConfig {
            max_memory_mb: default_ctx.cache.max_memory_mb.or(out.cache.max_memory_mb),
            eviction_policy: default_ctx
                .cache
                .eviction_policy
                .clone()
                .or(out.cache.eviction_policy.clone()),
            ..out.cache.clone()
        };
    }

    // Unique legs by resource_id (pool-by-resource-id).
    let mut seen = std::collections::HashSet::new();
    let mut upstreams = Vec::new();
    for ctx in resolved {
        for leg in &ctx.legs {
            if seen.insert(leg.resource_id.clone()) {
                upstreams.push(leg.endpoint.clone());
            }
        }
    }
    if !upstreams.is_empty() {
        out.upstreams = upstreams;
        out.upstream_url = None;
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_toml() -> &'static str {
        r#"
[server]
listen = "127.0.0.1:9999"

[upstreams.meili]
url = "http://127.0.0.1:7700"
type = "meilisearch"
timeout_secs = 30
api_key = "master"

[upstreams.qdrant]
url = "http://127.0.0.1:6333"
type = "qdrant"
collection = "chunks"

[embedders.mini]
provider = "onnx"
model = "all-MiniLM-L6-v2"

[contexts.docs]
default = true
description = "Docs RAG"

[[contexts.docs.upstreams]]
ref = "meili"
priority = 0
timeout_secs = 5

[[contexts.docs.upstreams]]
ref = "qdrant"
priority = 1
query_mode = "vector_only"

[contexts.docs.embedder]
ref = "mini"

[contexts.docs.cache]
fresh_secs = 300
stale_secs = 3600
max_entries = 10000

[contexts.docs.scope]
mode = "filter"
min_similarity = 0.25

[[contexts.docs.scope.weighted_phrases]]
text = "product api"
weight = 1.0

[contexts.support]
[[contexts.support.upstreams]]
ref = "meili"
priority = 0
timeout_secs = 15
index = "support"

[[contexts.support.upstreams]]
ref = "qdrant"
priority = 1

[contexts.support.cache]
fresh_secs = 60
max_entries = 5000

[contexts.support.scope]
mode = "filter"
min_similarity = 0.35
"#
    }

    #[derive(Debug, Deserialize)]
    struct FileSlice {
        #[serde(default)]
        server: ServerConfig,
        #[serde(default)]
        upstreams: HashMap<String, UpstreamResourceConfig>,
        #[serde(default)]
        embedders: HashMap<String, EmbedderResourceConfig>,
        #[serde(default)]
        contexts: HashMap<String, NamedContextConfig>,
    }

    fn parse_sample() -> FileSlice {
        toml::from_str(sample_toml()).expect("sample toml")
    }

    #[test]
    fn parse_canonical_toml() {
        let f = parse_sample();
        assert_eq!(f.server.listen(), "127.0.0.1:9999");
        assert_eq!(f.upstreams.len(), 2);
        assert_eq!(
            f.upstreams["meili"].upstream_type.as_deref(),
            Some("meilisearch")
        );
        assert_eq!(f.upstreams["qdrant"].index.as_deref(), Some("chunks"));
        assert!(f.contexts["docs"].default.unwrap_or(false));
        assert_eq!(f.contexts["docs"].upstreams.len(), 2);
        assert_eq!(f.contexts["docs"].upstreams[0].resource_ref, "meili");
        assert_eq!(f.contexts["docs"].upstreams[0].timeout_secs, Some(5));
        assert_eq!(
            f.embedders["mini"].model.as_deref(),
            Some("all-MiniLM-L6-v2")
        );
    }

    #[test]
    fn merge_timeout_override_keeps_base_url() {
        let f = parse_sample();
        let res = &f.upstreams["meili"];
        let leg = &f.contexts["docs"].upstreams[0];
        let resolved = resolve_leg("meili", res, leg).unwrap();
        assert_eq!(resolved.resource_id, "meili");
        assert_eq!(resolved.endpoint.url, "http://127.0.0.1:7700");
        assert_eq!(resolved.endpoint.timeout_secs, Some(5)); // override
        assert_eq!(resolved.endpoint.priority, Some(0));
        assert_eq!(resolved.endpoint.api_key.as_deref(), Some("master")); // base
    }

    #[test]
    fn merge_can_override_url_api_key_index() {
        let res = UpstreamResourceConfig {
            url: Some("http://base:7700".into()),
            upstream_type: Some("meilisearch".into()),
            api_key: Some("base-key".into()),
            index: Some("base-idx".into()),
            ..Default::default()
        };
        let leg = ContextLegConfig {
            resource_ref: "meili".into(),
            url: Some("http://override:7700".into()),
            api_key: Some("leg-key".into()),
            index: Some("support".into()),
            ..Default::default()
        };
        let r = resolve_leg("meili", &res, &leg).unwrap();
        assert_eq!(r.endpoint.url, "http://override:7700");
        assert_eq!(r.endpoint.api_key.as_deref(), Some("leg-key"));
        assert_eq!(r.endpoint.index.as_deref(), Some("support"));
    }

    #[test]
    fn two_contexts_share_same_resource_id() {
        let f = parse_sample();
        let all = resolve_all_contexts(&f.upstreams, &f.embedders, &f.contexts).unwrap();
        assert_eq!(all.len(), 2);
        let pool = pool_resource_ids(&all);
        assert_eq!(pool, vec!["meili".to_string(), "qdrant".to_string()]);
        // both contexts have meili leg with same resource_id
        for ctx in &all {
            assert!(ctx.legs.iter().any(|l| l.resource_id == "meili"));
        }
        let docs = all.iter().find(|c| c.id == "docs").unwrap();
        let support = all.iter().find(|c| c.id == "support").unwrap();
        let d_meili = docs.legs.iter().find(|l| l.resource_id == "meili").unwrap();
        let s_meili = support
            .legs
            .iter()
            .find(|l| l.resource_id == "meili")
            .unwrap();
        assert_eq!(d_meili.endpoint.timeout_secs, Some(5));
        assert_eq!(s_meili.endpoint.timeout_secs, Some(15));
        assert_eq!(s_meili.endpoint.index.as_deref(), Some("support"));
    }

    #[test]
    fn validate_missing_ref_fails() {
        let mut f = parse_sample();
        f.contexts.get_mut("docs").unwrap().upstreams[0].resource_ref = "nope".into();
        let err =
            validate_context_rooted(&f.upstreams, &f.embedders, &f.contexts, &[]).unwrap_err();
        assert!(err.contains("not found"), "got: {err}");
    }

    #[test]
    fn validate_two_defaults_fails() {
        let mut f = parse_sample();
        f.contexts.get_mut("support").unwrap().default = Some(true);
        let err =
            validate_context_rooted(&f.upstreams, &f.embedders, &f.contexts, &[]).unwrap_err();
        assert!(err.contains("only one"), "got: {err}");
    }

    #[test]
    fn validate_zero_defaults_fails() {
        let mut f = parse_sample();
        f.contexts.get_mut("docs").unwrap().default = Some(false);
        let err =
            validate_context_rooted(&f.upstreams, &f.embedders, &f.contexts, &[]).unwrap_err();
        assert!(err.contains("exactly one"), "got: {err}");
    }

    #[test]
    fn validate_ok_sample() {
        let f = parse_sample();
        validate_context_rooted(&f.upstreams, &f.embedders, &f.contexts, &[]).unwrap();
    }

    #[test]
    fn cache_configs_independent_per_context() {
        let f = parse_sample();
        let all = resolve_all_contexts(&f.upstreams, &f.embedders, &f.contexts).unwrap();
        let docs = all.iter().find(|c| c.id == "docs").unwrap();
        let support = all.iter().find(|c| c.id == "support").unwrap();
        assert_eq!(docs.cache.max_entries, Some(10_000));
        assert_eq!(support.cache.max_entries, Some(5_000));
        assert_ne!(docs.cache.fresh_secs, support.cache.fresh_secs);
        // distinct owned objects
        assert_eq!(docs.cache.fresh_secs, Some(300));
        assert_eq!(support.cache.fresh_secs, Some(60));
    }

    #[test]
    fn agent_unknown_context_fails() {
        let f = parse_sample();
        let agents = vec![AgentConfig {
            id: "bot".into(),
            api_key: "k".into(),
            default_context: Some("missing".into()),
            allowed_contexts: vec!["docs".into()],
            priority_class: None,
            rate_limit_rps: None,
            enabled: true,
        }];
        let err =
            validate_context_rooted(&f.upstreams, &f.embedders, &f.contexts, &agents).unwrap_err();
        assert!(err.contains("default_context"), "got: {err}");
    }

    #[test]
    fn empty_contexts_skips_validate() {
        validate_context_rooted(&HashMap::new(), &HashMap::new(), &HashMap::new(), &[]).unwrap();
    }

    #[test]
    fn project_to_proxy_uses_default_policy() {
        let f = parse_sample();
        let resolved = resolve_all_contexts(&f.upstreams, &f.embedders, &f.contexts).unwrap();
        let base = ProxyConfig::default();
        let projected = project_to_proxy(&base, &resolved).unwrap();
        // default context is docs: fresh 300, max 10000, scope min 0.25
        assert_eq!(projected.fresh_duration_secs, Some(300));
        assert_eq!(projected.max_entries, Some(10_000));
        assert!((projected.scope.min_similarity() - 0.25).abs() < f32::EPSILON);
        // both meili + qdrant in pool
        let ids: Vec<_> = projected.upstreams.iter().map(|u| u.id.as_str()).collect();
        assert!(ids.contains(&"meili"));
        assert!(ids.contains(&"qdrant"));
    }

    /// T4: every proxy-related example parses, validates, resolves.
    #[test]
    fn examples_context_rooted_toml_load() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples");
        let files = [
            "qdrant-quickstart.toml",
            "meilisearch-quickstart.toml",
            "multi-upstream-cascade.toml",
            "federated-search.toml",
            "multi-context.toml",
        ];
        for name in files {
            let path = root.join(name);
            let raw = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            let f: crate::config::ConfigFile =
                toml::from_str(&raw).unwrap_or_else(|e| panic!("parse {name}: {e}"));
            assert!(
                f.is_context_rooted(),
                "{name}: expected [contexts] non-empty"
            );
            f.validate()
                .unwrap_or_else(|e| panic!("validate {name}: {e}"));
            let resolved = f
                .resolve_contexts()
                .unwrap_or_else(|e| panic!("resolve {name}: {e}"));
            assert!(!resolved.is_empty(), "{name}: no resolved contexts");
            assert!(
                resolved.iter().filter(|c| c.is_default).count() == 1,
                "{name}: need exactly one default context"
            );
            let proxy = f
                .effective_proxy()
                .unwrap_or_else(|e| panic!("effective_proxy {name}: {e}"));
            assert!(
                !proxy.upstreams.is_empty(),
                "{name}: projected proxy has no upstreams"
            );
        }
    }

    #[test]
    fn multi_context_example_isolated_cache_and_shared_ref() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("examples/multi-context.toml");
        let raw = std::fs::read_to_string(&path).unwrap();
        let f: crate::config::ConfigFile = toml::from_str(&raw).unwrap();
        let all = f.resolve_contexts().unwrap();
        assert_eq!(all.len(), 2);
        let docs = all.iter().find(|c| c.id == "docs").unwrap();
        let support = all.iter().find(|c| c.id == "support").unwrap();
        assert!(docs.is_default);
        assert_eq!(docs.cache.fresh_secs, Some(300));
        assert_eq!(support.cache.fresh_secs, Some(60));
        assert_ne!(docs.cache.max_entries, support.cache.max_entries);
        // shared resource id
        assert_eq!(docs.legs[0].resource_id, "meili");
        assert_eq!(support.legs[0].resource_id, "meili");
        assert_eq!(docs.legs[0].endpoint.index.as_deref(), Some("docs"));
        assert_eq!(support.legs[0].endpoint.index.as_deref(), Some("support"));
        assert_eq!(docs.legs[0].endpoint.timeout_secs, Some(5));
        assert_eq!(support.legs[0].endpoint.timeout_secs, Some(15));
    }
}
