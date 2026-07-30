//! Mixed-traffic load test for the conproxy.
//!
//! Single `BenchSuite` implementation that produces realistic traffic:
//! - Zipf-distributed query pool (500 queries, s=1.07)
//! - Weighted request-type mixing (60% query, 15% batch, 10% stats, 10% health, 5% gRPC)
//! - Phased load shaping (warmup → sustain → burst → cooldown)
//! - Per-type latency tracking via hdrhistogram
//!
//! Requires `--features load-test,e2e` and a running proxy.
//!
//! Environment variables:
//!   PROXY_URL        - HTTP base URL (default: http://localhost:8081)
//!   GRPC_URL         - gRPC endpoint (default: http://localhost:8080)
//!   DURATION         - sustain phase duration in seconds (default: 30)
//!   USERS            - base concurrent workers (default: 10)
//!   SEED_DIR         - directory containing seed query files (default: tests/e2e/data)
//!   BENCH_OUTPUT_DIR - write summary.json + rlt output here (optional)

use async_trait::async_trait;
use clap::Parser;
use conproxy::proxy::grpc::proto::{self, search_service_client::SearchServiceClient};
use hdrhistogram::Histogram;
use parking_lot::Mutex;
use rand::distr::Distribution;
use rand::rngs::SmallRng;
use rand::{RngExt, SeedableRng};
use rand_distr::Zipf;
use rlt::cli::BenchCli;
use rlt::{BenchSuite, IterInfo, IterReport, Status};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tonic::transport::Channel;

// ---------------------------------------------------------------------------
// Request types and weights
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RequestType {
    HttpQuery,
    HttpBatch,
    HttpStats,
    HttpHealth,
    GrpcQuery,
}

impl RequestType {
    fn name(&self) -> &'static str {
        match self {
            Self::HttpQuery => "http_query",
            Self::HttpBatch => "http_batch",
            Self::HttpStats => "http_stats",
            Self::HttpHealth => "http_health",
            Self::GrpcQuery => "grpc_query",
        }
    }
}

/// Cumulative weights for request type selection (out of 100).
/// 60% HTTP query, 15% batch, 10% stats, 10% health, 5% gRPC query.
const REQUEST_WEIGHTS: [(RequestType, u8); 5] = [
    (RequestType::HttpQuery, 60),
    (RequestType::HttpBatch, 75),
    (RequestType::HttpStats, 85),
    (RequestType::HttpHealth, 95),
    (RequestType::GrpcQuery, 100),
];

fn pick_request_type(rng: &mut SmallRng) -> RequestType {
    let roll: u8 = rng.random_range(0..100);
    for &(req_type, threshold) in &REQUEST_WEIGHTS {
        if roll < threshold {
            return req_type;
        }
    }
    RequestType::HttpQuery
}

// ---------------------------------------------------------------------------
// Query pool (Zipf distribution)
// ---------------------------------------------------------------------------

const POOL_SIZE: usize = 500;

struct QueryPool {
    queries: Vec<String>,
}

impl QueryPool {
    fn from_seed_files(seed_dir: &Path) -> Self {
        let mut queries = Vec::new();
        for filename in ["seeds_fts.txt", "seeds_semantic.txt"] {
            let path = seed_dir.join(filename);
            if let Ok(content) = std::fs::read_to_string(&path) {
                for line in content.lines() {
                    let line = line.trim();
                    if !line.is_empty() && !line.starts_with('#') {
                        queries.push(line.to_string());
                    }
                }
            }
        }
        if queries.is_empty() {
            queries = vec![
                "zynthar dataflow framework".into(),
                "quorblax graph database".into(),
                "meldrix cache eviction".into(),
            ];
            eprintln!("WARNING: No seed files found, using fallback queries");
        }
        println!("Loaded {} seed queries from files", queries.len());

        // Generate synthetic variations to reach POOL_SIZE
        let mut rng = SmallRng::seed_from_u64(42);
        let suffixes = [
            "tutorial",
            "guide",
            "examples",
            "best practices",
            "performance",
            "comparison",
            "implementation",
            "overview",
            "advanced",
            "introduction",
            "deep dive",
            "optimization",
            "troubleshooting",
        ];
        let prefixes = [
            "how to",
            "understanding",
            "building",
            "scaling",
            "debugging",
            "improving",
            "modern",
            "practical",
            "efficient",
        ];

        let seed_count = queries.len();
        while queries.len() < POOL_SIZE {
            let base = &queries[rng.random_range(0..seed_count)];
            let variation = if rng.random_bool(0.5) {
                let suffix = suffixes[rng.random_range(0..suffixes.len())];
                format!("{base} {suffix}")
            } else {
                let prefix = prefixes[rng.random_range(0..prefixes.len())];
                format!("{prefix} {base}")
            };
            if !queries.contains(&variation) {
                queries.push(variation);
            }
        }

        Self { queries }
    }

    /// Sample a query using Zipf distribution (s=1.07).
    fn sample(&self, rng: &mut SmallRng) -> &str {
        let zipf = Zipf::new(self.queries.len() as f64, 1.07_f64).unwrap();
        let idx = zipf.sample(rng) as usize;
        // Zipf returns 1-based, clamp to valid range
        let idx = idx.saturating_sub(1).min(self.queries.len() - 1);
        &self.queries[idx]
    }
}

// ---------------------------------------------------------------------------
// Per-type latency metrics
// ---------------------------------------------------------------------------

struct TypeMetrics {
    histograms: Mutex<HashMap<RequestType, Histogram<u64>>>,
}

impl TypeMetrics {
    fn new() -> Self {
        Self {
            histograms: Mutex::new(HashMap::new()),
        }
    }

    fn record(&self, req_type: RequestType, duration: Duration) {
        let micros = duration.as_micros() as u64;
        let mut map = self.histograms.lock();
        let hist = map
            .entry(req_type)
            .or_insert_with(|| Histogram::new_with_bounds(1, 60_000_000, 3).unwrap());
        let _ = hist.record(micros);
    }

    /// Build synthetic rlt-compatible JSON entries for each request type.
    fn build_entries(&self, duration_secs: usize, users: usize) -> Vec<BenchmarkResult> {
        let map = self.histograms.lock();
        let mut entries = Vec::new();

        let all_types = [
            RequestType::HttpQuery,
            RequestType::HttpBatch,
            RequestType::HttpStats,
            RequestType::HttpHealth,
            RequestType::GrpcQuery,
        ];

        for &req_type in &all_types {
            let raw_metrics = if let Some(hist) = map.get(&req_type) {
                let total = hist.len();
                let elapsed = duration_secs as f64;
                let rate = if elapsed > 0.0 {
                    total as f64 / elapsed
                } else {
                    0.0
                };

                // Convert histogram micros → seconds for rlt compatibility
                let to_secs = |v: u64| v as f64 / 1_000_000.0;

                serde_json::json!({
                    "summary": {
                        "success_ratio": 1.0,
                        "total_time": elapsed,
                        "concurrency": users,
                        "iters": {
                            "total": total,
                            "rate": rate,
                        },
                        "items": {
                            "total": total,
                            "rate": rate,
                        },
                        "bytes": {
                            "total": 0,
                            "rate": 0.0,
                        }
                    },
                    "latency": {
                        "stats": {
                            "min": to_secs(hist.min()),
                            "max": to_secs(hist.max()),
                            "mean": to_secs(hist.mean() as u64),
                            "median": to_secs(hist.value_at_quantile(0.5)),
                            "stdev": to_secs(hist.stdev() as u64),
                        },
                        "percentiles": {
                            "p10": to_secs(hist.value_at_quantile(0.10)),
                            "p25": to_secs(hist.value_at_quantile(0.25)),
                            "p50": to_secs(hist.value_at_quantile(0.50)),
                            "p75": to_secs(hist.value_at_quantile(0.75)),
                            "p90": to_secs(hist.value_at_quantile(0.90)),
                            "p95": to_secs(hist.value_at_quantile(0.95)),
                            "p99": to_secs(hist.value_at_quantile(0.99)),
                            "p99.9": to_secs(hist.value_at_quantile(0.999)),
                            "p99.99": to_secs(hist.value_at_quantile(0.9999)),
                        }
                    },
                    "status": {
                        "Success(0)": total,
                    },
                    "errors": {}
                })
            } else {
                serde_json::json!({
                    "summary": {
                        "success_ratio": 0.0,
                        "total_time": duration_secs as f64,
                        "concurrency": users,
                        "iters": { "total": 0, "rate": 0.0 },
                        "items": { "total": 0, "rate": 0.0 },
                        "bytes": { "total": 0, "rate": 0.0 }
                    },
                    "status": {},
                    "errors": {}
                })
            };

            entries.push(BenchmarkResult {
                name: req_type.name().to_string(),
                duration_secs,
                users,
                raw_metrics,
            });
        }

        entries
    }
}

// ---------------------------------------------------------------------------
// Phase controller
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Warmup,
    Sustain,
    Burst,
    Cooldown,
}

struct PhaseController {
    start: Instant,
    warmup_dur: Duration,
    sustain_dur: Duration,
    burst_dur: Duration,
    cooldown_dur: Duration,
    base_workers: u32,
}

impl PhaseController {
    fn new(base_workers: u32, sustain_secs: u64) -> Self {
        Self {
            start: Instant::now(),
            warmup_dur: Duration::from_secs(10),
            sustain_dur: Duration::from_secs(sustain_secs),
            burst_dur: Duration::from_secs(5),
            cooldown_dur: Duration::from_secs(5),
            base_workers,
        }
    }

    fn total_duration(&self) -> Duration {
        self.warmup_dur + self.sustain_dur + self.burst_dur + self.cooldown_dur
    }

    fn current_phase(&self) -> Phase {
        let elapsed = self.start.elapsed();
        if elapsed < self.warmup_dur {
            Phase::Warmup
        } else if elapsed < self.warmup_dur + self.sustain_dur {
            Phase::Sustain
        } else if elapsed < self.warmup_dur + self.sustain_dur + self.burst_dur {
            Phase::Burst
        } else {
            Phase::Cooldown
        }
    }

    fn is_worker_active(&self, worker_id: u32) -> bool {
        let elapsed = self.start.elapsed();
        let n = self.base_workers;

        match self.current_phase() {
            Phase::Warmup => {
                // Ramp 1 → N over warmup period
                let progress = elapsed.as_secs_f64() / self.warmup_dur.as_secs_f64();
                let active = (progress * n as f64).ceil() as u32;
                let active = active.max(1).min(n);
                worker_id < active
            }
            Phase::Sustain => {
                // N workers active
                worker_id < n
            }
            Phase::Burst => {
                // 2N workers (all active)
                true
            }
            Phase::Cooldown => {
                // Ramp 2N → 0 over cooldown period
                let cooldown_start = self.warmup_dur + self.sustain_dur + self.burst_dur;
                let cooldown_elapsed = elapsed.saturating_sub(cooldown_start);
                let progress = cooldown_elapsed.as_secs_f64() / self.cooldown_dur.as_secs_f64();
                let total = 2 * n;
                let active = ((1.0 - progress) * total as f64).ceil() as u32;
                worker_id < active
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Worker state
// ---------------------------------------------------------------------------

struct WorkerState {
    http_client: reqwest::Client,
    grpc_search: Option<SearchServiceClient<Channel>>,
    rng: SmallRng,
}

// ---------------------------------------------------------------------------
// Result types for summary.json
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct BenchmarkResult {
    name: String,
    duration_secs: usize,
    users: usize,
    raw_metrics: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Request dispatch functions
// ---------------------------------------------------------------------------

async fn do_http_query(
    client: &reqwest::Client,
    http_url: &str,
    query: &str,
) -> (Duration, Status, u64) {
    let t = Instant::now();
    let resp = client
        .post(format!("{http_url}/query"))
        .json(&serde_json::json!({"query": query}))
        .send()
        .await;
    let duration = t.elapsed();
    match resp {
        Ok(r) => {
            let code = r.status().as_u16();
            let bytes = r.bytes().await.map(|b| b.len() as u64).unwrap_or(0);
            let status = if code < 400 {
                Status::success(code as i64)
            } else {
                Status::server_error(code as i64)
            };
            (duration, status, bytes)
        }
        Err(_) => (duration, Status::server_error(0), 0),
    }
}

async fn do_http_batch(
    client: &reqwest::Client,
    http_url: &str,
    queries: &[&str],
) -> (Duration, Status, u64, u64) {
    let batch: Vec<serde_json::Value> = queries
        .iter()
        .enumerate()
        .map(|(i, q)| serde_json::json!({"id": format!("q{i}"), "query": q}))
        .collect();
    let t = Instant::now();
    let resp = client
        .post(format!("{http_url}/batch"))
        .json(&serde_json::json!({"queries": batch}))
        .send()
        .await;
    let duration = t.elapsed();
    match resp {
        Ok(r) => {
            let code = r.status().as_u16();
            let bytes = r.bytes().await.map(|b| b.len() as u64).unwrap_or(0);
            let status = if code < 400 {
                Status::success(code as i64)
            } else {
                Status::server_error(code as i64)
            };
            (duration, status, bytes, queries.len() as u64)
        }
        Err(_) => (duration, Status::server_error(0), 0, queries.len() as u64),
    }
}

async fn do_http_stats(client: &reqwest::Client, http_url: &str) -> (Duration, Status, u64) {
    let t = Instant::now();
    let resp = client.get(format!("{http_url}/stats")).send().await;
    let duration = t.elapsed();
    match resp {
        Ok(r) => {
            let code = r.status().as_u16();
            let bytes = r.bytes().await.map(|b| b.len() as u64).unwrap_or(0);
            (duration, Status::success(code as i64), bytes)
        }
        Err(_) => (duration, Status::server_error(0), 0),
    }
}

async fn do_http_health(client: &reqwest::Client, http_url: &str) -> (Duration, Status, u64) {
    let t = Instant::now();
    let resp = client.get(format!("{http_url}/health")).send().await;
    let duration = t.elapsed();
    match resp {
        Ok(r) => {
            let code = r.status().as_u16();
            let bytes = r.bytes().await.map(|b| b.len() as u64).unwrap_or(0);
            (duration, Status::success(code as i64), bytes)
        }
        Err(_) => (duration, Status::server_error(0), 0),
    }
}

async fn do_grpc_query(
    client: &mut SearchServiceClient<Channel>,
    query: &str,
) -> (Duration, Status, u64) {
    let t = Instant::now();
    let resp = client
        .query(proto::QueryRequest {
            query: query.into(),
            top_k: 5,
            ..Default::default()
        })
        .await;
    let duration = t.elapsed();
    match resp {
        Ok(r) => {
            let inner = r.into_inner();
            (
                duration,
                Status::success(0),
                inner.results.len() as u64 * 100,
            )
        }
        Err(_) => (duration, Status::server_error(500), 0),
    }
}

// ---------------------------------------------------------------------------
// MixedTrafficBench
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct MixedTrafficBench {
    http_url: String,
    grpc_url: String,
    query_pool: Arc<QueryPool>,
    type_metrics: Arc<TypeMetrics>,
    phase: Arc<PhaseController>,
}

#[async_trait]
impl BenchSuite for MixedTrafficBench {
    type WorkerState = WorkerState;

    async fn state(&self, worker_id: u32) -> anyhow::Result<Self::WorkerState> {
        let http_client = reqwest::Client::new();

        // Attempt gRPC connections (non-fatal if unavailable)
        let grpc_search = match Channel::from_shared(self.grpc_url.clone()) {
            Ok(endpoint) => match endpoint.connect().await {
                Ok(ch) => Some(SearchServiceClient::new(ch)),
                Err(_) => None,
            },
            Err(_) => None,
        };

        Ok(WorkerState {
            http_client,
            grpc_search,
            rng: SmallRng::seed_from_u64(worker_id as u64 ^ 0xdeadbeef),
        })
    }

    async fn bench(
        &mut self,
        state: &mut Self::WorkerState,
        info: &IterInfo,
    ) -> anyhow::Result<IterReport> {
        // Phase gate: sleep if worker shouldn't be active
        if !self.phase.is_worker_active(info.worker_id) {
            tokio::time::sleep(Duration::from_millis(50)).await;
            return Ok(IterReport {
                duration: Duration::from_millis(50),
                status: Status::success(0),
                bytes: 0,
                items: 0,
            });
        }

        // Think time: 1-10ms random delay
        let think_ms = state.rng.random_range(1..=10);
        tokio::time::sleep(Duration::from_millis(think_ms)).await;

        // Pick request type
        let req_type = pick_request_type(&mut state.rng);

        // Pick query from Zipf pool
        let query = self.query_pool.sample(&mut state.rng).to_string();

        // Execute request
        let report = match req_type {
            RequestType::HttpQuery => {
                let (duration, status, bytes) =
                    do_http_query(&state.http_client, &self.http_url, &query).await;
                self.type_metrics.record(req_type, duration);
                IterReport {
                    duration,
                    status,
                    bytes,
                    items: 1,
                }
            }
            RequestType::HttpBatch => {
                // Pick 3 queries for the batch
                let q1 = self.query_pool.sample(&mut state.rng).to_string();
                let q2 = self.query_pool.sample(&mut state.rng).to_string();
                let queries_ref: Vec<&str> = vec![&query, &q1, &q2];
                let (duration, status, bytes, items) =
                    do_http_batch(&state.http_client, &self.http_url, &queries_ref).await;
                self.type_metrics.record(req_type, duration);
                IterReport {
                    duration,
                    status,
                    bytes,
                    items,
                }
            }
            RequestType::HttpStats => {
                let (duration, status, bytes) =
                    do_http_stats(&state.http_client, &self.http_url).await;
                self.type_metrics.record(req_type, duration);
                IterReport {
                    duration,
                    status,
                    bytes,
                    items: 1,
                }
            }
            RequestType::HttpHealth => {
                let (duration, status, bytes) =
                    do_http_health(&state.http_client, &self.http_url).await;
                self.type_metrics.record(req_type, duration);
                IterReport {
                    duration,
                    status,
                    bytes,
                    items: 1,
                }
            }
            RequestType::GrpcQuery => {
                if let Some(ref mut client) = state.grpc_search {
                    let (duration, status, bytes) = do_grpc_query(client, &query).await;
                    self.type_metrics.record(req_type, duration);
                    IterReport {
                        duration,
                        status,
                        bytes,
                        items: 1,
                    }
                } else {
                    // Fallback to HTTP query if gRPC unavailable
                    let (duration, status, bytes) =
                        do_http_query(&state.http_client, &self.http_url, &query).await;
                    self.type_metrics.record(RequestType::HttpQuery, duration);
                    IterReport {
                        duration,
                        status,
                        bytes,
                        items: 1,
                    }
                }
            }
        };

        Ok(report)
    }
}

// ---------------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let http_url = std::env::var("PROXY_URL").unwrap_or_else(|_| "http://localhost:8081".into());
    let grpc_url = std::env::var("GRPC_URL").unwrap_or_else(|_| "http://localhost:8080".into());
    let duration: usize = std::env::var("DURATION")
        .unwrap_or_else(|_| "30".into())
        .parse()?;
    let users: usize = std::env::var("USERS")
        .unwrap_or_else(|_| "10".into())
        .parse()?;
    let output_dir = std::env::var("BENCH_OUTPUT_DIR").ok().map(PathBuf::from);

    if let Some(ref dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }

    println!("Conpack Proxy Load Test (mixed traffic)");
    println!("========================================");
    println!("HTTP:     {http_url}");
    println!("gRPC:     {grpc_url}");
    println!("Duration: {duration}s (sustain) + 20s (warmup/burst/cooldown)");
    println!("Workers:  {users} (base), {} (burst)", users * 2);
    println!("Queries:  {POOL_SIZE} (Zipf s=1.07)");
    if let Some(ref dir) = output_dir {
        println!("Output:   {}", dir.display());
    }
    println!();

    // Quick reachability check
    let client = reqwest::Client::new();
    match client.get(format!("{http_url}/health")).send().await {
        Ok(r) if r.status().is_success() => {}
        _ => {
            eprintln!("ERROR: Proxy HTTP not reachable at {http_url}");
            eprintln!("Start the proxy first or set PROXY_URL.");
            std::process::exit(1);
        }
    }

    // Warmup: pre-fill cache with a sample of queries
    println!("Warming up cache with 20 queries...");
    let seed_dir = std::env::var("SEED_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("tests/e2e/data"));
    let query_pool = Arc::new(QueryPool::from_seed_files(&seed_dir));
    let mut warmup_rng = SmallRng::seed_from_u64(12345);
    for _ in 0..20 {
        let q = query_pool.sample(&mut warmup_rng);
        let _ = client
            .post(format!("{http_url}/query"))
            .json(&serde_json::json!({"query": q}))
            .send()
            .await;
    }
    println!("Warmup complete.\n");

    // Set up phase controller and metrics
    let type_metrics = Arc::new(TypeMetrics::new());
    let phase = Arc::new(PhaseController::new(users as u32, duration as u64));
    let total_dur = phase.total_duration();

    // Allocate 2*USERS workers so burst phase can use them all
    let total_workers = users * 2;

    let rlt_out = output_dir
        .as_ref()
        .map(|d| d.join("mixed_traffic_rlt.json"));

    let mut cli_args = vec![
        "bench".to_string(),
        "-c".to_string(),
        total_workers.to_string(),
        "-d".to_string(),
        format!("{}s", total_dur.as_secs()),
        "-q".to_string(),
    ];
    if let Some(ref path) = rlt_out {
        cli_args.extend([
            "-o".to_string(),
            "json".to_string(),
            "-O".to_string(),
            path.to_string_lossy().to_string(),
        ]);
    }
    let cli = BenchCli::parse_from(&cli_args);

    let bench = MixedTrafficBench {
        http_url: http_url.clone(),
        grpc_url,
        query_pool,
        type_metrics: type_metrics.clone(),
        phase,
    };

    println!("=== Mixed Traffic Benchmark ===");
    println!("Phases: warmup(10s) → sustain({duration}s) → burst(5s) → cooldown(5s)");
    println!("Request mix: 60% query, 15% batch, 10% stats, 10% health, 5% gRPC");
    println!();

    let run_result = rlt::cli::run(cli, bench).await;

    match run_result {
        Ok(()) => {
            println!("\nBenchmark complete.");
        }
        Err(e) => {
            eprintln!("Benchmark failed: {e}");
        }
    }

    // Build per-type entries for summary.json
    let total_secs = total_dur.as_secs() as usize;
    let benchmark_entries = type_metrics.build_entries(total_secs, users);

    // Print per-type summary
    println!("\n=== Per-Type Results ===");
    for entry in &benchmark_entries {
        let iters = entry.raw_metrics["summary"]["iters"]["total"]
            .as_u64()
            .unwrap_or(0);
        let rate = entry.raw_metrics["summary"]["iters"]["rate"]
            .as_f64()
            .unwrap_or(0.0);
        let p99 = entry
            .raw_metrics
            .get("latency")
            .and_then(|l| l["percentiles"]["p99"].as_f64())
            .unwrap_or(0.0);
        println!(
            "  {:<12} {:>8} iters  {:>8.1} rps  p99={:.3}s",
            entry.name, iters, rate, p99,
        );
    }

    // Fetch final proxy stats
    println!("\n=== Final Cache Stats ===");
    let proxy_stats: Option<serde_json::Value> =
        match client.get(format!("{http_url}/stats")).send().await {
            Ok(resp) => {
                let stats: Option<serde_json::Value> = resp.json().await.ok();
                if let Some(ref s) = stats {
                    println!("{}", serde_json::to_string_pretty(s).unwrap_or_default());
                }
                stats
            }
            Err(_) => None,
        };

    // Write results
    if let Some(ref dir) = output_dir {
        let summary = serde_json::json!({
            "benchmarks": benchmark_entries,
            "proxy_stats": proxy_stats,
        });
        std::fs::write(
            dir.join("summary.json"),
            serde_json::to_string_pretty(&summary)?,
        )?;
        if let Some(ref stats) = proxy_stats {
            std::fs::write(
                dir.join("proxy_stats.json"),
                serde_json::to_string_pretty(stats)?,
            )?;
        }
        println!("\nResults written to {}", dir.display());
    }

    println!("\nLoad test complete!");
    Ok(())
}
