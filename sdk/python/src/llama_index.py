"""
LlamaIndex retriever for conproxy.

Adapter exposing :class:`ConproxyClient` as a LlamaIndex ``BaseRetriever``.
Install with ``pip install conproxy[llama-index]`` and use directly in any
LlamaIndex RAG query engine.

Example:

    >>> from conproxy_py.llama_index import ConproxyRetriever
    >>> retriever = ConproxyRetriever(
    ...     grpc_url="http://localhost:9999",
    ...     api_key="my-secret",
    ...     top_k=5,
    ... )
    >>> from llama_index.core.query_engine import RetrieverQueryEngine
    >>> engine = RetrieverQueryEngine.from_args(retriever)
    >>> response = engine.query("how does conproxy handle cache misses?")
    >>> print(response)
"""
from __future__ import annotations

import json
from typing import Any, List, Optional

try:
    from llama_index.core import QueryBundle
    from llama_index.core.retrievers import BaseRetriever
    from llama_index.core.schema import NodeWithScore, TextNode
except ImportError as e:  # pragma: no cover - import-time guard
    raise ImportError(
        "llama-index-core is required for the LlamaIndex adapter. "
        "Install with: pip install conproxy[llama-index]"
    ) from e

from conproxy_py import ConproxyClient


def _result_to_node(result: Any) -> NodeWithScore:
    """Convert a ``PySearchResult`` into a ``NodeWithScore(TextNode)``.

    ``score``, ``id``, and ``upstream_id`` are always copied into the
    node's metadata. ``metadata_json`` (a JSON-encoded string) is parsed
    and merged when present; raw text is preserved under ``raw_metadata``
    on parse failure.
    """
    metadata: dict[str, Any] = {
        "score": result.score,
        "id": result.id,
        "upstream_id": result.upstream_id,
    }
    raw = result.metadata_json
    if raw:
        try:
            metadata.update(json.loads(raw))
        except (json.JSONDecodeError, TypeError):
            metadata["raw_metadata"] = raw
    node = TextNode(id_=result.id, text=result.content, metadata=metadata)
    return NodeWithScore(node=node, score=result.score)


class ConproxyRetriever(BaseRetriever):
    """LlamaIndex retriever backed by conproxy.

    The retriever lazily constructs a single :class:`ConproxyClient` on first
    use and reuses it for subsequent calls. Both sync (``retrieve``) and
    async (``aretrieve``) paths are implemented.

    Parameters
    ----------
    grpc_url:
        ``"host:port"`` of the conproxy gRPC server. If omitted, the client
        reads ``~/.conproxy/sdk.toml`` via :class:`ConproxyClient`'s default
        config loader.
    api_key:
        Optional API key for authenticated backends.
    top_k:
        Default number of nodes to return per query (default ``10``).
    """

    def __init__(
        self,
        grpc_url: Optional[str] = None,
        api_key: Optional[str] = None,
        top_k: int = 10,
        **kwargs: Any,
    ) -> None:
        super().__init__(**kwargs)
        self._grpc_url = grpc_url
        self._api_key = api_key
        self._top_k = top_k
        self._client: Optional[ConproxyClient] = None

    def _retrieve(self, query_bundle: Any) -> List[NodeWithScore]:
        """Synchronous retrieval — called by ``BaseRetriever.retrieve``."""
        client = self._get_client()
        resp = client.query(query_bundle.query_str, self._top_k)
        return [_result_to_node(r) for r in resp.results]

    async def _aretrieve(self, query_bundle: Any) -> List[NodeWithScore]:
        """Async retrieval — called by ``BaseRetriever.aretrieve``.

        Uses the SDK's async gRPC binding, so this never blocks the
        event loop while waiting for the proxy.
        """
        client = self._get_client()
        resp = await client.query_async(query_bundle.query_str, self._top_k)
        return [_result_to_node(r) for r in resp.results]

    def _get_client(self) -> ConproxyClient:
        """Return the shared client, creating it on first use."""
        if self._client is None:
            self._client = ConproxyClient(self._grpc_url, self._api_key)
        return self._client
