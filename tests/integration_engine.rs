//! Integration tests for the in-process `Engine` facade against a live backend.
//!
//! Proves the Engine -> toml -> typed-adapter -> live-Meilisearch path and
//! the read-only dashboard on a populated cache. Adapter-level behavior lives
//! in `integration_meilisearch`; transport/auth/load coverage lives in the
//! daemon e2e suite. This file only covers Engine wiring that is not exercised
//! anywhere else.
//!
//! Requires `--features integration-tests` and a running Docker daemon.

#![cfg(feature = "integration-tests")]

mod test_infra;

use conproxy::engine::{Engine, QueryOpts};
use conproxy::proxy::types::CacheStatus;
use std::path::{Path, PathBuf};
use test_infra::containers::MEILI_MASTER_KEY;

fn meili_toml(dir: &Path, base_url: &str, index: &str) -> PathBuf {
    let p = dir.join("conproxy.toml");
    std::fs::write(
        &p,
        format!(
            r#"
[upstreams.meili]
url = "{base_url}"
type = "meilisearch"
index = "{index}"
api_key = "{MEILI_MASTER_KEY}"

[contexts.global]
default = true

[[contexts.global.upstreams]]
ref = "meili"
"#
        ),
    )
    .unwrap();
    p
}

async fn seed_docs(index: &str, base_url: &str) {
    test_infra::containers::meili_create_index(base_url, index, "id").await;
    test_infra::containers::meili_add_documents(
        base_url,
        index,
        vec![
            serde_json::json!({
                "id": "doc-001",
                "title": "Rust async tokio runtime",
                "content": "Tokio is an async runtime for the Rust programming language."
            }),
            serde_json::json!({
                "id": "doc-002",
                "title": "Distributed cache patterns",
                "content": "Read-through and write-behind caching strategies."
            }),
        ],
    )
    .await;
}

#[tokio::test]
async fn engine_miss_then_hit_against_live_meilisearch() {
    test_infra::containers::docker_check();
    let inst = test_infra::containers::meilisearch_container().await;
    seed_docs("engine_fts", &inst.base_url).await;

    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::builder()
        .config_toml(meili_toml(dir.path(), &inst.base_url, "engine_fts"))
        .build()
        .expect("engine build");

    let first = engine
        .query("rust async", QueryOpts::default())
        .await
        .expect("first query should succeed");
    assert!(
        !first.results.is_empty(),
        "first query should return results from live Meilisearch"
    );
    assert_eq!(
        first.cache_status,
        CacheStatus::Miss,
        "first query must be a miss"
    );

    let second = engine
        .query("rust async", QueryOpts::default())
        .await
        .expect("second query should succeed");
    assert_eq!(
        second.cache_status,
        CacheStatus::Hit,
        "second query must be a cache hit"
    );
    assert!(
        !second.results.is_empty(),
        "hit should still return cached results"
    );
    engine.close();
}

#[tokio::test]
async fn engine_dashboard_serves_live_cache_state() {
    test_infra::containers::docker_check();
    let inst = test_infra::containers::meilisearch_container().await;
    seed_docs("engine_dash", &inst.base_url).await;

    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::builder()
        .config_toml(meili_toml(dir.path(), &inst.base_url, "engine_dash"))
        .dashboard_listen("127.0.0.1:0")
        .build()
        .expect("engine build");

    // Populate the cache through the real backend first.
    let first = engine
        .query("caching", QueryOpts::default())
        .await
        .expect("query should succeed");
    assert_eq!(first.cache_status, CacheStatus::Miss);

    let addr = engine.dashboard_addr().expect("dashboard bound");
    let client = reqwest::Client::new();

    for path in [
        "/health",
        "/stats",
        "/stats/queries",
        "/metrics",
        "/circuit",
        "/queue",
        "/pool",
        "/cache/integrity",
        "/cache/upstreams",
        "/cache/entries",
        "/contexts",
        "/contexts/current",
        "/contexts/global/stats",
        "/debug/tokio",
    ] {
        let resp = client
            .get(format!("http://{addr}{path}"))
            .send()
            .await
            .unwrap_or_else(|e| panic!("GET {path}: {e}"));
        assert!(
            resp.status().is_success(),
            "GET {path} should be 2xx, got {}",
            resp.status()
        );
    }

    for (method, path) in [
        ("POST", "/query"),
        ("POST", "/cache/clear"),
        ("POST", "/admin/reload"),
        ("POST", "/contexts/switch"),
    ] {
        let resp = client
            .request(
                reqwest::Method::from_bytes(method.as_bytes()).unwrap(),
                format!("http://{addr}{path}"),
            )
            .send()
            .await
            .unwrap_or_else(|e| panic!("{method} {path}: {e}"));
        assert_eq!(
            resp.status().as_u16(),
            404,
            "{method} {path} should be absent from the read-only dashboard"
        );
    }
    engine.close();
}
