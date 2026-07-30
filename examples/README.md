# conproxy examples

Runnable configs and integration snippets. Copy any file into `.conproxy/conproxy.toml` and adjust `url`/`api_key` for your environment.

**Config shape:** All examples are context-rooted — `[server]` + `[upstreams.*]` (shared resources) + `[contexts.*]` (legs via `ref` + per-context overrides). Cache, scope, cascade, and federated policy live on the context, not on a global section.

| File | Use case |
|------|----------|
| `qdrant-quickstart.toml` | Single Qdrant upstream, default context + cache. Matches [Quickstart](../docs/quickstart.md). |
| `meilisearch-quickstart.toml` | Single Meilisearch upstream with API key auth. |
| `multi-upstream-cascade.toml` | Two-upstream priority cascade with RRF fusion. See [Multi-Upstream](../docs/multi-upstream.md#priority-based-cascade). |
| `federated-search.toml` | Local-first federated search with merge modes. See [Federated](../docs/multi-upstream.md#federated-search). |
| `multi-context.toml` | Two contexts share one Meili resource; isolated cache + scope. |
| `mcp-claude-desktop.json` | Claude Desktop MCP server registration. See [MCP Integration](../docs/mcp-integration.md). |
| `mcp-opencode.jsonc` | opencode MCP server registration (global config). See [MCP Integration](../docs/mcp-integration.md). |
| `distill-postprocess.sh` | Post-process hook for `conproxy distill`. See [Distill](../docs/distill.md). |
