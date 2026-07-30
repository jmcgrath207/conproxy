use serde_json::Value;
use std::time::Instant;

/// Result of a single test execution.
pub struct TestResult {
    pub name: String,
    pub category: String,
    pub passed: bool,
    pub duration_ms: u64,
    pub output: String,
}

/// Per-section metrics snapshot.
pub struct SectionMetrics {
    pub section: String,
    pub description: &'static str,
    pub requests: u64,
    pub hits: u64,
    pub misses: u64,
    pub errors: u64,
    pub hit_rate: String,
}

/// Collects test results across all categories.
pub struct TestReport {
    pub suite: String,
    pub results: Vec<TestResult>,
    pub sections: Vec<SectionMetrics>,
    start: Instant,
}

impl TestReport {
    pub fn new(suite: &str) -> Self {
        Self {
            suite: suite.to_string(),
            results: Vec::new(),
            sections: Vec::new(),
            start: Instant::now(),
        }
    }

    pub fn record(&mut self, result: TestResult) {
        use std::io::Write;
        let mut stderr = std::io::stderr().lock();
        let icon = if result.passed {
            "\x1b[32mPASSED\x1b[0m"
        } else {
            "\x1b[31mFAILED\x1b[0m"
        };
        let _ = writeln!(
            stderr,
            "  {}: {} \x1b[2m({}ms)\x1b[0m",
            result.name, icon, result.duration_ms
        );
        if !result.passed && !result.output.is_empty() {
            let truncated: String = result.output.chars().take(500).collect();
            let _ = writeln!(stderr, "         \x1b[31mOutput: {}\x1b[0m", truncated);
        }
        let _ = stderr.flush();
        self.results.push(result);
    }

    /// Record a category as skipped (e.g. external-proxy mode).
    /// Appears in the report as a passed result named "<cat>: skipped".
    pub fn skip_category(&mut self, category: &str) {
        self.record(TestResult {
            name: format!("{category}: skipped"),
            category: category.to_string(),
            passed: true,
            duration_ms: 0,
            output: "skipped (external-proxy mode)".to_string(),
        });
    }

    pub fn record_section(&mut self, metrics: SectionMetrics) {
        use std::io::Write;
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(
            stderr,
            "  \x1b[36m[{}]\x1b[0m \x1b[2mrequests={} hits={} misses={} errors={} hit_rate={}\x1b[0m",
            metrics.section, metrics.requests, metrics.hits, metrics.misses, metrics.errors, metrics.hit_rate
        );
        let _ = stderr.flush();
        self.sections.push(metrics);
    }

    /// Look up a description for a section name.
    pub fn section_description(name: &str) -> &'static str {
        match name {
            "health+cache+query" => {
                "Health probe, cache warmup/hit/miss, query routing, pool status"
            }
            "operational" => "Readiness, pause/resume, eviction, context mgmt, client tracking",
            "metrics" => {
                "Prometheus metrics, latency percentiles, circuit breaker, cache correctness"
            }
            "cli" => "CLI search, context switching, seed fetch/list/clear",
            "content" => "Response body validation: batch, federated, stats, audit, Grafana",
            "relevance" => "BM25 keyword matching: doc-001/003/008 recall, batch relevance",
            "warmup" => "Bulk seed warmup via API and CLI, cache population verification",
            "resilience" => {
                "Failover, circuit breaker trip/recovery, degradation ladder, coalescing"
            }
            "reload" => "Hot config reload via /admin/reload, max_entries change detection",
            "cascade" => "Priority-based cascade queries, depth/score metrics, multi-upstream",
            "efficiency" => "Warm-cache hit rate (target >= 80%), warmup entry/request counters",
            "auth" => {
                "API key auth: public endpoints, 401 rejection, X-API-Key + Bearer acceptance"
            }
            "rate-limit" => "Token bucket rate limiting: 429 on burst, recovery after refill",
            "ttl" => "TTL lifecycle: miss → fresh hit → stale after 5s → miss after full expiry",
            _ => "",
        }
    }

    pub fn passed(&self) -> usize {
        self.results.iter().filter(|r| r.passed).count()
    }

    pub fn failed(&self) -> usize {
        self.results.iter().filter(|r| !r.passed).count()
    }

    pub fn print_summary(&self) {
        use std::io::Write;
        let mut stderr = std::io::stderr().lock();
        let elapsed = self.start.elapsed();
        let _ = writeln!(stderr);
        let _ = writeln!(stderr, "\x1b[1mTest Summary (suite: {})\x1b[0m", self.suite);
        let _ = writeln!(stderr, "============================================");
        if !self.sections.is_empty() {
            let _ = writeln!(stderr, "\x1b[1mPer-Section Metrics:\x1b[0m");
            for s in &self.sections {
                let _ = writeln!(
                    stderr,
                    "  {}: requests={} hits={} misses={} errors={} hit_rate={}",
                    s.section, s.requests, s.hits, s.misses, s.errors, s.hit_rate
                );
                if !s.description.is_empty() {
                    let _ = writeln!(stderr, "    \x1b[2m{}\x1b[0m", s.description);
                }
            }
            let _ = writeln!(stderr, "--------------------------------------------");
        }
        let _ = writeln!(
            stderr,
            "\x1b[32m{} passed\x1b[0m, \x1b[31m{} failed\x1b[0m, {} total in {:.1}s",
            self.passed(),
            self.failed(),
            self.results.len(),
            elapsed.as_secs_f64()
        );
        if self.failed() > 0 {
            let _ = writeln!(stderr, "\x1b[31mFailed tests:\x1b[0m");
            for r in self.results.iter().filter(|r| !r.passed) {
                let _ = writeln!(stderr, "  - {} ({})", r.name, r.category);
            }
        }
        let _ = writeln!(stderr, "============================================");
        let _ = stderr.flush();
    }

    /// Write results as JSON to the given path.
    pub fn write_json(&self, path: &std::path::Path) {
        let elapsed = self.start.elapsed();
        let tests: Vec<Value> = self
            .results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.name,
                    "category": r.category,
                    "status": if r.passed { "passed" } else { "failed" },
                    "duration_ms": r.duration_ms,
                    "output": r.output.chars().take(500).collect::<String>(),
                })
            })
            .collect();

        let section_metrics: Vec<Value> = self
            .sections
            .iter()
            .map(|s| {
                serde_json::json!({
                    "section": s.section,
                    "description": s.description,
                    "requests": s.requests,
                    "hits": s.hits,
                    "misses": s.misses,
                    "errors": s.errors,
                    "hit_rate": s.hit_rate,
                })
            })
            .collect();

        let report = serde_json::json!({
            "suite": self.suite,
            "timestamp": chrono_now(),
            "duration_secs": elapsed.as_secs(),
            "summary": {
                "passed": self.passed(),
                "failed": self.failed(),
                "total": self.results.len(),
            },
            "tests": tests,
            "section_metrics": section_metrics,
        });

        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(path, serde_json::to_string_pretty(&report).unwrap()) {
            eprintln!("Failed to write results JSON: {e}");
        } else {
            eprintln!("Results written to {}", path.display());
        }
    }

    /// Write results as an HTML report to the given path.
    ///
    /// If `resource_data` is provided (from `E2E_PROFILE=1`), appends a
    /// "Resource Profile" section with summary stats and sparkline charts.
    pub fn write_html(
        &self,
        path: &std::path::Path,
        resource_data: Option<&serde_json::Value>,
        profile_data: Option<&serde_json::Value>,
    ) {
        // Inline the shared CSS (same dark theme used across all reports)
        let css = r#"  :root { --bg: #1a1a2e; --card: #161b22; --accent: #0f3460; --text: #e0e0e0;
           --link: #58a6ff; --border: #30363d; --green: #3fb950; --red: #f85149;
           --yellow: #d29922; --surface: #161b22; }
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, monospace;
         background: var(--bg); color: var(--text); padding: 2rem; line-height: 1.5; }
  h1 { font-size: 1.5rem; margin-bottom: .25rem; }
  h2 { font-size: 1.15rem; margin: 1.5rem 0 .8rem; color: var(--link); }
  .timestamp { color: #888; margin-bottom: 1.5rem; font-size: .9rem; }
  table { width: 100%; border-collapse: collapse; margin-bottom: 1.2rem; }
  th, td { padding: .55rem .75rem; text-align: left; border: 1px solid var(--border); font-size: .85rem; }
  th { background: var(--surface); font-weight: 600; color: #8b949e; }
  tr:hover td { background: #1c2333; }
  .pass { color: var(--green); font-weight: 600; }
  .fail { color: var(--red); font-weight: 600; }
  details { margin: .4rem 0; }
  summary { cursor: pointer; font-size: .85rem; color: #8b949e; }
  summary:hover { color: var(--text); }
  pre { background: var(--surface); border: 1px solid var(--border); border-radius: 4px;
        padding: .6rem .8rem; font-size: .8rem; overflow-x: auto; white-space: pre-wrap; }
  footer { margin-top: 2rem; color: #555; font-size: .8rem; text-align: center; }"#;

        let elapsed = self.start.elapsed();
        let duration_secs = elapsed.as_secs();
        let duration_fmt = if duration_secs >= 60 {
            format!("{}m {}s", duration_secs / 60, duration_secs % 60)
        } else {
            format!("{duration_secs}s")
        };

        let pass_rate = if self.results.is_empty() {
            0.0
        } else {
            self.passed() as f64 / self.results.len() as f64 * 100.0
        };
        let status_class = if self.failed() == 0 { "pass" } else { "fail" };
        let status_text = if self.failed() == 0 {
            "ALL PASSED".to_string()
        } else {
            format!("{} FAILED", self.failed())
        };

        let mut html = String::with_capacity(16_000);

        // Head
        html.push_str(&format!(
            "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
             <title>E2E Report: {}</title>\n<style>\n{css}\n</style>\n</head>\n<body>\n",
            html_escape(&self.suite)
        ));

        html.push_str(&format!(
            "<h1>E2E Test Report: {}</h1>\n<p class=\"timestamp\">{} &mdash; {duration_fmt}</p>\n",
            html_escape(&self.suite),
            chrono_now()
        ));

        // Summary
        html.push_str("<h2>Summary</h2>\n<table>\n");
        html.push_str("<tr><th>Metric</th><th>Value</th></tr>\n");
        html.push_str(&format!(
            "<tr><td>Total tests</td><td>{}</td></tr>\n",
            self.results.len()
        ));
        html.push_str(&format!(
            "<tr><td>Passed</td><td class=\"pass\">{}</td></tr>\n",
            self.passed()
        ));
        html.push_str(&format!(
            "<tr><td>Failed</td><td class=\"fail\">{}</td></tr>\n",
            self.failed()
        ));
        html.push_str(&format!(
            "<tr><td>Pass rate</td><td>{pass_rate:.2}%</td></tr>\n"
        ));
        html.push_str(&format!(
            "<tr><td>Status</td><td class=\"{status_class}\"><strong>{status_text}</strong></td></tr>\n"
        ));
        html.push_str("</table>\n");

        // Tests table
        html.push_str("<h2>Tests</h2>\n<table>\n");
        html.push_str("<tr><th>Name</th><th>Category</th><th>Status</th><th>Duration</th></tr>\n");
        for r in &self.results {
            let cls = if r.passed { "pass" } else { "fail" };
            let st = if r.passed { "PASS" } else { "FAIL" };
            html.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td class=\"{cls}\">{st}</td><td>{}ms</td></tr>\n",
                html_escape(&r.name),
                html_escape(&r.category),
                r.duration_ms
            ));
            if !r.passed && !r.output.is_empty() {
                let truncated: String = r.output.chars().take(500).collect();
                html.push_str(&format!(
                    "<tr><td colspan=\"4\"><details><summary>Output</summary><pre>{}</pre></details></td></tr>\n",
                    html_escape(&truncated)
                ));
            }
        }
        html.push_str("</table>\n");

        // Section metrics
        if !self.sections.is_empty() {
            html.push_str("<h2>Section Metrics</h2>\n<table>\n");
            html.push_str("<tr><th>Section</th><th>Description</th><th>Requests</th><th>Hits</th><th>Misses</th><th>Errors</th><th>Hit Rate</th></tr>\n");
            for s in &self.sections {
                html.push_str(&format!(
                    "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
                    html_escape(&s.section), html_escape(s.description),
                    s.requests, s.hits, s.misses, s.errors, html_escape(&s.hit_rate)
                ));
            }
            html.push_str("</table>\n");
        }

        // Resource profile section (when E2E_PROFILE=1)
        if let Some(data) = resource_data {
            html.push_str("<h2>Resource Profile</h2>\n");

            if let Some(summary) = data.get("summary") {
                let peak_rss_mb =
                    summary["peak_rss_bytes"].as_u64().unwrap_or(0) as f64 / (1024.0 * 1024.0);
                let avg_rss_mb =
                    summary["avg_rss_bytes"].as_u64().unwrap_or(0) as f64 / (1024.0 * 1024.0);
                let cpu_pct = summary["cpu_percent"].as_f64().unwrap_or(0.0);
                let peak_threads = summary["peak_threads"].as_u64().unwrap_or(0);
                let peak_fds = summary["peak_fds"].as_u64().unwrap_or(0);

                html.push_str("<table>\n<tr><th>Metric</th><th>Value</th></tr>\n");
                html.push_str(&format!(
                    "<tr><td>Peak RSS</td><td>{peak_rss_mb:.1} MB</td></tr>\n"
                ));
                html.push_str(&format!(
                    "<tr><td>Avg RSS</td><td>{avg_rss_mb:.1} MB</td></tr>\n"
                ));
                html.push_str(&format!(
                    "<tr><td>CPU Usage</td><td>{cpu_pct:.1}%</td></tr>\n"
                ));
                html.push_str(&format!(
                    "<tr><td>Peak Threads</td><td>{peak_threads}</td></tr>\n"
                ));
                html.push_str(&format!("<tr><td>Peak FDs</td><td>{peak_fds}</td></tr>\n"));
                html.push_str(&format!(
                    "<tr><td>Context Switches (vol/invol)</td><td>{} / {}</td></tr>\n",
                    summary["total_voluntary_ctxt_switches"]
                        .as_u64()
                        .unwrap_or(0),
                    summary["total_nonvoluntary_ctxt_switches"]
                        .as_u64()
                        .unwrap_or(0),
                ));
                html.push_str("</table>\n");
            }

            // Sparkline charts from snapshots
            if let Some(snapshots) = data["snapshots"].as_array() {
                if snapshots.len() >= 2 {
                    let first_ts = snapshots[0]["timestamp_ms"].as_f64().unwrap_or(0.0);

                    let rss_points: Vec<(f64, f64)> = snapshots
                        .iter()
                        .map(|s| {
                            let t = s["timestamp_ms"].as_f64().unwrap_or(0.0) - first_ts;
                            let rss =
                                s["rss_bytes"].as_u64().unwrap_or(0) as f64 / (1024.0 * 1024.0);
                            (t / 1000.0, rss)
                        })
                        .collect();

                    let thread_points: Vec<(f64, f64)> = snapshots
                        .iter()
                        .map(|s| {
                            let t = s["timestamp_ms"].as_f64().unwrap_or(0.0) - first_ts;
                            (t / 1000.0, s["num_threads"].as_u64().unwrap_or(0) as f64)
                        })
                        .collect();

                    use crate::test_infra::html::render_sparkline_svg;
                    html.push_str("<div style=\"display:flex;gap:1rem;flex-wrap:wrap;\">\n");
                    html.push_str("<div>");
                    html.push_str(&render_sparkline_svg(
                        &rss_points,
                        "RSS (MB)",
                        "#58a6ff",
                        400,
                        100,
                    ));
                    html.push_str("</div>\n<div>");
                    html.push_str(&render_sparkline_svg(
                        &thread_points,
                        "Threads",
                        "#3fb950",
                        400,
                        100,
                    ));
                    // CPU % sparkline
                    let cpu_points: Vec<(f64, f64)> = snapshots
                        .iter()
                        .map(|s| {
                            let t = s["timestamp_ms"].as_f64().unwrap_or(0.0) - first_ts;
                            (t / 1000.0, s["cpu_percent"].as_f64().unwrap_or(0.0))
                        })
                        .collect();

                    // FD count sparkline
                    let fd_points: Vec<(f64, f64)> = snapshots
                        .iter()
                        .map(|s| {
                            let t = s["timestamp_ms"].as_f64().unwrap_or(0.0) - first_ts;
                            (t / 1000.0, s["fd_count"].as_u64().unwrap_or(0) as f64)
                        })
                        .collect();

                    html.push_str("<div>");
                    html.push_str(&render_sparkline_svg(
                        &cpu_points,
                        "CPU %",
                        "#f0883e",
                        400,
                        100,
                    ));
                    html.push_str("</div>\n<div>");
                    html.push_str(&render_sparkline_svg(
                        &fd_points, "FDs", "#d29922", 400, 100,
                    ));
                    html.push_str("</div>\n</div>\n");
                }
            }
        }

        // Full profiler data (CPU top functions, DHAT, flamegraph links)
        if let Some(pd) = profile_data {
            render_e2e_assessments(&mut html, pd);
            render_hardware_counters(&mut html, pd);
            render_syscall_summary(&mut html, pd);
            render_lock_summary(&mut html, pd);
            render_profiler_sections(&mut html, pd);
        }

        html.push_str("<footer>Generated by conproxy test suite</footer>\n</body>\n</html>\n");

        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(path, &html) {
            eprintln!("Failed to write HTML report: {e}");
        } else {
            eprintln!("HTML report written to {}", path.display());
        }
    }

    /// Panics with a summary if any test failed.
    pub fn assert_all_passed(&self) {
        if self.failed() > 0 {
            let failed_names: Vec<&str> = self
                .results
                .iter()
                .filter(|r| !r.passed)
                .map(|r| r.name.as_str())
                .collect();
            panic!(
                "{} tests failed: {}",
                self.failed(),
                failed_names.join(", ")
            );
        }
    }
}

fn chrono_now() -> String {
    // Simple ISO-8601 timestamp without external dep
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}s-since-epoch", d.as_secs())
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Render assessment cards from profile_data["assessments"].
fn render_e2e_assessments(html: &mut String, pd: &serde_json::Value) {
    let assessments = match pd["assessments"].as_array() {
        Some(arr) if !arr.is_empty() => arr,
        _ => return,
    };

    html.push_str("<h2>Assessments</h2>\n<div style=\"display:flex;flex-wrap:wrap;gap:.75rem;margin-bottom:1.2rem;\">\n");
    for a in assessments {
        let level = a["level"].as_str().unwrap_or("INFO");
        let metric = a["metric"].as_str().unwrap_or("");
        let message = a["message"].as_str().unwrap_or("");
        let border_color = match level {
            "FAIL" => "#f85149",
            "WARN" => "#d29922",
            "OK" => "#3fb950",
            _ => "#58a6ff",
        };
        html.push_str(&format!(
            "<div style=\"background:#161b22;border:1px solid #30363d;border-left:4px solid {border_color};\
             border-radius:6px;padding:.6rem .9rem;min-width:220px;\">\
             <strong style=\"color:{border_color}\">{level}</strong>\
             <span style=\"color:#8b949e;margin-left:.5rem\">{}</span>\
             <div style=\"font-size:.85rem;margin-top:.25rem\">{}</div></div>\n",
            html_escape(metric),
            html_escape(message),
        ));
    }
    html.push_str("</div>\n");
}

/// Render hardware counters section from profile_data["hardware_counters"].
fn render_hardware_counters(html: &mut String, pd: &serde_json::Value) {
    let hw = &pd["hardware_counters"];
    if !hw.is_object() {
        return;
    }

    let ipc = hw["ipc"].as_f64().unwrap_or(0.0);
    let cache_miss_pct = hw["cache_miss_percent"].as_f64().unwrap_or(0.0);
    let branch_misses = hw["branch_misses"].as_u64().unwrap_or(0);
    let page_faults = hw["page_faults"].as_u64().unwrap_or(0);
    let cycles = hw["cycles"].as_u64().unwrap_or(0);
    let instructions = hw["instructions"].as_u64().unwrap_or(0);

    if ipc == 0.0 && cycles == 0 && instructions == 0 {
        return;
    }

    let ipc_color = if ipc >= 1.5 {
        "#3fb950"
    } else if ipc >= 0.8 {
        "#d29922"
    } else {
        "#f85149"
    };
    let cache_color = if cache_miss_pct < 5.0 {
        "#3fb950"
    } else if cache_miss_pct < 15.0 {
        "#d29922"
    } else {
        "#f85149"
    };

    html.push_str("<h2>Hardware Counters</h2>\n\
        <div style=\"display:grid;grid-template-columns:repeat(auto-fill,minmax(150px,1fr));gap:.75rem;margin-bottom:1.2rem;\">\n");
    html.push_str(&format!(
        "<div style=\"background:#161b22;border:1px solid #30363d;border-radius:8px;padding:.8rem;text-align:center;\">\
         <div style=\"font-size:1.3rem;font-weight:bold;color:{ipc_color}\">{ipc:.2}</div>\
         <div style=\"font-size:.75rem;color:#8b949e\">IPC</div></div>\n"
    ));
    html.push_str(&format!(
        "<div style=\"background:#161b22;border:1px solid #30363d;border-radius:8px;padding:.8rem;text-align:center;\">\
         <div style=\"font-size:1.3rem;font-weight:bold;color:{cache_color}\">{cache_miss_pct:.1}%</div>\
         <div style=\"font-size:.75rem;color:#8b949e\">Cache Miss Rate</div></div>\n"
    ));
    html.push_str(&format!(
        "<div style=\"background:#161b22;border:1px solid #30363d;border-radius:8px;padding:.8rem;text-align:center;\">\
         <div style=\"font-size:1.3rem;font-weight:bold\">{branch_misses}</div>\
         <div style=\"font-size:.75rem;color:#8b949e\">Branch Misses</div></div>\n"
    ));
    html.push_str(&format!(
        "<div style=\"background:#161b22;border:1px solid #30363d;border-radius:8px;padding:.8rem;text-align:center;\">\
         <div style=\"font-size:1.3rem;font-weight:bold\">{page_faults}</div>\
         <div style=\"font-size:.75rem;color:#8b949e\">Page Faults</div></div>\n"
    ));
    html.push_str("</div>\n");
}

/// Render syscall summary table from profile_data["syscalls"].
fn render_syscall_summary(html: &mut String, pd: &serde_json::Value) {
    let syscalls = match pd["syscalls"].as_array() {
        Some(arr) if !arr.is_empty() => arr,
        _ => return,
    };

    html.push_str("<h2>Syscall Summary</h2>\n");
    html.push_str("<table>\n<tr><th>Syscall</th><th>Count</th><th>Avg Latency</th></tr>\n");
    for sc in syscalls {
        let name = sc["name"].as_str().unwrap_or("unknown");
        let count = sc["count"].as_u64().unwrap_or(0);
        let avg_lat = sc["avg_latency_us"].as_f64().unwrap_or(0.0);
        html.push_str(&format!(
            "<tr><td>{}</td><td>{count}</td><td>{avg_lat:.1}\u{00b5}s</td></tr>\n",
            html_escape(name),
        ));
    }
    html.push_str("</table>\n");
}

/// Render lock contention summary from profile_data["lock_contention"].
fn render_lock_summary(html: &mut String, pd: &serde_json::Value) {
    let lc = &pd["lock_contention"];
    if !lc.is_object() {
        return;
    }

    let futex_waits = lc["futex_waits"].as_u64().unwrap_or(0);
    let futex_wakes = lc["futex_wakes"].as_u64().unwrap_or(0);
    let total_wait_us = lc["total_wait_us"].as_u64().unwrap_or(0);
    let avg_wait_us = lc["avg_wait_us"].as_f64().unwrap_or(0.0);

    if futex_waits == 0 && futex_wakes == 0 {
        return;
    }

    html.push_str("<h2>Lock Contention</h2>\n\
        <div style=\"display:grid;grid-template-columns:repeat(auto-fill,minmax(150px,1fr));gap:.75rem;margin-bottom:1.2rem;\">\n");
    html.push_str(&format!(
        "<div style=\"background:#161b22;border:1px solid #30363d;border-radius:8px;padding:.8rem;text-align:center;\">\
         <div style=\"font-size:1.3rem;font-weight:bold\">{futex_waits}</div>\
         <div style=\"font-size:.75rem;color:#8b949e\">Futex Waits</div></div>\n"
    ));
    html.push_str(&format!(
        "<div style=\"background:#161b22;border:1px solid #30363d;border-radius:8px;padding:.8rem;text-align:center;\">\
         <div style=\"font-size:1.3rem;font-weight:bold\">{futex_wakes}</div>\
         <div style=\"font-size:.75rem;color:#8b949e\">Futex Wakes</div></div>\n"
    ));
    html.push_str(&format!(
        "<div style=\"background:#161b22;border:1px solid #30363d;border-radius:8px;padding:.8rem;text-align:center;\">\
         <div style=\"font-size:1.3rem;font-weight:bold\">{total_wait_us}\u{00b5}s</div>\
         <div style=\"font-size:.75rem;color:#8b949e\">Total Wait</div></div>\n"
    ));
    html.push_str(&format!(
        "<div style=\"background:#161b22;border:1px solid #30363d;border-radius:8px;padding:.8rem;text-align:center;\">\
         <div style=\"font-size:1.3rem;font-weight:bold\">{avg_wait_us:.1}\u{00b5}s</div>\
         <div style=\"font-size:.75rem;color:#8b949e\">Avg Wait</div></div>\n"
    ));
    html.push_str("</div>\n");
}

/// Render CPU top functions, DHAT summary, and flamegraph links from
/// the full profiler's profile_results.json data.
fn render_profiler_sections(html: &mut String, pd: &serde_json::Value) {
    // CPU profiling — top functions
    if let Some(top_fns) = pd["cpu"]["top_functions"].as_array() {
        if !top_fns.is_empty() {
            html.push_str("<h2>CPU Profiling &mdash; Top Functions</h2>\n");
            html.push_str(
                "<table>\n<tr><th>#</th><th>Function</th><th>%</th><th>Samples</th></tr>\n",
            );
            for (i, f) in top_fns.iter().take(10).enumerate() {
                let fname = f["name"].as_str().unwrap_or("unknown");
                let pct = f["percent"].as_f64().unwrap_or(0.0);
                let samples = f["samples"].as_u64().unwrap_or(0);
                html.push_str(&format!(
                    "<tr><td>{}</td><td>{}</td><td>{pct:.2}%</td><td>{samples}</td></tr>\n",
                    i + 1,
                    html_escape(fname),
                ));
            }
            html.push_str("</table>\n");
        }
    }

    // Flamegraph link
    if pd["cpu"]["flamegraph_svg"].is_string() {
        html.push_str(
            "<p style=\"margin:.5rem 0\"><a href=\"cpu_flamegraph.svg\" target=\"_blank\" \
             style=\"color:#58a6ff\">Open Flamegraph (SVG)</a></p>\n",
        );
    }

    // DHAT heap profile
    if pd["dhat"]["file"].is_string() {
        html.push_str("<h2>DHAT Heap Profile</h2>\n");

        let summary = &pd["dhat"]["summary"];
        if summary.is_object() {
            let total_alloc = summary["total_bytes_allocated"].as_u64().unwrap_or(0);
            let total_blocks = summary["total_blocks"].as_u64().unwrap_or(0);
            let peak_heap = summary["peak_heap_bytes"].as_u64().unwrap_or(0);
            let bytes_exit = summary["bytes_at_exit"].as_u64().unwrap_or(0);

            html.push_str("<table>\n<tr><th>Metric</th><th>Value</th></tr>\n");
            html.push_str(&format!(
                "<tr><td>Total Allocated</td><td>{}</td></tr>\n",
                format_bytes(total_alloc)
            ));
            html.push_str(&format!(
                "<tr><td>Total Blocks</td><td>{total_blocks}</td></tr>\n"
            ));
            html.push_str(&format!(
                "<tr><td>Peak Heap</td><td>{}</td></tr>\n",
                format_bytes(peak_heap)
            ));
            html.push_str(&format!(
                "<tr><td>Bytes at Exit</td><td>{}</td></tr>\n",
                format_bytes(bytes_exit)
            ));
            html.push_str("</table>\n");

            // Top allocation sites
            if let Some(sites) = summary["top_allocation_sites"].as_array() {
                if !sites.is_empty() {
                    html.push_str(
                        "<h3 style=\"margin-top:.8rem;font-size:1rem\">Top Allocation Sites</h3>\n",
                    );
                    html.push_str(
                        "<table>\n<tr><th>#</th><th>Function</th><th>Total Bytes</th><th>Blocks</th></tr>\n",
                    );
                    for (i, site) in sites.iter().take(10).enumerate() {
                        let func = site["function"].as_str().unwrap_or("<unknown>");
                        let tb = site["total_bytes"].as_u64().unwrap_or(0);
                        let blocks = site["blocks"].as_u64().unwrap_or(0);
                        html.push_str(&format!(
                            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{blocks}</td></tr>\n",
                            i + 1,
                            html_escape(func),
                            format_bytes(tb),
                        ));
                    }
                    html.push_str("</table>\n");
                }
            }
        }

        html.push_str(
            "<p style=\"margin:.5rem 0\"><a href=\"dhat-heap.json\" style=\"color:#58a6ff\">Download dhat-heap.json</a> · \
             <a href=\"https://nnethercote.github.io/dh_view/dh_view.html\" target=\"_blank\" style=\"color:#58a6ff\">Open DHAT Viewer</a></p>\n"
        );
    }
}

fn format_bytes(bytes: u64) -> String {
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

/// Macro that runs a single test, catching panics, timing execution,
/// and recording the result. One failure does not abort the suite.
#[macro_export]
macro_rules! run_test {
    ($report:expr, $cat:expr, $name:expr, $body:expr) => {{
        let start = std::time::Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| $body));
        let duration_ms = start.elapsed().as_millis() as u64;
        let (passed, output) = match result {
            Ok(_) => (true, String::new()),
            Err(e) => {
                let msg = if let Some(s) = e.downcast_ref::<String>() {
                    s.clone()
                } else if let Some(s) = e.downcast_ref::<&str>() {
                    s.to_string()
                } else {
                    "panicked".to_string()
                };
                (false, msg)
            }
        };
        $report.record($crate::helpers::report::TestResult {
            name: $name.to_string(),
            category: $cat.to_string(),
            passed,
            duration_ms,
            output,
        });
    }};
}

/// Assert expected cache behavior for a section snapshot.
///
/// Records pass/fail as test results (won't abort the suite on failure).
/// Only checks thresholds when the section had requests (skips idle sections).
///
/// Usage:
///   assert_section!(report, snapshot, "name", min_hits: 1, max_errors: 0);
///   assert_section!(report, snapshot, "name", min_hits: 3, min_hit_rate: 0.30, max_errors: 0);
///   assert_section!(report, snapshot, "name", max_errors: 0);
#[macro_export]
macro_rules! assert_section {
    ($report:expr, $snap:expr, $section:expr, $($key:ident : $val:expr),+ $(,)?) => {{
        let snap = $snap;
        let section = $section;
        $(
            $crate::_assert_section_field!($report, snap, section, $key, $val);
        )+
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! _assert_section_field {
    ($report:expr, $snap:expr, $section:expr, min_hits, $val:expr) => {
        if $snap.requests > 0 {
            let expected: u64 = $val;
            $crate::run_test!(
                $report,
                $section,
                &format!("Section {}: hits >= {}", $section, expected),
                {
                    assert!(
                        $snap.hits >= expected,
                        "Expected >= {} hits, got {} (requests={}, hit_rate={:.2})",
                        expected,
                        $snap.hits,
                        $snap.requests,
                        $snap.hit_rate
                    );
                }
            );
        }
    };
    ($report:expr, $snap:expr, $section:expr, min_hit_rate, $val:expr) => {
        if $snap.requests > 0 {
            let expected: f64 = $val;
            $crate::run_test!(
                $report,
                $section,
                &format!("Section {}: hit rate >= {:.0}%", $section, expected * 100.0),
                {
                    assert!(
                        $snap.hit_rate >= expected,
                        "Expected hit rate >= {:.2}, got {:.2} (hits={}, requests={})",
                        expected,
                        $snap.hit_rate,
                        $snap.hits,
                        $snap.requests
                    );
                }
            );
        }
    };
    ($report:expr, $snap:expr, $section:expr, max_errors, $val:expr) => {
        if $snap.requests > 0 {
            let expected: u64 = $val;
            $crate::run_test!(
                $report,
                $section,
                &format!("Section {}: errors <= {}", $section, expected),
                {
                    assert!(
                        $snap.errors <= expected,
                        "Expected <= {} errors, got {} (requests={})",
                        expected,
                        $snap.errors,
                        $snap.requests
                    );
                }
            );
        }
    };
}
