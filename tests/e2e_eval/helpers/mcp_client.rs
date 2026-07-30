use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};

/// Minimal MCP stdio client for driving `conproxy mcp` in tests.
///
/// Adapted from `tests/mcp_test.rs` patterns. Spawns the conproxy mcp
/// process, performs the MCP handshake, and provides `call_tool()` for
/// executing tools over JSON-RPC.
pub struct McpClient {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl McpClient {
    /// Spawn `conproxy mcp` and perform the MCP initialize handshake.
    ///
    /// `working_dir` must contain a `.conproxy/conproxy.toml` with proxy config
    /// so the MCP server can route search requests to the proxy.
    pub fn start(conproxy_bin: &Path, working_dir: &Path) -> Result<Self, String> {
        let mut child = Command::new(conproxy_bin)
            .current_dir(working_dir)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn conproxy mcp: {e}"))?;

        let stdin = child.stdin.take().ok_or("No stdin on child")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("No stdout on child")?);

        let mut client = Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        };

        // MCP initialize handshake
        client.initialize()?;

        Ok(client)
    }

    fn initialize(&mut self) -> Result<(), String> {
        // Send initialize request
        let init_req = json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "eval-test",
                    "version": "0.1.0"
                }
            },
            "id": self.next_id()
        });

        let resp = self
            .send_and_recv(&init_req)?
            .ok_or("No response to initialize")?;

        if resp.get("error").is_some() {
            return Err(format!("MCP initialize error: {}", resp["error"]));
        }

        // Send initialized notification
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        self.send_notification(&notification)?;

        Ok(())
    }

    /// Call an MCP tool by name with the given arguments.
    ///
    /// Returns the text content from the first content item in the response.
    pub fn call_tool(&mut self, name: &str, arguments: &Value) -> Result<String, String> {
        let req = json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments
            },
            "id": self.next_id()
        });

        let resp = self
            .send_and_recv(&req)?
            .ok_or_else(|| format!("No response to tools/call {name}"))?;

        if let Some(error) = resp.get("error") {
            return Err(format!("MCP tool error: {error}"));
        }

        // Extract text from first content item
        let content = resp["result"]["content"]
            .as_array()
            .ok_or("No content array in tool result")?;

        if content.is_empty() {
            return Ok(String::new());
        }

        Ok(content[0]["text"].as_str().unwrap_or("").to_string())
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn send_and_recv(&mut self, msg: &Value) -> Result<Option<Value>, String> {
        let line = serde_json::to_string(msg).map_err(|e| format!("JSON serialize: {e}"))?;
        writeln!(self.stdin, "{}", line).map_err(|e| format!("Write to stdin: {e}"))?;
        self.stdin
            .flush()
            .map_err(|e| format!("Flush stdin: {e}"))?;

        let mut response = String::new();
        self.stdout
            .read_line(&mut response)
            .map_err(|e| format!("Read from stdout: {e}"))?;

        if response.is_empty() {
            return Ok(None);
        }

        let parsed: Value = serde_json::from_str(response.trim())
            .map_err(|e| format!("Invalid JSON response: {e}"))?;
        Ok(Some(parsed))
    }

    fn send_notification(&mut self, msg: &Value) -> Result<(), String> {
        let line = serde_json::to_string(msg).map_err(|e| format!("JSON serialize: {e}"))?;
        writeln!(self.stdin, "{}", line).map_err(|e| format!("Write to stdin: {e}"))?;
        self.stdin
            .flush()
            .map_err(|e| format!("Flush stdin: {e}"))?;
        Ok(())
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
