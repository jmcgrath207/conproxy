#!/usr/bin/env python3
"""Smoke-test the in-process Engine path of the built conproxy Python SDK.

Constructs an Engine off-loop, binds a dashboard on an ephemeral port, hits
`/health`, and closes. Called by `make sdk-smoke` with the venv python so the
just-built wheel is importable.
"""

import json
import os
import sys
import tempfile
import urllib.request

import conproxy


def main() -> int:
    cfg = tempfile.NamedTemporaryFile(mode="w", suffix=".toml", delete=False)
    cfg.write('[proxy]\nupstream_url = "http://127.0.0.1:9"\n')
    cfg.close()
    try:
        engine = conproxy.Engine(config=cfg.name, dashboard_listen="127.0.0.1:0")
        try:
            addr = engine.dashboard_addr()
            if not addr:
                print("FAIL: engine.dashboard_addr() is None")
                return 1
            with urllib.request.urlopen(f"http://{addr}/health", timeout=5) as r:
                assert r.status == 200, f"health status {r.status}"
                health = json.loads(r.read())
                print(f"engine dashboard /health: {r.status} {health}")
        finally:
            engine.close()
    finally:
        os.unlink(cfg.name)
    print("engine construct + dashboard + close: OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
