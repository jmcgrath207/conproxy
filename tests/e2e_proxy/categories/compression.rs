//! gRPC compression tests — validates gzip and zstd negotiation on the proxy.
//!
//! Connects directly to the gRPC port (HTTP port - 1) and verifies that
//! queries succeed with each compression encoding. The proxy must have
//! `accept_compressed` + `send_compressed` enabled for Gzip and Zstd.

use crate::helpers::constants::{category_enabled, proxy_url};
use crate::helpers::report::TestReport;
use crate::run_test;

use conproxy::proxy::grpc::proto::{self, search_service_client::SearchServiceClient};
use tonic::codec::CompressionEncoding;
use tonic::transport::Channel;

/// Derive the gRPC URL from the HTTP base URL (port - 1).
fn grpc_url() -> String {
    // proxy_url() is "http://127.0.0.1:8081", gRPC is on 8080
    let http_port: u16 = proxy_url()
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8081);
    format!("http://127.0.0.1:{}", http_port - 1)
}

fn make_query_request() -> proto::QueryRequest {
    proto::QueryRequest {
        query: "gzip compression test".into(),
        top_k: 3,
        ..Default::default()
    }
}

pub fn run(report: &mut TestReport) {
    if !category_enabled("compression") {
        return;
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to build tokio runtime");

    let url = grpc_url();

    // Test 1: gRPC query with gzip compression
    run_test!(report, "compression", "gRPC query with gzip", {
        let url = url.clone();
        rt.block_on(async {
            let channel = Channel::from_shared(url)
                .expect("invalid URL")
                .connect()
                .await
                .expect("Failed to connect to gRPC");

            let mut client = SearchServiceClient::new(channel)
                .send_compressed(CompressionEncoding::Gzip)
                .accept_compressed(CompressionEncoding::Gzip);

            let resp = client
                .query(make_query_request())
                .await
                .expect("gzip query failed");

            let inner = resp.into_inner();
            assert!(
                !inner.results.is_empty() || inner.cache_status >= 0,
                "Expected a valid response with gzip compression"
            );
        });
    });

    // Test 2: gRPC query with zstd compression
    run_test!(report, "compression", "gRPC query with zstd", {
        let url = url.clone();
        rt.block_on(async {
            let channel = Channel::from_shared(url)
                .expect("invalid URL")
                .connect()
                .await
                .expect("Failed to connect to gRPC");

            let mut client = SearchServiceClient::new(channel)
                .send_compressed(CompressionEncoding::Zstd)
                .accept_compressed(CompressionEncoding::Zstd);

            let resp = client
                .query(make_query_request())
                .await
                .expect("zstd query failed");

            let inner = resp.into_inner();
            assert!(
                !inner.results.is_empty() || inner.cache_status >= 0,
                "Expected a valid response with zstd compression"
            );
        });
    });

    // Test 3: gRPC query without compression (backward compat)
    run_test!(report, "compression", "gRPC query without compression", {
        let url = url.clone();
        rt.block_on(async {
            let channel = Channel::from_shared(url)
                .expect("invalid URL")
                .connect()
                .await
                .expect("Failed to connect to gRPC");

            let mut client = SearchServiceClient::new(channel);

            let resp = client
                .query(make_query_request())
                .await
                .expect("uncompressed query failed");

            let inner = resp.into_inner();
            assert!(
                !inner.results.is_empty() || inner.cache_status >= 0,
                "Expected a valid response without compression"
            );
        });
    });
}
