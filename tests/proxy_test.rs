#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

//! Integration tests for the cache proxy module.
//!
//! These tests verify the proxy components work correctly together.

use std::time::Duration;

mod common;

// Import UpstreamAdapter trait for adapter methods
use conproxy::proxy::UpstreamAdapter;

/// Test that the proxy module exports are available.
#[test]
fn test_proxy_exports() {
    use conproxy::proxy::{
        CacheStore, CircuitBreaker, CircuitBreakerConfig, ClientConfig, QdrantConfig,
        RequestCoalescer,
    };

    // Verify types are accessible (compile-time check)
    let _config: ClientConfig = ClientConfig::default();
    let _qdrant_config: QdrantConfig = QdrantConfig::default();

    // Verify we can use these types
    let _ = CacheStore::new(Duration::from_secs(60), Duration::from_secs(300), 1000);
    let _ = RequestCoalescer::new();
    let _ = CircuitBreaker::new(CircuitBreakerConfig::default());
}

/// Test cache store operations.
#[test]
fn test_cache_store_basic_ops() {
    use conproxy::proxy::{CacheStatus, CacheStore, QueryResponse, SearchResult};

    let store = CacheStore::new(
        Duration::from_secs(60),  // fresh_duration
        Duration::from_secs(300), // stale_duration
        1000,                     // max_entries
    );

    let query = "test query";

    // Initially should be a miss
    assert!(store.get(query).is_none());

    // Insert a response
    let response = QueryResponse {
        results: vec![SearchResult {
            id: "1".to_string(),
            score: 0.9,
            content: "Test content".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 100,
        generated_at: None,
        miss_reason: None,
    };

    store.insert(query, response.clone(), "test-upstream".to_string());

    // Should now be a hit
    let cached = store.get(query);
    assert!(cached.is_some());
    let entry = cached.unwrap();
    assert_eq!(entry.response.results.len(), 1);
    assert_eq!(entry.response.results[0].id, "1");
}

/// Test query normalization.
#[test]
fn test_query_normalization() {
    use conproxy::proxy::CacheStore;

    // Same content, different whitespace
    let q1 = "  hello   world  ";
    let q2 = "hello world";

    let norm1 = CacheStore::normalize_query(q1);
    let norm2 = CacheStore::normalize_query(q2);

    assert_eq!(norm1, norm2);

    // Case insensitive
    let q3 = "HELLO WORLD";
    let norm3 = CacheStore::normalize_query(q3);
    assert_eq!(norm2, norm3);
}

/// Test request coalescer creation.
#[test]
fn test_request_coalescer() {
    use conproxy::proxy::{CoalesceAction, RequestCoalescer};

    let coalescer = RequestCoalescer::new();

    // First request becomes leader
    let hash = [0u8; 32]; // Example hash
    match coalescer.get_or_insert(hash) {
        CoalesceAction::Leader => {} // Expected
        CoalesceAction::Waiter(_) => panic!("First request should be leader"),
    }

    // Second request with same hash becomes waiter
    match coalescer.get_or_insert(hash) {
        CoalesceAction::Leader => panic!("Second request should be waiter"),
        CoalesceAction::Waiter(_) => {} // Expected
    }
}

/// Test circuit breaker states.
#[test]
fn test_circuit_breaker() {
    use conproxy::proxy::{CircuitBreaker, CircuitBreakerConfig, CircuitState};

    let config = CircuitBreakerConfig {
        failure_threshold: 3,
        success_threshold: 2,
        open_duration: Duration::from_secs(30),
        failure_window: Duration::from_secs(60),
    };

    let breaker = CircuitBreaker::new(config);

    // Initially closed
    assert_eq!(breaker.state(), CircuitState::Closed);

    // Record failures
    for _ in 0..3 {
        breaker.record_failure();
    }

    // Should be open after threshold
    assert_eq!(breaker.state(), CircuitState::Open);
}

/// Test Qdrant adapter configuration.
#[test]
fn test_qdrant_adapter_config() {
    use conproxy::proxy::{QdrantAdapter, QdrantConfig};

    let config = QdrantConfig {
        base_url: "http://qdrant:6333".to_string(),
        collection_name: "test_collection".to_string(),
        timeout: Duration::from_secs(10),
        with_payload: true,
        with_vectors: false,
        score_threshold: Some(0.5),
        api_key: Some("secret".to_string()),
    };

    let adapter = QdrantAdapter::new(config).unwrap();

    assert_eq!(adapter.identifier(), "http://qdrant:6333");
    assert_eq!(adapter.timeout(), Duration::from_secs(10));

    let metadata = adapter.metadata();
    assert_eq!(metadata.adapter_type, "qdrant");
    assert_eq!(
        metadata.properties.get("collection"),
        Some(&"test_collection".to_string())
    );
}

/// Test proxy client configuration.
#[tokio::test]
async fn test_proxy_client_config() {
    use conproxy::proxy::{ClientConfig, ProxyClient};

    let config = ClientConfig::new("http://localhost:9999")
        .with_timeout(Duration::from_secs(5))
        .with_api_key("test-key");

    assert_eq!(config.base_url, "http://localhost:9999");
    assert_eq!(config.timeout, Duration::from_secs(5));
    assert_eq!(config.api_key, Some("test-key".to_string()));

    // Should be able to create a client
    let client = ProxyClient::new(config);
    assert!(client.is_ok());
}

/// Test QueryMode discovery support.
#[test]
fn test_query_mode() {
    use conproxy::proxy::{QdrantAdapter, QueryMode};

    let adapter =
        QdrantAdapter::simple("http://localhost:6333", "test", Duration::from_secs(30)).unwrap();

    // Default should be Unknown
    assert_eq!(adapter.query_mode(), QueryMode::Unknown);

    // Can set to TextNative
    adapter.set_query_mode(QueryMode::TextNative);
    assert_eq!(adapter.query_mode(), QueryMode::TextNative);

    // Can set to VectorOnly
    adapter.set_query_mode(QueryMode::VectorOnly);
    assert_eq!(adapter.query_mode(), QueryMode::VectorOnly);
}

/// Test context manager operations.
#[test]
fn test_context_manager() {
    use conproxy::proxy::{ContextConfig, ContextManager, QueryMode};

    let config = ContextConfig {
        default_context: "default".to_string(),
        max_active_contexts: 5,
        auto_create: true,
        inactive_timeout: Duration::from_secs(3600),
    };
    let manager = ContextManager::new(config);

    // Initial state
    assert_eq!(manager.current(), "default");
    assert_eq!(manager.active_count(), 1);

    // Create a new context
    manager
        .create("project-a", "http://localhost:6333", "docs")
        .unwrap();
    assert_eq!(manager.active_count(), 2);

    // Switch context
    manager.switch("project-a").unwrap();
    assert_eq!(manager.current(), "project-a");

    // Get metadata
    let meta = manager.get("project-a").unwrap();
    assert_eq!(meta.upstream_url, "http://localhost:6333");
    assert_eq!(meta.collection, "docs");

    // Set query mode
    manager
        .set_query_mode("project-a", QueryMode::TextNative)
        .unwrap();
    assert_eq!(manager.query_mode("project-a"), Some(QueryMode::TextNative));

    // Record stats
    manager.record_hit();
    manager.record_miss();
    let stats = manager.stats("project-a").unwrap();
    assert_eq!(stats.hits, 1);
    assert_eq!(stats.misses, 1);

    // Delete context
    manager.switch("default").unwrap();
    manager.delete("project-a").unwrap();
    assert_eq!(manager.active_count(), 1);
}

/// Test embedder configuration.
#[test]
fn test_embedder_config() {
    use conproxy::proxy::EmbedderConfig;

    let config = EmbedderConfig::new("bge-small-en")
        .with_cache_max_entries(5000)
        .with_max_batch_size(64)
        .with_warmup(false)
        .with_cache_ttl(Duration::from_secs(1800));

    assert_eq!(config.model_name, "bge-small-en");
    assert_eq!(config.cache_max_entries, 5000);
    assert_eq!(config.max_batch_size, 64);
    assert!(!config.warmup_on_start);
    assert_eq!(config.cache_ttl, Duration::from_secs(1800));

    // Test without_cache
    let config_no_cache = EmbedderConfig::default().without_cache();
    assert_eq!(config_no_cache.cache_max_entries, 0);
}

mod mock_upstream_tests {
    use super::common;
    use common::mock_upstream::{MockUpstream, ResponseMode};
    use conproxy::proxy::upstream::GenericRestAdapter;
    use std::time::Duration;

    #[tokio::test]
    async fn test_mock_upstream_serves_query() {
        let (server, url) =
            MockUpstream::start_with_mode(ResponseMode::Success { result_count: 3 }).await;
        let adapter =
            GenericRestAdapter::new(&url, Duration::from_secs(5)).expect("adapter construction");
        let req = conproxy::proxy::QueryRequest {
            query: "test".to_string(),
            top_k: Some(3),
            priority: None,
            upstream_id: None,
            upstream_type: None,
        };
        let resp = adapter.query(&req).await.expect("query should succeed");
        assert_eq!(resp.results.len(), 3, "should get 3 mock results");
        assert_eq!(resp.results[0].id, "mock-doc-0");
        server.stop().await;
    }

    #[tokio::test]
    async fn test_mock_upstream_empty_mode() {
        let (server, url) = MockUpstream::start_with_mode(ResponseMode::Empty).await;
        let adapter =
            GenericRestAdapter::new(&url, Duration::from_secs(5)).expect("adapter construction");
        let req = conproxy::proxy::QueryRequest {
            query: "test".to_string(),
            top_k: Some(10),
            priority: None,
            upstream_id: None,
            upstream_type: None,
        };
        let resp = adapter.query(&req).await.expect("query should succeed");
        assert_eq!(resp.results.len(), 0, "empty mode returns no results");
        server.stop().await;
    }

    #[tokio::test]
    async fn test_mock_upstream_error_mode() {
        let (server, url) = MockUpstream::start_with_mode(ResponseMode::Error(503)).await;
        let adapter =
            GenericRestAdapter::new(&url, Duration::from_secs(5)).expect("adapter construction");
        let req = conproxy::proxy::QueryRequest {
            query: "test".to_string(),
            top_k: Some(10),
            priority: None,
            upstream_id: None,
            upstream_type: None,
        };
        let result = adapter.query(&req).await;
        assert!(result.is_err(), "error mode should propagate error");
        server.stop().await;
    }
}
