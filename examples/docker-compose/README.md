# docker-compose example

Runs **conproxy + Meilisearch** side by side with the published container
image. Use this as a starting point; for production see
[`../../docs/deployment.md`](../../docs/deployment.md) and the Helm chart.

Meilisearch is chosen here because it's **text-native out of the box** — no
embedder or FastEmbed config required for the demo flow. Swap it for
Qdrant / Elasticsearch / pgvector (see `examples/multi-upstream-cascade.toml`)
when you're ready for vector search.

## What's in here

| File | Role |
|------|------|
| `docker-compose.yml` | Two services (`meilisearch`, `conproxy`), healthcheck, pinned versions |
| `conproxy.toml`     | Context-rooted config; upstream uses Compose DNS (`http://meilisearch:7700`) |

## Run

```bash
docker compose up -d
```

Wait for `conproxy-compose-proxy` to log `listening on 0.0.0.0:9999` (`docker compose logs -f conproxy`).

## Smoke test

```bash
# 1. health
curl -s http://127.0.0.1:10000/health

# 2. create the index Meilisearch will search
curl -s -X POST http://127.0.0.1:7700/indexes \
  -H 'Authorization: Bearer dev_master_key' \
  -H 'Content-Type: application/json' \
  -d '{"uid": "docs", "primaryKey": "id"}'

# 3. seed one doc (searchable fields auto-inferred on first hit)
curl -s -X POST http://127.0.0.1:7700/indexes/docs/documents \
  -H 'Authorization: Bearer dev_master_key' \
  -H 'Content-Type: application/json' \
  -d '[{"id": 1, "title": "Rust errors", "body": "how to handle errors in rust"}]'

sleep 2  # let Meilisearch index

# 4. first call — miss
curl -s http://127.0.0.1:10000/query \
  -H 'Content-Type: application/json' \
  -d '{"query": "rust errors", "top_k": 5}'

# 5. second call — hit
curl -s http://127.0.0.1:10000/query \
  -H 'Content-Type: application/json' \
  -d '{"query": "rust errors", "top_k": 5}'
```

You should see `cache_status: "miss"` then `"hit"`.

## Stop / reset

```bash
docker compose down           # stop, keep volumes
docker compose down --volumes # nuke Meilisearch storage too
```

## Customize

- **Different release** — bump `conproxy` image tag in `docker-compose.yml`.
- **Different backend** — swap `meilisearch` service (e.g. `qdrant`) and update
  `conproxy.toml` (`type = "qdrant"`, URL `http://qdrant:6333`). Note: Qdrant
  vector search requires FastEmbed or `query_mode = "vector_only"` + a local
  embedder; Meilisearch is the path of least friction for a demo.
- **Multi-backend cascade** — add more `[upstreams.*]` + `[[contexts.default.upstreams]]` entries; see [`../multi-upstream-cascade.toml`](../multi-upstream-cascade.toml).
- **Persistent conproxy data** — if you enable the `persistence` feature, mount a volume for `redb` (advanced; default compose does not use disk-backed cache).

## Not for production

This example is **single-node**, **no TLS**, **master key in plain text**,
**no auth on conproxy**, **no replication**. For HA + secrets + scale, use
the Helm chart or roll your own systemd / k8s manifests. See
[`docs/deployment.md`](../../docs/deployment.md).