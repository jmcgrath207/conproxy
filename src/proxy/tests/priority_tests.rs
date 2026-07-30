#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

#[test]
fn test_priority_ordering() {
    assert!(Priority::Critical > Priority::High);
    assert!(Priority::High > Priority::Normal);
    assert!(Priority::Normal > Priority::Low);
}

#[test]
fn test_priority_from_str() {
    assert_eq!(Priority::parse("low"), Priority::Low);
    assert_eq!(Priority::parse("normal"), Priority::Normal);
    assert_eq!(Priority::parse("high"), Priority::High);
    assert_eq!(Priority::parse("critical"), Priority::Critical);
    assert_eq!(Priority::parse("unknown"), Priority::Normal);
}

#[test]
fn test_priority_from_int() {
    assert_eq!(Priority::from_int(0), Priority::Low);
    assert_eq!(Priority::from_int(1), Priority::Normal);
    assert_eq!(Priority::from_int(2), Priority::High);
    assert_eq!(Priority::from_int(3), Priority::Critical);
    assert_eq!(Priority::from_int(99), Priority::Critical);
}

#[test]
fn test_queue_creation() {
    let queue: PriorityQueue<i32> = PriorityQueue::new(10);
    assert!(queue.is_empty());
    assert_eq!(queue.max_size(), 10);
}

#[test]
fn test_queue_push_pop() {
    let queue = PriorityQueue::new(10);

    queue.push(1, Priority::Normal);
    queue.push(2, Priority::High);
    queue.push(3, Priority::Low);

    // Should pop in priority order
    assert_eq!(queue.pop().unwrap().payload, 2); // High
    assert_eq!(queue.pop().unwrap().payload, 1); // Normal
    assert_eq!(queue.pop().unwrap().payload, 3); // Low
    assert!(queue.pop().is_none());
}

#[test]
fn test_queue_fifo_within_priority() {
    let queue = PriorityQueue::new(10);

    queue.push(1, Priority::Normal);
    queue.push(2, Priority::Normal);
    queue.push(3, Priority::Normal);

    // Should pop in FIFO order within same priority
    assert_eq!(queue.pop().unwrap().payload, 1);
    assert_eq!(queue.pop().unwrap().payload, 2);
    assert_eq!(queue.pop().unwrap().payload, 3);
}

#[test]
fn test_queue_full_rejection() {
    let queue = PriorityQueue::new(2);

    assert!(queue.push(1, Priority::High).is_none()); // Accepted
    assert!(queue.push(2, Priority::High).is_none()); // Accepted
    assert!(queue.push(3, Priority::Normal).is_some()); // Rejected (lower priority)
}

#[test]
fn test_queue_full_eviction() {
    let queue = PriorityQueue::new(2);

    queue.push(1, Priority::Low);
    queue.push(2, Priority::Normal);

    // Higher priority should evict lower
    assert!(queue.push(3, Priority::High).is_none()); // Accepted, evicts Low
    assert_eq!(queue.len(), 2);

    let first = queue.pop().unwrap();
    assert_eq!(first.payload, 3); // High priority
}

#[test]
fn test_queue_stats() {
    let queue = PriorityQueue::new(10);

    queue.push(1, Priority::Low);
    queue.push(2, Priority::Normal);
    queue.push(3, Priority::Normal);
    queue.push(4, Priority::High);

    let stats = queue.stats();
    assert_eq!(stats.total, 4);
    assert_eq!(stats.low_priority, 1);
    assert_eq!(stats.normal_priority, 2);
    assert_eq!(stats.high_priority, 1);
    assert_eq!(stats.critical_priority, 0);
}

#[test]
fn test_queue_clear() {
    let queue = PriorityQueue::new(10);

    queue.push(1, Priority::Normal);
    queue.push(2, Priority::Normal);

    assert_eq!(queue.len(), 2);
    queue.clear();
    assert!(queue.is_empty());
}

#[test]
fn test_prioritized_request_wait_time() {
    let req = PrioritizedRequest::new(42, Priority::Normal, 1);
    std::thread::sleep(std::time::Duration::from_millis(10));
    assert!(req.wait_time() >= std::time::Duration::from_millis(10));
}

#[test]
fn test_queue_utilization() {
    let queue = PriorityQueue::new(10);

    queue.push(1, Priority::Normal);
    queue.push(2, Priority::Normal);
    queue.push(3, Priority::Normal);
    queue.push(4, Priority::Normal);
    queue.push(5, Priority::Normal);

    let stats = queue.stats();
    assert!((stats.utilization() - 0.5).abs() < 0.01);
}

#[test]
fn test_unbounded_queue() {
    let queue: PriorityQueue<i32> = PriorityQueue::unbounded();
    assert_eq!(queue.max_size(), usize::MAX);

    // Should accept many items
    for i in 0..1000 {
        assert!(queue.push(i, Priority::Normal).is_none());
    }
    assert_eq!(queue.len(), 1000);
}
