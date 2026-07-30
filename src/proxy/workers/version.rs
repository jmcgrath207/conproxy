//! Version check worker for detecting upstream corpus changes.
//!
//! Periodically polls upstream version endpoints to detect when
//! the corpus has been updated, triggering cache invalidation.

use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::proxy::cache::CacheStore;

/// Upstream corpus version information.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UpstreamVersion {
    /// Corpus version string (e.g., "v1.2.3" or timestamp).
    pub version: Option<String>,
    /// Hash of the corpus (if provided by upstream).
    pub corpus_hash: Option<String>,
    /// Document count (if provided).
    pub doc_count: Option<u64>,
    /// Last modified timestamp (Unix epoch seconds).
    pub last_modified: Option<u64>,
}

impl UpstreamVersion {
    /// Create a new version with just a version string.
    pub fn new(version: String) -> Self {
        Self {
            version: Some(version),
            ..Default::default()
        }
    }

    /// Create with full information.
    pub fn full(
        version: Option<String>,
        corpus_hash: Option<String>,
        doc_count: Option<u64>,
        last_modified: Option<u64>,
    ) -> Self {
        Self {
            version,
            corpus_hash,
            doc_count,
            last_modified,
        }
    }

    /// Check if this version is different from another (indicating a change).
    pub fn differs_from(&self, other: &UpstreamVersion) -> bool {
        // If both have version strings, compare them
        if let (Some(a), Some(b)) = (&self.version, &other.version) {
            if a != b {
                return true;
            }
        }

        // If both have corpus hashes, compare them
        if let (Some(a), Some(b)) = (&self.corpus_hash, &other.corpus_hash) {
            if a != b {
                return true;
            }
        }

        // If both have doc counts and they differ significantly
        if let (Some(a), Some(b)) = (self.doc_count, other.doc_count) {
            let diff = if a > b {
                a.saturating_sub(b)
            } else {
                b.saturating_sub(a)
            };
            let max_val = a.max(b) as f64;
            let threshold = (max_val * 0.01) as u64; // 1% change threshold
            if diff > threshold {
                return true;
            }
        }

        // If both have last_modified and they differ
        if let (Some(a), Some(b)) = (self.last_modified, other.last_modified) {
            if a != b {
                return true;
            }
        }

        false
    }

    /// Check if we have any version information.
    pub fn is_empty(&self) -> bool {
        self.version.is_none()
            && self.corpus_hash.is_none()
            && self.doc_count.is_none()
            && self.last_modified.is_none()
    }
}

/// Configuration for version checking.
#[derive(Debug, Clone)]
pub struct VersionCheckConfig {
    /// URL of the version endpoint.
    pub version_endpoint: String,
    /// How often to check (default: 5 minutes).
    pub poll_interval: Duration,
    /// Request timeout.
    pub timeout: Duration,
    /// Whether to invalidate all entries on version change.
    pub invalidate_on_change: bool,
}

impl Default for VersionCheckConfig {
    fn default() -> Self {
        Self {
            version_endpoint: String::new(),
            poll_interval: Duration::from_secs(300), // 5 minutes
            timeout: Duration::from_secs(10),
            invalidate_on_change: true,
        }
    }
}

/// Background worker that monitors upstream version.
pub struct VersionCheckWorker {
    /// Cache to invalidate on version change.
    cache: Arc<CacheStore>,
    /// Configuration.
    config: VersionCheckConfig,
    /// Current known version.
    current_version: Arc<RwLock<UpstreamVersion>>,
    /// HTTP client for version requests.
    client: reqwest::Client,
    /// Number of version changes detected.
    changes_detected: std::sync::atomic::AtomicU64,
    /// Cancellation token.
    cancel: CancellationToken,
}

impl VersionCheckWorker {
    /// Create a new version check worker.
    pub fn new(
        cache: Arc<CacheStore>,
        config: VersionCheckConfig,
        cancel: CancellationToken,
    ) -> Self {
        let client = super::super::socket_tuning::create_tuned_client_default(config.timeout)
            .build()
            .unwrap_or_default();

        Self {
            cache,
            config,
            current_version: Arc::new(RwLock::new(UpstreamVersion::default())),
            client,
            changes_detected: std::sync::atomic::AtomicU64::new(0),
            cancel,
        }
    }

    /// Get the current known version.
    pub fn current_version(&self) -> UpstreamVersion {
        self.current_version.read().clone()
    }

    /// Get the number of version changes detected.
    pub fn changes_detected(&self) -> u64 {
        self.changes_detected
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Run the version check worker loop.
    pub async fn run(&self) {
        if self.config.version_endpoint.is_empty() {
            // No endpoint configured, exit immediately
            return;
        }

        let mut interval = tokio::time::interval(self.config.poll_interval);
        // Do an immediate check
        interval.tick().await;

        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    break;
                }
                _ = interval.tick() => {
                    self.check_version().await;
                }
            }
        }
    }

    /// Perform a single version check.
    async fn check_version(&self) {
        match self.fetch_version().await {
            Ok(new_version) => {
                let current = self.current_version.read().clone();

                if !current.is_empty() && new_version.differs_from(&current) {
                    // Version changed!
                    self.changes_detected
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                    if self.config.invalidate_on_change {
                        self.cache.clear();
                    }
                }

                // Update current version
                *self.current_version.write() = new_version;
            }
            Err(_) => {
                // Failed to fetch version, skip this check
            }
        }
    }

    /// Fetch version from upstream.
    async fn fetch_version(&self) -> Result<UpstreamVersion, ()> {
        let response = self
            .client
            .get(&self.config.version_endpoint)
            .send()
            .await
            .map_err(|_| ())?;

        if !response.status().is_success() {
            return Err(());
        }

        // Try to parse as JSON
        let body = tokio::time::timeout(self.config.timeout, response.text())
            .await
            .map_err(|_| ())? // timeout elapsed
            .map_err(|_| ())?; // reqwest error

        // Try different response formats
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
            let version = json
                .get("version")
                .and_then(|v| v.as_str())
                .map(String::from);
            let corpus_hash = json
                .get("corpus_hash")
                .or_else(|| json.get("hash"))
                .and_then(|v| v.as_str())
                .map(String::from);
            let doc_count = json
                .get("doc_count")
                .or_else(|| json.get("count"))
                .and_then(|v| v.as_u64());
            let last_modified = json
                .get("last_modified")
                .or_else(|| json.get("timestamp"))
                .and_then(|v| v.as_u64());

            return Ok(UpstreamVersion::full(
                version,
                corpus_hash,
                doc_count,
                last_modified,
            ));
        }

        // Fall back to treating body as version string
        let version = body.trim().to_string();
        if !version.is_empty() {
            return Ok(UpstreamVersion::new(version));
        }

        Err(())
    }
}

#[cfg(test)]
#[path = "tests/version_tests.rs"]
mod tests;
