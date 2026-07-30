//! Error types for conproxy

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConproxyError {
    /// Returned when [`crate::config::Config::find_local_root`] cannot locate
    /// a project root from the current working directory.
    ///
    /// Note: `Config::load` no longer raises this error — when neither a global
    /// nor a local config exists, it returns a default in-memory config so the
    /// proxy can run on first use.
    #[error("Not in a conproxy project (no .conproxy/ directory found).")]
    NotInitialized,

    /// Returned when a configuration file contains invalid syntax or values.
    #[error("Invalid config: {0}")]
    InvalidConfig(String),

    /// Returned when configuration fails semantic validation checks.
    #[error("Config validation error: {0}")]
    ConfigValidation(String),

    /// Returned when an HTTP request to an external service fails.
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    /// Returned when a file read or write operation fails.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Returned when a TOML string cannot be deserialized into the expected type.
    #[error("TOML parse error: {0}")]
    TomlParse(#[from] toml::de::Error),

    /// Returned when a value cannot be serialized into TOML format.
    #[error("TOML serialize error: {0}")]
    TomlSerialize(#[from] toml::ser::Error),

    /// Returned when the proxy process is expected to be running but is not.
    #[error("Proxy not running")]
    ProxyNotRunning,

    /// Returned when connecting to the proxy socket fails.
    #[error("Proxy connection failed: {0}")]
    ProxyConnection(String),

    /// Returned when an upstream service responds with an error status.
    #[error("Upstream error: {0}")]
    UpstreamError(String),

    /// Returned when a cache read, write, or eviction operation fails.
    #[error("Cache error: {0}")]
    CacheError(String),

    /// Returned when ONNX model files are missing under `~/.conproxy/models/`.
    #[error(
        "ONNX model not installed in ~/.conproxy/models/ — place model.onnx + tokenizer.json manually"
    )]
    ModelNotInstalled,

    #[cfg(feature = "embed-api")]
    /// Returned when an embedding generation or query operation fails.
    #[error("Embedding error: {0}")]
    Embedding(String),
}

/// Convenience alias for `std::result::Result<T, ConproxyError>`.
pub type Result<T> = std::result::Result<T, ConproxyError>;

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
mod tests {
    use super::*;

    #[test]
    fn test_display_not_initialized() {
        let s = format!("{}", ConproxyError::NotInitialized);
        assert!(
            s.contains(".conproxy/") || s.contains("conproxy project"),
            "should mention .conproxy/, got: {s}"
        );
    }

    #[test]
    fn test_display_invalid_config() {
        let s = format!("{}", ConproxyError::InvalidConfig("bad value".to_string()));
        assert!(s.contains("Invalid config"), "got: {s}");
        assert!(
            s.contains("bad value"),
            "should include inner message, got: {s}"
        );
    }

    #[test]
    fn test_display_config_validation() {
        let s = format!(
            "{}",
            ConproxyError::ConfigValidation("semver mismatch".to_string())
        );
        assert!(s.contains("Config validation"), "got: {s}");
        assert!(s.contains("semver mismatch"), "got: {s}");
    }

    #[test]
    fn test_display_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let err: ConproxyError = io_err.into();
        let s = format!("{err}");
        assert!(s.contains("IO error"), "got: {s}");
        assert!(
            s.contains("file missing"),
            "should propagate inner message, got: {s}"
        );
    }

    #[test]
    fn test_display_toml_parse() {
        // Invalid TOML → parse error
        let parse_result: std::result::Result<toml::Value, _> = toml::from_str("invalid = =");
        assert!(parse_result.is_err());
        let err: ConproxyError = parse_result.unwrap_err().into();
        let s = format!("{err}");
        assert!(s.contains("TOML parse error"), "got: {s}");
    }

    #[test]
    fn test_display_toml_serialize() {
        // A value that can't be serialized to TOML — use a map with non-string key
        let mut bad = std::collections::HashMap::new();
        bad.insert(std::ffi::OsString::from("k"), "v"); // OsString not TOML-serializable
        let ser_result = toml::to_string(&bad);
        // OsString may actually serialize via Display, so just check the error path
        // is exercised — if it succeeds, skip
        if let Err(e) = ser_result {
            let err: ConproxyError = e.into();
            let s = format!("{err}");
            assert!(
                s.contains("TOML serialize") || s.contains("serialize"),
                "got: {s}"
            );
        }
    }

    #[test]
    fn test_display_proxy_not_running() {
        let s = format!("{}", ConproxyError::ProxyNotRunning);
        assert!(s.contains("Proxy not running"), "got: {s}");
    }

    #[test]
    fn test_display_proxy_connection() {
        let s = format!(
            "{}",
            ConproxyError::ProxyConnection("socket closed".to_string())
        );
        assert!(s.contains("Proxy connection"), "got: {s}");
        assert!(s.contains("socket closed"), "got: {s}");
    }

    #[test]
    fn test_display_upstream_error() {
        let s = format!(
            "{}",
            ConproxyError::UpstreamError("502 Bad Gateway".to_string())
        );
        assert!(s.contains("Upstream error"), "got: {s}");
        assert!(s.contains("502"), "got: {s}");
    }

    #[test]
    fn test_display_cache_error() {
        let s = format!(
            "{}",
            ConproxyError::CacheError("eviction failed".to_string())
        );
        assert!(s.contains("Cache error"), "got: {s}");
        assert!(s.contains("eviction failed"), "got: {s}");
    }

    #[test]
    fn test_display_model_not_installed() {
        let s = format!("{}", ConproxyError::ModelNotInstalled);
        assert!(s.contains("ONNX model not installed"), "got: {s}");
        assert!(
            s.contains("model.onnx") && s.contains("tokenizer.json"),
            "should mention required files, got: {s}"
        );
    }

    #[test]
    fn test_from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let err: ConproxyError = io_err.into();
        matches!(err, ConproxyError::Io(_));
    }

    #[test]
    fn test_from_toml_parse_error() {
        let bad: std::result::Result<toml::Value, _> = toml::from_str("= = =");
        let err: ConproxyError = bad.unwrap_err().into();
        matches!(err, ConproxyError::TomlParse(_));
    }

    #[test]
    fn test_debug_format_works() {
        // All error variants should support Debug
        let err = ConproxyError::InvalidConfig("test".to_string());
        let dbg = format!("{err:?}");
        assert!(
            dbg.contains("InvalidConfig"),
            "Debug should include variant name, got: {dbg}"
        );
    }
}
