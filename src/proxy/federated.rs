//! Federated search combining local and remote results.
//!
//! Provides local-first search with remote fallback, allowing seamless
//! integration of local LanceDB results with upstream proxy results.

use std::collections::HashSet;

use serde::Serialize;

use super::types::{CacheStatus, QueryResponse, SearchResult};
use crate::config::FederatedConfig;

/// Source of a search result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ResultSource {
    /// Result came from local LanceDB.
    Local,
    /// Result came from remote upstream via proxy.
    Remote,
}

/// Search result with source attribution.
#[derive(Debug, Clone, Serialize)]
pub struct FederatedResult {
    /// The search result.
    #[serde(flatten)]
    pub result: SearchResult,
    /// Source of this result.
    pub source: ResultSource,
}

impl FederatedResult {
    /// Create a local result.
    pub fn local(result: SearchResult) -> Self {
        Self {
            result,
            source: ResultSource::Local,
        }
    }

    /// Create a remote result.
    pub fn remote(result: SearchResult) -> Self {
        Self {
            result,
            source: ResultSource::Remote,
        }
    }
}

/// Merge mode for combining local and remote results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MergeMode {
    /// Use local results only, fall back to remote if insufficient.
    #[default]
    LocalOnlyFallback,
    /// Prioritize local results, supplement with remote.
    LocalPriority,
    /// Prioritize remote results, supplement with local.
    RemotePriority,
    /// Interleave local and remote results by score.
    Interleave,
}

impl MergeMode {
    /// Parse merge mode from string.
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "local_only_fallback" | "localonly" | "fallback" => MergeMode::LocalOnlyFallback,
            "local_priority" | "local" => MergeMode::LocalPriority,
            "remote_priority" | "remote" => MergeMode::RemotePriority,
            "interleave" | "blend" | "merge" => MergeMode::Interleave,
            _ => MergeMode::LocalOnlyFallback,
        }
    }
}

/// Decision on whether to query remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FallbackDecision {
    /// Local results are sufficient, no remote query needed.
    LocalSufficient,
    /// Should query remote because local results are empty.
    EmptyLocal,
    /// Should query remote because local results are below minimum count.
    BelowMinResults,
    /// Should query remote because local confidence is too low.
    LowConfidence,
    /// Should always query remote (for interleave/remote_priority modes).
    AlwaysRemote,
}

/// Configuration for federated search.
#[derive(Debug, Clone)]
pub struct FederatedSearchConfig {
    /// Whether federated search is enabled.
    pub enabled: bool,
    /// Minimum number of local results to consider "sufficient".
    pub min_local_results: usize,
    /// Minimum confidence score for local results.
    pub min_local_confidence: f32,
    /// Whether to fall back on empty local results.
    pub fallback_on_empty: bool,
    /// Whether to fall back on low confidence.
    pub fallback_on_low_confidence: bool,
    /// How to merge local and remote results.
    pub merge_mode: MergeMode,
    /// Maximum results after merging.
    pub max_merged_results: usize,
}

impl Default for FederatedSearchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_local_results: 3,
            min_local_confidence: 0.7,
            fallback_on_empty: true,
            fallback_on_low_confidence: true,
            merge_mode: MergeMode::LocalOnlyFallback,
            max_merged_results: 10,
        }
    }
}

impl From<&FederatedConfig> for FederatedSearchConfig {
    fn from(config: &FederatedConfig) -> Self {
        Self {
            enabled: config.enabled(),
            min_local_results: config.min_local_results(),
            min_local_confidence: config.min_local_confidence(),
            fallback_on_empty: config.fallback_on_empty.unwrap_or(true),
            fallback_on_low_confidence: config.fallback_on_low_confidence.unwrap_or(true),
            merge_mode: MergeMode::parse(config.merge_mode()),
            max_merged_results: config.max_merged_results.unwrap_or(10),
        }
    }
}

/// Statistics about a federated search.
#[derive(Debug, Clone, Default, Serialize)]
pub struct FederatedStats {
    /// Number of local results before merging.
    pub local_count: usize,
    /// Number of remote results before merging.
    pub remote_count: usize,
    /// Number of results after merging.
    pub merged_count: usize,
    /// Number of duplicates removed.
    pub duplicates_removed: usize,
    /// Whether remote was queried.
    pub remote_queried: bool,
    /// Why remote was queried (if applicable).
    pub fallback_reason: Option<String>,
    /// Local latency in milliseconds.
    pub local_latency_ms: u64,
    /// Remote latency in milliseconds (if queried).
    pub remote_latency_ms: Option<u64>,
}

/// Federated search orchestrator.
pub struct FederatedSearch {
    config: FederatedSearchConfig,
}

impl FederatedSearch {
    /// Create a new federated search with the given configuration.
    pub fn new(config: FederatedSearchConfig) -> Self {
        Self { config }
    }

    /// Create from a FederatedConfig reference.
    pub fn from_config(config: &FederatedConfig) -> Self {
        Self::new(FederatedSearchConfig::from(config))
    }

    /// Check if federated search is enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Decide whether to query remote based on local results.
    pub fn should_query_remote(&self, local_results: &[SearchResult]) -> FallbackDecision {
        // Always query remote for interleave/remote_priority modes
        if matches!(
            self.config.merge_mode,
            MergeMode::Interleave | MergeMode::RemotePriority
        ) {
            return FallbackDecision::AlwaysRemote;
        }

        // Check if local is empty
        if local_results.is_empty() {
            if self.config.fallback_on_empty {
                return FallbackDecision::EmptyLocal;
            }
            return FallbackDecision::LocalSufficient;
        }

        // Check if below minimum results
        if local_results.len() < self.config.min_local_results {
            return FallbackDecision::BelowMinResults;
        }

        // Check confidence of top result
        if self.config.fallback_on_low_confidence {
            let max_score = local_results.iter().map(|r| r.score).fold(0.0f32, f32::max);

            if max_score < self.config.min_local_confidence {
                return FallbackDecision::LowConfidence;
            }
        }

        FallbackDecision::LocalSufficient
    }

    /// Merge local and remote results according to the configured mode.
    pub fn merge(
        &self,
        local_results: Vec<SearchResult>,
        remote_results: Vec<SearchResult>,
    ) -> (Vec<FederatedResult>, FederatedStats) {
        let local_count = local_results.len();
        let remote_count = remote_results.len();

        let (merged, duplicates_removed) = match self.config.merge_mode {
            MergeMode::LocalOnlyFallback => {
                if local_results.is_empty() {
                    (
                        remote_results
                            .into_iter()
                            .map(FederatedResult::remote)
                            .collect(),
                        0,
                    )
                } else {
                    (
                        local_results
                            .into_iter()
                            .map(FederatedResult::local)
                            .collect(),
                        0,
                    )
                }
            }
            MergeMode::LocalPriority => self.merge_priority(local_results, remote_results, true),
            MergeMode::RemotePriority => self.merge_priority(local_results, remote_results, false),
            MergeMode::Interleave => self.merge_interleave(local_results, remote_results),
        };

        // Apply max results limit
        let merged: Vec<_> = merged
            .into_iter()
            .take(self.config.max_merged_results)
            .collect();

        let stats = FederatedStats {
            local_count,
            remote_count,
            merged_count: merged.len(),
            duplicates_removed,
            remote_queried: remote_count > 0,
            fallback_reason: None,
            local_latency_ms: 0,
            remote_latency_ms: None,
        };

        (merged, stats)
    }

    /// Merge with one source prioritized.
    fn merge_priority(
        &self,
        local_results: Vec<SearchResult>,
        remote_results: Vec<SearchResult>,
        local_first: bool,
    ) -> (Vec<FederatedResult>, usize) {
        let mut seen_ids: HashSet<String> = HashSet::new();
        let mut merged = Vec::new();

        let (primary, secondary) = if local_first {
            (local_results, remote_results)
        } else {
            (remote_results, local_results)
        };

        let primary_source = if local_first {
            ResultSource::Local
        } else {
            ResultSource::Remote
        };
        let secondary_source = if local_first {
            ResultSource::Remote
        } else {
            ResultSource::Local
        };

        // Add primary results
        for result in primary {
            seen_ids.insert(result.id.clone());
            merged.push(FederatedResult {
                result,
                source: primary_source,
            });
        }

        // Add secondary results that aren't duplicates
        let mut duplicates: usize = 0;
        for result in secondary {
            if !seen_ids.contains(&result.id) {
                seen_ids.insert(result.id.clone());
                merged.push(FederatedResult {
                    result,
                    source: secondary_source,
                });
            } else {
                duplicates = duplicates.saturating_add(1);
            }
        }

        (merged, duplicates)
    }

    /// Merge by interleaving based on score.
    fn merge_interleave(
        &self,
        local_results: Vec<SearchResult>,
        remote_results: Vec<SearchResult>,
    ) -> (Vec<FederatedResult>, usize) {
        let mut all: Vec<FederatedResult> = Vec::new();
        let mut seen_ids: HashSet<String> = HashSet::new();
        let mut duplicates: usize = 0;

        // Combine all results
        for result in local_results {
            all.push(FederatedResult::local(result));
        }
        for result in remote_results {
            all.push(FederatedResult::remote(result));
        }

        // Sort by score descending
        all.sort_by(|a, b| {
            b.result
                .score
                .partial_cmp(&a.result.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Remove duplicates (keep highest scored)
        let mut deduped = Vec::new();
        for item in all {
            if !seen_ids.contains(&item.result.id) {
                seen_ids.insert(item.result.id.clone());
                deduped.push(item);
            } else {
                duplicates = duplicates.saturating_add(1);
            }
        }

        (deduped, duplicates)
    }

    /// Create a merged response from local and remote responses.
    pub fn merge_responses(
        &self,
        local_response: QueryResponse,
        remote_response: Option<QueryResponse>,
    ) -> (QueryResponse, FederatedStats) {
        let local_latency = local_response.took_ms;
        let remote_latency = remote_response.as_ref().map(|r| r.took_ms);

        let remote_results = remote_response.map(|r| r.results).unwrap_or_default();

        let (merged, mut stats) = self.merge(local_response.results, remote_results);

        stats.local_latency_ms = local_latency;
        stats.remote_latency_ms = remote_latency;

        let response = QueryResponse {
            results: merged.into_iter().map(|f| f.result).collect(),
            cache_status: CacheStatus::Miss, // Will be set by caller
            took_ms: local_latency.saturating_add(remote_latency.unwrap_or(0)),
            generated_at: None,
            miss_reason: None,
        };

        (response, stats)
    }
}

/// Response from federated search including source attribution.
#[derive(Debug, Clone, Serialize)]
pub struct FederatedResponse {
    /// Results with source attribution.
    pub results: Vec<FederatedResult>,
    /// Cache status.
    pub cache_status: CacheStatus,
    /// Total time in milliseconds.
    pub took_ms: u64,
    /// Statistics about the federated search.
    pub stats: FederatedStats,
}

#[cfg(test)]
#[path = "tests/federated_tests.rs"]
mod tests;
