//! Snapshot server (stream cache entries) and client (request + apply on rejoin).

use std::sync::Arc;
use std::time::Duration;

use tonic::Status;
use tracing::{debug, info};

use crate::proxy::cache::CacheStore;
use crate::proxy::cdc::event::now_ms;
use crate::proxy::cdc::proto;
use crate::proxy::cdc::stream::proto_to_internal;
use crate::proxy::cdc::CdcEventType;
use crate::proxy::peer::receiver::{apply_event, deduplicate, DeduplicateResult};

/// Stream all cache entries as CDC INSERT events.
///
/// This is called by PeerServiceImpl when a peer requests a snapshot.
/// Entries are streamed in batches to control gRPC message size.
pub async fn stream_snapshot(
    cache: Arc<CacheStore>,
    event_sender: Arc<EventSender>,
    _context_id: &str,
    batch_size: usize,
    tx: tokio::sync::mpsc::Sender<Result<proto::CdcEvent, Status>>,
) {
    let entries = cache.snapshot_entries();
    let total = entries.len();
    let node_id = event_sender.node_id().to_string();
    #[allow(clippy::arithmetic_side_effects)]
    let ttl_ms = (cache.fresh_duration() + cache.stale_duration()).as_millis() as u64;
    let now = now_ms();

    info!(
        entries = total,
        batch_size, "Starting cache snapshot stream"
    );

    let mut sent: usize = 0;
    for (i, (query_text, payload, upstream_id)) in entries.into_iter().enumerate() {
        let absolute_expiry_ms = if ttl_ms > 0 {
            now.saturating_add(ttl_ms)
        } else {
            0
        };

        let proto_event = proto::CdcEvent {
            sequence: (i.saturating_add(1)) as u64,
            timestamp_ms: now,
            event_type: CdcEventType::Insert.to_proto(),
            query_key: query_text,
            payload,
            upstream_id,
            context_id: String::new(),
            origin_node_id: node_id.clone(),
            absolute_expiry_ms,
        };

        if tx.send(Ok(proto_event)).await.is_err() {
            debug!(sent, "Snapshot client disconnected");
            return;
        }
        sent = sent.saturating_add(1);

        // Yield periodically to avoid blocking the runtime
        #[allow(clippy::arithmetic_side_effects)]
        let should_yield = batch_size > 0 && sent.is_multiple_of(batch_size);
        if should_yield {
            tokio::task::yield_now().await;
        }
    }

    info!(sent, "Cache snapshot stream complete");
}

use crate::proxy::cdc::EventSender;

/// Request a snapshot from a remote peer and apply it to the local cache.
///
/// Returns the number of entries applied.
///
/// # Errors
///
/// Returns a transport error if the gRPC channel cannot be established
/// to `peer_addr` within the 5-minute timeout. Returns an error if the
/// snapshot stream is interrupted, the peer rejects the request, or any
/// received CDC event fails to deserialize.
pub async fn request_snapshot(
    peer_addr: &str,
    local_node_id: &str,
    cache: Arc<CacheStore>,
    context_id: &str,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    request_snapshot_with_secret(peer_addr, local_node_id, cache, context_id, None).await
}

/// Like [`request_snapshot`] but sends optional peer shared secret.
pub async fn request_snapshot_with_secret(
    peer_addr: &str,
    local_node_id: &str,
    cache: Arc<CacheStore>,
    context_id: &str,
    shared_secret: Option<&str>,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    let endpoint = format!("http://{}", peer_addr);
    let channel = tonic::transport::Endpoint::from_shared(endpoint)?
        .timeout(Duration::from_secs(300)) // Snapshots can take a while
        .connect()
        .await?;

    let mut client = proto::peer_service_client::PeerServiceClient::new(channel);

    let request = proto::SnapshotRequest {
        requester_node_id: local_node_id.to_string(),
        context_id: context_id.to_string(),
    };
    let mut request = tonic::Request::new(request);
    if let Some(secret) = shared_secret {
        request = crate::proxy::grpc::middleware::insert_peer_secret_metadata(request, secret);
    }

    let mut stream = client.snapshot(request).await?.into_inner();
    let mut applied: usize = 0;

    while let Some(proto_event) = stream.message().await? {
        if let Some(event) = proto_to_internal(&proto_event) {
            let result = deduplicate(&event, local_node_id, &cache);
            if result == DeduplicateResult::Apply && apply_event(&event, &cache) {
                applied = applied.saturating_add(1);
            }
        }
    }

    info!(applied, peer = %peer_addr, "Snapshot applied");
    Ok(applied)
}

/// Check parity between local cache and a remote peer.
///
/// Returns `(local_count, remote_count)`.
///
/// # Errors
///
/// Returns a transport error if the gRPC channel cannot be established
/// to `peer_addr` within the 5-second timeout, or if the parity request
/// fails on the remote side.
pub async fn check_parity(
    peer_addr: &str,
    local_cache: &CacheStore,
) -> Result<(u64, u64), Box<dyn std::error::Error + Send + Sync>> {
    check_parity_with_secret(peer_addr, local_cache, None).await
}

/// Like [`check_parity`] but sends optional peer shared secret.
pub async fn check_parity_with_secret(
    peer_addr: &str,
    local_cache: &CacheStore,
    shared_secret: Option<&str>,
) -> Result<(u64, u64), Box<dyn std::error::Error + Send + Sync>> {
    let endpoint = format!("http://{}", peer_addr);
    let channel = tonic::transport::Endpoint::from_shared(endpoint)?
        .timeout(Duration::from_secs(5))
        .connect()
        .await?;

    let mut client = proto::peer_service_client::PeerServiceClient::new(channel);
    let mut req = tonic::Request::new(proto::PeerStatusRequest {});
    if let Some(secret) = shared_secret {
        req = crate::proxy::grpc::middleware::insert_peer_secret_metadata(req, secret);
    }
    let resp = client.get_status(req).await?.into_inner();

    Ok((local_cache.len() as u64, resp.cache_entry_count))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::proxy::types::{QueryResponse, SearchResult};

    fn make_cache_with_entries(n: usize) -> Arc<CacheStore> {
        let cache = Arc::new(CacheStore::new(
            Duration::from_secs(300),
            Duration::from_secs(600),
            10000,
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
                cache_status: crate::proxy::types::CacheStatus::Miss,
                took_ms: 0,
                generated_at: None,
                miss_reason: None,
            };
            cache.insert(&format!("query-{i}"), response, "up-1".to_string());
        }
        cache
    }

    #[test]
    fn test_snapshot_entries_all_included() {
        let cache = make_cache_with_entries(5);
        let entries = cache.snapshot_entries();
        assert_eq!(entries.len(), 5);
        for (query, payload, upstream_id) in &entries {
            assert!(!query.is_empty());
            assert!(!payload.is_empty());
            assert_eq!(upstream_id, "up-1");
        }
    }

    #[test]
    fn test_snapshot_entries_empty_cache() {
        let cache = Arc::new(CacheStore::new(
            Duration::from_secs(300),
            Duration::from_secs(600),
            1000,
        ));
        assert!(cache.snapshot_entries().is_empty());
    }

    #[tokio::test]
    async fn test_stream_snapshot_empty_cache() {
        let cache = Arc::new(CacheStore::new(
            Duration::from_secs(300),
            Duration::from_secs(600),
            1000,
        ));
        let sender = Arc::new(EventSender::new(100, "node-a".to_string()));
        let (tx, mut rx) = tokio::sync::mpsc::channel(256);

        stream_snapshot(cache, sender, "", 100, tx).await;

        // No entries should be sent
        let msg = rx.try_recv();
        assert!(msg.is_err());
    }

    #[tokio::test]
    async fn test_stream_snapshot_with_entries() {
        let cache = make_cache_with_entries(5);
        let sender = Arc::new(EventSender::new(100, "node-a".to_string()));
        let (tx, mut rx) = tokio::sync::mpsc::channel(256);

        stream_snapshot(cache, sender, "", 100, tx).await;

        let mut count = 0;
        while let Ok(msg) = rx.try_recv() {
            let event = msg.unwrap();
            assert_eq!(event.event_type, CdcEventType::Insert.to_proto());
            assert!(!event.query_key.is_empty());
            assert!(!event.payload.is_empty());
            assert_eq!(event.origin_node_id, "node-a");
            assert_eq!(event.sequence as usize, count + 1);
            count += 1;
        }
        assert_eq!(count, 5);
    }

    #[tokio::test]
    async fn test_stream_snapshot_batch_yielding() {
        // With batch_size=2 and 5 entries, should yield every 2 entries
        let cache = make_cache_with_entries(5);
        let sender = Arc::new(EventSender::new(100, "node-a".to_string()));
        let (tx, mut rx) = tokio::sync::mpsc::channel(256);

        stream_snapshot(cache, sender, "", 2, tx).await;

        let mut count = 0;
        while let Ok(msg) = rx.try_recv() {
            msg.unwrap();
            count += 1;
        }
        assert_eq!(count, 5);
    }

    #[tokio::test]
    async fn test_stream_snapshot_client_disconnect() {
        let cache = make_cache_with_entries(100);
        let sender = Arc::new(EventSender::new(100, "node-a".to_string()));
        let (tx, rx) = tokio::sync::mpsc::channel(1);

        // Drop receiver to simulate client disconnect
        drop(rx);

        // Should not panic — just return early when send fails
        stream_snapshot(cache, sender, "", 10, tx).await;
    }

    #[tokio::test]
    async fn test_stream_snapshot_expiry_set() {
        let cache = make_cache_with_entries(1);
        let sender = Arc::new(EventSender::new(100, "node-a".to_string()));
        let (tx, mut rx) = tokio::sync::mpsc::channel(256);

        stream_snapshot(cache, sender, "", 100, tx).await;

        let msg = rx.recv().await.unwrap().unwrap();
        // absolute_expiry_ms should be set (fresh_duration + stale_duration > 0)
        assert!(msg.absolute_expiry_ms > 0);
    }

    /// Start a local gRPC PeerService server and return the address.
    async fn start_local_peer_server(
        cache: Arc<CacheStore>,
    ) -> (tokio::task::JoinHandle<()>, String) {
        use crate::proxy::cdc::EventSender;
        use crate::proxy::peer::service::PeerServiceImpl;
        use std::sync::atomic::AtomicU8;

        let sender = Arc::new(EventSender::new(100, "server-node".to_string()));
        let state = Arc::new(AtomicU8::new(super::super::PeerState::Ready as u8));
        let svc = PeerServiceImpl::new(cache, sender, state, 100);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let addr_str = format!("{}", addr);

        let handle = tokio::spawn(async move {
            let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
            tonic::transport::Server::builder()
                .add_service(proto::peer_service_server::PeerServiceServer::new(svc))
                .serve_with_incoming(incoming)
                .await
                .unwrap();
        });

        // Wait for server to be ready
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        (handle, addr_str)
    }

    #[tokio::test]
    async fn test_request_snapshot_from_empty_server() {
        let server_cache = Arc::new(CacheStore::new(
            Duration::from_secs(300),
            Duration::from_secs(600),
            1000,
        ));
        let (_handle, addr) = start_local_peer_server(server_cache).await;

        let local_cache = Arc::new(CacheStore::new(
            Duration::from_secs(300),
            Duration::from_secs(600),
            1000,
        ));

        let applied = request_snapshot(&addr, "local-node", local_cache.clone(), "")
            .await
            .unwrap();
        assert_eq!(applied, 0);
        assert_eq!(local_cache.len(), 0);
    }

    #[tokio::test]
    async fn test_request_snapshot_from_populated_server() {
        let server_cache = make_cache_with_entries(5);
        let (_handle, addr) = start_local_peer_server(server_cache).await;

        let local_cache = Arc::new(CacheStore::new(
            Duration::from_secs(300),
            Duration::from_secs(600),
            1000,
        ));

        let applied = request_snapshot(&addr, "local-node", local_cache.clone(), "")
            .await
            .unwrap();
        assert!(applied > 0);
        assert!(local_cache.len() > 0);
    }

    #[tokio::test]
    async fn test_check_parity_empty_caches() {
        let server_cache = Arc::new(CacheStore::new(
            Duration::from_secs(300),
            Duration::from_secs(600),
            1000,
        ));
        let (_handle, addr) = start_local_peer_server(server_cache).await;

        let local_cache = CacheStore::new(Duration::from_secs(300), Duration::from_secs(600), 1000);

        let (local, remote) = check_parity(&addr, &local_cache).await.unwrap();
        assert_eq!(local, 0);
        assert_eq!(remote, 0);
    }

    #[tokio::test]
    async fn test_check_parity_with_entries() {
        let server_cache = make_cache_with_entries(5);
        let (_handle, addr) = start_local_peer_server(server_cache).await;

        let local_cache = CacheStore::new(Duration::from_secs(300), Duration::from_secs(600), 1000);
        // Add 3 entries locally
        for i in 0..3 {
            let response = QueryResponse {
                results: vec![SearchResult {
                    id: format!("local-{i}"),
                    content: format!("local content-{i}"),
                    score: 0.8,
                    metadata: Default::default(),
                    upstream_id: None,
                }],
                cache_status: crate::proxy::types::CacheStatus::Miss,
                took_ms: 0,
                generated_at: None,
                miss_reason: None,
            };
            local_cache.insert(&format!("local-q-{i}"), response, "up".to_string());
        }

        let (local, remote) = check_parity(&addr, &local_cache).await.unwrap();
        assert_eq!(local, 3);
        assert_eq!(remote, 5);
    }

    #[tokio::test]
    async fn test_request_snapshot_connection_refused() {
        let local_cache = Arc::new(CacheStore::new(
            Duration::from_secs(300),
            Duration::from_secs(600),
            1000,
        ));

        let result = request_snapshot("127.0.0.1:19999", "local-node", local_cache, "").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_check_parity_connection_refused() {
        let local_cache = CacheStore::new(Duration::from_secs(300), Duration::from_secs(600), 1000);

        let result = check_parity("127.0.0.1:19999", &local_cache).await;
        assert!(result.is_err());
    }
}
