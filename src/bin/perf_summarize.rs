//! Tiny Criterion output summarizer for perf-tuning runs.
//!
//! Reads `target/criterion/*/new/estimates.json` (recursively, so parameterized
//! benches like `<group>/<id>/new/...` are picked up) and optionally
//! `change/estimates.json`. Compares new against the saved `main` baseline
//! (when present) using confidence-interval-aware verdicts.
//!
//! Usage:
//!   perf_summarize --results-dir tests/results/perf-tuning/<ts> [--threshold 15]
//!                   [--since <mtime-marker>] [--fail-on-regression]
//!                   [--metrics-file <path>] [--criterion-dir target/criterion]
//!                   [--flamegraph <path>] [--mode quick|default|full]

// CLI parsing here is index-based for clarity, and the bench tuple type is
// local-only. Allow the deny-by-default lints that would otherwise require
// wrapping this in a helper module just to please clippy.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::type_complexity,
    clippy::indexing_slicing
)]

use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::SystemTime;

// ---------------------------------------------------------------------------
// Criterion JSON shapes (subset)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct EstPoint {
    point_estimate: f64,
    #[allow(dead_code)]
    confidence_interval: Option<CIBounds>,
    #[allow(dead_code)]
    standard_error: Option<f64>,
}

#[derive(Deserialize)]
struct CIBounds {
    lower_bound: f64,
    upper_bound: f64,
    #[allow(dead_code)]
    confidence_level: f64,
}

#[derive(Deserialize)]
struct Est {
    mean: EstPoint,
}

#[derive(Deserialize)]
struct ChangeEst {
    mean: EstPoint,
}

// ---------------------------------------------------------------------------
// Output JSON
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct BenchEntry {
    name: String,
    group: String,
    mean_ns: f64,
    change_pct: Option<f64>,
    ci: Option<(f64, f64)>,
    has_main: bool,
    status: String, // "regression" | "improvement" | "inconclusive" | "ok" | "no_main"
    verdict_reason: String,
}

#[derive(Serialize)]
struct Summary {
    schema_version: u32,
    timestamp: String,
    mode: String,
    threshold_pct: f64,
    verdict: String,
    git_sha: String,
    rustc_version: String,
    benches: Vec<BenchEntry>,
    regressions: Vec<ChangeEntry>,
    improvements: Vec<ChangeEntry>,
    inconclusive: Vec<ChangeEntry>,
    metrics: Option<serde_json::Value>,
    artifacts: Artifacts,
    baseline_meta: Option<BaselineMeta>,
    baseline_status: String,
    baseline_age_hours: Option<f64>,
    baseline_commits_behind: Option<u32>,
    // v2 fields (additive; digest-safe)
    hitrate: Option<HitrateSummary>,
    diagnostics: Diagnostics,
    tokio: Option<TokioSnap>,
    tokio_metrics: Option<TokioMetrics>,
    tokio_dump: Option<TokioDump>,
    trend: Option<Vec<TrendPoint>>,
    chronic_regressions: Option<Vec<ChronicRegression>>,
    history: Option<Vec<HistoryEntry>>,
}

#[derive(Serialize)]
struct Artifacts {
    summary_md: String,
    report_criterion: String,
    analysis_md: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct BaselineMeta {
    schema_version: u32,
    saved_at: String,
    git_sha: String,
    git_dirty: bool,
    rustc: String,
    bench_count: u32,
}

#[derive(Serialize)]
struct EnvJson {
    git_sha: String,
    rustc_version: String,
    mode: String,
    threshold_pct: f64,
    timestamp: String,
    baseline_meta: Option<BaselineMeta>,
}

#[derive(Serialize, Clone)]
struct ChangeEntry {
    name: String,
    change_pct: f64,
    ci: Option<(f64, f64)>,
}

// Hitrate summary parsed from hitrate_bench summary.json (additive — only
// present when --hitrate-summary is provided). Best-effort parse: malformed
// files yield None so the rest of the report still works.
#[derive(Serialize, Deserialize, Clone)]
struct HitrateWorkload {
    name: String,
    best_exact_hr: Option<f64>,
    best_combined_hr: Option<f64>,
    best_tau: Option<f64>,
    false_hit_rate: Option<f64>,
    semantic_hr_uplift: Option<f64>,
}

#[derive(Serialize, Deserialize, Clone)]
struct HitrateSummary {
    verdict: String,
    agentic_best: Option<f64>,
    agentic_gate: Option<f64>,
    workloads: Vec<HitrateWorkload>,
}

#[derive(Serialize, Deserialize, Clone)]
struct TokioTask {
    name: String,
    total_poll_ms: f64,
    poll_count: u64,
    avg_poll_us: f64,
    busy_ratio: Option<f64>,
}

#[derive(Serialize, Deserialize, Clone)]
struct TokioSnap {
    sampled_secs: f64,
    top_tasks: Vec<TokioTask>,
    note: Option<String>,
}

// RuntimeMetrics JSON snapshot from `GET /debug/tokio`. The proxy returns
// whatever fields are available given the build's tokio_unstable flag:
// stable: num_alive_tasks, num_workers, worker_total_busy_duration_ns,
//         global_queue_depth
// unstable: worker_poll_count, worker_mean_poll_time_ns, budget_forced_yield_count
// Best-effort parse: any missing field is None. The `available` flag is set
// by the proxy when the endpoint is reachable; the loader sets it true if the
// JSON parses at all.
#[derive(Serialize, Deserialize, Clone)]
struct TokioMetrics {
    available: bool,
    num_alive_tasks: Option<u64>,
    num_workers: Option<usize>,
    global_queue_depth: Option<usize>,
    worker_total_busy_duration_ns: Option<u64>,
    worker_poll_count: Option<u64>,
    worker_mean_poll_time_ns: Option<u64>,
    budget_forced_yield_count: Option<u64>,
}

// Handle::dump() text output. Stored as raw text; size cap avoids huge
// summary.json. `available` distinguishes "endpoint not built" from
// "endpoint built but dump empty". `hint` set when endpoint returned 503
// with the RUSTFLAGS/feature message.
#[derive(Serialize, Clone)]
struct TokioDump {
    available: bool,
    path: String,
    text: String,
    hint: Option<String>,
}

#[derive(Serialize, Clone)]
struct Diagnostics {
    flamegraph: bool,
    heaptrack: bool,
    tokio_console: bool,
    tokio_metrics: bool,
    tokio_dump: bool,
    hitrate: bool,
}

#[derive(Serialize, Clone)]
struct HistoryEntry {
    timestamp: String,
    sha: String,
    verdict: String,
    top_mover: Option<String>,
    top_mover_pct: Option<f64>,
}

#[derive(Serialize, Clone)]
struct TrendPoint {
    name: String,
    prev_mean_ns: f64,
    new_mean_ns: f64,
    delta_pct: f64,
}

#[derive(Serialize, Clone)]
struct ChronicRegression {
    name: String,
    consecutive_runs: u32,
    last_pct: f64,
}

#[derive(Serialize)]
struct ReportCriterion {
    threshold_pct: f64,
    improvements: Vec<ChangeEntry>,
    regressions: Vec<ChangeEntry>,
    inconclusive: Vec<ChangeEntry>,
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Args {
    results_dir: Option<String>,
    threshold: f64,
    criterion_dir: Option<String>,
    mode: String,
    since: Option<String>,
    metrics_file: Option<String>,
    metrics_before_file: Option<String>,
    flamegraph: Option<String>,
    fail_on_regression: bool,
    hitrate_summary: Option<String>,
    history_dir: Option<String>,
    tokio_snap: Option<String>,
    tokio_metrics: Option<String>,
    tokio_dump: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        threshold: 15.0,
        mode: "quick".into(),
        ..Default::default()
    };
    let argv: Vec<String> = env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        let next = argv.get(i + 1);
        match arg.as_str() {
            "--results-dir" => {
                a.results_dir = next.cloned();
                i += 1;
            }
            "--threshold" => {
                a.threshold = next
                    .ok_or("--threshold requires value")?
                    .parse::<f64>()
                    .map_err(|e| format!("--threshold: {e}"))?;
                i += 1;
            }
            "--criterion-dir" => {
                a.criterion_dir = next.cloned();
                i += 1;
            }
            "--mode" => {
                a.mode = next.ok_or("--mode requires value")?.clone();
                i += 1;
            }
            "--since" => {
                a.since = next.cloned();
                i += 1;
            }
            "--metrics-file" => {
                a.metrics_file = next.cloned();
                i += 1;
            }
            "--metrics-before-file" => {
                a.metrics_before_file = next.cloned();
                i += 1;
            }
            "--flamegraph" => {
                a.flamegraph = next.cloned();
                i += 1;
            }
            "--fail-on-regression" => {
                a.fail_on_regression = true;
            }
            "--hitrate-summary" => {
                a.hitrate_summary = next.cloned();
                i += 1;
            }
            "--history-dir" => {
                a.history_dir = next.cloned();
                i += 1;
            }
            "--tokio-snap" => {
                a.tokio_snap = next.cloned();
                i += 1;
            }
            "--tokio-metrics" => {
                a.tokio_metrics = next.cloned();
                i += 1;
            }
            "--tokio-dump" => {
                a.tokio_dump = next.cloned();
                i += 1;
            }
            other => return Err(format!("Unknown arg: {other}")),
        }
        i += 1;
    }
    Ok(a)
}

// ---------------------------------------------------------------------------
// Bench name → group mapping
// ---------------------------------------------------------------------------

fn bench_group(name: &str) -> String {
    match name {
        n if n.starts_with("cache_") => "core_ops".into(),
        n if n.starts_with("query_") || n.starts_with("response_") => "core_ops".into(),
        n if n == "slugify" || n == "normalize_query" => "core_ops".into(),
        n if n.starts_with("fuse_rrf") || n.starts_with("scope_") => "cascade_scope".into(),
        _ => "other".into(),
    }
}

// Static map: bench name → (source file:fn, candidate rust-skills rules).
// Used in regression detail to give investigators a starting point.
// Keep entries short — one source line per bench + 1–2 rule hints.
fn hot_path_for(bench: &str) -> Option<(&'static str, &'static str)> {
    Some(match bench {
        "cache_insert" => (
            "src/proxy/cache.rs::CacheStore::insert_impl",
            "rust-skills: perf-entry-api, mem-collect-once, opt-cold-unlikely",
        ),
        "cache_lookup_hit" => (
            "src/proxy/cache.rs::CacheStore::get",
            "rust-skills: perf-iter-over-index, mem-clone-from",
        ),
        "cache_lookup_miss" => (
            "src/proxy/cache.rs::CacheStore::get (miss path)",
            "rust-skills: opt-inline-always-rare",
        ),
        "cache_eviction_pressure" => (
            "src/proxy/cache.rs::CacheStore::evict_expired",
            "rust-skills: mem-collect-once, opt-cold-unlikely",
        ),
        "cache_throughput" => (
            "src/proxy/cache.rs (insert + lookup combined)",
            "rust-skills: perf-entry-api, opt-codegen-units",
        ),
        "query_serialize_json" | "query_deserialize_json" => (
            "src/proxy/query.rs (serde derive)",
            "rust-skills: serde-flatten, opt-inline-always",
        ),
        "response_serialize_json" | "response_deserialize_json" => (
            "src/proxy/query.rs (serde derive)",
            "rust-skills: serde-flatten, opt-inline-always",
        ),
        "query_hash" => (
            "src/proxy/query.rs::compute_query_hash",
            "rust-skills: perf-collect-once",
        ),
        "slugify" => (
            "src/proxy/mod.rs::slugify",
            "rust-skills: mem-write-over-format, perf-iter-lazy",
        ),
        "normalize_query" => (
            "src/proxy/query.rs::normalize_query",
            "rust-skills: perf-iter-lazy, mem-collect-once",
        ),
        "fuse_rrf_3upstreams_k60" | "fuse_rrf_dedup_2upstreams" | "fuse_rrf_2upstreams_50each" => (
            "src/proxy/cascade.rs::fuse_rrf",
            "rust-skills: perf-collect-into, mem-collect-once, opt-codegen-units",
        ),
        "scope_best_sim_short_content"
        | "scope_best_sim_long_content"
        | "scope_best_sim_no_match"
        | "scope_filter_results_20items" => (
            "src/proxy/scope.rs::ScopeFilter::best_sim / filter_results",
            "rust-skills: perf-iter-lazy, mem-collect-once, opt-inline-always",
        ),
        _ => return None,
    })
}

// ---- History + trend loading (best-effort, additive) ----

fn load_hitrate_summary(path: &str) -> Option<HitrateSummary> {
    let text = fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    // hitrate_bench summary.json: top-level "workloads" array + "verdict" key.
    let verdict = v
        .get("verdict")
        .and_then(|x| x.as_str())
        .unwrap_or("UNKNOWN")
        .to_string();
    let mut workloads = Vec::new();
    if let Some(arr) = v.get("workloads").and_then(|x| x.as_array()) {
        for wl in arr {
            let name = wl
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("?")
                .to_string();
            // Workload rows have a "sweep" array; pick the best exact_hit_rate.
            let (best_exact, best_combined, best_tau, false_hit, uplift) =
                if let Some(sweep) = wl.get("sweep").and_then(|x| x.as_array()) {
                    let mut best_exact: Option<(f64, Option<f64>, Option<f64>, Option<f64>)> = None;
                    let mut best_combined: Option<(
                        f64,
                        Option<f64>,
                        Option<f64>,
                        Option<f64>,
                        Option<f64>,
                    )> = None;
                    for p in sweep {
                        let exact = p.get("exact_hit_rate").and_then(|x| x.as_f64());
                        let combined = p.get("combined_hit_rate").and_then(|x| x.as_f64());
                        let tau = p.get("tau").and_then(|x| x.as_f64());
                        let fh = p.get("false_hit_rate").and_then(|x| x.as_f64());
                        let upl = p.get("uplift").and_then(|x| x.as_f64());
                        if let Some(e) = exact {
                            if best_exact.is_none_or(|(be, _, _, _)| e > be) {
                                best_exact = Some((e, combined, tau, fh));
                            }
                        }
                        if let Some(c) = combined {
                            if best_combined.is_none_or(|(bc, _, _, _, _)| c > bc) {
                                best_combined = Some((c, exact, tau, fh, upl));
                            }
                        }
                    }
                    (
                        best_exact.map(|(e, _, _, _)| e),
                        best_combined.map(|(c, _, _, _, _)| c),
                        best_combined.and_then(|(_, _, t, _, _)| t),
                        best_exact
                            .and_then(|(_, _, _, f)| f)
                            .or_else(|| best_combined.and_then(|(_, _, _, f, _)| f)),
                        best_combined.and_then(|(_, _, _, _, u)| u),
                    )
                } else {
                    (None, None, None, None, None)
                };
            workloads.push(HitrateWorkload {
                name,
                best_exact_hr: best_exact,
                best_combined_hr: best_combined,
                best_tau,
                false_hit_rate: false_hit,
                semantic_hr_uplift: uplift,
            });
        }
    }
    // Agentic-specific gate + best.
    let agentic = workloads.iter().find(|w| w.name == "agentic");
    Some(HitrateSummary {
        verdict,
        agentic_best: agentic.and_then(|w| w.best_exact_hr),
        agentic_gate: Some(0.40),
        workloads,
    })
}

fn load_tokio_snap(path: &str) -> Option<TokioSnap> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

// Load RuntimeMetrics JSON from the /debug/tokio endpoint. The proxy writes
// either a real object (when endpoint is reachable) or a short text
// placeholder like "(no /debug/tokio endpoint)" on curl failure. We treat
// non-object content as unavailable.
fn load_tokio_metrics(path: &str) -> Option<TokioMetrics> {
    let text = fs::read_to_string(path).ok()?;
    if !text.trim_start().starts_with('{') {
        return None;
    }
    let val: serde_json::Value = serde_json::from_str(&text).ok()?;
    let obj = val.as_object()?;
    Some(TokioMetrics {
        available: true,
        num_alive_tasks: obj.get("num_alive_tasks").and_then(|v| v.as_u64()),
        num_workers: obj
            .get("num_workers")
            .and_then(|v| v.as_u64().map(|n| n as usize)),
        global_queue_depth: obj
            .get("global_queue_depth")
            .and_then(|v| v.as_u64().map(|n| n as usize)),
        worker_total_busy_duration_ns: obj
            .get("worker_total_busy_duration_ns")
            .and_then(|v| v.as_u64()),
        worker_poll_count: obj.get("worker_poll_count").and_then(|v| v.as_u64()),
        worker_mean_poll_time_ns: obj.get("worker_mean_poll_time_ns").and_then(|v| v.as_u64()),
        budget_forced_yield_count: obj
            .get("budget_forced_yield_count")
            .and_then(|v| v.as_u64()),
    })
}

// Load Handle::dump() text. The Makefile step writes a placeholder when the
// endpoint is missing (e.g. "(no /debug/tokio/dump endpoint — needs ...").
// Such placeholders are detected and reported with the `hint` so the user
// knows what to enable.
const DUMP_MAX_BYTES: usize = 64 * 1024;
fn load_tokio_dump(path: &str) -> TokioDump {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => {
            return TokioDump {
                available: false,
                path: path.to_string(),
                text: String::new(),
                hint: None,
            };
        }
    };
    // Proxy returns text/plain (or 503 with text). The 503 placeholder
    // contains "needs" — sniff for it to surface the build hint.
    let is_placeholder = !text.trim_start().starts_with('{')
        && text
            .lines()
            .any(|l| l.contains("RUSTFLAGS") || l.contains("tokio_unstable"));
    if is_placeholder {
        return TokioDump {
            available: false,
            path: path.to_string(),
            text: String::new(),
            hint: Some(text.trim().to_string()),
        };
    }
    let truncated = if text.len() > DUMP_MAX_BYTES {
        text[..DUMP_MAX_BYTES].to_string()
    } else {
        text
    };
    TokioDump {
        available: true,
        path: path.to_string(),
        text: truncated,
        hint: None,
    }
}

fn load_history(dir: &str, current_sha: &str) -> Vec<HistoryEntry> {
    let mut out = Vec::new();
    let path = Path::new(dir);
    if !path.is_dir() {
        return out;
    }
    let entries = match fs::read_dir(path) {
        Ok(e) => e,
        Err(_) => return out,
    };
    // Subdirs: <ts>-<sha8>/summary.json. Sort by mtime (newest first) and
    // take last 5 (excluding the current sha).
    let mut dirs: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let m = e.metadata().and_then(|m| m.modified()).ok();
        if let Some(m) = m {
            dirs.push((m, p));
        }
    }
    dirs.sort_by_key(|x| std::cmp::Reverse(x.0));
    for (_m, d) in dirs.into_iter().take(10) {
        let summary = d.join("summary.json");
        if !summary.is_file() {
            continue;
        }
        if let Ok(text) = fs::read_to_string(&summary) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                let sha = v
                    .get("git_sha")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                if sha.starts_with(current_sha) {
                    continue;
                }
                let ts = v
                    .get("timestamp")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let verdict = v
                    .get("verdict")
                    .and_then(|x| x.as_str())
                    .unwrap_or("?")
                    .to_string();
                // Top mover: max |change_pct| among benches with a change.
                let mut top: Option<(String, f64)> = None;
                if let Some(arr) = v.get("benches").and_then(|x| x.as_array()) {
                    for b in arr {
                        if let Some(p) = b.get("change_pct").and_then(|x| x.as_f64()) {
                            let name = b
                                .get("name")
                                .and_then(|x| x.as_str())
                                .unwrap_or("?")
                                .to_string();
                            if top.as_ref().is_none_or(|(_, tp)| p.abs() > tp.abs()) {
                                top = Some((name, p));
                            }
                        }
                    }
                }
                let (tm, tp) = top.map(|(n, p)| (Some(n), Some(p))).unwrap_or((None, None));
                out.push(HistoryEntry {
                    timestamp: ts,
                    sha: sha.chars().take(8).collect(),
                    verdict,
                    top_mover: tm,
                    top_mover_pct: tp,
                });
                if out.len() >= 5 {
                    break;
                }
            }
        }
    }
    out
}

fn compute_trend(
    new_benches: &[BenchEntry],
    history: &[HistoryEntry],
    history_dir: &str,
    current_sha: &str,
) -> Vec<TrendPoint> {
    // Trend: compare current mean_ns vs the most-recent prior published run
    // that has matching bench names. Only benches present in both compare.
    let Some(prior) = find_prior_bench_estimates(history_dir, current_sha) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for b in new_benches {
        if let Some(prev) = prior.get(&b.name) {
            if *prev > 0.0 {
                let delta = (b.mean_ns - *prev) / *prev * 100.0;
                out.push(TrendPoint {
                    name: b.name.clone(),
                    prev_mean_ns: *prev,
                    new_mean_ns: b.mean_ns,
                    delta_pct: delta,
                });
            }
        }
    }
    let _ = history; // history is loaded by caller separately
    out
}

fn find_prior_bench_estimates(
    history_dir: &str,
    current_sha: &str,
) -> Option<std::collections::HashMap<String, f64>> {
    let path = std::path::Path::new(history_dir);
    if !path.is_dir() {
        return None;
    }
    let entries = fs::read_dir(path).ok()?;
    let mut dirs: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
            dirs.push((m, p));
        }
    }
    dirs.sort_by_key(|x| std::cmp::Reverse(x.0));
    for (_m, d) in dirs {
        let summary = d.join("summary.json");
        if !summary.is_file() {
            continue;
        }
        if let Ok(text) = fs::read_to_string(&summary) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                let sha = v
                    .get("git_sha")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                if sha.starts_with(current_sha) {
                    continue;
                }
                let mut map = std::collections::HashMap::new();
                if let Some(arr) = v.get("benches").and_then(|x| x.as_array()) {
                    for b in arr {
                        if let (Some(name), Some(mean)) = (
                            b.get("name").and_then(|x| x.as_str()),
                            b.get("mean_ns").and_then(|x| x.as_f64()),
                        ) {
                            map.insert(name.to_string(), mean);
                        }
                    }
                }
                if !map.is_empty() {
                    return Some(map);
                }
            }
        }
    }
    None
}

fn compute_chronic_regressions(
    new_benches: &[BenchEntry],
    history_dir: &str,
    current_sha: &str,
) -> Vec<ChronicRegression> {
    // Walk the last 2 prior runs; flag benches that are regressed in
    // *both* prior runs (suggesting a chronic pattern, not one-off drift).
    let path = std::path::Path::new(history_dir);
    if !path.is_dir() {
        return Vec::new();
    }
    let entries = match fs::read_dir(path) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut dirs: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
            dirs.push((m, p));
        }
    }
    dirs.sort_by_key(|x| std::cmp::Reverse(x.0));
    let mut prior_runs: Vec<std::collections::HashMap<String, f64>> = Vec::new();
    for (_m, d) in dirs {
        if prior_runs.len() >= 2 {
            break;
        }
        let summary = d.join("summary.json");
        if !summary.is_file() {
            continue;
        }
        if let Ok(text) = fs::read_to_string(&summary) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                let sha = v
                    .get("git_sha")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                if sha.starts_with(current_sha) {
                    continue;
                }
                let mut map = std::collections::HashMap::new();
                if let Some(arr) = v.get("benches").and_then(|x| x.as_array()) {
                    for b in arr {
                        if let (Some(name), Some(pct)) = (
                            b.get("name").and_then(|x| x.as_str()),
                            b.get("change_pct").and_then(|x| x.as_f64()),
                        ) {
                            map.insert(name.to_string(), pct);
                        }
                    }
                }
                if !map.is_empty() {
                    prior_runs.push(map);
                }
            }
        }
    }
    if prior_runs.is_empty() {
        return Vec::new();
    }
    let mut chronic = Vec::new();
    for b in new_benches {
        if b.status != "regression" {
            continue;
        }
        let in_all = prior_runs
            .iter()
            .all(|m| m.get(&b.name).is_some_and(|p| *p > 0.0));
        if in_all {
            chronic.push(ChronicRegression {
                name: b.name.clone(),
                consecutive_runs: prior_runs.len() as u32 + 1,
                last_pct: b.change_pct.unwrap_or(0.0),
            });
        }
    }
    chronic
}

fn fmt_time(ns: f64) -> String {
    if ns < 1_000.0 {
        format!("{ns:.0}ns")
    } else if ns < 1_000_000.0 {
        format!("{:.1}µs", ns / 1_000.0)
    } else if ns < 1_000_000_000.0 {
        format!("{:.1}ms", ns / 1_000_000.0)
    } else {
        format!("{:.2}s", ns / 1_000_000_000.0)
    }
}

// ---------------------------------------------------------------------------
// Walk criterion dir
// ---------------------------------------------------------------------------

/// Returns (display_name, new_path, change_path_or_None, main_path_or_None).
/// display_name uses '/' separator for parameterized benches (matches criterion UI).
fn collect_benches(
    crit_path: &Path,
    since: Option<SystemTime>,
) -> Result<Vec<(String, PathBuf, Option<PathBuf>, Option<PathBuf>)>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    walk_criterion(crit_path, crit_path, since, &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn walk_criterion(
    root: &Path,
    dir: &Path,
    since: Option<SystemTime>,
    out: &mut Vec<(String, PathBuf, Option<PathBuf>, Option<PathBuf>)>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !dir.is_dir() {
        return Ok(());
    }
    // Leaf check: does this dir itself contain new/estimates.json?
    let new_path = dir.join("new/estimates.json");
    if new_path.is_file() {
        if let Some(min) = since {
            if let Ok(meta) = new_path.metadata() {
                if let Ok(mtime) = meta.modified() {
                    if mtime < min {
                        return Ok(()); // stale, skip
                    }
                }
            }
        }
        let name = dir
            .strip_prefix(root)
            .unwrap_or(dir)
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        // Skip internal criterion dirs (e.g. "report" at root has no new/ but
        // any nested dir whose name happens to be internal won't be a leaf with
        // new/ in it, so this guard is implicit).
        let change_path = dir.join("change/estimates.json");
        let main_path = dir.join("main/estimates.json");
        out.push((
            name,
            new_path,
            change_path.is_file().then_some(change_path),
            main_path.is_file().then_some(main_path),
        ));
        return Ok(());
    }
    // Otherwise recurse one level.
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            walk_criterion(root, &entry.path(), since, out)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Comparison + verdict
// ---------------------------------------------------------------------------

struct BenchResult {
    name: String,
    group: String,
    mean_ns: f64,
    change_pct: Option<f64>,
    ci: Option<(f64, f64)>,
    has_main: bool,
    status: String,
    verdict_reason: String,
}

fn evaluate(
    name: String,
    group: String,
    mean_ns: f64,
    new_se: Option<f64>,
    change_path: Option<PathBuf>,
    main_path: Option<PathBuf>,
    threshold: f64,
) -> Result<BenchResult, Box<dyn std::error::Error>> {
    // Strategy:
    //   1. If main/ exists, compute branch-vs-main delta + 95% CI from raw
    //      point estimates + standard errors. (se_Δ = sqrt(se_new² + se_main²)
    //      for independent normal samples; 1.96 ≈ z for 95%.)
    //   2. Else if change/ exists, trust criterion's precomputed delta + CI.
    //      (Caveat: change/ is new-vs-previous-run unless --baseline main was
    //      passed. With no main saved, this is the best we have.)
    //   3. Else: no baseline → status = no_main.
    let (delta_pct, ci, has_main) = if let Some(ref p) = main_path {
        let raw = fs::read_to_string(p)?;
        let m: Est = serde_json::from_str(&raw)?;
        let main_mean = m.mean.point_estimate;
        let main_se = m.mean.standard_error.unwrap_or(0.0);
        if main_mean > 0.0 && mean_ns > 0.0 {
            let delta = mean_ns - main_mean;
            let se_delta = (new_se.unwrap_or(0.0).powi(2) + main_se.powi(2)).sqrt();
            let ci_delta = 1.96 * se_delta;
            let pct = (delta / main_mean) * 100.0;
            let lo = ((delta - ci_delta) / main_mean) * 100.0;
            let hi = ((delta + ci_delta) / main_mean) * 100.0;
            (Some(pct), Some((lo, hi)), true)
        } else {
            (None, None, true)
        }
    } else if let Some(p) = change_path {
        let raw = fs::read_to_string(&p)?;
        let ch: ChangeEst = serde_json::from_str(&raw)?;
        let pct = ch.mean.point_estimate * 100.0;
        let ci = ch
            .mean
            .confidence_interval
            .map(|b| (b.lower_bound * 100.0, b.upper_bound * 100.0));
        (Some(pct), ci, false)
    } else {
        (None, None, false)
    };

    let (status, reason) = match (delta_pct, ci) {
        (None, _) => ("no_main".into(), "no main baseline + no change file".into()),
        (Some(_p), Some((lo, _hi))) if lo >= threshold => (
            "regression".into(),
            format!("CI lower bound {lo:+.1}% ≥ +{threshold}%"),
        ),
        (Some(_), Some((_lo, hi))) if hi <= -threshold => (
            "improvement".into(),
            format!("CI upper bound {hi:+.1}% ≤ -{threshold}%"),
        ),
        (Some(p), Some((lo, hi))) => (
            "inconclusive".into(),
            format!("CI [{lo:+.1}%, {hi:+.1}%] crosses ±{threshold}% (point {p:+.1}%)"),
        ),
        (Some(p), None) if p >= threshold => (
            "regression".into(),
            format!("point estimate +{p:.1}% ≥ +{threshold}% (no CI available)"),
        ),
        (Some(p), None) if p <= -threshold => (
            "improvement".into(),
            format!("point estimate {p:+.1}% ≤ -{threshold}% (no CI available)"),
        ),
        (Some(p), None) => (
            "ok".into(),
            format!("point estimate {p:+.1}% within threshold (no CI)"),
        ),
    };

    Ok(BenchResult {
        name,
        group,
        mean_ns,
        change_pct: delta_pct,
        ci,
        has_main,
        status,
        verdict_reason: reason,
    })
}

// ---------------------------------------------------------------------------
// Output writers
// ---------------------------------------------------------------------------

fn write_outputs(dir: &str, s: &Summary) -> Result<(), Box<dyn std::error::Error>> {
    fs::write(
        format!("{dir}/summary.json"),
        serde_json::to_string_pretty(s)?,
    )?;
    let report = ReportCriterion {
        threshold_pct: s.threshold_pct,
        improvements: s.improvements.clone(),
        regressions: s.regressions.clone(),
        inconclusive: s.inconclusive.clone(),
    };
    fs::write(
        format!("{dir}/report_criterion.json"),
        serde_json::to_string_pretty(&report)?,
    )?;
    fs::write(format!("{dir}/SUMMARY.md"), render_summary_md(s))?;
    fs::write(format!("{dir}/ANALYSIS.md"), render_analysis(s))?;
    fs::write(format!("{dir}/MANIFEST.md"), render_manifest(dir, s))?;
    eprintln!(
        "[perf_summarize] {} benches, {} regressions, {} improvements, {} inconclusive → {dir}/",
        s.benches.len(),
        s.regressions.len(),
        s.improvements.len(),
        s.inconclusive.len(),
    );
    Ok(())
}

fn render_summary_md(s: &Summary) -> String {
    let mut o = format!("# Perf Run {} — {}\n\n", s.timestamp, s.verdict);
    o.push_str(&format!(
        "| Field | Value |\n|-------|-------|\n\
         | Mode | {} |\n| Git | `{}` |\n| Rustc | {} |\n\
         | Threshold | ≥{}% (CI-based) |\n| Benches | {} total, {} regression(s), {} improvement(s), {} inconclusive, {} no-main |\n\n",
        s.mode,
        s.git_sha.chars().take(8).collect::<String>(),
        s.rustc_version,
        s.threshold_pct as u32,
        s.benches.len(),
        s.regressions.len(),
        s.improvements.len(),
        s.inconclusive.len(),
        s.benches.iter().filter(|b| b.status == "no_main").count(),
    ));

    // Baseline status section.
    if s.baseline_status == "no_meta" {
        o.push_str(
            "⚠️ **No baseline metadata.** Run `make bench-save` to stamp baseline context.\n\n",
        );
    } else {
        let status_icon = match s.baseline_status.as_str() {
            "ok" => "✅",
            _ => "⚠️",
        };
        o.push_str(&format!(
            "## Baseline\n\n| Field | Value |\n|-------|-------|\n\
             | Status | {status_icon} {} |\n",
            s.baseline_status,
        ));
        if let Some(ref meta) = s.baseline_meta {
            o.push_str(&format!(
                "| Saved at | {} |\n| SHA | `{}` |\n| Rustc | {} |\n| Bench count | {} |\n",
                meta.saved_at,
                meta.git_sha.chars().take(8).collect::<String>(),
                meta.rustc,
                meta.bench_count,
            ));
        }
        if let Some(h) = s.baseline_age_hours {
            o.push_str(&format!("| Age | {h:.1}h |\n"));
        }
        if let Some(n) = s.baseline_commits_behind {
            if n > 0 {
                o.push_str(&format!(
                    "| Commits behind | {n} — **drift likely if REGRESSIONS** |\n"
                ));
            } else {
                o.push_str("| Commits behind | 0 |\n");
            }
        }
        o.push('\n');
    }

    // Drift warning when REGRESSIONS + stale baseline.
    if s.verdict == "REGRESSIONS"
        && matches!(
            s.baseline_status.as_str(),
            "stale_sha" | "stale_age" | "stale_dirty"
        )
    {
        o.push_str("### ⚠️ Drift Warning\n\n");
        o.push_str("Baseline is stale (different SHA or >24h old). ");
        o.push_str("Regressions may reflect baseline drift, not code regression. ");
        o.push_str(
            "**Refresh baseline (`make bench-save`) before investigating code changes.**\n\n",
        );
    }

    if !s.regressions.is_empty() {
        o.push_str("## Regressions (CI lower bound ≥ threshold)\n\n");
        o.push_str(
            "| Bench | Group | Mean | Δ | CI | Why |\n|-------|-------|------|---|----|-----|\n",
        );
        for r in &s.regressions {
            if let Some(b) = s.benches.iter().find(|b| b.name == r.name) {
                let ci =
                    b.ci.map(|(lo, hi)| format!("[{lo:+.1}%, {hi:+.1}%]"))
                        .unwrap_or_else(|| "—".into());
                o.push_str(&format!(
                    "| {} | {} | {} | **+{:.1}%** | {} | {} |\n",
                    b.name.replace('_', " "),
                    b.group,
                    fmt_time(b.mean_ns),
                    r.change_pct,
                    ci,
                    b.verdict_reason,
                ));
            }
        }
        o.push('\n');
    }

    if !s.inconclusive.is_empty() {
        o.push_str("## Inconclusive (CI crosses threshold — needs more data)\n\n");
        o.push_str("| Bench | Δ | CI |\n|-------|---|----|\n");
        for ic in &s.inconclusive {
            if let Some(b) = s.benches.iter().find(|b| b.name == ic.name) {
                let ci =
                    b.ci.map(|(lo, hi)| format!("[{lo:+.1}%, {hi:+.1}%]"))
                        .unwrap_or_else(|| "—".into());
                o.push_str(&format!(
                    "| {} | {:+.1}% | {} |\n",
                    b.name.replace('_', " "),
                    ic.change_pct,
                    ci,
                ));
            }
        }
        o.push('\n');
    }

    if !s.improvements.is_empty() {
        o.push_str("## Top Improvements\n\n");
        o.push_str("| Bench | Group | Mean | Δ |\n|-------|-------|------|---|\n");
        for imp in &s.improvements {
            if let Some(b) = s.benches.iter().find(|b| b.name == imp.name) {
                o.push_str(&format!(
                    "| {} | {} | {} | {:+.1}% |\n",
                    b.name.replace('_', " "),
                    b.group,
                    fmt_time(b.mean_ns),
                    imp.change_pct,
                ));
            }
        }
        o.push('\n');
    }

    if s.regressions.is_empty() && s.improvements.is_empty() && s.inconclusive.is_empty() {
        o.push_str("## All Benches\n\n");
        o.push_str("| Bench | Group | Mean | Δ vs main | Status |\n|-------|-------|------|-----------|--------|\n");
        for b in &s.benches {
            let delta = b
                .change_pct
                .map(|p| format!("{p:+.1}%"))
                .unwrap_or_else(|| "—".into());
            o.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                b.name.replace('_', " "),
                b.group,
                fmt_time(b.mean_ns),
                delta,
                b.status,
            ));
        }
        o.push('\n');
    }

    o.push_str("## Artifacts\n\n");
    o.push_str("- `summary.json` — machine-readable (agents parse this)\n");
    o.push_str("- `report_criterion.json` — test_infra-compatible (index.html)\n");
    o.push_str("- `SUMMARY.md` — this file (human)\n");
    o.push_str("- `ANALYSIS.md` — narrative: what happened + next steps\n");
    o.push_str("- `MANIFEST.md` — what each file is\n");
    o.push_str("- `PLAN.md` — optimization plan (written by agent after analyze)\n\n");
    o.push_str("- `env.json` — run context (git SHA, mode, baseline meta copy)\n");
    o.push_str("- `baseline_meta.json` — copy of `target/criterion/.baseline_meta.json`\n");

    match s.verdict.as_str() {
        "PASS" => o.push_str("**No regressions detected.** Ready to merge.\n"),
        "NO_BASELINE" => {
            o.push_str("**No main baseline found.** Run `make bench-save` once, then re-run.\n")
        }
        "REGRESSIONS" => o.push_str("**Regressions detected.** Review `PLAN.md` before merging.\n"),
        other => o.push_str(&format!("**Verdict: {other}.**\n")),
    }

    o
}

// ---------------------------------------------------------------------------
// Narrative analysis (always generated — the "what happened" explanation)
// ---------------------------------------------------------------------------

fn render_analysis(s: &Summary) -> String {
    let mut o = String::new();
    let sha8: String = s.git_sha.chars().take(8).collect();

    // 1. What ran
    o.push_str(&format!(
        "# Performance Analysis — {}\n\n\
         **Run:** {} mode | Git `{}` | Threshold ≥{}%\n\n",
        s.verdict, s.mode, sha8, s.threshold_pct as u32,
    ));

    // 2. Verdict in plain English
    match s.verdict.as_str() {
        "PASS" => {
            o.push_str(
                "**No CI-bound regressions detected.** All measured changes \
                 are within the noise band (CI crosses zero) or below the \
                 threshold. This code change is safe from a performance \
                 perspective.\n\n",
            );
        }
        "REGRESSIONS" => {
            let n = s.regressions.len();
            o.push_str(&format!(
                "**{n} regression{} detected** with 95% CI lower bound ≥ +{}%. ",
                if n == 1 { "" } else { "s" },
                s.threshold_pct as u32,
            ));
            if matches!(
                s.baseline_status.as_str(),
                "stale_sha" | "stale_age" | "stale_dirty"
            ) {
                o.push_str(
                    "⚠️ Baseline is stale — **drift is the first hypothesis.** \
                     Refresh baseline (`make bench-save`) and re-run before \
                     investigating code.\n\n",
                );
            } else {
                o.push_str(
                    "Baseline is fresh — regressions likely reflect real \
                     code changes.\n\n",
                );
            }
        }
        "NO_BASELINE" => {
            o.push_str(
                "**No baseline found.** Run `make bench-save` to create one, \
                 then re-run perf-tuning to get actionable results.\n\n",
            );
        }
        other => {
            o.push_str(&format!("**Verdict: {other}.**\n\n"));
        }
    }

    // 3. Baseline health
    if s.baseline_status != "no_meta" {
        o.push_str("## Baseline Health\n\n");
        let icon = match s.baseline_status.as_str() {
            "ok" => "✅",
            _ => "⚠️",
        };
        o.push_str(&format!("- Status: {icon} `{}`\n", s.baseline_status));
        if let Some(ref meta) = s.baseline_meta {
            o.push_str(&format!(
                "- Saved: {} (sha `{}`)\n",
                meta.saved_at,
                meta.git_sha.chars().take(8).collect::<String>(),
            ));
            o.push_str(&format!("- Benchmarks: {}\n", meta.bench_count));
        }
        if let Some(h) = s.baseline_age_hours {
            o.push_str(&format!("- Age: {h:.1}h\n"));
        }
        if let Some(n) = s.baseline_commits_behind {
            if n > 0 {
                o.push_str(&format!("- Commits behind: {n} ⚠️ **drift likely**\n"));
            }
        }
        o.push('\n');
    }

    // 4. Run-over-run trend (vs most recent prior published run)
    if let Some(ref trend) = s.trend {
        if !trend.is_empty() {
            o.push_str("## Trend (vs most recent prior run)\n\n");
            o.push_str("| Bench | Prior mean | Current mean | Δ |\n");
            o.push_str("|-------|-----------:|-------------:|--:|\n");
            for t in trend {
                o.push_str(&format!(
                    "| {} | {} | {} | {:+.1}% |\n",
                    t.name.replace('_', " "),
                    fmt_time(t.prev_mean_ns),
                    fmt_time(t.new_mean_ns),
                    t.delta_pct,
                ));
            }
            o.push('\n');
        }
    }
    if let Some(ref chronic) = s.chronic_regressions {
        if !chronic.is_empty() {
            o.push_str("### ⚠️ Chronic regressions (flagged in prior runs too)\n\n");
            for c in chronic {
                o.push_str(&format!(
                    "- **{}** — regressed in {} consecutive runs, last +{:.1}%\n",
                    c.name.replace('_', " "),
                    c.consecutive_runs,
                    c.last_pct,
                ));
            }
            o.push('\n');
        }
    }

    // 5. Per-group deep dive (ALL benches per group, sorted)
    for group in &["core_ops", "cascade_scope"] {
        let group_benches: Vec<&BenchEntry> =
            s.benches.iter().filter(|b| b.group == *group).collect();
        if group_benches.is_empty() {
            continue;
        }
        o.push_str(&format!("## {group} benches (full)\n\n"));
        o.push_str("| Bench | Mean | Δ | CI | Status |\n");
        o.push_str("|-------|-----:|--:|----|--------|\n");
        for b in &group_benches {
            let delta = b
                .change_pct
                .map(|p| format!("{p:+.1}%"))
                .unwrap_or_else(|| "—".into());
            let ci =
                b.ci.map(|(lo, hi)| format!("[{lo:+.1}%, {hi:+.1}%]"))
                    .unwrap_or_else(|| "—".into());
            o.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                b.name.replace('_', " "),
                fmt_time(b.mean_ns),
                delta,
                ci,
                b.status,
            ));
        }
        o.push('\n');
    }

    // 6. Regressions detail with hot-path + repro + rule hints
    if !s.regressions.is_empty() {
        o.push_str("## Regressions (require investigation)\n\n");
        for r in &s.regressions {
            let ci_str = s
                .benches
                .iter()
                .find(|b| b.name == r.name)
                .and_then(|b| b.ci)
                .map(|(lo, hi)| format!("[{lo:+.1}%, {hi:+.1}%]"))
                .unwrap_or_else(|| "—".into());
            let reason = s
                .benches
                .iter()
                .find(|b| b.name == r.name)
                .map(|b| b.verdict_reason.as_str())
                .unwrap_or("");
            o.push_str(&format!(
                "### {} — +{:.1}% CI {}\n\n{}\n\n",
                r.name.replace('_', " "),
                r.change_pct,
                ci_str,
                reason,
            ));
            if let Some((path, rules)) = hot_path_for(&r.name) {
                o.push_str(&format!("- **Hot path:** `{path}`\n"));
                o.push_str(&format!("- **Rule hints:** {rules}\n"));
            }
            o.push_str(&format!(
                "- **Repro:** `cargo bench --bench {} -- --bench \"{}\" 2>&1 | tail -30`\n",
                if r.name.starts_with("fuse_") || r.name.starts_with("scope_") {
                    "cascade_scope"
                } else {
                    "core_ops"
                },
                r.name,
            ));
        }
        o.push('\n');
    }

    // 7. Cache effectiveness (hitrate family)
    if let Some(ref h) = s.hitrate {
        o.push_str("## Cache effectiveness (hit-rate benchmark)\n\n");
        let icon = match h.verdict.as_str() {
            "PASS" => "✅",
            "FAIL_CORE" => "🚨",
            "FAIL_TRUST" => "⚠️",
            _ => "•",
        };
        o.push_str(&format!("- Verdict: {icon} `{}`\n", h.verdict,));
        if let (Some(best), Some(gate)) = (h.agentic_best, h.agentic_gate) {
            let mark = if best >= gate { "✅" } else { "🚨" };
            o.push_str(&format!(
                "- Agentic exact HR: {mark} {:.1}% (gate {:.0}%)\n",
                best * 100.0,
                gate * 100.0,
            ));
        }
        if !h.workloads.is_empty() {
            o.push_str("\n| Workload | Best exact | Best combined (τ) | False-hit |\n");
            o.push_str("|----------|-----------:|-------------------:|----------:|\n");
            for w in &h.workloads {
                let exact = w
                    .best_exact_hr
                    .map(|v| format!("{:.1}%", v * 100.0))
                    .unwrap_or_else(|| "—".into());
                let combined = match (w.best_combined_hr, w.best_tau) {
                    (Some(c), Some(t)) => format!("{:.1}% (τ={:.2})", c * 100.0, t),
                    (Some(c), None) => format!("{:.1}%", c * 100.0),
                    _ => "—".into(),
                };
                let fh = w
                    .false_hit_rate
                    .map(|v| format!("{:.2}%", v * 100.0))
                    .unwrap_or_else(|| "—".into());
                o.push_str(&format!(
                    "| {} | {} | {} | {} |\n",
                    w.name, exact, combined, fh,
                ));
            }
            o.push('\n');
        }
        if h.verdict == "FAIL_CORE" {
            o.push_str(
                "🚨 **FAIL-CORE**: agentic exact hit rate below 40% — cache is not \
                 paying for itself in the agent workload. Investigate cache key \
                 (slugify, normalize_query) and TTL.\n\n",
            );
        }
    }

    // 8. Metrics deltas (Prometheus counters that moved)
    if let Some(ref m) = s.metrics {
        if let Some(serde_json::Value::Object(deltas)) = m.get("deltas") {
            if !deltas.is_empty() {
                o.push_str("## Metrics (counter deltas across the run)\n\n");
                o.push_str("| Metric | Delta |\n|--------|------:|\n");
                let mut entries: Vec<_> = deltas.iter().collect();
                entries.sort_by(|a, b| a.0.cmp(b.0));
                for (k, v) in entries {
                    let d = v.get("delta").and_then(|x| x.as_f64()).unwrap_or(0.0);
                    o.push_str(&format!("| `{}` | {:+.0} |\n", k, d));
                }
                o.push('\n');
            }
        }
    }

    // 9. Diagnostics status (what profiling artifacts exist)
    o.push_str("## Diagnostics\n\n");
    o.push_str(&format!(
        "- Flamegraph: {}\n",
        if s.diagnostics.flamegraph {
            "✅ present"
        } else {
            "❌ not produced"
        },
    ));
    o.push_str(&format!(
        "- Heaptrack: {}\n",
        if s.diagnostics.heaptrack {
            "✅ present"
        } else {
            "❌ not produced"
        },
    ));
    o.push_str(&format!(
        "- Tokio console: {}\n",
        if s.diagnostics.tokio_console {
            "✅ snap loaded (per-task)"
        } else {
            "❌ not produced (set TOKIO_CONSOLE=1)"
        },
    ));
    o.push_str(&format!(
        "- Tokio RuntimeMetrics: {}\n",
        if s.diagnostics.tokio_metrics {
            "✅ snapshot loaded (aggregates)"
        } else {
            "❌ not loaded (always-on /debug/tokio endpoint)"
        },
    ));
    o.push_str(&format!(
        "- Tokio Handle::dump(): {}\n",
        if s.diagnostics.tokio_dump {
            "✅ dump loaded (task backtraces)"
        } else {
            "❌ not loaded (needs RUSTFLAGS=--cfg tokio_unstable + tokio-console build)"
        },
    ));
    o.push_str(&format!(
        "- Hitrate bench: {}\n\n",
        if s.diagnostics.hitrate {
            "✅ present"
        } else {
            "❌ not run (perf-tuning-full default: HITRATE=1)"
        },
    ));
    if s.diagnostics.flamegraph {
        o.push_str("- Open flamegraph: `xdg-open flamegraph.svg` OR upload to https://profiler.firefox.com/\n");
    }

    // 10. Tokio runtime (console_snap top tasks)
    if let Some(ref t) = s.tokio {
        o.push_str("## Tokio Runtime (top tasks)\n\n");
        o.push_str(&format!("Sampled {}s.\n\n", t.sampled_secs as u32));
        if let Some(ref note) = t.note {
            o.push_str(&format!("_{note}_\n\n"));
        }
        if !t.top_tasks.is_empty() {
            o.push_str("| Task | Total poll | Polls | Avg poll | Busy ratio |\n");
            o.push_str("|------|-----------:|------:|---------:|-----------:|\n");
            for task in t.top_tasks.iter().take(5) {
                let busy = task
                    .busy_ratio
                    .map(|b| format!("{:.0}%", b * 100.0))
                    .unwrap_or_else(|| "—".into());
                o.push_str(&format!(
                    "| {} | {:.1}ms | {} | {:.1}µs | {} |\n",
                    task.name, task.total_poll_ms, task.poll_count, task.avg_poll_us, busy,
                ));
            }
            o.push('\n');
            // Interpretation: if any single task is >50% of total poll time,
            // it's dominating. Recommend investigating.
            let total: f64 = t.top_tasks.iter().map(|x| x.total_poll_ms).sum();
            if total > 0.0 {
                if let Some(top) = t.top_tasks.first() {
                    let share = top.total_poll_ms / total;
                    if share > 0.5 {
                        o.push_str(&format!(
                            "⚠️ `{}` is consuming {:.0}% of poll time — investigate\n\n",
                            top.name,
                            share * 100.0,
                        ));
                    }
                }
            }
        }
    }

    // 10b. Tokio RuntimeMetrics (B — always-on aggregates, no console stack)
    if let Some(ref m) = s.tokio_metrics {
        o.push_str("## Tokio RuntimeMetrics (B — aggregates)\n\n");
        o.push_str("Snapshot from `GET /debug/tokio` (Handle::current().metrics()). ");
        o.push_str("Always-on, no console stack required. ");
        o.push_str("Per-task detail requires section 10 above (TOKIO_CONSOLE=1).\n\n");
        let rows: Vec<(&str, String)> = vec![
            (
                "Alive tasks",
                m.num_alive_tasks
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "—".into()),
            ),
            (
                "Workers",
                m.num_workers
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "—".into()),
            ),
            (
                "Global queue depth",
                m.global_queue_depth
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "—".into()),
            ),
            (
                "Worker total busy (sum)",
                m.worker_total_busy_duration_ns
                    .map(format_ns)
                    .unwrap_or_else(|| "—".into()),
            ),
            (
                "Worker total polls",
                m.worker_poll_count
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "—".into()),
            ),
            (
                "Worker mean poll",
                m.worker_mean_poll_time_ns
                    .map(format_ns)
                    .unwrap_or_else(|| "—".into()),
            ),
            (
                "Budget forced yields",
                m.budget_forced_yield_count
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "—".into()),
            ),
        ];
        o.push_str("| Metric | Value |\n|---|---|\n");
        for (k, v) in &rows {
            o.push_str(&format!("| {k} | {v} |\n"));
        }
        // Highlight missing unstable fields
        let missing_unstable = [
            ("worker_poll_count", m.worker_poll_count.is_none()),
            (
                "worker_mean_poll_time_ns",
                m.worker_mean_poll_time_ns.is_none(),
            ),
            (
                "budget_forced_yield_count",
                m.budget_forced_yield_count.is_none(),
            ),
        ]
        .iter()
        .filter(|(_, m)| *m)
        .map(|(k, _)| *k)
        .collect::<Vec<_>>()
        .join(", ");
        if !missing_unstable.is_empty() {
            o.push_str(&format!(
                "\n_Note: {missing_unstable} require a `RUSTFLAGS=\"--cfg tokio_unstable\"` build. _\n",
            ));
        }
        o.push('\n');
    }

    // 10c. Tokio Handle::dump() (C — stuck-task backtraces)
    if let Some(ref d) = s.tokio_dump {
        o.push_str("## Tokio Handle::dump() (C — task backtraces)\n\n");
        if d.available {
            o.push_str(&format!("Dump file: `{}`\n\n", d.path));
            o.push_str("Use when tasks are stuck or contention is suspected. Open in any text editor or `less -R`.\n\n");
            // Show a brief preview (first ~30 lines) so the report is useful
            // even without the full file open.
            let preview: String = d.text.lines().take(30).collect::<Vec<_>>().join("\n");
            if !preview.is_empty() {
                o.push_str("<details><summary>Preview (first 30 lines)</summary>\n\n```\n");
                o.push_str(&preview);
                o.push_str("\n```\n\n</details>\n\n");
            }
        } else if let Some(ref hint) = d.hint {
            o.push_str("Endpoint returned 503 / not available. Build with:\n\n");
            o.push_str(&format!("> {hint}\n\n"));
        } else {
            o.push_str("Endpoint unreachable.\n\n");
        }
    }

    // 11. History (last 5 published runs)
    if let Some(ref h) = s.history {
        if !h.is_empty() {
            o.push_str("## History (last 5 published runs)\n\n");
            o.push_str("| Timestamp | SHA | Verdict | Top mover |\n|-----------|-----|---------|-----------|\n");
            for entry in h {
                let tm = match (entry.top_mover.as_ref(), entry.top_mover_pct) {
                    (Some(n), Some(p)) => format!("{} {:+.1}%", n.replace('_', " "), p),
                    (Some(n), None) => n.replace('_', " "),
                    _ => "—".into(),
                };
                o.push_str(&format!(
                    "| {} | `{}` | `{}` | {} |\n",
                    entry.timestamp, entry.sha, entry.verdict, tm,
                ));
            }
            o.push('\n');
        }
    }

    // 12. Rule-based recommendations
    if !s.regressions.is_empty() {
        o.push_str("## Recommendations\n\n");
        for r in &s.regressions {
            if let Some((path, rules)) = hot_path_for(&r.name) {
                o.push_str(&format!(
                    "- **{}** → inspect `{path}` ({rules})\n",
                    r.name.replace('_', " "),
                ));
            } else {
                o.push_str(&format!(
                    "- **{}** → no hot-path entry; run `make profile-flamegraph` and re-investigate\n",
                    r.name.replace('_', " "),
                ));
            }
        }
        o.push('\n');
    }

    // 13. Noise note
    if !s.inconclusive.is_empty() {
        o.push_str(&format!(
            "**Note:** {} bench{} showed raw change above threshold but the \
             95% CI crosses zero — this is statistical noise, not a real \
             regression. Do not act on inconclusive results; re-run with \
             more iterations if the signal persists.\n\n",
            s.inconclusive.len(),
            if s.inconclusive.len() == 1 { "" } else { "es" },
        ));
    }

    // 14. Next steps (branch on verdict)
    o.push_str("## Next Steps\n\n");
    match s.verdict.as_str() {
        "PASS" => {
            o.push_str("1. ✅ Safe to merge — no performance regressions\n");
            o.push_str(
                "2. Optional: run `make perf-tuning-default` or `full` for deeper profiling\n",
            );
            o.push_str("3. Publish: `make perf-publish` (commits evidence to `perf-history/`)\n");
        }
        "REGRESSIONS" => {
            if matches!(
                s.baseline_status.as_str(),
                "stale_sha" | "stale_age" | "stale_dirty"
            ) {
                o.push_str("1. **Refresh baseline:** `make bench-save` (verify clean tree)\n");
                o.push_str("2. **Re-run:** `make perf-tuning-quick`\n");
                o.push_str("3. If still REGRESSIONS → investigate code changes listed above\n");
            } else {
                o.push_str("1. **Investigate** the regression(s) listed above\n");
                o.push_str("2. Use hot-path map above as starting point\n");
                o.push_str(
                    "3. `make profile-flamegraph` or `make profile-dhat` for deeper diagnosis\n",
                );
            }
        }
        "NO_BASELINE" => {
            o.push_str(
                "1. **Create baseline:** `make bench-save` (clean tree, no uncommitted changes)\n",
            );
            o.push_str("2. **Re-run:** `make perf-tuning-quick`\n");
        }
        _ => {}
    }

    o
}

fn render_manifest(run_dir: &str, s: &Summary) -> String {
    let mut o = format!(
        "# MANIFEST — perf run {}\n\n\
         | File | Purpose | Who uses |\n|------|---------|----------|\n\
         | `summary.json` | Structured bench data + CI-aware verdicts | Agents (parse) |\n\
         | `report_criterion.json` | Improvements/regressions/inconclusive list | test_infra index |\n\
         | `SUMMARY.md` | Human scorecard | Humans (review) |\n\
         | `ANALYSIS.md` | Narrative analysis (what happened + next steps) | Humans + agents |\n\
         | `MANIFEST.md` | This file | Orientation |\n",
        s.timestamp,
    );
    let fg = Path::new(run_dir).join("flamegraph.svg");
    if fg.exists() {
        o.push_str("| `flamegraph.svg` | CPU flamegraph (inferno) | Humans (visual) |\n");
    }
    let fg_json = Path::new(run_dir).join("profile.json.gz");
    if fg_json.exists() {
        o.push_str(
            "| `profile.json.gz` | Firefox Profiler profile (samply) — open at https://profiler.firefox.com |\n",
        );
    }
    let metrics = Path::new(run_dir).join("metrics-prometheus.txt");
    if metrics.exists() {
        o.push_str(
            "| `metrics-prometheus.txt` | Prometheus scrape (after workload) | Humans/agents |\n",
        );
    }
    let metrics_before = Path::new(run_dir).join("metrics-before.txt");
    if metrics_before.exists() {
        o.push_str(
            "| `metrics-before.txt` | Prometheus scrape (before workload) — enables deltas |\n",
        );
    }
    if s.metrics.is_some() {
        o.push_str("| `summary.json:metrics` | Parsed headline counters | Agents (parse) |\n");
    }
    o.push_str("\n## Schema\n\n");
    o.push_str("`summary.json` schema v1 — see `src/bin/perf_summarize.rs`.\n");
    o.push_str("Verdict logic: regression iff `ci.lower_bound ≥ +threshold`;\n");
    o.push_str("improvement iff `ci.upper_bound ≤ −threshold`; else `inconclusive` or `ok`.\n");
    o
}

// ---------------------------------------------------------------------------
// Metrics parsing (best-effort)
// ---------------------------------------------------------------------------

/// Parse Prometheus text into a JSON object with sample counters.
/// Cheap heuristic: collect `# TYPE name counter` headers + numeric samples.
fn parse_prom_text(text: &str) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    let mut current_type: Option<(String, String)> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("# TYPE ") {
            let mut it = rest.split_whitespace();
            let name = it.next().unwrap_or("").to_string();
            let ty = it.next().unwrap_or("").to_string();
            current_type = Some((name, ty));
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        // Sample: metric_name{labels} value [timestamp]
        let (key, value_str) = match line.find(' ') {
            Some(idx) => (&line[..idx], &line[idx + 1..]),
            None => continue,
        };
        let value: f64 = match value_str.split_whitespace().next().unwrap_or("").parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        // Build entry
        let entry = out
            .entry(key.to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if let serde_json::Value::Object(map) = entry {
            let kind = current_type
                .as_ref()
                .filter(|(n, _)| n == key)
                .map(|(_, t)| t.clone());
            map.insert("value".into(), serde_json::json!(value));
            if let Some(k) = kind {
                map.insert("type".into(), serde_json::json!(k));
            }
        }
    }
    serde_json::Value::Object(out)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn format_ns(ns: u64) -> String {
    if ns >= 1_000_000_000 {
        format!("{:.1}s", ns as f64 / 1e9)
    } else if ns >= 1_000_000 {
        format!("{:.1}ms", ns as f64 / 1e6)
    } else if ns >= 1_000 {
        format!("{:.1}µs", ns as f64 / 1e3)
    } else {
        format!("{ns}ns")
    }
}

fn git_sha() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

fn rustc_version() -> Option<String> {
    let out = Command::new("rustc").arg("--version").output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

fn chrono_timestamp() -> String {
    let out = Command::new("date").arg("+%Y%m%d-%H%M%S").output().ok();
    match out {
        Some(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => "unknown".into(),
    }
}

/// Parse ISO 8601 timestamp (2024-01-15T10:30:00Z) and return hours elapsed.
fn parse_iso8601_hours(ts: &str) -> Option<f64> {
    // Format: "2024-01-15T10:30:00Z"
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let s = ts.trim_end_matches('Z');
    let year: i64 = s[0..4].parse().ok()?;
    let month: i64 = s[5..7].parse().ok()?;
    let day: i64 = s[8..10].parse().ok()?;
    let hour: i64 = s[11..13].parse().ok()?;
    let min: i64 = s[14..16].parse().ok()?;
    let sec: i64 = s.get(17..19).and_then(|v| v.parse().ok()).unwrap_or(0);
    // Days from civil date to Unix epoch (Howard Hinnant's algorithm, public domain).
    let y = year - (month <= 2) as i64;
    let m = month + (if month <= 2 { 12 } else { 0 });
    let days = 365 * y + y / 4 - y / 100 + y / 400 + (153 * (m - 3) + 2) / 5 + day - 719468;
    let stamp_secs = (days * 86400 + hour * 3600 + min * 60 + sec) as u64;
    Some((now_secs.saturating_sub(stamp_secs)) as f64 / 3600.0)
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn run() -> Result<i32, Box<dyn std::error::Error>> {
    let args = parse_args().map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    let results_path = args
        .results_dir
        .as_deref()
        .ok_or("--results-dir required")?;
    fs::create_dir_all(results_path)?;

    let crit_base = args.criterion_dir.as_deref().unwrap_or("target/criterion");
    let crit_path = Path::new(crit_base);

    // Read baseline metadata (written by `make bench-save`).
    let baseline_meta_path = crit_path.join(".baseline_meta.json");
    let baseline_meta: Option<BaselineMeta> = fs::read_to_string(&baseline_meta_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());

    // Compute baseline status + age + commits behind.
    let (baseline_status, baseline_age_hours, baseline_commits_behind): (
        String,
        Option<f64>,
        Option<u32>,
    ) = if let Some(ref meta) = baseline_meta {
        let current_sha = git_sha().unwrap_or_default();
        let meta_sha_short: String = meta.git_sha.chars().take(8).collect();
        let age_hours = parse_iso8601_hours(&meta.saved_at);
        let commits_behind = if current_sha.starts_with(&meta_sha_short) {
            Some(0u32)
        } else {
            // Count commits between meta SHA and HEAD.
            Command::new("git")
                .args(["rev-list", "--count", &format!("{}..HEAD", meta.git_sha)])
                .output()
                .ok()
                .and_then(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .trim()
                        .parse::<u32>()
                        .ok()
                })
        };
        let status = if meta.git_dirty {
            "stale_dirty"
        } else if commits_behind.unwrap_or(0) > 0 {
            "stale_sha"
        } else if age_hours.unwrap_or(f64::INFINITY) > 24.0 {
            "stale_age"
        } else {
            "ok"
        };
        (status.into(), age_hours, commits_behind)
    } else {
        ("no_meta".into(), None, None)
    };

    let since = args
        .since
        .as_deref()
        .and_then(|p| fs::metadata(p).ok())
        .and_then(|m| m.modified().ok());

    let mut benches: Vec<BenchEntry> = Vec::new();
    let mut regressions: Vec<ChangeEntry> = Vec::new();
    let mut improvements: Vec<ChangeEntry> = Vec::new();
    let mut inconclusive: Vec<ChangeEntry> = Vec::new();

    if crit_path.is_dir() {
        for (name, new_path, change_path, main_path) in collect_benches(crit_path, since)? {
            let raw = match fs::read_to_string(&new_path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[perf_summarize] skip {name}: {e}");
                    continue;
                }
            };
            let new_est: Est = match serde_json::from_str(&raw) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("[perf_summarize] skip {name}: {e}");
                    continue;
                }
            };
            let mean_ns = new_est.mean.point_estimate;
            let group = bench_group(&name);

            let r = evaluate(
                name.clone(),
                group.clone(),
                mean_ns,
                new_est.mean.standard_error,
                change_path,
                main_path,
                args.threshold,
            )?;
            let entry = BenchEntry {
                name: r.name.clone(),
                group: r.group,
                mean_ns: r.mean_ns,
                change_pct: r.change_pct,
                ci: r.ci,
                has_main: r.has_main,
                status: r.status.clone(),
                verdict_reason: r.verdict_reason,
            };
            match r.status.as_str() {
                "regression" => regressions.push(ChangeEntry {
                    name: r.name,
                    change_pct: r.change_pct.unwrap_or(0.0),
                    ci: r.ci,
                }),
                "improvement" => improvements.push(ChangeEntry {
                    name: r.name,
                    change_pct: r.change_pct.unwrap_or(0.0),
                    ci: r.ci,
                }),
                "inconclusive" => inconclusive.push(ChangeEntry {
                    name: r.name,
                    change_pct: r.change_pct.unwrap_or(0.0),
                    ci: r.ci,
                }),
                _ => {}
            }
            benches.push(entry);
        }
    } else {
        eprintln!("No criterion output at {crit_base}");
    }

    benches.sort_by(|a, b| {
        let rank = |s: &str| match s {
            "regression" => 0,
            "inconclusive" => 1,
            "ok" => 2,
            "no_main" => 3,
            "improvement" => 4,
            _ => 5,
        };
        rank(&a.status).cmp(&rank(&b.status)).then_with(|| {
            b.change_pct
                .unwrap_or(0.0)
                .partial_cmp(&a.change_pct.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });
    regressions.sort_by(|a, b| {
        b.change_pct
            .partial_cmp(&a.change_pct)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    improvements.sort_by(|a, b| {
        a.change_pct
            .partial_cmp(&b.change_pct)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let verdict = if !regressions.is_empty() {
        "REGRESSIONS"
    } else if benches.is_empty() || benches.iter().all(|b| !b.has_main && b.status == "no_main") {
        "NO_BASELINE"
    } else {
        "PASS"
    };

    let metrics = args.metrics_file.as_deref().and_then(|p| {
        let path = Path::new(p);
        if !path.is_file() {
            return None;
        }
        let text = fs::read_to_string(path).ok()?;
        let after = parse_prom_text(&text);
        // Compute deltas vs `--metrics-before-file` (counter-only). For each
        // metric present in both files with `type == "counter"`, emit a
        // `delta = after - before` entry. Gauges are left as after-only.
        let deltas = match args.metrics_before_file.as_deref() {
            Some(bp) => {
                let bp = Path::new(bp);
                if bp.is_file() {
                    let before_text = fs::read_to_string(bp).ok()?;
                    let before = parse_prom_text(&before_text);
                    let mut d = serde_json::Map::new();
                    if let (
                        serde_json::Value::Object(after_map),
                        serde_json::Value::Object(before_map),
                    ) = (&after, &before)
                    {
                        for (k, av) in after_map {
                            let is_counter =
                                av.get("type").and_then(|t| t.as_str()) == Some("counter");
                            if let Some(bv) = before_map.get(k) {
                                let a = av.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
                                let b = bv.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
                                if is_counter {
                                    d.insert(k.clone(), serde_json::json!({ "delta": a - b }));
                                }
                            }
                        }
                    }
                    Some(serde_json::Value::Object(d))
                } else {
                    None
                }
            }
            None => None,
        };
        let mut m = serde_json::Map::new();
        m.insert("after".into(), after);
        if let Some(d) = deltas {
            m.insert("deltas".into(), d);
        }
        Some(serde_json::Value::Object(m))
    });

    // ---- v2 wiring: load hitrate / history / tokio + compute trend/chronic ----
    let rd = Path::new(results_path);
    let hitrate = args
        .hitrate_summary
        .as_deref()
        .and_then(load_hitrate_summary);
    let tokio = args.tokio_snap.as_deref().and_then(load_tokio_snap);
    let tokio_metrics = args.tokio_metrics.as_deref().and_then(load_tokio_metrics);
    let tokio_dump = args.tokio_dump.as_deref().map(load_tokio_dump);
    let current_sha = git_sha().unwrap_or_else(|| "unknown".into());
    let history: Option<Vec<HistoryEntry>> = args.history_dir.as_ref().map(|d| {
        let mut h = load_history(d, &current_sha);
        h.truncate(5);
        h
    });
    let trend: Option<Vec<TrendPoint>> = if args.history_dir.is_some() {
        let mut t = compute_trend(
            &benches,
            &[],
            args.history_dir.as_deref().unwrap_or(""),
            &current_sha,
        );
        // Sort by |delta| descending so the report leads with the biggest moves.
        t.sort_by(|a, b| {
            b.delta_pct
                .abs()
                .partial_cmp(&a.delta_pct.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        t.truncate(5);
        Some(t)
    } else {
        None
    };
    let chronic_regressions: Option<Vec<ChronicRegression>> = if args.history_dir.is_some() {
        let c = compute_chronic_regressions(
            &benches,
            args.history_dir.as_deref().unwrap_or(""),
            &current_sha,
        );
        if c.is_empty() {
            None
        } else {
            Some(c)
        }
    } else {
        None
    };
    let diagnostics = Diagnostics {
        flamegraph: rd.join("flamegraph.svg").is_file() || rd.join("profile.json.gz").is_file(),
        heaptrack: rd.join("heaptrack.txt").is_file(),
        tokio_console: tokio.is_some(),
        tokio_metrics: tokio_metrics.is_some(),
        tokio_dump: tokio_dump.as_ref().is_some_and(|d| d.available),
        hitrate: hitrate.is_some(),
    };

    let summary = Summary {
        schema_version: 2,
        timestamp: chrono_timestamp(),
        mode: args.mode.clone(),
        threshold_pct: args.threshold,
        verdict: verdict.into(),
        git_sha: current_sha.clone(),
        rustc_version: rustc_version().unwrap_or_else(|| "unknown".into()),
        benches,
        regressions: regressions.clone(),
        improvements: improvements.clone(),
        inconclusive: inconclusive.clone(),
        metrics,
        artifacts: Artifacts {
            summary_md: "SUMMARY.md".into(),
            report_criterion: "report_criterion.json".into(),
            analysis_md: "ANALYSIS.md".into(),
        },
        baseline_meta: baseline_meta.clone(),
        baseline_status: baseline_status.clone(),
        baseline_age_hours,
        baseline_commits_behind,
        hitrate,
        diagnostics,
        tokio,
        tokio_metrics,
        tokio_dump,
        trend,
        chronic_regressions,
        history,
    };

    write_outputs(results_path, &summary)?;

    // Print ANALYSIS.md to stdout so the agent (and Make) see it directly.
    let analysis = render_analysis(&summary);
    println!("\n{analysis}");

    // Always write env.json (machine-readable run context).
    let env = EnvJson {
        git_sha: git_sha().unwrap_or_else(|| "unknown".into()),
        rustc_version: rustc_version().unwrap_or_else(|| "unknown".into()),
        mode: args.mode.clone(),
        threshold_pct: args.threshold,
        timestamp: chrono_timestamp(),
        baseline_meta: summary.baseline_meta.clone(),
    };
    let env_path = Path::new(results_path).join("env.json");
    if let Ok(env_json) = serde_json::to_string_pretty(&env) {
        if let Err(e) = fs::write(&env_path, env_json) {
            eprintln!("[perf_summarize] env.json write failed: {e}");
        }
    }
    // Copy baseline_meta.json into run dir for drift audit.
    if baseline_meta_path.is_file() {
        let dst = Path::new(results_path).join("baseline_meta.json");
        if let Err(e) = fs::copy(&baseline_meta_path, &dst) {
            eprintln!("[perf_summarize] baseline_meta.json copy failed: {e}");
        }
    }

    // Copy flamegraph (or profile.json.gz) into run dir if fresh + present.
    if let Some(src) = args.flamegraph.as_deref() {
        let src_path = Path::new(src);
        if src_path.is_file() {
            let fname = src_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("flamegraph.svg");
            let dst = Path::new(results_path).join(fname);
            if let Err(e) = fs::copy(src_path, &dst) {
                eprintln!("[perf_summarize] flamegraph copy failed: {e}");
            }
        }
    } else {
        // Back-compat: if a root-level flamegraph.svg exists, copy it,
        // but only if it was modified within the last hour.
        let fg = Path::new("flamegraph.svg");
        if fg.is_file() {
            if let Ok(meta) = fg.metadata() {
                if let Ok(mtime) = meta.modified() {
                    if mtime.elapsed().map(|d| d.as_secs() < 3600).unwrap_or(false) {
                        let dst = Path::new(results_path).join("flamegraph.svg");
                        let _ = fs::copy(fg, dst);
                    }
                }
            }
        }
    }

    if args.fail_on_regression && verdict == "REGRESSIONS" {
        eprintln!("[perf_summarize] failing: REGRESSIONS detected");
        return Ok(2);
    }
    Ok(0)
}

fn main() -> ExitCode {
    match run() {
        Ok(0) => ExitCode::SUCCESS,
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(e) => {
            eprintln!("[perf_summarize] error: {e}");
            ExitCode::FAILURE
        }
    }
}
