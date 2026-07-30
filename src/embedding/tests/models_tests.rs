#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

#[test]
fn test_model_paths() {
    let dir = ModelManager::model_dir("test-model");
    assert!(dir.to_string_lossy().contains("models"));
    assert!(dir.to_string_lossy().contains("test-model"));
}

#[test]
fn test_model_path_structure() {
    let model = ModelManager::model_path("test-model");
    let tokenizer = ModelManager::tokenizer_path("test-model");
    let dir = ModelManager::model_dir("test-model");

    assert!(model.starts_with(&dir));
    assert!(tokenizer.starts_with(&dir));
    assert!(model.to_string_lossy().ends_with("model.onnx"));
    assert!(tokenizer.to_string_lossy().ends_with("tokenizer.json"));
}

#[test]
fn test_is_installed_check() {
    assert!(!ModelManager::is_installed(
        "definitely-not-a-real-model-xyz"
    ));
}

#[test]
fn test_is_installed_in_with_files() {
    let dir = tempfile::tempdir().unwrap();
    let model_dir = dir.path().join("test-model");
    std::fs::create_dir_all(&model_dir).unwrap();

    assert!(!ModelManager::is_installed_in("test-model", dir.path()));

    std::fs::write(model_dir.join("model.onnx"), b"fake").unwrap();
    assert!(!ModelManager::is_installed_in("test-model", dir.path()));

    std::fs::write(model_dir.join("tokenizer.json"), b"fake").unwrap();
    assert!(ModelManager::is_installed_in("test-model", dir.path()));
}

#[test]
fn test_is_installed_empty_dir() {
    let dir = tempfile::tempdir().unwrap();
    let nonexistent = dir.path().join("nonexistent");
    assert!(!ModelManager::is_installed_in("any-model", &nonexistent));
    assert!(!ModelManager::is_installed_in("any-model", dir.path()));
}
