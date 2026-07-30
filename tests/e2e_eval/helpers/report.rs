use super::constants::Vertical;
use super::scoring::DocMatchScore;
use serde_json::Value;
use std::time::{Duration, Instant};

/// Result for a single query within a vertical.
pub struct QueryResult {
    pub query_id: String,
    pub query_text: String,
    /// Routing strategy this query exercises: "fts", "vector", or "cascade".
    pub strategy: String,
    pub recall: f64,
    pub expected_docs: usize,
    pub matched_docs: usize,
    pub response_time: Duration,
    pub success: bool,
    pub timed_out: bool,
    pub doc_scores: Vec<DocMatchScore>,
    /// Raw response text from the LLM invocation.
    pub response_text: String,
    /// First line of stderr when the invocation failed.
    pub error: Option<String>,
}

/// Aggregated result for one vertical.
pub struct VerticalResult {
    pub vertical: Vertical,
    pub total_recall: f64,
    /// Average confidence across all doc scores in successful queries.
    pub avg_confidence: f64,
    pub avg_response_time: Duration,
    pub successful_queries: usize,
    pub failed_queries: usize,
    pub timed_out_queries: usize,
    /// Response time percentile stats (seconds).
    pub time_p95: f64,
    pub time_p97: f64,
    pub time_p99: f64,
    pub time_best: f64,
    pub queries: Vec<QueryResult>,
}

impl VerticalResult {
    /// Compute aggregate stats from individual query results.
    pub fn compute(vertical: Vertical, queries: Vec<QueryResult>) -> Self {
        let successful = queries.iter().filter(|q| q.success).count();
        let failed = queries.iter().filter(|q| !q.success).count();
        let timed_out = queries.iter().filter(|q| q.timed_out).count();

        let successful_queries: Vec<&QueryResult> = queries.iter().filter(|q| q.success).collect();

        let total_recall = if successful_queries.is_empty() {
            0.0
        } else {
            successful_queries.iter().map(|q| q.recall).sum::<f64>()
                / successful_queries.len() as f64
        };

        // Average confidence across all doc scores from successful queries.
        let all_confidences: Vec<f64> = successful_queries
            .iter()
            .flat_map(|q| q.doc_scores.iter().map(|d| d.confidence))
            .collect();
        let avg_confidence = if all_confidences.is_empty() {
            0.0
        } else {
            all_confidences.iter().sum::<f64>() / all_confidences.len() as f64
        };

        let total_time: Duration = queries.iter().map(|q| q.response_time).sum();
        let avg_response_time = if queries.is_empty() {
            Duration::ZERO
        } else {
            total_time / queries.len() as u32
        };

        let mut times: Vec<f64> = queries
            .iter()
            .map(|q| q.response_time.as_secs_f64())
            .collect();
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let time_p95 = percentile(&times, 0.95);
        let time_p97 = percentile(&times, 0.97);
        let time_p99 = percentile(&times, 0.99);
        // "best" = fastest response time (minimum)
        let time_best = times.first().copied().unwrap_or(0.0);

        Self {
            vertical,
            total_recall,
            avg_confidence,
            avg_response_time,
            successful_queries: successful,
            failed_queries: failed,
            timed_out_queries: timed_out,
            time_p95,
            time_p97,
            time_p99,
            time_best,
            queries,
        }
    }
}

/// Compute the p-th percentile of a sorted slice using linear interpolation.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = p * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    let frac = rank - lo as f64;
    sorted[lo] * (1.0 - frac) + sorted[hi] * frac
}

/// Complete eval report across all verticals.
pub struct EvalReport {
    pub verticals: Vec<VerticalResult>,
    /// LLM model used for all invocations.
    pub model: String,
    start: Instant,
}

impl EvalReport {
    pub fn new(model: Option<&str>) -> Self {
        Self {
            verticals: Vec::new(),
            model: model.unwrap_or("default").to_string(),
            start: Instant::now(),
        }
    }

    pub fn add_vertical(&mut self, result: VerticalResult) {
        self.verticals.push(result);
    }

    /// Print a side-by-side comparison table to stderr.
    pub fn print_comparison_table(&self) {
        let elapsed = self.start.elapsed();

        eprintln!();
        eprintln!("\x1b[1mE2E Eval Comparison\x1b[0m");
        eprintln!("===========================================================================");

        if self.verticals.is_empty() {
            eprintln!("  No verticals were run.");
            return;
        }

        // Header
        eprint!("{:<20}", "Metric");
        for v in &self.verticals {
            eprint!("| {:<12}", v.vertical.short_name());
        }
        eprintln!();
        eprintln!("---------------------------------------------------------------------------");

        // Avg Confidence
        eprint!("{:<20}", "Avg Confidence");
        for v in &self.verticals {
            eprint!("| {:<12}", format!("{:.1}%", v.avg_confidence * 100.0));
        }
        eprintln!();

        // Pass Rate (recall >= 50%)
        eprint!("{:<20}", "Pass Rate");
        for v in &self.verticals {
            eprint!("| {:<12}", format!("{:.0}%", v.total_recall * 100.0));
        }
        eprintln!();

        // Avg Time
        eprint!("{:<20}", "Avg Time");
        for v in &self.verticals {
            eprint!(
                "| {:<12}",
                format!("{:.1}s", v.avg_response_time.as_secs_f64())
            );
        }
        eprintln!();

        // Time P95
        eprint!("{:<20}", "Time P95");
        for v in &self.verticals {
            eprint!("| {:<12}", format!("{:.1}s", v.time_p95));
        }
        eprintln!();

        // Time P97
        eprint!("{:<20}", "Time P97");
        for v in &self.verticals {
            eprint!("| {:<12}", format!("{:.1}s", v.time_p97));
        }
        eprintln!();

        // Time P99
        eprint!("{:<20}", "Time P99");
        for v in &self.verticals {
            eprint!("| {:<12}", format!("{:.1}s", v.time_p99));
        }
        eprintln!();

        // Best Time
        eprint!("{:<20}", "Best Time");
        for v in &self.verticals {
            eprint!("| {:<12}", format!("{:.1}s", v.time_best));
        }
        eprintln!();

        // Success Rate
        eprint!("{:<20}", "Success Rate");
        for v in &self.verticals {
            let total = v.successful_queries + v.failed_queries;
            eprint!("| {:<12}", format!("{}/{}", v.successful_queries, total));
        }
        eprintln!();

        // Timed Out
        eprint!("{:<20}", "Timed Out");
        for v in &self.verticals {
            eprint!("| {:<12}", v.timed_out_queries);
        }
        eprintln!();

        // Errors (non-timeout failures)
        eprint!("{:<20}", "Errors");
        for v in &self.verticals {
            let errors = v.failed_queries - v.timed_out_queries;
            eprint!("| {:<12}", errors);
        }
        eprintln!();

        // 12-Cell Coverage Matrix (Strategy × Vertical)
        eprintln!();
        eprintln!("\x1b[1m12-Cell Coverage Matrix (Strategy \u{00d7} Vertical)\x1b[0m");

        eprint!("{:<12}", "Strategy");
        for v in &self.verticals {
            eprint!("| {:<12}", v.vertical.short_name());
        }
        eprintln!();
        eprintln!("---------------------------------------------------------------------------");

        for strategy in &["fts", "vector", "cascade"] {
            eprint!("{:<12}", *strategy);
            for v in &self.verticals {
                let strat_queries: Vec<&QueryResult> = v
                    .queries
                    .iter()
                    .filter(|q| q.success && q.strategy == *strategy)
                    .collect();
                if strat_queries.is_empty() {
                    eprint!("| {:<12}", "-");
                } else {
                    let avg_recall = strat_queries.iter().map(|q| q.recall).sum::<f64>()
                        / strat_queries.len() as f64;
                    eprint!(
                        "| {:<12}",
                        format!(
                            "{:.0}% ({}/{})",
                            avg_recall * 100.0,
                            strat_queries.iter().filter(|q| q.recall >= 0.5).count(),
                            strat_queries.len(),
                        )
                    );
                }
            }
            eprintln!();
        }

        // Per-Query Confidence
        eprintln!();
        eprintln!("\x1b[1mPer-Query Confidence\x1b[0m");

        // Collect all unique query IDs
        let mut query_ids: Vec<String> = Vec::new();
        for v in &self.verticals {
            for q in &v.queries {
                if !query_ids.contains(&q.query_id) {
                    query_ids.push(q.query_id.clone());
                }
            }
        }

        for qid in &query_ids {
            // Get strategy from the first vertical that has this query
            let strategy = self
                .verticals
                .iter()
                .flat_map(|v| &v.queries)
                .find(|q| &q.query_id == qid)
                .map(|q| q.strategy.as_str())
                .unwrap_or("?");
            eprint!("{:<26}", format!("{} [{}]", qid, strategy));
            for v in &self.verticals {
                let cell = v
                    .queries
                    .iter()
                    .find(|q| &q.query_id == qid)
                    .map(|q| {
                        if q.timed_out {
                            "TIMEOUT".to_string()
                        } else if !q.success {
                            "ERR".to_string()
                        } else {
                            // Show avg confidence across doc scores for this query
                            let avg = if q.doc_scores.is_empty() {
                                0.0
                            } else {
                                q.doc_scores.iter().map(|d| d.confidence).sum::<f64>()
                                    / q.doc_scores.len() as f64
                            };
                            format!("{:.0}%", avg * 100.0)
                        }
                    })
                    .unwrap_or_else(|| "-".to_string());
                eprint!("| {:<12}", cell);
            }
            eprintln!();
        }

        eprintln!();

        // Best Confidence / Best Time / Worst
        let successful: Vec<_> = self
            .verticals
            .iter()
            .filter(|v| v.successful_queries > 0)
            .collect();
        if let Some(best_conf) = successful
            .iter()
            .max_by(|a, b| a.avg_confidence.partial_cmp(&b.avg_confidence).unwrap())
        {
            eprintln!(
                "\x1b[32mBest confidence: {} ({:.1}%, {:.1}s avg)\x1b[0m",
                best_conf.vertical.short_name(),
                best_conf.avg_confidence * 100.0,
                best_conf.avg_response_time.as_secs_f64()
            );
        }
        if let Some(best_time) = successful.iter().min_by_key(|v| v.avg_response_time) {
            eprintln!(
                "\x1b[34mBest time:       {} ({:.1}s avg, {:.1}% confidence)\x1b[0m",
                best_time.vertical.short_name(),
                best_time.avg_response_time.as_secs_f64(),
                best_time.avg_confidence * 100.0,
            );
        }
        if let Some(worst) = self.verticals.iter().min_by(|a, b| {
            a.avg_confidence
                .partial_cmp(&b.avg_confidence)
                .unwrap()
                .then(b.avg_response_time.cmp(&a.avg_response_time))
        }) {
            eprintln!(
                "\x1b[31mWorst:           {} ({:.1}% confidence, {:.1}s avg)\x1b[0m",
                worst.vertical.short_name(),
                worst.avg_confidence * 100.0,
                worst.avg_response_time.as_secs_f64()
            );
        }

        eprintln!();
        eprintln!("Total duration: {:.1}s", elapsed.as_secs_f64());
        eprintln!("===========================================================================");
    }

    /// Write results as JSON.
    pub fn write_json(&self, path: &std::path::Path) {
        let elapsed = self.start.elapsed();

        let verticals: Vec<Value> = self
            .verticals
            .iter()
            .map(|v| {
                let queries: Vec<Value> = v
                    .queries
                    .iter()
                    .map(|q| {
                        let doc_scores: Vec<Value> = q
                            .doc_scores
                            .iter()
                            .map(|d| {
                                serde_json::json!({
                                    "doc_id": d.doc_id,
                                    "matched": d.matched,
                                    "confidence": format!("{:.3}", d.confidence),
                                    "matched_terms": d.matched_terms,
                                })
                            })
                            .collect();

                        let mut query_json = serde_json::json!({
                            "query_id": q.query_id,
                            "query_text": q.query_text,
                            "strategy": q.strategy,
                            "recall": format!("{:.3}", q.recall),
                            "expected_docs": q.expected_docs,
                            "matched_docs": q.matched_docs,
                            "response_time_ms": q.response_time.as_millis(),
                            "success": q.success,
                            "timed_out": q.timed_out,
                            "doc_scores": doc_scores,
                            "response_text": q.response_text,
                        });
                        if let Some(err) = &q.error {
                            query_json["error"] = serde_json::Value::String(err.clone());
                        }
                        query_json
                    })
                    .collect();

                serde_json::json!({
                    "name": v.vertical.name(),
                    "avg_confidence": format!("{:.3}", v.avg_confidence),
                    "total_recall": format!("{:.3}", v.total_recall),
                    "time_p95_secs": format!("{:.2}", v.time_p95),
                    "time_p97_secs": format!("{:.2}", v.time_p97),
                    "time_p99_secs": format!("{:.2}", v.time_p99),
                    "time_best_secs": format!("{:.2}", v.time_best),
                    "avg_response_time_ms": v.avg_response_time.as_millis(),
                    "successful_queries": v.successful_queries,
                    "failed_queries": v.failed_queries,
                    "timed_out_queries": v.timed_out_queries,
                    "queries": queries,
                })
            })
            .collect();

        let json_successful: Vec<_> = self
            .verticals
            .iter()
            .filter(|v| v.successful_queries > 0)
            .collect();
        let best_conf_v = json_successful
            .iter()
            .max_by(|a, b| a.avg_confidence.partial_cmp(&b.avg_confidence).unwrap());
        let best_time_v = json_successful.iter().min_by_key(|v| v.avg_response_time);
        let worst_v = self.verticals.iter().min_by(|a, b| {
            a.avg_confidence
                .partial_cmp(&b.avg_confidence)
                .unwrap()
                .then(b.avg_response_time.cmp(&a.avg_response_time))
        });

        // Build strategy_matrix: { strategy: { vertical: { recall, count } } }
        let mut strategy_matrix = serde_json::Map::new();
        for strategy in &["fts", "vector", "cascade"] {
            let mut per_vertical = serde_json::Map::new();
            for v in &self.verticals {
                let strat_queries: Vec<&QueryResult> = v
                    .queries
                    .iter()
                    .filter(|q| q.success && q.strategy == *strategy)
                    .collect();
                let total: Vec<&QueryResult> = v
                    .queries
                    .iter()
                    .filter(|q| q.strategy == *strategy)
                    .collect();
                let avg_recall = if strat_queries.is_empty() {
                    0.0
                } else {
                    strat_queries.iter().map(|q| q.recall).sum::<f64>() / strat_queries.len() as f64
                };
                let key = v.vertical.short_name().to_lowercase().replace(' ', "_");
                per_vertical.insert(
                    key,
                    serde_json::json!({
                        "recall": format!("{:.3}", avg_recall),
                        "count": total.len(),
                        "pass": strat_queries.iter().filter(|q| q.recall >= 0.5).count(),
                    }),
                );
            }
            strategy_matrix.insert(strategy.to_string(), Value::Object(per_vertical));
        }

        let report = serde_json::json!({
            "type": "eval",
            "model": self.model,
            "timestamp": chrono_now(),
            "duration_secs": elapsed.as_secs(),
            "verticals": verticals,
            "strategy_matrix": strategy_matrix,
            "comparison": {
                "best_confidence_vertical": best_conf_v.map(|v| v.vertical.name()).unwrap_or("none"),
                "best_confidence": best_conf_v.map(|v| format!("{:.3}", v.avg_confidence)).unwrap_or_else(|| "0.000".into()),
                "best_time_vertical": best_time_v.map(|v| v.vertical.name()).unwrap_or("none"),
                "best_time_avg_ms": best_time_v.map(|v| v.avg_response_time.as_millis()).unwrap_or(0),
                "worst_vertical": worst_v.map(|v| v.vertical.name()).unwrap_or("none"),
            }
        });

        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(path, serde_json::to_string_pretty(&report).unwrap()) {
            eprintln!("Failed to write eval results JSON: {e}");
        } else {
            eprintln!("Eval results written to {}", path.display());
        }
    }

    /// Write results as a self-contained HTML report.
    /// If `documents` is provided, expected doc content is shown alongside agent responses.
    pub fn write_html(
        &self,
        path: &std::path::Path,
        documents: &[super::data::Document],
        resource_data: Option<&serde_json::Value>,
        profile_data: Option<&serde_json::Value>,
    ) {
        use std::fmt::Write as _;

        let mut html = String::with_capacity(16_000);

        // --- Head ---
        html.push_str(
            r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Conpack E2E Eval Results</title>
<style>
  :root { --bg: #0d1117; --surface: #161b22; --border: #30363d; --fg: #e6edf3;
          --fg2: #8b949e; --green: #3fb950; --red: #f85149; --yellow: #d29922;
          --blue: #58a6ff; --purple: #bc8cff; }
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, monospace;
         background: var(--bg); color: var(--fg); padding: 2rem; line-height: 1.5; }
  h1 { font-size: 1.6rem; margin-bottom: .3rem; }
  .subtitle { color: var(--fg2); font-size: .85rem; margin-bottom: 1.5rem; }
  h2 { font-size: 1.15rem; margin: 1.8rem 0 .8rem; color: var(--blue); }
  table { width: 100%; border-collapse: collapse; margin-bottom: 1.2rem; }
  th, td { padding: .55rem .75rem; text-align: left; border: 1px solid var(--border); }
  th { background: var(--surface); font-weight: 600; font-size: .85rem; color: var(--fg2); }
  td { font-size: .85rem; }
  tr:hover td { background: #1c2333; }
  .best { background: #0d2818; }
  .worst { background: #2d1114; }
  .pass { color: var(--green); font-weight: 600; }
  .fail { color: var(--red); font-weight: 600; }
  .timeout { color: var(--yellow); font-weight: 600; }
  .conf-high { color: var(--green); }
  .conf-mid { color: var(--yellow); }
  .conf-low { color: var(--red); }
  .badge { display: inline-block; padding: .15rem .5rem; border-radius: 3px;
           font-size: .75rem; font-weight: 600; }
  .badge-best { background: #0d2818; color: var(--green); border: 1px solid var(--green); }
  .badge-fast { background: #0d1b2a; color: var(--blue); border: 1px solid var(--blue); }
  .badge-worst { background: #2d1114; color: var(--red); border: 1px solid var(--red); }
  .bar-cell { position: relative; }
  .bar { position: absolute; top: 0; left: 0; bottom: 0; opacity: .15; border-radius: 2px; }
  .bar-green { background: var(--green); }
  .bar-red { background: var(--red); }
  .bar-yellow { background: var(--yellow); }
  .bar-label { position: relative; z-index: 1; }
  details { margin: .4rem 0; }
  summary { cursor: pointer; font-size: .8rem; color: var(--fg2); padding: .2rem 0; }
  summary:hover { color: var(--fg); }
  .tags { display: flex; flex-wrap: wrap; gap: .3rem; margin-top: .3rem; }
  .tag { font-size: .7rem; padding: .1rem .4rem; border-radius: 3px;
         background: var(--surface); border: 1px solid var(--border); color: var(--fg2); }
  .tag-phrase { border-color: var(--purple); color: var(--purple); }
  .tag-title { border-color: var(--blue); color: var(--blue); }
  .tag-tag { border-color: var(--green); color: var(--green); }
  .query-text { color: var(--fg2); font-size: .8rem; font-style: italic; display: block;
                margin-top: .2rem; max-width: 60ch; }
  .card { background: var(--surface); border: 1px solid var(--border); border-radius: 6px;
          padding: 1rem 1.2rem; margin-bottom: 1rem; }
  .grid-2 { display: grid; grid-template-columns: 1fr 1fr; gap: 1rem; }
  @media (max-width: 800px) { .grid-2 { grid-template-columns: 1fr; } }
  .metric-val { font-size: 1.8rem; font-weight: 700; }
  .metric-label { font-size: .75rem; color: var(--fg2); text-transform: uppercase; letter-spacing: .05em; }
  .response-section { margin-top: .6rem; }
  .response-label { font-size: .75rem; font-weight: 600; color: var(--fg2); text-transform: uppercase;
                     letter-spacing: .05em; margin-bottom: .3rem; }
  .response-box { background: var(--surface); border: 1px solid var(--border); border-radius: 4px;
                  padding: .6rem .8rem; font-size: .8rem; white-space: pre-wrap; word-break: break-word;
                  max-height: 20rem; overflow-y: auto; line-height: 1.5; }
  .response-box.expected { border-left: 3px solid var(--green); }
  .response-box.agent { border-left: 3px solid var(--blue); }
  .response-grid { display: grid; grid-template-columns: 1fr 1fr; gap: .8rem; margin-top: .5rem; }
  @media (max-width: 900px) { .response-grid { grid-template-columns: 1fr; } }
  .no-response { color: var(--fg2); font-style: italic; font-size: .8rem; }
</style>
</head>
<body>
<h1>Conpack E2E Eval Results</h1>
"#,
        );

        // Timestamp
        let timestamp = chrono_now();
        let _ = writeln!(html, "<p class=\"subtitle\">Generated: {timestamp}</p>");

        // Find best confidence and fastest time (independent awards)
        let best_conf_name = self
            .verticals
            .iter()
            .filter(|v| v.successful_queries > 0)
            .max_by(|a, b| a.avg_confidence.partial_cmp(&b.avg_confidence).unwrap())
            .map(|v| v.vertical.name());
        let best_time_name = self
            .verticals
            .iter()
            .filter(|v| v.successful_queries > 0)
            .min_by(|a, b| a.avg_response_time.cmp(&b.avg_response_time))
            .map(|v| v.vertical.name());
        let worst_name = self
            .verticals
            .iter()
            .min_by(|a, b| {
                a.avg_confidence
                    .partial_cmp(&b.avg_confidence)
                    .unwrap()
                    .then(b.avg_response_time.cmp(&a.avg_response_time))
            })
            .map(|v| v.vertical.name());

        // --- Summary cards ---
        html.push_str("<h2>Summary</h2>\n<div class=\"grid-2\">\n");
        for v in &self.verticals {
            let name = v.vertical.name();
            let is_best_conf = best_conf_name == Some(name);
            let is_best_time = best_time_name == Some(name);
            let is_worst = worst_name == Some(name) && !is_best_conf && !is_best_time;
            let conf_pct = v.avg_confidence * 100.0;
            let conf_class = if conf_pct >= 60.0 {
                "conf-high"
            } else if conf_pct >= 30.0 {
                "conf-mid"
            } else {
                "conf-low"
            };

            let mut badges = String::new();
            if is_best_conf {
                badges.push_str(" <span class=\"badge badge-best\">BEST CONFIDENCE</span>");
            }
            if is_best_time {
                badges.push_str(" <span class=\"badge badge-fast\">BEST TIME</span>");
            }
            if is_worst {
                badges.push_str(" <span class=\"badge badge-worst\">WORST</span>");
            }

            let avg_time = v.avg_response_time.as_secs_f64();
            let time_class = if avg_time <= 15.0 {
                "conf-high"
            } else if avg_time <= 30.0 {
                "conf-mid"
            } else {
                "conf-low"
            };

            let _ = writeln!(
                html,
                "<div class=\"card\">\
                 <div class=\"metric-label\">{}{badges}</div>\
                 <div class=\"metric-val {conf_class}\">{conf_pct:.1}%</div>\
                 <div class=\"metric-label\">confidence &nbsp;|&nbsp; pass rate: {:.0}%</div>\
                 <div class=\"metric-val {time_class}\">{avg_time:.1}s</div>\
                 <div class=\"metric-label\">avg time &nbsp;|&nbsp; p95: {:.1}s &nbsp;|&nbsp; p99: {:.1}s &nbsp;|&nbsp; best: {:.1}s</div>\
                 </div>",
                v.vertical.short_name(),
                v.total_recall * 100.0,
                v.time_p95,
                v.time_p99,
                v.time_best,
            );
        }
        html.push_str("</div>\n");

        // --- Comparison table ---
        html.push_str("<h2>Comparison</h2>\n<table>\n<tr><th>Metric</th>");
        for v in &self.verticals {
            let _ = write!(html, "<th>{}</th>", v.vertical.short_name());
        }
        html.push_str("</tr>\n");

        // Avg Confidence row
        html.push_str("<tr><td>Avg Confidence</td>");
        for v in &self.verticals {
            let pct = v.avg_confidence * 100.0;
            let extra = if best_conf_name == Some(v.vertical.name()) {
                " best"
            } else if worst_name == Some(v.vertical.name()) {
                " worst"
            } else {
                ""
            };
            let _ = write!(
                html,
                "<td class=\"bar-cell{extra}\"><span class=\"bar bar-green\" style=\"width:{pct:.0}%\"></span><span class=\"bar-label\">{pct:.1}%</span></td>"
            );
        }
        html.push_str("</tr>\n");

        // Pass Rate row
        html.push_str("<tr><td>Pass Rate</td>");
        for v in &self.verticals {
            let pct = v.total_recall * 100.0;
            let cls = if pct >= 80.0 {
                "pass"
            } else if pct >= 50.0 {
                "conf-mid"
            } else {
                "fail"
            };
            let _ = write!(html, "<td class=\"{cls}\">{pct:.0}%</td>");
        }
        html.push_str("</tr>\n");

        // Avg response time
        html.push_str("<tr><td>Avg Time</td>");
        for v in &self.verticals {
            let _ = write!(html, "<td>{:.1}s</td>", v.avg_response_time.as_secs_f64());
        }
        html.push_str("</tr>\n");

        // Time P95
        html.push_str("<tr><td>Time P95</td>");
        for v in &self.verticals {
            let _ = write!(html, "<td>{:.1}s</td>", v.time_p95);
        }
        html.push_str("</tr>\n");

        // Time P97
        html.push_str("<tr><td>Time P97</td>");
        for v in &self.verticals {
            let _ = write!(html, "<td>{:.1}s</td>", v.time_p97);
        }
        html.push_str("</tr>\n");

        // Time P99
        html.push_str("<tr><td>Time P99</td>");
        for v in &self.verticals {
            let _ = write!(html, "<td>{:.1}s</td>", v.time_p99);
        }
        html.push_str("</tr>\n");

        // Best Time
        html.push_str("<tr><td>Best Time</td>");
        for v in &self.verticals {
            let _ = write!(html, "<td>{:.1}s</td>", v.time_best);
        }
        html.push_str("</tr>\n");

        // Success
        html.push_str("<tr><td>Success Rate</td>");
        for v in &self.verticals {
            let total = v.successful_queries + v.failed_queries;
            let _ = write!(html, "<td>{}/{total}</td>", v.successful_queries);
        }
        html.push_str("</tr>\n");

        // Timeouts
        html.push_str("<tr><td>Timeouts</td>");
        for v in &self.verticals {
            let cls = if v.timed_out_queries > 0 {
                "timeout"
            } else {
                ""
            };
            let _ = write!(html, "<td class=\"{cls}\">{}</td>", v.timed_out_queries);
        }
        html.push_str("</tr>\n");

        // Errors
        html.push_str("<tr><td>Errors</td>");
        for v in &self.verticals {
            let errors = v.failed_queries - v.timed_out_queries;
            let cls = if errors > 0 { "fail" } else { "" };
            let _ = write!(html, "<td class=\"{cls}\">{errors}</td>");
        }
        html.push_str("</tr>\n</table>\n");

        // --- 12-Cell Coverage Matrix ---
        html.push_str(
            "<h2>Coverage Matrix (Strategy \u{00d7} Vertical)</h2>\n<table>\n<tr><th>Strategy</th>",
        );
        for v in &self.verticals {
            let _ = write!(html, "<th>{}</th>", v.vertical.short_name());
        }
        html.push_str("</tr>\n");

        for strategy in &["fts", "vector", "cascade"] {
            let _ = write!(html, "<tr><td><strong>{strategy}</strong></td>");
            for v in &self.verticals {
                let strat_queries: Vec<&QueryResult> = v
                    .queries
                    .iter()
                    .filter(|q| q.success && q.strategy == *strategy)
                    .collect();
                let all_strat: Vec<&QueryResult> = v
                    .queries
                    .iter()
                    .filter(|q| q.strategy == *strategy)
                    .collect();
                if all_strat.is_empty() {
                    html.push_str("<td>-</td>");
                } else {
                    let avg_recall = if strat_queries.is_empty() {
                        0.0
                    } else {
                        strat_queries.iter().map(|q| q.recall).sum::<f64>()
                            / strat_queries.len() as f64
                    };
                    let pct = avg_recall * 100.0;
                    let pass_count = strat_queries.iter().filter(|q| q.recall >= 0.5).count();
                    let bar_cls = if pct >= 80.0 {
                        "bar-green"
                    } else if pct >= 50.0 {
                        "bar-yellow"
                    } else {
                        "bar-red"
                    };
                    let text_cls = if pct >= 80.0 {
                        "pass"
                    } else if pct >= 50.0 {
                        "conf-mid"
                    } else {
                        "fail"
                    };
                    let _ = write!(
                        html,
                        "<td class=\"bar-cell {text_cls}\"><span class=\"bar {bar_cls}\" style=\"width:{pct:.0}%\"></span>\
                         <span class=\"bar-label\">{pct:.0}% ({pass_count}/{})</span></td>",
                        all_strat.len(),
                    );
                }
            }
            html.push_str("</tr>\n");
        }
        html.push_str("</table>\n");

        // --- Per-strategy recall ---
        html.push_str("<h2>Per-Strategy Recall</h2>\n<table>\n<tr><th>Strategy</th>");
        for v in &self.verticals {
            let _ = write!(html, "<th>{}</th>", v.vertical.short_name());
        }
        html.push_str("</tr>\n");

        for strategy in &["fts", "vector", "cascade"] {
            let _ = write!(html, "<tr><td><strong>{strategy}</strong></td>");
            for v in &self.verticals {
                let strat_queries: Vec<&QueryResult> = v
                    .queries
                    .iter()
                    .filter(|q| q.success && q.strategy == *strategy)
                    .collect();
                if strat_queries.is_empty() {
                    html.push_str("<td>-</td>");
                } else {
                    let avg_recall = strat_queries.iter().map(|q| q.recall).sum::<f64>()
                        / strat_queries.len() as f64;
                    let pct = avg_recall * 100.0;
                    let cls = if pct >= 80.0 {
                        "pass"
                    } else if pct >= 50.0 {
                        "conf-mid"
                    } else {
                        "fail"
                    };
                    let good = strat_queries.iter().filter(|q| q.recall >= 0.5).count();
                    let _ = write!(
                        html,
                        "<td class=\"{cls}\">{pct:.0}% ({good}/{})</td>",
                        strat_queries.len()
                    );
                }
            }
            html.push_str("</tr>\n");
        }
        html.push_str("</table>\n");

        // --- Per-query confidence ---
        html.push_str(
            "<h2>Per-Query Confidence</h2>\n<table>\n<tr><th>Query</th><th>Strategy</th>",
        );
        for v in &self.verticals {
            let _ = write!(html, "<th>{}</th>", v.vertical.short_name());
        }
        html.push_str("</tr>\n");

        // Collect unique query IDs
        let mut query_ids: Vec<String> = Vec::new();
        for v in &self.verticals {
            for q in &v.queries {
                if !query_ids.contains(&q.query_id) {
                    query_ids.push(q.query_id.clone());
                }
            }
        }

        for qid in &query_ids {
            // Get query text and strategy from the first vertical that has it
            let first_match = self
                .verticals
                .iter()
                .flat_map(|v| &v.queries)
                .find(|q| &q.query_id == qid);
            let query_text = first_match.map(|q| q.query_text.as_str()).unwrap_or("");
            let strategy = first_match.map(|q| q.strategy.as_str()).unwrap_or("");

            let strat_cls = match strategy {
                "fts" => "conf-high",
                "vector" => "",
                "cascade" => "",
                _ => "",
            };
            let strat_style = match strategy {
                "vector" => " style=\"color: var(--purple)\"",
                "cascade" => " style=\"color: var(--blue)\"",
                _ => "",
            };

            let _ = write!(
                html,
                "<tr><td><strong>{qid}</strong><span class=\"query-text\">{}</span></td>\
                 <td class=\"{strat_cls}\"{strat_style}>{strategy}</td>",
                html_escape(query_text),
            );

            for v in &self.verticals {
                if let Some(q) = v.queries.iter().find(|q| &q.query_id == qid) {
                    let (label, cls) = if q.timed_out {
                        ("TIMEOUT".to_string(), "timeout")
                    } else if !q.success {
                        ("ERR".to_string(), "fail")
                    } else {
                        let avg_conf = if q.doc_scores.is_empty() {
                            0.0
                        } else {
                            q.doc_scores.iter().map(|d| d.confidence).sum::<f64>()
                                / q.doc_scores.len() as f64
                        };
                        let pct = avg_conf * 100.0;
                        let c = if pct >= 60.0 {
                            "pass"
                        } else if pct >= 30.0 {
                            "conf-mid"
                        } else {
                            "fail"
                        };
                        (format!("{pct:.0}%"), c)
                    };
                    let _ = write!(html, "<td class=\"{cls}\">{label}</td>");
                } else {
                    html.push_str("<td>-</td>");
                }
            }
            html.push_str("</tr>\n");
        }
        html.push_str("</table>\n");

        // --- Per-query detail (scoring + responses) ---
        // Build doc lookup
        let doc_map: std::collections::HashMap<&str, &super::data::Document> =
            documents.iter().map(|d| (d.id.as_str(), d)).collect();

        html.push_str("<h2>Query Details</h2>\n");

        for qid in &query_ids {
            // Find the query result and its vertical name for the summary line.
            let mut query_text = "";
            let mut vertical_label = "";
            for v in &self.verticals {
                if let Some(q) = v.queries.iter().find(|q| &q.query_id == qid) {
                    query_text = &q.query_text;
                    vertical_label = v.vertical.short_name();
                    break;
                }
            }

            let _ = writeln!(
                html,
                "<details><summary><strong>{qid}</strong> \
                 <span class=\"tag\" style=\"margin-left:.4rem\">{vertical_label}</span> — {}</summary>",
                html_escape(query_text),
            );

            // Expected document content
            let expected_doc_ids: Vec<&str> = self
                .verticals
                .iter()
                .flat_map(|v| &v.queries)
                .find(|q| &q.query_id == qid)
                .map(|q| q.doc_scores.iter().map(|d| d.doc_id.as_str()).collect())
                .unwrap_or_default();

            for doc_id in &expected_doc_ids {
                if let Some(doc) = doc_map.get(doc_id) {
                    let _ = writeln!(
                        html,
                        "<div class=\"response-section\">\
                         <div class=\"response-label\">Expected: {} — {}</div>\
                         <div class=\"response-box expected\">{}</div></div>",
                        html_escape(doc_id),
                        html_escape(&doc.title),
                        html_escape(&doc.content),
                    );
                }
            }

            // Scoring table
            html.push_str("<table>\n<tr><th>Vertical</th><th>Doc</th><th>Match</th><th>Confidence</th><th>Time</th><th>Matched Terms</th></tr>\n");

            for v in &self.verticals {
                if let Some(q) = v.queries.iter().find(|q| &q.query_id == qid) {
                    for d in &q.doc_scores {
                        let match_cls = if d.matched { "pass" } else { "fail" };
                        let match_label = if d.matched { "YES" } else { "NO" };
                        let conf_cls = if d.confidence >= 0.5 {
                            "conf-high"
                        } else if d.confidence >= 0.3 {
                            "conf-mid"
                        } else {
                            "conf-low"
                        };

                        let _ = write!(
                            html,
                            "<tr><td>{}</td><td>{}</td><td class=\"{match_cls}\">{match_label}</td>\
                             <td class=\"{conf_cls}\">{:.3}</td><td>{:.1}s</td><td><div class=\"tags\">",
                            v.vertical.short_name(),
                            d.doc_id,
                            d.confidence,
                            q.response_time.as_secs_f64(),
                        );

                        for term in &d.matched_terms {
                            let tag_cls = if term.starts_with("phrase:") {
                                "tag tag-phrase"
                            } else if term.starts_with("title") {
                                "tag tag-title"
                            } else if term.starts_with("tag:") {
                                "tag tag-tag"
                            } else {
                                "tag"
                            };
                            let _ = write!(
                                html,
                                "<span class=\"{tag_cls}\">{}</span>",
                                html_escape(term)
                            );
                        }

                        html.push_str("</div></td></tr>\n");
                    }
                }
            }
            html.push_str("</table>\n");

            // Agent responses per vertical
            html.push_str("<div class=\"response-section\"><div class=\"response-label\">Agent Responses</div></div>\n");
            for v in &self.verticals {
                if let Some(q) = v.queries.iter().find(|q| &q.query_id == qid) {
                    let _ = writeln!(
                        html,
                        "<details open><summary>{} — {}</summary>",
                        v.vertical.short_name(),
                        if q.timed_out {
                            "TIMEOUT".to_string()
                        } else if !q.success {
                            "FAILED".to_string()
                        } else {
                            format!("recall {:.0}%", q.recall * 100.0)
                        },
                    );
                    if q.response_text.is_empty() {
                        html.push_str("<p class=\"no-response\">No response captured</p>\n");
                    } else {
                        let _ = writeln!(
                            html,
                            "<div class=\"response-box agent\">{}</div>",
                            html_escape(&q.response_text),
                        );
                    }
                    html.push_str("</details>\n");
                }
            }

            html.push_str("</details>\n");
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
                    "<tr><td>Peak Threads</td><td>{}</td></tr>\n",
                    summary["peak_threads"].as_u64().unwrap_or(0)
                ));
                html.push_str(&format!(
                    "<tr><td>Peak FDs</td><td>{}</td></tr>\n",
                    summary["peak_fds"].as_u64().unwrap_or(0)
                ));
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

                    // Inline sparkline SVG (same logic as test_infra::html::render_sparkline_svg)
                    let render_svg = |points: &[(f64, f64)], label: &str, color: &str| -> String {
                        if points.is_empty() {
                            return String::new();
                        }
                        let (w, h) = (400u32, 100u32);
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
                        let m = 4.0;
                        let pw = w as f64 - 2.0 * m;
                        let ph = h as f64 - 2.0 * m - 14.0;
                        let mut path = String::new();
                        for (i, (x, y)) in points.iter().enumerate() {
                            let px = m + (x - x_min) / x_range * pw;
                            let py = m + ph - (y - y_min) / y_range * ph;
                            if i == 0 {
                                path.push_str(&format!("M{px:.1},{py:.1}"));
                            } else {
                                path.push_str(&format!(" L{px:.1},{py:.1}"));
                            }
                        }
                        format!(
                            r##"<svg width="{w}" height="{h}" viewBox="0 0 {w} {h}" xmlns="http://www.w3.org/2000/svg">
  <path d="{path}" fill="none" stroke="{color}" stroke-width="1.5" stroke-linejoin="round"/>
  <text x="{m}" y="{}" font-size="10" fill="#8b949e" font-family="monospace">{label}</text>
</svg>"##,
                            h as f64 - 2.0
                        )
                    };

                    html.push_str("<div style=\"display:flex;gap:1rem;flex-wrap:wrap;\">\n");
                    html.push_str("<div>");
                    html.push_str(&render_svg(&rss_points, "RSS (MB)", "#58a6ff"));
                    html.push_str("</div>\n<div>");
                    html.push_str(&render_svg(&thread_points, "Threads", "#3fb950"));
                    html.push_str("</div>\n</div>\n");
                }
            }
        }

        // Full profiler data (CPU top functions, DHAT, flamegraph links)
        if let Some(pd) = profile_data {
            render_profiler_sections(&mut html, pd);
        }

        html.push_str("\n</body>\n</html>\n");

        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(path, &html) {
            eprintln!("Failed to write HTML report: {e}");
        } else {
            eprintln!("HTML report written to {}", path.display());
        }
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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

fn chrono_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Manual UTC date/time formatting (no chrono dependency needed).
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hh = time_of_day / 3600;
    let mm = (time_of_day % 3600) / 60;
    let ss = time_of_day % 60;

    // Days since 1970-01-01 → (year, month, day) using a civil calendar algorithm.
    // Reference: http://howardhinnant.github.io/date_algorithms.html#civil_from_days
    let z = days as i64 + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02} UTC")
}
