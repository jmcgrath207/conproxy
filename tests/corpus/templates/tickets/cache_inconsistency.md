---
topic: cache_inconsistency
query: "cache_inconsistency issue"
title: "cache inconsistency in {product}"
keywords: ["cache_inconsistency"]
category: bug
---

Environment: {product} running in production with default configuration.
cache inconsistency occurs intermittently under normal operating conditions.

---

Steps to reproduce:
1. Start {product} with a standard configuration.
2. Execute normal query workload.
3. Observe the cache inconsistency behavior.
4. The issue appears after approximately 1000 requests.

---

Expected: {product} handles this scenario gracefully without user-visible impact.

---

Actual: {product} exhibits unexpected behavior when cache inconsistency occurs.
The system does not recover automatically and requires manual intervention.
---
