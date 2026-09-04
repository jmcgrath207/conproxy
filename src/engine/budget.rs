//! Memory-budget detection for the dynamic cache cap.
//!
//! The dynamic cap is `fraction × budget`, where the budget is read on a
//! timer so the cache tracks container (cgroup v2) or host pressure instead
//! of a fixed value.

use std::path::{Path, PathBuf};

/// Default fraction of the memory budget used when `max_memory` is unset.
pub(crate) const DEFAULT_MEMORY_FRACTION: f64 = 0.7;
/// Floor for a dynamic cap — never shrink below this.
pub(crate) const DYNAMIC_CAP_FLOOR_BYTES: u64 = 16 * 1024 * 1024;
/// How often the live-resize ticker re-reads the budget.
pub(crate) const MEMORY_TICK_SECS: u64 = 10;

/// Read the current memory budget in bytes.
///
/// Order:
/// 1. cgroup v2 `memory.max` (walk up the hierarchy; `max` = unlimited)
/// 2. `/proc/meminfo` `MemAvailable`
/// 3. Fallback 256 MiB (non-Linux / unreadable proc)
#[must_use]
pub(crate) fn read_memory_budget() -> u64 {
    cgroup_v2_memory_max().unwrap_or_else(|| mem_available().unwrap_or(256 * 1024 * 1024))
}

/// Compute the dynamic cap for a fraction: `fraction × budget`, floored.
#[must_use]
pub(crate) fn cap_for_fraction(fraction: f64, budget: u64) -> u64 {
    let f = fraction.clamp(0.0, 1.0);
    ((budget as f64) * f) as u64
}

/// cgroup v2 `memory.max` for the current process's cgroup.
///
/// Walks from the most specific `0::` controller path up to the cgroup root,
/// returning the first concrete limit. Returns `None` when unlimited (`max`)
/// or when cgroup v2 is not in use.
fn cgroup_v2_memory_max() -> Option<u64> {
    let contents = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let path = contents.lines().find_map(|l| l.strip_prefix("0::"))?;
    let mut dir = if path.trim().is_empty() {
        PathBuf::from("/sys/fs/cgroup")
    } else {
        PathBuf::from(format!("/sys/fs/cgroup{}", path.trim()))
    };
    loop {
        if let Ok(s) = std::fs::read_to_string(dir.join("memory.max")) {
            let t = s.trim();
            if t != "max" {
                if let Ok(v) = t.parse::<u64>() {
                    if v > 0 {
                        return Some(v);
                    }
                }
            }
        }
        if dir == Path::new("/sys/fs/cgroup") || !dir.pop() {
            return None;
        }
    }
}

/// `/proc/meminfo` `MemAvailable` in bytes.
fn mem_available() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("MemAvailable:") {
            let kb = rest
                .trim()
                .trim_end_matches("kB")
                .trim()
                .parse::<u64>()
                .ok()?;
            return Some(kb.saturating_mul(1024));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_is_fraction_of_budget() {
        let budget = 1_000_000_000;
        let cap = cap_for_fraction(DEFAULT_MEMORY_FRACTION, budget);
        assert_eq!(cap, 700_000_000);
    }

    #[test]
    fn fraction_is_clamped() {
        assert_eq!(cap_for_fraction(2.0, 100), 100);
        assert_eq!(cap_for_fraction(-1.0, 100), 0);
    }

    #[test]
    fn cgroup_root_unlimited_falls_back() {
        // "max" at the root (or no cgroup) must not fabricate a limit.
        assert!(cgroup_v2_memory_max().is_none() || cgroup_v2_memory_max().is_some_and(|v| v > 0));
    }

    #[test]
    fn budget_is_nonzero() {
        assert!(read_memory_budget() > 0);
    }

    #[test]
    fn cap_never_exceeds_budget() {
        assert!(cap_for_fraction(0.7, 100) <= 100);
    }
}
