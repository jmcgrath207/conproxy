//! SDK parity E2E tests.
//!
//! Validates that the `conproxy-sdk` crate can exercise all 30 RPCs
//! across all 4 gRPC services (Search, Admin, Observability, Context).
//! Uses `ConproxyClient` from the SDK instead of raw tonic clients.

use crate::helpers::constants::{category_enabled, proxy_url};
use crate::helpers::report::TestReport;
use crate::run_test;

use conproxy_sdk::ConproxyClient;

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
    if !category_enabled("sdk_parity") {
        return;
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to build tokio runtime");

    let url = grpc_url();

    eprintln!();
    eprintln!("\x1b[32m[INFO]\x1b[0m SDK parity tests (conproxy-sdk)...");

    let client = rt
        .block_on(async { ConproxyClient::connect(&url) })
        .expect("SDK: Failed to create ConproxyClient");

    // ======== SearchService ========

    run_test!(report, "sdk_parity", "SDK: SearchService Query", {
        rt.block_on(async {
            let resp = client
                .query("sdk parity test", 3)
                .await
                .expect("SDK: SearchService.Query failed");
            assert!(
                !resp.results.is_empty() || resp.cache_status >= 0,
                "Expected valid Query response"
            );
        });
    });

    run_test!(report, "sdk_parity", "SDK: SearchService QueryFull", {
        rt.block_on(async {
            let req = conproxy_sdk::proto::QueryRequest {
                query: "sdk query full test".into(),
                top_k: 5,
                priority: 1,
                ..Default::default()
            };
            let resp = client
                .query_full(req)
                .await
                .expect("SDK: SearchService.QueryFull failed");
            let _ = resp.took_ms; // verify field exists
        });
    });

    run_test!(report, "sdk_parity", "SDK: SearchService BatchQuery", {
        rt.block_on(async {
            let resp = client
                .batch_query(&["sdk batch a", "sdk batch b"])
                .await
                .expect("SDK: SearchService.BatchQuery failed");
            assert!(!resp.results.is_empty(), "Expected batch results");
        });
    });

    run_test!(report, "sdk_parity", "SDK: SearchService FederatedQuery", {
        rt.block_on(async {
            let resp = client
                .federated_query("sdk federated test", vec![], 3)
                .await
                .expect("SDK: SearchService.FederatedQuery failed");
            assert!(
                resp.stats.is_some() || resp.took_ms > 0,
                "Expected federated response with stats or took_ms"
            );
        });
    });

    run_test!(report, "sdk_parity", "SDK: SearchService QueryStream", {
        rt.block_on(async {
            let mut stream = client
                .query_stream("sdk stream test", 3)
                .await
                .expect("SDK: SearchService.QueryStream failed");
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

    run_test!(report, "sdk_parity", "SDK: AdminService Reload", {
        rt.block_on(async {
            let resp = client
                .reload()
                .await
                .expect("SDK: AdminService.Reload failed");
            assert!(resp.success || !resp.error.is_empty() || !resp.message.is_empty());
        });
    });

    run_test!(report, "sdk_parity", "SDK: AdminService Pause", {
        rt.block_on(async {
            let resp = client
                .pause()
                .await
                .expect("SDK: AdminService.Pause failed");
            assert!(
                !resp.status.is_empty(),
                "Expected non-empty pause status, got: {}",
                resp.status
            );
        });
    });

    run_test!(report, "sdk_parity", "SDK: AdminService Resume", {
        rt.block_on(async {
            let resp = client
                .resume()
                .await
                .expect("SDK: AdminService.Resume failed");
            assert!(
                !resp.status.is_empty(),
                "Expected non-empty resume status, got: {}",
                resp.status
            );
        });
    });

    run_test!(report, "sdk_parity", "SDK: AdminService CacheClear", {
        rt.block_on(async {
            let resp = client
                .cache_clear()
                .await
                .expect("SDK: AdminService.CacheClear failed");
            let _ = resp.cleared_entries; // verify field exists
        });
    });

    run_test!(report, "sdk_parity", "SDK: AdminService CacheWarmup", {
        rt.block_on(async {
            let resp = client
                .cache_warmup(&["sdk warmup query a", "sdk warmup query b"])
                .await
                .expect("SDK: AdminService.CacheWarmup failed");
            let _ = resp.warmed; // verify field exists
        });
    });

    run_test!(report, "sdk_parity", "SDK: AdminService CacheEvict", {
        rt.block_on(async {
            let resp = client
                .cache_evict("", 10, false)
                .await
                .expect("SDK: AdminService.CacheEvict failed");
            let _ = resp.evicted; // verify field exists
        });
    });

    run_test!(report, "sdk_parity", "SDK: AdminService CacheIntegrity", {
        rt.block_on(async {
            let resp = client
                .cache_integrity()
                .await
                .expect("SDK: AdminService.CacheIntegrity failed");
            let _ = resp.total; // verify field exists
        });
    });

    run_test!(report, "sdk_parity", "SDK: AdminService MetricsReset", {
        rt.block_on(async {
            let resp = client
                .metrics_reset()
                .await
                .expect("SDK: AdminService.MetricsReset failed");
            let _ = resp.status; // verify field exists
        });
    });

    run_test!(report, "sdk_parity", "SDK: AdminService ListAgents", {
        rt.block_on(async {
            let resp = client
                .list_agents()
                .await
                .expect("SDK: AdminService.ListAgents failed");
            let _ = resp.total; // verify field exists
        });
    });

    run_test!(report, "sdk_parity", "SDK: AdminService CreateAgent", {
        rt.block_on(async {
            let req = conproxy_sdk::proto::CreateAgentRequest {
                id: "sdk-test-agent".into(),
                api_key: "sdk-test-key-123".into(),
                allowed_contexts: vec!["default".into()],
                priority_class: 1,
                rate_limit_rps: 100,
            };
            let resp = client
                .create_agent(req)
                .await
                .expect("SDK: AdminService.CreateAgent failed");
            assert!(!resp.status.is_empty() || !resp.agent_id.is_empty());
        });
    });

    run_test!(report, "sdk_parity", "SDK: AdminService RotateKey", {
        rt.block_on(async {
            let resp = client
                .rotate_key("sdk-test-agent", "new-sdk-key-456")
                .await
                .expect("SDK: AdminService.RotateKey failed");
            let _ = resp.status; // verify field exists
        });
    });

    run_test!(report, "sdk_parity", "SDK: AdminService DeleteAgent", {
        rt.block_on(async {
            let resp = client
                .delete_agent("sdk-test-agent")
                .await
                .expect("SDK: AdminService.DeleteAgent failed");
            let _ = resp.status; // verify field exists
        });
    });

    // ======== ObservabilityService ========

    run_test!(
        report,
        "sdk_parity",
        "SDK: ObservabilityService GetStats",
        {
            rt.block_on(async {
                let resp = client
                    .stats()
                    .await
                    .expect("SDK: ObservabilityService.GetStats failed");
                let _ = resp.cache_entries; // verify field exists
            });
        }
    );

    run_test!(
        report,
        "sdk_parity",
        "SDK: ObservabilityService GetQueryStats",
        {
            rt.block_on(async {
                let resp = client
                    .query_stats()
                    .await
                    .expect("SDK: ObservabilityService.GetQueryStats failed");
                let _ = resp.stats_json; // verify field exists
            });
        }
    );

    run_test!(
        report,
        "sdk_parity",
        "SDK: ObservabilityService GetAudit",
        {
            rt.block_on(async {
                let resp = client
                    .audit()
                    .await
                    .expect("SDK: ObservabilityService.GetAudit failed");
                let _ = resp.entries_json; // verify field exists
            });
        }
    );

    run_test!(
        report,
        "sdk_parity",
        "SDK: ObservabilityService GetCircuitStatus",
        {
            rt.block_on(async {
                let resp = client
                    .circuit_status()
                    .await
                    .expect("SDK: ObservabilityService.GetCircuitStatus failed");
                assert!(!resp.state.is_empty(), "Expected state string");
            });
        }
    );

    run_test!(
        report,
        "sdk_parity",
        "SDK: ObservabilityService GetQueueStats",
        {
            rt.block_on(async {
                let resp = client
                    .queue_stats()
                    .await
                    .expect("SDK: ObservabilityService.GetQueueStats failed");
                let _ = resp.capacity; // verify field exists
            });
        }
    );

    run_test!(
        report,
        "sdk_parity",
        "SDK: ObservabilityService GetClients",
        {
            rt.block_on(async {
                let resp = client
                    .clients()
                    .await
                    .expect("SDK: ObservabilityService.GetClients failed");
                let _ = resp.total_completed; // verify field exists
            });
        }
    );

    run_test!(
        report,
        "sdk_parity",
        "SDK: ObservabilityService GetPoolStatus",
        {
            rt.block_on(async {
                let resp = client
                    .pool_status()
                    .await
                    .expect("SDK: ObservabilityService.GetPoolStatus failed");
                let _ = resp.total_upstreams; // verify field exists
            });
        }
    );

    run_test!(
        report,
        "sdk_parity",
        "SDK: ObservabilityService GetCacheUpstreams",
        {
            rt.block_on(async {
                let resp = client
                    .cache_upstreams()
                    .await
                    .expect("SDK: ObservabilityService.GetCacheUpstreams failed");
                let _ = resp.upstreams; // verify field exists (map)
            });
        }
    );

    // ======== ContextService ========

    run_test!(report, "sdk_parity", "SDK: ContextService ListContexts", {
        rt.block_on(async {
            let resp = client
                .list_contexts()
                .await
                .expect("SDK: ContextService.ListContexts failed");
            assert!(!resp.current.is_empty(), "Expected current context");
        });
    });

    run_test!(report, "sdk_parity", "SDK: ContextService CreateContext", {
        rt.block_on(async {
            let resp = client
                .create_context("sdk-ctx")
                .await
                .expect("SDK: ContextService.CreateContext failed");
            assert!(
                resp.context_id == "sdk-ctx" || !resp.message.is_empty(),
                "Expected context creation response"
            );
        });
    });

    run_test!(
        report,
        "sdk_parity",
        "SDK: ContextService GetCurrentContext",
        {
            rt.block_on(async {
                let resp = client
                    .current_context()
                    .await
                    .expect("SDK: ContextService.GetCurrentContext failed");
                assert!(!resp.id.is_empty(), "Expected non-empty context id");
            });
        }
    );

    run_test!(report, "sdk_parity", "SDK: ContextService SwitchContext", {
        rt.block_on(async {
            let resp = client
                .switch_context("sdk-ctx")
                .await
                .expect("SDK: ContextService.SwitchContext failed");
            assert_eq!(resp.current, "sdk-ctx", "Expected current=sdk-ctx");
        });
    });

    run_test!(
        report,
        "sdk_parity",
        "SDK: ContextService GetContextStats",
        {
            rt.block_on(async {
                let resp = client
                    .context_stats("sdk-ctx")
                    .await
                    .expect("SDK: ContextService.GetContextStats failed");
                assert_eq!(resp.context_id, "sdk-ctx");
                let _ = resp.hits;
                let _ = resp.misses;
            });
        }
    );

    // Switch back to default
    {
        let _ = rt.block_on(async {
            let _ = client.switch_context("default").await;
        });
    }
}
