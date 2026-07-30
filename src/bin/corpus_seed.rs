//! corpus_seed — seed the e2e test backends with overlapping corpora.
//!
//! Generates three thematic corpora (docs, tickets, code), embeds them with
//! the real ONNX MiniLM model, and loads them into 6 backends with an
//! intentional overlap matrix so the e2e tests can exercise cascade /
//! federation / per-backend routing.
//!
//! Overlap design:
//! - 10 docs from each corpus are seeded to ALL 6 backends (cascade/federated tests)
//! - 50 docs from "docs" → qdrant + pgvector + elasticsearch only
//! - 40 docs from "tickets" → meili-1 + meili-2 + elasticsearch only
//! - 40 docs from "code" → qdrant + pgvector only
//!
//! Per-backend totals: qdrant=110, pgvector=110, elasticsearch=110,
//! opensearch=30 (overlap only), meili-1=50, meili-2=50.
//!
//! Run:
//!   cargo run --bin corpus_seed --features embed,pgvector -- --corpus all --host http://localhost
//!
//! Required env: ORT_LIB_LOCATION (for ONNX link), and the on-host model at
//! ~/.conproxy/models/all-MiniLM-L6-v2/ (download model.onnx + tokenizer.json from
//! https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/onnx).

#![allow(clippy::too_many_lines)]

use conproxy::embedding::embedder::Embedder;
use conproxy::embedding::models::ModelManager;
use reqwest::Client;
use serde_json::json;
use std::error::Error as _;
use std::process::ExitCode;
use std::time::Duration;
use tokio_postgres::NoTls;

// ---------------------------------------------------------------------------
// Doc type
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Doc {
    id: String,
    corpus: String, // "docs" | "tickets" | "code"
    title: String,
    content: String,
    category: String,
    tags: Vec<String>,
    vector: Vec<f32>,
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Args {
    corpus: String,      // "all" | "docs" | "tickets" | "code"
    host: String,        // backend base URL (e.g. http://localhost)
    clear: bool,         // delete collections/indexes before load
    embed_model: String, // ONNX model name (default: all-MiniLM-L6-v2)
    qdrant_collection: String,
    es_index: String,
    opensearch_index: String,
    meili1_index: String,
    meili2_index: String,
    pgvector_table: String,
    pg_url: Option<String>, // default: postgres://postgres:postgres@localhost:5432/conproxy_test
    corpus_dir: String,     // directory containing {docs,tickets,code}.jsonl
}

impl Default for Args {
    fn default() -> Self {
        Self {
            corpus: "all".into(),
            host: "http://localhost".into(),
            clear: false,
            embed_model: "all-MiniLM-L6-v2".into(),
            qdrant_collection: "conproxy_corpus".into(),
            es_index: "conproxy_corpus".into(),
            opensearch_index: "conproxy_corpus".into(),
            meili1_index: "conproxy_corpus".into(),
            meili2_index: "conproxy_corpus".into(),
            pgvector_table: "conproxy_corpus".into(),
            pg_url: None,
            corpus_dir: "tests/corpus/data".into(),
        }
    }
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args::default();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_str();
        let next = argv.get(i + 1).cloned();
        match arg {
            "--corpus" => {
                a.corpus = next.ok_or("--corpus requires value")?;
                i += 1;
            }
            "--host" => {
                a.host = next.ok_or("--host requires value")?;
                i += 1;
            }
            "--clear" => a.clear = true,
            "--embed-model" => {
                a.embed_model = next.ok_or("--embed-model requires value")?;
                i += 1;
            }
            "--qdrant-collection" => {
                a.qdrant_collection = next.ok_or("--qdrant-collection requires value")?;
                i += 1;
            }
            "--es-index" => {
                a.es_index = next.ok_or("--es-index requires value")?;
                i += 1;
            }
            "--opensearch-index" => {
                a.opensearch_index = next.ok_or("--opensearch-index requires value")?;
                i += 1;
            }
            "--meili1-index" => {
                a.meili1_index = next.ok_or("--meili1-index requires value")?;
                i += 1;
            }
            "--meili2-index" => {
                a.meili2_index = next.ok_or("--meili2-index requires value")?;
                i += 1;
            }
            "--pgvector-table" => {
                a.pgvector_table = next.ok_or("--pgvector-table requires value")?;
                i += 1;
            }
            "--pg-url" => {
                a.pg_url = Some(next.ok_or("--pg-url requires value")?);
                i += 1;
            }
            "--corpus-dir" => {
                a.corpus_dir = next.ok_or("--corpus-dir requires value")?;
                i += 1;
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {other}")),
        }
        i += 1;
    }
    if !matches!(a.corpus.as_str(), "all" | "docs" | "tickets" | "code") {
        return Err(format!(
            "--corpus must be one of all|docs|tickets|code (got {})",
            a.corpus
        ));
    }
    Ok(a)
}

fn print_help() {
    println!(
        "corpus_seed — seed e2e backends with overlapping corpora\n\
         \n\
         USAGE:\n  \
             cargo run --bin corpus_seed --features embed,pgvector -- [OPTIONS]\n\
         \n\
         OPTIONS:\n  \
             --corpus <all|docs|tickets|code>   Which corpora to seed (default: all)\n  \
             --host <URL>                       Backend base URL (default: http://localhost)\n  \
             --clear                            Delete collections/indexes/table before load\n  \
             --embed-model <NAME>               ONNX model name (default: all-MiniLM-L6-v2)\n  \
             --qdrant-collection <NAME>         Qdrant collection (default: conproxy_corpus)\n  \
             --es-index <NAME>                  Elasticsearch index (default: conproxy_corpus)\n  \
             --opensearch-index <NAME>          OpenSearch index (default: conproxy_corpus)\n  \
             --meili1-index <NAME>              Meilisearch 1 index (default: conproxy_corpus)\n  \
             --meili2-index <NAME>              Meilisearch 2 index (default: conproxy_corpus)\n  \
             --pgvector-table <NAME>            pgvector table (default: conproxy_corpus)\n  \
             --pg-url <URL>                     Postgres URL (default: postgres://postgres:postgres@localhost:5432/conproxy_test)\n  \
             --help, -h                         Show this help\n\
         \n\
         BACKEND URLS (derived from --host unless overridden):\n  \
             Qdrant:      {{host}}:6333\n  \
             Elastic:     {{host}}:9200\n  \
             OpenSearch:  {{host}}:9201\n  \
             Meili-1:     {{host}}:7700\n  \
             Meili-2:     {{host}}:7701\n  \
             pgvector:    {{host}}:5432 (or --pg-url)\n\
         \n\
         CORPUS OVERLAP:\n  \
             10 docs from each corpus (30 total) seeded to ALL 5 backends.\n  \
             Remaining docs seeded to assigned backends only (per overlap matrix in --help).\n"
    );
}

fn backend_url(host: &str, port: u16) -> String {
    // host may be http://localhost or http://localhost:6333 — strip trailing port if present
    let bare = host
        .rsplit_once(':')
        .map(|(h, p)| {
            if p.parse::<u16>().is_ok() {
                h.to_string()
            } else {
                host.to_string()
            }
        })
        .unwrap_or_else(|| host.to_string());
    format!("{bare}:{port}")
}

/// JSONL entry shape (matches corpus_gen output).
#[allow(dead_code)]
#[derive(serde::Deserialize)]
struct JsonlDoc {
    id: String,
    title: String,
    content: String,
    category: String,
    tags: Vec<String>,
    #[serde(default)]
    topic: Option<String>,
    overlap: bool,
}

fn load_corpus_jsonl(path: &std::path::Path) -> Result<Vec<(Doc, bool)>, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    let mut out = Vec::new();
    for (i, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: JsonlDoc = serde_json::from_str(line)
            .map_err(|e| format!("Failed to parse {}:{}: {}", path.display(), i + 1, e))?;
        let corpus = match entry.id.split('-').next() {
            Some(c) => c.to_string(),
            None => "unknown".to_string(),
        };
        out.push((
            Doc {
                id: entry.id,
                corpus,
                title: entry.title,
                content: entry.content,
                category: entry.category,
                tags: entry.tags,
                vector: vec![],
            },
            entry.overlap,
        ));
    }
    Ok(out)
}

/// Build the per-backend doc sets from the 3 corpora using the overlap matrix.
fn build_backend_sets(docs: &mut [Doc], tickets: &mut [Doc], code: &mut [Doc]) -> BackendSets {
    // Overlap = the first 10 of each corpus
    let overlap_docs: Vec<Doc> = docs.iter().take(10).cloned().collect();
    let overlap_tickets: Vec<Doc> = tickets.iter().take(10).cloned().collect();
    let overlap_code: Vec<Doc> = code.iter().take(10).cloned().collect();
    let universal: Vec<Doc> = overlap_docs
        .into_iter()
        .chain(overlap_tickets)
        .chain(overlap_code)
        .collect();

    // Unique-only sets (skip the first 10 which are overlap)
    let unique_docs: Vec<Doc> = docs.iter().skip(10).cloned().collect();
    let unique_tickets: Vec<Doc> = tickets.iter().skip(10).cloned().collect();
    let unique_code: Vec<Doc> = code.iter().skip(10).cloned().collect();

    BackendSets {
        qdrant: {
            let mut v = universal.clone();
            v.extend(unique_docs.iter().cloned());
            v.extend(unique_code.iter().cloned());
            v
        },
        elastic: {
            let mut v = universal.clone();
            v.extend(unique_docs.iter().cloned());
            v.extend(unique_tickets.iter().cloned());
            v
        },
        opensearch: universal.clone(),
        meili1: {
            let mut v = universal.clone();
            v.extend(unique_tickets.iter().cloned());
            v
        },
        meili2: {
            let mut v = universal.clone();
            v.extend(unique_tickets.iter().cloned());
            v
        },
        pgvector: {
            let mut v = universal;
            v.extend(unique_docs);
            v.extend(unique_code);
            v
        },
    }
}

struct BackendSets {
    qdrant: Vec<Doc>,
    elastic: Vec<Doc>,
    opensearch: Vec<Doc>,
    meili1: Vec<Doc>,
    meili2: Vec<Doc>,
    pgvector: Vec<Doc>,
}

// ---------------------------------------------------------------------------
// Embedding
// ---------------------------------------------------------------------------

fn embed_all(embedder: &Embedder, docs: &mut [Doc]) -> Result<(), String> {
    // Use titles for embedding (cheap, good semantic signal)
    let texts: Vec<String> = docs.iter().map(|d| d.title.clone()).collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let vecs = embedder
        .embed_batch(&refs)
        .map_err(|e| format!("embed_batch: {e}"))?;
    if vecs.len() != docs.len() {
        return Err(format!(
            "embed count mismatch: {} vs {}",
            vecs.len(),
            docs.len()
        ));
    }
    for (d, v) in docs.iter_mut().zip(vecs.into_iter()) {
        d.vector = v;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Loaders
// ---------------------------------------------------------------------------

async fn load_qdrant(
    client: &Client,
    url: &str,
    collection: &str,
    docs: &[Doc],
    clear: bool,
) -> Result<usize, String> {
    if clear {
        let _ = client
            .delete(format!("{url}/collections/{collection}"))
            .send()
            .await;
    }
    let create_url = format!("{url}/collections/{collection}");
    let _ = client
        .put(&create_url)
        .json(&json!({
            "vectors": { "size": 384, "distance": "Cosine" }
        }))
        .send()
        .await;

    eprintln!("qdrant: upserting {} docs to {collection}", docs.len());
    let points: Vec<serde_json::Value> = docs
        .iter()
        .map(|d| {
            // Qdrant requires integer or UUID id; use a stable hash of the string id.
            let id_num: u64 =
                d.id.bytes()
                    .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
            json!({
                "id": id_num,
                "vector": d.vector,
                "payload": {
                    "doc_id": d.id,
                    "corpus": d.corpus,
                    "title": d.title,
                    "content": d.content,
                    "category": d.category,
                    "tags": d.tags,
                }
            })
        })
        .collect();
    let resp = client
        .put(format!("{url}/collections/{collection}/points"))
        .json(&json!({ "points": points }))
        .send()
        .await
        .map_err(|e| format!("qdrant upsert: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("qdrant upsert HTTP {}", resp.status()));
    }
    Ok(docs.len())
}

async fn load_elastic(
    client: &Client,
    url: &str,
    index: &str,
    docs: &[Doc],
    clear: bool,
    include_vector: bool,
) -> Result<usize, String> {
    if clear {
        let _ = client.delete(format!("{url}/{index}")).send().await;
    }
    let index_url = format!("{url}/{index}");
    let mut mapping = json!({
        "settings": { "number_of_shards": 1, "number_of_replicas": 0 },
        "mappings": {
            "properties": {
                "doc_id": { "type": "keyword" },
                "corpus": { "type": "keyword" },
                "title": { "type": "text", "analyzer": "standard" },
                "content": { "type": "text", "analyzer": "standard" },
                "category": { "type": "keyword" },
                "tags": { "type": "keyword" }
            }
        }
    });
    if include_vector {
        mapping["mappings"]["properties"]["vector"] = json!({
            "type": "dense_vector", "dims": 384, "index": true, "similarity": "cosine"
        });
    }
    let _ = client.put(&index_url).json(&mapping).send().await;

    eprintln!(
        "{}: bulk-loading {} docs to {index}",
        if include_vector {
            "elastic"
        } else {
            "opensearch"
        },
        docs.len()
    );
    let mut body = String::new();
    for d in docs {
        let action = json!({ "index": { "_id": d.id } });
        let mut src = json!({
            "doc_id": d.id,
            "corpus": d.corpus,
            "title": d.title,
            "content": d.content,
            "category": d.category,
            "tags": d.tags,
        });
        if include_vector {
            src["vector"] = json!(d.vector);
        }
        body.push_str(&serde_json::to_string(&action).map_err(|e| e.to_string())?);
        body.push('\n');
        body.push_str(&serde_json::to_string(&src).map_err(|e| e.to_string())?);
        body.push('\n');
    }
    let resp = client
        .post(format!("{url}/{index}/_bulk"))
        .header("Content-Type", "application/x-ndjson")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("bulk: {e}"))?;
    if !resp.status().is_success() {
        let s = resp.status();
        let t = resp.text().await.unwrap_or_default();
        return Err(format!("bulk HTTP {s}: {t}"));
    }
    let _ = client.post(format!("{url}/{index}/_refresh")).send().await;
    Ok(docs.len())
}

async fn load_meili(
    client: &Client,
    url: &str,
    index: &str,
    docs: &[Doc],
    clear: bool,
) -> Result<usize, String> {
    let key = std::env::var("MEILI_MASTER_KEY").unwrap_or_else(|_| "conproxy_test_key".into());
    let mut req = client.delete(format!("{url}/indexes/{index}"));
    if !key.is_empty() {
        req = req.bearer_auth(&key);
    }
    if clear {
        let _ = req.send().await;
    }
    let mut create = client.post(format!("{url}/indexes"));
    if !key.is_empty() {
        create = create.bearer_auth(&key);
    }
    let _ = create
        .json(&json!({ "uid": index, "primaryKey": "doc_id" }))
        .send()
        .await;

    eprintln!("meili: uploading {} docs to {index}", docs.len());
    let meili_docs: Vec<serde_json::Value> = docs
        .iter()
        .map(|d| {
            json!({
                "doc_id": d.id,
                "corpus": d.corpus,
                "title": d.title,
                "content": d.content,
                "category": d.category,
                "tags": d.tags,
            })
        })
        .collect();
    let mut upload = client
        .post(format!("{url}/indexes/{index}/documents"))
        .json(&meili_docs);
    if !key.is_empty() {
        upload = upload.bearer_auth(&key);
    }
    let resp = upload
        .send()
        .await
        .map_err(|e| format!("meili upload: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("meili upload HTTP {}", resp.status()));
    }
    Ok(docs.len())
}

async fn load_pgvector(
    pg_url: &str,
    table: &str,
    docs: &[Doc],
    clear: bool,
) -> Result<usize, String> {
    let (client, connection) = tokio_postgres::connect(pg_url, NoTls)
        .await
        .map_err(|e| format!("pg connect: {e}"))?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("pg connection error: {e}");
        }
    });

    if clear {
        client
            .execute(&format!("DROP TABLE IF EXISTS {table}"), &[])
            .await
            .map_err(|e| format!("pg drop: {e}"))?;
    }
    client
        .execute("CREATE EXTENSION IF NOT EXISTS vector", &[])
        .await
        .map_err(|e| format!("pg extension: {e}"))?;
    client
        .execute(
            &format!(
                "CREATE TABLE IF NOT EXISTS {table} (
                    id BIGINT PRIMARY KEY,
                    doc_id TEXT NOT NULL,
                    corpus TEXT NOT NULL,
                    title TEXT NOT NULL,
                    content TEXT NOT NULL,
                    category TEXT,
                    tags TEXT,
                    vector VECTOR(384)
                )"
            ),
            &[],
        )
        .await
        .map_err(|e| format!("pg create table: {e}"))?;

    eprintln!("pgvector: inserting {} docs to {table}", docs.len());
    let mut count = 0;
    for d in docs {
        // Vector literal format: '[v1,v2,v3,...]'
        let vec_lit = format!(
            "[{}]",
            d.vector
                .iter()
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        let id_num: i64 =
            d.id.bytes()
                .fold(0i64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as i64));
        // Build SQL with inline values (avoids placeholder ordering surprises
        // when mixing primitive + array types). Vector is the only escape
        // hatch: pass it as text and let PG CAST.
        let esc = |s: &str| s.replace('\'', "''");
        let sql = format!(
            "INSERT INTO {table} (id, doc_id, corpus, title, content, category, tags, vector) \
             VALUES ({id_num}, '{doc_id}', '{corpus}', '{title}', '{content}', '{category}', '{tags}', CAST('{vec_lit}' AS vector)) \
             ON CONFLICT (id) DO UPDATE SET \
                 title = EXCLUDED.title, content = EXCLUDED.content, vector = EXCLUDED.vector",
            table = table,
            id_num = id_num,
            doc_id = esc(&d.id),
            corpus = esc(&d.corpus),
            title = esc(&d.title),
            content = esc(&d.content),
            category = esc(&d.category),
            tags = esc(&d.tags.join(",")),
            vec_lit = esc(&vec_lit),
        );
        client.execute(&sql, &[]).await.map_err(|e| {
            let mut msg = format!("pg insert {}: {e}", d.id);
            let mut src: Option<&dyn std::error::Error> = e.source();
            while let Some(s) = src {
                msg.push_str(&format!(" -> {s}"));
                src = s.source();
            }
            msg
        })?;
        count += 1;
    }
    Ok(count)
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[corpus_seed] arg error: {e}");
            eprintln!("Run with --help for usage.");
            return ExitCode::from(2);
        }
    };

    eprintln!(
        "[corpus_seed] starting (corpus={}, host={})",
        args.corpus, args.host
    );

    if !ModelManager::is_installed(&args.embed_model) {
        eprintln!(
            "[corpus_seed] model '{}' not installed at {}. Download model.onnx + tokenizer.json from https://huggingface.co/ into the directory above, or set --embed-model to an installed model.",
            args.embed_model,
            ModelManager::models_dir().display()
        );
        return ExitCode::from(1);
    }
    let model_path = ModelManager::model_path(&args.embed_model);
    let tok_path = ModelManager::tokenizer_path(&args.embed_model);
    let embedder = match Embedder::new(&model_path, &tok_path) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[corpus_seed] embedder init failed: {e}");
            return ExitCode::from(1);
        }
    };
    eprintln!(
        "[corpus_seed] embedder ready (dims={})",
        embedder.dimensions()
    );

    // Build corpora from JSONL files
    let base = std::path::Path::new(&args.corpus_dir);
    let all_docs_pairs = match load_corpus_jsonl(&base.join("docs.jsonl")) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("docs corpus: {e}");
            return ExitCode::FAILURE;
        }
    };
    let all_tickets_pairs = match load_corpus_jsonl(&base.join("tickets.jsonl")) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("tickets corpus: {e}");
            return ExitCode::FAILURE;
        }
    };
    let all_code_pairs = match load_corpus_jsonl(&base.join("code.jsonl")) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("code corpus: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (mut docs, mut tickets, mut code): (Vec<Doc>, Vec<Doc>, Vec<Doc>) =
        match args.corpus.as_str() {
            "docs" => (
                all_docs_pairs.into_iter().map(|(d, _)| d).collect(),
                Vec::new(),
                Vec::new(),
            ),
            "tickets" => (
                Vec::new(),
                all_tickets_pairs.into_iter().map(|(d, _)| d).collect(),
                Vec::new(),
            ),
            "code" => (
                Vec::new(),
                Vec::new(),
                all_code_pairs.into_iter().map(|(d, _)| d).collect(),
            ),
            _ => (
                all_docs_pairs.into_iter().map(|(d, _)| d).collect(),
                all_tickets_pairs.into_iter().map(|(d, _)| d).collect(),
                all_code_pairs.into_iter().map(|(d, _)| d).collect(),
            ),
        };

    let total = docs.len() + tickets.len() + code.len();
    eprintln!(
        "[corpus_seed] {} total docs to embed (docs={}, tickets={}, code={})",
        total,
        docs.len(),
        tickets.len(),
        code.len()
    );

    embed_all(&embedder, &mut docs).unwrap_or_else(|e| eprintln!("[corpus_seed] docs embed: {e}"));
    embed_all(&embedder, &mut tickets)
        .unwrap_or_else(|e| eprintln!("[corpus_seed] tickets embed: {e}"));
    embed_all(&embedder, &mut code).unwrap_or_else(|e| eprintln!("[corpus_seed] code embed: {e}"));

    let sets = build_backend_sets(&mut docs, &mut tickets, &mut code);
    eprintln!(
        "[corpus_seed] backend sets: qdrant={}, elastic={}, opensearch={}, meili1={}, meili2={}, pgvector={}",
        sets.qdrant.len(), sets.elastic.len(), sets.opensearch.len(),
        sets.meili1.len(), sets.meili2.len(), sets.pgvector.len()
    );

    let client = match Client::builder().timeout(Duration::from_secs(60)).build() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[corpus_seed] http client: {e}");
            return ExitCode::from(1);
        }
    };

    let qdrant_url = backend_url(&args.host, 6333);
    let elastic_url = backend_url(&args.host, 9200);
    let opensearch_url = backend_url(&args.host, 9201);
    let meili1_url = backend_url(&args.host, 7700);
    let meili2_url = backend_url(&args.host, 7701);
    let pg_url = args
        .pg_url
        .clone()
        .unwrap_or_else(|| "postgres://postgres:postgres@localhost:5432/conproxy_test".into());

    let qd = sets.qdrant.clone();
    let el = sets.elastic.clone();
    let os = sets.opensearch.clone();
    let m1 = sets.meili1.clone();
    let m2 = sets.meili2.clone();
    let pg = sets.pgvector.clone();
    let clear = args.clear;
    let qc = args.qdrant_collection.clone();
    let ec = args.es_index.clone();
    let oc = args.opensearch_index.clone();
    let m1c = args.meili1_index.clone();
    let m2c = args.meili2_index.clone();
    let pgt = args.pgvector_table.clone();
    let pg_u = pg_url.clone();
    let cl = client.clone();

    // Run all 5 backends in parallel
    let (qdrant_r, elastic_r, opensearch_r, meili1_r, meili2_r, pg_r) = tokio::join!(
        load_qdrant(&cl, &qdrant_url, &qc, &qd, clear),
        load_elastic(&cl, &elastic_url, &ec, &el, clear, true),
        load_elastic(&cl, &opensearch_url, &oc, &os, clear, false),
        load_meili(&cl, &meili1_url, &m1c, &m1, clear),
        load_meili(&cl, &meili2_url, &m2c, &m2, clear),
        load_pgvector(&pg_u, &pgt, &pg, clear),
    );

    let mut failed = 0;
    let results: Vec<(&str, &Result<usize, String>)> = vec![
        ("qdrant", &qdrant_r),
        ("elastic", &elastic_r),
        ("opensearch", &opensearch_r),
        ("meili-1", &meili1_r),
        ("meili-2", &meili2_r),
        ("pgvector", &pg_r),
    ];
    for (name, r) in &results {
        match r {
            Ok(n) => eprintln!("[corpus_seed] {name}: ✓ {n} docs loaded"),
            Err(e) => {
                eprintln!("[corpus_seed] {name}: ✗ {e}");
                failed += 1;
            }
        }
    }

    if failed > 0 {
        eprintln!("[corpus_seed] {failed} backends failed");
        ExitCode::from(1)
    } else {
        eprintln!("[corpus_seed] all backends seeded");
        ExitCode::SUCCESS
    }
}
