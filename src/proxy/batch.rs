//! Batch request processing for efficient multi-query handling.
//!
//! Allows clients to submit multiple queries in a single request,
//! reducing round-trip overhead and enabling parallel processing.
//!
//! # Partial failure semantics
//!
//! Default (`fail_fast = false`): **per-item** errors. Each query gets a
//! [`BatchQueryResult`]; `success_count` / `error_count` reflect mixed
//! outcomes and `complete` stays `true` if the batch finishes within timeout.
//!
//! With `fail_fast = true`: stop scheduling further work after the first
//! error (sequential path stops immediately; parallel path marks siblings
//! skipped). `complete` is `false` when the batch aborts early or times out.
//!
//! There is **no** all-or-nothing transaction across upstreams — cache writes
//! and metrics are recorded per successful item only.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::types::{QueryRequest, QueryResponse};

#[cfg(test)]
use super::types::CacheStatus;

/// A batch of queries to process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchRequest {
    /// Individual queries in the batch.
    pub queries: Vec<BatchQuery>,
    /// Whether to fail fast on first error (default: false).
    #[serde(default)]
    pub fail_fast: bool,
    /// Maximum time for entire batch in milliseconds (optional).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// A single query in a batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchQuery {
    /// Unique identifier for this query within the batch.
    pub id: String,
    /// The query text.
    pub query: String,
    /// Number of results to return.
    #[serde(default)]
    pub top_k: Option<usize>,
    /// Request priority (0=low, 1=normal, 2=high, 3=critical).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
}

impl From<BatchQuery> for QueryRequest {
    fn from(bq: BatchQuery) -> Self {
        QueryRequest {
            query: bq.query,
            top_k: bq.top_k,
            priority: bq.priority,
            upstream_id: None,
            upstream_type: None,
        }
    }
}

/// Response for a batch of queries.
#[derive(Debug, Clone, Serialize)]
pub struct BatchResponse {
    /// Results for each query, keyed by query ID.
    pub results: HashMap<String, BatchQueryResult>,
    /// Total time for the batch in milliseconds.
    pub took_ms: u64,
    /// Number of successful queries.
    pub success_count: usize,
    /// Number of failed queries.
    pub error_count: usize,
    /// Whether the batch completed fully (vs fail-fast or timeout).
    pub complete: bool,
}

/// Result for a single query in a batch.
#[derive(Debug, Clone, Serialize)]
pub struct BatchQueryResult {
    /// Whether this query succeeded.
    pub success: bool,
    /// The query response (if successful).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<QueryResponse>,
    /// Error message (if failed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Time for this query in milliseconds.
    pub took_ms: u64,
}

impl BatchQueryResult {
    /// Create a successful result.
    pub fn success(response: QueryResponse) -> Self {
        let took_ms = response.took_ms;
        Self {
            success: true,
            response: Some(response),
            error: None,
            took_ms,
        }
    }

    /// Create a failed result.
    pub fn error(message: &str, took_ms: u64) -> Self {
        Self {
            success: false,
            response: None,
            error: Some(message.to_string()),
            took_ms,
        }
    }
}

/// Configuration for batch processing.
#[derive(Debug, Clone)]
pub struct BatchConfig {
    /// Maximum queries per batch.
    pub max_queries: usize,
    /// Maximum total results across all queries.
    pub max_total_results: usize,
    /// Default timeout per query.
    pub default_query_timeout: Duration,
    /// Maximum batch timeout.
    pub max_batch_timeout: Duration,
    /// Whether to process queries in parallel.
    pub parallel: bool,
    /// Maximum parallel queries.
    pub max_parallel: usize,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            max_queries: 100,
            max_total_results: 1000,
            default_query_timeout: Duration::from_secs(30),
            max_batch_timeout: Duration::from_secs(300),
            parallel: true,
            max_parallel: 10,
        }
    }
}

/// Batch processor for handling multiple queries.
pub struct BatchProcessor {
    config: BatchConfig,
}

impl BatchProcessor {
    /// Create a new batch processor with the given configuration.
    pub fn new(config: BatchConfig) -> Self {
        Self { config }
    }

    /// Create with default configuration.
    pub fn default_config() -> Self {
        Self::new(BatchConfig::default())
    }

    /// Validate a batch request.
    pub fn validate(&self, request: &BatchRequest) -> Result<(), BatchError> {
        if request.queries.is_empty() {
            return Err(BatchError::EmptyBatch);
        }

        if request.queries.len() > self.config.max_queries {
            return Err(BatchError::TooManyQueries {
                count: request.queries.len(),
                max: self.config.max_queries,
            });
        }

        // Check for duplicate IDs
        let mut seen_ids = std::collections::HashSet::new();
        for query in &request.queries {
            if !seen_ids.insert(&query.id) {
                return Err(BatchError::DuplicateId(query.id.clone()));
            }
        }

        Ok(())
    }

    /// Process a batch of queries with a provided query function.
    ///
    /// The query function is called for each query in the batch.
    pub async fn process<F, Fut>(
        &self,
        request: BatchRequest,
        query_fn: F,
    ) -> Result<BatchResponse, BatchError>
    where
        F: Fn(QueryRequest) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<QueryResponse, String>> + Send,
    {
        self.validate(&request)?;

        let start = Instant::now();
        let batch_timeout = request
            .timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(self.config.max_batch_timeout);

        let mut results = HashMap::new();
        let mut success_count: usize = 0;
        let mut error_count: usize = 0;
        let mut complete = true;

        if self.config.parallel && request.queries.len() > 1 {
            // Process in parallel with semaphore for concurrency control
            let semaphore = Arc::new(tokio::sync::Semaphore::new(self.config.max_parallel));
            let query_fn = Arc::new(query_fn);
            let fail_fast = request.fail_fast;
            let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));

            let handles: Vec<_> = request
                .queries
                .into_iter()
                .map(|batch_query| {
                    let semaphore = semaphore.clone();
                    let query_fn = query_fn.clone();
                    let failed = failed.clone();

                    tokio::spawn(async move {
                        // Check if we should skip due to fail-fast
                        if fail_fast && failed.load(std::sync::atomic::Ordering::Relaxed) {
                            return (
                                batch_query.id.clone(),
                                BatchQueryResult::error("Skipped due to fail-fast", 0),
                            );
                        }

                        let _permit = match semaphore.acquire().await {
                            Ok(permit) => permit,
                            Err(_) => {
                                return (
                                    batch_query.id.clone(),
                                    BatchQueryResult::error(
                                        "Batch processing interrupted (shutdown)",
                                        0,
                                    ),
                                )
                            }
                        };
                        let query_start = Instant::now();
                        let query_request = QueryRequest::from(batch_query.clone());

                        let result = match query_fn(query_request).await {
                            Ok(response) => BatchQueryResult::success(response),
                            Err(e) => {
                                if fail_fast {
                                    failed.store(true, std::sync::atomic::Ordering::Relaxed);
                                }
                                BatchQueryResult::error(
                                    &e,
                                    query_start.elapsed().as_millis() as u64,
                                )
                            }
                        };

                        (batch_query.id, result)
                    })
                })
                .collect();

            // Wait for all queries with timeout
            #[allow(clippy::arithmetic_side_effects)]
            let deadline = tokio::time::Instant::now() + batch_timeout;

            for handle in handles {
                match tokio::time::timeout_at(deadline, handle).await {
                    Ok(Ok((id, result))) => {
                        if result.success {
                            success_count = success_count.saturating_add(1);
                        } else {
                            error_count = error_count.saturating_add(1);
                        }
                        results.insert(id, result);
                    }
                    Ok(Err(_)) => {
                        // Task panicked
                        error_count = error_count.saturating_add(1);
                        complete = false;
                    }
                    Err(_) => {
                        // Timeout
                        complete = false;
                        break;
                    }
                }
            }
        } else {
            // Process sequentially
            for batch_query in request.queries {
                // Check timeout
                if start.elapsed() > batch_timeout {
                    complete = false;
                    break;
                }

                let query_start = Instant::now();
                let query_request = QueryRequest::from(batch_query.clone());

                let result = match query_fn(query_request).await {
                    Ok(response) => {
                        success_count = success_count.saturating_add(1);
                        BatchQueryResult::success(response)
                    }
                    Err(e) => {
                        error_count = error_count.saturating_add(1);
                        let result =
                            BatchQueryResult::error(&e, query_start.elapsed().as_millis() as u64);

                        if request.fail_fast {
                            results.insert(batch_query.id, result);
                            complete = false;
                            break;
                        }

                        result
                    }
                };

                results.insert(batch_query.id, result);
            }
        }

        Ok(BatchResponse {
            results,
            took_ms: start.elapsed().as_millis() as u64,
            success_count,
            error_count,
            complete,
        })
    }

    /// Get the configuration.
    pub fn config(&self) -> &BatchConfig {
        &self.config
    }
}

/// Errors that can occur during batch processing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchError {
    /// Batch has no queries.
    EmptyBatch,
    /// Too many queries in batch.
    TooManyQueries { count: usize, max: usize },
    /// Duplicate query ID.
    DuplicateId(String),
}

impl std::fmt::Display for BatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BatchError::EmptyBatch => write!(f, "Batch cannot be empty"),
            BatchError::TooManyQueries { count, max } => {
                write!(f, "Too many queries ({}) in batch, max is {}", count, max)
            }
            BatchError::DuplicateId(id) => write!(f, "Duplicate query ID: {}", id),
        }
    }
}

impl std::error::Error for BatchError {}

#[cfg(test)]
#[path = "tests/batch_tests.rs"]
mod tests;
