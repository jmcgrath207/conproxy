//! redb-based cache persistence for durable storage.
//!
//! This module provides optional persistence for the cache proxy using redb,
//! an actively maintained pure-Rust embedded database. When enabled, cache
//! entries are persisted to disk and restored on startup.
//!
//! # Features
//!
//! - **Disk persistence**: Cache survives proxy restarts
//! - **ACID transactions**: All writes are transactional
//! - **Graceful shutdown**: Compact on demand
//! - **Health state persistence**: Upstream health status preserved
//!
//! # Usage
//!
//! Enable the `proxy-persistence` feature in Cargo.toml:
//!
//! ```toml
//! [dependencies]
//! conproxy = { features = ["proxy-persistence"] }
//! ```

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};

use super::context::ContextMetadata;
use super::types::{CacheStatus, QueryResponse, SearchResult};

// Table definitions (key type, value type)
const ENTRIES_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("cache_entries");
const HEALTH_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("upstream_health");
const META_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("metadata");

/// Error type for persistence operations.
#[derive(Debug)]
pub enum PersistenceError {
    /// Failed to open or create the database.
    DatabaseOpen(String),
    /// Failed to serialize data.
    Serialize(String),
    /// Failed to deserialize data.
    Deserialize(String),
    /// Failed to write to database.
    Write(String),
    /// Failed to read from database.
    Read(String),
    /// Failed to flush/compact database.
    Flush(String),
}

impl std::fmt::Display for PersistenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DatabaseOpen(e) => write!(f, "Failed to open database: {}", e),
            Self::Serialize(msg) => write!(f, "Serialization error: {}", msg),
            Self::Deserialize(msg) => write!(f, "Deserialization error: {}", msg),
            Self::Write(e) => write!(f, "Database write error: {}", e),
            Self::Read(e) => write!(f, "Database read error: {}", e),
            Self::Flush(e) => write!(f, "Database flush error: {}", e),
        }
    }
}

impl std::error::Error for PersistenceError {}

/// Persistent cache entry stored in redb.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistedEntry {
    /// The query string (normalized).
    pub query: String,
    /// The upstream ID that provided this response.
    pub upstream_id: String,
    /// The cached response.
    pub response: PersistedResponse,
    /// When this entry was cached (Unix timestamp millis).
    pub cached_at_ms: u64,
    /// How many times the TTL has been extended.
    pub extended_count: u32,
    /// Content hash for integrity verification.
    pub content_hash: [u8; 32],
}

/// Serializable version of QueryResponse.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistedResponse {
    /// Search results.
    pub results: Vec<PersistedResult>,
    /// Cache status when originally fetched.
    pub cache_status: String,
    /// Time taken to fetch (ms).
    pub took_ms: u64,
    /// When the response was generated.
    pub generated_at: Option<u64>,
}

/// Serializable version of SearchResult.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistedResult {
    pub id: String,
    pub content: String,
    pub score: f32,
    pub metadata: Option<serde_json::Value>,
}

impl From<&QueryResponse> for PersistedResponse {
    fn from(response: &QueryResponse) -> Self {
        Self {
            results: response
                .results
                .iter()
                .map(|r| PersistedResult {
                    id: r.id.clone(),
                    content: r.content.clone(),
                    score: r.score,
                    metadata: r.metadata.clone(),
                })
                .collect(),
            cache_status: match response.cache_status {
                CacheStatus::Hit => "hit".to_string(),
                CacheStatus::Miss => "miss".to_string(),
                CacheStatus::Stale => "stale".to_string(),
                CacheStatus::Frozen => "frozen".to_string(),
            },
            took_ms: response.took_ms,
            generated_at: response.generated_at,
        }
    }
}

impl From<&PersistedResponse> for QueryResponse {
    fn from(persisted: &PersistedResponse) -> Self {
        Self {
            results: persisted
                .results
                .iter()
                .map(|r| SearchResult {
                    id: r.id.clone(),
                    content: r.content.clone(),
                    score: r.score,
                    metadata: r.metadata.clone(),
                    upstream_id: None,
                })
                .collect(),
            cache_status: match persisted.cache_status.as_str() {
                "hit" => CacheStatus::Hit,
                "miss" => CacheStatus::Miss,
                "stale" => CacheStatus::Stale,
                "frozen" => CacheStatus::Frozen,
                _ => CacheStatus::Miss,
            },
            took_ms: persisted.took_ms,
            generated_at: persisted.generated_at,
            miss_reason: None,
        }
    }
}

/// Persisted upstream health state.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistedUpstreamHealth {
    /// Upstream ID.
    pub upstream_id: String,
    /// Current status.
    pub status: String,
    /// Consecutive failures count.
    pub consecutive_failures: u32,
    /// Consecutive successes count.
    pub consecutive_successes: u32,
    /// Last success timestamp (Unix millis).
    pub last_success_ms: Option<u64>,
    /// Last failure timestamp (Unix millis).
    pub last_failure_ms: Option<u64>,
}

/// Persistent cache storage using redb.
pub struct PersistentCache {
    db: Database,
}

impl PersistentCache {
    /// Open or create a persistent cache at the given path.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, PersistenceError> {
        let db =
            Database::create(path).map_err(|e| PersistenceError::DatabaseOpen(e.to_string()))?;

        // Initialize tables by opening a write transaction
        let write_txn = db
            .begin_write()
            .map_err(|e| PersistenceError::DatabaseOpen(e.to_string()))?;
        let _ = write_txn
            .open_table(ENTRIES_TABLE)
            .map_err(|e| PersistenceError::DatabaseOpen(e.to_string()))?;
        let _ = write_txn
            .open_table(HEALTH_TABLE)
            .map_err(|e| PersistenceError::DatabaseOpen(e.to_string()))?;
        let _ = write_txn
            .open_table(META_TABLE)
            .map_err(|e| PersistenceError::DatabaseOpen(e.to_string()))?;
        write_txn
            .commit()
            .map_err(|e| PersistenceError::DatabaseOpen(e.to_string()))?;

        Ok(Self { db })
    }

    /// Store a cache entry.
    pub fn store_entry(
        &self,
        query_hash: &[u8; 32],
        entry: &PersistedEntry,
    ) -> Result<(), PersistenceError> {
        let value =
            serde_json::to_vec(entry).map_err(|e| PersistenceError::Serialize(e.to_string()))?;
        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| PersistenceError::Write(e.to_string()))?;
        {
            let mut table = write_txn
                .open_table(ENTRIES_TABLE)
                .map_err(|e| PersistenceError::Write(e.to_string()))?;
            table
                .insert(query_hash.as_slice(), value.as_slice())
                .map_err(|e| PersistenceError::Write(e.to_string()))?;
        }
        write_txn
            .commit()
            .map_err(|e| PersistenceError::Write(e.to_string()))?;
        Ok(())
    }

    /// Load a cache entry.
    pub fn load_entry(
        &self,
        query_hash: &[u8; 32],
    ) -> Result<Option<PersistedEntry>, PersistenceError> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| PersistenceError::Read(e.to_string()))?;
        let table = read_txn
            .open_table(ENTRIES_TABLE)
            .map_err(|e| PersistenceError::Read(e.to_string()))?;
        match table
            .get(query_hash.as_slice())
            .map_err(|e| PersistenceError::Read(e.to_string()))?
        {
            Some(value) => {
                let entry: PersistedEntry = serde_json::from_slice(value.value())
                    .map_err(|e| PersistenceError::Deserialize(e.to_string()))?;
                Ok(Some(entry))
            }
            None => Ok(None),
        }
    }

    /// Remove a cache entry.
    pub fn remove_entry(&self, query_hash: &[u8; 32]) -> Result<(), PersistenceError> {
        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| PersistenceError::Write(e.to_string()))?;
        {
            let mut table = write_txn
                .open_table(ENTRIES_TABLE)
                .map_err(|e| PersistenceError::Write(e.to_string()))?;
            let _ = table
                .remove(query_hash.as_slice())
                .map_err(|e| PersistenceError::Write(e.to_string()))?;
        }
        write_txn
            .commit()
            .map_err(|e| PersistenceError::Write(e.to_string()))?;
        Ok(())
    }

    // --- Context-prefixed methods ---

    /// Build a context-prefixed key.
    fn context_key(context_id: &str, query_hash: &[u8; 32]) -> Vec<u8> {
        #[allow(clippy::arithmetic_side_effects)]
        let mut key = Vec::with_capacity(4 + context_id.len() + 1 + 32);
        key.extend_from_slice(b"ctx:");
        key.extend_from_slice(context_id.as_bytes());
        key.push(b':');
        key.extend_from_slice(query_hash);
        key
    }

    /// Store a cache entry for a specific context.
    pub fn store_entry_for_context(
        &self,
        context_id: &str,
        query_hash: &[u8; 32],
        entry: &PersistedEntry,
    ) -> Result<(), PersistenceError> {
        let key = Self::context_key(context_id, query_hash);
        let value =
            serde_json::to_vec(entry).map_err(|e| PersistenceError::Serialize(e.to_string()))?;
        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| PersistenceError::Write(e.to_string()))?;
        {
            let mut table = write_txn
                .open_table(ENTRIES_TABLE)
                .map_err(|e| PersistenceError::Write(e.to_string()))?;
            table
                .insert(key.as_slice(), value.as_slice())
                .map_err(|e| PersistenceError::Write(e.to_string()))?;
        }
        write_txn
            .commit()
            .map_err(|e| PersistenceError::Write(e.to_string()))?;
        Ok(())
    }

    /// Load a cache entry for a specific context.
    pub fn load_entry_for_context(
        &self,
        context_id: &str,
        query_hash: &[u8; 32],
    ) -> Result<Option<PersistedEntry>, PersistenceError> {
        let key = Self::context_key(context_id, query_hash);
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| PersistenceError::Read(e.to_string()))?;
        let table = read_txn
            .open_table(ENTRIES_TABLE)
            .map_err(|e| PersistenceError::Read(e.to_string()))?;
        match table
            .get(key.as_slice())
            .map_err(|e| PersistenceError::Read(e.to_string()))?
        {
            Some(value) => {
                let entry: PersistedEntry = serde_json::from_slice(value.value())
                    .map_err(|e| PersistenceError::Deserialize(e.to_string()))?;
                Ok(Some(entry))
            }
            None => Ok(None),
        }
    }

    /// Remove a cache entry for a specific context.
    pub fn remove_entry_for_context(
        &self,
        context_id: &str,
        query_hash: &[u8; 32],
    ) -> Result<(), PersistenceError> {
        let key = Self::context_key(context_id, query_hash);
        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| PersistenceError::Write(e.to_string()))?;
        {
            let mut table = write_txn
                .open_table(ENTRIES_TABLE)
                .map_err(|e| PersistenceError::Write(e.to_string()))?;
            let _ = table
                .remove(key.as_slice())
                .map_err(|e| PersistenceError::Write(e.to_string()))?;
        }
        write_txn
            .commit()
            .map_err(|e| PersistenceError::Write(e.to_string()))?;
        Ok(())
    }

    /// Get all entry hashes for a specific context.
    pub fn entries_for_context(&self, context_id: &str) -> Result<Vec<[u8; 32]>, PersistenceError> {
        let prefix = format!("ctx:{}:", context_id);
        let prefix_bytes = prefix.as_bytes();

        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| PersistenceError::Read(e.to_string()))?;
        let table = read_txn
            .open_table(ENTRIES_TABLE)
            .map_err(|e| PersistenceError::Read(e.to_string()))?;

        let mut hashes = Vec::new();
        let iter = table
            .iter()
            .map_err(|e| PersistenceError::Read(e.to_string()))?;

        for item in iter {
            let (key, _) = item.map_err(|e| PersistenceError::Read(e.to_string()))?;
            let key_bytes = key.value();
            if key_bytes.starts_with(prefix_bytes) {
                let hash_start = key_bytes.len().saturating_sub(32);
                let hash_end = key_bytes.len();
                if hash_start >= prefix_bytes.len() && hash_start.saturating_add(32) == hash_end {
                    let mut hash = [0u8; 32];
                    if let Some(slice) = key_bytes.get(hash_start..) {
                        hash.copy_from_slice(slice);
                        hashes.push(hash);
                    }
                }
            }
        }
        Ok(hashes)
    }

    /// Count entries for a specific context.
    pub fn entry_count_for_context(&self, context_id: &str) -> usize {
        self.entries_for_context(context_id)
            .map(|v| v.len())
            .unwrap_or(0)
    }

    /// Clear all entries for a specific context.
    pub fn clear_context(&self, context_id: &str) -> Result<usize, PersistenceError> {
        let prefix = format!("ctx:{}:", context_id);
        let prefix_bytes = prefix.as_bytes();

        // First collect keys to remove
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| PersistenceError::Read(e.to_string()))?;
        let table = read_txn
            .open_table(ENTRIES_TABLE)
            .map_err(|e| PersistenceError::Read(e.to_string()))?;

        let mut keys_to_remove: Vec<Vec<u8>> = Vec::new();
        let iter = table
            .iter()
            .map_err(|e| PersistenceError::Read(e.to_string()))?;
        for item in iter {
            let (key, _) = item.map_err(|e| PersistenceError::Read(e.to_string()))?;
            if key.value().starts_with(prefix_bytes) {
                keys_to_remove.push(key.value().to_vec());
            }
        }
        drop(table);
        drop(read_txn);

        // Now remove them in a write transaction
        let count = keys_to_remove.len();
        if count > 0 {
            let write_txn = self
                .db
                .begin_write()
                .map_err(|e| PersistenceError::Write(e.to_string()))?;
            {
                let mut table = write_txn
                    .open_table(ENTRIES_TABLE)
                    .map_err(|e| PersistenceError::Write(e.to_string()))?;
                for key in &keys_to_remove {
                    let _ = table
                        .remove(key.as_slice())
                        .map_err(|e| PersistenceError::Write(e.to_string()))?;
                }
            }
            write_txn
                .commit()
                .map_err(|e| PersistenceError::Write(e.to_string()))?;
        }

        Ok(count)
    }

    /// Get all stored entry hashes (for rebuilding in-memory index).
    pub fn all_entry_hashes(&self) -> Result<Vec<[u8; 32]>, PersistenceError> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| PersistenceError::Read(e.to_string()))?;
        let table = read_txn
            .open_table(ENTRIES_TABLE)
            .map_err(|e| PersistenceError::Read(e.to_string()))?;

        let mut hashes = Vec::new();
        let iter = table
            .iter()
            .map_err(|e| PersistenceError::Read(e.to_string()))?;
        for item in iter {
            let (key, _) = item.map_err(|e| PersistenceError::Read(e.to_string()))?;
            let key_bytes = key.value();
            if key_bytes.len() == 32 {
                let mut hash = [0u8; 32];
                hash.copy_from_slice(key_bytes);
                hashes.push(hash);
            }
        }
        Ok(hashes)
    }

    /// Count of stored entries.
    pub fn entry_count(&self) -> usize {
        let read_txn = match self.db.begin_read() {
            Ok(txn) => txn,
            Err(_) => return 0,
        };
        let table = match read_txn.open_table(ENTRIES_TABLE) {
            Ok(t) => t,
            Err(_) => return 0,
        };
        table.len().unwrap_or(0) as usize
    }

    /// Store upstream health state.
    pub fn store_health(
        &self,
        upstream_id: &str,
        health: &PersistedUpstreamHealth,
    ) -> Result<(), PersistenceError> {
        let value =
            serde_json::to_vec(health).map_err(|e| PersistenceError::Serialize(e.to_string()))?;
        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| PersistenceError::Write(e.to_string()))?;
        {
            let mut table = write_txn
                .open_table(HEALTH_TABLE)
                .map_err(|e| PersistenceError::Write(e.to_string()))?;
            table
                .insert(upstream_id, value.as_slice())
                .map_err(|e| PersistenceError::Write(e.to_string()))?;
        }
        write_txn
            .commit()
            .map_err(|e| PersistenceError::Write(e.to_string()))?;
        Ok(())
    }

    /// Load upstream health state.
    pub fn load_health(
        &self,
        upstream_id: &str,
    ) -> Result<Option<PersistedUpstreamHealth>, PersistenceError> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| PersistenceError::Read(e.to_string()))?;
        let table = read_txn
            .open_table(HEALTH_TABLE)
            .map_err(|e| PersistenceError::Read(e.to_string()))?;
        match table
            .get(upstream_id)
            .map_err(|e| PersistenceError::Read(e.to_string()))?
        {
            Some(value) => {
                let health: PersistedUpstreamHealth = serde_json::from_slice(value.value())
                    .map_err(|e| PersistenceError::Deserialize(e.to_string()))?;
                Ok(Some(health))
            }
            None => Ok(None),
        }
    }

    /// Load all upstream health states.
    pub fn all_health_states(&self) -> Result<Vec<PersistedUpstreamHealth>, PersistenceError> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| PersistenceError::Read(e.to_string()))?;
        let table = read_txn
            .open_table(HEALTH_TABLE)
            .map_err(|e| PersistenceError::Read(e.to_string()))?;

        let mut states = Vec::new();
        let iter = table
            .iter()
            .map_err(|e| PersistenceError::Read(e.to_string()))?;
        for item in iter {
            let (_, value) = item.map_err(|e| PersistenceError::Read(e.to_string()))?;
            let health: PersistedUpstreamHealth = serde_json::from_slice(value.value())
                .map_err(|e| PersistenceError::Deserialize(e.to_string()))?;
            states.push(health);
        }
        Ok(states)
    }

    /// Store metadata value.
    pub fn store_meta(&self, key: &str, value: &[u8]) -> Result<(), PersistenceError> {
        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| PersistenceError::Write(e.to_string()))?;
        {
            let mut table = write_txn
                .open_table(META_TABLE)
                .map_err(|e| PersistenceError::Write(e.to_string()))?;
            table
                .insert(key, value)
                .map_err(|e| PersistenceError::Write(e.to_string()))?;
        }
        write_txn
            .commit()
            .map_err(|e| PersistenceError::Write(e.to_string()))?;
        Ok(())
    }

    /// Load metadata value.
    pub fn load_meta(&self, key: &str) -> Result<Option<Vec<u8>>, PersistenceError> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| PersistenceError::Read(e.to_string()))?;
        let table = read_txn
            .open_table(META_TABLE)
            .map_err(|e| PersistenceError::Read(e.to_string()))?;
        match table
            .get(key)
            .map_err(|e| PersistenceError::Read(e.to_string()))?
        {
            Some(value) => Ok(Some(value.value().to_vec())),
            None => Ok(None),
        }
    }

    /// Record last flush timestamp.
    pub fn record_flush(&self) -> Result<(), PersistenceError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.store_meta("last_flush_ms", &now.to_le_bytes())
    }

    /// Get last flush timestamp.
    pub fn last_flush_time(&self) -> Result<Option<u64>, PersistenceError> {
        match self.load_meta("last_flush_ms")? {
            Some(bytes) if bytes.len() == 8 => {
                let mut arr = [0u8; 8];
                arr.copy_from_slice(&bytes);
                Ok(Some(u64::from_le_bytes(arr)))
            }
            _ => Ok(None),
        }
    }

    /// Flush writes (redb auto-commits, so this records the flush timestamp).
    pub fn flush(&self) -> Result<(), PersistenceError> {
        self.record_flush()?;
        Ok(())
    }

    /// Clear all entries (for testing or reset).
    pub fn clear_entries(&self) -> Result<usize, PersistenceError> {
        let count = self.entry_count();
        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| PersistenceError::Write(e.to_string()))?;
        {
            // Delete and recreate the table to clear it
            write_txn
                .delete_table(ENTRIES_TABLE)
                .map_err(|e| PersistenceError::Write(e.to_string()))?;
            let _ = write_txn
                .open_table(ENTRIES_TABLE)
                .map_err(|e| PersistenceError::Write(e.to_string()))?;
        }
        write_txn
            .commit()
            .map_err(|e| PersistenceError::Write(e.to_string()))?;
        Ok(count)
    }

    /// Clear all data (entries, health, metadata).
    pub fn clear_all(&self) -> Result<(), PersistenceError> {
        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| PersistenceError::Write(e.to_string()))?;
        {
            write_txn
                .delete_table(ENTRIES_TABLE)
                .map_err(|e| PersistenceError::Write(e.to_string()))?;
            write_txn
                .delete_table(HEALTH_TABLE)
                .map_err(|e| PersistenceError::Write(e.to_string()))?;
            write_txn
                .delete_table(META_TABLE)
                .map_err(|e| PersistenceError::Write(e.to_string()))?;
            // Recreate tables
            let _ = write_txn
                .open_table(ENTRIES_TABLE)
                .map_err(|e| PersistenceError::Write(e.to_string()))?;
            let _ = write_txn
                .open_table(HEALTH_TABLE)
                .map_err(|e| PersistenceError::Write(e.to_string()))?;
            let _ = write_txn
                .open_table(META_TABLE)
                .map_err(|e| PersistenceError::Write(e.to_string()))?;
        }
        write_txn
            .commit()
            .map_err(|e| PersistenceError::Write(e.to_string()))?;
        Ok(())
    }

    /// Get database size on disk (approximate).
    pub fn size_on_disk(&self) -> u64 {
        // redb doesn't have a direct size method; use file metadata
        0
    }

    // --- Context metadata persistence ---

    /// Store context metadata.
    pub fn store_context(&self, metadata: &ContextMetadata) -> Result<(), PersistenceError> {
        let key = format!("context:{}", metadata.id);
        let value =
            serde_json::to_vec(metadata).map_err(|e| PersistenceError::Serialize(e.to_string()))?;
        self.store_meta(&key, &value)
    }

    /// Load context metadata by ID.
    pub fn load_context(
        &self,
        context_id: &str,
    ) -> Result<Option<ContextMetadata>, PersistenceError> {
        let key = format!("context:{}", context_id);
        match self.load_meta(&key)? {
            Some(bytes) => {
                let metadata: ContextMetadata = serde_json::from_slice(&bytes)
                    .map_err(|e| PersistenceError::Deserialize(e.to_string()))?;
                Ok(Some(metadata))
            }
            None => Ok(None),
        }
    }

    /// Load all persisted context metadata.
    pub fn all_contexts(&self) -> Result<Vec<ContextMetadata>, PersistenceError> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| PersistenceError::Read(e.to_string()))?;
        let table = read_txn
            .open_table(META_TABLE)
            .map_err(|e| PersistenceError::Read(e.to_string()))?;

        let mut contexts = Vec::new();
        let iter = table
            .iter()
            .map_err(|e| PersistenceError::Read(e.to_string()))?;
        for item in iter {
            let (key, value) = item.map_err(|e| PersistenceError::Read(e.to_string()))?;
            if key.value().starts_with("context:") {
                let metadata: ContextMetadata = serde_json::from_slice(value.value())
                    .map_err(|e| PersistenceError::Deserialize(e.to_string()))?;
                contexts.push(metadata);
            }
        }
        Ok(contexts)
    }

    /// Remove context metadata.
    pub fn remove_context(&self, context_id: &str) -> Result<(), PersistenceError> {
        let key = format!("context:{}", context_id);
        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| PersistenceError::Write(e.to_string()))?;
        {
            let mut table = write_txn
                .open_table(META_TABLE)
                .map_err(|e| PersistenceError::Write(e.to_string()))?;
            let _ = table
                .remove(key.as_str())
                .map_err(|e| PersistenceError::Write(e.to_string()))?;
        }
        write_txn
            .commit()
            .map_err(|e| PersistenceError::Write(e.to_string()))?;
        Ok(())
    }

    /// Count of persisted contexts.
    pub fn context_count(&self) -> usize {
        self.all_contexts().map(|v| v.len()).unwrap_or(0)
    }
}

/// Statistics about the persistent cache.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PersistenceStats {
    /// Number of entries in persistent storage.
    pub entry_count: usize,
    /// Number of upstream health records.
    pub health_record_count: usize,
    /// Size on disk in bytes.
    pub size_on_disk: u64,
    /// Last flush timestamp (Unix millis).
    pub last_flush_ms: Option<u64>,
}

impl PersistentCache {
    /// Get persistence statistics.
    pub fn stats(&self) -> PersistenceStats {
        let health_count = {
            let read_txn = self.db.begin_read().ok();
            read_txn
                .and_then(|txn| {
                    txn.open_table(HEALTH_TABLE)
                        .ok()
                        .map(|t| t.len().unwrap_or(0) as usize)
                })
                .unwrap_or(0)
        };

        PersistenceStats {
            entry_count: self.entry_count(),
            health_record_count: health_count,
            size_on_disk: self.size_on_disk(),
            last_flush_ms: self.last_flush_time().ok().flatten(),
        }
    }
}

/// Helper to convert current time to Unix millis.
pub fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "tests/persistence_tests.rs"]
mod tests;
