#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

#[test]
fn test_retry_policy_default() {
    let policy = RetryPolicy::default();
    assert_eq!(policy.max_retries, 3);
    assert!(policy.should_retry(0));
    assert!(policy.should_retry(2));
    assert!(!policy.should_retry(3));
}

#[test]
fn test_retry_policy_no_retry() {
    let policy = RetryPolicy::no_retry();
    assert_eq!(policy.max_retries, 0);
    assert!(!policy.should_retry(0));
}

#[test]
fn test_delay_calculation() {
    let policy = RetryPolicy {
        initial_delay: Duration::from_millis(100),
        max_delay: Duration::from_secs(10),
        backoff_multiplier: 2.0,
        jitter_factor: 0.0,
        ..Default::default()
    };

    assert_eq!(policy.delay_for_attempt(0), Duration::ZERO);
    assert_eq!(policy.delay_for_attempt(1), Duration::from_millis(100));
    assert_eq!(policy.delay_for_attempt(2), Duration::from_millis(200));
    assert_eq!(policy.delay_for_attempt(3), Duration::from_millis(400));
}

#[test]
fn test_delay_capped_at_max() {
    let policy = RetryPolicy {
        initial_delay: Duration::from_secs(1),
        max_delay: Duration::from_secs(5),
        backoff_multiplier: 10.0,
        jitter_factor: 0.0,
        ..Default::default()
    };

    // 10^3 = 1000 seconds, but capped at 5
    assert_eq!(policy.delay_for_attempt(4), Duration::from_secs(5));
}

#[test]
fn test_retry_condition_should_retry_status() {
    let condition = RetryCondition::all();

    assert!(condition.should_retry_status(500));
    assert!(condition.should_retry_status(502));
    assert!(condition.should_retry_status(503));
    assert!(condition.should_retry_status(429));
    assert!(!condition.should_retry_status(400));
    assert!(!condition.should_retry_status(404));
}

#[test]
fn test_retry_condition_specific_codes() {
    let condition = RetryCondition {
        on_server_error: false,
        on_status_codes: vec![503, 504],
        ..Default::default()
    };

    assert!(!condition.should_retry_status(500));
    assert!(condition.should_retry_status(503));
    assert!(condition.should_retry_status(504));
}

#[test]
fn test_retry_error_retryable() {
    let condition = RetryCondition::all();

    assert!(RetryError::Network("test".into()).is_retryable(&condition));
    assert!(RetryError::Timeout.is_retryable(&condition));
    assert!(RetryError::Status(500, "test".into()).is_retryable(&condition));
    assert!(!RetryError::Status(400, "test".into()).is_retryable(&condition));
    assert!(!RetryError::Other("test".into()).is_retryable(&condition));
}

#[test]
fn test_retry_result() {
    let success: RetryResult<i32, RetryError> = RetryResult {
        result: Ok(42),
        attempts: 1,
        total_duration: Duration::from_millis(10),
    };
    assert!(success.is_success());
    assert!(!success.retried());

    let retried: RetryResult<i32, RetryError> = RetryResult {
        result: Ok(42),
        attempts: 3,
        total_duration: Duration::from_millis(500),
    };
    assert!(retried.retried());
}

#[tokio::test]
async fn test_retry_executor_success_first_try() {
    let policy = RetryPolicy::default();
    let executor = RetryExecutor::new(policy);

    let result: RetryResult<i32, RetryError> = executor.execute(|_| async { Ok(42) }).await;

    assert!(result.is_success());
    assert_eq!(result.attempts, 1);
    assert_eq!(result.into_result().unwrap(), 42);
}

#[tokio::test]
async fn test_retry_executor_succeeds_after_retry() {
    let policy = RetryPolicy {
        initial_delay: Duration::from_millis(1),
        ..Default::default()
    };
    let executor = RetryExecutor::new(policy);

    let result: RetryResult<i32, RetryError> = executor
        .execute(|attempt| async move {
            if attempt < 2 {
                Err(RetryError::Network("transient".into()))
            } else {
                Ok(42)
            }
        })
        .await;

    assert!(result.is_success());
    assert_eq!(result.attempts, 3);
}

#[tokio::test]
async fn test_retry_executor_exhausts_retries() {
    let policy = RetryPolicy {
        max_retries: 2,
        initial_delay: Duration::from_millis(1),
        retry_on: RetryCondition::all(),
        ..Default::default()
    };
    let executor = RetryExecutor::new(policy);

    let result: RetryResult<i32, RetryError> = executor
        .execute(|_| async { Err(RetryError::Network("persistent".into())) })
        .await;

    assert!(!result.is_success());
    assert_eq!(result.attempts, 3); // 1 initial + 2 retries
}

#[tokio::test]
async fn test_retry_executor_non_retryable_error() {
    let policy = RetryPolicy::default();
    let executor = RetryExecutor::new(policy);

    let result: RetryResult<i32, RetryError> = executor
        .execute(|_| async { Err(RetryError::Status(400, "bad request".into())) })
        .await;

    assert!(!result.is_success());
    assert_eq!(result.attempts, 1); // No retries for 400
}

#[test]
fn test_retry_policy_quick() {
    let policy = RetryPolicy::quick();
    assert_eq!(policy.max_retries, 2);
    assert_eq!(policy.initial_delay, Duration::from_millis(50));
    assert_eq!(policy.max_delay, Duration::from_millis(200));
    assert_eq!(policy.backoff_multiplier, 2.0);
}

#[test]
fn test_retry_policy_patient() {
    let policy = RetryPolicy::patient();
    assert_eq!(policy.max_retries, 5);
    assert_eq!(policy.initial_delay, Duration::from_millis(500));
    assert_eq!(policy.max_delay, Duration::from_secs(30));
    assert!(policy.retry_on.on_network_error);
    assert!(policy.retry_on.on_timeout);
    assert!(policy.retry_on.on_server_error);
    assert!(policy.retry_on.on_rate_limited);
}

#[test]
fn test_retry_condition_none() {
    let condition = RetryCondition::none();
    assert!(!condition.on_network_error);
    assert!(!condition.on_timeout);
    assert!(!condition.on_server_error);
    assert!(!condition.on_rate_limited);
    assert!(condition.on_status_codes.is_empty());
    assert!(!condition.should_retry_status(500));
    assert!(!condition.should_retry_status(429));
}

#[test]
fn test_retry_result_into_result_error() {
    let result: RetryResult<i32, RetryError> = RetryResult {
        result: Err(RetryError::Timeout),
        attempts: 3,
        total_duration: Duration::from_millis(300),
    };
    assert!(!result.is_success());
    assert!(result.retried());
    assert!(result.into_result().is_err());
}

#[test]
fn test_retry_error_display() {
    assert_eq!(
        RetryError::Network("conn refused".into()).to_string(),
        "Network error: conn refused"
    );
    assert_eq!(RetryError::Timeout.to_string(), "Request timed out");
    assert_eq!(
        RetryError::Status(503, "unavailable".into()).to_string(),
        "Status 503: unavailable"
    );
    assert_eq!(RetryError::Other("unknown".into()).to_string(), "unknown");
}

#[test]
fn test_retry_error_is_retryable_with_none_condition() {
    let condition = RetryCondition::none();
    assert!(!RetryError::Network("test".into()).is_retryable(&condition));
    assert!(!RetryError::Timeout.is_retryable(&condition));
    assert!(!RetryError::Status(500, "test".into()).is_retryable(&condition));
    assert!(!RetryError::Other("test".into()).is_retryable(&condition));
}

#[test]
fn test_retry_condition_429_only() {
    let condition = RetryCondition {
        on_server_error: false,
        on_rate_limited: true,
        on_network_error: false,
        on_timeout: false,
        on_status_codes: vec![],
    };
    assert!(condition.should_retry_status(429));
    assert!(!condition.should_retry_status(500));
    assert!(!condition.should_retry_status(503));
}

#[test]
fn test_delay_with_jitter() {
    let policy = RetryPolicy {
        initial_delay: Duration::from_millis(100),
        max_delay: Duration::from_secs(10),
        backoff_multiplier: 2.0,
        jitter_factor: 0.5,
        ..Default::default()
    };

    // Attempt 1: base 100ms + some jitter
    let d = policy.delay_for_attempt(1);
    assert!(d >= Duration::from_millis(100));
    assert!(d <= Duration::from_millis(200)); // max jitter 50% of 100ms
}
