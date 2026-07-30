# Python SDK

The `conproxy` Python package ships a native Rust-backed gRPC client plus optional adapters for the most popular LLM frameworks. The compiled module is `conproxy`; pure-Python submodules (`langchain`, `llama_index`) are bundled in the same wheel.

## Installation

```bash
# Core SDK only (conproxy + ConproxyClient)
pip install conproxy

Import: `import conproxy` then `ConproxyClient`.


# With the LangChain retriever adapter
pip install conproxy[langchain]

# With the LlamaIndex retriever adapter
pip install conproxy[llama-index]
```

The `langchain` extra pulls in `langchain-core>=0.1,<1.0`; the `llama-index` extra pulls in `llama-index-core>=0.10,<1.0`. Both extras are isolated — installing one does not require the other.

The package is built with [maturin](https://www.maturin.rs/). To build from source:

```bash
cd sdk/python
maturin develop --release
```

## Core client

```python
from conproxy import ConproxyClient

client = ConproxyClient(
    grpc_url="http://localhost:9999",   # optional; defaults to ~/.conproxy/sdk.toml
    api_key="my-secret",                 # optional
)

resp = client.query("how does conproxy handle cache misses?", top_k=5)
for r in resp.results:
    print(f"[{r.score:.2f}] {r.upstream_id}: {r.content[:80]}...")
```

The client blocks the calling thread for sync methods. For async use:

```python
resp = await client.query_async("how does conproxy handle cache misses?")
```

Full method set (sync + async where available): `query`, `batch_query`, `federated_query`, `stats`, `circuit_status`, `queue_stats`, `clients`, `pool_status`, `list_contexts`, `current_context`, `switch_context`, `create_context`, `context_stats`, `list_agents`, `delete_agent`, `rotate_key`, `reload`, `pause`, `resume`, `cache_clear`, `cache_warmup`, `cache_evict`, `cache_integrity`, `metrics_reset`, `distill`.

## LangChain adapter

```python
from conproxy.langchain import ConproxyRetriever

retriever = ConproxyRetriever(
    grpc_url="http://localhost:9999",
    api_key="my-secret",
    top_k=5,
)

# Sync
docs = retriever.invoke("how does conproxy handle cache misses?")
for d in docs:
    print(d.metadata["score"], d.page_content[:80])

# Async
docs = await retriever.ainvoke("how does conproxy handle cache misses?")
```

Each search hit becomes a LangChain `Document` with `page_content` set to the upstream `content` and `metadata` populated with `score`, `id`, `upstream_id`, and the parsed upstream metadata (or `raw_metadata` on parse failure). The retriever is a Pydantic v2 model (`BaseRetriever` subclass) and accepts all standard LangChain callbacks.

Drop it into any standard LangChain RAG pipeline:

```python
from langchain_core.prompts import ChatPromptTemplate
from langchain_core.runnables import RunnablePassthrough
from langchain_core.output_parsers import StrOutputParser
from langchain_openai import ChatOpenAI

prompt = ChatPromptTemplate.from_template(
    "Answer the question based only on the context below.\n\n"
    "Context:\n{context}\n\n"
    "Question: {question}\n\n"
    "Answer:"
)

chain = (
    {"context": retriever | (lambda docs: "\n\n".join(d.page_content for d in docs)),
     "question": RunnablePassthrough()}
    | prompt
    | ChatOpenAI(model="gpt-4o-mini")
    | StrOutputParser()
)

print(chain.invoke("how does conproxy handle cache misses?"))
```

## LlamaIndex adapter

```python
from conproxy.llama_index import ConproxyRetriever

retriever = ConproxyRetriever(
    grpc_url="http://localhost:9999",
    api_key="my-secret",
    top_k=5,
)

# Sync
nodes = retriever.retrieve("how does conproxy handle cache misses?")
for n in nodes:
    print(f"[{n.score:.2f}] {n.node.text[:80]}...")

# Async
nodes = await retriever.aretrieve("how does conproxy handle cache misses?")
```

Each hit becomes a `NodeWithScore(TextNode)` with the `TextNode` carrying the same metadata as the LangChain `Document` above. Plug it into any `RetrieverQueryEngine`:

```python
from llama_index.core.query_engine import RetrieverQueryEngine
from llama_index.llms.openai import OpenAI
from llama_index.core import Settings

Settings.llm = OpenAI(model="gpt-4o-mini")
engine = RetrieverQueryEngine.from_args(retriever)
print(engine.query("how does conproxy handle cache misses?"))
```

## Examples

Runnable RAG scripts live in `sdk/python/examples/`:

| File | Framework | Description |
|------|-----------|-------------|
| `langchain_rag.py` | LangChain | `ConproxyRetriever` + `ChatOpenAI` via LCEL `Runnable` chain |
| `llama_index_rag.py` | LlamaIndex | `ConproxyRetriever` + `OpenAI` via `RetrieverQueryEngine` |

Both read `CONPROXY_GRPC_URL` and `CONPROXY_API_KEY` from the environment. The chat model key (`OPENAI_API_KEY`) is required at runtime; the script imports `langchain-openai` / `llama-index-llms-openai` lazily so the adapters themselves stay usable without those packages.

### `client.distill()`

Stream cache entries out of a running proxy. Mirrors `conproxy distill`; the
returned list is in insertion-time order (oldest first).

```python
from conproxy import ConproxyClient

client = ConproxyClient(grpc_url="http://127.0.0.1:9090")

# All entries in the default context
entries = client.distill()

# A specific context, primary tier only (default), capped at 100
entries = client.distill(context="production", tier=0, limit=100)

# Include stale entries (past the fresh TTL)
entries = client.distill(include_stale=True)

for e in entries:
    print(e.cached_at_ms, e.context_id, e.query)
```

Each entry is a `DistillEntry` with `query`, `context_id`, `upstream_id`,
`cached_at_ms` (Unix epoch ms), `extended_count`, `response_json` (raw bytes
of the `QueryResponse`), `hash_hex`, and `embedding` (empty unless the
`embed-api` feature is on and the entry has a stored embedding).

## Configuration

`ConproxyClient(grpc_url=None)` falls back to the SDK config file:

| Path | Purpose |
|------|---------|
| `~/.conproxy/sdk.toml` | Default SDK config (loaded by `SdkConfig::load()`) |

See [Configuration](configuration.md) for the full conproxy config reference; the SDK reads the same file via the `conproxy-sdk` crate.

## Errors

| `SdkError` variant | Python exception |
|--------------------|------------------|
| `Connection(msg)`  | `ConnectionError` |
| `Request { code, message }` | `RuntimeError("code: message")` |
| `Config(msg)`      | `ValueError(msg)` |
| `Timeout`          | `TimeoutError("Request timed out")` |

## See also

- [MCP Integration](mcp-integration.md) — for using conproxy as an stdio MCP server (Claude Desktop, opencode)
- [API Reference](api-reference.md) — for the underlying HTTP/gRPC API
