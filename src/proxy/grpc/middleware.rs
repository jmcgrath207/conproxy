//! gRPC middleware for auth and rate limiting.
//!
//! Provides a tonic interceptor for API key extraction and rate limiting.

use tonic::{Request, Status};

use crate::proxy::agent::AgentRegistry;
use crate::proxy::middleware::RateLimiter;
use std::sync::Arc;

/// gRPC interceptor configuration.
#[derive(Clone)]
pub struct GrpcInterceptorConfig {
    /// Agent registry for API key resolution.
    pub agent_registry: Option<Arc<AgentRegistry>>,
    /// Rate limiter (shared with HTTP).
    pub rate_limiter: Option<Arc<RateLimiter>>,
    /// Global API key (if agent registry is not used).
    pub api_key: Option<String>,
}

impl GrpcInterceptorConfig {
    /// Create a new interceptor config.
    pub fn new(
        agent_registry: Option<Arc<AgentRegistry>>,
        rate_limiter: Option<Arc<RateLimiter>>,
        api_key: Option<String>,
    ) -> Self {
        Self {
            agent_registry,
            rate_limiter,
            api_key,
        }
    }

    /// Create a disabled interceptor (no auth, no rate limit).
    pub fn disabled() -> Self {
        Self {
            agent_registry: None,
            rate_limiter: None,
            api_key: None,
        }
    }

    /// Check if auth is required.
    pub fn requires_auth(&self) -> bool {
        self.agent_registry.is_some() || self.api_key.is_some()
    }
}

/// Extract API key from gRPC metadata.
///
/// Supports both `x-api-key` metadata and `Authorization: Bearer <token>` metadata.
fn extract_api_key_from_metadata(metadata: &tonic::metadata::MetadataMap) -> Option<String> {
    // Try x-api-key first
    if let Some(key) = metadata.get("x-api-key") {
        return key.to_str().ok().map(|s| s.to_string());
    }
    // Try Authorization: Bearer
    if let Some(auth) = metadata.get("authorization") {
        if let Ok(auth_str) = auth.to_str() {
            if let Some(token) = auth_str.strip_prefix("Bearer ") {
                return Some(token.to_string());
            }
        }
    }
    None
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

/// Create a tonic interceptor function for auth + rate limiting.
///
/// This can be used as a tonic layer:
/// ```ignore
/// let interceptor = make_interceptor(config);
/// Server::builder()
///     .add_service(SearchServiceServer::with_interceptor(svc, interceptor))
/// ```
pub fn make_interceptor(
    config: GrpcInterceptorConfig,
) -> impl Fn(Request<()>) -> Result<Request<()>, Status> + Clone {
    move |req: Request<()>| -> Result<Request<()>, Status> {
        // Rate limiting check
        if let Some(ref limiter) = config.rate_limiter {
            if !limiter.try_acquire() {
                return Err(Status::resource_exhausted("Rate limit exceeded"));
            }
        }

        // Auth check
        if config.requires_auth() {
            let api_key = extract_api_key_from_metadata(req.metadata());

            match api_key {
                Some(key) => {
                    let mut authenticated = false;

                    // Try agent registry first
                    if let Some(ref registry) = config.agent_registry {
                        if registry.lookup(&key).is_some() {
                            authenticated = true;
                        }
                    }

                    // Fall back to global API key
                    if !authenticated {
                        if let Some(ref expected) = config.api_key {
                            if constant_time_eq(&key, expected) {
                                authenticated = true;
                            }
                        }
                    }

                    if !authenticated {
                        return Err(Status::unauthenticated("Invalid API key"));
                    }
                }
                None => {
                    // No key provided — auth is required, reject
                    return Err(Status::unauthenticated(
                        "API key required (provide via x-api-key or Authorization: Bearer)",
                    ));
                }
            }
        }

        Ok(req)
    }
}

/// Metadata key for peer shared-secret auth (plan 07).
pub const PEER_SECRET_METADATA: &str = "x-peer-secret";

/// Extract peer shared secret from gRPC metadata.
///
/// Prefers `x-peer-secret`, then `x-api-key` (same value accepted for simple clients).
fn extract_peer_secret_from_metadata(metadata: &tonic::metadata::MetadataMap) -> Option<String> {
    if let Some(key) = metadata.get(PEER_SECRET_METADATA) {
        return key.to_str().ok().map(|s| s.to_string());
    }
    if let Some(key) = metadata.get("x-api-key") {
        return key.to_str().ok().map(|s| s.to_string());
    }
    None
}

/// Interceptor that requires a peer shared secret (constant-time compare).
///
/// Used on PeerService + CdcService when `[proxy.peer] shared_secret` is set.
/// Default-off path continues to use [`make_interceptor`] without a secret.
pub fn make_peer_secret_interceptor(
    expected: String,
) -> impl Fn(Request<()>) -> Result<Request<()>, Status> + Clone {
    move |req: Request<()>| -> Result<Request<()>, Status> {
        match extract_peer_secret_from_metadata(req.metadata()) {
            Some(provided) if constant_time_eq(&provided, &expected) => Ok(req),
            Some(_) => Err(Status::unauthenticated("Invalid peer shared secret")),
            None => Err(Status::unauthenticated(
                "Peer shared secret required (provide via x-peer-secret or x-api-key)",
            )),
        }
    }
}

/// Insert peer shared secret into a tonic request's metadata.
pub fn insert_peer_secret_metadata<T>(mut req: Request<T>, secret: &str) -> Request<T> {
    if let Ok(val) = secret.parse() {
        req.metadata_mut().insert(PEER_SECRET_METADATA, val);
    }
    req
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic
)]
mod tests {
    use super::*;

    #[test]
    fn test_disabled_interceptor_passes() {
        let config = GrpcInterceptorConfig::disabled();
        let interceptor = make_interceptor(config);
        let req = Request::new(());
        assert!(interceptor(req).is_ok());
    }

    #[test]
    fn test_peer_secret_accepts_correct() {
        let interceptor = make_peer_secret_interceptor("peer-s3cret".into());
        let mut req = Request::new(());
        req.metadata_mut()
            .insert(PEER_SECRET_METADATA, "peer-s3cret".parse().unwrap());
        assert!(interceptor(req).is_ok());
    }

    #[test]
    fn test_peer_secret_rejects_missing() {
        let interceptor = make_peer_secret_interceptor("peer-s3cret".into());
        let req = Request::new(());
        let err = interceptor(req).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn test_peer_secret_rejects_wrong() {
        let interceptor = make_peer_secret_interceptor("peer-s3cret".into());
        let mut req = Request::new(());
        req.metadata_mut()
            .insert(PEER_SECRET_METADATA, "wrong".parse().unwrap());
        let err = interceptor(req).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn test_peer_secret_accepts_x_api_key() {
        let interceptor = make_peer_secret_interceptor("peer-s3cret".into());
        let mut req = Request::new(());
        req.metadata_mut()
            .insert("x-api-key", "peer-s3cret".parse().unwrap());
        assert!(interceptor(req).is_ok());
    }

    #[test]
    fn test_auth_required_no_key() {
        let config = GrpcInterceptorConfig::new(None, None, Some("secret".to_string()));
        let interceptor = make_interceptor(config);
        let req = Request::new(());
        let result = interceptor(req);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn test_auth_correct_key() {
        let config = GrpcInterceptorConfig::new(None, None, Some("secret".to_string()));
        let interceptor = make_interceptor(config);
        let mut req = Request::new(());
        req.metadata_mut()
            .insert("x-api-key", "secret".parse().unwrap());
        assert!(interceptor(req).is_ok());
    }

    #[test]
    fn test_auth_wrong_key() {
        let config = GrpcInterceptorConfig::new(None, None, Some("secret".to_string()));
        let interceptor = make_interceptor(config);
        let mut req = Request::new(());
        req.metadata_mut()
            .insert("x-api-key", "wrong".parse().unwrap());
        let result = interceptor(req);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn test_rate_limit_exhausted() {
        // Create a limiter with 0 capacity to simulate exhaustion
        let limiter = Arc::new(RateLimiter::new(0, 0));
        let config = GrpcInterceptorConfig::new(None, Some(limiter), None);
        let interceptor = make_interceptor(config);
        let req = Request::new(());
        let result = interceptor(req);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::ResourceExhausted);
    }

    #[test]
    fn test_no_auth_configured_passes_without_key() {
        // Backwards compat: when no api_key and no agent_registry,
        // requests without any key should pass through.
        let config = GrpcInterceptorConfig::new(None, None, None);
        assert!(!config.requires_auth());
        let interceptor = make_interceptor(config);
        let req = Request::new(());
        assert!(interceptor(req).is_ok());
    }

    #[test]
    fn test_interceptor_is_clone() {
        // The interceptor must be Clone since we share it across 6 services.
        let config = GrpcInterceptorConfig::new(None, None, Some("key".to_string()));
        let interceptor = make_interceptor(config);
        let interceptor2 = interceptor.clone();
        // Both clones should enforce auth
        let req = Request::new(());
        assert!(interceptor(req).is_err());
        let req2 = Request::new(());
        assert!(interceptor2(req2).is_err());
    }

    #[test]
    fn test_agent_registry_valid_key() {
        let registry = Arc::new(AgentRegistry::new());
        registry.register(&crate::config::AgentConfig {
            id: "agent-1".to_string(),
            api_key: "agent-secret".to_string(),
            default_context: None,
            allowed_contexts: vec![],
            priority_class: None,
            rate_limit_rps: None,
            enabled: true,
        });
        let config = GrpcInterceptorConfig::new(Some(registry), None, None);
        assert!(config.requires_auth());
        let interceptor = make_interceptor(config);

        let mut req = Request::new(());
        req.metadata_mut()
            .insert("x-api-key", "agent-secret".parse().unwrap());
        assert!(interceptor(req).is_ok());
    }

    #[test]
    fn test_agent_registry_invalid_key() {
        let registry = Arc::new(AgentRegistry::new());
        registry.register(&crate::config::AgentConfig {
            id: "agent-1".to_string(),
            api_key: "agent-secret".to_string(),
            default_context: None,
            allowed_contexts: vec![],
            priority_class: None,
            rate_limit_rps: None,
            enabled: true,
        });
        let config = GrpcInterceptorConfig::new(Some(registry), None, None);
        let interceptor = make_interceptor(config);

        let mut req = Request::new(());
        req.metadata_mut()
            .insert("x-api-key", "wrong-key".parse().unwrap());
        let result = interceptor(req);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn test_rate_limit_and_auth_combined() {
        // Rate limit should be checked before auth
        let limiter = Arc::new(RateLimiter::new(0, 0)); // exhausted
        let config = GrpcInterceptorConfig::new(None, Some(limiter), Some("key".to_string()));
        let interceptor = make_interceptor(config);

        let mut req = Request::new(());
        req.metadata_mut()
            .insert("x-api-key", "key".parse().unwrap());
        let result = interceptor(req);
        assert!(result.is_err());
        // Rate limit checked first, so should be ResourceExhausted not Unauthenticated
        assert_eq!(result.unwrap_err().code(), tonic::Code::ResourceExhausted);
    }
}
