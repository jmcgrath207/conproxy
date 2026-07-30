//! PeerServiceImpl — gRPC GetStatus + Snapshot RPCs.

use std::sync::Arc;

use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use crate::proxy::cache::CacheStore;
use crate::proxy::cdc::proto;
use crate::proxy::cdc::SharedEventSender;

use super::PeerState;

/// gRPC implementation of PeerService.
pub struct PeerServiceImpl {
    cache: Arc<CacheStore>,
    event_sender: SharedEventSender,
    state: Arc<std::sync::atomic::AtomicU8>,
    snapshot_batch_size: usize,
}

impl PeerServiceImpl {
    pub fn new(
        cache: Arc<CacheStore>,
        event_sender: SharedEventSender,
        state: Arc<std::sync::atomic::AtomicU8>,
        snapshot_batch_size: usize,
    ) -> Self {
        Self {
            cache,
            event_sender,
            state,
            snapshot_batch_size,
        }
    }
}

#[tonic::async_trait]
impl proto::peer_service_server::PeerService for PeerServiceImpl {
    async fn get_status(
        &self,
        _request: Request<proto::PeerStatusRequest>,
    ) -> Result<Response<proto::PeerStatusResponse>, Status> {
        let state_val = self.state.load(std::sync::atomic::Ordering::Relaxed);
        let state_str = PeerState::from_u8(state_val).as_str().to_string();

        Ok(Response::new(proto::PeerStatusResponse {
            node_id: self.event_sender.node_id().to_string(),
            state: state_str,
            cache_entry_count: self.cache.len() as u64,
            sequence: self.event_sender.current_sequence(),
        }))
    }

    type SnapshotStream = ReceiverStream<Result<proto::CdcEvent, Status>>;

    async fn snapshot(
        &self,
        request: Request<proto::SnapshotRequest>,
    ) -> Result<Response<Self::SnapshotStream>, Status> {
        let req = request.into_inner();
        let (tx, rx) = tokio::sync::mpsc::channel(256);

        let cache = self.cache.clone();
        let event_sender = self.event_sender.clone();
        let batch_size = self.snapshot_batch_size;
        let context_id = req.context_id;

        tokio::spawn(async move {
            super::snapshot::stream_snapshot(cache, event_sender, &context_id, batch_size, tx)
                .await;
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::proxy::cdc::EventSender;
    use crate::proxy::types::{CacheStatus, QueryResponse, SearchResult};
    use proto::peer_service_server::PeerService;
    use std::sync::atomic::AtomicU8;
    use std::time::Duration;
    use tokio_stream::StreamExt;

    #[test]
    fn test_peer_state_str() {
        assert_eq!(PeerState::Warming.as_str(), "warming");
        assert_eq!(PeerState::Ready.as_str(), "ready");
    }

    #[test]
    fn test_peer_state_roundtrip() {
        assert_eq!(PeerState::from_u8(0), PeerState::Warming);
        assert_eq!(PeerState::from_u8(1), PeerState::Ready);
        assert_eq!(PeerState::from_u8(99), PeerState::Warming); // unknown defaults to Warming
    }

    fn make_service() -> PeerServiceImpl {
        let cache = Arc::new(CacheStore::new(
            Duration::from_secs(300),
            Duration::from_secs(600),
            1000,
        ));
        let sender = Arc::new(EventSender::new(100, "test-node".to_string()));
        let state = Arc::new(AtomicU8::new(PeerState::Ready as u8));
        PeerServiceImpl::new(cache, sender, state, 100)
    }

    fn make_service_with_entries(n: usize) -> PeerServiceImpl {
        let cache = Arc::new(CacheStore::new(
            Duration::from_secs(300),
            Duration::from_secs(600),
            1000,
        ));
        for i in 0..n {
            let response = QueryResponse {
                results: vec![SearchResult {
                    id: format!("doc-{i}"),
                    content: format!("content-{i}"),
                    score: 0.9,
                    metadata: Default::default(),
                    upstream_id: None,
                }],
                cache_status: CacheStatus::Miss,
                took_ms: 0,
                generated_at: None,
                miss_reason: None,
            };
            cache.insert(&format!("query-{i}"), response, "up-1".to_string());
        }
        let sender = Arc::new(EventSender::new(100, "test-node".to_string()));
        let state = Arc::new(AtomicU8::new(PeerState::Ready as u8));
        PeerServiceImpl::new(cache, sender, state, 100)
    }

    #[tokio::test]
    async fn test_get_status_empty_cache() {
        let service = make_service();
        let request = Request::new(proto::PeerStatusRequest {});
        let response = service.get_status(request).await.unwrap();
        let inner = response.into_inner();

        assert_eq!(inner.node_id, "test-node");
        assert_eq!(inner.state, "ready");
        assert_eq!(inner.cache_entry_count, 0);
    }

    #[tokio::test]
    async fn test_get_status_with_entries() {
        let service = make_service_with_entries(5);
        let request = Request::new(proto::PeerStatusRequest {});
        let response = service.get_status(request).await.unwrap();
        let inner = response.into_inner();

        assert_eq!(inner.cache_entry_count, 5);
        assert_eq!(inner.state, "ready");
    }

    #[tokio::test]
    async fn test_get_status_warming_state() {
        let cache = Arc::new(CacheStore::new(
            Duration::from_secs(300),
            Duration::from_secs(600),
            1000,
        ));
        let sender = Arc::new(EventSender::new(100, "n".to_string()));
        let state = Arc::new(AtomicU8::new(PeerState::Warming as u8));
        let service = PeerServiceImpl::new(cache, sender, state, 100);

        let request = Request::new(proto::PeerStatusRequest {});
        let response = service.get_status(request).await.unwrap();
        assert_eq!(response.into_inner().state, "warming");
    }

    #[tokio::test]
    async fn test_snapshot_empty_cache() {
        let service = make_service();
        let request = Request::new(proto::SnapshotRequest {
            requester_node_id: "peer-b".to_string(),
            context_id: "".to_string(),
        });
        let response = service.snapshot(request).await.unwrap();
        let mut stream = response.into_inner();

        // Empty cache should produce no messages
        let msg = stream.next().await;
        assert!(msg.is_none());
    }

    #[tokio::test]
    async fn test_snapshot_with_entries() {
        let service = make_service_with_entries(3);
        let request = Request::new(proto::SnapshotRequest {
            requester_node_id: "peer-b".to_string(),
            context_id: "".to_string(),
        });
        let response = service.snapshot(request).await.unwrap();
        let mut stream = response.into_inner();

        let mut count = 0;
        while let Some(Ok(event)) = stream.next().await {
            assert!(!event.query_key.is_empty());
            assert!(!event.payload.is_empty());
            count += 1;
        }
        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn test_snapshot_with_batch_size_one() {
        // batch_size=1 → yields after every single entry
        let cache = Arc::new(CacheStore::new(
            Duration::from_secs(300),
            Duration::from_secs(600),
            1000,
        ));
        for i in 0..3 {
            let response = QueryResponse {
                results: vec![SearchResult {
                    id: format!("d-{i}"),
                    content: format!("c-{i}"),
                    score: 0.9,
                    metadata: Default::default(),
                    upstream_id: None,
                }],
                cache_status: CacheStatus::Miss,
                took_ms: 0,
                generated_at: None,
                miss_reason: None,
            };
            cache.insert(&format!("q-{i}"), response, "u".to_string());
        }
        let sender = Arc::new(EventSender::new(100, "n".to_string()));
        let state = Arc::new(AtomicU8::new(PeerState::Ready as u8));
        let service = PeerServiceImpl::new(cache, sender, state, 1); // batch=1

        let request = Request::new(proto::SnapshotRequest {
            requester_node_id: "peer-b".to_string(),
            context_id: "".to_string(),
        });
        let response = service.snapshot(request).await.unwrap();
        let mut stream = response.into_inner();

        let mut count = 0;
        while let Some(Ok(_)) = stream.next().await {
            count += 1;
        }
        assert_eq!(count, 3, "should still get all 3 entries with batch=1");
    }

    #[tokio::test]
    async fn test_snapshot_with_batch_size_larger_than_cache() {
        // batch_size=100 with 3 entries → all 3 still sent (batch boundary irrelevant)
        let cache = Arc::new(CacheStore::new(
            Duration::from_secs(300),
            Duration::from_secs(600),
            1000,
        ));
        for i in 0..3 {
            let response = QueryResponse {
                results: vec![SearchResult {
                    id: format!("d-{i}"),
                    content: format!("c-{i}"),
                    score: 0.9,
                    metadata: Default::default(),
                    upstream_id: None,
                }],
                cache_status: CacheStatus::Miss,
                took_ms: 0,
                generated_at: None,
                miss_reason: None,
            };
            cache.insert(&format!("q-{i}"), response, "u".to_string());
        }
        let sender = Arc::new(EventSender::new(100, "n".to_string()));
        let state = Arc::new(AtomicU8::new(PeerState::Ready as u8));
        let service = PeerServiceImpl::new(cache, sender, state, 100);

        let request = Request::new(proto::SnapshotRequest {
            requester_node_id: "peer-b".to_string(),
            context_id: "".to_string(),
        });
        let response = service.snapshot(request).await.unwrap();
        let mut stream = response.into_inner();

        let mut count = 0;
        while let Some(Ok(_)) = stream.next().await {
            count += 1;
        }
        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn test_snapshot_cancellation_drops_stream() {
        // Drop the stream early → should not panic, task should clean up
        let service = make_service_with_entries(10);
        let request = Request::new(proto::SnapshotRequest {
            requester_node_id: "peer-c".to_string(),
            context_id: "ctx-1".to_string(),
        });
        let response = service.snapshot(request).await.unwrap();
        let mut stream = response.into_inner();

        // Pull just 2 events then drop
        let _ = stream.next().await;
        let _ = stream.next().await;
        drop(stream);
        // Give the spawned task a moment to observe the drop
        tokio::time::sleep(Duration::from_millis(50)).await;
        // No assertion needed — just verify no panic/hang
    }

    #[test]
    fn test_peer_service_construction_with_all_params() {
        // Verifies all constructor params are stored/used
        let cache = Arc::new(CacheStore::new(
            Duration::from_secs(60),
            Duration::from_secs(120),
            500,
        ));
        let sender = Arc::new(EventSender::new(50, "node-x".to_string()));
        let state = Arc::new(AtomicU8::new(2));
        let service = PeerServiceImpl::new(cache.clone(), sender, state, 25);
        // Service constructed; verify it's usable by calling get_status
        let svc = service; // move
        let req = Request::new(proto::PeerStatusRequest {});
        let rt = tokio::runtime::Runtime::new().unwrap();
        let resp = rt.block_on(svc.get_status(req)).unwrap();
        assert_eq!(resp.into_inner().node_id, "node-x");
    }
}
