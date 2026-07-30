use serde_json::Value;
use std::path::Path;

/// Generate a deterministic placeholder vector from a document ID.
///
/// Matches the Python logic in the old `load_qdrant.sh`:
///   h = sha256(doc_id).hexdigest()
///   vector = [int(h[i:i+2], 16) / 255 for i in range(0, len(h), 2)] * 12
pub fn placeholder_vector(doc_id: &str, dims: usize) -> Vec<f64> {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(doc_id.as_bytes());
    let hex: String = hash.as_slice().iter().map(|b| format!("{b:02x}")).collect();
    // Extract byte pairs from hex string
    let base: Vec<f64> = (0..hex.len())
        .step_by(2)
        .filter_map(|i| {
            hex.get(i..i + 2)
                .and_then(|s| u8::from_str_radix(s, 16).ok())
                .map(|b| f64::from(b) / 255.0)
        })
        .collect();

    // Repeat to fill dims
    base.iter().cycle().take(dims).copied().collect()
}

/// Generate a numeric ID from a document ID string (matching the old script's md5-based ID).
fn numeric_id_for(doc_id: &str) -> u64 {
    use sha2::{Digest, Sha256};
    // Use first 8 hex chars of SHA-256 as a numeric ID (close enough to md5 first-8-hex)
    let hash = Sha256::digest(doc_id.as_bytes());
    let hex: String = hash.as_slice().iter().map(|b| format!("{b:02x}")).collect();
    u64::from_str_radix(&hex[..8], 16).unwrap_or(0)
}

/// Load test documents into Qdrant.
///
/// If `embeddings_file` exists, uses pre-computed embeddings; otherwise generates
/// deterministic placeholder vectors.
pub fn load_docs_into_qdrant(
    url: &str,
    collection: &str,
    docs_file: &Path,
    embeddings_file: Option<&Path>,
    dims: usize,
) -> Result<usize, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    // Create collection (ignore if already exists)
    let create_url = format!("{url}/collections/{collection}");
    let _ = client
        .put(&create_url)
        .json(&serde_json::json!({
            "vectors": { "size": dims, "distance": "Cosine" }
        }))
        .send();

    eprintln!("Loading documents into Qdrant at {url}...");

    let has_embeddings = embeddings_file.map(|p| p.exists()).unwrap_or(false);

    let count = if has_embeddings {
        load_qdrant_with_embeddings(&client, url, collection, embeddings_file.unwrap())?
    } else {
        load_qdrant_with_placeholders(&client, url, collection, docs_file, dims)?
    };

    // Verify
    let info_url = format!("{url}/collections/{collection}");
    if let Ok(resp) = client.get(&info_url).send() {
        if let Ok(body) = resp.json::<Value>() {
            let points = body["result"]["points_count"].as_u64().unwrap_or(0);
            eprintln!("Loaded {points} documents into Qdrant collection: {collection}");
        }
    }

    Ok(count)
}

fn load_qdrant_with_embeddings(
    client: &reqwest::blocking::Client,
    url: &str,
    collection: &str,
    embeddings_file: &Path,
) -> Result<usize, String> {
    let content = std::fs::read_to_string(embeddings_file)
        .map_err(|e| format!("Failed to read embeddings file: {e}"))?;
    let entries: Vec<Value> =
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse embeddings: {e}"))?;

    let points_url = format!("{url}/collections/{collection}/points");
    let mut count = 0;

    for entry in &entries {
        let doc_id = entry["id"].as_str().unwrap_or("unknown");
        let vector = &entry["vector"];
        let payload = &entry["payload"];
        let nid = numeric_id_for(doc_id);

        let point = serde_json::json!({
            "points": [{
                "id": nid,
                "vector": vector,
                "payload": payload
            }]
        });

        let resp = client
            .put(&points_url)
            .json(&point)
            .send()
            .map_err(|e| format!("Qdrant point upload failed: {e}"))?;

        if !resp.status().is_success() {
            eprintln!("  WARNING: Failed to load {doc_id}: {}", resp.status());
        } else {
            count += 1;
        }
    }

    Ok(count)
}

fn load_qdrant_with_placeholders(
    client: &reqwest::blocking::Client,
    url: &str,
    collection: &str,
    docs_file: &Path,
    dims: usize,
) -> Result<usize, String> {
    eprintln!("WARNING: embeddings.json not found. Creating placeholder vectors...");

    let content =
        std::fs::read_to_string(docs_file).map_err(|e| format!("Failed to read docs file: {e}"))?;
    let json: Value =
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse docs: {e}"))?;
    let documents = json["documents"]
        .as_array()
        .ok_or("docs file must have a 'documents' array")?;

    let points_url = format!("{url}/collections/{collection}/points");
    let mut count = 0;

    for doc in documents {
        let doc_id = doc["id"].as_str().unwrap_or("unknown");
        let title = doc["title"].as_str().unwrap_or("");
        let content_text = doc["content"].as_str().unwrap_or("");
        let category = doc["category"].as_str().unwrap_or("");
        let tags = &doc["tags"];

        let vector = placeholder_vector(doc_id, dims);
        let nid = numeric_id_for(doc_id);

        let point = serde_json::json!({
            "points": [{
                "id": nid,
                "vector": vector,
                "payload": {
                    "doc_id": doc_id,
                    "title": title,
                    "content": content_text,
                    "category": category,
                    "tags": tags
                }
            }]
        });

        eprintln!("  Loading: {doc_id} - {title}");

        let resp = client
            .put(&points_url)
            .json(&point)
            .send()
            .map_err(|e| format!("Qdrant point upload failed: {e}"))?;

        if !resp.status().is_success() {
            eprintln!("  WARNING: Failed to load {doc_id}: {}", resp.status());
        } else {
            count += 1;
        }
    }

    Ok(count)
}

/// Load test documents into Elasticsearch.
pub fn load_docs_into_elasticsearch(
    url: &str,
    index: &str,
    docs_file: &Path,
) -> Result<usize, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    eprintln!("Loading documents into Elasticsearch at {url}...");

    // Create index with mappings (ignore if exists)
    let index_url = format!("{url}/{index}");
    let _ = client
        .put(&index_url)
        .json(&serde_json::json!({
            "settings": {
                "number_of_shards": 1,
                "number_of_replicas": 0,
                "analysis": {
                    "analyzer": {
                        "default": { "type": "standard" }
                    }
                }
            },
            "mappings": {
                "properties": {
                    "doc_id": { "type": "keyword" },
                    "title": { "type": "text", "analyzer": "standard" },
                    "content": { "type": "text", "analyzer": "standard" },
                    "category": { "type": "keyword" },
                    "tags": { "type": "keyword" }
                }
            }
        }))
        .send();

    // Load documents
    let content =
        std::fs::read_to_string(docs_file).map_err(|e| format!("Failed to read docs file: {e}"))?;
    let json: Value =
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse docs: {e}"))?;
    let documents = json["documents"]
        .as_array()
        .ok_or("docs file must have a 'documents' array")?;

    // Build NDJSON bulk payload
    let mut bulk_body = String::new();
    for doc in documents {
        let doc_id = doc["id"].as_str().unwrap_or("unknown");
        let action = serde_json::json!({"index": {"_id": doc_id}});
        let body = serde_json::json!({
            "doc_id": doc_id,
            "title": doc["title"],
            "content": doc["content"],
            "category": doc["category"],
            "tags": doc["tags"]
        });
        bulk_body.push_str(&serde_json::to_string(&action).unwrap());
        bulk_body.push('\n');
        bulk_body.push_str(&serde_json::to_string(&body).unwrap());
        bulk_body.push('\n');
    }

    // Send bulk request
    let bulk_url = format!("{url}/{index}/_bulk");
    let resp = client
        .post(&bulk_url)
        .header("Content-Type", "application/x-ndjson")
        .body(bulk_body)
        .send()
        .map_err(|e| format!("ES bulk load failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!(
            "ES bulk index returned {}: {}",
            resp.status(),
            resp.text().unwrap_or_default()
        ));
    }

    // Refresh index
    let refresh_url = format!("{url}/{index}/_refresh");
    let _ = client.post(&refresh_url).send();

    // Verify
    let count_url = format!("{url}/{index}/_count");
    let count = match client.get(&count_url).send() {
        Ok(resp) if resp.status().is_success() => resp
            .json::<Value>()
            .ok()
            .and_then(|v| v["count"].as_u64())
            .unwrap_or(0) as usize,
        _ => documents.len(),
    };

    eprintln!("Loaded {count} documents into Elasticsearch index: {index}");
    Ok(count)
}
