mod admin;
mod auth;
pub mod client;
pub mod config;
mod context;
pub mod error;
mod observability;
mod search;

/// Re-export proto types for advanced usage.
pub mod proto {
    tonic::include_proto!("conproxy.v1");
}

pub use client::ConproxyClient;
pub use config::SdkConfig;
pub use error::SdkError;
