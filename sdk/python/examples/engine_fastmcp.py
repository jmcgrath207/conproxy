"""FastMCP-style tool using in-process Engine. Always await query()."""

from conproxy import Engine

engine = Engine(config="conproxy.toml")


async def search_docs(query: str, limit: int = 10):
    return await engine.query(query, top_k=limit)
