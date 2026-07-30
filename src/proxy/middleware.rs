//! Middleware for authentication and rate limiting.
//!
//! Provides:
//! - API key authentication via `X-API-Key` header or `Authorization: Bearer` header
//! - Token bucket rate limiting
//! - Multi-tenancy: per-agent API keys with `AgentRegistry` lookup
//!
//! When agents are configured, the middleware resolves the API key to an
//! `AgentIdentity` and injects it as an axum `Extension`. Per-agent rate
//! limiting replaces the global rate limiter when available.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use parking_lot::Mutex;
use serde::Serialize;

use super::agent::{AgentIdentity, AgentRegistry};

/// Token bucket rate limiter.
///
/// Uses a simple token bucket algorithm where tokens are added at a fixed rate
/// up to a maximum burst capacity.
pub struct RateLimiter {
    /// Maximum tokens (burst capacity).
    capacity: u32,
    /// Tokens added per second.
    refill_rate: u32,
    /// Current token count (scaled by 1000 for sub-token precision).
    tokens: AtomicU64,
    /// Last refill timestamp.
    last_refill: Mutex<Instant>,
}

impl RateLimiter {
    /// Create a new rate limiter.
    ///
    /// # Arguments
    /// * `requests_per_second` - Maximum sustained request rate
    /// * `burst_size` - Maximum burst capacity
    pub fn new(requests_per_second: u32, burst_size: u32) -> Self {
        Self {
            capacity: burst_size,
            refill_rate: requests_per_second,
            tokens: AtomicU64::new((burst_size as u64).saturating_mul(1000)),
            last_refill: Mutex::new(Instant::now()),
        }
    }

    /// Try to acquire a token. Returns true if allowed, false if rate limited.
    pub fn try_acquire(&self) -> bool {
        self.refill();

        loop {
            let current = self.tokens.load(Ordering::Relaxed);
            if current < 1000 {
                return false; // Not enough tokens
            }

            let new_tokens = current.saturating_sub(1000);
            if self
                .tokens
                .compare_exchange(current, new_tokens, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return true;
            }
            // CAS failed, retry
        }
    }

    /// Refill tokens based on elapsed time.
    fn refill(&self) {
        let mut last_refill = self.last_refill.lock();
        let now = Instant::now();
        let elapsed = now.duration_since(*last_refill);

        if elapsed >= Duration::from_millis(1) {
            let tokens_to_add_f = elapsed.as_secs_f64() * self.refill_rate as f64 * 1000.0;
            let tokens_to_add = tokens_to_add_f as u64;
            let max_tokens = (self.capacity as u64).saturating_mul(1000);

            let current = self.tokens.load(Ordering::Relaxed);
            let new_tokens = current.saturating_add(tokens_to_add).min(max_tokens);
            self.tokens.store(new_tokens, Ordering::Relaxed);

            *last_refill = now;
        }
    }

    /// Get current token count (for metrics).
    pub fn available_tokens(&self) -> u32 {
        self.refill();
        (self.tokens.load(Ordering::Relaxed) / 1000) as u32
    }

    /// Get capacity.
    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    /// Get refill rate.
    pub fn refill_rate(&self) -> u32 {
        self.refill_rate
    }
}

/// Constant-time string comparison to prevent timing side-channel attacks.
fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result: u8 = 0;
    for (x, y) in a.bytes().zip(b.bytes()) {
        result |= x ^ y;
    }
    result == 0
}

/// Paths accessible without auth when `web_ui.enabled = true` (GET only).
const WEB_UI_ALLOWLIST: &[&str] = &[
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
];

/// Authentication configuration passed to middleware.
#[derive(Clone)]
pub struct AuthConfig {
    /// Expected API key (if None, auth is disabled).
    pub api_key: Option<String>,
    /// Agent registry for multi-tenancy (if None, global key only).
    pub agent_registry: Option<Arc<AgentRegistry>>,
    /// When true, GET requests to status paths bypass auth.
    pub web_ui_enabled: bool,
}

impl AuthConfig {
    /// Create auth config with an API key.
    pub fn with_key(key: String) -> Self {
        Self {
            api_key: Some(key),
            agent_registry: None,
            web_ui_enabled: false,
        }
    }

    /// Create auth config with no authentication.
    pub fn disabled() -> Self {
        Self {
            api_key: None,
            agent_registry: None,
            web_ui_enabled: false,
        }
    }

    /// Add an agent registry for multi-tenancy.
    pub fn with_registry(mut self, registry: Arc<AgentRegistry>) -> Self {
        self.agent_registry = Some(registry);
        self
    }

    /// Enable web UI auth bypass for GET status paths.
    pub fn with_web_ui(mut self) -> Self {
        self.web_ui_enabled = true;
        self
    }
}

/// Rate limit error response.
#[derive(Serialize)]
pub struct RateLimitResponse {
    pub error: &'static str,
    pub retry_after_ms: u64,
}

/// Authentication error response.
#[derive(Serialize)]
pub struct AuthErrorResponse {
    pub error: &'static str,
}

/// Extract API key from request headers.
///
/// Supports both `X-API-Key` header and `Authorization: Bearer <token>` header.
fn extract_api_key(request: &Request) -> Option<String> {
    // Try X-API-Key header first
    if let Some(key) = request.headers().get("x-api-key") {
        return key.to_str().ok().map(|s| s.to_string());
    }

    // Try Authorization: Bearer header
    if let Some(auth) = request.headers().get(header::AUTHORIZATION) {
        if let Ok(auth_str) = auth.to_str() {
            if let Some(token) = auth_str.strip_prefix("Bearer ") {
                return Some(token.to_string());
            }
        }
    }

    None
}

/// Check if a path matches `/contexts/{id}/stats` (any id).
fn is_context_stats_path(path: &str) -> bool {
    let trimmed = path.trim_start_matches('/').trim_end_matches('/');
    let parts: Vec<&str> = trimmed.split('/').collect();
    parts.len() == 3
        && parts.first().copied() == Some("contexts")
        && parts.get(2).copied() == Some("stats")
}

/// Check if a path is in the web UI allowlist.
pub fn is_web_ui_allowlisted(path: &str) -> bool {
    WEB_UI_ALLOWLIST.contains(&path) || is_context_stats_path(path)
}

/// Authentication middleware.
///
/// When an `AgentRegistry` is configured:
/// 1. Looks up the API key in the registry
/// 2. If found, injects `AgentIdentity` as an Extension and applies per-agent rate limiting
/// 3. If not found, falls back to global API key check
///
/// When no registry is configured, uses the global `api_key` for authentication.
///
/// When `web_ui_enabled` is true, GET requests to status allowlist paths
/// bypass auth entirely (read-only dashboard access).
pub async fn auth_middleware(
    State(config): State<AuthConfig>,
    mut request: Request,
    next: Next,
) -> Response {
    // Web UI bypass: GET requests to status paths skip auth when enabled
    if config.web_ui_enabled
        && request.method() == axum::http::Method::GET
        && is_web_ui_allowlisted(request.uri().path())
    {
        return next.run(request).await;
    }

    // If no auth configured at all (no global key, no agents), allow all
    let has_auth = config.api_key.is_some()
        || config
            .agent_registry
            .as_ref()
            .is_some_and(|r| !r.is_empty());

    if !has_auth {
        return next.run(request).await;
    }

    // Extract API key from request
    let provided_key = match extract_api_key(&request) {
        Some(key) => key,
        None => {
            let response = AuthErrorResponse {
                error: "API key required (provide via X-API-Key or Authorization: Bearer header)",
            };
            return (StatusCode::UNAUTHORIZED, axum::Json(response)).into_response();
        }
    };

    // Try agent registry first (if configured)
    if let Some(ref registry) = config.agent_registry {
        if !registry.is_empty() {
            if let Some(lookup) = registry.lookup(&provided_key) {
                // Agent found — apply per-agent rate limiting
                if let Some(ref limiter) = lookup.rate_limiter {
                    if !limiter.try_acquire() {
                        lookup.quota.record_rate_limited();
                        let response = RateLimitResponse {
                            error: "Rate limit exceeded",
                            retry_after_ms: 100,
                        };
                        return (StatusCode::TOO_MANY_REQUESTS, axum::Json(response))
                            .into_response();
                    }
                }

                // Record request and inject identity
                lookup.quota.record_request();
                request.extensions_mut().insert(lookup.identity);
                return next.run(request).await;
            }
            // Key not found in registry — fall through to global key check
        }
    }

    // Fall back to global API key check (constant-time comparison)
    match &config.api_key {
        Some(expected_key) if constant_time_eq(&provided_key, expected_key) => {
            // Valid global key, continue (no AgentIdentity injected)
            next.run(request).await
        }
        Some(_) => {
            // Neither agent key nor global key matched
            let response = AuthErrorResponse {
                error: "Invalid API key",
            };
            (StatusCode::UNAUTHORIZED, axum::Json(response)).into_response()
        }
        None => {
            // No global key configured but agents exist — key was not found in registry
            let response = AuthErrorResponse {
                error: "Invalid API key",
            };
            (StatusCode::UNAUTHORIZED, axum::Json(response)).into_response()
        }
    }
}

/// Rate limiting configuration passed to middleware.
#[derive(Clone)]
pub struct RateLimitConfig {
    /// Rate limiter instance.
    pub limiter: Option<Arc<RateLimiter>>,
}

impl RateLimitConfig {
    /// Create rate limit config with a limiter.
    pub fn with_limiter(limiter: Arc<RateLimiter>) -> Self {
        Self {
            limiter: Some(limiter),
        }
    }

    /// Create rate limit config with no limiting.
    pub fn disabled() -> Self {
        Self { limiter: None }
    }
}

/// Rate limiting middleware.
///
/// Uses token bucket algorithm for rate limiting.
/// Note: When agents are configured, per-agent rate limiting happens in
/// `auth_middleware` instead — this middleware provides the global fallback.
pub async fn rate_limit_middleware(
    State(config): State<RateLimitConfig>,
    request: Request,
    next: Next,
) -> Response {
    // If rate limiting is disabled, allow all requests
    let limiter = match &config.limiter {
        Some(l) => l,
        None => return next.run(request).await,
    };

    // Skip global rate limiting if this request already has an AgentIdentity
    // (per-agent rate limiting was already applied in auth_middleware)
    if request.extensions().get::<AgentIdentity>().is_some() {
        return next.run(request).await;
    }

    if limiter.try_acquire() {
        next.run(request).await
    } else {
        let response = RateLimitResponse {
            error: "Rate limit exceeded",
            retry_after_ms: 100, // Suggest retrying after 100ms
        };
        (StatusCode::TOO_MANY_REQUESTS, axum::Json(response)).into_response()
    }
}

#[cfg(test)]
#[path = "tests/middleware_tests.rs"]
mod tests;
