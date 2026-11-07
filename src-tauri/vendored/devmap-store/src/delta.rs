//! Per-worktree SQLite delta overlay storage (`delta.sqlite`) for `StackedStore`.
//!
//! Stores uncommitted or diverged file extractions, declared symbols, and
//! intra-worktree edges for a stacked worktree lane without mutating the shared
//! immutable `base.sqlite` snapshot or poisoning any shared CAS (`F4`, `OQ1`,
//! `OQ2`).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use devmap_extract::model::Extraction;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::seed::sidecar;

const DELTA_SCHEMA_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS delta_files (
    path            TEXT PRIMARY KEY,
    content_hash    TEXT NOT NULL,
    tombstoned      INTEGER NOT NULL CHECK (tombstoned IN (0, 1)),
    extraction_json TEXT,
    updated_at_ms   INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS delta_symbols (
    path           TEXT NOT NULL,
    qualified_name TEXT NOT NULL,
    name           TEXT NOT NULL,
    kind           TEXT NOT NULL,
    span_start     INTEGER NOT NULL,
    span_end       INTEGER NOT NULL,
    is_exported    INTEGER NOT NULL CHECK (is_exported IN (0, 1)),
    content_hash   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (path, qualified_name),
    FOREIGN KEY (path) REFERENCES delta_files(path) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_delta_symbols_name ON delta_symbols(name);

CREATE TABLE IF NOT EXISTS delta_edges (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    path          TEXT NOT NULL,
    source_symbol TEXT NOT NULL,
    target_symbol TEXT NOT NULL,
    edge_kind     TEXT NOT NULL,
    confidence    INTEGER NOT NULL CHECK (confidence BETWEEN 0 AND 100),
    FOREIGN KEY (path) REFERENCES delta_files(path) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_delta_edges_source ON delta_edges(source_symbol);
CREATE INDEX IF NOT EXISTS idx_delta_edges_target ON delta_edges(target_symbol);
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaFileRecord {
    pub path: String,
    pub content_hash: String,
    pub tombstoned: bool,
    pub extraction: Option<Extraction>,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeltaSymbolRecord {
    pub path: String,
    pub qualified_name: String,
    pub name: String,
    pub kind: String,
    pub span_start: usize,
    pub span_end: usize,
    pub is_exported: bool,
    pub content_hash: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeltaEdgeRecord {
    pub path: String,
    pub source_symbol: String,
    pub target_symbol: String,
    pub edge_kind: String,
    pub confidence: u8,
}

#[derive(Debug, Clone, Default)]
pub struct DeltaSnapshot {
    pub files: Vec<DeltaFileRecord>,
    pub symbols: Vec<DeltaSymbolRecord>,
    pub edges: Vec<DeltaEdgeRecord>,
}

pub struct DeltaStore {
    conn: Mutex<Connection>,
    path: PathBuf,
}

impl std::fmt::Debug for DeltaStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeltaStore")
            .field("path", &self.path)
            .finish()
    }
}

impl DeltaStore {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        let conn = Connection::open(&path)
            .with_context(|| format!("cannot open delta store {}", path.display()))?;
        Self::configure(&conn)?;
        conn.execute_batch(DELTA_SCHEMA_DDL)
            .with_context(|| format!("cannot initialize delta schema at {}", path.display()))?;
        Ok(Self {
            conn: Mutex::new(conn),
            path,
        })
    }

    /// Open an existing `delta.sqlite` for reading: no creation, no schema
    /// statements, no journal-mode change, and zero `-wal`/`-shm` sidecars.
    /// `None` when the file does not exist.
    pub fn open_read_only<P: AsRef<Path>>(path: P) -> Result<Option<Self>> {
        let path = path.as_ref().to_path_buf();
        if !path.is_file() {
            return Ok(None);
        }
        let conn = Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("cannot open delta store {} read-only", path.display()))?;
        conn.execute_batch("PRAGMA trusted_schema = OFF;")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Some(Self {
            conn: Mutex::new(conn),
            path,
        }))
    }

    pub fn open_existing<P: AsRef<Path>>(path: P) -> Result<Option<Self>> {
        let path = path.as_ref();
        if !path.is_file() {
            return Ok(None);
        }
        Self::open(path).map(Some)
    }

    fn configure(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "PRAGMA trusted_schema = OFF;
             PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = DELETE;
             PRAGMA synchronous = NORMAL;",
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Total bytes on disk of `delta.sqlite` plus any active `-wal` sidecar.
    pub fn bytes_on_disk(&self) -> u64 {
        let main = std::fs::symlink_metadata(&self.path)
            .map(|m| m.len())
            .unwrap_or(0);
        let wal = std::fs::symlink_metadata(sidecar(&self.path, "-wal"))
            .map(|m| m.len())
            .unwrap_or(0);
        main.saturating_add(wal)
    }

    pub fn dirty_file_count(&self) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("delta store mutex poisoned"))?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM delta_files", [], |r| r.get(0))?;
        usize::try_from(count).context("negative delta_files count")
    }

    pub fn file_hash(&self, rel_path: &str) -> Result<Option<(String, bool)>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("delta store mutex poisoned"))?;
        conn.query_row(
            "SELECT content_hash, tombstoned FROM delta_files WHERE path = ?1",
            params![rel_path],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? != 0)),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn upsert_extracted_file(
        &self,
        rel_path: &str,
        content_hash: &str,
        extraction: &Extraction,
        edges: &[DeltaEdgeRecord],
        updated_at_ms: u64,
    ) -> Result<()> {
        if rel_path.is_empty() {
            bail!("delta file path must not be empty");
        }
        let extraction_json =
            serde_json::to_string(extraction).context("serialize delta Extraction")?;
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("delta store mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM delta_files WHERE path = ?1", params![rel_path])?;
        tx.execute(
            "INSERT INTO delta_files (path, content_hash, tombstoned, extraction_json, updated_at_ms)
             VALUES (?1, ?2, 0, ?3, ?4)",
            params![rel_path, content_hash, extraction_json, updated_at_ms as i64],
        )?;
        for sym in &extraction.symbols {
            tx.execute(
                "INSERT OR REPLACE INTO delta_symbols
                    (path, qualified_name, name, kind, span_start, span_end, is_exported, content_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    rel_path,
                    sym.qualified_name,
                    sym.name,
                    sym.kind.as_str(),
                    sym.span.start_byte as i64,
                    sym.span.end_byte as i64,
                    sym.is_exported as i32,
                    extraction.content_hash as i64,
                ],
            )?;
        }
        for edge in edges {
            tx.execute(
                "INSERT INTO delta_edges (path, source_symbol, target_symbol, edge_kind, confidence)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    rel_path,
                    edge.source_symbol,
                    edge.target_symbol,
                    edge.edge_kind,
                    i64::from(edge.confidence.min(100)),
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn tombstone_file(
        &self,
        rel_path: &str,
        tombstone_marker: &str,
        updated_at_ms: u64,
    ) -> Result<()> {
        if rel_path.is_empty() {
            bail!("delta file path must not be empty");
        }
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("delta store mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM delta_files WHERE path = ?1", params![rel_path])?;
        tx.execute(
            "INSERT INTO delta_files (path, content_hash, tombstoned, extraction_json, updated_at_ms)
             VALUES (?1, ?2, 1, NULL, ?3)",
            params![rel_path, tombstone_marker, updated_at_ms as i64],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn remove_clean_file(&self, rel_path: &str) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("delta store mutex poisoned"))?;
        conn.execute("DELETE FROM delta_files WHERE path = ?1", params![rel_path])?;
        Ok(())
    }

    pub fn load_snapshot(&self) -> Result<DeltaSnapshot> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("delta store mutex poisoned"))?;
        let files = {
            let mut stmt = conn.prepare(
                "SELECT path, content_hash, tombstoned, extraction_json, updated_at_ms
                 FROM delta_files ORDER BY path",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)? != 0,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)? as u64,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (path, content_hash, tombstoned, extraction_json, updated_at_ms) = row?;
                let extraction = match extraction_json {
                    Some(raw) => Some(
                        serde_json::from_str::<Extraction>(&raw)
                            .with_context(|| format!("corrupt delta extraction_json for {path}"))?,
                    ),
                    None => None,
                };
                out.push(DeltaFileRecord {
                    path,
                    content_hash,
                    tombstoned,
                    extraction,
                    updated_at_ms,
                });
            }
            out
        };

        let symbols = {
            let mut stmt = conn.prepare(
                "SELECT path, qualified_name, name, kind, span_start, span_end, is_exported, content_hash
                 FROM delta_symbols ORDER BY path, span_start, qualified_name",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(DeltaSymbolRecord {
                    path: row.get(0)?,
                    qualified_name: row.get(1)?,
                    name: row.get(2)?,
                    kind: row.get(3)?,
                    span_start: usize::try_from(row.get::<_, i64>(4)?.max(0)).unwrap_or(0),
                    span_end: usize::try_from(row.get::<_, i64>(5)?.max(0)).unwrap_or(0),
                    is_exported: row.get::<_, i64>(6)? != 0,
                    content_hash: row.get::<_, i64>(7)? as u64,
                })
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };

        let edges = {
            let mut stmt = conn.prepare(
                "SELECT path, source_symbol, target_symbol, edge_kind, confidence
                 FROM delta_edges ORDER BY id",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(DeltaEdgeRecord {
                    path: row.get(0)?,
                    source_symbol: row.get(1)?,
                    target_symbol: row.get(2)?,
                    edge_kind: row.get(3)?,
                    confidence: u8::try_from(row.get::<_, i64>(4)?.clamp(0, 100)).unwrap_or(0),
                })
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };

        Ok(DeltaSnapshot {
            files,
            symbols,
            edges,
        })
    }
}
