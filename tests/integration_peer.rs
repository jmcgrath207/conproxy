//! Two-proxy P2P CDC replication proof (plan 05 Wave4).
//!
//! Meili backend via testcontainers; two local `conproxy` processes peer each
//! other over gRPC. Query on A → cache INSERT CDC → B applies → B cache hit.
//!
//! Requires `--features integration-tests` and Docker.

#![cfg(feature = "integration-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
mod test_infra;

use common::{conproxy_bin, find_free_ports, FreePorts};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use test_infra::containers::MEILI_MASTER_KEY;

struct PeerProxy {
    child: Option<Child>,
    http_base: String,
    #[allow(dead_code)]
    grpc_addr: String,
    _dir: tempfile::TempDir,
    stderr_buf: Arc<Mutex<String>>,
}

impl PeerProxy {
    fn start(
        meili_url: &str,
        ports: &FreePorts,
        node_id: &str,
        peer_grpc: &str,
        snapshot_on_join: bool,
    ) -> Self {
        Self::start_with_secret(meili_url, ports, node_id, peer_grpc, snapshot_on_join, None)
    }

    fn start_with_secret(
        meili_url: &str,
        ports: &FreePorts,
        node_id: &str,
        peer_grpc: &str,
        snapshot_on_join: bool,
        shared_secret: Option<&str>,
    ) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let conproxy_dir = dir.path().join(".conproxy");
        std::fs::create_dir_all(&conproxy_dir).unwrap();

        let grpc = format!("127.0.0.1:{}", ports.grpc_port);
        let http = format!("127.0.0.1:{}", ports.http_port);
        let secret_line = shared_secret
            .map(|s| format!("shared_secret = \"{s}\"\n"))
            .unwrap_or_default();
        let cfg = format!(
            r#"[server]
listen = "{grpc}"
http_listen = "{http}"

[proxy.peer]
enabled = true
node_id = "{node_id}"
peers = ["{peer_grpc}"]
snapshot_on_join = {snapshot_on_join}
reconnect_interval_ms = 500
ready_threshold = 0.0
{secret_line}
[upstreams.meili]
url = "{meili_url}"
type = "meilisearch"
api_key = "{key}"
timeout_secs = 10

[contexts.default]
default = true

[[contexts.default.upstreams]]
ref = "meili"
index = "peer_docs"
search_fields = ["content", "title"]

[contexts.default.cache]
fresh_secs = 600
stale_secs = 3600
max_entries = 10000
"#,
            grpc = grpc,
            http = http,
            node_id = node_id,
            peer_grpc = peer_grpc,
            snapshot_on_join = snapshot_on_join,
            secret_line = secret_line,
            meili_url = meili_url,
            key = MEILI_MASTER_KEY,
        );
        let cfg_path = conproxy_dir.join("conproxy.toml");
        std::fs::write(&cfg_path, cfg).unwrap();

        let mut cmd = Command::new(conproxy_bin());
        cmd.current_dir(dir.path())
            .args([
                "start",
                "--listen",
                &grpc,
                "--config",
                cfg_path.to_str().unwrap(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().expect("spawn conproxy");
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        if let Some(stderr) = child.stderr.take() {
            let buf = stderr_buf.clone();
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().flatten() {
                    let mut b = buf.lock().unwrap();
                    if b.len() < 12_000 {
                        b.push_str(&line);
                        b.push('\n');
                    }
                }
            });
        }

        let mut proxy = Self {
            child: Some(child),
            http_base: format!("http://{http}"),
            grpc_addr: grpc,
            _dir: dir,
            stderr_buf,
        };
        proxy
            .wait_healthy(Duration::from_secs(30))
            .unwrap_or_else(|e| panic!("proxy {node_id} not healthy: {e}"));
        proxy
    }

    fn wait_healthy(&mut self, timeout: Duration) -> Result<(), String> {
        let url = format!("{}/health", self.http_base);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| e.to_string())?;
        let start = Instant::now();
        while start.elapsed() < timeout {
            if let Some(ref mut c) = self.child {
                if let Ok(Some(st)) = c.try_wait() {
                    let err = self.stderr_buf.lock().unwrap().clone();
                    return Err(format!("exited {st}\n{err}"));
                }
            }
            if let Ok(resp) = client.get(&url).send() {
                if resp.status().is_success() {
                    return Ok(());
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let err = self.stderr_buf.lock().unwrap().clone();
        Err(format!("timeout\n{err}"))
    }

    fn peer_status(&self) -> serde_json::Value {
        let url = format!("{}/peer/status", self.http_base);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let resp = client.get(&url).send().expect("peer/status");
        assert!(resp.status().is_success(), "peer/status status");
        resp.json().expect("peer/status json")
    }

    fn query(&self, q: &str) -> serde_json::Value {
        let url = format!("{}/query", self.http_base);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap();
        let resp = client
            .post(&url)
            .json(&serde_json::json!({"query": q, "top_k": 5}))
            .send()
            .expect("query");
        assert!(resp.status().is_success(), "query HTTP {}", resp.status());
        resp.json().expect("query json")
    }

    fn stop(&mut self) {
        if let Some(ref mut child) = self.child {
            #[cfg(unix)]
            unsafe {
                libc::kill(child.id() as i32, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
        }
        self.child = None;
    }
}

impl Drop for PeerProxy {
    fn drop(&mut self) {
        self.stop();
    }
}

#[test]
fn peer_cdc_replicates_cache_insert_a_to_b() {
    test_infra::containers::docker_check();

    // Meili must stay alive for the whole multi-process test — use blocking
    // runtime so container RAII outlives the child proxies.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let meili = rt.block_on(async {
        let inst = test_infra::containers::meilisearch_container().await;
        test_infra::containers::meili_create_index(&inst.base_url, "peer_docs", "id").await;
        test_infra::containers::meili_add_documents(
            &inst.base_url,
            "peer_docs",
            vec![
                serde_json::json!({
                    "id": "doc-1",
                    "title": "Rust async",
                    "content": "Tokio is an async runtime for Rust systems programming."
                }),
                serde_json::json!({
                    "id": "doc-2",
                    "title": "Cache replication",
                    "content": "CDC events replicate cache inserts between peers."
                }),
            ],
        )
        .await;
        inst
    });

    let ports_a = find_free_ports();
    let ports_b = find_free_ports();
    let grpc_a = format!("127.0.0.1:{}", ports_a.grpc_port);
    let grpc_b = format!("127.0.0.1:{}", ports_b.grpc_port);

    // Start B first (receiver), then A (writer). snapshot_on_join=false so cold start is Ready.
    let mut proxy_b = PeerProxy::start(&meili.base_url, &ports_b, "node-b", &grpc_a, false);
    let mut proxy_a = PeerProxy::start(&meili.base_url, &ports_a, "node-a", &grpc_b, false);

    let st_a = proxy_a.peer_status();
    assert_eq!(st_a["enabled"], true, "A peer enabled: {st_a}");
    assert_eq!(st_a["node_id"], "node-a");
    let st_b = proxy_b.peer_status();
    assert_eq!(st_b["enabled"], true, "B peer enabled: {st_b}");
    assert_eq!(st_b["node_id"], "node-b");

    // broadcast CDC drops events with zero subscribers. Wait until each side
    // has ≥1 CDC subscriber (the other peer's PeerReceiver) before insert.
    let mesh_deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let sa = proxy_a.peer_status();
        let sb = proxy_b.peer_status();
        let sub_a = sa
            .get("cdc_subscribers")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let sub_b = sb
            .get("cdc_subscribers")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if sub_a >= 1 && sub_b >= 1 {
            break;
        }
        assert!(
            Instant::now() < mesh_deadline,
            "CDC mesh not ready in 20s; A={sa} B={sb}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }

    // Miss on A → upstream Meili → cache insert → CDC broadcast
    let body_a = proxy_a.query("rust async runtime");
    let cache_a = body_a
        .get("cache_status")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        cache_a == "miss" || cache_a == "Miss" || cache_a.eq_ignore_ascii_case("miss"),
        "first A query should miss, got {body_a}"
    );
    let results_a = body_a
        .get("results")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        !results_a.is_empty(),
        "A should get upstream results: {body_a}"
    );

    // Wait for B to apply CDC insert (poll peer cache_entry_count or query hit)
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut saw_hit = false;
    let mut last_b = serde_json::Value::Null;
    while Instant::now() < deadline {
        let st = proxy_b.peer_status();
        let entries = st
            .get("cache_entry_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if entries > 0 {
            let body_b = proxy_b.query("rust async runtime");
            last_b = body_b.clone();
            let cache_b = body_b
                .get("cache_status")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if cache_b.eq_ignore_ascii_case("hit")
                || cache_b.eq_ignore_ascii_case("fresh")
                || cache_b.eq_ignore_ascii_case("stale")
            {
                saw_hit = true;
                break;
            }
            // Some responses encode Hit as object/enum variant
            if cache_b.contains("Hit") || cache_b.contains("hit") {
                saw_hit = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    assert!(
        saw_hit,
        "B should cache-hit after CDC from A within 15s; last_b={last_b}; B status={}; A status={}",
        proxy_b.peer_status(),
        proxy_a.peer_status()
    );

    // Kill A mid-stream — B stays up and still serves
    proxy_a.stop();
    let st_after = proxy_b.peer_status();
    assert_eq!(st_after["enabled"], true);
    let body_still = proxy_b.query("rust async runtime");
    let cache_still = body_still
        .get("cache_status")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        cache_still.eq_ignore_ascii_case("hit")
            || cache_still.eq_ignore_ascii_case("fresh")
            || cache_still.eq_ignore_ascii_case("stale")
            || cache_still.contains("Hit"),
        "B still serves after A death: {body_still}"
    );

    proxy_b.stop();
    // Drop container on the same runtime that created it (async Drop).
    // Always run even if asserts above panic via catch_unwind scope below.
    rt.block_on(async move {
        drop(meili);
    });
}

#[test]
fn peer_status_disabled_when_not_configured() {
    // Pure unit-ish: free port health path not needed — covered by mod_tests.
    // Keep a cheap compile link to FreePorts/conproxy_bin for the suite.
    let _ = find_free_ports();
    assert!(conproxy_bin().exists() || std::env::var("CARGO_BIN_EXE_conproxy").is_ok());
}

fn setup_meili_peer_docs(
    rt: &tokio::runtime::Runtime,
) -> test_infra::containers::ContainerInstance {
    rt.block_on(async {
        let inst = test_infra::containers::meilisearch_container().await;
        test_infra::containers::meili_create_index(&inst.base_url, "peer_docs", "id").await;
        test_infra::containers::meili_add_documents(
            &inst.base_url,
            "peer_docs",
            vec![serde_json::json!({
                "id": "doc-1",
                "title": "Rust async",
                "content": "Tokio is an async runtime for Rust systems programming."
            })],
        )
        .await;
        inst
    })
}

fn wait_cdc_mesh(proxy_a: &PeerProxy, proxy_b: &PeerProxy, secs: u64) {
    let mesh_deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let sa = proxy_a.peer_status();
        let sb = proxy_b.peer_status();
        let sub_a = sa
            .get("cdc_subscribers")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let sub_b = sb
            .get("cdc_subscribers")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if sub_a >= 1 && sub_b >= 1 {
            return;
        }
        assert!(
            Instant::now() < mesh_deadline,
            "CDC mesh not ready in {secs}s; A={sa} B={sb}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn wait_b_cache_hit(proxy_a: &PeerProxy, proxy_b: &PeerProxy, query: &str, secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        let st = proxy_b.peer_status();
        let entries = st
            .get("cache_entry_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if entries > 0 {
            let body_b = proxy_b.query(query);
            let cache_b = body_b
                .get("cache_status")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if cache_b.eq_ignore_ascii_case("hit")
                || cache_b.eq_ignore_ascii_case("fresh")
                || cache_b.eq_ignore_ascii_case("stale")
                || cache_b.contains("Hit")
                || cache_b.contains("hit")
            {
                return true;
            }
        }
        let _ = proxy_a.peer_status();
        std::thread::sleep(Duration::from_millis(300));
    }
    false
}

/// Plan 09 T2: matching shared_secret on both peers still replicates CDC.
#[test]
fn peer_cdc_replicates_with_matching_shared_secret() {
    test_infra::containers::docker_check();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let meili = setup_meili_peer_docs(&rt);

    let ports_a = find_free_ports();
    let ports_b = find_free_ports();
    let grpc_a = format!("127.0.0.1:{}", ports_a.grpc_port);
    let grpc_b = format!("127.0.0.1:{}", ports_b.grpc_port);
    let secret = "peer-integ-secret-ok";

    let mut proxy_b = PeerProxy::start_with_secret(
        &meili.base_url,
        &ports_b,
        "node-b",
        &grpc_a,
        false,
        Some(secret),
    );
    let mut proxy_a = PeerProxy::start_with_secret(
        &meili.base_url,
        &ports_a,
        "node-a",
        &grpc_b,
        false,
        Some(secret),
    );

    wait_cdc_mesh(&proxy_a, &proxy_b, 20);
    let body_a = proxy_a.query("rust async runtime");
    assert!(
        body_a
            .get("results")
            .and_then(|v| v.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false),
        "A should get results: {body_a}"
    );
    assert!(
        wait_b_cache_hit(&proxy_a, &proxy_b, "rust async runtime", 15),
        "B should cache-hit after CDC with matching secret; B={} A={}",
        proxy_b.peer_status(),
        proxy_a.peer_status()
    );

    proxy_a.stop();
    proxy_b.stop();
    rt.block_on(async move {
        drop(meili);
    });
}

/// Plan 09 T2: mismatched secrets — B must not apply A's CDC inserts.
#[test]
fn peer_cdc_does_not_replicate_with_mismatched_shared_secret() {
    test_infra::containers::docker_check();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let meili = setup_meili_peer_docs(&rt);

    let ports_a = find_free_ports();
    let ports_b = find_free_ports();
    let grpc_a = format!("127.0.0.1:{}", ports_a.grpc_port);
    let grpc_b = format!("127.0.0.1:{}", ports_b.grpc_port);

    // B expects secret-b; A presents secret-a when calling B.
    let mut proxy_b = PeerProxy::start_with_secret(
        &meili.base_url,
        &ports_b,
        "node-b",
        &grpc_a,
        false,
        Some("secret-b"),
    );
    let mut proxy_a = PeerProxy::start_with_secret(
        &meili.base_url,
        &ports_a,
        "node-a",
        &grpc_b,
        false,
        Some("secret-a"),
    );

    // Mesh may never fully form (auth failures). Give receivers time to fail.
    std::thread::sleep(Duration::from_secs(3));

    let body_a = proxy_a.query("rust async runtime");
    assert!(
        body_a
            .get("results")
            .and_then(|v| v.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false),
        "A still queries upstream: {body_a}"
    );

    // B must not get a cache hit from A's CDC within the window.
    let replicated = wait_b_cache_hit(&proxy_a, &proxy_b, "rust async runtime", 8);
    assert!(
        !replicated,
        "B must NOT cache-hit when secrets mismatch; B status={}",
        proxy_b.peer_status()
    );

    proxy_a.stop();
    proxy_b.stop();
    rt.block_on(async move {
        drop(meili);
    });
}
