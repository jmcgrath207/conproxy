//! gRPC CdcService implementation — server-streaming Subscribe RPC.

use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use tracing::debug;

use super::event::{CdcEvent as InternalEvent, CdcEventType as InternalType, EventSender};
use super::proto;

/// gRPC implementation of CdcService.
pub struct CdcServiceImpl {
    event_sender: Arc<EventSender>,
}

impl CdcServiceImpl {
    pub fn new(event_sender: Arc<EventSender>) -> Self {
        Self { event_sender }
    }
}

#[tonic::async_trait]
impl proto::cdc_service_server::CdcService for CdcServiceImpl {
    type SubscribeStream = ReceiverStream<Result<proto::CdcEvent, Status>>;

    async fn subscribe(
        &self,
        request: Request<proto::CdcSubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let req = request.into_inner();
        let consumer_id = if req.consumer_id.is_empty() {
            "anonymous".to_string()
        } else {
            req.consumer_id
        };

        // Parse event type filters
        let filter: Vec<InternalType> = req
            .event_types
            .iter()
            .filter_map(|s| match s.as_str() {
                "insert" => Some(InternalType::Insert),
                "remove" => Some(InternalType::Remove),
                "fetch_start" => Some(InternalType::FetchStart),
                "fetch_cancel" => Some(InternalType::FetchCancel),
                _ => None,
            })
            .collect();

        debug!(consumer = %consumer_id, filters = ?filter, "CDC subscriber connected");

        let mut broadcast_rx = self.event_sender.subscribe();
        let (tx, rx) = tokio::sync::mpsc::channel(256);

        tokio::spawn(async move {
            loop {
                match broadcast_rx.recv().await {
                    Ok(event) => {
                        // Apply type filter
                        if !filter.is_empty() && !filter.contains(&event.event_type) {
                            continue;
                        }

                        let proto_event = internal_to_proto(&event);
                        if tx.send(Ok(proto_event)).await.is_err() {
                            // Client disconnected
                            debug!(consumer = %consumer_id, "CDC subscriber disconnected");
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        debug!(consumer = %consumer_id, skipped, "CDC subscriber lagged");
                        // Continue — client missed some events but can keep going
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        debug!(consumer = %consumer_id, "CDC channel closed");
                        break;
                    }
                }
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

/// Convert internal CdcEvent to protobuf CdcEvent.
pub fn internal_to_proto(event: &InternalEvent) -> proto::CdcEvent {
    proto::CdcEvent {
        sequence: event.sequence,
        timestamp_ms: event.timestamp_ms,
        event_type: event.event_type.to_proto(),
        query_key: event.query_key.clone(),
        payload: event.payload.clone(),
        upstream_id: event.upstream_id.clone(),
        context_id: event.context_id.clone(),
        origin_node_id: event.origin_node_id.clone(),
        absolute_expiry_ms: event.absolute_expiry_ms,
    }
}

/// Convert protobuf CdcEvent to internal CdcEvent.
pub fn proto_to_internal(event: &proto::CdcEvent) -> Option<InternalEvent> {
    let event_type = InternalType::from_proto(event.event_type)?;
    Some(InternalEvent {
        sequence: event.sequence,
        timestamp_ms: event.timestamp_ms,
        event_type,
        query_key: event.query_key.clone(),
        payload: event.payload.clone(),
        upstream_id: event.upstream_id.clone(),
        context_id: event.context_id.clone(),
        origin_node_id: event.origin_node_id.clone(),
        absolute_expiry_ms: event.absolute_expiry_ms,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::proxy::cdc::event::{CdcEvent as IntEvent, CdcEventType};

    #[test]
    fn test_internal_to_proto_roundtrip() {
        let internal = IntEvent {
            sequence: 42,
            timestamp_ms: 1700000000000,
            event_type: CdcEventType::Insert,
            query_key: "hello world".to_string(),
            payload: b"payload".to_vec(),
            upstream_id: "up-1".to_string(),
            context_id: "ctx-a".to_string(),
            origin_node_id: "node-1".to_string(),
            absolute_expiry_ms: 1700003600000,
        };

        let proto = internal_to_proto(&internal);
        assert_eq!(proto.sequence, 42);
        assert_eq!(proto.event_type, 1); // CDC_INSERT
        assert_eq!(proto.query_key, "hello world");
        assert_eq!(proto.origin_node_id, "node-1");

        let back = proto_to_internal(&proto).unwrap();
        assert_eq!(back.sequence, internal.sequence);
        assert_eq!(back.event_type, internal.event_type);
        assert_eq!(back.query_key, internal.query_key);
        assert_eq!(back.absolute_expiry_ms, internal.absolute_expiry_ms);
    }

    #[test]
    fn test_proto_to_internal_invalid_type() {
        let proto = proto::CdcEvent {
            event_type: 0, // CDC_UNSPECIFIED
            ..Default::default()
        };
        assert!(proto_to_internal(&proto).is_none());
    }

    #[test]
    fn test_proto_to_internal_all_valid_types() {
        for (proto_type, expected_type) in [
            (CdcEventType::Insert.to_proto(), CdcEventType::Insert),
            (CdcEventType::Remove.to_proto(), CdcEventType::Remove),
            (
                CdcEventType::FetchStart.to_proto(),
                CdcEventType::FetchStart,
            ),
            (
                CdcEventType::FetchCancel.to_proto(),
                CdcEventType::FetchCancel,
            ),
        ] {
            let proto = proto::CdcEvent {
                sequence: 1,
                timestamp_ms: 1700000000000,
                event_type: proto_type,
                query_key: "test".to_string(),
                payload: vec![],
                upstream_id: "up".to_string(),
                context_id: "ctx".to_string(),
                origin_node_id: "node".to_string(),
                absolute_expiry_ms: 0,
            };
            let internal = proto_to_internal(&proto).unwrap();
            assert_eq!(internal.event_type, expected_type);
            assert_eq!(internal.query_key, "test");
        }
    }

    #[test]
    fn test_internal_to_proto_all_event_types() {
        for event_type in [
            CdcEventType::Insert,
            CdcEventType::Remove,
            CdcEventType::FetchStart,
            CdcEventType::FetchCancel,
        ] {
            let internal = IntEvent {
                sequence: 1,
                timestamp_ms: 1700000000000,
                event_type,
                query_key: "key".to_string(),
                payload: vec![1, 2, 3],
                upstream_id: "up".to_string(),
                context_id: "ctx".to_string(),
                origin_node_id: "node".to_string(),
                absolute_expiry_ms: 0,
            };
            let proto = internal_to_proto(&internal);
            assert_eq!(proto.query_key, "key");
            assert_eq!(proto.payload, vec![1, 2, 3]);
            // Roundtrip
            let back = proto_to_internal(&proto).unwrap();
            assert_eq!(back.event_type, event_type);
        }
    }

    #[test]
    fn test_proto_to_internal_preserves_all_fields() {
        let proto = proto::CdcEvent {
            sequence: 99,
            timestamp_ms: 1234567890,
            event_type: CdcEventType::Insert.to_proto(),
            query_key: "my-key".to_string(),
            payload: vec![10, 20],
            upstream_id: "upstream-1".to_string(),
            context_id: "context-a".to_string(),
            origin_node_id: "node-x".to_string(),
            absolute_expiry_ms: 9999999,
        };
        let internal = proto_to_internal(&proto).unwrap();
        assert_eq!(internal.sequence, 99);
        assert_eq!(internal.timestamp_ms, 1234567890);
        assert_eq!(internal.query_key, "my-key");
        assert_eq!(internal.payload, vec![10, 20]);
        assert_eq!(internal.upstream_id, "upstream-1");
        assert_eq!(internal.context_id, "context-a");
        assert_eq!(internal.origin_node_id, "node-x");
        assert_eq!(internal.absolute_expiry_ms, 9999999);
    }

    #[test]
    fn test_cdc_service_impl_creation() {
        use super::EventSender;
        let sender = Arc::new(EventSender::new(100, "test-node".to_string()));
        let _service = CdcServiceImpl::new(sender);
    }

    #[tokio::test]
    async fn test_subscribe_basic_flow() {
        use super::EventSender;
        use proto::cdc_service_server::CdcService;
        use tonic::Request;

        let sender = Arc::new(EventSender::new(100, "test-node".to_string()));
        let service = CdcServiceImpl::new(sender.clone());

        // Subscribe with no filters
        let request = Request::new(proto::CdcSubscribeRequest {
            consumer_id: "test-consumer".to_string(),
            event_types: vec![],
        });

        let response = service.subscribe(request).await.unwrap();
        let mut stream = response.into_inner();

        // Send an event through the sender
        sender.send(
            CdcEventType::Insert,
            crate::proxy::cdc::CdcEventBuilder::insert(
                "q1".to_string(),
                b"payload".to_vec(),
                "up-1".to_string(),
            ),
        );

        // Should receive the event
        use tokio_stream::StreamExt;
        let msg = tokio::time::timeout(std::time::Duration::from_millis(100), stream.next()).await;
        assert!(msg.is_ok());
        let event = msg.unwrap().unwrap().unwrap();
        assert_eq!(event.query_key, "q1");
    }

    #[tokio::test]
    async fn test_subscribe_with_filter() {
        use super::EventSender;
        use proto::cdc_service_server::CdcService;
        use tonic::Request;

        let sender = Arc::new(EventSender::new(100, "test-node".to_string()));
        let service = CdcServiceImpl::new(sender.clone());

        // Subscribe with filter for only "remove" events
        let request = Request::new(proto::CdcSubscribeRequest {
            consumer_id: "".to_string(), // test anonymous consumer
            event_types: vec!["remove".to_string()],
        });

        let response = service.subscribe(request).await.unwrap();
        let mut stream = response.into_inner();

        // Send an insert event (should be filtered out)
        sender.send(
            CdcEventType::Insert,
            crate::proxy::cdc::CdcEventBuilder::insert(
                "q1".to_string(),
                b"payload".to_vec(),
                "up-1".to_string(),
            ),
        );

        // Send a remove event (should pass filter)
        sender.send(
            CdcEventType::Remove,
            crate::proxy::cdc::CdcEventBuilder::remove("q2".to_string()),
        );

        use tokio_stream::StreamExt;
        let msg = tokio::time::timeout(std::time::Duration::from_millis(100), stream.next()).await;
        assert!(msg.is_ok());
        let event = msg.unwrap().unwrap().unwrap();
        assert_eq!(event.query_key, "q2"); // Only the remove event passes
    }
}
