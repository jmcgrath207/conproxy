//! Shared HTTP client + dashboard-panel MCP status tools.
//!
//! Each `conproxy_*_status` tool mirrors a dashboard panel, fetching the same
//! JSON endpoints from `src/proxy/middleware.rs::WEB_UI_ALLOWLIST` (+ extras:
//! `/health`, `/pool`, `/peer/status`, `/debug/tokio`).
//!
//! Auth: optional `x-api-key` header when `proxy.api_key` is set.

use crate::config::Config;
use rmcp::model::ErrorData as McpError;
use serde_json::Value;

// ---------------------------------------------------------------------------
// HTTP helper (blocking reqwest inside spawn_blocking — avoids async connector
// init that fails on single-thread runtimes / slim containers)
// ---------------------------------------------------------------------------

/// Resolve the base HTTP URL the proxy listens on for status endpoints.
pub(crate) fn status_base_url(config: &Config) -> String {
    format!("http://{}", config.config.proxy.http_listen_addr())
}

/// GET a JSON path from the proxy's status endpoints.
/// `path` may start with `/` or omit it.
pub(crate) async fn http_get_json(config: &Config, path: &str) -> Result<Value, McpError> {
    let base = status_base_url(config);
    let trimmed = path.trim_start_matches('/').to_string();
    let url = format!("{base}/{trimmed}");
    let api_key = config.config.proxy.api_key.clone();
    let path_buf = path.to_string();

    let result = tokio::task::spawn_blocking(move || do_get(&url, api_key.as_deref()))
        .await
        .map_err(|e| McpError::internal_error(format!("spawn_blocking: {e:?}"), None))?;

    result.map_err(|e| McpError::internal_error(format!("GET {path_buf} failed: {e:?}"), None))
}

/// POST a JSON path with a JSON body, return parsed JSON.
pub(crate) async fn http_post_json(
    config: &Config,
    path: &str,
    body: Value,
) -> Result<Value, McpError> {
    let base = status_base_url(config);
    let trimmed = path.trim_start_matches('/').to_string();
    let url = format!("{base}/{trimmed}");
    let api_key = config.config.proxy.api_key.clone();
    let path_buf = path.to_string();

    let result = tokio::task::spawn_blocking(move || do_post(&url, api_key.as_deref(), &body))
        .await
        .map_err(|e| McpError::internal_error(format!("spawn_blocking: {e:?}"), None))?;

    result.map_err(|e| McpError::internal_error(format!("POST {path_buf} failed: {e:?}"), None))
}

fn do_get(url: &str, api_key: Option<&str>) -> Result<Value, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("build client: {e:?}"))?;
    let mut req = client.get(url).header("Accept", "application/json");
    if let Some(k) = api_key {
        req = req.header("x-api-key", k);
    }
    let resp = req.send().map_err(|e| format!("send: {e:?}"))?;
    let status = resp.status();
    let text = resp.text().map_err(|e| format!("read body: {e:?}"))?;
    if !status.is_success() {
        return Err(format!("status {}: {}", status, text));
    }
    serde_json::from_str(&text)
        .map_err(|e| format!("parse json: {e:?} (body: {})", &text[..text.len().min(200)]))
}

fn do_post(url: &str, api_key: Option<&str>, body: &Value) -> Result<Value, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("build client: {e:?}"))?;
    let mut req = client
        .post(url)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json");
    if let Some(k) = api_key {
        req = req.header("x-api-key", k);
    }
    let resp = req.json(body).send().map_err(|e| format!("send: {e:?}"))?;
    let status = resp.status();
    let text = resp.text().map_err(|e| format!("read body: {e:?}"))?;
    if !status.is_success() {
        return Err(format!("status {}: {}", status, text));
    }
    serde_json::from_str(&text).map_err(|e| format!("parse json: {e:?}"))
}

// ---------------------------------------------------------------------------
// Panel assemblers — each fetches one dashboard panel's worth of data.
// ---------------------------------------------------------------------------

/// Overview = metrics + stats + circuit.
pub(crate) async fn panel_overview(config: &Config) -> Result<Value, McpError> {
    let metrics = http_get_json(config, "/metrics").await?;
    let stats = http_get_json(config, "/stats").await?;
    let circuit = http_get_json(config, "/circuit").await?;
    Ok(serde_json::json!({
        "metrics": metrics,
        "stats": stats,
        "circuit": circuit,
    }))
}

/// Cache = stats + pool + cache/integrity.
pub(crate) async fn panel_cache(config: &Config) -> Result<Value, McpError> {
    let stats = http_get_json(config, "/stats").await?;
    let pool = http_get_json(config, "/pool").await?;
    let integrity = http_get_json(config, "/cache/integrity").await?;
    Ok(serde_json::json!({
        "stats": stats,
        "pool": pool,
        "integrity": integrity,
    }))
}

/// Pool = /pool.
pub(crate) async fn panel_pool(config: &Config) -> Result<Value, McpError> {
    http_get_json(config, "/pool").await
}

/// Circuit/Queue = circuit + queue.
pub(crate) async fn panel_circuit(config: &Config) -> Result<Value, McpError> {
    let circuit = http_get_json(config, "/circuit").await?;
    let queue = http_get_json(config, "/queue").await?;
    Ok(serde_json::json!({
        "circuit": circuit,
        "queue": queue,
    }))
}

/// Metrics = metrics + pool + stats/queries.
pub(crate) async fn panel_metrics(config: &Config) -> Result<Value, McpError> {
    let metrics = http_get_json(config, "/metrics").await?;
    let pool = http_get_json(config, "/pool").await?;
    let queries = http_get_json(config, "/stats/queries").await?;
    Ok(serde_json::json!({
        "metrics": metrics,
        "pool": pool,
        "queries": queries,
    }))
}

/// Contexts = /contexts + /contexts/current.
pub(crate) async fn panel_contexts(config: &Config) -> Result<Value, McpError> {
    let current = http_get_json(config, "/contexts/current").await?;
    let list = http_get_json(config, "/contexts").await?;
    Ok(serde_json::json!({
        "current": current,
        "contexts": list,
    }))
}

/// Peer = /peer/status.
pub(crate) async fn panel_peer(config: &Config) -> Result<Value, McpError> {
    http_get_json(config, "/peer/status").await
}

/// Tokio = /debug/tokio.
pub(crate) async fn panel_tokio(config: &Config) -> Result<Value, McpError> {
    http_get_json(config, "/debug/tokio").await
}

/// Health = /health.
pub(crate) async fn panel_health(config: &Config) -> Result<Value, McpError> {
    http_get_json(config, "/health").await
}

/// Cache entries = /cache/entries (list of cached query keys + metadata).
pub(crate) async fn panel_cache_entries(config: &Config) -> Result<Value, McpError> {
    http_get_json(config, "/cache/entries").await
}
