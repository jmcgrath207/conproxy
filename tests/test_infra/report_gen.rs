use super::html;
use serde_json::Value;
use std::io;
use std::path::Path;

/// Generate a Markdown report from an E2E results JSON file.
///
/// Replaces `tests/e2e/scripts/generate_report.sh`.
pub fn generate_markdown_report(
    input: &Path,
    output: Option<&Path>,
    compare: Option<&Path>,
) -> io::Result<String> {
    let content = std::fs::read_to_string(input)?;
    let json: Value = serde_json::from_str(&content)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let suite = json["suite"].as_str().unwrap_or("unknown");
    let timestamp = json["timestamp"].as_str().unwrap_or("unknown");
    let duration_secs = json["duration_secs"].as_u64().unwrap_or(0);
    let passed = json["summary"]["passed"].as_u64().unwrap_or(0);
    let failed = json["summary"]["failed"].as_u64().unwrap_or(0);
    let total = json["summary"]["total"].as_u64().unwrap_or(0);

    let pass_rate = if total > 0 {
        (passed as f64 / total as f64) * 100.0
    } else {
        0.0
    };

    let status_badge = if failed == 0 {
        "ALL PASSED".to_string()
    } else {
        format!("{failed} FAILED")
    };

    let duration_fmt = if duration_secs >= 60 {
        format!("{}m {}s", duration_secs / 60, duration_secs % 60)
    } else {
        format!("{duration_secs}s")
    };

    let mut md = String::with_capacity(8_000);

    // Header
    md.push_str(&format!("# E2E Test Report: {suite}\n\n"));
    md.push_str(&format!("**Timestamp:** {timestamp}\n"));
    md.push_str(&format!("**Duration:** {duration_fmt}\n\n---\n\n"));

    // Summary table
    md.push_str("## Summary\n\n");
    md.push_str("| Metric | Value |\n|--------|-------|\n");
    md.push_str(&format!("| Total tests | {total} |\n"));
    md.push_str(&format!("| Passed | {passed} |\n"));
    md.push_str(&format!("| Failed | {failed} |\n"));
    md.push_str(&format!("| Pass rate | {pass_rate:.2}% |\n"));
    md.push_str(&format!("| Status | **{status_badge}** |\n\n"));

    // Category breakdown
    if let Some(tests) = json["tests"].as_array() {
        md.push_str("## Category Breakdown\n\n");
        md.push_str("| Category | Total | Passed | Failed | Pass Rate |\n");
        md.push_str("|----------|-------|--------|--------|----------|\n");

        let mut categories: std::collections::BTreeMap<String, (u64, u64)> =
            std::collections::BTreeMap::new();
        for test in tests {
            let name = test["name"].as_str().unwrap_or("Uncategorized");
            let cat = name.split(':').next().unwrap_or(name).trim().to_string();
            let is_pass = test["status"].as_str() == Some("passed");
            let entry = categories.entry(cat).or_insert((0, 0));
            entry.0 += 1;
            if is_pass {
                entry.1 += 1;
            }
        }

        for (cat, (cat_total, cat_passed)) in &categories {
            let cat_failed = cat_total - cat_passed;
            let rate = if *cat_total > 0 {
                (*cat_passed as f64 / *cat_total as f64) * 100.0
            } else {
                0.0
            };
            md.push_str(&format!(
                "| {cat} | {cat_total} | {cat_passed} | {cat_failed} | {rate:.2}% |\n"
            ));
        }
        md.push('\n');

        // Duration analysis
        let mut durations: Vec<(&str, u64)> = tests
            .iter()
            .filter_map(|t| {
                let name = t["name"].as_str()?;
                let dur = t["duration_ms"].as_u64()?;
                Some((name, dur))
            })
            .collect();

        if !durations.is_empty() {
            durations.sort_by_key(|d| d.1);
            let avg = durations.iter().map(|d| d.1).sum::<u64>() as f64 / durations.len() as f64;
            let p95_idx =
                ((durations.len() as f64 * 0.95).floor() as usize).min(durations.len() - 1);
            let p95 = durations[p95_idx].1;
            let fastest = durations.first().unwrap();
            let slowest = durations.last().unwrap();

            md.push_str("## Duration Analysis\n\n");
            md.push_str("| Metric | Value |\n|--------|-------|\n");
            md.push_str(&format!("| Fastest | {} ({}ms) |\n", fastest.0, fastest.1));
            md.push_str(&format!("| Slowest | {} ({}ms) |\n", slowest.0, slowest.1));
            md.push_str(&format!("| Average | {avg:.2}ms |\n"));
            md.push_str(&format!("| P95 | {p95}ms |\n\n"));
        }

        // Failed tests
        if failed > 0 {
            md.push_str("## Failed Tests\n\n");
            for t in tests
                .iter()
                .filter(|t| t["status"].as_str() != Some("passed"))
            {
                let name = t["name"].as_str().unwrap_or("unnamed");
                let status = t["status"].as_str().unwrap_or("unknown");
                let dur = t["duration_ms"]
                    .as_u64()
                    .map(|d| format!("{d}ms"))
                    .unwrap_or("N/A".into());
                let out = t["output"].as_str().unwrap_or("(no output)");
                md.push_str(&format!("### {name}\n\n"));
                md.push_str(&format!("- **Status:** {status}\n"));
                md.push_str(&format!("- **Duration:** {dur}\n\n"));
                md.push_str(&format!(
                    "<details>\n<summary>Output</summary>\n\n```\n{out}\n```\n\n</details>\n\n"
                ));
            }
        }

        // Full test list
        md.push_str("## Full Test List\n\n");
        md.push_str("| Name | Status | Duration |\n|------|--------|----------|\n");

        let mut sorted_tests: Vec<&Value> = tests.iter().collect();
        sorted_tests.sort_by(|a, b| {
            let ca = a["name"]
                .as_str()
                .unwrap_or("")
                .split(':')
                .next()
                .unwrap_or("");
            let cb = b["name"]
                .as_str()
                .unwrap_or("")
                .split(':')
                .next()
                .unwrap_or("");
            ca.cmp(cb)
                .then_with(|| a["name"].as_str().cmp(&b["name"].as_str()))
        });

        for t in &sorted_tests {
            let name = t["name"].as_str().unwrap_or("unnamed");
            let status = if t["status"].as_str() == Some("passed") {
                "PASS"
            } else {
                "**FAIL**"
            };
            let dur = t["duration_ms"]
                .as_u64()
                .map(|d| format!("{d}ms"))
                .unwrap_or("N/A".into());
            md.push_str(&format!("| {name} | {status} | {dur} |\n"));
        }
        md.push('\n');
    }

    // Comparison
    if let Some(compare_path) = compare {
        if compare_path.exists() {
            if let Ok(cmp_content) = std::fs::read_to_string(compare_path) {
                if let Ok(cmp_json) = serde_json::from_str::<Value>(&cmp_content) {
                    write_comparison(&mut md, &json, &cmp_json);
                }
            }
        }
    }

    // Footer
    md.push_str("---\n\n*Generated by conproxy E2E test suite*\n");

    if let Some(out_path) = output {
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(out_path, &md)?;
        eprintln!("Report written to {}", out_path.display());
    }

    Ok(md)
}

fn write_comparison(md: &mut String, current: &Value, previous: &Value) {
    let cur_total = current["summary"]["total"].as_u64().unwrap_or(0);
    let cur_passed = current["summary"]["passed"].as_u64().unwrap_or(0);
    let cur_failed = current["summary"]["failed"].as_u64().unwrap_or(0);
    let prev_total = previous["summary"]["total"].as_u64().unwrap_or(0);
    let prev_passed = previous["summary"]["passed"].as_u64().unwrap_or(0);
    let prev_failed = previous["summary"]["failed"].as_u64().unwrap_or(0);

    let cur_rate = if cur_total > 0 {
        cur_passed as f64 / cur_total as f64 * 100.0
    } else {
        0.0
    };
    let prev_rate = if prev_total > 0 {
        prev_passed as f64 / prev_total as f64 * 100.0
    } else {
        0.0
    };

    let prev_suite = previous["suite"].as_str().unwrap_or("unknown");
    let prev_ts = previous["timestamp"].as_str().unwrap_or("unknown");

    let delta_tests = cur_total as i64 - prev_total as i64;
    let delta_rate = cur_rate - prev_rate;

    let delta_fmt = |d: i64| -> String {
        if d > 0 {
            format!("+{d}")
        } else {
            format!("{d}")
        }
    };

    md.push_str(&format!(
        "## Comparison\n\nComparing against: **{prev_suite}** ({prev_ts})\n\n"
    ));
    md.push_str(
        "| Metric | Previous | Current | Delta |\n|--------|----------|---------|-------|\n",
    );
    md.push_str(&format!(
        "| Total tests | {prev_total} | {cur_total} | {} |\n",
        delta_fmt(delta_tests)
    ));
    md.push_str(&format!(
        "| Passed | {prev_passed} | {cur_passed} | {} |\n",
        delta_fmt(cur_passed as i64 - prev_passed as i64)
    ));
    md.push_str(&format!(
        "| Failed | {prev_failed} | {cur_failed} | {} |\n",
        delta_fmt(cur_failed as i64 - prev_failed as i64)
    ));
    md.push_str(&format!(
        "| Pass rate | {prev_rate:.2}% | {cur_rate:.2}% | {delta_rate:+.2}% |\n\n"
    ));

    // New and removed tests
    let cur_names: std::collections::HashSet<&str> = current["tests"]
        .as_array()
        .map(|a| a.iter().filter_map(|t| t["name"].as_str()).collect())
        .unwrap_or_default();
    let prev_names: std::collections::HashSet<&str> = previous["tests"]
        .as_array()
        .map(|a| a.iter().filter_map(|t| t["name"].as_str()).collect())
        .unwrap_or_default();

    let new_tests: Vec<&&str> = cur_names.difference(&prev_names).collect();
    let removed_tests: Vec<&&str> = prev_names.difference(&cur_names).collect();

    if !new_tests.is_empty() {
        md.push_str("### New Tests\n\n");
        for t in &new_tests {
            md.push_str(&format!("- {}\n", t));
        }
        md.push('\n');
    }

    if !removed_tests.is_empty() {
        md.push_str("### Removed Tests\n\n");
        for t in &removed_tests {
            md.push_str(&format!("- {}\n", t));
        }
        md.push('\n');
    }

    if new_tests.is_empty() && removed_tests.is_empty() {
        md.push_str("No tests were added or removed.\n\n");
    }
}

/// Generate an HTML report from an E2E results JSON file.
///
/// Uses the shared CSS from `html.rs`.
pub fn generate_html_report(input: &Path, output: &Path, compare: Option<&Path>) -> io::Result<()> {
    let content = std::fs::read_to_string(input)?;
    let json: Value = serde_json::from_str(&content)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

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

    let duration_fmt = if duration_secs >= 60 {
        format!("{}m {}s", duration_secs / 60, duration_secs % 60)
    } else {
        format!("{duration_secs}s")
    };

    let mut out = String::with_capacity(16_000);
    out.push_str(&html::html_head(&format!("E2E Report: {suite}")));

    out.push_str(&format!(
        "<h1>E2E Test Report: {}</h1>\n",
        html::html_escape(suite)
    ));
    out.push_str(&format!(
        "<p class=\"timestamp\">{} &mdash; {duration_fmt}</p>\n",
        html::html_escape(timestamp)
    ));

    // Summary table
    let status_class = if failed == 0 { "pass" } else { "fail" };
    let status_text = if failed == 0 {
        "ALL PASSED".to_string()
    } else {
        format!("{failed} FAILED")
    };

    out.push_str("<h2>Summary</h2>\n<table>\n");
    out.push_str("<tr><th>Metric</th><th>Value</th></tr>\n");
    out.push_str(&format!("<tr><td>Total tests</td><td>{total}</td></tr>\n"));
    out.push_str(&format!(
        "<tr><td>Passed</td><td class=\"pass\">{passed}</td></tr>\n"
    ));
    out.push_str(&format!(
        "<tr><td>Failed</td><td class=\"fail\">{failed}</td></tr>\n"
    ));
    out.push_str(&format!(
        "<tr><td>Pass rate</td><td>{pass_rate:.2}%</td></tr>\n"
    ));
    out.push_str(&format!(
        "<tr><td>Status</td><td class=\"{status_class}\"><strong>{status_text}</strong></td></tr>\n"
    ));
    out.push_str("</table>\n");

    // Full test list
    if let Some(tests) = json["tests"].as_array() {
        out.push_str("<h2>Tests</h2>\n<table>\n");
        out.push_str("<tr><th>Name</th><th>Status</th><th>Duration</th></tr>\n");

        let mut sorted_tests: Vec<&Value> = tests.iter().collect();
        sorted_tests.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));

        for t in &sorted_tests {
            let name = t["name"].as_str().unwrap_or("unnamed");
            let is_pass = t["status"].as_str() == Some("passed");
            let cls = if is_pass { "pass" } else { "fail" };
            let status_text = if is_pass { "PASS" } else { "FAIL" };
            let dur = t["duration_ms"]
                .as_u64()
                .map(|d| format!("{d}ms"))
                .unwrap_or("N/A".into());

            out.push_str(&format!(
                "<tr><td>{}</td><td class=\"{cls}\">{status_text}</td><td>{dur}</td></tr>\n",
                html::html_escape(name)
            ));

            if !is_pass {
                let output_text = t["output"].as_str().unwrap_or("(no output)");
                out.push_str(&format!(
                    "<tr><td colspan=\"3\"><details><summary>Output</summary><pre>{}</pre></details></td></tr>\n",
                    html::html_escape(output_text)
                ));
            }
        }
        out.push_str("</table>\n");
    }

    // Section metrics
    if let Some(sections) = json["section_metrics"].as_array() {
        if !sections.is_empty() {
            out.push_str("<h2>Section Metrics</h2>\n<table>\n");
            out.push_str("<tr><th>Section</th><th>Requests</th><th>Hits</th><th>Misses</th><th>Errors</th><th>Hit Rate</th></tr>\n");
            for s in sections {
                let section = s["section"].as_str().unwrap_or("?");
                let requests = s["requests"].as_u64().unwrap_or(0);
                let hits = s["hits"].as_u64().unwrap_or(0);
                let misses = s["misses"].as_u64().unwrap_or(0);
                let errors = s["errors"].as_u64().unwrap_or(0);
                let hit_rate = s["hit_rate"].as_str().unwrap_or("0%");
                out.push_str(&format!(
                    "<tr><td>{section}</td><td>{requests}</td><td>{hits}</td><td>{misses}</td><td>{errors}</td><td>{hit_rate}</td></tr>\n"
                ));
            }
            out.push_str("</table>\n");
        }
    }

    // Comparison section
    if let Some(compare_path) = compare {
        if compare_path.exists() {
            if let Ok(cmp_content) = std::fs::read_to_string(compare_path) {
                if let Ok(cmp_json) = serde_json::from_str::<Value>(&cmp_content) {
                    write_html_comparison(&mut out, &json, &cmp_json);
                }
            }
        }
    }

    out.push_str(&html::html_footer());

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output, &out)?;
    eprintln!("HTML report written to {}", output.display());
    Ok(())
}

fn write_html_comparison(out: &mut String, current: &Value, previous: &Value) {
    let cur_total = current["summary"]["total"].as_u64().unwrap_or(0);
    let cur_passed = current["summary"]["passed"].as_u64().unwrap_or(0);
    let prev_total = previous["summary"]["total"].as_u64().unwrap_or(0);
    let prev_passed = previous["summary"]["passed"].as_u64().unwrap_or(0);
    let prev_failed = previous["summary"]["failed"].as_u64().unwrap_or(0);
    let cur_failed = current["summary"]["failed"].as_u64().unwrap_or(0);
    let prev_suite = previous["suite"].as_str().unwrap_or("unknown");

    out.push_str(&format!(
        "<h2>Comparison vs {}</h2>\n<table>\n",
        html::html_escape(prev_suite)
    ));
    out.push_str("<tr><th>Metric</th><th>Previous</th><th>Current</th><th>Delta</th></tr>\n");
    out.push_str(&format!(
        "<tr><td>Total</td><td>{prev_total}</td><td>{cur_total}</td><td>{}</td></tr>\n",
        cur_total as i64 - prev_total as i64
    ));
    out.push_str(&format!(
        "<tr><td>Passed</td><td>{prev_passed}</td><td>{cur_passed}</td><td>{}</td></tr>\n",
        cur_passed as i64 - prev_passed as i64
    ));
    out.push_str(&format!(
        "<tr><td>Failed</td><td>{prev_failed}</td><td>{cur_failed}</td><td>{}</td></tr>\n",
        cur_failed as i64 - prev_failed as i64
    ));
    out.push_str("</table>\n");
}
