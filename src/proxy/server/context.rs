//! Context management HTTP handlers.
//!
//! Extracted from the main server module. Handles:
//! - `GET /contexts` - List all available contexts
//! - `GET /contexts/current` - Get current context metadata and stats
//! - `POST /contexts/switch` - Switch to a different context
//! - `POST /contexts/create` - Create a new context
//! - `GET /contexts/:id/stats` - Per-context cache statistics

use super::*;

// ============================================================================
// Context Management Structs
// ============================================================================

/// Response for context list endpoint.
#[derive(Serialize)]
pub(super) struct ContextListResponse {
    pub(super) contexts: Vec<ContextMetadata>,
    pub(super) current: String,
}

/// Response for context switch endpoint.
#[derive(Serialize)]
pub(super) struct ContextSwitchResponse {
    pub(super) success: bool,
    pub(super) current: String,
    pub(super) message: String,
}

/// Request for context switch.
#[derive(serde::Deserialize)]
pub(super) struct ContextSwitchRequest {
    context_id: String,
}

/// Request for context creation.
#[derive(serde::Deserialize)]
pub(super) struct ContextCreateRequest {
    id: String,
    #[serde(default)]
    upstream_url: String,
    #[serde(default)]
    collection: String,
}

// ============================================================================
// Context Management Handlers
// ============================================================================

/// Handle GET /contexts requests.
///
/// Returns list of all available contexts.
pub(super) async fn handle_contexts_list(State(state): State<AppState>) -> impl IntoResponse {
    let contexts = state.context_manager.list_metadata();
    let current = state.context_manager.current();

    (
        StatusCode::OK,
        Json(ContextListResponse { contexts, current }),
    )
}

/// Handle GET /contexts/current requests.
///
/// Returns the current context metadata.
pub(super) async fn handle_context_current(State(state): State<AppState>) -> impl IntoResponse {
    match state.context_manager.get_current() {
        Some(meta) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "context": meta,
                "stats": state.context_manager.stats(&meta.id),
            })),
        ),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "No current context"
            })),
        ),
    }
}

/// Handle POST /contexts/switch requests.
///
/// Switches to a different context.
pub(super) async fn handle_context_switch(
    State(state): State<AppState>,
    Json(request): Json<ContextSwitchRequest>,
) -> impl IntoResponse {
    match state.context_manager.switch(&request.context_id) {
        Ok(()) => {
            let current = state.context_manager.current();

            (
                StatusCode::OK,
                Json(ContextSwitchResponse {
                    success: true,
                    current,
                    message: format!("Switched to context '{}'", request.context_id),
                }),
            )
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ContextSwitchResponse {
                success: false,
                current: state.context_manager.current(),
                message: e.to_string(),
            }),
        ),
    }
}

/// Handle POST /contexts/create requests.
///
/// Creates a new context.
pub(super) async fn handle_context_create(
    State(state): State<AppState>,
    Json(request): Json<ContextCreateRequest>,
) -> impl IntoResponse {
    match state
        .context_manager
        .create(&request.id, &request.upstream_url, &request.collection)
    {
        Ok(()) => {
            let meta = state.context_manager.get(&request.id);
            (
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "success": true,
                    "context": meta,
                    "message": format!("Created context '{}'", request.id),
                })),
            )
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "success": false,
                "error": e.to_string(),
            })),
        ),
    }
}

/// Handle GET /contexts/:id/stats requests.
///
/// Returns per-context cache statistics.
pub(super) async fn handle_context_stats(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.context_manager.get_context_stats(&id) {
        Some((snap, hit_rate)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "context": id,
                "hits": snap.hits,
                "misses": snap.misses,
                "queries": snap.queries,
                "hit_rate": hit_rate,
            })),
        ),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("Context '{}' not found", id),
            })),
        ),
    }
}
