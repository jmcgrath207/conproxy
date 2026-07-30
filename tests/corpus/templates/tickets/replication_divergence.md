---
topic: replication_divergence
query: "replication_divergence issue"
title: "replication divergence in {product}"
keywords: ["replication_divergence"]
category: bug
---

Environment: {product} running in production with default configuration.
replication divergence occurs intermittently under normal operating conditions.

---

Steps to reproduce:
1. Start {product} with a standard configuration.
2. Execute normal query workload.
3. Observe the replication divergence behavior.
4. The issue appears after approximately 1000 requests.

---

Expected: {product} handles this scenario gracefully without user-visible impact.

---

Actual: {product} exhibits unexpected behavior when replication divergence occurs.
The system does not recover automatically and requires manual intervention.
---
