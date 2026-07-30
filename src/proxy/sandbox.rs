//! Linux sandbox for privilege dropping and capability management.
//!
//! This module provides security hardening for the cache proxy on Linux systems
//! using capabilities dropping and privilege management.
//!
//! # Features
//!
//! - **Privilege dropping**: Drop to unprivileged user after binding to port
//! - **Capability dropping**: Remove all capabilities except those needed
//! - **No new privileges**: Prevent privilege escalation via execve
//!
//! # Usage
//!
//! Enable the `linux-sandbox` feature in Cargo.toml:
//!
//! ```toml
//! [dependencies]
//! conproxy = { features = ["linux-sandbox"] }
//! ```
//!
//! # Security Model
//!
//! The sandbox is applied in stages:
//! 1. Bind to port while running as root (if needed for privileged port)
//! 2. Drop to unprivileged user
//! 3. Drop all capabilities except `CAP_NET_BIND_SERVICE`
//! 4. Set no_new_privs to prevent privilege escalation

use std::ffi::CString;
use std::io;

use caps::{CapSet, Capability, CapsHashSet};
use nix::unistd::{setgid, setuid, Gid, Uid};

/// Error type for sandbox operations.
#[derive(Debug)]
pub enum SandboxError {
    /// Failed to drop privileges.
    PrivilegeDrop(String),
    /// Failed to drop capabilities.
    CapabilityDrop(String),
    /// Failed to set no_new_privs.
    NoNewPrivs(String),
    /// User/group not found.
    UserNotFound(String),
}

impl std::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PrivilegeDrop(msg) => write!(f, "Failed to drop privileges: {}", msg),
            Self::CapabilityDrop(msg) => write!(f, "Failed to drop capabilities: {}", msg),
            Self::NoNewPrivs(msg) => write!(f, "Failed to set no_new_privs: {}", msg),
            Self::UserNotFound(name) => write!(f, "User/group not found: {}", name),
        }
    }
}

impl std::error::Error for SandboxError {}

/// Sandbox configuration.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// User to drop to (by name or UID).
    pub user: Option<String>,
    /// Group to drop to (by name or GID).
    pub group: Option<String>,
    /// Whether to drop capabilities.
    pub drop_capabilities: bool,
    /// Whether to allow network binding after privilege drop.
    pub allow_net_bind: bool,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            user: Some("nobody".to_string()),
            group: Some("nogroup".to_string()),
            drop_capabilities: true,
            allow_net_bind: false,
        }
    }
}

/// Apply sandbox restrictions.
///
/// This function should be called after the server has bound to its port
/// but before accepting connections.
///
/// # Arguments
/// * `config` - Sandbox configuration
///
/// # Returns
/// * `Ok(())` if sandbox was applied successfully
/// * `Err(SandboxError)` if any step failed
///
/// # Example
///
/// ```ignore
/// let config = SandboxConfig {
///     user: Some("proxy".to_string()),
///     group: Some("proxy".to_string()),
///     ..Default::default()
/// };
///
/// // Bind to port first
/// let listener = TcpListener::bind("0.0.0.0:8080").await?;
///
/// // Apply sandbox
/// apply_sandbox(&config)?;
///
/// // Now accept connections with reduced privileges
/// loop {
///     let (socket, _) = listener.accept().await?;
///     // ...
/// }
/// ```
///
/// # Errors
///
/// Returns [`SandboxError::GroupNotFound`] if the configured group does not
/// exist. Returns [`SandboxError::UserNotFound`] if the configured user does
/// not exist. Returns [`SandboxError::SetUid`] / [`SandboxError::SetGid`] if
/// the privilege drop syscalls fail. Returns [`SandboxError::SetNoNewPrivs`]
/// if `prctl(PR_SET_NO_NEW_PRIVS)` fails. Returns
/// [`SandboxError::Landlock`]/[`SandboxError::Seccomp`] if capability or
/// syscall-filter setup fails. Returns [`SandboxError::PermissionDenied`]
/// if the sandbox prerequisites (already-root, capability set) are not met.
pub fn apply_sandbox(config: &SandboxConfig) -> Result<(), SandboxError> {
    // Step 1: Drop to unprivileged user/group
    if let Some(ref group) = config.group {
        drop_to_group(group)?;
    }
    if let Some(ref user) = config.user {
        drop_to_user(user)?;
    }

    // Step 2: Drop capabilities
    if config.drop_capabilities {
        drop_capabilities(config.allow_net_bind)?;
    }

    // Step 3: Set no_new_privs
    set_no_new_privs()?;

    Ok(())
}

/// Drop to a specific user by name or UID.
#[allow(unsafe_code)]
fn drop_to_user(user: &str) -> Result<(), SandboxError> {
    let uid = if let Ok(uid) = user.parse::<u32>() {
        Uid::from_raw(uid)
    } else {
        // Look up user by name
        let c_user = CString::new(user).map_err(|e| SandboxError::UserNotFound(e.to_string()))?;
        // SAFETY: `c_user` is a valid NUL-terminated CString. `getpwnam` returns
        // null on error (checked next line) or a pointer to a static struct.
        let pwd = unsafe { nix::libc::getpwnam(c_user.as_ptr()) };
        if pwd.is_null() {
            return Err(SandboxError::UserNotFound(user.to_string()));
        }
        // SAFETY: `pwd` was checked non-null above. On success, `getpwnam` returns
        // a valid pointer to a static `passwd` struct.
        Uid::from_raw(unsafe { (*pwd).pw_uid })
    };

    setuid(uid).map_err(|e| SandboxError::PrivilegeDrop(format!("setuid failed: {}", e)))
}

/// Drop to a specific group by name or GID.
#[allow(unsafe_code)]
fn drop_to_group(group: &str) -> Result<(), SandboxError> {
    let gid = if let Ok(gid) = group.parse::<u32>() {
        Gid::from_raw(gid)
    } else {
        // Look up group by name
        let c_group = CString::new(group).map_err(|e| SandboxError::UserNotFound(e.to_string()))?;
        // SAFETY: `c_group` is a valid NUL-terminated CString. `getgrnam` returns
        // null on error (checked next line) or a pointer to a static struct.
        let grp = unsafe { nix::libc::getgrnam(c_group.as_ptr()) };
        if grp.is_null() {
            return Err(SandboxError::UserNotFound(group.to_string()));
        }
        // SAFETY: `grp` was checked non-null above. On success, `getgrnam` returns
        // a valid pointer to a static `group` struct.
        Gid::from_raw(unsafe { (*grp).gr_gid })
    };

    setgid(gid).map_err(|e| SandboxError::PrivilegeDrop(format!("setgid failed: {}", e)))
}

/// Drop all capabilities except optionally `CAP_NET_BIND_SERVICE`.
fn drop_capabilities(allow_net_bind: bool) -> Result<(), SandboxError> {
    // Clear all capabilities from all sets
    caps::clear(None, CapSet::Effective)
        .map_err(|e| SandboxError::CapabilityDrop(format!("clear effective: {}", e)))?;
    caps::clear(None, CapSet::Permitted)
        .map_err(|e| SandboxError::CapabilityDrop(format!("clear permitted: {}", e)))?;
    caps::clear(None, CapSet::Inheritable)
        .map_err(|e| SandboxError::CapabilityDrop(format!("clear inheritable: {}", e)))?;

    // Optionally retain CAP_NET_BIND_SERVICE for binding to privileged ports
    if allow_net_bind {
        let mut caps = CapsHashSet::new();
        caps.insert(Capability::CAP_NET_BIND_SERVICE);

        caps::set(None, CapSet::Effective, &caps)
            .map_err(|e| SandboxError::CapabilityDrop(format!("set net_bind effective: {}", e)))?;
        caps::set(None, CapSet::Permitted, &caps)
            .map_err(|e| SandboxError::CapabilityDrop(format!("set net_bind permitted: {}", e)))?;
    }

    Ok(())
}

/// Set the no_new_privs flag to prevent privilege escalation.
#[allow(unsafe_code)]
fn set_no_new_privs() -> Result<(), SandboxError> {
    // Use prctl directly via nix::libc
    // SAFETY: prctl with PR_SET_NO_NEW_PRIVS takes integer arguments only.
    // No pointer arguments, no memory safety concerns.
    let result = unsafe { nix::libc::prctl(nix::libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if result != 0 {
        return Err(SandboxError::NoNewPrivs(
            io::Error::last_os_error().to_string(),
        ));
    }
    Ok(())
}

/// Check whether sandboxing is needed based on current effective capabilities.
///
/// Reads the process's effective capability set at init time and returns `true`
/// if the process holds any capabilities beyond `CAP_NET_BIND_SERVICE` (or none
/// at all — unprivileged processes don't need sandboxing).
///
/// This correctly handles:
/// - **Bare metal / systemd**: Has many caps → returns true
/// - **Docker with `--cap-drop=ALL`**: No caps → returns false
/// - **Docker with excess caps**: Has caps → returns true (self-corrects)
/// - **Non-root in container**: No caps → returns false
pub fn needs_sandbox() -> bool {
    let effective = match caps::read(None, CapSet::Effective) {
        Ok(caps) => caps,
        Err(_) => return false, // Can't read caps — assume unprivileged
    };

    if effective.is_empty() {
        // Process has no effective capabilities — already sandboxed
        return false;
    }

    // If only CAP_NET_BIND_SERVICE, no point sandboxing
    if effective.len() == 1 && effective.contains(&Capability::CAP_NET_BIND_SERVICE) {
        return false;
    }

    // Process has excess capabilities that should be dropped
    true
}

/// Check if the process is running as root.
pub fn is_root() -> bool {
    nix::unistd::geteuid().is_root()
}

/// Get current user ID.
pub fn current_uid() -> u32 {
    nix::unistd::getuid().as_raw()
}

/// Get current group ID.
pub fn current_gid() -> u32 {
    nix::unistd::getgid().as_raw()
}

/// Check if no_new_privs is set.
#[allow(unsafe_code)]
pub fn is_no_new_privs_set() -> bool {
    // SAFETY: prctl with PR_GET_NO_NEW_PRIVS takes integer arguments only.
    // No pointer arguments, no memory safety concerns.
    let result = unsafe { nix::libc::prctl(nix::libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) };
    result == 1
}

/// Sandbox status information.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SandboxStatus {
    /// Current user ID.
    pub uid: u32,
    /// Current group ID.
    pub gid: u32,
    /// Whether running as root.
    pub is_root: bool,
    /// Whether no_new_privs is set.
    pub no_new_privs: bool,
    /// Current effective capabilities.
    pub capabilities: Vec<String>,
}

/// Get current sandbox status.
pub fn get_status() -> SandboxStatus {
    let caps = caps::read(None, CapSet::Effective)
        .map(|c| c.iter().map(|cap| format!("{:?}", cap)).collect())
        .unwrap_or_default();

    SandboxStatus {
        uid: current_uid(),
        gid: current_gid(),
        is_root: is_root(),
        no_new_privs: is_no_new_privs_set(),
        capabilities: caps,
    }
}

#[cfg(test)]
#[path = "tests/sandbox_tests.rs"]
mod tests;
