//! Glean-style `StackedStore`: a lane's own immutable `chmod 0444` copy of a
//! shared base snapshot (`<state>/codeintel/stack/base.sqlite` + lazy mmap
//! `base.sqlite.csr`) with a per-worktree `DeltaStore` (`delta.sqlite`) and
//! `WorktreeOverlay` (`RoaringBitmap` dirty-node masking) (`F1`, `F4`, `F6`,
//! `F10`, `OQ1`, `OQ2`).
//!
//! # Trust model
//!
//! `<common_git_dir>/devmap/bases/` is a publish cache. Every checkout of the
//! repository publishes, replaces and collects entries there, so nothing under
//! it keeps its identity after [`provision_stacked_worktree`] returns, and no
//! read path names it. Provision pins one cache entry by directory descriptor,
//! clones the files it verified into the lane's own `stack/` directory and
//! records what it admitted (sha256, generation, `HEAD`, donor root and file
//! identity) in the lease. `open` and `materialize_seeded_store` serve only that
//! private clone, which is trusted exactly as a plain worktree's `devmap.sqlite`
//! is: a writer in the lane's own state directory is out of scope; benign
//! concurrency in the cache (another checkout's provision, replace or GC) is
//! unable to change what a lane serves.
//!
//! # Precedence
//!
//! A `devmap.sqlite` beside a lease means the lease is stale: materialize
//! places the store first and retires the lease after, so a crash between the
//! two leaves a store and a lease. A read (`open_if_leased`) refuses it and
//! deletes nothing; `open_if_leased_for_sync` (`seed --sync-delta`) retires it
//! under the writer lock and answers `None`; provision refuses a lane that has
//! a store.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use devmap_extract::model::{Extraction, WiringKind};
use devmap_extract::paths::{
    code_graph_path, compact_code_graph_path, csr_path_for_store, find_legacy_base_store_ref,
    main_checkout_of, registered_common_git_dir, repo_map_path, repo_root_from_store,
    same_registered_repo, stack_delta_path, stack_lease_path, state_dir, store_path,
};
use devmap_extract::safe_fs::{Access, Creation, PinnedDir};
use devmap_resolve::model::LangFamily;
use devmap_store::delta::{DeltaEdgeRecord, DeltaStore};
use devmap_store::schema::CURRENT_SCHEMA_VERSION;
use devmap_store::seed::{
    clone_or_copy_file, discard_staged, prepare_immutable_snapshot,
    promote_immutable_base_to_store, read_file_at, seed_artifact, seed_directory_cow,
    set_file_readonly, set_file_writable, stage_seed_healing_wal,
};
use devmap_store::{
    CallersPage, DeadPage, FileEdges, GenerationEdges, SearchNarrowing, SearchPage, Store,
    StoredEdge, StoredFile, StoredSymbol,
};
use serde::{Deserialize, Serialize};

use crate::artifacts::write_atomic;
use crate::cas::{blake3_hash, CAS_TOMBSTONE};
use crate::engine::traversal::node_id_of;
use crate::overlay::{OverlayEdge, RoaringBitmap, WorktreeOverlay};

/// `(generation, repository root, [(path, language)])`: the file list a scope resolves against.
pub type ScopeInputs = (u32, Option<String>, Vec<(String, String)>);

/// Format of both the lease and the published manifest. 2 moved the base into
/// the lane (`stack/`); a v1 lease named a shared cache entry.
pub const STACK_FORMAT_VERSION: u32 = 2;
pub const DEFAULT_REBASE_DIRTY_FILES: usize = 250;
pub const DEFAULT_REBASE_DELTA_BYTES: u64 = 16 * 1024 * 1024;

const BASE_SQLITE: &str = "base.sqlite";
const BASE_CSR: &str = "base.sqlite.csr";
const MANIFEST: &str = "manifest.json";

/// The publisher's record of a cache entry, kept inside the entry. It exists
/// so a later publish can reuse the entry; a lane never reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaseManifest {
    pub format_version: u32,
    pub generation_id: u32,
    pub schema_version: i32,
    pub source_repo_root: String,
    /// The donor's git `HEAD` the snapshot was taken at. Part of the base's
    /// identity: two donors (or one donor rebuilt) can reuse a generation id.
    pub head_sha: String,
    /// The donor store's creation epoch (`pending_state.epoch`). A donor
    /// deleted and rebuilt restarts its generation ids, so id and `HEAD` alone
    /// cannot tell its new base from the old one.
    pub epoch: String,
    pub created_at_unix_ms: u64,
    pub base_sqlite_sha256: String,
    /// The CSR sidecar's sha256, when one was published.
    #[serde(default)]
    pub csr_sha256: Option<String>,
}

/// Content hash and identity of one file the lane admitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRecord {
    pub sha256: String,
    pub len: u64,
    pub mtime_ns: u64,
    pub ctime_ns: u64,
    pub ino: u64,
    pub dev: u64,
}

/// What identifies a published base: generation id, donor `HEAD`, donor root,
/// and the donor store's creation epoch.
///
/// The epoch is not part of [`BaseKey::dir_name`] (garbage collection parses
/// that name). It is part of the identity every reuse and admission check
/// compares, so a donor deleted and rebuilt at the same `HEAD`, which restarts
/// at the same generation id, never matches an entry published from the old store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseKey {
    pub generation_id: u32,
    pub head_sha: String,
    pub source_repo_root: String,
    /// `pending_state.epoch` of the donor store.
    pub epoch: String,
}

impl BaseKey {
    pub fn new(
        generation_id: u32,
        head_sha: &str,
        source_root: &Path,
        epoch: &str,
    ) -> Result<Self> {
        if !is_commit_sha(head_sha) {
            bail!(
                "donor generation {generation_id} carries head_sha {head_sha:?}, not a git commit; \
                 a stacked base must be taken from a built, committed checkout"
            );
        }
        if epoch.len() != 32
            || !epoch
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            bail!("donor store epoch {epoch:?} is not 32 lowercase hex characters");
        }
        Ok(Self {
            generation_id,
            head_sha: head_sha.to_string(),
            source_repo_root: source_root.to_string_lossy().into_owned(),
            epoch: epoch.to_string(),
        })
    }

    /// `gen-<G>-<head12>-<src8>`. Names the cache entry; it does not carry the
    /// epoch, so a rebuilt donor's publish finds the old entry, fails its
    /// manifest check, and replaces it.
    pub fn dir_name(&self) -> String {
        format!(
            "gen-{}-{}-{}",
            self.generation_id,
            &self.head_sha[..12],
            &sha256_str_hex(&self.source_repo_root)[..8]
        )
    }

    fn of_manifest(manifest: &BaseManifest) -> Result<Self> {
        Self::new(
            manifest.generation_id,
            &manifest.head_sha,
            Path::new(&manifest.source_repo_root),
            &manifest.epoch,
        )
    }
}

fn is_commit_sha(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Parse `gen-<G>-<head12>-<src8>`; `None` for legacy `gen-<G>` and anything else.
fn parse_base_dir_name(name: &str) -> Option<(u32, String)> {
    let rest = name.strip_prefix("gen-")?;
    let mut parts = rest.split('-');
    let gen_id = parts.next()?.parse::<u32>().ok()?;
    let head12 = parts.next()?;
    let src8 = parts.next()?;
    let hex = |t: &str, n: usize| {
        t.len() == n && t.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    };
    if parts.next().is_some() || !hex(head12, 12) || !hex(src8, 8) {
        return None;
    }
    Some((gen_id, src8.to_string()))
}

/// What a lane admitted at provision, and so what every later open must find.
/// The base itself is the lane's own clone in `<state>/codeintel/stack/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StackLease {
    pub format_version: u32,
    pub generation_id: u32,
    pub head_sha: String,
    pub source_repo_root: String,
    /// The private `base.sqlite`: sha256 and `(len, mtime_ns, ctime_ns, ino, dev)`.
    pub base: FileRecord,
    /// The private CSR sidecar, when the donor had one.
    #[serde(default)]
    pub csr: Option<FileRecord>,
    pub worktree_root: String,
    pub leased_at_unix_ms: u64,
    pub rebases_after_dirty_files: usize,
    pub rebases_after_delta_bytes: u64,
}

impl StackLease {
    /// The lane's private directory holding its base (and CSR sidecar).
    pub fn base_dir(&self) -> PathBuf {
        stack_dir(Path::new(&self.worktree_root))
    }

    pub fn base_sqlite_path(&self) -> PathBuf {
        self.base_dir().join(BASE_SQLITE)
    }

    pub fn read_from(path: &Path) -> Result<Self> {
        parse_lease(&read_lease_bytes(path)?, path)
    }
}

/// The lane's private base directory: `<state>/codeintel/stack/`.
fn stack_dir(worktree_root: &Path) -> PathBuf {
    let lease = stack_lease_path(worktree_root);
    lease.parent().unwrap_or(worktree_root).join("stack")
}

/// How to get out of any state that strands a lane: re-provisioning replaces
/// the lease and the lane's private base.
fn restack_hint(lease_path: &Path) -> String {
    format!(
        "to recover, remove {} and run `devmap seed --stacked --from <donor checkout>` in this worktree",
        lease_path.display()
    )
}

/// Parse a lease, refusing another format by name before looking at its fields.
fn parse_lease(raw: &[u8], path: &Path) -> Result<StackLease> {
    // serde's messages quote the offending value or field name, which is the
    // file's own text: bounded and escaped like every other lease echo.
    let invalid = |err: serde_json::Error| {
        anyhow::anyhow!(
            "invalid stack lease {}: {}; {}",
            path.display(),
            lease_text(&err.to_string()),
            restack_hint(path)
        )
    };
    let value: serde_json::Value = serde_json::from_slice(raw).map_err(invalid)?;
    let version = value
        .get("format_version")
        .and_then(serde_json::Value::as_u64);
    if version != Some(u64::from(STACK_FORMAT_VERSION)) {
        bail!(
            "stack lease {} is format {} but this devmap reads format {STACK_FORMAT_VERSION} \
             (a v1 lease named a shared base that lanes no longer read); {}",
            path.display(),
            version.map_or_else(|| "unknown".to_string(), |v| v.to_string()),
            restack_hint(path)
        );
    }
    serde_json::from_value(value).map_err(invalid)
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcBasesReport {
    pub retained_generations: Vec<u32>,
    pub removed_generations: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackedStatus {
    pub worktree_root: String,
    pub base_generation_id: u32,
    pub base_dir: String,
    pub dirty_files: usize,
    pub tombstoned_files: usize,
    pub delta_symbols: usize,
    pub delta_edges: usize,
    pub delta_bytes: u64,
    pub needs_rebase: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackedSyncReport {
    pub synced_files: usize,
    pub deleted_files: usize,
    pub cleaned_files: usize,
    pub rebased_to_standalone: Option<PathBuf>,
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// How long provision and GC wait for each other before failing closed.
pub const BASES_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// The writer locks this module takes, in the only order a thread may hold
/// them: the lane's delta, then its store, then the shared bases directory.
/// `sync_worktree` holds the delta lock while an auto-rebase takes the store
/// lock; provision holds the delta lock before the bases lock. A thread that
/// took them in another order could deadlock against one that took them in
/// this one, so [`ordered_lock`] asserts the order (debug builds).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum LockRank {
    Delta,
    Store,
    Bases,
}

thread_local! {
    static HELD_LOCK_RANKS: std::cell::RefCell<Vec<LockRank>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// A held writer lock and its place in this thread's lock order. The lock is
/// released before the rank is popped.
#[derive(Debug)]
struct OrderedLock {
    _lock: devmap_store::WriterLock,
    _rank: RankGuard,
}

#[derive(Debug)]
struct RankGuard(LockRank);

impl RankGuard {
    fn enter(rank: LockRank) -> Self {
        HELD_LOCK_RANKS.with(|held| {
            let mut held = held.borrow_mut();
            debug_assert!(
                held.last().is_none_or(|last| *last < rank),
                "lock order violated: taking {rank:?} while holding {held:?} \
                 (the order is delta, then store, then bases)"
            );
            held.push(rank);
        });
        Self(rank)
    }
}

impl Drop for RankGuard {
    fn drop(&mut self) {
        HELD_LOCK_RANKS.with(|held| {
            let mut held = held.borrow_mut();
            if let Some(at) = held.iter().rposition(|rank| *rank == self.0) {
                held.remove(at);
            }
        });
    }
}

/// Take `acquire`'s lock as `rank`. The order is checked before the call
/// blocks, so a violation fails instead of waiting out `wait`.
fn ordered_lock(
    rank: LockRank,
    acquire: impl FnOnce() -> Result<devmap_store::WriterLock>,
) -> Result<OrderedLock> {
    let guard = RankGuard::enter(rank);
    let lock = acquire()?;
    Ok(OrderedLock {
        _lock: lock,
        _rank: guard,
    })
}

/// The writer lock on `<state>/codeintel/delta.sqlite` for `worktree_root`:
/// every write to the delta holds it.
fn lock_delta_writer_at(worktree_root: &Path, wait: std::time::Duration) -> Result<OrderedLock> {
    let delta_path = stack_delta_path(worktree_root);
    ordered_lock(LockRank::Delta, || Store::lock_writer_at(&delta_path, wait))
}

/// The writer lock on the lane's `devmap.sqlite` path.
fn lock_store_writer_at(store: &Path, wait: std::time::Duration) -> Result<OrderedLock> {
    ordered_lock(LockRank::Store, || Store::lock_writer_at(store, wait))
}

/// The exclusive lock on `<common_git_dir>/devmap/bases/.lock`, held by GC for
/// its whole run and by provision from before publishing until the lease is
/// written. Bounded; the timeout names the holder's pid.
fn lock_bases(common_git_dir: &Path, wait: std::time::Duration) -> Result<OrderedLock> {
    let bases_dir = bases_root_dir(common_git_dir);
    fs::create_dir_all(&bases_dir)
        .with_context(|| format!("cannot create {}", bases_dir.display()))?;
    ordered_lock(LockRank::Bases, || {
        Store::lock_file_at(&bases_dir.join(".lock"), wait)
    })
}

pub fn bases_root_dir(common_git_dir: &Path) -> PathBuf {
    common_git_dir.join("devmap").join("bases")
}

pub fn base_generation_dir(common_git_dir: &Path, key: &BaseKey) -> PathBuf {
    bases_root_dir(common_git_dir).join(key.dir_name())
}

/// Publish or reuse an immutable `chmod 0444` base snapshot at
/// `<common_git_dir>/devmap/bases/gen-<G>-<head12>-<src8>/` from `source_root`'s
/// latest generation (`OQ1`).
///
/// The donor must be a built, committed, clean checkout whose latest generation
/// was indexed at its current `HEAD`: those three facts together mean the base
/// holds exactly `HEAD`'s tree, so the delta (worktree against `HEAD`) is the
/// only difference a lane can have from it.
///
/// The entry is a cache line. Nothing reads it after provision, so one that
/// fails verification is replaced; callers hold the bases lock.
pub fn publish_base_snapshot(source_root: &Path) -> Result<(PathBuf, BaseManifest)> {
    publish_base_snapshot_with(source_root, true)
}

/// [`publish_base_snapshot`]; with `reuse` false an existing entry is replaced
/// without being looked at: the one place a cache entry is removed.
fn publish_base_snapshot_with(source_root: &Path, reuse: bool) -> Result<(PathBuf, BaseManifest)> {
    let source_canon = source_root
        .canonicalize()
        .with_context(|| format!("cannot canonicalize {}", source_root.display()))?;
    let common_git_dir = registered_common_git_dir(&source_canon).ok_or_else(|| {
        anyhow::anyhow!(
            "{} is not a registered git checkout; refusing base snapshot publish",
            source_canon.display()
        )
    })?;
    let source_db = store_path(&source_canon);
    if !source_db.is_file() {
        bail!(
            "donor checkout {} has no devmap store at {}",
            source_canon.display(),
            source_db.display()
        );
    }
    let key = {
        let store = Store::open_read_only(&source_db)
            .with_context(|| format!("cannot read donor store {}", source_db.display()))?;
        let generation_id = store.latest_generation_id()?.ok_or_else(|| {
            anyhow::anyhow!(
                "donor store {} has no indexed generation",
                source_db.display()
            )
        })?;
        let head = store.latest_generation_head_sha()?.unwrap_or_default();
        let epoch = store.pending_watermark()?;
        let key = BaseKey::new(generation_id, &head, &source_canon, epoch.epoch())?;
        assert_donor_is_head_tree(&source_canon, &store, &key.head_sha)?;
        key
    };

    let bases_dir = bases_root_dir(&common_git_dir);
    fs::create_dir_all(&bases_dir)
        .with_context(|| format!("cannot create {}", bases_dir.display()))?;
    let final_dir = base_generation_dir(&common_git_dir, &key);
    if fs::symlink_metadata(&final_dir).is_ok() {
        // A rebuilt donor restarts its generation ids, so its new base can meet
        // an old one of the same name whose bytes differ; an entry can also be
        // of another format or damaged. No lane reads the entry, so none of
        // those is a conflict: replace it.
        if let Some(existing) = reuse
            .then(|| verify_base_dir(&final_dir, &key).ok())
            .flatten()
        {
            return Ok((final_dir, existing));
        }
        remove_readonly_tree(&final_dir)
            .with_context(|| format!("cannot replace base {}", final_dir.display()))?;
    }

    let stage_dir = bases_dir.join(format!(
        ".stage-{}-{}-{}",
        key.dir_name(),
        std::process::id(),
        now_unix_ms()
    ));
    let _ = remove_readonly_tree(&stage_dir);
    fs::create_dir_all(&stage_dir)
        .with_context(|| format!("cannot create {}", stage_dir.display()))?;

    let staged_res = (|| -> Result<BaseManifest> {
        let staged_sqlite = stage_dir.join(BASE_SQLITE);
        stage_seed_healing_wal(&source_db, &staged_sqlite)?;
        prepare_immutable_snapshot(&staged_sqlite)?;
        // The donor may have been edited or rebuilt while it was staged:
        // re-ask both questions, and ask the staged copy what it holds.
        {
            let staged = Store::open_immutable_snapshot(&staged_sqlite, None)
                .context("staged base snapshot failed admission")?;
            let staged_gen = staged.latest_generation_id()?;
            let staged_head = staged.latest_generation_head_sha()?;
            if staged_gen != Some(key.generation_id)
                || staged_head.as_deref() != Some(key.head_sha.as_str())
                || staged.pending_watermark()?.epoch() != key.epoch
            {
                bail!(
                    "donor {} changed while its base was being staged (expected generation {} at {}, staged copy holds {:?} at {:?}); retry",
                    source_canon.display(),
                    key.generation_id,
                    key.head_sha,
                    staged_gen,
                    staged_head
                );
            }
        }
        assert_donor_is_clean_head(&source_canon, &key.head_sha)?;
        let sha256 = sha256_file_hex(&staged_sqlite)?;

        let src_csr = csr_path_for_store(&source_db);
        let staged_csr = csr_path_for_store(&staged_sqlite);
        let csr_sha256 = if src_csr.is_file() && seed_artifact(&src_csr, &staged_csr)?.is_some() {
            set_file_readonly(&staged_csr)?;
            Some(sha256_file_hex(&staged_csr)?)
        } else {
            None
        };

        let manifest = BaseManifest {
            format_version: STACK_FORMAT_VERSION,
            generation_id: key.generation_id,
            schema_version: CURRENT_SCHEMA_VERSION,
            source_repo_root: key.source_repo_root.clone(),
            head_sha: key.head_sha.clone(),
            epoch: key.epoch.clone(),
            created_at_unix_ms: now_unix_ms(),
            base_sqlite_sha256: sha256,
            csr_sha256,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
        write_atomic(&stage_dir.join(MANIFEST), &manifest_bytes)?;
        set_file_readonly(&stage_dir.join(MANIFEST))?;
        Ok(manifest)
    })();

    let manifest = match staged_res {
        Ok(m) => m,
        Err(err) => {
            let _ = remove_readonly_tree(&stage_dir);
            return Err(err);
        }
    };

    match fs::rename(&stage_dir, &final_dir) {
        Ok(()) => {
            // A swap by rename now needs an explicit chmod first.
            set_dir_mode(&final_dir, 0o555)?;
            Ok((final_dir, manifest))
        }
        Err(err) => {
            let _ = remove_readonly_tree(&stage_dir);
            if fs::symlink_metadata(&final_dir).is_ok() {
                // A concurrent publisher won the rename: it must be the same key.
                let existing = verify_base_dir(&final_dir, &key)?;
                Ok((final_dir, existing))
            } else {
                Err(err).with_context(|| {
                    format!(
                        "cannot publish staged base snapshot to {}",
                        final_dir.display()
                    )
                })
            }
        }
    }
}

#[cfg(unix)]
fn set_dir_mode(dir: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(mode))
        .with_context(|| format!("cannot set mode {mode:04o} on {}", dir.display()))
}

#[cfg(not(unix))]
fn set_dir_mode(_dir: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

/// `file`'s sha256 and identity, read from its descriptor (never from a name).
fn file_record(file: &File, sha256: String) -> Result<FileRecord> {
    let meta = file.metadata().context("cannot stat an admitted file")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let nanos = |secs: i64, nsec: i64| {
            u64::try_from(secs)
                .unwrap_or(0)
                .saturating_mul(1_000_000_000)
                .saturating_add(u64::try_from(nsec).unwrap_or(0))
        };
        Ok(FileRecord {
            sha256,
            len: meta.len(),
            mtime_ns: nanos(meta.mtime(), meta.mtime_nsec()),
            ctime_ns: nanos(meta.ctime(), meta.ctime_nsec()),
            ino: meta.ino(),
            dev: meta.dev(),
        })
    }
    #[cfg(not(unix))]
    {
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        Ok(FileRecord {
            sha256,
            len: meta.len(),
            mtime_ns,
            ctime_ns: mtime_ns,
            ino: 0,
            dev: 0,
        })
    }
}

/// Refuse `file` unless it is the one the lease recorded: same length, times,
/// inode and device. An in-place edit that restores `mtime` still moves
/// `ctime`, which user space cannot set.
fn check_identity(file: &File, record: &FileRecord, what: &str, lease_path: &Path) -> Result<()> {
    let now = file_record(file, record.sha256.clone())?;
    if now != *record {
        bail!(
            "the lane's {what} was replaced or modified after it was admitted \
             (recorded len/mtime_ns/ctime_ns/ino/dev {:?}, now {:?}); {}",
            (
                record.len,
                record.mtime_ns,
                record.ctime_ns,
                record.ino,
                record.dev
            ),
            (now.len, now.mtime_ns, now.ctime_ns, now.ino, now.dev),
            restack_hint(lease_path)
        );
    }
    Ok(())
}

/// Open `name` in `dir` by descriptor and refuse it unless it is sealed. With
/// `lease` (the lease path and the identity it recorded) the file must also be
/// the one the lease admitted; a cache entry has no lease and no hint.
fn open_sealed_child(
    dir: &PinnedDir,
    name: &str,
    what: &str,
    lease: Option<(&Path, &FileRecord)>,
) -> Result<File> {
    let hint = lease.map_or_else(String::new, |(path, _)| format!("; {}", restack_hint(path)));
    let file = dir
        .open_file(std::ffi::OsStr::new(name), Access::Read, Creation::Never)
        .with_context(|| format!("cannot open {name} ({what}){hint}"))?
        .try_clone()?;
    let sealed = if lease.is_some() {
        format!("the lane's {what}")
    } else {
        what.to_string()
    };
    require_sealed(&file, &sealed, lease.map(|(path, _)| path))?;
    if let Some((path, record)) = lease {
        check_identity(&file, record, what, path)?;
    }
    Ok(file)
}

/// Refuse a file anyone can write to: an immutable snapshot is `0444`. A lane's
/// own files name the recovery command (`lease_path`); a cache entry's do not.
fn require_sealed(file: &File, what: &str, lease_path: Option<&Path>) -> Result<()> {
    let meta = file
        .metadata()
        .with_context(|| format!("cannot stat {what}"))?;
    if !meta.is_file() {
        bail!("{what} is not a regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.mode() & 0o222 != 0 {
            bail!(
                "{what} is writable (mode {:04o}); refusing to serve it as an immutable snapshot{}",
                meta.mode() & 0o7777,
                lease_path.map_or_else(String::new, |p| format!("; {}", restack_hint(p)))
            );
        }
    }
    Ok(())
}

const MAX_MANIFEST_BYTES: u64 = 1 << 20;

fn read_base_manifest(base_dir: &Path) -> Result<BaseManifest> {
    let manifest_path = base_dir.join(MANIFEST);
    let unreadable = || format!("cannot read base manifest {}", manifest_path.display());
    let file = fs::File::open(&manifest_path).with_context(unreadable)?;
    // A real manifest is under a kilobyte; refuse to slurp a hostile one.
    let mut raw = Vec::new();
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut raw)
        .with_context(unreadable)?;
    parse_manifest(&raw, &manifest_path)
}

fn parse_manifest(raw: &[u8], manifest_path: &Path) -> Result<BaseManifest> {
    if raw.len() as u64 > MAX_MANIFEST_BYTES {
        bail!(
            "base manifest {} is larger than {MAX_MANIFEST_BYTES} bytes; refusing to read it",
            manifest_path.display()
        );
    }
    serde_json::from_slice(raw).with_context(|| {
        format!(
            "base manifest {} is legacy or malformed",
            manifest_path.display()
        )
    })
}

/// Compare a base's recorded identity with the one a caller expects. Never
/// returns `Ok` on a mismatch.
fn check_manifest_matches(manifest: &BaseManifest, expected: &BaseKey, dir: &Path) -> Result<()> {
    let mismatch = |field: &str, recorded: String, wanted: String| {
        anyhow::anyhow!(
            "base snapshot {} does not match its key: {field} is {recorded} but {wanted} was expected",
            dir.display()
        )
    };
    if manifest.format_version != STACK_FORMAT_VERSION {
        return Err(mismatch(
            "format_version",
            manifest.format_version.to_string(),
            STACK_FORMAT_VERSION.to_string(),
        ));
    }
    if manifest.schema_version != CURRENT_SCHEMA_VERSION {
        return Err(mismatch(
            "schema_version",
            manifest.schema_version.to_string(),
            CURRENT_SCHEMA_VERSION.to_string(),
        ));
    }
    if manifest.generation_id != expected.generation_id {
        return Err(mismatch(
            "generation_id",
            manifest.generation_id.to_string(),
            expected.generation_id.to_string(),
        ));
    }
    if manifest.head_sha != expected.head_sha {
        return Err(mismatch(
            "head_sha",
            manifest.head_sha.clone(),
            expected.head_sha.clone(),
        ));
    }
    if manifest.source_repo_root != expected.source_repo_root {
        return Err(mismatch(
            "source_repo_root",
            manifest.source_repo_root.clone(),
            expected.source_repo_root.clone(),
        ));
    }
    if manifest.epoch != expected.epoch {
        return Err(mismatch(
            "epoch",
            manifest.epoch.clone(),
            expected.epoch.clone(),
        ));
    }
    Ok(())
}

/// Reuse check for a cache entry that already exists under `expected`'s name,
/// run under the bases lock: the manifest's fields, and the recomputed sha256 of
/// each file it records. What the SQLite holds is checked at admission, which
/// reads it through descriptors; an entry that fails there is replaced once.
fn verify_base_dir(base_dir: &Path, expected: &BaseKey) -> Result<BaseManifest> {
    let manifest = read_base_manifest(base_dir)?;
    check_manifest_matches(&manifest, expected, base_dir)?;
    for (name, recorded) in [
        (BASE_SQLITE, Some(&manifest.base_sqlite_sha256)),
        (BASE_CSR, manifest.csr_sha256.as_ref()),
    ] {
        let Some(recorded) = recorded else { continue };
        let path = base_dir.join(name);
        let actual = sha256_file_hex(&path)?;
        if actual != *recorded {
            bail!(
                "{} fails its sha256 check: recorded {recorded} but the file hashes to {actual}",
                path.display()
            );
        }
    }
    Ok(manifest)
}

/// What a store's latest generation row holds, read from the store itself
/// (never through a caller's root override): what [`check_lease_matches_base`]
/// compares a lease's claims with. Provision-time admission
/// ([`check_store_holds_key`]) reads the store's own accessors instead.
struct StoredIdentity {
    generation: Option<u32>,
    head: Option<String>,
    root: Option<String>,
}

impl StoredIdentity {
    fn read(store: &Store) -> Result<Self> {
        Ok(Self {
            generation: store.latest_generation_id()?,
            head: store.latest_generation_head_sha()?,
            root: store.stored_latest_generation_repo_root()?,
        })
    }
}

/// Whether two spellings name one root, without needing either to exist. Used
/// only by [`check_lease_matches_base`]; provision-time admission keeps its own
/// stricter comparison in [`check_store_holds_key`]:
/// the longest existing ancestor of each is canonicalized (`/tmp` and
/// `/private/tmp` are one directory) and the rest kept as written. A donor
/// that was moved or deleted after provision still has one spelling.
fn same_root(a: &Path, b: &Path) -> bool {
    canonicalize_existing_prefix(a) == canonicalize_existing_prefix(b)
}

fn canonicalize_existing_prefix(path: &Path) -> PathBuf {
    let mut tail = Vec::new();
    let mut head = path;
    loop {
        if let Ok(canon) = head.canonicalize() {
            return tail.iter().rev().fold(canon, |acc, part| acc.join(part));
        }
        match (head.parent(), head.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name);
                head = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Refuse a store whose latest generation, `HEAD`, root or creation epoch is not `key`'s.
fn check_store_holds_key(store: &Store, key: &BaseKey, what: &str) -> Result<()> {
    let root = store.latest_generation_repo_root()?;
    let root_matches = root.as_deref().is_some_and(|r| {
        Path::new(r).canonicalize().ok().as_deref() == Some(Path::new(&key.source_repo_root))
    });
    let (generation, head) = (
        store.latest_generation_id()?,
        store.latest_generation_head_sha()?,
    );
    let epoch = store.pending_watermark()?;
    if generation != Some(key.generation_id)
        || head.as_deref() != Some(key.head_sha.as_str())
        || !root_matches
        || epoch.epoch() != key.epoch
    {
        bail!(
            "{what} holds generation {generation:?} at {head:?} for {root:?} (store epoch {}), \
             not generation {} at {} for {} (store epoch {})",
            epoch.epoch(),
            key.generation_id,
            key.head_sha,
            key.source_repo_root,
            key.epoch
        );
    }
    Ok(())
}

/// Refuse a lease whose claims about its base disagree with the base's own
/// latest generation row: `generation_id`, `head_sha` and `source_repo_root`.
/// `check_identity` pins the file's stat tuple to the lease, but the lease is
/// plain JSON; without this a lease can say one thing while the store it admits
/// holds another, and `status` and every answer would carry the lease's word.
/// (The lease has no epoch, so unlike [`check_store_holds_key`] it checks none.)
///
/// One `SELECT` per field on the open connection: no git, no file hashing.
///
/// What is and is not verified about the base's sha256 (read from the code, not
/// assumed): it is computed at publish (`publish_base_snapshot`), re-checked
/// when a cache entry is reused (`verify_base_dir`) and again on the lane's own
/// clone at admission (`admit_published_base`), before the lease is written.
/// After that the lane's file is held to the lease only by its stat tuple
/// (`check_identity`), at open and at `materialize_seeded_store`, which
/// re-hashes the CSR sidecar (`verify_csr_content`) but not the base. Ruling
/// ft-3abcc3b9 (c), 2026-11-10: the stat identity pins the base bytes;
/// `base.sha256` is recorded at admission and advisory after it, so open does
/// not recompute it (it cannot inside the hook's 400 ms budget). Drop the field
/// at the next STACK_FORMAT_VERSION bump; do not bump for it alone. Agreement
/// between lease and store is consistency, not authentication: whoever can
/// rewrite both the lease and a sealed store can still present a store of
/// their choosing.
///
/// The root is compared by [`same_root`], which does not need the donor's path
/// to exist: the lane's base is its own clone and must open after the donor is
/// moved or deleted. Provision always writes a canonical absolute root, so a
/// lease root that is relative (no leading `/`, or a leading `.`) or has a `..`
/// component is refused before the comparison: `canonicalize` would resolve it
/// against the process's cwd. A non-leading `.` (`<root>/.`) is not refused:
/// `Path::components` normalises it away, so it resolves to the same root.
///
/// Every lease value this echoes goes through [`lease_text`].
fn check_lease_matches_base(store: &Store, lease: &StackLease, lease_path: &Path) -> Result<()> {
    let held = StoredIdentity::read(store)?;
    let mismatch = |field: &str, lease_says: &str, store_holds: String| {
        anyhow::anyhow!(
            "the stack lease does not match the lane's base snapshot: lease {field} is {} \
             but the base holds {}; {}",
            lease_text(lease_says),
            lease_text(&store_holds),
            restack_hint(lease_path)
        )
    };
    if held.generation != Some(lease.generation_id) {
        return Err(mismatch(
            "generation_id",
            &lease.generation_id.to_string(),
            format!("{:?}", held.generation),
        ));
    }
    if held.head.as_deref() != Some(lease.head_sha.as_str()) {
        return Err(mismatch(
            "head_sha",
            &lease.head_sha,
            format!("{:?}", held.head),
        ));
    }
    let claimed = Path::new(&lease.source_repo_root);
    let not_canonical_shape = !claimed.is_absolute()
        || claimed.components().any(|c| {
            matches!(
                c,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        });
    let root_matches = !not_canonical_shape
        && held
            .root
            .as_deref()
            .is_some_and(|stored| same_root(Path::new(stored), claimed));
    if !root_matches {
        return Err(mismatch(
            "source_repo_root",
            &lease.source_repo_root,
            format!("{:?}", held.root),
        ));
    }
    Ok(())
}

/// Longest lease value an error repeats, in bytes, before [`lease_text`] cuts it.
const LEASE_ECHO_BYTES: usize = 200;

/// A lease value made safe to put in an error: control and bidi characters
/// escaped, cut at [`LEASE_ECHO_BYTES`] with a marker naming the cut. The lease
/// is a plain file anyone with write access to the worktree can rewrite.
fn lease_text(text: &str) -> String {
    crate::bounded_text(text, LEASE_ECHO_BYTES)
}

/// The most a lease may weigh. A real lease is under a kilobyte (two paths, a
/// sha, three file records); anything past this is refused unread, with the
/// restack hint, like an oversized base manifest.
const MAX_LEASE_BYTES: u64 = 64 * 1024;

/// Read the lease file, never more than [`MAX_LEASE_BYTES`] of it.
fn read_lease_bytes(lease_path: &Path) -> Result<Vec<u8>> {
    let unreadable = || {
        format!(
            "cannot read stack lease {}; {}",
            lease_path.display(),
            restack_hint(lease_path)
        )
    };
    let file = fs::File::open(lease_path).with_context(unreadable)?;
    let mut raw = Vec::new();
    file.take(MAX_LEASE_BYTES + 1)
        .read_to_end(&mut raw)
        .with_context(unreadable)?;
    if raw.len() as u64 > MAX_LEASE_BYTES {
        bail!(
            "stack lease {} is larger than {MAX_LEASE_BYTES} bytes; refusing to read it; {}",
            lease_path.display(),
            restack_hint(lease_path)
        );
    }
    Ok(raw)
}

/// A base admitted into a lane: the files are the lane's own clones.
struct AdmittedBase {
    sqlite: FileRecord,
    csr: Option<FileRecord>,
}

fn require_child_absent(dir: &PinnedDir, name: &str, what: &str) -> Result<()> {
    match dir.inspect_child(std::ffi::OsStr::new(name)) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => bail!("{what} has an unexpected {name}"),
        Err(err) => bail!("cannot inspect {name} of {what}: {err}"),
    }
}

/// Admit one cache entry into the lane directory `private`, reading the entry
/// only through descriptors: the entry's directory is pinned with
/// `O_DIRECTORY|O_NOFOLLOW`, each file is opened relative to it and checked
/// (regular, `0444`, no `-wal`/`-shm`/`-journal`), and what is cloned out is the
/// open file, not a name. The clone is then held to `published`, the manifest
/// [`publish_base_snapshot`] returned (whose sha256s it computed itself; the
/// manifest inside the entry is never read here), and the SQLite handle opened
/// on the clone must report the donor's generation, `HEAD` and root.
///
/// So a swap of the entry's directory or files at any moment leaves the lane
/// with either the admitted bytes or a refusal.
fn admit_published_base(
    entry: &Path,
    key: &BaseKey,
    published: &BaseManifest,
    private: &Path,
) -> Result<AdmittedBase> {
    let dir = PinnedDir::open(entry, false)
        .with_context(|| format!("cannot pin published base {}", entry.display()))?;
    let what = format!("published base {}", entry.display());
    for suffix in ["-wal", "-shm", "-journal"] {
        require_child_absent(&dir, &format!("{BASE_SQLITE}{suffix}"), &what)?;
    }
    let open_entry = |name: &str| open_sealed_child(&dir, name, &format!("published {name}"), None);
    let sqlite_src = open_entry(BASE_SQLITE)?;
    let csr_src = match published.csr_sha256 {
        Some(_) => Some(open_entry(BASE_CSR)?),
        None => {
            require_child_absent(&dir, BASE_CSR, &what)?;
            None
        }
    };

    // Clone the open file, then admit the clone: it is a separate inode in the
    // lane's own directory, so what is hashed is what will be served.
    let admit_clone = |source: &File,
                       name: &str,
                       expected_sha: &str|
     -> Result<(File, PathBuf, FileRecord)> {
        let path = private.join(name);
        clone_or_copy_file(source, &path)?;
        set_file_readonly(&path)?;
        let clone = File::open(&path).with_context(|| format!("cannot open {}", path.display()))?;
        let sha = sha256_of_open_file(&clone)?;
        if sha != expected_sha {
            bail!(
                "{what} holds {name} bytes that hash to {sha}, not the {expected_sha} publish verified; \
                 the cache entry changed while it was being admitted"
            );
        }
        let record = file_record(&clone, sha)?;
        Ok((clone, path, record))
    };

    let (sqlite, sqlite_path, sqlite_record) =
        admit_clone(&sqlite_src, BASE_SQLITE, &published.base_sqlite_sha256)?;
    {
        let admitted = Store::open_immutable_snapshot_file(&sqlite, &sqlite_path, None)
            .context("the cloned base failed admission")?;
        check_store_holds_key(&admitted, key, &format!("the clone of {what}"))
            .context("the cache entry changed while it was being admitted")?;
    }
    let csr_record = match (csr_src, &published.csr_sha256) {
        (Some(src), Some(expected)) => Some(admit_clone(&src, BASE_CSR, expected)?.2),
        _ => None,
    };
    Ok(AdmittedBase {
        sqlite: sqlite_record,
        csr: csr_record,
    })
}

const GIT_BOUNDS: devmap_extract::subprocess::Bounds = devmap_extract::subprocess::Bounds {
    deadline: std::time::Duration::from_secs(10),
    stdout_cap: 8 * 1024 * 1024,
    stderr_cap: 64 * 1024,
};

/// Run git in `root` under bounds. Every failure (spawn, deadline, non-success,
/// output cap) is an `Err`: a git that could not answer must never read as an
/// empty change set.
fn run_git_checked(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    run_git_within(root, args, GIT_BOUNDS.deadline)
}

/// [`run_git_checked`] with the wall-clock bound named, capped at the default.
fn run_git_within(root: &Path, args: &[&str], deadline: std::time::Duration) -> Result<Vec<u8>> {
    use devmap_extract::subprocess::{git, run_bounded};
    let bounds = devmap_extract::subprocess::Bounds {
        deadline: deadline.min(GIT_BOUNDS.deadline),
        ..GIT_BOUNDS
    };
    let mut cmd = git(root, args.iter().copied())
        .map_err(|e| anyhow::anyhow!("git {args:?} in {}: {e}", root.display()))?;
    let out = run_bounded(&mut cmd, bounds)
        .map_err(|e| anyhow::anyhow!("git {args:?} in {}: {e}", root.display()))?;
    if !out.status.success() {
        bail!(
            "git {args:?} failed in {} ({}): {}",
            root.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    if out.stdout_truncated {
        bail!(
            "git {args:?} in {} produced more than {} bytes; the answer would be incomplete",
            root.display(),
            GIT_BOUNDS.stdout_cap
        );
    }
    Ok(out.stdout)
}

fn git_head_sha(root: &Path) -> Result<String> {
    let out = run_git_checked(root, &["rev-parse", "HEAD"])?;
    let head = String::from_utf8(out)
        .map_err(|_| anyhow::anyhow!("git rev-parse HEAD in {} is not UTF-8", root.display()))?
        .trim()
        .to_string();
    if !is_commit_sha(&head) {
        bail!("git rev-parse HEAD in {} returned {head:?}", root.display());
    }
    Ok(head)
}

/// Repository-relative paths `git status` lists in `root`, tracked and
/// untracked, with rename and copy origins. A non-UTF-8 path is an `Err`.
fn git_status_paths(root: &Path) -> Result<BTreeSet<String>> {
    git_status_paths_within(root, GIT_BOUNDS.deadline)
}

fn git_status_paths_within(root: &Path, deadline: std::time::Duration) -> Result<BTreeSet<String>> {
    let out = run_git_within(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        deadline,
    )?;
    if !out.is_empty() && out.last() != Some(&0) {
        bail!(
            "git status in {} returned output that is not NUL-terminated porcelain: {:?}",
            root.display(),
            String::from_utf8_lossy(&out[..out.len().min(80)])
        );
    }
    let mut paths = BTreeSet::new();
    let mut entries = out.split(|&b| b == 0);
    while let Some(entry) = entries.next() {
        if entry.is_empty() {
            continue;
        }
        let is_code = |b: u8| {
            matches!(
                b,
                b' ' | b'M' | b'T' | b'A' | b'D' | b'R' | b'C' | b'U' | b'?' | b'!'
            )
        };
        if entry.len() < 4 || !is_code(entry[0]) || !is_code(entry[1]) || entry[2] != b' ' {
            bail!(
                "git status in {} returned a line that is not porcelain v1 ({:?})",
                root.display(),
                String::from_utf8_lossy(&entry[..entry.len().min(80)])
            );
        }
        let status = &entry[..2];
        paths.insert(status_path(root, &entry[3..])?);
        if status.contains(&b'R') || status.contains(&b'C') {
            let orig = entries.next().ok_or_else(|| {
                anyhow::anyhow!(
                    "git status in {} lists a rename with no origin path",
                    root.display()
                )
            })?;
            paths.insert(status_path(root, orig)?);
        }
    }
    Ok(paths)
}

fn status_path(root: &Path, raw: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(raw).map_err(|_| {
        anyhow::anyhow!(
            "git lists a non-UTF-8 path in {} ({:?}); it cannot be tracked in a stacked delta",
            root.display(),
            String::from_utf8_lossy(raw)
        )
    })?;
    normalize_rel_path(text)
}

fn is_tool_state_path(path: &str) -> bool {
    path == ".devmap"
        || path == ".devcouncil"
        || path.starts_with(".devmap/")
        || path.starts_with(".devcouncil/")
}

#[derive(Debug, Default, Deserialize)]
struct LegacyBaseStoreRef {
    #[serde(default)]
    anchor_repo_root: Option<String>,
    #[serde(default, alias = "base_store_path", alias = "base_sqlite_path")]
    anchor_db_path: Option<String>,
}

/// Inspect `<worktree_root>/<state>/codeintel/base_store_ref.json` (if present)
/// and return the registered anchor checkout root it names when that checkout
/// is a different registered worktree of the same repository and has a store.
/// A hint for the refusal's upgrade command: it only reads, and every failure
/// is "no hint", never a reason to trust the pointer.
pub fn legacy_base_store_anchor_root(worktree_root: &Path) -> Option<PathBuf> {
    let ref_path = find_legacy_base_store_ref(worktree_root)?;
    let meta = fs::symlink_metadata(&ref_path).ok()?;
    if !meta.is_file() || meta.len() > 64 * 1024 {
        return None;
    }
    let raw = fs::read_to_string(&ref_path).ok()?;
    let parsed: LegacyBaseStoreRef = serde_json::from_str(&raw).ok()?;
    let anchor_raw = parsed
        .anchor_repo_root
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            parsed
                .anchor_db_path
                .filter(|s| !s.trim().is_empty())
                .and_then(|p| repo_root_from_store(Path::new(&p)))
        })?;
    let anchor_canon = anchor_raw.canonicalize().ok()?;
    let wt_canon = worktree_root.canonicalize().ok()?;
    if anchor_canon == wt_canon || !same_registered_repo(&anchor_canon, &wt_canon) {
        return None;
    }
    store_path(&anchor_canon).is_file().then_some(anchor_canon)
}

/// Fail closed on a retired `base_store_ref.json` pointer without writing or
/// upgrading anything on a read. A lane that has a store or a lease is served
/// by those, so the pointer is moot and this answers `Ok`.
pub fn refuse_legacy_base_store_ref(worktree_root: &Path) -> Result<()> {
    let wt_canon = worktree_root
        .canonicalize()
        .unwrap_or_else(|_| worktree_root.to_path_buf());
    if store_path(&wt_canon).is_file() || stack_lease_path(&wt_canon).is_file() {
        return Ok(());
    }
    let Some(ref_path) = find_legacy_base_store_ref(&wt_canon) else {
        return Ok(());
    };
    let donor_hint = legacy_base_store_anchor_root(&wt_canon)
        .or_else(|| main_checkout_of(&wt_canon))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<main>".to_string());
    bail!(
        "retired legacy worktree pointer {} is no longer accepted on read; \
         run `devmap seed --stacked --from {donor_hint}` (or `devmap build --manifest`) to upgrade",
        ref_path.display()
    )
}

/// Refuse unless `source`'s `HEAD` is `expected_head` and its tracked and
/// untracked files are clean against it (tool-owned state excepted).
fn assert_donor_is_clean_head(source: &Path, expected_head: &str) -> Result<()> {
    let remedy = format!(
        "commit or stash in {}, run devmap build there, or pass --from a clean checkout",
        source.display()
    );
    let head = git_head_sha(source)?;
    if head != expected_head {
        bail!(
            "donor {} is at HEAD {head} but its index was built at {expected_head}; {remedy}",
            source.display()
        );
    }
    let dirty: Vec<String> = git_status_paths(source)?
        .into_iter()
        .filter(|p| !is_tool_state_path(p))
        .collect();
    if !dirty.is_empty() {
        let shown: Vec<&str> = dirty.iter().take(8).map(String::as_str).collect();
        bail!(
            "donor {} has {} uncommitted path(s) ({}{}); a stacked base must equal HEAD's tree; {remedy}",
            source.display(),
            dirty.len(),
            shown.join(", "),
            if dirty.len() > shown.len() { ", ..." } else { "" }
        );
    }
    Ok(())
}

/// The three facts that together mean "the base's indexed content is `HEAD`'s
/// tree": the donor is clean against `HEAD`, its latest generation matches the
/// working tree, and that generation's `head_sha` is `HEAD`.
fn assert_donor_is_head_tree(source: &Path, store: &Store, generation_head: &str) -> Result<()> {
    let remedy = format!(
        "commit or stash in {}, run devmap build there, or pass --from a clean checkout",
        source.display()
    );
    assert_donor_is_clean_head(source, generation_head)?;
    match store.latest_generation_repo_root()? {
        Some(root) if Path::new(&root).canonicalize().ok().as_deref() == Some(source) => {}
        other => bail!(
            "donor store at {} was indexed for {other:?}, not for this checkout; {remedy}",
            source.display()
        ),
    }
    // Asked the same way with or without the parsing frontend: the comparison
    // is a scan and a manifest pass, and neither reaches a grammar.
    match store.latest_generation_matches_working_tree() {
        Ok(true) => Ok(()),
        Ok(false) => bail!(
            "donor {} has changed since its index was built (the latest generation does not match the working tree); {remedy}",
            source.display()
        ),
        Err(err) => bail!(
            "cannot confirm donor {}'s index matches its working tree: {err}; {remedy}",
            source.display()
        ),
    }
}

/// Provision a Glean-style stacked worktree lane at `worktree_root` backed by
/// `source_root`'s base snapshot (`P2.1`, `P2.2`, Critique 1).
///
/// Under the bases lock for its whole run: publish (or reuse) the cache entry,
/// admit it into the lane's private `stack/` directory through pinned
/// descriptors, and only then write the lease. Re-provisioning replaces the
/// lane's previous lease and base; a lane that has its own store is refused.
pub fn provision_stacked_worktree(source_root: &Path, worktree_root: &Path) -> Result<StackLease> {
    let source_canon = source_root
        .canonicalize()
        .with_context(|| format!("cannot canonicalize {}", source_root.display()))?;
    let wt_canon = worktree_root
        .canonicalize()
        .with_context(|| format!("cannot canonicalize {}", worktree_root.display()))?;
    if !same_registered_repo(&source_canon, &wt_canon) {
        bail!(
            "refusing stacked provisioning: {} and {} are not registered worktrees of the same repository",
            source_canon.display(),
            wt_canon.display()
        );
    }
    let lane_store = store_path(&wt_canon);
    if fs::symlink_metadata(&lane_store).is_ok() {
        bail!(
            "refusing stacked provisioning: {} already has its own store {}; a stacked lane has none \
             (remove the store to stack it, or keep using it as a plain worktree)",
            wt_canon.display(),
            lane_store.display()
        );
    }

    let common_git_dir = registered_common_git_dir(&source_canon).ok_or_else(|| {
        anyhow::anyhow!(
            "{} is not a registered git checkout; refusing stacked provisioning",
            source_canon.display()
        )
    })?;
    // A lane that already has a delta may be mid-sync: wait for that writer
    // before touching the lane, and before the bases lock (delta, then bases).
    let _delta_lock = if stack_delta_path(&wt_canon).exists() {
        Some(lock_delta_writer_at(&wt_canon, Store::WRITER_LOCK_WAIT)?)
    } else {
        None
    };
    let _bases_lock = lock_bases(&common_git_dir, BASES_LOCK_WAIT)?;
    let (published, manifest) = publish_base_snapshot(&source_canon)?;
    let key = BaseKey::of_manifest(&manifest)?;

    let final_stack = stack_dir(&wt_canon);
    let lease_path = stack_lease_path(&wt_canon);
    let stage = final_stack.with_file_name(format!(
        "stack.stage-{}-{}",
        std::process::id(),
        now_unix_ms()
    ));

    // Everything that can fail comes before the lease: a failed provision must
    // not leave a leased lane behind a non-zero exit. `delta.sqlite` is not
    // created here either; only a sync or a build creates it.
    let admit_once = |entry: &Path, verified: &BaseManifest| -> Result<AdmittedBase> {
        let _ = fs::remove_dir_all(&stage);
        fs::create_dir_all(&stage).with_context(|| format!("cannot create {}", stage.display()))?;
        admit_published_base(entry, &key, verified, &stage)
    };
    let admitted = admit_once(&published, &manifest)
        .or_else(|first| {
            // The entry held what its manifest said but not what the donor
            // built (an impostor, or a rebuilt donor that reused the name):
            // publish replaces it, and the fresh one is admitted once more.
            let (entry, verified) = publish_base_snapshot_with(&source_canon, false)?;
            admit_once(&entry, &verified).with_context(|| {
                format!("the replacement cache entry was refused too (first refusal: {first:#})")
            })
        })
        .and_then(|admitted| {
            seed_derived_artifacts_and_reuse_caches(&source_canon, &wt_canon)?;
            Ok(admitted)
        });
    let admitted = match admitted {
        Ok(admitted) => admitted,
        Err(err) => {
            let _ = fs::remove_dir_all(&stage);
            return Err(err);
        }
    };

    let lease = StackLease {
        format_version: STACK_FORMAT_VERSION,
        generation_id: key.generation_id,
        head_sha: key.head_sha.clone(),
        source_repo_root: key.source_repo_root.clone(),
        base: admitted.sqlite,
        csr: admitted.csr,
        worktree_root: wt_canon.to_string_lossy().into_owned(),
        leased_at_unix_ms: now_unix_ms(),
        rebases_after_dirty_files: DEFAULT_REBASE_DIRTY_FILES,
        rebases_after_delta_bytes: DEFAULT_REBASE_DELTA_BYTES,
    };
    let placed = (|| -> Result<()> {
        // The previous lease goes first: between here and the new lease the
        // lane is plainly unstacked, never leased to a base that is not there.
        remove_lease_and_base(&lease_path, &final_stack)?;
        fs::rename(&stage, &final_stack)
            .with_context(|| format!("cannot place {}", final_stack.display()))?;
        write_atomic(&lease_path, &serde_json::to_vec_pretty(&lease)?)
            .map(|_| ())
            .with_context(|| format!("cannot write the lease {}", lease_path.display()))
            .inspect_err(|_| {
                let _ = fs::remove_dir_all(&final_stack);
            })
    })();
    if let Err(err) = placed {
        let _ = fs::remove_dir_all(&stage);
        return Err(err.context(restack_hint(&lease_path)));
    }
    Ok(lease)
}

/// Clone warm derived artifacts (`repo_map.json`, `code_graph.json`,
/// `compact_code_graph.json`) and any `resolution-reuse-*` directory from
/// `source_root` to `worktree_root` without copying `<db>.artifacts.json` (`F9`).
pub fn seed_derived_artifacts_and_reuse_caches(
    source_root: &Path,
    worktree_root: &Path,
) -> Result<()> {
    let _ = seed_artifact(&repo_map_path(source_root), &repo_map_path(worktree_root))?;
    let _ = seed_artifact(
        &code_graph_path(source_root),
        &code_graph_path(worktree_root),
    )?;
    let _ = seed_artifact(
        &compact_code_graph_path(source_root),
        &compact_code_graph_path(worktree_root),
    )?;
    let src_state = state_dir(source_root);
    let dst_state = state_dir(worktree_root);
    if let Ok(entries) = fs::read_dir(&src_state) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if let Some(s) = name.to_str() {
                if s.starts_with("resolution-reuse-") && entry.path().is_dir() {
                    let _ = seed_directory_cow(&entry.path(), &dst_state.join(s))?;
                }
            }
        }
    }
    Ok(())
}

/// Garbage-collect superseded `gen-<old>` entries under
/// `<common_git_dir>/devmap/bases/`: the newest generation of each donor stays
/// published, every other entry is deleted.
///
/// No lease is read. A lane serves its own private clone, so no entry here is
/// ever "in use" and GC of a lane's source entry cannot affect the lane.
pub fn gc_unused_bases(common_git_dir: &Path) -> Result<GcBasesReport> {
    gc_unused_bases_waiting(common_git_dir, BASES_LOCK_WAIT)
}

/// [`gc_unused_bases`] with the lock wait named, so a held lock is a testable
/// refusal rather than a minute's wait.
pub fn gc_unused_bases_waiting(
    common_git_dir: &Path,
    wait: std::time::Duration,
) -> Result<GcBasesReport> {
    let common_canon = common_git_dir
        .canonicalize()
        .with_context(|| format!("cannot canonicalize {}", common_git_dir.display()))?;
    let bases_dir = bases_root_dir(&common_canon);
    if !bases_dir.is_dir() {
        return Ok(GcBasesReport {
            retained_generations: Vec::new(),
            removed_generations: Vec::new(),
        });
    }

    let _bases_lock = lock_bases(&common_canon, wait)?;

    // Only `gen-<G>-<head12>-<src8>` directories are ours to judge; legacy
    // `gen-<G>` directories and anything else are left alone.
    let mut discovered: Vec<(u32, String, String, PathBuf)> = Vec::new();
    for entry in fs::read_dir(&bases_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if let Some((gen_id, src8)) = parse_base_dir_name(name_str) {
            discovered.push((gen_id, src8, name_str.to_string(), entry.path()));
        }
    }
    discovered.sort_by(|a, b| (a.0, &a.2).cmp(&(b.0, &b.2)));
    // The newest generation of each donor stays published.
    let mut newest_per_source: std::collections::BTreeMap<String, u32> =
        std::collections::BTreeMap::new();
    for (gen_id, src8, _, _) in &discovered {
        let slot = newest_per_source.entry(src8.clone()).or_insert(*gen_id);
        *slot = (*slot).max(*gen_id);
    }

    let mut retained_generations = Vec::new();
    let mut removed_generations = Vec::new();
    for (gen_id, src8, _, dir_path) in discovered {
        if newest_per_source.get(&src8) == Some(&gen_id) {
            retained_generations.push(gen_id);
        } else {
            remove_readonly_tree(&dir_path)
                .with_context(|| format!("cannot remove superseded base {}", dir_path.display()))?;
            removed_generations.push(gen_id);
        }
    }

    Ok(GcBasesReport {
        retained_generations,
        removed_generations,
    })
}

fn remove_readonly_tree(dir: &Path) -> Result<()> {
    if fs::symlink_metadata(dir).is_err() {
        return Ok(());
    }
    make_tree_writable_best_effort(dir);
    fs::remove_dir_all(dir)?;
    Ok(())
}

fn make_tree_writable_best_effort(path: &Path) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return;
    };
    if meta.file_type().is_symlink() {
        return;
    }
    if meta.is_dir() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
        }
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                make_tree_writable_best_effort(&entry.path());
            }
        }
    } else {
        let _ = set_file_writable(path);
    }
}

/// A lane's lease and its base, admitted for reading: the one place a lane's
/// base is opened, shared by [`StackedStore::open`] and the MCP engine.
struct LaneBase {
    lease: StackLease,
    lease_path: PathBuf,
    pinned: PinnedDir,
    base_file: File,
    base: Store,
}

/// Read the lease of the canonical worktree `wt_canon`, refuse it unless it is
/// this lane's, and open the lane's own `base.sqlite`: its directory by
/// descriptor, the file checked against the identity the lease admitted, the
/// connection bound to that descriptor. Nothing under `bases/` is named.
fn open_lane_base(wt_canon: &Path, repo_root_override: Option<String>) -> Result<LaneBase> {
    let lease_path = stack_lease_path(wt_canon);
    let hint = restack_hint(&lease_path);
    let lease = parse_lease(&read_lease_bytes(&lease_path)?, &lease_path)?;

    // The lease must belong to the lane being opened: one copied in from
    // another lane or repository records another root.
    let lease_root = Path::new(&lease.worktree_root).canonicalize().ok();
    if lease_root.as_deref() != Some(wt_canon) {
        bail!(
            "stack lease in {} was issued for {} (refusing a lease that belongs to another lane); {hint}",
            wt_canon.display(),
            lease_text(&lease.worktree_root)
        );
    }

    let stack = stack_dir(wt_canon);
    let pinned = PinnedDir::open(&stack, false).with_context(|| {
        format!(
            "cannot open the lane's base directory {}; {hint}",
            stack.display()
        )
    })?;
    let base_sqlite_path = stack.join(BASE_SQLITE);
    let base_file = open_sealed_child(
        &pinned,
        BASE_SQLITE,
        "base snapshot",
        Some((&lease_path, &lease.base)),
    )?;
    let base =
        Store::open_immutable_snapshot_file(&base_file, &base_sqlite_path, repo_root_override)
            .with_context(|| {
                format!(
                    "cannot open the lane's base snapshot {}; {hint}",
                    base_sqlite_path.display()
                )
            })?;
    check_lease_matches_base(&base, &lease, &lease_path)?;
    Ok(LaneBase {
        lease,
        lease_path,
        pinned,
        base_file,
        base,
    })
}

/// Open the base of the lane whose private `base.sqlite` is `base_sqlite`, with
/// the same lease and identity checks as [`StackedStore::open`], for readers
/// that want only the SQLite store (the MCP engine) and are handed a path.
pub fn open_lane_base_store(base_sqlite: &Path) -> Result<Store> {
    let lease_path = base_sqlite
        .parent()
        .and_then(Path::parent)
        .map(|codeintel| codeintel.join("stack_lease.json"))
        .with_context(|| {
            format!(
                "{} is not inside a lane's stack/ directory",
                base_sqlite.display()
            )
        })?;
    let lease = StackLease::read_from(&lease_path)?;
    let wt_canon = Path::new(&lease.worktree_root)
        .canonicalize()
        .with_context(|| {
            format!(
                "cannot canonicalize {}; {}",
                lease_text(&lease.worktree_root),
                restack_hint(&lease_path)
            )
        })?;
    if stack_lease_path(&wt_canon) != lease_path {
        bail!(
            "{} is not the lane's own base for lease {}; {}",
            base_sqlite.display(),
            lease_path.display(),
            restack_hint(&lease_path)
        );
    }
    open_lane_base(&wt_canon, None).map(|lane| lane.base)
}

/// What `seed --sync-delta` found in a lane.
pub enum SyncTarget {
    /// A live stacked lane to sync.
    Lane(Box<StackedStore>),
    /// The lane has its own store, so its stack lease was stale; it was removed.
    RetiredStaleLease,
    /// No stack lease at all.
    NoLease,
}

pub struct StackedStore {
    worktree_root: PathBuf,
    lease: StackLease,
    /// The lane's own `base.sqlite`, held open: the connection, a materialize
    /// and any re-check all go through this descriptor, never a name.
    base_file: File,
    csr_file: Option<File>,
    base: Store,
    /// `None` until `delta.sqlite` exists. A read path (hook, MCP, status) never
    /// creates or migrates it: a missing delta reads as an empty one.
    delta: Option<DeltaStore>,
    delta_writable: bool,
    overlay: WorktreeOverlay,
    /// The base edge index merged with the delta's edges, built on first use and
    /// dropped whenever the overlay is rebuilt. Unused while the delta is empty.
    merged_edges: Mutex<Option<(u32, Arc<GenerationEdges>)>>,
}

impl std::fmt::Debug for StackedStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StackedStore")
            .field("worktree_root", &self.worktree_root)
            .field("generation_id", &self.lease.generation_id)
            .field("dirty_files", &self.overlay.modified_files.len())
            .finish()
    }
}

impl StackedStore {
    /// Open the stacked worktree store declared by `<worktree_root>/<state>/codeintel/stack_lease.json`.
    ///
    /// Names nothing under the shared `bases/` cache: it opens the lane's own
    /// `stack/` directory by descriptor, checks the base against the identity
    /// the lease recorded at provision (one directory open, then one `fstat` per file) and
    /// binds the SQLite connection to that descriptor.
    pub fn open(worktree_root: &Path) -> Result<Self> {
        let wt_canon = worktree_root
            .canonicalize()
            .with_context(|| format!("cannot canonicalize {}", worktree_root.display()))?;
        let wt_root_str = wt_canon.to_string_lossy().into_owned();
        let LaneBase {
            lease,
            lease_path,
            pinned,
            base_file,
            base,
        } = open_lane_base(&wt_canon, Some(wt_root_str.clone()))?;
        let hint = restack_hint(&lease_path);

        let csr_file = match &lease.csr {
            Some(record) => {
                let file = open_sealed_child(
                    &pinned,
                    BASE_CSR,
                    "CSR sidecar",
                    Some((&lease_path, record)),
                )?;
                Some(file)
            }
            None => {
                require_child_absent(&pinned, BASE_CSR, "the lane's base directory")
                    .with_context(|| hint.clone())?;
                None
            }
        };

        let delta = DeltaStore::open_read_only(stack_delta_path(&wt_canon))?;
        let overlay =
            build_overlay_from_delta(&wt_root_str, lease.generation_id, &base, delta.as_ref())?;

        Ok(Self {
            worktree_root: wt_canon,
            lease,
            base_file,
            csr_file,
            base,
            delta,
            delta_writable: false,
            overlay,
            merged_edges: Mutex::new(None),
        })
    }

    /// `None` when the lane has no lease. A read path: it deletes nothing, so a
    /// store beside a lease (an interrupted materialize) is refused with the
    /// recovery command.
    pub fn open_if_leased(worktree_root: &Path) -> Result<Option<Self>> {
        if !stack_lease_path(worktree_root).is_file() {
            refuse_legacy_base_store_ref(worktree_root)?;
            return Ok(None);
        }
        if store_path(worktree_root).is_file() {
            bail!(
                "{} has its own store beside a stale stack lease (an interrupted materialize); \
                 run `devmap seed --sync-delta` there to retire the lease",
                worktree_root.display()
            );
        }
        Self::open(worktree_root).map(Some)
    }

    /// [`open_if_leased`](Self::open_if_leased) for `seed --sync-delta`, a write
    /// command: retires a stale lease (a store beside it) and says so, and rolls
    /// back a hot delta journal with a writable open before the read-only one.
    pub fn open_if_leased_for_sync(worktree_root: &Path) -> Result<SyncTarget> {
        if stack_lease_path(worktree_root).is_file() {
            if store_path(worktree_root).is_file() && retire_stale_lease(worktree_root)? {
                return Ok(SyncTarget::RetiredStaleLease);
            }
            DeltaStore::open_existing(stack_delta_path(worktree_root))?;
        }
        Ok(match Self::open_if_leased(worktree_root)? {
            Some(lane) => SyncTarget::Lane(Box::new(lane)),
            None => SyncTarget::NoLease,
        })
    }

    pub fn worktree_root(&self) -> &Path {
        &self.worktree_root
    }

    pub fn lease(&self) -> &StackLease {
        &self.lease
    }

    pub fn base_store(&self) -> &Store {
        &self.base
    }

    pub fn delta_store(&self) -> Option<&DeltaStore> {
        self.delta.as_ref()
    }

    /// Whether `delta.sqlite` exists for this lane. Absent reads as an empty
    /// delta; only a sync or a build creates it.
    pub fn delta_present(&self) -> bool {
        self.delta.is_some()
    }

    /// The writable delta, created on first use. Only write paths call this.
    fn writable_delta(&mut self) -> Result<&DeltaStore> {
        if !self.delta_writable || self.delta.is_none() {
            self.delta = Some(DeltaStore::open(stack_delta_path(&self.worktree_root))?);
            self.delta_writable = true;
        }
        self.delta
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("delta store unavailable after open"))
    }

    pub fn overlay(&self) -> &WorktreeOverlay {
        &self.overlay
    }

    /// Whether the delta holds any edit or deletion. With none, every engine
    /// surface is the base's own answer.
    pub fn has_delta_changes(&self) -> bool {
        !self.overlay.modified_files.is_empty() || !self.overlay.tombstoned_files.is_empty()
    }

    /// The freshness stamp of an answer merged over the delta. It is never
    /// `verified`: the base is a generation behind the tree and the delta covers
    /// only the files that were synced.
    pub fn delta_source_freshness(&self) -> crate::model::SourceFreshness {
        crate::model::SourceFreshness::unverified(format!(
            "answered over immutable base generation {} merged with {} uncommitted stacked delta file(s) ({} deleted)",
            self.lease.generation_id,
            self.overlay.modified_files.len(),
            self.overlay.tombstoned_files.len()
        ))
    }

    /// Why `dead` over this lane may be missing symbols, or `None` when the
    /// delta changes nothing (the answer is then the base's own).
    ///
    /// `dead_page` filters the base's dead rows against the merged edges, so it
    /// can only ever drop rows the delta resurrected. It cannot add the two
    /// kinds the delta creates: a base symbol whose only caller the delta edited
    /// away or deleted, and a symbol the delta adds with no caller. Listing
    /// them would take the base's exemption rules (entry points, tests,
    /// visibility) re-applied over the merged graph, so the answer says it is a
    /// lower bound instead, naming the delta files that can cause it.
    pub fn dead_lower_bound_reason(&self) -> Option<String> {
        let delta = self.delta_files_summary()?;
        Some(format!(
            "dead symbols over this lane are a lower bound: the stacked delta {delta}, so a base symbol whose only caller the delta removed, and a symbol the delta adds with no caller, may be missing from the list; run `devmap build` for a complete answer"
        ))
    }

    /// Why the base's abandoned-cycle scan is not reported over this lane, or
    /// `None` when the delta changes nothing. The scan is a whole-graph pass
    /// over the base; a delta that deletes a cycle's file, or adds a caller
    /// into one, makes its clusters false, and the engine does not re-run it
    /// over the merged edges, so it is dropped rather than passed through.
    pub fn dead_clusters_dropped_reason(&self) -> Option<String> {
        let delta = self.delta_files_summary()?;
        Some(format!(
            "abandoned-cycle clusters are not reported over this lane: the base's cluster scan cannot see the stacked delta, which {delta}; run `devmap build` for the cluster list"
        ))
    }

    /// "edits or adds N file(s) (..) and deletes M (..)" for the delta, or
    /// `None` when it changes nothing.
    fn delta_files_summary(&self) -> Option<String> {
        if !self.has_delta_changes() {
            return None;
        }
        let mut edited: Vec<String> = self
            .overlay
            .modified_files
            .keys()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let mut deleted: Vec<String> = self
            .overlay
            .tombstoned_files
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        edited.sort();
        deleted.sort();
        let name = |files: &[String]| {
            let shown: Vec<&str> = files.iter().take(5).map(String::as_str).collect();
            format!(
                "{}{}",
                shown.join(", "),
                if files.len() > shown.len() {
                    ", ..."
                } else {
                    ""
                }
            )
        };
        Some(format!(
            "edits or adds {} file(s) ({}) and deletes {} ({})",
            edited.len(),
            name(&edited),
            deleted.len(),
            name(&deleted)
        ))
    }

    /// What a lane answer may be missing, or `None` when it is complete.
    ///
    /// A lane answers from an immutable base plus a delta that a detached hook
    /// syncs, so every answer except `status` (which reports the delta itself)
    /// can be missing the lane's own edits. This is the one place that says so,
    /// for the MCP server and the CLI alike: the delta's size, a delta that does
    /// not exist yet, and the edited files it has not ingested.
    pub fn incompleteness_reason(&self) -> Result<Option<String>> {
        let st = self.status()?;
        let mut reasons = Vec::new();
        if st.dirty_files > 0 || st.tombstoned_files > 0 {
            reasons.push(format!(
                "stacked worktree delta has {} dirty and {} deleted file(s) ({} delta edges): this answer comes from the immutable base plus whatever the delta covers; run `devmap build --manifest` for full cross-file resolution",
                st.dirty_files, st.tombstoned_files, st.delta_edges
            ));
        }
        if !self.delta_present() {
            reasons.push(
                "this lane has no delta.sqlite yet (no sync has run): its delta is read as empty"
                    .to_string(),
            );
        }
        match self.unsynced_paths() {
            Ok(paths) if !paths.is_empty() => {
                let shown: Vec<&str> = paths.iter().take(5).map(String::as_str).collect();
                reasons.push(format!(
                    "{} edited file(s) are not in the stacked delta yet ({}{}); run `devmap seed --sync-delta` or `devmap build --manifest`",
                    paths.len(),
                    shown.join(", "),
                    if paths.len() > shown.len() { ", ..." } else { "" }
                ));
            }
            Ok(_) => {}
            Err(err) => reasons.push(format!(
                "cannot tell whether the stacked delta is current ({err}); run `devmap build --manifest`"
            )),
        }
        Ok((!reasons.is_empty()).then(|| reasons.join("; ")))
    }

    /// Put [`Self::incompleteness_reason`] on a lane answer's JSON, appended to
    /// any `walk_incomplete` the producer already gave, and stamp the
    /// delta's freshness on an answer type that carries none of its own
    /// (`literals`, `doc_drift`, `savings` do not go through the engine's
    /// freshness exit). A value that is not an object is left alone.
    pub fn attach_incompleteness(&self, value: &mut serde_json::Value) -> Result<()> {
        let reason = self.incompleteness_reason()?;
        let Some(object) = value.as_object_mut() else {
            return Ok(());
        };
        if let Some(reason) = reason {
            let existing = object
                .get("walk_incomplete")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            if let Some(combined) = devmap_analyze::combine_reasons(existing, Some(reason)) {
                object.insert(
                    "walk_incomplete".into(),
                    serde_json::Value::String(combined),
                );
            }
        }
        if self.has_delta_changes() && !object.contains_key("source_freshness") {
            object.insert(
                "source_freshness".into(),
                serde_json::to_value(self.delta_source_freshness())?,
            );
        }
        Ok(())
    }

    /// Record a parsed `Extraction` for `rel_path` in the worktree's private
    /// `delta.sqlite` and refresh the `WorktreeOverlay` (`F4`).
    ///
    /// Never invokes regex fallback and never writes to any shared/anchor CAS.
    pub fn record_extracted_file(
        &mut self,
        rel_path: &str,
        source_bytes: &[u8],
        extraction: &Extraction,
    ) -> Result<()> {
        let _delta_lock = lock_delta_writer_at(&self.worktree_root, Store::WRITER_LOCK_WAIT)?;
        self.record_extracted_file_locked(rel_path, source_bytes, extraction)
    }

    /// [`Self::record_extracted_file`] for a caller that holds the delta writer lock.
    fn record_extracted_file_locked(
        &mut self,
        rel_path: &str,
        source_bytes: &[u8],
        extraction: &Extraction,
    ) -> Result<()> {
        let normalized = normalize_rel_path(rel_path)?;
        let cas_hash = blake3_hash(source_bytes);
        let edges = extract_delta_edges(&normalized, extraction);
        self.writable_delta()?.upsert_extracted_file(
            &normalized,
            &cas_hash,
            extraction,
            &edges,
            now_unix_ms(),
        )?;
        self.reload_overlay()
    }

    /// Mark `rel_path` as deleted in the worktree's private `delta.sqlite` and
    /// mask all of its base symbols in `WorktreeOverlay` (`F4`).
    pub fn record_deleted_file(&mut self, rel_path: &str) -> Result<()> {
        let _delta_lock = lock_delta_writer_at(&self.worktree_root, Store::WRITER_LOCK_WAIT)?;
        self.record_deleted_file_locked(rel_path)
    }

    /// [`Self::record_deleted_file`] for a caller that holds the delta writer lock.
    fn record_deleted_file_locked(&mut self, rel_path: &str) -> Result<()> {
        let normalized = normalize_rel_path(rel_path)?;
        self.writable_delta()?
            .tombstone_file(&normalized, CAS_TOMBSTONE, now_unix_ms())?;
        self.reload_overlay()
    }

    /// Clear `rel_path` from `delta.sqlite` when it matches the base snapshot again.
    pub fn clear_reverted_file(&mut self, rel_path: &str) -> Result<()> {
        let _delta_lock = lock_delta_writer_at(&self.worktree_root, Store::WRITER_LOCK_WAIT)?;
        self.clear_reverted_file_locked(rel_path)
    }

    /// [`Self::clear_reverted_file`] for a caller that holds the delta writer lock.
    fn clear_reverted_file_locked(&mut self, rel_path: &str) -> Result<()> {
        let normalized = normalize_rel_path(rel_path)?;
        self.writable_delta()?.remove_clean_file(&normalized)?;
        self.reload_overlay()
    }

    /// Re-extract `rel_path` from `<worktree_root>/<rel_path>` using the real
    /// Tree-sitter frontend (`devmap_extract::extract_file`) — never regex!
    #[cfg(feature = "parse")]
    pub fn sync_file_from_disk(&mut self, rel_path: &str) -> Result<()> {
        let _delta_lock = lock_delta_writer_at(&self.worktree_root, Store::WRITER_LOCK_WAIT)?;
        self.sync_file_from_disk_locked(rel_path)
    }

    /// [`Self::sync_file_from_disk`] for a caller that holds the delta writer lock.
    #[cfg(feature = "parse")]
    fn sync_file_from_disk_locked(&mut self, rel_path: &str) -> Result<()> {
        let normalized = normalize_rel_path(rel_path)?;
        if devmap_extract::languages::is_ignored_path(&normalized) {
            return Ok(());
        }
        let full = self.worktree_root.join(&normalized);
        match fs::symlink_metadata(&full) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                self.record_deleted_file_locked(&normalized)
            }
            Err(err) => Err(err).with_context(|| format!("cannot stat {}", full.display())),
            Ok(meta) if !meta.is_file() => self.record_deleted_file_locked(&normalized),
            Ok(_) => {
                if !devmap_extract::languages::is_indexable_source(&normalized) {
                    return Ok(());
                }
                let bytes =
                    fs::read(&full).with_context(|| format!("cannot read {}", full.display()))?;
                let Ok(source_str) = std::str::from_utf8(&bytes) else {
                    return Ok(());
                };
                if let Some(base_file) = self.base.latest_file(&normalized)? {
                    let lang = devmap_extract::languages::detect_language(Path::new(&normalized));
                    let key = devmap_extract::cache::CacheKey::for_source(lang, source_str);
                    if key.content_hash == base_file.content_hash {
                        return self.clear_reverted_file_locked(&normalized);
                    }
                }
                let extraction = devmap_extract::extract_file(&normalized, source_str);
                self.record_extracted_file_locked(&normalized, &bytes, &extraction)
            }
        }
    }

    /// Synchronize uncommitted/diverged git paths in `worktree_root` into `delta.sqlite`
    /// with real Tree-sitter extraction, auto-rebasing to a standalone CoW
    /// `devmap.sqlite` when thresholds (`250` dirty files / `16 MiB`) are exceeded.
    #[cfg(feature = "parse")]
    pub fn sync_worktree(&mut self) -> Result<StackedSyncReport> {
        // The delta writer lock covers the whole sync: the snapshot of what the
        // delta holds, every row written, and the auto-rebase below (which
        // takes the store lock under it: delta, then store). Not taken by any
        // read path.
        let _delta_lock = lock_delta_writer_at(&self.worktree_root, Store::WRITER_LOCK_WAIT)?;
        let changed_paths = git_changed_paths_against_head(&self.worktree_root, &self.base)?;
        let existing_delta: BTreeSet<String> = match &self.delta {
            Some(delta) => delta
                .load_snapshot()?
                .files
                .into_iter()
                .map(|f| f.path)
                .collect(),
            None => BTreeSet::new(),
        };

        let mut synced_files = 0usize;
        let mut deleted_files = 0usize;
        let mut cleaned_files = 0usize;

        for rel_path in &changed_paths {
            if devmap_extract::languages::is_ignored_path(rel_path) {
                continue;
            }
            let full = self.worktree_root.join(rel_path);
            if full.is_file() {
                if !devmap_extract::languages::is_indexable_source(rel_path) {
                    continue;
                }
                self.sync_file_from_disk_locked(rel_path)?;
                synced_files += 1;
            } else if self.base.latest_path_is_indexed(rel_path)?
                || existing_delta.contains(rel_path)
            {
                self.record_deleted_file_locked(rel_path)?;
                deleted_files += 1;
            }
        }

        // A delta row for a path git no longer reports as changed was reverted
        // or removed after it was synced; it equals the base again.
        for prev in existing_delta {
            if !changed_paths.contains(&prev) {
                self.clear_reverted_file_locked(&prev)?;
                cleaned_files += 1;
            }
        }

        let rebased_to_standalone = if self.needs_rebase()? {
            Some(self.materialize_seeded_store()?)
        } else {
            None
        };

        Ok(StackedSyncReport {
            synced_files,
            deleted_files,
            cleaned_files,
            rebased_to_standalone,
        })
    }

    /// Indexable paths git says differ from the base that the delta does not
    /// yet cover. The delta is synced by a detached hook, so it lags by design;
    /// a reader that answers from the base needs to know by how much. Every git
    /// failure is an `Err`: "could not ask" is not "nothing is pending".
    pub fn unsynced_paths(&self) -> Result<BTreeSet<String>> {
        self.unsynced_paths_within(GIT_BOUNDS.deadline)
    }

    /// [`Self::unsynced_paths`] with each of its two git calls bounded by
    /// `deadline` (never more than the 10 s default), for callers with a budget.
    pub fn unsynced_paths_within(&self, deadline: std::time::Duration) -> Result<BTreeSet<String>> {
        let changed = git_changed_paths_within(&self.worktree_root, &self.base, deadline)?;
        let mut pending: BTreeSet<String> = changed
            .iter()
            .filter(|p| {
                !devmap_extract::languages::is_ignored_path(p)
                    && !is_tool_state_path(p)
                    && devmap_extract::languages::is_indexable_source(p)
                    && !self.delta_covers(p)
            })
            .cloned()
            .collect();
        // A delta row for a path git no longer reports as changed was reverted
        // or removed after its sync: the delta still serves the old content.
        let rows = self
            .overlay
            .modified_files
            .keys()
            .chain(self.overlay.tombstoned_files.iter());
        for path in rows {
            if let Some(p) = path.to_str().filter(|p| !changed.contains(*p)) {
                pending.insert(p.to_string());
            }
        }
        Ok(pending)
    }

    /// Whether the delta row for `rel_path` is the file as it is now: a
    /// tombstone for a path that is gone, or the hash of the bytes on disk.
    /// Identity by content, not by path: a second edit of a synced path differs.
    fn delta_covers(&self, rel_path: &str) -> bool {
        let path = Path::new(rel_path);
        let full = self.worktree_root.join(rel_path);
        if self.overlay.is_file_tombstoned(path) {
            return !full.is_file();
        }
        match self.overlay.modified_files.get(path) {
            Some(recorded) => fs::read(&full)
                .map(|bytes| blake3_hash(&bytes) == *recorded)
                .unwrap_or(false),
            None => false,
        }
    }

    pub fn needs_rebase(&self) -> Result<bool> {
        let dirty = match &self.delta {
            Some(delta) => delta.dirty_file_count()?,
            None => 0,
        };
        let bytes = self.delta.as_ref().map_or(0, DeltaStore::bytes_on_disk);
        Ok(dirty > self.lease.rebases_after_dirty_files
            || bytes > self.lease.rebases_after_delta_bytes)
    }

    /// Promote this stacked worktree into a standalone writable CoW `devmap.sqlite`
    /// and retire `stack_lease.json`, the lane's private base and `delta.sqlite` (`OQ2`).
    pub fn materialize_seeded_store(&self) -> Result<PathBuf> {
        self.materialize_seeded_store_waiting(Store::WRITER_LOCK_WAIT)
    }

    /// [`Self::materialize_seeded_store`] with the writer-lock wait named, so a
    /// test can watch the refusal without waiting out a minute.
    ///
    /// Clones the base this store admitted (the held descriptor, a clone of a
    /// clone), places `devmap.sqlite` by atomic rename and only then retires the
    /// lease: a store beside a lease means the lease is stale. A failure before
    /// the store is placed leaves the lane exactly as the call found it.
    pub fn materialize_seeded_store_waiting(&self, wait: std::time::Duration) -> Result<PathBuf> {
        let target_db = store_path(&self.worktree_root);
        if let Some(parent) = target_db.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        // Another writer (a build, a drain) may be placing this store: hold the
        // same lock they hold for the whole promotion.
        let _writer = lock_store_writer_at(&target_db, wait)?;
        if fs::symlink_metadata(&target_db).is_ok() {
            bail!(
                "refusing to materialize over an existing store {}; its stack lease is stale \
                 and `devmap seed --sync-delta` retires it",
                target_db.display()
            );
        }
        let lease_path = stack_lease_path(&self.worktree_root);
        check_identity(
            &self.base_file,
            &self.lease.base,
            "base snapshot",
            &lease_path,
        )?;
        if let (Some(file), Some(record)) = (&self.csr_file, &self.lease.csr) {
            check_identity(file, record, "CSR sidecar", &lease_path)?;
            verify_csr_content(file, record)?;
        }
        let dst_csr = csr_path_for_store(&target_db);
        let placed = (|| -> Result<()> {
            promote_immutable_base_to_store(&self.base_file, &target_db, &self.worktree_root)?;
            if let Some(csr_file) = &self.csr_file {
                clone_or_copy_file(csr_file, &dst_csr)?;
                set_file_writable(&dst_csr)?;
            }
            Ok(())
        })();
        if let Err(err) = placed {
            let _ = set_file_writable(&dst_csr);
            let _ = fs::remove_file(&dst_csr);
            discard_staged(&target_db);
            return Err(err.context(format!(
                "materialize failed and changed nothing; {}",
                restack_hint(&lease_path)
            )));
        }
        if let Err(err) = retire_stack(&self.worktree_root) {
            // The lane is consistent: a store beside a stale lease, which the
            // next `open_if_leased_for_sync` retires.
            eprintln!(
                "devmap: materialized {} but could not retire the stack lease: {err:#}",
                target_db.display()
            );
        }
        Ok(target_db)
    }
    pub fn symbols_for_file(&self, rel_path: &str) -> Result<Vec<StoredSymbol>> {
        let normalized = normalize_rel_path(rel_path)?;
        let path_ref = Path::new(&normalized);
        if self.overlay.is_file_tombstoned(path_ref) {
            return Ok(Vec::new());
        }
        if let Some(ext) = self.overlay.added_extractions.get(&normalized) {
            return Ok(ext
                .symbols
                .iter()
                .map(|s| StoredSymbol {
                    name: s.name.clone(),
                    qualified_name: s.qualified_name.clone(),
                    kind: s.kind.as_str().to_string(),
                    path: normalized.clone(),
                    span_start: s.span.start_byte,
                    span_end: s.span.end_byte,
                    is_exported: s.is_exported,
                    content_hash: ext.content_hash,
                })
                .collect());
        }
        let Some(ext) = self.base.latest_extraction_for_path(&normalized)? else {
            return Ok(Vec::new());
        };
        Ok(ext
            .symbols
            .into_iter()
            .filter(|s| !self.overlay.is_symbol_tombstoned(&s.qualified_name))
            .map(|s| StoredSymbol {
                name: s.name,
                qualified_name: s.qualified_name,
                kind: s.kind.as_str().to_string(),
                path: normalized.clone(),
                span_start: s.span.start_byte,
                span_end: s.span.end_byte,
                is_exported: s.is_exported,
                content_hash: ext.content_hash,
            })
            .collect())
    }

    pub fn search_symbols(&self, query: &str, limit: usize) -> Result<Vec<StoredSymbol>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let needle = query.trim().to_ascii_lowercase();
        let mut results = Vec::new();
        let mut seen = BTreeSet::new();

        let mut modified_paths: Vec<&String> = self.overlay.added_extractions.keys().collect();
        modified_paths.sort();
        for path in modified_paths {
            if let Some(ext) = self.overlay.added_extractions.get(path) {
                for sym in &ext.symbols {
                    if (needle.is_empty()
                        || sym.name.to_ascii_lowercase().contains(&needle)
                        || sym.qualified_name.to_ascii_lowercase().contains(&needle))
                        && seen.insert((path.clone(), sym.qualified_name.clone()))
                    {
                        results.push(StoredSymbol {
                            name: sym.name.clone(),
                            qualified_name: sym.qualified_name.clone(),
                            kind: sym.kind.as_str().to_string(),
                            path: path.clone(),
                            span_start: sym.span.start_byte,
                            span_end: sym.span.end_byte,
                            is_exported: sym.is_exported,
                            content_hash: ext.content_hash,
                        });
                        if results.len() >= limit {
                            return Ok(results);
                        }
                    }
                }
            }
        }

        let base_rows = if needle.is_empty() {
            self.base
                .all_symbols_page()?
                .map(|p| p.rows)
                .unwrap_or_default()
        } else {
            self.base.search_symbols(
                query,
                limit
                    .saturating_add(self.overlay.modified_files.len() * 8)
                    .max(64),
            )?
        };
        for sym in base_rows {
            let p = Path::new(&sym.path);
            if self.overlay.is_file_tombstoned(p)
                || self.overlay.modified_files.contains_key(p)
                || self.overlay.is_symbol_tombstoned(&sym.qualified_name)
            {
                continue;
            }
            if seen.insert((sym.path.clone(), sym.qualified_name.clone())) {
                results.push(sym);
                if results.len() >= limit {
                    break;
                }
            }
        }
        Ok(results)
    }

    pub fn edges_for_symbol(&self, symbol: &str) -> Result<Vec<OverlayEdge>> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for e in self
            .overlay
            .delta_edges
            .iter()
            .filter(|e| e.source == symbol || e.target == symbol)
        {
            if seen.insert((e.source.clone(), e.target.clone(), e.kind.clone())) {
                out.push(e.clone());
            }
        }

        let mut base_edges = self.base.callers_of(&[symbol.to_string()], "", 0.0)?;
        let mut defining_files = BTreeSet::new();
        for sym in self.base.search_symbols(symbol, 64)? {
            if sym.name == symbol || sym.qualified_name == symbol {
                defining_files.insert(sym.path);
            }
        }
        for file_path in defining_files {
            for edge in self.base.latest_edges_for_file(&file_path, 0.0)? {
                if edge.source_symbol == symbol || edge.target_symbol == symbol {
                    base_edges.push(edge);
                }
            }
        }

        for edge in base_edges {
            let src_path = Path::new(&edge.source_file);
            let dst_path = Path::new(&edge.target_file);
            if self.overlay.is_file_tombstoned(src_path)
                || self.overlay.modified_files.contains_key(src_path)
                || self.overlay.is_file_tombstoned(dst_path)
                || self.overlay.is_symbol_tombstoned(&edge.source_symbol)
                || self.overlay.is_symbol_tombstoned(&edge.target_symbol)
            {
                continue;
            }
            if seen.insert((
                edge.source_symbol.clone(),
                edge.target_symbol.clone(),
                edge.edge_kind.clone(),
            )) {
                out.push(OverlayEdge {
                    source: edge.source_symbol,
                    target: edge.target_symbol,
                    kind: edge.edge_kind,
                    confidence: (edge.confidence * 100.0).round().clamp(0.0, 100.0) as u8,
                });
            }
        }
        Ok(out)
    }

    pub fn latest_file(&self, rel_path: &str) -> Result<Option<StoredFile>> {
        let Ok(normalized) = normalize_rel_path(rel_path) else {
            return Ok(None);
        };
        let p = Path::new(&normalized);
        if self.overlay.is_file_tombstoned(p) {
            return Ok(None);
        }
        if let Some(ext) = self.overlay.added_extractions.get(&normalized) {
            return Ok(Some(StoredFile {
                path: normalized,
                language: ext.language.clone(),
                content_hash: ext.content_hash,
                parse_outcome: ext.parse_outcome.clone(),
                engine: ext.engine.clone(),
            }));
        }
        Ok(self.base.latest_file(&normalized)?)
    }

    pub fn latest_extraction_for_path(&self, rel_path: &str) -> Result<Option<Extraction>> {
        let Ok(normalized) = normalize_rel_path(rel_path) else {
            return Ok(None);
        };
        let p = Path::new(&normalized);
        if self.overlay.is_file_tombstoned(p) {
            return Ok(None);
        }
        if let Some(ext) = self.overlay.added_extractions.get(&normalized) {
            return Ok(Some(ext.clone()));
        }
        Ok(self.base.latest_extraction_for_path(&normalized)?)
    }

    /// A unified `GenerationEdges` adjacency: the base's edges that the delta did
    /// not mask, plus the delta files' own `Contains`, `Calls` and `Imports` edges,
    /// so every `StoreQueryEngine` traversal (`impact`, `trace`, `neighbors`,
    /// `explore`, `affected_tests`, `dependencies`, `ask`) sees worktree edits and
    /// deletions. The merged index is cached until the overlay is rebuilt.
    ///
    /// The rebuild reads every base edge and does one base lookup per distinct
    /// delta callee, so it consults `cancel` as it goes. A cancelled rebuild
    /// returns [`crate::cancel::QueryCancelled`] and caches nothing: the cache
    /// is written only with a finished merge.
    pub fn generation_edges_with_id(
        &self,
        cancel: &crate::cancel::Cancel,
    ) -> Result<Option<(u32, Arc<GenerationEdges>)>> {
        if !self.has_delta_changes() {
            return Ok(self.base.generation_edges_with_id()?);
        }
        let mut guard = self
            .merged_edges
            .lock()
            .map_err(|_| anyhow::anyhow!("StackedStore merged_edges mutex poisoned"))?;
        if let Some((generation, cached)) = guard.as_ref() {
            return Ok(Some((*generation, Arc::clone(cached))));
        }
        let Some((generation, base_index)) = self.base.generation_edges_with_id()? else {
            return Ok(None);
        };
        let built = Arc::new(merge_generation_edges(
            &base_index,
            &self.overlay,
            &self.base,
            generation,
            cancel,
        )?);
        *guard = Some((generation, Arc::clone(&built)));
        Ok(Some((generation, built)))
    }

    pub fn file_edges(
        &self,
        path: &str,
        min_confidence: f32,
        cancel: &crate::cancel::Cancel,
    ) -> Result<Option<FileEdges>> {
        if !self.has_delta_changes() {
            return Ok(self.base.file_edges(path, min_confidence)?);
        }
        let min_confidence = devmap_store::checked_min_confidence(min_confidence)?;
        let Some(file) = self.latest_file(path)? else {
            return Ok(None);
        };
        let Some((generation, index)) = self.generation_edges_with_id(cancel)? else {
            return Ok(None);
        };
        let mut edges = Vec::new();
        let mut inbound_edges = 0usize;
        for id in 0..index.len() as u32 {
            if !index.admits(id, min_confidence) {
                continue;
            }
            if index.source_file(id) == file.path {
                edges.push(index.stored_edge(id));
            } else if index.target_file(id) == file.path {
                inbound_edges += 1;
            }
        }
        edges.sort_by(|a, b| {
            b.confidence
                .total_cmp(&a.confidence)
                .then_with(|| a.source_file.cmp(&b.source_file))
                .then_with(|| a.target_file.cmp(&b.target_file))
                .then_with(|| a.source_symbol.cmp(&b.source_symbol))
                .then_with(|| a.target_symbol.cmp(&b.target_symbol))
                .then_with(|| a.edge_kind.cmp(&b.edge_kind))
        });
        Ok(Some(FileEdges {
            generation,
            file,
            edges,
            inbound_edges,
            analysis: index.analysis().cloned(),
        }))
    }

    pub fn all_indexed_files(&self) -> Result<Option<ScopeInputs>> {
        let Some((gen_id, _root, base_files)) = self.base.latest_scope_inputs()? else {
            return Ok(None);
        };
        let mut by_path: std::collections::BTreeMap<String, String> = base_files
            .into_iter()
            .filter(|(p, _)| {
                let path_ref = Path::new(p);
                !self.overlay.is_file_tombstoned(path_ref)
                    && !self.overlay.modified_files.contains_key(path_ref)
            })
            .collect();
        for (path, ext) in &self.overlay.added_extractions {
            by_path.insert(path.clone(), ext.language.clone());
        }
        let wt_root = Some(self.worktree_root.to_string_lossy().into_owned());
        Ok(Some((gen_id, wt_root, by_path.into_iter().collect())))
    }

    pub fn all_merged_symbols(&self) -> Result<Option<SearchPage>> {
        let Some(mut page) = self.base.all_symbols_page()? else {
            return Ok(None);
        };
        page.rows.retain(|sym| {
            let p = Path::new(&sym.path);
            !self.overlay.is_file_tombstoned(p)
                && !self.overlay.modified_files.contains_key(p)
                && !self.overlay.is_symbol_tombstoned(&sym.qualified_name)
        });
        let mut dirty_paths: Vec<&String> = self.overlay.added_extractions.keys().collect();
        dirty_paths.sort();
        for path in dirty_paths {
            if let Some(ext) = self.overlay.added_extractions.get(path) {
                for sym in &ext.symbols {
                    page.rows.push(StoredSymbol {
                        name: sym.name.clone(),
                        qualified_name: sym.qualified_name.clone(),
                        kind: sym.kind.as_str().to_string(),
                        path: path.clone(),
                        span_start: sym.span.start_byte,
                        span_end: sym.span.end_byte,
                        is_exported: sym.is_exported,
                        content_hash: ext.content_hash,
                    });
                }
            }
        }
        page.total = u32::try_from(page.rows.len()).unwrap_or(u32::MAX);
        page.repo_root = Some(self.worktree_root.to_string_lossy().into_owned());
        Ok(Some(page))
    }

    pub fn keyword_page(
        &self,
        query: &str,
        pool: usize,
        filter: Option<&crate::scope::NameQueryFilter>,
    ) -> Result<Option<SearchPage>> {
        let Some(mut page) = self.all_merged_symbols()? else {
            return Ok(None);
        };
        let Some((_, root, all_files)) = self.all_indexed_files()? else {
            return Ok(None);
        };
        let corpus_files = u32::try_from(all_files.len()).unwrap_or(u32::MAX);
        let corpus_symbols = page.total;

        if let Some(filter) = filter {
            let mut scope_paths = Vec::new();
            let mut scope_langs = Vec::new();
            if let Some(scope) = filter.symbol_scope().map_err(anyhow::Error::msg)? {
                let resolved = scope.resolve(&all_files, root.as_deref())?;
                scope_paths = resolved.report.paths.clone();
                scope_langs = resolved.report.languages.clone();
                page.rows.retain(|row| resolved.contains(&row.path));
            }
            let kinds = filter.kinds();
            if !kinds.is_empty() {
                let present: BTreeSet<String> =
                    page.rows.iter().map(|row| row.kind.clone()).collect();
                let missing: Vec<String> = kinds
                    .iter()
                    .filter(|k| !present.contains(k.as_str()))
                    .cloned()
                    .collect();
                if !missing.is_empty() {
                    bail!(crate::scope::missing_kind_message(
                        &missing,
                        &present,
                        !scope_paths.is_empty() || !scope_langs.is_empty()
                    ));
                }
                page.rows.retain(|row| kinds.iter().any(|k| k == &row.kind));
            }
            let scoped_files = page
                .rows
                .iter()
                .map(|r| r.path.as_str())
                .collect::<BTreeSet<_>>()
                .len();
            let scoped_symbols = u32::try_from(page.rows.len()).unwrap_or(u32::MAX);
            page.narrowing = Some(SearchNarrowing {
                paths: scope_paths,
                languages: scope_langs,
                kinds: kinds.to_vec(),
                files: u32::try_from(scoped_files).unwrap_or(u32::MAX),
                symbols: scoped_symbols,
                corpus_files,
                corpus_symbols,
            });
        }

        let trimmed = query.trim().to_ascii_lowercase();
        if !trimmed.is_empty() {
            let terms: Vec<&str> = trimmed
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .filter(|t| !t.is_empty())
                .collect();
            page.rows.retain(|sym| {
                let name_l = sym.name.to_ascii_lowercase();
                let qual_l = sym.qualified_name.to_ascii_lowercase();
                if terms.is_empty() {
                    name_l.contains(&trimmed) || qual_l.contains(&trimmed)
                } else {
                    terms
                        .iter()
                        .all(|term| name_l.contains(term) || qual_l.contains(term))
                }
            });
        }

        page.total = u32::try_from(page.rows.len()).unwrap_or(u32::MAX);
        page.rows.sort_by(|a, b| {
            let exact_a = a.name.eq_ignore_ascii_case(&trimmed);
            let exact_b = b.name.eq_ignore_ascii_case(&trimmed);
            exact_b
                .cmp(&exact_a)
                .then_with(|| a.name.len().cmp(&b.name.len()))
                .then_with(|| a.path.cmp(&b.path))
                .then_with(|| a.qualified_name.cmp(&b.qualified_name))
        });
        if page.rows.len() > pool {
            page.rows.truncate(pool);
        }
        Ok(Some(page))
    }

    pub fn ranking_corpus(
        &self,
        scope: Option<&crate::scope::SymbolScope>,
    ) -> Result<Option<(SearchPage, Option<crate::scope::ResolvedScope>)>> {
        let Some(mut page) = self.all_merged_symbols()? else {
            return Ok(None);
        };
        let Some(scope) = scope else {
            return Ok(Some((page, None)));
        };
        let Some((_, root, files)) = self.all_indexed_files()? else {
            return Ok(None);
        };
        let mut resolved = scope.resolve(&files, root.as_deref())?;
        let corpus_symbols = page.rows.len();
        page.rows.retain(|row| resolved.contains(&row.path));
        resolved.report.symbols = u32::try_from(page.rows.len()).unwrap_or(u32::MAX);
        resolved.report.corpus_symbols = u32::try_from(corpus_symbols).unwrap_or(u32::MAX);
        page.total = resolved.report.symbols;
        Ok(Some((page, Some(resolved))))
    }

    pub fn latest_test_entry_symbols(&self) -> Result<HashSet<String>> {
        let mut symbols = self.base.latest_test_entry_symbols()?;
        if !self.has_delta_changes() {
            return Ok(symbols);
        }
        symbols.retain(|sym| {
            if self.overlay.is_symbol_tombstoned(sym) {
                return false;
            }
            if let Some((file_part, _)) = sym.split_once("::") {
                let p = Path::new(file_part);
                if self.overlay.is_file_tombstoned(p) || self.overlay.modified_files.contains_key(p)
                {
                    return false;
                }
            }
            true
        });
        for ext in self.overlay.added_extractions.values() {
            for annotation in &ext.wiring {
                if annotation.kind == WiringKind::RuntimeEntryPoint
                    && devmap_extract::wiring::is_test_harness_reason(&annotation.details)
                {
                    symbols.insert(annotation.target_symbol.clone());
                }
            }
        }
        Ok(symbols)
    }

    pub fn dead_page(
        &self,
        limit: usize,
        cancel: &crate::cancel::Cancel,
    ) -> Result<Option<DeadPage>> {
        if !self.has_delta_changes() {
            return Ok(self.base.dead_page(limit)?);
        }
        let fetch = limit.saturating_add(self.overlay.modified_files.len() * 8 + 64);
        let Some(mut page) = self.base.dead_page(fetch)? else {
            return Ok(None);
        };
        let edges_opt = self.generation_edges_with_id(cancel)?.map(|(_, idx)| idx);
        let before_filter = page.rows.len();
        page.rows.retain(|row| {
            let p = Path::new(&row.file_path);
            let qn = node_id_of(&row.file_path, &row.symbol_name);
            if self.overlay.is_file_tombstoned(p)
                || self.overlay.modified_files.contains_key(p)
                || self.overlay.is_symbol_tombstoned(&qn)
                || self.overlay.is_symbol_tombstoned(&row.symbol_name)
            {
                return false;
            }
            if let Some(ref idx) = edges_opt {
                let has_inbound_call = idx
                    .into_target_symbol(&qn)
                    .iter()
                    .chain(idx.into_target_symbol(&row.symbol_name).iter())
                    .any(|id| {
                        idx.kind(*id) != devmap_extract::model::EdgeKind::Contains
                            && idx.source_symbol(*id) != qn
                            && idx.source_symbol(*id) != row.symbol_name
                    });
                if has_inbound_call {
                    return false;
                }
            }
            true
        });
        let removed_count = before_filter.saturating_sub(page.rows.len());
        page.total_non_exempt = page.total_non_exempt.saturating_sub(removed_count);
        if page.rows.len() > limit {
            page.rows.truncate(limit);
        }
        Ok(Some(page))
    }

    pub fn callers_page(
        &self,
        targets: &[String],
        exclude_file: &str,
        min_confidence: f32,
        cancel: &crate::cancel::Cancel,
    ) -> Result<Option<CallersPage>> {
        if !self.has_delta_changes() {
            return Ok(self
                .base
                .callers_page(targets, exclude_file, min_confidence)?);
        }
        let min_confidence = devmap_store::checked_min_confidence(min_confidence)?;
        let Some((generation, index)) = self.generation_edges_with_id(cancel)? else {
            return Ok(None);
        };
        let mut callers = Vec::new();
        let mut total_unfiltered = 0usize;
        let target_set: BTreeSet<&str> = targets.iter().map(String::as_str).collect();
        for target in target_set {
            for id in index.into_target_symbol(target) {
                if index.kind(*id) != devmap_extract::model::EdgeKind::Calls
                    || index.source_file(*id) == exclude_file
                {
                    continue;
                }
                total_unfiltered += 1;
                if index.admits(*id, min_confidence) {
                    callers.push(index.stored_edge(*id));
                }
            }
        }
        callers.sort_by(|left, right| {
            right
                .confidence
                .total_cmp(&left.confidence)
                .then_with(|| left.target_symbol.cmp(&right.target_symbol))
                .then_with(|| left.source_file.cmp(&right.source_file))
                .then_with(|| left.source_symbol.cmp(&right.source_symbol))
        });
        Ok(Some(CallersPage {
            generation,
            callers,
            total_unfiltered,
        }))
    }

    pub fn latest_parse_failures(&self) -> Result<(usize, Vec<String>)> {
        let (base_total, base_paths) = self.base.latest_parse_failures()?;
        if !self.has_delta_changes() {
            return Ok((base_total, base_paths));
        }
        let mut paths: Vec<String> = base_paths
            .into_iter()
            .filter(|p| {
                let pr = Path::new(p);
                !self.overlay.is_file_tombstoned(pr)
                    && !self.overlay.modified_files.contains_key(pr)
            })
            .collect();
        for (path, ext) in &self.overlay.added_extractions {
            if matches!(
                ext.parse_outcome,
                devmap_extract::model::ParseOutcome::Failed { .. }
            ) {
                paths.push(path.clone());
            }
        }
        paths.sort();
        paths.dedup();
        let total = paths.len();
        Ok((total, paths))
    }

    pub fn merged_extractions_cancellable(
        &self,
        cancel: &crate::cancel::Cancel,
    ) -> Result<Vec<Extraction>> {
        let mut exts = self
            .base
            .latest_extractions_until(&mut || cancel.check().is_err())?
            .ok_or_else(|| anyhow::Error::new(crate::cancel::QueryCancelled))?;
        if !self.has_delta_changes() {
            return Ok(exts);
        }
        exts.retain(|ext| {
            let p = Path::new(&ext.file_path);
            !self.overlay.is_file_tombstoned(p) && !self.overlay.modified_files.contains_key(p)
        });
        for ext in self.overlay.added_extractions.values() {
            exts.push(ext.clone());
        }
        Ok(exts)
    }

    pub fn latest_clone_candidates(&self) -> Result<(Vec<devmap_analyze::CloneCandidate>, usize)> {
        let (mut candidates, mut unsigned) = self.base.latest_clone_candidates()?;
        if !self.has_delta_changes() {
            return Ok((candidates, unsigned));
        }
        candidates.retain(|cand| {
            let p = Path::new(&cand.file_path);
            !self.overlay.is_file_tombstoned(p)
                && !self.overlay.modified_files.contains_key(p)
                && !self.overlay.is_symbol_tombstoned(&cand.qualified_name)
        });
        let mut dirty_paths: Vec<&String> = self.overlay.added_extractions.keys().collect();
        dirty_paths.sort();
        for path in dirty_paths {
            if let Some(ext) = self.overlay.added_extractions.get(path) {
                for sym in &ext.symbols {
                    if let Some(sig) = sym.body_signature {
                        candidates.push(devmap_analyze::CloneCandidate {
                            file_path: path.clone(),
                            symbol_name: sym.name.clone(),
                            qualified_name: sym.qualified_name.clone(),
                            span_start: sym.span.start_byte,
                            span_end: sym.span.end_byte,
                            kind: sym.kind,
                            exact: sig.exact,
                            structural: sig.structural,
                            nodes: sig.nodes,
                        });
                    } else {
                        unsigned += 1;
                    }
                }
            }
        }
        Ok((candidates, unsigned))
    }

    pub fn search_literals(
        &self,
        query: &str,
        exact: bool,
        page_cap: usize,
    ) -> Result<Option<devmap_store::LiteralPage>> {
        if !self.has_delta_changes() {
            return Ok(self.base.search_literals(query, exact, page_cap)?);
        }
        let fetch = page_cap.saturating_add(self.overlay.modified_files.len() * 16 + 64);
        let Some(base_page) = self.base.search_literals(query, exact, fetch)? else {
            return Ok(None);
        };
        let mut rows: Vec<devmap_store::StoredLiteral> = base_page
            .rows
            .into_iter()
            .filter(|lit| {
                let p = Path::new(&lit.file_path);
                !self.overlay.is_file_tombstoned(p) && !self.overlay.modified_files.contains_key(p)
            })
            .collect();
        for (path, ext) in &self.overlay.added_extractions {
            for lit in &ext.literals {
                let matched = if exact {
                    lit.value == query
                } else {
                    lit.value.contains(query)
                };
                if matched {
                    rows.push(devmap_store::StoredLiteral {
                        file_path: path.clone(),
                        line: lit.line,
                        span_start: u32::try_from(lit.start_byte).unwrap_or(u32::MAX),
                        value: lit.value.clone(),
                        qualified_name: lit.enclosing_qualified_name.clone(),
                        symbol_name: lit.enclosing_name.clone(),
                    });
                }
            }
        }
        rows.sort_by(|a, b| {
            a.file_path
                .cmp(&b.file_path)
                .then_with(|| a.line.cmp(&b.line))
                .then_with(|| a.span_start.cmp(&b.span_start))
        });
        let total = u32::try_from(rows.len()).unwrap_or(u32::MAX);
        if rows.len() > page_cap.max(1) {
            rows.truncate(page_cap.max(1));
        }
        Ok(Some(devmap_store::LiteralPage { total, rows }))
    }
    pub fn status(&self) -> Result<StackedStatus> {
        let delta_bytes = self.delta.as_ref().map_or(0, DeltaStore::bytes_on_disk);
        let dirty_files = self.overlay.modified_files.len();
        let delta_symbols: usize = self
            .overlay
            .added_extractions
            .values()
            .map(|e| e.symbols.len())
            .sum();
        Ok(StackedStatus {
            worktree_root: self.worktree_root.to_string_lossy().into_owned(),
            base_generation_id: self.lease.generation_id,
            base_dir: self.lease.base_dir().to_string_lossy().into_owned(),
            dirty_files,
            tombstoned_files: self.overlay.tombstoned_files.len(),
            delta_symbols,
            delta_edges: self.overlay.delta_edges.len(),
            delta_bytes,
            needs_rebase: dirty_files > self.lease.rebases_after_dirty_files
                || delta_bytes > self.lease.rebases_after_delta_bytes,
        })
    }

    fn reload_overlay(&mut self) -> Result<()> {
        let wt_str = self.worktree_root.to_string_lossy().into_owned();
        self.overlay = build_overlay_from_delta(
            &wt_str,
            self.lease.generation_id,
            &self.base,
            self.delta.as_ref(),
        )?;
        // The merged edge index was built from the overlay just replaced.
        *self
            .merged_edges
            .get_mut()
            .map_err(|_| anyhow::anyhow!("StackedStore merged_edges mutex poisoned"))? = None;
        Ok(())
    }
}

fn bare_tail(symbol: &str) -> Option<&str> {
    let tail = symbol.rsplit("::").next().unwrap_or(symbol);
    let tail = tail.rsplit('.').next().unwrap_or(tail).trim();
    (!tail.is_empty()).then_some(tail)
}

struct ImportAliasMaps {
    /// `local_name -> (module_specifier, imported_name)` from `ExtractedImport::binding_pairs()`.
    symbol_aliases: BTreeMap<String, (String, String)>,
    /// `module_alias -> module_specifier` when `imported_names` is empty (excluding `.`).
    module_aliases: BTreeMap<String, String>,
}

fn import_alias_maps(ext: &Extraction) -> ImportAliasMaps {
    let mut symbol_aliases = BTreeMap::new();
    let mut module_aliases = BTreeMap::new();
    for imp in &ext.imports {
        let spec = imp.module_specifier.trim();
        for (local, imported) in imp.binding_pairs() {
            let local = local.trim();
            let imported = imported.trim();
            if !local.is_empty() && !imported.is_empty() {
                symbol_aliases.insert(local.to_string(), (spec.to_string(), imported.to_string()));
            }
        }
        if imp.imported_names.is_empty() {
            if let Some(alias) = imp.alias.as_deref().map(str::trim) {
                if !alias.is_empty() && alias != "." && !spec.is_empty() {
                    module_aliases.insert(alias.to_string(), spec.to_string());
                }
            }
        }
    }
    ImportAliasMaps {
        symbol_aliases,
        module_aliases,
    }
}

fn specifier_matches_path(specifier: &str, candidate_path: &str) -> bool {
    let mut s = specifier.trim();
    while let Some(rest) = s
        .strip_prefix("./")
        .or_else(|| s.strip_prefix("../"))
        .or_else(|| s.strip_prefix("crate::"))
        .or_else(|| s.strip_prefix("self::"))
        .or_else(|| s.strip_prefix("super::"))
    {
        s = rest;
    }
    if matches!(s, "" | "crate" | "self" | "super" | ".") {
        return false;
    }
    let cand_no_ext = candidate_path
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(candidate_path);
    let mut norm_variants = Vec::with_capacity(2);
    let slash_spec = s.replace("::", "/").trim_matches('/').to_string();
    if !slash_spec.is_empty() {
        norm_variants.push(slash_spec);
    }
    if s.contains('.') && !s.contains('/') && !s.contains("::") {
        let dot_spec = s.trim_start_matches('.').replace('.', "/");
        if !dot_spec.is_empty() && !norm_variants.contains(&dot_spec) {
            norm_variants.push(dot_spec);
        }
    }
    norm_variants.iter().any(|norm_spec| {
        cand_no_ext == norm_spec
            || cand_no_ext.ends_with(&format!("/{norm_spec}"))
            || cand_no_ext.ends_with(&format!("/{norm_spec}/mod"))
            || cand_no_ext.ends_with(&format!("/{norm_spec}/index"))
            || cand_no_ext.ends_with(&format!("/{norm_spec}/__init__"))
    })
}

/// The most base rows one callee lookup will read. A name matching more than this
/// is too common to resolve by uniqueness.
const CALLEE_LOOKUP_CEILING: usize = 4096;

/// Every base symbol the index matches for `callee`, with whether that is the
/// *whole* match set. `search_page` counts and reads in one snapshot, so
/// `exhaustive` is `rows.len() >= total`: a capped read is never mistaken for a
/// complete one.
fn base_symbols_for_callee(base: &Store, callee: &str) -> Result<(Vec<StoredSymbol>, bool)> {
    base_symbols_for_callee_within(base, callee, CALLEE_LOOKUP_CEILING)
}

fn base_symbols_for_callee_within(
    base: &Store,
    callee: &str,
    ceiling: usize,
) -> Result<(Vec<StoredSymbol>, bool)> {
    let Some(page) = base.search_page(callee, ceiling)? else {
        return Ok((Vec::new(), true));
    };
    let exhaustive = page.rows.len() >= usize::try_from(page.total).unwrap_or(usize::MAX);
    Ok((page.rows, exhaustive))
}

/// The edges one dirty file contributes to the merged index.
///
/// `base_scope_paths` is every path the base indexes, read once per merge by the
/// caller. `lookups` memoises [`base_symbols_for_callee`] across the merge.
fn resolve_delta_stored_edges(
    rel_path: &str,
    ext: &Extraction,
    overlay: &WorktreeOverlay,
    base: &Store,
    base_scope_paths: &[String],
    lookups: &mut BTreeMap<String, (Vec<StoredSymbol>, bool)>,
    cancel: &crate::cancel::Cancel,
) -> Result<Vec<StoredEdge>> {
    let mut out = Vec::new();
    let caller_family = LangFamily::from_lang(devmap_extract::languages::detect_language(
        Path::new(rel_path),
    ));
    let aliases = import_alias_maps(ext);

    for sym in &ext.symbols {
        out.push(StoredEdge {
            source_file: rel_path.to_string(),
            target_file: rel_path.to_string(),
            source_symbol: rel_path.to_string(),
            target_symbol: sym.qualified_name.clone(),
            edge_kind: "Contains".to_string(),
            confidence: 1.0,
            resolution: Some("Structural".to_string()),
        });
    }

    for call in &ext.calls {
        let raw_callee = call.callee_name.trim();
        if raw_callee.is_empty() {
            continue;
        }
        let source_symbol = call
            .caller_symbol
            .as_deref()
            .map(|caller| {
                let want_qual = node_id_of(rel_path, caller);
                ext.symbols
                    .iter()
                    .find(|s| {
                        s.qualified_name == caller
                            || s.name == caller
                            || node_id_of(rel_path, &s.qualified_name) == want_qual
                    })
                    .map(|s| s.qualified_name.clone())
                    .unwrap_or(want_qual)
            })
            .unwrap_or_else(|| rel_path.to_string());
        let raw_recv =
            call.receiver_expr.as_deref().map(str::trim).filter(|r| {
                !r.is_empty() && !matches!(*r, "self" | "this" | "Self" | "super" | "cls")
            });

        // 1. SameFile resolution (deterministic 1.0) checks raw local callee/receiver first.
        let same_file: Vec<&devmap_extract::model::ExtractedSymbol> = ext
            .symbols
            .iter()
            .filter(|s| {
                let tail_ok =
                    s.name == raw_callee || bare_tail(&s.qualified_name) == Some(raw_callee);
                if !tail_ok {
                    return false;
                }
                match raw_recv {
                    Some(r) => {
                        s.qualified_name.contains(&format!("{r}.{raw_callee}"))
                            || s.qualified_name.contains(&format!("{r}::{raw_callee}"))
                    }
                    None => true,
                }
            })
            .collect();
        if same_file.len() == 1 {
            out.push(StoredEdge {
                source_file: rel_path.to_string(),
                target_file: rel_path.to_string(),
                source_symbol,
                target_symbol: same_file[0].qualified_name.clone(),
                edge_kind: "Calls".to_string(),
                confidence: 1.0,
                resolution: Some("SameFile".to_string()),
            });
            continue;
        }
        if !same_file.is_empty() {
            continue;
        }

        // Resolve symbol and module import aliases (`use a::b as c`, `import { b as c }`,
        // `use crate::mod_a as m`, `import * as m`).
        let mut explicit_specs: Vec<String> = Vec::new();
        let (callee, recv): (&str, Option<&str>) = match raw_recv {
            None => {
                if let Some((spec, imported)) = aliases.symbol_aliases.get(raw_callee) {
                    if !spec.is_empty() {
                        explicit_specs.push(spec.clone());
                    }
                    (imported.as_str(), None)
                } else {
                    (raw_callee, None)
                }
            }
            Some(r) => {
                if let Some(mod_spec) = aliases.module_aliases.get(r) {
                    explicit_specs.push(mod_spec.clone());
                    (raw_callee, None)
                } else if let Some((spec, imported)) = aliases.symbol_aliases.get(r) {
                    if !spec.is_empty() {
                        explicit_specs.push(spec.clone());
                        explicit_specs.push(format!("{spec}::{imported}"));
                        explicit_specs.push(format!("{spec}/{imported}"));
                    } else {
                        explicit_specs.push(imported.clone());
                    }
                    (raw_callee, Some(imported.as_str()))
                } else {
                    (raw_callee, Some(r))
                }
            }
        };

        // 2. Cross-file resolution across other delta extractions + non-masked base symbols.
        let mut candidates: BTreeSet<(String, String)> = BTreeSet::new();
        for (other_path, other_ext) in &overlay.added_extractions {
            if other_path == rel_path {
                continue;
            }
            let fam = LangFamily::from_lang(devmap_extract::languages::detect_language(Path::new(
                other_path,
            )));
            if fam != caller_family {
                continue;
            }
            for s in &other_ext.symbols {
                if s.name == callee || bare_tail(&s.qualified_name) == Some(callee) {
                    candidates.insert((other_path.clone(), s.qualified_name.clone()));
                }
            }
        }
        if !lookups.contains_key(callee) {
            cancel.check()?;
            lookups.insert(callee.to_string(), base_symbols_for_callee(base, callee)?);
        }
        let (base_rows, base_exhaustive) = &lookups[callee];
        for sym in base_rows {
            if sym.path == rel_path {
                continue;
            }
            let p = Path::new(&sym.path);
            if overlay.is_file_tombstoned(p)
                || overlay.modified_files.contains_key(p)
                || overlay.is_symbol_tombstoned(&sym.qualified_name)
            {
                continue;
            }
            let fam = LangFamily::from_lang(devmap_extract::languages::detect_language(p));
            if fam != caller_family {
                continue;
            }
            if sym.name == callee || bare_tail(&sym.qualified_name) == Some(callee) {
                candidates.insert((sym.path.clone(), sym.qualified_name.clone()));
            }
        }

        if let Some(r) = recv {
            let narrowed: BTreeSet<(String, String)> = candidates
                .iter()
                .filter(|(cand_file, cand_qual)| {
                    cand_qual.contains(&format!("{r}.{callee}"))
                        || cand_qual.contains(&format!("{r}::{callee}"))
                        || Path::new(cand_file).file_stem().and_then(|s| s.to_str()) == Some(r)
                })
                .cloned()
                .collect();
            if !narrowed.is_empty() {
                candidates = narrowed;
            }
        }

        let imported: Vec<&(String, String)> = candidates
            .iter()
            .filter(|(cand_file, _)| {
                Path::new(cand_file).parent() == Path::new(rel_path).parent()
                    || explicit_specs
                        .iter()
                        .any(|spec| specifier_matches_path(spec, cand_file))
                    || ext
                        .imports
                        .iter()
                        .any(|imp| specifier_matches_path(&imp.module_specifier, cand_file))
            })
            .collect();

        if imported.len() == 1 {
            let (target_file, target_symbol) = imported[0];
            out.push(StoredEdge {
                source_file: rel_path.to_string(),
                target_file: target_file.clone(),
                source_symbol,
                target_symbol: target_symbol.clone(),
                edge_kind: "Calls".to_string(),
                confidence: 0.9,
                resolution: Some("ImportScoped".to_string()),
            });
        } else if candidates.len() == 1 && *base_exhaustive {
            // `UniqueGlobal` claims no other symbol in the corpus carries the
            // name. That holds only when the base lookup saw every match; a read
            // that hit the ceiling proves nothing, so it adds no edge.
            if let Some((target_file, target_symbol)) = candidates.into_iter().next() {
                out.push(StoredEdge {
                    source_file: rel_path.to_string(),
                    target_file,
                    source_symbol,
                    target_symbol,
                    edge_kind: "Calls".to_string(),
                    confidence: 0.9,
                    resolution: Some("UniqueGlobal".to_string()),
                });
            }
        }
    }

    for imp in &ext.imports {
        let spec = imp.module_specifier.trim();
        if spec.is_empty() {
            continue;
        }
        let mut candidate_specs = vec![spec.to_string()];
        for (_, imported) in imp.binding_pairs() {
            let imported = imported.trim();
            if !imported.is_empty() {
                candidate_specs.push(format!("{spec}::{imported}"));
                candidate_specs.push(format!("{spec}/{imported}"));
            }
        }
        for cand_spec in candidate_specs {
            let mut matched_files: BTreeSet<String> = BTreeSet::new();
            for other_path in overlay.added_extractions.keys() {
                if other_path != rel_path && specifier_matches_path(&cand_spec, other_path) {
                    matched_files.insert(other_path.clone());
                }
            }
            if matched_files.is_empty() {
                for bp in base_scope_paths {
                    let p = Path::new(bp);
                    if bp != rel_path
                        && !overlay.is_file_tombstoned(p)
                        && specifier_matches_path(&cand_spec, bp)
                    {
                        matched_files.insert(bp.clone());
                    }
                }
            }
            if matched_files.len() == 1 {
                if let Some(target_file) = matched_files.into_iter().next() {
                    out.push(StoredEdge {
                        source_file: rel_path.to_string(),
                        target_file: target_file.clone(),
                        source_symbol: rel_path.to_string(),
                        target_symbol: target_file,
                        edge_kind: "Imports".to_string(),
                        confidence: 1.0,
                        resolution: Some("ImportScoped".to_string()),
                    });
                    break;
                }
            }
        }
    }

    Ok(out)
}

/// The base edge index with the delta applied: base edges whose file or symbol
/// the delta masked are dropped, and each dirty file's own edges are added.
/// Edges are de-duplicated on `(source file, target file, source symbol, target
/// symbol, kind)`.
///
/// This walks every base edge and materialises the survivors, so its cost grows
/// with the base. `StackedStore::generation_edges_with_id` caches the result.
fn merge_generation_edges(
    base_index: &GenerationEdges,
    overlay: &WorktreeOverlay,
    base: &Store,
    generation: u32,
    cancel: &crate::cancel::Cancel,
) -> Result<GenerationEdges> {
    let mut merged: Vec<StoredEdge> = Vec::with_capacity(base_index.len() + 64);
    let mut seen: HashSet<(String, String, String, String, String)> = HashSet::new();
    let key_of = |edge: &StoredEdge| {
        (
            edge.source_file.clone(),
            edge.target_file.clone(),
            edge.source_symbol.clone(),
            edge.target_symbol.clone(),
            edge.edge_kind.clone(),
        )
    };

    for id in 0..base_index.len() as u32 {
        cancel.check_every(id as usize)?;
        let src_p = Path::new(base_index.source_file(id));
        let dst_p = Path::new(base_index.target_file(id));
        if overlay.is_file_tombstoned(src_p)
            || overlay.modified_files.contains_key(src_p)
            || overlay.is_file_tombstoned(dst_p)
            || overlay.is_symbol_tombstoned(base_index.source_symbol(id))
            || overlay.is_symbol_tombstoned(base_index.target_symbol(id))
        {
            continue;
        }
        let edge = base_index.stored_edge(id);
        seen.insert(key_of(&edge));
        merged.push(edge);
    }

    let base_scope_paths: Vec<String> = base
        .latest_scope_inputs()?
        .map(|(_, _, base_files)| base_files.into_iter().map(|(bp, _)| bp).collect())
        .unwrap_or_default();
    let mut lookups = BTreeMap::new();
    let mut dirty_paths: Vec<&String> = overlay.added_extractions.keys().collect();
    dirty_paths.sort();
    for rel_path in dirty_paths {
        cancel.check()?;
        let Some(ext) = overlay.added_extractions.get(rel_path) else {
            continue;
        };
        for edge in resolve_delta_stored_edges(
            rel_path,
            ext,
            overlay,
            base,
            &base_scope_paths,
            &mut lookups,
            cancel,
        )? {
            if seen.insert(key_of(&edge)) {
                merged.push(edge);
            }
        }
    }

    Ok(
        GenerationEdges::build(Arc::new(merged), base_index.analysis().cloned())?
            .with_generation(generation),
    )
}

fn build_overlay_from_delta(
    worktree_id: &str,
    generation_id: u32,
    base: &Store,
    delta: Option<&DeltaStore>,
) -> Result<WorktreeOverlay> {
    let snap = match delta {
        Some(delta) => delta.load_snapshot()?,
        None => Default::default(),
    };
    let mut overlay = WorktreeOverlay::new(worktree_id.to_string(), u64::from(generation_id));
    overlay.dirty_nodes = RoaringBitmap::new();

    for (idx, file_rec) in snap.files.into_iter().enumerate() {
        let dirty_id = u32::try_from(idx).unwrap_or(u32::MAX);
        let base_symbols: BTreeSet<String> = base
            .latest_extraction_for_path(&file_rec.path)?
            .map(|ext| ext.symbols.into_iter().map(|s| s.qualified_name).collect())
            .unwrap_or_default();

        if file_rec.tombstoned {
            overlay.mark_file_tombstone(&file_rec.path, Some(dirty_id));
            for old_sym in base_symbols {
                overlay.mark_symbol_tombstone(old_sym, Some(dirty_id));
            }
        } else {
            overlay.record_modified_file(&file_rec.path, &file_rec.content_hash, Some(dirty_id));
            if let Some(ext) = file_rec.extraction {
                let current_symbols: BTreeSet<String> = ext
                    .symbols
                    .iter()
                    .map(|s| s.qualified_name.clone())
                    .collect();
                for removed_sym in base_symbols.difference(&current_symbols) {
                    overlay.mark_symbol_tombstone(removed_sym.clone(), Some(dirty_id));
                }
                overlay.record_extraction(file_rec.path.clone(), ext, dirty_id);
            }
        }
    }

    for edge in snap.edges {
        overlay.add_edge(OverlayEdge {
            source: edge.source_symbol,
            target: edge.target_symbol,
            kind: edge.edge_kind,
            confidence: edge.confidence,
        });
    }

    Ok(overlay)
}

fn extract_delta_edges(rel_path: &str, extraction: &Extraction) -> Vec<DeltaEdgeRecord> {
    let mut edges = Vec::new();
    let mut seen = BTreeSet::new();
    for call in &extraction.calls {
        let src = call
            .caller_symbol
            .clone()
            .unwrap_or_else(|| rel_path.to_string());
        let dst = match &call.receiver_expr {
            Some(recv) if !recv.is_empty() => format!("{recv}.{}", call.callee_name),
            _ => call.callee_name.clone(),
        };
        if seen.insert((src.clone(), dst.clone(), "Calls")) {
            edges.push(DeltaEdgeRecord {
                path: rel_path.to_string(),
                source_symbol: src,
                target_symbol: dst,
                edge_kind: "Calls".to_string(),
                confidence: 85,
            });
        }
    }
    for imp in &extraction.imports {
        if !imp.module_specifier.is_empty()
            && seen.insert((
                rel_path.to_string(),
                imp.module_specifier.clone(),
                "Imports",
            ))
        {
            edges.push(DeltaEdgeRecord {
                path: rel_path.to_string(),
                source_symbol: rel_path.to_string(),
                target_symbol: imp.module_specifier.clone(),
                edge_kind: "Imports".to_string(),
                confidence: 95,
            });
        }
    }
    edges
}

fn normalize_rel_path(rel_path: &str) -> Result<String> {
    let trimmed = rel_path.trim().replace('\\', "/");
    let trimmed = trimmed.trim_start_matches("./");
    if trimmed.is_empty() || trimmed.starts_with('/') {
        bail!("overlay path must be non-empty and repository-relative: {rel_path:?}");
    }
    for comp in Path::new(trimmed).components() {
        if matches!(comp, std::path::Component::ParentDir) {
            bail!("overlay path must not traverse '..': {rel_path:?}");
        }
    }
    Ok(trimmed.to_string())
}

/// Paths that differ from the base: `git status` plus `base_head..HEAD`.
/// Every git failure is an `Err`; an empty set only ever means "clean".
fn git_changed_paths_against_head(worktree_root: &Path, base: &Store) -> Result<BTreeSet<String>> {
    git_changed_paths_within(worktree_root, base, GIT_BOUNDS.deadline)
}

fn git_changed_paths_within(
    worktree_root: &Path,
    base: &Store,
    deadline: std::time::Duration,
) -> Result<BTreeSet<String>> {
    let mut paths = git_status_paths_within(worktree_root, deadline)?;
    let base_head = base.latest_generation_head_sha()?.ok_or_else(|| {
        anyhow::anyhow!("base snapshot records no head_sha; re-run `devmap seed --stacked`")
    })?;
    if !is_commit_sha(&base_head) {
        bail!(
            "base snapshot head_sha {base_head:?} is not a commit; re-run `devmap seed --stacked`"
        );
    }
    let diff = run_git_within(
        worktree_root,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            &base_head,
            "HEAD",
        ],
        deadline,
    )?;
    if !diff.is_empty() && diff.last() != Some(&0) {
        bail!(
            "git diff in {} returned output that is not NUL-terminated: {:?}",
            worktree_root.display(),
            String::from_utf8_lossy(&diff[..diff.len().min(80)])
        );
    }
    for raw in diff.split(|&b| b == 0) {
        if !raw.is_empty() {
            paths.insert(status_path(worktree_root, raw)?);
        }
    }
    Ok(paths)
}

/// Longest `open_if_leased_for_sync` waits for the writer lock to retire a stale lease.
/// A build that holds it for minutes must not stall a hook, and the answer
/// does not depend on it.
const STALE_LEASE_LOCK_WAIT: std::time::Duration = std::time::Duration::from_millis(50);

/// Retire a lease that a store has outlived. Under the writer lock, because a
/// materialize that is still placing the store holds it and owns the lease
/// until it finishes; the store is re-checked once the lock is held. `false`
/// when the store is gone by then (the lease is live again, nothing retired).
fn retire_stale_lease(worktree_root: &Path) -> Result<bool> {
    let target_db = store_path(worktree_root);
    // Retirement deletes the delta, and every write to the delta holds this
    // lock. The order is delta, then store, then bases.
    let _delta = lock_delta_writer_at(worktree_root, STALE_LEASE_LOCK_WAIT).with_context(|| {
        format!(
            "{} has a store and a stale stack lease, but the delta writer lock on {} is held; \
             wait for that sync to finish and run `devmap seed --sync-delta` again",
            worktree_root.display(),
            stack_delta_path(worktree_root).display()
        )
    })?;
    let _writer = lock_store_writer_at(&target_db, STALE_LEASE_LOCK_WAIT).with_context(|| {
        format!(
            "{} has a store and a stale stack lease, but the writer lock on {} is held; \
             wait for that process to finish and run `devmap seed --sync-delta` again",
            worktree_root.display(),
            target_db.display()
        )
    })?;
    if !target_db.is_file() {
        return Ok(false);
    }
    retire_stack(worktree_root).with_context(|| {
        format!(
            "{} has a store and a stale stack lease that could not be retired",
            worktree_root.display()
        )
    })?;
    Ok(true)
}

/// Remove a lane's lease, private base and delta. The lease goes first, so an
/// interruption never leaves a lease naming a base that is gone.
fn retire_stack(worktree_root: &Path) -> Result<()> {
    remove_lease_and_base(&stack_lease_path(worktree_root), &stack_dir(worktree_root))?;
    discard_staged(&stack_delta_path(worktree_root));
    Ok(())
}

/// Remove a lane's lease, then its private base; already gone is fine. The lease
/// goes first, so an interruption never leaves a lease naming a base that is gone.
fn remove_lease_and_base(lease_path: &Path, stack: &Path) -> Result<()> {
    let tolerate = |removed: std::io::Result<()>, path: &Path| match removed {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            Err(err).with_context(|| format!("cannot remove {}", path.display()))
        }
        _ => Ok(()),
    };
    tolerate(fs::remove_file(lease_path), lease_path)?;
    tolerate(fs::remove_dir_all(stack), stack)
}

/// Streaming sha256 of an open file, 64 KiB at a time, from offset 0.
fn sha256_of_open_file(file: &File) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut offset = 0u64;
    loop {
        let n = read_file_at(file, &mut buf, offset).context("cannot read a file for sha256")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        offset += n as u64;
    }
    Ok(crate::digest::hex(&hasher.finalize()))
}

/// The CSR sidecar's content against what the lease admitted, read through the
/// descriptor the store holds.
fn verify_csr_content(file: &File, record: &FileRecord) -> Result<()> {
    let actual = sha256_of_open_file(file)?;
    if actual != record.sha256 {
        bail!(
            "the lane's CSR sidecar fails its sha256 check: the lease admitted {} but it hashes to {actual}",
            record.sha256
        );
    }
    Ok(())
}

/// Streaming sha256 of a file, 64 KiB at a time.
pub fn sha256_file_hex(path: &Path) -> Result<String> {
    let file = fs::File::open(path)
        .with_context(|| format!("cannot open {} for sha256", path.display()))?;
    sha256_of_open_file(&file).with_context(|| format!("while hashing {}", path.display()))
}

fn sha256_str_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    crate::digest::hex(&Sha256::digest(text.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "parse")]
    use std::process::Command;
    #[cfg(feature = "parse")]
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    /// `with_fake_git` replaces `PATH` for the whole process, so every test that
    /// runs real git holds a shared guard (taken in `temp_repo_dir`, kept for the
    /// life of its thread) and a fake-git run takes the exclusive one. Without
    /// this, a fake git leaked into whichever test happened to be running.
    #[cfg(feature = "parse")]
    static GIT_PATH: std::sync::RwLock<()> = std::sync::RwLock::new(());

    #[cfg(feature = "parse")]
    thread_local! {
        static GIT_PATH_SHARED: std::cell::RefCell<Vec<std::sync::RwLockReadGuard<'static, ()>>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    #[cfg(feature = "parse")]
    fn hold_git_path_shared() {
        // Once per thread: a second read while a writer waits would deadlock
        // against the first.
        if GIT_PATH_SHARED.with(|held| !held.borrow().is_empty()) {
            return;
        }
        let guard = GIT_PATH.read().unwrap_or_else(|p| p.into_inner());
        GIT_PATH_SHARED.with(|held| held.borrow_mut().push(guard));
    }

    #[cfg(feature = "parse")]
    fn release_git_path_shared() {
        GIT_PATH_SHARED.with(|held| held.borrow_mut().clear());
    }

    #[cfg(feature = "parse")]
    fn temp_repo_dir(tag: &str) -> PathBuf {
        hold_git_path_shared();
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "devmap-stacked-test-{}-{}-{}",
            tag,
            std::process::id(),
            id
        ));
        let _ = remove_readonly_tree(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(feature = "parse")]
    fn run_git_ok(cwd: &Path, args: &[&str]) {
        let out = Command::new("git")
            .current_dir(cwd)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("git command");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[cfg(feature = "parse")]
    fn seed_indexed_git_repo(anchor: &Path) {
        fs::create_dir_all(anchor.join("src")).unwrap();
        fs::write(
            anchor.join("src/svc.go"),
            "package svc\n\ntype Server struct{}\n\nfunc (s *Server) Handle() int { return 1 }\nfunc Helper() int { return 2 }\n",
        )
        .unwrap();
        fs::write(
            anchor.join("src/routes.ts"),
            "export const API_ROUTE = '/v1/items';\nexport class Router {\n  dispatch(): string { return API_ROUTE; }\n}\n",
        )
        .unwrap();
        fs::write(
            anchor.join("src/limits.rs"),
            "pub const MAX_BATCH: usize = 64;\nmacro_rules! check_cap { ($x:expr) => { $x < MAX_BATCH }; }\npub fn within_cap(v: usize) -> bool { check_cap!(v) }\n",
        )
        .unwrap();

        run_git_ok(anchor, &["init", "-b", "main"]);
        run_git_ok(anchor, &["add", "."]);
        run_git_ok(anchor, &["commit", "-m", "initial"]);

        index_tree(anchor, &["src/svc.go", "src/routes.ts", "src/limits.rs"]);
    }

    #[cfg(feature = "parse")]
    fn git_head(cwd: &Path) -> String {
        let out = Command::new("git")
            .current_dir(cwd)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("git rev-parse");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// Index `rels` (the whole committed tree) as a new generation stamped with
    /// the real `HEAD`, as `devmap build` does. `save_generation` stamps
    /// `"unknown"`, which a stacked base now refuses.
    #[cfg(feature = "parse")]
    fn index_tree(anchor: &Path, rels: &[&str]) {
        let db_path = store_path(anchor);
        fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let store = Store::open(&db_path).unwrap();
        let anchor_canon = anchor.canonicalize().unwrap();
        store.bind_repo_root(&anchor_canon).unwrap();
        let exts: Vec<_> = rels
            .iter()
            .map(|rel| {
                devmap_extract::extract_file(rel, &fs::read_to_string(anchor.join(rel)).unwrap())
            })
            .collect();
        let mut resolver = devmap_resolve::Resolver::new();
        resolver.index_extractions(&exts);
        let res = resolver.resolve_all(&exts).unwrap();
        let analysis = devmap_analyze::analyze(&exts, &res);
        store
            .save_generation_with_metadata(
                &exts,
                &res,
                &analysis,
                devmap_store::GenerationWriteOpts::default(),
                &git_head(anchor),
            )
            .unwrap();
        let _ = store.write_latest_csr_sidecar().unwrap();
    }

    #[cfg(feature = "parse")]
    #[test]
    fn stacked_store_preserves_go_receiver_ts_const_and_rust_const_edits_and_refuses_writable_base()
    {
        let root = temp_repo_dir("ast-fidelity");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);

        let lane = root.join("lane-1");
        run_git_ok(
            &anchor,
            &["worktree", "add", "-b", "lane-1", lane.to_str().unwrap()],
        );

        // Adversarial 1: forged `.git` pointer in a fake checkout MUST be refused!
        let forged = root.join("forged");
        fs::create_dir_all(&forged).unwrap();
        let common = registered_common_git_dir(&anchor).unwrap();
        fs::write(
            forged.join(".git"),
            format!("gitdir: {}\n", common.join("worktrees/lane-1").display()),
        )
        .unwrap();
        assert!(
            provision_stacked_worktree(&anchor, &forged).is_err(),
            "forged .git file must be refused by provision_stacked_worktree"
        );

        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        assert_eq!(lease.generation_id, 1);

        // Edit Go receiver method, TS `export const` + class method, and Rust `pub const` + macro (`F4`)!
        fs::write(
            lane.join("src/svc.go"),
            "package svc\n\ntype Server struct{}\n\nfunc (s *Server) Handle() int { return 99 }\nfunc (s *Server) Shutdown() {}\n",
        )
        .unwrap();
        fs::write(
            lane.join("src/routes.ts"),
            "export const API_ROUTE = '/v2/items';\nexport class Router {\n  dispatch(): string { return API_ROUTE; }\n  health(): boolean { return true; }\n}\n",
        )
        .unwrap();
        fs::write(
            lane.join("src/limits.rs"),
            "pub const MAX_BATCH: usize = 128;\nmacro_rules! check_cap { ($x:expr) => { $x <= MAX_BATCH }; }\npub fn within_cap(v: usize) -> bool { check_cap!(v) }\n",
        )
        .unwrap();

        let mut stacked = StackedStore::open(&lane).unwrap();
        let sync = stacked.sync_worktree().unwrap();
        assert_eq!(sync.synced_files, 3);
        assert!(sync.rebased_to_standalone.is_none());

        // Go receiver methods `Handle` AND `Shutdown` must be live; deleted `Helper` must be masked!
        let go_syms = stacked.symbols_for_file("src/svc.go").unwrap();
        let go_names: Vec<&str> = go_syms.iter().map(|s| s.name.as_str()).collect();
        assert!(
            go_names.contains(&"Handle"),
            "Go receiver method Handle lost: {go_names:?}"
        );
        assert!(
            go_names.contains(&"Shutdown"),
            "Go receiver method Shutdown lost: {go_names:?}"
        );
        assert!(
            !go_names.contains(&"Helper"),
            "Removed Helper should be tombstoned: {go_names:?}"
        );
        assert!(
            stacked.search_symbols("Helper", 10).unwrap().is_empty(),
            "Removed Helper must not leak from immutable base"
        );

        // TS `API_ROUTE` (`export const`) and `dispatch`/`health` class methods must survive!
        let ts_syms = stacked.symbols_for_file("src/routes.ts").unwrap();
        let ts_names: Vec<&str> = ts_syms.iter().map(|s| s.name.as_str()).collect();
        assert!(
            ts_names.contains(&"API_ROUTE"),
            "TS export const lost: {ts_names:?}"
        );
        assert!(
            ts_names.contains(&"dispatch"),
            "TS class method lost: {ts_names:?}"
        );
        assert!(
            ts_names.contains(&"health"),
            "TS new class method lost: {ts_names:?}"
        );

        // Rust `MAX_BATCH` (`pub const`) and `check_cap` (`macro_rules!`) must survive!
        let rs_syms = stacked.symbols_for_file("src/limits.rs").unwrap();
        let rs_names: Vec<&str> = rs_syms.iter().map(|s| s.name.as_str()).collect();
        assert!(
            rs_names.contains(&"MAX_BATCH"),
            "Rust pub const lost: {rs_names:?}"
        );
        assert!(
            rs_names.contains(&"check_cap"),
            "Rust macro_rules! lost: {rs_names:?}"
        );

        drop(stacked);

        // Adversarial 2: flipping `base.sqlite` to `0644` MUST cause `StackedStore::open` to fail closed!
        let base_sqlite = lease.base_sqlite_path();
        set_file_writable(&base_sqlite).unwrap();
        let err = StackedStore::open(&lane)
            .expect_err("writable base.sqlite must be refused")
            .to_string();
        assert!(err.contains("immutable"), "unexpected error: {err}");
        set_file_readonly(&base_sqlite).unwrap();

        let _ = remove_readonly_tree(&root);
    }

    #[cfg(feature = "parse")]
    #[test]
    fn gc_reads_no_lease_a_lane_outlives_its_source_entry_and_autorebases_above_dirty_threshold() {
        let root = temp_repo_dir("gc-and-rebase");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);

        let lane_a = root.join("lane-a");
        let lane_b = root.join("lane-b");
        run_git_ok(
            &anchor,
            &["worktree", "add", "-b", "lane-a", lane_a.to_str().unwrap()],
        );
        run_git_ok(
            &anchor,
            &["worktree", "add", "-b", "lane-b", lane_b.to_str().unwrap()],
        );

        let lease_a = provision_stacked_worktree(&anchor, &lane_a).unwrap();
        assert_eq!(lease_a.generation_id, 1);
        let entry_a = published_dir(&anchor, &lease_a);
        assert!(entry_a.is_dir());

        // Advance anchor to generation 2 (committed, indexed at the new HEAD) and
        // provision lane-b on it.
        fs::write(anchor.join("src/extra.rs"), "pub fn gen2() {}\n").unwrap();
        run_git_ok(&anchor, &["add", "."]);
        run_git_ok(&anchor, &["commit", "-m", "gen2"]);
        index_tree(
            &anchor,
            &[
                "src/svc.go",
                "src/routes.ts",
                "src/limits.rs",
                "src/extra.rs",
            ],
        );
        let lease_b = provision_stacked_worktree(&anchor, &lane_b).unwrap();
        assert_eq!(lease_b.generation_id, 2);

        let common = registered_common_git_dir(&anchor).unwrap();

        // A corrupt lease changes nothing: GC reads no lease. The newest entry of
        // the donor stays, the older one goes, although lane-a was admitted from it.
        fs::write(stack_lease_path(&lane_a), b"{corrupt-json").unwrap();
        let gc1 = gc_unused_bases(&common).unwrap();
        assert_eq!(gc1.retained_generations, vec![2]);
        assert_eq!(gc1.removed_generations, vec![1]);
        assert!(!entry_a.exists(), "the non-newest entry must be deleted");

        // Restore a valid lease with a low dirty-file threshold (2 files). The
        // lane serves its own clone, so the deleted source entry is irrelevant:
        // it opens, and its auto-rebase promotes the admitted base.
        let mut custom_lease = lease_a.clone();
        custom_lease.rebases_after_dirty_files = 2;
        fs::write(
            stack_lease_path(&lane_a),
            serde_json::to_vec_pretty(&custom_lease).unwrap(),
        )
        .unwrap();

        for idx in 0..3 {
            fs::write(
                lane_a.join(format!("src/new_{idx}.rs")),
                format!("pub fn f_{idx}() -> usize {{ {idx} }}\n"),
            )
            .unwrap();
        }
        let mut stacked_a = StackedStore::open(&lane_a).unwrap();
        let sync_report = stacked_a.sync_worktree().unwrap();
        let promoted_db = sync_report
            .rebased_to_standalone
            .expect("3 dirty files > threshold 2 must auto-rebase to standalone store");
        assert!(promoted_db.is_file());
        assert!(!stack_lease_path(&lane_a).exists());
        assert!(
            !lease_a.base_dir().exists(),
            "materialize retires the lane's private base"
        );
        drop(stacked_a);

        let gc2 = gc_unused_bases(&common).unwrap();
        assert!(gc2.removed_generations.is_empty());
        assert_eq!(gc2.retained_generations, vec![2]);

        let _ = remove_readonly_tree(&root);
    }

    #[cfg(feature = "parse")]
    fn add_lane(anchor: &Path, root: &Path, name: &str) -> PathBuf {
        let lane = root.join(name);
        run_git_ok(
            anchor,
            &["worktree", "add", "-b", name, lane.to_str().unwrap()],
        );
        lane
    }

    #[cfg(feature = "parse")]
    fn make_tree_writable(dir: &Path) {
        make_tree_writable_best_effort(dir);
    }

    /// The shared cache entry a lane was admitted from: the one place a test
    /// may tamper to prove that nothing reads it after provision.
    #[cfg(feature = "parse")]
    fn published_dir(anchor: &Path, lease: &StackLease) -> PathBuf {
        let common = registered_common_git_dir(&anchor.canonicalize().unwrap()).unwrap();
        base_generation_dir(
            &common,
            // The entry's name does not depend on the epoch, which a lease
            // does not record.
            &BaseKey {
                generation_id: lease.generation_id,
                head_sha: lease.head_sha.clone(),
                source_repo_root: lease.source_repo_root.clone(),
                epoch: String::new(),
            },
        )
    }

    /// `<common_git_dir>/devmap/bases`.
    #[cfg(feature = "parse")]
    fn bases_dir_of(anchor: &Path) -> PathBuf {
        bases_root_dir(&registered_common_git_dir(&anchor.canonicalize().unwrap()).unwrap())
    }

    /// Flip the last byte of `file` (made writable first), then reseal it.
    #[cfg(feature = "parse")]
    fn corrupt_last_byte(file: &Path) {
        make_tree_writable_best_effort(file.parent().unwrap());
        let mut bytes = fs::read(file).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        fs::write(file, &bytes).unwrap();
        set_file_readonly(file).unwrap();
    }

    #[cfg(feature = "parse")]
    #[test]
    fn base_key_includes_source_and_head() {
        let root = temp_repo_dir("base-key");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let sib = add_lane(&anchor, &root, "sib");
        fs::write(sib.join("src/sib_only.rs"), "pub fn only_in_sibling() {}\n").unwrap();
        run_git_ok(&sib, &["add", "."]);
        run_git_ok(&sib, &["commit", "-m", "sibling"]);
        index_tree(
            &sib,
            &[
                "src/svc.go",
                "src/routes.ts",
                "src/limits.rs",
                "src/sib_only.rs",
            ],
        );
        let lane_main = add_lane(&anchor, &root, "lane-main");
        let lane_sib = add_lane(&sib, &root, "lane-sib");

        let lease_main = provision_stacked_worktree(&anchor, &lane_main).unwrap();
        let lease_sib = provision_stacked_worktree(&sib, &lane_sib).unwrap();
        assert_eq!(
            lease_main.generation_id, lease_sib.generation_id,
            "fixture must reproduce the colliding generation id"
        );
        assert_ne!(
            published_dir(&anchor, &lease_main),
            published_dir(&sib, &lease_sib),
            "two donors with the same generation id must not share a cache entry"
        );
        let stacked = StackedStore::open(&lane_sib).unwrap();
        let names: Vec<String> = stacked
            .symbols_for_file("src/sib_only.rs")
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(
            names.iter().any(|n| n == "only_in_sibling"),
            "lane provisioned from the sibling must read the sibling's symbols, got {names:?}"
        );
        drop(stacked);
        let _ = remove_readonly_tree(&root);
    }

    #[cfg(feature = "parse")]
    #[test]
    fn a_cache_entry_that_fails_verification_is_replaced_and_no_lane_notices() {
        let root = temp_repo_dir("reuse-sha");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane_a = add_lane(&anchor, &root, "lane-a");
        let lane_b = add_lane(&anchor, &root, "lane-b");
        let lane_c = add_lane(&anchor, &root, "lane-c");
        let lease = provision_stacked_worktree(&anchor, &lane_a).unwrap();
        let entry = published_dir(&anchor, &lease);
        let honest_manifest = fs::read(entry.join("manifest.json")).unwrap();

        // Bytes that no longer hash to the manifest: nobody reads the entry, so
        // it is a bad cache line, replaced, not a conflict.
        corrupt_last_byte(&entry.join("base.sqlite"));
        let lease_b = provision_stacked_worktree(&anchor, &lane_b)
            .expect("an entry whose bytes no longer hash to the manifest is replaced");
        assert_eq!(lease_b.base.sha256, lease.base.sha256);
        let b = StackedStore::open(&lane_b).unwrap();
        assert!(!symbol_names(&b, "src/svc.go").is_empty());

        // A manifest that names another HEAD under the expected directory name is replaced too.
        let manifest_path = entry.join("manifest.json");
        let mut manifest: BaseManifest = serde_json::from_slice(&honest_manifest).unwrap();
        manifest.head_sha = "0".repeat(40);
        set_file_writable(&manifest_path).unwrap();
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        provision_stacked_worktree(&anchor, &lane_c)
            .expect("a manifest for another head is replaced");
        let republished: BaseManifest =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        assert_eq!(republished.head_sha, lease.head_sha);
        // lane-a was admitted from the entry that was corrupted afterwards and
        // still serves its own clone.
        let a = StackedStore::open(&lane_a).unwrap();
        assert!(!symbol_names(&a, "src/svc.go").is_empty());
        let _ = remove_readonly_tree(&root);
    }

    #[cfg(feature = "parse")]
    #[test]
    fn open_refuses_swapped_base() {
        let root = temp_repo_dir("swap");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        let base_sqlite = lease.base_sqlite_path();

        // An identical-looking, valid, 0444 file under another inode is refused.
        let impostor = lease.base_dir().join("impostor.sqlite");
        fs::copy(&base_sqlite, &impostor).unwrap();
        set_file_readonly(&impostor).unwrap();
        fs::rename(&impostor, &base_sqlite).unwrap();
        let err = StackedStore::open(&lane)
            .expect_err("a swapped base.sqlite must be refused")
            .to_string();
        assert!(
            err.contains("replaced or modified"),
            "unexpected error: {err}"
        );
        assert!(
            err.contains("devmap seed --stacked --from"),
            "the refusal must name the recovery command: {err}"
        );
        let _ = remove_readonly_tree(&root);
    }

    #[cfg(feature = "parse")]
    #[test]
    fn sync_reports_git_failure_not_clean() {
        let root = temp_repo_dir("sync-git");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        provision_stacked_worktree(&anchor, &lane).unwrap();
        fs::write(lane.join("src/svc.go"), "package svc\n\nfunc Edited() {}\n").unwrap();
        let mut stacked = StackedStore::open(&lane).unwrap();
        // Break git for the lane: a gitlink to nowhere.
        fs::write(lane.join(".git"), "gitdir: /nonexistent/devmap-test\n").unwrap();
        let err = stacked
            .sync_worktree()
            .expect_err("a git failure must not read as a clean tree");
        assert!(
            err.to_string().contains("git"),
            "error must name git: {err}"
        );
        drop(stacked);
        let _ = remove_readonly_tree(&root);
    }

    #[cfg(feature = "parse")]
    #[test]
    fn materialize_waits_for_the_writer_lock_and_then_refuses() {
        let root = temp_repo_dir("materialize-lock");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        provision_stacked_worktree(&anchor, &lane).unwrap();
        let stacked = StackedStore::open(&lane).unwrap();

        let target_db = store_path(&stacked.worktree_root);
        fs::create_dir_all(target_db.parent().unwrap()).unwrap();
        let held = Store::lock_writer_at(&target_db, std::time::Duration::from_secs(5)).unwrap();
        let err = stacked
            .materialize_seeded_store_waiting(std::time::Duration::from_millis(300))
            .expect_err("a held writer lock must refuse the promotion")
            .to_string();
        assert!(
            !target_db.exists(),
            "nothing may be placed under a held lock"
        );
        assert!(
            stack_lease_path(&lane).is_file(),
            "the lease must survive a refusal"
        );
        assert!(!err.is_empty());
        drop(held);

        let placed = stacked.materialize_seeded_store().unwrap();
        assert!(placed.is_file());
        let again = stacked
            .materialize_seeded_store()
            .expect_err("must not overwrite a store");
        assert!(again.to_string().contains("existing store"), "{again}");
        drop(stacked);
        let _ = remove_readonly_tree(&root);
    }

    #[cfg(feature = "parse")]
    #[test]
    fn gc_ignores_unverifiable_worktrees_and_waits_on_the_bases_lock() {
        let root = temp_repo_dir("gc-unverifiable");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane_a = add_lane(&anchor, &root, "lane-a");
        let lane_b = add_lane(&anchor, &root, "lane-b");
        provision_stacked_worktree(&anchor, &lane_a).unwrap();

        // Advance the donor to generation 2 and stack lane-b on it.
        fs::write(anchor.join("src/extra.rs"), "pub fn gen2() {}\n").unwrap();
        run_git_ok(&anchor, &["add", "."]);
        run_git_ok(&anchor, &["commit", "-m", "gen2"]);
        index_tree(
            &anchor,
            &[
                "src/svc.go",
                "src/routes.ts",
                "src/limits.rs",
                "src/extra.rs",
            ],
        );
        provision_stacked_worktree(&anchor, &lane_b).unwrap();
        let common = registered_common_git_dir(&anchor).unwrap();

        // lane-a becomes unreachable: unmounted and locked. GC reads no lease, so
        // it neither refuses nor can hurt the lane.
        let moved = root.join("lane-a.unmounted");
        fs::rename(&lane_a, &moved).unwrap();
        fs::write(common.join("worktrees/lane-a/locked"), "unmounted\n").unwrap();
        let report = gc_unused_bases(&common).expect("an unverifiable worktree does not stop GC");
        assert_eq!(report.removed_generations, vec![1]);
        assert_eq!(report.retained_generations, vec![2]);

        // Mounted again, lane-a still serves the base it was admitted with.
        fs::rename(&moved, &lane_a).unwrap();
        fs::remove_file(common.join("worktrees/lane-a/locked")).unwrap();
        let a = StackedStore::open(&lane_a).expect("lane-a survives GC of its source entry");
        assert!(!symbol_names(&a, "src/svc.go").is_empty());

        // A held bases lock is a bounded refusal, not a race. The holder stands
        // in for another process (a provision), so it takes the file lock
        // directly: this thread's own lock-order bookkeeping must not see it.
        let bases_dir = bases_root_dir(&common);
        fs::create_dir_all(&bases_dir).unwrap();
        let held = Store::lock_file_at(&bases_dir.join(".lock"), std::time::Duration::from_secs(5))
            .unwrap();
        let err = gc_unused_bases_waiting(&common, std::time::Duration::from_millis(300))
            .expect_err("GC must not run while a provision holds the bases lock")
            .to_string();
        assert!(
            err.contains("another devmap writer"),
            "unexpected error: {err}"
        );
        drop(held);
        gc_unused_bases(&common).unwrap();
        let _ = remove_readonly_tree(&root);
    }

    #[cfg(feature = "parse")]
    #[test]
    fn publish_refuses_a_dirty_or_unbuilt_donor() {
        let root = temp_repo_dir("dirty-donor");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");

        fs::write(
            anchor.join("src/svc.go"),
            "package svc\n\nfunc Dirty() {}\n",
        )
        .unwrap();
        let err = provision_stacked_worktree(&anchor, &lane)
            .expect_err("a donor with uncommitted edits must be refused")
            .to_string();
        assert!(err.contains("uncommitted"), "unexpected error: {err}");
        assert!(
            err.contains("src/svc.go"),
            "must name the dirty path: {err}"
        );

        run_git_ok(&anchor, &["commit", "-am", "move head without reindexing"]);
        let err = provision_stacked_worktree(&anchor, &lane)
            .expect_err("a donor whose index predates HEAD must be refused")
            .to_string();
        assert!(err.contains("HEAD"), "unexpected error: {err}");
        let _ = remove_readonly_tree(&root);
    }

    // ---------------------------------------------------------------------
    // Tests from the adversarial pass, renamed to state the behaviour they
    // require. Each asserts the SAFE behaviour.
    // ---------------------------------------------------------------------

    #[cfg(feature = "parse")]
    fn symbol_names(stacked: &StackedStore, rel: &str) -> Vec<String> {
        stacked
            .symbols_for_file(rel)
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect()
    }

    #[cfg(feature = "parse")]
    fn other_repo_with(root: &Path, name: &str, extra_file: &str, extra_src: &str) -> PathBuf {
        let repo = root.join(name);
        seed_indexed_git_repo(&repo);
        fs::write(repo.join(extra_file), extra_src).unwrap();
        run_git_ok(&repo, &["add", "."]);
        run_git_ok(&repo, &["commit", "-m", "extra"]);
        index_tree(
            &repo,
            &["src/svc.go", "src/routes.ts", "src/limits.rs", extra_file],
        );
        repo
    }

    /// 14: a lease copied from another repository's lane must not open.
    #[cfg(feature = "parse")]
    #[test]
    fn open_refuses_a_lease_from_another_repository() {
        let root = temp_repo_dir("atk14");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane_a = add_lane(&anchor, &root, "lane-a");
        let other = other_repo_with(
            &root,
            "other",
            "src/other_only.rs",
            "pub fn only_in_other() {}\n",
        );
        let lane_o = add_lane(&other, &root, "lane-o");
        // `other_repo_with` commits the donor's index, so the new checkout
        // carries a store; a stacked lane has none.
        let committed_store = store_path(&lane_o);
        for suffix in ["", "-wal", "-shm", ".csr"] {
            let mut p = committed_store.clone().into_os_string();
            p.push(suffix);
            let _ = fs::remove_file(PathBuf::from(p));
        }
        provision_stacked_worktree(&anchor, &lane_a).unwrap();
        provision_stacked_worktree(&other, &lane_o).unwrap();

        // Copy the OTHER repo's lease over lane-a's (lane-a belongs to `anchor`).
        let canon_a = lane_a.canonicalize().unwrap();
        let canon_o = lane_o.canonicalize().unwrap();
        fs::copy(stack_lease_path(&canon_o), stack_lease_path(&canon_a)).unwrap();
        match StackedStore::open(&lane_a) {
            Err(_) => {}
            Ok(stacked) => {
                let names = symbol_names(&stacked, "src/other_only.rs");
                panic!(
                    "lane-a (repo `anchor`) opened with a lease for repo `other`: lease.worktree_root={} lane={}; \
                     it serves the other repo's graph: src/other_only.rs symbols = {names:?}",
                    stacked.lease().worktree_root,
                    canon_a.display()
                );
            }
        }
        let _ = remove_readonly_tree(&root);
    }

    /// 14b: a lease copied between two lanes of the same repo records the wrong root.
    #[cfg(feature = "parse")]
    #[test]
    fn open_refuses_a_lease_from_another_lane() {
        let root = temp_repo_dir("atk14b");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane_a = add_lane(&anchor, &root, "lane-a");
        let lane_b = add_lane(&anchor, &root, "lane-b");
        provision_stacked_worktree(&anchor, &lane_a).unwrap();
        let ca = lane_a.canonicalize().unwrap();
        let cb = lane_b.canonicalize().unwrap();
        fs::create_dir_all(stack_lease_path(&cb).parent().unwrap()).unwrap();
        fs::copy(stack_lease_path(&ca), stack_lease_path(&cb)).unwrap();
        let res = StackedStore::open(&lane_b);
        assert!(
            res.is_err(),
            "lane-b opened with lane-a's lease (lease.worktree_root={})",
            res.as_ref()
                .map(|s| s.lease().worktree_root.clone())
                .unwrap_or_default()
        );
        let _ = remove_readonly_tree(&root);
    }

    /// 15: a replaced `base.sqlite.csr` must not be served.
    #[cfg(feature = "parse")]
    #[test]
    fn materialize_refuses_a_tampered_csr_sidecar() {
        let root = temp_repo_dir("atk15");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let other = other_repo_with(
            &root,
            "other",
            "src/other_only.rs",
            "pub fn only_in_other() { }\npub fn caller() { only_in_other(); }\n",
        );
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        let base_dir = lease.base_dir();
        let csr = csr_path_for_store(base_dir.join("base.sqlite"));
        assert!(csr.is_file(), "fixture must have a csr sidecar");
        let honest_len = fs::metadata(&csr).unwrap().len();
        let other_csr = csr_path_for_store(store_path(&other));
        assert!(other_csr.is_file());
        make_tree_writable(&base_dir);
        fs::copy(&other_csr, &csr).unwrap();
        let tampered_len = fs::metadata(&csr).unwrap().len();
        set_file_readonly(&csr).unwrap();
        assert_ne!(honest_len, tampered_len, "fixture: CSRs must differ");
        let served = StackedStore::open(&lane).and_then(|s| s.materialize_seeded_store());
        eprintln!(
            "attack_15 outcome: {:?}",
            served.as_ref().map_err(|e| format!("{e:#}"))
        );
        assert!(
            served.is_err(),
            "a csr from another repository (len {tampered_len} vs {honest_len}) was mapped and served: {served:?}"
        );
        let _ = remove_readonly_tree(&root);
    }

    /// 16: edit in place, restore the exact mtime, reseal.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn open_refuses_an_in_place_edit_with_restored_mtime() {
        let root = temp_repo_dir("atk16");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        let base_dir = lease.base_dir();
        let base_sqlite = base_dir.join("base.sqlite");
        let orig_mtime = fs::metadata(&base_sqlite).unwrap().modified().unwrap();

        make_tree_writable(&base_dir);
        let mut bytes = fs::read(&base_sqlite).unwrap();
        let needle = b"Helper";
        let at = bytes
            .windows(needle.len())
            .position(|w| w == needle)
            .expect("symbol name present in the base bytes");
        bytes[at] = b'X'; // Helper -> Xelper
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut f = fs::OpenOptions::new()
                .write(true)
                .open(&base_sqlite)
                .unwrap();
            f.seek(SeekFrom::Start(at as u64)).unwrap();
            f.write_all(&bytes[at..at + 1]).unwrap();
            f.set_modified(orig_mtime).unwrap();
        }
        set_file_readonly(&base_sqlite).unwrap();

        match StackedStore::open(&lane) {
            Err(e) => {
                let m = format!("{e:#}");
                eprintln!("attack_16 refused with: {m}");
                assert!(
                    m.contains("replaced or modified"),
                    "refused, but not by the identity check: {m}"
                );
            }
            Ok(stacked) => {
                let names = symbol_names(&stacked, "src/svc.go");
                panic!("in-place edit with restored mtime was accepted; src/svc.go symbols now {names:?}");
            }
        }
        let _ = remove_readonly_tree(&root);
    }

    /// 5 (control): the same edit WITHOUT restoring mtime must be caught.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn open_refuses_an_in_place_edit_that_moves_mtime() {
        let root = temp_repo_dir("atk5");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        let base_dir = lease.base_dir();
        let base_sqlite = base_dir.join("base.sqlite");
        make_tree_writable(&base_dir);
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut bytes = fs::read(&base_sqlite).unwrap();
        let at = bytes.windows(6).position(|w| w == b"Helper").unwrap();
        bytes[at] = b'X';
        fs::write(&base_sqlite, &bytes).unwrap();
        set_file_readonly(&base_sqlite).unwrap();
        assert!(StackedStore::open(&lane).is_err());
        let _ = remove_readonly_tree(&root);
    }

    /// 17: real race. One thread rename-swaps honest <-> impostor while another
    /// loops `StackedStore::open`; any Ok instance serving the impostor's graph is a break.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn open_never_serves_a_base_swapped_during_open() {
        use std::sync::atomic::{AtomicBool, Ordering as AO};
        use std::sync::Arc;
        let root = temp_repo_dir("atk17");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let other = other_repo_with(
            &root,
            "other",
            "src/other_only.rs",
            "pub fn only_in_other() {}\n",
        );
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        let base_dir = lease.base_dir();
        let base_sqlite = base_dir.join("base.sqlite");
        let impostor = base_dir.join("impostor.sqlite");
        let hold = base_dir.join("hold.sqlite");
        make_tree_writable(&base_dir);
        fs::copy(store_path(&other), &impostor).unwrap();
        set_file_readonly(&impostor).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let swapper = {
            let (stop, base_sqlite, impostor, hold) = (
                stop.clone(),
                base_sqlite.clone(),
                impostor.clone(),
                hold.clone(),
            );
            std::thread::spawn(move || {
                while !stop.load(AO::Relaxed) {
                    let _ = fs::rename(&base_sqlite, &hold);
                    let _ = fs::rename(&impostor, &base_sqlite);
                    std::thread::sleep(std::time::Duration::from_micros(300));
                    let _ = fs::rename(&base_sqlite, &impostor);
                    let _ = fs::rename(&hold, &base_sqlite);
                    std::thread::sleep(std::time::Duration::from_micros(300));
                }
            })
        };
        let t = std::time::Instant::now();
        let (mut ok_honest, mut ok_impostor, mut refused) = (0u32, 0u32, 0u32);
        while t.elapsed() < std::time::Duration::from_secs(8) {
            match StackedStore::open(&lane) {
                Err(_) => refused += 1,
                Ok(s) => {
                    if symbol_names(&s, "src/other_only.rs").is_empty() {
                        ok_honest += 1;
                    } else {
                        ok_impostor += 1;
                    }
                }
            }
        }
        stop.store(true, AO::Relaxed);
        swapper.join().unwrap();
        eprintln!("attack_17: honest={ok_honest} impostor={ok_impostor} refused={refused}");
        assert_eq!(
            ok_impostor, 0,
            "opens that served the impostor graph (honest={ok_honest}, refused={refused})"
        );
        let _ = remove_readonly_tree(&root);
    }

    /// 2: donor dirtiness the status-only check could miss.
    #[cfg(feature = "parse")]
    #[test]
    fn publish_refuses_staged_untracked_and_hidden_donor_edits() {
        // staged edit
        let root = temp_repo_dir("atk2a");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        fs::write(
            anchor.join("src/svc.go"),
            "package svc\n\nfunc Staged() {}\n",
        )
        .unwrap();
        run_git_ok(&anchor, &["add", "src/svc.go"]);
        assert!(
            provision_stacked_worktree(&anchor, &lane).is_err(),
            "staged edit accepted"
        );
        let _ = remove_readonly_tree(&root);

        // untracked indexable file
        let root = temp_repo_dir("atk2b");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        fs::write(anchor.join("src/untracked.rs"), "pub fn u() {}\n").unwrap();
        assert!(
            provision_stacked_worktree(&anchor, &lane).is_err(),
            "untracked file accepted"
        );
        let _ = remove_readonly_tree(&root);

        // skip-worktree / assume-unchanged hide an edit from `git status`
        for flag in ["--skip-worktree", "--assume-unchanged"] {
            let root = temp_repo_dir("atk2c");
            let anchor = root.join("anchor");
            seed_indexed_git_repo(&anchor);
            let lane = add_lane(&anchor, &root, "lane");
            run_git_ok(&anchor, &["update-index", flag, "src/svc.go"]);
            fs::write(
                anchor.join("src/svc.go"),
                "package svc\n\nfunc Hidden() {}\n",
            )
            .unwrap();
            let r = provision_stacked_worktree(&anchor, &lane);
            assert!(
                r.is_err(),
                "edit hidden by {flag} accepted: base is stale vs the donor tree"
            );
            let _ = remove_readonly_tree(&root);
        }

        // index built from an edited tree, edit then reverted: status is clean,
        // the generation holds content that is not HEAD's.
        let root = temp_repo_dir("atk2d");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        fs::write(
            anchor.join("src/svc.go"),
            "package svc\n\nfunc Ghost() {}\n",
        )
        .unwrap();
        index_tree(&anchor, &["src/svc.go", "src/routes.ts", "src/limits.rs"]);
        run_git_ok(&anchor, &["checkout", "--", "src/svc.go"]);
        match provision_stacked_worktree(&anchor, &lane) {
            Err(_) => {}
            Ok(_) => {
                let s = StackedStore::open(&lane).unwrap();
                panic!(
                    "donor index built from a since-reverted edit was accepted; base src/svc.go symbols = {:?}",
                    symbol_names(&s, "src/svc.go")
                );
            }
        }
        let _ = remove_readonly_tree(&root);
    }

    /// 1: two donors at the same HEAD with different content, and a rebuilt donor.
    #[cfg(feature = "parse")]
    #[test]
    fn same_head_donors_never_share_a_graph() {
        let root = temp_repo_dir("atk1");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let sib = add_lane(&anchor, &root, "sib");
        // Same HEAD, same generation id (1), but the sibling indexed a subset.
        index_tree(&sib, &["src/svc.go"]);
        let lane_main = add_lane(&anchor, &root, "lane-main");
        let lane_sib = add_lane(&sib, &root, "lane-sib");
        provision_stacked_worktree(&anchor, &lane_main).unwrap();
        // The sibling's index (a subset) does not match its working tree: that
        // donor is refused, so its lane can never serve a graph that differs from
        // what is checked out. The main lane still serves the full graph.
        let err = provision_stacked_worktree(&sib, &lane_sib)
            .expect_err("a donor whose index is a subset of its tree must be refused")
            .to_string();
        assert!(err.contains("does not match"), "unexpected error: {err}");
        let m = StackedStore::open(&lane_main).unwrap();
        assert!(!symbol_names(&m, "src/routes.ts").is_empty());
        drop(m);
        let _ = remove_readonly_tree(&root);
    }

    /// 20 / FIX 5: donor store deleted and rebuilt at the same HEAD.
    #[cfg(feature = "parse")]
    #[test]
    fn a_rebuilt_donor_can_still_stack() {
        let root = temp_repo_dir("atk20");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane_a = add_lane(&anchor, &root, "lane-a");
        let lane_b = add_lane(&anchor, &root, "lane-b");
        provision_stacked_worktree(&anchor, &lane_a).unwrap();
        // delete + rebuild the donor store: generation id restarts at 1, bytes differ
        let db = store_path(&anchor);
        for suffix in ["", "-wal", "-shm", ".csr"] {
            let mut p = db.clone().into_os_string();
            p.push(suffix);
            let _ = fs::remove_file(PathBuf::from(p));
        }
        std::thread::sleep(std::time::Duration::from_millis(1100));
        index_tree(&anchor, &["src/svc.go", "src/routes.ts", "src/limits.rs"]);
        let r = provision_stacked_worktree(&anchor, &lane_b);
        assert!(
            r.is_ok(),
            "rebuilt donor at the same HEAD cannot be stacked (liveness): {:?}",
            r.err().map(|e| e.to_string())
        );
        let _ = remove_readonly_tree(&root);
    }

    /// 3: git that fails, hangs, or returns garbage must not read as a clean delta.
    #[cfg(feature = "parse")]
    fn with_fake_git<T>(root: &Path, body: &str, f: impl FnOnce() -> T) -> T {
        use std::os::unix::fs::PermissionsExt;
        let bin = root.join("fakebin");
        fs::create_dir_all(&bin).unwrap();
        let script = bin.join("git");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\n[ \"$1\" = version ] && {{ echo 'git version 2.54.0'; exit 0; }}\n{body}\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        release_git_path_shared();
        let exclusive = GIT_PATH.write().unwrap_or_else(|p| p.into_inner());
        let old = std::env::var_os("PATH").unwrap_or_default();
        let mut parts = vec![bin.clone()];
        parts.extend(std::env::split_paths(&old));
        std::env::set_var("PATH", std::env::join_paths(parts).unwrap());
        let out = f();
        std::env::set_var("PATH", old);
        drop(exclusive);
        hold_git_path_shared();
        out
    }

    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn unsynced_paths_refuses_git_failure_hang_and_garbage() {
        let root = temp_repo_dir("atk3");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        provision_stacked_worktree(&anchor, &lane).unwrap();
        fs::write(lane.join("src/svc.go"), "package svc\n\nfunc Edited() {}\n").unwrap();
        let stacked = StackedStore::open(&lane).unwrap();
        let mut failures = Vec::new();

        // non-zero
        let r = with_fake_git(&root, "echo boom >&2; exit 3", || stacked.unsynced_paths());
        if let Ok(paths) = &r {
            failures.push(format!("git exit 3 -> Ok({:?})", paths));
        }
        // garbage on stdout with exit 0 (no NUL framing, not a porcelain record)
        let r = with_fake_git(&root, "printf 'garbage'; exit 0", || {
            stacked.unsynced_paths()
        });
        match r {
            Ok(p) if p.is_empty() => failures.push(
                "git printing 'garbage' (exit 0) -> Ok(empty): reads as a clean delta".into(),
            ),
            _ => {}
        }
        // non-UTF-8
        let r = with_fake_git(&root, "printf '?? \\377\\376\\377\\000'; exit 0", || {
            stacked.unsynced_paths()
        });
        if let Ok(paths) = &r {
            failures.push(format!("non-UTF-8 -> Ok({:?})", paths));
        }
        // hang
        let t = std::time::Instant::now();
        let r = with_fake_git(&root, "exec sleep 120", || stacked.unsynced_paths());
        let dt = t.elapsed();
        if let Ok(paths) = &r {
            failures.push(format!("hung git -> Ok({:?})", paths));
        }
        if dt > std::time::Duration::from_secs(25) {
            failures.push(format!("hung git not bounded: {dt:?}"));
        }
        eprintln!("attack_3: hang returned after {dt:?}");
        drop(stacked);
        let _ = remove_readonly_tree(&root);
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// 22: opening a lane for READ (hook / MCP) must not create or write `delta.sqlite`.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn open_does_not_create_delta_sqlite() {
        let root = temp_repo_dir("atk22");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        provision_stacked_worktree(&anchor, &lane).unwrap();
        let canon = lane.canonicalize().unwrap();
        let delta = stack_delta_path(&canon);
        for suffix in ["", "-wal", "-shm"] {
            let mut p = delta.clone().into_os_string();
            p.push(suffix);
            let _ = fs::remove_file(PathBuf::from(p));
        }
        let _ = StackedStore::open(&lane);
        assert!(
            !delta.exists(),
            "a read-path open created {}",
            delta.display()
        );
        let _ = remove_readonly_tree(&root);
    }

    /// 19 (lib level): `unsynced_paths` has no budget parameter, so a slow git is waited on in full.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn unsynced_paths_within_is_bounded_by_the_budget_it_is_given() {
        let root = temp_repo_dir("atk19");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        provision_stacked_worktree(&anchor, &lane).unwrap();
        let stacked = StackedStore::open(&lane).unwrap();
        // Timed inside the closure: `with_fake_git` first waits for any other
        // fake-git test to finish, and that wait is not the git's. Only `status`
        // and `diff` are slow: dc-proc's own startup probes (`version`, `config`)
        // run before the caller's bound applies, so they answer at once.
        let (res, dt) = with_fake_git(
            &root,
            "for a in \"$@\"; do case \"$a\" in status|diff) exec sleep 2;; esac; done; exit 0",
            || {
                let t = std::time::Instant::now();
                let res = stacked.unsynced_paths_within(std::time::Duration::from_millis(100));
                (res, t.elapsed())
            },
        );
        assert!(
            res.is_err(),
            "a git that cannot answer in time must be an Err"
        );
        assert!(
            dt < std::time::Duration::from_secs(1),
            "unsynced_paths_within(100ms) blocked {dt:?} on a slow git"
        );
        drop(stacked);
        let _ = remove_readonly_tree(&root);
    }

    /// 12: a lane never reads the cache entry's manifest: truncated, missing
    /// or empty, the lane still opens and serves the bytes it admitted.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a_damaged_or_missing_cache_manifest_does_not_affect_an_admitted_lane() {
        let root = temp_repo_dir("atk12");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        let entry = published_dir(&anchor, &lease);
        let manifest = entry.join("manifest.json");
        let good = fs::read(&manifest).unwrap();
        make_tree_writable(&entry);
        let head_of = |lane: &Path| {
            StackedStore::open(lane)
                .expect("a lane does not read the cache manifest")
                .base_store()
                .latest_generation_head_sha()
                .unwrap()
        };
        let honest = head_of(&lane);
        fs::write(&manifest, &good[..good.len() / 2]).unwrap();
        assert_eq!(head_of(&lane), honest, "truncated manifest");
        fs::remove_file(&manifest).unwrap();
        assert_eq!(head_of(&lane), honest, "missing manifest");
        fs::write(&manifest, b"{}").unwrap();
        assert_eq!(head_of(&lane), honest, "empty manifest");
        fs::write(&manifest, vec![b' '; 2 << 20]).unwrap();
        assert_eq!(head_of(&lane), honest, "oversized manifest");
        let _ = remove_readonly_tree(&root);
    }

    /// 4: GC keeps the newest entry per donor and deletes the rest with no lease
    /// scan; a lane never loses its base because its source entry is collected.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn gc_keeps_the_newest_entry_per_donor_and_no_lane_loses_its_base() {
        let root = temp_repo_dir("atk4");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane_a = add_lane(&anchor, &root, "lane-a");
        let lane_b = add_lane(&anchor, &root, "lane-b");
        let lease_a = provision_stacked_worktree(&anchor, &lane_a).unwrap();
        // donor advances: new commit, new generation
        fs::write(anchor.join("src/more.rs"), "pub fn more() {}\n").unwrap();
        run_git_ok(&anchor, &["add", "."]);
        run_git_ok(&anchor, &["commit", "-m", "advance"]);
        index_tree(
            &anchor,
            &[
                "src/svc.go",
                "src/routes.ts",
                "src/limits.rs",
                "src/more.rs",
            ],
        );
        let lease_b = provision_stacked_worktree(&anchor, &lane_b).unwrap();
        assert_ne!(
            published_dir(&anchor, &lease_a),
            published_dir(&anchor, &lease_b)
        );
        let common = registered_common_git_dir(&anchor.canonicalize().unwrap()).unwrap();
        let rep = gc_unused_bases(&common).unwrap();
        assert_eq!(rep.removed_generations, vec![1], "{rep:?}");
        assert_eq!(rep.retained_generations, vec![2], "{rep:?}");
        assert!(!published_dir(&anchor, &lease_a).exists());
        assert!(published_dir(&anchor, &lease_b).is_dir());
        StackedStore::open(&lane_a).expect("lane-a lost its base after GC");
        StackedStore::open(&lane_b).expect("lane-b lost its base after GC");
        // collecting again changes nothing
        let rep = gc_unused_bases(&common).unwrap();
        assert!(rep.removed_generations.is_empty(), "{rep:?}");
        let _ = remove_readonly_tree(&root);
    }

    /// 9: a store is placed before the lease is retired, so a lease that cannot
    /// be removed leaves a store beside a STALE lease: the lane is plain, and the
    /// next `open_if_leased_for_sync` retires the lease under the writer lock.
    #[cfg(all(feature = "parse", target_os = "macos"))]
    #[test]
    fn materialize_with_an_unremovable_lease_leaves_a_stale_lease_the_next_sync_retires() {
        let root = temp_repo_dir("atk9");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        provision_stacked_worktree(&anchor, &lane).unwrap();
        fs::write(lane.join("src/svc.go"), "package svc\n\nfunc Edited() {}\n").unwrap();
        let mut stacked = StackedStore::open(&lane).unwrap();
        stacked.sync_worktree().unwrap();
        let canon = lane.canonicalize().unwrap();
        let lease = stack_lease_path(&canon);
        let ok = Command::new("chflags")
            .arg("uchg")
            .arg(&lease)
            .status()
            .unwrap()
            .success();
        assert!(ok, "chflags uchg");
        let res = stacked.materialize_seeded_store();
        let store_exists = store_path(&canon).exists();
        let lease_exists = lease.exists();
        // Not leased any more: the store beside the lease wins.
        let during = format!(
            "{:#}",
            StackedStore::open_if_leased(&lane).expect_err("a read refuses")
        );
        let _ = Command::new("chflags").arg("nouchg").arg(&lease).status();
        drop(stacked);
        assert!(res.is_ok(), "the store was placed: {res:?}");
        assert!(
            store_exists && lease_exists,
            "fixture: a store and a stale lease"
        );
        assert!(
            during.contains("devmap seed --sync-delta"),
            "a read names the recovery: {during}"
        );
        assert!(lease.exists(), "a read deletes nothing");
        assert!(matches!(
            StackedStore::open_if_leased_for_sync(&lane),
            Ok(SyncTarget::RetiredStaleLease)
        ));
        assert!(!lease.exists(), "the stale lease is retired");
        assert!(!stack_dir(&canon).exists(), "and so is the private base");
        assert!(!stack_delta_path(&canon).exists(), "and the delta");
        assert!(store_path(&canon).is_file(), "the store stays");
        let _ = remove_readonly_tree(&root);
    }

    /// 13 / FIX 3: an artifact-seed failure after the lease is written must not
    /// leave a leased lane behind a failed provision.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn provision_failure_leaves_no_lease() {
        let root = temp_repo_dir("atk13");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let src_graph = code_graph_path(anchor.canonicalize().unwrap());
        fs::create_dir_all(src_graph.parent().unwrap()).unwrap();
        fs::write(&src_graph, "{}").unwrap();
        let lane_graph = code_graph_path(lane.canonicalize().unwrap());
        // "graph is a file": the destination's parent directory cannot be created.
        let graph_parent = lane_graph.parent().unwrap().to_path_buf();
        fs::create_dir_all(graph_parent.parent().unwrap()).unwrap();
        fs::write(&graph_parent, "not a directory").unwrap();
        let res = provision_stacked_worktree(&anchor, &lane);
        let lease_exists = stack_lease_path(lane.canonicalize().unwrap()).exists();
        assert!(res.is_err(), "fixture: artifact seeding must have failed");
        eprintln!("attack_13 err: {:#}", res.as_ref().err().unwrap());
        assert!(
            !lease_exists,
            "provision returned Err ({:?}) but left a stack lease behind",
            res.err().map(|e| e.to_string())
        );
        let canon = lane.canonicalize().unwrap();
        assert!(
            !stack_dir(&canon).exists(),
            "a failed provision left a base"
        );
        let strays: Vec<_> = fs::read_dir(stack_dir(&canon).parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("stack"))
            .collect();
        assert!(strays.is_empty(), "staging left behind: {strays:?}");
        let _ = remove_readonly_tree(&root);
    }

    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn publish_replaces_an_entry_with_an_oversized_manifest_without_slurping_it() {
        let root = temp_repo_dir("big-manifest");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane_a = add_lane(&anchor, &root, "lane-a");
        let lane_b = add_lane(&anchor, &root, "lane-b");
        let lease = provision_stacked_worktree(&anchor, &lane_a).unwrap();
        let entry = published_dir(&anchor, &lease);
        make_tree_writable(&entry);
        fs::write(entry.join("manifest.json"), vec![b' '; 2 << 20]).unwrap();
        set_file_readonly(&entry.join("manifest.json")).unwrap();
        let err = read_base_manifest(&entry)
            .expect_err("a 2 MiB manifest must be refused, not slurped")
            .to_string();
        assert!(err.contains("larger than"), "unexpected error: {err}");
        provision_stacked_worktree(&anchor, &lane_b).expect("the bad entry is replaced");
        assert!(fs::metadata(entry.join("manifest.json")).unwrap().len() < 4096);
        let _ = remove_readonly_tree(&root);
    }

    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn materialize_refuses_a_tampered_csr_and_changes_nothing() {
        let root = temp_repo_dir("materialize-csr");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        let stacked = StackedStore::open(&lane).unwrap();
        // Tamper while the store is open: only the content check can catch it now.
        let base_dir = lease.base_dir();
        let csr = csr_path_for_store(base_dir.join("base.sqlite"));
        set_file_writable(&csr).unwrap();
        let mut bytes = fs::read(&csr).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        fs::write(&csr, &bytes).unwrap();
        set_file_readonly(&csr).unwrap();
        let err = stacked
            .materialize_seeded_store()
            .expect_err("a tampered CSR must not be copied into a standalone store")
            .to_string();
        assert!(err.contains("CSR sidecar"), "unexpected error: {err}");
        let canon = lane.canonicalize().unwrap();
        assert!(!store_path(&canon).exists(), "no store may be placed");
        assert!(stack_lease_path(&canon).is_file(), "the lease must remain");
        drop(stacked);
        let _ = remove_readonly_tree(&root);
    }

    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn provision_replaces_a_cache_entry_with_a_bad_sha_even_while_a_lane_stands_on_its_clone() {
        let root = temp_repo_dir("bad-sha");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane_a = add_lane(&anchor, &root, "lane-a");
        let lane_b = add_lane(&anchor, &root, "lane-b");
        let lease = provision_stacked_worktree(&anchor, &lane_a).unwrap();
        let entry = published_dir(&anchor, &lease);
        corrupt_last_byte(&entry.join("base.sqlite"));
        set_dir_mode(&entry, 0o555).unwrap();

        // lane-a stands on its own clone, so nothing is "leased" in the cache:
        // the damaged entry is replaced for lane-b and lane-a is unaffected.
        provision_stacked_worktree(&anchor, &lane_b).expect("a damaged entry is replaced");
        StackedStore::open(&lane_b).expect("the replacement base opens");
        StackedStore::open(&lane_a).expect("lane-a is unaffected");
        let _ = remove_readonly_tree(&root);
    }

    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a_missing_delta_reads_as_empty_and_only_a_sync_creates_it() {
        let root = temp_repo_dir("delta-lazy");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        provision_stacked_worktree(&anchor, &lane).unwrap();
        let canon = lane.canonicalize().unwrap();
        fs::write(lane.join("src/svc.go"), "package svc\n\nfunc Edited() {}\n").unwrap();

        let mut stacked = StackedStore::open(&lane).unwrap();
        assert!(!stacked.delta_present());
        assert_eq!(stacked.status().unwrap().dirty_files, 0);
        assert!(
            !stack_delta_path(&canon).exists(),
            "open must not create it"
        );
        stacked.sync_worktree().unwrap();
        assert!(stack_delta_path(&canon).is_file(), "a sync creates it");
        drop(stacked);

        let again = StackedStore::open(&lane).unwrap();
        assert!(again.delta_present());
        assert_eq!(again.status().unwrap().dirty_files, 1);
        let _ = remove_readonly_tree(&root);
    }

    // ===================== attack round 2: identity of the base =====================
    //
    // These began as the round-2 attack's failing tests. Each now states the
    // behaviour the redesign guarantees: a lane serves the bytes it admitted at
    // provision, whatever another checkout does to the shared cache afterwards.

    /// A cache entry that looks honest (the honest manifest, verbatim) but holds
    /// another repository's `base.sqlite`.
    #[cfg(all(feature = "parse", unix))]
    fn a2_impostor_dir(bases: &Path, entry: &Path, other: &Path) -> PathBuf {
        let imp_dir = bases.join("imp-dir");
        fs::create_dir_all(&imp_dir).unwrap();
        fs::copy(entry.join("manifest.json"), imp_dir.join("manifest.json")).unwrap();
        fs::copy(store_path(other), imp_dir.join("base.sqlite")).unwrap();
        set_file_readonly(&imp_dir.join("base.sqlite")).unwrap();
        set_file_readonly(&imp_dir.join("manifest.json")).unwrap();
        set_dir_mode(&imp_dir, 0o555).unwrap();
        imp_dir
    }

    #[cfg(all(feature = "parse", target_os = "macos"))]
    fn a2_swap(a: &Path, b: &Path) -> bool {
        use std::os::unix::ffi::OsStrExt;
        let ca = std::ffi::CString::new(a.as_os_str().as_bytes()).unwrap();
        let cb = std::ffi::CString::new(b.as_os_str().as_bytes()).unwrap();
        // atomic exchange (RENAME_SWAP): the name never disappears
        unsafe { libc::renamex_np(ca.as_ptr(), cb.as_ptr(), libc::RENAME_SWAP) == 0 }
    }

    /// 1a (deterministic): swapping the cache entry for an impostor, before,
    /// between or after the stages of an open, cannot change what the lane
    /// serves, because an open names nothing under `bases/`.
    #[cfg(all(feature = "parse", target_os = "macos"))]
    #[test]
    fn a2_1a_dir_swap_of_the_cache_entry_does_not_change_what_open_serves() {
        let root = temp_repo_dir("a2-1a-sim");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let other = other_repo_with(
            &root,
            "other",
            "src/other_only.rs",
            "pub fn only_in_other() {}\n",
        );
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        let entry = published_dir(&anchor, &lease);
        let imp_dir = a2_impostor_dir(&bases_dir_of(&anchor), &entry, &other);

        let honest_head = StackedStore::open(&lane)
            .unwrap()
            .base_store()
            .latest_generation_head_sha()
            .unwrap();
        assert!(a2_swap(&entry, &imp_dir));
        let swapped = StackedStore::open(&lane).expect("open does not read the cache entry");
        assert_eq!(
            swapped.base_store().latest_generation_head_sha().unwrap(),
            honest_head
        );
        assert!(symbol_names(&swapped, "src/other_only.rs").is_empty());
        assert!(a2_swap(&entry, &imp_dir)); // swapped back
        let back = StackedStore::open(&lane).unwrap();
        assert_eq!(
            back.base_store().latest_generation_head_sha().unwrap(),
            honest_head
        );
        let _ = remove_readonly_tree(&root);
    }

    /// 1a (real race): atomic dir exchange thread vs <=2000 opens of the real
    /// StackedStore::open. Two threads only.
    #[cfg(all(feature = "parse", target_os = "macos"))]
    #[test]
    fn a2_1a_dir_swap_race_never_serves_the_impostor() {
        use std::sync::atomic::{AtomicBool, Ordering as AO};
        use std::sync::Arc;
        let root = temp_repo_dir("a2-1a-race");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let other = other_repo_with(
            &root,
            "other",
            "src/other_only.rs",
            "pub fn only_in_other() {}\n",
        );
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        let entry = published_dir(&anchor, &lease);
        let imp_dir = a2_impostor_dir(&bases_dir_of(&anchor), &entry, &other);
        let honest_head = {
            let s = StackedStore::open(&lane).unwrap();
            s.base_store().latest_generation_head_sha().unwrap()
        };
        let stop = Arc::new(AtomicBool::new(false));
        let swapper = {
            let (stop, a, b) = (stop.clone(), entry.clone(), imp_dir.clone());
            std::thread::spawn(move || {
                let mut n = 0u64;
                while !stop.load(AO::Relaxed) {
                    if a2_swap(&a, &b) {
                        n += 1;
                    }
                    std::thread::yield_now();
                }
                n
            })
        };
        let (mut ok_honest, mut ok_imp, mut refused) = (0u32, 0u32, 0u32);
        let t = std::time::Instant::now();
        for _ in 0..2000 {
            if t.elapsed() > std::time::Duration::from_secs(60) {
                break;
            }
            match StackedStore::open(&lane) {
                Err(_) => refused += 1,
                Ok(s) => {
                    if s.base_store().latest_generation_head_sha().unwrap() == honest_head {
                        ok_honest += 1;
                    } else {
                        ok_imp += 1;
                    }
                }
            }
        }
        stop.store(true, AO::Relaxed);
        let swaps = swapper.join().unwrap();
        eprintln!(
            "a2_1a_race: honest={ok_honest} impostor={ok_imp} refused={refused} swaps={swaps}"
        );
        assert_eq!(
            ok_imp, 0,
            "impostor={ok_imp} (honest={ok_honest}, refused={refused}, swaps={swaps})"
        );
        assert_eq!(
            refused, 0,
            "an open that names nothing in bases/ has nothing to be refused over"
        );
        let _ = remove_readonly_tree(&root);
    }

    /// 1a (static, same-uid): an impostor entry whose manifest is self-consistent
    /// (sha recomputed from the impostor file) swapped in permanently. Open is
    /// unaffected; the next provision refuses to admit it and replaces it.
    #[cfg(all(feature = "parse", target_os = "macos"))]
    #[test]
    fn a2_1a_static_forged_cache_entry_is_replaced_and_never_admitted() {
        let root = temp_repo_dir("a2-1a-static");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let lane2 = add_lane(&anchor, &root, "lane-2");
        let other = other_repo_with(
            &root,
            "other",
            "src/other_only.rs",
            "pub fn only_in_other() {}\n",
        );
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        let entry = published_dir(&anchor, &lease);
        let imp_dir = bases_dir_of(&anchor).join("imp-dir");
        fs::create_dir_all(&imp_dir).unwrap();
        fs::copy(store_path(&other), imp_dir.join("base.sqlite")).unwrap();
        set_file_readonly(&imp_dir.join("base.sqlite")).unwrap();
        let mut manifest: BaseManifest =
            serde_json::from_slice(&fs::read(entry.join("manifest.json")).unwrap()).unwrap();
        manifest.base_sqlite_sha256 = sha256_file_hex(&imp_dir.join("base.sqlite")).unwrap();
        manifest.csr_sha256 = None;
        fs::write(
            imp_dir.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        set_dir_mode(&imp_dir, 0o555).unwrap();
        assert!(a2_swap(&entry, &imp_dir));

        let s = StackedStore::open(&lane).expect("a lane does not read the cache entry");
        assert!(
            symbol_names(&s, "src/other_only.rs").is_empty(),
            "served the impostor's graph"
        );
        drop(s);

        let lease2 = provision_stacked_worktree(&anchor, &lane2)
            .expect("a forged entry is replaced by an honest publish");
        let s2 = StackedStore::open(&lane2).unwrap();
        assert!(
            symbol_names(&s2, "src/other_only.rs").is_empty(),
            "admitted the impostor's graph"
        );
        assert_eq!(lease2.head_sha, lease.head_sha);
        let _ = remove_readonly_tree(&root);
    }

    /// 1a (admission): the entry is swapped for a self-consistent impostor
    /// between publish and clone. `admit_published_base` is refused, not served.
    #[cfg(all(feature = "parse", target_os = "macos"))]
    #[test]
    fn a2_provision_admission_refuses_an_impostor_swapped_in_after_publish() {
        let root = temp_repo_dir("a2-admit");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let other = other_repo_with(
            &root,
            "other",
            "src/other_only.rs",
            "pub fn only_in_other() {}\n",
        );
        let (entry, manifest) = publish_base_snapshot(&anchor).unwrap();
        let key = BaseKey::of_manifest(&manifest).unwrap();
        let imp_dir = bases_dir_of(&anchor).join("imp-dir");
        fs::create_dir_all(&imp_dir).unwrap();
        fs::copy(store_path(&other), imp_dir.join("base.sqlite")).unwrap();
        set_file_readonly(&imp_dir.join("base.sqlite")).unwrap();
        // Same CSR as the donor's, so the impostor differs only in its graph and
        // admission reaches the sha and key checks rather than a missing file.
        fs::copy(entry.join(BASE_CSR), imp_dir.join(BASE_CSR)).unwrap();
        set_file_readonly(&imp_dir.join(BASE_CSR)).unwrap();
        let mut forged = manifest.clone();
        forged.base_sqlite_sha256 = sha256_file_hex(&imp_dir.join("base.sqlite")).unwrap();
        fs::write(
            imp_dir.join("manifest.json"),
            serde_json::to_vec_pretty(&forged).unwrap(),
        )
        .unwrap();
        set_dir_mode(&imp_dir, 0o555).unwrap();
        // The swap lands after publish returned `entry` and before admission.
        assert!(a2_swap(&entry, &imp_dir));
        let stage = lane.canonicalize().unwrap().join("stage");
        fs::create_dir_all(&stage).unwrap();
        // Held to the sha publish computed (the entry's own manifest is never
        // read), the impostor's bytes are refused.
        let err = match admit_published_base(&entry, &key, &manifest, &stage) {
            Ok(_) => panic!("an impostor swapped in after publish was admitted"),
            Err(err) => format!("{err:#}"),
        };
        assert!(
            err.contains("publish verified"),
            "unexpected refusal: {err}"
        );
        // Even when the sha is the impostor's own, its generation, HEAD and root
        // are not the donor's.
        let _ = fs::remove_dir_all(&stage);
        fs::create_dir_all(&stage).unwrap();
        let err = match admit_published_base(&entry, &key, &forged, &stage) {
            Ok(_) => panic!("an impostor with a self-consistent sha was admitted"),
            Err(err) => format!("{err:#}"),
        };
        assert!(err.contains("generation"), "unexpected refusal: {err}");

        // A swap whose manifest still describes the honest bytes fails the sha check.
        assert!(a2_swap(&entry, &imp_dir));
        make_tree_writable(&imp_dir);
        fs::write(
            imp_dir.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        set_file_readonly(&imp_dir.join("base.sqlite")).unwrap();
        set_file_readonly(&imp_dir.join(BASE_CSR)).unwrap();
        set_dir_mode(&imp_dir, 0o555).unwrap();
        assert!(a2_swap(&entry, &imp_dir));
        let _ = fs::remove_dir_all(&stage);
        fs::create_dir_all(&stage).unwrap();
        let err = match admit_published_base(&entry, &key, &manifest, &stage) {
            Ok(_) => {
                panic!("an entry whose manifest still describes the honest bytes was admitted")
            }
            Err(err) => format!("{err:#}"),
        };
        assert!(
            err.contains("publish verified"),
            "unexpected refusal: {err}"
        );
        let _ = remove_readonly_tree(&root);
    }

    /// 1b: reseal after a file-level swap away and back must be refused.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a2_1b_file_swap_away_and_back_with_reseal_is_refused() {
        let root = temp_repo_dir("a2-1b");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        let base_dir = lease.base_dir();
        let sqlite = base_dir.join("base.sqlite");
        let hold = base_dir.join("hold.sqlite");
        fs::rename(&sqlite, &hold).unwrap();
        fs::rename(&hold, &sqlite).unwrap();
        match StackedStore::open(&lane) {
            Err(e) => {
                let m = format!("{e:#}");
                eprintln!("a2_1b refused: {m}");
                assert!(
                    m.contains("replaced or modified"),
                    "refused for another reason: {m}"
                );
            }
            Ok(_) => panic!("file swapped away and back: open accepted"),
        }
        let _ = remove_readonly_tree(&root);
    }

    /// 1c: the lane's `stack/` directory replaced by a symlink is refused.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a2_1c_symlinked_base_dir_is_refused() {
        let root = temp_repo_dir("a2-1c-sym");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        let base_dir = lease.base_dir();
        let away = root.join("away-stack");
        fs::rename(&base_dir, &away).unwrap();
        std::os::unix::fs::symlink(&away, &base_dir).unwrap();
        match StackedStore::open(&lane) {
            Err(e) => {
                let text = format!("{e:#}");
                assert!(
                    text.contains("cannot open the lane's base directory"),
                    "{text}"
                );
                assert!(text.contains("devmap seed --stacked"), "{text}");
            }
            Ok(_) => panic!("a stack dir that is a symlink to a dir outside the lane was opened"),
        }
        let _ = remove_readonly_tree(&root);
    }

    /// 1c: `bases/` turned into a symlink after provision does not touch an
    /// admitted lane; provisioning through it is refused (no links followed).
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a2_1c_symlinked_cache_dir_does_not_affect_a_lane_and_is_refused_for_provision() {
        let root = temp_repo_dir("a2-1c-bases");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let lane2 = add_lane(&anchor, &root, "lane-2");
        provision_stacked_worktree(&anchor, &lane).unwrap();
        let bases = bases_dir_of(&anchor);
        let elsewhere = root.join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        let moved = elsewhere.join("bases");
        fs::rename(&bases, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &bases).unwrap();
        StackedStore::open(&lane).expect("open names nothing under bases/");
        let err = provision_stacked_worktree(&anchor, &lane2)
            .expect_err("provisioning through a symlinked bases/ must be refused");
        assert!(
            format!("{err:#}").contains("without following links"),
            "{err:#}"
        );
        let _ = remove_readonly_tree(&root);
    }

    /// 1c: a lease can no longer name a base directory at all, so there is no
    /// lexical `..` to escape the cache with: a lease that carries one is refused.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a2_1c_lease_that_names_a_base_dir_is_refused_there_is_no_path_to_escape_with() {
        let root = temp_repo_dir("a2-1c-dotdot");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        provision_stacked_worktree(&anchor, &lane).unwrap();
        let lease_path = stack_lease_path(lane.canonicalize().unwrap());
        let mut l: serde_json::Value =
            serde_json::from_slice(&fs::read(&lease_path).unwrap()).unwrap();
        l["base_dir"] = serde_json::Value::String("/tmp/../outside/gen-1".into());
        fs::write(&lease_path, serde_json::to_vec_pretty(&l).unwrap()).unwrap();
        let err = StackedStore::open(&lane).expect_err("a lease naming a base_dir must be refused");
        assert!(
            format!("{err:#}").contains("invalid stack lease"),
            "{err:#}"
        );
    }

    /// Rewrite one top-level lease field and nothing else the base's stat
    /// identity covers: the lease file is not part of that identity.
    #[cfg(all(feature = "parse", unix))]
    fn tamper_lease_field(lane: &Path, field: &str, value: serde_json::Value) {
        let lease_path = stack_lease_path(lane.canonicalize().unwrap());
        let mut lease: serde_json::Value =
            serde_json::from_slice(&fs::read(&lease_path).unwrap()).unwrap();
        lease[field] = value;
        fs::write(&lease_path, serde_json::to_vec_pretty(&lease).unwrap()).unwrap();
    }

    /// A lease whose claim about the lane's base disagrees with what the base
    /// holds is refused at open, by field, with the recovery, through both
    /// readers of a lane's base (the CLI/hook store and the MCP engine's).
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a_lease_that_disagrees_with_its_base_is_refused_at_open_naming_the_field() {
        let root = temp_repo_dir("lease-claims");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        let base_sqlite = lease.base_sqlite_path();
        let honest = fs::read(stack_lease_path(lane.canonicalize().unwrap())).unwrap();
        StackedStore::open(&lane).expect("fixture: the honest lease opens");
        open_lane_base_store(&base_sqlite).expect("fixture: the honest lease opens for MCP");

        let forged_head = "0123456789abcdef0123456789abcdef01234567";
        let cases = [
            ("generation_id", serde_json::json!(lease.generation_id + 41)),
            ("head_sha", serde_json::json!(forged_head)),
            (
                "source_repo_root",
                serde_json::json!("/nonexistent/some/other/repo"),
            ),
            // The root the store is opened with (the lane's own) must not stand in for the stored row.
            (
                "source_repo_root",
                serde_json::json!(lane.canonicalize().unwrap().to_string_lossy()),
            ),
        ];
        // Every (case, reader) pair is tried and the failures reported together,
        // so one run shows which fields and readers are unguarded.
        let mut unguarded = Vec::new();
        for (field, forged) in cases {
            fs::write(stack_lease_path(lane.canonicalize().unwrap()), &honest).unwrap();
            tamper_lease_field(&lane, field, forged.clone());
            let refusals = [
                ("StackedStore::open", StackedStore::open(&lane).map(drop)),
                (
                    "open_lane_base_store",
                    open_lane_base_store(&base_sqlite).map(drop),
                ),
            ];
            for (reader, outcome) in refusals {
                match outcome {
                    Ok(()) => unguarded.push(format!("{reader} accepted lease {field} = {forged}")),
                    Err(e) => {
                        let text = format!("{e:#}");
                        if !text.contains(&format!("lease {field}")) {
                            unguarded.push(format!("{reader} did not name {field}: {text}"));
                        }
                        if !text.contains("devmap seed --stacked") {
                            unguarded.push(format!("{reader} did not name the recovery: {text}"));
                        }
                    }
                }
            }
        }
        assert!(
            unguarded.is_empty(),
            "lease claims that were not refused: {unguarded:#?}"
        );
        let _ = remove_readonly_tree(&root);
    }

    /// The donor may be moved or deleted after provision: the lane's base is its
    /// own clone, and the lease's root check must not need the donor's path.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a_lane_still_opens_after_its_donor_checkout_is_moved() {
        let root = temp_repo_dir("lease-donor-moved");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        provision_stacked_worktree(&anchor, &lane).unwrap();
        fs::rename(&anchor, root.join("anchor-moved")).unwrap();
        assert!(
            !anchor.exists(),
            "fixture: the donor is gone from its old path"
        );
        StackedStore::open(&lane).expect("a lane opens without its donor");
        let wt = lane.canonicalize().unwrap();
        let lease = StackLease::read_from(&stack_lease_path(&wt)).unwrap();
        open_lane_base_store(&lease.base_sqlite_path())
            .expect("the MCP reader opens without the donor");
        let _ = remove_readonly_tree(&root);
    }

    /// A base with no generation row holds nothing for the lease to claim: refused,
    /// not passed and not a panic. The lease's own stat identity is re-recorded so
    /// that only the claim check can refuse it.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a_base_with_no_generation_row_is_refused_at_open() {
        let root = temp_repo_dir("lease-empty-base");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        let sqlite = lease.base_sqlite_path();
        let empty = root.join("empty.sqlite");
        drop(Store::open(&empty).unwrap());
        set_file_writable(&sqlite).unwrap();
        fs::remove_file(&sqlite).unwrap();
        fs::copy(&empty, &sqlite).unwrap();
        set_file_readonly(&sqlite).unwrap();
        let file = File::open(&sqlite).unwrap();
        let record = file_record(&file, sha256_file_hex(&sqlite).unwrap()).unwrap();
        tamper_lease_field(&lane, "base", serde_json::to_value(&record).unwrap());
        let err = match StackedStore::open(&lane) {
            Err(e) => format!("{e:#}"),
            Ok(_) => panic!("a base with no generation row was opened"),
        };
        assert!(
            err.contains("lease generation_id is") && err.contains("the base holds None"),
            "the claim check must be what refuses an empty base: {err}"
        );
        assert!(err.contains("devmap seed --stacked"), "{err}");
        let _ = remove_readonly_tree(&root);
    }

    /// 6: a legacy manifest in the cache is replaced at once; provision does not
    /// refuse the same entry again and again.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a2_6_legacy_manifest_does_not_loop_on_the_same_refusal() {
        let root = temp_repo_dir("a2-6");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane_a = add_lane(&anchor, &root, "lane-a");
        let lane_b = add_lane(&anchor, &root, "lane-b");
        let lease = provision_stacked_worktree(&anchor, &lane_a).unwrap();
        let entry = published_dir(&anchor, &lease);
        let mp = entry.join("manifest.json");
        make_tree_writable(&entry);
        let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&mp).unwrap()).unwrap();
        // The v1 manifest's fields: unknown to this format, so it parses as legacy.
        v["base_sqlite_ctime_ns"] = serde_json::json!(1);
        v["format_version"] = serde_json::json!(1);
        fs::write(&mp, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
        set_file_readonly(&mp).unwrap();
        set_dir_mode(&entry, 0o555).unwrap();
        assert!(
            read_base_manifest(&entry).is_err(),
            "fixture: the manifest is legacy"
        );
        provision_stacked_worktree(&anchor, &lane_b)
            .expect("a legacy entry is replaced on the first provision, not refused");
        assert_eq!(
            read_base_manifest(&entry).unwrap().format_version,
            STACK_FORMAT_VERSION
        );
        let _ = remove_readonly_tree(&root);
    }

    /// 7: a worktree that cannot be verified (unmounted) is irrelevant to the
    /// replace path: its base is its own clone, which nothing else touches.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a2_7_replace_path_cannot_touch_an_unverifiable_lanes_base() {
        let root = temp_repo_dir("a2-7");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane1 = add_lane(&anchor, &root, "lane-1");
        let lane2 = add_lane(&anchor, &root, "lane-2");
        let lease = provision_stacked_worktree(&anchor, &lane1).unwrap();
        corrupt_last_byte(&published_dir(&anchor, &lease).join("base.sqlite"));
        // lane-1 becomes unverifiable (unmounted)
        let away = root.join("lane-1.away");
        fs::rename(&lane1, &away).unwrap();
        provision_stacked_worktree(&anchor, &lane2)
            .expect("the damaged entry is replaced; no lane stands on it");
        let common = registered_common_git_dir(&anchor.canonicalize().unwrap()).unwrap();
        gc_unused_bases(&common).expect("GC reads no lease");
        fs::rename(&away, &lane1).unwrap();
        let s = StackedStore::open(&lane1).expect("lane-1's own base is intact");
        assert!(!symbol_names(&s, "src/svc.go").is_empty());
        let _ = remove_readonly_tree(&root);
    }

    /// 4a: GC deletes every non-newest entry per donor with no lease scan, and a
    /// lane whose source entry is collected mid-lifecycle can still materialize
    /// the base it admitted.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a2_4a_gc_of_a_lanes_source_entry_does_not_break_its_materialize() {
        let root = temp_repo_dir("a2-4a");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let lane_new = add_lane(&anchor, &root, "lane-new");
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        let old_entry = published_dir(&anchor, &lease);
        let stacked = StackedStore::open(&lane).unwrap();
        let admitted_head = stacked.base_store().latest_generation_head_sha().unwrap();
        // a newer generation of the same donor
        fs::write(anchor.join("src/extra.rs"), "pub fn extra() {}\n").unwrap();
        run_git_ok(&anchor, &["add", "."]);
        run_git_ok(&anchor, &["commit", "-m", "second"]);
        index_tree(
            &anchor,
            &[
                "src/svc.go",
                "src/routes.ts",
                "src/limits.rs",
                "src/extra.rs",
            ],
        );
        let lease_new = provision_stacked_worktree(&anchor, &lane_new).unwrap();
        assert_ne!(published_dir(&anchor, &lease_new), old_entry);
        let common = registered_common_git_dir(&anchor.canonicalize().unwrap()).unwrap();
        let report = gc_unused_bases(&common).unwrap();
        assert_eq!(report.removed_generations, vec![1], "{report:?}");
        assert!(
            !old_entry.exists(),
            "no lease scan: the older entry is collected"
        );
        let path = stacked
            .materialize_seeded_store()
            .expect("the lane owns what it admitted");
        let promoted = Store::open_read_only(&path).unwrap();
        assert_eq!(
            promoted.latest_generation_head_sha().unwrap(),
            admitted_head
        );
        let _ = remove_readonly_tree(&root);
    }

    /// 4b: a store beside a lease means the lease is stale (a materialize was
    /// interrupted after placing the store). `open_if_leased` (a read) refuses
    /// and deletes nothing; `open_if_leased_for_sync` says "unstacked", retires the lease, the private
    /// base and the delta under the writer lock, and provision refuses a lane that has a store.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a2_4b_a_store_beside_a_stale_lease_is_refused_on_read_and_retired_by_sync() {
        let root = temp_repo_dir("a2-4b");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        fs::write(lane.join("src/svc.go"), "package svc\n\nfunc Edited() {}\n").unwrap();
        {
            let mut s = StackedStore::open(&lane).unwrap();
            s.sync_worktree().unwrap();
        }
        let wt = lane.canonicalize().unwrap();
        let lease_bytes = fs::read(stack_lease_path(&wt)).unwrap();
        let stacked = StackedStore::open(&lane).unwrap();
        let store = stacked.materialize_seeded_store().unwrap();
        drop(stacked);
        // The interrupted state: the store is placed, the lease is still there.
        fs::create_dir_all(lease.base_dir()).unwrap();
        fs::write(stack_lease_path(&wt), &lease_bytes).unwrap();
        fs::write(stack_delta_path(&wt), b"stale delta").unwrap();

        let err = provision_stacked_worktree(&anchor, &lane)
            .expect_err("provision refuses a lane that has a store")
            .to_string();
        assert!(err.contains("already has its own store"), "{err}");
        assert!(
            stack_lease_path(&wt).is_file(),
            "a refused provision changes nothing"
        );

        let read = format!(
            "{:#}",
            StackedStore::open_if_leased(&lane).expect_err("a read refuses")
        );
        assert!(read.contains("devmap seed --sync-delta"), "{read}");
        assert!(stack_lease_path(&wt).is_file(), "a read retires nothing");
        assert!(matches!(
            StackedStore::open_if_leased_for_sync(&lane),
            Ok(SyncTarget::RetiredStaleLease)
        ));
        assert!(
            !stack_lease_path(&wt).exists(),
            "the stale lease is retired"
        );
        assert!(!lease.base_dir().exists(), "and the private base");
        assert!(!stack_delta_path(&wt).exists(), "and the delta");
        assert!(store.is_file(), "the store stays");
        let _ = remove_readonly_tree(&root);
    }

    /// 4c: a placement failure leaves the lane exactly as the call found it:
    /// lease, private base and delta intact, no partial store, no tombstone.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a2_4c_materialize_placement_failure_changes_nothing() {
        let root = temp_repo_dir("a2-4c");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        provision_stacked_worktree(&anchor, &lane).unwrap();
        let stacked = StackedStore::open(&lane).unwrap();
        let wt = lane.canonicalize().unwrap();
        // placement fails: the target's parent holds a *directory* named like the
        // staged promote file, so the clone cannot create it.
        let target = store_path(&wt);
        let staged = sidecar_for_test(&target, &format!(".promote-{}", std::process::id()));
        fs::create_dir_all(&staged).unwrap();
        fs::write(staged.join("keep"), b"x").unwrap();
        let before_delta = stack_delta_path(&wt).exists();
        let r = stacked.materialize_seeded_store();
        eprintln!(
            "a2_4c: materialize -> {:?}",
            r.as_ref()
                .map(|p| p.display().to_string())
                .map_err(|e| format!("{e:#}"))
        );
        let err = format!(
            "{:#}",
            r.expect_err("materialize should fail with the staged path blocked")
        );
        assert!(
            err.contains("devmap seed --stacked --from"),
            "must name the recovery: {err}"
        );
        let lp = stack_lease_path(&wt);
        assert!(lp.is_file(), "lease removed");
        assert!(
            !lp.with_extension("json.materializing").exists(),
            "no tombstone is ever written"
        );
        assert!(
            stacked.lease().base_dir().join("base.sqlite").is_file(),
            "private base removed"
        );
        assert!(!target.exists(), "partial store left behind");
        assert_eq!(stack_delta_path(&wt).exists(), before_delta);
        let _ = fs::remove_dir_all(&staged);
        drop(stacked);
        StackedStore::open(&lane).expect("lane opens after the failed materialize");
        let _ = remove_readonly_tree(&root);
    }

    #[cfg(all(feature = "parse", unix))]
    fn sidecar_for_test(db: &Path, suffix: &str) -> PathBuf {
        let mut s = db.as_os_str().to_os_string();
        s.push(suffix);
        PathBuf::from(s)
    }

    /// 4d: the cache entry swapped (dir-level, same name) AFTER open, then
    /// materialize: the promoted writable store is a clone of the admitted file.
    #[cfg(all(feature = "parse", target_os = "macos"))]
    #[test]
    fn a2_4d_materialize_after_a_dir_swap_promotes_the_opened_base() {
        let root = temp_repo_dir("a2-4d");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let other = other_repo_with(
            &root,
            "other",
            "src/other_only.rs",
            "pub fn only_in_other() {}\n",
        );
        let _ = fs::remove_file(csr_path_for_store(store_path(&anchor)));
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        assert!(
            lease.csr.is_none(),
            "fixture: a donor without a CSR sidecar"
        );
        let entry = published_dir(&anchor, &lease);
        let stacked = StackedStore::open(&lane).unwrap();
        let imp_dir = a2_impostor_dir(&bases_dir_of(&anchor), &entry, &other);
        assert!(a2_swap(&entry, &imp_dir));
        let path = stacked
            .materialize_seeded_store()
            .expect("materialize clones the admitted file");
        let s = Store::open_read_only(&path).unwrap();
        let head = s.latest_generation_head_sha().unwrap();
        let honest_head = stacked.base_store().latest_generation_head_sha().unwrap();
        assert_eq!(head, honest_head, "materialized store holds the impostor's graph (head {head:?}) not the opened base's ({honest_head:?})");
        let _ = remove_readonly_tree(&root);
    }

    /// 8: every byte written on a read path (open + status + queries) is listed.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a2_8_read_path_writes_no_byte() {
        fn snap(dir: &Path, out: &mut Vec<String>) {
            if let Ok(rd) = fs::read_dir(dir) {
                let mut es: Vec<_> = rd.flatten().collect();
                es.sort_by_key(|e| e.file_name());
                for e in es {
                    let p = e.path();
                    let m = fs::symlink_metadata(&p).unwrap();
                    use std::os::unix::fs::MetadataExt;
                    out.push(format!(
                        "{} len={} mtime={}.{} ctime={}.{}",
                        p.display(),
                        m.len(),
                        m.mtime(),
                        m.mtime_nsec(),
                        m.ctime(),
                        m.ctime_nsec()
                    ));
                    if m.is_dir() {
                        snap(&p, out);
                    }
                }
            }
        }
        let root = temp_repo_dir("a2-8");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        provision_stacked_worktree(&anchor, &lane).unwrap();
        fs::write(lane.join("src/svc.go"), "package svc\n\nfunc Edited() {}\n").unwrap();
        {
            let mut s = StackedStore::open(&lane).unwrap();
            s.sync_worktree().unwrap();
        }
        let wt = lane.canonicalize().unwrap();
        let state = state_dir(&wt);
        let bases = bases_dir_of(&anchor);
        let mut before = Vec::new();
        snap(&state, &mut before);
        snap(&bases, &mut before);
        {
            let s = StackedStore::open(&lane).unwrap();
            let _ = s.status().unwrap();
            let _ = s.symbols_for_file("src/svc.go").unwrap();
            let _ = s.search_symbols("Helper", 5).unwrap();
            let mut mid = Vec::new();
            snap(&state, &mut mid);
            snap(&bases, &mut mid);
            let diff: Vec<_> = mid.iter().filter(|l| !before.contains(l)).collect();
            eprintln!("a2_8 (while open) changed/new: {diff:#?}");
            assert!(diff.is_empty(), "read path wrote while open: {diff:#?}");
        }
        let mut after = Vec::new();
        snap(&state, &mut after);
        snap(&bases, &mut after);
        let diff: Vec<_> = after.iter().filter(|l| !before.contains(l)).collect();
        eprintln!("a2_8 (after drop) changed/new: {diff:#?}");
        assert!(diff.is_empty(), "read path wrote: {diff:#?}");
        let _ = remove_readonly_tree(&root);
    }

    /// 8b: read-only open of a WAL delta in a read-only state dir.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a2_8b_open_in_a_read_only_state_dir_still_answers() {
        let root = temp_repo_dir("a2-8b");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        provision_stacked_worktree(&anchor, &lane).unwrap();
        fs::write(lane.join("src/svc.go"), "package svc\n\nfunc Edited() {}\n").unwrap();
        {
            let mut s = StackedStore::open(&lane).unwrap();
            s.sync_worktree().unwrap();
        }
        let wt = lane.canonicalize().unwrap();
        let cdir = stack_delta_path(&wt).parent().unwrap().to_path_buf();
        set_dir_mode(&cdir, 0o555).unwrap();
        let r = StackedStore::open(&lane);
        let dirty = r.as_ref().ok().map(|s| s.status().unwrap().dirty_files);
        set_dir_mode(&cdir, 0o755).unwrap();
        assert!(
            r.is_ok(),
            "read-only state dir: {:?}",
            r.err().map(|e| format!("{e:#}"))
        );
        assert_eq!(dirty, Some(1), "the delta must be read, not skipped");
        let _ = remove_readonly_tree(&root);
    }

    /// 5: a writer holds the delta exclusively while the hook's open+status run.
    /// CARDED (ft-983bd7bc, fix round after attack 3): the 5 s busy timeout of a
    /// read-only delta open against an exclusive-mode writer exceeds the hook's
    /// 400 ms budget. Out of scope for the stacked-base redesign.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    #[ignore = "carded ft-983bd7bc: exclusive delta lock vs the hook's 400 ms budget (5 s busy timeout)"]
    fn a2_5_open_and_status_stay_inside_the_hook_budget_while_a_writer_holds_the_delta() {
        let root = temp_repo_dir("a2-5");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        provision_stacked_worktree(&anchor, &lane).unwrap();
        fs::write(lane.join("src/svc.go"), "package svc\n\nfunc Edited() {}\n").unwrap();
        {
            let mut s = StackedStore::open(&lane).unwrap();
            s.sync_worktree().unwrap();
        }
        let wt = lane.canonicalize().unwrap();
        let conn = rusqlite::Connection::open(stack_delta_path(&wt)).unwrap();
        let _: String = conn
            .query_row("PRAGMA locking_mode=EXCLUSIVE", [], |r| r.get(0))
            .unwrap();
        conn.execute_batch("BEGIN EXCLUSIVE;").unwrap();
        let t = std::time::Instant::now();
        let r = StackedStore::open(&lane);
        let st = r.as_ref().ok().map(|s| {
            s.status()
                .map(|x| x.dirty_files)
                .map_err(|e| format!("{e:#}"))
        });
        let took = t.elapsed();
        eprintln!(
            "a2_5: open={:?} status={:?} took={took:?}",
            r.as_ref().map(|_| "ok").map_err(|e| format!("{e:#}")),
            st
        );
        conn.execute_batch("ROLLBACK;").unwrap();
        assert!(
            took < std::time::Duration::from_millis(400),
            "open+status blocked {took:?} against the 400 ms hook budget"
        );
        let _ = remove_readonly_tree(&root);
    }

    // ===================== the redesign's invariants =====================

    /// Open names nothing under `bases/`: delete the whole cache, replace its
    /// entry with a file, or corrupt it, and open and materialize still serve
    /// the bytes the lane admitted.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn open_and_materialize_serve_the_admitted_bytes_whatever_becomes_of_the_cache() {
        let root = temp_repo_dir("inv-open");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lanes: Vec<PathBuf> = ["a", "b", "c"]
            .iter()
            .map(|n| add_lane(&anchor, &root, n))
            .collect();
        let leases: Vec<StackLease> = lanes
            .iter()
            .map(|l| provision_stacked_worktree(&anchor, l).unwrap())
            .collect();
        let admitted_head = StackedStore::open(&lanes[0])
            .unwrap()
            .base_store()
            .latest_generation_head_sha()
            .unwrap();
        let entry = published_dir(&anchor, &leases[0]);

        // lane a: the entry's bytes are corrupted.
        corrupt_last_byte(&entry.join("base.sqlite"));
        // lane b: the entry becomes a plain file.
        // lane c: the whole cache disappears (checked last, after b).
        for lane in &lanes {
            let s = StackedStore::open(lane).expect("a corrupted entry is not read by open");
            assert_eq!(
                s.base_store().latest_generation_head_sha().unwrap(),
                admitted_head
            );
        }
        let a = StackedStore::open(&lanes[0]).unwrap();
        let path = a
            .materialize_seeded_store()
            .expect("materialize clones the admitted file");
        assert_eq!(
            Store::open_read_only(&path)
                .unwrap()
                .latest_generation_head_sha()
                .unwrap(),
            admitted_head
        );

        remove_readonly_tree(&entry).unwrap();
        fs::write(&entry, b"not a directory").unwrap();
        let b = StackedStore::open(&lanes[1]).expect("a file where the entry was changes nothing");
        assert_eq!(
            b.base_store().latest_generation_head_sha().unwrap(),
            admitted_head
        );
        let path = b.materialize_seeded_store().unwrap();
        assert_eq!(
            Store::open_read_only(&path)
                .unwrap()
                .latest_generation_head_sha()
                .unwrap(),
            admitted_head
        );

        remove_readonly_tree(&bases_dir_of(&anchor)).unwrap();
        let c = StackedStore::open(&lanes[2]).expect("no cache at all changes nothing");
        assert_eq!(
            c.base_store().latest_generation_head_sha().unwrap(),
            admitted_head
        );
        assert!(!symbol_names(&c, "src/svc.go").is_empty());
        let path = c.materialize_seeded_store().unwrap();
        assert_eq!(
            Store::open_read_only(&path)
                .unwrap()
                .latest_generation_head_sha()
                .unwrap(),
            admitted_head
        );
        let _ = remove_readonly_tree(&root);
    }

    /// Admission is through descriptors: an entry that is writable, carries a
    /// SQLite sidecar, or has a CSR its manifest does not record is refused.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn admission_refuses_a_writable_entry_a_sidecar_and_an_unrecorded_csr() {
        let root = temp_repo_dir("inv-admit");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let (entry, manifest) = publish_base_snapshot(&anchor).unwrap();
        let key = BaseKey::of_manifest(&manifest).unwrap();
        let stage = root.join("stage");
        let admit = || {
            let _ = fs::remove_dir_all(&stage);
            fs::create_dir_all(&stage).unwrap();
            admit_published_base(&entry, &key, &manifest, &stage)
                .map(|_| ())
                .map_err(|e| format!("{e:#}"))
        };
        admit().expect("an honest entry is admitted");
        let admitted = fs::metadata(stage.join("base.sqlite")).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                admitted.permissions().mode() & 0o777,
                0o444,
                "the clone is 0444"
            );
        }

        make_tree_writable(&entry);
        set_file_writable(&entry.join("base.sqlite")).unwrap();
        let err = admit().expect_err("a writable base.sqlite is refused");
        assert!(err.contains("writable"), "{err}");
        set_file_readonly(&entry.join("base.sqlite")).unwrap();

        fs::write(entry.join("base.sqlite-wal"), b"x").unwrap();
        let err = admit().expect_err("a sidecar is refused");
        assert!(err.contains("base.sqlite-wal"), "{err}");
        fs::remove_file(entry.join("base.sqlite-wal")).unwrap();

        let csr = entry.join("base.sqlite.csr");
        if csr.exists() {
            fs::remove_file(&csr).unwrap();
            let err =
                admit().expect_err("a CSR the manifest records but the entry lacks is refused");
            assert!(err.contains("csr"), "{err}");
        }
        let _ = remove_readonly_tree(&root);
    }

    /// A v1 lease named a shared base that lanes no longer read: it is refused
    /// with the exact recovery command, by `open`, `read_from` and
    /// `open_if_leased`, and re-provisioning replaces it.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a_v1_lease_is_refused_with_the_recovery_command_and_reprovision_replaces_it() {
        let root = temp_repo_dir("inv-v1");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let wt = lane.canonicalize().unwrap();
        provision_stacked_worktree(&anchor, &lane).unwrap();
        let lease_path = stack_lease_path(&wt);
        let v1 = serde_json::json!({
            "format_version": 1,
            "generation_id": 1,
            "base_dir": "/somewhere/bases/gen-1-aaaaaaaaaaaa-bbbbbbbb",
            "worktree_root": wt,
            "leased_at_unix_ms": 1,
            "rebases_after_dirty_files": 250,
            "rebases_after_delta_bytes": 16777216,
        });
        fs::write(&lease_path, serde_json::to_vec_pretty(&v1).unwrap()).unwrap();
        for err in [
            format!(
                "{:#}",
                StackedStore::open(&lane).expect_err("v1 must be refused")
            ),
            format!(
                "{:#}",
                StackLease::read_from(&lease_path).expect_err("v1 must be refused")
            ),
            format!(
                "{:#}",
                StackedStore::open_if_leased(&lane).expect_err("v1 must be refused")
            ),
        ] {
            assert!(err.contains("format 1"), "{err}");
            assert!(
                err.contains("devmap seed --stacked --from <donor checkout>")
                    && err.contains(&lease_path.display().to_string()),
                "the refusal must name the exact recovery command: {err}"
            );
        }
        provision_stacked_worktree(&anchor, &lane).expect("re-provisioning replaces a v1 lease");
        let s = StackedStore::open(&lane).unwrap();
        assert_eq!(s.lease().format_version, STACK_FORMAT_VERSION);
        assert_eq!(STACK_FORMAT_VERSION, 2);
        let _ = remove_readonly_tree(&root);
    }

    /// Every error that can strand a lane names its recovery: a missing private
    /// base, a lease for another lane and a corrupt lease.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn errors_that_strand_a_lane_name_the_recovery_command() {
        let root = temp_repo_dir("inv-hint");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        let wt = lane.canonicalize().unwrap();
        let hint = "devmap seed --stacked --from <donor checkout>";

        // A private base that became writable strands the lane too.
        set_file_writable(&lease.base_sqlite_path()).unwrap();
        let err = format!(
            "{:#}",
            StackedStore::open(&lane).expect_err("a writable base")
        );
        assert!(err.contains("writable") && err.contains(hint), "{err}");
        set_file_readonly(&lease.base_sqlite_path()).unwrap();

        fs::remove_file(lease.base_sqlite_path()).unwrap();
        let err = format!(
            "{:#}",
            StackedStore::open(&lane).expect_err("a missing base")
        );
        assert!(err.contains(hint), "{err}");
        fs::remove_dir_all(lease.base_dir()).unwrap();
        let err = format!(
            "{:#}",
            StackedStore::open(&lane).expect_err("a missing stack dir")
        );
        assert!(err.contains(hint), "{err}");

        fs::write(stack_lease_path(&wt), b"{corrupt").unwrap();
        let err = format!(
            "{:#}",
            StackedStore::open(&lane).expect_err("a corrupt lease")
        );
        assert!(err.contains(hint), "{err}");

        provision_stacked_worktree(&anchor, &lane).expect("the named command recovers the lane");
        StackedStore::open(&lane).unwrap();
        let _ = remove_readonly_tree(&root);
    }

    /// The MCP engine opens the lane's base through the same lease-checked path
    /// as `StackedStore::open`: an honest base opens, a swapped one is refused.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn the_mcp_engines_base_open_checks_the_lease_identity() {
        let root = temp_repo_dir("inv-mcp");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        let base = lease.base_sqlite_path();
        let store = open_lane_base_store(&base).expect("an honest lane base opens");
        assert_eq!(
            store.latest_generation_head_sha().unwrap().as_deref(),
            Some(lease.head_sha.as_str())
        );
        drop(store);
        // A same-content file under another inode is not the admitted file.
        let impostor = lease.base_dir().join("impostor.sqlite");
        fs::copy(&base, &impostor).unwrap();
        set_file_readonly(&impostor).unwrap();
        fs::rename(&impostor, &base).unwrap();
        let Err(err) = open_lane_base_store(&base) else {
            panic!("a swapped base is refused")
        };
        let err = format!("{err:#}");
        assert!(err.contains("replaced or modified"), "{err}");
        // A path that is not a lane's stack/base.sqlite has no lease to check.
        assert!(open_lane_base_store(&root.join("nowhere/base.sqlite")).is_err());
        let _ = remove_readonly_tree(&root);
    }

    /// The lease records what was admitted: sha256 of the clone, its identity,
    /// the donor's generation, `HEAD` and root; and the clone is 0444.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn the_lease_records_what_was_admitted() {
        let root = temp_repo_dir("inv-lease");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        assert_eq!(lease.head_sha, git_head(&anchor));
        assert_eq!(
            lease.source_repo_root,
            anchor.canonicalize().unwrap().to_string_lossy()
        );
        assert_eq!(
            lease.base.sha256,
            sha256_file_hex(&lease.base_sqlite_path()).unwrap()
        );
        assert_eq!(
            lease.base.len,
            fs::metadata(lease.base_sqlite_path()).unwrap().len()
        );
        let csr = lease
            .csr
            .as_ref()
            .expect("the fixture donor has a CSR sidecar");
        assert_eq!(
            csr.sha256,
            sha256_file_hex(&csr_path_for_store(lease.base_sqlite_path())).unwrap()
        );
        let on_disk =
            StackLease::read_from(&stack_lease_path(lane.canonicalize().unwrap())).unwrap();
        assert_eq!(on_disk, lease);
        let _ = remove_readonly_tree(&root);
    }

    /// FIX-C: the production writer (`DeltaStore`, rollback-journal mode) commits
    /// in a loop while a reader opens and asks status, as the hook does. A reader
    /// is blocked only while a commit holds PENDING/EXCLUSIVE, so the worst case
    /// over many opens is measured against the hook's 400 ms read budget.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a2_5b_a_committing_writer_in_rollback_mode_stays_inside_the_hook_read_budget() {
        let root = temp_repo_dir("a2-5b");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        provision_stacked_worktree(&anchor, &lane).unwrap();
        fs::write(lane.join("src/svc.go"), "package svc\n\nfunc Edited() {}\n").unwrap();
        {
            let mut s = StackedStore::open(&lane).unwrap();
            s.sync_worktree().unwrap();
        }
        let wt = lane.canonicalize().unwrap();
        let delta_path = stack_delta_path(&wt);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let delta = DeltaStore::open(&delta_path).unwrap();
                let mut commits = 0u64;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    delta
                        .tombstone_file(
                            &format!("src/w_{}.rs", commits % 64),
                            "CAS_TOMBSTONE",
                            commits,
                        )
                        .unwrap();
                    commits += 1;
                }
                commits
            })
        };
        let mut worst = std::time::Duration::ZERO;
        let began = std::time::Instant::now();
        let mut opens = 0u32;
        while began.elapsed() < std::time::Duration::from_secs(3) {
            let t = std::time::Instant::now();
            let s = StackedStore::open(&lane).expect("a reader opens beside a committing writer");
            s.status().expect("status beside a committing writer");
            worst = worst.max(t.elapsed());
            opens += 1;
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let commits = writer.join().unwrap();
        eprintln!("a2_5b: {opens} opens against {commits} commits, worst open+status {worst:?}");
        assert!(commits > 10, "fixture: the writer committed ({commits})");
        assert!(
            worst < std::time::Duration::from_millis(400),
            "open+status took {worst:?} beside a committing writer (hook read budget 400 ms)"
        );
        let _ = remove_readonly_tree(&root);
    }

    /// FIX-A at the lane: an interrupted delta writer's hot journal makes every
    /// read refuse with the cause and `devmap seed --sync-delta`, and that
    /// command (a writable open first) recovers the lane.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a_hot_delta_journal_refuses_reads_and_sync_delta_recovers_the_lane() {
        let root = temp_repo_dir("hotjournal");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        provision_stacked_worktree(&anchor, &lane).unwrap();
        fs::write(lane.join("src/svc.go"), "package svc\n\nfunc Edited() {}\n").unwrap();
        {
            let mut s = StackedStore::open(&lane).unwrap();
            s.sync_worktree().unwrap();
        }
        let wt = lane.canonicalize().unwrap();
        let delta = stack_delta_path(&wt);
        // A writer killed mid-commit: copy the db and its journal while a
        // spilling transaction is open, then put the copies in place.
        let live = root.join("live.sqlite");
        fs::copy(&delta, &live).unwrap();
        let mut conn = rusqlite::Connection::open(&live).unwrap();
        conn.execute_batch("PRAGMA cache_size = 1;").unwrap();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        for i in 0..400 {
            tx.execute(
                "INSERT INTO delta_files (path, content_hash, tombstoned, extraction_json, updated_at_ms)
                 VALUES (?1, 'x', 1, ?2, 1)",
                rusqlite::params![format!("src/spill_{i}.rs"), "y".repeat(2000)],
            )
            .unwrap();
        }
        fs::copy(&live, &delta).unwrap();
        fs::copy(
            root.join("live.sqlite-journal"),
            delta.with_file_name("delta.sqlite-journal"),
        )
        .unwrap();
        drop(tx);
        drop(conn);

        let err = format!(
            "{:#}",
            StackedStore::open(&lane).expect_err("a hot journal refuses reads")
        );
        assert!(
            err.contains("hot rollback journal") && err.contains("devmap seed --sync-delta"),
            "{err}"
        );
        let SyncTarget::Lane(mut s) =
            StackedStore::open_if_leased_for_sync(&lane).expect("sync --sync-delta opens the lane")
        else {
            panic!("the lane is leased");
        };
        s.sync_worktree().unwrap();
        drop(s);
        StackedStore::open(&lane).expect("the lane reads again after the sync");
        let _ = remove_readonly_tree(&root);
    }

    // ===================== attack round 3 =====================

    #[cfg(all(feature = "parse", unix))]
    fn a3_lane(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = temp_repo_dir(tag);
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        provision_stacked_worktree(&anchor, &lane).unwrap();
        (root, anchor, lane)
    }

    #[cfg(all(feature = "parse", unix))]
    fn a3_sync(lane: &Path) {
        let mut s = StackedStore::open(lane).unwrap();
        s.sync_worktree().unwrap();
    }

    /// A delta db plus a hot journal copied while a spilling transaction was
    /// open (a writer killed mid-commit), written to `dir/{delta.sqlite,delta.sqlite-journal}`.
    #[cfg(all(feature = "parse", unix))]
    fn a3_hot_delta(dir: &Path, tag: &str, committed_rows: usize) -> (PathBuf, PathBuf) {
        let src = dir.join(format!("src-{tag}"));
        fs::create_dir_all(&src).unwrap();
        {
            let w = DeltaStore::open(src.join("delta.sqlite")).unwrap();
            for i in 0..committed_rows {
                w.tombstone_file(&format!("src/{tag}_{i}.rs"), "CAS_TOMBSTONE", 1)
                    .unwrap();
            }
        }
        let mut conn = rusqlite::Connection::open(src.join("delta.sqlite")).unwrap();
        conn.execute_batch("PRAGMA cache_size = 1;").unwrap();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        for i in 0..400 {
            tx.execute(
                "INSERT INTO delta_files (path, content_hash, tombstoned, extraction_json, updated_at_ms)
                 VALUES (?1, 'x', 1, ?2, 1)",
                rusqlite::params![format!("src/spill_{tag}_{i}.rs"), "y".repeat(2000)],
            )
            .unwrap();
        }
        let db = dir.join(format!("{tag}.sqlite"));
        let jr = dir.join(format!("{tag}.sqlite-journal"));
        fs::copy(src.join("delta.sqlite"), &db).unwrap();
        fs::copy(src.join("delta.sqlite-journal"), &jr).unwrap();
        drop(tx);
        drop(conn);
        (db, jr)
    }

    /// Dirty-file count a lane reports, or the refusal.
    #[cfg(all(feature = "parse", unix))]
    fn a3_dirty(lane: &Path) -> Result<usize, String> {
        StackedStore::open(lane)
            .and_then(|s| s.status())
            .map(|st| st.dirty_files)
            .map_err(|e| format!("{e:#}"))
    }

    /// A committed rename: `git diff --name-only` (rename detection on) lists
    /// only the new path, so the old path never reaches the delta and the base
    /// keeps serving the deleted file's symbols.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a3_committed_rename_does_not_leave_the_old_path_in_the_answer() {
        let (root, _anchor, lane) = a3_lane("a3-rename");
        run_git_ok(&lane, &["mv", "src/svc.go", "src/svc2.go"]);
        run_git_ok(&lane, &["commit", "-m", "rename"]);
        a3_sync(&lane);
        let s = StackedStore::open(&lane).unwrap();
        assert!(
            !symbol_names(&s, "src/svc2.go").is_empty(),
            "the new path is indexed"
        );
        let old = symbol_names(&s, "src/svc.go");
        assert!(
            old.is_empty(),
            "src/svc.go no longer exists but the answer still lists {old:?}"
        );
        let _ = remove_readonly_tree(&root);
    }

    /// A second edit of a path the delta already holds is not reported as
    /// unsynced, and the delta serves the first edit's symbols.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a3_a_second_edit_of_a_synced_path_is_reported_as_unsynced() {
        let (root, _anchor, lane) = a3_lane("a3-second-edit");
        fs::write(
            lane.join("src/svc.go"),
            "package svc\n\nfunc EditedOne() {}\n",
        )
        .unwrap();
        a3_sync(&lane);
        fs::write(
            lane.join("src/svc.go"),
            "package svc\n\nfunc EditedTwo() {}\n",
        )
        .unwrap();
        let s = StackedStore::open(&lane).unwrap();
        let names = symbol_names(&s, "src/svc.go");
        let unsynced = s.unsynced_paths().unwrap();
        assert!(
            names.contains(&"EditedTwo".to_string()) || unsynced.contains("src/svc.go"),
            "serves {names:?} for a file whose content is EditedTwo, and unsynced_paths={unsynced:?} is silent"
        );
        let _ = remove_readonly_tree(&root);
    }

    /// A revert after a sync (git checkout) leaves the delta serving the edit.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a3_a_revert_after_a_sync_is_reported_or_answered() {
        let (root, _anchor, lane) = a3_lane("a3-revert");
        fs::write(
            lane.join("src/svc.go"),
            "package svc\n\nfunc EditedOne() {}\n",
        )
        .unwrap();
        a3_sync(&lane);
        run_git_ok(&lane, &["checkout", "--", "src/svc.go"]);
        let s = StackedStore::open(&lane).unwrap();
        let names = symbol_names(&s, "src/svc.go");
        let unsynced = s.unsynced_paths().unwrap();
        assert!(
            !names.contains(&"EditedOne".to_string()) || unsynced.contains("src/svc.go"),
            "serves the reverted edit {names:?}; unsynced_paths={unsynced:?} is silent"
        );
        let _ = remove_readonly_tree(&root);
    }

    /// A new untracked file that was synced and then removed: git no longer
    /// reports it, so the delta row is stale until the next sync clears it.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a3_a_synced_file_removed_afterwards_is_unsynced_then_cleared() {
        let (root, _anchor, lane) = a3_lane("a3-ghost");
        fs::write(
            lane.join("src/ghost.go"),
            "package svc\n\nfunc Ghost() {}\n",
        )
        .unwrap();
        a3_sync(&lane);
        fs::remove_file(lane.join("src/ghost.go")).unwrap();
        let mut s = StackedStore::open(&lane).unwrap();
        assert!(
            s.unsynced_paths().unwrap().contains("src/ghost.go"),
            "the stale row is reported"
        );
        s.sync_worktree().unwrap();
        let s = StackedStore::open(&lane).unwrap();
        assert!(
            symbol_names(&s, "src/ghost.go").is_empty(),
            "the removed file's symbols are served"
        );
        assert!(s.unsynced_paths().unwrap().is_empty());
        let _ = remove_readonly_tree(&root);
    }

    /// A hot journal that belongs to a different database: the recovery the
    /// refusal prints replays foreign pages into this lane's delta.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    #[ignore = "carded ft-983bd7bc: a foreign hot journal planted in the lane's own state dir is outside the stated threat model (lead ruling, fix round 5)"]
    fn a3_a_journal_for_a_different_db_is_not_replayed_into_the_lanes_delta() {
        let (root, _anchor, lane) = a3_lane("a3-foreign-journal");
        fs::write(lane.join("src/svc.go"), "package svc\n\nfunc Edited() {}\n").unwrap();
        a3_sync(&lane);
        let wt = lane.canonicalize().unwrap();
        let before = a3_dirty(&lane).unwrap();
        let (_foreign_db, foreign_journal) = a3_hot_delta(&root, "foreign", 7);
        fs::copy(
            &foreign_journal,
            stack_delta_path(&wt).with_file_name("delta.sqlite-journal"),
        )
        .unwrap();
        let read = a3_dirty(&lane);
        eprintln!("a3 foreign journal: read before recovery -> {read:?}");
        let recovered = StackedStore::open_if_leased_for_sync(&lane)
            .map(|o| matches!(o, SyncTarget::Lane(_)))
            .map_err(|e| format!("{e:#}"));
        eprintln!("a3 foreign journal: recovery -> {recovered:?}");
        let after = a3_dirty(&lane);
        eprintln!("a3 foreign journal: before={before} after={after:?}");
        assert!(
            after == Ok(before) || after.is_err(),
            "the lane's delta was replaced by a foreign db's rows: {after:?} (was {before})"
        );
        let _ = remove_readonly_tree(&root);
    }

    /// A truncated hot journal, at several cut points.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a3_a_truncated_hot_journal_is_refused_or_recovers_to_the_committed_state() {
        for cut in [1u64, 12, 28, 512, 4096, u64::MAX] {
            let (root, _anchor, lane) = a3_lane(&format!("a3-trunc-{}", cut.min(99999)));
            fs::write(lane.join("src/svc.go"), "package svc\n\nfunc Edited() {}\n").unwrap();
            a3_sync(&lane);
            let wt = lane.canonicalize().unwrap();
            let delta = stack_delta_path(&wt);
            let (db, jr) = a3_hot_delta(&root, "hot", 1);
            fs::copy(&db, &delta).unwrap();
            let len = fs::metadata(&jr).unwrap().len();
            let keep = if cut == u64::MAX {
                len / 2
            } else {
                cut.min(len)
            };
            let bytes = fs::read(&jr).unwrap();
            fs::write(
                delta.with_file_name("delta.sqlite-journal"),
                &bytes[..keep as usize],
            )
            .unwrap();
            let read = a3_dirty(&lane);
            let recovered = StackedStore::open_if_leased_for_sync(&lane)
                .map(|o| matches!(o, SyncTarget::Lane(_)))
                .map_err(|e| format!("{e:#}"));
            let after = a3_dirty(&lane);
            eprintln!("a3 truncated cut={cut} keep={keep}/{len}: read={read:?} recovery={recovered:?} after={after:?}");
            // the committed delta had exactly the rows a3_hot_delta wrote (1)
            assert!(
                read.is_err() || read == Ok(1),
                "cut {cut}: read answered {read:?}"
            );
            assert!(
                after == Ok(1),
                "cut {cut}: after recovery the lane reports {after:?}, not the committed 1"
            );
            let _ = remove_readonly_tree(&root);
        }
    }

    /// A journal that is a symlink to a hot journal elsewhere.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a3_a_symlinked_journal_is_neither_followed_nor_modified() {
        let (root, _anchor, lane) = a3_lane("a3-symlink-journal");
        fs::write(lane.join("src/svc.go"), "package svc\n\nfunc Edited() {}\n").unwrap();
        a3_sync(&lane);
        let wt = lane.canonicalize().unwrap();
        let before = a3_dirty(&lane).unwrap();
        let (_db, foreign) = a3_hot_delta(&root, "outside", 5);
        let outside = root.join("outside-journal");
        fs::copy(&foreign, &outside).unwrap();
        let outside_bytes = fs::read(&outside).unwrap();
        let link = stack_delta_path(&wt).with_file_name("delta.sqlite-journal");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let read = a3_dirty(&lane);
        let recovered = StackedStore::open_if_leased_for_sync(&lane)
            .map(|o| matches!(o, SyncTarget::Lane(_)))
            .map_err(|e| format!("{e:#}"));
        let after = a3_dirty(&lane);
        eprintln!("a3 symlink journal: read={read:?} recovery={recovered:?} after={after:?} before={before}");
        assert_eq!(
            fs::read(&outside).ok(),
            Some(outside_bytes),
            "the file outside the lane was modified or removed"
        );
        assert!(
            after == Ok(before) || after.is_err(),
            "foreign journal replayed through a symlink: {after:?} (was {before})"
        );
        let _ = remove_readonly_tree(&root);
    }

    /// `retire_stack` (the sync's stale-lease retirement and a materialize)
    /// discards the delta but leaves its `-journal`.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a3_retiring_a_lane_leaves_no_journal_behind() {
        let (root, _anchor, lane) = a3_lane("a3-orphan-journal");
        fs::write(lane.join("src/svc.go"), "package svc\n\nfunc Edited() {}\n").unwrap();
        a3_sync(&lane);
        let wt = lane.canonicalize().unwrap();
        let delta = stack_delta_path(&wt);
        let journal = delta.with_file_name("delta.sqlite-journal");
        fs::write(&journal, b"\x01").unwrap();
        retire_stack(&wt).unwrap();
        assert!(!delta.exists());
        assert!(!journal.exists(), "an orphan delta.sqlite-journal survives retire_stack (hygiene only: a3_an_orphan_journal_is_not_replayed_into_a_recreated_delta shows SQLite discards it)");
        let _ = remove_readonly_tree(&root);
    }

    #[cfg(all(feature = "parse", unix))]
    fn a3_store_beside_lease() -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let (root, _anchor, lane) = a3_lane("a3-recover-store");
        let wt = lane.canonicalize().unwrap();
        let lease_bytes = fs::read(stack_lease_path(&wt)).unwrap();
        let base_dir = StackLease::read_from(&stack_lease_path(&wt))
            .unwrap()
            .base_dir();
        let stacked = StackedStore::open(&lane).unwrap();
        let store = stacked.materialize_seeded_store().unwrap();
        drop(stacked);
        fs::create_dir_all(&base_dir).unwrap();
        fs::write(stack_lease_path(&wt), &lease_bytes).unwrap();
        (root, wt, lane, store)
    }

    #[cfg(all(feature = "parse", unix))]
    fn a3_run_sync_delta(wt: &Path) -> (Option<i32>, String) {
        let bin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/debug/devmap");
        assert!(
            bin.is_file(),
            "build the devmap binary first: {}",
            bin.display()
        );
        let out = Command::new(&bin)
            .arg("--root")
            .arg(wt)
            .args(["seed", "--sync-delta"])
            .output()
            .unwrap();
        (
            out.status.code(),
            format!(
                "{} / {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    /// The recovery a read prints for "a store beside a stale lease" must
    /// retire the lease and exit 0.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a3_the_printed_recovery_for_a_store_beside_a_lease_recovers() {
        let (root, wt, lane, _store) = a3_store_beside_lease();
        let read_err = format!(
            "{:#}",
            StackedStore::open_if_leased(&lane).expect_err("a read refuses")
        );
        assert!(read_err.contains("devmap seed --sync-delta"));
        let (code, msg) = a3_run_sync_delta(&wt);
        eprintln!(
            "a3 sync-delta unlocked: status={code:?} lease_gone={} msg={msg}",
            !stack_lease_path(&wt).exists()
        );
        assert!(
            !stack_lease_path(&wt).exists(),
            "the lease was not retired: {msg}"
        );
        assert_eq!(
            code,
            Some(0),
            "the recovery the refusal names exits {code:?}: {msg}"
        );
        let _ = remove_readonly_tree(&root);
    }

    /// While another writer holds the lane's writer lock the lease cannot be
    /// retired; the command must say so, not claim there is no lease.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a3_the_recovery_does_not_claim_there_is_no_lease_while_it_is_locked_out() {
        let (root, wt, _lane, store) = a3_store_beside_lease();
        let held = Store::lock_writer_at(&store, std::time::Duration::from_secs(1)).unwrap();
        let (code, msg) = a3_run_sync_delta(&wt);
        let lease_still = stack_lease_path(&wt).is_file();
        drop(held);
        eprintln!(
            "a3 sync-delta while locked: status={code:?} lease_still={lease_still} msg={msg}"
        );
        assert!(
            !(lease_still && msg.contains("no stack lease")),
            "told 'no stack lease ... nothing to sync' while the lease is still there: {msg}"
        );
        let _ = remove_readonly_tree(&root);
    }

    /// A journal that is a symlink: the read refuses naming `devmap seed
    /// --sync-delta`; that command must then recover the lane.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a3_a_symlinked_journal_is_recoverable_by_the_printed_command() {
        let (root, _anchor, lane) = a3_lane("a3-symlink-recover");
        fs::write(lane.join("src/svc.go"), "package svc\n\nfunc Edited() {}\n").unwrap();
        a3_sync(&lane);
        let wt = lane.canonicalize().unwrap();
        let (_db, foreign) = a3_hot_delta(&root, "outside", 5);
        let outside = root.join("outside-journal");
        fs::copy(&foreign, &outside).unwrap();
        let link = stack_delta_path(&wt).with_file_name("delta.sqlite-journal");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let read = a3_dirty(&lane).expect_err("a hot journal refuses reads");
        assert!(read.contains("devmap seed --sync-delta"), "{read}");
        let (code, msg) = a3_run_sync_delta(&wt);
        let after = a3_dirty(&lane);
        eprintln!("a3 symlink recovery: status={code:?} after={after:?} msg={msg}");
        assert_eq!(code, Some(0), "the printed recovery failed: {msg}");
        assert!(
            after.is_ok(),
            "the lane is still stranded after the printed recovery: {after:?}"
        );
        let _ = remove_readonly_tree(&root);
    }

    /// `seed --sync-delta` in a child process (rollback-journal commits) while
    /// this process opens the lane in a loop, as the hook and MCP do.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a3_sync_delta_racing_a_reader_never_refuses_or_serves_a_torn_delta() {
        let (root, _anchor, lane) = a3_lane("a3-race");
        let wt = lane.canonicalize().unwrap();
        let bin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/debug/devmap");
        assert!(bin.is_file(), "build the devmap binary first");
        for i in 0..4 {
            fs::write(
                wt.join(format!("src/extra{i}.go")),
                format!("package svc\n\nfunc Extra{i}() {{}}\n"),
            )
            .unwrap();
        }
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = {
            let (stop, wt, bin) = (Arc::clone(&stop), wt.clone(), bin.clone());
            std::thread::spawn(move || {
                let mut fails = Vec::new();
                for round in 0..12 {
                    fs::write(
                        wt.join("src/svc.go"),
                        format!("package svc\n\nfunc Round{round}() {{}}\n"),
                    )
                    .unwrap();
                    let out = Command::new(&bin)
                        .arg("--root")
                        .arg(&wt)
                        .args(["seed", "--sync-delta"])
                        .output()
                        .unwrap();
                    if !out.status.success() {
                        fails.push(String::from_utf8_lossy(&out.stderr).into_owned());
                    }
                }
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
                fails
            })
        };
        let mut errs: Vec<String> = Vec::new();
        let mut reads = 0u32;
        let mut worst = std::time::Duration::ZERO;
        let mut last_dirty = 0usize;
        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
            let t = std::time::Instant::now();
            match StackedStore::open(&lane) {
                Ok(s) => {
                    last_dirty = s.status().map(|x| x.dirty_files).unwrap_or(usize::MAX);
                }
                Err(e) => errs.push(format!("{e:#}")),
            }
            worst = worst.max(t.elapsed());
            reads += 1;
        }
        let fails = writer.join().unwrap();
        eprintln!("a3 race: reads={reads} errors={} writer_failures={} worst={worst:?} last_dirty={last_dirty}", errs.len(), fails.len());
        assert!(fails.is_empty(), "sync-delta failed: {fails:?}");
        assert!(
            errs.is_empty(),
            "a reader beside a live sync-delta was refused: {:?}",
            errs.first()
        );
        assert_eq!(a3_dirty(&lane), Ok(5));
        let _ = remove_readonly_tree(&root);
    }

    /// FIX-3 area: a WAL-mode delta from an older build, with `-wal`/`-shm`
    /// leftovers, in a writable and a read-only state directory. A read may
    /// refuse; it may never answer with fewer rows than were committed.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a3_wal_leftovers_never_read_as_fewer_rows_than_were_committed() {
        for (keep_shm, keep_wal, ro) in [
            (true, true, true),
            (false, true, true),
            (true, true, false),
            (false, true, false),
            (true, false, true),
            (true, false, false),
        ] {
            let (root, _anchor, lane) = a3_lane(&format!("a3-wal-{keep_shm}-{keep_wal}-{ro}"));
            let wt = lane.canonicalize().unwrap();
            let delta = stack_delta_path(&wt);
            // an older build's WAL delta: 3 committed rows, all still in the -wal
            let src = root.join("walsrc");
            fs::create_dir_all(&src).unwrap();
            {
                drop(DeltaStore::open(src.join("delta.sqlite")).unwrap());
            }
            let conn = rusqlite::Connection::open(src.join("delta.sqlite")).unwrap();
            let _: String = conn
                .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
                .unwrap();
            conn.execute_batch("PRAGMA wal_autocheckpoint = 0;")
                .unwrap();
            for i in 0..3 {
                conn.execute(
                    "INSERT INTO delta_files (path, content_hash, tombstoned, extraction_json, updated_at_ms) VALUES (?1, 'x', 1, NULL, 1)",
                    rusqlite::params![format!("src/w{i}.rs")],
                ).unwrap();
            }
            let _ = fs::remove_file(&delta);
            fs::copy(src.join("delta.sqlite"), &delta).unwrap();
            if keep_wal {
                fs::copy(
                    src.join("delta.sqlite-wal"),
                    sidecar_for_test(&delta, "-wal"),
                )
                .unwrap();
            }
            if keep_shm {
                fs::copy(
                    src.join("delta.sqlite-shm"),
                    sidecar_for_test(&delta, "-shm"),
                )
                .unwrap();
            }
            drop(conn);
            let cdir = delta.parent().unwrap().to_path_buf();
            if ro {
                set_dir_mode(&cdir, 0o555).unwrap();
            }
            let got = a3_dirty(&lane);
            if ro {
                set_dir_mode(&cdir, 0o755).unwrap();
            }
            eprintln!("a3 wal leftovers shm={keep_shm} wal={keep_wal} ro={ro}: {got:?}");
            if let Ok(n) = got {
                assert!(keep_wal || n == 0 || n == 3, "unexpected");
                assert!(!keep_wal || n == 3, "shm={keep_shm} wal={keep_wal} ro={ro}: answered {n} dirty files; 3 were committed");
            }
            let _ = remove_readonly_tree(&root);
        }
    }

    /// The fd a store is bound to is recycled by other opens: the store still
    /// reads the lane's own base.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a3_fd_reuse_after_open_lane_base_store_does_not_redirect_the_store() {
        let (root, _anchor, lane) = a3_lane("a3-fd");
        let wt = lane.canonicalize().unwrap();
        let lease = StackLease::read_from(&stack_lease_path(&wt)).unwrap();
        let store = open_lane_base_store(&lease.base_sqlite_path()).unwrap();
        let head = store.latest_generation_head_sha().unwrap();
        let other = other_repo_with(
            &root,
            "other",
            "src/other_only.rs",
            "pub fn only_in_other() {}\n",
        );
        let mut held = Vec::new();
        for _ in 0..64 {
            held.push(fs::File::open(store_path(&other)).unwrap());
            held.push(fs::File::open(store_path(&other)).unwrap());
            drop(held.pop());
        }
        assert_eq!(store.latest_generation_head_sha().unwrap(), head);
        assert!(store.latest_path_is_indexed("src/svc.go").unwrap());
        assert!(!store.latest_path_is_indexed("src/other_only.rs").unwrap());
        drop(held);
        let _ = remove_readonly_tree(&root);
    }

    /// The reuse path trusts a cache entry whose manifest and bytes agree with
    /// each other. A donor deleted and rebuilt at the same HEAD restarts at the
    /// same generation id, so id, HEAD and root alone match the old entry: the
    /// new lane must still be admitted from the rebuilt store, told apart by
    /// the store's creation epoch.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a3_a_rebuilt_donor_with_other_content_at_the_same_key_is_not_served_from_the_old_entry() {
        let root = temp_repo_dir("a3-rebuilt");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane1 = add_lane(&anchor, &root, "lane-1");
        let lane2 = add_lane(&anchor, &root, "lane-2");
        let lease1 = provision_stacked_worktree(&anchor, &lane1).unwrap();
        let old_epoch = donor_epoch(&anchor);
        // The donor is deleted and rebuilt over the same files at the same HEAD.
        let db = store_path(&anchor);
        discard_staged(&db);
        let _ = fs::remove_file(csr_path_for_store(&db));
        index_tree(&anchor, &["src/svc.go", "src/routes.ts", "src/limits.rs"]);
        let new_epoch = donor_epoch(&anchor);
        assert_ne!(
            old_epoch, new_epoch,
            "precondition: a rebuilt store is a new store"
        );
        let lease2 = provision_stacked_worktree(&anchor, &lane2).unwrap();
        assert_eq!(
            (lease1.generation_id, &lease1.head_sha),
            (lease2.generation_id, &lease2.head_sha),
            "precondition: the rebuilt donor collides with the old entry's name"
        );
        let admitted = StackedStore::open(&lane2).unwrap();
        assert_eq!(
            admitted.base_store().pending_watermark().unwrap().epoch(),
            new_epoch,
            "lane-2 was admitted from the previous store's cache entry"
        );
        let _ = remove_readonly_tree(&root);
    }

    /// `pending_state.epoch` of the donor's store at `anchor`.
    #[cfg(all(feature = "parse", unix))]
    fn donor_epoch(anchor: &Path) -> String {
        Store::open_read_only(store_path(anchor))
            .unwrap()
            .pending_watermark()
            .unwrap()
            .epoch()
            .to_string()
    }

    /// An exclusive writer on the delta makes the read-only open fail with
    /// "database is locked"; the refusal must say that, not blame a WAL delta.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a3_a_locked_delta_is_not_blamed_on_a_wal_mode_delta() {
        let (root, _anchor, lane) = a3_lane("a3-busy");
        fs::write(lane.join("src/svc.go"), "package svc\n\nfunc Edited() {}\n").unwrap();
        a3_sync(&lane);
        let wt = lane.canonicalize().unwrap();
        let conn = rusqlite::Connection::open(stack_delta_path(&wt)).unwrap();
        let _: String = conn
            .query_row("PRAGMA locking_mode=EXCLUSIVE", [], |r| r.get(0))
            .unwrap();
        conn.execute_batch("BEGIN EXCLUSIVE;").unwrap();
        let err = a3_dirty(&lane).expect_err("an exclusive writer blocks the read");
        conn.execute_batch("ROLLBACK;").unwrap();
        eprintln!("a3 busy: {err}");
        assert!(
            !err.contains("WAL-mode delta"),
            "wrong cause named for a locked delta: {err}"
        );
        let _ = remove_readonly_tree(&root);
    }

    /// The other printed recovery, `devmap seed --stacked --from <donor>`,
    /// replaces only the lease and `stack/`; a symlinked journal beside the
    /// delta survives it.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a3_a_symlinked_journal_after_reprovisioning_names_the_link_and_recovers() {
        let (root, anchor, lane) = a3_lane("a3-symlink-reprov");
        fs::write(lane.join("src/svc.go"), "package svc\n\nfunc Edited() {}\n").unwrap();
        a3_sync(&lane);
        let wt = lane.canonicalize().unwrap();
        let (_db, foreign) = a3_hot_delta(&root, "outside", 5);
        let outside = root.join("outside-journal");
        fs::copy(&foreign, &outside).unwrap();
        std::os::unix::fs::symlink(
            &outside,
            stack_delta_path(&wt).with_file_name("delta.sqlite-journal"),
        )
        .unwrap();
        provision_stacked_worktree(&anchor, &lane).expect("re-provision succeeds");
        // Re-provisioning leaves the delta and its sidecars alone, so the only
        // recovery a refusal names for this cause is `seed --sync-delta`: it
        // must name the link, and running it must recover the lane.
        let refused = a3_dirty(&lane).expect_err("a symlinked journal refuses reads");
        assert!(
            refused.contains("delta.sqlite-journal is a symlink"),
            "{refused}"
        );
        assert!(refused.contains("devmap seed --sync-delta"), "{refused}");
        let (code, msg) = a3_run_sync_delta(&wt);
        assert_eq!(code, Some(0), "the printed recovery failed: {msg}");
        assert!(a3_dirty(&lane).is_ok(), "the lane is still stranded: {msg}");
        assert!(
            fs::symlink_metadata(stack_delta_path(&wt).with_file_name("delta.sqlite-journal"))
                .is_err(),
            "the link was not unlinked"
        );
        assert!(outside.is_file(), "the link's target was removed");
        let _ = remove_readonly_tree(&root);
    }

    /// An orphan `-journal` (left by `retire_stack`) beside a delta that is
    /// then recreated: is it replayed into the new delta?
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a3_an_orphan_journal_is_not_replayed_into_a_recreated_delta() {
        let root = temp_repo_dir("a3-orphan-replay");
        let (_db, foreign) = a3_hot_delta(&root, "old", 3);
        let dir = root.join("lane-state");
        fs::create_dir_all(&dir).unwrap();
        fs::copy(&foreign, dir.join("delta.sqlite-journal")).unwrap();
        let d = DeltaStore::open(dir.join("delta.sqlite")).unwrap();
        d.tombstone_file("src/new_a.rs", "CAS_TOMBSTONE", 1)
            .unwrap();
        d.tombstone_file("src/new_b.rs", "CAS_TOMBSTONE", 1)
            .unwrap();
        assert_eq!(d.dirty_file_count().unwrap(), 2);
        drop(d);
        let r = DeltaStore::open_read_only(dir.join("delta.sqlite"))
            .unwrap()
            .expect("exists");
        assert_eq!(
            r.dirty_file_count().unwrap(),
            2,
            "an orphan journal altered the recreated delta"
        );
        let _ = remove_readonly_tree(&root);
    }

    /// Every file under `dir` with its bytes, for "nothing was written" checks.
    #[cfg(feature = "parse")]
    fn state_snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(d) = pending.pop() {
            for entry in fs::read_dir(&d).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    out.push((path.clone(), fs::read(&path).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    #[cfg(feature = "parse")]
    #[test]
    fn legacy_base_store_ref_is_cleanly_refused_without_silent_upgrade() {
        let root = temp_repo_dir("legacy-ref-refused");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        // Two plain lanes: no lease, no store, so the pointer is all there is.
        let lane_a = add_lane(&anchor, &root, "lane-a");
        let lane_b = add_lane(&anchor, &root, "lane-b");
        let anchor_canon = anchor.canonicalize().unwrap();

        // A pointer at an unrelated repository resolves to no hint, and the
        // refusal still names the upgrade command.
        let unrelated = root.join("unrelated");
        seed_indexed_git_repo(&unrelated);
        let ref_a = lane_a.join(".devcouncil/codeintel/base_store_ref.json");
        fs::create_dir_all(ref_a.parent().unwrap()).unwrap();
        fs::write(
            &ref_a,
            serde_json::to_vec(&serde_json::json!({
                "anchor_repo_root": unrelated.canonicalize().unwrap(),
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(legacy_base_store_anchor_root(&lane_a).is_none());
        let before = state_snapshot(&lane_a.join(".devcouncil"));
        let err = StackedStore::open_if_leased(&lane_a)
            .expect_err("a legacy pointer must be refused")
            .to_string();
        assert!(err.contains("retired legacy worktree pointer"), "{err}");
        assert!(err.contains("devmap seed --stacked --from"), "{err}");
        assert_eq!(state_snapshot(&lane_a.join(".devcouncil")), before);
        assert!(!stack_lease_path(lane_a.canonicalize().unwrap()).exists());
        assert!(!stack_delta_path(lane_a.canonicalize().unwrap()).exists());
        assert!(!store_path(lane_a.canonicalize().unwrap()).exists());

        // A pointer at the sibling anchor puts that exact path in the command.
        fs::write(
            &ref_a,
            serde_json::to_vec(&serde_json::json!({ "anchor_repo_root": &anchor_canon })).unwrap(),
        )
        .unwrap();
        assert_eq!(
            legacy_base_store_anchor_root(&lane_a),
            Some(anchor_canon.clone())
        );
        let err = StackedStore::open_if_leased(&lane_a)
            .expect_err("a sibling pointer must still be refused")
            .to_string();
        assert!(
            err.contains(&format!(
                "devmap seed --stacked --from {}",
                anchor_canon.display()
            )),
            "{err}"
        );

        // The `.devmap` spelling and the `base_store_path` field are refused too.
        let ref_b = lane_b.join(".devmap/codeintel/base_store_ref.json");
        fs::create_dir_all(ref_b.parent().unwrap()).unwrap();
        fs::write(
            &ref_b,
            serde_json::to_vec(&serde_json::json!({
                "base_store_path": store_path(&anchor_canon),
            }))
            .unwrap(),
        )
        .unwrap();
        let err = StackedStore::open_if_leased(&lane_b)
            .expect_err("the .devmap pointer must be refused")
            .to_string();
        assert!(err.contains("devmap seed --stacked --from"), "{err}");

        // A lane that has a lease is served by it; the stale pointer is moot.
        provision_stacked_worktree(&anchor, &lane_a).unwrap();
        assert!(StackedStore::open_if_leased(&lane_a).unwrap().is_some());

        let _ = remove_readonly_tree(&root);
    }

    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn delta_writer_lock_is_taken_by_writes_never_by_reads_and_keeps_its_inode() {
        use std::os::unix::fs::MetadataExt;
        let (root, _anchor, lane) = a3_lane("delta-writer-lock");
        let lock_path = Store::writer_lock_path(&stack_delta_path(lane.canonicalize().unwrap()));

        // The hook's read path (open, status) must never create or take it.
        {
            let read_only = StackedStore::open(&lane).unwrap();
            read_only.status().unwrap();
        }
        assert!(
            !lock_path.exists(),
            "a read-only open created {}",
            lock_path.display()
        );

        // A write takes it and leaves the file in place, so a flock waiter can
        // never be split across two inodes.
        {
            let mut first = StackedStore::open(&lane).unwrap();
            first.record_deleted_file("src/a.rs").unwrap();
        }
        let first_meta = fs::metadata(&lock_path).expect("a write must have taken the lock");
        {
            let mut second = StackedStore::open(&lane).unwrap();
            second.record_deleted_file("src/b.rs").unwrap();
        }
        let second_meta = fs::metadata(&lock_path).unwrap();
        assert_eq!(
            (first_meta.dev(), first_meta.ino()),
            (second_meta.dev(), second_meta.ino())
        );

        // While another holder has it, a contender is refused, and it is free
        // again once released.
        let delta = stack_delta_path(lane.canonicalize().unwrap());
        let held = Store::lock_writer_at(&delta, std::time::Duration::from_secs(2)).unwrap();
        assert!(Store::lock_writer_at(&delta, std::time::Duration::from_millis(50)).is_err());
        drop(held);
        assert!(Store::lock_writer_at(&delta, std::time::Duration::from_secs(2)).is_ok());

        let _ = remove_readonly_tree(&root);
    }

    #[test]
    fn writer_locks_are_taken_delta_then_store_then_bases() {
        let ranks = || HELD_LOCK_RANKS.with(|held| held.borrow().clone());
        let _delta = RankGuard::enter(LockRank::Delta);
        let store = RankGuard::enter(LockRank::Store);
        let bases = RankGuard::enter(LockRank::Bases);
        assert_eq!(
            ranks(),
            vec![LockRank::Delta, LockRank::Store, LockRank::Bases]
        );
        drop(bases);
        drop(store);
        assert_eq!(ranks(), vec![LockRank::Delta]);
        // Released, the store rank can be taken again.
        drop(RankGuard::enter(LockRank::Store));
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "lock order violated")]
    fn taking_the_delta_lock_while_holding_the_store_lock_is_refused() {
        let _store = RankGuard::enter(LockRank::Store);
        let _delta = RankGuard::enter(LockRank::Delta);
    }

    #[cfg(all(feature = "parse", unix))]
    fn commit_all(anchor: &Path) {
        fs::write(anchor.join(".gitignore"), ".devmap/\n.devcouncil/\n").unwrap();
        run_git_ok(anchor, &["init", "-b", "main"]);
        run_git_ok(anchor, &["add", "."]);
        run_git_ok(anchor, &["commit", "-m", "initial"]);
    }

    #[cfg(all(feature = "parse", unix))]
    fn lane_over(tag: &str, files: &[(&str, &str)]) -> (PathBuf, PathBuf, PathBuf) {
        let root = temp_repo_dir(tag);
        let anchor = root.join("anchor");
        for (rel, text) in files {
            let path = anchor.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        commit_all(&anchor);
        let rels: Vec<&str> = files.iter().map(|(rel, _)| *rel).collect();
        index_tree(&anchor, &rels);
        let lane = add_lane(&anchor, &root, "lane");
        provision_stacked_worktree(&anchor, &lane).unwrap();
        (root, anchor, lane)
    }

    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn store_query_engine_with_stacked_answers_all_graph_and_search_surfaces_over_delta_edits_and_deletions(
    ) {
        let (root, _anchor, lane) = lane_over(
            "full-engine-surfaces",
            &[
                (
                    "src/db.rs",
                    "pub fn run_query() -> i32 { 42 }\npub fn legacy_literal_holder() -> &'static str { \"OLD_SECRET_TAG\" }\n",
                ),
                (
                    "src/dead_mod.rs",
                    "pub(crate) fn orphan_fn() -> i32 { 0 }\npub(crate) fn soon_called() -> i32 { 7 }\n",
                ),
                (
                    "src/obsolete.rs",
                    "pub(crate) fn vanished_api() -> &'static str { \"VANISHED_LITERAL\" }\n",
                ),
                (
                    "src/lib.rs",
                    "pub mod db;\npub mod dead_mod;\npub mod obsolete;\npub fn handle_req() -> i32 { db::run_query() }\n",
                ),
                (
                    "tests/smoke.rs",
                    "#[test]\nfn test_handle_req() {\n    let _ = handle_req();\n}\n",
                ),
            ],
        );

        fs::write(
            lane.join("src/db.rs"),
            "// header comment shifting spans\npub fn run_query() -> i32 { 99 }\npub fn stream_rows() -> i32 {\n    let _tag = \"LIVE_DELTA_LITERAL\";\n    run_query() + soon_called()\n}\n",
        )
        .unwrap();
        fs::write(
            lane.join("src/api.rs"),
            "use crate::db;\npub fn serve_stream() -> i32 {\n    stream_rows()\n}\n",
        )
        .unwrap();
        fs::write(
            lane.join("tests/smoke.rs"),
            "#[test]\nfn test_handle_req() {\n    let _ = handle_req();\n}\n\n#[test]\nfn test_stream_pipeline() {\n    let _ = serve_stream();\n}\n",
        )
        .unwrap();
        fs::remove_file(lane.join("src/obsolete.rs")).unwrap();
        a3_sync(&lane);

        let lane_canon = lane.canonicalize().unwrap();
        let delta_file = stack_delta_path(&lane_canon);
        let lease_file = stack_lease_path(&lane_canon);
        let delta_before = fs::read(&delta_file).unwrap();
        let lease_before = fs::read(&lease_file).unwrap();

        // An edit that is on disk only: a read-only open must not ingest it.
        fs::write(
            lane.join("src/unsynced_scratch.rs"),
            "pub fn unsynced_only_on_disk() {}\n",
        )
        .unwrap();

        let stacked = StackedStore::open(&lane).unwrap();
        let engine =
            crate::StoreQueryEngine::new(stacked.base_store()).with_stacked(Some(&stacked));
        let request = |query: &str, depth: usize| crate::Request {
            query: query.to_string(),
            token_budget: 4000,
            min_confidence: 0.0,
            max_depth: depth,
        };

        let search_new = engine.search(request("stream_rows", 1)).unwrap();
        assert_eq!(search_new.items.len(), 1);
        assert!(search_new.items[0]
            .source_span
            .contains("LIVE_DELTA_LITERAL"));
        assert!(engine
            .search(request("unsynced_only_on_disk", 1))
            .unwrap()
            .items
            .is_empty());
        assert!(engine
            .search(request("vanished_api", 1))
            .unwrap()
            .items
            .is_empty());

        let sem = engine.search_semantic("stream rows", 4000).unwrap();
        assert!(sem.items.iter().any(|h| h.symbol_name == "stream_rows"));
        let ask_res = engine.ask("stream rows", 4000, 0.0).unwrap();
        assert!(ask_res.items.iter().any(|h| h.symbol_name == "stream_rows"));

        let exp = engine.explore("stream_rows", 10, 4000, 0.0, 2).unwrap();
        assert_eq!(exp.definitions.items.len(), 1);
        let entry = &exp.definitions.items[0];
        assert!(entry
            .callers
            .items
            .iter()
            .any(|e| e.source_symbol.contains("serve_stream")));
        assert!(entry
            .callees
            .items
            .iter()
            .any(|e| e.target_symbol.contains("run_query")));

        let imp = engine.impact(request("run_query", 5)).unwrap();
        assert!(imp
            .items
            .iter()
            .any(|e| e.source_symbol.contains("stream_rows")));
        assert!(imp
            .items
            .iter()
            .any(|e| e.source_symbol.contains("test_stream_pipeline")));
        let layered = engine.impact_layered(request("run_query", 5)).unwrap();
        assert!(!layered.edges.items.is_empty());

        let neigh = engine
            .neighbors(&["stream_rows".to_string()], 4000, 0.0, 1)
            .unwrap();
        assert_eq!(neigh.len(), 1);
        assert!(!neigh[0].callers.items.is_empty());
        assert!(!neigh[0].callees.items.is_empty());

        let tr = engine
            .trace_between(crate::Request {
                query: ("test_stream_pipeline".to_string(), "run_query".to_string()),
                token_budget: 4000,
                min_confidence: 0.0,
                max_depth: 5,
            })
            .unwrap();
        assert!(!tr.items.is_empty());

        let deps_api = engine.dependencies(request("src/api.rs", 1)).unwrap();
        assert!(deps_api.items.iter().any(|e| e.target_file == "src/db.rs"));

        let aff = engine
            .affected_tests(&["src/db.rs".to_string()], 4000, 0.0, 5)
            .unwrap();
        assert!(aff
            .tests
            .items
            .iter()
            .any(|t| t.symbols.iter().any(|s| s.contains("test_stream_pipeline"))));

        // A repository-relative and a worktree-absolute path reach the same row.
        let abs_db = lane_canon.join("src/db.rs");
        let skel_db = engine.skeleton(abs_db.to_str().unwrap(), 4000).unwrap();
        assert_eq!(skel_db.presence, crate::SkeletonPresence::Indexed);
        assert!(skel_db
            .items
            .iter()
            .any(|i| i.qualified_name.contains("stream_rows")));
        let skel_del = engine.skeleton("src/obsolete.rs", 4000).unwrap();
        assert_eq!(skel_del.presence, crate::SkeletonPresence::NotInIndex);

        let dead = engine.dead_symbols(4000).unwrap();
        let dead_names: Vec<&str> = dead.items.iter().map(|d| d.symbol_name.as_str()).collect();
        assert!(dead_names.contains(&"orphan_fn"), "{dead_names:?}");
        assert!(!dead_names.contains(&"soon_called"), "{dead_names:?}");
        assert!(!dead_names.contains(&"vanished_api"), "{dead_names:?}");

        assert_eq!(
            engine
                .literals("LIVE_DELTA_LITERAL", false, 4000)
                .unwrap()
                .items
                .len(),
            1
        );
        assert!(engine
            .literals("OLD_SECRET_TAG", false, 4000)
            .unwrap()
            .items
            .is_empty());
        assert!(engine
            .literals("VANISHED_LITERAL", false, 4000)
            .unwrap()
            .items
            .is_empty());

        let prev = engine
            .preview(
                "src/db.rs",
                "pub fn run_query() -> i32 { 99 }\npub fn stream_rows(limit: usize) -> i32 { limit as i32 }\n",
                4000,
                0.0,
            )
            .unwrap();
        assert!(prev.delta_available);
        assert!(prev
            .broken_callers
            .items
            .iter()
            .any(|e| e.caller_symbol.contains("serve_stream")));

        drop(engine);
        drop(stacked);
        assert_eq!(
            fs::read(&delta_file).unwrap(),
            delta_before,
            "queries must not mutate a byte of delta.sqlite"
        );
        assert_eq!(
            fs::read(&lease_file).unwrap(),
            lease_before,
            "queries must not mutate stack_lease.json"
        );
        assert!(!delta_file.with_extension("sqlite-wal").exists());
        assert!(!delta_file.with_extension("sqlite-shm").exists());

        let _ = remove_readonly_tree(&root);
    }

    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn test_stacked_aliased_imports_resolve_calls_and_affected_tests() {
        let (root, _anchor, lane) = lane_over(
            "aliased-imports",
            &[
                (
                    "src/callee.rs",
                    "pub fn probe_callee() -> i32 {\n    7\n}\n",
                ),
                (
                    "src/mod_callee.rs",
                    "pub fn mod_target() -> i32 {\n    11\n}\n",
                ),
                ("src/lib.rs", "pub mod callee;\npub mod mod_callee;\n"),
                (
                    "tests/alias_test.rs",
                    "#[test]\nfn test_initial() {\n    assert_eq!(1, 1);\n}\n",
                ),
            ],
        );
        fs::write(
            lane.join("tests/alias_test.rs"),
            "use crate::callee::probe_callee as aliased_callee;\nuse crate::mod_callee as mc;\n\n#[test]\nfn test_via_symbol_alias() {\n    let _ = aliased_callee();\n}\n\n#[test]\nfn test_via_module_alias() {\n    let _ = mc::mod_target();\n}\n",
        )
        .unwrap();
        a3_sync(&lane);

        let stacked = StackedStore::open(&lane).unwrap();
        let engine =
            crate::StoreQueryEngine::new(stacked.base_store()).with_stacked(Some(&stacked));
        for (target, test) in [
            ("probe_callee", "test_via_symbol_alias"),
            ("mod_target", "test_via_module_alias"),
        ] {
            let affected = engine
                .affected_tests(&[target.to_string()], 4000, 0.0, 5)
                .unwrap();
            assert!(
                affected
                    .tests
                    .items
                    .iter()
                    .any(|t| t.symbols.iter().any(|s| s.contains(test))),
                "an aliased import must reach {target} from {test}: {:?}",
                affected.tests.items
            );
        }
        let _ = remove_readonly_tree(&root);
    }

    /// A delta call whose callee name the base does not hold uniquely is not a
    /// `UniqueGlobal` edge. The label asserts "no other symbol carries this name",
    /// which a lookup that stopped at a cap cannot know.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a_delta_call_is_unique_global_only_when_the_base_lookup_saw_every_match() {
        let (root, _anchor, lane) = lane_over(
            "unique-global-label",
            &[
                ("core/db.rs", "pub fn run_query() -> i32 { 42 }\n"),
                ("src/other.rs", "pub fn other_fn() -> i32 { 1 }\n"),
            ],
        );
        fs::write(
            lane.join("src/api.rs"),
            "pub fn serve() -> i32 {\n    run_query()\n}\n",
        )
        .unwrap();
        a3_sync(&lane);
        let stacked = StackedStore::open(&lane).unwrap();
        let ext = stacked
            .overlay()
            .added_extractions
            .get("src/api.rs")
            .expect("the synced file is in the overlay")
            .clone();
        let scope: Vec<String> = vec!["core/db.rs".to_string(), "src/other.rs".to_string()];
        let calls_to_run_query = |lookups: &mut BTreeMap<String, (Vec<StoredSymbol>, bool)>| {
            resolve_delta_stored_edges(
                "src/api.rs",
                &ext,
                stacked.overlay(),
                stacked.base_store(),
                &scope,
                lookups,
                &crate::cancel::Cancel::new(),
            )
            .unwrap()
            .into_iter()
            .filter(|e| e.edge_kind == "Calls" && e.target_symbol.contains("run_query"))
            .collect::<Vec<_>>()
        };

        // The real lookup sees the one match: a unique-global edge.
        let found = calls_to_run_query(&mut BTreeMap::new());
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].resolution.as_deref(), Some("UniqueGlobal"));

        // The same rows, but a lookup that reports it stopped short: no such claim.
        let (rows, _) = base_symbols_for_callee(stacked.base_store(), "run_query").unwrap();
        assert!(!rows.is_empty());
        let mut capped = BTreeMap::new();
        capped.insert("run_query".to_string(), (rows, false));
        assert!(
            calls_to_run_query(&mut capped).is_empty(),
            "a capped lookup must not mint a unique-global edge"
        );

        // A ceiling below the match count is reported as not exhaustive.
        let (_, exhaustive) =
            base_symbols_for_callee_within(stacked.base_store(), "run_query", 0).unwrap();
        assert!(
            !exhaustive,
            "a zero-row read of a matching name is not exhaustive"
        );
        let _ = remove_readonly_tree(&root);
    }

    /// A lane whose base holds `callees` functions and whose delta holds one file
    /// per callee, each calling its own: the merge does one base lookup per
    /// distinct callee.
    #[cfg(all(feature = "parse", unix))]
    fn lane_with_a_delta_caller_per_callee(tag: &str, callees: usize) -> (PathBuf, PathBuf) {
        let base_src: String = (0..callees)
            .map(|i| format!("pub fn callee_{i}() -> i32 {{ {i} }}\n"))
            .collect();
        let (root, _anchor, lane) = lane_over(tag, &[("src/base.rs", base_src.as_str())]);
        for i in 0..callees {
            fs::write(
                lane.join(format!("src/d{i}.rs")),
                format!("pub fn caller_{i}() -> i32 {{\n    callee_{i}()\n}}\n"),
            )
            .unwrap();
        }
        a3_sync(&lane);
        (root, lane)
    }

    /// A `Cancel` that counts every consultation and, when `trip_at` is set,
    /// cancels itself at that consultation.
    #[cfg(all(feature = "parse", unix))]
    fn counting_cancel(
        trip_at: Option<usize>,
    ) -> (crate::cancel::Cancel, Arc<std::sync::atomic::AtomicUsize>) {
        let checks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let flag = crate::cancel::Cancel::new();
        let handle = flag.clone();
        let seen = Arc::clone(&checks);
        let cancel = flag.with_check_probe(move || {
            let n = seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            if trip_at == Some(n) {
                handle.cancel();
            }
        });
        (cancel, checks)
    }

    #[cfg(all(feature = "parse", unix))]
    fn impact_request(query: &str) -> crate::Request<String> {
        crate::Request {
            query: query.to_string(),
            token_budget: 4000,
            min_confidence: 0.0,
            max_depth: 3,
        }
    }

    /// Cancellation has to reach the merged-edge rebuild. The merge reads every
    /// base edge and does one FTS lookup per distinct delta callee; a caller that
    /// has stopped waiting must not leave that running on a blocking thread.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn the_merged_edge_rebuild_consults_cancellation_once_per_callee_lookup() {
        const CALLEES: usize = 24;
        let (root, lane) = lane_with_a_delta_caller_per_callee("merge-cancel-count", CALLEES);
        let stacked = StackedStore::open(&lane).unwrap();

        let (first, first_checks) = counting_cancel(None);
        crate::StoreQueryEngine::new(stacked.base_store())
            .with_stacked(Some(&stacked))
            .with_cancel(first)
            .impact(impact_request("callee_5"))
            .unwrap();
        // The merge is cached now, so this run consults the flag only for the
        // engine's own work; the difference is what the merge itself consulted.
        let (second, second_checks) = counting_cancel(None);
        crate::StoreQueryEngine::new(stacked.base_store())
            .with_stacked(Some(&stacked))
            .with_cancel(second)
            .impact(impact_request("callee_5"))
            .unwrap();
        let in_merge = first_checks
            .load(std::sync::atomic::Ordering::SeqCst)
            .saturating_sub(second_checks.load(std::sync::atomic::Ordering::SeqCst));
        assert!(
            in_merge >= CALLEES,
            "the merge made {in_merge} cancellation checks for {CALLEES} distinct callee lookups"
        );
        let _ = remove_readonly_tree(&root);
    }

    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a_cancelled_merge_returns_cancelled_caches_nothing_and_the_next_call_is_complete() {
        const CALLEES: usize = 24;
        let (root, lane) = lane_with_a_delta_caller_per_callee("merge-cancel-trip", CALLEES);
        let stacked = StackedStore::open(&lane).unwrap();

        // The engine's own consultations, with the merge already cached.
        let primed = StackedStore::open(&lane).unwrap();
        primed
            .generation_edges_with_id(&crate::cancel::Cancel::new())
            .unwrap()
            .unwrap();
        let (probe, engine_only) = counting_cancel(None);
        crate::StoreQueryEngine::new(primed.base_store())
            .with_stacked(Some(&primed))
            .with_cancel(probe)
            .impact(impact_request("callee_5"))
            .unwrap();
        let engine_only = engine_only.load(std::sync::atomic::Ordering::SeqCst);

        // Trip three consultations past everything the engine does outside the
        // merge, which lands inside it.
        let (cancel, _) = counting_cancel(Some(engine_only + 3));
        let err = crate::StoreQueryEngine::new(stacked.base_store())
            .with_stacked(Some(&stacked))
            .with_cancel(cancel)
            .impact(impact_request("callee_5"))
            .expect_err("a cancelled merge must not return an answer");
        assert!(
            err.downcast_ref::<crate::cancel::QueryCancelled>()
                .is_some(),
            "the error must be the typed cancellation, got: {err:#}"
        );
        assert!(
            stacked.merged_edges.lock().unwrap().is_none(),
            "a cancelled merge must not leave a result in the cache"
        );

        let answer = crate::StoreQueryEngine::new(stacked.base_store())
            .with_stacked(Some(&stacked))
            .impact(impact_request("callee_5"))
            .unwrap();
        assert!(
            answer
                .items
                .iter()
                .any(|edge| edge.source_symbol.contains("caller_5")),
            "the call after a cancelled merge must hold the full delta: {:?}",
            answer.items
        );
        let _ = remove_readonly_tree(&root);
    }

    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn dead_over_a_lane_is_a_named_lower_bound_exactly_when_the_delta_changes_something() {
        let (root, _anchor, lane) = lane_over(
            "dead-lower-bound",
            &[
                ("src/helper.rs", "pub fn helper() -> i32 { 1 }\n"),
                ("src/user.rs", "pub fn user() -> i32 { helper() }\n"),
            ],
        );
        let clean = StackedStore::open(&lane).unwrap();
        assert_eq!(clean.dead_lower_bound_reason(), None);
        let engine = crate::StoreQueryEngine::new(clean.base_store()).with_stacked(Some(&clean));
        let answer = engine.dead_symbols(4000).unwrap();
        assert!(
            answer
                .walk_incomplete
                .as_deref()
                .is_none_or(|r| !r.contains("lower bound")),
            "a lane with no delta answers as its base: {:?}",
            answer.walk_incomplete
        );

        fs::remove_file(lane.join("src/user.rs")).unwrap();
        fs::write(lane.join("src/fresh.rs"), "pub fn fresh() -> i32 { 3 }\n").unwrap();
        a3_sync(&lane);
        let dirty = StackedStore::open(&lane).unwrap();
        let reason = dirty
            .dead_lower_bound_reason()
            .expect("a delta that edits and deletes files is a lower bound");
        assert!(
            reason.contains("src/fresh.rs") && reason.contains("src/user.rs"),
            "the reason names the delta's files: {reason}"
        );
        let engine = crate::StoreQueryEngine::new(dirty.base_store()).with_stacked(Some(&dirty));
        let answer = engine.dead_symbols(4000).unwrap();
        assert!(
            answer
                .walk_incomplete
                .as_deref()
                .is_some_and(|r| r.contains("lower bound") && r.contains("src/user.rs")),
            "{:?}",
            answer.walk_incomplete
        );
        let _ = remove_readonly_tree(&root);
    }

    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn attach_incompleteness_appends_to_a_producer_reason_and_stamps_a_bare_answer() {
        let (root, _anchor, lane) = lane_over(
            "attach-incompleteness",
            &[("src/a.rs", "pub fn a() -> i32 { 1 }\n")],
        );
        // Nothing synced, one edit on disk.
        fs::write(lane.join("src/b.rs"), "pub fn b() -> i32 { 2 }\n").unwrap();
        let stacked = StackedStore::open(&lane).unwrap();

        let mut answer = serde_json::json!({"walk_incomplete": "producer reason", "items": []});
        stacked.attach_incompleteness(&mut answer).unwrap();
        let walk = answer["walk_incomplete"].as_str().unwrap();
        assert!(walk.starts_with("producer reason; "), "{walk}");
        assert!(
            walk.contains("no delta.sqlite yet") && walk.contains("src/b.rs"),
            "{walk}"
        );
        assert!(
            answer.get("source_freshness").is_none(),
            "a lane with no delta changes is the base's own answer: {answer}"
        );

        a3_sync(&lane);
        let synced = StackedStore::open(&lane).unwrap();
        let mut bare = serde_json::json!({"items": []});
        synced.attach_incompleteness(&mut bare).unwrap();
        assert!(
            bare["source_freshness"]["reason"]
                .as_str()
                .is_some_and(|r| r.contains("stacked delta file(s)")),
            "{bare}"
        );
        let mut stamped = serde_json::json!({"source_freshness": {"fresh": true}});
        synced.attach_incompleteness(&mut stamped).unwrap();
        assert_eq!(
            stamped["source_freshness"],
            serde_json::json!({"fresh": true}),
            "an answer that already carries a stamp keeps it"
        );
        let mut not_an_object = serde_json::json!([1, 2]);
        synced.attach_incompleteness(&mut not_an_object).unwrap();
        assert_eq!(not_an_object, serde_json::json!([1, 2]));
        let _ = remove_readonly_tree(&root);
    }

    /// Retiring a stale lease deletes the delta, and every write to the delta
    /// holds the delta writer lock, so a retirement must not delete it while a
    /// sync holds that lock. The lock order is delta, then store, then bases.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn retiring_a_stale_lease_waits_on_the_delta_writer_lock_before_deleting_the_delta() {
        let (root, wt, _lane, _store) = a3_store_beside_lease();
        let delta = stack_delta_path(&wt);
        fs::write(&delta, b"a delta a sync is writing").unwrap();

        // Another thread holds the delta lock, as a running `seed --sync-delta` would.
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let holder = {
            let wt = wt.clone();
            std::thread::spawn(move || {
                let _held = lock_delta_writer_at(&wt, std::time::Duration::from_secs(5)).unwrap();
                locked_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
        };
        locked_rx.recv().unwrap();

        let refused = retire_stale_lease(&wt);
        let delta_survived = delta.is_file();
        let lease_survived = stack_lease_path(&wt).is_file();
        release_tx.send(()).unwrap();
        holder.join().unwrap();

        let err = format!(
            "{:#}",
            refused.expect_err("a held delta lock must refuse the retirement")
        );
        assert!(
            err.contains("delta"),
            "the refusal names the delta lock: {err}"
        );
        assert!(
            delta_survived,
            "the delta was deleted under a held delta writer lock"
        );
        assert!(lease_survived, "a refused retirement changes nothing");

        assert!(
            retire_stale_lease(&wt).unwrap(),
            "with the lock free the lease retires"
        );
        assert!(!delta.exists() && !stack_lease_path(&wt).exists());
        let _ = remove_readonly_tree(&root);
    }

    /// A merge cancelled before it starts stops at its first consultation, well
    /// inside a bound that a full merge of this delta is not held to.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a_pre_cancelled_merge_stops_at_the_first_check_and_quickly() {
        const CALLEES: usize = 24;
        let (root, lane) = lane_with_a_delta_caller_per_callee("merge-pre-cancelled", CALLEES);
        let stacked = StackedStore::open(&lane).unwrap();
        let (cancel, checks) = counting_cancel(Some(1));

        let started = std::time::Instant::now();
        let err = stacked
            .generation_edges_with_id(&cancel)
            .expect_err("a cancelled merge must not produce an index");
        let elapsed = started.elapsed();

        assert!(err
            .downcast_ref::<crate::cancel::QueryCancelled>()
            .is_some());
        assert_eq!(
            checks.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the merge kept consulting the flag after it was cancelled"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "a pre-cancelled merge took {elapsed:?}"
        );
        assert!(stacked.merged_edges.lock().unwrap().is_none());
        let (_, full) = stacked
            .generation_edges_with_id(&crate::cancel::Cancel::new())
            .unwrap()
            .unwrap();
        assert!(
            !full.into_target_symbol("src/base.rs::callee_7").is_empty(),
            "the uncancelled call after a cancelled one must be the full merge"
        );
        let _ = remove_readonly_tree(&root);
    }

    /// Cost of the merged-`GenerationEdges` rebuild against the MCP per-call
    /// ceiling (`devmap-serve/src/mcp.rs` `CALL_TIMEOUT`, 30 s). Every MCP call
    /// opens a fresh `StackedStore`, so this runs per call while the delta is
    /// non-empty. Ignored by default; run it with
    /// `cargo test -p devmap-query --lib --release merged_generation_edges -- --ignored --nocapture`.
    ///
    /// The base index is synthetic at 100k edges. When `DEVMAP_BENCH_STORE` names
    /// a real store (read only), that store's own edge index and symbol lookups
    /// are used instead, with a delta of two real source files and one deletion.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    #[ignore = "benchmark: run explicitly with --ignored --nocapture"]
    fn merged_generation_edges_rebuild_is_bounded_against_the_mcp_call_timeout() {
        const MCP_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
        const RUNS: usize = 7;

        let (root, base, base_index) = match std::env::var_os("DEVMAP_BENCH_STORE") {
            Some(path) => {
                let store = Store::open_read_only(Path::new(&path)).unwrap();
                let (_, index) = store.generation_edges_with_id().unwrap().unwrap();
                (None, store, index)
            }
            None => {
                let (root, _anchor, lane) =
                    lane_over("merge-bench", &[("src/a.rs", "pub fn a() -> i32 { 1 }\n")]);
                let stacked = StackedStore::open(&lane).unwrap();
                let base = open_lane_base_store(&stacked.lease().base_sqlite_path()).unwrap();
                let edges: Vec<StoredEdge> = (0..100_000u32)
                    .map(|i| StoredEdge {
                        source_file: format!("src/m{}.rs", i % 2000),
                        target_file: format!("src/m{}.rs", (i / 50) % 2000),
                        source_symbol: format!("src/m{}.rs::f{}", i % 2000, i / 2000),
                        target_symbol: format!("src/m{}.rs::f{}", (i / 50) % 2000, i % 50),
                        edge_kind: "Calls".to_string(),
                        confidence: 1.0,
                        resolution: Some("SameFile".to_string()),
                    })
                    .collect();
                let index = Arc::new(GenerationEdges::build(Arc::new(edges), None).unwrap());
                (Some(root), base, index)
            }
        };

        let mut overlay = WorktreeOverlay::new("bench", 1);
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        for (id, rel) in ["stacked.rs", "engine.rs"].iter().enumerate() {
            let src = fs::read_to_string(manifest.join("src").join(rel)).unwrap();
            let repo_rel = format!("rust/devmap-query/src/{rel}");
            let ext = devmap_extract::extract_file(&repo_rel, &src);
            let id = u32::try_from(id).unwrap();
            overlay.record_modified_file(&repo_rel, "bench", Some(id));
            overlay.record_extraction(repo_rel, ext, id);
        }
        overlay.mark_file_tombstone("src/m1.rs", Some(9));
        assert!(!overlay.modified_files.is_empty());

        let mut timings = Vec::new();
        let mut merged_len = 0;
        for _ in 0..RUNS {
            let started = std::time::Instant::now();
            let merged = merge_generation_edges(
                &base_index,
                &overlay,
                &base,
                1,
                &crate::cancel::Cancel::new(),
            )
            .unwrap();
            timings.push(started.elapsed());
            merged_len = merged.len();
        }
        timings.sort();
        let min = timings[0];
        eprintln!(
            "merge_generation_edges: base edges {}, merged edges {}, runs {RUNS}, min {:?}, median {:?}, max {:?}, MCP CALL_TIMEOUT {:?}",
            base_index.len(),
            merged_len,
            min,
            timings[RUNS / 2],
            timings[RUNS - 1],
            MCP_CALL_TIMEOUT
        );
        assert!(
            base_index.len() >= 90_000,
            "the benchmark must run at about 100k base edges, got {}",
            base_index.len()
        );
        assert!(
            min < MCP_CALL_TIMEOUT / 3,
            "the merged-edge rebuild must leave the call most of its {MCP_CALL_TIMEOUT:?}: min {min:?}"
        );
        if let Some(root) = root {
            let _ = remove_readonly_tree(&root);
        }
    }

    // ===== ATTACK TESTS (adversarial round 1, lane B ft-178dcf14). Each asserts the
    // CORRECT behaviour; a failure on 62d12f35 is a finding. =====

    /// A donor key naming `root_spelling`, taken from the donor's own store.
    #[cfg(all(feature = "parse", unix))]
    fn atk_key_for(store: &Store, root_spelling: &str) -> BaseKey {
        BaseKey {
            generation_id: store.latest_generation_id().unwrap().unwrap(),
            head_sha: store.latest_generation_head_sha().unwrap().unwrap(),
            source_repo_root: root_spelling.to_string(),
            epoch: store.pending_watermark().unwrap().epoch().to_string(),
        }
    }

    /// FINDING B-W1 (widening): on c2348467 `check_store_holds_key` accepted only a
    /// key whose root EQUALS the canonical stored root. A key spelled through a
    /// symlink alias of the donor was refused; `same_root` now accepts it.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn attack_w1_key_root_through_a_symlink_alias_was_refused_before_and_must_stay_refused() {
        let root = temp_repo_dir("atk-w1-alias");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let store = Store::open(&store_path(&anchor)).unwrap();
        let canon = anchor.canonicalize().unwrap();
        check_store_holds_key(
            &store,
            &atk_key_for(&store, &canon.to_string_lossy()),
            "control",
        )
        .expect("control: the canonical spelling is accepted");
        let alias = root.join("alias");
        std::os::unix::fs::symlink(&canon, &alias).unwrap();
        let res = check_store_holds_key(
            &store,
            &atk_key_for(&store, &alias.to_string_lossy()),
            "alias key",
        );
        assert!(res.is_err(), "c2348467 refused a key root that is not the canonical stored root; 62d12f35 accepts the alias");
        let _ = remove_readonly_tree(&root);
    }

    /// FINDING B-W2 (widening): on c2348467 a stored root that no longer exists
    /// could never match (`canonicalize` failed => refused). It now matches a key
    /// spelled identically.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn attack_w2_nonexistent_stored_root_was_refused_before_and_must_stay_refused() {
        let root = temp_repo_dir("atk-w2-gone");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let moved = root.join("anchor-moved");
        fs::rename(&anchor, &moved).unwrap();
        let store = Store::open(&store_path(&moved)).unwrap();
        let stored = store
            .stored_latest_generation_repo_root()
            .unwrap()
            .expect("a stored root");
        assert!(
            !Path::new(&stored).exists(),
            "fixture: the stored root is gone: {stored}"
        );
        let res = check_store_holds_key(&store, &atk_key_for(&store, &stored), "moved donor");
        assert!(
            res.is_err(),
            "c2348467 refused a store whose recorded root canonicalizes to nothing"
        );
        let _ = remove_readonly_tree(&root);
    }

    /// Provision-time admission compares the key with the root the caller's
    /// store reports (`latest_generation_repo_root`, which honours a
    /// `repo_root_override`), as c2348467 did: a store opened under an override
    /// that differs from its stored root is not the donor the key names.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn check_store_holds_key_refuses_a_store_opened_under_a_different_root_override() {
        let root = temp_repo_dir("cshk-override");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        provision_stacked_worktree(&anchor, &lane).unwrap();
        let base_sqlite = stack_dir(&lane.canonicalize().unwrap()).join(BASE_SQLITE);

        let plain = Store::open_immutable_snapshot(&base_sqlite, None).unwrap();
        let stored = plain
            .stored_latest_generation_repo_root()
            .unwrap()
            .expect("a stored root");
        let canon = Path::new(&stored).canonicalize().unwrap();
        let key = atk_key_for(&plain, &canon.to_string_lossy());
        check_store_holds_key(&plain, &key, "control")
            .expect("control: no override, the stored root is the key's");

        let overridden = Store::open_immutable_snapshot(
            &base_sqlite,
            Some(root.join("elsewhere").to_string_lossy().into_owned()),
        )
        .unwrap();
        assert_eq!(
            overridden.stored_latest_generation_repo_root().unwrap(),
            Some(stored),
            "fixture: the stored root is unchanged by the override"
        );
        let res = check_store_holds_key(&overridden, &key, "overridden base");
        assert!(
            res.is_err(),
            "a store opened under a different root override must not hold a key for its stored root"
        );
        let _ = remove_readonly_tree(&root);
    }

    /// FINDING B-R1 (false accept): a RELATIVE lease root is canonicalized against
    /// the process cwd, so a lease saying `../../../..<abs path>` is accepted as the
    /// stored absolute root.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn attack_r1_a_relative_lease_root_is_not_the_stored_absolute_root() {
        let root = temp_repo_dir("atk-r1-rel");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        let rel = format!(
            "{}{}",
            "../".repeat(64),
            lease.source_repo_root.trim_start_matches('/')
        );
        tamper_lease_field(&lane, "source_repo_root", serde_json::json!(rel));
        let opened = StackedStore::open(&lane).map(drop);
        let mcp = open_lane_base_store(&lease.base_sqlite_path()).map(drop);
        assert!(
            opened.is_err(),
            "StackedStore::open accepted relative root {rel}"
        );
        assert!(
            mcp.is_err(),
            "open_lane_base_store accepted relative root {rel}"
        );
        let _ = remove_readonly_tree(&root);
    }

    /// Spelling table for `same_root` (B-S1..). Prints every verdict (--nocapture)
    /// and asserts the ones with one defensible answer.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn attack_s1_same_root_spelling_table() {
        let root = temp_repo_dir("atk-s1");
        let a = root.join("repo").canonicalize().unwrap_or_else(|_| {
            fs::create_dir_all(root.join("repo")).unwrap();
            root.join("repo").canonicalize().unwrap()
        });
        fs::create_dir_all(root.join("repoc")).unwrap();
        let s = a.to_string_lossy().into_owned();
        let gone = format!("{s}/gone");
        let upper = s.to_uppercase();
        let swapped: String = s
            .chars()
            .map(|c| {
                if c.is_lowercase() {
                    c.to_ascii_uppercase()
                } else {
                    c.to_ascii_lowercase()
                }
            })
            .collect();
        // (spelling, stored, should_match)
        let cases: Vec<(String, String, bool)> = vec![
            (format!("{s}c"), s.clone(), false),
            (format!("{s}/"), s.clone(), true),
            (format!("{s}/."), s.clone(), true),
            (format!("{s}//"), s.clone(), true),
            (format!("{s}\0"), s.clone(), false),
            (format!("{s}\n"), s.clone(), false),
            (String::new(), s.clone(), false),
            (".".to_string(), s.clone(), false),
            (format!("{s}/gone/.."), s.clone(), false),
            (format!("{gone}x"), gone.clone(), false),
            (format!("{gone}/"), gone.clone(), true),
        ];
        let mut bad = Vec::new();
        for (lease, stored, want) in &cases {
            let got = same_root(Path::new(stored), Path::new(lease));
            eprintln!("same_root(stored={stored:?}, lease={lease:?}) = {got} (want {want})");
            if got != *want {
                bad.push(format!(
                    "stored={stored:?} lease={lease:?} got {got} want {want}"
                ));
            }
        }
        eprintln!(
            "INFO upper-case: {}",
            same_root(Path::new(&s), Path::new(&upper))
        );
        eprintln!(
            "INFO swapped-case: {}",
            same_root(Path::new(&s), Path::new(&swapped))
        );
        eprintln!(
            "INFO nonexistent-tail case variant: {}",
            same_root(Path::new(&gone), Path::new(&format!("{swapped}/gone")))
        );
        assert!(bad.is_empty(), "same_root verdicts: {bad:#?}");
        let _ = remove_readonly_tree(&root);
    }

    /// FINDING B-N1: a base whose latest generation recorded a NULL or empty root
    /// must be refused, not passed and not a panic.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn attack_n1_null_or_empty_stored_root_is_refused() {
        for (tag, sql) in [
            ("null", "UPDATE generations SET repo_root = NULL"),
            ("empty", "UPDATE generations SET repo_root = ''"),
        ] {
            let root = temp_repo_dir(&format!("atk-n1-{tag}"));
            let anchor = root.join("anchor");
            seed_indexed_git_repo(&anchor);
            let lane = add_lane(&anchor, &root, "lane");
            let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
            let db = store_path(&anchor);
            {
                let conn = rusqlite::Connection::open(&db).unwrap();
                // pending_state / seed_rebind may also carry a root; clear both so the row decides.
                let _ = conn.execute("DELETE FROM seed_rebind", []);
                let n = conn.execute(sql, []);
                eprintln!("{tag}: update -> {n:?}");
            }
            let store = Store::open(&db).unwrap();
            eprintln!(
                "{tag}: stored root = {:?}",
                store.stored_latest_generation_repo_root()
            );
            let lease_path = stack_lease_path(lane.canonicalize().unwrap());
            let res = check_lease_matches_base(&store, &lease, &lease_path);
            assert!(
                res.is_err(),
                "{tag}: a base with no recorded root passed the claim check"
            );
            let _ = remove_readonly_tree(&root);
        }
    }

    /// FINDING B-E1: hostile lease values are echoed into the error text unbounded
    /// and unescaped (ANSI escape, newline, 1 MB).
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn attack_e1_hostile_lease_values_are_bounded_and_escaped_in_errors() {
        let root = temp_repo_dir("atk-e1");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        let honest = fs::read(stack_lease_path(lane.canonicalize().unwrap())).unwrap();
        // A few KiB: well under the 64 KiB lease cap, so the claim check (not the
        // size cap) refuses the lease and echoes the value through `bounded_text`.
        let hostile = format!(
            "\x1b[2J\x1b]0;pwned\x07\u{202e}\nFAKE LINE: all good{}",
            "A".repeat(4 * 1024)
        );
        let mut bad = Vec::new();
        for field in ["source_repo_root", "head_sha"] {
            fs::write(stack_lease_path(lane.canonicalize().unwrap()), &honest).unwrap();
            tamper_lease_field(&lane, field, serde_json::json!(hostile));
            for (reader, res) in [
                ("StackedStore::open", StackedStore::open(&lane).map(drop)),
                (
                    "open_lane_base_store",
                    open_lane_base_store(&lease.base_sqlite_path()).map(drop),
                ),
            ] {
                let text = format!("{:#}", res.expect_err("must refuse"));
                if text.len() > 16 * 1024 {
                    bad.push(format!("{reader}/{field}: error is {} bytes", text.len()));
                }
                if text.contains('\x1b') {
                    bad.push(format!("{reader}/{field}: raw ESC in error"));
                }
                if text.contains('\u{202e}') || !text.contains("\\u{202e}") {
                    bad.push(format!("{reader}/{field}: bidi control not escaped"));
                }
                if !text.contains("…[") || !text.contains("bytes]") {
                    bad.push(format!(
                        "{reader}/{field}: no cut marker, value not bounded"
                    ));
                }
                if text.contains("\nFAKE LINE") {
                    bad.push(format!(
                        "{reader}/{field}: raw newline lets the value forge a line"
                    ));
                }
            }
        }
        assert!(bad.is_empty(), "{bad:#?}");
        let _ = remove_readonly_tree(&root);
    }

    /// A lease past the size cap is refused unread, naming the recovery, by both
    /// readers of the lease file; the padding is JSON whitespace, so an
    /// unbounded read would have parsed it and opened the lane.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a_lease_over_the_size_cap_is_refused_unread_with_the_recovery() {
        let root = temp_repo_dir("lease-cap");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        let lease_path = stack_lease_path(lane.canonicalize().unwrap());
        let mut bytes = fs::read(&lease_path).unwrap();
        bytes.extend(std::iter::repeat_n(b' ', MAX_LEASE_BYTES as usize));
        fs::write(&lease_path, &bytes).unwrap();
        let mut unguarded = Vec::new();
        for (reader, res) in [
            ("StackedStore::open", StackedStore::open(&lane).map(drop)),
            (
                "open_lane_base_store",
                open_lane_base_store(&lease.base_sqlite_path()).map(drop),
            ),
            (
                "StackLease::read_from",
                StackLease::read_from(&lease_path).map(drop),
            ),
        ] {
            match res {
                Ok(()) => unguarded.push(format!("{reader} read an oversized lease")),
                Err(e) => {
                    let text = format!("{e:#}");
                    if !text.contains("larger than") || !text.contains("devmap seed --stacked") {
                        unguarded.push(format!("{reader}: {text}"));
                    }
                }
            }
        }
        assert!(unguarded.is_empty(), "{unguarded:#?}");
        let _ = remove_readonly_tree(&root);
    }

    /// A lease root with a `..` component, a leading `.` or no leading `/` is
    /// refused even when it resolves to the stored root: provision only writes canonical
    /// absolute roots.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a_lease_root_with_parent_or_relative_components_is_refused_even_when_it_resolves() {
        let root = temp_repo_dir("lease-root-shape");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        let honest = fs::read(stack_lease_path(lane.canonicalize().unwrap())).unwrap();
        let canon = lease.source_repo_root.clone();
        let parent = Path::new(&canon)
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let name = Path::new(&canon)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let cases = [
            format!(
                "{parent}/../{}/{name}",
                Path::new(&parent).file_name().unwrap().to_string_lossy()
            ),
            format!("{canon}/../{name}"),
            format!("./{}", canon.trim_start_matches('/')),
        ];
        let mut unguarded = Vec::new();
        for forged in cases {
            fs::write(stack_lease_path(lane.canonicalize().unwrap()), &honest).unwrap();
            tamper_lease_field(&lane, "source_repo_root", serde_json::json!(forged));
            match StackedStore::open(&lane) {
                Ok(_) => unguarded.push(format!("accepted {forged}")),
                Err(e) => {
                    let text = format!("{e:#}");
                    if !text.contains("lease source_repo_root")
                        || !text.contains("devmap seed --stacked")
                    {
                        unguarded.push(format!("{forged}: {text}"));
                    }
                }
            }
        }
        assert!(unguarded.is_empty(), "{unguarded:#?}");
        let _ = remove_readonly_tree(&root);
    }

    /// Bidi overrides in a lease value are escaped in the refusal, not echoed.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn bidi_characters_in_a_lease_value_are_escaped_in_the_refusal() {
        let root = temp_repo_dir("lease-bidi");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        provision_stacked_worktree(&anchor, &lane).unwrap();
        tamper_lease_field(
            &lane,
            "head_sha",
            serde_json::json!("abc\u{202e}def\u{2066}"),
        );
        let text = format!(
            "{:#}",
            StackedStore::open(&lane)
                .map(drop)
                .expect_err("must refuse")
        );
        assert!(
            !text.contains('\u{202e}') && !text.contains('\u{2066}'),
            "raw bidi in: {text}"
        );
        assert!(text.contains("\\u{202e}"), "{text}");
        let _ = remove_readonly_tree(&root);
    }

    /// Ruling ft-3abcc3b9 (c), 2026-11-10: the stat identity pins the base bytes;
    /// `base.sha256` is recorded at admission and advisory after it. Drop the field
    /// at the next STACK_FORMAT_VERSION bump; do not bump for it alone.
    ///
    /// So a lease whose recorded sha256 is not the base's hash still opens, through
    /// both readers, while a stat-identity tamper of the base is still refused.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn a_lease_only_base_sha256_mismatch_opens_because_stat_identity_is_the_anchor() {
        let root = temp_repo_dir("atk2-sha");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        let mut refused = Vec::new();
        for forged in [
            serde_json::json!("ab".repeat(32)),
            serde_json::json!(""),
            serde_json::json!("zz"),
        ] {
            let honest = serde_json::to_value(&lease.base).unwrap();
            let mut rec = honest.clone();
            rec["sha256"] = forged.clone();
            tamper_lease_field(&lane, "base", rec);
            if let Err(e) = StackedStore::open(&lane) {
                refused.push(format!(
                    "StackedStore::open refused base.sha256 = {forged}: {e:#}"
                ));
            }
            if let Err(e) = open_lane_base_store(&lease.base_sqlite_path()) {
                refused.push(format!(
                    "open_lane_base_store refused base.sha256 = {forged}: {e:#}"
                ));
            }
            tamper_lease_field(&lane, "base", honest);
        }
        assert!(refused.is_empty(), "{refused:#?}");

        // The anchor: with the honest lease restored, a change to the base's stat
        // identity (the same edit as `open_refuses_an_in_place_edit_that_moves_mtime`)
        // is refused by both readers.
        StackedStore::open(&lane).expect("the honest lease opens");
        let base_dir = lease.base_dir();
        let base_sqlite = base_dir.join("base.sqlite");
        make_tree_writable(&base_dir);
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut bytes = fs::read(&base_sqlite).unwrap();
        let at = bytes.windows(6).position(|w| w == b"Helper").unwrap();
        bytes[at] = b'X';
        fs::write(&base_sqlite, &bytes).unwrap();
        set_file_readonly(&base_sqlite).unwrap();
        assert!(
            StackedStore::open(&lane).is_err(),
            "a stat-identity tamper must still be refused by StackedStore::open"
        );
        assert!(
            open_lane_base_store(&lease.base_sqlite_path()).is_err(),
            "a stat-identity tamper must still be refused by open_lane_base_store"
        );
        let _ = remove_readonly_tree(&root);
    }
}
