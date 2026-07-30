#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;
use std::sync::Arc;

#[test]
fn test_context_manager_creation() {
    let manager = ContextManager::with_defaults();
    assert_eq!(manager.current(), "default");
    assert_eq!(manager.active_count(), 1);
}

#[test]
fn test_create_context() {
    let manager = ContextManager::with_defaults();

    manager
        .create("project-a", "http://localhost:6333", "docs")
        .unwrap();

    assert_eq!(manager.active_count(), 2);
    assert!(manager.get("project-a").is_some());
}

#[test]
fn test_switch_context() {
    let manager = ContextManager::with_defaults();
    manager.create("other", "", "").unwrap();

    manager.switch("other").unwrap();
    assert_eq!(manager.current(), "other");

    manager.switch("default").unwrap();
    assert_eq!(manager.current(), "default");
}

#[test]
fn test_auto_create_context() {
    let manager = ContextManager::with_defaults();

    // Auto-create on switch
    manager.switch("new-context").unwrap();
    assert_eq!(manager.current(), "new-context");
    assert!(manager.get("new-context").is_some());
}

#[test]
fn test_delete_context() {
    let manager = ContextManager::with_defaults();
    manager.create("temp", "", "").unwrap();

    assert_eq!(manager.active_count(), 2);
    manager.delete("temp").unwrap();
    assert_eq!(manager.active_count(), 1);
}

#[test]
fn test_cannot_delete_default() {
    let manager = ContextManager::with_defaults();
    let result = manager.delete("default");
    assert!(matches!(result, Err(ContextError::CannotDeleteDefault)));
}

#[test]
fn test_context_stats() {
    let manager = ContextManager::with_defaults();

    manager.record_hit();
    manager.record_hit();
    manager.record_miss();

    let stats = manager.stats("default").unwrap();
    assert_eq!(stats.hits, 2);
    assert_eq!(stats.misses, 1);
    assert_eq!(stats.queries, 3);
    assert!((stats.hit_rate() - 0.666).abs() < 0.01);
}

#[test]
fn test_cache_key_prefixing() {
    let manager = ContextManager::with_defaults();

    let key = manager.cache_key(0x1234567890abcdef);
    assert!(key.starts_with("ctx:default:resp:"));
    assert!(key.contains("1234567890abcdef"));

    manager.switch("project-x").unwrap();
    let key2 = manager.cache_key(0x1234567890abcdef);
    assert!(key2.starts_with("ctx:project-x:resp:"));
}

#[test]
fn test_context_metadata() {
    let manager = ContextManager::with_defaults();
    manager
        .create("test", "http://example.com", "my-collection")
        .unwrap();

    let meta = manager.get("test").unwrap();
    assert_eq!(meta.upstream_url, "http://example.com");
    assert_eq!(meta.collection, "my-collection");
}

#[test]
fn test_set_query_mode() {
    let manager = ContextManager::with_defaults();

    manager
        .set_query_mode("default", QueryMode::TextNative)
        .unwrap();
    assert_eq!(manager.query_mode("default"), Some(QueryMode::TextNative));
}

#[test]
fn test_list_contexts() {
    let manager = ContextManager::with_defaults();
    manager.create("a", "", "").unwrap();
    manager.create("b", "", "").unwrap();

    let list = manager.list();
    assert_eq!(list.len(), 3);
    assert!(list.contains(&"default".to_string()));
    assert!(list.contains(&"a".to_string()));
    assert!(list.contains(&"b".to_string()));
}

#[test]
fn test_context_eviction() {
    let config = ContextConfig {
        max_active_contexts: 3,
        ..Default::default()
    };
    let manager = ContextManager::new(config);

    // Create contexts up to limit
    manager.create("a", "", "").unwrap();
    manager.create("b", "", "").unwrap();
    assert_eq!(manager.active_count(), 3);

    // Creating more should trigger eviction
    manager.create("c", "", "").unwrap();
    manager.switch("c").unwrap(); // This triggers eviction

    // Should evict oldest (not current, not default)
    assert!(manager.active_count() <= 3);
}

#[test]
fn test_context_metadata_new() {
    let meta = ContextMetadata::new("test", "http://localhost", "collection");
    assert_eq!(meta.id, "test");
    assert_eq!(meta.upstream_url, "http://localhost");
    assert_eq!(meta.collection, "collection");
    assert!(meta.created_at > 0);
}

#[test]
fn test_context_error_display() {
    let err = ContextError::NotFound("test".to_string());
    assert_eq!(err.to_string(), "Context not found: test");

    let err = ContextError::AlreadyExists("test".to_string());
    assert_eq!(err.to_string(), "Context already exists: test");

    let err = ContextError::CannotDeleteDefault;
    assert_eq!(err.to_string(), "Cannot delete the default context");
}

// === UpstreamType Tests ===

#[test]
fn test_upstream_type_default() {
    let t = UpstreamType::default();
    assert_eq!(t, UpstreamType::Unknown);
    assert!(!t.is_known());
}

#[test]
fn test_upstream_type_is_fts() {
    assert!(UpstreamType::FullTextSearch.is_fts());
    assert!(UpstreamType::Hybrid.is_fts());
    assert!(!UpstreamType::VectorDatabase.is_fts());
    assert!(!UpstreamType::Unknown.is_fts());
}

#[test]
fn test_upstream_type_is_vector_db() {
    assert!(UpstreamType::VectorDatabase.is_vector_db());
    assert!(UpstreamType::Hybrid.is_vector_db());
    assert!(!UpstreamType::FullTextSearch.is_vector_db());
    assert!(!UpstreamType::Unknown.is_vector_db());
}

#[test]
fn test_upstream_type_score_range() {
    // FTS has higher max (BM25 can be > 1.0)
    let (min, max) = UpstreamType::FullTextSearch.score_range();
    assert_eq!(min, 0.0);
    assert!(max > 1.0);

    // VectorDB has 0-1 (cosine similarity)
    let (min, max) = UpstreamType::VectorDatabase.score_range();
    assert_eq!(min, 0.0);
    assert_eq!(max, 1.0);

    // Hybrid normalized
    let (min, max) = UpstreamType::Hybrid.score_range();
    assert_eq!(min, 0.0);
    assert_eq!(max, 1.0);
}

#[test]
fn test_upstream_type_as_str() {
    assert_eq!(UpstreamType::FullTextSearch.as_str(), "fts");
    assert_eq!(UpstreamType::VectorDatabase.as_str(), "vector_db");
    assert_eq!(UpstreamType::Hybrid.as_str(), "hybrid");
    assert_eq!(UpstreamType::Unknown.as_str(), "unknown");
}

#[test]
fn test_upstream_type_display() {
    assert_eq!(format!("{}", UpstreamType::FullTextSearch), "fts");
    assert_eq!(format!("{}", UpstreamType::VectorDatabase), "vector_db");
}

#[test]
fn test_context_metadata_with_upstream_type() {
    let meta = ContextMetadata::new("test", "http://localhost", "collection")
        .with_upstream_type(UpstreamType::FullTextSearch)
        .with_query_mode(QueryMode::TextNative);

    assert_eq!(meta.upstream_type, UpstreamType::FullTextSearch);
    assert_eq!(meta.query_mode, QueryMode::TextNative);
}

#[test]
fn test_context_stats_reset() {
    let stats = ContextStats::new();
    stats.record_hit();
    stats.record_hit();
    stats.record_miss();

    assert_eq!(stats.hits.load(Ordering::Relaxed), 2);
    assert_eq!(stats.misses.load(Ordering::Relaxed), 1);
    assert_eq!(stats.queries.load(Ordering::Relaxed), 3);

    stats.reset();

    assert_eq!(stats.hits.load(Ordering::Relaxed), 0);
    assert_eq!(stats.misses.load(Ordering::Relaxed), 0);
    assert_eq!(stats.queries.load(Ordering::Relaxed), 0);
    assert_eq!(stats.hit_rate(), 0.0);
}

#[test]
fn test_get_context_stats() {
    let config = ContextConfig::default();
    let manager = ContextManager::new(config);

    manager.record_hit();
    manager.record_miss();

    let result = manager.get_context_stats("default");
    assert!(result.is_some());
    let (snap, hit_rate) = result.unwrap();
    assert_eq!(snap.hits, 1);
    assert_eq!(snap.misses, 1);
    assert_eq!(snap.queries, 2);
    assert!((hit_rate - 0.5).abs() < f64::EPSILON);

    // Non-existent context returns None
    assert!(manager.get_context_stats("nonexistent").is_none());
}

#[test]
fn test_all_context_stats() {
    let config = ContextConfig::default();
    let manager = ContextManager::new(config);

    manager.record_hit();
    manager.create("ctx2", "http://localhost", "coll2").unwrap();
    manager.switch("ctx2").unwrap();
    manager.record_miss();

    let all = manager.all_context_stats();
    assert_eq!(all.len(), 2);

    // Find default and ctx2
    let default_stats = all.iter().find(|(id, _, _)| id == "default");
    let ctx2_stats = all.iter().find(|(id, _, _)| id == "ctx2");
    assert!(default_stats.is_some());
    assert!(ctx2_stats.is_some());

    let (_, default_snap, _) = default_stats.unwrap();
    assert_eq!(default_snap.hits, 1);

    let (_, ctx2_snap, _) = ctx2_stats.unwrap();
    assert_eq!(ctx2_snap.misses, 1);
}

#[test]
fn test_context_stats_hit_rate_zero_queries() {
    let stats = ContextStats::new();
    assert_eq!(stats.hit_rate(), 0.0);
}

#[test]
fn test_context_manager_reset_all_stats() {
    let config = ContextConfig::default();
    let manager = ContextManager::new(config);

    // Record some stats on the default context
    manager.record_hit();
    manager.record_hit();
    manager.record_miss();

    // Create and switch to a second context
    manager
        .create("ctx2", "http://localhost", "collection2")
        .unwrap();
    manager.switch("ctx2").unwrap();
    manager.record_hit();
    manager.record_miss();

    // Reset all stats
    manager.reset_all_stats();

    // Verify default context stats are zeroed
    let default_ctx = manager.active.get("default").unwrap();
    assert_eq!(default_ctx.stats.hits.load(Ordering::Relaxed), 0);
    assert_eq!(default_ctx.stats.misses.load(Ordering::Relaxed), 0);

    // Verify ctx2 stats are zeroed
    let ctx2 = manager.active.get("ctx2").unwrap();
    assert_eq!(ctx2.stats.hits.load(Ordering::Relaxed), 0);
    assert_eq!(ctx2.stats.misses.load(Ordering::Relaxed), 0);
}

#[test]
fn test_record_hit_for_specific_context() {
    let manager = ContextManager::with_defaults();
    manager.create("project-a", "", "").unwrap();

    // Record hits for a specific context (not the current one)
    manager.record_hit_for("project-a");
    manager.record_hit_for("project-a");
    manager.record_miss_for("project-a");

    // project-a should have the stats
    let stats = manager.stats("project-a").unwrap();
    assert_eq!(stats.hits, 2);
    assert_eq!(stats.misses, 1);
    assert_eq!(stats.queries, 3);

    // default (current) should have no stats
    let default_stats = manager.stats("default").unwrap();
    assert_eq!(default_stats.hits, 0);
    assert_eq!(default_stats.misses, 0);
}

#[test]
fn test_record_hit_for_nonexistent_context() {
    let manager = ContextManager::with_defaults();

    // Recording for a nonexistent context should not panic
    manager.record_hit_for("nonexistent");
    manager.record_miss_for("nonexistent");

    // default should be unaffected
    let stats = manager.stats("default").unwrap();
    assert_eq!(stats.hits, 0);
    assert_eq!(stats.misses, 0);
}

#[test]
fn test_concurrent_switch_auto_create() {
    let manager = Arc::new(ContextManager::with_defaults());
    let barrier = Arc::new(std::sync::Barrier::new(20));
    let mut handles = vec![];

    for _ in 0..20 {
        let m = manager.clone();
        let b = barrier.clone();
        handles.push(std::thread::spawn(move || {
            b.wait();
            m.switch("concurrent-ctx").unwrap();
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    assert!(manager.get("concurrent-ctx").is_some());
}

#[test]
fn test_install_resolved_context_policies() {
    use crate::config::{
        resolve_all_contexts, ContextCacheConfig, ContextLegConfig, NamedContextConfig,
        ProxyScopeConfig, UpstreamResourceConfig,
    };
    use std::collections::HashMap;

    let mut upstreams = HashMap::new();
    upstreams.insert(
        "u1".into(),
        UpstreamResourceConfig {
            url: Some("http://127.0.0.1:7700".into()),
            upstream_type: Some("meilisearch".into()),
            ..Default::default()
        },
    );
    let mut contexts = HashMap::new();
    contexts.insert(
        "docs".into(),
        NamedContextConfig {
            default: Some(true),
            description: Some("docs".into()),
            upstreams: vec![ContextLegConfig {
                resource_ref: "u1".into(),
                ..Default::default()
            }],
            cache: ContextCacheConfig {
                fresh_secs: Some(10),
                ..Default::default()
            },
            scope: ProxyScopeConfig {
                mode: Some("filter".into()),
                seeds: vec!["alpha".into()],
                min_seed_similarity: Some(0.5),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    contexts.insert(
        "support".into(),
        NamedContextConfig {
            default: Some(false),
            upstreams: vec![ContextLegConfig {
                resource_ref: "u1".into(),
                ..Default::default()
            }],
            scope: ProxyScopeConfig {
                mode: Some("filter".into()),
                seeds: vec!["beta".into()],
                min_seed_similarity: Some(0.9),
                ..Default::default()
            },
            ..Default::default()
        },
    );

    let resolved = resolve_all_contexts(&upstreams, &HashMap::new(), &contexts).unwrap();
    let mgr = ContextManager::from_resolved(&resolved);

    assert_eq!(mgr.current(), "docs");
    assert!(mgr.scope_filter_for("docs").unwrap().is_enabled());
    assert!(mgr.scope_filter_for("support").unwrap().is_enabled());
    // Distinct policies per context
    let p_docs = mgr.policy_for("docs").unwrap();
    let p_sup = mgr.policy_for("support").unwrap();
    assert_eq!(p_docs.cache.fresh_secs, Some(10));
    assert_ne!(
        p_docs.scope.min_seed_similarity,
        p_sup.scope.min_seed_similarity
    );
    assert_eq!(p_sup.scope.min_seed_similarity, Some(0.9));
}
