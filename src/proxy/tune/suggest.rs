//! `scope_suggest` — propose weighted_phrases from hit texts.

#![allow(clippy::arithmetic_side_effects)] // tf/df counters on tokenized texts

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::session::{TuneRunRecord, TuneSessionStore};
use super::{TuneBudget, TuneReport};

/// Input for phrase suggestion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeSuggestParams {
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub context_id: Option<String>,
    /// Free-text contents (hits, dropped docs, etc.).
    pub texts: Vec<String>,
    #[serde(default = "default_max_phrases")]
    pub max_phrases: usize,
    #[serde(default)]
    pub budget: TuneBudget,
}

fn default_max_phrases() -> usize {
    8
}

/// One suggested phrase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestedPhrase {
    pub text: String,
    pub weight: f32,
    pub score: f32,
    pub rationale: String,
}

/// Suggest report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeSuggestReport {
    pub tool: String,
    pub session_id: String,
    pub run_id: String,
    pub phrases: Vec<SuggestedPhrase>,
    pub report: TuneReport,
}

/// Suggest phrases by simple term frequency / distinctiveness.
///
/// # Errors
///
/// Session missing or empty texts.
pub fn scope_suggest(
    store: &TuneSessionStore,
    params: ScopeSuggestParams,
) -> Result<ScopeSuggestReport, String> {
    let _sess = store
        .get(
            &params.session_id,
            params.agent_id.as_deref(),
            params.context_id.as_deref(),
        )
        .ok_or_else(|| "session not found".to_string())?;

    if params.texts.is_empty() {
        return Err("texts must not be empty".into());
    }

    let max_p = params.max_phrases.clamp(1, 32);
    let phrases = suggest_from_texts(&params.texts, max_p);

    let run_id = format!(
        "run-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let params_json = serde_json::json!({
        "weighted_phrases": phrases.iter().map(|p| {
            serde_json::json!({"text": p.text, "weight": p.weight})
        }).collect::<Vec<_>>(),
        "mode": "filter",
        "min_similarity": 0.25,
        "max_phrases": max_p,
        "text_count": params.texts.len(),
    });

    let mut report = TuneReport::new("scope_suggest", &params.session_id, &run_id);
    report.budget = params.budget;
    report.params_used = params_json.clone();
    report.candidates = phrases
        .iter()
        .map(|p| {
            serde_json::json!({
                "text": p.text,
                "weight": p.weight,
                "score": p.score,
                "rationale": p.rationale,
            })
        })
        .collect();
    report.metrics = serde_json::json!({ "suggested": phrases.len() });

    store.append_run(
        &params.session_id,
        params.agent_id.as_deref(),
        TuneRunRecord {
            run_id: run_id.clone(),
            tool: "scope_suggest".into(),
            params: params_json,
            metrics: report.metrics.clone(),
            selected: false,
        },
    )?;

    Ok(ScopeSuggestReport {
        tool: "scope_suggest".into(),
        session_id: params.session_id,
        run_id,
        phrases,
        report,
    })
}

fn suggest_from_texts(texts: &[String], max_phrases: usize) -> Vec<SuggestedPhrase> {
    let mut df: HashMap<String, usize> = HashMap::new();
    let mut tf: HashMap<String, usize> = HashMap::new();
    let n_docs = texts.len().max(1);

    for text in texts {
        let mut seen = std::collections::HashSet::new();
        for tok in tokenize(text) {
            *tf.entry(tok.clone()).or_insert(0) += 1;
            if seen.insert(tok.clone()) {
                *df.entry(tok).or_insert(0) += 1;
            }
        }
    }

    let mut scored: Vec<SuggestedPhrase> = tf
        .into_iter()
        .filter(|(t, _)| t.len() >= 3)
        .filter(|(t, _)| !STOP.contains(&t.as_str()))
        .map(|(text, term_freq)| {
            let doc_freq = *df.get(&text).unwrap_or(&1) as f32;
            // Prefer terms that appear often but not in every doc equally
            let idf = ((n_docs as f32) / doc_freq).ln().max(0.0) + 1.0;
            let score = (term_freq as f32) * idf;
            SuggestedPhrase {
                text,
                weight: 1.0,
                score,
                rationale: format!("tf={term_freq} df={doc_freq:.0} idf={idf:.2}"),
            }
        })
        .collect();

    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored.truncate(max_phrases);
    scored
}

fn tokenize(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

const STOP: &[&str] = &[
    "the", "and", "for", "with", "from", "that", "this", "are", "was", "were", "have", "has",
    "been", "not", "but", "you", "your", "all", "any", "can", "will", "into", "about", "over",
];
