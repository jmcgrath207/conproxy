use crate::helpers::agent_file::prepare_vertical_dir;
use crate::helpers::claude::ClaudeRunner;
use crate::helpers::constants::{EvalConfig, EvalProvider, Vertical};
use crate::helpers::data::{DocIndex, EvalQuery};
use crate::helpers::mcp_client::McpClient;
use crate::helpers::ollama::OllamaRunner;
use crate::helpers::openai_compat::OpenAiCompatRunner;
use crate::helpers::report::{QueryResult, VerticalResult};
use crate::helpers::scoring::score_response;
use crate::helpers::tool_defs::conproxy_tools;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

/// Run all queries for a given vertical, returning aggregated results.
pub fn run_vertical(
    vertical: Vertical,
    queries: &[EvalQuery],
    doc_index: &DocIndex,
    config: &EvalConfig,
) -> VerticalResult {
    let vertical_dir = prepare_vertical_dir(
        vertical,
        &config.base_dir,
        &config.conproxy_bin,
        &config.proxy_listen,
        config.provider,
    );

    match config.provider {
        EvalProvider::Ollama => {
            run_vertical_ollama(vertical, queries, doc_index, config, &vertical_dir)
        }
        EvalProvider::Claude => {
            run_vertical_claude(vertical, queries, doc_index, config, &vertical_dir)
        }
        EvalProvider::LlamaCpp => {
            run_vertical_llamacpp(vertical, queries, doc_index, config, &vertical_dir)
        }
    }
}

/// Run vertical using Ollama provider.
fn run_vertical_ollama(
    vertical: Vertical,
    queries: &[EvalQuery],
    doc_index: &DocIndex,
    config: &EvalConfig,
    vertical_dir: &Path,
) -> VerticalResult {
    let runner = OllamaRunner::new(
        &config.ollama_base_url,
        &config.ollama_model,
        config.timeout,
    );

    // For McpTools, start MCP client once for the vertical
    let mcp_client = if vertical == Vertical::McpTools {
        match McpClient::start(&config.conproxy_bin, vertical_dir) {
            Ok(client) => Some(std::sync::Mutex::new(client)),
            Err(e) => {
                eprintln!(
                    "  [{}] Failed to start MCP client: {e}",
                    vertical.short_name()
                );
                None
            }
        }
    } else {
        None
    };

    let query_results: Vec<QueryResult> = queries
        .iter()
        .enumerate()
        .map(|(i, query)| {
            let total = queries.len();
            eprintln!(
                "  [{}] [{}/{}] {} ...",
                vertical.short_name(),
                i + 1,
                total,
                &query.id
            );

            let start = Instant::now();

            let (response_text, success, error) = match vertical {
                Vertical::NoContext => {
                    let prompt = format!(
                        "Answer the following question with specific technical details, names, and concepts.\n\n\
                         Question: {}",
                        query.query
                    );
                    match runner.simple_chat(&prompt, "Answer with specific technical details.") {
                        Ok(text) => (text, true, None),
                        Err(e) => (String::new(), false, Some(e)),
                    }
                }
                Vertical::McpTools => {
                    if let Some(ref mcp_mutex) = mcp_client {
                        let tools = conproxy_tools();
                        let prompt = format!(
                            "You have access to conproxy MCP tools: search.\n\
                             Use search to find relevant documents and answer with specific technical details.\n\n\
                             Question: {}",
                            query.query
                        );
                        let mut mcp = mcp_mutex.lock().unwrap();
                        let result = runner.chat_with_tools(
                            &prompt,
                            "Use the available tools to search for relevant documents and answer with specific technical details.",
                            &tools,
                            &mut mcp,
                            5,
                        );
                        (result.response, result.completed, None)
                    } else {
                        (String::new(), false, Some("MCP client not available".to_string()))
                    }
                }
            };

            let duration = start.elapsed();

            let recall_score = if success {
                score_response(&response_text, query, doc_index)
            } else {
                crate::helpers::scoring::QueryRecallScore {
                    query_id: query.id.clone(),
                    expected_doc_count: query.expected_doc_ids.len(),
                    matched_doc_count: 0,
                    recall: 0.0,
                    doc_scores: query
                        .expected_doc_ids
                        .iter()
                        .map(|id| crate::helpers::scoring::DocMatchScore {
                            doc_id: id.clone(),
                            matched: false,
                            confidence: 0.0,
                            matched_terms: vec![],
                        })
                        .collect(),
                }
            };

            print_query_status(vertical, &recall_score, duration.as_secs_f64(), success, false);

            QueryResult {
                query_id: query.id.clone(),
                query_text: query.query.clone(),
                strategy: query.strategy.clone(),
                recall: recall_score.recall,
                expected_docs: recall_score.expected_doc_count,
                matched_docs: recall_score.matched_doc_count,
                response_time: duration,
                success,
                timed_out: false,
                doc_scores: recall_score.doc_scores,
                response_text,
                error,
            }
        })
        .collect();

    VerticalResult::compute(vertical, query_results)
}

/// Run vertical using Claude CLI provider.
fn run_vertical_claude(
    vertical: Vertical,
    queries: &[EvalQuery],
    doc_index: &DocIndex,
    config: &EvalConfig,
    vertical_dir: &Path,
) -> VerticalResult {
    let runner = ClaudeRunner::new(config);

    // For McpTools, generate inline MCP config
    let mcp_config_path = if vertical == Vertical::McpTools {
        let config_json = serde_json::json!({
            "mcpServers": {
                "conproxy": {
                    "command": config.conproxy_bin.to_string_lossy(),
                    "args": ["mcp"]
                }
            }
        });
        let path = config.base_dir.join("mcp-config.json");
        std::fs::write(&path, serde_json::to_string_pretty(&config_json).unwrap())
            .expect("Failed to write MCP config");
        Some(path)
    } else {
        None
    };

    let filtered: Vec<&EvalQuery> = queries.iter().collect();
    let concurrency = config.query_concurrency;

    let (permit_tx, permit_rx) = std::sync::mpsc::sync_channel::<()>(concurrency);
    for _ in 0..concurrency {
        let _ = permit_tx.send(());
    }
    let permit_rx = Arc::new(std::sync::Mutex::new(permit_rx));

    let query_results: Vec<QueryResult> = std::thread::scope(|s| {
        let handles: Vec<_> = filtered
            .iter()
            .enumerate()
            .map(|(i, query)| {
                let runner_ref = &runner;
                let doc_index_ref = doc_index;
                let vertical_dir_ref = vertical_dir;
                let mcp_ref = mcp_config_path.as_deref();
                let permit_rx = Arc::clone(&permit_rx);
                let permit_tx = permit_tx.clone();
                let total = filtered.len();

                s.spawn(move || {
                    let _permit = permit_rx.lock().unwrap().recv();

                    eprintln!(
                        "  [{}] [{}/{}] {} ...",
                        vertical.short_name(),
                        i + 1,
                        total,
                        &query.id
                    );

                    let invocation =
                        runner_ref.invoke(vertical, &query.query, mcp_ref, vertical_dir_ref);

                    let error = if !invocation.success && !invocation.stderr.is_empty() {
                        let first_lines: String = invocation
                            .stderr
                            .lines()
                            .take(3)
                            .collect::<Vec<_>>()
                            .join(" | ");
                        eprintln!(
                            "  [{}] {} error: {}",
                            vertical.short_name(),
                            &query.id,
                            &first_lines,
                        );
                        Some(invocation.stderr.lines().next().unwrap_or("").to_string())
                    } else if !invocation.success {
                        Some("CLI exited with non-zero status (no stderr)".to_string())
                    } else {
                        None
                    };

                    let recall_score = if invocation.success {
                        score_response(&invocation.stdout, query, doc_index_ref)
                    } else {
                        crate::helpers::scoring::QueryRecallScore {
                            query_id: query.id.clone(),
                            expected_doc_count: query.expected_doc_ids.len(),
                            matched_doc_count: 0,
                            recall: 0.0,
                            doc_scores: query
                                .expected_doc_ids
                                .iter()
                                .map(|id| crate::helpers::scoring::DocMatchScore {
                                    doc_id: id.clone(),
                                    matched: false,
                                    confidence: 0.0,
                                    matched_terms: vec![],
                                })
                                .collect(),
                        }
                    };

                    print_query_status(
                        vertical,
                        &recall_score,
                        invocation.duration.as_secs_f64(),
                        invocation.success,
                        invocation.timed_out,
                    );

                    let _ = permit_tx.send(());

                    QueryResult {
                        query_id: query.id.clone(),
                        query_text: query.query.clone(),
                        strategy: query.strategy.clone(),
                        recall: recall_score.recall,
                        expected_docs: recall_score.expected_doc_count,
                        matched_docs: recall_score.matched_doc_count,
                        response_time: invocation.duration,
                        success: invocation.success,
                        timed_out: invocation.timed_out,
                        doc_scores: recall_score.doc_scores,
                        response_text: invocation.stdout.clone(),
                        error,
                    }
                })
            })
            .collect();

        handles
            .into_iter()
            .map(|h| h.join().expect("Query thread panicked"))
            .collect()
    });

    VerticalResult::compute(vertical, query_results)
}

/// Run vertical using OpenAI-compatible (llama.cpp) provider.
fn run_vertical_llamacpp(
    vertical: Vertical,
    queries: &[EvalQuery],
    doc_index: &DocIndex,
    config: &EvalConfig,
    vertical_dir: &Path,
) -> VerticalResult {
    let runner = OpenAiCompatRunner::new(
        &config.llama_base_url,
        &config.llama_model,
        &config.llama_api_key,
        config.timeout,
    );

    // For McpTools, start MCP client once for the vertical
    let mcp_client = if vertical == Vertical::McpTools {
        match McpClient::start(&config.conproxy_bin, vertical_dir) {
            Ok(client) => Some(std::sync::Mutex::new(client)),
            Err(e) => {
                eprintln!(
                    "  [{}] Failed to start MCP client: {e}",
                    vertical.short_name()
                );
                None
            }
        }
    } else {
        None
    };

    let query_results: Vec<QueryResult> = queries
        .iter()
        .enumerate()
        .map(|(i, query)| {
            let total = queries.len();
            eprintln!(
                "  [{}] [{}/{}] {} ...",
                vertical.short_name(),
                i + 1,
                total,
                &query.id
            );

            let start = Instant::now();

            let (response_text, success, error) = match vertical {
                Vertical::NoContext => {
                    let prompt = format!(
                        "Answer the following question with specific technical details, names, and concepts.\n\n\
                         Question: {}",
                        query.query
                    );
                    match runner.simple_chat(&prompt, "Answer with specific technical details.") {
                        Ok(text) => (text, true, None),
                        Err(e) => (String::new(), false, Some(e)),
                    }
                }
                Vertical::McpTools => {
                    if let Some(ref mcp_mutex) = mcp_client {
                        let tools = conproxy_tools();
                        let prompt = format!(
                            "You have access to conproxy MCP tools: search.\n\
                             Use search to find relevant documents and answer with specific technical details.\n\n\
                             Question: {}",
                            query.query
                        );
                        let mut mcp = mcp_mutex.lock().unwrap();
                        let result = runner.chat_with_tools(
                            &prompt,
                            "Use the available tools to search for relevant documents and answer with specific technical details.",
                            &tools,
                            &mut mcp,
                            5,
                        );
                        (result.response, result.completed, None)
                    } else {
                        (String::new(), false, Some("MCP client not available".to_string()))
                    }
                }
            };

            let duration = start.elapsed();

            let recall_score = if success {
                score_response(&response_text, query, doc_index)
            } else {
                crate::helpers::scoring::QueryRecallScore {
                    query_id: query.id.clone(),
                    expected_doc_count: query.expected_doc_ids.len(),
                    matched_doc_count: 0,
                    recall: 0.0,
                    doc_scores: query
                        .expected_doc_ids
                        .iter()
                        .map(|id| crate::helpers::scoring::DocMatchScore {
                            doc_id: id.clone(),
                            matched: false,
                            confidence: 0.0,
                            matched_terms: vec![],
                        })
                        .collect(),
                }
            };

            print_query_status(vertical, &recall_score, duration.as_secs_f64(), success, false);

            QueryResult {
                query_id: query.id.clone(),
                query_text: query.query.clone(),
                strategy: query.strategy.clone(),
                recall: recall_score.recall,
                expected_docs: recall_score.expected_doc_count,
                matched_docs: recall_score.matched_doc_count,
                response_time: duration,
                success,
                timed_out: false,
                doc_scores: recall_score.doc_scores,
                response_text,
                error,
            }
        })
        .collect();

    VerticalResult::compute(vertical, query_results)
}

/// Print color-coded query status with doc match details.
fn print_query_status(
    vertical: Vertical,
    recall_score: &crate::helpers::scoring::QueryRecallScore,
    elapsed_secs: f64,
    success: bool,
    timed_out: bool,
) {
    let status = if timed_out {
        "\x1b[33mTIMEOUT\x1b[0m"
    } else if !success {
        "\x1b[31mFAILED\x1b[0m"
    } else if recall_score.recall >= 0.5 {
        "\x1b[32mGOOD\x1b[0m"
    } else if recall_score.recall > 0.0 {
        "\x1b[33mPARTIAL\x1b[0m"
    } else {
        "\x1b[31mMISS\x1b[0m"
    };

    eprintln!(
        "  [{}] recall={:.0}% time={:.1}s {}",
        vertical.short_name(),
        recall_score.recall * 100.0,
        elapsed_secs,
        status,
    );

    for ds in &recall_score.doc_scores {
        if !ds.matched_terms.is_empty() {
            eprintln!(
                "    {} ({:.0}%): {}",
                ds.doc_id,
                ds.confidence * 100.0,
                ds.matched_terms.join(", ")
            );
        } else {
            eprintln!(
                "    {} ({:.0}%): <no matches>",
                ds.doc_id,
                ds.confidence * 100.0
            );
        }
    }
}
