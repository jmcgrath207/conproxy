//! `benchmark` — evaluate if a query improved/degraded by tuning.
//!
//! Pure functions; no I/O. The MCP handler does the live query + applies
//! `ScopeFilter`, then calls [`evaluate`] for the diff + verdict.
//!
//! Verdict heuristic — three-signal intent-relative:
//! 1. **Eviction guardrail**: any baseline top-K result evicted → `degraded`.
//! 2. **Surface novelty**: new results in tuned top-K → `improved` (unless
//!    paired with an eviction, which keeps degraded).
//! 3. **Stability** (no eviction, no novelty): `unchanged`.
//!
//! No golden set, no relevance labels — "improvement" is relative to the
//! session's own declared phrases (intent alignment via the diff signal).

use crate::config::WeightedPhrase;
use crate::proxy::types::SearchResult;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Single hit entry inside a benchmark report (baseline or tuned).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkHit {
    pub id: String,
    pub score: f32,
    pub rank: usize,
    pub content: String,
}

/// Movement of one result between baseline and tuned rankings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Movement {
    pub id: String,
    /// One of: `"new"`, `"evicted"`, `"up"`, `"down"`, `"stable"`.
    pub status: String,
    pub baseline_rank: Option<usize>,
    pub tuned_rank: Option<usize>,
    pub baseline_score: f32,
    pub tuned_score: f32,
    pub score_delta: f32,
}

/// Verdict for a single query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    /// `"improved"`, `"degraded"`, `"unchanged"`, or `"changed"`.
    pub label: String,
    /// Human-readable explanation of the verdict.
    pub reason: String,
}

/// Full benchmark report for one query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReport {
    pub query: String,
    pub top_k: usize,
    pub verdict: Verdict,
    pub baseline_count: usize,
    pub tuned_count: usize,
    pub overlapping_count: usize,
    /// Per-id movements.
    pub movements: Vec<Movement>,
    /// Baseline top-K hits.
    pub baseline: Vec<BenchmarkHit>,
    /// Tuned top-K hits.
    pub tuned: Vec<BenchmarkHit>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Evaluate whether a query improved or degraded after applying tuned params.
///
/// # Arguments
///
/// * `query` — The search query (copied into the report for context).
/// * `baseline` — Results from live proxy with current config.
/// * `tuned` — Same baseline re-scored with tuned scope params.
/// * `top_k` — Number of results to compare (truncates both sides).
/// * `_phrases` — Tuned weighted phrases from the session. Reserved for
///   future enhanced intent-alignment scoring.
///
/// # Returns
///
/// A [`BenchmarkReport`] with per-id movements, verdict, and hit details.
#[must_use]
pub fn evaluate(
    query: &str,
    baseline: &[SearchResult],
    tuned: &[SearchResult],
    top_k: usize,
    _phrases: &[WeightedPhrase],
) -> BenchmarkReport {
    let baseline_top: Vec<&SearchResult> = baseline.iter().take(top_k).collect();
    let tuned_top: Vec<&SearchResult> = tuned.iter().take(top_k).collect();

    let baseline_ids: HashSet<&str> = baseline_top.iter().map(|r| r.id.as_str()).collect();
    let tuned_ids: HashSet<&str> = tuned_top.iter().map(|r| r.id.as_str()).collect();

    // --- Movements ---
    let mut movements: Vec<Movement> =
        Vec::with_capacity(baseline_top.len().saturating_add(tuned_top.len()));

    for b in &baseline_top {
        let b_pos = baseline_top.iter().position(|r| r.id == b.id).unwrap_or(0);
        if let Some(t_pos) = tuned_top.iter().position(|r| r.id == b.id) {
            let t_score = tuned_top.get(t_pos).map_or(0.0, |r| r.score);
            let b_rank = b_pos.saturating_add(1);
            let t_rank = t_pos.saturating_add(1);
            let status = if t_rank < b_rank {
                "up"
            } else if t_rank > b_rank {
                "down"
            } else {
                "stable"
            };
            movements.push(Movement {
                id: b.id.clone(),
                status: status.to_string(),
                baseline_rank: Some(b_rank),
                tuned_rank: Some(t_rank),
                baseline_score: b.score,
                tuned_score: t_score,
                score_delta: t_score - b.score,
            });
        } else {
            movements.push(Movement {
                id: b.id.clone(),
                status: "evicted".to_string(),
                baseline_rank: Some(b_pos.saturating_add(1)),
                tuned_rank: None,
                baseline_score: b.score,
                tuned_score: 0.0,
                score_delta: -b.score,
            });
        }
    }

    for t in &tuned_top {
        if !baseline_ids.contains(t.id.as_str()) {
            let t_pos = tuned_top.iter().position(|r| r.id == t.id).unwrap_or(0);
            movements.push(Movement {
                id: t.id.clone(),
                status: "new".to_string(),
                baseline_rank: None,
                tuned_rank: Some(t_pos.saturating_add(1)),
                baseline_score: 0.0,
                tuned_score: t.score,
                score_delta: t.score,
            });
        }
    }

    let overlapping_count = baseline_ids.intersection(&tuned_ids).count();

    // --- Verdict ---
    let evicted_count = movements.iter().filter(|m| m.status == "evicted").count();
    let new_count = movements.iter().filter(|m| m.status == "new").count();

    let (label, reason) = if evicted_count > 0 {
        let evicted_ids: Vec<&str> = movements
            .iter()
            .filter(|m| m.status == "evicted")
            .map(|m| m.id.as_str())
            .collect();
        (
            "degraded".to_string(),
            format!(
                "{} result{} evicted from top-{}: {}",
                evicted_count,
                if evicted_count == 1 {
                    " evicted"
                } else {
                    "s evicted"
                },
                top_k,
                evicted_ids.join(", "),
            ),
        )
    } else if new_count > 0 && overlapping_count > 0 {
        let new_ids: Vec<&str> = movements
            .iter()
            .filter(|m| m.status == "new")
            .map(|m| m.id.as_str())
            .collect();
        (
            "improved".to_string(),
            format!(
                "{} new result{} surfaced in top-{}: {}",
                new_count,
                if new_count == 1 { "" } else { "s" },
                top_k,
                new_ids.join(", "),
            ),
        )
    } else if new_count > 0 && overlapping_count == 0 {
        (
            "changed".to_string(),
            format!(
                "complete replacement: {} new result{} in top-{}",
                new_count,
                if new_count == 1 { "" } else { "s" },
                top_k,
            ),
        )
    } else {
        (
            "unchanged".to_string(),
            format!(
                "all {} baseline result{} remain in top-{} at same ranks",
                overlapping_count,
                if overlapping_count == 1 { "" } else { "s" },
                top_k,
            ),
        )
    };

    // --- Format hits ---
    let baseline_hits: Vec<BenchmarkHit> = baseline_top
        .iter()
        .enumerate()
        .map(|(i, r)| BenchmarkHit {
            id: r.id.clone(),
            score: r.score,
            rank: i.saturating_add(1),
            content: truncate(&r.content, 120),
        })
        .collect();

    let tuned_hits: Vec<BenchmarkHit> = tuned_top
        .iter()
        .enumerate()
        .map(|(i, r)| BenchmarkHit {
            id: r.id.clone(),
            score: r.score,
            rank: i.saturating_add(1),
            content: truncate(&r.content, 120),
        })
        .collect();

    BenchmarkReport {
        query: query.to_string(),
        top_k,
        verdict: Verdict { label, reason },
        baseline_count: baseline_top.len(),
        tuned_count: tuned_top.len(),
        overlapping_count,
        movements,
        baseline: baseline_hits,
        tuned: tuned_hits,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut out = String::with_capacity(max.saturating_add(3));
        out.push_str(&s[..max]);
        out.push_str("...");
        out
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_hit(id: &str, score: f32, content: &str) -> SearchResult {
        SearchResult {
            id: id.to_string(),
            score,
            content: content.to_string(),
            metadata: None,
            upstream_id: None,
        }
    }

    fn no_phrases() -> Vec<WeightedPhrase> {
        vec![]
    }

    #[test]
    fn test_unchanged_when_identical() {
        let hits = vec![make_hit("a", 1.0, "foo"), make_hit("b", 0.5, "bar")];
        let report = evaluate("test", &hits, &hits, 5, &no_phrases());
        assert_eq!(report.verdict.label, "unchanged");
        assert_eq!(report.overlapping_count, 2);
        assert_eq!(report.movements.len(), 2);
        assert!(report.movements.iter().all(|m| m.status == "stable"));
    }

    #[test]
    fn test_improved_when_new_surfaced() {
        // Baseline is a subset of tuned — no eviction, one new result.
        let baseline = vec![make_hit("a", 1.0, "foo")];
        let tuned = vec![make_hit("a", 1.0, "foo"), make_hit("b", 0.8, "baz")];
        let report = evaluate("test", &baseline, &tuned, 5, &no_phrases());
        assert_eq!(report.verdict.label, "improved");
        assert_eq!(report.overlapping_count, 1);
        // a stable (rank 1→1), b new (rank 2)
    }

    #[test]
    fn test_degraded_when_evicted() {
        let baseline = vec![make_hit("a", 1.0, "foo"), make_hit("b", 0.9, "bar")];
        let tuned = vec![make_hit("c", 0.4, "baz")];
        let report = evaluate("test", &baseline, &tuned, 5, &no_phrases());
        assert_eq!(report.verdict.label, "degraded");
        assert!(report.verdict.reason.contains("evicted"));
    }

    #[test]
    fn test_eviction_guardrail_overrides_improve() {
        // Even though c is new (good), b was evicted (bad) → degraded wins
        let baseline = vec![make_hit("a", 1.0, "foo"), make_hit("b", 0.9, "bar")];
        let tuned = vec![make_hit("a", 0.8, "foo"), make_hit("c", 0.7, "baz")];
        let report = evaluate("test", &baseline, &tuned, 5, &no_phrases());
        assert_eq!(
            report.verdict.label, "degraded",
            "eviction guardrail should override improvement"
        );
    }

    #[test]
    fn test_top_k_truncation() {
        let baseline = vec![
            make_hit("a", 1.0, ""),
            make_hit("b", 0.9, ""),
            make_hit("c", 0.8, ""),
        ];
        let tuned = vec![
            make_hit("b", 1.0, ""),
            make_hit("a", 0.5, ""),
            make_hit("d", 0.4, ""),
        ];
        let report = evaluate("test", &baseline, &tuned, 2, &no_phrases());
        // top-2: baseline = a(1.0), b(0.9); tuned = b(1.0), a(0.5)
        // a moved down (rank 1→2), b moved up (rank 2→1), c ignored
        assert_eq!(report.top_k, 2);
        assert_eq!(report.baseline_count, 2);
        assert_eq!(report.tuned_count, 2);
    }

    #[test]
    fn test_movement_up_down_stable() {
        let baseline = vec![
            make_hit("a", 1.0, ""),
            make_hit("b", 0.5, ""),
            make_hit("c", 0.3, ""),
        ];
        let tuned = vec![
            make_hit("b", 0.9, ""), // moved up 2→1
            make_hit("a", 0.8, ""), // moved down 1→2
            make_hit("c", 0.3, ""), // stable 3→3
        ];
        let report = evaluate("test", &baseline, &tuned, 3, &no_phrases());
        let find =
            |id: &str| -> &Movement { report.movements.iter().find(|m| m.id == id).unwrap() };
        assert_eq!(find("b").status, "up");
        assert_eq!(find("a").status, "down");
        assert_eq!(find("c").status, "stable");
        assert_eq!(report.verdict.label, "unchanged"); // 0 evicted, 0 new
    }

    #[test]
    fn test_empty_baseline_unchanged() {
        let tuned = vec![make_hit("a", 1.0, "")];
        let report = evaluate("test", &[], &tuned, 5, &no_phrases());
        assert_eq!(report.baseline_count, 0);
        assert_eq!(report.movements.len(), 1); // a is new
        assert_eq!(report.verdict.label, "changed");
    }

    #[test]
    fn test_empty_tuned_with_baseline() {
        let baseline = vec![make_hit("a", 1.0, "")];
        let report = evaluate("test", &baseline, &[], 5, &no_phrases());
        assert_eq!(report.tuned_count, 0);
        assert_eq!(report.verdict.label, "degraded");
        assert_eq!(report.movements.len(), 1);
        assert_eq!(report.movements[0].status, "evicted");
    }

    #[test]
    fn test_truncate_long_content() {
        let long = "x".repeat(300);
        let short = "y".repeat(50);
        assert_eq!(truncate(&long, 120).len(), 123); // 120 + "..."
        assert_eq!(truncate(&short, 120).len(), 50);
    }
}
