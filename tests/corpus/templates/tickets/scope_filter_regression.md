---
topic: scope_filter_regression
query: "scope_filter_regression issue"
title: "scope filter regression in {product}"
keywords: ["scope_filter_regression"]
category: bug
---

Environment: {product} running in production with default configuration.
scope filter regression occurs intermittently under normal operating conditions.

---

Steps to reproduce:
1. Start {product} with a standard configuration.
2. Execute normal query workload.
3. Observe the scope filter regression behavior.
4. The issue appears after approximately 1000 requests.

---

Expected: {product} handles this scenario gracefully without user-visible impact.

---

Actual: {product} exhibits unexpected behavior when scope filter regression occurs.
The system does not recover automatically and requires manual intervention.
---
