//! Query handler for the cache proxy.
//!
//! Contains `handle_query` (POST /query) and the QueryMode-aware
//! upstream routing helper `query_with_mode`.

use super::*;

/// Execute a query against an upstream with QueryMode-aware routing.
///
/// For TextNative upstreams, forwards the text query directly.
/// For VectorOnly upstreams (with proxy-embed feature), embeds the query locally
/// and forwards the vector.
#[cfg(feature = "embed-api")]
pub(crate) async fn query_with_mode(
    upstream: &GenericRestAdapter,
    request: &QueryRequest,
    smart_embedder: &Option<Arc<SmartEmbedder>>,
) -> Result<QueryResponse, super::super::upstream::UpstreamError> {
    let mode = upstream.query_mode();

    match mode {
        QueryMode::TextNative => {
            // Upstream handles embedding internally
            upstream.query(request).await
        }
        QueryMode::VectorOnly => {
            // We need to embed locally
            let embedder = smart_embedder.as_ref().ok_or_else(|| {
                super::super::upstream::UpstreamError::EmbeddingRequired(
                    "VectorOnly upstream requires proxy-embed feature with embedder configured"
                        .to_string(),
                )
            })?;

            // Embed the query text
            let vector = embedder.embed(&request.query).await.map_err(
                |e: crate::error::ConproxyError| {
                    super::super::upstream::UpstreamError::EmbeddingFailed(e.to_string())
                },
            )?;

            // Send vector query to upstream
            upstream.query_vector(request, &vector).await
        }
        QueryMode::Unknown => {
            // Try text first, then discover mode if it fails
            match upstream.query(request).await {
                Ok(response) => {
                    // Text worked - cache this discovery
                    upstream.set_query_mode(QueryMode::TextNative);
                    Ok(response)
                }
                Err(super::super::upstream::UpstreamError::UnsupportedQueryType(_)) => {
                    // Text not supported - try to discover and switch to vector
                    if let Ok(discovered) = upstream.discover_query_mode().await {
                        upstream.set_query_mode(discovered);
                        if discovered == QueryMode::VectorOnly {
                            // Call with updated mode (non-recursive since mode is now known)
                            let embedder = smart_embedder.as_ref().ok_or_else(|| {
                                super::super::upstream::UpstreamError::EmbeddingRequired(
                                    "VectorOnly upstream requires embedding support".to_string(),
                                )
                            })?;
                            let vector = embedder.embed(&request.query).await.map_err(
                                |e: crate::error::ConproxyError| {
                                    super::super::upstream::UpstreamError::EmbeddingFailed(
                                        e.to_string(),
                                    )
                                },
                            )?;
                            return upstream.query_vector(request, &vector).await;
                        }
                    }
                    Err(super::super::upstream::UpstreamError::UnsupportedQueryType(
                        "Could not determine upstream query mode".to_string(),
                    ))
                }
                Err(e) => Err(e),
            }
        }
    }
}

/// Handle POST /query requests.
///
/// Thin HTTP wrapper that delegates to `query_core::execute_query`.
#[instrument(skip(state, headers, agent_identity), fields(query_len = request.query.len(), top_k = request.top_k))]
pub(super) async fn handle_query(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    agent_identity: Option<axum::Extension<AgentIdentity>>,
    Json(request): Json<QueryRequest>,
) -> impl IntoResponse {
    let request_id = extract_request_id(&headers);
    let agent_id = agent_identity.as_ref().map(|a| a.id.clone());
    debug!(request_id = %request_id, agent = ?agent_id, "Processing query request");

    let ctx_id = resolve_context(&headers, agent_identity.as_deref());
    let source = agent_id.unwrap_or_else(|| {
        headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("local")
            .to_string()
    });

    let result = super::query_core::execute_query(
        &state,
        request,
        ctx_id,
        request_id,
        agent_identity.as_deref(),
        source,
    )
    .await;

    let status_code = match result.status {
        200 => StatusCode::OK,
        400 => StatusCode::BAD_REQUEST,
        403 => StatusCode::FORBIDDEN,
        502 => StatusCode::BAD_GATEWAY,
        503 => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };

    (status_code, Json(result.response))
}
