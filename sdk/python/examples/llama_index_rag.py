"""
LlamaIndex RAG example with conproxy as the retriever.

This script demonstrates a minimal Retrieval-Augmented Generation (RAG)
query engine using conproxy as the document retriever and an OpenAI chat
model for the answer-synthesis step.

Prerequisites:
    pip install conproxy[llama-index] llama-index-llms-openai

Environment:
    OPENAI_API_KEY     API key for the chat model.
    CONPROXY_GRPC_URL  gRPC endpoint of the proxy (default: localhost:9999).
    CONPROXY_API_KEY   Optional API key if the proxy enforces auth.

Run with:
    python llama_index_rag.py
"""
from __future__ import annotations

import os

from conproxy_py.llama_index import ConproxyRetriever


def build_retriever() -> ConproxyRetriever:
    """Construct a conproxy-backed LlamaIndex retriever from env vars."""
    return ConproxyRetriever(
        grpc_url=os.environ.get("CONPROXY_GRPC_URL", "localhost:9999"),
        api_key=os.environ.get("CONPROXY_API_KEY"),
        top_k=4,
    )


def main() -> None:
    # Lazy import so the script is usable without llama-index-llms-openai
    # installed (the retriever adapter itself only requires
    # llama-index-core).
    from llama_index.core.query_engine import RetrieverQueryEngine
    from llama_index.core import Settings
    from llama_index.llms.openai import OpenAI

    Settings.llm = OpenAI(model="gpt-4o-mini", temperature=0)

    retriever = build_retriever()
    engine = RetrieverQueryEngine.from_args(retriever)

    question = "How does conproxy decide between cache HIT and MISS?"
    response = engine.query(question)
    print(f"Q: {question}\nA: {response}")


if __name__ == "__main__":
    main()
