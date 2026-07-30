use tonic::transport::Channel;

use crate::config::SdkConfig;
use crate::error::SdkError;
use crate::proto::{
    admin_service_client::AdminServiceClient, context_service_client::ContextServiceClient,
    observability_service_client::ObservabilityServiceClient,
    search_service_client::SearchServiceClient,
};

const MAX_DECODE_SIZE: usize = 16 * 1024 * 1024; // 16 MB

/// Main SDK client for interacting with a conproxy instance.
///
/// Provides methods for all four gRPC services: Search, Admin, Observability, and Context.
/// Uses lazy connection — the gRPC channel connects on first use.
pub struct ConproxyClient {
    pub(crate) config: SdkConfig,
    pub(crate) search: SearchServiceClient<Channel>,
    pub(crate) admin: AdminServiceClient<Channel>,
    pub(crate) obs: ObservabilityServiceClient<Channel>,
    pub(crate) ctx: ContextServiceClient<Channel>,
}

impl ConproxyClient {
    /// Create a new client with the given configuration.
    pub fn new(config: SdkConfig) -> Result<Self, SdkError> {
        let endpoint = tonic::transport::Endpoint::from_shared(config.grpc_url.clone())
            .map_err(|e| SdkError::Connection(format!("invalid endpoint: {}", e)))?
            .timeout(config.timeout);

        let channel = endpoint.connect_lazy();

        let search =
            SearchServiceClient::new(channel.clone()).max_decoding_message_size(MAX_DECODE_SIZE);
        let admin =
            AdminServiceClient::new(channel.clone()).max_decoding_message_size(MAX_DECODE_SIZE);
        let obs = ObservabilityServiceClient::new(channel.clone())
            .max_decoding_message_size(MAX_DECODE_SIZE);
        let ctx = ContextServiceClient::new(channel).max_decoding_message_size(MAX_DECODE_SIZE);

        Ok(Self {
            config,
            search,
            admin,
            obs,
            ctx,
        })
    }

    /// Create a client using auto-discovered configuration from conproxy.toml.
    pub fn from_config() -> Result<Self, SdkError> {
        let config = SdkConfig::load()?;
        Self::new(config)
    }

    /// Create a client connected to a specific gRPC endpoint URL.
    pub fn connect(grpc_url: &str) -> Result<Self, SdkError> {
        let config = SdkConfig::builder().grpc_url(grpc_url).build();
        Self::new(config)
    }
}
