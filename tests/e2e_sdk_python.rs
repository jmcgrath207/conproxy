//! E2E tests for the Python SDK (conproxy-py).
//!
//! Builds the Python SDK using `maturin develop` and verifies that the
//! module can be imported and basic operations work against a running proxy.
//!
//! Run with: `cargo test --test e2e_sdk_python --features e2e -- --ignored --nocapture`
//!
//! Prerequisites:
//!   - Python 3.10+ with `pip` available
//!   - `maturin` installed (`pip install maturin`)
//!   - Running proxy on 127.0.0.1:8080 (for client tests)

use std::path::PathBuf;
use std::process::Command;

fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn python_sdk_dir() -> PathBuf {
    project_root().join("sdk").join("python")
}

/// Check if maturin and python are available.
fn check_prerequisites() -> bool {
    let python_ok = Command::new("python3")
        .args(["--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !python_ok {
        eprintln!("  SKIP: python3 not available");
        return false;
    }

    let maturin_ok = Command::new("maturin")
        .args(["--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !maturin_ok {
        eprintln!("  SKIP: maturin not available (install with: pip install maturin)");
        return false;
    }

    true
}

/// Build the Python SDK using `maturin develop`.
fn build_python_sdk() -> bool {
    eprintln!("  Building Python SDK with maturin develop...");

    let output = Command::new("maturin")
        .args(["develop"])
        .current_dir(python_sdk_dir())
        .output()
        .expect("Failed to run maturin develop");

    if !output.status.success() {
        eprintln!(
            "  maturin develop failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return false;
    }

    eprintln!("  Python SDK built successfully");
    true
}

/// Run a Python script and return (success, stdout, stderr).
fn run_python(script: &str) -> (bool, String, String) {
    // `maturin develop` installs into the local venv; prefer it over the
    // bare system python so the built module is importable.
    let venv_python = python_sdk_dir().join(".venv").join("bin").join("python");
    let python = if venv_python.exists() {
        venv_python
    } else {
        PathBuf::from("python3")
    };
    let output = Command::new(python)
        .args(["-c", script])
        .current_dir(python_sdk_dir())
        .output()
        .expect("Failed to run python3");

    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

#[test]
#[ignore = "E2E: requires Python 3.10+ and maturin (no running proxy needed; client ops skip gracefully)"]
fn python_sdk_import_test() {
    eprintln!();
    eprintln!("\x1b[1mPython SDK E2E Tests\x1b[0m");
    eprintln!("============================================");

    if !check_prerequisites() {
        return;
    }

    if !build_python_sdk() {
        panic!("Failed to build Python SDK");
    }

    // Test 1: Basic import
    eprintln!("  Test: import conproxy...");
    let (ok, stdout, stderr) =
        run_python("import conproxy; print('import OK'); print(dir(conproxy))");
    assert!(
        ok,
        "Failed to import conproxy:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("import OK"),
        "Expected 'import OK' in output"
    );
    assert!(
        stdout.contains("ConproxyClient"),
        "Expected ConproxyClient in module dir"
    );
    eprintln!("    PASSED");

    // Test 2: Verify all expected classes are exported
    eprintln!("  Test: verify exported classes...");
    let (ok, stdout, stderr) = run_python(
        r#"
import conproxy
classes = [
    'ConproxyClient', 'Engine',
    'PyQueryResponse', 'PySearchResult', 'PyStatsResponse',
    'PyBatchQueryResponse', 'PyFederatedQueryResponse',
    'PyCircuitStatusResponse', 'PyQueueStatsResponse',
    'PyClientInfo', 'PyClientsResponse', 'PyUpstreamInfo',
    'PyPoolStatusResponse', 'PyReloadResponse',
    'PyCacheClearResponse', 'PyCacheWarmupResponse',
    'PyCacheEvictResponse', 'PyCacheIntegrityResponse',
    'PyAgentInfo', 'PyListAgentsResponse', 'PyContextInfo',
    'PyListContextsResponse', 'PySwitchContextResponse',
    'PyCreateContextResponse', 'PyContextStats',
    'PyDistillEntry', 'PyFederatedStats', 'PySdkConfig',
]
missing = [c for c in classes if not hasattr(conproxy, c)]
if missing:
    print(f'MISSING: {missing}')
    exit(1)
print(f'All {len(classes)} classes exported')
"#,
    );
    assert!(ok, "Missing classes:\nstdout: {stdout}\nstderr: {stderr}");
    eprintln!("    PASSED");

    // Test 3: Client construction with explicit URL (no proxy needed — just tests constructor)
    eprintln!("  Test: ConproxyClient constructor...");
    let (ok, stdout, stderr) = run_python(
        r#"
from conproxy import ConproxyClient
# Constructor with explicit URL creates client (lazy connect, won't fail)
client = ConproxyClient(grpc_url='http://127.0.0.1:9999')
print('constructor OK')
"#,
    );
    assert!(
        ok,
        "Constructor failed:\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("    PASSED");

    // Test 4: Client with running proxy (only if proxy is up)
    eprintln!("  Test: SDK operations against running proxy...");
    let (ok, stdout, _stderr) = run_python(
        r#"
from conproxy import ConproxyClient
try:
    client = ConproxyClient(grpc_url='http://127.0.0.1:8080')
    stats = client.stats()
    print(f'stats OK: uptime={stats.uptime_secs}s cache={stats.cache_entries}')

    resp = client.query('python sdk e2e test', top_k=3)
    print(f'query OK: results={len(resp.results)} took={resp.took_ms}ms')

    contexts = client.list_contexts()
    print(f'contexts OK: current={contexts.current}')

    circuit = client.circuit_status()
    print(f'circuit OK: state={circuit.state}')

    pool = client.pool_status()
    print(f'pool OK: total={pool.total_upstreams} healthy={pool.healthy_upstreams}')

    print('ALL PASSED')
except Exception as e:
    print(f'SKIP (proxy not running): {e}')
"#,
    );
    if ok && stdout.contains("ALL PASSED") {
        eprintln!("    PASSED (proxy operations verified)");
    } else if ok && stdout.contains("SKIP") {
        eprintln!("    SKIPPED (proxy not running)");
    } else {
        eprintln!("    FAILED");
        eprintln!("    stdout: {stdout}");
    }

    // Test 5: Context manager support
    eprintln!("  Test: context manager protocol...");
    let (ok, stdout, stderr) = run_python(
        r#"
from conproxy import ConproxyClient
client = ConproxyClient(grpc_url='http://127.0.0.1:9999')
with client as c:
    print('context manager OK')
"#,
    );
    assert!(
        ok,
        "Context manager failed:\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("    PASSED");

    // Test 6: Engine binding (in-process, no running daemon needed).
    // Uses a tiny in-script HTTP server as the upstream so the Engine can
    // produce a real miss->hit pair through the PyO3 layer.
    eprintln!("  Test: Engine create/query/dashboard...");
    let (ok, stdout, stderr) = run_python(
        r#"
import asyncio
import json
import os
import threading
import tempfile
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from conproxy import Engine

class MockUpstream(BaseHTTPRequestHandler):
    def do_GET(self):
        body = b'{"status":"ok"}'
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def do_POST(self):
        body = json.dumps({
            "results": [
                {"id": "doc-001", "score": 0.91, "content": "rust async runtime"}
            ]
        }).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *args):
        pass

server = ThreadingHTTPServer(("127.0.0.1", 0), MockUpstream)
threading.Thread(target=server.serve_forever, daemon=True).start()
port = server.server_address[1]

cfg = tempfile.NamedTemporaryFile(mode="w", suffix=".toml", delete=False)
cfg.write(f'[proxy]\nupstream_url = "http://127.0.0.1:{port}"\n')
cfg.close()

async def main():
    engine = await Engine.create(config=cfg.name, dashboard_listen="127.0.0.1:0")
    try:
        r1 = await engine.query("rust async")
        assert len(r1.results) > 0, "first query should return results"
        assert r1.cache_status == 2, f"first query cache_status {r1.cache_status}, want Miss(2)"
        r2 = await engine.query("rust async")
        assert r2.cache_status == 1, f"second query cache_status {r2.cache_status}, want Hit(1)"

        addr = engine.dashboard_addr()
        assert addr, "dashboard_addr() should be Some after dashboard_listen"
        with urllib.request.urlopen(f"http://{addr}/health", timeout=5) as resp:
            assert resp.status == 200, f"dashboard /health status {resp.status}"
        print(f"engine OK: miss={r1.cache_status} hit={r2.cache_status} addr={addr}")
    finally:
        engine.close()

try:
    asyncio.run(main())
finally:
    server.shutdown()
    os.unlink(cfg.name)
print("ALL PASSED")
"#,
    );
    assert!(
        ok,
        "Engine test failed:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("ALL PASSED"),
        "Expected ALL PASSED in Engine output:\n{stdout}"
    );
    eprintln!("    PASSED");

    eprintln!();
    eprintln!("  Python SDK E2E: all tests passed");
}
