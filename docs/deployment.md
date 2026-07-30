# Deployment Guide

This guide covers running conproxy in production: system services, daemon mode, Docker, P2P replication, monitoring, and performance tuning.

## System Service

Conproxy can install itself as a systemd service (Linux) or launchd service (macOS).

### Install

```bash
# Install and start immediately
conproxy install --start

# Install with custom settings
conproxy install --listen 0.0.0.0:9090 --upstream http://qdrant:6333 --start
```

### Manage

```bash
# Check status
conproxy status

# View logs
conproxy logs -n 100
conproxy logs -f          # Follow (tail -f)

# Stop the service
conproxy stop

# Uninstall
conproxy uninstall
conproxy uninstall --purge   # Also remove config files
```

The service unit file is installed to the standard systemd location. The proxy runs as the current user.

## Daemon Mode

For environments where systemd isn't available, run in daemon mode:

```bash
conproxy start --daemon
```

This:
- Forks to background
- Writes a PID file (per-project, using a blake3 hash of the project path)
- Redirects output to the log file

Stop with:

```bash
conproxy stop
```

PID files are stored per-project to allow multiple proxy instances for different projects.

## Docker

The image runs as non-root user `conproxy` (uid 10001) with `WORKDIR /var/lib/conproxy`.

### Pull (recommended)

Published multi-arch images (linux/amd64 + linux/arm64) are pushed to GHCR on every `v*` tag:

```bash
# Latest tag
docker pull ghcr.io/jmcgrath207/conproxy:0.1.0

# Floating
docker pull ghcr.io/jmcgrath207/conproxy:latest
```

### Build (local dev)

```bash
# Local tag used by Tilt / kind dev loop
docker build -t conproxy:dev .

# Versioned local build
make docker-build VERSION=0.1.0

# Multi-arch smoke (requires buildx)
make docker-buildx VERSION=0.1.0 PLATFORMS=linux/amd64,linux/arm64
```

### Run

```bash
# Config auto-discovery looks for .conproxy/conproxy.toml relative to CWD.
# Mount your project directory at /var/lib/conproxy (the WORKDIR):

docker run -d \
  -p 9999:9999 \
  -v $(pwd):/var/lib/conproxy:ro \
  ghcr.io/jmcgrath207/conproxy:0.1.0 start --listen 0.0.0.0:9999
```

### Docker Compose

```yaml
services:
  conproxy:
    build: .
    ports:
      - "9999:9999"
      - "10000:10000"
    volumes:
      - ./.conproxy:/var/lib/conproxy/.conproxy:ro
    command: start --listen 0.0.0.0:9999

  qdrant:
    image: qdrant/qdrant:latest
    ports:
      - "6333:6333"
```

## P2P Replication

Multiple conproxy instances can replicate cache state between each other using CDC events over gRPC.

### Configuration

```toml
# Node A
[proxy.peer]
enabled = true
node_id = "pod-a"
peers = ["pod-b.conproxy-svc:9090"]
snapshot_on_join = true
ready_threshold = 0.8

# Node B
[proxy.peer]
enabled = true
node_id = "pod-b"
peers = ["pod-a.conproxy-svc:9090"]
snapshot_on_join = true
```

### How it works

1. Each node publishes cache mutations as CDC events
2. Peers subscribe to each other's CDC streams
3. Events are applied with deduplication (echo prevention + last-write-wins by wall timestamp)
4. New peers request a full snapshot on join for fast warm-up

### Security / when to use

- **Trusted private network by default.** Set `[proxy.peer] shared_secret` so peer gRPC requires `x-peer-secret`. **No mTLS (not planned)** — external mesh/NetworkPolicy if you need more.
- When `peer.enabled = true` without `shared_secret`, startup logs `WARN` (trusted-net only). With secret set, logs `info` instead.
- With `shared_secret` set, unauthenticated peer RPCs are rejected. Still treat peer ports as cluster-internal.
- Prefer P2P when multiple conproxy replicas share one logical cache and you want warm joins + miss coalescing across pods.
- Prefer single-node when one replica is enough or the network path between peers is untrusted.

### CLI for replication status

```bash
conproxy peer          # Show replication status
conproxy peer --json
conproxy cdc           # Show CDC event stream status
```

### Start with peer flags

```bash
conproxy start --node-id pod-a --peers pod-b:9090,pod-c:9090
```

## Monitoring

### Prometheus

Conproxy exposes a Prometheus metrics endpoint at `/metrics/prometheus` (no authentication required).

```bash
curl http://127.0.0.1:9090/metrics/prometheus
```

Key metrics:

| Metric | Description |
|--------|-------------|
| `conproxy_pool_upstreams_total` | Total configured upstreams |
| `conproxy_pool_upstreams_healthy` | Healthy upstreams |
| `conproxy_pool_upstreams_by_type` | Upstreams by type (fts, vector_db, hybrid) |
| `conproxy_pool_active_connections` | Current active connections |
| `conproxy_pool_utilization` | Pool utilization percentage |


### Prometheus scrape config

```yaml
scrape_configs:
  - job_name: conproxy
    static_configs:
      - targets: ['localhost:9090']
    metrics_path: /metrics/prometheus
    scrape_interval: 15s
```

### Grafana

The Prometheus metrics can be visualized with any Grafana dashboard that supports the exposed metric names. Key panels to create: cache hit rate, upstream health status, request latency (P50/P95/P99), and connection pool utilization.

### JSON metrics

For programmatic access, use the JSON metrics endpoint:

```bash
curl -H 'X-Api-Key: my-key' http://127.0.0.1:9090/metrics
curl -H 'X-Api-Key: my-key' http://127.0.0.1:9090/stats
```

### Health checks

```bash
# Health (is the proxy running?)
curl http://127.0.0.1:9090/health

# Readiness (is the proxy ready to serve?)
curl http://127.0.0.1:9090/ready
```


## Socket Tuning

For production workloads, tune TCP settings:

```toml
[proxy.socket_tuning]
tcp_nodelay = true           # Disable Nagle's (lower latency)
reuse_port = true            # Kernel-level load balancing
listen_backlog = 4096        # Connection queue depth
tcp_keepalive_secs = 60      # Keepalive idle
tcp_keepalive_interval = 15  # Probe interval
tcp_keepalive_probes = 5     # Probes before dead
defer_accept_secs = 5        # Defer accept (Linux, reduces SYN floods)
user_timeout_ms = 30000      # TCP user timeout (Linux)

# Upstream connection recycling
upstream_pool_idle_timeout_secs = 90
upstream_pool_max_idle = 32

# Buffer sizes (omit for OS autotuning, which is usually best)
# send_buffer_size = 262144    # 256 KB
# recv_buffer_size = 262144
```

### Linux kernel tuning

For high-throughput deployments, also tune the OS:

```bash
# Increase connection tracking
sysctl -w net.core.somaxconn=65535
sysctl -w net.ipv4.tcp_max_syn_backlog=65535

# Reuse TIME_WAIT sockets
sysctl -w net.ipv4.tcp_tw_reuse=1

# Increase file descriptor limits
ulimit -n 65535
```

## Linux Sandbox

When built with the `linux-sandbox` feature, conproxy can apply a seccomp sandbox that restricts system calls. This limits what the process can do if compromised.

```bash
cargo build --release --features linux-sandbox
```

The sandbox is applied automatically on Linux when the feature is compiled in.

**When to enable vs skip:**

- **Enable** when running conproxy as a **bare binary** started as root (e.g., to bind to ports <1024, or via systemd with `User=root`). The sandbox drops to an unprivileged user/group, sets `PR_SET_NO_NEW_PRIVS`, and drops Linux capabilities after binding. Any parser/FFI/RCE vulnerability in the proxy lands as an unprivileged user instead of full root.
- **Skip** when running inside a **container** (Docker, Kubernetes, Podman). The container runtime already provides seccomp filtering, capability bounding, and `no-new-privileges` via `--cap-drop=ALL --security-opt=no-new-privileges` or PodSecurityStandards. Adding the feature is harmless but redundant — omit it to keep the binary and build smaller.

## Log Management

```bash
# View recent logs
conproxy logs -n 100

# Follow logs in real-time
conproxy logs -f

# Combine
conproxy logs -n 50 -f
```

Log verbosity is controlled via the `RUST_LOG` environment variable:

```bash
RUST_LOG=info conproxy start           # Default
RUST_LOG=debug conproxy start          # Verbose
RUST_LOG=conproxy=debug conproxy start # Verbose for conproxy only
```

## Hot Reload

Reload configuration without restarting:

```bash
curl -X POST http://127.0.0.1:9090/admin/reload -H 'X-Api-Key: my-key'
```

This re-reads `conproxy.toml` and applies changes to most settings. Upstream additions/removals and listen address changes require a restart.

## Test / e2e compose note

Default `tests/e2e/docker-compose.yml` starts **qdrant + elasticsearch + opensearch + meilisearch×2 + pgvector**.
Single-backend proof prefers testcontainers: `make test-integration`.
