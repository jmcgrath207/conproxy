use std::path::PathBuf;
use std::process::{Command, Output};

fn run_cli(args: &[&str]) -> Output {
    let bin = if let Ok(b) = std::env::var("PROXY_BIN") {
        PathBuf::from(b)
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("release")
            .join("conproxy")
    };
    Command::new(bin)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(args)
        .output()
        .expect("Failed to run conproxy CLI")
}

#[test]
#[ignore = "requires release build; needs live upstreams"]
fn test_distill_help() {
    let output = run_cli(&["distill", "--help"]);
    assert!(
        output.status.success(),
        "distill --help should exit 0 (stderr: {})",
        String::from_utf8_lossy(&output.stderr)
    );
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(
        out.contains("Usage:") || out.contains("distill"),
        "help output should contain usage info, got: {}",
        out
    );
}

#[test]
#[ignore = "needs live upstreams + release build"]
fn test_distill_smoke() {
    let output = run_cli(&["distill", "--limit", "1", "--cat"]);
    // Assert command ran to completion (didn't crash with signal)
    assert!(
        output.status.code().is_some(),
        "distill smoke terminated by signal"
    );
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(!out.is_empty(), "distill should produce output");
}
