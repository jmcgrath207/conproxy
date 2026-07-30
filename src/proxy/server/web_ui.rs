//! Embedded web UI — serves static assets at `/dashboard`.
//!
//! Files under `ui/` are compiled into the binary via `rust-embed`.
//! SPA fallback: any `/dashboard/*` path not matching a static asset
//! returns `index.html`.

use axum::{
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};

/// Embedded UI assets.
#[derive(rust_embed::Embed)]
#[folder = "ui/"]
#[prefix = "dashboard/"]
struct Assets;

/// Serve embedded UI assets with SPA fallback.
///
/// GET `/dashboard` returns `index.html`.
/// GET `/dashboard/{file}` returns the file if it exists,
/// otherwise falls back to `index.html` (SPA history mode).
pub async fn handle_dashboard(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    // Try exact match first
    if let Some(content) = Assets::get(path) {
        return serve_asset(content);
    }

    // Try with .html extension
    if !path.ends_with(".html") {
        let with_html = format!("{path}.html");
        if let Some(content) = Assets::get(&with_html) {
            return serve_asset(content);
        }
    }

    // SPA fallback — serve index.html
    if let Some(content) = Assets::get("dashboard/index.html") {
        return serve_asset(content);
    }

    StatusCode::NOT_FOUND.into_response()
}

fn serve_asset(content: rust_embed::EmbeddedFile) -> Response {
    let mime: header::HeaderValue = content
        .metadata
        .mimetype()
        .parse()
        .unwrap_or_else(|_| header::HeaderValue::from_static("application/octet-stream"));

    ([(header::CONTENT_TYPE, mime)], content.data.to_vec()).into_response()
}
