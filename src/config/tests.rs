#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;
use serial_test::serial;

#[test]
fn test_config_merge_local_overrides_global() {
    let global = ConfigFile {
        search: SearchConfig {
            use_hybrid: Some(false),
            ..Default::default()
        },
        ..Default::default()
    };

    let local = ConfigFile {
        search: SearchConfig {
            use_hybrid: Some(true),
            ..Default::default()
        },
        ..Default::default()
    };

    let merged = global.merge_with(&local);

    // Local overrides global
    assert!(merged.search.use_hybrid());
}

#[test]
fn test_config_registries_merged() {
    let global = ConfigFile {
        registries: HashMap::from([(
            "company".to_string(),
            "https://github.com/company/registry".to_string(),
        )]),
        ..Default::default()
    };

    let local = ConfigFile {
        registries: HashMap::from([(
            "project".to_string(),
            "https://github.com/project/registry".to_string(),
        )]),
        ..Default::default()
    };

    let merged = global.merge_with(&local);

    // Both registries should be present
    assert_eq!(merged.registries.len(), 2);
    assert!(merged.registries.contains_key("company"));
    assert!(merged.registries.contains_key("project"));
}

#[test]
fn test_config_defaults_applied() {
    let config = ConfigFile::default();

    // Check defaults via accessor methods
    assert!(!config.search.use_hybrid());
    assert_eq!(config.embedding.batch_size(), 32);
    assert!(!config.web.auto_index());
    assert_eq!(config.web.content_dir(), "web");
}

#[test]
fn test_config_file_default_local() {
    let config = ConfigFile::default_local();
    assert!(config.packages.is_empty());
    assert!(config.registries.is_empty());
}

#[test]
fn test_config_file_default_global() {
    let config = ConfigFile::default_global();
    assert!(config.packages.is_empty());
    assert!(config.registries.is_empty());
}

#[test]
fn test_static_paths() {
    // Test static path methods
    let local_dir = Config::local_dir();
    assert_eq!(local_dir, PathBuf::from(".conproxy"));

    let local_config = Config::local_config_path();
    assert_eq!(local_config, PathBuf::from(".conproxy/conproxy.toml"));

    // Global paths should contain .conproxy
    let global_dir = Config::global_dir();
    assert!(global_dir.to_string_lossy().contains(".conproxy"));

    let global_config = Config::global_config_path();
    assert!(global_config.to_string_lossy().contains("conproxy.toml"));

    let models_dir = Config::global_models_dir();
    assert!(models_dir.to_string_lossy().contains("models"));
}

#[test]
fn test_config_directory_accessors() {
    let config = Config {
        config: ConfigFile::default(),
        local_root: Some(PathBuf::from("/test/.conproxy")),
    };

    assert_eq!(config.conproxy_dir(), PathBuf::from("/test/.conproxy"));
    assert_eq!(config.index_dir(), PathBuf::from("/test/.conproxy/index"));
    assert_eq!(config.cache_dir(), PathBuf::from("/test/.conproxy/cache"));
    assert_eq!(
        config.packages_dir(),
        PathBuf::from("/test/.conproxy/packages")
    );
    assert_eq!(config.web_dir(), PathBuf::from("/test/.conproxy/web"));
}

#[test]
fn test_config_directory_accessors_no_local_root() {
    let config = Config {
        config: ConfigFile::default(),
        local_root: None,
    };

    // Should fall back to default .conproxy
    assert_eq!(config.conproxy_dir(), PathBuf::from(".conproxy"));
    assert_eq!(config.index_dir(), PathBuf::from(".conproxy/index"));
}

#[test]
fn test_embedding_config_defaults() {
    let config = EmbeddingConfig::default();

    // Test that paths are generated correctly
    let model_path = config.model_path();
    assert!(model_path.to_string_lossy().contains("model.onnx"));

    let tokenizer_path = config.tokenizer_path();
    assert!(tokenizer_path.to_string_lossy().contains("tokenizer.json"));

    assert_eq!(config.batch_size(), 32);
}

#[test]
fn test_embedding_config_custom() {
    let config = EmbeddingConfig {
        model_path: Some(PathBuf::from("/custom/model.onnx")),
        tokenizer_path: Some(PathBuf::from("/custom/tokenizer.json")),
        batch_size: Some(64),
        ..Default::default()
    };

    assert_eq!(config.model_path(), PathBuf::from("/custom/model.onnx"));
    assert_eq!(
        config.tokenizer_path(),
        PathBuf::from("/custom/tokenizer.json")
    );
    assert_eq!(config.batch_size(), 64);
}

#[test]
fn test_web_config_merge() {
    let base = WebConfig {
        auto_index: Some(true),
        content_dir: Some("base_web".to_string()),
    };

    let overlay = WebConfig {
        auto_index: None,
        content_dir: Some("overlay_web".to_string()),
    };

    let merged = overlay.merge_with(&base);

    // overlay.auto_index is None, so base value should be used
    assert!(merged.auto_index());
    // overlay has content_dir, so it overrides
    assert_eq!(merged.content_dir(), "overlay_web");
}

#[test]
fn test_context_config_defaults() {
    let config = ContextConfig::default();

    // Default paths returns ["packages/**/*.md"] when None
    assert_eq!(config.paths(), vec!["packages/**/*.md".to_string()]);

    // Default warm_interval should be 300 seconds (5 minutes)
    assert_eq!(config.warm_interval(), 300);

    // Default warm_limit should be 1000
    assert_eq!(config.warm_limit(), 1000);
}

#[test]
fn test_context_config_custom_values() {
    let config = ContextConfig {
        paths: Some(vec![
            "packages/**/*.md".to_string(),
            "docs/**/*.md".to_string(),
        ]),
        warm_interval: Some(600),
        warm_limit: Some(500),
    };

    assert_eq!(config.paths().len(), 2);
    assert!(config.paths().contains(&"packages/**/*.md".to_string()));
    assert_eq!(config.warm_interval(), 600);
    assert_eq!(config.warm_limit(), 500);
}

#[test]
fn test_context_config_merge() {
    let base = ContextConfig {
        paths: Some(vec!["base/**/*.md".to_string()]),
        warm_interval: Some(300),
        warm_limit: Some(1000),
    };

    let overlay = ContextConfig {
        paths: Some(vec!["overlay/**/*.md".to_string()]),
        warm_interval: None,
        warm_limit: Some(500),
    };

    let merged = overlay.merge_with(&base);

    // overlay.paths overrides base
    assert_eq!(merged.paths().len(), 1);
    assert!(merged.paths().contains(&"overlay/**/*.md".to_string()));

    // overlay.warm_interval is None, so base value is used
    assert_eq!(merged.warm_interval(), 300);

    // overlay.warm_limit overrides base
    assert_eq!(merged.warm_limit(), 500);
}

#[test]
fn test_context_config_merge_empty_overlay() {
    let base = ContextConfig {
        paths: Some(vec!["base/**/*.md".to_string()]),
        warm_interval: Some(600),
        warm_limit: Some(2000),
    };

    let overlay = ContextConfig::default();

    let merged = overlay.merge_with(&base);

    // All base values should be preserved
    assert_eq!(merged.paths().len(), 1);
    assert!(merged.paths().contains(&"base/**/*.md".to_string()));
    assert_eq!(merged.warm_interval(), 600);
    assert_eq!(merged.warm_limit(), 2000);
}

#[test]
fn test_search_config_merge() {
    let base = SearchConfig {
        use_hybrid: Some(true),
        ..Default::default()
    };

    let overlay = SearchConfig {
        use_hybrid: None,
        ..Default::default()
    };

    let merged = overlay.merge_with(&base);
    assert!(merged.use_hybrid());
}

#[test]
fn test_embedding_config_merge() {
    let base = EmbeddingConfig {
        model_path: Some(PathBuf::from("/base/model.onnx")),
        tokenizer_path: Some(PathBuf::from("/base/tokenizer.json")),
        batch_size: Some(16),
        ..Default::default()
    };

    let overlay = EmbeddingConfig {
        model_path: None,
        tokenizer_path: Some(PathBuf::from("/overlay/tokenizer.json")),
        batch_size: None,
        ..Default::default()
    };

    let merged = overlay.merge_with(&base);

    assert_eq!(merged.model_path(), PathBuf::from("/base/model.onnx"));
    assert_eq!(
        merged.tokenizer_path(),
        PathBuf::from("/overlay/tokenizer.json")
    );
    assert_eq!(merged.batch_size(), 16);
}

#[test]
fn test_package_entry_serialization() {
    let entry = PackageEntry {
        git: "https://github.com/user/repo".to_string(),
        tag: Some("v1.0.0".to_string()),
    };

    // Test that it can be serialized to TOML
    let toml_str = toml::to_string(&entry).unwrap();
    assert!(toml_str.contains("git = "));
    assert!(toml_str.contains("tag = "));

    // Test deserialization
    let parsed: PackageEntry = toml::from_str(&toml_str).unwrap();
    assert_eq!(parsed.git, entry.git);
    assert_eq!(parsed.tag, entry.tag);
}

#[test]
fn test_config_file_serialization() {
    let mut config = ConfigFile::default();
    config.packages.insert(
        "test-pkg".to_string(),
        PackageEntry {
            git: "https://github.com/test/pkg".to_string(),
            tag: None,
        },
    );
    config
        .registries
        .insert("myregistry".to_string(), "https://example.com".to_string());

    // Test round-trip serialization
    let toml_str = toml::to_string_pretty(&config).unwrap();
    let parsed: ConfigFile = toml::from_str(&toml_str).unwrap();

    assert!(parsed.packages.contains_key("test-pkg"));
    assert!(parsed.registries.contains_key("myregistry"));
}

#[test]
fn test_config_load_from_file() {
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let config_path = dir.path().join("conproxy.toml");

    // Write a test config
    let config_content = r#"
[packages.test-pkg]
git = "https://github.com/test/pkg"
tag = "v1.0.0"

[registries]
myregistry = "https://example.com"

[search]
use_hybrid = true
"#;
    std::fs::write(&config_path, config_content).unwrap();

    // Load and verify
    let config = Config::load_from(config_path.to_str().unwrap()).unwrap();

    assert!(config.config.packages.contains_key("test-pkg"));
    assert!(config.config.registries.contains_key("myregistry"));
    assert!(config.config.search.use_hybrid());
}

#[test]
fn test_config_save_and_load() {
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let conproxy_dir = dir.path().join(".conproxy");
    std::fs::create_dir_all(&conproxy_dir).unwrap();

    // Create config with some data
    let mut config_file = ConfigFile::default();
    config_file.packages.insert(
        "my-pkg".to_string(),
        PackageEntry {
            git: "https://github.com/my/pkg".to_string(),
            tag: Some("v2.0.0".to_string()),
        },
    );

    let config = Config {
        config: config_file,
        local_root: Some(conproxy_dir.clone()),
    };

    // Save the config
    let config_path = conproxy_dir.join("conproxy.toml");
    let content = toml::to_string_pretty(&config.config).unwrap();
    std::fs::write(&config_path, content).unwrap();

    // Load it back
    let loaded = Config::load_from(config_path.to_str().unwrap()).unwrap();

    assert!(loaded.config.packages.contains_key("my-pkg"));
    let pkg = &loaded.config.packages["my-pkg"];
    assert_eq!(pkg.git, "https://github.com/my/pkg");
    assert_eq!(pkg.tag, Some("v2.0.0".to_string()));
}

#[test]
fn test_models_dir() {
    let config = Config {
        config: ConfigFile::default(),
        local_root: None,
    };

    // models_dir should point to global models directory
    let models = config.models_dir();
    assert!(models.to_string_lossy().contains("models"));
}

#[test]
fn test_config_packages_empty_uses_self() {
    let global = ConfigFile {
        packages: HashMap::from([(
            "global-pkg".to_string(),
            PackageEntry {
                git: "https://github.com/global/pkg".to_string(),
                tag: None,
            },
        )]),
        ..Default::default()
    };

    let local = ConfigFile {
        packages: HashMap::new(), // Empty
        ..Default::default()
    };

    let merged = global.merge_with(&local);

    // Since local packages is empty, should use global
    assert!(merged.packages.contains_key("global-pkg"));
}

#[test]
fn test_config_packages_local_overrides() {
    let global = ConfigFile {
        packages: HashMap::from([(
            "global-pkg".to_string(),
            PackageEntry {
                git: "https://github.com/global/pkg".to_string(),
                tag: None,
            },
        )]),
        ..Default::default()
    };

    let local = ConfigFile {
        packages: HashMap::from([(
            "local-pkg".to_string(),
            PackageEntry {
                git: "https://github.com/local/pkg".to_string(),
                tag: None,
            },
        )]),
        ..Default::default()
    };

    let merged = global.merge_with(&local);

    // Since local packages is not empty, should use local only
    assert!(!merged.packages.contains_key("global-pkg"));
    assert!(merged.packages.contains_key("local-pkg"));
}

#[test]
fn test_load_local_not_exists() {
    // This tests that load_local returns None when no .conproxy/conproxy.toml exists
    // We can't easily test this without changing cwd, but we can verify the method exists
    // The actual behavior is tested in UAT tests
    let result = Config::load_local();
    // Result should be Ok (either Some or None)
    assert!(result.is_ok());
}

#[test]
fn test_load_global_not_exists_or_exists() {
    // Similar to above - this is a smoke test
    let result = Config::load_global();
    assert!(result.is_ok());
}

#[test]
fn test_web_config_defaults() {
    let config = WebConfig::default();
    assert!(!config.auto_index());
    assert_eq!(config.content_dir(), "web");
}

#[test]
fn test_web_config_custom() {
    let config = WebConfig {
        auto_index: Some(true),
        content_dir: Some("custom_web".to_string()),
    };
    assert!(config.auto_index());
    assert_eq!(config.content_dir(), "custom_web");
}

#[test]
fn test_search_config_defaults() {
    let config = SearchConfig::default();
    assert!(!config.use_hybrid());
}

#[test]
fn test_search_config_custom() {
    let config = SearchConfig {
        use_hybrid: Some(true),
        ..Default::default()
    };
    assert!(config.use_hybrid());
}

// Note: Config::save now calls ensure_local_dirs() on first write, so the
// `.conproxy/` directory structure is created automatically. There is no
// `init_local` / `init_global` step anymore. `Config::load` falls back to a
// default in-memory config when no file exists.

#[test]
fn test_default_local_config_toml_serialization() {
    // Test that default_local config can be serialized to TOML
    let config = ConfigFile::default_local();
    let toml_str = toml::to_string_pretty(&config);
    assert!(toml_str.is_ok());

    // Verify it can be parsed back
    let parsed: std::result::Result<ConfigFile, _> = toml::from_str(&toml_str.unwrap());
    assert!(parsed.is_ok());
}

#[test]
fn test_default_global_config_toml_serialization() {
    // Test that default_global config can be serialized to TOML
    let config = ConfigFile::default_global();
    let toml_str = toml::to_string_pretty(&config);
    assert!(toml_str.is_ok());

    // Verify it can be parsed back
    let parsed: std::result::Result<ConfigFile, _> = toml::from_str(&toml_str.unwrap());
    assert!(parsed.is_ok());
}

// Note: Tests that change cwd (save, find_local_root, load) cause race
// conditions in parallel test execution. The actual functionality is
// tested in UAT tests which run with --test-threads=1.
//
// These unit tests verify the logic paths without relying on cwd:

#[test]
fn test_web_config_merge_both_none() {
    let base = WebConfig::default();
    let overlay = WebConfig::default();

    let merged = overlay.merge_with(&base);

    // Both None, should use defaults
    assert!(!merged.auto_index());
    assert_eq!(merged.content_dir(), "web");
}

// =========================================================================
// Serial tests that change the current working directory
// These tests must be run serially to avoid race conditions
// =========================================================================

#[test]
#[serial]
fn test_ensure_local_dirs_creates_structure() {
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let original_dir = std::env::current_dir().unwrap();

    std::env::set_current_dir(dir.path()).unwrap();

    let result = Config::ensure_local_dirs();
    assert!(
        result.is_ok(),
        "ensure_local_dirs should succeed: {:?}",
        result
    );

    assert!(dir.path().join(".conproxy").exists());
    assert!(dir.path().join(".conproxy/packages").exists());
    assert!(dir.path().join(".conproxy/index").exists());
    assert!(dir.path().join(".conproxy/cache").exists());
    assert!(dir.path().join(".conproxy/web").exists());
    assert!(dir.path().join(".conproxy/.gitignore").exists());

    let gitignore = std::fs::read_to_string(dir.path().join(".conproxy/.gitignore")).unwrap();
    assert!(gitignore.contains("cache/"));
    assert!(gitignore.contains("*.pid"));

    // Idempotent: calling again must succeed and not overwrite the gitignore.
    let original_gitignore = gitignore.clone();
    Config::ensure_local_dirs().unwrap();
    let gitignore_after = std::fs::read_to_string(dir.path().join(".conproxy/.gitignore")).unwrap();
    assert_eq!(gitignore_after, original_gitignore);

    std::env::set_current_dir(original_dir).unwrap();
}

#[test]
#[serial]
fn test_save_local_config() {
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let original_dir = std::env::current_dir().unwrap();

    // Change to temp directory
    std::env::set_current_dir(dir.path()).unwrap();

    // Create .conproxy directory
    let conproxy_dir = dir.path().join(".conproxy");
    std::fs::create_dir_all(&conproxy_dir).unwrap();

    // Create a config and save it
    let mut config_file = ConfigFile::default();
    config_file.packages.insert(
        "test-pkg".to_string(),
        PackageEntry {
            git: "https://github.com/test/pkg".to_string(),
            tag: Some("v1.0.0".to_string()),
        },
    );

    let config = Config {
        config: config_file,
        local_root: Some(conproxy_dir.clone()),
    };

    // Save should succeed
    let result = config.save();
    assert!(result.is_ok());

    // Verify file was written
    let saved_path = dir.path().join(".conproxy/conproxy.toml");
    assert!(saved_path.exists());

    // Verify content
    let content = std::fs::read_to_string(&saved_path).unwrap();
    assert!(content.contains("test-pkg"));
    assert!(content.contains("v1.0.0"));

    // Restore original directory
    std::env::set_current_dir(original_dir).unwrap();
}

#[test]
#[serial]
fn test_find_local_root_exists() {
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let original_dir = std::env::current_dir().unwrap();

    // Create .conproxy directory
    std::fs::create_dir(dir.path().join(".conproxy")).unwrap();

    // Change to temp directory
    std::env::set_current_dir(dir.path()).unwrap();

    // find_local_root should succeed
    let result = Config::find_local_root();
    assert!(result.is_ok());

    // Restore original directory
    std::env::set_current_dir(original_dir).unwrap();
}

#[test]
#[serial]
fn test_find_local_root_not_exists() {
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let original_dir = std::env::current_dir().unwrap();

    // Change to temp directory (no .conproxy exists)
    std::env::set_current_dir(dir.path()).unwrap();

    // find_local_root should fail
    let result = Config::find_local_root();
    assert!(result.is_err());

    // Restore original directory
    std::env::set_current_dir(original_dir).unwrap();
}

#[test]
#[serial]
fn test_load_with_local_only() {
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let original_dir = std::env::current_dir().unwrap();

    // Create local config
    let conproxy_dir = dir.path().join(".conproxy");
    std::fs::create_dir_all(&conproxy_dir).unwrap();

    let config_content = r#"
[packages.local-pkg]
git = "https://github.com/local/pkg"

[search]
use_hybrid = true
"#;
    std::fs::write(conproxy_dir.join("conproxy.toml"), config_content).unwrap();

    // Change to temp directory
    std::env::set_current_dir(dir.path()).unwrap();

    // Load should succeed
    let result = Config::load();
    assert!(result.is_ok());

    let config = result.unwrap();
    assert!(config.config.packages.contains_key("local-pkg"));
    assert!(config.config.search.use_hybrid());

    // Restore original directory
    std::env::set_current_dir(original_dir).unwrap();
}

#[test]
#[serial]
fn test_load_local_exists() {
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let original_dir = std::env::current_dir().unwrap();

    // Create local config file
    let conproxy_dir = dir.path().join(".conproxy");
    std::fs::create_dir_all(&conproxy_dir).unwrap();

    let config_content = r#"
[packages]

[registries]

[search]
use_hybrid = false
"#;
    std::fs::write(conproxy_dir.join("conproxy.toml"), config_content).unwrap();

    // Change to temp directory
    std::env::set_current_dir(dir.path()).unwrap();

    // load_local should return Some
    let result = Config::load_local();
    assert!(result.is_ok());
    assert!(result.unwrap().is_some());

    // Restore original directory
    std::env::set_current_dir(original_dir).unwrap();
}

#[test]
#[serial]
fn test_load_merges_global_and_local() {
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let original_dir = std::env::current_dir().unwrap();

    // Create local config
    let conproxy_dir = dir.path().join(".conproxy");
    std::fs::create_dir_all(&conproxy_dir).unwrap();

    let config_content = r#"
[packages.my-pkg]
git = "https://github.com/my/pkg"

[search]
use_hybrid = true
"#;
    std::fs::write(conproxy_dir.join("conproxy.toml"), config_content).unwrap();

    // Change to temp directory
    std::env::set_current_dir(dir.path()).unwrap();

    // Load should merge global (if exists) and local
    let result = Config::load();
    assert!(result.is_ok());

    let config = result.unwrap();
    // Verify local config is loaded
    assert!(config.config.packages.contains_key("my-pkg"));
    assert!(config.config.search.use_hybrid());
    // Verify local_root is set
    assert!(config.local_root.is_some());

    // Restore original directory
    std::env::set_current_dir(original_dir).unwrap();
}

#[test]
#[serial]
fn test_load_returns_default_when_no_config() {
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let original_dir = std::env::current_dir().unwrap();

    // Change to empty temp directory (no .conproxy, no global config)
    std::env::set_current_dir(dir.path()).unwrap();

    // `Config::load` no longer returns `NotInitialized` — when neither a
    // global nor a local config exists, it falls back to a default
    // in-memory local config so the proxy can run on first use.
    let result = Config::load();
    if !Config::global_config_path().exists() {
        assert!(
            result.is_ok(),
            "load should return a default config when none exists: {result:?}"
        );
        let config = result.unwrap();
        // No local_root because no local config was discovered
        assert!(config.local_root.is_none());
    }

    // Restore original directory
    std::env::set_current_dir(original_dir).unwrap();
}

#[test]
fn test_load_global_returns_some_if_exists() {
    // Test that load_global returns the correct result based on whether
    // the global config exists
    let result = Config::load_global();
    assert!(result.is_ok());

    let global_path = Config::global_config_path();
    if global_path.exists() {
        assert!(result.unwrap().is_some());
    } else {
        assert!(result.unwrap().is_none());
    }
}

#[test]
fn test_config_file_merge_both_have_data() {
    // Test merging when both global and local have data
    let global = ConfigFile {
        packages: HashMap::from([(
            "global-pkg".to_string(),
            PackageEntry {
                git: "https://github.com/global/pkg".to_string(),
                tag: None,
            },
        )]),
        registries: HashMap::from([("global-reg".to_string(), "https://global.com".to_string())]),
        search: SearchConfig {
            use_hybrid: Some(false),
            ..Default::default()
        },
        embedding: EmbeddingConfig::default(),
        web: WebConfig {
            auto_index: Some(true),
            ..Default::default()
        },
        proxy: ProxyConfig::default(),
        context: ContextConfig::default(),
        ..Default::default()
    };

    let local = ConfigFile {
        packages: HashMap::from([(
            "local-pkg".to_string(),
            PackageEntry {
                git: "https://github.com/local/pkg".to_string(),
                tag: Some("v1.0.0".to_string()),
            },
        )]),
        registries: HashMap::from([("local-reg".to_string(), "https://local.com".to_string())]),
        search: SearchConfig {
            use_hybrid: Some(true), // Override global
            ..Default::default()
        },
        embedding: EmbeddingConfig::default(),
        web: WebConfig::default(), // Don't override
        proxy: ProxyConfig::default(),
        context: ContextConfig::default(),
        ..Default::default()
    };

    let merged = global.merge_with(&local);

    // Local packages replace global packages (since local is not empty)
    assert!(!merged.packages.contains_key("global-pkg"));
    assert!(merged.packages.contains_key("local-pkg"));

    // Registries are merged
    assert!(merged.registries.contains_key("global-reg"));
    assert!(merged.registries.contains_key("local-reg"));

    // Search: local overrides
    assert!(merged.search.use_hybrid());

    // Web: local has None, so global value is used
    assert!(merged.web.auto_index());
}

#[test]
fn test_web_merge_with_called_directly() {
    // Direct test of merge_with to cover the impl line
    let base = WebConfig {
        auto_index: Some(true),
        content_dir: Some("base_dir".to_string()),
    };
    let overlay = WebConfig {
        auto_index: Some(false),
        content_dir: None,
    };

    let merged = overlay.merge_with(&base);
    assert!(!merged.auto_index());
    assert_eq!(merged.content_dir(), "base_dir");
}

#[test]
fn test_config_struct_fields() {
    // Test Config struct creation and access
    let config = Config {
        config: ConfigFile::default(),
        local_root: Some(PathBuf::from("/test/path")),
    };

    assert!(config.config.packages.is_empty());
    assert_eq!(config.local_root, Some(PathBuf::from("/test/path")));
}

#[test]
fn test_config_all_dir_accessors() {
    // Test all directory accessors
    let config = Config {
        config: ConfigFile::default(),
        local_root: Some(PathBuf::from("/project/.conproxy")),
    };

    // Test each accessor returns expected paths
    assert!(config
        .conproxy_dir()
        .to_string_lossy()
        .contains(".conproxy"));
    assert!(config.index_dir().to_string_lossy().contains("index"));
    assert!(config.cache_dir().to_string_lossy().contains("cache"));
    assert!(config.packages_dir().to_string_lossy().contains("packages"));
    assert!(config.web_dir().to_string_lossy().contains("web"));
    assert!(config.models_dir().to_string_lossy().contains("models"));
}

#[test]
#[serial]
fn test_load_global_with_temp_home() {
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let original_home = std::env::var("HOME").ok();
    let original_dir = std::env::current_dir().unwrap();

    // Create global config in temp "home"
    let global_dir = dir.path().join(".conproxy");
    std::fs::create_dir_all(&global_dir).unwrap();

    let global_content = r#"
[packages]

[registries]
global-reg = "https://global.example.com"

[search]
use_hybrid = true
"#;
    std::fs::write(global_dir.join("conproxy.toml"), global_content).unwrap();

    // Set HOME to temp directory
    std::env::set_var("HOME", dir.path());

    // Verify global config can be loaded
    let result = Config::load_global();
    assert!(result.is_ok());
    let config = result.unwrap();
    assert!(config.is_some());

    let cfg = config.unwrap();
    assert!(cfg.registries.contains_key("global-reg"));
    assert!(cfg.search.use_hybrid());

    // Restore HOME
    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    }
    std::env::set_current_dir(original_dir).unwrap();
}

#[test]
#[serial]
fn test_load_with_global_only() {
    use tempfile::TempDir;

    let home_dir = TempDir::new().unwrap();
    let work_dir = TempDir::new().unwrap();
    let original_home = std::env::var("HOME").ok();
    let original_dir = std::env::current_dir().unwrap();

    // Create global config
    let global_dir = home_dir.path().join(".conproxy");
    std::fs::create_dir_all(&global_dir).unwrap();

    let global_content = r#"
[packages]

[registries]
test-reg = "https://test.example.com"
"#;
    std::fs::write(global_dir.join("conproxy.toml"), global_content).unwrap();

    // Set HOME to temp directory
    std::env::set_var("HOME", home_dir.path());

    // Change to work directory (no local .conproxy)
    std::env::set_current_dir(work_dir.path()).unwrap();

    // Load should succeed with global only
    let result = Config::load();
    assert!(result.is_ok());

    let config = result.unwrap();
    // Global config should be loaded
    assert!(config.config.registries.contains_key("test-reg"));
    // No local_root since no local config
    assert!(config.local_root.is_none());

    // Restore HOME and cwd
    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    }
    std::env::set_current_dir(original_dir).unwrap();
}

#[test]
#[serial]
fn test_load_merges_global_and_local_configs() {
    use tempfile::TempDir;

    let home_dir = TempDir::new().unwrap();
    let work_dir = TempDir::new().unwrap();
    let original_home = std::env::var("HOME").ok();
    let original_dir = std::env::current_dir().unwrap();

    // Create global config
    let global_dir = home_dir.path().join(".conproxy");
    std::fs::create_dir_all(&global_dir).unwrap();

    let global_content = r#"
[packages]

[registries]
global-reg = "https://global.com"

[search]
use_hybrid = false
"#;
    std::fs::write(global_dir.join("conproxy.toml"), global_content).unwrap();

    // Create local config
    let local_dir = work_dir.path().join(".conproxy");
    std::fs::create_dir_all(&local_dir).unwrap();

    let local_content = r#"
[packages.local-pkg]
git = "https://github.com/local/pkg"

[registries]
local-reg = "https://local.com"

[search]
use_hybrid = true
"#;
    std::fs::write(local_dir.join("conproxy.toml"), local_content).unwrap();

    // Set HOME and change to work directory
    std::env::set_var("HOME", home_dir.path());
    std::env::set_current_dir(work_dir.path()).unwrap();

    // Load should merge both configs
    let result = Config::load();
    assert!(result.is_ok());

    let config = result.unwrap();

    // Local packages should be used (not global)
    assert!(config.config.packages.contains_key("local-pkg"));

    // Registries should be merged
    assert!(config.config.registries.contains_key("global-reg"));
    assert!(config.config.registries.contains_key("local-reg"));

    // Local overrides global for search
    assert!(config.config.search.use_hybrid());

    // local_root should be set
    assert!(config.local_root.is_some());

    // Restore HOME and cwd
    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    }
    std::env::set_current_dir(original_dir).unwrap();
}

#[test]
fn test_upstream_config_new_fields_parsing() {
    let toml_str = r#"
id = "my-qdrant"
url = "http://localhost:6333"
upstream_type = "qdrant"
query_mode = "vector_only"
"#;
    let config: UpstreamEndpointConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.upstream_type(), Some("qdrant"));
    assert_eq!(config.query_mode(), Some("vector_only"));
    assert!(!config.is_pgvector());
    assert!(config.validate().is_ok());
}

#[test]
fn test_upstream_validate_pgvector() {
    // pgvector without table should fail
    let toml_str = r#"
id = "pg"
url = "postgresql://localhost/mydb"
upstream_type = "pgvector"
"#;
    let config: UpstreamEndpointConfig = toml::from_str(toml_str).unwrap();
    assert!(config.is_pgvector());
    let err = config.validate().unwrap_err();
    assert!(err.contains("table"));

    // pgvector with table should pass
    let toml_str = r#"
id = "pg"
url = "postgresql://localhost/mydb"
upstream_type = "pgvector"
table = "documents"
embedding_column = "embedding"
content_column = "content"
distance_metric = "cosine"
dimensions = 384
metadata_columns = ["title", "source"]
"#;
    let config: UpstreamEndpointConfig = toml::from_str(toml_str).unwrap();
    assert!(config.validate().is_ok());
    assert_eq!(config.table, Some("documents".to_string()));
    assert_eq!(config.dimensions, Some(384));
    assert_eq!(config.metadata_columns.len(), 2);
    assert_eq!(config.distance_metric(), "cosine");
}

#[test]
fn test_upstream_validate_invalid_type() {
    let toml_str = r#"
id = "bad"
url = "http://localhost"
upstream_type = "mongodb"
"#;
    let config: UpstreamEndpointConfig = toml::from_str(toml_str).unwrap();
    let err = config.validate().unwrap_err();
    assert!(err.contains("invalid upstream_type"));
}

#[test]
fn test_upstream_validate_solr_rejected() {
    let toml_str = r#"
id = "solr-gone"
url = "http://localhost:8983"
upstream_type = "solr"
"#;
    let config: UpstreamEndpointConfig = toml::from_str(toml_str).unwrap();
    let err = config.validate().unwrap_err();
    assert!(err.contains("invalid upstream_type"), "got: {err}");
    assert!(err.contains("solr"), "got: {err}");
}

#[test]
fn test_upstream_validate_invalid_query_mode() {
    let toml_str = r#"
id = "bad"
url = "http://localhost"
query_mode = "hybrid_magic"
"#;
    let config: UpstreamEndpointConfig = toml::from_str(toml_str).unwrap();
    let err = config.validate().unwrap_err();
    assert!(err.contains("invalid query_mode"));
}

#[test]
fn test_upstream_es_fields_parsing() {
    let toml_str = r#"
id = "my-es"
url = "http://localhost:9200"
upstream_type = "elasticsearch"
query_mode = "text_native"
index = "documents"
search_fields = ["title", "content", "tags"]
return_fields = ["title", "content"]
"#;
    let config: UpstreamEndpointConfig = toml::from_str(toml_str).unwrap();
    assert!(config.validate().is_ok());
    assert_eq!(config.index, Some("documents".to_string()));
    assert_eq!(config.search_fields.len(), 3);
    assert_eq!(config.return_fields.len(), 2);
}

#[test]
fn test_normalize_upstreams_converts() {
    let mut config = ProxyConfig {
        upstream_url: Some("http://localhost:9200".to_string()),
        upstream_timeout_secs: Some(45),
        ..Default::default()
    };

    assert!(config.upstreams.is_empty());
    let converted = config.normalize_upstreams();
    assert!(converted);
    assert_eq!(config.upstreams.len(), 1);
    assert_eq!(config.upstreams[0].id, "default");
    assert_eq!(config.upstreams[0].url, "http://localhost:9200");
    assert_eq!(config.upstreams[0].timeout_secs(), 45);
}

#[test]
fn test_normalize_upstreams_noop_when_upstreams_exist() {
    let toml_str = r#"
id = "existing"
url = "http://localhost:6333"
"#;
    let upstream: UpstreamEndpointConfig = toml::from_str(toml_str).unwrap();

    let mut config = ProxyConfig {
        upstream_url: Some("http://old-url".to_string()),
        upstreams: vec![upstream],
        ..Default::default()
    };

    let converted = config.normalize_upstreams();
    assert!(!converted);
    assert_eq!(config.upstreams.len(), 1);
    assert_eq!(config.upstreams[0].id, "existing");
}

#[test]
fn test_normalize_upstreams_noop_when_no_url() {
    let mut config = ProxyConfig::default();
    let converted = config.normalize_upstreams();
    assert!(!converted);
    assert!(config.upstreams.is_empty());
}

// All env var override tests combined into a single function to avoid
// races from parallel test execution (env vars are process-global).
#[test]
fn test_apply_env_overrides() {
    // Clear all override vars first
    std::env::remove_var("CONPROXY_HOST");
    std::env::remove_var("CONPROXY_PORT");
    std::env::remove_var("CONPROXY_API_KEY");
    std::env::remove_var("CONPROXY_CACHE_MAX_ENTRIES");
    std::env::remove_var("CONPROXY_UPSTREAM_MY_QDRANT_URL");

    // Subtest 1: No vars set → 0 overrides
    {
        let mut config = ProxyConfig::default();
        let count = config.apply_env_overrides();
        assert_eq!(count, 0, "no overrides when no env vars set");
    }

    // Subtest 2: Upstream URL override
    {
        let mut config = ProxyConfig::default();
        config.upstreams.push(UpstreamEndpointConfig {
            id: "my-qdrant".to_string(),
            url: "http://localhost:6333".to_string(),
            timeout_secs: None,
            weight: None,
            priority: None,
            max_concurrent: None,
            enabled: None,
            version_endpoint: None,
            version_poll_interval_secs: None,
            upstream_type: None,
            query_mode: None,
            table: None,
            embedding_column: None,
            content_column: None,
            metadata_columns: Vec::new(),
            distance_metric: None,
            dimensions: None,
            index: None,
            search_fields: Vec::new(),
            return_fields: Vec::new(),
            api_key: None,
        });

        std::env::set_var("CONPROXY_UPSTREAM_MY_QDRANT_URL", "http://prod-qdrant:6333");
        let count = config.apply_env_overrides();
        std::env::remove_var("CONPROXY_UPSTREAM_MY_QDRANT_URL");

        assert!(count >= 1, "upstream URL override applied");
        assert_eq!(config.upstreams[0].url, "http://prod-qdrant:6333");
    }

    // Subtest 3: API key override
    {
        let mut config = ProxyConfig::default();

        std::env::set_var("CONPROXY_API_KEY", "prod-secret");
        let count = config.apply_env_overrides();
        std::env::remove_var("CONPROXY_API_KEY");

        assert!(count >= 1, "api key override applied");
        assert_eq!(config.api_key.as_deref(), Some("prod-secret"));
        assert_eq!(config.security.api_key.as_deref(), Some("prod-secret"));
    }

    // Subtest 4: Listen address override
    {
        let mut config = ProxyConfig {
            listen: Some("127.0.0.1:3000".to_string()),
            ..Default::default()
        };

        std::env::set_var("CONPROXY_HOST", "0.0.0.0");
        std::env::set_var("CONPROXY_PORT", "8080");
        let count = config.apply_env_overrides();
        std::env::remove_var("CONPROXY_HOST");
        std::env::remove_var("CONPROXY_PORT");

        assert!(count >= 1, "listen address override applied");
        assert_eq!(config.listen.as_deref(), Some("0.0.0.0:8080"));
    }

    // Subtest 5: Cache max entries override
    {
        let mut config = ProxyConfig::default();

        std::env::set_var("CONPROXY_CACHE_MAX_ENTRIES", "50000");
        let count = config.apply_env_overrides();
        std::env::remove_var("CONPROXY_CACHE_MAX_ENTRIES");

        assert!(count >= 1, "cache max entries override applied");
        assert_eq!(config.max_entries, Some(50000));
    }
}

#[test]
fn test_config_reload_is_fresh_load() {
    // reload() should produce the same result as load()
    // (We can't easily test file changes, but we verify it doesn't panic)
    // Note: This test requires config to exist or will error, just like load()
    let result = Config::reload();
    // If config exists, both should succeed; if not, both error the same way
    let load_result = Config::load();
    match (result, load_result) {
        (Ok(_), Ok(_)) => {}   // Both succeeded
        (Err(_), Err(_)) => {} // Both failed (no config file)
        _ => panic!("reload() and load() should behave identically"),
    }
}

#[test]
#[serial]
fn test_ensure_local_dirs_creates_gitignore() {
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();

    Config::ensure_local_dirs().unwrap();

    let gitignore_path = dir.path().join(".conproxy/.gitignore");
    assert!(gitignore_path.exists());

    let content = std::fs::read_to_string(&gitignore_path).unwrap();
    assert!(content.contains("cache/"));
    assert!(content.contains("*.pid"));

    std::env::set_current_dir(original_dir).unwrap();
}

#[test]
fn test_configfile_validate_catches_bad_upstream() {
    let mut config = ConfigFile::default_local();
    config.proxy.upstreams.push(UpstreamEndpointConfig {
        id: "bad".to_string(),
        url: "http://localhost".to_string(),
        upstream_type: Some("invalid_type".to_string()),
        ..Default::default()
    });
    let result = config.validate();
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("invalid upstream_type"));
}

// === AgentConfig tests ===

#[test]
fn test_agent_config_context_access_unrestricted() {
    let agent = AgentConfig {
        id: "agent-1".to_string(),
        api_key: "key-1".to_string(),
        default_context: None,
        allowed_contexts: vec![],
        priority_class: None,
        rate_limit_rps: None,
        enabled: true,
    };
    // Empty allowed_contexts means unrestricted
    assert!(agent.can_access_context("any-context"));
    assert!(agent.can_access_context("another-context"));
}

#[test]
fn test_agent_config_context_access_restricted() {
    let agent = AgentConfig {
        id: "agent-2".to_string(),
        api_key: "key-2".to_string(),
        default_context: None,
        allowed_contexts: vec!["codebase-rust".to_string(), "codebase-python".to_string()],
        priority_class: Some(2),
        rate_limit_rps: Some(50),
        enabled: true,
    };
    assert!(agent.can_access_context("codebase-rust"));
    assert!(agent.can_access_context("codebase-python"));
    assert!(!agent.can_access_context("codebase-go"));
}

#[test]
fn test_agent_config_priority_class_default() {
    let agent = AgentConfig {
        id: "agent-3".to_string(),
        api_key: "key-3".to_string(),
        default_context: None,
        allowed_contexts: vec![],
        priority_class: None,
        rate_limit_rps: None,
        enabled: true,
    };
    assert_eq!(agent.priority_class(), 0);
}

#[test]
fn test_proxy_config_agents_default_empty() {
    let config = ProxyConfig::default();
    assert!(config.agents.is_empty());
    assert!(!config.has_agents());
}

#[test]
fn test_proxy_config_with_agents() {
    let mut config = ProxyConfig::default();
    config.agents.push(AgentConfig {
        id: "code-review".to_string(),
        api_key: "crv-xxx".to_string(),
        default_context: None,
        allowed_contexts: vec!["codebase-rust".to_string()],
        priority_class: Some(2),
        rate_limit_rps: Some(50),
        enabled: true,
    });
    config.agents.push(AgentConfig {
        id: "docs-gen".to_string(),
        api_key: "dga-yyy".to_string(),
        default_context: None,
        allowed_contexts: vec![],
        priority_class: Some(1),
        rate_limit_rps: Some(20),
        enabled: false,
    });

    assert!(config.has_agents());
    assert_eq!(config.agents().len(), 2);
    assert_eq!(config.enabled_agents().len(), 1);
    assert_eq!(config.enabled_agents()[0].id, "code-review");
}

#[test]
fn test_agent_config_toml_roundtrip() {
    let toml_str = r#"
        [[proxy.agents]]
        id = "code-review-agent"
        api_key = "crv-xxxxxxxx"
        allowed_contexts = ["codebase-rust", "codebase-python"]
        priority_class = 2
        rate_limit_rps = 50

        [[proxy.agents]]
        id = "docs-gen-agent"
        api_key = "dga-yyyyyyyy"
        priority_class = 1
        rate_limit_rps = 20
    "#;

    let config: ConfigFile = toml::from_str(toml_str).unwrap();
    assert_eq!(config.proxy.agents.len(), 2);

    let agent1 = &config.proxy.agents[0];
    assert_eq!(agent1.id, "code-review-agent");
    assert_eq!(agent1.api_key, "crv-xxxxxxxx");
    assert_eq!(
        agent1.allowed_contexts,
        vec!["codebase-rust", "codebase-python"]
    );
    assert_eq!(agent1.priority_class, Some(2));
    assert_eq!(agent1.rate_limit_rps, Some(50));
    assert!(agent1.enabled);

    let agent2 = &config.proxy.agents[1];
    assert_eq!(agent2.id, "docs-gen-agent");
    assert!(agent2.allowed_contexts.is_empty());
    assert!(agent2.enabled); // default true
}

#[test]
fn test_agent_config_merge_local_overrides() {
    let global = ProxyConfig {
        agents: vec![AgentConfig {
            id: "global-agent".to_string(),
            api_key: "glo-xxx".to_string(),
            default_context: None,
            allowed_contexts: vec![],
            priority_class: None,
            rate_limit_rps: None,
            enabled: true,
        }],
        ..Default::default()
    };

    // Local with agents replaces global
    let local = ProxyConfig {
        agents: vec![AgentConfig {
            id: "local-agent".to_string(),
            api_key: "loc-yyy".to_string(),
            default_context: None,
            allowed_contexts: vec!["my-ctx".to_string()],
            priority_class: Some(1),
            rate_limit_rps: Some(100),
            enabled: true,
        }],
        ..Default::default()
    };

    let merged = local.merge_with(&global);
    assert_eq!(merged.agents.len(), 1);
    assert_eq!(merged.agents[0].id, "local-agent");
}

#[test]
fn test_agent_config_merge_inherits_from_base() {
    let global = ProxyConfig {
        agents: vec![AgentConfig {
            id: "base-agent".to_string(),
            api_key: "base-xxx".to_string(),
            default_context: None,
            allowed_contexts: vec![],
            priority_class: None,
            rate_limit_rps: None,
            enabled: true,
        }],
        ..Default::default()
    };

    // Local with no agents inherits from global
    let local = ProxyConfig::default();
    let merged = local.merge_with(&global);
    assert_eq!(merged.agents.len(), 1);
    assert_eq!(merged.agents[0].id, "base-agent");
}

// =============================================================================
// ConfigFile::validate() — deterministic tests
// =============================================================================

/// Helper: create a valid upstream for mutation-based tests.
fn valid_upstream(id: &str, url: &str) -> UpstreamEndpointConfig {
    UpstreamEndpointConfig {
        id: id.to_string(),
        url: url.to_string(),
        ..Default::default()
    }
}

// --- Branch 1: upstream URL scheme ---

#[test]
fn test_validate_upstream_url_ftp_rejected() {
    let mut config = ConfigFile::default_local();
    config
        .proxy
        .upstreams
        .push(valid_upstream("u1", "ftp://host/path"));
    let err = config.validate().unwrap_err();
    assert!(err.contains("http(s)://"), "got: {err}");
}

#[test]
fn test_validate_upstream_url_postgres_accepted() {
    let mut config = ConfigFile::default_local();
    config
        .proxy
        .upstreams
        .push(valid_upstream("u1", "postgres://host/db"));
    assert!(config.validate().is_ok());
}

#[test]
fn test_validate_upstream_url_postgresql_accepted() {
    let mut config = ConfigFile::default_local();
    config
        .proxy
        .upstreams
        .push(valid_upstream("u1", "postgresql://host/db"));
    assert!(config.validate().is_ok());
}

// --- Branch 2: per-upstream timeout_secs ---

#[test]
fn test_validate_upstream_timeout_zero_rejected() {
    let mut config = ConfigFile::default_local();
    let mut u = valid_upstream("u1", "http://localhost");
    u.timeout_secs = Some(0);
    config.proxy.upstreams.push(u);
    let err = config.validate().unwrap_err();
    assert!(err.contains("timeout_secs"), "got: {err}");
}

#[test]
fn test_validate_upstream_timeout_301_rejected() {
    let mut config = ConfigFile::default_local();
    let mut u = valid_upstream("u1", "http://localhost");
    u.timeout_secs = Some(301);
    config.proxy.upstreams.push(u);
    let err = config.validate().unwrap_err();
    assert!(err.contains("timeout_secs"), "got: {err}");
}

#[test]
fn test_validate_upstream_timeout_1_accepted() {
    let mut config = ConfigFile::default_local();
    let mut u = valid_upstream("u1", "http://localhost");
    u.timeout_secs = Some(1);
    config.proxy.upstreams.push(u);
    assert!(config.validate().is_ok());
}

#[test]
fn test_validate_upstream_timeout_300_accepted() {
    let mut config = ConfigFile::default_local();
    let mut u = valid_upstream("u1", "http://localhost");
    u.timeout_secs = Some(300);
    config.proxy.upstreams.push(u);
    assert!(config.validate().is_ok());
}

// --- Branch 3: legacy upstream_url scheme ---

#[test]
fn test_validate_legacy_upstream_url_postgres_rejected() {
    let mut config = ConfigFile::default_local();
    config.proxy.upstream_url = Some("postgres://host/db".to_string());
    let err = config.validate().unwrap_err();
    assert!(err.contains("upstream_url"), "got: {err}");
}

#[test]
fn test_validate_legacy_upstream_url_https_accepted() {
    let mut config = ConfigFile::default_local();
    config.proxy.upstream_url = Some("https://host/path".to_string());
    assert!(config.validate().is_ok());
}

#[test]
fn test_validate_legacy_upstream_url_none_skipped() {
    let config = ConfigFile::default_local();
    assert!(config.proxy.upstream_url.is_none());
    assert!(config.validate().is_ok());
}

// --- Branch 4: upstream_timeout_secs ---

#[test]
fn test_validate_upstream_timeout_secs_zero_rejected() {
    let mut config = ConfigFile::default_local();
    config.proxy.upstream_timeout_secs = Some(0);
    let err = config.validate().unwrap_err();
    assert!(err.contains("upstream_timeout_secs"), "got: {err}");
}

#[test]
fn test_validate_upstream_timeout_secs_301_rejected() {
    let mut config = ConfigFile::default_local();
    config.proxy.upstream_timeout_secs = Some(301);
    let err = config.validate().unwrap_err();
    assert!(err.contains("upstream_timeout_secs"), "got: {err}");
}

#[test]
fn test_validate_upstream_timeout_secs_1_accepted() {
    let mut config = ConfigFile::default_local();
    config.proxy.upstream_timeout_secs = Some(1);
    assert!(config.validate().is_ok());
}

#[test]
fn test_validate_upstream_timeout_secs_300_accepted() {
    let mut config = ConfigFile::default_local();
    config.proxy.upstream_timeout_secs = Some(300);
    assert!(config.validate().is_ok());
}

// --- Branch 5: max_entries == 0 ---

#[test]
fn test_validate_max_entries_zero_rejected() {
    let mut config = ConfigFile::default_local();
    config.proxy.max_entries = Some(0);
    let err = config.validate().unwrap_err();
    assert!(err.contains("max_entries"), "got: {err}");
}

#[test]
fn test_validate_max_entries_one_accepted() {
    let mut config = ConfigFile::default_local();
    config.proxy.max_entries = Some(1);
    assert!(config.validate().is_ok());
}

#[test]
fn test_validate_max_entries_none_skipped() {
    let config = ConfigFile::default_local();
    assert!(config.proxy.max_entries.is_none());
    assert!(config.validate().is_ok());
}

// --- Branch 6: pool.max_connections ---

#[test]
fn test_validate_pool_max_connections_zero_rejected() {
    let mut config = ConfigFile::default_local();
    config.proxy.pool.max_connections = 0;
    let err = config.validate().unwrap_err();
    assert!(err.contains("max_connections"), "got: {err}");
}

#[test]
fn test_validate_pool_max_connections_10001_rejected() {
    let mut config = ConfigFile::default_local();
    config.proxy.pool.max_connections = 10_001;
    let err = config.validate().unwrap_err();
    assert!(err.contains("max_connections"), "got: {err}");
}

#[test]
fn test_validate_pool_max_connections_1_accepted() {
    let mut config = ConfigFile::default_local();
    config.proxy.pool.max_connections = 1;
    assert!(config.validate().is_ok());
}

#[test]
fn test_validate_pool_max_connections_10000_accepted() {
    let mut config = ConfigFile::default_local();
    config.proxy.pool.max_connections = 10_000;
    assert!(config.validate().is_ok());
}

#[test]
fn test_validate_pool_max_connections_default_accepted() {
    let config = ConfigFile::default_local();
    // Default is 100
    assert_eq!(config.proxy.pool.max_connections, 100);
    assert!(config.validate().is_ok());
}

// --- Branch 7: listen_backlog > 65535 ---

#[test]
fn test_validate_listen_backlog_65536_rejected() {
    let mut config = ConfigFile::default_local();
    config.proxy.socket_tuning.listen_backlog = 65_536;
    let err = config.validate().unwrap_err();
    assert!(err.contains("listen_backlog"), "got: {err}");
}

#[test]
fn test_validate_listen_backlog_65535_accepted() {
    let mut config = ConfigFile::default_local();
    config.proxy.socket_tuning.listen_backlog = 65_535;
    assert!(config.validate().is_ok());
}

#[test]
fn test_validate_listen_backlog_default_accepted() {
    let config = ConfigFile::default_local();
    // Default is 4096
    assert_eq!(config.proxy.socket_tuning.listen_backlog, 4096);
    assert!(config.validate().is_ok());
}

// --- Branch 8: fresh_duration_secs == 0 ---

#[test]
fn test_validate_fresh_duration_zero_rejected() {
    let mut config = ConfigFile::default_local();
    config.proxy.fresh_duration_secs = Some(0);
    let err = config.validate().unwrap_err();
    assert!(err.contains("fresh_duration_secs"), "got: {err}");
}

#[test]
fn test_validate_fresh_duration_one_accepted() {
    let mut config = ConfigFile::default_local();
    config.proxy.fresh_duration_secs = Some(1);
    assert!(config.validate().is_ok());
}

// --- Branch 9: stale_duration_secs == 0 ---

#[test]
fn test_validate_stale_duration_zero_rejected() {
    let mut config = ConfigFile::default_local();
    config.proxy.stale_duration_secs = Some(0);
    let err = config.validate().unwrap_err();
    assert!(err.contains("stale_duration_secs"), "got: {err}");
}

#[test]
fn test_validate_stale_duration_one_accepted() {
    let mut config = ConfigFile::default_local();
    config.proxy.stale_duration_secs = Some(1);
    assert!(config.validate().is_ok());
}

// --- Branch 10: refresh_interval_secs == 0 ---

#[test]
fn test_validate_refresh_interval_zero_rejected() {
    let mut config = ConfigFile::default_local();
    config.proxy.refresh_interval_secs = Some(0);
    let err = config.validate().unwrap_err();
    assert!(err.contains("refresh_interval_secs"), "got: {err}");
}

#[test]
fn test_validate_refresh_interval_one_accepted() {
    let mut config = ConfigFile::default_local();
    config.proxy.refresh_interval_secs = Some(1);
    assert!(config.validate().is_ok());
}

// --- Default config validates ---

#[test]
fn test_validate_default_local_passes() {
    assert!(ConfigFile::default_local().validate().is_ok());
}

// =============================================================================
// proptest: config validation range properties
// =============================================================================

mod proptest_config {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn prop_upstream_timeout_invalid_always_rejected(t in prop_oneof![Just(0u64), 301..=u64::MAX]) {
            let mut config = ConfigFile::default_local();
            let mut u = UpstreamEndpointConfig {
                id: "u1".to_string(),
                url: "http://localhost".to_string(),
                ..Default::default()
            };
            u.timeout_secs = Some(t);
            config.proxy.upstreams.push(u);
            prop_assert!(config.validate().is_err());
        }

        #[test]
        fn prop_upstream_timeout_valid_always_accepted(t in 1u64..=300) {
            let mut config = ConfigFile::default_local();
            let mut u = UpstreamEndpointConfig {
                id: "u1".to_string(),
                url: "http://localhost".to_string(),
                ..Default::default()
            };
            u.timeout_secs = Some(t);
            config.proxy.upstreams.push(u);
            prop_assert!(config.validate().is_ok());
        }

        #[test]
        fn prop_legacy_timeout_invalid_always_rejected(t in prop_oneof![Just(0u64), 301..=u64::MAX]) {
            let mut config = ConfigFile::default_local();
            config.proxy.upstream_timeout_secs = Some(t);
            prop_assert!(config.validate().is_err());
        }

        #[test]
        fn prop_legacy_timeout_valid_always_accepted(t in 1u64..=300) {
            let mut config = ConfigFile::default_local();
            config.proxy.upstream_timeout_secs = Some(t);
            prop_assert!(config.validate().is_ok());
        }

        #[test]
        fn prop_pool_max_connections_invalid_always_rejected(n in prop_oneof![Just(0usize), 10_001..=usize::MAX]) {
            let mut config = ConfigFile::default_local();
            config.proxy.pool.max_connections = n;
            prop_assert!(config.validate().is_err());
        }

        #[test]
        fn prop_pool_max_connections_valid_always_accepted(n in 1usize..=10_000) {
            let mut config = ConfigFile::default_local();
            config.proxy.pool.max_connections = n;
            prop_assert!(config.validate().is_ok());
        }

        #[test]
        fn prop_listen_backlog_over_limit_always_rejected(b in 65_536u32..=u32::MAX) {
            let mut config = ConfigFile::default_local();
            config.proxy.socket_tuning.listen_backlog = b;
            prop_assert!(config.validate().is_err());
        }

        #[test]
        fn prop_listen_backlog_in_range_always_accepted(b in 0u32..=65_535) {
            let mut config = ConfigFile::default_local();
            config.proxy.socket_tuning.listen_backlog = b;
            prop_assert!(config.validate().is_ok());
        }
    }
}

// =============================================================================
// Distill Config Tests
// =============================================================================

#[test]
fn test_distill_config_default() {
    let config = DistillConfig::default();
    assert_eq!(config.format(), "md");
    assert!(!config.include_stale());
    assert!(config.output_dir.is_none());
    assert!(config.post_process_cmd.is_none());
    assert!(config.validate().is_ok());
}

#[test]
fn test_distill_config_validate_format() {
    let mut config = DistillConfig::default();
    config.format = Some("md".to_string());
    assert!(config.validate().is_ok());

    config.format = Some("json".to_string());
    assert!(config.validate().is_ok());

    config.format = Some("both".to_string());
    assert!(config.validate().is_ok());

    config.format = Some("xml".to_string());
    let err = config.validate().unwrap_err();
    assert!(err.contains("proxy.distill.format"));
    assert!(err.contains("'xml'"));
}

#[test]
fn test_distill_config_merge_with() {
    let base = DistillConfig {
        output_dir: Some("/var/distill".to_string()),
        post_process_cmd: Some("echo done".to_string()),
        format: Some("json".to_string()),
        include_stale: Some(true),
    };
    let local = DistillConfig {
        output_dir: Some("/tmp/distill".to_string()),
        post_process_cmd: None,
        format: None,
        include_stale: Some(false),
    };
    let merged = local.merge_with(&base);
    assert_eq!(merged.output_dir.as_deref(), Some("/tmp/distill"));
    assert_eq!(merged.post_process_cmd.as_deref(), Some("echo done"));
    assert_eq!(merged.format.as_deref(), Some("json"));
    assert!(!merged.include_stale());
}

#[test]
fn test_proxy_config_distill_field_default() {
    let config = ProxyConfig::default();
    // distill field must exist with default values
    assert!(config.distill.output_dir.is_none());
    assert_eq!(config.distill.format(), "md");
}

#[test]
fn test_config_file_validate_distill_rejects_bad_format() {
    let mut config = ConfigFile::default_local();
    config.proxy.distill.format = Some("yaml".to_string());
    let err = config.validate().unwrap_err();
    assert!(err.contains("proxy.distill.format"));
    assert!(err.contains("'yaml'"));
}

// =============================================================================
// Triplet tests: default + merge_with + validate for each config struct
// =============================================================================

// --- FederatedConfig ---

#[test]
fn test_federated_config_default() {
    let config = FederatedConfig::default();
    assert!(!config.enabled());
    assert_eq!(config.min_local_results(), 3);
    assert!((config.min_local_confidence() - 0.7).abs() < f32::EPSILON);
    assert_eq!(config.merge_mode(), "local_only_fallback");
    assert_eq!(config.max_merged_results(), 10);
}

#[test]
fn test_federated_config_merge_with() {
    let base = FederatedConfig {
        min_local_results: Some(5),
        min_local_confidence: Some(0.8),
        merge_mode: Some("interleave".to_string()),
        ..Default::default()
    };
    let local = FederatedConfig {
        min_local_results: None,
        merge_mode: Some("local_priority".to_string()),
        ..Default::default()
    };
    let merged = local.merge_with(&base);
    assert_eq!(merged.min_local_results(), 5);
    assert!((merged.min_local_confidence() - 0.8).abs() < f32::EPSILON);
    assert_eq!(merged.merge_mode(), "local_priority");
}

#[test]
fn test_federated_config_validate() {
    let config = FederatedConfig::default();
    assert!(config.validate().is_ok());

    let mut config = FederatedConfig::default();
    config.min_local_confidence = Some(1.5);
    assert!(config.validate().is_err());

    let mut config = FederatedConfig::default();
    config.merge_mode = Some("invalid".to_string());
    assert!(config.validate().is_err());

    let mut config = FederatedConfig::default();
    config.max_merged_results = Some(0);
    assert!(config.validate().is_err());

    let mut config = FederatedConfig::default();
    config.min_local_results = Some(0);
    assert!(config.validate().is_err());
}

// --- AdvancedSecurityConfig ---

#[test]
fn test_advanced_security_config_default() {
    let config = AdvancedSecurityConfig::default();
    assert!(!config.enabled());
    assert!(!config.tls_pinning());
    assert!(!config.replay_detection());
    assert_eq!(config.replay_window_seconds(), 300);
}

#[test]
fn test_advanced_security_config_merge_with() {
    let base = AdvancedSecurityConfig {
        enabled: Some(true),
        signature_algorithm: Some("blake3".to_string()),
        replay_window_seconds: Some(600),
        ..Default::default()
    };
    let local = AdvancedSecurityConfig {
        enabled: None,
        signature_algorithm: Some("hmac-sha256".to_string()),
        ..Default::default()
    };
    let merged = local.merge_with(&base);
    assert!(merged.enabled());
    assert_eq!(merged.signature_algorithm.as_deref(), Some("hmac-sha256"));
    assert_eq!(merged.replay_window_seconds(), 600);
}

#[test]
fn test_advanced_security_config_validate() {
    let config = AdvancedSecurityConfig::default();
    assert!(config.validate().is_ok());

    let mut config = AdvancedSecurityConfig::default();
    config.signature_algorithm = Some("md5".to_string());
    assert!(config.validate().is_err());

    let mut config = AdvancedSecurityConfig::default();
    config.replay_window_seconds = Some(0);
    assert!(config.validate().is_err());
}

// --- SecurityConfig ---

#[test]
fn test_security_config_default() {
    let config = SecurityConfig::default();
    assert!(config.api_key.is_none());
    assert!(!config.rate_limit.enabled());
}

#[test]
fn test_security_config_merge_with() {
    let base = SecurityConfig {
        api_key: Some("base-key".to_string()),
        ..Default::default()
    };
    let local = SecurityConfig {
        api_key: Some("local-key".to_string()),
        ..Default::default()
    };
    let merged = local.merge_with(&base);
    assert_eq!(merged.api_key.as_deref(), Some("local-key"));
}

#[test]
fn test_security_config_validate() {
    let config = SecurityConfig::default();
    assert!(config.validate().is_ok());

    let mut config = SecurityConfig::default();
    config.api_key = Some(String::new());
    assert!(config.validate().is_err());

    let mut config = SecurityConfig::default();
    config.api_key = Some("valid-key".to_string());
    assert!(config.validate().is_ok());
}

// --- ProxyCircuitBreakerConfig ---

#[test]
fn test_proxy_circuit_breaker_config_default() {
    let config = ProxyCircuitBreakerConfig::default();
    assert_eq!(config.failure_threshold, 25);
    assert_eq!(config.success_threshold, 2);
    assert_eq!(config.open_duration_secs, 30);
    assert_eq!(config.failure_window_secs, 60);
}

#[test]
fn test_proxy_circuit_breaker_config_merge_with() {
    // ProxyCircuitBreakerConfig is cloned (full override) in ProxyConfig merge
    let global = ProxyConfig {
        circuit_breaker: ProxyCircuitBreakerConfig {
            failure_threshold: 50,
            ..Default::default()
        },
        ..Default::default()
    };
    let local = ProxyConfig {
        circuit_breaker: ProxyCircuitBreakerConfig {
            failure_threshold: 10,
            ..Default::default()
        },
        ..Default::default()
    };
    let merged = local.merge_with(&global);
    assert_eq!(merged.circuit_breaker.failure_threshold, 10);
}

#[test]
fn test_proxy_circuit_breaker_config_validate() {
    let config = ProxyCircuitBreakerConfig::default();
    assert!(config.validate().is_ok());

    let config = ProxyCircuitBreakerConfig {
        failure_threshold: 0,
        ..Default::default()
    };
    assert!(config.validate().is_err());

    let config = ProxyCircuitBreakerConfig {
        success_threshold: 0,
        ..Default::default()
    };
    assert!(config.validate().is_err());

    let config = ProxyCircuitBreakerConfig {
        open_duration_secs: 0,
        ..Default::default()
    };
    assert!(config.validate().is_err());

    let config = ProxyCircuitBreakerConfig {
        failure_window_secs: 0,
        ..Default::default()
    };
    assert!(config.validate().is_err());
}

// --- ProxyRateLimitConfig ---

#[test]
fn test_proxy_rate_limit_config_default() {
    let config = ProxyRateLimitConfig::default();
    assert!(!config.enabled());
    assert_eq!(config.requests_per_second(), 100);
    assert_eq!(config.burst_size(), 50);
}

#[test]
fn test_proxy_rate_limit_config_merge_with() {
    let base = ProxyRateLimitConfig {
        enabled: Some(true),
        requests_per_second: Some(200),
        ..Default::default()
    };
    let local = ProxyRateLimitConfig {
        enabled: None,
        requests_per_second: Some(50),
        ..Default::default()
    };
    let merged = local.merge_with(&base);
    assert!(merged.enabled());
    assert_eq!(merged.requests_per_second(), 50);
    assert_eq!(merged.burst_size(), 50);
}

#[test]
fn test_proxy_rate_limit_config_validate() {
    let config = ProxyRateLimitConfig::default();
    assert!(config.validate().is_ok());

    let mut config = ProxyRateLimitConfig::default();
    config.requests_per_second = Some(0);
    assert!(config.validate().is_err());

    let mut config = ProxyRateLimitConfig::default();
    config.burst_size = Some(0);
    assert!(config.validate().is_err());
}

// --- ProxyRetryConfig ---

#[test]
fn test_proxy_retry_config_default() {
    let config = ProxyRetryConfig::default();
    assert!(config.enabled());
    assert_eq!(config.max_retries(), 3);
    assert_eq!(config.initial_delay_ms(), 100);
    assert_eq!(config.max_delay_ms(), 10000);
    assert!((config.backoff_multiplier() - 2.0).abs() < f64::EPSILON);
    assert!(config.on_network_error());
    assert!(config.on_timeout());
    assert!(config.on_server_error());
    assert!(config.on_rate_limited());
}

#[test]
fn test_proxy_retry_config_merge_with() {
    let base = ProxyRetryConfig {
        max_retries: Some(5),
        initial_delay_ms: Some(200),
        backoff_multiplier: Some(3.0),
        ..Default::default()
    };
    let local = ProxyRetryConfig {
        max_retries: None,
        initial_delay_ms: Some(500),
        ..Default::default()
    };
    let merged = local.merge_with(&base);
    assert_eq!(merged.max_retries(), 5);
    assert_eq!(merged.initial_delay_ms(), 500);
    assert!((merged.backoff_multiplier() - 3.0).abs() < f64::EPSILON);
}

#[test]
fn test_proxy_retry_config_validate() {
    let config = ProxyRetryConfig::default();
    assert!(config.validate().is_ok());

    let mut config = ProxyRetryConfig::default();
    config.max_retries = Some(0);
    assert!(config.validate().is_ok());

    // 0 for initial_delay_ms = immediate retry, valid
    let mut config = ProxyRetryConfig::default();
    config.initial_delay_ms = Some(0);
    assert!(config.validate().is_ok());

    let mut config = ProxyRetryConfig::default();
    config.max_delay_ms = Some(0);
    assert!(config.validate().is_err());

    let mut config = ProxyRetryConfig::default();
    config.initial_delay_ms = Some(500);
    config.max_delay_ms = Some(100);
    assert!(config.validate().is_err());

    let mut config = ProxyRetryConfig::default();
    config.backoff_multiplier = Some(0.5);
    assert!(config.validate().is_err());
}

// --- ProxyScopeConfig ---

#[test]
fn test_proxy_scope_config_default() {
    let config = ProxyScopeConfig::default();
    assert!(config.seeds.is_empty());
    assert_eq!(config.mode(), "filter");
    assert!((config.min_seed_similarity() - 0.25).abs() < f32::EPSILON);
    assert!((config.seed_weight() - 0.3).abs() < f32::EPSILON);
}

#[test]
fn test_proxy_scope_config_merge_with() {
    let base = ProxyScopeConfig {
        weighted_phrases: vec![],
        seeds: vec!["base-seed".to_string()],
        mode: Some("rerank".to_string()),
        min_seed_similarity: Some(0.5),
        ..Default::default()
    };
    let local = ProxyScopeConfig {
        weighted_phrases: vec![],
        seeds: vec![],
        mode: Some("boost".to_string()),
        ..Default::default()
    };
    let merged = local.merge_with(&base);
    assert_eq!(merged.seeds, vec!["base-seed"]);
    assert_eq!(merged.mode(), "boost");
    assert!((merged.min_seed_similarity() - 0.5).abs() < f32::EPSILON);
}

#[test]
fn test_proxy_scope_config_validate() {
    let config = ProxyScopeConfig::default();
    assert!(config.validate().is_ok());

    let mut config = ProxyScopeConfig::default();
    config.mode = Some("invalid".to_string());
    assert!(config.validate().is_err());

    let mut config = ProxyScopeConfig::default();
    config.min_seed_similarity = Some(1.5);
    assert!(config.validate().is_err());

    let mut config = ProxyScopeConfig::default();
    config.seed_weight = Some(-0.1);
    assert!(config.validate().is_err());
}

#[test]
fn test_proxy_scope_weighted_phrases_toml() {
    let toml = r#"
weighted_phrases = [
  { text = "refund", weight = 1.5, min_similarity = 0.3 },
  { text = "billing" }
]
mode = "filter"
min_similarity = 0.25
scope_weight = 0.4
"#;
    let config: ProxyScopeConfig = toml::from_str(toml).expect("parse weighted_phrases");
    assert_eq!(config.weighted_phrases.len(), 2);
    assert_eq!(config.weighted_phrases[0].text, "refund");
    assert!((config.weighted_phrases[0].weight - 1.5).abs() < f32::EPSILON);
    assert_eq!(config.weighted_phrases[0].min_similarity, Some(0.3));
    assert!((config.weighted_phrases[1].weight - 1.0).abs() < f32::EPSILON);
    assert!((config.min_similarity() - 0.25).abs() < f32::EPSILON);
    assert!((config.scope_weight() - 0.4).abs() < f32::EPSILON);
    assert!(config.validate().is_ok());
    let texts = config.phrase_texts();
    assert_eq!(texts, vec!["refund", "billing"]);
}

#[test]
fn test_proxy_scope_legacy_seeds_alias_phrases() {
    let toml = r#"
phrases = ["a", "b"]
min_seed_similarity = 0.3
"#;
    let config: ProxyScopeConfig = toml::from_str(toml).expect("parse phrases alias");
    assert_eq!(config.seeds, vec!["a", "b"]);
    let eff = config.effective_phrases();
    assert_eq!(eff.len(), 2);
    assert!((eff[0].weight - 1.0).abs() < f32::EPSILON);
}

// --- PerUpstreamCacheConfig ---

#[test]
fn test_per_upstream_cache_config_default() {
    let config = PerUpstreamCacheConfig::default();
    assert!(!config.enabled());
    assert_eq!(config.max_entries_per_upstream(), 500);
}

#[test]
fn test_per_upstream_cache_config_merge_with() {
    // PerUpstreamCacheConfig is merged inline in ProxyCacheConfig::merge_with
    let base = ProxyCacheConfig {
        per_upstream: PerUpstreamCacheConfig {
            enabled: Some(true),
            max_entries_per_upstream: Some(1000),
        },
        ..Default::default()
    };
    let local = ProxyCacheConfig {
        per_upstream: PerUpstreamCacheConfig {
            enabled: None,
            max_entries_per_upstream: Some(200),
        },
        ..Default::default()
    };
    let merged = local.merge_with(&base);
    assert!(merged.per_upstream.enabled());
    assert_eq!(merged.per_upstream.max_entries_per_upstream(), 200);
}

#[test]
fn test_per_upstream_cache_config_validate() {
    let config = PerUpstreamCacheConfig::default();
    assert!(config.validate().is_ok());

    let mut config = PerUpstreamCacheConfig::default();
    config.max_entries_per_upstream = Some(0);
    assert!(config.validate().is_err());
}

// --- ProxyCacheConfig ---

#[test]
fn test_proxy_cache_config_default() {
    let config = ProxyCacheConfig::default();
    assert_eq!(config.max_memory_mb(), 256);
    assert_eq!(config.max_entry_size_kb(), 512);
    assert_eq!(config.eviction_policy(), "lru");
    assert!(!config.normalized_matching());
}

#[test]
fn test_proxy_cache_config_merge_with() {
    let base = ProxyCacheConfig {
        max_memory_mb: Some(512),
        max_entry_size_kb: Some(1024),
        eviction_policy: Some("lfu".to_string()),
        ..Default::default()
    };
    let local = ProxyCacheConfig {
        max_entry_size_kb: Some(2048),
        eviction_policy: None,
        ..Default::default()
    };
    let merged = local.merge_with(&base);
    assert_eq!(merged.max_memory_mb(), 512);
    assert_eq!(merged.max_entry_size_kb(), 2048);
    assert_eq!(merged.eviction_policy(), "lfu");
}

#[test]
fn test_proxy_cache_config_validate() {
    let config = ProxyCacheConfig::default();
    assert!(config.validate().is_ok());

    let mut config = ProxyCacheConfig::default();
    config.eviction_policy = Some("fifo".to_string());
    assert!(config.validate().is_err());

    let mut config = ProxyCacheConfig::default();
    config.max_entry_size_kb = Some(0);
    assert!(config.validate().is_err());

    // PerUpstreamCache validation through ProxyCacheConfig
    let mut config = ProxyCacheConfig::default();
    config.per_upstream.max_entries_per_upstream = Some(0);
    assert!(config.validate().is_err());
}

#[test]
fn test_llm_section_hard_rejected() {
    let toml = r#"
[upstreams.dummy]
url = "http://127.0.0.1:1"
type = "elasticsearch"
index = "test"

[contexts.default]
default = true

[[contexts.default.upstreams]]
ref = "dummy"
priority = 0

[llm]
cache_enabled = true

[[llm.providers]]
id = "openai"
provider = "openai"
base_url = "https://api.openai.com"
"#;
    let cfg: ConfigFile = toml::from_str(toml).expect("toml should parse");
    // Deserialization succeeds (field captured); validate must fail.
    let err = cfg.validate().expect_err("validate must reject [llm]");
    assert!(
        err.contains("removed") || err.contains("[llm]"),
        "unexpected err: {err}"
    );
}

// === WebUiConfig tests ===

#[test]
fn test_web_ui_config_default() {
    let cfg: ConfigFile = toml::from_str(
        r#"
[packages.test]
git = "https://github.com/test/pkg"
"#,
    )
    .unwrap();
    assert!(!cfg.proxy.web_ui.enabled);
}

#[test]
fn test_web_ui_config_enabled() {
    let cfg: ConfigFile = toml::from_str(
        r#"
[proxy.web_ui]
enabled = true

[packages.test]
git = "https://github.com/test/pkg"
"#,
    )
    .unwrap();
    assert!(cfg.proxy.web_ui.enabled);
}

#[test]
fn test_web_ui_config_merge() {
    let overlay = WebUiConfig { enabled: true };
    let base = WebUiConfig { enabled: false };
    assert!(overlay.merge_with(&base).enabled);

    let overlay = WebUiConfig { enabled: false };
    let base = WebUiConfig { enabled: true };
    assert!(overlay.merge_with(&base).enabled);

    let overlay = WebUiConfig { enabled: false };
    let base = WebUiConfig { enabled: false };
    assert!(!overlay.merge_with(&base).enabled);
}
