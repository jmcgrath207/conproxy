#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

fn make_config(id: &str, url: &str, weight: u32, priority: u32) -> UpstreamEndpointConfig {
    UpstreamEndpointConfig {
        id: id.to_string(),
        url: url.to_string(),
        timeout_secs: Some(30),
        weight: Some(weight),
        priority: Some(priority),
        max_concurrent: None,
        enabled: Some(true),
        version_endpoint: None,
        version_poll_interval_secs: None,
        upstream_type: None,
        query_mode: None,
        table: None,
        embedding_column: None,
        content_column: None,
        metadata_columns: vec![],
        distance_metric: None,
        dimensions: None,
        index: None,
        search_fields: vec![],
        return_fields: vec![],
        api_key: None,
    }
}

#[test]
fn test_pool_creation() {
    let configs = vec![
        make_config("us-east", "http://us-east.example.com", 1, 0),
        make_config("us-west", "http://us-west.example.com", 1, 1),
    ];

    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();

    assert_eq!(pool.len(), 2);
    assert!(!pool.is_empty());
    assert!(pool.get("us-east").is_some());
    assert!(pool.get("us-west").is_some());
    assert!(pool.get("unknown").is_none());
}

#[test]
fn test_vector_db_upstream_types_build() {
    for ut in ["pinecone", "milvus"] {
        let mut cfg = make_config(ut, "http://localhost:9999", 1, 0);
        cfg.upstream_type = Some(ut.to_string());
        cfg.index = Some("demo".to_string());
        let pooled = PooledUpstream::new(&cfg).unwrap_or_else(|e| panic!("type={ut}: {e}"));
        assert_eq!(pooled.adapter.metadata().adapter_type, ut);
        assert_eq!(pooled.adapter.query_mode(), QueryMode::VectorOnly);
    }
}

#[test]
fn test_pool_empty() {
    let pool = UpstreamPool::empty();
    assert!(pool.is_empty());
    assert_eq!(pool.len(), 0);
    assert!(pool.select().is_none());
}

#[test]
fn test_round_robin_selection() {
    let configs = vec![
        make_config("a", "http://a.example.com", 1, 0),
        make_config("b", "http://b.example.com", 1, 0),
        make_config("c", "http://c.example.com", 1, 0),
    ];

    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();

    // First round
    let first = pool.select().unwrap();
    let second = pool.select().unwrap();
    let third = pool.select().unwrap();

    // Should cycle through
    assert_ne!(first.id, second.id);
    assert_ne!(second.id, third.id);
}

#[test]
fn test_failover_selection() {
    let configs = vec![
        make_config("primary", "http://primary.example.com", 1, 0),
        make_config("secondary", "http://secondary.example.com", 1, 1),
        make_config("tertiary", "http://tertiary.example.com", 1, 2),
    ];

    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::Failover).unwrap();

    // Should select primary (lowest priority)
    let selected = pool.select().unwrap();
    assert_eq!(selected.id, "primary");
}

#[test]
fn test_weighted_selection() {
    let configs = vec![
        make_config("heavy", "http://heavy.example.com", 10, 0),
        make_config("light", "http://light.example.com", 1, 0),
    ];

    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::Weighted).unwrap();

    // Heavy should be selected more often
    let mut heavy_count = 0;
    let mut light_count = 0;

    for _ in 0..110 {
        let selected = pool.select().unwrap();
        if selected.id == "heavy" {
            heavy_count += 1;
        } else {
            light_count += 1;
        }
    }

    // Heavy should have roughly 10x more selections
    assert!(heavy_count > light_count * 5);
}

#[test]
fn test_pooled_upstream_metrics() {
    let config = make_config("test", "http://test.example.com", 1, 0);
    let upstream = PooledUpstream::new(&config).unwrap();

    assert_eq!(upstream.request_count(), 0);
    assert_eq!(upstream.failure_count(), 0);
    assert_eq!(upstream.failure_rate(), 0.0);

    upstream.record_success();
    upstream.record_success();
    upstream.record_failure();

    assert_eq!(upstream.request_count(), 3);
    assert_eq!(upstream.failure_count(), 1);
    assert!((upstream.failure_rate() - 0.333).abs() < 0.01);
}

#[test]
fn test_pool_stats() {
    let configs = vec![
        make_config("a", "http://a.example.com", 1, 0),
        make_config("b", "http://b.example.com", 1, 0),
    ];

    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();

    let stats = pool.stats();
    assert_eq!(stats.total_upstreams, 2);
    assert_eq!(stats.healthy_upstreams, 2); // Default to Online
    assert_eq!(stats.total_requests, 0);
    // All upstreams are Unknown by default
    assert_eq!(stats.type_counts.unknown, 2);
    assert_eq!(stats.type_counts.fts, 0);
    assert_eq!(stats.type_counts.vector_db, 0);
    assert_eq!(stats.type_counts.hybrid, 0);
}

#[test]
fn test_pool_stats_with_types() {
    let configs = vec![
        make_config("es", "http://es.example.com", 1, 0),
        make_config("qdrant", "http://qdrant.example.com", 1, 0),
        make_config("hybrid", "http://hybrid.example.com", 1, 0),
    ];

    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();

    // Set types
    pool.get("es")
        .unwrap()
        .set_upstream_type(UpstreamType::FullTextSearch);
    pool.get("qdrant")
        .unwrap()
        .set_upstream_type(UpstreamType::VectorDatabase);
    pool.get("hybrid")
        .unwrap()
        .set_upstream_type(UpstreamType::Hybrid);

    let stats = pool.stats();
    assert_eq!(stats.total_upstreams, 3);
    assert_eq!(stats.type_counts.fts, 1);
    assert_eq!(stats.type_counts.vector_db, 1);
    assert_eq!(stats.type_counts.hybrid, 1);
    assert_eq!(stats.type_counts.unknown, 0);
}

#[test]
fn test_upstream_enable_disable() {
    let config = make_config("test", "http://test.example.com", 1, 0);
    let upstream = PooledUpstream::new(&config).unwrap();

    assert!(upstream.is_available());

    upstream.disable();
    assert!(!upstream.is_available());

    upstream.enable();
    assert!(upstream.is_available());
}

#[test]
fn test_strategy_as_str() {
    assert_eq!(LoadBalanceStrategy::RoundRobin.as_str(), "round_robin");
    assert_eq!(LoadBalanceStrategy::Weighted.as_str(), "weighted");
    assert_eq!(LoadBalanceStrategy::Random.as_str(), "random");
    assert_eq!(LoadBalanceStrategy::Failover.as_str(), "failover");
}

#[test]
fn test_pool_strategy_getter() {
    let configs = vec![make_config("a", "http://a.example.com", 1, 0)];

    let pool_rr = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();
    assert_eq!(pool_rr.strategy(), LoadBalanceStrategy::RoundRobin);

    let pool_failover = UpstreamPool::new(&configs, LoadBalanceStrategy::Failover).unwrap();
    assert_eq!(pool_failover.strategy(), LoadBalanceStrategy::Failover);

    let empty = UpstreamPool::empty();
    assert_eq!(empty.strategy(), LoadBalanceStrategy::RoundRobin);
}

// === QueryMode Tests ===

#[test]
fn test_pooled_upstream_query_mode_default() {
    let config = make_config("test", "http://test.example.com", 1, 0);
    let upstream = PooledUpstream::new(&config).unwrap();

    // Default mode is Unknown
    assert_eq!(upstream.query_mode(), QueryMode::Unknown);
    assert!(!upstream.is_mode_known());
}

#[test]
fn test_pooled_upstream_set_query_mode() {
    let config = make_config("test", "http://test.example.com", 1, 0);
    let upstream = PooledUpstream::new(&config).unwrap();

    // Set to TextNative
    upstream.set_query_mode(QueryMode::TextNative);
    assert_eq!(upstream.query_mode(), QueryMode::TextNative);
    assert!(upstream.supports_text());
    assert!(!upstream.requires_embedding());
    assert!(upstream.is_mode_known());

    // Set to VectorOnly
    upstream.set_query_mode(QueryMode::VectorOnly);
    assert_eq!(upstream.query_mode(), QueryMode::VectorOnly);
    assert!(!upstream.supports_text());
    assert!(upstream.requires_embedding());
    assert!(upstream.is_mode_known());
}

#[test]
fn test_pool_mode_counts_all_unknown() {
    let configs = vec![
        make_config("a", "http://a.example.com", 1, 0),
        make_config("b", "http://b.example.com", 1, 0),
        make_config("c", "http://c.example.com", 1, 0),
    ];

    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();
    let counts = pool.mode_counts();

    // All should be Unknown by default
    assert_eq!(counts.unknown, 3);
    assert_eq!(counts.text_native, 0);
    assert_eq!(counts.vector_only, 0);
}

#[test]
fn test_pool_mode_counts_mixed() {
    let configs = vec![
        make_config("text1", "http://text1.example.com", 1, 0),
        make_config("text2", "http://text2.example.com", 1, 0),
        make_config("vector", "http://vector.example.com", 1, 0),
        make_config("unknown", "http://unknown.example.com", 1, 0),
    ];

    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();

    // Set modes
    pool.get("text1")
        .unwrap()
        .set_query_mode(QueryMode::TextNative);
    pool.get("text2")
        .unwrap()
        .set_query_mode(QueryMode::TextNative);
    pool.get("vector")
        .unwrap()
        .set_query_mode(QueryMode::VectorOnly);
    // "unknown" remains Unknown

    let counts = pool.mode_counts();
    assert_eq!(counts.text_native, 2);
    assert_eq!(counts.vector_only, 1);
    assert_eq!(counts.unknown, 1);
}

#[test]
fn test_pool_with_mode_filter() {
    let configs = vec![
        make_config("text1", "http://text1.example.com", 1, 0),
        make_config("vector1", "http://vector1.example.com", 1, 0),
        make_config("text2", "http://text2.example.com", 1, 0),
    ];

    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();

    // Set modes
    pool.get("text1")
        .unwrap()
        .set_query_mode(QueryMode::TextNative);
    pool.get("text2")
        .unwrap()
        .set_query_mode(QueryMode::TextNative);
    pool.get("vector1")
        .unwrap()
        .set_query_mode(QueryMode::VectorOnly);

    // Filter by mode
    let text = pool.with_mode(QueryMode::TextNative);
    assert_eq!(text.len(), 2);

    let vector = pool.with_mode(QueryMode::VectorOnly);
    assert_eq!(vector.len(), 1);

    let unknown = pool.with_mode(QueryMode::Unknown);
    assert_eq!(unknown.len(), 0);
}

#[test]
fn test_pool_text_native_filter() {
    let configs = vec![
        make_config("text1", "http://text1.example.com", 1, 0),
        make_config("vector1", "http://vector1.example.com", 1, 0),
    ];

    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();

    pool.get("text1")
        .unwrap()
        .set_query_mode(QueryMode::TextNative);
    pool.get("vector1")
        .unwrap()
        .set_query_mode(QueryMode::VectorOnly);

    let text_native = pool.text_native();
    assert_eq!(text_native.len(), 1);
    assert_eq!(text_native[0].id, "text1");
}

#[test]
fn test_pool_vector_only_filter() {
    let configs = vec![
        make_config("text1", "http://text1.example.com", 1, 0),
        make_config("vector1", "http://vector1.example.com", 1, 0),
    ];

    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();

    pool.get("text1")
        .unwrap()
        .set_query_mode(QueryMode::TextNative);
    pool.get("vector1")
        .unwrap()
        .set_query_mode(QueryMode::VectorOnly);

    let vector_only = pool.vector_only();
    assert_eq!(vector_only.len(), 1);
    assert_eq!(vector_only[0].id, "vector1");
}

#[test]
fn test_pool_unknown_mode_filter() {
    let configs = vec![
        make_config("known", "http://known.example.com", 1, 0),
        make_config("unknown1", "http://unknown1.example.com", 1, 0),
        make_config("unknown2", "http://unknown2.example.com", 1, 0),
    ];

    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();

    pool.get("known")
        .unwrap()
        .set_query_mode(QueryMode::TextNative);
    // unknown1 and unknown2 remain Unknown

    let unknown = pool.unknown_mode();
    assert_eq!(unknown.len(), 2);
}

#[test]
fn test_select_text_native_empty_pool() {
    let pool = UpstreamPool::empty();
    assert!(pool.select_text_native().is_none());
}

#[test]
fn test_select_text_native_no_text_upstreams() {
    let configs = vec![make_config("vector", "http://vector.example.com", 1, 0)];

    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();
    pool.get("vector")
        .unwrap()
        .set_query_mode(QueryMode::VectorOnly);

    assert!(pool.select_text_native().is_none());
}

#[test]
fn test_select_text_native_round_robin() {
    let configs = vec![
        make_config("text1", "http://text1.example.com", 1, 0),
        make_config("text2", "http://text2.example.com", 1, 0),
    ];

    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();
    pool.get("text1")
        .unwrap()
        .set_query_mode(QueryMode::TextNative);
    pool.get("text2")
        .unwrap()
        .set_query_mode(QueryMode::TextNative);

    // Should select one
    let selected = pool.select_text_native();
    assert!(selected.is_some());
}

#[test]
fn test_select_vector_only_empty_pool() {
    let pool = UpstreamPool::empty();
    assert!(pool.select_vector_only().is_none());
}

#[test]
fn test_select_vector_only_round_robin() {
    let configs = vec![
        make_config("vector1", "http://vector1.example.com", 1, 0),
        make_config("vector2", "http://vector2.example.com", 1, 0),
    ];

    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();
    pool.get("vector1")
        .unwrap()
        .set_query_mode(QueryMode::VectorOnly);
    pool.get("vector2")
        .unwrap()
        .set_query_mode(QueryMode::VectorOnly);

    let selected = pool.select_vector_only();
    assert!(selected.is_some());
}

#[test]
fn test_query_mode_counts_default() {
    let counts = QueryModeCounts::default();
    assert_eq!(counts.text_native, 0);
    assert_eq!(counts.vector_only, 0);
    assert_eq!(counts.unknown, 0);
}

// === UpstreamType Tests ===

#[test]
fn test_upstream_type_default() {
    let config = make_config("test", "http://test.example.com", 1, 0);
    let upstream = PooledUpstream::new(&config).unwrap();

    // Default type is Unknown
    assert_eq!(upstream.upstream_type(), UpstreamType::Unknown);
    assert!(!upstream.is_type_known());
    assert!(!upstream.is_fts());
    assert!(!upstream.is_vector_db());
}

#[test]
fn test_upstream_type_set_and_get() {
    let config = make_config("test", "http://test.example.com", 1, 0);
    let upstream = PooledUpstream::new(&config).unwrap();

    // Set to FTS
    upstream.set_upstream_type(UpstreamType::FullTextSearch);
    assert_eq!(upstream.upstream_type(), UpstreamType::FullTextSearch);
    assert!(upstream.is_fts());
    assert!(!upstream.is_vector_db());
    assert!(upstream.is_type_known());

    // Set to VectorDB
    upstream.set_upstream_type(UpstreamType::VectorDatabase);
    assert_eq!(upstream.upstream_type(), UpstreamType::VectorDatabase);
    assert!(!upstream.is_fts());
    assert!(upstream.is_vector_db());

    // Set to Hybrid
    upstream.set_upstream_type(UpstreamType::Hybrid);
    assert_eq!(upstream.upstream_type(), UpstreamType::Hybrid);
    assert!(upstream.is_fts());
    assert!(upstream.is_vector_db());
}

#[test]
fn test_upstream_type_score_range() {
    let config = make_config("test", "http://test.example.com", 1, 0);
    let upstream = PooledUpstream::new(&config).unwrap();

    // FTS has higher max score range
    upstream.set_upstream_type(UpstreamType::FullTextSearch);
    let (min, max) = upstream.score_range();
    assert_eq!(min, 0.0);
    assert!(max > 1.0); // BM25 can be > 1.0

    // VectorDB has 0-1 range (cosine similarity)
    upstream.set_upstream_type(UpstreamType::VectorDatabase);
    let (min, max) = upstream.score_range();
    assert_eq!(min, 0.0);
    assert_eq!(max, 1.0);
}

#[test]
fn test_pool_type_counts_all_unknown() {
    let configs = vec![
        make_config("a", "http://a.example.com", 1, 0),
        make_config("b", "http://b.example.com", 1, 0),
    ];

    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();
    let counts = pool.type_counts();

    assert_eq!(counts.unknown, 2);
    assert_eq!(counts.fts, 0);
    assert_eq!(counts.vector_db, 0);
    assert_eq!(counts.hybrid, 0);
}

#[test]
fn test_pool_type_counts_mixed() {
    let configs = vec![
        make_config("fts1", "http://es1.example.com", 1, 0),
        make_config("fts2", "http://es2.example.com", 1, 0),
        make_config("vector", "http://qdrant.example.com", 1, 0),
        make_config("hybrid", "http://hybrid.example.com", 1, 0),
    ];

    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();

    pool.get("fts1")
        .unwrap()
        .set_upstream_type(UpstreamType::FullTextSearch);
    pool.get("fts2")
        .unwrap()
        .set_upstream_type(UpstreamType::FullTextSearch);
    pool.get("vector")
        .unwrap()
        .set_upstream_type(UpstreamType::VectorDatabase);
    pool.get("hybrid")
        .unwrap()
        .set_upstream_type(UpstreamType::Hybrid);

    let counts = pool.type_counts();
    assert_eq!(counts.fts, 2);
    assert_eq!(counts.vector_db, 1);
    assert_eq!(counts.hybrid, 1);
    assert_eq!(counts.unknown, 0);
}

#[test]
fn test_pool_fts_upstreams_filter() {
    let configs = vec![
        make_config("es", "http://es.example.com", 1, 0),
        make_config("qdrant", "http://qdrant.example.com", 1, 0),
    ];

    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();
    pool.get("es")
        .unwrap()
        .set_upstream_type(UpstreamType::FullTextSearch);
    pool.get("qdrant")
        .unwrap()
        .set_upstream_type(UpstreamType::VectorDatabase);

    let fts = pool.fts_upstreams();
    assert_eq!(fts.len(), 1);
    assert_eq!(fts[0].id, "es");
}

#[test]
fn test_pool_vector_db_upstreams_filter() {
    let configs = vec![
        make_config("es", "http://es.example.com", 1, 0),
        make_config("qdrant", "http://qdrant.example.com", 1, 0),
    ];

    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();
    pool.get("es")
        .unwrap()
        .set_upstream_type(UpstreamType::FullTextSearch);
    pool.get("qdrant")
        .unwrap()
        .set_upstream_type(UpstreamType::VectorDatabase);

    let vector_dbs = pool.vector_db_upstreams();
    assert_eq!(vector_dbs.len(), 1);
    assert_eq!(vector_dbs[0].id, "qdrant");
}

#[test]
fn test_pool_select_fts() {
    let configs = vec![
        make_config("es1", "http://es1.example.com", 1, 0),
        make_config("es2", "http://es2.example.com", 1, 0),
        make_config("qdrant", "http://qdrant.example.com", 1, 0),
    ];

    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();
    pool.get("es1")
        .unwrap()
        .set_upstream_type(UpstreamType::FullTextSearch);
    pool.get("es2")
        .unwrap()
        .set_upstream_type(UpstreamType::FullTextSearch);
    pool.get("qdrant")
        .unwrap()
        .set_upstream_type(UpstreamType::VectorDatabase);

    let selected = pool.select_fts();
    assert!(selected.is_some());
    assert!(selected.unwrap().is_fts());
}

#[test]
fn test_pool_select_vector_db() {
    let configs = vec![
        make_config("es", "http://es.example.com", 1, 0),
        make_config("qdrant", "http://qdrant.example.com", 1, 0),
    ];

    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();
    pool.get("es")
        .unwrap()
        .set_upstream_type(UpstreamType::FullTextSearch);
    pool.get("qdrant")
        .unwrap()
        .set_upstream_type(UpstreamType::VectorDatabase);

    let selected = pool.select_vector_db();
    assert!(selected.is_some());
    assert!(selected.unwrap().is_vector_db());
}

#[test]
fn test_pool_select_fts_none_available() {
    let configs = vec![make_config("qdrant", "http://qdrant.example.com", 1, 0)];

    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();
    pool.get("qdrant")
        .unwrap()
        .set_upstream_type(UpstreamType::VectorDatabase);

    // No FTS upstreams
    assert!(pool.select_fts().is_none());
}

#[test]
fn test_upstream_type_counts_default() {
    let counts = UpstreamTypeCounts::default();
    assert_eq!(counts.fts, 0);
    assert_eq!(counts.vector_db, 0);
    assert_eq!(counts.hybrid, 0);
    assert_eq!(counts.unknown, 0);
}

#[test]
fn test_upstream_type_as_str() {
    assert_eq!(UpstreamType::FullTextSearch.as_str(), "fts");
    assert_eq!(UpstreamType::VectorDatabase.as_str(), "vector_db");
    assert_eq!(UpstreamType::Hybrid.as_str(), "hybrid");
    assert_eq!(UpstreamType::Unknown.as_str(), "unknown");
}

// === Adapter Selection Tests ===

#[test]
fn test_create_adapter_elasticsearch() {
    let mut config = make_config("es", "http://localhost:9200", 1, 0);
    config.upstream_type = Some("elasticsearch".to_string());
    config.index = Some("my_index".to_string());

    let upstream = PooledUpstream::new(&config).unwrap();

    // Should create ElasticsearchAdapter, identified by base_url
    assert_eq!(upstream.adapter.identifier(), "http://localhost:9200");
    assert_eq!(upstream.adapter.metadata().adapter_type, "elasticsearch");
    // Should auto-set upstream type to FTS
    assert_eq!(upstream.upstream_type(), UpstreamType::FullTextSearch);
}

#[test]
fn test_create_adapter_qdrant() {
    let mut config = make_config("qd", "http://localhost:6333", 1, 0);
    config.upstream_type = Some("qdrant".to_string());
    config.index = Some("my_collection".to_string());

    let upstream = PooledUpstream::new(&config).unwrap();

    assert_eq!(upstream.adapter.identifier(), "http://localhost:6333");
    assert_eq!(upstream.adapter.metadata().adapter_type, "qdrant");
    assert_eq!(
        upstream.adapter.metadata().properties.get("collection"),
        Some(&"my_collection".to_string())
    );
    assert_eq!(upstream.upstream_type(), UpstreamType::VectorDatabase);
}

#[test]
fn test_create_adapter_generic_fallback() {
    let config = make_config("gen", "http://localhost:8000", 1, 0);

    let upstream = PooledUpstream::new(&config).unwrap();

    assert_eq!(upstream.adapter.identifier(), "http://localhost:8000");
    assert_eq!(upstream.adapter.metadata().adapter_type, "generic");
    assert_eq!(upstream.upstream_type(), UpstreamType::Unknown);
}

// === PooledUpstream unit tests ===

#[test]
fn test_pooled_upstream_is_healthy_when_enabled_and_online() {
    let config = make_config("u", "http://localhost:8080", 1, 0);
    let upstream = PooledUpstream::new(&config).unwrap();
    assert!(upstream.is_healthy());
    assert!(upstream.is_available());
}

#[test]
fn test_pooled_upstream_disable_enable() {
    let config = make_config("u", "http://localhost:8080", 1, 0);
    let upstream = PooledUpstream::new(&config).unwrap();

    upstream.disable();
    assert!(!upstream.is_healthy());
    assert!(!upstream.is_available());

    upstream.enable();
    assert!(upstream.is_healthy());
    assert!(upstream.is_available());
}

#[test]
fn test_pooled_upstream_failure_rate() {
    let config = make_config("u", "http://localhost:8080", 1, 0);
    let upstream = PooledUpstream::new(&config).unwrap();

    // Initially 0
    assert_eq!(upstream.failure_rate(), 0.0);

    // Record 3 successes and 1 failure
    upstream.record_success();
    upstream.record_success();
    upstream.record_success();
    upstream.record_failure();

    assert_eq!(upstream.request_count(), 4);
    assert_eq!(upstream.failure_count(), 1);
    assert!((upstream.failure_rate() - 0.25).abs() < 0.001);
}

#[test]
fn test_pooled_upstream_query_mode() {
    let config = make_config("u", "http://localhost:8080", 1, 0);
    let upstream = PooledUpstream::new(&config).unwrap();

    // Default is Unknown
    assert_eq!(upstream.query_mode(), QueryMode::Unknown);
    assert!(upstream.supports_text()); // Unknown supports text
    assert!(!upstream.requires_embedding());
    assert!(!upstream.is_mode_known());

    upstream.set_query_mode(QueryMode::TextNative);
    assert!(upstream.is_mode_known());
    assert!(upstream.supports_text());
    assert!(!upstream.requires_embedding());

    upstream.set_query_mode(QueryMode::VectorOnly);
    assert!(upstream.requires_embedding());
    assert!(!upstream.supports_text());
}

#[test]
fn test_pooled_upstream_type_methods() {
    let mut config = make_config("es", "http://localhost:9200", 1, 0);
    config.upstream_type = Some("elasticsearch".to_string());
    let upstream = PooledUpstream::new(&config).unwrap();

    assert!(upstream.is_fts());
    assert!(!upstream.is_vector_db());
    assert!(upstream.is_type_known());

    let (min, max) = upstream.score_range();
    assert_eq!(min, 0.0);
    assert!(max > 1.0); // FTS scores are unbounded
}

#[test]
fn test_pooled_upstream_set_upstream_type() {
    let config = make_config("u", "http://localhost:8080", 1, 0);
    let upstream = PooledUpstream::new(&config).unwrap();

    assert_eq!(upstream.upstream_type(), UpstreamType::Unknown);

    upstream.set_upstream_type(UpstreamType::VectorDatabase);
    assert_eq!(upstream.upstream_type(), UpstreamType::VectorDatabase);
    assert!(upstream.is_vector_db());
}

#[test]
fn test_pooled_upstream_pool_stats() {
    let config = make_config("u", "http://localhost:8080", 1, 0);
    let upstream = PooledUpstream::new(&config).unwrap();

    assert!(upstream.has_pool_capacity());
    assert_eq!(upstream.queue_depth(), 0);
    assert_eq!(upstream.active_connections(), 0);
    assert!(upstream.max_connections() > 0);
    assert!(upstream.available_connections() > 0);

    let stats = upstream.pool_stats();
    assert_eq!(stats.active_connections, 0);
}

// === UpstreamPool query mode/type filtering ===

#[test]
fn test_pool_text_native_and_vector_only() {
    let configs = vec![
        make_config("a", "http://a.example.com", 1, 0),
        make_config("b", "http://b.example.com", 1, 1),
    ];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();

    // Set modes
    pool.get("a").unwrap().set_query_mode(QueryMode::TextNative);
    pool.get("b").unwrap().set_query_mode(QueryMode::VectorOnly);

    assert_eq!(pool.text_native().len(), 1);
    assert_eq!(pool.text_native()[0].id, "a");
    assert_eq!(pool.vector_only().len(), 1);
    assert_eq!(pool.vector_only()[0].id, "b");
    assert_eq!(pool.unknown_mode().len(), 0);
}

#[test]
fn test_pool_mode_counts() {
    let configs = vec![
        make_config("a", "http://a.example.com", 1, 0),
        make_config("b", "http://b.example.com", 1, 1),
        make_config("c", "http://c.example.com", 1, 2),
    ];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();

    pool.get("a").unwrap().set_query_mode(QueryMode::TextNative);
    pool.get("b").unwrap().set_query_mode(QueryMode::VectorOnly);
    // c stays Unknown

    let counts = pool.mode_counts();
    assert_eq!(counts.text_native, 1);
    assert_eq!(counts.vector_only, 1);
    assert_eq!(counts.unknown, 1);
}

#[test]
fn test_pool_type_filtering() {
    let mut config_es = make_config("es", "http://localhost:9200", 1, 0);
    config_es.upstream_type = Some("elasticsearch".to_string());
    let mut config_qd = make_config("qd", "http://localhost:6333", 1, 1);
    config_qd.upstream_type = Some("qdrant".to_string());
    let config_gen = make_config("gen", "http://localhost:8000", 1, 2);

    let pool = UpstreamPool::new(
        &[config_es, config_qd, config_gen],
        LoadBalanceStrategy::RoundRobin,
    )
    .unwrap();

    assert_eq!(pool.fts_upstreams().len(), 1);
    assert_eq!(pool.vector_db_upstreams().len(), 1);
    assert_eq!(pool.unknown_type_upstreams().len(), 1);

    let counts = pool.type_counts();
    assert_eq!(counts.fts, 1);
    assert_eq!(counts.vector_db, 1);
    assert_eq!(counts.unknown, 1);
    assert_eq!(counts.hybrid, 0);
}

#[test]
fn test_pool_with_type() {
    let mut config_es = make_config("es", "http://localhost:9200", 1, 0);
    config_es.upstream_type = Some("elasticsearch".to_string());
    let config_gen = make_config("gen", "http://localhost:8000", 1, 1);

    let pool =
        UpstreamPool::new(&[config_es, config_gen], LoadBalanceStrategy::RoundRobin).unwrap();

    assert_eq!(pool.with_type(UpstreamType::FullTextSearch).len(), 1);
    assert_eq!(pool.with_type(UpstreamType::Unknown).len(), 1);
    assert_eq!(pool.with_type(UpstreamType::Hybrid).len(), 0);
}

#[test]
fn test_pool_with_mode() {
    let configs = vec![make_config("a", "http://a.example.com", 1, 0)];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();

    assert_eq!(pool.with_mode(QueryMode::Unknown).len(), 1);
    assert_eq!(pool.with_mode(QueryMode::TextNative).len(), 0);
}

#[test]
fn test_pool_stats_detailed() {
    let configs = vec![
        make_config("a", "http://a.example.com", 1, 0),
        make_config("b", "http://b.example.com", 1, 1),
    ];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();

    let stats = pool.stats();
    assert_eq!(stats.total_upstreams, 2);
    assert_eq!(stats.healthy_upstreams, 2);
    assert_eq!(stats.degraded_upstreams, 0);
    assert_eq!(stats.offline_upstreams, 0);
    assert_eq!(stats.total_requests, 0);
    assert_eq!(stats.total_failures, 0);
    assert_eq!(stats.failure_rate, 0.0);
    assert_eq!(stats.active_connections, 0);
}

#[test]
fn test_pool_strategy_accessor() {
    let pool = UpstreamPool::new(
        &[make_config("a", "http://a.example.com", 1, 0)],
        LoadBalanceStrategy::Weighted,
    )
    .unwrap();
    assert_eq!(pool.strategy(), LoadBalanceStrategy::Weighted);
}

#[test]
fn test_load_balance_strategy_as_str() {
    assert_eq!(LoadBalanceStrategy::RoundRobin.as_str(), "round_robin");
    assert_eq!(LoadBalanceStrategy::Weighted.as_str(), "weighted");
    assert_eq!(LoadBalanceStrategy::Random.as_str(), "random");
    assert_eq!(LoadBalanceStrategy::Failover.as_str(), "failover");
}

#[test]
fn test_pool_select_text_native() {
    let configs = vec![
        make_config("a", "http://a.example.com", 1, 0),
        make_config("b", "http://b.example.com", 1, 1),
    ];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();

    pool.get("a").unwrap().set_query_mode(QueryMode::TextNative);
    pool.get("b").unwrap().set_query_mode(QueryMode::VectorOnly);

    let selected = pool.select_text_native().unwrap();
    assert_eq!(selected.id, "a");
}

#[test]
fn test_pool_select_vector_only() {
    let configs = vec![
        make_config("a", "http://a.example.com", 1, 0),
        make_config("b", "http://b.example.com", 1, 1),
    ];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();

    pool.get("a").unwrap().set_query_mode(QueryMode::TextNative);
    pool.get("b").unwrap().set_query_mode(QueryMode::VectorOnly);

    let selected = pool.select_vector_only().unwrap();
    assert_eq!(selected.id, "b");
}

#[test]
fn test_pool_select_vector_only_none_available() {
    let configs = vec![make_config("a", "http://a.example.com", 1, 0)];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();
    pool.get("a").unwrap().set_query_mode(QueryMode::TextNative);

    assert!(pool.select_vector_only().is_none());
}

#[test]
fn test_pool_config_query_mode_override() {
    let mut config = make_config("a", "http://a.example.com", 1, 0);
    config.query_mode = Some("text_native".to_string());

    let pool = UpstreamPool::new(&[config], LoadBalanceStrategy::RoundRobin).unwrap();
    assert_eq!(pool.get("a").unwrap().query_mode(), QueryMode::TextNative);
}

#[test]
fn test_pool_config_query_mode_vector_only() {
    let mut config = make_config("a", "http://a.example.com", 1, 0);
    config.query_mode = Some("vector_only".to_string());

    let pool = UpstreamPool::new(&[config], LoadBalanceStrategy::RoundRobin).unwrap();
    assert_eq!(pool.get("a").unwrap().query_mode(), QueryMode::VectorOnly);
}

// === select_from_filtered strategy coverage ===

#[test]
fn test_select_random_strategy() {
    let configs = vec![
        make_config("a", "http://a.example.com", 1, 0),
        make_config("b", "http://b.example.com", 1, 0),
        make_config("c", "http://c.example.com", 1, 0),
    ];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::Random).unwrap();
    // Random selection should always return something
    for _ in 0..10 {
        let selected = pool.select();
        assert!(selected.is_some());
    }
}

#[test]
fn test_select_failover_strategy() {
    let configs = vec![
        make_config("primary", "http://primary.example.com", 1, 0),
        make_config("secondary", "http://secondary.example.com", 1, 1),
    ];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::Failover).unwrap();
    // Failover should pick lowest priority (primary) when all healthy
    let selected = pool.select().unwrap();
    assert_eq!(selected.id, "primary");
}

#[test]
fn test_select_failover_falls_back_when_primary_unhealthy() {
    let configs = vec![
        make_config("primary", "http://primary.example.com", 1, 0),
        make_config("secondary", "http://secondary.example.com", 1, 1),
    ];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::Failover).unwrap();
    // Make primary unhealthy
    let primary = pool.get("primary").unwrap();
    for _ in 0..10 {
        primary.record_failure();
    }
    let selected = pool.select().unwrap();
    assert_eq!(selected.id, "secondary");
}

#[test]
fn test_select_weighted_strategy() {
    let configs = vec![
        make_config("heavy", "http://heavy.example.com", 10, 0),
        make_config("light", "http://light.example.com", 1, 0),
    ];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::Weighted).unwrap();
    // With weight 10 vs 1, most selections should go to "heavy"
    let mut heavy_count = 0;
    for _ in 0..11 {
        let selected = pool.select().unwrap();
        if selected.id == "heavy" {
            heavy_count += 1;
        }
    }
    // In 11 round-robin weighted picks, heavy should get at least 8
    assert!(heavy_count >= 8, "heavy_count={}", heavy_count);
}

#[test]
fn test_select_weighted_zero_weight() {
    // When all weights are 0, it should fall back to another strategy
    let mut c1 = make_config("a", "http://a.example.com", 0, 0);
    c1.weight = Some(0);
    let mut c2 = make_config("b", "http://b.example.com", 0, 0);
    c2.weight = Some(0);
    let pool = UpstreamPool::new(&[c1, c2], LoadBalanceStrategy::Weighted).unwrap();
    // Should still return something
    assert!(pool.select().is_some());
}

// === mode_counts and type_counts ===

#[test]
fn test_mode_counts_default() {
    let configs = vec![
        make_config("a", "http://a.example.com", 1, 0),
        make_config("b", "http://b.example.com", 1, 0),
    ];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();
    let counts = pool.mode_counts();
    // Default query mode is Unknown
    assert_eq!(counts.unknown, 2);
    assert_eq!(counts.text_native, 0);
    assert_eq!(counts.vector_only, 0);
}

#[test]
fn test_mode_counts_mixed() {
    let mut c1 = make_config("a", "http://a.example.com", 1, 0);
    c1.query_mode = Some("text_native".to_string());
    let mut c2 = make_config("b", "http://b.example.com", 1, 0);
    c2.query_mode = Some("vector_only".to_string());
    let c3 = make_config("c", "http://c.example.com", 1, 0);

    let pool = UpstreamPool::new(&[c1, c2, c3], LoadBalanceStrategy::RoundRobin).unwrap();
    let counts = pool.mode_counts();
    assert_eq!(counts.text_native, 1);
    assert_eq!(counts.vector_only, 1);
    assert_eq!(counts.unknown, 1);
}

#[test]
fn test_type_counts_mixed() {
    let mut c1 = make_config("es1", "http://es.example.com", 1, 0);
    c1.upstream_type = Some("elasticsearch".to_string());
    let mut c2 = make_config("qdrant1", "http://qdrant.example.com", 1, 0);
    c2.upstream_type = Some("qdrant".to_string());
    let c3 = make_config("generic1", "http://generic.example.com", 1, 0);

    let pool = UpstreamPool::new(&[c1, c2, c3], LoadBalanceStrategy::RoundRobin).unwrap();
    let counts = pool.type_counts();
    assert_eq!(counts.fts, 1);
    assert_eq!(counts.vector_db, 1);
    assert_eq!(counts.unknown, 1);
    assert_eq!(counts.hybrid, 0);
}

// === fts/vector_db upstream filtering ===

#[test]
fn test_fts_upstreams_filtering() {
    let mut c1 = make_config("es1", "http://es.example.com", 1, 0);
    c1.upstream_type = Some("elasticsearch".to_string());
    let c2 = make_config("generic", "http://generic.example.com", 1, 0);

    let pool = UpstreamPool::new(&[c1, c2], LoadBalanceStrategy::RoundRobin).unwrap();
    let fts = pool.fts_upstreams();
    assert_eq!(fts.len(), 1);
    assert_eq!(fts[0].id, "es1");
}

#[test]
fn test_vector_db_upstreams_filtering() {
    let mut c1 = make_config("qdrant1", "http://qdrant.example.com", 1, 0);
    c1.upstream_type = Some("qdrant".to_string());
    let c2 = make_config("generic", "http://generic.example.com", 1, 0);

    let pool = UpstreamPool::new(&[c1, c2], LoadBalanceStrategy::RoundRobin).unwrap();
    let vdb = pool.vector_db_upstreams();
    assert_eq!(vdb.len(), 1);
    assert_eq!(vdb[0].id, "qdrant1");
}

#[test]
fn test_unknown_type_upstreams_filtering() {
    let mut c1 = make_config("qdrant1", "http://qdrant.example.com", 1, 0);
    c1.upstream_type = Some("qdrant".to_string());
    let c2 = make_config("generic", "http://generic.example.com", 1, 0);

    let pool = UpstreamPool::new(&[c1, c2], LoadBalanceStrategy::RoundRobin).unwrap();
    let unknown = pool.unknown_type_upstreams();
    assert_eq!(unknown.len(), 1);
    assert_eq!(unknown[0].id, "generic");
}

#[test]
fn test_with_type_filter() {
    let mut c1 = make_config("es1", "http://es.example.com", 1, 0);
    c1.upstream_type = Some("elasticsearch".to_string());
    let mut c2 = make_config("es2", "http://es2.example.com", 1, 0);
    c2.upstream_type = Some("opensearch".to_string());
    let c3 = make_config("generic", "http://generic.example.com", 1, 0);

    let pool = UpstreamPool::new(&[c1, c2, c3], LoadBalanceStrategy::RoundRobin).unwrap();
    let fts = pool.with_type(UpstreamType::FullTextSearch);
    assert_eq!(fts.len(), 2);
}

#[test]
fn test_select_fts_no_fts_upstreams() {
    let c1 = make_config("generic", "http://generic.example.com", 1, 0);
    let pool = UpstreamPool::new(&[c1], LoadBalanceStrategy::RoundRobin).unwrap();
    assert!(pool.select_fts().is_none());
}

#[test]
fn test_select_fts_with_fts_upstream() {
    let mut c1 = make_config("es1", "http://es.example.com", 1, 0);
    c1.upstream_type = Some("elasticsearch".to_string());
    let pool = UpstreamPool::new(&[c1], LoadBalanceStrategy::RoundRobin).unwrap();
    let selected = pool.select_fts();
    assert!(selected.is_some());
    assert_eq!(selected.unwrap().id, "es1");
}

#[test]
fn test_select_vector_db_with_qdrant() {
    let mut c1 = make_config("qdrant1", "http://qdrant.example.com", 1, 0);
    c1.upstream_type = Some("qdrant".to_string());
    let pool = UpstreamPool::new(&[c1], LoadBalanceStrategy::RoundRobin).unwrap();
    let selected = pool.select_vector_db();
    assert!(selected.is_some());
    assert_eq!(selected.unwrap().id, "qdrant1");
}

// === select_from_filtered with non-RoundRobin strategies ===

#[test]
fn test_select_fts_random_strategy() {
    let mut c1 = make_config("es1", "http://es.example.com", 1, 0);
    c1.upstream_type = Some("elasticsearch".to_string());
    let mut c2 = make_config("es2", "http://es2.example.com", 1, 0);
    c2.upstream_type = Some("opensearch".to_string());
    let pool = UpstreamPool::new(&[c1, c2], LoadBalanceStrategy::Random).unwrap();
    // Random should return something
    assert!(pool.select_fts().is_some());
}

#[test]
fn test_select_fts_failover_strategy() {
    let mut c1 = make_config("es1", "http://es.example.com", 1, 0);
    c1.upstream_type = Some("elasticsearch".to_string());
    let mut c2 = make_config("es2", "http://es2.example.com", 1, 1);
    c2.upstream_type = Some("opensearch".to_string());
    let pool = UpstreamPool::new(&[c1, c2], LoadBalanceStrategy::Failover).unwrap();
    let selected = pool.select_fts().unwrap();
    // Failover should pick lowest priority
    assert_eq!(selected.id, "es1");
}

#[test]
fn test_select_fts_weighted_strategy() {
    let mut c1 = make_config("es1", "http://es.example.com", 10, 0);
    c1.upstream_type = Some("elasticsearch".to_string());
    let mut c2 = make_config("es2", "http://es2.example.com", 1, 0);
    c2.upstream_type = Some("opensearch".to_string());
    let pool = UpstreamPool::new(&[c1, c2], LoadBalanceStrategy::Weighted).unwrap();
    // Should return something
    assert!(pool.select_fts().is_some());
}

#[test]
fn test_select_from_filtered_empty() {
    let c1 = make_config("generic", "http://generic.example.com", 1, 0);
    let pool = UpstreamPool::new(&[c1], LoadBalanceStrategy::Failover).unwrap();
    // No FTS upstreams, so select_fts returns None
    assert!(pool.select_fts().is_none());
}

// === create_adapter elasticsearch/qdrant paths ===

#[test]
fn test_pool_create_elasticsearch_adapter() {
    let mut c = make_config("es1", "http://es.example.com:9200", 1, 0);
    c.upstream_type = Some("elasticsearch".to_string());
    c.index = Some("my_index".to_string());
    c.search_fields = vec!["title".to_string(), "body".to_string()];
    let pool = UpstreamPool::new(&[c], LoadBalanceStrategy::RoundRobin).unwrap();
    let upstream = pool.get("es1").unwrap();
    assert!(upstream.is_fts());
}

#[test]
fn test_pool_create_qdrant_adapter() {
    let mut c = make_config("qdrant1", "http://qdrant.example.com:6333", 1, 0);
    c.upstream_type = Some("qdrant".to_string());
    c.index = Some("my_collection".to_string());
    let pool = UpstreamPool::new(&[c], LoadBalanceStrategy::RoundRobin).unwrap();
    let upstream = pool.get("qdrant1").unwrap();
    assert!(upstream.is_vector_db());
}

#[test]
fn test_pool_with_max_concurrent() {
    let mut c = make_config("a", "http://a.example.com", 1, 0);
    c.max_concurrent = Some(5);
    let pool = UpstreamPool::new(&[c], LoadBalanceStrategy::RoundRobin).unwrap();
    let upstream = pool.get("a").unwrap();
    // Pool stats should reflect the concurrency limit
    let stats = upstream.pool_stats();
    assert_eq!(stats.active_connections, 0);
}

#[test]
fn test_pool_all_accessor() {
    let configs = vec![
        make_config("a", "http://a.example.com", 1, 0),
        make_config("b", "http://b.example.com", 1, 0),
    ];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();
    assert_eq!(pool.all().len(), 2);
}

#[test]
fn test_pool_healthy_accessor() {
    let configs = vec![
        make_config("a", "http://a.example.com", 1, 0),
        make_config("b", "http://b.example.com", 1, 0),
    ];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();
    // All start healthy
    assert_eq!(pool.healthy().len(), 2);
}

#[test]
fn test_pool_select_round_robin_empty() {
    let pool = UpstreamPool::new(&[], LoadBalanceStrategy::RoundRobin).unwrap();
    assert!(pool.select().is_none());
}

// === Pool stats with recorded requests (covers failure_rate branch) ===

#[test]
fn test_pool_stats_with_requests() {
    let configs = vec![
        make_config("a", "http://a.example.com", 1, 0),
        make_config("b", "http://b.example.com", 1, 1),
    ];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();

    let a = pool.get("a").unwrap();
    a.record_success();
    a.record_success();
    a.record_failure();

    let b = pool.get("b").unwrap();
    b.record_success();

    let stats = pool.stats();
    assert_eq!(stats.total_requests, 4);
    assert_eq!(stats.total_failures, 1);
    assert!((stats.failure_rate - 0.25).abs() < 0.001);
    // Connection pool metrics (default pool)
    assert!(stats.total_max_connections > 0);
    assert!((stats.pool_utilization - 0.0).abs() < 0.001);
    assert_eq!(stats.total_queue_depth, 0);
}

#[test]
fn test_pool_stats_degraded_and_offline() {
    let configs = vec![
        make_config("a", "http://a.example.com", 1, 0),
        make_config("b", "http://b.example.com", 1, 1),
        make_config("c", "http://c.example.com", 1, 2),
    ];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();

    // Put "b" offline (3 consecutive failures)
    let b = pool.get("b").unwrap();
    b.record_failure();
    b.record_failure();
    b.record_failure();

    let stats = pool.stats();
    assert_eq!(stats.total_upstreams, 3);
    assert_eq!(stats.healthy_upstreams, 2);
    assert_eq!(stats.offline_upstreams, 1);
}

// === select_text_native / select_vector_only with various strategies ===

fn make_typed_config(
    id: &str,
    url: &str,
    upstream_type: &str,
    query_mode: &str,
) -> UpstreamEndpointConfig {
    UpstreamEndpointConfig {
        id: id.to_string(),
        url: url.to_string(),
        upstream_type: Some(upstream_type.to_string()),
        query_mode: Some(query_mode.to_string()),
        ..make_config(id, url, 1, 0)
    }
}

#[test]
fn test_select_text_native_failover_strategy() {
    let configs = vec![
        make_typed_config(
            "es1",
            "http://es1.example.com",
            "elasticsearch",
            "text_native",
        ),
        make_typed_config(
            "es2",
            "http://es2.example.com",
            "elasticsearch",
            "text_native",
        ),
    ];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::Failover).unwrap();
    assert!(pool.select_text_native().is_some());
}

#[test]
fn test_select_text_native_weighted_strategy() {
    let configs = vec![make_typed_config(
        "es1",
        "http://es1.example.com",
        "elasticsearch",
        "text_native",
    )];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::Weighted).unwrap();
    assert!(pool.select_text_native().is_some());
}

#[test]
fn test_select_text_native_random_strategy() {
    let configs = vec![make_typed_config(
        "es1",
        "http://es1.example.com",
        "elasticsearch",
        "text_native",
    )];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::Random).unwrap();
    assert!(pool.select_text_native().is_some());
}

#[test]
fn test_select_vector_only_failover_strategy() {
    let configs = vec![
        make_typed_config("q1", "http://q1.example.com", "qdrant", "vector_only"),
        make_typed_config("q2", "http://q2.example.com", "qdrant", "vector_only"),
    ];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::Failover).unwrap();
    assert!(pool.select_vector_only().is_some());
}

#[test]
fn test_select_vector_only_weighted_strategy() {
    let configs = vec![make_typed_config(
        "q1",
        "http://q1.example.com",
        "qdrant",
        "vector_only",
    )];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::Weighted).unwrap();
    assert!(pool.select_vector_only().is_some());
}

#[test]
fn test_select_vector_only_random_strategy() {
    let configs = vec![make_typed_config(
        "q1",
        "http://q1.example.com",
        "qdrant",
        "vector_only",
    )];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::Random).unwrap();
    assert!(pool.select_vector_only().is_some());
}

#[test]
fn test_select_fts_failover_with_unhealthy() {
    let configs = vec![
        make_typed_config(
            "es1",
            "http://es1.example.com",
            "elasticsearch",
            "text_native",
        ),
        make_typed_config(
            "es2",
            "http://es2.example.com",
            "elasticsearch",
            "text_native",
        ),
    ];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::Failover).unwrap();

    // Make first upstream offline
    let es1 = pool.get("es1").unwrap();
    es1.record_failure();
    es1.record_failure();
    es1.record_failure();

    // Should still find the second one
    let selected = pool.select_fts();
    assert!(selected.is_some());
    assert_eq!(selected.unwrap().id, "es2");
}

#[test]
fn test_select_vector_db_failover_with_unhealthy() {
    let configs = vec![
        make_typed_config("q1", "http://q1.example.com", "qdrant", "vector_only"),
        make_typed_config("q2", "http://q2.example.com", "qdrant", "vector_only"),
    ];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::Failover).unwrap();

    // Make first upstream offline
    let q1 = pool.get("q1").unwrap();
    q1.record_failure();
    q1.record_failure();
    q1.record_failure();

    let selected = pool.select_vector_db();
    assert!(selected.is_some());
    assert_eq!(selected.unwrap().id, "q2");
}

#[test]
fn test_pool_available_excludes_offline() {
    let configs = vec![
        make_config("a", "http://a.example.com", 1, 0),
        make_config("b", "http://b.example.com", 1, 1),
    ];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();

    // Put "b" offline
    let b = pool.get("b").unwrap();
    b.record_failure();
    b.record_failure();
    b.record_failure();

    assert_eq!(pool.available().len(), 1);
    assert_eq!(pool.healthy().len(), 1);
}

#[test]
fn test_pooled_upstream_connection_pool_accessors() {
    let config = make_config("test", "http://test.example.com", 1, 0);
    let upstream = PooledUpstream::new(&config).unwrap();

    // Default pool config: max_connections = 100
    assert_eq!(upstream.max_connections(), 100);
    assert_eq!(upstream.active_connections(), 0);
    assert_eq!(upstream.queue_depth(), 0);
    assert!(upstream.has_pool_capacity());
    assert_eq!(upstream.available_connections(), 100);

    let stats = upstream.pool_stats();
    assert_eq!(stats.active_connections, 0);
    assert_eq!(stats.queue_depth, 0);
}

#[test]
fn test_pooled_upstream_with_custom_max_concurrent() {
    let mut config = make_config("test", "http://test.example.com", 1, 0);
    config.max_concurrent = Some(5);
    let upstream = PooledUpstream::new(&config).unwrap();

    assert_eq!(upstream.max_connections(), 5);
    assert_eq!(upstream.available_connections(), 5);
}

// === Atomic field tests ===

#[test]
fn test_upstream_enable_disable_atomic() {
    let config = make_config("atomic-test", "http://atomic.example.com", 1, 0);
    let upstream = Arc::new(PooledUpstream::new(&config).unwrap());

    // Starts enabled
    assert!(upstream.is_available());

    // Disable and verify across a clone (simulates cross-thread visibility)
    let upstream2 = upstream.clone();
    upstream.disable();
    assert!(!upstream2.is_available());

    // Re-enable
    upstream2.enable();
    assert!(upstream.is_available());
}

#[test]
fn test_upstream_type_atomic_roundtrip() {
    use crate::proxy::context::UpstreamType;

    let variants = [
        UpstreamType::FullTextSearch,
        UpstreamType::VectorDatabase,
        UpstreamType::Hybrid,
        UpstreamType::Unknown,
    ];

    for variant in &variants {
        let u8_val = variant.to_u8();
        let roundtrip = UpstreamType::from_u8(u8_val);
        assert_eq!(*variant, roundtrip, "roundtrip failed for {:?}", variant);
    }

    // Out-of-range falls back to Unknown
    assert_eq!(UpstreamType::from_u8(255), UpstreamType::Unknown);
}

#[test]
fn test_upstream_type_atomic_set_get() {
    use crate::proxy::context::UpstreamType;

    let config = make_config("type-test", "http://type.example.com", 1, 0);
    let upstream = PooledUpstream::new(&config).unwrap();

    // Default is Unknown (no upstream_type in config)
    assert_eq!(upstream.upstream_type(), UpstreamType::Unknown);

    upstream.set_upstream_type(UpstreamType::VectorDatabase);
    assert_eq!(upstream.upstream_type(), UpstreamType::VectorDatabase);
    assert!(upstream.is_vector_db());

    upstream.set_upstream_type(UpstreamType::FullTextSearch);
    assert_eq!(upstream.upstream_type(), UpstreamType::FullTextSearch);
    assert!(upstream.is_fts());
}

// === Regression tests for select_where / pre-sorted failover ===

fn make_typed_config_wp(
    id: &str,
    url: &str,
    upstream_type: &str,
    query_mode: &str,
    weight: u32,
    priority: u32,
) -> UpstreamEndpointConfig {
    UpstreamEndpointConfig {
        upstream_type: Some(upstream_type.to_string()),
        query_mode: Some(query_mode.to_string()),
        ..make_config(id, url, weight, priority)
    }
}

#[test]
fn test_select_text_native_weighted_zero_weight() {
    // Two text-native upstreams with weight: 0 — Weighted strategy.
    // Regression: deleted select_from_filtered had infinite recursion on zero total_weight.
    let configs = vec![
        make_typed_config_wp(
            "es1",
            "http://es1.example.com",
            "elasticsearch",
            "text_native",
            0,
            0,
        ),
        make_typed_config_wp(
            "es2",
            "http://es2.example.com",
            "elasticsearch",
            "text_native",
            0,
            0,
        ),
    ];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::Weighted).unwrap();
    assert!(pool.select_text_native().is_some());
}

#[test]
fn test_select_fts_weighted_zero_weight() {
    // Single FTS upstream with weight: 0 — Weighted strategy.
    let configs = vec![make_typed_config_wp(
        "es1",
        "http://es1.example.com",
        "elasticsearch",
        "text_native",
        0,
        0,
    )];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::Weighted).unwrap();
    assert!(pool.select_fts().is_some());
}

#[test]
fn test_select_fts_failover_respects_priority() {
    // Two FTS upstreams: priority 10 listed first, priority 0 listed second.
    // Failover should pick priority 0 (lower = higher priority).
    let configs = vec![
        make_typed_config_wp(
            "es-high",
            "http://es-high.example.com",
            "elasticsearch",
            "text_native",
            1,
            10,
        ),
        make_typed_config_wp(
            "es-low",
            "http://es-low.example.com",
            "elasticsearch",
            "text_native",
            1,
            0,
        ),
    ];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::Failover).unwrap();
    let selected = pool.select_fts().unwrap();
    assert_eq!(selected.id, "es-low");
}

#[test]
fn test_select_text_native_all_disabled() {
    // Two text-native upstreams, both disabled.
    let configs = vec![
        make_typed_config(
            "es1",
            "http://es1.example.com",
            "elasticsearch",
            "text_native",
        ),
        make_typed_config(
            "es2",
            "http://es2.example.com",
            "elasticsearch",
            "text_native",
        ),
    ];
    let pool = UpstreamPool::new(&configs, LoadBalanceStrategy::RoundRobin).unwrap();

    pool.get("es1").unwrap().disable();
    pool.get("es2").unwrap().disable();

    assert!(pool.select_text_native().is_none());
}

// === Regression tests for create_adapter pgvector arm (H1) ===
// The pgvector arm uses Handle::current().block_on to call PgvectorAdapter::connect,
// which requires a live PostgreSQL+pgvector database. The routing logic is verified
// structurally: cargo check --features pgvector confirms the match arm compiles and
// PgvectorConfig fields (embedding_column, content_column) are wired. Live connect
// is covered by the existing #[ignore]d integration_pgvector test.

// Regression: pool.rs must accept `table` field, not only `index`/`collection`.
// Helm context-rooted config projects to `table` only (no `index`).
#[cfg(feature = "pgvector")]
#[tokio::test(flavor = "multi_thread")]
async fn test_pgvector_table_field_accepted() {
    let mut c = make_config("pg", "postgres://u:p@127.0.0.1:1/nonexistent", 1, 0);
    c.upstream_type = Some("pgvector".to_string());
    c.table = Some("my_table".to_string()); // table set, no index
    c.embedding_column = Some("vector".to_string());
    c.content_column = Some("content".to_string());

    let result = PooledUpstream::new(&c);
    let err = result
        .err()
        .expect("pgvector connect should fail without DB");
    assert!(
        err.contains("pgvector connect") || err.contains("connection refused"),
        "expected connect error from table field, got: {err}"
    );
}
