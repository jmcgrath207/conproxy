#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;
use crate::proxy::types::SearchResult;

fn make_result(content: &str, score: f32) -> SearchResult {
    SearchResult {
        id: "test".to_string(),
        score,
        content: content.to_string(),
        metadata: None,
        upstream_id: None,
    }
}

#[test]
fn test_filter_mode_from_str() {
    assert_eq!(FilterMode::from("filter"), FilterMode::Filter);
    assert_eq!(FilterMode::from("rerank"), FilterMode::Rerank);
    assert_eq!(FilterMode::from("boost"), FilterMode::Boost);
    assert_eq!(FilterMode::from("unknown"), FilterMode::Filter);
}

#[test]
fn test_scope_filter_none() {
    let filter = ScopeFilter::none();
    assert!(!filter.is_enabled());
}

#[test]
fn test_scope_filter_from_config() {
    let config = ProxyScopeConfig {
        weighted_phrases: vec![],
        seeds: vec!["rust".to_string(), "async".to_string()],
        mode: Some("filter".to_string()),
        min_seed_similarity: Some(0.5),
        seed_weight: Some(0.4),
        query_prefix: Some("code:".to_string()),
        ..Default::default()
    };
    let filter = ScopeFilter::from_config(&config);
    assert!(filter.is_enabled());
    assert_eq!(filter.mode(), FilterMode::Filter);
    assert_eq!(filter.query_prefix(), Some("code:"));
}

#[test]
fn test_apply_prefix() {
    let config = ProxyScopeConfig {
        weighted_phrases: vec![],
        seeds: vec![],
        mode: None,
        min_seed_similarity: None,
        seed_weight: None,
        query_prefix: Some("rust:".to_string()),
        ..Default::default()
    };
    let filter = ScopeFilter::from_config(&config);
    assert_eq!(filter.apply_prefix("async await"), "rust: async await");
}

#[test]
fn test_max_seed_similarity_no_seeds() {
    let filter = ScopeFilter::none();
    assert_eq!(filter.max_seed_similarity("anything"), 1.0);
}

#[test]
fn test_max_seed_similarity_exact_match() {
    let config = ProxyScopeConfig {
        weighted_phrases: vec![],
        seeds: vec!["rust async".to_string()],
        mode: None,
        min_seed_similarity: None,
        seed_weight: None,
        query_prefix: None,
        ..Default::default()
    };
    let filter = ScopeFilter::from_config(&config);
    let sim = filter.max_seed_similarity("rust async programming");
    assert!(sim > 0.3); // Should have good similarity
}

#[test]
fn test_max_seed_similarity_no_match() {
    let config = ProxyScopeConfig {
        weighted_phrases: vec![],
        seeds: vec!["rust".to_string()],
        mode: None,
        min_seed_similarity: None,
        seed_weight: None,
        query_prefix: None,
        ..Default::default()
    };
    let filter = ScopeFilter::from_config(&config);
    let sim = filter.max_seed_similarity("python javascript");
    assert!(sim < 0.2); // Should have low similarity
}

#[test]
fn test_filter_results_filter_mode() {
    let config = ProxyScopeConfig {
        weighted_phrases: vec![],
        seeds: vec!["rust".to_string()],
        mode: Some("filter".to_string()),
        min_seed_similarity: Some(0.2),
        seed_weight: None,
        query_prefix: None,
        ..Default::default()
    };
    let filter = ScopeFilter::from_config(&config);

    let results = vec![
        make_result("rust programming guide", 0.9),
        make_result("python tutorial", 0.8),
        make_result("rust async await", 0.7),
    ];

    let filtered = filter.filter_results(results);
    assert_eq!(filtered.len(), 2); // Only rust-related results kept
}

#[test]
fn test_filter_results_rerank_mode() {
    let config = ProxyScopeConfig {
        weighted_phrases: vec![],
        seeds: vec!["rust".to_string()],
        mode: Some("rerank".to_string()),
        min_seed_similarity: Some(0.0),
        seed_weight: Some(0.5),
        query_prefix: None,
        ..Default::default()
    };
    let filter = ScopeFilter::from_config(&config);

    let results = vec![
        make_result("python tutorial", 0.9),
        make_result("rust guide", 0.8),
    ];

    let reranked = filter.filter_results(results);
    // Rust result should be ranked higher despite lower original score
    assert_eq!(reranked.len(), 2);
    assert!(reranked[0].content.contains("rust"));
}

#[test]
fn test_filter_with_stats() {
    let config = ProxyScopeConfig {
        weighted_phrases: vec![],
        seeds: vec!["test".to_string()],
        mode: Some("filter".to_string()),
        min_seed_similarity: Some(0.3),
        seed_weight: None,
        query_prefix: None,
        ..Default::default()
    };
    let filter = ScopeFilter::from_config(&config);

    let results = vec![
        make_result("test content", 0.9),
        make_result("unrelated", 0.8),
        make_result("test example", 0.7),
    ];

    let (filtered, stats) = filter.filter_with_stats(results);
    assert_eq!(stats.input_count, 3);
    assert_eq!(stats.output_count, filtered.len());
    assert!(stats.filtered_count > 0);
}

#[test]
fn test_filter_with_discarded() {
    let config = ProxyScopeConfig {
        weighted_phrases: vec![],
        seeds: vec!["rust".to_string()],
        mode: Some("filter".to_string()),
        min_seed_similarity: Some(0.2),
        seed_weight: None,
        query_prefix: None,
        ..Default::default()
    };
    let filter = ScopeFilter::from_config(&config);

    let results = vec![
        make_result("rust programming", 0.9),
        make_result("python guide", 0.8),
        make_result("rust async", 0.7),
    ];

    let (kept, discarded) = filter.filter_with_discarded(results);
    assert_eq!(kept.len(), 2);
    assert_eq!(discarded.len(), 1);
    assert_eq!(discarded[0].id, "test");
    match &discarded[0].reason {
        DiscardReason::BelowThreshold { actual, threshold } => {
            assert!(*actual < *threshold);
        }
    }
}

#[test]
fn test_filter_with_discarded_no_seeds() {
    let filter = ScopeFilter::none();
    let results = vec![make_result("anything", 0.9)];

    let (kept, discarded) = filter.filter_with_discarded(results);
    assert_eq!(kept.len(), 1);
    assert_eq!(discarded.len(), 0);
}

#[test]
fn test_score_c_filter_ignores_weight() {
    use crate::config::WeightedPhrase;

    // Weak lexical match with huge weight must not pass filter.
    let config = ProxyScopeConfig {
        weighted_phrases: vec![WeightedPhrase {
            text: "zzzzunlikely".to_string(),
            weight: 100.0,
            min_similarity: Some(0.01),
        }],
        seeds: vec![],
        mode: Some("filter".to_string()),
        min_seed_similarity: Some(0.5),
        seed_weight: Some(0.9),
        query_prefix: None,
        ..Default::default()
    };
    let filter = ScopeFilter::from_config(&config);
    let results = vec![make_result("totally unrelated content here", 0.99)];
    let filtered = filter.filter_results(results);
    assert!(
        filtered.is_empty(),
        "weight must not save sub-threshold sim"
    );
}

#[test]
fn test_score_c_boost_uses_weight() {
    use crate::config::WeightedPhrase;

    let low = ProxyScopeConfig {
        weighted_phrases: vec![WeightedPhrase {
            text: "rust".to_string(),
            weight: 0.5,
            min_similarity: None,
        }],
        seeds: vec![],
        mode: Some("boost".to_string()),
        min_seed_similarity: Some(0.2),
        seed_weight: Some(0.5),
        query_prefix: None,
        ..Default::default()
    };
    let high = ProxyScopeConfig {
        weighted_phrases: vec![WeightedPhrase {
            text: "rust".to_string(),
            weight: 2.0,
            min_similarity: None,
        }],
        seeds: vec![],
        mode: Some("boost".to_string()),
        min_seed_similarity: Some(0.2),
        seed_weight: Some(0.5),
        query_prefix: None,
        ..Default::default()
    };
    let fl = ScopeFilter::from_config(&low);
    let fh = ScopeFilter::from_config(&high);
    let base = vec![make_result("rust programming", 1.0)];
    let out_l = fl.filter_results(base.clone());
    let out_h = fh.filter_results(base);
    assert_eq!(out_l.len(), 1);
    assert_eq!(out_h.len(), 1);
    assert!(
        out_h[0].score > out_l[0].score,
        "higher phrase weight should boost more: {} vs {}",
        out_h[0].score,
        out_l[0].score
    );
}

#[test]
fn test_weighted_phrases_from_config() {
    use crate::config::WeightedPhrase;

    let config = ProxyScopeConfig {
        weighted_phrases: vec![WeightedPhrase::new("billing")],
        seeds: vec!["ignored-when-weighted-set".to_string()],
        mode: Some("filter".to_string()),
        min_seed_similarity: Some(0.2),
        seed_weight: None,
        query_prefix: None,
        ..Default::default()
    };
    let filter = ScopeFilter::from_config(&config);
    assert_eq!(filter.phrases(), &["billing".to_string()]);
    let kept = filter.filter_results(vec![
        make_result("customer billing refund", 0.5),
        make_result("unrelated astronomy", 0.9),
    ]);
    assert_eq!(kept.len(), 1);
    assert!(kept[0].content.contains("billing"));
}

#[test]
fn test_legacy_seeds_toml_compat_via_struct() {
    // bare seeds still enable filter
    let config = ProxyScopeConfig {
        weighted_phrases: vec![],
        seeds: vec!["invoice".to_string()],
        mode: Some("filter".to_string()),
        min_seed_similarity: Some(0.2),
        seed_weight: None,
        query_prefix: None,
        ..Default::default()
    };
    let filter = ScopeFilter::from_config(&config);
    assert!(filter.is_enabled());
    assert_eq!(filter.seeds(), &["invoice".to_string()]);
}

#[test]
fn test_hybrid_in_band_lifts_weak_lexical() {
    use crate::config::WeightedPhrase;

    // One shared token → lexical in band; pure lexical fails floor; semantic lifts.
    // phrase tokens=2, overlap=1 → recall=0.5, no substring → lexical=0.5
    // with band [0.1, 0.55] and floor 0.6, lexical alone fails; hybrid lifts.
    let config = ProxyScopeConfig {
        weighted_phrases: vec![WeightedPhrase {
            text: "refund policy".into(),
            weight: 1.0,
            min_similarity: None,
        }],
        seeds: vec![],
        mode: Some("filter".into()),
        min_seed_similarity: Some(0.6),
        seed_weight: None,
        query_prefix: None,
        lexical_weight: Some(0.2),
        embed_band: Some([0.1, 0.55]),
    };
    let filter = ScopeFilter::from_config(&config).with_phrase_embeddings(vec![vec![1.0, 0.0]]);

    let content = "store refund process for buyers";
    let lexical_only = filter.best_sim(content);
    assert!(
        lexical_only < 0.6,
        "lexical alone below floor, got {lexical_only}"
    );
    // lexical ~0.5 is inside band → hybrid blends high semantic
    let hybrid = filter.best_sim_with_embedding(content, Some(&[1.0, 0.0]));
    assert!(
        hybrid >= 0.6,
        "hybrid semantic should lift in-band hit, got {hybrid} (lexical={lexical_only})"
    );

    let results = vec![make_result(content, 0.9)];
    let kept = filter.filter_results_hybrid(results, Some(&[vec![1.0, 0.0]]));
    assert_eq!(kept.len(), 1, "hybrid filter keeps in-band semantic hit");
}

#[test]
fn test_hybrid_outside_band_stays_lexical() {
    use crate::config::WeightedPhrase;

    let config = ProxyScopeConfig {
        weighted_phrases: vec![WeightedPhrase::new("exact phrase match here")],
        seeds: vec![],
        mode: Some("filter".into()),
        min_seed_similarity: Some(0.2),
        seed_weight: None,
        query_prefix: None,
        lexical_weight: Some(0.0), // would be pure semantic if hybrid ran
        embed_band: Some([0.1, 0.55]),
    };
    let filter = ScopeFilter::from_config(&config).with_phrase_embeddings(vec![vec![1.0, 0.0]]);

    // High lexical (substring) → above band hi → stay lexical even if emb orthogonal
    let content = "exact phrase match here in doc";
    let sim = filter.best_sim_with_embedding(content, Some(&[0.0, 1.0]));
    let lexical = filter.best_sim(content);
    assert!(
        (sim - lexical).abs() < 1e-5,
        "above band must ignore semantic: hybrid={sim} lexical={lexical}"
    );
}

#[test]
fn test_scope_config_embed_band_validate() {
    let mut c = ProxyScopeConfig {
        embed_band: Some([0.8, 0.2]),
        ..Default::default()
    };
    assert!(c.validate().is_err());
    c.embed_band = Some([0.1, 0.55]);
    assert!(c.validate().is_ok());
}

#[test]
fn test_mismatched_phrase_embeddings_ignored() {
    use crate::config::WeightedPhrase;
    let config = ProxyScopeConfig {
        weighted_phrases: vec![WeightedPhrase::new("alpha"), WeightedPhrase::new("beta")],
        ..Default::default()
    };
    let filter = ScopeFilter::from_config(&config).with_phrase_embeddings(vec![vec![1.0]]);
    assert!(!filter.has_phrase_embeddings());
}
