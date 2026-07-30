//! In-process mock upstream server for E2E tests.
//!
//! Spawns an axum HTTP server in a background thread with its own tokio runtime.
//! Serves Elasticsearch, Qdrant, and generic search response formats simultaneously.
//! Shares `Arc<MockState>` with tests for behavior control and request counting.

use axum::{
    extract::{Path, State},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// Shared state between the mock server and test assertions.
pub struct MockState {
    behavior: RwLock<MockBehavior>,
    latency_ms: AtomicU64,
    total_requests: AtomicU64,
    query_counts: DashMap<String, AtomicU64>,
    response_config: RwLock<MockResponseConfig>,
}

/// Controls how the mock upstream responds.
#[allow(dead_code)]
pub enum MockBehavior {
    /// Return successful responses.
    Healthy,
    /// Return a specific HTTP error code.
    ErrorCode(u16),
    /// Return unparseable JSON.
    MalformedJson,
    /// Never respond (hang forever).
    Timeout,
    /// Cycle through a sequence of behaviors.
    Sequence(Vec<MockBehavior>, AtomicUsize),
}

struct MockResponseConfig {
    documents: Vec<MockDocument>,
    #[allow(dead_code)]
    default_limit: usize,
}

/// A mock document returned by the upstream.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MockDocument {
    pub id: String,
    pub content: String,
    pub score: f32,
    pub metadata: serde_json::Value,
}

// ---------------------------------------------------------------------------
// MockUpstream — public API
// ---------------------------------------------------------------------------

/// An in-process mock upstream HTTP server for testing.
pub struct MockUpstream {
    state: Arc<MockState>,
    addr: std::net::SocketAddr,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    thread_handle: Option<std::thread::JoinHandle<()>>,
}

#[allow(dead_code)]
impl MockUpstream {
    /// Start a mock upstream with 5 default documents.
    pub fn start() -> Self {
        Self::start_with_docs(default_documents())
    }

    /// Start a mock upstream with custom documents.
    pub fn start_with_docs(docs: Vec<MockDocument>) -> Self {
        let state = Arc::new(MockState {
            behavior: RwLock::new(MockBehavior::Healthy),
            latency_ms: AtomicU64::new(0),
            total_requests: AtomicU64::new(0),
            query_counts: DashMap::new(),
            response_config: RwLock::new(MockResponseConfig {
                documents: docs,
                default_limit: 10,
            }),
        });

        // Bind to OS-assigned port
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("Failed to bind mock");
        let addr = listener.local_addr().expect("Failed to get addr");
        listener
            .set_nonblocking(true)
            .expect("Failed to set nonblocking");

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let server_state = state.clone();

        let thread_handle = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to build mock runtime");

            rt.block_on(async move {
                let app = build_router(server_state);
                let tcp = tokio::net::TcpListener::from_std(listener)
                    .expect("Failed to convert listener");

                axum::serve(tcp, app)
                    .with_graceful_shutdown(async {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    .expect("Mock server error");
            });
        });

        // Wait for server to be ready
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        for _ in 0..50 {
            if client.get(format!("http://{}/health", addr)).send().is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        Self {
            state,
            addr,
            shutdown_tx: Some(shutdown_tx),
            thread_handle: Some(thread_handle),
        }
    }

    /// Get the base URL of the mock upstream.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.addr.port())
    }

    /// Set the mock behavior.
    pub fn set_behavior(&self, b: MockBehavior) {
        let state = self.state.clone();
        // Use a blocking approach since tests are sync
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                *state.behavior.write().await = b;
            });
        })
        .join()
        .expect("Failed to set behavior");
    }

    /// Set artificial latency in milliseconds.
    pub fn set_latency_ms(&self, ms: u64) {
        self.state.latency_ms.store(ms, Ordering::SeqCst);
    }

    /// Get total request count.
    pub fn request_count(&self) -> u64 {
        self.state.total_requests.load(Ordering::SeqCst)
    }

    /// Get request count for a specific query string.
    pub fn query_count(&self, query: &str) -> u64 {
        self.state
            .query_counts
            .get(query)
            .map(|v| v.load(Ordering::SeqCst))
            .unwrap_or(0)
    }

    /// Reset all counters.
    pub fn reset_stats(&self) {
        self.state.total_requests.store(0, Ordering::SeqCst);
        self.state.query_counts.clear();
    }
}

impl Drop for MockUpstream {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Default documents
// ---------------------------------------------------------------------------

fn default_documents() -> Vec<MockDocument> {
    vec![
        MockDocument {
            id: "mock-1".into(),
            content: "Rust is a systems programming language focused on safety and performance"
                .into(),
            score: 0.95,
            metadata: serde_json::json!({"title": "Rust Programming"}),
        },
        MockDocument {
            id: "mock-2".into(),
            content: "Elasticsearch provides full-text search and analytics capabilities".into(),
            score: 0.88,
            metadata: serde_json::json!({"title": "Elasticsearch Overview"}),
        },
        MockDocument {
            id: "mock-3".into(),
            content: "Vector databases store embeddings for similarity search".into(),
            score: 0.82,
            metadata: serde_json::json!({"title": "Vector Databases"}),
        },
        MockDocument {
            id: "mock-4".into(),
            content: "Cache eviction policies include LRU, LFU, and TTL-based strategies".into(),
            score: 0.75,
            metadata: serde_json::json!({"title": "Cache Strategies"}),
        },
        MockDocument {
            id: "mock-5".into(),
            content: "Circuit breakers prevent cascading failures in distributed systems".into(),
            score: 0.70,
            metadata: serde_json::json!({"title": "Circuit Breaker Pattern"}),
        },
    ]
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

fn build_router(state: Arc<MockState>) -> Router {
    Router::new()
        // Elasticsearch endpoints
        .route("/{index}/_search", post(handle_es_search))
        .route("/_cluster/health", get(handle_es_health))
        // Qdrant endpoints
        .route(
            "/collections/{coll}/points/search",
            post(handle_qdrant_search),
        )
        .route("/collections/{coll}", get(handle_qdrant_collection_info))
        // Generic endpoints
        .route("/query", post(handle_generic_query))
        .route("/health", get(handle_generic_health))
        // Qdrant root
        .route("/", get(handle_qdrant_root))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Behavior resolution
// ---------------------------------------------------------------------------

/// Check behavior and return early response if not Healthy.
/// Returns None if the request should proceed normally.
async fn check_behavior(state: &MockState) -> Option<axum::response::Response> {
    let behavior = state.behavior.read().await;
    match &*behavior {
        MockBehavior::Healthy => None,
        MockBehavior::ErrorCode(code) => {
            let status = axum::http::StatusCode::from_u16(*code)
                .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
            Some(
                (
                    status,
                    Json(serde_json::json!({"error": format!("Mock error {}", code)})),
                )
                    .into_response(),
            )
        }
        MockBehavior::MalformedJson => {
            Some((axum::http::StatusCode::OK, "this is not valid json {{{").into_response())
        }
        MockBehavior::Timeout => {
            // Hang forever (well, until the connection drops)
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            None
        }
        MockBehavior::Sequence(behaviors, idx) => {
            let current = idx.fetch_add(1, Ordering::SeqCst);
            let behavior = &behaviors[current % behaviors.len()];
            match behavior {
                MockBehavior::Healthy => None,
                MockBehavior::ErrorCode(code) => {
                    let status = axum::http::StatusCode::from_u16(*code)
                        .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
                    Some(
                        (
                            status,
                            Json(serde_json::json!({"error": format!("Mock error {}", code)})),
                        )
                            .into_response(),
                    )
                }
                MockBehavior::MalformedJson => {
                    Some((axum::http::StatusCode::OK, "this is not valid json {{{").into_response())
                }
                MockBehavior::Timeout => {
                    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                    None
                }
                MockBehavior::Sequence(_, _) => None, // Nested sequences not supported
            }
        }
    }
}

/// Increment counters and apply latency.
async fn record_and_delay(state: &MockState, query: Option<&str>) {
    state.total_requests.fetch_add(1, Ordering::SeqCst);
    if let Some(q) = query {
        state
            .query_counts
            .entry(q.to_string())
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::SeqCst);
    }

    let latency = state.latency_ms.load(Ordering::SeqCst);
    if latency > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(latency)).await;
    }
}

// ---------------------------------------------------------------------------
// Elasticsearch handlers
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct EsSearchBody {
    query: Option<serde_json::Value>,
}

async fn handle_es_search(
    State(state): State<Arc<MockState>>,
    Path(_index): Path<String>,
    body: Option<Json<EsSearchBody>>,
) -> impl IntoResponse {
    // Extract query string from ES query body
    let query_str = body
        .as_ref()
        .and_then(|b| b.query.as_ref())
        .and_then(|q| {
            // Try multi_match → query, then match_all
            q.get("multi_match")
                .and_then(|m| m.get("query"))
                .and_then(|v| v.as_str())
                .or_else(|| {
                    q.get("match")
                        .and_then(|m| m.as_object())
                        .and_then(|obj| obj.values().next())
                        .and_then(|v| v.as_str())
                })
        })
        .map(|s| s.to_string());

    record_and_delay(&state, query_str.as_deref()).await;

    if let Some(resp) = check_behavior(&state).await {
        return resp;
    }

    let config = state.response_config.read().await;
    let hits: Vec<serde_json::Value> = config
        .documents
        .iter()
        .map(|doc| {
            serde_json::json!({
                "_id": doc.id,
                "_score": doc.score * 15.0,
                "_source": {
                    "content": doc.content,
                    "title": doc.metadata.get("title").and_then(|t| t.as_str()).unwrap_or(""),
                }
            })
        })
        .collect();

    let max_score = config
        .documents
        .first()
        .map(|d| d.score * 15.0)
        .unwrap_or(0.0);

    Json(serde_json::json!({
        "hits": {
            "total": {"value": hits.len()},
            "max_score": max_score,
            "hits": hits
        }
    }))
    .into_response()
}

async fn handle_es_health(State(state): State<Arc<MockState>>) -> impl IntoResponse {
    record_and_delay(&state, None).await;
    if let Some(resp) = check_behavior(&state).await {
        return resp;
    }
    Json(serde_json::json!({
        "status": "green",
        "cluster_name": "mock"
    }))
    .into_response()
}

// ---------------------------------------------------------------------------
// Qdrant handlers
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct QdrantSearchBody {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    filter: Option<serde_json::Value>,
}

async fn handle_qdrant_search(
    State(state): State<Arc<MockState>>,
    Path(_coll): Path<String>,
    body: Option<Json<QdrantSearchBody>>,
) -> impl IntoResponse {
    let query_str = body.as_ref().and_then(|b| b.query.as_deref());
    record_and_delay(&state, query_str).await;

    if let Some(resp) = check_behavior(&state).await {
        return resp;
    }

    let config = state.response_config.read().await;
    let results: Vec<serde_json::Value> = config
        .documents
        .iter()
        .map(|doc| {
            serde_json::json!({
                "id": doc.id,
                "score": doc.score,
                "payload": {
                    "content": doc.content,
                    "title": doc.metadata.get("title").and_then(|t| t.as_str()).unwrap_or(""),
                }
            })
        })
        .collect();

    Json(serde_json::json!({
        "result": results,
        "time": 0.003
    }))
    .into_response()
}

async fn handle_qdrant_collection_info(
    State(state): State<Arc<MockState>>,
    Path(_coll): Path<String>,
) -> impl IntoResponse {
    record_and_delay(&state, None).await;
    if let Some(resp) = check_behavior(&state).await {
        return resp;
    }
    Json(serde_json::json!({
        "result": {
            "status": "green",
            "points_count": 100
        }
    }))
    .into_response()
}

async fn handle_qdrant_root(State(state): State<Arc<MockState>>) -> impl IntoResponse {
    record_and_delay(&state, None).await;
    if let Some(resp) = check_behavior(&state).await {
        return resp;
    }
    Json(serde_json::json!({
        "title": "qdrant - vector search engine",
        "version": "1.0.0"
    }))
    .into_response()
}

// ---------------------------------------------------------------------------
// Generic handlers
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct GenericQueryBody {
    #[serde(default)]
    query: Option<String>,
}

async fn handle_generic_query(
    State(state): State<Arc<MockState>>,
    body: Option<Json<GenericQueryBody>>,
) -> impl IntoResponse {
    let query_str = body.as_ref().and_then(|b| b.query.as_deref());
    record_and_delay(&state, query_str).await;

    if let Some(resp) = check_behavior(&state).await {
        return resp;
    }

    let config = state.response_config.read().await;
    let results: Vec<serde_json::Value> = config
        .documents
        .iter()
        .map(|doc| {
            serde_json::json!({
                "id": doc.id,
                "score": doc.score,
                "content": doc.content,
                "metadata": doc.metadata
            })
        })
        .collect();

    Json(serde_json::json!({
        "results": results,
        "total": results.len(),
        "took_ms": 1
    }))
    .into_response()
}

async fn handle_generic_health(State(state): State<Arc<MockState>>) -> impl IntoResponse {
    record_and_delay(&state, None).await;
    if let Some(resp) = check_behavior(&state).await {
        return resp;
    }
    Json(serde_json::json!({
        "status": "ok"
    }))
    .into_response()
}
