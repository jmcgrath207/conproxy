# In-process Engine

Query-core cache inside your process. Same request path as the daemon (`execute_query`). No gRPC, no peer. Optional read-only HTTP dashboard. 

Most users should keep the **daemon** (shared cache across agents; stdio MCP dies with the client; the hop is not the bill). Use Engine when you own the MCP server and want an in-process cache.

The default Python wheel is **not thin** — it links the query core.

## Rust

```rust
use conproxy::{Engine, QueryOpts};
use std::time::Duration;

let engine = Engine::builder()
    .config_toml("conproxy.toml")
    .ttl(Duration::from_secs(300))
    .context("global")
    .build()?;

let result = engine
    .query("how does X work", QueryOpts { top_k: Some(10), ..Default::default() })
    .await?;
// result.cache_status: Hit | Miss | Stale | Frozen
```

`build()` is sync. Caller should already be on a tokio runtime so the refresh worker can spawn.

## Python

```python
from conproxy import Engine

engine = Engine(config="conproxy.toml")                 # may block on toml/redb/ONNX
# engine = await Engine.create(config="conproxy.toml")  # same build, off the event loop

result = await engine.query("how does X work", top_k=10)
# result.cache_status: 1=hit, 2=miss, 3=stale, 4=frozen  (proto ints)
```

Always `await query()`. There is no sync `query()` (deadlock under a running loop).

GIL is released for the whole Rust `execute_query`. Held only to convert args in and the response out.

```python
async def search_docs(query: str, limit: int = 10):
    return await engine.query(query, top_k=limit)
```

`async with engine:` calls `close()` on exit.

## Defaults vs daemon

| | Daemon | Engine |
|---|---|---|
| Default context | `"default"` | `"global"` (created on build) |
| Transport | gRPC / HTTP / `conproxy mcp` | Function call |
| Refresh worker | Started with HTTP bind | Started with Engine |
| HTTP dashboard | Full REST + auth | Read-only, opt-in |
| Peer | Optional | Never started |

Shared redb with a daemon: pass `context="default"` or you will not see those hits.

## Dashboard (opt-in)

Bind a read-only HTTP server serving the embedded SPA and the GETs it needs — `/health`, `/stats`, `/metrics`, `/circuit`, `/queue`, `/pool`, `/cache/integrity`, `/cache/upstreams`, `/cache/entries`, `/contexts`, `/contexts/current`, `/contexts/{id}/stats`, `/debug/tokio`, `/peer/status`, `/dashboard`. **No `/query`, no admin/cache mutations, no auth** — bind loopback unless you know what you're doing.

```python
engine = Engine(config="conproxy.toml", dashboard_listen="127.0.0.1:10000")
print(engine.dashboard_addr())  # "127.0.0.1:10000"; resolves ":0"
```

Rust: `.dashboard_listen("127.0.0.1:10000")`; `Engine::dashboard_addr()` returns the bound `SocketAddr`. `:0` picks a free port. Default: **no listener**. `close()` shuts the server down. Inbound toml `[proxy.web_ui]` is ignored — the listen arg is the switch.

## Knobs

Constructor / builder: `ttl` (300s), `stale_ttl` (3600s), `context`, `semantic`, `threshold`, `max_entries`, `max_memory` (int bytes or `"256MiB"`), `memory_fraction`, `persist_path`, `dashboard_listen`.

Per-call `query()`: `top_k`, `context`, `ttl`, `semantic`, `threshold`, `skip_cache`.

`skip_cache=True` skips lookup, hits upstream, writes. Upstream failure may still serve **Frozen**.

`clear()` wipes the whole store (daemon `cache_clear`). `stats()` returns hits/misses/size/evictions. `close()` / `Drop` stops the refresh worker.

### Memory cap

- `max_memory="256MiB"` → **fixed** cap, no resizing.
- `memory_fraction=0.7` (default) → **dynamic** cap = `fraction × budget`, where budget is cgroup v2 `memory.max`, else `/proc/meminfo MemAvailable`, else 256 MiB.
- A 10s ticker re-reads the budget and resizes when the cap moves by >5% (floor 16 MiB). Shrinks run the S3-FIFO eviction loop immediately. Use `memory_fraction=0` to disable the memory cap entirely.

## Persistence

Optional. Feature `persistence` / extra `conproxy[persist]`. Write-through + restore on open. Live `get()` does not read disk.

redb is single-writer. A second `Engine` or daemon opening the same file fails at construct. No extra flock.

## Cache behavior

S3-FIFO is **eviction**, not hit-path speed. Key is the query string (plus context prefix). `top_k` is not in the key.

## Extras

| Extra | Meaning |
|---|---|
| (none) | `ConproxyClient` + `Engine` (full query core) |
| `[onnx]` | Documents ONNX; wrong wheel → construct error |
| `[persist]` | Documents redb; wrong wheel → construct error |

`fork()` after construct is undefined. Engine does not install a tracing subscriber.

Inbound toml `[[agents]]` / `api_key` are ignored. Circuit, coalesce, retry, and the pool still apply.
