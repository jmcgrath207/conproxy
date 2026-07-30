use std::path::PathBuf;
use std::time::Duration;

/// Well-known base directory for all eval temp files.
/// Cleaned up at the start of each run.
const EVAL_BASE_DIR: &str = "/tmp/conproxy-eval";

/// LLM provider for the eval harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalProvider {
    Ollama,
    Claude,
    LlamaCpp,
}

impl EvalProvider {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "ollama" => Some(EvalProvider::Ollama),
            "claude" => Some(EvalProvider::Claude),
            "llamacpp" | "llama" | "openai-compat" => Some(EvalProvider::LlamaCpp),
            _ => None,
        }
    }
}

impl std::fmt::Display for EvalProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EvalProvider::Ollama => write!(f, "ollama"),
            EvalProvider::Claude => write!(f, "claude"),
            EvalProvider::LlamaCpp => write!(f, "llamacpp"),
        }
    }
}

/// The 2 eval verticals — different LLM configurations to compare.
///
/// Each vertical tests a different level of conproxy integration.
/// Both Ollama and Claude providers can run both verticals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vertical {
    /// Pure LLM baseline — no conproxy integration (control group).
    NoContext,
    /// MCP tool-calling with search via McpClient.
    McpTools,
}

impl Vertical {
    pub const ALL: &[Vertical] = &[Vertical::NoContext, Vertical::McpTools];

    pub fn name(&self) -> &'static str {
        match self {
            Vertical::NoContext => "no_context",
            Vertical::McpTools => "mcp_tools",
        }
    }

    pub fn short_name(&self) -> &'static str {
        match self {
            Vertical::NoContext => "No Ctx",
            Vertical::McpTools => "MCP Tools",
        }
    }

    /// Parse from string (matches env var values).
    pub fn from_str(s: &str) -> Option<Self> {
        match s.trim() {
            "no_context" => Some(Vertical::NoContext),
            "mcp_tools" => Some(Vertical::McpTools),
            _ => None,
        }
    }
}

impl std::fmt::Display for Vertical {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// Eval configuration from environment variables.
pub struct EvalConfig {
    /// LLM provider (default: Ollama).
    pub provider: EvalProvider,
    /// Claude CLI binary path (used when provider == Claude).
    pub claude_bin: PathBuf,
    /// Claude model override (used when provider == Claude).
    pub claude_model: Option<String>,
    /// Ollama base URL.
    pub ollama_base_url: String,
    /// Ollama model name.
    pub ollama_model: String,
    /// llama.cpp / OpenAI-compat base URL (used when provider == LlamaCpp).
    pub llama_base_url: String,
    /// llama.cpp model name (used when provider == LlamaCpp).
    pub llama_model: String,
    /// API key for OpenAI-compat endpoint.
    pub llama_api_key: String,
    pub conproxy_bin: PathBuf,
    pub project_root: PathBuf,
    pub timeout: Duration,
    pub enabled_verticals: Vec<Vertical>,
    pub enabled_queries: Option<Vec<String>>,
    pub output_dir: Option<PathBuf>,
    pub proxy_listen: String,
    /// Base directory for all eval temp files.
    pub base_dir: PathBuf,
    /// Max concurrent invocations per vertical (default: 3).
    pub query_concurrency: usize,
}

impl EvalConfig {
    pub fn from_env() -> Self {
        let provider = std::env::var("EVAL_PROVIDER")
            .ok()
            .and_then(|v| EvalProvider::from_str(&v))
            .unwrap_or(EvalProvider::Ollama);

        let claude_bin = std::env::var("CLAUDE_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("claude"));

        let claude_model = std::env::var("EVAL_CLAUDE_MODEL").ok();

        let ollama_base_url = std::env::var("OLLAMA_BASE_URL")
            .unwrap_or_else(|_| "http://localhost:11434".to_string());

        let ollama_model =
            std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "qwen3:0.6b".to_string());

        let llama_base_url =
            std::env::var("LLAMA_BASE_URL").unwrap_or_else(|_| "http://localhost:8081".to_string());

        let llama_model = std::env::var("LLAMA_MODEL").unwrap_or_else(|_| "auto".to_string());

        let llama_api_key =
            std::env::var("LLAMA_API_KEY").unwrap_or_else(|_| "sk-local".to_string());

        let conproxy_bin = std::env::var("PROXY_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("target")
                    .join("release")
                    .join("conproxy")
            });

        let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

        // Support both new and old env var names for timeout
        let timeout_secs: u64 = std::env::var("EVAL_TIMEOUT_SECS")
            .or_else(|_| std::env::var("EVAL_CLAUDE_TIMEOUT_SECS"))
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60);

        let enabled_verticals = match std::env::var("EVAL_VERTICALS") {
            Ok(val) if !val.is_empty() => val.split(',').filter_map(Vertical::from_str).collect(),
            _ => Vertical::ALL.to_vec(),
        };

        let enabled_queries = std::env::var("EVAL_QUERIES").ok().map(|val| {
            val.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        });

        let proxy_listen =
            std::env::var("EVAL_PROXY_LISTEN").unwrap_or_else(|_| "127.0.0.1:8080".to_string());

        let output_dir = std::env::var("EVAL_OUTPUT_DIR").ok().map(PathBuf::from);

        let base_dir = PathBuf::from(EVAL_BASE_DIR);

        let query_concurrency: usize = std::env::var("EVAL_QUERY_CONCURRENCY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3)
            .max(1);

        Self {
            provider,
            claude_bin,
            claude_model,
            ollama_base_url,
            ollama_model,
            llama_base_url,
            llama_model,
            llama_api_key,
            conproxy_bin,
            project_root,
            timeout: Duration::from_secs(timeout_secs),
            enabled_verticals,
            enabled_queries,
            output_dir,
            proxy_listen,
            base_dir,
            query_concurrency,
        }
    }

    /// Display string for the active model.
    pub fn model_display(&self) -> String {
        match self.provider {
            EvalProvider::Ollama => self.ollama_model.clone(),
            EvalProvider::Claude => self
                .claude_model
                .clone()
                .unwrap_or_else(|| "(default)".into()),
            EvalProvider::LlamaCpp => self.llama_model.clone(),
        }
    }

    /// Initialize the eval base directory.
    /// Always removes leftover files from previous runs.
    /// When provider is Claude, also cleans Claude project memory.
    pub fn init_base_dir(&self) {
        let _ = std::fs::remove_dir_all(&self.base_dir);
        std::fs::create_dir_all(&self.base_dir).expect("Failed to create eval base dir");

        // Only clean Claude project memory when using Claude provider
        if self.provider == EvalProvider::Claude {
            if let Some(home) = std::env::var_os("HOME") {
                let claude_projects = PathBuf::from(home).join(".claude").join("projects");
                if claude_projects.is_dir() {
                    if let Ok(entries) = std::fs::read_dir(&claude_projects) {
                        for entry in entries.flatten() {
                            let name = entry.file_name();
                            let name_str = name.to_string_lossy();
                            if name_str.starts_with("-tmp-conproxy-eval") {
                                let path = entry.path();
                                eprintln!("  Cleaning Claude project memory: {}", path.display());
                                let _ = std::fs::remove_dir_all(&path);
                            }
                        }
                    }
                }
            }
        }
    }
}
