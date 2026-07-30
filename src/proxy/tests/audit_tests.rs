#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

#[test]
fn test_audit_entry_creation() {
    let entry = AuditEntry::new(1, "test query", Some(10));
    assert_eq!(entry.request_id, 1);
    assert_eq!(entry.query, "test query");
    assert_eq!(entry.top_k, Some(10));
    assert!(!entry.success);
}

#[test]
fn test_audit_entry_with_success() {
    let entry = AuditEntry::new(1, "query", None)
        .with_success(5)
        .with_cache_status(CacheStatus::Hit)
        .with_latency(Duration::from_millis(50));

    assert!(entry.success);
    assert_eq!(entry.result_count, 5);
    assert_eq!(entry.cache_status, CacheStatus::Hit);
    assert_eq!(entry.latency_ms, 50);
}

#[test]
fn test_audit_entry_with_error() {
    let entry = AuditEntry::new(1, "query", None).with_error("connection failed");

    assert!(!entry.success);
    assert_eq!(entry.error, Some("connection failed".to_string()));
}

#[test]
fn test_audit_entry_truncation() {
    let long_query = "x".repeat(300);
    let entry = AuditEntry::new(1, &long_query, None);

    assert!(entry.query.len() < 300);
    assert!(entry.query.ends_with("..."));
}

#[test]
fn test_audit_builder() {
    let builder = AuditBuilder::new(1, "query", Some(10))
        .cache_status(CacheStatus::Miss)
        .client_id("test-client");

    let entry = builder.success(3);

    assert!(entry.success);
    assert_eq!(entry.result_count, 3);
    assert_eq!(entry.cache_status, CacheStatus::Miss);
    assert_eq!(entry.client_id, Some("test-client".to_string()));
}

#[test]
fn test_audit_log_creation() {
    let log = AuditLog::new(100);
    assert_eq!(log.stats().entries_in_buffer, 0);
}

#[test]
fn test_audit_log_logging() {
    let log = AuditLog::new(100);

    let entry = log.start("query 1", None).success(5);
    log.log(entry);

    let entry = log.start("query 2", None).error("failed");
    log.log(entry);

    let stats = log.stats();
    assert_eq!(stats.entries_in_buffer, 2);
    assert_eq!(stats.total_logged, 2);
    assert_eq!(stats.success_rate, 0.5);
}

#[test]
fn test_audit_log_recent() {
    let log = AuditLog::new(100);

    for i in 0..5 {
        let entry = AuditEntry::new(i + 1, &format!("query {}", i), None).with_success(1);
        log.log(entry);
    }

    let recent = log.recent(3);
    assert_eq!(recent.len(), 3);
    // Newest first
    assert_eq!(recent[0].request_id, 5);
    assert_eq!(recent[1].request_id, 4);
    assert_eq!(recent[2].request_id, 3);
}

#[test]
fn test_audit_log_ring_buffer() {
    let log = AuditLog::new(3);

    // Fill beyond capacity
    for i in 0..5 {
        let entry = AuditEntry::new(i + 1, &format!("query {}", i), None).with_success(1);
        log.log(entry);
    }

    let stats = log.stats();
    assert_eq!(stats.entries_in_buffer, 3);
    assert_eq!(stats.total_logged, 5);

    let recent = log.recent(3);
    assert_eq!(recent.len(), 3);
}

#[test]
fn test_audit_log_filter() {
    let log = AuditLog::new(100);

    let entry = AuditEntry::new(1, "query 1", None)
        .with_success(1)
        .with_client_id("client-a");
    log.log(entry);

    let entry = AuditEntry::new(2, "query 2", None)
        .with_error("failed")
        .with_client_id("client-b");
    log.log(entry);

    let entry = AuditEntry::new(3, "query 3", None)
        .with_success(1)
        .with_client_id("client-a");
    log.log(entry);

    let client_a = log.for_client("client-a", 10);
    assert_eq!(client_a.len(), 2);

    let failures = log.failures(10);
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].request_id, 2);
}

#[test]
fn test_audit_log_slow() {
    let log = AuditLog::new(100);

    let entry = AuditEntry::new(1, "fast", None)
        .with_success(1)
        .with_latency(Duration::from_millis(10));
    log.log(entry);

    let entry = AuditEntry::new(2, "slow", None)
        .with_success(1)
        .with_latency(Duration::from_millis(500));
    log.log(entry);

    let slow = log.slow(100, 10);
    assert_eq!(slow.len(), 1);
    assert_eq!(slow[0].request_id, 2);
}

#[test]
fn test_audit_log_clear() {
    let log = AuditLog::new(100);

    let entry = AuditEntry::new(1, "query", None).with_success(1);
    log.log(entry);

    assert_eq!(log.stats().entries_in_buffer, 1);

    log.clear();
    assert_eq!(log.stats().entries_in_buffer, 0);
}

#[test]
fn test_audit_log_next_request_id() {
    let log = AuditLog::new(100);

    assert_eq!(log.next_request_id(), 1);
    assert_eq!(log.next_request_id(), 2);
    assert_eq!(log.next_request_id(), 3);
}
