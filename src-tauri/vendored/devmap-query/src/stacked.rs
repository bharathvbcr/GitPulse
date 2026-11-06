//! Glean-style `StackedStore` combining an immutable `chmod 0444` base snapshot
//! (`<common_git_dir>/devmap/bases/gen-<G>/base.sqlite?immutable=1` + lazy mmap
//! `base.sqlite.csr`) with a per-worktree `DeltaStore` (`delta.sqlite`) and
//! `WorktreeOverlay` (`RoaringBitmap` dirty-node masking) (`F1`, `F4`, `F6`,
//! `F10`, `OQ1`, `OQ2`).

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use devmap_extract::model::Extraction;
use devmap_extract::paths::{
    code_graph_path, compact_code_graph_path, csr_path_for_store, registered_common_git_dir,
    registered_worktrees_of_common_dir, repo_map_path, same_registered_repo, stack_delta_path,
    stack_lease_path, state_dir, store_path,
};
use devmap_store::csr::CsrGraphReader;
use devmap_store::delta::{DeltaEdgeRecord, DeltaStore};
use devmap_store::schema::CURRENT_SCHEMA_VERSION;
use devmap_store::seed::{
    discard_staged, prepare_immutable_snapshot, promote_immutable_base_to_store, seed_artifact,
    seed_directory_cow, set_file_readonly, set_file_writable, stage_seed_healing_wal,
};
use devmap_store::{Store, StoredSymbol};
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
/// worktree carries an unreadable or malformed `stack_lease.json` (`OQ2`).
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
    /// Open the stacked worktree store declared by `<worktree_root>/<state>/codeintel/stack_lease.json`.
    pub fn open(worktree_root: &Path) -> Result<Self> {
        let wt_canon = worktree_root
            .canonicalize()
            .with_context(|| format!("cannot canonicalize {}", worktree_root.display()))?;
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
            delta,
            overlay,
        })
    }

    pub fn open_if_leased(worktree_root: &Path) -> Result<Option<Self>> {
        if !stack_lease_path(worktree_root).is_file() {
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

    pub fn base_store(&self) -> &Store {
        &self.base
    }

    pub fn delta_store(&self) -> &DeltaStore {
        &self.delta
    }

    pub fn overlay(&self) -> &WorktreeOverlay {
        &self.overlay
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
                    let lang = devmap_extract::languages::detect_language(Path::new(&normalized));
                    let key = devmap_extract::cache::CacheKey::for_source(lang, source_str);
                    if key.content_hash == base_file.content_hash {
                        return self.clear_reverted_file(&normalized);
                    }
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
        let changed_paths = git_changed_paths_against_head(&self.worktree_root, &self.base)?;
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
            } else if self.base.latest_path_is_indexed(rel_path)? || existing_delta.contains(rel_path)
            {
                self.record_deleted_file(rel_path)?;
                deleted_files += 1;
            }
        }

        for prev in existing_delta {
            if !changed_paths.contains(&prev) {
                let full = self.worktree_root.join(&prev);
                if full.is_file() {
                    if let Ok(bytes) = fs::read(&full) {
                        if let Ok(src) = std::str::from_utf8(&bytes) {
                            if let Some(base_file) = self.base.latest_file(&prev)? {
                                let lang = devmap_extract::languages::detect_language(Path::new(&prev));
                                let key = devmap_extract::cache::CacheKey::for_source(lang, src);
                                if key.content_hash == base_file.content_hash {
                                    self.clear_reverted_file(&prev)?;
                                    cleaned_files += 1;
                                }
                            }
                        }
                    }
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
        let wt_str = self.worktree_root.to_string_lossy().into_owned();
        self.overlay =
            build_overlay_from_delta(&wt_str, self.lease.generation_id, &self.base, &self.delta)?;
        Ok(())
    }
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
fn git_changed_paths_against_head(worktree_root: &Path, base: &Store) -> Result<BTreeSet<String>> {
    use devmap_extract::subprocess::{git, run_bounded, Bounds};
    use std::time::Duration;

    let mut paths = BTreeSet::new();
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
                            paths.insert(norm);
                        }
                    }
                    if status.contains(&b'R') || status.contains(&b'C') {
                        if let Some(orig) = entries.next() {
                            if let Ok(p) = std::str::from_utf8(orig) {
                                if let Ok(norm) = normalize_rel_path(p) {
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

    Ok(paths)
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
}
