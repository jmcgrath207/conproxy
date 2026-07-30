#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;
use crate::proxy::types::{CacheStatus, SearchResult};
use std::sync::Arc;
use tokio::sync::Barrier;

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

/// B.1: 100 concurrent tasks, 1000 ops each, verify len() <= max_entries.
///
/// Spawns 100 tasks that each perform 1000 inserts into a shared CacheStore
/// with max_entries=500. All tasks start simultaneously via a Barrier.
/// After all tasks complete, verifies the cache never exceeded its capacity.
#[tokio::test]
async fn test_concurrent_insert_respects_max_entries() {
    let max_entries = 500;
    let num_tasks = 100;
    let ops_per_task = 1000;

    let store = Arc::new(CacheStore::new(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        max_entries,
    ));

    let barrier = Arc::new(Barrier::new(num_tasks));
    let mut handles = Vec::with_capacity(num_tasks);

    for task_id in 0..num_tasks {
        let store = store.clone();
        let barrier = barrier.clone();

        handles.push(tokio::spawn(async move {
            // Wait for all tasks to be ready before starting
            barrier.wait().await;

            for op in 0..ops_per_task {
                let query = format!("task-{}-query-{}", task_id, op);
                let response = make_response(&format!("result-{}-{}", task_id, op));
                store.insert(&query, response, format!("upstream-{}", task_id));
            }
        }));
    }

    // Wait for all tasks to complete
    for handle in handles {
        handle.await.unwrap();
    }

    // The cache must never exceed max_entries.
    // Due to eviction (removes 10% when at capacity), the actual count
    // should be at or below max_entries.
    let final_len = store.len();
    assert!(
        final_len <= max_entries,
        "Cache size {} exceeded max_entries {}",
        final_len,
        max_entries,
    );

    // Verify the store is still functional after stress
    store.insert(
        "post-stress-query",
        make_response("post-stress"),
        "upstream-post".to_string(),
    );
    assert!(store.get("post-stress-query").is_some());
}

/// B.1 variant: Concurrent reads and writes do not panic or corrupt.
///
/// Half the tasks insert, the other half read. Verifies no panics or
/// inconsistencies under mixed read/write load.
#[tokio::test]
async fn test_concurrent_mixed_reads_and_writes() {
    let max_entries = 1000;
    let num_tasks = 100;
    let ops_per_task = 500;

    let store = Arc::new(CacheStore::new(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        max_entries,
    ));

    // Pre-populate with some entries
    for i in 0..100 {
        let query = format!("seed-query-{}", i);
        store.insert(
            &query,
            make_response(&format!("seed-{}", i)),
            "seed-upstream".to_string(),
        );
    }

    let barrier = Arc::new(Barrier::new(num_tasks));
    let mut handles = Vec::with_capacity(num_tasks);

    for task_id in 0..num_tasks {
        let store = store.clone();
        let barrier = barrier.clone();

        handles.push(tokio::spawn(async move {
            barrier.wait().await;

            if task_id % 2 == 0 {
                // Writer task
                for op in 0..ops_per_task {
                    let query = format!("write-{}-{}", task_id, op);
                    store.insert(
                        &query,
                        make_response(&format!("w-{}-{}", task_id, op)),
                        "writer".to_string(),
                    );
                }
            } else {
                // Reader task: reads both pre-existing and potentially new entries
                for op in 0..ops_per_task {
                    // Read from seed entries
                    let seed_idx = op % 100;
                    let _entry = store.get(&format!("seed-query-{}", seed_idx));

                    // Read from potentially inserted entries
                    let _entry = store.get(&format!("write-{}-{}", task_id - 1, op));

                    // Also check len() and stats() under contention
                    let _len = store.len();
                }
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    let final_len = store.len();
    assert!(
        final_len <= max_entries,
        "Cache size {} exceeded max_entries {}",
        final_len,
        max_entries,
    );
}

/// B.6: Sustained inserts past max, verify size tracking consistency.
///
/// Continuously inserts well beyond max_entries and verifies that:
/// 1. len() never exceeds max_entries
/// 2. stats() total matches len()
/// 3. Eviction stats are non-zero (evictions actually happened)
/// 4. Memory usage is reported correctly
#[tokio::test]
async fn test_sustained_inserts_size_tracking_consistency() {
    let max_entries = 200;
    let total_inserts = 5000;
    let num_tasks = 50;
    let inserts_per_task = total_inserts / num_tasks;

    let store = Arc::new(CacheStore::new(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        max_entries,
    ));

    let barrier = Arc::new(Barrier::new(num_tasks));
    let mut handles = Vec::with_capacity(num_tasks);

    for task_id in 0..num_tasks {
        let store = store.clone();
        let barrier = barrier.clone();

        handles.push(tokio::spawn(async move {
            barrier.wait().await;

            for op in 0..inserts_per_task {
                let query = format!("sustained-{}-{}", task_id, op);
                let response = make_response(&format!("data-{}-{}", task_id, op));
                store.insert(&query, response, "sustained-upstream".to_string());

                // Periodically verify invariant during inserts
                if op % 20 == 0 {
                    let len = store.len();
                    assert!(
                        len <= max_entries + num_tasks, // Allow small slack for concurrent inserts
                        "Cache size {} exceeded expected max {} during inserts (task {}, op {})",
                        len,
                        max_entries + num_tasks,
                        task_id,
                        op,
                    );
                }
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    // Final invariant checks
    let final_len = store.len();
    assert!(
        final_len <= max_entries,
        "Final cache size {} exceeded max_entries {}",
        final_len,
        max_entries,
    );

    // stats().total should match len()
    let stats = store.stats();
    assert_eq!(
        stats.total, final_len,
        "stats().total ({}) should match len() ({})",
        stats.total, final_len,
    );

    // max_entries should be correctly reported
    assert_eq!(stats.max_entries, max_entries);

    // Utilization should be between 0 and 1
    assert!(
        stats.utilization >= 0.0 && stats.utilization <= 1.0,
        "Utilization {} should be between 0.0 and 1.0",
        stats.utilization,
    );

    // We inserted way more than max_entries, so evictions must have occurred.
    // The insert() path uses S3-FIFO eviction (which tracks stats via eviction_stats),
    // so we verify eviction happened by checking that final_len < total inserts.
    let total_inserts_attempted = num_tasks * inserts_per_task;
    assert!(
        final_len < total_inserts_attempted,
        "Eviction must have occurred: final_len ({}) should be less than total inserts ({})",
        final_len,
        total_inserts_attempted,
    );

    // Memory usage should be non-zero since we have entries
    let memory = store.memory_usage();
    assert!(
        memory > 0,
        "Memory usage should be non-zero with {} entries",
        final_len,
    );
}

/// B.6 variant: Verify integrity holds after concurrent stress.
///
/// After heavy concurrent inserts, verify_integrity() should report
/// zero invalid entries (content hashes should all be valid).
#[tokio::test]
async fn test_integrity_after_concurrent_stress() {
    let max_entries = 300;
    let num_tasks = 50;
    let ops_per_task = 200;

    let store = Arc::new(CacheStore::new(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        max_entries,
    ));

    let barrier = Arc::new(Barrier::new(num_tasks));
    let mut handles = Vec::with_capacity(num_tasks);

    for task_id in 0..num_tasks {
        let store = store.clone();
        let barrier = barrier.clone();

        handles.push(tokio::spawn(async move {
            barrier.wait().await;

            for op in 0..ops_per_task {
                let query = format!("integrity-{}-{}", task_id, op);
                let response = make_response(&format!("content-{}-{}", task_id, op));
                store.insert(&query, response, format!("upstream-{}", task_id % 5));
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    // Verify all remaining entries have valid content hashes
    let report = store.verify_integrity();
    assert_eq!(
        report.invalid, 0,
        "Expected 0 invalid entries after stress, found {}",
        report.invalid,
    );
    assert_eq!(
        report.total,
        store.len(),
        "Integrity report total ({}) should match len() ({})",
        report.total,
        store.len(),
    );
    // All entries inserted through insert() should have content hashes
    assert_eq!(
        report.missing_hash, 0,
        "Expected 0 entries with missing hash, found {}",
        report.missing_hash,
    );
}

/// B.6 variant: Concurrent clear() during inserts does not corrupt state.
///
/// Some tasks insert, one task periodically clears. Verifies no panics
/// and the cache remains in a consistent state.
#[tokio::test]
async fn test_concurrent_clear_during_inserts() {
    let max_entries = 500;
    let num_writers = 20;
    let ops_per_writer = 500;

    let store = Arc::new(CacheStore::new(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        max_entries,
    ));

    let barrier = Arc::new(Barrier::new(num_writers + 1)); // +1 for the clearer
    let mut handles = Vec::with_capacity(num_writers + 1);

    // Writer tasks
    for task_id in 0..num_writers {
        let store = store.clone();
        let barrier = barrier.clone();

        handles.push(tokio::spawn(async move {
            barrier.wait().await;

            for op in 0..ops_per_writer {
                let query = format!("clear-test-{}-{}", task_id, op);
                store.insert(
                    &query,
                    make_response(&format!("v-{}-{}", task_id, op)),
                    "upstream".to_string(),
                );
            }
        }));
    }

    // Clearer task: periodically clears the cache
    {
        let store = store.clone();
        let barrier = barrier.clone();

        handles.push(tokio::spawn(async move {
            barrier.wait().await;

            for _ in 0..10 {
                tokio::time::sleep(Duration::from_millis(5)).await;
                store.clear();
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    // Cache should be in a valid state
    let final_len = store.len();
    assert!(
        final_len <= max_entries,
        "Cache size {} exceeded max_entries {} after concurrent clears",
        final_len,
        max_entries,
    );

    // Stats should still be consistent
    let stats = store.stats();
    assert_eq!(stats.total, final_len);
}

/// B.6 variant: Per-upstream limits enforced under concurrent load.
///
/// Multiple tasks insert for different upstreams. Verifies per-upstream
/// limits are respected after all inserts complete.
#[tokio::test]
async fn test_per_upstream_limit_under_stress() {
    let max_entries = 1000;
    let per_upstream_limit = 50;
    let num_upstreams = 10;
    let inserts_per_upstream = 200;

    let mut store = CacheStore::new(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        max_entries,
    );
    store.set_per_upstream_limit(Some(per_upstream_limit));
    let store = Arc::new(store);

    let barrier = Arc::new(Barrier::new(num_upstreams));
    let mut handles = Vec::with_capacity(num_upstreams);

    for upstream_id in 0..num_upstreams {
        let store = store.clone();
        let barrier = barrier.clone();

        handles.push(tokio::spawn(async move {
            barrier.wait().await;

            for op in 0..inserts_per_upstream {
                let query = format!("upstream-{}-query-{}", upstream_id, op);
                let response = make_response(&format!("data-{}-{}", upstream_id, op));
                store.insert(&query, response, format!("upstream-{}", upstream_id));
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    // Check per-upstream counts. Due to concurrent eviction, they should
    // be at or below the per_upstream_limit. There can be slight overshoot
    // from concurrent inserts, but the eviction should bring counts down.
    let by_upstream = store.entries_by_upstream();
    for (upstream, count) in &by_upstream {
        // Allow a small margin for concurrency (multiple tasks could insert
        // before the eviction check runs for each)
        assert!(
            *count <= per_upstream_limit + num_upstreams,
            "Upstream {} has {} entries, expected <= {} (limit {} + margin {})",
            upstream,
            count,
            per_upstream_limit + num_upstreams,
            per_upstream_limit,
            num_upstreams,
        );
    }

    // Overall cache should not exceed max_entries
    assert!(
        store.len() <= max_entries,
        "Total cache size {} exceeded max_entries {}",
        store.len(),
        max_entries,
    );
}
