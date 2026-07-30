use super::*;
use ndarray::Array3;

#[test]
fn test_mean_pool_simple() {
    // 1x3x2 tensor, all-ones mask -> mean of tokens
    let data = Array3::from_shape_vec((1, 3, 2), vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]).unwrap();
    let mask = Array2::from_shape_vec((1, 3), vec![1i64, 1, 1]).unwrap();

    let result = mean_pool(&data.view(), &mask);

    assert_eq!(result.shape(), &[1, 2]);
    // Mean of [1,3,5] = 3.0, mean of [2,4,6] = 4.0
    assert!((result[[0, 0]] - 3.0).abs() < 1e-6);
    assert!((result[[0, 1]] - 4.0).abs() < 1e-6);
}

#[test]
fn test_mean_pool_with_mask() {
    // 1x3x2 tensor, mask=[1,1,0] -> mean of first 2 tokens only
    let data = Array3::from_shape_vec((1, 3, 2), vec![1.0, 2.0, 3.0, 4.0, 100.0, 200.0]).unwrap();
    let mask = Array2::from_shape_vec((1, 3), vec![1i64, 1, 0]).unwrap();

    let result = mean_pool(&data.view(), &mask);

    assert_eq!(result.shape(), &[1, 2]);
    // Mean of [1,3] = 2.0, mean of [2,4] = 3.0 (100,200 masked out)
    assert!((result[[0, 0]] - 2.0).abs() < 1e-6);
    assert!((result[[0, 1]] - 3.0).abs() < 1e-6);
}

#[test]
fn test_mean_pool_batch() {
    // 2x3x2 tensor -> correct per-row means
    let data = Array3::from_shape_vec(
        (2, 3, 2),
        vec![
            1.0, 2.0, 3.0, 4.0, 5.0, 6.0, // batch 0
            10.0, 20.0, 30.0, 40.0, 50.0, 60.0, // batch 1
        ],
    )
    .unwrap();
    let mask = Array2::from_shape_vec((2, 3), vec![1i64, 1, 1, 1, 1, 1]).unwrap();

    let result = mean_pool(&data.view(), &mask);

    assert_eq!(result.shape(), &[2, 2]);
    assert!((result[[0, 0]] - 3.0).abs() < 1e-6);
    assert!((result[[0, 1]] - 4.0).abs() < 1e-6);
    assert!((result[[1, 0]] - 30.0).abs() < 1e-6);
    assert!((result[[1, 1]] - 40.0).abs() < 1e-6);
}

#[test]
fn test_normalize_unit_vector() {
    let mut v = vec![1.0, 0.0, 0.0];
    normalize(&mut v);
    assert!((v[0] - 1.0).abs() < 1e-6);
    assert!((v[1]).abs() < 1e-6);
    assert!((v[2]).abs() < 1e-6);
}

#[test]
fn test_normalize_zero_vector() {
    let mut v = vec![0.0, 0.0, 0.0];
    normalize(&mut v);
    assert!(v.iter().all(|x| *x == 0.0));
    assert!(v.iter().all(|x| !x.is_nan()));
}

#[test]
fn test_normalize_arbitrary() {
    let mut v = vec![3.0, 4.0];
    normalize(&mut v);
    assert!((v[0] - 0.6).abs() < 1e-6);
    assert!((v[1] - 0.8).abs() < 1e-6);

    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-6);
}

// ONNX-dependent tests: skip if model files are not present
fn default_model_path() -> std::path::PathBuf {
    crate::embedding::models::ModelManager::model_path("all-MiniLM-L6-v2")
}

fn default_tokenizer_path() -> std::path::PathBuf {
    crate::embedding::models::ModelManager::tokenizer_path("all-MiniLM-L6-v2")
}

fn model_available() -> bool {
    default_model_path().exists() && default_tokenizer_path().exists()
}

#[test]
fn test_embed_returns_correct_dimensions() {
    if !model_available() {
        eprintln!("Skipping test_embed_returns_correct_dimensions: model not installed");
        return;
    }

    let embedder = Embedder::new(default_model_path(), default_tokenizer_path()).unwrap();
    let embedding = embedder.embed("Hello, world!").unwrap();

    assert_eq!(embedding.len(), embedder.dimensions());
    assert_eq!(embedding.len(), 384);
}

#[test]
fn test_embed_batch_matches_single() {
    if !model_available() {
        eprintln!("Skipping test_embed_batch_matches_single: model not installed");
        return;
    }

    let embedder = Embedder::new(default_model_path(), default_tokenizer_path()).unwrap();

    let texts = &["Hello world", "Rust programming"];
    let batch_results = embedder.embed_batch(texts).unwrap();
    let single_0 = embedder.embed(texts[0]).unwrap();
    let single_1 = embedder.embed(texts[1]).unwrap();

    assert_eq!(batch_results.len(), 2);
    assert_eq!(batch_results[0].len(), single_0.len());

    let tolerance = 0.01;
    for (a, b) in batch_results[0].iter().zip(single_0.iter()) {
        assert!(
            (a - b).abs() < tolerance,
            "Mismatch: batch={}, single={}, diff={}",
            a,
            b,
            (a - b).abs()
        );
    }
    for (a, b) in batch_results[1].iter().zip(single_1.iter()) {
        assert!(
            (a - b).abs() < tolerance,
            "Mismatch: batch={}, single={}, diff={}",
            a,
            b,
            (a - b).abs()
        );
    }
}

/// Cosine helper for the geometry test (embeddings are L2-normalized by
/// `embed`, so dot product = cosine).
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// Semantic-geometry regression: a paraphrase pair must score clearly higher
/// than an unrelated pair. Guards the ONNX input binding order — with
/// `attention_mask` / `token_type_ids` swapped, every pair collapses to
/// cosine ≈ 1 (found via hitrate_bench --probe, 2026-07).
#[test]
fn test_embed_semantic_geometry() {
    if !model_available() {
        eprintln!("Skipping test_embed_semantic_geometry: model not installed");
        return;
    }

    let embedder = Embedder::new(default_model_path(), default_tokenizer_path()).unwrap();

    let q1 = embedder.embed("how do I reset my password").unwrap();
    let q2 = embedder.embed("password reset instructions").unwrap();
    let unrelated = embedder.embed("best italian restaurants nearby").unwrap();

    let paraphrase_sim = cosine(&q1, &q2);
    let unrelated_sim = cosine(&q1, &unrelated);

    assert!(
        paraphrase_sim > unrelated_sim + 0.3,
        "geometry broken: paraphrase={paraphrase_sim:.3}, unrelated={unrelated_sim:.3} \
         (input order regression?)"
    );
    assert!(
        unrelated_sim < 0.5,
        "unrelated similarity too high: {unrelated_sim:.3} (collapsed embeddings?)"
    );
}
