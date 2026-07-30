//! Distributed singleflight — cross-peer fetch coordination via FETCH_START/CANCEL.

use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use tokio::sync::Notify;

use crate::proxy::cdc::{CdcEventBuilder, CdcEventType, SharedEventSender};

/// State of a remote fetch operation.
struct FetchState {
    /// Node that started the fetch. Retained for debug inspection of active_fetches.
    #[allow(dead_code)]
    origin_node_id: String,
    /// Notifier for waiters.
    notify: Arc<Notify>,
}

/// Action to take after checking the coalescer.
pub enum CoalesceDecision {
    /// No one is fetching this key — proceed with upstream query.
    Proceed,
    /// Another peer is already fetching — wait result.
    Wait(Arc<Notify>),
}

/// Distributed request coalescer that coordinates fetch operations across peers.
///
/// When a cache miss occurs, before querying upstream:
/// 1. Check if another peer is already fetching this key
/// 2. If yes: wait for the result (with timeout)
/// 3. If no: register our fetch, publish FETCH_START, proceed
///
/// This is **best-effort** — network latency means both peers can start
/// fetching before either sees the other's FETCH_START. The fallback
/// (last-write-wins by timestamp) handles this gracefully.
pub struct DistributedCoalescer {
    /// Active remote fetches: query_key → FetchState
    active_fetches: DashMap<String, FetchState>,
    /// CDC sender for publishing FETCH_START/CANCEL
    cdc_sender: SharedEventSender,
    /// Timeout for waiting on remote fetches
    wait_timeout: Duration,
}

impl DistributedCoalescer {
    pub fn new(cdc_sender: SharedEventSender, wait_timeout: Duration) -> Self {
        Self {
            active_fetches: DashMap::new(),
            cdc_sender,
            wait_timeout,
        }
    }

    /// Check if another peer is fetching this key.
    ///
    /// Returns `Proceed` if we should fetch, or `Wait` with a notify handle.
    pub fn check(&self, query_key: &str) -> CoalesceDecision {
        if let Some(state) = self.active_fetches.get(query_key) {
            CoalesceDecision::Wait(state.notify.clone())
        } else {
            CoalesceDecision::Proceed
        }
    }

    /// Register that we are starting a fetch for this key.
    /// Publishes a FETCH_START CDC event.
    pub fn register_fetch(&self, query_key: &str) {
        self.cdc_sender.send(
            CdcEventType::FetchStart,
            CdcEventBuilder::fetch_start(query_key.to_string()),
        );
    }

    /// Complete a fetch (success). Removes the entry and publishes INSERT via CacheStore.
    pub fn complete_fetch(&self, query_key: &str) {
        if let Some((_, state)) = self.active_fetches.remove(query_key) {
            state.notify.notify_waiters();
        }
    }

    /// Cancel a fetch (failure). Removes the entry and publishes FETCH_CANCEL.
    pub fn cancel_fetch(&self, query_key: &str) {
        self.active_fetches.remove(query_key);
        self.cdc_sender.send(
            CdcEventType::FetchCancel,
            CdcEventBuilder::fetch_cancel(query_key.to_string()),
        );
    }

    /// Handle a FETCH_START event from a remote peer.
    pub fn on_remote_fetch_start(&self, query_key: &str, origin_node_id: &str) {
        let notify = Arc::new(Notify::new());
        self.active_fetches.insert(
            query_key.to_string(),
            FetchState {
                origin_node_id: origin_node_id.to_string(),
                notify,
            },
        );
    }

    /// Handle a FETCH_CANCEL event from a remote peer.
    pub fn on_remote_fetch_cancel(&self, query_key: &str) {
        if let Some((_, state)) = self.active_fetches.remove(query_key) {
            state.notify.notify_waiters();
        }
    }

    /// Handle a CDC INSERT event — completes any pending remote fetch.
    pub fn on_remote_insert(&self, query_key: &str) {
        if let Some((_, state)) = self.active_fetches.remove(query_key) {
            state.notify.notify_waiters();
        }
    }

    /// Wait for a remote fetch to complete, with timeout.
    ///
    /// Returns `true` if the fetch completed (result should be in cache),
    /// `false` if timed out (caller should proceed with own fetch).
    pub async fn wait_for_fetch(&self, notify: Arc<Notify>) -> bool {
        tokio::select! {
            _ = notify.notified() => true,
            _ = tokio::time::sleep(self.wait_timeout) => false,
        }
    }

    /// Get the number of active remote fetches being tracked.
    pub fn active_fetch_count(&self) -> usize {
        self.active_fetches.len()
    }

    /// Get the wait timeout.
    pub fn wait_timeout(&self) -> Duration {
        self.wait_timeout
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::cdc::EventSender;

    fn make_coalescer() -> DistributedCoalescer {
        let sender = Arc::new(EventSender::new(100, "test-node".to_string()));
        DistributedCoalescer::new(sender, Duration::from_millis(100))
    }

    #[test]
    fn test_check_empty() {
        let c = make_coalescer();
        assert!(matches!(c.check("q1"), CoalesceDecision::Proceed));
    }

    #[test]
    fn test_check_after_remote_start() {
        let c = make_coalescer();
        c.on_remote_fetch_start("q1", "node-b");
        assert!(matches!(c.check("q1"), CoalesceDecision::Wait(_)));
    }

    #[test]
    fn test_remote_insert_clears_fetch() {
        let c = make_coalescer();
        c.on_remote_fetch_start("q1", "node-b");
        assert_eq!(c.active_fetch_count(), 1);
        c.on_remote_insert("q1");
        assert_eq!(c.active_fetch_count(), 0);
    }

    #[test]
    fn test_remote_cancel_clears_fetch() {
        let c = make_coalescer();
        c.on_remote_fetch_start("q1", "node-b");
        c.on_remote_fetch_cancel("q1");
        assert_eq!(c.active_fetch_count(), 0);
    }

    #[test]
    fn test_complete_fetch_clears() {
        let c = make_coalescer();
        c.on_remote_fetch_start("q1", "node-b");
        c.complete_fetch("q1");
        assert_eq!(c.active_fetch_count(), 0);
    }

    #[tokio::test]
    async fn test_wait_for_fetch_timeout() {
        let c = make_coalescer();
        let notify = Arc::new(Notify::new());
        // Should timeout since no one notifies
        let result = c.wait_for_fetch(notify).await;
        assert!(!result);
    }

    #[tokio::test]
    async fn test_wait_for_fetch_notified() {
        let c = make_coalescer();
        let notify = Arc::new(Notify::new());
        let notify_clone = notify.clone();

        // Notify immediately in background
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            notify_clone.notify_waiters();
        });

        let result = c.wait_for_fetch(notify).await;
        assert!(result);
    }

    #[test]
    fn test_register_fetch_publishes_event() {
        let c = make_coalescer();
        // register_fetch should not panic and should publish a CDC event
        c.register_fetch("q1");
        // The CDC event was sent to the broadcast channel; no active fetch is registered
        // (register_fetch only publishes, it doesn't insert into active_fetches)
        assert_eq!(c.active_fetch_count(), 0);
    }

    #[test]
    fn test_cancel_fetch_publishes_event() {
        let c = make_coalescer();
        // Register a remote fetch first
        c.on_remote_fetch_start("q1", "node-b");
        assert_eq!(c.active_fetch_count(), 1);

        // Cancel should remove the entry and publish FETCH_CANCEL
        c.cancel_fetch("q1");
        assert_eq!(c.active_fetch_count(), 0);
    }

    #[test]
    fn test_cancel_fetch_nonexistent_key() {
        let c = make_coalescer();
        // Should not panic when canceling a key that doesn't exist
        c.cancel_fetch("nonexistent");
        assert_eq!(c.active_fetch_count(), 0);
    }

    #[test]
    fn test_wait_timeout_accessor() {
        let c = make_coalescer();
        assert_eq!(c.wait_timeout(), Duration::from_millis(100));
    }

    #[test]
    fn test_multiple_keys_tracked_independently() {
        let c = make_coalescer();
        c.on_remote_fetch_start("q1", "node-b");
        c.on_remote_fetch_start("q2", "node-c");
        assert_eq!(c.active_fetch_count(), 2);

        c.complete_fetch("q1");
        assert_eq!(c.active_fetch_count(), 1);
        assert!(matches!(c.check("q1"), CoalesceDecision::Proceed));
        assert!(matches!(c.check("q2"), CoalesceDecision::Wait(_)));
    }

    #[test]
    fn test_overwrite_same_key_different_origin() {
        // on_remote_fetch_start with a different origin should replace the prior state
        let c = make_coalescer();
        c.on_remote_fetch_start("q1", "node-b");
        assert_eq!(c.active_fetch_count(), 1);
        c.on_remote_fetch_start("q1", "node-c");
        assert_eq!(
            c.active_fetch_count(),
            1,
            "same key should not double-count"
        );
    }

    #[test]
    fn test_on_remote_insert_unknown_key_no_panic() {
        // Insert event for a key that no one is tracking → should silently no-op
        let c = make_coalescer();
        c.on_remote_insert("never-registered");
        assert_eq!(c.active_fetch_count(), 0);
    }

    #[test]
    fn test_on_remote_cancel_unknown_key_no_panic() {
        let c = make_coalescer();
        c.on_remote_fetch_cancel("never-registered");
        assert_eq!(c.active_fetch_count(), 0);
    }

    #[test]
    fn test_complete_fetch_unknown_key_no_panic() {
        let c = make_coalescer();
        c.complete_fetch("never-registered");
        assert_eq!(c.active_fetch_count(), 0);
    }

    #[test]
    fn test_check_after_complete_returns_proceed() {
        // After a fetch completes, check() should return Proceed (not Wait)
        let c = make_coalescer();
        c.on_remote_fetch_start("q1", "node-b");
        assert!(matches!(c.check("q1"), CoalesceDecision::Wait(_)));
        c.complete_fetch("q1");
        assert!(matches!(c.check("q1"), CoalesceDecision::Proceed));
    }

    #[tokio::test]
    async fn test_concurrent_waits_all_resolve_on_complete() {
        // Multiple waiters on the same fetch should all be notified
        let c = make_coalescer();
        c.on_remote_fetch_start("q1", "node-b");
        let CoalesceDecision::Wait(notify) = c.check("q1") else {
            panic!("expected Wait");
        };
        let n1 = notify.clone();
        let n2 = notify.clone();

        let h1 = tokio::spawn(async move { n1.notified().await });
        let h2 = tokio::spawn(async move { n2.notified().await });

        // Give waiters time to register
        tokio::time::sleep(Duration::from_millis(20)).await;

        c.complete_fetch("q1");

        // Both should resolve (with timeout to avoid hang)
        let r1 = tokio::time::timeout(Duration::from_millis(500), h1).await;
        let r2 = tokio::time::timeout(Duration::from_millis(500), h2).await;
        assert!(r1.is_ok(), "waiter 1 should resolve");
        assert!(r2.is_ok(), "waiter 2 should resolve");
    }
}
