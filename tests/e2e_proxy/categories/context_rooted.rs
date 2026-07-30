//! Plan 10 e2e: context-rooted config starts proxy.

use crate::helpers::client::E2eClient;
use crate::helpers::config::ConfigManager;
use crate::helpers::constants::{category_enabled, proxy_url};
use crate::helpers::mock_upstream::MockUpstream;
use crate::helpers::proxy::ProxyProcess;
use crate::helpers::report::TestReport;
use crate::run_test;
use std::path::PathBuf;
use std::time::Duration;

/// Own mock + proxy: context-rooted TOML start path.
pub fn run(report: &mut TestReport) {
    if !category_enabled("context_rooted") {
        return;
    }

    eprintln!();
    eprintln!("\x1b[1mContext-rooted config E2E (plan 10)\x1b[0m");
    eprintln!("--------------------------------------------");

    let mock = MockUpstream::start();
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut config = ConfigManager::new(&project_root);
    let client = E2eClient::new(proxy_url());

    // --- Context-rooted start ---
    config.write_context_rooted_mock(&mock.url(), "elasticsearch");
    eprintln!(
        "\x1b[32m[INFO]\x1b[0m Starting proxy with context-rooted config @ {}...",
        mock.url()
    );
    let mut proxy = ProxyProcess::start("127.0.0.1:8080");
    if let Err(e) = proxy.wait_healthy(Duration::from_secs(8)) {
        eprintln!("\x1b[31m[ERROR]\x1b[0m Context-rooted proxy failed: {e}");
        proxy.stop();
        config.restore();
        return;
    }

    run_test!(report, "context_rooted", "CR: health ok", {
        let (status, _) = client.health();
        assert_eq!(status, 200, "health after context-rooted start");
    });

    run_test!(report, "context_rooted", "CR: query miss then hit", {
        let q = "plan10 context rooted query";
        let (s1, b1) = client.query(q);
        assert_eq!(s1, 200, "first query status");
        let cs1 = b1["cache_status"].as_str().unwrap_or("");
        assert_eq!(cs1, "miss", "first query miss, got {cs1}");

        let (s2, b2) = client.query(q);
        assert_eq!(s2, 200, "second query status");
        let cs2 = b2["cache_status"].as_str().unwrap_or("");
        assert_eq!(cs2, "hit", "second query hit, got {cs2}");
    });

    run_test!(report, "context_rooted", "CR: contexts list has default", {
        let (status, body) = client.contexts();
        assert_eq!(status, 200, "contexts list");
        let text = body.to_string();
        assert!(
            text.contains("default"),
            "expected default context in list: {text}"
        );
    });

    proxy.stop();
    config.restore();
    eprintln!("--------------------------------------------");
}
