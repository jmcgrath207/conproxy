//! In-process load benchmarks for the conproxy.
//!
//! Each `#[test]` (run with `--release`) spins up a `MockUpstream`, runs a
//! fixed-request workload against it via the proxy library code, and
//! reports latency stats. No external proxy process or Docker required.
//!
//! Run with:
//!   cargo test --release --test load_benches --features "load-test,e2e" -- --nocapture --test-threads=1
//!
//! Skipped with `BENCH_SKIP=1` for CI environments without a tight clock.
//! Each test uses an ephemeral port and `MockUpstream::stop()` on cleanup.
//!
//! Env vars:
//!   BENCH_SKIP              = 1 → skip all benches (CI mode)
//!   BENCH_PURE_CACHE_N      = request count for PureCacheBench (default 5000)
//!   BENCH_GRPC_ONLY_N       = request count for GrpcOnlyBench (default 2000)
//!   BENCH_EVICTION_N        = request count for CacheEvictionBench (default 3000)
//!   BENCH_EVICTION_POOL     = unique query pool size (default 200)

#![cfg(feature = "load-test")]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use common::mock_upstream::{MockUpstream, ResponseMode};
use hdrhistogram::Histogram;
use std::sync::Arc;
use std::time::{Duration, Instant};

mod common;

const SKIP_ENV: &str = "BENCH_SKIP";

fn should_skip() -> bool {
    std::env::var(SKIP_ENV).ok().as_deref() == Some("1")
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn percentile(h: &Histogram<u64>, p: f64) -> u64 {
    h.value_at_percentile(p)
}

fn summarize(name: &str, h: &Histogram<u64>, total: Duration) {
    println!(
        "  {name}: n={} p50={}us p99={}us p999={}us max={}us total={}ms rps={:.0}",
        h.len(),
        percentile(h, 50.0),
        percentile(h, 99.0),
        percentile(h, 99.9),
        h.max(),
        total.as_millis(),
        h.len() as f64 / total.as_secs_f64().max(0.001),
    );
}

// ─── PureCacheBench ────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bench_pure_cache() {
    if should_skip() {
        eprintln!("BENCH_SKIP=1 set, skipping bench_pure_cache");
        return;
    }
    let n = env_usize("BENCH_PURE_CACHE_N", 5000);
    let (server, url) =
        MockUpstream::start_with_mode(ResponseMode::Success { result_count: 5 }).await;
    let adapter = Arc::new(
        conproxy::proxy::upstream::GenericRestAdapter::new(&url, Duration::from_secs(5))
            .expect("adapter"),
    );

    let hist = Arc::new(parking_lot::Mutex::new(
        Histogram::<u64>::new(3).expect("histogram"),
    ));

    let start = Instant::now();
    let mut handles = vec![];
    for i in 0..4 {
        let adapter = adapter.clone();
        let hist = hist.clone();
        handles.push(tokio::spawn(async move {
            let req = conproxy::proxy::QueryRequest {
                query: format!("bench-query-{i}"),
                top_k: Some(5),
                priority: None,
                upstream_id: None,
                upstream_type: None,
            };
            for j in 0..(n / 4) {
                let t0 = Instant::now();
                let _ = adapter.query(&req).await;
                let dt_us = t0.elapsed().as_micros() as u64;
                hist.lock().record(dt_us).ok();
                let _ = j;
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    let total = start.elapsed();

    let h = hist.lock();
    summarize("pure_cache", &h, total);
    drop(h);
    server.stop().await;
}

// ─── GrpcOnlyBench (HTTP-only, since gRPC needs tonic + channel setup) ───

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bench_grpc_only_http() {
    // Note: gRPC tonic channel setup is heavyweight; this bench uses HTTP
    // for fast iteration. Pure gRPC bench needs full client wiring — see plan.
    if should_skip() {
        eprintln!("BENCH_SKIP=1 set, skipping bench_grpc_only_http");
        return;
    }
    let n = env_usize("BENCH_GRPC_ONLY_N", 2000);
    let (server, url) =
        MockUpstream::start_with_mode(ResponseMode::Success { result_count: 3 }).await;
    let adapter = Arc::new(
        conproxy::proxy::upstream::GenericRestAdapter::new(&url, Duration::from_secs(5))
            .expect("adapter"),
    );

    let hist = Arc::new(parking_lot::Mutex::new(
        Histogram::<u64>::new(3).expect("histogram"),
    ));
    let start = Instant::now();
    let mut handles = vec![];
    for worker in 0..4 {
        let adapter = adapter.clone();
        let hist = hist.clone();
        handles.push(tokio::spawn(async move {
            let req = conproxy::proxy::QueryRequest {
                query: format!("grpc-bench-{worker}"),
                top_k: Some(3),
                priority: None,
                upstream_id: None,
                upstream_type: None,
            };
            for j in 0..(n / 4) {
                let t0 = Instant::now();
                let _ = adapter.query(&req).await;
                hist.lock().record(t0.elapsed().as_micros() as u64).ok();
                let _ = j;
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    let total = start.elapsed();

    let h = hist.lock();
    summarize("grpc_only_http_proxy", &h, total);
    drop(h);
    server.stop().await;
}

// ─── CacheEvictionBench ────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bench_cache_eviction() {
    if should_skip() {
        eprintln!("BENCH_SKIP=1 set, skipping bench_cache_eviction");
        return;
    }
    let n = env_usize("BENCH_EVICTION_N", 3000);
    let pool_size = env_usize("BENCH_EVICTION_POOL", 200);
    let (server, url) =
        MockUpstream::start_with_mode(ResponseMode::Success { result_count: 1 }).await;
    let adapter = Arc::new(
        conproxy::proxy::upstream::GenericRestAdapter::new(&url, Duration::from_secs(5))
            .expect("adapter"),
    );

    let hist = Arc::new(parking_lot::Mutex::new(
        Histogram::<u64>::new(3).expect("histogram"),
    ));
    let start = Instant::now();
    let mut handles = vec![];
    for worker in 0..4 {
        let adapter = adapter.clone();
        let hist = hist.clone();
        handles.push(tokio::spawn(async move {
            for j in 0..(n / 4) {
                // High-cardinality queries: 200 unique rotating queries
                // (forces high churn; mock upstream doesn't cache so each is a fresh fetch)
                let q_id = (worker * 1000 + j) % pool_size;
                let req = conproxy::proxy::QueryRequest {
                    query: format!("evict-q-{q_id}"),
                    top_k: Some(1),
                    priority: None,
                    upstream_id: None,
                    upstream_type: None,
                };
                let t0 = Instant::now();
                let _ = adapter.query(&req).await;
                hist.lock().record(t0.elapsed().as_micros() as u64).ok();
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    let total = start.elapsed();

    let h = hist.lock();
    summarize("cache_eviction_high_churn", &h, total);
    drop(h);
    server.stop().await;
}
