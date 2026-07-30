use conproxy::config::Config;

pub(crate) fn run(command: super::ProxyCommands) -> anyhow::Result<()> {
    use conproxy::proxy::{lifecycle, CacheProxy};
    use tokio_util::sync::CancellationToken;

    match command {
        super::ProxyCommands::Start {
            listen,
            upstream,
            daemon: run_daemon,
            node_id,
            peers,
            config: config_path,
        } => {
            let config = match config_path {
                Some(ref p) => Config::load_from(p)?,
                None => match std::env::var("CONPROXY_CONFIG") {
                    Ok(p) if !p.is_empty() => Config::load_from(&p)?,
                    _ => Config::load()?,
                },
            };
            // Resolve the effective config path for reload-source tracking so
            // `/admin/reload` (and ConfigMap edits in k8s) re-read from the
            // same source rather than falling back to the user-default search
            // path (~/.conproxy/conproxy.toml).
            let effective_config_path: Option<std::path::PathBuf> = config_path
                .as_deref()
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    std::env::var("CONPROXY_CONFIG")
                        .ok()
                        .filter(|p| !p.is_empty())
                        .map(std::path::PathBuf::from)
                });
            // Plan 10: project context-rooted → ProxyConfig (or legacy [proxy])
            let mut proxy_config = config
                .config
                .effective_proxy()
                .map_err(|e| anyhow::anyhow!("config project: {e}"))?;

            // CLI args override config
            if let Some(ref addr) = listen {
                proxy_config.listen = Some(addr.clone());
            }
            if let Some(ref url) = upstream {
                proxy_config.upstream_url = Some(url.clone());
            }
            if let Some(ref nid) = node_id {
                proxy_config.peer.node_id = Some(nid.clone());
            }
            if let Some(ref p) = peers {
                proxy_config.peer.peers = p.split(',').map(|s| s.trim().to_string()).collect();
                proxy_config.peer.enabled = Some(true);
            }

            // Normalize legacy upstream_url → upstreams array
            proxy_config.normalize_upstreams();

            let listen_addr = proxy_config.listen().to_string();
            let http_listen_addr = proxy_config.http_listen_addr().to_string();

            if run_daemon {
                // Check if already running
                if lifecycle::is_proxy_running() {
                    println!("Proxy is already running.");
                    return Ok(());
                }

                // Spawn as a detached background process
                let exe = std::env::current_exe()?;
                let mut cmd = std::process::Command::new(exe);
                cmd.arg("start");

                if let Some(ref addr) = listen {
                    cmd.arg("--listen").arg(addr);
                }
                if let Some(ref url) = upstream {
                    cmd.arg("--upstream").arg(url);
                }

                // Redirect stderr to daemon.log for debugging child startup failures
                let daemon_log = config
                    .local_root
                    .as_deref()
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .join("daemon.log");
                let daemon_log_file = std::fs::File::create(&daemon_log).map_err(|e| {
                    anyhow::anyhow!("Failed to create {}: {}", daemon_log.display(), e)
                })?;
                cmd.stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(daemon_log_file);

                let child = cmd.spawn()?;
                lifecycle::write_pid_file(child.id());

                println!("Started proxy in background (PID: {})", child.id());
                println!("  Listen: {}", listen_addr);
                if let Some(url) = proxy_config.upstream_url() {
                    println!("  Upstream: {}", url);
                }
            } else {
                // Run in foreground
                let cancel = CancellationToken::new();

                // Initialize tracing subscriber for proxy logging.
                // tokio-console: registry with console layer + fmt layer (single subscriber).
                // Without tokio-console: plain fmt subscriber (unchanged behavior).
                // `console_subscriber::spawn()` returns a layer and spawns the
                // console server internally (no separate Server handle needed).
                #[cfg(feature = "tokio-console")]
                {
                    use tracing_subscriber::prelude::*;
                    use tracing_subscriber::EnvFilter;
                    let env_filter = EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| EnvFilter::new("info"));
                    tracing_subscriber::registry()
                        .with(console_subscriber::spawn())
                        .with(
                            tracing_subscriber::fmt::layer()
                                .with_target(true)
                                .with_writer(std::io::stderr)
                                .with_filter(env_filter),
                        )
                        .init();
                }
                #[cfg(not(feature = "tokio-console"))]
                {
                    tracing_subscriber::fmt()
                        .with_env_filter(
                            tracing_subscriber::EnvFilter::try_from_default_env()
                                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
                        )
                        .with_target(true)
                        .with_writer(std::io::stderr)
                        .init();
                }

                lifecycle::write_pid_file(std::process::id());

                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()?;

                rt.block_on(async {
                    // Set up Ctrl+C handler inside the runtime context
                    tokio::spawn(wait_for_shutdown(cancel.clone()));

                    let resolved = config.config.resolve_contexts().unwrap_or_default();
                    let proxy = CacheProxy::new(&proxy_config)?
                        .with_resolved_contexts(&resolved)
                        .with_reload_source(effective_config_path);

                    // Configure embedder provider from [embedding] config
                    #[cfg(feature = "embed-api")]
                    let proxy = proxy.with_embedding_config(&config.config.embedding);

                    // Wire in semantic cache tier from [proxy.cache.semantic]
                    #[cfg(feature = "embed-api")]
                    let proxy = proxy.with_semantic_cache(&proxy_config.cache.semantic);

                    proxy.run(&listen_addr, &http_listen_addr, cancel).await
                })?;

                lifecycle::remove_pid_file();
            }
        }

        super::ProxyCommands::Stop => {
            let config = Config::load()?;
            let http_listen_addr = config.config.proxy.http_listen_addr().to_string();

            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;

            rt.block_on(async { lifecycle::stop_proxy(&http_listen_addr).await })?;

            println!("Proxy stopped.");
        }

        super::ProxyCommands::Status { json } => {
            let config = Config::load()?;
            let listen_addr = config.config.proxy.listen().to_string();
            let http_listen_addr = config.config.proxy.http_listen_addr().to_string();

            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;

            let status = rt.block_on(async { lifecycle::proxy_status(&http_listen_addr).await })?;

            // Fetch context info via gRPC if proxy is running
            let context_info: Option<(String, usize)> = if status.running {
                rt.block_on(async {
                    use conproxy::proxy::grpc::proto;
                    let grpc_url = format!("http://{}", listen_addr);
                    let channel = tonic::transport::Endpoint::from_shared(grpc_url)
                        .ok()?
                        .connect()
                        .await
                        .ok()?;
                    let mut client =
                        proto::context_service_client::ContextServiceClient::new(channel);
                    let resp = client
                        .get_current_context(proto::GetCurrentContextRequest {})
                        .await
                        .ok()?;
                    let current = resp.into_inner().id;
                    let list_resp = client
                        .list_contexts(proto::ListContextsRequest {})
                        .await
                        .ok()?;
                    let count = list_resp.into_inner().contexts.len();
                    Some((current, count))
                })
            } else {
                None
            };

            if json {
                let mut output = serde_json::json!({
                    "running": status.running,
                    "pid": status.pid,
                    "health": status.health,
                });
                if let Some((current, count)) = &context_info {
                    if let Some(obj) = output.as_object_mut() {
                        obj.insert("context".to_string(), serde_json::json!(current));
                        obj.insert("contexts_count".to_string(), serde_json::json!(count));
                    }
                }
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else if status.running {
                println!("Proxy is running.");
                if let Some(pid) = status.pid {
                    println!("  PID: {}", pid);
                }
                if let Some(health) = status.health {
                    if let Some(uptime) = health.get("uptime_secs") {
                        println!("  Uptime: {}s", uptime);
                    }
                    if let Some(entries) = health.get("cache_entries") {
                        println!("  Cache entries: {}", entries);
                    }
                    if let Some(upstream) = health.get("upstream_configured") {
                        println!("  Upstream configured: {}", upstream);
                    }
                    if let Some(healthy) = health.get("upstream_healthy") {
                        println!("  Upstream healthy: {}", healthy);
                    }
                }
                if let Some((current, count)) = &context_info {
                    println!("  Context: {}", current);
                    println!("  Contexts available: {}", count);
                }
            } else {
                println!("Proxy is not running.");
            }
        }

        super::ProxyCommands::Contexts { json } => {
            let config = Config::load()?;
            let listen_addr = config.config.proxy.listen().to_string();

            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;

            rt.block_on(async {
                use conproxy::proxy::grpc::proto;

                let grpc_url = format!("http://{}", listen_addr);
                let channel = match tonic::transport::Endpoint::from_shared(grpc_url) {
                    Ok(ep) => match ep
                        .timeout(std::time::Duration::from_secs(5))
                        .connect()
                        .await
                    {
                        Ok(ch) => ch,
                        Err(_) => {
                            if json {
                                println!(r#"{{"error": "Proxy is not running"}}"#);
                            } else {
                                println!("Proxy is not running.");
                                println!("Start with: conproxy start");
                            }
                            return Ok(());
                        }
                    },
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        return Ok(());
                    }
                };

                let mut client = proto::context_service_client::ContextServiceClient::new(channel);

                let current_resp = client
                    .get_current_context(proto::GetCurrentContextRequest {})
                    .await;
                let current = current_resp
                    .map(|r| r.into_inner().id)
                    .unwrap_or_else(|_| "default".to_string());

                let list_resp = client.list_contexts(proto::ListContextsRequest {}).await;

                match list_resp {
                    Ok(resp) => {
                        let inner = resp.into_inner();
                        if json {
                            let data = serde_json::json!({
                                "current": current,
                                "contexts": inner.contexts.iter().map(|c| {
                                    serde_json::json!({
                                        "id": c.id,
                                    })
                                }).collect::<Vec<_>>(),
                            });
                            println!("{}", serde_json::to_string_pretty(&data)?);
                        } else {
                            println!("Current context: {}", current);
                            println!();
                            println!("Available contexts ({}):", inner.contexts.len());
                            for ctx in &inner.contexts {
                                let marker = if ctx.id == current { " *" } else { "" };
                                println!("  {}{}", ctx.id, marker);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Error: {}", e.message());
                    }
                }

                Ok::<(), anyhow::Error>(())
            })?;
        }

        super::ProxyCommands::Context {
            id,
            switch,
            create,
            upstream: _upstream,
            collection: _collection,
            json: json_output,
        } => {
            let config = Config::load()?;
            let listen_addr = config.config.proxy.listen().to_string();

            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;

            rt.block_on(async {
                use conproxy::proxy::grpc::proto;

                let grpc_url = format!("http://{}", listen_addr);
                let channel = match tonic::transport::Endpoint::from_shared(grpc_url) {
                    Ok(ep) => match ep
                        .timeout(std::time::Duration::from_secs(5))
                        .connect()
                        .await
                    {
                        Ok(ch) => ch,
                        Err(_) => {
                            if json_output {
                                println!(r#"{{"error": "Proxy is not running"}}"#);
                            } else {
                                eprintln!("Error: Proxy is not running");
                            }
                            return Ok(());
                        }
                    },
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        return Ok(());
                    }
                };

                let mut client = proto::context_service_client::ContextServiceClient::new(channel);

                if create {
                    match client
                        .create_context(proto::CreateContextRequest {
                            context_id: id.clone(),
                        })
                        .await
                    {
                        Ok(resp) => {
                            let inner = resp.into_inner();
                            if !switch {
                                if json_output {
                                    println!(
                                        "{}",
                                        serde_json::to_string_pretty(&serde_json::json!({
                                            "success": true,
                                            "context_id": inner.context_id,
                                            "message": inner.message,
                                        }))?
                                    );
                                } else {
                                    println!("Created context: {}", inner.context_id);
                                }
                            }
                        }
                        Err(e) => {
                            // Already exists is fine when also switching
                            if !switch {
                                if json_output {
                                    println!(
                                        "{}",
                                        serde_json::to_string_pretty(&serde_json::json!({
                                            "success": false,
                                            "error": e.message(),
                                        }))?
                                    );
                                } else {
                                    eprintln!("Error: {}", e.message());
                                }
                                return Ok(());
                            }
                        }
                    }
                }
                if switch {
                    match client
                        .switch_context(proto::SwitchContextRequest {
                            context_id: id.clone(),
                        })
                        .await
                    {
                        Ok(resp) => {
                            let inner = resp.into_inner();
                            if json_output {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&serde_json::json!({
                                        "success": true,
                                        "previous": inner.previous,
                                        "current": inner.current,
                                    }))?
                                );
                            } else {
                                println!("Switched to context: {}", inner.current);
                            }
                        }
                        Err(e) => {
                            if json_output {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&serde_json::json!({
                                        "success": false,
                                        "error": e.message(),
                                    }))?
                                );
                            } else {
                                eprintln!("Error: {}", e.message());
                            }
                        }
                    }
                } else if !create {
                    // Show context details via get_context_stats
                    match client
                        .get_context_stats(proto::GetContextStatsRequest {
                            context_id: id.clone(),
                        })
                        .await
                    {
                        Ok(resp) => {
                            let inner = resp.into_inner();
                            if json_output {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&serde_json::json!({
                                        "id": id,
                                        "cache_entries": inner.cache_entries,
                                        "hits": inner.hits,
                                        "misses": inner.misses,
                                        "hit_rate": inner.hit_rate,
                                    }))?
                                );
                            } else {
                                println!("Context: {}", id);
                                println!("  Cache entries: {}", inner.cache_entries);
                                println!("  Hits: {}", inner.hits);
                                println!("  Misses: {}", inner.misses);

                                // Check if this is the current context
                                if let Ok(current_resp) = client
                                    .get_current_context(proto::GetCurrentContextRequest {})
                                    .await
                                {
                                    if current_resp.into_inner().id == id {
                                        println!("  Status: active (current)");
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            if json_output {
                                println!(r#"{{"error": "Context not found"}}"#);
                            } else {
                                eprintln!("Context not found: {} ({})", id, e.message());
                                println!("Create with: conproxy context {} --create", id);
                            }
                        }
                    }
                }
                Ok::<(), anyhow::Error>(())
            })?;
        }

        super::ProxyCommands::Install {
            listen,
            upstream,
            start,
        } => {
            let config = Config::load()?;
            let listen_addr = listen
                .as_deref()
                .unwrap_or_else(|| config.config.proxy.listen());
            let upstream_url = upstream
                .as_ref()
                .or(config.config.proxy.upstream_url.as_ref());

            // Determine the service manager based on OS
            #[cfg(target_os = "linux")]
            {
                println!("Installing systemd service...");

                let exe = std::env::current_exe()?;
                let service_content = format!(
                    r#"[Unit]
Description=Conproxy Cache Proxy
After=network.target

[Service]
Type=simple
ExecStart={} proxy start --listen {}{}
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
"#,
                    exe.display(),
                    listen_addr,
                    upstream_url
                        .map(|u| format!(" --upstream {}", u))
                        .unwrap_or_default()
                );

                let service_path = std::path::Path::new("/etc/systemd/system/conproxy.service");
                if service_path.exists() {
                    println!("Service already installed at {}", service_path.display());
                    println!("Use `conproxy uninstall` first to reinstall.");
                } else {
                    println!(
                        "Service file would be created at: {}",
                        service_path.display()
                    );
                    println!();
                    println!("Run the following commands as root:");
                    println!("  sudo tee {} << 'EOF'", service_path.display());
                    println!("{}", service_content);
                    println!("EOF");
                    println!("  sudo systemctl daemon-reload");
                    println!("  sudo systemctl enable conproxy");
                    if start {
                        println!("  sudo systemctl start conproxy");
                    }
                }
            }

            #[cfg(target_os = "macos")]
            {
                println!("Installing launchd service...");

                let exe = std::env::current_exe()?;
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
                let plist_path = format!("{}/Library/LaunchAgents/com.conproxy.plist", home);

                let mut args = vec![
                    format!("<string>{}</string>", exe.display()),
                    "<string>proxy</string>".to_string(),
                    "<string>start</string>".to_string(),
                    format!("<string>--listen</string>"),
                    format!("<string>{}</string>", listen_addr),
                ];
                if let Some(url) = upstream_url {
                    args.push("<string>--upstream</string>".to_string());
                    args.push(format!("<string>{}</string>", url));
                }

                let plist_content = format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.conproxy</string>
    <key>ProgramArguments</key>
    <array>
        {}
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{}/Library/Logs/conproxy.log</string>
    <key>StandardErrorPath</key>
    <string>{}/Library/Logs/conproxy.error.log</string>
</dict>
</plist>
"#,
                    args.join("\n        "),
                    home,
                    home
                );

                if std::path::Path::new(&plist_path).exists() {
                    println!("Service already installed at {}", plist_path);
                    println!("Use `conproxy uninstall` first to reinstall.");
                } else {
                    std::fs::write(&plist_path, &plist_content)?;
                    println!("Created launchd plist: {}", plist_path);
                    if start {
                        let status = std::process::Command::new("launchctl")
                            .args(["load", &plist_path])
                            .status()?;
                        if status.success() {
                            println!("Service started.");
                        } else {
                            println!(
                                "Failed to start service. Run: launchctl load {}",
                                plist_path
                            );
                        }
                    } else {
                        println!("To start: launchctl load {}", plist_path);
                    }
                }
            }

            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            {
                println!("Service installation not supported on this platform.");
                println!(
                    "Run manually: conproxy start --listen {} --daemon",
                    listen_addr
                );
            }
        }

        super::ProxyCommands::Uninstall { purge } => {
            #[cfg(target_os = "linux")]
            {
                let service_path = "/etc/systemd/system/conproxy.service";
                if std::path::Path::new(service_path).exists() {
                    println!("To uninstall, run these commands as root:");
                    println!("  sudo systemctl stop conproxy");
                    println!("  sudo systemctl disable conproxy");
                    if purge {
                        println!("  sudo rm {}", service_path);
                        println!("  sudo systemctl daemon-reload");
                    }
                } else {
                    println!("Service not installed.");
                }
            }

            #[cfg(target_os = "macos")]
            {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
                let plist_path = format!("{}/Library/LaunchAgents/com.conproxy.plist", home);

                if std::path::Path::new(&plist_path).exists() {
                    // Unload the service first
                    let _ = std::process::Command::new("launchctl")
                        .args(["unload", &plist_path])
                        .status();
                    println!("Service unloaded.");

                    if purge {
                        std::fs::remove_file(&plist_path)?;
                        println!("Removed: {}", plist_path);
                    } else {
                        println!("Service disabled. Use --purge to remove configuration.");
                    }
                } else {
                    println!("Service not installed.");
                }
            }

            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            {
                println!("Service management not supported on this platform.");
            }
        }

        super::ProxyCommands::Logs { lines, follow } => {
            #[cfg(target_os = "linux")]
            {
                let mut cmd = std::process::Command::new("journalctl");
                cmd.args(["-u", "conproxy", "-n", &lines.to_string()]);
                if follow {
                    cmd.arg("-f");
                }
                let status = cmd.status()?;
                if !status.success() {
                    println!("Could not read logs. Is the service installed?");
                    println!("Try: sudo journalctl -u conproxy -n {}", lines);
                }
            }

            #[cfg(target_os = "macos")]
            {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
                let log_path = format!("{}/Library/Logs/conproxy.log", home);

                if std::path::Path::new(&log_path).exists() {
                    if follow {
                        let status = std::process::Command::new("tail")
                            .args(["-n", &lines.to_string(), "-f", &log_path])
                            .status()?;
                        if !status.success() {
                            eprintln!("Error reading logs");
                        }
                    } else {
                        let status = std::process::Command::new("tail")
                            .args(["-n", &lines.to_string(), &log_path])
                            .status()?;
                        if !status.success() {
                            eprintln!("Error reading logs");
                        }
                    }
                } else {
                    println!("Log file not found: {}", log_path);
                    println!("Is the service installed and running?");
                }
            }

            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            {
                println!("Log viewing not supported on this platform.");
            }
        }

        super::ProxyCommands::Peer { json } => {
            let config = Config::load()?;
            let http_listen_addr = config.config.proxy.http_listen_addr().to_string();

            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;

            rt.block_on(async {
                let url = format!("http://{}/peer/status", http_listen_addr);
                match reqwest::get(&url).await {
                    Ok(resp) => {
                        let body: serde_json::Value = resp.json().await.unwrap_or_default();
                        if json {
                            println!("{}", serde_json::to_string_pretty(&body)?);
                        } else {
                            let enabled = body
                                .get("enabled")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            if enabled {
                                println!("Peer replication: enabled");
                                if let Some(nid) = body.get("node_id").and_then(|v| v.as_str()) {
                                    println!("  Node ID: {}", nid);
                                }
                                if let Some(state) = body.get("state").and_then(|v| v.as_str()) {
                                    println!("  State: {}", state);
                                }
                                if let Some(count) =
                                    body.get("cache_entry_count").and_then(|v| v.as_u64())
                                {
                                    println!("  Cache entries: {}", count);
                                }
                                if let Some(subs) =
                                    body.get("cdc_subscribers").and_then(|v| v.as_u64())
                                {
                                    println!("  CDC subscribers: {}", subs);
                                }
                                if let Some(seq) = body.get("cdc_sequence").and_then(|v| v.as_u64())
                                {
                                    println!("  CDC sequence: {}", seq);
                                }
                            } else {
                                println!("Peer replication: disabled");
                            }
                        }
                    }
                    Err(_) => {
                        if json {
                            println!(r#"{{"error": "Proxy is not running"}}"#);
                        } else {
                            println!("Proxy is not running.");
                            println!("Start with: conproxy start");
                        }
                    }
                }
                Ok::<(), anyhow::Error>(())
            })?;
        }

        super::ProxyCommands::Cdc { json } => {
            let config = Config::load()?;
            let http_listen_addr = config.config.proxy.http_listen_addr().to_string();

            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;

            rt.block_on(async {
                let url = format!("http://{}/peer/status", http_listen_addr);
                match reqwest::get(&url).await {
                    Ok(resp) => {
                        let body: serde_json::Value = resp.json().await.unwrap_or_default();
                        if json {
                            let cdc_info = serde_json::json!({
                                "enabled": body.get("cdc_sequence").is_some(),
                                "sequence": body.get("cdc_sequence"),
                                "subscribers": body.get("cdc_subscribers"),
                            });
                            println!("{}", serde_json::to_string_pretty(&cdc_info)?);
                        } else if let Some(seq) = body.get("cdc_sequence").and_then(|v| v.as_u64())
                        {
                            println!("CDC event stream: enabled");
                            println!("  Sequence: {}", seq);
                            if let Some(subs) = body.get("cdc_subscribers").and_then(|v| v.as_u64())
                            {
                                println!("  Subscribers: {}", subs);
                            }
                        } else {
                            println!("CDC event stream: disabled");
                        }
                    }
                    Err(_) => {
                        if json {
                            println!(r#"{{"error": "Proxy is not running"}}"#);
                        } else {
                            println!("Proxy is not running.");
                        }
                    }
                }
                Ok::<(), anyhow::Error>(())
            })?;
        }

        super::ProxyCommands::Distill {
            context,
            tier,
            limit,
            include_stale,
            output_dir,
            cat,
            post_process,
        } => {
            run_distill(
                context,
                tier,
                limit,
                include_stale,
                output_dir,
                cat,
                post_process,
            )?;
        }
    }

    Ok(())
}

async fn wait_for_shutdown(cancel: tokio_util::sync::CancellationToken) {
    let _ = tokio::signal::ctrl_c().await;
    println!("\nReceived Ctrl+C, shutting down...");
    cancel.cancel();
}

// =============================================================================

/// Distill the running proxy's cache to disk as markdown and/or JSON.
///
/// Streams cache entries from the running proxy over gRPC, renders them,
/// and writes per-entry files + consolidated index files to `output_dir`.
/// Optionally runs a post-process command after writing.
#[allow(clippy::too_many_arguments)]
fn run_distill(
    context: Option<String>,
    tier: super::DistillTierArg,
    limit: u32,
    include_stale: bool,
    output_dir: Option<std::path::PathBuf>,
    cat: bool,
    post_process: Option<String>,
) -> anyhow::Result<()> {
    use conproxy::proxy::grpc::proto;
    use conproxy::proxy::grpc::proto::observability_service_client::ObservabilityServiceClient;
    use conproxy::proxy::slug::slugify;
    use std::io::Write;
    use std::path::PathBuf;
    use tokio_stream::StreamExt;

    let config = Config::load()?;
    let listen_addr = config.config.proxy.listen().to_string();

    // Resolve tier: 0=primary, 1=semantic, 2=both
    let tier_u32: u32 = match tier {
        super::DistillTierArg::Primary => 0,
        super::DistillTierArg::Semantic => 1,
        super::DistillTierArg::Both => 2,
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    // Connect + call streaming RPC
    let entries: Vec<proto::DistillEntry> = rt.block_on(async {
        let grpc_url = format!("http://{}", listen_addr);
        let channel = tonic::transport::Endpoint::from_shared(grpc_url)
            .map_err(|e| anyhow::anyhow!("invalid gRPC URL: {}", e))?
            .connect()
            .await
            .map_err(|e| anyhow::anyhow!("failed to connect to proxy at {}: {}", listen_addr, e))?;
        let mut client = ObservabilityServiceClient::new(channel);
        let req = proto::DistillRequest {
            context: context.unwrap_or_default(),
            tier: tier_u32,
            limit,
            include_stale,
        };
        let stream = client
            .get_cache_distill(req)
            .await
            .map_err(|e| anyhow::anyhow!("get_cache_distill RPC failed: {}", e))?
            .into_inner();
        let mut collected: Vec<proto::DistillEntry> = Vec::new();
        let mut s = stream;
        while let Some(item) = s.next().await {
            match item {
                Ok(e) => collected.push(e),
                Err(status) => {
                    eprintln!("stream error: {}", status.message());
                }
            }
        }
        Ok::<_, anyhow::Error>(collected)
    })?;

    if entries.is_empty() {
        println!("No cache entries to distill.");
        return Ok(());
    }

    // --cat: print all entries flat to stdout
    if cat {
        for e in &entries {
            println!("--- query: {} ---", e.query);
            println!("context_id: {}", e.context_id);
            println!("upstream_id: {}", e.upstream_id);
            println!("hash: {}", e.hash_hex);
            println!("cached_at_ms: {}", e.cached_at_ms);
            println!("response_json:");
            let resp = String::from_utf8_lossy(&e.response_json);
            println!("{}", resp);
            if !e.embedding.is_empty() {
                println!("embedding: [{} floats]", e.embedding.len());
            }
            println!();
        }
        return Ok(());
    }

    // Resolve output directory (CLI flag > config default > cwd/distill)
    let out_dir: PathBuf = output_dir
        .or_else(|| {
            config
                .config
                .proxy
                .distill
                .output_dir
                .as_ref()
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| PathBuf::from("distill"));
    std::fs::create_dir_all(&out_dir)?;

    let format = config.config.proxy.distill.format().to_string();
    let write_md = format == "md" || format == "both";
    let write_json = format == "json" || format == "both";

    let mut index_md = String::from("# Distill Index\n\n");
    let mut index_json: Vec<serde_json::Value> = Vec::new();
    index_md.push_str(&format!("Entries: {}\n\n", entries.len()));

    for e in &entries {
        let suffix = if e.hash_hex.len() >= 8 {
            &e.hash_hex[..8]
        } else {
            e.hash_hex.as_str()
        };
        let base_slug = slugify(&e.query);
        let base_name = if base_slug.is_empty() {
            format!("entry-{}", suffix)
        } else {
            format!("{}-{}", base_slug, suffix)
        };

        if write_md {
            let mut md = String::new();
            md.push_str(&format!("# {}\n\n", e.query));
            md.push_str(&format!("- **Context**: `{}`\n", e.context_id));
            md.push_str(&format!("- **Upstream**: `{}`\n", e.upstream_id));
            md.push_str(&format!("- **Hash**: `{}`\n", e.hash_hex));
            md.push_str(&format!("- **Cached at (ms)**: {}\n", e.cached_at_ms));
            md.push_str(&format!("- **TTL extensions**: {}\n", e.extended_count));
            md.push_str("\n## Response\n\n```json\n");
            md.push_str(&String::from_utf8_lossy(&e.response_json));
            md.push_str("\n```\n");
            if !e.embedding.is_empty() {
                md.push_str(&format!("\n_Embedding: {} floats_\n", e.embedding.len()));
            }
            let md_path = out_dir.join(format!("{}.md", base_name));
            let mut f = std::fs::File::create(&md_path)?;
            f.write_all(md.as_bytes())?;
        }

        if write_json {
            let json_path = out_dir.join(format!("{}.json", base_name));
            let mut f = std::fs::File::create(&json_path)?;
            f.write_all(&e.response_json)?;
        }

        index_md.push_str(&format!(
            "- `{}` — context `{}`, upstream `{}`\n",
            base_name, e.context_id, e.upstream_id
        ));
        index_json.push(serde_json::json!({
            "slug": base_name,
            "query": e.query,
            "context_id": e.context_id,
            "upstream_id": e.upstream_id,
            "hash_hex": e.hash_hex,
            "cached_at_ms": e.cached_at_ms,
            "extended_count": e.extended_count,
            "has_embedding": !e.embedding.is_empty(),
        }));
    }

    if write_md {
        let index_path = out_dir.join("_index.md");
        let mut f = std::fs::File::create(&index_path)?;
        f.write_all(index_md.as_bytes())?;
    }
    if write_json {
        let index_path = out_dir.join("_index.json");
        let arr = serde_json::json!(index_json);
        let pretty = serde_json::to_string_pretty(&arr)?;
        let mut f = std::fs::File::create(&index_path)?;
        f.write_all(pretty.as_bytes())?;
    }

    println!("Wrote {} entries to {}", entries.len(), out_dir.display());

    // Optional post-process: cross-platform, no shell
    if let Some(cmdline) =
        post_process
            .as_ref()
            .or(config.config.proxy.distill.post_process_cmd.as_ref())
    {
        let parts: Vec<&str> = cmdline.split_whitespace().collect();
        let program = parts
            .first()
            .ok_or_else(|| anyhow::anyhow!("post-process command is empty"))?;
        let mut cmd = std::process::Command::new(program);
        cmd.args(parts.get(1..).unwrap_or(&[]));
        let status = cmd.status()?;
        if !status.success() {
            eprintln!(
                "post-process command exited with status: {}",
                status.code().unwrap_or(-1)
            );
        }
    }

    Ok(())
}
