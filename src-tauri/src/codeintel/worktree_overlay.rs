//! Parallel DevMap worktree seeding and ephemeral delta overlays.
//!
//! When an agent or developer creates a linked Git worktree, starting with no
//! DevMap state forces either cold multi-second `devmap build` runs or
//! `available: false` query refusals across every parallel lane. Simply
//! hard-linking or raw-copying `devmap.sqlite` fails two invariants in
//! `devmap-store`:
//! 1. `Store::validate_database_file` refuses any SQLite file with `nlink > 1`.
//! 2. `Store::validate_repo_root` refuses any store whose `pending_state.repo_root`
//!    still names a live sibling worktree (`RepoRootOwner::Live`).
//!
//! This module implements a Scion/Jujutsu-inspired two-layer worktree code
//! intelligence engine:
//! - **Shared physical extents, isolated store ownership**: CoW-clones
//!   (`clonefile(2)` on macOS, `FICLONE` on Linux, online SQLite backup when an
//!   active `-wal` has uncheckpointed frames) the anchor's `devmap.sqlite`,
//!   `<db>.csr` mmap graph sidecar, `repo_map.json`, and `cas/ast/` cache into
//!   the new worktree's git-excluded state directory, then rebinds
//!   `pending_state.repo_root` and `generations.repo_root` to the worktree's
//!   canonical root before atomic placement.
//! - **Ephemeral delta overlay (`overlay.json` + `CasAstStore`)**: diffs the
//!   worktree against the base generation's `head_sha` plus untracked files,
//!   recording `CAS_TOMBSTONE` for deleted files/symbols, `blake3_hash` digests
//!   for modified/added files, reused `CasAstStore` AST payloads across lanes,
//!   and `RoaringBitmap` dirty CSR node IDs.

use crate::devmap::init::ensure_dir_excluded;
use crate::engine::cow_clone::{reflink_copy_dir, reflink_copy_file};
use crate::engine::git_cli::{git_captured, validate_repo, BoundedRun};
use crate::ipc_error::CommandError;
use devmap_query::{blake3_hash, CasAstStore, WorktreeOverlay, CAS_TOMBSTONE};
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

/// Maximum size of a dirty source file hashed into a worktree overlay (8 MiB).
pub const MAX_OVERLAY_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Maximum number of dirty/untracked paths inspected in one overlay sync pass.
pub const MAX_OVERLAY_DIRTY_FILES: usize = 10_000;

/// Relative path of the persisted [`WorktreeOverlay`] inside the DevMap state directory.
pub const OVERLAY_RELPATH: &str = "codeintel/overlay.json";

/// Relative path of the zero-copy shared base store pointer inside the DevMap state directory.
pub const BASE_STORE_REF_RELPATH: &str = "codeintel/base_store_ref.json";

/// Relative path of the shared [`CasAstStore`] directory inside the DevMap state directory.
pub const CAS_AST_RELPATH: &str = "codeintel/cas/ast";

/// Lightweight (<512-byte) pointer linking an ephemeral worktree to its anchor
/// repository's immutable DevMap base generation, mmap CSR sidecar, and CAS AST
/// directory without copying `devmap.sqlite`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ZeroCopyBaseRef {
    pub anchor_repo_root: String,
    pub anchor_db_path: String,
    pub anchor_csr_path: Option<String>,
    pub anchor_cas_dir: Option<String>,
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

/// Summary of an ephemeral [`WorktreeOverlay`] synchronization pass.
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

/// Resolves the path to `<state_dir>/codeintel/overlay.json` for `repo`.
pub fn worktree_overlay_path(repo_path: &str) -> PathBuf {
    let state = devmap_query::paths::state_dir(Path::new(repo_path));
    OVERLAY_RELPATH
        .split('/')
        .fold(state, |acc, part| acc.join(part))
}

/// Resolves the path to `<state_dir>/codeintel/base_store_ref.json` for `repo`.
pub fn zero_copy_base_ref_path(repo_path: &str) -> PathBuf {
    let state = devmap_query::paths::state_dir(Path::new(repo_path));
    BASE_STORE_REF_RELPATH
        .split('/')
        .fold(state, |acc, part| acc.join(part))
}

fn cas_ast_dir_for_state(state_dir: &Path) -> PathBuf {
    CAS_AST_RELPATH
        .split('/')
        .fold(state_dir.to_path_buf(), |acc, part| acc.join(part))
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

/// Relative path inside `<common_git_dir>/devmap/` holding shared fallback
/// declaration extractions across sibling zero-copy worktrees without writing
/// `RegexFallback` payloads into the anchor's full tree-sitter `cas/ast/` (`F4`).
const SHARED_OVERLAY_CAS_RELPATH: &str = "devmap/cas/overlay_ast";

fn shared_overlay_cas_dir(repo: &Path) -> Option<PathBuf> {
    let common = crate::engine::git_cli::resolve_git_common_dir(repo).ok()?;
    Some(
        SHARED_OVERLAY_CAS_RELPATH
            .split('/')
            .fold(common, |acc, part| acc.join(part)),
    )
}

fn fallback_supports_symbol_kind(kind: &str) -> bool {
    matches!(
        kind.to_ascii_lowercase().as_str(),
        "function" | "class" | "struct" | "interface" | "enum" | "module" | "type"
    )
}

/// Strips line (`//`, `#`, `--`) and C-style block (`/* ... */`) comments so
/// deleted symbol names mentioned only in comments do not suppress tombstoning,
/// while live receiver methods, macros, and multi-line signatures keep their
/// identifiers intact (`F4`).
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
            // Preserve Rust attributes `#[...]` / `#![...]`, strip `#` comments.
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

/// Provisions a linked worktree's DevMap store, mmap CSR graph sidecar,
/// navigator manifests, CAS AST cache, and initial [`WorktreeOverlay`] from
/// `anchor_repo` using Copy-on-Write cloning where supported.
///
/// Safe to run concurrently inside `std::thread::scope` alongside build-cache
/// CoW cloning. Never modifies tracked files or leaves unignored files in
/// `worktree_repo`.
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

    // Zero-spawn fast check: if the anchor path holds neither a DevMap SQLite
    // file, a zero-copy base ref, nor a stacked lease on disk, skip before spawning `git rev-parse`.
    if !devmap_query::paths::store_path(anchor_repo).is_file()
        && !zero_copy_base_ref_path(anchor_str).is_file()
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
        if let Some(chained) = load_zero_copy_base_ref_internal(&anchor)? {
            anchor = validate_repo(&chained.anchor_repo_root)?;
        } else if let Some(donor) = resolve_stack_lease_donor(&anchor)? {
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

    // Determine which state directory name `worktree` will resolve to.
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

    // Fail closed if Git does not confirm the state directory is excluded from `git status`.
    let exclude = ensure_dir_excluded(&worktree, target_state_name, DEVMAP_EXCLUDE_MARKER);
    if !exclude.is_clean() {
        return Err(CommandError::from(format!(
            "refusing to seed DevMap into {} because {target_state_name}/ is not git-ignored: {exclude:?}",
            worktree.display()
        )));
    }

    // Idempotent fast-return when the worktree already holds a valid local indexed store.
    if let Ok(existing_store) = super::open_local_indexed_store(&worktree) {
        let existing_gen = existing_store
            .latest_generation_id()
            .ok()
            .flatten()
            .unwrap_or(generation_id);
        drop(existing_store);
        let target_db = devmap_query::paths::store_path(&worktree);
        let (overlay, overlay_error) =
            match sync_worktree_overlay_internal(&worktree, Some(existing_gen)) {
                Ok(report) => (Some(report), None),
                Err(e) => (None, Some(format!("overlay sync failed: {}", e.message()))),
            };
        return Ok(DevmapWorktreeProvision {
            seeded: true,
            is_cow: false,
            used_online_backup: false,
            zero_copy_base_ref: false,
            generation_id: Some(existing_gen),
            db_path: Some(target_db.to_string_lossy().into_owned()),
            csr_seeded: Store::csr_sidecar_path(&target_db).is_file(),
            repo_map_seeded: devmap_query::paths::repo_map_path(&worktree).is_file(),
            cas_seeded: cas_ast_dir_for_state(&worktree.join(target_state_name)).is_dir(),
            overlay_path: overlay.map(|r| r.overlay_path),
            duration_ms: start.elapsed().as_millis() as u64,
            reason: overlay_error,
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
    let tmp_db = target_codeintel.join(format!(
        ".devmap.sqlite.seed.{}.{}",
        std::process::id(),
        next_seed_seq()
    ));
    let mut tmp_guard = TempSqliteGuard::new(tmp_db.clone());

    let seed_method = devmap_store::seed::stage_seed_healing_wal(&anchor_db, &tmp_db).map_err(
        |e| {
            CommandError::from(format!(
                "failed to stage DevMap seed from {}: {e}",
                anchor_db.display()
            ))
        },
    )?;
    let is_cow = seed_method == devmap_store::seed::SeedMethod::Clone;
    let used_online_backup = seed_method == devmap_store::seed::SeedMethod::Backup;

    devmap_store::seed::rebind_staged(&tmp_db, &worktree)
        .map_err(|e| CommandError::from(format!("failed to rebind seeded DevMap store: {e}")))?;
    devmap_store::seed::settle_staged(&tmp_db)
        .map_err(|e| CommandError::from(format!("failed to settle seeded DevMap store: {e}")))?;
    tmp_guard.clean_sidecars();

    // Remove any stale unindexed DB before atomic placement.
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

    // Immediately verify the placed store passes `nlink == 1` and `validate_repo_root`.
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
    let anchor_cas = cas_ast_dir_for_state(&anchor_state);
    let target_cas = cas_ast_dir_for_state(&target_state);

    // Seed CSR sidecar, navigator manifests + resolution-reuse caches, and CAS AST cache concurrently.
    let (csr_seeded, repo_map_seeded, cas_seeded) = std::thread::scope(|scope| {
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
            // Cache CSR on the anchor once when excluded so subsequent worktrees clone in <1 ms (`F6`).
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

        let cas_task = scope.spawn(|| {
            if anchor_cas.is_dir() && !target_cas.exists() {
                if let Some(parent) = target_cas.parent() {
                    if fs::create_dir_all(parent).is_err() {
                        return false;
                    }
                }
                return reflink_copy_dir(&anchor_cas, &target_cas).is_ok();
            }
            target_cas.is_dir()
        });

        (
            csr_task.join().unwrap_or(false),
            manifest_task.join().unwrap_or(false),
            cas_task.join().unwrap_or(false),
        )
    });

    drop(seeded_store);
    let overlay_report = sync_worktree_overlay_internal(&worktree, Some(generation_id))?;

    Ok(DevmapWorktreeProvision {
        seeded: true,
        is_cow,
        used_online_backup,
        zero_copy_base_ref: false,
        generation_id: Some(generation_id),
        db_path: Some(target_db.to_string_lossy().into_owned()),
        csr_seeded,
        repo_map_seeded,
        cas_seeded,
        overlay_path: Some(overlay_report.overlay_path),
        duration_ms: start.elapsed().as_millis() as u64,
        reason: None,
    })
}

/// Provisions a zero-copy DevMap shared base graph reference (`base_store_ref.json`),
/// canonical immutable `StackedStore` lease (`stack_lease.json` + `0444` base snapshot
/// under `<common_git_dir>/devmap/bases/`), and ephemeral [`WorktreeOverlay`]
/// (`overlay.json`) inside `worktree_repo` WITHOUT copying `devmap.sqlite` or `cas/ast/`
/// into `worktree_repo` (`F1`, `F2`, `F6`, `F10`).
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
    if !devmap_query::paths::store_path(anchor_repo).is_file()
        && !zero_copy_base_ref_path(anchor_str).is_file()
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
        if let Some(chained) = load_zero_copy_base_ref_internal(&anchor)? {
            anchor = validate_repo(&chained.anchor_repo_root)?;
        } else if let Some(donor) = resolve_stack_lease_donor(&anchor)? {
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
    if anchor_common != worktree_common {
        return Err(CommandError::from(
            "refusing zero-copy DevMap base ref across unrelated repositories",
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
    let base_head_sha = anchor_store.latest_generation_head_sha().ok().flatten();
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
            "refusing to write zero-copy DevMap pointer into {} because {target_state_name}/ is not git-ignored: {exclude:?}",
            worktree.display()
        )));
    }

    let anchor_cas = cas_ast_dir_for_state(&anchor_state);
    if !anchor_cas.is_dir()
        && ensure_dir_excluded(&anchor, anchor_state_name, DEVMAP_EXCLUDE_MARKER).is_clean()
    {
        let _ = fs::create_dir_all(&anchor_cas);
    }

    // Create the excluded target state directory BEFORE `acquire_stack_lease` so
    // `devmap_extract::paths::state_dir(&worktree)` picks `target_state_name` even
    // when the anchor uses `.devmap` (`LEGACY_STATE_DIR`).
    let target_state = worktree.join(target_state_name);
    let ref_path = BASE_STORE_REF_RELPATH
        .split('/')
        .fold(target_state.clone(), |acc, part| acc.join(part));
    if let Some(parent) = ref_path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            CommandError::from(format!("failed to create {}: {e}", parent.display()))
        })?;
    }

    // Publish sealed 0444 base snapshot under `<common_git_dir>/devmap/bases/gen-<G>/`
    // and write canonical `stack_lease.json` + `delta.sqlite` so `devmap` CLI, MCP,
    // and hooks recognize this zero-copy worktree natively (`F1`, `F10`).
    let stack_lease = devmap_query::stacked::provision_stacked_worktree(&anchor, &worktree)
        .map_err(|e| CommandError::from(format!("failed to acquire DevMap stack lease: {e}")))?;
    let base_csr_candidate =
        devmap_extract::paths::csr_path_for_store(&stack_lease.base_sqlite_path());

    let base_ref = ZeroCopyBaseRef {
        anchor_repo_root: anchor.to_string_lossy().into_owned(),
        anchor_db_path: anchor_db.to_string_lossy().into_owned(),
        anchor_csr_path: if base_csr_candidate.is_file() {
            Some(base_csr_candidate.to_string_lossy().into_owned())
        } else if anchor_csr.is_file() {
            Some(anchor_csr.to_string_lossy().into_owned())
        } else {
            None
        },
        anchor_cas_dir: anchor_cas
            .is_dir()
            .then(|| anchor_cas.to_string_lossy().into_owned()),
        generation_id,
        base_head_sha,
    };

    let encoded_ref = serde_json::to_vec_pretty(&base_ref)
        .map_err(|e| CommandError::from(format!("cannot serialize ZeroCopyBaseRef: {e}")))?;
    devmap_query::write_atomic(&ref_path, &encoded_ref)
        .map_err(|e| CommandError::from(format!("cannot write {}: {e}", ref_path.display())))?;

    let overlay_report = sync_worktree_overlay_internal(&worktree, Some(generation_id))?;

    Ok(DevmapWorktreeProvision {
        seeded: true,
        is_cow: false,
        used_online_backup: false,
        zero_copy_base_ref: true,
        generation_id: Some(generation_id),
        db_path: Some(stack_lease.base_sqlite_path().to_string_lossy().into_owned()),
        csr_seeded: base_ref.anchor_csr_path.is_some(),
        repo_map_seeded: devmap_query::paths::repo_map_path(&worktree).is_file(),
        cas_seeded: base_ref.anchor_cas_dir.is_some(),
        overlay_path: Some(overlay_report.overlay_path),
        duration_ms: start.elapsed().as_millis() as u64,
        reason: None,
    })
}

/// Provisions a stacked (`stack_lease.json` + `delta.sqlite` + `base_store_ref.json`)
/// zero-copy DevMap store for `worktree_repo` from `anchor_repo`.
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
            // In `-z` porcelain v1, `R`/`C` puts the destination path in the first
            // token and the source path in the next NUL-separated token.
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
            // A cut-short diff would drop deltas; fall through to the full status read instead.
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

    // Never include DevMap's own state directories in the overlay.
    map.retain(|path, _| {
        !devmap_query::paths::STATE_DIR_NAMES
            .iter()
            .any(|dir| path == *dir || path.starts_with(&format!("{dir}/")))
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

/// Reads and verifies the persisted [`ZeroCopyBaseRef`] for `repo_path` when one exists.
///
/// Fails closed if `anchor_repo_root` does not share the exact same Git common
/// directory as `repo_path`, preventing cross-repository pointer spoofing.
pub fn load_zero_copy_base_ref(repo_path: &str) -> Result<Option<ZeroCopyBaseRef>, CommandError> {
    let repo = validate_repo(repo_path)?;
    load_zero_copy_base_ref_internal(&repo)
}

fn load_zero_copy_base_ref_internal(repo: &Path) -> Result<Option<ZeroCopyBaseRef>, CommandError> {
    let state_dir = devmap_query::paths::state_dir(repo);
    let ref_path = BASE_STORE_REF_RELPATH
        .split('/')
        .fold(state_dir, |acc, part| acc.join(part));
    if !ref_path.is_file() {
        return Ok(None);
    }
    let bytes = read_bounded_file(&ref_path, 64 * 1024)?.ok_or_else(|| {
        CommandError::from(format!(
            "zero-copy DevMap base ref {} exceeds size bound or is not a regular file",
            ref_path.display()
        ))
    })?;
    let base_ref: ZeroCopyBaseRef = serde_json::from_slice(&bytes).map_err(|e| {
        CommandError::from(format!(
            "failed to parse zero-copy DevMap base ref {}: {e}",
            ref_path.display()
        ))
    })?;
    let anchor_root = validate_repo(&base_ref.anchor_repo_root)?;
    let repo_common = crate::engine::git_cli::resolve_git_common_dir(repo)?;
    let anchor_common = crate::engine::git_cli::resolve_git_common_dir(&anchor_root)?;
    if repo_common != anchor_common {
        return Err(CommandError::from(format!(
            "refusing zero-copy DevMap base ref {} pointing outside worktree family ({})",
            ref_path.display(),
            anchor_root.display()
        )));
    }
    Ok(Some(base_ref))
}

fn resolve_stack_lease_donor(repo: &Path) -> Result<Option<PathBuf>, CommandError> {
    let lease_path = devmap_extract::paths::stack_lease_path(repo);
    if !lease_path.is_file() {
        return Ok(None);
    }
    let lease = devmap_query::stacked::StackLease::read_from(&lease_path)
        .map_err(|e| CommandError::from(format!("failed to read stack lease: {e}")))?;
    let manifest_path = PathBuf::from(&lease.base_dir).join("manifest.json");
    let raw = read_bounded_file(&manifest_path, 64 * 1024)?.ok_or_else(|| {
        CommandError::from(format!(
            "stacked base manifest {} is missing or oversized",
            manifest_path.display()
        ))
    })?;
    let manifest: devmap_query::stacked::BaseManifest = serde_json::from_slice(&raw).map_err(|e| {
        CommandError::from(format!(
            "failed to parse stacked base manifest {}: {e}",
            manifest_path.display()
        ))
    })?;
    let donor = validate_repo(&manifest.source_repo_root)?;
    if !devmap_extract::paths::same_registered_repo(repo, &donor) {
        return Err(CommandError::from(format!(
            "refusing stack lease {} pointing outside worktree family ({})",
            lease_path.display(),
            donor.display()
        )));
    }
    Ok(Some(donor))
}

fn open_verified_stack_lease_store(
    repo: &Path,
) -> Result<Option<(Store, ZeroCopyBaseRef)>, CommandError> {
    let lease_path = devmap_extract::paths::stack_lease_path(repo);
    if !lease_path.is_file() {
        return Ok(None);
    }
    let lease = devmap_query::stacked::StackLease::read_from(&lease_path)
        .map_err(|e| CommandError::from(format!("failed to read stack lease: {e}")))?;
    let manifest_path = PathBuf::from(&lease.base_dir).join("manifest.json");
    let raw = read_bounded_file(&manifest_path, 64 * 1024)?.ok_or_else(|| {
        CommandError::from(format!(
            "stacked base manifest {} is missing or oversized",
            manifest_path.display()
        ))
    })?;
    let manifest: devmap_query::stacked::BaseManifest = serde_json::from_slice(&raw).map_err(|e| {
        CommandError::from(format!(
            "failed to parse stacked base manifest {}: {e}",
            manifest_path.display()
        ))
    })?;
    let anchor_root = validate_repo(&manifest.source_repo_root)?;
    let repo_common = crate::engine::git_cli::resolve_git_common_dir(repo)?;
    let anchor_common = crate::engine::git_cli::resolve_git_common_dir(&anchor_root)?;
    if repo_common != anchor_common {
        return Err(CommandError::from(format!(
            "refusing stack lease {} pointing outside worktree family ({})",
            lease_path.display(),
            anchor_root.display()
        )));
    }
    let base_sqlite = lease.base_sqlite_path();
    let store = Store::open_immutable_snapshot(&base_sqlite, None).map_err(|e| {
        CommandError::from(format!(
            "failed to open immutable base snapshot {}: {e}",
            base_sqlite.display()
        ))
    })?;
    let base_csr = devmap_extract::paths::csr_path_for_store(&base_sqlite);
    let anchor_state = devmap_query::paths::state_dir(&anchor_root);
    let anchor_cas = cas_ast_dir_for_state(&anchor_state);
    let base_head_sha = store.latest_generation_head_sha().ok().flatten();
    let base_ref = ZeroCopyBaseRef {
        anchor_repo_root: anchor_root.to_string_lossy().into_owned(),
        anchor_db_path: base_sqlite.to_string_lossy().into_owned(),
        anchor_csr_path: base_csr
            .is_file()
            .then(|| base_csr.to_string_lossy().into_owned()),
        anchor_cas_dir: anchor_cas
            .is_dir()
            .then(|| anchor_cas.to_string_lossy().into_owned()),
        generation_id: lease.generation_id,
        base_head_sha,
    };
    Ok(Some((store, base_ref)))
}

pub(super) fn open_verified_zero_copy_anchor_store(
    repo: &Path,
) -> Result<Option<(Store, ZeroCopyBaseRef)>, CommandError> {
    if let Some(base_ref) = load_zero_copy_base_ref_internal(repo)? {
        // When a sealed `0444` immutable base snapshot also exists via `stack_lease.json`,
        // prefer opening `?immutable=1` so readers create zero `-shm`/`-wal` contention (`OQ1`).
        if let Ok(Some((immutable_store, mut lease_ref))) = open_verified_stack_lease_store(repo) {
            if lease_ref.generation_id == base_ref.generation_id {
                if lease_ref.anchor_csr_path.is_none() {
                    lease_ref.anchor_csr_path = base_ref.anchor_csr_path;
                }
                if lease_ref.anchor_cas_dir.is_none() {
                    lease_ref.anchor_cas_dir = base_ref.anchor_cas_dir;
                }
                return Ok(Some((immutable_store, lease_ref)));
            }
        }
        let anchor_root = validate_repo(&base_ref.anchor_repo_root)?;
        let store = super::open_local_indexed_store(&anchor_root).map_err(CommandError::from)?;
        return Ok(Some((store, base_ref)));
    }
    open_verified_stack_lease_store(repo)
}

pub(super) fn is_sparse_worktree(repo: &Path) -> bool {
    // `--get` is a read; an unset key exits 1, which is "not sparse".
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

fn head_sha_of(repo: &Path) -> Option<String> {
    git_captured(repo, &["rev-parse", "--verify", "HEAD"])
        .ok()
        .filter(|run| run.status_code == 0)
        .map(|run| String::from_utf8_lossy(&run.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Evaluates source freshness of a zero-copy `worktree` against `anchor_store`'s
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
                        "sparse worktree HEAD differs from the indexed anchor generation; rebuild or sync overlay"
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
                        "sparse worktree differs from the indexed anchor generation; rebuild or sync overlay"
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
                "source freshness unverified: sparse zero-copy worktree cannot verify unmaterialized paths while anchor checkout differs from indexed generation"
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
                "worktree source tree differs from the indexed anchor generation; rebuild or sync overlay"
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

fn rebase_cached_extraction(
    mut ext: devmap_extract::Extraction,
    rel_path: &str,
) -> devmap_extract::Extraction {
    let old_path = std::mem::replace(&mut ext.file_path, rel_path.to_string());
    let old_prefix = format!("{old_path}::");
    let new_file_name = rel_path.rsplit('/').next().unwrap_or(rel_path).to_string();
    for sym in &mut ext.symbols {
        if matches!(sym.kind, devmap_extract::SymbolKind::File) {
            sym.name = new_file_name.clone();
            sym.qualified_name = rel_path.to_string();
        } else if let Some(rest) = sym.qualified_name.strip_prefix(&old_prefix) {
            sym.qualified_name = format!("{rel_path}::{rest}");
        }
    }
    devmap_extract::secret::withhold_secret_values(&mut ext);
    ext.for_durable_store()
}

fn extract_fallback_ast(rel_path: &str, source: &str) -> (devmap_extract::Extraction, bool) {
    let lang = devmap_extract::detect_language(Path::new(rel_path));
    let declarative = devmap_extract::fallback::applies_to(lang);
    let scan = if declarative {
        devmap_extract::fallback::scan_declarations_in(lang, rel_path, source)
    } else {
        devmap_extract::fallback::FallbackScan {
            symbols: Vec::new(),
            references: Vec::new(),
            calls: Vec::new(),
            truncated: 0,
            truncated_calls: 0,
            skipped_long_lines: 0,
            unread_bytes: 0,
        }
    };
    let recovered_count = scan.symbols.len();
    let recovered = recovered_count > 0 || devmap_extract::fallback::has_dedicated_reader(lang);
    let mut symbols = Vec::with_capacity(scan.symbols.len() + 1);
    symbols.push(devmap_extract::ExtractedSymbol {
        name: rel_path.rsplit('/').next().unwrap_or(rel_path).to_string(),
        qualified_name: rel_path.to_string(),
        kind: devmap_extract::SymbolKind::File,
        span: devmap_extract::Span {
            start_byte: 0,
            end_byte: source.len(),
        },
        is_exported: true,
        docstring: None,
        signature: None,
        parent_symbol: None,
        body_signature: None,
        declaration_hash: None,
        return_type: None,
    });
    symbols.extend(scan.symbols);

    let mut ext = devmap_extract::Extraction {
        file_path: rel_path.to_string(),
        language: lang.to_string(),
        content_hash: devmap_extract::content_hash(source),
        engine: match (recovered, declarative) {
            (true, _) => devmap_extract::ExtractionEngine::RegexFallback {
                requested_language: lang.to_string(),
            },
            (false, true) => devmap_extract::ExtractionEngine::Unavailable {
                requested_language: lang.to_string(),
            },
            (false, false) => devmap_extract::ExtractionEngine::NotApplicable {
                language: lang.to_string(),
            },
        },
        parse_outcome: match (recovered, declarative) {
            (true, _) => devmap_extract::ParseOutcome::Fallback {
                reason: format!(
                    "worktree overlay declaration scan for {lang}; {recovered_count} declaration(s) recovered"
                ),
            },
            (false, true) => devmap_extract::ParseOutcome::Failed {
                reason: format!("no declarations recovered in worktree overlay for {lang}"),
            },
            (false, false) => devmap_extract::ParseOutcome::Failed {
                reason: format!("{lang} is a prose/data format; indexed as a File node"),
            },
        },
        symbols,
        imports: Vec::new(),
        calls: scan.calls,
        exports: Vec::new(),
        references: scan.references,
        diagnostics: Vec::new(),
        routes: Vec::new(),
        wiring: devmap_extract::wiring::extract_wiring_annotations(rel_path, source),
        go_package: None,
        jvm_package: None,
        scope_receivers: Vec::new(),
        swift_type_aliases: Vec::new(),
        go_build_constrained: false,
        go_interface_methods: Vec::new(),
        go_method_params: Vec::new(),
        go_member_names: None,
        go_facts: None,
        scope_locals: Vec::new(),
        rust_inline_modules: Vec::new(),
        rust_deref_targets: Vec::new(),
        rust_blanket_impl_params: Vec::new(),
        rust_implicit_calls: Vec::new(),
        rust_try_conversions: Vec::new(),
        local_bindings: Vec::new(),
        source_code: None,
        literals: Vec::new(),
        js_objects: Vec::new(),
        jvm_decls: Vec::new(),
        package_scopes: Vec::new(),
        local_extents: Vec::new(),
        decl_gaps: Vec::new(),
        recovered_decl_ranges: Vec::new(),
        dynamic_dispatch: Vec::new(),
    };
    devmap_extract::secret::withhold_secret_values(&mut ext);
    (ext.for_durable_store(), declarative)
}

pub(super) fn has_zero_copy_or_stack_lease(repo: &Path) -> bool {
    zero_copy_base_ref_path(&repo.to_string_lossy()).is_file()
        || devmap_extract::paths::stack_lease_path(repo).is_file()
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

/// Proves whether `candidate` (`affected_tests` computed from `anchor_store`) is
/// graph-theoretically invariant under `worktree`'s dirty deltas (`F4`).
///
/// Returns `true` ONLY when:
/// 1. `candidate` is available, complete (`!fail_closed`), and has zero omitted
///    blast-radius layer nodes (`nodes_omitted == 0` on every layer).
/// 2. Every difference between `worktree` and `anchor_store`'s indexed generation
///    is a bounded (`<= MAX_OVERLAY_DIRTY_FILES`) set of modified/added files
///    with zero deleted files and unchanged discovery refusals / over-limit sets.
/// 3. Every modified/added file is disjoint from `targets`, `seeds`, `layers`,
///    and `tests` (both by path and by indexed symbols in `anchor_store`), AND
///    for declarative code files, its UTF-8 comment-stripped source contains
///    zero wildcard re-exports/imports and zero identifier tokens matching any
///    target, seed, blast-layer symbol, test symbol, or cone file stem.
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
                // Conservative: any deleted indexed file forces fail_closed.
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

/// Opens the mmap [`devmap_store::CsrGraphReader`] for `repo`, falling back to
/// a verified zero-copy anchor base ref when `repo` holds `base_store_ref.json`
/// or `stack_lease.json` instead of a full local `devmap.sqlite`.
pub(crate) fn open_worktree_or_anchor_csr(
    repo: &Path,
) -> Option<(devmap_store::CsrGraphReader, u64)> {
    let prefer_zero_copy = is_sparse_worktree(repo) && has_zero_copy_or_stack_lease(repo);
    if !prefer_zero_copy {
        if let Ok(store) = super::open_local_indexed_store(repo) {
            if let Ok(Some(reader)) = store.open_latest_csr_sidecar() {
                let gen = reader.generation();
                return Some((reader, gen));
            }
        }
    }
    if let Ok(Some((anchor_store, base_ref))) = open_verified_zero_copy_anchor_store(repo) {
        if let Some(csr_path) = base_ref.anchor_csr_path.as_deref() {
            if let Ok(reader) = devmap_store::CsrGraphReader::open_mmap(Path::new(csr_path)) {
                let gen = reader.generation();
                return Some((reader, gen));
            }
        }
        if let Ok(Some(reader)) = anchor_store.open_latest_csr_sidecar() {
            let gen = reader.generation();
            return Some((reader, gen));
        }
    }
    if prefer_zero_copy {
        if let Ok(store) = super::open_local_indexed_store(repo) {
            if let Ok(Some(reader)) = store.open_latest_csr_sidecar() {
                let gen = reader.generation();
                return Some((reader, gen));
            }
        }
    }
    None
}

/// Synchronizes and atomically persists `<state_dir>/codeintel/overlay.json`
/// for `repo_path`, hashing dirty/untracked files with Blake3, marking deleted
/// files and their indexed symbols with [`CAS_TOMBSTONE`], reusing any matching
/// [`CasAstStore`] AST blobs, and populating `dirty_nodes` (`RoaringBitmap`).
pub fn sync_worktree_overlay(repo_path: &str) -> Result<WorktreeOverlayReport, CommandError> {
    let repo = validate_repo(repo_path)?;
    sync_worktree_overlay_internal(&repo, None)
}

fn sync_worktree_overlay_internal(
    repo: &Path,
    generation_hint: Option<u32>,
) -> Result<WorktreeOverlayReport, CommandError> {
    let start = Instant::now();
    let state_dir = devmap_query::paths::state_dir(repo);
    if let Ok(rel) = state_dir.strip_prefix(repo) {
        if let Some(dir_name) = plain_single_dir_component(rel) {
            let exclude = ensure_dir_excluded(repo, dir_name, DEVMAP_EXCLUDE_MARKER);
            if !exclude.is_clean() {
                return Err(CommandError::from(format!(
                    "refusing to write worktree overlay into {} because {dir_name}/ is not git-ignored: {exclude:?}",
                    repo.display()
                )));
            }
        }
    }

    let prefer_zero_copy = is_sparse_worktree(repo) && has_zero_copy_or_stack_lease(repo);
    let zero_copy_anchor = if prefer_zero_copy {
        open_verified_zero_copy_anchor_store(repo)?
    } else {
        None
    };
    let local_store = if zero_copy_anchor.is_none() {
        super::open_local_indexed_store(repo).ok()
    } else {
        None
    };
    let zero_copy_anchor = match zero_copy_anchor {
        Some(zc) => Some(zc),
        None if local_store.is_none() => open_verified_zero_copy_anchor_store(repo)?,
        None => None,
    };
    let active_store: Option<&Store> = zero_copy_anchor
        .as_ref()
        .map(|(s, _)| s)
        .or(local_store.as_ref());

    let generation_id = active_store
        .and_then(|s| s.latest_generation_id().ok().flatten())
        .or_else(|| zero_copy_anchor.as_ref().map(|(_, r)| r.generation_id))
        .or(generation_hint)
        .unwrap_or(0);
    let base_head_sha = active_store
        .and_then(|s| s.latest_generation_head_sha().ok().flatten())
        .or_else(|| {
            zero_copy_anchor
                .as_ref()
                .and_then(|(_, r)| r.base_head_sha.clone())
        });
    let worktree_id = repo
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("worktree")
        .to_string();
    let mut overlay = WorktreeOverlay::new(worktree_id, u64::from(generation_id));
    let cas_store = CasAstStore::with_custom_dir(cas_ast_dir_for_state(&state_dir));
    // `anchor_cas_store` is strictly READ-ONLY so `RegexFallback` overlay extractions
    // never overwrite or poison full tree-sitter AST blobs in the anchor (`F4`).
    let anchor_cas_store = zero_copy_anchor
        .as_ref()
        .and_then(|(_, r)| r.anchor_cas_dir.as_deref())
        .map(|p| CasAstStore::with_custom_dir(PathBuf::from(p)));
    let shared_overlay_cas_store = shared_overlay_cas_dir(repo).map(CasAstStore::with_custom_dir);

    let (deltas, truncated) = collect_worktree_deltas(repo, base_head_sha.as_deref())?;
    let csr_lookup = if deltas.is_empty() {
        None
    } else {
        active_store
            .and_then(|s| s.open_latest_csr_sidecar().ok().flatten())
            .map(|reader| CsrDigestLookup::from_reader(&reader))
    };
    let mut skipped_large_files = 0usize;
    let mut reused_cas_asts = 0usize;

    for (rel_path, kind) in deltas {
        let indexed_rows = active_store
            .and_then(|s| s.latest_symbols_for_file(&rel_path).ok().flatten())
            .map(|page| page.rows)
            .unwrap_or_default();
        let indexed_symbols: Vec<String> = indexed_rows
            .iter()
            .map(|r| r.qualified_name.clone())
            .collect();

        let node_ids = csr_lookup
            .as_ref()
            .map(|lookup| lookup.matching_nodes(&rel_path, &indexed_symbols))
            .unwrap_or_default();
        let first_node = node_ids.iter().next().copied();

        match kind {
            DeltaKind::Deleted => {
                overlay.mark_file_tombstone(&rel_path, first_node);
                for row in &indexed_rows {
                    overlay
                        .mark_symbol_tombstone(file_scoped_symbol_id(&rel_path, &row.name), None);
                    overlay.mark_symbol_tombstone(
                        file_scoped_symbol_id(&rel_path, &row.qualified_name),
                        None,
                    );
                }
                for nid in node_ids {
                    overlay.dirty_nodes.insert(nid);
                }
            }
            DeltaKind::Modified => {
                let full_path = repo.join(&rel_path);
                match read_bounded_file(&full_path, MAX_OVERLAY_FILE_BYTES)? {
                    Some(bytes) => {
                        let hash = blake3_hash(&bytes);
                        overlay.record_modified_file(&rel_path, &hash, first_node);
                        for nid in &node_ids {
                            overlay.dirty_nodes.insert(*nid);
                        }
                        let lang = devmap_extract::detect_language(Path::new(&rel_path));
                        let cached_ast = cas_store
                            .load_ast(&hash)
                            .ok()
                            .flatten()
                            .filter(|ext| ext.language == lang)
                            .or_else(|| {
                                anchor_cas_store
                                    .as_ref()
                                    .and_then(|ac| ac.load_ast(&hash).ok().flatten())
                                    .filter(|ext| ext.language == lang)
                            })
                            .or_else(|| {
                                shared_overlay_cas_store
                                    .as_ref()
                                    .and_then(|sc| sc.load_ast(&hash).ok().flatten())
                                    .filter(|ext| ext.language == lang)
                            });
                        let source_str_opt = std::str::from_utf8(&bytes).ok();
                        let extracted = if let Some(cached) = cached_ast {
                            reused_cas_asts += 1;
                            Some((
                                rebase_cached_extraction(cached, &rel_path),
                                devmap_extract::fallback::applies_to(lang),
                            ))
                        } else if let Some(source_str) = source_str_opt {
                            let (ext, declarative) = extract_fallback_ast(&rel_path, source_str);
                            if local_store.is_some() {
                                let _ = cas_store.store_ast(&bytes, &ext);
                            }
                            if let Some(ref sc) = shared_overlay_cas_store {
                                let _ = sc.store_ast(&bytes, &ext);
                            }
                            Some((ext, declarative))
                        } else {
                            None
                        };
                        if let Some((extraction, declarative)) = extracted {
                            if declarative {
                                let is_full_ast = matches!(
                                    extraction.engine,
                                    devmap_extract::ExtractionEngine::TreeSitter { .. }
                                );
                                let code_no_comments = if !is_full_ast && !indexed_rows.is_empty() {
                                    source_str_opt.map(strip_comments_preserving_identifiers)
                                } else {
                                    None
                                };
                                let declared_names: std::collections::HashSet<&str> = extraction
                                    .symbols
                                    .iter()
                                    .filter(|s| !matches!(s.kind, devmap_extract::SymbolKind::File))
                                    .map(|s| s.name.as_str())
                                    .collect();
                                for row in &indexed_rows {
                                    if row.kind.eq_ignore_ascii_case("file")
                                        || declared_names.contains(row.name.as_str())
                                    {
                                        continue;
                                    }
                                    let safe_to_tombstone = if is_full_ast {
                                        true
                                    } else {
                                        fallback_supports_symbol_kind(&row.kind)
                                            && code_no_comments.as_deref().is_some_and(|cleaned| {
                                                !contains_identifier_token(cleaned, &row.name)
                                            })
                                    };
                                    if safe_to_tombstone {
                                        overlay.mark_symbol_tombstone(
                                            file_scoped_symbol_id(&rel_path, &row.name),
                                            None,
                                        );
                                        overlay.mark_symbol_tombstone(
                                            file_scoped_symbol_id(&rel_path, &row.qualified_name),
                                            None,
                                        );
                                    }
                                }
                            }
                            let anchor_nid = first_node.unwrap_or(0);
                            overlay.record_extraction(&rel_path, extraction, anchor_nid);
                        }
                    }
                    None => {
                        if !full_path.exists() {
                            overlay.mark_file_tombstone(&rel_path, first_node);
                            for row in &indexed_rows {
                                overlay.mark_symbol_tombstone(
                                    file_scoped_symbol_id(&rel_path, &row.name),
                                    None,
                                );
                                overlay.mark_symbol_tombstone(
                                    file_scoped_symbol_id(&rel_path, &row.qualified_name),
                                    None,
                                );
                            }
                        } else {
                            skipped_large_files += 1;
                        }
                        for nid in node_ids {
                            overlay.dirty_nodes.insert(nid);
                        }
                    }
                }
            }
        }
    }

    let overlay_path = OVERLAY_RELPATH
        .split('/')
        .fold(state_dir, |acc, part| acc.join(part));
    if let Some(parent) = overlay_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| CommandError::from(format!("cannot create {}: {e}", parent.display())))?;
    }
    let encoded = serde_json::to_vec_pretty(&overlay)
        .map_err(|e| CommandError::from(format!("cannot serialize WorktreeOverlay: {e}")))?;
    devmap_query::write_atomic(&overlay_path, &encoded)
        .map_err(|e| CommandError::from(format!("cannot write {}: {e}", overlay_path.display())))?;

    let modified_count = overlay
        .modified_files
        .values()
        .filter(|v| v.as_str() != CAS_TOMBSTONE)
        .count();

    Ok(WorktreeOverlayReport {
        overlay_path: overlay_path.to_string_lossy().into_owned(),
        generation_id: overlay.generation,
        base_head_sha,
        modified_files: modified_count,
        tombstoned_files: overlay.tombstoned_files.len(),
        tombstoned_symbols: overlay.tombstoned_symbols.len(),
        dirty_nodes: overlay.dirty_nodes.len(),
        reused_cas_asts,
        skipped_large_files,
        truncated,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}

/// Reads the persisted [`WorktreeOverlay`] for `repo_path` when one exists.
pub fn load_worktree_overlay(repo_path: &str) -> Result<Option<WorktreeOverlay>, CommandError> {
    let repo = validate_repo(repo_path)?;
    let state_dir = devmap_query::paths::state_dir(&repo);
    let overlay_path = OVERLAY_RELPATH
        .split('/')
        .fold(state_dir, |acc, part| acc.join(part));
    if !overlay_path.is_file() {
        return Ok(None);
    }
    let bytes = read_bounded_file(&overlay_path, 32 * 1024 * 1024)?.ok_or_else(|| {
        CommandError::from(format!(
            "worktree overlay {} exceeds size bound or is not a regular file",
            overlay_path.display()
        ))
    })?;
    let overlay: WorktreeOverlay = serde_json::from_slice(&bytes).map_err(|e| {
        CommandError::from(format!(
            "failed to parse worktree overlay {}: {e}",
            overlay_path.display()
        ))
    })?;
    Ok(Some(overlay))
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
/// or writing `overlay.json` / CAS AST blobs. Used by read-only radar scans (`readOnlyHint: true`).
///
/// The result says whether the delta list was capped ([`DirtyNodeScan::truncated`]); a
/// persisted overlay that cannot be read, or whose generation differs from the CSR being
/// intersected, is never merged silently (the first is an error, the second is discarded
/// because the live delta below supersedes it).
pub(crate) fn compute_worktree_dirty_nodes_readonly(
    repo_path: &str,
    fallback_anchor: Option<&Path>,
) -> Result<DirtyNodeScan, CommandError> {
    let repo = validate_repo(repo_path)?;
    let prefer_zero_copy = is_sparse_worktree(&repo) && has_zero_copy_or_stack_lease(&repo);
    let zero_copy_anchor = if prefer_zero_copy {
        open_verified_zero_copy_anchor_store(&repo)?
    } else {
        None
    };
    let local_store = if zero_copy_anchor.is_none() {
        super::open_local_indexed_store(&repo).ok()
    } else {
        None
    };
    let zero_copy_anchor = match zero_copy_anchor {
        Some(zc) => Some(zc),
        None if local_store.is_none() => open_verified_zero_copy_anchor_store(&repo)?,
        None => None,
    };
    let fallback_store = if local_store.is_none() && zero_copy_anchor.is_none() {
        fallback_anchor.and_then(|a| super::open_local_indexed_store(a).ok())
    } else {
        None
    };

    let active_store: Option<&Store> = zero_copy_anchor
        .as_ref()
        .map(|(s, _)| s)
        .or(local_store.as_ref())
        .or(fallback_store.as_ref());

    // Node IDs are only comparable within one CSR generation; a persisted overlay is
    // merged below only after its generation is checked against the CSR in hand.
    let persisted = load_worktree_overlay(repo_path)?;

    let Some(store) = active_store else {
        return Ok(DirtyNodeScan::default());
    };
    let Some(csr_reader) = store.open_latest_csr_sidecar().ok().flatten() else {
        return Ok(DirtyNodeScan::default());
    };
    let mut dirty = match persisted {
        Some(existing) if existing.generation == csr_reader.generation() => existing.dirty_nodes,
        _ => devmap_query::RoaringBitmap::new(),
    };
    let csr_lookup = CsrDigestLookup::from_reader(&csr_reader);
    let base_head_sha = store
        .latest_generation_head_sha()
        .ok()
        .flatten()
        .or_else(|| {
            zero_copy_anchor
                .as_ref()
                .and_then(|(_, r)| r.base_head_sha.clone())
        });

    let (deltas, truncated) = collect_worktree_deltas(&repo, base_head_sha.as_deref())?;
    for (rel_path, _kind) in deltas {
        let indexed_symbols: Vec<String> = store
            .latest_symbols_for_file(&rel_path)
            .ok()
            .flatten()
            .map(|page| page.rows.into_iter().map(|r| r.qualified_name).collect())
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
    fn provision_worktree_devmap_clones_and_rebinds_store_keeping_git_status_clean() {
        let anchor_dir = crate::codeintel::tests::repo_with_one_generation();
        let anchor = anchor_dir.path();
        crate::test_support::trust_repo(anchor);

        // Seed a repo_map.json and CAS AST artifact in the anchor so we prove
        // sidecar and CAS CoW seeding in the same pass.
        let anchor_map = devmap_query::paths::repo_map_path(anchor);
        fs::write(&anchor_map, r#"{"schema_version":1,"files":[]}"#).unwrap();

        let anchor_state = devmap_query::paths::state_dir(anchor);
        let anchor_cas = CasAstStore::with_custom_dir(cas_ast_dir_for_state(&anchor_state));
        let sample_bytes = b"pub fn shared_across_lanes() -> usize { 42 }\n";
        let sample_ext: devmap_extract::model::Extraction = serde_json::from_str(
            r#"{"file_path":"src/shared.rs","language":"rust","content_hash":77,"parse_outcome":"Clean","symbols":[],"imports":[],"calls":[]}"#,
        )
        .unwrap();
        let (cas_hash, _, _) = anchor_cas.store_ast(sample_bytes, &sample_ext).unwrap();

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
        assert!(report.cas_seeded, "{report:?}");
        assert!(report.generation_id.is_some());

        // `nlink` must be 1 so `Store::validate_database_file` accepts it.
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

        // Codeintel status and CSR graph must open cleanly in the new worktree
        // without running `devmap build`!
        let wt_str = wt.to_str().unwrap();
        let status = crate::codeintel::status(wt_str);
        assert!(status.available, "worktree store should open: {status:?}");
        assert_eq!(status.generation_id, report.generation_id);
        let csr = crate::codeintel::open_repo_csr_graph(wt_str).expect("CSR graph");
        assert_eq!(csr.generation(), u64::from(report.generation_id.unwrap()));

        // Git status in the worktree must be 100% clean (excluded via info/exclude).
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

        // Now write the CAS-matching file + delete any tracked file and sync overlay.
        fs::create_dir_all(wt.join("src")).unwrap();
        fs::write(wt.join("src/shared.rs"), sample_bytes).unwrap();
        let sync = sync_worktree_overlay(wt_str).expect("sync overlay");
        assert_eq!(sync.modified_files, 1);
        assert_eq!(sync.reused_cas_asts, 1);

        let loaded = load_worktree_overlay(wt_str)
            .expect("load overlay")
            .expect("overlay exists");
        assert_eq!(
            loaded.resolve_file(Path::new("src/shared.rs"), None),
            Some(cas_hash.as_str())
        );
        assert!(loaded.added_extractions.contains_key("src/shared.rs"));
    }

    #[test]
    fn provision_worktree_devmap_uses_online_backup_when_wal_is_non_empty() {
        let anchor_dir = crate::codeintel::tests::repo_with_one_generation();
        let anchor = anchor_dir.path();
        crate::test_support::trust_repo(anchor);

        // Hold a pinned reader on the older snapshot AND a subsequent WAL commit so
        // `wal_checkpoint(TRUNCATE)` cannot truncate the WAL and must fall back to
        // `VACUUM INTO` online backup.
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
            fs::metadata(&wal_path).map(|m| m.len() > 0).unwrap_or(false),
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
        let repo_dir = crate::codeintel::tests::repo_with_one_generation();
        let repo = repo_dir.path();
        crate::test_support::trust_repo(repo);
        crate::devmap::ensure_state_dir_excluded(repo);

        // Create and commit a tracked file, then delete it so git status sees deletion.
        fs::write(repo.join(" doomed.rs".trim()), "pub fn vanish() {}\n").unwrap();
        git(repo, &["add", "doomed.rs"]);
        git(
            repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "add doomed",
            ],
        );
        fs::remove_file(repo.join("doomed.rs")).unwrap();

        let report = sync_worktree_overlay(repo.to_str().unwrap()).expect("sync");
        assert!(report.tombstoned_files >= 1, "{report:?}");

        let overlay = load_worktree_overlay(repo.to_str().unwrap())
            .unwrap()
            .unwrap();
        assert!(overlay.is_file_tombstoned(Path::new("doomed.rs")));
        assert_eq!(
            overlay.resolve_file(Path::new("doomed.rs"), Some("b3_old")),
            None
        );
    }

    #[test]
    fn provision_worktree_devmap_zero_copy_shares_anchor_graph_without_copying_sqlite() {
        let anchor_dir = crate::codeintel::tests::repo_with_one_generation();
        let anchor = anchor_dir.path();
        crate::test_support::trust_repo(anchor);

        let anchor_state = devmap_query::paths::state_dir(anchor);
        let anchor_cas = CasAstStore::with_custom_dir(cas_ast_dir_for_state(&anchor_state));
        let sample_bytes = b"pub fn zero_copy_ast_shared() -> usize { 99 }\n";
        let sample_ext: devmap_extract::model::Extraction = serde_json::from_str(
            r#"{"file_path":"src/zc.rs","language":"rust","content_hash":99,"parse_outcome":"Clean","symbols":[],"imports":[],"calls":[]}"#,
        )
        .unwrap();
        let (cas_hash, _, _) = anchor_cas.store_ast(sample_bytes, &sample_ext).unwrap();

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
        assert!(report.cas_seeded);

        // Zero SQLite bytes copied into the worktree!
        assert!(
            !devmap_query::paths::store_path(&wt).exists(),
            "zero-copy worktree must NOT duplicate devmap.sqlite"
        );
        let wt_str = wt.to_str().unwrap();
        let loaded_ref = load_zero_copy_base_ref(wt_str)
            .expect("load base ref")
            .expect("base_store_ref.json exists");
        assert_eq!(loaded_ref.generation_id, report.generation_id.unwrap());

        let (csr, gen) = open_worktree_or_anchor_csr(&wt).expect("anchor CSR mmap");
        assert_eq!(gen, u64::from(loaded_ref.generation_id));
        assert!(csr.node_count() > 0);

        // Anchor CAS AST read-through works without copying cas/ast/!
        assert!(
            !cas_ast_dir_for_state(&devmap_query::paths::state_dir(&wt)).exists(),
            "zero-copy worktree must NOT duplicate cas/ast directory"
        );
        fs::create_dir_all(wt.join("src")).unwrap();
        fs::write(wt.join("src/zc.rs"), sample_bytes).unwrap();
        let sync = sync_worktree_overlay(wt_str).expect("sync zero-copy overlay");
        assert_eq!(sync.modified_files, 1);
        assert_eq!(sync.reused_cas_asts, 1);

        let overlay = load_worktree_overlay(wt_str).unwrap().unwrap();
        assert_eq!(
            overlay.resolve_file(Path::new("src/zc.rs"), None),
            Some(cas_hash.as_str())
        );
        assert!(overlay.added_extractions.contains_key("src/zc.rs"));
    }

    #[test]
    fn load_zero_copy_base_ref_refuses_cross_repository_pointer_spoofing() {
        let anchor_dir = crate::codeintel::tests::repo_with_one_generation();
        let anchor = anchor_dir.path();
        crate::test_support::trust_repo(anchor);

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

        let spoofed = ZeroCopyBaseRef {
            anchor_repo_root: anchor.to_string_lossy().into_owned(),
            anchor_db_path: devmap_query::paths::store_path(anchor)
                .to_string_lossy()
                .into_owned(),
            anchor_csr_path: None,
            anchor_cas_dir: None,
            generation_id: 1,
            base_head_sha: None,
        };
        let ref_path = zero_copy_base_ref_path(stranger.to_str().unwrap());
        fs::create_dir_all(ref_path.parent().unwrap()).unwrap();
        fs::write(&ref_path, serde_json::to_vec(&spoofed).unwrap()).unwrap();

        let err = load_zero_copy_base_ref(stranger.to_str().unwrap())
            .expect_err("must refuse cross-family pointer");
        assert!(
            err.message().contains("outside worktree family"),
            "unexpected error: {err:?}"
        );
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
        let mut head_out_cmd = Command::new("git");
        crate::test_support::pin_git(&mut head_out_cmd);
        let head_out = head_out_cmd
            .current_dir(anchor)
            .args(["rev-parse", "HEAD"])
            .output_locked()
            .unwrap();
        let head_sha = String::from_utf8_lossy(&head_out.stdout).trim().to_string();
        crate::test_support::trust_repo(anchor);
        crate::devmap::ensure_state_dir_excluded(anchor);

        let scanned = devmap_extract::scan_tree(anchor).unwrap();
        let hashes: std::collections::BTreeMap<String, u64> = scanned
            .file_hashes()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let (grammar_ver, analyzer_ver) = devmap_extract::cache::current_payload_identity("rust");

        let db = devmap_query::paths::store_path(anchor);
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = Store::open(&db).unwrap();
        store.bind_repo_root(anchor).unwrap();
        drop(store);

        let root_str = anchor.to_string_lossy().into_owned();
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
            let content_hash = *hashes.get(*path).expect("scanned hash") as i64;
            conn.execute("INSERT INTO paths (id, path) VALUES (?1, ?2)", (id, *path))
                .unwrap();
            let engine_json =
                serde_json::to_string(&devmap_extract::ExtractionEngine::TreeSitter {
                    grammar: "rust".to_string(),
                    grammar_version: grammar_ver.parse::<u32>().unwrap_or(1),
                })
                .unwrap();
            conn.execute(
                "INSERT INTO file_payloads
                   (payload_id, file_id, content_hash, language,
                    parse_outcome_json, engine_json, extraction_json,
                    entry_wiring_json, grammar_version, analyzer_version)
                 VALUES (?1, ?1, ?2, 'rust', '\"Clean\"', ?3, '{}', '[]', ?4, ?5)",
                (id, content_hash, &engine_json, &grammar_ver, &analyzer_ver),
            )
            .unwrap();
            conn.execute(
                "INSERT INTO file_rows (file_id, payload_id, valid_from, valid_to)
                 VALUES (?1, ?1, 1, NULL)",
                [id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO payload_nodes
                   (payload_id, ord_in_file, name, qualified_name, kind,
                    span_start, span_end, is_exported)
                 VALUES (?1, 0, ?2, ?2, 'Function', 0, ?3, 1)",
                (id, *sym, *span_end),
            )
            .unwrap();
            conn.execute(
                "INSERT INTO nodes_fts (rowid, name, qualified_name, path)
                 VALUES (?1, ?2, ?2, ?3)",
                (id << 24, *sym, *path),
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

        (dir, head_sha)
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

        // 1. Every CodeIntel surface works against the zero-copy worktree!
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

        // 2. Mutating ONLY the zero-copy worktree (while anchor stays clean!) MUST mark the
        //    worktree stale, fail-close `affected_tests`, and withhold stale anchor `source_span`!
        fs::write(
            wt.join("src/caller.rs"),
            "// shifted line 1\n// shifted line 2\npub fn probe_caller() { probe_callee(); }\n",
        )
        .unwrap();

        let dirty_st = crate::codeintel::status(wt_str);
        assert_eq!(
            dirty_st.source_freshness,
            Some(false),
            "dirty zero-copy worktree must NOT launder anchor's clean source_freshness: {dirty_st:?}"
        );
        assert_eq!(dirty_st.is_fresh, Some(false));

        let dirty_selected = crate::codeintel::affected_tests(
            wt_str,
            &["probe_callee".to_string()],
            None,
            crate::codeintel::WalkBounds::default(),
        );
        assert!(
            dirty_selected.fail_closed,
            "dirty zero-copy worktree MUST fail closed on affected_tests: {dirty_selected:?}"
        );

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
    fn search_surfaces_an_unreadable_overlay_instead_of_reading_it_as_absent() {
        let (anchor_dir, _) = fixture_with_csr();
        let anchor = anchor_dir.path();
        let overlay_path = OVERLAY_RELPATH
            .split('/')
            .fold(devmap_query::paths::state_dir(anchor), |acc, part| {
                acc.join(part)
            });
        fs::create_dir_all(overlay_path.parent().unwrap()).unwrap();
        fs::write(&overlay_path, b"{ not json").unwrap();
        let hits = crate::codeintel::search(anchor.to_str().unwrap(), "probe_caller", None);
        assert!(
            hits.overlay_note
                .as_deref()
                .is_some_and(|n| n.contains("unreadable")),
            "a corrupt overlay must be reported: {hits:?}"
        );
    }

    #[test]
    fn overlay_search_hits_are_capped_and_the_cap_sets_truncated() {
        let (anchor_dir, _head_sha) = seed_fresh_anchor_for_zero_copy();
        let anchor = anchor_dir.path();
        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("wt-cap");
        git(
            anchor,
            &["worktree", "add", "-b", "lane/cap", wt.to_str().unwrap()],
        );
        crate::test_support::trust_repo(&wt);
        provision_worktree_devmap_zero_copy(anchor, &wt).unwrap();
        let src: String = (0..80)
            .map(|i| format!("pub fn overlay_cap_probe_{i}() -> usize {{ {i} }}\n"))
            .collect();
        fs::write(wt.join("src/many.rs"), src).unwrap();
        let wt_str = wt.to_str().unwrap();
        sync_worktree_overlay(wt_str).expect("sync");

        let hits = crate::codeintel::search(wt_str, "overlay_cap_probe_", Some(100_000));
        let overlay_hits = hits
            .items
            .iter()
            .filter(|h| h.file_path == "src/many.rs")
            .count();
        assert!(overlay_hits > 0, "premise: overlay symbols found: {hits:?}");
        assert!(
            overlay_hits <= 50,
            "overlay hits must be bounded, got {overlay_hits}"
        );
        assert!(hits.truncated, "a capped overlay search must say truncated");
        assert!(hits.walk_incomplete.is_none(), "{:?}", hits.walk_incomplete);
    }

    #[test]
    fn sync_worktree_overlay_extracts_fallback_declarations_tombstones_removed_symbols_and_shares_cas(
    ) {
        let (anchor_dir, _head_sha) = seed_fresh_anchor_for_zero_copy();
        let anchor = anchor_dir.path();

        let wt_parent = tempfile::tempdir().unwrap();
        let wt_a = wt_parent.path().join("wt-lane-a");
        let wt_b = wt_parent.path().join("wt-lane-b");
        git(
            anchor,
            &[
                "worktree",
                "add",
                "-b",
                "lane/extract-a",
                wt_a.to_str().unwrap(),
            ],
        );
        git(
            anchor,
            &[
                "worktree",
                "add",
                "-b",
                "lane/extract-b",
                wt_b.to_str().unwrap(),
            ],
        );
        crate::test_support::trust_repo(&wt_a);
        crate::test_support::trust_repo(&wt_b);
        provision_worktree_devmap_zero_copy(anchor, &wt_a).unwrap();
        provision_worktree_devmap_zero_copy(anchor, &wt_b).unwrap();

        let new_file_src = "pub fn brand_new_lane_helper() -> usize { 42 }\n";
        fs::write(wt_a.join("src/new_feature.rs"), new_file_src).unwrap();
        // Replace `probe_caller` with `replacement_caller` in `src/caller.rs`
        fs::write(
            wt_a.join("src/caller.rs"),
            "pub fn replacement_caller() { probe_callee(); }\n",
        )
        .unwrap();

        let wt_a_str = wt_a.to_str().unwrap();
        let sync_a = sync_worktree_overlay(wt_a_str).expect("sync_a");
        assert_eq!(sync_a.modified_files, 2);

        let overlay_a = load_worktree_overlay(wt_a_str).unwrap().unwrap();
        let new_ext = overlay_a
            .added_extractions
            .get("src/new_feature.rs")
            .expect("fallback declaration extraction must populate added_extractions");
        assert!(
            new_ext
                .symbols
                .iter()
                .any(|s| s.name == "brand_new_lane_helper"),
            "expected brand_new_lane_helper in {new_ext:?}"
        );
        assert!(
            overlay_a
                .tombstoned_symbols
                .contains("src/caller.rs::probe_caller"),
            "removed symbol probe_caller must be tombstoned: {:?}",
            overlay_a.tombstoned_symbols
        );

        // `codeintel::search` surfaces uncommitted `brand_new_lane_helper` with `walk_incomplete`
        // and filters out tombstoned `probe_caller`!
        let new_hits = crate::codeintel::search(wt_a_str, "brand_new_lane_helper", None);
        assert!(new_hits.available);
        assert!(
            new_hits
                .items
                .iter()
                .any(|h| h.symbol_name == "brand_new_lane_helper"
                    && h.file_path == "src/new_feature.rs"),
            "expected overlay symbol in search results: {new_hits:?}"
        );
        assert!(
            new_hits
                .overlay_note
                .as_deref()
                .is_some_and(|n| n.contains("overlay symbols")),
            "fallback overlay symbol search must disclose itself in overlay_note: {new_hits:?}"
        );
        assert!(
            new_hits.walk_incomplete.is_none(),
            "overlay provenance is not an early stop: {:?}",
            new_hits.walk_incomplete
        );

        let old_hits = crate::codeintel::search(wt_a_str, "probe_caller", None);
        assert!(
            !old_hits
                .items
                .iter()
                .any(|h| h.symbol_name == "probe_caller"),
            "tombstoned symbol probe_caller must be filtered from worktree search: {old_hits:?}"
        );

        // Sibling zero-copy lane `wt_b` writing identical `src/new_feature.rs` hits shared anchor CAS!
        fs::write(wt_b.join("src/new_feature.rs"), new_file_src).unwrap();
        let sync_b = sync_worktree_overlay(wt_b.to_str().unwrap()).expect("sync_b");
        assert_eq!(
            sync_b.reused_cas_asts, 1,
            "sibling zero-copy lane must reuse CAS AST written by lane A via anchor CAS"
        );
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

        // Pre-existing untracked fixture files count toward the cap; fill to it exactly.
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
    fn readonly_dirty_scan_ignores_a_persisted_overlay_from_another_csr_generation() {
        let (anchor_dir, generation) = fixture_with_csr();
        let anchor = anchor_dir.path();
        let anchor_str = anchor.to_str().unwrap();
        let overlay_path = OVERLAY_RELPATH
            .split('/')
            .fold(devmap_query::paths::state_dir(anchor), |acc, part| {
                acc.join(part)
            });
        fs::create_dir_all(overlay_path.parent().unwrap()).unwrap();
        let write_overlay = |overlay_generation: u64| {
            let mut overlay = WorktreeOverlay::new("fixture".to_string(), overlay_generation);
            overlay.dirty_nodes.insert(u32::MAX);
            fs::write(&overlay_path, serde_json::to_vec(&overlay).unwrap()).unwrap();
        };

        write_overlay(generation);
        let same = compute_worktree_dirty_nodes_readonly(anchor_str, None).unwrap();
        assert!(
            same.dirty_nodes.contains(u32::MAX),
            "an overlay from the CSR's own generation is merged"
        );

        write_overlay(generation + 7);
        let stale = compute_worktree_dirty_nodes_readonly(anchor_str, None).unwrap();
        assert!(
            !stale.dirty_nodes.contains(u32::MAX),
            "node IDs from generation {} must not be intersected with generation {generation}",
            generation + 7
        );
    }

    #[test]
    fn readonly_dirty_scan_fails_loudly_on_an_unreadable_persisted_overlay() {
        let (anchor_dir, _) = fixture_with_csr();
        let anchor = anchor_dir.path();
        let overlay_path = OVERLAY_RELPATH
            .split('/')
            .fold(devmap_query::paths::state_dir(anchor), |acc, part| {
                acc.join(part)
            });
        fs::create_dir_all(overlay_path.parent().unwrap()).unwrap();
        fs::write(&overlay_path, b"{ not json").unwrap();
        assert!(
            compute_worktree_dirty_nodes_readonly(anchor.to_str().unwrap(), None).is_err(),
            "a corrupt overlay is a failed scan, not an empty one"
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
    fn provisioning_reports_why_an_overlay_sync_failed_on_the_already_seeded_path() {
        let anchor_dir = crate::codeintel::tests::repo_with_one_generation();
        let anchor = anchor_dir.path();
        crate::test_support::trust_repo(anchor);
        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("lane-seeded");
        git(
            anchor,
            &["worktree", "add", "-b", "lane/seeded", wt.to_str().unwrap()],
        );
        crate::test_support::trust_repo(&wt);
        let first = provision_worktree_devmap(anchor, &wt).expect("first provision");
        assert!(first.seeded, "{first:?}");

        // A directory where overlay.json must be written makes the sync fail.
        let overlay_path = OVERLAY_RELPATH
            .split('/')
            .fold(devmap_query::paths::state_dir(&wt), |acc, part| {
                acc.join(part)
            });
        let _ = fs::remove_file(&overlay_path);
        fs::create_dir_all(&overlay_path).unwrap();

        let second = provision_worktree_devmap(anchor, &wt).expect("second provision");
        assert!(second.seeded, "{second:?}");
        assert!(second.overlay_path.is_none(), "{second:?}");
        assert!(
            second
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("overlay sync failed")),
            "a dropped overlay error must be reported: {second:?}"
        );
    }

    #[test]
    fn provision_worktree_devmap_rebinds_via_seed_rebind_and_clones_resolution_reuse() {
        let anchor_dir = crate::codeintel::tests::repo_with_one_generation();
        let anchor = anchor_dir.path();
        crate::test_support::trust_repo(anchor);

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

        // 1. `resolution-reuse-1/` must be CoW-cloned into the worktree (`F9`).
        let wt_reuse_file =
            devmap_query::paths::state_dir(&wt).join("resolution-reuse-1/unit.bin");
        assert_eq!(
            fs::read(&wt_reuse_file).unwrap(),
            b"cached-resolution-artifact"
        );

        // 2. Both `seed_rebind` (`F8`) and the latest `generations` row bind `wt`,
        //    while stacked mode avoids rewriting `base.sqlite` entirely (`0 B`).
        let wt_db = devmap_query::paths::store_path(&wt);
        let conn = Connection::open(&wt_db).unwrap();
        let raw_gen_root: String = conn
            .query_row(
                "SELECT repo_root FROM generations ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let canonical_wt = fs::canonicalize(&wt).unwrap();
        assert_eq!(
            fs::canonicalize(&raw_gen_root).unwrap(),
            canonical_wt,
            "latest generation repo_root must bind the lane root"
        );
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
    fn stack_lease_only_worktree_opens_in_codeintel_and_zero_copy_seals_base_snapshot() {
        let (anchor_dir, _head_sha) = seed_fresh_anchor_for_zero_copy();
        let anchor = anchor_dir.path();

        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("wt-stack-lease-only");
        git(
            anchor,
            &[
                "worktree",
                "add",
                "-b",
                "lane/stack-lease-only",
                wt.to_str().unwrap(),
            ],
        );
        crate::test_support::trust_repo(&wt);

        let report = provision_worktree_devmap_zero_copy(anchor, &wt).expect("zero-copy");
        assert!(report.seeded, "{report:?}");

        let lease_path = devmap_extract::paths::stack_lease_path(&wt);
        assert!(
            lease_path.is_file(),
            "zero-copy provisioning must emit canonical stack_lease.json (`F1`)"
        );
        let lease = devmap_query::stacked::StackLease::read_from(&lease_path).unwrap();
        let base_sqlite = lease.base_sqlite_path();
        assert!(base_sqlite.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&base_sqlite).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o444, "sealed base snapshot must be read-only 0444");
        }

        // Remove `base_store_ref.json` so ONLY `stack_lease.json` remains (simulating a
        // worktree seeded directly via `devmap seed --mode stacked`).
        let wt_str = wt.to_str().unwrap();
        fs::remove_file(zero_copy_base_ref_path(wt_str)).unwrap();

        let st = crate::codeintel::status(wt_str);
        assert!(st.available, "stack_lease-only worktree must open: {st:?}");
        assert_eq!(st.is_fresh, Some(true), "{st:?}");

        let hits = crate::codeintel::search(wt_str, "probe_caller", None);
        assert!(hits.available);
        assert!(hits.items.iter().any(|h| h.symbol_name == "probe_caller"));
    }

    #[test]
    fn sparse_worktree_prefers_zero_copy_monorepo_base_over_cone_only_local_store() {
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
        assert!(
            is_sparse_worktree(&wt),
            "expected cone sparse-checkout worktree"
        );
        assert!(
            !wt.join("src/callee.rs").exists(),
            "out-of-cone src/callee.rs must not be materialized"
        );

        provision_worktree_devmap_zero_copy(anchor, &wt).expect("zero-copy provision");

        // Simulate a partial cone-only local `devmap.sqlite` created inside the sparse
        // worktree (`F2`) that contains 0 `probe_callee` rows.
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

        // `codeintel::search` MUST prefer the full monorepo zero-copy base over the
        // partial cone-only local `devmap.sqlite` (`F2`)!
        let wt_str = wt.to_str().unwrap();
        let hits = crate::codeintel::search(wt_str, "probe_callee", None);
        assert!(hits.available, "{hits:?}");
        assert!(
            hits.items.iter().any(|h| h.symbol_name == "probe_callee"),
            "sparse worktree must serve out-of-cone monorepo symbols: {hits:?}"
        );
    }

    #[test]
    fn overlay_sync_preserves_live_methods_and_consts_without_poisoning_anchor_cas() {
        let dir = tempfile::tempdir().unwrap();
        let anchor = dir.path();
        git(anchor, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(anchor.join("src")).unwrap();
        let rs_src = "pub const MAX_RETRIES: usize = 3;\npub fn keep_me() -> usize { MAX_RETRIES }\n";
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
        let head_out = {
            let mut c = Command::new("git");
            crate::test_support::pin_git(&mut c);
            c.current_dir(anchor)
                .args(["rev-parse", "HEAD"])
                .output_locked()
                .unwrap()
        };
        let head_sha = String::from_utf8_lossy(&head_out.stdout).trim().to_string();
        crate::test_support::trust_repo(anchor);
        crate::devmap::ensure_state_dir_excluded(anchor);

        let anchor_state = devmap_query::paths::state_dir(anchor);
        let anchor_cas_dir = cas_ast_dir_for_state(&anchor_state);
        fs::create_dir_all(&anchor_cas_dir).unwrap();

        let db = devmap_query::paths::store_path(anchor);
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = Store::open(&db).unwrap();
        store.bind_repo_root(anchor).unwrap();
        drop(store);

        let conn = Connection::open(&db).unwrap();
        conn.execute(
            "INSERT INTO generations (id, created_at, head_sha, analysis_json, repo_root)
             VALUES (1, 0.0, ?1, '{\"total_files\":3,\"total_symbols\":4,\"total_edges\":0,\"dead_symbols\":[],\"communities\":[],\"status\":\"Ok\",\"unresolved_calls\":0}', ?2)",
            (&head_sha, anchor.to_string_lossy().as_ref()),
        )
        .unwrap();
        let indexed = [
            (1i64, "src/lib.rs", "rust", "MAX_RETRIES", "Constant"),
            (2i64, "src/srv.go", "go", "HandleReq", "Method"),
            (3i64, "src/client.ts", "typescript", "API_URL", "Constant"),
        ];
        for (id, path, lang, sym, kind) in indexed {
            let (gv, av) = devmap_extract::cache::current_payload_identity(lang);
            conn.execute("INSERT INTO paths (id, path) VALUES (?1, ?2)", (id, path))
                .unwrap();
            conn.execute(
                "INSERT INTO file_payloads
                   (payload_id, file_id, content_hash, language, parse_outcome_json,
                    engine_json, extraction_json, entry_wiring_json, grammar_version, analyzer_version)
                 VALUES (?1, ?1, ?2, ?3, '\"Clean\"', '{\"TreeSitter\":{\"grammar\":\"x\",\"grammar_version\":1}}', '{}', '[]', ?4, ?5)",
                (id, id * 100, lang, &gv, &av),
            )
            .unwrap();
            conn.execute(
                "INSERT INTO file_rows (file_id, payload_id, valid_from, valid_to) VALUES (?1, ?1, 1, NULL)",
                [id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO payload_nodes (payload_id, ord_in_file, name, qualified_name, kind, span_start, span_end, is_exported)
                 VALUES (?1, 0, ?2, ?2, ?3, 0, 20, 1)",
                (id, sym, kind),
            )
            .unwrap();
        }
        drop(conn);

        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("wt-polyglot");
        git(
            anchor,
            &["worktree", "add", "-b", "lane/polyglot", wt.to_str().unwrap()],
        );
        crate::test_support::trust_repo(&wt);
        provision_worktree_devmap_zero_copy(anchor, &wt).expect("zero-copy");

        // Edit 1 comment line in each file while keeping `MAX_RETRIES`, `HandleReq`,
        // and `API_URL` intact (`F4`).
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

        // Anchor CAS directory must remain 100% untouched by RegexFallback ASTs (`F4`).
        let anchor_cas_entries = fs::read_dir(&anchor_cas_dir).unwrap().count();
        assert_eq!(
            anchor_cas_entries, 0,
            "sync_worktree_overlay must never write RegexFallback ASTs into anchor CAS"
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

        // 1. Unrelated prose + unrelated Rust file outside `probe_callee`'s cone MUST
        //    keep `affected_tests` open (`fail_closed == false`, `F4`)!
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

        // 2. Adding a new file that references `probe_callee` in code MUST immediately
        //    fail closed (`fail_closed == true`)!
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
}
