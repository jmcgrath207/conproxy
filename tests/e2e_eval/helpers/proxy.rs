use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Upstream backend addresses (Docker services).
const QDRANT_URL: &str = "http://localhost:6333";
const ELASTICSEARCH_URL: &str = "http://localhost:9200";

/// Elasticsearch index used for eval documents.
const ES_INDEX: &str = "conproxy_test";

/// RAII guard that manages a conproxy proxy process.
///
/// Starts the proxy in a temp directory with a config pointing to the Docker
/// backends. Kills the proxy process on drop.
pub struct ProxyGuard {
    child: Child,
    #[allow(dead_code)]
    work_dir: PathBuf,
}

impl ProxyGuard {
    /// Start the conproxy proxy configured to use the eval Docker backends.
    ///
    /// Creates a temporary `.conproxy/conproxy.toml` in a work directory,
    /// spawns the proxy as a foreground child process, and waits for it to
    /// start accepting connections.
    pub fn start(conproxy_bin: &Path, listen: &str) -> Self {
        let work_dir = PathBuf::from("/tmp/conproxy-eval-proxy");
        let _ = std::fs::remove_dir_all(&work_dir);
        let conproxy_dir = work_dir.join(".conproxy");
        std::fs::create_dir_all(&conproxy_dir).expect("Failed to create eval proxy work dir");

        // Write proxy config pointing to Docker backends
        let config = format!(
            r#"[server]
listen = "{listen}"

[upstreams.qdrant-vector]
url = "{QDRANT_URL}"
type = "qdrant"
index = "conproxy_test"
query_mode = "vector_only"
timeout_secs = 30

[upstreams.elasticsearch-fts]
url = "{ELASTICSEARCH_URL}"
type = "elasticsearch"
index = "conproxy_test"
search_fields = ["content", "title"]
timeout_secs = 30

[contexts.default]
default = true

[[contexts.default.upstreams]]
ref = "qdrant-vector"
priority = 0
weight = 2

[[contexts.default.upstreams]]
ref = "elasticsearch-fts"
priority = 1
weight = 1

[contexts.default.cache]
fresh_secs = 300
stale_secs = 600
max_entries = 2000

[contexts.default.cascade]
enabled = true
min_score_threshold = 0.3
min_results = 3
max_cascade_depth = 2
cascade_timeout_ms = 5000

[proxy.retry]
enabled = true
max_retries = 3
initial_delay_ms = 100
max_delay_ms = 5000
"#
        );

        std::fs::write(conproxy_dir.join("conproxy.toml"), &config)
            .expect("Failed to write eval proxy config");

        // Spawn proxy as a foreground child process
        let child = Command::new(conproxy_bin)
            .args(["start"])
            .current_dir(&work_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| {
                panic!(
                    "Failed to spawn conproxy at {}: {e}",
                    conproxy_bin.display()
                )
            });

        eprintln!("  Proxy: starting on {} (PID: {}) ...", listen, child.id());

        let mut guard = Self { child, work_dir };

        // Wait for proxy to accept connections
        if !guard.wait_ready(listen, Duration::from_secs(15)) {
            let _ = guard.child.kill();
            let _ = guard.child.wait();
            panic!("Proxy failed to start on {} within 15s", listen);
        }

        eprintln!("  Proxy: ready on {}", listen);
        guard
    }

    /// Return the PID of the proxy child process.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Poll until the proxy accepts TCP connections.
    fn wait_ready(&mut self, addr: &str, timeout: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            // Check if the child has already exited (crashed)
            if let Ok(Some(status)) = self.child.try_wait() {
                eprintln!("  Proxy exited prematurely with status: {}", status);
                // Dump stderr for debugging
                if let Some(ref mut stderr) = self.child.stderr {
                    use std::io::Read;
                    let mut buf = String::new();
                    let _ = stderr.read_to_string(&mut buf);
                    if !buf.is_empty() {
                        eprintln!("  Proxy stderr:\n{}", &buf[..buf.len().min(2000)]);
                    }
                }
                return false;
            }

            if std::net::TcpStream::connect(addr).is_ok() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        false
    }
}

impl Drop for ProxyGuard {
    fn drop(&mut self) {
        eprintln!("  Proxy: shutting down (PID: {}) ...", self.child.id());

        // Kill the process group on Unix (proxy may have spawned workers)
        #[cfg(unix)]
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGTERM);
        }

        // Give it enough time for DHAT atexit handler to flush dhat-heap.json
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
            }
        }
    }
}

/// Load eval documents into Elasticsearch so the proxy can find them.
///
/// Reads `eval_docs.json` and indexes each document into the ES index that the
/// proxy is configured to query. Uses the ES bulk API for efficiency.
pub fn load_eval_docs_into_elasticsearch(data_dir: &Path) {
    let docs_path = data_dir.join("eval_docs.json");
    let content = std::fs::read_to_string(&docs_path).unwrap_or_else(|e| {
        panic!("Failed to read {}: {e}", docs_path.display());
    });
    let json: serde_json::Value = serde_json::from_str(&content).unwrap_or_else(|e| {
        panic!("Failed to parse {}: {e}", docs_path.display());
    });

    let documents = json["documents"]
        .as_array()
        .expect("eval_docs.json must have a 'documents' array");

    // Build NDJSON bulk payload
    let mut bulk_body = String::new();
    for doc in documents {
        let id = doc["id"].as_str().unwrap();
        let action = serde_json::json!({"index": {"_id": id}});
        let body = serde_json::json!({
            "doc_id": id,
            "title": doc["title"],
            "content": doc["content"],
            "category": doc["category"],
            "tags": doc["tags"],
        });
        bulk_body.push_str(&serde_json::to_string(&action).unwrap());
        bulk_body.push('\n');
        bulk_body.push_str(&serde_json::to_string(&body).unwrap());
        bulk_body.push('\n');
    }

    let client = reqwest::blocking::Client::new();

    // Index documents via bulk API
    let bulk_url = format!("{}/{ES_INDEX}/_bulk", ELASTICSEARCH_URL);
    let resp = client
        .post(&bulk_url)
        .header("Content-Type", "application/x-ndjson")
        .body(bulk_body)
        .send()
        .unwrap_or_else(|e| panic!("Failed to bulk index eval docs into ES: {e}"));

    if !resp.status().is_success() {
        panic!(
            "ES bulk index returned {}: {}",
            resp.status(),
            resp.text().unwrap_or_default()
        );
    }

    // Refresh so docs are immediately searchable
    let refresh_url = format!("{}/{ES_INDEX}/_refresh", ELASTICSEARCH_URL);
    let refresh_resp = client.post(&refresh_url).send();
    if let Err(ref e) = refresh_resp {
        eprintln!("  WARNING: ES refresh failed: {}", e);
    }

    // Verify documents are searchable with a quick count query
    let count_url = format!("{}/{ES_INDEX}/_count", ELASTICSEARCH_URL);
    match client.get(&count_url).send() {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(body) = resp.json::<serde_json::Value>() {
                let count = body["count"].as_u64().unwrap_or(0);
                eprintln!(
                    "  Loaded {} eval docs into Elasticsearch ({}) — verified {} searchable",
                    documents.len(),
                    ES_INDEX,
                    count
                );
                if count == 0 {
                    panic!(
                        "ES reports 0 documents after bulk load + refresh! \
                         Documents were loaded but are not searchable."
                    );
                }
            }
        }
        Ok(resp) => {
            eprintln!(
                "  WARNING: ES count returned {}: {}",
                resp.status(),
                resp.text().unwrap_or_default()
            );
        }
        Err(e) => {
            eprintln!("  WARNING: ES count query failed: {}", e);
        }
    }
}

/// Load eval documents into Qdrant with real embeddings so the eval exercises
/// vector search and cascade.
///
/// Feature-gated: requires `embedder` feature for embedding generation.
/// Without the feature, prints a warning and returns (vector seeds will
/// cascade to ES).
#[cfg(feature = "embed")]
pub fn load_eval_docs_into_qdrant(data_dir: &Path) {
    use conproxy::embedding::embedder::Embedder;
    use conproxy::embedding::models::ModelManager;

    const QDRANT_COLLECTION: &str = "conproxy_test";

    let model_name = "all-MiniLM-L6-v2";

    if !ModelManager::is_installed(model_name) {
        eprintln!(
            "  WARNING: Model '{}' not installed — skipping Qdrant data loading.",
            model_name
        );
        eprintln!(
            "  Place model files under ~/.conproxy/models/{}/",
            model_name
        );
        return;
    }

    let docs_path = data_dir.join("eval_docs.json");
    let content = std::fs::read_to_string(&docs_path).unwrap_or_else(|e| {
        panic!("Failed to read {}: {e}", docs_path.display());
    });
    let json: serde_json::Value = serde_json::from_str(&content).unwrap_or_else(|e| {
        panic!("Failed to parse {}: {e}", docs_path.display());
    });

    let documents = json["documents"]
        .as_array()
        .expect("eval_docs.json must have a 'documents' array");

    // Build texts for embedding: "{title} {content}"
    let texts: Vec<String> = documents
        .iter()
        .map(|doc| {
            format!(
                "{} {}",
                doc["title"].as_str().unwrap_or(""),
                doc["content"].as_str().unwrap_or("")
            )
        })
        .collect();

    let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();

    // Generate embeddings
    let model_path = ModelManager::model_path(model_name);
    let tokenizer_path = ModelManager::tokenizer_path(model_name);
    let embedder =
        Embedder::new(&model_path, &tokenizer_path).expect("Failed to load embedding model");

    eprintln!(
        "  Qdrant: embedding {} documents with '{}'...",
        documents.len(),
        model_name
    );
    let embeddings = embedder
        .embed_batch(&text_refs)
        .expect("Failed to generate embeddings");

    let dims = embedder.dimensions();

    let client = reqwest::blocking::Client::new();

    // Delete existing collection (ignore errors — may not exist)
    let delete_url = format!("{}/collections/{}", QDRANT_URL, QDRANT_COLLECTION);
    let _ = client.delete(&delete_url).send();

    // Create collection with correct dimensions and cosine distance
    let create_url = format!("{}/collections/{}", QDRANT_URL, QDRANT_COLLECTION);
    let create_body = serde_json::json!({
        "vectors": {
            "size": dims,
            "distance": "Cosine"
        }
    });
    let resp = client
        .put(&create_url)
        .json(&create_body)
        .send()
        .unwrap_or_else(|e| panic!("Failed to create Qdrant collection: {e}"));

    if !resp.status().is_success() {
        panic!(
            "Qdrant collection creation returned {}: {}",
            resp.status(),
            resp.text().unwrap_or_default()
        );
    }

    // Build points for upsert
    let points: Vec<serde_json::Value> = documents
        .iter()
        .zip(embeddings.iter())
        .enumerate()
        .map(|(i, (doc, vector))| {
            serde_json::json!({
                "id": i + 1,
                "vector": vector,
                "payload": {
                    "doc_id": doc["id"],
                    "title": doc["title"],
                    "content": doc["content"],
                    "category": doc["category"],
                    "tags": doc["tags"],
                }
            })
        })
        .collect();

    // Upsert all points
    let upsert_url = format!("{}/collections/{}/points", QDRANT_URL, QDRANT_COLLECTION);
    let upsert_body = serde_json::json!({ "points": points });
    let resp = client
        .put(&upsert_url)
        .json(&upsert_body)
        .query(&[("wait", "true")])
        .send()
        .unwrap_or_else(|e| panic!("Failed to upsert points into Qdrant: {e}"));

    if !resp.status().is_success() {
        panic!(
            "Qdrant upsert returned {}: {}",
            resp.status(),
            resp.text().unwrap_or_default()
        );
    }

    // Brief pause for indexing, then verify
    std::thread::sleep(Duration::from_secs(1));

    let info_url = format!("{}/collections/{}", QDRANT_URL, QDRANT_COLLECTION);
    match client.get(&info_url).send() {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(body) = resp.json::<serde_json::Value>() {
                let count = body["result"]["points_count"].as_u64().unwrap_or(0);
                eprintln!(
                    "  Loaded {} eval docs into Qdrant ({}) — verified {} points, {} dims",
                    documents.len(),
                    QDRANT_COLLECTION,
                    count,
                    dims
                );
                if count == 0 {
                    panic!(
                        "Qdrant reports 0 points after upsert! \
                         Documents were loaded but not indexed."
                    );
                }
            }
        }
        Ok(resp) => {
            eprintln!(
                "  WARNING: Qdrant info returned {}: {}",
                resp.status(),
                resp.text().unwrap_or_default()
            );
        }
        Err(e) => {
            eprintln!("  WARNING: Qdrant info query failed: {}", e);
        }
    }
}

/// Stub when embedder feature is not available — warns and returns.
#[cfg(not(feature = "embed"))]
pub fn load_eval_docs_into_qdrant(_data_dir: &Path) {
    eprintln!("  WARNING: Qdrant data loading skipped (requires 'embedder' feature).");
    eprintln!("  Build with: cargo test --test e2e_eval --features proxy-embed");
}
