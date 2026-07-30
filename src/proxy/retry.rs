//! Retry policies for upstream requests.
//!
//! Provides configurable retry behavior with exponential backoff
//! and jitter for handling transient failures.

use std::time::Duration;

/// Retry policy configuration.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum number of retry attempts.
    pub max_retries: u32,
    /// Initial delay before first retry.
    pub initial_delay: Duration,
    /// Maximum delay between retries.
    pub max_delay: Duration,
    /// Multiplier for exponential backoff (e.g., 2.0 doubles delay each retry).
    pub backoff_multiplier: f64,
    /// Jitter factor (0.0 to 1.0) to randomize delays.
    pub jitter_factor: f32,
    /// Which errors to retry on.
    pub retry_on: RetryCondition,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(10),
            backoff_multiplier: 2.0,
            jitter_factor: 0.1,
            retry_on: RetryCondition::default(),
        }
    }
}

impl RetryPolicy {
    /// Create a policy with no retries.
    pub fn no_retry() -> Self {
        Self {
            max_retries: 0,
            ..Default::default()
        }
    }

    /// Create a policy for quick retries (useful for network blips).
    pub fn quick() -> Self {
        Self {
            max_retries: 2,
            initial_delay: Duration::from_millis(50),
            max_delay: Duration::from_millis(200),
            backoff_multiplier: 2.0,
            jitter_factor: 0.1,
            retry_on: RetryCondition::default(),
        }
    }

    /// Create a policy for patient retries (useful for overloaded upstreams).
    pub fn patient() -> Self {
        Self {
            max_retries: 5,
            initial_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
            backoff_multiplier: 2.0,
            jitter_factor: 0.2,
            retry_on: RetryCondition::all(),
        }
    }

    /// Calculate the delay for a given attempt number (0-indexed).
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return Duration::ZERO;
        }

        let exponent = attempt.saturating_sub(1) as i32;
        let base_delay = self.initial_delay.as_secs_f64() * self.backoff_multiplier.powi(exponent);

        let delay_secs = base_delay.min(self.max_delay.as_secs_f64());

        // Apply jitter
        let jitter = if self.jitter_factor > 0.0 {
            // Use attempt number as seed for deterministic jitter
            let jitter_seed_f = attempt as f32 * 0.618;
            let jitter_seed = jitter_seed_f % 1.0;
            let jitter_range = delay_secs * self.jitter_factor as f64;
            jitter_seed as f64 * jitter_range
        } else {
            0.0
        };

        let total_delay = delay_secs + jitter;
        Duration::from_secs_f64(total_delay)
    }

    /// Check if the policy allows retrying after the given attempt.
    pub fn should_retry(&self, attempt: u32) -> bool {
        attempt < self.max_retries
    }
}

/// Conditions under which to retry.
#[derive(Debug, Clone)]
pub struct RetryCondition {
    /// Retry on network errors.
    pub on_network_error: bool,
    /// Retry on timeout.
    pub on_timeout: bool,
    /// Retry on 5xx status codes.
    pub on_server_error: bool,
    /// Retry on 429 (rate limited).
    pub on_rate_limited: bool,
    /// Specific status codes to retry on.
    pub on_status_codes: Vec<u16>,
}

impl Default for RetryCondition {
    fn default() -> Self {
        // Default to retrying on transient errors
        Self {
            on_network_error: true,
            on_timeout: true,
            on_server_error: true,
            on_rate_limited: true,
            on_status_codes: vec![],
        }
    }
}

impl RetryCondition {
    /// Create a condition that retries on all transient errors.
    pub fn all() -> Self {
        Self {
            on_network_error: true,
            on_timeout: true,
            on_server_error: true,
            on_rate_limited: true,
            on_status_codes: vec![],
        }
    }

    /// Create a condition that retries on nothing.
    pub fn none() -> Self {
        Self {
            on_network_error: false,
            on_timeout: false,
            on_server_error: false,
            on_rate_limited: false,
            on_status_codes: vec![],
        }
    }

    /// Check if a status code should be retried.
    pub fn should_retry_status(&self, status: u16) -> bool {
        if self.on_server_error && (500..600).contains(&status) {
            return true;
        }
        if self.on_rate_limited && status == 429 {
            return true;
        }
        self.on_status_codes.contains(&status)
    }
}

/// Result of a retry operation.
#[derive(Debug)]
pub struct RetryResult<T, E> {
    /// The final result.
    pub result: Result<T, E>,
    /// Number of attempts made.
    pub attempts: u32,
    /// Total time spent including delays.
    pub total_duration: Duration,
}

impl<T, E> RetryResult<T, E> {
    /// Check if the operation succeeded.
    pub fn is_success(&self) -> bool {
        self.result.is_ok()
    }

    /// Get the result, discarding retry metadata.
    pub fn into_result(self) -> Result<T, E> {
        self.result
    }

    /// Check if any retries were needed.
    pub fn retried(&self) -> bool {
        self.attempts > 1
    }
}

/// Execute an operation with retries.
pub struct RetryExecutor {
    policy: RetryPolicy,
}

impl RetryExecutor {
    /// Create a new retry executor with the given policy.
    pub fn new(policy: RetryPolicy) -> Self {
        Self { policy }
    }

    /// Execute an async operation with retries.
    ///
    /// The operation function receives the current attempt number (0-indexed).
    pub async fn execute<F, Fut, T, E>(&self, mut operation: F) -> RetryResult<T, E>
    where
        F: FnMut(u32) -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
        E: RetryableError,
    {
        let start = std::time::Instant::now();
        let mut attempt = 0;

        loop {
            // Wait for delay (if not first attempt)
            let delay = self.policy.delay_for_attempt(attempt);
            if delay > Duration::ZERO {
                tokio::time::sleep(delay).await;
            }

            let result = operation(attempt).await;

            match &result {
                Ok(_) => {
                    return RetryResult {
                        result,
                        attempts: attempt.saturating_add(1),
                        total_duration: start.elapsed(),
                    };
                }
                Err(e) => {
                    // After attempt N fails, we've done N retries (attempt 0 is initial, 1+ are retries)
                    // Check if we can do another retry
                    let can_retry =
                        self.policy.should_retry(attempt) && e.is_retryable(&self.policy.retry_on);

                    if !can_retry {
                        return RetryResult {
                            result,
                            attempts: attempt.saturating_add(1),
                            total_duration: start.elapsed(),
                        };
                    }
                }
            }

            attempt = attempt.saturating_add(1);
        }
    }
}

/// Trait for errors that can be checked for retry eligibility.
pub trait RetryableError {
    /// Check if this error should trigger a retry given the conditions.
    fn is_retryable(&self, condition: &RetryCondition) -> bool;
}

/// Simple retryable error wrapper.
#[derive(Debug, Clone)]
pub enum RetryError {
    /// Network error.
    Network(String),
    /// Timeout.
    Timeout,
    /// Server returned an error status.
    Status(u16, String),
    /// Other error.
    Other(String),
}

impl RetryableError for RetryError {
    fn is_retryable(&self, condition: &RetryCondition) -> bool {
        match self {
            RetryError::Network(_) => condition.on_network_error,
            RetryError::Timeout => condition.on_timeout,
            RetryError::Status(code, _) => condition.should_retry_status(*code),
            RetryError::Other(_) => false,
        }
    }
}

impl std::fmt::Display for RetryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RetryError::Network(msg) => write!(f, "Network error: {}", msg),
            RetryError::Timeout => write!(f, "Request timed out"),
            RetryError::Status(code, msg) => write!(f, "Status {}: {}", code, msg),
            RetryError::Other(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for RetryError {}

#[cfg(test)]
#[path = "tests/retry_tests.rs"]
mod tests;
