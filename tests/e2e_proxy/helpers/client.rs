use reqwest::blocking::Client;
use serde_json::Value;
use std::time::Duration;

/// HTTP client wrapper for E2E proxy tests.
pub struct E2eClient {
    client: Client,
    base_url: String,
    api_key: Option<String>,
}

#[allow(dead_code)]
impl E2eClient {
    pub fn new(base_url: &str) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("Failed to build HTTP client");
        Self {
            client,
            base_url: base_url.to_string(),
            api_key: None,
        }
    }

    pub fn with_api_key(mut self, key: &str) -> Self {
        self.api_key = Some(key.to_string());
        self
    }

    fn apply_auth(
        &self,
        req: reqwest::blocking::RequestBuilder,
    ) -> reqwest::blocking::RequestBuilder {
        if let Some(ref key) = self.api_key {
            req.header("X-API-Key", key)
        } else {
            req
        }
    }

    // --- POST helpers ---

    pub fn query(&self, q: &str) -> (u16, Value) {
        self.post_json("/query", &serde_json::json!({"query": q}))
    }

    /// Query scoped to a specific context via the `x-context` header.
    /// The query handler reads `x-context` to resolve the cache namespace,
    /// so this is the correct way to test context isolation.
    pub fn query_with_context(&self, ctx: &str, q: &str) -> (u16, Value) {
        let url = format!("{}{}", self.base_url, "/query");
        let req = self.client.post(&url).json(&serde_json::json!({"query": q}));
        let req = self.apply_auth(req).header("x-context", ctx);
        match req.send() {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let body = resp.json::<Value>().unwrap_or(Value::Null);
                (status, body)
            }
            Err(e) => {
                eprintln!("POST /query failed: {e}");
                (0, Value::Null)
            }
        }
    }

    pub fn batch(&self, queries: &[(&str, &str)]) -> (u16, Value) {
        let qs: Vec<Value> = queries
            .iter()
            .map(|(id, q)| serde_json::json!({"id": id, "query": q}))
            .collect();
        self.post_json("/batch", &serde_json::json!({"queries": qs}))
    }

    pub fn federated(&self, q: &str) -> (u16, Value) {
        self.post_json("/federated", &serde_json::json!({"query": q}))
    }

    pub fn warmup(&self, queries: &[&str]) -> (u16, Value) {
        self.post_json("/cache/warmup", &serde_json::json!({"queries": queries}))
    }

    pub fn warmup_with_fetch(&self, queries: &[&str]) -> (u16, Value) {
        self.post_json(
            "/cache/warmup",
            &serde_json::json!({"queries": queries, "fetch_from_upstream": true}),
        )
    }

    pub fn cache_clear(&self) -> u16 {
        self.post_json("/cache/clear", &serde_json::json!({})).0
    }

    pub fn cache_evict(&self, pattern: &str) -> (u16, Value) {
        self.post_json("/cache/evict", &serde_json::json!({"pattern": pattern}))
    }

    pub fn admin_pause(&self) -> (u16, Value) {
        self.post_json("/admin/pause", &serde_json::json!({}))
    }

    pub fn admin_resume(&self) -> (u16, Value) {
        self.post_json("/admin/resume", &serde_json::json!({}))
    }

    pub fn admin_reload(&self) -> (u16, Value) {
        self.post_json("/admin/reload", &serde_json::json!({}))
    }

    pub fn admin_metrics_reset(&self) -> u16 {
        self.post_json("/admin/metrics/reset", &serde_json::json!({}))
            .0
    }

    pub fn context_switch(&self, ctx: &str) -> (u16, Value) {
        self.post_json("/contexts/switch", &serde_json::json!({"context_id": ctx}))
    }

    // --- GET helpers ---

    pub fn health(&self) -> (u16, Value) {
        self.get_json("/health")
    }

    pub fn ready(&self) -> (u16, Value) {
        self.get_json("/ready")
    }

    pub fn stats(&self) -> (u16, Value) {
        self.get_json("/stats")
    }

    pub fn metrics(&self) -> (u16, Value) {
        self.get_json("/metrics")
    }

    pub fn pool(&self) -> (u16, Value) {
        self.get_json("/pool")
    }

    pub fn circuit(&self) -> (u16, Value) {
        self.get_json("/circuit")
    }

    pub fn queue(&self) -> (u16, Value) {
        self.get_json("/queue")
    }

    pub fn audit(&self) -> (u16, Value) {
        self.get_json("/audit")
    }

    pub fn clients(&self) -> (u16, Value) {
        self.get_json("/clients")
    }

    pub fn contexts(&self) -> (u16, Value) {
        self.get_json("/contexts")
    }

    pub fn contexts_current(&self) -> (u16, Value) {
        self.get_json("/contexts/current")
    }

    pub fn prometheus(&self) -> (u16, String) {
        self.get_text("/metrics/prometheus")
    }

    // --- Agent management ---

    pub fn admin_agents_list(&self) -> (u16, Value) {
        self.get_json("/admin/agents")
    }

    pub fn admin_agents_create(&self, body: &Value) -> (u16, Value) {
        self.post_json("/admin/agents", body)
    }

    pub fn admin_agents_delete(&self, id: &str) -> (u16, Value) {
        self.delete_json(&format!("/admin/agents/{id}"))
    }

    pub fn admin_agents_rotate_key(&self, id: &str, body: &Value) -> (u16, Value) {
        self.post_json(&format!("/admin/agents/{id}/rotate-key"), body)
    }

    // --- Context management ---

    pub fn context_create(&self, body: &Value) -> (u16, Value) {
        self.post_json("/contexts/create", body)
    }

    pub fn context_stats(&self, id: &str) -> (u16, Value) {
        self.get_json(&format!("/contexts/{id}/stats"))
    }

    // --- Cache inspection ---

    pub fn cache_integrity(&self) -> (u16, Value) {
        self.get_json("/cache/integrity")
    }

    pub fn cache_upstreams(&self) -> (u16, Value) {
        self.get_json("/cache/upstreams")
    }

    // --- Federated with local results ---

    pub fn federated_with_local(&self, q: &str, local: &Value) -> (u16, Value) {
        self.post_json(
            "/federated",
            &serde_json::json!({"query": q, "local_results": local}),
        )
    }

    // --- Generic helpers ---

    pub fn post_json(&self, path: &str, body: &Value) -> (u16, Value) {
        let url = format!("{}{}", self.base_url, path);
        let req = self.client.post(&url).json(body);
        let req = self.apply_auth(req);
        match req.send() {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let body = resp.json::<Value>().unwrap_or(Value::Null);
                (status, body)
            }
            Err(e) => {
                eprintln!("POST {path} failed: {e}");
                (0, Value::Null)
            }
        }
    }

    pub fn get_json(&self, path: &str) -> (u16, Value) {
        let url = format!("{}{}", self.base_url, path);
        let req = self.client.get(&url);
        let req = self.apply_auth(req);
        match req.send() {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let body = resp.json::<Value>().unwrap_or(Value::Null);
                (status, body)
            }
            Err(e) => {
                eprintln!("GET {path} failed: {e}");
                (0, Value::Null)
            }
        }
    }

    pub fn get_text(&self, path: &str) -> (u16, String) {
        let url = format!("{}{}", self.base_url, path);
        let req = self.client.get(&url);
        let req = self.apply_auth(req);
        match req.send() {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let body = resp.text().unwrap_or_default();
                (status, body)
            }
            Err(e) => {
                eprintln!("GET {path} failed: {e}");
                (0, String::new())
            }
        }
    }

    pub fn delete_json(&self, path: &str) -> (u16, Value) {
        let url = format!("{}{}", self.base_url, path);
        let req = self.client.delete(&url);
        let req = self.apply_auth(req);
        match req.send() {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let body = resp.json::<Value>().unwrap_or(Value::Null);
                (status, body)
            }
            Err(e) => {
                eprintln!("DELETE {path} failed: {e}");
                (0, Value::Null)
            }
        }
    }

    pub fn get_status(&self, path: &str) -> u16 {
        let url = format!("{}{}", self.base_url, path);
        let req = self.client.get(&url);
        let req = self.apply_auth(req);
        match req.send() {
            Ok(resp) => resp.status().as_u16(),
            Err(_) => 0,
        }
    }

    /// Send a POST query without auth headers (used for auth-rejection tests).
    pub fn query_no_auth(&self, q: &str) -> (u16, Value) {
        let url = format!("{}/query", self.base_url);
        match self
            .client
            .post(&url)
            .json(&serde_json::json!({"query": q}))
            .send()
        {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let body = resp.json::<Value>().unwrap_or(Value::Null);
                (status, body)
            }
            Err(e) => {
                eprintln!("POST /query (no-auth) failed: {e}");
                (0, Value::Null)
            }
        }
    }

    /// Send a POST query with a specific API key header (used for wrong-key tests).
    pub fn query_with_key(&self, q: &str, key: &str) -> (u16, Value) {
        let url = format!("{}/query", self.base_url);
        match self
            .client
            .post(&url)
            .header("X-API-Key", key)
            .json(&serde_json::json!({"query": q}))
            .send()
        {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let body = resp.json::<Value>().unwrap_or(Value::Null);
                (status, body)
            }
            Err(e) => {
                eprintln!("POST /query (key={key}) failed: {e}");
                (0, Value::Null)
            }
        }
    }

    /// Send a POST query with a Bearer authorization header.
    pub fn query_with_bearer(&self, q: &str, token: &str) -> (u16, Value) {
        let url = format!("{}/query", self.base_url);
        match self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .json(&serde_json::json!({"query": q}))
            .send()
        {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let body = resp.json::<Value>().unwrap_or(Value::Null);
                (status, body)
            }
            Err(e) => {
                eprintln!("POST /query (bearer) failed: {e}");
                (0, Value::Null)
            }
        }
    }

    /// Check if the proxy is responding.
    pub fn is_up(&self) -> bool {
        self.get_status("/health") == 200
    }
}
