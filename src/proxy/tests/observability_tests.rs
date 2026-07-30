#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

#[test]
fn test_request_id_generate_unique() {
    let a = RequestId::generate();
    let b = RequestId::generate();
    assert_ne!(a, b);
    assert!(a.as_str().starts_with("req-"));
}

#[test]
fn test_request_id_from_header_valid() {
    let id = RequestId::from_header("abc-123").unwrap();
    assert_eq!(id.as_str(), "abc-123");
    assert_eq!(id.to_string(), "abc-123");
}

#[test]
fn test_request_id_from_header_rejects_empty() {
    assert!(RequestId::from_header("").is_none());
}

#[test]
fn test_request_id_from_header_rejects_too_long() {
    let long = "x".repeat(129);
    assert!(RequestId::from_header(&long).is_none());
}

#[test]
fn test_mutation_log_ring_buffer() {
    let log = CacheMutationLog::new(3);
    assert!(log.is_empty());

    log.record(MutationAuditEntry::insert("k1", None, None));
    log.record(MutationAuditEntry::insert("k2", None, None));
    log.record(MutationAuditEntry::insert("k3", None, None));
    assert_eq!(log.len(), 3);

    log.record(MutationAuditEntry::insert("k4", None, None));
    assert_eq!(log.len(), 3);
    assert_eq!(log.total_logged(), 4);

    let recent = log.recent(2);
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].key, "k4");
    assert_eq!(recent[1].key, "k3");
}

#[test]
fn test_mutation_log_recent_newest_first() {
    let log = CacheMutationLog::new(10);
    log.record(MutationAuditEntry::insert("first", None, None));
    log.record(MutationAuditEntry::insert("second", None, None));

    let recent = log.recent(10);
    assert_eq!(recent[0].key, "second");
    assert_eq!(recent[1].key, "first");
}

#[test]
fn test_mutation_audit_entry_insert() {
    let rid = RequestId::generate();
    let entry = MutationAuditEntry::insert("query-hash", Some("ctx1".into()), Some(&rid));
    assert_eq!(entry.mutation, CacheMutation::Insert);
    assert_eq!(entry.key, "query-hash");
    assert_eq!(entry.context, Some("ctx1".into()));
    assert!(entry.request_id.is_some());
    assert_eq!(entry.entries_affected, 1);
}

#[test]
fn test_mutation_audit_entry_evict() {
    let entry = MutationAuditEntry::evict("old-key", MutationEvictionReason::Ttl);
    assert_eq!(entry.mutation, CacheMutation::Evict);
    assert_eq!(entry.eviction_reason, Some(MutationEvictionReason::Ttl));
}

#[test]
fn test_mutation_audit_entry_clear() {
    let entry = MutationAuditEntry::clear(ClearScope::All, 42);
    assert_eq!(entry.mutation, CacheMutation::Clear);
    assert_eq!(entry.clear_scope, Some(ClearScope::All));
    assert_eq!(entry.entries_affected, 42);
}

#[test]
fn test_trace_builder_stages() {
    let rid = RequestId::generate();
    let mut builder = TraceBuilder::new(rid);

    let s0 = builder.begin_stage("cache_lookup");
    std::thread::sleep(Duration::from_millis(1));
    builder.end_stage(s0);

    let s1 = builder.begin_stage("upstream_query");
    std::thread::sleep(Duration::from_millis(1));
    builder.end_stage(s1);

    builder.set_cache_hit(false);
    builder.set_upstream("qdrant-1");

    let trace = builder.finish();
    assert_eq!(trace.stages.len(), 2);
    assert_eq!(trace.stages[0].name, "cache_lookup");
    assert_eq!(trace.stages[1].name, "upstream_query");
    assert!(!trace.cache_hit);
    assert_eq!(trace.upstream_name, Some("qdrant-1".to_string()));
    assert!(trace.total_duration_us > 0);
}

#[test]
fn test_trace_builder_cache_hit() {
    let rid = RequestId::generate();
    let mut builder = TraceBuilder::new(rid);
    builder.set_cache_hit(true);

    let trace = builder.finish();
    assert!(trace.cache_hit);
    assert!(trace.stages.is_empty());
}

#[test]
fn test_trace_builder_unfinished_stage() {
    let rid = RequestId::generate();
    let mut builder = TraceBuilder::new(rid);
    builder.begin_stage("never_ended");
    // Don't end the stage — should still produce a valid trace
    let trace = builder.finish();
    assert_eq!(trace.stages.len(), 1);
    // duration_us is unsigned, so any value is valid; just verify the stage was recorded
    let _ = trace.stages[0].duration_us;
}
