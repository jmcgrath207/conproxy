# Tour

A full walkthrough of conproxy features.

## Project Initialization

Conproxy uses a `.conproxy/` directory in your project root for configuration. A global `~/.conproxy/` config provides defaults that local configs can override.

There is no separate `init` step — `.conproxy/` is created automatically the first time you `start`, `save` a config, or run a CLI command that needs to persist state.

### Point `start` at an example config

```bash
# Bring up a backend, then start with the matching example config
docker run -d -p 6333:6333 qdrant/qdrant
conproxy start --config examples/qdrant-quickstart.toml --daemon
```

This creates `.conproxy/conproxy.toml` (and a `.gitignore`) on first launch
with the example upstream pre-configured. To customize, edit
`.conproxy/conproxy.toml` directly — see [Configuration](configuration.md).

### What gets created

```
.conproxy/
├── conproxy.toml    # Project configuration
├── cache/           # Disk cache (if persistence enabled)
└── .gitignore       # Excludes cache/, *.pid
```

## Configuration

Open `.conproxy/conproxy.toml` and verify your upstream:

```toml
[proxy]
listen = "127.0.0.1:9090"
fresh_duration_secs = 300
stale_duration_secs = 600
max_entries = 10000

[[proxy.upstreams]]
id = "my-qdrant"
url = "http://localhost:6333"
upstream_type = "qdrant"
timeout_secs = 30
```

**Key settings:**

- `listen` — Address the proxy binds to (gRPC + HTTP share the same port by default; HTTP defaults to gRPC port + 1 if `http_listen` is set separately)
- `fresh_duration_secs` — How long a cached response is considered fresh
- `stale_duration_secs` — How long a stale response is served while refreshing in the background
- `max_entries` — Maximum cache entries before eviction kicks in

See [Configuration](configuration.md) for the full reference.

## Starting the Proxy

### Foreground (development)

```bash
conproxy start
```

The proxy logs to stdout. Press Ctrl+C to stop.

### Daemon mode (background)

```bash
conproxy start --daemon
```

The proxy writes a PID file and runs in the background.

### Override config from CLI

```bash
conproxy start --listen 127.0.0.1:8080 --upstream http://localhost:6333
```

### Check status

```bash
conproxy status
```

```
Proxy Status
  State:    running (PID 12345)
  Listen:   127.0.0.1:9090
  Uptime:   2m 30s
  Cache:    142 entries (3.2 MB)
  Upstreams: 1 healthy, 0 degraded, 0 offline
```

### Stop the proxy

```bash
conproxy stop
```

## Your First Query

With the proxy running, send a query:

```bash
curl -s http://127.0.0.1:9090/query \
  -H 'Content-Type: application/json' \
  -d '{"query": "error handling in rust", "top_k": 5}' | jq .
```

Response:

```json
{
  "results": [
    {
      "id": "doc-42",
      "score": 0.89,
      "content": "Rust uses the Result type for recoverable errors...",
      "metadata": {"source": "rust-book"},
      "upstream_id": "my-qdrant"
    }
  ],
  "cache_status": "miss",
  "took_ms": 45
}
```

Run the same query again:

```json
{
  "results": [ ... ],
  "cache_status": "hit",
  "took_ms": 0
}
```

The second response comes from cache — `cache_status` changes from `miss` to `hit` and latency drops to near zero.

### Cache status values

| Status | Meaning |
|--------|---------|
| `hit` | Served from fresh cache |
| `miss` | Fetched from upstream, now cached |
| `stale` | Served stale while refreshing in background |
| `frozen` | Upstream offline, serving last known good response |

## Batch Queries

Send multiple queries in one request:

```bash
curl -s http://127.0.0.1:9090/batch \
  -H 'Content-Type: application/json' \
  -d '{
    "queries": [
      {"query": "error handling", "top_k": 3},
      {"query": "async programming", "top_k": 3}
    ]
  }' | jq .
```

## Using the CLI Search

You can also search directly from the command line:

```bash
# Text output
conproxy search "error handling in rust"

# JSON output
conproxy search "error handling in rust" --format json

# Limit results
conproxy search "error handling in rust" --limit 3
```

## Adding Scope Phrases

Scope phrases (formerly called "seeds") define what your project cares
about. They help filter irrelevant results and prioritize relevant
content. The CLI command is `conproxy scope`; `conproxy seed` is a
deprecated alias kept for one release.

### Add scope phrases to config

```toml
# In .conproxy/conproxy.toml

[proxy.scope]
seeds = [                    # field is still named `seeds` for back-compat
  "error handling patterns",
  "async runtime architecture",
  "memory safety guarantees"
]
mode = "filter"              # "filter", "rerank", or "boost"
min_seed_similarity = 0.25   # Minimum relevance to scope phrases
```

### Manage scope phrases via CLI

```bash
# Add a scope phrase
# edit [[contexts.*.scope.weighted_phrases]] or use MCP tune

# List scope phrases
conproxy scope list

# Fetch and cache results for a query
# warm via MCP / proxy search (CLI fetch removed)

# Look up what's cached
conproxy scope list
```

## Contexts

Contexts provide cache isolation — different projects or environments can share a proxy without cache collisions.

```bash
# List contexts
conproxy contexts

# Create and switch to a new context
conproxy context my-project --create --switch

# Switch to an existing context
conproxy context production --switch
```

## Export cache for LLM ingestion

Dump cached entries to structured Markdown (with optional JSON sidecars) for bootstrapping LLM knowledge:

```bash
conproxy distill --output-dir ./knowledge-base
```

See [Distill](distill.md) for tier selection, stale handling, and post-process hooks.

## Next Steps

- **[Configuration](configuration.md)** — Full reference for every config field
- **[CLI Reference](cli-reference.md)** — All commands, subcommands, and flags
- **[API Reference](api-reference.md)** — HTTP and gRPC endpoint documentation
- **[Multi-Upstream](multi-upstream.md)** — Set up cascade, federation, and discovery
- **[Deployment](deployment.md)** — Production setup with systemd, monitoring, and replication
- **[MCP Integration](mcp-integration.md)** — Connect conproxy to any stdio MCP client (Claude Desktop, opencode)
