use serde_json::{json, Value};

/// Return the 2 MCP tools in Ollama's tool-calling format.
///
/// These match the tool schemas defined in `src/mcp/mod.rs`:
/// - `search` — proxy search (query required, limit optional)
/// - `conproxy_list` — list installed packages (no params)
pub fn conproxy_tools() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "search",
                "description": "Search via the cache proxy. Queries the configured upstream vector database through the proxy cache. Requires the 'proxy' feature and a running proxy server.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "The search query string"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of results (default: 10)"
                        }
                    },
                    "required": ["query"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "conproxy_list",
                "description": "List all installed conproxy packages. Returns JSON with name, git_url, and tag.",
                "parameters": {
                    "type": "object",
                    "properties": {}
                }
            }
        }),
    ]
}
