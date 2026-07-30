//! Unified worker scheduler for background proxy tasks.
//!
//! Consolidates all background workers into a single scheduling framework
//! with one cancellation token, coordinated shutdown, and unified logging.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// Type alias for the async handler closure.
type TaskHandler = dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync;

/// A scheduled task that the WorkerScheduler runs.
pub struct ScheduledTask {
    /// Name of the task (for logging/metrics).
    pub name: &'static str,
    /// Interval between runs.
    pub interval: Duration,
    /// The async handler function.
    handler: Arc<TaskHandler>,
    /// Whether this task is enabled.
    pub enabled: bool,
}

/// Unified scheduler for all background workers.
///
/// Each registered task runs on its own interval inside a spawned tokio task.
/// All tasks share the same [`CancellationToken`] for coordinated shutdown.
pub struct WorkerScheduler {
    tasks: Vec<ScheduledTask>,
    cancel: CancellationToken,
}

impl WorkerScheduler {
    /// Create a new, empty scheduler bound to the given cancellation token.
    pub fn new(cancel: CancellationToken) -> Self {
        Self {
            tasks: Vec::new(),
            cancel,
        }
    }

    /// Register a new scheduled task.
    ///
    /// The `handler` closure is called every `interval`. It must return a
    /// pinned, boxed, `Send` future so it can be driven inside a spawned task.
    pub fn register(
        &mut self,
        name: &'static str,
        interval: Duration,
        handler: impl Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync + 'static,
    ) {
        self.tasks.push(ScheduledTask {
            name,
            interval,
            handler: Arc::new(handler),
            enabled: true,
        });
    }

    /// Register a task that may be disabled.
    ///
    /// Disabled tasks are tracked (appear in [`task_names`]) but are not
    /// spawned when [`run`] is called.
    pub fn register_optional(
        &mut self,
        name: &'static str,
        interval: Duration,
        enabled: bool,
        handler: impl Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync + 'static,
    ) {
        self.tasks.push(ScheduledTask {
            name,
            interval,
            handler: Arc::new(handler),
            enabled,
        });
    }

    /// Get the total number of registered tasks (enabled + disabled).
    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }

    /// Get the number of **enabled** tasks.
    pub fn enabled_count(&self) -> usize {
        self.tasks.iter().filter(|t| t.enabled).count()
    }

    /// Get task names in registration order.
    pub fn task_names(&self) -> Vec<&'static str> {
        self.tasks.iter().map(|t| t.name).collect()
    }

    /// Run all enabled scheduled tasks.
    ///
    /// Each enabled task is spawned as a separate tokio task that loops on its
    /// own interval. The method awaits until all spawned tasks exit, which
    /// happens when the cancellation token is triggered.
    ///
    /// If there are no enabled tasks the method simply awaits cancellation.
    pub async fn run(&self) {
        let mut handles = Vec::new();

        for task in &self.tasks {
            if !task.enabled {
                continue;
            }

            let cancel = self.cancel.clone();
            let interval = task.interval;
            let handler = Arc::clone(&task.handler);

            let handle = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            break;
                        }
                        _ = tokio::time::sleep(interval) => {
                            handler().await;
                        }
                    }
                }
            });

            handles.push(handle);
        }

        // If nothing was spawned, just wait for cancellation.
        if handles.is_empty() {
            self.cancel.cancelled().await;
            return;
        }

        // Wait for all tasks to finish (they exit when cancel fires).
        for handle in handles {
            let _ = handle.await;
        }
    }

    /// Cancel the shared token, causing all running tasks to exit.
    pub fn shutdown(&self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
#[path = "tests/scheduler_tests.rs"]
mod tests;
