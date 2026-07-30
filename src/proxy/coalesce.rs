//! Request coalescing with singleflight pattern.
//!
//! Deduplicates concurrent identical requests by using broadcast channels
//! to share results among waiters.

use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::broadcast;

use super::lifecycle::ProxyError;
use super::types::{QueryHash, QueryResponse};

/// Default channel capacity for broadcast.
const BROADCAST_CAPACITY: usize = 1;

/// Result type for coalesced requests.
///
/// Uses `Arc<QueryResponse>` so broadcast to N waiters only bumps refcounts
/// instead of deep-cloning `Vec<SearchResult>` N times.
pub type CoalesceResult = Result<Arc<QueryResponse>, Arc<ProxyError>>;

/// Request coalescer implementing the singleflight pattern.
///
/// When multiple requests for the same query arrive concurrently,
/// only the first "leader" makes the actual upstream request.
/// Other waiters receive the result via broadcast channel.
pub struct RequestCoalescer {
    /// In-flight requests mapped by query hash.
    in_flight: DashMap<QueryHash, broadcast::Sender<CoalesceResult>>,
}

/// Result of attempting to join or lead a request.
pub enum CoalesceAction {
    /// This request is the leader and should make the upstream call.
    Leader,
    /// This request should wait for the leader's result.
    Waiter(broadcast::Receiver<CoalesceResult>),
}

impl RequestCoalescer {
    /// Create a new request coalescer.
    pub fn new() -> Self {
        Self {
            in_flight: DashMap::new(),
        }
    }

    /// Get or insert an in-flight request.
    ///
    /// Returns `(receiver, is_leader)`:
    /// - If this is a new request, returns `(receiver, true)` - caller is the leader
    /// - If request is already in-flight, returns `(receiver, false)` - caller is a waiter
    pub fn get_or_insert(&self, query_hash: QueryHash) -> CoalesceAction {
        // Try to insert a new sender
        use dashmap::mapref::entry::Entry;

        match self.in_flight.entry(query_hash) {
            Entry::Occupied(entry) => {
                // Already in-flight, subscribe to existing sender
                let receiver = entry.get().subscribe();
                CoalesceAction::Waiter(receiver)
            }
            Entry::Vacant(entry) => {
                // New request, create sender and become leader
                let (tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
                entry.insert(tx);
                CoalesceAction::Leader
            }
        }
    }

    /// Complete a request and broadcast the result to all waiters.
    ///
    /// This should only be called by the leader.
    pub fn complete(&self, query_hash: &QueryHash, result: CoalesceResult) {
        if let Some((_, sender)) = self.in_flight.remove(query_hash) {
            // Broadcast to all waiters (ignore send errors - no receivers is OK)
            let _ = sender.send(result);
        }
    }

    /// Remove an in-flight request without broadcasting a result.
    ///
    /// Useful for cleanup if the leader encounters an unrecoverable error
    /// before having a result to share.
    pub fn remove(&self, query_hash: &QueryHash) {
        self.in_flight.remove(query_hash);
    }

    /// Get the number of in-flight requests.
    pub fn in_flight_count(&self) -> usize {
        self.in_flight.len()
    }

    /// Check if a specific query is currently in-flight.
    pub fn is_in_flight(&self, query_hash: &QueryHash) -> bool {
        self.in_flight.contains_key(query_hash)
    }
}

impl Default for RequestCoalescer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "tests/coalesce_tests.rs"]
mod tests;
