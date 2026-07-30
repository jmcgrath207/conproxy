use super::*;
use tempfile::tempdir;

#[test]
fn test_open_persistent_cache() {
    let dir = tempdir().unwrap();
    let cache = PersistentCache::open(dir.path().join("cache.redb")).unwrap();
    assert_eq!(cache.entry_count(), 0);
}

#[test]
fn test_store_and_load_entry() {
    let dir = tempdir().unwrap();
    let cache = PersistentCache::open(dir.path().join("cache.redb")).unwrap();

    let query_hash = [1u8; 32];
    let entry = PersistedEntry {
        query: "test query".to_string(),
        upstream_id: "upstream1".to_string(),
        response: PersistedResponse {
            results: vec![PersistedResult {
                id: "doc1".to_string(),
                content: "Test content".to_string(),
                score: 0.95,
                metadata: None,
            }],
            cache_status: "miss".to_string(),
            took_ms: 50,
            generated_at: Some(1700000000000),
        },
        cached_at_ms: current_time_ms(),
        extended_count: 0,
        content_hash: [2u8; 32],
    };

    cache.store_entry(&query_hash, &entry).unwrap();

    let loaded = cache.load_entry(&query_hash).unwrap().unwrap();
    assert_eq!(loaded.query, "test query");
    assert_eq!(loaded.upstream_id, "upstream1");
    assert_eq!(loaded.response.results.len(), 1);
    assert_eq!(loaded.response.results[0].id, "doc1");
}

#[test]
fn test_remove_entry() {
    let dir = tempdir().unwrap();
    let cache = PersistentCache::open(dir.path().join("cache.redb")).unwrap();

    let query_hash = [3u8; 32];
    let entry = PersistedEntry {
        query: "test".to_string(),
        upstream_id: "u1".to_string(),
        response: PersistedResponse {
            results: vec![],
            cache_status: "miss".to_string(),
            took_ms: 10,
            generated_at: None,
        },
        cached_at_ms: current_time_ms(),
        extended_count: 0,
        content_hash: [0u8; 32],
    };

    cache.store_entry(&query_hash, &entry).unwrap();
    assert!(cache.load_entry(&query_hash).unwrap().is_some());

    cache.remove_entry(&query_hash).unwrap();
    assert!(cache.load_entry(&query_hash).unwrap().is_none());
}

#[test]
fn test_all_entry_hashes() {
    let dir = tempdir().unwrap();
    let cache = PersistentCache::open(dir.path().join("cache.redb")).unwrap();

    let entry = PersistedEntry {
        query: "test".to_string(),
        upstream_id: "u1".to_string(),
        response: PersistedResponse {
            results: vec![],
            cache_status: "miss".to_string(),
            took_ms: 10,
            generated_at: None,
        },
        cached_at_ms: current_time_ms(),
        extended_count: 0,
        content_hash: [0u8; 32],
    };

    cache.store_entry(&[1u8; 32], &entry).unwrap();
    cache.store_entry(&[2u8; 32], &entry).unwrap();
    cache.store_entry(&[3u8; 32], &entry).unwrap();

    let hashes = cache.all_entry_hashes().unwrap();
    assert_eq!(hashes.len(), 3);
}

#[test]
fn test_store_and_load_health() {
    let dir = tempdir().unwrap();
    let cache = PersistentCache::open(dir.path().join("cache.redb")).unwrap();

    let health = PersistedUpstreamHealth {
        upstream_id: "upstream1".to_string(),
        status: "online".to_string(),
        consecutive_failures: 0,
        consecutive_successes: 5,
        last_success_ms: Some(1700000000000),
        last_failure_ms: None,
    };

    cache.store_health("upstream1", &health).unwrap();

    let loaded = cache.load_health("upstream1").unwrap().unwrap();
    assert_eq!(loaded.upstream_id, "upstream1");
    assert_eq!(loaded.status, "online");
    assert_eq!(loaded.consecutive_successes, 5);
}

#[test]
fn test_all_health_states() {
    let dir = tempdir().unwrap();
    let cache = PersistentCache::open(dir.path().join("cache.redb")).unwrap();

    let health1 = PersistedUpstreamHealth {
        upstream_id: "u1".to_string(),
        status: "online".to_string(),
        consecutive_failures: 0,
        consecutive_successes: 1,
        last_success_ms: None,
        last_failure_ms: None,
    };

    let health2 = PersistedUpstreamHealth {
        upstream_id: "u2".to_string(),
        status: "offline".to_string(),
        consecutive_failures: 3,
        consecutive_successes: 0,
        last_success_ms: None,
        last_failure_ms: Some(1700000000000),
    };

    cache.store_health("u1", &health1).unwrap();
    cache.store_health("u2", &health2).unwrap();

    let states = cache.all_health_states().unwrap();
    assert_eq!(states.len(), 2);
}

#[test]
fn test_flush_and_last_flush_time() {
    let dir = tempdir().unwrap();
    let cache = PersistentCache::open(dir.path().join("cache.redb")).unwrap();

    assert!(cache.last_flush_time().unwrap().is_none());

    cache.flush().unwrap();

    let last_flush = cache.last_flush_time().unwrap().unwrap();
    assert!(last_flush > 0);
}

#[test]
fn test_clear_entries() {
    let dir = tempdir().unwrap();
    let cache = PersistentCache::open(dir.path().join("cache.redb")).unwrap();

    let entry = PersistedEntry {
        query: "test".to_string(),
        upstream_id: "u1".to_string(),
        response: PersistedResponse {
            results: vec![],
            cache_status: "miss".to_string(),
            took_ms: 10,
            generated_at: None,
        },
        cached_at_ms: current_time_ms(),
        extended_count: 0,
        content_hash: [0u8; 32],
    };

    cache.store_entry(&[1u8; 32], &entry).unwrap();
    cache.store_entry(&[2u8; 32], &entry).unwrap();

    assert_eq!(cache.entry_count(), 2);

    let cleared = cache.clear_entries().unwrap();
    assert_eq!(cleared, 2);
    assert_eq!(cache.entry_count(), 0);
}

#[test]
fn test_stats() {
    let dir = tempdir().unwrap();
    let cache = PersistentCache::open(dir.path().join("cache.redb")).unwrap();

    let entry = PersistedEntry {
        query: "test".to_string(),
        upstream_id: "u1".to_string(),
        response: PersistedResponse {
            results: vec![],
            cache_status: "miss".to_string(),
            took_ms: 10,
            generated_at: None,
        },
        cached_at_ms: current_time_ms(),
        extended_count: 0,
        content_hash: [0u8; 32],
    };

    cache.store_entry(&[1u8; 32], &entry).unwrap();

    let health = PersistedUpstreamHealth {
        upstream_id: "u1".to_string(),
        status: "online".to_string(),
        consecutive_failures: 0,
        consecutive_successes: 1,
        last_success_ms: None,
        last_failure_ms: None,
    };
    cache.store_health("u1", &health).unwrap();

    cache.flush().unwrap();

    let stats = cache.stats();
    assert_eq!(stats.entry_count, 1);
    assert_eq!(stats.health_record_count, 1);
}

#[test]
fn test_persisted_response_conversion() {
    let response = QueryResponse {
        results: vec![SearchResult {
            id: "doc1".to_string(),
            content: "Test content".to_string(),
            score: 0.95,
            metadata: Some(serde_json::json!({"key": "value"})),
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 50,
        generated_at: Some(1700000000000),
        miss_reason: None,
    };

    let persisted: PersistedResponse = (&response).into();
    assert_eq!(persisted.cache_status, "miss");
    assert_eq!(persisted.results.len(), 1);

    let restored: QueryResponse = (&persisted).into();
    assert_eq!(restored.cache_status, CacheStatus::Miss);
    assert_eq!(restored.results.len(), 1);
    assert_eq!(restored.results[0].id, "doc1");
}

#[test]
fn test_persistence_error_display() {
    let err = PersistenceError::Serialize("test error".to_string());
    assert!(err.to_string().contains("Serialization"));
    assert!(err.to_string().contains("test error"));
}

// --- Context-prefixed persistence tests ---

#[test]
fn test_store_and_load_entry_for_context() {
    let dir = tempdir().unwrap();
    let cache = PersistentCache::open(dir.path().join("cache.redb")).unwrap();

    let query_hash = [1u8; 32];
    let entry = PersistedEntry {
        query: "test query".to_string(),
        upstream_id: "upstream1".to_string(),
        response: PersistedResponse {
            results: vec![PersistedResult {
                id: "doc1".to_string(),
                content: "Test content".to_string(),
                score: 0.95,
                metadata: None,
            }],
            cache_status: "miss".to_string(),
            took_ms: 50,
            generated_at: Some(1700000000000),
        },
        cached_at_ms: current_time_ms(),
        extended_count: 0,
        content_hash: [2u8; 32],
    };

    cache
        .store_entry_for_context("project-a", &query_hash, &entry)
        .unwrap();

    let loaded = cache
        .load_entry_for_context("project-a", &query_hash)
        .unwrap()
        .unwrap();
    assert_eq!(loaded.query, "test query");

    let not_found = cache
        .load_entry_for_context("project-b", &query_hash)
        .unwrap();
    assert!(not_found.is_none());
}

#[test]
fn test_remove_entry_for_context() {
    let dir = tempdir().unwrap();
    let cache = PersistentCache::open(dir.path().join("cache.redb")).unwrap();

    let query_hash = [3u8; 32];
    let entry = PersistedEntry {
        query: "test".to_string(),
        upstream_id: "u1".to_string(),
        response: PersistedResponse {
            results: vec![],
            cache_status: "miss".to_string(),
            took_ms: 10,
            generated_at: None,
        },
        cached_at_ms: current_time_ms(),
        extended_count: 0,
        content_hash: [0u8; 32],
    };

    cache
        .store_entry_for_context("ctx1", &query_hash, &entry)
        .unwrap();
    assert!(cache
        .load_entry_for_context("ctx1", &query_hash)
        .unwrap()
        .is_some());

    cache.remove_entry_for_context("ctx1", &query_hash).unwrap();
    assert!(cache
        .load_entry_for_context("ctx1", &query_hash)
        .unwrap()
        .is_none());
}

#[test]
fn test_entries_for_context() {
    let dir = tempdir().unwrap();
    let cache = PersistentCache::open(dir.path().join("cache.redb")).unwrap();

    let entry = PersistedEntry {
        query: "test".to_string(),
        upstream_id: "u1".to_string(),
        response: PersistedResponse {
            results: vec![],
            cache_status: "miss".to_string(),
            took_ms: 10,
            generated_at: None,
        },
        cached_at_ms: current_time_ms(),
        extended_count: 0,
        content_hash: [0u8; 32],
    };

    cache
        .store_entry_for_context("ctx1", &[1u8; 32], &entry)
        .unwrap();
    cache
        .store_entry_for_context("ctx1", &[2u8; 32], &entry)
        .unwrap();
    cache
        .store_entry_for_context("ctx2", &[3u8; 32], &entry)
        .unwrap();

    let hashes = cache.entries_for_context("ctx1").unwrap();
    assert_eq!(hashes.len(), 2);

    let hashes2 = cache.entries_for_context("ctx2").unwrap();
    assert_eq!(hashes2.len(), 1);
}

#[test]
fn test_entry_count_for_context() {
    let dir = tempdir().unwrap();
    let cache = PersistentCache::open(dir.path().join("cache.redb")).unwrap();

    let entry = PersistedEntry {
        query: "test".to_string(),
        upstream_id: "u1".to_string(),
        response: PersistedResponse {
            results: vec![],
            cache_status: "miss".to_string(),
            took_ms: 10,
            generated_at: None,
        },
        cached_at_ms: current_time_ms(),
        extended_count: 0,
        content_hash: [0u8; 32],
    };

    cache
        .store_entry_for_context("ctx-a", &[1u8; 32], &entry)
        .unwrap();
    cache
        .store_entry_for_context("ctx-a", &[2u8; 32], &entry)
        .unwrap();
    cache
        .store_entry_for_context("ctx-b", &[3u8; 32], &entry)
        .unwrap();

    assert_eq!(cache.entry_count_for_context("ctx-a"), 2);
    assert_eq!(cache.entry_count_for_context("ctx-b"), 1);
    assert_eq!(cache.entry_count_for_context("ctx-c"), 0);
}

#[test]
fn test_clear_context() {
    let dir = tempdir().unwrap();
    let cache = PersistentCache::open(dir.path().join("cache.redb")).unwrap();

    let entry = PersistedEntry {
        query: "test".to_string(),
        upstream_id: "u1".to_string(),
        response: PersistedResponse {
            results: vec![],
            cache_status: "miss".to_string(),
            took_ms: 10,
            generated_at: None,
        },
        cached_at_ms: current_time_ms(),
        extended_count: 0,
        content_hash: [0u8; 32],
    };

    cache
        .store_entry_for_context("ctx-x", &[1u8; 32], &entry)
        .unwrap();
    cache
        .store_entry_for_context("ctx-x", &[2u8; 32], &entry)
        .unwrap();
    cache
        .store_entry_for_context("ctx-y", &[3u8; 32], &entry)
        .unwrap();

    let cleared = cache.clear_context("ctx-x").unwrap();
    assert_eq!(cleared, 2);

    assert_eq!(cache.entry_count_for_context("ctx-x"), 0);
    assert_eq!(cache.entry_count_for_context("ctx-y"), 1);
}

#[test]
fn test_context_isolation() {
    let dir = tempdir().unwrap();
    let cache = PersistentCache::open(dir.path().join("cache.redb")).unwrap();

    let query_hash = [42u8; 32];

    let entry_a = PersistedEntry {
        query: "test".to_string(),
        upstream_id: "upstream-a".to_string(),
        response: PersistedResponse {
            results: vec![],
            cache_status: "miss".to_string(),
            took_ms: 10,
            generated_at: None,
        },
        cached_at_ms: current_time_ms(),
        extended_count: 0,
        content_hash: [0u8; 32],
    };

    let entry_b = PersistedEntry {
        query: "test".to_string(),
        upstream_id: "upstream-b".to_string(),
        response: PersistedResponse {
            results: vec![],
            cache_status: "miss".to_string(),
            took_ms: 20,
            generated_at: None,
        },
        cached_at_ms: current_time_ms(),
        extended_count: 0,
        content_hash: [1u8; 32],
    };

    cache
        .store_entry_for_context("context-a", &query_hash, &entry_a)
        .unwrap();
    cache
        .store_entry_for_context("context-b", &query_hash, &entry_b)
        .unwrap();

    let loaded_a = cache
        .load_entry_for_context("context-a", &query_hash)
        .unwrap()
        .unwrap();
    assert_eq!(loaded_a.upstream_id, "upstream-a");

    let loaded_b = cache
        .load_entry_for_context("context-b", &query_hash)
        .unwrap()
        .unwrap();
    assert_eq!(loaded_b.upstream_id, "upstream-b");
}

// --- Context metadata persistence tests ---

#[test]
fn test_store_and_load_context() {
    let dir = tempdir().unwrap();
    let cache = PersistentCache::open(dir.path().join("cache.redb")).unwrap();

    let metadata = ContextMetadata::new("project-a", "http://localhost:6333", "docs");

    cache.store_context(&metadata).unwrap();

    let loaded = cache.load_context("project-a").unwrap().unwrap();
    assert_eq!(loaded.id, "project-a");
    assert_eq!(loaded.upstream_url, "http://localhost:6333");
    assert_eq!(loaded.collection, "docs");
}

#[test]
fn test_load_nonexistent_context() {
    let dir = tempdir().unwrap();
    let cache = PersistentCache::open(dir.path().join("cache.redb")).unwrap();

    let result = cache.load_context("nonexistent").unwrap();
    assert!(result.is_none());
}

#[test]
fn test_all_contexts() {
    let dir = tempdir().unwrap();
    let cache = PersistentCache::open(dir.path().join("cache.redb")).unwrap();

    let ctx1 = ContextMetadata::new("ctx1", "http://host1:6333", "coll1");
    let ctx2 = ContextMetadata::new("ctx2", "http://host2:6333", "coll2");
    let ctx3 = ContextMetadata::new("ctx3", "http://host3:6333", "coll3");

    cache.store_context(&ctx1).unwrap();
    cache.store_context(&ctx2).unwrap();
    cache.store_context(&ctx3).unwrap();

    let all = cache.all_contexts().unwrap();
    assert_eq!(all.len(), 3);

    let ids: Vec<_> = all.iter().map(|c| c.id.as_str()).collect();
    assert!(ids.contains(&"ctx1"));
    assert!(ids.contains(&"ctx2"));
    assert!(ids.contains(&"ctx3"));
}

#[test]
fn test_remove_context() {
    let dir = tempdir().unwrap();
    let cache = PersistentCache::open(dir.path().join("cache.redb")).unwrap();

    let metadata = ContextMetadata::new("temp-ctx", "http://localhost", "test");
    cache.store_context(&metadata).unwrap();

    assert!(cache.load_context("temp-ctx").unwrap().is_some());

    cache.remove_context("temp-ctx").unwrap();
    assert!(cache.load_context("temp-ctx").unwrap().is_none());
}

#[test]
fn test_context_count() {
    let dir = tempdir().unwrap();
    let cache = PersistentCache::open(dir.path().join("cache.redb")).unwrap();

    assert_eq!(cache.context_count(), 0);

    cache
        .store_context(&ContextMetadata::new("a", "", ""))
        .unwrap();
    cache
        .store_context(&ContextMetadata::new("b", "", ""))
        .unwrap();

    assert_eq!(cache.context_count(), 2);

    cache.remove_context("a").unwrap();
    assert_eq!(cache.context_count(), 1);
}

fn make_test_entry(query: &str, upstream: &str) -> PersistedEntry {
    PersistedEntry {
        query: query.to_string(),
        upstream_id: upstream.to_string(),
        response: PersistedResponse {
            results: vec![],
            cache_status: "miss".to_string(),
            took_ms: 10,
            generated_at: None,
        },
        cached_at_ms: current_time_ms(),
        extended_count: 0,
        content_hash: [0u8; 32],
    }
}

#[test]
fn test_cache_survives_restart() {
    let dir = tempdir().unwrap();
    let cache_path = dir.path().join("restart_test.redb");

    let hash1 = [1u8; 32];
    let hash2 = [2u8; 32];

    // First "session"
    {
        let cache = PersistentCache::open(&cache_path).unwrap();
        cache
            .store_entry(&hash1, &make_test_entry("query1", "upstream1"))
            .unwrap();
        cache
            .store_entry(&hash2, &make_test_entry("query2", "upstream2"))
            .unwrap();

        let ctx = ContextMetadata::new("test-ctx", "http://test", "collection");
        cache.store_context(&ctx).unwrap();

        let health1 = PersistedUpstreamHealth {
            upstream_id: "upstream1".to_string(),
            status: "online".to_string(),
            consecutive_failures: 0,
            consecutive_successes: 5,
            last_success_ms: Some(1700000000000),
            last_failure_ms: None,
        };
        let health2 = PersistedUpstreamHealth {
            upstream_id: "upstream2".to_string(),
            status: "offline".to_string(),
            consecutive_failures: 3,
            consecutive_successes: 0,
            last_success_ms: None,
            last_failure_ms: Some(1700000000000),
        };
        cache.store_health("upstream1", &health1).unwrap();
        cache.store_health("upstream2", &health2).unwrap();

        assert_eq!(cache.entry_count(), 2);
        assert_eq!(cache.context_count(), 1);
        cache.flush().unwrap();
    }

    // Second "session"
    {
        let cache = PersistentCache::open(&cache_path).unwrap();
        assert_eq!(cache.entry_count(), 2);

        let loaded1 = cache.load_entry(&hash1).unwrap();
        assert!(loaded1.is_some());
        assert_eq!(loaded1.unwrap().query, "query1");

        let loaded2 = cache.load_entry(&hash2).unwrap();
        assert!(loaded2.is_some());
        assert_eq!(loaded2.unwrap().query, "query2");

        assert_eq!(cache.context_count(), 1);
        let ctx = cache.load_context("test-ctx").unwrap();
        assert!(ctx.is_some());
        assert_eq!(ctx.unwrap().upstream_url, "http://test");

        let health1 = cache.load_health("upstream1").unwrap();
        assert!(health1.is_some());
        assert_eq!(health1.unwrap().status, "online");

        let health2 = cache.load_health("upstream2").unwrap();
        assert!(health2.is_some());
        assert_eq!(health2.unwrap().status, "offline");
    }
}

#[test]
fn test_multiple_restarts_preserve_data() {
    let dir = tempdir().unwrap();
    let cache_path = dir.path().join("multi_restart.redb");

    let hash1 = [10u8; 32];
    let hash2 = [20u8; 32];
    let hash3 = [30u8; 32];

    {
        let cache = PersistentCache::open(&cache_path).unwrap();
        cache
            .store_entry(&hash1, &make_test_entry("q1", "u1"))
            .unwrap();
        cache.flush().unwrap();
    }

    {
        let cache = PersistentCache::open(&cache_path).unwrap();
        assert_eq!(cache.entry_count(), 1);
        cache
            .store_entry(&hash2, &make_test_entry("q2", "u2"))
            .unwrap();
        cache.flush().unwrap();
    }

    {
        let cache = PersistentCache::open(&cache_path).unwrap();
        assert_eq!(cache.entry_count(), 2);
        cache
            .store_entry(&hash3, &make_test_entry("q3", "u3"))
            .unwrap();
        cache.flush().unwrap();
    }

    {
        let cache = PersistentCache::open(&cache_path).unwrap();
        assert_eq!(cache.entry_count(), 3);

        assert!(cache.load_entry(&hash1).unwrap().is_some());
        assert!(cache.load_entry(&hash2).unwrap().is_some());
        assert!(cache.load_entry(&hash3).unwrap().is_some());
    }
}

#[test]
fn test_restart_after_clear() {
    let dir = tempdir().unwrap();
    let cache_path = dir.path().join("clear_restart.redb");

    let hash1 = [100u8; 32];
    let hash2 = [200u8; 32];

    {
        let cache = PersistentCache::open(&cache_path).unwrap();
        cache
            .store_entry(&hash1, &make_test_entry("q1", "u1"))
            .unwrap();
        cache
            .store_entry(&hash2, &make_test_entry("q2", "u2"))
            .unwrap();
        assert_eq!(cache.entry_count(), 2);

        cache.clear_entries().unwrap();
        assert_eq!(cache.entry_count(), 0);
        cache.flush().unwrap();
    }

    {
        let cache = PersistentCache::open(&cache_path).unwrap();
        assert_eq!(cache.entry_count(), 0);
        assert!(cache.load_entry(&hash1).unwrap().is_none());
        assert!(cache.load_entry(&hash2).unwrap().is_none());
    }
}

// === context_key helper ===

#[test]
fn test_context_key_format() {
    let hash = [0xABu8; 32];
    let key = PersistentCache::context_key("my-ctx", &hash);
    assert!(key.starts_with(b"ctx:my-ctx:"));
    assert_eq!(key.len(), 4 + "my-ctx".len() + 1 + 32);
}

#[test]
fn test_context_key_different_contexts() {
    let hash = [1u8; 32];
    let key_a = PersistentCache::context_key("ctx-a", &hash);
    let key_b = PersistentCache::context_key("ctx-b", &hash);
    assert_ne!(key_a, key_b);
}

#[test]
fn test_context_key_different_hashes() {
    let hash1 = [1u8; 32];
    let hash2 = [2u8; 32];
    let key1 = PersistentCache::context_key("ctx", &hash1);
    let key2 = PersistentCache::context_key("ctx", &hash2);
    assert_ne!(key1, key2);
}

// === From impls (QueryResponse ↔ PersistedResponse) ===

#[test]
fn test_query_response_to_persisted_response() {
    use crate::proxy::types::{CacheStatus, QueryResponse, SearchResult};

    let response = QueryResponse {
        results: vec![
            SearchResult {
                id: "doc1".to_string(),
                score: 0.95,
                content: "Hello".to_string(),
                metadata: None,
                upstream_id: None,
            },
            SearchResult {
                id: "doc2".to_string(),
                score: 0.8,
                content: "World".to_string(),
                metadata: Some(serde_json::json!({"key": "value"})),
                upstream_id: None,
            },
        ],
        cache_status: CacheStatus::Hit,
        took_ms: 42,
        generated_at: Some(1700000000000),
        miss_reason: None,
    };

    let persisted = PersistedResponse::from(&response);
    assert_eq!(persisted.results.len(), 2);
    assert_eq!(persisted.results[0].id, "doc1");
    assert_eq!(persisted.results[0].score, 0.95);
    assert_eq!(persisted.cache_status, "hit");
    assert_eq!(persisted.took_ms, 42);
    assert_eq!(persisted.generated_at, Some(1700000000000));
}

#[test]
fn test_persisted_response_to_query_response() {
    let persisted = PersistedResponse {
        results: vec![PersistedResult {
            id: "doc1".to_string(),
            content: "Content here".to_string(),
            score: 0.7,
            metadata: None,
        }],
        cache_status: "stale".to_string(),
        took_ms: 100,
        generated_at: None,
    };

    let response = crate::proxy::types::QueryResponse::from(&persisted);
    assert_eq!(response.results.len(), 1);
    assert_eq!(response.results[0].id, "doc1");
    assert_eq!(response.results[0].content, "Content here");
    assert_eq!(
        response.cache_status,
        crate::proxy::types::CacheStatus::Stale
    );
    assert_eq!(response.took_ms, 100);
    assert!(response.generated_at.is_none());
}

#[test]
fn test_persisted_response_cache_status_mapping() {
    let statuses = vec![
        ("hit", crate::proxy::types::CacheStatus::Hit),
        ("miss", crate::proxy::types::CacheStatus::Miss),
        ("stale", crate::proxy::types::CacheStatus::Stale),
        ("frozen", crate::proxy::types::CacheStatus::Frozen),
        ("unknown", crate::proxy::types::CacheStatus::Miss), // default
    ];

    for (status_str, expected) in statuses {
        let persisted = PersistedResponse {
            results: vec![],
            cache_status: status_str.to_string(),
            took_ms: 0,
            generated_at: None,
        };
        let response = crate::proxy::types::QueryResponse::from(&persisted);
        assert_eq!(
            response.cache_status, expected,
            "Failed for status: {}",
            status_str
        );
    }
}

// === PersistenceError Display ===

#[test]
fn test_persistence_error_display_all_variants() {
    let errors = vec![
        (
            PersistenceError::DatabaseOpen("err".to_string()),
            "Failed to open database: err",
        ),
        (
            PersistenceError::Serialize("err".to_string()),
            "Serialization error: err",
        ),
        (
            PersistenceError::Deserialize("err".to_string()),
            "Deserialization error: err",
        ),
        (
            PersistenceError::Write("err".to_string()),
            "Database write error: err",
        ),
        (
            PersistenceError::Read("err".to_string()),
            "Database read error: err",
        ),
        (
            PersistenceError::Flush("err".to_string()),
            "Database flush error: err",
        ),
    ];
    for (err, expected) in errors {
        assert_eq!(err.to_string(), expected);
    }
}

#[test]
fn test_persistence_error_is_std_error() {
    let err: Box<dyn std::error::Error> = Box::new(PersistenceError::Write("test".to_string()));
    assert!(err.to_string().contains("write error"));
}

// === current_time_ms ===

#[test]
fn test_current_time_ms_is_reasonable() {
    let now = current_time_ms();
    // Should be after 2020-01-01 and before 2040-01-01
    assert!(now > 1577836800000); // 2020-01-01
    assert!(now < 2208988800000); // 2040-01-01
}
