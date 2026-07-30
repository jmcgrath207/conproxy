//! `cache_tune` — dry-run TTL / freshness hit-rate probe.

#![allow(clippy::arithmetic_side_effects)] // counters / rates; bounded event sets

use serde::{Deserialize, Serialize};

use super::session::{TuneRunRecord, TuneSessionStore};
use super::{TuneBudget, TuneReport};

/// One synthetic cache access event (caller-supplied timestamps).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheAccessEvent {
    /// Query or key id (for grouping).
    pub key: String,
    /// Age of cached entry at access time (seconds). `None` = miss (not in cache).
    #[serde(default)]
    pub age_secs: Option<u64>,
}

/// Cache tune params.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheTuneParams {
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub context_id: Option<String>,
    pub events: Vec<CacheAccessEvent>,
    /// Fresh TTL candidates (seconds).
    #[serde(default)]
    pub fresh_ttl_secs: Option<Vec<u64>>,
    /// Stale-serve TTL candidates (seconds); entry older than fresh but ≤ stale may serve stale.
    #[serde(default)]
    pub stale_ttl_secs: Option<Vec<u64>>,
    #[serde(default)]
    pub budget: TuneBudget,
}

/// One TTL candidate outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheTuneCandidate {
    pub fresh_ttl_secs: u64,
    pub stale_ttl_secs: u64,
    pub hits: usize,
    pub stale_hits: usize,
    pub misses: usize,
    pub hit_rate: f64,
    pub estimated_upstream_calls: usize,
}

/// Cache tune report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheTuneReport {
    pub tool: String,
    pub session_id: String,
    pub run_id: String,
    pub candidates: Vec<CacheTuneCandidate>,
    pub warnings: Vec<String>,
    pub report: TuneReport,
}

/// Simulate hit/stale/miss under candidate TTLs. Does not touch live cache.
///
/// # Errors
///
/// Session missing, empty events, or oversized grid.
pub fn cache_tune(
    store: &TuneSessionStore,
    params: CacheTuneParams,
) -> Result<CacheTuneReport, String> {
    let _sess = store
        .get(
            &params.session_id,
            params.agent_id.as_deref(),
            params.context_id.as_deref(),
        )
        .ok_or_else(|| "session not found".to_string())?;

    if params.events.is_empty() {
        return Err("events must not be empty".into());
    }

    let mut warnings = Vec::new();
    let events: Vec<_> = params
        .events
        .iter()
        .take(params.budget.max_results.saturating_mul(4).max(64))
        .cloned()
        .collect();
    if params.events.len() > events.len() {
        warnings.push(format!(
            "truncated events {} → {}",
            params.events.len(),
            events.len()
        ));
    }

    let fresh = params
        .fresh_ttl_secs
        .clone()
        .unwrap_or_else(|| vec![60, 300, 900]);
    let stale = params
        .stale_ttl_secs
        .clone()
        .unwrap_or_else(|| vec![0, 60, 300]);

    let mut grid = Vec::new();
    for &f in &fresh {
        for &s in &stale {
            grid.push((f, s));
        }
    }
    if grid.len() > params.budget.max_sweep_points {
        return Err(format!(
            "TTL grid has {} points; max is {}",
            grid.len(),
            params.budget.max_sweep_points
        ));
    }

    let mut candidates = Vec::with_capacity(grid.len());
    for (fresh_ttl, stale_ttl) in grid {
        let mut hits = 0usize;
        let mut stale_hits = 0usize;
        let mut misses = 0usize;
        for ev in &events {
            match ev.age_secs {
                None => misses += 1,
                Some(age) if age <= fresh_ttl => hits += 1,
                Some(age) if stale_ttl > 0 && age <= fresh_ttl.saturating_add(stale_ttl) => {
                    stale_hits += 1;
                }
                Some(_) => misses += 1,
            }
        }
        let total = events.len().max(1) as f64;
        let served = hits + stale_hits;
        candidates.push(CacheTuneCandidate {
            fresh_ttl_secs: fresh_ttl,
            stale_ttl_secs: stale_ttl,
            hits,
            stale_hits,
            misses,
            hit_rate: served as f64 / total,
            estimated_upstream_calls: misses,
        });
    }

    let run_id = new_run_id();
    let params_json = serde_json::json!({
        "fresh_ttl_secs": fresh,
        "stale_ttl_secs": stale,
        "event_count": events.len(),
    });

    let mut report = TuneReport::new("cache_tune", &params.session_id, &run_id);
    report.budget = params.budget;
    report.params_used = params_json.clone();
    report.candidates = candidates
        .iter()
        .filter_map(|c| serde_json::to_value(c).ok())
        .collect();
    report.metrics = serde_json::json!({
        "candidates": candidates.len(),
        "best_hit_rate": candidates.iter().map(|c| c.hit_rate).fold(0.0_f64, f64::max),
    });
    report.warnings = warnings.clone();

    store.append_run(
        &params.session_id,
        params.agent_id.as_deref(),
        TuneRunRecord {
            run_id: run_id.clone(),
            tool: "cache_tune".into(),
            params: params_json,
            metrics: report.metrics.clone(),
            selected: false,
        },
    )?;

    Ok(CacheTuneReport {
        tool: "cache_tune".into(),
        session_id: params.session_id,
        run_id,
        candidates,
        warnings,
        report,
    })
}

fn new_run_id() -> String {
    format!(
        "run-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    )
}
