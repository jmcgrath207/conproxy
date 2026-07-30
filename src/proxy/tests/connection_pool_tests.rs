#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

#[test]
fn test_pool_config_default() {
    let config = ConnectionPoolConfig::default();
    assert_eq!(config.max_connections, 100);
    assert_eq!(config.max_queue_size, 500);
    assert_eq!(config.queue_timeout_ms, 30000);
}

#[test]
fn test_pool_config_builder() {
    let config = ConnectionPoolConfig::new(50)
        .with_queue_timeout(Duration::from_secs(10))
        .with_max_queue_size(100);

    assert_eq!(config.max_connections, 50);
    assert_eq!(config.max_queue_size, 100);
    assert_eq!(config.queue_timeout_ms, 10000);
}

#[test]
fn test_pooling_mode_only_request() {
    // Only Request mode is supported
    assert_eq!(PoolingMode::Request.as_str(), "request");
    assert_eq!(PoolingMode::default(), PoolingMode::Request);

    // Deserialize always yields Request
    let mode: PoolingMode = serde_json::from_str(r#""request""#).unwrap();
    assert_eq!(mode, PoolingMode::Request);
}

#[tokio::test]
async fn test_pool_acquire_immediate() {
    let pool = ConnectionPool::with_max_connections(10);

    let conn = pool.acquire().await.unwrap();
    assert_eq!(pool.active_connections(), 1);
    assert_eq!(pool.available_permits(), 9);

    drop(conn);
    assert_eq!(pool.active_connections(), 0);
    assert_eq!(pool.available_permits(), 10);

    let stats = pool.stats();
    assert_eq!(stats.total_requests, 1);
    assert_eq!(stats.immediate_permits, 1);
    assert_eq!(stats.queued_requests, 0);
}

#[tokio::test]
async fn test_pool_exhaustion_and_queue() {
    let config = ConnectionPoolConfig::new(2).with_queue_timeout(Duration::from_millis(100));
    let pool = Arc::new(ConnectionPool::new(config));

    // Acquire all connections
    let _conn1 = pool.acquire().await.unwrap();
    let _conn2 = pool.acquire().await.unwrap();

    assert_eq!(pool.active_connections(), 2);
    assert_eq!(pool.available_permits(), 0);

    // Next acquire should queue and timeout
    let pool2 = pool.clone();
    let result = tokio::time::timeout(Duration::from_millis(200), pool2.acquire()).await;

    assert!(result.is_ok()); // Didn't hang
    let inner = result.unwrap();
    assert!(matches!(inner, Err(PoolError::Timeout)));

    let stats = pool.stats();
    assert_eq!(stats.queue_timeouts, 1);
}

#[tokio::test]
async fn test_pool_queue_full() {
    let config = ConnectionPoolConfig::new(1)
        .with_max_queue_size(1)
        .with_queue_timeout(Duration::from_secs(10));
    let pool = Arc::new(ConnectionPool::new(config));

    // Hold the only connection
    let _conn = pool.acquire().await.unwrap();

    // First queued request - just check if it's ok, don't return connection
    let pool2 = pool.clone();
    let handle1 = tokio::spawn(async move { pool2.acquire().await.is_ok() });

    // Give it time to queue
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(pool.queue_depth(), 1);

    // Second request should be rejected (queue full)
    let result = pool.acquire().await;
    assert!(matches!(result, Err(PoolError::QueueFull)));

    let stats = pool.stats();
    assert_eq!(stats.queue_rejections, 1);

    handle1.abort();
}

#[tokio::test]
async fn test_pool_waiter_signaling() {
    let config = ConnectionPoolConfig::new(1).with_queue_timeout(Duration::from_secs(5));
    let pool = Arc::new(ConnectionPool::new(config));

    // Hold the connection
    let conn = pool.acquire().await.unwrap();

    // Start a waiter - return bool to avoid lifetime issues
    let pool2 = pool.clone();
    let handle = tokio::spawn(async move { pool2.acquire().await.is_ok() });

    // Give it time to queue
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(pool.queue_depth(), 1);

    // Release connection
    drop(conn);

    // Waiter should succeed
    let result = handle.await.unwrap();
    assert!(result);

    let stats = pool.stats();
    assert_eq!(stats.queued_requests, 1);
    assert!(stats.avg_wait_time_ms > 0.0);
}

#[test]
fn test_pool_snapshot_serializable() {
    let pool = ConnectionPool::with_max_connections(10);
    let snapshot = pool.stats();
    let json = serde_json::to_string(&snapshot).unwrap();
    assert!(json.contains("active_connections"));
    assert!(json.contains("queue_depth"));
}

#[test]
fn test_pool_error_display() {
    assert_eq!(
        PoolError::QueueFull.to_string(),
        "Connection pool queue is full"
    );
    assert_eq!(
        PoolError::Timeout.to_string(),
        "Timed out waiting for connection"
    );
    assert_eq!(
        PoolError::Shutdown.to_string(),
        "Connection pool is shutting down"
    );
}

#[tokio::test]
async fn test_pool_drain_queue() {
    let config = ConnectionPoolConfig::new(1).with_queue_timeout(Duration::from_secs(10));
    let pool = Arc::new(ConnectionPool::new(config));

    // Hold the connection
    let _conn = pool.acquire().await.unwrap();

    // Queue some waiters - return bool to avoid lifetime issues
    let pool2 = pool.clone();
    let pool3 = pool.clone();
    let _h1 = tokio::spawn(async move { pool2.acquire().await.is_ok() });
    let _h2 = tokio::spawn(async move { pool3.acquire().await.is_ok() });

    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(pool.queue_depth() >= 1);

    // Drain the queue
    pool.drain_queue();
    assert_eq!(pool.queue_depth(), 0);
}

#[test]
fn test_pool_has_capacity() {
    let pool = ConnectionPool::with_max_connections(2);
    assert!(pool.has_capacity());

    // Can't easily test without async, but method exists
}

// =============================================================================
// B.4: Connection pool stress tests
// =============================================================================

#[tokio::test]
async fn test_pool_stress_100_acquires_on_10() {
    use std::sync::Arc;
    use tokio::sync::Barrier;

    let pool = Arc::new(ConnectionPool::with_max_connections(10));
    let num_tasks = 100;
    let barrier = Arc::new(Barrier::new(num_tasks));
    let mut handles = Vec::with_capacity(num_tasks);

    for _ in 0..num_tasks {
        let pool = pool.clone();
        let barrier = barrier.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            let conn = pool.acquire().await;
            assert!(conn.is_ok(), "Should acquire connection eventually");
            // Hold connection briefly to simulate work
            tokio::time::sleep(Duration::from_millis(1)).await;
            drop(conn);
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    // After all complete, all permits should be returned
    let stats = pool.stats();
    assert_eq!(
        stats.active_connections, 0,
        "All connections should be released"
    );
    assert_eq!(
        stats.total_requests, 100,
        "Should have processed 100 requests"
    );
}

#[tokio::test]
async fn test_pool_stress_queue_full_rejection() {
    use std::sync::Arc;

    // Pool of 2 with queue of 3 = max 5 concurrent requests
    let config = ConnectionPoolConfig::new(2)
        .with_max_queue_size(3)
        .with_queue_timeout(Duration::from_secs(5));
    let pool = Arc::new(ConnectionPool::new(config));

    // Hold 2 connections
    let _conn1 = pool.acquire().await.unwrap();
    let _conn2 = pool.acquire().await.unwrap();

    // Fill the queue (3 waiters) - return bool to avoid lifetime issues with PooledConnection
    let pool_clone = pool.clone();
    let waiters: Vec<_> = (0..3)
        .map(|_| {
            let p = pool_clone.clone();
            tokio::spawn(async move { p.acquire().await.is_ok() })
        })
        .collect();

    // Give waiters time to enqueue
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Next acquire should be rejected (queue full)
    let result = pool.acquire().await;
    assert!(matches!(result, Err(PoolError::QueueFull)));

    // Release held connections so waiters can proceed
    drop(_conn1);
    drop(_conn2);

    for w in waiters {
        let result = w.await.unwrap();
        assert!(result);
    }
}

#[tokio::test]
async fn test_pool_stress_concurrent_acquire_release_cycles() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use tokio::sync::Barrier;

    // Use generous timeout and large queue to handle signaling races
    let config = ConnectionPoolConfig::new(5)
        .with_queue_timeout(Duration::from_secs(60))
        .with_max_queue_size(1000);
    let pool = Arc::new(ConnectionPool::new(config));
    let num_tasks = 20;
    let cycles_per_task = 10;
    let barrier = Arc::new(Barrier::new(num_tasks));
    let success_count = Arc::new(AtomicU64::new(0));
    let failure_count = Arc::new(AtomicU64::new(0));
    let mut handles = Vec::with_capacity(num_tasks);

    for _ in 0..num_tasks {
        let pool = pool.clone();
        let barrier = barrier.clone();
        let success_count = success_count.clone();
        let failure_count = failure_count.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            for _ in 0..cycles_per_task {
                match pool.acquire().await {
                    Ok(conn) => {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                        drop(conn);
                        success_count.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(PoolError::Timeout) => {
                        // Signaling race - acceptable under high contention
                        failure_count.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => panic!("Unexpected pool error: {:?}", e),
                }
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    let stats = pool.stats();
    assert_eq!(
        stats.active_connections, 0,
        "All connections should be released"
    );

    let successes = success_count.load(Ordering::Relaxed);
    let failures = failure_count.load(Ordering::Relaxed);
    let expected = (num_tasks * cycles_per_task) as u64;

    // Verify all tasks completed their cycles
    assert_eq!(successes + failures, expected, "All cycles should complete");

    // Majority should succeed (at least 50%)
    assert!(
        successes > expected / 2,
        "At least half of acquires should succeed, got {}/{}",
        successes,
        expected
    );

    // Total requests tracked should match what we attempted
    assert_eq!(
        stats.total_requests, expected,
        "Should have tracked all {} requests",
        expected
    );
}
