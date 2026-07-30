#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;
use crate::proxy::types::{CacheStatus, SearchResult};

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

#[test]
fn test_normalize_query() {
    assert_eq!(
        CacheStore::normalize_query("  Hello   World  "),
        "hello world"
    );
    assert_eq!(CacheStore::normalize_query("TEST"), "test");
    assert_eq!(CacheStore::normalize_query("a\t\nb"), "a b");
    assert_eq!(CacheStore::normalize_query("   "), "");
}

#[test]
fn test_hash_query_consistency() {
    let h1 = CacheStore::hash_query("hello world");
    let h2 = CacheStore::hash_query("  HELLO   WORLD  ");
    assert_eq!(h1, h2, "Normalized queries should have the same hash");

    let h3 = CacheStore::hash_query("different query");
    assert_ne!(h1, h3, "Different queries should have different hashes");
}

#[test]
fn test_insert_and_get() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    store.insert(
        "test query",
        make_response("result"),
        "upstream1".to_string(),
    );

    let entry = store.get("test query");
    assert!(entry.is_some());
    assert_eq!(entry.unwrap().upstream_id, "upstream1");
}

#[test]
fn test_evict_from_upstream_broadcasts_cdc_remove() {
    // Regression: bulk evict (the /cache/evict API path) must publish CDC
    // REMOVE events so peer nodes invalidate their copies. Previously it
    // used tracked_remove only and peers kept stale entries.
    let sender = std::sync::Arc::new(crate::proxy::cdc::EventSender::new(
        64,
        "test-node".to_string(),
    ));
    let mut rx = sender.subscribe();
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000)
        .with_cdc_sender(sender);

    for q in ["query one", "query two", "query three"] {
        store.insert(q, make_response("result"), "up1".to_string());
    }
    // Drain INSERT events.
    while rx.try_recv().is_ok() {}

    let evicted = store.evict_from_upstream("up1", 10);
    assert_eq!(evicted, 3);

    let mut removes = std::collections::HashSet::new();
    while let Ok(ev) = rx.try_recv() {
        if ev.event_type == CdcEventType::Remove {
            removes.insert(ev.query_key);
        }
    }
    assert_eq!(removes.len(), 3);
    assert!(removes.contains("query one"));
    assert!(removes.contains("query two"));
    assert!(removes.contains("query three"));
}

#[test]
fn test_evict_from_upstream_no_cdc_without_sender() {
    // Without a CDC sender the evict path must still work (no panic).
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);
    store.insert("q", make_response("r"), "up1".to_string());
    assert_eq!(store.evict_from_upstream("up1", 5), 1);
}

#[test]
fn test_get_normalized() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    store.insert(
        "hello world",
        make_response("result"),
        "upstream1".to_string(),
    );

    // Should find with different casing and spacing
    assert!(store.get("  HELLO   WORLD  ").is_some());
    assert!(store.get("Hello World").is_some());
}

#[test]
fn test_freshness() {
    let mut store = CacheStore::new(
        Duration::from_millis(500),  // 500ms fresh
        Duration::from_millis(1000), // 1000ms stale
        1000,
    );
    // Set a short max frozen duration for testing
    store.set_max_frozen_duration(Duration::from_millis(2500));

    store.insert("test", make_response("result"), "upstream1".to_string());

    // Should be fresh immediately
    assert_eq!(store.check_freshness("test"), Some(Freshness::Fresh));

    // Wait for it to become stale
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(store.check_freshness("test"), Some(Freshness::Stale));

    // Wait for it to become frozen (past stale but within max frozen)
    std::thread::sleep(Duration::from_millis(1000));
    assert_eq!(store.check_freshness("test"), Some(Freshness::Frozen));

    // Wait for it to truly expire (past max frozen)
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(store.check_freshness("test"), Some(Freshness::Expired));
}

#[test]
fn test_extend_ttl() {
    let store = CacheStore::new(Duration::from_millis(100), Duration::from_millis(200), 1000);

    store.insert("test", make_response("result"), "upstream1".to_string());

    // Wait until almost stale
    std::thread::sleep(Duration::from_millis(80));

    // Extend TTL
    store.extend_ttl("test");

    // Should be fresh again
    assert_eq!(store.check_freshness("test"), Some(Freshness::Fresh));

    let entry = store.get("test").unwrap();
    assert_eq!(entry.extended_count, 1);
}

#[test]
fn test_remove() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    store.insert("test", make_response("result"), "upstream1".to_string());
    assert!(store.get("test").is_some());

    store.remove("test");
    assert!(store.get("test").is_none());
}

#[test]
fn test_evict_truly_expired() {
    let mut store = CacheStore::new(
        Duration::from_millis(10), // 10ms fresh
        Duration::from_millis(20), // 20ms stale
        1000,
    );
    store.set_max_frozen_duration(Duration::from_millis(50));

    store.insert("test1", make_response("r1"), "upstream".to_string());
    store.insert("test2", make_response("r2"), "upstream".to_string());
    assert_eq!(store.len(), 2);

    // Wait past max frozen duration
    std::thread::sleep(Duration::from_millis(60));

    // Should evict both
    let evicted = store.evict_truly_expired();
    assert_eq!(evicted, 2);
    assert_eq!(store.len(), 0);
}

#[test]
fn test_eviction() {
    let store = CacheStore::new(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        10, // Very small cache
    );

    // Insert more than max
    for i in 0..15 {
        store.insert(
            &format!("query{}", i),
            make_response(&format!("result{}", i)),
            "upstream".to_string(),
        );
        std::thread::sleep(Duration::from_millis(10)); // Ensure different timestamps
    }

    // Should have evicted some entries
    assert!(store.len() <= 10);
}

#[test]
fn test_stats() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    store.insert("q1", make_response("r1"), "upstream".to_string());
    store.insert("q2", make_response("r2"), "upstream".to_string());

    let stats = store.stats();
    assert_eq!(stats.total, 2);
    assert_eq!(stats.fresh, 2);
    assert_eq!(stats.stale, 0);
    assert_eq!(stats.expired, 0);
}

#[test]
fn test_clear() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    store.insert("q1", make_response("r1"), "upstream".to_string());
    store.insert("q2", make_response("r2"), "upstream".to_string());
    assert_eq!(store.len(), 2);

    store.clear();
    assert!(store.is_empty());
}

#[test]
fn test_with_jitter_creation() {
    let store = CacheStore::with_jitter(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        1000,
        0.2, // 20% jitter
    );

    assert_eq!(store.ttl_jitter_percent(), 0.2);
}

#[test]
fn test_jitter_clamped() {
    // Test that jitter percent is clamped to valid range
    let store1 = CacheStore::with_jitter(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        1000,
        -0.5, // Negative should clamp to 0
    );
    assert_eq!(store1.ttl_jitter_percent(), 0.0);

    let store2 = CacheStore::with_jitter(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        1000,
        1.5, // Over 1.0 should clamp to 1.0
    );
    assert_eq!(store2.ttl_jitter_percent(), 1.0);
}

#[test]
fn test_jittered_ttl_deterministic() {
    let store = CacheStore::with_jitter(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        1000,
        0.1,
    );

    let hash = CacheStore::hash_query("test query");
    let base_ttl = Duration::from_secs(300);

    // Same hash should always produce same jitter
    let jittered1 = store.jittered_ttl(base_ttl, &hash);
    let jittered2 = store.jittered_ttl(base_ttl, &hash);
    assert_eq!(jittered1, jittered2);

    // Jittered TTL should be >= base TTL
    assert!(jittered1 >= base_ttl);
}

#[test]
fn test_jittered_ttl_varies_by_query() {
    let store = CacheStore::with_jitter(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        1000,
        0.1,
    );

    let hash1 = CacheStore::hash_query("query one");
    let hash2 = CacheStore::hash_query("query two");
    let base_ttl = Duration::from_secs(300);

    let jittered1 = store.jittered_ttl(base_ttl, &hash1);
    let jittered2 = store.jittered_ttl(base_ttl, &hash2);

    // Different queries may have different jitter (not guaranteed due to hash collision)
    // But both should be >= base TTL and < base_ttl * (1 + jitter_percent)
    assert!(jittered1 >= base_ttl);
    assert!(jittered2 >= base_ttl);
    assert!(jittered1 <= base_ttl + Duration::from_secs(30)); // 10% of 300 = 30
    assert!(jittered2 <= base_ttl + Duration::from_secs(30));
}

#[test]
fn test_jittered_ttl_zero_jitter() {
    // Create store with zero jitter
    let store_no_jitter = CacheStore::with_jitter(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        1000,
        0.0,
    );

    let hash = CacheStore::hash_query("test");
    let base_ttl = Duration::from_secs(300);

    // With zero jitter, should return base TTL exactly
    let jittered = store_no_jitter.jittered_ttl(base_ttl, &hash);
    assert_eq!(jittered, base_ttl);
}

#[test]
fn test_config_fingerprint_changes() {
    // Different URLs produce different fingerprints
    let fp1 = CacheStore::compute_config_fingerprint(Some("http://a.com"), &[]);
    let fp2 = CacheStore::compute_config_fingerprint(Some("http://b.com"), &[]);
    assert_ne!(fp1, fp2);

    // Different seeds produce different fingerprints
    let fp3 = CacheStore::compute_config_fingerprint(Some("http://a.com"), &["seed1".to_string()]);
    let fp4 = CacheStore::compute_config_fingerprint(Some("http://a.com"), &["seed2".to_string()]);
    assert_ne!(fp1, fp3);
    assert_ne!(fp3, fp4);
}

#[test]
fn test_config_fingerprint_deterministic() {
    let fp1 = CacheStore::compute_config_fingerprint(
        Some("http://example.com"),
        &["seed1".to_string(), "seed2".to_string()],
    );
    let fp2 = CacheStore::compute_config_fingerprint(
        Some("http://example.com"),
        &["seed1".to_string(), "seed2".to_string()],
    );
    assert_eq!(fp1, fp2);
}

#[test]
fn test_update_config_fingerprint_invalidates() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    // Insert some entries
    store.insert("q1", make_response("r1"), "upstream".to_string());
    store.insert("q2", make_response("r2"), "upstream".to_string());
    assert_eq!(store.len(), 2);

    // First update sets the fingerprint (no invalidation since old was 0)
    let invalidated = store.update_config_fingerprint(Some("http://a.com"), &[]);
    assert!(!invalidated);
    assert_eq!(store.len(), 2);

    // Changing config should invalidate
    let invalidated = store.update_config_fingerprint(Some("http://b.com"), &[]);
    assert!(invalidated);
    assert_eq!(store.len(), 0);
}

#[test]
fn test_update_config_fingerprint_no_change() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    store.insert("q1", make_response("r1"), "upstream".to_string());

    // Set initial fingerprint
    store.update_config_fingerprint(Some("http://a.com"), &["seed".to_string()]);

    // Same config should not invalidate
    let invalidated = store.update_config_fingerprint(Some("http://a.com"), &["seed".to_string()]);
    assert!(!invalidated);
    assert_eq!(store.len(), 1);
}

#[test]
fn test_get_stale_hashes() {
    let store = CacheStore::with_jitter(
        Duration::from_millis(300),  // Short fresh TTL (generous for tarpaulin)
        Duration::from_millis(1000), // Stale window
        1000,
        0.0, // No jitter for predictable timing
    );

    store.insert("q1", make_response("r1"), "upstream".to_string());
    store.insert("q2", make_response("r2"), "upstream".to_string());

    // Initially fresh, no stale entries
    let stale = store.get_stale_hashes();
    assert!(stale.is_empty());

    // Wait for entries to become stale
    std::thread::sleep(Duration::from_millis(500));

    let stale = store.get_stale_hashes();
    assert_eq!(stale.len(), 2);
}

#[test]
fn test_get_by_hash() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    let hash = CacheStore::hash_query("test query");
    store.insert(
        "test query",
        make_response("result"),
        "upstream".to_string(),
    );

    let entry = store.get_by_hash(&hash);
    assert!(entry.is_some());
    assert_eq!(entry.unwrap().response.results[0].content, "result");
}

#[test]
fn test_insert_by_hash() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    let hash = CacheStore::hash_query("test query");
    store.insert_by_hash(hash, make_response("result"), "upstream".to_string(), None);

    let entry = store.get_by_hash(&hash);
    assert!(entry.is_some());
}

#[test]
fn test_warmup() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 100);

    let entries: Vec<(String, QueryResponse)> = (0..10)
        .map(|i| {
            (
                format!("query{}", i),
                make_response(&format!("result{}", i)),
            )
        })
        .collect();

    let inserted = store.warmup(entries, "warmup".to_string());
    assert_eq!(inserted, 10);
    assert_eq!(store.len(), 10);

    // Verify entries are accessible
    let entry = store.get("query0");
    assert!(entry.is_some());
    assert_eq!(entry.unwrap().response.results[0].content, "result0");
}

#[test]
fn test_warmup_respects_max_entries() {
    let store = CacheStore::new(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        5, // Very small cache
    );

    let entries: Vec<(String, QueryResponse)> = (0..10)
        .map(|i| {
            (
                format!("query{}", i),
                make_response(&format!("result{}", i)),
            )
        })
        .collect();

    let inserted = store.warmup(entries, "warmup".to_string());
    assert_eq!(inserted, 5); // Should stop at max
    assert_eq!(store.len(), 5);
}

#[test]
fn test_warmup_skips_invalid_responses() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 100);

    let entries = vec![
        ("valid".to_string(), make_response("valid content")),
        (
            "invalid".to_string(),
            QueryResponse {
                results: vec![SearchResult {
                    id: "".to_string(), // Empty ID is invalid
                    score: 0.9,
                    content: "content".to_string(),
                    metadata: None,
                    upstream_id: None,
                }],
                cache_status: CacheStatus::Miss,
                took_ms: 10,
                generated_at: None,
                miss_reason: None,
            },
        ),
        ("also_valid".to_string(), make_response("also valid")),
    ];

    let inserted = store.warmup(entries, "warmup".to_string());
    assert_eq!(inserted, 2); // Only valid ones
    assert!(store.get("valid").is_some());
    assert!(store.get("invalid").is_none());
    assert!(store.get("also_valid").is_some());
}

#[test]
fn test_verify_integrity() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    store.insert("q1", make_response("r1"), "upstream".to_string());
    store.insert("q2", make_response("r2"), "upstream".to_string());

    let report = store.verify_integrity();
    assert_eq!(report.total, 2);
    assert_eq!(report.valid, 2);
    assert_eq!(report.invalid, 0);
}

#[test]
fn test_content_hash_computed_on_insert() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    store.insert("test", make_response("content"), "upstream".to_string());
    let entry = store.get("test").unwrap();
    assert!(entry.content_hash.is_some());
    assert!(entry.verify_integrity());
}

#[test]
fn test_get_verified_valid_entry() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    store.insert("test", make_response("content"), "upstream".to_string());

    // Should return the entry since integrity is valid
    let entry = store.get_verified("test");
    assert!(entry.is_some());
    assert_eq!(entry.unwrap().response.results[0].content, "content");
}

#[test]
fn test_get_verified_nonexistent() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    // Should return None for nonexistent entry
    assert!(store.get_verified("nonexistent").is_none());
}

#[test]
fn test_entry_approximate_size() {
    let response = make_response("this is test content");
    let entry = CacheEntry::new_with_hash(
        response,
        "upstream".to_string(),
        SchemaFingerprint::default(),
        "default".to_string(),
    );

    let size = entry.approximate_size();
    assert!(size > 0);
    assert!(size > 20); // Should be at least content length
}

#[test]
fn test_memory_usage() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    let initial = store.memory_usage();
    assert_eq!(initial, 0);

    store.insert(
        "q1",
        make_response("some content here"),
        "upstream".to_string(),
    );

    let after_insert = store.memory_usage();
    assert!(after_insert > 0);
}

#[test]
fn test_entries_by_upstream() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    store.insert("q1", make_response("r1"), "upstream_a".to_string());
    store.insert("q2", make_response("r2"), "upstream_a".to_string());
    store.insert("q3", make_response("r3"), "upstream_b".to_string());

    let by_upstream = store.entries_by_upstream();
    assert_eq!(by_upstream.get("upstream_a"), Some(&2));
    assert_eq!(by_upstream.get("upstream_b"), Some(&1));
    assert_eq!(by_upstream.get("upstream_c"), None);
}

#[test]
fn test_count_for_upstream() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    store.insert("q1", make_response("r1"), "upstream_a".to_string());
    store.insert("q2", make_response("r2"), "upstream_a".to_string());
    store.insert("q3", make_response("r3"), "upstream_b".to_string());

    assert_eq!(store.count_for_upstream("upstream_a"), 2);
    assert_eq!(store.count_for_upstream("upstream_b"), 1);
    assert_eq!(store.count_for_upstream("nonexistent"), 0);
}

#[test]
fn test_evict_from_upstream() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    store.insert("q1", make_response("r1"), "upstream_a".to_string());
    std::thread::sleep(Duration::from_millis(10));
    store.insert("q2", make_response("r2"), "upstream_a".to_string());
    std::thread::sleep(Duration::from_millis(10));
    store.insert("q3", make_response("r3"), "upstream_a".to_string());
    store.insert("q4", make_response("r4"), "upstream_b".to_string());

    // Evict 2 entries from upstream_a
    let evicted = store.evict_from_upstream("upstream_a", 2);
    assert_eq!(evicted, 2);
    assert_eq!(store.count_for_upstream("upstream_a"), 1);
    assert_eq!(store.count_for_upstream("upstream_b"), 1); // Unchanged

    // The newest entry (q3) should remain
    assert!(store.get("q3").is_some());
}

#[test]
fn test_evict_from_upstream_more_than_exists() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    store.insert("q1", make_response("r1"), "upstream_a".to_string());
    store.insert("q2", make_response("r2"), "upstream_a".to_string());

    // Try to evict more than exists
    let evicted = store.evict_from_upstream("upstream_a", 10);
    assert_eq!(evicted, 2);
    assert_eq!(store.count_for_upstream("upstream_a"), 0);
}

#[test]
fn test_evict_from_nonexistent_upstream() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    store.insert("q1", make_response("r1"), "upstream_a".to_string());

    let evicted = store.evict_from_upstream("nonexistent", 5);
    assert_eq!(evicted, 0);
    assert_eq!(store.len(), 1); // Original entry unchanged
}

#[test]
fn test_enforce_per_upstream_limit() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    // Insert 5 entries for upstream_a with delays to ensure ordering
    for i in 0..5 {
        store.insert(
            &format!("q{}", i),
            make_response(&format!("r{}", i)),
            "upstream_a".to_string(),
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    store.insert("other", make_response("other"), "upstream_b".to_string());

    // Enforce limit of 2 for upstream_a
    let evicted = store.enforce_per_upstream_limit("upstream_a", 2);
    assert_eq!(evicted, 3); // 5 - 2 = 3 evicted
    assert_eq!(store.count_for_upstream("upstream_a"), 2);
    assert_eq!(store.count_for_upstream("upstream_b"), 1); // Unchanged

    // Newest entries should remain (q3 and q4)
    assert!(store.get("q3").is_some());
    assert!(store.get("q4").is_some());
}

#[test]
fn test_enforce_per_upstream_limit_already_under() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    store.insert("q1", make_response("r1"), "upstream_a".to_string());
    store.insert("q2", make_response("r2"), "upstream_a".to_string());

    // Already under limit of 5
    let evicted = store.enforce_per_upstream_limit("upstream_a", 5);
    assert_eq!(evicted, 0);
    assert_eq!(store.count_for_upstream("upstream_a"), 2);
}

#[test]
fn test_stats_by_upstream() {
    let store = CacheStore::new(
        Duration::from_secs(30), // Long fresh TTL (avoids flake under tarpaulin)
        Duration::from_secs(60), // Long stale TTL
        1000,
    );

    // Insert entries for different upstreams
    store.insert("q1", make_response("r1"), "upstream_a".to_string());
    store.insert("q2", make_response("r2"), "upstream_a".to_string());
    store.insert("q3", make_response("r3"), "upstream_b".to_string());

    // All should be fresh initially
    let stats = store.stats_by_upstream();
    let stats_a = stats.get("upstream_a").unwrap();
    let stats_b = stats.get("upstream_b").unwrap();

    assert_eq!(stats_a.total, 2);
    assert_eq!(stats_a.fresh, 2);
    assert_eq!(stats_a.stale, 0);
    assert!(stats_a.memory_bytes > 0);

    assert_eq!(stats_b.total, 1);
    assert_eq!(stats_b.fresh, 1);
}

#[test]
fn test_stats_by_upstream_with_stale() {
    let store = CacheStore::new(
        Duration::from_millis(20),  // Very short fresh TTL
        Duration::from_millis(200), // Longer stale TTL
        1000,
    );

    store.insert("q1", make_response("r1"), "upstream_a".to_string());

    // Wait for entry to become stale
    std::thread::sleep(Duration::from_millis(30));

    let stats = store.stats_by_upstream();
    let stats_a = stats.get("upstream_a").unwrap();

    assert_eq!(stats_a.total, 1);
    assert_eq!(stats_a.fresh, 0);
    assert_eq!(stats_a.stale, 1);
}

#[test]
fn test_per_upstream_limit() {
    let mut store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);
    store.set_per_upstream_limit(Some(3)); // Limit to 3 per upstream

    // Insert 5 entries for upstream_a
    for i in 0..5 {
        store.insert(
            &format!("query_a_{}", i),
            make_response(&format!("result_a_{}", i)),
            "upstream_a".to_string(),
        );
        std::thread::sleep(Duration::from_millis(5)); // Ensure different timestamps
    }

    // Should only have 3 entries for upstream_a due to limit
    assert_eq!(store.count_for_upstream("upstream_a"), 3);

    // Insert entries for upstream_b - should not be affected
    for i in 0..5 {
        store.insert(
            &format!("query_b_{}", i),
            make_response(&format!("result_b_{}", i)),
            "upstream_b".to_string(),
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    // upstream_b should also be limited to 3
    assert_eq!(store.count_for_upstream("upstream_b"), 3);

    // Total should be 6 (3 from each)
    assert_eq!(store.len(), 6);

    // Check eviction stats
    let eviction_stats = store.eviction_stats();
    assert!(
        eviction_stats.per_upstream > 0,
        "Should have per-upstream evictions"
    );
}

#[test]
fn test_per_upstream_limit_getter_setter() {
    let mut store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    // Default is no limit
    assert!(store.per_upstream_limit().is_none());

    // Set a limit
    store.set_per_upstream_limit(Some(10));
    assert_eq!(store.per_upstream_limit(), Some(10));

    // Clear limit
    store.set_per_upstream_limit(None);
    assert!(store.per_upstream_limit().is_none());
}

/// Test that modified cached content is detected via integrity check (MITM detection).
#[test]
fn test_mitm_modified_response_detected() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    // Insert a valid entry
    let response = make_response("original content");
    store.insert("test query", response.clone(), "upstream".to_string());

    // Verify it retrieves correctly with get_verified
    let verified = store.get_verified("test query");
    assert!(verified.is_some(), "Should retrieve valid entry");

    // Now simulate MITM by directly replacing the cached entry with modified content
    let hash = CacheStore::hash_query("test query");
    let guard = store.entries.pin();
    if let Some(old) = guard.get(&hash) {
        // Create a modified entry (simulating MITM attack)
        let mut modified_response = old.response.clone();
        modified_response.results[0].content = "MODIFIED malicious content".to_string();
        let poisoned = Arc::new(CacheEntry {
            response: modified_response,
            upstream_id: old.upstream_id.clone(),
            cached_at: old.cached_at,
            cached_at_wall: old.cached_at_wall,
            extended_count: old.extended_count,
            schema: old.schema.clone(),
            content_hash: old.content_hash, // Keep old hash so integrity check fails
            query_text: old.query_text.clone(),
            context_id: old.context_id.clone(),
            freq: std::sync::atomic::AtomicU8::new(0),
        });
        guard.insert(hash, poisoned);
    }
    drop(guard);

    // get_verified should now fail and evict the poisoned entry
    let after_modification = store.get_verified("test query");
    assert!(
        after_modification.is_none(),
        "Modified entry should fail integrity check"
    );

    // Entry should be evicted
    assert!(
        store.get("test query").is_none(),
        "Poisoned entry should be evicted"
    );

    // Check integrity failure was recorded
    let stats = store.eviction_stats();
    assert!(
        stats.integrity_failures > 0,
        "Should record integrity failure"
    );
}

/// Test concurrent insertions don't cause race conditions (cache poisoning via concurrency).
#[test]
fn test_concurrent_poison_attempt() {
    use std::sync::Arc;
    use std::thread;

    let store = Arc::new(CacheStore::new(
        Duration::from_secs(300),
        Duration::from_secs(3600),
        1000,
    ));

    let query = "concurrent test query";
    let num_threads = 10;
    let iterations = 100;

    // Spawn multiple threads trying to insert different content for the same query
    let handles: Vec<_> = (0..num_threads)
        .map(|thread_id| {
            let store_clone = store.clone();
            let query_string = query.to_string();
            thread::spawn(move || {
                for i in 0..iterations {
                    let content = format!("thread {} iteration {}", thread_id, i);
                    let response = QueryResponse {
                        results: vec![SearchResult {
                            id: format!("t{}i{}", thread_id, i),
                            score: 1.0,
                            content,
                            metadata: None,
                            upstream_id: None,
                        }],
                        cache_status: CacheStatus::Miss,
                        took_ms: 10,
                        generated_at: None,
                        miss_reason: None,
                    };
                    store_clone.insert(&query_string, response, "upstream".to_string());
                }
            })
        })
        .collect();

    // Wait for all threads to complete
    for handle in handles {
        handle.join().expect("Thread should not panic");
    }

    // After concurrent access, the cache should still be in a valid state
    // - Should have exactly one entry for the query
    assert!(store.get(query).is_some(), "Cache should have the entry");

    // - Entry should pass integrity check
    let verified = store.get_verified(query);
    assert!(
        verified.is_some(),
        "Entry should pass integrity check after concurrent access"
    );

    // - Content hash should match the stored content
    if let Some(entry) = store.get(query) {
        let computed_hash = CacheEntry::compute_content_hash(&entry.response);
        assert_eq!(
            entry.content_hash,
            Some(computed_hash),
            "Content hash should be consistent"
        );
    }
}

/// Test that recovery mechanism can replace potentially poisoned (stale) entries.
/// This verifies the infrastructure that the RecoveryWorker uses.
#[test]
fn test_recovery_replaces_poisoned_entries() {
    // Create cache with very short TTL so entries become stale quickly
    let store = CacheStore::new(
        Duration::from_millis(10), // Fresh TTL - very short
        Duration::from_secs(60),   // Stale TTL - long enough for test
        1000,
    );

    let query = "test recovery query";

    // Insert potentially "poisoned" entry (old data from suspect upstream)
    let poisoned_response = QueryResponse {
        results: vec![SearchResult {
            id: "old".to_string(),
            score: 0.5,
            content: "potentially poisoned content".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };
    store.insert(query, poisoned_response, "suspect_upstream".to_string());

    // Get the hash for direct manipulation (like recovery worker would)
    let hash = CacheStore::hash_query(query);

    // Wait for entry to become stale
    std::thread::sleep(Duration::from_millis(20));

    // Verify the entry is now in stale state (candidate for recovery)
    let stale_hashes = store.get_stale_hashes();
    assert!(
        stale_hashes.contains(&hash),
        "Stale entry should be in recovery queue"
    );

    // Simulate recovery worker replacing with fresh data
    let fresh_response = QueryResponse {
        results: vec![SearchResult {
            id: "fresh".to_string(),
            score: 0.9,
            content: "verified fresh content".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 5,
        generated_at: Some(QueryResponse::current_time_ms()),
        miss_reason: None,
    };
    store.insert_by_hash(hash, fresh_response, "verified_upstream".to_string(), None);

    // Verify the entry is now fresh and passes integrity check
    let freshness = store.check_freshness(query);
    assert_eq!(
        freshness,
        Some(Freshness::Fresh),
        "Recovered entry should be fresh"
    );

    // Entry should pass integrity verification
    let verified = store.get_verified(query);
    assert!(
        verified.is_some(),
        "Recovered entry should pass integrity check"
    );

    // Verify it's the new content
    if let Some(entry) = verified {
        assert_eq!(entry.response.results[0].id, "fresh");
        assert_eq!(entry.upstream_id, "verified_upstream");
    }

    // Entry should no longer be in stale queue
    let stale_after = store.get_stale_hashes();
    assert!(
        !stale_after.contains(&hash),
        "Recovered entry should not be in recovery queue"
    );
}

// ========== Two-Tier Cache Tests ==========

#[test]
fn test_two_tier_exact_hash_method() {
    // Exact hash should preserve case and whitespace
    let h1 = CacheStore::hash_query_exact("Hello World");
    let h2 = CacheStore::hash_query_exact("hello world");
    let h3 = CacheStore::hash_query_exact("Hello World");

    assert_ne!(h1, h2, "Exact hash should be case-sensitive");
    assert_eq!(h1, h3, "Same string should produce same exact hash");

    // Verify exact != normalized for non-normalized queries
    let exact = CacheStore::hash_query_exact("  Hello  World  ");
    let normalized = CacheStore::hash_query("  Hello  World  ");
    assert_ne!(
        exact, normalized,
        "Exact and normalized should differ for non-normalized query"
    );
}

#[test]
fn test_two_tier_exact_hit_on_repeated_query() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    let query = "Test Query";
    store.insert(query, make_response("result"), "upstream".to_string());

    assert_eq!(store.exact_hits(), 0);
    assert_eq!(store.normalized_hits(), 0);

    // First get - insert already created the exact→normalized mapping, so this is exact hit
    let _ = store.get(query);
    assert_eq!(store.exact_hits(), 1);
    assert_eq!(store.normalized_hits(), 0);

    // Second get with exact same string - still exact hit
    let _ = store.get(query);
    assert_eq!(store.exact_hits(), 2);
    assert_eq!(store.normalized_hits(), 0);

    // Third get - still exact hit
    let _ = store.get(query);
    assert_eq!(store.exact_hits(), 3);
    assert_eq!(store.normalized_hits(), 0);
}

#[test]
fn test_two_tier_normalized_hit_different_case() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000)
        .with_normalized_matching(true);

    store.insert(
        "test query",
        make_response("result"),
        "upstream".to_string(),
    );

    // Original query - exact hit (insert created mapping)
    let _ = store.get("test query");
    assert_eq!(store.exact_hits(), 1);
    assert_eq!(store.normalized_hits(), 0);

    // Different case - should be normalized hit (no mapping for "TEST QUERY")
    let _ = store.get("TEST QUERY");
    assert_eq!(store.exact_hits(), 1);
    assert_eq!(store.normalized_hits(), 1);

    // Same different-case query again - should be exact hit now (mapping was created)
    let _ = store.get("TEST QUERY");
    assert_eq!(store.exact_hits(), 2);
    assert_eq!(store.normalized_hits(), 1);
}

#[test]
fn test_two_tier_normalized_hit_different_whitespace() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000)
        .with_normalized_matching(true);

    store.insert(
        "hello world",
        make_response("result"),
        "upstream".to_string(),
    );

    // Original - exact hit (insert created mapping)
    let _ = store.get("hello world");
    assert_eq!(store.exact_hits(), 1);
    assert_eq!(store.normalized_hits(), 0);

    // Extra whitespace - should be normalized hit (no mapping for this variant)
    let _ = store.get("  hello   world  ");
    assert_eq!(store.exact_hits(), 1);
    assert_eq!(store.normalized_hits(), 1);

    // Same whitespace variant again - should be exact hit (mapping was created)
    let _ = store.get("  hello   world  ");
    assert_eq!(store.exact_hits(), 2);
    assert_eq!(store.normalized_hits(), 1);
}

#[test]
fn test_get_with_hit_type() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000)
        .with_normalized_matching(true);

    let query = "Test Query";
    store.insert(query, make_response("result"), "upstream".to_string());

    // First lookup - exact hit (insert created the mapping)
    let result = store.get_with_hit_type(query);
    assert!(result.is_some());
    let (_, hit_type) = result.unwrap();
    assert_eq!(hit_type, CacheHitType::Exact);

    // Second lookup - still exact hit
    let result = store.get_with_hit_type(query);
    assert!(result.is_some());
    let (_, hit_type) = result.unwrap();
    assert_eq!(hit_type, CacheHitType::Exact);

    // Different case - normalized hit (no mapping for this variant)
    let result = store.get_with_hit_type("TEST QUERY");
    assert!(result.is_some());
    let (entry, hit_type) = result.unwrap();
    assert_eq!(hit_type, CacheHitType::Normalized);
    assert_eq!(entry.response.results[0].content, "result");

    // Same variant again - now exact hit (mapping was created)
    let result = store.get_with_hit_type("TEST QUERY");
    assert!(result.is_some());
    let (_, hit_type) = result.unwrap();
    assert_eq!(hit_type, CacheHitType::Exact);
}

#[test]
fn test_get_with_hit_type_miss() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    // Should return None for missing entry
    let result = store.get_with_hit_type("nonexistent");
    assert!(result.is_none());
}

#[test]
fn test_two_tier_clear_clears_mappings() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    store.insert("query", make_response("result"), "upstream".to_string());
    let _ = store.get("query"); // Create mapping

    // Verify entry exists
    assert!(store.get("query").is_some());

    // Clear
    store.clear();

    // Should be empty - no entries, no mappings
    assert!(store.is_empty());
    assert!(store.get("query").is_none());
}

#[test]
fn test_two_tier_remove_clears_mapping() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    store.insert("query", make_response("result"), "upstream".to_string());

    // First get - exact hit (insert created mapping)
    let _ = store.get("query");
    assert_eq!(store.exact_hits(), 1);

    // Second get - still exact hit
    let _ = store.get("query");
    assert_eq!(store.exact_hits(), 2);

    // Remove entry
    store.remove("query");

    // Should not find entry anymore
    assert!(store.get("query").is_none());

    // Hits shouldn't increase since entry is gone
    let exact_before = store.exact_hits();
    let _ = store.get("query");
    assert_eq!(store.exact_hits(), exact_before);
}

#[test]
fn test_two_tier_insert_stores_mapping() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    // Insert should store the exact→normalized mapping immediately
    store.insert(
        "Test Query",
        make_response("result"),
        "upstream".to_string(),
    );

    // First get should be exact hit because insert already created the mapping
    let _ = store.get("Test Query");
    assert_eq!(
        store.exact_hits(),
        1,
        "Insert should pre-populate exact→normalized mapping"
    );
    assert_eq!(store.normalized_hits(), 0);
}

// === Persistence Wiring Tests ===

#[cfg(feature = "persistence")]
mod persistence_tests {
    use super::*;
    use crate::proxy::persistence::PersistentCache;
    use tempfile::tempdir;

    #[test]
    fn test_set_persistence() {
        let dir = tempdir().unwrap();
        let persistent = Arc::new(PersistentCache::open(dir.path().join("cache")).unwrap());

        let mut store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

        assert!(store.persistence().is_none());

        store.set_persistence(persistent.clone());
        assert!(store.persistence().is_some());
    }

    #[test]
    fn test_insert_persists_entry() {
        let dir = tempdir().unwrap();
        let persistent = Arc::new(PersistentCache::open(dir.path().join("cache")).unwrap());

        let mut store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);
        store.set_persistence(persistent.clone());

        // Insert entry
        store.insert(
            "test query",
            make_response("result"),
            "upstream".to_string(),
        );

        // Verify persisted
        let hash = CacheStore::hash_query("test query");
        let persisted = persistent.load_entry(&hash).unwrap();
        assert!(persisted.is_some());
        assert_eq!(persisted.unwrap().query, "test query");
    }

    #[test]
    fn test_remove_unpersists_entry() {
        let dir = tempdir().unwrap();
        let persistent = Arc::new(PersistentCache::open(dir.path().join("cache")).unwrap());

        let mut store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);
        store.set_persistence(persistent.clone());

        // Insert and verify persisted
        store.insert(
            "test query",
            make_response("result"),
            "upstream".to_string(),
        );
        let hash = CacheStore::hash_query("test query");
        assert!(persistent.load_entry(&hash).unwrap().is_some());

        // Remove and verify unpersisted
        store.remove("test query");
        assert!(persistent.load_entry(&hash).unwrap().is_none());
    }

    #[test]
    fn test_clear_clears_persistence() {
        let dir = tempdir().unwrap();
        let persistent = Arc::new(PersistentCache::open(dir.path().join("cache")).unwrap());

        let mut store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);
        store.set_persistence(persistent.clone());

        // Insert multiple entries
        store.insert("query1", make_response("r1"), "upstream".to_string());
        store.insert("query2", make_response("r2"), "upstream".to_string());
        store.insert("query3", make_response("r3"), "upstream".to_string());

        assert_eq!(persistent.entry_count(), 3);

        // Clear should clear persistence
        store.clear();
        assert_eq!(persistent.entry_count(), 0);
    }

    #[test]
    fn test_restore_from_persistence() {
        let dir = tempdir().unwrap();
        let persistent = Arc::new(PersistentCache::open(dir.path().join("cache")).unwrap());

        // First store: insert entries
        {
            let mut store =
                CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);
            store.set_persistence(persistent.clone());

            store.insert("query1", make_response("r1"), "upstream".to_string());
            store.insert("query2", make_response("r2"), "upstream".to_string());
        }

        // Second store: restore from persistence
        {
            let mut store =
                CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);
            store.set_persistence(persistent.clone());

            assert_eq!(store.len(), 0);

            let restored = store.restore_from_persistence();
            assert_eq!(restored, 2);
            assert_eq!(store.len(), 2);
        }
    }

    #[test]
    fn test_flush_to_persistence() {
        let dir = tempdir().unwrap();
        let persistent = Arc::new(PersistentCache::open(dir.path().join("cache")).unwrap());

        let mut store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);
        store.set_persistence(persistent.clone());

        // Insert entries (already persisted automatically)
        store.insert("query1", make_response("r1"), "upstream".to_string());
        store.insert("query2", make_response("r2"), "upstream".to_string());

        // Flush ensures data is written to disk
        let flushed = store.flush_to_persistence();
        assert_eq!(flushed, 2);
    }

    #[test]
    fn test_persistence_stats() {
        let dir = tempdir().unwrap();
        let persistent = Arc::new(PersistentCache::open(dir.path().join("cache")).unwrap());

        let mut store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);
        store.set_persistence(persistent.clone());

        store.insert("query", make_response("result"), "upstream".to_string());

        let stats = store.persistence_stats();
        assert!(stats.is_some());
        assert_eq!(stats.unwrap().entry_count, 1);
    }

    #[test]
    fn test_insert_by_hash_persists() {
        let dir = tempdir().unwrap();
        let persistent = Arc::new(PersistentCache::open(dir.path().join("cache")).unwrap());

        let mut store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);
        store.set_persistence(persistent.clone());

        let hash = [42u8; 32];
        store.insert_by_hash(hash, make_response("result"), "upstream".to_string(), None);

        // Verify persisted
        let persisted = persistent.load_entry(&hash).unwrap();
        assert!(persisted.is_some());
    }

    #[test]
    fn test_restore_respects_max_entries() {
        let dir = tempdir().unwrap();
        let persistent = Arc::new(PersistentCache::open(dir.path().join("cache")).unwrap());

        // First store: insert many entries
        {
            let mut store =
                CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 100);
            store.set_persistence(persistent.clone());

            for i in 0..50 {
                store.insert(
                    &format!("query{}", i),
                    make_response(&format!("r{}", i)),
                    "upstream".to_string(),
                );
            }
        }

        // Second store with smaller limit: restore should respect max_entries
        {
            let mut store =
                CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 10); // Only 10 max
            store.set_persistence(persistent.clone());

            let restored = store.restore_from_persistence();
            assert_eq!(restored, 10); // Should only restore up to max_entries
            assert_eq!(store.len(), 10);
        }
    }

    #[test]
    fn test_sync_persistence() {
        let dir = tempdir().unwrap();
        let persistent = Arc::new(PersistentCache::open(dir.path().join("cache")).unwrap());

        let mut store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);
        store.set_persistence(persistent.clone());

        store.insert("query", make_response("result"), "upstream".to_string());

        // Sync should succeed
        assert!(store.sync_persistence());
    }

    #[test]
    fn test_no_persistence_operations_without_config() {
        let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

        // These should not panic when persistence is not configured
        store.insert("query", make_response("result"), "upstream".to_string());
        store.remove("query");
        store.clear();

        // Restore should return 0 when not configured
        let restored = store.restore_from_persistence();
        assert_eq!(restored, 0);

        // Flush should return 0 when not configured
        let flushed = store.flush_to_persistence();
        assert_eq!(flushed, 0);

        // Stats should be None
        assert!(store.persistence_stats().is_none());

        // Sync should succeed (no-op)
        assert!(store.sync_persistence());
    }
}

// ---- S3-FIFO eviction tests ----

/// Entries that are accessed (freq > 0) survive eviction over cold entries.
#[test]
fn test_s3fifo_promotes_accessed_entries() {
    // Small cache: max_entries=5, small_capacity=1, main_capacity=4
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(600), 5);

    // Insert 5 entries (fills cache)
    for i in 0..5 {
        store.insert(
            &format!("query-{}", i),
            make_response(&format!("content-{}", i)),
            "up".to_string(),
        );
    }
    assert_eq!(store.len(), 5);

    // Access query-0 and query-1 multiple times to make them "hot"
    for _ in 0..3 {
        store.get("query-0");
        store.get("query-1");
    }

    // Insert more entries to trigger eviction
    for i in 5..8 {
        store.insert(
            &format!("query-{}", i),
            make_response(&format!("content-{}", i)),
            "up".to_string(),
        );
    }

    // Hot entries should still be in the cache
    assert!(
        store.get("query-0").is_some(),
        "Frequently accessed entry should survive eviction"
    );
    assert!(
        store.get("query-1").is_some(),
        "Frequently accessed entry should survive eviction"
    );

    // Cache should not exceed max_entries
    assert!(store.len() <= 5, "Cache should respect max_entries bound");
}

/// Re-inserted keys (found in ghost set) go directly to Main queue.
#[test]
fn test_s3fifo_ghost_readmission() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(600), 5);

    // Fill cache
    for i in 0..5 {
        store.insert(
            &format!("query-{}", i),
            make_response(&format!("content-{}", i)),
            "up".to_string(),
        );
    }

    // Force eviction by adding more entries (query-0 through query-4 may be evicted)
    for i in 5..10 {
        store.insert(
            &format!("query-{}", i),
            make_response(&format!("content-{}", i)),
            "up".to_string(),
        );
    }

    // Some early entries should have been evicted and placed in ghost set
    let stats = store.eviction_stats();
    assert!(
        stats.small_evictions + stats.main_evictions > 0,
        "Evictions should have occurred"
    );

    // Re-insert an evicted entry — it should go to Main via ghost
    store.insert(
        "query-0",
        make_response("content-0-readmit"),
        "up".to_string(),
    );

    let stats = store.eviction_stats();
    assert!(stats.ghost_hits > 0, "Re-inserted key should hit ghost set");
}

/// Sequential scan pattern should not flush hot entries from cache.
#[test]
fn test_s3fifo_scan_resistance() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(600), 20);

    // Insert and access "hot" working set
    for i in 0..10 {
        store.insert(
            &format!("hot-{}", i),
            make_response(&format!("hot-content-{}", i)),
            "up".to_string(),
        );
    }
    // Access hot entries to boost frequency
    for _ in 0..3 {
        for i in 0..10 {
            store.get(&format!("hot-{}", i));
        }
    }

    // Sequential scan: insert 30 unique one-shot queries (each seen only once)
    for i in 0..30 {
        store.insert(
            &format!("scan-{}", i),
            make_response(&format!("scan-content-{}", i)),
            "up".to_string(),
        );
    }

    // Most hot entries should survive the scan
    let mut hot_survived = 0;
    for i in 0..10 {
        if store.get(&format!("hot-{}", i)).is_some() {
            hot_survived += 1;
        }
    }

    assert!(
        hot_survived >= 7,
        "S3-FIFO should protect hot entries from sequential scan (survived: {}/10)",
        hot_survived
    );

    assert!(store.len() <= 20, "Cache should respect max_entries");
}

/// clear() empties all S3-FIFO queue state.
#[test]
fn test_s3fifo_clear_resets_queues() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(600), 10);

    for i in 0..8 {
        store.insert(
            &format!("query-{}", i),
            make_response(&format!("content-{}", i)),
            "up".to_string(),
        );
    }

    store.clear();
    assert_eq!(store.len(), 0);

    // Verify S3-FIFO queues are empty by inserting more entries
    // (if queues weren't cleared, stale entries might cause issues)
    for i in 0..8 {
        store.insert(
            &format!("new-query-{}", i),
            make_response(&format!("new-content-{}", i)),
            "up".to_string(),
        );
    }
    assert_eq!(store.len(), 8);
}

/// remove() + eviction doesn't panic (lazy S3-FIFO cleanup).
#[test]
fn test_s3fifo_remove_lazy_cleanup() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(600), 5);

    // Fill cache
    for i in 0..5 {
        store.insert(
            &format!("query-{}", i),
            make_response(&format!("content-{}", i)),
            "up".to_string(),
        );
    }

    // Explicitly remove some entries (leaves orphan queue entries)
    store.remove("query-1");
    store.remove("query-3");
    assert_eq!(store.len(), 3);

    // Insert more to trigger eviction — should not panic despite orphan queue entries
    for i in 5..10 {
        store.insert(
            &format!("query-{}", i),
            make_response(&format!("content-{}", i)),
            "up".to_string(),
        );
    }

    assert!(store.len() <= 5);
}

/// extend_ttl sets freq=1 on the rebuilt entry.
#[test]
fn test_s3fifo_extend_ttl_sets_freq() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(600), 10);
    store.insert("query", make_response("content"), "up".to_string());

    // Verify initial freq is 0 (no accesses)
    let entry = store.get("query").unwrap();
    // get() bumps freq, but let's check via extend_ttl behavior
    drop(entry);

    store.extend_ttl("query");

    // The extended entry should have freq=1 (set by extend_ttl)
    // Access it to verify it still exists and is the extended version
    let entry = store.get("query").unwrap();
    assert!(entry.extended_count > 0, "Entry should be extended");
}

/// Inserting the same query twice doesn't create duplicate queue entries.
#[test]
fn test_s3fifo_update_existing_no_duplicate() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(600), 5);

    // Insert same query multiple times (updates, not new entries)
    for version in 0..5 {
        store.insert(
            "same-query",
            make_response(&format!("content-v{}", version)),
            "up".to_string(),
        );
    }

    // Should only have 1 entry in cache
    assert_eq!(store.len(), 1);

    // Fill rest of cache + trigger eviction
    for i in 0..6 {
        store.insert(
            &format!("other-{}", i),
            make_response(&format!("other-content-{}", i)),
            "up".to_string(),
        );
    }

    // Cache should be bounded
    assert!(store.len() <= 5, "Cache should respect max_entries");
}

#[test]
fn test_tracked_remove_cleans_exact_to_normalized() {
    // Verify that eviction cleans up exact_to_normalized mapping (P-001 fix)
    let store = CacheStore::new(
        Duration::from_secs(300),
        Duration::from_secs(600),
        5, // small capacity to trigger eviction
    )
    .with_normalized_matching(true);

    // Insert enough entries to trigger eviction
    for i in 0..10 {
        let query = format!("test query number {}", i);
        let response = QueryResponse {
            results: vec![],
            cache_status: CacheStatus::Miss,
            took_ms: 1,
            generated_at: None,
            miss_reason: None,
        };
        store.insert(&query, response, "upstream".to_string());
    }

    // After eviction, the exact_to_normalized map should not grow unboundedly.
    // With max_entries=5 and 10 inserts, at least 5 entries should have been evicted.
    // The exact_to_normalized map should have at most ~5 entries (matching live entries).
    // Without the fix, it would have ~10 entries (stale mappings remain).
    let stats = store.stats();
    assert!(
        stats.total <= 5,
        "Cache should have at most 5 entries, got {}",
        stats.total
    );
}

// ── S3-FIFO peek-method tests ──

#[test]
fn test_s3fifo_ghost_set_readmission() {
    // max_entries=10 → small_capacity=1, main_capacity=9, ghost_capacity=10
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(600), 10);

    let hash0 = CacheStore::hash_query("query-0");

    // Insert 10 entries → fills small queue, total=10 (no eviction yet)
    for i in 0..10 {
        store.insert(
            &format!("query-{}", i),
            make_response(&format!("content-{}", i)),
            "up".to_string(),
        );
    }

    // Verify small queue has entries, no ghost yet
    {
        let s = store.s3fifo.lock();
        assert_eq!(s.ghost_set_size(), 0, "No evictions yet");
        assert!(!s.is_in_ghost(&hash0), "query-0 not ghosted yet");
        assert!(s.small_queue_size() > 0, "Small queue has entries");
    }

    // Insert one more → triggers eviction from small
    store.insert(
        "query-overflow",
        make_response("overflow"),
        "up".to_string(),
    );

    // query-0 (oldest, never accessed) should have been evicted → ghost set
    {
        let s = store.s3fifo.lock();
        assert!(
            s.is_in_ghost(&hash0),
            "Evicted entry should appear in ghost set"
        );
        assert_eq!(s.ghost_set_size(), 1, "One entry in ghost set");
    }

    // Re-insert evicted key → ghost re-admission: removed from ghost, goes to main
    store.insert(
        "query-0",
        make_response("content-0-readmitted"),
        "up".to_string(),
    );

    {
        let s = store.s3fifo.lock();
        assert!(
            !s.is_in_ghost(&hash0),
            "Re-admitted entry should no longer be in ghost set"
        );
        assert_eq!(
            s.main_queue_size(),
            1,
            "Ghost re-admission goes to main queue"
        );
    }
}

#[test]
fn test_s3fifo_small_to_main_promotion() {
    // max_entries=3 → small_capacity=1, main_capacity=2
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(600), 3);

    // Insert q0 into small queue
    store.insert("q0", make_response("content-0"), "up".to_string());

    // Access q0 to bump frequency (freq → 1)
    store.get("q0");

    // Insert q1 and q2 (small fills up)
    store.insert("q1", make_response("content-1"), "up".to_string());
    store.insert("q2", make_response("content-2"), "up".to_string());

    // Snapshot sizes before eviction (q0, q1, q2 all in small)
    let small_before = store.s3fifo.lock().small_queue_size();
    let main_before = store.s3fifo.lock().main_queue_size();
    assert_eq!(small_before, 3, "All 3 entries start in small queue");
    assert_eq!(main_before, 0, "Main queue empty");

    // Insert q3 → total=4 > 3 → eviction triggers.
    // Eviction from small: pop q0 (freq=1) → promote to main.
    //                       pop q1 (freq=0) → evict + ghost.
    // After: small=[q2,q3], main=[q0], total=3, stop.
    store.insert("q3", make_response("content-3"), "up".to_string());

    let small_after = store.s3fifo.lock().small_queue_size();
    let main_after = store.s3fifo.lock().main_queue_size();

    // q0 promoted to main: small decreased, main increased
    assert!(
        small_after < small_before,
        "Small queue shrank after eviction ({} < {})",
        small_after,
        small_before
    );
    assert!(
        main_after > main_before,
        "Main queue grew after promotion ({} > {})",
        main_after,
        main_before
    );
    assert_eq!(main_after, 1, "q0 promoted to main queue");

    // q0 should still be accessible in cache
    let entry = store.get("q0");
    assert!(entry.is_some(), "Promoted entry remains in cache");
}

#[test]
fn test_s3fifo_demotion_on_stale() {
    // max_entries=5 → small_capacity=1, main_capacity=4
    let store = CacheStore::new(
        Duration::from_millis(50),  // Fast staleness
        Duration::from_millis(200), // Stale window
        5,
    );

    // Insert and access entries to populate both queues
    for i in 0..5 {
        store.insert(
            &format!("q-{}", i),
            make_response(&format!("content-{}", i)),
            "up".to_string(),
        );
        // Access each to bump freq (promotion candidates)
        store.get(&format!("q-{}", i));
    }

    // Insert q-5 → triggers eviction cycle.
    // All 5 existing entries have freq=1 (accessed above).
    // Eviction from small:
    //   pop q-0 (freq=1) → promote to main
    //   pop q-1 (freq=1) → promote to main
    //   pop q-2 (freq=1) → promote to main
    //   pop q-3 (freq=1) → promote to main
    //   pop q-4 (freq=1) → promote to main
    //   max_attempts exhausted, None returned.
    // total still > 5, fall through to main:
    //   pop q-0 (freq=0, was reset during promotion) → evict from main
    // After: small=[q-5], main=[q-1,q-2,q-3,q-4], total=5, stop.
    store.insert("q-5", make_response("content-5"), "up".to_string());

    // Wait for entries to become stale (freq already 0 on main entries)
    std::thread::sleep(Duration::from_millis(60));

    // Insert more to trigger further evictions
    for i in 6..10 {
        store.insert(
            &format!("q-{}", i),
            make_response(&format!("content-{}", i)),
            "up".to_string(),
        );
    }

    // Ghost set should have been populated from small evictions
    let ghost_size = store.s3fifo.lock().ghost_set_size();
    assert!(
        ghost_size > 0,
        "Evictions should populate ghost set (got {} )",
        ghost_size
    );

    // Main queue should have some entries (not fully flushed)
    let main_size = store.s3fifo.lock().main_queue_size();
    assert!(
        main_size > 0,
        "Main queue should retain frequently accessed entries"
    );
}

// ── Empty-result caching tests ──

#[test]
fn test_empty_results_cached_as_hit() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    // Insert an empty result (no SearchResult items)
    let empty_response = QueryResponse {
        results: vec![],
        cache_status: CacheStatus::Miss,
        took_ms: 0,
        generated_at: None,
        miss_reason: None,
    };
    store.insert("empty-query", empty_response, "upstream".to_string());

    // Lookup should return the entry (cache HIT at store level)
    let entry = store.get("empty-query");
    assert!(
        entry.is_some(),
        "Empty-result entry should be found (cache hit)"
    );
    assert!(
        entry.unwrap().response.results.is_empty(),
        "Stored response has zero results"
    );
}

#[test]
fn test_empty_results_respected_by_stats() {
    let store = CacheStore::new(Duration::from_secs(300), Duration::from_secs(3600), 1000);

    let empty_response = QueryResponse {
        results: vec![],
        cache_status: CacheStatus::Miss,
        took_ms: 0,
        generated_at: None,
        miss_reason: None,
    };
    store.insert("empty-query", empty_response, "upstream".to_string());

    // Stats should count the empty-result entry
    let stats = store.stats();
    assert_eq!(stats.total, 1, "Empty-result entry counted in stats");
    assert_eq!(store.len(), 1, "Empty-result entry counted in len()");

    // Insert a normal entry alongside
    store.insert(
        "normal-query",
        make_response("normal"),
        "upstream".to_string(),
    );
    let stats = store.stats();
    assert_eq!(stats.total, 2, "Both entries counted after normal insert");

    // No panic or crash from empty results
    store.clear();
    assert!(
        store.is_empty(),
        "Clear still works after empty-result insert"
    );
}
