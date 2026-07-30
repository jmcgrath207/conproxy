//! gRPC service implementations for the cache proxy.
//!
//! Uses tonic to serve all operational endpoints over HTTP/2 + protobuf.
//! HTTP is retained only for infrastructure (health, readiness, prometheus).

pub mod admin;
pub mod context;
pub mod convert;
pub mod middleware;
pub mod observability;
pub mod search;

/// Generated protobuf types for conproxy.v1.
pub mod proto {
    tonic::include_proto!("conproxy.v1");
}
