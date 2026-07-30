#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

fn make_result(id: &str, score: f32) -> SearchResult {
    SearchResult {
        id: id.to_string(),
        score,
        content: format!("Content for {}", id),
        metadata: None,
        upstream_id: None,
    }
}

#[test]
fn test_merge_mode_from_str() {
    assert_eq!(
        MergeMode::parse("local_only_fallback"),
        MergeMode::LocalOnlyFallback
    );
    assert_eq!(MergeMode::parse("local_priority"), MergeMode::LocalPriority);
    assert_eq!(
        MergeMode::parse("remote_priority"),
        MergeMode::RemotePriority
    );
    assert_eq!(MergeMode::parse("interleave"), MergeMode::Interleave);
    assert_eq!(MergeMode::parse("unknown"), MergeMode::LocalOnlyFallback);
}

#[test]
fn test_should_query_remote_empty_local() {
    let config = FederatedSearchConfig {
        fallback_on_empty: true,
        merge_mode: MergeMode::LocalOnlyFallback,
        ..Default::default()
    };
    let federated = FederatedSearch::new(config);

    let decision = federated.should_query_remote(&[]);
    assert_eq!(decision, FallbackDecision::EmptyLocal);
}

#[test]
fn test_should_query_remote_below_min() {
    let config = FederatedSearchConfig {
        min_local_results: 5,
        merge_mode: MergeMode::LocalOnlyFallback,
        ..Default::default()
    };
    let federated = FederatedSearch::new(config);

    let local = vec![make_result("1", 0.9), make_result("2", 0.8)];
    let decision = federated.should_query_remote(&local);
    assert_eq!(decision, FallbackDecision::BelowMinResults);
}

#[test]
fn test_should_query_remote_low_confidence() {
    let config = FederatedSearchConfig {
        min_local_results: 1,
        min_local_confidence: 0.8,
        fallback_on_low_confidence: true,
        merge_mode: MergeMode::LocalOnlyFallback,
        ..Default::default()
    };
    let federated = FederatedSearch::new(config);

    let local = vec![make_result("1", 0.5), make_result("2", 0.4)];
    let decision = federated.should_query_remote(&local);
    assert_eq!(decision, FallbackDecision::LowConfidence);
}

#[test]
fn test_should_query_remote_sufficient() {
    let config = FederatedSearchConfig {
        min_local_results: 2,
        min_local_confidence: 0.7,
        merge_mode: MergeMode::LocalOnlyFallback,
        ..Default::default()
    };
    let federated = FederatedSearch::new(config);

    let local = vec![make_result("1", 0.9), make_result("2", 0.8)];
    let decision = federated.should_query_remote(&local);
    assert_eq!(decision, FallbackDecision::LocalSufficient);
}

#[test]
fn test_should_query_remote_interleave_mode() {
    let config = FederatedSearchConfig {
        merge_mode: MergeMode::Interleave,
        ..Default::default()
    };
    let federated = FederatedSearch::new(config);

    let local = vec![make_result("1", 0.9)];
    let decision = federated.should_query_remote(&local);
    assert_eq!(decision, FallbackDecision::AlwaysRemote);
}

#[test]
fn test_merge_local_only_fallback_with_local() {
    let config = FederatedSearchConfig {
        merge_mode: MergeMode::LocalOnlyFallback,
        max_merged_results: 10,
        ..Default::default()
    };
    let federated = FederatedSearch::new(config);

    let local = vec![make_result("1", 0.9)];
    let remote = vec![make_result("2", 0.8)];

    let (merged, stats) = federated.merge(local, remote);

    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].result.id, "1");
    assert_eq!(merged[0].source, ResultSource::Local);
    assert_eq!(stats.merged_count, 1);
}

#[test]
fn test_merge_local_only_fallback_empty_local() {
    let config = FederatedSearchConfig {
        merge_mode: MergeMode::LocalOnlyFallback,
        max_merged_results: 10,
        ..Default::default()
    };
    let federated = FederatedSearch::new(config);

    let local = vec![];
    let remote = vec![make_result("2", 0.8)];

    let (merged, stats) = federated.merge(local, remote);

    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].result.id, "2");
    assert_eq!(merged[0].source, ResultSource::Remote);
    assert_eq!(stats.local_count, 0);
    assert_eq!(stats.remote_count, 1);
}

#[test]
fn test_merge_local_priority() {
    let config = FederatedSearchConfig {
        merge_mode: MergeMode::LocalPriority,
        max_merged_results: 10,
        ..Default::default()
    };
    let federated = FederatedSearch::new(config);

    let local = vec![make_result("1", 0.9)];
    let remote = vec![make_result("2", 0.95), make_result("3", 0.85)];

    let (merged, _) = federated.merge(local, remote);

    assert_eq!(merged.len(), 3);
    assert_eq!(merged[0].result.id, "1"); // Local first
    assert_eq!(merged[0].source, ResultSource::Local);
    assert_eq!(merged[1].source, ResultSource::Remote);
}

#[test]
fn test_merge_remote_priority() {
    let config = FederatedSearchConfig {
        merge_mode: MergeMode::RemotePriority,
        max_merged_results: 10,
        ..Default::default()
    };
    let federated = FederatedSearch::new(config);

    let local = vec![make_result("1", 0.9)];
    let remote = vec![make_result("2", 0.8)];

    let (merged, _) = federated.merge(local, remote);

    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0].result.id, "2"); // Remote first
    assert_eq!(merged[0].source, ResultSource::Remote);
}

#[test]
fn test_merge_interleave() {
    let config = FederatedSearchConfig {
        merge_mode: MergeMode::Interleave,
        max_merged_results: 10,
        ..Default::default()
    };
    let federated = FederatedSearch::new(config);

    let local = vec![make_result("1", 0.9), make_result("2", 0.7)];
    let remote = vec![make_result("3", 0.85), make_result("4", 0.6)];

    let (merged, _) = federated.merge(local, remote);

    assert_eq!(merged.len(), 4);
    // Sorted by score: 0.9, 0.85, 0.7, 0.6
    assert_eq!(merged[0].result.id, "1");
    assert_eq!(merged[1].result.id, "3");
    assert_eq!(merged[2].result.id, "2");
    assert_eq!(merged[3].result.id, "4");
}

#[test]
fn test_merge_deduplication() {
    let config = FederatedSearchConfig {
        merge_mode: MergeMode::Interleave,
        max_merged_results: 10,
        ..Default::default()
    };
    let federated = FederatedSearch::new(config);

    // Same ID in both sources
    let local = vec![make_result("1", 0.9)];
    let remote = vec![make_result("1", 0.8)]; // Duplicate

    let (merged, stats) = federated.merge(local, remote);

    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].result.score, 0.9); // Higher score kept
    assert_eq!(stats.duplicates_removed, 1);
}

#[test]
fn test_merge_respects_max_results() {
    let config = FederatedSearchConfig {
        merge_mode: MergeMode::Interleave,
        max_merged_results: 2,
        ..Default::default()
    };
    let federated = FederatedSearch::new(config);

    let local = vec![make_result("1", 0.9), make_result("2", 0.8)];
    let remote = vec![make_result("3", 0.7), make_result("4", 0.6)];

    let (merged, stats) = federated.merge(local, remote);

    assert_eq!(merged.len(), 2);
    assert_eq!(stats.merged_count, 2);
}

#[test]
fn test_federated_result_source() {
    let result = make_result("1", 0.9);

    let local = FederatedResult::local(result.clone());
    assert_eq!(local.source, ResultSource::Local);

    let remote = FederatedResult::remote(result);
    assert_eq!(remote.source, ResultSource::Remote);
}

#[test]
fn test_federated_search_config_default() {
    let config = FederatedSearchConfig::default();
    assert!(!config.enabled);
    assert_eq!(config.min_local_results, 3);
    assert_eq!(config.min_local_confidence, 0.7);
    assert!(config.fallback_on_empty);
}
