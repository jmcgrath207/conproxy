//! `scope_tune` — dry-run Score C filter/boost/rerank + min_sim sweep.

use serde::{Deserialize, Serialize};

use crate::config::{ProxyScopeConfig, WeightedPhrase};
use crate::proxy::scope::ScopeFilter;
use crate::proxy::types::SearchResult;

use super::session::{TuneRunRecord, TuneSessionStore};
use super::{TuneBudget, TuneReport};

/// Default max sweep points (also in [`TuneBudget::max_sweep_points`]).
pub const DEFAULT_MAX_SWEEP_POINTS: usize = 16;

/// Parameters for a scope tune call (lib + MCP).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeTuneParams {
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub context_id: Option<String>,
    /// Upstream (or cached) hits to score — caller supplies; dry-run does not fetch.
    pub hits: Vec<SearchResult>,
    #[serde(default)]
    pub weighted_phrases: Vec<WeightedPhrase>,
    #[serde(default)]
    pub mode: Option<String>,
    /// Single threshold (used when sweep is empty).
    #[serde(default)]
    pub min_similarity: Option<f32>,
    /// Sweep grid for min_similarity (filter Score C).
    #[serde(default)]
    pub min_similarity_sweep: Option<Vec<f32>>,
    #[serde(default)]
    pub scope_weight: Option<f32>,
    #[serde(default)]
    pub lexical_weight: Option<f32>,
    #[serde(default)]
    pub budget: TuneBudget,
}

/// One row in a min_similarity sweep table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweepPointResult {
    pub min_similarity: f32,
    pub kept: usize,
    pub dropped: usize,
    pub kept_ids: Vec<String>,
    pub dropped_ids: Vec<String>,
}

/// Full scope tune outcome (also recorded on the session).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeTuneReport {
    pub tool: String,
    pub session_id: String,
    pub run_id: String,
    pub mode: String,
    pub baseline_count: usize,
    pub baseline_ids: Vec<String>,
    pub sweep: Vec<SweepPointResult>,
    /// Primary candidate (first sweep point or single threshold).
    pub primary_kept: usize,
    pub primary_dropped: usize,
    pub warnings: Vec<String>,
    pub report: TuneReport,
}

/// Run scope_tune against supplied hits; append run to session.
///
/// # Errors
///
/// Session missing/forbidden, empty hits, or sweep over budget.
pub fn scope_tune(
    store: &TuneSessionStore,
    params: ScopeTuneParams,
) -> Result<ScopeTuneReport, String> {
    let sess = store
        .get(
            &params.session_id,
            params.agent_id.as_deref(),
            params.context_id.as_deref(),
        )
        .ok_or_else(|| "session not found".to_string())?;

    let mut warnings = Vec::new();
    if params.hits.is_empty() {
        return Err(
            "hits must not be empty — pass search results mapped to [{id, content, score}]; \
             scope_tune does not fetch upstream. Run tune_workflow (composite) to \
             search + tune in one call, or call search first."
                .into(),
        );
    }
    if params.hits.len() > params.budget.max_results {
        warnings.push(format!(
            "truncating hits {} → {}",
            params.hits.len(),
            params.budget.max_results
        ));
    }
    let hits: Vec<SearchResult> = params
        .hits
        .iter()
        .take(params.budget.max_results)
        .cloned()
        .collect();

    let mode = params.mode.clone().unwrap_or_else(|| "filter".into());

    let mut thresholds = params.min_similarity_sweep.clone().unwrap_or_default();
    if thresholds.is_empty() {
        thresholds.push(params.min_similarity.unwrap_or(0.25));
    }
    if thresholds.len() > params.budget.max_sweep_points {
        return Err(format!(
            "sweep has {} points; max is {}",
            thresholds.len(),
            params.budget.max_sweep_points
        ));
    }

    let baseline_ids: Vec<String> = hits.iter().map(|h| h.id.clone()).collect();
    let baseline_count = baseline_ids.len();

    let mut sweep = Vec::with_capacity(thresholds.len());
    for &ms in &thresholds {
        let cfg = build_scope_config(&params, &mode, ms);
        let filter = ScopeFilter::from_config(&cfg);
        let kept = filter.filter_results(hits.clone());
        let kept_ids: Vec<String> = kept.iter().map(|r| r.id.clone()).collect();
        let kept_set: std::collections::HashSet<&str> =
            kept_ids.iter().map(String::as_str).collect();
        let dropped_ids: Vec<String> = hits
            .iter()
            .filter(|h| !kept_set.contains(h.id.as_str()))
            .map(|h| h.id.clone())
            .collect();
        sweep.push(SweepPointResult {
            min_similarity: ms,
            kept: kept_ids.len(),
            dropped: dropped_ids.len(),
            kept_ids,
            dropped_ids,
        });
    }

    let primary = sweep.first().cloned().unwrap_or(SweepPointResult {
        min_similarity: 0.0,
        kept: 0,
        dropped: 0,
        kept_ids: vec![],
        dropped_ids: vec![],
    });

    let run_id = format!(
        "run-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let params_json = serde_json::json!({
        "mode": mode,
        "min_similarity": primary.min_similarity,
        "min_similarity_sweep": thresholds,
        "weighted_phrases": params.weighted_phrases,
        "scope_weight": params.scope_weight,
        "lexical_weight": params.lexical_weight,
    });

    let mut report = TuneReport::new("scope_tune", &params.session_id, &run_id);
    report.budget = params.budget.clone();
    report.params_used = params_json.clone();
    report.baseline = Some(serde_json::json!({
        "count": baseline_count,
        "ids": baseline_ids,
    }));
    report.candidates = sweep
        .iter()
        .map(|p| {
            serde_json::json!({
                "min_similarity": p.min_similarity,
                "kept": p.kept,
                "dropped": p.dropped,
                "kept_ids": p.kept_ids,
                "dropped_ids": p.dropped_ids,
            })
        })
        .collect();
    report.metrics = serde_json::json!({
        "primary_kept": primary.kept,
        "primary_dropped": primary.dropped,
        "sweep_points": sweep.len(),
    });
    report.warnings = warnings.clone();

    store.append_run(
        &params.session_id,
        params.agent_id.as_deref(),
        TuneRunRecord {
            run_id: run_id.clone(),
            tool: "scope_tune".into(),
            params: params_json,
            metrics: report.metrics.clone(),
            selected: false,
        },
    )?;

    // silence unused sess warning path — ownership checked above
    let _ = sess.context_id;

    Ok(ScopeTuneReport {
        tool: "scope_tune".into(),
        session_id: params.session_id,
        run_id,
        mode,
        baseline_count,
        baseline_ids,
        sweep,
        primary_kept: primary.kept,
        primary_dropped: primary.dropped,
        warnings,
        report,
    })
}

fn build_scope_config(
    params: &ScopeTuneParams,
    mode: &str,
    min_similarity: f32,
) -> ProxyScopeConfig {
    ProxyScopeConfig {
        weighted_phrases: params.weighted_phrases.clone(),
        mode: Some(mode.to_string()),
        min_seed_similarity: Some(min_similarity),
        seed_weight: params.scope_weight,
        lexical_weight: params.lexical_weight,
        ..Default::default()
    }
}
