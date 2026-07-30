use crate::helpers::client::E2eClient;
use crate::helpers::config::ConfigManager;
use crate::helpers::constants::{category_enabled, Suite};
use crate::helpers::report::TestReport;
use crate::run_test;

pub fn run(client: &E2eClient, suite: Suite, config: &ConfigManager, report: &mut TestReport) {
    if !category_enabled("reload") || !suite.is_all() {
        return;
    }

    eprintln!();
    eprintln!("\x1b[1mHot Reload Tests\x1b[0m");
    eprintln!("--------------------------------------------");

    // Reload returns success
    run_test!(report, "reload", "Reload: success response", {
        let (status, body) = client.admin_reload();
        assert_eq!(status, 200);
        assert_eq!(body["success"], true, "Expected success=true: {}", body);
    });

    // Modify config on disk
    config.backup_current();
    config.set_max_entries(9999);

    // Reload picks up new max_entries
    run_test!(report, "reload", "Reload: picks up new max_entries", {
        let (status, body) = client.admin_reload();
        assert_eq!(status, 200);
        let msg = body["message"].as_str().unwrap_or("");
        assert!(
            msg.contains("9999"),
            "Expected message to contain 9999, got: {msg}"
        );
    });

    // Reload reports reloaded sections
    run_test!(report, "reload", "Reload: reports reloaded sections", {
        let (_, body) = client.admin_reload();
        let reloaded = body["reloaded"].as_array().map(|a| a.len()).unwrap_or(0);
        assert!(reloaded > 0, "Expected reloaded sections > 0");
    });

    // Restore original config
    config.restore_reload_backup();

    // Reload with restored config
    run_test!(report, "reload", "Reload: restored config", {
        let (status, body) = client.admin_reload();
        assert_eq!(status, 200);
        assert_eq!(body["success"], true);
    });

    eprintln!("--------------------------------------------");
}
