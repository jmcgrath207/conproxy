---
topic: cascade_exhaustion
query: "cascade_exhaustion issue"
title: "cascade exhaustion in {product}"
keywords: ["cascade_exhaustion"]
category: bug
---

Environment: {product} running in production with default configuration.
cascade exhaustion occurs intermittently under normal operating conditions.

---

Steps to reproduce:
1. Start {product} with a standard configuration.
2. Execute normal query workload.
3. Observe the cascade exhaustion behavior.
4. The issue appears after approximately 1000 requests.

---

Expected: {product} handles this scenario gracefully without user-visible impact.

---

Actual: {product} exhibits unexpected behavior when cascade exhaustion occurs.
The system does not recover automatically and requires manual intervention.
---
