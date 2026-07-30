use super::html;
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

/// Scan a results directory and generate an `index.html` summarizing all outputs.
///
/// Replaces `tests/e2e/scripts/generate_index.sh`.
pub fn generate_index(results_dir: &Path) -> io::Result<()> {
    if !results_dir.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} is not a directory", results_dir.display()),
        ));
    }

    // Generate section-level HTML reports before walking the directory
    // so the generated files appear in the index listing.
    generate_section_reports(results_dir);

    let timestamp = results_dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    // Collect files grouped by top-level subdirectory
    let mut sections: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for entry in walkdir::WalkDir::new(results_dir)
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
            .strip_prefix(results_dir)
            .unwrap()
            .to_string_lossy()
            .into_owned();

        if rel == "index.html" {
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

    // Ordered section display
    let ordered = ["lint", "unit", "coverage", "bench", "e2e", "load", "eval"];
    let section_label = |s: &str| -> &'static str {
        match s {
            "lint" => "Lint & Format",
            "unit" => "Unit Tests",
            "coverage" => "Code Coverage",
            "bench" => "Benchmarks",
            "e2e" => "E2E Proxy Tests",
            "load" => "Load Tests (rlt)",
            "eval" => "Evaluation",
            _ => "Other",
        }
    };

    let mut out = String::with_capacity(8_000);
    out.push_str(&html::html_head("Test Results"));

    out.push_str(&format!(
        "<h1>Conpack Test Results</h1>\n<p class=\"timestamp\">Run: {timestamp}</p>\n<div class=\"grid\">\n"
    ));

    // Emit ordered sections first, then any remaining
    let mut emitted = std::collections::HashSet::new();
    let emit_section = |out: &mut String, name: &str, files: &[String]| {
        if files.is_empty() {
            return;
        }
        let label = section_label(name);
        out.push_str(&format!("<div class=\"card\">\n<h2>{label}</h2>\n<ul>\n"));

        for f in files {
            let fname = f.rsplit('/').next().unwrap_or(f);
            let ext = fname.rsplit('.').next().unwrap_or("");
            let badge = match ext {
                "html" => r#" <span class="badge html">HTML</span>"#,
                "json" => r#" <span class="badge json">JSON</span>"#,
                "txt" => r#" <span class="badge txt">TXT</span>"#,
                _ => "",
            };
            out.push_str(&format!("<li><a href=\"{f}\">{fname}</a>{badge}</li>\n"));
        }

        out.push_str("</ul>\n</div>\n");
    };

    for &name in &ordered {
        if let Some(files) = sections.get(name) {
            emit_section(&mut out, name, files);
            emitted.insert(name.to_string());
        }
    }

    for (name, files) in &sections {
        if !emitted.contains(name.as_str()) {
            emit_section(&mut out, name, files);
        }
    }

    out.push_str("</div>\n");
    out.push_str(&html::html_footer());

    let index_path = results_dir.join("index.html");
    std::fs::write(&index_path, &out)?;
    eprintln!("Generated {}", index_path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// section report generation
// ---------------------------------------------------------------------------

/// Generate HTML reports for sections that don't already have one.
fn generate_section_reports(dir: &Path) {
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
}

// ---- lint report ----

fn generate_lint_report(dir: &Path) -> Result<(), String> {
    let content =
        std::fs::read_to_string(dir.join("output.txt")).map_err(|e| format!("Read: {e}"))?;

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

    let mut h = String::with_capacity(4_000);
    h.push_str(&html::html_head("Lint & Format Report"));
    h.push_str(&format!(
        "<h1>Lint &amp; Format Report</h1>\n\
         <p>Overall: <span class=\"{overall_class}\"><strong>{overall}</strong></span></p>\n"
    ));

    h.push_str("<div class=\"grid\">\n");
    let fmt_class = if fmt_status == "PASS" { "pass" } else { "fail" };
    h.push_str(&format!(
        "<div class=\"stat\"><div class=\"value {fmt_class}\">{fmt_status}</div>\
         <div class=\"label\">cargo fmt</div></div>\n"
    ));
    let clippy_class = if clippy_status == "PASS" {
        "pass"
    } else {
        "fail"
    };
    h.push_str(&format!(
        "<div class=\"stat\"><div class=\"value {clippy_class}\">{clippy_status}</div>\
         <div class=\"label\">cargo clippy</div></div>\n"
    ));
    h.push_str("</div>\n");

    h.push_str("<h2>Full Output</h2>\n");
    h.push_str(&format!("<pre>{}</pre>\n", html::html_escape(&content)));
    h.push_str(&html::html_footer());

    let report_path = dir.join("report.html");
    std::fs::write(&report_path, &h).map_err(|e| format!("Write: {e}"))?;
    eprintln!("Generated {}", report_path.display());
    Ok(())
}

// ---- unit report ----

fn generate_unit_report(dir: &Path) -> Result<(), String> {
    let content =
        std::fs::read_to_string(dir.join("output.txt")).map_err(|e| format!("Read: {e}"))?;

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

    let mut modules: BTreeMap<String, Vec<&TestEntry>> = BTreeMap::new();
    for t in &tests {
        modules.entry(t.module.clone()).or_default().push(t);
    }

    let overall = if total_failed > 0 { "FAIL" } else { "PASS" };
    let overall_class = if total_failed > 0 { "fail" } else { "pass" };

    let mut h = String::with_capacity(32_000);
    h.push_str(&html::html_head("Unit Test Report"));
    h.push_str(&format!(
        "<h1>Unit Test Report</h1>\n\
         <p>Overall: <span class=\"{overall_class}\"><strong>{overall}</strong></span></p>\n"
    ));

    h.push_str("<div class=\"grid\">\n");
    h.push_str(&format!(
        "<div class=\"stat\"><div class=\"value\">{total_tests}</div>\
         <div class=\"label\">Total</div></div>\n"
    ));
    h.push_str(&format!(
        "<div class=\"stat\"><div class=\"value pass\">{total_passed}</div>\
         <div class=\"label\">Passed</div></div>\n"
    ));
    h.push_str(&format!(
        "<div class=\"stat\"><div class=\"value fail\">{total_failed}</div>\
         <div class=\"label\">Failed</div></div>\n"
    ));
    h.push_str(&format!(
        "<div class=\"stat\"><div class=\"value warn\">{total_ignored}</div>\
         <div class=\"label\">Ignored</div></div>\n"
    ));
    if !total_duration.is_empty() {
        h.push_str(&format!(
            "<div class=\"stat\"><div class=\"value\">{}</div>\
             <div class=\"label\">Duration</div></div>\n",
            html::html_escape(&total_duration)
        ));
    }
    h.push_str("</div>\n");

    if !modules.is_empty() {
        h.push_str("<h2>Tests by Module</h2>\n");
        for (module, entries) in &modules {
            let mod_passed = entries.iter().filter(|e| e.status == "ok").count();
            let mod_failed = entries.iter().filter(|e| e.status == "FAILED").count();
            let mod_ignored = entries.iter().filter(|e| e.status == "ignored").count();

            h.push_str(&format!(
                "<details{}>\n<summary><strong>{}</strong> &mdash; \
                 <span class=\"pass\">{mod_passed} passed</span>",
                if mod_failed > 0 { " open" } else { "" },
                html::html_escape(module)
            ));
            if mod_failed > 0 {
                h.push_str(&format!(
                    ", <span class=\"fail\">{mod_failed} failed</span>"
                ));
            }
            if mod_ignored > 0 {
                h.push_str(&format!(
                    ", <span class=\"warn\">{mod_ignored} ignored</span>"
                ));
            }
            h.push_str("</summary>\n<table>\n<tr><th>Test</th><th>Status</th></tr>\n");

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
                h.push_str(&format!(
                    "<tr><td>{}</td><td class=\"{cls}\">{label}</td></tr>\n",
                    html::html_escape(short_name)
                ));
            }

            h.push_str("</table>\n</details>\n");
        }
    }

    h.push_str(&html::html_footer());

    let report_path = dir.join("report.html");
    std::fs::write(&report_path, &h).map_err(|e| format!("Write: {e}"))?;
    eprintln!("Generated {}", report_path.display());
    Ok(())
}

fn extract_count(line: &str, label: &str) -> Option<u64> {
    let idx = line.find(label)?;
    let before = line[..idx].trim_end();
    let num_str = before.rsplit(|c: char| !c.is_ascii_digit()).next()?;
    num_str.parse().ok()
}

// ---- bench report ----

fn generate_bench_report(dir: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("Read: {e}"))?;

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

    let mut h = String::with_capacity(8_000);
    h.push_str(&html::html_head("Benchmark Report"));
    h.push_str(&format!(
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
        h.push_str(&format!(
            "<div class=\"card\">\n<h2>Profile: {} \
             <span class=\"{status}\">{} improvements, {} regressions</span></h2>\n\
             <p>Threshold: {:.1}%</p>\n",
            html::html_escape(&profile.name),
            profile.improvements.len(),
            profile.regressions.len(),
            profile.threshold
        ));

        if !profile.regressions.is_empty() {
            h.push_str(
                "<h2>Regressions</h2>\n<table>\n\
                 <tr><th>Benchmark</th><th>Change</th></tr>\n",
            );
            for r in &profile.regressions {
                let bench_name = r["name"].as_str().unwrap_or("unknown");
                let pct = r["change_pct"].as_f64().unwrap_or(0.0);
                h.push_str(&format!(
                    "<tr><td>{}</td><td class=\"fail\">{pct:+.2}%</td></tr>\n",
                    html::html_escape(bench_name)
                ));
            }
            h.push_str("</table>\n");
        }

        if !profile.improvements.is_empty() {
            h.push_str(
                "<h2>Improvements</h2>\n<table>\n\
                 <tr><th>Benchmark</th><th>Change</th></tr>\n",
            );
            for imp in &profile.improvements {
                let bench_name = imp["name"].as_str().unwrap_or("unknown");
                let pct = imp["change_pct"].as_f64().unwrap_or(0.0);
                h.push_str(&format!(
                    "<tr><td>{}</td><td class=\"pass\">{pct:+.2}%</td></tr>\n",
                    html::html_escape(bench_name)
                ));
            }
            h.push_str("</table>\n");
        }

        if let Some(ref txt) = profile.report_txt {
            h.push_str(&format!(
                "<details>\n<summary>Report output (report_{}.txt)</summary>\n\
                 <pre>{}</pre>\n</details>\n",
                html::html_escape(&profile.name),
                html::html_escape(txt)
            ));
        }

        if let Some(ref crit) = profile.criterion_output {
            h.push_str(&format!(
                "<details>\n<summary>Criterion output (criterion_{}.txt)</summary>\n\
                 <pre>{}</pre>\n</details>\n",
                html::html_escape(&profile.name),
                html::html_escape(crit)
            ));
        }

        h.push_str("</div>\n");
    }

    h.push_str(&html::html_footer());

    let report_path = dir.join("report.html");
    std::fs::write(&report_path, &h).map_err(|e| format!("Write: {e}"))?;
    eprintln!("Generated {}", report_path.display());
    Ok(())
}
