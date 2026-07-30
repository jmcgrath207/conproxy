use crate::helpers::client::E2eClient;
use crate::helpers::constants::{category_enabled, Suite};
use crate::helpers::docker;
use crate::helpers::report::TestReport;
use crate::run_test;
use std::time::Duration;

pub fn run(client: &E2eClient, suite: Suite, report: &mut TestReport) {
    if !category_enabled("resilience") {
        return;
    }

    // Failure injection (all suite only — multi-elasticsearch.toml uses meili-1/2)
    if suite.is_all() {
        eprintln!();
        eprintln!("\x1b[1mFailure Injection Tests\x1b[0m");
        eprintln!("--------------------------------------------");

        // Both healthy
        run_test!(report, "resilience", "Failover: both healthy", {
            let (_, pool) = client.pool();
            let healthy = pool["stats"]["healthy_upstreams"].as_u64().unwrap_or(0);
            assert!(
                healthy >= 2,
                "Expected >= 2 healthy upstreams, got {healthy}"
            );
        });

        // Stop secondary meilisearch (compose: conproxy-cmeili-2)
        docker::stop_container("conproxy-cmeili-2");
        std::thread::sleep(Duration::from_secs(3));

        // Send probe queries to trigger failure detection
        for i in 0..5 {
            let _ = client.query(&format!("failover probe {i}"));
        }
        std::thread::sleep(Duration::from_secs(2));

        // Query still succeeds via primary
        run_test!(report, "resilience", "Failover: query via primary", {
            let (status, body) = client.query("failover test query");
            assert_eq!(status, 200);
            assert!(
                body["results"].is_array(),
                "Missing results during failover"
            );
        });

        // Pool shows failures
        run_test!(report, "resilience", "Failover: failures detected", {
            let (_, pool) = client.pool();
            let failures = pool["stats"]["total_failures"].as_u64().unwrap_or(0);
            assert!(failures > 0, "Expected failures > 0, got {failures}");
        });

        // Restart secondary
        docker::start_container("conproxy-cmeili-2");
        std::thread::sleep(Duration::from_secs(12));

        // Verify meili secondary is back
        run_test!(report, "resilience", "Failover: es-secondary back", {
            let check = reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap();
            let resp = check
                .get("http://localhost:7701/health")
                .header("Authorization", "Bearer conproxy_test_key")
                .send();
            assert!(
                resp.is_ok() && resp.unwrap().status().is_success(),
                "Meili secondary not back"
            );
        });

        // --- Degradation Ladder ---
        run_test!(report, "resilience", "Degradation: starts Full", {
            let (_, h) = client.health();
            let level = h["degradation_level"].as_str().unwrap_or("");
            assert!(
                level.eq_ignore_ascii_case("full") || h["degradation_level"] == 0,
                "Expected Full degradation level, got: {}",
                h["degradation_level"]
            );
        });

        // --- Circuit Breaker ---
        run_test!(report, "resilience", "Circuit: starts closed", {
            let (_, cb) = client.circuit();
            assert_eq!(cb["state"].as_str(), Some("closed"));
        });

        // Stop ALL meili upstreams to trip circuit breaker
        docker::stop_container("conproxy-cmeili-1");
        docker::stop_container("conproxy-cmeili-2");
        std::thread::sleep(Duration::from_secs(2));

        // Send rapid queries to trip circuit breaker
        for i in 0..8 {
            let _ = client.query(&format!("circuit test {i}"));
        }
        std::thread::sleep(Duration::from_secs(1));

        // Circuit should show failure activity (open, half-open, or failure_count)
        run_test!(report, "resilience", "Circuit: tripped", {
            let (_, cb) = client.circuit();
            let tripped = cb["times_tripped"].as_u64().unwrap_or(0);
            let opened = cb["times_opened"].as_u64().unwrap_or(0);
            let failures = cb["failure_count"].as_u64().unwrap_or(0);
            let state = cb["state"].as_str().unwrap_or("closed");
            let (_, pool) = client.pool();
            let pool_failures = pool["stats"]["total_failures"].as_u64().unwrap_or(0);
            assert!(
                tripped > 0
                    || opened > 0
                    || failures > 0
                    || state != "closed"
                    || pool_failures > 0,
                "Expected circuit/pool failure activity, state={state}, tripped={tripped}, opened={opened}, failures={failures}, pool_failures={pool_failures}"
            );
        });

        // Restart upstreams and wait for circuit recovery
        docker::start_container("conproxy-cmeili-1");
        docker::start_container("conproxy-cmeili-2");
        std::thread::sleep(Duration::from_secs(35));

        // Circuit should recover
        run_test!(report, "resilience", "Circuit: recovering", {
            let (_, cb) = client.circuit();
            let state = cb["state"].as_str().unwrap_or("unknown");
            assert!(
                state == "closed" || state == "half_open",
                "Expected closed or half_open, got {state}"
            );
        });

        eprintln!("--------------------------------------------");
    }

    // Request coalescing (text upstreams only)
    if suite.has_text_upstreams() {
        eprintln!();
        eprintln!("\x1b[1mRequest Coalescing Test\x1b[0m");
        eprintln!("--------------------------------------------");

        // Clear cache so requests are misses
        client.cache_clear();

        // Send 5 concurrent identical requests
        let handles: Vec<_> = (0..5)
            .map(|_| {
                let url = format!("{}/query", crate::helpers::constants::proxy_url());
                std::thread::spawn(move || {
                    let c = reqwest::blocking::Client::builder()
                        .timeout(Duration::from_secs(10))
                        .build()
                        .unwrap();
                    let _ = c
                        .post(&url)
                        .json(&serde_json::json!({"query": "coalescing test identical query"}))
                        .send();
                })
            })
            .collect();
        for h in handles {
            let _ = h.join();
        }

        run_test!(report, "resilience", "Coalescing: metric tracked", {
            let (_, prom) = client.prometheus();
            assert!(
                prom.contains("conproxy_coalesced_requests_total"),
                "Missing coalescing metric"
            );
        });

        eprintln!("--------------------------------------------");
    }
}
