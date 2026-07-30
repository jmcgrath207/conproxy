---
topic: metrics_drift
query: "metrics_drift issue"
title: "metrics drift in {product}"
keywords: ["metrics_drift"]
category: bug
---

Environment: {product} running in production with default configuration.
metrics drift occurs intermittently under normal operating conditions.

---

Steps to reproduce:
1. Start {product} with a standard configuration.
2. Execute normal query workload.
3. Observe the metrics drift behavior.
4. The issue appears after approximately 1000 requests.

---

Expected: {product} handles this scenario gracefully without user-visible impact.

---

Actual: {product} exhibits unexpected behavior when metrics drift occurs.
The system does not recover automatically and requires manual intervention.
---
