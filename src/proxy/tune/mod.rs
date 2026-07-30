//! Dry-run tuning platform (plan 09): sessions + full tune tool suite.
//!
//! All tools default to dry-run. Sessions are process-local, keyed by
//! `(agent_id, context_id, session_id)` with idle TTL.

mod apply;
mod benchmark;
mod cache;
mod cascade_tune;
mod compare;
mod embed_tune;
mod federated_tune;
mod rate_limit;
mod scope;
mod session;
mod suggest;
mod warm;

pub use apply::{apply_export_artifact, apply_tune_export, ApplyReport};
pub use benchmark::{evaluate, BenchmarkHit, BenchmarkReport, Movement, Verdict};
pub use cache::{cache_tune, CacheAccessEvent, CacheTuneParams, CacheTuneReport};
pub use cascade_tune::{cascade_tune, CascadeLegProbe, CascadeTuneParams, CascadeTuneReport};
pub use compare::{compare_runs, CompareReport, CompareRequest};
pub use embed_tune::{embed_tune, EmbedTuneParams, EmbedTuneReport};
pub use federated_tune::{federated_tune, FedHit, FederatedTuneParams, FederatedTuneReport};
pub use rate_limit::{rate_limit_tune, RateLimitTuneParams, RateLimitTuneReport};
pub use scope::{
    scope_tune, ScopeTuneParams, ScopeTuneReport, SweepPointResult, DEFAULT_MAX_SWEEP_POINTS,
};
pub use session::{
    SessionError, TuneExportArtifact, TuneExportFormats, TuneRunRecord, TuneSession,
    TuneSessionStore, TuneSessionSummary, DEFAULT_SESSION_TTL_SECS,
};
pub use suggest::{scope_suggest, ScopeSuggestParams, ScopeSuggestReport, SuggestedPhrase};
pub use warm::{warm_tune, WarmTuneParams, WarmTuneReport};

use serde::{Deserialize, Serialize};

/// Budget caps shared by tune tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuneBudget {
    /// Wall-clock budget for a single tool call (advisory for callers).
    pub timeout_ms: u64,
    /// Max results retained per candidate/baseline.
    pub max_results: usize,
    /// Max points in a sweep grid.
    pub max_sweep_points: usize,
    /// Max queries in a multi-query tune.
    pub max_queries: usize,
}

impl Default for TuneBudget {
    fn default() -> Self {
        Self {
            timeout_ms: 30_000,
            max_results: 50,
            max_sweep_points: 16,
            max_queries: 32,
        }
    }
}

/// Stable envelope returned by every tune tool (serialized for MCP).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuneReport {
    pub tool: String,
    pub session_id: String,
    pub run_id: String,
    pub params_used: serde_json::Value,
    pub budget: TuneBudget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<serde_json::Value>,
    #[serde(default)]
    pub candidates: Vec<serde_json::Value>,
    #[serde(default)]
    pub metrics: serde_json::Value,
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl TuneReport {
    /// Build a report shell; fill baseline/candidates/metrics in the tool.
    #[must_use]
    pub fn new(
        tool: impl Into<String>,
        session_id: impl Into<String>,
        run_id: impl Into<String>,
    ) -> Self {
        Self {
            tool: tool.into(),
            session_id: session_id.into(),
            run_id: run_id.into(),
            params_used: serde_json::Value::Null,
            budget: TuneBudget::default(),
            baseline: None,
            candidates: Vec::new(),
            metrics: serde_json::json!({}),
            warnings: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::config::{ProxyScopeConfig, WeightedPhrase};
    use crate::proxy::types::SearchResult;

    fn hit(id: &str, content: &str, score: f32) -> SearchResult {
        SearchResult {
            id: id.to_string(),
            content: content.to_string(),
            score,
            metadata: Default::default(),
            upstream_id: None,
        }
    }

    fn sample_hits() -> Vec<SearchResult> {
        vec![
            hit("1", "Rust async runtime with Tokio", 0.9),
            hit("2", "Python web framework Django", 0.8),
            hit("3", "Refund policy for online orders", 0.7),
            hit("4", "tokio spawn task concurrency", 0.85),
        ]
    }

    #[test]
    fn session_open_close_and_isolation() {
        let store = TuneSessionStore::new(3600);
        let s = store
            .open("agent-a".into(), "docs".into(), None)
            .expect("open");
        assert_eq!(s.agent_id, "agent-a");
        assert_eq!(s.context_id, "docs");
        assert!(!s.session_id.is_empty());

        // Wrong agent cannot get
        assert!(store
            .get(&s.session_id, Some("agent-b"), Some("docs"))
            .is_none());
        // Wrong context cannot get
        assert!(store
            .get(&s.session_id, Some("agent-a"), Some("other"))
            .is_none());
        // Own session ok
        assert!(store
            .get(&s.session_id, Some("agent-a"), Some("docs"))
            .is_some());

        assert!(store.close(&s.session_id, Some("agent-a")));
        assert!(store
            .get(&s.session_id, Some("agent-a"), Some("docs"))
            .is_none());
    }

    #[test]
    fn scope_tune_filter_sweep_score_c() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "ctx".into(), None).unwrap();

        let phrases = vec![
            WeightedPhrase {
                text: "rust".into(),
                weight: 9.0, // weight ignored in filter
                min_similarity: None,
            },
            WeightedPhrase {
                text: "tokio".into(),
                weight: 0.1,
                min_similarity: None,
            },
        ];
        // WeightedPhrase fields match config crate shape.

        let report = scope_tune(
            &store,
            ScopeTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("ctx".into()),
                hits: sample_hits(),
                weighted_phrases: phrases,
                mode: Some("filter".into()),
                min_similarity: None,
                min_similarity_sweep: Some(vec![0.1, 0.5, 0.9]),
                scope_weight: None,
                lexical_weight: None,
                budget: TuneBudget::default(),
            },
        )
        .expect("tune");

        assert_eq!(report.tool, "scope_tune");
        assert_eq!(report.sweep.len(), 3);
        // Lower threshold keeps more
        assert!(report.sweep[0].kept >= report.sweep[1].kept);
        assert!(report.sweep[1].kept >= report.sweep[2].kept);
        // Baseline = all hits (bypass)
        assert_eq!(report.baseline_count, 4);
        // Session recorded a run
        let sess = store.get(&s.session_id, Some("a"), Some("ctx")).unwrap();
        assert_eq!(sess.runs.len(), 1);
    }

    #[test]
    fn scope_suggest_and_export() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("ops".into(), "docs".into(), None).unwrap();

        let suggest = scope_suggest(
            &store,
            ScopeSuggestParams {
                session_id: s.session_id.clone(),
                agent_id: Some("ops".into()),
                context_id: Some("docs".into()),
                texts: vec![
                    "Rust async Tokio runtime".into(),
                    "Tokio spawn and Rust channels".into(),
                    "Django Python web".into(),
                ],
                max_phrases: 5,
                budget: TuneBudget::default(),
            },
        )
        .expect("suggest");
        assert!(!suggest.phrases.is_empty());
        assert!(suggest.phrases.iter().any(|p| p.text.contains("rust")
            || p.text.contains("tokio")
            || p.text.contains("async")));

        // Record a synthetic selected run then export
        store
            .append_run(
                &s.session_id,
                Some("ops"),
                TuneRunRecord {
                    run_id: "run-1".into(),
                    tool: "scope_tune".into(),
                    params: serde_json::json!({
                        "mode": "filter",
                        "min_similarity": 0.25,
                        "weighted_phrases": suggest.phrases.iter().map(|p| {
                            serde_json::json!({"text": p.text, "weight": p.weight})
                        }).collect::<Vec<_>>()
                    }),
                    metrics: serde_json::json!({"kept": 2}),
                    selected: true,
                },
            )
            .unwrap();

        let art = store
            .export(&s.session_id, Some("ops"), Some("docs"))
            .expect("export");
        assert_eq!(art.context_id, "docs");
        assert!(art.formats.toml.contains("contexts.docs.scope"));
        assert!(art.formats.toml.contains("min_similarity"));
    }

    #[test]
    fn compare_runs_same_session_only() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        store
            .append_run(
                &s.session_id,
                Some("a"),
                TuneRunRecord {
                    run_id: "r1".into(),
                    tool: "t".into(),
                    params: serde_json::json!({"x": 1}),
                    metrics: serde_json::json!({"kept": 3}),
                    selected: false,
                },
            )
            .unwrap();
        store
            .append_run(
                &s.session_id,
                Some("a"),
                TuneRunRecord {
                    run_id: "r2".into(),
                    tool: "t".into(),
                    params: serde_json::json!({"x": 2}),
                    metrics: serde_json::json!({"kept": 5}),
                    selected: false,
                },
            )
            .unwrap();

        let cmp = compare_runs(
            &store,
            CompareRequest {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                run_id_a: "r1".into(),
                run_id_b: "r2".into(),
            },
        )
        .unwrap();
        assert_eq!(cmp.run_a.run_id, "r1");
        assert_eq!(cmp.run_b.run_id, "r2");
        assert!(!cmp.param_diff.is_empty() || cmp.metric_diff.contains_key("kept"));
    }

    #[test]
    fn scope_config_roundtrip_for_filter() {
        let cfg = ProxyScopeConfig {
            weighted_phrases: vec![WeightedPhrase {
                text: "refund".into(),
                weight: 1.5,
                min_similarity: Some(0.2),
            }],
            mode: Some("filter".into()),
            min_seed_similarity: Some(0.3),
            ..Default::default()
        };
        let filter = crate::proxy::scope::ScopeFilter::from_config(&cfg);
        let out = filter.filter_results(sample_hits());
        // Only refund-ish content survives if sim high enough — may be empty
        // depending on lexical; just ensure no panic and baseline path works.
        let _ = out;
    }

    #[test]
    fn cache_cascade_federated_rate_embed_warm_smoke() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();

        let cache = cache_tune(
            &store,
            CacheTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                events: vec![
                    CacheAccessEvent {
                        key: "q1".into(),
                        age_secs: Some(30),
                    },
                    CacheAccessEvent {
                        key: "q2".into(),
                        age_secs: Some(400),
                    },
                    CacheAccessEvent {
                        key: "q3".into(),
                        age_secs: None,
                    },
                ],
                fresh_ttl_secs: Some(vec![60, 300]),
                stale_ttl_secs: Some(vec![0, 120]),
                budget: TuneBudget::default(),
            },
        )
        .unwrap();
        assert_eq!(cache.candidates.len(), 4);

        let cas = cascade_tune(
            &store,
            CascadeTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                legs: vec![
                    CascadeLegProbe {
                        upstream_id: "primary".into(),
                        priority: 0,
                        best_score: 0.4,
                        result_count: 1,
                        latency_ms: 10,
                    },
                    CascadeLegProbe {
                        upstream_id: "secondary".into(),
                        priority: 1,
                        best_score: 0.9,
                        result_count: 5,
                        latency_ms: 20,
                    },
                ],
                min_score_threshold: Some(0.7),
                min_results: Some(1),
                max_cascade_depth: Some(2),
                min_score_sweep: Some(vec![0.5, 0.8]),
                budget: TuneBudget::default(),
            },
        )
        .unwrap();
        assert_eq!(cas.candidates.len(), 2);
        assert!(cas.candidates.iter().any(|c| c.cascaded));

        let fed = federated_tune(
            &store,
            FederatedTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                hits: vec![
                    FedHit {
                        id: "l1".into(),
                        score: 0.9,
                        source: "local".into(),
                    },
                    FedHit {
                        id: "r1".into(),
                        score: 0.95,
                        source: "remote".into(),
                    },
                ],
                local_weight_sweep: Some(vec![0.2, 0.8]),
                top_k: Some(2),
                budget: TuneBudget::default(),
            },
        )
        .unwrap();
        assert_eq!(fed.candidates.len(), 2);

        let rl = rate_limit_tune(
            &store,
            RateLimitTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                arrival_ms: vec![0, 10, 20, 30, 40, 50],
                rps_sweep: Some(vec![10.0, 1000.0]),
                burst_sweep: Some(vec![1, 10]),
                budget: TuneBudget::default(),
            },
        )
        .unwrap();
        assert_eq!(rl.candidates.len(), 4);
        assert!(rl.candidates.iter().any(|c| c.denied > 0));

        let emb = embed_tune(
            &store,
            EmbedTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                text_count: 100,
                batch_size_sweep: Some(vec![8, 32]),
                per_text_ms: Some(1.0),
                batch_overhead_ms: Some(10.0),
                budget: TuneBudget::default(),
            },
        )
        .unwrap();
        assert_eq!(emb.candidates.len(), 2);

        let warm = warm_tune(
            &store,
            WarmTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                plan_keys: vec!["a".into(), "b".into(), "c".into()],
                cached_keys: vec!["a".into()],
                per_key_ms: Some(40),
                concurrency_sweep: Some(vec![1, 2]),
                execute: false,
                budget: TuneBudget::default(),
            },
        )
        .unwrap();
        assert_eq!(warm.keys_to_warm.len(), 2);
        assert!(!warm.execute_performed);

        assert!(warm_tune(
            &store,
            WarmTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                plan_keys: vec!["x".into()],
                cached_keys: vec![],
                per_key_ms: None,
                concurrency_sweep: None,
                execute: true,
                budget: TuneBudget::default(),
            },
        )
        .is_err());
    }

    // --- scope_tune mode coverage: boost + rerank ---

    #[test]
    fn scope_tune_boost_mode() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let report = scope_tune(
            &store,
            ScopeTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                hits: sample_hits(),
                weighted_phrases: vec![WeightedPhrase {
                    text: "tokio".into(),
                    weight: 2.0,
                    min_similarity: None,
                }],
                mode: Some("boost".into()),
                min_similarity: None,
                min_similarity_sweep: Some(vec![0.1, 0.5]),
                scope_weight: Some(1.5),
                lexical_weight: None,
                budget: TuneBudget::default(),
            },
        )
        .unwrap();
        assert_eq!(report.mode, "boost");
        assert_eq!(report.sweep.len(), 2);
        // boost keeps all hits (no filtering), just re-scores
        for pt in &report.sweep {
            assert_eq!(pt.kept, 4);
        }
    }

    #[test]
    fn scope_tune_rerank_mode() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let report = scope_tune(
            &store,
            ScopeTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                hits: sample_hits(),
                weighted_phrases: vec![WeightedPhrase {
                    text: "tokio".into(),
                    weight: 2.0,
                    min_similarity: None,
                }],
                mode: Some("rerank".into()),
                min_similarity: None,
                min_similarity_sweep: Some(vec![0.1]),
                scope_weight: Some(1.0),
                lexical_weight: None,
                budget: TuneBudget::default(),
            },
        )
        .unwrap();
        assert_eq!(report.mode, "rerank");
        assert_eq!(report.sweep.len(), 1);
        // rerank keeps all hits (no filter), just re-scores
        assert_eq!(report.sweep[0].kept, 4);
    }

    #[test]
    fn scope_tune_default_mode_is_filter() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let report = scope_tune(
            &store,
            ScopeTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                hits: sample_hits(),
                weighted_phrases: vec![],
                mode: None,
                min_similarity: None,
                min_similarity_sweep: Some(vec![0.5]),
                scope_weight: None,
                lexical_weight: None,
                budget: TuneBudget::default(),
            },
        )
        .unwrap();
        assert_eq!(report.mode, "filter");
    }

    // --- scope_tune: single threshold (no sweep) ---

    #[test]
    fn scope_tune_single_threshold() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let report = scope_tune(
            &store,
            ScopeTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                hits: sample_hits(),
                weighted_phrases: vec![WeightedPhrase {
                    text: "rust".into(),
                    weight: 1.0,
                    min_similarity: None,
                }],
                mode: Some("filter".into()),
                min_similarity: Some(0.3),
                min_similarity_sweep: None,
                scope_weight: None,
                lexical_weight: None,
                budget: TuneBudget::default(),
            },
        )
        .unwrap();
        // single threshold → sweep has 1 point
        assert_eq!(report.sweep.len(), 1);
    }

    // --- scope_tune: truncation warning ---

    #[test]
    fn scope_tune_truncates_over_budget_hits() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let mut hits = sample_hits();
        hits.push(hit("5", "extra", 0.5));
        hits.push(hit("6", "extra2", 0.4));
        let report = scope_tune(
            &store,
            ScopeTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                hits,
                weighted_phrases: vec![],
                mode: Some("filter".into()),
                min_similarity: None,
                min_similarity_sweep: Some(vec![0.5]),
                scope_weight: None,
                lexical_weight: None,
                budget: TuneBudget {
                    max_results: 4,
                    ..TuneBudget::default()
                },
            },
        )
        .unwrap();
        assert!(report.warnings.iter().any(|w| w.contains("truncating")));
    }

    // --- scope_tune: sweep over budget ---

    #[test]
    fn scope_tune_sweep_over_budget_fails() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let sweep: Vec<f32> = (0..50).map(|i| i as f32 * 0.02).collect();
        let err = scope_tune(
            &store,
            ScopeTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                hits: sample_hits(),
                weighted_phrases: vec![],
                mode: Some("filter".into()),
                min_similarity: None,
                min_similarity_sweep: Some(sweep),
                scope_weight: None,
                lexical_weight: None,
                budget: TuneBudget::default(),
            },
        )
        .unwrap_err();
        assert!(err.contains("sweep has"));
    }

    // --- empty hits error ---

    #[test]
    fn scope_tune_empty_hits_error() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let err = scope_tune(
            &store,
            ScopeTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                hits: vec![],
                weighted_phrases: vec![],
                mode: None,
                min_similarity: None,
                min_similarity_sweep: None,
                scope_weight: None,
                lexical_weight: None,
                budget: TuneBudget::default(),
            },
        )
        .unwrap_err();
        assert!(err.contains("hits must not be empty"));
    }

    // --- unknown session for all tools ---

    #[test]
    fn all_tune_tools_reject_unknown_session() {
        let store = TuneSessionStore::new(3600);
        let default_budget = TuneBudget::default();

        let err = scope_tune(
            &store,
            ScopeTuneParams {
                session_id: "nope".into(),
                agent_id: None,
                context_id: None,
                hits: sample_hits(),
                weighted_phrases: vec![],
                mode: None,
                min_similarity: None,
                min_similarity_sweep: None,
                scope_weight: None,
                lexical_weight: None,
                budget: default_budget.clone(),
            },
        )
        .unwrap_err();
        assert!(err.contains("session not found"));

        let err = scope_suggest(
            &store,
            ScopeSuggestParams {
                session_id: "nope".into(),
                agent_id: None,
                context_id: None,
                texts: vec!["hi".into()],
                max_phrases: 8,
                budget: default_budget.clone(),
            },
        )
        .unwrap_err();
        assert!(err.contains("session not found"));

        let err = compare_runs(
            &store,
            CompareRequest {
                session_id: "nope".into(),
                agent_id: None,
                context_id: None,
                run_id_a: "a".into(),
                run_id_b: "b".into(),
            },
        )
        .unwrap_err();
        assert!(err.contains("session not found"));

        let err = cache_tune(
            &store,
            CacheTuneParams {
                session_id: "nope".into(),
                agent_id: None,
                context_id: None,
                events: vec![CacheAccessEvent {
                    key: "k".into(),
                    age_secs: Some(1),
                }],
                fresh_ttl_secs: None,
                stale_ttl_secs: None,
                budget: default_budget.clone(),
            },
        )
        .unwrap_err();
        assert!(err.contains("session not found"));

        let err = cascade_tune(
            &store,
            CascadeTuneParams {
                session_id: "nope".into(),
                agent_id: None,
                context_id: None,
                legs: vec![CascadeLegProbe {
                    upstream_id: "a".into(),
                    priority: 0,
                    best_score: 0.5,
                    result_count: 1,
                    latency_ms: 10,
                }],
                min_score_threshold: None,
                min_results: None,
                max_cascade_depth: None,
                min_score_sweep: None,
                budget: default_budget.clone(),
            },
        )
        .unwrap_err();
        assert!(err.contains("session not found"));

        let err = federated_tune(
            &store,
            FederatedTuneParams {
                session_id: "nope".into(),
                agent_id: None,
                context_id: None,
                hits: vec![FedHit {
                    id: "a".into(),
                    score: 0.5,
                    source: "local".into(),
                }],
                local_weight_sweep: None,
                top_k: None,
                budget: default_budget.clone(),
            },
        )
        .unwrap_err();
        assert!(err.contains("session not found"));

        let err = rate_limit_tune(
            &store,
            RateLimitTuneParams {
                session_id: "nope".into(),
                agent_id: None,
                context_id: None,
                arrival_ms: vec![0],
                rps_sweep: None,
                burst_sweep: None,
                budget: default_budget.clone(),
            },
        )
        .unwrap_err();
        assert!(err.contains("session not found"));

        let err = embed_tune(
            &store,
            EmbedTuneParams {
                session_id: "nope".into(),
                agent_id: None,
                context_id: None,
                text_count: 1,
                batch_size_sweep: None,
                per_text_ms: None,
                batch_overhead_ms: None,
                budget: default_budget.clone(),
            },
        )
        .unwrap_err();
        assert!(err.contains("session not found"));

        let err = warm_tune(
            &store,
            WarmTuneParams {
                session_id: "nope".into(),
                agent_id: None,
                context_id: None,
                plan_keys: vec!["a".into()],
                cached_keys: vec![],
                per_key_ms: None,
                concurrency_sweep: None,
                execute: false,
                budget: default_budget.clone(),
            },
        )
        .unwrap_err();
        assert!(err.contains("session not found"));
    }

    // --- empty input validation for each tool ---

    #[test]
    fn cache_tune_empty_events_error() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let err = cache_tune(
            &store,
            CacheTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                events: vec![],
                fresh_ttl_secs: None,
                stale_ttl_secs: None,
                budget: TuneBudget::default(),
            },
        )
        .unwrap_err();
        assert!(err.contains("events must not be empty"));
    }

    #[test]
    fn cascade_tune_empty_legs_error() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let err = cascade_tune(
            &store,
            CascadeTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                legs: vec![],
                min_score_threshold: None,
                min_results: None,
                max_cascade_depth: None,
                min_score_sweep: None,
                budget: TuneBudget::default(),
            },
        )
        .unwrap_err();
        assert!(err.contains("legs must not be empty"));
    }

    #[test]
    fn federated_tune_empty_hits_error() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let err = federated_tune(
            &store,
            FederatedTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                hits: vec![],
                local_weight_sweep: None,
                top_k: None,
                budget: TuneBudget::default(),
            },
        )
        .unwrap_err();
        assert!(err.contains("hits must not be empty"));
    }

    #[test]
    fn rate_limit_tune_empty_arrivals_error() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let err = rate_limit_tune(
            &store,
            RateLimitTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                arrival_ms: vec![],
                rps_sweep: None,
                burst_sweep: None,
                budget: TuneBudget::default(),
            },
        )
        .unwrap_err();
        assert!(err.contains("arrival_ms must not be empty"));
    }

    #[test]
    fn embed_tune_zero_text_count_error() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let err = embed_tune(
            &store,
            EmbedTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                text_count: 0,
                batch_size_sweep: None,
                per_text_ms: None,
                batch_overhead_ms: None,
                budget: TuneBudget::default(),
            },
        )
        .unwrap_err();
        assert!(err.contains("text_count must be > 0"));
    }

    #[test]
    fn suggest_empty_texts_error() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let err = scope_suggest(
            &store,
            ScopeSuggestParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                texts: vec![],
                max_phrases: 8,
                budget: TuneBudget::default(),
            },
        )
        .unwrap_err();
        assert!(err.contains("texts must not be empty"));
    }

    // --- compare_runs error paths ---

    #[test]
    fn compare_runs_unknown_run_id() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        store
            .append_run(
                &s.session_id,
                None,
                TuneRunRecord {
                    run_id: "r1".into(),
                    tool: "t".into(),
                    params: serde_json::json!({}),
                    metrics: serde_json::json!({}),
                    selected: false,
                },
            )
            .unwrap();
        let err = compare_runs(
            &store,
            CompareRequest {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                run_id_a: "r1".into(),
                run_id_b: "nonexistent".into(),
            },
        )
        .unwrap_err();
        assert!(err.contains("unknown run_id"));
    }

    #[test]
    fn compare_runs_same_params_empty_diff() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        for rid in ["r1", "r2"] {
            store
                .append_run(
                    &s.session_id,
                    None,
                    TuneRunRecord {
                        run_id: rid.into(),
                        tool: "t".into(),
                        params: serde_json::json!({"x": 1}),
                        metrics: serde_json::json!({"y": 2}),
                        selected: false,
                    },
                )
                .unwrap();
        }
        let cmp = compare_runs(
            &store,
            CompareRequest {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                run_id_a: "r1".into(),
                run_id_b: "r2".into(),
            },
        )
        .unwrap();
        assert!(cmp.param_diff.is_empty());
        assert!(cmp.metric_diff.is_empty());
    }

    // --- cascade_tune: no legs cascade ---

    #[test]
    fn cascade_tune_all_above_threshold() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let cas = cascade_tune(
            &store,
            CascadeTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                legs: vec![
                    CascadeLegProbe {
                        upstream_id: "p1".into(),
                        priority: 0,
                        best_score: 0.9,
                        result_count: 10,
                        latency_ms: 5,
                    },
                    CascadeLegProbe {
                        upstream_id: "p2".into(),
                        priority: 1,
                        best_score: 0.8,
                        result_count: 8,
                        latency_ms: 10,
                    },
                ],
                min_score_threshold: Some(0.5),
                min_results: Some(1),
                max_cascade_depth: Some(2),
                min_score_sweep: None,
                budget: TuneBudget::default(),
            },
        )
        .unwrap();
        // All legs above threshold → no cascading needed
        assert!(cas.candidates.iter().all(|c| !c.cascaded));
    }

    // --- cache_tune: TTL grid over budget ---

    #[test]
    fn cache_tune_grid_over_budget_fails() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let big_fresh: Vec<u64> = (0..20).collect();
        let big_stale: Vec<u64> = (0..20).collect();
        let err = cache_tune(
            &store,
            CacheTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                events: vec![CacheAccessEvent {
                    key: "k".into(),
                    age_secs: Some(1),
                }],
                fresh_ttl_secs: Some(big_fresh),
                stale_ttl_secs: Some(big_stale),
                budget: TuneBudget::default(), // max_sweep_points = 16
            },
        )
        .unwrap_err();
        assert!(err.contains("TTL grid has"));
    }

    // --- cache_tune: events truncation ---

    #[test]
    fn cache_tune_truncates_over_budget_events() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let events: Vec<_> = (0..100)
            .map(|i| CacheAccessEvent {
                key: format!("k{i}"),
                age_secs: Some(10),
            })
            .collect();
        let report = cache_tune(
            &store,
            CacheTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                events,
                fresh_ttl_secs: Some(vec![60]),
                stale_ttl_secs: Some(vec![0]),
                budget: TuneBudget {
                    max_results: 10,
                    ..TuneBudget::default()
                },
            },
        )
        .unwrap();
        assert!(report.warnings.iter().any(|w| w.contains("truncated")));
    }

    // --- warm_tune: execute=true rejected ---

    #[test]
    fn warm_tune_execute_rejected() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let err = warm_tune(
            &store,
            WarmTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                plan_keys: vec!["a".into()],
                cached_keys: vec![],
                per_key_ms: None,
                concurrency_sweep: None,
                execute: true,
                budget: TuneBudget::default(),
            },
        )
        .unwrap_err();
        assert!(err.contains("execute") || err.contains("rejected"));
    }

    // --- federated_tune: blended score reorder ---

    #[test]
    fn federated_tune_local_weight_reorder() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let fed = federated_tune(
            &store,
            FederatedTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                hits: vec![
                    FedHit {
                        id: "local1".into(),
                        score: 0.6,
                        source: "local".into(),
                    },
                    FedHit {
                        id: "remote1".into(),
                        score: 0.9,
                        source: "remote".into(),
                    },
                ],
                local_weight_sweep: Some(vec![0.0, 0.5, 1.0]),
                top_k: Some(2),
                budget: TuneBudget::default(),
            },
        )
        .unwrap();
        // 3 weights → 3 candidates
        assert_eq!(fed.candidates.len(), 3);
        // All candidates should have top_k hits worth of ordered IDs
        for c in &fed.candidates {
            assert!(c.ordered_ids.len() <= 2);
        }
    }

    // --- rate_limit_tune: all allowed ---

    #[test]
    fn rate_limit_tune_high_rps_all_allowed() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let rl = rate_limit_tune(
            &store,
            RateLimitTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                arrival_ms: vec![0, 100, 200, 300],
                rps_sweep: Some(vec![1000.0]),
                burst_sweep: Some(vec![10]),
                budget: TuneBudget::default(),
            },
        )
        .unwrap();
        assert_eq!(rl.candidates.len(), 1);
        assert_eq!(rl.candidates[0].denied, 0);
        assert_eq!(rl.candidates[0].allowed, 4);
    }

    // --- embed_tune: batch size affects estimate ---

    #[test]
    fn embed_tune_larger_batch_lower_latency() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let emb = embed_tune(
            &store,
            EmbedTuneParams {
                session_id: s.session_id.clone(),
                agent_id: Some("a".into()),
                context_id: Some("c".into()),
                text_count: 100,
                batch_size_sweep: Some(vec![1, 100]),
                per_text_ms: Some(1.0),
                batch_overhead_ms: Some(10.0),
                budget: TuneBudget::default(),
            },
        )
        .unwrap();
        assert_eq!(emb.candidates.len(), 2);
        // batch_size=1: 100 batches × 10ms overhead + 100 texts × 1ms = 1100ms
        // batch_size=100: 1 batch × 10ms + 100 × 1ms = 110ms
        assert!(emb.candidates[0].est_total_ms > emb.candidates[1].est_total_ms);
    }

    // --- Default TuneBudget ---

    #[test]
    fn tune_budget_default_values() {
        let b = TuneBudget::default();
        assert_eq!(b.timeout_ms, 30_000);
        assert_eq!(b.max_results, 50);
        assert_eq!(b.max_sweep_points, 16);
        assert_eq!(b.max_queries, 32);
    }
}
