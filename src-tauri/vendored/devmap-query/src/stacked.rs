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
    registered_common_git_dir, registered_worktrees_of_common_dir, repo_map_path,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaseManifest {
    pub format_version: u32,
    pub generation_id: u32,
    pub schema_version: i32,
    pub source_repo_root: String,
    pub created_at_unix_ms: u64,
    pub base_sqlite_sha256: String,
    #[serde(default)]
    pub has_csr: bool,
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

pub fn bases_root_dir(common_git_dir: &Path) -> PathBuf {
    common_git_dir.join("devmap").join("bases")
}

pub fn base_generation_dir(common_git_dir: &Path, generation_id: u32) -> PathBuf {
    bases_root_dir(common_git_dir).join(format!("gen-{generation_id}"))
}

/// Publish or reuse an immutable `chmod 0444` base snapshot at
/// `<common_git_dir>/devmap/bases/gen-<G>/` from `source_root`'s latest
/// generation (`OQ1`).
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
    let generation_id = {
        let store = Store::open_read_only(&source_db)
            .with_context(|| format!("cannot read donor store {}", source_db.display()))?;
        store.latest_generation_id()?.ok_or_else(|| {
            anyhow::anyhow!(
                "donor store {} has no indexed generation",
                source_db.display()
            )
        })?
    };

    let bases_dir = bases_root_dir(&common_git_dir);
    fs::create_dir_all(&bases_dir)
        .with_context(|| format!("cannot create {}", bases_dir.display()))?;
    let final_dir = base_generation_dir(&common_git_dir, generation_id);
    if let Some(existing) = read_verified_base_manifest(&final_dir)? {
        return Ok((final_dir, existing));
    }

    let stage_dir = bases_dir.join(format!(
        ".stage-gen-{generation_id}-{}-{}",
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
        let sha256 = sha256_file_hex(&staged_sqlite)?;

        let src_csr = csr_path_for_store(&source_db);
        let staged_csr = csr_path_for_store(&staged_sqlite);
        let has_csr = if src_csr.is_file() {
            seed_artifact(&src_csr, &staged_csr)?.is_some() && {
                set_file_readonly(&staged_csr)?;
                true
            }
        } else {
            false
        };

        let manifest = BaseManifest {
            format_version: STACK_FORMAT_VERSION,
            generation_id,
            schema_version: CURRENT_SCHEMA_VERSION,
            source_repo_root: source_canon.to_string_lossy().into_owned(),
            created_at_unix_ms: now_unix_ms(),
            base_sqlite_sha256: sha256,
            has_csr,
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
        Ok(()) => Ok((final_dir, manifest)),
        Err(err) => {
            let _ = remove_readonly_tree(&stage_dir);
            if let Some(existing) = read_verified_base_manifest(&final_dir)? {
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

fn read_verified_base_manifest(base_dir: &Path) -> Result<Option<BaseManifest>> {
    let manifest_path = base_dir.join("manifest.json");
    let base_sqlite = base_dir.join("base.sqlite");
    if !manifest_path.is_file() || !base_sqlite.is_file() {
        return Ok(None);
    }
    let raw = fs::read(&manifest_path)
        .with_context(|| format!("cannot read {}", manifest_path.display()))?;
    let manifest: BaseManifest = serde_json::from_slice(&raw)
        .with_context(|| format!("invalid base manifest {}", manifest_path.display()))?;
    // Verify the snapshot satisfies Store::open_immutable_snapshot invariants (`0444`, no sidecars).
    let _ = Store::open_immutable_snapshot(&base_sqlite, None)
        .with_context(|| format!("published base snapshot {} failed admission", base_sqlite.display()))?;
    Ok(Some(manifest))
}

#[derive(Debug, Deserialize)]
struct LegacyBaseStoreRef {
    #[serde(default)]
    anchor_repo_root: Option<String>,
    #[serde(default, alias = "base_store_path")]
    anchor_db_path: Option<String>,
}

/// Inspect `<worktree_root>/<state>/codeintel/base_store_ref.json` (if present) and
/// return the bidirectionally verified registered anchor checkout root when its
/// store exists (`F1`, `F2`).
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

fn acquire_gc_lock(common_git_dir: &Path, exclusive: bool) -> Result<fs::File> {
    let bases_dir = bases_root_dir(common_git_dir);
    fs::create_dir_all(&bases_dir)
        .with_context(|| format!("cannot create {}", bases_dir.display()))?;
    let lock_path = bases_dir.join(".gc.lock");
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("cannot open base GC lock {}", lock_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let op = if exclusive {
            libc::LOCK_EX
        } else {
            libc::LOCK_SH
        };
        loop {
            let rc = unsafe { libc::flock(file.as_raw_fd(), op) };
            if rc == 0 {
                break;
            }
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err)
                .with_context(|| format!("cannot flock base GC lock {}", lock_path.display()));
        }
    }
    #[cfg(not(unix))]
    let _ = exclusive;
    Ok(file)
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
    let common_git_dir = registered_common_git_dir(&source_canon).ok_or_else(|| {
        anyhow::anyhow!(
            "{} is not a registered git checkout; refusing stacked provisioning",
            source_canon.display()
        )
    })?;

    // Hold shared GC lock from base snapshot publication through lease commit so
    // a concurrent `gc_unused_bases` (`LOCK_EX`) cannot delete `gen-G` in between.
    let _gc_guard = acquire_gc_lock(&common_git_dir, false)?;

    let (base_dir, manifest) = publish_base_snapshot(&source_canon)?;
    let delta_path = stack_delta_path(&wt_canon);
    let _delta = DeltaStore::open(&delta_path)?;

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
    drop(_gc_guard);

    seed_derived_artifacts_and_reuse_caches(&source_canon, &wt_canon)?;

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
/// worktree has a moved/missing `gitdir` target or carries an unreadable or
/// malformed `stack_lease.json` (`OQ2`).
pub fn gc_unused_bases(common_git_dir: &Path) -> Result<GcBasesReport> {
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

    let _gc_guard = acquire_gc_lock(&common_canon, true)?;

    // Fail closed if ANY `<common>/worktrees/<lane>/gitdir` entry cannot be read
    // or points to a moved/missing path on disk (e.g. a worktree moved before
    // `git worktree repair`, which `registered_worktrees_of_common_dir` would
    // otherwise skip).
    let worktrees_meta = common_canon.join("worktrees");
    if worktrees_meta.is_dir() {
        for entry in fs::read_dir(&worktrees_meta)
            .with_context(|| format!("refusing base GC: cannot read {}", worktrees_meta.display()))?
        {
            let entry = entry?;
            let ft = entry.file_type()?;
            if !ft.is_dir() {
                continue;
            }
            let wt_meta_dir = entry.path();
            let gitdir_file = wt_meta_dir.join("gitdir");
            let reg_spec = fs::read_to_string(&gitdir_file).with_context(|| {
                format!(
                    "refusing base GC: cannot read registered worktree pointer {}",
                    gitdir_file.display()
                )
            })?;
            let reg_spec = reg_spec.trim();
            if reg_spec.is_empty() || reg_spec.contains('\0') {
                bail!(
                    "refusing base GC: empty or invalid gitdir pointer in {}",
                    gitdir_file.display()
                );
            }
            let gitlink = wt_meta_dir.join(reg_spec).canonicalize().with_context(|| {
                format!(
                    "refusing base GC: registered worktree pointer {} -> {:?} cannot be resolved (run `git worktree repair` or `git worktree prune` first)",
                    gitdir_file.display(),
                    reg_spec
                )
            })?;
            let Some(wt_root) = gitlink.parent() else {
                bail!(
                    "refusing base GC: gitlink {} has no parent directory",
                    gitlink.display()
                );
            };
            if registered_common_git_dir(wt_root).as_deref() != Some(common_canon.as_path()) {
                bail!(
                    "refusing base GC: worktree {} failed bidirectional registration verification",
                    wt_root.display()
                );
            }
        }
    }

    let mut leased_generations = BTreeSet::new();
    for wt_root in registered_worktrees_of_common_dir(&common_canon) {
        let lease_file = stack_lease_path(&wt_root);
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
        leased_generations.insert(lease.generation_id);
    }

    let mut discovered: Vec<(u32, PathBuf)> = Vec::new();
    for entry in fs::read_dir(&bases_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if let Some(num) = name_str.strip_prefix("gen-") {
            if let Ok(gen_id) = num.parse::<u32>() {
                discovered.push((gen_id, entry.path()));
            }
        }
    }
    discovered.sort_by_key(|(g, _)| *g);
    if let Some((newest_gen, _)) = discovered.last() {
        leased_generations.insert(*newest_gen);
    }

    let mut retained_generations = Vec::new();
    let mut removed_generations = Vec::new();
    for (gen_id, dir_path) in discovered {
        if leased_generations.contains(&gen_id) {
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

pub struct StackedStore {
    worktree_root: PathBuf,
    lease: StackLease,
    base_sqlite_path: PathBuf,
    base_csr_path: Option<PathBuf>,
    base: Store,
    csr: Mutex<Option<Arc<CsrGraphReader>>>,
    merged_edges: Mutex<Option<(u32, Arc<GenerationEdges>)>>,
    delta: DeltaStore,
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
    /// Auto-upgrade a legacy `base_store_ref.json` worktree into a Glean-style
    /// `StackedStore` (`stack_lease.json` + `delta.sqlite`) (`F1`, `F2`).
    pub fn upgrade_legacy_base_store_ref_if_needed(worktree_root: &Path) -> Result<bool> {
        if store_path(worktree_root).is_file() || stack_lease_path(worktree_root).is_file() {
            return Ok(false);
        }
        if find_legacy_base_store_ref(worktree_root).is_none() {
            return Ok(false);
        }
        let Some(anchor_root) = legacy_base_store_anchor_root(worktree_root) else {
            return Ok(false);
        };
        provision_stacked_worktree(&anchor_root, worktree_root)?;
        Ok(true)
    }

    /// Open the stacked worktree store declared by `<worktree_root>/<state>/codeintel/stack_lease.json`
    /// (auto-upgrading legacy `base_store_ref.json` first when present).
    pub fn open(worktree_root: &Path) -> Result<Self> {
        let wt_canon = worktree_root
            .canonicalize()
            .with_context(|| format!("cannot canonicalize {}", worktree_root.display()))?;
        let _ = Self::upgrade_legacy_base_store_ref_if_needed(&wt_canon)?;
        let lease_path = stack_lease_path(&wt_canon);
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
        let wt_root_str = wt_canon.to_string_lossy().into_owned();
        let base =
            Store::open_immutable_snapshot(&base_sqlite_path, Some(wt_root_str.clone()))
                .with_context(|| {
                    format!(
                        "cannot open immutable base snapshot {}",
                        base_sqlite_path.display()
                    )
                })?;

        let csr_candidate = csr_path_for_store(&base_sqlite_path);
        let base_csr_path = csr_candidate.is_file().then_some(csr_candidate);
        let delta = DeltaStore::open(stack_delta_path(&wt_canon))?;
        let overlay = build_overlay_from_delta(&wt_root_str, lease.generation_id, &base, &delta)?;

        Ok(Self {
            worktree_root: wt_canon,
            lease,
            base_sqlite_path,
            base_csr_path,
            base,
            csr: Mutex::new(None),
            merged_edges: Mutex::new(None),
            delta,
            overlay,
        })
    }

    pub fn open_if_leased(worktree_root: &Path) -> Result<Option<Self>> {
        let _ = Self::upgrade_legacy_base_store_ref_if_needed(worktree_root)?;
        if !stack_lease_path(worktree_root).is_file() {
            return Ok(None);
        }
        Self::open(worktree_root).map(Some)
    }

    /// Open and synchronize uncommitted/diverged git paths in `worktree_root`
    /// with Tree-sitter. Returns `Ok(None)` when `worktree_root` is not leased
    /// OR when `sync_worktree()` auto-rebased past threshold into a standalone
    /// `devmap.sqlite` (so the caller seamlessly opens standalone `Store`).
    #[cfg(feature = "parse")]
    pub fn open_synced(worktree_root: &Path) -> Result<Option<Self>> {
        let Some(mut stacked) = Self::open_if_leased(worktree_root)? else {
            return Ok(None);
        };
        let report = stacked.sync_worktree()?;
        if report.rebased_to_standalone.is_some() {
            return Ok(None);
        }
        Ok(Some(stacked))
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

    pub fn delta_store(&self) -> &DeltaStore {
        &self.delta
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
        let reader = Arc::new(
            CsrGraphReader::open_mmap(csr_path)
                .map_err(|e| anyhow::anyhow!("cannot mmap {}: {e}", csr_path.display()))?,
        );
        *guard = Some(Arc::clone(&reader));
        Ok(Some(reader))
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
        let normalized = normalize_rel_path(rel_path)?;
        let cas_hash = blake3_hash(source_bytes);
        let edges = extract_delta_edges(&normalized, extraction);
        self.delta.upsert_extracted_file(
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
        let normalized = normalize_rel_path(rel_path)?;
        if self.overlay.is_file_tombstoned(Path::new(&normalized)) {
            return Ok(());
        }
        self.delta
            .tombstone_file(&normalized, CAS_TOMBSTONE, now_unix_ms())?;
        self.reload_overlay()
    }

    /// Clear `rel_path` from `delta.sqlite` when it matches the base snapshot again.
    pub fn clear_reverted_file(&mut self, rel_path: &str) -> Result<()> {
        let normalized = normalize_rel_path(rel_path)?;
        self.delta.remove_clean_file(&normalized)?;
        self.reload_overlay()
    }

    /// Re-extract `rel_path` from `<worktree_root>/<rel_path>` using the real
    /// Tree-sitter frontend (`devmap_extract::extract_file`) — never regex!
    #[cfg(feature = "parse")]
    pub fn sync_file_from_disk(&mut self, rel_path: &str) -> Result<()> {
        let normalized = normalize_rel_path(rel_path)?;
        if devmap_extract::languages::is_ignored_path(&normalized) {
            return Ok(());
        }
        let full = self.worktree_root.join(&normalized);
        match fs::symlink_metadata(&full) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                self.record_deleted_file(&normalized)
            }
            Err(err) => Err(err).with_context(|| format!("cannot stat {}", full.display())),
            Ok(meta) if !meta.is_file() => self.record_deleted_file(&normalized),
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
                        if self.overlay.modified_files.contains_key(Path::new(&normalized))
                            || self.overlay.is_file_tombstoned(Path::new(&normalized))
                        {
                            return self.clear_reverted_file(&normalized);
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
                self.record_extracted_file(&normalized, &bytes, &extraction)
            }
        }
    }

    /// Synchronize uncommitted/diverged git paths in `worktree_root` into `delta.sqlite`
    /// with real Tree-sitter extraction, auto-rebasing to a standalone CoW
    /// `devmap.sqlite` when thresholds (`250` dirty files / `16 MiB`) are exceeded.
    #[cfg(feature = "parse")]
    pub fn sync_worktree(&mut self) -> Result<StackedSyncReport> {
        let (changed_paths, status_deleted_paths) =
            git_changed_paths_against_head(&self.worktree_root, &self.base)?;
        let existing_delta: BTreeSet<String> = self
            .delta
            .load_snapshot()?
            .files
            .into_iter()
            .map(|f| f.path)
            .collect();

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
                self.sync_file_from_disk(rel_path)?;
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
                        let matches_base = self
                            .base
                            .latest_file(rel_path)?
                            .is_some_and(|bf| {
                                devmap_extract::content_hash(source_str) == bf.content_hash
                            });
                        if matches_base {
                            if existing_delta.contains(rel_path) {
                                self.clear_reverted_file(rel_path)?;
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
                                let extraction = devmap_extract::extract_file(rel_path, source_str);
                                self.record_extracted_file(rel_path, &bytes, &extraction)?;
                            }
                            synced_files += 1;
                        }
                    }
                } else if self.base.latest_path_is_indexed(rel_path)?
                    || existing_delta.contains(rel_path)
                {
                    self.record_deleted_file(rel_path)?;
                    deleted_files += 1;
                }
            }
        }

        for prev in existing_delta {
            if !changed_paths.contains(&prev) {
                let full = self.worktree_root.join(&prev);
                if full.is_file() {
                    if let Ok(bytes) = fs::read(&full) {
                        if let Ok(src) = std::str::from_utf8(&bytes) {
                            if let Some(base_file) = self.base.latest_file(&prev)? {
                                if devmap_extract::content_hash(src) == base_file.content_hash {
                                    self.clear_reverted_file(&prev)?;
                                    cleaned_files += 1;
                                }
                            }
                        }
                    }
                } else if !self.base.latest_path_is_indexed(&prev)? {
                    self.clear_reverted_file(&prev)?;
                    cleaned_files += 1;
                }
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

    pub fn needs_rebase(&self) -> Result<bool> {
        let dirty = self.delta.dirty_file_count()?;
        let bytes = self.delta.bytes_on_disk();
        Ok(dirty > self.lease.rebases_after_dirty_files
            || bytes > self.lease.rebases_after_delta_bytes)
    }

    /// Promote this stacked worktree into a standalone writable CoW `devmap.sqlite`
    /// and retire `stack_lease.json` + `delta.sqlite` (`OQ2`).
    pub fn materialize_seeded_store(&self) -> Result<PathBuf> {
        let target_db = store_path(&self.worktree_root);
        promote_immutable_base_to_store(&self.base_sqlite_path, &target_db, &self.worktree_root)?;
        if let Some(ref src_csr) = self.base_csr_path {
            let dst_csr = csr_path_for_store(&target_db);
            let _ = seed_artifact(src_csr, &dst_csr)?;
            if dst_csr.is_file() {
                let _ = set_file_writable(&dst_csr);
            }
        }
        let _ = fs::remove_file(stack_lease_path(&self.worktree_root));
        discard_staged(self.delta.path());
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

    pub fn all_indexed_files(
        &self,
    ) -> Result<Option<(u32, Option<String>, Vec<(String, String)>)>> {
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
                page.rows
                    .retain(|row| kinds.iter().any(|k| k == &row.kind));
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
            return Ok(self.base.callers_page(targets, exclude_file, min_confidence)?);
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
                !self.overlay.is_file_tombstoned(pr) && !self.overlay.modified_files.contains_key(pr)
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
                    if needle.is_empty()
                        || sym.name.to_ascii_lowercase().contains(&needle)
                        || sym.qualified_name.to_ascii_lowercase().contains(&needle)
                    {
                        if seen.insert((path.clone(), sym.qualified_name.clone())) {
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
        }

        let base_rows = if needle.is_empty() {
            self.base
                .all_symbols_page()?
                .map(|p| p.rows)
                .unwrap_or_default()
        } else {
            self.base
                .search_symbols(query, limit.saturating_add(self.overlay.modified_files.len() * 8).max(64))?
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
        let delta_bytes = self.delta.bytes_on_disk();
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
        self.overlay =
            build_overlay_from_delta(&wt_str, self.lease.generation_id, &self.base, &self.delta)?;
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
    let caller_family =
        LangFamily::from_lang(devmap_extract::languages::detect_language(Path::new(rel_path)));

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
        let recv = call
            .receiver_expr
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty() && !matches!(*r, "self" | "this" | "Self" | "super" | "cls"));

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
                        || Path::new(cand_file)
                            .file_stem()
                            .and_then(|s| s.to_str())
                            == Some(r)
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
    delta: &DeltaStore,
) -> Result<WorktreeOverlay> {
    let snap = delta.load_snapshot()?;
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
                let current_symbols: BTreeSet<String> =
                    ext.symbols.iter().map(|s| s.qualified_name.clone()).collect();
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

#[cfg(feature = "parse")]
fn git_changed_paths_against_head(
    worktree_root: &Path,
    base: &Store,
) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
    use devmap_extract::subprocess::{git, run_bounded, Bounds};
    use std::time::Duration;

    let mut paths = BTreeSet::new();
    let mut status_deleted = BTreeSet::new();
    let bounds = Bounds {
        deadline: Duration::from_secs(10),
        stdout_cap: 8 * 1024 * 1024,
        stderr_cap: 64 * 1024,
    };

    if let Ok(mut cmd) = git(
        worktree_root,
        ["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    ) {
        if let Ok(out) = run_bounded(&mut cmd, bounds) {
            if out.status.success() {
                let mut entries = out.stdout.split(|&b| b == 0).peekable();
                while let Some(entry) = entries.next() {
                    if entry.len() < 4 {
                        continue;
                    }
                    let status = &entry[..2];
                    let path_bytes = &entry[3..];
                    if let Ok(p) = std::str::from_utf8(path_bytes) {
                        if let Ok(norm) = normalize_rel_path(p) {
                            if status.contains(&b'D') {
                                status_deleted.insert(norm.clone());
                            }
                            paths.insert(norm);
                        }
                    }
                    if status.contains(&b'R') || status.contains(&b'C') {
                        if let Some(orig) = entries.next() {
                            if let Ok(p) = std::str::from_utf8(orig) {
                                if let Ok(norm) = normalize_rel_path(p) {
                                    if status.contains(&b'R') {
                                        status_deleted.insert(norm.clone());
                                    }
                                    paths.insert(norm);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if let Ok(Some(base_head)) = base.latest_generation_head_sha() {
        if let Ok(mut diff_cmd) = git(
            worktree_root,
            ["diff", "--name-only", "-z", &base_head, "HEAD"],
        ) {
            if let Ok(out) = run_bounded(&mut diff_cmd, bounds) {
                if out.status.success() {
                    for raw in out.stdout.split(|&b| b == 0) {
                        if raw.is_empty() {
                            continue;
                        }
                        if let Ok(p) = std::str::from_utf8(raw) {
                            if let Ok(norm) = normalize_rel_path(p) {
                                paths.insert(norm);
                            }
                        }
                    }
                }
            }
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

// ---- Streaming FIPS 180-4 SHA-256 -------------------------------------------

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
    0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
    0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
    0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
    0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
    0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
    0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
    0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
    0xc67178f2,
];

struct Sha256 {
    h: [u32; 8],
    buf: [u8; 64],
    buffered: usize,
    bits: u64,
}

impl Sha256 {
    fn new() -> Self {
        Self {
            h: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c,
                0x1f83d9ab, 0x5be0cd19,
            ],
            buf: [0u8; 64],
            buffered: 0,
            bits: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.bits = self.bits.wrapping_add((data.len() as u64) * 8);
        if self.buffered > 0 {
            let take = (64 - self.buffered).min(data.len());
            self.buf[self.buffered..self.buffered + take].copy_from_slice(&data[..take]);
            self.buffered += take;
            data = &data[take..];
            if self.buffered < 64 {
                return;
            }
            let block = self.buf;
            self.compress(&block);
            self.buffered = 0;
        }
        while data.len() >= 64 {
            let mut block = [0u8; 64];
            block.copy_from_slice(&data[..64]);
            self.compress(&block);
            data = &data[64..];
        }
        self.buf[..data.len()].copy_from_slice(data);
        self.buffered = data.len();
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (slot, add) in self.h.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(add);
        }
    }

    fn finish(mut self) -> [u8; 32] {
        let bits = self.bits;
        self.buf[self.buffered] = 0x80;
        self.buffered += 1;
        if self.buffered > 56 {
            self.buf[self.buffered..64].fill(0);
            let block = self.buf;
            self.compress(&block);
            self.buffered = 0;
        }
        self.buf[self.buffered..56].fill(0);
        self.buf[56..64].copy_from_slice(&bits.to_be_bytes());
        let block = self.buf;
        self.compress(&block);
        let mut out = [0u8; 32];
        for (i, word) in self.h.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

pub fn sha256_bytes_hex(bytes: &[u8]) -> String {
    let mut s = Sha256::new();
    s.update(bytes);
    crate::digest::hex(&s.finish())
}

pub fn sha256_file_hex(path: &Path) -> Result<String> {
    let mut file =
        fs::File::open(path).with_context(|| format!("cannot open {} for sha256", path.display()))?;
    let mut s = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        s.update(&buf[..n]);
    }
    Ok(crate::digest::hex(&s.finish()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "parse")]
    use std::process::Command;
    #[cfg(feature = "parse")]
    use std::sync::atomic::{AtomicU64, Ordering};

    #[cfg(feature = "parse")]
    fn temp_repo_dir(tag: &str) -> PathBuf {
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

    #[test]
    fn sha256_matches_nist_fips_180_4_vectors() {
        assert_eq!(
            sha256_bytes_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_bytes_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
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

        let db_path = store_path(anchor);
        fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let store = Store::open(&db_path).unwrap();
        let anchor_canon = anchor.canonicalize().unwrap();
        store.bind_repo_root(&anchor_canon).unwrap();
        let exts = vec![
            devmap_extract::extract_file(
                "src/svc.go",
                &fs::read_to_string(anchor.join("src/svc.go")).unwrap(),
            ),
            devmap_extract::extract_file(
                "src/routes.ts",
                &fs::read_to_string(anchor.join("src/routes.ts")).unwrap(),
            ),
            devmap_extract::extract_file(
                "src/limits.rs",
                &fs::read_to_string(anchor.join("src/limits.rs")).unwrap(),
            ),
        ];
        let mut resolver = devmap_resolve::Resolver::new();
        resolver.index_extractions(&exts);
        let res = resolver.resolve_all(&exts).unwrap();
        let analysis = devmap_analyze::analyze(&exts, &res);
        store.save_generation(&exts, &res, &analysis).unwrap();
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
        assert!(go_names.contains(&"Handle"), "Go receiver method Handle lost: {go_names:?}");
        assert!(go_names.contains(&"Shutdown"), "Go receiver method Shutdown lost: {go_names:?}");
        assert!(!go_names.contains(&"Helper"), "Removed Helper should be tombstoned: {go_names:?}");
        assert!(
            stacked
                .search_symbols("Helper", 10)
                .unwrap()
                .is_empty(),
            "Removed Helper must not leak from immutable base"
        );

        // TS `API_ROUTE` (`export const`) and `dispatch`/`health` class methods must survive!
        let ts_syms = stacked.symbols_for_file("src/routes.ts").unwrap();
        let ts_names: Vec<&str> = ts_syms.iter().map(|s| s.name.as_str()).collect();
        assert!(ts_names.contains(&"API_ROUTE"), "TS export const lost: {ts_names:?}");
        assert!(ts_names.contains(&"dispatch"), "TS class method lost: {ts_names:?}");
        assert!(ts_names.contains(&"health"), "TS new class method lost: {ts_names:?}");

        // Rust `MAX_BATCH` (`pub const`) and `check_cap` (`macro_rules!`) must survive!
        let rs_syms = stacked.symbols_for_file("src/limits.rs").unwrap();
        let rs_names: Vec<&str> = rs_syms.iter().map(|s| s.name.as_str()).collect();
        assert!(rs_names.contains(&"MAX_BATCH"), "Rust pub const lost: {rs_names:?}");
        assert!(rs_names.contains(&"check_cap"), "Rust macro_rules! lost: {rs_names:?}");

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

        let lease_a = provision_stacked_worktree(&anchor, &lane_a).unwrap();
        assert_eq!(lease_a.generation_id, 1);

        // Advance anchor to generation 2 and provision lane-b on gen-2.
        {
            let store = Store::open(store_path(&anchor)).unwrap();
            let ext = devmap_extract::extract_file("src/extra.rs", "pub fn gen2() {}\n");
            let mut resolver = devmap_resolve::Resolver::new();
            resolver.index_extractions(std::slice::from_ref(&ext));
            let res = resolver.resolve_all(std::slice::from_ref(&ext)).unwrap();
            let analysis = devmap_analyze::analyze(std::slice::from_ref(&ext), &res);
            store
                .save_generation(std::slice::from_ref(&ext), &res, &analysis)
                .unwrap();
        }
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
        assert!(base_generation_dir(&common, 1).is_dir());

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
    #[test]
    fn gc_unused_bases_fails_closed_when_worktree_moved_before_git_repair() {
        let root = temp_repo_dir("gc-moved-worktree");
        let anchor = root.join("anchor");
        seed_indexed_git_repo(&anchor);

        let lane_a = root.join("lane-a");
        run_git_ok(
            &anchor,
            &["worktree", "add", "-b", "lane-a", lane_a.to_str().unwrap()],
        );
        let lease_a = provision_stacked_worktree(&anchor, &lane_a).unwrap();
        assert_eq!(lease_a.generation_id, 1);

        // Advance anchor to generation 2 and publish gen-2 so gen-1 is no longer newest.
        {
            let store = Store::open(store_path(&anchor)).unwrap();
            let ext = devmap_extract::extract_file("src/extra.rs", "pub fn gen2() {}\n");
            let mut resolver = devmap_resolve::Resolver::new();
            resolver.index_extractions(std::slice::from_ref(&ext));
            let res = resolver.resolve_all(std::slice::from_ref(&ext)).unwrap();
            let analysis = devmap_analyze::analyze(std::slice::from_ref(&ext), &res);
            store
                .save_generation(std::slice::from_ref(&ext), &res, &analysis)
                .unwrap();
        }
        let _ = publish_base_snapshot(&anchor).unwrap();

        // Simulate user moving `lane-a` on disk before running `git worktree repair`.
        let lane_moved = root.join("lane-a-moved");
        fs::rename(&lane_a, &lane_moved).unwrap();

        let common = registered_common_git_dir(&anchor).unwrap();
        let res = gc_unused_bases(&common);
        assert!(
            res.is_err(),
            "gc_unused_bases must fail closed when a registered worktree gitdir points to a moved/missing path, got: {res:?}"
        );
        assert!(
            base_generation_dir(&common, 1).is_dir(),
            "gen-1 must NOT be deleted when a registered worktree could not be inspected"
        );

        let _ = remove_readonly_tree(&root);
    }

    #[cfg(feature = "parse")]
    #[test]
    fn legacy_base_store_ref_auto_upgrades_into_stacked_store_and_preserves_queries() {
        let root = temp_repo_dir("legacy-ref-upgrade");
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

        // Adversarial: forged legacy ref pointing at an unrelated repo MUST NOT upgrade!
        let unrelated = root.join("unrelated");
        seed_indexed_git_repo(&unrelated);
        let forged_ref = devmap_extract::paths::legacy_base_store_ref_path(&lane_a);
        fs::create_dir_all(forged_ref.parent().unwrap()).unwrap();
        fs::write(
            &forged_ref,
            serde_json::to_vec(&serde_json::json!({
                "anchor_repo_root": unrelated.canonicalize().unwrap(),
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(
            legacy_base_store_anchor_root(&lane_a).is_none(),
            "unrelated anchor_repo_root must be rejected"
        );
        assert!(StackedStore::open_synced(&lane_a).unwrap().is_none());

        // Variant 1: `anchor_repo_root` pointing at sibling registered anchor.
        fs::write(
            &forged_ref,
            serde_json::to_vec(&serde_json::json!({
                "anchor_repo_root": anchor.canonicalize().unwrap(),
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(legacy_base_store_anchor_root(&lane_a).is_some());
        let stacked_a = StackedStore::open_synced(&lane_a)
            .unwrap()
            .expect("legacy base_store_ref.json should auto-upgrade to StackedStore");
        assert!(stack_lease_path(&lane_a).is_file());
        assert!(forged_ref.is_file(), "base_store_ref.json stays intact alongside stack_lease.json");
        assert!(
            !StackedStore::upgrade_legacy_base_store_ref_if_needed(&lane_a).unwrap(),
            "subsequent upgrade calls are O(1) no-ops once stack_lease.json exists"
        );
        {
            let eng_a =
                crate::StoreQueryEngine::new(stacked_a.base_store()).with_stacked(Some(&stacked_a));
            let hits = eng_a
                .search(crate::Request {
                    query: "Handle".to_string(),
                    token_budget: 2000,
                    min_confidence: 0.0,
                    max_depth: 1,
                })
                .unwrap();
            assert!(!hits.items.is_empty(), "upgraded lane-a should serve Handle");
        }
        drop(stacked_a);

        // Variant 2: `base_store_path` under `.devmap/codeintel/base_store_ref.json`.
        let alt_ref = lane_b.join(".devmap/codeintel/base_store_ref.json");
        fs::create_dir_all(alt_ref.parent().unwrap()).unwrap();
        fs::write(
            &alt_ref,
            serde_json::to_vec(&serde_json::json!({
                "base_store_path": store_path(&anchor.canonicalize().unwrap()),
            }))
            .unwrap(),
        )
        .unwrap();
        let stacked_b = StackedStore::open_synced(&lane_b)
            .unwrap()
            .expect("base_store_path variant should auto-upgrade to StackedStore");
        assert!(stack_lease_path(&lane_b).is_file());
        assert!(alt_ref.is_file());
        drop(stacked_b);

        let _ = remove_readonly_tree(&root);
    }

    #[cfg(feature = "parse")]
    #[test]
    fn store_query_engine_with_stacked_answers_all_graph_and_search_surfaces_over_delta_edits_and_deletions()
    {
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

        run_git_ok(&anchor, &["init", "-b", "main"]);
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
                    devmap_extract::extract_file(
                        p,
                        &fs::read_to_string(anchor.join(p)).unwrap(),
                    )
                })
                .collect();
            let mut resolver = devmap_resolve::Resolver::new();
            resolver.index_extractions(&exts);
            let res = resolver.resolve_all(&exts).unwrap();
            let analysis = devmap_analyze::analyze(&exts, &res);
            store.save_generation(&exts, &res, &analysis).unwrap();
            let _ = store.write_latest_csr_sidecar().unwrap();
        }

        let lane = root.join("lane");
        run_git_ok(
            &anchor,
            &["worktree", "add", "-b", "lane", lane.to_str().unwrap()],
        );
        provision_stacked_worktree(&anchor, &lane).unwrap();

        // Uncommitted worktree mutations:
        // 1. Modify `src/db.rs`: shift lines, add `stream_rows` calling `run_query` + `soon_called`, new literal.
        fs::write(
            lane.join("src/db.rs"),
            "// header comment shifting spans\npub fn run_query() -> i32 { 99 }\npub fn stream_rows() -> i32 {\n    let _tag = \"LIVE_DELTA_LITERAL\";\n    run_query() + soon_called()\n}\n",
        )
        .unwrap();
        // 2. Brand-new file `src/api.rs` calling `stream_rows`.
        fs::write(
            lane.join("src/api.rs"),
            "use crate::db;\npub fn serve_stream() -> i32 {\n    stream_rows()\n}\n",
        )
        .unwrap();
        // 3. Modify `tests/smoke.rs` adding a brand-new `#[test]` calling `serve_stream`.
        fs::write(
            lane.join("tests/smoke.rs"),
            "#[test]\nfn test_handle_req() {\n    let _ = handle_req();\n}\n\n#[test]\nfn test_stream_pipeline() {\n    let _ = serve_stream();\n}\n",
        )
        .unwrap();
        // 4. Delete `src/obsolete.rs` from disk.
        fs::remove_file(lane.join("src/obsolete.rs")).unwrap();

        let stacked = StackedStore::open_synced(&lane)
            .unwrap()
            .expect("lane should remain stacked");
        let engine = crate::StoreQueryEngine::new(stacked.base_store()).with_stacked(Some(&stacked));

        // 1. Keyword search: finds new delta symbols with verified verbatim source_span, tombstones deleted/removed symbols.
        let search_new = engine
            .search(crate::Request {
                query: "stream_rows".to_string(),
                token_budget: 4000,
                min_confidence: 0.0,
                max_depth: 1,
            })
            .unwrap();
        assert_eq!(search_new.items.len(), 1);
        assert!(
            search_new.items[0]
                .source_span
                .contains("LIVE_DELTA_LITERAL"),
            "delta hit must verify content_hash and populate verbatim source_span: {:?}",
            search_new.items[0]
        );
        let search_deleted = engine
            .search(crate::Request {
                query: "vanished_api".to_string(),
                token_budget: 4000,
                min_confidence: 0.0,
                max_depth: 1,
            })
            .unwrap();
        assert!(
            search_deleted.items.is_empty(),
            "deleted file symbol vanished_api must be masked: {:?}",
            search_deleted.items
        );

        // 2. Semantic search & ask: include delta symbols and exclude tombstoned symbols.
        let sem = engine.search_semantic("stream rows", 4000).unwrap();
        assert!(sem.items.iter().any(|h| h.symbol_name == "stream_rows"));
        let ask_res = engine.ask("stream rows", 4000, 0.0).unwrap();
        assert!(ask_res.items.iter().any(|h| h.symbol_name == "stream_rows"));

        // 3. Explore: finds delta symbol `stream_rows` with inbound `serve_stream` and outbound `run_query`.
        let exp = engine.explore("stream_rows", 10, 4000, 0.0, 2).unwrap();
        assert_eq!(exp.definitions.items.len(), 1);
        let entry = &exp.definitions.items[0];
        assert!(
            entry
                .callers
                .items
                .iter()
                .any(|e| e.source_symbol.contains("serve_stream")),
            "explore callers missing serve_stream: {:?}",
            entry.callers.items
        );
        assert!(
            entry
                .callees
                .items
                .iter()
                .any(|e| e.target_symbol.contains("run_query")),
            "explore callees missing run_query: {:?}",
            entry.callees.items
        );

        // 4. Impact & Layered Impact: walks across base + delta edges (`run_query` <- `stream_rows` <- `serve_stream` <- `test_stream_pipeline`).
        let imp = engine
            .impact(crate::Request {
                query: "run_query".to_string(),
                token_budget: 4000,
                min_confidence: 0.0,
                max_depth: 5,
            })
            .unwrap();
        assert!(
            imp.items.iter().any(|e| e.source_symbol.contains("stream_rows")),
            "impact missing delta caller stream_rows: {:?}",
            imp.items
        );
        assert!(
            imp.items
                .iter()
                .any(|e| e.source_symbol.contains("test_stream_pipeline")),
            "impact missing transitive delta test caller: {:?}",
            imp.items
        );
        let layered = engine
            .impact_layered(crate::Request {
                query: "run_query".to_string(),
                token_budget: 4000,
                min_confidence: 0.0,
                max_depth: 5,
            })
            .unwrap();
        assert!(!layered.edges.items.is_empty());

        // 5. Neighbors & Trace / Trace-Between across delta + base boundaries.
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
        assert!(
            !tr.items.is_empty(),
            "trace_between must connect new delta test to run_query"
        );

        // 6. File dependencies: works on brand-new delta file `src/api.rs` and masks deleted `src/obsolete.rs`.
        let deps_api = engine
            .dependencies(crate::Request {
                query: "src/api.rs".to_string(),
                token_budget: 4000,
                min_confidence: 0.0,
                max_depth: 1,
            })
            .unwrap();
        assert!(
            deps_api.items.iter().any(|e| e.target_file == "src/db.rs"),
            "dependencies(src/api.rs) should reach src/db.rs: {:?}",
            deps_api.items
        );

        // 7. Affected tests: NEVER fails closed on 1+ edits; discovers new `test_stream_pipeline`!
        let aff = engine
            .affected_tests(&["src/db.rs".to_string()], 4000, 0.0, 5)
            .unwrap();
        assert!(
            aff.tests
                .items
                .iter()
                .any(|t| t.symbols.iter().any(|s| s.contains("test_stream_pipeline"))),
            "affected_tests must discover new delta test test_stream_pipeline: {:?}",
            aff.tests.items
        );

        // 8. Skeleton: reflects modified `src/db.rs` and reports `NotInIndex` for deleted `src/obsolete.rs`.
        let skel_db = engine.skeleton("src/db.rs", 4000).unwrap();
        assert_eq!(skel_db.presence, crate::SkeletonPresence::Indexed);
        assert!(skel_db
            .items
            .iter()
            .any(|i| i.qualified_name.contains("stream_rows")));
        let skel_del = engine.skeleton("src/obsolete.rs", 4000).unwrap();
        assert_eq!(skel_del.presence, crate::SkeletonPresence::NotInIndex);

        // 9. Dead symbols: `soon_called` is now called by delta `stream_rows` so NOT dead; `orphan_fn` remains dead; `vanished_api` is tombstoned!
        let dead = engine.dead_symbols(4000).unwrap();
        let dead_names: Vec<&str> = dead.items.iter().map(|d| d.symbol_name.as_str()).collect();
        assert!(dead_names.contains(&"orphan_fn"), "orphan_fn should remain dead: {dead_names:?}");
        assert!(
            !dead_names.contains(&"soon_called"),
            "soon_called now has a live delta caller and must not be reported dead: {dead_names:?}"
        );
        assert!(
            !dead_names.contains(&"vanished_api"),
            "deleted vanished_api must not leak into dead_symbols: {dead_names:?}"
        );

        // 10. Literals: finds new `LIVE_DELTA_LITERAL` and masks removed `OLD_SECRET_TAG` + `VANISHED_LITERAL`.
        let lit_live = engine.literals("LIVE_DELTA_LITERAL", false, 4000).unwrap();
        assert_eq!(lit_live.items.len(), 1);
        let lit_old = engine.literals("OLD_SECRET_TAG", false, 4000).unwrap();
        assert!(lit_old.items.is_empty());
        let lit_vanished = engine.literals("VANISHED_LITERAL", false, 4000).unwrap();
        assert!(lit_vanished.items.is_empty());

        // 11. Preview: detects signature break against delta-synced `src/db.rs` including delta caller `serve_stream`!
        let prev = engine
            .preview(
                "src/db.rs",
                "pub fn run_query() -> i32 { 99 }\npub fn stream_rows(limit: usize) -> i32 { limit as i32 }\n",
                4000,
                0.0,
            )
            .unwrap();
        assert!(prev.delta_available);
        assert!(
            prev.broken_callers
                .items
                .iter()
                .any(|e| e.caller_symbol.contains("serve_stream")),
            "preview must surface delta caller serve_stream when stream_rows signature changes: {:?}",
            prev.broken_callers.items
        );

        let _ = remove_readonly_tree(&root);
    }
}
