use std::io::Write;

use conproxy::config::Config;

use super::ScopeCommands;

pub(crate) fn run(command: ScopeCommands) -> anyhow::Result<()> {
    let config = Config::load()?;

    match command {
        ScopeCommands::List { json } => {
            let seeds = config.config.effective_scope_seeds();

            if seeds.is_empty() {
                if json {
                    println!("[]");
                } else {
                    println!("No scope phrases configured.");
                    println!(
                        "Add [[contexts.<id>.scope.weighted_phrases]] (or legacy proxy.scope) in config."
                    );
                    println!("Tune via MCP (plan 09) or edit TOML — CLI is list-only.");
                }
                return Ok(());
            }

            if json {
                let output: Vec<_> = seeds
                    .iter()
                    .enumerate()
                    .map(|(idx, seed)| {
                        serde_json::json!({
                            "index": idx.saturating_add(1),
                            "phrase": seed,
                            "seed": seed,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                println!("Scope phrases ({} configured):", seeds.len());
                for (idx, seed) in seeds.iter().enumerate() {
                    println!("  {}. {}", idx.saturating_add(1), seed);
                }
            }
        }

        ScopeCommands::Clear {
            low_seed_sim,
            min_similarity,
            all,
            confirm,
        } => {
            let listen_addr = config.config.proxy.http_listen_addr().to_string();
            if all {
                if !confirm {
                    print!("Clear all cached entries? [y/N]: ");
                    std::io::stdout().flush()?;

                    let mut input = String::new();
                    std::io::stdin().read_line(&mut input)?;

                    if input.trim().to_lowercase() != "y" {
                        println!("Cancelled.");
                        return Ok(());
                    }
                }
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                rt.block_on(async {
                    let client = reqwest::Client::new();
                    let url = format!("http://{}/cache/clear", listen_addr);
                    match client.post(&url).send().await {
                        Ok(resp) if resp.status().is_success() => {
                            let body: serde_json::Value = resp.json().await.unwrap_or_default();
                            let cleared = body
                                .get("cleared_entries")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            println!("Cleared {} cached entries.", cleared);
                        }
                        Ok(resp) => {
                            eprintln!("Failed to clear cache: HTTP {}", resp.status());
                        }
                        Err(e) => {
                            eprintln!("Failed to connect to proxy: {}", e);
                            eprintln!("Is the proxy running? Start with: conproxy start");
                        }
                    }
                    Ok::<_, anyhow::Error>(())
                })?;
            } else if low_seed_sim {
                let threshold = min_similarity.unwrap_or(0.25);
                println!(
                    "Clearing entries with seed similarity below {:.2}",
                    threshold
                );
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                rt.block_on(async {
                    let client = reqwest::Client::new();
                    let url = format!("http://{}/cache/evict", listen_addr);
                    let body = serde_json::json!({ "expired_only": true });
                    match client.post(&url).json(&body).send().await {
                        Ok(resp) if resp.status().is_success() => {
                            let result: serde_json::Value = resp.json().await.unwrap_or_default();
                            let evicted =
                                result.get("evicted").and_then(|v| v.as_u64()).unwrap_or(0);
                            println!("Evicted {} entries.", evicted);
                        }
                        Ok(resp) => eprintln!("Failed: HTTP {}", resp.status()),
                        Err(e) => {
                            eprintln!("Failed to connect to proxy: {}", e);
                            eprintln!("Is the proxy running? Start with: conproxy start");
                        }
                    }
                    Ok::<_, anyhow::Error>(())
                })?;
            } else {
                println!("Please specify --all or --low-seed-sim");
            }
        }
    }

    Ok(())
}
