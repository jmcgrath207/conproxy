# conproxy

Retrieval-leg cache for **agentic RAG**. Native Rust bindings: in-process `Engine` or gRPC `ConproxyClient`.

Not an LLM-answer cache (that's GPTCache / RedisVL). Hits skip embed + upstream search when agents re-query.

```bash
pip install conproxy
```

```python
from conproxy import Engine

engine = Engine(config="conproxy.toml")
result = await engine.query("how does X work", top_k=10)
# result.cache_status: 1=hit, 2=miss, 3=stale, 4=frozen
```

Talk to a running daemon instead:

```python
from conproxy import ConproxyClient

client = ConproxyClient(grpc_url="http://localhost:9999")
resp = client.query("how does X work", top_k=5)
```

Extras: `pip install conproxy[langchain]` · `pip install conproxy[llama-index]`

- Docs: [Python SDK](https://github.com/jmcgrath207/conproxy/blob/main/docs/sdk-python.md) · [Engine](https://github.com/jmcgrath207/conproxy/blob/main/docs/engine.md)
- Source: [github.com/jmcgrath207/conproxy](https://github.com/jmcgrath207/conproxy)
