//! MCP (Model Context Protocol) server for stdio MCP clients (Claude Desktop, opencode)
//!
//! Provides an MCP server over stdio transport that exposes conproxy
//! functionality as tools: proxy search + dry-run tune suite (plan 09).
//!
//! Start with: `conproxy mcp`

use crate::config::{Config, ProxyScopeConfig, WeightedPhrase};
use crate::proxy::scope::ScopeFilter;
use crate::proxy::tune::{
    cache_tune, cascade_tune, compare_runs, embed_tune, evaluate, federated_tune, rate_limit_tune,
    scope_suggest, scope_tune, warm_tune, CacheAccessEvent, CacheTuneParams, CascadeLegProbe,
    CascadeTuneParams, CompareRequest, EmbedTuneParams, FedHit, FederatedTuneParams,
    RateLimitTuneParams, ScopeSuggestParams, ScopeTuneParams, SessionError, TuneBudget,
    TuneSessionStore, WarmTuneParams, DEFAULT_SESSION_TTL_SECS,
};
use crate::proxy::types::SearchResult;
use rmcp::{
    handler::server::router::tool::ToolRouter, handler::server::wrapper::Parameters, model::*,
    schemars, tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

mod status;

// ============================================================================
// Tool parameter types
// ============================================================================

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ProxySearchParams {
    /// The search query string
    pub query: String,
    /// Maximum number of results (default: 10)
    #[serde(default = "default_query_limit")]
    pub limit: usize,
}

fn default_query_limit() -> usize {
    10
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct BenchmarkToolParams {
    /// Session id from tune_session_open.
    pub session_id: String,
    /// Agent identity that owns the session.
    pub agent_id: String,
    /// Context id the session was opened against.
    pub context_id: String,
    /// The search query to benchmark.
    pub query: String,
    /// Number of results to compare (default: 10).
    #[serde(default = "default_query_limit")]
    pub top_k: usize,
    /// Optional explicit run id; defaults to session's selected or last run.
    #[serde(default)]
    pub run_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ApplyTuneParams {
    pub session_id: String,
    pub agent_id: String,
    pub context_id: String,
    /// Optional explicit config path; defaults to .conproxy/conproxy.toml.
    #[serde(default)]
    pub config_path: Option<String>,
    /// Whether to also POST /admin/reload after writing (default true).
    #[serde(default = "default_reload_flag")]
    pub reload: bool,
}

fn default_reload_flag() -> bool {
    true
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TuneSessionOpenParams {
    /// Agent or user identity owning the session
    pub agent_id: String,
    /// Context id (`contexts.<id>`) being tuned
    pub context_id: String,
    /// Optional existing session id to resume
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TuneSessionCloseParams {
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TuneSessionListParams {
    pub agent_id: String,
    #[serde(default)]
    pub context_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct McpHit {
    #[serde(default)]
    pub id: String,
    pub content: String,
    #[serde(default)]
    pub score: f32,
}

impl From<McpHit> for SearchResult {
    fn from(h: McpHit) -> Self {
        SearchResult {
            id: h.id,
            content: h.content,
            score: h.score,
            metadata: None,
            upstream_id: None,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct McpWeightedPhrase {
    pub text: String,
    #[serde(default = "default_weight")]
    pub weight: f32,
    #[serde(default)]
    pub min_similarity: Option<f32>,
}

fn default_weight() -> f32 {
    1.0
}

impl From<McpWeightedPhrase> for WeightedPhrase {
    fn from(p: McpWeightedPhrase) -> Self {
        WeightedPhrase {
            text: p.text,
            weight: p.weight,
            min_similarity: p.min_similarity,
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScopeTuneToolParams {
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub context_id: Option<String>,
    /// Hits to score (caller-supplied; dry-run does not fetch upstream)
    pub hits: Vec<McpHit>,
    #[serde(default)]
    pub weighted_phrases: Vec<McpWeightedPhrase>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub min_similarity: Option<f32>,
    #[serde(default)]
    pub min_similarity_sweep: Option<Vec<f32>>,
    #[serde(default)]
    pub scope_weight: Option<f32>,
    #[serde(default)]
    pub lexical_weight: Option<f32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScopeSuggestToolParams {
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub context_id: Option<String>,
    pub texts: Vec<String>,
    #[serde(default = "default_max_phrases")]
    pub max_phrases: usize,
}

fn default_max_phrases() -> usize {
    8
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CompareRunsParams {
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub context_id: Option<String>,
    pub run_id_a: String,
    pub run_id_b: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TuneExportParams {
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub context_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TuneSelectRunParams {
    pub session_id: String,
    pub run_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CacheAccessEventParam {
    pub key: String,
    #[serde(default)]
    pub age_secs: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CacheTuneToolParams {
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub context_id: Option<String>,
    pub events: Vec<CacheAccessEventParam>,
    #[serde(default)]
    pub fresh_ttl_secs: Option<Vec<u64>>,
    #[serde(default)]
    pub stale_ttl_secs: Option<Vec<u64>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CascadeLegParam {
    pub upstream_id: String,
    pub priority: u32,
    pub best_score: f32,
    pub result_count: usize,
    #[serde(default)]
    pub latency_ms: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CascadeTuneToolParams {
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub context_id: Option<String>,
    pub legs: Vec<CascadeLegParam>,
    #[serde(default)]
    pub min_score_threshold: Option<f32>,
    #[serde(default)]
    pub min_results: Option<usize>,
    #[serde(default)]
    pub max_cascade_depth: Option<usize>,
    #[serde(default)]
    pub min_score_sweep: Option<Vec<f32>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FedHitParam {
    pub id: String,
    pub score: f32,
    pub source: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FederatedTuneToolParams {
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub context_id: Option<String>,
    pub hits: Vec<FedHitParam>,
    #[serde(default)]
    pub local_weight_sweep: Option<Vec<f32>>,
    #[serde(default)]
    pub top_k: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EmbedTuneToolParams {
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub context_id: Option<String>,
    pub text_count: usize,
    #[serde(default)]
    pub batch_size_sweep: Option<Vec<usize>>,
    #[serde(default)]
    pub per_text_ms: Option<f64>,
    #[serde(default)]
    pub batch_overhead_ms: Option<f64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RateLimitTuneToolParams {
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub context_id: Option<String>,
    pub arrival_ms: Vec<u64>,
    #[serde(default)]
    pub rps_sweep: Option<Vec<f64>>,
    #[serde(default)]
    pub burst_sweep: Option<Vec<u32>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WarmTuneToolParams {
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub context_id: Option<String>,
    pub plan_keys: Vec<String>,
    #[serde(default)]
    pub cached_keys: Vec<String>,
    #[serde(default)]
    pub per_key_ms: Option<u64>,
    #[serde(default)]
    pub concurrency_sweep: Option<Vec<usize>>,
    #[serde(default)]
    pub execute: bool,
}

/// Composite tune workflow: open session → search via running proxy →
/// scope_tune on the results → (optional) apply + reload + close.
///
/// Default `apply=false` keeps the tool safe; the tune report is returned
/// alongside the artifact so the caller can review before persisting.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TuneWorkflowParams {
    pub agent_id: String,
    pub context_id: String,
    pub query: String,
    #[serde(default = "default_query_limit")]
    pub top_k: usize,
    #[serde(default)]
    pub weighted_phrases: Vec<McpWeightedPhrase>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub min_similarity: Option<f32>,
    #[serde(default)]
    pub min_similarity_sweep: Option<Vec<f32>>,
    #[serde(default)]
    pub scope_weight: Option<f32>,
    #[serde(default)]
    pub lexical_weight: Option<f32>,
    #[serde(default = "default_workflow_apply")]
    pub apply: bool,
    #[serde(default = "default_reload_flag")]
    pub reload: bool,
    #[serde(default)]
    pub config_path: Option<String>,
    #[serde(default = "default_workflow_close")]
    pub close_session: bool,
    /// Optional explicit session id to reuse; otherwise a new session is opened.
    #[serde(default)]
    pub session_id: Option<String>,
}

fn default_workflow_apply() -> bool {
    false
}
fn default_workflow_close() -> bool {
    true
}

// ============================================================================
// Response types (for JSON serialization)
// ============================================================================

#[derive(Debug, Serialize)]
struct ProxySearchResultItem {
    id: String,
    content: String,
    score: f32,
    cache_status: String,
}

// ============================================================================
// MCP Server
// ============================================================================

#[derive(Clone)]
pub struct ConproxyServer {
    config: Arc<Config>,
    tune_sessions: TuneSessionStore,
    #[allow(dead_code)] // populated by `#[tool_handler]` macro
    tool_router: ToolRouter<ConproxyServer>,
}

fn json_ok<T: Serialize>(value: &T) -> Result<CallToolResult, McpError> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| McpError::internal_error(format!("JSON serialization failed: {e}"), None))?;
    Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
}

fn tune_err(e: impl std::fmt::Display) -> McpError {
    McpError::invalid_params(e.to_string(), None)
}

/// Build a gRPC client for the configured proxy listen address, attaching
/// `proxy.api_key` from the MCP-side `Config` if it is set. Without this, a
/// proxy that has `proxy.api_key` (or any `[[agents]]`) would reject every
/// search from the MCP with `401 / API key required`.
fn build_proxy_client(
    config: &crate::config::Config,
) -> Result<crate::proxy::ProxyClient, McpError> {
    use crate::proxy::ClientConfig;

    let proxy_listen = config.config.proxy.listen().to_string();
    let grpc_url = format!("http://{proxy_listen}");

    let mut cfg = ClientConfig::new(&grpc_url);
    if let Some(k) = config.config.proxy.api_key() {
        if !k.is_empty() {
            cfg = cfg.with_api_key(k);
        }
    }
    crate::proxy::ProxyClient::new(cfg).map_err(|e| {
        McpError::internal_error(
            format!("Failed to create proxy client for {grpc_url}: {e}"),
            None,
        )
    })
}

#[tool_router]
impl ConproxyServer {
    pub fn new(config: Config) -> Self {
        Self {
            config: Arc::new(config),
            tune_sessions: TuneSessionStore::new(DEFAULT_SESSION_TTL_SECS),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Search via the cache proxy. Queries the configured upstream vector database through the proxy cache."
    )]
    async fn search(
        &self,
        Parameters(params): Parameters<ProxySearchParams>,
    ) -> Result<CallToolResult, McpError> {
        use crate::proxy::QueryRequest;

        let client = build_proxy_client(&self.config)?;

        let request = QueryRequest {
            query: params.query.clone(),
            top_k: Some(params.limit),
            priority: None,
            upstream_id: None,
            upstream_type: None,
        };

        let proxy_response = client.query(&request).await.map_err(|e| {
            let msg = e.to_string();
            let auth_hint = if msg.contains("API key required") || msg.contains("UNAUTHENTICATED") {
                " — proxy requires authentication; set proxy.api_key in the MCP-side config \
                 (or unset it on the running proxy if the deployment is local-only)"
            } else {
                ""
            };
            McpError::internal_error(format!("Proxy query failed: {e}{auth_hint}"), None)
        })?;

        let items: Vec<ProxySearchResultItem> = proxy_response
            .results
            .into_iter()
            .map(|r| ProxySearchResultItem {
                id: r.id,
                content: r.content,
                score: r.score,
                cache_status: format!("{:?}", proxy_response.cache_status),
            })
            .collect();

        json_ok(&items)
    }

    #[tool(
        description = "Benchmark a query against the session's tuned scope params. Live-query → re-score → diff → verdict (improved/degraded/unchanged)."
    )]
    async fn benchmark(
        &self,
        Parameters(params): Parameters<BenchmarkToolParams>,
    ) -> Result<CallToolResult, McpError> {
        use crate::proxy::QueryRequest;

        // 1. Look up session → get the selected run (or explicit run_id)
        let sess = self
            .tune_sessions
            .get(
                &params.session_id,
                Some(&params.agent_id),
                Some(&params.context_id),
            )
            .ok_or_else(|| tune_err("session not found"))?;

        let run = if let Some(ref rid) = params.run_id {
            sess.runs.iter().find(|r| r.run_id == *rid).cloned()
        } else {
            sess.runs
                .iter()
                .find(|r| r.selected)
                .cloned()
                .or_else(|| sess.runs.last().cloned())
        }
        .ok_or_else(|| tune_err("no run found in session (select one or provide run_id)"))?;

        // Extract tuned params from the run
        let weighted_phrases: Vec<WeightedPhrase> = run
            .params
            .get("weighted_phrases")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let mode = run
            .params
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("filter")
            .to_string();
        let min_similarity = run
            .params
            .get("min_similarity")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.25) as f32;
        let scope_weight = run
            .params
            .get("scope_weight")
            .and_then(|v| v.as_f64())
            .map(|v| v as f32);
        let lexical_weight = run
            .params
            .get("lexical_weight")
            .and_then(|v| v.as_f64())
            .map(|v| v as f32);

        // 2. Live query via ProxyClient
        let client = build_proxy_client(&self.config)?;

        let request = QueryRequest {
            query: params.query.clone(),
            top_k: Some(params.top_k),
            priority: None,
            upstream_id: None,
            upstream_type: None,
        };

        let proxy_response = client.query(&request).await.map_err(|e| {
            let msg = e.to_string();
            let auth_hint = if msg.contains("API key required") || msg.contains("UNAUTHENTICATED") {
                " — proxy requires authentication; set proxy.api_key in the MCP-side config \
                 (or unset it on the running proxy if the deployment is local-only)"
            } else {
                ""
            };
            McpError::internal_error(format!("Proxy query failed: {e}{auth_hint}"), None)
        })?;

        let baseline_hits: Vec<SearchResult> = proxy_response.results;

        // 3. Re-score baseline with tuned scope params
        let cfg = ProxyScopeConfig {
            weighted_phrases: weighted_phrases.clone(),
            seeds: Vec::new(),
            mode: Some(mode),
            min_seed_similarity: Some(min_similarity),
            seed_weight: scope_weight,
            query_prefix: None,
            lexical_weight,
            embed_band: None,
        };
        let filter = ScopeFilter::from_config(&cfg);
        let tuned_hits = filter.filter_results(baseline_hits.clone());

        // 4. Benchmark verdict
        let report = evaluate(
            &params.query,
            &baseline_hits,
            &tuned_hits,
            params.top_k,
            &weighted_phrases,
        );

        json_ok(&report)
    }

    #[tool(
        description = "Open a dry-run tune session bound to agent_id + context_id. Returns session_id for subsequent tune tools."
    )]
    async fn tune_session_open(
        &self,
        Parameters(params): Parameters<TuneSessionOpenParams>,
    ) -> Result<CallToolResult, McpError> {
        let sess = self
            .tune_sessions
            .open(params.agent_id, params.context_id, params.session_id)
            .map_err(tune_err)?;
        json_ok(&sess)
    }

    #[tool(
        description = "Close a tune session owned by agent_id (drops process-local state). On failure returns a `reason` field with the precise cause (unknown_session, agent_mismatch)."
    )]
    async fn tune_session_close(
        &self,
        Parameters(params): Parameters<TuneSessionCloseParams>,
    ) -> Result<CallToolResult, McpError> {
        match self
            .tune_sessions
            .close_with_reason(&params.session_id, params.agent_id.as_deref())
        {
            Ok(()) => json_ok(&serde_json::json!({
                "session_id": params.session_id,
                "closed": true,
                "reason": "ok",
            })),
            Err(SessionError::Unknown { session_id }) => json_ok(&serde_json::json!({
                "session_id": session_id,
                "closed": false,
                "reason": "unknown_session",
                "hint": "session may have expired (TTL) or never existed; check session_id",
            })),
            Err(SessionError::AgentMismatch {
                session_id,
                expected,
                got,
            }) => json_ok(&serde_json::json!({
                "session_id": session_id,
                "closed": false,
                "reason": "agent_mismatch",
                "expected_agent_id": expected,
                "got_agent_id": got,
                "hint": "another agent owns this session; either omit agent_id or use tune_session_list to find yours",
            })),
            Err(SessionError::ContextMismatch { .. }) => {
                // close_with_reason does not surface ContextMismatch today, but
                // keep the match exhaustive in case future variants land.
                json_ok(&serde_json::json!({
                    "closed": false,
                    "reason": "context_mismatch",
                }))
            }
        }
    }

    #[tool(description = "List open tune sessions for an agent (optional context filter).")]
    async fn tune_session_list(
        &self,
        Parameters(params): Parameters<TuneSessionListParams>,
    ) -> Result<CallToolResult, McpError> {
        let list = self
            .tune_sessions
            .list(&params.agent_id, params.context_id.as_deref());
        json_ok(&list)
    }

    #[tool(
        description = "Dry-run scope filter/boost/rerank on supplied hits. Optional min_similarity sweep. Score C: filter ignores phrase weight. Does not write config. hits[] must be non-empty; call search first (or use tune_workflow to do search + tune in one call)."
    )]
    async fn scope_tune(
        &self,
        Parameters(params): Parameters<ScopeTuneToolParams>,
    ) -> Result<CallToolResult, McpError> {
        let report = scope_tune(
            &self.tune_sessions,
            ScopeTuneParams {
                session_id: params.session_id,
                agent_id: params.agent_id,
                context_id: params.context_id,
                hits: params.hits.into_iter().map(Into::into).collect(),
                weighted_phrases: params
                    .weighted_phrases
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                mode: params.mode,
                min_similarity: params.min_similarity,
                min_similarity_sweep: params.min_similarity_sweep,
                scope_weight: params.scope_weight,
                lexical_weight: params.lexical_weight,
                budget: TuneBudget::default(),
            },
        )
        .map_err(tune_err)?;
        json_ok(&report)
    }

    #[tool(
        description = "Suggest weighted_phrases from hit/dropped texts (TF-IDF-ish). Dry-run; records run on session."
    )]
    async fn scope_suggest(
        &self,
        Parameters(params): Parameters<ScopeSuggestToolParams>,
    ) -> Result<CallToolResult, McpError> {
        let report = scope_suggest(
            &self.tune_sessions,
            ScopeSuggestParams {
                session_id: params.session_id,
                agent_id: params.agent_id,
                context_id: params.context_id,
                texts: params.texts,
                max_phrases: params.max_phrases,
                budget: TuneBudget::default(),
            },
        )
        .map_err(tune_err)?;
        json_ok(&report)
    }

    #[tool(description = "Diff two run_ids within the same tune session (params + metrics).")]
    async fn compare_runs(
        &self,
        Parameters(params): Parameters<CompareRunsParams>,
    ) -> Result<CallToolResult, McpError> {
        let report = compare_runs(
            &self.tune_sessions,
            CompareRequest {
                session_id: params.session_id,
                agent_id: params.agent_id,
                context_id: params.context_id,
                run_id_a: params.run_id_a,
                run_id_b: params.run_id_b,
            },
        )
        .map_err(tune_err)?;
        json_ok(&report)
    }

    #[tool(
        description = "Mark a run as selected for export (winning params). Dry-run; no config write."
    )]
    async fn tune_select_run(
        &self,
        Parameters(params): Parameters<TuneSelectRunParams>,
    ) -> Result<CallToolResult, McpError> {
        self.tune_sessions
            .select_run(
                &params.session_id,
                params.agent_id.as_deref(),
                &params.run_id,
            )
            .map_err(tune_err)?;
        json_ok(&serde_json::json!({
            "session_id": params.session_id,
            "run_id": params.run_id,
            "selected": true,
        }))
    }

    #[tool(
        description = "Export happy-path scope params as contexts.<id> TOML/JSON fragment. Dry-run; does not write files."
    )]
    async fn tune_export(
        &self,
        Parameters(params): Parameters<TuneExportParams>,
    ) -> Result<CallToolResult, McpError> {
        let art = self
            .tune_sessions
            .export(
                &params.session_id,
                params.agent_id.as_deref(),
                params.context_id.as_deref(),
            )
            .map_err(tune_err)?;
        json_ok(&art)
    }

    #[tool(
        description = "Dry-run cache TTL hit/stale/miss probe on synthetic access events. Does not purge cache."
    )]
    async fn cache_tune(
        &self,
        Parameters(params): Parameters<CacheTuneToolParams>,
    ) -> Result<CallToolResult, McpError> {
        let report = cache_tune(
            &self.tune_sessions,
            CacheTuneParams {
                session_id: params.session_id,
                agent_id: params.agent_id,
                context_id: params.context_id,
                events: params
                    .events
                    .into_iter()
                    .map(|e| CacheAccessEvent {
                        key: e.key,
                        age_secs: e.age_secs,
                    })
                    .collect(),
                fresh_ttl_secs: params.fresh_ttl_secs,
                stale_ttl_secs: params.stale_ttl_secs,
                budget: TuneBudget::default(),
            },
        )
        .map_err(tune_err)?;
        json_ok(&report)
    }

    #[tool(
        description = "Dry-run cascade leg selection under score/result thresholds. Caller supplies per-leg probes."
    )]
    async fn cascade_tune(
        &self,
        Parameters(params): Parameters<CascadeTuneToolParams>,
    ) -> Result<CallToolResult, McpError> {
        let report = cascade_tune(
            &self.tune_sessions,
            CascadeTuneParams {
                session_id: params.session_id,
                agent_id: params.agent_id,
                context_id: params.context_id,
                legs: params
                    .legs
                    .into_iter()
                    .map(|l| CascadeLegProbe {
                        upstream_id: l.upstream_id,
                        priority: l.priority,
                        best_score: l.best_score,
                        result_count: l.result_count,
                        latency_ms: l.latency_ms,
                    })
                    .collect(),
                min_score_threshold: params.min_score_threshold,
                min_results: params.min_results,
                max_cascade_depth: params.max_cascade_depth,
                min_score_sweep: params.min_score_sweep,
                budget: TuneBudget::default(),
            },
        )
        .map_err(tune_err)?;
        json_ok(&report)
    }

    #[tool(
        description = "Dry-run federated local/remote merge weight preview. Does not query upstream."
    )]
    async fn federated_tune(
        &self,
        Parameters(params): Parameters<FederatedTuneToolParams>,
    ) -> Result<CallToolResult, McpError> {
        let report = federated_tune(
            &self.tune_sessions,
            FederatedTuneParams {
                session_id: params.session_id,
                agent_id: params.agent_id,
                context_id: params.context_id,
                hits: params
                    .hits
                    .into_iter()
                    .map(|h| FedHit {
                        id: h.id,
                        score: h.score,
                        source: h.source,
                    })
                    .collect(),
                local_weight_sweep: params.local_weight_sweep,
                top_k: params.top_k,
                budget: TuneBudget::default(),
            },
        )
        .map_err(tune_err)?;
        json_ok(&report)
    }

    #[tool(
        description = "Estimate embed batch latency from text_count + batch size grid. Simulation only."
    )]
    async fn embed_tune(
        &self,
        Parameters(params): Parameters<EmbedTuneToolParams>,
    ) -> Result<CallToolResult, McpError> {
        let report = embed_tune(
            &self.tune_sessions,
            EmbedTuneParams {
                session_id: params.session_id,
                agent_id: params.agent_id,
                context_id: params.context_id,
                text_count: params.text_count,
                batch_size_sweep: params.batch_size_sweep,
                per_text_ms: params.per_text_ms,
                batch_overhead_ms: params.batch_overhead_ms,
                budget: TuneBudget::default(),
            },
        )
        .map_err(tune_err)?;
        json_ok(&report)
    }

    #[tool(
        description = "Simulate token-bucket allow/deny under RPS/burst grids. Does not throttle live traffic."
    )]
    async fn rate_limit_tune(
        &self,
        Parameters(params): Parameters<RateLimitTuneToolParams>,
    ) -> Result<CallToolResult, McpError> {
        let report = rate_limit_tune(
            &self.tune_sessions,
            RateLimitTuneParams {
                session_id: params.session_id,
                agent_id: params.agent_id,
                context_id: params.context_id,
                arrival_ms: params.arrival_ms,
                rps_sweep: params.rps_sweep,
                burst_sweep: params.burst_sweep,
                budget: TuneBudget::default(),
            },
        )
        .map_err(tune_err)?;
        json_ok(&report)
    }

    #[tool(
        description = "Warm plan ETA and cache-key overlap. execute=false default; execute=true rejected in v1."
    )]
    async fn warm_tune(
        &self,
        Parameters(params): Parameters<WarmTuneToolParams>,
    ) -> Result<CallToolResult, McpError> {
        let report = warm_tune(
            &self.tune_sessions,
            WarmTuneParams {
                session_id: params.session_id,
                agent_id: params.agent_id,
                context_id: params.context_id,
                plan_keys: params.plan_keys,
                cached_keys: params.cached_keys,
                per_key_ms: params.per_key_ms,
                concurrency_sweep: params.concurrency_sweep,
                execute: params.execute,
                budget: TuneBudget::default(),
            },
        )
        .map_err(tune_err)?;
        json_ok(&report)
    }

    // ========================================================================
    // Composite workflow (open → search → scope_tune → optional apply)
    // ========================================================================

    #[tool(
        description = "Composite tune workflow: open session → search via running proxy → scope_tune on the results → (optional) apply + reload + close. Default apply=false returns the dry-run report and export artifact. Use this instead of chaining open/search/scope_tune manually to avoid client-side arg merging bugs."
    )]
    async fn tune_workflow(
        &self,
        Parameters(params): Parameters<TuneWorkflowParams>,
    ) -> Result<CallToolResult, McpError> {
        use crate::proxy::tune::apply_tune_export;
        use crate::proxy::QueryRequest;
        use std::path::PathBuf;

        // 1. Open (or reuse) session
        let sess = self
            .tune_sessions
            .open(
                params.agent_id.clone(),
                params.context_id.clone(),
                params.session_id.clone(),
            )
            .map_err(tune_err)?;
        let session_id = sess.session_id.clone();

        // 2. Live search through the proxy
        let client = build_proxy_client(&self.config)?;
        let request = QueryRequest {
            query: params.query.clone(),
            top_k: Some(params.top_k),
            priority: None,
            upstream_id: None,
            upstream_type: None,
        };
        let proxy_response = client.query(&request).await.map_err(|e| {
            let msg = e.to_string();
            let auth_hint = if msg.contains("API key required") || msg.contains("UNAUTHENTICATED") {
                " — proxy requires authentication; set proxy.api_key in the MCP-side config \
                 (or unset it on the running proxy if the deployment is local-only)"
            } else {
                ""
            };
            McpError::internal_error(
                format!(
                    "Workflow search failed: {e}{auth_hint}. \
                     Is conproxy running on the configured listen address?"
                ),
                None,
            )
        })?;
        let search_hits: Vec<SearchResult> = proxy_response.results.clone();
        let hit_count = search_hits.len();

        // 3. scope_tune (only if we have hits)
        let tune_report = if search_hits.is_empty() {
            // Surface this clearly rather than dumping "hits must not be empty" mid-flow.
            return Err(McpError::invalid_params(
                format!(
                    "tune_workflow: search returned 0 hits for query {:?} on context {:?}. \
                     Check the corpus is seeded (`make dev-up` runs corpus_seed), the context_id is \
                     a known context, and the query matches what the upstream backend indexed.",
                    params.query, params.context_id
                ),
                None,
            ));
        } else {
            scope_tune(
                &self.tune_sessions,
                ScopeTuneParams {
                    session_id: session_id.clone(),
                    agent_id: Some(params.agent_id.clone()),
                    context_id: Some(params.context_id.clone()),
                    hits: search_hits.clone(),
                    weighted_phrases: params
                        .weighted_phrases
                        .iter()
                        .map(|p| WeightedPhrase {
                            text: p.text.clone(),
                            weight: p.weight,
                            min_similarity: p.min_similarity,
                        })
                        .collect(),
                    mode: params.mode.clone(),
                    min_similarity: params.min_similarity,
                    min_similarity_sweep: params.min_similarity_sweep.clone(),
                    scope_weight: params.scope_weight,
                    lexical_weight: params.lexical_weight,
                    budget: TuneBudget::default(),
                },
            )
            .map_err(tune_err)?
        };

        // 4. Optional apply + reload
        let mut apply_payload: Option<serde_json::Value> = None;
        if params.apply {
            let path = params.config_path.as_deref().map(PathBuf::from);
            let report = apply_tune_export(
                &self.tune_sessions,
                &session_id,
                Some(&params.agent_id),
                Some(&params.context_id),
                path.as_deref(),
            )
            .map_err(tune_err)?;

            let mut payload = json!({
                "applied": true,
                "config_path": report.config_path.to_string_lossy(),
                "context_id": report.context_id,
                "source_run_id": report.source_run_id,
                "context_created": report.context_created,
                "toml_applied": report.toml_applied,
            });

            if params.reload {
                match status::http_post_json(&self.config, "/admin/reload", json!({})).await {
                    Ok(reload) => {
                        if let Value::Object(ref mut map) = payload {
                            map.insert("reload".into(), reload);
                        }
                    }
                    Err(e) => {
                        if let Value::Object(ref mut map) = payload {
                            map.insert("reload_error".into(), json!(e.to_string()));
                        }
                    }
                }
            }

            apply_payload = Some(payload);
        }

        // 5. Optional close
        let mut close_payload: Option<serde_json::Value> = None;
        if params.close_session {
            match self
                .tune_sessions
                .close_with_reason(&session_id, Some(&params.agent_id))
            {
                Ok(()) => {
                    close_payload = Some(json!({"closed": true, "reason": "ok"}));
                }
                Err(e) => {
                    close_payload = Some(json!({"closed": false, "reason": e.to_string()}));
                }
            }
        }

        json_ok(&json!({
            "session_id": session_id,
            "search": {
                "query": params.query,
                "top_k": params.top_k,
                "hit_count": hit_count,
            },
            "tune": tune_report,
            "apply": apply_payload,
            "close": close_payload,
        }))
    }

    // ========================================================================
    // Dashboard-parity status tools (one per panel)
    // ========================================================================

    #[tool(
        description = "Health probe (status, uptime, pool health, error rate). Mirrors dashboard status dot."
    )]
    async fn health(&self) -> Result<CallToolResult, McpError> {
        let data = status::panel_health(&self.config).await?;
        json_ok(&data)
    }

    #[tool(
        description = "Overview panel: total requests, hits/misses, hit rate, error rate, uptime, circuit state."
    )]
    async fn overview(&self) -> Result<CallToolResult, McpError> {
        let data = status::panel_overview(&self.config).await?;
        json_ok(&data)
    }

    #[tool(
        description = "Cache panel: size, fresh/stale/expired, upstreams table, integrity counters."
    )]
    async fn cache_status(&self) -> Result<CallToolResult, McpError> {
        let data = status::panel_cache(&self.config).await?;
        json_ok(&data)
    }

    #[tool(
        description = "Connection pool panel: upstreams, health, requests, failures, type counts, strategy."
    )]
    async fn pool_status(&self) -> Result<CallToolResult, McpError> {
        let data = status::panel_pool(&self.config).await?;
        json_ok(&data)
    }

    #[tool(description = "Circuit breaker + request queue state.")]
    async fn circuit_status(&self) -> Result<CallToolResult, McpError> {
        let data = status::panel_circuit(&self.config).await?;
        json_ok(&data)
    }

    #[tool(description = "Metrics panel: counters, latency percentiles, query stats, hot queries.")]
    async fn metrics_status(&self) -> Result<CallToolResult, McpError> {
        let data = status::panel_metrics(&self.config).await?;
        json_ok(&data)
    }

    #[tool(description = "Contexts panel: list of contexts + current context metadata/stats.")]
    async fn contexts_status(&self) -> Result<CallToolResult, McpError> {
        let data = status::panel_contexts(&self.config).await?;
        json_ok(&data)
    }

    #[tool(description = "Peer mesh status (enabled, connected peers).")]
    async fn peer_status(&self) -> Result<CallToolResult, McpError> {
        let data = status::panel_peer(&self.config).await?;
        json_ok(&data)
    }

    #[tool(
        description = "Tokio runtime snapshot: alive tasks, workers, global queue depth, busy time."
    )]
    async fn tokio_status(&self) -> Result<CallToolResult, McpError> {
        let data = status::panel_tokio(&self.config).await?;
        json_ok(&data)
    }

    #[tool(
        description = "List cached query entries with metadata (hash, query text, upstream, context, freshness, result count). Mirrors the cache panel."
    )]
    async fn cache_entries(&self) -> Result<CallToolResult, McpError> {
        let data = status::panel_cache_entries(&self.config).await?;
        json_ok(&data)
    }

    // ========================================================================
    // Apply & reload (local MCP mode)
    // ========================================================================

    #[tool(
        description = "Apply the session's selected run scope params to local config. Optionally reloads the proxy. Writes contexts.<id>.scope to disk."
    )]
    async fn apply_tune(
        &self,
        Parameters(params): Parameters<ApplyTuneParams>,
    ) -> Result<CallToolResult, McpError> {
        use std::path::PathBuf;

        let path = params.config_path.as_deref().map(PathBuf::from);
        let report = crate::proxy::tune::apply_tune_export(
            &self.tune_sessions,
            &params.session_id,
            Some(&params.agent_id),
            Some(&params.context_id),
            path.as_deref(),
        )
        .map_err(tune_err)?;

        let mut payload = json!({
            "applied": true,
            "config_path": report.config_path.to_string_lossy(),
            "context_id": report.context_id,
            "source_run_id": report.source_run_id,
            "context_created": report.context_created,
            "toml_applied": report.toml_applied,
        });

        if params.reload {
            match status::http_post_json(&self.config, "/admin/reload", json!({})).await {
                Ok(reload) => {
                    if let Value::Object(ref mut map) = payload {
                        map.insert("reload".to_string(), reload);
                    }
                }
                Err(e) => {
                    if let Value::Object(ref mut map) = payload {
                        map.insert("reload_error".to_string(), json!(e.to_string()));
                    }
                }
            }
        }

        json_ok(&payload)
    }

    #[tool(
        description = "POST /admin/reload on the running proxy. Triggers full hot-reload of contexts, cache, pool, circuit, agents."
    )]
    async fn reload(&self) -> Result<CallToolResult, McpError> {
        let data = status::http_post_json(&self.config, "/admin/reload", json!({})).await?;
        json_ok(&data)
    }
}

#[allow(clippy::let_and_return)] // retain intermediate for review
#[tool_handler]
impl ServerHandler for ConproxyServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_instructions(
                "Conproxy MCP server. Tools: search; full dry-run tune suite — \
                 session open/close/list, scope_tune/suggest, compare_runs, select_run, export, \
                 cache/cascade/federated/embed/rate_limit/warm_tune. Export pastes into contexts.<id>.\n\
                 \n\
                 Concurrent tool calls: this server supports parallel tool calls. Independent status \
                 panels (health / overview / cache_status / pool_status / circuit_status / \
                 metrics_status / contexts_status / peer_status / tokio_status / cache_entries) and \
                 multiple search calls are safe to fan out.\n\
                 \n\
                 Sequence required: any tune tool that depends on a session_id (scope_tune, \
                 scope_suggest, cache_tune, cascade_tune, federated_tune, embed_tune, rate_limit_tune, \
                 warm_tune, compare_runs, select_run, export, apply_tune) needs the session opened \
                 first, and scope_tune/federated_tune need hits from a prior search. Either \
                 chain them in order, or use the composite tool tune_workflow which does \
                 open + search + scope_tune (+ optional apply/reload/close) in one call.\n\
                 \n\
                 Tool arguments: never merge fields across tool calls. Each tool call must carry a \
                 complete, self-contained JSON arguments object. If a client is batching multiple \
                 tools into a single tool-use block, ensure every tool has its own arguments object; \
                 truncated or merged args are a client bug, not a server failure.",
            )
    }
}

/// Start the MCP server on stdio transport.
///
/// Blocks until the client disconnects. All logging goes to stderr.
///
/// # Errors
///
/// Returns an error if the MCP server fails to construct (config
/// validation), fails to bind the stdio transport, or the server
/// terminates abnormally while waiting on the client.
#[cfg(not(tarpaulin_include))]
pub async fn run_server(config: Config) -> anyhow::Result<()> {
    use rmcp::transport::stdio;
    use rmcp::ServiceExt;

    let server = ConproxyServer::new(config).serve(stdio()).await?;

    server.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests;
