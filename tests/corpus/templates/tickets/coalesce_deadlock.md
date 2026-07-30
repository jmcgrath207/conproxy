---
topic: coalesce_deadlock
query: "coalesce_deadlock issue"
title: "coalesce deadlock in {product}"
keywords: ["coalesce_deadlock"]
category: bug
---

Environment: {product} running in production with default configuration.
coalesce deadlock occurs intermittently under normal operating conditions.

---

Steps to reproduce:
1. Start {product} with a standard configuration.
2. Execute normal query workload.
3. Observe the coalesce deadlock behavior.
4. The issue appears after approximately 1000 requests.

---

Expected: {product} handles this scenario gracefully without user-visible impact.

---

Actual: {product} exhibits unexpected behavior when coalesce deadlock occurs.
The system does not recover automatically and requires manual intervention.
---
