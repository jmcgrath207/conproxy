#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

#[test]
fn test_query_stats_creation() {
    let stats = QueryStats::new(1000);
    assert_eq!(stats.request_count(), 1);
    assert_eq!(stats.first_seen, 1000);
}

#[test]
fn test_query_stats_record() {
    let stats = QueryStats::new(1000);

    stats.record(true, Duration::from_millis(50), 1001);
    stats.record(false, Duration::from_millis(100), 1002);

    assert_eq!(stats.request_count(), 3); // 1 initial + 2 recorded
    assert_eq!(stats.hits.load(Ordering::Relaxed), 1);
    assert_eq!(stats.misses.load(Ordering::Relaxed), 2); // 1 initial + 1 recorded
}

#[test]
fn test_query_stats_hit_rate() {
    let stats = QueryStats::new(1000);
    stats.record(true, Duration::from_millis(10), 1001);
    stats.record(true, Duration::from_millis(10), 1002);
    stats.record(false, Duration::from_millis(10), 1003);

    // 2 hits out of 4 requests (1 initial miss + 3 recorded)
    assert!((stats.hit_rate() - 0.5).abs() < 0.01);
}

#[test]
fn test_query_stats_latency() {
    let stats = QueryStats::new(1000);
    stats.record(true, Duration::from_millis(10), 1001);
    stats.record(true, Duration::from_millis(20), 1002);
    stats.record(true, Duration::from_millis(30), 1003);

    assert_eq!(stats.min_latency_us.load(Ordering::Relaxed), 10000);
    assert_eq!(stats.max_latency_us.load(Ordering::Relaxed), 30000);
}

#[test]
fn test_tracker_creation() {
    let tracker = QueryStatsTracker::new(100);
    assert!(tracker.is_empty());
    assert_eq!(tracker.len(), 0);
}

#[test]
fn test_tracker_record() {
    let tracker = QueryStatsTracker::new(100);

    tracker.record("query 1", true, Duration::from_millis(10));
    tracker.record("query 1", true, Duration::from_millis(20));
    tracker.record("query 2", false, Duration::from_millis(50));

    assert_eq!(tracker.len(), 2);

    let stats = tracker.get("query 1").unwrap();
    assert_eq!(stats.count, 2);
    assert_eq!(stats.hits, 2);
}

#[test]
fn test_tracker_top_by_count() {
    let tracker = QueryStatsTracker::new(100);

    for _ in 0..5 {
        tracker.record("popular", true, Duration::from_millis(10));
    }
    for _ in 0..2 {
        tracker.record("moderate", true, Duration::from_millis(10));
    }
    tracker.record("rare", true, Duration::from_millis(10));

    let top = tracker.top_by_count(2);
    assert_eq!(top.len(), 2);
    assert_eq!(top[0].query, "popular");
    assert_eq!(top[0].count, 5);
    assert_eq!(top[1].query, "moderate");
}

#[test]
fn test_tracker_top_by_latency() {
    let tracker = QueryStatsTracker::new(100);

    tracker.record("fast", true, Duration::from_millis(10));
    tracker.record("slow", true, Duration::from_millis(100));
    tracker.record("medium", true, Duration::from_millis(50));

    let top = tracker.top_by_latency(2);
    assert_eq!(top.len(), 2);
    assert_eq!(top[0].query, "slow");
    assert_eq!(top[1].query, "medium");
}

#[test]
fn test_tracker_low_hit_rate() {
    let tracker = QueryStatsTracker::new(100);

    // Good hit rate
    tracker.record("good", true, Duration::from_millis(10));
    tracker.record("good", true, Duration::from_millis(10));

    // Poor hit rate
    tracker.record("poor", false, Duration::from_millis(10));
    tracker.record("poor", false, Duration::from_millis(10));

    let low = tracker.low_hit_rate(0.5, 10);
    assert_eq!(low.len(), 1);
    assert_eq!(low[0].query, "poor");
}

#[test]
fn test_tracker_eviction() {
    let tracker = QueryStatsTracker::new(3);

    // Record queries - they all have the same timestamp initially
    tracker.record("query 1", true, Duration::from_millis(10));
    tracker.record("query 2", true, Duration::from_millis(10));
    tracker.record("query 3", true, Duration::from_millis(10));

    assert_eq!(tracker.len(), 3);

    // Wait to ensure different timestamp for the 4th query
    std::thread::sleep(Duration::from_millis(1100));

    // Adding a 4th should evict one of the oldest
    tracker.record("query 4", true, Duration::from_millis(10));

    assert_eq!(tracker.len(), 3);
    // query 4 should be present
    assert!(tracker.get("query 4").is_some());
    // At least one of the original queries should be gone
    let remaining = [
        tracker.get("query 1").is_some(),
        tracker.get("query 2").is_some(),
        tracker.get("query 3").is_some(),
    ]
    .iter()
    .filter(|&&x| x)
    .count();
    assert_eq!(remaining, 2);
}

#[test]
fn test_tracker_hash_only() {
    let tracker = QueryStatsTracker::hash_only(100);

    tracker.record("sensitive query", true, Duration::from_millis(10));

    // Can't get by original query (different hash)
    let stats = tracker.get("sensitive query");
    assert!(stats.is_some());
    assert!(stats.unwrap().query.starts_with("hash:"));
}

#[test]
fn test_aggregate_stats() {
    let tracker = QueryStatsTracker::new(100);

    tracker.record("query 1", true, Duration::from_millis(10));
    tracker.record("query 1", true, Duration::from_millis(20));
    tracker.record("query 2", false, Duration::from_millis(50));

    let agg = tracker.aggregate();
    assert_eq!(agg.unique_queries, 2);
    assert_eq!(agg.total_requests, 3);
    assert_eq!(agg.total_hits, 2);
    assert!((agg.hit_rate - 0.666).abs() < 0.01);
}

#[test]
fn test_tracker_clear() {
    let tracker = QueryStatsTracker::new(100);

    tracker.record("query", true, Duration::from_millis(10));
    assert_eq!(tracker.len(), 1);

    tracker.clear();
    assert!(tracker.is_empty());
}
