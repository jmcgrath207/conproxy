---
topic: peer_sync_stall
query: "peer_sync_stall issue"
title: "peer sync stall in {product}"
keywords: ["peer_sync_stall"]
category: bug
---

Environment: {product} running in production with default configuration.
peer sync stall occurs intermittently under normal operating conditions.

---

Steps to reproduce:
1. Start {product} with a standard configuration.
2. Execute normal query workload.
3. Observe the peer sync stall behavior.
4. The issue appears after approximately 1000 requests.

---

Expected: {product} handles this scenario gracefully without user-visible impact.

---

Actual: {product} exhibits unexpected behavior when peer sync stall occurs.
The system does not recover automatically and requires manual intervention.
---
