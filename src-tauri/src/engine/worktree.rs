//! Linked-worktree support.
//!
//! AI coding agents parallelize by checking a task out into its own worktree,
//! so a client that only understands "the repository" breaks down the moment
//! an agent's workflow starts. This module lists every worktree of a
//! repository — including the main checkout and bare entries — and creates and
//! removes them through the same validated, harness-gated paths as every other
//! write.

use crate::engine::git_cli::{git_text, resolve_git_common_dir, validate_repo};
use crate::engine::git_writer::validate_oid_or_revision;
use crate::engine::git_writer::validate_ref_name;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Canonical address of one checkout inside a linked-worktree family.
///
/// `anchor` is the primary worktree (the first entry in Git's porcelain
/// listing), which owns repository-wide `.devcouncil` state. `worktree` is the
/// actual checkout an operation targets. Keeping both prevents two opposite
/// mistakes: splitting one repository's ledger between sibling directories,
/// and erasing which checkout an action happened in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeFamily {
    pub anchor: std::path::PathBuf,
    pub worktree: std::path::PathBuf,
    /// Every active checkout authenticated by Git for this common directory.
    /// Callers use this to consolidate pre-family state without scanning or
    /// trusting arbitrary sibling paths from the filesystem.
    pub members: Vec<std::path::PathBuf>,
}

/// Resolves and authenticates a repository/worktree pair.
///
/// A common git directory is necessary but not sufficient: a directory with a
/// hand-written gitfile can point at another checkout's git directory without
/// being a registered worktree. The target must also appear in `git worktree
/// list`, and both paths must resolve to the same common directory.
pub fn resolve_worktree_family(
    repo_path: &str,
    worktree_path: &str,
) -> Result<WorktreeFamily, String> {
    let repo = validate_repo(repo_path)?;
    let worktree = if Path::new(worktree_path) == repo {
        repo.clone()
    } else {
        validate_repo(worktree_path)?
    };

    let repo_common = resolve_git_common_dir(&repo)?;
    let worktree_common = if worktree == repo {
        repo_common.clone()
    } else {
        resolve_git_common_dir(&worktree)?
    };
    if repo_common != worktree_common {
        return Err(format!(
            "Worktree '{}' does not belong to repository '{}'",
            worktree.display(),
            repo.display()
        ));
    }

    // `-z` makes paths containing newlines unambiguous. IPC repository paths
    // are UTF-8 strings, so an unrepresentable list entry cannot equal either
    // validated input and is safely ignored.
    let raw = git_text(&repo, &["worktree", "list", "--porcelain", "-z"])?;
    let mut registered = Vec::new();
    for field in raw.split('\0') {
        let Some(path) = field.strip_prefix("worktree ") else {
            continue;
        };
        let Ok(canonical) = std::fs::canonicalize(path) else {
            // Prunable entries are history, not active authority targets.
            continue;
        };
        registered.push(canonical);
    }

    let Some(anchor) = registered.first().cloned() else {
        return Err("Git returned no active worktrees for this repository".to_string());
    };
    if !registered.iter().any(|path| path == &repo) {
        return Err(format!(
            "Repository '{}' is not a registered worktree",
            repo.display()
        ));
    }
    if !registered.iter().any(|path| path == &worktree) {
        return Err(format!(
            "Worktree '{}' is not registered with repository '{}'",
            worktree.display(),
            repo.display()
        ));
    }

    let anchor_common = if anchor == repo {
        repo_common.clone()
    } else if anchor == worktree {
        worktree_common.clone()
    } else {
        resolve_git_common_dir(&anchor)?
    };
    if anchor_common != repo_common {
        return Err("Git's primary worktree belongs to a different repository".to_string());
    }

    Ok(WorktreeFamily {
        anchor,
        worktree,
        members: registered,
    })
}

/// How many worktrees get a dirty-file scan when listing. Worktrees number in
/// the low dozens even under heavy agent use; this cap keeps a pathological
/// tree from turning one listing call into hundreds of subprocess spawns.
const MAX_DIRTY_SCANS: usize = 32;

/// Worktrees one listing scans at a time. Each scan is three to five git
/// children; an unbounded parallel walk put all 32 scans' children in the
/// spawn gate's queue at once, ahead of everything else the app was asking.
const DIRTY_SCAN_FAN_OUT: usize = 4;

use crate::engine::cow_clone::reflink_ignored_caches;
use crate::engine::portless::{detect_worktree_routes, WorktreeRouteInfo};
use crate::engine::worktree_hooks::{execute_worktree_hooks, load_worktree_hooks};

/// Diff stats for uncommitted changes in a worktree (`HEAD±`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeDiffStat {
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
}

/// Divergence stats between worktree branch and default/main branch (`main↕`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeDivergence {
    pub ahead: usize,
    pub behind: usize,
}

/// One entry of `git worktree list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeInfo {
    /// Absolute path of the worktree directory.
    pub path: String,
    /// Directory name, for display where the full path does not fit.
    pub name: String,
    pub head: String,
    /// Short branch name, `None` when detached or bare.
    pub branch: Option<String>,
    pub is_bare: bool,
    pub is_detached: bool,
    /// True for the repository's primary worktree (listed first by git).
    pub is_main: bool,
    pub is_locked: bool,
    pub is_prunable: bool,
    /// Working-tree change count from `git status`; `None` when not
    /// scanned — `scan_note` says why.
    pub dirty_files: Option<usize>,
    /// Uncommitted line insertions/deletions diff stats.
    #[serde(default)]
    pub diff_stat: Option<WorktreeDiffStat>,
    /// Ahead/behind divergence against default branch.
    #[serde(default)]
    pub main_divergence: Option<WorktreeDivergence>,
    /// Active portless or dev server routes for this worktree.
    #[serde(default)]
    pub active_routes: Vec<WorktreeRouteInfo>,
    /// Why this entry's measurements are absent or partial: a bare entry,
    /// one past the scan cap, a missing directory, or a read that failed or
    /// was deferred under load. `None` when every measurement was taken, so
    /// an absent count never reads the same as a clean one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan_note: Option<String>,
}

/// One parsed block of `worktree list --porcelain`, before dirty counting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ParsedWorktree {
    path: String,
    head: String,
    branch: Option<String>,
    is_bare: bool,
    is_detached: bool,
    is_locked: bool,
    is_prunable: bool,
}

/// Parses `git worktree list --porcelain` output.
///
/// The format is newline-separated fields in blank-line-delimited blocks:
/// `worktree <path>`, `HEAD <sha>`, one of `branch <ref>` / `detached`,
/// optionally `bare`, `locked [reason]`, `prunable [reason]`. Unknown lines are
/// skipped rather than rejected so a newer git can add fields without breaking
/// this parser.
fn parse_worktree_porcelain(raw: &str) -> Vec<ParsedWorktree> {
    let mut out: Vec<ParsedWorktree> = Vec::new();
    let mut current: Option<ParsedWorktree> = None;
    let flush = |current: &mut Option<ParsedWorktree>, out: &mut Vec<ParsedWorktree>| {
        if let Some(entry) = current.take() {
            if !entry.path.is_empty() {
                out.push(entry);
            }
        }
    };

    for line in raw.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            flush(&mut current, &mut out);
            continue;
        }
        let Some((key, value)) = line.split_once(' ') else {
            // A field with no payload ("detached" never appears alone, but a
            // bare "locked" or "prunable" can).
            match line {
                "detached" => {
                    if let Some(entry) = current.as_mut() {
                        entry.is_detached = true;
                    }
                }
                // `bare` carries no payload either.
                "bare" => {
                    if let Some(entry) = current.as_mut() {
                        entry.is_bare = true;
                    }
                }
                "locked" => {
                    if let Some(entry) = current.as_mut() {
                        entry.is_locked = true;
                    }
                }
                "prunable" => {
                    if let Some(entry) = current.as_mut() {
                        entry.is_prunable = true;
                    }
                }
                _ => {}
            }
            continue;
        };
        match key {
            "worktree" => {
                flush(&mut current, &mut out);
                current = Some(ParsedWorktree {
                    path: value.to_string(),
                    ..ParsedWorktree::default()
                });
            }
            "HEAD" => {
                if let Some(entry) = current.as_mut() {
                    entry.head = value.to_string();
                }
            }
            "branch" => {
                if let Some(entry) = current.as_mut() {
                    entry.branch = Some(value.trim_start_matches("refs/heads/").to_string());
                }
            }
            "bare" => {
                if let Some(entry) = current.as_mut() {
                    entry.is_bare = true;
                }
            }
            "locked" => {
                if let Some(entry) = current.as_mut() {
                    entry.is_locked = true;
                }
            }
            "prunable" => {
                if let Some(entry) = current.as_mut() {
                    entry.is_prunable = true;
                }
            }
            _ => {}
        }
    }
    flush(&mut current, &mut out);
    out
}

/// Counts entries in `git status --porcelain -z` output.
///
/// Rename/copy records carry two NUL-separated fields (new path, then origin);
/// both belong to one entry.
fn count_status_entries(bytes: &[u8]) -> usize {
    let mut count = 0usize;
    let mut i = 0usize;
    while i + 3 <= bytes.len() {
        let x = bytes[i] as char;
        let y = bytes[i + 1] as char;
        i += 3; // XY plus separator
        let end = match bytes[i..].iter().position(|&b| b == 0) {
            Some(n) => i + n,
            None => break,
        };
        i = end + 1;
        count += 1;
        if x == 'R' || x == 'C' || y == 'R' || y == 'C' {
            // Skip the paired original-path record.
            match bytes[i..].iter().position(|&b| b == 0) {
                Some(n) => i += n + 1,
                None => break,
            }
        }
    }
    count
}

fn display_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| path.to_string())
}

/// The directory name an agent nests its sessions under.
const WORKTREES_SEGMENT: &str = "worktrees";

/// A matched agent layout: the tool, and the session directory under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLayout {
    /// The hidden directory's name, without the leading dot. Never empty.
    pub kind: String,
    /// The session directory under `worktrees`, or empty when the path stops
    /// at the container itself. Empty means "this path names no session",
    /// never a session whose name could not be read.
    pub slug: String,
}

/// The agent's name when `segment` is the hidden directory one nests
/// worktrees under, else `None`.
///
/// Rejected, in order:
///
/// * anything not starting with a dot — an ordinary directory;
/// * a bare `.`, and anything starting with `..` — those are traversal, not
///   directory names, and no agent is called `.foo`. Without this,
///   `/repo/../worktrees/x` reported an agent of kind `.`;
/// * `git` in any case. On the case-insensitive volumes macOS and Windows
///   ship by default, `.GIT/worktrees` IS git's own metadata store; an
///   earlier case-sensitive comparison let `/repo/.GIT/worktrees` through as
///   an agent of kind `GIT`.
fn agent_directory_name(segment: &str) -> Option<&str> {
    let name = segment.strip_prefix('.')?;
    if name.is_empty() || name.starts_with('.') || name.eq_ignore_ascii_case("git") {
        return None;
    }
    Some(name)
}

/// The agent layout this path sits in, or `None` when it is not an agent
/// worktree.
///
/// Coding agents isolate a task under `<repo>/.<agent>/worktrees/<slug>`.
/// Detection is from directory layout, never from a branch name — a human can
/// name a branch `claude/…`. Git's own `.git/worktrees/` metadata is the same
/// shape and is excluded: that path is not a checkout.
///
/// The scan runs left to right over separator-normalised segments, so the
/// outermost layout wins and **the slug is always read from the match that
/// named the kind**. Deriving the two separately is what let an ancestor
/// directory called `worktrees` capture the slug: in
/// `/Users/me/worktrees/app/.claude/worktrees/session-abc` the slug came back
/// as `app`, collapsing every session under such a parent to one label.
///
/// Held with the frontend copy of this rule (`agentLayout` in
/// `src/lib/work/agentWorktree.ts`) to the shared corpus in
/// `src/lib/work/agentWorktree.cases.json`.
pub fn agent_layout(path: &str) -> Option<AgentLayout> {
    if path.is_empty() {
        return None;
    }
    // Splitting on both separators and dropping empties normalises Windows
    // paths and collapses `//` and any trailing slash in one pass.
    let segments: Vec<&str> = path
        .split(['/', '\\'])
        .filter(|segment| !segment.is_empty())
        .collect();
    for i in 0..segments.len().saturating_sub(1) {
        let Some(kind) = agent_directory_name(segments[i]) else {
            continue;
        };
        if segments[i + 1] != WORKTREES_SEGMENT {
            continue;
        }
        return Some(AgentLayout {
            kind: kind.to_string(),
            slug: segments.get(i + 2).copied().unwrap_or_default().to_string(),
        });
    }
    None
}

/// The agent that created this worktree (`claude`, `cursor`, `codex`, `grok`, `agy`, …).
///
/// `None` when the path is not an agent worktree. See [`agent_layout`].
pub fn agent_kind(path: &str) -> Option<String> {
    agent_layout(path).map(|layout| layout.kind)
}

/// The session slug when `path` is an agent worktree.
///
/// Claude Code appends a short hash so concurrent sessions on the same task
/// stay distinct. The whole segment is returned rather than a prettified
/// prefix — trimming it would merge two sessions in the reader's eye.
///
/// `None` covers both "not an agent worktree" and "the path names the
/// container rather than a session"; every caller renders the two the same
/// way, as no session name to show.
pub fn agent_session_slug(path: &str) -> Option<String> {
    agent_layout(path)
        .map(|layout| layout.slug)
        .filter(|slug| !slug.is_empty())
}

/// Ceiling on paths returned by [`changed_paths`]. Collision detection only
/// needs identity, and a worktree that dirtied tens of thousands of files
/// must not turn one insights call into an unbounded allocation.
pub const MAX_CHANGED_PATHS: usize = 256;

/// Repo-relative paths with uncommitted changes in this worktree.
///
/// Porcelain only — no numstat — so collision scans stay cheap enough to run
/// across many worktrees. Past [`MAX_CHANGED_PATHS`] the vector is cut and
/// the caller sees `truncated`.
pub fn changed_paths(worktree_path: &str) -> Result<(Vec<String>, bool), String> {
    let repo = validate_repo(worktree_path)?;
    if !repo.is_dir() {
        return Err(format!(
            "worktree path is not a directory: {}",
            repo.display()
        ));
    }
    let stdout = git_text(&repo, &["status", "--porcelain", "-z"])?;
    let mut paths = status_paths(stdout.as_bytes());
    let truncated = paths.len() > MAX_CHANGED_PATHS;
    if truncated {
        paths.truncate(MAX_CHANGED_PATHS);
    }
    Ok((paths, truncated))
}

/// Paths from `git status --porcelain -z`, including the origin of a rename.
fn status_paths(bytes: &[u8]) -> Vec<String> {
    let mut paths = Vec::new();
    let mut i = 0usize;
    while i + 3 <= bytes.len() {
        let x = bytes[i] as char;
        let y = bytes[i + 1] as char;
        i += 3;
        let end = match bytes[i..].iter().position(|&b| b == 0) {
            Some(n) => i + n,
            None => break,
        };
        let path = String::from_utf8_lossy(&bytes[i..end]).into_owned();
        i = end + 1;
        if !path.is_empty() {
            paths.push(path);
        }
        if x == 'R' || x == 'C' || y == 'R' || y == 'C' {
            match bytes[i..].iter().position(|&b| b == 0) {
                Some(n) => {
                    let origin = String::from_utf8_lossy(&bytes[i..i + n]).into_owned();
                    i += n + 1;
                    if !origin.is_empty() {
                        paths.push(origin);
                    }
                }
                None => break,
            }
        }
    }
    paths
}

/// Lists every worktree of the repository without scanning any of them.
///
/// Exactly one `git` spawn, whatever the worktree count. [`list_worktrees`]
/// additionally runs `git status` in up to [`MAX_DIRTY_SCANS`] worktrees,
/// which is right for the Work view of ONE repository and wrong for a sweep
/// over a whole workspace: twenty-four repositories with a dozen agent
/// worktrees each is several hundred subprocesses for a column of counts.
///
/// Every entry comes back with `dirty_files: None`, which already means "not
/// scanned" in this type — so a caller cannot mistake an unscanned worktree
/// for a clean one.
pub fn list_worktrees_lite(repo_path: &str) -> Result<Vec<WorktreeInfo>, String> {
    let repo = validate_repo(repo_path)?;
    let stdout = git_text(&repo, &["worktree", "list", "--porcelain"])?;
    Ok(parse_worktree_porcelain(&stdout)
        .into_iter()
        .enumerate()
        .map(|(idx, entry)| WorktreeInfo {
            name: display_name(&entry.path),
            is_main: idx == 0,
            dirty_files: None,
            diff_stat: None,
            main_divergence: None,
            active_routes: Vec::new(),
            scan_note: None,
            path: entry.path,
            head: entry.head,
            branch: entry.branch,
            is_bare: entry.is_bare,
            is_detached: entry.is_detached,
            is_locked: entry.is_locked,
            is_prunable: entry.is_prunable,
        })
        .collect())
}

/// Measures uncommitted line insertions/deletions in a worktree.
pub fn measure_diff_stat(dir: &Path) -> Option<WorktreeDiffStat> {
    let stdout = git_text(dir, &["diff", "HEAD", "--shortstat"]).ok()?;
    parse_shortstat(&stdout)
}

pub fn parse_shortstat(text: &str) -> Option<WorktreeDiffStat> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Some(WorktreeDiffStat::default());
    }
    let mut stat = WorktreeDiffStat::default();
    for part in trimmed.split(',') {
        let p = part.trim();
        if p.contains("file") {
            if let Some(num) = p.split_whitespace().next().and_then(|s| s.parse().ok()) {
                stat.files_changed = num;
            }
        } else if p.contains("insertion") {
            if let Some(num) = p.split_whitespace().next().and_then(|s| s.parse().ok()) {
                stat.insertions = num;
            }
        } else if p.contains("deletion") {
            if let Some(num) = p.split_whitespace().next().and_then(|s| s.parse().ok()) {
                stat.deletions = num;
            }
        }
    }
    Some(stat)
}

/// Measures commit divergence (ahead/behind count) compared to default branch (`main` or `master`).
pub fn measure_main_divergence(repo: &Path, branch: Option<&str>) -> Option<WorktreeDivergence> {
    let b = branch?;
    if b == "main" || b == "master" {
        return Some(WorktreeDivergence {
            ahead: 0,
            behind: 0,
        });
    }
    let default_ref = if git_text(repo, &["rev-parse", "--verify", "refs/heads/main"]).is_ok() {
        "main"
    } else if git_text(repo, &["rev-parse", "--verify", "refs/heads/master"]).is_ok() {
        "master"
    } else {
        return None;
    };

    let stdout = git_text(
        repo,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{default_ref}...{b}"),
        ],
    )
    .ok()?;
    let mut parts = stdout.split_whitespace();
    let behind = parts.next()?.parse().ok()?;
    let ahead = parts.next()?.parse().ok()?;
    Some(WorktreeDivergence { ahead, behind })
}

/// Lists every worktree of the repository, main entry first, with dirty-file
/// counts, diff deltas, and detected portless routes for the worktrees closest to the front.
pub fn list_worktrees(repo_path: &str) -> Result<Vec<WorktreeInfo>, String> {
    let repo = validate_repo(repo_path)?;
    let stdout = git_text(&repo, &["worktree", "list", "--porcelain"])?;
    let parsed = parse_worktree_porcelain(&stdout);

    let scan_targets: Vec<usize> = parsed
        .iter()
        .enumerate()
        .filter(|(_, entry)| !entry.is_bare)
        .map(|(idx, _)| idx)
        .take(MAX_DIRTY_SCANS)
        .collect();

    // Scoped threads in bounded waves rather than rayon's pool: a pool thread
    // starts in the default admission class, so the scans of a listing asked
    // for as background work were promoted to refresh traffic.
    let class = crate::engine::git_cli::current_admission();
    let mut metrics: HashMap<usize, WorktreeScan> = HashMap::new();
    for wave in scan_targets.chunks(DIRTY_SCAN_FAN_OUT) {
        std::thread::scope(|scope| {
            let handles: Vec<_> = wave
                .iter()
                .map(|&idx| {
                    let entry = &parsed[idx];
                    let repo = &repo;
                    let handle = scope.spawn(move || {
                        crate::engine::git_cli::with_admission(class, || scan_worktree(repo, entry))
                    });
                    (idx, handle)
                })
                .collect();
            for (idx, handle) in handles {
                let scan = handle.join().unwrap_or_else(|_| WorktreeScan {
                    note: Some("the scan panicked; nothing was measured".into()),
                    ..WorktreeScan::default()
                });
                metrics.insert(idx, scan);
            }
        });
    }

    Ok(parsed
        .into_iter()
        .enumerate()
        .map(|(idx, entry)| {
            let scan = metrics.remove(&idx).unwrap_or_else(|| WorktreeScan {
                note: Some(if entry.is_bare {
                    "bare entry: no working tree to scan".to_string()
                } else {
                    format!("not scanned: past the {MAX_DIRTY_SCANS}-worktree scan limit")
                }),
                ..WorktreeScan::default()
            });
            WorktreeInfo {
                name: display_name(&entry.path),
                is_main: idx == 0,
                dirty_files: scan.dirty,
                diff_stat: scan.diff_stat,
                main_divergence: scan.divergence,
                active_routes: scan.routes,
                scan_note: scan.note,
                path: entry.path,
                head: entry.head,
                branch: entry.branch,
                is_bare: entry.is_bare,
                is_detached: entry.is_detached,
                is_locked: entry.is_locked,
                is_prunable: entry.is_prunable,
            }
        })
        .collect())
}

/// What one worktree's scan measured, and why anything is missing.
#[derive(Default)]
struct WorktreeScan {
    dirty: Option<usize>,
    diff_stat: Option<WorktreeDiffStat>,
    divergence: Option<WorktreeDivergence>,
    routes: Vec<WorktreeRouteInfo>,
    note: Option<String>,
}

fn scan_worktree(repo: &Path, entry: &ParsedWorktree) -> WorktreeScan {
    let dir = Path::new(&entry.path);
    if !dir.is_dir() {
        return WorktreeScan {
            note: Some("the worktree directory is missing".into()),
            ..WorktreeScan::default()
        };
    }
    // The measurements below swallow their own errors as "absent"; the
    // failure counter is what says whether one was a refusal rather than a
    // real absence (no upstream, no main branch).
    let failures = crate::engine::git_cli::process_failures();
    let status = match git_text(dir, &["status", "--porcelain", "-z"]) {
        Ok(status) => status,
        Err(reason) => {
            return WorktreeScan {
                note: Some(format!("status could not be read: {reason}")),
                ..WorktreeScan::default()
            }
        }
    };
    let mut scan = WorktreeScan {
        dirty: Some(count_status_entries(status.as_bytes())),
        diff_stat: measure_diff_stat(dir),
        divergence: measure_main_divergence(repo, entry.branch.as_deref()),
        routes: detect_worktree_routes(&entry.path, entry.branch.as_deref()),
        note: None,
    };
    if crate::engine::git_cli::process_failures() != failures {
        scan.note = Some(format!(
            "some measurements were not taken: {}",
            crate::engine::git_cli::last_process_failure()
                .unwrap_or_else(|| "a git read produced no answer".into())
        ));
    }
    scan
}

use std::collections::HashMap;

fn validate_target_path(target: &str) -> Result<(), String> {
    if target.is_empty() || target.contains('\0') || target.starts_with('-') {
        return Err("Invalid worktree path".into());
    }
    if !Path::new(target).is_absolute() {
        return Err("Worktree path must be absolute".into());
    }
    Ok(())
}

use crate::engine::git_writer::repo_mutation_lock;

/// Creates a linked worktree. Exactly one of `new_branch` / `detach` shapes the
/// checkout; `start_point` may name any commit-ish.
pub fn add_worktree(
    repo_path: &str,
    target_path: &str,
    new_branch: Option<&str>,
    start_point: Option<&str>,
    detach: bool,
) -> Result<String, String> {
    let repo = validate_repo(repo_path)?;
    let _repo_lock = repo_mutation_lock(&repo);
    let _guard = _repo_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    validate_target_path(target_path)?;
    if let Some(branch) = new_branch {
        validate_ref_name(branch)?;
    }
    if let Some(start) = start_point {
        validate_oid_or_revision(start)?;
    }

    let mut args: Vec<&str> = vec!["worktree", "add"];
    if let Some(branch) = new_branch {
        args.push("-b");
        args.push(branch);
    } else if detach {
        args.push("--detach");
    }
    args.push(target_path);
    if let Some(start) = start_point {
        args.push(start);
    }
    git_text(&repo, &args)?;
    Ok(target_path.to_string())
}

/// A worktree that exists, and what of its optional setup did not happen.
///
/// The worktree itself is the result: once `git worktree add` succeeded it is
/// on disk, so a later step failing must not read as "not created". But it
/// must not read as "fully set up" either — both errors were discarded, so a
/// `post_create` hook that failed (dependencies never installed) left a tree
/// that looked ready and was not.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeCreated {
    pub path: String,
    /// Copying build caches from the main checkout failed as a whole.
    /// Individual cache directories that fail are logged and skipped there.
    pub cache_error: Option<String>,
    /// A `post_create` hook failed, timed out, or was refused.
    pub hook_error: Option<String>,
}

/// Creates a linked worktree with optional Copy-on-Write cache cloning and lifecycle hooks.
pub fn add_worktree_extended(
    repo_path: &str,
    target_path: &str,
    new_branch: Option<&str>,
    start_point: Option<&str>,
    detach: bool,
    cow_caches: bool,
) -> Result<WorktreeCreated, String> {
    let path = add_worktree(repo_path, target_path, new_branch, start_point, detach)?;
    let mut created = WorktreeCreated {
        path,
        cache_error: None,
        hook_error: None,
    };
    if cow_caches {
        let repo = validate_repo(repo_path)?;
        let target = Path::new(target_path);
        created.cache_error = reflink_ignored_caches(&repo, target).err();
        let hooks = load_worktree_hooks(&repo);
        let branch_name = new_branch.unwrap_or("");
        created.hook_error = execute_worktree_hooks(
            &repo,
            target,
            &hooks.post_create,
            "post_create",
            &[
                ("GITPULSE_BRANCH", branch_name),
                ("GITPULSE_WORKTREE_PATH", target_path),
            ],
        )
        .err();
    }
    Ok(created)
}

/// Outcome of merging a worktree branch and tearing down the worktree directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MergeTeardownResult {
    pub merged_branch: String,
    pub target_branch: String,
    pub commits_merged: usize,
    pub worktree_removed: bool,
    pub branch_deleted: bool,
    /// A `post_merge` hook failed, timed out, or was refused. The merge and
    /// the teardown had already happened; this says the cleanup after them
    /// did not, where it used to be discarded.
    pub hook_error: Option<String>,
}

/// Merges a worktree branch into target_branch (defaulting to main/master) in the primary
/// repository checkout, tears down the worktree cleanly, and prunes the merged branch.
///
/// `gate` is asked about each git command before it runs, with git's
/// arguments as they actually run (no leading `git`): the merge of *this
/// worktree's* branch, the squash commit, the forced worktree removal and the
/// branch deletion. The caller used to ask once, up front, about
/// `merge --ff-only <target>` — a command this never ran, and spelled without
/// the program, so it was judged as a program named `merge` — and the three
/// later mutations were not asked about at all.
pub fn merge_and_teardown_worktree(
    repo_path: &str,
    worktree_path: &str,
    target_branch: Option<&str>,
    squash: bool,
    gate: &mut dyn FnMut(&[&str]) -> Result<(), String>,
) -> Result<MergeTeardownResult, String> {
    let family = resolve_worktree_family(repo_path, worktree_path)?;
    if family.worktree == family.anchor {
        return Err("Cannot merge and teardown the repository's primary checkout".into());
    }

    let repo = family.anchor.clone();
    let worktree = family.worktree.clone();

    // 1. Resolve branch name
    let branch_raw = git_text(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let branch = branch_raw.trim().to_string();
    if branch.is_empty() || branch == "HEAD" {
        return Err("Worktree has a detached HEAD; cannot merge an unbranched checkout".into());
    }

    let default_target = if git_text(&repo, &["rev-parse", "--verify", "refs/heads/main"]).is_ok() {
        "main"
    } else if git_text(&repo, &["rev-parse", "--verify", "refs/heads/master"]).is_ok() {
        "master"
    } else {
        "main"
    };
    let target = target_branch.unwrap_or(default_target);
    validate_ref_name(target)?;

    // The merge runs in the main checkout, onto whatever it has checked out.
    // It used to run there regardless and then report `target`: a main
    // checkout on another branch took the merge, and the panel said "main".
    let current = git_text(&repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .map(|head| head.trim().to_string())
        .unwrap_or_default();
    if current != target {
        return Err(format!(
            "The main checkout is on {}, not {target}. Switch it to {target} first; nothing was merged.",
            if current.is_empty() { "a detached HEAD".to_string() } else { current }
        ));
    }

    // 2. Pre-merge hooks
    let hooks = load_worktree_hooks(&repo);
    execute_worktree_hooks(
        &repo,
        &worktree,
        &hooks.pre_merge,
        "pre_merge",
        &[
            ("GITPULSE_BRANCH", &branch),
            ("GITPULSE_WORKTREE_PATH", &worktree.to_string_lossy()),
        ],
    )?;

    // 3. Collision / uncommitted changes check
    let (changed, _) = changed_paths(&worktree.to_string_lossy())?;
    if !changed.is_empty() {
        return Err(format!(
            "Worktree has {} uncommitted file changes; commit or stash them before merging",
            changed.len()
        ));
    }

    // Count commits being merged
    let count_stdout = git_text(
        &repo,
        &["rev-list", "--count", &format!("{target}..{branch}")],
    )
    .unwrap_or_else(|_| "0".into());
    let commits_merged: usize = count_stdout.trim().parse().unwrap_or(0);

    // 4. Merge in anchor repository
    {
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let merge = merge_teardown_argv(&branch, squash);
        let merge: Vec<&str> = merge.iter().map(String::as_str).collect();
        gate(&merge)?;
        if squash {
            git_text(&repo, &merge)?;
            // `git merge` refuses to start over staged changes, so whatever is
            // staged now is the squash. Nothing staged means the branch's
            // changes are already on the target and there is nothing to
            // commit — not a failure.
            let staged = git_text(&repo, &["diff", "--cached", "--name-only"])?;
            if !staged.trim().is_empty() {
                let commit_msg = format!("Merge branch '{branch}' (squashed)");
                let commit = ["commit", "-m", commit_msg.as_str()];
                gate(&commit)?;
                // Fail closed before anything is torn down. This was
                // discarded, and the next steps removed the worktree and
                // force-deleted the branch whose work was never committed.
                git_text(&repo, &commit).map_err(|e| {
                    format!(
                        "The squashed changes of {branch} are staged on {target} but could not be committed: {e}. \
                         The worktree and the branch were kept. Commit the staged changes, or undo them with `git reset --merge`."
                    )
                })?;
            }
        } else if let Err(ff_err) = git_text(&repo, &merge) {
            let commit_msg = format!("Merge branch '{branch}' into {target}");
            let merge = ["merge", branch.as_str(), "-m", commit_msg.as_str()];
            gate(&merge)?;
            git_text(&repo, &merge)
                .map_err(|e| format!("Merge failed (ff error: {ff_err}): {e}"))?;
        }
    }

    // 5. Remove worktree
    let worktree_text = worktree.to_string_lossy().to_string();
    let remove = remove_worktree_argv(&worktree_text, true);
    // That builder spells the program; the gate takes git's arguments.
    let remove: Vec<&str> = remove.iter().skip(1).map(String::as_str).collect();
    gate(&remove)?;
    remove_worktree(repo_path, &worktree_text, true)?;

    // 6. Delete merged branch
    let delete = ["branch", if squash { "-D" } else { "-d" }, branch.as_str()];
    let branch_deleted = gate(&delete).is_ok() && git_text(&repo, &delete).is_ok();

    // 7. Post-merge hooks
    let hook_error = execute_worktree_hooks(
        &repo,
        &repo,
        &hooks.post_merge,
        "post_merge",
        &[
            ("GITPULSE_BRANCH", &branch),
            ("GITPULSE_TARGET_BRANCH", target),
        ],
    )
    .err();

    Ok(MergeTeardownResult {
        merged_branch: branch,
        target_branch: target.to_string(),
        commits_merged,
        worktree_removed: true,
        branch_deleted,
        hook_error,
    })
}

/// Git's arguments for the merge step of [`merge_and_teardown_worktree`].
pub fn merge_teardown_argv(branch: &str, squash: bool) -> Vec<String> {
    if squash {
        vec!["merge".into(), "--squash".into(), branch.into()]
    } else {
        vec!["merge".into(), "--ff-only".into(), branch.into()]
    }
}

/// Removes a linked worktree. Git refuses the main worktree itself.
pub fn remove_worktree(repo_path: &str, target_path: &str, force: bool) -> Result<(), String> {
    let repo = validate_repo(repo_path)?;
    let _repo_lock = repo_mutation_lock(&repo);
    let _guard = _repo_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    validate_target_path(target_path)?;
    // Git scans the target's status internally. Its config.worktree helpers
    // can execute even though the parent is the process's current directory.
    let target = validate_repo(target_path)?;
    let target_path = target.to_str().ok_or("Worktree path is not UTF-8")?;
    let mut args: Vec<&str> = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(target_path);
    git_text(&repo, &args)?;
    Ok(())
}

/// Locks a linked worktree to prevent it from being automatically pruned or removed.
pub fn lock_worktree(
    repo_path: &str,
    target_path: &str,
    reason: Option<&str>,
) -> Result<(), String> {
    let repo = validate_repo(repo_path)?;
    let _repo_lock = repo_mutation_lock(&repo);
    let _guard = _repo_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    validate_target_path(target_path)?;
    let mut args: Vec<&str> = vec!["worktree", "lock"];
    if let Some(r) = reason {
        if !r.trim().is_empty() {
            args.push("--reason");
            args.push(r);
        }
    }
    args.push(target_path);
    git_text(&repo, &args)?;
    Ok(())
}

/// Unlocks a locked linked worktree.
pub fn unlock_worktree(repo_path: &str, target_path: &str) -> Result<(), String> {
    let repo = validate_repo(repo_path)?;
    let _repo_lock = repo_mutation_lock(&repo);
    let _guard = _repo_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    validate_target_path(target_path)?;
    git_text(&repo, &["worktree", "unlock", target_path])?;
    Ok(())
}

/// Prunes stale worktree administrative data where worktree directory no longer exists.
pub fn prune_worktree(repo_path: &str) -> Result<(), String> {
    let repo = validate_repo(repo_path)?;
    let _repo_lock = repo_mutation_lock(&repo);
    let _guard = _repo_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    git_text(&repo, &["worktree", "prune", "--expire", "now"])?;
    Ok(())
}

/// The exact argv [`add_worktree`] would run, built independently so the
/// command gate judges the same command line the writer executes.
pub fn add_worktree_argv(
    target_path: &str,
    new_branch: Option<&str>,
    start_point: Option<&str>,
    detach: bool,
) -> Vec<String> {
    let mut argv: Vec<String> = vec!["git".into(), "worktree".into(), "add".into()];
    if let Some(branch) = new_branch {
        argv.push("-b".into());
        argv.push(branch.into());
    } else if detach {
        argv.push("--detach".into());
    }
    argv.push(target_path.into());
    if let Some(start) = start_point {
        argv.push(start.into());
    }
    argv
}

pub fn remove_worktree_argv(target_path: &str, force: bool) -> Vec<String> {
    let mut argv: Vec<String> = vec!["git".into(), "worktree".into(), "remove".into()];
    if force {
        argv.push("--force".into());
    }
    argv.push(target_path.into());
    argv
}

pub fn lock_worktree_argv(target_path: &str, reason: Option<&str>) -> Vec<String> {
    let mut argv: Vec<String> = vec!["git".into(), "worktree".into(), "lock".into()];
    if let Some(r) = reason {
        if !r.trim().is_empty() {
            argv.push("--reason".into());
            argv.push(r.into());
        }
    }
    argv.push(target_path.into());
    argv
}

pub fn unlock_worktree_argv(target_path: &str) -> Vec<String> {
    vec![
        "git".into(),
        "worktree".into(),
        "unlock".into(),
        target_path.into(),
    ]
}

pub fn prune_worktree_argv() -> Vec<String> {
    vec![
        "git".into(),
        "worktree".into(),
        "prune".into(),
        "--expire".into(),
        "now".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procguard::LockedSpawn;
    use std::path::PathBuf;

    #[test]
    fn test_parse_porcelain_full_blocks() {
        let raw = "\
worktree /repos/main
HEAD abc123
branch refs/heads/main

worktree /repos/task-a
HEAD def456
detached

";
        let parsed = parse_worktree_porcelain(raw);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].path, "/repos/main");
        assert_eq!(parsed[0].head, "abc123");
        assert_eq!(parsed[0].branch.as_deref(), Some("main"));
        assert!(!parsed[0].is_detached);
        assert_eq!(parsed[1].path, "/repos/task-a");
        assert!(parsed[1].is_detached);
        assert!(parsed[1].branch.is_none());
    }

    #[test]
    fn test_parse_porcelain_bare_locked_prunable_and_unknown_fields() {
        let raw = "\
worktree /srv/bare.git
bare

worktree /repos/locked-one
HEAD abc
branch refs/heads/wip
locked reason why
prunable
some-future-field whatever

";
        let parsed = parse_worktree_porcelain(raw);
        assert_eq!(parsed.len(), 2);
        assert!(parsed[0].is_bare);
        assert!(!parsed[0].is_detached);
        assert!(parsed[1].is_locked);
        assert!(parsed[1].is_prunable);
        assert_eq!(parsed[1].branch.as_deref(), Some("wip"));
    }

    #[test]
    fn test_parse_porcelain_empty_input_yields_nothing() {
        assert!(parse_worktree_porcelain("").is_empty());
        assert!(parse_worktree_porcelain("\n\n").is_empty());
    }

    #[test]
    fn test_count_status_entries_counts_renames_once() {
        // M + R (two NUL fields) + untracked
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(b"M  src/a.rs\0");
        bytes.extend_from_slice(b"R  new-name.rs\0old-name.rs\0");
        bytes.extend_from_slice(b"?? notes.txt\0");
        assert_eq!(count_status_entries(&bytes), 3);
    }

    #[test]
    fn test_count_status_entries_empty_and_truncated() {
        assert_eq!(count_status_entries(b""), 0);
        assert_eq!(count_status_entries(b"M"), 0);
        // A record with a structurally valid but empty path still counts:
        // git never emits one, so the parser stays simple rather than
        // second-guessing its own input.
        assert_eq!(count_status_entries(b"M  \0"), 1);
    }

    #[test]
    fn test_parse_porcelain_bare_without_payload() {
        let parsed = parse_worktree_porcelain("worktree /srv/bare.git\nbare\n");
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].is_bare);
    }

    #[test]
    fn test_validate_target_path_rejects_flags_and_relative() {
        // Absolute in the platform's own spelling. `/tmp/wt` is absolute on
        // Unix and merely rooted on Windows, where a worktree path with no
        // drive is genuinely ambiguous and the refusal is correct.
        let absolute = if cfg!(windows) {
            "C:\\tmp\\wt"
        } else {
            "/tmp/wt"
        };
        assert!(validate_target_path(absolute).is_ok());
        assert!(validate_target_path("--force").is_err());
        assert!(validate_target_path("relative/path").is_err());
        assert!(validate_target_path("").is_err());
    }

    #[test]
    fn test_add_worktree_argv_matches_writer_shape() {
        let argv = add_worktree_argv("/tmp/wt", Some("agent/task"), Some("main"), false);
        assert_eq!(
            argv,
            vec![
                "git",
                "worktree",
                "add",
                "-b",
                "agent/task",
                "/tmp/wt",
                "main"
            ]
        );
        let detached = add_worktree_argv("/tmp/wt2", None, None, true);
        assert_eq!(
            detached,
            vec!["git", "worktree", "add", "--detach", "/tmp/wt2"]
        );
        let plain = add_worktree_argv("/tmp/wt3", None, None, false);
        assert_eq!(plain, vec!["git", "worktree", "add", "/tmp/wt3"]);
    }

    #[test]
    fn test_display_name_handles_root_paths() {
        assert_eq!(display_name("/repos/main"), "main");
        assert_eq!(display_name("/"), "/");
    }

    #[cfg(test)]
    use crate::test_support::git_in;

    #[test]
    fn test_list_add_remove_roundtrip() {
        let main = tempfile::TempDir::new().unwrap();
        git_in(main.path(), &["init", "-b", "main"]);
        std::fs::write(main.path().join("seed.txt"), "seed").unwrap();
        git_in(main.path(), &["add", "."]);
        git_in(main.path(), &["commit", "-m", "init"]);

        let listed = list_worktrees(main.path().to_str().unwrap()).expect("list main only");
        assert_eq!(listed.len(), 1);
        assert!(listed[0].is_main);
        assert_eq!(listed[0].dirty_files, Some(0));

        let parent = tempfile::TempDir::new().unwrap();
        let wt_path = parent.path().join("agent-task");
        let created = add_worktree(
            main.path().to_str().unwrap(),
            wt_path.to_str().unwrap(),
            Some("agent/task"),
            Some("main"),
            false,
        )
        .expect("add worktree");
        assert_eq!(created, wt_path.to_str().unwrap());
        crate::test_support::trust_repo(&wt_path);

        let listed = list_worktrees(main.path().to_str().unwrap()).expect("list two");
        assert_eq!(listed.len(), 2);
        assert!(!listed[1].is_main);
        assert_eq!(listed[1].branch.as_deref(), Some("agent/task"));
        assert_eq!(listed[1].dirty_files, Some(0));

        remove_worktree(main.path().to_str().unwrap(), created.as_str(), false)
            .expect("remove worktree");
        let listed = list_worktrees(main.path().to_str().unwrap()).expect("list back to one");
        assert_eq!(listed.len(), 1);
    }

    #[test]
    fn test_lock_unlock_and_prune_roundtrip() {
        let main = tempfile::TempDir::new().unwrap();
        git_in(main.path(), &["init", "-b", "main"]);
        std::fs::write(main.path().join("seed.txt"), "seed").unwrap();
        git_in(main.path(), &["add", "."]);
        git_in(main.path(), &["commit", "-m", "init"]);

        let parent = tempfile::TempDir::new().unwrap();
        let wt_path = parent.path().join("locked-task");
        let created = add_worktree(
            main.path().to_str().unwrap(),
            wt_path.to_str().unwrap(),
            Some("agent/locked"),
            Some("main"),
            false,
        )
        .expect("add worktree");

        lock_worktree(
            main.path().to_str().unwrap(),
            created.as_str(),
            Some("agent hold"),
        )
        .expect("lock");
        let listed = list_worktrees(main.path().to_str().unwrap()).expect("list locked");
        let created_canon = Path::new(&created)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(&created));
        assert!(
            listed.iter().any(|w| {
                let listed_canon = Path::new(&w.path)
                    .canonicalize()
                    .unwrap_or_else(|_| PathBuf::from(&w.path));
                listed_canon == created_canon && w.is_locked
            }),
            "locked worktree missing: created={created}, listed={listed:?}"
        );

        unlock_worktree(main.path().to_str().unwrap(), created.as_str()).expect("unlock");
        let listed = list_worktrees(main.path().to_str().unwrap()).expect("list unlocked");
        assert!(
            listed.iter().any(|w| {
                let listed_canon = Path::new(&w.path)
                    .canonicalize()
                    .unwrap_or_else(|_| PathBuf::from(&w.path));
                listed_canon == created_canon && !w.is_locked
            }),
            "unlocked worktree missing: created={created}, listed={listed:?}"
        );

        // Delete the worktree directory out of band; prune must drop the stale
        // administrative entry.
        std::fs::remove_dir_all(&wt_path).unwrap();
        prune_worktree(main.path().to_str().unwrap()).expect("prune");
        let listed = list_worktrees(main.path().to_str().unwrap()).expect("list after prune");
        assert_eq!(listed.len(), 1);
        assert!(listed[0].is_main);
    }

    #[test]
    fn test_lock_unlock_prune_argv_matches_writer_shape() {
        assert_eq!(
            lock_worktree_argv("/tmp/wt", Some("hold")),
            vec!["git", "worktree", "lock", "--reason", "hold", "/tmp/wt"]
        );
        assert_eq!(
            unlock_worktree_argv("/tmp/wt"),
            vec!["git", "worktree", "unlock", "/tmp/wt"]
        );
        assert_eq!(
            prune_worktree_argv(),
            vec!["git", "worktree", "prune", "--expire", "now"]
        );
    }

    #[test]
    fn test_list_worktrees_rejects_non_repo() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(list_worktrees(dir.path().to_str().unwrap()).is_err());
    }

    /// A main checkout with `linked` worktrees under one parent directory.
    fn repo_with_worktrees(linked: usize) -> (tempfile::TempDir, tempfile::TempDir, Vec<PathBuf>) {
        let main = tempfile::TempDir::new().unwrap();
        git_in(main.path(), &["init", "-b", "main"]);
        std::fs::write(main.path().join("seed.txt"), "seed").unwrap();
        git_in(main.path(), &["add", "."]);
        git_in(main.path(), &["commit", "-m", "init"]);
        let parent = tempfile::TempDir::new().unwrap();
        let paths = (0..linked)
            .map(|i| {
                let path = parent.path().join(format!("wt-{i:02}"));
                let branch = format!("agent/{i:02}");
                git_in(
                    main.path(),
                    &[
                        "worktree",
                        "add",
                        "-q",
                        "-b",
                        &branch,
                        path.to_str().unwrap(),
                    ],
                );
                crate::test_support::trust_repo(&path);
                path.canonicalize().unwrap()
            })
            .collect();
        (main, parent, paths)
    }

    /// Every absent count says why, and a scanned one says nothing.
    #[test]
    fn every_unmeasured_worktree_names_its_reason() {
        let (main, _parent, linked) = repo_with_worktrees(3);
        // One directory gone, one whose `.git` link points nowhere.
        std::fs::remove_dir_all(&linked[1]).unwrap();
        std::fs::write(linked[2].join(".git"), "gitdir: /nonexistent/worktree\n").unwrap();
        let listed = list_worktrees(main.path().to_str().unwrap()).expect("list");
        assert_eq!(listed.len(), 4);
        let named = |name: &str| listed.iter().find(|w| w.name == name).expect(name);
        assert_eq!(listed[0].dirty_files, Some(0));
        assert_eq!(listed[0].scan_note, None, "a full scan carries no note");
        assert_eq!(named("wt-00").dirty_files, Some(0));
        assert_eq!(named("wt-00").scan_note, None);
        assert_eq!(named("wt-01").dirty_files, None);
        assert_eq!(
            named("wt-01").scan_note.as_deref(),
            Some("the worktree directory is missing")
        );
        assert_eq!(named("wt-02").dirty_files, None);
        let note = named("wt-02").scan_note.as_deref().unwrap_or_default();
        assert!(note.starts_with("status could not be read: "), "{note}");
        // On the wire an absent note is absent, so older readers see no change.
        let wire = serde_json::to_value(&listed[0]).unwrap();
        assert!(wire.get("scan_note").is_none(), "{wire}");
    }

    /// A status that answered with a diff stat that did not: the count is
    /// kept, and the note says what is missing instead of the stat silently
    /// reading as absent.
    #[test]
    fn a_partly_refused_scan_keeps_what_it_measured_and_names_the_rest() {
        let (main, _parent, _linked) = repo_with_worktrees(0);
        let entry = ParsedWorktree {
            path: main.path().to_str().unwrap().to_string(),
            branch: Some("main".into()),
            ..ParsedWorktree::default()
        };
        let repo = main.path().canonicalize().unwrap();
        let scan = crate::engine::git_cli::with_forced_spawn_failure_of("diff", || {
            scan_worktree(&repo, &entry)
        });
        assert_eq!(scan.dirty, Some(0));
        assert_eq!(scan.diff_stat, None);
        let note = scan.note.unwrap_or_default();
        assert!(
            note.starts_with("some measurements were not taken: "),
            "{note}"
        );
        assert!(note.contains("forced by test"), "{note}");
        let whole = scan_worktree(&repo, &entry);
        assert_eq!(whole.note, None);
        assert!(whole.diff_stat.is_some());
    }

    #[test]
    fn a_worktree_past_the_scan_limit_says_so() {
        let (main, _parent, _linked) = repo_with_worktrees(MAX_DIRTY_SCANS);
        let listed = list_worktrees(main.path().to_str().unwrap()).expect("list");
        assert_eq!(listed.len(), MAX_DIRTY_SCANS + 1);
        let measured = listed.iter().filter(|w| w.dirty_files.is_some()).count();
        assert_eq!(measured, MAX_DIRTY_SCANS);
        let last = listed.last().unwrap();
        assert_eq!(last.dirty_files, None);
        assert_eq!(
            last.scan_note.as_deref(),
            Some("not scanned: past the 32-worktree scan limit")
        );
    }

    /// The scans run in the class the listing was asked for. Under rayon they
    /// ran on pool threads, which start `Reactive`, so a listing asked for as
    /// background work competed as refresh traffic.
    #[test]
    fn worktree_scans_keep_the_callers_admission_class() {
        use crate::engine::git_cli::{spawn_log, with_admission, Admission};
        let (main, _parent, linked) = repo_with_worktrees(6);
        let listed = with_admission(Admission::Background, || {
            list_worktrees(main.path().to_str().unwrap()).expect("list")
        });
        // The directories exactly as the listing spelled them, which is what
        // each scan's children ran in, plus their canonical form.
        let mut dirs: Vec<PathBuf> = listed
            .iter()
            .skip(1)
            .map(|w| PathBuf::from(&w.path))
            .collect();
        dirs.extend(linked.iter().cloned());
        dirs.sort();
        dirs.dedup();
        let mut checked = 0;
        for dir in &dirs {
            for (argv, class) in spawn_log::classed_spawns_in(dir) {
                assert_eq!(
                    class,
                    Admission::Background,
                    "{argv:?} in {}",
                    dir.display()
                );
                checked += 1;
            }
        }
        assert!(
            checked >= linked.len() * 2,
            "only {checked} scan spawns recorded"
        );
    }

    #[test]
    fn agent_kind_matches_the_layout_agents_actually_create() {
        assert_eq!(
            agent_kind("/repo/.claude/worktrees/add-parser-8540d4").as_deref(),
            Some("claude")
        );
        assert_eq!(
            agent_kind("/repo/.cursor/worktrees/fix-auth").as_deref(),
            Some("cursor")
        );
        assert_eq!(
            agent_kind("/repo/.codex/worktrees/session-1").as_deref(),
            Some("codex")
        );
        assert_eq!(
            agent_kind("C:\\Users\\me\\app\\.claude\\worktrees\\slug").as_deref(),
            Some("claude")
        );
    }

    #[test]
    fn agent_kind_never_labels_git_metadata_or_a_branch_name() {
        // `.git/worktrees/` is the same shape and is the one false positive
        // that would put an "agent" chip on every linked worktree git creates.
        assert_eq!(agent_kind("/repo/.git/worktrees/feature"), None);
        assert_eq!(agent_kind("C:\\repo\\.git\\worktrees\\feature"), None);
        assert_eq!(agent_kind("/repo"), None);
        assert_eq!(agent_kind("/repo/worktrees/feature"), None);
        assert_eq!(agent_kind("/repo/wt/claude/my-own-branch"), None);
        assert_eq!(agent_kind("/home/claude/projects/app"), None);
        assert_eq!(agent_kind(""), None);
    }

    #[test]
    fn agent_session_slug_keeps_the_whole_segment() {
        assert_eq!(
            agent_session_slug("/repo/.claude/worktrees/agentic-git-repo-8540d4").as_deref(),
            Some("agentic-git-repo-8540d4")
        );
        assert_eq!(
            agent_session_slug("/repo/.claude/worktrees/slug/src/lib/x.ts").as_deref(),
            Some("slug")
        );
        assert_eq!(
            agent_session_slug("C:\\app\\.claude\\worktrees\\slug\\src").as_deref(),
            Some("slug")
        );
        assert_eq!(agent_session_slug("/repo"), None);
        assert_eq!(agent_session_slug("/repo/.git/worktrees/feature"), None);
        assert_eq!(agent_session_slug("/repo/.claude/worktrees/"), None);
    }

    #[test]
    fn agent_session_slug_is_read_from_the_match_that_named_the_kind() {
        // An ancestor directory called `worktrees` is an ordinary thing for a
        // person to have, and deriving the slug by searching the whole path
        // for `/worktrees/` found that one first: every session under such a
        // parent came back with the same slug, which is exactly the merging
        // the slug exists to prevent.
        assert_eq!(
            agent_session_slug("/Users/me/worktrees/myrepo/.claude/worktrees/session-abc")
                .as_deref(),
            Some("session-abc")
        );
        assert_eq!(
            agent_session_slug("/work/worktrees/.claude/worktrees/a").as_deref(),
            Some("a")
        );
        // A `worktrees` directory BELOW the session must not redirect it either.
        assert_eq!(
            agent_session_slug("/repo/.claude/worktrees/slug/worktrees/other").as_deref(),
            Some("slug")
        );
    }

    #[test]
    fn agent_kind_rejects_git_in_any_case_and_dotted_traversal() {
        // `.GIT` and `.git` are the same directory on the case-insensitive
        // volumes macOS and Windows ship by default. The container spelling
        // (no trailing separator) escaped the old guard and was reported as
        // an agent of kind `GIT`.
        assert_eq!(agent_kind("/repo/.GIT/worktrees"), None);
        assert_eq!(agent_kind("/repo/.GIT/worktrees/feature"), None);
        assert_eq!(agent_kind("/repo/.Git/worktrees/feature"), None);
        assert_eq!(agent_kind("/repo/.gIt/worktrees/x"), None);
        // `.` and `..` are traversal, not directory names. Stripping the
        // leading dot from `..` left `.`, which was accepted as an agent.
        assert_eq!(agent_kind("/repo/../worktrees/x"), None);
        assert_eq!(agent_kind("/repo/..foo/worktrees/x"), None);
        assert_eq!(agent_kind("/repo/.../worktrees/x"), None);
        assert_eq!(agent_kind("/repo/./worktrees/x"), None);
    }

    /// Deterministic LCG: a fuzz run nobody can reproduce is not evidence.
    fn lcg(seed: u32) -> impl FnMut() -> u32 {
        let mut state = seed;
        move || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state
        }
    }

    /// Segments chosen to collide with every branch of the rule.
    const FUZZ_ALPHABET: [&str; 15] = [
        "a",
        "z",
        ".",
        "..",
        ".git",
        ".GIT",
        ".claude",
        "worktrees",
        "Worktrees",
        "",
        " ",
        "-",
        "é",
        "0",
        "..foo",
    ];

    fn fuzz_path(next: &mut impl FnMut() -> u32) -> String {
        let depth = 1 + (next() % 12) as usize;
        let separator = if next().is_multiple_of(2) { '/' } else { '\\' };
        let mut parts: Vec<&str> = Vec::with_capacity(depth);
        for _ in 0..depth {
            parts.push(FUZZ_ALPHABET[(next() as usize) % FUZZ_ALPHABET.len()]);
        }
        let mut path = String::new();
        if next().is_multiple_of(2) {
            path.push(separator);
        }
        path.push_str(&parts.join(&separator.to_string()));
        path
    }

    fn swap_separators(path: &str) -> String {
        if path.contains('\\') {
            path.replace('\\', "/")
        } else {
            path.replace('/', "\\")
        }
    }

    #[test]
    fn agent_layout_holds_its_invariants_under_fuzzing() {
        // The corpus pins the cases we decided about. This attacks the space
        // nobody decided about, and asserts what has to be true of any answer
        // at all: a wrong label here puts an agent chip on a person's own
        // checkout, and a panic takes the whole snapshot with it.
        let mut next = lcg(0x5eed);
        let mut matched = 0usize;
        let mut rejected = 0usize;
        for _ in 0..20_000 {
            let path = fuzz_path(&mut next);
            let layout = agent_layout(&path);
            // The accessors are views of one scan; if they can disagree, a
            // snapshot and the row rendered from it describe different worlds.
            assert_eq!(
                agent_kind(&path),
                layout.as_ref().map(|l| l.kind.clone()),
                "{path:?}"
            );
            assert_eq!(
                agent_session_slug(&path),
                layout
                    .as_ref()
                    .map(|l| l.slug.clone())
                    .filter(|s| !s.is_empty()),
                "{path:?}"
            );
            // Windows and POSIX spellings of one path are one path.
            assert_eq!(agent_layout(&swap_separators(&path)), layout, "{path:?}");
            // Redundant separators are not information.
            let padded = path.replace('/', "///").replace('\\', "\\\\\\");
            assert_eq!(agent_layout(&padded), layout, "{path:?}");

            match layout {
                None => rejected += 1,
                Some(found) => {
                    matched += 1;
                    assert!(!found.kind.is_empty(), "{path:?}");
                    assert!(!found.kind.starts_with('.'), "{path:?}");
                    assert!(!found.kind.eq_ignore_ascii_case("git"), "{path:?}");
                    if found.slug.is_empty() {
                        // The path stopped at the container. Appending to it
                        // NAMES a session, so suffix-independence does not
                        // apply — it only holds once a session exists. The
                        // fuzzer found this by handing over
                        // `-\worktrees\.GIT\.claude\worktrees`, where the
                        // suffix supplies the slug rather than hiding it.
                        assert_eq!(
                            agent_layout(&format!("{path}/named")).map(|l| l.slug),
                            Some("named".to_string()),
                            "{path:?} must take its session name from the suffix"
                        );
                    } else {
                        // A slug is a segment of the path it came from, never
                        // a fragment and never a name borrowed from elsewhere.
                        assert!(
                            path.split(['/', '\\']).any(|s| s == found.slug),
                            "{path:?} produced a slug that is not one of its segments: {:?}",
                            found.slug
                        );
                        // Anything below a session still reports that session.
                        for suffix in ["/src/lib.rs", "/worktrees/other", "/.git/worktrees/z"] {
                            assert_eq!(
                                agent_layout(&format!("{path}{suffix}")),
                                Some(found.clone()),
                                "{path:?} changed its mind because of {suffix:?}"
                            );
                        }
                    }
                }
            }
        }
        // A generator that stopped producing matches would satisfy every
        // assertion above while checking nothing — a check that did not run,
        // reporting the same green as one that did.
        assert!(matched > 100, "the fuzz corpus produced {matched} layouts");
        assert!(rejected > 100, "the fuzz corpus rejected only {rejected}");
    }

    #[test]
    fn agent_layout_stays_linear_on_hostile_input() {
        // A tripwire, not a benchmark. The budget is loose on purpose: a
        // machine under load must not fail this for being slow, but anything
        // that reintroduces quadratic normalisation blows past it by orders
        // of magnitude long before a user's Work view would.
        let hostile = [
            format!("/{}worktrees/slug", ".claude/".repeat(20_000)),
            format!("/{}.claude/worktrees/slug", "a/".repeat(50_000)),
            format!("/repo/{}/worktrees/x", ".".repeat(100_000)),
            format!("/repo/.claude/worktrees/{}", "x".repeat(200_000)),
            format!("/{}.claude/worktrees/s", "/".repeat(200_000)),
        ];
        let started = std::time::Instant::now();
        for _ in 0..20 {
            for path in &hostile {
                let _ = agent_layout(path);
            }
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "hostile paths took {elapsed:?}"
        );
    }

    #[derive(Deserialize)]
    struct LayoutCorpus {
        cases: Vec<LayoutCase>,
    }

    #[derive(Deserialize)]
    struct LayoutCase {
        path: String,
        kind: String,
        slug: String,
        why: String,
    }

    #[test]
    fn agent_layout_matches_the_shared_corpus() {
        // The frontend carries a second implementation of this same rule, for
        // labelling paths the Work view already holds without another IPC
        // round trip. `src/lib/work/agentWorktree.cases.json` is the one
        // corpus both are held to; the TypeScript half runs in
        // `src/lib/work/agentWorktree.contract.test.ts`. Changing either
        // implementation on its own turns one of the two red, which is the
        // whole point of keeping the corpus outside both.
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/lib/work/agentWorktree.cases.json"
        ))
        .expect("reading src/lib/work/agentWorktree.cases.json");
        let corpus: LayoutCorpus =
            serde_json::from_str(&raw).expect("parsing agentWorktree.cases.json");
        assert!(
            corpus.cases.len() >= 40,
            "the shared corpus shrank to {} cases; it is meant to keep growing",
            corpus.cases.len()
        );
        for case in &corpus.cases {
            let (kind, slug) = match agent_layout(&case.path) {
                Some(layout) => (layout.kind, layout.slug),
                None => (String::new(), String::new()),
            };
            assert_eq!(
                (kind.as_str(), slug.as_str()),
                (case.kind.as_str(), case.slug.as_str()),
                "{:?} — {}",
                case.path,
                case.why
            );
            // The two public accessors must agree with the scan they wrap;
            // an accessor that answered differently would put one label on a
            // snapshot and another on the row rendered from it.
            assert_eq!(
                agent_kind(&case.path).unwrap_or_default(),
                case.kind,
                "agent_kind {:?}",
                case.path
            );
            assert_eq!(
                agent_session_slug(&case.path).unwrap_or_default(),
                case.slug,
                "agent_session_slug {:?}",
                case.path
            );
        }
    }

    #[test]
    fn status_paths_reads_porcelain_and_rename_pairs() {
        assert!(status_paths(b"").is_empty());
        assert_eq!(status_paths(b" M src/lib.rs\0"), vec!["src/lib.rs"]);
        assert_eq!(
            status_paths(b"R  new.rs\0old.rs\0"),
            vec!["new.rs", "old.rs"]
        );
    }

    #[test]
    fn test_parse_shortstat_cases() {
        let empty = parse_shortstat("");
        assert_eq!(empty, Some(WorktreeDiffStat::default()));

        let text = " 3 files changed, 25 insertions(+), 4 deletions(-)";
        let stat = parse_shortstat(text).expect("parsed shortstat");
        assert_eq!(stat.files_changed, 3);
        assert_eq!(stat.insertions, 25);
        assert_eq!(stat.deletions, 4);

        let insertions_only = " 1 file changed, 10 insertions(+)";
        let stat2 = parse_shortstat(insertions_only).expect("parsed shortstat");
        assert_eq!(stat2.files_changed, 1);
        assert_eq!(stat2.insertions, 10);
        assert_eq!(stat2.deletions, 0);
    }

    #[test]
    fn test_merge_teardown_argv_shape() {
        let normal = merge_teardown_argv("feature-1", false);
        assert_eq!(normal, vec!["merge", "--ff-only", "feature-1"]);

        let squash = merge_teardown_argv("feature-1", true);
        assert_eq!(squash, vec!["merge", "--squash", "feature-1"]);
    }

    /// A trusted repository on `main` with one commit, and a linked worktree
    /// on `feature-x` holding one more commit. Returns (repo dir, worktree
    /// dir); both are trusted.
    fn teardown_fixture(hooks: Option<&str>) -> (tempfile::TempDir, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path();
        let git = |cwd: &Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output_locked()
                .expect("git");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(repo, &["init", "-b", "main", "."]);
        git(repo, &["config", "user.email", "test@gitpulse.local"]);
        git(repo, &["config", "user.name", "GitPulse Tester"]);
        std::fs::write(repo.join("README.md"), "hello\n").unwrap();
        if let Some(hooks) = hooks {
            std::fs::create_dir_all(repo.join(".gitpulse")).unwrap();
            std::fs::write(repo.join(".gitpulse/hooks.json"), hooks).unwrap();
        }
        git(repo, &["add", "."]);
        git(repo, &["commit", "-m", "initial commit"]);
        crate::test_support::trust_repo(repo);
        let wt = tempfile::tempdir().expect("wt tempdir");
        let created = add_worktree_extended(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("feature-x"),
            None,
            false,
            false,
        )
        .expect("created worktree");
        assert_eq!(created.path, wt.path().to_str().unwrap());
        crate::test_support::trust_repo(wt.path());
        std::fs::write(wt.path().join("feature.txt"), "lane work\n").unwrap();
        git(wt.path(), &["add", "."]);
        git(wt.path(), &["commit", "-m", "feature commit"]);
        (dir, wt)
    }

    fn allow_all(_: &[&str]) -> Result<(), String> {
        Ok(())
    }

    fn branch_exists(repo: &Path, branch: &str) -> bool {
        git_text(
            repo,
            &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
        )
        .is_ok()
    }

    /// The worktree exists once `git worktree add` succeeded, so a failed
    /// setup step must not read as "not created" — nor, as it did when both
    /// errors were discarded, as "fully set up".
    #[cfg(unix)]
    #[test]
    fn a_failed_post_create_hook_is_reported_with_the_worktree_it_left() {
        let (dir, _) = teardown_fixture(Some(
            r#"{"worktree":{"post_create":["echo setup broke >&2; exit 3"]}}"#,
        ));
        let wt = tempfile::tempdir().unwrap();
        let created = add_worktree_extended(
            dir.path().to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("feature-y"),
            None,
            false,
            true,
        )
        .expect("the worktree itself was created");
        let hook = created.hook_error.expect("the failed hook was reported");
        assert!(
            hook.contains("post_create") && hook.contains("setup broke"),
            "{hook}"
        );
        assert!(wt.path().join("README.md").exists());
        // Without cow_caches nothing optional runs, and nothing is reported.
        let plain = tempfile::tempdir().unwrap();
        let created = add_worktree_extended(
            dir.path().to_str().unwrap(),
            plain.path().to_str().unwrap(),
            Some("feature-z"),
            None,
            false,
            false,
        )
        .unwrap();
        assert_eq!((created.cache_error, created.hook_error), (None, None));
    }

    /// The merge runs in the main checkout, onto what it has checked out. On
    /// another branch it used to merge there and report "main".
    #[test]
    fn teardown_refuses_when_the_main_checkout_is_not_on_the_target() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        git_text(repo, &["switch", "-c", "elsewhere"]).unwrap();
        let mut asked = Vec::new();
        let refused = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            &mut |args: &[&str]| {
                asked.push(args.join(" "));
                Ok(())
            },
        )
        .unwrap_err();
        assert!(refused.contains("is on elsewhere, not main"), "{refused}");
        assert!(
            asked.is_empty(),
            "a refused teardown asked the gate about {asked:?}"
        );
        assert!(
            !repo.join("feature.txt").exists(),
            "the merge reached the wrong branch"
        );
        assert!(wt.path().join("feature.txt").exists() && branch_exists(repo, "feature-x"));
    }

    /// A squash whose commit fails must stop before anything is torn down:
    /// the commit's result was discarded, and the next steps removed the
    /// worktree and force-deleted the branch whose work was never committed.
    #[cfg(unix)]
    #[test]
    fn a_squash_whose_commit_fails_keeps_the_worktree_and_the_branch() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        let hook = repo.join(".git/hooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\necho refusing this commit >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let refused = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            true,
            &mut allow_all,
        )
        .unwrap_err();
        assert!(
            refused.contains("could not be committed") && refused.contains("git reset --merge"),
            "{refused}"
        );
        assert!(
            wt.path().join("feature.txt").exists(),
            "the worktree was removed"
        );
        assert!(
            branch_exists(repo, "feature-x"),
            "the unmerged branch was deleted"
        );
        let staged = git_text(repo, &["diff", "--cached", "--name-only"]).unwrap();
        assert_eq!(
            staged.trim(),
            "feature.txt",
            "the squash should be left staged to finish or undo"
        );
    }

    /// The gate is asked about what runs, in order, with the worktree's own
    /// branch — and a refusal at any step stops there. It was asked once
    /// about `merge --ff-only main`, a command this never ran.
    #[test]
    fn teardown_asks_the_gate_about_each_command_it_runs() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        let wt_path = wt
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let mut asked = Vec::new();
        merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            true,
            &mut |args: &[&str]| {
                asked.push(args.join(" "));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            asked,
            [
                "merge --squash feature-x".to_string(),
                "commit -m Merge branch 'feature-x' (squashed)".to_string(),
                format!("worktree remove --force {wt_path}"),
                "branch -D feature-x".to_string(),
            ]
        );

        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        let refused = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            &mut |args: &[&str]| {
                if args.first() == Some(&"worktree") {
                    Err("policy: worktree removal denied".into())
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
        assert!(refused.contains("worktree removal denied"), "{refused}");
        assert!(
            repo.join("feature.txt").exists(),
            "the merge before the refusal stands"
        );
        assert!(
            wt.path().join("feature.txt").exists(),
            "a refused removal removed the worktree"
        );
        assert!(
            branch_exists(repo, "feature-x"),
            "a refused removal still deleted the branch"
        );
    }

    /// The merge and teardown happened; a post_merge hook that failed after
    /// them is said, not discarded.
    #[cfg(unix)]
    #[test]
    fn a_failed_post_merge_hook_is_reported_after_a_completed_teardown() {
        let (dir, wt) = teardown_fixture(Some(
            r#"{"worktree":{"post_merge":["echo cleanup broke >&2; exit 4"]}}"#,
        ));
        let result = merge_and_teardown_worktree(
            dir.path().to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            &mut allow_all,
        )
        .unwrap();
        assert!(result.worktree_removed && result.branch_deleted);
        let hook = result.hook_error.expect("the failed hook was reported");
        assert!(
            hook.contains("post_merge") && hook.contains("cleanup broke"),
            "{hook}"
        );
    }

    #[test]
    fn test_merge_and_teardown_lifecycle() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_path = dir.path().to_str().unwrap();
        // git init -b main
        let _ = std::process::Command::new("git")
            .args(["init", "-b", "main", repo_path])
            .output_locked()
            .expect("git init");
        let _ = std::process::Command::new("git")
            .args(["config", "user.email", "test@gitpulse.local"])
            .current_dir(repo_path)
            .output_locked();
        let _ = std::process::Command::new("git")
            .args(["config", "user.name", "GitPulse Tester"])
            .current_dir(repo_path)
            .output_locked();

        let readme = dir.path().join("README.md");
        std::fs::write(&readme, "hello\n").unwrap();
        let _ = std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(repo_path)
            .output_locked();
        let _ = std::process::Command::new("git")
            .args(["commit", "-m", "initial commit"])
            .current_dir(repo_path)
            .output_locked();
        crate::test_support::trust_repo(dir.path());

        // Add linked worktree
        let wt_dir = tempfile::tempdir().expect("wt tempdir");
        let wt_path = wt_dir.path().to_str().unwrap();
        let created =
            add_worktree_extended(repo_path, wt_path, Some("feature-x"), None, false, false)
                .expect("created worktree");
        assert_eq!(created.path, wt_path);
        assert_eq!((created.cache_error, created.hook_error), (None, None));
        crate::test_support::trust_repo(wt_dir.path());

        // Add commit in worktree
        let feature_file = wt_dir.path().join("feature.txt");
        std::fs::write(&feature_file, "lane work\n").unwrap();
        let _ = std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(wt_path)
            .output_locked();
        let _ = std::process::Command::new("git")
            .args(["commit", "-m", "feature commit"])
            .current_dir(wt_path)
            .output_locked();

        // Merge and teardown
        let res =
            merge_and_teardown_worktree(repo_path, wt_path, Some("main"), false, &mut allow_all)
                .expect("merged and torn down");
        assert_eq!(res.hook_error, None);
        assert_eq!(res.merged_branch, "feature-x");
        assert_eq!(res.target_branch, "main");
        assert_eq!(res.commits_merged, 1);
        assert!(res.worktree_removed);
        assert!(res.branch_deleted);

        // Verify main has the merged file
        assert!(dir.path().join("feature.txt").exists());
        // Verify worktree directory is removed or no longer tracked
        let worktrees = list_worktrees(repo_path).expect("worktrees");
        assert_eq!(worktrees.len(), 1);
        assert_eq!(worktrees[0].branch.as_deref(), Some("main"));
    }
}
