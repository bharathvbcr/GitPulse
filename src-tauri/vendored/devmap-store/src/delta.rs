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

/// A read or write of the stacked delta store failed with `SQLITE_CORRUPT`.
///
/// Carried as error context, added where the delta's own connection failed, so
/// a caller that sees a corrupt error from a stacked lane knows which of the
/// lane's two SQLite files it came from: this one, or (anything untagged) the
/// shared base snapshot. Attribution by probing afterwards would blame the
/// wrong file whenever the probe happened not to touch the damaged page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeltaDamaged {
    pub path: PathBuf,
}

impl std::fmt::Display for DeltaDamaged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the stacked delta store {} is damaged",
            self.path.display()
        )
    }
}

impl std::error::Error for DeltaDamaged {}

/// Tag `result`'s error as damage to the delta at `path`, if it is damage.
fn tag_damage<T>(path: &Path, result: Result<T>) -> Result<T> {
    result.map_err(|error| {
        if crate::Store::is_database_corrupt(&error)
            && error.downcast_ref::<DeltaDamaged>().is_none()
        {
            error.context(DeltaDamaged {
                path: path.to_path_buf(),
            })
        } else {
            error
        }
    })
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

/// The `-journal`, `-wal` and `-shm` beside `db` that are symlinks.
fn symlinked_sidecars(db: &Path) -> Vec<PathBuf> {
    ["-journal", "-wal", "-shm"]
        .into_iter()
        .map(|suffix| sidecar(db, suffix))
        .filter(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()))
        .collect()
}

impl DeltaStore {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        tag_damage(path.as_ref(), Self::open_untagged(path.as_ref()))
    }

    fn open_untagged(path: &Path) -> Result<Self> {
        let path = path.to_path_buf();
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
    /// statements, no journal-mode change. `None` when the file does not exist.
    ///
    /// An ordinary read-only connection, never an immutable one: a live delta is
    /// rewritten by the detached `seed --sync-delta`, and an immutable open takes
    /// no lock, so it could read a torn page or a stale file. The delta is kept
    /// in rollback-journal mode (see `configure`), so this open takes SQLite's
    /// shared lock, creates no `-shm`/`-wal` sidecar and needs no write access
    /// to the state directory. A delta left in WAL mode by an older build cannot
    /// be read without creating those sidecars: in a read-only directory that is
    /// an error naming the reason, never an empty or stale answer.
    pub fn open_read_only<P: AsRef<Path>>(path: P) -> Result<Option<Self>> {
        tag_damage(path.as_ref(), Self::open_read_only_untagged(path.as_ref()))
    }

    fn open_read_only_untagged(path: &Path) -> Result<Option<Self>> {
        let path = path.to_path_buf();
        if !path.is_file() {
            return Ok(None);
        }
        if let Some(link) = symlinked_sidecars(&path).into_iter().next() {
            bail!(
                "delta store {} cannot be read: {} is a symlink, which SQLite will not follow; \
                 run `devmap seed --sync-delta`, which unlinks it",
                path.display(),
                link.display()
            );
        }
        let conn = Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("cannot open delta store {} read-only", path.display()))?;
        conn.execute_batch("PRAGMA trusted_schema = OFF;")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // SQLite opens lazily: read now, so a refusal is reported here.
        conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|err| {
            let code = match &err {
                rusqlite::Error::SqliteFailure(e, _) => Some(*e),
                _ => None,
            };
            let why = match code {
                Some(e) if e.extended_code == rusqlite::ffi::SQLITE_READONLY_ROLLBACK => {
                    "an interrupted writer left a hot rollback journal that a read-only open \
                         cannot roll back; run `devmap seed --sync-delta`"
                }
                Some(e)
                    if matches!(
                        e.code,
                        rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                    ) =>
                {
                    "another process holds a write lock on it; wait for that writer (often a \
                         running `devmap seed --sync-delta`) to finish and retry"
                }
                Some(e) if e.code == rusqlite::ErrorCode::ReadOnly => {
                    "a WAL-mode delta from an earlier build in a read-only state directory is \
                         unreadable, not empty"
                }
                _ => "the database could not be read",
            };
            anyhow::Error::new(err).context(format!(
                "delta store {} cannot be read ({why})",
                path.display()
            ))
        })?;
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
        // SQLite refuses a journal or WAL sidecar that is a symlink, which
        // strands the delta. A real sidecar is never a link: unlink it (the link
        // only, never its target) and say so.
        for link in symlinked_sidecars(path) {
            std::fs::remove_file(&link)
                .with_context(|| format!("cannot unlink the symlinked {}", link.display()))?;
            eprintln!(
                "devmap: unlinked {}, a symlink where SQLite keeps a delta sidecar (its target was not touched)",
                link.display()
            );
        }
        Self::open(path).map(Some)
    }

    fn configure(conn: &Connection) -> Result<()> {
        conn.execute_batch("PRAGMA trusted_schema = OFF; PRAGMA foreign_keys = ON;")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // Leaving WAL needs exclusive access: read the mode back, never assume it.
        let mode: String = conn
            .query_row("PRAGMA journal_mode = DELETE", [], |r| r.get(0))
            .context("cannot switch the delta store to rollback-journal mode (another connection holds it)")?;
        if !mode.eq_ignore_ascii_case("delete") {
            bail!("delta store stayed in {mode} journal mode: another connection holds it open");
        }
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
        tag_damage(&self.path, self.dirty_file_count_untagged())
    }

    fn dirty_file_count_untagged(&self) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("delta store mutex poisoned"))?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM delta_files", [], |r| r.get(0))?;
        usize::try_from(count).context("negative delta_files count")
    }

    pub fn file_hash(&self, rel_path: &str) -> Result<Option<(String, bool)>> {
        tag_damage(&self.path, self.file_hash_untagged(rel_path))
    }

    fn file_hash_untagged(&self, rel_path: &str) -> Result<Option<(String, bool)>> {
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
        tag_damage(
            &self.path,
            self.upsert_extracted_file_untagged(
                rel_path,
                content_hash,
                extraction,
                edges,
                updated_at_ms,
            ),
        )
    }

    fn upsert_extracted_file_untagged(
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
        tag_damage(
            &self.path,
            self.tombstone_file_untagged(rel_path, tombstone_marker, updated_at_ms),
        )
    }

    fn tombstone_file_untagged(
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
        tag_damage(&self.path, self.remove_clean_file_untagged(rel_path))
    }

    fn remove_clean_file_untagged(&self, rel_path: &str) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("delta store mutex poisoned"))?;
        conn.execute("DELETE FROM delta_files WHERE path = ?1", params![rel_path])?;
        Ok(())
    }

    pub fn load_snapshot(&self) -> Result<DeltaSnapshot> {
        tag_damage(&self.path, self.load_snapshot_untagged())
    }

    fn load_snapshot_untagged(&self) -> Result<DeltaSnapshot> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("devmap-delta-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// A read of `delta.sqlite` writes nothing: after its last writer closes (and
    /// checkpoints) no `-wal`/`-shm` exists, and opening it read-only must not
    /// create them, even when the directory is writable; a read-only directory
    /// must still open.
    #[cfg(unix)]
    #[test]
    fn open_read_only_creates_no_sidecar_and_opens_in_a_read_only_directory() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("readonly");
        let path = dir.join("delta.sqlite");
        {
            let writer = DeltaStore::open(&path).unwrap();
            writer
                .tombstone_file("src/a.rs", "CAS_TOMBSTONE", 1)
                .unwrap();
        }
        assert_eq!(
            names_in(&dir),
            vec!["delta.sqlite".to_string()],
            "fixture: a closed writer leaves one file"
        );

        let reader = DeltaStore::open_read_only(&path).unwrap().expect("exists");
        assert_eq!(reader.dirty_file_count().unwrap(), 1);
        assert_eq!(reader.load_snapshot().unwrap().files.len(), 1);
        drop(reader);
        assert_eq!(
            names_in(&dir),
            vec!["delta.sqlite".to_string()],
            "a read created a sidecar"
        );

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let reader = DeltaStore::open_read_only(&path);
        let rows = reader
            .as_ref()
            .ok()
            .and_then(|r| r.as_ref())
            .map(|r| r.dirty_file_count().unwrap());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            reader.is_ok(),
            "a read-only directory must still open: {:?}",
            reader.err()
        );
        assert_eq!(rows, Some(1));
        assert_eq!(names_in(&dir), vec!["delta.sqlite".to_string()]);
        assert!(DeltaStore::open_read_only(dir.join("missing.sqlite"))
            .unwrap()
            .is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// While a writer holds the database open its `-wal` exists, and a read
    /// must see that writer's committed rows (an immutable open would not).
    #[test]
    fn open_read_only_sees_rows_a_live_writer_has_committed() {
        let dir = temp_dir("livewriter");
        let path = dir.join("delta.sqlite");
        let writer = DeltaStore::open(&path).unwrap();
        writer
            .tombstone_file("src/a.rs", "CAS_TOMBSTONE", 1)
            .unwrap();
        writer
            .tombstone_file("src/b.rs", "CAS_TOMBSTONE", 2)
            .unwrap();
        let reader = DeltaStore::open_read_only(&path).unwrap().expect("exists");
        assert_eq!(reader.dirty_file_count().unwrap(), 2);
        drop(reader);
        drop(writer);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A reader that opened while no writer was running must follow a writer
    /// that then starts, commits and closes: an immutable open would assume the
    /// file never changes and could serve the old page, a torn one, or an error.
    #[test]
    fn open_read_only_follows_a_writer_that_starts_after_the_reader_opened() {
        let dir = temp_dir("lateWriter");
        let path = dir.join("delta.sqlite");
        {
            let writer = DeltaStore::open(&path).unwrap();
            writer
                .tombstone_file("src/a.rs", "CAS_TOMBSTONE", 1)
                .unwrap();
        }
        let reader = DeltaStore::open_read_only(&path).unwrap().expect("exists");
        assert_eq!(reader.dirty_file_count().unwrap(), 1);
        {
            let writer = DeltaStore::open(&path).unwrap();
            for i in 0..50 {
                writer
                    .tombstone_file(&format!("src/late_{i}.rs"), "CAS_TOMBSTONE", 10 + i)
                    .unwrap();
            }
        }
        assert_eq!(
            reader.dirty_file_count().unwrap(),
            51,
            "a reader opened before the writer must see what the writer committed"
        );
        assert_eq!(reader.load_snapshot().unwrap().files.len(), 51);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A delta an earlier build left in WAL mode, in a read-only directory: it
    /// is either read correctly or refused with a reason, never read stale or
    /// empty (no immutable fallback that would miss a live `-wal`).
    #[cfg(unix)]
    #[test]
    fn a_legacy_wal_delta_in_a_read_only_directory_is_read_correctly_or_refused_never_stale() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("legacywal");
        let path = dir.join("delta.sqlite");
        // A live writer: its rows sit in the `-wal`, not in the main file, so an
        // immutable read of the main file alone would answer 0.
        let conn = Connection::open(&path).unwrap();
        let _: String = conn
            .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
            .unwrap();
        conn.execute_batch("PRAGMA wal_autocheckpoint = 0;")
            .unwrap();
        conn.execute_batch(DELTA_SCHEMA_DDL).unwrap();
        conn.execute(
            "INSERT INTO delta_files (path, content_hash, tombstoned, extraction_json, updated_at_ms)
             VALUES ('src/a.rs', 'x', 1, NULL, 1)",
            [],
        )
        .unwrap();
        assert!(
            dir.join("delta.sqlite-wal").is_file(),
            "the rows must be in the -wal"
        );
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let opened = DeltaStore::open_read_only(&path);
        let rows = match &opened {
            Ok(Some(store)) => Some(store.dirty_file_count().unwrap()),
            _ => None,
        };
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        drop(conn);
        match opened {
            Ok(Some(_)) => assert_eq!(rows, Some(1), "an answer must be the committed rows"),
            Ok(None) => panic!("the delta exists"),
            Err(err) => {
                let text = format!("{err:#}");
                assert!(
                    text.contains("read-only") && text.contains("not empty"),
                    "{text}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Leave `dir/delta.sqlite` + a hot `-journal`, as a writer killed mid-commit
    /// does: the copy is taken while a spilling transaction is open, then the
    /// writer is dropped, so nothing in `dir` holds a lock.
    pub(crate) fn hot_journal_delta(dir: &Path) -> PathBuf {
        let src = temp_dir("hotsrc");
        let path = dir.join("delta.sqlite");
        std::fs::create_dir_all(dir).unwrap();
        {
            let w = DeltaStore::open(src.join("delta.sqlite")).unwrap();
            w.tombstone_file("src/a.rs", "CAS_TOMBSTONE", 1).unwrap();
        }
        let mut conn = Connection::open(src.join("delta.sqlite")).unwrap();
        conn.execute_batch("PRAGMA cache_size = 1;").unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        for i in 0..400 {
            tx.execute(
                "INSERT INTO delta_files (path, content_hash, tombstoned, extraction_json, updated_at_ms)
                 VALUES (?1, 'x', 1, ?2, 1)",
                params![format!("src/spill_{i}.rs"), "y".repeat(2000)],
            )
            .unwrap();
        }
        assert!(
            src.join("delta.sqlite-journal").metadata().unwrap().len() > 0,
            "fixture: a live journal"
        );
        std::fs::copy(src.join("delta.sqlite"), &path).unwrap();
        std::fs::copy(
            src.join("delta.sqlite-journal"),
            dir.join("delta.sqlite-journal"),
        )
        .unwrap();
        drop(tx);
        drop(conn);
        let _ = std::fs::remove_dir_all(&src);
        path
    }

    /// FIX-A: a read-only open cannot roll a hot journal back; it names the
    /// cause and the recovery command, and a writable open recovers.
    #[test]
    fn a_hot_rollback_journal_is_refused_read_only_with_the_recovery_command_and_a_writer_recovers()
    {
        let dir = temp_dir("hotjournal");
        let path = hot_journal_delta(&dir);
        let err = format!(
            "{:#}",
            DeltaStore::open_read_only(&path).expect_err("a hot journal is refused")
        );
        assert!(err.contains("hot rollback journal"), "{err}");
        assert!(err.contains("devmap seed --sync-delta"), "{err}");
        assert!(!err.contains("WAL-mode delta"), "wrong cause named: {err}");
        drop(DeltaStore::open(&path).unwrap());
        let reader = DeltaStore::open_read_only(&path).unwrap().expect("exists");
        assert_eq!(
            reader.dirty_file_count().unwrap(),
            1,
            "the interrupted transaction rolled back"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// FIX-E: leaving WAL needs exclusive access; a legacy WAL delta another
    /// connection holds open is refused, not silently kept in WAL.
    #[test]
    fn a_delta_that_cannot_leave_wal_is_refused() {
        let dir = temp_dir("stayswal");
        let path = dir.join("delta.sqlite");
        let held = Connection::open(&path).unwrap();
        let _: String = held
            .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
            .unwrap();
        held.execute_batch(DELTA_SCHEMA_DDL).unwrap();
        held.execute_batch("BEGIN; SELECT COUNT(*) FROM delta_files;")
            .unwrap();
        let err = format!("{:#}", DeltaStore::open(&path).expect_err("must refuse"));
        assert!(err.contains("rollback-journal mode"), "{err}");
        held.execute_batch("COMMIT;").unwrap();
        drop(held);
        DeltaStore::open(&path).expect("once nothing holds it, it converts");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
