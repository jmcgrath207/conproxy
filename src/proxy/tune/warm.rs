//! `warm_tune` — warm plan ETA / overlap (execute off by default).

#![allow(clippy::arithmetic_side_effects)] // set sizes / ETA on plan keys

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::session::{TuneRunRecord, TuneSessionStore};
use super::{TuneBudget, TuneReport};

/// Warm tune params.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarmTuneParams {
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub context_id: Option<String>,
    /// Keys/queries planned for warm.
    pub plan_keys: Vec<String>,
    /// Keys already present in cache (for overlap).
    #[serde(default)]
    pub cached_keys: Vec<String>,
    /// Assumed per-key warm latency ms.
    #[serde(default)]
    pub per_key_ms: Option<u64>,
    /// Concurrency candidates for ETA.
    #[serde(default)]
    pub concurrency_sweep: Option<Vec<usize>>,
    /// When true, would execute warm — **rejected in v1** (dry-run only).
    #[serde(default)]
    pub execute: bool,
    #[serde(default)]
    pub budget: TuneBudget,
}

/// One concurrency candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarmTuneCandidate {
    pub concurrency: usize,
    pub keys_to_warm: usize,
    pub already_cached: usize,
    pub est_eta_ms: u64,
}

/// Warm tune report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarmTuneReport {
    pub tool: String,
    pub session_id: String,
    pub run_id: String,
    pub keys_to_warm: Vec<String>,
    pub already_cached: usize,
    pub candidates: Vec<WarmTuneCandidate>,
    pub execute_requested: bool,
    pub execute_performed: bool,
    pub warnings: Vec<String>,
    pub report: TuneReport,
}

/// Plan warm ETA and cache overlap. `execute: true` is rejected (documented).
///
/// # Errors
///
/// Session missing, empty plan, execute requested, or oversized sweep.
pub fn warm_tune(
    store: &TuneSessionStore,
    params: WarmTuneParams,
) -> Result<WarmTuneReport, String> {
    let _sess = store
        .get(
            &params.session_id,
            params.agent_id.as_deref(),
            params.context_id.as_deref(),
        )
        .ok_or_else(|| "session not found".to_string())?;

    if params.plan_keys.is_empty() {
        return Err("plan_keys must not be empty".into());
    }
    if params.execute {
        return Err(
            "execute=true not supported in v1; dry-run plan only (set execute=false)".into(),
        );
    }

    let cached: HashSet<&str> = params.cached_keys.iter().map(String::as_str).collect();
    let to_warm: Vec<String> = params
        .plan_keys
        .iter()
        .filter(|k| !cached.contains(k.as_str()))
        .cloned()
        .collect();
    let already = params.plan_keys.len() - to_warm.len();
    let per_key = params.per_key_ms.unwrap_or(50).max(1);

    let mut conc = params.concurrency_sweep.clone().unwrap_or_default();
    if conc.is_empty() {
        conc = vec![1, 4, 8];
    }
    if conc.len() > params.budget.max_sweep_points {
        return Err(format!(
            "sweep has {} points; max is {}",
            conc.len(),
            params.budget.max_sweep_points
        ));
    }

    let n = to_warm.len();
    let mut candidates = Vec::with_capacity(conc.len());
    for &c in &conc {
        let c = c.max(1);
        let waves = n.div_ceil(c);
        candidates.push(WarmTuneCandidate {
            concurrency: c,
            keys_to_warm: n,
            already_cached: already,
            est_eta_ms: (waves as u64).saturating_mul(per_key),
        });
    }

    let warnings = vec!["execute defaults false; no warm performed".into()];
    let run_id = new_run_id();
    let params_json = serde_json::json!({
        "plan_count": params.plan_keys.len(),
        "to_warm": n,
        "already_cached": already,
        "concurrency_sweep": conc,
        "per_key_ms": per_key,
        "execute": false,
    });

    let mut report = TuneReport::new("warm_tune", &params.session_id, &run_id);
    report.budget = params.budget;
    report.params_used = params_json.clone();
    report.candidates = candidates
        .iter()
        .filter_map(|c| serde_json::to_value(c).ok())
        .collect();
    report.metrics = serde_json::json!({
        "keys_to_warm": n,
        "already_cached": already,
        "best_eta_ms": candidates.iter().map(|c| c.est_eta_ms).min().unwrap_or(0),
    });
    report.warnings = warnings.clone();

    store.append_run(
        &params.session_id,
        params.agent_id.as_deref(),
        TuneRunRecord {
            run_id: run_id.clone(),
            tool: "warm_tune".into(),
            params: params_json,
            metrics: report.metrics.clone(),
            selected: false,
        },
    )?;

    Ok(WarmTuneReport {
        tool: "warm_tune".into(),
        session_id: params.session_id,
        run_id,
        keys_to_warm: to_warm,
        already_cached: already,
        candidates,
        execute_requested: false,
        execute_performed: false,
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
