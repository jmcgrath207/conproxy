---
topic: health_check_failure
query: "health_check_failure issue"
title: "health check failure in {product}"
keywords: ["health_check_failure"]
category: bug
---

Environment: {product} running in production with default configuration.
health check failure occurs intermittently under normal operating conditions.

---

Steps to reproduce:
1. Start {product} with a standard configuration.
2. Execute normal query workload.
3. Observe the health check failure behavior.
4. The issue appears after approximately 1000 requests.

---

Expected: {product} handles this scenario gracefully without user-visible impact.

---

Actual: {product} exhibits unexpected behavior when health check failure occurs.
The system does not recover automatically and requires manual intervention.
---
