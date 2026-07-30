//! E2E Eval Test Harness: LLM vertical comparison (Ollama/Qwen default, Claude switchable).
//!
//! Compares 3 LLM configurations ("verticals") against the same set of eval queries,
//! measuring **document retrieval recall** as the golden metric.
//!
//! Run with: `cargo test --test e2e_eval --features proxy-embed -- --ignored --nocapture`
//!
//! The `proxy-embed` feature enables Qdrant data loading with real embeddings.
//! Without it, only Elasticsearch is loaded.
//!
//! Prerequisites:
//!   - Docker services running (Qdrant on 6333, ES on 9200)
//!   - Ollama running with model pulled (default), OR `claude` CLI (EVAL_PROVIDER=claude)
//!   - `conproxy` binary built: `cargo build --release --features release`
//!
//! The test automatically starts a conproxy proxy on the configured listen address.
//!
//! Environment variables:
//!   EVAL_PROVIDER            - "ollama" (default), "claude", or "llamacpp"
//!   EVAL_VERTICALS           - Comma-separated vertical filter (default: all 3)
//!   EVAL_QUERIES             - Comma-separated query ID filter (default: all)
//!   EVAL_OUTPUT_DIR          - Directory for JSON/HTML results
//!   EVAL_TIMEOUT_SECS        - Per-invocation timeout (default: 60)
//!   EVAL_QUERY_CONCURRENCY   - Max concurrent invocations per vertical (default: 3)
//!   EVAL_PROXY_LISTEN        - Proxy listen address (default: 127.0.0.1:8080)
//!   OLLAMA_BASE_URL          - Ollama server URL (default: http://localhost:11434)
//!   OLLAMA_MODEL             - Ollama model (default: qwen3:0.6b)
//!   CLAUDE_BIN               - Path to claude CLI (default: claude)
//!   EVAL_CLAUDE_MODEL        - Claude model override
//!   LLAMA_BASE_URL           - llama.cpp server URL (default: http://localhost:8081)
//!   LLAMA_MODEL              - llama.cpp model name (default: "auto")
//!   LLAMA_API_KEY            - API key for OpenAI-compat endpoint (default: sk-local)
//!   PROXY_BIN                - Path to conproxy binary (default: target/release/conproxy)

mod helpers;
mod verticals;

#[path = "../test_infra/mod.rs"]
mod test_infra;

use helpers::constants::{EvalConfig, EvalProvider};
use helpers::data::{
    load_eval_documents, load_eval_queries, partition_queries_by_vertical, DocIndex,
};
use helpers::proxy::{load_eval_docs_into_elasticsearch, load_eval_docs_into_qdrant, ProxyGuard};
use helpers::report::EvalReport;

#[test]
#[ignore = "E2E Eval: requires Docker + Ollama (or EVAL_PROVIDER=claude)"]
fn e2e_eval_suite() {
    let config = EvalConfig::from_env();
    let data_dir = config.project_root.join("tests").join("e2e").join("data");

    // Load eval data
    let queries = load_eval_queries(&data_dir);
    let documents = load_eval_documents(&data_dir);
    let doc_index = DocIndex::from_documents(&documents);

    // Initialize eval workspace (always cleans old files)
    config.init_base_dir();

    eprintln!();
    eprintln!("\x1b[1mE2E Eval Suite\x1b[0m");
    eprintln!("============================================");
    let fts_count = queries.iter().filter(|q| q.strategy == "fts").count();
    let vec_count = queries.iter().filter(|q| q.strategy == "vector").count();
    let cas_count = queries.iter().filter(|q| q.strategy == "cascade").count();
    eprintln!(
        "  Queries: {} total, 3 per vertical (1 FTS + 1 vector + 1 cascade)",
        queries.len(),
    );
    eprintln!(
        "  Strategy groups: {} FTS, {} vector, {} cascade",
        fts_count, vec_count, cas_count,
    );
    eprintln!("  Documents: {}", doc_index.docs.len());
    eprintln!(
        "  Matrix: {} verticals \u{00d7} 3 strategies = {} cells (1 query each)",
        config.enabled_verticals.len(),
        config.enabled_verticals.len() * 3,
    );
    eprintln!(
        "  Verticals: {}",
        config
            .enabled_verticals
            .iter()
            .map(|v| v.short_name())
            .collect::<Vec<_>>()
            .join(", ")
    );
    eprintln!("  Provider: {}", config.provider);
    eprintln!("  Model: {}", config.model_display());
    eprintln!("  Timeout: {}s", config.timeout.as_secs());
    eprintln!(
        "  Concurrency: {} queries/vertical",
        config.query_concurrency
    );
    eprintln!("  Base dir: {}", config.base_dir.display());

    // Check LLM provider availability
    match config.provider {
        EvalProvider::Ollama => assert_ollama_available(&config),
        EvalProvider::Claude => assert_claude_available(&config),
        EvalProvider::LlamaCpp => assert_llama_available(&config),
    }

    // Load eval documents into backends in parallel (ES and Qdrant are independent)
    std::thread::scope(|s| {
        let data_dir_ref = &data_dir;
        let es = s.spawn(move || load_eval_docs_into_elasticsearch(data_dir_ref));
        let qd = s.spawn(move || load_eval_docs_into_qdrant(data_dir_ref));
        es.join().expect("ES data load panicked");
        qd.join().expect("Qdrant data load panicked");
    });

    // Start the conproxy proxy (connects to Docker backends, killed on drop)
    let mut _proxy = ProxyGuard::start(&config.conproxy_bin, &config.proxy_listen);

    // SDK readiness check: verify the proxy is reachable via gRPC SDK
    {
        let sdk_url = format!("http://{}", config.proxy_listen);

        // Build tokio runtime first — `ConproxyClient::connect` triggers
        // `hyper_util::TokioExecutor::execute` (via tonic's `connect_lazy`),
        // which requires being inside a tokio runtime context (not just
        // having a runtime object in scope).
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to build tokio runtime for SDK check");

        // Connect inside the runtime via `block_on` so the lazy channel's
        // executor is initialised under the runtime.
        let sdk_client = rt
            .block_on(async { conproxy_sdk::ConproxyClient::connect(&sdk_url) })
            .expect("SDK: Failed to create ConproxyClient for eval proxy");

        // Verify gRPC stats endpoint responds
        let stats = rt
            .block_on(sdk_client.stats())
            .expect("SDK: proxy stats check failed — proxy may not be ready");
        eprintln!(
            "  SDK readiness: OK (uptime={}s, cache_entries={})",
            stats.uptime_secs, stats.cache_entries
        );

        // Run a test query via SDK to verify search pipeline works
        let query_result = rt.block_on(sdk_client.query("sdk eval readiness check", 3));
        match query_result {
            Ok(resp) => {
                eprintln!(
                    "  SDK search check: OK (results={}, took_ms={})",
                    resp.results.len(),
                    resp.took_ms
                );
            }
            Err(e) => {
                eprintln!(
                    "  SDK search check: query returned error (expected if no data yet): {e}"
                );
            }
        }
    }

    // Optional resource profiling when E2E_PROFILE=1
    #[cfg(target_os = "linux")]
    let (_proc_monitor_handle, mut _full_profiler) = {
        let do_profile = std::env::var("E2E_PROFILE").unwrap_or_default() == "1";
        if do_profile {
            let pid = _proxy.pid();
            eprintln!("\x1b[36m[PROFILE]\x1b[0m Starting process monitor (PID: {pid})...");
            let lightweight = Some(test_infra::proc_monitor::ProcMonitor::spawn_background(
                pid,
                std::time::Duration::from_secs(2),
            ));
            let full = spawn_full_profiler(pid, &config.conproxy_bin, config.output_dir.as_deref());
            (lightweight, full)
        } else {
            (None, None)
        }
    };
    #[cfg(not(target_os = "linux"))]
    let mut _full_profiler: Option<std::process::Child> = None;

    // Partition queries so each vertical gets exactly 3 (1 per strategy).
    // Apply EVAL_QUERIES filter first if set, then partition the remainder.
    let filtered_queries: Vec<_> = if let Some(ref ids) = config.enabled_queries {
        queries
            .into_iter()
            .filter(|q| ids.contains(&q.id))
            .collect()
    } else {
        queries
    };
    let partitioned = partition_queries_by_vertical(&filtered_queries, &config.enabled_verticals);

    // Pre-borrow shared read-only data so `move` closures capture references (which are Copy)
    let doc_index_ref = &doc_index;
    let config_ref = &config;
    let partitioned_ref = &partitioned;

    // Run verticals in parallel — each gets its own isolated dir and unique query subset
    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = config_ref
            .enabled_verticals
            .iter()
            .enumerate()
            .map(|(i, &vertical)| {
                s.spawn(move || {
                    verticals::run_vertical(
                        vertical,
                        &partitioned_ref[i],
                        doc_index_ref,
                        config_ref,
                    )
                })
            })
            .collect();

        handles
            .into_iter()
            .map(|h| h.join().expect("Vertical thread panicked"))
            .collect()
    });

    let mut report = EvalReport::new(Some(&config.model_display()));
    for result in results {
        report.add_vertical(result);
    }

    // Print comparison table
    report.print_comparison_table();

    // SDK post-eval verification: capture proxy stats after all eval queries
    {
        let sdk_url = format!("http://{}", config.proxy_listen);

        // Build tokio runtime first (see SDK readiness check above for rationale).
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to build tokio runtime for SDK post-check");

        if let Ok(sdk_client) =
            rt.block_on(async { conproxy_sdk::ConproxyClient::connect(&sdk_url) })
        {
            if let Ok(stats) = rt.block_on(sdk_client.stats()) {
                eprintln!();
                eprintln!("\x1b[36m[SDK]\x1b[0m Post-eval proxy stats:");
                eprintln!(
                    "  cache_entries={} hits={} misses={} hit_rate={:.1}%",
                    stats.cache_entries,
                    stats.total_hits,
                    stats.total_misses,
                    stats.hit_rate * 100.0
                );
                eprintln!(
                    "  upstream_requests={} upstream_failures={} degradation={}",
                    stats.upstream_requests, stats.upstream_failures, stats.degradation_level
                );
            }

            if let Ok(pool) = rt.block_on(sdk_client.pool_status()) {
                eprintln!(
                    "  upstreams: total={} healthy={} degraded={} offline={}",
                    pool.total_upstreams,
                    pool.healthy_upstreams,
                    pool.degraded_upstreams,
                    pool.offline_upstreams
                );
            }

            if let Ok(circuit) = rt.block_on(sdk_client.circuit_status()) {
                eprintln!(
                    "  circuit: state={} failures={} consecutive_failures={}",
                    circuit.state, circuit.failure_count, circuit.consecutive_failures
                );
            }
        }
    }

    // Assert every vertical exercised at least one query per strategy.
    let required_strategies = ["fts", "vector", "cascade"];
    for v in &report.verticals {
        for strat in &required_strategies {
            let count = v.queries.iter().filter(|q| q.strategy == *strat).count();
            assert!(
                count > 0,
                "Vertical '{}' has no queries for strategy '{}'. \
                 Each vertical must exercise every routing strategy at least once.",
                v.vertical.name(),
                strat,
            );
        }
    }

    // Drop the proxy explicitly so dhat-heap.json is written before the profiler collects it
    if _full_profiler.is_some() {
        eprintln!("\x1b[36m[PROFILE]\x1b[0m Stopping proxy for dhat collection...");
    }
    drop(_proxy);

    // Wait for full profiler to finish (it watches the PID and exits when it dies)
    if let Some(ref mut child) = _full_profiler {
        eprintln!("\x1b[36m[PROFILE]\x1b[0m Waiting for full profiler to finish...");
        let _ = child.wait();
    }

    // Stop ProcMonitor and collect resource data
    #[cfg(target_os = "linux")]
    let resource_data: Option<serde_json::Value> = _proc_monitor_handle.map(|h| {
        eprintln!("\x1b[36m[PROFILE]\x1b[0m Stopping process monitor...");
        let monitor = h.stop();
        let data = monitor.to_json();
        eprintln!(
            "\x1b[36m[PROFILE]\x1b[0m Collected {} snapshots",
            data["snapshots"].as_array().map(|a| a.len()).unwrap_or(0)
        );
        data
    });
    #[cfg(not(target_os = "linux"))]
    let resource_data: Option<serde_json::Value> = None;

    // Read full profiler data (CPU top functions, DHAT, etc.) if available
    let profile_data: Option<serde_json::Value> = config.output_dir.as_ref().and_then(|dir| {
        let path = dir.join("profile_results.json");
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    });

    // Write JSON + HTML results if output dir specified
    if let Some(ref dir) = config.output_dir {
        report.write_json(&dir.join("eval_results.json"));
        report.write_html(
            &dir.join("eval_results.html"),
            &documents,
            resource_data.as_ref(),
            profile_data.as_ref(),
        );

        // Write resource profile if profiling was enabled
        if let Some(ref data) = resource_data {
            let resource_path = dir.join("resource_profile.json");
            if let Ok(json_str) = serde_json::to_string_pretty(data) {
                let _ = std::fs::write(&resource_path, json_str);
                eprintln!("Resource profile written to {}", resource_path.display());
            }
        }
    }
}

/// Spawn the full profiler (`test_runner proc-monitor`) targeting the proxy PID.
///
/// The test_runner binary is in the same target directory as the conproxy binary.
/// Returns `None` if the binary is not found or spawn fails (graceful degradation).
fn spawn_full_profiler(
    proxy_pid: u32,
    conproxy_bin: &std::path::Path,
    output_dir: Option<&std::path::Path>,
) -> Option<std::process::Child> {
    let output_dir = output_dir?;

    // Derive test_runner binary path from conproxy binary path (same target dir)
    let target_dir = conproxy_bin.parent()?;
    let test_runner = target_dir.join("test_runner");

    if !test_runner.exists() {
        eprintln!(
            "\x1b[33m[PROFILE]\x1b[0m test_runner not found at {} — skipping full profiler",
            test_runner.display()
        );
        return None;
    }

    // The proxy CWD is /tmp/conproxy-eval-proxy/ — that's where dhat-heap.json will be written
    let dhat_search_dir = "/tmp/conproxy-eval-proxy";

    let mut cmd = std::process::Command::new(&test_runner);
    cmd.args([
        "proc-monitor",
        "--pid",
        &proxy_pid.to_string(),
        "--output-dir",
        &output_dir.to_string_lossy(),
        "--perf",
        "--bpftrace",
        "--dhat",
        "--dhat-search-dir",
        dhat_search_dir,
    ]);
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::inherit());

    match cmd.spawn() {
        Ok(child) => {
            eprintln!(
                "\x1b[36m[PROFILE]\x1b[0m Full profiler started (PID: {}, targeting proxy PID: {proxy_pid})",
                child.id()
            );
            Some(child)
        }
        Err(e) => {
            eprintln!(
                "\x1b[33m[PROFILE]\x1b[0m Failed to spawn full profiler: {e} — falling back to lightweight monitor only"
            );
            None
        }
    }
}

/// Verify Ollama is running and the configured model is available.
fn assert_ollama_available(config: &EvalConfig) {
    let runner = helpers::ollama::OllamaRunner::new(
        &config.ollama_base_url,
        &config.ollama_model,
        config.timeout,
    );
    match runner.check_ready() {
        Ok(()) => {
            eprintln!(
                "  Ollama: {} at {}",
                config.ollama_model, config.ollama_base_url
            );
        }
        Err(e) => {
            panic!(
                "Ollama not available: {e}\n\
                 Ensure Ollama is running and model '{}' is pulled.\n\
                 Set OLLAMA_BASE_URL / OLLAMA_MODEL env vars if needed.",
                config.ollama_model
            );
        }
    }
}

/// Verify the Claude CLI binary is available on the host.
fn assert_claude_available(config: &EvalConfig) {
    let output = std::process::Command::new(&config.claude_bin)
        .arg("--version")
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let version = String::from_utf8_lossy(&o.stdout);
            eprintln!("  Claude CLI: {}", version.trim());
        }
        Ok(o) => {
            panic!(
                "Claude CLI at '{}' returned error: {}",
                config.claude_bin.display(),
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
        Err(e) => {
            panic!(
                "Claude CLI not found at '{}': {e}\n\
                 Set CLAUDE_BIN env var to the path of the claude CLI binary.",
                config.claude_bin.display()
            );
        }
    }
}

/// Verify llama.cpp / OpenAI-compatible server is running and model available.
fn assert_llama_available(config: &EvalConfig) {
    let runner = helpers::openai_compat::OpenAiCompatRunner::new(
        &config.llama_base_url,
        &config.llama_model,
        &config.llama_api_key,
        config.timeout,
    );
    match runner.check_ready() {
        Ok(()) => {
            eprintln!(
                "  llama.cpp: {} at {}",
                config.llama_model, config.llama_base_url
            );
        }
        Err(e) => {
            panic!(
                "llama.cpp not available: {e}\n\
                 Ensure llama-server is running and model is available.\n\
                 Default: `llama-server -m models/llm.gguf --port 8081 --host 127.0.0.1 -c 2048`\n\
                 Set LLAMA_BASE_URL / LLAMA_MODEL env vars if needed.",
            );
        }
    }
}
