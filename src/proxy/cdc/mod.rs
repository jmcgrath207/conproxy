//! CDC (Change Data Capture) module for cache mutation events.
//!
//! Provides a generic event stream for cache mutations. Any consumer can
//! subscribe via gRPC (`CdcService.Subscribe`). Peer replication is one
//! built-in consumer; external programs can also connect for audit,
//! analytics, webhooks, or custom sync.
//!
//! # Architecture
//!
//! ```text
//! CacheStore mutations
//!     │
//!     ▼
//! CDC Event Channel (tokio::broadcast)
//!     │
//!     ├──► Peer Replication (built-in consumer)
//!     ├──► gRPC CdcStream (external consumers)
//!     └──► Metrics observer (replication lag, event rates)
//! ```

pub mod event;
pub mod stream;

pub use event::{CdcEvent, CdcEventBuilder, CdcEventType, EventSender, SharedEventSender};
pub use stream::{internal_to_proto, proto_to_internal, CdcServiceImpl};

use std::sync::Arc;

use crate::config::CdcConfig;

/// Generated protobuf types for conproxy.cdc.v1.
pub mod proto {
    tonic::include_proto!("conproxy.cdc.v1");
}

/// CdcManager owns the EventSender and exposes the gRPC service.
pub struct CdcManager {
    event_sender: Arc<EventSender>,
}

impl CdcManager {
    /// Create a new CdcManager from configuration.
    pub fn new(config: &CdcConfig, node_id: String) -> Self {
        let event_sender = Arc::new(EventSender::new(config.buffer_size(), node_id));
        Self { event_sender }
    }

    /// Get a shared reference to the event sender (for CacheStore hooks).
    pub fn event_sender(&self) -> Arc<EventSender> {
        self.event_sender.clone()
    }

    /// Create the gRPC CdcServiceImpl for registration.
    pub fn grpc_service(&self) -> CdcServiceImpl {
        CdcServiceImpl::new(self.event_sender.clone())
    }

    /// Get the current sequence number.
    pub fn current_sequence(&self) -> u64 {
        self.event_sender.current_sequence()
    }

    /// Get the number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.event_sender.receiver_count()
    }

    /// Get the node ID.
    pub fn node_id(&self) -> &str {
        self.event_sender.node_id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cdc_manager_creation() {
        let config = CdcConfig {
            enabled: Some(true),
            buffer_size: Some(500),
        };
        let manager = CdcManager::new(&config, "test-node".to_string());
        assert_eq!(manager.node_id(), "test-node");
        assert_eq!(manager.current_sequence(), 1);
        assert_eq!(manager.subscriber_count(), 0);
    }

    #[test]
    fn test_cdc_manager_event_sender_shared() {
        let config = CdcConfig::default();
        let manager = CdcManager::new(&config, "n".to_string());
        let sender1 = manager.event_sender();
        let sender2 = manager.event_sender();
        // Both point to the same underlying sender
        assert_eq!(Arc::as_ptr(&sender1), Arc::as_ptr(&sender2));
    }

    #[test]
    fn test_cdc_sequence_increments_on_send() {
        let config = CdcConfig::default();
        let manager = CdcManager::new(&config, "n".to_string());
        let sender = manager.event_sender();
        let _rx = sender.subscribe(); // 1 subscriber
        let initial = manager.current_sequence();
        let count = sender.send(
            CdcEventType::Insert,
            CdcEventBuilder::insert("k".to_string(), b"p".to_vec(), "u".to_string()),
        );
        assert_eq!(count, 1, "first send should reach 1 subscriber");
        assert!(
            manager.current_sequence() > initial,
            "sequence should increment"
        );
    }

    #[test]
    fn test_cdc_subscriber_count_tracking() {
        let config = CdcConfig::default();
        let manager = CdcManager::new(&config, "n".to_string());
        let sender = manager.event_sender();
        assert_eq!(manager.subscriber_count(), 0);

        let _r1 = sender.subscribe();
        assert_eq!(manager.subscriber_count(), 1);
        let _r2 = sender.subscribe();
        assert_eq!(manager.subscriber_count(), 2);
        let _r3 = sender.subscribe();
        assert_eq!(manager.subscriber_count(), 3);
    }

    #[test]
    fn test_cdc_grpc_service_construction() {
        let config = CdcConfig::default();
        let manager = CdcManager::new(&config, "test-node".to_string());
        // grpc_service() should be callable repeatedly and return new impl
        let _svc1 = manager.grpc_service();
        let _svc2 = manager.grpc_service();
        // No panic → success
    }

    #[test]
    fn test_cdc_node_id_preserved() {
        let manager = CdcManager::new(&CdcConfig::default(), "node-alpha".to_string());
        assert_eq!(manager.node_id(), "node-alpha");

        let manager2 = CdcManager::new(&CdcConfig::default(), "node-beta".to_string());
        assert_eq!(manager2.node_id(), "node-beta");
    }
}
