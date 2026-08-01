# Quickstart

Get your first cached query running in 60 seconds.

## Prerequisites

- **Rust** 1.75+ ([rustup](https://rustup.rs/))
- **Docker** (for search backend)
- ~50 MB disk space

## Install

```bash
cargo install --path . --features release

# Verify
conproxy --version
```

## Bring up a backend

```bash
docker run -d -p 6333:6333 qdrant/qdrant
```

## Start with an example config

`conproxy start` creates the `.conproxy/` project directory on first launch
— no separate `init` step is needed. Pick a config that matches your backend
and point `start` at it with `--config`:

```bash
conproxy start --config examples/qdrant-quickstart.toml --daemon
```

## First query

```bash
curl -s http://127.0.0.1:9090/query \
  -H 'Content-Type: application/json' \
  -d '{"query": "error handling in rust", "top_k": 5}'
```

Response has `"cache_status": "miss"`, took_ms ~45 ms.

Run the same query again — `"cache_status": "hit"`, took_ms ~0 ms.

## Stop

```bash
conproxy stop
```

## Next steps

- **[Tour](tour.md)** — full feature walkthrough
- **[Configuration](configuration.md)** — all config fields
- **[CLI Reference](cli-reference.md)** — all commands and flags
- **[Docker Compose](docker-compose.md)** — side-by-side proxy + backend stack (no Rust toolchain needed)
