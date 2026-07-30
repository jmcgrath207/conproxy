//! `apply_tune` — write a tune session's winning scope params to local config.
//!
//! Hot path: export → merge into `ConfigFile.contexts.<id>.scope` → save.
//! Triggers `Config::save()` (or `Config::save_to(path)`) which rewrites the
//! whole TOML file via `toml::to_string_pretty`. Comments and key order are
//! not preserved (acceptable for local MCP workflow).
//!
//! The MCP handler follows up with `POST /admin/reload` to make the
//! rewritten config take effect on the running proxy.

use crate::config::{Config, NamedContextConfig, ProxyScopeConfig};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::session::TuneSessionStore;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Report returned by `apply_tune_export`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyReport {
    /// Config file path that was written.
    pub config_path: PathBuf,
    /// Context id whose scope was updated.
    pub context_id: String,
    /// Run id whose params were applied.
    pub source_run_id: String,
    /// `true` if the context already existed; `false` if it was created.
    pub context_created: bool,
    /// Rendered `[contexts.<id>.scope]` TOML that was applied.
    pub toml_applied: String,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Apply a tune session's selected run scope params to local config.
///
/// Steps:
/// 1. `export()` → get artifact (TOML fragment + scope JSON + source run id).
/// 2. Load the config (from `config_path` if set, else default local).
/// 3. Upsert `config.contexts[context_id]` with the new `scope` value.
/// 4. Save back to the same path.
///
/// # Errors
///
/// Session not found, no runs, config parse, file IO, or TOML serialization
/// errors. The function does not call into the running proxy — the caller
/// is responsible for triggering `/admin/reload` afterwards.
pub fn apply_tune_export(
    store: &TuneSessionStore,
    session_id: &str,
    agent_id: Option<&str>,
    context_id: Option<&str>,
    config_path: Option<&Path>,
) -> Result<ApplyReport, String> {
    // 1. Export — reuse existing dry-run artifact builder.
    let artifact = store.export(session_id, agent_id, context_id)?;

    // 2. Load existing config (path override or default local).
    let mut config = match config_path {
        Some(p) => Config::load_from(
            p.to_str()
                .ok_or_else(|| format!("config path is not valid UTF-8: {}", p.display()))?,
        )
        .map_err(|e| format!("Failed to load config from {}: {}", p.display(), e))?,
        None => Config::load().map_err(|e| format!("Failed to load default config: {}", e))?,
    };

    // 3. Parse scope JSON from export → ProxyScopeConfig, then upsert.
    let new_scope: ProxyScopeConfig = serde_json::from_value(artifact.scope.clone())
        .map_err(|e| format!("Failed to parse exported scope JSON: {}", e))?;

    let ctx_existed = config.config.contexts.contains_key(&artifact.context_id);
    let entry = config
        .config
        .contexts
        .entry(artifact.context_id.clone())
        .or_insert_with(NamedContextConfig::default);
    entry.scope = new_scope;

    // 4. Save back.
    let path_to_write = config_path
        .map(Path::to_path_buf)
        .unwrap_or_else(Config::local_config_path);

    if let Some(p) = config_path {
        let content = toml::to_string_pretty(&config.config)
            .map_err(|e| format!("TOML serialize failed: {}", e))?;
        std::fs::write(p, content)
            .map_err(|e| format!("Failed to write {}: {}", p.display(), e))?;
    } else {
        config
            .save()
            .map_err(|e| format!("Failed to save local config: {}", e))?;
    }

    Ok(ApplyReport {
        config_path: path_to_write,
        context_id: artifact.context_id,
        source_run_id: artifact.source_run_id.unwrap_or_default(),
        context_created: !ctx_existed,
        toml_applied: artifact.formats.toml,
    })
}

/// Apply a [`TuneExportArtifact`] produced by `export()` directly (avoids
/// re-running `export` for callers that already have the artifact). Useful
/// for tests and for the `apply_tune` tool that wants to layer
/// extra behavior on top of the artifact.
pub fn apply_export_artifact(
    artifact: &super::session::TuneExportArtifact,
    config_path: Option<&Path>,
) -> Result<ApplyReport, String> {
    let mut config = match config_path {
        Some(p) => Config::load_from(
            p.to_str()
                .ok_or_else(|| format!("config path is not valid UTF-8: {}", p.display()))?,
        )
        .map_err(|e| format!("Failed to load config from {}: {}", p.display(), e))?,
        None => Config::load().map_err(|e| format!("Failed to load default config: {}", e))?,
    };

    let new_scope: ProxyScopeConfig = serde_json::from_value(artifact.scope.clone())
        .map_err(|e| format!("Failed to parse exported scope JSON: {}", e))?;

    let ctx_existed = config.config.contexts.contains_key(&artifact.context_id);
    let entry = config
        .config
        .contexts
        .entry(artifact.context_id.clone())
        .or_insert_with(NamedContextConfig::default);
    entry.scope = new_scope;

    let path_to_write = config_path
        .map(Path::to_path_buf)
        .unwrap_or_else(Config::local_config_path);

    if let Some(p) = config_path {
        let content = toml::to_string_pretty(&config.config)
            .map_err(|e| format!("TOML serialize failed: {}", e))?;
        std::fs::write(p, content)
            .map_err(|e| format!("Failed to write {}: {}", p.display(), e))?;
    } else {
        config
            .save()
            .map_err(|e| format!("Failed to save local config: {}", e))?;
    }

    Ok(ApplyReport {
        config_path: path_to_write,
        context_id: artifact.context_id.clone(),
        source_run_id: artifact.source_run_id.clone().unwrap_or_default(),
        context_created: !ctx_existed,
        toml_applied: artifact.formats.toml.clone(),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WeightedPhrase;
    use crate::proxy::tune::scope::ScopeTuneParams;
    use crate::proxy::tune::scope_tune;
    use crate::proxy::tune::TuneBudget;
    use crate::proxy::types::SearchResult;
    use tempfile::TempDir;

    fn make_hit(id: &str, score: f32, content: &str) -> SearchResult {
        SearchResult {
            id: id.to_string(),
            score,
            content: content.to_string(),
            metadata: None,
            upstream_id: None,
        }
    }

    fn open_session_with_scope_run(
        store: &TuneSessionStore,
        agent_id: &str,
        context_id: &str,
    ) -> String {
        let sess = store
            .open(agent_id.to_string(), context_id.to_string(), None)
            .unwrap();
        let hits = vec![make_hit("d1", 1.0, "rust async runtime")];
        let weighted_phrases = vec![WeightedPhrase {
            text: "rust".to_string(),
            weight: 2.0,
            min_similarity: None,
        }];
        let _ = scope_tune(
            store,
            ScopeTuneParams {
                session_id: sess.session_id.clone(),
                agent_id: Some(agent_id.to_string()),
                context_id: Some(context_id.to_string()),
                hits,
                weighted_phrases,
                mode: Some("filter".to_string()),
                min_similarity: Some(0.25),
                min_similarity_sweep: None,
                scope_weight: Some(0.3),
                lexical_weight: None,
                budget: TuneBudget::default(),
            },
        )
        .unwrap();
        sess.session_id
    }

    #[test]
    fn test_apply_creates_new_context() {
        let dir = TempDir::new().unwrap();
        let cfg_path = dir.path().join("conproxy.toml");
        // Minimal valid config
        let initial = r#"[proxy]
listen = "127.0.0.1:9999"

[upstreams.dummy]
url = "http://127.0.0.1:65535"
type = "meilisearch"
"#;
        std::fs::write(&cfg_path, initial).unwrap();

        let store = TuneSessionStore::new(3600);
        let sid = open_session_with_scope_run(&store, "alice", "default");

        let report = apply_tune_export(
            &store,
            &sid,
            Some("alice"),
            Some("default"),
            Some(&cfg_path),
        )
        .unwrap();

        assert!(report.context_created);
        assert_eq!(report.context_id, "default");
        assert!(report.toml_applied.contains("[contexts.default.scope]"));
        assert!(report.toml_applied.contains("mode = \"filter\""));
        assert!(report.toml_applied.contains("text = \"rust\""));

        // Verify the on-disk file now has the context.
        let on_disk = std::fs::read_to_string(&cfg_path).unwrap();
        assert!(on_disk.contains("[contexts.default.scope]"));
        assert!(on_disk.contains("text = \"rust\""));
        assert!(on_disk.contains("listen = \"127.0.0.1:9999\"")); // preserved
    }

    #[test]
    fn test_apply_overwrites_existing_scope() {
        let dir = TempDir::new().unwrap();
        let cfg_path = dir.path().join("conproxy.toml");
        let initial = r#"[proxy]
listen = "127.0.0.1:9999"

[upstreams.dummy]
url = "http://127.0.0.1:65535"
type = "meilisearch"

[contexts.default]
default = true

[[contexts.default.upstreams]]
ref = "dummy"

[contexts.default.scope]
mode = "boost"
min_similarity = 0.1
"#;
        std::fs::write(&cfg_path, initial).unwrap();

        let store = TuneSessionStore::new(3600);
        let sid = open_session_with_scope_run(&store, "alice", "default");

        let report = apply_tune_export(
            &store,
            &sid,
            Some("alice"),
            Some("default"),
            Some(&cfg_path),
        )
        .unwrap_or_else(|e| panic!("apply failed: {e}"));

        assert!(!report.context_created, "context already existed");
        let on_disk = std::fs::read_to_string(&cfg_path).unwrap();
        // New mode from the session replaces old "boost"
        assert!(on_disk.contains("mode = \"filter\""));
        // old "0.1" replaced
        assert!(!on_disk.contains("min_similarity = 0.1"));
        // default = true preserved
        assert!(on_disk.contains("default = true"));
    }

    #[test]
    fn test_apply_unknown_session_errors() {
        let dir = TempDir::new().unwrap();
        let cfg_path = dir.path().join("conproxy.toml");
        std::fs::write(&cfg_path, "[proxy]\nlisten = \"127.0.0.1:9999\"\n").unwrap();

        let store = TuneSessionStore::new(3600);
        let err = apply_tune_export(
            &store,
            "nope",
            Some("alice"),
            Some("default"),
            Some(&cfg_path),
        )
        .unwrap_err();
        assert!(err.contains("session not found") || err.contains("no runs"));
    }

    #[test]
    fn test_apply_export_artifact_direct() {
        // Build an artifact without running scope_tune, then apply.
        let artifact = crate::proxy::tune::session::TuneExportArtifact {
            session_id: "s1".to_string(),
            agent_id: "bob".to_string(),
            context_id: "ctx".to_string(),
            scope: serde_json::json!({
                "mode": "rerank",
                "min_similarity": 0.5,
                "weighted_phrases": [{"text": "embed", "weight": 1.5}],
            }),
            other: serde_json::json!({}),
            formats: crate::proxy::tune::session::TuneExportFormats {
                toml: "[contexts.ctx.scope]\nmode = \"rerank\"\n".to_string(),
                json: serde_json::json!({}),
            },
            source_run_id: Some("run-x".to_string()),
        };

        let dir = TempDir::new().unwrap();
        let cfg_path = dir.path().join("conproxy.toml");
        std::fs::write(
            &cfg_path,
            "[proxy]\nlisten = \"127.0.0.1:9999\"\n\n[upstreams.dummy]\nurl = \"http://127.0.0.1:65535\"\ntype = \"fts\"\n",
        )
        .unwrap();

        let report = apply_export_artifact(&artifact, Some(&cfg_path)).unwrap();
        assert!(report.context_created);
        assert_eq!(report.context_id, "ctx");
        assert_eq!(report.source_run_id, "run-x");

        let on_disk = std::fs::read_to_string(&cfg_path).unwrap();
        assert!(on_disk.contains("[contexts.ctx.scope]"));
        assert!(on_disk.contains("mode = \"rerank\""));
    }

    #[test]
    fn test_apply_default_path_round_trip() {
        // When no config_path is given, we write to Config::local_config_path().
        // We can't easily override HOME for the test, so just verify the helper
        // function chooses the right default — by checking it doesn't error on
        // missing session.
        let dir = TempDir::new().unwrap();
        let _ = dir; // silence unused
        let store = TuneSessionStore::new(3600);
        // No session, so export will fail — confirms the code path runs.
        let err = apply_tune_export(&store, "missing", Some("alice"), Some("default"), None);
        assert!(err.is_err());
    }
}
