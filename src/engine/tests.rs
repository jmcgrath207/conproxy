use super::*;
use crate::proxy::types::{CacheStatus, QueryResponse};

fn minimal_toml(dir: &std::path::Path) -> std::path::PathBuf {
    let p = dir.join("conproxy.toml");
    std::fs::write(
        &p,
        r#"
[proxy]
upstream_url = "http://127.0.0.1:9"
"#,
    )
    .unwrap();
    p
}

fn hit_response() -> QueryResponse {
    QueryResponse {
        results: vec![],
        cache_status: CacheStatus::Hit,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    }
}

#[tokio::test]
async fn engine_creates_global_context() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::builder()
        .config_toml(minimal_toml(dir.path()))
        .build()
        .unwrap();
    assert_eq!(engine.context(), "global");
    assert!(engine.context_exists("global"));
}

#[tokio::test]
async fn engine_does_not_start_peer() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::builder()
        .config_toml(minimal_toml(dir.path()))
        .build()
        .unwrap();
    assert!(!engine.peer_started());
}

#[test]
fn engine_missing_config_errors() {
    let result = Engine::builder()
        .config_toml("/no/such/conproxy.toml")
        .build();
    assert!(matches!(result, Err(EngineError::Config(_))));
}

#[tokio::test]
async fn query_miss_then_hit_via_insert() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::builder()
        .config_toml(minimal_toml(dir.path()))
        .build()
        .unwrap();
    let q = "how does X work";
    engine.cache().insert_with_context(
        &format!("ctx:global:{q}"),
        hit_response(),
        "test".into(),
        "global",
    );
    let out = engine.query(q, QueryOpts::default()).await.unwrap();
    assert_eq!(out.cache_status, CacheStatus::Hit);
}

#[tokio::test]
async fn global_context_isolated_from_default() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::builder()
        .config_toml(minimal_toml(dir.path()))
        .build()
        .unwrap();
    let q = "isolated query";
    engine.cache().insert_with_context(
        &format!("ctx:default:{q}"),
        hit_response(),
        "test".into(),
        "default",
    );
    let err = engine.query(q, QueryOpts::default()).await.unwrap_err();
    assert!(matches!(err, EngineError::Query(_)));
}

#[tokio::test]
async fn skip_cache_bypasses_hit() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::builder()
        .config_toml(minimal_toml(dir.path()))
        .build()
        .unwrap();
    let q = "skip me";
    engine.cache().insert_with_context(
        &format!("ctx:global:{q}"),
        hit_response(),
        "test".into(),
        "global",
    );
    let out = engine
        .query(
            q,
            QueryOpts {
                skip_cache: true,
                ..QueryOpts::default()
            },
        )
        .await
        .unwrap();
    assert_ne!(out.cache_status, CacheStatus::Hit);
    assert_eq!(out.cache_status, CacheStatus::Frozen);
}

#[tokio::test]
async fn clear_wipes_store() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::builder()
        .config_toml(minimal_toml(dir.path()))
        .build()
        .unwrap();
    let q = "wipe me";
    engine.cache().insert_with_context(
        &format!("ctx:global:{q}"),
        hit_response(),
        "test".into(),
        "global",
    );
    engine.clear();
    let err = engine.query(q, QueryOpts::default()).await.unwrap_err();
    assert!(matches!(err, EngineError::Query(_)));
}

#[tokio::test]
async fn stats_counters_move() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::builder()
        .config_toml(minimal_toml(dir.path()))
        .build()
        .unwrap();
    let q = "stats query";
    let _ = engine.query(q, QueryOpts::default()).await;
    engine.cache().insert_with_context(
        &format!("ctx:global:{q}"),
        hit_response(),
        "test".into(),
        "global",
    );
    let _ = engine.query(q, QueryOpts::default()).await.unwrap();
    let s = engine.stats();
    assert!(s.hits >= 1);
    assert!(s.size >= 1);
}

#[test]
fn max_memory_parse_mib() {
    assert_eq!(parse_memory("256MiB").unwrap(), 256 * 1024 * 1024);
}

#[test]
fn invalid_max_memory_errors() {
    assert!(parse_memory("nope").is_err());
}

#[test]
fn max_memory_fixed_sets_cap() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::builder()
        .config_toml(minimal_toml(dir.path()))
        .max_memory(Some(4096))
        .build()
        .unwrap();
    assert_eq!(engine.cache().max_memory_bytes(), 4096);
}

#[test]
fn dynamic_cap_default_is_70_percent() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::builder()
        .config_toml(minimal_toml(dir.path()))
        .build()
        .unwrap();
    let expected = cap_for_fraction(DEFAULT_MEMORY_FRACTION, read_memory_budget())
        .max(DYNAMIC_CAP_FLOOR_BYTES) as usize;
    assert_eq!(engine.cache().max_memory_bytes(), expected);
}

#[test]
fn memory_fraction_overrides_default() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::builder()
        .config_toml(minimal_toml(dir.path()))
        .memory_fraction(0.5)
        .build()
        .unwrap();
    let expected =
        cap_for_fraction(0.5, read_memory_budget()).max(DYNAMIC_CAP_FLOOR_BYTES) as usize;
    assert_eq!(engine.cache().max_memory_bytes(), expected);
}

#[test]
fn max_memory_beats_memory_fraction() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::builder()
        .config_toml(minimal_toml(dir.path()))
        .max_memory(Some(2048))
        .memory_fraction(0.5)
        .build()
        .unwrap();
    assert_eq!(engine.cache().max_memory_bytes(), 2048);
}

#[test]
fn shrink_cap_evicts() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::builder()
        .config_toml(minimal_toml(dir.path()))
        .build()
        .unwrap();
    for i in 0..10 {
        let q = format!("shrink query {i}");
        engine.cache().insert_with_context(
            &format!("ctx:global:{q}"),
            hit_response(),
            "test".into(),
            "global",
        );
    }
    assert!(engine.cache().max_memory_bytes() > 0);
    engine.cache().set_max_memory_bytes(1);
    let evicted = engine.cache().enforce_memory_limit();
    assert!(evicted >= 1);
    assert_eq!(engine.cache().stats().total, 0);
}

#[test]
fn dynamic_cap_never_zero_with_tiny_budget() {
    // The floor guarantees a non-zero cap even when the budget would give
    // a cap below the floor.
    let cap = cap_for_fraction(0.7, 1).max(DYNAMIC_CAP_FLOOR_BYTES);
    assert_eq!(cap, DYNAMIC_CAP_FLOOR_BYTES);
}

#[cfg(feature = "persistence")]
#[test]
fn persist_second_opener_errors() {
    let dir = tempfile::tempdir().unwrap();
    let toml = minimal_toml(dir.path());
    let db = dir.path().join("cache.redb");
    let _a = Engine::builder()
        .config_toml(&toml)
        .persist_path(&db)
        .build()
        .unwrap();
    let result = Engine::builder()
        .config_toml(&toml)
        .persist_path(&db)
        .build();
    assert!(matches!(result, Err(EngineError::Persist(_))));
}

#[cfg(not(feature = "persistence"))]
#[test]
fn persist_without_feature_errors() {
    let dir = tempfile::tempdir().unwrap();
    let result = Engine::builder()
        .config_toml(minimal_toml(dir.path()))
        .persist_path(dir.path().join("cache.redb"))
        .build();
    assert!(matches!(result, Err(EngineError::Feature("persistence"))));
}

#[tokio::test]
async fn close_rejects_query() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::builder()
        .config_toml(minimal_toml(dir.path()))
        .build()
        .unwrap();
    engine.close();
    let err = engine.query("x", QueryOpts::default()).await.unwrap_err();
    assert!(matches!(err, EngineError::Closed));
}

async fn http_raw(addr: SocketAddr, method: &str, path: &str) -> String {
    http_raw_fallible(addr, method, path).await.unwrap()
}

async fn http_raw_fallible(addr: SocketAddr, method: &str, path: &str) -> std::io::Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await?;
    let req = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await?;
    let mut buf = String::new();
    let _ = stream.read_to_string(&mut buf).await;
    Ok(buf)
}

#[test]
fn dashboard_off_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::builder()
        .config_toml(minimal_toml(dir.path()))
        .build()
        .unwrap();
    assert!(!engine.dashboard_configured());
    assert!(engine.dashboard_addr().is_none());
}

#[tokio::test]
async fn dashboard_serves_read_endpoints() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::builder()
        .config_toml(minimal_toml(dir.path()))
        .dashboard_listen("127.0.0.1:0")
        .build()
        .unwrap();
    assert!(engine.dashboard_configured());
    let addr = engine.dashboard_addr().unwrap();
    assert_eq!(addr.ip().to_string(), "127.0.0.1");

    let health = http_raw(addr, "GET", "/health").await;
    assert!(health.starts_with("HTTP/1.1 200"), "health: {health}");
    let ui = http_raw(addr, "GET", "/dashboard").await;
    assert!(ui.starts_with("HTTP/1.1 200"), "dashboard: {ui}");
    let stats = http_raw(addr, "GET", "/stats").await;
    assert!(stats.starts_with("HTTP/1.1 200"), "stats: {stats}");
    let contexts = http_raw(addr, "GET", "/contexts").await;
    assert!(contexts.starts_with("HTTP/1.1 200"), "contexts: {contexts}");
}

#[tokio::test]
async fn dashboard_rejects_write_routes() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::builder()
        .config_toml(minimal_toml(dir.path()))
        .dashboard_listen("127.0.0.1:0")
        .build()
        .unwrap();
    let addr = engine.dashboard_addr().unwrap();

    // Every mutating route the daemon serves must be absent from the
    // read-only dashboard router. Mirror the daemon's REST surface so a new
    // write endpoint cannot silently leak onto the dashboard.
    let mutating: &[(&str, &str)] = &[
        ("POST", "/query"),
        ("POST", "/batch"),
        ("POST", "/federated"),
        ("POST", "/cache/clear"),
        ("POST", "/cache/warmup"),
        ("POST", "/cache/evict"),
        ("POST", "/admin/reload"),
        ("POST", "/admin/pause"),
        ("POST", "/admin/resume"),
        ("POST", "/admin/agents"),
        ("DELETE", "/admin/agents/x"),
        ("POST", "/admin/agents/x/rotate-key"),
        ("POST", "/admin/metrics/reset"),
        ("POST", "/contexts/switch"),
        ("POST", "/contexts/create"),
    ];
    for (method, path) in mutating {
        let resp = http_raw(addr, method, path).await;
        assert!(
            resp.contains("404 Not Found"),
            "{method} {path} should be absent: {resp}"
        );
    }
}

#[tokio::test]
async fn dashboard_serves_every_spa_fetch_path() {
    // The SPA (`ui/app.js`) and the dashboard router live in different files.
    // Parse the SPA for its fetch targets and assert the router serves each
    // one, so a path the SPA needs that the router drops fails here instead
    // of surfacing as a broken dashboard panel.
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let app_js = std::fs::read_to_string(manifest.join("ui/app.js"))
        .expect("ui/app.js should exist at the repo root");
    let mut paths: Vec<String> = Vec::new();
    for line in app_js.lines() {
        for (left, right) in [
            ("fetchJSON('", "')"),
            ("fetchText('", "')"),
            ("fetch(BASE + '", "'"),
            ("fetchJSON(\"", "\")"),
            ("fetchText(\"", "\")"),
            ("fetch(BASE + \"", "\""),
        ] {
            let mut rest = line;
            while let Some(start) = rest.find(left) {
                let tail = &rest[start + left.len()..];
                let Some(end) = tail.find(right) else {
                    break;
                };
                let lit = &tail[..end];
                if !paths.contains(&lit.to_string()) {
                    paths.push(lit.to_string());
                }
                rest = &tail[end + right.len()..];
            }
        }
    }
    let expected: &[&str] = &[
        "metrics",
        "stats",
        "circuit",
        "pool",
        "cache/integrity",
        "queue",
        "stats/queries",
        "contexts/current",
        "contexts",
        "debug/tokio",
        "peer/status",
        "health",
    ];
    let mut missing: Vec<&str> = expected
        .iter()
        .filter(|e| !paths.contains(&e.to_string()))
        .copied()
        .collect();
    let mut extra: Vec<String> = paths
        .iter()
        .filter(|p| !expected.contains(&p.as_str()))
        .cloned()
        .collect();
    missing.sort_unstable();
    extra.sort();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "SPA fetch targets drifted — missing: {missing:?}, extra: {extra:?}"
    );

    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::builder()
        .config_toml(minimal_toml(dir.path()))
        .dashboard_listen("127.0.0.1:0")
        .build()
        .unwrap();
    let addr = engine.dashboard_addr().unwrap();
    for p in &paths {
        let path = if p.starts_with('/') {
            p.clone()
        } else {
            format!("/{p}")
        };
        let resp = http_raw(addr, "GET", &path).await;
        assert!(
            resp.starts_with("HTTP/1.1 200"),
            "SPA fetch path {path} not served: {resp}"
        );
    }
}

#[tokio::test]
async fn close_stops_dashboard() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::builder()
        .config_toml(minimal_toml(dir.path()))
        .dashboard_listen("127.0.0.1:0")
        .build()
        .unwrap();
    let addr = engine.dashboard_addr().unwrap();
    engine.close();

    // Graceful shutdown stops accepting; the kernel may still complete the
    // TCP handshake via the backlog, so the real signal is "no HTTP response".
    let mut still_serving = true;
    for _ in 0..100 {
        let out = tokio::time::timeout(
            Duration::from_millis(100),
            http_raw_fallible(addr, "GET", "/health"),
        )
        .await;
        match out {
            Ok(Ok(resp)) if resp.starts_with("HTTP/1.1 200") => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            _ => {
                still_serving = false;
                break;
            }
        }
    }
    assert!(!still_serving, "dashboard still serving after close()");
}

#[test]
fn dashboard_listen_conflict_errors() {
    let dir = tempfile::tempdir().unwrap();
    let first = Engine::builder()
        .config_toml(minimal_toml(dir.path()))
        .dashboard_listen("127.0.0.1:0")
        .build()
        .unwrap();
    let addr = first.dashboard_addr().unwrap();
    let result = Engine::builder()
        .config_toml(minimal_toml(dir.path()))
        .dashboard_listen(addr.to_string())
        .build();
    assert!(matches!(result, Err(EngineError::Config(_))));
}
