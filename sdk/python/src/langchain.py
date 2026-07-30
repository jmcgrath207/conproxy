"""
LangChain retriever for conproxy.

Adapter exposing :class:`ConproxyClient` as a LangChain ``BaseRetriever``.
Install with ``pip install conproxy[langchain]`` and use directly in any
LangChain RAG pipeline.

Example:

    >>> from conproxy_py.langchain import ConproxyRetriever
    >>> retriever = ConproxyRetriever(
    ...     grpc_url="http://localhost:9999",
    ...     api_key="my-secret",
    ...     top_k=5,
    ... )
    >>> docs = retriever.invoke("how does conproxy handle cache misses?")
    >>> for d in docs:
    ...     print(d.metadata["score"], d.page_content[:80])
"""
from __future__ import annotations

import json
from typing import Any, List, Optional

try:
    from langchain_core.callbacks import CallbackManagerForRetrieverRun
    from langchain_core.documents import Document
    from langchain_core.retrievers import BaseRetriever
    from pydantic import ConfigDict
except ImportError as e:  # pragma: no cover - import-time guard
    raise ImportError(
        "langchain-core is required for the LangChain adapter. "
        "Install with: pip install conproxy[langchain]"
    ) from e

from conproxy_py import ConproxyClient


def _result_to_document(result: Any) -> Document:
    """Convert a ``PySearchResult`` into a LangChain ``Document``.

    ``score``, ``id``, and ``upstream_id`` are always copied into metadata.
    ``metadata_json`` (a JSON-encoded string) is parsed and merged when
    present; raw text is preserved under ``raw_metadata`` on parse failure.
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
    return Document(page_content=result.content, metadata=metadata)


class ConproxyRetriever(BaseRetriever):
    model_config = ConfigDict(arbitrary_types_allowed=True)
    """LangChain retriever backed by conproxy.

    The retriever lazily constructs a single :class:`ConproxyClient` on first
    use and reuses it for subsequent calls. Both sync (``invoke``) and async
    (``ainvoke``) paths are implemented.

    Parameters
    ----------
    grpc_url:
        ``"host:port"`` of the conproxy gRPC server. If omitted, the client
        reads ``~/.conproxy/sdk.toml`` via :class:`ConproxyClient`'s default
        config loader.
    api_key:
        Optional API key for authenticated backends.
    top_k:
        Default number of documents to return per query (default ``10``).
    client:
        Optional pre-constructed :class:`ConproxyClient`. Useful for sharing
        a client across multiple retrievers in the same process.
    """

    grpc_url: Optional[str] = None
    api_key: Optional[str] = None
    top_k: int = 10
    client: Optional[Any] = None

    def _get_client(self) -> ConproxyClient:
        """Return the shared client, creating it on first use."""
        if self.client is None:
            self.client = ConproxyClient(self.grpc_url, self.api_key)
        return self.client

    def _get_relevant_documents(
        self,
        query: str,
        *,
        run_manager: Optional[CallbackManagerForRetrieverRun] = None,
    ) -> List[Document]:
        """Synchronous retrieval — called by ``BaseRetriever.invoke``."""
        resp = self._get_client().query(query, self.top_k)
        return [_result_to_document(r) for r in resp.results]

    async def _aget_relevant_documents(
        self,
        query: str,
        *,
        run_manager: Optional[CallbackManagerForRetrieverRun] = None,
    ) -> List[Document]:
        """Async retrieval — called by ``BaseRetriever.ainvoke``.

        Uses the SDK's async gRPC binding, so this never blocks the
        event loop while waiting for the proxy.
        """
        resp = await self._get_client().query_async(query, self.top_k)
        return [_result_to_document(r) for r in resp.results]
