---
topic: warmup_timeout
query: "warmup_timeout issue"
title: "warmup timeout in {product}"
keywords: ["warmup_timeout"]
category: bug
---

Environment: {product} running in production with default configuration.
warmup timeout occurs intermittently under normal operating conditions.

---

Steps to reproduce:
1. Start {product} with a standard configuration.
2. Execute normal query workload.
3. Observe the warmup timeout behavior.
4. The issue appears after approximately 1000 requests.

---

Expected: {product} handles this scenario gracefully without user-visible impact.

---

Actual: {product} exhibits unexpected behavior when warmup timeout occurs.
The system does not recover automatically and requires manual intervention.
---
