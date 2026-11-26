//! Retention for the `<profile>.pre-schema-<version>.bak` copies a schema
//! upgrade leaves beside the profile.
//!
//! Every upgrade writes a whole copy of the profile (`VACUUM INTO`) and, before
//! this module, nothing ever removed one: a profile that moved through four
//! schema versions held four extra copies of itself. The rules:
//!
//! * Pruning runs only right after this call wrote a new backup and verified it
//!   (see `backup_profile`). A plain open, a read, and the resumption of an
//!   interrupted upgrade (whose backup already exists) never delete anything.
//! * The newest `keep` backups are kept, ordered by the schema version in the
//!   name, numerically. The backup just written always counts and is never
//!   removed, so `keep` is at least one by construction.
//! * A backup that fails verification (cannot be opened read-only, fails
//!   `PRAGMA quick_check`, or holds a different schema version than its name
//!   says) never counts toward `keep`, and is left in place and reported rather
//!   than deleted: it may be the only evidence of what went wrong.
//! * Only `<profile>.pre-schema-<digits>.bak` regular files are candidates. The
//!   `pre-tasks-retire` copy, `*.partial` files and anything else are not.
//! * Each removal is named on stderr and in the returned [`PruneReport`].
//! * A file the upgrade moved aside because it did not verify, or verified but
//!   no longer matched the profile it was meant to protect
//!   (`<name>.bak.invalid-<unix seconds>[-n]`), is not a candidate: it is never
//!   counted and never removed. The upgrade names it in
//!   [`PruneReport::unverified`] as `...; moved aside to <path>`, and the
//!   tasks-retire path in `Retired::moved_aside`. At most [`MAX_MOVED_ASIDE`]
//!   may sit beside one backup name: past that the upgrade fails closed rather
//!   than move another, and [`moved_aside_copies`] lists them for an operator.

use rusqlite::{Connection, OpenFlags};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

/// Environment variable that sets the retention for every handle opened by this
/// process: a positive integer (keep that many) or `all` (keep every backup).
pub const KEEP_BACKUPS_ENV: &str = "DC_STORE_KEEP_BACKUPS";

/// How many `pre-schema` backups are kept when nothing says otherwise.
pub const DEFAULT_KEPT_BACKUPS: usize = 2;

/// How many `pre-schema` backups survive an upgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupRetention {
    /// Keep the newest `n` verified backups, the one just written among them.
    Keep(NonZeroUsize),
    /// Never remove a backup.
    KeepAll,
}

impl Default for BackupRetention {
    fn default() -> Self {
        Self::Keep(NonZeroUsize::new(DEFAULT_KEPT_BACKUPS).expect("the default is positive"))
    }
}

impl BackupRetention {
    /// Keep the newest `n`; `None` for zero, which would delete the backup an
    /// upgrade has only just made.
    pub fn keep(n: usize) -> Option<Self> {
        NonZeroUsize::new(n).map(Self::Keep)
    }

    /// Reads a setting: `all` (any case) or a positive integer.
    pub fn parse(setting: &str) -> Result<Self, String> {
        let setting = setting.trim();
        if setting.eq_ignore_ascii_case("all") {
            return Ok(Self::KeepAll);
        }
        // Digits only: `usize::parse` would also take a leading `+`.
        let digits = !setting.is_empty() && setting.bytes().all(|b| b.is_ascii_digit());
        match setting.parse::<usize>().ok().filter(|_| digits) {
            Some(n) => Self::keep(n).ok_or_else(|| {
                "0 would delete the backup an upgrade has just made; use a positive number or \"all\""
                    .to_string()
            }),
            None => Err(format!(
                "{setting:?} is neither a positive number nor \"all\""
            )),
        }
    }

    /// The process-wide setting from [`KEEP_BACKUPS_ENV`]. An unset variable is
    /// the default. A value that cannot be read keeps every backup, loudly: a
    /// retention setting nobody can parse must not become permission to delete.
    pub(crate) fn from_environment() -> Self {
        match std::env::var(KEEP_BACKUPS_ENV) {
            Err(std::env::VarError::NotPresent) => Self::default(),
            Err(std::env::VarError::NotUnicode(_)) => {
                eprintln!("dc-store: {KEEP_BACKUPS_ENV} is not valid UTF-8; keeping every backup");
                Self::KeepAll
            }
            Ok(value) => Self::parse(&value).unwrap_or_else(|reason| {
                eprintln!("dc-store: {KEEP_BACKUPS_ENV}: {reason}; keeping every backup");
                Self::KeepAll
            }),
        }
    }
}

/// How many moved-aside copies (`<backup name>.invalid-*`) may sit beside one
/// backup name. The upgrade never deletes one, because it may be the only copy
/// of something; past this many it refuses to move another aside and fails
/// closed, so a profile that keeps meeting unusable or out-of-date backups is
/// surfaced to an operator instead of filling the disk with profile-sized files.
pub const MAX_MOVED_ASIDE: usize = 8;

/// What a prune pass did, for the host to log and for `store.upgrade` to report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneReport {
    /// Full paths of the backups removed.
    pub removed: Vec<String>,
    /// Older backups that failed verification: not counted, not removed. When
    /// the upgrade itself found an unusable file at the name of the backup it
    /// needed, an entry reading `<path>: <reason>; moved aside to <path>` is
    /// added too: that file was renamed (never deleted) and a fresh copy written.
    pub unverified: Vec<String>,
    /// Backups that should have been removed and could not be.
    pub errors: Vec<String>,
}

/// A copy is readable when it opens read-only, passes `PRAGMA quick_check`,
/// and (when the schema it should hold is known) reads back that version from
/// `work_meta`, so a truncated, empty or mislabelled file is not a backup.
pub(super) fn verify(path: &Path, expected_version: Option<i64>) -> Result<(), String> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("it cannot be opened: {error}"))?;
    let verdict: String = conn
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(|error| format!("it cannot be read: {error}"))?;
    if verdict != "ok" {
        return Err(format!("it fails PRAGMA quick_check: {verdict}"));
    }
    if let Some(expected) = expected_version {
        let found: i64 = conn
            .query_row("SELECT version FROM work_meta WHERE id=1", [], |row| {
                row.get(0)
            })
            .map_err(|error| format!("its schema version cannot be read: {error}"))?;
        if found != expected {
            return Err(format!(
                "it holds schema {found}, not the {expected} its name says"
            ));
        }
    }
    Ok(())
}

/// Whether `conn`'s database has a table called `table`.
pub(super) fn table_present(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [table],
        |row| row.get(0),
    )
}

/// A copy of a store that is about to lose `table` is a backup of it only when
/// the copy holds that table and its rows read back. The `pre-tasks-retire`
/// copy has no workbench version to compare, so this is its expectation.
pub(super) fn verify_holds_table(path: &Path, table: &str) -> Result<(), String> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("it cannot be opened: {error}"))?;
    if !table_present(&conn, table).map_err(|error| format!("it cannot be read: {error}"))? {
        return Err(format!("it holds no `{table}` table"));
    }
    let quoted = table.replace('"', "\"\"");
    conn.query_row(&format!("SELECT count(*) FROM \"{quoted}\""), [], |row| {
        row.get::<_, i64>(0)
    })
    .map(|_| ())
    .map_err(|error| format!("its `{table}` rows cannot be read: {error}"))
}

/// Files in `dir` that were moved aside from a backup name and whose file name
/// starts with `prefix`. A directory that cannot be listed is an error, never
/// "none": the bound on moved-aside copies must not fail open.
fn moved_aside_in(dir: &Path, prefix: &str) -> std::io::Result<Vec<PathBuf>> {
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(prefix) && name.contains(".bak.invalid-"))
        {
            found.push(entry.path());
        }
    }
    found.sort();
    Ok(found)
}

/// Every copy an upgrade moved aside beside `profile`
/// (`<profile>.pre-*.bak.invalid-*`), for an operator deciding which to keep.
/// The upgrade itself never removes one.
pub fn moved_aside_copies(profile: &Path) -> std::io::Result<Vec<PathBuf>> {
    let Some(name) = profile.file_name().and_then(|name| name.to_str()) else {
        return Ok(Vec::new());
    };
    moved_aside_in(
        profile.parent().unwrap_or(Path::new(".")),
        &format!("{name}.pre-"),
    )
}

/// The copies already moved aside from the backup name `backup`.
pub(super) fn moved_aside_from(backup: &Path) -> std::io::Result<Vec<PathBuf>> {
    let Some(name) = backup.file_name().and_then(|name| name.to_str()) else {
        return Ok(Vec::new());
    };
    moved_aside_in(
        backup.parent().unwrap_or(Path::new(".")),
        &format!("{name}.invalid-"),
    )
}

/// A digest of the rows of `only` (or of every table, when `None`), independent
/// of row order, because `VACUUM INTO` may renumber rowids. Two databases with
/// the same digest hold the same rows in those tables.
pub(super) fn content_digest(conn: &Connection, only: Option<&str>) -> rusqlite::Result<u64> {
    use rusqlite::types::ValueRef;
    use std::hash::{DefaultHasher, Hash, Hasher};
    let tables: Vec<String> = match only {
        Some(table) => vec![table.to_string()],
        None => conn
            .prepare(
                "SELECT name FROM sqlite_master
                 WHERE type='table' AND substr(name,1,7)<>'sqlite_' ORDER BY name",
            )?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?,
    };
    let mut digest = DefaultHasher::new();
    for table in tables {
        let quoted = table.replace('"', "\"\"");
        let mut stmt = conn.prepare(&format!("SELECT * FROM \"{quoted}\""))?;
        let columns = stmt.column_count();
        let mut rows = stmt.query([])?;
        let (mut count, mut sum) = (0u64, 0u64);
        while let Some(row) = rows.next()? {
            let mut row_hash = DefaultHasher::new();
            for column in 0..columns {
                match row.get_ref(column)? {
                    ValueRef::Null => 0u8.hash(&mut row_hash),
                    ValueRef::Integer(v) => (1u8, v).hash(&mut row_hash),
                    ValueRef::Real(v) => (2u8, v.to_bits()).hash(&mut row_hash),
                    ValueRef::Text(v) => (3u8, v).hash(&mut row_hash),
                    ValueRef::Blob(v) => (4u8, v).hash(&mut row_hash),
                }
            }
            count += 1;
            sum = sum.wrapping_add(row_hash.finish());
        }
        (table, count, sum).hash(&mut digest);
    }
    Ok(digest.finish())
}

/// The schema version in `<profile>.pre-schema-<version>.bak`, and only that
/// exact shape: no sign, no padding, no suffix after `.bak`.
fn version_of(profile_name: &str, file_name: &str) -> Option<i64> {
    let rest = file_name
        .strip_prefix(profile_name)?
        .strip_prefix(".pre-schema-")?
        .strip_suffix(".bak")?;
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

/// Every `pre-schema` backup beside `profile`, newest schema first. An
/// unreadable directory is no candidates: nothing is removed on a listing that
/// could not be made.
fn candidates(profile: &Path) -> Vec<(i64, PathBuf)> {
    let (Some(dir), Some(name)) = (
        profile.parent().map(|dir| {
            if dir.as_os_str().is_empty() {
                Path::new(".")
            } else {
                dir
            }
        }),
        profile.file_name().and_then(|name| name.to_str()),
    ) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(i64, PathBuf)> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let file_name = entry.file_name();
            let version = version_of(name, file_name.to_str()?)?;
            // A symlink or directory with this name is not ours to delete.
            let regular = std::fs::symlink_metadata(entry.path())
                .map(|meta| meta.file_type().is_file())
                .unwrap_or(false);
            regular.then(|| (version, entry.path()))
        })
        .collect();
    found.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
    found
}

/// Removes the `pre-schema` backups beyond the newest `retention` keeps.
/// `written` is the backup this call has just made and verified; it is counted
/// first and never removed.
pub(super) fn prune(profile: &Path, written: &Path, retention: BackupRetention) -> PruneReport {
    let mut report = PruneReport::default();
    let BackupRetention::Keep(keep) = retention else {
        return report;
    };
    let mut counted = 1usize;
    for (version, path) in candidates(profile) {
        if path == written {
            continue;
        }
        if counted >= keep.get() {
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    eprintln!(
                        "dc-store: removed superseded workbench backup {} (keeping the newest {keep})",
                        path.display()
                    );
                    report.removed.push(path.display().to_string());
                }
                Err(error) => report.errors.push(format!("{}: {error}", path.display())),
            }
        } else {
            match verify(&path, Some(version)) {
                Ok(()) => counted += 1,
                Err(reason) => {
                    eprintln!(
                        "dc-store: workbench backup {} is not counted and was left in place: {reason}",
                        path.display()
                    );
                    report
                        .unverified
                        .push(format!("{}: {reason}", path.display()));
                }
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unreadable_directory_is_an_error_not_no_moved_aside_copies() {
        let missing = Path::new("/nonexistent-dc-store-dir/profile.sqlite.pre-schema-1.bak");
        assert!(moved_aside_from(missing).is_err());
        assert!(moved_aside_copies(Path::new("/nonexistent-dc-store-dir/profile.sqlite")).is_err());
    }

    #[test]
    fn settings_parse_to_a_count_or_all_and_zero_is_refused() {
        assert_eq!(
            BackupRetention::parse("2"),
            Ok(BackupRetention::keep(2).unwrap())
        );
        assert_eq!(
            BackupRetention::parse(" 7 "),
            Ok(BackupRetention::keep(7).unwrap())
        );
        assert_eq!(BackupRetention::parse("all"), Ok(BackupRetention::KeepAll));
        assert_eq!(BackupRetention::parse("ALL"), Ok(BackupRetention::KeepAll));
        for bad in ["0", "", "-1", "two", "2.5", "+3"] {
            assert!(BackupRetention::parse(bad).is_err(), "{bad:?} was accepted");
        }
        assert_eq!(
            BackupRetention::default(),
            BackupRetention::keep(2).unwrap()
        );
    }

    #[test]
    fn only_the_exact_pre_schema_shape_is_a_backup() {
        let profile = "workbench.sqlite";
        let version = |name: &str| version_of(profile, name);
        assert_eq!(version("workbench.sqlite.pre-schema-9.bak"), Some(9));
        assert_eq!(version("workbench.sqlite.pre-schema-16.bak"), Some(16));
        for other in [
            "workbench.sqlite.pre-schema-16.bak.partial",
            "workbench.sqlite.pre-tasks-retire.bak",
            "workbench.sqlite.pre-schema-.bak",
            "workbench.sqlite.pre-schema-+3.bak",
            "workbench.sqlite.pre-schema-1x.bak",
            "other.sqlite.pre-schema-3.bak",
            "workbench.sqlite.compacted",
            "workbench.sqlite",
        ] {
            assert_eq!(version(other), None, "{other}");
        }
    }
}
