# CLI Reference

```
conproxy <COMMAND> [OPTIONS]
```

## Commands Overview

| Command | Description |
|---------|-------------|
| `init` | Initialize a new conproxy project |
| `start` | Start the cache proxy server |
| `stop` | Stop the running proxy |
| `status` | Show proxy status |
| `search` | Search via the proxy |
| `seed` | Manage seed phrases |
| `discover` | Test upstreams with seed queries |
| `distill` | Dump cache entries to disk for LLM ingestion |
| `contexts` | List cache contexts |
| `context` | Manage a specific context |
| `peer` | Show peer replication status |
| `cdc` | Show CDC event stream status |
| `install` | Install as a system service |
| `uninstall` | Uninstall the system service |
| `logs` | Show proxy service logs |
| `mcp` | Start the MCP server |

---

## `conproxy start`

Start the cache proxy server. The `.conproxy/` project directory is
created automatically on first launch — no separate `init` step is needed.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--config` | `path` | merged `~/.conproxy` + `.conproxy/conproxy.toml` | Path to a conproxy.toml file |
| `--listen` | `string` | from config | Listen address |
| `--upstream` | `string` | from config | Upstream URL (single upstream shorthand) |
| `--daemon` | — | — | Run in background |
| `--node-id` | `string` | from config | Node ID for peer replication |
| `--peers` | `string` | from config | Comma-separated peer addresses |

```bash
# Foreground (uses .conproxy/conproxy.toml)
conproxy start

# Start with an example config
conproxy start --config examples/qdrant-quickstart.toml --daemon

# Background with custom address
conproxy start --daemon --listen 0.0.0.0:8080

# With peer replication
conproxy start --node-id pod-a --peers pod-b:9090,pod-c:9090
```

---

## `conproxy stop`

Stop the running proxy. Reads the PID file to find the process.

```bash
conproxy stop
```

---

## `conproxy status`

Show proxy status including uptime, cache stats, and upstream health.

| Flag | Type | Description |
|------|------|-------------|
| `--json` | — | Output as JSON |

```bash
conproxy status
conproxy status --json
```

---

## `conproxy search`

Search documentation via the running proxy.

| Argument | Type | Description |
|----------|------|-------------|
| `query` | `string` | Search query (required) |

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `-l, --limit` | `usize` | `5` | Maximum results |
| `-f, --format` | `text\|json\|markdown` | `text` | Output format |

```bash
conproxy search "error handling in rust"
conproxy search "async patterns" --limit 3 --format json
```

---

## `conproxy scope` (alias: `seed`)

Ops-thin scope/cache helpers. **Tune phrases via MCP or TOML** — CLI does not add/remove phrases.

### `conproxy scope list`

List configured scope phrases (from `weighted_phrases` / legacy seeds).

```bash
conproxy scope list
conproxy scope list --json
conproxy seed list   # deprecated alias
```

### `conproxy scope clear`

Clear cached entries (requires running proxy).

```bash
conproxy scope clear --all --confirm
conproxy scope clear --low-seed-sim --confirm
conproxy seed clear --all --confirm   # deprecated alias
```

Removed (use MCP plan 09 + edit config): `seed add`, `seed remove`, `seed fetch`, `seed info`, `seed lookup`.


## `conproxy contexts`

List all available cache contexts.

| Flag | Type | Description |
|------|------|-------------|
| `--json` | — | Output as JSON |

```bash
conproxy contexts
```

---

## `conproxy context`

Manage a specific context.

| Argument | Type | Description |
|----------|------|-------------|
| `id` | `string` | Context ID |

| Flag | Type | Description |
|------|------|-------------|
| `--switch` | — | Switch to this context |
| `--create` | — | Create if it doesn't exist |
| `--upstream` | `string` | Upstream URL for new context |
| `--collection` | `string` | Collection name for new context |
| `--json` | — | Output as JSON |

```bash
# View context details
conproxy context production

# Create and switch
conproxy context my-project --create --switch

# Create with upstream
conproxy context staging --create --upstream http://staging-qdrant:6333
```

---

## `conproxy distill`

Dump cache entries to disk for LLM ingestion. Connects to the running proxy
over gRPC, streams matching entries, and writes one Markdown file (plus an
optional JSON sidecar) per entry. See [Distill](distill.md) for full details.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--context` | `string` | all | Filter to a single context ID |
| `--tier` | `primary\|semantic\|both` | `primary` | Which cache level to dump |
| `--limit` | `u32` | unlimited | Cap on entries returned (oldest first) |
| `--include-stale` | — | `false` | Include entries past their fresh TTL |
| `--output-dir` | `path` | config or `distill/` | Where to write per-entry files + index |
| `--cat` | — | `false` | Print everything to stdout instead of writing files |
| `--post-process` | `string` | — | Command to run after the dump (whitespace-split, no shell) |

```bash
# Default: write everything to ./distill as Markdown
conproxy distill

# Snapshot a single context, JSON only
conproxy distill --context production --output-dir /snap

# Stream to stdout and pipe into another tool
conproxy distill --cat | jq .

# Include stale entries and cap at 50
conproxy distill --include-stale --limit 50
```

---

## `conproxy peer`

Show peer replication status.

| Flag | Type | Description |
|------|------|-------------|
| `--json` | — | Output as JSON |

```bash
conproxy peer
```

---

## `conproxy cdc`

Show CDC event stream status.

| Flag | Type | Description |
|------|------|-------------|
| `--json` | — | Output as JSON |

```bash
conproxy cdc
```

---

## `conproxy install`

Print a systemd (Linux) or launchd (macOS) unit and the root commands to install it.
**Does not write to `/etc` itself** — you run the printed `sudo tee` / `launchctl` steps.

| Flag | Type | Description |
|------|------|-------------|
| `--listen` | `string` | Listen address for the service |
| `--upstream` | `string` | Upstream URL for the service |
| `--start` | — | Include start command in printed instructions |

```bash
conproxy install --start
conproxy install --listen 0.0.0.0:9090 --start
```

---

## `conproxy uninstall`

Uninstall the system service.

| Flag | Type | Description |
|------|------|-------------|
| `--purge` | — | Remove configuration files too |

```bash
conproxy uninstall
conproxy uninstall --purge
```

---

## `conproxy logs`

Show proxy service logs.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `-n, --lines` | `usize` | `50` | Number of lines to show |
| `-f, --follow` | — | — | Follow log output (tail -f) |

```bash
conproxy logs
conproxy logs -n 100 -f
```

---

## `conproxy mcp`

Start the MCP server using stdio transport. This command is intended to be launched by an MCP client (Claude Desktop, opencode), not run directly.

Requires the `mcp` feature flag at compile time. See [MCP Integration](mcp-integration.md).

```bash
conproxy mcp
```
