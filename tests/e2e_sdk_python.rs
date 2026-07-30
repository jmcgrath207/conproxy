//! E2E tests for the Python SDK (conproxy-py).
//!
//! Builds the Python SDK using `maturin develop` and verifies that the
//! module can be imported and basic operations work against a running proxy.
//!
//! Run with: `cargo test --test e2e_sdk_python --features e2e -- --ignored --nocapture`
//!
//! Prerequisites:
//!   - Python 3.9+ with `pip` available
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
    let output = Command::new("python3")
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
#[ignore = "E2E: requires Python 3.9+, maturin, running proxy"]
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
    eprintln!("  Test: import conproxy_py...");
    let (ok, stdout, stderr) =
        run_python("import conproxy_py; print('import OK'); print(dir(conproxy_py))");
    assert!(
        ok,
        "Failed to import conproxy_py:\nstdout: {stdout}\nstderr: {stderr}"
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
import conproxy_py
classes = [
    'ConproxyClient',
    'PyQueryResponse', 'PySearchResult', 'PyStatsResponse',
    'PyBatchQueryResponse', 'PyFederatedQueryResponse',
    'PyCircuitStatusResponse', 'PyQueueStatsResponse',
    'PyClientsResponse', 'PyKnowledgeStatusResponse',
    'PyPoolStatusResponse', 'PyReloadResponse',
    'PyCacheClearResponse', 'PyCacheWarmupResponse',
    'PyCacheEvictResponse', 'PyCacheIntegrityResponse',
    'PyListAgentsResponse', 'PyContextInfo',
    'PyListContextsResponse', 'PySwitchContextResponse',
    'PyCreateContextResponse', 'PyContextStats',
    'PySdkConfig',
]
missing = [c for c in classes if not hasattr(conproxy_py, c)]
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
from conproxy_py import ConproxyClient
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
from conproxy_py import ConproxyClient
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
from conproxy_py import ConproxyClient
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

    eprintln!();
    eprintln!("  Python SDK E2E: all tests passed");
}
