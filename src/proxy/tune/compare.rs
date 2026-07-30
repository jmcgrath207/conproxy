//! `compare_runs` — diff two runs in the same session.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::session::{TuneRunRecord, TuneSessionStore};

/// Compare request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompareRequest {
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub context_id: Option<String>,
    pub run_id_a: String,
    pub run_id_b: String,
}

/// Compare report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompareReport {
    pub tool: String,
    pub session_id: String,
    pub run_a: TuneRunRecord,
    pub run_b: TuneRunRecord,
    pub param_diff: BTreeMap<String, serde_json::Value>,
    pub metric_diff: BTreeMap<String, serde_json::Value>,
}

/// Diff two runs; same session only.
///
/// # Errors
///
/// Session or run missing.
pub fn compare_runs(
    store: &TuneSessionStore,
    req: CompareRequest,
) -> Result<CompareReport, String> {
    let sess = store
        .get(
            &req.session_id,
            req.agent_id.as_deref(),
            req.context_id.as_deref(),
        )
        .ok_or_else(|| "session not found".to_string())?;

    let run_a = sess
        .runs
        .iter()
        .find(|r| r.run_id == req.run_id_a)
        .cloned()
        .ok_or_else(|| format!("unknown run_id: {}", req.run_id_a))?;
    let run_b = sess
        .runs
        .iter()
        .find(|r| r.run_id == req.run_id_b)
        .cloned()
        .ok_or_else(|| format!("unknown run_id: {}", req.run_id_b))?;

    let param_diff = flat_diff(&run_a.params, &run_b.params);
    let metric_diff = flat_diff(&run_a.metrics, &run_b.metrics);

    Ok(CompareReport {
        tool: "compare_runs".into(),
        session_id: req.session_id,
        run_a,
        run_b,
        param_diff,
        metric_diff,
    })
}

fn flat_diff(a: &serde_json::Value, b: &serde_json::Value) -> BTreeMap<String, serde_json::Value> {
    let mut out = BTreeMap::new();
    let map_a = a.as_object();
    let map_b = b.as_object();
    match (map_a, map_b) {
        (Some(ma), Some(mb)) => {
            let mut keys: std::collections::BTreeSet<&String> = ma.keys().collect();
            keys.extend(mb.keys());
            for k in keys {
                let va = ma.get(k);
                let vb = mb.get(k);
                if va != vb {
                    out.insert(k.clone(), serde_json::json!({ "a": va, "b": vb }));
                }
            }
        }
        _ => {
            if a != b {
                out.insert("_".into(), serde_json::json!({ "a": a, "b": b }));
            }
        }
    }
    out
}
