//! Priority queue for request handling.
//!
//! Provides priority-based request ordering to ensure important queries
//! are processed first during high load.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::time::Instant;

use parking_lot::Mutex;

/// Priority level for requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Priority {
    /// Low priority - background or non-urgent requests.
    Low = 0,
    /// Normal priority - default for most requests.
    #[default]
    Normal = 1,
    /// High priority - important or time-sensitive requests.
    High = 2,
    /// Critical priority - must be processed immediately.
    Critical = 3,
}

impl Priority {
    /// Parse priority from string.
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "low" | "background" => Priority::Low,
            "normal" | "default" => Priority::Normal,
            "high" | "important" => Priority::High,
            "critical" | "urgent" => Priority::Critical,
            _ => Priority::Normal,
        }
    }

    /// Parse priority from integer (0-3).
    pub fn from_int(n: u8) -> Self {
        match n {
            0 => Priority::Low,
            1 => Priority::Normal,
            2 => Priority::High,
            _ => Priority::Critical,
        }
    }
}

/// A request with priority for queue ordering.
#[derive(Debug)]
pub struct PrioritizedRequest<T> {
    /// The request payload.
    pub payload: T,
    /// Priority level.
    pub priority: Priority,
    /// Sequence number for FIFO ordering within same priority.
    sequence: u64,
    /// When the request was enqueued.
    pub enqueued_at: Instant,
}

impl<T> PrioritizedRequest<T> {
    /// Create a new prioritized request.
    pub fn new(payload: T, priority: Priority, sequence: u64) -> Self {
        Self {
            payload,
            priority,
            sequence,
            enqueued_at: Instant::now(),
        }
    }

    /// Get wait time since enqueue.
    pub fn wait_time(&self) -> std::time::Duration {
        self.enqueued_at.elapsed()
    }
}

impl<T> PartialEq for PrioritizedRequest<T> {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.sequence == other.sequence
    }
}

impl<T> Eq for PrioritizedRequest<T> {}

impl<T> PartialOrd for PrioritizedRequest<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for PrioritizedRequest<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher priority first
        match self.priority.cmp(&other.priority) {
            Ordering::Equal => {
                // Within same priority, lower sequence (older) first (reverse for max-heap)
                other.sequence.cmp(&self.sequence)
            }
            other => other,
        }
    }
}

/// Thread-safe priority queue for requests.
pub struct PriorityQueue<T> {
    heap: Mutex<BinaryHeap<PrioritizedRequest<T>>>,
    sequence: AtomicU64,
    max_size: usize,
}

impl<T> PriorityQueue<T> {
    /// Create a new priority queue with the given maximum size.
    pub fn new(max_size: usize) -> Self {
        Self {
            heap: Mutex::new(BinaryHeap::new()),
            sequence: AtomicU64::new(0),
            max_size,
        }
    }

    /// Create an unbounded priority queue.
    pub fn unbounded() -> Self {
        Self::new(usize::MAX)
    }

    /// Push a request with the given priority.
    ///
    /// Returns `Some(request)` if the queue is full and the request was rejected,
    /// or `None` if the request was accepted.
    pub fn push(&self, payload: T, priority: Priority) -> Option<T> {
        let mut heap = self.heap.lock();

        if heap.len() >= self.max_size {
            // Check if we can evict a lower priority request
            if let Some(lowest) = heap.peek() {
                if lowest.priority >= priority {
                    // Queue full with equal or higher priority items
                    return Some(payload);
                }
                // Evict lowest priority item
                heap.pop();
            }
        }

        let sequence = self.sequence.fetch_add(1, AtomicOrdering::Relaxed);
        heap.push(PrioritizedRequest::new(payload, priority, sequence));
        None
    }

    /// Push a request with normal priority.
    pub fn push_normal(&self, payload: T) -> Option<T> {
        self.push(payload, Priority::Normal)
    }

    /// Pop the highest priority request.
    pub fn pop(&self) -> Option<PrioritizedRequest<T>> {
        self.heap.lock().pop()
    }

    /// Peek at the highest priority request without removing it.
    pub fn peek_priority(&self) -> Option<Priority> {
        self.heap.lock().peek().map(|r| r.priority)
    }

    /// Get the current queue length.
    pub fn len(&self) -> usize {
        self.heap.lock().len()
    }

    /// Check if the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.heap.lock().is_empty()
    }

    /// Check if the queue is full.
    pub fn is_full(&self) -> bool {
        self.heap.lock().len() >= self.max_size
    }

    /// Get the maximum size.
    pub fn max_size(&self) -> usize {
        self.max_size
    }

    /// Clear all items from the queue.
    pub fn clear(&self) {
        self.heap.lock().clear();
    }

    /// Get statistics about the queue.
    pub fn stats(&self) -> QueueStats {
        let heap = self.heap.lock();
        let mut by_priority = [0usize; 4];

        for item in heap.iter() {
            let idx = item.priority as usize;
            if let Some(slot) = by_priority.get_mut(idx) {
                *slot = slot.saturating_add(1);
            }
        }

        QueueStats {
            total: heap.len(),
            max_size: self.max_size,
            low_priority: *by_priority.first().unwrap_or(&0),
            normal_priority: *by_priority.get(1).unwrap_or(&0),
            high_priority: *by_priority.get(2).unwrap_or(&0),
            critical_priority: *by_priority.get(3).unwrap_or(&0),
        }
    }
}

/// Statistics about the priority queue.
#[derive(Debug, Clone, Default)]
pub struct QueueStats {
    /// Total items in queue.
    pub total: usize,
    /// Maximum queue size.
    pub max_size: usize,
    /// Items with low priority.
    pub low_priority: usize,
    /// Items with normal priority.
    pub normal_priority: usize,
    /// Items with high priority.
    pub high_priority: usize,
    /// Items with critical priority.
    pub critical_priority: usize,
}

impl QueueStats {
    /// Get utilization (0.0 to 1.0).
    pub fn utilization(&self) -> f64 {
        if self.max_size == 0 || self.max_size == usize::MAX {
            return 0.0;
        }
        self.total as f64 / self.max_size as f64
    }
}

#[cfg(test)]
#[path = "tests/priority_tests.rs"]
mod tests;
