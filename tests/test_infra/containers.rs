//! Testcontainers-based service containers for integration tests.
//!
//! Each function starts a container, waits for readiness via HTTP polling,
//! and returns a `ContainerInstance` keeping the container alive for the
//! test duration (drop = cleanup).

use std::time::Duration;
use testcontainers::{runners::AsyncRunner, ContainerAsync, GenericImage, ImageExt};

/// Holds a running container and its host-mapped base URL.
/// Container is stopped and removed on drop.
pub struct ContainerInstance {
    container: ContainerAsync<GenericImage>,
    /// Container-internal listen port (for host-port remap after restart).
    internal_port: u16,
    pub base_url: String,
}

impl ContainerInstance {
    fn new(container: ContainerAsync<GenericImage>, internal_port: u16, base_url: String) -> Self {
        Self {
            container,
            internal_port,
            base_url,
        }
    }

    /// Stop container (SIGTERM then timeout). Keeps container id for later `start`.
    pub async fn stop(&self) {
        self.container
            .stop_with_timeout(Some(5))
            .await
            .expect("container stop");
    }

    /// Start a previously stopped container; re-resolve host port; poll `path` until ready.
    pub async fn start(&mut self, ready_path: &str) {
        self.container.start().await.expect("container start");
        let port = self
            .container
            .get_host_port_ipv4(self.internal_port)
            .await
            .expect("host port after restart");
        self.base_url = format!("http://localhost:{port}");
        wait_for_http(&self.base_url, ready_path, Duration::from_secs(90)).await;
    }
}

/// Quick Docker-daemon availability check. Panics with a hard error if
/// Docker is not running — integration tests cannot proceed without it.
pub fn docker_check() {
    let output = std::process::Command::new("docker")
        .arg("info")
        .output()
        .unwrap_or_else(|e| panic!("Failed to execute `docker info`: {e}"));
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!("Docker daemon unavailable — integration tests require Docker:\n{stderr}");
    }
}

/// Poll an HTTP endpoint until success (200/2xx) or timeout.
pub async fn wait_for_http(base_url: &str, path: &str, timeout: Duration) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .expect("reqwest client build");
    let url = format!("{base_url}{path}");
    let deadline = tokio::time::Instant::now() + timeout;

    while tokio::time::Instant::now() < deadline {
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => return,
            _ => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    panic!("Timeout waiting for {url} (waited {timeout:?})");
}

// ---------------------------------------------------------------------------
// Qdrant
// ---------------------------------------------------------------------------

/// Start a Qdrant container and return the mapped base URL.
pub async fn qdrant_container() -> ContainerInstance {
    let container: ContainerAsync<GenericImage> = GenericImage::new("qdrant/qdrant", "v1.13.2")
        .with_exposed_port(6333.into())
        .start()
        .await
        .expect("Qdrant container failed to start");
    let port = container
        .get_host_port_ipv4(6333)
        .await
        .expect("Qdrant port mapping");
    let base_url = format!("http://localhost:{port}");
    wait_for_http(&base_url, "/readyz", Duration::from_secs(60)).await;
    ContainerInstance::new(container, 6333, base_url)
}

/// Create a Qdrant collection with the given name and vector dimension.
pub async fn qdrant_create_collection(base_url: &str, name: &str, dims: usize) {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "name": name,
        "vectors": {
            "size": dims,
            "distance": "Cosine"
        }
    });
    let resp = client
        .put(format!("{base_url}/collections/{name}"))
        .json(&body)
        .send()
        .await
        .expect("Qdrant create_collection request");
    assert!(
        resp.status().is_success(),
        "Qdrant create_collection failed: {}",
        resp.text().await.unwrap_or_default()
    );
}

/// Insert points into a Qdrant collection.
#[allow(clippy::implicit_hasher)]
pub async fn qdrant_insert_points(
    base_url: &str,
    collection: &str,
    points: Vec<serde_json::Value>,
) {
    let client = reqwest::Client::new();
    let body = serde_json::json!({ "points": points });
    let resp = client
        .put(format!(
            "{base_url}/collections/{collection}/points?wait=true"
        ))
        .json(&body)
        .send()
        .await
        .expect("Qdrant upsert request");
    assert!(
        resp.status().is_success(),
        "Qdrant upsert failed: {}",
        resp.text().await.unwrap_or_default()
    );
}

// ---------------------------------------------------------------------------
// Elasticsearch
// ---------------------------------------------------------------------------

/// Start an Elasticsearch container.
pub async fn elasticsearch_container() -> ContainerInstance {
    let container: ContainerAsync<GenericImage> =
        GenericImage::new("docker.elastic.co/elasticsearch/elasticsearch", "8.17.0")
            .with_exposed_port(9200.into())
            .with_env_var("discovery.type", "single-node")
            .with_env_var("xpack.security.enabled", "false")
            .with_env_var("ES_JAVA_OPTS", "-Xms512m -Xmx512m")
            .start()
            .await
            .expect("Elasticsearch container failed to start");
    let port = container
        .get_host_port_ipv4(9200)
        .await
        .expect("ES port mapping");
    let base_url = format!("http://localhost:{port}");
    wait_for_http(&base_url, "/_cluster/health", Duration::from_secs(120)).await;
    ContainerInstance::new(container, 9200, base_url)
}

/// Create an Elasticsearch index with the given mapping.
pub async fn es_create_index(base_url: &str, name: &str, fields: &[&str]) {
    let client = reqwest::Client::new();
    let properties: serde_json::Value = fields
        .iter()
        .map(|f| (f.to_string(), serde_json::json!({ "type": "text" })))
        .collect();
    let body = serde_json::json!({
        "mappings": { "properties": properties }
    });
    let resp = client
        .put(format!("{base_url}/{name}"))
        .json(&body)
        .send()
        .await
        .expect("ES create_index request");
    assert!(
        resp.status().is_success(),
        "ES create_index failed (HTTP {}): {}",
        resp.status().as_u16(),
        resp.text().await.unwrap_or_default()
    );
}

/// Index documents into Elasticsearch and refresh.
pub async fn es_index_docs(base_url: &str, index: &str, docs: Vec<serde_json::Value>) {
    let client = reqwest::Client::new();
    let mut body = String::new();
    for doc in &docs {
        body.push_str(
            &serde_json::to_string(&serde_json::json!({ "index": { "_index": index } })).unwrap(),
        );
        body.push('\n');
        body.push_str(&serde_json::to_string(doc).unwrap());
        body.push('\n');
    }
    let resp = client
        .post(format!("{base_url}/_bulk"))
        .header("Content-Type", "application/x-ndjson")
        .body(body)
        .send()
        .await
        .expect("ES bulk request");
    assert!(
        resp.status().is_success(),
        "ES bulk failed: {}",
        resp.text().await.unwrap_or_default()
    );
    let resp = client
        .post(format!("{base_url}/{index}/_refresh"))
        .send()
        .await
        .expect("ES refresh request");
    assert!(resp.status().is_success(), "ES refresh failed");
}

// ---------------------------------------------------------------------------
// OpenSearch (ES-compatible API — same adapter)
// ---------------------------------------------------------------------------

/// Start an OpenSearch container (security plugin disabled for tests).
pub async fn opensearch_container() -> ContainerInstance {
    let container: ContainerAsync<GenericImage> =
        GenericImage::new("opensearchproject/opensearch", "2.18.0")
            .with_exposed_port(9200.into())
            .with_env_var("discovery.type", "single-node")
            .with_env_var("DISABLE_SECURITY_PLUGIN", "true")
            .with_env_var("DISABLE_INSTALL_DEMO_CONFIG", "true")
            .with_env_var("OPENSEARCH_JAVA_OPTS", "-Xms512m -Xmx512m")
            .start()
            .await
            .expect("OpenSearch container failed to start");
    let port = container
        .get_host_port_ipv4(9200)
        .await
        .expect("OpenSearch port mapping");
    let base_url = format!("http://localhost:{port}");
    wait_for_http(&base_url, "/_cluster/health", Duration::from_secs(120)).await;
    ContainerInstance::new(container, 9200, base_url)
}

// ---------------------------------------------------------------------------
// pgvector — cfg-gated to `pgvector` feature (requires tokio-postgres)
// ---------------------------------------------------------------------------

/// Start a pgvector container.
#[cfg(feature = "pgvector")]
pub async fn pgvector_container() -> ContainerInstance {
    let container: ContainerAsync<GenericImage> = GenericImage::new("pgvector/pgvector", "pg16")
        .with_exposed_port(5432.into())
        .with_env_var("POSTGRES_PASSWORD", "test")
        .with_env_var("POSTGRES_DB", "test")
        .start()
        .await
        .expect("pgvector container failed to start");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("pgvector port mapping");
    let base_url = format!("localhost:{port}");
    wait_for_pg(&base_url, Duration::from_secs(30)).await;
    ContainerInstance::new(container, 5432, base_url)
}

/// Poll PostgreSQL via raw TCP until SELECT 1 succeeds.
#[cfg(feature = "pgvector")]
async fn wait_for_pg(host_port: &str, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    let url = format!("postgresql://postgres:test@{host_port}/test");

    while tokio::time::Instant::now() < deadline {
        if let Ok((client, connection)) = tokio_postgres::connect(&url, tokio_postgres::NoTls).await
        {
            drop(client);
            drop(connection);
            tokio::time::sleep(Duration::from_millis(500)).await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("Timeout waiting for pgvector at {host_port} (waited {timeout:?})");
}

/// Execute SQL on pgvector (CREATE EXTENSION, CREATE TABLE, etc.).
#[cfg(feature = "pgvector")]
pub async fn pgv_execute(host_port: &str, sql: &str) {
    let url = format!("postgresql://postgres:test@{host_port}/test");
    let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
        .await
        .unwrap_or_else(|e| panic!("pgv_execute connect: {e}"));
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
        .batch_execute(sql)
        .await
        .unwrap_or_else(|e| panic!("pgv_execute: {e}"));
}

/// Insert a row into a pgvector table.
#[cfg(feature = "pgvector")]
pub async fn pgv_insert(host_port: &str, table: &str, content: &str, embedding: &[f32]) {
    let vector_text = conproxy::proxy::pgvector::format_vector(embedding);
    // Interpolate vector_text directly so toko-postgres doesn't
    // need to serialize a String for the pgvector `vector` cast.
    let sql =
        format!("INSERT INTO {table} (content, embedding) VALUES ($1, '{vector_text}'::vector)");
    let url = format!("postgresql://postgres:test@{host_port}/test");
    let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
        .await
        .expect("pgv_insert connect");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
        .execute(&sql, &[&content])
        .await
        .unwrap_or_else(|e| panic!("pgv_insert execute: {e}"));
}

// ---------------------------------------------------------------------------
// Milvus
// ---------------------------------------------------------------------------

/// Start Milvus standalone (gRPC/REST :19530, health :9091).
pub async fn milvus_container() -> ContainerInstance {
    let container: ContainerAsync<GenericImage> = GenericImage::new("milvusdb/milvus", "v2.4.15")
        .with_exposed_port(19530.into())
        .with_exposed_port(9091.into())
        .with_env_var("ETCD_USE_EMBED", "true")
        .with_env_var("COMMON_STORAGETYPE", "local")
        .with_cmd(vec!["milvus", "run", "standalone"])
        .start()
        .await
        .expect("Milvus container failed to start");
    let api_port = container
        .get_host_port_ipv4(19530)
        .await
        .expect("Milvus API port mapping");
    let health_port = container
        .get_host_port_ipv4(9091)
        .await
        .expect("Milvus health port mapping");
    // Health is on 9091; API base is 19530.
    let health_url = format!("http://localhost:{health_port}");
    wait_for_http(&health_url, "/healthz", Duration::from_secs(180)).await;
    let base_url = format!("http://localhost:{api_port}");
    ContainerInstance::new(container, 19530, base_url)
}

/// Create a Milvus collection (float vector + content varchar) and load it.
pub async fn milvus_create_collection(base_url: &str, name: &str, dims: usize) {
    let client = reqwest::Client::new();
    let create = serde_json::json!({
        "collectionName": name,
        "schema": {
            "autoId": false,
            "enableDynamicField": false,
            "fields": [
                {
                    "fieldName": "id",
                    "dataType": "Int64",
                    "isPrimary": true
                },
                {
                    "fieldName": "vector",
                    "dataType": "FloatVector",
                    "elementTypeParams": { "dim": dims.to_string() }
                },
                {
                    "fieldName": "content",
                    "dataType": "VarChar",
                    "elementTypeParams": { "max_length": "512" }
                }
            ]
        }
    });
    let resp = client
        .post(format!("{base_url}/v2/vectordb/collections/create"))
        .json(&create)
        .send()
        .await
        .expect("Milvus create_collection request");
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    assert!(
        status.is_success(),
        "Milvus create_collection failed (HTTP {status}): {text}"
    );

    // Index required before load/search on standalone.
    let index = serde_json::json!({
        "collectionName": name,
        "indexParams": [{
            "fieldName": "vector",
            "indexName": "vector_idx",
            "metricType": "COSINE",
            "indexType": "AUTOINDEX"
        }]
    });
    let resp = client
        .post(format!("{base_url}/v2/vectordb/indexes/create"))
        .json(&index)
        .send()
        .await
        .expect("Milvus create_index request");
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    assert!(
        status.is_success(),
        "Milvus create_index failed (HTTP {status}): {text}"
    );

    let load = serde_json::json!({ "collectionName": name });
    let resp = client
        .post(format!("{base_url}/v2/vectordb/collections/load"))
        .json(&load)
        .send()
        .await
        .expect("Milvus load_collection request");
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    assert!(
        status.is_success(),
        "Milvus load_collection failed (HTTP {status}): {text}"
    );
    // Brief wait for load to settle.
    tokio::time::sleep(Duration::from_secs(2)).await;
}

/// Insert points into a Milvus collection (id, vector, content).
pub async fn milvus_insert(base_url: &str, name: &str, rows: Vec<(i64, Vec<f32>, String)>) {
    let client = reqwest::Client::new();
    let data: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(id, vector, content)| {
            serde_json::json!({
                "id": id,
                "vector": vector,
                "content": content,
            })
        })
        .collect();
    let body = serde_json::json!({
        "collectionName": name,
        "data": data,
    });
    let resp = client
        .post(format!("{base_url}/v2/vectordb/entities/insert"))
        .json(&body)
        .send()
        .await
        .expect("Milvus insert request");
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    assert!(
        status.is_success(),
        "Milvus insert failed (HTTP {status}): {text}"
    );
    // Flush so search sees data immediately.
    let flush = serde_json::json!({ "collectionName": name });
    let _ = client
        .post(format!("{base_url}/v2/vectordb/collections/flush"))
        .json(&flush)
        .send()
        .await;
    tokio::time::sleep(Duration::from_millis(800)).await;
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// Deterministic test vector of given dimension.
pub fn sample_vector(dim: usize) -> Vec<f32> {
    (0..dim).map(|i| (i as f32 + 1.0) / dim as f32).collect()
}

// ---------------------------------------------------------------------------
// Meilisearch
// ---------------------------------------------------------------------------

/// Master key set on the Meilisearch container.
pub const MEILI_MASTER_KEY: &str = "conproxy_test_key";

/// Start a Meilisearch v1.8 container.
pub async fn meilisearch_container() -> ContainerInstance {
    let container: ContainerAsync<GenericImage> = GenericImage::new("getmeili/meilisearch", "v1.8")
        .with_exposed_port(7700.into())
        .with_env_var("MEILI_MASTER_KEY", MEILI_MASTER_KEY)
        .with_env_var("MEILI_ENV", "development")
        .with_env_var("MEILI_NO_ANALYTICS", "true")
        .start()
        .await
        .expect("Meilisearch container failed to start");
    let port = container
        .get_host_port_ipv4(7700)
        .await
        .expect("Meilisearch port mapping");
    let base_url = format!("http://localhost:{port}");
    wait_for_http(&base_url, "/health", Duration::from_secs(60)).await;
    ContainerInstance::new(container, 7700, base_url)
}

/// Create a Meilisearch index with the given primary key.
pub async fn meili_create_index(base_url: &str, uid: &str, primary_key: &str) {
    let client = reqwest::Client::new();
    let body = serde_json::json!({ "uid": uid, "primaryKey": primary_key });
    let resp = client
        .post(format!("{base_url}/indexes"))
        .header("Authorization", format!("Bearer {MEILI_MASTER_KEY}"))
        .json(&body)
        .send()
        .await
        .expect("Meili create_index request");
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    // 201 = created, 202 = task queued, 400 with "already exists" code = idempotent.
    let ok = status.is_success()
        || text.contains("index_already_exists")
        || text.contains("already exists");
    assert!(ok, "Meili create_index failed (HTTP {}): {}", status, text);
}

/// Add documents to a Meilisearch index.
pub async fn meili_add_documents(base_url: &str, uid: &str, docs: Vec<serde_json::Value>) {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base_url}/indexes/{uid}/documents?primaryKey=id"))
        .header("Authorization", format!("Bearer {MEILI_MASTER_KEY}"))
        .json(&docs)
        .send()
        .await
        .expect("Meili add_documents request");
    assert!(
        resp.status().is_success(),
        "Meili add_documents failed: {}",
        resp.text().await.unwrap_or_default()
    );
    // Brief wait for Meili to finish indexing (sub-second for small datasets).
    tokio::time::sleep(Duration::from_millis(500)).await;
}
