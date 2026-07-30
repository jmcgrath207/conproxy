//! Generate embeddings for test documents and queries.
//!
//! Replaces `tests/e2e/scripts/generate_embeddings.py`.
//!
//! Requires: `cargo run --bin generate_embeddings --features embedder`
//!
//! Uses the all-MiniLM-L6-v2 model (384 dimensions) via the conproxy Embedder.

#[cfg(not(feature = "embed"))]
fn main() {
    eprintln!("ERROR: This binary requires the 'embedder' feature.");
    eprintln!("Build with: cargo run --bin generate_embeddings --features embedder");
    std::process::exit(1);
}

#[cfg(feature = "embed")]
#[allow(clippy::expect_used)]
fn main() {
    use conproxy::embedding::embedder::Embedder;
    use conproxy::embedding::models::ModelManager;

    let model_name = "all-MiniLM-L6-v2";
    let data_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("e2e")
        .join("data");

    // Check model is installed
    if !ModelManager::is_installed(model_name) {
        eprintln!("ERROR: Model '{model_name}' not installed.");
        eprintln!("Place model files under ~/.conproxy/models/{model_name}/");
        std::process::exit(1);
    }

    eprintln!("Loading model: {model_name}");
    let model_path = ModelManager::model_path(model_name);
    let tokenizer_path = ModelManager::tokenizer_path(model_name);
    let embedder = Embedder::new(&model_path, &tokenizer_path).expect("Failed to load embedder");

    // --- Document embeddings ---
    let docs_path = data_dir.join("sample_docs.json");
    let docs_content = std::fs::read_to_string(&docs_path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", docs_path.display()));
    let docs_json: serde_json::Value = serde_json::from_str(&docs_content)
        .unwrap_or_else(|e| panic!("Failed to parse {}: {e}", docs_path.display()));
    let documents = docs_json["documents"]
        .as_array()
        .expect("Expected 'documents' array");

    eprintln!("Generating embeddings for {} documents...", documents.len());
    let mut doc_embeddings: Vec<serde_json::Value> = Vec::new();

    for doc in documents {
        let id = doc["id"].as_str().unwrap_or("unknown");
        let text = format!(
            "{} {}",
            doc["title"].as_str().unwrap_or(""),
            doc["content"].as_str().unwrap_or("")
        );

        let embedding = embedder.embed(&text).expect("Embedding failed");
        let vector: Vec<f64> = embedding.iter().map(|&v| f64::from(v)).collect();

        doc_embeddings.push(serde_json::json!({
            "id": id,
            "vector": vector,
            "payload": {
                "doc_id": id,
                "title": doc["title"],
                "content": doc["content"],
                "category": doc["category"],
                "tags": doc["tags"]
            }
        }));
    }

    let embeddings_path = data_dir.join("embeddings.json");
    let json_str = serde_json::to_string_pretty(&doc_embeddings).expect("serialize doc embeddings");
    std::fs::write(&embeddings_path, json_str)
        .unwrap_or_else(|e| panic!("Failed to write {}: {e}", embeddings_path.display()));
    eprintln!(
        "Saved document embeddings to: {}",
        embeddings_path.display()
    );

    // --- Query embeddings ---
    let queries_path = data_dir.join("queries.json");
    let queries_content = std::fs::read_to_string(&queries_path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", queries_path.display()));
    let queries_json: serde_json::Value = serde_json::from_str(&queries_content)
        .unwrap_or_else(|e| panic!("Failed to parse {}: {e}", queries_path.display()));
    let queries = queries_json["queries"]
        .as_array()
        .expect("Expected 'queries' array");

    eprintln!("Generating embeddings for {} queries...", queries.len());
    let mut query_embeddings: Vec<serde_json::Value> = Vec::new();

    for query in queries {
        let id = query["id"].as_str().unwrap_or("unknown");
        let text = query["query"].as_str().unwrap_or("");
        let qtype = query["type"].as_str().unwrap_or("");

        let embedding = embedder.embed(text).expect("Embedding failed");
        let vector: Vec<f64> = embedding.iter().map(|&v| f64::from(v)).collect();

        query_embeddings.push(serde_json::json!({
            "id": id,
            "query": text,
            "type": qtype,
            "vector": vector,
            "expected_doc_ids": query["expected_doc_ids"]
        }));
    }

    let query_embeddings_path = data_dir.join("query_embeddings.json");
    let json_str =
        serde_json::to_string_pretty(&query_embeddings).expect("serialize query embeddings");
    std::fs::write(&query_embeddings_path, json_str)
        .unwrap_or_else(|e| panic!("Failed to write {}: {e}", query_embeddings_path.display()));
    eprintln!(
        "Saved query embeddings to: {}",
        query_embeddings_path.display()
    );

    // --- Seed embeddings ---
    eprintln!("Generating seed embeddings...");
    for seed_file in &["seeds_fts.txt", "seeds_semantic.txt"] {
        let seed_path = data_dir.join(seed_file);
        if !seed_path.exists() {
            continue;
        }

        let content = std::fs::read_to_string(&seed_path)
            .unwrap_or_else(|e| panic!("Failed to read {}: {e}", seed_path.display()));
        let seeds: Vec<&str> = content
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();

        let mut seed_embeddings: Vec<serde_json::Value> = Vec::new();
        for seed in &seeds {
            let embedding = embedder.embed(seed).expect("Embedding failed");
            let vector: Vec<f64> = embedding.iter().map(|&v| f64::from(v)).collect();
            seed_embeddings.push(serde_json::json!({
                "query": seed,
                "vector": vector
            }));
        }

        let output_name = seed_file.replace(".txt", "_embeddings.json");
        let output_path = data_dir.join(&output_name);
        let json_str =
            serde_json::to_string_pretty(&seed_embeddings).expect("serialize seed embeddings");
        std::fs::write(&output_path, json_str)
            .unwrap_or_else(|e| panic!("Failed to write {}: {e}", output_path.display()));
        eprintln!("Saved seed embeddings to: {}", output_path.display());
    }

    eprintln!("Done!");
}
