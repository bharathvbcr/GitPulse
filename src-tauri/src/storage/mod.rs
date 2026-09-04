//! Repository disk-usage scanner: what is taking up space, what is reclaimable,
//! and what is silently growing.
//!
//! The scan answers four questions in one pass:
//!
//! 1. **Git internals** — packfiles vs loose objects (authoritative numbers
//!    from `git count-objects -v`), reflogs, LFS, submodule `.git/modules`
//!    payloads, per-worktree admin directories, and the index.
//! 2. **Build & cache artifacts** — known directory names across ecosystems
//!    (`node_modules`, `target`, `__pycache__`, …), sized wherever they nest,
//!    each byte attributed to its nearest enclosing artifact scope.
//! 3. **Hygiene gaps** — an artifact directory *not* covered by ignore rules,
//!    or an ignored one that still has files committed to the index. Both are
//!    how "temporary" junk ends up in status noise or in history forever.
//! 4. **Large files** outside the git dir that dominate the working tree.
//!
//! Every walk is budgeted (deadline, file count, depth, entries per directory)
//! and never follows symlinks, so a hostile or enormous repository degrades
//! into an honest `truncated` report instead of a hang. Sub-scan failures
//! degrade into notes on the report; only an invalid repository fails the
//! command.

use crate::engine::git_cli::{git_text, resolve_git_common_dir, validate_repo};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

/// Soft deadline for one full scan. Hitting it stops expansion and flags
/// `scan.truncated` rather than presenting a partial total as complete.
const SCAN_DEADLINE: Duration = Duration::from_secs(20);
/// Maximum traversal depth below the scanned root.
const MAX_DEPTH: usize = 48;
/// Maximum regular files visited across the main worktree walk before truncating.
const MAX_FILES_WORKTREE: usize = 250_000;
/// Maximum regular files visited across the git directory walk before truncating.
const MAX_FILES_GIT: usize = 100_000;
/// Maximum regular files visited per linked worktree walk before truncating.
const MAX_FILES_PER_WORKTREE: usize = 100_000;
/// Maximum directory entries enumerated per directory in normal source trees
/// before truncating (defends against pathological flat directories).
const MAX_ENTRIES_PER_DIR: usize = 4_000;
/// Maximum directory entries enumerated per directory inside known build/cache
/// artifact trees and git directories. Large compilation trees (like Cargo deps)
/// easily exceed 4,000 files without being pathological.
const MAX_ENTRIES_PER_ARTIFACT_DIR: usize = 100_000;
/// Files at or above this size are large-file candidates.
const LARGE_FILE_THRESHOLD: u64 = 10 * 1024 * 1024;
/// Report at most this many large files.
const MAX_LARGE_FILES: usize = 25;
/// Report at most this many artifact directories (largest first).
const MAX_ARTIFACT_DIRS: usize = 64;
/// Size at most this many linked worktrees.
const MAX_SIZED_WORKTREES: usize = 8;
/// Sample at most this many stale branch names in the summary.
const MAX_BRANCH_SAMPLES: usize = 20;

/// A build-output or cache directory found in the working tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactDir {
    /// Path relative to the repository root, forward slashes.
    pub path: String,
    pub bytes: u64,
    /// Build output vs cache classification.
    pub kind: ArtifactKind,
    /// True when NO ignore rule covers this directory: it shows up as noise
    /// in every status listing and can be committed by accident.
    pub unignored: bool,
    /// Number of index-tracked files inside this directory. Committed cache
    /// content survives even a correct .gitignore — the classic history bloat.
    pub tracked_files: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Build,
    Cache,
}

/// One oversized file in the working tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LargeFile {
    pub path: String,
    pub bytes: u64,
}

/// Disk usage of one linked worktree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeUsage {
    pub path: String,
    pub name: String,
    pub branch: Option<String>,
    pub bytes: u64,
    pub truncated: bool,
}

/// Breakdown of the (common) git directory itself.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitStorage {
    /// Bytes held in packfiles (`size-pack` from count-objects, authoritative;
    /// KiB-rounded upward by git's own accounting).
    pub pack_bytes: u64,
    pub pack_file_count: u64,
    /// Bytes held by loose objects (`size`; KiB-rounded by git).
    pub loose_bytes: u64,
    pub loose_object_count: u64,
    pub refs_bytes: u64,
    /// Reflog data under `logs/`; grows without bound on active repos until
    /// expired — frequently the second-largest consumer after packs.
    pub reflog_bytes: u64,
    /// Git LFS object payloads, when present.
    pub lfs_bytes: u64,
    /// Embedded submodule repositories under `.git/modules`.
    pub modules_bytes: u64,
    /// Per-worktree admin state (HEADs, index copies) for linked worktrees.
    pub worktrees_admin_bytes: u64,
    pub index_bytes: u64,
    /// Everything else in the git dir not accounted above (hooks, info,
    /// COMMIT_EDITMSG, …).
    pub other_bytes: u64,
    /// Total bytes under the resolved common git directory from the walk.
    pub total_bytes: u64,
    /// True when loose objects are numerous enough that `git gc` would
    /// meaningfully compact the repository.
    pub gc_recommended: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BranchStorageSummary {
    pub local_count: usize,
    pub remote_tracking_count: usize,
    /// Local branches already merged into the default branch — deletable
    /// weight, deliberately identical to MANVI's conservative cleanup plan.
    pub merged_stale_count: usize,
    /// Local branches whose upstream no longer exists on the remote.
    pub gone_upstream_count: usize,
    pub sample_merged_stale: Vec<String>,
    pub sample_gone_upstream: Vec<String>,
    pub error: Option<String>,
}

/// How confident the audit is that a reclaim item's bytes are real.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReclaimConfidence {
    /// The bytes were walked and counted. Deleting the item frees this much.
    Measured,
    /// The bytes are an upper bound on what the action could free — repacking
    /// loose objects reclaims *some* of their size, never all of it.
    Estimated,
}

/// Whether reclaiming is a decision GitPulse would make for the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReclaimSafety {
    /// Regenerable output. Deleting it costs a rebuild and nothing else.
    Safe,
    /// Needs a human: the content may not be reproducible, or removing it
    /// changes what git tracks.
    NeedsReview,
}

/// What kind of waste an item is. The UI groups by this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReclaimCategory {
    BuildOutput,
    Cache,
    /// Loose objects a repack would compact.
    GitObjects,
    /// Reflog entries past their expiry.
    Reflog,
    /// `.git/worktrees/<name>` whose worktree no longer exists on disk.
    OrphanedWorktreeAdmin,
    /// Local branches already merged into the default branch.
    MergedBranches,
    /// Oversized files in the working tree.
    LargeFile,
}

/// One line of the reclaim audit: a thing taking space, what it would take to
/// get the space back, and how sure the audit is about both.
///
/// The report used to publish raw sizes and leave every judgement to the
/// reader: `artifacts` said a directory was 4 GB, `gc_recommended` was a bare
/// boolean, prunable worktree admin was not detected at all, and nothing
/// anywhere said how many bytes were actually recoverable. A number with no
/// action attached is not an audit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReclaimItem {
    pub category: ReclaimCategory,
    /// Repository-relative path, or a synthetic label for items that are not
    /// one path (`.git objects`, `merged branches`).
    pub label: String,
    pub bytes: u64,
    pub confidence: ReclaimConfidence,
    pub safety: ReclaimSafety,
    /// The command that reclaims it. Shown, never run: this audit reports, and
    /// destructive git operations stay behind the user's own hand.
    pub action: String,
    /// Why this item is here, in one sentence.
    pub detail: String,
    /// Set when something prevents a straightforward reclaim — a build
    /// directory with files committed to git, for instance.
    pub blocked_reason: Option<String>,
}

/// The audit's bottom line.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReclaimSummary {
    /// Sum of `Measured` + `Safe` items only.
    ///
    /// Deliberately narrow. Adding estimates in would produce a headline the
    /// repository cannot actually deliver, and adding `NeedsReview` items in
    /// would promise space that only exists if the user agrees to lose
    /// something. Both are reported separately rather than folded in.
    pub reclaimable_bytes: u64,
    /// Upper bound contributed by `Estimated` items.
    pub estimated_bytes: u64,
    /// Bytes behind items a human has to judge.
    pub needs_review_bytes: u64,
    pub item_count: usize,
    /// True when the walk that fed this audit was cut short, so the ledger is
    /// a sample and `reclaimable_bytes` is a floor.
    pub partial: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanStats {
    pub elapsed_ms: u128,
    pub files_visited: u64,
    pub permission_denied: u64,
    /// True when any budget (time/files/entries/depth) cut the scan short.
    /// Totals are floors then, not ceilings — never render them as complete.
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StorageTotals {
    /// Everything under the main working tree excluding the git directory.
    pub worktree_bytes: u64,
    /// The resolved COMMON git directory (shared by all linked worktrees).
    pub git_dir_bytes: u64,
    pub grand_bytes: u64,
    pub build_artifacts_bytes: u64,
    pub cache_artifacts_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageReport {
    pub repo_path: String,
    pub generated_at_epoch_secs: u64,
    pub is_bare: bool,
    pub totals: StorageTotals,
    pub git: GitStorage,
    /// Working-tree artifact directories with nonzero size, largest first.
    pub artifacts: Vec<ArtifactDir>,
    pub largest_files: Vec<LargeFile>,
    /// Linked worktrees with their on-disk size. The MAIN worktree's tree is
    /// already `totals.worktree_bytes`; only additional worktrees appear here.
    pub worktrees: Vec<WorktreeUsage>,
    pub branches: BranchStorageSummary,
    /// Ranked reclaim audit, largest recoverable first.
    #[serde(default)]
    pub reclaim: Vec<ReclaimItem>,
    #[serde(default)]
    pub reclaim_summary: ReclaimSummary,
    pub scan: ScanStats,
}

/// Mutable budget governing one walk phase.
struct WalkBudget {
    deadline: Instant,
    truncated: bool,
    permission_denied: u64,
    files_visited: u64,
    max_files: usize,
}

impl WalkBudget {
    #[cfg(test)]
    fn new() -> Self {
        Self::with_limits(Instant::now() + SCAN_DEADLINE, MAX_FILES_WORKTREE)
    }

    fn with_limits(deadline: Instant, max_files: usize) -> Self {
        Self {
            deadline,
            truncated: false,
            permission_denied: 0,
            files_visited: 0,
            max_files,
        }
    }

    fn exhausted(&self) -> bool {
        Instant::now() >= self.deadline || self.files_visited >= self.max_files as u64
    }

    fn mark_exhausted(&mut self) {
        self.truncated = true;
    }
}

fn artifact_kind(dir_name: &str) -> Option<ArtifactKind> {
    // Known output/cache directory names across ecosystems. Deliberately
    // name-based (not path-based) so monorepo nesting is caught anywhere.
    const BUILD: &[&str] = &[
        "node_modules",
        "target",
        "dist",
        "build",
        "buildout",
        "out",
        ".next",
        ".nuxt",
        ".output",
        ".svelte-kit",
        "cmake-build-debug",
        "cmake-build-release",
        "DerivedData",
        "_build",
    ];
    const CACHE: &[&str] = &[
        "__pycache__",
        ".pytest_cache",
        ".mypy_cache",
        ".ruff_cache",
        ".tox",
        ".venv",
        "venv",
        ".gradle",
        ".dart_tool",
        ".terraform",
        ".cxx",
        ".kotlin",
        "Pods",
        "obj",
        ".parcel-cache",
        ".turbo",
        ".eslintcache",
        ".pnpm-store",
        ".npm-cache",
        ".yarn-cache",
        "coverage",
        ".nyc_output",
        ".vite",
        ".webpack",
        ".sass-cache",
        ".cache",
        ".devcouncil",
        ".gitnexus",
        ".claude",
        ".cursor",
        ".agents",
        ".gemini",
        ".antigravity",
        // Go ecosystem compilation and module caches
        ".gocache",
        ".gopath",
        ".gomodcache",
        // Temporary, scratch, and test artifact directories
        ".tmp",
        "tmp",
        "temp",
        // Runtime and test log outputs
        "logs",
        "log",
        // Benchmarking suites
        ".bench-cache",
        ".bench",
        // Agent state directories
        ".opencode",
    ];
    if BUILD.contains(&dir_name) {
        Some(ArtifactKind::Build)
    } else if CACHE.contains(&dir_name) {
        Some(ArtifactKind::Cache)
    } else {
        None
    }
}

/// Returns true if a directory path is located inside a source tree (`src/`, `*/src/`).
fn is_inside_src(rel: &str) -> bool {
    rel.starts_with("src/") || rel.contains("/src/")
}

/// Generic directory names that are commonly used inside source trees (e.g.
/// `src/lib/coverage` in GitPulse, or `src/build/`), which must NOT be treated
/// as build/cache artifacts when found inside source roots.
fn is_generic_source_name(name: &str) -> bool {
    matches!(
        name,
        "coverage"
            | "vendor"
            | "build"
            | "out"
            | "dist"
            | "obj"
            | "_build"
            | "tmp"
            | "temp"
            | "log"
            | "logs"
    )
}

/// Known monolithic artifact containers that should NEVER open nested child
/// artifact scopes. Everything inside them belongs entirely to this container.
fn is_container_artifact(name: &str) -> bool {
    matches!(
        name,
        "target"
            | "node_modules"
            | ".venv"
            | "venv"
            | "DerivedData"
            | "cmake-build-debug"
            | "cmake-build-release"
            | ".gradle"
            | ".dart_tool"
            | ".pnpm-store"
    )
}

/// Generic build subdirectories that should never open as a new child artifact
/// scope if already inside ANY active artifact scope.
fn is_generic_build_subdir(name: &str) -> bool {
    matches!(name, "build" | "out" | "dist" | "obj" | "_build")
}

/// Directories never descended into during any walk.
fn is_special_dir(name: &str) -> bool {
    // Nested git directories (submodule working copies, vendored repos) are
    // accounted through `.git/modules` or reported as ordinary subtrees; the
    // walker must never cross into them or follow them elsewhere.
    name == ".git"
}

/// Helper to collect the largest files across traversals using a bounded min-heap.
struct LargeFileCollector {
    heap: std::collections::BinaryHeap<std::cmp::Reverse<(u64, String)>>,
}

impl LargeFileCollector {
    fn new() -> Self {
        Self {
            heap: std::collections::BinaryHeap::with_capacity(MAX_LARGE_FILES + 1),
        }
    }

    fn consider(&mut self, bytes: u64, rel_path: &str) {
        if bytes >= LARGE_FILE_THRESHOLD {
            self.heap
                .push(std::cmp::Reverse((bytes, rel_path.to_string())));
            if self.heap.len() > MAX_LARGE_FILES {
                self.heap.pop();
            }
        }
    }

    fn finish(mut self) -> Vec<LargeFile> {
        let mut files: Vec<LargeFile> = Vec::with_capacity(self.heap.len());
        while let Some(std::cmp::Reverse((bytes, path))) = self.heap.pop() {
            files.push(LargeFile { path, bytes });
        }
        files.reverse(); // descending order: largest first
        files
    }
}

/// Sums the logical size of every regular file under `root`, iteratively.
///
/// Symlinks are never followed (their own link size counts, targets do not).
/// Returns the byte floor plus whether any budget cut the walk short.
fn size_tree(root: &Path, budget: &mut WalkBudget) -> (u64, bool) {
    size_tree_scoped(root, budget, false)
}

fn size_tree_scoped(root: &Path, budget: &mut WalkBudget, is_git: bool) -> (u64, bool) {
    let mut total = 0u64;
    // (dir, depth, in_artifact)
    let mut stack: Vec<(PathBuf, usize, bool)> = vec![(root.to_path_buf(), 0, is_git)];
    let mut local_truncated = false;
    #[cfg(unix)]
    let mut seen_inodes = std::collections::HashSet::<(u64, u64)>::new();
    #[cfg(unix)]
    let mut seen_dir_inodes = std::collections::HashSet::<(u64, u64)>::new();
    #[cfg(unix)]
    if let Ok(meta) = fs::metadata(root) {
        seen_dir_inodes.insert((meta.dev(), meta.ino()));
    }

    while let Some((dir, depth, in_artifact)) = stack.pop() {
        if budget.exhausted() || local_truncated {
            budget.mark_exhausted();
            return (total, true);
        }
        let read = match fs::read_dir(&dir) {
            Ok(read) => read,
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                budget.permission_denied += 1;
                continue;
            }
            Err(_) => continue,
        };

        let max_entries = if in_artifact || is_git {
            MAX_ENTRIES_PER_ARTIFACT_DIR
        } else {
            MAX_ENTRIES_PER_DIR
        };

        let mut entries_seen = 0usize;
        for entry in read.flatten() {
            entries_seen += 1;
            if entries_seen > max_entries {
                local_truncated = true;
                break;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let Ok(name) = entry.file_name().into_string() else {
                continue; // non-UTF8 name: skipped, never panics
            };
            if file_type.is_symlink() {
                // Count the link's own size, never its target's.
                if let Ok(meta) = fs::symlink_metadata(entry.path()) {
                    total += meta.len();
                }
                continue;
            }
            if file_type.is_dir() {
                if is_special_dir(&name) {
                    continue;
                }
                if depth >= MAX_DEPTH {
                    local_truncated = true;
                    break;
                }
                #[cfg(unix)]
                if let Ok(meta) = fs::metadata(entry.path()) {
                    if !seen_dir_inodes.insert((meta.dev(), meta.ino())) {
                        continue;
                    }
                }
                let child_in_artifact = in_artifact || artifact_kind(&name).is_some();
                stack.push((entry.path(), depth + 1, child_in_artifact));
            } else if file_type.is_file() {
                budget.files_visited += 1;
                match fs::metadata(entry.path()) {
                    Ok(meta) => {
                        let logical_len = meta.len();
                        #[cfg(unix)]
                        let is_dup = if meta.nlink() > 1 {
                            !seen_inodes.insert((meta.dev(), meta.ino()))
                        } else {
                            false
                        };
                        #[cfg(not(unix))]
                        let is_dup = false;

                        if !is_dup {
                            total += logical_len;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                        budget.permission_denied += 1;
                    }
                    Err(_) => {}
                }
            }
        }
    }
    if local_truncated {
        budget.mark_exhausted();
    }
    (total, local_truncated)
}

/// Collects regular files at or above [`LARGE_FILE_THRESHOLD`] under `root`,
/// keeping the [`MAX_LARGE_FILES`] biggest, sorted descending by size.
#[cfg(test)]
fn collect_large_files(
    root: &Path,
    budget: &mut WalkBudget,
    skip_top_level: &[&str],
) -> Vec<LargeFile> {
    let mut collector = LargeFileCollector::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    let mut truncated = false;

    'outer: while let Some((dir, depth)) = stack.pop() {
        if budget.exhausted() {
            budget.mark_exhausted();
            break;
        }
        let Ok(read) = fs::read_dir(&dir) else {
            continue;
        };
        let mut entries_seen = 0usize;
        for entry in read.flatten() {
            entries_seen += 1;
            if entries_seen > MAX_ENTRIES_PER_DIR {
                truncated = true;
                break;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if is_special_dir(&name) {
                    continue;
                }
                if depth >= MAX_DEPTH {
                    truncated = true;
                    break;
                }
                if depth == 0 && skip_top_level.contains(&name.as_str()) {
                    continue;
                }
                stack.push((entry.path(), depth + 1));
            } else if file_type.is_file() {
                budget.files_visited += 1;
                let len = fs::metadata(entry.path()).map(|m| m.len()).unwrap_or(0);
                if len >= LARGE_FILE_THRESHOLD {
                    let rel = entry
                        .path()
                        .strip_prefix(root)
                        .unwrap_or(entry.path().as_path())
                        .to_string_lossy()
                        .replace('\\', "/");
                    collector.consider(len, &rel);
                }
            }
            if budget.exhausted() {
                budget.mark_exhausted();
                break 'outer;
            }
        }
    }
    if truncated {
        budget.mark_exhausted();
    }
    collector.finish()
}

/// Parses `git count-objects -v` into authoritative object-store numbers:
/// (loose_count, loose_kib, pack_file_count, pack_kib). Values carry git's
/// own unit suffix ("24 KiB"), so only the leading integer is taken.
/// Malformed lines degrade to zero, matching reader conventions elsewhere.
fn parse_count_objects(stdout: &str) -> (u64, u64, u64, u64) {
    let mut out = (0u64, 0u64, 0u64, 0u64);
    for line in stdout.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let digits: String = value
            .trim()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let value = digits.parse::<u64>().unwrap_or(0);
        match key.trim() {
            "count" => out.0 = value,
            "size" => out.1 = value,
            "packs" => out.2 = value,
            "size-pack" => out.3 = value,
            _ => {}
        }
    }
    out
}

/// Sizes the named subdirectory of the git dir, tolerating absence.
fn git_subdir_bytes(git_dir: &Path, name: &str, budget: &mut WalkBudget) -> u64 {
    let path = git_dir.join(name);
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_dir() => size_tree(&path, budget).0,
        _ => 0,
    }
}

/// Which of `candidates` are covered by ignore rules, in ONE batched call.
///
/// Candidates are directories, so each is fed with a trailing `/`: patterns
/// like `coverage/` only match directory paths, and check-ignore compares
/// the pathname as given. Echoed names are normalized back to bare paths so
/// callers can look candidates up directly.
///
/// Exit code 1 means "nothing is ignored" — a valid answer here, not an
/// error. Any other failure returns the empty set (callers then report
/// `unignored` conservatively, which is the safe direction for surfacing
/// hygiene gaps).
fn batch_check_ignore(repo: &Path, candidates: &[String]) -> std::collections::HashSet<String> {
    if candidates.is_empty() {
        return Default::default();
    }

    // Paths go over stdin (they are pathnames, not pathspecs): no globbing,
    // no argv-length limit, and `-z` framing survives any byte except NUL.
    let mut stdin_bytes = Vec::with_capacity(candidates.iter().map(|c| c.len() + 2).sum());
    for candidate in candidates {
        stdin_bytes.extend_from_slice(candidate.as_bytes());
        stdin_bytes.push(b'/');
        stdin_bytes.push(0);
    }

    match crate::engine::git_cli::git_with_stdin(
        repo,
        // `--no-index`: report what the RULES say, independent of index
        // state. Without it, any directory containing tracked files is
        // silently reported as non-ignored — hiding exactly the
        // "ignored dir with committed content" gap this scan exists to find.
        &["check-ignore", "--stdin", "-z", "--verbose", "--no-index"],
        &stdin_bytes,
    ) {
        Ok(out) => parse_check_ignore_z(&String::from_utf8_lossy(&out)),
        Err(e) if e.contains("failed with status 1") => Default::default(),
        Err(_) => Default::default(),
    }
}

/// Parses `check-ignore -z --verbose` output: records of
/// `<source>\0<linenum>\0<pattern>\0<pathname>\0`. Only records carrying a
/// real source and pattern mean "ignored"; anything malformed is skipped.
/// Trailing slashes are stripped from echoed names (candidates are fed with
/// one to activate directory-only patterns).
fn parse_check_ignore_z(stdout: &str) -> std::collections::HashSet<String> {
    let mut ignored = std::collections::HashSet::new();
    let fields: Vec<&str> = stdout.split('\0').collect();
    let mut i = 0;
    while i + 3 < fields.len() {
        let source = fields[i];
        let pattern = fields[i + 2];
        let pathname = fields[i + 3].trim_end_matches('/');
        if !source.trim().is_empty() && !pattern.trim().is_empty() && !pathname.is_empty() {
            ignored.insert(pathname.to_string());
        }
        i += 4;
    }
    ignored
}

/// Counts index-tracked files under each candidate prefix in ONE batched
/// `git ls-files` call. Candidate prefixes get `:(literal)` pathspec magic so
/// glob characters inside user-named intermediate directories cannot widen
/// what is counted.
fn batch_tracked_counts(repo: &Path, candidates: &[String]) -> HashMap<String, u64> {
    let mut counts: HashMap<String, u64> = HashMap::new();
    if candidates.is_empty() {
        return counts;
    }
    let literal: Vec<String> = candidates
        .iter()
        .map(|c| format!(":(literal){c}"))
        .collect();
    let mut argv: Vec<&str> = vec!["ls-files", "-z", "--"];
    argv.extend(literal.iter().map(String::as_str));
    let Ok(bytes) = crate::engine::git_cli::git_with_stdin(repo, &argv, &[]) else {
        return counts;
    };
    for raw in bytes.split(|b| *b == 0) {
        let Ok(path) = std::str::from_utf8(raw) else {
            continue;
        };
        if path.is_empty() {
            continue;
        }
        for candidate in candidates {
            let prefix = format!("{candidate}/");
            if path.starts_with(prefix.as_str()) || path == candidate {
                *counts.entry(candidate.to_string()).or_insert(0) += 1;
                break;
            }
        }
    }
    counts
}

/// Attributes every walked byte to its nearest enclosing artifact scope, so
/// nested artifacts report both levels honestly and category totals count
/// each byte exactly once. Monolithic containers (like `target`) roll up all
/// nested compilation output (e.g. `debug/build/.../out`) without fragmentation.
struct ArtifactCollector {
    /// (rel path, kind, abs path, bytes attributed to THIS scope only)
    found: Vec<(String, ArtifactKind, PathBuf, u64)>,
    /// Indices into `found` for scopes currently being walked.
    open: Vec<usize>,
    build_total: u64,
    cache_total: u64,
}

impl ArtifactCollector {
    fn new() -> Self {
        Self {
            found: Vec::new(),
            open: Vec::new(),
            build_total: 0,
            cache_total: 0,
        }
    }

    fn on_dir_enter(&mut self, abs: &Path, rel: &str, name: &str) {
        // Protect source trees: generic names like `coverage` inside `src/` are source code.
        if is_inside_src(rel) && is_generic_source_name(name) {
            return;
        }

        let Some(kind) = artifact_kind(name) else {
            return;
        };

        // If already inside an artifact scope, do not open nested child scopes if:
        // 1. The enclosing scope is a monolithic container (e.g. `target`, `node_modules`, `.venv`).
        // 2. The child directory is a generic build output directory (e.g. `build`, `out`, `dist`).
        if let Some(&top) = self.open.last() {
            let enclosing_rel = &self.found[top].0;
            let enclosing_name = enclosing_rel
                .rsplit('/')
                .next()
                .unwrap_or(enclosing_rel.as_str());
            if is_container_artifact(enclosing_name) || is_generic_build_subdir(name) {
                return;
            }
        }

        self.found
            .push((rel.to_string(), kind, abs.to_path_buf(), 0));
        self.open.push(self.found.len() - 1);
    }

    fn on_file(&mut self, bytes: u64) {
        if bytes == 0 {
            return;
        }
        if let Some(&top) = self.open.last() {
            self.found[top].3 += bytes;
            match self.found[top].1 {
                ArtifactKind::Build => self.build_total += bytes,
                ArtifactKind::Cache => self.cache_total += bytes,
            }
        }
    }
}

/// Traversal context passed down during the single-pass worktree walk.
struct WorktreeWalkContext<'a> {
    root: &'a Path,
    collector: &'a mut ArtifactCollector,
    entered_depths: &'a mut Vec<(usize, usize)>,
    #[cfg(unix)]
    seen_inodes: &'a mut std::collections::HashSet<(u64, u64)>,
    large_collector: &'a mut LargeFileCollector,
    linked_worktrees: &'a std::collections::HashSet<PathBuf>,
    ancestor_names: &'a mut Vec<String>,
    #[cfg(unix)]
    seen_dir_inodes: &'a mut std::collections::HashSet<(u64, u64)>,
}

/// Depth-first worktree walk attributing sizes to artifact scopes and collecting
/// large files in a single pass. Returns total bytes under `root` excluding the
/// `.git` entry and any linked worktrees (measured separately).
fn walk_worktree(
    ctx: &mut WorktreeWalkContext<'_>,
    dir: &Path,
    depth: usize,
    budget: &mut WalkBudget,
) -> u64 {
    if budget.exhausted() {
        budget.mark_exhausted();
        return 0;
    }
    let read = match fs::read_dir(dir) {
        Ok(read) => read,
        // An unreadable subtree is skipped and counted — never fatal, and
        // never silently treated as "empty" in the stats.
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            budget.permission_denied += 1;
            return 0;
        }
        Err(_) => return 0,
    };
    let mut total = 0u64;
    let mut subdirs: Vec<(PathBuf, String)> = Vec::new();
    let mut entries_seen = 0usize;

    let in_artifact = !ctx.collector.open.is_empty();
    let max_entries = if in_artifact {
        MAX_ENTRIES_PER_ARTIFACT_DIR
    } else {
        MAX_ENTRIES_PER_DIR
    };

    let rel_dir = dir
        .strip_prefix(ctx.root)
        .unwrap_or(Path::new(""))
        .to_string_lossy()
        .replace('\\', "/");

    for entry in read.flatten() {
        entries_seen += 1;
        if entries_seen > max_entries {
            budget.mark_exhausted();
            break;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let child_rel = if rel_dir.is_empty() {
            name.clone()
        } else {
            format!("{rel_dir}/{name}")
        };

        if file_type.is_symlink() {
            let bytes = fs::symlink_metadata(entry.path())
                .map(|m| m.len())
                .unwrap_or(0);
            ctx.collector.on_file(bytes);
            total += bytes;
        } else if file_type.is_dir() {
            if is_special_dir(&name) && depth == 0 {
                // Only the checkout's own .git entry is excluded here; deeper
                // `.git` lookalikes are ordinary (vendored) trees.
                continue;
            }
            if depth >= MAX_DEPTH {
                budget.mark_exhausted();
                break;
            }
            subdirs.push((entry.path(), child_rel));
        } else if file_type.is_file() {
            budget.files_visited += 1;
            match fs::metadata(entry.path()) {
                Ok(meta) => {
                    let logical_len = meta.len();
                    #[cfg(unix)]
                    let is_dup = if meta.nlink() > 1 {
                        !ctx.seen_inodes.insert((meta.dev(), meta.ino()))
                    } else {
                        false
                    };
                    #[cfg(not(unix))]
                    let is_dup = false;

                    if !is_dup {
                        ctx.collector.on_file(logical_len);
                        total += logical_len;
                        ctx.large_collector.consider(logical_len, &child_rel);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                    budget.permission_denied += 1;
                }
                Err(_) => {}
            }
        }
    }

    // Enter subdirectories after files so scope bookkeeping stays ordered.
    for (child_path, child_rel) in subdirs {
        // Linked worktrees (e.g. .claude/worktrees/*, .cursor/worktrees/*, or worktrees/*)
        // or submodules with their own git pointers: skip descending. They are sized
        // individually in collect_worktree_usage, never as part of the main worktree.
        let is_linked_worktree = ctx.linked_worktrees.contains(&child_path)
            || fs::canonicalize(&child_path)
                .map(|c| ctx.linked_worktrees.contains(&c))
                .unwrap_or(false)
            || child_path.join(".git").exists();
        if is_linked_worktree {
            continue;
        }

        #[cfg(unix)]
        if let Ok(meta) = fs::metadata(&child_path) {
            if !ctx.seen_dir_inodes.insert((meta.dev(), meta.ino())) {
                continue;
            }
        }

        let child_depth = depth + 1;
        let child_name = child_rel.rsplit('/').next().unwrap_or_default().to_string();

        // Rogue nesting / recursive loop heuristic: if a directory component name
        // appears >= 2 times in its active parent chain (e.g. backend/go_orchestrator/backend/go_orchestrator/backend),
        // prune the runaway recursion and mark budget exhausted.
        let repetition_count = ctx
            .ancestor_names
            .iter()
            .filter(|&a| a == &child_name)
            .count();
        if repetition_count >= 2 {
            budget.mark_exhausted();
            continue;
        }

        ctx.ancestor_names.push(child_name.clone());
        let scopes_before = ctx.collector.open.len();
        ctx.collector
            .on_dir_enter(&child_path, &child_rel, &child_name);
        let opened_scope = ctx.collector.open.len() > scopes_before;
        if opened_scope {
            ctx.entered_depths
                .push((ctx.collector.open.len() - 1, child_depth));
        }
        total = total.saturating_add(walk_worktree(ctx, &child_path, child_depth, budget));
        ctx.ancestor_names.pop();
        // The child's subtree is done: close ITS scope and anything deeper
        // it opened. Without this, a finished artifact scope stays open and
        // every later sibling's bytes are attributed to it.
        if opened_scope {
            while let Some(&(_, d)) = ctx.entered_depths.last() {
                if d < child_depth {
                    break;
                }
                ctx.entered_depths.pop();
                let closed = ctx.collector.open.pop();
                debug_assert!(closed.is_some(), "scope stack underflow");
            }
        }
    }

    total
}

/// Sizes linked worktrees (never the main one — it is the scanned tree
/// itself). Each linked worktree receives its own file budget bounded by the
/// global scan deadline so a large worktree cannot starve other scans.
fn collect_worktree_usage(
    infos: &[crate::engine::WorktreeInfo],
    deadline: Instant,
) -> (Vec<WorktreeUsage>, u64, u64, bool) {
    let mut out = Vec::new();
    let mut total_files = 0u64;
    let mut total_perms = 0u64;
    let mut any_truncated = false;

    for info in infos
        .iter()
        .filter(|i| !i.is_main)
        .take(MAX_SIZED_WORKTREES)
    {
        let path = PathBuf::from(&info.path);
        let mut wt_budget = WalkBudget::with_limits(deadline, MAX_FILES_PER_WORKTREE);
        let (bytes, truncated) = size_tree(&path, &mut wt_budget);
        total_files = total_files.saturating_add(wt_budget.files_visited);
        total_perms = total_perms.saturating_add(wt_budget.permission_denied);
        let is_trunc = truncated || wt_budget.truncated;
        if is_trunc {
            any_truncated = true;
        }
        out.push(WorktreeUsage {
            path: info.path.clone(),
            name: info.name.clone(),
            branch: info.branch.clone(),
            bytes,
            truncated: is_trunc,
        });
    }
    (out, total_files, total_perms, any_truncated)
}

fn branch_summary(repo_path: &str) -> BranchStorageSummary {
    let mut summary = BranchStorageSummary::default();
    let branches = match crate::engine::GitReader::list_branches(repo_path) {
        Ok(b) => b,
        Err(e) => {
            summary.error = Some(e);
            return summary;
        }
    };
    // `is_gone` is list_branches' own upstream-vanished signal — the same
    // source every other view uses, so this panel cannot disagree with them.
    let gone_upstreams = || -> Vec<&crate::engine::BranchInfo> {
        branches
            .iter()
            .filter(|b| !b.is_remote && b.is_gone)
            .collect()
    };
    summary.local_count = branches.iter().filter(|b| !b.is_remote).count();
    summary.remote_tracking_count = branches.iter().filter(|b| b.is_remote).count();
    summary.gone_upstream_count = gone_upstreams().len();
    summary.sample_gone_upstream = gone_upstreams()
        .into_iter()
        .take(MAX_BRANCH_SAMPLES)
        .map(|b| b.name.clone())
        .collect();

    // Reuse MANVI's conservative cleanup plan so the two surfaces can never
    // disagree about what is safely deletable.
    match crate::ops::branch_cleanup_plan(repo_path) {
        Ok(plan) => {
            summary.merged_stale_count = plan.candidates.len();
            summary.sample_merged_stale = plan
                .candidates
                .iter()
                .take(MAX_BRANCH_SAMPLES)
                .map(|c| c.name.clone())
                .collect();
        }
        Err(e) => summary.error = Some(e),
    }
    summary
}

/// Full storage scan. Fails only when `repo_path` is not a readable git
/// repository; everything else degrades into the report.
/// Cap on reclaim rows. The audit is a to-do list, not an inventory.
const MAX_RECLAIM_ITEMS: usize = 40;

/// Loose-object weight past which repacking is worth suggesting.
const GC_LOOSE_OBJECT_FLOOR: u64 = 10_000;
const GC_LOOSE_BYTES_FLOOR: u64 = 256 * 1024 * 1024;

/// Reflog weight past which expiry is worth suggesting. Below this the reflog
/// is doing its job cheaply and telling the user to prune their own safety net
/// would be bad advice.
const RECLAIM_REFLOG_FLOOR: u64 = 32 * 1024 * 1024;

/// Linked-worktree admin directories whose worktree is gone.
///
/// `.git/worktrees/<name>/gitdir` holds the path of the `.git` file inside the
/// checkout. When that path no longer exists the worktree was deleted with
/// `rm -rf` instead of `git worktree remove`, and the admin directory — index
/// copy, HEAD, ORIG_HEAD, per-worktree refs — is pure residue that `git
/// worktree prune` deletes.
///
/// Nothing in the previous report noticed this. `worktrees_admin_bytes` counted
/// the space and attributed it to live worktrees.
fn orphaned_worktree_admin(git_dir: &Path, budget: &mut WalkBudget) -> Vec<(String, u64)> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(git_dir.join("worktrees")) else {
        return found;
    };
    for entry in entries.flatten().take(MAX_SIZED_WORKTREES * 8) {
        if budget.exhausted() {
            break;
        }
        let admin = entry.path();
        if !admin.is_dir() {
            continue;
        }
        let Ok(gitdir) = fs::read_to_string(admin.join("gitdir")) else {
            // No gitdir file at all: git itself treats this as prunable.
            let (bytes, _) = size_tree(&admin, budget);
            found.push((entry.file_name().to_string_lossy().into_owned(), bytes));
            continue;
        };
        let target = PathBuf::from(gitdir.trim());
        if target.as_os_str().is_empty() || target.exists() {
            continue;
        }
        let (bytes, _) = size_tree(&admin, budget);
        found.push((entry.file_name().to_string_lossy().into_owned(), bytes));
    }
    found
}

/// Builds the reclaim audit from measurements the scan already took.
///
/// Everything here is derived, not re-walked — the audit costs one extra
/// `read_dir` of `.git/worktrees` and nothing else. An audit that doubled the
/// scan time would not be run often enough to matter.
fn build_reclaim(
    artifacts: &[ArtifactDir],
    git: &GitStorage,
    branches: &BranchStorageSummary,
    large_files: &[LargeFile],
    orphaned_admin: &[(String, u64)],
    partial: bool,
) -> (Vec<ReclaimItem>, ReclaimSummary) {
    let mut items: Vec<ReclaimItem> = Vec::new();

    for artifact in artifacts {
        if artifact.bytes == 0 {
            continue;
        }
        let committed = artifact.tracked_files > 0;
        let category = match artifact.kind {
            ArtifactKind::Build => ReclaimCategory::BuildOutput,
            ArtifactKind::Cache => ReclaimCategory::Cache,
        };
        items.push(ReclaimItem {
            category,
            label: artifact.path.clone(),
            bytes: artifact.bytes,
            confidence: ReclaimConfidence::Measured,
            // Committed content is not regenerable output any more, whatever
            // the directory is named: deleting it changes the working tree.
            safety: if committed {
                ReclaimSafety::NeedsReview
            } else {
                ReclaimSafety::Safe
            },
            action: format!("rm -rf {}", artifact.path),
            detail: if committed {
                format!(
                    "{} file(s) inside this directory are tracked by git.",
                    artifact.tracked_files
                )
            } else if artifact.unignored {
                "Regenerable output that no ignore rule covers, so it shows up in every status listing.".into()
            } else {
                "Regenerable build or cache output.".into()
            },
            blocked_reason: committed.then(|| {
                "Tracked in git — removing it is a commit, not a cleanup.".to_string()
            }),
        });
    }

    for (name, bytes) in orphaned_admin {
        items.push(ReclaimItem {
            category: ReclaimCategory::OrphanedWorktreeAdmin,
            label: format!(".git/worktrees/{name}"),
            bytes: *bytes,
            confidence: ReclaimConfidence::Measured,
            safety: ReclaimSafety::Safe,
            action: "git worktree prune".into(),
            detail: "Admin state for a linked worktree whose directory no longer exists.".into(),
            blocked_reason: None,
        });
    }

    if git.loose_object_count > GC_LOOSE_OBJECT_FLOOR || git.loose_bytes > GC_LOOSE_BYTES_FLOOR {
        items.push(ReclaimItem {
            category: ReclaimCategory::GitObjects,
            label: ".git/objects (loose)".into(),
            bytes: git.loose_bytes,
            // A repack compresses and deduplicates; how much it wins depends on
            // the content. Reporting the loose size as recoverable would be a
            // promise the repository cannot keep.
            confidence: ReclaimConfidence::Estimated,
            safety: ReclaimSafety::Safe,
            action: "git gc".into(),
            detail: format!(
                "{} loose object(s). Repacking compacts them; the figure is their current size, not the guaranteed saving.",
                git.loose_object_count
            ),
            blocked_reason: None,
        });
    }

    if git.reflog_bytes > RECLAIM_REFLOG_FLOOR {
        items.push(ReclaimItem {
            category: ReclaimCategory::Reflog,
            label: ".git/logs".into(),
            bytes: git.reflog_bytes,
            confidence: ReclaimConfidence::Estimated,
            // The reflog is the undo history for every ref. Expiring it is a
            // real loss of recoverability, so it is never "safe" here.
            safety: ReclaimSafety::NeedsReview,
            action: "git reflog expire --expire=90.days --all && git gc --prune=now".into(),
            detail: "Reflog entries accumulate without bound until expired; they are also what makes a bad reset recoverable.".into(),
            blocked_reason: None,
        });
    }

    if branches.merged_stale_count > 0 {
        items.push(ReclaimItem {
            category: ReclaimCategory::MergedBranches,
            label: format!("{} merged branch(es)", branches.merged_stale_count),
            // Branch refs are ~41 bytes each; the space is not the point, the
            // clutter is. Reporting a byte figure would overstate the win.
            bytes: 0,
            confidence: ReclaimConfidence::Estimated,
            safety: ReclaimSafety::NeedsReview,
            action: "Review in MANVI cleanup".into(),
            detail: "Local branches already merged into the default branch. They cost almost no disk, but they crowd every branch listing.".into(),
            blocked_reason: None,
        });
    }

    for file in large_files.iter().take(5) {
        items.push(ReclaimItem {
            category: ReclaimCategory::LargeFile,
            label: file.path.clone(),
            bytes: file.bytes,
            confidence: ReclaimConfidence::Measured,
            // GitPulse has no idea whether this is a dataset the user needs.
            safety: ReclaimSafety::NeedsReview,
            action: "Review, or move to Git LFS".into(),
            detail: "An oversized file in the working tree.".into(),
            blocked_reason: None,
        });
    }

    // Largest first, and a stable tiebreak so two scans of an unchanged
    // repository produce byte-identical reports.
    items.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.label.cmp(&b.label)));
    items.truncate(MAX_RECLAIM_ITEMS);

    let mut summary = ReclaimSummary {
        item_count: items.len(),
        partial,
        ..Default::default()
    };
    for item in &items {
        match (item.confidence, item.safety) {
            (ReclaimConfidence::Measured, ReclaimSafety::Safe) => {
                summary.reclaimable_bytes = summary.reclaimable_bytes.saturating_add(item.bytes);
            }
            (ReclaimConfidence::Estimated, _) => {
                summary.estimated_bytes = summary.estimated_bytes.saturating_add(item.bytes);
            }
            (ReclaimConfidence::Measured, ReclaimSafety::NeedsReview) => {
                summary.needs_review_bytes = summary.needs_review_bytes.saturating_add(item.bytes);
            }
        }
    }
    (items, summary)
}

pub fn scan_storage(repo_path: &str) -> Result<StorageReport, String> {
    let started = Instant::now();
    let repo = validate_repo(repo_path)?;
    let resolved = crate::engine::git_cli::resolve_repo(repo_path)?;
    let git_dir = resolve_git_common_dir(&repo)?;

    let deadline = started + SCAN_DEADLINE;

    // Discover worktree layout upfront: identify any linked worktrees whose roots
    // live inside the repository (e.g. .claude/worktrees/*, .cursor/worktrees/*, or worktrees/*).
    let worktree_infos = crate::engine::worktree::list_worktrees(repo_path).unwrap_or_default();
    let mut linked_worktrees = std::collections::HashSet::new();
    for wt in worktree_infos.iter().filter(|w| !w.is_main) {
        let p = PathBuf::from(&wt.path);
        if let Ok(canonical) = fs::canonicalize(&p) {
            linked_worktrees.insert(canonical);
        }
        linked_worktrees.insert(p);
    }

    // ---- Worktree walk: totals, artifacts, large files -------------------
    let mut collector = ArtifactCollector::new();
    let mut worktree_bytes = 0u64;
    let mut large_files: Vec<LargeFile> = Vec::new();
    let mut worktree_budget = WalkBudget::with_limits(deadline, MAX_FILES_WORKTREE);

    if !resolved.is_bare {
        let mut entered_depths: Vec<(usize, usize)> = Vec::new();
        let mut large_collector = LargeFileCollector::new();
        let mut ancestor_names: Vec<String> = Vec::new();
        #[cfg(unix)]
        let mut seen_inodes = std::collections::HashSet::new();
        #[cfg(unix)]
        let mut seen_dir_inodes = std::collections::HashSet::new();
        #[cfg(unix)]
        if let Ok(meta) = fs::metadata(&repo) {
            seen_dir_inodes.insert((meta.dev(), meta.ino()));
        }

        let mut ctx = WorktreeWalkContext {
            root: &repo,
            collector: &mut collector,
            entered_depths: &mut entered_depths,
            #[cfg(unix)]
            seen_inodes: &mut seen_inodes,
            large_collector: &mut large_collector,
            linked_worktrees: &linked_worktrees,
            ancestor_names: &mut ancestor_names,
            #[cfg(unix)]
            seen_dir_inodes: &mut seen_dir_inodes,
        };
        worktree_bytes = walk_worktree(&mut ctx, &repo, 0, &mut worktree_budget);
        large_files = large_collector.finish();
    }

    // ---- Object store: authoritative counters ----------------------------
    let (loose_objects, loose_kib, pack_files, pack_kib) =
        match git_text(&repo, &["count-objects", "-v"]) {
            Ok(text) => parse_count_objects(&text),
            Err(_) => (0, 0, 0, 0),
        };

    // ---- Git directory walk ----------------------------------------------
    // Dedicated budget: even if the worktree walk reached its file cap, the .git
    // directory sizing starts with a fresh budget up to MAX_FILES_GIT.
    let mut git_budget = WalkBudget::with_limits(deadline, MAX_FILES_GIT);
    let (raw_git_dir_bytes, git_truncated) = size_tree_scoped(&git_dir, &mut git_budget, true);
    let refs_bytes = git_subdir_bytes(&git_dir, "refs", &mut git_budget);
    let reflog_bytes = git_subdir_bytes(&git_dir, "logs", &mut git_budget);
    let lfs_bytes = git_subdir_bytes(&git_dir, "lfs", &mut git_budget);
    let modules_bytes = git_subdir_bytes(&git_dir, "modules", &mut git_budget);
    let worktrees_admin_bytes = git_subdir_bytes(&git_dir, "worktrees", &mut git_budget);
    let index_bytes = fs::symlink_metadata(git_dir.join("index"))
        .map(|m| m.len())
        .unwrap_or(0);

    let pack_bytes = pack_kib.saturating_mul(1024);
    let loose_bytes = loose_kib.saturating_mul(1024);
    let admin_accounted = refs_bytes
        .saturating_add(reflog_bytes)
        .saturating_add(lfs_bytes)
        .saturating_add(modules_bytes)
        .saturating_add(worktrees_admin_bytes)
        .saturating_add(index_bytes);
    let total_authoritative = pack_bytes
        .saturating_add(loose_bytes)
        .saturating_add(admin_accounted);

    // Fallback: If the git dir walk was truncated or returned fewer bytes
    // than the authoritative objects + admin metadata, floor it so Git data
    // never falsely reports 0 B when objects exist.
    let git_dir_bytes = raw_git_dir_bytes.max(total_authoritative);
    let other_bytes = git_dir_bytes.saturating_sub(total_authoritative);

    let git_storage = GitStorage {
        pack_bytes,
        pack_file_count: pack_files,
        loose_bytes,
        loose_object_count: loose_objects,
        refs_bytes,
        reflog_bytes,
        lfs_bytes,
        modules_bytes,
        worktrees_admin_bytes,
        index_bytes,
        other_bytes,
        total_bytes: git_dir_bytes,
        gc_recommended: loose_objects > 10_000 || loose_bytes > 256 * 1024 * 1024,
    };

    // ---- Artifacts: hygiene cross-check ----------------------------------
    let mut artifacts: Vec<ArtifactDir> = collector
        .found
        .iter()
        .filter(|(_, _, _, bytes)| *bytes > 0)
        .map(|(rel, kind, _, bytes)| ArtifactDir {
            path: rel.clone(),
            bytes: *bytes,
            kind: *kind,
            unignored: false,
            tracked_files: 0,
        })
        .collect();
    artifacts.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    if artifacts.len() > MAX_ARTIFACT_DIRS {
        artifacts.truncate(MAX_ARTIFACT_DIRS);
    }

    let candidates: Vec<String> = artifacts.iter().map(|a| a.path.clone()).collect();
    let ignored = batch_check_ignore(&repo, &candidates);
    let tracked = batch_tracked_counts(&repo, &candidates);
    for artifact in &mut artifacts {
        artifact.unignored = !ignored.contains(&artifact.path);
        artifact.tracked_files = tracked.get(&artifact.path).copied().unwrap_or(0);
    }

    // ---- Linked worktrees + branches -------------------------------------
    let (worktrees, wt_files, wt_perms, wt_truncated) =
        collect_worktree_usage(&worktree_infos, deadline);
    let branches = branch_summary(repo_path);

    // Category totals can exceed the walked tree only through budget skew;
    // clamp so the UI's stacked bar can never overflow the total.
    let build_total = collector.build_total.min(worktree_bytes);
    let cache_total = collector.cache_total.min(worktree_bytes);
    let grand = worktree_bytes.saturating_add(git_dir_bytes);

    let total_files = worktree_budget
        .files_visited
        .saturating_add(git_budget.files_visited)
        .saturating_add(wt_files);
    let total_perms = worktree_budget
        .permission_denied
        .saturating_add(git_budget.permission_denied)
        .saturating_add(wt_perms);
    let is_truncated =
        worktree_budget.truncated || git_truncated || git_budget.truncated || wt_truncated;

    // The deep audit. Derived from what the walks already measured, plus one
    // cheap read_dir for prunable worktree admin.
    let orphaned_admin = orphaned_worktree_admin(&git_dir, &mut git_budget);
    let (reclaim, reclaim_summary) = build_reclaim(
        &artifacts,
        &git_storage,
        &branches,
        &large_files,
        &orphaned_admin,
        is_truncated,
    );

    Ok(StorageReport {
        repo_path: repo_path.to_string(),
        generated_at_epoch_secs: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        is_bare: resolved.is_bare,
        totals: StorageTotals {
            worktree_bytes,
            git_dir_bytes,
            grand_bytes: grand,
            build_artifacts_bytes: build_total,
            cache_artifacts_bytes: cache_total,
        },
        git: git_storage,
        artifacts,
        largest_files: large_files,
        worktrees,
        branches,
        reclaim,
        reclaim_summary,
        scan: ScanStats {
            elapsed_ms: started.elapsed().as_millis(),
            files_visited: total_files,
            permission_denied: total_perms,
            truncated: is_truncated,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(path: &Path, size: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![b'x'; size]).unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn test_walk_ctx<'a>(
        root: &'a Path,
        collector: &'a mut ArtifactCollector,
        entered: &'a mut Vec<(usize, usize)>,
        large: &'a mut LargeFileCollector,
        linked: &'a std::collections::HashSet<PathBuf>,
        ancestors: &'a mut Vec<String>,
        #[cfg(unix)] seen_inodes: &'a mut std::collections::HashSet<(u64, u64)>,
        #[cfg(unix)] seen_dir_inodes: &'a mut std::collections::HashSet<(u64, u64)>,
    ) -> WorktreeWalkContext<'a> {
        WorktreeWalkContext {
            root,
            collector,
            entered_depths: entered,
            #[cfg(unix)]
            seen_inodes,
            large_collector: large,
            linked_worktrees: linked,
            ancestor_names: ancestors,
            #[cfg(unix)]
            seen_dir_inodes,
        }
    }

    #[test]
    fn artifact_kind_classifies_known_ecosystems() {
        assert_eq!(artifact_kind("node_modules"), Some(ArtifactKind::Build));
        assert_eq!(artifact_kind("__pycache__"), Some(ArtifactKind::Cache));
        assert_eq!(artifact_kind(".next"), Some(ArtifactKind::Build));
        assert_eq!(artifact_kind(".gocache"), Some(ArtifactKind::Cache));
        assert_eq!(artifact_kind(".gopath"), Some(ArtifactKind::Cache));
        assert_eq!(artifact_kind(".gomodcache"), Some(ArtifactKind::Cache));
        assert_eq!(artifact_kind(".tmp"), Some(ArtifactKind::Cache));
        assert_eq!(artifact_kind("tmp"), Some(ArtifactKind::Cache));
        assert_eq!(artifact_kind("temp"), Some(ArtifactKind::Cache));
        assert_eq!(artifact_kind("logs"), Some(ArtifactKind::Cache));
        assert_eq!(artifact_kind("log"), Some(ArtifactKind::Cache));
        assert_eq!(artifact_kind(".bench-cache"), Some(ArtifactKind::Cache));
        assert_eq!(artifact_kind(".opencode"), Some(ArtifactKind::Cache));
        assert_eq!(artifact_kind("src"), None);
        assert_eq!(artifact_kind(""), None);
    }

    #[test]
    fn size_tree_skips_git_dirs_and_never_follows_links() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(&root.join("a.txt"), 100);
        write_file(&root.join(".git/objects/pack/x.pack"), 5_000);
        write_file(&root.join("node_modules/pkg/index.js"), 700);
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/hosts", root.join("hostlink")).unwrap();

        let mut budget = WalkBudget::new();
        let (total, truncated) = size_tree(root, &mut budget);
        assert!(!truncated);
        #[cfg(unix)]
        {
            // A symlink's counted size is the length of its target-path
            // string, not the target file's content size.
            let link_len = std::fs::symlink_metadata(root.join("hostlink"))
                .unwrap()
                .len();
            assert_eq!(total, 100 + 700 + link_len);
        }
        #[cfg(not(unix))]
        assert_eq!(total, 800);
    }

    #[test]
    fn parse_count_objects_handles_real_output() {
        let text =
            "count: 17\nsize: 24 KiB\nin-pack: 300\npacks: 2\nsize-pack: 50000 KiB\ngarbage: 1\n";
        assert_eq!(parse_count_objects(text), (17, 24, 2, 50_000));
        assert_eq!(parse_count_objects("").0, 0);
        assert_eq!(parse_count_objects("garbage").0, 0);
    }

    #[test]
    fn check_ignore_parser_reads_verbose_z_records() {
        // \x00 keeps the NUL explicit: "\012" would read as an octal escape.
        let ignored = parse_check_ignore_z(".gitignore\x0012\0/build/\0build\0::\0src\0");
        assert!(ignored.contains("build"));
        assert!(!ignored.contains("src"));
        assert!(parse_check_ignore_z("").is_empty());
        // Truncated trailing record must not panic or half-match.
        assert!(parse_check_ignore_z("x\x001\0/p\0").is_empty());
    }

    #[test]
    fn deep_tree_respects_depth_cap_without_hanging() {
        let tmp = tempfile::tempdir().unwrap();
        let mut deep = tmp.path().to_path_buf();
        for i in 0..(MAX_DEPTH + 8) {
            deep.push(format!("d{i}"));
            std::fs::create_dir_all(&deep).unwrap();
        }
        write_file(&deep.join("leaf.bin"), 64);
        let mut budget = WalkBudget::new();
        let (_, truncated) = size_tree(tmp.path(), &mut budget);
        assert!(truncated, "depth cap must truncate, not recurse forever");
    }

    #[test]
    fn huge_fanout_respects_entries_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let fan = tmp.path().join("fan");
        std::fs::create_dir_all(&fan).unwrap();
        for i in 0..(MAX_ENTRIES_PER_DIR + 10) {
            write_file(&fan.join(format!("f{i}")), 1);
        }
        let mut budget = WalkBudget::new();
        let (_, truncated) = size_tree(tmp.path(), &mut budget);
        assert!(truncated);
    }

    #[test]
    fn large_files_skip_threshold_and_git_and_sort_descending() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(&root.join(".git/huge.pack"), 20 * 1024 * 1024);
        write_file(&root.join("video.mp4"), 15 * 1024 * 1024);
        write_file(&root.join("bigger.iso"), 30 * 1024 * 1024);
        write_file(&root.join("small.txt"), 12);
        let mut budget = WalkBudget::new();
        let files = collect_large_files(root, &mut budget, &[".git"]);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path.replace('\\', "/"), "bigger.iso");
        assert_eq!(files[0].bytes, 30 * 1024 * 1024);
        assert_eq!(files[1].path.replace('\\', "/"), "video.mp4");
    }

    #[test]
    fn nested_artifact_dirs_attribute_to_nearest_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(&root.join("build/direct.o"), 100);
        write_file(&root.join("build/node_modules/lib.js"), 400);
        write_file(&root.join("__pycache__/m.pyc"), 50);
        let mut budget = WalkBudget::new();
        let mut collector = ArtifactCollector::new();
        let mut entered = Vec::new();
        let mut large = LargeFileCollector::new();
        let linked = std::collections::HashSet::new();
        let mut ancestors = Vec::new();
        #[cfg(unix)]
        let mut seen = std::collections::HashSet::new();
        #[cfg(unix)]
        let mut seen_dirs = std::collections::HashSet::new();
        let mut ctx = test_walk_ctx(
            root,
            &mut collector,
            &mut entered,
            &mut large,
            &linked,
            &mut ancestors,
            #[cfg(unix)]
            &mut seen,
            #[cfg(unix)]
            &mut seen_dirs,
        );
        let total = walk_worktree(&mut ctx, root, 0, &mut budget);
        assert_eq!(total, 550);
        let mut by_path: HashMap<String, (u64, ArtifactKind)> = collector
            .found
            .iter()
            .map(|(rel, kind, _, bytes)| (rel.clone(), (*bytes, *kind)))
            .collect();
        assert_eq!(by_path.remove("build"), Some((100, ArtifactKind::Build)));
        assert_eq!(
            by_path.remove("build/node_modules"),
            Some((400, ArtifactKind::Build))
        );
        assert_eq!(
            by_path.remove("__pycache__"),
            Some((50, ArtifactKind::Cache))
        );
        assert!(by_path.is_empty());
        assert_eq!(collector.build_total, 500);
        assert_eq!(collector.cache_total, 50);
    }

    #[test]
    fn container_artifacts_roll_up_inner_build_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(&root.join("target/debug/app"), 100);
        write_file(&root.join("target/debug/build/pkg/out/lib.a"), 400);
        let mut budget = WalkBudget::new();
        let mut collector = ArtifactCollector::new();
        let mut entered = Vec::new();
        let mut large = LargeFileCollector::new();
        let linked = std::collections::HashSet::new();
        let mut ancestors = Vec::new();
        #[cfg(unix)]
        let mut seen = std::collections::HashSet::new();
        #[cfg(unix)]
        let mut seen_dirs = std::collections::HashSet::new();
        let mut ctx = test_walk_ctx(
            root,
            &mut collector,
            &mut entered,
            &mut large,
            &linked,
            &mut ancestors,
            #[cfg(unix)]
            &mut seen,
            #[cfg(unix)]
            &mut seen_dirs,
        );
        let total = walk_worktree(&mut ctx, root, 0, &mut budget);
        assert_eq!(total, 500);
        assert_eq!(collector.found.len(), 1);
        assert_eq!(collector.found[0].0, "target");
        assert_eq!(collector.found[0].3, 500);
    }

    #[test]
    fn source_dirs_exclude_generic_cache_names() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(&root.join("src/lib/coverage/file.ts"), 100);
        write_file(&root.join("src/build/script.rs"), 200);
        write_file(&root.join("src/lib/log/runtime.log"), 150);
        write_file(&root.join("src/lib/tmp/scratch.txt"), 80);
        write_file(&root.join("coverage/report.html"), 300);
        let mut budget = WalkBudget::new();
        let mut collector = ArtifactCollector::new();
        let mut entered = Vec::new();
        let mut large = LargeFileCollector::new();
        let linked = std::collections::HashSet::new();
        let mut ancestors = Vec::new();
        #[cfg(unix)]
        let mut seen = std::collections::HashSet::new();
        #[cfg(unix)]
        let mut seen_dirs = std::collections::HashSet::new();
        let mut ctx = test_walk_ctx(
            root,
            &mut collector,
            &mut entered,
            &mut large,
            &linked,
            &mut ancestors,
            #[cfg(unix)]
            &mut seen,
            #[cfg(unix)]
            &mut seen_dirs,
        );
        let total = walk_worktree(&mut ctx, root, 0, &mut budget);
        assert_eq!(total, 830);
        // Only coverage at root is an artifact, not src/lib/coverage, src/build, src/lib/log, src/lib/tmp.
        assert_eq!(collector.found.len(), 1);
        assert_eq!(collector.found[0].0, "coverage");
        assert_eq!(collector.found[0].3, 300);
    }

    #[cfg(unix)]
    #[test]
    fn hardlinks_deduplicate_in_size_tree_and_walk() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let f1 = root.join("file1.bin");
        let f2 = root.join("file2.bin");
        write_file(&f1, 5_000);
        std::fs::hard_link(&f1, &f2).unwrap();

        let mut budget = WalkBudget::new();
        let (tree_size, _) = size_tree(root, &mut budget);
        assert_eq!(
            tree_size, 5_000,
            "size_tree must deduplicate hard links on Unix"
        );

        let mut budget2 = WalkBudget::new();
        let mut collector = ArtifactCollector::new();
        let mut entered = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut seen_dirs = std::collections::HashSet::new();
        let mut large = LargeFileCollector::new();
        let linked = std::collections::HashSet::new();
        let mut ancestors = Vec::new();
        let mut ctx = test_walk_ctx(
            root,
            &mut collector,
            &mut entered,
            &mut large,
            &linked,
            &mut ancestors,
            &mut seen,
            &mut seen_dirs,
        );
        let walk_size = walk_worktree(&mut ctx, root, 0, &mut budget2);
        assert_eq!(
            walk_size, 5_000,
            "walk_worktree must deduplicate hard links on Unix"
        );
    }

    #[test]
    fn permission_denied_is_counted_not_fatal() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let tmp = tempfile::tempdir().unwrap();
            let locked = tmp.path().join("locked");
            std::fs::create_dir_all(&locked).unwrap();
            write_file(&locked.join("secret.bin"), 32);
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
            let mut budget = WalkBudget::new();
            let (_, truncated) = size_tree(tmp.path(), &mut budget);
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(!truncated, "unreadable subtree is skipped, not fatal");
        }
    }

    #[test]
    fn linked_worktrees_inside_repo_are_skipped_in_walk_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(&root.join("main.rs"), 1_000);
        write_file(&root.join(".claude/worktrees/feat/heavy.bin"), 50_000);
        write_file(&root.join(".claude/worktrees/feat/.git"), 100);

        let mut budget = WalkBudget::new();
        let mut collector = ArtifactCollector::new();
        let mut entered = Vec::new();
        let mut large = LargeFileCollector::new();
        let mut linked = std::collections::HashSet::new();
        let feat_path = root.join(".claude/worktrees/feat");
        linked.insert(feat_path.clone());
        if let Ok(c) = fs::canonicalize(&feat_path) {
            linked.insert(c);
        }
        let mut ancestors = Vec::new();
        #[cfg(unix)]
        let mut seen = std::collections::HashSet::new();
        #[cfg(unix)]
        let mut seen_dirs = std::collections::HashSet::new();
        let mut ctx = test_walk_ctx(
            root,
            &mut collector,
            &mut entered,
            &mut large,
            &linked,
            &mut ancestors,
            #[cfg(unix)]
            &mut seen,
            #[cfg(unix)]
            &mut seen_dirs,
        );
        let total = walk_worktree(&mut ctx, root, 0, &mut budget);
        // Linked worktree feat was skipped entirely! Only main.rs was walked.
        assert_eq!(total, 1_000);
        assert_eq!(collector.cache_total, 0);
        assert!(collector.found.iter().all(|(_, _, _, bytes)| *bytes == 0));
    }

    #[test]
    fn repeated_ancestor_names_prunes_recursion() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let runaway = root.join("orch/backend/orch/backend/orch/backend");
        write_file(&runaway.join("deep.bin"), 200);

        let mut budget = WalkBudget::new();
        let mut collector = ArtifactCollector::new();
        let mut entered = Vec::new();
        let mut large = LargeFileCollector::new();
        let linked = std::collections::HashSet::new();
        let mut ancestors = Vec::new();
        #[cfg(unix)]
        let mut seen = std::collections::HashSet::new();
        #[cfg(unix)]
        let mut seen_dirs = std::collections::HashSet::new();
        let mut ctx = test_walk_ctx(
            root,
            &mut collector,
            &mut entered,
            &mut large,
            &linked,
            &mut ancestors,
            #[cfg(unix)]
            &mut seen,
            #[cfg(unix)]
            &mut seen_dirs,
        );
        let total = walk_worktree(&mut ctx, root, 0, &mut budget);
        // The runaway loop must have been pruned before reaching deep.bin
        assert_eq!(total, 0);
        assert!(
            budget.truncated,
            "runaway recursion must mark budget exhausted"
        );
    }

    #[test]
    fn walk_budget_limits_and_exhaustion() {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut budget = WalkBudget::with_limits(deadline, 5);
        assert!(!budget.exhausted());
        budget.files_visited = 5;
        assert!(budget.exhausted());
    }
}
