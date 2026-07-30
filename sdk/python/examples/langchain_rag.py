"""
LangChain RAG example with conproxy as the retriever.

This script demonstrates a minimal Retrieval-Augmented Generation (RAG)
pipeline using conproxy as the document retriever and an OpenAI chat
model for the answer-generation step.

Prerequisites:
    pip install conproxy[langchain] langchain-openai

Environment:
    OPENAI_API_KEY    API key for the chat model.
    CONPROXY_GRPC_URL gRPC endpoint of the proxy (default: localhost:9999).
    CONPROXY_API_KEY  Optional API key if the proxy enforces auth.

Run with:
    python langchain_rag.py
"""
from __future__ import annotations

import os

from conproxy_py.langchain import ConproxyRetriever


def build_retriever() -> ConproxyRetriever:
    """Construct a conproxy-backed LangChain retriever from env vars."""
    return ConproxyRetriever(
        grpc_url=os.environ.get("CONPROXY_GRPC_URL", "localhost:9999"),
        api_key=os.environ.get("CONPROXY_API_KEY"),
        top_k=4,
    )


def main() -> None:
    # Lazy import so the script is usable without langchain-openai installed
    # (the retriever adapter itself only requires langchain-core).
    from langchain_core.prompts import ChatPromptTemplate
    from langchain_core.runnables import RunnablePassthrough
    from langchain_core.output_parsers import StrOutputParser
    from langchain_openai import ChatOpenAI

    retriever = build_retriever()
    llm = ChatOpenAI(model="gpt-4o-mini", temperature=0)

    prompt = ChatPromptTemplate.from_template(
        "Answer the question based only on the context below.\n\n"
        "Context:\n{context}\n\n"
        "Question: {question}\n\n"
        "Answer:"
    )

    def format_docs(docs):
        return "\n\n".join(d.page_content for d in docs)

    chain = (
        {"context": retriever | format_docs, "question": RunnablePassthrough()}
        | prompt
        | llm
        | StrOutputParser()
    )

    question = "How does conproxy decide between cache HIT and MISS?"
    answer = chain.invoke(question)
    print(f"Q: {question}\nA: {answer}")


if __name__ == "__main__":
    main()
