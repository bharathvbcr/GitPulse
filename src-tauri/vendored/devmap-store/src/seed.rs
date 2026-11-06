//! Seeding a new worktree's store from another worktree's.
//!
//! A cold `devmap build` of a linked worktree re-parses every file the main
//! checkout's store already describes. Copying that store and then running the
//! ordinary incremental build gives the same graph for the files that did not
//! change. This module is the file and SQL half of that: it never decides
//! *whether* a copy is allowed (the CLI decides which checkout may seed which),
//! it only produces a staged store file that is safe to place.
//!
//! The rules the store imposes shape every step:
//!
//! * A store with more than one hard link is refused at open, so the copy is a
//!   separate inode: a reflink clone (`clonefile` on macOS, `FICLONE` on Linux)
//!   or a plain copy, never a hard link.
//! * A store whose recorded owner is a live sibling is refused, so the copy is
//!   rebound to its own worktree in `pending_state` and `generations` before it
//!   is placed.
//! * A raw copy of a database file is a consistent snapshot only when no WAL
//!   holds committed pages the file lacks. A non-empty `-wal`, or a source that
//!   changes while it is copied, takes SQLite's own `VACUUM INTO` instead. The
//!   `-shm` is never copied.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};

/// How the staged file was produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeedMethod {
    /// Copy-on-write clone of the database file.
    Clone,
    /// Plain byte copy of the database file (the filesystem has no reflink).
    Copy,
    /// SQLite `VACUUM INTO`, taken because the source had a live WAL.
    Backup,
}

impl SeedMethod {
    pub fn label(self) -> &'static str {
        match self {
            Self::Clone => "clone",
            Self::Copy => "copy",
            Self::Backup => "backup",
        }
    }
}

/// `path` with `suffix` appended to its file name (`devmap.sqlite` + `-wal`).
pub fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// What identifies the source's bytes: if it differs after a raw copy, a writer
/// touched it meanwhile and the copy cannot be trusted.
#[derive(PartialEq, Eq)]
struct Fingerprint {
    db_len: u64,
    db_mtime: Option<SystemTime>,
    wal_len: u64,
    wal_mtime: Option<SystemTime>,
}

impl Fingerprint {
    fn of(db: &Path) -> Result<Self> {
        let meta = std::fs::symlink_metadata(db)
            .with_context(|| format!("cannot inspect {}", db.display()))?;
        if !meta.is_file() {
            bail!("{} is not a regular file", db.display());
        }
        let wal = sidecar(db, "-wal");
        let (wal_len, wal_mtime) = match std::fs::symlink_metadata(&wal) {
            Ok(wal_meta) if wal_meta.is_file() => (wal_meta.len(), wal_meta.modified().ok()),
            Ok(_) => bail!("{} is not a regular file", wal.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (0, None),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot inspect {}", wal.display()))
            }
        };
        Ok(Self {
            db_len: meta.len(),
            db_mtime: meta.modified().ok(),
            wal_len,
            wal_mtime,
        })
    }
}

/// Remove `path` and its `-wal` / `-shm`. For files this process staged.
pub fn discard_staged(path: &Path) {
    for file in [
        path.to_path_buf(),
        sidecar(path, "-wal"),
        sidecar(path, "-shm"),
    ] {
        let _ = std::fs::remove_file(file);
    }
}

/// Write a consistent copy of the store at `source` to `staged`, which must not
/// exist. A symlinked source is refused, not followed.
pub fn stage_seed(source: &Path, staged: &Path) -> Result<SeedMethod> {
    let before = Fingerprint::of(source)?;
    if before.wal_len == 0 {
        let method = clone_or_copy(source, staged)?;
        if Fingerprint::of(source)? == before {
            return Ok(method);
        }
        discard_staged(staged);
    }
    backup(source, staged)?;
    Ok(SeedMethod::Backup)
}

/// Write a consistent copy of `source` to `staged`, opportunistically healing or
/// pinning `source`'s WAL so an open `devmap serve` daemon does not force a
/// 272 MiB `VACUUM INTO` copy (`F7`, Critique 5).
///
/// 1. When `-wal` is empty, clones directly (`0` SQLite connections).
/// 2. When `-wal` is non-empty and `source` is writable, tries
///    `PRAGMA busy_timeout = 20; PRAGMA wal_checkpoint(TRUNCATE);`.
/// 3. If a live reader holds `-shm` (`busy != 0`), runs `PASSIVE`, pins
///    `WAL_READ_LOCK` via `BEGIN DEFERRED; SELECT 1 FROM sqlite_master LIMIT 1;`,
///    re-verifies `PASSIVE` reports `(busy == 0 && log == checkpointed)`, and
///    clones `source` while holding the read mark.
/// 4. Falls back to `VACUUM INTO` only if uncheckpointed WAL frames remain or
///    `source` is read-only.
pub fn stage_seed_healing_wal(source: &Path, staged: &Path) -> Result<SeedMethod> {
    let before = Fingerprint::of(source)?;
    if before.wal_len == 0 {
        let method = clone_or_copy(source, staged)?;
        if Fingerprint::of(source)? == before {
            return Ok(method);
        }
        discard_staged(staged);
    }

    if source_is_writable(source) {
        if let Ok(method) = try_heal_or_pin_wal_and_clone(source, staged) {
            return Ok(method);
        }
        discard_staged(staged);
    }

    backup(source, staged)?;
    Ok(SeedMethod::Backup)
}

fn source_is_writable(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.is_file() || meta.permissions().readonly() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o200 == 0 {
            return false;
        }
    }
    true
}

fn try_heal_or_pin_wal_and_clone(source: &Path, staged: &Path) -> Result<SeedMethod> {
    let conn = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.execute_batch("PRAGMA trusted_schema = OFF;")?;
    conn.busy_timeout(std::time::Duration::from_millis(20))?;

    let (busy, log, _ckpt): (i64, i64, i64) =
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    if busy == 0 && log == 0 {
        drop(conn);
        let clean = Fingerprint::of(source)?;
        if clean.wal_len == 0 {
            let method = clone_or_copy(source, staged)?;
            if Fingerprint::of(source)? == clean {
                return Ok(method);
            }
            discard_staged(staged);
        }
        bail!("source changed immediately after TRUNCATE checkpoint");
    }

    let _ = conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    });
    conn.execute_batch("BEGIN DEFERRED; SELECT 1 FROM sqlite_master LIMIT 1;")?;
    let (p_busy, p_log, p_ckpt): (i64, i64, i64) =
        conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    if p_busy != 0 || p_log != p_ckpt {
        let _ = conn.execute_batch("ROLLBACK;");
        bail!("source WAL holds uncheckpointed frames (busy={p_busy}, log={p_log}, ckpt={p_ckpt})");
    }
    let meta_before = std::fs::symlink_metadata(source)?;
    let len_before = meta_before.len();
    let mtime_before = meta_before.modified().ok();
    let method = clone_or_copy(source, staged)?;
    let meta_after = std::fs::symlink_metadata(source)?;
    let _ = conn.execute_batch("ROLLBACK;");
    if meta_after.len() != len_before || meta_after.modified().ok() != mtime_before {
        discard_staged(staged);
        bail!("source database file changed during pinned WAL clone");
    }
    Ok(method)
}

fn backup(source: &Path, staged: &Path) -> Result<()> {
    let conn = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("cannot open {} for backup", source.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    let target = staged
        .to_str()
        .context("staged store path is not valid UTF-8")?;
    conn.execute("VACUUM INTO ?1", [target])
        .with_context(|| format!("VACUUM INTO {target} failed"))?;
    Ok(())
}

/// Seed one derived artifact (`repo_map.json`, `code_graph.json`, ...) beside a
/// seeded store: clone `source` to `target` when `target` is absent.
///
/// The manifest writer patches a changed artifact in place of the previous
/// file, keeping every unchanged block (see `devmap_query::code_graph`). A
/// worktree seeded with the store alone has no previous file, so its first
/// manifest build lays the graph out afresh and shares nothing with the main
/// checkout. A clone gives that build the main checkout's file to patch. The
/// clone is never trusted as current: the artifacts stamp is not copied, so
/// the next manifest build regenerates it for this worktree.
///
/// `None` when nothing was placed: `target` already exists, or `source` is not
/// a regular file. Staged under a sibling name and renamed into place, so a
/// reader never sees a partial artifact.
pub fn seed_artifact(source: &Path, target: &Path) -> Result<Option<SeedMethod>> {
    match std::fs::symlink_metadata(target) {
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("cannot inspect {}", target.display()))
        }
    }
    match std::fs::symlink_metadata(source) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot inspect {}", source.display()))
        }
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let staged = sidecar(target, &format!(".seed-{}", std::process::id()));
    let _ = std::fs::remove_file(&staged);
    let placed = clone_or_copy(source, &staged).and_then(|method| {
        std::fs::rename(&staged, target)
            .with_context(|| format!("cannot place {}", target.display()))?;
        Ok(method)
    });
    if placed.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    placed.map(Some)
}

/// Seed a flat cache directory (such as `resolution-reuse-1/`) via copy-on-write
/// cloning when `target_dir` does not yet exist (`F9`).
///
/// On APFS, `clonefile` clones the entire directory tree metadata-only in one
/// syscall; on filesystems without directory `clonefile`, creates a staged
/// directory and clones/copies regular file entries atomically before rename.
pub fn seed_directory_cow(source_dir: &Path, target_dir: &Path) -> Result<Option<SeedMethod>> {
    match std::fs::symlink_metadata(target_dir) {
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("cannot inspect {}", target_dir.display()))
        }
    }
    match std::fs::symlink_metadata(source_dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot inspect {}", source_dir.display()))
        }
    }
    if let Some(parent) = target_dir.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let staged = sidecar(target_dir, &format!(".seed-dir-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staged);
    if try_clone(source_dir, &staged) {
        if let Err(error) = std::fs::rename(&staged, target_dir) {
            let _ = std::fs::remove_dir_all(&staged);
            return Err(error).with_context(|| format!("cannot place {}", target_dir.display()));
        }
        return Ok(Some(SeedMethod::Clone));
    }
    std::fs::create_dir_all(&staged)?;
    let mut method = SeedMethod::Clone;
    let copy_res = (|| -> Result<()> {
        for entry in std::fs::read_dir(source_dir)? {
            let entry = entry?;
            let meta = std::fs::symlink_metadata(entry.path())?;
            if !meta.is_file() {
                continue;
            }
            let dst = staged.join(entry.file_name());
            if clone_or_copy(&entry.path(), &dst)? == SeedMethod::Copy {
                method = SeedMethod::Copy;
            }
        }
        std::fs::rename(&staged, target_dir)
            .with_context(|| format!("cannot place {}", target_dir.display()))?;
        Ok(())
    })();
    if copy_res.is_err() {
        let _ = std::fs::remove_dir_all(&staged);
        return copy_res.map(|()| None);
    }
    Ok(Some(method))
}

pub fn clone_or_copy(source: &Path, staged: &Path) -> Result<SeedMethod> {
    if try_clone(source, staged) {
        return Ok(SeedMethod::Clone);
    }
    // `create_new`: never write through something already at `staged`.
    let mut input = std::fs::File::open(source)?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(staged)?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    Ok(SeedMethod::Copy)
}

#[cfg(target_os = "macos")]
fn try_clone(source: &Path, staged: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let (Ok(src), Ok(dst)) = (
        std::ffi::CString::new(source.as_os_str().as_bytes()),
        std::ffi::CString::new(staged.as_os_str().as_bytes()),
    ) else {
        return false;
    };
    // SAFETY: both are valid NUL-terminated paths that outlive the call;
    // `clonefile` fails with EEXIST rather than replacing `staged`.
    unsafe { libc::clonefile(src.as_ptr(), dst.as_ptr(), 0) == 0 }
}

#[cfg(target_os = "linux")]
fn try_clone(source: &Path, staged: &Path) -> bool {
    use std::os::unix::io::AsRawFd;
    // _IOW(0x94, 9, int)
    const FICLONE: u32 = 0x4004_9409;
    let Ok(input) = std::fs::File::open(source) else {
        return false;
    };
    let Ok(output) = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(staged)
    else {
        return false;
    };
    // SAFETY: both descriptors are open for the duration of the call.
    let cloned = unsafe { libc::ioctl(output.as_raw_fd(), FICLONE as _, input.as_raw_fd()) == 0 };
    if !cloned {
        drop(output);
        let _ = std::fs::remove_file(staged);
    }
    cloned
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn try_clone(_source: &Path, _staged: &Path) -> bool {
    false
}

/// Rebind a staged database to `root` without touching `generations` (`F6`, `F8`,
/// Critique 3, `SC-001`): writes `seeded_repo_root` into `seed_rebind` and
/// updates `pending_state.repo_root` (`<= 16-32 KiB`), leaving every
/// `generations` row and its `analysis_json` overflow chain untouched and
/// CoW-shared with the donor store.
/// Note: stacked/zero-copy mode (`base.sqlite`) never invokes `rebind_staged` at
/// all (`0 B` CoW write amplification).
pub fn rebind_staged(staged: &Path, root: &Path) -> Result<()> {
    let root = root
        .canonicalize()
        .with_context(|| format!("cannot resolve {}", root.display()))?;
    let root = root
        .to_str()
        .context("worktree root is not valid UTF-8")?
        .to_owned();
    let mut conn = Connection::open(staged)?;
    conn.execute_batch("PRAGMA trusted_schema = OFF; PRAGMA synchronous = OFF;")?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(crate::schema::SEED_REBIND_TABLE)?;
    tx.execute(
        "INSERT INTO seed_rebind (singleton, seeded_repo_root) VALUES (1, ?1)
         ON CONFLICT (singleton) DO UPDATE SET seeded_repo_root = excluded.seeded_repo_root",
        [&root],
    )?;
    let bound = tx.execute(
        "UPDATE pending_state SET repo_root = ?1 WHERE singleton = 1",
        [&root],
    )?;
    if bound != 1 {
        bail!("the seed store has no pending_state row to rebind");
    }
    tx.commit()?;
    conn.close().map_err(|(_, error)| error)?;
    Ok(())
}

/// Convert a staged database into a sidecar-free, read-only (`chmod 0444`)
/// immutable base snapshot suitable for `?immutable=1` (`OQ1`).
pub fn prepare_immutable_snapshot(staged: &Path) -> Result<()> {
    {
        let conn = Connection::open(staged)?;
        conn.execute_batch("PRAGMA trusted_schema = OFF;")?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        let (busy, _log, _checkpointed): (i64, i64, i64) =
            conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
        if busy != 0 {
            bail!("the staged base snapshot could not be checkpointed (busy)");
        }
        let _: String = conn.query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))?;
        conn.close().map_err(|(_, error)| error)?;
    }
    for suffix in ["-wal", "-shm"] {
        let side = sidecar(staged, suffix);
        let _ = std::fs::remove_file(&side);
    }
    set_file_readonly(staged)?;
    Ok(())
}

pub fn set_file_readonly(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444))
            .with_context(|| format!("cannot chmod 0444 {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(path, perms)
            .with_context(|| format!("cannot set readonly {}", path.display()))?;
    }
    Ok(())
}

pub fn set_file_writable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))
            .with_context(|| format!("cannot chmod 0644 {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_readonly(false);
        std::fs::set_permissions(path, perms)
            .with_context(|| format!("cannot clear readonly {}", path.display()))?;
    }
    Ok(())
}

/// Promote a sealed `0444` `base.sqlite` snapshot into an independent writable
/// WAL-mode `devmap.sqlite` rebound to `worktree_root`, atomically placed at
/// `target_db` (`OQ2` auto-rebase / promotion).
pub fn promote_immutable_base_to_store(
    base_sqlite: &Path,
    target_db: &Path,
    worktree_root: &Path,
) -> Result<SeedMethod> {
    if let Some(parent) = target_db.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let staged = sidecar(target_db, &format!(".promote-{}", std::process::id()));
    discard_staged(&staged);
    let result = (|| -> Result<SeedMethod> {
        let method = clone_or_copy(base_sqlite, &staged)?;
        set_file_writable(&staged)?;
        {
            let conn = Connection::open(&staged)?;
            conn.execute_batch("PRAGMA trusted_schema = OFF;")?;
            conn.busy_timeout(std::time::Duration::from_secs(10))?;
            let _: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
            conn.close().map_err(|(_, e)| e)?;
        }
        rebind_staged(&staged, worktree_root)?;
        settle_staged(&staged)?;
        std::fs::rename(&staged, target_db)
            .with_context(|| format!("cannot place {}", target_db.display()))?;
        Ok(method)
    })();
    if result.is_err() {
        let _ = set_file_writable(&staged);
        discard_staged(&staged);
    }
    result
}

/// Fold the staged store's WAL into the file and drop its sidecars, so what is
/// placed is one self-contained file.
pub fn settle_staged(staged: &Path) -> Result<()> {
    let conn = Connection::open(staged)?;
    let (busy, _log, _checkpointed): (i64, i64, i64) =
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    if busy != 0 {
        bail!("the staged store could not be checkpointed (busy)");
    }
    conn.close().map_err(|(_, error)| error)?;
    for suffix in ["-wal", "-shm"] {
        let side = sidecar(staged, suffix);
        if let Ok(meta) = std::fs::symlink_metadata(&side) {
            if meta.len() != 0 {
                bail!("{} still holds data after checkpoint", side.display());
            }
            let _ = std::fs::remove_file(&side);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_test_dir(tag: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "devmap-seed-test-{}-{}-{}",
            tag,
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        dir
    }

    #[test]
    fn rebind_staged_overrides_repo_root_without_rewriting_historical_generations() {
        let tmp = temp_test_dir("rebind");
        let donor_root = tmp.join("donor");
        let lane_root = tmp.join("lane");
        let alt_root = tmp.join("alt-lane");
        std::fs::create_dir_all(&donor_root).unwrap();
        std::fs::create_dir_all(&lane_root).unwrap();
        std::fs::create_dir_all(&alt_root).unwrap();
        let donor_canon = donor_root.canonicalize().unwrap();
        let lane_canon = lane_root.canonicalize().unwrap();
        let alt_canon = alt_root.canonicalize().unwrap();

        let source_db = tmp.join("source.sqlite");
        {
            let store = Store::open(&source_db).unwrap();
            store.bind_repo_root(&donor_canon).unwrap();
            drop(store);
            let empty_res = devmap_resolve::Resolver::new().resolve_all(&[]).unwrap();
            let analysis_json =
                serde_json::to_string(&devmap_analyze::analyze(&[], &empty_res)).unwrap();
            let conn = Connection::open(&source_db).unwrap();
            conn.execute(
                "INSERT INTO generations (created_at, head_sha, analysis_json, repo_root)
                 VALUES (1730000000.0, 'oldbeef', ?1, ?2)",
                rusqlite::params![&analysis_json, donor_canon.to_str().unwrap()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO generations (created_at, head_sha, analysis_json, repo_root)
                 VALUES (1730000001.0, 'deadbeef', ?1, ?2)",
                rusqlite::params![&analysis_json, donor_canon.to_str().unwrap()],
            )
            .unwrap();
        }
        settle_staged(&source_db).unwrap();

        let staged_db = tmp.join("staged.sqlite");
        let method = stage_seed_healing_wal(&source_db, &staged_db).unwrap();
        assert_ne!(method, SeedMethod::Backup);
        rebind_staged(&staged_db, &lane_canon).unwrap();
        settle_staged(&staged_db).unwrap();

        // ALL generations rows (historical AND MAX(id)) MUST remain untouched (CoW-shared), while seed_rebind binds lane_canon.
        {
            let conn = Connection::open(&staged_db).unwrap();
            let oldest_gen_root: String = conn
                .query_row(
                    "SELECT repo_root FROM generations ORDER BY id ASC LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(oldest_gen_root, donor_canon.to_str().unwrap());
            let latest_gen_root: String = conn
                .query_row(
                    "SELECT repo_root FROM generations ORDER BY id DESC LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(latest_gen_root, donor_canon.to_str().unwrap());
            let seeded: String = conn
                .query_row(
                    "SELECT seeded_repo_root FROM seed_rebind WHERE singleton = 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(seeded, lane_canon.to_str().unwrap());
        }

        // Every Store repo_root reader returns the rebound worktree root.
        let reopened = Store::open_read_only(&staged_db).unwrap();
        assert_eq!(
            reopened.latest_repo_root().unwrap().as_deref(),
            lane_canon.to_str()
        );
        assert_eq!(
            reopened.latest_generation_repo_root().unwrap().as_deref(),
            lane_canon.to_str()
        );
        let page = reopened.all_symbols_page().unwrap().unwrap();
        assert_eq!(page.repo_root.as_deref(), lane_canon.to_str());
        let search = reopened.search_page("nonexistent", 10).unwrap().unwrap();
        assert_eq!(search.repo_root.as_deref(), lane_canon.to_str());
        drop(reopened);

        // And seed_rebind takes precedence via COALESCE even when generations.repo_root differs.
        {
            let conn = Connection::open(&staged_db).unwrap();
            conn.execute(
                "UPDATE seed_rebind SET seeded_repo_root = ?1 WHERE singleton = 1",
                [alt_canon.to_str().unwrap()],
            )
            .unwrap();
        }
        let reopened_alt = Store::open_read_only(&staged_db).unwrap();
        assert_eq!(
            reopened_alt.latest_repo_root().unwrap().as_deref(),
            alt_canon.to_str()
        );
        assert_eq!(
            reopened_alt
                .latest_generation_repo_root()
                .unwrap()
                .as_deref(),
            alt_canon.to_str()
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn rebind_staged_writes_under_32kib_on_8mib_latest_generation() {
        let tmp = temp_test_dir("rebind-8mib");
        let donor_root = tmp.join("donor");
        let lane_root = tmp.join("lane");
        std::fs::create_dir_all(&donor_root).unwrap();
        std::fs::create_dir_all(&lane_root).unwrap();
        let donor_canon = donor_root.canonicalize().unwrap();
        let lane_canon = lane_root.canonicalize().unwrap();

        let source_db = tmp.join("source-8mib.sqlite");
        {
            let store = Store::open(&source_db).unwrap();
            store.bind_repo_root(&donor_canon).unwrap();
            drop(store);
            // Build a valid >8 MiB JSON payload on MAX(id) matching real DevCouncil overflow chains (~514 pages).
            let mut large_json =
                serde_json::to_value(devmap_analyze::analyze(
                    &[],
                    &devmap_resolve::Resolver::new().resolve_all(&[]).unwrap(),
                ))
                .unwrap();
            large_json
                .as_object_mut()
                .unwrap()
                .insert("padding_8mib".to_string(), serde_json::Value::String("x".repeat(8 * 1024 * 1024)));
            let payload = serde_json::to_string(&large_json).unwrap();

            let conn = Connection::open(&source_db).unwrap();
            conn.execute(
                "INSERT INTO generations (created_at, head_sha, analysis_json, repo_root)
                 VALUES (1730000001.0, 'deadbeef', ?1, ?2)",
                rusqlite::params![&payload, donor_canon.to_str().unwrap()],
            )
            .unwrap();
        }
        settle_staged(&source_db).unwrap();

        let staged_db = tmp.join("staged-8mib.sqlite");
        let method = stage_seed_healing_wal(&source_db, &staged_db).unwrap();
        assert_ne!(method, SeedMethod::Backup);
        rebind_staged(&staged_db, &lane_canon).unwrap();
        settle_staged(&staged_db).unwrap();

        let src_bytes = std::fs::read(&source_db).unwrap();
        let dst_bytes = std::fs::read(&staged_db).unwrap();
        let min_blocks = (src_bytes.len() / 4096).min(dst_bytes.len() / 4096);
        let mut differing_blocks = (src_bytes.len().abs_diff(dst_bytes.len())).div_ceil(4096);
        for b in 0..min_blocks {
            if src_bytes[b * 4096..(b + 1) * 4096] != dst_bytes[b * 4096..(b + 1) * 4096] {
                differing_blocks += 1;
            }
        }
        let cow_dirty_bytes = differing_blocks * 4096;
        assert!(
            cow_dirty_bytes <= 32 * 1024,
            "SC-001 violation: rebind_staged dirtied {} bytes ({} x 4 KiB CoW pages; expected <= 32 KiB)",
            cow_dirty_bytes,
            differing_blocks
        );

        let conn = Connection::open(&staged_db).unwrap();
        let raw_gen_root: String = conn
            .query_row(
                "SELECT repo_root FROM generations ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            raw_gen_root,
            donor_canon.to_str().unwrap(),
            "generations table pages must remain untouched so APFS CoW extents stay shared"
        );
        drop(conn);

        let reopened = Store::open_read_only(&staged_db).unwrap();
        assert_eq!(reopened.latest_repo_root().unwrap().as_deref(), lane_canon.to_str());
        assert_eq!(
            reopened.latest_generation_repo_root().unwrap().as_deref(),
            lane_canon.to_str()
        );
        drop(reopened);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn stage_seed_healing_wal_clones_when_reader_pins_checkpointed_wal_and_backs_up_on_uncheckpointed()
    {
        let tmp = temp_test_dir("wal-heal");
        let source_db = tmp.join("source.sqlite");
        {
            let store = Store::open(&source_db).unwrap();
            drop(store);
        }

        // Step 1: Write into WAL and checkpoint PASSIVE while leaving a non-empty -wal file,
        // and hold a read mark (`BEGIN DEFERRED; SELECT ...`) so TRUNCATE returns busy != 0.
        let writer = Connection::open(&source_db).unwrap();
        writer
            .execute(
                "UPDATE pending_state SET repo_root = '/tmp/ckpt' WHERE singleton = 1",
                [],
            )
            .unwrap();
        let _: (i64, i64, i64) = writer
            .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert!(
            sidecar(&source_db, "-wal").metadata().unwrap().len() > 0,
            "PASSIVE checkpoint must leave non-zero -wal sidecar"
        );

        let reader = Connection::open(&source_db).unwrap();
        reader
            .execute_batch("BEGIN DEFERRED; SELECT 1 FROM sqlite_master LIMIT 1;")
            .unwrap();

        let staged_pinned = tmp.join("staged-pinned.sqlite");
        let method_pinned = stage_seed_healing_wal(&source_db, &staged_pinned).unwrap();
        assert_ne!(
            method_pinned,
            SeedMethod::Backup,
            "checkpointed WAL with active reader must clone/copy via pinned WAL_READ_LOCK, not VACUUM INTO"
        );

        // Step 2: Commit a NEW write AFTER `reader` pinned the old snapshot!
        // Now `PASSIVE` cannot checkpoint the new frame past `reader` (`log != ckpt`),
        // so `stage_seed_healing_wal` MUST fall back to `Backup` (`VACUUM INTO`) and capture `/tmp/unckpt`!
        writer
            .execute(
                "UPDATE pending_state SET repo_root = '/tmp/unckpt' WHERE singleton = 1",
                [],
            )
            .unwrap();
        let staged_unckpt = tmp.join("staged-unckpt.sqlite");
        let method_unckpt = stage_seed_healing_wal(&source_db, &staged_unckpt).unwrap();
        assert_eq!(method_unckpt, SeedMethod::Backup);

        let verify = Connection::open(&staged_unckpt).unwrap();
        let captured: String = verify
            .query_row(
                "SELECT repo_root FROM pending_state WHERE singleton = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(captured, "/tmp/unckpt");

        let _ = reader.execute_batch("ROLLBACK;");
        drop(reader);
        drop(writer);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn open_immutable_snapshot_refuses_writable_or_sidecar_db_and_accepts_sealed_0444() {
        let tmp = temp_test_dir("immutable");
        let db = tmp.join("base.sqlite");
        {
            let store = Store::open(&db).unwrap();
            drop(store);
            let conn = Connection::open(&db).unwrap();
            conn.execute(
                "INSERT INTO generations (created_at, head_sha, analysis_json, repo_root)
                 VALUES (1730000000.0, 'cafebabe', '{}', '/tmp/anchor')",
                [],
            )
            .unwrap();
        }

        // 1. Writable file MUST fail closed (`OQ1`).
        let err_writable = match Store::open_immutable_snapshot(&db, None) {
            Ok(_) => panic!("writable database must be refused by open_immutable_snapshot"),
            Err(err) => err.to_string(),
        };
        assert!(
            err_writable.contains("writable"),
            "expected writable refusal, got: {err_writable}"
        );

        // 2. Seal via `prepare_immutable_snapshot`.
        prepare_immutable_snapshot(&db).unwrap();

        // 3. Inject a stray `-wal` sidecar next to the `0444` DB -> MUST fail closed!
        let wal_side = sidecar(&db, "-wal");
        std::fs::write(&wal_side, b"stray").unwrap();
        let err_sidecar = match Store::open_immutable_snapshot(&db, None) {
            Ok(_) => panic!("snapshot with stray -wal sidecar must be refused"),
            Err(err) => err.to_string(),
        };
        assert!(
            err_sidecar.contains("sidecar"),
            "expected sidecar refusal, got: {err_sidecar}"
        );
        std::fs::remove_file(&wal_side).unwrap();

        // 4. Clean `0444` snapshot opens and honors `repo_root_override`!
        let snap =
            Store::open_immutable_snapshot(&db, Some("/tmp/worktree-lane".to_string())).unwrap();
        assert_eq!(
            snap.latest_repo_root().unwrap().as_deref(),
            Some("/tmp/worktree-lane")
        );
        assert_eq!(
            snap.latest_generation_repo_root().unwrap().as_deref(),
            Some("/tmp/worktree-lane")
        );

        // Restore write bit before temp dir cleanup.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o644));
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
