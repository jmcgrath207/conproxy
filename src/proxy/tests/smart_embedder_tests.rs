use super::*;

#[test]
fn test_smart_embedder_creation() {
    let embedder = SmartEmbedder::with_defaults();
    assert!(!embedder.is_loaded());
    assert!(embedder.dimensions().is_none());
}

#[test]
fn test_smart_embedder_config() {
    let config = EmbedderConfig::new("test-model")
        .with_cache_max_entries(100)
        .with_warmup(false);
    let embedder = SmartEmbedder::new(config);

    let stats = embedder.stats();
    assert_eq!(stats.model_name, "test-model");
    assert!(!stats.model_loaded);
}

#[test]
fn test_text_hash_deterministic() {
    let hash1 = SmartEmbedder::text_hash("onnx", "m", "hello world");
    let hash2 = SmartEmbedder::text_hash("onnx", "m", "hello world");
    let hash3 = SmartEmbedder::text_hash("onnx", "m", "different text");

    assert_eq!(hash1, hash2);
    assert_ne!(hash1, hash3);
}

#[test]
fn test_stats_initial() {
    let embedder = SmartEmbedder::with_defaults();
    let stats = embedder.stats();

    assert_eq!(stats.cache_size, 0);
    assert_eq!(stats.cache_hits, 0);
    assert_eq!(stats.cache_misses, 0);
    assert_eq!(stats.coalesced_requests, 0);
    assert_eq!(stats.embeddings_computed, 0);
    assert!((stats.hit_rate() - 0.0).abs() < f64::EPSILON);
}

#[test]
fn test_clear_cache() {
    let embedder = SmartEmbedder::with_defaults();

    // Manually insert some cache entries
    embedder.cache.insert(
        1,
        CachedEmbedding {
            vector: vec![1.0, 2.0],
            created_at: Instant::now(),
        },
    );
    embedder.cache.insert(
        2,
        CachedEmbedding {
            vector: vec![3.0, 4.0],
            created_at: Instant::now(),
        },
    );

    assert_eq!(embedder.cache.len(), 2);
    embedder.clear_cache();
    assert_eq!(embedder.cache.len(), 0);
}

#[test]
fn test_hit_rate_calculation() {
    let stats = SmartEmbedderStats {
        cache_size: 10,
        cache_hits: 80,
        cache_misses: 20,
        coalesced_requests: 5,
        embeddings_computed: 20,
        model_loaded: true,
        model_name: "test".to_string(),
        dimensions: Some(384),
    };

    assert!((stats.hit_rate() - 0.8).abs() < 0.001);
}

#[test]
fn test_hit_rate_zero_requests() {
    let stats = SmartEmbedderStats {
        cache_size: 0,
        cache_hits: 0,
        cache_misses: 0,
        coalesced_requests: 0,
        embeddings_computed: 0,
        model_loaded: false,
        model_name: "test".to_string(),
        dimensions: None,
    };

    // Should return 0.0 when no requests (avoid division by zero)
    assert!((stats.hit_rate() - 0.0).abs() < 0.001);
}

#[test]
fn test_cache_key_uniqueness() {
    // Different queries should produce different hashes
    let hash1 = SmartEmbedder::text_hash("onnx", "m", "what is rust programming");
    let hash2 = SmartEmbedder::text_hash("onnx", "m", "what is rust");
    let hash3 = SmartEmbedder::text_hash("onnx", "m", "What Is Rust Programming"); // case different
    let hash4 = SmartEmbedder::text_hash("onnx", "m", "what is rust programming"); // same as hash1

    assert_ne!(hash1, hash2);
    assert_ne!(hash1, hash3); // case matters
    assert_eq!(hash1, hash4); // identical should match
}

#[test]
fn test_embedder_config_builder() {
    use std::time::Duration;

    let config = EmbedderConfig::new("all-MiniLM-L6-v2")
        .with_cache_max_entries(500)
        .with_cache_ttl(Duration::from_secs(7200))
        .with_warmup(true);

    assert_eq!(config.model_name, "all-MiniLM-L6-v2");
    assert_eq!(config.cache_max_entries, 500);
    assert_eq!(config.cache_ttl, Duration::from_secs(7200));
    assert!(config.warmup_on_start);
}

#[test]
fn test_embedder_config_defaults() {
    use std::time::Duration;

    let config = EmbedderConfig::default();

    // Check defaults are reasonable
    assert!(!config.model_name.is_empty());
    assert!(config.cache_max_entries > 0);
    assert!(config.cache_ttl > Duration::ZERO);
}

#[test]
fn test_stats_model_info() {
    let embedder = SmartEmbedder::new(EmbedderConfig::new("test-model"));
    let stats = embedder.stats();

    assert_eq!(stats.model_name, "test-model");
    assert!(!stats.model_loaded); // Not loaded until first embed
    assert!(stats.dimensions.is_none()); // Unknown until loaded
}

#[test]
fn test_cache_key_includes_provider_and_model() {
    let same_text = "hello world";
    let a = SmartEmbedder::text_hash("onnx", "model-a", same_text);
    let b = SmartEmbedder::text_hash("onnx", "model-b", same_text);
    let c = SmartEmbedder::text_hash("openai", "model-a", same_text);
    let d = SmartEmbedder::text_hash("onnx", "model-a", same_text);
    let e = SmartEmbedder::text_hash("onnx", "model-a", "other text");

    assert_ne!(a, b, "different models must not collide");
    assert_ne!(a, c, "different providers must not collide");
    assert_eq!(a, d, "identical identity must match");
    assert_ne!(a, e, "different text must not collide");
}

#[test]
fn test_instance_cache_key_matches_helper() {
    let mut cfg = EmbedderConfig::new("my-model");
    cfg.provider = "openai".into();
    let emb = SmartEmbedder::new(cfg);
    let via_inst = emb.cache_key("q");
    let via_helper = SmartEmbedder::text_hash("openai", "my-model", "q");
    assert_eq!(via_inst, via_helper);
}
