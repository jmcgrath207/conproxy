#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

//! UAT tests for thin `conproxy scope` / deprecated `seed` alias (plan 07).
//!
//! Config-only — no running proxy needed for list. Fat mutate CLI removed.

mod common;

use common::*;
use std::fs;
use std::path::Path;

fn write_scope_config(dir: &Path, phrases: &[&str]) {
    write_project_config(dir);
    let mut body = String::from(
        "[upstreams.dummy]\nurl = \"http://127.0.0.1:1\"\ntype = \"elasticsearch\"\nindex = \"test\"\n\n\
         [contexts.default]\ndefault = true\n\n\
         [[contexts.default.upstreams]]\nref = \"dummy\"\npriority = 0\n\n\
         [contexts.default.scope]\nenabled = true\n\n",
    );
    for p in phrases {
        body.push_str(&format!(
            "[[contexts.default.scope.weighted_phrases]]\ntext = \"{p}\"\nweight = 1.0\n\n"
        ));
    }
    fs::write(dir.join(".conproxy/conproxy.toml"), body).expect("write scope config");
}

#[test]
fn test_scope_list_empty() {
    let dir = temp_dir();
    write_project_config(dir.path());
    let output = run_conproxy_in(dir.path(), &["scope", "list"]);
    assert_success(&output);
    assert!(
        stdout_contains(&output, "No scope phrases configured"),
        "expected empty scope list message, got: {}",
        stdout(&output)
    );
}

#[test]
fn test_seed_alias_list_empty() {
    let dir = temp_dir();
    write_project_config(dir.path());
    let output = run_conproxy_in(dir.path(), &["seed", "list"]);
    assert_success(&output);
    assert!(
        stdout_contains(&output, "No scope phrases configured"),
        "seed alias should list scope phrases"
    );
}

#[test]
fn test_scope_list_json_empty() {
    let dir = temp_dir();
    write_project_config(dir.path());
    let output = run_conproxy_in(dir.path(), &["scope", "list", "--json"]);
    assert_success(&output);
    assert_eq!(stdout(&output).trim(), "[]");
}

#[test]
fn test_scope_list_phrases() {
    let dir = temp_dir();
    write_scope_config(dir.path(), &["rust async", "error handling"]);

    let output = run_conproxy_in(dir.path(), &["scope", "list"]);
    assert_success(&output);
    let out = stdout(&output);
    assert!(out.contains("rust async"), "should list first phrase");
    assert!(out.contains("error handling"), "should list second phrase");
    assert!(
        out.contains("Scope phrases (2 configured)"),
        "should show phrase count: {out}"
    );
}

#[test]
fn test_scope_list_json() {
    let dir = temp_dir();
    write_scope_config(dir.path(), &["rust async", "error handling"]);

    let output = run_conproxy_in(dir.path(), &["scope", "list", "--json"]);
    assert_success(&output);

    let json = parse_json_output(&output);
    let arr = json.as_array().expect("expected JSON array");
    assert_eq!(arr.len(), 2, "expected 2 phrases in JSON output");
    assert_eq!(arr[0]["phrase"], "rust async");
    assert_eq!(arr[1]["seed"], "error handling");
}

#[test]
fn test_seed_alias_list_phrases() {
    let dir = temp_dir();
    write_scope_config(dir.path(), &["rust async"]);
    let output = run_conproxy_in(dir.path(), &["seed", "list", "--json"]);
    assert_success(&output);
    let json = parse_json_output(&output);
    let arr = json.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["phrase"], "rust async");
}

#[test]
fn test_fat_scope_mutate_removed() {
    let dir = temp_dir();
    write_project_config(dir.path());
    for args in [
        &["scope", "add", "x"][..],
        &["seed", "add", "x"][..],
        &["scope", "remove", "1"][..],
        &["seed", "fetch", "q"][..],
        &["seed", "info", "q"][..],
        &["seed", "lookup", "q"][..],
    ] {
        let output = run_conproxy_in(dir.path(), args);
        assert!(
            !output.status.success(),
            "fat subcommand should fail: {args:?}\n{}",
            stderr(&output)
        );
    }
}

#[test]
fn test_scope_clear_help() {
    let dir = temp_dir();
    write_project_config(dir.path());
    let output = run_conproxy_in(dir.path(), &["scope", "clear", "--help"]);
    assert_success(&output);
    assert!(
        stdout_contains(&output, "Clear") || stdout_contains(&output, "clear"),
        "clear help should mention clear"
    );
}
