---
topic: data_loss
query: "data_loss issue"
title: "data loss in {product}"
keywords: ["data_loss"]
category: bug
---

Environment: {product} running in production with default configuration.
data loss occurs intermittently under normal operating conditions.

---

Steps to reproduce:
1. Start {product} with a standard configuration.
2. Execute normal query workload.
3. Observe the data loss behavior.
4. The issue appears after approximately 1000 requests.

---

Expected: {product} handles this scenario gracefully without user-visible impact.

---

Actual: {product} exhibits unexpected behavior when data loss occurs.
The system does not recover automatically and requires manual intervention.
---
