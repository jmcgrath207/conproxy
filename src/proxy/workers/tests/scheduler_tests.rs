#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn test_scheduler_creation() {
    let cancel = CancellationToken::new();
    let scheduler = WorkerScheduler::new(cancel);
    assert_eq!(scheduler.task_count(), 0);
    assert_eq!(scheduler.enabled_count(), 0);
    assert!(scheduler.task_names().is_empty());
}

#[test]
fn test_scheduler_register_tasks() {
    let cancel = CancellationToken::new();
    let mut scheduler = WorkerScheduler::new(cancel);

    scheduler.register("health_check", Duration::from_secs(30), || {
        Box::pin(async {})
    });
    scheduler.register("cleanup", Duration::from_secs(300), || Box::pin(async {}));
    scheduler.register("version_check", Duration::from_secs(60), || {
        Box::pin(async {})
    });

    assert_eq!(scheduler.task_count(), 3);
    assert_eq!(scheduler.enabled_count(), 3);
}

#[test]
fn test_scheduler_task_names() {
    let cancel = CancellationToken::new();
    let mut scheduler = WorkerScheduler::new(cancel);

    scheduler.register("alpha", Duration::from_secs(1), || Box::pin(async {}));
    scheduler.register("beta", Duration::from_secs(1), || Box::pin(async {}));
    scheduler.register_optional(
        "gamma",
        Duration::from_secs(1),
        false,
        || Box::pin(async {}),
    );

    let names = scheduler.task_names();
    assert_eq!(names, vec!["alpha", "beta", "gamma"]);
}

#[test]
fn test_scheduler_disabled_tasks() {
    let cancel = CancellationToken::new();
    let mut scheduler = WorkerScheduler::new(cancel);

    scheduler.register("enabled_1", Duration::from_secs(1), || Box::pin(async {}));
    scheduler.register_optional("disabled_1", Duration::from_secs(1), false, || {
        Box::pin(async {})
    });
    scheduler.register("enabled_2", Duration::from_secs(1), || Box::pin(async {}));
    scheduler.register_optional("disabled_2", Duration::from_secs(1), false, || {
        Box::pin(async {})
    });

    assert_eq!(scheduler.task_count(), 4);
    assert_eq!(scheduler.enabled_count(), 2);
}

#[tokio::test]
async fn test_scheduler_run_and_shutdown() {
    let cancel = CancellationToken::new();
    let mut scheduler = WorkerScheduler::new(cancel.clone());

    let counter_a = Arc::new(AtomicUsize::new(0));
    let counter_b = Arc::new(AtomicUsize::new(0));

    let ca = counter_a.clone();
    scheduler.register("task_a", Duration::from_millis(10), move || {
        let ca = ca.clone();
        Box::pin(async move {
            ca.fetch_add(1, Ordering::Relaxed);
        })
    });

    let cb = counter_b.clone();
    scheduler.register("task_b", Duration::from_millis(20), move || {
        let cb = cb.clone();
        Box::pin(async move {
            cb.fetch_add(1, Ordering::Relaxed);
        })
    });

    // Spawn the scheduler and let it run briefly.
    let handle = tokio::spawn(async move {
        scheduler.run().await;
    });

    // Let tasks tick a few times.
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel.cancel();

    // Wait for scheduler to finish.
    handle.await.unwrap();

    // Both counters should have been incremented at least once.
    let a = counter_a.load(Ordering::Relaxed);
    let b = counter_b.load(Ordering::Relaxed);
    assert!(a >= 1, "task_a should have run at least once, got {a}");
    assert!(b >= 1, "task_b should have run at least once, got {b}");

    // task_a (10ms interval) should have run at least as often as task_b (20ms interval).
    assert!(
        a >= b,
        "task_a ({a}) should have run at least as often as task_b ({b})"
    );
}

#[tokio::test]
async fn test_scheduler_empty_run() {
    let cancel = CancellationToken::new();
    let scheduler = WorkerScheduler::new(cancel.clone());

    assert_eq!(scheduler.task_count(), 0);

    let handle = tokio::spawn(async move {
        scheduler.run().await;
    });

    // Give a moment then cancel.
    tokio::time::sleep(Duration::from_millis(20)).await;
    cancel.cancel();

    // Should complete without panic.
    handle.await.unwrap();
}
