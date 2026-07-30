---
topic: memory_leak
query: "memory_leak issue"
title: "memory leak in {product}"
keywords: ["memory_leak"]
category: bug
---

Environment: {product} running in production with default configuration.
memory leak occurs intermittently under normal operating conditions.

---

Steps to reproduce:
1. Start {product} with a standard configuration.
2. Execute normal query workload.
3. Observe the memory leak behavior.
4. The issue appears after approximately 1000 requests.

---

Expected: {product} handles this scenario gracefully without user-visible impact.

---

Actual: {product} exhibits unexpected behavior when memory leak occurs.
The system does not recover automatically and requires manual intervention.
---
