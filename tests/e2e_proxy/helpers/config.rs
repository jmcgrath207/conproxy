use std::path::{Path, PathBuf};

use super::constants::Suite;

/// Manages proxy config injection and restoration for E2E tests.
pub struct ConfigManager {
    config_path: PathBuf,
    backup_path: PathBuf,
    configs_dir: PathBuf,
    backed_up: bool,
}

impl ConfigManager {
    pub fn new(project_root: &Path) -> Self {
        Self {
            config_path: project_root.join(".conproxy/conproxy.toml"),
            backup_path: project_root.join(".conproxy/conproxy.toml.e2e-backup"),
            configs_dir: project_root.join("tests/e2e/configs"),
            backed_up: false,
        }
    }

    /// Copy the suite-specific config from `tests/e2e/configs/` into `.conproxy/conproxy.toml`.
    pub fn inject_suite_config(&mut self, suite: Suite) {
        // Ensure .conproxy dir exists
        if let Some(parent) = self.config_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // Back up existing config
        if self.config_path.exists() && !self.backed_up {
            std::fs::copy(&self.config_path, &self.backup_path).expect("Failed to back up config");
            self.backed_up = true;
            eprintln!("  Backed up config to {}", self.backup_path.display());
        }

        let src = self.configs_dir.join(suite.config_name());
        std::fs::copy(&src, &self.config_path).unwrap_or_else(|e| {
            panic!(
                "Failed to copy {} -> {}: {}",
                src.display(),
                self.config_path.display(),
                e
            )
        });
        eprintln!(
            "  Injected config: {} (suite={})",
            suite.config_name(),
            suite
        );
    }

    /// Write the advanced config (auth + rate_limit + short TTL) inline.
    pub fn write_advanced_config(&mut self) {
        if let Some(parent) = self.config_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // Back up if not already
        if self.config_path.exists() && !self.backed_up {
            std::fs::copy(&self.config_path, &self.backup_path).expect("Failed to back up config");
            self.backed_up = true;
        }

        let config = r#"[server]
listen = "127.0.0.1:8080"

[upstreams.elasticsearch-primary]
url = "http://localhost:9200"
type = "elasticsearch"
index = "conproxy_test"
search_fields = ["content", "title"]
timeout_secs = 30

[upstreams.elasticsearch-secondary]
url = "http://localhost:9201"
type = "elasticsearch"
index = "conproxy_test"
search_fields = ["content", "title"]
timeout_secs = 30

[contexts.default]
default = true

[[contexts.default.upstreams]]
ref = "elasticsearch-primary"
priority = 0
weight = 1

[[contexts.default.upstreams]]
ref = "elasticsearch-secondary"
priority = 1
weight = 1

[contexts.default.cache]
fresh_secs = 5
stale_secs = 10
max_entries = 1000

[proxy]
api_key = "e2e-test-key"

[proxy.retry]
enabled = true
max_retries = 2
base_delay_ms = 50
max_delay_ms = 2000

[proxy.rate_limit]
enabled = true
requests_per_second = 10
burst_size = 5
"#;
        std::fs::write(&self.config_path, config).expect("Failed to write advanced config");
        eprintln!("  Wrote advanced config (auth + rate_limit + short TTL)");
    }

    /// Write a config pointing at a mock upstream URL with the specified type.
    pub fn write_mock_config(&mut self, mock_url: &str, upstream_type: &str) {
        if let Some(parent) = self.config_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if self.config_path.exists() && !self.backed_up {
            std::fs::copy(&self.config_path, &self.backup_path).expect("Failed to back up config");
            self.backed_up = true;
        }

        let config = format!(
            r#"[server]
listen = "127.0.0.1:8080"

[upstreams.mock]
url = "{mock_url}"
type = "{upstream_type}"
index = "mock_index"
search_fields = ["content", "title"]
timeout_secs = 3

[contexts.default]
default = true

[[contexts.default.upstreams]]
ref = "mock"
priority = 0
weight = 1

[contexts.default.cache]
fresh_secs = 5
stale_secs = 10
max_entries = 1000

[proxy.retry]
enabled = true
max_retries = 3
base_delay_ms = 100
max_delay_ms = 2000

[proxy.circuit_breaker]
failure_threshold = 3
success_threshold = 2
open_duration_secs = 3
failure_window_secs = 10
"#
        );
        std::fs::write(&self.config_path, config).expect("Failed to write mock config");
        eprintln!("  Wrote mock config (upstream_type={upstream_type}, url={mock_url})");
    }

    /// Write a config with agent auth pointing at a mock upstream.
    pub fn write_agent_config(&mut self, mock_url: &str) {
        if let Some(parent) = self.config_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if self.config_path.exists() && !self.backed_up {
            std::fs::copy(&self.config_path, &self.backup_path).expect("Failed to back up config");
            self.backed_up = true;
        }

        let config = format!(
            r#"[server]
listen = "127.0.0.1:8080"

[upstreams.mock]
url = "{mock_url}"
type = "elasticsearch"
index = "mock_index"
search_fields = ["content", "title"]
timeout_secs = 5

[contexts.default]
default = true

[[contexts.default.upstreams]]
ref = "mock"
priority = 0
weight = 1

[contexts.default.cache]
fresh_secs = 5
stale_secs = 10
max_entries = 1000

[proxy]
api_key = "e2e-global-key"

[[proxy.agents]]
id = "seed-agent"
api_key = "seed-key"
enabled = true

[proxy.retry]
enabled = true
max_retries = 2
base_delay_ms = 50
max_delay_ms = 2000
"#
        );
        std::fs::write(&self.config_path, config).expect("Failed to write agent config");
        eprintln!("  Wrote agent config (url={mock_url})");
    }

    /// Write a config with federated search enabled pointing at a mock upstream.
    pub fn write_federated_config(&mut self, mock_url: &str) {
        if let Some(parent) = self.config_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if self.config_path.exists() && !self.backed_up {
            std::fs::copy(&self.config_path, &self.backup_path).expect("Failed to back up config");
            self.backed_up = true;
        }

        let config = format!(
            r#"[server]
listen = "127.0.0.1:8080"

[upstreams.mock]
url = "{mock_url}"
type = "elasticsearch"
index = "mock_index"
search_fields = ["content", "title"]
timeout_secs = 5

[contexts.default]
default = true

[[contexts.default.upstreams]]
ref = "mock"
priority = 0
weight = 1

[contexts.default.cache]
fresh_secs = 5
stale_secs = 10
max_entries = 1000

[contexts.default.federated]
enabled = true
merge_mode = "local_only_fallback"
min_local_results = 3
min_local_confidence = 0.7

[proxy.retry]
enabled = true
max_retries = 2
base_delay_ms = 50
max_delay_ms = 2000
"#
        );
        std::fs::write(&self.config_path, config).expect("Failed to write federated config");
        eprintln!("  Wrote federated config (url={mock_url})");
    }

    /// Update max_entries in the current config file (for reload tests).
    pub fn set_max_entries(&self, value: u64) {
        let content = std::fs::read_to_string(&self.config_path)
            .expect("Failed to read config for modification");
        let updated = content
            .lines()
            .map(|line| {
                if line.trim_start().starts_with("max_entries") {
                    format!("max_entries = {}", value)
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&self.config_path, updated).expect("Failed to write modified config");
    }

    /// Context-rooted mock config (plan 10): `[server]` + `[upstreams.*]` + `[contexts.default]`.
    pub fn write_context_rooted_mock(&mut self, mock_url: &str, upstream_type: &str) {
        if let Some(parent) = self.config_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if self.config_path.exists() && !self.backed_up {
            std::fs::copy(&self.config_path, &self.backup_path).expect("Failed to back up config");
            self.backed_up = true;
        }

        let config = format!(
            r#"[server]
listen = "127.0.0.1:8080"

[upstreams.mock]
url = "{mock_url}"
type = "{upstream_type}"
index = "mock_index"
search_fields = ["content", "title"]
timeout_secs = 3

[contexts.default]
default = true

[[contexts.default.upstreams]]
ref = "mock"
priority = 0
weight = 1

[contexts.default.cache]
fresh_secs = 5
stale_secs = 10
max_entries = 1000

[proxy.retry]
enabled = true
max_retries = 3
base_delay_ms = 100
max_delay_ms = 2000

[proxy.circuit_breaker]
failure_threshold = 3
success_threshold = 2
open_duration_secs = 3
failure_window_secs = 10
"#
        );
        std::fs::write(&self.config_path, config).expect("Failed to write context-rooted config");
        eprintln!("  Wrote context-rooted mock config (type={upstream_type}, url={mock_url})");
    }

    /// Back up the current config (used before reload modifications).
    pub fn backup_current(&self) {
        let reload_backup = self.config_path.with_extension("toml.reload-backup");
        let _ = std::fs::copy(&self.config_path, &reload_backup);
    }

    /// Restore from reload backup.
    pub fn restore_reload_backup(&self) {
        let reload_backup = self.config_path.with_extension("toml.reload-backup");
        if reload_backup.exists() {
            let _ = std::fs::rename(&reload_backup, &self.config_path);
        }
    }

    /// Restore the original config from backup.
    pub fn restore(&mut self) {
        if self.backup_path.exists() {
            let _ = std::fs::rename(&self.backup_path, &self.config_path);
            self.backed_up = false;
            eprintln!("  Restored original config from backup");
        } else if !self.backed_up {
            // No backup means we created from scratch — remove it
            let _ = std::fs::remove_file(&self.config_path);
        }
    }
}

impl Drop for ConfigManager {
    fn drop(&mut self) {
        self.restore();
    }
}
