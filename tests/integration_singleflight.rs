//! Singleflight proof: N concurrent identical queries → one upstream hit (Meili).
//!
//! Requires `--features integration-tests` and Docker.

#![cfg(feature = "integration-tests")]

mod test_infra;

use conproxy::proxy::coalesce::{CoalesceAction, RequestCoalescer};
use conproxy::proxy::meilisearch::{MeilisearchAdapter, MeilisearchConfig};
use conproxy::proxy::types::{QueryHash, QueryRequest, QueryResponse};
use conproxy::proxy::upstream::{AdapterMetadata, UpstreamAdapter, UpstreamError};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use test_infra::containers::MEILI_MASTER_KEY;
use tokio::sync::Barrier;

/// Counts `query` calls into an inner adapter.
struct CountingAdapter {
    inner: MeilisearchAdapter,
    hits: AtomicU64,
}

#[async_trait::async_trait]
impl UpstreamAdapter for CountingAdapter {
    async fn query(&self, request: &QueryRequest) -> Result<QueryResponse, UpstreamError> {
        self.hits.fetch_add(1, Ordering::SeqCst);
        // Hold briefly so waiters join before leader completes.
        tokio::time::sleep(Duration::from_millis(80)).await;
        self.inner.query(request).await
    }

    async fn health_check(&self) -> Result<bool, UpstreamError> {
        self.inner.health_check().await
    }

    fn identifier(&self) -> &str {
        self.inner.identifier()
    }

    fn timeout(&self) -> Duration {
        self.inner.timeout()
    }

    fn metadata(&self) -> AdapterMetadata {
        self.inner.metadata()
    }
}

fn query_hash(q: &str) -> QueryHash {
    *blake3::hash(q.as_bytes()).as_bytes()
}

#[tokio::test]
async fn singleflight_one_upstream_hit_for_concurrent_identical_queries() {
    test_infra::containers::docker_check();
    let inst = test_infra::containers::meilisearch_container().await;

    test_infra::containers::meili_create_index(&inst.base_url, "sf_test", "id").await;
    test_infra::containers::meili_add_documents(
        &inst.base_url,
        "sf_test",
        vec![serde_json::json!({
            "id": "doc-001",
            "title": "Rust async",
            "content": "Tokio is an async runtime for Rust."
        })],
    )
    .await;

    let meili = MeilisearchAdapter::new(MeilisearchConfig {
        base_url: inst.base_url.clone(),
        index: "sf_test".into(),
        timeout: Duration::from_secs(10),
        search_attributes: vec!["content".into()],
        displayed_attributes: vec![],
        api_key: Some(MEILI_MASTER_KEY.to_string()),
        score_threshold: None,
    })
    .expect("MeilisearchAdapter::new");

    let adapter = Arc::new(CountingAdapter {
        inner: meili,
        hits: AtomicU64::new(0),
    });
    let coalescer = Arc::new(RequestCoalescer::new());
    let n = 12usize;
    let barrier = Arc::new(Barrier::new(n));
    let req = QueryRequest {
        query: "rust async".into(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };
    let hash = query_hash(&req.query);

    let mut handles = Vec::with_capacity(n);
    for _ in 0..n {
        let adapter = Arc::clone(&adapter);
        let coalescer = Arc::clone(&coalescer);
        let barrier = Arc::clone(&barrier);
        let req = req.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            match coalescer.get_or_insert(hash) {
                CoalesceAction::Leader => {
                    let result = adapter.query(&req).await.map(Arc::new).map_err(Arc::new);
                    // Map UpstreamError → leave as Arc; complete expects CoalesceResult
                    // which is Result<Arc<QueryResponse>, Arc<ProxyError>>.
                    // Use remove+manual path if types differ — convert via complete only on Ok.
                    match result {
                        Ok(resp) => {
                            coalescer.complete(&hash, Ok(resp.clone()));
                            Ok(resp)
                        }
                        Err(e) => {
                            coalescer.remove(&hash);
                            Err(e.to_string())
                        }
                    }
                }
                CoalesceAction::Waiter(mut rx) => match rx.recv().await {
                    Ok(Ok(resp)) => Ok(resp),
                    Ok(Err(e)) => Err(e.to_string()),
                    Err(e) => Err(e.to_string()),
                },
            }
        }));
    }

    let mut ok = 0usize;
    for h in handles {
        let r = h.await.expect("join");
        assert!(r.is_ok(), "all waiters/leader should get Ok: {r:?}");
        let resp = r.unwrap();
        assert!(!resp.results.is_empty(), "non-empty results");
        ok += 1;
    }
    assert_eq!(ok, n);
    assert_eq!(
        adapter.hits.load(Ordering::SeqCst),
        1,
        "exactly one upstream query for {n} concurrent identical requests"
    );
}
