//! Peer-to-peer cache replication module.
//!
//! Consumes the generic CDC event stream to replicate cache entries between
//! peers. Each peer subscribes to the others' CDC streams and applies
//! received events with dedup (echo prevention, timestamp-based LWW).
//!
//! # Components
//!
//! - `PeerManager` — Facade that coordinates all peer operations
//! - `PeerReceiver` — Subscribes to a remote peer's CDC stream, applies events
//! - `DistributedCoalescer` — Cross-peer fetch coordination (FETCH_START/CANCEL)
//! - `PeerServiceImpl` — gRPC GetStatus + Snapshot RPCs
//! - Snapshot — Server streams cache entries, client applies on rejoin

pub mod receiver;
pub mod service;
pub mod singleflight;
pub mod snapshot;

pub use receiver::{
    apply_event, deduplicate, DeduplicateResult, PeerReceiver, PeerReplicationStats,
    PeerReplicationStatsSnapshot,
};
pub use service::PeerServiceImpl;
pub use singleflight::{CoalesceDecision, DistributedCoalescer};

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::config::PeerConfig;
use crate::proxy::cache::CacheStore;
use crate::proxy::cdc::SharedEventSender;

/// Peer state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerState {
    /// Cache is being populated from snapshot. `/ready` returns 503.
    Warming = 0,
    /// Cache has reached parity. `/ready` returns 200.
    Ready = 1,
}

impl PeerState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Warming => "warming",
            Self::Ready => "ready",
        }
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Ready,
            _ => Self::Warming,
        }
    }
}

/// PeerManager coordinates peer-to-peer cache replication.
pub struct PeerManager {
    config: PeerConfig,
    cache: Arc<CacheStore>,
    event_sender: SharedEventSender,
    state: Arc<AtomicU8>,
    cancel: CancellationToken,
    coalescer: Arc<DistributedCoalescer>,
}

impl PeerManager {
    /// Create a new PeerManager.
    pub fn new(
        config: PeerConfig,
        cache: Arc<CacheStore>,
        event_sender: SharedEventSender,
        cancel: CancellationToken,
    ) -> Self {
        let wait_timeout = Duration::from_millis(config.fetch_wait_timeout_ms());
        let coalescer = Arc::new(DistributedCoalescer::new(
            event_sender.clone(),
            wait_timeout,
        ));

        // Start as Ready if no peers configured (fresh deployment)
        let initial_state = if config.peers().is_empty() || !config.snapshot_on_join() {
            PeerState::Ready
        } else {
            PeerState::Warming
        };

        Self {
            config,
            cache,
            event_sender,
            state: Arc::new(AtomicU8::new(initial_state as u8)),
            cancel,
            coalescer,
        }
    }

    /// Get the current peer state.
    pub fn state(&self) -> PeerState {
        PeerState::from_u8(self.state.load(Ordering::Relaxed))
    }

    /// Check if this peer is ready to serve traffic.
    pub fn is_ready(&self) -> bool {
        self.state() == PeerState::Ready
    }

    /// Get the node ID.
    pub fn node_id(&self) -> String {
        self.config.node_id()
    }

    /// Get the distributed coalescer.
    pub fn coalescer(&self) -> &Arc<DistributedCoalescer> {
        &self.coalescer
    }

    /// Get the state atomic for gRPC service.
    pub fn state_atomic(&self) -> Arc<AtomicU8> {
        self.state.clone()
    }

    /// Create the gRPC PeerServiceImpl.
    pub fn grpc_service(&self) -> PeerServiceImpl {
        PeerServiceImpl::new(
            self.cache.clone(),
            self.event_sender.clone(),
            self.state.clone(),
            self.config.snapshot_batch_size(),
        )
    }

    /// Start all peer operations (receivers + snapshot).
    ///
    /// This spawns background tasks for each peer and handles snapshot resync.
    pub async fn start(&self) {
        let peers = self.config.peers();
        if peers.is_empty() {
            info!("No peers configured, peer replication inactive");
            return;
        }

        let node_id = self.config.node_id();
        let reconnect = Duration::from_millis(self.config.reconnect_interval_ms());
        let shared_secret = self.config.resolve_shared_secret().ok().flatten();

        // Spawn a receiver for each peer
        for peer_addr in peers {
            let receiver = PeerReceiver::with_secret(
                peer_addr.clone(),
                node_id.clone(),
                self.cache.clone(),
                self.cancel.clone(),
                reconnect,
                shared_secret.clone(),
            );
            info!(peer = %peer_addr, "Starting peer receiver");
            tokio::spawn(receiver.run());
        }

        // Snapshot resync on join
        if self.config.snapshot_on_join() && !peers.is_empty() {
            let cache = self.cache.clone();
            let node_id = node_id.clone();
            let threshold = self.config.ready_threshold();
            let state = self.state.clone();
            let peers = peers.to_vec();
            let secret = shared_secret.clone();

            tokio::spawn(async move {
                for peer_addr in &peers {
                    match snapshot::request_snapshot_with_secret(
                        peer_addr,
                        &node_id,
                        cache.clone(),
                        "",
                        secret.as_deref(),
                    )
                    .await
                    {
                        Ok(applied) => {
                            info!(peer = %peer_addr, applied, "Snapshot received");

                            // Check parity
                            match snapshot::check_parity_with_secret(
                                peer_addr,
                                &cache,
                                secret.as_deref(),
                            )
                            .await
                            {
                                Ok((local, remote)) => {
                                    let ratio = if remote > 0 {
                                        local as f64 / remote as f64
                                    } else {
                                        1.0 // Empty peer = ready
                                    };
                                    if ratio >= threshold {
                                        state.store(PeerState::Ready as u8, Ordering::Relaxed);
                                        info!(
                                            local,
                                            remote, ratio, "Parity reached, peer is READY"
                                        );
                                        return;
                                    }
                                }
                                Err(e) => {
                                    warn!(error = %e, "Failed to check parity");
                                }
                            }
                        }
                        Err(e) => {
                            warn!(peer = %peer_addr, error = %e, "Snapshot request failed");
                        }
                    }
                }

                // If we get here, no snapshot succeeded — mark ready anyway (degrade to cold cache)
                state.store(PeerState::Ready as u8, Ordering::Relaxed);
                warn!("No snapshot source available, starting with cold cache");
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::cdc::EventSender;

    fn make_peer_manager(peers: Vec<String>) -> PeerManager {
        let cache = Arc::new(CacheStore::new(
            Duration::from_secs(300),
            Duration::from_secs(600),
            1000,
        ));
        let sender = Arc::new(EventSender::new(100, "test-node".to_string()));
        let cancel = CancellationToken::new();
        let config = PeerConfig {
            enabled: Some(true),
            node_id: Some("test-node".to_string()),
            peers,
            snapshot_on_join: Some(true),
            ready_threshold: Some(0.8),
            ..Default::default()
        };
        PeerManager::new(config, cache, sender, cancel)
    }

    #[test]
    fn test_initial_state_no_peers() {
        let pm = make_peer_manager(vec![]);
        assert_eq!(pm.state(), PeerState::Ready);
        assert!(pm.is_ready());
    }

    #[test]
    fn test_initial_state_with_peers() {
        let pm = make_peer_manager(vec!["peer-b:9090".to_string()]);
        assert_eq!(pm.state(), PeerState::Warming);
        assert!(!pm.is_ready());
    }

    #[test]
    fn test_node_id() {
        let pm = make_peer_manager(vec![]);
        assert_eq!(pm.node_id(), "test-node");
    }

    #[test]
    fn test_initial_state_no_snapshot() {
        let cache = Arc::new(CacheStore::new(
            Duration::from_secs(300),
            Duration::from_secs(600),
            1000,
        ));
        let sender = Arc::new(EventSender::new(100, "n".to_string()));
        let config = PeerConfig {
            enabled: Some(true),
            peers: vec!["peer:9090".to_string()],
            snapshot_on_join: Some(false),
            ..Default::default()
        };
        let pm = PeerManager::new(config, cache, sender, CancellationToken::new());
        assert_eq!(pm.state(), PeerState::Ready);
    }

    #[test]
    fn test_coalescer_accessible() {
        let pm = make_peer_manager(vec![]);
        assert_eq!(pm.coalescer().active_fetch_count(), 0);
    }

    #[test]
    fn test_state_atomic_accessor() {
        let pm = make_peer_manager(vec![]);
        let atomic = pm.state_atomic();
        assert_eq!(
            PeerState::from_u8(atomic.load(Ordering::Relaxed)),
            PeerState::Ready
        );
    }

    #[test]
    fn test_grpc_service_creation() {
        let pm = make_peer_manager(vec![]);
        // grpc_service() should return a valid PeerServiceImpl without panicking
        let _service = pm.grpc_service();
    }

    #[tokio::test]
    async fn test_start_no_peers_returns_immediately() {
        let pm = make_peer_manager(vec![]);
        // Should return immediately since no peers are configured
        pm.start().await;
        // State should remain Ready
        assert!(pm.is_ready());
    }

    #[test]
    fn test_peer_state_display() {
        assert_eq!(PeerState::Warming.as_str(), "warming");
        assert_eq!(PeerState::Ready.as_str(), "ready");
    }

    #[test]
    fn test_peer_state_from_u8_all_values() {
        assert_eq!(PeerState::from_u8(0), PeerState::Warming);
        assert_eq!(PeerState::from_u8(1), PeerState::Ready);
        assert_eq!(PeerState::from_u8(2), PeerState::Warming); // unknown
        assert_eq!(PeerState::from_u8(255), PeerState::Warming); // unknown
    }

    #[test]
    fn test_coalescer_wait_timeout() {
        let pm = make_peer_manager(vec![]);
        // Default fetch_wait_timeout_ms
        let timeout = pm.coalescer().wait_timeout();
        assert!(timeout.as_millis() > 0);
    }

    #[tokio::test]
    async fn test_start_with_peers_spawns_receivers() {
        // Peer won't connect (nothing listening) but start() should complete
        // and spawn tasks without panicking
        let cache = Arc::new(CacheStore::new(
            Duration::from_secs(300),
            Duration::from_secs(600),
            1000,
        ));
        let sender = Arc::new(EventSender::new(100, "test-node".to_string()));
        let cancel = CancellationToken::new();
        let config = PeerConfig {
            enabled: Some(true),
            node_id: Some("test-node".to_string()),
            peers: vec!["127.0.0.1:19997".to_string()],
            snapshot_on_join: Some(true),
            ready_threshold: Some(0.8),
            reconnect_interval_ms: Some(50),
            ..Default::default()
        };
        let pm = PeerManager::new(config, cache, sender, cancel.clone());

        // start() spawns background tasks and returns
        pm.start().await;

        // Let the spawned tasks run briefly (they'll fail to connect)
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Cancel to stop background tasks
        cancel.cancel();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn test_start_with_peers_no_snapshot() {
        let cache = Arc::new(CacheStore::new(
            Duration::from_secs(300),
            Duration::from_secs(600),
            1000,
        ));
        let sender = Arc::new(EventSender::new(100, "test-node".to_string()));
        let cancel = CancellationToken::new();
        let config = PeerConfig {
            enabled: Some(true),
            node_id: Some("test-node".to_string()),
            peers: vec!["127.0.0.1:19996".to_string()],
            snapshot_on_join: Some(false),
            reconnect_interval_ms: Some(50),
            ..Default::default()
        };
        let pm = PeerManager::new(config, cache, sender, cancel.clone());

        pm.start().await;

        // State should be Ready (no snapshot = already ready)
        assert!(pm.is_ready());

        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
    }

    fn make_peer_manager_with_options(
        peers: Vec<String>,
        snapshot_on_join: bool,
    ) -> (PeerManager, CancellationToken) {
        let cache = Arc::new(CacheStore::new(
            Duration::from_secs(300),
            Duration::from_secs(600),
            1000,
        ));
        let sender = Arc::new(EventSender::new(100, "test-node".to_string()));
        let cancel = CancellationToken::new();
        let config = PeerConfig {
            enabled: Some(true),
            node_id: Some("test-node".to_string()),
            peers,
            snapshot_on_join: Some(snapshot_on_join),
            ready_threshold: Some(0.8),
            reconnect_interval_ms: Some(50),
            ..Default::default()
        };
        let pm = PeerManager::new(config, cache, sender, cancel.clone());
        (pm, cancel)
    }

    #[tokio::test]
    async fn test_start_multiple_peers() {
        let (pm, cancel) = make_peer_manager_with_options(
            vec!["127.0.0.1:19994".to_string(), "127.0.0.1:19995".to_string()],
            true,
        );

        pm.start().await;

        // Let tasks run
        tokio::time::sleep(Duration::from_millis(300)).await;

        // After snapshot failures, state transitions to Ready (cold cache fallback)
        // This covers lines 207-209
        tokio::time::sleep(Duration::from_millis(500)).await;

        cancel.cancel();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
