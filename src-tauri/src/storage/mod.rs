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

/// Soft deadline for one full scan. Hitting it stops expansion and flags
/// `scan.truncated` rather than presenting a partial total as complete.
const SCAN_DEADLINE: Duration = Duration::from_secs(20);
/// Maximum traversal depth below the scanned root.
const MAX_DEPTH: usize = 48;
/// Maximum regular files visited across one whole scan (shared by all roots:
/// worktree, git dir, and each linked worktree) before truncating.
const MAX_FILES_PER_SCAN: usize = 250_000;
/// Maximum directory entries enumerated per directory before truncating.
const MAX_ENTRIES_PER_DIR: usize = 4_000;
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
    pub scan: ScanStats,
}

/// Mutable budget shared by every walk in one scan.
struct WalkBudget {
    deadline: Instant,
    truncated: bool,
    permission_denied: u64,
    files_visited: u64,
}

impl WalkBudget {
    fn new() -> Self {
        Self {
            deadline: Instant::now() + SCAN_DEADLINE,
            truncated: false,
            permission_denied: 0,
            files_visited: 0,
        }
    }
    fn exhausted(&self) -> bool {
        Instant::now() >= self.deadline || self.files_visited >= MAX_FILES_PER_SCAN as u64
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
        "vendor",
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
    ];
    if BUILD.contains(&dir_name) {
        Some(ArtifactKind::Build)
    } else if CACHE.contains(&dir_name) {
        Some(ArtifactKind::Cache)
    } else {
        None
    }
}

/// Directories never descended into during any walk.
fn is_special_dir(name: &str) -> bool {
    // Nested git directories (submodule working copies, vendored repos) are
    // accounted through `.git/modules` or reported as ordinary subtrees; the
    // walker must never cross into them or follow them elsewhere.
    name == ".git"
}

/// Sums the logical size of every regular file under `root`, iteratively.
///
/// Symlinks are never followed (their own link size counts, targets do not).
/// Returns the byte floor plus whether any budget cut the walk short.
fn size_tree(root: &Path, budget: &mut WalkBudget) -> (u64, bool) {
    let mut total = 0u64;
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    let mut local_truncated = false;

    while let Some((dir, depth)) = stack.pop() {
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

        let mut entries_seen = 0usize;
        for entry in read.flatten() {
            entries_seen += 1;
            if entries_seen > MAX_ENTRIES_PER_DIR {
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
                stack.push((entry.path(), depth + 1));
            } else if file_type.is_file() {
                budget.files_visited += 1;
                match fs::metadata(entry.path()) {
                    Ok(meta) => total += meta.len(),
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
fn collect_large_files(
    root: &Path,
    budget: &mut WalkBudget,
    skip_top_level: &[&str],
) -> Vec<LargeFile> {
    use std::cmp::Reverse;
    let mut heap: std::collections::BinaryHeap<Reverse<(u64, String)>> =
        std::collections::BinaryHeap::with_capacity(MAX_LARGE_FILES + 1);
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
                if len < LARGE_FILE_THRESHOLD {
                    continue;
                }
                let rel = entry
                    .path()
                    .strip_prefix(root)
                    .unwrap_or(entry.path().as_path())
                    .to_string_lossy()
                    .replace('\\', "/");
                heap.push(Reverse((len, rel)));
                if heap.len() > MAX_LARGE_FILES {
                    heap.pop();
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
    // `pop()` yields smallest-first on this min-heap; collecting then
    // reversing gives descending-by-size without nightly's sorted iterator.
    let mut files: Vec<LargeFile> = Vec::with_capacity(heap.len());
    while let Some(Reverse((bytes, path))) = heap.pop() {
        files.push(LargeFile { path, bytes });
    }
    files.reverse(); // descending: biggest first for display
    files
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
/// nested artifacts (vendored `build/` containing `node_modules/`) report
/// both levels honestly and category totals count each byte exactly once.
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
        if let Some(kind) = artifact_kind(name) {
            self.found
                .push((rel.to_string(), kind, abs.to_path_buf(), 0));
            self.open.push(self.found.len() - 1);
        }
    }

    fn on_file(&mut self, bytes: u64) {
        if let Some(&top) = self.open.last() {
            self.found[top].3 += bytes;
        }
        if let Some(&top) = self.open.last() {
            match self.found[top].1 {
                ArtifactKind::Build => self.build_total += bytes,
                ArtifactKind::Cache => self.cache_total += bytes,
            }
        }
    }
}

/// Depth-first worktree walk attributing sizes to artifact scopes. Returns
/// total bytes under `root` excluding the `.git` entry (measured separately
/// and authoritatively through the resolved common git dir).
fn walk_worktree(
    root: &Path,
    dir: &Path,
    depth: usize,
    budget: &mut WalkBudget,
    collector: &mut ArtifactCollector,
    entered_depths: &mut Vec<(usize, usize)>,
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

    let rel_dir = dir
        .strip_prefix(root)
        .unwrap_or(Path::new(""))
        .to_string_lossy()
        .replace('\\', "/");

    for entry in read.flatten() {
        entries_seen += 1;
        if entries_seen > MAX_ENTRIES_PER_DIR {
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
            collector.on_file(bytes);
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
            let bytes = match fs::metadata(entry.path()) {
                Ok(meta) => meta.len(),
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                    budget.permission_denied += 1;
                    0
                }
                Err(_) => 0,
            };
            collector.on_file(bytes);
            total += bytes;
        }
    }

    // Enter subdirectories after files so scope bookkeeping stays ordered.
    for (child_path, child_rel) in subdirs {
        let child_depth = depth + 1;
        let child_name = child_rel.rsplit('/').next().unwrap_or_default().to_string();
        let scopes_before = collector.open.len();
        collector.on_dir_enter(&child_path, &child_rel, &child_name);
        let opened_scope = collector.open.len() > scopes_before;
        if opened_scope {
            entered_depths.push((collector.open.len() - 1, child_depth));
        }
        total = total.saturating_add(walk_worktree(
            root,
            &child_path,
            child_depth,
            budget,
            collector,
            entered_depths,
        ));
        // The child's subtree is done: close ITS scope and anything deeper
        // it opened. Without this, a finished artifact scope stays open and
        // every later sibling's bytes are attributed to it.
        if opened_scope {
            while let Some(&(_, d)) = entered_depths.last() {
                if d < child_depth {
                    break;
                }
                entered_depths.pop();
                let closed = collector.open.pop();
                debug_assert!(closed.is_some(), "scope stack underflow");
            }
        }
    }

    total
}

/// Sizes linked worktrees (never the main one — it is the scanned tree
/// itself). Shares the caller's budget so a pathological worktree cannot
/// blow past the scan deadline unnoticed.
fn collect_worktree_usage(repo: &Path, budget: &mut WalkBudget) -> Vec<WorktreeUsage> {
    let infos = match crate::engine::worktree::list_worktrees(&repo.to_string_lossy()) {
        Ok(list) => list,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for info in infos
        .into_iter()
        .filter(|i| !i.is_main)
        .take(MAX_SIZED_WORKTREES)
    {
        let path = PathBuf::from(&info.path);
        let (bytes, truncated) = size_tree(&path, budget);
        out.push(WorktreeUsage {
            path: info.path,
            name: info.name,
            branch: info.branch,
            bytes,
            truncated,
        });
    }
    out
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
pub fn scan_storage(repo_path: &str) -> Result<StorageReport, String> {
    let started = Instant::now();
    let repo = validate_repo(repo_path)?;
    let resolved = crate::engine::git_cli::resolve_repo(repo_path)?;
    let git_dir = resolve_git_common_dir(&repo)?;

    let mut budget = WalkBudget::new();

    // ---- Worktree walk: totals, artifacts, large files -------------------
    let mut collector = ArtifactCollector::new();
    let mut worktree_bytes = 0u64;
    let mut large_files: Vec<LargeFile> = Vec::new();

    if !resolved.is_bare {
        let mut entered_depths: Vec<(usize, usize)> = Vec::new();
        worktree_bytes = walk_worktree(
            &repo,
            &repo,
            0,
            &mut budget,
            &mut collector,
            &mut entered_depths,
        );
        large_files = collect_large_files(&repo, &mut budget, &[".git"]);
    }

    // ---- Object store: authoritative counters ----------------------------
    let (loose_objects, loose_kib, pack_files, pack_kib) =
        match git_text(&repo, &["count-objects", "-v"]) {
            Ok(text) => parse_count_objects(&text),
            Err(_) => (0, 0, 0, 0),
        };

    // ---- Git directory walk ----------------------------------------------
    let (git_dir_bytes, git_truncated) = size_tree(&git_dir, &mut budget);
    let refs_bytes = git_subdir_bytes(&git_dir, "refs", &mut budget);
    let reflog_bytes = git_subdir_bytes(&git_dir, "logs", &mut budget);
    let lfs_bytes = git_subdir_bytes(&git_dir, "lfs", &mut budget);
    let modules_bytes = git_subdir_bytes(&git_dir, "modules", &mut budget);
    let worktrees_admin_bytes = git_subdir_bytes(&git_dir, "worktrees", &mut budget);
    let index_bytes = fs::symlink_metadata(git_dir.join("index"))
        .map(|m| m.len())
        .unwrap_or(0);

    let pack_bytes = pack_kib.saturating_mul(1024);
    let loose_bytes = loose_kib.saturating_mul(1024);
    let accounted = refs_bytes
        .saturating_add(reflog_bytes)
        .saturating_add(lfs_bytes)
        .saturating_add(modules_bytes)
        .saturating_add(worktrees_admin_bytes)
        .saturating_add(index_bytes);
    // `other` absorbs accounting skew between git's KiB-rounded counters and
    // the raw walk (pack headers, tmp objects) — saturating keeps it ≥ 0.
    let other_bytes = git_dir_bytes.saturating_sub(accounted);

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
    let worktrees = collect_worktree_usage(&repo, &mut budget);
    let branches = branch_summary(repo_path);

    // Category totals can exceed the walked tree only through budget skew;
    // clamp so the UI's stacked bar can never overflow the total.
    let build_total = collector.build_total.min(worktree_bytes);
    let cache_total = collector.cache_total.min(worktree_bytes);
    let grand = worktree_bytes.saturating_add(git_dir_bytes);

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
        scan: ScanStats {
            elapsed_ms: started.elapsed().as_millis(),
            files_visited: budget.files_visited,
            permission_denied: budget.permission_denied,
            truncated: budget.truncated || git_truncated,
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

    #[test]
    fn artifact_kind_classifies_known_ecosystems() {
        assert_eq!(artifact_kind("node_modules"), Some(ArtifactKind::Build));
        assert_eq!(artifact_kind("__pycache__"), Some(ArtifactKind::Cache));
        assert_eq!(artifact_kind(".next"), Some(ArtifactKind::Build));
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
        let total = walk_worktree(root, root, 0, &mut budget, &mut collector, &mut entered);
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
}
