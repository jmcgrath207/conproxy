use super::mcp_client::McpClient;
use serde_json::{json, Value};
use std::time::Duration;

/// Result of a multi-turn tool-calling conversation.
#[allow(dead_code)]
pub struct ToolCallingResult {
    /// Final text response from the model (after all tool calls resolved).
    pub response: String,
    /// Total number of tool calls made across all turns.
    pub tool_calls_made: usize,
    /// Number of conversation turns (each turn = one API call).
    pub turns: usize,
    /// Whether the conversation completed (vs hitting max_turns).
    pub completed: bool,
}

/// HTTP client wrapping Ollama's `POST /api/chat` endpoint.
pub struct OllamaRunner {
    client: reqwest::blocking::Client,
    base_url: String,
    model: String,
}

impl OllamaRunner {
    pub fn new(base_url: &str, model: &str, timeout: Duration) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .build()
            .expect("Failed to build HTTP client");

        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
        }
    }

    /// Check that Ollama is running and the model is available.
    pub fn check_ready(&self) -> Result<(), String> {
        let url = format!("{}/api/tags", self.base_url);
        let resp = self
            .client
            .get(&url)
            .send()
            .map_err(|e| format!("Ollama not reachable at {}: {e}", self.base_url))?;

        if !resp.status().is_success() {
            return Err(format!("Ollama returned {}", resp.status()));
        }

        let body: Value = resp
            .json()
            .map_err(|e| format!("Invalid JSON from Ollama: {e}"))?;

        let models = body["models"]
            .as_array()
            .ok_or("No 'models' array in /api/tags response")?;

        let found = models.iter().any(|m| {
            m["name"]
                .as_str()
                .map(|n| n == self.model || n.starts_with(&format!("{}:", self.model)))
                .unwrap_or(false)
        });

        if !found {
            let available: Vec<&str> = models.iter().filter_map(|m| m["name"].as_str()).collect();
            return Err(format!(
                "Model '{}' not found. Available: {:?}",
                self.model, available
            ));
        }

        Ok(())
    }

    /// Single-turn chat with no tool calling.
    ///
    /// Returns the assistant's text response.
    pub fn simple_chat(&self, prompt: &str, system_prompt: &str) -> Result<String, String> {
        let messages = vec![
            json!({"role": "system", "content": system_prompt}),
            json!({"role": "user", "content": prompt}),
        ];
        let resp = self.chat(&messages, &[])?;
        Ok(resp["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string())
    }

    /// Single-turn chat (internal).
    fn chat(&self, messages: &[Value], tools: &[Value]) -> Result<Value, String> {
        let url = format!("{}/api/chat", self.base_url);

        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "stream": false,
        });

        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .map_err(|e| format!("Ollama chat request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            return Err(format!("Ollama returned {status}: {text}"));
        }

        resp.json()
            .map_err(|e| format!("Invalid JSON from Ollama chat: {e}"))
    }

    /// Multi-turn tool-calling loop.
    ///
    /// Sends the prompt with tools, executes any tool calls via the McpClient,
    /// appends tool results as messages, and repeats until the model returns
    /// a text response (no tool_calls) or max_turns is reached.
    pub fn chat_with_tools(
        &self,
        prompt: &str,
        system_prompt: &str,
        tools: &[Value],
        mcp_client: &mut McpClient,
        max_turns: usize,
    ) -> ToolCallingResult {
        let mut messages = vec![
            json!({"role": "system", "content": system_prompt}),
            json!({"role": "user", "content": prompt}),
        ];

        let mut total_tool_calls = 0;

        for turn in 0..max_turns {
            let response = match self.chat(&messages, tools) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("    Ollama error on turn {}: {e}", turn + 1);
                    return ToolCallingResult {
                        response: format!("[Ollama error: {e}]"),
                        tool_calls_made: total_tool_calls,
                        turns: turn + 1,
                        completed: false,
                    };
                }
            };

            let message = &response["message"];
            let tool_calls = message["tool_calls"].as_array();

            // If no tool calls, this is the final text response
            if tool_calls.is_none() || tool_calls.unwrap().is_empty() {
                let content = message["content"].as_str().unwrap_or("").to_string();
                return ToolCallingResult {
                    response: content,
                    tool_calls_made: total_tool_calls,
                    turns: turn + 1,
                    completed: true,
                };
            }

            let tool_calls = tool_calls.unwrap();

            // Append assistant message with tool calls
            messages.push(message.clone());

            // Execute each tool call via MCP
            for tc in tool_calls {
                let func = &tc["function"];
                let tool_name = func["name"].as_str().unwrap_or("unknown");
                let arguments = &func["arguments"];

                eprintln!("    Tool call: {}({})", tool_name, arguments);
                total_tool_calls += 1;

                let result = match mcp_client.call_tool(tool_name, arguments) {
                    Ok(text) => text,
                    Err(e) => format!("Error calling {tool_name}: {e}"),
                };

                // Append tool result message
                messages.push(json!({
                    "role": "tool",
                    "content": result,
                }));
            }
        }

        // Hit max turns — extract whatever content we have
        let last_content = messages
            .last()
            .and_then(|m| m["content"].as_str())
            .unwrap_or("")
            .to_string();

        ToolCallingResult {
            response: last_content,
            tool_calls_made: total_tool_calls,
            turns: max_turns,
            completed: false,
        }
    }
}
