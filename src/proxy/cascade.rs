//! Priority-based cascade query execution.
//!
//! When a query doesn't find satisfactory results in the primary upstream
//! (based on score threshold or result count), the system cascades to the
//! next priority upstream.
//!
//! # Example Flow
//!
//! ```text
//! Query -> Priority 1 (VectorDB) -> score < 0.8 -> Priority 2 (FTS) -> score >= 0.7 -> Return
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::context::UpstreamType;
use super::metrics::ProxyMetrics;
use super::pool::{PooledUpstream, UpstreamPool};
use super::types::{QueryRequest, QueryResponse, SearchResult};
use super::upstream::{QueryMode, UpstreamError};

/// Configuration for cascade query behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CascadeConfig {
    /// Enable cascade fallback when results are below threshold.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Default minimum score threshold (normalized 0-1).
    /// Results below this trigger cascade to next upstream.
    #[serde(default = "default_min_score_threshold")]
    pub min_score_threshold: f32,

    /// Minimum number of results required.
    /// Fewer results trigger cascade.
    #[serde(default = "default_min_results")]
    pub min_results: usize,

    /// Maximum number of upstreams to try.
    #[serde(default = "default_max_cascade_depth")]
    pub max_cascade_depth: usize,

    /// Whether to merge results from multiple upstreams.
    #[serde(default)]
    pub merge_cascade_results: bool,

    /// Timeout for entire cascade operation in milliseconds.
    #[serde(default = "default_cascade_timeout_ms")]
    pub cascade_timeout_ms: u64,

    /// Result fusion method for equal-priority upstream groups.
    /// `None` returns the first upstream that meets the threshold.
    /// `Rrf` (Reciprocal Rank Fusion) runs equal-priority upstreams in
    /// parallel and merges their results by content-deduplicated RRF score.
    /// Fusion only applies within a priority group; the cascade still
    /// proceeds to the next priority group if the threshold is not met.
    #[serde(default)]
    pub fusion_method: FusionMethod,

    /// RRF constant: `score(d) = sum(1.0 / (k + rank))`. Standard value is 60.
    #[serde(default = "default_rrf_k")]
    pub rrf_k: u32,
}

/// Result fusion method for multi-upstream queries within a priority group.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FusionMethod {
    /// No fusion: return first upstream that meets the threshold (default).
    #[default]
    None,
    /// Reciprocal Rank Fusion: merge by `1.0 / (k + rank)` with content dedup.
    Rrf,
}

fn default_enabled() -> bool {
    true
}
fn default_min_score_threshold() -> f32 {
    0.7
}
fn default_min_results() -> usize {
    1
}
fn default_max_cascade_depth() -> usize {
    3
}
fn default_cascade_timeout_ms() -> u64 {
    30000
}
fn default_rrf_k() -> u32 {
    60
}

impl Default for CascadeConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            min_score_threshold: default_min_score_threshold(),
            min_results: default_min_results(),
            max_cascade_depth: default_max_cascade_depth(),
            merge_cascade_results: false,
            cascade_timeout_ms: default_cascade_timeout_ms(),
            fusion_method: FusionMethod::default(),
            rrf_k: default_rrf_k(),
        }
    }
}

impl CascadeConfig {
    /// Create a new cascade config with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the minimum score threshold.
    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.min_score_threshold = threshold;
        self
    }

    /// Set the minimum results required.
    pub fn with_min_results(mut self, min_results: usize) -> Self {
        self.min_results = min_results;
        self
    }

    /// Set the maximum cascade depth.
    pub fn with_max_depth(mut self, max_depth: usize) -> Self {
        self.max_cascade_depth = max_depth;
        self
    }

    /// Enable result merging from multiple upstreams.
    pub fn with_merge(mut self, merge: bool) -> Self {
        self.merge_cascade_results = merge;
        self
    }

    /// Set the cascade timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.cascade_timeout_ms = timeout.as_millis() as u64;
        self
    }

    /// Set the fusion method for equal-priority upstream groups.
    pub fn with_fusion(mut self, method: FusionMethod) -> Self {
        self.fusion_method = method;
        self
    }

    /// Set the RRF k constant.
    pub fn with_rrf_k(mut self, k: u32) -> Self {
        self.rrf_k = k;
        self
    }

    /// Disable cascade.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    /// Get the cascade timeout as a Duration.
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.cascade_timeout_ms)
    }
}

/// Per-upstream cascade settings.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpstreamCascadeConfig {
    /// Override threshold for this upstream (None = use default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_score_threshold: Option<f32>,

    /// Priority in cascade order (lower = tried first).
    #[serde(default)]
    pub cascade_priority: u32,

    /// Whether to skip this upstream in cascade.
    #[serde(default)]
    pub skip_in_cascade: bool,
}

impl UpstreamCascadeConfig {
    /// Create a new per-upstream cascade config.
    pub fn new(priority: u32) -> Self {
        Self {
            cascade_priority: priority,
            ..Self::default()
        }
    }

    /// Set the per-upstream threshold.
    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.min_score_threshold = Some(threshold);
        self
    }

    /// Mark this upstream to skip in cascade.
    pub fn skip(mut self) -> Self {
        self.skip_in_cascade = true;
        self
    }
}

/// Result of a cascade query.
#[derive(Debug, Clone, Serialize)]
pub struct CascadeResult {
    /// The final results.
    pub results: Vec<SearchResult>,

    /// Which upstreams were tried.
    pub upstreams_tried: Vec<String>,

    /// Which upstream provided the final results.
    pub final_upstream: Option<String>,

    /// Why cascade stopped.
    pub stop_reason: CascadeStopReason,

    /// Scores from each upstream tried.
    pub upstream_scores: Vec<UpstreamScore>,

    /// Total time for cascade operation in milliseconds.
    pub cascade_time_ms: u64,

    /// Cascade depth (how many upstreams were tried).
    pub cascade_depth: usize,
}

impl CascadeResult {
    /// Check if the cascade was successful (found results meeting threshold).
    pub fn is_success(&self) -> bool {
        matches!(
            self.stop_reason,
            CascadeStopReason::ThresholdMet | CascadeStopReason::MinResultsMet
        )
    }

    /// Get the number of results.
    pub fn result_count(&self) -> usize {
        self.results.len()
    }

    /// Get the maximum score from results.
    pub fn max_score(&self) -> Option<f32> {
        self.results
            .iter()
            .map(|r| r.score)
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
    }
}

/// Reason why cascade stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CascadeStopReason {
    /// Score threshold was met.
    ThresholdMet,
    /// Minimum results were achieved.
    MinResultsMet,
    /// Max cascade depth reached.
    MaxDepthReached,
    /// All upstreams failed or exhausted.
    AllExhausted,
    /// Timeout reached.
    Timeout,
    /// Cascade is disabled.
    Disabled,
    /// No upstreams available.
    NoUpstreams,
}

impl CascadeStopReason {
    /// Get a human-readable description.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ThresholdMet => "threshold_met",
            Self::MinResultsMet => "min_results_met",
            Self::MaxDepthReached => "max_depth_reached",
            Self::AllExhausted => "all_exhausted",
            Self::Timeout => "timeout",
            Self::Disabled => "disabled",
            Self::NoUpstreams => "no_upstreams",
        }
    }
}

/// Score information from a single upstream query.
#[derive(Debug, Clone, Serialize)]
pub struct UpstreamScore {
    /// Upstream identifier.
    pub upstream_id: String,

    /// Type of upstream backend.
    pub upstream_type: UpstreamType,

    /// Query mode used.
    pub query_mode: QueryMode,

    /// Maximum raw score from results.
    pub max_score: f32,

    /// Score normalized to 0-1 range.
    pub normalized_score: f32,

    /// Number of results returned.
    pub result_count: usize,

    /// Query latency in milliseconds.
    pub latency_ms: u64,

    /// Whether this upstream met the threshold.
    pub met_threshold: bool,

    /// Error message if query failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Reciprocal Rank Fusion: merge result lists from multiple upstreams.
///
/// For each document `d`, `rrf_score(d) = sum_l 1.0 / (k + rank_l(d))`
/// where `rank_l(d)` is the 1-based position of `d` in list `l`.
/// Documents are deduplicated by `blake3(content)` so the same content
/// from different upstreams is merged into a single entry.
///
/// Returns at most `max_results` entries, sorted by RRF score descending.
pub fn fuse_rrf(
    lists: Vec<(String, Vec<SearchResult>)>,
    k: u32,
    max_results: usize,
) -> Vec<SearchResult> {
    use std::collections::HashMap;

    struct Accum {
        rrf_score: f32,
        best_result: SearchResult,
    }

    let mut accum: HashMap<[u8; 32], Accum> = HashMap::new();

    for (_upstream_id, results) in lists {
        for (rank, result) in results.iter().enumerate() {
            // Deduplicate by content hash (cross-backend safe)
            let key = *blake3::hash(result.content.as_bytes()).as_bytes();
            let rrf = 1.0 / (k as f32 + rank as f32 + 1.0);

            accum
                .entry(key)
                .and_modify(|a| {
                    a.rrf_score += rrf;
                    // Keep highest-scoring instance as representative
                    if result.score > a.best_result.score {
                        a.best_result = result.clone();
                    }
                })
                .or_insert_with(|| Accum {
                    rrf_score: rrf,
                    best_result: result.clone(),
                });
        }
    }

    let mut merged: Vec<(f32, SearchResult)> = accum
        .into_values()
        .map(|a| (a.rrf_score, a.best_result))
        .collect();

    merged.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    merged.truncate(max_results);
    merged.into_iter().map(|(_, r)| r).collect()
}

/// Group upstreams by priority. Returns groups sorted by priority ascending
/// (lower number = higher priority, tried first). Each inner vec is unsorted.
fn group_by_priority(upstreams: &[Arc<PooledUpstream>]) -> Vec<Vec<Arc<PooledUpstream>>> {
    let mut groups: std::collections::BTreeMap<u32, Vec<Arc<PooledUpstream>>> =
        std::collections::BTreeMap::new();
    for u in upstreams {
        groups.entry(u.priority).or_default().push(Arc::clone(u));
    }
    groups.into_values().collect()
}

/// Result of fusing one upstream's output during RRF.
struct RrfUpstreamResult {
    uid: String,
    u_type: UpstreamType,
    u_mode: QueryMode,
    results: Vec<SearchResult>,
    latency_ms: u64,
    error: Option<String>,
}

/// Aggregate outcome of `run_rrf_group`.
struct RrfGroupOutcome {
    /// Fused results across all successful upstreams; `None` if all errored.
    fused_results: Option<Vec<SearchResult>>,
    /// Whether the fused result meets the configured threshold.
    met_threshold: bool,
    /// Best normalized score seen across the group (for best_results tracking).
    best_normalized: f32,
    /// Upstream IDs in completion order (for `upstreams_tried` accounting).
    upstream_ids: Vec<String>,
    /// Per-upstream score records (for `upstream_scores` reporting).
    scores: Vec<UpstreamScore>,
}

/// Cascade query executor.
pub struct CascadeExecutor {
    pool: Arc<UpstreamPool>,
    config: CascadeConfig,
    metrics: Option<Arc<ProxyMetrics>>,
    /// Smart embedder for VectorOnly upstreams (optional, requires proxy-embed feature).
    #[cfg(feature = "embed-api")]
    embedder: Option<Arc<super::smart_embedder::SmartEmbedder>>,
}

impl CascadeExecutor {
    /// Create a new cascade executor.
    pub fn new(pool: Arc<UpstreamPool>, config: CascadeConfig) -> Self {
        Self {
            pool,
            config,
            metrics: None,
            #[cfg(feature = "embed-api")]
            embedder: None,
        }
    }

    /// Create with metrics tracking.
    pub fn with_metrics(
        pool: Arc<UpstreamPool>,
        config: CascadeConfig,
        metrics: Arc<ProxyMetrics>,
    ) -> Self {
        Self {
            pool,
            config,
            metrics: Some(metrics),
            #[cfg(feature = "embed-api")]
            embedder: None,
        }
    }

    /// Set the smart embedder for VectorOnly upstream support.
    #[cfg(feature = "embed-api")]
    pub fn with_embedder(mut self, embedder: Arc<super::smart_embedder::SmartEmbedder>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    /// Get the cascade configuration.
    pub fn config(&self) -> &CascadeConfig {
        &self.config
    }

    /// Get upstreams sorted by cascade priority.
    ///
    /// Returns upstreams sorted by their priority field (lower = tried first).
    /// Falls back to all upstreams when none are available (allows recovery
    /// from offline state, matching `UpstreamPool::query()` behavior).
    pub fn cascade_order(&self) -> Vec<Arc<PooledUpstream>> {
        let mut upstreams: Vec<_> = self.pool.available();
        if upstreams.is_empty() {
            upstreams = self.pool.all().to_vec();
        }
        upstreams.sort_by_key(|u| u.priority);
        upstreams
    }

    /// Get upstreams sorted by cascade priority, with optional upstream type preference.
    ///
    /// When `preferred_type` is set, matching upstreams are moved to the front
    /// of the cascade order (preserving priority order within each group).
    pub fn cascade_order_with_preference(
        &self,
        preferred_type: Option<&str>,
    ) -> Vec<Arc<PooledUpstream>> {
        let mut upstreams = self.cascade_order();

        if let Some(pref) = preferred_type {
            let pref_lower = pref.to_lowercase();
            // R8: pre-compute sort keys once per upstream instead of allocating
            // to_lowercase() per sort comparison (O(n log n) → O(n) allocations)
            let mut keyed: Vec<_> = upstreams
                .into_iter()
                .map(|u| {
                    let type_str = u.upstream_type().as_str().to_ascii_lowercase();
                    let type_match = type_str.contains(&pref_lower);
                    let id_match = u.id.to_ascii_lowercase().contains(&pref_lower);
                    let key = (!(type_match || id_match) as u32, u.priority);
                    (key, u)
                })
                .collect();
            keyed.sort_by_key(|(key, _)| *key);
            upstreams = keyed.into_iter().map(|(_, u)| u).collect();
        }

        upstreams
    }

    /// Execute a cascade query.
    ///
    /// Tries upstreams in priority order until one returns results that meet
    /// the configured threshold, or all upstreams are exhausted.
    ///
    /// Respects `request.upstream_id` (direct routing) and `request.upstream_type`
    /// (preference hint for cascade ordering).
    #[allow(clippy::arithmetic_side_effects)] // depth counters; never overflow in practice
    pub async fn query(&self, request: &QueryRequest) -> CascadeResult {
        let start = Instant::now();

        // Check if cascade is disabled
        if !self.config.enabled {
            return CascadeResult {
                results: Vec::new(),
                upstreams_tried: Vec::new(),
                final_upstream: None,
                stop_reason: CascadeStopReason::Disabled,
                upstream_scores: Vec::new(),
                cascade_time_ms: start.elapsed().as_millis() as u64,
                cascade_depth: 0,
            };
        }

        let mut upstreams_tried = Vec::with_capacity(self.config.max_cascade_depth);
        let mut upstream_scores = Vec::with_capacity(self.config.max_cascade_depth);
        let mut best_results: Option<(String, Vec<SearchResult>, f32)> = None;

        // If upstream_id is set, try that upstream first. On failure, fall
        // through to the normal cascade so vector-targeted queries degrade
        // gracefully (e.g. when the proxy lacks embedding support).
        if let Some(ref target_id) = request.upstream_id {
            if let Some(upstream) = self.pool.get(target_id) {
                let targeted = self.query_single_targeted(request, &upstream, start).await;
                if !targeted.results.is_empty() {
                    return targeted;
                }
                // Targeted upstream returned no results — fall through to cascade
                debug!(
                    target_id = %target_id,
                    stop_reason = targeted.stop_reason.as_str(),
                    "Targeted upstream returned no results, falling back to cascade"
                );
            } else {
                debug!(target_id = %target_id, "Targeted upstream not found, falling back to cascade");
            }
        }

        // Get upstreams sorted by cascade priority, with optional type preference
        let upstreams = self.cascade_order_with_preference(request.upstream_type.as_deref());

        info!(
            query = %request.query,
            upstream_count = upstreams.len(),
            threshold = self.config.min_score_threshold,
            max_depth = self.config.max_cascade_depth,
            "Cascade query starting"
        );

        if upstreams.is_empty() {
            return CascadeResult {
                results: Vec::new(),
                upstreams_tried: Vec::new(),
                final_upstream: None,
                stop_reason: CascadeStopReason::NoUpstreams,
                upstream_scores: Vec::new(),
                cascade_time_ms: start.elapsed().as_millis() as u64,
                cascade_depth: 0,
            };
        }

        let timeout = self.config.timeout();

        // Group upstreams by priority for cascade with optional RRF fusion.
        // Lower priority number = tried first. RRF only merges within a group.
        let groups = group_by_priority(&upstreams);
        let mut depth: usize = 0;

        for group in &groups {
            // Check max depth at group boundary
            if depth >= self.config.max_cascade_depth {
                self.record_cascade_depth(depth);
                let (final_upstream, results) = match best_results.take() {
                    Some((id, r, _)) => (Some(id), r),
                    None => (None, Vec::new()),
                };
                return CascadeResult {
                    results,
                    upstreams_tried,
                    final_upstream,
                    stop_reason: CascadeStopReason::MaxDepthReached,
                    upstream_scores,
                    cascade_time_ms: start.elapsed().as_millis() as u64,
                    cascade_depth: depth,
                };
            }

            // Check timeout at group boundary
            if start.elapsed() > timeout {
                self.record_cascade_timeout();
                let (final_upstream, results) = match best_results.take() {
                    Some((id, r, _)) => (Some(id), r),
                    None => (None, Vec::new()),
                };
                return CascadeResult {
                    results,
                    upstreams_tried,
                    final_upstream,
                    stop_reason: CascadeStopReason::Timeout,
                    upstream_scores,
                    cascade_time_ms: start.elapsed().as_millis() as u64,
                    cascade_depth: depth,
                };
            }

            // RRF path: query all upstreams in this group in parallel and fuse.
            // Only applies when fusion_method == Rrf AND group has 2+ upstreams.
            if self.config.fusion_method == FusionMethod::Rrf && group.len() >= 2 {
                let rrf_outcome = self.run_rrf_group(group, request, start, &timeout).await;
                depth += group.len();
                for uid in &rrf_outcome.upstream_ids {
                    upstreams_tried.push(uid.clone());
                }
                upstream_scores.extend(rrf_outcome.scores);

                if let Some(fused) = rrf_outcome.fused_results {
                    if rrf_outcome.met_threshold {
                        let final_depth = depth;
                        self.record_cascade_success(final_depth);
                        let cascade_time_ms = start.elapsed().as_millis() as u64;
                        info!(
                            stop_reason = "rrf_threshold_met",
                            cascade_depth = final_depth,
                            cascade_time_ms,
                            "Cascade complete (RRF)"
                        );
                        return CascadeResult {
                            results: fused,
                            upstreams_tried,
                            final_upstream: None, // RRF result is multi-source
                            stop_reason: CascadeStopReason::ThresholdMet,
                            upstream_scores,
                            cascade_time_ms,
                            cascade_depth: final_depth,
                        };
                    }
                    // Not met — track as best and continue to next priority group
                    if best_results.is_none()
                        || rrf_outcome.best_normalized
                            > best_results.as_ref().map(|r| r.2).unwrap_or(0.0)
                    {
                        best_results = Some((
                            rrf_outcome
                                .upstream_ids
                                .first()
                                .cloned()
                                .unwrap_or_default(),
                            fused,
                            rrf_outcome.best_normalized,
                        ));
                    }
                }
                // If fused is None (all upstreams errored), continue to next group
                continue;
            }

            // Sequential path: process each upstream in the group one at a time
            for upstream in group {
                // Check max depth
                if depth >= self.config.max_cascade_depth {
                    self.record_cascade_depth(depth);
                    let (final_upstream, results) = match best_results.take() {
                        Some((id, r, _)) => (Some(id), r),
                        None => (None, Vec::new()),
                    };
                    return CascadeResult {
                        results,
                        upstreams_tried,
                        final_upstream,
                        stop_reason: CascadeStopReason::MaxDepthReached,
                        upstream_scores,
                        cascade_time_ms: start.elapsed().as_millis() as u64,
                        cascade_depth: depth,
                    };
                }

                // Check timeout
                if start.elapsed() > timeout {
                    self.record_cascade_timeout();
                    let (final_upstream, results) = match best_results.take() {
                        Some((id, r, _)) => (Some(id), r),
                        None => (None, Vec::new()),
                    };
                    return CascadeResult {
                        results,
                        upstreams_tried,
                        final_upstream,
                        stop_reason: CascadeStopReason::Timeout,
                        upstream_scores,
                        cascade_time_ms: start.elapsed().as_millis() as u64,
                        cascade_depth: depth,
                    };
                }

                // R2: clone uid once per iteration; uid.clone() for tried/results/scores
                // PERF(R2): deferred — changing upstream_id to Arc<str> would eliminate
                // N+1 String clones per cascade step (N = result count)
                let uid = upstream.id.clone();
                upstreams_tried.push(uid.clone());

                // Cache RwLock reads once per iteration
                let u_type = upstream.upstream_type();
                let u_mode = upstream.query_mode();

                let query_start = Instant::now();
                match self.query_upstream(upstream, request).await {
                    Ok(response) => {
                        // Tag each result with the upstream that produced it
                        let mut results = response.results;
                        for r in &mut results {
                            r.upstream_id = Some(uid.clone());
                        }

                        let max_score = results
                            .iter()
                            .map(|r| r.score)
                            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                            .unwrap_or(0.0);

                        // Normalize score based on upstream type
                        let normalized = self.normalize_score(max_score, u_type);

                        // Get threshold (use global default)
                        let threshold = self.config.min_score_threshold;

                        let met_threshold =
                            normalized >= threshold && results.len() >= self.config.min_results;

                        let latency_ms = query_start.elapsed().as_millis() as u64;

                        upstream_scores.push(UpstreamScore {
                            upstream_id: uid.clone(),
                            upstream_type: u_type,
                            query_mode: u_mode,
                            max_score,
                            normalized_score: normalized,
                            result_count: results.len(),
                            latency_ms,
                            met_threshold,
                            error: None,
                        });

                        info!(
                            upstream_id = %uid,
                            upstream_type = %u_type.as_str(),
                            normalized_score = normalized,
                            met_threshold,
                            result_count = results.len(),
                            latency_ms,
                            "Cascade upstream queried"
                        );
                        debug!(
                            upstream_id = %uid,
                            raw_score = max_score,
                            threshold,
                            "Cascade raw score details"
                        );

                        if met_threshold {
                            let final_depth = depth.saturating_add(1);
                            self.record_cascade_success(final_depth);
                            upstream.record_success();

                            let cascade_time_ms = start.elapsed().as_millis() as u64;
                            info!(
                                stop_reason = "threshold_met",
                                cascade_depth = final_depth,
                                cascade_time_ms,
                                final_upstream = %uid,
                                "Cascade complete"
                            );

                            return CascadeResult {
                                results,
                                upstreams_tried,
                                final_upstream: Some(uid),
                                stop_reason: CascadeStopReason::ThresholdMet,
                                upstream_scores,
                                cascade_time_ms,
                                cascade_depth: final_depth,
                            };
                        }

                        // Track best results seen (move instead of clone — we continue cascading)
                        if best_results.is_none()
                            || normalized > best_results.as_ref().map(|r| r.2).unwrap_or(0.0)
                        {
                            best_results = Some((uid, results, normalized));
                        }

                        // Record success but continue cascade
                        upstream.record_success();
                    }
                    Err(e) => {
                        let latency_ms = query_start.elapsed().as_millis() as u64;
                        upstream.record_failure();
                        upstream_scores.push(UpstreamScore {
                            upstream_id: uid,
                            upstream_type: u_type,
                            query_mode: u_mode,
                            max_score: 0.0,
                            normalized_score: 0.0,
                            result_count: 0,
                            latency_ms,
                            met_threshold: false,
                            error: Some(e.to_string()),
                        });
                        debug!(
                            upstream_id = %upstream.id,
                            error = %e,
                            latency_ms,
                            "Cascade upstream failed"
                        );
                        // Continue to next upstream on error
                    }
                }
                depth += 1;
            }
        }

        // All upstreams exhausted, return best found
        let cascade_depth = upstreams_tried.len();
        self.record_cascade_exhausted();
        self.record_cascade_depth(cascade_depth);
        let cascade_time_ms = start.elapsed().as_millis() as u64;
        let (final_upstream, results) = match best_results.take() {
            Some((id, r, _)) => (Some(id), r),
            None => (None, Vec::new()),
        };

        info!(
            stop_reason = "all_exhausted",
            cascade_depth,
            cascade_time_ms,
            final_upstream = final_upstream.as_deref().unwrap_or("none"),
            "Cascade complete"
        );

        CascadeResult {
            results,
            upstreams_tried,
            final_upstream,
            stop_reason: CascadeStopReason::AllExhausted,
            upstream_scores,
            cascade_time_ms,
            cascade_depth,
        }
    }

    /// Query a single targeted upstream directly (bypass cascade logic).
    async fn query_single_targeted(
        &self,
        request: &QueryRequest,
        upstream: &Arc<PooledUpstream>,
        start: Instant,
    ) -> CascadeResult {
        let query_start = Instant::now();
        match self.query_upstream(upstream, request).await {
            Ok(response) => {
                let mut results = response.results;
                // R2: clone uid for each result tag, then clone once for tried,
                // move original into final_upstream (N+1 clones → N+1, but
                // restructured so uid is moved not cloned for final_upstream)
                let uid = upstream.id.clone();
                for r in &mut results {
                    r.upstream_id = Some(uid.clone());
                }
                upstream.record_success();
                let tried = vec![uid.clone()];
                CascadeResult {
                    results,
                    upstreams_tried: tried,
                    final_upstream: Some(uid),
                    stop_reason: CascadeStopReason::ThresholdMet,
                    upstream_scores: vec![],
                    cascade_time_ms: start.elapsed().as_millis() as u64,
                    cascade_depth: 1,
                }
            }
            Err(e) => {
                let _latency_ms = query_start.elapsed().as_millis() as u64;
                upstream.record_failure();
                debug!(upstream_id = %upstream.id, error = %e, "Targeted upstream query failed");
                // R2: clone once for upstreams_tried, no final_upstream needed
                CascadeResult {
                    results: Vec::new(),
                    upstreams_tried: vec![upstream.id.clone()],
                    final_upstream: None,
                    stop_reason: CascadeStopReason::AllExhausted,
                    upstream_scores: vec![],
                    cascade_time_ms: start.elapsed().as_millis() as u64,
                    cascade_depth: 1,
                }
            }
        }
    }

    /// Execute a query against an upstream with QueryMode-aware routing.
    ///
    /// Mirrors the probe-first logic from `query_with_mode()` in query.rs:
    /// - `VectorOnly` → embed text via SmartEmbedder, call `query_vector_pooled()`
    /// - `TextNative` → `query_pooled()` directly
    /// - `Unknown` → try text first; on failure, discover mode; if VectorOnly, embed and retry
    ///
    /// Without the `proxy-embed` feature, always uses `query_pooled()` (text only).
    async fn query_upstream(
        &self,
        upstream: &PooledUpstream,
        request: &QueryRequest,
    ) -> Result<QueryResponse, UpstreamError> {
        #[cfg(feature = "embed-api")]
        {
            let mode = upstream.query_mode();
            match mode {
                QueryMode::VectorOnly => {
                    let embedder = self.embedder.as_ref().ok_or_else(|| {
                        UpstreamError::EmbeddingRequired(
                            "VectorOnly upstream requires proxy-embed feature with embedder configured"
                                .to_string(),
                        )
                    })?;
                    let vector = embedder
                        .embed(&request.query)
                        .await
                        .map_err(|e| UpstreamError::EmbeddingFailed(e.to_string()))?;
                    upstream.query_vector_pooled(request, &vector).await
                }
                QueryMode::TextNative => upstream.query_pooled(request).await,
                QueryMode::Unknown => {
                    // Probe-first: try text, discover mode on failure
                    match upstream.query_pooled(request).await {
                        Ok(response) => {
                            upstream.set_query_mode(QueryMode::TextNative);
                            Ok(response)
                        }
                        Err(e) if e.indicates_text_not_supported() => {
                            // Text not supported — try to discover and switch to vector
                            if let Ok(discovered) = upstream.adapter.discover_query_mode().await {
                                upstream.set_query_mode(discovered);
                                if discovered == QueryMode::VectorOnly {
                                    let embedder = self.embedder.as_ref().ok_or_else(|| {
                                        UpstreamError::EmbeddingRequired(
                                            "VectorOnly upstream requires embedding support"
                                                .to_string(),
                                        )
                                    })?;
                                    let vector =
                                        embedder.embed(&request.query).await.map_err(|e| {
                                            UpstreamError::EmbeddingFailed(e.to_string())
                                        })?;
                                    return upstream.query_vector_pooled(request, &vector).await;
                                }
                            }
                            Err(e)
                        }
                        Err(e) => Err(e),
                    }
                }
            }
        }

        #[cfg(not(feature = "embed-api"))]
        {
            upstream.query_pooled(request).await
        }
    }

    /// Normalize score to 0-1 range based on upstream type.
    fn normalize_score(&self, score: f32, upstream_type: UpstreamType) -> f32 {
        let (min, max) = upstream_type.score_range();
        if max <= min {
            return score.clamp(0.0, 1.0);
        }
        ((score - min) / (max - min)).clamp(0.0, 1.0)
    }

    /// Run a priority group's upstreams in parallel and fuse results with RRF.
    ///
    /// Used when `fusion_method == Rrf` and the group has 2+ upstreams.
    /// Each upstream is queried concurrently; successful result lists are
    /// merged via `fuse_rrf`. Returns a struct with fused results (if any
    /// upstream succeeded), per-upstream score records, and whether the
    /// fused result meets the threshold.
    async fn run_rrf_group(
        &self,
        group: &[Arc<PooledUpstream>],
        request: &QueryRequest,
        start: Instant,
        timeout: &Duration,
    ) -> RrfGroupOutcome {
        use tokio::task::JoinSet;

        let mut set = JoinSet::new();
        for upstream in group {
            let upstream = Arc::clone(upstream);
            let request = request.clone();
            // Per-task embedder clone for VectorOnly routing (H5 fix).
            #[cfg(feature = "embed-api")]
            let embedder = self.embedder.clone();
            set.spawn(async move {
                let uid = upstream.id.clone();
                let u_type = upstream.upstream_type();
                let u_mode = upstream.query_mode();
                let query_start = Instant::now();
                // Mode-aware dispatch: mirrors `CascadeExecutor::query_upstream`
                // but without borrowing `self`. VectorOnly upstreams get
                // embedded before the call; TextNative uses the text path;
                // Unknown probes text-first and falls back to vector on
                // "text not supported" errors.
                #[cfg(feature = "embed-api")]
                let query_result: Result<QueryResponse, UpstreamError> = match u_mode {
                    QueryMode::VectorOnly => match embedder.as_ref() {
                        Some(emb) => match emb.embed(&request.query).await {
                            Ok(vector) => upstream.query_vector_pooled(&request, &vector).await,
                            Err(e) => Err(UpstreamError::EmbeddingFailed(e.to_string())),
                        },
                        None => Err(UpstreamError::EmbeddingRequired(
                            "VectorOnly upstream requires embedder".to_string(),
                        )),
                    },
                    QueryMode::TextNative => upstream.query_pooled(&request).await,
                    QueryMode::Unknown => match upstream.query_pooled(&request).await {
                        Ok(r) => {
                            upstream.set_query_mode(QueryMode::TextNative);
                            Ok(r)
                        }
                        Err(e) if e.indicates_text_not_supported() => {
                            let discovered = upstream.adapter.discover_query_mode().await.ok();
                            if let Some(d) = discovered {
                                upstream.set_query_mode(d);
                            }
                            if matches!(discovered, Some(QueryMode::VectorOnly)) {
                                match embedder.as_ref() {
                                    Some(emb) => match emb.embed(&request.query).await {
                                        Ok(vector) => {
                                            upstream.query_vector_pooled(&request, &vector).await
                                        }
                                        Err(ee) => {
                                            Err(UpstreamError::EmbeddingFailed(ee.to_string()))
                                        }
                                    },
                                    None => Err(UpstreamError::EmbeddingRequired(
                                        "VectorOnly upstream requires embedder".to_string(),
                                    )),
                                }
                            } else {
                                Err(e)
                            }
                        }
                        Err(e) => Err(e),
                    },
                };
                #[cfg(not(feature = "embed-api"))]
                let query_result: Result<QueryResponse, UpstreamError> =
                    upstream.query_pooled(&request).await;

                match query_result {
                    Ok(mut response) => {
                        for r in &mut response.results {
                            r.upstream_id = Some(uid.clone());
                        }
                        let latency_ms = query_start.elapsed().as_millis() as u64;
                        upstream.record_success();
                        RrfUpstreamResult {
                            uid,
                            u_type,
                            u_mode,
                            results: response.results,
                            latency_ms,
                            error: None,
                        }
                    }
                    Err(e) => {
                        let latency_ms = query_start.elapsed().as_millis() as u64;
                        upstream.record_failure();
                        RrfUpstreamResult {
                            uid,
                            u_type,
                            u_mode,
                            results: Vec::new(),
                            latency_ms,
                            error: Some(e.to_string()),
                        }
                    }
                }
            });
        }

        let mut upstream_ids = Vec::with_capacity(group.len());
        let mut scores = Vec::with_capacity(group.len());
        let mut lists: Vec<(String, Vec<SearchResult>)> = Vec::new();
        let mut best_normalized: f32 = 0.0;

        while let Some(joined) = set.join_next().await {
            let elapsed = start.elapsed();
            if elapsed > *timeout {
                // Timed out — abandon remaining
                set.abort_all();
                break;
            }
            // join_next: outer is JoinError, inner is our RrfUpstreamResult
            let result = match joined {
                Ok(r) => r,
                Err(je) => {
                    // Task panicked or was aborted. Log so production can
                    // observe upstream-driver panics; do not let one bad
                    // upstream take down the whole RRF group.
                    if je.is_panic() {
                        warn!(error = %je, "RRF: upstream task panicked");
                    } else if je.is_cancelled() {
                        debug!(error = %je, "RRF: upstream task cancelled");
                    } else {
                        debug!(error = %je, "RRF: upstream task join error");
                    }
                    continue;
                }
            };
            upstream_ids.push(result.uid.clone());
            scores.push(UpstreamScore {
                upstream_id: result.uid.clone(),
                upstream_type: result.u_type,
                query_mode: result.u_mode,
                max_score: result
                    .results
                    .iter()
                    .map(|r| r.score)
                    .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                    .unwrap_or(0.0),
                normalized_score: {
                    let mx = result
                        .results
                        .iter()
                        .map(|r| r.score)
                        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                        .unwrap_or(0.0);
                    self.normalize_score(mx, result.u_type)
                },
                result_count: result.results.len(),
                latency_ms: result.latency_ms,
                met_threshold: {
                    let mx = result
                        .results
                        .iter()
                        .map(|r| r.score)
                        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                        .unwrap_or(0.0);
                    let norm = self.normalize_score(mx, result.u_type);
                    norm >= self.config.min_score_threshold
                        && result.results.len() >= self.config.min_results
                },
                error: result.error.clone(),
            });
            // Track best normalized for best_results fallback
            if !result.results.is_empty() {
                let mx = result
                    .results
                    .iter()
                    .map(|r| r.score)
                    .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                    .unwrap_or(0.0);
                let norm = self.normalize_score(mx, result.u_type);
                if norm > best_normalized {
                    best_normalized = norm;
                }
                lists.push((result.uid, result.results));
            }
        }

        if lists.is_empty() {
            return RrfGroupOutcome {
                fused_results: None,
                met_threshold: false,
                best_normalized: 0.0,
                upstream_ids,
                scores,
            };
        }

        // Cap fused results to request.top_k (or a reasonable default) to avoid
        // unbounded lists when many upstreams return many results.
        let max_results = request.top_k.unwrap_or(10).max(1);
        let fused = fuse_rrf(lists, self.config.rrf_k, max_results);
        let met_threshold = !fused.is_empty()
            && best_normalized >= self.config.min_score_threshold
            && fused.len() >= self.config.min_results;

        RrfGroupOutcome {
            fused_results: Some(fused),
            met_threshold,
            best_normalized,
            upstream_ids,
            scores,
        }
    }

    fn record_cascade_success(&self, depth: usize) {
        if let Some(ref metrics) = self.metrics {
            metrics.record_cascade_success(depth);
        }
    }

    fn record_cascade_exhausted(&self) {
        if let Some(ref metrics) = self.metrics {
            metrics.record_cascade_exhausted();
        }
    }

    fn record_cascade_timeout(&self) {
        if let Some(ref metrics) = self.metrics {
            metrics.record_cascade_timeout();
        }
    }

    fn record_cascade_depth(&self, depth: usize) {
        if let Some(ref metrics) = self.metrics {
            metrics.record_cascade_depth(depth);
        }
    }
}

/// Cascade query error.
#[derive(Debug)]
pub enum CascadeError {
    /// No upstreams available.
    NoUpstreamsAvailable,
    /// All upstreams failed.
    AllUpstreamsFailed(Vec<String>),
    /// Timeout reached.
    Timeout,
    /// Cascade is disabled.
    Disabled,
}

impl std::fmt::Display for CascadeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoUpstreamsAvailable => write!(f, "No upstreams available"),
            Self::AllUpstreamsFailed(ids) => {
                write!(f, "All upstreams failed: {}", ids.join(", "))
            }
            Self::Timeout => write!(f, "Cascade timeout reached"),
            Self::Disabled => write!(f, "Cascade is disabled"),
        }
    }
}

impl std::error::Error for CascadeError {}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
#[path = "tests/cascade_tests.rs"]
mod tests;
