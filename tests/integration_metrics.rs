//! Metrics smoke: traffic against Meili, then HTTP scrape of prometheus text.
//!
//! Requires `--features integration-tests` and Docker.

#![cfg(feature = "integration-tests")]

mod test_infra;

use axum::routing::get;
use axum::{extract::State, Router};
use conproxy::proxy::meilisearch::{MeilisearchAdapter, MeilisearchConfig};
use conproxy::proxy::metrics::ProxyMetrics;
use conproxy::proxy::types::{MissReason, QueryRequest};
use conproxy::proxy::upstream::UpstreamAdapter;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use test_infra::containers::MEILI_MASTER_KEY;
use tokio::net::TcpListener;

async fn prom_handler(State(metrics): State<Arc<ProxyMetrics>>) -> String {
    metrics.snapshot().to_prometheus()
}

#[tokio::test]
async fn metrics_prometheus_scrape_after_traffic() {
    test_infra::containers::docker_check();
    let inst = test_infra::containers::meilisearch_container().await;

    test_infra::containers::meili_create_index(&inst.base_url, "metrics_idx", "id").await;
    test_infra::containers::meili_add_documents(
        &inst.base_url,
        "metrics_idx",
        vec![serde_json::json!({
            "id": "m1",
            "content": "metrics scrape proof for conproxy prometheus export"
        })],
    )
    .await;

    let adapter = MeilisearchAdapter::new(MeilisearchConfig {
        base_url: inst.base_url.clone(),
        index: "metrics_idx".into(),
        timeout: Duration::from_secs(10),
        search_attributes: vec!["content".into()],
        displayed_attributes: vec![],
        api_key: Some(MEILI_MASTER_KEY.to_string()),
        score_threshold: None,
    })
    .expect("adapter");

    let metrics = Arc::new(ProxyMetrics::new());
    let req = QueryRequest {
        query: "prometheus conproxy".into(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };

    // Simulate query path counters after real upstream traffic
    for _ in 0..3 {
        let t0 = Instant::now();
        let resp = adapter.query(&req).await.expect("query");
        assert!(!resp.results.is_empty());
        metrics.record_miss(MissReason::NotInCache);
        metrics.record_latency(t0.elapsed());
        metrics.record_upstream_request();
    }
    // One synthetic hit for series presence
    metrics.record_hit();

    let app = Router::new()
        .route("/metrics/prometheus", get(prom_handler))
        .with_state(metrics.clone());
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    let body = reqwest::Client::new()
        .get(format!("http://{addr}/metrics/prometheus"))
        .send()
        .await
        .expect("scrape")
        .text()
        .await
        .expect("body");

    for needle in [
        "conproxy_requests_total",
        "conproxy_cache_hits_total",
        "conproxy_cache_misses_total",
        "conproxy_upstream_requests_total",
    ] {
        assert!(
            body.contains(needle),
            "prometheus body missing {needle}:\n{body}"
        );
    }
    let snap = metrics.snapshot();
    assert!(snap.requests_total >= 4, "expected traffic counters");
    assert!(snap.cache_hits >= 1);
    assert!(snap.cache_misses >= 3);
}
