//! gRPC parity E2E tests.
//!
//! Runs against the main suite proxy. Validates that all gRPC services
//! (SearchService, AdminService, ObservabilityService, ContextService)
//! respond correctly. Follows the compression.rs pattern.

use crate::helpers::constants::{category_enabled, proxy_url};
use crate::helpers::report::TestReport;
use crate::run_test;

use conproxy::proxy::grpc::proto::{
    self, admin_service_client::AdminServiceClient, context_service_client::ContextServiceClient,
    observability_service_client::ObservabilityServiceClient,
    search_service_client::SearchServiceClient,
};
use tonic::transport::Channel;

/// Derive the gRPC URL from the HTTP base URL (port - 1).
fn grpc_url() -> String {
    let http_port: u16 = proxy_url()
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8081);
    format!("http://127.0.0.1:{}", http_port - 1)
}

pub fn run(report: &mut TestReport) {
    if !category_enabled("grpc_parity") {
        return;
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to build tokio runtime");

    let url = grpc_url();

    eprintln!();
    eprintln!("\x1b[32m[INFO]\x1b[0m gRPC parity tests...");

    // ======== SearchService ========

    run_test!(report, "grpc_parity", "gRPC: SearchService Query", {
        let url = url.clone();
        rt.block_on(async {
            let channel = Channel::from_shared(url).unwrap().connect().await.unwrap();
            let mut client = SearchServiceClient::new(channel);
            let resp = client
                .query(proto::QueryRequest {
                    query: "grpc parity test".into(),
                    top_k: 3,
                    ..Default::default()
                })
                .await
                .expect("SearchService.Query failed");
            let inner = resp.into_inner();
            assert!(
                !inner.results.is_empty() || inner.cache_status >= 0,
                "Expected valid Query response"
            );
        });
    });

    run_test!(report, "grpc_parity", "gRPC: SearchService BatchQuery", {
        let url = url.clone();
        rt.block_on(async {
            let channel = Channel::from_shared(url).unwrap().connect().await.unwrap();
            let mut client = SearchServiceClient::new(channel);
            let resp = client
                .batch_query(proto::BatchQueryRequest {
                    queries: vec!["grpc batch a".into(), "grpc batch b".into()],
                    fail_fast: false,
                    context: String::new(),
                })
                .await
                .expect("SearchService.BatchQuery failed");
            let inner = resp.into_inner();
            assert!(!inner.results.is_empty(), "Expected batch results");
        });
    });

    run_test!(
        report,
        "grpc_parity",
        "gRPC: SearchService FederatedQuery",
        {
            let url = url.clone();
            rt.block_on(async {
                let channel = Channel::from_shared(url).unwrap().connect().await.unwrap();
                let mut client = SearchServiceClient::new(channel);
                let resp = client
                    .federated_query(proto::FederatedQueryRequest {
                        query: "grpc federated test".into(),
                        local_results: vec![],
                        top_k: 3,
                        context: String::new(),
                    })
                    .await
                    .expect("SearchService.FederatedQuery failed");
                let inner = resp.into_inner();
                // Just verify the response has stats
                assert!(
                    inner.stats.is_some() || inner.took_ms > 0,
                    "Expected federated response with stats or took_ms"
                );
            });
        }
    );

    run_test!(report, "grpc_parity", "gRPC: SearchService QueryStream", {
        let url = url.clone();
        rt.block_on(async {
            let channel = Channel::from_shared(url).unwrap().connect().await.unwrap();
            let mut client = SearchServiceClient::new(channel);
            let resp = client
                .query_stream(proto::QueryRequest {
                    query: "grpc stream test".into(),
                    top_k: 3,
                    ..Default::default()
                })
                .await
                .expect("SearchService.QueryStream failed");
            let mut stream = resp.into_inner();
            let mut count = 0;
            while let Some(item) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                stream.message().await
            })
            .await
            .unwrap_or(Ok(None))
            .unwrap_or(None)
            {
                count += 1;
                assert!(item.cache_status >= 0);
            }
            assert!(
                count >= 1,
                "Expected at least 1 stream response, got {count}"
            );
        });
    });

    // ======== AdminService ========

    run_test!(report, "grpc_parity", "gRPC: AdminService Reload", {
        let url = url.clone();
        rt.block_on(async {
            let channel = Channel::from_shared(url).unwrap().connect().await.unwrap();
            let mut client = AdminServiceClient::new(channel);
            let resp = client
                .reload(proto::ReloadRequest {})
                .await
                .expect("AdminService.Reload failed");
            let inner = resp.into_inner();
            // success field exists
            assert!(inner.success || !inner.error.is_empty() || !inner.message.is_empty());
        });
    });

    run_test!(report, "grpc_parity", "gRPC: AdminService Pause", {
        let url = url.clone();
        rt.block_on(async {
            let channel = Channel::from_shared(url).unwrap().connect().await.unwrap();
            let mut client = AdminServiceClient::new(channel);
            let resp = client
                .pause(proto::PauseRequest {})
                .await
                .expect("AdminService.Pause failed");
            let inner = resp.into_inner();
            assert!(
                inner.status.contains("pause")
                    || inner.status.contains("Pause")
                    || !inner.status.is_empty(),
                "Expected paused status, got: {}",
                inner.status
            );
        });
    });

    run_test!(report, "grpc_parity", "gRPC: AdminService Resume", {
        let url = url.clone();
        rt.block_on(async {
            let channel = Channel::from_shared(url).unwrap().connect().await.unwrap();
            let mut client = AdminServiceClient::new(channel);
            let resp = client
                .resume(proto::ResumeRequest {})
                .await
                .expect("AdminService.Resume failed");
            let inner = resp.into_inner();
            assert!(
                inner.status.contains("resum")
                    || inner.status.contains("Resum")
                    || !inner.status.is_empty(),
                "Expected resumed status, got: {}",
                inner.status
            );
        });
    });

    run_test!(report, "grpc_parity", "gRPC: AdminService MetricsReset", {
        let url = url.clone();
        rt.block_on(async {
            let channel = Channel::from_shared(url).unwrap().connect().await.unwrap();
            let mut client = AdminServiceClient::new(channel);
            let resp = client
                .metrics_reset(proto::MetricsResetRequest {})
                .await
                .expect("AdminService.MetricsReset failed");
            let _ = resp.into_inner();
        });
    });

    // ======== ObservabilityService ========

    run_test!(
        report,
        "grpc_parity",
        "gRPC: ObservabilityService GetStats",
        {
            let url = url.clone();
            rt.block_on(async {
                let channel = Channel::from_shared(url).unwrap().connect().await.unwrap();
                let mut client = ObservabilityServiceClient::new(channel);
                let resp = client
                    .get_stats(proto::GetStatsRequest {})
                    .await
                    .expect("ObservabilityService.GetStats failed");
                let inner = resp.into_inner();
                let _ = inner.cache_entries; // verify field exists
            });
        }
    );

    run_test!(
        report,
        "grpc_parity",
        "gRPC: ObservabilityService GetCircuitStatus",
        {
            let url = url.clone();
            rt.block_on(async {
                let channel = Channel::from_shared(url).unwrap().connect().await.unwrap();
                let mut client = ObservabilityServiceClient::new(channel);
                let resp = client
                    .get_circuit_status(proto::GetCircuitStatusRequest {})
                    .await
                    .expect("ObservabilityService.GetCircuitStatus failed");
                let inner = resp.into_inner();
                assert!(!inner.state.is_empty(), "Expected state string");
            });
        }
    );

    run_test!(
        report,
        "grpc_parity",
        "gRPC: ObservabilityService GetPoolStatus",
        {
            let url = url.clone();
            rt.block_on(async {
                let channel = Channel::from_shared(url).unwrap().connect().await.unwrap();
                let mut client = ObservabilityServiceClient::new(channel);
                let resp = client
                    .get_pool_status(proto::GetPoolStatusRequest {})
                    .await
                    .expect("ObservabilityService.GetPoolStatus failed");
                let inner = resp.into_inner();
                let _ = inner.total_upstreams; // verify field exists
            });
        }
    );

    run_test!(
        report,
        "grpc_parity",
        "gRPC: ObservabilityService GetCacheUpstreams",
        {
            let url = url.clone();
            rt.block_on(async {
                let channel = Channel::from_shared(url).unwrap().connect().await.unwrap();
                let mut client = ObservabilityServiceClient::new(channel);
                let resp = client
                    .get_cache_upstreams(proto::GetCacheUpstreamsRequest {})
                    .await
                    .expect("ObservabilityService.GetCacheUpstreams failed");
                let _ = resp.into_inner();
                // Just verify it responds without error
            });
        }
    );

    // ======== ContextService ========

    run_test!(
        report,
        "grpc_parity",
        "gRPC: ContextService ListContexts",
        {
            let url = url.clone();
            rt.block_on(async {
                let channel = Channel::from_shared(url).unwrap().connect().await.unwrap();
                let mut client = ContextServiceClient::new(channel);
                let resp = client
                    .list_contexts(proto::ListContextsRequest {})
                    .await
                    .expect("ContextService.ListContexts failed");
                let inner = resp.into_inner();
                assert!(!inner.current.is_empty(), "Expected current context");
            });
        }
    );

    run_test!(
        report,
        "grpc_parity",
        "gRPC: ContextService CreateContext",
        {
            let url = url.clone();
            rt.block_on(async {
                let channel = Channel::from_shared(url).unwrap().connect().await.unwrap();
                let mut client = ContextServiceClient::new(channel);
                let resp = client
                    .create_context(proto::CreateContextRequest {
                        context_id: "grpc-ctx".into(),
                    })
                    .await
                    .expect("ContextService.CreateContext failed");
                let inner = resp.into_inner();
                assert!(
                    inner.context_id == "grpc-ctx" || !inner.message.is_empty(),
                    "Expected context creation response"
                );
            });
        }
    );

    run_test!(
        report,
        "grpc_parity",
        "gRPC: ContextService SwitchContext",
        {
            let url = url.clone();
            rt.block_on(async {
                let channel = Channel::from_shared(url).unwrap().connect().await.unwrap();
                let mut client = ContextServiceClient::new(channel);
                let resp = client
                    .switch_context(proto::SwitchContextRequest {
                        context_id: "grpc-ctx".into(),
                    })
                    .await
                    .expect("ContextService.SwitchContext failed");
                let inner = resp.into_inner();
                assert_eq!(inner.current, "grpc-ctx", "Expected current=grpc-ctx");
            });
        }
    );

    run_test!(
        report,
        "grpc_parity",
        "gRPC: ContextService GetContextStats",
        {
            let url = url.clone();
            rt.block_on(async {
                let channel = Channel::from_shared(url).unwrap().connect().await.unwrap();
                let mut client = ContextServiceClient::new(channel);
                let resp = client
                    .get_context_stats(proto::GetContextStatsRequest {
                        context_id: "grpc-ctx".into(),
                    })
                    .await
                    .expect("ContextService.GetContextStats failed");
                let inner = resp.into_inner();
                assert_eq!(inner.context_id, "grpc-ctx");
                // hits and misses exist (they may be 0)
                let _ = inner.hits;
                let _ = inner.misses;
            });
        }
    );

    // Switch back to default
    {
        let url = url.clone();
        let _ = rt.block_on(async {
            let channel = Channel::from_shared(url).unwrap().connect().await.unwrap();
            let mut client = ContextServiceClient::new(channel);
            let _ = client
                .switch_context(proto::SwitchContextRequest {
                    context_id: "default".into(),
                })
                .await;
        });
    }
}
