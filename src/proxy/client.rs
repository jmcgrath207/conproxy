//! Proxy client for external applications.
//!
//! Provides a gRPC client to interact with the cache proxy server.
//! Uses tonic for gRPC (query, stats, admin) and reqwest for HTTP health checks.
//!
//! This can be used by applications that want to query the proxy without
//! going through the CLI or MCP server.

use std::time::Duration;

use super::grpc::proto;
use super::types::{QueryRequest, QueryResponse};

/// Error type for proxy client operations.
#[derive(Debug)]
pub enum ClientError {
    /// Failed to connect to the proxy.
    Connection(String),
    /// Request failed with an error status.
    Request(u16, String),
    /// Failed to parse response.
    Parse(String),
    /// Request timed out.
    Timeout,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Connection(msg) => write!(f, "Connection error: {}", msg),
            ClientError::Request(status, msg) => write!(f, "Request error ({}): {}", status, msg),
            ClientError::Parse(msg) => write!(f, "Parse error: {}", msg),
            ClientError::Timeout => write!(f, "Request timed out"),
        }
    }
}

impl std::error::Error for ClientError {}

/// Map tonic status codes to HTTP-equivalent status codes.
fn tonic_status_to_http(code: tonic::Code) -> u16 {
    match code {
        tonic::Code::Ok => 200,
        tonic::Code::InvalidArgument => 400,
        tonic::Code::Unauthenticated => 401,
        tonic::Code::PermissionDenied => 403,
        tonic::Code::NotFound => 404,
        tonic::Code::ResourceExhausted => 429,
        tonic::Code::Internal => 500,
        tonic::Code::Unavailable => 503,
        tonic::Code::DeadlineExceeded => 504,
        _ => 500,
    }
}

/// Configuration for the proxy client.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// gRPC endpoint URL (e.g., "http://127.0.0.1:9999").
    pub base_url: String,
    /// HTTP health/metrics URL (e.g., "http://127.0.0.1:10000").
    /// Defaults to gRPC port + 1 if not set.
    pub http_url: Option<String>,
    /// Request timeout.
    pub timeout: Duration,
    /// Optional API key for authentication.
    pub api_key: Option<String>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:9999".to_string(),
            http_url: None,
            timeout: Duration::from_secs(30),
            api_key: None,
        }
    }
}

impl ClientConfig {
    /// Create a new config with the given gRPC endpoint URL.
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            ..Default::default()
        }
    }

    /// Set the timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Set the API key.
    pub fn with_api_key(mut self, api_key: &str) -> Self {
        self.api_key = Some(api_key.to_string());
        self
    }

    /// Set the HTTP health URL.
    pub fn with_http_url(mut self, http_url: &str) -> Self {
        self.http_url = Some(http_url.to_string());
        self
    }

    /// Get the HTTP health URL, deriving from gRPC port + 1 if not set.
    fn http_base_url(&self) -> String {
        if let Some(ref url) = self.http_url {
            return url.clone();
        }
        // Derive from base_url: increment port by 1
        if let Some(colon_pos) = self.base_url.rfind(':') {
            let port_str = self
                .base_url
                .get(colon_pos.saturating_add(1)..)
                .unwrap_or("");
            if let Ok(port) = port_str.parse::<u16>() {
                return format!("http://127.0.0.1:{}", port.saturating_add(1));
            }
        }
        self.base_url.to_string()
    }
}

/// gRPC client for the cache proxy.
pub struct ProxyClient {
    search_client: proto::search_service_client::SearchServiceClient<tonic::transport::Channel>,
    admin_client: proto::admin_service_client::AdminServiceClient<tonic::transport::Channel>,
    obs_client:
        proto::observability_service_client::ObservabilityServiceClient<tonic::transport::Channel>,
    http_client: reqwest::Client,
    config: ClientConfig,
}

impl ProxyClient {
    /// Create a new proxy client with the given configuration.
    ///
    /// Uses lazy connection — the gRPC channel connects on first use.
    pub fn new(config: ClientConfig) -> Result<Self, ClientError> {
        let endpoint = tonic::transport::Endpoint::from_shared(config.base_url.clone())
            .map_err(|e| ClientError::Connection(format!("Invalid endpoint: {}", e)))?
            .timeout(config.timeout);

        let channel = endpoint.connect_lazy();

        let mut search_client =
            proto::search_service_client::SearchServiceClient::new(channel.clone());
        let mut admin_client =
            proto::admin_service_client::AdminServiceClient::new(channel.clone());
        let mut obs_client =
            proto::observability_service_client::ObservabilityServiceClient::new(channel);

        // Apply timeout to all clients
        let timeout = config.timeout;
        search_client = search_client.max_decoding_message_size(16 * 1024 * 1024);
        admin_client = admin_client.max_decoding_message_size(16 * 1024 * 1024);
        obs_client = obs_client.max_decoding_message_size(16 * 1024 * 1024);

        let http_client = super::socket_tuning::create_tuned_client_default(timeout)
            .build()
            .map_err(|e| ClientError::Connection(e.to_string()))?;

        Ok(Self {
            search_client,
            admin_client,
            obs_client,
            http_client,
            config,
        })
    }

    /// Create a client connected to the default local proxy.
    pub fn local() -> Result<Self, ClientError> {
        Self::new(ClientConfig::default())
    }

    /// Create a client connected to a specific gRPC endpoint URL.
    pub fn connect(base_url: &str) -> Result<Self, ClientError> {
        Self::new(ClientConfig::new(base_url))
    }

    /// Build gRPC request with optional API key metadata.
    fn make_request<T>(&self, inner: T) -> tonic::Request<T> {
        let mut request = tonic::Request::new(inner);
        if let Some(ref api_key) = self.config.api_key {
            if let Ok(value) = api_key.parse() {
                request.metadata_mut().insert("x-api-key", value);
            }
        }
        request
    }

    /// Execute a query against the proxy via gRPC.
    pub async fn query(&self, request: &QueryRequest) -> Result<QueryResponse, ClientError> {
        let proto_req = proto::QueryRequest::from(request);
        let grpc_request = self.make_request(proto_req);

        let response = self
            .search_client
            .clone()
            .query(grpc_request)
            .await
            .map_err(|status| {
                let code = tonic_status_to_http(status.code());
                ClientError::Request(code, status.message().to_string())
            })?;

        Ok(QueryResponse::from(response.into_inner()))
    }

    /// Execute a simple text query.
    pub async fn search(&self, query: &str, limit: usize) -> Result<QueryResponse, ClientError> {
        let request = QueryRequest {
            query: query.to_string(),
            top_k: Some(limit),
            priority: None,
            upstream_id: None,
            upstream_type: None,
        };
        self.query(&request).await
    }

    /// Check if the proxy is healthy (via HTTP health endpoint).
    pub async fn health(&self) -> Result<bool, ClientError> {
        let url = format!(
            "{}/health",
            self.config.http_base_url().trim_end_matches('/')
        );

        let response = self
            .http_client
            .get(&url)
            .send()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;

        Ok(response.status().is_success())
    }

    /// Check if the proxy is ready to serve requests (via HTTP readiness endpoint).
    pub async fn ready(&self) -> Result<bool, ClientError> {
        let url = format!(
            "{}/ready",
            self.config.http_base_url().trim_end_matches('/')
        );

        let response = self
            .http_client
            .get(&url)
            .send()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;

        Ok(response.status().is_success())
    }

    /// Get cache statistics via gRPC.
    pub async fn stats(&self) -> Result<serde_json::Value, ClientError> {
        let grpc_request = self.make_request(proto::GetStatsRequest {});

        let response = self
            .obs_client
            .clone()
            .get_stats(grpc_request)
            .await
            .map_err(|status| {
                let code = tonic_status_to_http(status.code());
                ClientError::Request(code, status.message().to_string())
            })?;

        let stats = response.into_inner();
        let json = serde_json::json!({
            "uptime_secs": stats.uptime_secs,
            "cache_entries": stats.cache_entries,
            "total_hits": stats.total_hits,
            "total_misses": stats.total_misses,
            "hit_rate": stats.hit_rate,
            "upstream_requests": stats.upstream_requests,
            "upstream_failures": stats.upstream_failures,
            "upstream_error_rate": stats.upstream_error_rate,
            "degradation_level": stats.degradation_level,
            "paused": stats.paused,
        });

        Ok(json)
    }

    /// Clear the cache via gRPC.
    pub async fn clear_cache(&self) -> Result<(), ClientError> {
        let grpc_request = self.make_request(proto::CacheClearRequest {});

        self.admin_client
            .clone()
            .cache_clear(grpc_request)
            .await
            .map_err(|status| {
                let code = tonic_status_to_http(status.code());
                ClientError::Request(code, status.message().to_string())
            })?;

        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/client_tests.rs"]
mod tests;
