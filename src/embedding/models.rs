//! ONNX model path helpers.
//!
//! Resolves conventional install paths under `~/.conproxy/models/<name>/`
//! (`model.onnx` + `tokenizer.json`). No catalog or download CLI — place
//! files manually from HuggingFace or elsewhere.

use crate::config::Config;
use std::path::PathBuf;

/// Unit struct for model path helpers — all methods are associated functions.
pub struct ModelManager;

impl ModelManager {
    /// Root directory for all models (`~/.conproxy/models/`).
    pub fn models_dir() -> PathBuf {
        Config::global_models_dir()
    }

    /// Directory for a specific model (`~/.conproxy/models/<name>/`).
    pub fn model_dir(name: &str) -> PathBuf {
        Self::models_dir().join(name)
    }

    /// Path to the ONNX model file for a given model.
    pub fn model_path(name: &str) -> PathBuf {
        Self::model_dir(name).join("model.onnx")
    }

    /// Path to the tokenizer file for a given model.
    pub fn tokenizer_path(name: &str) -> PathBuf {
        Self::model_dir(name).join("tokenizer.json")
    }

    /// Check whether a model is installed (both model.onnx and tokenizer.json exist).
    pub fn is_installed(name: &str) -> bool {
        Self::is_installed_in(name, &Self::models_dir())
    }

    /// Check whether a model is installed under a given base directory.
    pub fn is_installed_in(name: &str, base: &std::path::Path) -> bool {
        base.join(name).join("model.onnx").exists()
            && base.join(name).join("tokenizer.json").exists()
    }
}

#[cfg(test)]
#[path = "tests/models_tests.rs"]
mod tests;
