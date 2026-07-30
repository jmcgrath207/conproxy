use super::*;

#[test]
fn test_sandbox_config_default() {
    let config = SandboxConfig::default();
    assert!(config.user.is_some());
    assert!(config.group.is_some());
    assert!(config.drop_capabilities);
    assert!(!config.allow_net_bind);
}

#[test]
fn test_current_uid_gid() {
    // Just test that these don't panic
    let uid = current_uid();
    let gid = current_gid();
    // Should be valid IDs (most systems have IDs < 100000)
    assert!(uid < 100000);
    assert!(gid < 100000);
}

#[test]
fn test_is_root() {
    // In CI, we're usually not root
    // Just test that the function works
    let _ = is_root();
}

#[test]
fn test_get_status() {
    let status = get_status();
    // Status should have valid fields
    assert!(status.uid > 0 || status.is_root);
}

#[test]
fn test_sandbox_error_display() {
    let err = SandboxError::PrivilegeDrop("test".to_string());
    assert!(err.to_string().contains("privilege"));
    assert!(err.to_string().contains("test"));

    let err = SandboxError::UserNotFound("nobody".to_string());
    assert!(err.to_string().contains("nobody"));
}

#[test]
fn test_needs_sandbox_returns_bool() {
    // On non-root CI, needs_sandbox() should return false (no capabilities)
    // On root with capabilities, it returns true
    let result = needs_sandbox();
    // Just verify it doesn't panic and returns a reasonable value
    if is_root() {
        // Root typically has capabilities — should need sandbox
        assert!(result, "Root should need sandboxing");
    } else {
        // Non-root typically has no capabilities
        assert!(!result, "Non-root should not need sandboxing");
    }
}

// Note: Tests that actually apply the sandbox require root privileges
// and should be run in a separate test environment (e.g., Docker container)

#[test]
#[ignore] // Requires root to test
fn test_apply_sandbox_requires_root() {
    // This test would require running as root in a container
    // apply_sandbox(&SandboxConfig::default()).unwrap();
}
