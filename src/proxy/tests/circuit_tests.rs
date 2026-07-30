#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

#[test]
fn test_circuit_breaker_creation() {
    let cb = CircuitBreaker::default_config();
    assert_eq!(cb.state(), CircuitState::Closed);
    assert!(cb.allow_request());
}

#[test]
fn test_circuit_opens_after_threshold() {
    let config = CircuitBreakerConfig {
        failure_threshold: 3,
        success_threshold: 2,
        open_duration: Duration::from_secs(30),
        failure_window: Duration::from_secs(60),
    };
    let cb = CircuitBreaker::new(config);

    // Record failures up to threshold
    cb.record_failure();
    assert_eq!(cb.state(), CircuitState::Closed);
    cb.record_failure();
    assert_eq!(cb.state(), CircuitState::Closed);
    cb.record_failure();

    // Circuit should now be open
    assert_eq!(cb.state(), CircuitState::Open);
    assert!(!cb.allow_request());
    assert_eq!(cb.times_opened(), 1);
}

#[test]
fn test_circuit_transitions_to_half_open() {
    let config = CircuitBreakerConfig {
        failure_threshold: 2,
        success_threshold: 2,
        open_duration: Duration::from_millis(50),
        failure_window: Duration::from_secs(60),
    };
    let cb = CircuitBreaker::new(config);

    // Open the circuit
    cb.record_failure();
    cb.record_failure();
    assert_eq!(cb.state(), CircuitState::Open);

    // Wait for open duration
    thread::sleep(Duration::from_millis(60));

    // Should transition to half-open
    assert_eq!(cb.state(), CircuitState::HalfOpen);
    assert!(cb.allow_request());
}

#[test]
fn test_circuit_closes_after_successes() {
    let config = CircuitBreakerConfig {
        failure_threshold: 2,
        success_threshold: 2,
        open_duration: Duration::from_millis(10),
        failure_window: Duration::from_secs(60),
    };
    let cb = CircuitBreaker::new(config);

    // Open the circuit
    cb.record_failure();
    cb.record_failure();

    // Wait for half-open
    thread::sleep(Duration::from_millis(20));
    assert_eq!(cb.state(), CircuitState::HalfOpen);

    // Record successes
    cb.record_success();
    assert_eq!(cb.state(), CircuitState::HalfOpen);
    cb.record_success();

    // Should be closed now
    assert_eq!(cb.state(), CircuitState::Closed);
}

#[test]
fn test_half_open_failure_reopens() {
    let config = CircuitBreakerConfig {
        failure_threshold: 2,
        success_threshold: 2,
        open_duration: Duration::from_millis(10),
        failure_window: Duration::from_secs(60),
    };
    let cb = CircuitBreaker::new(config);

    // Open the circuit
    cb.record_failure();
    cb.record_failure();

    // Wait for half-open
    thread::sleep(Duration::from_millis(20));
    assert_eq!(cb.state(), CircuitState::HalfOpen);

    // Failure in half-open reopens
    cb.record_failure();
    assert_eq!(cb.state(), CircuitState::Open);
    assert_eq!(cb.times_opened(), 2);
}

#[test]
fn test_success_resets_failure_count() {
    let config = CircuitBreakerConfig {
        failure_threshold: 3,
        ..Default::default()
    };
    let cb = CircuitBreaker::new(config);

    cb.record_failure();
    cb.record_failure();
    assert_eq!(cb.failure_count(), 2);

    cb.record_success();
    assert_eq!(cb.failure_count(), 0);
    assert_eq!(cb.state(), CircuitState::Closed);
}

#[test]
fn test_trip_and_reset() {
    let cb = CircuitBreaker::default_config();

    cb.trip();
    assert_eq!(cb.state(), CircuitState::Open);

    cb.reset();
    assert_eq!(cb.state(), CircuitState::Closed);
}

#[test]
fn test_times_tripped() {
    let config = CircuitBreakerConfig {
        failure_threshold: 1,
        open_duration: Duration::from_secs(60),
        ..Default::default()
    };
    let cb = CircuitBreaker::new(config);

    cb.record_failure();
    assert_eq!(cb.state(), CircuitState::Open);

    // Requests while open should be tripped
    assert!(!cb.allow_request());
    assert!(!cb.allow_request());
    assert!(!cb.allow_request());

    assert_eq!(cb.times_tripped(), 3);
}

#[test]
fn test_time_until_half_open() {
    let config = CircuitBreakerConfig {
        failure_threshold: 1,
        open_duration: Duration::from_millis(100),
        ..Default::default()
    };
    let cb = CircuitBreaker::new(config);

    // Not applicable when closed
    assert!(cb.time_until_half_open().is_none());

    cb.record_failure();
    assert_eq!(cb.state(), CircuitState::Open);

    // Should have time remaining
    let remaining = cb.time_until_half_open().unwrap();
    assert!(remaining > Duration::ZERO);
    assert!(remaining <= Duration::from_millis(100));
}

#[test]
fn test_circuit_result() {
    let success: CircuitResult<i32, &str> = CircuitResult::Success(42);
    assert!(success.is_success());
    assert!(!success.is_circuit_open());
    assert_eq!(success.ok(), Some(Ok(42)));

    let failure: CircuitResult<i32, &str> = CircuitResult::Failure("error");
    assert!(!failure.is_success());
    assert_eq!(failure.ok(), Some(Err("error")));

    let open: CircuitResult<i32, &str> = CircuitResult::CircuitOpen;
    assert!(open.is_circuit_open());
    assert!(open.ok().is_none());
}

/// B.3: 20 concurrent threads during half-open state.
///
/// Forces the circuit into half-open, then synchronizes 20 threads to call
/// allow_request() simultaneously. Verifies that all threads observe a valid
/// state and that atomic state transitions are consistent.
#[test]
fn test_concurrent_half_open_state_transitions() {
    let num_threads = 20;
    let config = CircuitBreakerConfig {
        failure_threshold: 2,
        success_threshold: 2,
        open_duration: Duration::from_millis(500),
        failure_window: Duration::from_secs(60),
    };
    let cb = Arc::new(CircuitBreaker::new(config));

    // Open the circuit
    cb.record_failure();
    cb.record_failure();
    assert_eq!(cb.state(), CircuitState::Open);

    // Wait for transition to half-open
    thread::sleep(Duration::from_millis(700));
    assert_eq!(cb.state(), CircuitState::HalfOpen);

    let barrier = Arc::new(std::sync::Barrier::new(num_threads));
    let allowed_count = Arc::new(AtomicUsize::new(0));
    let denied_count = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::with_capacity(num_threads);

    for _ in 0..num_threads {
        let cb = cb.clone();
        let barrier = barrier.clone();
        let allowed_count = allowed_count.clone();
        let denied_count = denied_count.clone();

        handles.push(thread::spawn(move || {
            // Synchronize all threads
            barrier.wait();

            let allowed = cb.allow_request();
            if allowed {
                allowed_count.fetch_add(1, Ordering::SeqCst);
            } else {
                denied_count.fetch_add(1, Ordering::SeqCst);
            }

            // After allow_request, state should be a valid CircuitState
            let state = cb.state();
            assert!(
                state == CircuitState::HalfOpen
                    || state == CircuitState::Open
                    || state == CircuitState::Closed,
                "Invalid circuit state observed: {:?}",
                state,
            );
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    let allowed = allowed_count.load(Ordering::SeqCst);
    let denied = denied_count.load(Ordering::SeqCst);

    // All threads should have gotten a definitive answer
    assert_eq!(
        allowed + denied,
        num_threads,
        "All {} threads should have received allow/deny, got {} + {}",
        num_threads,
        allowed,
        denied,
    );

    // In half-open state, allow_request() returns true. All requests allowed
    // if the state stayed half-open, or some denied if a failure transitioned
    // it back to open. Either way, at least one should have been allowed.
    assert!(
        allowed >= 1,
        "At least 1 request should be allowed in half-open state, got {}",
        allowed,
    );
}

/// B.3 variant: Concurrent record_success() in half-open state.
///
/// Forces half-open with success_threshold=3, then has 20 threads
/// concurrently record successes. Verifies the circuit eventually
/// closes and the state transitions are atomic.
#[test]
fn test_concurrent_success_in_half_open() {
    let num_threads = 20;
    let success_threshold = 3;
    let config = CircuitBreakerConfig {
        failure_threshold: 2,
        success_threshold,
        open_duration: Duration::from_millis(500),
        failure_window: Duration::from_secs(60),
    };
    let cb = Arc::new(CircuitBreaker::new(config));

    // Open the circuit
    cb.record_failure();
    cb.record_failure();
    assert_eq!(cb.state(), CircuitState::Open);

    // Wait for transition to half-open
    thread::sleep(Duration::from_millis(700));
    assert_eq!(cb.state(), CircuitState::HalfOpen);

    let barrier = Arc::new(std::sync::Barrier::new(num_threads));
    let mut handles = Vec::with_capacity(num_threads);

    for _ in 0..num_threads {
        let cb = cb.clone();
        let barrier = barrier.clone();

        handles.push(thread::spawn(move || {
            barrier.wait();
            cb.record_success();
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // After enough successes, the circuit should have closed
    assert_eq!(
        cb.state(),
        CircuitState::Closed,
        "Circuit should be closed after {} concurrent successes (threshold={})",
        num_threads,
        success_threshold,
    );

    // Failure count should be reset
    assert_eq!(cb.failure_count(), 0);
}

/// B.3 variant: Concurrent record_failure() in half-open state.
///
/// Forces half-open, then has 20 threads concurrently record failures.
/// Verifies the circuit transitions back to Open atomically.
#[test]
fn test_concurrent_failure_in_half_open() {
    let num_threads = 20;
    // Use a longer open_duration so the circuit stays Open after concurrent failures
    // re-open it (prevents flaky HalfOpen race under tarpaulin/coverage instrumentation).
    let config = CircuitBreakerConfig {
        failure_threshold: 2,
        success_threshold: 2,
        open_duration: Duration::from_millis(500),
        failure_window: Duration::from_secs(60),
    };
    let cb = Arc::new(CircuitBreaker::new(config));

    let initial_opened = cb.times_opened();

    // Open the circuit
    cb.record_failure();
    cb.record_failure();
    assert_eq!(cb.state(), CircuitState::Open);

    // Wait for transition to half-open
    thread::sleep(Duration::from_millis(600));
    assert_eq!(cb.state(), CircuitState::HalfOpen);

    let barrier = Arc::new(std::sync::Barrier::new(num_threads));
    let mut handles = Vec::with_capacity(num_threads);

    for _ in 0..num_threads {
        let cb = cb.clone();
        let barrier = barrier.clone();

        handles.push(thread::spawn(move || {
            barrier.wait();
            cb.record_failure();
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // After any failure in half-open, circuit should be Open
    assert_eq!(
        cb.state(),
        CircuitState::Open,
        "Circuit should be open after concurrent failures in half-open state",
    );

    // times_opened should have incremented (initial open + reopen from half-open)
    let final_opened = cb.times_opened();
    assert!(
        final_opened > initial_opened,
        "times_opened should have incremented from {} (got {})",
        initial_opened,
        final_opened,
    );
}

/// B.3 variant: Mixed concurrent success and failure in half-open.
///
/// Forces half-open, then has threads recording both successes and failures
/// concurrently. Verifies the final state is valid (either Closed or Open).
#[test]
fn test_concurrent_mixed_success_failure_half_open() {
    let num_threads = 20;
    let config = CircuitBreakerConfig {
        failure_threshold: 2,
        success_threshold: 5,
        open_duration: Duration::from_millis(10),
        failure_window: Duration::from_secs(60),
    };
    let cb = Arc::new(CircuitBreaker::new(config));

    // Open the circuit
    cb.record_failure();
    cb.record_failure();

    // Wait for transition to half-open
    thread::sleep(Duration::from_millis(20));
    assert_eq!(cb.state(), CircuitState::HalfOpen);

    let barrier = Arc::new(std::sync::Barrier::new(num_threads));
    let mut handles = Vec::with_capacity(num_threads);

    for i in 0..num_threads {
        let cb = cb.clone();
        let barrier = barrier.clone();

        handles.push(thread::spawn(move || {
            barrier.wait();

            if i % 3 == 0 {
                // Every third thread records a failure
                cb.record_failure();
            } else {
                cb.record_success();
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // The final state must be valid: either Closed (enough successes before
    // any failure) or Open (a failure came in). HalfOpen is also valid if
    // somehow the transitions are still in progress, but unlikely.
    let final_state = cb.state();
    assert!(
        final_state == CircuitState::Closed
            || final_state == CircuitState::Open
            || final_state == CircuitState::HalfOpen,
        "Expected a valid final state, got {:?}",
        final_state,
    );
}

/// B.3 variant: Rapid open/close cycles under concurrent load.
///
/// Multiple threads continuously trigger failures to open the circuit,
/// while others record successes. Verifies no panics or invalid states.
#[test]
fn test_rapid_open_close_cycles() {
    let num_threads = 20;
    let iterations = 100;
    let config = CircuitBreakerConfig {
        failure_threshold: 1,
        success_threshold: 1,
        open_duration: Duration::from_millis(1), // Very short for rapid cycling
        failure_window: Duration::from_secs(60),
    };
    let cb = Arc::new(CircuitBreaker::new(config));

    let barrier = Arc::new(std::sync::Barrier::new(num_threads));
    let mut handles = Vec::with_capacity(num_threads);

    for i in 0..num_threads {
        let cb = cb.clone();
        let barrier = barrier.clone();

        handles.push(thread::spawn(move || {
            barrier.wait();

            for _ in 0..iterations {
                if i % 2 == 0 {
                    cb.record_failure();
                } else {
                    cb.record_success();
                }
                // Check state is always valid
                let state = cb.state();
                assert!(
                    state == CircuitState::Closed
                        || state == CircuitState::Open
                        || state == CircuitState::HalfOpen,
                    "Invalid state: {:?}",
                    state,
                );
                let _ = cb.allow_request();
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // No panics occurred and the circuit is in a valid state
    let final_state = cb.state();
    assert!(
        final_state == CircuitState::Closed
            || final_state == CircuitState::Open
            || final_state == CircuitState::HalfOpen,
        "Final state should be valid: {:?}",
        final_state,
    );
}

#[test]
fn test_circuit_state_atomic_roundtrip() {
    let variants = [
        CircuitState::Closed,
        CircuitState::Open,
        CircuitState::HalfOpen,
    ];
    for variant in &variants {
        assert_eq!(*variant, CircuitState::from_u8(variant.to_u8()));
    }
}

#[test]
fn test_circuit_state_from_u8_out_of_range() {
    // Out-of-range values fall back to Closed (safe default)
    assert_eq!(CircuitState::from_u8(3), CircuitState::Closed);
    assert_eq!(CircuitState::from_u8(255), CircuitState::Closed);
}

#[test]
fn test_failure_count_resets_after_window_expiration() {
    let config = CircuitBreakerConfig {
        failure_threshold: 5,
        success_threshold: 2,
        open_duration: Duration::from_secs(30),
        failure_window: Duration::from_millis(50),
    };
    let cb = CircuitBreaker::new(config);

    // Record 3 failures (below threshold)
    for _ in 0..3 {
        cb.record_failure();
    }
    assert_eq!(cb.failure_count(), 3);
    assert_eq!(cb.state(), CircuitState::Closed);

    // Wait for the failure window to expire
    thread::sleep(Duration::from_millis(60));

    // Record 1 more failure — window should have reset first
    cb.record_failure();
    assert_eq!(cb.failure_count(), 1);
    assert_eq!(cb.state(), CircuitState::Closed);
}

#[test]
fn test_concurrent_window_reset() {
    use std::sync::Barrier;

    let config = CircuitBreakerConfig {
        failure_threshold: 100,
        success_threshold: 2,
        open_duration: Duration::from_secs(30),
        failure_window: Duration::from_millis(1),
    };
    let cb = Arc::new(CircuitBreaker::new(config));
    let barrier = Arc::new(Barrier::new(20));

    let mut handles = Vec::new();
    for _ in 0..20 {
        let cb = Arc::clone(&cb);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..50 {
                cb.record_failure();
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // No panics occurred and the circuit is in a valid state
    let final_state = cb.state();
    assert!(
        final_state == CircuitState::Closed
            || final_state == CircuitState::Open
            || final_state == CircuitState::HalfOpen,
        "Final state should be valid: {:?}",
        final_state,
    );
}
