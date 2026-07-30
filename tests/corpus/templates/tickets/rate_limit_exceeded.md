---
topic: rate_limit_exceeded
query: "rate_limit_exceeded issue"
title: "rate limit exceeded in {product}"
keywords: ["rate_limit_exceeded"]
category: bug
---

Environment: {product} running in production with default configuration.
rate limit exceeded occurs intermittently under normal operating conditions.

---

Steps to reproduce:
1. Start {product} with a standard configuration.
2. Execute normal query workload.
3. Observe the rate limit exceeded behavior.
4. The issue appears after approximately 1000 requests.

---

Expected: {product} handles this scenario gracefully without user-visible impact.

---

Actual: {product} exhibits unexpected behavior when rate limit exceeded occurs.
The system does not recover automatically and requires manual intervention.
---
