# Security

## Reporting a vulnerability

Please **do not** open a public GitHub issue for security problems.

Report privately to one of:

- GitHub Security Advisories: <https://github.com/jmcgrath207/conproxy/security/advisories/new>
- Email: <security@conproxy.dev> (PGP key on request)

Include:

- Affected version (commit SHA or `conproxy --version` output)
- Reproduction steps or a minimal config
- Impact assessment
- Any known workarounds

We aim to acknowledge within **3 business days** and provide a fix or
mitigation timeline within **10 business days** for critical issues.

## Scope

- All binaries published under `jmcgrath207/conproxy` (tags, `cargo install`).
- Container images on `ghcr.io/jmcgrath207/conproxy` (multi-arch: linux/amd64 + linux/arm64).
- Helm charts on `oci://ghcr.io/jmcgrath207/charts/conproxy` and the matching `.tgz` artifacts on GitHub Releases.
- Workspace crates: `conproxy`, `sdk/rust`, `sdk/python`.
- MCP server (`conproxy mcp`) and the cache proxy itself.

## Out of scope

- Upstream backends (qdrant, Elasticsearch, OpenSearch, Meilisearch,
  pgvector, Pinecone, Milvus). Report those to the respective vendors.
- Example configs in `examples/`. They use placeholder API keys
  (`dev_master_key`) for local development only.

## Supported versions

| Version | Supported |
|---------|-----------|
| latest release (`v0.1.x`) | yes |
| `main` / default branch | best-effort |

There is no LTS policy yet. Pre-`1.0` releases may receive breaking
changes between minor versions; pin with `--tag` or `Cargo.lock`.

## Disclosure policy

We follow coordinated disclosure. Allow a reasonable disclosure window
(default 90 days) before public disclosure, in coordination with the
reporter. Critical issues are addressed faster.
