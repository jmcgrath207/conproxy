//! `embed_tune` — dry-run embed batch shape / latency estimate.

#![allow(clippy::arithmetic_side_effects)] // latency estimates on bounded grids

use serde::{Deserialize, Serialize};

use super::session::{TuneRunRecord, TuneSessionStore};
use super::{TuneBudget, TuneReport};

/// Embed tune params (simulation from text counts + assumed per-item latency).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedTuneParams {
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub context_id: Option<String>,
    /// Number of texts to embed in the probe set.
    pub text_count: usize,
    /// Batch size candidates.
    #[serde(default)]
    pub batch_size_sweep: Option<Vec<usize>>,
    /// Assumed per-text latency ms (linear model).
    #[serde(default)]
    pub per_text_ms: Option<f64>,
    /// Fixed overhead per batch call ms.
    #[serde(default)]
    pub batch_overhead_ms: Option<f64>,
    #[serde(default)]
    pub budget: TuneBudget,
}

/// One batch-size candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedTuneCandidate {
    pub batch_size: usize,
    pub batches: usize,
    pub est_total_ms: f64,
    pub est_p50_batch_ms: f64,
    pub est_p95_batch_ms: f64,
}

/// Embed tune report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedTuneReport {
    pub tool: String,
    pub session_id: String,
    pub run_id: String,
    pub candidates: Vec<EmbedTuneCandidate>,
    pub warnings: Vec<String>,
    pub report: TuneReport,
}

/// Estimate embed latency under batch size grid. Pure math; no provider call.
///
/// # Errors
///
/// Session missing, zero texts, or oversized sweep.
pub fn embed_tune(
    store: &TuneSessionStore,
    params: EmbedTuneParams,
) -> Result<EmbedTuneReport, String> {
    let _sess = store
        .get(
            &params.session_id,
            params.agent_id.as_deref(),
            params.context_id.as_deref(),
        )
        .ok_or_else(|| "session not found".to_string())?;

    if params.text_count == 0 {
        return Err("text_count must be > 0".into());
    }

    let mut warnings = vec!["simulation only — no live embed provider call".into()];
    let per_text = params.per_text_ms.unwrap_or(2.0).max(0.01);
    let overhead = params.batch_overhead_ms.unwrap_or(15.0).max(0.0);

    let mut sizes = params.batch_size_sweep.clone().unwrap_or_default();
    if sizes.is_empty() {
        sizes = vec![1, 8, 16, 32];
    }
    if sizes.len() > params.budget.max_sweep_points {
        return Err(format!(
            "sweep has {} points; max is {}",
            sizes.len(),
            params.budget.max_sweep_points
        ));
    }

    let n = params.text_count;
    let mut candidates = Vec::with_capacity(sizes.len());
    for &bs in &sizes {
        let bs = bs.max(1);
        let batches = n.div_ceil(bs);
        let last = n % bs;
        let full_batch_ms = overhead + (bs as f64) * per_text;
        let last_batch_ms = if last == 0 {
            full_batch_ms
        } else {
            overhead + (last as f64) * per_text
        };
        let est_total = if batches <= 1 {
            last_batch_ms
        } else {
            full_batch_ms * ((batches - 1) as f64) + last_batch_ms
        };
        // Sequential model: p50 ≈ median batch, p95 ≈ max batch * 1.2 jitter
        candidates.push(EmbedTuneCandidate {
            batch_size: bs,
            batches,
            est_total_ms: est_total,
            est_p50_batch_ms: full_batch_ms,
            est_p95_batch_ms: full_batch_ms * 1.2,
        });
    }

    if candidates.iter().any(|c| c.est_total_ms > 30_000.0) {
        warnings.push("some candidates exceed 30s wall estimate".into());
    }

    let run_id = new_run_id();
    let params_json = serde_json::json!({
        "text_count": n,
        "batch_size_sweep": sizes,
        "per_text_ms": per_text,
        "batch_overhead_ms": overhead,
    });

    let mut report = TuneReport::new("embed_tune", &params.session_id, &run_id);
    report.budget = params.budget;
    report.params_used = params_json.clone();
    report.candidates = candidates
        .iter()
        .filter_map(|c| serde_json::to_value(c).ok())
        .collect();
    report.metrics = serde_json::json!({
        "candidates": candidates.len(),
        "best_total_ms": candidates.iter().map(|c| c.est_total_ms).fold(f64::INFINITY, f64::min),
    });
    report.warnings = warnings.clone();

    store.append_run(
        &params.session_id,
        params.agent_id.as_deref(),
        TuneRunRecord {
            run_id: run_id.clone(),
            tool: "embed_tune".into(),
            params: params_json,
            metrics: report.metrics.clone(),
            selected: false,
        },
    )?;

    Ok(EmbedTuneReport {
        tool: "embed_tune".into(),
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
