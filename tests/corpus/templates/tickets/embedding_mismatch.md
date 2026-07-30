---
topic: embedding_mismatch
query: "embedding_mismatch issue"
title: "embedding mismatch in {product}"
keywords: ["embedding_mismatch"]
category: bug
---

Environment: {product} running in production with default configuration.
embedding mismatch occurs intermittently under normal operating conditions.

---

Steps to reproduce:
1. Start {product} with a standard configuration.
2. Execute normal query workload.
3. Observe the embedding mismatch behavior.
4. The issue appears after approximately 1000 requests.

---

Expected: {product} handles this scenario gracefully without user-visible impact.

---

Actual: {product} exhibits unexpected behavior when embedding mismatch occurs.
The system does not recover automatically and requires manual intervention.
---
