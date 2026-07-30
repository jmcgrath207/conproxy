//! `rate_limit_tune` — token-bucket allow/deny simulation.

#![allow(clippy::arithmetic_side_effects)] // allow/deny counters on synthetic arrivals

use serde::{Deserialize, Serialize};

use super::session::{TuneRunRecord, TuneSessionStore};
use super::{TuneBudget, TuneReport};

/// Rate limit tune params (synthetic timestamps only).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitTuneParams {
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub context_id: Option<String>,
    /// Request arrival times in milliseconds from t=0.
    pub arrival_ms: Vec<u64>,
    /// RPS candidates.
    #[serde(default)]
    pub rps_sweep: Option<Vec<f64>>,
    /// Burst size candidates.
    #[serde(default)]
    pub burst_sweep: Option<Vec<u32>>,
    #[serde(default)]
    pub budget: TuneBudget,
}

/// One RPS/burst candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitTuneCandidate {
    pub rps: f64,
    pub burst: u32,
    pub allowed: usize,
    pub denied: usize,
    pub allow_rate: f64,
}

/// Rate limit tune report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitTuneReport {
    pub tool: String,
    pub session_id: String,
    pub run_id: String,
    pub candidates: Vec<RateLimitTuneCandidate>,
    pub warnings: Vec<String>,
    pub report: TuneReport,
}

/// Simulate token bucket over arrival timestamps. Does not throttle live traffic.
///
/// # Errors
///
/// Session missing, empty arrivals, or oversized grid.
pub fn rate_limit_tune(
    store: &TuneSessionStore,
    params: RateLimitTuneParams,
) -> Result<RateLimitTuneReport, String> {
    let _sess = store
        .get(
            &params.session_id,
            params.agent_id.as_deref(),
            params.context_id.as_deref(),
        )
        .ok_or_else(|| "session not found".to_string())?;

    if params.arrival_ms.is_empty() {
        return Err("arrival_ms must not be empty".into());
    }

    let mut warnings = vec!["simulation only — does not throttle live traffic".into()];
    let arrivals: Vec<u64> = params
        .arrival_ms
        .iter()
        .take(params.budget.max_results.saturating_mul(10).max(256))
        .copied()
        .collect();
    if params.arrival_ms.len() > arrivals.len() {
        warnings.push(format!(
            "truncated arrivals {} → {}",
            params.arrival_ms.len(),
            arrivals.len()
        ));
    }

    let rps_list = params
        .rps_sweep
        .clone()
        .unwrap_or_else(|| vec![10.0, 50.0, 100.0]);
    let burst_list = params
        .burst_sweep
        .clone()
        .unwrap_or_else(|| vec![1, 10, 50]);

    let mut grid = Vec::new();
    for &rps in &rps_list {
        for &burst in &burst_list {
            grid.push((rps, burst));
        }
    }
    if grid.len() > params.budget.max_sweep_points {
        return Err(format!(
            "grid has {} points; max is {}",
            grid.len(),
            params.budget.max_sweep_points
        ));
    }

    let mut candidates = Vec::with_capacity(grid.len());
    for (rps, burst) in grid {
        let rps = rps.max(0.001);
        let burst = burst.max(1) as f64;
        let mut tokens = burst;
        let mut last_ms = 0u64;
        let mut allowed = 0usize;
        let mut denied = 0usize;
        for &t in &arrivals {
            let dt = t.saturating_sub(last_ms) as f64 / 1000.0;
            tokens = (tokens + dt * rps).min(burst);
            last_ms = t;
            if tokens >= 1.0 {
                tokens -= 1.0;
                allowed += 1;
            } else {
                denied += 1;
            }
        }
        let total = arrivals.len().max(1) as f64;
        candidates.push(RateLimitTuneCandidate {
            rps,
            burst: burst as u32,
            allowed,
            denied,
            allow_rate: allowed as f64 / total,
        });
    }

    let run_id = new_run_id();
    let params_json = serde_json::json!({
        "rps_sweep": rps_list,
        "burst_sweep": burst_list,
        "arrival_count": arrivals.len(),
    });

    let mut report = TuneReport::new("rate_limit_tune", &params.session_id, &run_id);
    report.budget = params.budget;
    report.params_used = params_json.clone();
    report.candidates = candidates
        .iter()
        .filter_map(|c| serde_json::to_value(c).ok())
        .collect();
    report.metrics = serde_json::json!({
        "candidates": candidates.len(),
        "best_allow_rate": candidates.iter().map(|c| c.allow_rate).fold(0.0_f64, f64::max),
    });
    report.warnings = warnings.clone();

    store.append_run(
        &params.session_id,
        params.agent_id.as_deref(),
        TuneRunRecord {
            run_id: run_id.clone(),
            tool: "rate_limit_tune".into(),
            params: params_json,
            metrics: report.metrics.clone(),
            selected: false,
        },
    )?;

    Ok(RateLimitTuneReport {
        tool: "rate_limit_tune".into(),
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
