//! console_snap: headless dump of tokio-console updates for a configured
//! sample window. Mirrors the official `console-subscriber/examples/dump.rs`
//! but aggregates top tasks by total busy time, then writes JSON + a text
//! table. Used by `make perf-tuning-full TOKIO_CONSOLE=1` and by
//! `make profile-tokio-console` for non-interactive analysis.
//!
//! Usage: console_snap [URL] [DURATION_SECS] [--out DIR] [--top N]
//!   URL:            console endpoint (default http://127.0.0.1:6669)
//!   DURATION_SECS:  sample window (default 5)
//!   --out DIR:      write console-snap.json + console-snap.txt there
//!                   (default /tmp/conproxy-tokio-snap)
//!   --top N:        number of top tasks to keep (default 20)
//!
//! Connects via tonic, streams `watch_updates`, aggregates per-task stats
//! keyed by task id, resolves human task name from `RegisterMetadata`.
//! On stream end (proxy exit) or duration expiry, flushes report.

#![allow(clippy::arithmetic_side_effects)] // duration / polls, bounded u64

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use console_api::instrument::{instrument_client::InstrumentClient, InstrumentRequest};
use futures::stream::StreamExt;
use serde::Serialize;

// ---------------------------------------------------------------------------
// Aggregated state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize)]
struct TaskAgg {
    /// Last-known name (from metadata). Empty until metadata arrives.
    name: String,
    /// MetaId (u64) this task's name resolves through. Set on spawn.
    #[serde(skip)]
    meta_id: Option<u64>,
    kind: String, // "SPAWN" | "BLOCKING"
    polls: u64,
    /// Most-recent cumulative busy time (nanoseconds) from PollStats.
    /// Diffed between updates to compute per-window busy delta.
    busy_ns: u64,
    scheduled_ns: u64,
    /// Per-window poll count delta (sum of positive diffs between snapshots).
    window_polls: u64,
    /// Per-window busy delta (nanoseconds).
    window_busy_ns: u64,
    /// Slowest single poll observed (nanoseconds, derived from histogram).
    slowest_poll_ns: u64,
}

#[derive(Debug, Serialize)]
struct SnapReport {
    schema_version: u32,
    target: String,
    sample_seconds: f64,
    captured_updates: u64,
    dropped_events_total: u64,
    top_tasks: Vec<TaskRow>,
    totals: Totals,
}

#[derive(Debug, Clone, Serialize)]
struct TaskRow {
    name: String,
    kind: String,
    polls: u64,
    busy_ns: u64,
    mean_poll_ns: u64,
    scheduled_ns: u64,
    slowest_poll_ns: u64,
}

#[derive(Debug, Default, Serialize)]
struct Totals {
    unique_tasks: u64,
    total_polls: u64,
    total_busy_ns: u64,
}

// ---------------------------------------------------------------------------
// Args
// ---------------------------------------------------------------------------

struct Args {
    target: String,
    duration_secs: u64,
    out: PathBuf,
    top: usize,
}

fn parse_args() -> Result<Args, String> {
    let argv: Vec<String> = env::args().skip(1).collect();
    let mut a = Args {
        target: "http://127.0.0.1:6669".into(),
        duration_secs: 5,
        out: PathBuf::from("/tmp/conproxy-tokio-snap"),
        top: 20,
    };
    let mut i = 0;
    while let Some(arg) = argv.get(i) {
        let next = argv.get(i + 1);
        match arg.as_str() {
            "--out" => {
                a.out = next.ok_or("--out requires value")?.into();
                i += 1;
            }
            "--top" => {
                a.top = next
                    .ok_or("--top requires value")?
                    .parse::<usize>()
                    .map_err(|e| format!("--top: {e}"))?;
                i += 1;
            }
            "--duration" | "-d" => {
                a.duration_secs = next
                    .ok_or("--duration requires value")?
                    .parse::<u64>()
                    .map_err(|e| format!("--duration: {e}"))?;
                i += 1;
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other if other.starts_with("http://") || other.starts_with("https://") => {
                a.target = other.to_string();
            }
            other => {
                if let Ok(n) = other.parse::<u64>() {
                    a.duration_secs = n;
                } else {
                    return Err(format!("unknown arg: {other}"));
                }
            }
        }
        i += 1;
    }
    Ok(a)
}

fn print_help() {
    eprintln!("console_snap — headless tokio-console dump");
    eprintln!();
    eprintln!("Usage: console_snap [URL] [DURATION_SECS] [--out DIR] [--top N]");
    eprintln!();
    eprintln!("  URL            console endpoint (default http://127.0.0.1:6669)");
    eprintln!("  DURATION_SECS  sample window (default 5)");
    eprintln!("  --out DIR      write console-snap.json + console-snap.txt");
    eprintln!("  --top N        top tasks by busy time (default 20)");
    eprintln!();
    eprintln!("Connects to a proxy built with --features tokio-console under");
    eprintln!("RUSTFLAGS=--cfg tokio_unstable, then samples the update stream.");
}

// ---------------------------------------------------------------------------
// Stream consumer
// ---------------------------------------------------------------------------

fn dur_to_ns(d: Option<&prost_types::Duration>) -> u64 {
    d.map_or(0, |x| x.seconds as u64 * 1_000_000_000 + x.nanos as u64)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse_args().map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    fs::create_dir_all(&args.out)?;

    eprintln!("[console_snap] connecting to {}", args.target);
    let mut client = InstrumentClient::connect(args.target.clone()).await?;
    let request = tonic::Request::new(InstrumentRequest {});
    let mut stream = client.watch_updates(request).await?.into_inner();
    eprintln!(
        "[console_snap] streaming for {}s (out={}, top={})",
        args.duration_secs,
        args.out.display(),
        args.top
    );

    // Live aggregation: tasks keyed by u64 task id.
    let mut tasks: HashMap<u64, TaskAgg> = HashMap::new();
    // Metadata: MetaId (u64) -> human task name.
    let mut names: HashMap<u64, String> = HashMap::new();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(args.duration_secs);
    let mut captured_updates: u64 = 0;
    let mut dropped_events_total: u64 = 0;

    loop {
        let next = tokio::time::timeout_at(deadline, stream.next()).await;
        let update = match next {
            Ok(Some(Ok(u))) => u,
            Ok(Some(Err(e))) => {
                eprintln!("[console_snap] stream error: {e}");
                break;
            }
            Ok(None) => {
                eprintln!("[console_snap] stream ended");
                break;
            }
            Err(_) => {
                eprintln!("[console_snap] sample window elapsed");
                break;
            }
        };
        captured_updates += 1;
        if let Some(tu) = &update.task_update {
            dropped_events_total = dropped_events_total.saturating_add(tu.dropped_events);
            for new_task in &tu.new_tasks {
                let id = match new_task.id.as_ref() {
                    Some(x) => x.id,
                    None => continue,
                };
                let kind = match new_task.kind {
                    0 => "SPAWN",
                    1 => "BLOCKING",
                    _ => "OTHER",
                }
                .to_string();
                let meta_id = new_task.metadata.as_ref().map(|m| m.id);
                let name = meta_id
                    .and_then(|mid| names.get(&mid).cloned())
                    .unwrap_or_default();
                tasks.entry(id).or_insert_with(|| TaskAgg {
                    name,
                    meta_id,
                    kind,
                    ..Default::default()
                });
            }
            for (id, stats) in &tu.stats_update {
                let entry = tasks.entry(*id).or_default();
                let prev_polls = entry.polls;
                let prev_busy = entry.busy_ns;
                entry.polls = stats.poll_stats.as_ref().map_or(0, |p| p.polls);
                entry.busy_ns =
                    dur_to_ns(stats.poll_stats.as_ref().and_then(|p| p.busy_time.as_ref()));
                entry.scheduled_ns = dur_to_ns(stats.scheduled_time.as_ref());
                let dp = entry.polls.saturating_sub(prev_polls);
                let db = entry.busy_ns.saturating_sub(prev_busy);
                entry.window_polls = entry.window_polls.saturating_add(dp);
                entry.window_busy_ns = entry.window_busy_ns.saturating_add(db);
            }
        }
        if let Some(nm) = &update.new_metadata {
            for new_meta in &nm.metadata {
                if let (Some(id), Some(payload)) =
                    (new_meta.id.as_ref(), new_meta.metadata.as_ref())
                {
                    names.insert(id.id, payload.name.clone());
                }
            }
            // Backfill names: for any task whose meta_id matches a newly
            // registered metadata entry, set its name.
            for entry in tasks.values_mut() {
                if entry.name.is_empty() {
                    if let Some(mid) = entry.meta_id {
                        if let Some(n) = names.get(&mid) {
                            if !n.is_empty() {
                                entry.name = n.clone();
                            }
                        }
                    }
                }
            }
        }
    }

    // Build report.
    let mut rows: Vec<TaskRow> = tasks
        .values()
        .filter_map(|t| {
            if t.window_polls == 0 && t.polls == 0 {
                return None;
            }
            let mean = t.busy_ns.checked_div(t.polls).unwrap_or(0);
            Some(TaskRow {
                name: if t.name.is_empty() {
                    "<unmetadatized>".into()
                } else {
                    t.name.clone()
                },
                kind: t.kind.clone(),
                polls: t.polls,
                busy_ns: t.busy_ns,
                mean_poll_ns: mean,
                scheduled_ns: t.scheduled_ns,
                slowest_poll_ns: t.slowest_poll_ns,
            })
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.busy_ns));
    rows.truncate(args.top);
    let totals = Totals {
        unique_tasks: tasks.len() as u64,
        total_polls: tasks.values().map(|t| t.polls).sum(),
        total_busy_ns: tasks.values().map(|t| t.busy_ns).sum(),
    };
    let report = SnapReport {
        schema_version: 1,
        target: args.target.clone(),
        sample_seconds: args.duration_secs as f64,
        captured_updates,
        dropped_events_total,
        top_tasks: rows.clone(),
        totals,
    };

    let json_path = args.out.join("console-snap.json");
    let txt_path = args.out.join("console-snap.txt");
    let json = serde_json::to_string_pretty(&report)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    fs::write(&json_path, format!("{json}\n"))?;
    let mut txt = String::new();
    txt.push_str(&format!(
        "tokio-console snapshot: {} updates in {:.0}s (dropped={})\n",
        report.captured_updates, report.sample_seconds, report.dropped_events_total
    ));
    txt.push_str(&format!(
        "totals: tasks={}, polls={}, busy={:.2} ms\n",
        report.totals.unique_tasks,
        report.totals.total_polls,
        report.totals.total_busy_ns as f64 / 1_000_000.0
    ));
    txt.push_str(&format!(
        "\nTop {} tasks by busy time:\n{:>4}  {:>10}  {:>10}  {:>12}  {:>12}  {}\n",
        args.top, "#", "kind", "polls", "busy(us)", "mean(us)", "name"
    ));
    for (i, r) in rows.iter().enumerate() {
        txt.push_str(&format!(
            "{:>4}  {:>10}  {:>10}  {:>12}  {:>12}  {}\n",
            i + 1,
            r.kind,
            r.polls,
            r.busy_ns / 1_000,
            r.mean_poll_ns / 1_000,
            r.name
        ));
    }
    fs::write(&txt_path, txt)?;

    eprintln!(
        "[console_snap] wrote {} + {}",
        json_path.display(),
        txt_path.display()
    );
    Ok(())
}
