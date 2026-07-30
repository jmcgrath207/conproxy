use crate::helpers::constants::Vertical;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// A single eval query with expected document IDs.
#[allow(dead_code)]
pub struct EvalQuery {
    pub id: String,
    pub query: String,
    pub query_type: String,
    /// Routing strategy this query exercises: "fts", "vector", or "cascade".
    pub strategy: String,
    pub expected_doc_ids: Vec<String>,
    pub min_score: f64,
    pub description: String,
}

/// A proprietary eval document.
#[allow(dead_code)]
#[derive(Clone)]
pub struct Document {
    pub id: String,
    pub title: String,
    pub content: String,
    pub category: String,
    pub tags: Vec<String>,
}

/// Pre-computed index of documents for efficient scoring.
pub struct DocIndex {
    pub docs: HashMap<String, Document>,
    /// Per-document uncommon terms (lowercase).
    pub key_terms: HashMap<String, Vec<String>>,
}

/// Common English stop words to filter from key terms.
const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "all", "can", "had", "her", "was", "one",
    "our", "out", "has", "have", "each", "make", "like", "from", "this", "that", "with", "they",
    "been", "will", "more", "when", "than", "into", "some", "very", "what", "about", "which",
    "their", "other", "also", "its", "over", "such", "most", "only", "then", "them", "same",
    "much", "would", "where", "does", "using", "based", "uses", "used",
];

impl DocIndex {
    /// Build a DocIndex from loaded documents.
    ///
    /// Pre-computes key terms per document: words that appear in <= 2 docs across the corpus.
    pub fn build(docs: &[Document]) -> Self {
        let stop_set: HashSet<&str> = STOP_WORDS.iter().copied().collect();

        // Count document frequency per word
        let mut doc_freq: HashMap<String, usize> = HashMap::new();
        let mut per_doc_words: HashMap<String, HashSet<String>> = HashMap::new();

        for doc in docs {
            let mut words = HashSet::new();
            for word in tokenize(&doc.content) {
                if word.len() > 3 && !stop_set.contains(word.as_str()) {
                    words.insert(word);
                }
            }
            // Also include title words
            for word in tokenize(&doc.title) {
                if word.len() > 3 && !stop_set.contains(word.as_str()) {
                    words.insert(word);
                }
            }
            per_doc_words.insert(doc.id.clone(), words);
        }

        for words in per_doc_words.values() {
            for word in words {
                *doc_freq.entry(word.clone()).or_insert(0) += 1;
            }
        }

        // Key terms: words appearing in <= 2 documents
        let mut key_terms: HashMap<String, Vec<String>> = HashMap::new();
        for (doc_id, words) in &per_doc_words {
            let terms: Vec<String> = words
                .iter()
                .filter(|w| doc_freq.get(*w).copied().unwrap_or(0) <= 2)
                .cloned()
                .collect();
            key_terms.insert(doc_id.clone(), terms);
        }

        let docs_map: HashMap<String, Document> = HashMap::new();
        // We'll build this in load_eval_documents

        Self {
            docs: docs_map,
            key_terms,
        }
    }

    /// Build from a slice of documents (clones into internal map).
    pub fn from_documents(docs: &[Document]) -> Self {
        let index = Self::build(docs);
        let docs_map: HashMap<String, Document> =
            docs.iter().cloned().map(|d| (d.id.clone(), d)).collect();
        Self {
            docs: docs_map,
            key_terms: index.key_terms,
        }
    }
}

/// Tokenize text into lowercase words.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect()
}

/// Load eval queries from the JSON file.
pub fn load_eval_queries(data_dir: &Path) -> Vec<EvalQuery> {
    let path = data_dir.join("eval_queries.json");
    let content = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("Failed to read {}: {e}", path.display());
    });
    let json: Value = serde_json::from_str(&content).unwrap_or_else(|e| {
        panic!("Failed to parse {}: {e}", path.display());
    });

    let queries = json["queries"]
        .as_array()
        .expect("eval_queries.json must have a 'queries' array");

    queries
        .iter()
        .map(|q| EvalQuery {
            id: q["id"].as_str().unwrap().to_string(),
            query: q["query"].as_str().unwrap().to_string(),
            query_type: q["query_type"].as_str().unwrap().to_string(),
            strategy: q["strategy"].as_str().unwrap_or("unknown").to_string(),
            expected_doc_ids: q["expected_doc_ids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect(),
            min_score: q["min_score"].as_f64().unwrap_or(0.3),
            description: q["description"].as_str().unwrap_or("").to_string(),
        })
        .collect()
}

/// Load eval documents from the JSON file.
pub fn load_eval_documents(data_dir: &Path) -> Vec<Document> {
    let path = data_dir.join("eval_docs.json");
    let content = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("Failed to read {}: {e}", path.display());
    });
    let json: Value = serde_json::from_str(&content).unwrap_or_else(|e| {
        panic!("Failed to parse {}: {e}", path.display());
    });

    let documents = json["documents"]
        .as_array()
        .expect("eval_docs.json must have a 'documents' array");

    documents
        .iter()
        .map(|d| Document {
            id: d["id"].as_str().unwrap().to_string(),
            title: d["title"].as_str().unwrap().to_string(),
            content: d["content"].as_str().unwrap().to_string(),
            category: d["category"].as_str().unwrap_or("").to_string(),
            tags: d["tags"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|v| v.as_str().unwrap().to_string())
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect()
}

/// Partition queries across verticals so each vertical gets exactly 1 query per strategy.
///
/// Groups queries by strategy (fts, vector, cascade), then assigns one query from each
/// group to each vertical by index. Returns a `Vec<Vec<EvalQuery>>` aligned with
/// `verticals` — entry `i` contains the 3 queries for `verticals[i]`.
///
/// Panics if any strategy group has fewer queries than the number of verticals.
pub fn partition_queries_by_vertical(
    queries: &[EvalQuery],
    verticals: &[Vertical],
) -> Vec<Vec<EvalQuery>> {
    let mut by_strategy: HashMap<&str, Vec<&EvalQuery>> = HashMap::new();
    for q in queries {
        by_strategy.entry(q.strategy.as_str()).or_default().push(q);
    }

    for strategy in &["fts", "vector", "cascade"] {
        let group = by_strategy.get(strategy).map(|g| g.len()).unwrap_or(0);
        assert!(
            group >= verticals.len(),
            "Strategy '{}' has {} queries but need {} (one per vertical)",
            strategy,
            group,
            verticals.len(),
        );
    }

    let mut result: Vec<Vec<EvalQuery>> = (0..verticals.len())
        .map(|_| Vec::with_capacity(3))
        .collect();

    for strategy in &["fts", "vector", "cascade"] {
        if let Some(group) = by_strategy.get(strategy) {
            for (vi, q) in group.iter().enumerate().take(verticals.len()) {
                result[vi].push(EvalQuery {
                    id: q.id.clone(),
                    query: q.query.clone(),
                    query_type: q.query_type.clone(),
                    strategy: q.strategy.clone(),
                    expected_doc_ids: q.expected_doc_ids.clone(),
                    min_score: q.min_score,
                    description: q.description.clone(),
                });
            }
        }
    }

    result
}
