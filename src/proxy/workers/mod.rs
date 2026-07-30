//! Background workers for the cache proxy.
//!
//! These workers handle periodic maintenance tasks like health checking,
//! cache refresh, and recovery from upstream failures.

pub mod cleanup;
pub mod health;
pub mod recovery;
pub mod scheduler;
pub mod version;

pub use cleanup::{CleanupConfig, CleanupWorker};
pub use health::{HealthCheckConfig, HealthCheckWorker};
pub use recovery::{RecoveryConfig, RecoveryState, RecoveryTracker, RecoveryWorker};
pub use scheduler::WorkerScheduler;
pub use version::{UpstreamVersion, VersionCheckConfig, VersionCheckWorker};
