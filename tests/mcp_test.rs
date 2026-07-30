//! Integration tests for MCP server (Phase 11)
//!
//! Tests the `conproxy mcp` command by spawning the process and
//! communicating over stdio using the MCP JSON-RPC protocol.
//!
//! Requires: `cargo test --features mcp`

#![cfg(feature = "mcp")]

mod common;

use common::*;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
/// Spawn the MCP server process in a fresh temp directory.
/// The temp dir is leaked intentionally (cleaned up by OS) because
/// the child process holds a reference to it.
fn spawn_mcp_server() -> std::process::Child {
    let dir = temp_dir();

    // Initialize a conproxy project so Config::load() works
    write_project_config(dir.path());
    let child = Command::new(conproxy_bin())
        .current_dir(dir.path())
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn conproxy mcp");

    // Leak the temp dir so it outlives the child process
    std::mem::forget(dir);

    child
}

fn initialize_request(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "conproxy-test",
                "version": "0.1.0"
            }
        },
        "id": id
    })
}

fn initialized_notification() -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    })
}

fn tools_list_request(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "tools/list",
        "params": {},
        "id": id
    })
}

fn tools_call_request(id: u64, tool_name: &str, args: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "tools/call",
        "params": {
            "name": tool_name,
            "arguments": args,
        },
        "id": id
    })
}

/// Send a JSON-RPC message (newline-delimited) and read the response line.
fn send_and_recv(stdin: &mut impl Write, stdout: &mut impl BufRead, msg: &Value) -> Option<Value> {
    let line = serde_json::to_string(msg).unwrap();
    writeln!(stdin, "{}", line).unwrap();
    stdin.flush().unwrap();

    let mut response = String::new();
    // Read with a simple approach — the server sends one line per response
    stdout.read_line(&mut response).unwrap();

    if response.is_empty() {
        return None;
    }

    Some(serde_json::from_str(response.trim()).expect("Invalid JSON response"))
}

/// Send a notification (no response expected)
fn send_notification(stdin: &mut impl Write, msg: &Value) {
    let line = serde_json::to_string(msg).unwrap();
    writeln!(stdin, "{}", line).unwrap();
    stdin.flush().unwrap();
}

#[test]
fn test_mcp_server_starts_and_initializes() {
    let mut child = spawn_mcp_server();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // Send initialize request
    let resp = send_and_recv(&mut stdin, &mut stdout, &initialize_request(1));
    assert!(resp.is_some(), "Server should respond to initialize");

    let resp = resp.unwrap();
    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 1);

    let result = &resp["result"];
    assert!(result.is_object(), "Should have a result object");
    assert_eq!(result["serverInfo"]["name"], "conproxy");

    // Send initialized notification
    send_notification(&mut stdin, &initialized_notification());

    // Clean up
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn test_mcp_tools_list() {
    let mut child = spawn_mcp_server();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // Initialize
    let resp = send_and_recv(&mut stdin, &mut stdout, &initialize_request(1));
    assert!(resp.is_some());
    send_notification(&mut stdin, &initialized_notification());

    // List tools
    let resp = send_and_recv(&mut stdin, &mut stdout, &tools_list_request(2));
    assert!(resp.is_some(), "Server should respond to tools/list");

    let resp = resp.unwrap();
    assert_eq!(resp["id"], 2);

    let tools = resp["result"]["tools"]
        .as_array()
        .expect("tools should be an array");

    let tool_names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();

    // search is always registered
    assert!(
        tool_names.contains(&"search"),
        "Should have search tool, got: {:?}",
        tool_names
    );

    // Plan 08 W3: tune session + at least one tune family tool present
    for required in [
        "tune_session_open",
        "tune_session_close",
        "scope_tune",
        "tune_export",
    ] {
        assert!(
            tool_names.contains(&required),
            "Should have {required}, got: {tool_names:?}"
        );
    }

    // Clean up
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn test_mcp_server_info_has_instructions() {
    let mut child = spawn_mcp_server();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // Initialize
    let resp = send_and_recv(&mut stdin, &mut stdout, &initialize_request(1));
    let resp = resp.unwrap();

    let result = &resp["result"];
    let instructions = result["instructions"].as_str();
    assert!(instructions.is_some(), "Should have instructions");
    assert!(
        instructions.unwrap().contains("search"),
        "Instructions should mention search"
    );

    // Clean up
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn test_mcp_tools_call_returns_response() {
    let mut child = spawn_mcp_server();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // Initialize + initialized notification
    let resp = send_and_recv(&mut stdin, &mut stdout, &initialize_request(1));
    assert!(resp.is_some(), "initialize should respond");
    send_notification(&mut stdin, &initialized_notification());

    // Call search — no proxy running so we expect a graceful error response,
    // not a panic/crash. The MCP protocol requires a structured response either way.
    let resp = send_and_recv(
        &mut stdin,
        &mut stdout,
        &tools_call_request(2, "search", json!({"query": "test"})),
    );
    assert!(resp.is_some(), "tools/call should respond");

    let resp = resp.unwrap();
    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 2);

    // tools/call returns either a `result` (success or graceful isError) or
    // a JSON-RPC `error` envelope. Both are valid MCP responses.
    if let Some(result) = resp.get("result") {
        // result.is_object() — at minimum should be a structured object
        assert!(result.is_object(), "result should be an object");
    } else if let Some(err) = resp.get("error") {
        // JSON-RPC error envelope (e.g., -32603 internal error)
        assert!(err.is_object(), "error should be an object");
        assert!(
            err["code"].is_i64() || err["code"].is_number(),
            "error code present"
        );
    } else {
        panic!("tools/call response should have either `result` or `error`, got: {resp}");
    }

    // Clean up
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn test_mcp_tune_session_open_and_close() {
    // Plan 05 T3: at least one automated tune path beyond pure unit.
    // Exercises the MCP tools/call routing for tune (no upstream needed — dry-run session).
    let mut child = spawn_mcp_server();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // Initialize + initialized notification
    let resp = send_and_recv(&mut stdin, &mut stdout, &initialize_request(1));
    assert!(resp.is_some(), "initialize should respond");
    send_notification(&mut stdin, &initialized_notification());

    // Open a tune session bound to agent_id + context_id
    let resp = send_and_recv(
        &mut stdin,
        &mut stdout,
        &tools_call_request(
            2,
            "tune_session_open",
            json!({
                "agent_id": "tune-test-agent",
                "context_id": "default",
            }),
        ),
    );
    assert!(resp.is_some(), "tune session open should respond");
    let resp = resp.unwrap();
    assert_eq!(resp["id"], 2);

    // The session-open tool returns the TuneSession JSON wrapped via rmcp's
    // `result.content` (list of content blocks). Extract the first text block
    // and parse it as JSON to access TuneSession fields.
    let result = resp
        .get("result")
        .expect("tune session open should return a result envelope");
    let content = result["content"]
        .as_array()
        .expect("result.content should be an array");
    let text = content[0]["text"]
        .as_str()
        .expect("first content block should be text");
    let session: Value =
        serde_json::from_str(text).expect("tool result text should be JSON TuneSession");
    let session_id = session["session_id"]
        .as_str()
        .expect("session_id should be a non-null string");
    assert!(
        !session_id.is_empty(),
        "session_id should be non-empty, got: {session_id}"
    );
    assert_eq!(session["agent_id"], "tune-test-agent");
    assert_eq!(session["context_id"], "default");

    // Plan 08 W3: one dry-run scope tune with caller-supplied hits (no upstream).
    let resp = send_and_recv(
        &mut stdin,
        &mut stdout,
        &tools_call_request(
            3,
            "scope_tune",
            json!({
                "session_id": session_id,
                "hits": [
                    {"id": "h1", "content": "rust async tokio", "score": 0.9},
                    {"id": "h2", "content": "unrelated cooking recipe", "score": 0.8}
                ],
                "weighted_phrases": [{"text": "rust async", "weight": 1.0}],
            }),
        ),
    );
    assert!(resp.is_some(), "scope tune should respond");
    let resp = resp.unwrap();
    let result = resp
        .get("result")
        .expect("scope tune should return a result envelope");
    let content = result["content"]
        .as_array()
        .expect("result.content should be an array");
    let text = content[0]["text"]
        .as_str()
        .expect("first content block should be text");
    assert!(
        !text.is_empty(),
        "scope tune result text should be non-empty"
    );
    // Should be JSON report (or structured error text) — not a crash.
    let _ = serde_json::from_str::<Value>(text);

    // Close the session by session_id
    let resp = send_and_recv(
        &mut stdin,
        &mut stdout,
        &tools_call_request(4, "tune_session_close", json!({ "session_id": session_id })),
    );
    assert!(resp.is_some(), "tune session close should respond");
    let resp = resp.unwrap();
    let result = resp
        .get("result")
        .expect("tune session close should return a result envelope");
    let content = result["content"]
        .as_array()
        .expect("result.content should be an array");
    let text = content[0]["text"]
        .as_str()
        .expect("first content block should be text");
    let close_payload: Value = serde_json::from_str(text).expect("close result should be JSON");
    assert_eq!(
        close_payload["closed"], true,
        "session should be closed, got: {close_payload}"
    );

    // Clean up
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
}
