#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

#[test]
fn test_rate_limiter_creation() {
    let limiter = RateLimiter::new(100, 50);
    assert_eq!(limiter.capacity(), 50);
    assert_eq!(limiter.refill_rate(), 100);
    assert_eq!(limiter.available_tokens(), 50);
}

#[test]
fn test_rate_limiter_acquire() {
    // Use low refill rate so no tokens regenerate during the loop
    // (tarpaulin instrumentation can add enough delay for fast refills to trigger)
    let limiter = RateLimiter::new(1, 10);

    // Should be able to acquire burst capacity
    for _ in 0..10 {
        assert!(limiter.try_acquire());
    }

    // Should be rate limited now
    assert!(!limiter.try_acquire());
}

#[test]
fn test_rate_limiter_refill() {
    let limiter = RateLimiter::new(1000, 10);

    // Exhaust tokens
    for _ in 0..10 {
        limiter.try_acquire();
    }
    assert!(!limiter.try_acquire());

    // Wait for refill
    std::thread::sleep(Duration::from_millis(15));

    // Should have tokens again
    assert!(limiter.try_acquire());
}

#[test]
fn test_auth_config_creation() {
    let config = AuthConfig::with_key("test-key".to_string());
    assert_eq!(config.api_key.as_deref(), Some("test-key"));
    assert!(!config.web_ui_enabled);

    let disabled = AuthConfig::disabled();
    assert!(disabled.api_key.is_none());
    assert!(!disabled.web_ui_enabled);
}

#[test]
fn test_auth_config_with_web_ui() {
    let config = AuthConfig::with_key("test-key".to_string()).with_web_ui();
    assert!(config.web_ui_enabled);
}

#[test]
fn test_is_web_ui_allowlisted() {
    // Allowlisted paths
    assert!(is_web_ui_allowlisted("/stats"));
    assert!(is_web_ui_allowlisted("/stats/queries"));
    assert!(is_web_ui_allowlisted("/metrics"));
    assert!(is_web_ui_allowlisted("/audit"));
    assert!(is_web_ui_allowlisted("/circuit"));
    assert!(is_web_ui_allowlisted("/queue"));
    assert!(is_web_ui_allowlisted("/clients"));
    assert!(is_web_ui_allowlisted("/cache/integrity"));
    assert!(is_web_ui_allowlisted("/cache/upstreams"));
    assert!(is_web_ui_allowlisted("/contexts"));
    assert!(is_web_ui_allowlisted("/contexts/current"));
    assert!(is_web_ui_allowlisted("/contexts/my-ctx/stats"));

    // Not allowlisted
    assert!(!is_web_ui_allowlisted("/health"));
    assert!(!is_web_ui_allowlisted("/ready"));
    assert!(!is_web_ui_allowlisted("/pool"));
    assert!(!is_web_ui_allowlisted("/query"));
    assert!(!is_web_ui_allowlisted("/batch"));
    assert!(!is_web_ui_allowlisted("/cache/clear"));
    assert!(!is_web_ui_allowlisted("/cache/warmup"));
    assert!(!is_web_ui_allowlisted("/admin/reload"));
    assert!(!is_web_ui_allowlisted("/contexts/switch"));
    assert!(!is_web_ui_allowlisted("/contexts/create"));
}

#[test]
fn test_rate_limit_config_creation() {
    let limiter = Arc::new(RateLimiter::new(100, 50));
    let config = RateLimitConfig::with_limiter(limiter);
    assert!(config.limiter.is_some());

    let disabled = RateLimitConfig::disabled();
    assert!(disabled.limiter.is_none());
}

// === Additional Security Tests ===

#[test]
fn test_rate_limiter_burst_protection() {
    // Test that burst capacity is respected
    let limiter = RateLimiter::new(10, 5); // 10 RPS, burst of 5

    // Burst should allow 5 immediate requests
    let mut successes = 0;
    for _ in 0..10 {
        if limiter.try_acquire() {
            successes += 1;
        }
    }
    // Should get exactly burst capacity (5)
    assert_eq!(successes, 5);
}

#[test]
fn test_rate_limiter_concurrent_access() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    // Use low refill rate (1 RPS) so no tokens regenerate during the test.
    // Tarpaulin instrumentation slows threads enough that fast refill rates
    // can cause spurious token regeneration.
    let limiter = Arc::new(RateLimiter::new(1, 100));
    let successes = Arc::new(AtomicUsize::new(0));

    let mut handles = vec![];

    // Spawn 10 threads each trying to acquire 20 tokens
    for _ in 0..10 {
        let limiter = Arc::clone(&limiter);
        let successes = Arc::clone(&successes);
        handles.push(thread::spawn(move || {
            for _ in 0..20 {
                if limiter.try_acquire() {
                    successes.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    // Total successes should equal burst capacity (100), plus at most 1-2
    // tokens from the 1 RPS refill if the test takes over a second
    let total = successes.load(Ordering::Relaxed);
    assert!(
        total <= 105,
        "Should not significantly exceed burst capacity: {}",
        total
    );
    assert!(total >= 95, "Should use most of burst capacity: {}", total);
}

#[test]
fn test_rate_limiter_recovery_after_exhaust() {
    let limiter = RateLimiter::new(100, 5);

    // Exhaust all tokens (at rate 100, some may refill under heavy instrumentation)
    for _ in 0..10 {
        limiter.try_acquire();
    }

    // Wait for refill (100ms = ~10 tokens at 100 RPS, well above burst 5)
    std::thread::sleep(Duration::from_millis(100));

    // Should have recovered tokens
    assert!(limiter.try_acquire(), "Should have recovered tokens");
}

#[test]
fn test_rate_limiter_tokens_capped_at_capacity() {
    let limiter = RateLimiter::new(1000, 10);

    // Wait a long time - tokens should not exceed capacity
    std::thread::sleep(Duration::from_millis(100));

    // Force refill
    let available = limiter.available_tokens();
    assert!(
        available <= 10,
        "Tokens should be capped at capacity: {}",
        available
    );
}

#[test]
fn test_auth_config_multiple_keys() {
    // Test with multiple API keys
    let config = AuthConfig {
        api_key: Some("key1".to_string()),
        agent_registry: None,
        web_ui_enabled: false,
    };
    assert!(config.api_key.is_some());
}

#[test]
fn test_rate_limiter_zero_capacity() {
    // Edge case: zero capacity should block all requests
    let limiter = RateLimiter::new(100, 0);
    assert!(!limiter.try_acquire(), "Zero capacity should block all");
}

#[test]
fn test_rate_limiter_high_refill_rate() {
    // Test high refill rate
    let limiter = RateLimiter::new(10000, 100);

    // Exhaust tokens
    for _ in 0..100 {
        limiter.try_acquire();
    }

    // With 10000 RPS, should refill quickly
    std::thread::sleep(Duration::from_millis(20));
    assert!(
        limiter.try_acquire(),
        "High refill rate should recover quickly"
    );
}

// === extract_api_key tests ===

fn build_request_with_header(name: &str, value: &str) -> Request {
    let mut req = Request::builder()
        .method("POST")
        .uri("/query")
        .body(axum::body::Body::empty())
        .unwrap();
    req.headers_mut().insert(
        axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
        axum::http::HeaderValue::from_str(value).unwrap(),
    );
    req
}

#[test]
fn test_extract_api_key_from_x_api_key_header() {
    let req = build_request_with_header("x-api-key", "my-secret-key");
    let key = extract_api_key(&req);
    assert_eq!(key, Some("my-secret-key".to_string()));
}

#[test]
fn test_extract_api_key_from_bearer_header() {
    let req = build_request_with_header("authorization", "Bearer my-bearer-token");
    let key = extract_api_key(&req);
    assert_eq!(key, Some("my-bearer-token".to_string()));
}

#[test]
fn test_extract_api_key_missing() {
    let req = Request::builder()
        .method("POST")
        .uri("/query")
        .body(axum::body::Body::empty())
        .unwrap();
    let key = extract_api_key(&req);
    assert!(key.is_none());
}

#[test]
fn test_extract_api_key_x_api_key_takes_precedence() {
    let mut req = Request::builder()
        .method("POST")
        .uri("/query")
        .body(axum::body::Body::empty())
        .unwrap();
    req.headers_mut().insert(
        axum::http::HeaderName::from_bytes(b"x-api-key").unwrap(),
        axum::http::HeaderValue::from_str("key-from-x-api-key").unwrap(),
    );
    req.headers_mut().insert(
        header::AUTHORIZATION,
        axum::http::HeaderValue::from_str("Bearer key-from-bearer").unwrap(),
    );
    let key = extract_api_key(&req);
    assert_eq!(key, Some("key-from-x-api-key".to_string()));
}

#[test]
fn test_extract_api_key_non_bearer_auth() {
    let req = build_request_with_header("authorization", "Basic dXNlcjpwYXNz");
    let key = extract_api_key(&req);
    assert!(key.is_none());
}

#[test]
fn test_rate_limit_response_serialization() {
    let resp = RateLimitResponse {
        error: "Rate limit exceeded",
        retry_after_ms: 100,
    };
    let json = serde_json::to_value(&resp).unwrap();
    assert_eq!(json["error"], "Rate limit exceeded");
    assert_eq!(json["retry_after_ms"], 100);
}

#[test]
fn test_auth_error_response_serialization() {
    let resp = AuthErrorResponse {
        error: "API key required (provide via X-API-Key or Authorization: Bearer header)",
    };
    let json = serde_json::to_value(&resp).unwrap();
    assert!(json["error"].as_str().unwrap().contains("API key required"));
}

// === Axum middleware integration tests ===

mod middleware_integration {
    use super::*;
    use axum::routing::get;
    use axum::{middleware as mw, Router};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn handler() -> &'static str {
        "ok"
    }

    #[tokio::test]
    async fn test_auth_middleware_disabled_allows_all() {
        let config = AuthConfig::disabled();
        let app = Router::new()
            .route("/test", get(handler))
            .layer(mw::from_fn_with_state(config, auth_middleware));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_auth_middleware_valid_key() {
        let config = AuthConfig::with_key("secret-key".to_string());
        let app = Router::new()
            .route("/test", get(handler))
            .layer(mw::from_fn_with_state(config, auth_middleware));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header("x-api-key", "secret-key")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_auth_middleware_invalid_key() {
        let config = AuthConfig::with_key("secret-key".to_string());
        let app = Router::new()
            .route("/test", get(handler))
            .layer(mw::from_fn_with_state(config, auth_middleware));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header("x-api-key", "wrong-key")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "Invalid API key");
    }

    #[tokio::test]
    async fn test_auth_middleware_missing_key() {
        let config = AuthConfig::with_key("secret-key".to_string());
        let app = Router::new()
            .route("/test", get(handler))
            .layer(mw::from_fn_with_state(config, auth_middleware));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["error"].as_str().unwrap().contains("API key required"));
    }

    #[tokio::test]
    async fn test_auth_middleware_bearer_token() {
        let config = AuthConfig::with_key("my-token".to_string());
        let app = Router::new()
            .route("/test", get(handler))
            .layer(mw::from_fn_with_state(config, auth_middleware));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header("authorization", "Bearer my-token")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_rate_limit_middleware_disabled_allows_all() {
        let config = RateLimitConfig::disabled();
        let app = Router::new()
            .route("/test", get(handler))
            .layer(mw::from_fn_with_state(config, rate_limit_middleware));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_rate_limit_middleware_allows_within_capacity() {
        let limiter = Arc::new(RateLimiter::new(100, 10));
        let config = RateLimitConfig::with_limiter(limiter);
        let app = Router::new()
            .route("/test", get(handler))
            .layer(mw::from_fn_with_state(config, rate_limit_middleware));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_rate_limit_middleware_rejects_when_exhausted() {
        let limiter = Arc::new(RateLimiter::new(1, 0)); // zero capacity
        let config = RateLimitConfig::with_limiter(limiter);
        let app = Router::new()
            .route("/test", get(handler))
            .layer(mw::from_fn_with_state(config, rate_limit_middleware));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "Rate limit exceeded");
        assert_eq!(json["retry_after_ms"], 100);
    }

    // === Agent-aware middleware tests ===

    use crate::config::AgentConfig;
    use crate::proxy::agent::AgentRegistry;

    fn test_agent(id: &str, key: &str, rps: Option<u32>) -> AgentConfig {
        AgentConfig {
            id: id.to_string(),
            api_key: key.to_string(),
            default_context: None,
            allowed_contexts: vec![],
            priority_class: None,
            rate_limit_rps: rps,
            enabled: true,
        }
    }

    #[tokio::test]
    async fn test_agent_auth_valid_agent_key() {
        let registry = Arc::new(AgentRegistry::from_config(&[test_agent(
            "agent-1",
            "agent-key-1",
            None,
        )]));
        let config = AuthConfig::disabled().with_registry(registry);
        let app = Router::new()
            .route("/test", get(handler))
            .layer(mw::from_fn_with_state(config, auth_middleware));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header("x-api-key", "agent-key-1")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_agent_auth_invalid_key_no_global() {
        let registry = Arc::new(AgentRegistry::from_config(&[test_agent(
            "agent-1",
            "agent-key-1",
            None,
        )]));
        let config = AuthConfig::disabled().with_registry(registry);
        let app = Router::new()
            .route("/test", get(handler))
            .layer(mw::from_fn_with_state(config, auth_middleware));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header("x-api-key", "wrong-key")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_agent_auth_falls_back_to_global_key() {
        let registry = Arc::new(AgentRegistry::from_config(&[test_agent(
            "agent-1",
            "agent-key-1",
            None,
        )]));
        let config = AuthConfig::with_key("global-key".to_string()).with_registry(registry);
        let app = Router::new()
            .route("/test", get(handler))
            .layer(mw::from_fn_with_state(config, auth_middleware));

        // Agent key works
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header("x-api-key", "agent-key-1")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Global key also works
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header("x-api-key", "global-key")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_agent_per_agent_rate_limiting() {
        let registry = Arc::new(AgentRegistry::from_config(&[
            test_agent("agent-1", "agent-key-1", Some(1)), // 1 RPS, burst=2
        ]));
        let config = AuthConfig::disabled().with_registry(registry);
        let app = Router::new()
            .route("/test", get(handler))
            .layer(mw::from_fn_with_state(config, auth_middleware));

        // First request should succeed (within burst)
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header("x-api-key", "agent-key-1")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Second request should succeed (burst=2)
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header("x-api-key", "agent-key-1")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Third request should be rate limited (burst exhausted)
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header("x-api-key", "agent-key-1")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn test_agent_rate_limit_skips_global() {
        // Agent has per-agent rate limit, so global rate limiter should be skipped
        let registry = Arc::new(AgentRegistry::from_config(&[test_agent(
            "agent-1",
            "agent-key-1",
            Some(1000),
        )]));
        let auth_config = AuthConfig::disabled().with_registry(registry);

        // Global limiter with 0 capacity (blocks everything)
        let rate_config = RateLimitConfig::with_limiter(Arc::new(RateLimiter::new(1, 0)));

        let app = Router::new()
            .route("/test", get(handler))
            .layer(mw::from_fn_with_state(rate_config, rate_limit_middleware))
            .layer(mw::from_fn_with_state(auth_config, auth_middleware));

        // Agent should bypass global rate limiter
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header("x-api-key", "agent-key-1")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_agent_missing_key_with_agents() {
        let registry = Arc::new(AgentRegistry::from_config(&[test_agent(
            "agent-1",
            "agent-key-1",
            None,
        )]));
        let config = AuthConfig::disabled().with_registry(registry);
        let app = Router::new()
            .route("/test", get(handler))
            .layer(mw::from_fn_with_state(config, auth_middleware));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    // === Web UI auth bypass tests ===

    #[tokio::test]
    async fn test_web_ui_bypass_get_stats_without_key() {
        let config = AuthConfig::with_key("secret".to_string()).with_web_ui();
        let app = Router::new()
            .route("/stats", get(handler))
            .route("/query", get(handler))
            .layer(mw::from_fn_with_state(config, auth_middleware));

        // GET /stats should bypass auth
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/stats")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // GET /query should NOT bypass auth (not in allowlist)
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/query")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_web_ui_bypass_post_not_allowed() {
        use axum::http::Method;

        let config = AuthConfig::with_key("secret".to_string()).with_web_ui();
        let app = Router::new()
            .route("/stats", get(handler).post(handler))
            .layer(mw::from_fn_with_state(config, auth_middleware));

        // POST /stats should NOT bypass auth (only GET)
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/stats")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_web_ui_disabled_no_bypass() {
        let config = AuthConfig::with_key("secret".to_string()); // web_ui_enabled = false
        let app = Router::new()
            .route("/stats", get(handler))
            .layer(mw::from_fn_with_state(config, auth_middleware));

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/stats")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_web_ui_bypass_all_allowlisted_paths() {
        let config = AuthConfig::with_key("secret".to_string()).with_web_ui();
        let mut router = Router::new();
        for path in &[
            "/stats",
            "/stats/queries",
            "/metrics",
            "/audit",
            "/circuit",
            "/queue",
            "/clients",
            "/cache/integrity",
            "/cache/upstreams",
            "/contexts",
            "/contexts/current",
            "/contexts/x/stats",
        ] {
            router = router.route(path, get(handler));
        }
        let app = router.layer(mw::from_fn_with_state(config, auth_middleware));

        for path in &[
            "/stats",
            "/stats/queries",
            "/metrics",
            "/audit",
            "/circuit",
            "/queue",
            "/clients",
            "/cache/integrity",
            "/cache/upstreams",
            "/contexts",
            "/contexts/current",
            "/contexts/x/stats",
        ] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(*path)
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "expected OK for {path}, got {}",
                resp.status()
            );
        }
    }
}
