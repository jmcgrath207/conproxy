//! PeerReceiver — subscribes to a remote peer's CDC stream and applies events locally.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::proxy::cache::CacheStore;
use crate::proxy::cdc::event::now_ms;
use crate::proxy::cdc::proto;
use crate::proxy::cdc::stream::proto_to_internal;
use crate::proxy::cdc::{CdcEvent, CdcEventType};
use crate::proxy::types::QueryResponse;

/// Dedup result for a received CDC event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeduplicateResult {
    /// Apply the event (new or newer).
    Apply,
    /// Skip — this is our own event (echo).
    Echo,
    /// Skip — local entry is newer or same age.
    Stale,
    /// Skip — event has already expired.
    Expired,
}

/// Check whether a CDC event should be applied to the local cache.
pub fn deduplicate(event: &CdcEvent, local_node_id: &str, cache: &CacheStore) -> DeduplicateResult {
    // Echo prevention: skip our own events
    if event.origin_node_id == local_node_id {
        return DeduplicateResult::Echo;
    }

    // Expiry check: skip events that have already expired
    if event.absolute_expiry_ms > 0 && event.absolute_expiry_ms <= now_ms() {
        return DeduplicateResult::Expired;
    }

    // For inserts, check if local entry is newer (last-write-wins by timestamp)
    if event.event_type == CdcEventType::Insert {
        if let Some(existing) = cache.get(&event.query_key) {
            // Use wall-clock timestamp for LWW comparison — cached_at (monotonic)
            // combined with now_ms() (wall) produces skew under clock drift or
            // process restart. cached_at_wall is set at insert time.
            let local_timestamp_ms = existing
                .cached_at_wall
                .and_then(|w| w.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            if local_timestamp_ms >= event.timestamp_ms {
                return DeduplicateResult::Stale;
            }
        }
    }

    DeduplicateResult::Apply
}

/// Apply a CDC event to the local cache.
///
/// Returns true if the event was applied, false if skipped.
pub fn apply_event(event: &CdcEvent, cache: &CacheStore) -> bool {
    match event.event_type {
        CdcEventType::Insert => {
            if event.payload.is_empty() {
                return false;
            }
            match serde_json::from_slice::<QueryResponse>(&event.payload) {
                Ok(response) => {
                    cache.insert(&event.query_key, response, event.upstream_id.clone());
                    true
                }
                Err(e) => {
                    warn!(error = %e, key = %event.query_key, "Failed to deserialize CDC payload");
                    false
                }
            }
        }
        CdcEventType::Remove => {
            if event.query_key.is_empty() {
                // Clear all
                cache.clear();
            } else {
                cache.remove(&event.query_key);
            }
            true
        }
        // FETCH_START and FETCH_CANCEL are handled by DistributedCoalescer, not here
        CdcEventType::FetchStart | CdcEventType::FetchCancel => false,
    }
}

/// Replication statistics for a single peer connection.
#[derive(Debug, Default)]
pub struct PeerReplicationStats {
    /// Total events received from this peer.
    pub events_received: AtomicU64,
    /// Events applied to local cache.
    pub events_applied: AtomicU64,
    /// Events skipped (echo, stale, expired).
    pub events_skipped: AtomicU64,
    /// Errors during replication.
    pub errors: AtomicU64,
}

impl PeerReplicationStats {
    pub fn snapshot(&self) -> PeerReplicationStatsSnapshot {
        PeerReplicationStatsSnapshot {
            events_received: self.events_received.load(Ordering::Relaxed),
            events_applied: self.events_applied.load(Ordering::Relaxed),
            events_skipped: self.events_skipped.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PeerReplicationStatsSnapshot {
    pub events_received: u64,
    pub events_applied: u64,
    pub events_skipped: u64,
    pub errors: u64,
}

/// PeerReceiver manages the CDC subscription to a single remote peer.
///
/// It connects to the peer's gRPC CdcService, receives events, deduplicates,
/// and applies them to the local cache. Reconnects automatically on failure.
pub struct PeerReceiver {
    peer_addr: String,
    local_node_id: String,
    cache: Arc<CacheStore>,
    cancel: CancellationToken,
    reconnect_interval: Duration,
    stats: Arc<PeerReplicationStats>,
    /// Optional peer shared secret (plan 07) — sent as `x-peer-secret`.
    shared_secret: Option<String>,
}

impl PeerReceiver {
    pub fn new(
        peer_addr: String,
        local_node_id: String,
        cache: Arc<CacheStore>,
        cancel: CancellationToken,
        reconnect_interval: Duration,
    ) -> Self {
        Self::with_secret(
            peer_addr,
            local_node_id,
            cache,
            cancel,
            reconnect_interval,
            None,
        )
    }

    /// Create a receiver that authenticates with a peer shared secret.
    pub fn with_secret(
        peer_addr: String,
        local_node_id: String,
        cache: Arc<CacheStore>,
        cancel: CancellationToken,
        reconnect_interval: Duration,
        shared_secret: Option<String>,
    ) -> Self {
        Self {
            peer_addr,
            local_node_id,
            cache,
            cancel,
            reconnect_interval,
            stats: Arc::new(PeerReplicationStats::default()),
            shared_secret,
        }
    }

    /// Get the replication stats.
    pub fn stats(&self) -> Arc<PeerReplicationStats> {
        self.stats.clone()
    }

    /// Get the peer address.
    pub fn peer_addr(&self) -> &str {
        &self.peer_addr
    }

    /// Run the receiver loop. Reconnects on failure until cancelled.
    pub async fn run(self) {
        loop {
            if self.cancel.is_cancelled() {
                break;
            }

            match self.connect_and_receive().await {
                Ok(()) => {
                    debug!(peer = %self.peer_addr, "CDC stream ended normally");
                }
                Err(e) => {
                    warn!(peer = %self.peer_addr, error = %e, "CDC stream error");
                    self.stats.errors.fetch_add(1, Ordering::Relaxed);
                }
            }

            // Wait before reconnecting
            tokio::select! {
                _ = sleep(self.reconnect_interval) => {},
                _ = self.cancel.cancelled() => break,
            }
        }

        info!(peer = %self.peer_addr, "Peer receiver stopped");
    }

    async fn connect_and_receive(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let endpoint = format!("http://{}", self.peer_addr);
        let channel = tonic::transport::Endpoint::from_shared(endpoint)?
            .timeout(Duration::from_secs(30))
            .connect()
            .await?;

        let mut client = proto::cdc_service_client::CdcServiceClient::new(channel);

        let request = proto::CdcSubscribeRequest {
            consumer_id: format!("peer-{}", self.local_node_id),
            event_types: vec![], // All events
        };
        let mut request = tonic::Request::new(request);
        if let Some(ref secret) = self.shared_secret {
            request = crate::proxy::grpc::middleware::insert_peer_secret_metadata(request, secret);
        }

        let mut stream = client.subscribe(request).await?.into_inner();

        info!(peer = %self.peer_addr, "Connected to peer CDC stream");

        loop {
            tokio::select! {
                msg = stream.message() => {
                    match msg {
                        Ok(Some(proto_event)) => {
                            self.stats.events_received.fetch_add(1, Ordering::Relaxed);
                            self.handle_event(proto_event);
                        }
                        Ok(None) => {
                            // Stream ended
                            break;
                        }
                        Err(e) => {
                            return Err(e.into());
                        }
                    }
                }
                _ = self.cancel.cancelled() => break,
            }
        }

        Ok(())
    }

    fn handle_event(&self, proto_event: proto::CdcEvent) {
        let Some(event) = proto_to_internal(&proto_event) else {
            return;
        };

        let result = deduplicate(&event, &self.local_node_id, &self.cache);
        match result {
            DeduplicateResult::Apply if apply_event(&event, &self.cache) => {
                self.stats.events_applied.fetch_add(1, Ordering::Relaxed);
            }
            _ => {
                self.stats.events_skipped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::proxy::cdc::event::CdcEvent;
    use crate::proxy::types::{CacheStatus, QueryResponse, SearchResult};

    fn make_cache() -> Arc<CacheStore> {
        Arc::new(CacheStore::new(
            Duration::from_secs(300),
            Duration::from_secs(600),
            1000,
        ))
    }

    fn make_insert_event(key: &str, origin: &str, timestamp_ms: u64) -> CdcEvent {
        let response = QueryResponse {
            results: vec![SearchResult {
                id: key.to_string(),
                content: "test".to_string(),
                score: 0.9,
                metadata: Default::default(),
                upstream_id: None,
            }],
            cache_status: CacheStatus::Miss,
            took_ms: 0,
            generated_at: None,
            miss_reason: None,
        };
        CdcEvent {
            sequence: 1,
            timestamp_ms,
            event_type: CdcEventType::Insert,
            query_key: key.to_string(),
            payload: serde_json::to_vec(&response).unwrap(),
            upstream_id: "up-1".to_string(),
            context_id: String::new(),
            origin_node_id: origin.to_string(),
            absolute_expiry_ms: 0,
        }
    }

    fn make_remove_event(key: &str, origin: &str) -> CdcEvent {
        CdcEvent {
            sequence: 1,
            timestamp_ms: now_ms(),
            event_type: CdcEventType::Remove,
            query_key: key.to_string(),
            payload: vec![],
            upstream_id: String::new(),
            context_id: String::new(),
            origin_node_id: origin.to_string(),
            absolute_expiry_ms: 0,
        }
    }

    #[test]
    fn test_dedup_echo_prevention() {
        let cache = make_cache();
        let event = make_insert_event("q1", "node-a", now_ms());
        assert_eq!(
            deduplicate(&event, "node-a", &cache),
            DeduplicateResult::Echo
        );
    }

    #[test]
    fn test_dedup_expired_event() {
        let cache = make_cache();
        let mut event = make_insert_event("q1", "node-b", now_ms());
        event.absolute_expiry_ms = 1; // epoch ms 1 = expired
        assert_eq!(
            deduplicate(&event, "node-a", &cache),
            DeduplicateResult::Expired
        );
    }

    #[test]
    fn test_dedup_apply_new_event() {
        let cache = make_cache();
        let event = make_insert_event("q1", "node-b", now_ms());
        assert_eq!(
            deduplicate(&event, "node-a", &cache),
            DeduplicateResult::Apply
        );
    }

    #[test]
    fn test_dedup_stale_event() {
        let cache = make_cache();
        // Insert locally first
        let response = QueryResponse {
            results: vec![],
            cache_status: CacheStatus::Miss,
            took_ms: 0,
            generated_at: None,
            miss_reason: None,
        };
        cache.insert("q1", response, "up-1".to_string());

        // Older event from peer
        let event = make_insert_event("q1", "node-b", 1); // timestamp = 1ms (very old)
        assert_eq!(
            deduplicate(&event, "node-a", &cache),
            DeduplicateResult::Stale
        );
    }

    #[test]
    fn test_apply_insert_event() {
        let cache = make_cache();
        let event = make_insert_event("q1", "node-b", now_ms());
        assert!(apply_event(&event, &cache));
        assert!(cache.get("q1").is_some());
    }

    #[test]
    fn test_apply_remove_event() {
        let cache = make_cache();
        // Insert first
        let response = QueryResponse {
            results: vec![],
            cache_status: CacheStatus::Miss,
            took_ms: 0,
            generated_at: None,
            miss_reason: None,
        };
        cache.insert("q1", response, "up-1".to_string());
        assert!(cache.get("q1").is_some());

        // Remove
        let event = make_remove_event("q1", "node-b");
        assert!(apply_event(&event, &cache));
        assert!(cache.get("q1").is_none());
    }

    #[test]
    fn test_apply_clear_event() {
        let cache = make_cache();
        let response = QueryResponse {
            results: vec![],
            cache_status: CacheStatus::Miss,
            took_ms: 0,
            generated_at: None,
            miss_reason: None,
        };
        cache.insert("q1", response, "up-1".to_string());

        let mut event = make_remove_event("", "node-b");
        event.query_key = String::new(); // empty = clear all
        assert!(apply_event(&event, &cache));
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn test_apply_insert_bad_payload() {
        let cache = make_cache();
        let mut event = make_insert_event("q1", "node-b", now_ms());
        event.payload = b"not json".to_vec();
        assert!(!apply_event(&event, &cache));
    }

    #[test]
    fn test_apply_fetch_events_are_noop() {
        let cache = make_cache();
        let event = CdcEvent {
            sequence: 1,
            timestamp_ms: now_ms(),
            event_type: CdcEventType::FetchStart,
            query_key: "q1".to_string(),
            payload: vec![],
            upstream_id: String::new(),
            context_id: String::new(),
            origin_node_id: "node-b".to_string(),
            absolute_expiry_ms: 0,
        };
        assert!(!apply_event(&event, &cache));
    }

    #[test]
    fn test_stats_snapshot() {
        let stats = PeerReplicationStats::default();
        stats.events_received.store(10, Ordering::Relaxed);
        stats.events_applied.store(7, Ordering::Relaxed);
        stats.events_skipped.store(3, Ordering::Relaxed);
        let snap = stats.snapshot();
        assert_eq!(snap.events_received, 10);
        assert_eq!(snap.events_applied, 7);
        assert_eq!(snap.events_skipped, 3);
    }

    #[test]
    fn test_apply_insert_empty_payload() {
        let cache = make_cache();
        let mut event = make_insert_event("q1", "node-b", now_ms());
        event.payload = vec![]; // empty payload
        assert!(!apply_event(&event, &cache));
        assert!(cache.get("q1").is_none());
    }

    #[test]
    fn test_apply_fetch_cancel_is_noop() {
        let cache = make_cache();
        let event = CdcEvent {
            sequence: 1,
            timestamp_ms: now_ms(),
            event_type: CdcEventType::FetchCancel,
            query_key: "q1".to_string(),
            payload: vec![],
            upstream_id: String::new(),
            context_id: String::new(),
            origin_node_id: "node-b".to_string(),
            absolute_expiry_ms: 0,
        };
        assert!(!apply_event(&event, &cache));
    }

    #[test]
    fn test_dedup_remove_event_from_other_node() {
        let cache = make_cache();
        let event = make_remove_event("q1", "node-b");
        // Remove events from other nodes should always Apply
        assert_eq!(
            deduplicate(&event, "node-a", &cache),
            DeduplicateResult::Apply
        );
    }

    #[test]
    fn test_dedup_remove_event_echo() {
        let cache = make_cache();
        let event = make_remove_event("q1", "node-a");
        // Remove events from our own node should be Echo
        assert_eq!(
            deduplicate(&event, "node-a", &cache),
            DeduplicateResult::Echo
        );
    }

    #[test]
    fn test_dedup_insert_no_expiry_not_expired() {
        let cache = make_cache();
        let mut event = make_insert_event("q1", "node-b", now_ms());
        event.absolute_expiry_ms = 0; // disabled expiry
        assert_eq!(
            deduplicate(&event, "node-a", &cache),
            DeduplicateResult::Apply
        );
    }

    #[test]
    fn test_peer_receiver_creation_and_accessors() {
        let cache = make_cache();
        let cancel = CancellationToken::new();
        let receiver = PeerReceiver::new(
            "peer-b:9090".to_string(),
            "node-a".to_string(),
            cache,
            cancel,
            Duration::from_secs(5),
        );

        assert_eq!(receiver.peer_addr(), "peer-b:9090");
        let stats = receiver.stats();
        let snap = stats.snapshot();
        assert_eq!(snap.events_received, 0);
        assert_eq!(snap.events_applied, 0);
        assert_eq!(snap.events_skipped, 0);
        assert_eq!(snap.errors, 0);
    }

    #[test]
    fn test_stats_errors_count() {
        let stats = PeerReplicationStats::default();
        stats.errors.store(5, Ordering::Relaxed);
        let snap = stats.snapshot();
        assert_eq!(snap.errors, 5);
    }

    #[test]
    fn test_stats_snapshot_serialization() {
        let snap = PeerReplicationStatsSnapshot {
            events_received: 100,
            events_applied: 80,
            events_skipped: 15,
            errors: 5,
        };
        let json = serde_json::to_value(&snap).unwrap();
        assert_eq!(json["events_received"], 100);
        assert_eq!(json["events_applied"], 80);
        assert_eq!(json["events_skipped"], 15);
        assert_eq!(json["errors"], 5);
    }

    #[tokio::test]
    async fn test_peer_receiver_run_cancelled_immediately() {
        let cache = make_cache();
        let cancel = CancellationToken::new();
        cancel.cancel(); // Cancel before run

        let receiver = PeerReceiver::new(
            "127.0.0.1:19999".to_string(),
            "node-a".to_string(),
            cache,
            cancel,
            Duration::from_millis(10),
        );

        // Should exit immediately due to cancellation
        receiver.run().await;
    }

    #[tokio::test]
    async fn test_peer_receiver_run_connect_failure_then_cancel() {
        let cache = make_cache();
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();

        let receiver = PeerReceiver::new(
            "127.0.0.1:19998".to_string(), // nothing listening
            "node-a".to_string(),
            cache,
            cancel,
            Duration::from_millis(10),
        );

        let handle = tokio::spawn(async move {
            receiver.run().await;
        });

        // Let it attempt connection + get error + wait for reconnect
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel2.cancel();
        handle.await.unwrap();
    }

    #[test]
    fn test_handle_event_insert_from_peer() {
        let cache = make_cache();
        let cancel = CancellationToken::new();
        let receiver = PeerReceiver::new(
            "peer:9090".to_string(),
            "node-a".to_string(),
            cache.clone(),
            cancel,
            Duration::from_secs(5),
        );

        let event = make_insert_event("q-handle", "node-b", now_ms());
        let proto_event = crate::proxy::cdc::stream::internal_to_proto(&event);
        receiver.handle_event(proto_event);

        assert!(cache.get("q-handle").is_some());
        let stats = receiver.stats();
        assert_eq!(stats.events_applied.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_handle_event_echo_from_self() {
        let cache = make_cache();
        let cancel = CancellationToken::new();
        let receiver = PeerReceiver::new(
            "peer:9090".to_string(),
            "node-a".to_string(),
            cache.clone(),
            cancel,
            Duration::from_secs(5),
        );

        let event = make_insert_event("q-echo", "node-a", now_ms()); // same origin as local
        let proto_event = crate::proxy::cdc::stream::internal_to_proto(&event);
        receiver.handle_event(proto_event);

        assert!(cache.get("q-echo").is_none());
        let stats = receiver.stats();
        assert_eq!(stats.events_skipped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_handle_event_bad_payload_skipped() {
        let cache = make_cache();
        let cancel = CancellationToken::new();
        let receiver = PeerReceiver::new(
            "peer:9090".to_string(),
            "node-a".to_string(),
            cache,
            cancel,
            Duration::from_secs(5),
        );

        let mut event = make_insert_event("q-bad", "node-b", now_ms());
        event.payload = b"not json".to_vec();
        let proto_event = crate::proxy::cdc::stream::internal_to_proto(&event);
        receiver.handle_event(proto_event);

        let stats = receiver.stats();
        assert_eq!(stats.events_skipped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_handle_event_remove() {
        let cache = make_cache();
        // Pre-insert
        let response = QueryResponse {
            results: vec![],
            cache_status: CacheStatus::Miss,
            took_ms: 0,
            generated_at: None,
            miss_reason: None,
        };
        cache.insert("q-remove", response, "up".to_string());
        assert!(cache.get("q-remove").is_some());

        let cancel = CancellationToken::new();
        let receiver = PeerReceiver::new(
            "peer:9090".to_string(),
            "node-a".to_string(),
            cache.clone(),
            cancel,
            Duration::from_secs(5),
        );

        let event = make_remove_event("q-remove", "node-b");
        let proto_event = crate::proxy::cdc::stream::internal_to_proto(&event);
        receiver.handle_event(proto_event);

        assert!(cache.get("q-remove").is_none());
        let stats = receiver.stats();
        assert_eq!(stats.events_applied.load(Ordering::Relaxed), 1);
    }
}
