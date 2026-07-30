---
topic: performance_regression
query: "performance_regression issue"
title: "performance regression in {product}"
keywords: ["performance_regression"]
category: bug
---

Environment: {product} running in production with default configuration.
performance regression occurs intermittently under normal operating conditions.

---

Steps to reproduce:
1. Start {product} with a standard configuration.
2. Execute normal query workload.
3. Observe the performance regression behavior.
4. The issue appears after approximately 1000 requests.

---

Expected: {product} handles this scenario gracefully without user-visible impact.

---

Actual: {product} exhibits unexpected behavior when performance regression occurs.
The system does not recover automatically and requires manual intervention.
---
