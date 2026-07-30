#![deny(unsafe_code)]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used
)]
//! Test orchestration tool — bundles service management, data loading,
//! index generation, and report generation into a single binary.
//!
//! Replaces several shell scripts:
//!   - wait_for_services.sh   → `test_runner wait <profile>`
//!   - load_qdrant.sh + load_elastic.sh  → `test_runner load-data [opts]`
//!   - generate_index.sh      → `test_runner index <results-dir>`
//!   - generate_report.sh     → `test_runner report --input <json> [opts]`
//!
//! Usage:
//!   cargo run --bin test_runner -- wait all
//!   cargo run --bin test_runner -- load-data
//!   cargo run --bin test_runner -- index tests/results/20260225-120000
//!   cargo run --bin test_runner -- report --input results.json --output report.md

// The test_infra module lives under tests/ and is compiled as part of test crates.
// For this binary, we inline the same logic to avoid path gymnastics.

// ---------------------------------------------------------------------------
// Performance digest thresholds — read-favored philosophy
//
// conproxy is a cache proxy: reads are the hot path (queries, health, stats)
// and must be fast. Writes/batch ops are cold-path and get relaxed budgets.
// ---------------------------------------------------------------------------

// Read endpoints: *_query, *_health, *_stats
const READ_P99_WARN_MS: f64 = 5.0;
const READ_P99_FAIL_MS: f64 = 25.0;

// Write/batch endpoints: *_batch, *_warmup, *_insert, *_clear
const WRITE_P99_WARN_MS: f64 = 50.0;
const WRITE_P99_FAIL_MS: f64 = 200.0;

// Cache hit rate — elevated; reads depend on cache
const CACHE_HIT_OK: f64 = 0.95;
const CACHE_HIT_WARN: f64 = 0.80;

const RSS_GROWTH_WARN_PCT: f64 = 20.0;
const RSS_GROWTH_FAIL_PCT: f64 = 100.0;
const E2E_PASS_RATE_WARN: f64 = 0.90;
const BENCH_REGRESSION_THRESHOLD: f64 = 15.0;

// Minimum read throughput (RPS per endpoint under 10-user load)
const READ_RPS_WARN: f64 = 200.0;
const READ_RPS_FAIL: f64 = 50.0;

// Tail latency amplification thresholds (P99.9 / P99 ratio)
const TAIL_AMP_WARN: f64 = 3.0;
const TAIL_AMP_SEVERE: f64 = 5.0;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        print_usage();
        std::process::exit(1);
    }

    match args[1].as_str() {
        "wait" => cmd_wait(&args[2..]).await,
        "load-data" => cmd_load_data(&args[2..]).await,
        "index" => cmd_index(&args[2..]),
        "report" => cmd_report(&args[2..]),
        "proc-monitor" => cmd_proc_monitor(&args[2..]),
        "-h" | "--help" | "help" => print_usage(),
        other => {
            eprintln!("Unknown subcommand: {other}");
            print_usage();
            std::process::exit(1);
        }
    }
}

fn print_usage() {
    eprintln!("Usage: test_runner <subcommand> [options]");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  wait <profile>        Wait for Docker services (qdrant|elastic|meili|mixed|all)");
    eprintln!("  load-data [options]   Load test fixtures into Qdrant/Elasticsearch/Meilisearch");
    eprintln!("  index <results-dir>   Generate index.html for a results directory");
    eprintln!("  report [options]      Generate markdown/HTML report from results JSON");
    eprintln!();
    eprintln!("load-data options:");
    eprintln!("  --qdrant-url <url>    Qdrant URL (default: http://localhost:6333)");
    eprintln!("  --es-url <url>        Elasticsearch URL (default: http://localhost:9200)");
    eprintln!("  --es-port2 <port>     Second ES port (default: 9201)");
    eprintln!("  --meili-url <url>     Meilisearch URL (default: http://localhost:7700)");
    eprintln!("  --meili-port2 <port>  Second Meilisearch port (default: 7701)");
    eprintln!("  --meili-key <key>     Meilisearch master key (default: conproxy_test_key)");
    eprintln!("  --docs <path>         Path to sample_docs.json");
    eprintln!("  --collection <name>   Qdrant collection (default: conproxy_test)");
    eprintln!("  --index <name>        ES/Meilisearch index name (default: conproxy_test)");
    eprintln!();
    eprintln!("report options:");
    eprintln!("  --input <file>        Input results.json (required)");
    eprintln!("  --output <file>       Output markdown file");
    eprintln!("  --compare <file>      Previous results.json for comparison");
    eprintln!("  --html <file>         Output HTML file");
    eprintln!();
    eprintln!(
        "  proc-monitor [opts]     Monitor a process via /proc (+ optional perf/bpftrace) and write profile_results.json"
    );
    eprintln!("    Options:");
    eprintln!("      --pid <pid>           PID to monitor (required)");
    eprintln!("      --output-dir <dir>    Output directory (required)");
    eprintln!("      --perf                Enable perf record (CPU profiling)");
    eprintln!(
        "      --bpftrace            Enable bpftrace (memory + syscall profiling, requires sudo)"
    );
    eprintln!(
        "      --dhat                Collect dhat-heap.json (binary must be built with --features dhat-heap)"
    );
    eprintln!("      --freq <hz>           CPU sampling frequency (default: 99)");
    eprintln!(
        "      --diff-baseline <path>  Path to baseline cpu_folded.txt for differential flamegraph"
    );
    eprintln!("      --resource-profile <name>  cgroup v2 limits: small | medium | large");
    eprintln!("      --ready-file <path>   Write marker file after init (for Makefile sync)");
}

// ---------------------------------------------------------------------------
// wait subcommand
// ---------------------------------------------------------------------------

async fn cmd_wait(args: &[String]) {
    let profile = args.first().map(|s| s.as_str()).unwrap_or("all");

    let timeout = std::time::Duration::from_secs(
        std::env::var("TIMEOUT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120),
    );
    let interval = std::time::Duration::from_secs(
        std::env::var("INTERVAL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5),
    );

    let endpoints: Vec<(&str, &str)> = match profile {
        "qdrant" => vec![("Qdrant :6333", "http://localhost:6333/readyz")],
        "elastic" => vec![(
            "Elasticsearch :9200",
            "http://localhost:9200/_cluster/health",
        )],
        "meili" => vec![
            ("Meilisearch :7700", "http://localhost:7700/health"),
            ("Meilisearch :7701", "http://localhost:7701/health"),
        ],
        "mixed" => vec![
            ("Qdrant :6333", "http://localhost:6333/readyz"),
            ("Meilisearch :7700", "http://localhost:7700/health"),
        ],
        "all" => vec![
            ("Qdrant :6333", "http://localhost:6333/readyz"),
            ("Meilisearch :7700", "http://localhost:7700/health"),
            ("Meilisearch :7701", "http://localhost:7701/health"),
        ],
        _ => {
            eprintln!("Unknown profile: {profile}. Use: qdrant, elastic, meili, mixed, all");
            std::process::exit(1);
        }
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("Failed to build HTTP client");

    for (name, url) in &endpoints {
        eprintln!("Waiting for {name}...");
        let start = std::time::Instant::now();
        loop {
            if let Ok(resp) = client.get(*url).send().await {
                if resp.status().is_success() {
                    eprintln!("{name} is ready");
                    break;
                }
            }
            if start.elapsed() >= timeout {
                eprintln!(
                    "ERROR: {name} did not become ready within {:.0}s",
                    timeout.as_secs_f64()
                );
                std::process::exit(1);
            }
            tokio::time::sleep(interval).await;
        }
    }

    eprintln!("All services are ready!");
}

// ---------------------------------------------------------------------------
// load-data subcommand
// ---------------------------------------------------------------------------

async fn cmd_load_data(args: &[String]) {
    let mut qdrant_url = "http://localhost:6333".to_string();
    let mut es_url = "http://localhost:9200".to_string();
    let mut es_port2: u16 = 9201;
    let mut meili_url = "http://localhost:7700".to_string();
    let mut meili_port2: u16 = 7701;
    let mut meili_key = "conproxy_test_key".to_string();
    let mut collection = "conproxy_test".to_string();
    let mut index = "conproxy_test".to_string();
    let mut docs_path: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--qdrant-url" if i + 1 < args.len() => {
                qdrant_url = args[i + 1].clone();
                i += 2;
            }
            "--es-url" if i + 1 < args.len() => {
                es_url = args[i + 1].clone();
                i += 2;
            }
            "--es-port2" if i + 1 < args.len() => {
                es_port2 = args[i + 1].parse().unwrap_or(9201);
                i += 2;
            }
            "--meili-url" if i + 1 < args.len() => {
                meili_url = args[i + 1].clone();
                i += 2;
            }
            "--meili-port2" if i + 1 < args.len() => {
                meili_port2 = args[i + 1].parse().unwrap_or(7701);
                i += 2;
            }
            "--meili-key" if i + 1 < args.len() => {
                meili_key = args[i + 1].clone();
                i += 2;
            }
            "--collection" if i + 1 < args.len() => {
                collection = args[i + 1].clone();
                i += 2;
            }
            "--index" if i + 1 < args.len() => {
                index = args[i + 1].clone();
                i += 2;
            }
            "--docs" if i + 1 < args.len() => {
                docs_path = Some(args[i + 1].clone());
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let data_dir = std::path::PathBuf::from(manifest_dir)
        .join("tests")
        .join("e2e")
        .join("data");
    let docs_file = docs_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| data_dir.join("sample_docs.json"));
    let embeddings_file = data_dir.join("embeddings.json");

    // Load into Qdrant
    eprintln!("Loading into Qdrant ({qdrant_url})...");
    let emb_opt = if embeddings_file.exists() {
        Some(embeddings_file.as_path())
    } else {
        None
    };
    match load_docs_qdrant(&qdrant_url, &collection, &docs_file, emb_opt, 384).await {
        Ok(n) => eprintln!("Qdrant: loaded {n} documents"),
        Err(e) => eprintln!("Qdrant load failed: {e}"),
    }

    // Load into Elasticsearch (port 9200)
    eprintln!("Loading into Elasticsearch ({es_url})...");
    match load_docs_elasticsearch(&es_url, &index, &docs_file).await {
        Ok(n) => eprintln!("ES :9200: loaded {n} documents"),
        Err(e) => eprintln!("ES load failed: {e}"),
    }

    // Load into second ES instance
    let es_url2 = format!("http://localhost:{es_port2}");
    eprintln!("Loading into Elasticsearch ({es_url2})...");
    match load_docs_elasticsearch(&es_url2, &index, &docs_file).await {
        Ok(n) => eprintln!("ES :{es_port2}: loaded {n} documents"),
        Err(e) => eprintln!("ES :{es_port2} load failed: {e}"),
    }

    // Load into Meilisearch (port 7700)
    eprintln!("Loading into Meilisearch ({meili_url})...");
    match load_docs_meilisearch(&meili_url, &meili_key, &index, &docs_file).await {
        Ok(n) => eprintln!("Meili :7700: loaded {n} documents"),
        Err(e) => eprintln!("Meili :7700 load failed: {e}"),
    }

    // Load into second Meilisearch instance
    let meili_url2 = format!("http://localhost:{meili_port2}");
    eprintln!("Loading into Meilisearch ({meili_url2})...");
    match load_docs_meilisearch(&meili_url2, &meili_key, &index, &docs_file).await {
        Ok(n) => eprintln!("Meili :{meili_port2}: loaded {n} documents"),
        Err(e) => eprintln!("Meili :{meili_port2} load failed: {e}"),
    }
}

async fn load_docs_qdrant(
    url: &str,
    collection: &str,
    docs_file: &std::path::Path,
    embeddings_file: Option<&std::path::Path>,
    dims: usize,
) -> Result<usize, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("HTTP client: {e}"))?;

    // Create collection
    let _ = client
        .put(format!("{url}/collections/{collection}"))
        .json(&serde_json::json!({ "vectors": { "size": dims, "distance": "Cosine" } }))
        .send()
        .await;

    let has_emb = embeddings_file.map(|p| p.exists()).unwrap_or(false);

    if has_emb {
        let content = std::fs::read_to_string(embeddings_file.unwrap())
            .map_err(|e| format!("Read embeddings: {e}"))?;
        let entries: Vec<serde_json::Value> =
            serde_json::from_str(&content).map_err(|e| format!("Parse embeddings: {e}"))?;
        let points_url = format!("{url}/collections/{collection}/points");
        let mut count = 0;
        for entry in &entries {
            let doc_id = entry["id"].as_str().unwrap_or("unknown");
            let nid = sha256_numeric_id(doc_id);
            let point = serde_json::json!({
                "points": [{ "id": nid, "vector": entry["vector"], "payload": entry["payload"] }]
            });
            if let Ok(resp) = client.put(&points_url).json(&point).send().await {
                if resp.status().is_success() {
                    count += 1;
                }
            }
        }
        Ok(count)
    } else {
        eprintln!("WARNING: embeddings.json not found — using placeholder vectors");
        let content = std::fs::read_to_string(docs_file).map_err(|e| format!("Read docs: {e}"))?;
        let json: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| format!("Parse: {e}"))?;
        let documents = json["documents"]
            .as_array()
            .ok_or("Missing 'documents' array")?;
        let points_url = format!("{url}/collections/{collection}/points");
        let mut count = 0;
        for doc in documents {
            let doc_id = doc["id"].as_str().unwrap_or("unknown");
            let vector = placeholder_vector(doc_id, dims);
            let nid = sha256_numeric_id(doc_id);
            let point = serde_json::json!({
                "points": [{ "id": nid, "vector": vector, "payload": {
                    "doc_id": doc_id, "title": doc["title"], "content": doc["content"],
                    "category": doc["category"], "tags": doc["tags"]
                }}]
            });
            if let Ok(resp) = client.put(&points_url).json(&point).send().await {
                if resp.status().is_success() {
                    count += 1;
                }
            }
        }
        Ok(count)
    }
}

async fn load_docs_elasticsearch(
    url: &str,
    index: &str,
    docs_file: &std::path::Path,
) -> Result<usize, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("HTTP client: {e}"))?;

    // Create index
    let _ = client
        .put(format!("{url}/{index}"))
        .json(&serde_json::json!({
            "settings": { "number_of_shards": 1, "number_of_replicas": 0 },
            "mappings": { "properties": {
                "doc_id": { "type": "keyword" }, "title": { "type": "text" },
                "content": { "type": "text" }, "category": { "type": "keyword" },
                "tags": { "type": "keyword" }
            }}
        }))
        .send()
        .await;

    let content = std::fs::read_to_string(docs_file).map_err(|e| format!("Read: {e}"))?;
    let json: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("Parse: {e}"))?;
    let documents = json["documents"]
        .as_array()
        .ok_or("Missing 'documents' array")?;

    let mut bulk = String::new();
    for doc in documents {
        let id = doc["id"].as_str().unwrap_or("unknown");
        let action = serde_json::json!({"index": {"_id": id}});
        let body = serde_json::json!({
            "doc_id": id, "title": doc["title"], "content": doc["content"],
            "category": doc["category"], "tags": doc["tags"]
        });
        bulk.push_str(&serde_json::to_string(&action).unwrap());
        bulk.push('\n');
        bulk.push_str(&serde_json::to_string(&body).unwrap());
        bulk.push('\n');
    }

    let resp = client
        .post(format!("{url}/{index}/_bulk"))
        .header("Content-Type", "application/x-ndjson")
        .body(bulk)
        .send()
        .await
        .map_err(|e| format!("Bulk: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("ES bulk returned {}", resp.status()));
    }

    let _ = client.post(format!("{url}/{index}/_refresh")).send().await;

    let count = if let Ok(r) = client.get(format!("{url}/{index}/_count")).send().await {
        r.json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v["count"].as_u64())
            .unwrap_or(documents.len() as u64) as usize
    } else {
        documents.len()
    };

    Ok(count)
}

async fn load_docs_meilisearch(
    url: &str,
    api_key: &str,
    index: &str,
    docs_file: &std::path::Path,
) -> Result<usize, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("HTTP client: {e}"))?;

    // Delete the index if it exists (so re-runs are idempotent).
    let _ = client
        .delete(format!("{url}/indexes/{index}"))
        .header("Authorization", format!("Bearer {api_key}"))
        .send()
        .await;

    // Create the index with the `id` primary key.
    let resp = client
        .post(format!("{url}/indexes"))
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&serde_json::json!({"uid": index, "primaryKey": "id"}))
        .send()
        .await
        .map_err(|e| format!("Create index: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Meili create index returned {}", resp.status()));
    }

    // Read documents from file.
    let content = std::fs::read_to_string(docs_file).map_err(|e| format!("Read: {e}"))?;
    let json: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("Parse: {e}"))?;
    let documents = json["documents"]
        .as_array()
        .ok_or("Missing 'documents' array")?;

    // Bulk-add documents to the index.
    let resp = client
        .post(format!("{url}/indexes/{index}/documents?primaryKey=id"))
        .header("Authorization", format!("Bearer {api_key}"))
        .json(documents)
        .send()
        .await
        .map_err(|e| format!("Add documents: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Meili add docs returned {}", resp.status()));
    }

    // Give Meili a brief moment to index (it's fast — sub-second for 10 docs).
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    Ok(documents.len())
}

fn placeholder_vector(doc_id: &str, dims: usize) -> Vec<f64> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    // Simple deterministic vector using hash
    let mut hasher = DefaultHasher::new();
    doc_id.hash(&mut hasher);
    let seed = hasher.finish();
    (0..dims)
        .map(|i| {
            let mut h = DefaultHasher::new();
            (seed, i).hash(&mut h);
            (h.finish() % 256) as f64 / 255.0
        })
        .collect()
}

fn sha256_numeric_id(doc_id: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    doc_id.hash(&mut hasher);
    hasher.finish() & 0xFFFFFFFF // 32-bit for Qdrant compatibility
}

// ---------------------------------------------------------------------------
// perf digest helpers
// ---------------------------------------------------------------------------

/// Generate an HTML stat div with machine-readable data-* attributes.
fn stat_div(display: &str, label: &str, metric: &str, raw_value: &str, unit: &str) -> String {
    format!(
        "<div class=\"stat\" data-metric=\"{metric}\" data-value=\"{raw}\" data-unit=\"{unit}\">\
         <div class=\"value\">{display}</div><div class=\"label\">{label}</div></div>\n",
        metric = html_escape(metric),
        raw = html_escape(raw_value),
        unit = html_escape(unit),
        display = display,
        label = label,
    )
}

/// Build an assessment JSON object.
fn assess(level: &str, metric: &str, message: &str) -> serde_json::Value {
    serde_json::json!({ "level": level, "metric": metric, "message": message })
}

/// Flexibly extract an f64 from a JSON value that may be a number or a string.
/// Handles cases like `"0.818"` or `"1.000"` which `.as_f64()` returns None for.
fn as_f64_flexible(val: &serde_json::Value) -> Option<f64> {
    val.as_f64()
        .or_else(|| val.as_str().and_then(|s| s.parse::<f64>().ok()))
}

/// Compute RSS trend from profile snapshots.
/// Returns (trend_label, growth_percent).
fn compute_rss_trend(snapshots: &[serde_json::Value]) -> (&'static str, f64) {
    if snapshots.len() < 2 {
        return ("stable", 0.0);
    }
    let rss_values: Vec<f64> = snapshots
        .iter()
        .filter_map(|s| s["rss_bytes"].as_u64().map(|v| v as f64))
        .collect();
    if rss_values.len() < 2 {
        return ("stable", 0.0);
    }
    let first = rss_values[0];
    let last = *rss_values.last().unwrap();
    let growth_pct = if first > 0.0 {
        ((last - first) / first) * 100.0
    } else {
        0.0
    };

    // Check monotonicity
    let mut increasing = true;
    let mut decreasing = true;
    for w in rss_values.windows(2) {
        if w[1] < w[0] {
            increasing = false;
        }
        if w[1] > w[0] {
            decreasing = false;
        }
    }

    let trend = if increasing && growth_pct > 5.0 {
        "increasing"
    } else if decreasing && growth_pct.abs() > 5.0 {
        "decreasing"
    } else if growth_pct.abs() <= 5.0 {
        "stable"
    } else {
        "spiky"
    };

    (trend, growth_pct)
}

/// Classify a raw perf function name into (short_name, source).
/// e.g. "libc.so.6 [.] malloc" -> ("malloc", "libc")
///      "[k] page_fault" -> ("page_fault", "kernel")
///      "0x55abc123" -> ("0x55abc123", "conproxy (unresolved)")
fn classify_cpu_function(raw_name: &str) -> (String, String) {
    let raw = raw_name.trim();

    // Format: "libfoo.so.6 [.] func_name" or "[k] func_name"
    if let Some(bracket_pos) = raw.find("[.] ") {
        let func = raw[bracket_pos + 4..].trim().to_string();
        let lib_part = raw[..bracket_pos].trim();
        let source = if lib_part.contains("libc") {
            "libc"
        } else if lib_part.contains("libpthread") || lib_part.contains("libm") {
            lib_part.split('.').next().unwrap_or(lib_part)
        } else if lib_part.is_empty() || lib_part.contains("conproxy") {
            "conproxy"
        } else {
            lib_part.split('.').next().unwrap_or(lib_part)
        };
        return (func, source.to_string());
    }
    if let Some(bracket_pos) = raw.find("[k] ") {
        let func = raw[bracket_pos + 4..].trim().to_string();
        return (func, "kernel".to_string());
    }

    // Unresolved hex address
    if raw.starts_with("0x") || raw.starts_with("[0x") {
        return (raw.to_string(), "conproxy (unresolved)".to_string());
    }

    // Bare function name — assume conproxy
    (raw.to_string(), "conproxy".to_string())
}

// ---------------------------------------------------------------------------
// perf digest generation
// ---------------------------------------------------------------------------

fn generate_audit_digest(dir: &std::path::Path) {
    let dir_name = dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent_name = dir
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    // Profile-aware layout: .../YYYYMMDD-HHMMSS/<profile>/
    // Legacy flat layout:   .../YYYYMMDD-HHMMSS/  (dir_name IS the timestamp)
    let is_timestamp = dir_name.len() == 15 && dir_name.chars().nth(8) == Some('-');
    let (timestamp, profile) = if is_timestamp {
        // Legacy flat layout — dir itself is the timestamp
        (dir_name.clone(), "default".to_string())
    } else {
        // Profile-aware layout
        (parent_name, dir_name)
    };

    // Read section statuses and durations
    let ordered = ["lint", "unit", "coverage", "bench", "e2e", "load", "eval"];
    let mut sections = serde_json::Map::new();
    let mut has_failure = false;
    let mut total_duration_secs: u64 = 0;

    for &name in &ordered {
        let section_dir = dir.join(name);
        if !section_dir.is_dir() {
            continue;
        }
        let status = detect_section_status(dir, name);
        let duration = read_section_duration(dir, name);
        if status == "fail" {
            has_failure = true;
        }
        let mut obj = serde_json::json!({ "status": status });
        if let Some(secs) = duration {
            obj["duration_secs"] = serde_json::json!(secs);
            total_duration_secs += secs;
        }
        sections.insert(name.to_string(), obj);
    }

    let overall = if has_failure { "fail" } else { "pass" };

    let mut digest = serde_json::json!({
        "version": 2,
        "timestamp": timestamp,
        "profile": profile,
        "overall": overall,
        "total_duration_secs": total_duration_secs,
        "sections": sections,
    });

    // Build each sub-digest
    let load_dir = dir.join("load");
    if load_dir.join("summary.json").exists() {
        if let Some(val) = build_load_digest(&load_dir) {
            digest["load"] = val;
        }
    }

    let bench_dir = dir.join("bench");
    if bench_dir.is_dir() {
        if let Some(val) = build_bench_digest(&bench_dir) {
            digest["bench"] = val;
        }
    }

    // Hit-rate bench digest (bench-hitrate / bench-hitrate-sem run dirs):
    // top-level summary.json with "tool": "hitrate_bench".
    if dir.join("summary.json").exists() {
        if let Some(val) = build_hitrate_digest(dir) {
            digest["hitrate"] = val;
        }
    }

    let e2e_dir = dir.join("e2e");
    if e2e_dir.join("results.json").exists() {
        if let Some(val) = build_e2e_digest(&e2e_dir) {
            digest["e2e"] = val;
        }
    }

    let eval_dir = dir.join("eval");
    if eval_dir.join("eval_results.json").exists() {
        if let Some(val) = build_eval_digest(&eval_dir) {
            digest["eval"] = val;
        }
    }

    // Coverage digest
    let coverage_dir = dir.join("coverage");
    if coverage_dir.join("tarpaulin-report.json").exists() {
        if let Some(val) = build_coverage_digest(&coverage_dir) {
            digest["coverage"] = val;
        }
    }

    // Profile digests for any stage that has profile_results.json
    let mut profiles = serde_json::Map::new();
    for &stage in &["load", "e2e", "eval"] {
        let stage_dir = dir.join(stage);
        if stage_dir.join("profile_results.json").exists() {
            if let Some(val) = build_profile_digest(&stage_dir) {
                profiles.insert(stage.to_string(), val);
            }
        }
    }
    if !profiles.is_empty() {
        digest["profiles"] = serde_json::Value::Object(profiles);
    }

    // Cascade digest from proxy logs (check both load and e2e)
    for &stage in &["load", "e2e"] {
        let stage_dir = dir.join(stage);
        if stage_dir.join("proxy_logs.txt").exists() {
            if let Some(val) = build_cascade_digest(&stage_dir) {
                digest["cascade"] = val;
                break; // Use first available (load preferred)
            }
        }
    }

    // --- v2 additions ---

    // Bpftrace digests per stage
    let mut bpftrace = serde_json::Map::new();
    for &stage in &["load", "e2e", "eval"] {
        let stage_dir = dir.join(stage);
        if stage_dir.is_dir() {
            if let Some(val) = build_bpftrace_digest(&stage_dir) {
                bpftrace.insert(stage.to_string(), val);
            }
        }
    }
    if !bpftrace.is_empty() {
        digest["bpftrace"] = serde_json::Value::Object(bpftrace);
    }

    // Proxy logs digest per stage
    let mut proxy_logs = serde_json::Map::new();
    for &stage in &["load", "e2e"] {
        let stage_dir = dir.join(stage);
        if stage_dir.join("proxy_logs.txt").exists() {
            if let Some(val) = build_proxy_logs_digest(&stage_dir) {
                proxy_logs.insert(stage.to_string(), val);
            }
        }
    }
    if !proxy_logs.is_empty() {
        digest["proxy_logs"] = serde_json::Value::Object(proxy_logs);
    }

    // Security digest
    if let Some(val) = build_security_digest(dir) {
        digest["security"] = val;
    }

    // Operational digest
    if let Some(val) = build_operational_digest(dir) {
        digest["operational"] = val;
    }

    // Build observations
    digest["observations"] = build_observations(&digest);

    // Write audit_digest.json (v2 primary output)
    let audit_digest_path = dir.join("audit_digest.json");
    match serde_json::to_string_pretty(&digest) {
        Ok(json_str) => {
            if let Err(e) = std::fs::write(&audit_digest_path, &json_str) {
                eprintln!("Warning: failed to write audit_digest.json: {e}");
            } else {
                eprintln!("Generated {}", audit_digest_path.display());
            }
            // Backward-compat copy as perf_digest.json
            let compat_path = dir.join("perf_digest.json");
            if let Err(e) = std::fs::write(&compat_path, &json_str) {
                eprintln!("Warning: failed to write perf_digest.json (compat): {e}");
            }
        }
        Err(e) => eprintln!("Warning: failed to serialize audit_digest: {e}"),
    }
}

fn build_load_digest(load_dir: &std::path::Path) -> Option<serde_json::Value> {
    let summary_str = std::fs::read_to_string(load_dir.join("summary.json")).ok()?;
    let summary: serde_json::Value = serde_json::from_str(&summary_str).ok()?;
    let benchmarks = summary["benchmarks"].as_array()?;

    let proxy_stats: Option<serde_json::Value> =
        std::fs::read_to_string(load_dir.join("proxy_stats.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok());

    let mut scenarios = Vec::new();
    let mut total_rps: f64 = 0.0;
    let mut total_requests: u64 = 0;
    let mut total_errors: u64 = 0;

    for b in benchmarks {
        let name = b["name"].as_str().unwrap_or("unknown").to_string();
        let rps = b["raw_metrics"]["summary"]["iters"]["rate"]
            .as_f64()
            .unwrap_or(0.0);
        let iters = b["raw_metrics"]["summary"]["iters"]["total"]
            .as_u64()
            .unwrap_or(0);
        let p50 = b["raw_metrics"]["latency"]["percentiles"]["p50"]
            .as_f64()
            .unwrap_or(0.0);
        let p95 = b["raw_metrics"]["latency"]["percentiles"]["p95"]
            .as_f64()
            .unwrap_or(0.0);
        let p99 = b["raw_metrics"]["latency"]["percentiles"]["p99"]
            .as_f64()
            .unwrap_or(0.0);
        // Tail latency percentiles (keys contain dots: "p99.9", "p99.99")
        let p999 = b["raw_metrics"]["latency"]["percentiles"]["p99.9"]
            .as_f64()
            .unwrap_or(0.0);
        let p9999 = b["raw_metrics"]["latency"]["percentiles"]["p99.99"]
            .as_f64()
            .unwrap_or(0.0);

        let mut errors: u64 = 0;
        if let Some(status_map) = b["raw_metrics"]["status"].as_object() {
            for (key, count) in status_map {
                if !key.starts_with("Success") {
                    errors += count.as_u64().unwrap_or(0);
                }
            }
        }

        total_rps += rps;
        total_requests += iters;
        total_errors += errors;

        let error_rate = if iters > 0 {
            errors as f64 / iters as f64
        } else {
            0.0
        };

        // Classify scenario: reads (query, health, stats) get tight budgets;
        // writes/batch (batch, warmup, insert, clear) get relaxed budgets.
        let is_read = name.contains("query") || name.contains("health") || name.contains("stats");
        let (p99_warn, p99_fail) = if is_read {
            (READ_P99_WARN_MS, READ_P99_FAIL_MS)
        } else {
            (WRITE_P99_WARN_MS, WRITE_P99_FAIL_MS)
        };
        let kind = if is_read { "read" } else { "write" };

        // Convert latency from seconds to ms for the digest
        let p50_ms = p50 * 1000.0;
        let p95_ms = p95 * 1000.0;
        let p99_ms = p99 * 1000.0;
        let p999_ms = p999 * 1000.0;
        let p9999_ms = p9999 * 1000.0;

        // Tail latency amplification (P99.9 / P99)
        let tail_amplification = if p99_ms > 0.0 { p999_ms / p99_ms } else { 0.0 };

        let mut assessments = Vec::new();

        // Error rate assessment
        if error_rate > 0.5 {
            assessments.push(assess(
                "FAIL",
                "error_rate",
                &format!(
                    "{:.0}% error rate — most requests failed",
                    error_rate * 100.0
                ),
            ));
        } else if error_rate > 0.0 {
            assessments.push(assess(
                "WARN",
                "error_rate",
                &format!("{:.1}% error rate", error_rate * 100.0),
            ));
        }

        // P99 latency assessment (read-favored thresholds)
        if p99_ms > p99_fail {
            assessments.push(assess(
                "FAIL",
                "p99_latency",
                &format!("p99 latency {p99_ms:.1}ms exceeds {kind} threshold {p99_fail}ms"),
            ));
        } else if p99_ms > p99_warn {
            assessments.push(assess(
                "WARN",
                "p99_latency",
                &format!("p99 latency {p99_ms:.1}ms above {kind} warn {p99_warn}ms"),
            ));
        }

        // Read throughput assessment
        if is_read {
            if rps < READ_RPS_FAIL {
                assessments.push(assess(
                    "FAIL",
                    "read_throughput",
                    &format!("read throughput {rps:.0} RPS below minimum {READ_RPS_FAIL}"),
                ));
            } else if rps < READ_RPS_WARN {
                assessments.push(assess(
                    "WARN",
                    "read_throughput",
                    &format!("read throughput {rps:.0} RPS below target {READ_RPS_WARN}"),
                ));
            }
        }

        // Tail latency amplification assessment (read-favored: stricter for reads)
        if tail_amplification > 0.0 && p999_ms > 0.0 {
            let (tail_level, tail_threshold) = if is_read {
                if tail_amplification > TAIL_AMP_SEVERE {
                    ("FAIL", TAIL_AMP_SEVERE)
                } else if tail_amplification > TAIL_AMP_WARN {
                    ("WARN", TAIL_AMP_WARN)
                } else {
                    ("OK", 0.0)
                }
            } else {
                // Writes only WARN at severe level
                if tail_amplification > TAIL_AMP_SEVERE {
                    ("WARN", TAIL_AMP_SEVERE)
                } else {
                    ("OK", 0.0)
                }
            };
            if tail_level != "OK" {
                assessments.push(assess(
                    tail_level,
                    "tail_latency",
                    &format!(
                        "P99.9/P99 amplification {tail_amplification:.1}x (P99={p99_ms:.1}ms → P99.9={p999_ms:.1}ms) exceeds {tail_threshold:.0}x"
                    ),
                ));
            }
        }

        scenarios.push(serde_json::json!({
            "name": name,
            "kind": kind,
            "rps": rps,
            "total_requests": iters,
            "error_rate": error_rate,
            "latency": {
                "p50_ms": p50_ms,
                "p95_ms": p95_ms,
                "p99_ms": p99_ms,
                "p999_ms": p999_ms,
                "p9999_ms": p9999_ms,
                "tail_amplification": tail_amplification,
            },
            "assessments": assessments,
        }));
    }

    let overall_error_rate = if total_requests > 0 {
        total_errors as f64 / total_requests as f64
    } else {
        0.0
    };

    // Cache stats
    let cache_ctx = proxy_stats
        .as_ref()
        .and_then(|ps| ps["contexts"].as_array())
        .and_then(|arr| arr.first());
    let hit_rate = cache_ctx
        .and_then(|c| c["hit_rate"].as_f64())
        .unwrap_or(0.0);

    let mut cache_assessments = Vec::new();
    if hit_rate >= CACHE_HIT_OK {
        cache_assessments.push(assess(
            "OK",
            "hit_rate",
            &format!("Cache hit rate {:.2}%", hit_rate * 100.0),
        ));
    } else if hit_rate >= CACHE_HIT_WARN {
        cache_assessments.push(assess(
            "WARN",
            "hit_rate",
            &format!(
                "Cache hit rate {:.2}% — below {:.0}% target",
                hit_rate * 100.0,
                CACHE_HIT_OK * 100.0
            ),
        ));
    } else {
        cache_assessments.push(assess(
            "FAIL",
            "hit_rate",
            &format!("Cache hit rate {:.2}% — critically low", hit_rate * 100.0),
        ));
    }

    // Miss reasons extraction and error correlation
    let circuit_open = proxy_stats
        .as_ref()
        .and_then(|ps| ps["miss_reasons"]["circuit_open"].as_u64())
        .unwrap_or(0);
    let upstream_error = proxy_stats
        .as_ref()
        .and_then(|ps| ps["miss_reasons"]["upstream_error"].as_u64())
        .unwrap_or(0);
    let not_in_cache = proxy_stats
        .as_ref()
        .and_then(|ps| ps["miss_reasons"]["not_in_cache"].as_u64())
        .unwrap_or(0);
    let expired = proxy_stats
        .as_ref()
        .and_then(|ps| ps["miss_reasons"]["expired"].as_u64())
        .unwrap_or(0);

    let mut error_correlation = serde_json::json!({
        "circuit_open": circuit_open,
        "upstream_error": upstream_error,
        "not_in_cache": not_in_cache,
        "expired": expired,
        "load_test_errors": total_errors,
    });

    if circuit_open > 0 && total_errors > 0 {
        let overlap_pct = if total_errors > 0 {
            (circuit_open.min(total_errors) as f64 / total_errors as f64) * 100.0
        } else {
            0.0
        };
        error_correlation["correlation_note"] = serde_json::json!(format!(
            "Circuit breaker opened {circuit_open} times, load test saw {total_errors} errors — \
             circuit_open accounts for up to {overlap_pct:.0}% of errors"
        ));
        cache_assessments.push(assess(
            "WARN",
            "error_correlation",
            &format!(
                "Circuit breaker ({circuit_open}) correlates with load test errors ({total_errors})"
            ),
        ));
    }

    let mut result = serde_json::json!({
        "scenarios": scenarios,
        "aggregate": {
            "total_rps": total_rps,
            "total_requests": total_requests,
            "total_errors": total_errors,
            "error_rate": overall_error_rate,
        },
    });

    if proxy_stats.is_some() {
        result["cache"] = serde_json::json!({
            "hit_rate": hit_rate,
            "assessments": cache_assessments,
        });
        result["error_correlation"] = error_correlation;
    }

    Some(result)
}

fn build_bench_digest(bench_dir: &std::path::Path) -> Option<serde_json::Value> {
    let entries = std::fs::read_dir(bench_dir).ok()?;

    let mut profiles = Vec::new();
    let mut total_regressions = 0u64;
    let mut total_improvements = 0u64;
    let mut total_inconclusive = 0u64;

    for entry in entries.flatten() {
        let fname = entry.file_name();
        let fname_str = fname.to_string_lossy();
        if let Some(profile_name) = fname_str
            .strip_prefix("report_")
            .and_then(|s| s.strip_suffix(".json"))
        {
            if let Ok(content) = std::fs::read_to_string(entry.path()) {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                    let regressions = json["regressions"].as_array().cloned().unwrap_or_default();
                    let improvements = json["improvements"].as_array().cloned().unwrap_or_default();
                    let inconclusive = json["inconclusive"].as_array().cloned().unwrap_or_default();
                    total_regressions += regressions.len() as u64;
                    total_improvements += improvements.len() as u64;
                    total_inconclusive += inconclusive.len() as u64;
                    profiles.push(serde_json::json!({
                        "name": profile_name,
                        "regressions": regressions,
                        "improvements": improvements,
                        "inconclusive": inconclusive,
                    }));
                }
            }
        }
    }

    if profiles.is_empty() {
        return None;
    }

    let mut assessments = Vec::new();
    if total_regressions > 0 {
        assessments.push(assess(
            "FAIL",
            "regressions",
            &format!(
                "{total_regressions} regression(s) above {BENCH_REGRESSION_THRESHOLD:.0}% threshold"
            ),
        ));
    } else {
        assessments.push(assess(
            "OK",
            "regressions",
            &format!("No regressions above {BENCH_REGRESSION_THRESHOLD:.0}% threshold"),
        ));
    }
    if total_improvements > 0 {
        assessments.push(assess(
            "INFO",
            "improvements",
            &format!("{total_improvements} improvement(s) detected"),
        ));
    }
    if total_inconclusive > 0 {
        assessments.push(assess(
            "INFO",
            "inconclusive",
            &format!(
                "{total_inconclusive} inconclusive bench(es) — CI crosses threshold, needs more data"
            ),
        ));
    }

    Some(serde_json::json!({
        "profiles": profiles,
        "assessments": assessments,
    }))
}

/// Hit-rate bench digest (bench-hitrate / bench-hitrate-sem run dirs).
///
/// Triggered by a top-level `summary.json` carrying `"tool": "hitrate_bench"`.
/// Reads the optional `frontier.json` for semantic best-valid-τ data.
fn build_hitrate_digest(dir: &std::path::Path) -> Option<serde_json::Value> {
    let content = std::fs::read_to_string(dir.join("summary.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    if json["tool"].as_str() != Some("hitrate_bench") {
        return None;
    }

    let verdict = json["verdict"].as_str().unwrap_or("UNKNOWN").to_string();

    // Optional semantic frontier: workload name -> best-valid point
    let frontier_content = std::fs::read_to_string(dir.join("frontier.json")).ok();
    let frontier: serde_json::Value = frontier_content
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or(serde_json::Value::Null);

    let mut workloads = Vec::new();
    if let Some(arr) = json["workloads"].as_array() {
        for w in arr {
            let name = w["name"].as_str().unwrap_or("?").to_string();
            // Best exact hit rate across the cache-size sweep
            let best_exact = w["sweep"]
                .as_array()
                .map(|s| {
                    s.iter()
                        .filter_map(|p| p["exact_hit_rate"].as_f64())
                        .fold(0.0_f64, f64::max)
                })
                .unwrap_or(0.0);
            // Best-valid semantic point from frontier.json (if present)
            let mut best_tau: Option<f64> = None;
            let mut best_combined: Option<f64> = None;
            let mut best_false: Option<f64> = None;
            let mut best_uplift: Option<f64> = None;
            if let Some(farr) = frontier["workloads"].as_array() {
                for fw in farr {
                    if fw["name"].as_str() == Some(name.as_str()) {
                        let bv = &fw["best_valid"];
                        if !bv.is_null() {
                            best_tau = bv["tau"].as_f64();
                            best_combined = bv["combined_hit_rate"].as_f64();
                            best_false = bv["false_hit_rate"].as_f64();
                            best_uplift = bv["uplift"].as_f64();
                        }
                    }
                }
            }
            workloads.push(serde_json::json!({
                "name": name,
                "best_exact_hit_rate": best_exact,
                "best_tau": best_tau,
                "best_combined_hit_rate": best_combined,
                "best_false_hit_rate": best_false,
                "best_uplift": best_uplift,
            }));
        }
    }

    let mut assessments = Vec::new();
    match verdict.as_str() {
        "PASS" => assessments.push(assess("OK", "verdict", "hit-rate gates passed")),
        other => assessments.push(assess(
            "FAIL",
            "verdict",
            &format!("hit-rate verdict: {other}"),
        )),
    }

    Some(serde_json::json!({
        "verdict": verdict,
        "workloads": workloads,
        "assessments": assessments,
    }))
}

fn build_e2e_digest(e2e_dir: &std::path::Path) -> Option<serde_json::Value> {
    let content = std::fs::read_to_string(e2e_dir.join("results.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    let total = json["summary"]["total"].as_u64().unwrap_or(0);
    let passed = json["summary"]["passed"].as_u64().unwrap_or(0);
    let pass_rate = if total > 0 {
        passed as f64 / total as f64
    } else {
        0.0
    };

    let mut assessments = Vec::new();
    if pass_rate >= 1.0 {
        assessments.push(assess(
            "OK",
            "pass_rate",
            &format!("{passed}/{total} e2e tests passed"),
        ));
    } else if pass_rate >= E2E_PASS_RATE_WARN {
        assessments.push(assess(
            "WARN",
            "pass_rate",
            &format!(
                "{passed}/{total} e2e tests passed ({:.1}%)",
                pass_rate * 100.0
            ),
        ));
    } else {
        assessments.push(assess(
            "FAIL",
            "pass_rate",
            &format!(
                "{passed}/{total} e2e tests passed ({:.1}%) — below {:.0}% threshold",
                pass_rate * 100.0,
                E2E_PASS_RATE_WARN * 100.0
            ),
        ));
    }

    Some(serde_json::json!({
        "total": total,
        "passed": passed,
        "pass_rate": pass_rate,
        "assessments": assessments,
    }))
}

fn build_eval_digest(eval_dir: &std::path::Path) -> Option<serde_json::Value> {
    let content = std::fs::read_to_string(eval_dir.join("eval_results.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    let mut best_confidence: f64 = 0.0;
    let mut best_confidence_vertical = String::new();
    let mut all_recalls_zero = true;

    // Primary source: verticals[].queries[].recall and doc_scores[].confidence
    if let Some(verticals) = json["verticals"].as_array() {
        for v in verticals {
            let v_name = v["name"].as_str().unwrap_or("unknown");
            if let Some(queries) = v["queries"].as_array() {
                for q in queries {
                    let recall = as_f64_flexible(&q["recall"]).unwrap_or(0.0);
                    if recall > 0.0 {
                        all_recalls_zero = false;
                    }
                    if let Some(doc_scores) = q["doc_scores"].as_array() {
                        for ds in doc_scores {
                            let conf = as_f64_flexible(&ds["confidence"]).unwrap_or(0.0);
                            if conf > best_confidence {
                                best_confidence = conf;
                                best_confidence_vertical = v_name.to_string();
                            }
                        }
                    }
                }
            }
        }
    }

    // Secondary recall source: strategy_matrix (dict of {strategy: {vertical: {recall: "1.000"}}})
    if all_recalls_zero {
        if let Some(strategies) = json["strategy_matrix"].as_object() {
            for (_strategy, verticals) in strategies {
                if let Some(verts) = verticals.as_object() {
                    for (_vert, metrics) in verts {
                        let recall = as_f64_flexible(&metrics["recall"]).unwrap_or(0.0);
                        if recall > 0.0 {
                            all_recalls_zero = false;
                            break;
                        }
                    }
                }
                if !all_recalls_zero {
                    break;
                }
            }
        }
    }

    // Fallback: comparison.best_confidence (may be a string like "0.818")
    if best_confidence == 0.0 {
        if let Some(comp_conf) = as_f64_flexible(&json["comparison"]["best_confidence"]) {
            if comp_conf > best_confidence {
                best_confidence = comp_conf;
                best_confidence_vertical = json["comparison"]["best_confidence_vertical"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string();
            }
        }
    }

    // Check if all invocations failed (CLI errors, not just zero recall)
    let all_invocations_failed = json["verticals"]
        .as_array()
        .map(|vs| {
            vs.iter()
                .all(|v| v["successful_queries"].as_u64().unwrap_or(0) == 0)
        })
        .unwrap_or(false);

    let mut assessments = Vec::new();
    if all_invocations_failed {
        assessments.push(assess(
            "FAIL",
            "invocations",
            "All Claude CLI invocations failed — check stderr in eval output.txt",
        ));
    }
    if all_recalls_zero {
        assessments.push(assess(
            "WARN",
            "recall",
            "All recalls are 0.0 — expected documents not matched",
        ));
    }
    if best_confidence < 0.5 {
        assessments.push(assess(
            "WARN",
            "confidence",
            &format!("Best confidence {best_confidence:.2} is below 0.5"),
        ));
    } else {
        assessments.push(assess(
            "OK",
            "confidence",
            &format!("Best confidence {best_confidence:.2} in {best_confidence_vertical}"),
        ));
    }

    Some(serde_json::json!({
        "best_confidence": best_confidence,
        "best_confidence_vertical": best_confidence_vertical,
        "assessments": assessments,
    }))
}

fn build_profile_digest(stage_dir: &std::path::Path) -> Option<serde_json::Value> {
    let content = std::fs::read_to_string(stage_dir.join("profile_results.json")).ok()?;
    let pd: serde_json::Value = serde_json::from_str(&content).ok()?;

    let summary = &pd["process_metrics"]["summary"];
    let peak_rss_bytes = summary["peak_rss_bytes"].as_u64().unwrap_or(0);
    let final_rss_bytes = summary["final_rss_bytes"]
        .as_u64()
        .or_else(|| {
            // Fall back to last snapshot if final_rss_bytes not present
            pd["process_metrics"]["snapshots"]
                .as_array()
                .and_then(|s| s.last())
                .and_then(|s| s["rss_bytes"].as_u64())
        })
        .unwrap_or(0);
    let cpu_percent = summary["cpu_percent"].as_f64().unwrap_or(0.0);
    let peak_rss_mb = peak_rss_bytes as f64 / 1_048_576.0;
    let final_rss_mb = final_rss_bytes as f64 / 1_048_576.0;

    // RSS trend from snapshots
    let snapshots = pd["process_metrics"]["snapshots"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let (rss_trend, rss_growth_pct) = compute_rss_trend(&snapshots);

    // CPU top functions
    let cpu_top: Vec<serde_json::Value> = pd["cpu"]["top_functions"]
        .as_array()
        .map(|fns| {
            fns.iter()
                .take(10)
                .map(|f| {
                    let raw_name = f["name"].as_str().unwrap_or("unknown");
                    let pct = f["percent"].as_f64().unwrap_or(0.0);
                    let (name, source) = classify_cpu_function(raw_name);
                    serde_json::json!({ "name": name, "source": source, "percent": pct })
                })
                .collect()
        })
        .unwrap_or_default();

    let mut assessments = Vec::new();

    // RSS growth assessment
    if rss_growth_pct > RSS_GROWTH_FAIL_PCT {
        assessments.push(assess(
            "FAIL",
            "rss_trend",
            &format!(
                "RSS grew {rss_growth_pct:.0}% — exceeds {RSS_GROWTH_FAIL_PCT:.0}% threshold, possible memory leak"
            ),
        ));
    } else if rss_growth_pct > RSS_GROWTH_WARN_PCT {
        let trend_desc = if rss_trend == "increasing" {
            ", monotonically increasing"
        } else {
            ""
        };
        let first_rss_mb = snapshots
            .first()
            .and_then(|s| s["rss_bytes"].as_u64())
            .map(|v| v as f64 / 1_048_576.0)
            .unwrap_or(0.0);
        assessments.push(assess(
            "WARN",
            "rss_trend",
            &format!(
                "RSS grew {rss_growth_pct:.0}% ({first_rss_mb:.0}MB -> {peak_rss_mb:.0}MB){trend_desc}"
            ),
        ));
    } else {
        assessments.push(assess(
            "OK",
            "rss_trend",
            &format!("RSS stable (growth {rss_growth_pct:.0}%)"),
        ));
    }

    // Hardware counter interpretation
    let hw = &pd["hardware_counters"];
    let ipc = hw.get("ipc").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let cache_miss_pct = hw
        .get("cache_miss_percent")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let branch_misses = hw
        .get("branch_misses")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let page_faults = hw.get("page_faults").and_then(|v| v.as_u64()).unwrap_or(0);

    let bottleneck = if ipc < 0.1 && cache_miss_pct > 20.0 {
        "memory-bound"
    } else if ipc < 0.5 && cache_miss_pct > 10.0 {
        "likely-memory-bound"
    } else if ipc > 2.0 {
        "compute-efficient"
    } else {
        "balanced"
    };

    let hardware_counters = serde_json::json!({
        "ipc": ipc,
        "cache_miss_percent": cache_miss_pct,
        "branch_misses": branch_misses,
        "page_faults": page_faults,
        "bottleneck": bottleneck,
    });

    // Hardware counter assessments
    if bottleneck == "memory-bound" {
        assessments.push(assess(
            "WARN",
            "hw_bottleneck",
            &format!(
                "Memory-bound: IPC={ipc:.2}, cache miss={cache_miss_pct:.1}% — \
                 reduce working set or improve data locality"
            ),
        ));
    } else if bottleneck == "likely-memory-bound" {
        assessments.push(assess(
            "WARN",
            "hw_bottleneck",
            &format!(
                "Likely memory-bound: IPC={ipc:.2}, cache miss={cache_miss_pct:.1}% — \
                 consider cache-friendlier data structures"
            ),
        ));
    }
    if page_faults > 100_000 {
        assessments.push(assess(
            "WARN",
            "hw_page_faults",
            &format!("High page fault count: {page_faults} — possible excessive mmap/munmap churn"),
        ));
    }

    // Context switch deltas from first/last snapshots
    let context_switches = if snapshots.len() >= 2 {
        let first = &snapshots[0];
        let last = snapshots.last().unwrap();
        let vol_first = first["voluntary_ctxt_switches"].as_u64().unwrap_or(0);
        let vol_last = last["voluntary_ctxt_switches"].as_u64().unwrap_or(0);
        let nvol_first = first["nonvoluntary_ctxt_switches"].as_u64().unwrap_or(0);
        let nvol_last = last["nonvoluntary_ctxt_switches"].as_u64().unwrap_or(0);
        serde_json::json!({
            "voluntary_delta": vol_last.saturating_sub(vol_first),
            "nonvoluntary_delta": nvol_last.saturating_sub(nvol_first),
        })
    } else {
        serde_json::json!({})
    };

    // Context switch assessment
    let nvol_delta = context_switches["nonvoluntary_delta"].as_u64().unwrap_or(0);
    if nvol_delta > 50_000 {
        assessments.push(assess(
            "WARN",
            "context_switches",
            &format!(
                "High nonvoluntary context switches: {nvol_delta} — excessive preemption, \
                 consider reducing thread count or pinning cores"
            ),
        ));
    }

    // Lock contention assessment
    let lock = &pd["lock_contention"];
    let futex_avg_wait_us = lock
        .get("avg_wait_us")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let futex_total_wait_us = lock
        .get("total_wait_us")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if futex_avg_wait_us > 1000.0 {
        assessments.push(assess(
            "WARN",
            "lock_avg_wait",
            &format!(
                "High average futex wait: {:.1}ms — meaningful lock contention for parking_lot mutexes",
                futex_avg_wait_us / 1000.0
            ),
        ));
    }
    if futex_total_wait_us > 5_000_000 {
        assessments.push(assess(
            "WARN",
            "lock_total_wait",
            &format!(
                "High total futex wait: {:.1}s — threads spending significant time blocked on locks",
                futex_total_wait_us as f64 / 1_000_000.0
            ),
        ));
    }

    // Epoll reactor efficiency assessment
    let epoll = &pd["syscalls"]["epoll_wait"];
    let epoll_count = epoll.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
    let epoll_avg_events = epoll
        .get("avg_events_per_call")
        .and_then(|v| v.as_u64())
        .unwrap_or(1);
    if epoll_count > 1000 && epoll_avg_events == 0 {
        assessments.push(assess(
            "WARN",
            "epoll_spinning",
            &format!(
                "Epoll reactor spinning: {epoll_count} epoll_wait calls with 0 avg events/call — \
                 reactor waking without work"
            ),
        ));
    }

    // Connection lifecycle assessment
    let net_conn = &pd["net_connections"];
    let net_accept = net_conn
        .get("accept_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let net_connect = net_conn
        .get("connect_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let net_connect_avg_lat = net_conn
        .get("connect_avg_latency_us")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if net_accept > 0 && net_connect > net_accept * 5 {
        assessments.push(assess(
            "WARN",
            "connection_reuse",
            &format!(
                "Low connection reuse: {net_connect} outbound connects vs {net_accept} inbound accepts — \
                 connection pool may be churning"
            ),
        ));
    }
    if net_connect_avg_lat > 5000 {
        assessments.push(assess(
            "WARN",
            "connect_latency",
            &format!(
                "Slow upstream connection establishment: avg {:.1}ms",
                net_connect_avg_lat as f64 / 1000.0
            ),
        ));
    }

    // Memory leak forensics cross-check (RSS trend + DHAT bytes_at_exit)
    let dhat_bytes_at_exit = {
        // Check for dhat-heap.json in same directory
        let dhat_path = stage_dir.join("dhat-heap.json");
        if dhat_path.exists() {
            std::fs::read_to_string(&dhat_path)
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .and_then(|d| d["bytes_at_exit"].as_u64())
                .unwrap_or(0)
        } else {
            0
        }
    };

    let rss_monotonic_growing = rss_trend == "increasing" && rss_growth_pct > 20.0;
    let allocator_leaking = dhat_bytes_at_exit > 0;

    let leak_confidence = if rss_monotonic_growing && allocator_leaking {
        "confirmed"
    } else if rss_monotonic_growing {
        "suspected"
    } else {
        "unlikely"
    };

    if leak_confidence == "confirmed" {
        assessments.push(assess(
            "FAIL",
            "leak_confidence",
            &format!(
                "Memory leak confirmed: RSS monotonically increasing +{rss_growth_pct:.0}%, \
                 dhat_bytes_at_exit={dhat_bytes_at_exit}"
            ),
        ));
    } else if leak_confidence == "suspected" {
        assessments.push(assess(
            "WARN",
            "leak_confidence",
            &format!(
                "Memory leak suspected: RSS monotonically increasing +{rss_growth_pct:.0}%, \
                 but DHAT shows no unreleased bytes"
            ),
        ));
    }

    // cgroup v2 resource limit assessments
    let cgroup = &pd["cgroup"];
    let mut cgroup_digest = serde_json::Value::Null;
    if cgroup.is_object() && cgroup["enforced"].as_bool() == Some(true) {
        let oom_kill = cgroup["memory"]["oom_kill_events"].as_u64().unwrap_or(0);
        let mem_util = cgroup["memory"]["utilization_percent"]
            .as_f64()
            .unwrap_or(0.0);
        let throttle_pct = cgroup["cpu"]["throttle_percent"].as_f64().unwrap_or(0.0);

        if oom_kill > 0 {
            assessments.push(assess(
                "FAIL",
                "cgroup_oom",
                &format!(
                    "OOM kill under cgroup limits: {oom_kill} kill event(s) — profile memory limit too low"
                ),
            ));
        }
        if mem_util > 90.0 {
            assessments.push(assess(
                "WARN",
                "cgroup_memory",
                &format!("Memory utilization {mem_util:.1}% of cgroup limit — close to OOM"),
            ));
        }
        if throttle_pct > 25.0 {
            assessments.push(assess(
                "WARN",
                "cgroup_cpu_throttle",
                &format!(
                    "CPU throttled {throttle_pct:.1}% of periods — profile CPU limit constraining"
                ),
            ));
        }

        cgroup_digest = serde_json::json!({
            "profile_name": cgroup["profile"]["name"],
            "memory_utilization_pct": mem_util,
            "cpu_throttle_pct": throttle_pct,
            "oom_kill_events": oom_kill,
        });
    }

    let mut result = serde_json::json!({
        "peak_rss_mb": (peak_rss_mb * 10.0).round() / 10.0,
        "final_rss_mb": (final_rss_mb * 10.0).round() / 10.0,
        "cpu_percent": (cpu_percent * 10.0).round() / 10.0,
        "rss_trend": rss_trend,
        "rss_growth_pct": (rss_growth_pct * 10.0).round() / 10.0,
        "cpu_top_functions": cpu_top,
        "hardware_counters": hardware_counters,
        "context_switches": context_switches,
        "leak_confidence": leak_confidence,
        "dhat_bytes_at_exit": dhat_bytes_at_exit,
        "assessments": assessments,
    });
    if !cgroup_digest.is_null() {
        result["cgroup"] = cgroup_digest;
    }
    Some(result)
}

/// Extract a string field value from a structured log line.
/// Looks for `prefix=value` or `prefix="value"` patterns.
fn extract_field<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let needle = format!("{prefix}=");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    if let Some(stripped) = rest.strip_prefix('"') {
        let end = stripped.find('"')?;
        Some(&stripped[..end])
    } else {
        let end = rest.find(' ').unwrap_or(rest.len());
        Some(&rest[..end])
    }
}

/// Extract a numeric field from a structured log line.
fn extract_numeric_field(line: &str, prefix: &str) -> Option<f64> {
    extract_field(line, prefix).and_then(|s| s.parse::<f64>().ok())
}

fn build_coverage_digest(coverage_dir: &std::path::Path) -> Option<serde_json::Value> {
    let content = std::fs::read_to_string(coverage_dir.join("tarpaulin-report.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    let overall_coverage = json["coverage"].as_f64().unwrap_or(0.0);
    let overall_coverable = json["coverable"].as_u64().unwrap_or(0);
    let overall_covered = json["covered"].as_u64().unwrap_or(0);

    // Group files by module (src/XXX/) and compute per-module coverage
    let mut module_coverable: std::collections::HashMap<String, u64> =
        std::collections::HashMap::new();
    let mut module_covered: std::collections::HashMap<String, u64> =
        std::collections::HashMap::new();

    if let Some(files) = json["files"].as_array() {
        for file in files {
            let path_segments: Vec<&str> = file["path"]
                .as_array()
                .map(|arr| arr.iter().filter_map(|s| s.as_str()).collect())
                .unwrap_or_default();
            let full_path = path_segments.join("/");

            // Extract module name: look for "src/XXX/" pattern
            let module = if let Some(src_idx) = path_segments.iter().position(|&s| s == "src") {
                if src_idx + 1 < path_segments.len() {
                    let next = path_segments[src_idx + 1];
                    // If next segment is a .rs file, use "src/root"
                    if next.ends_with(".rs") {
                        "src/root".to_string()
                    } else {
                        format!("src/{next}")
                    }
                } else {
                    "other".to_string()
                }
            } else if full_path.contains("benches") {
                "benches".to_string()
            } else if full_path.contains("tests") {
                "tests".to_string()
            } else {
                "other".to_string()
            };

            let coverable = file["coverable"].as_u64().unwrap_or(0);
            let covered = file["covered"].as_u64().unwrap_or(0);
            *module_coverable.entry(module.clone()).or_insert(0) += coverable;
            *module_covered.entry(module).or_insert(0) += covered;
        }
    }

    // Build per-module coverage list
    let critical_modules = ["src/proxy", "src/cache"];
    let mut modules = Vec::new();
    let mut assessments = Vec::new();

    let mut module_names: Vec<&String> = module_coverable.keys().collect();
    module_names.sort();

    for module in &module_names {
        let coverable = module_coverable[*module];
        let covered = module_covered.get(*module).copied().unwrap_or(0);
        let pct = if coverable > 0 {
            (covered as f64 / coverable as f64) * 100.0
        } else {
            0.0
        };

        modules.push(serde_json::json!({
            "module": module,
            "coverable": coverable,
            "covered": covered,
            "coverage_pct": (pct * 10.0).round() / 10.0,
        }));

        // Flag critical modules below 70%
        if critical_modules.contains(&module.as_str()) && pct < 70.0 && coverable > 0 {
            assessments.push(assess(
                "WARN",
                "low_coverage",
                &format!("{module} coverage {pct:.1}% — critical module below 70% target"),
            ));
        }
    }

    Some(serde_json::json!({
        "overall_coverage_pct": (overall_coverage * 10.0).round() / 10.0,
        "overall_coverable": overall_coverable,
        "overall_covered": overall_covered,
        "modules": modules,
        "assessments": assessments,
    }))
}

fn build_cascade_digest(load_dir: &std::path::Path) -> Option<serde_json::Value> {
    let log_content = std::fs::read_to_string(load_dir.join("proxy_logs.txt")).ok()?;

    let mut total_cascades: u64 = 0;
    let mut stop_reasons: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut depths: Vec<u64> = Vec::new();
    let mut times_ms: Vec<f64> = Vec::new();
    let mut upstream_queries: u64 = 0;
    let mut upstream_met_threshold: u64 = 0;

    for line in log_content.lines() {
        if line.contains("Cascade complete") {
            total_cascades += 1;
            if let Some(reason) = extract_field(line, "stop_reason") {
                *stop_reasons.entry(reason.to_string()).or_insert(0) += 1;
            }
            if let Some(depth) = extract_numeric_field(line, "cascade_depth") {
                depths.push(depth as u64);
            }
            if let Some(time) = extract_numeric_field(line, "cascade_time_ms") {
                times_ms.push(time);
            }
        } else if line.contains("Cascade upstream queried") {
            upstream_queries += 1;
            if extract_field(line, "met_threshold") == Some("true") {
                upstream_met_threshold += 1;
            }
        }
    }

    if total_cascades == 0 {
        return None;
    }

    // Timing stats
    times_ms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let avg_time = times_ms.iter().sum::<f64>() / times_ms.len().max(1) as f64;
    let p99_time = if !times_ms.is_empty() {
        let idx = ((times_ms.len() as f64) * 0.99).ceil() as usize;
        times_ms[idx.min(times_ms.len() - 1)]
    } else {
        0.0
    };

    // Depth stats
    let avg_depth = if !depths.is_empty() {
        depths.iter().sum::<u64>() as f64 / depths.len() as f64
    } else {
        0.0
    };
    let max_depth = depths.iter().max().copied().unwrap_or(0);

    // Stop reason distribution
    let all_exhausted = stop_reasons.get("all_exhausted").copied().unwrap_or(0);
    let threshold_met = stop_reasons.get("threshold_met").copied().unwrap_or(0);
    let all_exhausted_pct = (all_exhausted as f64 / total_cascades as f64) * 100.0;
    let success_rate = (threshold_met as f64 / total_cascades as f64) * 100.0;

    let mut assessments = Vec::new();
    if all_exhausted_pct > 50.0 {
        assessments.push(assess(
            "WARN",
            "cascade_exhaustion",
            &format!(
                "{all_exhausted_pct:.0}% of cascades exhausted all upstreams — \
                 threshold may be too high or upstreams returning low scores"
            ),
        ));
    }
    if p99_time > 100.0 {
        assessments.push(assess(
            "WARN",
            "cascade_latency",
            &format!("Cascade P99 latency {p99_time:.0}ms — adds to read-path latency"),
        ));
    }

    Some(serde_json::json!({
        "total_cascades": total_cascades,
        "stop_reasons": stop_reasons,
        "all_exhausted_pct": (all_exhausted_pct * 10.0).round() / 10.0,
        "success_rate_pct": (success_rate * 10.0).round() / 10.0,
        "timing": {
            "avg_ms": (avg_time * 10.0).round() / 10.0,
            "p99_ms": (p99_time * 10.0).round() / 10.0,
        },
        "depth": {
            "avg": (avg_depth * 10.0).round() / 10.0,
            "max": max_depth,
        },
        "upstream_queries": upstream_queries,
        "upstream_met_threshold": upstream_met_threshold,
        "assessments": assessments,
    }))
}

// ---------------------------------------------------------------------------
// audit digest v2 builders — parse raw/unstructured files into structured JSON
// ---------------------------------------------------------------------------

/// Parse all bpftrace raw files + perf_stat for a given stage directory.
fn build_bpftrace_digest(stage_dir: &std::path::Path) -> Option<serde_json::Value> {
    let mut result = serde_json::Map::new();

    // --- lock_raw.txt ---
    if let Ok(content) = std::fs::read_to_string(stage_dir.join("lock_raw.txt")) {
        let mut lock = serde_json::Map::new();
        let mut in_data = false;
        let mut distribution = Vec::new();
        let mut top_sites: Vec<serde_json::Value> = Vec::new();
        let mut current_stack_addrs: Vec<String> = Vec::new();
        let mut in_by_count = false;
        let mut in_by_wait = false;

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("========") {
                in_data = true;
                continue;
            }
            if !in_data {
                continue;
            }

            // Parse totals
            if let Some(rest) = trimmed.strip_prefix("futex_wait count:") {
                if let Ok(v) = rest.trim().parse::<u64>() {
                    lock.insert("futex_wait_count".into(), serde_json::json!(v));
                }
            } else if let Some(rest) = trimmed.strip_prefix("futex_wake count:") {
                if let Ok(v) = rest.trim().parse::<u64>() {
                    lock.insert("futex_wake_count".into(), serde_json::json!(v));
                }
            } else if let Some(rest) = trimmed.strip_prefix("total wait us:") {
                if let Ok(v) = rest.trim().parse::<u64>() {
                    lock.insert("total_wait_us".into(), serde_json::json!(v));
                }
            } else if let Some(rest) = trimmed.strip_prefix("avg wait us:") {
                if let Ok(v) = rest.trim().parse::<u64>() {
                    lock.insert("avg_wait_us".into(), serde_json::json!(v));
                }
            }

            // Parse histogram lines: [range)  count |bars|
            if trimmed.starts_with('[') && trimmed.contains('|') {
                if let Some((range_part, rest)) = trimmed.split_once(')') {
                    let range = format!("{})", range_part);
                    let count_str = rest.split('|').next().unwrap_or("").trim();
                    if let Ok(count) = count_str.parse::<u64>() {
                        if in_by_count || in_by_wait {
                            // Skip histograms in stack sections
                        } else {
                            distribution.push(serde_json::json!({
                                "range": range,
                                "count": count,
                            }));
                        }
                    }
                }
            }

            // Track section headers for top sites
            if trimmed.contains("Top 15 Contended Lock Sites (by count)") {
                in_by_count = true;
                in_by_wait = false;
            } else if trimmed.contains("Top 15 Contended Lock Sites (by total wait") {
                in_by_count = false;
                in_by_wait = true;
            }

            // Parse stack blocks: @futex_wait_by_stack[  then addresses then ]: count
            if trimmed.starts_with("@futex_wait_by_stack[")
                || trimmed.starts_with("@futex_wait_lat_by_stack[")
            {
                current_stack_addrs.clear();
            } else if trimmed.starts_with("0x") && (in_by_count || in_by_wait) {
                current_stack_addrs.push(trimmed.to_string());
            } else if trimmed.starts_with("]:") && (in_by_count) {
                if let Ok(count) = trimmed[2..].trim().parse::<u64>() {
                    let addr = current_stack_addrs.first().cloned().unwrap_or_default();
                    top_sites.push(serde_json::json!({
                        "address": addr,
                        "count": count,
                    }));
                }
                current_stack_addrs.clear();
            }
        }

        if !distribution.is_empty() {
            lock.insert("distribution".into(), serde_json::json!(distribution));
        }
        if !top_sites.is_empty() {
            lock.insert("top_sites".into(), serde_json::json!(top_sites));
        }
        if !lock.is_empty() {
            result.insert("lock".into(), serde_json::Value::Object(lock));
        }
    }

    // --- syscall_raw.txt ---
    if let Ok(content) = std::fs::read_to_string(stage_dir.join("syscall_raw.txt")) {
        let mut syscall = serde_json::Map::new();
        let mut in_data = false;
        let mut current_section = String::new();
        let mut section_data = serde_json::Map::new();

        for line in content.lines() {
            let trimmed = line.trim();

            // Skip bpftrace warnings
            if trimmed.contains(".bt:") && trimmed.contains("WARNING:") {
                continue;
            }

            if trimmed.starts_with("========") {
                in_data = true;
                continue;
            }
            if !in_data {
                continue;
            }

            // Section headers: --- name ---
            if trimmed.starts_with("--- ") && trimmed.ends_with(" ---") {
                // Save previous section
                if !current_section.is_empty() && !section_data.is_empty() {
                    syscall.insert(
                        current_section.clone(),
                        serde_json::Value::Object(section_data.clone()),
                    );
                }
                current_section = trimmed
                    .trim_start_matches("--- ")
                    .trim_end_matches(" ---")
                    .to_string();
                section_data = serde_json::Map::new();
                continue;
            }

            if current_section.is_empty() {
                continue;
            }

            // Parse key: value lines
            if let Some((key, val)) = trimmed.split_once(':') {
                let key = key.trim();
                let val = val.trim();
                match key {
                    "count" => {
                        if let Ok(v) = val.parse::<u64>() {
                            section_data.insert("count".into(), serde_json::json!(v));
                        }
                    }
                    "total bytes" => {
                        if let Ok(v) = val.parse::<u64>() {
                            section_data.insert("total_bytes".into(), serde_json::json!(v));
                        }
                    }
                    "avg latency us" => {
                        if let Ok(v) = val.parse::<u64>() {
                            section_data.insert("avg_latency_us".into(), serde_json::json!(v));
                        }
                    }
                    "total events" => {
                        if let Ok(v) = val.parse::<u64>() {
                            section_data.insert("total_events".into(), serde_json::json!(v));
                        }
                    }
                    "avg events/call" => {
                        if let Ok(v) = val.parse::<u64>() {
                            section_data.insert("avg_events_per_call".into(), serde_json::json!(v));
                        }
                    }
                    _ => {} // Skip histogram headers, advice breakdowns, etc.
                }
            }
        }
        // Save last section
        if !current_section.is_empty() && !section_data.is_empty() {
            syscall.insert(current_section, serde_json::Value::Object(section_data));
        }
        if !syscall.is_empty() {
            result.insert("syscall".into(), serde_json::Value::Object(syscall));
        }
    }

    // --- net_raw.txt ---
    if let Ok(content) = std::fs::read_to_string(stage_dir.join("net_raw.txt")) {
        let mut net = serde_json::Map::new();
        let mut in_data = false;
        let mut current_section = String::new();

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("========") {
                in_data = true;
                continue;
            }
            if !in_data {
                continue;
            }

            if trimmed.starts_with("--- ") && trimmed.ends_with(" ---") {
                current_section = trimmed
                    .trim_start_matches("--- ")
                    .trim_end_matches(" ---")
                    .to_string();
                continue;
            }

            if let Some((key, val)) = trimmed.split_once(':') {
                let key = key.trim();
                let val = val.trim();
                match (current_section.as_str(), key) {
                    ("accept4", "count") => {
                        if let Ok(v) = val.parse::<u64>() {
                            net.insert("accept_count".into(), serde_json::json!(v));
                        }
                    }
                    ("accept4", "avg latency us") => {
                        if let Ok(v) = val.parse::<u64>() {
                            net.insert("accept_avg_latency_us".into(), serde_json::json!(v));
                        }
                    }
                    ("connect", "count") => {
                        if let Ok(v) = val.parse::<u64>() {
                            net.insert("connect_count".into(), serde_json::json!(v));
                        }
                    }
                    ("connect", "avg latency us") => {
                        if let Ok(v) = val.parse::<u64>() {
                            net.insert("connect_avg_latency_us".into(), serde_json::json!(v));
                        }
                    }
                    ("close", "count") => {
                        if let Ok(v) = val.parse::<u64>() {
                            net.insert("close_count".into(), serde_json::json!(v));
                        }
                    }
                    ("close", "close/accept") => {
                        if let Ok(v) = val.parse::<u64>() {
                            net.insert("close_accept_ratio".into(), serde_json::json!(v));
                        }
                    }
                    _ => {}
                }
            }
        }
        if !net.is_empty() {
            result.insert("net".into(), serde_json::Value::Object(net));
        }
    }

    // --- perf_stat_raw.txt ---
    if let Ok(content) = std::fs::read_to_string(stage_dir.join("perf_stat_raw.txt")) {
        let mut perf_stat = serde_json::Map::new();

        for line in content.lines() {
            let trimmed = line.trim();

            // Parse counter lines: "1,234,567  counter-name:u  (83.55%)"
            // or "1,234,567  counter-name:u"
            if trimmed.is_empty() || trimmed.starts_with("Performance counter") {
                continue;
            }

            // Extract "seconds time elapsed"
            if trimmed.contains("seconds time elapsed") {
                let secs_str = trimmed.split_whitespace().next().unwrap_or("");
                if let Ok(secs) = secs_str.parse::<f64>() {
                    perf_stat.insert(
                        "elapsed_secs".into(),
                        serde_json::json!((secs * 100.0).round() / 100.0),
                    );
                }
                continue;
            }

            // Parse counter lines
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 2 {
                // First part is comma-separated number
                let num_str = parts[0].replace(',', "");
                if let Ok(count) = num_str.parse::<u64>() {
                    let counter_name = parts[1];
                    // Strip :u suffix
                    let name = counter_name.trim_end_matches(":u");
                    let key = match name {
                        "cycles" => "cycles",
                        "instructions" => "instructions",
                        "cache-misses" => "cache_misses",
                        "cache-references" => "cache_references",
                        "branch-misses" => "branch_misses",
                        "page-faults" => "page_faults",
                        _ => continue,
                    };
                    perf_stat.insert(key.into(), serde_json::json!(count));
                }
            }
        }

        if !perf_stat.is_empty() {
            result.insert("perf_stat".into(), serde_json::Value::Object(perf_stat));
        }
    }

    if result.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(result))
    }
}

/// Parse proxy_logs.txt and extract operational/security summary.
fn build_proxy_logs_digest(stage_dir: &std::path::Path) -> Option<serde_json::Value> {
    let content = std::fs::read_to_string(stage_dir.join("proxy_logs.txt")).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    let mut sysctl_warnings = Vec::new();
    let mut error_lines = Vec::new();
    let mut warn_lines = Vec::new();
    let mut cascade_events_count: u64 = 0;
    let mut unique_upstreams: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();

    for line in &lines {
        if line.contains("sysctl") || line.contains("recommended:") {
            sysctl_warnings.push(line.trim().to_string());
        }
        if line.contains(" ERROR ") {
            error_lines.push(line.trim().to_string());
        } else if line.contains(" WARN ") {
            warn_lines.push(line.trim().to_string());
        }
        if line.contains("Cascade complete") {
            cascade_events_count += 1;
        }
        if line.contains("upstream_id=") {
            if let Some(id) = extract_field(line, "upstream_id") {
                unique_upstreams.insert(id.to_string());
            }
        }
    }

    Some(serde_json::json!({
        "total_lines": total_lines,
        "sysctl_warnings": sysctl_warnings,
        "error_lines": error_lines,
        "warn_lines": warn_lines,
        "cascade_events_count": cascade_events_count,
        "unique_upstream_ids": unique_upstreams.into_iter().collect::<Vec<_>>(),
    }))
}

/// Extract security-relevant facts from test results.
fn build_security_digest(dir: &std::path::Path) -> Option<serde_json::Value> {
    let mut result = serde_json::Map::new();

    // Upstream protocols: gather upstream IDs from proxy logs and check proxy_stats for URLs
    let mut upstream_protocols = Vec::new();
    for &stage in &["load", "e2e"] {
        let stage_dir = dir.join(stage);
        if let Ok(log_content) = std::fs::read_to_string(stage_dir.join("proxy_logs.txt")) {
            for line in log_content.lines() {
                if line.contains("upstream_id=") {
                    if let Some(id) = extract_field(line, "upstream_id") {
                        let id_str = id.to_string();
                        // Avoid duplicates
                        if !upstream_protocols
                            .iter()
                            .any(|p: &serde_json::Value| p["id"].as_str() == Some(&id_str))
                        {
                            // Infer scheme from upstream type/id — in real data, upstreams
                            // are configured in config, so we mark as unknown unless detectable
                            let is_localhost = id_str.contains("localhost")
                                || id_str.contains("127.0.0.1")
                                || id_str.contains("local");
                            upstream_protocols.push(serde_json::json!({
                                "id": id_str,
                                "scheme": "unknown",
                                "is_localhost": is_localhost,
                            }));
                        }
                    }
                }
            }
        }

        // Try to extract upstream URLs from proxy_stats.json
        if let Ok(stats_content) = std::fs::read_to_string(stage_dir.join("proxy_stats.json")) {
            if let Ok(stats) = serde_json::from_str::<serde_json::Value>(&stats_content) {
                if let Some(upstreams) = stats["upstreams"].as_array() {
                    for u in upstreams {
                        if let Some(url) = u["url"].as_str() {
                            let id = u["id"].as_str().unwrap_or("unknown");
                            let scheme = if url.starts_with("https") {
                                "https"
                            } else if url.starts_with("http") {
                                "http"
                            } else {
                                "unknown"
                            };
                            let is_localhost = url.contains("localhost")
                                || url.contains("127.0.0.1")
                                || url.contains("0.0.0.0");
                            // Update existing entry or add
                            let mut found = false;
                            for p in upstream_protocols.iter_mut() {
                                if p["id"].as_str() == Some(id) {
                                    p["scheme"] = serde_json::json!(scheme);
                                    p["is_localhost"] = serde_json::json!(is_localhost);
                                    found = true;
                                    break;
                                }
                            }
                            if !found {
                                upstream_protocols.push(serde_json::json!({
                                    "id": id,
                                    "scheme": scheme,
                                    "is_localhost": is_localhost,
                                }));
                            }
                        }
                    }
                }
            }
        }
    }

    if !upstream_protocols.is_empty() {
        result.insert(
            "upstream_protocols".into(),
            serde_json::json!(upstream_protocols),
        );
    }

    // E2E security test coverage
    let e2e_results_path = dir.join("e2e").join("results.json");
    if let Ok(content) = std::fs::read_to_string(&e2e_results_path) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
            let total = json["summary"]["total"].as_u64().unwrap_or(0);
            let mut auth_tests: u64 = 0;
            let mut rate_limit_tests: u64 = 0;
            let mut injection_tests: u64 = 0;

            if let Some(tests) = json["tests"].as_array() {
                for t in tests {
                    let name = t["name"].as_str().unwrap_or("").to_lowercase();
                    if name.contains("auth") || name.contains("tls") || name.contains("security") {
                        auth_tests += 1;
                    }
                    if name.contains("rate_limit")
                        || name.contains("rate-limit")
                        || name.contains("throttl")
                    {
                        rate_limit_tests += 1;
                    }
                    if name.contains("injection")
                        || name.contains("inject")
                        || name.contains("xss")
                        || name.contains("sqli")
                    {
                        injection_tests += 1;
                    }
                }
            }

            result.insert(
                "e2e_test_security_coverage".into(),
                serde_json::json!({
                    "auth_tests": auth_tests,
                    "rate_limit_tests": rate_limit_tests,
                    "injection_tests": injection_tests,
                    "total_tests": total,
                }),
            );
        }
    }

    if result.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(result))
    }
}

/// Summarize operational snapshots (pool, circuit, DHAT).
fn build_operational_digest(dir: &std::path::Path) -> Option<serde_json::Value> {
    let mut result = serde_json::Map::new();

    // Pool and circuit state per stage
    let mut pool = serde_json::Map::new();
    let mut circuit = serde_json::Map::new();
    let mut dhat = serde_json::Map::new();

    for &stage in &["e2e", "load"] {
        let stage_dir = dir.join(stage);
        if !stage_dir.is_dir() {
            continue;
        }

        // pool.json
        if let Ok(content) = std::fs::read_to_string(stage_dir.join("pool.json")) {
            let val = match serde_json::from_str::<serde_json::Value>(&content) {
                Ok(v) => {
                    if v.as_object().is_none_or(|m| m.is_empty())
                        && v.as_array().is_none_or(|a| a.is_empty())
                    {
                        serde_json::json!("empty")
                    } else {
                        serde_json::json!("populated")
                    }
                }
                Err(_) => serde_json::json!("invalid"),
            };
            pool.insert(stage.into(), val);
        }

        // circuit.json
        if let Ok(content) = std::fs::read_to_string(stage_dir.join("circuit.json")) {
            let val = match serde_json::from_str::<serde_json::Value>(&content) {
                Ok(v) => {
                    if v.as_object().is_none_or(|m| m.is_empty())
                        && v.as_array().is_none_or(|a| a.is_empty())
                    {
                        serde_json::json!("empty")
                    } else {
                        serde_json::json!("populated")
                    }
                }
                Err(_) => serde_json::json!("invalid"),
            };
            circuit.insert(stage.into(), val);
        }

        // dhat-heap.json — read only first ~500 bytes to extract top-level fields
        let dhat_path = stage_dir.join("dhat-heap.json");
        if dhat_path.exists() {
            // Read limited bytes to avoid pulling in 57K+ file
            match std::fs::File::open(&dhat_path) {
                Ok(file) => {
                    use std::io::Read;
                    let mut reader = std::io::BufReader::new(file);
                    let mut buf = vec![0u8; 1024];
                    let n = reader.read(&mut buf).unwrap_or(0);
                    let header = String::from_utf8_lossy(&buf[..n]);

                    let mut dhat_entry = serde_json::Map::new();
                    dhat_entry.insert("present".into(), serde_json::json!(true));

                    // Try to parse as full JSON first (small files)
                    // For large files, extract fields from the header text
                    if let Ok(full) = std::fs::read_to_string(&dhat_path) {
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&full) {
                            if let Some(bae) = json.get("bytes-at-exit").and_then(|v| v.as_u64()) {
                                dhat_entry.insert("bytes_at_exit".into(), serde_json::json!(bae));
                            }
                            if let Some(bkt) = json.get("bk-total").and_then(|v| v.as_u64()) {
                                dhat_entry.insert("total_blocks".into(), serde_json::json!(bkt));
                            }
                        }
                    } else {
                        // Fallback: extract from header text
                        for line in header.lines() {
                            let trimmed = line.trim().trim_end_matches(',');
                            if let Some(rest) = trimmed.strip_prefix("\"bytes-at-exit\":") {
                                if let Ok(v) = rest.trim().parse::<u64>() {
                                    dhat_entry.insert("bytes_at_exit".into(), serde_json::json!(v));
                                }
                            } else if let Some(rest) = trimmed.strip_prefix("\"bk-total\":") {
                                if let Ok(v) = rest.trim().parse::<u64>() {
                                    dhat_entry.insert("total_blocks".into(), serde_json::json!(v));
                                }
                            }
                        }
                    }

                    dhat.insert(stage.into(), serde_json::Value::Object(dhat_entry));
                }
                Err(_) => {
                    dhat.insert(stage.into(), serde_json::json!({ "present": false }));
                }
            }
        } else {
            dhat.insert(stage.into(), serde_json::json!({ "present": false }));
        }
    }

    if !pool.is_empty() {
        result.insert("pool".into(), serde_json::Value::Object(pool));
    }
    if !circuit.is_empty() {
        result.insert("circuit".into(), serde_json::Value::Object(circuit));
    }
    if !dhat.is_empty() {
        result.insert("dhat".into(), serde_json::Value::Object(dhat));
    }

    if result.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(result))
    }
}

fn build_observations(digest: &serde_json::Value) -> serde_json::Value {
    let mut obs: Vec<String> = Vec::new();

    // Load test observations
    if let Some(scenarios) = digest["load"]["scenarios"].as_array() {
        for s in scenarios {
            let name = s["name"].as_str().unwrap_or("unknown");
            let error_rate = s["error_rate"].as_f64().unwrap_or(0.0);
            if error_rate > 0.9 {
                obs.push(format!(
                    "{name} returning {:.0}% errors — upstream likely not running",
                    error_rate * 100.0
                ));
            } else if error_rate > 0.0 {
                obs.push(format!("{name} has {:.1}% error rate", error_rate * 100.0));
            }
        }
    }

    // Profile observations
    if let Some(profiles) = digest["profiles"].as_object() {
        for (stage, profile) in profiles {
            let growth = profile["rss_growth_pct"].as_f64().unwrap_or(0.0);
            let trend = profile["rss_trend"].as_str().unwrap_or("stable");
            if growth > RSS_GROWTH_WARN_PCT && trend == "increasing" {
                obs.push(format!(
                    "RSS grew {growth:.0}% during {stage} with monotonic increase — investigate for memory leak"
                ));
            }
        }
    }

    // Cache observations
    if let Some(hit_rate) = digest["load"]["cache"]["hit_rate"].as_f64() {
        if hit_rate >= CACHE_HIT_OK {
            obs.push(format!(
                "Cache hit rate {:.2}% — cache working correctly",
                hit_rate * 100.0
            ));
        } else {
            obs.push(format!(
                "Cache hit rate {:.2}% — below target",
                hit_rate * 100.0
            ));
        }
    }

    // Bench observations
    if let Some(bench_assessments) = digest["bench"]["assessments"].as_array() {
        for a in bench_assessments {
            if a["level"].as_str() == Some("OK") && a["metric"].as_str() == Some("regressions") {
                obs.push(format!(
                    "No benchmark regressions (threshold: {BENCH_REGRESSION_THRESHOLD:.0}%)"
                ));
            } else if a["level"].as_str() == Some("FAIL")
                && a["metric"].as_str() == Some("regressions")
            {
                if let Some(msg) = a["message"].as_str() {
                    obs.push(msg.to_string());
                }
            }
        }
    }

    // E2E observations
    if let Some(e2e) = digest.get("e2e") {
        let total = e2e["total"].as_u64().unwrap_or(0);
        let passed = e2e["passed"].as_u64().unwrap_or(0);
        if total > 0 && passed == total {
            obs.push(format!("All {total} e2e tests passed"));
        } else if total > 0 {
            obs.push(format!("{passed}/{total} e2e tests passed"));
        }
    }

    // Eval observations
    if let Some(eval_assessments) = digest["eval"]["assessments"].as_array() {
        for a in eval_assessments {
            if a["level"].as_str() == Some("WARN") {
                if let Some(msg) = a["message"].as_str() {
                    obs.push(msg.to_string());
                }
            }
        }
    }

    serde_json::json!(obs)
}

// ---------------------------------------------------------------------------
// dashboard rendering for index.html (visually renders perf_digest.json)
// ---------------------------------------------------------------------------

/// Top-level orchestrator: render the performance dashboard.
fn render_dashboard(html: &mut String, digest: &serde_json::Value) {
    html.push_str("<div class=\"dashboard\">\n");
    render_observations_banner(html, digest);
    render_assessments_grid(html, digest);
    render_load_summary_strip(html, digest);
    render_bench_summary(html, digest);
    render_hitrate_summary(html, digest);
    render_coverage_summary(html, digest);
    render_profile_summary(html, digest);
    render_cascade_summary(html, digest);
    html.push_str("</div>\n");
}

/// Render observations as a highlighted banner.
fn render_observations_banner(html: &mut String, digest: &serde_json::Value) {
    let obs = match digest["observations"].as_array() {
        Some(arr) if !arr.is_empty() => arr,
        _ => return,
    };

    html.push_str("<div class=\"obs-banner\">\n<strong>Observations</strong>\n<ul>\n");
    for o in obs {
        if let Some(text) = o.as_str() {
            html.push_str(&format!("<li>{}</li>\n", html_escape(text)));
        }
    }
    html.push_str("</ul>\n</div>\n");
}

/// Collect and render ALL assessments from every section.
fn render_assessments_grid(html: &mut String, digest: &serde_json::Value) {
    let mut all: Vec<(&str, &str, &str, &str)> = Vec::new(); // (level, metric, message, source)

    // Helper to collect assessments from an array
    fn collect<'a>(
        arr: Option<&'a Vec<serde_json::Value>>,
        source: &'a str,
        out: &mut Vec<(&'a str, &'a str, &'a str, &'a str)>,
    ) {
        if let Some(assessments) = arr {
            for a in assessments {
                let level = a["level"].as_str().unwrap_or("INFO");
                let metric = a["metric"].as_str().unwrap_or("");
                let message = a["message"].as_str().unwrap_or("");
                out.push((level, metric, message, source));
            }
        }
    }

    // Load scenario assessments
    if let Some(scenarios) = digest["load"]["scenarios"].as_array() {
        for s in scenarios {
            let name = s["name"].as_str().unwrap_or("load");
            collect(s["assessments"].as_array(), name, &mut all);
        }
    }
    collect(
        digest["load"]["cache"]["assessments"].as_array(),
        "cache",
        &mut all,
    );
    collect(digest["bench"]["assessments"].as_array(), "bench", &mut all);
    collect(digest["e2e"]["assessments"].as_array(), "e2e", &mut all);
    collect(digest["eval"]["assessments"].as_array(), "eval", &mut all);
    collect(
        digest["cascade"]["assessments"].as_array(),
        "cascade",
        &mut all,
    );

    // Profile assessments
    if let Some(profiles) = digest["profiles"].as_object() {
        for (stage, profile) in profiles {
            let source_label: &str = match stage.as_str() {
                "load" => "profile/load",
                "e2e" => "profile/e2e",
                _ => "profile",
            };
            // Can't use collect() directly since source_label is not &'a str with same lifetime
            if let Some(assessments) = profile["assessments"].as_array() {
                for a in assessments {
                    let level = a["level"].as_str().unwrap_or("INFO");
                    let metric = a["metric"].as_str().unwrap_or("");
                    let message = a["message"].as_str().unwrap_or("");
                    all.push((level, metric, message, source_label));
                }
            }
        }
    }

    if all.is_empty() {
        return;
    }

    // Sort by severity: FAIL first, then WARN, then OK, then INFO
    all.sort_by_key(|(level, _, _, _)| match *level {
        "FAIL" => 0,
        "WARN" => 1,
        "OK" => 2,
        _ => 3,
    });

    html.push_str(
        "<div class=\"dash-section\">\n<h2>Assessments</h2>\n\
        <div style=\"display:flex;flex-wrap:wrap;gap:.6rem;\">\n",
    );
    for (level, metric, message, source) in &all {
        let border_color = match *level {
            "FAIL" => "var(--fail)",
            "WARN" => "var(--skip)",
            "OK" => "var(--pass)",
            _ => "var(--link)",
        };
        html.push_str(&format!(
            "<div class=\"assessment-card\" style=\"border-left-color:{border_color};min-width:260px;flex:1;max-width:500px;\">\
             <div><strong style=\"color:{border_color}\">{level}</strong>\
             <span style=\"color:#8b949e;margin-left:.4rem;font-size:.75rem\">{}</span>\
             <span style=\"color:#555;margin-left:.4rem;font-size:.7rem\">[{}]</span></div>\
             <div style=\"font-size:.83rem;margin-top:.15rem\">{}</div></div>\n",
            html_escape(metric),
            html_escape(source),
            html_escape(message),
        ));
    }
    html.push_str("</div>\n</div>\n");
}

/// Render load test aggregate stats as a horizontal strip.
fn render_load_summary_strip(html: &mut String, digest: &serde_json::Value) {
    let agg = &digest["load"]["aggregate"];
    if !agg.is_object() {
        return;
    }

    let total_rps = agg["total_rps"].as_f64().unwrap_or(0.0);
    let total_requests = agg["total_requests"].as_u64().unwrap_or(0);
    let error_rate = agg["error_rate"].as_f64().unwrap_or(0.0);
    let hit_rate = digest["load"]["cache"]["hit_rate"].as_f64().unwrap_or(0.0);
    let scenario_count = digest["load"]["scenarios"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);

    html.push_str(
        "<div class=\"dash-section\">\n<h2>Load Test Summary</h2>\n<div class=\"stat-strip\">\n",
    );
    for (val, lbl) in [
        (format!("{:.0}", total_rps), "Total RPS"),
        (format_number_commas(total_requests), "Total Requests"),
        (format!("{:.1}%", error_rate * 100.0), "Error Rate"),
        (format!("{:.1}%", hit_rate * 100.0), "Cache Hit Rate"),
        (format!("{scenario_count}"), "Scenarios"),
    ] {
        html.push_str(&format!(
            "<div class=\"stat-box\"><div class=\"value\">{val}</div><div class=\"label\">{lbl}</div></div>\n"
        ));
    }
    html.push_str("</div>\n</div>\n");
}

/// Render benchmark regressions/improvements table.
fn render_bench_summary(html: &mut String, digest: &serde_json::Value) {
    let profiles = match digest["bench"]["profiles"].as_array() {
        Some(arr) if !arr.is_empty() => arr,
        _ => return,
    };

    let mut regressions: Vec<(&str, &str, f64)> = Vec::new(); // (name, profile, change_pct)
    let mut improvements: Vec<(&str, &str, f64)> = Vec::new();
    let mut inconclusive: Vec<(&str, &str, f64)> = Vec::new();

    for p in profiles {
        let profile_name = p["name"].as_str().unwrap_or("unknown");
        if let Some(regs) = p["regressions"].as_array() {
            for r in regs {
                let name = r["name"].as_str().unwrap_or("?");
                let pct = r["change_pct"].as_f64().unwrap_or(0.0);
                regressions.push((name, profile_name, pct));
            }
        }
        if let Some(imps) = p["improvements"].as_array() {
            for imp in imps {
                let name = imp["name"].as_str().unwrap_or("?");
                let pct = imp["change_pct"].as_f64().unwrap_or(0.0);
                improvements.push((name, profile_name, pct));
            }
        }
        if let Some(incs) = p["inconclusive"].as_array() {
            for inc in incs {
                let name = inc["name"].as_str().unwrap_or("?");
                let pct = inc["change_pct"].as_f64().unwrap_or(0.0);
                inconclusive.push((name, profile_name, pct));
            }
        }
    }

    if regressions.is_empty() && improvements.is_empty() && inconclusive.is_empty() {
        return;
    }

    regressions.sort_by(|a, b| {
        b.2.abs()
            .partial_cmp(&a.2.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    improvements.sort_by(|a, b| {
        b.2.abs()
            .partial_cmp(&a.2.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    inconclusive.sort_by(|a, b| {
        b.2.abs()
            .partial_cmp(&a.2.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    html.push_str("<div class=\"dash-section\">\n<h2>Benchmark Changes</h2>\n");
    html.push_str("<table>\n<tr><th>Name</th><th>Profile</th><th>Change</th></tr>\n");
    for (name, profile, pct) in regressions.iter().take(10) {
        html.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td style=\"color:var(--fail)\">+{:.1}%</td></tr>\n",
            html_escape(name),
            html_escape(profile),
            pct,
        ));
    }
    for (name, profile, pct) in improvements.iter().take(5) {
        html.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td style=\"color:var(--pass)\">{:.1}%</td></tr>\n",
            html_escape(name),
            html_escape(profile),
            pct,
        ));
    }
    for (name, profile, pct) in inconclusive.iter().take(5) {
        html.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td style=\"color:var(--skip)\">~{:.1}%</td></tr>\n",
            html_escape(name),
            html_escape(profile),
            pct,
        ));
    }
    html.push_str("</table>\n</div>\n");
}

/// Render hit-rate bench results (bench-hitrate / bench-hitrate-sem).
fn render_hitrate_summary(html: &mut String, digest: &serde_json::Value) {
    let hitrate = &digest["hitrate"];
    let workloads = match hitrate["workloads"].as_array() {
        Some(arr) if !arr.is_empty() => arr,
        _ => return,
    };

    let verdict = hitrate["verdict"].as_str().unwrap_or("UNKNOWN");
    let verdict_color = if verdict == "PASS" {
        "var(--pass)"
    } else {
        "var(--fail)"
    };

    html.push_str("<div class=\"dash-section\">\n<h2>Hit-Rate Benchmark</h2>\n");
    html.push_str(&format!(
        "<p>Verdict: <strong style=\"color:{verdict_color}\">{}</strong></p>\n",
        html_escape(verdict)
    ));
    html.push_str(
        "<table>\n<tr><th>Workload</th><th>Best Exact HR</th><th>Best τ</th>\
         <th>Combined HR</th><th>Uplift</th><th>False-Hit</th></tr>\n",
    );
    for w in workloads {
        let name = w["name"].as_str().unwrap_or("?");
        let exact = w["best_exact_hit_rate"].as_f64().unwrap_or(0.0) * 100.0;
        let tau = w["best_tau"]
            .as_f64()
            .map(|t| format!("{t:.2}"))
            .unwrap_or_else(|| "—".to_string());
        let combined = w["best_combined_hit_rate"]
            .as_f64()
            .map(|c| format!("{:.1}%", c * 100.0))
            .unwrap_or_else(|| "—".to_string());
        let uplift = w["best_uplift"]
            .as_f64()
            .map(|u| format!("+{:.1}pp", u * 100.0))
            .unwrap_or_else(|| "—".to_string());
        let false_hr = w["best_false_hit_rate"]
            .as_f64()
            .map(|f| format!("{:.2}%", f * 100.0))
            .unwrap_or_else(|| "—".to_string());
        html.push_str(&format!(
            "<tr><td>{}</td><td>{exact:.1}%</td><td>{tau}</td><td>{combined}</td>\
             <td>{uplift}</td><td>{false_hr}</td></tr>\n",
            html_escape(name),
        ));
    }
    html.push_str("</table>\n</div>\n");
}

/// Render code coverage breakdown with progress bars.
fn render_coverage_summary(html: &mut String, digest: &serde_json::Value) {
    let cov = &digest["coverage"];
    if !cov.is_object() {
        return;
    }

    let overall_pct = cov["overall_coverage_pct"].as_f64().unwrap_or(0.0);
    let overall_color = if overall_pct >= 70.0 {
        "var(--pass)"
    } else if overall_pct >= 50.0 {
        "var(--skip)"
    } else {
        "var(--fail)"
    };

    html.push_str("<div class=\"dash-section\">\n<h2>Code Coverage</h2>\n");
    html.push_str(&format!(
        "<p>Overall: <strong style=\"color:{overall_color}\">{overall_pct:.1}%</strong></p>\n"
    ));

    if let Some(modules) = cov["modules"].as_array() {
        html.push_str("<table>\n<tr><th>Module</th><th>Coverage</th><th></th></tr>\n");
        for m in modules {
            let name = m["name"].as_str().unwrap_or("?");
            let pct = m["coverage_pct"].as_f64().unwrap_or(0.0);
            let color = if pct < 70.0 {
                "var(--skip)"
            } else {
                "var(--pass)"
            };
            let bar_width = pct.clamp(0.0, 100.0);
            html.push_str(&format!(
                "<tr><td>{}</td><td style=\"color:{color}\">{pct:.1}%</td>\
                 <td><div style=\"width:120px;height:8px;background:var(--border);border-radius:4px;overflow:hidden\">\
                 <div style=\"width:{bar_width:.0}%;height:100%;background:{color};border-radius:4px\"></div>\
                 </div></td></tr>\n",
                html_escape(name),
            ));
        }
        html.push_str("</table>\n");
    }
    html.push_str("</div>\n");
}

/// Render profile highlights (RSS trend, IPC, leak confidence) for load and e2e.
fn render_profile_summary(html: &mut String, digest: &serde_json::Value) {
    let profiles = match digest["profiles"].as_object() {
        Some(p) if !p.is_empty() => p,
        _ => return,
    };

    html.push_str(
        "<div class=\"dash-section\">\n<h2>Runtime Profiles</h2>\n\
         <div style=\"display:flex;gap:1rem;flex-wrap:wrap;\">\n",
    );

    for (stage, p) in profiles {
        let peak_rss = p["peak_rss_mb"].as_f64().unwrap_or(0.0);
        let rss_trend = p["rss_trend"].as_str().unwrap_or("stable");
        let rss_growth = p["rss_growth_pct"].as_f64().unwrap_or(0.0);
        let leak = p["leak_confidence"].as_str().unwrap_or("unlikely");
        let cpu_pct = p["cpu_percent"].as_f64().unwrap_or(0.0);

        let ipc = p["hardware_counters"]["ipc"].as_f64().unwrap_or(0.0);
        let cache_miss = p["hardware_counters"]["cache_miss_percent"]
            .as_f64()
            .unwrap_or(0.0);

        let leak_color = match leak {
            "confirmed" => "var(--fail)",
            "suspected" => "var(--skip)",
            _ => "var(--pass)",
        };

        let title = match stage.as_str() {
            "load" => "Load Profile",
            "e2e" => "E2E Profile",
            s => s,
        };

        html.push_str(&format!(
            "<div class=\"assessment-card\" style=\"flex:1;min-width:280px;border-left-color:var(--link);padding:.8rem;\">\n\
             <strong>{title}</strong>\n\
             <div style=\"display:grid;grid-template-columns:1fr 1fr;gap:.3rem .8rem;margin-top:.5rem;font-size:.85rem;\">\
             <div>Peak RSS</div><div><strong>{peak_rss:.1} MB</strong></div>\
             <div>RSS Trend</div><div>{rss_trend} ({rss_growth:+.0}%)</div>\
             <div>CPU</div><div>{cpu_pct:.1}%</div>\
             <div>Leak</div><div style=\"color:{leak_color}\">{leak}</div>"
        ));
        if ipc > 0.0 {
            html.push_str(&format!(
                "<div>IPC</div><div>{ipc:.2}</div>\
                 <div>Cache Miss</div><div>{cache_miss:.1}%</div>"
            ));
        }
        html.push_str("</div>\n</div>\n");
    }
    html.push_str("</div>\n</div>\n");
}

/// Render cascade stats and error correlation.
fn render_cascade_summary(html: &mut String, digest: &serde_json::Value) {
    let cascade = &digest["cascade"];
    if !cascade.is_object() {
        return;
    }

    let total = cascade["total_cascades"].as_u64().unwrap_or(0);
    let success_rate = cascade["success_rate_pct"].as_f64().unwrap_or(0.0);
    let avg_time = cascade["timing"]["avg_ms"].as_f64().unwrap_or(0.0);
    let p99_time = cascade["timing"]["p99_ms"].as_f64().unwrap_or(0.0);
    let avg_depth = cascade["depth"]["avg"].as_f64().unwrap_or(0.0);
    let max_depth = cascade["depth"]["max"].as_u64().unwrap_or(0);

    html.push_str(
        "<div class=\"dash-section\">\n<h2>Cascade Queries</h2>\n<div class=\"stat-strip\">\n",
    );
    for (val, lbl) in [
        (format_number_commas(total), "Total Cascades"),
        (format!("{success_rate:.1}%"), "Success Rate"),
        (format!("{avg_time:.1}ms"), "Avg Time"),
        (format!("{p99_time:.1}ms"), "P99 Time"),
        (format!("{avg_depth:.1} / {max_depth}"), "Avg/Max Depth"),
    ] {
        html.push_str(&format!(
            "<div class=\"stat-box\"><div class=\"value\">{val}</div><div class=\"label\">{lbl}</div></div>\n"
        ));
    }
    html.push_str("</div>\n");

    // Stop reason breakdown
    if let Some(reasons) = cascade["stop_reasons"].as_object() {
        if !reasons.is_empty() {
            html.push_str("<table>\n<tr><th>Stop Reason</th><th>Count</th></tr>\n");
            for (reason, count) in reasons {
                html.push_str(&format!(
                    "<tr><td>{}</td><td>{}</td></tr>\n",
                    html_escape(reason),
                    count.as_u64().unwrap_or(0),
                ));
            }
            html.push_str("</table>\n");
        }
    }

    // Error correlation from load digest
    let ec = &digest["load"]["error_correlation"];
    if ec.is_object() {
        if let Some(note) = ec["correlation_note"].as_str() {
            html.push_str(&format!(
                "<p style=\"font-size:.85rem;color:var(--skip);margin-top:.5rem\">{}</p>\n",
                html_escape(note),
            ));
        }
    }

    html.push_str("</div>\n");
}

// ---------------------------------------------------------------------------
// index subcommand
// ---------------------------------------------------------------------------

fn cmd_index(args: &[String]) {
    let results_dir = args.first().unwrap_or_else(|| {
        eprintln!("Usage: test_runner index <results-dir>");
        std::process::exit(1);
    });

    let dir = std::path::Path::new(results_dir);
    if !dir.is_dir() {
        eprintln!("ERROR: {results_dir} is not a directory");
        std::process::exit(1);
    }

    // Clean up old result directories — keep only the 10 most recent.
    // Directory layout is tests/results/<timestamp>/<profile>/, so cleanup
    // targets the grandparent (tests/results/) to remove old timestamp dirs.
    //
    // Safety: only run cleanup when the immediate parent (`timestamp_dir`) is a
    // top-level results dir (i.e. its name looks like a timestamp string and
    // its parent is `tests/results/`). Skip when nested inside a named
    // sub-layout (e.g. `tests/results/perf-tuning/<ts>/`, `tests/results/debug/`)
    // — those are owned by their own workflow and must not be swept by a
    // generic cleanup that would delete unrelated runs.
    if let Some(timestamp_dir) = dir.parent() {
        if let Some(results_dir) = timestamp_dir.parent() {
            let parent_name = timestamp_dir.file_name().and_then(|n| n.to_str());
            let in_named_subdir = matches!(
                parent_name,
                Some("perf-tuning")
                    | Some("debug")
                    | Some("load")
                    | Some("eval")
                    | Some("uat")
                    | Some("hitrate")
            );
            let is_canonical = !in_named_subdir
                && results_dir
                    .file_name()
                    .map(|n| n == "results")
                    .unwrap_or(false)
                && results_dir
                    .parent()
                    .and_then(|p| p.file_name())
                    .map(|n| n == "tests")
                    .unwrap_or(false);
            if is_canonical {
                cleanup_old_results(results_dir, 10);
            } else {
                eprintln!(
                    "Skipping cleanup: {} is nested under named subdir {} (not a canonical <repo>/tests/results/ path)",
                    dir.display(),
                    parent_name.unwrap_or("?")
                );
            }
        }
    }

    // Generate section-level HTML reports before walking the directory
    // so the generated files appear in the index listing.
    generate_section_reports(dir);

    // Timestamp comes from the grandparent directory name (<timestamp>/<profile>/)
    let timestamp = dir
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    // Collect files grouped by top-level subdirectory
    let mut sections: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();

    for entry in walkdir::WalkDir::new(dir)
        .min_depth(1)
        .sort_by_file_name()
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(dir)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if rel == "index.html"
            || rel == "index.json"
            || rel == "perf_digest.json"
            || rel == "audit_digest.json"
        {
            continue;
        }
        // Skip hidden timing files from the file listing
        let fname = rel.rsplit('/').next().unwrap_or(&rel);
        if fname.starts_with('.') {
            continue;
        }
        let section = rel.split('/').next().unwrap_or(&rel).to_string();
        sections.entry(section).or_default().push(rel);
    }

    // Sort files within each section: HTML first, then TXT, then JSON, then rest
    for files in sections.values_mut() {
        files.sort_by(|a, b| {
            fn ext_order(s: &str) -> u8 {
                match s.rsplit('.').next().unwrap_or("") {
                    "html" => 0,
                    "txt" => 1,
                    "json" => 2,
                    _ => 3,
                }
            }
            ext_order(a).cmp(&ext_order(b)).then_with(|| a.cmp(b))
        });
    }

    // Detect pass/fail status for each section
    let statuses: std::collections::BTreeMap<String, &str> = sections
        .keys()
        .map(|name| {
            let status = detect_section_status(dir, name);
            (name.clone(), status)
        })
        .collect();

    // Read timing data for each section (.start_time / .end_time epoch files)
    let durations: std::collections::BTreeMap<String, Option<u64>> = sections
        .keys()
        .map(|name| {
            let duration = read_section_duration(dir, name);
            (name.clone(), duration)
        })
        .collect();

    let total_duration_secs: u64 = durations.values().filter_map(|d| *d).sum();

    let css = r#"  :root { --bg: #1a1a2e; --card: #16213e; --accent: #0f3460; --text: #e0e0e0; --link: #58a6ff; --border: #30363d; --pass: #3fb950; --fail: #f85149; --skip: #d29922; }
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, monospace; background: var(--bg); color: var(--text); padding: 2rem; }
  h1 { margin-bottom: .25rem; font-size: 1.5rem; }
  .summary { margin-bottom: 1rem; font-size: 1rem; }
  .summary .pass { color: var(--pass); font-weight: bold; }
  .summary .fail { color: var(--fail); font-weight: bold; }
  .timestamp { color: #888; margin-bottom: 1.5rem; font-size: .9rem; }
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(340px, 1fr)); gap: 1.25rem; }
  .card { background: var(--card); border: 1px solid var(--border); border-radius: 8px; padding: 1.25rem; }
  .card.status-pass { border-color: var(--pass); }
  .card.status-fail { border-color: var(--fail); }
  .card.status-skip { border-color: var(--skip); }
  .card h2 { font-size: 1.1rem; margin-bottom: .75rem; text-transform: capitalize; border-bottom: 1px solid var(--border); padding-bottom: .5rem; display: flex; justify-content: space-between; align-items: center; flex-wrap: wrap; gap: .25rem; }
  .card .duration { font-size: .75rem; color: #888; font-weight: normal; }
  .card ul { list-style: none; }
  .card li { padding: .25rem 0; }
  .card a { color: var(--link); text-decoration: none; }
  .card a:hover { text-decoration: underline; }
  .badge { display: inline-block; font-size: .7rem; padding: .1rem .4rem; border-radius: 4px; margin-left: .4rem; vertical-align: middle; }
  .html { background: #1f6feb33; color: #58a6ff; }
  .json { background: #23863633; color: #3fb950; }
  .txt  { background: #d2992233; color: #d29922; }
  .status { font-size: .75rem; padding: .15rem .5rem; border-radius: 4px; font-weight: bold; text-transform: uppercase; }
  .status-label-pass { background: #23863633; color: var(--pass); }
  .status-label-fail { background: #f8514933; color: var(--fail); }
  .status-label-skip { background: #d2992233; color: var(--skip); }
  .status-label-warn { background: #d2992233; color: var(--skip); }
  .status-label-unknown { background: #30363d; color: #888; }
  footer { margin-top: 2rem; color: #555; font-size: .8rem; text-align: center; }
  .dashboard { margin-bottom: 1.5rem; }
  .dash-section { margin-bottom: 1.25rem; }
  .dash-section h2 { font-size: 1.1rem; margin-bottom: .6rem; color: var(--link); }
  .assessment-card { background: var(--card); border: 1px solid var(--border); border-left: 4px solid var(--border);
                     border-radius: 6px; padding: .5rem .8rem; }
  .obs-banner { background: #0f3460; border: 1px solid var(--border); border-radius: 6px;
                padding: .75rem 1rem; margin-bottom: 1rem; }
  .obs-banner ul { margin-left: 1.2rem; font-size: .85rem; }
  .stat-strip { display: flex; gap: .75rem; flex-wrap: wrap; margin-bottom: 1rem; }
  .stat-strip .stat-box { flex: 1; min-width: 120px; background: var(--card); border: 1px solid var(--border);
                           border-radius: 8px; padding: .8rem; text-align: center; }
  .stat-strip .stat-box .value { font-size: 1.3rem; font-weight: bold; }
  .stat-strip .stat-box .label { font-size: .75rem; color: #8b949e; margin-top: .2rem; }"#;

    let mut html = String::with_capacity(8_000);
    html.push_str(&format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>Test Results</title>\n<style>\n{css}\n</style>\n</head>\n<body>\n"
    ));

    // Overall pass/fail summary
    let pass_count = statuses.values().filter(|&&s| s == "pass").count();
    let fail_count = statuses.values().filter(|&&s| s == "fail").count();
    let skip_count = statuses.values().filter(|&&s| s == "skip").count();
    let overall = if fail_count > 0 { "FAIL" } else { "PASS" };
    let overall_class = if fail_count > 0 { "fail" } else { "pass" };

    let total_duration_str = format_duration(total_duration_secs);
    let duration_suffix = if total_duration_str.is_empty() {
        String::new()
    } else {
        format!(" &mdash; {total_duration_str}")
    };
    html.push_str(&format!(
        "<h1>Conproxy Test Results</h1>\n\
         <p class=\"summary\">Overall: <span class=\"{overall_class}\">{overall}</span> \
         &mdash; {pass_count} passed, {fail_count} failed, {skip_count} skipped\
         {duration_suffix}</p>\n\
         <p class=\"timestamp\">Run: {timestamp}</p>\n"
    ));

    // Generate audit digest (v2) early so we can render it visually
    generate_audit_digest(dir);
    let digest: Option<serde_json::Value> = std::fs::read_to_string(dir.join("audit_digest.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());
    if let Some(ref d) = digest {
        render_dashboard(&mut html, d);
    }

    html.push_str("<div class=\"grid\">\n");

    let ordered = ["lint", "unit", "coverage", "bench", "e2e", "load", "eval"];
    fn label(s: &str) -> &str {
        match s {
            "lint" => "Lint & Format",
            "unit" => "Unit Tests",
            "coverage" => "Code Coverage",
            "bench" => "Benchmarks",
            "e2e" => "E2E Proxy Tests",
            "load" => "Load Tests (rlt)",
            "eval" => "Evaluation",
            _ => s,
        }
    }

    // Build per-file status maps for each section
    let file_statuses = detect_file_statuses(dir, &sections);

    // Run log analysis for each section
    let log_analyses: std::collections::BTreeMap<String, LogAnalysis> = sections
        .keys()
        .map(|name| {
            let analysis = analyze_section_logs(&dir.join(name));
            (name.clone(), analysis)
        })
        .collect();

    let total_log_errors: u64 = log_analyses.values().map(|a| a.errors).sum();
    let total_log_warnings: u64 = log_analyses.values().map(|a| a.warnings).sum();

    // Add log counts to the summary line
    let log_suffix = if total_log_errors > 0 || total_log_warnings > 0 {
        let mut parts = Vec::new();
        if total_log_errors > 0 {
            parts.push(format!(
                "<span class=\"fail\">{total_log_errors} log errors</span>"
            ));
        }
        if total_log_warnings > 0 {
            parts.push(format!(
                "<span style=\"color: var(--skip)\">{total_log_warnings} log warnings</span>"
            ));
        }
        format!(" &mdash; {}", parts.join(", "))
    } else {
        String::new()
    };

    // Re-emit summary line with log counts (replace the one already written)
    // We need to update the summary before the grid. Easiest: insert before </p>\n<div
    html = html.replace(
        &format!("<p class=\"timestamp\">Run: {timestamp}</p>\n<div class=\"grid\">\n"),
        &format!(
            "<p class=\"summary\" style=\"font-size: .9rem\">{log_suffix}</p>\n\
             <p class=\"timestamp\">Run: {timestamp}</p>\n<div class=\"grid\">\n"
        ),
    );

    let mut emitted = std::collections::HashSet::new();
    let emit = |html: &mut String,
                name: &str,
                files: &[String],
                status: &str,
                duration: Option<u64>,
                fstat: &std::collections::HashMap<String, &str>,
                logs: &LogAnalysis| {
        if files.is_empty() {
            return;
        }
        let card_class = match status {
            "pass" => "card status-pass",
            "fail" => "card status-fail",
            "skip" => "card status-skip",
            _ => "card",
        };
        let status_label = match status {
            "pass" => r#"<span class="status status-label-pass">PASS</span>"#,
            "fail" => r#"<span class="status status-label-fail">FAIL</span>"#,
            "skip" => r#"<span class="status status-label-skip">SKIP</span>"#,
            _ => r#"<span class="status status-label-unknown">?</span>"#,
        };
        let duration_label = match duration {
            Some(secs) => format!(
                r#" <span class="duration">{}</span>"#,
                format_duration(secs)
            ),
            None => String::new(),
        };

        // Log error/warning badges
        let mut log_badges = String::new();
        if logs.errors > 0 {
            log_badges.push_str(&format!(
                r#" <span class="status status-label-fail">{} errors</span>"#,
                logs.errors
            ));
        }
        if logs.warnings > 0 {
            log_badges.push_str(&format!(
                r#" <span class="status status-label-warn">{} warnings</span>"#,
                logs.warnings
            ));
        }

        html.push_str(&format!(
            "<div class=\"{card_class}\">\n<h2>{}{duration_label}{status_label}{log_badges}</h2>\n<ul>\n",
            label(name)
        ));
        for f in files {
            let fname = f.rsplit('/').next().unwrap_or(f);
            let ext = fname.rsplit('.').next().unwrap_or("");
            let badge = match ext {
                "html" => r#" <span class="badge html">HTML</span>"#,
                "json" => r#" <span class="badge json">JSON</span>"#,
                "txt" => r#" <span class="badge txt">TXT</span>"#,
                _ => "",
            };
            let item_status = match fstat.get(f.as_str()) {
                Some(&"pass") => r#" <span class="status status-label-pass">PASS</span>"#,
                Some(&"fail") => r#" <span class="status status-label-fail">FAIL</span>"#,
                Some(&"warn") => r#" <span class="status status-label-warn">WARN</span>"#,
                Some(&"skip") => r#" <span class="status status-label-skip">SKIP</span>"#,
                _ => "",
            };
            html.push_str(&format!(
                "<li><a href=\"{f}\">{fname}</a>{badge}{item_status}</li>\n"
            ));
        }
        html.push_str("</ul>\n</div>\n");
    };

    let default_logs = LogAnalysis::default();

    for &name in &ordered {
        if let Some(files) = sections.get(name) {
            let status = statuses.get(name).copied().unwrap_or("unknown");
            let dur = durations.get(name).copied().flatten();
            let empty = std::collections::HashMap::new();
            let fstat = file_statuses.get(name).unwrap_or(&empty);
            let logs = log_analyses.get(name).unwrap_or(&default_logs);
            emit(&mut html, name, files, status, dur, fstat, logs);
            emitted.insert(name.to_string());
        }
    }
    for (name, files) in &sections {
        if !emitted.contains(name.as_str()) {
            let status = statuses.get(name.as_str()).copied().unwrap_or("unknown");
            let dur = durations.get(name.as_str()).copied().flatten();
            let empty = std::collections::HashMap::new();
            let fstat = file_statuses.get(name.as_str()).unwrap_or(&empty);
            let logs = log_analyses.get(name.as_str()).unwrap_or(&default_logs);
            emit(&mut html, name, files, status, dur, fstat, logs);
        }
    }

    html.push_str("</div>\n<footer>Generated by conproxy test suite</footer>\n");

    // Embed perf digest as JSON for programmatic access
    if let Some(ref d) = digest {
        if let Ok(digest_str) = serde_json::to_string_pretty(d) {
            html.push_str("<script type=\"application/json\" id=\"perf-data\">\n");
            html.push_str(&digest_str);
            html.push_str("\n</script>\n");
        }
    }

    html.push_str("</body>\n</html>\n");

    let index_path = dir.join("index.html");
    std::fs::write(&index_path, &html).expect("Failed to write index.html");
    eprintln!("Generated {}", index_path.display());

    // Generate index.json
    let json_sections: Vec<serde_json::Value> = {
        let mut out = Vec::new();
        let build_section = |name: &str, files: &[String]| -> serde_json::Value {
            let status = statuses.get(name).copied().unwrap_or("unknown");
            let dur = durations.get(name).copied().flatten();
            let logs = log_analyses.get(name);
            let mut obj = serde_json::json!({
                "name": name,
                "label": label(name),
                "status": status,
                "files": files,
            });
            if let Some(secs) = dur {
                obj["duration_secs"] = serde_json::json!(secs);
            }
            if let Some(la) = logs {
                if la.errors > 0 || la.warnings > 0 {
                    obj["log_errors"] = serde_json::json!(la.errors);
                    obj["log_warnings"] = serde_json::json!(la.warnings);
                    if !la.error_lines.is_empty() {
                        obj["log_error_samples"] = serde_json::json!(la.error_lines);
                    }
                    if !la.warning_lines.is_empty() {
                        obj["log_warning_samples"] = serde_json::json!(la.warning_lines);
                    }
                }
            }
            obj
        };
        for &name in &ordered {
            if let Some(files) = sections.get(name) {
                out.push(build_section(name, files));
            }
        }
        for (name, files) in &sections {
            if !emitted.contains(name.as_str()) {
                out.push(build_section(name, files));
            }
        }
        out
    };

    // Build per-section log analysis for the top-level summary
    let mut log_sections = serde_json::Map::new();
    for (name, la) in &log_analyses {
        if la.errors > 0 || la.warnings > 0 {
            let mut entry = serde_json::json!({
                "errors": la.errors,
                "warnings": la.warnings,
            });
            if !la.error_lines.is_empty() {
                entry["error_samples"] = serde_json::json!(la.error_lines);
            }
            if !la.warning_lines.is_empty() {
                entry["warning_samples"] = serde_json::json!(la.warning_lines);
            }
            log_sections.insert(name.clone(), entry);
        }
    }

    let mut index_json = serde_json::json!({
        "timestamp": timestamp,
        "overall": overall.to_lowercase(),
        "pass_count": pass_count,
        "fail_count": fail_count,
        "skip_count": skip_count,
        "total_duration_secs": total_duration_secs,
        "sections": json_sections,
    });

    if total_log_errors > 0 || total_log_warnings > 0 {
        index_json["log_analysis"] = serde_json::json!({
            "total_errors": total_log_errors,
            "total_warnings": total_log_warnings,
            "sections": log_sections,
        });
    }

    let json_path = dir.join("index.json");
    let json_str =
        serde_json::to_string_pretty(&index_json).expect("Failed to serialize index.json");
    std::fs::write(&json_path, &json_str).expect("Failed to write index.json");
    eprintln!("Generated {}", json_path.display());
}

/// Remove old result directories, keeping only the `keep` most recent.
/// Directories are sorted by name (which is a timestamp like `20260227-223656`).
fn cleanup_old_results(results_parent: &std::path::Path, keep: usize) {
    let mut dirs: Vec<std::path::PathBuf> = match std::fs::read_dir(results_parent) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
            .map(|e| e.path())
            .collect(),
        Err(_) => return,
    };

    // Sort ascending by directory name (timestamp format sorts naturally)
    dirs.sort();

    if dirs.len() <= keep {
        return;
    }

    let to_remove = dirs.len() - keep;
    for dir in dirs.iter().take(to_remove) {
        eprintln!(
            "Cleaning up old results: {}",
            dir.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
        if let Err(e) = std::fs::remove_dir_all(dir) {
            eprintln!("Warning: failed to remove {}: {e}", dir.display());
        }
    }
}

/// Read `.start_time` and `.end_time` epoch files from a section directory.
/// Returns the duration in seconds, or None if either file is missing.
fn read_section_duration(results_dir: &std::path::Path, section: &str) -> Option<u64> {
    let section_dir = results_dir.join(section);
    let start = std::fs::read_to_string(section_dir.join(".start_time"))
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    let end = std::fs::read_to_string(section_dir.join(".end_time"))
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    Some(end.saturating_sub(start))
}

/// Format seconds into a human-readable duration string like "2m 30s" or "45s".
fn format_duration(secs: u64) -> String {
    if secs == 0 {
        return String::new();
    }
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

/// Detect pass/fail status for a section by reading its output files.
///
/// Returns one of: "pass", "fail", "skip", "unknown"
fn detect_section_status<'a>(results_dir: &std::path::Path, section: &str) -> &'a str {
    let section_dir = results_dir.join(section);

    match section {
        "lint" => detect_lint_status(&section_dir),
        "bench" => detect_bench_status(&section_dir),
        "load" => detect_load_status(&section_dir),
        "eval" => detect_eval_status(&section_dir),
        _ => detect_test_result_status(&section_dir),
    }
}

/// Lint: look for "fmt: PASS"/"fmt: FAIL" and "clippy: PASS"/"clippy: FAIL" in output.txt
fn detect_lint_status(section_dir: &std::path::Path) -> &'static str {
    let output = section_dir.join("output.txt");
    let content = match std::fs::read_to_string(&output) {
        Ok(c) => c,
        Err(_) => return "unknown",
    };
    if content.contains("fmt: FAIL") || content.contains("clippy: FAIL") {
        return "fail";
    }
    if content.contains("fmt: PASS") && content.contains("clippy: PASS") {
        return "pass";
    }
    "unknown"
}

/// Bench: check report_*.txt files for "Regressions exceed" (fail) or presence = pass
fn detect_bench_status(section_dir: &std::path::Path) -> &'static str {
    let mut found_report = false;
    let entries = match std::fs::read_dir(section_dir) {
        Ok(e) => e,
        Err(_) => return "unknown",
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("report_") && name_str.ends_with(".txt") {
            found_report = true;
            if let Ok(content) = std::fs::read_to_string(entry.path()) {
                if content.contains("Regressions exceed") {
                    return "fail";
                }
            }
        }
    }
    if found_report {
        "pass"
    } else {
        "unknown"
    }
}

/// Load: check output.txt for "Benchmark complete!" (pass)
fn detect_load_status(section_dir: &std::path::Path) -> &'static str {
    let output = section_dir.join("output.txt");
    let content = match std::fs::read_to_string(&output) {
        Ok(c) => c,
        Err(_) => return "unknown",
    };
    if content.contains("Benchmark complete!") {
        return "pass";
    }
    if content.contains("FAILED") || content.contains("error") {
        return "fail";
    }
    "unknown"
}

/// Eval: check eval_results.json — fail if all queries errored or all recalls are zero.
fn detect_eval_status(section_dir: &std::path::Path) -> &'static str {
    let eval_json = section_dir.join("eval_results.json");
    if let Ok(content) = std::fs::read_to_string(&eval_json) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(verticals) = json["verticals"].as_array() {
                // Check if all queries failed (CLI errors)
                let all_failed = verticals
                    .iter()
                    .all(|v| v["successful_queries"].as_u64().unwrap_or(0) == 0);
                if all_failed {
                    return "fail";
                }

                // Check if all recalls are zero
                let all_zero_recall = verticals
                    .iter()
                    .all(|v| v["total_recall"].as_str() == Some("0.000"));
                if all_zero_recall {
                    return "fail";
                }
            }
        }
    }
    // Fall through to standard test result check
    detect_test_result_status(section_dir)
}

/// Generic test sections (unit, e2e, coverage): check output.txt for
/// "test result: ok." (pass) or "test result: FAILED." (fail) or "SKIP:" (skip)
fn detect_test_result_status(section_dir: &std::path::Path) -> &'static str {
    let output = section_dir.join("output.txt");
    let content = match std::fs::read_to_string(&output) {
        Ok(c) => c,
        Err(_) => return "unknown",
    };
    if content.starts_with("SKIP:") || content.contains("\nSKIP:") {
        return "skip";
    }
    // Check all "test result:" lines — any FAILED means fail
    let mut found_result = false;
    let mut has_failure = false;
    for line in content.lines() {
        if line.contains("test result:") {
            found_result = true;
            if line.contains("FAILED") {
                has_failure = true;
            }
        }
    }
    if has_failure {
        return "fail";
    }
    if found_result {
        return "pass";
    }
    "unknown"
}

// ---------------------------------------------------------------------------
// log analysis
// ---------------------------------------------------------------------------

/// Counts of errors and warnings found in section log/output files.
#[derive(Default)]
struct LogAnalysis {
    errors: u64,
    warnings: u64,
    error_lines: Vec<String>,
    warning_lines: Vec<String>,
}

/// Scan all text files in a section directory for error and warning patterns.
///
/// Detects:
/// - tracing format: `  ERROR `, `  WARN ` (two leading spaces + level)
/// - cargo format: `error[E`, `error:`, `warning[`, `warning:`
///
/// Excludes false positives like metric names (`error_rate`, `errors_total`),
/// success summaries (`0 failed`, `errors=0`), and Prometheus metadata.
fn analyze_section_logs(section_dir: &std::path::Path) -> LogAnalysis {
    let mut analysis = LogAnalysis::default();

    if !section_dir.is_dir() {
        return analysis;
    }

    // Scan specific files: output.txt, proxy_logs.txt, and any *.log files
    let candidates = [
        section_dir.join("output.txt"),
        section_dir.join("proxy_logs.txt"),
    ];

    for path in &candidates {
        if path.is_file() {
            analyze_file(path, &mut analysis);
        }
    }

    // Also scan any .log files
    if let Ok(entries) = std::fs::read_dir(section_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.ends_with(".log") && entry.path().is_file() {
                analyze_file(&entry.path(), &mut analysis);
            }
        }
    }

    analysis
}

/// Check if a line is a false positive that should be excluded from error/warning counts.
fn is_false_positive(line: &str) -> bool {
    // Metric names and counters
    if line.contains("error_rate")
        || line.contains("error_count")
        || line.contains("errors_total")
        || line.contains("failures_total")
        || line.contains("failure_rate")
        || line.contains("failure_count")
    {
        return true;
    }
    // Success summaries
    if line.contains("0 failed")
        || line.contains("errors=0")
        || line.contains("errors: 0")
        || line.contains("max_errors:")
    {
        return true;
    }
    // Prometheus metadata
    if line.starts_with("# HELP") || line.starts_with("# TYPE") {
        return true;
    }
    // Test result summary lines (already tracked via pass/fail status)
    if line.contains("test result:") {
        return true;
    }
    // Metric labels and config keys
    if line.contains("upstream_error") || line.contains("miss_reason") {
        return true;
    }
    false
}

/// Analyze a single file for error/warning patterns.
fn analyze_file(path: &std::path::Path, analysis: &mut LogAnalysis) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return,
    };

    const MAX_LINES: usize = 5;

    for line in content.lines() {
        if is_false_positive(line) {
            continue;
        }

        // Check for error patterns
        if line.contains("  ERROR ")
            || line.starts_with("error[E")
            || (line.starts_with("error:") && !line.starts_with("error: could not compile"))
            || line.starts_with("error[")
        {
            analysis.errors += 1;
            if analysis.error_lines.len() < MAX_LINES {
                analysis.error_lines.push(truncate_line(line, 200));
            }
            continue;
        }

        // Check for warning patterns
        if line.contains("  WARN ") || line.starts_with("warning[") || line.starts_with("warning:")
        {
            analysis.warnings += 1;
            if analysis.warning_lines.len() < MAX_LINES {
                analysis.warning_lines.push(truncate_line(line, 200));
            }
        }
    }
}

/// Truncate a line to `max_len` characters, appending "..." if truncated.
fn truncate_line(line: &str, max_len: usize) -> String {
    if line.len() <= max_len {
        line.to_string()
    } else {
        format!("{}...", &line[..max_len])
    }
}

// ---------------------------------------------------------------------------
// per-file status detection
// ---------------------------------------------------------------------------

/// Detect per-file statuses for all sections in the results directory.
/// Returns section_name → { relative_path → status }.
fn detect_file_statuses<'a>(
    results_dir: &std::path::Path,
    sections: &std::collections::BTreeMap<String, Vec<String>>,
) -> std::collections::HashMap<String, std::collections::HashMap<String, &'a str>> {
    let mut all = std::collections::HashMap::new();

    for (section, files) in sections {
        let mut fstat: std::collections::HashMap<String, &str> = std::collections::HashMap::new();
        let section_dir = results_dir.join(section);

        match section.as_str() {
            "lint" => {
                // Parse output.txt for per-check results
                if let Ok(content) = std::fs::read_to_string(section_dir.join("output.txt")) {
                    let fmt_ok = content.contains("fmt: PASS");
                    let fmt_fail = content.contains("fmt: FAIL");
                    let clippy_ok = content.contains("clippy: PASS");
                    let clippy_fail = content.contains("clippy: FAIL");
                    for f in files {
                        let fname = f.rsplit('/').next().unwrap_or(f);
                        if fname == "report.html" || fname == "output.txt" {
                            if fmt_fail || clippy_fail {
                                fstat.insert(f.clone(), "fail");
                            } else if fmt_ok && clippy_ok {
                                fstat.insert(f.clone(), "pass");
                            }
                        }
                    }
                }
            }
            "bench" => {
                // Per report_*.json: has regressions = fail, else pass
                for f in files {
                    let fname = f.rsplit('/').next().unwrap_or(f);
                    if fname.starts_with("report_") && fname.ends_with(".json") {
                        if let Ok(content) = std::fs::read_to_string(results_dir.join(f)) {
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                                let regr = json["regression_count"].as_u64().unwrap_or(0);
                                fstat.insert(f.clone(), if regr > 0 { "fail" } else { "pass" });
                                // Match corresponding txt files
                                let profile_name = fname
                                    .strip_prefix("report_")
                                    .and_then(|s| s.strip_suffix(".json"))
                                    .unwrap_or("");
                                let txt_name = format!("{section}/report_{profile_name}.txt");
                                let crit_name = format!("{section}/criterion_{profile_name}.txt");
                                let st = if regr > 0 { "fail" } else { "pass" };
                                if files.iter().any(|x| x == &txt_name) {
                                    fstat.insert(txt_name, st);
                                }
                                if files.iter().any(|x| x == &crit_name) {
                                    fstat.insert(crit_name, st);
                                }
                            }
                        }
                    } else if fname == "report.html" {
                        // Overall bench report gets card status
                        let card_status = detect_bench_status(&section_dir);
                        fstat.insert(f.clone(), card_status);
                    }
                }
            }
            "load" => {
                // Per *_rlt.json: check for non-Success status codes
                if let Ok(content) = std::fs::read_to_string(section_dir.join("summary.json")) {
                    if let Ok(summary) = serde_json::from_str::<serde_json::Value>(&content) {
                        if let Some(benchmarks) = summary["benchmarks"].as_array() {
                            for b in benchmarks {
                                let name = b["name"].as_str().unwrap_or("");
                                let rlt_file = format!("load/{name}_rlt.json");
                                let mut has_errors = false;
                                if let Some(status_map) = b["raw_metrics"]["status"].as_object() {
                                    for key in status_map.keys() {
                                        if !key.starts_with("Success") {
                                            has_errors = true;
                                        }
                                    }
                                }
                                if files.iter().any(|x| x == &rlt_file) {
                                    fstat
                                        .insert(rlt_file, if has_errors { "warn" } else { "pass" });
                                }
                            }
                        }
                    }
                }
                // report.html and summary.json get card status
                let card_status = detect_load_status(&section_dir);
                for f in files {
                    let fname = f.rsplit('/').next().unwrap_or(f);
                    if fname == "report.html" || fname == "summary.json" {
                        fstat.insert(f.clone(), card_status);
                    } else if (fname.starts_with("profile_") && fname.ends_with(".html"))
                        || fname == "profile_results.json"
                    {
                        fstat.insert(f.clone(), "pass");
                    }
                }
            }
            "e2e" => {
                // Parse results.json for per-section errors
                if let Ok(content) = std::fs::read_to_string(section_dir.join("results.json")) {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                        if let Some(metrics) = json["section_metrics"].as_array() {
                            let total_errors: u64 = metrics
                                .iter()
                                .map(|m| m["errors"].as_u64().unwrap_or(0))
                                .sum();
                            // results.json / results.html status
                            for f in files {
                                let fname = f.rsplit('/').next().unwrap_or(f);
                                if fname == "results.json" || fname == "results.html" {
                                    fstat.insert(
                                        f.clone(),
                                        if total_errors > 0 { "warn" } else { "pass" },
                                    );
                                }
                            }
                        }
                    }
                }
                // Per endpoint JSON: parse and check for error indicators
                for f in files {
                    let fname = f.rsplit('/').next().unwrap_or(f);
                    if fname.ends_with(".json")
                        && fname != "results.json"
                        && !fname.starts_with("profile_")
                    {
                        if let Ok(content) = std::fs::read_to_string(results_dir.join(f)) {
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                                // Check for error fields in endpoint snapshots
                                let has_error = json["error"].is_string()
                                    || json.as_object().is_some_and(|m| m.contains_key("error"));
                                fstat.insert(f.clone(), if has_error { "fail" } else { "pass" });
                            }
                        }
                    } else if (fname.starts_with("profile_") && fname.ends_with(".html"))
                        || fname == "profile_results.json"
                    {
                        fstat.insert(f.clone(), "pass");
                    }
                }
            }
            "unit" | "coverage" => {
                // Use card-level status for all files
                let card_status = detect_test_result_status(&section_dir);
                for f in files {
                    let fname = f.rsplit('/').next().unwrap_or(f);
                    if fname == "output.txt"
                        || fname == "report.html"
                        || fname.ends_with("-report.html")
                    {
                        fstat.insert(f.clone(), card_status);
                    }
                }
            }
            "eval" => {
                let card_status = detect_test_result_status(&section_dir);
                for f in files {
                    let fname = f.rsplit('/').next().unwrap_or(f);
                    if fname == "output.txt" {
                        fstat.insert(f.clone(), card_status);
                    } else if (fname.starts_with("profile_") && fname.ends_with(".html"))
                        || fname == "profile_results.json"
                    {
                        fstat.insert(f.clone(), "pass");
                    }
                }
            }
            _ => {}
        }

        if !fstat.is_empty() {
            all.insert(section.clone(), fstat);
        }
    }

    all
}

// ---------------------------------------------------------------------------
// section report generation
// ---------------------------------------------------------------------------

/// Generate HTML reports for sections that don't already have one.
/// Called before the directory walk so generated files appear in the index.
fn generate_section_reports(dir: &std::path::Path) {
    let lint_dir = dir.join("lint");
    if lint_dir.join("output.txt").exists() && !lint_dir.join("report.html").exists() {
        if let Err(e) = generate_lint_report(&lint_dir) {
            eprintln!("Warning: lint report generation failed: {e}");
        }
    }

    let unit_dir = dir.join("unit");
    if unit_dir.join("output.txt").exists() && !unit_dir.join("report.html").exists() {
        if let Err(e) = generate_unit_report(&unit_dir) {
            eprintln!("Warning: unit report generation failed: {e}");
        }
    }

    let bench_dir = dir.join("bench");
    if bench_dir.is_dir() && !bench_dir.join("report.html").exists() {
        if let Err(e) = generate_bench_report(&bench_dir) {
            eprintln!("Warning: bench report generation failed: {e}");
        }
    }

    let load_dir = dir.join("load");
    if load_dir.join("summary.json").exists() && !load_dir.join("report.html").exists() {
        let profile_opt = if load_dir.join("profile_results.json").exists() {
            Some(load_dir.clone())
        } else {
            None
        };
        if let Err(e) = generate_load_report(&load_dir, profile_opt.as_deref()) {
            eprintln!("Warning: load report generation failed: {e}");
        }
    }

    // Generate per-stage profile reports for e2e, eval, and load (from proc-monitor data)
    for (stage_dir_name, stage_label) in &[
        ("e2e", "E2E Proxy Tests"),
        ("eval", "Evaluation"),
        ("load", "Load Tests"),
    ] {
        let stage_dir = dir.join(stage_dir_name);
        if stage_dir.join("profile_results.json").exists()
            && !stage_dir.join("profile_report.html").exists()
        {
            if let Err(e) = generate_stage_profile_report(&stage_dir, stage_label) {
                eprintln!("Warning: {stage_dir_name} profile report generation failed: {e}");
            }
        }
    }
}

fn section_report_css() -> &'static str {
    r#"  :root { --bg: #1a1a2e; --card: #161b22; --text: #e0e0e0; --link: #58a6ff;
           --border: #30363d; --green: #3fb950; --red: #f85149; --yellow: #d29922;
           --surface: #161b22; }
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, monospace;
         background: var(--bg); color: var(--text); padding: 2rem; line-height: 1.5; }
  h1 { font-size: 1.5rem; margin-bottom: .25rem; }
  h2 { font-size: 1.15rem; margin: 1.5rem 0 .8rem; color: var(--link); }
  p { margin-bottom: .75rem; }
  table { width: 100%; border-collapse: collapse; margin-bottom: 1.2rem; }
  th, td { padding: .55rem .75rem; text-align: left; border: 1px solid var(--border); font-size: .85rem; }
  th { background: var(--surface); font-weight: 600; color: #8b949e; }
  tr:hover td { background: #1c2333; }
  .pass { color: var(--green); font-weight: 600; }
  .fail { color: var(--red); font-weight: 600; }
  .warn { color: var(--yellow); font-weight: 600; }
  .card { background: var(--card); border: 1px solid var(--border); border-radius: 8px;
          padding: 1.25rem; margin-bottom: 1.25rem; }
  .card h2 { margin-top: 0; }
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(200px, 1fr));
          gap: 1rem; margin-bottom: 1.25rem; }
  .stat { background: var(--card); border: 1px solid var(--border); border-radius: 8px;
          padding: 1rem; text-align: center; }
  .stat .value { font-size: 1.5rem; font-weight: bold; }
  .stat .label { font-size: .75rem; color: #8b949e; margin-top: .25rem; }
  details { margin: .4rem 0; }
  summary { cursor: pointer; font-size: .85rem; color: #8b949e; }
  summary:hover { color: var(--text); }
  pre { background: var(--surface); border: 1px solid var(--border); border-radius: 4px;
        padding: .6rem .8rem; font-size: .8rem; overflow-x: auto; white-space: pre-wrap;
        margin-top: .5rem; }
  footer { margin-top: 2rem; color: #555; font-size: .8rem; text-align: center; }
  .card-link { display: inline-block; margin-top: .75rem; color: var(--link); text-decoration: none;
               font-weight: 600; font-size: .85rem; }
  .card-link:hover { text-decoration: underline; }
  .nav-back { display: inline-block; margin-bottom: 1.25rem; color: var(--link); text-decoration: none;
              font-size: .85rem; }
  .nav-back:hover { text-decoration: underline; }
  .nav-back::before { content: '\2190  '; }"#
}

fn section_report_head(title: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{}</title>\n<style>\n{}\n</style>\n</head>\n<body>\n",
        html_escape(title),
        section_report_css()
    )
}

fn section_report_footer() -> &'static str {
    "<footer>Generated by conproxy test suite</footer>\n</body>\n</html>\n"
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn format_latency(secs: f64) -> String {
    if secs < 1e-6 {
        format!("{:.0}ns", secs * 1e9)
    } else if secs < 1e-3 {
        format!("{:.1}\u{00b5}s", secs * 1e6)
    } else {
        format!("{:.2}ms", secs * 1e3)
    }
}

fn format_number_commas(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result.chars().rev().collect()
}

fn format_bytes_human(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

// ---- lint report ----

fn generate_lint_report(dir: &std::path::Path) -> Result<(), String> {
    let content = std::fs::read_to_string(dir.join("output.txt"))
        .map_err(|e| format!("Read lint output: {e}"))?;

    let fmt_status = if content.contains("fmt: PASS") {
        "PASS"
    } else if content.contains("fmt: FAIL") {
        "FAIL"
    } else {
        "UNKNOWN"
    };
    let clippy_status = if content.contains("clippy: PASS") {
        "PASS"
    } else if content.contains("clippy: FAIL") {
        "FAIL"
    } else {
        "UNKNOWN"
    };

    let overall = if fmt_status == "PASS" && clippy_status == "PASS" {
        "PASS"
    } else if fmt_status == "FAIL" || clippy_status == "FAIL" {
        "FAIL"
    } else {
        "UNKNOWN"
    };
    let overall_class = if overall == "PASS" { "pass" } else { "fail" };

    let mut html = String::with_capacity(4_000);
    html.push_str(&section_report_head("Lint & Format Report"));
    html.push_str(&format!(
        "<h1>Lint &amp; Format Report</h1>\n\
         <p>Overall: <span class=\"{overall_class}\"><strong>{overall}</strong></span></p>\n"
    ));

    // Summary cards
    html.push_str("<div class=\"grid\">\n");
    let fmt_class = if fmt_status == "PASS" { "pass" } else { "fail" };
    html.push_str(&format!(
        "<div class=\"stat\"><div class=\"value {fmt_class}\">{fmt_status}</div>\
         <div class=\"label\">cargo fmt</div></div>\n"
    ));
    let clippy_class = if clippy_status == "PASS" {
        "pass"
    } else {
        "fail"
    };
    html.push_str(&format!(
        "<div class=\"stat\"><div class=\"value {clippy_class}\">{clippy_status}</div>\
         <div class=\"label\">cargo clippy</div></div>\n"
    ));
    html.push_str("</div>\n");

    // Full output
    html.push_str("<h2>Full Output</h2>\n");
    html.push_str(&format!("<pre>{}</pre>\n", html_escape(&content)));
    html.push_str(section_report_footer());

    let report_path = dir.join("report.html");
    std::fs::write(&report_path, &html).map_err(|e| format!("Write lint report: {e}"))?;
    eprintln!("Generated {}", report_path.display());
    Ok(())
}

// ---- unit report ----

fn generate_unit_report(dir: &std::path::Path) -> Result<(), String> {
    let content = std::fs::read_to_string(dir.join("output.txt"))
        .map_err(|e| format!("Read unit output: {e}"))?;

    // Parse test result lines to get aggregate totals
    let mut total_passed: u64 = 0;
    let mut total_failed: u64 = 0;
    let mut total_ignored: u64 = 0;
    let mut total_duration = String::new();

    for line in content.lines() {
        if line.starts_with("test result:") {
            if let Some(n) = extract_count(line, "passed") {
                total_passed += n;
            }
            if let Some(n) = extract_count(line, "failed") {
                total_failed += n;
            }
            if let Some(n) = extract_count(line, "ignored") {
                total_ignored += n;
            }
            if let Some(idx) = line.find("finished in ") {
                total_duration = line[idx + 12..].trim_end().to_string();
            }
        }
    }

    let total_tests = total_passed + total_failed + total_ignored;

    // Parse individual test lines
    struct TestEntry {
        name: String,
        module: String,
        status: String,
    }

    let mut tests: Vec<TestEntry> = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("test ") {
            continue;
        }
        let rest = &trimmed[5..];
        let (name, status) = if let Some(n) = rest.strip_suffix(" ... ok") {
            (n.to_string(), "ok".to_string())
        } else if let Some(n) = rest.strip_suffix(" ... FAILED") {
            (n.to_string(), "FAILED".to_string())
        } else if let Some(n) = rest.strip_suffix(" ... ignored") {
            (n.to_string(), "ignored".to_string())
        } else {
            continue;
        };

        let module = name
            .rsplit_once("::")
            .map(|(m, _)| m.to_string())
            .unwrap_or_else(|| "(root)".to_string());

        tests.push(TestEntry {
            name,
            module,
            status,
        });
    }

    // Group by module
    let mut modules: std::collections::BTreeMap<String, Vec<&TestEntry>> =
        std::collections::BTreeMap::new();
    for t in &tests {
        modules.entry(t.module.clone()).or_default().push(t);
    }

    let overall = if total_failed > 0 { "FAIL" } else { "PASS" };
    let overall_class = if total_failed > 0 { "fail" } else { "pass" };

    let mut html = String::with_capacity(32_000);
    html.push_str(&section_report_head("Unit Test Report"));
    html.push_str(&format!(
        "<h1>Unit Test Report</h1>\n\
         <p>Overall: <span class=\"{overall_class}\"><strong>{overall}</strong></span></p>\n"
    ));

    // Summary cards
    html.push_str("<div class=\"grid\">\n");
    html.push_str(&format!(
        "<div class=\"stat\"><div class=\"value\">{total_tests}</div>\
         <div class=\"label\">Total</div></div>\n"
    ));
    html.push_str(&format!(
        "<div class=\"stat\"><div class=\"value pass\">{total_passed}</div>\
         <div class=\"label\">Passed</div></div>\n"
    ));
    html.push_str(&format!(
        "<div class=\"stat\"><div class=\"value fail\">{total_failed}</div>\
         <div class=\"label\">Failed</div></div>\n"
    ));
    html.push_str(&format!(
        "<div class=\"stat\"><div class=\"value warn\">{total_ignored}</div>\
         <div class=\"label\">Ignored</div></div>\n"
    ));
    if !total_duration.is_empty() {
        html.push_str(&format!(
            "<div class=\"stat\"><div class=\"value\">{}</div>\
             <div class=\"label\">Duration</div></div>\n",
            html_escape(&total_duration)
        ));
    }
    html.push_str("</div>\n");

    // Tests grouped by module
    if !modules.is_empty() {
        html.push_str("<h2>Tests by Module</h2>\n");
        for (module, entries) in &modules {
            let mod_passed = entries.iter().filter(|e| e.status == "ok").count();
            let mod_failed = entries.iter().filter(|e| e.status == "FAILED").count();
            let mod_ignored = entries.iter().filter(|e| e.status == "ignored").count();

            html.push_str(&format!(
                "<details{}>\n<summary><strong>{}</strong> &mdash; \
                 <span class=\"pass\">{mod_passed} passed</span>",
                if mod_failed > 0 { " open" } else { "" },
                html_escape(module)
            ));
            if mod_failed > 0 {
                html.push_str(&format!(
                    ", <span class=\"fail\">{mod_failed} failed</span>"
                ));
            }
            if mod_ignored > 0 {
                html.push_str(&format!(
                    ", <span class=\"warn\">{mod_ignored} ignored</span>"
                ));
            }
            html.push_str("</summary>\n<table>\n<tr><th>Test</th><th>Status</th></tr>\n");

            for entry in entries {
                let short_name = entry
                    .name
                    .rsplit_once("::")
                    .map(|(_, n)| n)
                    .unwrap_or(&entry.name);
                let (cls, label) = match entry.status.as_str() {
                    "ok" => ("pass", "PASS"),
                    "FAILED" => ("fail", "FAIL"),
                    _ => ("warn", "SKIP"),
                };
                html.push_str(&format!(
                    "<tr><td>{}</td><td class=\"{cls}\">{label}</td></tr>\n",
                    html_escape(short_name)
                ));
            }

            html.push_str("</table>\n</details>\n");
        }
    }

    html.push_str(section_report_footer());

    let report_path = dir.join("report.html");
    std::fs::write(&report_path, &html).map_err(|e| format!("Write unit report: {e}"))?;
    eprintln!("Generated {}", report_path.display());
    Ok(())
}

/// Extract a numeric count preceding a label in a test result line.
/// e.g. extract_count("test result: ok. 5 passed; 2 failed;", "passed") => Some(5)
fn extract_count(line: &str, label: &str) -> Option<u64> {
    let idx = line.find(label)?;
    let before = line[..idx].trim_end();
    let num_str = before.rsplit(|c: char| !c.is_ascii_digit()).next()?;
    num_str.parse().ok()
}

// ---- bench report ----

fn generate_bench_report(dir: &std::path::Path) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("Read bench dir: {e}"))?;

    struct BenchProfile {
        name: String,
        threshold: f64,
        improvements: Vec<serde_json::Value>,
        regressions: Vec<serde_json::Value>,
        criterion_output: Option<String>,
        report_txt: Option<String>,
    }

    let mut profiles: Vec<BenchProfile> = Vec::new();

    for entry in entries.flatten() {
        let fname = entry.file_name();
        let fname_str = fname.to_string_lossy();
        if let Some(profile_name) = fname_str
            .strip_prefix("report_")
            .and_then(|s| s.strip_suffix(".json"))
        {
            let content = std::fs::read_to_string(entry.path())
                .map_err(|e| format!("Read {fname_str}: {e}"))?;
            let json: serde_json::Value =
                serde_json::from_str(&content).map_err(|e| format!("Parse {fname_str}: {e}"))?;

            let threshold = json["threshold_pct"].as_f64().unwrap_or(0.0);
            let improvements = json["improvements"].as_array().cloned().unwrap_or_default();
            let regressions = json["regressions"].as_array().cloned().unwrap_or_default();

            let criterion_output =
                std::fs::read_to_string(dir.join(format!("criterion_{profile_name}.txt"))).ok();
            let report_txt =
                std::fs::read_to_string(dir.join(format!("report_{profile_name}.txt"))).ok();

            profiles.push(BenchProfile {
                name: profile_name.to_string(),
                threshold,
                improvements,
                regressions,
                criterion_output,
                report_txt,
            });
        }
    }

    if profiles.is_empty() {
        return Err("No bench report JSON files found".into());
    }

    profiles.sort_by(|a, b| a.name.cmp(&b.name));

    let total_improvements: usize = profiles.iter().map(|p| p.improvements.len()).sum();
    let total_regressions: usize = profiles.iter().map(|p| p.regressions.len()).sum();
    let overall = if total_regressions > 0 {
        "REGRESSIONS"
    } else {
        "PASS"
    };
    let overall_class = if total_regressions > 0 {
        "fail"
    } else {
        "pass"
    };

    let mut html = String::with_capacity(8_000);
    html.push_str(&section_report_head("Benchmark Report"));
    html.push_str(&format!(
        "<h1>Benchmark Report</h1>\n\
         <p>Overall: <span class=\"{overall_class}\"><strong>{overall}</strong></span> \
         &mdash; {total_improvements} improvements, {total_regressions} regressions</p>\n"
    ));

    for profile in &profiles {
        let status = if profile.regressions.is_empty() {
            "pass"
        } else {
            "fail"
        };
        html.push_str(&format!(
            "<div class=\"card\">\n<h2>Profile: {} \
             <span class=\"{status}\">{} improvements, {} regressions</span></h2>\n\
             <p>Threshold: {:.1}%</p>\n",
            html_escape(&profile.name),
            profile.improvements.len(),
            profile.regressions.len(),
            profile.threshold
        ));

        if !profile.regressions.is_empty() {
            html.push_str(
                "<h2>Regressions</h2>\n<table>\n\
                 <tr><th>Benchmark</th><th>Change</th></tr>\n",
            );
            for r in &profile.regressions {
                let bench_name = r["name"].as_str().unwrap_or("unknown");
                let pct = r["change_pct"].as_f64().unwrap_or(0.0);
                html.push_str(&format!(
                    "<tr><td>{}</td><td class=\"fail\">{pct:+.2}%</td></tr>\n",
                    html_escape(bench_name)
                ));
            }
            html.push_str("</table>\n");
        }

        if !profile.improvements.is_empty() {
            html.push_str(
                "<h2>Improvements</h2>\n<table>\n\
                 <tr><th>Benchmark</th><th>Change</th></tr>\n",
            );
            for imp in &profile.improvements {
                let bench_name = imp["name"].as_str().unwrap_or("unknown");
                let pct = imp["change_pct"].as_f64().unwrap_or(0.0);
                html.push_str(&format!(
                    "<tr><td>{}</td><td class=\"pass\">{pct:+.2}%</td></tr>\n",
                    html_escape(bench_name)
                ));
            }
            html.push_str("</table>\n");
        }

        if let Some(ref txt) = profile.report_txt {
            html.push_str(&format!(
                "<details>\n<summary>Report output (report_{}.txt)</summary>\n\
                 <pre>{}</pre>\n</details>\n",
                html_escape(&profile.name),
                html_escape(txt)
            ));
        }

        if let Some(ref crit) = profile.criterion_output {
            html.push_str(&format!(
                "<details>\n<summary>Criterion output (criterion_{}.txt)</summary>\n\
                 <pre>{}</pre>\n</details>\n",
                html_escape(&profile.name),
                html_escape(crit)
            ));
        }

        html.push_str("</div>\n");
    }

    html.push_str(section_report_footer());

    let report_path = dir.join("report.html");
    std::fs::write(&report_path, &html).map_err(|e| format!("Write bench report: {e}"))?;
    eprintln!("Generated {}", report_path.display());
    Ok(())
}

// ---- profile sections (shared by load, e2e, eval reports) ----

// -- Predicate functions: does this profile type have data? --

fn has_profile_process(pd: &serde_json::Value) -> bool {
    let summary = &pd["process_metrics"]["summary"];
    summary.is_object() && !summary.as_object().is_none_or(|m| m.is_empty())
}

fn has_profile_cpu(pd: &serde_json::Value, report_dir: &std::path::Path) -> bool {
    pd["hardware_counters"]
        .as_object()
        .is_some_and(|m| !m.is_empty())
        || pd["cpu"]["top_functions"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
        || report_dir.join("cpu_flamegraph.svg").exists()
        || report_dir.join("cpu_offcpu_flamegraph.svg").exists()
        || report_dir.join("cpu_diff_flamegraph.svg").exists()
        || report_dir.join("cpu_perf.data").exists()
        || report_dir.join("cpu_profile.json.gz").exists()
}

fn has_profile_syscalls(pd: &serde_json::Value) -> bool {
    pd["syscalls"].as_object().is_some_and(|m| !m.is_empty())
}

fn has_profile_locks(pd: &serde_json::Value) -> bool {
    pd["lock_contention"]
        .as_object()
        .is_some_and(|m| !m.is_empty())
}

fn has_profile_net(pd: &serde_json::Value) -> bool {
    pd["net_connections"]
        .as_object()
        .is_some_and(|m| !m.is_empty())
}

fn has_profile_dhat(pd: &serde_json::Value) -> bool {
    pd["dhat"]["file"].is_string()
}

fn has_profile_cgroup(pd: &serde_json::Value) -> bool {
    let cgroup = &pd["cgroup"];
    cgroup.is_object()
        && (cgroup["enforced"].as_bool() == Some(true)
            || cgroup["enforced"].as_bool() == Some(false))
}

// -- Per-type render functions --

fn render_profile_process(html: &mut String, pd: &serde_json::Value) {
    let pm = &pd["process_metrics"];
    let summary = &pm["summary"];
    let tier = pd["tier"].as_str().unwrap_or("Unknown");

    html.push_str("<div class=\"card\">\n<h2>Process Resource Usage</h2>\n");
    html.push_str(&format!(
        "<p style=\"color:#8b949e;font-size:.85rem\">Data from runtime profiling (Tier: {tier})</p>\n"
    ));

    let peak_rss = summary["peak_rss_bytes"].as_u64().unwrap_or(0);
    let avg_rss = summary["avg_rss_bytes"].as_u64().unwrap_or(0);
    let cpu_pct = summary["cpu_percent"].as_f64().unwrap_or(0.0);
    let peak_threads = summary["peak_threads"].as_u64().unwrap_or(0);
    let peak_fds = summary["peak_fds"].as_u64().unwrap_or(0);
    let vol_cs = summary["total_voluntary_ctxt_switches"]
        .as_u64()
        .unwrap_or(0);
    let nonvol_cs = summary["total_nonvoluntary_ctxt_switches"]
        .as_u64()
        .unwrap_or(0);

    let peak_rss_mb = peak_rss as f64 / 1_048_576.0;
    let avg_rss_mb = avg_rss as f64 / 1_048_576.0;

    html.push_str("<div class=\"grid\">\n");
    html.push_str(&stat_div(
        &format!("{peak_rss_mb:.1} MB"),
        "Peak RSS",
        "peak_rss_mb",
        &format!("{peak_rss_mb:.1}"),
        "MB",
    ));
    html.push_str(&stat_div(
        &format!("{avg_rss_mb:.1} MB"),
        "Avg RSS",
        "avg_rss_mb",
        &format!("{avg_rss_mb:.1}"),
        "MB",
    ));
    html.push_str(&stat_div(
        &format!("{cpu_pct:.1}%"),
        "CPU Usage",
        "cpu_percent",
        &format!("{cpu_pct:.1}"),
        "%",
    ));
    html.push_str(&stat_div(
        &format!("{peak_threads}"),
        "Peak Threads",
        "peak_threads",
        &format!("{peak_threads}"),
        "count",
    ));
    html.push_str(&stat_div(
        &format!("{peak_fds}"),
        "Peak FDs",
        "peak_fds",
        &format!("{peak_fds}"),
        "count",
    ));
    html.push_str(&stat_div(
        &format!(
            "{} / {}",
            format_number_commas(vol_cs),
            format_number_commas(nonvol_cs)
        ),
        "Ctx Switches (vol/nonvol)",
        "ctx_switches",
        &format!("{vol_cs}/{nonvol_cs}"),
        "count",
    ));

    let total_majflt = summary["total_majflt"].as_u64().unwrap_or(0);
    let total_minflt = summary["total_minflt"].as_u64().unwrap_or(0);
    let total_read_bytes = summary["total_read_bytes"].as_u64().unwrap_or(0);
    let total_write_bytes = summary["total_write_bytes"].as_u64().unwrap_or(0);

    if total_minflt > 0 || total_majflt > 0 {
        let majflt_class = if total_majflt > 100 {
            " style=\"color:#f85149\""
        } else if total_majflt > 0 {
            " style=\"color:#d29922\""
        } else {
            ""
        };
        html.push_str(&format!(
            "<div class=\"stat\" data-metric=\"page_faults\" data-value=\"{total_majflt}/{total_minflt}\" data-unit=\"count\">\
             <div class=\"value\"{}>{} / {}</div><div class=\"label\">Page Faults (major/minor)</div></div>\n",
            majflt_class,
            format_number_commas(total_majflt),
            format_number_commas(total_minflt)
        ));
    }
    if total_read_bytes > 0 || total_write_bytes > 0 {
        let read_mb = total_read_bytes as f64 / 1_048_576.0;
        let write_mb = total_write_bytes as f64 / 1_048_576.0;
        html.push_str(&stat_div(
            &format!("{read_mb:.1} / {write_mb:.1} MB"),
            "I/O Read / Write",
            "io_bytes",
            &format!("{read_mb:.1}/{write_mb:.1}"),
            "MB",
        ));
    }

    html.push_str("</div>\n");

    // Sparkline charts from snapshots
    if let Some(snapshots) = pm["snapshots"].as_array() {
        if snapshots.len() >= 2 {
            let rss_points: Vec<(f64, f64)> = snapshots
                .iter()
                .enumerate()
                .filter_map(|(i, s)| {
                    s["rss_bytes"]
                        .as_u64()
                        .map(|v| (i as f64, v as f64 / 1_048_576.0))
                })
                .collect();
            let thread_points: Vec<(f64, f64)> = snapshots
                .iter()
                .enumerate()
                .filter_map(|(i, s)| s["num_threads"].as_u64().map(|v| (i as f64, v as f64)))
                .collect();
            let fd_points: Vec<(f64, f64)> = snapshots
                .iter()
                .enumerate()
                .filter_map(|(i, s)| s["fd_count"].as_u64().map(|v| (i as f64, v as f64)))
                .collect();

            let cpu_points: Vec<(f64, f64)> = snapshots
                .iter()
                .enumerate()
                .filter_map(|(i, s)| s["cpu_percent"].as_f64().map(|v| (i as f64, v)))
                .collect();
            let majflt_points: Vec<(f64, f64)> = snapshots
                .iter()
                .enumerate()
                .filter_map(|(i, s)| s["majflt"].as_u64().map(|v| (i as f64, v as f64)))
                .collect();

            html.push_str(
                "<div style=\"display:flex;gap:1rem;flex-wrap:wrap;margin-top:.75rem\">\n",
            );
            html.push_str(&render_sparkline_svg(
                &rss_points,
                "RSS (MB)",
                "#58a6ff",
                240,
                80,
            ));
            html.push_str(&render_sparkline_svg(
                &cpu_points,
                "CPU %",
                "#f0883e",
                240,
                80,
            ));
            html.push_str(&render_sparkline_svg(
                &thread_points,
                "Threads",
                "#3fb950",
                240,
                80,
            ));
            html.push_str(&render_sparkline_svg(&fd_points, "FDs", "#d29922", 240, 80));
            if majflt_points.iter().any(|(_, v)| *v > 0.0) {
                html.push_str(&render_sparkline_svg(
                    &majflt_points,
                    "Major Faults",
                    "#f85149",
                    240,
                    80,
                ));
            }
            html.push_str("</div>\n");
        }
    }
    html.push_str("</div>\n");
}

fn render_profile_cpu(html: &mut String, pd: &serde_json::Value, report_dir: &std::path::Path) {
    // Hardware counters (perf stat)
    if let Some(hw) = pd["hardware_counters"].as_object() {
        if !hw.is_empty() {
            html.push_str("<div class=\"card\">\n<h2>Hardware Counters</h2>\n");
            html.push_str("<p style=\"color:#8b949e;font-size:.85rem\">From <code>perf stat</code> &mdash; microarchitectural performance</p>\n");
            html.push_str("<div class=\"grid\">\n");

            let ipc = hw.get("ipc").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let ipc_color = if ipc < 1.0 {
                "#f85149"
            } else if ipc < 2.0 {
                "#d29922"
            } else {
                "#3fb950"
            };
            html.push_str(&format!(
                "<div class=\"stat\" data-metric=\"ipc\" data-value=\"{ipc:.2}\" data-unit=\"insn/cycle\">\
                 <div class=\"value\" style=\"color:{ipc_color}\">{ipc:.2}</div><div class=\"label\">IPC (insn/cycle)</div></div>\n"
            ));

            let cache_miss_pct = hw
                .get("cache_miss_percent")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let cm_color = if cache_miss_pct > 5.0 {
                "#f85149"
            } else if cache_miss_pct > 2.0 {
                "#d29922"
            } else {
                "#3fb950"
            };
            html.push_str(&format!(
                "<div class=\"stat\" data-metric=\"cache_miss_percent\" data-value=\"{cache_miss_pct:.2}\" data-unit=\"%\">\
                 <div class=\"value\" style=\"color:{cm_color}\">{cache_miss_pct:.2}%</div><div class=\"label\">Cache Miss Rate</div></div>\n"
            ));

            let branch_misses = hw
                .get("branch_misses")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            html.push_str(&stat_div(
                &format_number_commas(branch_misses),
                "Branch Misses",
                "branch_misses",
                &format!("{branch_misses}"),
                "count",
            ));

            let page_faults_hw = hw.get("page_faults").and_then(|v| v.as_u64()).unwrap_or(0);
            html.push_str(&stat_div(
                &format_number_commas(page_faults_hw),
                "Page Faults",
                "hw_page_faults",
                &format!("{page_faults_hw}"),
                "count",
            ));

            let cycles = hw.get("cycles").and_then(|v| v.as_u64()).unwrap_or(0);
            let instructions = hw.get("instructions").and_then(|v| v.as_u64()).unwrap_or(0);
            html.push_str(&stat_div(
                &format_number_commas(cycles),
                "Cycles",
                "cycles",
                &format!("{cycles}"),
                "count",
            ));
            html.push_str(&stat_div(
                &format_number_commas(instructions),
                "Instructions",
                "instructions",
                &format!("{instructions}"),
                "count",
            ));

            html.push_str("</div>\n</div>\n");
        }
    }

    // CPU profiling — top functions
    if let Some(top_fns) = pd["cpu"]["top_functions"].as_array() {
        if !top_fns.is_empty() {
            html.push_str("<div class=\"card\">\n<h2>CPU Profiling &mdash; Top Functions</h2>\n");
            html.push_str(
                "<table>\n<tr><th>#</th><th>Function</th><th>%</th><th>Samples</th></tr>\n",
            );
            for (i, f) in top_fns.iter().take(10).enumerate() {
                let fname = f["name"].as_str().unwrap_or("unknown");
                let pct = f["percent"].as_f64().unwrap_or(0.0);
                let samples = f["samples"].as_u64().unwrap_or(0);
                html.push_str(&format!(
                    "<tr><td>{}</td><td>{}</td><td>{pct:.2}%</td><td>{}</td></tr>\n",
                    i + 1,
                    html_escape(fname),
                    format_number_commas(samples)
                ));
            }
            html.push_str("</table>\n");

            html.push_str("</div>\n");
        }
    }

    // Flamegraph SVG (interactive — generated by inferno)
    if report_dir.join("cpu_flamegraph.svg").exists() {
        html.push_str("<div class=\"card\">\n<h2>Flamegraph</h2>\n");
        html.push_str(
            "<p><a href=\"cpu_flamegraph.svg\" target=\"_blank\" \
             style=\"color:var(--link)\">Open Flamegraph (SVG)</a></p>\n",
        );
        html.push_str(
            "<p style=\"color:#8b949e;font-size:.8rem\">Interactive &mdash; \
             click to zoom, right-click to reset</p>\n",
        );
        html.push_str("</div>\n");
    }

    // Off-CPU Flamegraph
    if report_dir.join("cpu_offcpu_flamegraph.svg").exists() {
        html.push_str("<div class=\"card\">\n<h2>Off-CPU Flamegraph</h2>\n");
        html.push_str(
            "<p><a href=\"cpu_offcpu_flamegraph.svg\" target=\"_blank\" \
             style=\"color:var(--link)\">Open Off-CPU Flamegraph (SVG)</a></p>\n",
        );
        html.push_str(
            "<p style=\"color:#8b949e;font-size:.8rem\">Shows where the process waits &mdash; \
             I/O, locks, sleep. Color: I/O palette. Click to zoom, right-click to reset.</p>\n",
        );
        html.push_str("</div>\n");
    }

    // Differential Flamegraph
    if report_dir.join("cpu_diff_flamegraph.svg").exists() {
        html.push_str("<div class=\"card\">\n<h2>Differential Flamegraph</h2>\n");
        html.push_str(
            "<p><a href=\"cpu_diff_flamegraph.svg\" target=\"_blank\" \
             style=\"color:var(--link)\">Open Diff Flamegraph (SVG)</a></p>\n",
        );
        html.push_str(
            "<p style=\"color:#8b949e;font-size:.8rem\"><span style=\"color:#f85149\">Red = regression</span> / \
             <span style=\"color:#58a6ff\">Blue = improvement</span> vs. baseline run</p>\n",
        );
        html.push_str("</div>\n");
    }

    // Hotspot (perf.data viewer)
    if report_dir.join("cpu_perf.data").exists() {
        let perf_path = report_dir.join("cpu_perf.data");
        let abs_path = std::fs::canonicalize(&perf_path).unwrap_or_else(|_| perf_path.clone());
        html.push_str("<div class=\"card\">\n<h2>Hotspot</h2>\n");
        html.push_str(&format!(
            "<p>Open in <a href=\"https://github.com/KDAB/hotspot\" target=\"_blank\" \
             style=\"color:var(--link)\">Hotspot</a> for interactive call-tree, timeline, \
             and flame graph analysis:</p>\n\
             <pre style=\"background:#161b22;padding:.5em;border-radius:4px;overflow-x:auto\">\
             hotspot {}</pre>\n",
            html_escape(&abs_path.display().to_string())
        ));
        html.push_str("</div>\n");
    }

    // Legacy: Firefox Profiler link for older runs that have cpu_profile.json.gz
    if report_dir.join("cpu_profile.json.gz").exists() {
        html.push_str(
            "<div class=\"card\">\n\
             <p><a href=\"cpu_profile.json.gz\" style=\"color:var(--link)\">Download CPU Profile</a> · \
             <a href=\"https://profiler.firefox.com/\" target=\"_blank\" style=\"color:var(--link)\">Open Firefox Profiler</a></p>\n\
             <p style=\"color:#8b949e;font-size:.8rem\">Load the downloaded .json.gz via &ldquo;Load a profile from file&rdquo; in Firefox Profiler</p>\n\
             </div>\n",
        );
    }
}

fn render_profile_syscalls(html: &mut String, pd: &serde_json::Value) {
    let syscalls = pd["syscalls"].as_object().unwrap();
    html.push_str("<div class=\"card\">\n<h2>Syscall Breakdown</h2>\n");
    html.push_str(
        "<table>\n<tr><th>Syscall</th><th>Count</th><th>Avg Latency</th><th>Details</th></tr>\n",
    );
    for (name, info) in syscalls {
        let count = info["count"].as_u64().unwrap_or(0);
        let avg_lat = info["avg_latency_us"].as_f64().unwrap_or(0.0);
        let details = info["details"].as_str().unwrap_or("");
        html.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
            html_escape(name),
            format_number_commas(count),
            format_latency(avg_lat / 1e6),
            html_escape(details)
        ));
    }
    html.push_str("</table>\n</div>\n");
}

fn render_profile_locks(html: &mut String, pd: &serde_json::Value) {
    let lock = pd["lock_contention"].as_object().unwrap();
    html.push_str("<div class=\"card\">\n<h2>Lock Contention</h2>\n");
    html.push_str("<p style=\"color:#8b949e;font-size:.85rem\">From <code>bpftrace</code> &mdash; futex wait/wake tracing</p>\n");
    html.push_str("<div class=\"grid\">\n");

    let wait_count = lock
        .get("futex_wait_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let wake_count = lock
        .get("futex_wake_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let total_wait_us = lock
        .get("total_wait_us")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let avg_wait_us = lock
        .get("avg_wait_us")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    html.push_str(&stat_div(
        &format_number_commas(wait_count),
        "Futex Waits",
        "futex_waits",
        &format!("{wait_count}"),
        "count",
    ));
    html.push_str(&stat_div(
        &format_number_commas(wake_count),
        "Futex Wakes",
        "futex_wakes",
        &format!("{wake_count}"),
        "count",
    ));
    html.push_str(&stat_div(
        &format_latency(total_wait_us as f64 / 1e6),
        "Total Wait",
        "futex_total_wait_us",
        &format!("{total_wait_us}"),
        "us",
    ));
    html.push_str(&stat_div(
        &format_latency(avg_wait_us / 1e6),
        "Avg Wait",
        "futex_avg_wait_us",
        &format!("{avg_wait_us:.1}"),
        "us",
    ));

    html.push_str("</div>\n</div>\n");
}

fn render_profile_net(html: &mut String, pd: &serde_json::Value) {
    let net = &pd["net_connections"];
    let accept_count = net["accept_count"].as_u64().unwrap_or(0);
    let accept_avg_lat = net["accept_avg_latency_us"].as_u64().unwrap_or(0);
    let connect_count = net["connect_count"].as_u64().unwrap_or(0);
    let connect_avg_lat = net["connect_avg_latency_us"].as_u64().unwrap_or(0);
    let close_count = net["close_count"].as_u64().unwrap_or(0);
    let close_accept_ratio = net["close_accept_ratio"].as_u64().unwrap_or(0);

    html.push_str("<div class=\"card\">\n<h2>TCP Connection Lifecycle</h2>\n");
    html.push_str("<p style=\"color:#8b949e;font-size:.85rem\">From <code>bpftrace</code> &mdash; accept4/connect/close tracing</p>\n");
    html.push_str("<div class=\"grid\">\n");
    html.push_str(&stat_div(
        &format_number_commas(accept_count),
        "Inbound (accept4)",
        "accept_count",
        &format!("{accept_count}"),
        "count",
    ));
    html.push_str(&stat_div(
        &format!("{accept_avg_lat} \u{00B5}s"),
        "Accept Avg Latency",
        "accept_avg_latency_us",
        &format!("{accept_avg_lat}"),
        "us",
    ));
    html.push_str(&stat_div(
        &format_number_commas(connect_count),
        "Outbound (connect)",
        "connect_count",
        &format!("{connect_count}"),
        "count",
    ));
    html.push_str(&stat_div(
        &format!("{connect_avg_lat} \u{00B5}s"),
        "Connect Avg Latency",
        "connect_avg_latency_us",
        &format!("{connect_avg_lat}"),
        "us",
    ));
    html.push_str(&stat_div(
        &format_number_commas(close_count),
        "Close",
        "close_count",
        &format!("{close_count}"),
        "count",
    ));
    html.push_str(&stat_div(
        &format!("{close_accept_ratio}"),
        "Close/Accept Ratio",
        "close_accept_ratio",
        &format!("{close_accept_ratio}"),
        "ratio",
    ));
    html.push_str("</div>\n</div>\n");
}

fn render_profile_dhat(html: &mut String, pd: &serde_json::Value) {
    let size = pd["dhat"]["size_bytes"].as_u64().unwrap_or(0);
    html.push_str("<div class=\"card\">\n<h2>DHAT Heap Profile</h2>\n");

    // Show summary stats if available
    let summary = &pd["dhat"]["summary"];
    if summary.is_object() {
        let total_alloc = summary["total_bytes_allocated"].as_u64().unwrap_or(0);
        let total_blocks = summary["total_blocks"].as_u64().unwrap_or(0);
        let peak_heap = summary["peak_heap_bytes"].as_u64().unwrap_or(0);
        let bytes_exit = summary["bytes_at_exit"].as_u64().unwrap_or(0);

        html.push_str("<div class=\"grid\">\n");
        html.push_str(&stat_div(
            &format_bytes_human(total_alloc),
            "Total Allocated",
            "dhat_total_allocated",
            &format!("{total_alloc}"),
            "bytes",
        ));
        html.push_str(&stat_div(
            &format_number_commas(total_blocks),
            "Total Blocks",
            "dhat_total_blocks",
            &format!("{total_blocks}"),
            "count",
        ));
        html.push_str(&stat_div(
            &format_bytes_human(peak_heap),
            "Peak Heap",
            "dhat_peak_heap",
            &format!("{peak_heap}"),
            "bytes",
        ));
        html.push_str(&stat_div(
            &format_bytes_human(bytes_exit),
            "Bytes at Exit",
            "dhat_bytes_at_exit",
            &format!("{bytes_exit}"),
            "bytes",
        ));
        html.push_str("</div>\n");

        // Top allocation sites table
        if let Some(sites) = summary["top_allocation_sites"].as_array() {
            if !sites.is_empty() {
                html.push_str("<h3 style=\"margin-top:1rem\">Top Allocation Sites</h3>\n");
                html.push_str(
                    "<table>\n<tr><th>#</th><th>Function</th><th>Total Bytes</th><th>Blocks</th></tr>\n",
                );
                for (i, site) in sites.iter().take(10).enumerate() {
                    let func = site["function"].as_str().unwrap_or("<unknown>");
                    let tb = site["total_bytes"].as_u64().unwrap_or(0);
                    let blocks = site["blocks"].as_u64().unwrap_or(0);
                    html.push_str(&format!(
                        "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
                        i + 1,
                        html_escape(func),
                        format_bytes_human(tb),
                        format_number_commas(blocks),
                    ));
                }
                html.push_str("</table>\n");
            }
        }
    } else {
        html.push_str(&format!(
            "<p>Profile size: <strong>{:.1} KB</strong></p>\n",
            size as f64 / 1024.0
        ));
    }

    html.push_str(
        "<p style=\"margin-top:.8rem\"><a href=\"dhat-heap.json\" style=\"color:var(--link)\">Download dhat-heap.json</a> · \
         <a href=\"https://nnethercote.github.io/dh_view/dh_view.html\" target=\"_blank\" style=\"color:var(--link)\">Open DHAT Viewer</a></p>\n"
    );
    html.push_str(
        "<p style=\"color:#8b949e;font-size:.8rem\">Load the downloaded dhat-heap.json via the &ldquo;Load&hellip;&rdquo; button in the DHAT viewer</p>\n"
    );
    html.push_str("</div>\n");
}

fn render_profile_cgroup(html: &mut String, pd: &serde_json::Value) {
    let cgroup = &pd["cgroup"];
    if cgroup["enforced"].as_bool() == Some(true) {
        let profile_name = cgroup["profile"]["name"].as_str().unwrap_or("unknown");
        let cpu_max = cgroup["profile"]["cpu_max"].as_str().unwrap_or("?");
        let mem_max = cgroup["profile"]["memory_max_bytes"].as_u64().unwrap_or(0);
        let mem_max_mb = mem_max as f64 / 1_048_576.0;

        let mem_peak = cgroup["memory"]["peak_bytes"].as_u64().unwrap_or(0);
        let mem_peak_mb = mem_peak as f64 / 1_048_576.0;
        let mem_util = cgroup["memory"]["utilization_percent"]
            .as_f64()
            .unwrap_or(0.0);
        let oom_events = cgroup["memory"]["oom_events"].as_u64().unwrap_or(0);
        let oom_kills = cgroup["memory"]["oom_kill_events"].as_u64().unwrap_or(0);

        let nr_periods = cgroup["cpu"]["nr_periods"].as_u64().unwrap_or(0);
        let nr_throttled = cgroup["cpu"]["nr_throttled"].as_u64().unwrap_or(0);
        let throttle_pct = cgroup["cpu"]["throttle_percent"].as_f64().unwrap_or(0.0);
        let usage_usec = cgroup["cpu"]["usage_usec"].as_u64().unwrap_or(0);
        let usage_ms = usage_usec as f64 / 1000.0;

        html.push_str("<div class=\"card\">\n<h2>Resource Limits (cgroup v2)</h2>\n");
        html.push_str(&format!(
            "<p style=\"color:#8b949e;font-size:.85rem\">Profile: <strong>{profile_name}</strong> \
             &mdash; cpu.max={cpu_max}, memory.max={mem_max_mb:.0} MB</p>\n"
        ));
        html.push_str("<div class=\"grid\">\n");

        // Memory utilization
        let mem_util_color = if oom_kills > 0 {
            "#f85149"
        } else if mem_util > 90.0 {
            "#d29922"
        } else {
            "#3fb950"
        };
        html.push_str(&format!(
            "<div class=\"stat\" data-metric=\"cgroup_mem_util\" data-value=\"{mem_util:.1}\" data-unit=\"%\">\
             <div class=\"value\" style=\"color:{mem_util_color}\">{mem_util:.1}%</div>\
             <div class=\"label\">Memory Utilization</div></div>\n"
        ));

        html.push_str(&stat_div(
            &format!("{mem_peak_mb:.1} / {mem_max_mb:.0} MB"),
            "Peak / Limit",
            "cgroup_mem_peak",
            &format!("{mem_peak_mb:.1}"),
            "MB",
        ));

        // OOM events
        if oom_kills > 0 {
            html.push_str(&format!(
                "<div class=\"stat\" data-metric=\"cgroup_oom_kills\" data-value=\"{oom_kills}\" data-unit=\"count\">\
                 <div class=\"value\" style=\"color:#f85149\">{oom_kills}</div>\
                 <div class=\"label\">OOM Kills</div></div>\n"
            ));
        }
        if oom_events > 0 {
            html.push_str(&stat_div(
                &format!("{oom_events}"),
                "OOM Events",
                "cgroup_oom_events",
                &format!("{oom_events}"),
                "count",
            ));
        }

        // CPU throttling
        let throttle_color = if throttle_pct > 25.0 {
            "#d29922"
        } else {
            "#3fb950"
        };
        html.push_str(&format!(
            "<div class=\"stat\" data-metric=\"cgroup_cpu_throttle\" data-value=\"{throttle_pct:.1}\" data-unit=\"%\">\
             <div class=\"value\" style=\"color:{throttle_color}\">{throttle_pct:.1}%</div>\
             <div class=\"label\">CPU Throttled</div></div>\n"
        ));

        html.push_str(&stat_div(
            &format!("{nr_throttled} / {nr_periods}"),
            "Throttled / Total Periods",
            "cgroup_throttle_periods",
            &format!("{nr_throttled}/{nr_periods}"),
            "count",
        ));
        html.push_str(&stat_div(
            &format!("{usage_ms:.0} ms"),
            "CPU Usage",
            "cgroup_cpu_usage",
            &format!("{usage_ms:.0}"),
            "ms",
        ));

        html.push_str("</div>\n</div>\n");
    } else {
        // cgroup was requested but not enforced
        let error = cgroup["error"].as_str().unwrap_or("unknown");
        html.push_str("<div class=\"card\">\n<h2>Resource Limits (cgroup v2)</h2>\n");
        html.push_str(&format!(
            "<p style=\"color:#d29922\">Not enforced: {}</p>\n",
            html_escape(error)
        ));
        html.push_str("</div>\n");
    }
}

// -- Summary card functions (compact cards for index page) --

fn render_profile_process_summary(html: &mut String, pd: &serde_json::Value) {
    let summary = &pd["process_metrics"]["summary"];
    let peak_rss = summary["peak_rss_bytes"].as_u64().unwrap_or(0);
    let peak_rss_mb = peak_rss as f64 / 1_048_576.0;
    let cpu_pct = summary["cpu_percent"].as_f64().unwrap_or(0.0);
    let peak_threads = summary["peak_threads"].as_u64().unwrap_or(0);

    html.push_str("<div class=\"card\">\n<h2>Process Resources</h2>\n<div class=\"grid\">\n");
    html.push_str(&stat_div(
        &format!("{peak_rss_mb:.1} MB"),
        "Peak RSS",
        "peak_rss_mb",
        &format!("{peak_rss_mb:.1}"),
        "MB",
    ));
    html.push_str(&stat_div(
        &format!("{cpu_pct:.1}%"),
        "CPU %",
        "cpu_percent",
        &format!("{cpu_pct:.1}"),
        "%",
    ));
    html.push_str(&stat_div(
        &format!("{peak_threads}"),
        "Peak Threads",
        "peak_threads",
        &format!("{peak_threads}"),
        "count",
    ));
    html.push_str("</div>\n<a class=\"card-link\" href=\"profile_process.html\">View details &rarr;</a>\n</div>\n");
}

fn render_profile_cpu_summary(html: &mut String, pd: &serde_json::Value) {
    html.push_str("<div class=\"card\">\n<h2>CPU Profiling</h2>\n<div class=\"grid\">\n");
    let ipc = pd["hardware_counters"]
        .as_object()
        .and_then(|hw| hw.get("ipc"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let cache_miss_pct = pd["hardware_counters"]
        .as_object()
        .and_then(|hw| hw.get("cache_miss_percent"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let top_fn_name = pd["cpu"]["top_functions"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|f| f["name"].as_str())
        .unwrap_or("-");

    if ipc > 0.0 {
        html.push_str(&stat_div(
            &format!("{ipc:.2}"),
            "IPC",
            "ipc",
            &format!("{ipc:.2}"),
            "insn/cycle",
        ));
    }
    if cache_miss_pct > 0.0 || ipc > 0.0 {
        html.push_str(&stat_div(
            &format!("{cache_miss_pct:.2}%"),
            "Cache Miss %",
            "cache_miss_percent",
            &format!("{cache_miss_pct:.2}"),
            "%",
        ));
    }
    if top_fn_name != "-" {
        let truncated: String = top_fn_name.chars().take(30).collect();
        html.push_str(&format!(
            "<div class=\"stat\"><div class=\"value\" style=\"font-size:0.85rem\">{}</div><div class=\"label\">Top Function</div></div>\n",
            html_escape(&truncated)
        ));
    }
    html.push_str("</div>\n<a class=\"card-link\" href=\"profile_cpu.html\">View details &rarr;</a>\n</div>\n");
}

fn render_profile_syscalls_summary(html: &mut String, pd: &serde_json::Value) {
    let syscalls = pd["syscalls"].as_object().unwrap();
    let num_types = syscalls.len();
    let hottest = syscalls
        .iter()
        .max_by_key(|(_, info)| info["count"].as_u64().unwrap_or(0));

    html.push_str("<div class=\"card\">\n<h2>Syscall Breakdown</h2>\n<div class=\"grid\">\n");
    html.push_str(&stat_div(
        &format!("{num_types}"),
        "Syscall Types",
        "syscall_types",
        &format!("{num_types}"),
        "count",
    ));
    if let Some((name, info)) = hottest {
        let count = info["count"].as_u64().unwrap_or(0);
        html.push_str(&format!(
            "<div class=\"stat\"><div class=\"value\">{}</div><div class=\"label\">Hottest: {}</div></div>\n",
            format_number_commas(count),
            html_escape(name)
        ));
    }
    html.push_str("</div>\n<a class=\"card-link\" href=\"profile_syscalls.html\">View details &rarr;</a>\n</div>\n");
}

fn render_profile_locks_summary(html: &mut String, pd: &serde_json::Value) {
    let lock = pd["lock_contention"].as_object().unwrap();
    let wait_count = lock
        .get("futex_wait_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let avg_wait_us = lock
        .get("avg_wait_us")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    html.push_str("<div class=\"card\">\n<h2>Lock Contention</h2>\n<div class=\"grid\">\n");
    html.push_str(&stat_div(
        &format_number_commas(wait_count),
        "Futex Waits",
        "futex_waits",
        &format!("{wait_count}"),
        "count",
    ));
    html.push_str(&stat_div(
        &format_latency(avg_wait_us / 1e6),
        "Avg Wait",
        "futex_avg_wait_us",
        &format!("{avg_wait_us:.1}"),
        "us",
    ));
    html.push_str("</div>\n<a class=\"card-link\" href=\"profile_locks.html\">View details &rarr;</a>\n</div>\n");
}

fn render_profile_net_summary(html: &mut String, pd: &serde_json::Value) {
    let net = &pd["net_connections"];
    let accept_count = net["accept_count"].as_u64().unwrap_or(0);
    let connect_count = net["connect_count"].as_u64().unwrap_or(0);
    let connect_avg_lat = net["connect_avg_latency_us"].as_u64().unwrap_or(0);

    html.push_str("<div class=\"card\">\n<h2>TCP Connections</h2>\n<div class=\"grid\">\n");
    html.push_str(&stat_div(
        &format_number_commas(accept_count),
        "Inbound",
        "accept_count",
        &format!("{accept_count}"),
        "count",
    ));
    html.push_str(&stat_div(
        &format_number_commas(connect_count),
        "Outbound",
        "connect_count",
        &format!("{connect_count}"),
        "count",
    ));
    html.push_str(&stat_div(
        &format!("{connect_avg_lat} \u{00B5}s"),
        "Connect Avg Latency",
        "connect_avg_latency_us",
        &format!("{connect_avg_lat}"),
        "us",
    ));
    html.push_str("</div>\n<a class=\"card-link\" href=\"profile_net.html\">View details &rarr;</a>\n</div>\n");
}

fn render_profile_dhat_summary(html: &mut String, pd: &serde_json::Value) {
    let summary = &pd["dhat"]["summary"];
    let total_alloc = summary["total_bytes_allocated"].as_u64().unwrap_or(0);
    let peak_heap = summary["peak_heap_bytes"].as_u64().unwrap_or(0);

    html.push_str("<div class=\"card\">\n<h2>DHAT Heap Profile</h2>\n<div class=\"grid\">\n");
    html.push_str(&stat_div(
        &format_bytes_human(total_alloc),
        "Total Allocated",
        "dhat_total_allocated",
        &format!("{total_alloc}"),
        "bytes",
    ));
    html.push_str(&stat_div(
        &format_bytes_human(peak_heap),
        "Peak Heap",
        "dhat_peak_heap",
        &format!("{peak_heap}"),
        "bytes",
    ));
    html.push_str("</div>\n<a class=\"card-link\" href=\"profile_dhat.html\">View details &rarr;</a>\n</div>\n");
}

fn render_profile_cgroup_summary(html: &mut String, pd: &serde_json::Value) {
    let cgroup = &pd["cgroup"];
    html.push_str("<div class=\"card\">\n<h2>Resource Limits (cgroup v2)</h2>\n");
    if cgroup["enforced"].as_bool() == Some(true) {
        let mem_util = cgroup["memory"]["utilization_percent"]
            .as_f64()
            .unwrap_or(0.0);
        let throttle_pct = cgroup["cpu"]["throttle_percent"].as_f64().unwrap_or(0.0);
        html.push_str("<div class=\"grid\">\n");
        html.push_str(&stat_div(
            &format!("{mem_util:.1}%"),
            "Memory Util",
            "cgroup_mem_util",
            &format!("{mem_util:.1}"),
            "%",
        ));
        html.push_str(&stat_div(
            &format!("{throttle_pct:.1}%"),
            "CPU Throttled",
            "cgroup_cpu_throttle",
            &format!("{throttle_pct:.1}"),
            "%",
        ));
        html.push_str("</div>\n");
    } else {
        let error = cgroup["error"].as_str().unwrap_or("unknown");
        html.push_str(&format!(
            "<p style=\"color:#d29922\">Not enforced: {}</p>\n",
            html_escape(error)
        ));
    }
    html.push_str(
        "<a class=\"card-link\" href=\"profile_cgroup.html\">View details &rarr;</a>\n</div>\n",
    );
}

// -- Index and sub-report generation --

/// Render the profile index page with summary cards linking to sub-reports.
fn render_profile_index(html: &mut String, pd: &serde_json::Value, report_dir: &std::path::Path) {
    if has_profile_process(pd) {
        render_profile_process_summary(html, pd);
    }
    if has_profile_cpu(pd, report_dir) {
        render_profile_cpu_summary(html, pd);
    }
    if has_profile_syscalls(pd) {
        render_profile_syscalls_summary(html, pd);
    }
    if has_profile_locks(pd) {
        render_profile_locks_summary(html, pd);
    }
    if has_profile_net(pd) {
        render_profile_net_summary(html, pd);
    }
    if has_profile_dhat(pd) {
        render_profile_dhat_summary(html, pd);
    }
    if has_profile_cgroup(pd) {
        render_profile_cgroup_summary(html, pd);
    }
}

/// Generate a standalone sub-report HTML file for a single profile type.
fn generate_profile_sub_report(
    report_dir: &std::path::Path,
    filename: &str,
    title: &str,
    stage_name: &str,
    pd: &serde_json::Value,
    render_fn: fn(&mut String, &serde_json::Value, &std::path::Path),
) -> Result<(), String> {
    let full_title = format!("{title} — {stage_name}");
    let mut html = String::with_capacity(4_000);
    html.push_str(&section_report_head(&full_title));
    html.push_str(
        "<a class=\"nav-back\" href=\"profile_report.html\">Back to Profile Overview</a>\n",
    );
    html.push_str(&format!("<h1>{}</h1>\n", html_escape(&full_title)));
    render_fn(&mut html, pd, report_dir);
    html.push_str(section_report_footer());

    let report_path = report_dir.join(filename);
    std::fs::write(&report_path, &html).map_err(|e| format!("Write {filename}: {e}"))?;
    eprintln!("Generated {}", report_path.display());
    Ok(())
}

/// Thin dispatcher — renders all profile sections inline (used by load report).
fn render_profile_sections(
    html: &mut String,
    pd: &serde_json::Value,
    report_dir: &std::path::Path,
) {
    if has_profile_process(pd) {
        render_profile_process(html, pd);
    }
    if has_profile_cpu(pd, report_dir) {
        render_profile_cpu(html, pd, report_dir);
    }
    if has_profile_syscalls(pd) {
        render_profile_syscalls(html, pd);
    }
    if has_profile_locks(pd) {
        render_profile_locks(html, pd);
    }
    if has_profile_net(pd) {
        render_profile_net(html, pd);
    }
    if has_profile_dhat(pd) {
        render_profile_dhat(html, pd);
    }
    if has_profile_cgroup(pd) {
        render_profile_cgroup(html, pd);
    }
}

// Wrapper adapters for generate_profile_sub_report (unifies 2-arg and 3-arg render fns)
fn render_profile_process_wrap(html: &mut String, pd: &serde_json::Value, _dir: &std::path::Path) {
    render_profile_process(html, pd);
}
fn render_profile_syscalls_wrap(html: &mut String, pd: &serde_json::Value, _dir: &std::path::Path) {
    render_profile_syscalls(html, pd);
}
fn render_profile_locks_wrap(html: &mut String, pd: &serde_json::Value, _dir: &std::path::Path) {
    render_profile_locks(html, pd);
}
fn render_profile_net_wrap(html: &mut String, pd: &serde_json::Value, _dir: &std::path::Path) {
    render_profile_net(html, pd);
}
fn render_profile_dhat_wrap(html: &mut String, pd: &serde_json::Value, _dir: &std::path::Path) {
    render_profile_dhat(html, pd);
}
fn render_profile_cgroup_wrap(html: &mut String, pd: &serde_json::Value, _dir: &std::path::Path) {
    render_profile_cgroup(html, pd);
}

/// Generate a standalone profile report for a stage that has its own profile_results.json.
/// Produces an index page (profile_report.html) with summary cards, plus individual
/// sub-report files (profile_process.html, profile_cpu.html, etc.) for each profile type.
fn generate_stage_profile_report(
    stage_dir: &std::path::Path,
    stage_name: &str,
) -> Result<(), String> {
    let profile_path = stage_dir.join("profile_results.json");
    let content = std::fs::read_to_string(&profile_path)
        .map_err(|e| format!("Read profile_results.json: {e}"))?;
    let pd: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("Parse profile_results.json: {e}"))?;

    let pm = &pd["process_metrics"]["summary"];
    if !pm.is_object() || pm.as_object().is_none_or(|m| m.is_empty()) {
        return Err("No process metrics in profile_results.json".into());
    }

    // Generate sub-reports for each type that has data
    type SubReportEntry = (
        fn(&serde_json::Value, &std::path::Path) -> bool,
        &'static str,
        &'static str,
        fn(&mut String, &serde_json::Value, &std::path::Path),
    );
    let sub_reports: &[SubReportEntry] = &[
        (
            |pd, _dir| has_profile_process(pd),
            "profile_process.html",
            "Process Resources",
            render_profile_process_wrap,
        ),
        (
            |pd, dir| has_profile_cpu(pd, dir),
            "profile_cpu.html",
            "CPU Profiling",
            render_profile_cpu,
        ),
        (
            |pd, _dir| has_profile_syscalls(pd),
            "profile_syscalls.html",
            "Syscall Breakdown",
            render_profile_syscalls_wrap,
        ),
        (
            |pd, _dir| has_profile_locks(pd),
            "profile_locks.html",
            "Lock Contention",
            render_profile_locks_wrap,
        ),
        (
            |pd, _dir| has_profile_net(pd),
            "profile_net.html",
            "TCP Connections",
            render_profile_net_wrap,
        ),
        (
            |pd, _dir| has_profile_dhat(pd),
            "profile_dhat.html",
            "DHAT Heap Profile",
            render_profile_dhat_wrap,
        ),
        (
            |pd, _dir| has_profile_cgroup(pd),
            "profile_cgroup.html",
            "Resource Limits",
            render_profile_cgroup_wrap,
        ),
    ];

    for (has_fn, filename, title, render_fn) in sub_reports {
        if has_fn(&pd, stage_dir) {
            generate_profile_sub_report(stage_dir, filename, title, stage_name, &pd, *render_fn)?;
        }
    }

    // Generate index page (profile_report.html)
    let title = format!("Runtime Profiling — {stage_name}");
    let mut html = String::with_capacity(4_000);
    html.push_str(&section_report_head(&title));
    html.push_str(&format!(
        "<h1>{}</h1>\n<p>Process resource metrics collected via /proc sampling</p>\n",
        html_escape(&title)
    ));

    render_profile_index(&mut html, &pd, stage_dir);

    html.push_str(section_report_footer());

    let report_path = stage_dir.join("profile_report.html");
    std::fs::write(&report_path, &html).map_err(|e| format!("Write profile report: {e}"))?;
    eprintln!("Generated {}", report_path.display());
    Ok(())
}

// ---- load report helper renderers ----

/// Render assessment cards for the load test report.
fn render_load_assessments(
    html: &mut String,
    benchmarks: &[serde_json::Value],
    proxy_stats: &Option<serde_json::Value>,
    total_errors: u64,
) {
    let mut assessments: Vec<(&str, String, String)> = Vec::new(); // (level, metric, message)

    for b in benchmarks {
        let name = b["name"].as_str().unwrap_or("unknown");
        let is_read = name.contains("query") || name.contains("health") || name.contains("stats");
        let kind = if is_read { "read" } else { "write" };
        let (p99_warn, p99_fail) = if is_read {
            (READ_P99_WARN_MS, READ_P99_FAIL_MS)
        } else {
            (WRITE_P99_WARN_MS, WRITE_P99_FAIL_MS)
        };

        let p99 = b["raw_metrics"]["latency"]["percentiles"]["p99"]
            .as_f64()
            .unwrap_or(0.0);
        let p99_ms = p99 * 1000.0;
        let p999 = b["raw_metrics"]["latency"]["percentiles"]["p99.9"]
            .as_f64()
            .unwrap_or(0.0);
        let p999_ms = p999 * 1000.0;
        let iters = b["raw_metrics"]["summary"]["iters"]["total"]
            .as_u64()
            .unwrap_or(0);

        let mut errors: u64 = 0;
        if let Some(status_map) = b["raw_metrics"]["status"].as_object() {
            for (key, count) in status_map {
                if !key.starts_with("Success") {
                    errors += count.as_u64().unwrap_or(0);
                }
            }
        }
        let error_rate = if iters > 0 {
            errors as f64 / iters as f64
        } else {
            0.0
        };

        if error_rate > 0.5 {
            assessments.push((
                "FAIL",
                format!("{name} error_rate"),
                format!("{:.0}% error rate", error_rate * 100.0),
            ));
        } else if error_rate > 0.0 {
            assessments.push((
                "WARN",
                format!("{name} error_rate"),
                format!("{:.1}% error rate", error_rate * 100.0),
            ));
        }

        if p99_ms > p99_fail {
            assessments.push((
                "FAIL",
                format!("{name} p99"),
                format!("p99 {p99_ms:.1}ms > {kind} threshold {p99_fail}ms"),
            ));
        } else if p99_ms > p99_warn {
            assessments.push((
                "WARN",
                format!("{name} p99"),
                format!("p99 {p99_ms:.1}ms > {kind} warn {p99_warn}ms"),
            ));
        }

        if p99_ms > 0.0 && p999_ms > 0.0 {
            let amp = p999_ms / p99_ms;
            if amp > TAIL_AMP_SEVERE {
                assessments.push((
                    if is_read { "FAIL" } else { "WARN" },
                    format!("{name} tail"),
                    format!("P99.9/P99 = {amp:.1}x"),
                ));
            } else if amp > TAIL_AMP_WARN && is_read {
                assessments.push((
                    "WARN",
                    format!("{name} tail"),
                    format!("P99.9/P99 = {amp:.1}x"),
                ));
            }
        }
    }

    // Cache assessment
    let cache_ctx = proxy_stats
        .as_ref()
        .and_then(|ps| ps["contexts"].as_array())
        .and_then(|arr| arr.first());
    let hit_rate = cache_ctx
        .and_then(|c| c["hit_rate"].as_f64())
        .unwrap_or(0.0);

    if proxy_stats.is_some() {
        if hit_rate < CACHE_HIT_WARN {
            assessments.push((
                "FAIL",
                "cache hit_rate".into(),
                format!("Hit rate {:.1}% critically low", hit_rate * 100.0),
            ));
        } else if hit_rate < CACHE_HIT_OK {
            assessments.push((
                "WARN",
                "cache hit_rate".into(),
                format!("Hit rate {:.1}% below target", hit_rate * 100.0),
            ));
        }
    }

    // Error correlation
    let circuit_open = proxy_stats
        .as_ref()
        .and_then(|ps| ps["miss_reasons"]["circuit_open"].as_u64())
        .unwrap_or(0);
    if circuit_open > 0 && total_errors > 0 {
        assessments.push((
            "WARN",
            "error_correlation".into(),
            format!("Circuit breaker ({circuit_open}) correlates with {total_errors} errors"),
        ));
    }

    if assessments.is_empty() {
        return;
    }

    // Sort: FAIL first, then WARN, then OK
    assessments.sort_by_key(|(level, _, _)| match *level {
        "FAIL" => 0,
        "WARN" => 1,
        _ => 2,
    });

    html.push_str("<div style=\"display:flex;flex-wrap:wrap;gap:.75rem;margin:1rem 0;\">\n");
    for (level, metric, message) in &assessments {
        let border_color = match *level {
            "FAIL" => "var(--red)",
            "WARN" => "var(--yellow)",
            _ => "var(--green)",
        };
        html.push_str(&format!(
            "<div class=\"card\" style=\"border-left:4px solid {border_color};padding:.6rem .9rem;min-width:220px;flex:1;\">\
             <strong style=\"color:{border_color}\">{level}</strong>\
             <span style=\"color:#8b949e;margin-left:.5rem;font-size:.8rem\">{}</span>\
             <div style=\"font-size:.85rem;margin-top:.25rem\">{}</div></div>\n",
            html_escape(metric),
            html_escape(message),
        ));
    }
    html.push_str("</div>\n");
}

/// Render tail latency analysis table.
fn render_tail_latency_section(html: &mut String, benchmarks: &[serde_json::Value]) {
    let has_tail_data = benchmarks.iter().any(|b| {
        b["raw_metrics"]["latency"]["percentiles"]["p99.9"]
            .as_f64()
            .unwrap_or(0.0)
            > 0.0
    });
    if !has_tail_data {
        return;
    }

    html.push_str("<div class=\"card\">\n<h2>Tail Latency Analysis</h2>\n");
    html.push_str(
        "<table>\n<tr><th>Scenario</th><th>P99</th><th>P99.9</th><th>P99.99</th>\
         <th>Amplification</th><th>Status</th></tr>\n",
    );

    for b in benchmarks {
        let name = b["name"].as_str().unwrap_or("unknown");
        let p99 = b["raw_metrics"]["latency"]["percentiles"]["p99"]
            .as_f64()
            .unwrap_or(0.0);
        let p999 = b["raw_metrics"]["latency"]["percentiles"]["p99.9"]
            .as_f64()
            .unwrap_or(0.0);
        let p9999 = b["raw_metrics"]["latency"]["percentiles"]["p99.99"]
            .as_f64()
            .unwrap_or(0.0);

        let amp = if p99 > 0.0 && p999 > 0.0 {
            p999 / p99
        } else {
            0.0
        };

        let (amp_color, status) = if amp <= 0.0 {
            ("#8b949e", "N/A")
        } else if amp <= 2.0 {
            ("var(--green)", "OK")
        } else if amp <= 3.0 {
            ("var(--yellow)", "WARN")
        } else if amp <= 5.0 {
            ("#f0883e", "HIGH")
        } else {
            ("var(--red)", "SEVERE")
        };

        let amp_display = if amp > 0.0 {
            format!("{amp:.1}x")
        } else {
            "—".to_string()
        };
        let p9999_display = if p9999 > 0.0 {
            format_latency(p9999)
        } else {
            "—".to_string()
        };

        html.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{p9999_display}</td>\
             <td style=\"color:{amp_color};font-weight:600\">{amp_display}</td>\
             <td style=\"color:{amp_color}\">{status}</td></tr>\n",
            html_escape(name),
            format_latency(p99),
            if p999 > 0.0 {
                format_latency(p999)
            } else {
                "—".to_string()
            },
        ));
    }
    html.push_str("</table>\n</div>\n");
}

/// Render error correlation card when errors > 0.
fn render_error_correlation(
    html: &mut String,
    proxy_stats: &Option<serde_json::Value>,
    total_errors: u64,
) {
    if total_errors == 0 {
        return;
    }
    let ps = match proxy_stats.as_ref() {
        Some(ps) => ps,
        None => return,
    };

    let circuit_open = ps["miss_reasons"]["circuit_open"].as_u64().unwrap_or(0);
    let upstream_error = ps["miss_reasons"]["upstream_error"].as_u64().unwrap_or(0);
    let expired = ps["miss_reasons"]["expired"].as_u64().unwrap_or(0);
    let not_in_cache = ps["miss_reasons"]["not_in_cache"].as_u64().unwrap_or(0);

    html.push_str("<div class=\"card\">\n<h2>Error Correlation</h2>\n");
    html.push_str("<div class=\"grid\">\n");
    html.push_str(&stat_div(
        &format_number_commas(circuit_open),
        "Circuit Open",
        "circuit_open",
        &format!("{circuit_open}"),
        "count",
    ));
    html.push_str(&stat_div(
        &format_number_commas(upstream_error),
        "Upstream Errors",
        "upstream_error",
        &format!("{upstream_error}"),
        "count",
    ));
    html.push_str(&stat_div(
        &format_number_commas(expired),
        "Expired",
        "expired",
        &format!("{expired}"),
        "count",
    ));
    html.push_str(&stat_div(
        &format_number_commas(not_in_cache),
        "Not in Cache",
        "not_in_cache",
        &format!("{not_in_cache}"),
        "count",
    ));
    html.push_str("</div>\n");

    if circuit_open > 0 {
        let overlap_pct = if total_errors > 0 {
            (circuit_open.min(total_errors) as f64 / total_errors as f64) * 100.0
        } else {
            0.0
        };
        html.push_str(&format!(
            "<p style=\"margin-top:.5rem;font-size:.85rem\">Circuit breaker accounts for up to \
             <strong style=\"color:var(--yellow)\">{overlap_pct:.0}%</strong> of {total_errors} load test errors</p>\n"
        ));
    }

    html.push_str("</div>\n");
}

// ---- load report ----

fn generate_load_report(
    load_dir: &std::path::Path,
    profile_dir: Option<&std::path::Path>,
) -> Result<(), String> {
    let summary_str = std::fs::read_to_string(load_dir.join("summary.json"))
        .map_err(|e| format!("Read summary.json: {e}"))?;
    let summary: serde_json::Value =
        serde_json::from_str(&summary_str).map_err(|e| format!("Parse summary.json: {e}"))?;
    let benchmarks = summary["benchmarks"]
        .as_array()
        .ok_or("summary.json missing benchmarks array")?;

    let proxy_stats: Option<serde_json::Value> =
        std::fs::read_to_string(load_dir.join("proxy_stats.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok());

    let profile_data: Option<serde_json::Value> = profile_dir.and_then(|d| {
        std::fs::read_to_string(d.join("profile_results.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    });

    // Compute aggregate stats
    let mut total_rps: f64 = 0.0;
    let mut total_requests: u64 = 0;
    let mut weighted_latency_sum: f64 = 0.0;
    let mut total_iters: u64 = 0;
    let mut total_errors: u64 = 0;

    for b in benchmarks {
        let rate = b["raw_metrics"]["summary"]["iters"]["rate"]
            .as_f64()
            .unwrap_or(0.0);
        let iters = b["raw_metrics"]["summary"]["iters"]["total"]
            .as_u64()
            .unwrap_or(0);
        let mean = b["raw_metrics"]["latency"]["stats"]["mean"]
            .as_f64()
            .unwrap_or(0.0);
        total_rps += rate;
        total_requests += iters;
        weighted_latency_sum += mean * iters as f64;
        total_iters += iters;

        if let Some(status_map) = b["raw_metrics"]["status"].as_object() {
            for (key, count) in status_map {
                if !key.starts_with("Success") {
                    total_errors += count.as_u64().unwrap_or(0);
                }
            }
        }
    }

    let avg_latency = if total_iters > 0 {
        weighted_latency_sum / total_iters as f64
    } else {
        0.0
    };

    let duration_secs = benchmarks
        .first()
        .and_then(|b| b["duration_secs"].as_u64())
        .unwrap_or(0);
    let users = benchmarks
        .first()
        .and_then(|b| b["users"].as_u64())
        .unwrap_or(0);

    let cache_ctx = proxy_stats
        .as_ref()
        .and_then(|ps| ps["contexts"].as_array())
        .and_then(|arr| arr.first());
    let cache_hit_rate = cache_ctx
        .and_then(|c| c["hit_rate"].as_f64())
        .unwrap_or(0.0);
    let cache_queries = cache_ctx.and_then(|c| c["queries"].as_u64()).unwrap_or(0);
    let cache_hits = cache_ctx.and_then(|c| c["hits"].as_u64()).unwrap_or(0);
    let cache_misses = cache_ctx.and_then(|c| c["misses"].as_u64()).unwrap_or(0);

    let overall_status = if total_errors > 0 { "WARN" } else { "PASS" };
    let overall_class = if total_errors > 0 { "warn" } else { "pass" };

    let mut html = String::with_capacity(16_000);
    html.push_str(&section_report_head("Load Test Report"));

    // -- A: Header --
    html.push_str(&format!(
        "<h1>Load Test Report</h1>\n\
         <p>Overall: <span class=\"{overall_class}\"><strong>{overall_status}</strong></span> \
         &mdash; {} scenarios, {}s duration, {} workers</p>\n",
        benchmarks.len(),
        duration_secs,
        users
    ));

    // -- A2: Assessment Cards --
    render_load_assessments(&mut html, benchmarks, &proxy_stats, total_errors);

    // -- B: Summary Grid --
    html.push_str("<div class=\"grid\">\n");
    html.push_str(&stat_div(
        &format_number_commas(total_rps as u64),
        "Total RPS",
        "total_rps",
        &format!("{total_rps:.0}"),
        "rps",
    ));
    html.push_str(&stat_div(
        &format_number_commas(total_requests),
        "Total Requests",
        "total_requests",
        &format!("{total_requests}"),
        "count",
    ));
    html.push_str(&stat_div(
        &format!("{duration_secs}s / {users}"),
        "Duration / Workers",
        "duration_workers",
        &format!("{duration_secs}/{users}"),
        "s/count",
    ));
    html.push_str(&stat_div(
        &format!("{:.2}%", cache_hit_rate * 100.0),
        "Cache Hit Rate",
        "cache_hit_rate",
        &format!("{cache_hit_rate:.4}"),
        "ratio",
    ));
    html.push_str(&stat_div(
        &format_latency(avg_latency),
        "Avg Latency",
        "avg_latency_secs",
        &format!("{avg_latency}"),
        "s",
    ));
    html.push_str("</div>\n");

    // -- C: Scenario Results Table --
    html.push_str("<div class=\"card\">\n<h2>Scenario Results</h2>\n");
    html.push_str(
        "<table>\n<tr><th>Protocol</th><th>Scenario</th><th>Kind</th>\
         <th>p50</th><th>p95</th><th>p99</th>\
         <th>RPS</th><th>Total</th><th>Errors</th><th>Status</th></tr>\n",
    );

    for b in benchmarks {
        let name = b["name"].as_str().unwrap_or("unknown");
        let protocol = if name.starts_with("grpc") {
            "gRPC"
        } else {
            "HTTP"
        };

        let is_read = name.contains("query") || name.contains("health") || name.contains("stats");
        let kind = if is_read { "read" } else { "write" };
        let p99_threshold_ms = if is_read {
            READ_P99_FAIL_MS
        } else {
            WRITE_P99_FAIL_MS
        };

        let p50 = b["raw_metrics"]["latency"]["percentiles"]["p50"]
            .as_f64()
            .unwrap_or(0.0);
        let p95 = b["raw_metrics"]["latency"]["percentiles"]["p95"]
            .as_f64()
            .unwrap_or(0.0);
        let p99 = b["raw_metrics"]["latency"]["percentiles"]["p99"]
            .as_f64()
            .unwrap_or(0.0);
        let p99_ms = p99 * 1000.0;
        let rps = b["raw_metrics"]["summary"]["iters"]["rate"]
            .as_f64()
            .unwrap_or(0.0);
        let total = b["raw_metrics"]["summary"]["iters"]["total"]
            .as_u64()
            .unwrap_or(0);

        let mut errors: u64 = 0;
        if let Some(status_map) = b["raw_metrics"]["status"].as_object() {
            for (key, count) in status_map {
                if !key.starts_with("Success") {
                    errors += count.as_u64().unwrap_or(0);
                }
            }
        }

        let (status_badge, status_class) = if errors > 0 {
            ("WARN", "warn")
        } else {
            ("PASS", "pass")
        };

        let threshold_color = if p99_ms > p99_threshold_ms {
            "var(--red)"
        } else {
            "var(--green)"
        };
        let p99_display = format!(
            "{} <span style=\"font-size:.75rem;color:{}\">({}ms)</span>",
            format_latency(p99),
            threshold_color,
            p99_threshold_ms
        );

        html.push_str(&format!(
            "<tr><td>{protocol}</td><td>{}</td><td>{kind}</td>\
             <td>{}</td><td>{}</td><td>{p99_display}</td>\
             <td>{}</td><td>{}</td>\
             <td>{}</td>\
             <td><span class=\"{status_class}\">{status_badge}</span></td></tr>\n",
            html_escape(name),
            format_latency(p50),
            format_latency(p95),
            format_number_commas(rps as u64),
            format_number_commas(total),
            format_number_commas(errors),
        ));
    }
    html.push_str("</table>\n</div>\n");

    // -- C2: Tail Latency Section --
    render_tail_latency_section(&mut html, benchmarks);

    // -- D: Per-Scenario Details --
    for b in benchmarks {
        let name = b["name"].as_str().unwrap_or("unknown");
        html.push_str(&format!(
            "<details>\n<summary>Details: {}</summary>\n<div class=\"card\">\n",
            html_escape(name)
        ));

        // Percentile table
        if let Some(pcts) = b["raw_metrics"]["latency"]["percentiles"].as_object() {
            let mut pct_vec: Vec<(&String, &serde_json::Value)> = pcts.iter().collect();
            pct_vec.sort_by(|a, b| {
                let pa: f64 = a.0.trim_start_matches('p').parse().unwrap_or(0.0);
                let pb: f64 = b.0.trim_start_matches('p').parse().unwrap_or(0.0);
                pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
            });
            html.push_str("<h2>Percentiles</h2>\n<table>\n<tr>");
            for (label, _) in &pct_vec {
                html.push_str(&format!("<th>{label}</th>"));
            }
            html.push_str("</tr>\n<tr>");
            for (_, val) in &pct_vec {
                let v = val.as_f64().unwrap_or(0.0);
                html.push_str(&format!("<td>{}</td>", format_latency(v)));
            }
            html.push_str("</tr>\n</table>\n");
        }

        // Latency stats
        if let Some(stats) = b["raw_metrics"]["latency"]["stats"].as_object() {
            html.push_str("<h2>Latency Stats</h2>\n<table>\n<tr>");
            for key in &["min", "max", "mean", "median", "stdev"] {
                html.push_str(&format!("<th>{key}</th>"));
            }
            html.push_str("</tr>\n<tr>");
            for key in &["min", "max", "mean", "median", "stdev"] {
                let v = stats.get(*key).and_then(|v| v.as_f64()).unwrap_or(0.0);
                html.push_str(&format!("<td>{}</td>", format_latency(v)));
            }
            html.push_str("</tr>\n</table>\n");
        }

        // Status code breakdown
        if let Some(status_map) = b["raw_metrics"]["status"].as_object() {
            html.push_str(
                "<h2>Status Codes</h2>\n<table>\n<tr><th>Status</th><th>Count</th></tr>\n",
            );
            for (code, count) in status_map {
                let cls = if code.starts_with("Success") {
                    "pass"
                } else {
                    "fail"
                };
                html.push_str(&format!(
                    "<tr><td>{}</td><td class=\"{cls}\">{}</td></tr>\n",
                    html_escape(code),
                    format_number_commas(count.as_u64().unwrap_or(0))
                ));
            }
            html.push_str("</table>\n");
        }

        html.push_str("</div>\n</details>\n");
    }

    // -- E: Proxy Cache Stats --
    if proxy_stats.is_some() {
        html.push_str("<div class=\"card\">\n<h2>Proxy Cache Stats</h2>\n<div class=\"grid\">\n");
        html.push_str(&stat_div(
            &format_number_commas(cache_queries),
            "Total Queries",
            "cache_queries",
            &format!("{cache_queries}"),
            "count",
        ));
        html.push_str(&stat_div(
            &format!("{:.2}%", cache_hit_rate * 100.0),
            "Hit Rate",
            "cache_hit_rate",
            &format!("{cache_hit_rate:.4}"),
            "ratio",
        ));
        html.push_str(&stat_div(
            &format_number_commas(cache_hits),
            "Hits",
            "cache_hits",
            &format!("{cache_hits}"),
            "count",
        ));
        html.push_str(&stat_div(
            &format_number_commas(cache_misses),
            "Misses",
            "cache_misses",
            &format!("{cache_misses}"),
            "count",
        ));
        html.push_str("</div>\n</div>\n");
    }

    // -- E2: Error Correlation --
    render_error_correlation(&mut html, &proxy_stats, total_errors);

    // -- F/G/H: Profile sections (process resources, CPU, memory/syscalls) --
    if let Some(ref pd) = profile_data {
        render_profile_sections(&mut html, pd, load_dir);
    }

    html.push_str(section_report_footer());

    let report_path = load_dir.join("report.html");
    std::fs::write(&report_path, &html).map_err(|e| format!("Write load report: {e}"))?;
    eprintln!("Generated {}", report_path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// report subcommand
// ---------------------------------------------------------------------------

fn cmd_report(args: &[String]) {
    let mut input: Option<String> = None;
    let mut output: Option<String> = None;
    let mut compare: Option<String> = None;
    let mut html_output: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--input" if i + 1 < args.len() => {
                input = Some(args[i + 1].clone());
                i += 2;
            }
            "--output" if i + 1 < args.len() => {
                output = Some(args[i + 1].clone());
                i += 2;
            }
            "--compare" if i + 1 < args.len() => {
                compare = Some(args[i + 1].clone());
                i += 2;
            }
            "--html" if i + 1 < args.len() => {
                html_output = Some(args[i + 1].clone());
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }

    let input_path = input.unwrap_or_else(|| {
        eprintln!("Error: --input is required");
        std::process::exit(1);
    });
    let input_path = std::path::Path::new(&input_path);

    if !input_path.exists() {
        eprintln!("Error: input file not found: {}", input_path.display());
        std::process::exit(1);
    }

    let content = std::fs::read_to_string(input_path).expect("Failed to read input");
    let json: serde_json::Value =
        serde_json::from_str(&content).expect("Failed to parse input JSON");

    let compare_json = compare.as_ref().and_then(|p| {
        let path = std::path::Path::new(p);
        if path.exists() {
            std::fs::read_to_string(path)
                .ok()
                .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
        } else {
            None
        }
    });

    // Generate markdown
    let md = generate_markdown(&json, compare_json.as_ref());

    if let Some(ref out_path) = output {
        let p = std::path::Path::new(out_path);
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(p, &md).expect("Failed to write markdown");
        eprintln!("Report written to {}", p.display());
    } else {
        print!("{md}");
    }

    // Generate HTML if requested
    if let Some(ref html_path) = html_output {
        let p = std::path::Path::new(html_path);
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let html = generate_html(&json, compare_json.as_ref());
        std::fs::write(p, &html).expect("Failed to write HTML");
        eprintln!("HTML report written to {}", p.display());
    }
}

// ---------------------------------------------------------------------------
// profile subcommand — ProcMonitor + tiered profiling
// ---------------------------------------------------------------------------

/// Single /proc sample for a running process.
#[cfg(target_os = "linux")]
struct ProcSnapshot {
    timestamp_ms: u64,
    rss_bytes: u64,
    vsize_bytes: u64,
    utime_ticks: u64,
    stime_ticks: u64,
    num_threads: u32,
    fd_count: u32,
    voluntary_ctxt_switches: u64,
    nonvoluntary_ctxt_switches: u64,
    cpu_percent: f64,
    minflt: u64,
    majflt: u64,
    read_bytes: u64,
    write_bytes: u64,
}

/// Aggregated summary of process resource usage.
#[cfg(target_os = "linux")]
struct ProcSummary {
    peak_rss_bytes: u64,
    avg_rss_bytes: u64,
    final_rss_bytes: u64,
    cpu_percent: f64,
    peak_threads: u32,
    peak_fds: u32,
    total_voluntary_ctxt_switches: u64,
    total_nonvoluntary_ctxt_switches: u64,
    total_minflt: u64,
    total_majflt: u64,
    total_read_bytes: u64,
    total_write_bytes: u64,
}

/// Monitors a process via /proc reads.
#[cfg(target_os = "linux")]
struct ProcMonitor {
    pid: u32,
    #[allow(dead_code)]
    interval: std::time::Duration,
    snapshots: Vec<ProcSnapshot>,
    clock_ticks_per_sec: u64,
}

#[cfg(target_os = "linux")]
impl ProcMonitor {
    fn new(pid: u32) -> Self {
        #[allow(unsafe_code)]
        // SAFETY: `sysconf` is a read-only query of system limits, no side effects.
        let clock_ticks_per_sec = unsafe { libc::sysconf(libc::_SC_CLK_TCK) as u64 };
        Self {
            pid,
            interval: std::time::Duration::from_secs(1),
            snapshots: Vec::new(),
            clock_ticks_per_sec,
        }
    }

    /// Take a single sample from /proc. Returns false if the process is gone.
    fn sample(&mut self) -> bool {
        let stat_path = format!("/proc/{}/stat", self.pid);
        let stat_content = match std::fs::read_to_string(&stat_path) {
            Ok(c) => c,
            Err(_) => return false,
        };

        // /proc/PID/stat: skip past comm (which may contain spaces/parens)
        let after_comm = match stat_content.rfind(')') {
            Some(pos) => &stat_content[pos + 2..],
            None => return false,
        };
        let fields: Vec<&str> = after_comm.split_whitespace().collect();
        // fields[0]=state, [1]=ppid, ..., [11]=utime, [12]=stime, ..., [17]=num_threads, ..., [20]=vsize, [21]=rss(pages)
        if fields.len() < 22 {
            return false;
        }

        // fields[8]=minflt, [9]=cminflt, [10]=majflt, [11]=cmajflt — but after
        // stripping comm+state, indices are shifted: [7]=minflt, [9]=majflt
        let minflt: u64 = fields[7].parse().unwrap_or(0);
        let majflt: u64 = fields[9].parse().unwrap_or(0);
        let utime_ticks: u64 = fields[11].parse().unwrap_or(0);
        let stime_ticks: u64 = fields[12].parse().unwrap_or(0);
        let num_threads: u32 = fields[17].parse().unwrap_or(0);
        let vsize_bytes: u64 = fields[20].parse().unwrap_or(0);
        let rss_pages: u64 = fields[21].parse().unwrap_or(0);
        #[allow(unsafe_code)]
        // SAFETY: `sysconf` is a read-only query of system limits, no side effects.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as u64 };
        let rss_bytes = rss_pages * page_size;

        // Per-snapshot CPU% from tick deltas
        let cpu_percent = if let Some(prev) = self.snapshots.last() {
            let delta_ticks =
                (utime_ticks + stime_ticks).saturating_sub(prev.utime_ticks + prev.stime_ticks);
            let delta_wall_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let delta_wall_ms = delta_wall_ms.saturating_sub(prev.timestamp_ms);
            if delta_wall_ms > 0 && self.clock_ticks_per_sec > 0 {
                (delta_ticks as f64
                    / (delta_wall_ms as f64 / 1000.0 * self.clock_ticks_per_sec as f64))
                    * 100.0
            } else {
                0.0
            }
        } else {
            0.0
        };

        // I/O bytes from /proc/PID/io
        let mut read_bytes = 0u64;
        let mut write_bytes = 0u64;
        let io_path = format!("/proc/{}/io", self.pid);
        if let Ok(io_content) = std::fs::read_to_string(&io_path) {
            for line in io_content.lines() {
                if let Some(v) = line.strip_prefix("read_bytes: ") {
                    read_bytes = v.trim().parse().unwrap_or(0);
                } else if let Some(v) = line.strip_prefix("write_bytes: ") {
                    write_bytes = v.trim().parse().unwrap_or(0);
                }
            }
        }

        // Context switches from /proc/PID/status
        let mut voluntary_ctxt_switches = 0u64;
        let mut nonvoluntary_ctxt_switches = 0u64;
        let status_path = format!("/proc/{}/status", self.pid);
        if let Ok(status) = std::fs::read_to_string(&status_path) {
            for line in status.lines() {
                if let Some(v) = line.strip_prefix("voluntary_ctxt_switches:") {
                    voluntary_ctxt_switches = v.trim().parse().unwrap_or(0);
                } else if let Some(v) = line.strip_prefix("nonvoluntary_ctxt_switches:") {
                    nonvoluntary_ctxt_switches = v.trim().parse().unwrap_or(0);
                }
            }
        }

        // FD count
        let fd_dir = format!("/proc/{}/fd", self.pid);
        let fd_count = std::fs::read_dir(&fd_dir)
            .map(|entries| entries.count() as u32)
            .unwrap_or(0);

        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        self.snapshots.push(ProcSnapshot {
            timestamp_ms,
            rss_bytes,
            vsize_bytes,
            utime_ticks,
            stime_ticks,
            num_threads,
            fd_count,
            voluntary_ctxt_switches,
            nonvoluntary_ctxt_switches,
            cpu_percent,
            minflt,
            majflt,
            read_bytes,
            write_bytes,
        });

        true
    }

    /// Convert all snapshots + summary to JSON.
    fn to_json(&self) -> serde_json::Value {
        let snapshots: Vec<serde_json::Value> = self
            .snapshots
            .iter()
            .map(|s| {
                serde_json::json!({
                    "timestamp_ms": s.timestamp_ms,
                    "rss_bytes": s.rss_bytes,
                    "vsize_bytes": s.vsize_bytes,
                    "utime_ticks": s.utime_ticks,
                    "stime_ticks": s.stime_ticks,
                    "num_threads": s.num_threads,
                    "fd_count": s.fd_count,
                    "voluntary_ctxt_switches": s.voluntary_ctxt_switches,
                    "nonvoluntary_ctxt_switches": s.nonvoluntary_ctxt_switches,
                    "cpu_percent": s.cpu_percent,
                    "minflt": s.minflt,
                    "majflt": s.majflt,
                    "read_bytes": s.read_bytes,
                    "write_bytes": s.write_bytes,
                })
            })
            .collect();

        let summary = self.summary();
        serde_json::json!({
            "snapshots": snapshots,
            "summary": {
                "peak_rss_bytes": summary.peak_rss_bytes,
                "avg_rss_bytes": summary.avg_rss_bytes,
                "final_rss_bytes": summary.final_rss_bytes,
                "cpu_percent": summary.cpu_percent,
                "peak_threads": summary.peak_threads,
                "peak_fds": summary.peak_fds,
                "total_voluntary_ctxt_switches": summary.total_voluntary_ctxt_switches,
                "total_nonvoluntary_ctxt_switches": summary.total_nonvoluntary_ctxt_switches,
                "total_minflt": summary.total_minflt,
                "total_majflt": summary.total_majflt,
                "total_read_bytes": summary.total_read_bytes,
                "total_write_bytes": summary.total_write_bytes,
            }
        })
    }

    /// Compute aggregated summary from snapshots.
    fn summary(&self) -> ProcSummary {
        if self.snapshots.is_empty() {
            return ProcSummary {
                peak_rss_bytes: 0,
                avg_rss_bytes: 0,
                final_rss_bytes: 0,
                cpu_percent: 0.0,
                peak_threads: 0,
                peak_fds: 0,
                total_voluntary_ctxt_switches: 0,
                total_nonvoluntary_ctxt_switches: 0,
                total_minflt: 0,
                total_majflt: 0,
                total_read_bytes: 0,
                total_write_bytes: 0,
            };
        }

        let peak_rss = self
            .snapshots
            .iter()
            .map(|s| s.rss_bytes)
            .max()
            .unwrap_or(0);
        let avg_rss =
            self.snapshots.iter().map(|s| s.rss_bytes).sum::<u64>() / self.snapshots.len() as u64;
        let final_snap = self.snapshots.last().unwrap();
        let first_snap = self.snapshots.first().unwrap();

        // CPU% = (delta_utime + delta_stime) / (delta_wall_time * clock_ticks_per_sec) * 100
        let delta_ticks = (final_snap.utime_ticks + final_snap.stime_ticks)
            .saturating_sub(first_snap.utime_ticks + first_snap.stime_ticks);
        let delta_wall_ms = final_snap
            .timestamp_ms
            .saturating_sub(first_snap.timestamp_ms);
        let cpu_percent = if delta_wall_ms > 0 && self.clock_ticks_per_sec > 0 {
            (delta_ticks as f64 / (delta_wall_ms as f64 / 1000.0 * self.clock_ticks_per_sec as f64))
                * 100.0
        } else {
            0.0
        };

        ProcSummary {
            peak_rss_bytes: peak_rss,
            avg_rss_bytes: avg_rss,
            final_rss_bytes: final_snap.rss_bytes,
            cpu_percent,
            peak_threads: self
                .snapshots
                .iter()
                .map(|s| s.num_threads)
                .max()
                .unwrap_or(0),
            peak_fds: self.snapshots.iter().map(|s| s.fd_count).max().unwrap_or(0),
            total_voluntary_ctxt_switches: final_snap.voluntary_ctxt_switches,
            total_nonvoluntary_ctxt_switches: final_snap.nonvoluntary_ctxt_switches,
            total_minflt: final_snap.minflt,
            total_majflt: final_snap.majflt,
            total_read_bytes: final_snap.read_bytes,
            total_write_bytes: final_snap.write_bytes,
        }
    }
}

/// Profiling capability tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProfilingTier {
    /// sudo + perf + bpftrace — full profiling
    Full,
    /// perf without sudo (perf_event_paranoid <= 1) — flamegraph + top functions
    PerfOnly,
    /// /proc only — process metrics + proxy stats
    Lightweight,
}

#[allow(dead_code)]
struct TierDetection {
    tier: ProfilingTier,
    has_perf: bool,
    has_bpftrace: bool,
    has_sudo: bool,
    perf_paranoid: Option<i32>,
    reasons: Vec<String>,
}

/// Detect the best available profiling tier for the current environment.
fn detect_profiling_tier(mode: &str) -> TierDetection {
    use std::process::{Command, Stdio};

    let has_perf = check_tool("perf");
    let has_bpftrace = check_tool("bpftrace");
    let has_sudo = Command::new("sudo")
        .args(["-n", "true"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    let perf_paranoid: Option<i32> =
        std::fs::read_to_string("/proc/sys/kernel/perf_event_paranoid")
            .ok()
            .and_then(|s| s.trim().parse().ok());

    let mut reasons = Vec::new();

    let need_bpftrace = matches!(mode, "mem" | "syscall" | "all");

    // Tier 1: Full — sudo + perf + (bpftrace or cpu-only mode)
    if has_sudo && has_perf && (has_bpftrace || !need_bpftrace) {
        reasons.push("sudo: available".into());
        reasons.push("perf: available".into());
        if has_bpftrace {
            reasons.push("bpftrace: available".into());
        } else {
            reasons.push("bpftrace: not needed for cpu-only mode".into());
        }
        return TierDetection {
            tier: ProfilingTier::Full,
            has_perf,
            has_bpftrace,
            has_sudo,
            perf_paranoid,
            reasons,
        };
    }

    // Tier 2: PerfOnly — perf available and perf_event_paranoid <= 1
    if has_perf {
        if let Some(paranoid) = perf_paranoid {
            if paranoid <= 1 {
                reasons.push(format!("perf: available (perf_event_paranoid={paranoid})"));
                if !has_sudo {
                    reasons.push("sudo: not available (not needed for perf)".into());
                }
                return TierDetection {
                    tier: ProfilingTier::PerfOnly,
                    has_perf,
                    has_bpftrace,
                    has_sudo,
                    perf_paranoid,
                    reasons,
                };
            }
            reasons.push(format!(
                "perf: available but perf_event_paranoid={paranoid} (needs <=1 without sudo)"
            ));
        } else {
            reasons.push("perf: available but cannot read perf_event_paranoid".into());
        }
    } else {
        reasons.push("perf: not found".into());
    }

    if !has_sudo {
        reasons.push("sudo: not available".into());
    }
    if !has_bpftrace {
        reasons.push("bpftrace: not found".into());
    }

    TierDetection {
        tier: ProfilingTier::Lightweight,
        has_perf,
        has_bpftrace,
        has_sudo,
        perf_paranoid,
        reasons,
    }
}

/// Generate an inline SVG sparkline chart.
fn render_sparkline_svg(points: &[(f64, f64)], label: &str, color: &str, w: u32, h: u32) -> String {
    if points.is_empty() {
        return String::new();
    }

    let x_min = points.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
    let x_max = points.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max);
    let y_min = points.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let y_max = points.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);

    let x_range = if (x_max - x_min).abs() < f64::EPSILON {
        1.0
    } else {
        x_max - x_min
    };
    let y_range = if (y_max - y_min).abs() < f64::EPSILON {
        1.0
    } else {
        y_max - y_min
    };

    let margin = 4.0;
    let plot_w = w as f64 - 2.0 * margin;
    let plot_h = h as f64 - 2.0 * margin - 14.0; // room for label

    let mut path = String::new();
    for (i, (x, y)) in points.iter().enumerate() {
        let px = margin + (x - x_min) / x_range * plot_w;
        let py = margin + plot_h - (y - y_min) / y_range * plot_h;
        if i == 0 {
            path.push_str(&format!("M{px:.1},{py:.1}"));
        } else {
            path.push_str(&format!(" L{px:.1},{py:.1}"));
        }
    }

    format!(
        r##"<svg width="{w}" height="{h}" viewBox="0 0 {w} {h}" xmlns="http://www.w3.org/2000/svg">
  <path d="{path}" fill="none" stroke="{color}" stroke-width="1.5" stroke-linejoin="round"/>
  <text x="{}" y="{}" font-size="10" fill="#8b949e" font-family="monospace">{label}</text>
</svg>"##,
        margin,
        h as f64 - 2.0,
    )
}

// ---------------------------------------------------------------------------
// proc-monitor subcommand — lightweight /proc monitoring for any PID
// ---------------------------------------------------------------------------

/// Monitor a process via /proc and write profile_results.json when it exits.
/// With --perf, also runs perf record and generates a flamegraph.
/// With --bpftrace, also runs bpftrace memory/syscall profiling (requires sudo + bpftrace).
///
/// Usage: test_runner proc-monitor --pid <PID> --output-dir <DIR> [--perf] [--bpftrace] [--freq <hz>]
fn cmd_proc_monitor(args: &[String]) {
    use std::process::{Command, Stdio};

    let mut pid: Option<u32> = None;
    let mut output_dir: Option<String> = None;
    let mut do_perf = false;
    let mut do_bpftrace = false;
    let mut do_dhat = false;
    let mut dhat_search_dir: Option<String> = None;
    let mut freq: u64 = 99;
    let mut diff_baseline: Option<String> = None;
    let mut resource_profile_name: Option<String> = None;
    let mut ready_file: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--pid" if i + 1 < args.len() => {
                pid = args[i + 1].parse().ok();
                i += 2;
            }
            "--output-dir" if i + 1 < args.len() => {
                output_dir = Some(args[i + 1].clone());
                i += 2;
            }
            "--perf" => {
                do_perf = true;
                i += 1;
            }
            "--bpftrace" => {
                do_bpftrace = true;
                i += 1;
            }
            "--dhat" => {
                do_dhat = true;
                i += 1;
            }
            "--dhat-search-dir" if i + 1 < args.len() => {
                dhat_search_dir = Some(args[i + 1].clone());
                i += 2;
            }
            "--freq" if i + 1 < args.len() => {
                freq = args[i + 1].parse().unwrap_or(99);
                i += 2;
            }
            "--diff-baseline" if i + 1 < args.len() => {
                diff_baseline = Some(args[i + 1].clone());
                i += 2;
            }
            "--resource-profile" if i + 1 < args.len() => {
                resource_profile_name = Some(args[i + 1].clone());
                i += 2;
            }
            "--ready-file" if i + 1 < args.len() => {
                ready_file = Some(args[i + 1].clone());
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }

    let pid = pid.unwrap_or_else(|| {
        eprintln!("Error: --pid is required");
        std::process::exit(1);
    });
    let output_dir = output_dir.unwrap_or_else(|| {
        eprintln!("Error: --output-dir is required");
        std::process::exit(1);
    });
    let out_path = std::path::Path::new(&output_dir);
    let _ = std::fs::create_dir_all(out_path);

    // Resolve resource profile (platform-independent parsing)
    let resource_profile = resource_profile_name.as_deref().and_then(|name| {
        let profile = resolve_profile(name);
        if profile.is_none() {
            eprintln!(
                "proc-monitor: unknown resource profile '{name}' — valid: small, medium, large"
            );
        }
        profile
    });

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (do_perf, do_bpftrace, freq, resource_profile);
        if let Some(ref path) = ready_file {
            let _ = std::fs::write(path, "unsupported\n");
        }
        eprintln!("proc-monitor is only supported on Linux");
        std::process::exit(1);
    }

    #[cfg(target_os = "linux")]
    {
        // Optionally start perf record alongside /proc monitoring
        let freq_str = freq.to_string();
        let mut perf_child = if do_perf {
            let perf_data = out_path.join("cpu_perf.data");
            // Try perf without sudo first (works at paranoid <= 2 for own processes)
            let perf_data_str = perf_data.to_string_lossy().to_string();
            let pid_str = pid.to_string();
            match Command::new("perf")
                .args([
                    "record",
                    "-F",
                    &freq_str,
                    "-g",
                    "--call-graph",
                    "dwarf,16384",
                    "--max-size",
                    "256M",
                    "-p",
                    &pid_str,
                    "-o",
                    &perf_data_str,
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(child) => {
                    eprintln!("proc-monitor: perf recording PID {pid} @ {freq}Hz");
                    Some(child)
                }
                Err(_) => {
                    // Try with sudo
                    match Command::new("sudo")
                        .args([
                            "perf",
                            "record",
                            "-F",
                            &freq_str,
                            "-g",
                            "--call-graph",
                            "dwarf,16384",
                            "--max-size",
                            "256M",
                            "-p",
                            &pid_str,
                            "-o",
                            &perf_data_str,
                        ])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                    {
                        Ok(child) => {
                            eprintln!("proc-monitor: perf recording PID {pid} @ {freq}Hz (sudo)");
                            Some(child)
                        }
                        Err(e) => {
                            eprintln!(
                                "proc-monitor: perf not available ({e}), skipping CPU profiling"
                            );
                            None
                        }
                    }
                }
            }
        } else {
            None
        };

        // Start perf stat for hardware counters alongside perf record
        let mut perf_stat_child = if do_perf {
            let pid_str = pid.to_string();
            let perf_stat_out = out_path.join("perf_stat_raw.txt");
            (|| -> Option<std::process::Child> {
                let perf_stat_file = match std::fs::File::create(&perf_stat_out) {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!(
                            "proc-monitor: skipping perf stat — cannot create {}: {e}",
                            perf_stat_out.display()
                        );
                        return None;
                    }
                };
                match Command::new("perf")
                    .args([
                        "stat",
                        "-e",
                        "cycles,instructions,cache-misses,cache-references,branch-misses,page-faults",
                        "-p",
                        &pid_str,
                    ])
                    .stdout(Stdio::null())
                    .stderr(perf_stat_file)
                    .spawn()
                {
                    Ok(child) => {
                        eprintln!("proc-monitor: perf stat attached to PID {pid}");
                        Some(child)
                    }
                    Err(_) => {
                        // Try with sudo
                        let perf_stat_file2 = match std::fs::File::create(&perf_stat_out) {
                            Ok(f) => f,
                            Err(e) => {
                                eprintln!(
                                    "proc-monitor: skipping perf stat — cannot create {}: {e}",
                                    perf_stat_out.display()
                                );
                                return None;
                            }
                        };
                        match Command::new("sudo")
                            .args([
                                "perf",
                                "stat",
                                "-e",
                                "cycles,instructions,cache-misses,cache-references,branch-misses,page-faults",
                                "-p",
                                &pid_str,
                            ])
                            .stdout(Stdio::null())
                            .stderr(perf_stat_file2)
                            .spawn()
                        {
                            Ok(child) => {
                                eprintln!("proc-monitor: perf stat attached to PID {pid} (sudo)");
                                Some(child)
                            }
                            Err(e) => {
                                eprintln!("proc-monitor: perf stat not available ({e}), skipping hardware counters");
                                None
                            }
                        }
                    }
                }
            })()
        } else {
            None
        };

        // Start off-CPU perf record (sched:sched_switch tracepoint, requires sudo)
        let mut offcpu_child = if do_perf {
            let pid_str = pid.to_string();
            let offcpu_data = out_path.join("cpu_offcpu_perf.data");
            let offcpu_data_str = offcpu_data.to_string_lossy().to_string();
            match Command::new("sudo")
                .args([
                    "perf",
                    "record",
                    "-e",
                    "sched:sched_switch",
                    "-g",
                    "--call-graph",
                    "dwarf,16384",
                    "--max-size",
                    "512M",
                    "-p",
                    &pid_str,
                    "-o",
                    &offcpu_data_str,
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(child) => {
                    eprintln!("proc-monitor: off-CPU perf recording PID {pid}");
                    Some(child)
                }
                Err(e) => {
                    eprintln!("proc-monitor: off-CPU recording not available ({e}), skipping (needs sudo + sched:sched_switch tracepoint)");
                    None
                }
            }
        } else {
            None
        };

        // Optionally start bpftrace profilers
        let mut bpftrace_children: Vec<(&str, std::process::Child)> = Vec::new();
        if do_bpftrace {
            let tier_detection = detect_profiling_tier("all");
            if tier_detection.has_bpftrace && tier_detection.has_sudo {
                let manifest_dir = env!("CARGO_MANIFEST_DIR");
                let bt_dir = format!("{manifest_dir}/tests/e2e/profiling");
                let pid_str = pid.to_string();

                // Syscall profiler
                let syscall_script = format!("{bt_dir}/syscall_profile.bt");
                if std::path::Path::new(&syscall_script).exists() {
                    let syscall_out = out_path.join("syscall_raw.txt");
                    let syscall_err = out_path.join("syscall_stderr.txt");
                    match (
                        std::fs::File::create(&syscall_out),
                        std::fs::File::create(&syscall_err),
                    ) {
                        (Ok(syscall_file), Ok(syscall_err_file)) => {
                            eprintln!("proc-monitor: attaching bpftrace (syscall) to PID {pid}");
                            match Command::new("sudo")
                                .args(["bpftrace", "-p", &pid_str, &syscall_script, &pid_str])
                                .stdout(syscall_file)
                                .stderr(syscall_err_file)
                                .spawn()
                            {
                                Ok(child) => bpftrace_children.push(("bpftrace-syscall", child)),
                                Err(e) => {
                                    eprintln!(
                                        "proc-monitor: failed to start bpftrace (syscall): {e}"
                                    )
                                }
                            }
                        }
                        _ => {
                            eprintln!("proc-monitor: skipping syscall profiling — cannot create output files");
                        }
                    }
                }

                // Lock contention profiler
                let lock_script = format!("{bt_dir}/lock_profile.bt");
                if std::path::Path::new(&lock_script).exists() {
                    let lock_out = out_path.join("lock_raw.txt");
                    let lock_err = out_path.join("lock_stderr.txt");
                    match (
                        std::fs::File::create(&lock_out),
                        std::fs::File::create(&lock_err),
                    ) {
                        (Ok(lock_file), Ok(lock_err_file)) => {
                            eprintln!("proc-monitor: attaching bpftrace (lock) to PID {pid}");
                            match Command::new("sudo")
                                .args(["bpftrace", "-p", &pid_str, &lock_script, &pid_str])
                                .stdout(lock_file)
                                .stderr(lock_err_file)
                                .spawn()
                            {
                                Ok(child) => bpftrace_children.push(("bpftrace-lock", child)),
                                Err(e) => {
                                    eprintln!("proc-monitor: failed to start bpftrace (lock): {e}")
                                }
                            }
                        }
                        _ => {
                            eprintln!("proc-monitor: skipping lock profiling — cannot create output files");
                        }
                    }
                }

                // Net connection lifecycle profiler
                let net_script = format!("{bt_dir}/net_profile.bt");
                if std::path::Path::new(&net_script).exists() {
                    let net_out = out_path.join("net_raw.txt");
                    let net_err = out_path.join("net_stderr.txt");
                    match (
                        std::fs::File::create(&net_out),
                        std::fs::File::create(&net_err),
                    ) {
                        (Ok(net_file), Ok(net_err_file)) => {
                            eprintln!("proc-monitor: attaching bpftrace (net) to PID {pid}");
                            match Command::new("sudo")
                                .args(["bpftrace", "-p", &pid_str, &net_script, &pid_str])
                                .stdout(net_file)
                                .stderr(net_err_file)
                                .spawn()
                            {
                                Ok(child) => bpftrace_children.push(("bpftrace-net", child)),
                                Err(e) => {
                                    eprintln!("proc-monitor: failed to start bpftrace (net): {e}")
                                }
                            }
                        }
                        _ => {
                            eprintln!(
                                "proc-monitor: skipping net profiling — cannot create output files"
                            );
                        }
                    }
                }

                // Give bpftrace time to attach
                if !bpftrace_children.is_empty() {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
            } else {
                eprintln!(
                    "proc-monitor: --bpftrace requested but {}",
                    if !tier_detection.has_bpftrace {
                        "bpftrace not found"
                    } else {
                        "sudo not available"
                    }
                );
            }
        }

        // cgroup v2: clean stale scopes, set up resource limits
        cleanup_stale_cgroups();
        let cgroup_scope = resource_profile.and_then(|profile| {
            if !is_cgroupv2() {
                eprintln!("proc-monitor: cgroups v2 not available, skipping resource scoping");
                return None;
            }
            match setup_cgroup(pid, profile) {
                Ok(scope) => Some(scope),
                Err(e) => {
                    eprintln!("proc-monitor: cgroup setup failed: {e}");
                    None
                }
            }
        });

        // Signal readiness — all profilers attached & cgroup applied
        if let Some(ref path) = ready_file {
            if let Err(e) = std::fs::write(path, "ready\n") {
                eprintln!("proc-monitor: failed to write ready-file {path}: {e}");
            }
        }

        eprintln!("proc-monitor: monitoring PID {pid}, output to {output_dir}");
        let mut monitor = ProcMonitor::new(pid);
        loop {
            if !monitor.sample() {
                eprintln!(
                    "proc-monitor: PID {pid} exited after {} samples",
                    monitor.snapshots.len()
                );
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }

        // Stop perf and generate flamegraph
        let mut cpu_json = serde_json::Value::Null;
        if let Some(ref mut child) = perf_child {
            eprintln!("proc-monitor: stopping perf...");
            stop_child_with_timeout(child, "perf record", 60);

            let perf_data = out_path.join("cpu_perf.data");
            if perf_data.exists() {
                cpu_json = process_perf_data(out_path, &perf_data, diff_baseline.as_deref());
            }
        }

        // Stop perf stat
        if let Some(ref mut child) = perf_stat_child {
            eprintln!("proc-monitor: stopping perf stat...");
            stop_child_with_timeout(child, "perf stat", 10);
        }

        // Stop off-CPU perf and generate off-CPU flamegraph
        if let Some(ref mut child) = offcpu_child {
            eprintln!("proc-monitor: stopping off-CPU perf...");
            stop_child_with_timeout(child, "off-CPU perf", 15);

            let offcpu_data = out_path.join("cpu_offcpu_perf.data");
            if offcpu_data.exists() {
                process_offcpu_flamegraph(out_path, &offcpu_data);
            }
        }

        // Stop bpftrace children
        if !bpftrace_children.is_empty() {
            eprintln!("proc-monitor: stopping bpftrace profilers...");
        }
        for (name, mut child) in bpftrace_children {
            stop_child_with_timeout(&mut child, name, 15);
            // Report bpftrace errors if stderr file exists
            let kind = if name.contains("lock") {
                "lock"
            } else if name.contains("net") {
                "net"
            } else {
                "syscall"
            };
            let stderr_path = out_path.join(format!("{kind}_stderr.txt"));
            if let Ok(stderr) = std::fs::read_to_string(&stderr_path) {
                let stderr = stderr.trim();
                if !stderr.is_empty() {
                    eprintln!("  {name} stderr: {stderr}");
                }
            }
        }

        if monitor.snapshots.is_empty() {
            eprintln!("proc-monitor: no samples collected for PID {pid}");
            return;
        }

        // Determine actual tier based on what profilers ran
        let actual_tier = detect_profiling_tier("all");
        let dhat_suffix = if do_dhat { " + dhat-heap" } else { "" };
        let (tier_label, tier_reasons) =
            if do_bpftrace && actual_tier.has_bpftrace && actual_tier.has_sudo && do_perf {
                (
                    "Full",
                    vec![format!(
                        "proc-monitor: /proc + perf + bpftrace{dhat_suffix}"
                    )],
                )
            } else if do_perf {
                (
                    "PerfOnly",
                    vec![format!("proc-monitor: /proc + perf{dhat_suffix}")],
                )
            } else {
                (
                    "Lightweight",
                    vec![format!("proc-monitor: /proc sampling only{dhat_suffix}")],
                )
            };

        let mut json = serde_json::json!({
            "mode": "proc-monitor",
            "tier": tier_label,
            "tier_reasons": tier_reasons,
            "process_metrics": monitor.to_json(),
            "syscalls": {},
        });

        // Merge CPU profiling data if available
        if !cpu_json.is_null() {
            if let Some(top_fns) = cpu_json["top_functions"].as_array() {
                let coverage = compute_benchmark_coverage(top_fns);
                cpu_json["benchmark_coverage"] = coverage;
            }
            cpu_json["sampling_freq_hz"] = serde_json::json!(freq);
            json["cpu"] = cpu_json;
        }

        // Parse bpftrace syscall output
        let syscall_path = out_path.join("syscall_raw.txt");
        if syscall_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&syscall_path) {
                json["syscalls"] = parse_syscall_output(&content);
            }
        }

        // Parse perf stat hardware counters
        let perf_stat_path = out_path.join("perf_stat_raw.txt");
        if perf_stat_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&perf_stat_path) {
                let hw = parse_perf_stat_output(&content);
                if hw.is_object() && !hw.as_object().is_none_or(|m| m.is_empty()) {
                    json["hardware_counters"] = hw;
                }
            }
        }

        // Parse bpftrace lock contention output
        let lock_path = out_path.join("lock_raw.txt");
        if lock_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&lock_path) {
                let lock_data = parse_lock_output(&content);
                if lock_data.is_object() && !lock_data.as_object().is_none_or(|m| m.is_empty()) {
                    json["lock_contention"] = lock_data;
                }
            }
        }

        // Parse bpftrace net connection lifecycle output
        let net_path = out_path.join("net_raw.txt");
        if net_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&net_path) {
                let net_data = parse_net_output(&content);
                if net_data.is_object() && !net_data.as_object().is_none_or(|m| m.is_empty()) {
                    json["net_connections"] = net_data;
                }
            }
        }

        // Collect dhat-heap.json output — search multiple locations
        if do_dhat {
            let dhat_dest = out_path.join("dhat-heap.json");
            if !dhat_dest.exists() {
                // Build candidate paths in priority order
                let mut candidates: Vec<std::path::PathBuf> = Vec::new();
                if let Some(ref search_dir) = dhat_search_dir {
                    candidates.push(std::path::PathBuf::from(search_dir).join("dhat-heap.json"));
                }
                // Try /proc/<pid>/cwd symlink (works if process just exited but /proc still accessible)
                candidates.push(std::path::PathBuf::from(format!(
                    "/proc/{pid}/cwd/dhat-heap.json"
                )));
                // CWD fallback (original behavior)
                candidates.push(std::path::PathBuf::from("dhat-heap.json"));

                for candidate in &candidates {
                    if candidate.exists() {
                        // Try rename first (fast, same filesystem), fall back to copy+remove (cross-device)
                        if std::fs::rename(candidate, &dhat_dest).is_err()
                            && std::fs::copy(candidate, &dhat_dest).is_ok()
                        {
                            let _ = std::fs::remove_file(candidate);
                        }
                        if dhat_dest.exists() {
                            eprintln!(
                                "proc-monitor: found dhat-heap.json at {}",
                                candidate.display()
                            );
                            break;
                        }
                    }
                }
            }
            if dhat_dest.exists() {
                let size = std::fs::metadata(&dhat_dest).map(|m| m.len()).unwrap_or(0);
                let summary = parse_dhat_summary(&dhat_dest);
                let mut dhat_json = serde_json::json!({
                    "file": "dhat-heap.json",
                    "size_bytes": size,
                });
                if !summary.is_null() {
                    dhat_json["summary"] = summary;
                }
                json["dhat"] = dhat_json;
                eprintln!(
                    "proc-monitor: collected dhat-heap.json ({:.1} KB)",
                    size as f64 / 1024.0
                );
            } else {
                eprintln!("proc-monitor: --dhat specified but no dhat-heap.json found (binary built with --features dhat-heap?)");
                if let Some(ref search_dir) = dhat_search_dir {
                    eprintln!("proc-monitor:   searched: {}/dhat-heap.json", search_dir);
                }
            }
        }

        // Read cgroup stats before cleanup (if scope exists)
        if let Some(ref scope) = cgroup_scope {
            json["cgroup"] = read_cgroup_stats(scope);
        } else if resource_profile_name.is_some() {
            // Profile was requested but cgroup setup failed
            json["cgroup"] = serde_json::json!({
                "enforced": false,
                "error": "cgroup v2 setup failed or unavailable",
            });
        }

        let results_path = out_path.join("profile_results.json");
        match serde_json::to_string_pretty(&json) {
            Ok(content) => match std::fs::write(&results_path, &content) {
                Ok(()) => eprintln!("proc-monitor: wrote {}", results_path.display()),
                Err(e) => eprintln!(
                    "proc-monitor: failed to write {}: {e}",
                    results_path.display()
                ),
            },
            Err(e) => eprintln!("proc-monitor: failed to serialize profile results: {e}"),
        }

        // Cleanup cgroup scope after writing results
        if let Some(ref scope) = cgroup_scope {
            cleanup_cgroup(scope);
        }
    }
}

/// Process perf data: generate flamegraph via inferno, extract top functions.
/// Saves folded stacks for diff flamegraph support.
/// Returns cpu JSON for profile_results.json.
fn process_perf_data(
    out_dir: &std::path::Path,
    perf_data: &std::path::Path,
    diff_baseline: Option<&str>,
) -> serde_json::Value {
    use std::process::{Command, Stdio};

    let perf_data_str = perf_data.to_string_lossy().to_string();
    let mut cpu_json = serde_json::json!({});

    // Log perf.data file size for diagnostics
    if let Ok(meta) = std::fs::metadata(perf_data) {
        let size_mb = meta.len() as f64 / (1024.0 * 1024.0);
        eprintln!(
            "proc-monitor: processing {} ({:.1} MB)",
            perf_data_str, size_mb
        );
    }

    // Check perf.data integrity before running the pipeline
    let header_check = Command::new("perf")
        .args(["report", "--header-only", "-i", &perf_data_str])
        .output();

    if let Ok(ref out) = header_check {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("data size field is 0") || stderr.contains("not properly terminated") {
            eprintln!(
                "proc-monitor: WARNING: {} appears corrupted (perf was not cleanly stopped)",
                perf_data_str
            );
            cpu_json["error"] =
                serde_json::json!("perf.data corrupted — perf record was not cleanly terminated");
            return cpu_json;
        }
    }

    // Generate folded stacks + flamegraph SVG via inferno pipeline
    let flamegraph_path = out_dir.join("cpu_flamegraph.svg");
    let folded_path = out_dir.join("cpu_folded.txt");
    let perf_script = Command::new("perf")
        .args(["script", "-i", &perf_data_str])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();

    if let Ok(mut script_proc) = perf_script {
        let collapse = Command::new("inferno-collapse-perf")
            .stdin(script_proc.stdout.take().unwrap())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();

        if let Ok(mut collapse_child) = collapse {
            // Timeout: kill pipeline if it hangs on corrupted/huge perf.data
            const FLAMEGRAPH_TIMEOUT_SECS: u64 = 120;
            if !poll_child_exit(&mut collapse_child, FLAMEGRAPH_TIMEOUT_SECS) {
                eprintln!(
                    "proc-monitor: flamegraph pipeline timed out after {FLAMEGRAPH_TIMEOUT_SECS}s, killing"
                );
                let _ = collapse_child.kill();
                let _ = script_proc.kill();
                let _ = collapse_child.wait();
                let _ = script_proc.wait();
            } else {
                // Read collapsed stacks from stdout
                use std::io::Read;
                let mut collapsed_data = Vec::new();
                if let Some(mut stdout) = collapse_child.stdout.take() {
                    let _ = stdout.read_to_end(&mut collapsed_data);
                }

                if !collapsed_data.is_empty() {
                    // Save folded stacks for diff flamegraph support
                    let _ = std::fs::write(&folded_path, &collapsed_data);

                    // Generate flamegraph from folded stacks
                    let fg_output = Command::new("inferno-flamegraph")
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::null())
                        .spawn()
                        .and_then(|mut fg_proc| {
                            use std::io::Write;
                            if let Some(ref mut stdin) = fg_proc.stdin {
                                let _ = stdin.write_all(&collapsed_data);
                            }
                            fg_proc.wait_with_output()
                        });

                    if let Ok(output) = fg_output {
                        if output.status.success() && !output.stdout.is_empty() {
                            let _ = std::fs::write(&flamegraph_path, &output.stdout);
                            eprintln!(
                                "proc-monitor: flamegraph saved to {} ({:.1}KB)",
                                flamegraph_path.display(),
                                output.stdout.len() as f64 / 1024.0
                            );
                        }
                    }

                    // Generate diff flamegraph if baseline folded stacks are available
                    if let Some(baseline_path) = diff_baseline {
                        let baseline = std::path::Path::new(baseline_path);
                        if baseline.exists() {
                            generate_diff_flamegraph(out_dir, baseline, &folded_path);
                        } else {
                            eprintln!(
                                "proc-monitor: diff baseline not found: {baseline_path}, skipping diff flamegraph"
                            );
                        }
                    }
                }
                let _ = script_proc.wait();
            }
        } else {
            let _ = script_proc.wait();
        }
    }

    if !flamegraph_path.exists() {
        eprintln!("proc-monitor: inferno not found or failed, skipping flamegraph generation");
    }

    // Extract top functions via perf report (quick summary for the HTML report)
    let perf_report = Command::new("perf")
        .args(["report", "--stdio", "--no-children", "-i", &perf_data_str])
        .output()
        .or_else(|_| {
            Command::new("sudo")
                .args([
                    "perf",
                    "report",
                    "--stdio",
                    "--no-children",
                    "-i",
                    &perf_data_str,
                ])
                .output()
        });

    if let Ok(report) = perf_report {
        if report.status.success() && !report.stdout.is_empty() {
            let report_str = String::from_utf8_lossy(&report.stdout);
            let mut top_functions = Vec::new();
            let mut total_samples: u64 = 0;

            for line in report_str.lines() {
                let line = line.trim();
                // perf report lines: "  XX.XX%  binary  [.] function_name"
                // kernel lines:      "  XX.XX%  [kernel]  [k] kernel_func"
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                // Skip kernel symbols — they dominate when profiling userspace apps
                if line.contains("[k]") || line.contains("[kernel") {
                    continue;
                }
                // Extract userspace function name from [.] marker
                if let Some(marker_pos) = line.find("[.] ") {
                    let func_name = line[marker_pos + 4..].trim();
                    // Parse percentage from the start of the line
                    let pct_part = line[..marker_pos].trim();
                    let pct_str = pct_part
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.strip_suffix('%'));
                    if let Some(pct_str) = pct_str {
                        if let Ok(pct) = pct_str.parse::<f64>() {
                            if !func_name.is_empty() && top_functions.len() < 20 {
                                let samples = (pct * 100.0) as u64; // approximate
                                total_samples += samples;
                                top_functions.push(serde_json::json!({
                                    "name": func_name,
                                    "percent": pct,
                                    "samples": samples,
                                }));
                            }
                        }
                    }
                }
            }

            if !top_functions.is_empty() {
                cpu_json = serde_json::json!({
                    "total_samples": total_samples,
                    "top_functions": top_functions,
                });
            }
        }
    }

    // Record flamegraph and perf.data paths in JSON
    if flamegraph_path.exists() {
        cpu_json["flamegraph_svg"] = serde_json::json!("cpu_flamegraph.svg");
    }
    if folded_path.exists() {
        cpu_json["folded_stacks_file"] = serde_json::json!("cpu_folded.txt");
    }
    let diff_path = out_dir.join("cpu_diff_flamegraph.svg");
    if diff_path.exists() {
        cpu_json["diff_flamegraph_svg"] = serde_json::json!("cpu_diff_flamegraph.svg");
    }
    let offcpu_path = out_dir.join("cpu_offcpu_flamegraph.svg");
    if offcpu_path.exists() {
        cpu_json["offcpu_flamegraph_svg"] = serde_json::json!("cpu_offcpu_flamegraph.svg");
    }
    cpu_json["perf_data_file"] = serde_json::json!("cpu_perf.data");

    cpu_json
}

// ---------------------------------------------------------------------------
// cgroup v2 resource scoping (Linux only)
// ---------------------------------------------------------------------------

/// Preset resource limits for cgroup v2 scoping.
struct ResourceProfile {
    name: String,
    cpu_quota: u64,
    cpu_period: u64,
    memory_max: u64,
}

/// Resolve a named resource profile to preset cgroup limits.
fn resolve_profile(name: &str) -> Option<ResourceProfile> {
    match name {
        "default" => Some(ResourceProfile {
            name: "default".into(),
            cpu_quota: 100_000,
            cpu_period: 100_000,
            memory_max: 512 * 1024 * 1024, // 512 MB
        }),
        _ => None,
    }
}

/// Active cgroup scope — holds the path for stats reading and cleanup.
#[cfg(target_os = "linux")]
struct CgroupScope {
    cgroup_path: std::path::PathBuf,
    profile: ResourceProfile,
}

/// Check whether cgroups v2 is available (unified hierarchy with cpu + memory controllers).
#[cfg(target_os = "linux")]
fn is_cgroupv2() -> bool {
    let Ok(controllers) = std::fs::read_to_string("/sys/fs/cgroup/cgroup.controllers") else {
        return false;
    };
    controllers.contains("cpu") && controllers.contains("memory")
}

/// Write content to a cgroup file, falling back to `sudo tee` on permission error.
#[cfg(target_os = "linux")]
fn cgroup_write(path: &std::path::Path, content: &str) -> Result<(), String> {
    match std::fs::write(path, content) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => run_sudo_write(path, content),
        Err(e) => Err(format!("write {}: {e}", path.display())),
    }
}

/// Write to a file using `sudo tee`.
#[cfg(target_os = "linux")]
fn run_sudo_write(path: &std::path::Path, content: &str) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new("sudo")
        .args(["tee", &path.to_string_lossy()])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("sudo tee: {e}"))?;
    if let Some(ref mut stdin) = child.stdin {
        use std::io::Write;
        let _ = stdin.write_all(content.as_bytes());
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("sudo tee wait: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "sudo tee {} failed: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Set up a cgroup v2 scope for the given PID with resource limits.
#[cfg(target_os = "linux")]
fn setup_cgroup(pid: u32, profile: ResourceProfile) -> Result<CgroupScope, String> {
    let cgroup_path = std::path::PathBuf::from(format!("/sys/fs/cgroup/conproxy-test-{pid}"));

    // Create cgroup directory
    if !cgroup_path.exists() && std::fs::create_dir(&cgroup_path).is_err() {
        // Try with sudo
        let status = std::process::Command::new("sudo")
            .args(["mkdir", "-p", &cgroup_path.to_string_lossy()])
            .status()
            .map_err(|e| format!("sudo mkdir: {e}"))?;
        if !status.success() {
            return Err(format!("failed to create {}", cgroup_path.display()));
        }
    }

    // Write cpu.max
    let cpu_max = format!("{} {}", profile.cpu_quota, profile.cpu_period);
    cgroup_write(&cgroup_path.join("cpu.max"), &cpu_max)?;

    // Write memory.max
    cgroup_write(
        &cgroup_path.join("memory.max"),
        &profile.memory_max.to_string(),
    )?;

    // Disable swap
    cgroup_write(&cgroup_path.join("memory.swap.max"), "0")?;

    // Migrate the target PID into the cgroup
    cgroup_write(&cgroup_path.join("cgroup.procs"), &pid.to_string())?;

    eprintln!(
        "proc-monitor: cgroup v2 scope created at {} (cpu.max={}, memory.max={} MB)",
        cgroup_path.display(),
        cpu_max,
        profile.memory_max / (1024 * 1024)
    );

    Ok(CgroupScope {
        cgroup_path,
        profile,
    })
}

/// Read cgroup v2 statistics after the process exits.
#[cfg(target_os = "linux")]
fn read_cgroup_stats(scope: &CgroupScope) -> serde_json::Value {
    let cg = &scope.cgroup_path;

    // Memory stats
    let memory_current = read_cgroup_u64(&cg.join("memory.current")).unwrap_or(0);
    let memory_peak = read_cgroup_u64(&cg.join("memory.peak")).unwrap_or(0);
    let memory_max = read_cgroup_u64(&cg.join("memory.max")).unwrap_or(scope.profile.memory_max);

    // Parse memory.events for OOM counts
    let mem_events = read_cgroup_kv(&cg.join("memory.events"));
    let oom_events = mem_events
        .get("oom")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let oom_kill_events = mem_events
        .get("oom_kill")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    let memory_utilization_pct = if memory_max > 0 {
        let peak_for_util = if memory_peak > 0 {
            memory_peak
        } else {
            memory_current
        };
        (peak_for_util as f64 / memory_max as f64) * 100.0
    } else {
        0.0
    };

    // CPU stats
    let cpu_stat = read_cgroup_kv(&cg.join("cpu.stat"));
    let usage_usec = cpu_stat
        .get("usage_usec")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let nr_periods = cpu_stat
        .get("nr_periods")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let nr_throttled = cpu_stat
        .get("nr_throttled")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let throttled_usec = cpu_stat
        .get("throttled_usec")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    let throttle_percent = if nr_periods > 0 {
        (nr_throttled as f64 / nr_periods as f64) * 100.0
    } else {
        0.0
    };

    serde_json::json!({
        "profile": {
            "name": scope.profile.name,
            "cpu_max": format!("{} {}", scope.profile.cpu_quota, scope.profile.cpu_period),
            "memory_max_bytes": scope.profile.memory_max,
        },
        "enforced": true,
        "memory": {
            "current_bytes": memory_current,
            "peak_bytes": memory_peak,
            "max_bytes": memory_max,
            "utilization_percent": (memory_utilization_pct * 10.0).round() / 10.0,
            "oom_events": oom_events,
            "oom_kill_events": oom_kill_events,
        },
        "cpu": {
            "usage_usec": usage_usec,
            "nr_periods": nr_periods,
            "nr_throttled": nr_throttled,
            "throttled_usec": throttled_usec,
            "throttle_percent": (throttle_percent * 10.0).round() / 10.0,
        },
    })
}

/// Read a single u64 value from a cgroup file.
#[cfg(target_os = "linux")]
fn read_cgroup_u64(path: &std::path::Path) -> Option<u64> {
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

/// Read key-value pairs from a cgroup file (e.g., cpu.stat, memory.events).
#[cfg(target_os = "linux")]
fn read_cgroup_kv(path: &std::path::Path) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    if let Ok(content) = std::fs::read_to_string(path) {
        for line in content.lines() {
            let mut parts = line.split_whitespace();
            if let (Some(key), Some(val)) = (parts.next(), parts.next()) {
                map.insert(key.to_string(), val.to_string());
            }
        }
    }
    map
}

/// Clean up a cgroup scope: migrate remaining PIDs to parent, rmdir.
#[cfg(target_os = "linux")]
fn cleanup_cgroup(scope: &CgroupScope) {
    let cg = &scope.cgroup_path;
    if !cg.exists() {
        return;
    }

    // Migrate any remaining PIDs to parent cgroup
    if let Ok(procs) = std::fs::read_to_string(cg.join("cgroup.procs")) {
        for pid_str in procs.lines() {
            let pid_str = pid_str.trim();
            if !pid_str.is_empty() {
                // Write to parent's cgroup.procs (root cgroup)
                let _ = cgroup_write(std::path::Path::new("/sys/fs/cgroup/cgroup.procs"), pid_str);
            }
        }
    }

    // Remove cgroup directory
    if std::fs::remove_dir(cg).is_err() {
        let _ = std::process::Command::new("sudo")
            .args(["rmdir", &cg.to_string_lossy()])
            .status();
    }
    eprintln!("proc-monitor: cgroup scope cleaned up: {}", cg.display());
}

/// Sweep stale `conproxy-test-*` cgroup directories whose PIDs no longer exist.
#[cfg(target_os = "linux")]
fn cleanup_stale_cgroups() {
    let cgroup_base = std::path::Path::new("/sys/fs/cgroup");
    let Ok(entries) = std::fs::read_dir(cgroup_base) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.starts_with("conproxy-test-") {
            continue;
        }
        // Extract PID from name
        let Some(pid_str) = name_str.strip_prefix("conproxy-test-") else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        // Check if the PID still exists
        let proc_path = format!("/proc/{pid}");
        if std::path::Path::new(&proc_path).exists() {
            continue;
        }
        // Stale — clean up
        let stale_path = cgroup_base.join(&*name_str);
        eprintln!(
            "proc-monitor: cleaning stale cgroup: {}",
            stale_path.display()
        );
        // Migrate any remaining procs first
        if let Ok(procs) = std::fs::read_to_string(stale_path.join("cgroup.procs")) {
            for line in procs.lines() {
                let line = line.trim();
                if !line.is_empty() {
                    let _ = cgroup_write(std::path::Path::new("/sys/fs/cgroup/cgroup.procs"), line);
                }
            }
        }
        if std::fs::remove_dir(&stale_path).is_err() {
            let _ = std::process::Command::new("sudo")
                .args(["rmdir", &stale_path.to_string_lossy()])
                .status();
        }
    }
}

/// Parse a DHAT v2 heap profile and extract summary statistics.
///
/// DHAT v2 format (key fields):
/// - `tg`: peak heap bytes (global max)
/// - `te`: bytes at exit (leaked)
/// - `pps[]`: program points — `tb` (total bytes), `tbk` (total blocks), `mb` (max live bytes), `eb` (end bytes)
/// - `ftbl[]`: frame table strings — "0xADDR: function_name (file:line)"
/// - `fs[]` on each pp: frame indices into `ftbl` (leaf → root)
fn parse_dhat_summary(path: &std::path::Path) -> serde_json::Value {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return serde_json::Value::Null,
    };
    let dhat: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return serde_json::Value::Null,
    };

    let peak_heap_bytes = dhat["tg"].as_u64().unwrap_or(0);
    let bytes_at_exit = dhat["te"].as_u64().unwrap_or(0);

    let pps = match dhat["pps"].as_array() {
        Some(a) => a,
        None => return serde_json::Value::Null,
    };
    let ftbl: Vec<&str> = dhat["ftbl"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let mut total_bytes: u64 = 0;
    let mut total_blocks: u64 = 0;

    // Collect (total_bytes, pp_index) for sorting
    struct PpInfo {
        tb: u64,
        tbk: u64,
        caller: String,
    }
    let mut pp_infos: Vec<PpInfo> = Vec::with_capacity(pps.len());

    for pp in pps {
        let tb = pp["tb"].as_u64().unwrap_or(0);
        let tbk = pp["tbk"].as_u64().unwrap_or(0);
        total_bytes += tb;
        total_blocks += tbk;

        // Resolve caller from frame stack — skip dhat/alloc internals
        let caller = if let Some(fs) = pp["fs"].as_array() {
            resolve_dhat_caller(fs, &ftbl)
        } else {
            "<unknown>".to_string()
        };

        pp_infos.push(PpInfo { tb, tbk, caller });
    }

    // Sort by total bytes descending, take top 10
    pp_infos.sort_by_key(|p| std::cmp::Reverse(p.tb));
    let top_sites: Vec<serde_json::Value> = pp_infos
        .iter()
        .take(10)
        .map(|info| {
            serde_json::json!({
                "function": info.caller,
                "total_bytes": info.tb,
                "blocks": info.tbk,
            })
        })
        .collect();

    serde_json::json!({
        "total_bytes_allocated": total_bytes,
        "total_blocks": total_blocks,
        "peak_heap_bytes": peak_heap_bytes,
        "bytes_at_exit": bytes_at_exit,
        "top_allocation_sites": top_sites,
    })
}

/// Resolve the most meaningful caller from a DHAT frame stack, skipping
/// allocator internals (alloc::, __rust_alloc, dhat::, malloc, etc.).
fn resolve_dhat_caller(fs: &[serde_json::Value], ftbl: &[&str]) -> String {
    let skip_prefixes = [
        "alloc::",
        "std::alloc::",
        "__rust_alloc",
        "__rdl_alloc",
        "dhat::",
        "<alloc::",
        "malloc",
        "realloc",
        "calloc",
        "__libc_malloc",
        "__GI___libc_malloc",
    ];

    for frame_idx in fs {
        let idx = frame_idx.as_u64().unwrap_or(u64::MAX) as usize;
        if idx >= ftbl.len() {
            continue;
        }
        let frame = ftbl[idx];
        // Frame format: "0xADDR: function_name (file:line)" or just "0xADDR: function_name"
        let func_name = if let Some(after_colon) = frame.find(": ") {
            let rest = &frame[after_colon + 2..];
            // Strip trailing " (file:line)" if present
            if let Some(paren) = rest.rfind(" (") {
                &rest[..paren]
            } else {
                rest
            }
        } else {
            frame
        };

        let should_skip = skip_prefixes
            .iter()
            .any(|prefix| func_name.starts_with(prefix));
        if !should_skip && !func_name.is_empty() {
            return func_name.to_string();
        }
    }
    "<unknown>".to_string()
}

fn check_tool(name: &str) -> bool {
    std::process::Command::new("which")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Known benchmark targets from benches/core_ops.rs.
const BENCHMARK_TARGETS: &[&str] = &[
    "normalize_query",
    "hash_query",
    "hash_query_exact",
    "jittered_ttl",
    "compute_config_fingerprint",
    "CacheStore::get",
    "CacheStore::insert",
    "CacheStore::stats",
    "verify_integrity",
    "ProxyMetrics::record_hit",
    "ProxyMetrics::record_miss",
    "ProxyMetrics::record_latency",
    "ProxyMetrics::snapshot",
    "to_prometheus",
    "compute_content_hash",
    "detect_drift",
    "DriftAggregator::record",
    "DriftAggregator::summary",
    "QueryRequest::validate",
    "similarity",
    "best_similarity",
];

/// Cross-reference CPU hot functions with Criterion benchmark targets.
fn compute_benchmark_coverage(top_functions: &[serde_json::Value]) -> serde_json::Value {
    let mut covered = Vec::new();
    let mut uncovered = Vec::new();

    for func in top_functions.iter().take(30) {
        let name = func["name"].as_str().unwrap_or("");
        let is_covered = BENCHMARK_TARGETS.iter().any(|target| name.contains(target));

        if is_covered {
            covered.push(name.to_string());
        } else if !name.is_empty()
            && !name.starts_with('[')
            && !name.starts_with("std::")
            && !name.starts_with("core::")
            && !name.starts_with("__")
        {
            uncovered.push(name.to_string());
        }
    }

    serde_json::json!({
        "covered": covered,
        "uncovered": uncovered,
    })
}

/// Parse bpftrace syscall_profile.bt output into structured JSON.
fn parse_syscall_output(content: &str) -> serde_json::Value {
    let mut result = serde_json::json!({});

    let sections = [
        "fadvise64",
        "madvise",
        "mmap",
        "write",
        "writev",
        "readv",
        "epoll_wait",
        "read",
    ];

    for section in &sections {
        let marker = format!("--- {section} ---");
        if let Some(start_idx) = content.find(&marker) {
            let block = &content[start_idx..];
            let end_idx = block[marker.len()..]
                .find("--- ")
                .map(|i| i + marker.len())
                .unwrap_or(block.len());
            let block = &block[..end_idx];

            let mut info = serde_json::json!({});
            for line in block.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("count:") {
                    if let Some(v) = extract_trailing_number(trimmed) {
                        info["count"] = serde_json::json!(v);
                    }
                } else if trimmed.starts_with("total bytes:") {
                    if let Some(v) = extract_trailing_number(trimmed) {
                        info["total_bytes"] = serde_json::json!(v);
                    }
                } else if trimmed.starts_with("total events:") {
                    if let Some(v) = extract_trailing_number(trimmed) {
                        info["total_events"] = serde_json::json!(v);
                    }
                } else if trimmed.starts_with("WILLNEED") {
                    if let Some(v) = extract_trailing_number(trimmed) {
                        info["willneed"] = serde_json::json!(v);
                    }
                } else if trimmed.starts_with("DONTNEED") {
                    if let Some(v) = extract_trailing_number(trimmed) {
                        info["dontneed"] = serde_json::json!(v);
                    }
                } else if trimmed.starts_with("avg latency us:") {
                    if let Some(v) = extract_trailing_number(trimmed) {
                        info["avg_latency_us"] = serde_json::json!(v);
                    }
                } else if trimmed.starts_with("avg events/call:") {
                    if let Some(v) = extract_trailing_number(trimmed) {
                        info["avg_events_per_call"] = serde_json::json!(v);
                    }
                }
            }
            result[section] = info;
        }
    }

    result
}

fn extract_trailing_number(s: &str) -> Option<u64> {
    s.rsplit_once(':').and_then(|(_, v)| v.trim().parse().ok())
}

/// Parse `perf stat` stderr output into structured hardware counter JSON.
///
/// perf stat outputs lines like:
///   3,214,567,890      cycles
///   5,891,234,567      instructions              #    1.83  insn per cycle
///      12,345,678      cache-misses              #    2.10% of all cache refs
///     588,123,456      cache-references
///       1,234,567      branch-misses             #    0.30% of all branches
///          45,678      page-faults
fn parse_perf_stat_output(content: &str) -> serde_json::Value {
    let mut cycles: u64 = 0;
    let mut instructions: u64 = 0;
    let mut cache_misses: u64 = 0;
    let mut cache_references: u64 = 0;
    let mut branch_misses: u64 = 0;
    let mut page_faults: u64 = 0;

    for line in content.lines() {
        let trimmed = line.trim();
        // Parse counter value: strip commas from the number part
        let parts: Vec<&str> = trimmed.splitn(2, char::is_whitespace).collect();
        if parts.len() < 2 {
            continue;
        }
        let num_str = parts[0].replace(',', "");
        let val: u64 = match num_str.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let rest = parts[1].trim();
        if rest.starts_with("cycles") {
            cycles = val;
        } else if rest.starts_with("instructions") {
            instructions = val;
        } else if rest.starts_with("cache-misses") {
            cache_misses = val;
        } else if rest.starts_with("cache-references") {
            cache_references = val;
        } else if rest.starts_with("branch-misses") {
            branch_misses = val;
        } else if rest.starts_with("page-faults") {
            page_faults = val;
        }
    }

    if cycles == 0 && instructions == 0 {
        return serde_json::json!({});
    }

    let ipc = if cycles > 0 {
        instructions as f64 / cycles as f64
    } else {
        0.0
    };
    let cache_miss_pct = if cache_references > 0 {
        cache_misses as f64 / cache_references as f64 * 100.0
    } else {
        0.0
    };

    serde_json::json!({
        "cycles": cycles,
        "instructions": instructions,
        "ipc": ipc,
        "cache_misses": cache_misses,
        "cache_references": cache_references,
        "cache_miss_percent": cache_miss_pct,
        "branch_misses": branch_misses,
        "page_faults": page_faults,
    })
}

/// Parse bpftrace lock_profile.bt output into structured JSON.
fn parse_lock_output(content: &str) -> serde_json::Value {
    let mut futex_wait_count: u64 = 0;
    let mut futex_wake_count: u64 = 0;
    let mut total_wait_us: u64 = 0;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("futex_wait count:") {
            futex_wait_count = extract_trailing_number(trimmed).unwrap_or(0);
        } else if trimmed.starts_with("futex_wake count:") {
            futex_wake_count = extract_trailing_number(trimmed).unwrap_or(0);
        } else if trimmed.starts_with("total wait us:") {
            total_wait_us = extract_trailing_number(trimmed).unwrap_or(0);
        }
    }

    if futex_wait_count == 0 && futex_wake_count == 0 {
        return serde_json::json!({});
    }

    let avg_wait_us = if futex_wait_count > 0 {
        total_wait_us as f64 / futex_wait_count as f64
    } else {
        0.0
    };

    serde_json::json!({
        "futex_wait_count": futex_wait_count,
        "futex_wake_count": futex_wake_count,
        "total_wait_us": total_wait_us,
        "avg_wait_us": avg_wait_us,
    })
}

/// Parse bpftrace net_profile.bt output into structured JSON.
fn parse_net_output(content: &str) -> serde_json::Value {
    let mut accept_count: u64 = 0;
    let mut accept_avg_lat_us: u64 = 0;
    let mut connect_count: u64 = 0;
    let mut connect_avg_lat_us: u64 = 0;
    let mut close_count: u64 = 0;
    let mut close_accept_ratio: u64 = 0;

    let mut current_section = "";
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("--- accept4 ---") {
            current_section = "accept4";
        } else if trimmed.starts_with("--- connect ---") {
            current_section = "connect";
        } else if trimmed.starts_with("--- close ---") {
            current_section = "close";
        } else if trimmed.starts_with("count:") {
            let v = extract_trailing_number(trimmed).unwrap_or(0);
            match current_section {
                "accept4" => accept_count = v,
                "connect" => connect_count = v,
                "close" => close_count = v,
                _ => {}
            }
        } else if trimmed.starts_with("avg latency us:") {
            let v = extract_trailing_number(trimmed).unwrap_or(0);
            match current_section {
                "accept4" => accept_avg_lat_us = v,
                "connect" => connect_avg_lat_us = v,
                _ => {}
            }
        } else if trimmed.starts_with("close/accept:") {
            close_accept_ratio = extract_trailing_number(trimmed).unwrap_or(0);
        }
    }

    if accept_count == 0 && connect_count == 0 && close_count == 0 {
        return serde_json::json!({});
    }

    serde_json::json!({
        "accept_count": accept_count,
        "accept_avg_latency_us": accept_avg_lat_us,
        "connect_count": connect_count,
        "connect_avg_latency_us": connect_avg_lat_us,
        "close_count": close_count,
        "close_accept_ratio": close_accept_ratio,
    })
}

/// Poll a child process for up to `timeout_secs`, returning `true` if it exited.
#[cfg(target_os = "linux")]
fn poll_child_exit(child: &mut std::process::Child, timeout_secs: u64) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {}
            Err(_) => return true,
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

/// Stop a child process gracefully, escalating from self-exit → SIGINT → SIGTERM → SIGKILL.
///
/// perf record often exits on its own once the monitored process dies.  We give it
/// a 5-second head start before sending any signal, then allocate 75% of the remaining
/// timeout to SIGINT (buffer flush) and 25% to SIGTERM.  This avoids interrupting
/// an in-progress perf flush with SIGTERM, which corrupts perf.data.
///
/// For `sudo`-wrapped processes we also `sudo kill -9` the entire process group.
#[cfg(target_os = "linux")]
fn stop_child_with_timeout(child: &mut std::process::Child, label: &str, timeout_secs: u64) {
    let child_pid = child.id() as i32;

    // 1. Self-exit poll — perf often exits on its own after the monitored process dies
    const SELF_EXIT_SECS: u64 = 5;
    let self_exit_wait = SELF_EXIT_SECS.min(timeout_secs);
    if poll_child_exit(child, self_exit_wait) {
        return;
    }

    let remaining = timeout_secs.saturating_sub(self_exit_wait);

    // 2. SIGINT (graceful — perf flushes buffers)
    #[allow(unsafe_code)]
    // SAFETY: `child_pid` is a valid child process PID obtained from `Command::id()`.
    // `libc::kill` with SIGINT is safe to call — signal is delivered asynchronously.
    unsafe {
        libc::kill(child_pid, libc::SIGINT);
    }

    // 3. Poll 75% of remaining timeout for SIGINT
    let sigint_wait = remaining * 3 / 4;
    if poll_child_exit(child, sigint_wait) {
        return;
    }

    // 4. SIGTERM (stronger signal, still allows cleanup)
    let elapsed = self_exit_wait + sigint_wait;
    eprintln!("proc-monitor: {label} did not exit after SIGINT within {elapsed}s, sending SIGTERM");
    #[allow(unsafe_code)]
    // SAFETY: `child_pid` is a valid child process PID. `libc::kill` with
    // SIGTERM is safe — signal delivery is asynchronous, no memory safety impact.
    unsafe {
        libc::kill(child_pid, libc::SIGTERM);
    }

    // 5. Poll remaining 25%
    let sigterm_wait = remaining.saturating_sub(sigint_wait);
    if poll_child_exit(child, sigterm_wait) {
        return;
    }

    // 6. SIGKILL (last resort)
    eprintln!("proc-monitor: {label} did not exit within {timeout_secs}s, sending SIGKILL");
    #[allow(unsafe_code)]
    // SAFETY: `child_pid` is a valid child PID. `-child_pid` refers to the process
    // group. `libc::kill` is safe to call — signal delivery is asynchronous.
    unsafe {
        // Kill the process group (negative PID) to catch sudo children
        libc::kill(-child_pid, libc::SIGKILL);
        libc::kill(child_pid, libc::SIGKILL);
    }
    // Also sudo kill in case the child is a privileged perf process
    let _ = std::process::Command::new("sudo")
        .args(["kill", "-9", &format!("{child_pid}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let _ = child.wait();
}

/// Generate off-CPU flamegraph from sched:sched_switch perf data.
fn process_offcpu_flamegraph(out_dir: &std::path::Path, perf_data: &std::path::Path) {
    use std::process::{Command, Stdio};

    let perf_data_str = perf_data.to_string_lossy().to_string();
    let flamegraph_path = out_dir.join("cpu_offcpu_flamegraph.svg");

    let perf_script = Command::new("sudo")
        .args(["perf", "script", "-i", &perf_data_str])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();

    if let Ok(mut script_proc) = perf_script {
        let collapse = Command::new("inferno-collapse-perf")
            .stdin(script_proc.stdout.take().unwrap())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();

        if let Ok(mut collapse_child) = collapse {
            const FLAMEGRAPH_TIMEOUT_SECS: u64 = 120;
            if !poll_child_exit(&mut collapse_child, FLAMEGRAPH_TIMEOUT_SECS) {
                eprintln!(
                    "proc-monitor: off-CPU flamegraph pipeline timed out after {FLAMEGRAPH_TIMEOUT_SECS}s, killing"
                );
                let _ = collapse_child.kill();
                let _ = script_proc.kill();
                let _ = collapse_child.wait();
                let _ = script_proc.wait();
            } else {
                use std::io::Read;
                let mut collapsed_data = Vec::new();
                if let Some(mut stdout) = collapse_child.stdout.take() {
                    let _ = stdout.read_to_end(&mut collapsed_data);
                }

                if !collapsed_data.is_empty() {
                    let fg_output = Command::new("inferno-flamegraph")
                        .args(["--color", "io"])
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::null())
                        .spawn()
                        .and_then(|mut fg_proc| {
                            use std::io::Write;
                            if let Some(ref mut stdin) = fg_proc.stdin {
                                let _ = stdin.write_all(&collapsed_data);
                            }
                            fg_proc.wait_with_output()
                        });

                    if let Ok(output) = fg_output {
                        if output.status.success() && !output.stdout.is_empty() {
                            let _ = std::fs::write(&flamegraph_path, &output.stdout);
                            eprintln!(
                                "proc-monitor: off-CPU flamegraph saved to {} ({:.1}KB)",
                                flamegraph_path.display(),
                                output.stdout.len() as f64 / 1024.0
                            );
                        }
                    }
                }
                let _ = script_proc.wait();
            }
        } else {
            let _ = script_proc.wait();
        }
    }

    if !flamegraph_path.exists() {
        eprintln!("proc-monitor: off-CPU flamegraph generation failed or inferno not available");
    }
}

/// Generate differential flamegraph comparing baseline folded stacks to current.
fn generate_diff_flamegraph(
    out_dir: &std::path::Path,
    baseline_folded: &std::path::Path,
    current_folded: &std::path::Path,
) {
    use std::process::{Command, Stdio};

    let diff_path = out_dir.join("cpu_diff_flamegraph.svg");
    let baseline_str = baseline_folded.to_string_lossy().to_string();
    let current_str = current_folded.to_string_lossy().to_string();

    // inferno-diff-folded <baseline> <current> | inferno-flamegraph --negate
    let diff_output = Command::new("inferno-diff-folded")
        .args([&baseline_str, &current_str])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();

    if let Ok(diff_out) = diff_output {
        if diff_out.status.success() && !diff_out.stdout.is_empty() {
            let fg_output = Command::new("inferno-flamegraph")
                .args(["--negate"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .and_then(|mut fg_proc| {
                    use std::io::Write;
                    if let Some(ref mut stdin) = fg_proc.stdin {
                        let _ = stdin.write_all(&diff_out.stdout);
                    }
                    fg_proc.wait_with_output()
                });

            if let Ok(output) = fg_output {
                if output.status.success() && !output.stdout.is_empty() {
                    let _ = std::fs::write(&diff_path, &output.stdout);
                    eprintln!(
                        "proc-monitor: diff flamegraph saved to {} ({:.1}KB)",
                        diff_path.display(),
                        output.stdout.len() as f64 / 1024.0
                    );
                }
            }
        }
    }

    if !diff_path.exists() {
        eprintln!(
            "proc-monitor: diff flamegraph generation failed (is inferno-diff-folded installed?)"
        );
    }
}

fn generate_markdown(json: &serde_json::Value, compare: Option<&serde_json::Value>) -> String {
    let suite = json["suite"].as_str().unwrap_or("unknown");
    let timestamp = json["timestamp"].as_str().unwrap_or("unknown");
    let duration_secs = json["duration_secs"].as_u64().unwrap_or(0);
    let passed = json["summary"]["passed"].as_u64().unwrap_or(0);
    let failed = json["summary"]["failed"].as_u64().unwrap_or(0);
    let total = json["summary"]["total"].as_u64().unwrap_or(0);
    let pass_rate = if total > 0 {
        passed as f64 / total as f64 * 100.0
    } else {
        0.0
    };
    let status = if failed == 0 {
        "ALL PASSED".to_string()
    } else {
        format!("{failed} FAILED")
    };
    let dur = if duration_secs >= 60 {
        format!("{}m {}s", duration_secs / 60, duration_secs % 60)
    } else {
        format!("{duration_secs}s")
    };

    let mut md = String::with_capacity(8_000);
    md.push_str(&format!(
        "# E2E Test Report: {suite}\n\n**Timestamp:** {timestamp}\n**Duration:** {dur}\n\n---\n\n"
    ));
    md.push_str(&format!("## Summary\n\n| Metric | Value |\n|--------|-------|\n| Total tests | {total} |\n| Passed | {passed} |\n| Failed | {failed} |\n| Pass rate | {pass_rate:.2}% |\n| Status | **{status}** |\n\n"));

    if let Some(tests) = json["tests"].as_array() {
        // Category breakdown
        let mut cats: std::collections::BTreeMap<String, (u64, u64)> =
            std::collections::BTreeMap::new();
        for t in tests {
            let name = t["name"].as_str().unwrap_or("Uncategorized");
            let cat = name.split(':').next().unwrap_or(name).trim().to_string();
            let e = cats.entry(cat).or_insert((0, 0));
            e.0 += 1;
            if t["status"].as_str() == Some("passed") {
                e.1 += 1;
            }
        }
        md.push_str("## Category Breakdown\n\n| Category | Total | Passed | Failed | Pass Rate |\n|----------|-------|--------|--------|----------|\n");
        for (cat, (ct, cp)) in &cats {
            let rate = if *ct > 0 {
                *cp as f64 / *ct as f64 * 100.0
            } else {
                0.0
            };
            md.push_str(&format!(
                "| {cat} | {ct} | {cp} | {} | {rate:.2}% |\n",
                ct - cp
            ));
        }
        md.push('\n');

        // Full test list
        md.push_str(
            "## Full Test List\n\n| Name | Status | Duration |\n|------|--------|----------|\n",
        );
        for t in tests {
            let name = t["name"].as_str().unwrap_or("unnamed");
            let st = if t["status"].as_str() == Some("passed") {
                "PASS"
            } else {
                "**FAIL**"
            };
            let dur = t["duration_ms"]
                .as_u64()
                .map(|d| format!("{d}ms"))
                .unwrap_or("N/A".into());
            md.push_str(&format!("| {name} | {st} | {dur} |\n"));
        }
        md.push('\n');
    }

    if let Some(prev) = compare {
        let pt = prev["summary"]["total"].as_u64().unwrap_or(0);
        let pp = prev["summary"]["passed"].as_u64().unwrap_or(0);
        let pf = prev["summary"]["failed"].as_u64().unwrap_or(0);
        let ps = prev["suite"].as_str().unwrap_or("unknown");
        md.push_str(&format!("## Comparison\n\nComparing against: **{ps}**\n\n| Metric | Previous | Current | Delta |\n|--------|----------|---------|-------|\n"));
        md.push_str(&format!(
            "| Total | {pt} | {total} | {} |\n",
            total as i64 - pt as i64
        ));
        md.push_str(&format!(
            "| Passed | {pp} | {passed} | {} |\n",
            passed as i64 - pp as i64
        ));
        md.push_str(&format!(
            "| Failed | {pf} | {failed} | {} |\n\n",
            failed as i64 - pf as i64
        ));
    }

    md.push_str("---\n\n*Generated by conproxy E2E test suite*\n");
    md
}

fn generate_html(json: &serde_json::Value, _compare: Option<&serde_json::Value>) -> String {
    let suite = json["suite"].as_str().unwrap_or("unknown");
    let passed = json["summary"]["passed"].as_u64().unwrap_or(0);
    let failed = json["summary"]["failed"].as_u64().unwrap_or(0);
    let total = json["summary"]["total"].as_u64().unwrap_or(0);
    let pass_rate = if total > 0 {
        passed as f64 / total as f64 * 100.0
    } else {
        0.0
    };

    let css = r#"  :root { --bg: #1a1a2e; --card: #161b22; --text: #e0e0e0; --link: #58a6ff; --border: #30363d; --green: #3fb950; --red: #f85149; --surface: #161b22; }
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, monospace; background: var(--bg); color: var(--text); padding: 2rem; line-height: 1.5; }
  h1 { font-size: 1.5rem; margin-bottom: .25rem; }
  h2 { font-size: 1.15rem; margin: 1.5rem 0 .8rem; color: var(--link); }
  table { width: 100%; border-collapse: collapse; margin-bottom: 1.2rem; }
  th, td { padding: .55rem .75rem; text-align: left; border: 1px solid var(--border); font-size: .85rem; }
  th { background: var(--surface); font-weight: 600; color: #8b949e; }
  .pass { color: var(--green); font-weight: 600; }
  .fail { color: var(--red); font-weight: 600; }
  footer { margin-top: 2rem; color: #555; font-size: .8rem; text-align: center; }"#;

    let status_cls = if failed == 0 { "pass" } else { "fail" };
    let status_txt = if failed == 0 {
        "ALL PASSED".to_string()
    } else {
        format!("{failed} FAILED")
    };

    let mut h = format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>E2E Report: {suite}</title>\n<style>\n{css}\n</style>\n</head>\n<body>\n\
         <h1>E2E Test Report: {suite}</h1>\n<h2>Summary</h2>\n<table>\n\
         <tr><th>Metric</th><th>Value</th></tr>\n\
         <tr><td>Total</td><td>{total}</td></tr>\n\
         <tr><td>Passed</td><td class=\"pass\">{passed}</td></tr>\n\
         <tr><td>Failed</td><td class=\"fail\">{failed}</td></tr>\n\
         <tr><td>Pass rate</td><td>{pass_rate:.2}%</td></tr>\n\
         <tr><td>Status</td><td class=\"{status_cls}\"><strong>{status_txt}</strong></td></tr>\n\
         </table>\n"
    );

    if let Some(tests) = json["tests"].as_array() {
        h.push_str(
            "<h2>Tests</h2>\n<table>\n<tr><th>Name</th><th>Status</th><th>Duration</th></tr>\n",
        );
        for t in tests {
            let name = t["name"].as_str().unwrap_or("unnamed");
            let is_pass = t["status"].as_str() == Some("passed");
            let cls = if is_pass { "pass" } else { "fail" };
            let st = if is_pass { "PASS" } else { "FAIL" };
            let dur = t["duration_ms"]
                .as_u64()
                .map(|d| format!("{d}ms"))
                .unwrap_or("N/A".into());
            h.push_str(&format!(
                "<tr><td>{name}</td><td class=\"{cls}\">{st}</td><td>{dur}</td></tr>\n"
            ));
        }
        h.push_str("</table>\n");
    }

    h.push_str("<footer>Generated by conproxy test suite</footer>\n</body>\n</html>\n");
    h
}
