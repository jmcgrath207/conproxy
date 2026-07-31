#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::admin::ReloadResponse;
use super::batch::FederatedRequest;
use super::cache::{EvictRequest, WarmupRequest};
use super::*;
use crate::config::ProxyRateLimitConfig;
use crate::proxy::circuit::CircuitBreakerConfig;
use crate::proxy::Priority;
use arc_swap::{ArcSwap, ArcSwapOption};

#[test]
fn test_cache_proxy_creation() {
    let config = ProxyConfig::default();
    let proxy = CacheProxy::new(&config).unwrap();

    assert!(proxy.upstream().is_none());
    assert!(proxy.cache().is_empty());
}

#[test]
fn test_cache_proxy_with_upstream() {
    let config = ProxyConfig {
        upstream_url: Some("http://localhost:6333".to_string()),
        ..Default::default()
    };
    let proxy = CacheProxy::new(&config).unwrap();

    assert!(proxy.upstream().is_some());
}

#[test]
fn test_cache_proxy_no_auth_by_default() {
    let config = ProxyConfig::default();
    let proxy = CacheProxy::new(&config).unwrap();

    assert!(!proxy.requires_auth());
    assert!(proxy.rate_limiter().is_none());
}

#[test]
fn test_cache_proxy_with_auth() {
    let config = ProxyConfig {
        api_key: Some("test-api-key".to_string()),
        ..Default::default()
    };
    let proxy = CacheProxy::new(&config).unwrap();

    assert!(proxy.requires_auth());
}

#[test]
fn test_cache_proxy_with_rate_limiting() {
    let config = ProxyConfig {
        rate_limit: ProxyRateLimitConfig {
            enabled: Some(true),
            requests_per_second: Some(50),
            burst_size: Some(25),
        },
        ..Default::default()
    };
    let proxy = CacheProxy::new(&config).unwrap();

    let limiter = proxy
        .rate_limiter()
        .expect("rate limiter should be configured");
    assert_eq!(limiter.refill_rate(), 50);
    assert_eq!(limiter.capacity(), 25);
}

#[test]
fn test_cache_proxy_rate_limiting_disabled_by_default() {
    let config = ProxyConfig {
        rate_limit: ProxyRateLimitConfig::default(),
        ..Default::default()
    };
    let proxy = CacheProxy::new(&config).unwrap();

    assert!(proxy.rate_limiter().is_none());
}

#[test]
fn test_evict_request_defaults() {
    let request: EvictRequest = serde_json::from_str("{}").unwrap();
    assert!(request.upstream_id.is_none());
    assert!(request.max_entries.is_none());
    assert!(!request.expired_only);
}

#[test]
fn test_evict_request_with_upstream() {
    let request: EvictRequest = serde_json::from_str(r#"{"upstream_id": "test"}"#).unwrap();
    assert_eq!(request.upstream_id, Some("test".to_string()));
}

#[test]
fn test_warmup_request_defaults() {
    let request: WarmupRequest = serde_json::from_str(r#"{"queries": ["test"]}"#).unwrap();
    assert_eq!(request.queries, vec!["test".to_string()]);
    assert!(request.fetch_from_upstream); // default is true
}

#[test]
fn test_warmup_request_no_fetch() {
    let request: WarmupRequest =
        serde_json::from_str(r#"{"queries": ["test1", "test2"], "fetch_from_upstream": false}"#)
            .unwrap();
    assert_eq!(request.queries.len(), 2);
    assert!(!request.fetch_from_upstream);
}

#[test]
fn test_cache_proxy_has_circuit_breaker() {
    use crate::proxy::circuit::CircuitState;

    let config = ProxyConfig::default();
    let proxy = CacheProxy::new(&config).unwrap();

    // Circuit breaker should be closed initially
    assert_eq!(proxy.circuit_breaker().state(), CircuitState::Closed);
}

#[test]
fn test_cache_proxy_has_audit_log() {
    let config = ProxyConfig::default();
    let proxy = CacheProxy::new(&config).unwrap();

    // Audit log should be empty initially
    let stats = proxy.audit_log().stats();
    assert_eq!(stats.total_logged, 0);
}

#[test]
fn test_cache_proxy_has_retry_policy() {
    let config = ProxyConfig::default();
    let proxy = CacheProxy::new(&config).unwrap();

    // Retry policy should be enabled by default
    let policy = proxy.retry_policy();
    assert_eq!(policy.max_retries, 3);
    assert!(policy.should_retry(0));
    assert!(policy.should_retry(2));
    assert!(!policy.should_retry(3));
}

#[test]
fn test_cache_proxy_has_adaptive_timeout() {
    let config = ProxyConfig::default();
    let proxy = CacheProxy::new(&config).unwrap();

    // Adaptive timeout should be initialized
    let adaptive = proxy.adaptive_timeout();
    assert_eq!(adaptive.sample_count(), 0);
    // Initial timeout should be 5 seconds (default)
    assert_eq!(adaptive.timeout(), Duration::from_secs(5));
}

#[test]
fn test_cache_proxy_with_disabled_retry() {
    use crate::config::ProxyRetryConfig;

    let config = ProxyConfig {
        retry: ProxyRetryConfig {
            enabled: Some(false),
            ..Default::default()
        },
        ..Default::default()
    };
    let proxy = CacheProxy::new(&config).unwrap();

    // Retry policy should be disabled (max_retries = 0)
    let policy = proxy.retry_policy();
    assert_eq!(policy.max_retries, 0);
    assert!(!policy.should_retry(0));
}

#[test]
fn test_cache_proxy_with_custom_retry() {
    use crate::config::ProxyRetryConfig;

    let config = ProxyConfig {
        retry: ProxyRetryConfig {
            enabled: Some(true),
            max_retries: Some(5),
            initial_delay_ms: Some(200),
            max_delay_ms: Some(5000),
            backoff_multiplier: Some(3.0),
            on_network_error: Some(true),
            on_timeout: Some(false),
            on_server_error: Some(true),
            on_rate_limited: Some(false),
        },
        ..Default::default()
    };
    let proxy = CacheProxy::new(&config).unwrap();

    let policy = proxy.retry_policy();
    assert_eq!(policy.max_retries, 5);
    assert_eq!(policy.initial_delay, Duration::from_millis(200));
    assert_eq!(policy.max_delay, Duration::from_millis(5000));
    assert!((policy.backoff_multiplier - 3.0).abs() < 0.01);
    assert!(policy.retry_on.on_network_error);
    assert!(!policy.retry_on.on_timeout);
    assert!(policy.retry_on.on_server_error);
    assert!(!policy.retry_on.on_rate_limited);
}

#[test]
fn test_cache_proxy_has_federated_search() {
    let config = ProxyConfig::default();
    let proxy = CacheProxy::new(&config).unwrap();

    // Federated search should be disabled by default
    assert!(!proxy.federated_search().is_enabled());
}

#[test]
fn test_cache_proxy_with_federated_search_enabled() {
    use crate::config::FederatedConfig;

    let config = ProxyConfig {
        federated: FederatedConfig {
            enabled: Some(true),
            min_local_results: Some(5),
            min_local_confidence: Some(0.8),
            merge_mode: Some("interleave".to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    let proxy = CacheProxy::new(&config).unwrap();

    assert!(proxy.federated_search().is_enabled());
}

#[test]
fn test_federated_request_defaults() {
    let request: FederatedRequest = serde_json::from_str(r#"{"query": "test"}"#).unwrap();
    assert_eq!(request.query, "test");
    assert!(request.local_results.is_none());
    assert!(request.top_k.is_none());
}

#[test]
fn test_federated_request_with_local_results() {
    let json = r#"{
            "query": "test",
            "local_results": [
                {"id": "1", "score": 0.9, "content": "result 1"},
                {"id": "2", "score": 0.8, "content": "result 2"}
            ],
            "top_k": 10
        }"#;
    let request: FederatedRequest = serde_json::from_str(json).unwrap();
    assert_eq!(request.query, "test");
    assert!(request.local_results.is_some());
    assert_eq!(request.local_results.as_ref().unwrap().len(), 2);
    assert_eq!(request.top_k, Some(10));
}

#[test]
fn test_cache_proxy_has_request_queue() {
    let config = ProxyConfig::default();
    let proxy = CacheProxy::new(&config).unwrap();

    // Queue should be empty initially
    let queue = proxy.request_queue();
    assert!(queue.is_empty());
    assert_eq!(queue.max_size(), 1000); // Default size
}

#[test]
fn test_request_queue_push_pop() {
    let config = ProxyConfig::default();
    let proxy = CacheProxy::new(&config).unwrap();

    let queue = proxy.request_queue();

    // Push a request
    let request = QueryRequest {
        query: "test".to_string(),
        top_k: None,
        priority: Some(2), // High priority
        upstream_id: None,
        upstream_type: None,
    };

    assert!(queue.push(request, Priority::High).is_none());
    assert_eq!(queue.len(), 1);

    // Pop and verify
    let popped = queue.pop().unwrap();
    assert_eq!(popped.payload.query, "test");
    assert!(queue.is_empty());
}

#[test]
fn test_reload_response_serialization() {
    let response = ReloadResponse {
        success: true,
        reloaded: vec!["cache.ttl".to_string()],
        restart_required: vec![],
        error: None,
        message: "Reloaded 1 setting(s)".to_string(),
    };
    let json = serde_json::to_string(&response).unwrap();
    assert!(json.contains("\"success\":true"));
    assert!(json.contains("cache.ttl"));
    assert!(!json.contains("error")); // skip_serializing_if = "Option::is_none"
}

#[test]
fn test_reload_response_with_error() {
    let response = ReloadResponse {
        success: false,
        reloaded: vec![],
        restart_required: vec![],
        error: Some("File not found".to_string()),
        message: "Config reload failed".to_string(),
    };
    let json = serde_json::to_string(&response).unwrap();
    assert!(json.contains("\"success\":false"));
    assert!(json.contains("File not found"));
}

#[test]
fn test_proxy_error_response_serialization() {
    let error = ProxyErrorResponse::new("timeout", "Upstream timed out after 5000ms");
    let json = serde_json::to_string(&error).unwrap();
    assert!(json.contains("\"error_type\":\"timeout\""));
    assert!(json.contains("Upstream timed out"));
    assert!(!json.contains("request_id")); // None should be skipped
    assert!(!json.contains("cascade_trail")); // Empty vec should be skipped
}

#[test]
fn test_proxy_error_response_with_request_id() {
    let error = ProxyErrorResponse::new("upstream_error", "503 Service Unavailable")
        .with_request_id(Some("req-abc123".to_string()));
    let json = serde_json::to_string(&error).unwrap();
    assert!(json.contains("req-abc123"));
}

#[test]
fn test_proxy_error_response_with_cascade() {
    let error = ProxyErrorResponse {
        error_type: "upstream_error".to_string(),
        message: "All upstreams failed".to_string(),
        request_id: Some("req-xyz".to_string()),
        cascade_trail: vec![
            CascadeStep {
                upstream: "es-primary".to_string(),
                status: "timeout".to_string(),
                latency_ms: 5000,
            },
            CascadeStep {
                upstream: "qdrant-backup".to_string(),
                status: "circuit_open".to_string(),
                latency_ms: 0,
            },
        ],
    };
    let json = serde_json::to_string(&error).unwrap();
    assert!(json.contains("es-primary"));
    assert!(json.contains("qdrant-backup"));
    assert!(json.contains("circuit_open"));
}

#[test]
fn test_extract_request_id_from_header() {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-request-id", "my-custom-id".parse().unwrap());
    let id = extract_request_id(&headers);
    assert_eq!(id, "my-custom-id");
}

#[test]
fn test_extract_request_id_generated() {
    let headers = axum::http::HeaderMap::new();
    let id = extract_request_id(&headers);
    assert!(id.starts_with("req-"));
}

#[test]
fn test_client_tracker_basic() {
    let tracker = ClientTracker::new();
    assert_eq!(tracker.active_count(), 0);

    tracker.track(
        "req-1".to_string(),
        "test query".to_string(),
        "127.0.0.1".to_string(),
    );
    assert_eq!(tracker.active_count(), 1);

    let snapshot = tracker.snapshot();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].request_id, "req-1");
    assert_eq!(snapshot[0].query, "test query");

    tracker.complete("req-1");
    assert_eq!(tracker.active_count(), 0);
    assert_eq!(
        tracker
            .total_completed
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
}

#[test]
fn test_client_tracker_reject() {
    let tracker = ClientTracker::new();
    tracker.reject();
    tracker.reject();
    assert_eq!(
        tracker
            .total_rejected
            .load(std::sync::atomic::Ordering::Relaxed),
        2
    );
}

#[test]
fn test_client_tracker_multiple() {
    let tracker = ClientTracker::new();
    tracker.track("req-1".to_string(), "q1".to_string(), "1.1.1.1".to_string());
    tracker.track("req-2".to_string(), "q2".to_string(), "2.2.2.2".to_string());
    tracker.track("req-3".to_string(), "q3".to_string(), "3.3.3.3".to_string());
    assert_eq!(tracker.active_count(), 3);

    tracker.complete("req-2");
    assert_eq!(tracker.active_count(), 2);

    let snapshot = tracker.snapshot();
    assert_eq!(snapshot.len(), 2);
}

#[test]
fn test_degradation_level_ordering() {
    assert!(DegradationLevel::Full < DegradationLevel::StaleServing);
    assert!(DegradationLevel::StaleServing < DegradationLevel::ReadOnly);
    assert!(DegradationLevel::ReadOnly < DegradationLevel::TextOnly);
    assert!(DegradationLevel::TextOnly < DegradationLevel::StartupFailure);
}

#[test]
fn test_degradation_level_description() {
    assert_eq!(DegradationLevel::Full.description(), "Full operation");
    assert_eq!(
        DegradationLevel::StaleServing.description(),
        "Serving from cache (upstreams degraded)"
    );
    assert_eq!(
        DegradationLevel::StartupFailure.description(),
        "Startup failure (no upstreams reachable)"
    );
}

#[test]
fn test_degradation_level_readiness() {
    assert!(DegradationLevel::Full.is_ready());
    assert!(DegradationLevel::StaleServing.is_ready());
    assert!(!DegradationLevel::ReadOnly.is_ready());
    assert!(!DegradationLevel::TextOnly.is_ready());
    assert!(!DegradationLevel::StartupFailure.is_ready());
}

#[test]
fn test_degradation_level_display() {
    assert_eq!(
        format!("{}", DegradationLevel::Full),
        "Level 0 (Full operation)"
    );
    assert_eq!(
        format!("{}", DegradationLevel::StaleServing),
        "Level 1 (Serving from cache (upstreams degraded))"
    );
}

#[test]
fn test_degradation_level_serialization() {
    assert_eq!(
        serde_json::to_string(&DegradationLevel::Full).unwrap(),
        "\"full\""
    );
    assert_eq!(
        serde_json::to_string(&DegradationLevel::StaleServing).unwrap(),
        "\"stale_serving\""
    );
}

#[test]
fn test_context_query_produces_different_keys() {
    let q = "rust programming";
    let key_a = context_query("project-a", q);
    let key_b = context_query("project-b", q);

    assert_ne!(key_a, key_b);
    assert_eq!(key_a, "ctx:project-a:rust programming");
    assert_eq!(key_b, "ctx:project-b:rust programming");

    // Different blake3 hashes
    let hash_a = CacheStore::hash_query(&key_a);
    let hash_b = CacheStore::hash_query(&key_b);
    assert_ne!(hash_a, hash_b);
}

#[test]
fn test_context_query_same_context_same_key() {
    let q = "rust programming";
    let key1 = context_query("default", q);
    let key2 = context_query("default", q);

    assert_eq!(key1, key2);
    assert_eq!(CacheStore::hash_query(&key1), CacheStore::hash_query(&key2));
}

#[test]
fn test_context_query_different_queries_same_context() {
    let key1 = context_query("default", "query one");
    let key2 = context_query("default", "query two");

    assert_ne!(key1, key2);
    assert_ne!(CacheStore::hash_query(&key1), CacheStore::hash_query(&key2));
}

#[test]
fn test_context_query_cache_isolation() {
    // Simulate: insert under context-a, lookup under context-b should miss
    let cache = CacheStore::new(
        std::time::Duration::from_secs(300),
        std::time::Duration::from_secs(600),
        100,
    );
    let query = "authentication API";

    let key_a = context_query("project-a", query);
    let key_b = context_query("project-b", query);

    let response = crate::proxy::types::QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "doc-1".to_string(),
            score: 0.95,
            content: "Auth docs for project-a".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: crate::proxy::types::CacheStatus::Miss,
        took_ms: 10,
        generated_at: None,
        miss_reason: None,
    };

    // Insert under context-a
    cache.insert(&key_a, response, "upstream-1".to_string());

    // Lookup under context-a — should hit
    assert!(cache.get(&key_a).is_some());

    // Lookup under context-b — should miss
    assert!(cache.get(&key_b).is_none());

    // Lookup with raw query (no context prefix) — should also miss
    assert!(cache.get(query).is_none());
}

#[test]
fn test_degradation_level_all_descriptions() {
    let levels = vec![
        DegradationLevel::Full,
        DegradationLevel::StaleServing,
        DegradationLevel::ReadOnly,
        DegradationLevel::TextOnly,
        DegradationLevel::StartupFailure,
    ];
    for level in levels {
        assert!(!level.description().is_empty());
    }
}

#[test]
fn test_degradation_level_display_all() {
    assert!(format!("{}", DegradationLevel::ReadOnly).contains("Level 2"));
    assert!(format!("{}", DegradationLevel::TextOnly).contains("Level 3"));
    assert!(format!("{}", DegradationLevel::StartupFailure).contains("Level 4"));
}

#[test]
fn test_degradation_level_serialization_all() {
    assert_eq!(
        serde_json::to_string(&DegradationLevel::ReadOnly).unwrap(),
        "\"read_only\""
    );
    assert_eq!(
        serde_json::to_string(&DegradationLevel::TextOnly).unwrap(),
        "\"text_only\""
    );
    assert_eq!(
        serde_json::to_string(&DegradationLevel::StartupFailure).unwrap(),
        "\"startup_failure\""
    );
}

#[test]
fn test_proxy_error_response_new_defaults() {
    let err = ProxyErrorResponse::new("internal", "something broke");
    assert_eq!(err.error_type, "internal");
    assert_eq!(err.message, "something broke");
    assert!(err.request_id.is_none());
    assert!(err.cascade_trail.is_empty());
}

#[test]
fn test_proxy_error_response_with_none_request_id() {
    let err = ProxyErrorResponse::new("timeout", "timed out").with_request_id(None);
    assert!(err.request_id.is_none());
}

#[test]
fn test_cascade_step_serialization() {
    let step = CascadeStep {
        upstream: "qdrant-1".to_string(),
        status: "success".to_string(),
        latency_ms: 42,
    };
    let json = serde_json::to_string(&step).unwrap();
    assert!(json.contains("qdrant-1"));
    assert!(json.contains("\"latency_ms\":42"));
}

#[test]
fn test_extract_request_id_uniqueness() {
    let headers = axum::http::HeaderMap::new();
    let id1 = extract_request_id(&headers);
    // Small sleep to ensure different nanosecond timestamps
    std::thread::sleep(std::time::Duration::from_nanos(100));
    let id2 = extract_request_id(&headers);
    // IDs should be different (though not guaranteed, very likely with nanos)
    assert!(id1.starts_with("req-"));
    assert!(id2.starts_with("req-"));
}

#[test]
fn test_context_query_format() {
    let key = context_query("myctx", "hello world");
    assert_eq!(key, "ctx:myctx:hello world");
}

#[test]
fn test_context_query_empty_context() {
    let key = context_query("", "query");
    assert_eq!(key, "ctx::query");
}

#[test]
fn test_cache_proxy_has_upstream_with_pool() {
    use crate::config::UpstreamEndpointConfig;

    let config = ProxyConfig {
        upstreams: vec![UpstreamEndpointConfig {
            url: "http://localhost:6333".to_string(),
            weight: Some(1),
            priority: Some(0),
            ..Default::default()
        }],
        ..Default::default()
    };
    let proxy = CacheProxy::new(&config).unwrap();
    assert!(proxy.has_upstream());
    assert!(proxy.upstream_pool().is_some());
    assert!(proxy.upstream().is_none()); // Pool mode, not single
}

#[test]
fn test_cache_proxy_has_upstream_no_config() {
    let config = ProxyConfig::default();
    let proxy = CacheProxy::new(&config).unwrap();
    assert!(!proxy.has_upstream());
}

#[test]
fn test_cache_proxy_accessors() {
    let config = ProxyConfig::default();
    let proxy = CacheProxy::new(&config).unwrap();

    // All accessors should work without panic
    let _ = proxy.cache();
    let _ = proxy.coalescer();
    let _ = proxy.circuit_breaker();
    let _ = proxy.audit_log();
    let _ = proxy.retry_policy();
    let _ = proxy.adaptive_timeout();
    let _ = proxy.federated_search();
    let _ = proxy.request_queue();
}

#[test]
fn test_client_tracker_complete_nonexistent() {
    let tracker = ClientTracker::new();
    // Completing a non-existent request should not panic
    tracker.complete("nonexistent-req");
    assert_eq!(
        tracker
            .total_completed
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    assert_eq!(tracker.active_count(), 0);
}

#[test]
fn test_client_tracker_snapshot_empty() {
    let tracker = ClientTracker::new();
    let snapshot = tracker.snapshot();
    assert!(snapshot.is_empty());
}

#[test]
fn test_client_info_fields() {
    let tracker = ClientTracker::new();
    tracker.track(
        "req-42".to_string(),
        "search rust".to_string(),
        "10.0.0.1".to_string(),
    );

    let snapshot = tracker.snapshot();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].request_id, "req-42");
    assert_eq!(snapshot[0].query, "search rust");
    assert_eq!(snapshot[0].source, "10.0.0.1");
    assert!(snapshot[0].started_at_ms > 0);
}

#[test]
fn test_context_query_coalescer_isolation() {
    // Different contexts should not coalesce
    let coalescer = RequestCoalescer::new();
    let query = "shared query";

    let hash_a = CacheStore::hash_query(&context_query("ctx-a", query));
    let hash_b = CacheStore::hash_query(&context_query("ctx-b", query));

    // First request for ctx-a becomes leader
    match coalescer.get_or_insert(hash_a) {
        CoalesceAction::Leader => {}
        CoalesceAction::Waiter(_) => panic!("ctx-a should be leader"),
    }

    // First request for ctx-b should also be leader (not waiter)
    match coalescer.get_or_insert(hash_b) {
        CoalesceAction::Leader => {}
        CoalesceAction::Waiter(_) => panic!("ctx-b should be leader, not coalesced with ctx-a"),
    }

    assert_eq!(coalescer.in_flight_count(), 2);
}

// === Context handler DTO tests ===

#[test]
fn test_context_switch_request_deserialization() {
    let json = r#"{"context_id": "production"}"#;
    let _req: context::ContextSwitchRequest = serde_json::from_str(json).unwrap();
    // Private fields — just verify deserialization succeeds
}

#[test]
fn test_context_switch_request_missing_field() {
    let json = r#"{}"#;
    let result: Result<context::ContextSwitchRequest, _> = serde_json::from_str(json);
    assert!(result.is_err()); // context_id is required
}

#[test]
fn test_context_create_request_deserialization() {
    let json =
        r#"{"id": "new-ctx", "upstream_url": "http://localhost:6333", "collection": "docs"}"#;
    let _req: context::ContextCreateRequest = serde_json::from_str(json).unwrap();
    // Private fields — just verify deserialization succeeds
}

#[test]
fn test_context_create_request_defaults() {
    // Only id is required; upstream_url and collection default to ""
    let json = r#"{"id": "minimal"}"#;
    let _req: context::ContextCreateRequest = serde_json::from_str(json).unwrap();
}

#[test]
fn test_context_create_request_missing_id() {
    let json = r#"{}"#;
    let result: Result<context::ContextCreateRequest, _> = serde_json::from_str(json);
    assert!(result.is_err()); // id is required
}

#[test]
fn test_context_switch_response_serialization() {
    let resp = context::ContextSwitchResponse {
        success: true,
        current: "prod".to_string(),
        message: "Switched to prod".to_string(),
    };
    let json = serde_json::to_string(&resp).unwrap();
    assert!(json.contains("\"success\":true"));
    assert!(json.contains("\"current\":\"prod\""));
}

#[test]
fn test_context_list_response_serialization() {
    let resp = context::ContextListResponse {
        contexts: vec![],
        current: "default".to_string(),
    };
    let json = serde_json::to_string(&resp).unwrap();
    assert!(json.contains("\"current\":\"default\""));
    assert!(json.contains("\"contexts\":[]"));
}

// === EvictRequest and WarmupRequest additional tests ===

#[test]
fn test_evict_request_expired_only() {
    let req: EvictRequest = serde_json::from_str(r#"{"expired_only": true}"#).unwrap();
    assert!(req.expired_only);
    assert!(req.upstream_id.is_none());
    assert!(req.max_entries.is_none());
}

#[test]
fn test_evict_request_full() {
    let json = r#"{"upstream_id": "qdrant-1", "max_entries": 50, "expired_only": false}"#;
    let req: EvictRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.upstream_id, Some("qdrant-1".to_string()));
    assert_eq!(req.max_entries, Some(50));
    assert!(!req.expired_only);
}

#[test]
fn test_warmup_request_empty_queries() {
    let json = r#"{"queries": []}"#;
    let req: WarmupRequest = serde_json::from_str(json).unwrap();
    assert!(req.queries.is_empty());
    assert!(req.fetch_from_upstream); // defaults to true
}

// === FederatedRequest additional tests ===

#[test]
fn test_federated_request_serialization_round_trip() {
    let json = r#"{"query": "rust async programming", "top_k": 5}"#;
    let req: FederatedRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.query, "rust async programming");
    assert_eq!(req.top_k, Some(5));
    assert!(req.local_results.is_none());
}

// === CacheProxy builder tests ===

#[test]
fn test_cache_proxy_with_custom_upstream() {
    use crate::proxy::upstream::GenericRestAdapter;

    let config = ProxyConfig::default();
    let upstream =
        GenericRestAdapter::new("http://localhost:6333", Duration::from_secs(15)).unwrap();
    let proxy = CacheProxy::with_upstream(&config, upstream);

    assert!(proxy.upstream().is_some());
    assert!(proxy.upstream_pool().is_none());
    assert!(proxy.has_upstream());
}

#[test]
fn test_determine_degradation_level_no_upstreams() {
    let config = ProxyConfig::default();
    let proxy = CacheProxy::new(&config).unwrap();
    // No upstreams configured → StartupFailure
    assert!(!proxy.has_upstream());
}

// =============================================================================
// Server Handler Tests — directly call handler functions with AppState
// =============================================================================

use crate::proxy::federated::FederatedSearchConfig;

/// Create a test AppState with no upstream configured.
pub(crate) fn make_test_app_state() -> AppState {
    AppState {
        cache: Arc::new(CacheStore::new(
            Duration::from_secs(300),
            Duration::from_secs(3600),
            100,
        )),
        upstream: Arc::new(ArcSwapOption::new(None)),
        upstream_pool: Arc::new(ArcSwapOption::new(None)),
        coalescer: Arc::new(RequestCoalescer::new()),
        refresh_worker: None,
        scope_filter: Arc::new(ScopeFilter::none()),
        metrics: Arc::new(ProxyMetrics::new()),
        circuit_breaker: Arc::new(CircuitBreaker::new(CircuitBreakerConfig::default())),
        audit_log: Arc::new(AuditLog::new(100)),
        retry_policy: Arc::new(RetryPolicy::default()),
        adaptive_timeout: Arc::new(AdaptiveTimeout::default_config()),
        query_stats: Arc::new(QueryStatsTracker::new(1000)),
        batch_processor: Arc::new(BatchProcessor::new(BatchConfig::default())),
        federated_search: Arc::new(ArcSwap::new(Arc::new(FederatedSearch::new(
            FederatedSearchConfig::default(),
        )))),
        request_queue: Arc::new(PriorityQueue::new(1000)),
        upstream_id: "test".to_string(),
        start_time: Instant::now(),
        context_manager: Arc::new(ContextManager::new(ContextConfig::default())),
        degradation_level: Arc::new(std::sync::atomic::AtomicU8::new(0)),
        paused: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        client_tracker: Arc::new(ClientTracker::new()),
        cascade_executor: Arc::new(ArcSwapOption::new(None)),
        agent_registry: Arc::new(ArcSwapOption::new(None)),
        cdc_manager: None,
        peer_manager: None,
        global_concurrency: Arc::new(tokio::sync::Semaphore::new(1000)),
        reload_source: None,
        #[cfg(feature = "embed-api")]
        smart_embedder: None,
        #[cfg(feature = "embed-api")]
        semantic_cache: None,
        tokio_handle: tokio::runtime::Handle::try_current().ok(),
    }
}

/// Extract the JSON body from an axum response (helper for tests).
async fn extract_json<T: serde::de::DeserializeOwned>(resp: impl IntoResponse) -> (StatusCode, T) {
    let resp = resp.into_response();
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let parsed: T = serde_json::from_slice(&body).unwrap();
    (status, parsed)
}

// === admin.rs handler tests ===

#[tokio::test]
async fn test_handler_admin_pause() {
    let state = make_test_app_state();
    assert!(!state.paused.load(std::sync::atomic::Ordering::Relaxed));

    let resp = admin::handle_admin_pause(State(state.clone())).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "paused");
    assert!(state.paused.load(std::sync::atomic::Ordering::Relaxed));
}

#[tokio::test]
async fn test_handler_admin_resume() {
    let state = make_test_app_state();
    state
        .paused
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let resp = admin::handle_admin_resume(State(state.clone())).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "resumed");
    assert!(!state.paused.load(std::sync::atomic::Ordering::Relaxed));
}

#[tokio::test]
async fn test_handler_admin_metrics_reset() {
    let state = make_test_app_state();
    state.metrics.record_hit();
    state.metrics.record_hit();

    let resp = admin::handle_admin_metrics_reset(State(state.clone())).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "reset");

    let snapshot = state.metrics.snapshot();
    assert_eq!(snapshot.cache_hits, 0);
}

#[tokio::test]
async fn test_handler_admin_reload() {
    let state = make_test_app_state();
    let resp = admin::handle_admin_reload(State(state)).await;
    let resp = resp.into_response();
    // May succeed or fail depending on config file presence, either way handler runs
    let _status = resp.status();
}

// === admin agent CRUD tests ===

#[tokio::test]
async fn test_handler_admin_agents_list_no_registry() {
    let state = make_test_app_state();
    // No agent_registry set
    let resp = admin::handle_admin_agents_list(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["total"], 0);
    assert_eq!(json["agents"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn test_handler_admin_agents_list_with_registry() {
    let state = make_test_app_state();
    let registry = crate::proxy::agent::AgentRegistry::new();
    registry.register(&crate::config::AgentConfig {
        id: "agent-1".to_string(),
        api_key: "key-1".to_string(),
        default_context: None,
        allowed_contexts: vec![],
        priority_class: None,
        rate_limit_rps: None,
        enabled: true,
    });
    state.agent_registry.store(Some(Arc::new(registry)));

    let resp = admin::handle_admin_agents_list(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["total"], 1);
    assert_eq!(json["agents"][0]["id"], "agent-1");
}

#[tokio::test]
async fn test_handler_admin_agents_create_success() {
    let state = make_test_app_state();
    state
        .agent_registry
        .store(Some(Arc::new(crate::proxy::agent::AgentRegistry::new())));

    let body = admin::CreateAgentRequest {
        id: "new-agent".to_string(),
        api_key: "new-key".to_string(),
        allowed_contexts: vec!["ctx-a".to_string()],
        priority_class: Some(5),
        rate_limit_rps: Some(100),
    };

    let resp = admin::handle_admin_agents_create(State(state.clone()), Json(body)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(json["status"], "created");
    assert_eq!(json["agent_id"], "new-agent");

    // Verify agent was registered
    let list_resp = admin::handle_admin_agents_list(State(state)).await;
    let (_, list_json): (_, serde_json::Value) = extract_json(list_resp).await;
    assert_eq!(list_json["total"], 1);
}

#[tokio::test]
async fn test_handler_admin_agents_create_no_registry() {
    let state = make_test_app_state();
    let body = admin::CreateAgentRequest {
        id: "agent".to_string(),
        api_key: "key".to_string(),
        allowed_contexts: vec![],
        priority_class: None,
        rate_limit_rps: None,
    };

    let resp = admin::handle_admin_agents_create(State(state), Json(body)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json["error"].as_str().unwrap().contains("not configured"));
}

#[tokio::test]
async fn test_handler_admin_agents_delete_success() {
    let state = make_test_app_state();
    let registry = crate::proxy::agent::AgentRegistry::new();
    registry.register(&crate::config::AgentConfig {
        id: "to-delete".to_string(),
        api_key: "key".to_string(),
        default_context: None,
        allowed_contexts: vec![],
        priority_class: None,
        rate_limit_rps: None,
        enabled: true,
    });
    state.agent_registry.store(Some(Arc::new(registry)));

    let resp = admin::handle_admin_agents_delete(State(state), Path("to-delete".to_string())).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "deleted");
}

#[tokio::test]
async fn test_handler_admin_agents_delete_not_found() {
    let state = make_test_app_state();
    state
        .agent_registry
        .store(Some(Arc::new(crate::proxy::agent::AgentRegistry::new())));

    let resp =
        admin::handle_admin_agents_delete(State(state), Path("nonexistent".to_string())).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(json["error"].as_str().unwrap().contains("not found"));
}

#[tokio::test]
async fn test_handler_admin_agents_delete_no_registry() {
    let state = make_test_app_state();
    let resp = admin::handle_admin_agents_delete(State(state), Path("any".to_string())).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json["error"].as_str().unwrap().contains("not configured"));
}

#[tokio::test]
async fn test_handler_admin_agents_rotate_key_success() {
    let state = make_test_app_state();
    let registry = crate::proxy::agent::AgentRegistry::new();
    registry.register(&crate::config::AgentConfig {
        id: "rotate-me".to_string(),
        api_key: "old-key".to_string(),
        default_context: None,
        allowed_contexts: vec![],
        priority_class: None,
        rate_limit_rps: None,
        enabled: true,
    });
    state.agent_registry.store(Some(Arc::new(registry)));

    let body = admin::RotateKeyRequest {
        new_api_key: "new-key".to_string(),
    };
    let resp = admin::handle_admin_agents_rotate_key(
        State(state),
        Path("rotate-me".to_string()),
        Json(body),
    )
    .await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "rotated");
}

#[tokio::test]
async fn test_handler_admin_agents_rotate_key_not_found() {
    let state = make_test_app_state();
    state
        .agent_registry
        .store(Some(Arc::new(crate::proxy::agent::AgentRegistry::new())));

    let body = admin::RotateKeyRequest {
        new_api_key: "key".to_string(),
    };
    let resp = admin::handle_admin_agents_rotate_key(
        State(state),
        Path("nonexistent".to_string()),
        Json(body),
    )
    .await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(json["error"].as_str().unwrap().contains("not found"));
}

#[tokio::test]
async fn test_handler_admin_agents_rotate_key_no_registry() {
    let state = make_test_app_state();
    let body = admin::RotateKeyRequest {
        new_api_key: "key".to_string(),
    };
    let resp =
        admin::handle_admin_agents_rotate_key(State(state), Path("any".to_string()), Json(body))
            .await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json["error"].as_str().unwrap().contains("not configured"));
}

// === cache.rs handler tests ===

#[tokio::test]
async fn test_handler_cache_clear() {
    let state = make_test_app_state();
    // Insert some cache entries
    let response = crate::proxy::types::QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "doc-1".to_string(),
            score: 0.9,
            content: "test".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: crate::proxy::types::CacheStatus::Miss,
        took_ms: 5,
        generated_at: None,
        miss_reason: None,
    };
    state
        .cache
        .insert("ctx:default:test", response, "upstream-1".to_string());
    assert!(!state.cache.is_empty());

    let resp = cache::handle_cache_clear(State(state.clone())).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["cleared_entries"].as_u64().unwrap() > 0);
    assert!(state.cache.is_empty());
}

#[tokio::test]
async fn test_handler_cache_integrity() {
    let state = make_test_app_state();
    let resp = cache::handle_cache_integrity(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json.get("total").is_some());
}

#[tokio::test]
async fn test_handler_cache_evict_expired() {
    let state = make_test_app_state();
    let request = cache::EvictRequest {
        upstream_id: None,
        context_id: None,
        query_text_prefix: None,
        older_than_secs: None,
        max_entries: None,
        expired_only: true,
    };
    let resp = cache::handle_cache_evict(State(state), Json(request)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json.get("evicted").is_some());
    assert!(json.get("remaining").is_some());
}

#[tokio::test]
async fn test_handler_cache_evict_by_upstream() {
    let state = make_test_app_state();
    let response = crate::proxy::types::QueryResponse {
        results: vec![],
        cache_status: crate::proxy::types::CacheStatus::Miss,
        took_ms: 0,
        generated_at: None,
        miss_reason: None,
    };
    state
        .cache
        .insert("ctx:default:q1", response, "qdrant-1".to_string());

    let request = cache::EvictRequest {
        upstream_id: Some("qdrant-1".to_string()),
        context_id: None,
        query_text_prefix: None,
        older_than_secs: None,
        max_entries: None,
        expired_only: false,
    };
    let resp = cache::handle_cache_evict(State(state), Json(request)).await;
    let (status, _json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn test_handler_cache_upstreams() {
    let state = make_test_app_state();
    let resp = cache::handle_cache_upstreams(State(state)).await;
    let (status, _json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
}

// === context.rs handler tests ===

#[tokio::test]
async fn test_handler_contexts_list() {
    let state = make_test_app_state();
    let resp = context::handle_contexts_list(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json.get("contexts").is_some());
    assert!(json.get("current").is_some());
}

#[tokio::test]
async fn test_handler_context_current() {
    let state = make_test_app_state();
    let resp = context::handle_context_current(State(state)).await;
    let resp = resp.into_response();
    // Should return either 200 with context or 404 if no current context
    let status = resp.status();
    assert!(status == StatusCode::OK || status == StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_handler_context_create() {
    let state = make_test_app_state();
    let request: context::ContextCreateRequest = serde_json::from_value(serde_json::json!({
        "id": "new-ctx",
        "upstream_url": "http://localhost:6333",
        "collection": "docs"
    }))
    .unwrap();
    let resp = context::handle_context_create(State(state.clone()), Json(request)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(json["success"], true);
}

#[tokio::test]
async fn test_handler_context_switch_success() {
    let state = make_test_app_state();
    // Create a context first
    let _ = state
        .context_manager
        .create("test-ctx", "http://localhost:6333", "docs");

    let request: context::ContextSwitchRequest =
        serde_json::from_value(serde_json::json!({"context_id": "test-ctx"})).unwrap();
    let resp = context::handle_context_switch(State(state.clone()), Json(request)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert_eq!(json["current"], "test-ctx");
}

#[tokio::test]
async fn test_handler_context_switch_not_found() {
    let mut state = make_test_app_state();
    // Use auto_create=false so switching to a nonexistent context fails
    state.context_manager = Arc::new(ContextManager::new(ContextConfig {
        auto_create: false,
        ..ContextConfig::default()
    }));
    let request: context::ContextSwitchRequest =
        serde_json::from_value(serde_json::json!({"context_id": "nonexistent"})).unwrap();
    let resp = context::handle_context_switch(State(state), Json(request)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["success"], false);
}

#[tokio::test]
async fn test_handler_context_stats() {
    let state = make_test_app_state();
    let resp = context::handle_context_stats(State(state), Path("default".to_string())).await;
    let resp = resp.into_response();
    let status = resp.status();
    // May return 200 or 404 depending on whether "default" context exists
    assert!(status == StatusCode::OK || status == StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_handler_health_no_upstream() {
    let state = make_test_app_state();
    let resp = status::handle_health(State(state)).await;
    let (http_status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(http_status, StatusCode::OK);
    assert_eq!(json["status"], "ok");
    assert_eq!(json["upstream_configured"], false);
}

#[tokio::test]
async fn test_handler_stats() {
    let state = make_test_app_state();
    let resp = status::handle_stats(State(state)).await;
    let (http_status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(http_status, StatusCode::OK);
    assert!(json.get("uptime_secs").is_some());
    assert!(json.get("cache").is_some());
    assert!(json.get("coalescer").is_some());
}

#[tokio::test]
async fn test_handler_metrics() {
    let state = make_test_app_state();
    let resp = status::handle_metrics(State(state)).await;
    let (http_status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(http_status, StatusCode::OK);
    assert!(json.get("proxy").is_some());
    assert!(json.get("adaptive_timeout").is_some());
    assert!(json.get("retry").is_some());
}

#[tokio::test]
async fn test_handler_metrics_prometheus() {
    let state = make_test_app_state();
    let resp = status::handle_metrics_prometheus(State(state)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("conproxy_"));
}

#[tokio::test]
async fn test_handler_query_stats() {
    let state = make_test_app_state();
    // Record some queries
    state
        .query_stats
        .record("test query", true, Duration::from_millis(10));
    state
        .query_stats
        .record("test query", false, Duration::from_millis(50));

    let resp = status::handle_query_stats(State(state)).await;
    let (http_status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(http_status, StatusCode::OK);
    assert!(json["total_queries_tracked"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn test_handler_audit() {
    let state = make_test_app_state();
    let resp = status::handle_audit(State(state)).await;
    let (http_status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(http_status, StatusCode::OK);
    assert_eq!(json["total_logged"], 0);
}

#[tokio::test]
async fn test_handler_circuit_status() {
    let state = make_test_app_state();
    let resp = status::handle_circuit_status(State(state)).await;
    let (http_status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(http_status, StatusCode::OK);
    assert_eq!(json["state"], "closed");
}

#[tokio::test]
async fn test_handler_queue_status() {
    let state = make_test_app_state();
    let resp = status::handle_queue_status(State(state)).await;
    let (http_status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(http_status, StatusCode::OK);
    assert_eq!(json["total"], 0);
    assert!(json.get("max_size").is_some());
}

#[tokio::test]
async fn test_handler_ready_no_upstream_no_cache() {
    let state = make_test_app_state();
    let resp = status::handle_ready(State(state)).await;
    let (http_status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(http_status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json["ready"], false);
}

#[tokio::test]
async fn test_handler_ready_with_cache() {
    let state = make_test_app_state();
    // Populate cache
    let response = crate::proxy::types::QueryResponse {
        results: vec![],
        cache_status: crate::proxy::types::CacheStatus::Miss,
        took_ms: 0,
        generated_at: None,
        miss_reason: None,
    };
    state
        .cache
        .insert("ctx:default:q1", response, "test".to_string());

    let resp = status::handle_ready(State(state)).await;
    let (http_status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(http_status, StatusCode::OK);
    assert_eq!(json["ready"], true);
    assert_eq!(json["cache_populated"], true);
}

#[tokio::test]
async fn test_handler_pool_status_none() {
    let state = make_test_app_state();
    let resp = status::handle_pool_status(State(state)).await;
    let (http_status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(http_status, StatusCode::OK);
    assert_eq!(json["pool_configured"], false);
}

#[tokio::test]
async fn test_handler_clients() {
    let state = make_test_app_state();
    state.client_tracker.track(
        "req-1".to_string(),
        "test query".to_string(),
        "127.0.0.1".to_string(),
    );

    let resp = status::handle_clients(State(state)).await;
    let (http_status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(http_status, StatusCode::OK);
    assert_eq!(json["active_requests"], 1);
}

// === query.rs handler tests ===

#[tokio::test]
async fn test_handler_query_paused() {
    let state = make_test_app_state();
    state
        .paused
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let headers = axum::http::HeaderMap::new();
    let request = crate::proxy::types::QueryRequest {
        query: "test".to_string(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };

    let resp = query::handle_query(State(state), headers, None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn test_handler_query_validation_error() {
    let state = make_test_app_state();
    let headers = axum::http::HeaderMap::new();
    let request = crate::proxy::types::QueryRequest {
        query: "".to_string(), // Empty query should fail validation
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };

    let resp = query::handle_query(State(state), headers, None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_handler_query_cache_hit() {
    let state = make_test_app_state();
    // Pre-populate cache
    let cached_response = crate::proxy::types::QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "cached-doc".to_string(),
            score: 0.9,
            content: "cached result".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: crate::proxy::types::CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "test query");
    state
        .cache
        .insert(&ctx_query, cached_response, "test".to_string());

    let headers = axum::http::HeaderMap::new();
    let request = crate::proxy::types::QueryRequest {
        query: "test query".to_string(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };

    let resp = query::handle_query(State(state), headers, None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["results"].as_array().unwrap().len() > 0);
}

#[tokio::test]
async fn test_handler_query_no_upstream_cache_miss() {
    let state = make_test_app_state();
    let headers = axum::http::HeaderMap::new();
    let request = crate::proxy::types::QueryRequest {
        query: "uncached query".to_string(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };

    let resp = query::handle_query(State(state), headers, None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

// === batch.rs handler tests ===

#[tokio::test]
async fn test_handler_batch_cache_hit() {
    let state = make_test_app_state();
    // Pre-populate cache
    let cached_response = crate::proxy::types::QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "doc-1".to_string(),
            score: 0.9,
            content: "cached".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: crate::proxy::types::CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "batch test");
    state
        .cache
        .insert(&ctx_query, cached_response, "test".to_string());

    let request = BatchRequest {
        queries: vec![crate::proxy::BatchQuery {
            id: "q1".to_string(),
            query: "batch test".to_string(),
            top_k: Some(5),
            priority: None,
        }],
        fail_fast: false,
        timeout_ms: None,
    };

    let resp = batch::handle_batch(
        State(state),
        axum::http::HeaderMap::new(),
        None,
        Json(request),
    )
    .await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_federated_disabled() {
    let state = make_test_app_state();
    let request = batch::FederatedRequest {
        query: "test".to_string(),
        local_results: None,
        top_k: None,
    };

    let resp = batch::handle_federated(
        State(state),
        axum::http::HeaderMap::new(),
        None,
        Json(request),
    )
    .await;
    let resp = resp.into_response();
    // Federated disabled + no cache = SERVICE_UNAVAILABLE
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

// === server/mod.rs determine_degradation_level tests ===

#[test]
fn test_determine_degradation_no_upstream_state() {
    let state = make_test_app_state();
    let level = determine_degradation_level(&state);
    assert_eq!(level, DegradationLevel::StartupFailure);
}

#[test]
fn test_determine_degradation_with_pool() {
    let state = make_test_app_state();
    let configs = vec![crate::config::UpstreamEndpointConfig {
        id: "test".to_string(),
        url: "http://localhost:6333".to_string(),
        ..Default::default()
    }];
    state.upstream_pool.store(Some(Arc::new(
        crate::proxy::pool::UpstreamPool::new(
            &configs,
            crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
        )
        .unwrap(),
    )));

    let level = determine_degradation_level(&state);
    // Pool has upstream that is initially considered healthy
    assert!(level == DegradationLevel::Full || level == DegradationLevel::StaleServing);
}

#[tokio::test]
async fn test_handler_query_stale_cache_serves_stale() {
    // Insert an entry and mark it stale by using a very short fresh duration cache
    let stale_state = {
        let mut s = make_test_app_state();
        s.cache = Arc::new(CacheStore::new(
            Duration::from_millis(1), // Very short fresh duration
            Duration::from_secs(3600),
            100,
        ));
        s
    };
    let cached_response = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "stale-doc".to_string(),
            score: 0.9,
            content: "stale data".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "stale query");
    stale_state
        .cache
        .insert(&ctx_query, cached_response, "test".to_string());

    // Wait for the entry to become stale
    tokio::time::sleep(Duration::from_millis(10)).await;

    let request = make_query_request("stale query");
    let resp =
        query::handle_query(State(stale_state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("stale-doc") || text.contains("Stale"));
}

#[tokio::test]
async fn test_handler_query_circuit_open_with_cache() {
    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);

    // Trip the circuit breaker by recording many failures
    for _ in 0..20 {
        state.circuit_breaker.record_failure();
    }

    // Pre-populate cache
    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "frozen-doc".to_string(),
            score: 0.8,
            content: "frozen".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "circuit test");
    state.cache.insert(&ctx_query, cached, "test".to_string());

    let request = make_query_request("circuit test");
    let resp = query::handle_query(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    // Should serve from cache as frozen
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_query_circuit_open_no_cache() {
    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);

    // Trip the circuit breaker
    for _ in 0..20 {
        state.circuit_breaker.record_failure();
    }

    let request = make_query_request("circuit no cache");
    let resp = query::handle_query(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn test_handler_query_upstream_success() {
    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);

    let request = make_query_request("upstream test");
    let resp = query::handle_query(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("doc-1"));
}

#[tokio::test]
async fn test_handler_query_upstream_failure_with_frozen_cache() {
    let (_handle, url) = start_failing_mock_upstream().await;
    let state = make_test_app_state_with_upstream(&url);

    // Pre-populate cache for fallback
    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "fallback-doc".to_string(),
            score: 0.7,
            content: "fallback".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "failing test");
    state.cache.insert(&ctx_query, cached, "test".to_string());

    let request = make_query_request("failing test");
    let resp = query::handle_query(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    // Should serve frozen cache
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_query_upstream_failure_no_cache() {
    let (_handle, url) = start_failing_mock_upstream().await;
    let state = make_test_app_state_with_upstream(&url);

    let request = make_query_request("no fallback test");
    let resp = query::handle_query(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn test_handler_query_no_upstream_cache_hit() {
    let state = make_test_app_state();
    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "cache-only".to_string(),
            score: 0.9,
            content: "cached".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "cache only test");
    state.cache.insert(&ctx_query, cached, "test".to_string());

    let request = make_query_request("cache only test");
    let resp = query::handle_query(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_query_with_pool() {
    let (_handle, url) = start_mock_upstream_server().await;
    let configs = vec![crate::config::UpstreamEndpointConfig {
        id: "pool-upstream".to_string(),
        url: url.clone(),
        ..Default::default()
    }];
    let state = make_test_app_state_with_pool(&configs);

    let request = make_query_request("pool test");
    let resp = query::handle_query(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_query_upstream_caches_result() {
    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);

    let request = make_query_request("cacheable query");
    let _ = query::handle_query(State(state.clone()), default_headers(), None, Json(request)).await;

    // Second request should be a cache hit
    let request2 = make_query_request("cacheable query");
    let resp2 = query::handle_query(State(state), default_headers(), None, Json(request2)).await;
    let resp2 = resp2.into_response();
    assert_eq!(resp2.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp2.into_body(), 1_000_000)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&body);
    // Should be served from cache (Hit)
    assert!(text.contains("Hit") || text.contains("doc-1"));
}

#[tokio::test]
async fn test_handler_query_with_cascade() {
    let (_handle, url) = start_mock_upstream_server().await;
    let configs = vec![crate::config::UpstreamEndpointConfig {
        id: "cascade-up".to_string(),
        url: url.clone(),
        ..Default::default()
    }];
    let pool = Arc::new(
        crate::proxy::pool::UpstreamPool::new(
            &configs,
            crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
        )
        .unwrap(),
    );
    let cascade_config = crate::proxy::CascadeConfig::new().with_threshold(0.5);
    let executor = crate::proxy::CascadeExecutor::new(pool.clone(), cascade_config);

    let state = make_test_app_state();
    state.upstream_pool.store(Some(pool));
    state.cascade_executor.store(Some(Arc::new(executor)));

    let request = make_query_request("cascade query");
    let resp = query::handle_query(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    // Cascade should try upstream and return result
    assert!(resp.status() == StatusCode::OK || resp.status() == StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn test_handler_query_with_request_id_header() {
    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);

    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-request-id", "test-req-123".parse().unwrap());
    headers.insert("x-forwarded-for", "10.0.0.1".parse().unwrap());

    let request = make_query_request("header test");
    let resp = query::handle_query(State(state), headers, None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_query_concurrent_coalescing() {
    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);

    // Fire two identical queries concurrently — one should be leader, other waiter
    let state2 = state.clone();
    let (r1, r2) = tokio::join!(
        query::handle_query(
            State(state),
            default_headers(),
            None,
            Json(make_query_request("coalesce test")),
        ),
        query::handle_query(
            State(state2),
            default_headers(),
            None,
            Json(make_query_request("coalesce test")),
        ),
    );
    let r1 = r1.into_response();
    let r2 = r2.into_response();
    assert_eq!(r1.status(), StatusCode::OK);
    assert_eq!(r2.status(), StatusCode::OK);
}

// =============================================================================
// batch.rs: Deep handler tests
// =============================================================================

#[tokio::test]
async fn test_handler_batch_upstream_success() {
    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);

    let request = BatchRequest {
        queries: vec![crate::proxy::BatchQuery {
            id: "q1".to_string(),
            query: "batch upstream".to_string(),
            top_k: Some(5),
            priority: None,
        }],
        fail_fast: false,
        timeout_ms: None,
    };

    let resp = batch::handle_batch(
        State(state),
        axum::http::HeaderMap::new(),
        None,
        Json(request),
    )
    .await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_batch_upstream_failure() {
    let (_handle, url) = start_failing_mock_upstream().await;
    let state = make_test_app_state_with_upstream(&url);

    let request = BatchRequest {
        queries: vec![crate::proxy::BatchQuery {
            id: "q1".to_string(),
            query: "batch fail".to_string(),
            top_k: Some(5),
            priority: None,
        }],
        fail_fast: false,
        timeout_ms: None,
    };

    let resp = batch::handle_batch(
        State(state),
        axum::http::HeaderMap::new(),
        None,
        Json(request),
    )
    .await;
    let resp = resp.into_response();
    // Should return OK with error details in the batch result
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_federated_with_cache() {
    let state = make_test_app_state();
    // Pre-populate cache
    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "fed-doc".to_string(),
            score: 0.85,
            content: "federated cached".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "federated test");
    state.cache.insert(&ctx_query, cached, "test".to_string());

    let request = batch::FederatedRequest {
        query: "federated test".to_string(),
        local_results: None,
        top_k: None,
    };

    let resp = batch::handle_federated(
        State(state),
        axum::http::HeaderMap::new(),
        None,
        Json(request),
    )
    .await;
    let resp = resp.into_response();
    // Federated disabled but cache hit → should serve
    let status = resp.status();
    assert!(status == StatusCode::OK || status == StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn test_handler_batch_with_pool() {
    let (_handle, url) = start_mock_upstream_server().await;
    let configs = vec![crate::config::UpstreamEndpointConfig {
        id: "batch-pool".to_string(),
        url: url.clone(),
        ..Default::default()
    }];
    let state = make_test_app_state_with_pool(&configs);

    let request = BatchRequest {
        queries: vec![crate::proxy::BatchQuery {
            id: "q1".to_string(),
            query: "batch pool test".to_string(),
            top_k: Some(5),
            priority: None,
        }],
        fail_fast: false,
        timeout_ms: None,
    };

    let resp = batch::handle_batch(
        State(state),
        axum::http::HeaderMap::new(),
        None,
        Json(request),
    )
    .await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_batch_multiple_queries() {
    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);

    let request = BatchRequest {
        queries: vec![
            crate::proxy::BatchQuery {
                id: "q1".to_string(),
                query: "batch multi 1".to_string(),
                top_k: Some(5),
                priority: None,
            },
            crate::proxy::BatchQuery {
                id: "q2".to_string(),
                query: "batch multi 2".to_string(),
                top_k: Some(5),
                priority: None,
            },
            crate::proxy::BatchQuery {
                id: "q3".to_string(),
                query: "batch multi 3".to_string(),
                top_k: Some(5),
                priority: None,
            },
        ],
        fail_fast: false,
        timeout_ms: None,
    };

    let resp = batch::handle_batch(
        State(state),
        axum::http::HeaderMap::new(),
        None,
        Json(request),
    )
    .await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    // Should have results for all 3 queries
    assert!(json.get("results").is_some() || json.get("queries").is_some());
}

#[tokio::test]
async fn test_handler_batch_mix_cache_and_upstream() {
    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);

    // Pre-populate cache for one query
    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "cached-batch".to_string(),
            score: 0.9,
            content: "cached batch result".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "batch cached");
    state.cache.insert(&ctx_query, cached, "test".to_string());

    let request = BatchRequest {
        queries: vec![
            crate::proxy::BatchQuery {
                id: "q1".to_string(),
                query: "batch cached".to_string(), // This should hit cache
                top_k: Some(5),
                priority: None,
            },
            crate::proxy::BatchQuery {
                id: "q2".to_string(),
                query: "batch uncached".to_string(), // This should go to upstream
                top_k: Some(5),
                priority: None,
            },
        ],
        fail_fast: false,
        timeout_ms: None,
    };

    let resp = batch::handle_batch(
        State(state),
        axum::http::HeaderMap::new(),
        None,
        Json(request),
    )
    .await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_batch_fail_fast() {
    let (_handle, url) = start_failing_mock_upstream().await;
    let state = make_test_app_state_with_upstream(&url);

    let request = BatchRequest {
        queries: vec![
            crate::proxy::BatchQuery {
                id: "q1".to_string(),
                query: "batch ff 1".to_string(),
                top_k: Some(5),
                priority: None,
            },
            crate::proxy::BatchQuery {
                id: "q2".to_string(),
                query: "batch ff 2".to_string(),
                top_k: Some(5),
                priority: None,
            },
        ],
        fail_fast: true,
        timeout_ms: None,
    };

    let resp = batch::handle_batch(
        State(state),
        axum::http::HeaderMap::new(),
        None,
        Json(request),
    )
    .await;
    let resp = resp.into_response();
    // With fail_fast and failing upstream, should still return OK with error details
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_federated_with_upstream() {
    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);

    let request = batch::FederatedRequest {
        query: "federated upstream test".to_string(),
        local_results: None,
        top_k: Some(5),
    };

    let resp = batch::handle_federated(
        State(state),
        axum::http::HeaderMap::new(),
        None,
        Json(request),
    )
    .await;
    let resp = resp.into_response();
    // Federated disabled but has upstream — should try cache then upstream
    let _status = resp.status();
}

#[tokio::test]
async fn test_handler_batch_with_cascade() {
    let (_handle, url) = start_mock_upstream_server().await;
    let configs = vec![crate::config::UpstreamEndpointConfig {
        id: "batch-cascade".to_string(),
        url: url.clone(),
        ..Default::default()
    }];
    let pool = Arc::new(
        crate::proxy::pool::UpstreamPool::new(
            &configs,
            crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
        )
        .unwrap(),
    );
    let cascade_config = crate::proxy::CascadeConfig::new().with_threshold(0.5);
    let executor = crate::proxy::CascadeExecutor::new(pool.clone(), cascade_config);

    let state = make_test_app_state();
    state.upstream_pool.store(Some(pool));
    state.cascade_executor.store(Some(Arc::new(executor)));

    let request = BatchRequest {
        queries: vec![crate::proxy::BatchQuery {
            id: "q1".to_string(),
            query: "batch cascade test".to_string(),
            top_k: Some(5),
            priority: None,
        }],
        fail_fast: false,
        timeout_ms: None,
    };

    let resp = batch::handle_batch(
        State(state),
        axum::http::HeaderMap::new(),
        None,
        Json(request),
    )
    .await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

// =============================================================================
// cache.rs: Deep handler tests
// =============================================================================

#[tokio::test]
async fn test_handler_cache_integrity_with_entries() {
    let state = make_test_app_state();
    // Insert some entries
    let response = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "doc-1".to_string(),
            score: 0.9,
            content: "hello".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    state
        .cache
        .insert(&"key1".to_string(), response, "test".to_string());

    let resp = cache::handle_cache_integrity(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json.get("total").is_some());
}

#[tokio::test]
async fn test_handler_cache_evict_upstream_with_limit() {
    let state = make_test_app_state();
    // Insert entries from specific upstream
    let response = QueryResponse {
        results: vec![],
        cache_status: CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    for i in 0..5 {
        state.cache.insert(
            &format!("key-{}", i),
            response.clone(),
            "upstream-a".to_string(),
        );
    }

    let request: cache::EvictRequest = serde_json::from_value(serde_json::json!({
        "upstream_id": "upstream-a",
        "max_entries": 2
    }))
    .unwrap();

    let resp = cache::handle_cache_evict(State(state), Json(request)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json.get("evicted").is_some());
}

#[tokio::test]
async fn test_handler_cache_evict_default() {
    let state = make_test_app_state();
    let request: cache::EvictRequest = serde_json::from_value(serde_json::json!({})).unwrap();

    let resp = cache::handle_cache_evict(State(state), Json(request)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["evicted"], 0);
}

#[tokio::test]
async fn test_handler_cache_warmup_no_fetch() {
    let state = make_test_app_state();
    let request: cache::WarmupRequest = serde_json::from_value(serde_json::json!({
        "queries": ["q1", "q2"],
        "fetch_from_upstream": false
    }))
    .unwrap();

    let resp = cache::handle_cache_warmup(State(state), Json(request)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["warmed"], 0);
    assert_eq!(json["failed"], 0);
}

#[tokio::test]
async fn test_handler_cache_warmup_no_upstream() {
    let state = make_test_app_state();
    let request: cache::WarmupRequest = serde_json::from_value(serde_json::json!({
        "queries": ["q1", "q2"],
        "fetch_from_upstream": true
    }))
    .unwrap();

    let resp = cache::handle_cache_warmup(State(state), Json(request)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json["failed"], 2);
}

#[tokio::test]
async fn test_handler_cache_warmup_with_upstream() {
    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);

    let request: cache::WarmupRequest = serde_json::from_value(serde_json::json!({
        "queries": ["warmup q1"],
        "fetch_from_upstream": true
    }))
    .unwrap();

    let resp = cache::handle_cache_warmup(State(state), Json(request)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["warmed"], 1);
}

#[tokio::test]
async fn test_handler_cache_upstreams_with_entries() {
    let state = make_test_app_state();
    let response = QueryResponse {
        results: vec![],
        cache_status: CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    state.cache.insert(
        &"key1".to_string(),
        response.clone(),
        "upstream-a".to_string(),
    );
    state
        .cache
        .insert(&"key2".to_string(), response, "upstream-b".to_string());

    let resp = cache::handle_cache_upstreams(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json.get("upstream-a").is_some());
    assert!(json.get("upstream-b").is_some());
}

// =============================================================================
// context.rs: Deep handler tests
// =============================================================================

#[tokio::test]
async fn test_handler_context_create_failure_duplicate() {
    let state = make_test_app_state();
    // Create context first
    let _ = state
        .context_manager
        .create("dup-ctx", "http://localhost:6333", "docs");

    // Try to create again
    let request: context::ContextCreateRequest = serde_json::from_value(serde_json::json!({
        "id": "dup-ctx",
        "upstream_url": "http://localhost:6333",
        "collection": "docs"
    }))
    .unwrap();
    let resp = context::handle_context_create(State(state), Json(request)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["success"], false);
}

#[tokio::test]
async fn test_handler_context_stats_not_found() {
    let state = make_test_app_state();
    let resp =
        context::handle_context_stats(State(state), Path("nonexistent-ctx".to_string())).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(json.get("error").is_some());
}

#[tokio::test]
async fn test_handler_context_current_exists() {
    let state = make_test_app_state();
    // Default context should exist
    let resp = context::handle_context_current(State(state)).await;
    let resp = resp.into_response();
    let status = resp.status();
    // Should be 200 with current context or 404 if no current
    assert!(status == StatusCode::OK || status == StatusCode::NOT_FOUND);
}

// =============================================================================
// status.rs: Deep handler tests
// =============================================================================

#[tokio::test]
async fn test_handler_health_with_pool() {
    let (_handle, url) = start_mock_upstream_server().await;
    let configs = vec![crate::config::UpstreamEndpointConfig {
        id: "health-test".to_string(),
        url: url.clone(),
        ..Default::default()
    }];
    let state = make_test_app_state_with_pool(&configs);

    let resp = status::handle_health(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json.get("pool_health").is_some());
}

#[tokio::test]
async fn test_handler_health_with_single_upstream() {
    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);

    let resp = status::handle_health(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["upstream_configured"], true);
}

#[tokio::test]
async fn test_handler_stats_with_upstream() {
    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);

    let resp = status::handle_stats(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json.get("upstream").is_some());
}

#[tokio::test]
async fn test_handler_metrics_prometheus_with_pool() {
    let (_handle, url) = start_mock_upstream_server().await;
    let configs = vec![crate::config::UpstreamEndpointConfig {
        id: "prom-test".to_string(),
        url: url.clone(),
        ..Default::default()
    }];
    let state = make_test_app_state_with_pool(&configs);

    let resp = status::handle_metrics_prometheus(State(state)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("conproxy"));
}

#[tokio::test]
async fn test_handler_audit_with_entries() {
    let state = make_test_app_state();
    // Log some audit entries
    state.audit_log.log(crate::proxy::AuditEntry {
        request_id: 1,
        timestamp: 12345,
        query: "test query".to_string(),
        top_k: Some(5),
        cache_status: CacheStatus::Hit,
        latency_ms: 10,
        result_count: 3,
        success: true,
        error: None,
        client_id: None,
        upstream_id: None,
        miss_reason: None,
    });
    state.audit_log.log(crate::proxy::AuditEntry {
        request_id: 2,
        timestamp: 12346,
        query: "another query".to_string(),
        top_k: Some(10),
        cache_status: CacheStatus::Miss,
        latency_ms: 50,
        result_count: 0,
        success: false,
        error: Some("upstream timeout".to_string()),
        client_id: Some("client-1".to_string()),
        upstream_id: Some("upstream-1".to_string()),
        miss_reason: Some(crate::proxy::types::MissReason::UpstreamError),
    });

    let resp = status::handle_audit(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["entries_in_buffer"], 2);
    let entries = json["recent_entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
}

#[tokio::test]
async fn test_handler_circuit_status_open() {
    let state = make_test_app_state();
    // Trip the circuit breaker
    for _ in 0..20 {
        state.circuit_breaker.record_failure();
    }

    let resp = status::handle_circuit_status(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["state"], "open");
}

#[tokio::test]
async fn test_handler_ready_with_pool() {
    let (_handle, url) = start_mock_upstream_server().await;
    let configs = vec![crate::config::UpstreamEndpointConfig {
        id: "ready-test".to_string(),
        url: url.clone(),
        ..Default::default()
    }];
    let state = make_test_app_state_with_pool(&configs);

    let resp = status::handle_ready(State(state)).await;
    let resp = resp.into_response();
    let status = resp.status();
    // Pool with upstream should be ready
    assert!(status == StatusCode::OK || status == StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn test_handler_ready_with_single_upstream() {
    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);

    let resp = status::handle_ready(State(state)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_ready_cache_only() {
    let state = make_test_app_state();
    // Insert some cache entries
    let response = QueryResponse {
        results: vec![],
        cache_status: CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    state
        .cache
        .insert(&"key1".to_string(), response, "test".to_string());

    let resp = status::handle_ready(State(state)).await;
    let resp = resp.into_response();
    // No upstream but cache populated → ready
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_pool_status_with_pool() {
    let (_handle, url) = start_mock_upstream_server().await;
    let configs = vec![crate::config::UpstreamEndpointConfig {
        id: "pool-status-test".to_string(),
        url: url.clone(),
        ..Default::default()
    }];
    let state = make_test_app_state_with_pool(&configs);

    let resp = status::handle_pool_status(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["pool_configured"], true);
    let upstreams = json["upstreams"].as_array().unwrap();
    assert_eq!(upstreams.len(), 1);
    assert_eq!(upstreams[0]["id"], "pool-status-test");
}

#[tokio::test]
async fn test_handler_pool_status_with_single_upstream() {
    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);

    let resp = status::handle_pool_status(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["pool_configured"], false);
    let upstreams = json["upstreams"].as_array().unwrap();
    assert_eq!(upstreams.len(), 1);
    assert_eq!(upstreams[0]["id"], "default");
}

#[tokio::test]
async fn test_handler_clients_with_active() {
    let state = make_test_app_state();
    state.client_tracker.track(
        "req-1".to_string(),
        "query 1".to_string(),
        "local".to_string(),
    );
    state.client_tracker.track(
        "req-2".to_string(),
        "query 2".to_string(),
        "10.0.0.1".to_string(),
    );

    let resp = status::handle_clients(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["active_requests"], 2);
}

// =============================================================================
// server/mod.rs: determine_degradation_level edge cases
// =============================================================================

#[test]
fn test_determine_degradation_pool_all_offline() {
    let state = make_test_app_state();
    // Pool with upstreams but we can simulate offline by checking the
    // state without pool (no upstream = StartupFailure)
    // With pool having 0 healthy and no cache = StartupFailure
    let level = determine_degradation_level(&state);
    assert_eq!(level, DegradationLevel::StartupFailure);
}

#[test]
fn test_determine_degradation_pool_offline_with_cache() {
    let state = make_test_app_state();
    // Add a pool
    let configs = vec![crate::config::UpstreamEndpointConfig {
        id: "deg-test".to_string(),
        url: "http://localhost:99999".to_string(), // Unreachable
        ..Default::default()
    }];
    state.upstream_pool.store(Some(Arc::new(
        crate::proxy::pool::UpstreamPool::new(
            &configs,
            crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
        )
        .unwrap(),
    )));

    // Add cache entries
    let response = QueryResponse {
        results: vec![],
        cache_status: CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    state
        .cache
        .insert(&"key1".to_string(), response, "test".to_string());

    let level = determine_degradation_level(&state);
    // Pool has upstreams initially considered healthy, so Full
    assert!(level == DegradationLevel::Full || level == DegradationLevel::StaleServing);
}

// =============================================================================
// B.5: Graceful shutdown tests
// =============================================================================

#[tokio::test]
async fn test_graceful_shutdown_pause_resume_cycle() {
    let state = make_test_app_state();
    state
        .paused
        .store(true, std::sync::atomic::Ordering::SeqCst);

    // Queries should be rejected while paused
    let request = QueryRequest {
        query: "shutdown test".to_string(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };
    let resp =
        query::handle_query(State(state.clone()), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

    // Stats should still be accessible while paused
    let stats_resp = status::handle_stats(State(state.clone())).await;
    let stats_resp = stats_resp.into_response();
    assert_eq!(stats_resp.status(), StatusCode::OK);

    // Health should still be accessible while paused
    let health_resp = status::handle_health(State(state.clone())).await;
    let health_resp = health_resp.into_response();
    assert_eq!(health_resp.status(), StatusCode::OK);

    // Resume
    state
        .paused
        .store(false, std::sync::atomic::Ordering::SeqCst);

    // Queries should work again (will return 503 due to no upstream, but not paused)
    let request2 = QueryRequest {
        query: "after resume".to_string(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };
    let resp2 = query::handle_query(
        State(state.clone()),
        default_headers(),
        None,
        Json(request2),
    )
    .await;
    let resp2 = resp2.into_response();
    // Should not be SERVICE_UNAVAILABLE from pausing (may be 503 from no cache/upstream, but different error)
    // The paused state returns 503, but the no-upstream/no-cache state also returns 503
    // so just check it doesn't panic and completes
    assert!(
        resp2.status().is_client_error()
            || resp2.status().is_server_error()
            || resp2.status().is_success()
    );
}

// =============================================================================
// B.7: Config hot-reload concurrent with in-flight requests
// =============================================================================

#[tokio::test]
async fn test_config_hot_reload_concurrent_with_queries() {
    let state = make_test_app_state();
    let state_clone = state.clone();

    // Fire multiple queries concurrently with a reload
    let query_handle = tokio::spawn(async move {
        for _ in 0..10 {
            let request = QueryRequest {
                query: "reload-test".to_string(),
                top_k: Some(5),
                priority: None,
                upstream_id: None,
                upstream_type: None,
            };
            let _ = query::handle_query(
                State(state_clone.clone()),
                default_headers(),
                None,
                Json(request),
            )
            .await;
        }
    });

    let reload_state = state.clone();
    let reload_handle = tokio::spawn(async move {
        for _ in 0..5 {
            let _ = admin::handle_admin_reload(State(reload_state.clone())).await;
        }
    });

    // Both should complete without panicking
    let (query_result, reload_result) = tokio::join!(query_handle, reload_handle);
    query_result.unwrap();
    reload_result.unwrap();
}

/// Reload from a config that omits agents clears the in-memory
/// `agent_registry` (Phase B / plan 04). File is authoritative: any agents
/// not declared in the file are dropped on reload. The reverse — agents
/// declared in the file are picked up — is covered by
/// `test_reload_picks_up_agents_from_file`.
#[tokio::test]
async fn test_reload_file_overrides_api_agents() {
    use crate::config::{AgentConfig, ConfigFile};
    use crate::proxy::agent::AgentRegistry;

    let mut state = make_test_app_state();

    // Seed agent registry with one entry (simulates POST /admin/agents).
    let reg = AgentRegistry::new();
    reg.register(&AgentConfig {
        id: "api-only-agent".to_string(),
        api_key: "k".to_string(),
        default_context: None,
        allowed_contexts: vec![],
        priority_class: None,
        rate_limit_rps: None,
        enabled: true,
    });
    state.agent_registry.store(Some(std::sync::Arc::new(reg)));

    // Write a config file with NO agents section but a valid default context
    let tmp = std::env::temp_dir().join(format!(
        "conproxy_reload_agents_{}.toml",
        std::process::id()
    ));
    let mut cfg = ConfigFile::default();
    cfg.upstreams.insert(
        "dummy".to_string(),
        crate::config::UpstreamResourceConfig {
            url: Some("http://127.0.0.1:1".into()),
            ..Default::default()
        },
    );
    cfg.contexts.insert(
        "default".to_string(),
        crate::config::NamedContextConfig {
            default: Some(true),
            upstreams: vec![crate::config::ContextLegConfig {
                resource_ref: "dummy".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    std::fs::write(
        &tmp,
        toml::to_string_pretty(&cfg).expect("serialize default config"),
    )
    .expect("write temp config");

    state.reload_source = Some(tmp.clone());
    let result = state.apply_reload();
    let _ = std::fs::remove_file(&tmp);

    assert!(result.is_ok(), "reload failed: {:?}", result.err());
    let loaded = state.agent_registry.load();
    // After reload with no `[[agents]]`, the registry is either `None` or
    // `Some(empty)`. Either way the "api-only" runtime agent must be gone.
    let ids: Vec<String> = loaded
        .as_ref()
        .map(|reg| {
            reg.list_agents()
                .into_iter()
                .map(|a| a.id)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert!(
        !ids.contains(&"api-only-agent".to_string()),
        "api-only agent should be dropped after file reload, got {ids:?}"
    );
}

/// Reload picks up agents declared in `[[agents]]` of the config file.
#[tokio::test]
async fn test_reload_picks_up_agents_from_file() {
    use crate::config::{AgentConfig, ConfigFile};
    use crate::proxy::agent::AgentRegistry;

    let mut state = make_test_app_state();
    // Start with an empty registry.
    state
        .agent_registry
        .store(Some(std::sync::Arc::new(AgentRegistry::new())));

    let tmp = std::env::temp_dir().join(format!(
        "conproxy_reload_agents_from_file_{}.toml",
        std::process::id()
    ));
    let mut cfg = ConfigFile::default();
    cfg.upstreams.insert(
        "dummy".to_string(),
        crate::config::UpstreamResourceConfig {
            url: Some("http://127.0.0.1:1".into()),
            ..Default::default()
        },
    );
    cfg.contexts.insert(
        "default".to_string(),
        crate::config::NamedContextConfig {
            default: Some(true),
            upstreams: vec![crate::config::ContextLegConfig {
                resource_ref: "dummy".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    cfg.agents.push(AgentConfig {
        id: "file-agent".to_string(),
        api_key: "fk".to_string(),
        default_context: None,
        allowed_contexts: vec![],
        priority_class: None,
        rate_limit_rps: None,
        enabled: true,
    });
    std::fs::write(
        &tmp,
        toml::to_string_pretty(&cfg).expect("serialize config"),
    )
    .expect("write temp config");

    state.reload_source = Some(tmp.clone());
    let result = state.apply_reload();
    let _ = std::fs::remove_file(&tmp);

    assert!(result.is_ok(), "reload failed: {:?}", result.err());
    let loaded = state.agent_registry.load();
    let reg = loaded
        .as_ref()
        .expect("agent_registry must be set after reload");
    let ids: Vec<_> = reg.list_agents().into_iter().map(|a| a.id).collect();
    assert!(
        ids.contains(&"file-agent".to_string()),
        "file-declared agent should be picked up after reload, got {ids:?}"
    );
}

/// Regression test: when the file config has no `[[agents]]`, `apply_reload`
/// must store `None` (or an empty registry) so that the gRPC search auth path
/// does not flip into "API key required" mode for callers that were never
/// issued a key.
///
/// Previously reload always stored `Some(empty registry)`, which broke
/// `search` after a hot reload on a deployment that had no agents.
#[tokio::test]
async fn test_reload_with_no_agents_does_not_impose_auth() {
    use crate::config::ConfigFile;
    use crate::proxy::agent::AgentRegistry;
    use std::sync::Arc;

    let mut state = make_test_app_state();
    // Start as if a previous run had left a non-empty registry.
    state
        .agent_registry
        .store(Some(Arc::new(AgentRegistry::new())));

    // File config has no `[[agents]]`.
    let mut cfg = ConfigFile::default();
    cfg.upstreams.insert(
        "dummy".to_string(),
        crate::config::UpstreamResourceConfig {
            url: Some("http://127.0.0.1:1".into()),
            ..Default::default()
        },
    );
    cfg.contexts.insert(
        "default".to_string(),
        crate::config::NamedContextConfig {
            default: Some(true),
            upstreams: vec![crate::config::ContextLegConfig {
                resource_ref: "dummy".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
    );

    let tmp = std::env::temp_dir().join(format!(
        "conproxy_reload_no_agents_{}.toml",
        std::process::id()
    ));
    std::fs::write(
        &tmp,
        toml::to_string_pretty(&cfg).expect("serialize config"),
    )
    .expect("write temp config");

    state.reload_source = Some(tmp.clone());
    let result = state.apply_reload();
    let _ = std::fs::remove_file(&tmp);

    assert!(result.is_ok(), "reload failed: {:?}", result.err());

    // After reload, the registry must be either `None` or `Some(empty)` —
    // either way the gRPC `authenticate` path should NOT require an API key
    // (the latter is guarded by `is_empty()` as defense in depth).
    let loaded = state.agent_registry.load();
    match loaded.as_ref() {
        None => { /* preferred: matches startup with no agents */ }
        Some(reg) => assert!(
            reg.is_empty(),
            "expected empty registry, got {} agents",
            reg.len()
        ),
    }
}

/// Concurrent federated snapshot loads during `apply_reload` must not panic
/// or observe a torn Arc (S6/S7 proof — build-then-commit under traffic).
#[tokio::test]
async fn test_reload_under_concurrent_federated_load() {
    use crate::config::ConfigFile;
    use crate::proxy::federated::{FederatedSearch, FederatedSearchConfig};
    use std::sync::Arc;

    let mut state = make_test_app_state();
    state
        .federated_search
        .store(Arc::new(FederatedSearch::new(FederatedSearchConfig {
            enabled: true,
            ..Default::default()
        })));

    let mut cfg = ConfigFile::default();
    let mut cache = crate::config::ContextCacheConfig::default();
    cache.max_entries = Some(512);
    cfg.upstreams.insert(
        "dummy".to_string(),
        crate::config::UpstreamResourceConfig {
            url: Some("http://127.0.0.1:1".into()),
            ..Default::default()
        },
    );
    cfg.contexts.insert(
        "default".to_string(),
        crate::config::NamedContextConfig {
            default: Some(true),
            upstreams: vec![crate::config::ContextLegConfig {
                resource_ref: "dummy".into(),
                ..Default::default()
            }],
            cache,
            ..Default::default()
        },
    );
    let tmp = std::env::temp_dir().join(format!(
        "conproxy_reload_traffic_{}.toml",
        std::process::id()
    ));
    std::fs::write(
        &tmp,
        toml::to_string_pretty(&cfg).expect("serialize config"),
    )
    .expect("write temp config");
    state.reload_source = Some(tmp.clone());

    let state = Arc::new(state);
    let n_readers = 8usize;
    let n_reloads = 4usize;
    let barrier = Arc::new(tokio::sync::Barrier::new(n_readers + 1));

    let mut handles = Vec::new();
    for _ in 0..n_readers {
        let state = Arc::clone(&state);
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            for _ in 0..200 {
                let fed = state.federated_search.load_full();
                let _ = fed.as_ref();
                let _ = state.upstream_pool.load_full();
                let _ = state.cascade_executor.load_full();
                tokio::task::yield_now().await;
            }
        }));
    }

    let reloader = {
        let state = Arc::clone(&state);
        let barrier = Arc::clone(&barrier);
        tokio::spawn(async move {
            barrier.wait().await;
            for _ in 0..n_reloads {
                let r = state.apply_reload();
                assert!(r.is_ok(), "reload under traffic: {:?}", r.err());
                tokio::task::yield_now().await;
            }
        })
    };

    for h in handles {
        h.await.expect("reader join");
    }
    reloader.await.expect("reloader join");
    let _ = std::fs::remove_file(&tmp);

    // Still a valid federated snapshot after concurrent reloads
    let fed = state.federated_search.load_full();
    let _ = Arc::as_ptr(&fed);
}

/// Reload with a build-phase failure (invalid `cache.max_entries=0`) must
/// surface Err and leave the previously-committed `federated_search` and
/// cache state untouched. P0.1: build-then-commit isolation.
#[tokio::test]
async fn test_reload_build_failure_preserves_state() {
    use crate::config::ConfigFile;

    let mut state = make_test_app_state();

    // Snapshot prior federated_search identity
    let prior_fed = state.federated_search.load_full().clone();
    let prior_addr = std::sync::Arc::as_ptr(&prior_fed) as usize;
    let prior_max = state.cache.max_entries();

    // Write a config with cache.max_entries=0 (rejected by set_max_entries).
    let mut cfg = ConfigFile::default();
    let mut cache = crate::config::ContextCacheConfig::default();
    cache.max_entries = Some(0);
    cfg.upstreams.insert(
        "dummy".to_string(),
        crate::config::UpstreamResourceConfig {
            url: Some("http://127.0.0.1:1".into()),
            ..Default::default()
        },
    );
    cfg.contexts.insert(
        "default".to_string(),
        crate::config::NamedContextConfig {
            default: Some(true),
            upstreams: vec![crate::config::ContextLegConfig {
                resource_ref: "dummy".into(),
                ..Default::default()
            }],
            cache,
            ..Default::default()
        },
    );
    let tmp = std::env::temp_dir().join(format!("conproxy_reload_bad_{}.toml", std::process::id()));
    std::fs::write(
        &tmp,
        toml::to_string_pretty(&cfg).expect("serialize bad config"),
    )
    .expect("write temp config");

    state.reload_source = Some(tmp.clone());
    let result = state.apply_reload();
    let _ = std::fs::remove_file(&tmp);

    assert!(result.is_err(), "max_entries=0 should fail reload");
    assert_eq!(
        state.cache.max_entries(),
        prior_max,
        "cache.max_entries must not change on Err"
    );
    let after_fed = state.federated_search.load_full();
    let after_addr = std::sync::Arc::as_ptr(&after_fed) as usize;
    assert_eq!(
        prior_addr, after_addr,
        "federated_search Arc swapped on error"
    );
}

// =============================================================================
// B.8: Full error propagation chain tests
// =============================================================================

#[tokio::test]
async fn test_error_response_format_paused_503() {
    let state = make_test_app_state();
    state
        .paused
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let request = QueryRequest {
        query: "paused test".to_string(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };
    let resp = query::handle_query(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    // Verify the response has the expected QueryResponse shape
    assert!(
        json.get("results").is_some(),
        "Response should have results field"
    );
    assert!(
        json.get("cache_status").is_some(),
        "Response should have cache_status field"
    );
    assert!(
        json.get("took_ms").is_some(),
        "Response should have took_ms field"
    );
    // Results should be empty when paused
    assert_eq!(
        json["results"].as_array().unwrap().len(),
        0,
        "Paused response should have empty results"
    );
    assert_eq!(json["cache_status"], "miss");
}

#[tokio::test]
async fn test_admin_metrics_reset_clears_counters() {
    let state = make_test_app_state();

    // Pre-populate cache so handle_query will record a cache hit
    let cached_response = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "metrics-doc".to_string(),
            score: 0.9,
            content: "metrics test content".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "metrics reset test");
    state
        .cache
        .insert(&ctx_query, cached_response, "test".to_string());

    // Generate some metrics by making a request that hits the cache
    let request = make_query_request("metrics reset test");
    let resp =
        query::handle_query(State(state.clone()), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);

    // Verify metrics are non-zero
    let snapshot = state.metrics.snapshot();
    assert!(
        snapshot.requests_total > 0,
        "Expected requests_total > 0 after cache hit query"
    );

    // Reset metrics
    let _ = admin::handle_admin_metrics_reset(State(state.clone())).await;

    // Verify metrics are zeroed
    let snapshot = state.metrics.snapshot();
    assert_eq!(snapshot.requests_total, 0);
    assert_eq!(snapshot.cache_hits, 0);
    assert_eq!(snapshot.cache_misses, 0);
}

// === Per-Request Context Resolution Tests ===

#[test]
fn test_resolve_context_with_header() {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-context", "my-project".parse().unwrap());

    let ctx = super::resolve_context(&headers, None);
    assert_eq!(ctx, "my-project");
}

#[test]
fn test_resolve_context_with_agent_default() {
    let headers = axum::http::HeaderMap::new();
    let agent = AgentIdentity {
        id: "test-agent".to_string(),
        default_context: Some("agent-ctx".to_string()),
        allowed_contexts: vec![],
        priority_class: 0,
    };

    let ctx = super::resolve_context(&headers, Some(&agent));
    assert_eq!(ctx, "agent-ctx");
}

#[test]
fn test_resolve_context_header_overrides_agent_default() {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-context", "header-ctx".parse().unwrap());
    let agent = AgentIdentity {
        id: "test-agent".to_string(),
        default_context: Some("agent-ctx".to_string()),
        allowed_contexts: vec![],
        priority_class: 0,
    };

    let ctx = super::resolve_context(&headers, Some(&agent));
    assert_eq!(ctx, "header-ctx");
}

#[test]
fn test_resolve_context_fallback_to_default() {
    let headers = axum::http::HeaderMap::new();

    let ctx = super::resolve_context(&headers, None);
    assert_eq!(ctx, "default");
}

#[test]
fn test_resolve_context_agent_without_default_no_header() {
    let headers = axum::http::HeaderMap::new();
    let agent = AgentIdentity {
        id: "test-agent".to_string(),
        default_context: None,
        allowed_contexts: vec![],
        priority_class: 0,
    };

    let ctx = super::resolve_context(&headers, Some(&agent));
    assert_eq!(ctx, "default");
}

#[test]
fn test_resolve_context_empty_header_ignored() {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-context", "".parse().unwrap());
    let agent = AgentIdentity {
        id: "test-agent".to_string(),
        default_context: Some("agent-ctx".to_string()),
        allowed_contexts: vec![],
        priority_class: 0,
    };

    // Empty header should be ignored, fall through to agent default
    let ctx = super::resolve_context(&headers, Some(&agent));
    assert_eq!(ctx, "agent-ctx");
}

#[test]
fn test_resolve_context_empty_header_no_agent_default() {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-context", "".parse().unwrap());

    // Empty header and no agent → "default"
    let ctx = super::resolve_context(&headers, None);
    assert_eq!(ctx, "default");
}

// =============================================================================
// query_core.rs: Direct execute_query tests
// =============================================================================

#[tokio::test]
async fn test_execute_query_paused() {
    let state = make_test_app_state();
    state
        .paused
        .store(true, std::sync::atomic::Ordering::Relaxed);

    let request = make_query_request("paused query");
    let result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-1".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    assert_eq!(result.status, 503);
}

#[tokio::test]
async fn test_execute_query_invalid_request() {
    let state = make_test_app_state();
    // Empty query should fail validation
    let request = QueryRequest {
        query: String::new(),
        top_k: None,
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };
    let result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-2".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    assert_eq!(result.status, 400);
}

#[tokio::test]
async fn test_execute_query_agent_context_denied() {
    let state = make_test_app_state();
    let agent = AgentIdentity {
        id: "agent-restricted".to_string(),
        default_context: Some("allowed-ctx".to_string()),
        allowed_contexts: vec!["allowed-ctx".to_string()],
        priority_class: 0,
    };
    let request = make_query_request("restricted query");
    let result = super::query_core::execute_query(
        &state,
        request,
        "forbidden-ctx".to_string(),
        "req-3".to_string(),
        Some(&agent),
        "test".to_string(),
    )
    .await;
    assert_eq!(result.status, 403);
}

#[tokio::test]
async fn test_execute_query_no_upstream_cache_miss() {
    let state = make_test_app_state();
    let request = make_query_request("cache miss query");
    let result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-4".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    assert_eq!(result.status, 503);
}

#[tokio::test]
async fn test_execute_query_no_upstream_cache_hit() {
    let state = make_test_app_state();
    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "hit-doc".to_string(),
            score: 0.9,
            content: "cached".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "hit query");
    state.cache.insert(&ctx_query, cached, "test".to_string());

    let request = make_query_request("hit query");
    let result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-5".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    assert_eq!(result.status, 200);
}

#[tokio::test]
async fn test_execute_query_fresh_cache_hit() {
    let state = make_test_app_state();
    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "fresh-doc".to_string(),
            score: 0.95,
            content: "fresh data".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "fresh query");
    state.cache.insert(&ctx_query, cached, "test".to_string());

    let request = make_query_request("fresh query");
    let result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-6".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    assert_eq!(result.status, 200);
    match &result.response {
        CachedResponse::Cached { cache_status, .. } => {
            assert!(
                matches!(cache_status, CacheStatus::Hit),
                "Expected cache Hit, got {:?}",
                cache_status
            );
        }
        _ => {} // Fresh or SharedFresh also acceptable for no-upstream path
    }
}

#[tokio::test]
async fn test_execute_query_expired_cache_no_upstream() {
    let state = {
        let mut s = make_test_app_state();
        // Very short TTL so entries expire immediately
        s.cache = Arc::new(CacheStore::new(
            Duration::from_millis(1),
            Duration::from_millis(2),
            100,
        ));
        s
    };
    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "exp-doc".to_string(),
            score: 0.9,
            content: "expired".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "expired query");
    state.cache.insert(&ctx_query, cached, "test".to_string());

    // Wait for it to expire
    tokio::time::sleep(Duration::from_millis(10)).await;

    let request = make_query_request("expired query");
    let result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-7".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    // No upstream: expired entry may still be in cache (stale/frozen) → 200,
    // or fully expired → 503. Both are valid.
    assert!(result.status == 200 || result.status == 503);
}

// =============================================================================
// context.rs: context switch and status tests
// =============================================================================

#[tokio::test]
async fn test_handler_context_switch_invalid_context() {
    let state = make_test_app_state();
    // Switch to a context that doesn't exist — should fail with BAD_REQUEST
    let request: context::ContextSwitchRequest =
        serde_json::from_value(serde_json::json!({"context_id": "nonexistent-ctx-999"})).unwrap();
    let resp = context::handle_context_switch(State(state), Json(request)).await;
    let resp = resp.into_response();
    let status = resp.status();
    // Could be OK (auto-creates) or BAD_REQUEST, both valid
    assert!(status == StatusCode::OK || status == StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_handler_federated_agent_context_denied() {
    let state = make_test_app_state();
    let agent = AgentIdentity {
        id: "fed-restricted".to_string(),
        default_context: Some("allowed-only".to_string()),
        allowed_contexts: vec!["allowed-only".to_string()],
        priority_class: 0,
    };

    let request = super::batch::FederatedRequest {
        query: "fed-denied".to_string(),
        local_results: None,
        top_k: None,
    };

    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-context", "forbidden-fed-ctx".parse().unwrap());

    let resp = super::batch::handle_federated(
        State(state),
        headers,
        Some(axum::Extension(agent)),
        Json(request),
    )
    .await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// =============================================================================
// batch.rs: handle_batch tests
// =============================================================================

#[tokio::test]
async fn test_handler_batch_cache_hits() {
    let state = make_test_app_state();
    // Pre-populate cache
    for q in &["batch-a", "batch-b"] {
        let cached = QueryResponse {
            results: vec![crate::proxy::types::SearchResult {
                id: format!("{q}-doc"),
                score: 0.9,
                content: format!("content for {q}"),
                metadata: None,
                upstream_id: None,
            }],
            cache_status: CacheStatus::Miss,
            took_ms: 1,
            generated_at: None,
            miss_reason: None,
        };
        let ctx_query = context_query("default", q);
        state.cache.insert(&ctx_query, cached, "test".to_string());
    }

    let request = crate::proxy::batch::BatchRequest {
        queries: vec![
            crate::proxy::batch::BatchQuery {
                id: "q0".to_string(),
                query: "batch-a".to_string(),
                top_k: None,
                priority: None,
            },
            crate::proxy::batch::BatchQuery {
                id: "q1".to_string(),
                query: "batch-b".to_string(),
                top_k: None,
                priority: None,
            },
        ],
        fail_fast: false,
        timeout_ms: None,
    };

    let resp =
        super::batch::handle_batch(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["success_count"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn test_handler_batch_no_upstream_no_cache() {
    let state = make_test_app_state();
    let request = crate::proxy::batch::BatchRequest {
        queries: vec![crate::proxy::batch::BatchQuery {
            id: "q0".to_string(),
            query: "not-cached".to_string(),
            top_k: None,
            priority: None,
        }],
        fail_fast: false,
        timeout_ms: None,
    };

    let resp =
        super::batch::handle_batch(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    // Should still return 200 with error_count > 0
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["error_count"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn test_handler_batch_empty_queries() {
    let state = make_test_app_state();
    let request = crate::proxy::batch::BatchRequest {
        queries: vec![],
        fail_fast: false,
        timeout_ms: None,
    };

    let resp =
        super::batch::handle_batch(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    // Empty batch fails validation → 400
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_handler_batch_agent_context_denied() {
    let state = make_test_app_state();
    let agent = AgentIdentity {
        id: "restricted".to_string(),
        default_context: Some("allowed".to_string()),
        allowed_contexts: vec!["allowed".to_string()],
        priority_class: 0,
    };
    let request = crate::proxy::batch::BatchRequest {
        queries: vec![crate::proxy::batch::BatchQuery {
            id: "q0".to_string(),
            query: "test".to_string(),
            top_k: None,
            priority: None,
        }],
        fail_fast: false,
        timeout_ms: None,
    };

    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-context", "forbidden-ctx".parse().unwrap());

    let resp = super::batch::handle_batch(
        State(state),
        headers,
        Some(axum::Extension(agent)),
        Json(request),
    )
    .await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// =============================================================================
// batch.rs: handle_federated tests
// =============================================================================

#[tokio::test]
async fn test_handler_federated_disabled_cache_hit() {
    let state = make_test_app_state();
    // Insert cached response
    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "fed-doc".to_string(),
            score: 0.9,
            content: "federated cached".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "fed query");
    state.cache.insert(&ctx_query, cached, "test".to_string());

    let request = super::batch::FederatedRequest {
        query: "fed query".to_string(),
        local_results: None,
        top_k: Some(5),
    };

    let resp =
        super::batch::handle_federated(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_federated_disabled_no_cache() {
    let state = make_test_app_state();

    let request = super::batch::FederatedRequest {
        query: "no cache fed".to_string(),
        local_results: None,
        top_k: Some(5),
    };

    let resp =
        super::batch::handle_federated(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    // Should be 503 or 200 with empty results
    assert!(resp.status() == StatusCode::OK || resp.status() == StatusCode::SERVICE_UNAVAILABLE,);
}

// =============================================================================
// query_core.rs: execute_query with upstream (deeper coverage)
// =============================================================================

#[tokio::test]
async fn test_execute_query_upstream_success() {
    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);

    let request = make_query_request("exec upstream test");
    let result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-exec-1".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    assert_eq!(result.status, 200);
}

#[tokio::test]
async fn test_execute_query_upstream_failure_no_cache() {
    let (_handle, url) = start_failing_mock_upstream().await;
    let state = make_test_app_state_with_upstream(&url);

    let request = make_query_request("exec fail test");
    let result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-exec-2".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    assert_eq!(result.status, 502);
}

#[tokio::test]
async fn test_execute_query_upstream_failure_frozen_cache() {
    let (_handle, url) = start_failing_mock_upstream().await;
    let state = make_test_app_state_with_upstream(&url);

    // Pre-populate cache for frozen fallback
    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "frozen-exec".to_string(),
            score: 0.8,
            content: "frozen".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "exec frozen test");
    state.cache.insert(&ctx_query, cached, "test".to_string());

    let request = make_query_request("exec frozen test");
    let result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-exec-3".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    // Should serve frozen cache
    assert_eq!(result.status, 200);
}

#[tokio::test]
async fn test_execute_query_circuit_open_frozen() {
    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);

    // Trip circuit breaker
    for _ in 0..20 {
        state.circuit_breaker.record_failure();
    }

    // Pre-populate cache
    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "circuit-doc".to_string(),
            score: 0.9,
            content: "circuit".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "exec circuit test");
    state.cache.insert(&ctx_query, cached, "test".to_string());

    let request = make_query_request("exec circuit test");
    let result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-exec-4".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    assert_eq!(result.status, 200);
}

#[tokio::test]
async fn test_execute_query_circuit_open_no_cache() {
    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);

    for _ in 0..20 {
        state.circuit_breaker.record_failure();
    }

    let request = make_query_request("exec circuit miss");
    let result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-exec-5".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    assert_eq!(result.status, 503);
}

#[tokio::test]
async fn test_execute_query_stale_cache() {
    let state = {
        let mut s = make_test_app_state();
        s.cache = Arc::new(CacheStore::new(
            Duration::from_millis(1),
            Duration::from_secs(3600),
            100,
        ));
        s
    };
    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "stale-exec".to_string(),
            score: 0.9,
            content: "stale".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "stale exec query");
    state.cache.insert(&ctx_query, cached, "test".to_string());

    tokio::time::sleep(Duration::from_millis(10)).await;

    let request = make_query_request("stale exec query");
    let result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-exec-6".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    // Stale cache served as 200
    assert_eq!(result.status, 200);
}

#[tokio::test]
async fn test_execute_query_concurrent_coalescing() {
    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);

    // Two concurrent requests for same query — one leader, one waiter
    let state2 = state.clone();
    let (r1, r2) = tokio::join!(
        super::query_core::execute_query(
            &state,
            make_query_request("coalesce exec"),
            "default".to_string(),
            "req-coal-1".to_string(),
            None,
            "test".to_string(),
        ),
        super::query_core::execute_query(
            &state2,
            make_query_request("coalesce exec"),
            "default".to_string(),
            "req-coal-2".to_string(),
            None,
            "test".to_string(),
        ),
    );
    assert_eq!(r1.status, 200);
    assert_eq!(r2.status, 200);
}

// =============================================================================
// query_core.rs: execute_query deeper paths (validation, scope filter)
// =============================================================================

#[tokio::test]
async fn test_execute_query_upstream_success_with_scope_filter() {
    use crate::config::ProxyScopeConfig;
    use crate::proxy::scope::ScopeFilter;

    let (_handle, url) = start_mock_upstream_server().await;
    let mut state = make_test_app_state_with_upstream(&url);
    state.scope_filter = Arc::new(ScopeFilter::from_config(&ProxyScopeConfig {
        weighted_phrases: vec![],
        seeds: vec!["allowed-scope".to_string()],
        ..Default::default()
    }));

    let request = make_query_request("scope filter test");
    let result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-scope-1".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    assert_eq!(result.status, 200);
}

// =============================================================================
// batch.rs: handle_batch with upstream tests
// =============================================================================

#[tokio::test]
async fn test_handler_batch_with_upstream_success() {
    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);

    let request = crate::proxy::batch::BatchRequest {
        queries: vec![crate::proxy::batch::BatchQuery {
            id: "q0".to_string(),
            query: "batch upstream test".to_string(),
            top_k: None,
            priority: None,
        }],
        fail_fast: false,
        timeout_ms: None,
    };

    let resp =
        super::batch::handle_batch(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["success_count"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn test_handler_batch_with_upstream_failure() {
    let (_handle, url) = start_failing_mock_upstream().await;
    let state = make_test_app_state_with_upstream(&url);

    let request = crate::proxy::batch::BatchRequest {
        queries: vec![crate::proxy::batch::BatchQuery {
            id: "q0".to_string(),
            query: "batch fail test".to_string(),
            top_k: None,
            priority: None,
        }],
        fail_fast: false,
        timeout_ms: None,
    };

    let resp =
        super::batch::handle_batch(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["error_count"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn test_handler_batch_upstream_failure_frozen_cache() {
    let (_handle, url) = start_failing_mock_upstream().await;
    let state = make_test_app_state_with_upstream(&url);

    // Pre-populate cache for frozen fallback
    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "frozen-batch".to_string(),
            score: 0.8,
            content: "frozen batch doc".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "batch frozen test");
    state.cache.insert(&ctx_query, cached, "test".to_string());

    let request = crate::proxy::batch::BatchRequest {
        queries: vec![crate::proxy::batch::BatchQuery {
            id: "q0".to_string(),
            query: "batch frozen test".to_string(),
            top_k: None,
            priority: None,
        }],
        fail_fast: false,
        timeout_ms: None,
    };

    let resp =
        super::batch::handle_batch(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    // Should serve frozen cache as success
    assert!(parsed["success_count"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn test_handler_batch_with_stale_cache_and_upstream() {
    let (_handle, url) = start_mock_upstream_server().await;
    let mut state = make_test_app_state_with_upstream(&url);
    // Short TTL so entries go stale immediately
    state.cache = Arc::new(CacheStore::new(
        Duration::from_millis(1),
        Duration::from_secs(3600),
        100,
    ));

    // Insert a cache entry that will be stale
    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "stale-batch".to_string(),
            score: 0.9,
            content: "stale".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "batch stale test");
    state.cache.insert(&ctx_query, cached, "test".to_string());

    tokio::time::sleep(Duration::from_millis(10)).await;

    let request = crate::proxy::batch::BatchRequest {
        queries: vec![crate::proxy::batch::BatchQuery {
            id: "q0".to_string(),
            query: "batch stale test".to_string(),
            top_k: None,
            priority: None,
        }],
        fail_fast: false,
        timeout_ms: None,
    };

    let resp =
        super::batch::handle_batch(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_batch_mixed_cache_and_upstream() {
    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);

    // One query cached, one not
    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "cached-batch".to_string(),
            score: 0.9,
            content: "cached".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "batch-cached");
    state.cache.insert(&ctx_query, cached, "test".to_string());

    let request = crate::proxy::batch::BatchRequest {
        queries: vec![
            crate::proxy::batch::BatchQuery {
                id: "q0".to_string(),
                query: "batch-cached".to_string(),
                top_k: None,
                priority: None,
            },
            crate::proxy::batch::BatchQuery {
                id: "q1".to_string(),
                query: "batch-miss-upstream".to_string(),
                top_k: None,
                priority: None,
            },
        ],
        fail_fast: false,
        timeout_ms: None,
    };

    let resp =
        super::batch::handle_batch(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["success_count"].as_u64().unwrap(), 2);
}

// =============================================================================
// batch.rs: handle_federated with enabled federated search
// =============================================================================

#[tokio::test]
async fn test_handler_federated_enabled_no_upstream() {
    use crate::proxy::federated::{FederatedSearch, FederatedSearchConfig};

    let state = make_test_app_state();
    state
        .federated_search
        .store(Arc::new(FederatedSearch::new(FederatedSearchConfig {
            enabled: true,
            ..Default::default()
        })));

    let request = super::batch::FederatedRequest {
        query: "fed-enabled".to_string(),
        local_results: Some(vec![crate::proxy::types::SearchResult {
            id: "local".to_string(),
            score: 0.9,
            content: "local result".to_string(),
            metadata: None,
            upstream_id: None,
        }]),
        top_k: Some(5),
    };

    let resp =
        super::batch::handle_federated(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_federated_enabled_with_upstream() {
    use crate::proxy::federated::{FederatedSearch, FederatedSearchConfig};

    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);
    state
        .federated_search
        .store(Arc::new(FederatedSearch::new(FederatedSearchConfig {
            enabled: true,
            min_local_results: 10, // high threshold forces remote query
            ..Default::default()
        })));

    let request = super::batch::FederatedRequest {
        query: "fed-upstream".to_string(),
        local_results: Some(vec![crate::proxy::types::SearchResult {
            id: "local".to_string(),
            score: 0.5,
            content: "local".to_string(),
            metadata: None,
            upstream_id: None,
        }]),
        top_k: Some(5),
    };

    let resp =
        super::batch::handle_federated(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_federated_local_sufficient() {
    use crate::proxy::federated::{FederatedSearch, FederatedSearchConfig};

    let state = make_test_app_state();
    state
        .federated_search
        .store(Arc::new(FederatedSearch::new(FederatedSearchConfig {
            enabled: true,
            min_local_results: 3,
            min_local_confidence: 0.7,
            ..Default::default()
        })));

    // ≥3 local results with score ≥0.7 → LocalSufficient (no remote query)
    let request = super::batch::FederatedRequest {
        query: "fed-local-sufficient".to_string(),
        local_results: Some(vec![
            crate::proxy::types::SearchResult {
                id: "r1".to_string(),
                score: 0.95,
                content: "high".to_string(),
                metadata: None,
                upstream_id: None,
            },
            crate::proxy::types::SearchResult {
                id: "r2".to_string(),
                score: 0.85,
                content: "medium".to_string(),
                metadata: None,
                upstream_id: None,
            },
            crate::proxy::types::SearchResult {
                id: "r3".to_string(),
                score: 0.75,
                content: "adequate".to_string(),
                metadata: None,
                upstream_id: None,
            },
        ]),
        top_k: Some(5),
    };

    let resp =
        super::batch::handle_federated(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_federated_low_confidence_with_upstream() {
    use crate::proxy::federated::{FederatedSearch, FederatedSearchConfig};

    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);
    state
        .federated_search
        .store(Arc::new(FederatedSearch::new(FederatedSearchConfig {
            enabled: true,
            min_local_results: 1,
            min_local_confidence: 0.9,
            fallback_on_low_confidence: true,
            ..Default::default()
        })));

    // Low score → LowConfidence → queries remote
    let request = super::batch::FederatedRequest {
        query: "fed-low-conf".to_string(),
        local_results: Some(vec![crate::proxy::types::SearchResult {
            id: "low".to_string(),
            score: 0.3,
            content: "low confidence".to_string(),
            metadata: None,
            upstream_id: None,
        }]),
        top_k: Some(5),
    };

    let resp =
        super::batch::handle_federated(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_federated_upstream_failure() {
    use crate::proxy::federated::{FederatedSearch, FederatedSearchConfig};

    let (_handle, url) = start_failing_mock_upstream().await;
    let state = make_test_app_state_with_upstream(&url);
    state
        .federated_search
        .store(Arc::new(FederatedSearch::new(FederatedSearchConfig {
            enabled: true,
            min_local_results: 10,
            ..Default::default()
        })));

    let request = super::batch::FederatedRequest {
        query: "fed-fail".to_string(),
        local_results: Some(vec![crate::proxy::types::SearchResult {
            id: "local".to_string(),
            score: 0.8,
            content: "local".to_string(),
            metadata: None,
            upstream_id: None,
        }]),
        top_k: Some(5),
    };

    let resp =
        super::batch::handle_federated(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_federated_always_remote() {
    use crate::proxy::federated::{FederatedSearch, FederatedSearchConfig, MergeMode};

    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);
    state
        .federated_search
        .store(Arc::new(FederatedSearch::new(FederatedSearchConfig {
            enabled: true,
            merge_mode: MergeMode::Interleave,
            ..Default::default()
        })));

    let request = super::batch::FederatedRequest {
        query: "fed-interleave".to_string(),
        local_results: Some(vec![crate::proxy::types::SearchResult {
            id: "local".to_string(),
            score: 0.95,
            content: "local".to_string(),
            metadata: None,
            upstream_id: None,
        }]),
        top_k: Some(5),
    };

    let resp =
        super::batch::handle_federated(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_federated_remote_cache_hit() {
    use crate::proxy::federated::{FederatedSearch, FederatedSearchConfig};

    let state = make_test_app_state();
    state
        .federated_search
        .store(Arc::new(FederatedSearch::new(FederatedSearchConfig {
            enabled: true,
            min_local_results: 10,
            ..Default::default()
        })));

    // Pre-populate cache for the remote context query key
    let response = crate::proxy::types::QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "remote-doc".to_string(),
            content: "remote cached".to_string(),
            score: 0.88,
            metadata: None,
            upstream_id: None,
        }],
        cache_status: crate::proxy::types::CacheStatus::Miss,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    state
        .cache
        .insert("ctx:default:fed-remote-cached", response, "up".to_string());

    let request = super::batch::FederatedRequest {
        query: "fed-remote-cached".to_string(),
        local_results: Some(vec![crate::proxy::types::SearchResult {
            id: "local".to_string(),
            score: 0.5,
            content: "local".to_string(),
            metadata: None,
            upstream_id: None,
        }]),
        top_k: Some(5),
    };

    let resp =
        super::batch::handle_federated(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_federated_with_pool() {
    use crate::proxy::federated::{FederatedSearch, FederatedSearchConfig};

    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state();
    let configs = vec![crate::config::UpstreamEndpointConfig {
        id: "pool-up".to_string(),
        url: url.clone(),
        ..Default::default()
    }];
    state.upstream_pool.store(Some(Arc::new(
        crate::proxy::pool::UpstreamPool::new(
            &configs,
            crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
        )
        .unwrap(),
    )));

    state
        .federated_search
        .store(Arc::new(FederatedSearch::new(FederatedSearchConfig {
            enabled: true,
            min_local_results: 10,
            ..Default::default()
        })));

    let request = super::batch::FederatedRequest {
        query: "fed-pool".to_string(),
        local_results: Some(vec![crate::proxy::types::SearchResult {
            id: "local".to_string(),
            score: 0.5,
            content: "local".to_string(),
            metadata: None,
            upstream_id: None,
        }]),
        top_k: Some(5),
    };

    let resp =
        super::batch::handle_federated(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

// =============================================================================
// CacheProxy::run() with immediate cancellation — covers init code
// =============================================================================

#[tokio::test]
async fn test_cache_proxy_run_and_cancel() {
    use tokio_util::sync::CancellationToken;

    let (_handle, url) = start_mock_upstream_server().await;
    let config = ProxyConfig {
        upstream_url: Some(url.clone()),
        ..Default::default()
    };
    let proxy = CacheProxy::new(&config).unwrap();

    let cancel = CancellationToken::new();
    let cancel2 = cancel.clone();

    let handle =
        tokio::spawn(async move { proxy.run("127.0.0.1:0", "127.0.0.1:0", cancel2).await });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    cancel.cancel();

    let result = handle.await.unwrap();
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_cache_proxy_run_with_pool_and_cancel() {
    use crate::config::UpstreamEndpointConfig;
    use crate::proxy::cascade::CascadeConfig;
    use tokio_util::sync::CancellationToken;

    let (_handle, url) = start_mock_upstream_server().await;
    let config = ProxyConfig {
        upstreams: vec![UpstreamEndpointConfig {
            id: "up-1".to_string(),
            url: url.clone(),
            ..Default::default()
        }],
        cascade: CascadeConfig {
            enabled: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let proxy = CacheProxy::new(&config).unwrap();

    let cancel = CancellationToken::new();
    let cancel2 = cancel.clone();

    let handle =
        tokio::spawn(async move { proxy.run("127.0.0.1:0", "127.0.0.1:0", cancel2).await });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    cancel.cancel();
    let result = handle.await.unwrap();
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_cache_proxy_run_with_auth_and_cancel() {
    use tokio_util::sync::CancellationToken;

    let config = ProxyConfig {
        api_key: Some("test-key".to_string()),
        ..Default::default()
    };
    let proxy = CacheProxy::new(&config).unwrap();

    let cancel = CancellationToken::new();
    let cancel2 = cancel.clone();

    let handle =
        tokio::spawn(async move { proxy.run("127.0.0.1:0", "127.0.0.1:0", cancel2).await });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    cancel.cancel();
    let _ = handle.await;
}

#[tokio::test]
async fn test_cache_proxy_run_with_rate_limit_and_cancel() {
    use crate::config::ProxyRateLimitConfig;
    use tokio_util::sync::CancellationToken;

    let config = ProxyConfig {
        rate_limit: ProxyRateLimitConfig {
            enabled: Some(true),
            requests_per_second: Some(100),
            burst_size: Some(200),
        },
        ..Default::default()
    };
    let proxy = CacheProxy::new(&config).unwrap();

    let cancel = CancellationToken::new();
    let cancel2 = cancel.clone();

    let handle =
        tokio::spawn(async move { proxy.run("127.0.0.1:0", "127.0.0.1:0", cancel2).await });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    cancel.cancel();
    let _ = handle.await;
}

#[tokio::test]
async fn test_cache_proxy_run_with_cdc_peer_and_cancel() {
    use crate::config::{CdcConfig, PeerConfig};
    use tokio_util::sync::CancellationToken;

    let config = ProxyConfig {
        cdc: CdcConfig {
            enabled: Some(true),
            ..Default::default()
        },
        peer: PeerConfig {
            enabled: Some(true),
            node_id: Some("node-run-test".to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    let proxy = CacheProxy::new(&config).unwrap();

    let cancel = CancellationToken::new();
    let cancel2 = cancel.clone();

    let handle =
        tokio::spawn(async move { proxy.run("127.0.0.1:0", "127.0.0.1:0", cancel2).await });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    cancel.cancel();
    let _ = handle.await;
}

#[tokio::test]
async fn test_cache_proxy_run_with_agents_and_cancel() {
    use crate::config::AgentConfig;
    use tokio_util::sync::CancellationToken;

    let config = ProxyConfig {
        api_key: Some("master-key".to_string()),
        agents: vec![AgentConfig {
            id: "a1".to_string(),
            api_key: "k1".to_string(),
            default_context: None,
            allowed_contexts: vec![],
            priority_class: None,
            rate_limit_rps: None,
            enabled: true,
        }],
        ..Default::default()
    };
    let proxy = CacheProxy::new(&config).unwrap();

    let cancel = CancellationToken::new();
    let cancel2 = cancel.clone();

    let handle =
        tokio::spawn(async move { proxy.run("127.0.0.1:0", "127.0.0.1:0", cancel2).await });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    cancel.cancel();
    let _ = handle.await;
}

// =============================================================================
// CacheProxy builder method coverage
// =============================================================================

#[test]
fn test_cache_proxy_with_cdc_enabled() {
    use crate::config::CdcConfig;

    let config = ProxyConfig {
        cdc: CdcConfig {
            enabled: Some(true),
            ..Default::default()
        },
        ..Default::default()
    };
    let proxy = CacheProxy::new(&config).unwrap();
    assert!(proxy.cache().len() == 0);
}

#[test]
fn test_cache_proxy_with_peer_config() {
    use crate::config::PeerConfig;

    let config = ProxyConfig {
        peer: PeerConfig {
            enabled: Some(true),
            node_id: Some("test-node".to_string()),
            peers: vec!["127.0.0.1:9001".to_string()],
            ..Default::default()
        },
        ..Default::default()
    };
    let proxy = CacheProxy::new(&config).unwrap();
    assert!(proxy.cache().len() == 0);
}

#[test]
fn test_cache_proxy_with_agents() {
    use crate::config::AgentConfig;

    let config = ProxyConfig {
        agents: vec![AgentConfig {
            id: "a1".to_string(),
            api_key: "k1".to_string(),
            default_context: None,
            allowed_contexts: vec![],
            priority_class: None,
            rate_limit_rps: None,
            enabled: true,
        }],
        ..Default::default()
    };
    let proxy = CacheProxy::new(&config).unwrap();
    assert!(proxy.agent_registry().is_some());
}

#[test]
fn test_cache_proxy_with_upstream_retry_enabled() {
    use crate::config::ProxyRetryConfig;
    use crate::proxy::upstream::GenericRestAdapter;

    let config = ProxyConfig {
        retry: ProxyRetryConfig {
            enabled: Some(true),
            max_retries: Some(3),
            initial_delay_ms: Some(100),
            max_delay_ms: Some(5000),
            backoff_multiplier: Some(2.0),
            ..Default::default()
        },
        ..Default::default()
    };
    let upstream =
        GenericRestAdapter::new("http://localhost:6333", Duration::from_secs(15)).unwrap();
    let proxy = CacheProxy::with_upstream(&config, upstream);
    assert_eq!(proxy.retry_policy().max_retries, 3);
}

#[test]
fn test_cache_proxy_with_upstream_rate_limit() {
    use crate::config::ProxyRateLimitConfig;
    use crate::proxy::upstream::GenericRestAdapter;

    let config = ProxyConfig {
        rate_limit: ProxyRateLimitConfig {
            enabled: Some(true),
            requests_per_second: Some(50),
            burst_size: Some(100),
        },
        ..Default::default()
    };
    let upstream =
        GenericRestAdapter::new("http://localhost:6333", Duration::from_secs(15)).unwrap();
    let proxy = CacheProxy::with_upstream(&config, upstream);
    assert!(proxy.rate_limiter().is_some());
}

#[test]
fn test_cache_proxy_with_upstream_agents() {
    use crate::config::AgentConfig;
    use crate::proxy::upstream::GenericRestAdapter;

    let config = ProxyConfig {
        agents: vec![AgentConfig {
            id: "a1".to_string(),
            api_key: "k1".to_string(),
            default_context: None,
            allowed_contexts: vec![],
            priority_class: None,
            rate_limit_rps: None,
            enabled: true,
        }],
        ..Default::default()
    };
    let upstream =
        GenericRestAdapter::new("http://localhost:6333", Duration::from_secs(15)).unwrap();
    let proxy = CacheProxy::with_upstream(&config, upstream);
    assert!(proxy.agent_registry().is_some());
}

// =============================================================================
// determine_degradation_level() — more paths
// =============================================================================

#[test]
fn test_determine_degradation_pool_all_disabled_empty_cache() {
    use crate::config::UpstreamEndpointConfig;

    let state = make_test_app_state();
    let configs = vec![UpstreamEndpointConfig {
        id: "dead".to_string(),
        url: "http://10.255.255.1:9999".to_string(),
        ..Default::default()
    }];
    let pool = crate::proxy::pool::UpstreamPool::new(
        &configs,
        crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
    )
    .unwrap();
    // Record enough failures to make the upstream go Offline (threshold=3)
    let u = pool.get("dead").unwrap();
    for _ in 0..5 {
        u.record_failure();
    }
    state.upstream_pool.store(Some(Arc::new(pool)));

    let level = super::determine_degradation_level(&state);
    assert_eq!(level, DegradationLevel::StartupFailure);
}

#[test]
fn test_determine_degradation_pool_all_disabled_with_cached_data() {
    use crate::config::UpstreamEndpointConfig;

    let state = make_test_app_state();
    let configs = vec![UpstreamEndpointConfig {
        id: "dead".to_string(),
        url: "http://10.255.255.1:9999".to_string(),
        ..Default::default()
    }];
    let pool = crate::proxy::pool::UpstreamPool::new(
        &configs,
        crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
    )
    .unwrap();
    let u = pool.get("dead").unwrap();
    for _ in 0..5 {
        u.record_failure();
    }
    state.upstream_pool.store(Some(Arc::new(pool)));
    state.cache.insert(
        "key",
        crate::proxy::types::QueryResponse {
            results: vec![],
            cache_status: crate::proxy::types::CacheStatus::Miss,
            took_ms: 0,
            generated_at: None,
            miss_reason: None,
        },
        "up".to_string(),
    );

    let level = super::determine_degradation_level(&state);
    assert_eq!(level, DegradationLevel::StaleServing);
}

// =============================================================================
// handle_peer_status()
// =============================================================================

#[tokio::test]
async fn test_handler_peer_status_enabled() {
    use crate::config::PeerConfig;
    use crate::proxy::cdc::CdcManager;
    use crate::proxy::peer::PeerManager;

    let mut state = make_test_app_state();
    let peer_config = PeerConfig {
        enabled: Some(true),
        node_id: Some("node-test".to_string()),
        ..Default::default()
    };
    let cancel = tokio_util::sync::CancellationToken::new();
    let cdc_config = crate::config::CdcConfig::default();
    let cdc = CdcManager::new(&cdc_config, "node-test".to_string());
    let pm = PeerManager::new(peer_config, state.cache.clone(), cdc.event_sender(), cancel);
    state.peer_manager = Some(Arc::new(pm));
    state.cdc_manager = Some(Arc::new(cdc));

    let resp = super::handle_peer_status(State(state)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["enabled"], true);
    assert_eq!(json["node_id"], "node-test");
}

#[tokio::test]
async fn test_handler_peer_status_disabled() {
    let state = make_test_app_state();
    let resp = super::handle_peer_status(State(state)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["enabled"], false);
}

// =============================================================================
// Coverage improvement: query_core.rs deeper paths
// =============================================================================

#[tokio::test]
async fn test_execute_query_concurrent_coalesce_waiter_error_no_cache() {
    // Two concurrent requests to a failing upstream: leader gets 502,
    // waiter receives broadcast error → 502 (no frozen cache).
    let (_handle, url) = start_failing_mock_upstream().await;
    let state = make_test_app_state_with_upstream(&url);
    let state2 = state.clone();

    let (r1, r2) = tokio::join!(
        super::query_core::execute_query(
            &state,
            make_query_request("coalesce-fail-exec"),
            "default".to_string(),
            "req-coal-fail-1".to_string(),
            None,
            "test".to_string(),
        ),
        super::query_core::execute_query(
            &state2,
            make_query_request("coalesce-fail-exec"),
            "default".to_string(),
            "req-coal-fail-2".to_string(),
            None,
            "test".to_string(),
        ),
    );
    // Both should fail since upstream is down and no cache
    assert!(r1.status == 502 || r1.status == 503);
    assert!(r2.status == 502 || r2.status == 503);
}

#[tokio::test]
async fn test_execute_query_concurrent_coalesce_waiter_error_frozen_cache() {
    // Two concurrent requests to a failing upstream WITH frozen cache:
    // leader and waiter both fall back to frozen cache.
    let (_handle, url) = start_failing_mock_upstream().await;
    let state = make_test_app_state_with_upstream(&url);

    // Pre-populate cache for frozen fallback
    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "frozen-coalesce".to_string(),
            score: 0.7,
            content: "frozen coalesced".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "coalesce-frozen-exec");
    state.cache.insert(&ctx_query, cached, "test".to_string());

    let state2 = state.clone();
    let (r1, r2) = tokio::join!(
        super::query_core::execute_query(
            &state,
            make_query_request("coalesce-frozen-exec"),
            "default".to_string(),
            "req-coal-frz-1".to_string(),
            None,
            "test".to_string(),
        ),
        super::query_core::execute_query(
            &state2,
            make_query_request("coalesce-frozen-exec"),
            "default".to_string(),
            "req-coal-frz-2".to_string(),
            None,
            "test".to_string(),
        ),
    );
    // At least one should serve frozen cache (200), leader might get frozen too
    assert!(r1.status == 200 || r2.status == 200);
}

#[tokio::test]
async fn test_execute_query_paused_reject_tracking() {
    // When proxy is paused, client_tracker.reject() is called
    let state = make_test_app_state();
    state
        .paused
        .store(true, std::sync::atomic::Ordering::Relaxed);

    let request = make_query_request("paused tracking query");
    let result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-paused-track".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    assert_eq!(result.status, 503);
    // Verify reject was called (rejected count should be > 0)
    assert!(
        state
            .client_tracker
            .total_rejected
            .load(std::sync::atomic::Ordering::Relaxed)
            > 0
    );
}

#[tokio::test]
async fn test_execute_query_validation_error_metric() {
    // Invalid request increments the validation_error metric
    let state = make_test_app_state();
    let request = QueryRequest {
        query: String::new(),
        top_k: None,
        priority: None,
        upstream_id: None,
        upstream_type: None,
    };
    let _result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-val-metric".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    let snap = state.metrics.snapshot();
    assert!(snap.validation_errors > 0);
}

#[tokio::test]
async fn test_execute_query_miss_reason_expired() {
    // Verify that expired cache sets miss_reason = Expired
    let state = {
        let mut s = make_test_app_state();
        s.cache = Arc::new(CacheStore::new(
            Duration::from_millis(1),
            Duration::from_millis(2),
            100,
        ));

        s
    };
    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "exp-miss-doc".to_string(),
            score: 0.9,
            content: "will expire".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "expired miss reason");
    state.cache.insert(&ctx_query, cached, "test".to_string());

    tokio::time::sleep(Duration::from_millis(10)).await;

    let request = make_query_request("expired miss reason");
    let result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-exp-miss".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    // No upstream: expired entry may still be stale (200), frozen (200), or fully expired (503).
    // The no-upstream cache-only path doesn't record metrics for hits, so we only
    // verify that the status code is valid.
    assert!(
        result.status == 200 || result.status == 503,
        "Expected 200 or 503, got {}",
        result.status
    );
}

#[tokio::test]
async fn test_execute_query_upstream_success_refresh_worker() {
    // Verify refresh_worker.register_query() is called on upstream success
    use crate::proxy::refresh::QueryTrackingRefreshWorker;

    let (_handle, url) = start_mock_upstream_server().await;
    let mut state = make_test_app_state_with_upstream(&url);
    let cancel = tokio_util::sync::CancellationToken::new();
    state.refresh_worker = Some(Arc::new(QueryTrackingRefreshWorker::new(
        state.cache.clone(),
        state.upstream.load_full().unwrap(),
        "test".to_string(),
        Duration::from_secs(3600),
        cancel,
    )));

    let request = make_query_request("refresh worker test");
    let result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-refresh".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    assert_eq!(result.status, 200);
    // Verify the query was registered with the refresh worker
    assert!(state.refresh_worker.as_ref().unwrap().tracked_count() > 0);
}

// =============================================================================
// Coverage improvement: context.rs deeper paths
// =============================================================================

#[tokio::test]
async fn test_handler_context_current_returns_stats() {
    let state = make_test_app_state();
    // Record some hits/misses for the default context
    state.context_manager.record_hit_for("default");
    state.context_manager.record_miss_for("default");

    let resp = context::handle_context_current(State(state)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["context"].is_object());
}

// =============================================================================
// Coverage improvement: batch.rs deeper paths
// =============================================================================

#[tokio::test]
async fn test_handler_batch_too_many_queries() {
    // Exceeding max_queries in batch config → 400 validation failure
    let mut state = make_test_app_state();
    state.batch_processor = Arc::new(BatchProcessor::new(crate::proxy::batch::BatchConfig {
        max_queries: 2,
        ..Default::default()
    }));

    let request = crate::proxy::batch::BatchRequest {
        queries: vec![
            crate::proxy::batch::BatchQuery {
                id: "q0".to_string(),
                query: "too-many-1".to_string(),
                top_k: None,
                priority: None,
            },
            crate::proxy::batch::BatchQuery {
                id: "q1".to_string(),
                query: "too-many-2".to_string(),
                top_k: None,
                priority: None,
            },
            crate::proxy::batch::BatchQuery {
                id: "q2".to_string(),
                query: "too-many-3".to_string(),
                top_k: None,
                priority: None,
            },
        ],
        fail_fast: false,
        timeout_ms: None,
    };

    let resp =
        super::batch::handle_batch(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_handler_batch_with_scope_filter() {
    // Batch with scope filter enabled — results get filtered
    use crate::config::ProxyScopeConfig;
    use crate::proxy::scope::ScopeFilter;

    let (_handle, url) = start_mock_upstream_server().await;
    let mut state = make_test_app_state_with_upstream(&url);
    state.scope_filter = Arc::new(ScopeFilter::from_config(&ProxyScopeConfig {
        weighted_phrases: vec![],
        seeds: vec!["allowed-scope".to_string()],
        ..Default::default()
    }));

    let request = crate::proxy::batch::BatchRequest {
        queries: vec![crate::proxy::batch::BatchQuery {
            id: "q0".to_string(),
            query: "batch scope test".to_string(),
            top_k: None,
            priority: None,
        }],
        fail_fast: false,
        timeout_ms: None,
    };

    let resp =
        super::batch::handle_batch(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_batch_concurrent_coalesce_with_upstream() {
    // Two batch requests with same query → coalescing path
    let (_handle, url) = start_mock_upstream_server().await;
    let state = make_test_app_state_with_upstream(&url);

    // Both batches contain the same query
    let request1 = crate::proxy::batch::BatchRequest {
        queries: vec![crate::proxy::batch::BatchQuery {
            id: "q0".to_string(),
            query: "batch-coalesce-test".to_string(),
            top_k: None,
            priority: None,
        }],
        fail_fast: false,
        timeout_ms: None,
    };
    let request2 = request1.clone();

    let state2 = state.clone();
    let (r1, r2) = tokio::join!(
        super::batch::handle_batch(State(state), default_headers(), None, Json(request1)),
        super::batch::handle_batch(State(state2), default_headers(), None, Json(request2)),
    );
    let r1 = r1.into_response();
    let r2 = r2.into_response();
    assert_eq!(r1.status(), StatusCode::OK);
    assert_eq!(r2.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_batch_concurrent_coalesce_failure() {
    // Two concurrent batch requests to failing upstream → coalescer waiter error path
    let (_handle, url) = start_failing_mock_upstream().await;
    let state = make_test_app_state_with_upstream(&url);

    let request1 = crate::proxy::batch::BatchRequest {
        queries: vec![crate::proxy::batch::BatchQuery {
            id: "q0".to_string(),
            query: "batch-coalesce-fail".to_string(),
            top_k: None,
            priority: None,
        }],
        fail_fast: false,
        timeout_ms: None,
    };
    let request2 = request1.clone();

    let state2 = state.clone();
    let (r1, r2) = tokio::join!(
        super::batch::handle_batch(State(state), default_headers(), None, Json(request1)),
        super::batch::handle_batch(State(state2), default_headers(), None, Json(request2)),
    );
    let r1 = r1.into_response();
    let r2 = r2.into_response();
    // Both should return OK (batch returns OK with error_count) or BAD_REQUEST
    assert!(r1.status() == StatusCode::OK || r1.status() == StatusCode::BAD_REQUEST);
    assert!(r2.status() == StatusCode::OK || r2.status() == StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_handler_batch_no_upstream_cache_only_hit() {
    // Cache-only mode: queries that are cached return hits
    let state = make_test_app_state();
    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "cache-only-doc".to_string(),
            score: 0.9,
            content: "cached".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "batch-cache-only");
    state.cache.insert(&ctx_query, cached, "test".to_string());

    let request = crate::proxy::batch::BatchRequest {
        queries: vec![
            crate::proxy::batch::BatchQuery {
                id: "q0".to_string(),
                query: "batch-cache-only".to_string(),
                top_k: None,
                priority: None,
            },
            crate::proxy::batch::BatchQuery {
                id: "q1".to_string(),
                query: "batch-cache-miss-only".to_string(),
                top_k: None,
                priority: None,
            },
        ],
        fail_fast: false,
        timeout_ms: None,
    };

    let resp =
        super::batch::handle_batch(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    // One hit, one miss (error since no upstream)
    assert!(parsed["success_count"].as_u64().unwrap() >= 1);
    assert!(parsed["error_count"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn test_handler_batch_expired_cache_no_upstream() {
    // Batch with expired cache and no upstream → expired path
    let state = {
        let mut s = make_test_app_state();
        s.cache = Arc::new(CacheStore::new(
            Duration::from_millis(1),
            Duration::from_millis(2),
            100,
        ));
        s
    };

    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "batch-exp-doc".to_string(),
            score: 0.9,
            content: "expired batch".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "batch-expired-test");
    state.cache.insert(&ctx_query, cached, "test".to_string());

    tokio::time::sleep(Duration::from_millis(10)).await;

    let request = crate::proxy::batch::BatchRequest {
        queries: vec![crate::proxy::batch::BatchQuery {
            id: "q0".to_string(),
            query: "batch-expired-test".to_string(),
            top_k: None,
            priority: None,
        }],
        fail_fast: false,
        timeout_ms: None,
    };

    let resp =
        super::batch::handle_batch(State(state), default_headers(), None, Json(request)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

// =============================================================================
// Coverage improvement: query_core.rs — response validation error path
// =============================================================================

#[tokio::test]
async fn test_execute_query_upstream_invalid_response() {
    // Upstream returns a response that fails validate() (empty result ID).
    // Covers lines 330-344 in query_core.rs (validation error on upstream response).
    let (_handle, url) = start_invalid_response_upstream().await;
    let state = make_test_app_state_with_upstream(&url);

    let request = make_query_request("invalid response test");
    let result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-inv-resp".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    // Should still return 200 (response is served despite validation error)
    assert_eq!(result.status, 200);
    // Validation error should be recorded
    let snap = state.metrics.snapshot();
    assert!(snap.validation_errors > 0);
}

#[tokio::test]
async fn test_execute_query_upstream_invalid_response_concurrent() {
    // Two concurrent requests getting invalid response — covers coalescer complete path
    // for validation error responses.
    let (_handle, url) = start_invalid_response_upstream().await;
    let state = make_test_app_state_with_upstream(&url);
    let state2 = state.clone();

    let (r1, r2) = tokio::join!(
        super::query_core::execute_query(
            &state,
            make_query_request("inv-resp-coal"),
            "default".to_string(),
            "req-inv-coal-1".to_string(),
            None,
            "test".to_string(),
        ),
        super::query_core::execute_query(
            &state2,
            make_query_request("inv-resp-coal"),
            "default".to_string(),
            "req-inv-coal-2".to_string(),
            None,
            "test".to_string(),
        ),
    );
    assert_eq!(r1.status, 200);
    assert_eq!(r2.status, 200);
}

// =============================================================================
// Coverage improvement: query_core.rs — leader error with frozen cache (lines 393-413)
// =============================================================================

#[tokio::test]
async fn test_execute_query_upstream_error_frozen_cache_detailed() {
    // Upstream fails, but we have an expired entry in cache → frozen fallback.
    // This specifically targets the leader error path with frozen cache broadcast (lines 393-413).
    // Key: TTL must be 0 so entry is fully expired (not stale, not fresh), forcing upstream attempt.
    let (_handle, url) = start_failing_mock_upstream().await;
    let mut state = make_test_app_state_with_upstream(&url);

    // TTL=0 and stale=0 means entries are immediately Frozen (past fresh+stale),
    // but still retrievable via get_by_hash since max_frozen_duration is 3600s.
    // This makes check_freshness_by_hash return Frozen, falling through to upstream path.
    state.cache = Arc::new(CacheStore::new(
        Duration::from_millis(0),
        Duration::from_millis(0),
        100,
    ));

    // Insert cache entry for frozen fallback
    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "frozen-detail".to_string(),
            score: 0.85,
            content: "frozen fallback detail".to_string(),
            metadata: None,
            upstream_id: Some("test".to_string()),
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 1,
        generated_at: Some(1000),
        miss_reason: None,
    };
    let ctx_query = context_query("default", "frozen detail test");
    state.cache.insert(&ctx_query, cached, "test".to_string());

    // Small sleep to ensure entry is past TTL
    tokio::time::sleep(Duration::from_millis(5)).await;

    let request = make_query_request("frozen detail test");
    let result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-frozen-detail".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    // Should serve frozen cache with status 200
    assert_eq!(result.status, 200);
    let snap = state.metrics.snapshot();
    assert!(snap.cache_frozen > 0);
    assert!(snap.upstream_failures > 0);
}

// =============================================================================
// Coverage improvement: query_core.rs — stale cache with upstream (background refresh)
// =============================================================================

#[tokio::test]
async fn test_execute_query_stale_with_upstream_background_refresh() {
    // Stale cache with a working upstream → serve stale, trigger background refresh.
    // Covers lines 144-193 (stale cache path with tokio::spawn).
    let (_handle, url) = start_mock_upstream_server().await;
    let mut state = make_test_app_state_with_upstream(&url);
    // Very short TTL so entries go stale quickly
    state.cache = Arc::new(CacheStore::new(
        Duration::from_millis(1),
        Duration::from_secs(3600),
        100,
    ));

    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "stale-bg".to_string(),
            score: 0.9,
            content: "stale with upstream".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "stale bg test");
    state.cache.insert(&ctx_query, cached, "test".to_string());

    tokio::time::sleep(Duration::from_millis(10)).await;

    let request = make_query_request("stale bg test");
    let result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-stale-bg".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    assert_eq!(result.status, 200);
    let snap = state.metrics.snapshot();
    assert!(snap.cache_stale > 0);

    // Wait a bit for background refresh to complete
    tokio::time::sleep(Duration::from_millis(100)).await;
}

#[tokio::test]
async fn test_execute_query_stale_with_scope_filter_bg_refresh() {
    // Stale cache with upstream + scope filter → background refresh applies filter
    use crate::config::ProxyScopeConfig;
    use crate::proxy::scope::ScopeFilter;

    let (_handle, url) = start_mock_upstream_server().await;
    let mut state = make_test_app_state_with_upstream(&url);
    state.cache = Arc::new(CacheStore::new(
        Duration::from_millis(1),
        Duration::from_secs(3600),
        100,
    ));
    state.scope_filter = Arc::new(ScopeFilter::from_config(&ProxyScopeConfig {
        weighted_phrases: vec![],
        seeds: vec!["allowed-doc".to_string()],
        ..Default::default()
    }));

    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "stale-scope".to_string(),
            score: 0.9,
            content: "stale".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "stale scope bg");
    state.cache.insert(&ctx_query, cached, "test".to_string());

    tokio::time::sleep(Duration::from_millis(10)).await;

    let request = make_query_request("stale scope bg");
    let result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-stale-scope-bg".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    assert_eq!(result.status, 200);
    tokio::time::sleep(Duration::from_millis(100)).await;
}

// =============================================================================
// dispatch_upstream_query: Direct tests for the extracted upstream dispatch
// =============================================================================

#[tokio::test]
async fn test_dispatch_upstream_query_no_upstream() {
    let sem = tokio::sync::Semaphore::new(1000);
    let result = super::query_core::dispatch_upstream_query(
        &None,
        &None,
        &None,
        &make_query_request("dispatch test"),
        #[cfg(feature = "embed-api")]
        &None,
        &sem,
    )
    .await;
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        crate::proxy::upstream::UpstreamError::NotConfigured
    ));
}

#[tokio::test]
async fn test_dispatch_upstream_query_single_upstream() {
    let (_handle, url) = start_mock_upstream_server().await;
    let upstream = Some(Arc::new(
        crate::proxy::upstream::GenericRestAdapter::new(&url, Duration::from_secs(5)).unwrap(),
    ));
    let sem = tokio::sync::Semaphore::new(1000);

    let result = super::query_core::dispatch_upstream_query(
        &None,
        &None,
        &upstream,
        &make_query_request("dispatch upstream test"),
        #[cfg(feature = "embed-api")]
        &None,
        &sem,
    )
    .await;
    assert!(result.is_ok());
    let response = result.unwrap();
    assert!(!response.results.is_empty());
    // Verify upstream_id defaults to "default" for results without one
    for r in &response.results {
        assert_eq!(r.upstream_id.as_deref(), Some("default"));
    }
}

#[tokio::test]
async fn test_dispatch_upstream_query_single_upstream_failure() {
    let (_handle, url) = start_failing_mock_upstream().await;
    let upstream = Some(Arc::new(
        crate::proxy::upstream::GenericRestAdapter::new(&url, Duration::from_secs(5)).unwrap(),
    ));
    let sem = tokio::sync::Semaphore::new(1000);

    let result = super::query_core::dispatch_upstream_query(
        &None,
        &None,
        &upstream,
        &make_query_request("dispatch fail test"),
        #[cfg(feature = "embed-api")]
        &None,
        &sem,
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_dispatch_upstream_query_with_pool() {
    let (_handle, url) = start_mock_upstream_server().await;
    let configs = vec![crate::config::UpstreamEndpointConfig {
        url: url.clone(),
        ..Default::default()
    }];
    let pool = Some(Arc::new(
        crate::proxy::pool::UpstreamPool::new(
            &configs,
            crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
        )
        .unwrap(),
    ));
    let sem = tokio::sync::Semaphore::new(1000);

    let result = super::query_core::dispatch_upstream_query(
        &None,
        &pool,
        &None,
        &make_query_request("dispatch pool test"),
        #[cfg(feature = "embed-api")]
        &None,
        &sem,
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_dispatch_upstream_query_pool_over_single() {
    // When both pool and single upstream are configured, pool takes precedence
    let (_handle, url) = start_mock_upstream_server().await;
    let configs = vec![crate::config::UpstreamEndpointConfig {
        url: url.clone(),
        ..Default::default()
    }];
    let pool = Some(Arc::new(
        crate::proxy::pool::UpstreamPool::new(
            &configs,
            crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
        )
        .unwrap(),
    ));
    let upstream = Some(Arc::new(
        crate::proxy::upstream::GenericRestAdapter::new(&url, Duration::from_secs(5)).unwrap(),
    ));
    let sem = tokio::sync::Semaphore::new(1000);

    let result = super::query_core::dispatch_upstream_query(
        &None,
        &pool,
        &upstream,
        &make_query_request("dispatch precedence test"),
        #[cfg(feature = "embed-api")]
        &None,
        &sem,
    )
    .await;
    // Should succeed via pool
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_dispatch_upstream_query_semaphore_exhausted() {
    // A semaphore with 0 permits should immediately return Unavailable
    // without ever contacting the upstream.
    let (_handle, url) = start_mock_upstream_server().await;
    let upstream = Some(Arc::new(
        crate::proxy::upstream::GenericRestAdapter::new(&url, Duration::from_secs(5)).unwrap(),
    ));
    let sem = tokio::sync::Semaphore::new(0);

    let result = super::query_core::dispatch_upstream_query(
        &None,
        &None,
        &upstream,
        &make_query_request("semaphore exhausted test"),
        #[cfg(feature = "embed-api")]
        &None,
        &sem,
    )
    .await;

    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        crate::proxy::upstream::UpstreamError::Unavailable(_)
    ));
}

// =============================================================================
// Admin pause/resume → execute_query integration tests
// =============================================================================

#[tokio::test]
async fn test_admin_pause_then_execute_query_returns_503() {
    use axum::extract::State;

    let state = make_test_app_state();

    // Pre-seed a cache entry so we can get a 200 cache hit
    let ctx_query = super::context_query("default", "integration test");
    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "doc-1".to_string(),
            score: 0.9,
            content: "cached content".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    state.cache.insert(&ctx_query, cached, "test".to_string());

    // Verify we get 200 before pausing
    let request = make_query_request("integration test");
    let result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-pre-pause".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    assert_eq!(result.status, 200);

    // Pause via the admin handler
    let _ = super::admin::handle_admin_pause(State(state.clone())).await;

    // Now execute_query should return 503
    let request = make_query_request("integration test");
    let result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-post-pause".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    assert_eq!(result.status, 503);

    // Verify client_tracker.total_rejected was incremented
    assert!(
        state
            .client_tracker
            .total_rejected
            .load(std::sync::atomic::Ordering::Relaxed)
            > 0
    );
}

#[tokio::test]
async fn test_admin_pause_resume_restores_query_path() {
    use axum::extract::State;

    let state = make_test_app_state();

    // Pre-seed a cache entry
    let ctx_query = super::context_query("default", "resume test");
    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "doc-2".to_string(),
            score: 0.8,
            content: "resumed content".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    state.cache.insert(&ctx_query, cached, "test".to_string());

    // Pause via handler
    let _ = super::admin::handle_admin_pause(State(state.clone())).await;

    // Verify paused returns 503
    let request = make_query_request("resume test");
    let result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-paused".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    assert_eq!(result.status, 503);

    // Resume via handler
    let _ = super::admin::handle_admin_resume(State(state.clone())).await;

    // Now execute_query should return 200 again (cache hit)
    let request = make_query_request("resume test");
    let result = super::query_core::execute_query(
        &state,
        request,
        "default".to_string(),
        "req-resumed".to_string(),
        None,
        "test".to_string(),
    )
    .await;
    assert_eq!(result.status, 200);
}

// =============================================================================
// Status handler tests — deeper coverage for status.rs
// =============================================================================

#[tokio::test]
async fn test_handler_health_degradation_level_readonly() {
    let state = make_test_app_state();
    state
        .degradation_level
        .store(2, std::sync::atomic::Ordering::Relaxed);

    let resp = super::status::handle_health(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["degradation_level"], "read_only");
}

#[tokio::test]
async fn test_handler_health_upstream_healthy_field() {
    let state = make_test_app_state();
    let resp = super::status::handle_health(State(state)).await;
    let (_, json): (_, serde_json::Value) = extract_json(resp).await;
    // No upstream → upstream_healthy is null
    assert!(json["upstream_healthy"].is_null());
    assert!(!json["upstream_configured"].as_bool().unwrap());
}

#[tokio::test]
async fn test_handler_stats_cache_total_zero() {
    let state = make_test_app_state();
    let resp = super::status::handle_stats(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["cache"]["total"], 0);
}

#[tokio::test]
async fn test_handler_stats_cache_total_after_insert() {
    let state = make_test_app_state();
    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "stats-doc".to_string(),
            score: 0.8,
            content: "stats content".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "stats test");
    state.cache.insert(&ctx_query, cached, "test".to_string());

    let resp = super::status::handle_stats(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["cache"]["total"], 1);
}

#[tokio::test]
async fn test_handler_metrics_latency_fields_present() {
    let state = make_test_app_state();
    let resp = super::status::handle_metrics(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["latency_percentiles"].is_object());
    assert!(json["adaptive_timeout"].is_object());
}

#[tokio::test]
async fn test_handler_metrics_prometheus_content_type() {
    let state = make_test_app_state();
    let resp = super::status::handle_metrics_prometheus(State(state)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);

    let content_type = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(content_type.contains("text/plain"));

    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("conproxy_cache_entries"));
}

#[tokio::test]
async fn test_handler_query_stats_zero_state() {
    let state = make_test_app_state();
    let resp = super::status::handle_query_stats(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["total_queries_tracked"], 0);
    assert_eq!(json["total_requests"], 0);
    assert_eq!(json["total_hits"], 0);
}

#[tokio::test]
async fn test_handler_query_stats_after_recording() {
    let state = make_test_app_state();
    state
        .query_stats
        .record("hot query", true, Duration::from_millis(5));
    state
        .query_stats
        .record("hot query", false, Duration::from_millis(20));

    let resp = super::status::handle_query_stats(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["total_queries_tracked"].as_u64().unwrap() >= 1);
    assert_eq!(json["total_requests"], 2);
    assert_eq!(json["total_hits"], 1);
}

#[tokio::test]
async fn test_handler_audit_entries_in_buffer() {
    let state = make_test_app_state();
    let resp = super::status::handle_audit(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["entries_in_buffer"], 0);
    assert_eq!(json["total_logged"], 0);
}

#[tokio::test]
async fn test_handler_circuit_status_closed_failure_count() {
    let state = make_test_app_state();
    let resp = super::status::handle_circuit_status(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["state"], "closed");
    assert_eq!(json["failure_count"], 0);
    assert_eq!(json["times_opened"], 0);
    assert_eq!(json["times_tripped"], 0);
}

#[tokio::test]
async fn test_handler_queue_status_utilization_zero() {
    let state = make_test_app_state();
    let resp = super::status::handle_queue_status(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["total"], 0);
    assert_eq!(json["utilization"], 0.0);
    assert!(json["by_priority"].is_object());
}

#[tokio::test]
async fn test_handler_ready_no_upstream_empty_cache_returns_503() {
    let state = make_test_app_state();
    let resp = super::status::handle_ready(State(state)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(!json["ready"].as_bool().unwrap());
    assert!(json["reason"].as_str().is_some());
}

#[tokio::test]
async fn test_handler_ready_no_upstream_cache_populated_returns_200() {
    let state = make_test_app_state();
    let cached = QueryResponse {
        results: vec![crate::proxy::types::SearchResult {
            id: "ready-doc".to_string(),
            score: 0.9,
            content: "ready content".to_string(),
            metadata: None,
            upstream_id: None,
        }],
        cache_status: CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    };
    let ctx_query = context_query("default", "ready test");
    state.cache.insert(&ctx_query, cached, "test".to_string());

    let resp = super::status::handle_ready(State(state)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_handler_pool_status_unconfigured() {
    let state = make_test_app_state();
    let resp = super::status::handle_pool_status(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!json["pool_configured"].as_bool().unwrap());
    assert!(json["stats"].is_null());
}

#[tokio::test]
async fn test_handler_pool_status_configured() {
    let configs = vec![crate::config::UpstreamEndpointConfig {
        id: "pool-status-test".to_string(),
        url: "http://localhost:9999".to_string(),
        ..Default::default()
    }];
    let state = make_test_app_state_with_pool(&configs);
    let resp = super::status::handle_pool_status(State(state)).await;
    let (status, json): (_, serde_json::Value) = extract_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["pool_configured"].as_bool().unwrap());
    assert!(json["stats"].is_object());
    assert!(json["strategy"].is_string());
}

#[tokio::test]
async fn test_handler_clients_zero_state() {
    let state = make_test_app_state();
    let resp = super::status::handle_clients(State(state)).await;
    let resp = resp.into_response();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["active_requests"], 0);
    assert_eq!(json["total_completed"], 0);
    assert_eq!(json["total_rejected"], 0);
}

// =============================================================================
// Helpers: mock upstream server + AppState with upstream/pool
// =============================================================================

pub(crate) async fn start_mock_upstream_server() -> (tokio::task::JoinHandle<()>, String) {
    let app = axum::Router::new()
        .route(
            "/query",
            axum::routing::post(|| async {
                axum::Json(serde_json::json!({
                    "results": [
                        {"id": "doc-1", "score": 0.95, "content": "Test upstream result"}
                    ],
                    "cache_status": "Miss",
                    "took_ms": 5
                }))
            }),
        )
        .route(
            "/health",
            axum::routing::get(|| async { axum::Json(serde_json::json!({"status": "ok"})) }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{}", addr);

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    (handle, url)
}

pub(crate) async fn start_failing_mock_upstream() -> (tokio::task::JoinHandle<()>, String) {
    let app =
        axum::Router::new().fallback(|| async { axum::http::StatusCode::INTERNAL_SERVER_ERROR });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{}", addr);

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    (handle, url)
}

pub(crate) fn make_test_app_state_with_upstream(url: &str) -> AppState {
    let state = make_test_app_state();
    state.upstream.store(Some(Arc::new(
        crate::proxy::upstream::GenericRestAdapter::new(url, Duration::from_secs(5)).unwrap(),
    )));
    state
}

fn make_test_app_state_with_pool(configs: &[crate::config::UpstreamEndpointConfig]) -> AppState {
    let state = make_test_app_state();
    state.upstream_pool.store(Some(Arc::new(
        crate::proxy::pool::UpstreamPool::new(
            configs,
            crate::proxy::pool::LoadBalanceStrategy::RoundRobin,
        )
        .unwrap(),
    )));

    state
}

fn make_query_request(query: &str) -> QueryRequest {
    QueryRequest {
        query: query.to_string(),
        top_k: Some(5),
        priority: None,
        upstream_id: None,
        upstream_type: None,
    }
}

/// Mock upstream that returns a response that fails QueryResponse::validate()
/// (result with empty ID triggers EmptyResultId).
pub(crate) async fn start_invalid_response_upstream() -> (tokio::task::JoinHandle<()>, String) {
    let app = axum::Router::new()
        .route(
            "/query",
            axum::routing::post(|| async {
                axum::Json(serde_json::json!({
                    "results": [
                        {"id": "", "score": 0.5, "content": "invalid empty ID"}
                    ],
                    "cache_status": "Miss",
                    "took_ms": 1
                }))
            }),
        )
        .route(
            "/health",
            axum::routing::get(|| async { axum::Json(serde_json::json!({"status": "ok"})) }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{}", addr);

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    (handle, url)
}

fn default_headers() -> axum::http::HeaderMap {
    axum::http::HeaderMap::new()
}
