use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::error::SdkError;

/// SDK configuration for connecting to a conproxy instance.
#[derive(Debug, Clone)]
pub struct SdkConfig {
    /// gRPC endpoint URL (e.g., "http://127.0.0.1:9999").
    pub grpc_url: String,
    /// HTTP endpoint URL (e.g., "http://127.0.0.1:10000").
    pub http_url: String,
    /// Optional API key for authentication.
    pub api_key: Option<String>,
    /// Request timeout.
    pub timeout: Duration,
}

impl Default for SdkConfig {
    fn default() -> Self {
        Self {
            grpc_url: "http://127.0.0.1:9999".to_string(),
            http_url: "http://127.0.0.1:10000".to_string(),
            api_key: None,
            timeout: Duration::from_secs(30),
        }
    }
}

impl SdkConfig {
    /// Auto-discover configuration from conproxy.toml files.
    ///
    /// Discovery order:
    /// 1. Local: `.conproxy/conproxy.toml` (in CWD)
    /// 2. Global: `~/.conproxy/conproxy.toml`
    /// 3. Merge: local overrides global
    pub fn load() -> Result<Self, SdkError> {
        let global = Self::load_file_opt(&Self::global_config_path());
        let local = Self::load_file_opt(&Self::local_config_path());

        let merged = match (global, local) {
            (Some(g), Some(l)) => g.merge(l),
            (Some(g), None) => g,
            (None, Some(l)) => l,
            (None, None) => return Ok(Self::default()),
        };

        Ok(Self::from_config_file(merged))
    }

    /// Load configuration from a specific file path.
    pub fn from_file(path: &Path) -> Result<Self, SdkError> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| SdkError::Config(format!("failed to read {}: {}", path.display(), e)))?;
        let config: ConfigFile = toml::from_str(&content)
            .map_err(|e| SdkError::Config(format!("failed to parse {}: {}", path.display(), e)))?;
        Ok(Self::from_config_file(config))
    }

    /// Create a builder for programmatic configuration.
    pub fn builder() -> SdkConfigBuilder {
        SdkConfigBuilder::default()
    }

    fn load_file_opt(path: &Path) -> Option<ConfigFile> {
        let content = std::fs::read_to_string(path).ok()?;
        toml::from_str(&content).ok()
    }

    fn global_config_path() -> PathBuf {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        home.join(".conproxy").join("conproxy.toml")
    }

    fn local_config_path() -> PathBuf {
        PathBuf::from(".conproxy").join("conproxy.toml")
    }

    fn from_config_file(config: ConfigFile) -> Self {
        let grpc_url = config
            .proxy
            .listen
            .unwrap_or_else(|| "http://127.0.0.1:9999".to_string());

        let http_url = config
            .proxy
            .http_listen
            .unwrap_or_else(|| derive_http_url(&grpc_url));

        Self {
            grpc_url,
            http_url,
            api_key: config.proxy.api_key,
            timeout: Duration::from_secs(30),
        }
    }
}

/// Builder for constructing SdkConfig programmatically.
#[derive(Debug, Default)]
pub struct SdkConfigBuilder {
    grpc_url: Option<String>,
    http_url: Option<String>,
    api_key: Option<String>,
    timeout: Option<Duration>,
}

impl SdkConfigBuilder {
    pub fn grpc_url(mut self, url: &str) -> Self {
        self.grpc_url = Some(url.to_string());
        self
    }

    pub fn http_url(mut self, url: &str) -> Self {
        self.http_url = Some(url.to_string());
        self
    }

    pub fn api_key(mut self, key: &str) -> Self {
        self.api_key = Some(key.to_string());
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn build(self) -> SdkConfig {
        let grpc_url = self
            .grpc_url
            .unwrap_or_else(|| "http://127.0.0.1:9999".to_string());
        let http_url = self.http_url.unwrap_or_else(|| derive_http_url(&grpc_url));

        SdkConfig {
            grpc_url,
            http_url,
            api_key: self.api_key,
            timeout: self.timeout.unwrap_or(Duration::from_secs(30)),
        }
    }
}

/// Derive HTTP URL from gRPC URL by incrementing port by 1.
fn derive_http_url(grpc_url: &str) -> String {
    if let Some(colon_pos) = grpc_url.rfind(':') {
        let port_str = &grpc_url[colon_pos.saturating_add(1)..];
        if let Ok(port) = port_str.parse::<u16>() {
            return format!("http://127.0.0.1:{}", port.saturating_add(1));
        }
    }
    "http://127.0.0.1:10000".to_string()
}

#[derive(Deserialize, Default)]
struct ConfigFile {
    #[serde(default)]
    proxy: ProxySection,
}

impl ConfigFile {
    /// Merge two configs: self is base, other overrides.
    fn merge(self, other: ConfigFile) -> ConfigFile {
        ConfigFile {
            proxy: ProxySection {
                listen: other.proxy.listen.or(self.proxy.listen),
                http_listen: other.proxy.http_listen.or(self.proxy.http_listen),
                api_key: other.proxy.api_key.or(self.proxy.api_key),
            },
        }
    }
}

#[derive(Deserialize, Default)]
struct ProxySection {
    listen: Option<String>,
    http_listen: Option<String>,
    api_key: Option<String>,
}
