# MCP Integration

Conproxy includes a [Model Context Protocol](https://modelcontextprotocol.io) (MCP) server over stdio transport. Any stdio MCP client — Claude Desktop, opencode, or custom tools — can connect and use `search` plus the dry-run **tune** suite (scope, cache, cascade, federated, embed, rate limit, warm).

## Prerequisites

- Conproxy built with the `mcp` feature (included in the `release` feature)
- A running conproxy instance (for the search tool)

## Building with MCP

The `release` build includes MCP (alongside `persistence`, `embed-api`,
`pgvector`). For the smallest MCP-only binary, use the `mcp` feature by
itself:

```bash
# Recommended: production build (mcp + persistence + embed-api + pgvector)
cargo install --path . --release --locked --features release

# Minimal MCP-only (no persistence/pgvector, smaller binary)
cargo install --path . --release --locked --features mcp
```

If you try to run `conproxy mcp` without the feature, you'll get a message telling you to rebuild.

## How It Works

The MCP server uses **stdio transport** — your MCP client launches `conproxy mcp` as a subprocess and communicates over stdin/stdout.

```
MCP client ──stdio──▶ conproxy mcp
                           │
              ┌────────────┼────────────┐
              ▼            ▼            ▼
      search   tune suite   (dry-run)
              │            │
       HTTP to proxy   in-process sessions
```

The server:
1. Loads your conproxy configuration to find the proxy address
2. Registers search + tune tools with the MCP client
3. Handles search by querying the proxy; tune tools run dry-run math/scorers in-process
4. Returns JSON results for the LLM

## Configure Your Client

### Claude Desktop

Add conproxy to your Claude Desktop config file:

**macOS:** `~/Library/Application Support/Claude/claude_desktop_config.json`
**Linux:** `~/.config/Claude/claude_desktop_config.json`

```json
{
  "mcpServers": {
    "conproxy": {
      "command": "conproxy",
      "args": ["mcp"]
    }
  }
}
```

If conproxy is not on your PATH, use the full path to the binary:

```json
{
  "mcpServers": {
    "conproxy": {
      "command": "/path/to/conproxy",
      "args": ["mcp"]
    }
  }
}
```

Restart Claude Desktop after editing the config.

### opencode

Add conproxy to your global opencode config at `~/.config/opencode/opencode.jsonc`:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "conproxy": {
      "type": "local",
      "command": ["conproxy", "mcp"],
      // If conproxy is not on your PATH, use the full binary path:
      // "command": ["/path/to/conproxy", "mcp"],
      "enabled": true
    }
  }
}
```

After saving the config, restart opencode. Include `use conproxy` in your prompts to activate the search tool, or add a corresponding note to your project's `AGENTS.md`:

```
When you need to search documentation or vector-cached content, use `conproxy` tools.
```

Use `opencode mcp list` to verify the server is connected and `search` appears in the tool list.

### Other stdio MCP clients

Any MCP client that supports stdio transport can use the `conproxy mcp` server. Pass `conproxy mcp` as the command (executable + args) when registering the server.

## Available Tools

### `search`

Search your documentation through the cache proxy.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `query` | `string` | required | Search query |
| `limit` | `usize` | `10` | Maximum results |

This tool requires a running proxy instance. It queries the proxy gRPC/HTTP API on the configured listen address.

### Dry-run tune suite (plan 09)

All tune tools are **dry-run by default**. They never write config files. Sessions are process-local (`agent_id` + `context_id` + `session_id`) with idle TTL.

| Tool | Purpose |
|------|---------|
| `tune_session_open` | Bind agent + context; get `session_id` |
| `tune_session_close` | Drop session |
| `tune_session_list` | List own sessions |
| `scope_tune` | Score C filter/boost/rerank on supplied hits; `min_similarity` sweep |
| `scope_suggest` | Propose `weighted_phrases` from texts |
| `compare_runs` | Diff two `run_id`s in same session |
| `tune_select_run` | Mark winning run for export |
| `tune_export` | `contexts.<id>.scope` TOML/JSON fragment (paste into config) |
| `cache_tune` | TTL hit/stale/miss probe on synthetic events |
| `cascade_tune` | Which leg would serve under thresholds |
| `federated_tune` | Local/remote merge weight preview |
| `embed_tune` | Batch-size latency estimate (no provider call) |
| `rate_limit_tune` | Token-bucket allow/deny simulation |
| `warm_tune` | Warm plan ETA / key overlap (`execute=true` rejected in v1) |
| `tune_workflow` | Composite: open → search → scope_tune → (optional) apply + reload + close |

#### Happy-path cookbook

```
1. tune_session_open  { agent_id, context_id }
2. scope_tune         { session_id, hits[], weighted_phrases[], min_similarity_sweep[] }
3. scope_suggest      { session_id, texts[] }          # optional
4. compare_runs       { session_id, run_id_a, run_id_b }  # optional
5. tune_select_run    { session_id, run_id }           # optional
6. tune_export        { session_id }  → paste formats.toml into config
7. apply_tune         { session_id, reload: true }    # write + hot-reload proxy
   ↳ or reload          # reload-only if you edited config by hand
8. tune_session_close { session_id }
```

**Score C:** filter sweeps use unweighted similarity; phrase `weight` only affects boost/rerank modes.

**Isolation:** another `agent_id` cannot get/export your session. Wrong `context_id` → not found.

#### Composite workflow (recommended)

If your client is prone to batching/merging tool arguments (opencode), prefer the single composite tool:

```
tune_workflow {
  agent_id, context_id, query, top_k?,
  weighted_phrases[], mode?, min_similarity?, min_similarity_sweep[]?,
  apply: false,           // dry-run by default
  reload: true,           // only used when apply=true
  close_session: true,
}
```

The tool opens a session, calls the running proxy for live search hits, runs `scope_tune` on the results, and (optionally) applies + reloads + closes. On a 0-hit search it returns a clear `0 hits for query "<q>" on context "<c>"` error pointing at corpus seeding.

#### Parallel vs sequential tool calls

- **Safe to parallelize**: any of the dashboard status tools (`health` / `overview` / `cache_status` / `pool_status` / `circuit_status` / `metrics_status` / `contexts_status` / `peer_status` / `tokio_status` / `cache_entries`), and multiple `search` calls.
- **Must be ordered (or use `tune_workflow`)**: any tune tool that consumes a `session_id` — the session must be opened first, and `scope_tune` / `federated_tune` need hits from a prior `search`.
- **Argument hygiene**: each tool call must carry a complete, self-contained JSON arguments object. Truncated or merged args across tools are a client bug — the server cannot recover them.

### `tune_workflow`

Composite tune workflow: open session → live search via the running proxy → `scope_tune` on the hits → (optional) `apply_tune` + `reload` + session close. Designed to avoid client-side argument merging bugs by collapsing the whole chain into one tool call.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `agent_id` | `string` | required | Session owner |
| `context_id` | `string` | required | Target context |
| `query` | `string` | required | Live search query |
| `top_k` | `usize` | `10` | Search depth |
| `weighted_phrases` | array? | `[]` | Optional phrase boosts (same shape as `scope_tune`) |
| `mode` | `string?` | — | `filter` / `boost` / `rerank` |
| `min_similarity` | `f32?` | — | Single threshold |
| `min_similarity_sweep` | `f32[]?` | — | Sweep grid |
| `scope_weight` | `f32?` | — | Phrase boost weight |
| `lexical_weight` | `f32?` | — | Lexical weight |
| `apply` | `bool` | `false` | Write selected run to local config |
| `reload` | `bool` | `true` | POST `/admin/reload` after apply |
| `config_path` | `string?` | default local | Optional config path |
| `close_session` | `bool` | `true` | Close the session when finished |
| `session_id` | `string?` | — | Resume an existing session |

Returns one JSON envelope:

```jsonc
{
  "session_id": "sess-...",
  "search": { "query": "...", "top_k": 10, "hit_count": 7 },
  "tune": { /* ScopeTuneReport */ },
  "apply": null,         // or ApplyReport if apply=true
  "close": { "closed": true, "reason": "ok" }
}
```

On a 0-hit search the tool returns a clear error: `tune_workflow: search returned 0 hits for query "..." on context "...". Check the corpus is seeded ...`.

### `apply_tune`

Tune export + write local config + hot-reload, in one call. Defaults:

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `session_id` | `string` | required | From `tune_session_open` |
| `agent_id` | `string` | required | Session owner |
| `context_id` | `string` | required | Target context |
| `config_path` | `string?` | `.conproxy/conproxy.toml` | Optional explicit path |
| `reload` | `bool` | `true` | POST `/admin/reload` after writing |

Returns the `ApplyReport` (path, context_id, source_run_id, context_created, toml_applied) plus the `/admin/reload` response under `reload` if `reload=true`. If reload fails, `reload_error` is set but the tool still returns success (config was written).

### `reload`

POST `/admin/reload` on the running proxy. Returns the raw `ReloadResponse` (success, reloaded[], restart_required[], message). Use after manual edits to `.conproxy/conproxy.toml` or `~/.conproxy/conproxy.toml`.

### Dashboard-parity status tools

One tool per dashboard panel. Same JSON endpoints as `ui/app.js`:

| Tool | Mirrors panel | Endpoints |
|------|---------------|-----------|
| `health` | status dot | `/health` |
| `overview` | Overview | `/metrics`, `/stats`, `/circuit` |
| `cache_status` | Cache | `/stats`, `/pool`, `/cache/integrity` |
| `pool_status` | Connection Pool | `/pool` |
| `circuit_status` | Circuit / Queue | `/circuit`, `/queue` |
| `metrics_status` | Metrics | `/metrics`, `/pool`, `/stats/queries` |
| `contexts_status` | Contexts | `/contexts`, `/contexts/current` |
| `peer_status` | Peer | `/peer/status` |
| `tokio_status` | Tokio | `/debug/tokio` |

All tools honor `proxy.api_key` (`x-api-key` header) and `proxy.http_listen_addr()` for the base URL.

### `benchmark`

Evaluate if a query improved or degraded under the session's tuned scope params. Live query → re-score via `ScopeFilter` → diff (new/evicted/up/down/stable) → verdict.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `session_id` | `string` | required | Tune session |
| `agent_id` | `string` | required | Session owner |
| `context_id` | `string` | required | Context id |
| `query` | `string` | required | Live query to benchmark |
| `top_k` | `usize` | `10` | Comparison depth |
| `run_id` | `string?` | selected/last | Explicit run to apply |

Verdict heuristic: any baseline top-K hit evicted → `degraded` (guardrail); new tuned-only hits → `improved`; complete replacement → `changed`; otherwise `unchanged`. Pure read — does not record on the session.

## Usage Tips

|- **Keep proxy running**: `search` and `tune_workflow` need a running proxy (`conproxy start --daemon`). Pure dry-run tune tools (`scope_tune` with caller-supplied hits, `scope_suggest`, etc.) do not.
- **opencode users**: Add `use conproxy` to your prompt or `AGENTS.md` to make the tools available to the agent.
- **DevEx auto-smoke**: in the Tilt dev cluster, `opencode-test` + `devex-smoke` Tilt resources drive a scripted, MCP-only smoke against a random corpus product. The opencode session DB is in-container only, so every container restart starts with a fresh session (the host sticky SID is cleared too). See [k8s-dev](k8s-dev.md#devex-auto-smoke-opencode-test--sticky-session) (`make devex*`).
- **Apply & reload**: use `apply_tune` or set `apply=true` on `tune_workflow` to write scope params to local config and trigger `/admin/reload`. `reload` reloads only.
- **Close reports the reason**: `tune_session_close` returns `{closed, reason, expected_agent_id, got_agent_id}` when an `agent_id` filter does not match, so an agent can recover without re-opening.

### Authentication

The MCP server forwards `proxy.api_key` (from the MCP-side `Config`) as `x-api-key` on every gRPC search call. So a proxy with `proxy.api_key = "sk-local"` expects the MCP container/process to see the **same** key in its own `conproxy.toml` and the call succeeds.

If the proxy returns `API key required` / `UNAUTHENTICATED` from `search` or `tune_workflow`:

1. Check the MCP-side `conproxy.toml` has the same `proxy.api_key` as the running proxy. The two are independent configs (MCP config is for outbound calls, proxy config is for inbound auth).
2. Reload does not invalidate the upstream API key, but a reload that used to install `Some(empty AgentRegistry)` after a `apply_tune + reload` cycle used to flip gRPC search into "API key required" mode for callers without a key. That regression is fixed: reload now stores `None` (or `Some(empty)`) when no agents are configured, and `authenticate` defensively treats an empty registry as "no auth required".
