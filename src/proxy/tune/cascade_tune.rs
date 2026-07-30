//! `cascade_tune` — dry-run cascade leg selection.

use serde::{Deserialize, Serialize};

use super::session::{TuneRunRecord, TuneSessionStore};
use super::{TuneBudget, TuneReport};

/// Synthetic per-upstream outcome for cascade dry-run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CascadeLegProbe {
    pub upstream_id: String,
    pub priority: u32,
    /// Best score observed (0-1).
    pub best_score: f32,
    pub result_count: usize,
    /// Estimated latency ms for this leg.
    #[serde(default)]
    pub latency_ms: u64,
}

/// Cascade tune params.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CascadeTuneParams {
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub context_id: Option<String>,
    /// Legs ordered by intended try order (priority ascending).
    pub legs: Vec<CascadeLegProbe>,
    #[serde(default)]
    pub min_score_threshold: Option<f32>,
    #[serde(default)]
    pub min_results: Option<usize>,
    #[serde(default)]
    pub max_cascade_depth: Option<usize>,
    /// Optional threshold sweep.
    #[serde(default)]
    pub min_score_sweep: Option<Vec<f32>>,
    #[serde(default)]
    pub budget: TuneBudget,
}

/// One cascade policy outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CascadeTuneCandidate {
    pub min_score_threshold: f32,
    pub min_results: usize,
    pub chosen_upstream_id: Option<String>,
    pub tried: Vec<String>,
    pub cascaded: bool,
    pub total_latency_ms: u64,
}

/// Cascade tune report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CascadeTuneReport {
    pub tool: String,
    pub session_id: String,
    pub run_id: String,
    pub candidates: Vec<CascadeTuneCandidate>,
    pub warnings: Vec<String>,
    pub report: TuneReport,
}

/// Dry-run which leg would serve under cascade thresholds.
///
/// # Errors
///
/// Session missing, empty legs, or oversized sweep.
pub fn cascade_tune(
    store: &TuneSessionStore,
    params: CascadeTuneParams,
) -> Result<CascadeTuneReport, String> {
    let _sess = store
        .get(
            &params.session_id,
            params.agent_id.as_deref(),
            params.context_id.as_deref(),
        )
        .ok_or_else(|| "session not found".to_string())?;

    if params.legs.is_empty() {
        return Err("legs must not be empty".into());
    }

    let min_results = params.min_results.unwrap_or(1);
    let max_depth = params.max_cascade_depth.unwrap_or(params.legs.len());

    let mut thresholds = params.min_score_sweep.clone().unwrap_or_default();
    if thresholds.is_empty() {
        thresholds.push(params.min_score_threshold.unwrap_or(0.7));
    }
    if thresholds.len() > params.budget.max_sweep_points {
        return Err(format!(
            "sweep has {} points; max is {}",
            thresholds.len(),
            params.budget.max_sweep_points
        ));
    }

    let mut legs = params.legs.clone();
    legs.sort_by_key(|l| l.priority);

    let mut candidates = Vec::with_capacity(thresholds.len());
    for &thresh in &thresholds {
        let mut tried = Vec::new();
        let mut total_latency = 0u64;
        let mut chosen = None;
        let mut cascaded = false;
        for (i, leg) in legs.iter().enumerate() {
            if tried.len() >= max_depth {
                break;
            }
            tried.push(leg.upstream_id.clone());
            total_latency = total_latency.saturating_add(leg.latency_ms);
            let ok = leg.best_score >= thresh && leg.result_count >= min_results;
            if ok {
                chosen = Some(leg.upstream_id.clone());
                cascaded = i > 0;
                break;
            }
        }
        candidates.push(CascadeTuneCandidate {
            min_score_threshold: thresh,
            min_results,
            chosen_upstream_id: chosen,
            tried,
            cascaded,
            total_latency_ms: total_latency,
        });
    }

    let run_id = new_run_id();
    let params_json = serde_json::json!({
        "min_score_threshold": thresholds.first().copied(),
        "min_score_sweep": thresholds,
        "min_results": min_results,
        "max_cascade_depth": max_depth,
        "legs": params.legs.iter().map(|l| &l.upstream_id).collect::<Vec<_>>(),
    });

    let mut report = TuneReport::new("cascade_tune", &params.session_id, &run_id);
    report.budget = params.budget;
    report.params_used = params_json.clone();
    report.candidates = candidates
        .iter()
        .filter_map(|c| serde_json::to_value(c).ok())
        .collect();
    report.metrics = serde_json::json!({
        "candidates": candidates.len(),
        "any_cascade": candidates.iter().any(|c| c.cascaded),
    });

    store.append_run(
        &params.session_id,
        params.agent_id.as_deref(),
        TuneRunRecord {
            run_id: run_id.clone(),
            tool: "cascade_tune".into(),
            params: params_json,
            metrics: report.metrics.clone(),
            selected: false,
        },
    )?;

    Ok(CascadeTuneReport {
        tool: "cascade_tune".into(),
        session_id: params.session_id,
        run_id,
        candidates,
        warnings: Vec::new(),
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
