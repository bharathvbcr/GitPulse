//! Glean-style `StackedStore` combining an immutable `chmod 0444` base snapshot
//! (`<common_git_dir>/devmap/bases/gen-<G>/base.sqlite?immutable=1` + lazy mmap
//! `base.sqlite.csr`) with a per-worktree `DeltaStore` (`delta.sqlite`) and
//! `WorktreeOverlay` (`RoaringBitmap` dirty-node masking) (`F1`, `F4`, `F6`,
//! `F10`, `OQ1`, `OQ2`).

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use devmap_extract::model::{Extraction, WiringKind};
use devmap_extract::paths::{
    code_graph_path, compact_code_graph_path, csr_path_for_store, find_legacy_base_store_ref,
    main_checkout_of, registered_common_git_dir, registered_worktrees_of_common_dir, repo_map_path,
    repo_root_from_store, same_registered_repo, stack_delta_path, stack_lease_path, state_dir,
    store_path,
};
use devmap_resolve::model::LangFamily;
use devmap_store::csr::CsrGraphReader;
use devmap_store::delta::{DeltaEdgeRecord, DeltaStore};
use devmap_store::schema::CURRENT_SCHEMA_VERSION;
use devmap_store::seed::{
    discard_staged, prepare_immutable_snapshot, promote_immutable_base_to_store, seed_artifact,
    seed_directory_cow, set_file_readonly, set_file_writable, stage_seed_healing_wal,
};
use devmap_store::{
    CallersPage, DeadPage, FileEdges, GenerationEdges, SearchNarrowing, SearchPage, Store,
    StoredEdge, StoredFile, StoredSymbol,
};
use serde::{Deserialize, Serialize};

use crate::artifacts::write_atomic;
use crate::cas::{blake3_hash, CAS_TOMBSTONE};
use crate::overlay::{OverlayEdge, RoaringBitmap, WorktreeOverlay};

pub const STACK_FORMAT_VERSION: u32 = 1;
pub const DEFAULT_REBASE_DIRTY_FILES: usize = 250;
pub const DEFAULT_REBASE_DELTA_BYTES: u64 = 16 * 1024 * 1024;
pub type ScopeInputs = (u32, Option<String>, Vec<(String, String)>);

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
    pub created_at_unix_ms: u64,
    pub base_sqlite_sha256: String,
    /// `(len, mtime_ns, ino, dev)` of `base.sqlite` at publish. `open` runs per
    /// MCP call, so it compares these instead of re-hashing the file; a swap
    /// by rename changes the inode.
    pub base_sqlite_len: u64,
    pub base_sqlite_mtime_ns: u64,
    /// Recorded after the final chmod. An in-place edit that restores `mtime`
    /// still moves `ctime`, which user space cannot set.
    pub base_sqlite_ctime_ns: u64,
    pub base_sqlite_ino: u64,
    pub base_sqlite_dev: u64,
    #[serde(default)]
    pub has_csr: bool,
    /// The CSR sidecar's content and identity, when one was published.
    #[serde(default)]
    pub csr: Option<FileRecord>,
}

/// Content hash and identity of one published file.
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

/// What names a published base: generation id, donor `HEAD`, donor root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseKey {
    pub generation_id: u32,
    pub head_sha: String,
    pub source_repo_root: String,
}

impl BaseKey {
    pub fn new(generation_id: u32, head_sha: &str, source_root: &Path) -> Result<Self> {
        if !is_commit_sha(head_sha) {
            bail!(
                "donor generation {generation_id} carries head_sha {head_sha:?}, not a git commit; \
                 a stacked base must be taken from a built, committed checkout"
            );
        }
        Ok(Self {
            generation_id,
            head_sha: head_sha.to_string(),
            source_repo_root: source_root.to_string_lossy().into_owned(),
        })
    }

    /// `gen-<G>-<head12>-<src8>`.
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StackLease {
    pub format_version: u32,
    pub generation_id: u32,
    pub base_dir: String,
    pub worktree_root: String,
    pub leased_at_unix_ms: u64,
    pub rebases_after_dirty_files: usize,
    pub rebases_after_delta_bytes: u64,
}

impl StackLease {
    pub fn base_sqlite_path(&self) -> PathBuf {
        PathBuf::from(&self.base_dir).join("base.sqlite")
    }

    pub fn read_from(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("cannot read stack lease {}", path.display()))?;
        let lease: Self = serde_json::from_str(&raw)
            .with_context(|| format!("cannot parse stack lease {}", path.display()))?;
        Ok(lease)
    }
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

/// The exclusive lock on `<common_git_dir>/devmap/bases/.lock`, held by GC for
/// its whole run and by provision from before publishing until the lease is
/// written. Bounded; the timeout names the holder's pid.
fn lock_bases(
    common_git_dir: &Path,
    wait: std::time::Duration,
) -> Result<devmap_store::WriterLock> {
    let bases_dir = bases_root_dir(common_git_dir);
    fs::create_dir_all(&bases_dir)
        .with_context(|| format!("cannot create {}", bases_dir.display()))?;
    Store::lock_file_at(&bases_dir.join(".lock"), wait)
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
pub fn publish_base_snapshot(source_root: &Path) -> Result<(PathBuf, BaseManifest)> {
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
        let key = BaseKey::new(generation_id, &head, &source_canon)?;
        assert_donor_is_head_tree(&source_canon, &store, &key.head_sha)?;
        key
    };

    let bases_dir = bases_root_dir(&common_git_dir);
    fs::create_dir_all(&bases_dir)
        .with_context(|| format!("cannot create {}", bases_dir.display()))?;
    let final_dir = base_generation_dir(&common_git_dir, &key);
    if fs::symlink_metadata(&final_dir).is_ok() {
        match verify_base_dir(&final_dir, &key, true) {
            Ok(existing) => return Ok((final_dir, existing)),
            // A rebuilt donor restarts its generation ids, so its new base can
            // meet an old one of the same name whose bytes differ. Nobody leases
            // it: replace it (callers hold the bases lock). A leased one is a
            // real conflict and stays an error.
            Err(err)
                if err.downcast_ref::<BaseShaMismatch>().is_some()
                    && !base_is_leased(&common_git_dir, &final_dir)? =>
            {
                remove_readonly_tree(&final_dir).with_context(|| {
                    format!("cannot replace unleased base {}", final_dir.display())
                })?;
            }
            Err(err) => return Err(err),
        }
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
        let staged_sqlite = stage_dir.join("base.sqlite");
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
        let identity = file_identity(&staged_sqlite)?;

        let src_csr = csr_path_for_store(&source_db);
        let staged_csr = csr_path_for_store(&staged_sqlite);
        let csr = if src_csr.is_file() && seed_artifact(&src_csr, &staged_csr)?.is_some() {
            set_file_readonly(&staged_csr)?;
            let id = file_identity(&staged_csr)?;
            Some(FileRecord {
                sha256: sha256_file_hex(&staged_csr)?,
                len: id.len,
                mtime_ns: id.mtime_ns,
                ctime_ns: id.ctime_ns,
                ino: id.ino,
                dev: id.dev,
            })
        } else {
            None
        };

        let manifest = BaseManifest {
            format_version: STACK_FORMAT_VERSION,
            generation_id: key.generation_id,
            schema_version: CURRENT_SCHEMA_VERSION,
            source_repo_root: key.source_repo_root.clone(),
            head_sha: key.head_sha.clone(),
            created_at_unix_ms: now_unix_ms(),
            base_sqlite_sha256: sha256,
            base_sqlite_len: identity.len,
            base_sqlite_mtime_ns: identity.mtime_ns,
            base_sqlite_ctime_ns: identity.ctime_ns,
            base_sqlite_ino: identity.ino,
            base_sqlite_dev: identity.dev,
            has_csr: csr.is_some(),
            csr,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
        write_atomic(&stage_dir.join("manifest.json"), &manifest_bytes)?;
        set_file_readonly(&stage_dir.join("manifest.json"))?;
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
                let existing = verify_base_dir(&final_dir, &key, true)?;
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

struct FileIdentity {
    len: u64,
    mtime_ns: u64,
    ctime_ns: u64,
    ino: u64,
    dev: u64,
}

fn file_identity(path: &Path) -> Result<FileIdentity> {
    let meta =
        fs::symlink_metadata(path).with_context(|| format!("cannot stat {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mtime_ns = u64::try_from(meta.mtime())
            .unwrap_or(0)
            .saturating_mul(1_000_000_000)
            .saturating_add(u64::try_from(meta.mtime_nsec()).unwrap_or(0));
        let ctime_ns = u64::try_from(meta.ctime())
            .unwrap_or(0)
            .saturating_mul(1_000_000_000)
            .saturating_add(u64::try_from(meta.ctime_nsec()).unwrap_or(0));
        Ok(FileIdentity {
            len: meta.len(),
            mtime_ns,
            ctime_ns,
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
        Ok(FileIdentity {
            len: meta.len(),
            mtime_ns,
            ctime_ns: mtime_ns,
            ino: 0,
            dev: 0,
        })
    }
}

const MAX_MANIFEST_BYTES: u64 = 1 << 20;

fn read_base_manifest(base_dir: &Path) -> Result<BaseManifest> {
    let manifest_path = base_dir.join("manifest.json");
    let unreadable = || {
        format!(
            "cannot read base manifest {}; re-run `devmap seed --stacked`",
            manifest_path.display()
        )
    };
    let file = fs::File::open(&manifest_path).with_context(unreadable)?;
    // A real manifest is under a kilobyte; refuse to slurp a hostile one.
    let mut raw = Vec::new();
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut raw)
        .with_context(unreadable)?;
    if raw.len() as u64 > MAX_MANIFEST_BYTES {
        bail!(
            "base manifest {} is larger than {MAX_MANIFEST_BYTES} bytes; refusing to read it",
            manifest_path.display()
        );
    }
    serde_json::from_slice(&raw).with_context(|| {
        format!(
            "base manifest {} is legacy or malformed; re-run `devmap seed --stacked`",
            manifest_path.display()
        )
    })
}

/// Compare a base's recorded identity with the one a caller expects. Never
/// returns `Ok` on a mismatch and never republishes over one: a different value
/// under the expected name is tampering or a bug.
fn check_manifest_matches(manifest: &BaseManifest, expected: &BaseKey, dir: &Path) -> Result<()> {
    let mismatch = |field: &str, recorded: String, wanted: String| {
        anyhow::anyhow!(
            "base snapshot {} does not match its key: {field} is {recorded} but {wanted} was expected; \
             refusing to reuse it (remove it and re-run `devmap seed --stacked`)",
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
    Ok(())
}

fn check_identity_matches(manifest: &BaseManifest, base_sqlite: &Path) -> Result<()> {
    let now = file_identity(base_sqlite)?;
    let recorded = (
        manifest.base_sqlite_len,
        manifest.base_sqlite_mtime_ns,
        manifest.base_sqlite_ctime_ns,
        manifest.base_sqlite_ino,
        manifest.base_sqlite_dev,
    );
    let current = (now.len, now.mtime_ns, now.ctime_ns, now.ino, now.dev);
    if recorded != current {
        bail!(
            "base snapshot {} was replaced or modified after it was published \
             (recorded len/mtime_ns/ctime_ns/ino/dev {recorded:?}, now {current:?}); \
             remove it and re-run `devmap seed --stacked`",
            base_sqlite.display()
        );
    }
    Ok(())
}

/// The CSR sidecar a base was published with: `None` when it was published
/// without one. A sidecar the manifest does not record, or one that is missing,
/// replaced or (with `full_hash`) different in content is refused.
fn verify_csr(
    manifest: &BaseManifest,
    base_sqlite: &Path,
    full_hash: bool,
) -> Result<Option<PathBuf>> {
    let csr_path = csr_path_for_store(base_sqlite);
    let present = fs::symlink_metadata(&csr_path).is_ok();
    let Some(record) = manifest.csr.as_ref() else {
        if present {
            bail!(
                "base snapshot has a CSR sidecar {} its manifest does not record; \
                 remove the base and re-run `devmap seed --stacked`",
                csr_path.display()
            );
        }
        return Ok(None);
    };
    if !present {
        bail!(
            "base snapshot's CSR sidecar {} is missing; remove the base and re-run `devmap seed --stacked`",
            csr_path.display()
        );
    }
    let now = file_identity(&csr_path)?;
    let recorded = (
        record.len,
        record.mtime_ns,
        record.ctime_ns,
        record.ino,
        record.dev,
    );
    let current = (now.len, now.mtime_ns, now.ctime_ns, now.ino, now.dev);
    if recorded != current {
        bail!(
            "CSR sidecar {} was replaced or modified after it was published \
             (recorded len/mtime_ns/ctime_ns/ino/dev {recorded:?}, now {current:?}); \
             remove the base and re-run `devmap seed --stacked`",
            csr_path.display()
        );
    }
    if full_hash {
        let actual = sha256_file_hex(&csr_path)?;
        if actual != record.sha256 {
            bail!(
                "CSR sidecar {} fails its sha256 check: recorded {} but the file hashes to {actual}; \
                 remove the base and re-run `devmap seed --stacked`",
                csr_path.display(),
                record.sha256
            );
        }
    }
    Ok(Some(csr_path))
}

/// Refuse a base that anyone can write to: the file, or the directory that
/// would let it be swapped by rename.
#[cfg(unix)]
fn ensure_sealed(base_dir: &Path, base_sqlite: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    for (what, path) in [("file", base_sqlite), ("directory", base_dir)] {
        let meta = fs::symlink_metadata(path)
            .with_context(|| format!("cannot stat {}", path.display()))?;
        if meta.mode() & 0o222 != 0 {
            bail!(
                "base snapshot {what} {} is writable (mode {:04o}); refusing to open it as an immutable snapshot",
                path.display(),
                meta.mode() & 0o7777
            );
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_sealed(_base_dir: &Path, _base_sqlite: &Path) -> Result<()> {
    Ok(())
}

/// Reuse check for a base directory that already exists under `expected`'s
/// name: manifest fields, the recomputed sha256 of `base.sqlite` (when
/// `full_hash`), and SQLite admission. `Err` on any disagreement.
/// A published base whose bytes no longer hash to its manifest. Typed so that
/// provision can tell it from every other refusal: an unleased one is replaced.
#[derive(Debug)]
struct BaseShaMismatch(String);

impl std::fmt::Display for BaseShaMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BaseShaMismatch {}

/// Whether any registered worktree's lease names `base_dir`. An unreadable or
/// malformed lease is `Err`: not knowing is not "unleased".
fn base_is_leased(common_git_dir: &Path, base_dir: &Path) -> Result<bool> {
    let name = base_dir.file_name();
    for (_, lease_file) in lease_files_of_worktrees(common_git_dir) {
        match fs::symlink_metadata(&lease_file) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => bail!("cannot inspect lease {} ({err})", lease_file.display()),
            Ok(_) => {}
        }
        let lease = StackLease::read_from(&lease_file)?;
        if Path::new(&lease.base_dir).file_name() == name {
            return Ok(true);
        }
    }
    Ok(false)
}

fn verify_base_dir(base_dir: &Path, expected: &BaseKey, full_hash: bool) -> Result<BaseManifest> {
    let manifest = read_base_manifest(base_dir)?;
    check_manifest_matches(&manifest, expected, base_dir)?;
    let base_sqlite = base_dir.join("base.sqlite");
    if full_hash {
        let actual = sha256_file_hex(&base_sqlite)?;
        if actual != manifest.base_sqlite_sha256 {
            return Err(anyhow::Error::new(BaseShaMismatch(format!(
                "base snapshot {} fails its sha256 check: recorded {} but the file hashes to {actual}; \
                 refusing to reuse it. To discard it: `chmod -R u+w {dir} && rm -rf {dir}`, then re-run `devmap seed --stacked`",
                base_sqlite.display(),
                manifest.base_sqlite_sha256,
                dir = base_dir.display()
            ))));
        }
    }
    // Verify the snapshot satisfies Store::open_immutable_snapshot invariants (`0444`, no sidecars).
    let _ = Store::open_immutable_snapshot(&base_sqlite, None).with_context(|| {
        format!(
            "published base snapshot {} failed admission",
            base_sqlite.display()
        )
    })?;
    Ok(manifest)
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
    git_status_paths_and_deletions_within(root, deadline).map(|(paths, _)| paths)
}

fn git_status_paths_and_deletions_within(
    root: &Path,
    deadline: std::time::Duration,
) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
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
    let mut status_deleted = BTreeSet::new();
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
        let norm = status_path(root, &entry[3..])?;
        if status.contains(&b'D') {
            status_deleted.insert(norm.clone());
        }
        paths.insert(norm);
        if status.contains(&b'R') || status.contains(&b'C') {
            let orig = entries.next().ok_or_else(|| {
                anyhow::anyhow!(
                    "git status in {} lists a rename with no origin path",
                    root.display()
                )
            })?;
            let orig_norm = status_path(root, orig)?;
            if status.contains(&b'R') {
                status_deleted.insert(orig_norm.clone());
            }
            paths.insert(orig_norm);
        }
    }
    Ok((paths, status_deleted))
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

/// Inspect `<worktree_root>/<state>/codeintel/base_store_ref.json` (if present) and
/// return the bidirectionally verified registered anchor checkout root when its
/// store exists. Never provisions or mutates state on disk.
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

/// Fail closed on retired `base_store_ref.json` pointers without writing or
/// auto-upgrading any state on read (`Owner Stop #2`).
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
/// `source_root`'s immutable base snapshot (`P2.1`, `P2.2`, Critique 1).
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

    // One lock for publish through the lease write, so a base cannot be
    // collected between being published and being leased.
    let common_git_dir = registered_common_git_dir(&source_canon).ok_or_else(|| {
        anyhow::anyhow!(
            "{} is not a registered git checkout; refusing stacked provisioning",
            source_canon.display()
        )
    })?;
    let _bases_lock = lock_bases(&common_git_dir, BASES_LOCK_WAIT)?;
    let (base_dir, manifest) = publish_base_snapshot(&source_canon)?;
    // Everything that can fail comes before the lease: a failed provision must
    // not leave a leased lane behind a non-zero exit. `delta.sqlite` is not
    // created here either; only a sync or a build creates it.
    seed_derived_artifacts_and_reuse_caches(&source_canon, &wt_canon)?;

    let lease = StackLease {
        format_version: STACK_FORMAT_VERSION,
        generation_id: manifest.generation_id,
        base_dir: base_dir.to_string_lossy().into_owned(),
        worktree_root: wt_canon.to_string_lossy().into_owned(),
        leased_at_unix_ms: now_unix_ms(),
        rebases_after_dirty_files: DEFAULT_REBASE_DIRTY_FILES,
        rebases_after_delta_bytes: DEFAULT_REBASE_DELTA_BYTES,
    };
    let lease_bytes = serde_json::to_vec_pretty(&lease)?;
    write_atomic(&stack_lease_path(&wt_canon), &lease_bytes)?;

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

/// Garbage-collect unleased `gen-<old>` snapshots under `<common_git_dir>/devmap/bases/`.
///
/// Fails closed (`Err`) without deleting any base directory if ANY registered
/// worktree carries an unreadable or malformed `stack_lease.json` (`OQ2`).
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
    refuse_unverifiable_worktrees(&common_canon)?;

    let mut leased_dirs: BTreeSet<String> = BTreeSet::new();
    for (_, lease_file) in lease_files_of_worktrees(&common_canon) {
        match fs::symlink_metadata(&lease_file) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                bail!(
                    "refusing base GC: cannot inspect lease {} ({err})",
                    lease_file.display()
                );
            }
            Ok(meta) if !meta.is_file() => {
                bail!(
                    "refusing base GC: lease {} is not a regular file",
                    lease_file.display()
                );
            }
            Ok(_) => {}
        }
        let raw = fs::read(&lease_file).with_context(|| {
            format!(
                "refusing base GC: cannot read worktree lease {}",
                lease_file.display()
            )
        })?;
        let lease: StackLease = serde_json::from_slice(&raw).with_context(|| {
            format!(
                "refusing base GC: malformed stack_lease.json at {}",
                lease_file.display()
            )
        })?;
        if lease.format_version != STACK_FORMAT_VERSION {
            bail!(
                "refusing base GC: unsupported stack_lease format_version {} at {}",
                lease.format_version,
                lease_file.display()
            );
        }
        if let Some(name) = Path::new(&lease.base_dir)
            .file_name()
            .and_then(|n| n.to_str())
        {
            leased_dirs.insert(name.to_string());
        }
    }

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
    for (gen_id, src8, name, dir_path) in discovered {
        if leased_dirs.contains(&name) || newest_per_source.get(&src8) == Some(&gen_id) {
            retained_generations.push(gen_id);
        } else {
            remove_readonly_tree(&dir_path)
                .with_context(|| format!("cannot remove unleased base {}", dir_path.display()))?;
            removed_generations.push(gen_id);
        }
    }

    Ok(GcBasesReport {
        retained_generations,
        removed_generations,
    })
}

/// Every lease file a registered worktree may carry, in every layout (the state
/// directory depends on `$DEVMAP_HOME`, which the caller's environment may not
/// share with the worktree's own tools). Mirrors the hook's `has_stack_lease`.
fn lease_files_of_worktrees(common_git_dir: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut out: Vec<(PathBuf, PathBuf)> = Vec::new();
    for wt_root in registered_worktrees_of_common_dir(common_git_dir) {
        let candidates = [
            wt_root.join(".devmap/codeintel/stack_lease.json"),
            wt_root.join(".devcouncil/codeintel/stack_lease.json"),
            stack_lease_path(&wt_root),
        ];
        for lease in candidates {
            if !out.iter().any(|(_, seen)| seen == &lease) {
                out.push((wt_root.clone(), lease));
            }
        }
    }
    out
}

/// A worktree git knows of but whose lease cannot be read (unmounted, locked,
/// or its `.git` link points nowhere) may hold a base: treating it as unleased
/// would delete that base. Refuse, naming each one.
fn refuse_unverifiable_worktrees(common_git_dir: &Path) -> Result<()> {
    let entries = match fs::read_dir(common_git_dir.join("worktrees")) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "refusing base GC: cannot list {}",
                    common_git_dir.join("worktrees").display()
                )
            })
        }
    };
    let mut problems = Vec::new();
    for entry in entries {
        let entry = entry?;
        let meta_dir = entry.path();
        if !meta_dir.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if meta_dir.join("locked").exists() {
            problems.push(format!("{name} (locked)"));
            continue;
        }
        match fs::read_to_string(meta_dir.join("gitdir")) {
            Err(err) => problems.push(format!("{name} (gitdir unreadable: {err})")),
            Ok(spec) => {
                let target = meta_dir.join(spec.trim());
                if !target.exists() {
                    problems.push(format!(
                        "{name} (gitdir target {} is missing)",
                        target.display()
                    ));
                }
            }
        }
    }
    if problems.is_empty() {
        return Ok(());
    }
    bail!(
        "refusing base GC: cannot verify the leases of {} registered worktree(s): {}; \
         run `git worktree prune`, or unlock and mount them",
        problems.len(),
        problems.join(", ")
    )
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

/// The per-open check: cheap enough for every MCP call, strong enough to catch
/// a legacy directory, a lease pointing at another base, and a swapped file.
/// The full sha256 is verified when a base is reused at publish.
fn verify_leased_base(
    lease: &StackLease,
    base_dir: &Path,
    base_sqlite: &Path,
    wt_canon: &Path,
) -> Result<BaseManifest> {
    // The lease must belong to the lane being opened, and to its repository:
    // a lease copied in from another lane or another repository names a base
    // (and a root) that are not this lane's.
    let lease_root = Path::new(&lease.worktree_root).canonicalize().ok();
    if lease_root.as_deref() != Some(wt_canon) {
        bail!(
            "stack lease in {} was issued for {} (refusing a lease that belongs to another lane); \
             re-run `devmap seed --stacked`",
            wt_canon.display(),
            lease.worktree_root
        );
    }
    let lane_common = registered_common_git_dir(wt_canon).and_then(|c| c.canonicalize().ok());
    let base_common = base_dir
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .and_then(|c| c.canonicalize().ok());
    if lane_common.is_none() || lane_common != base_common {
        bail!(
            "stack lease in {} points at base {} which belongs to repository {:?}, not this lane's {:?}; \
             re-run `devmap seed --stacked`",
            wt_canon.display(),
            base_dir.display(),
            base_common,
            lane_common
        );
    }
    let manifest = read_base_manifest(base_dir)?;
    let key = BaseKey::of_manifest(&manifest)
        .with_context(|| format!("base manifest in {} is invalid", base_dir.display()))?;
    check_manifest_matches(&manifest, &key, base_dir)?;
    if manifest.generation_id != lease.generation_id {
        bail!(
            "stack lease names generation {} but base {} holds generation {}; re-run `devmap seed --stacked`",
            lease.generation_id,
            base_dir.display(),
            manifest.generation_id
        );
    }
    let dir_name = base_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if dir_name != key.dir_name() {
        bail!(
            "base directory {} is not named for its manifest (expected {}); re-run `devmap seed --stacked`",
            base_dir.display(),
            key.dir_name()
        );
    }
    ensure_sealed(base_dir, base_sqlite)?;
    check_identity_matches(&manifest, base_sqlite)?;
    Ok(manifest)
}

pub struct StackedStore {
    worktree_root: PathBuf,
    lease: StackLease,
    base_sqlite_path: PathBuf,
    base_csr_path: Option<PathBuf>,
    manifest: BaseManifest,
    base: Store,
    csr: Mutex<Option<Arc<CsrGraphReader>>>,
    merged_edges: Mutex<Option<(u32, Arc<GenerationEdges>)>>,
    /// `None` until `delta.sqlite` exists. A read path (hook, CLI, MCP, status)
    /// never creates or migrates it: a missing delta reads as an empty one.
    delta: Option<DeltaStore>,
    delta_writable: bool,
    overlay: WorktreeOverlay,
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
    /// Purely read-only: never provisions, upgrades, or writes to disk.
    pub fn open(worktree_root: &Path) -> Result<Self> {
        let wt_canon = worktree_root
            .canonicalize()
            .with_context(|| format!("cannot canonicalize {}", worktree_root.display()))?;
        let lease_path = stack_lease_path(&wt_canon);
        if !lease_path.is_file() {
            refuse_legacy_base_store_ref(&wt_canon)?;
        }
        let raw_lease = fs::read(&lease_path)
            .with_context(|| format!("cannot read stack lease {}", lease_path.display()))?;
        let lease: StackLease = serde_json::from_slice(&raw_lease)
            .with_context(|| format!("invalid stack lease {}", lease_path.display()))?;
        if lease.format_version != STACK_FORMAT_VERSION {
            bail!(
                "unsupported stack_lease format_version {} in {}",
                lease.format_version,
                lease_path.display()
            );
        }

        let base_dir = PathBuf::from(&lease.base_dir);
        let base_sqlite_path = base_dir.join("base.sqlite");
        // Admission is bound to the file the connection opened by checking its
        // identity (dev, ino, len, mtime, ctime) before AND after the open, and
        // that nothing around it is writable. Residual window: an attacker who
        // can chmod the 0555 directory (the same uid) can swap in a file and
        // forge every one of those between the two stats; that needs the same
        // privilege that could edit the lease and the store, and is outside what
        // an immutable snapshot can defend. A rename-swap of an honest file
        // moves its ctime, so swapping and swapping back is detected too.
        let manifest = verify_leased_base(&lease, &base_dir, &base_sqlite_path, &wt_canon)?;
        let wt_root_str = wt_canon.to_string_lossy().into_owned();
        let base = Store::open_immutable_snapshot(&base_sqlite_path, Some(wt_root_str.clone()))
            .with_context(|| {
                format!(
                    "cannot open immutable base snapshot {}",
                    base_sqlite_path.display()
                )
            })?;
        ensure_sealed(&base_dir, &base_sqlite_path)?;
        check_identity_matches(&manifest, &base_sqlite_path)?;

        let base_csr_path = verify_csr(&manifest, &base_sqlite_path, false)?;
        let delta = DeltaStore::open_read_only(stack_delta_path(&wt_canon))?;
        let overlay =
            build_overlay_from_delta(&wt_root_str, lease.generation_id, &base, delta.as_ref())?;

        Ok(Self {
            worktree_root: wt_canon,
            lease,
            base_sqlite_path,
            base_csr_path,
            manifest,
            base,
            csr: Mutex::new(None),
            merged_edges: Mutex::new(None),
            delta,
            delta_writable: false,
            overlay,
        })
    }

    /// Purely read-only stacked open: returns `Ok(None)` when unleased, `Err`
    /// when a retired `base_store_ref.json` is present (`Owner Stop #2`).
    pub fn open_if_leased(worktree_root: &Path) -> Result<Option<Self>> {
        if !stack_lease_path(worktree_root).is_file() {
            refuse_legacy_base_store_ref(worktree_root)?;
            return Ok(None);
        }
        Self::open(worktree_root).map(Some)
    }

    pub fn worktree_root(&self) -> &Path {
        &self.worktree_root
    }

    pub fn lease(&self) -> &StackLease {
        &self.lease
    }

    pub fn manifest(&self) -> &BaseManifest {
        &self.manifest
    }

    pub fn base_sqlite_path(&self) -> &Path {
        &self.base_sqlite_path
    }

    pub fn base_csr_path(&self) -> Option<&Path> {
        self.base_csr_path.as_deref()
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

    /// The writable delta, created on first use. Only explicit write paths call this.
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

    pub fn has_delta_changes(&self) -> bool {
        !self.overlay.modified_files.is_empty() || !self.overlay.tombstoned_files.is_empty()
    }

    pub fn delta_source_freshness(&self) -> crate::model::SourceFreshness {
        crate::model::SourceFreshness::unverified(format!(
            "answered over immutable base generation {} merged with {} uncommitted stacked delta file(s) ({} deleted)",
            self.lease.generation_id,
            self.overlay.modified_files.len(),
            self.overlay.tombstoned_files.len()
        ))
    }

    /// Lazily map `base.sqlite.csr` on first traversal (`F6` — zero eager edge scan at open).
    pub fn csr_reader(&self) -> Result<Option<Arc<CsrGraphReader>>> {
        let mut guard = self
            .csr
            .lock()
            .map_err(|_| anyhow::anyhow!("StackedStore CSR mutex poisoned"))?;
        if let Some(ref reader) = *guard {
            return Ok(Some(Arc::clone(reader)));
        }
        let Some(ref csr_path) = self.base_csr_path else {
            return Ok(None);
        };
        // Content and identity, once, before the first mapping.
        verify_csr(&self.manifest, &self.base_sqlite_path, true)?;
        let reader = Arc::new(
            CsrGraphReader::open_mmap(csr_path)
                .map_err(|e| anyhow::anyhow!("cannot mmap {}: {e}", csr_path.display()))?,
        );
        *guard = Some(Arc::clone(&reader));
        Ok(Some(reader))
    }

    fn lock_delta_writer(&self) -> Result<(PathBuf, devmap_store::WriterLock)> {
        let delta_path = stack_delta_path(&self.worktree_root);
        let guard = Store::lock_writer_at(&delta_path, Store::WRITER_LOCK_WAIT)?;
        Ok((delta_path, guard))
    }

    fn cleanup_delta_lock(delta_path: &Path, guard: devmap_store::WriterLock) {
        drop(guard);
        let _ = fs::remove_file(devmap_store::seed::sidecar(delta_path, ".lock"));
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
        let (delta_path, guard) = self.lock_delta_writer()?;
        let res = self.record_extracted_file_locked(rel_path, source_bytes, extraction);
        Self::cleanup_delta_lock(&delta_path, guard);
        res
    }

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
        let (delta_path, guard) = self.lock_delta_writer()?;
        let res = self.record_deleted_file_locked(rel_path);
        Self::cleanup_delta_lock(&delta_path, guard);
        res
    }

    fn record_deleted_file_locked(&mut self, rel_path: &str) -> Result<()> {
        let normalized = normalize_rel_path(rel_path)?;
        if self.overlay.is_file_tombstoned(Path::new(&normalized)) {
            return Ok(());
        }
        self.writable_delta()?
            .tombstone_file(&normalized, CAS_TOMBSTONE, now_unix_ms())?;
        self.reload_overlay()
    }

    /// Clear `rel_path` from `delta.sqlite` when it matches the base snapshot again.
    pub fn clear_reverted_file(&mut self, rel_path: &str) -> Result<()> {
        let (delta_path, guard) = self.lock_delta_writer()?;
        let res = self.clear_reverted_file_locked(rel_path);
        Self::cleanup_delta_lock(&delta_path, guard);
        res
    }

    fn clear_reverted_file_locked(&mut self, rel_path: &str) -> Result<()> {
        let normalized = normalize_rel_path(rel_path)?;
        self.writable_delta()?.remove_clean_file(&normalized)?;
        self.reload_overlay()
    }

    /// Re-extract `rel_path` from `<worktree_root>/<rel_path>` using the real
    /// Tree-sitter frontend (`devmap_extract::extract_file`) — never regex!
    #[cfg(feature = "parse")]
    pub fn sync_file_from_disk(&mut self, rel_path: &str) -> Result<()> {
        let (delta_path, guard) = self.lock_delta_writer()?;
        let res = self.sync_file_from_disk_locked(rel_path);
        Self::cleanup_delta_lock(&delta_path, guard);
        res
    }

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
                    if devmap_extract::content_hash(source_str) == base_file.content_hash {
                        if self
                            .overlay
                            .modified_files
                            .contains_key(Path::new(&normalized))
                            || self.overlay.is_file_tombstoned(Path::new(&normalized))
                        {
                            return self.clear_reverted_file_locked(&normalized);
                        }
                        return Ok(());
                    }
                }
                let cas_hash = blake3_hash(&bytes);
                if self
                    .overlay
                    .modified_files
                    .get(Path::new(&normalized))
                    .is_some_and(|existing| existing == &cas_hash)
                {
                    return Ok(());
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
        let (delta_path, guard) = self.lock_delta_writer()?;
        let sync_res = (|| -> Result<(usize, usize, usize)> {
            let (changed_paths, status_deleted_paths) =
                git_changed_paths_and_deletions_against_head(&self.worktree_root, &self.base)?;
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
                } else {
                    let sparse_head_blob = if !status_deleted_paths.contains(rel_path)
                        && devmap_extract::languages::is_indexable_source(rel_path)
                    {
                        git_read_head_blob(&self.worktree_root, rel_path)
                    } else {
                        None
                    };
                    if let Some(bytes) = sparse_head_blob {
                        if let Ok(source_str) = std::str::from_utf8(&bytes) {
                            let matches_base = self.base.latest_file(rel_path)?.is_some_and(|bf| {
                                devmap_extract::content_hash(source_str) == bf.content_hash
                            });
                            if matches_base {
                                if existing_delta.contains(rel_path) {
                                    self.clear_reverted_file_locked(rel_path)?;
                                    cleaned_files += 1;
                                }
                            } else {
                                let cas_hash = blake3_hash(&bytes);
                                let unchanged_delta = self
                                    .overlay
                                    .modified_files
                                    .get(Path::new(rel_path))
                                    .is_some_and(|existing| existing == &cas_hash);
                                if !unchanged_delta {
                                    let extraction =
                                        devmap_extract::extract_file(rel_path, source_str);
                                    self.record_extracted_file_locked(
                                        rel_path,
                                        &bytes,
                                        &extraction,
                                    )?;
                                }
                                synced_files += 1;
                            }
                        }
                    } else if self.base.latest_path_is_indexed(rel_path)?
                        || existing_delta.contains(rel_path)
                    {
                        self.record_deleted_file_locked(rel_path)?;
                        deleted_files += 1;
                    }
                }
            }

            for prev in existing_delta {
                if changed_paths.contains(&prev) {
                    continue;
                }
                let full = self.worktree_root.join(&prev);
                if full.is_file() {
                    let bytes = match fs::read(&full) {
                        Ok(bytes) => bytes,
                        Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(err) => {
                            return Err(err)
                                .with_context(|| format!("cannot read {}", full.display()))
                        }
                    };
                    let src = std::str::from_utf8(&bytes)
                        .with_context(|| format!("{} is not valid UTF-8", full.display()))?;
                    if let Some(base_file) = self.base.latest_file(&prev)? {
                        if devmap_extract::content_hash(src) == base_file.content_hash {
                            self.clear_reverted_file_locked(&prev)?;
                            cleaned_files += 1;
                        }
                    }
                } else if !self.base.latest_path_is_indexed(&prev)? {
                    self.clear_reverted_file_locked(&prev)?;
                    cleaned_files += 1;
                }
            }

            Ok((synced_files, deleted_files, cleaned_files))
        })();
        Self::cleanup_delta_lock(&delta_path, guard);
        let (synced_files, deleted_files, cleaned_files) = sync_res?;

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
        Ok(changed
            .into_iter()
            .filter(|p| {
                !devmap_extract::languages::is_ignored_path(p)
                    && !is_tool_state_path(p)
                    && devmap_extract::languages::is_indexable_source(p)
                    && !self.overlay.modified_files.contains_key(Path::new(p))
                    && !self.overlay.is_file_tombstoned(Path::new(p))
            })
            .collect())
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
    /// and retire `stack_lease.json` + `delta.sqlite` (`OQ2`).
    pub fn materialize_seeded_store(&self) -> Result<PathBuf> {
        self.materialize_seeded_store_waiting(Store::WRITER_LOCK_WAIT)
    }

    /// [`Self::materialize_seeded_store`] with the writer-lock wait named, so a
    /// test can watch the refusal without waiting out a minute.
    pub fn materialize_seeded_store_waiting(&self, wait: std::time::Duration) -> Result<PathBuf> {
        let target_db = store_path(&self.worktree_root);
        if let Some(parent) = target_db.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        // Another writer (a build, a drain) may be placing this store: hold the
        // same lock they hold for the whole promotion.
        let _writer = Store::lock_writer_at(&target_db, wait)?;
        if fs::symlink_metadata(&target_db).is_ok() {
            bail!(
                "refusing to materialize over an existing store {}",
                target_db.display()
            );
        }
        if self.base_csr_path.is_some() {
            verify_csr(&self.manifest, &self.base_sqlite_path, true)?;
        }
        // Retire the lease FIRST, by rename. If that fails nothing else has
        // happened and the call leaves the state it found. Only then place the
        // store; if placing fails, put the lease back and remove what was placed,
        // so a plain store, a lease and a delta are never left together.
        let lease_path = stack_lease_path(&self.worktree_root);
        let tombstone = lease_path.with_extension("json.materializing");
        let had_lease = match fs::rename(&lease_path, &tombstone) {
            Ok(()) => true,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "cannot retire the stack lease {}; nothing was changed",
                        lease_path.display()
                    )
                })
            }
        };
        let dst_csr = csr_path_for_store(&target_db);
        let placed = (|| -> Result<()> {
            promote_immutable_base_to_store(
                &self.base_sqlite_path,
                &target_db,
                &self.worktree_root,
            )?;
            if let Some(ref src_csr) = self.base_csr_path {
                seed_artifact(src_csr, &dst_csr)?;
                if dst_csr.is_file() {
                    set_file_writable(&dst_csr)?;
                }
            }
            Ok(())
        })();
        if let Err(err) = placed {
            let _ = set_file_writable(&dst_csr);
            let _ = fs::remove_file(&dst_csr);
            discard_staged(&target_db);
            if had_lease {
                if let Err(restore) = fs::rename(&tombstone, &lease_path) {
                    return Err(err).with_context(|| {
                        format!(
                            "materialize failed and the lease could not be restored from {} ({restore})",
                            tombstone.display()
                        )
                    });
                }
            }
            return Err(err);
        }
        if had_lease {
            if let Err(err) = fs::remove_file(&tombstone) {
                // The lane is consistent (a store, no lease); only the retired
                // copy of the lease is left behind.
                eprintln!(
                    "devmap: materialized {} but could not delete {}: {err}",
                    target_db.display(),
                    tombstone.display()
                );
            }
        }
        discard_staged(&stack_delta_path(&self.worktree_root));
        Ok(target_db)
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

    /// Return a unified `GenerationEdges` adjacency merging non-masked immutable
    /// base edges with Tree-sitter delta `Contains`, `Calls`, and `Imports` edges
    /// so every `StoreQueryEngine` traversal (`impact`, `trace`, `neighbors`,
    /// `explore`, `affected_tests`, `dependencies`, `ask`) sees worktree edits
    /// and deletions without split-brain (`F1`, `F4`, `F10`).
    pub fn generation_edges_with_id(&self) -> Result<Option<(u32, Arc<GenerationEdges>)>> {
        if !self.has_delta_changes() {
            return Ok(self.base.generation_edges_with_id()?);
        }
        let mut guard = self
            .merged_edges
            .lock()
            .map_err(|_| anyhow::anyhow!("StackedStore merged_edges mutex poisoned"))?;
        if let Some(ref cached) = *guard {
            return Ok(Some((cached.0, Arc::clone(&cached.1))));
        }

        let Some((gen_id, base_index)) = self.base.generation_edges_with_id()? else {
            return Ok(None);
        };

        let mut merged: Vec<StoredEdge> = Vec::with_capacity(base_index.len() + 64);
        let mut seen: HashSet<(String, String, String, String, String)> = HashSet::new();

        for id in 0..base_index.len() as u32 {
            let src_file = base_index.source_file(id);
            let dst_file = base_index.target_file(id);
            let src_sym = base_index.source_symbol(id);
            let dst_sym = base_index.target_symbol(id);
            let src_p = Path::new(src_file);
            let dst_p = Path::new(dst_file);
            if self.overlay.is_file_tombstoned(src_p)
                || self.overlay.modified_files.contains_key(src_p)
                || self.overlay.is_file_tombstoned(dst_p)
                || self.overlay.is_symbol_tombstoned(src_sym)
                || self.overlay.is_symbol_tombstoned(dst_sym)
            {
                continue;
            }
            let edge = base_index.stored_edge(id);
            seen.insert((
                edge.source_file.clone(),
                edge.target_file.clone(),
                edge.source_symbol.clone(),
                edge.target_symbol.clone(),
                edge.edge_kind.clone(),
            ));
            merged.push(edge);
        }

        let mut dirty_paths: Vec<&String> = self.overlay.added_extractions.keys().collect();
        dirty_paths.sort();
        for rel_path in dirty_paths {
            if let Some(ext) = self.overlay.added_extractions.get(rel_path) {
                for edge in resolve_delta_stored_edges(rel_path, ext, &self.overlay, &self.base)? {
                    if seen.insert((
                        edge.source_file.clone(),
                        edge.target_file.clone(),
                        edge.source_symbol.clone(),
                        edge.target_symbol.clone(),
                        edge.edge_kind.clone(),
                    )) {
                        merged.push(edge);
                    }
                }
            }
        }

        let built = Arc::new(
            GenerationEdges::build(Arc::new(merged), base_index.analysis().cloned())?
                .with_generation(gen_id),
        );
        *guard = Some((gen_id, Arc::clone(&built)));
        Ok(Some((gen_id, built)))
    }

    pub fn file_edges(&self, path: &str, min_confidence: f32) -> Result<Option<FileEdges>> {
        if !self.has_delta_changes() {
            return Ok(self.base.file_edges(path, min_confidence)?);
        }
        let min_confidence = devmap_store::checked_min_confidence(min_confidence)?;
        let Some(file) = self.latest_file(path)? else {
            return Ok(None);
        };
        let Some((generation, index)) = self.generation_edges_with_id()? else {
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

    pub fn dead_page(&self, limit: usize) -> Result<Option<DeadPage>> {
        if !self.has_delta_changes() {
            return Ok(self.base.dead_page(limit)?);
        }
        let fetch = limit.saturating_add(self.overlay.modified_files.len() * 8 + 64);
        let Some(mut page) = self.base.dead_page(fetch)? else {
            return Ok(None);
        };
        let edges_opt = self.generation_edges_with_id()?.map(|(_, idx)| idx);
        let before_filter = page.rows.len();
        page.rows.retain(|row| {
            let p = Path::new(&row.file_path);
            let qn = qualify_symbol(&row.file_path, &row.symbol_name);
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
    ) -> Result<Option<CallersPage>> {
        if !self.has_delta_changes() {
            return Ok(self
                .base
                .callers_page(targets, exclude_file, min_confidence)?);
        }
        let min_confidence = devmap_store::checked_min_confidence(min_confidence)?;
        let Some((generation, index)) = self.generation_edges_with_id()? else {
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
        let Some((_, index)) = self.generation_edges_with_id()? else {
            return Ok(Vec::new());
        };
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for id in 0..index.len() as u32 {
            let src = index.source_symbol(id);
            let dst = index.target_symbol(id);
            let matches = src == symbol
                || dst == symbol
                || bare_tail(src) == Some(symbol)
                || bare_tail(dst) == Some(symbol);
            if !matches {
                continue;
            }
            let kind = index.kind_label(id).to_string();
            if seen.insert((src.to_string(), dst.to_string(), kind.clone())) {
                out.push(OverlayEdge {
                    source: src.to_string(),
                    target: dst.to_string(),
                    kind,
                    confidence: (index.confidence(id) * 100.0).round().clamp(0.0, 100.0) as u8,
                });
            }
        }
        Ok(out)
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
            base_dir: self.lease.base_dir.clone(),
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
        if let Ok(mut guard) = self.merged_edges.lock() {
            *guard = None;
        }
        let wt_str = self.worktree_root.to_string_lossy().into_owned();
        self.overlay = build_overlay_from_delta(
            &wt_str,
            self.lease.generation_id,
            &self.base,
            self.delta.as_ref(),
        )?;
        Ok(())
    }
}

fn qualify_symbol(file_path: &str, symbol_name: &str) -> String {
    if file_path.is_empty() {
        return symbol_name.to_string();
    }
    if symbol_name.is_empty() {
        return file_path.to_string();
    }
    if symbol_name == file_path
        || symbol_name
            .strip_prefix(file_path)
            .is_some_and(|rest| rest.starts_with("::"))
    {
        return symbol_name.to_string();
    }
    format!("{file_path}::{symbol_name}")
}

fn bare_tail(symbol: &str) -> Option<&str> {
    let tail = symbol.rsplit("::").next().unwrap_or(symbol);
    let tail = tail.rsplit('.').next().unwrap_or(tail).trim();
    (!tail.is_empty()).then_some(tail)
}

fn specifier_matches_path(specifier: &str, candidate_path: &str) -> bool {
    let norm_spec = specifier
        .trim()
        .trim_start_matches("./")
        .trim_start_matches("../")
        .replace("::", "/");
    if norm_spec.is_empty() {
        return false;
    }
    let cand_no_ext = candidate_path
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(candidate_path);
    cand_no_ext == norm_spec
        || cand_no_ext.ends_with(&format!("/{norm_spec}"))
        || cand_no_ext.ends_with(&format!("/{norm_spec}/mod"))
        || cand_no_ext.ends_with(&format!("/{norm_spec}/index"))
}

fn resolve_delta_stored_edges(
    rel_path: &str,
    ext: &Extraction,
    overlay: &WorktreeOverlay,
    base: &Store,
) -> Result<Vec<StoredEdge>> {
    let mut out = Vec::new();
    let caller_family = LangFamily::from_lang(devmap_extract::languages::detect_language(
        Path::new(rel_path),
    ));

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
        let callee = call.callee_name.trim();
        if callee.is_empty() {
            continue;
        }
        let source_symbol = call
            .caller_symbol
            .as_deref()
            .map(|caller| {
                let want_qual = qualify_symbol(rel_path, caller);
                ext.symbols
                    .iter()
                    .find(|s| {
                        s.qualified_name == caller
                            || s.name == caller
                            || qualify_symbol(rel_path, &s.qualified_name) == want_qual
                    })
                    .map(|s| s.qualified_name.clone())
                    .unwrap_or(want_qual)
            })
            .unwrap_or_else(|| rel_path.to_string());
        let recv =
            call.receiver_expr.as_deref().map(str::trim).filter(|r| {
                !r.is_empty() && !matches!(*r, "self" | "this" | "Self" | "super" | "cls")
            });

        // 1. SameFile resolution (deterministic 1.0).
        let same_file: Vec<&devmap_extract::model::ExtractedSymbol> = ext
            .symbols
            .iter()
            .filter(|s| {
                let tail_ok = s.name == callee || bare_tail(&s.qualified_name) == Some(callee);
                if !tail_ok {
                    return false;
                }
                match recv {
                    Some(r) => {
                        s.qualified_name.contains(&format!("{r}.{callee}"))
                            || s.qualified_name.contains(&format!("{r}::{callee}"))
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
        for sym in base.search_symbols(callee, 48)? {
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
        } else if candidates.len() == 1 {
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
        let mut matched_files: BTreeSet<String> = BTreeSet::new();
        for other_path in overlay.added_extractions.keys() {
            if other_path != rel_path && specifier_matches_path(spec, other_path) {
                matched_files.insert(other_path.clone());
            }
        }
        if matched_files.is_empty() {
            if let Some((_, _, base_files)) = base.latest_scope_inputs()? {
                for (bp, _) in base_files {
                    let p = Path::new(&bp);
                    if bp != rel_path
                        && !overlay.is_file_tombstoned(p)
                        && specifier_matches_path(spec, &bp)
                    {
                        matched_files.insert(bp);
                    }
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
            }
        }
    }

    Ok(out)
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
#[cfg(feature = "parse")]
fn git_changed_paths_and_deletions_against_head(
    worktree_root: &Path,
    base: &Store,
) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
    git_changed_paths_and_deletions_within(worktree_root, base, GIT_BOUNDS.deadline)
}

fn git_changed_paths_within(
    worktree_root: &Path,
    base: &Store,
    deadline: std::time::Duration,
) -> Result<BTreeSet<String>> {
    git_changed_paths_and_deletions_within(worktree_root, base, deadline).map(|(paths, _)| paths)
}

fn git_changed_paths_and_deletions_within(
    worktree_root: &Path,
    base: &Store,
    deadline: std::time::Duration,
) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
    let (mut paths, status_deleted) =
        git_status_paths_and_deletions_within(worktree_root, deadline)?;
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
        &["diff", "--name-only", "-z", &base_head, "HEAD"],
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
    Ok((paths, status_deleted))
}

#[cfg(feature = "parse")]
fn git_read_head_blob(worktree_root: &Path, rel_path: &str) -> Option<Vec<u8>> {
    use devmap_extract::subprocess::{git, run_bounded, Bounds};
    use std::time::Duration;

    let spec = format!("HEAD:{rel_path}");
    let bounds = Bounds {
        deadline: Duration::from_secs(5),
        stdout_cap: (devmap_extract::MAX_SOURCE_BYTES as usize).saturating_add(1),
        stderr_cap: 16 * 1024,
    };
    let mut type_cmd = git(worktree_root, ["cat-file", "-t", &spec]).ok()?;
    let type_out = run_bounded(&mut type_cmd, bounds).ok()?;
    if !type_out.status.success() || type_out.stdout.trim_ascii() != b"blob" {
        return None;
    }
    let mut blob_cmd = git(worktree_root, ["cat-file", "blob", &spec]).ok()?;
    let blob_out = run_bounded(&mut blob_cmd, bounds).ok()?;
    if !blob_out.status.success()
        || (blob_out.stdout.len() as u64) > devmap_extract::MAX_SOURCE_BYTES
    {
        return None;
    }
    Some(blob_out.stdout)
}

/// Streaming sha256 of a file, 64 KiB at a time.
pub fn sha256_file_hex(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = fs::File::open(path)
        .with_context(|| format!("cannot open {} for sha256", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("cannot read {} for sha256", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(crate::digest::hex(&hasher.finalize()))
}

fn sha256_str_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    crate::digest::hex(&Sha256::digest(text.as_bytes()))
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "parse")]
    use super::*;
    #[cfg(feature = "parse")]
    use std::process::Command;
    #[cfg(feature = "parse")]
    use std::sync::atomic::{AtomicU64, Ordering};

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

        // Lazy CSR mmap works without scanning SQLite edges!
        assert!(stacked.csr_reader().unwrap().is_some());
        drop(stacked);

        // Adversarial 2: flipping `base.sqlite` to `0644` MUST cause `StackedStore::open` to fail closed!
        let base_sqlite = PathBuf::from(&lease.base_dir).join("base.sqlite");
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
    fn gc_unused_bases_fails_closed_on_corrupt_lease_and_autorebases_above_dirty_threshold() {
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

        let lease_a_head = git_head(&anchor);
        let lease_a = provision_stacked_worktree(&anchor, &lane_a).unwrap();
        assert_eq!(lease_a.generation_id, 1);

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

        // Both gen-1 (leased by lane-a) and gen-2 (leased by lane-b + newest) are retained!
        let gc1 = gc_unused_bases(&common).unwrap();
        assert_eq!(gc1.retained_generations, vec![1, 2]);
        assert!(gc1.removed_generations.is_empty());

        // Corrupt lane-a's stack_lease.json -> GC MUST FAIL CLOSED without deleting gen-1!
        fs::write(stack_lease_path(&lane_a), b"{corrupt-json").unwrap();
        assert!(
            gc_unused_bases(&common).is_err(),
            "corrupt stack_lease.json must fail closed"
        );
        assert!(base_generation_dir(
            &common,
            &BaseKey::new(1, &lease_a_head, &anchor.canonicalize().unwrap()).unwrap()
        )
        .is_dir());

        // Restore valid lease with low dirty-file threshold (2 files) to exercise auto-rebase!
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
        drop(stacked_a);

        // Now lane-a no longer leases gen-1 -> GC cleanly removes gen-1 while retaining gen-2!
        let gc2 = gc_unused_bases(&common).unwrap();
        assert_eq!(gc2.removed_generations, vec![1]);
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
            lease_main.base_dir, lease_sib.base_dir,
            "two donors with the same generation id must not share a base directory"
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
    fn reuse_refuses_manifest_or_sha_mismatch() {
        let root = temp_repo_dir("reuse-sha");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane_a = add_lane(&anchor, &root, "lane-a");
        let lane_b = add_lane(&anchor, &root, "lane-b");
        let lease = provision_stacked_worktree(&anchor, &lane_a).unwrap();

        let base_dir = PathBuf::from(&lease.base_dir);
        let base_sqlite = base_dir.join("base.sqlite");
        make_tree_writable(&base_dir);
        let mut bytes = fs::read(&base_sqlite).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        fs::write(&base_sqlite, &bytes).unwrap();
        set_file_readonly(&base_sqlite).unwrap();

        let err = provision_stacked_worktree(&anchor, &lane_b)
            .expect_err("a base whose bytes no longer hash to the manifest must not be reused")
            .to_string();
        assert!(err.contains("sha256"), "unexpected error: {err}");

        // A manifest that names another HEAD under the expected directory name is refused too.
        let manifest_path = base_dir.join("manifest.json");
        let mut manifest: BaseManifest =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest.head_sha = "0".repeat(40);
        set_file_writable(&manifest_path).unwrap();
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let err = provision_stacked_worktree(&anchor, &lane_b)
            .expect_err("a manifest for another head must not be reused")
            .to_string();
        assert!(err.contains("head_sha"), "unexpected error: {err}");
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
        let base_dir = PathBuf::from(&lease.base_dir);
        let base_sqlite = base_dir.join("base.sqlite");

        // Without the explicit chmod, the published directory cannot be swapped into.
        let impostor = root.join("impostor.sqlite");
        fs::copy(&base_sqlite, &impostor).unwrap();
        set_file_readonly(&impostor).unwrap();
        assert!(
            fs::rename(&impostor, &base_sqlite).is_err() || cfg!(not(unix)),
            "published base directory must not accept a rename"
        );

        // With it, an identical-looking, valid, 0444 file under another inode is still refused.
        make_tree_writable(&base_dir);
        fs::rename(&impostor, &base_sqlite).unwrap();
        set_file_readonly(&base_sqlite).unwrap();
        set_dir_mode(&base_dir, 0o555).unwrap();
        let err = StackedStore::open(&lane)
            .expect_err("a swapped base.sqlite must be refused")
            .to_string();
        assert!(
            err.contains("replaced or modified"),
            "unexpected error: {err}"
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
    fn gc_refuses_unverifiable_worktrees_and_waits_on_the_bases_lock() {
        let root = temp_repo_dir("gc-unverifiable");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane_a = add_lane(&anchor, &root, "lane-a");
        let lane_b = add_lane(&anchor, &root, "lane-b");
        provision_stacked_worktree(&anchor, &lane_a).unwrap();

        // Advance the donor to generation 2 and lease it from lane-b, so gen 1
        // is retained only by lane-a's lease.
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

        // lane-a becomes unreachable: unmounted and locked. Its lease cannot be read.
        let moved = root.join("lane-a.unmounted");
        fs::rename(&lane_a, &moved).unwrap();
        fs::write(common.join("worktrees/lane-a/locked"), "unmounted\n").unwrap();

        let err = gc_unused_bases(&common)
            .expect_err("an unverifiable worktree must stop the collection")
            .to_string();
        assert!(err.contains("lane-a"), "must name the worktree: {err}");
        let bases: Vec<_> = fs::read_dir(bases_root_dir(&common))
            .unwrap()
            .flatten()
            .filter(|e| parse_base_dir_name(&e.file_name().to_string_lossy()).is_some())
            .collect();
        assert_eq!(bases.len(), 2, "no base may be deleted: {bases:?}");

        // Mounted again, a held bases lock is a bounded refusal, not a race.
        fs::rename(&moved, &lane_a).unwrap();
        fs::remove_file(common.join("worktrees/lane-a/locked")).unwrap();
        let held = lock_bases(&common, std::time::Duration::from_secs(5)).unwrap();
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
    fn csr_reader_refuses_a_tampered_sidecar() {
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
        let base_dir = PathBuf::from(&lease.base_dir);
        let csr = csr_path_for_store(base_dir.join("base.sqlite"));
        assert!(csr.is_file(), "fixture must have a csr sidecar");
        let honest_len = fs::metadata(&csr).unwrap().len();
        let other_csr = csr_path_for_store(store_path(&other));
        assert!(other_csr.is_file());
        make_tree_writable(&base_dir);
        fs::copy(&other_csr, &csr).unwrap();
        let tampered_len = fs::metadata(&csr).unwrap().len();
        set_file_readonly(&csr).unwrap();
        set_file_readonly(&base_dir.join("base.sqlite")).unwrap();
        set_file_readonly(&base_dir.join("manifest.json")).unwrap();
        set_dir_mode(&base_dir, 0o555).unwrap();
        assert_ne!(honest_len, tampered_len, "fixture: CSRs must differ");
        let served = StackedStore::open(&lane).and_then(|s| s.csr_reader().map(|r| r.is_some()));
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
        let base_dir = PathBuf::from(&lease.base_dir);
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
        set_dir_mode(&base_dir, 0o555).unwrap();

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
        let base_dir = PathBuf::from(&lease.base_dir);
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
        let base_dir = PathBuf::from(&lease.base_dir);
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

    /// 12: missing and truncated manifests are refused at open.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn open_refuses_a_missing_truncated_or_empty_manifest() {
        let root = temp_repo_dir("atk12");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        let base_dir = PathBuf::from(&lease.base_dir);
        let manifest = base_dir.join("manifest.json");
        let good = fs::read(&manifest).unwrap();
        make_tree_writable(&base_dir);
        fs::write(&manifest, &good[..good.len() / 2]).unwrap();
        assert!(
            StackedStore::open(&lane).is_err(),
            "truncated manifest accepted"
        );
        fs::remove_file(&manifest).unwrap();
        assert!(
            StackedStore::open(&lane).is_err(),
            "missing manifest accepted"
        );
        // an empty manifest next to a sha-less lease must not open either
        fs::write(&manifest, b"{}").unwrap();
        assert!(
            StackedStore::open(&lane).is_err(),
            "empty manifest accepted"
        );
        let _ = remove_readonly_tree(&root);
    }

    /// 4: a leased older base survives GC after the donor advances; a lane never loses its base.
    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn gc_keeps_leased_base_and_newest_by_generation() {
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
        assert_ne!(lease_a.base_dir, lease_b.base_dir);
        let common = registered_common_git_dir(&anchor.canonicalize().unwrap()).unwrap();
        let rep = gc_unused_bases(&common).unwrap();
        assert!(
            rep.removed_generations.is_empty(),
            "GC removed a leased/newest base: {rep:?}"
        );
        StackedStore::open(&lane_a).expect("lane-a lost its base after GC");
        StackedStore::open(&lane_b).expect("lane-b lost its base after GC");
        // now delete lane-a's lease: its base becomes unleased and older -> removable; lane-b must remain
        fs::remove_file(stack_lease_path(lane_a.canonicalize().unwrap())).unwrap();
        let rep = gc_unused_bases(&common).unwrap();
        assert_eq!(rep.removed_generations.len(), 1, "{rep:?}");
        StackedStore::open(&lane_b).expect("lane-b lost its base after the second GC");
        let _ = remove_readonly_tree(&root);
    }

    /// 9: materialize whose lease removal fails must not leave a store AND a lease.
    #[cfg(all(feature = "parse", target_os = "macos"))]
    #[test]
    fn materialize_failure_leaves_the_pre_call_state() {
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
        let delta_exists = stack_delta_path(&canon).exists();
        let _ = Command::new("chflags").arg("nouchg").arg(&lease).status();
        drop(stacked);
        assert!(res.is_err(), "fixture: lease removal must have failed");
        assert!(
            !(store_exists && lease_exists),
            "after a failed materialize the lane holds BOTH a plain store ({store_exists}) and a stack lease ({lease_exists}); delta present={delta_exists}"
        );
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
        let _ = remove_readonly_tree(&root);
    }

    #[cfg(all(feature = "parse", unix))]
    #[test]
    fn open_refuses_an_oversized_manifest() {
        let root = temp_repo_dir("big-manifest");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane = add_lane(&anchor, &root, "lane");
        let lease = provision_stacked_worktree(&anchor, &lane).unwrap();
        let base_dir = PathBuf::from(&lease.base_dir);
        make_tree_writable(&base_dir);
        fs::write(base_dir.join("manifest.json"), vec![b' '; 2 << 20]).unwrap();
        set_file_readonly(&base_dir.join("manifest.json")).unwrap();
        set_dir_mode(&base_dir, 0o555).unwrap();
        let err = StackedStore::open(&lane)
            .expect_err("a 2 MiB manifest must be refused, not slurped")
            .to_string();
        assert!(err.contains("larger than"), "unexpected error: {err}");
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
        let base_dir = PathBuf::from(&lease.base_dir);
        let csr = csr_path_for_store(base_dir.join("base.sqlite"));
        make_tree_writable(&base_dir);
        let mut bytes = fs::read(&csr).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        fs::write(&csr, &bytes).unwrap();
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
    fn provision_replaces_an_unleased_base_with_a_bad_sha_and_names_the_recovery_for_a_leased_one()
    {
        let root = temp_repo_dir("bad-sha");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);
        let lane_a = add_lane(&anchor, &root, "lane-a");
        let lane_b = add_lane(&anchor, &root, "lane-b");
        let lease = provision_stacked_worktree(&anchor, &lane_a).unwrap();
        let base_dir = PathBuf::from(&lease.base_dir);
        let base_sqlite = base_dir.join("base.sqlite");
        make_tree_writable(&base_dir);
        let mut bytes = fs::read(&base_sqlite).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        fs::write(&base_sqlite, &bytes).unwrap();
        set_file_readonly(&base_sqlite).unwrap();
        set_dir_mode(&base_dir, 0o555).unwrap();

        // Leased by lane-a: a real conflict, and the error carries the way out.
        let err = provision_stacked_worktree(&anchor, &lane_b)
            .expect_err("a leased base with a bad sha must not be replaced")
            .to_string();
        assert!(err.contains("chmod -R u+w"), "no recovery command: {err}");

        // Unleased (lane-a's lease is gone): replaced under the bases lock.
        fs::remove_file(stack_lease_path(lane_a.canonicalize().unwrap())).unwrap();
        provision_stacked_worktree(&anchor, &lane_b).expect("an unleased bad base is replaced");
        StackedStore::open(&lane_b).expect("the replacement base opens");
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

    #[cfg(feature = "parse")]
    #[test]
    fn legacy_base_store_ref_is_cleanly_refused_without_silent_upgrade() {
        let root = temp_repo_dir("legacy-ref-refused");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);

        let lane_a = add_lane(&anchor, &root, "lane-a");
        let lane_b = add_lane(&anchor, &root, "lane-b");

        // Unrelated repo anchor_repo_root is recognized as unverified, still refused cleanly.
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
        assert!(
            legacy_base_store_anchor_root(&lane_a).is_none(),
            "unrelated anchor_repo_root must not resolve as a sibling anchor"
        );
        let err_unrelated = StackedStore::open_if_leased(&lane_a)
            .expect_err("legacy base_store_ref.json must be refused on open_if_leased")
            .to_string();
        assert!(
            err_unrelated.contains("devmap seed --stacked --from"),
            "expected explicit upgrade command in refusal: {err_unrelated}"
        );
        assert!(
            !stack_lease_path(lane_a.canonicalize().unwrap()).exists(),
            "open_if_leased must never silently create stack_lease.json"
        );

        // Sibling registered anchor includes the exact anchor path in the upgrade command.
        fs::write(
            &ref_a,
            serde_json::to_vec(&serde_json::json!({
                "anchor_repo_root": anchor.canonicalize().unwrap(),
            }))
            .unwrap(),
        )
        .unwrap();
        let anchor_canon = anchor.canonicalize().unwrap();
        assert_eq!(
            legacy_base_store_anchor_root(&lane_a),
            Some(anchor_canon.clone())
        );
        let err_a = StackedStore::open_if_leased(&lane_a)
            .expect_err("sibling legacy base_store_ref.json must still refuse without auto-upgrade")
            .to_string();
        assert!(
            err_a.contains(&format!(
                "devmap seed --stacked --from {}",
                anchor_canon.display()
            )),
            "expected concrete upgrade command in refusal: {err_a}"
        );
        assert!(!stack_lease_path(lane_a.canonicalize().unwrap()).exists());

        // Alternate `.devmap/codeintel/base_store_ref.json` path is also refused cleanly.
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
        let err_b = StackedStore::open_if_leased(&lane_b)
            .expect_err("alternate .devmap legacy pointer must also be refused")
            .to_string();
        assert!(err_b.contains("devmap seed --stacked --from"));
        assert!(!stack_lease_path(lane_b.canonicalize().unwrap()).exists());

        let _ = remove_readonly_tree(&root);
    }

    #[cfg(feature = "parse")]
    #[test]
    fn store_query_engine_with_stacked_answers_all_graph_and_search_surfaces_over_delta_edits_and_deletions(
    ) {
        let root = temp_repo_dir("full-engine-surfaces");
        let anchor = root.join("anchor");
        fs::create_dir_all(anchor.join("src")).unwrap();
        fs::create_dir_all(anchor.join("tests")).unwrap();

        fs::write(
            anchor.join("src/db.rs"),
            "pub fn run_query() -> i32 { 42 }\npub fn legacy_literal_holder() -> &'static str { \"OLD_SECRET_TAG\" }\n",
        )
        .unwrap();
        fs::write(
            anchor.join("src/dead_mod.rs"),
            "pub(crate) fn orphan_fn() -> i32 { 0 }\npub(crate) fn soon_called() -> i32 { 7 }\n",
        )
        .unwrap();
        fs::write(
            anchor.join("src/obsolete.rs"),
            "pub(crate) fn vanished_api() -> &'static str { \"VANISHED_LITERAL\" }\n",
        )
        .unwrap();
        fs::write(
            anchor.join("src/lib.rs"),
            "pub mod db;\npub mod dead_mod;\npub mod obsolete;\npub fn handle_req() -> i32 { db::run_query() }\n",
        )
        .unwrap();
        fs::write(
            anchor.join("tests/smoke.rs"),
            "#[test]\nfn test_handle_req() {\n    let _ = handle_req();\n}\n",
        )
        .unwrap();

        fs::write(anchor.join(".gitignore"), ".devmap/\n.devcouncil/\n").unwrap();

        run_git_ok(&anchor, &["init", "-b", "main"]);
        run_git_ok(&anchor, &["config", "user.email", "t@t"]);
        run_git_ok(&anchor, &["config", "user.name", "t"]);
        run_git_ok(&anchor, &["add", "."]);
        run_git_ok(&anchor, &["commit", "-m", "initial"]);

        {
            let db_path = store_path(&anchor);
            fs::create_dir_all(db_path.parent().unwrap()).unwrap();
            let store = Store::open(&db_path).unwrap();
            let anchor_canon = anchor.canonicalize().unwrap();
            store.bind_repo_root(&anchor_canon).unwrap();
            let rel_paths = [
                "src/db.rs",
                "src/dead_mod.rs",
                "src/obsolete.rs",
                "src/lib.rs",
                "tests/smoke.rs",
            ];
            let exts: Vec<Extraction> = rel_paths
                .iter()
                .map(|p| {
                    devmap_extract::extract_file(p, &fs::read_to_string(anchor.join(p)).unwrap())
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
                    devmap_store::GenerationWriteOpts {
                        repo_root: Some(anchor_canon.to_string_lossy().into_owned()),
                        ..devmap_store::GenerationWriteOpts::default()
                    },
                    &git_head(&anchor),
                )
                .unwrap();
            let _ = store.write_latest_csr_sidecar().unwrap();
        }

        let lane = add_lane(&anchor, &root, "lane");
        provision_stacked_worktree(&anchor, &lane).unwrap();

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

        // Explicit write step syncs worktree into delta.sqlite under WriterLock.
        {
            let mut writer = StackedStore::open(&lane).unwrap();
            writer.sync_worktree().unwrap();
        }

        let lane_canon = lane.canonicalize().unwrap();
        let delta_file = stack_delta_path(&lane_canon);
        let lease_file = stack_lease_path(&lane_canon);
        let delta_before = fs::read(&delta_file).unwrap();
        let lease_before = fs::read(&lease_file).unwrap();

        // Subsequent dirty edit NOT synced yet: read-only open + queries must NOT touch delta.sqlite!
        fs::write(
            lane.join("src/unsynced_scratch.rs"),
            "pub fn unsynced_only_on_disk() {}\n",
        )
        .unwrap();

        let stacked = StackedStore::open(&lane).unwrap();
        let engine =
            crate::StoreQueryEngine::new(stacked.base_store()).with_stacked(Some(&stacked));

        let search_new = engine
            .search(crate::Request {
                query: "stream_rows".to_string(),
                token_budget: 4000,
                min_confidence: 0.0,
                max_depth: 1,
            })
            .unwrap();
        assert_eq!(search_new.items.len(), 1);
        assert!(search_new.items[0]
            .source_span
            .contains("LIVE_DELTA_LITERAL"));

        let search_unsynced = engine
            .search(crate::Request {
                query: "unsynced_only_on_disk".to_string(),
                token_budget: 4000,
                min_confidence: 0.0,
                max_depth: 1,
            })
            .unwrap();
        assert!(
            search_unsynced.items.is_empty(),
            "read-only open must not silently ingest unsynced dirty files"
        );

        let search_deleted = engine
            .search(crate::Request {
                query: "vanished_api".to_string(),
                token_budget: 4000,
                min_confidence: 0.0,
                max_depth: 1,
            })
            .unwrap();
        assert!(search_deleted.items.is_empty());

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

        let imp = engine
            .impact(crate::Request {
                query: "run_query".to_string(),
                token_budget: 4000,
                min_confidence: 0.0,
                max_depth: 5,
            })
            .unwrap();
        assert!(imp
            .items
            .iter()
            .any(|e| e.source_symbol.contains("stream_rows")));
        assert!(imp
            .items
            .iter()
            .any(|e| e.source_symbol.contains("test_stream_pipeline")));

        let layered = engine
            .impact_layered(crate::Request {
                query: "run_query".to_string(),
                token_budget: 4000,
                min_confidence: 0.0,
                max_depth: 5,
            })
            .unwrap();
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

        let deps_api = engine
            .dependencies(crate::Request {
                query: "src/api.rs".to_string(),
                token_budget: 4000,
                min_confidence: 0.0,
                max_depth: 1,
            })
            .unwrap();
        assert!(deps_api.items.iter().any(|e| e.target_file == "src/db.rs"));

        let aff = engine
            .affected_tests(&["src/db.rs".to_string()], 4000, 0.0, 5)
            .unwrap();
        assert!(aff
            .tests
            .items
            .iter()
            .any(|t| t.symbols.iter().any(|s| s.contains("test_stream_pipeline"))));

        // Skeleton works with both repo-relative and worktree-absolute paths!
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
        assert!(dead_names.contains(&"orphan_fn"));
        assert!(!dead_names.contains(&"soon_called"));
        assert!(!dead_names.contains(&"vanished_api"));

        let lit_live = engine.literals("LIVE_DELTA_LITERAL", false, 4000).unwrap();
        assert_eq!(lit_live.items.len(), 1);
        let lit_old = engine.literals("OLD_SECRET_TAG", false, 4000).unwrap();
        assert!(lit_old.items.is_empty());
        let lit_vanished = engine.literals("VANISHED_LITERAL", false, 4000).unwrap();
        assert!(lit_vanished.items.is_empty());

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
            "queries must not mutate a single byte of delta.sqlite"
        );
        assert_eq!(
            fs::read(&lease_file).unwrap(),
            lease_before,
            "queries must not mutate stack_lease.json"
        );
        assert!(!delta_file.with_extension("sqlite-wal").exists());
        assert!(!delta_file.with_extension("sqlite-shm").exists());
        assert!(!delta_file.with_extension("sqlite.lock").exists());

        let _ = remove_readonly_tree(&root);
    }
}
