# Cache Distill

The `distill` command exports the proxy's cache to disk in a format suitable for
LLM ingestion. Each entry becomes a small Markdown file (with a parallel JSON
sidecar when requested), and a consolidated index lists everything that was
written.

> Distill is **always compiled in** (no Cargo feature flag). It's part of
> the standard `conproxy` binary and exposed both as `conproxy distill`
> and as the `GetCacheDistill` gRPC stream (see [API Reference](api-reference.md)).

Distill is **read-only** — it does not touch the running proxy's cache or evict
entries. Use it to snapshot your cache before a major change, share a corpus
with a colleague, or feed a fresh set of seed queries to a local LLM.

## When to use it

- Snapshot the cache after a successful warmup run, then archive the directory.
- Hand a colleague a small, human-readable sample of what's in your cache.
- Bootstrap a retrieval-augmented prompt by dumping the responses your agents
  see most often.

## Quick start

```bash
# Dump everything to ./distill (default output dir)
conproxy distill

# Print everything to stdout instead of writing files
conproxy distill --cat

# Restrict to a single context, top 10 entries by insertion time
conproxy distill --context production --limit 10

# Write to a custom directory
conproxy distill --output-dir /tmp/cache-snapshot

# Include stale entries that would normally be filtered out
conproxy distill --include-stale
```

## Tier selection

`--tier` controls which cache level is dumped:

| Value | Meaning |
|-------|---------|
| `primary` (default) | Exact-match cache, always available |
| `semantic` | Similarity-match cache (requires `embed` or `embed-api` feature) |
| `both` | Stream primary entries with their semantic embeddings attached when present |

The semantic tier is silently downgraded to primary-only when:
- The `embed` / `embed-api` feature is not compiled in,
- The running proxy has no semantic cache configured, or
- The requested tier is `primary` or `both` but no embeddings exist for the
  primary entries (they still ship, just without the `embedding` field).

## Stale handling

By default, entries that have crossed the cache's `stale_duration` are filtered
out — they are kept around for thundering-herd protection but are not useful
for downstream consumption. Pass `--include-stale` to dump them too.

The TTL gate uses the same `jittered_ttl(fresh_duration, &hash)` the running
proxy uses to decide when an entry is past its freshness window, so what you
see matches what an agent would see.

## Output layout

When `--cat` is **not** set, distill writes to `--output-dir` (or
`[proxy.distill.output_dir]` from config, or the default `distill/`):

```
distill/
├── _index.md
├── _index.json
├── error-handling-in-rust-3a2b1c4d.md
├── error-handling-in-rust-3a2b1c4d.json
├── async-patterns-7e8f9a0b.md
└── async-patterns-7e8f9a0b.json
```

- Each entry produces `{slug}-{hash8}.md` (human-readable) and
  `{slug}-{hash8}.json` (raw `QueryResponse` payload).
- The 8-character hash suffix guarantees uniqueness even when two queries
  produce the same slug.
- `_index.md` lists every entry in insertion-time order; `_index.json` is the
  machine-readable equivalent.

## Post-process

Pass `--post-process "<command>"` to run a command after the dump completes.
The command is split on whitespace and executed with `Command::new(parts[0])`,
so no shell is involved — the first token is the program, the rest are
arguments. The following environment variables are set:

| Variable | Description |
|----------|-------------|
| `DISTILL_OUTPUT_DIR` | Absolute path of the dump directory |
| `DISTILL_FILE_COUNT` | Number of entry files written |
| `DISTILL_INDEX_MD` | Absolute path of `_index.md` |
| `DISTILL_INDEX_JSON` | Absolute path of `_index.json` |

Example:

```bash
conproxy distill --output-dir /snap --post-process "tar czf cache-snap.tar.gz -C /snap ."
```

## Configuration

`[proxy.distill]` in `conproxy.toml` provides defaults that the CLI flags
override:

```toml
[proxy.distill]
output_dir = "/var/lib/conproxy/distill"
post_process_cmd = "tar czf /backups/cache-$(date +%s).tar.gz -C $DISTILL_OUTPUT_DIR ."
include_stale = false
```

## Limits

`--limit N` (or `limit` in the request proto) caps the number of entries
returned. `0` means unlimited. Entries are sorted by insertion time (oldest
first) before the limit is applied, so `limit=10` always returns the oldest
ten — pass `--include-stale` if you want recent but expired entries mixed in.

## See also

- [CLI Reference](cli-reference.md) — `conproxy distill` flags in full
- [Configuration](configuration.md) — `[proxy.distill]` TOML reference
- [API Reference](api-reference.md) — `GetCacheDistill` gRPC streaming rpc
- [Python SDK](sdk-python.md) — `client.distill()` method
