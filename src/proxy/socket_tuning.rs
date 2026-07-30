//! OS-level socket tuning for proxy listeners and upstream clients.

use crate::config::SocketTuningConfig;
use std::net::SocketAddr;
use std::time::Duration;
use tracing::{info, warn};

/// Create a TCP listener socket with production-tuned options.
///
/// Applied options (controlled by `config`):
/// - SO_REUSEADDR — allows quick restart after crash
/// - SO_REUSEPORT — kernel-level load balancing across accept threads (Unix)
/// - SO_KEEPALIVE — detects dead connections
/// - TCP_NODELAY — disables Nagle's algorithm
/// - TCP_DEFER_ACCEPT — kernel waits for data before waking accept() (Linux)
/// - TCP_USER_TIMEOUT — caps unacknowledged data lifetime (Linux)
/// - Optional send/receive buffer sizes
/// - Configurable listen backlog
pub fn create_tuned_listener(
    addr: SocketAddr,
    config: &SocketTuningConfig,
) -> std::io::Result<tokio::net::TcpListener> {
    use socket2::{Domain, Protocol, Socket, TcpKeepalive, Type};

    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;

    // SO_REUSEPORT: kernel-level load balancing across accept threads
    #[cfg(unix)]
    if config.reuse_port {
        socket.set_reuse_port(true)?;
    }

    // TCP_NODELAY: disable Nagle's algorithm so accepted connections inherit it
    if config.tcp_nodelay {
        socket.set_tcp_nodelay(true)?;
    }

    // SO_KEEPALIVE: detect dead connections
    socket.set_tcp_keepalive(
        &TcpKeepalive::new()
            .with_time(Duration::from_secs(config.tcp_keepalive_secs))
            .with_interval(Duration::from_secs(config.tcp_keepalive_interval)),
    )?;

    // Optional send/receive buffer sizes (skip to preserve OS autotuning)
    if let Some(size) = config.send_buffer_size {
        socket.set_send_buffer_size(size)?;
    }
    if let Some(size) = config.recv_buffer_size {
        socket.set_recv_buffer_size(size)?;
    }

    // TCP_DEFER_ACCEPT: don't wake accept() until client sends data
    #[cfg(target_os = "linux")]
    set_defer_accept(&socket, config.defer_accept_secs)?;

    // TCP_USER_TIMEOUT: max time for unacknowledged data
    #[cfg(target_os = "linux")]
    set_user_timeout(&socket, config.user_timeout_ms)?;

    socket.bind(&addr.into())?;
    socket.listen(config.listen_backlog as i32)?;

    let std_listener: std::net::TcpListener = socket.into();
    tokio::net::TcpListener::from_std(std_listener)
}

/// Create a reqwest `ClientBuilder` with production TCP tuning for upstream connections.
///
/// Sets TCP_NODELAY, keepalive, connection pool sizing, and timeout.
/// Returns a `ClientBuilder` so callers can add headers/auth before `.build()`.
pub fn create_tuned_client(
    timeout: Duration,
    config: &SocketTuningConfig,
) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .tcp_nodelay(config.tcp_nodelay)
        .tcp_keepalive(Duration::from_secs(config.tcp_keepalive_secs))
        .pool_idle_timeout(Duration::from_secs(config.upstream_pool_idle_timeout_secs))
        .pool_max_idle_per_host(config.upstream_pool_max_idle)
        .timeout(timeout)
}

/// Create a reqwest `ClientBuilder` with default socket tuning.
///
/// Convenience wrapper that uses `SocketTuningConfig::default()`.
pub fn create_tuned_client_default(timeout: Duration) -> reqwest::ClientBuilder {
    create_tuned_client(timeout, &SocketTuningConfig::default())
}

/// Log a warning if the file descriptor limit is too low for proxy workloads.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn check_fd_limit() {
    let mut rlim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `rlim` is a valid stack-allocated rlimit struct, `getrlimit` writes
    // into it. No alignment or aliasing issues.
    let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlim) };
    if rc == 0 {
        if rlim.rlim_cur < 4096 {
            warn!(
                limit = rlim.rlim_cur,
                "File descriptor limit is low (recommended: >=65535). \
                 Set `ulimit -n 65535` or Docker `--ulimit nofile=65535:65535`"
            );
        } else {
            info!(limit = rlim.rlim_cur, "File descriptor limit");
        }
    }
}

#[cfg(not(unix))]
pub fn check_fd_limit() {}

/// Read a sysctl value from `/proc/sys/...` (Linux only).
#[cfg(target_os = "linux")]
fn read_sysctl(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// Check Linux sysctls and log warnings for suboptimal networking settings.
///
/// Informational only — never modifies sysctls.
#[cfg(target_os = "linux")]
pub fn check_sysctl_recommendations() {
    struct SysctlCheck {
        path: &'static str,
        name: &'static str,
        min: u64,
        why: &'static str,
    }

    let checks = [
        SysctlCheck {
            path: "/proc/sys/net/core/somaxconn",
            name: "net.core.somaxconn",
            min: 4096,
            why: "must match listen backlog for burst handling",
        },
        SysctlCheck {
            path: "/proc/sys/net/ipv4/tcp_max_syn_backlog",
            name: "net.ipv4.tcp_max_syn_backlog",
            min: 4096,
            why: "SYN queue size for connection burst handling",
        },
        SysctlCheck {
            path: "/proc/sys/net/core/netdev_max_backlog",
            name: "net.core.netdev_max_backlog",
            min: 5000,
            why: "input packet queue for burst traffic",
        },
    ];

    struct SysctlExact {
        path: &'static str,
        name: &'static str,
        expected: &'static str,
        why: &'static str,
    }

    let exact_checks = [
        SysctlExact {
            path: "/proc/sys/net/ipv4/tcp_slow_start_after_idle",
            name: "net.ipv4.tcp_slow_start_after_idle",
            expected: "0",
            why: "prevents cwnd reset after idle (hurts keep-alive connections)",
        },
        SysctlExact {
            path: "/proc/sys/net/ipv4/tcp_tw_reuse",
            name: "net.ipv4.tcp_tw_reuse",
            expected: "1",
            why: "reuse TIME_WAIT sockets for outbound connections",
        },
    ];

    let mut warnings: u32 = 0;

    for check in &checks {
        if let Some(val_str) = read_sysctl(check.path) {
            if let Ok(val) = val_str.parse::<u64>() {
                if val < check.min {
                    warn!(
                        sysctl = check.name,
                        value = val,
                        recommended_min = check.min,
                        reason = check.why,
                        "Sysctl value below recommended minimum"
                    );
                    warnings = warnings.saturating_add(1);
                }
            }
        }
    }

    for check in &exact_checks {
        if let Some(val_str) = read_sysctl(check.path) {
            if val_str.trim() != check.expected {
                warn!(
                    sysctl = check.name,
                    value = val_str.trim(),
                    recommended = check.expected,
                    reason = check.why,
                    "Sysctl value differs from recommended"
                );
                warnings = warnings.saturating_add(1);
            }
        }
    }

    // Check tcp_fin_timeout (should be <= 15)
    if let Some(val_str) = read_sysctl("/proc/sys/net/ipv4/tcp_fin_timeout") {
        if let Ok(val) = val_str.parse::<u64>() {
            if val > 15 {
                warn!(
                    sysctl = "net.ipv4.tcp_fin_timeout",
                    value = val,
                    recommended_max = 15,
                    "tcp_fin_timeout too high, reduces TIME_WAIT duration"
                );
                warnings = warnings.saturating_add(1);
            }
        }
    }

    // Check tcp_notsent_lowat (should be set to 131072)
    if let Some(val_str) = read_sysctl("/proc/sys/net/ipv4/tcp_notsent_lowat") {
        if let Ok(val) = val_str.parse::<u64>() {
            if val == u64::MAX || val > 131_072 {
                warn!(
                    sysctl = "net.ipv4.tcp_notsent_lowat",
                    value = val,
                    recommended = 131072,
                    "tcp_notsent_lowat too high, reduces send bufferbloat"
                );
                warnings = warnings.saturating_add(1);
            }
        }
    }

    if warnings == 0 {
        info!("Sysctl check: all networking parameters look good");
    } else {
        warn!(
            warnings,
            "Sysctl check: see https://docs.conproxy.dev/tuning for details"
        );
    }
}

#[cfg(not(target_os = "linux"))]
pub fn check_sysctl_recommendations() {
    // Sysctl checks only apply to Linux
}

/// Set TCP_DEFER_ACCEPT (Linux only).
/// Value is timeout in seconds — kernel waits for data before completing accept().
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn set_defer_accept(socket: &socket2::Socket, secs: i32) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let fd = socket.as_raw_fd();
    // SAFETY: `fd` is a valid socket fd, `secs` is a valid i32 on the stack.
    // `setsockopt` copies the value, no ownership transfer. Size matches type.
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_DEFER_ACCEPT,
            &secs as *const i32 as *const libc::c_void,
            std::mem::size_of::<i32>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Set TCP_USER_TIMEOUT (Linux only).
/// Value is timeout in milliseconds — max time unacknowledged data may remain.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn set_user_timeout(socket: &socket2::Socket, millis: u32) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let fd = socket.as_raw_fd();
    // SAFETY: `fd` is a valid socket fd, `millis` is a valid u32 on the stack.
    // `setsockopt` copies the value, no ownership transfer. Size matches type.
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_USER_TIMEOUT,
            &millis as *const u32 as *const libc::c_void,
            std::mem::size_of::<u32>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[tokio::test]
    async fn test_create_tuned_listener_port_zero() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let config = SocketTuningConfig::default();
        let listener = create_tuned_listener(addr, &config).unwrap();
        let local = listener.local_addr().unwrap();
        assert_ne!(local.port(), 0); // OS assigned a real port
    }

    #[tokio::test]
    async fn test_create_tuned_listener_with_buffer_sizes() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let config = SocketTuningConfig {
            send_buffer_size: Some(65536),
            recv_buffer_size: Some(65536),
            ..Default::default()
        };
        let listener = create_tuned_listener(addr, &config).unwrap();
        assert_ne!(listener.local_addr().unwrap().port(), 0);
    }

    #[test]
    fn test_create_tuned_client_returns_builder() {
        let config = SocketTuningConfig::default();
        let builder = create_tuned_client(Duration::from_secs(30), &config);
        // Builder should produce a valid client
        let client = builder.build().unwrap();
        drop(client);
    }

    #[test]
    fn test_create_tuned_client_default_returns_builder() {
        let builder = create_tuned_client_default(Duration::from_secs(10));
        let client = builder.build().unwrap();
        drop(client);
    }

    #[test]
    fn test_create_tuned_client_custom_config() {
        let config = SocketTuningConfig {
            tcp_nodelay: false,
            tcp_keepalive_secs: 120,
            upstream_pool_idle_timeout_secs: 30,
            upstream_pool_max_idle: 5,
            ..Default::default()
        };
        let builder = create_tuned_client(Duration::from_secs(5), &config);
        let client = builder.build().unwrap();
        drop(client);
    }

    #[test]
    fn test_check_fd_limit_no_panic() {
        // Should not panic on any platform
        check_fd_limit();
    }

    #[test]
    fn test_check_sysctl_recommendations_no_panic() {
        // Should not panic on any platform (no-op on non-Linux)
        check_sysctl_recommendations();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_read_sysctl_valid_path() {
        // somaxconn should always exist on Linux
        let val = read_sysctl("/proc/sys/net/core/somaxconn");
        assert!(val.is_some());
        assert!(!val.unwrap().is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_read_sysctl_invalid_path() {
        let val = read_sysctl("/proc/sys/nonexistent/value");
        assert!(val.is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_set_defer_accept_on_new_socket() {
        use socket2::{Domain, Protocol, Socket, Type};
        let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)).unwrap();
        let result = set_defer_accept(&socket, 5);
        assert!(result.is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_set_user_timeout_on_new_socket() {
        use socket2::{Domain, Protocol, Socket, Type};
        let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)).unwrap();
        let result = set_user_timeout(&socket, 10000);
        assert!(result.is_ok());
    }
}
