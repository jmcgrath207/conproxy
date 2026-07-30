//! pgvector adapter for PostgreSQL vector search.
//!
//! Implements `UpstreamAdapter` for pgvector backends, communicating over
//! the PostgreSQL wire protocol via `tokio-postgres`. This adapter is
//! VectorOnly — the proxy must embed queries locally before sending vectors.
//!
//! # Distance Metrics
//!
//! - `Cosine` — `<=>` operator, similarity = 1 - distance
//! - `L2` — `<->` operator, similarity = 1 / (1 + distance)
//! - `InnerProduct` — `<#>` operator, similarity = -distance

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio_postgres::Client;

use super::types::{CacheStatus, QueryRequest, QueryResponse, SearchResult};
use super::upstream::{AdapterMetadata, QueryMode, UpstreamAdapter, UpstreamError};

/// Distance metric for pgvector similarity search.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DistanceMetric {
    /// Cosine distance: `<=>` operator.
    #[default]
    Cosine,
    /// Euclidean (L2) distance: `<->` operator.
    L2,
    /// Negative inner product: `<#>` operator.
    InnerProduct,
}

impl DistanceMetric {
    /// Get the pgvector SQL operator for this metric.
    pub fn operator(&self) -> &'static str {
        match self {
            Self::Cosine => "<=>",
            Self::L2 => "<->",
            Self::InnerProduct => "<#>",
        }
    }

    /// Convert a pgvector distance value to a 0-1 similarity score.
    pub fn to_similarity(&self, distance: f64) -> f32 {
        match self {
            Self::Cosine => (1.0 - distance) as f32,
            Self::L2 => (1.0 / (1.0 + distance)) as f32,
            Self::InnerProduct => (-distance) as f32,
        }
    }
}

/// Configuration for the pgvector adapter.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PgvectorConfig {
    /// PostgreSQL connection URL (e.g., `postgresql://user:pass@host:5432/db`).
    pub url: String,
    /// Table name containing vectors.
    pub table: String,
    /// Column containing the vector embedding.
    pub embedding_column: String,
    /// Column containing the text content.
    pub content_column: String,
    /// Optional column for document title/ID.
    #[serde(default)]
    pub title_column: Option<String>,
    /// Additional columns to include in results.
    #[serde(default)]
    pub metadata_columns: Vec<String>,
    /// Distance metric to use.
    #[serde(default)]
    pub distance_metric: DistanceMetric,
    /// Expected vector dimensions (validated at startup).
    #[serde(default)]
    pub dimensions: Option<usize>,
    /// Query timeout.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_timeout_secs() -> u64 {
    30
}

impl PgvectorConfig {
    /// Validate the configuration.
    pub fn validate(&self) -> Result<(), String> {
        if self.url.is_empty() {
            return Err("pgvector url is required".to_string());
        }
        if self.table.is_empty() {
            return Err("pgvector table is required".to_string());
        }
        if self.embedding_column.is_empty() {
            return Err("pgvector embedding_column is required".to_string());
        }
        if self.content_column.is_empty() {
            return Err("pgvector content_column is required".to_string());
        }
        // Validate identifier safety (prevent SQL injection)
        for name in std::iter::once(&self.table)
            .chain(std::iter::once(&self.embedding_column))
            .chain(std::iter::once(&self.content_column))
            .chain(self.title_column.iter())
            .chain(self.metadata_columns.iter())
        {
            if !is_safe_identifier(name) {
                return Err(format!("unsafe identifier: {}", name));
            }
        }
        Ok(())
    }
}

/// Check that a string is a safe SQL identifier (alphanumeric + underscore).
fn is_safe_identifier(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Build the SELECT column list for a pgvector query.
pub fn build_select_columns(config: &PgvectorConfig) -> String {
    let mut cols = vec![config.content_column.clone()];
    if let Some(ref title_col) = config.title_column {
        cols.push(title_col.clone());
    }
    for col in &config.metadata_columns {
        cols.push(col.clone());
    }
    // Add distance as a computed column
    cols.push(format!(
        "{} {} $1::vector AS distance",
        config.embedding_column,
        config.distance_metric.operator()
    ));
    cols.join(", ")
}

/// Build the full SQL query for vector similarity search.
pub fn build_query_sql(config: &PgvectorConfig) -> String {
    let columns = build_select_columns(config);
    format!(
        "SELECT {} FROM {} ORDER BY {} {} $1::vector LIMIT $2",
        columns,
        config.table,
        config.embedding_column,
        config.distance_metric.operator()
    )
}

/// Format a vector as pgvector text representation: `[0.1,0.2,0.3]`.
#[allow(clippy::arithmetic_side_effects)]
pub fn format_vector(vector: &[f32]) -> String {
    let mut s = String::with_capacity(vector.len() * 8 + 2);
    s.push('[');
    for (i, v) in vector.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&v.to_string());
    }
    s.push(']');
    s
}

/// pgvector upstream adapter.
pub struct PgvectorAdapter {
    client: Arc<Client>,
    config: PgvectorConfig,
    query_sql: String,
    query_mode: AtomicU8,
}

impl PgvectorAdapter {
    /// Connect to PostgreSQL and create the adapter.
    #[allow(clippy::redundant_closure)]
    pub async fn connect(config: PgvectorConfig) -> Result<Self, UpstreamError> {
        config.validate().map_err(|e| UpstreamError::Network(e))?;

        let (client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls)
            .await
            .map_err(|e| UpstreamError::Network(format!("pgvector connect: {e}")))?;

        // Spawn the connection handler
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                tracing::error!(error = %e, "pgvector connection terminated with error");
            }
        });

        // Validate dimensions if specified
        if let Some(dims) = config.dimensions {
            let check_sql = format!(
                "SELECT vector_dims({}) FROM {} LIMIT 1",
                config.embedding_column, config.table
            );
            match client.query_opt(&check_sql, &[]).await {
                Ok(Some(row)) => {
                    let actual_dims: i32 = row.get(0);
                    if actual_dims as usize != dims {
                        return Err(UpstreamError::Network(format!(
                            "pgvector dimension mismatch: expected {}, got {}",
                            dims, actual_dims
                        )));
                    }
                }
                Ok(None) => {
                    // Empty table — can't validate, proceed anyway
                }
                Err(e) => {
                    return Err(UpstreamError::Network(format!(
                        "pgvector dimension check failed: {}",
                        e
                    )));
                }
            }
        }

        let query_sql = build_query_sql(&config);

        Ok(Self {
            client: Arc::new(client),
            config,
            query_sql,
            query_mode: AtomicU8::new(QueryMode::VectorOnly as u8),
        })
    }

    /// Get the configuration.
    pub fn config(&self) -> &PgvectorConfig {
        &self.config
    }
}

#[async_trait]
impl UpstreamAdapter for PgvectorAdapter {
    async fn query(&self, _request: &QueryRequest) -> Result<QueryResponse, UpstreamError> {
        // pgvector is VectorOnly — text queries need embedding first
        Err(UpstreamError::UnsupportedQueryType(
            "pgvector requires vector queries (use query_vector)".to_string(),
        ))
    }

    #[allow(clippy::arithmetic_side_effects)]
    async fn query_vector(
        &self,
        request: &QueryRequest,
        vector: &[f32],
    ) -> Result<QueryResponse, UpstreamError> {
        if let Some(dims) = self.config.dimensions {
            if vector.len() != dims {
                return Err(UpstreamError::Network(format!(
                    "pgvector query vector dimension mismatch: expected {dims}, got {}",
                    vector.len()
                )));
            }
        }
        let start = Instant::now();
        let vector_text = format_vector(vector);
        let limit = request.top_k.unwrap_or(10) as i64;

        // Interpolate vector_text directly into SQL — tokio-postgres
        // cannot serialise a String parameter for the `vector` cast.
        // Rename $2 to $1 after replacement since $1 is now a literal.
        let sql = self
            .query_sql
            .replace("$1", &format!("'{vector_text}'"))
            .replace("$2", "$1");
        let rows = self
            .client
            .query(&sql, &[&limit])
            .await
            .map_err(|e| UpstreamError::Network(format!("pgvector query: {sql}: {e}")))?;

        let mut results = Vec::with_capacity(rows.len());
        for (idx, row) in rows.iter().enumerate() {
            let content: String = row.get(0);
            let title = if self.config.title_column.is_some() {
                row.try_get::<_, String>(1).ok()
            } else {
                None
            };

            // Distance is always the last column
            let distance_col_idx = row.len() - 1;
            let distance: f64 = row.get(distance_col_idx);
            let score = self.config.distance_metric.to_similarity(distance);

            let id = title.unwrap_or_else(|| format!("row-{}", idx));

            // Collect metadata columns into JSON
            let metadata = if !self.config.metadata_columns.is_empty() {
                let mut map = serde_json::Map::new();
                let offset = if self.config.title_column.is_some() {
                    2
                } else {
                    1
                };
                for (i, col_name) in self.config.metadata_columns.iter().enumerate() {
                    if let Ok(val) = row.try_get::<_, String>(offset + i) {
                        map.insert(col_name.clone(), serde_json::Value::String(val));
                    }
                }
                if map.is_empty() {
                    None
                } else {
                    Some(serde_json::Value::Object(map))
                }
            } else {
                None
            };

            results.push(SearchResult {
                id,
                content,
                score,
                metadata,
                upstream_id: None,
            });
        }

        Ok(QueryResponse {
            results,
            cache_status: CacheStatus::Miss,
            took_ms: start.elapsed().as_millis() as u64,
            generated_at: Some(QueryResponse::current_time_ms()),
            miss_reason: None,
        })
    }

    async fn health_check(&self) -> Result<bool, UpstreamError> {
        self.client
            .query_one("SELECT 1", &[])
            .await
            .map(|_| true)
            .map_err(|e| UpstreamError::Network(format!("pgvector health: {}", e)))
    }

    fn identifier(&self) -> &str {
        &self.config.url
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(self.config.timeout_secs)
    }

    fn metadata(&self) -> AdapterMetadata {
        AdapterMetadata {
            adapter_type: "pgvector".to_string(),
            version: None,
            properties: {
                let mut p = std::collections::HashMap::new();
                p.insert("table".to_string(), self.config.table.clone());
                p.insert(
                    "distance_metric".to_string(),
                    format!("{:?}", self.config.distance_metric),
                );
                p
            },
        }
    }

    fn query_mode(&self) -> QueryMode {
        QueryMode::from(self.query_mode.load(Ordering::SeqCst))
    }

    fn set_query_mode(&self, mode: QueryMode) {
        self.query_mode.store(mode as u8, Ordering::SeqCst);
    }

    async fn discover_query_mode(&self) -> Result<QueryMode, UpstreamError> {
        // pgvector is always VectorOnly
        Ok(QueryMode::VectorOnly)
    }
}

#[cfg(test)]
#[path = "tests/pgvector_tests.rs"]
mod tests;
