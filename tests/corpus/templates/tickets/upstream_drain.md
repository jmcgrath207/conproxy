---
topic: upstream_drain
query: "upstream_drain issue"
title: "upstream drain in {product}"
keywords: ["upstream_drain"]
category: bug
---

Environment: {product} running in production with default configuration.
upstream drain occurs intermittently under normal operating conditions.

---

Steps to reproduce:
1. Start {product} with a standard configuration.
2. Execute normal query workload.
3. Observe the upstream drain behavior.
4. The issue appears after approximately 1000 requests.

---

Expected: {product} handles this scenario gracefully without user-visible impact.

---

Actual: {product} exhibits unexpected behavior when upstream drain occurs.
The system does not recover automatically and requires manual intervention.
---
