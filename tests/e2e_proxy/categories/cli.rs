use crate::helpers::constants::{category_enabled, Suite};
use crate::helpers::report::TestReport;
use crate::run_test;
use std::path::PathBuf;
use std::process::{Command, Output};

pub fn run(suite: Suite, report: &mut TestReport) {
    if !category_enabled("cli") {
        return;
    }

    let bin = conproxy_bin();
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    // CLI search via proxy (text upstreams only)
    if suite.has_text_upstreams() {
        run_test!(report, "cli", "CLI: search via proxy", {
            let out = run_cli(
                &bin,
                &project_root,
                &["search", "rust programming", "--format", "json"],
            );
            assert!(out.status.success(), "CLI search failed: {}", stderr(&out));
            let json = parse_json_stdout(&out);
            let len = json["results"].as_array().map(|a| a.len()).unwrap_or(0);
            assert!(len > 0, "Expected results from CLI search");
        });

        run_test!(report, "cli", "CLI: search shows cache status", {
            let out = run_cli(
                &bin,
                &project_root,
                &["search", "rust programming", "--format", "json"],
            );
            assert!(out.status.success());
            let json = parse_json_stdout(&out);
            assert!(
                json["cache_status"].is_string(),
                "Missing cache_status in CLI search"
            );
        });
    }

    // CLI proxy contexts list
    run_test!(report, "cli", "CLI: proxy contexts list", {
        let out = run_cli(&bin, &project_root, &["contexts", "--json"]);
        assert!(
            out.status.success(),
            "CLI contexts failed: {}",
            stderr(&out)
        );
        let json = parse_json_stdout(&out);
        assert!(
            !json["contexts"].is_null(),
            "Missing contexts in CLI output"
        );
    });

    // CLI proxy status
    run_test!(report, "cli", "CLI: proxy status", {
        let out = run_cli(&bin, &project_root, &["status", "--json"]);
        assert!(
            out.status.success(),
            "CLI proxy status failed: {}",
            stderr(&out)
        );
        let json = parse_json_stdout(&out);
        assert_eq!(json["running"], true, "Expected running=true");
    });

    // CLI conproxy status
    run_test!(report, "cli", "CLI: conproxy status", {
        let out = run_cli(&bin, &project_root, &["status", "--json"]);
        assert!(out.status.success(), "CLI status failed: {}", stderr(&out));
        let json = parse_json_stdout(&out);
        assert_eq!(json["proxy_running"], true, "Expected proxy_running=true");
    });

    // Context switch workflow (text upstreams only)
    if suite.has_text_upstreams() {
        run_test!(report, "cli", "CLI: switch to project-cli", {
            let out = run_cli(
                &bin,
                &project_root,
                &["context", "project-cli", "--switch", "--json"],
            );
            assert!(
                out.status.success(),
                "CLI context switch failed: {}",
                stderr(&out)
            );
            let json = parse_json_stdout(&out);
            assert_eq!(json["success"], true);
        });

        run_test!(report, "cli", "CLI: current is project-cli", {
            let out = run_cli(&bin, &project_root, &["contexts", "--json"]);
            assert!(out.status.success());
            let json = parse_json_stdout(&out);
            assert_eq!(json["current"].as_str(), Some("project-cli"));
        });

        run_test!(report, "cli", "CLI: search in new context", {
            let out = run_cli(
                &bin,
                &project_root,
                &["search", "context test query", "--format", "json"],
            );
            assert!(out.status.success());
            let json = parse_json_stdout(&out);
            assert!(!json["results"].is_null(), "Missing results in new context");
        });

        run_test!(report, "cli", "CLI: switch back to default", {
            let out = run_cli(
                &bin,
                &project_root,
                &["context", "default", "--switch", "--json"],
            );
            assert!(out.status.success());
            let json = parse_json_stdout(&out);
            assert_eq!(json["success"], true);
        });

        run_test!(report, "cli", "CLI: contexts list shows >= 2", {
            let out = run_cli(&bin, &project_root, &["contexts", "--json"]);
            assert!(out.status.success());
            let json = parse_json_stdout(&out);
            let count = json["contexts"].as_array().map(|a| a.len()).unwrap_or(0);
            assert!(count >= 2, "Expected >= 2 contexts, got {count}");
        });
    }

    // CLI seed clear
    run_test!(report, "cli", "CLI: seed clear all", {
        let out = run_cli(
            &bin,
            &project_root,
            &["seed", "clear", "--all", "--confirm"],
        );
        // Allow success or expected error (no cache to clear)
        assert!(
            out.status.success() || stderr(&out).contains("no cache"),
            "CLI seed clear failed: {}",
            stderr(&out)
        );
    });

    // CLI search after clear (miss)
    if suite.has_text_upstreams() {
        run_test!(report, "cli", "CLI: search miss after clear", {
            let out = run_cli(
                &bin,
                &project_root,
                &["search", "after cli clear test", "--format", "json"],
            );
            assert!(out.status.success());
            let json = parse_json_stdout(&out);
            assert_eq!(json["cache_status"].as_str(), Some("miss"));
        });
    }

    // Seed fetch
    if suite.has_text_upstreams() {
        run_test!(report, "cli", "CLI: seed fetch", {
            let out = run_cli(&bin, &project_root, &["seed", "fetch", "rust safety"]);
            assert!(
                out.status.success(),
                "CLI seed fetch failed: {}",
                stderr(&out)
            );
        });

        run_test!(report, "cli", "CLI: seed list", {
            let out = run_cli(&bin, &project_root, &["seed", "list", "--json"]);
            assert!(
                out.status.success(),
                "CLI seed list failed: {}",
                stderr(&out)
            );
            let json = parse_json_stdout(&out);
            assert!(
                json.is_array() || json.is_object(),
                "Expected JSON output from seed list"
            );
        });
    }
}

fn conproxy_bin() -> PathBuf {
    if let Ok(bin) = std::env::var("PROXY_BIN") {
        return PathBuf::from(bin);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("release")
        .join("conproxy")
}

fn run_cli(bin: &PathBuf, dir: &PathBuf, args: &[&str]) -> Output {
    Command::new(bin)
        .current_dir(dir)
        .args(args)
        .output()
        .expect("Failed to run conproxy CLI")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn parse_json_stdout(out: &Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(&stdout).unwrap_or(serde_json::Value::Null)
}
