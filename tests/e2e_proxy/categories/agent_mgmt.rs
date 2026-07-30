//! Agent management CRUD E2E tests.
//!
//! Manages its own ProxyProcess with agent config and MockUpstream.
//! Tests agent listing, creation, key rotation, and deletion.

use crate::helpers::client::E2eClient;
use crate::helpers::config::ConfigManager;
use crate::helpers::constants::{category_enabled, proxy_url};
use crate::helpers::mock_upstream::MockUpstream;
use crate::helpers::proxy::ProxyProcess;
use crate::helpers::report::TestReport;
use crate::run_test;
use std::path::PathBuf;
use std::time::Duration;

pub fn run(report: &mut TestReport) {
    if !category_enabled("agent_mgmt") {
        return;
    }

    eprintln!();
    eprintln!("\x1b[1mAgent Management CRUD Tests\x1b[0m");
    eprintln!("--------------------------------------------");

    let mock = MockUpstream::start();
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut config = ConfigManager::new(&project_root);
    config.write_agent_config(&mock.url());

    eprintln!("\x1b[32m[INFO]\x1b[0m Starting proxy with agent config...");
    let mut proxy = ProxyProcess::start("127.0.0.1:8080");
    if let Err(e) = proxy.wait_healthy(Duration::from_secs(5)) {
        eprintln!("\x1b[31m[ERROR]\x1b[0m Agent proxy failed to start: {e}");
        proxy.stop();
        config.restore();
        return;
    }

    // Client with global API key for admin operations
    let admin_client = E2eClient::new(proxy_url()).with_api_key("e2e-global-key");

    // Test 1: List initial agents
    run_test!(report, "agent_mgmt", "Agent: list initial", {
        let (status, body) = admin_client.admin_agents_list();
        assert_eq!(status, 200);
        let agents = body["agents"].as_array();
        assert!(agents.is_some(), "Expected agents array");
        let count = agents.unwrap().len();
        assert!(count >= 1, "Expected at least 1 seed agent, got {count}");
    });

    // Test 2: Create new agent
    run_test!(report, "agent_mgmt", "Agent: create", {
        let (status, body) = admin_client.admin_agents_create(&serde_json::json!({
            "id": "e2e-agent",
            "api_key": "new-key",
            "allowed_contexts": [],
            "priority_class": 1
        }));
        assert!(
            status == 201 || status == 200,
            "Expected 201/200, got {status}: {body}"
        );
    });

    // Test 3: List after create
    run_test!(report, "agent_mgmt", "Agent: list after create", {
        let (status, body) = admin_client.admin_agents_list();
        assert_eq!(status, 200);
        let agents = body["agents"].as_array().expect("Expected agents array");
        assert!(
            agents.len() >= 2,
            "Expected at least 2 agents after create, got {}",
            agents.len()
        );
    });

    // Test 4: New agent can query
    run_test!(report, "agent_mgmt", "Agent: new agent can query", {
        let agent_client = E2eClient::new(proxy_url()).with_api_key("new-key");
        let (status, body) = agent_client.query("agent mgmt test query");
        assert_eq!(
            status, 200,
            "Expected new agent to query successfully, got {status}"
        );
        assert!(body["results"].is_array());
    });

    // Test 5: Rotate key
    run_test!(report, "agent_mgmt", "Agent: rotate key", {
        let (status, body) = admin_client.admin_agents_rotate_key(
            "e2e-agent",
            &serde_json::json!({"new_api_key": "rotated-key"}),
        );
        assert_eq!(
            status, 200,
            "Expected 200 for key rotation, got {status}: {body}"
        );
    });

    // Test 6: Old key rejected
    run_test!(report, "agent_mgmt", "Agent: old key rejected", {
        let old_client = E2eClient::new(proxy_url()).with_api_key("new-key");
        let (status, _) = old_client.query("agent old key test");
        assert_eq!(status, 401, "Expected 401 with old key, got {status}");
    });

    // Test 7: Rotated key works
    run_test!(report, "agent_mgmt", "Agent: rotated key works", {
        let rotated_client = E2eClient::new(proxy_url()).with_api_key("rotated-key");
        let (status, body) = rotated_client.query("agent rotated key test");
        assert_eq!(status, 200, "Expected 200 with rotated key, got {status}");
        assert!(body["results"].is_array());
    });

    // Test 8: Delete agent
    run_test!(report, "agent_mgmt", "Agent: delete", {
        let (status, body) = admin_client.admin_agents_delete("e2e-agent");
        assert_eq!(
            status, 200,
            "Expected 200 for agent delete, got {status}: {body}"
        );

        // Verify rotated key no longer works
        let deleted_client = E2eClient::new(proxy_url()).with_api_key("rotated-key");
        let (query_status, _) = deleted_client.query("agent deleted test");
        assert_eq!(
            query_status, 401,
            "Expected 401 after agent deletion, got {query_status}"
        );
    });

    // Cleanup
    eprintln!("\x1b[32m[INFO]\x1b[0m Stopping agent proxy...");
    proxy.stop();
    config.restore();
    eprintln!("--------------------------------------------");
}
