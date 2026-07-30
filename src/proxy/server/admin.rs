//! Admin handler functions for the cache proxy server.
//!
//! Extracted from `mod.rs` — handles `/admin/*` endpoints:
//! - `POST /admin/reload`
//! - `POST /admin/pause`
//! - `POST /admin/resume`
//! - `POST /admin/metrics/reset`

use super::*;

/// Response for config hot-reload.
#[derive(Debug, Serialize)]
pub(super) struct ReloadResponse {
    pub(super) success: bool,
    pub(super) reloaded: Vec<String>,
    /// Sections parsed and validated but requiring a process restart to take
    /// effect. Always present (empty when nothing needs restart).
    pub(super) restart_required: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<String>,
    pub(super) message: String,
}

pub(super) async fn handle_admin_reload(State(state): State<AppState>) -> impl IntoResponse {
    match state.apply_reload() {
        Ok(summary) => {
            let count = summary.sections.len();
            let msg = if summary.restart_required.is_empty() {
                format!(
                    "Reloaded {} section(s): {}",
                    count,
                    summary.sections.join(", ")
                )
            } else {
                format!(
                    "Reloaded {} section(s): {}. Restart required for: {}",
                    count,
                    summary.sections.join(", "),
                    summary.restart_required.join("; ")
                )
            };
            (
                StatusCode::OK,
                Json(ReloadResponse {
                    success: true,
                    reloaded: summary.sections,
                    restart_required: summary.restart_required,
                    error: None,
                    message: msg,
                }),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ReloadResponse {
                success: false,
                reloaded: vec![],
                restart_required: vec![],
                error: Some(e.clone()),
                message: format!("Config reload failed: {}", e),
            }),
        ),
    }
}

/// Pause the proxy, rejecting new queries while in-flight requests drain.
pub(super) async fn handle_admin_pause(State(state): State<AppState>) -> impl IntoResponse {
    state
        .paused
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let active = state.client_tracker.active_count();
    Json(serde_json::json!({
        "status": "paused",
        "active_requests": active,
        "message": "Proxy is paused. New queries will be rejected. Use POST /admin/resume to resume."
    }))
}

/// Resume the proxy, accepting queries again.
pub(super) async fn handle_admin_resume(State(state): State<AppState>) -> impl IntoResponse {
    state
        .paused
        .store(false, std::sync::atomic::Ordering::SeqCst);
    Json(serde_json::json!({
        "status": "resumed",
        "message": "Proxy is accepting queries again."
    }))
}

/// Reset all metrics counters to zero.
pub(super) async fn handle_admin_metrics_reset(State(state): State<AppState>) -> impl IntoResponse {
    state.metrics.reset();
    state.context_manager.reset_all_stats();
    Json(serde_json::json!({
        "status": "reset",
        "message": "All metrics counters reset to zero."
    }))
}

// ============================================================================
// Agent Management
// ============================================================================

/// Response for agent list.
#[derive(Debug, Serialize)]
pub(super) struct AgentListResponse {
    pub(super) agents: Vec<super::super::agent::AgentInfo>,
    pub(super) total: usize,
}

/// Handle GET /admin/agents - List all agents.
pub(super) async fn handle_admin_agents_list(State(state): State<AppState>) -> impl IntoResponse {
    let reg = state.agent_registry.load_full();
    match reg.as_deref() {
        Some(registry) => {
            let agents = registry.list_agents();
            let total = agents.len();
            (StatusCode::OK, Json(AgentListResponse { agents, total }))
        }
        None => (
            StatusCode::OK,
            Json(AgentListResponse {
                agents: vec![],
                total: 0,
            }),
        ),
    }
}

/// Request body for creating an agent.
#[derive(Debug, serde::Deserialize)]
pub(super) struct CreateAgentRequest {
    pub id: String,
    pub api_key: String,
    #[serde(default)]
    pub allowed_contexts: Vec<String>,
    pub priority_class: Option<u32>,
    pub rate_limit_rps: Option<u32>,
}

/// Handle POST /admin/agents - Create a new agent.
pub(super) async fn handle_admin_agents_create(
    State(state): State<AppState>,
    Json(body): Json<CreateAgentRequest>,
) -> impl IntoResponse {
    let reg = state.agent_registry.load_full();
    let Some(ref registry) = reg else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Agent registry not configured"
            })),
        );
    };

    let config = crate::config::AgentConfig {
        id: body.id.clone(),
        api_key: body.api_key,
        default_context: None,
        allowed_contexts: body.allowed_contexts,
        priority_class: body.priority_class,
        rate_limit_rps: body.rate_limit_rps,
        enabled: true,
    };

    registry.register(&config);
    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "status": "created",
            "agent_id": body.id,
        })),
    )
}

/// Handle DELETE /admin/agents/:id - Remove an agent.
pub(super) async fn handle_admin_agents_delete(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
) -> impl IntoResponse {
    let reg = state.agent_registry.load_full();
    let Some(ref registry) = reg else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Agent registry not configured"
            })),
        );
    };

    if registry.remove_by_id(&agent_id) {
        (
            StatusCode::OK,
            Json(serde_json::json!({
                "status": "deleted",
                "agent_id": agent_id,
            })),
        )
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "Agent not found",
                "agent_id": agent_id,
            })),
        )
    }
}

/// Request body for key rotation.
#[derive(Debug, serde::Deserialize)]
pub(super) struct RotateKeyRequest {
    pub new_api_key: String,
}

/// Handle POST /admin/agents/:id/rotate-key - Rotate an agent's API key.
pub(super) async fn handle_admin_agents_rotate_key(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    Json(body): Json<RotateKeyRequest>,
) -> impl IntoResponse {
    let reg = state.agent_registry.load_full();
    let Some(ref registry) = reg else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Agent registry not configured"
            })),
        );
    };

    if registry.rotate_key(&agent_id, body.new_api_key) {
        (
            StatusCode::OK,
            Json(serde_json::json!({
                "status": "rotated",
                "agent_id": agent_id,
            })),
        )
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "Agent not found",
                "agent_id": agent_id,
            })),
        )
    }
}
// touched Tue Jul 21 12:28:54 AM CDT 2026
