#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;
use crate::config::AgentConfig;

fn test_agent_config(id: &str, key: &str) -> AgentConfig {
    AgentConfig {
        id: id.to_string(),
        api_key: key.to_string(),
        default_context: None,
        allowed_contexts: vec![],
        priority_class: None,
        rate_limit_rps: None,
        enabled: true,
    }
}

#[test]
fn test_registry_empty() {
    let registry = AgentRegistry::new();
    assert!(registry.is_empty());
    assert_eq!(registry.len(), 0);
}

#[test]
fn test_registry_from_config() {
    let agents = vec![
        test_agent_config("agent-1", "key-1"),
        test_agent_config("agent-2", "key-2"),
    ];
    let registry = AgentRegistry::from_config(&agents);
    assert_eq!(registry.len(), 2);
    assert!(!registry.is_empty());
}

#[test]
fn test_registry_lookup_success() {
    let registry = AgentRegistry::new();
    registry.register(&test_agent_config("agent-1", "key-1"));

    let lookup = registry.lookup("key-1");
    assert!(lookup.is_some());
    let lookup = lookup.unwrap();
    assert_eq!(lookup.identity.id, "agent-1");
}

#[test]
fn test_registry_lookup_missing() {
    let registry = AgentRegistry::new();
    registry.register(&test_agent_config("agent-1", "key-1"));
    assert!(registry.lookup("nonexistent").is_none());
}

#[test]
fn test_registry_lookup_disabled() {
    let registry = AgentRegistry::new();
    let mut config = test_agent_config("agent-1", "key-1");
    config.enabled = false;
    registry.register(&config);

    assert!(registry.lookup("key-1").is_none());
}

#[test]
fn test_registry_with_contexts() {
    let registry = AgentRegistry::new();
    let mut config = test_agent_config("agent-1", "key-1");
    config.allowed_contexts = vec!["ctx-a".to_string(), "ctx-b".to_string()];
    registry.register(&config);

    let lookup = registry.lookup("key-1").unwrap();
    assert!(lookup.identity.can_access_context("ctx-a"));
    assert!(lookup.identity.can_access_context("ctx-b"));
    assert!(!lookup.identity.can_access_context("ctx-c"));
}

#[test]
fn test_registry_unrestricted_contexts() {
    let registry = AgentRegistry::new();
    registry.register(&test_agent_config("agent-1", "key-1"));

    let lookup = registry.lookup("key-1").unwrap();
    assert!(lookup.identity.can_access_context("any-context"));
}

#[test]
fn test_registry_with_rate_limit() {
    let registry = AgentRegistry::new();
    let mut config = test_agent_config("agent-1", "key-1");
    config.rate_limit_rps = Some(50);
    registry.register(&config);

    let lookup = registry.lookup("key-1").unwrap();
    assert!(lookup.rate_limiter.is_some());
    let limiter = lookup.rate_limiter.unwrap();
    assert_eq!(limiter.refill_rate(), 50);
    // Burst should be 2x RPS
    assert_eq!(limiter.capacity(), 100);
}

#[test]
fn test_registry_no_rate_limit() {
    let registry = AgentRegistry::new();
    registry.register(&test_agent_config("agent-1", "key-1"));

    let lookup = registry.lookup("key-1").unwrap();
    assert!(lookup.rate_limiter.is_none());
}

#[test]
fn test_quota_counters() {
    let quota = AgentQuota::new();
    quota.record_request();
    quota.record_request();
    quota.record_hit();
    quota.record_miss();
    quota.record_rate_limited();
    quota.record_context_denied();

    let snap = quota.snapshot();
    assert_eq!(snap.requests_total, 2);
    assert_eq!(snap.cache_hits, 1);
    assert_eq!(snap.cache_misses, 1);
    assert_eq!(snap.rate_limited, 1);
    assert_eq!(snap.context_denied, 1);
}

#[test]
fn test_registry_remove_by_id() {
    let registry = AgentRegistry::new();
    registry.register(&test_agent_config("agent-1", "key-1"));
    registry.register(&test_agent_config("agent-2", "key-2"));

    assert!(registry.remove_by_id("agent-1"));
    assert_eq!(registry.len(), 1);
    assert!(registry.lookup("key-1").is_none());
    assert!(registry.lookup("key-2").is_some());
}

#[test]
fn test_registry_remove_nonexistent() {
    let registry = AgentRegistry::new();
    assert!(!registry.remove_by_id("ghost"));
}

#[test]
fn test_registry_rotate_key() {
    let registry = AgentRegistry::new();
    registry.register(&test_agent_config("agent-1", "old-key"));

    assert!(registry.rotate_key("agent-1", "new-key".to_string()));

    // Old key should not work
    assert!(registry.lookup("old-key").is_none());
    // New key should work
    let lookup = registry.lookup("new-key").unwrap();
    assert_eq!(lookup.identity.id, "agent-1");
}

#[test]
fn test_registry_rotate_key_nonexistent() {
    let registry = AgentRegistry::new();
    assert!(!registry.rotate_key("ghost", "new-key".to_string()));
}

#[test]
fn test_registry_list_agents() {
    let registry = AgentRegistry::new();
    let mut config1 = test_agent_config("agent-1", "key-1");
    config1.rate_limit_rps = Some(50);
    config1.priority_class = Some(2);
    registry.register(&config1);
    registry.register(&test_agent_config("agent-2", "key-2"));

    let agents = registry.list_agents();
    assert_eq!(agents.len(), 2);

    // Find agent-1
    let a1 = agents.iter().find(|a| a.id == "agent-1").unwrap();
    assert_eq!(a1.priority_class, 2);
    assert_eq!(a1.rate_limit_rps, Some(50));
    assert!(a1.enabled);
}

#[test]
fn test_agent_identity_priority() {
    let identity = AgentIdentity {
        id: "test".to_string(),
        default_context: None,
        allowed_contexts: vec![],
        priority_class: 5,
    };
    assert_eq!(identity.priority_class, 5);
}

#[test]
fn test_quota_shared_across_lookups() {
    let registry = AgentRegistry::new();
    registry.register(&test_agent_config("agent-1", "key-1"));

    // First lookup, record a request
    let lookup1 = registry.lookup("key-1").unwrap();
    lookup1.quota.record_request();

    // Second lookup should see the same counter
    let lookup2 = registry.lookup("key-1").unwrap();
    assert_eq!(lookup2.quota.snapshot().requests_total, 1);
}

#[test]
fn test_registry_with_default_context() {
    let registry = AgentRegistry::new();
    let mut config = test_agent_config("agent-1", "key-1");
    config.default_context = Some("my-project".to_string());
    registry.register(&config);

    let lookup = registry.lookup("key-1").unwrap();
    assert_eq!(
        lookup.identity.default_context,
        Some("my-project".to_string())
    );
}

#[test]
fn test_registry_without_default_context() {
    let registry = AgentRegistry::new();
    registry.register(&test_agent_config("agent-1", "key-1"));

    let lookup = registry.lookup("key-1").unwrap();
    assert_eq!(lookup.identity.default_context, None);
}
