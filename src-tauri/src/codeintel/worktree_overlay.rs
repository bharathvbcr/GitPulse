//! Parallel DevMap worktree seeding and canonical `StackedStore` delta overlays.
//!
//! When an agent or developer creates a linked Git worktree, starting with no
//! DevMap state forces either cold multi-second `devmap build` runs or
//! `available: false` query refusals across every parallel lane.
//!
//! This module delegates all zero-copy / stacked worktree code intelligence to
//! DevMap's canonical `StackedStore` (`stack_lease.json` + sealed `0444`
//! `base.sqlite` snapshot under `<common_git_dir>/devmap/bases/gen-<G>-<head12>-<src8>/`
//! + lazy per-worktree `delta.sqlite` populated by real Tree-sitter extraction):
//! - **Zero write-on-read**: Every read surface (`open_verified_stacked_store`,
//!   `load_worktree_overlay`, `compute_worktree_dirty_nodes_readonly`,
//!   `open_worktree_or_anchor_csr`) opens `StackedStore::open` strictly read-only,
//!   never creating or mutating `delta.sqlite`, `stack_lease.json`, or `base.sqlite`.
//! - **No legacy sidecars**: Retired `base_store_ref.json`, `overlay.json`,
//!   `CasAstStore`, and regex declaration fallbacks are gone; legacy
//!   `base_store_ref.json` files are refused naming `devmap seed --stacked --from <main>`.
//! - **CoW standalone seeding (`provision_worktree_devmap`)**: Clones `devmap.sqlite`
//!   under the target's writer lock, rebinds via `seed_rebind` without rewriting
//!   `generations` overflow pages, and seeds `.csr`, `repo_map.json`, and
//!   `resolution-reuse-*` artifacts.

use crate::devmap::init::ensure_dir_excluded;
use crate::engine::cow_clone::reflink_copy_file;
use crate::engine::git_cli::{git_captured, validate_repo, BoundedRun};
use crate::ipc_error::CommandError;
use devmap_query::WorktreeOverlay;
use devmap_store::{IncrementalDigestEngine, Store};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Marker written above `.git/info/exclude` entries when seeding a DevMap state directory.
const DEVMAP_EXCLUDE_MARKER: &str =
    "# GitPulse: DevMap index state (machine-generated, never committed)";

///Retired pointer filename checked only so legacy checkouts fail closed with an explicit upgrade hint.
const LEGACY_BASE_STORE_REF_RELPATH: &str = "codeintel/base_store_ref.json";

/// Maximum size of a dirty source file hashed into a worktree delta check (8 MiB).
pub const MAX_OVERLAY_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Maximum number of dirty/untracked paths inspected in one delta scan pass.
pub const MAX_OVERLAY_DIRTY_FILES: usize = 10_000;

/// Verified metadata for a worktree opened over a canonical `StackedStore` lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackedBaseMeta {
    pub anchor_repo_root: PathBuf,
    pub base_db_path: String,
    pub base_csr_path: Option<PathBuf>,
    pub generation_id: u32,
    pub base_head_sha: Option<String>,
}

/// Outcome of provisioning DevMap artifacts into a newly created or isolated worktree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DevmapWorktreeProvision {
    pub seeded: bool,
    pub is_cow: bool,
    pub used_online_backup: bool,
    #[serde(default)]
    pub zero_copy_base_ref: bool,
    pub generation_id: Option<u32>,
    pub db_path: Option<String>,
    pub csr_seeded: bool,
    pub repo_map_seeded: bool,
    pub cas_seeded: bool,
    pub overlay_path: Option<String>,
    pub duration_ms: u64,
    pub reason: Option<String>,
}

impl DevmapWorktreeProvision {
    fn skipped(reason: impl Into<String>, start: Instant) -> Self {
        Self {
            seeded: false,
            is_cow: false,
            used_online_backup: false,
            zero_copy_base_ref: false,
            generation_id: None,
            db_path: None,
            csr_seeded: false,
            repo_map_seeded: false,
            cas_seeded: false,
            overlay_path: None,
            duration_ms: start.elapsed().as_millis() as u64,
            reason: Some(reason.into()),
        }
    }
}

/// Summary of an explicit stacked `delta.sqlite` synchronization pass.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeOverlayReport {
    pub overlay_path: String,
    pub generation_id: u64,
    pub base_head_sha: Option<String>,
    pub modified_files: usize,
    pub tombstoned_files: usize,
    pub tombstoned_symbols: usize,
    pub dirty_nodes: usize,
    pub reused_cas_asts: usize,
    pub skipped_large_files: usize,
    pub truncated: bool,
    pub duration_ms: u64,
}

struct TempSqliteGuard {
    base: PathBuf,
    armed: bool,
}

impl TempSqliteGuard {
    fn new(base: PathBuf) -> Self {
        Self { base, armed: true }
    }

    fn clean_sidecars(&self) {
        for suffix in ["-wal", "-shm", "-journal"] {
            let sidecar = PathBuf::from(format!("{}{suffix}", self.base.display()));
            let _ = fs::remove_file(sidecar);
        }
    }
}

impl Drop for TempSqliteGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.base);
            self.clean_sidecars();
        }
    }
}

fn next_seed_seq() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn legacy_base_store_ref_path(repo: &Path) -> PathBuf {
    let state = devmap_query::paths::state_dir(repo);
    LEGACY_BASE_STORE_REF_RELPATH
        .split('/')
        .fold(state, |acc, part| acc.join(part))
}

/// Refuses retired `<state_dir>/codeintel/base_store_ref.json` files without
/// auto-upgrading or touching disk (`Owner Stop #2`).
pub fn refuse_legacy_base_store_ref(repo: &Path) -> Result<(), CommandError> {
    devmap_query::stacked::refuse_legacy_base_store_ref(repo)
        .map_err(|e| CommandError::from(e.to_string()))
}

pub(super) fn has_stack_lease_or_legacy_ref(repo: &Path) -> bool {
    devmap_extract::paths::stack_lease_path(repo).is_file()
        || legacy_base_store_ref_path(repo).is_file()
}

fn plain_single_dir_component(rel: &Path) -> Option<&str> {
    let mut components = rel.components();
    let first = match components.next()? {
        Component::Normal(os) => os.to_str()?,
        _ => return None,
    };
    if components.next().is_some() || first.is_empty() {
        return None;
    }
    Some(first)
}

/// Strips line (`//`, `#`, `--`) and C-style block (`/* ... */`) comments so
/// deleted symbol names mentioned only in comments do not trigger false
/// identifier overlap in `affected_tests` cone checks.
fn strip_comments_preserving_identifiers(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            out.push(' ');
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                if bytes[i] == b'\n' {
                    out.push('\n');
                }
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        if i + 1 < bytes.len()
            && ((bytes[i] == b'/' && bytes[i + 1] == b'/')
                || (bytes[i] == b'-' && bytes[i + 1] == b'-'))
        {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'#' {
            let next = bytes.get(i + 1).copied();
            if next != Some(b'[') && next != Some(b'!') {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
        }
        let ch = source[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn contains_identifier_token(code_without_comments: &str, ident: &str) -> bool {
    if ident.is_empty() {
        return false;
    }
    let is_ident_char = |c: char| c.is_alphanumeric() || c == '_';
    let mut offset = 0;
    while let Some(pos) = code_without_comments[offset..].find(ident) {
        let start = offset + pos;
        let end = start + ident.len();
        let before_ok = code_without_comments[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !is_ident_char(c));
        let after_ok = code_without_comments[end..]
            .chars()
            .next()
            .is_none_or(|c| !is_ident_char(c));
        if before_ok && after_ok {
            return true;
        }
        offset = start + ident.chars().next().map_or(1, char::len_utf8);
    }
    false
}

fn materialize_csr_from_store(store: &Store, target_csr: &Path) -> bool {
    let Ok(Some((_, index))) = store.generation_edges_with_id() else {
        return false;
    };
    let tmp_csr = target_csr.with_file_name(format!(
        ".devmap.sqlite.csr.seed.{}.{}",
        std::process::id(),
        next_seed_seq()
    ));
    if index.to_csr_writer().write_to_file(&tmp_csr).is_ok()
        && fs::rename(&tmp_csr, target_csr).is_ok()
    {
        return true;
    }
    let _ = fs::remove_file(&tmp_csr);
    false
}

fn resolve_stack_lease_donor(repo: &Path) -> Result<Option<PathBuf>, CommandError> {
    refuse_legacy_base_store_ref(repo)?;
    let lease_path = devmap_extract::paths::stack_lease_path(repo);
    if !lease_path.is_file() {
        return Ok(None);
    }
    let lease = devmap_query::stacked::StackLease::read_from(&lease_path)
        .map_err(|e| CommandError::from(format!("failed to read stack lease: {e}")))?;
    // The lane owns its base: the lease itself records the donor checkout, so
    // there is no shared-cache manifest to read (and nothing outside the lane
    // to trust).
    let donor = validate_repo(&lease.source_repo_root)?;
    let repo_common = crate::engine::git_cli::resolve_git_common_dir(repo)?;
    let donor_common = crate::engine::git_cli::resolve_git_common_dir(&donor)?;
    if repo_common != donor_common || !devmap_extract::paths::same_registered_repo(repo, &donor) {
        return Err(CommandError::from(format!(
            "refusing stack lease {} pointing outside worktree family ({})",
            lease_path.display(),
            donor.display()
        )));
    }
    Ok(Some(donor))
}

/// Opens and authenticates a stacked worktree's immutable base `Store` and
/// read-only `StackedStore` (`Owner Stop #1` & `#2`).
pub(super) fn open_verified_stacked_store(
    repo: &Path,
) -> Result<Option<(Store, StackedBaseMeta, devmap_query::StackedStore)>, CommandError> {
    // Family first, from the lease alone: since the base moved into the lane,
    // opening the store first would try a lease's base before anything had
    // asked whose lease it is, and a spoofed lease would be refused (or
    // admitted) for the wrong reason.
    let Some(anchor_root) = resolve_stack_lease_donor(repo)? else {
        return Ok(None);
    };
    let stacked = devmap_query::StackedStore::open(repo)
        .map_err(|e| CommandError::from(format!("failed to open stacked store: {e}")))?;
    // The store read the lease again; it must still name the donor just checked.
    if validate_repo(&stacked.lease().source_repo_root)? != anchor_root {
        return Err(CommandError::from(format!(
            "refusing stack lease {}: it changed while the lane was being opened",
            devmap_extract::paths::stack_lease_path(repo).display()
        )));
    }
    let base_sqlite = stacked.lease().base_sqlite_path();
    let wt_root_str = repo.to_string_lossy().into_owned();
    let store = Store::open_immutable_snapshot(&base_sqlite, Some(wt_root_str)).map_err(|e| {
        CommandError::from(format!(
            "failed to open immutable base snapshot {}: {e}",
            base_sqlite.display()
        ))
    })?;
    let base_head_sha = store
        .latest_generation_head_sha()
        .ok()
        .flatten()
        .or_else(|| Some(stacked.lease().head_sha.clone()));
    // The lease records whether the lane holds a CSR sidecar and its sha256.
    let base_csr_path = stacked
        .lease()
        .csr
        .as_ref()
        .map(|_| devmap_extract::paths::csr_path_for_store(&base_sqlite));
    let meta = StackedBaseMeta {
        anchor_repo_root: anchor_root,
        base_db_path: base_sqlite.to_string_lossy().into_owned(),
        base_csr_path,
        generation_id: stacked.lease().generation_id,
        base_head_sha,
    };
    Ok(Some((store, meta, stacked)))
}

/// Provisions a linked worktree's standalone DevMap store, mmap CSR graph sidecar,
/// navigator manifests, and `resolution-reuse-*` caches from `anchor_repo` using
/// Copy-on-Write cloning where supported.
///
/// Safe to run concurrently inside `std::thread::scope` alongside build-cache
/// CoW cloning. Never modifies tracked files, never writes `overlay.json` or
/// `CasAstStore`, and acquires the target's writer lock around placement (`F-d`).
pub fn provision_worktree_devmap(
    anchor_repo: &Path,
    worktree_repo: &Path,
) -> Result<DevmapWorktreeProvision, CommandError> {
    let start = Instant::now();
    let anchor_str = anchor_repo
        .to_str()
        .ok_or_else(|| CommandError::from("anchor repository path is not valid UTF-8"))?;
    let worktree_str = worktree_repo
        .to_str()
        .ok_or_else(|| CommandError::from("worktree repository path is not valid UTF-8"))?;

    if anchor_repo == worktree_repo {
        return Ok(DevmapWorktreeProvision::skipped(
            "anchor and target worktree are the same checkout",
            start,
        ));
    }

    refuse_legacy_base_store_ref(anchor_repo)?;
    refuse_legacy_base_store_ref(worktree_repo)?;

    if !devmap_query::paths::store_path(anchor_repo).is_file()
        && !devmap_extract::paths::stack_lease_path(anchor_repo).is_file()
    {
        return Ok(DevmapWorktreeProvision::skipped(
            "anchor repository has no DevMap store",
            start,
        ));
    }

    let mut anchor = validate_repo(anchor_str)?;
    let worktree = validate_repo(worktree_str)?;
    if anchor == worktree {
        return Ok(DevmapWorktreeProvision::skipped(
            "anchor and target worktree are the same checkout",
            start,
        ));
    }
    if !devmap_query::paths::store_path(&anchor).is_file() {
        if let Some(donor) = resolve_stack_lease_donor(&anchor)? {
            anchor = donor;
        }
    }
    if anchor == worktree {
        return Ok(DevmapWorktreeProvision::skipped(
            "anchor and target worktree are the same checkout",
            start,
        ));
    }

    let anchor_state = devmap_query::paths::state_dir(&anchor);
    let Ok(anchor_rel_state) = anchor_state.strip_prefix(&anchor) else {
        return Ok(DevmapWorktreeProvision::skipped(
            "DEVMAP_HOME points outside the anchor worktree",
            start,
        ));
    };
    let Some(anchor_state_name) = plain_single_dir_component(anchor_rel_state) else {
        return Ok(DevmapWorktreeProvision::skipped(
            "anchor DevMap state directory is not a top-level worktree directory",
            start,
        ));
    };

    let anchor_db = devmap_query::paths::store_path(&anchor);
    if !anchor_db.is_file() {
        return Ok(DevmapWorktreeProvision::skipped(
            "anchor repository has no DevMap store",
            start,
        ));
    }

    let anchor_store = match super::open_local_indexed_store(&anchor) {
        Ok(store) => store,
        Err(reason) => {
            return Ok(DevmapWorktreeProvision::skipped(
                format!("anchor DevMap store is not ready: {reason}"),
                start,
            ));
        }
    };
    let generation_id = match anchor_store.latest_generation_id() {
        Ok(Some(gen)) => gen,
        Ok(None) => {
            return Ok(DevmapWorktreeProvision::skipped(
                "anchor DevMap store holds no generation",
                start,
            ));
        }
        Err(e) => {
            return Ok(DevmapWorktreeProvision::skipped(
                format!("failed to read anchor generation: {e}"),
                start,
            ));
        }
    };
    drop(anchor_store);

    let target_state_name = if worktree.join(devmap_query::paths::STATE_DIR).is_dir() {
        devmap_query::paths::STATE_DIR
    } else if worktree
        .join(devmap_query::paths::LEGACY_STATE_DIR)
        .is_dir()
    {
        devmap_query::paths::LEGACY_STATE_DIR
    } else {
        anchor_state_name
    };

    let exclude = ensure_dir_excluded(&worktree, target_state_name, DEVMAP_EXCLUDE_MARKER);
    if !exclude.is_clean() {
        return Err(CommandError::from(format!(
            "refusing to seed DevMap into {} because {target_state_name}/ is not git-ignored: {exclude:?}",
            worktree.display()
        )));
    }

    if let Ok(existing_store) = super::open_local_indexed_store(&worktree) {
        let existing_gen = existing_store
            .latest_generation_id()
            .ok()
            .flatten()
            .unwrap_or(generation_id);
        drop(existing_store);
        let target_db = devmap_query::paths::store_path(&worktree);
        return Ok(DevmapWorktreeProvision {
            seeded: true,
            is_cow: false,
            used_online_backup: false,
            zero_copy_base_ref: false,
            generation_id: Some(existing_gen),
            db_path: Some(target_db.to_string_lossy().into_owned()),
            csr_seeded: Store::csr_sidecar_path(&target_db).is_file(),
            repo_map_seeded: devmap_query::paths::repo_map_path(&worktree).is_file(),
            cas_seeded: false,
            overlay_path: None,
            duration_ms: start.elapsed().as_millis() as u64,
            reason: None,
        });
    }

    let target_state = worktree.join(target_state_name);
    let target_codeintel = target_state.join("codeintel");
    fs::create_dir_all(&target_codeintel).map_err(|e| {
        CommandError::from(format!(
            "failed to create {}: {e}",
            target_codeintel.display()
        ))
    })?;

    let target_db = devmap_query::paths::store_path(&worktree);
    let _writer_lock = Store::lock_writer_at(&target_db, Store::WRITER_LOCK_WAIT)
        .map_err(|e| CommandError::from(format!("failed to lock {}: {e}", target_db.display())))?;
    let tmp_db = target_codeintel.join(format!(
        ".devmap.sqlite.seed.{}.{}",
        std::process::id(),
        next_seed_seq()
    ));
    let mut tmp_guard = TempSqliteGuard::new(tmp_db.clone());

    let seed_method =
        devmap_store::seed::stage_seed_healing_wal(&anchor_db, &tmp_db).map_err(|e| {
            CommandError::from(format!(
                "failed to stage DevMap seed from {}: {e}",
                anchor_db.display()
            ))
        })?;
    let is_cow = seed_method == devmap_store::seed::SeedMethod::Clone;
    let used_online_backup = seed_method == devmap_store::seed::SeedMethod::Backup;

    devmap_store::seed::rebind_staged(&tmp_db, &worktree)
        .map_err(|e| CommandError::from(format!("failed to rebind seeded DevMap store: {e}")))?;
    devmap_store::seed::settle_staged(&tmp_db)
        .map_err(|e| CommandError::from(format!("failed to settle seeded DevMap store: {e}")))?;
    tmp_guard.clean_sidecars();

    if target_db.exists() {
        let _ = fs::remove_file(&target_db);
        for suffix in ["-wal", "-shm", "-journal"] {
            let _ = fs::remove_file(format!("{}{suffix}", target_db.display()));
        }
    }

    fs::rename(&tmp_db, &target_db).map_err(|e| {
        CommandError::from(format!(
            "failed to place seeded DevMap store {}: {e}",
            target_db.display()
        ))
    })?;
    tmp_guard.armed = false;

    let seeded_store = match super::open_local_indexed_store(&worktree) {
        Ok(store) => store,
        Err(err) => {
            let _ = fs::remove_file(&target_db);
            return Err(CommandError::from(format!(
                "seeded DevMap store failed post-placement verification: {err}"
            )));
        }
    };

    let anchor_csr = Store::csr_sidecar_path(&anchor_db);
    let target_csr = Store::csr_sidecar_path(&target_db);
    let anchor_repo_map = devmap_query::paths::repo_map_path(&anchor);
    let target_repo_map = devmap_query::paths::repo_map_path(&worktree);
    let anchor_code_graph = devmap_query::paths::code_graph_path(&anchor);
    let target_code_graph = devmap_query::paths::code_graph_path(&worktree);
    let anchor_compact_graph = devmap_query::paths::compact_code_graph_path(&anchor);
    let target_compact_graph = devmap_query::paths::compact_code_graph_path(&worktree);

    let (csr_seeded, repo_map_seeded) = std::thread::scope(|scope| {
        let csr_task = scope.spawn(|| {
            if anchor_csr.is_file()
                && !target_csr.exists()
                && reflink_copy_file(&anchor_csr, &target_csr).is_ok()
            {
                if let Ok(reader) = devmap_store::CsrGraphReader::open_mmap(&target_csr) {
                    if reader.generation() == u64::from(generation_id) {
                        return true;
                    }
                }
                let _ = fs::remove_file(&target_csr);
            }
            if !anchor_csr.is_file()
                && ensure_dir_excluded(&anchor, anchor_state_name, DEVMAP_EXCLUDE_MARKER).is_clean()
                && materialize_csr_from_store(&seeded_store, &anchor_csr)
                && !target_csr.exists()
                && reflink_copy_file(&anchor_csr, &target_csr).is_ok()
            {
                return true;
            }
            materialize_csr_from_store(&seeded_store, &target_csr)
        });

        let manifest_task = scope.spawn(|| {
            let map_ok = devmap_store::seed::seed_artifact(&anchor_repo_map, &target_repo_map)
                .ok()
                .flatten()
                .is_some()
                || target_repo_map.is_file();
            let _ = devmap_store::seed::seed_artifact(&anchor_code_graph, &target_code_graph);
            let _ = devmap_store::seed::seed_artifact(&anchor_compact_graph, &target_compact_graph);
            if let Ok(entries) = fs::read_dir(&anchor_state) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    if let Some(s) = name.to_str() {
                        if s.starts_with("resolution-reuse-") && entry.path().is_dir() {
                            let _ = devmap_store::seed::seed_directory_cow(
                                &entry.path(),
                                &target_state.join(s),
                            );
                        }
                    }
                }
            }
            map_ok
        });

        (
            csr_task.join().unwrap_or(false),
            manifest_task.join().unwrap_or(false),
        )
    });

    drop(seeded_store);

    Ok(DevmapWorktreeProvision {
        seeded: true,
        is_cow,
        used_online_backup,
        zero_copy_base_ref: false,
        generation_id: Some(generation_id),
        db_path: Some(target_db.to_string_lossy().into_owned()),
        csr_seeded,
        repo_map_seeded,
        cas_seeded: false,
        overlay_path: None,
        duration_ms: start.elapsed().as_millis() as u64,
        reason: None,
    })
}

/// Provisions a canonical zero-copy `StackedStore` lease (`stack_lease.json` +
/// sealed `0444` `base.sqlite` snapshot under `<common_git_dir>/devmap/bases/`)
/// inside `worktree_repo` WITHOUT copying `devmap.sqlite` or writing any legacy
/// `base_store_ref.json`, `overlay.json`, or `delta.sqlite` (`P2.3`).
pub fn provision_worktree_devmap_zero_copy(
    anchor_repo: &Path,
    worktree_repo: &Path,
) -> Result<DevmapWorktreeProvision, CommandError> {
    let start = Instant::now();
    let anchor_str = anchor_repo
        .to_str()
        .ok_or_else(|| CommandError::from("anchor repository path is not valid UTF-8"))?;
    let worktree_str = worktree_repo
        .to_str()
        .ok_or_else(|| CommandError::from("worktree repository path is not valid UTF-8"))?;

    if anchor_repo == worktree_repo {
        return Ok(DevmapWorktreeProvision::skipped(
            "anchor and target worktree are the same checkout",
            start,
        ));
    }

    refuse_legacy_base_store_ref(anchor_repo)?;
    refuse_legacy_base_store_ref(worktree_repo)?;

    if !devmap_query::paths::store_path(anchor_repo).is_file()
        && !devmap_extract::paths::stack_lease_path(anchor_repo).is_file()
    {
        return Ok(DevmapWorktreeProvision::skipped(
            "anchor repository has no DevMap store",
            start,
        ));
    }

    let mut anchor = validate_repo(anchor_str)?;
    let worktree = validate_repo(worktree_str)?;
    if anchor == worktree {
        return Ok(DevmapWorktreeProvision::skipped(
            "anchor and target worktree are the same checkout",
            start,
        ));
    }
    if !devmap_query::paths::store_path(&anchor).is_file() {
        if let Some(donor) = resolve_stack_lease_donor(&anchor)? {
            anchor = donor;
        }
    }
    if anchor == worktree {
        return Ok(DevmapWorktreeProvision::skipped(
            "anchor and target worktree are the same checkout",
            start,
        ));
    }

    let anchor_common = crate::engine::git_cli::resolve_git_common_dir(&anchor)?;
    let worktree_common = crate::engine::git_cli::resolve_git_common_dir(&worktree)?;
    if anchor_common != worktree_common
        || !devmap_extract::paths::same_registered_repo(&anchor, &worktree)
    {
        return Err(CommandError::from(
            "refusing stacked DevMap lease across unrelated repositories",
        ));
    }

    let anchor_state = devmap_query::paths::state_dir(&anchor);
    let Ok(anchor_rel_state) = anchor_state.strip_prefix(&anchor) else {
        return Ok(DevmapWorktreeProvision::skipped(
            "DEVMAP_HOME points outside the anchor worktree",
            start,
        ));
    };
    let Some(anchor_state_name) = plain_single_dir_component(anchor_rel_state) else {
        return Ok(DevmapWorktreeProvision::skipped(
            "anchor DevMap state directory is not a top-level worktree directory",
            start,
        ));
    };

    let anchor_db = devmap_query::paths::store_path(&anchor);
    if !anchor_db.is_file() {
        return Ok(DevmapWorktreeProvision::skipped(
            "anchor repository has no DevMap store",
            start,
        ));
    }
    let anchor_store = match super::open_local_indexed_store(&anchor) {
        Ok(store) => store,
        Err(reason) => {
            return Ok(DevmapWorktreeProvision::skipped(
                format!("anchor DevMap store is not ready: {reason}"),
                start,
            ));
        }
    };
    let generation_id = match anchor_store.latest_generation_id() {
        Ok(Some(gen)) => gen,
        Ok(None) => {
            return Ok(DevmapWorktreeProvision::skipped(
                "anchor DevMap store holds no generation",
                start,
            ));
        }
        Err(e) => {
            return Ok(DevmapWorktreeProvision::skipped(
                format!("failed to read anchor generation: {e}"),
                start,
            ));
        }
    };
    let anchor_csr = Store::csr_sidecar_path(&anchor_db);
    if !anchor_csr.is_file()
        && ensure_dir_excluded(&anchor, anchor_state_name, DEVMAP_EXCLUDE_MARKER).is_clean()
    {
        let _ = materialize_csr_from_store(&anchor_store, &anchor_csr);
    }
    drop(anchor_store);

    let target_state_name = if worktree.join(devmap_query::paths::STATE_DIR).is_dir() {
        devmap_query::paths::STATE_DIR
    } else if worktree
        .join(devmap_query::paths::LEGACY_STATE_DIR)
        .is_dir()
    {
        devmap_query::paths::LEGACY_STATE_DIR
    } else {
        anchor_state_name
    };

    let exclude = ensure_dir_excluded(&worktree, target_state_name, DEVMAP_EXCLUDE_MARKER);
    if !exclude.is_clean() {
        return Err(CommandError::from(format!(
            "refusing to write DevMap stack lease into {} because {target_state_name}/ is not git-ignored: {exclude:?}",
            worktree.display()
        )));
    }

    // Create the excluded target state directory before `provision_stacked_worktree`
    // so `devmap_extract::paths::state_dir(&worktree)` picks `target_state_name` even
    // when the anchor uses `.devmap` (`LEGACY_STATE_DIR`).
    let target_codeintel = worktree.join(target_state_name).join("codeintel");
    fs::create_dir_all(&target_codeintel).map_err(|e| {
        CommandError::from(format!(
            "failed to create {}: {e}",
            target_codeintel.display()
        ))
    })?;

    let stack_lease = devmap_query::stacked::provision_stacked_worktree(&anchor, &worktree)
        .map_err(|e| CommandError::from(format!("failed to acquire DevMap stack lease: {e}")))?;
    let base_csr_candidate =
        devmap_extract::paths::csr_path_for_store(stack_lease.base_sqlite_path());

    Ok(DevmapWorktreeProvision {
        seeded: true,
        is_cow: false,
        used_online_backup: false,
        zero_copy_base_ref: true,
        generation_id: Some(generation_id),
        db_path: Some(
            stack_lease
                .base_sqlite_path()
                .to_string_lossy()
                .into_owned(),
        ),
        csr_seeded: base_csr_candidate.is_file() || anchor_csr.is_file(),
        repo_map_seeded: devmap_query::paths::repo_map_path(&worktree).is_file(),
        cas_seeded: false,
        overlay_path: None,
        duration_ms: start.elapsed().as_millis() as u64,
        reason: None,
    })
}

/// Alias of [`provision_worktree_devmap_zero_copy`] for canonical `StackedStore` provisioning.
pub fn provision_worktree_devmap_stacked(
    anchor_repo: &Path,
    worktree_repo: &Path,
) -> Result<DevmapWorktreeProvision, CommandError> {
    provision_worktree_devmap_zero_copy(anchor_repo, worktree_repo)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DeltaKind {
    Modified,
    Deleted,
}

fn parse_nul_diff_name_status(bytes: &[u8]) -> Vec<(String, DeltaKind)> {
    let mut fields = bytes.split(|&b| b == 0).filter(|s| !s.is_empty());
    let mut out = Vec::new();
    while let Some(status_raw) = fields.next() {
        let status = String::from_utf8_lossy(status_raw);
        let code = status.chars().next().unwrap_or('\0');
        match code {
            'R' | 'C' => {
                let Some(old_raw) = fields.next() else {
                    break;
                };
                let Some(new_raw) = fields.next() else {
                    break;
                };
                if let Ok(old_path) = std::str::from_utf8(old_raw) {
                    if code == 'R' {
                        out.push((old_path.to_string(), DeltaKind::Deleted));
                    }
                }
                if let Ok(new_path) = std::str::from_utf8(new_raw) {
                    out.push((new_path.to_string(), DeltaKind::Modified));
                }
            }
            'D' => {
                let Some(path_raw) = fields.next() else {
                    break;
                };
                if let Ok(path) = std::str::from_utf8(path_raw) {
                    out.push((path.to_string(), DeltaKind::Deleted));
                }
            }
            'M' | 'A' | 'T' | 'U' => {
                let Some(path_raw) = fields.next() else {
                    break;
                };
                if let Ok(path) = std::str::from_utf8(path_raw) {
                    out.push((path.to_string(), DeltaKind::Modified));
                }
            }
            _ => {
                let _ = fields.next();
            }
        }
    }
    out
}

fn parse_nul_porcelain_status(bytes: &[u8]) -> Vec<(String, DeltaKind)> {
    let mut entries = bytes.split(|&b| b == 0).filter(|s| !s.is_empty());
    let mut out = Vec::new();
    while let Some(entry) = entries.next() {
        if entry.len() < 4 {
            continue;
        }
        let x = entry[0] as char;
        let y = entry[1] as char;
        let Ok(path) = std::str::from_utf8(&entry[3..]) else {
            continue;
        };
        if matches!(x, 'R' | 'C') {
            out.push((path.to_string(), DeltaKind::Modified));
            if let Some(orig_raw) = entries.next() {
                if x == 'R' {
                    if let Ok(orig) = std::str::from_utf8(orig_raw) {
                        out.push((orig.to_string(), DeltaKind::Deleted));
                    }
                }
            }
            continue;
        }
        if x == 'D' || y == 'D' {
            out.push((path.to_string(), DeltaKind::Deleted));
        } else if x != '!' {
            out.push((path.to_string(), DeltaKind::Modified));
        }
    }
    out
}

/// Folds `git ls-files --others` output into `map`. A failed or cut-short listing is an
/// error: reading it as "no untracked files" would silently drop every new file.
fn merge_untracked_listing(
    map: &mut HashMap<String, DeltaKind>,
    run: BoundedRun,
) -> Result<(), CommandError> {
    if run.status_code != 0 {
        return Err(CommandError::from(format!(
            "git ls-files --others failed with code {}: {}",
            run.status_code,
            String::from_utf8_lossy(&run.stderr).trim()
        )));
    }
    let run = run.require_complete("git ls-files --others")?;
    for raw in run.stdout.split(|&b| b == 0).filter(|s| !s.is_empty()) {
        if let Ok(path) = std::str::from_utf8(raw) {
            map.insert(path.to_string(), DeltaKind::Modified);
        }
    }
    Ok(())
}

pub(super) fn collect_worktree_deltas(
    repo: &Path,
    base_head_sha: Option<&str>,
) -> Result<(Vec<(String, DeltaKind)>, bool), CommandError> {
    let mut map: HashMap<String, DeltaKind> = HashMap::new();

    let mut diffed_against_base = false;
    if let Some(sha) = base_head_sha.filter(|s| !s.is_empty()) {
        if let Ok(run) = git_captured(repo, &["diff", "--name-status", "-z", sha, "--"]) {
            if run.status_code == 0 && run.incomplete.is_none() {
                diffed_against_base = true;
                for (path, kind) in parse_nul_diff_name_status(&run.stdout) {
                    map.insert(path, kind);
                }
            }
        }
    }

    if diffed_against_base {
        let untracked = git_captured(repo, &["ls-files", "--others", "--exclude-standard", "-z"])?;
        merge_untracked_listing(&mut map, untracked)?;
    } else {
        let status = git_captured(repo, &["status", "--porcelain=v1", "-z", "-uall"])?;
        if status.status_code != 0 {
            return Err(CommandError::from(format!(
                "git status failed with code {}: {}",
                status.status_code,
                String::from_utf8_lossy(&status.stderr).trim()
            )));
        }
        let status = status.require_complete("git status")?;
        for (path, kind) in parse_nul_porcelain_status(&status.stdout) {
            map.insert(path, kind);
        }
    }

    map.retain(|path, _| {
        !devmap_query::paths::STATE_DIR_NAMES
            .iter()
            .any(|dir| path == *dir || path.starts_with(&format!("{dir}/")))
            && path != ".gitpulse"
            && !path.starts_with(".gitpulse/")
    });

    let mut sorted: Vec<(String, DeltaKind)> = map.into_iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let truncated = sorted.len() > MAX_OVERLAY_DIRTY_FILES;
    if truncated {
        sorted.truncate(MAX_OVERLAY_DIRTY_FILES);
    }
    Ok((sorted, truncated))
}

struct CsrDigestLookup {
    by_name_hash: HashMap<u64, Vec<u32>>,
    by_file_hash: HashMap<u64, Vec<u32>>,
}

impl CsrDigestLookup {
    fn from_reader(reader: &devmap_store::CsrGraphReader) -> Self {
        let mut by_name_hash: HashMap<u64, Vec<u32>> = HashMap::new();
        let mut by_file_hash: HashMap<u64, Vec<u32>> = HashMap::new();
        for idx in 0..reader.node_count() {
            let node_id = idx as u32;
            if let Some(rec) = reader.node(node_id) {
                if rec.name_hash != 0 {
                    by_name_hash
                        .entry(rec.name_hash)
                        .or_default()
                        .push(rec.node_id);
                }
                if rec.file_path_hash != 0 {
                    by_file_hash
                        .entry(rec.file_path_hash)
                        .or_default()
                        .push(rec.node_id);
                }
            }
        }
        Self {
            by_name_hash,
            by_file_hash,
        }
    }

    fn matching_nodes(&self, file_path: &str, symbol_qualified_names: &[String]) -> BTreeSet<u32> {
        let mut ids = BTreeSet::new();
        let f_hash = IncrementalDigestEngine::hash_bytes(file_path.as_bytes());
        if let Some(list) = self.by_file_hash.get(&f_hash) {
            ids.extend(list.iter().copied());
        }
        for sym in symbol_qualified_names {
            let s_hash = IncrementalDigestEngine::hash_bytes(sym.as_bytes());
            if let Some(list) = self.by_name_hash.get(&s_hash) {
                ids.extend(list.iter().copied());
            }
        }
        ids
    }
}

pub(super) fn read_bounded_file(path: &Path, cap: u64) -> Result<Option<Vec<u8>>, CommandError> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(CommandError::from(format!(
                "cannot stat {}: {e}",
                path.display()
            )))
        }
    };
    if !meta.file_type().is_file() || meta.len() > cap {
        return Ok(None);
    }
    let mut file = fs::File::open(path)
        .map_err(|e| CommandError::from(format!("cannot open {}: {e}", path.display())))?;
    let mut buf = Vec::with_capacity(meta.len() as usize);
    (&mut file)
        .take(cap + 1)
        .read_to_end(&mut buf)
        .map_err(|e| CommandError::from(format!("cannot read {}: {e}", path.display())))?;
    if buf.len() as u64 > cap {
        return Ok(None);
    }
    Ok(Some(buf))
}

pub(super) fn is_sparse_worktree(repo: &Path) -> bool {
    git_captured(repo, &["config", "--get", "core.sparseCheckout"])
        .map(|run| {
            run.status_code == 0
                && matches!(
                    String::from_utf8_lossy(&run.stdout)
                        .trim()
                        .to_ascii_lowercase()
                        .as_str(),
                    "true" | "yes" | "on" | "1"
                )
        })
        .unwrap_or(false)
}

/// What the spawn-gate report calls the `rev-parse` launches [`head_sha_of`]
/// makes.
pub(crate) const HEAD_SHA_SOURCE: &str = "codeintel.overlay.head_sha";

fn head_sha_of(repo: &Path) -> Option<String> {
    crate::engine::git_cli::with_spawn_source(HEAD_SHA_SOURCE, || {
        git_captured(repo, &["rev-parse", "--verify", "HEAD"])
    })
    .ok()
    .filter(|run| run.status_code == 0)
    .map(|run| String::from_utf8_lossy(&run.stdout).trim().to_string())
    .filter(|s| !s.is_empty())
}

/// Evaluates source freshness of a stacked `worktree` against `anchor_store`'s
/// latest generation instead of reporting `anchor`'s own working-tree freshness.
pub(super) fn verify_zero_copy_worktree_freshness(
    worktree: &Path,
    anchor: &Path,
    anchor_store: &Store,
    anchor_summary: &devmap_store::StoreStatus,
) -> (Option<bool>, Option<String>) {
    if anchor_summary.latest_generation.is_none() || anchor_summary.pending_count > 0 {
        return (None, anchor_summary.freshness_reason());
    }
    if is_sparse_worktree(worktree) {
        let gen_head = anchor_store.latest_generation_head_sha().ok().flatten();
        let wt_head = head_sha_of(worktree);
        if let Some(ref gh) = gen_head {
            if wt_head.as_deref() != Some(gh.as_str()) {
                return (
                    Some(false),
                    Some(
                        "sparse worktree HEAD differs from the indexed anchor generation; rebuild or sync delta"
                            .to_string(),
                    ),
                );
            }
        }
        match collect_worktree_deltas(worktree, gen_head.as_deref()) {
            Err(e) => {
                return (
                    None,
                    Some(format!("source freshness unverified: {}", e.message())),
                )
            }
            Ok((deltas, _)) if !deltas.is_empty() => {
                return (
                    Some(false),
                    Some(
                        "sparse worktree differs from the indexed anchor generation; rebuild or sync delta"
                            .to_string(),
                    ),
                );
            }
            Ok(_) => {}
        }
        let anchor_matches_gen = anchor_summary.source_freshness == Some(true)
            && gen_head
                .as_deref()
                .is_some_and(|gh| head_sha_of(anchor).as_deref() == Some(gh));
        if anchor_matches_gen {
            return (Some(true), None);
        }
        return (
            None,
            Some(
                "source freshness unverified: sparse stacked worktree cannot verify unmaterialized paths while anchor checkout differs from indexed generation"
                    .to_string(),
            ),
        );
    }

    let hashes = match anchor_store.latest_file_hashes() {
        Ok(h) => h,
        Err(e) => return (None, Some(format!("source freshness unverified: {e}"))),
    };
    let stored = match anchor_store.latest_discovery_inventory() {
        Ok(inv) => inv,
        Err(e) => return (None, Some(format!("source freshness unverified: {e}"))),
    };
    let scanned = match devmap_extract::scan_tree(worktree) {
        Ok(s) => s,
        Err(e) => return (None, Some(format!("source freshness unverified: {e}"))),
    };
    let (delta, _) =
        scanned.file_delta_with_samples(&hashes, devmap_store::SourceTreeDelta::SAMPLE);
    if !delta.is_unchanged() {
        return (
            Some(false),
            Some(
                "worktree source tree differs from the indexed anchor generation; rebuild or sync delta"
                    .to_string(),
            ),
        );
    }
    let manifests = match devmap_extract::collect_project_manifests(worktree) {
        Ok(m) => m,
        Err(e) => {
            return (
                None,
                Some(format!(
                    "source freshness unverified: project manifests could not be read ({e})"
                )),
            )
        }
    };
    let current = devmap_store::DiscoveryInventory::of(&scanned.report, &manifests);
    if current.refusals != stored.refusals {
        return (
            Some(false),
            Some(
                "worktree source discovery refusals differ from the indexed generation; rebuild required"
                    .to_string(),
            ),
        );
    }
    if current.over_limit != stored.over_limit {
        return (
            Some(false),
            Some(
                "worktree source files over the size limit differ from the indexed generation; rebuild required"
                    .to_string(),
            ),
        );
    }
    (Some(true), None)
}

fn file_scoped_symbol_id(rel_path: &str, sym: &str) -> String {
    let prefix = format!("{rel_path}::");
    if sym.starts_with(&prefix) {
        sym.to_string()
    } else {
        format!("{rel_path}::{sym}")
    }
}

/// Replaces `StackedStore::overlay().dirty_nodes` (which carries ordinal delta file
/// indices) with true CSR node IDs from `csr_reader`, and ensures file-scoped
/// symbol tombstones are populated from `base_store.latest_symbols_for_file`.
fn enrich_stacked_overlay_with_csr(
    overlay: &mut WorktreeOverlay,
    base_store: &Store,
    csr_reader: Option<&devmap_store::CsrGraphReader>,
) {
    overlay.dirty_nodes = devmap_query::RoaringBitmap::new();
    let csr_lookup = csr_reader
        .filter(|r| r.generation() == overlay.generation)
        .map(CsrDigestLookup::from_reader);

    let modified_paths: Vec<String> = overlay
        .modified_files
        .keys()
        .filter_map(|p| p.to_str().map(str::to_owned))
        .collect();
    let tombstoned_paths: Vec<String> = overlay
        .tombstoned_files
        .iter()
        .filter_map(|p| p.to_str().map(str::to_owned))
        .collect();

    for rel_path in &tombstoned_paths {
        let rows = base_store
            .latest_symbols_for_file(rel_path)
            .ok()
            .flatten()
            .map(|p| p.rows)
            .unwrap_or_default();
        let mut qnames = Vec::with_capacity(rows.len() * 2);
        for row in &rows {
            if !row.kind.eq_ignore_ascii_case("file") {
                overlay.mark_symbol_tombstone(file_scoped_symbol_id(rel_path, &row.name), None);
                overlay.mark_symbol_tombstone(
                    file_scoped_symbol_id(rel_path, &row.qualified_name),
                    None,
                );
            }
            qnames.push(row.name.clone());
            if row.qualified_name != row.name {
                qnames.push(row.qualified_name.clone());
            }
        }
        if let Some(ref lookup) = csr_lookup {
            for nid in lookup.matching_nodes(rel_path, &qnames) {
                overlay.dirty_nodes.insert(nid);
            }
        }
    }

    for rel_path in &modified_paths {
        let rows = base_store
            .latest_symbols_for_file(rel_path)
            .ok()
            .flatten()
            .map(|p| p.rows)
            .unwrap_or_default();
        let mut qnames = Vec::with_capacity(rows.len() * 2);
        let missing_tombstones: Vec<String> = if let Some(ext) =
            overlay.added_extractions.get(rel_path.as_str())
        {
            let live_names: std::collections::HashSet<&str> = ext
                .symbols
                .iter()
                .filter(|s| !matches!(s.kind, devmap_extract::SymbolKind::File))
                .map(|s| s.name.as_str())
                .collect();
            let mut missing = Vec::new();
            for row in &rows {
                if !row.kind.eq_ignore_ascii_case("file") && !live_names.contains(row.name.as_str())
                {
                    missing.push(file_scoped_symbol_id(rel_path, &row.name));
                    missing.push(file_scoped_symbol_id(rel_path, &row.qualified_name));
                }
            }
            missing
        } else {
            Vec::new()
        };
        for tomb in missing_tombstones {
            overlay.mark_symbol_tombstone(tomb, None);
        }
        for row in &rows {
            qnames.push(row.name.clone());
            if row.qualified_name != row.name {
                qnames.push(row.qualified_name.clone());
            }
        }
        if let Some(ref lookup) = csr_lookup {
            for nid in lookup.matching_nodes(rel_path, &qnames) {
                overlay.dirty_nodes.insert(nid);
            }
        }
    }
}

fn insert_cone_symbol_and_identifiers(
    raw: &str,
    cone_paths: &mut std::collections::HashSet<String>,
    cone_symbols: &mut std::collections::HashSet<String>,
    cone_identifiers: &mut std::collections::HashSet<String>,
) {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return;
    }
    cone_symbols.insert(trimmed.to_string());
    if let Some((before_colons, after_colons)) = trimmed.split_once("::") {
        if before_colons.contains('/') || before_colons.contains('.') {
            cone_paths.insert(before_colons.to_string());
            if let Some(stem) = Path::new(before_colons)
                .file_stem()
                .and_then(|s| s.to_str())
                .filter(|s| !s.is_empty())
            {
                cone_identifiers.insert(stem.to_string());
            }
        }
        let tail = after_colons.rsplit("::").next().unwrap_or(after_colons);
        let ident = tail.rsplit('.').next().unwrap_or(tail);
        if !ident.is_empty() {
            cone_identifiers.insert(ident.to_string());
        }
    } else if trimmed.contains('/') {
        cone_paths.insert(trimmed.to_string());
        if let Some(stem) = Path::new(trimmed)
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
        {
            cone_identifiers.insert(stem.to_string());
        }
    } else {
        let ident = trimmed.rsplit('.').next().unwrap_or(trimmed);
        if !ident.is_empty() {
            cone_identifiers.insert(ident.to_string());
        }
    }
}

/// Verifies that every file divergence between `anchor_store`'s latest generation
/// and `worktree` on disk is accounted for in `stacked` (`delta.sqlite`) with
/// matching current blake3 content hashes and file tombstones.
pub(super) fn is_stacked_delta_fully_synced(
    worktree: &Path,
    anchor_store: &Store,
    stacked: &devmap_query::StackedStore,
) -> bool {
    let ov = stacked.overlay();
    for (rel_path, expected_b3) in &ov.modified_files {
        let Ok(Some(bytes)) = read_bounded_file(&worktree.join(rel_path), MAX_OVERLAY_FILE_BYTES)
        else {
            return false;
        };
        if devmap_query::blake3_hash(&bytes) != *expected_b3 {
            return false;
        }
    }
    for tombstoned in &ov.tombstoned_files {
        if worktree.join(tombstoned).exists() {
            return false;
        }
    }

    if is_sparse_worktree(worktree) {
        let Some(gen_head) = anchor_store.latest_generation_head_sha().ok().flatten() else {
            return false;
        };
        if head_sha_of(worktree).as_deref() != Some(gen_head.as_str()) {
            return false;
        }
        let Ok((deltas, truncated)) = collect_worktree_deltas(worktree, Some(&gen_head)) else {
            return false;
        };
        if truncated || deltas.len() > MAX_OVERLAY_DIRTY_FILES {
            return false;
        }
        for (rel_path, kind) in deltas {
            match kind {
                DeltaKind::Deleted => {
                    if !ov.is_file_tombstoned(Path::new(&rel_path))
                        || worktree.join(&rel_path).exists()
                    {
                        return false;
                    }
                }
                DeltaKind::Modified => {
                    if !devmap_extract::languages::is_indexable_source(&rel_path) {
                        continue;
                    }
                    let Ok(Some(bytes)) =
                        read_bounded_file(&worktree.join(&rel_path), MAX_OVERLAY_FILE_BYTES)
                    else {
                        return false;
                    };
                    if ov.modified_files.get(Path::new(&rel_path))
                        != Some(&devmap_query::blake3_hash(&bytes))
                    {
                        return false;
                    }
                }
            }
        }
        return true;
    }

    let Ok(hashes) = anchor_store.latest_file_hashes() else {
        return false;
    };
    let Ok(stored) = anchor_store.latest_discovery_inventory() else {
        return false;
    };
    let Ok(scanned) = devmap_extract::scan_tree(worktree) else {
        return false;
    };
    let Ok(manifests) = devmap_extract::collect_project_manifests(worktree) else {
        return false;
    };
    let current = devmap_store::DiscoveryInventory::of(&scanned.report, &manifests);
    if current.refusals != stored.refusals || current.over_limit != stored.over_limit {
        return false;
    }
    let scanned_hashes: std::collections::HashMap<&str, u64> =
        scanned.file_hashes().into_iter().collect();
    for indexed_path in hashes.keys() {
        if !scanned_hashes.contains_key(indexed_path.as_str())
            && !ov.is_file_tombstoned(Path::new(indexed_path))
        {
            return false;
        }
    }
    for (path, scan_hash) in scanned_hashes {
        if hashes.get(path) != Some(&scan_hash) {
            if !devmap_extract::languages::is_indexable_source(path) {
                continue;
            }
            let Ok(Some(bytes)) = read_bounded_file(&worktree.join(path), MAX_OVERLAY_FILE_BYTES)
            else {
                return false;
            };
            if ov.modified_files.get(Path::new(path)) != Some(&devmap_query::blake3_hash(&bytes)) {
                return false;
            }
        }
    }
    true
}

/// Proves whether `candidate` (`affected_tests` computed from `anchor_store`) is
/// graph-theoretically invariant under `worktree`'s dirty deltas (`F4`).
pub(super) fn verify_zero_copy_affected_tests_disjoint_from_deltas(
    worktree: &Path,
    anchor: &Path,
    anchor_store: &Store,
    anchor_summary: &devmap_store::StoreStatus,
    targets: &[String],
    candidate: &super::CodeintelAffectedTests,
) -> bool {
    if !candidate.available
        || candidate.fail_closed
        || candidate.blast_radius.seeds.is_empty()
        || candidate
            .blast_radius
            .layers
            .items
            .iter()
            .any(|layer| layer.nodes_omitted > 0)
    {
        return false;
    }

    let modified_paths: Vec<String> = if is_sparse_worktree(worktree) {
        let Some(gen_head) = anchor_store.latest_generation_head_sha().ok().flatten() else {
            return false;
        };
        if head_sha_of(worktree).as_deref() != Some(gen_head.as_str()) {
            return false;
        }
        let anchor_matches_gen = anchor_summary.source_freshness == Some(true)
            && head_sha_of(anchor).as_deref() == Some(gen_head.as_str());
        if !anchor_matches_gen {
            return false;
        }
        let Ok((deltas, truncated)) = collect_worktree_deltas(worktree, Some(&gen_head)) else {
            return false;
        };
        if truncated || deltas.is_empty() || deltas.len() > MAX_OVERLAY_DIRTY_FILES {
            return false;
        }
        let mut paths = Vec::with_capacity(deltas.len());
        for (rel_path, kind) in deltas {
            match kind {
                DeltaKind::Modified => paths.push(rel_path),
                DeltaKind::Deleted => return false,
            }
        }
        paths
    } else {
        let Ok(hashes) = anchor_store.latest_file_hashes() else {
            return false;
        };
        let Ok(stored) = anchor_store.latest_discovery_inventory() else {
            return false;
        };
        let Ok(scanned) = devmap_extract::scan_tree(worktree) else {
            return false;
        };
        let Ok(manifests) = devmap_extract::collect_project_manifests(worktree) else {
            return false;
        };
        let current = devmap_store::DiscoveryInventory::of(&scanned.report, &manifests);
        if current.refusals != stored.refusals || current.over_limit != stored.over_limit {
            return false;
        }
        let scanned_hashes: std::collections::HashMap<&str, u64> =
            scanned.file_hashes().into_iter().collect();
        for indexed_path in hashes.keys() {
            if !scanned_hashes.contains_key(indexed_path.as_str()) {
                return false;
            }
        }
        let mut changed = Vec::new();
        for (path, scan_hash) in scanned_hashes {
            if hashes.get(path) != Some(&scan_hash) {
                changed.push(path.to_string());
                if changed.len() > MAX_OVERLAY_DIRTY_FILES {
                    return false;
                }
            }
        }
        if changed.is_empty() {
            return false;
        }
        changed
    };

    let mut cone_paths = std::collections::HashSet::new();
    let mut cone_symbols = std::collections::HashSet::new();
    let mut cone_identifiers = std::collections::HashSet::new();

    for t in targets {
        insert_cone_symbol_and_identifiers(
            t,
            &mut cone_paths,
            &mut cone_symbols,
            &mut cone_identifiers,
        );
    }
    for seed in &candidate.blast_radius.seeds {
        insert_cone_symbol_and_identifiers(
            seed,
            &mut cone_paths,
            &mut cone_symbols,
            &mut cone_identifiers,
        );
    }
    for layer in &candidate.blast_radius.layers.items {
        for node in &layer.nodes {
            insert_cone_symbol_and_identifiers(
                node,
                &mut cone_paths,
                &mut cone_symbols,
                &mut cone_identifiers,
            );
        }
    }
    for test in &candidate.tests.items {
        insert_cone_symbol_and_identifiers(
            &test.path,
            &mut cone_paths,
            &mut cone_symbols,
            &mut cone_identifiers,
        );
        for sym in &test.symbols {
            insert_cone_symbol_and_identifiers(
                sym,
                &mut cone_paths,
                &mut cone_symbols,
                &mut cone_identifiers,
            );
        }
    }
    if cone_identifiers.is_empty() {
        return false;
    }

    for rel_path in &modified_paths {
        if cone_paths.contains(rel_path.as_str()) {
            return false;
        }
        match anchor_store.latest_symbols_for_file(rel_path) {
            Ok(Some(page)) => {
                if page.rows.iter().any(|row| {
                    cone_symbols.contains(row.name.as_str())
                        || cone_symbols.contains(row.qualified_name.as_str())
                        || cone_identifiers.contains(row.name.as_str())
                }) {
                    return false;
                }
            }
            Ok(None) => {}
            Err(_) => return false,
        }
        let lang = devmap_extract::detect_language(Path::new(rel_path));
        if devmap_extract::fallback::applies_to(lang) {
            let full_path = worktree.join(rel_path);
            let Ok(Some(bytes)) = read_bounded_file(&full_path, MAX_OVERLAY_FILE_BYTES) else {
                return false;
            };
            let Ok(src) = std::str::from_utf8(&bytes) else {
                return false;
            };
            let cleaned = strip_comments_preserving_identifiers(src);
            if cleaned.contains("::*")
                || cleaned.contains("export *")
                || cleaned.contains("import *")
            {
                return false;
            }
            if cone_identifiers
                .iter()
                .any(|ident| contains_identifier_token(&cleaned, ident))
            {
                return false;
            }
        }
    }

    true
}

/// Maps the lane's CSR sidecar after checking it against the sha256 the stack
/// lease admitted. `Ok(None)` means the lease records no sidecar; a sidecar
/// that fails its hash is an error, never mapped.
pub(super) fn open_verified_stacked_csr(
    meta: &StackedBaseMeta,
    stacked: &devmap_query::StackedStore,
) -> Result<Option<devmap_store::CsrGraphReader>, CommandError> {
    let (Some(record), Some(csr_path)) =
        (stacked.lease().csr.as_ref(), meta.base_csr_path.as_ref())
    else {
        return Ok(None);
    };
    let actual = devmap_query::stacked::sha256_file_hex(csr_path)
        .map_err(|e| CommandError::from(format!("Failed to verify stacked CSR graph: {e}")))?;
    if actual != record.sha256 {
        return Err(CommandError::from(format!(
            "Failed to verify stacked CSR graph: {} hashes to {actual}, but the stack lease admitted {}",
            csr_path.display(),
            record.sha256
        )));
    }
    devmap_store::CsrGraphReader::open_mmap(csr_path)
        .map(Some)
        .map_err(|e| CommandError::from(format!("Failed to open CSR graph: {e}")))
}

/// Opens the mmap [`devmap_store::CsrGraphReader`] for `repo`, preferring the
/// verified `StackedStore` base snapshot in sparse checkouts (`F2`).
pub(crate) fn open_worktree_or_anchor_csr(
    repo: &Path,
) -> Option<(devmap_store::CsrGraphReader, u64)> {
    let prefer_stacked = is_sparse_worktree(repo) && has_stack_lease_or_legacy_ref(repo);
    if !prefer_stacked {
        if let Ok(store) = super::open_local_indexed_store(repo) {
            if let Ok(Some(reader)) = store.open_latest_csr_sidecar() {
                let gen = reader.generation();
                return Some((reader, gen));
            }
        }
    }
    if let Ok(Some((base_store, meta, stacked))) = open_verified_stacked_store(repo) {
        match open_verified_stacked_csr(&meta, &stacked) {
            Ok(Some(reader)) => {
                let gen = reader.generation();
                return Some((reader, gen));
            }
            Ok(None) => {
                if let Ok(Some(reader)) = base_store.open_latest_csr_sidecar() {
                    let gen = reader.generation();
                    return Some((reader, gen));
                }
            }
            Err(_) => return None,
        }
    }
    if prefer_stacked {
        if let Ok(store) = super::open_local_indexed_store(repo) {
            if let Ok(Some(reader)) = store.open_latest_csr_sidecar() {
                let gen = reader.generation();
                return Some((reader, gen));
            }
        }
    }
    None
}

/// Explicitly synchronizes a stacked worktree's `delta.sqlite` using real
/// Tree-sitter extraction (`devmap --root <wt> seed --sync-delta --json` under
/// `StackedStore`'s writer lock), or computes an in-memory delta summary for
/// non-stacked checkouts without writing any `overlay.json` or `CasAstStore`.
pub fn sync_worktree_overlay(repo_path: &str) -> Result<WorktreeOverlayReport, CommandError> {
    let start = Instant::now();
    let repo = validate_repo(repo_path)?;
    refuse_legacy_base_store_ref(&repo)?;

    if devmap_extract::paths::stack_lease_path(&repo).is_file() {
        let (base_store, meta, existing_stacked) = open_verified_stacked_store(&repo)?
            .ok_or_else(|| CommandError::from("stacked lease vanished during sync"))?;
        let (deltas, truncated) = collect_worktree_deltas(&repo, meta.base_head_sha.as_deref())?;
        let mut skipped_large_files = 0usize;
        for (rel_path, kind) in &deltas {
            if *kind == DeltaKind::Modified {
                let full_path = repo.join(rel_path);
                if read_bounded_file(&full_path, MAX_OVERLAY_FILE_BYTES)?.is_none()
                    && full_path.exists()
                {
                    skipped_large_files += 1;
                }
            }
        }

        let delta_path = devmap_extract::paths::stack_delta_path(&repo);
        // Clean stacked worktree fast-path: zero files diverged and no prior
        // `delta.sqlite` to clear -> return in <1 ms without creating `delta.sqlite`.
        if deltas.is_empty() && !existing_stacked.delta_present() {
            return Ok(WorktreeOverlayReport {
                overlay_path: delta_path.to_string_lossy().into_owned(),
                generation_id: u64::from(meta.generation_id),
                base_head_sha: meta.base_head_sha,
                modified_files: 0,
                tombstoned_files: 0,
                tombstoned_symbols: 0,
                dirty_nodes: 0,
                reused_cas_asts: 0,
                skipped_large_files: 0,
                truncated: false,
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }

        let state_dir = devmap_query::paths::state_dir(&repo);
        if let Ok(rel) = state_dir.strip_prefix(&repo) {
            if let Some(dir_name) = plain_single_dir_component(rel) {
                let exclude = ensure_dir_excluded(&repo, dir_name, DEVMAP_EXCLUDE_MARKER);
                if !exclude.is_clean() {
                    return Err(CommandError::from(format!(
                        "refusing to sync stacked delta in {} because {dir_name}/ is not git-ignored: {exclude:?}",
                        repo.display()
                    )));
                }
            }
        }

        drop(existing_stacked);
        drop(base_store);

        let repo_canon_str = repo.to_string_lossy();
        let outcome = crate::devmap::cli::sync_stacked_delta(&repo_canon_str).map_err(|e| {
            CommandError::from(format!("failed to sync stacked delta via devmap: {e}"))
        })?;
        if !outcome.ok {
            let detail = if outcome.timed_out {
                "devmap seed --sync-delta timed out".to_string()
            } else if !outcome.stderr.trim().is_empty() {
                outcome.stderr.trim().to_string()
            } else if !outcome.stdout.trim().is_empty() {
                outcome.stdout.trim().to_string()
            } else {
                format!("exit code {:?}", outcome.exit_code)
            };
            return Err(CommandError::from(format!(
                "failed to sync stacked delta via devmap: {detail}"
            )));
        }

        // If `StackedStore::sync_worktree` auto-rebased (>250 dirty files or >16 MiB)
        // into a standalone CoW `devmap.sqlite`, report the standalone DB cleanly.
        if !devmap_extract::paths::stack_lease_path(&repo).is_file() {
            let local = super::open_local_indexed_store(&repo).map_err(CommandError::from)?;
            let gen_id = local.latest_generation_id().ok().flatten().unwrap_or(0);
            let head_sha = local.latest_generation_head_sha().ok().flatten();
            return Ok(WorktreeOverlayReport {
                overlay_path: devmap_query::paths::store_path(&repo)
                    .to_string_lossy()
                    .into_owned(),
                generation_id: u64::from(gen_id),
                base_head_sha: head_sha,
                modified_files: 0,
                tombstoned_files: 0,
                tombstoned_symbols: 0,
                dirty_nodes: 0,
                reused_cas_asts: 0,
                skipped_large_files,
                truncated,
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }

        let (reopened_base, reopened_meta, reopened_stacked) = open_verified_stacked_store(&repo)?
            .ok_or_else(|| CommandError::from("stacked store missing after sync"))?;
        let mut ov = reopened_stacked.overlay().clone();
        let csr_opt = open_worktree_or_anchor_csr(&repo);
        enrich_stacked_overlay_with_csr(&mut ov, &reopened_base, csr_opt.as_ref().map(|(r, _)| r));

        return Ok(WorktreeOverlayReport {
            overlay_path: delta_path.to_string_lossy().into_owned(),
            generation_id: ov.generation,
            base_head_sha: reopened_meta.base_head_sha,
            modified_files: ov.modified_files.len(),
            tombstoned_files: ov.tombstoned_files.len(),
            tombstoned_symbols: ov.tombstoned_symbols.len(),
            dirty_nodes: ov.dirty_nodes.len(),
            reused_cas_asts: 0,
            skipped_large_files,
            truncated,
            duration_ms: start.elapsed().as_millis() as u64,
        });
    }

    // Non-stacked checkout: compute delta + CSR summary purely in memory without
    // writing any `overlay.json` or `CasAstStore` sidecars.
    let local_store = super::open_local_indexed_store(&repo).ok();
    let generation_id = local_store
        .as_ref()
        .and_then(|s| s.latest_generation_id().ok().flatten())
        .unwrap_or(0);
    let base_head_sha = local_store
        .as_ref()
        .and_then(|s| s.latest_generation_head_sha().ok().flatten());
    let (deltas, truncated) = collect_worktree_deltas(&repo, base_head_sha.as_deref())?;
    let csr_lookup = if deltas.is_empty() {
        None
    } else {
        local_store
            .as_ref()
            .and_then(|s| s.open_latest_csr_sidecar().ok().flatten())
            .map(|reader| CsrDigestLookup::from_reader(&reader))
    };

    let mut modified_files = 0usize;
    let mut tombstoned_files = 0usize;
    let mut tombstoned_symbols = BTreeSet::new();
    let mut dirty_nodes = devmap_query::RoaringBitmap::new();
    let mut skipped_large_files = 0usize;

    for (rel_path, kind) in deltas {
        let indexed_rows = local_store
            .as_ref()
            .and_then(|s| s.latest_symbols_for_file(&rel_path).ok().flatten())
            .map(|p| p.rows)
            .unwrap_or_default();
        let mut qnames = Vec::with_capacity(indexed_rows.len() * 2);
        for r in &indexed_rows {
            qnames.push(r.name.clone());
            if r.qualified_name != r.name {
                qnames.push(r.qualified_name.clone());
            }
        }
        if let Some(ref lookup) = csr_lookup {
            for nid in lookup.matching_nodes(&rel_path, &qnames) {
                dirty_nodes.insert(nid);
            }
        }
        match kind {
            DeltaKind::Deleted => {
                tombstoned_files += 1;
                for row in &indexed_rows {
                    if !row.kind.eq_ignore_ascii_case("file") {
                        tombstoned_symbols.insert(file_scoped_symbol_id(&rel_path, &row.name));
                        tombstoned_symbols
                            .insert(file_scoped_symbol_id(&rel_path, &row.qualified_name));
                    }
                }
            }
            DeltaKind::Modified => {
                let full_path = repo.join(&rel_path);
                match read_bounded_file(&full_path, MAX_OVERLAY_FILE_BYTES)? {
                    Some(_) => modified_files += 1,
                    None if !full_path.exists() => {
                        tombstoned_files += 1;
                        for row in &indexed_rows {
                            if !row.kind.eq_ignore_ascii_case("file") {
                                tombstoned_symbols
                                    .insert(file_scoped_symbol_id(&rel_path, &row.name));
                                tombstoned_symbols
                                    .insert(file_scoped_symbol_id(&rel_path, &row.qualified_name));
                            }
                        }
                    }
                    None => skipped_large_files += 1,
                }
            }
        }
    }

    Ok(WorktreeOverlayReport {
        overlay_path: String::new(),
        generation_id: u64::from(generation_id),
        base_head_sha,
        modified_files,
        tombstoned_files,
        tombstoned_symbols: tombstoned_symbols.len(),
        dirty_nodes: dirty_nodes.len(),
        reused_cas_asts: 0,
        skipped_large_files,
        truncated,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}

/// Reads the canonical [`WorktreeOverlay`] projected from a stacked worktree's
/// `delta.sqlite` (enriched with CSR `dirty_nodes`), strictly read-only (`Owner Stop #1`).
///
/// Returns `Ok(None)` when the checkout has no `stack_lease.json` or when no
/// `delta.sqlite` exists yet, and returns `Err` when `stack_lease.json`,
/// `base.sqlite`, or `delta.sqlite` is corrupt or when a retired `base_store_ref.json`
/// is encountered (`Owner Stop #2`).
pub fn load_worktree_overlay(repo_path: &str) -> Result<Option<WorktreeOverlay>, CommandError> {
    let repo = validate_repo(repo_path)?;
    let Some((base_store, _meta, stacked)) = open_verified_stacked_store(&repo)? else {
        return Ok(None);
    };
    if !stacked.delta_present() {
        return Ok(None);
    }
    let mut ov = stacked.overlay().clone();
    let csr_opt = open_worktree_or_anchor_csr(&repo);
    enrich_stacked_overlay_with_csr(&mut ov, &base_store, csr_opt.as_ref().map(|(r, _)| r));
    Ok(Some(ov))
}

/// What a read-only dirty-node scan found, and whether it is the whole answer.
#[derive(Debug, Default)]
pub(crate) struct DirtyNodeScan {
    pub dirty_nodes: devmap_query::RoaringBitmap,
    /// The worktree's changed-file list was capped at [`MAX_OVERLAY_DIRTY_FILES`], so
    /// `dirty_nodes` covers only the files that fit.
    pub truncated: bool,
}

/// Computes live [`RoaringBitmap`] CSR `dirty_nodes` for `repo_path` purely in memory
/// without modifying `.git/info/exclude`, creating `.devmap`/`.devcouncil` directories,
/// or touching `delta.sqlite` (`Owner Stop #1`, `readOnlyHint: true`).
pub(crate) fn compute_worktree_dirty_nodes_readonly(
    repo_path: &str,
    fallback_anchor: Option<&Path>,
) -> Result<DirtyNodeScan, CommandError> {
    let repo = validate_repo(repo_path)?;
    refuse_legacy_base_store_ref(&repo)?;

    let prefer_stacked = is_sparse_worktree(&repo) && has_stack_lease_or_legacy_ref(&repo);
    let stacked_open = if prefer_stacked {
        open_verified_stacked_store(&repo)?
    } else {
        None
    };
    let local_store = if stacked_open.is_none() {
        super::open_local_indexed_store(&repo).ok()
    } else {
        None
    };
    let stacked_open = match stacked_open {
        Some(st) => Some(st),
        None if local_store.is_none() => open_verified_stacked_store(&repo)?,
        None => None,
    };
    let fallback_store = if local_store.is_none() && stacked_open.is_none() {
        fallback_anchor.and_then(|a| super::open_local_indexed_store(a).ok())
    } else {
        None
    };

    let active_store: Option<&Store> = stacked_open
        .as_ref()
        .map(|(s, _, _)| s)
        .or(local_store.as_ref())
        .or(fallback_store.as_ref());

    let Some(store) = active_store else {
        return Ok(DirtyNodeScan::default());
    };
    let Some(csr_reader) = store.open_latest_csr_sidecar().ok().flatten() else {
        return Ok(DirtyNodeScan::default());
    };

    let mut dirty = devmap_query::RoaringBitmap::new();
    if let Some((ref base_store, _, ref stacked)) = stacked_open {
        if stacked.delta_present() {
            let mut ov = stacked.overlay().clone();
            enrich_stacked_overlay_with_csr(&mut ov, base_store, Some(&csr_reader));
            if ov.generation == csr_reader.generation() {
                for nid in ov.dirty_nodes.to_vec() {
                    dirty.insert(nid);
                }
            }
        }
    }

    let csr_lookup = CsrDigestLookup::from_reader(&csr_reader);
    let base_head_sha = store
        .latest_generation_head_sha()
        .ok()
        .flatten()
        .or_else(|| {
            stacked_open
                .as_ref()
                .and_then(|(_, m, _)| m.base_head_sha.clone())
        });

    let (deltas, truncated) = collect_worktree_deltas(&repo, base_head_sha.as_deref())?;
    for (rel_path, _kind) in deltas {
        let indexed_symbols: Vec<String> = store
            .latest_symbols_for_file(&rel_path)
            .ok()
            .flatten()
            .map(|page| {
                let mut names = Vec::with_capacity(page.rows.len() * 2);
                for r in page.rows {
                    if r.qualified_name != r.name {
                        names.push(r.qualified_name);
                    }
                    names.push(r.name);
                }
                names
            })
            .unwrap_or_default();
        for nid in csr_lookup.matching_nodes(&rel_path, &indexed_symbols) {
            dirty.insert(nid);
        }
    }
    Ok(DirtyNodeScan {
        dirty_nodes: dirty,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procguard::LockedSpawn;
    use rusqlite::Connection;
    use std::process::Command;

    fn git(repo: &Path, args: &[&str]) {
        let mut out_cmd = Command::new("git");
        crate::test_support::pin_git(&mut out_cmd);
        let out = out_cmd
            .current_dir(repo)
            .args(args)
            .output_locked()
            .expect("git spawn");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn head_sha_of_attributes_its_rev_parse_to_its_own_source() {
        let repo = crate::test_support::git_repo();
        crate::test_support::git_in(repo.path(), &["commit", "-q", "--allow-empty", "-m", "one"]);
        let before = crate::engine::git_cli::rev_parse_launches_from(HEAD_SHA_SOURCE);
        assert!(head_sha_of(repo.path()).is_some(), "the head must resolve");
        assert!(
            crate::engine::git_cli::rev_parse_launches_from(HEAD_SHA_SOURCE) > before,
            "head_sha_of's rev-parse was not attributed to {HEAD_SHA_SOURCE}"
        );
    }

    fn bind_real_devmap() -> crate::devmap::cli::TestBinaryBinding {
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            for rel in [".local/bin/devmap", ".cargo/bin/devmap"] {
                let candidate = home.join(rel);
                if candidate.is_file() {
                    return crate::devmap::cli::bind_test_binary(
                        candidate.to_string_lossy().into_owned(),
                    );
                }
            }
        }
        let resolved = crate::devmap::cli::resolve_binary().expect("real devmap binary installed");
        crate::devmap::cli::bind_test_binary(resolved.path)
    }

    fn seed_fresh_anchor_for_zero_copy() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let anchor = dir.path();
        git(anchor, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(anchor.join("src")).unwrap();
        fs::create_dir_all(anchor.join("tests")).unwrap();
        let caller_src = "pub fn probe_caller() { probe_callee(); }\n";
        let callee_src = "pub fn probe_callee() {}\n";
        let test_src = "#[test]\nfn calls_probe_caller() { probe_caller(); }\n";
        fs::write(anchor.join("src/caller.rs"), caller_src).unwrap();
        fs::write(anchor.join("src/callee.rs"), callee_src).unwrap();
        fs::write(anchor.join("tests/caller_test.rs"), test_src).unwrap();
        git(anchor, &["add", "."]);
        git(
            anchor,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "seed fresh anchor",
            ],
        );
        let canon_anchor = fs::canonicalize(anchor).unwrap();
        let mut head_out_cmd = Command::new("git");
        crate::test_support::pin_git(&mut head_out_cmd);
        let head_out = head_out_cmd
            .current_dir(&canon_anchor)
            .args(["rev-parse", "HEAD"])
            .output_locked()
            .unwrap();
        let head_sha = String::from_utf8_lossy(&head_out.stdout).trim().to_string();
        crate::test_support::trust_repo(&canon_anchor);
        crate::devmap::ensure_state_dir_excluded(&canon_anchor);

        let scanned = devmap_extract::scan_tree(&canon_anchor).unwrap();
        let hashes: std::collections::BTreeMap<String, u64> = scanned
            .file_hashes()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let (grammar_ver, analyzer_ver) = devmap_extract::cache::current_payload_identity("rust");

        let db = devmap_query::paths::store_path(&canon_anchor);
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = Store::open(&db).unwrap();
        store.bind_repo_root(&canon_anchor).unwrap();
        drop(store);

        let root_str = canon_anchor.to_string_lossy().into_owned();
        let conn = rusqlite::Connection::open(&db).unwrap();
        let files = [
            ("src/caller.rs", "probe_caller", caller_src.len() as i64),
            ("src/callee.rs", "probe_callee", callee_src.len() as i64),
            (
                "tests/caller_test.rs",
                "calls_probe_caller",
                test_src.len() as i64,
            ),
        ];
        conn.execute(
            "INSERT INTO generations (id, created_at, head_sha, analysis_json, repo_root)
             VALUES (1, 0.0, ?1, ?2, ?3)",
            (
                &head_sha,
                r#"{"total_files":3,"total_symbols":3,"total_edges":2,"dead_symbols":[],"communities":[],"status":"Ok","unresolved_calls":0}"#,
                &root_str,
            ),
        )
        .unwrap();
        for (idx, (path, sym, span_end)) in files.iter().enumerate() {
            let id = (idx + 1) as i64;
            let raw_hash = *hashes.get(*path).expect("scanned hash");
            let content_hash = raw_hash as i64;
            conn.execute("INSERT INTO paths (id, path) VALUES (?1, ?2)", (id, *path))
                .unwrap();
            let engine_json =
                serde_json::to_string(&devmap_extract::ExtractionEngine::TreeSitter {
                    grammar: "rust".to_string(),
                    grammar_version: grammar_ver.parse::<u32>().unwrap_or(1),
                })
                .unwrap();
            let extraction_json = serde_json::json!({
                "file_path": *path,
                "language": "rust",
                "content_hash": raw_hash,
                "parse_outcome": "Clean",
                "symbols": [{
                    "name": *sym,
                    "qualified_name": format!("{path}::{sym}"),
                    "kind": "Function",
                    "span": { "start_byte": 0, "end_byte": *span_end as usize },
                    "is_exported": true
                }],
                "imports": [],
                "calls": []
            })
            .to_string();
            conn.execute(
                "INSERT INTO file_payloads
                   (payload_id, file_id, content_hash, language,
                    parse_outcome_json, engine_json, extraction_json,
                    entry_wiring_json, grammar_version, analyzer_version)
                 VALUES (?1, ?1, ?2, 'rust', '\"Clean\"', ?3, ?4, '[]', ?5, ?6)",
                (
                    id,
                    content_hash,
                    &engine_json,
                    &extraction_json,
                    &grammar_ver,
                    &analyzer_ver,
                ),
            )
            .unwrap();
            conn.execute(
                "INSERT INTO file_rows (file_id, payload_id, valid_from, valid_to)
                 VALUES (?1, ?1, 1, NULL)",
                [id],
            )
            .unwrap();
            let qname = format!("{path}::{sym}");
            conn.execute(
                "INSERT INTO payload_nodes
                   (payload_id, ord_in_file, name, qualified_name, kind,
                    span_start, span_end, is_exported)
                 VALUES (?1, 0, ?2, ?3, 'Function', 0, ?4, 1)",
                (id, *sym, &qname, *span_end),
            )
            .unwrap();
            conn.execute(
                "INSERT INTO nodes_fts (rowid, name, qualified_name, path)
                 VALUES (?1, ?2, ?3, ?4)",
                (id << 24, *sym, &qname, *path),
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO edge_rows
               (edge_id, source_file_id, target_file_id,
                source_symbol, target_symbol, edge_kind, confidence,
                valid_from, valid_to)
             VALUES (0, 1, 2, 'probe_caller', 'probe_callee', 'Calls', 0.95, 1, NULL),
                    (1, 3, 1, 'calls_probe_caller', 'probe_caller', 'Calls', 0.95, 1, NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO build_history
               (generation_id, built_at, head_sha, files, symbols, edges,
                dead_confident, dead_ambiguous, parse_failed, languages_covered,
                build_ms, db_bytes)
             VALUES (1, 0.0, ?1, 3, 3, 2, 0, 0, 0, 1, NULL, 0)",
            [&head_sha],
        )
        .unwrap();
        drop(conn);

        // Materialize the anchor CSR sidecar once so sealed base snapshots publish `.csr`.
        let _csr = crate::codeintel::open_repo_csr_graph(canon_anchor.to_str().unwrap()).unwrap();

        (dir, head_sha)
    }

    #[test]
    fn provision_worktree_devmap_clones_and_rebinds_store_keeping_git_status_clean() {
        let anchor_dir = crate::codeintel::tests::repo_with_one_generation();
        let anchor = anchor_dir.path();
        crate::test_support::trust_repo(anchor);

        let anchor_map = devmap_query::paths::repo_map_path(anchor);
        fs::write(&anchor_map, r#"{"schema_version":1,"files":[]}"#).unwrap();

        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("lane-alpha");
        git(
            anchor,
            &["worktree", "add", "-b", "lane/alpha", wt.to_str().unwrap()],
        );
        crate::test_support::trust_repo(&wt);

        let report = provision_worktree_devmap(anchor, &wt).expect("provision");
        assert!(report.seeded, "{report:?}");
        assert!(report.csr_seeded, "{report:?}");
        assert!(report.repo_map_seeded, "{report:?}");
        assert!(!report.cas_seeded, "{report:?}");
        assert!(report.overlay_path.is_none(), "{report:?}");
        assert!(report.generation_id.is_some());

        // Legacy sidecars must never be written (`P2.3`).
        let wt_ci = devmap_query::paths::state_dir(&wt).join("codeintel");
        assert!(!wt_ci.join("overlay.json").exists());
        assert!(!wt_ci.join("base_store_ref.json").exists());
        assert!(!wt_ci.join("cas").exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let db_meta = fs::metadata(devmap_query::paths::store_path(&wt)).unwrap();
            assert_eq!(
                db_meta.nlink(),
                1,
                "seeded SQLite DB must not be hardlinked"
            );
        }

        let wt_str = wt.to_str().unwrap();
        let status = crate::codeintel::status(wt_str);
        assert!(status.available, "worktree store should open: {status:?}");
        assert_eq!(status.generation_id, report.generation_id);
        let csr = crate::codeintel::open_repo_csr_graph(wt_str).expect("CSR graph");
        assert_eq!(csr.generation(), u64::from(report.generation_id.unwrap()));

        let mut git_status_cmd = Command::new("git");
        crate::test_support::pin_git(&mut git_status_cmd);
        let git_status = git_status_cmd
            .current_dir(&wt)
            .args(["status", "--porcelain"])
            .output_locked()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&git_status.stdout).trim(),
            "",
            "DevMap seeding must never dirty git status"
        );
    }

    #[test]
    fn provision_worktree_devmap_uses_online_backup_when_wal_is_non_empty() {
        let anchor_dir = crate::codeintel::tests::repo_with_one_generation();
        let anchor = anchor_dir.path();
        crate::test_support::trust_repo(anchor);

        let anchor_db = devmap_query::paths::store_path(anchor);
        let writer = Connection::open(&anchor_db).unwrap();
        writer.pragma_update(None, "journal_mode", "WAL").unwrap();
        writer
            .pragma_update(None, "wal_autocheckpoint", 0i64)
            .unwrap();
        let reader = Connection::open(&anchor_db).unwrap();
        reader
            .execute_batch("BEGIN DEFERRED; SELECT 1 FROM sqlite_master LIMIT 1;")
            .unwrap();
        writer
            .execute(
                "INSERT OR REPLACE INTO pending_paths (path, queued_at, attempts) VALUES ('wal_probe.rs', 1.0, 0)",
                [],
            )
            .unwrap();
        let wal_path = PathBuf::from(format!("{}-wal", anchor_db.display()));
        assert!(
            fs::metadata(&wal_path)
                .map(|m| m.len() > 0)
                .unwrap_or(false),
            "expected non-empty WAL sidecar"
        );

        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("lane-wal");
        git(
            anchor,
            &["worktree", "add", "-b", "lane/wal", wt.to_str().unwrap()],
        );
        crate::test_support::trust_repo(&wt);

        let report = provision_worktree_devmap(anchor, &wt).expect("provision with pinned WAL");
        assert!(report.seeded, "{report:?}");
        assert!(report.used_online_backup, "{report:?}");

        let status = crate::codeintel::status(wt.to_str().unwrap());
        assert!(status.available, "{status:?}");
        drop(reader);
        drop(writer);
    }

    #[test]
    fn provision_worktree_devmap_heals_unpinned_wal_without_online_backup() {
        let anchor_dir = crate::codeintel::tests::repo_with_one_generation();
        let anchor = anchor_dir.path();
        crate::test_support::trust_repo(anchor);

        let anchor_db = devmap_query::paths::store_path(anchor);
        let writer = Connection::open(&anchor_db).unwrap();
        writer.pragma_update(None, "journal_mode", "WAL").unwrap();
        writer
            .pragma_update(None, "wal_autocheckpoint", 0i64)
            .unwrap();
        writer
            .execute(
                "INSERT OR REPLACE INTO pending_paths (path, queued_at, attempts) VALUES ('wal_heal_probe.rs', 1.0, 0)",
                [],
            )
            .unwrap();
        let wal_path = PathBuf::from(format!("{}-wal", anchor_db.display()));
        assert!(
            fs::metadata(&wal_path)
                .map(|m| m.len() > 0)
                .unwrap_or(false),
            "expected non-empty unpinned WAL before heal"
        );

        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("lane-wal-healed");
        git(
            anchor,
            &[
                "worktree",
                "add",
                "-b",
                "lane/wal-healed",
                wt.to_str().unwrap(),
            ],
        );
        crate::test_support::trust_repo(&wt);

        let report = provision_worktree_devmap(anchor, &wt).expect("provision healing WAL");
        assert!(report.seeded, "{report:?}");
        assert!(
            !report.used_online_backup,
            "unpinned WAL must be healed via TRUNCATE + clonefile instead of VACUUM INTO: {report:?}"
        );
        drop(writer);
    }

    #[test]
    fn provision_worktree_devmap_skips_cleanly_when_anchor_has_no_store() {
        let anchor_dir = tempfile::tempdir().unwrap();
        let anchor = anchor_dir.path();
        git(anchor, &["init", "-q"]);
        git(
            anchor,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
        );
        crate::test_support::trust_repo(anchor);

        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("lane-plain");
        git(
            anchor,
            &["worktree", "add", "-b", "lane/plain", wt.to_str().unwrap()],
        );
        crate::test_support::trust_repo(&wt);

        let report = provision_worktree_devmap(anchor, &wt).expect("skip cleanly");
        assert!(!report.seeded);
        assert!(report
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("no DevMap store"));
        assert!(!wt.join(".devmap").exists());
        assert!(!wt.join(".devcouncil").exists());
    }

    #[test]
    fn sync_worktree_overlay_marks_deleted_files_and_symbols_with_tombstone() {
        let _devmap = bind_real_devmap();
        let (anchor_dir, _head_sha) = seed_fresh_anchor_for_zero_copy();
        let anchor = anchor_dir.path();

        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("lane-tombstone");
        git(
            anchor,
            &[
                "worktree",
                "add",
                "-b",
                "lane/tombstone",
                wt.to_str().unwrap(),
            ],
        );
        crate::test_support::trust_repo(&wt);
        provision_worktree_devmap_zero_copy(anchor, &wt).expect("provision stacked");

        fs::remove_file(wt.join("src/callee.rs")).unwrap();

        let wt_str = wt.to_str().unwrap();
        let report = sync_worktree_overlay(wt_str).expect("sync");
        assert_eq!(report.tombstoned_files, 1, "{report:?}");
        assert!(report.tombstoned_symbols >= 1, "{report:?}");
        assert!(report.dirty_nodes >= 1, "{report:?}");

        let overlay = load_worktree_overlay(wt_str).unwrap().unwrap();
        assert!(overlay.is_file_tombstoned(Path::new("src/callee.rs")));
        assert!(overlay
            .tombstoned_symbols
            .contains("src/callee.rs::probe_callee"));
        assert_eq!(
            overlay.resolve_file(Path::new("src/callee.rs"), Some("b3_old")),
            None
        );
    }

    #[test]
    fn provision_worktree_devmap_zero_copy_shares_anchor_graph_without_copying_sqlite() {
        let _devmap = bind_real_devmap();
        let (anchor_dir, _head_sha) = seed_fresh_anchor_for_zero_copy();
        let anchor = anchor_dir.path();

        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("lane-zero-copy");
        git(
            anchor,
            &[
                "worktree",
                "add",
                "-b",
                "lane/zero-copy",
                wt.to_str().unwrap(),
            ],
        );
        crate::test_support::trust_repo(&wt);

        let report = provision_worktree_devmap_zero_copy(anchor, &wt).expect("zero-copy provision");
        assert!(report.seeded, "{report:?}");
        assert!(report.zero_copy_base_ref, "{report:?}");
        assert!(!report.is_cow);
        assert!(report.csr_seeded);
        assert!(!report.cas_seeded);
        assert!(report.overlay_path.is_none());

        // Zero SQLite bytes, zero legacy `base_store_ref.json`/`overlay.json`/`cas`,
        // and zero `delta.sqlite` when clean!
        let wt_ci = devmap_query::paths::state_dir(&wt).join("codeintel");
        assert!(!devmap_query::paths::store_path(&wt).exists());
        assert!(!wt_ci.join("base_store_ref.json").exists());
        assert!(!wt_ci.join("overlay.json").exists());
        assert!(!wt_ci.join("cas").exists());
        assert!(!devmap_extract::paths::stack_delta_path(&wt).exists());

        let lease_path = devmap_extract::paths::stack_lease_path(&wt);
        assert!(lease_path.is_file());
        let lease = devmap_query::stacked::StackLease::read_from(&lease_path).unwrap();
        assert_eq!(Some(lease.generation_id), report.generation_id);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(lease.base_sqlite_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o444, "sealed base snapshot must be read-only 0444");
        }

        let (csr, gen) = open_worktree_or_anchor_csr(&wt).expect("base CSR mmap");
        assert_eq!(gen, u64::from(lease.generation_id));
        assert!(csr.node_count() > 0);

        let wt_str = wt.to_str().unwrap();
        assert!(
            load_worktree_overlay(wt_str).unwrap().is_none(),
            "clean stacked worktree has no delta.sqlite yet"
        );

        let sample_bytes = b"pub fn zero_copy_treesitter_symbol() -> usize { 99 }\n";
        fs::write(wt.join("src/zc.rs"), sample_bytes).unwrap();
        let sync = sync_worktree_overlay(wt_str).expect("sync stacked delta");
        assert_eq!(sync.modified_files, 1);

        let overlay = load_worktree_overlay(wt_str).unwrap().unwrap();
        let expected_b3 = devmap_query::blake3_hash(sample_bytes);
        assert_eq!(
            overlay.resolve_file(Path::new("src/zc.rs"), None),
            Some(expected_b3.as_str())
        );
        let ext = overlay
            .added_extractions
            .get("src/zc.rs")
            .expect("Tree-sitter extraction stored in delta.sqlite");
        assert!(matches!(
            ext.engine,
            devmap_extract::ExtractionEngine::TreeSitter { .. }
        ));
        assert!(ext
            .symbols
            .iter()
            .any(|s| s.name == "zero_copy_treesitter_symbol"));
    }

    #[test]
    fn legacy_base_store_ref_and_cross_repository_stack_lease_are_refused_cleanly() {
        let (anchor_dir, _head_sha) = seed_fresh_anchor_for_zero_copy();
        let anchor = anchor_dir.path();

        let stranger_dir = tempfile::tempdir().unwrap();
        let stranger = stranger_dir.path();
        git(stranger, &["init", "-q"]);
        git(
            stranger,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
        );
        crate::test_support::trust_repo(stranger);
        crate::devmap::ensure_state_dir_excluded(stranger);

        // 1. Owner Stop #2: retired `base_store_ref.json` is refused naming `devmap seed --stacked`
        //    and NEVER auto-provisioned or upgraded on read!
        let legacy_path = legacy_base_store_ref_path(stranger);
        fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        fs::write(&legacy_path, br#"{"generation_id":1}"#).unwrap();

        let stranger_str = stranger.to_str().unwrap();
        let err = load_worktree_overlay(stranger_str)
            .expect_err("retired base_store_ref.json must be refused");
        assert!(
            err.message().contains("devmap seed --stacked"),
            "unexpected error: {err:?}"
        );
        let st = crate::codeintel::status(stranger_str);
        assert!(!st.available, "{st:?}");
        assert!(
            st.reason
                .as_deref()
                .is_some_and(|r| r.contains("devmap seed --stacked")),
            "{st:?}"
        );
        assert!(
            !devmap_extract::paths::stack_lease_path(stranger).exists(),
            "refusing legacy base_store_ref.json must never write stack_lease.json"
        );
        fs::remove_file(&legacy_path).unwrap();

        // 2. Cross-repository `stack_lease.json` spoofing is refused cleanly!
        let legit_parent = tempfile::tempdir().unwrap();
        let legit_wt = legit_parent.path().join("legit-wt");
        git(
            anchor,
            &[
                "worktree",
                "add",
                "-b",
                "lane/legit-lease",
                legit_wt.to_str().unwrap(),
            ],
        );
        crate::test_support::trust_repo(&legit_wt);
        provision_worktree_devmap_zero_copy(anchor, &legit_wt).unwrap();

        let mut spoofed_lease = devmap_query::stacked::StackLease::read_from(
            &devmap_extract::paths::stack_lease_path(&legit_wt),
        )
        .unwrap();
        spoofed_lease.worktree_root = fs::canonicalize(stranger)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let stranger_lease_path = devmap_extract::paths::stack_lease_path(stranger);
        fs::create_dir_all(stranger_lease_path.parent().unwrap()).unwrap();
        fs::write(
            &stranger_lease_path,
            serde_json::to_vec(&spoofed_lease).unwrap(),
        )
        .unwrap();

        let spoof_err = load_worktree_overlay(stranger_str)
            .expect_err("cross-repository stack_lease.json must be refused");
        assert!(
            spoof_err.message().contains("belongs to repository")
                || spoof_err.message().contains("outside worktree family"),
            "unexpected error: {spoof_err:?}"
        );
    }

    #[test]
    fn dirty_or_ahead_donor_is_refused_by_provision_worktree_devmap_zero_copy() {
        let (anchor_dir, _head_sha) = seed_fresh_anchor_for_zero_copy();
        let anchor = anchor_dir.path();

        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("wt-dirty-donor");
        git(
            anchor,
            &[
                "worktree",
                "add",
                "-b",
                "lane/dirty-donor",
                wt.to_str().unwrap(),
            ],
        );
        crate::test_support::trust_repo(&wt);

        // 1. Uncommitted edit in donor checkout -> refused!
        let orig_callee = fs::read_to_string(anchor.join("src/callee.rs")).unwrap();
        fs::write(
            anchor.join("src/callee.rs"),
            "pub fn probe_callee() { /* uncommitted donor edit */ }\n",
        )
        .unwrap();
        let dirty_err = provision_worktree_devmap_zero_copy(anchor, &wt)
            .expect_err("dirty donor must be refused");
        assert!(
            dirty_err.message().contains("uncommitted")
                || dirty_err.message().contains("changed since its index"),
            "unexpected dirty donor error: {dirty_err:?}"
        );
        assert!(
            !devmap_extract::paths::stack_lease_path(&wt).exists(),
            "failed provisioning must not leave stack_lease.json"
        );
        fs::write(anchor.join("src/callee.rs"), &orig_callee).unwrap();

        // 2.Untracked non-tool file in donor checkout -> refused!
        fs::write(anchor.join("src/untracked_donor.rs"), "pub fn stray() {}\n").unwrap();
        let untracked_err = provision_worktree_devmap_zero_copy(anchor, &wt)
            .expect_err("untracked file in donor must be refused");
        assert!(
            untracked_err.message().contains("uncommitted")
                || untracked_err.message().contains("changed since its index"),
            "unexpected untracked donor error: {untracked_err:?}"
        );
        fs::remove_file(anchor.join("src/untracked_donor.rs")).unwrap();

        // 3. Donor HEAD ahead of indexed generation `head_sha` -> refused!
        git(
            anchor,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "advance HEAD past index",
            ],
        );
        let ahead_err = provision_worktree_devmap_zero_copy(anchor, &wt)
            .expect_err("ahead donor HEAD must be refused");
        assert!(
            ahead_err.message().contains("HEAD") || ahead_err.message().contains("indexed at"),
            "unexpected ahead donor error: {ahead_err:?}"
        );
    }

    #[test]
    fn zero_write_on_read_across_all_codeintel_and_radar_surfaces() {
        let _devmap = bind_real_devmap();
        let (anchor_dir, head_sha) = seed_fresh_anchor_for_zero_copy();
        let anchor = anchor_dir.path();

        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("wt-zero-write-on-read");
        git(
            anchor,
            &[
                "worktree",
                "add",
                "-b",
                "lane/zero-write-on-read",
                wt.to_str().unwrap(),
            ],
        );
        crate::test_support::trust_repo(&wt);
        provision_worktree_devmap_zero_copy(anchor, &wt).expect("provision stacked");

        // Dirty tracked file + add untracked file WITHOUT calling `sync_worktree_overlay`!
        fs::write(
            wt.join("src/caller.rs"),
            "pub fn probe_caller() { probe_callee(); /* dirty */ }\n",
        )
        .unwrap();
        fs::write(wt.join("src/unsynced.rs"), "pub fn unsynced_fn() {}\n").unwrap();

        let lease_path = devmap_extract::paths::stack_lease_path(&wt);
        let delta_path = devmap_extract::paths::stack_delta_path(&wt);
        let lease = devmap_query::stacked::StackLease::read_from(&lease_path).unwrap();
        let base_sqlite = lease.base_sqlite_path();
        assert!(!delta_path.exists());

        let snap = |p: &Path| -> (Vec<u8>, std::time::SystemTime) {
            let bytes = fs::read(p).unwrap();
            let mtime = fs::metadata(p).unwrap().modified().unwrap();
            (bytes, mtime)
        };
        let lease_before = snap(&lease_path);
        let base_before = snap(&base_sqlite);

        let wt_str = wt.to_str().unwrap();
        let _ = crate::codeintel::status(wt_str);
        let _ = crate::codeintel::search(wt_str, "probe_caller", None);
        let _ = crate::codeintel::explore(
            wt_str,
            "probe_caller",
            None,
            None,
            crate::codeintel::WalkBounds::default(),
        );
        let _ = crate::codeintel::impact(wt_str, "probe_callee", None);
        let _ = crate::codeintel::dependencies(wt_str, "src/caller.rs", None);
        let _ = crate::codeintel::neighbors(
            wt_str,
            &["probe_caller".to_string()],
            None,
            None,
            crate::codeintel::WalkBounds::default(),
        );
        let _ = crate::codeintel::dead_symbols(wt_str, None);
        let _ = crate::codeintel::affected_tests(
            wt_str,
            &["probe_callee".to_string()],
            None,
            crate::codeintel::WalkBounds::default(),
        );
        let _ = crate::codeintel::symbols_for_file(wt_str, "src/caller.rs", &head_sha);
        let _ = load_worktree_overlay(wt_str).unwrap();
        let _ = compute_worktree_dirty_nodes_readonly(wt_str, Some(anchor)).unwrap();
        let _ = crate::engine::agent_bus::scan_parallel_agent_radar(anchor.to_str().unwrap())
            .expect("radar");

        assert!(
            !delta_path.exists(),
            "Owner Stop #1: read surfaces must NEVER create delta.sqlite"
        );
        assert_eq!(lease_before, snap(&lease_path), "stack_lease.json mutated");
        assert_eq!(base_before, snap(&base_sqlite), "base.sqlite mutated");

        // Now explicitly sync `delta.sqlite`, make a SECOND unsynced edit, and prove
        // read surfaces leave the existing `delta.sqlite` bytes & mtime untouched!
        sync_worktree_overlay(wt_str).expect("explicit sync");
        assert!(delta_path.is_file());
        fs::write(
            wt.join("src/second_dirty.rs"),
            "pub fn second_unsynced_fn() {}\n",
        )
        .unwrap();
        let delta_before = snap(&delta_path);

        let _ = crate::codeintel::status(wt_str);
        let _ = crate::codeintel::search(wt_str, "unsynced_fn", None);
        let _ = crate::codeintel::impact(wt_str, "probe_callee", None);
        let _ = load_worktree_overlay(wt_str).unwrap();
        let _ = compute_worktree_dirty_nodes_readonly(wt_str, Some(anchor)).unwrap();

        assert_eq!(
            delta_before,
            snap(&delta_path),
            "Owner Stop #1: read surfaces must NEVER mutate existing delta.sqlite"
        );
    }

    #[test]
    fn zero_copy_worktree_supports_all_codeintel_queries_and_tracks_worktree_freshness() {
        let (anchor_dir, head_sha) = seed_fresh_anchor_for_zero_copy();
        let anchor = anchor_dir.path();
        let anchor_status = crate::codeintel::status(anchor.to_str().unwrap());
        assert_eq!(anchor_status.is_fresh, Some(true), "{anchor_status:?}");

        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("wt-zc-queries");
        git(
            anchor,
            &[
                "worktree",
                "add",
                "-b",
                "lane/zc-queries",
                wt.to_str().unwrap(),
            ],
        );
        crate::test_support::trust_repo(&wt);
        provision_worktree_devmap_zero_copy(anchor, &wt).expect("zero-copy provision");
        assert!(!devmap_query::paths::store_path(&wt).exists());

        let wt_str = wt.to_str().unwrap();

        let st = crate::codeintel::status(wt_str);
        assert!(st.available, "zero-copy status failed: {st:?}");
        assert_eq!(st.is_fresh, Some(true), "{st:?}");
        assert_eq!(st.source_freshness, Some(true), "{st:?}");

        let hits = crate::codeintel::search(wt_str, "probe_caller", None);
        assert!(hits.available, "zero-copy search failed: {hits:?}");
        let caller_hit = hits
            .items
            .iter()
            .find(|h| h.symbol_name == "probe_caller")
            .expect("expected probe_caller hit");
        assert!(
            caller_hit.source_span.contains("probe_caller"),
            "clean zero-copy worktree should read source span: {caller_hit:?}"
        );

        let exp = crate::codeintel::explore(
            wt_str,
            "probe_caller",
            None,
            None,
            crate::codeintel::WalkBounds::default(),
        );
        assert!(exp.available, "zero-copy explore failed: {exp:?}");
        assert!(exp
            .definitions
            .items
            .iter()
            .any(|d| d.symbol_name == "probe_caller"));

        let imp = crate::codeintel::impact(wt_str, "probe_callee", None);
        assert!(imp.available, "zero-copy impact failed: {imp:?}");
        assert!(!imp.items.is_empty());

        let deps = crate::codeintel::dependencies(wt_str, "src/caller.rs", None);
        assert!(deps.available, "zero-copy dependencies failed: {deps:?}");
        assert_eq!(deps.items.len(), 1);

        let neigh = crate::codeintel::neighbors(
            wt_str,
            &["probe_caller".to_string()],
            None,
            None,
            crate::codeintel::WalkBounds::default(),
        )
        .expect("zero-copy neighbors");
        assert_eq!(neigh.len(), 1);

        let file_syms = crate::codeintel::symbols_for_file(wt_str, "src/caller.rs", &head_sha);
        assert!(
            file_syms.available,
            "zero-copy symbols_for_file failed: {file_syms:?}"
        );
        assert!(file_syms.head_matches);
        assert_eq!(file_syms.items.len(), 1);

        let csr =
            crate::codeintel::open_repo_csr_graph(wt_str).expect("zero-copy open_repo_csr_graph");
        assert!(csr.node_count() >= 3);

        let selected = crate::codeintel::affected_tests(
            wt_str,
            &["probe_callee".to_string()],
            None,
            crate::codeintel::WalkBounds::default(),
        );
        assert!(
            !selected.fail_closed,
            "clean zero-copy worktree should authorize affected_tests: {selected:?}"
        );
        assert!(
            selected
                .tests
                .items
                .iter()
                .any(|t| t.path == "tests/caller_test.rs"),
            "expected tests/caller_test.rs in {selected:?}"
        );

        // Mutating ONLY the zero-copy worktree MUST mark it stale, fail-close
        // `affected_tests`, and withhold stale anchor `source_span`!
        fs::write(
            wt.join("src/caller.rs"),
            "// shifted line 1\n// shifted line 2\npub fn probe_caller() { probe_callee(); }\n",
        )
        .unwrap();

        let dirty_st = crate::codeintel::status(wt_str);
        assert_eq!(dirty_st.source_freshness, Some(false), "{dirty_st:?}");
        assert_eq!(dirty_st.is_fresh, Some(false));

        let dirty_selected = crate::codeintel::affected_tests(
            wt_str,
            &["probe_callee".to_string()],
            None,
            crate::codeintel::WalkBounds::default(),
        );
        assert!(dirty_selected.fail_closed, "{dirty_selected:?}");

        let dirty_hits = crate::codeintel::search(wt_str, "probe_caller", None);
        let dirty_caller = dirty_hits
            .items
            .iter()
            .find(|h| h.symbol_name == "probe_caller")
            .expect("expected probe_caller hit");
        assert!(
            dirty_caller.source_span.is_empty() && dirty_caller.source_unavailable_reason.is_some(),
            "dirty zero-copy worktree must NOT return anchor's stale source_span: {dirty_caller:?}"
        );
    }

    #[test]
    fn sync_worktree_overlay_extracts_treesitter_declarations_and_tombstones_removed_symbols() {
        let _devmap = bind_real_devmap();
        let (anchor_dir, head_sha) = seed_fresh_anchor_for_zero_copy();
        let anchor = anchor_dir.path();

        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("wt-lane-ts");
        git(
            anchor,
            &[
                "worktree",
                "add",
                "-b",
                "lane/extract-ts",
                wt.to_str().unwrap(),
            ],
        );
        crate::test_support::trust_repo(&wt);
        provision_worktree_devmap_zero_copy(anchor, &wt).unwrap();

        let wt_str = wt.to_str().unwrap();
        // Clean fast-path creates zero `delta.sqlite`.
        let clean_sync = sync_worktree_overlay(wt_str).expect("clean sync");
        assert_eq!(clean_sync.modified_files, 0);
        assert!(!devmap_extract::paths::stack_delta_path(&wt).exists());

        let new_file_src = "pub fn brand_new_lane_helper() -> usize { 42 }\n";
        fs::write(wt.join("src/new_feature.rs"), new_file_src).unwrap();
        fs::write(
            wt.join("src/caller.rs"),
            "pub fn replacement_caller() { probe_callee(); }\n",
        )
        .unwrap();

        let sync = sync_worktree_overlay(wt_str).expect("sync");
        assert_eq!(sync.modified_files, 2);
        assert!(sync.dirty_nodes >= 1);

        let overlay = load_worktree_overlay(wt_str).unwrap().unwrap();
        let new_ext = overlay
            .added_extractions
            .get("src/new_feature.rs")
            .expect("Tree-sitter extraction must populate added_extractions");
        assert!(matches!(
            new_ext.engine,
            devmap_extract::ExtractionEngine::TreeSitter { .. }
        ));
        assert!(new_ext
            .symbols
            .iter()
            .any(|s| s.name == "brand_new_lane_helper"));
        assert!(overlay
            .tombstoned_symbols
            .contains("src/caller.rs::probe_caller"));

        let new_hits = crate::codeintel::search(wt_str, "brand_new_lane_helper", None);
        assert!(new_hits.available, "{new_hits:?}");
        assert!(new_hits.items.iter().any(|h| {
            h.symbol_name == "brand_new_lane_helper"
                && h.file_path == "src/new_feature.rs"
                && h.source_span.contains("42")
        }));

        let old_hits = crate::codeintel::search(wt_str, "probe_caller", None);
        assert!(
            !old_hits
                .items
                .iter()
                .any(|h| h.symbol_name == "probe_caller"),
            "replaced symbol probe_caller must be masked from search: {old_hits:?}"
        );

        let file_syms = crate::codeintel::symbols_for_file(wt_str, "src/new_feature.rs", &head_sha);
        assert!(file_syms.available, "{file_syms:?}");
        assert!(!file_syms.head_matches);
        assert!(file_syms
            .items
            .iter()
            .any(|s| s.symbol_name == "brand_new_lane_helper"));
    }

    #[test]
    fn polyglot_treesitter_sync_preserves_live_methods_and_consts() {
        let _devmap = bind_real_devmap();
        let dir = tempfile::tempdir().unwrap();
        let anchor = dir.path();
        git(anchor, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(anchor.join("src")).unwrap();
        let rs_src =
            "pub const MAX_RETRIES: usize = 3;\npub fn keep_me() -> usize { MAX_RETRIES }\n";
        let go_src = "package srv\ntype Srv struct{}\nfunc (s *Srv) HandleReq() {}\n";
        let ts_src = "export const API_URL = 'https://example.test';\nexport class Client { fetchAll() { return API_URL; } }\n";
        fs::write(anchor.join("src/lib.rs"), rs_src).unwrap();
        fs::write(anchor.join("src/srv.go"), go_src).unwrap();
        fs::write(anchor.join("src/client.ts"), ts_src).unwrap();
        git(anchor, &["add", "."]);
        git(
            anchor,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "polyglot init",
            ],
        );
        let canon_anchor = fs::canonicalize(anchor).unwrap();
        crate::test_support::trust_repo(&canon_anchor);
        crate::devmap::ensure_state_dir_excluded(&canon_anchor);
        let built = crate::devmap::cli::build(&canon_anchor.to_string_lossy())
            .expect("build polyglot anchor");
        assert!(built.ok, "{built:?}");

        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("wt-polyglot");
        git(
            &canon_anchor,
            &[
                "worktree",
                "add",
                "-b",
                "lane/polyglot",
                wt.to_str().unwrap(),
            ],
        );
        crate::test_support::trust_repo(&wt);
        provision_worktree_devmap_zero_copy(&canon_anchor, &wt).expect("zero-copy");

        fs::write(wt.join("src/lib.rs"), format!("// edit\n{rs_src}")).unwrap();
        fs::write(wt.join("src/srv.go"), format!("// edit\n{go_src}")).unwrap();
        fs::write(wt.join("src/client.ts"), format!("// edit\n{ts_src}")).unwrap();

        let wt_str = wt.to_str().unwrap();
        let sync = sync_worktree_overlay(wt_str).expect("sync");
        assert_eq!(sync.modified_files, 3);

        let overlay = load_worktree_overlay(wt_str).unwrap().unwrap();
        assert!(
            overlay.tombstoned_symbols.is_empty(),
            "live Rust consts, Go receiver methods, and TS exported consts must NEVER be false-negation tombstoned: {:?}",
            overlay.tombstoned_symbols
        );
        for path in ["src/lib.rs", "src/srv.go", "src/client.ts"] {
            let ext = overlay.added_extractions.get(path).expect(path);
            assert!(
                matches!(
                    ext.engine,
                    devmap_extract::ExtractionEngine::TreeSitter { .. }
                ),
                "expected TreeSitter engine for {path}, got {:?}",
                ext.engine
            );
        }
    }

    fn fixture_with_csr() -> (tempfile::TempDir, u64) {
        let anchor_dir = crate::codeintel::tests::repo_with_one_generation();
        crate::test_support::trust_repo(anchor_dir.path());
        let csr =
            crate::codeintel::open_repo_csr_graph(anchor_dir.path().to_str().unwrap()).unwrap();
        let generation = csr.generation();
        (anchor_dir, generation)
    }

    fn write_bulk_files(dir: &Path, count: usize) {
        let bulk = dir.join("bulk");
        fs::create_dir_all(&bulk).unwrap();
        for i in 0..count {
            fs::write(bulk.join(format!("f{i}.txt")), b"x").unwrap();
        }
    }

    #[test]
    fn readonly_dirty_scan_reports_a_capped_delta_list_as_truncated() {
        let (anchor_dir, _) = fixture_with_csr();
        let anchor = anchor_dir.path();
        let anchor_str = anchor.to_str().unwrap();

        let (existing, _) = collect_worktree_deltas(anchor, None).unwrap();
        write_bulk_files(anchor, MAX_OVERLAY_DIRTY_FILES - existing.len());
        let at_cap = compute_worktree_dirty_nodes_readonly(anchor_str, None).unwrap();
        assert!(
            !at_cap.truncated,
            "exactly {MAX_OVERLAY_DIRTY_FILES} changed files is complete, not truncated"
        );

        fs::write(anchor.join("bulk/one_more.txt"), b"x").unwrap();
        let over = compute_worktree_dirty_nodes_readonly(anchor_str, None).unwrap();
        assert!(
            over.truncated,
            "{} changed files exceed the {MAX_OVERLAY_DIRTY_FILES} cap and must say so",
            MAX_OVERLAY_DIRTY_FILES + 1
        );
    }

    #[test]
    fn readonly_dirty_scan_fails_loudly_on_corrupt_stack_lease_or_delta_sqlite() {
        let (anchor_dir, _) = seed_fresh_anchor_for_zero_copy();
        let anchor = anchor_dir.path();

        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("wt-corrupt-scan");
        git(
            anchor,
            &[
                "worktree",
                "add",
                "-b",
                "lane/corrupt-scan",
                wt.to_str().unwrap(),
            ],
        );
        crate::test_support::trust_repo(&wt);
        provision_worktree_devmap_zero_copy(anchor, &wt).unwrap();

        let wt_str = wt.to_str().unwrap();
        let delta_path = devmap_extract::paths::stack_delta_path(&wt);
        fs::write(&delta_path, b"not a valid sqlite database").unwrap();
        assert!(
            compute_worktree_dirty_nodes_readonly(wt_str, Some(anchor)).is_err(),
            "corrupt delta.sqlite must fail scan loudly"
        );
        fs::remove_file(&delta_path).unwrap();

        let lease_path = devmap_extract::paths::stack_lease_path(&wt);
        fs::write(&lease_path, b"{ not json").unwrap();
        assert!(
            compute_worktree_dirty_nodes_readonly(wt_str, Some(anchor)).is_err(),
            "corrupt stack_lease.json must fail scan loudly"
        );
    }

    #[test]
    fn a_failed_untracked_listing_is_an_error_not_an_empty_listing() {
        let failed = BoundedRun {
            stdout: Vec::new(),
            stderr: b"fatal: not a git repository".to_vec(),
            success: false,
            status_code: 128,
            incomplete: None,
            stderr_incomplete: None,
            cancelled: false,
        };
        let mut map = HashMap::new();
        let err = merge_untracked_listing(&mut map, failed).unwrap_err();
        assert!(err.message().contains("128"), "{err:?}");
        assert!(map.is_empty());

        let ok = BoundedRun {
            stdout: b"new.rs\0dir/other.rs\0".to_vec(),
            stderr: Vec::new(),
            success: true,
            status_code: 0,
            incomplete: None,
            stderr_incomplete: None,
            cancelled: false,
        };
        merge_untracked_listing(&mut map, ok).unwrap();
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn provision_worktree_devmap_rebinds_via_seed_rebind_and_clones_resolution_reuse() {
        let anchor_dir = crate::codeintel::tests::repo_with_one_generation();
        let anchor = anchor_dir.path();
        crate::test_support::trust_repo(anchor);
        let canonical_anchor = fs::canonicalize(anchor).unwrap();
        {
            let anchor_conn = Connection::open(devmap_query::paths::store_path(anchor)).unwrap();
            anchor_conn
                .execute(
                    "INSERT INTO generations (id, created_at, head_sha, analysis_json, repo_root)
                     VALUES (0, 0.0, NULL, '{}', ?1)",
                    [canonical_anchor.to_string_lossy().as_ref()],
                )
                .unwrap();
        }

        let anchor_state = devmap_query::paths::state_dir(anchor);
        let reuse_dir = anchor_state.join("resolution-reuse-1");
        fs::create_dir_all(&reuse_dir).unwrap();
        fs::write(reuse_dir.join("unit.bin"), b"cached-resolution-artifact").unwrap();

        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("lane-rebind-reuse");
        git(
            anchor,
            &[
                "worktree",
                "add",
                "-b",
                "lane/rebind-reuse",
                wt.to_str().unwrap(),
            ],
        );
        crate::test_support::trust_repo(&wt);

        let report = provision_worktree_devmap(anchor, &wt).expect("provision");
        assert!(report.seeded, "{report:?}");

        let wt_reuse_file = devmap_query::paths::state_dir(&wt).join("resolution-reuse-1/unit.bin");
        assert_eq!(
            fs::read(&wt_reuse_file).unwrap(),
            b"cached-resolution-artifact"
        );

        let wt_db = devmap_query::paths::store_path(&wt);
        let canonical_wt = fs::canonicalize(&wt).unwrap();
        let reopened = Store::open_read_only(&wt_db).unwrap();
        let effective_root = reopened.latest_repo_root().unwrap().unwrap();
        drop(reopened);
        assert_eq!(fs::canonicalize(&effective_root).unwrap(), canonical_wt);

        let conn = Connection::open(&wt_db).unwrap();
        let hist_gen_root: String = conn
            .query_row(
                "SELECT repo_root FROM generations WHERE id = 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            fs::canonicalize(&hist_gen_root).unwrap(),
            canonical_anchor,
            "rebind_staged must preserve historical generation CoW pages"
        );
        let latest_gen_root: String = conn
            .query_row(
                "SELECT repo_root FROM generations ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(PathBuf::from(latest_gen_root), canonical_wt);
        let rebound_root: String = conn
            .query_row(
                "SELECT seeded_repo_root FROM seed_rebind WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(PathBuf::from(rebound_root), canonical_wt);
    }

    #[test]
    fn sparse_worktree_prefers_stacked_monorepo_base_over_cone_only_local_store() {
        let (anchor_dir, _head_sha) = seed_fresh_anchor_for_zero_copy();
        let anchor = anchor_dir.path();

        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("wt-sparse-cone");
        git(
            anchor,
            &[
                "worktree",
                "add",
                "--no-checkout",
                "-b",
                "lane/sparse-cone",
                wt.to_str().unwrap(),
            ],
        );
        git(&wt, &["sparse-checkout", "init", "--cone"]);
        git(&wt, &["sparse-checkout", "set", "tests"]);
        git(&wt, &["checkout"]);
        crate::test_support::trust_repo(&wt);
        assert!(is_sparse_worktree(&wt));
        assert!(!wt.join("src/callee.rs").exists());

        provision_worktree_devmap_zero_copy(anchor, &wt).expect("zero-copy provision");

        let local_db = devmap_query::paths::store_path(&wt);
        fs::create_dir_all(local_db.parent().unwrap()).unwrap();
        let partial = Store::open(&local_db).unwrap();
        partial.bind_repo_root(&wt).unwrap();
        drop(partial);
        let conn = Connection::open(&local_db).unwrap();
        conn.execute(
            "INSERT INTO generations (id, created_at, head_sha, analysis_json, repo_root)
             VALUES (1, 0.0, NULL, '{\"total_files\":0,\"total_symbols\":0,\"total_edges\":0,\"dead_symbols\":[],\"communities\":[],\"status\":\"Ok\",\"unresolved_calls\":0}', ?1)",
            [wt.to_string_lossy().as_ref()],
        )
        .unwrap();
        drop(conn);

        let wt_str = wt.to_str().unwrap();
        let hits = crate::codeintel::search(wt_str, "probe_callee", None);
        assert!(hits.available, "{hits:?}");
        assert!(
            hits.items.iter().any(|h| h.symbol_name == "probe_callee"),
            "sparse worktree must serve out-of-cone monorepo symbols: {hits:?}"
        );
    }

    #[test]
    fn zero_copy_affected_tests_passes_on_disjoint_edit_and_fails_closed_on_new_caller() {
        let (anchor_dir, _head_sha) = seed_fresh_anchor_for_zero_copy();
        let anchor = anchor_dir.path();

        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("wt-disjoint-affected");
        git(
            anchor,
            &[
                "worktree",
                "add",
                "-b",
                "lane/disjoint-affected",
                wt.to_str().unwrap(),
            ],
        );
        crate::test_support::trust_repo(&wt);
        provision_worktree_devmap_zero_copy(anchor, &wt).expect("zero-copy");
        let wt_str = wt.to_str().unwrap();

        fs::write(wt.join("README.md"), "# Worktree notes\n").unwrap();
        fs::write(
            wt.join("src/unrelated.rs"),
            "pub fn completely_independent_feature() -> usize { 123 }\n",
        )
        .unwrap();

        let disjoint = crate::codeintel::affected_tests(
            wt_str,
            &["probe_callee".to_string()],
            None,
            crate::codeintel::WalkBounds::default(),
        );
        assert!(
            !disjoint.fail_closed,
            "unrelated worktree edits disjoint from target cone must NOT force fail_closed: {disjoint:?}"
        );
        assert!(disjoint
            .tests
            .items
            .iter()
            .any(|t| t.path == "tests/caller_test.rs"));

        fs::write(
            wt.join("src/sneaky_new_caller.rs"),
            "pub fn sneaky() { crate::callee::probe_callee(); }\n",
        )
        .unwrap();

        let overlapping = crate::codeintel::affected_tests(
            wt_str,
            &["probe_callee".to_string()],
            None,
            crate::codeintel::WalkBounds::default(),
        );
        assert!(
            overlapping.fail_closed,
            "new caller referencing target cone identifier must fail closed: {overlapping:?}"
        );
    }

    #[test]
    fn forged_gitlink_worktree_is_refused_by_stacked_store_openers() {
        let (anchor_dir, _head_sha) = seed_fresh_anchor_for_zero_copy();
        let anchor = anchor_dir.path();

        let legit_parent = tempfile::tempdir().unwrap();
        let legit_wt = legit_parent.path().join("legit-lane");
        git(
            anchor,
            &[
                "worktree",
                "add",
                "-b",
                "lane/legit",
                legit_wt.to_str().unwrap(),
            ],
        );
        crate::test_support::trust_repo(&legit_wt);
        provision_worktree_devmap_zero_copy(anchor, &legit_wt).expect("legit zero-copy");

        let forged_parent = tempfile::tempdir().unwrap();
        let forged_wt = forged_parent.path().join("forged-lane");
        fs::create_dir_all(&forged_wt).unwrap();
        let legit_git_file = fs::read_to_string(legit_wt.join(".git")).unwrap();
        fs::write(forged_wt.join(".git"), legit_git_file).unwrap();
        crate::test_support::trust_repo(&forged_wt);

        let legit_lease = devmap_extract::paths::stack_lease_path(&legit_wt);
        let forged_ci = forged_wt.join(".devcouncil/codeintel");
        fs::create_dir_all(&forged_ci).unwrap();
        fs::copy(&legit_lease, forged_ci.join("stack_lease.json")).unwrap();

        let forged_str = forged_wt.to_str().unwrap();
        assert!(load_worktree_overlay(forged_str).is_err());
        let st = crate::codeintel::status(forged_str);
        assert!(
            !st.available,
            "forged gitlink with stack_lease.json must fail closed: {st:?}"
        );
    }

    #[test]
    fn stacked_delta_sqlite_serves_search_impact_and_affected_tests_and_fails_closed_when_unsynced()
    {
        let _devmap = bind_real_devmap();
        let (anchor_dir, _head_sha) = seed_fresh_anchor_for_zero_copy();
        let anchor = anchor_dir.path();

        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("wt-stacked-delta");
        git(
            anchor,
            &[
                "worktree",
                "add",
                "-b",
                "lane/stacked-delta",
                wt.to_str().unwrap(),
            ],
        );
        crate::test_support::trust_repo(&wt);
        provision_worktree_devmap_zero_copy(anchor, &wt).expect("provision stacked");

        let extra_test_src =
            "#[test]\nfn delta_added_test_for_callee() { crate::callee::probe_callee(); }\n";
        fs::write(wt.join("tests/extra_test.rs"), extra_test_src).unwrap();

        let wt_str = wt.to_str().unwrap();
        sync_worktree_overlay(wt_str).expect("sync via real Tree-sitter devmap");

        let hits = crate::codeintel::search(wt_str, "delta_added_test_for_callee", None);
        assert!(hits.available, "{hits:?}");
        assert!(
            hits.items.iter().any(|h| {
                h.file_path == "tests/extra_test.rs"
                    && h.symbol_name == "delta_added_test_for_callee"
                    && h.source_span.contains("probe_callee")
            }),
            "search must return delta.sqlite symbol with intact source span: {hits:?}"
        );

        let imp = crate::codeintel::impact(wt_str, "probe_callee", None);
        assert!(imp.available, "{imp:?}");
        assert!(
            imp.items
                .iter()
                .any(|e| e.source_file == "tests/extra_test.rs"),
            "impact must include caller edge from delta.sqlite: {imp:?}"
        );

        let aff = crate::codeintel::affected_tests(
            wt_str,
            &["probe_callee".to_string()],
            None,
            crate::codeintel::WalkBounds::default(),
        );
        assert!(
            !aff.fail_closed,
            "synced delta.sqlite must answer affected_tests without fail_closed: {aff:?}"
        );
        assert!(aff
            .tests
            .items
            .iter()
            .any(|t| t.path == "tests/caller_test.rs"));
        assert!(aff
            .tests
            .items
            .iter()
            .any(|t| t.path == "tests/extra_test.rs"));

        // Writing an UNSYNCED caller to disk without syncing `delta.sqlite` MUST
        // immediately flip `affected_tests` back to `fail_closed == true`!
        fs::write(
            wt.join("src/unsynced_caller.rs"),
            "pub fn unsynced() { crate::callee::probe_callee(); }\n",
        )
        .unwrap();
        let unsynced_aff = crate::codeintel::affected_tests(
            wt_str,
            &["probe_callee".to_string()],
            None,
            crate::codeintel::WalkBounds::default(),
        );
        assert!(
            unsynced_aff.fail_closed,
            "unsynced disk caller beyond delta.sqlite must fail closed: {unsynced_aff:?}"
        );
    }

    #[test]
    fn stacked_synced_delta_keeps_status_not_fresh_resolves_import_aliases_and_refuses_tampered_csr(
    ) {
        let _devmap = bind_real_devmap();
        let (anchor_dir, _head_sha) = seed_fresh_anchor_for_zero_copy();
        let anchor = anchor_dir.path();

        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("wt-block3-alias-csr");
        git(
            anchor,
            &[
                "worktree",
                "add",
                "-b",
                "lane/block3-alias-csr",
                wt.to_str().unwrap(),
            ],
        );
        crate::test_support::trust_repo(&wt);
        provision_worktree_devmap_zero_copy(anchor, &wt).expect("provision stacked");
        let wt_str = wt.to_str().unwrap();

        // 1. Write a test using an aliased Rust `use ... as ...` import and sync delta.sqlite.
        let alias_test_src = concat!(
            "use crate::callee::probe_callee as aliased_probe_callee;\n",
            "#[test]\n",
            "fn aliased_caller_test() { aliased_probe_callee(); }\n",
        );
        fs::write(wt.join("tests/alias_caller_test.rs"), alias_test_src).unwrap();
        sync_worktree_overlay(wt_str).expect("sync aliased caller into delta.sqlite");

        // 2. BLOCK 3: a stacked worktree with dirty/delta files must NEVER report `is_fresh: true`!
        let st = crate::codeintel::status(wt_str);
        assert!(st.available, "status must remain available: {st:?}");
        assert_eq!(
            st.is_fresh,
            Some(false),
            "BLOCK 3: synced stacked delta changes must NEVER force status.is_fresh = true: {st:?}"
        );
        assert_eq!(st.source_freshness, Some(false), "{st:?}");
        assert!(
            st.freshness_reason
                .as_deref()
                .is_some_and(|r| r.contains("stacked delta")),
            "expected stacked delta freshness_reason, got: {st:?}"
        );

        // 3. `affected_tests` must traverse the aliased import edge from `delta.sqlite`,
        //    answer without `fail_closed`, AND stamp `source_freshness.fresh = Some(false)`.
        let aff = crate::codeintel::affected_tests(
            wt_str,
            &["probe_callee".to_string()],
            None,
            crate::codeintel::WalkBounds::default(),
        );
        assert!(
            !aff.fail_closed,
            "fully synced delta.sqlite must answer affected_tests without fail_closed: {aff:?}"
        );
        assert!(
            aff.tests
                .items
                .iter()
                .any(|t| t.path == "tests/alias_caller_test.rs"),
            "aliased import call in delta.sqlite must resolve to probe_callee: {aff:?}"
        );
        assert_eq!(
            aff.tests.source_freshness.fresh,
            Some(false),
            "affected_tests on synced delta must honestly report source_freshness.fresh == Some(false): {aff:?}"
        );

        // 4. Writer lock contention on `delta.sqlite.writer.lock` must preserve inode and block concurrent writers.
        let delta_path = devmap_extract::paths::stack_delta_path(&wt);
        let lock_path = devmap_store::Store::writer_lock_path(&delta_path);
        assert!(
            lock_path.is_file(),
            "stable writer lock file must remain on disk after sync: {}",
            lock_path.display()
        );
        {
            let _held = devmap_store::Store::lock_writer_at(
                &delta_path,
                std::time::Duration::from_millis(200),
            )
            .expect("acquire delta writer lock");
            let contended = devmap_store::Store::lock_writer_at(
                &delta_path,
                std::time::Duration::from_millis(50),
            );
            assert!(
                contended.is_err(),
                "concurrent writer on delta.sqlite.writer.lock must be refused"
            );
        }

        // 5. Corrupting `base.sqlite.csr` bytes without updating `manifest.csr` SHA-256 MUST fail closed
        //    in BOTH `open_worktree_or_anchor_csr` and `codeintel::open_repo_csr_graph` (never falling back
        //    to an unverified `Store::open_latest_csr_sidecar` mmap!).
        let lease = devmap_query::stacked::StackLease::read_from(
            &devmap_extract::paths::stack_lease_path(&wt),
        )
        .unwrap();
        let csr_path = devmap_store::Store::csr_sidecar_path(&lease.base_sqlite_path());
        assert!(csr_path.is_file(), "expected base.sqlite.csr");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&csr_path).unwrap().permissions();
            perms.set_mode(0o644);
            fs::set_permissions(&csr_path, perms).unwrap();
        }
        let mut csr_bytes = fs::read(&csr_path).unwrap();
        let last = csr_bytes.len() - 1;
        csr_bytes[last] ^= 0xFF;
        fs::write(&csr_path, &csr_bytes).unwrap();

        assert!(
            open_worktree_or_anchor_csr(&wt).is_none(),
            "tampered base.sqlite.csr must NOT fall back to unverified Store::open_latest_csr_sidecar"
        );
        let csr_err = crate::codeintel::open_repo_csr_graph(wt_str)
            .err()
            .expect("open_repo_csr_graph must refuse tampered stacked base.sqlite.csr");
        assert!(
            csr_err.message().contains("CSR") || csr_err.message().contains("sha256"),
            "unexpected CSR verification error: {csr_err:?}"
        );
    }
}
