//! conproxy CLI binary
//!
//! A cache proxy server for RAG/vector search with LLM passthrough.

mod commands;

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

use clap::{Parser, Subcommand, ValueEnum};
use conproxy::config::Config;

/// Bytes per megabyte for size conversions.

#[derive(Parser)]
#[command(name = "conproxy")]
#[command(author, version, about = "Cache proxy server for RAG/vector search", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, ValueEnum, Debug, Default)]
enum OutputFormat {
    #[default]
    Text,
    Json,
    Markdown,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the cache proxy server
    Start {
        /// Listen address (e.g., "127.0.0.1:9999")
        #[arg(long)]
        listen: Option<String>,

        /// Upstream RAG service URL
        #[arg(long)]
        upstream: Option<String>,

        /// Run in background (daemon mode)
        #[arg(long)]
        daemon: bool,

        /// Node ID for peer replication (overrides config)
        #[arg(long)]
        node_id: Option<String>,

        /// Peer addresses for cache replication (comma-separated)
        #[arg(long)]
        peers: Option<String>,

        /// Path to conproxy.toml (default: merge ~/.conproxy + .conproxy/)
        #[arg(long)]
        config: Option<String>,
    },

    /// Stop the running proxy
    Stop,

    /// Show proxy status
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Show peer replication status
    Peer {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Show CDC event stream status
    Cdc {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// List available cache contexts
    Contexts {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Manage a specific context
    Context {
        /// Context ID to show or switch to
        id: String,

        /// Switch to this context
        #[arg(long)]
        switch: bool,

        /// Create the context if it doesn't exist
        #[arg(long)]
        create: bool,

        /// Upstream URL for new context
        #[arg(long)]
        upstream: Option<String>,

        /// Collection name for new context
        #[arg(long)]
        collection: Option<String>,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Install proxy as a system service
    Install {
        /// Listen address for the service
        #[arg(long)]
        listen: Option<String>,

        /// Upstream URL for the service
        #[arg(long)]
        upstream: Option<String>,

        /// Start the service after installation
        #[arg(long)]
        start: bool,
    },

    /// Uninstall the proxy system service
    Uninstall {
        /// Remove service configuration files
        #[arg(long)]
        purge: bool,
    },

    /// Show proxy service logs
    Logs {
        /// Number of lines to show
        #[arg(short = 'n', long, default_value = "50")]
        lines: usize,

        /// Follow log output (like tail -f)
        #[arg(short, long)]
        follow: bool,
    },

    /// Dump cache entries to disk (markdown + JSON) for LLM ingestion
    Distill {
        /// Filter by context ID
        #[arg(long)]
        context: Option<String>,

        /// Cache tier to dump (primary, semantic, both)
        #[arg(long, value_enum, default_value = "primary")]
        tier: DistillTierArg,

        /// Maximum number of entries to dump
        #[arg(long, default_value = "0")]
        limit: u32,

        /// Include entries past their fresh TTL
        #[arg(long)]
        include_stale: bool,

        /// Output directory (overrides config.distill.output_dir)
        #[arg(long)]
        output_dir: Option<std::path::PathBuf>,

        /// Write consolidated index file instead of per-entry files
        #[arg(long)]
        cat: bool,

        /// Post-process command (whitespace-split, no shell)
        #[arg(long)]
        post_process: Option<String>,
    },

    /// List scope phrases / clear cache (ops-thin; tune via MCP or TOML)
    Scope(ScopeArgs),

    /// Deprecated alias for `scope` (one release)
    #[command(hide = false)]
    Seed(ScopeArgs),

    /// Search documentation via proxy
    Search {
        /// Search query
        query: String,
        /// Maximum number of results
        #[arg(short, long, default_value = "5")]
        limit: usize,
        /// Output format
        #[arg(short, long, value_enum, default_value = "text")]
        format: OutputFormat,
    },

    /// Start the MCP server (stdio transport for MCP clients — Claude Desktop, opencode)
    #[cfg(feature = "mcp")]
    Mcp {},

    /// Start the MCP server (stdio transport for MCP clients — Claude Desktop, opencode)
    #[cfg(not(feature = "mcp"))]
    Mcp {},
}

#[derive(Clone, Copy, clap::ValueEnum, Debug, Default)]
enum DistillTierArg {
    /// Primary cache tier only.
    #[default]
    Primary,
    /// Semantic cache tier only.
    Semantic,
    /// Both tiers merged.
    Both,
}

#[derive(clap::Args)]
struct ScopeArgs {
    #[command(subcommand)]
    command: ScopeCommands,
}

#[derive(Subcommand)]
enum ScopeCommands {
    /// List configured scope phrases (read-only)
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Clear cached entries (ops)
    Clear {
        /// Clear entries below seed similarity threshold
        #[arg(long)]
        low_seed_sim: bool,

        /// Minimum seed similarity to keep (default: uses config)
        #[arg(long)]
        min_similarity: Option<f32>,

        /// Clear all cached entries
        #[arg(long)]
        all: bool,

        /// Skip confirmation prompt
        #[arg(long)]
        confirm: bool,
    },
}

/// Internal enum mirroring the proxy-related top-level commands.
/// Used by commands/proxy.rs to match on proxy subcommands.
enum ProxyCommands {
    Start {
        listen: Option<String>,
        upstream: Option<String>,
        daemon: bool,
        node_id: Option<String>,
        peers: Option<String>,
        /// Path to conproxy.toml (default: merge ~/.conproxy + .conproxy/)
        config: Option<String>,
    },
    Stop,
    Status {
        json: bool,
    },
    Contexts {
        json: bool,
    },
    Context {
        id: String,
        switch: bool,
        create: bool,
        upstream: Option<String>,
        collection: Option<String>,
        json: bool,
    },
    Install {
        listen: Option<String>,
        upstream: Option<String>,
        start: bool,
    },
    Uninstall {
        purge: bool,
    },
    Logs {
        lines: usize,
        follow: bool,
    },
    Peer {
        json: bool,
    },
    Cdc {
        json: bool,
    },
    Distill {
        context: Option<String>,
        tier: DistillTierArg,
        limit: u32,
        include_stale: bool,
        output_dir: Option<std::path::PathBuf>,
        cat: bool,
        post_process: Option<String>,
    },
}

fn main() {
    #[cfg(feature = "dhat-heap")]
    let _profiler = if std::env::var("CONPROXY_DHAT").as_deref() == Ok("1") {
        Some(dhat::Profiler::new_heap())
    } else {
        None
    };

    if let Err(e) = run() {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Start {
            listen,
            upstream,
            daemon,
            node_id,
            peers,
            config,
        } => {
            commands::proxy::run(ProxyCommands::Start {
                listen,
                upstream,
                daemon,
                node_id,
                peers,
                config,
            })?;
        }

        Commands::Stop => {
            commands::proxy::run(ProxyCommands::Stop)?;
        }

        Commands::Status { json } => {
            commands::proxy::run(ProxyCommands::Status { json })?;
        }

        Commands::Peer { json } => {
            commands::proxy::run(ProxyCommands::Peer { json })?;
        }

        Commands::Cdc { json } => {
            commands::proxy::run(ProxyCommands::Cdc { json })?;
        }

        Commands::Contexts { json } => {
            commands::proxy::run(ProxyCommands::Contexts { json })?;
        }

        Commands::Context {
            id,
            switch,
            create,
            upstream,
            collection,
            json,
        } => {
            commands::proxy::run(ProxyCommands::Context {
                id,
                switch,
                create,
                upstream,
                collection,
                json,
            })?;
        }

        Commands::Install {
            listen,
            upstream,
            start,
        } => {
            commands::proxy::run(ProxyCommands::Install {
                listen,
                upstream,
                start,
            })?;
        }

        Commands::Uninstall { purge } => {
            commands::proxy::run(ProxyCommands::Uninstall { purge })?;
        }

        Commands::Logs { lines, follow } => {
            commands::proxy::run(ProxyCommands::Logs { lines, follow })?;
        }

        Commands::Distill {
            context,
            tier,
            limit,
            include_stale,
            output_dir,
            cat,
            post_process,
        } => {
            commands::proxy::run(ProxyCommands::Distill {
                context,
                tier,
                limit,
                include_stale,
                output_dir,
                cat,
                post_process,
            })?;
        }

        Commands::Scope(args) | Commands::Seed(args) => {
            commands::seed::run(args.command)?;
        }

        Commands::Search {
            query,
            limit,
            format,
        } => {
            run_search(&query, limit, format)?;
        }

        #[cfg(feature = "mcp")]
        Commands::Mcp {} => {
            run_mcp()?;
        }

        #[cfg(not(feature = "mcp"))]
        Commands::Mcp {} => {
            eprintln!("error: 'mcp' command requires the 'mcp' feature");
            eprintln!();
            eprintln!("Rebuild with: cargo build --release --features mcp");
            std::process::exit(1);
        }
    }

    Ok(())
}

// =============================================================================
// Search via proxy
// =============================================================================

fn run_search(query: &str, limit: usize, format: OutputFormat) -> anyhow::Result<()> {
    use conproxy::proxy::lifecycle;

    if lifecycle::is_proxy_running() {
        return run_search_proxy(query, limit, format);
    }

    let _ = (query, limit, format);
    eprintln!("No proxy running. Start the proxy: conproxy start");
    Ok(())
}

fn run_search_proxy(query: &str, limit: usize, format: OutputFormat) -> anyhow::Result<()> {
    use conproxy::proxy::QueryRequest;

    let config = Config::load()?;
    let http_addr = config.config.proxy.http_listen_addr();
    let url = format!("http://{}/query", http_addr);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let response = rt.block_on(async {
        let client = reqwest::Client::new();
        let request = QueryRequest {
            query: query.to_string(),
            top_k: Some(limit),
            priority: None,
            upstream_id: None,
            upstream_type: None,
        };

        let resp = client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to proxy: {}", e))?;

        if !resp.status().is_success() {
            return Err(anyhow::anyhow!("Proxy returned error: {}", resp.status()));
        }

        let proxy_response: conproxy::proxy::QueryResponse = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse proxy response: {}", e))?;

        Ok(proxy_response)
    })?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        OutputFormat::Text | OutputFormat::Markdown => {
            if response.results.is_empty() {
                println!("No results found for '{}'", query);
            } else {
                println!("Found {} results:\n", response.results.len());
                for (i, r) in response.results.iter().enumerate() {
                    let content_preview = if r.content.len() > 200 {
                        format!("{}...", &r.content[..200])
                    } else {
                        r.content.clone()
                    };
                    println!("{}. {} (score: {:.2})", i.saturating_add(1), r.id, r.score);
                    println!("   {}\n", content_preview.replace('\n', " "));
                }
            }
        }
    }

    Ok(())
}

// =============================================================================
// MCP Server
// =============================================================================

#[cfg(feature = "mcp")]
fn run_mcp() -> anyhow::Result<()> {
    let config = Config::load()?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    rt.block_on(conproxy::mcp::run_server(config))
}
