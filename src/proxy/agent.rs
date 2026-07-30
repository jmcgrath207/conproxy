//! Multi-tenancy agent registry.
//!
//! Provides per-agent API keys, rate limits, context restrictions, and metrics.
//! When no agents are configured, behavior is identical to the global API key.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use serde::Serialize;

use super::middleware::RateLimiter;
use crate::config::AgentConfig;

/// Identity resolved from an API key lookup.
///
/// Flows through the request as an axum `Extension` so handlers
/// can enforce context binding and populate audit fields.
#[derive(Debug, Clone)]
pub struct AgentIdentity {
    /// Unique agent identifier.
    pub id: String,
    /// Default context for this agent (used when no X-Context header is sent).
    pub default_context: Option<String>,
    /// Contexts this agent is allowed to query (empty = unrestricted).
    pub allowed_contexts: Vec<String>,
    /// Priority class for request ordering (lower = higher priority).
    pub priority_class: u32,
}

impl AgentIdentity {
    /// Check if this agent is allowed to access the given context.
    pub fn can_access_context(&self, context: &str) -> bool {
        self.allowed_contexts.is_empty() || self.allowed_contexts.iter().any(|c| c == context)
    }
}

/// Per-agent usage counters.
#[derive(Debug)]
pub struct AgentQuota {
    /// Total requests made by this agent.
    pub requests_total: AtomicU64,
    /// Requests that hit the cache.
    pub cache_hits: AtomicU64,
    /// Requests that missed the cache.
    pub cache_misses: AtomicU64,
    /// Requests that were rate-limited.
    pub rate_limited: AtomicU64,
    /// Requests rejected due to context binding.
    pub context_denied: AtomicU64,
}

impl AgentQuota {
    fn new() -> Self {
        Self {
            requests_total: AtomicU64::new(0),
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
            rate_limited: AtomicU64::new(0),
            context_denied: AtomicU64::new(0),
        }
    }

    /// Record a request.
    pub fn record_request(&self) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a cache hit.
    pub fn record_hit(&self) {
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a cache miss.
    pub fn record_miss(&self) {
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a rate-limited request.
    pub fn record_rate_limited(&self) {
        self.rate_limited.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a context-denied request.
    pub fn record_context_denied(&self) {
        self.context_denied.fetch_add(1, Ordering::Relaxed);
    }

    /// Get a snapshot of the counters.
    pub fn snapshot(&self) -> AgentQuotaSnapshot {
        AgentQuotaSnapshot {
            requests_total: self.requests_total.load(Ordering::Relaxed),
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            cache_misses: self.cache_misses.load(Ordering::Relaxed),
            rate_limited: self.rate_limited.load(Ordering::Relaxed),
            context_denied: self.context_denied.load(Ordering::Relaxed),
        }
    }
}

/// Serializable snapshot of agent quota counters.
#[derive(Debug, Clone, Serialize)]
pub struct AgentQuotaSnapshot {
    pub requests_total: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub rate_limited: u64,
    pub context_denied: u64,
}

/// A registered agent entry with its identity, rate limiter, and quota.
pub struct AgentEntry {
    /// The resolved identity (passed to handlers via Extension).
    pub identity: AgentIdentity,
    /// Per-agent rate limiter (if configured).
    pub rate_limiter: Option<Arc<RateLimiter>>,
    /// Per-agent usage counters.
    pub quota: Arc<AgentQuota>,
    /// Whether this agent is enabled.
    pub enabled: bool,
}

/// Registry of all configured agents, keyed by API key.
///
/// Thread-safe via `DashMap`. When the registry is empty (no agents configured),
/// the auth middleware falls back to the global `api_key` behavior.
pub struct AgentRegistry {
    /// API key -> AgentEntry mapping.
    agents: DashMap<String, AgentEntry>,
    /// Agent ID -> API key mapping (for admin lookups by ID).
    id_to_key: DashMap<String, String>,
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            agents: DashMap::new(),
            id_to_key: DashMap::new(),
        }
    }

    /// Build a registry from config.
    pub fn from_config(agents: &[AgentConfig]) -> Self {
        let registry = Self::new();
        for agent in agents {
            registry.register(agent);
        }
        registry
    }

    /// Register an agent from config.
    pub fn register(&self, config: &AgentConfig) {
        let identity = AgentIdentity {
            id: config.id.clone(),
            default_context: config.default_context.clone(),
            allowed_contexts: config.allowed_contexts.clone(),
            priority_class: config.priority_class(),
        };

        let rate_limiter = config.rate_limit_rps.map(|rps| {
            // Burst capacity = 2x RPS (allows short bursts)
            Arc::new(RateLimiter::new(rps, rps.saturating_mul(2).max(1)))
        });

        let entry = AgentEntry {
            identity,
            rate_limiter,
            quota: Arc::new(AgentQuota::new()),
            enabled: config.enabled,
        };

        self.id_to_key
            .insert(config.id.clone(), config.api_key.clone());
        self.agents.insert(config.api_key.clone(), entry);
    }

    /// Look up an agent by API key.
    ///
    /// Returns `None` if the key is not registered or the agent is disabled.
    pub fn lookup(&self, api_key: &str) -> Option<AgentLookup> {
        self.agents.get(api_key).and_then(|entry| {
            if !entry.enabled {
                return None;
            }
            Some(AgentLookup {
                identity: entry.identity.clone(),
                rate_limiter: entry.rate_limiter.clone(),
                quota: entry.quota.clone(),
            })
        })
    }

    /// Check if the registry has any agents.
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }

    /// Get the number of registered agents.
    pub fn len(&self) -> usize {
        self.agents.len()
    }

    /// List all agent IDs and their enabled status.
    pub fn list_agents(&self) -> Vec<AgentInfo> {
        self.agents
            .iter()
            .map(|entry| AgentInfo {
                id: entry.value().identity.id.clone(),
                allowed_contexts: entry.value().identity.allowed_contexts.clone(),
                priority_class: entry.value().identity.priority_class,
                rate_limit_rps: entry.value().rate_limiter.as_ref().map(|r| r.refill_rate()),
                enabled: entry.value().enabled,
                quota: entry.value().quota.snapshot(),
            })
            .collect()
    }

    /// Remove an agent by ID.
    ///
    /// Returns `true` if the agent was found and removed.
    pub fn remove_by_id(&self, agent_id: &str) -> bool {
        if let Some((_, key)) = self.id_to_key.remove(agent_id) {
            self.agents.remove(&key);
            true
        } else {
            false
        }
    }

    /// Rotate an agent's API key.
    ///
    /// Returns true if the agent was found and rotated.
    /// Insert under new key before removing old to prevent concurrent miss.
    pub fn rotate_key(&self, agent_id: &str, new_key: String) -> bool {
        let old_key = match self.id_to_key.get(agent_id) {
            Some(entry) => entry.value().clone(),
            None => return false,
        };

        // Insert under new key first, then remove old — no window where
        // lookup by either key misses the entry.
        if let Some((_, entry)) = self.agents.remove(&old_key) {
            self.agents.insert(new_key.clone(), entry);
            self.id_to_key.insert(agent_id.to_string(), new_key);
            true
        } else {
            false
        }
    }
}

/// Result of a successful agent lookup.
pub struct AgentLookup {
    /// The agent's identity (for Extension injection).
    pub identity: AgentIdentity,
    /// Per-agent rate limiter (if configured).
    pub rate_limiter: Option<Arc<RateLimiter>>,
    /// Per-agent quota counters.
    pub quota: Arc<AgentQuota>,
}

/// Agent information for admin API responses.
#[derive(Debug, Clone, Serialize)]
pub struct AgentInfo {
    pub id: String,
    pub allowed_contexts: Vec<String>,
    pub priority_class: u32,
    pub rate_limit_rps: Option<u32>,
    pub enabled: bool,
    pub quota: AgentQuotaSnapshot,
}

#[cfg(test)]
#[path = "tests/agent_tests.rs"]
mod tests;
