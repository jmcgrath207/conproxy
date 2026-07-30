//! `federated_tune` — dry-run local/remote merge weight preview.

#![allow(clippy::arithmetic_side_effects)] // count diffs on truncated top_k

use serde::{Deserialize, Serialize};

use super::session::{TuneRunRecord, TuneSessionStore};
use super::{TuneBudget, TuneReport};

/// One hit with source tag for merge preview.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedHit {
    pub id: String,
    pub score: f32,
    /// `local` or `remote`
    pub source: String,
}

/// Federated tune params.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederatedTuneParams {
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub context_id: Option<String>,
    pub hits: Vec<FedHit>,
    /// Local weight candidates in blended score: `w*local + (1-w)*remote` per side.
    /// Applied as multiplier on local scores; remote gets `(1-w)`.
    #[serde(default)]
    pub local_weight_sweep: Option<Vec<f32>>,
    #[serde(default)]
    pub top_k: Option<usize>,
    #[serde(default)]
    pub budget: TuneBudget,
}

/// One merge candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederatedTuneCandidate {
    pub local_weight: f32,
    pub ordered_ids: Vec<String>,
    pub local_count: usize,
    pub remote_count: usize,
    pub top_id: Option<String>,
}

/// Federated tune report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederatedTuneReport {
    pub tool: String,
    pub session_id: String,
    pub run_id: String,
    pub candidates: Vec<FederatedTuneCandidate>,
    pub warnings: Vec<String>,
    pub report: TuneReport,
}

/// Preview merge order under local_weight grid. Dry-run only.
///
/// # Errors
///
/// Session missing, empty hits, or oversized sweep.
pub fn federated_tune(
    store: &TuneSessionStore,
    params: FederatedTuneParams,
) -> Result<FederatedTuneReport, String> {
    let _sess = store
        .get(
            &params.session_id,
            params.agent_id.as_deref(),
            params.context_id.as_deref(),
        )
        .ok_or_else(|| "session not found".to_string())?;

    if params.hits.is_empty() {
        return Err(
            "hits must not be empty — pass [{id, score, source}] where source is \"local\" or \"remote\"; \
             federated_tune does not fetch upstream. Call search first, then tag each \
             hit with its origin."
                .into(),
        );
    }

    let top_k = params
        .top_k
        .unwrap_or(10)
        .clamp(1, params.budget.max_results);
    let mut weights = params.local_weight_sweep.clone().unwrap_or_default();
    if weights.is_empty() {
        weights = vec![0.3, 0.5, 0.7];
    }
    if weights.len() > params.budget.max_sweep_points {
        return Err(format!(
            "sweep has {} points; max is {}",
            weights.len(),
            params.budget.max_sweep_points
        ));
    }

    let mut candidates = Vec::with_capacity(weights.len());
    for &w in &weights {
        let w = w.clamp(0.0, 1.0);
        let mut scored: Vec<(String, f32, bool)> = params
            .hits
            .iter()
            .map(|h| {
                let is_local = h.source.eq_ignore_ascii_case("local");
                let adj = if is_local {
                    h.score * w
                } else {
                    h.score * (1.0 - w)
                };
                (h.id.clone(), adj, is_local)
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);
        let local_count = scored.iter().filter(|(_, _, l)| *l).count();
        let remote_count = scored.len() - local_count;
        let top_id = scored.first().map(|(id, _, _)| id.clone());
        let ordered_ids = scored.into_iter().map(|(id, _, _)| id).collect();
        candidates.push(FederatedTuneCandidate {
            local_weight: w,
            ordered_ids,
            local_count,
            remote_count,
            top_id,
        });
    }

    let run_id = new_run_id();
    let params_json = serde_json::json!({
        "local_weight_sweep": weights,
        "top_k": top_k,
        "hit_count": params.hits.len(),
    });

    let mut report = TuneReport::new("federated_tune", &params.session_id, &run_id);
    report.budget = params.budget;
    report.params_used = params_json.clone();
    report.candidates = candidates
        .iter()
        .filter_map(|c| serde_json::to_value(c).ok())
        .collect();
    report.metrics = serde_json::json!({ "candidates": candidates.len() });

    store.append_run(
        &params.session_id,
        params.agent_id.as_deref(),
        TuneRunRecord {
            run_id: run_id.clone(),
            tool: "federated_tune".into(),
            params: params_json,
            metrics: report.metrics.clone(),
            selected: false,
        },
    )?;

    Ok(FederatedTuneReport {
        tool: "federated_tune".into(),
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
