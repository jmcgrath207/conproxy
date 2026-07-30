#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;
use crate::proxy::types::{CacheStatus, SearchResult};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn make_response(content: &str) -> QueryResponse {
    QueryResponse {
        results: vec![SearchResult {
            id: "test".to_string(),
            score: 1.0,
            content: content.to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    }
}

fn make_hash(s: &str) -> QueryHash {
    *blake3::hash(s.as_bytes()).as_bytes()
}

#[test]
fn test_new_request_becomes_leader() {
    let coalescer = RequestCoalescer::new();
    let hash = make_hash("test query");

    match coalescer.get_or_insert(hash) {
        CoalesceAction::Leader => {}
        CoalesceAction::Waiter(_) => panic!("Expected to be leader"),
    }

    assert!(coalescer.is_in_flight(&hash));
    assert_eq!(coalescer.in_flight_count(), 1);
}

#[test]
fn test_second_request_becomes_waiter() {
    let coalescer = RequestCoalescer::new();
    let hash = make_hash("test query");

    // First request becomes leader
    match coalescer.get_or_insert(hash) {
        CoalesceAction::Leader => {}
        CoalesceAction::Waiter(_) => panic!("First request should be leader"),
    }

    // Second request becomes waiter
    match coalescer.get_or_insert(hash) {
        CoalesceAction::Leader => panic!("Second request should be waiter"),
        CoalesceAction::Waiter(_) => {}
    }

    assert_eq!(coalescer.in_flight_count(), 1);
}

#[test]
fn test_complete_removes_entry() {
    let coalescer = RequestCoalescer::new();
    let hash = make_hash("test query");

    coalescer.get_or_insert(hash);
    assert!(coalescer.is_in_flight(&hash));

    coalescer.complete(&hash, Ok(Arc::new(make_response("result"))));
    assert!(!coalescer.is_in_flight(&hash));
    assert_eq!(coalescer.in_flight_count(), 0);
}

#[test]
fn test_remove_without_result() {
    let coalescer = RequestCoalescer::new();
    let hash = make_hash("test query");

    coalescer.get_or_insert(hash);
    coalescer.remove(&hash);

    assert!(!coalescer.is_in_flight(&hash));
}

#[test]
fn test_different_queries_both_leaders() {
    let coalescer = RequestCoalescer::new();
    let hash1 = make_hash("query 1");
    let hash2 = make_hash("query 2");

    match coalescer.get_or_insert(hash1) {
        CoalesceAction::Leader => {}
        CoalesceAction::Waiter(_) => panic!("Should be leader for query 1"),
    }

    match coalescer.get_or_insert(hash2) {
        CoalesceAction::Leader => {}
        CoalesceAction::Waiter(_) => panic!("Should be leader for query 2"),
    }

    assert_eq!(coalescer.in_flight_count(), 2);
}

#[tokio::test]
async fn test_broadcast_result_to_waiters() {
    let coalescer = Arc::new(RequestCoalescer::new());
    let hash = make_hash("test query");

    // First request becomes leader
    coalescer.get_or_insert(hash);

    // Second request becomes waiter
    let receiver = match coalescer.get_or_insert(hash) {
        CoalesceAction::Waiter(rx) => rx,
        CoalesceAction::Leader => panic!("Should be waiter"),
    };

    // Leader completes
    let response = make_response("shared result");
    coalescer.complete(&hash, Ok(Arc::new(response)));

    // Waiter should receive the result
    let mut rx = receiver;
    let result = rx.recv().await.unwrap();
    assert!(result.is_ok());
    let received = result.unwrap();
    assert_eq!(received.results[0].content, "shared result");
}

#[tokio::test]
async fn test_concurrent_requests_coalesce() {
    let coalescer = Arc::new(RequestCoalescer::new());
    let hash = make_hash("concurrent query");
    let call_count = Arc::new(AtomicUsize::new(0));

    let mut handles = vec![];

    for _ in 0..5 {
        let coalescer = coalescer.clone();
        let call_count = call_count.clone();

        handles.push(tokio::spawn(async move {
            match coalescer.get_or_insert(hash) {
                CoalesceAction::Leader => {
                    // Simulate work
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    call_count.fetch_add(1, Ordering::SeqCst);
                    let response = Arc::new(make_response("leader result"));
                    coalescer.complete(&hash, Ok(Arc::clone(&response)));
                    Arc::try_unwrap(response).unwrap_or_else(|a| (*a).clone())
                }
                CoalesceAction::Waiter(mut rx) => {
                    // Wait for leader's result
                    let arc = rx.recv().await.unwrap().unwrap();
                    Arc::try_unwrap(arc).unwrap_or_else(|a| (*a).clone())
                }
            }
        }));
    }

    // Wait for all tasks
    for handle in handles {
        let result = handle.await.unwrap();
        assert_eq!(result.results[0].content, "leader result");
    }

    // Only one actual "upstream" call should have been made
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_error_broadcast() {
    let coalescer = Arc::new(RequestCoalescer::new());
    let hash = make_hash("error query");

    // First request becomes leader
    coalescer.get_or_insert(hash);

    // Second request becomes waiter
    let receiver = match coalescer.get_or_insert(hash) {
        CoalesceAction::Waiter(rx) => rx,
        CoalesceAction::Leader => panic!("Should be waiter"),
    };

    // Leader completes with error
    let error = Arc::new(ProxyError::Http("upstream failed".to_string()));
    coalescer.complete(&hash, Err(error));

    // Waiter should receive the error
    let mut rx = receiver;
    let result = rx.recv().await.unwrap();
    assert!(result.is_err());
}

#[test]
fn test_default_impl() {
    let coalescer = RequestCoalescer::default();
    assert_eq!(coalescer.in_flight_count(), 0);
}

/// B.2: 50 concurrent same-query requests verify exactly 1 Leader and N-1 Waiters.
///
/// Uses a tokio::sync::Barrier so all 50 tasks call get_or_insert at approximately
/// the same instant. Counts leaders and waiters to verify the singleflight invariant.
#[tokio::test]
async fn test_stress_50_concurrent_same_query_one_leader() {
    let num_tasks = 50;
    let coalescer = Arc::new(RequestCoalescer::new());
    let hash = make_hash("stress-coalesce-query");

    let barrier = Arc::new(tokio::sync::Barrier::new(num_tasks));
    let leader_count = Arc::new(AtomicUsize::new(0));
    let waiter_count = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::with_capacity(num_tasks);

    for _ in 0..num_tasks {
        let coalescer = coalescer.clone();
        let barrier = barrier.clone();
        let leader_count = leader_count.clone();
        let waiter_count = waiter_count.clone();

        handles.push(tokio::spawn(async move {
            // Synchronize all tasks to start at the same time
            barrier.wait().await;

            match coalescer.get_or_insert(hash) {
                CoalesceAction::Leader => {
                    leader_count.fetch_add(1, Ordering::SeqCst);
                }
                CoalesceAction::Waiter(_) => {
                    waiter_count.fetch_add(1, Ordering::SeqCst);
                }
            }
        }));
    }

    // Wait for all tasks to complete their get_or_insert
    for handle in handles {
        handle.await.unwrap();
    }

    let leaders = leader_count.load(Ordering::SeqCst);
    let waiters = waiter_count.load(Ordering::SeqCst);

    // Exactly 1 leader
    assert_eq!(leaders, 1, "Expected exactly 1 leader, got {}", leaders,);

    // The rest are waiters
    assert_eq!(
        waiters,
        num_tasks - 1,
        "Expected {} waiters, got {}",
        num_tasks - 1,
        waiters,
    );

    // Only 1 in-flight entry
    assert_eq!(coalescer.in_flight_count(), 1);

    // Clean up: complete the request so waiters can finish
    coalescer.complete(&hash, Ok(Arc::new(make_response("stress result"))));
    assert_eq!(coalescer.in_flight_count(), 0);
}

/// B.2 variant: 50 concurrent same-query requests all receive the leader's result.
///
/// Similar to the above, but also verifies that all waiters receive the
/// correct result from the leader after it completes.
#[tokio::test]
async fn test_stress_50_concurrent_all_receive_leader_result() {
    let num_tasks = 50;
    let coalescer = Arc::new(RequestCoalescer::new());
    let hash = make_hash("stress-broadcast-query");

    let barrier = Arc::new(tokio::sync::Barrier::new(num_tasks));
    let mut handles = Vec::with_capacity(num_tasks);

    for _ in 0..num_tasks {
        let coalescer = coalescer.clone();
        let barrier = barrier.clone();

        handles.push(tokio::spawn(async move {
            barrier.wait().await;

            match coalescer.get_or_insert(hash) {
                CoalesceAction::Leader => {
                    // Simulate upstream work
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    let response = Arc::new(make_response("shared-stress-result"));
                    coalescer.complete(&hash, Ok(Arc::clone(&response)));
                    Arc::try_unwrap(response).unwrap_or_else(|a| (*a).clone())
                }
                CoalesceAction::Waiter(mut rx) => {
                    // Wait for leader's result
                    let arc = rx.recv().await.unwrap().unwrap();
                    Arc::try_unwrap(arc).unwrap_or_else(|a| (*a).clone())
                }
            }
        }));
    }

    // Verify all tasks received the correct result
    for handle in handles {
        let result = handle.await.unwrap();
        assert_eq!(
            result.results[0].content, "shared-stress-result",
            "All tasks should receive the leader's result",
        );
    }

    assert_eq!(coalescer.in_flight_count(), 0);
}

/// B.2 variant: Multiple distinct queries coalesce independently under stress.
///
/// 10 different queries, each with 10 concurrent requests = 100 total tasks.
/// Verifies that each query group has exactly 1 leader.
#[tokio::test]
async fn test_stress_multiple_queries_independent_coalescing() {
    let num_queries = 10;
    let tasks_per_query = 10;
    let total_tasks = num_queries * tasks_per_query;

    let coalescer = Arc::new(RequestCoalescer::new());
    let barrier = Arc::new(tokio::sync::Barrier::new(total_tasks));

    // Track leaders per query
    let leader_counts: Vec<Arc<AtomicUsize>> = (0..num_queries)
        .map(|_| Arc::new(AtomicUsize::new(0)))
        .collect();

    let mut handles = Vec::with_capacity(total_tasks);

    for query_id in 0..num_queries {
        let hash = make_hash(&format!("multi-query-{}", query_id));
        let leader_count = leader_counts[query_id].clone();

        for _ in 0..tasks_per_query {
            let coalescer = coalescer.clone();
            let barrier = barrier.clone();
            let leader_count = leader_count.clone();

            handles.push(tokio::spawn(async move {
                barrier.wait().await;

                match coalescer.get_or_insert(hash) {
                    CoalesceAction::Leader => {
                        leader_count.fetch_add(1, Ordering::SeqCst);
                        // Simulate work then complete
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        coalescer.complete(&hash, Ok(Arc::new(make_response("multi-result"))));
                    }
                    CoalesceAction::Waiter(mut rx) => {
                        let _ = rx.recv().await;
                    }
                }
            }));
        }
    }

    for handle in handles {
        handle.await.unwrap();
    }

    // Each query should have exactly 1 leader
    for (query_id, leader_count) in leader_counts.iter().enumerate() {
        let leaders = leader_count.load(Ordering::SeqCst);
        assert_eq!(
            leaders, 1,
            "Query {} should have exactly 1 leader, got {}",
            query_id, leaders,
        );
    }

    // All requests should be cleaned up
    assert_eq!(coalescer.in_flight_count(), 0);
}
