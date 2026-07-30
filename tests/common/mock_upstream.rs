//! Lightweight mock upstream HTTP server for tests and load benchmarks.
//!
//! Uses `axum` (already a dep) + `tokio::net::TcpListener` to bind to an
//! ephemeral port and serve canned responses. No new crate dependencies.
//!
//! Use cases:
//! - Integration tests that need a stub upstream without Docker
//! - `tests/e2e_load` PureCacheBench (deterministic upstream, eliminates
//!   network variance from latency measurement)
//! - `tests/common/mod.rs` re-exported for use in any `tests/*.rs` file
//!
//! Endpoints (match `GenericRestAdapter` defaults — see
//! `src/proxy/upstream.rs:463-464`):
//! - `POST /query`   — renders `QueryResponse` JSON per `ResponseMode`
//! - `GET  /health`  — 200 OK (always)
//!
//! Future endpoints (added when integration tests need them):
//! - `POST /search`        — Qdrant-style points response
//! - `POST /_search`       — Elasticsearch-style hits response
//! - `POST /_vector`       — vector-only search path

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde_json::json;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// How `/query` should respond.
#[derive(Debug, Clone)]
pub enum ResponseMode {
    /// Return 200 with a canned `QueryResponse` containing `n` results.
    Success { result_count: usize },
    /// Return 200 with `QueryResponse { results: vec![], ... }`.
    Empty,
    /// Return the given HTTP status code with empty body.
    Error(u16),
    /// Sleep for `Duration` then respond normally (caller's timeout will fire).
    SlowThenSuccess(Duration),
}

impl Default for ResponseMode {
    fn default() -> Self {
        ResponseMode::Success { result_count: 5 }
    }
}

/// A running mock upstream HTTP server.
pub struct MockUpstream {
    /// Base URL (e.g., `http://127.0.0.1:34567`).
    base_url: String,
    /// Sender to signal graceful shutdown to the axum server.
    shutdown_tx: Option<oneshot::Sender<()>>,
    /// The server task handle (kept so we can await/cleanup on stop).
    server_handle: Option<JoinHandle<()>>,
}

impl MockUpstream {
    /// Bind to a random port and start serving. Returns the server + base URL.
    pub async fn start() -> (Self, String) {
        Self::start_with_mode(ResponseMode::default()).await
    }

    /// Bind to a random port with a custom response mode.
    pub async fn start_with_mode(mode: ResponseMode) -> (Self, String) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind to port 0 should succeed");
        let addr: SocketAddr = listener.local_addr().expect("local_addr");
        let base_url = format!("http://{addr}");

        let app = Router::new()
            .route("/query", post(handle_query))
            .route("/health", get(handle_health))
            .with_state(mode);

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let server_handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
        });

        let server = MockUpstream {
            base_url: base_url.clone(),
            shutdown_tx: Some(shutdown_tx),
            server_handle: Some(server_handle),
        };
        (server, base_url)
    }

    /// Get the base URL (no trailing slash).
    pub fn url(&self) -> &str {
        &self.base_url
    }

    /// Signal shutdown and wait for the server task to complete.
    pub async fn stop(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.server_handle.take() {
            let _ = handle.await;
        }
    }
}

async fn handle_health() -> impl IntoResponse {
    // Health check always returns 200
    (StatusCode::OK, "ok")
}

async fn handle_query(State(mode): State<ResponseMode>) -> impl IntoResponse {
    match mode {
        ResponseMode::Success { result_count } => {
            let results: Vec<serde_json::Value> = (0..result_count)
                .map(|i| {
                    json!({
                        "id": format!("mock-doc-{i}"),
                        "score": 1.0 - (i as f32) * 0.01,
                        "content": format!("mock content for doc {i}"),
                        "metadata": null,
                        "upstream_id": "mock",
                    })
                })
                .collect();
            (
                StatusCode::OK,
                Json(json!({
                    "results": results,
                    "cache_status": "Miss",
                    "took_ms": 1u64,
                })),
            )
        }
        ResponseMode::Empty => (
            StatusCode::OK,
            Json(json!({
                "results": [],
                "cache_status": "Miss",
                "took_ms": 1u64,
            })),
        ),
        ResponseMode::Error(status) => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"error": "mock upstream error"})),
        ),
        ResponseMode::SlowThenSuccess(d) => {
            tokio::time::sleep(d).await;
            (
                StatusCode::OK,
                Json(json!({
                    "results": [{"id": "late", "score": 1.0, "content": "late"}],
                    "cache_status": "Miss",
                    "took_ms": d.as_millis() as u64,
                })),
            )
        }
    }
}
