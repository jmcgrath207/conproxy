use super::*;

#[test]
fn test_default_config() {
    let config = EmbedderConfig::default();
    assert_eq!(config.model_name, "all-MiniLM-L6-v2");
    assert_eq!(config.cache_max_entries, 10_000);
    assert_eq!(config.max_batch_size, 32);
    assert!(config.warmup_on_start);
}

#[test]
fn test_config_new() {
    let config = EmbedderConfig::new("bge-small-en");
    assert_eq!(config.model_name, "bge-small-en");
}

#[test]
fn test_config_builder() {
    let config = EmbedderConfig::new("test-model")
        .with_cache_max_entries(5000)
        .with_max_batch_size(64)
        .with_warmup(false)
        .with_cache_ttl(Duration::from_secs(1800));

    assert_eq!(config.model_name, "test-model");
    assert_eq!(config.cache_max_entries, 5000);
    assert_eq!(config.max_batch_size, 64);
    assert!(!config.warmup_on_start);
    assert_eq!(config.cache_ttl, Duration::from_secs(1800));
}

#[test]
fn test_without_cache() {
    let config = EmbedderConfig::default().without_cache();
    assert_eq!(config.cache_max_entries, 0);
}

#[test]
fn test_with_max_batch_size() {
    let config = EmbedderConfig::default().with_max_batch_size(64);
    assert_eq!(config.max_batch_size, 64);
}

#[test]
fn test_with_request_timeout() {
    let config = EmbedderConfig::default().with_request_timeout(Duration::from_secs(60));
    assert_eq!(config.request_timeout, Duration::from_secs(60));
}

#[test]
fn test_full_builder_chain() {
    let config = EmbedderConfig::new("custom-model")
        .with_cache_max_entries(500)
        .with_cache_ttl(Duration::from_secs(900))
        .with_max_batch_size(16)
        .with_warmup(false)
        .with_request_timeout(Duration::from_secs(10))
        .without_cache();

    assert_eq!(config.model_name, "custom-model");
    assert_eq!(config.cache_max_entries, 0); // without_cache overrides
    assert_eq!(config.cache_ttl, Duration::from_secs(900));
    assert_eq!(config.max_batch_size, 16);
    assert!(!config.warmup_on_start);
    assert_eq!(config.request_timeout, Duration::from_secs(10));
}

#[test]
fn test_default_values() {
    let config = EmbedderConfig::default();
    assert_eq!(config.cache_ttl, Duration::from_secs(3600));
    assert_eq!(config.warmup_iterations, 3);
    assert_eq!(config.request_timeout, Duration::from_secs(30));
}
