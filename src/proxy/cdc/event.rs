//! CDC event types and broadcast channel sender.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::broadcast;

/// CDC event type matching the protobuf enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CdcEventType {
    Insert,
    Remove,
    FetchStart,
    FetchCancel,
}

impl CdcEventType {
    /// Convert to protobuf enum value.
    pub fn to_proto(self) -> i32 {
        match self {
            Self::Insert => 1,
            Self::Remove => 2,
            Self::FetchStart => 3,
            Self::FetchCancel => 4,
        }
    }

    /// Convert from protobuf enum value.
    pub fn from_proto(value: i32) -> Option<Self> {
        match value {
            1 => Some(Self::Insert),
            2 => Some(Self::Remove),
            3 => Some(Self::FetchStart),
            4 => Some(Self::FetchCancel),
            _ => None,
        }
    }

    /// String name for filtering.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Insert => "insert",
            Self::Remove => "remove",
            Self::FetchStart => "fetch_start",
            Self::FetchCancel => "fetch_cancel",
        }
    }
}

/// A CDC event representing a cache mutation.
#[derive(Debug, Clone)]
pub struct CdcEvent {
    /// Per-node monotonic sequence number.
    pub sequence: u64,
    /// Wall-clock epoch milliseconds.
    pub timestamp_ms: u64,
    /// Type of mutation.
    pub event_type: CdcEventType,
    /// Cache key (query text or hash).
    pub query_key: String,
    /// JSON-serialized payload (INSERT only).
    pub payload: Vec<u8>,
    /// Upstream that produced the result.
    pub upstream_id: String,
    /// Context ID for multi-context isolation.
    pub context_id: String,
    /// Node that originated this event.
    pub origin_node_id: String,
    /// Absolute wall-clock expiry (0 = no expiry).
    pub absolute_expiry_ms: u64,
}

impl CdcEvent {
    /// Check if this event has expired based on current wall clock.
    pub fn is_expired(&self) -> bool {
        if self.absolute_expiry_ms == 0 {
            return false;
        }
        now_ms() >= self.absolute_expiry_ms
    }
}

/// Get current wall-clock epoch milliseconds.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Broadcast-based CDC event sender.
///
/// Wraps a `tokio::sync::broadcast` channel. Sending when no receivers
/// are subscribed is a no-op (the event is dropped). A monotonic sequence
/// counter is maintained per-sender for gap detection.
pub struct EventSender {
    tx: broadcast::Sender<CdcEvent>,
    sequence: AtomicU64,
    node_id: String,
}

impl EventSender {
    /// Create a new event sender with the given buffer capacity.
    pub fn new(buffer_size: usize, node_id: String) -> Self {
        let (tx, _) = broadcast::channel(buffer_size);
        Self {
            tx,
            sequence: AtomicU64::new(1),
            node_id,
        }
    }

    /// Publish a CDC event. Returns the number of receivers that got it.
    /// Returns 0 if no receivers are subscribed (no-op).
    pub fn send(&self, event_type: CdcEventType, event: CdcEventBuilder) -> usize {
        let seq = self.sequence.fetch_add(1, Ordering::Relaxed);
        let cdc_event = CdcEvent {
            sequence: seq,
            timestamp_ms: now_ms(),
            event_type,
            query_key: event.query_key,
            payload: event.payload,
            upstream_id: event.upstream_id,
            context_id: event.context_id,
            origin_node_id: self.node_id.clone(),
            absolute_expiry_ms: event.absolute_expiry_ms,
        };
        self.tx.send(cdc_event).unwrap_or(0)
    }

    /// Subscribe to the event stream. Returns a receiver.
    pub fn subscribe(&self) -> broadcast::Receiver<CdcEvent> {
        self.tx.subscribe()
    }

    /// Get the current sequence number.
    pub fn current_sequence(&self) -> u64 {
        self.sequence.load(Ordering::Relaxed)
    }

    /// Get the node ID.
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Get the number of active receivers.
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

/// Builder for constructing CDC events with only the fields needed.
pub struct CdcEventBuilder {
    pub query_key: String,
    pub payload: Vec<u8>,
    pub upstream_id: String,
    pub context_id: String,
    pub absolute_expiry_ms: u64,
}

impl CdcEventBuilder {
    /// Create a builder for an INSERT event.
    pub fn insert(query_key: String, payload: Vec<u8>, upstream_id: String) -> Self {
        Self {
            query_key,
            payload,
            upstream_id,
            context_id: String::new(),
            absolute_expiry_ms: 0,
        }
    }

    /// Create a builder for a REMOVE event.
    pub fn remove(query_key: String) -> Self {
        Self {
            query_key,
            payload: Vec::new(),
            upstream_id: String::new(),
            context_id: String::new(),
            absolute_expiry_ms: 0,
        }
    }

    /// Create a builder for FETCH_START.
    pub fn fetch_start(query_key: String) -> Self {
        Self {
            query_key,
            payload: Vec::new(),
            upstream_id: String::new(),
            context_id: String::new(),
            absolute_expiry_ms: 0,
        }
    }

    /// Create a builder for FETCH_CANCEL.
    pub fn fetch_cancel(query_key: String) -> Self {
        Self {
            query_key,
            payload: Vec::new(),
            upstream_id: String::new(),
            context_id: String::new(),
            absolute_expiry_ms: 0,
        }
    }

    /// Set the context ID.
    pub fn with_context(mut self, context_id: String) -> Self {
        self.context_id = context_id;
        self
    }

    /// Set the absolute expiry.
    pub fn with_expiry(mut self, absolute_expiry_ms: u64) -> Self {
        self.absolute_expiry_ms = absolute_expiry_ms;
        self
    }
}

/// Shared reference to an EventSender.
pub type SharedEventSender = Arc<EventSender>;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn test_event_type_roundtrip() {
        for et in [
            CdcEventType::Insert,
            CdcEventType::Remove,
            CdcEventType::FetchStart,
            CdcEventType::FetchCancel,
        ] {
            assert_eq!(CdcEventType::from_proto(et.to_proto()), Some(et));
        }
        assert_eq!(CdcEventType::from_proto(0), None);
        assert_eq!(CdcEventType::from_proto(99), None);
    }

    #[test]
    fn test_event_type_as_str() {
        assert_eq!(CdcEventType::Insert.as_str(), "insert");
        assert_eq!(CdcEventType::Remove.as_str(), "remove");
        assert_eq!(CdcEventType::FetchStart.as_str(), "fetch_start");
        assert_eq!(CdcEventType::FetchCancel.as_str(), "fetch_cancel");
    }

    #[test]
    fn test_event_sender_no_receivers() {
        let sender = EventSender::new(100, "node-1".to_string());
        let count = sender.send(
            CdcEventType::Insert,
            CdcEventBuilder::insert("key".into(), vec![], "up-1".into()),
        );
        assert_eq!(count, 0);
        assert_eq!(sender.current_sequence(), 2); // sequence still increments
    }

    #[test]
    fn test_event_sender_with_receiver() {
        let sender = EventSender::new(100, "node-1".to_string());
        let mut rx = sender.subscribe();
        let count = sender.send(
            CdcEventType::Insert,
            CdcEventBuilder::insert("key".into(), b"data".to_vec(), "up-1".into()),
        );
        assert_eq!(count, 1);

        let event = rx.try_recv().unwrap();
        assert_eq!(event.sequence, 1);
        assert_eq!(event.query_key, "key");
        assert_eq!(event.payload, b"data");
        assert_eq!(event.upstream_id, "up-1");
        assert_eq!(event.origin_node_id, "node-1");
        assert!(matches!(event.event_type, CdcEventType::Insert));
    }

    #[test]
    fn test_event_sender_sequence_monotonic() {
        let sender = EventSender::new(100, "n".to_string());
        let _rx = sender.subscribe();
        for i in 1..=5 {
            sender.send(
                CdcEventType::Remove,
                CdcEventBuilder::remove(format!("k{i}")),
            );
        }
        assert_eq!(sender.current_sequence(), 6);
    }

    #[test]
    fn test_event_builder_with_context_and_expiry() {
        let sender = EventSender::new(100, "n".to_string());
        let mut rx = sender.subscribe();
        sender.send(
            CdcEventType::Insert,
            CdcEventBuilder::insert("q".into(), vec![], "u".into())
                .with_context("ctx-1".into())
                .with_expiry(99999),
        );
        let event = rx.try_recv().unwrap();
        assert_eq!(event.context_id, "ctx-1");
        assert_eq!(event.absolute_expiry_ms, 99999);
    }

    #[test]
    fn test_event_is_expired() {
        let event = CdcEvent {
            sequence: 1,
            timestamp_ms: 0,
            event_type: CdcEventType::Insert,
            query_key: String::new(),
            payload: vec![],
            upstream_id: String::new(),
            context_id: String::new(),
            origin_node_id: String::new(),
            absolute_expiry_ms: 0,
        };
        assert!(!event.is_expired()); // 0 = no expiry

        let expired = CdcEvent {
            absolute_expiry_ms: 1, // epoch ms 1 is definitely in the past
            ..event.clone()
        };
        assert!(expired.is_expired());

        let future = CdcEvent {
            absolute_expiry_ms: u64::MAX,
            ..event
        };
        assert!(!future.is_expired());
    }

    #[test]
    fn test_receiver_count() {
        let sender = EventSender::new(10, "n".to_string());
        assert_eq!(sender.receiver_count(), 0);
        let _r1 = sender.subscribe();
        assert_eq!(sender.receiver_count(), 1);
        let _r2 = sender.subscribe();
        assert_eq!(sender.receiver_count(), 2);
        drop(_r1);
        assert_eq!(sender.receiver_count(), 1);
    }

    #[test]
    fn test_now_ms_is_reasonable() {
        let ts = now_ms();
        // Should be after 2024-01-01 in epoch ms
        assert!(ts > 1_704_067_200_000);
    }
}
