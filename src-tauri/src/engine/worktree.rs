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
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::Duration;

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

/// The agent kind GitPulse's own task worktrees carry: the hidden directory
/// `workbench::agent_worktree` places them under, without its dot.
///
/// The layout rule accepts any hidden directory, so `.gitpulse/worktrees/<slug>`
/// was reported as kind `gitpulse` only because nothing excluded it. Naming it
/// here makes it a decision: the provisioner builds its path from this
/// constant, and [`is_gitpulse_lane`] is how a reader recognises the result.
pub const GITPULSE_LANE_KIND: &str = "gitpulse";

/// Whether an agent kind names a worktree GitPulse provisioned for a task,
/// rather than one an external agent made for itself.
///
/// Case-insensitive: on the case-insensitive volumes macOS and Windows ship by
/// default, `.GitPulse/worktrees` is the same directory. Held to the shared
/// corpus with `isGitPulseLane` in `src/lib/work/agentWorktree.ts`.
pub fn is_gitpulse_lane(kind: &str) -> bool {
    kind.eq_ignore_ascii_case(GITPULSE_LANE_KIND)
}

/// GitPulse's task-worktree container relative to the main checkout, in the
/// `/`-separated form an `info/exclude` pattern takes. Always
/// `.<GITPULSE_LANE_KIND>/<WORKTREES_SEGMENT>`; a test holds the two together.
pub const GITPULSE_LANE_DIR: &str = ".gitpulse/worktrees";

/// The task-worktree container under a main checkout, built from the same
/// segments the layout rule reads, so the place GitPulse creates them and the
/// rule that recognises them cannot drift.
pub fn gitpulse_lane_container(main_checkout: &Path) -> PathBuf {
    main_checkout
        .join(format!(".{GITPULSE_LANE_KIND}"))
        .join(WORKTREES_SEGMENT)
}

/// Refuses a hand-made worktree inside GitPulse's task-worktree container.
///
/// `workbench::agent_worktree` is the one creator of worktrees there: it
/// excludes the container from `git status` before adding, names the branch
/// after the attempt, and removes what a refused attempt made. A worktree added
/// there by hand got none of that — `git add -A` in the main checkout would
/// stage it — and was then labelled "GitPulse task" with no task behind it.
/// Read from the path text, so it holds whether or not the target exists yet.
pub fn refuse_gitpulse_lane_target(target_path: &str) -> Result<(), String> {
    match agent_layout(target_path) {
        Some(layout) if is_gitpulse_lane(&layout.kind) => Err(format!(
            "{target_path} is inside {GITPULSE_LANE_DIR}/, where GitPulse places the worktrees it creates for task attempts. Start the task from the task board to get one, or choose a path outside {GITPULSE_LANE_DIR}/."
        )),
        _ => Ok(()),
    }
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
    list_worktrees_scanned(repo_path, ScanDepth::Listing)
}

/// How much of each worktree a listing measures. Every level past
/// [`ScanDepth::Listing`] costs git children per worktree, so a caller asks
/// for what it reads and no more: the collision check reads paths and
/// branches, and used to pay for diff stats, divergence and routes it threw
/// away, on every Work refresh.
/// Ordered shallowest first, so a shared scan takes the deepest ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScanDepth {
    /// One `git worktree list`; every count is `None` ("not scanned").
    Listing,
    /// Adds the dirty-file count (`git status`) for each scanned worktree.
    Dirty,
    /// Adds diff stats, divergence from main/master, and portless routes.
    Full,
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
    if is_main_name(b) {
        return Some(WorktreeDivergence::default());
    }
    divergence_from(repo, &main_ref(repo), b)
}

fn is_main_name(branch: &str) -> bool {
    branch == "main" || branch == "master"
}

/// Which of `main` / `master` a repository's divergence is measured from.
/// One answer per listing: it is a property of the repository, and asking it
/// once per worktree cost two processes a worktree.
#[derive(Clone, Debug, PartialEq, Eq)]
enum MainRef {
    Found(&'static str),
    Absent,
    /// The lookup itself failed; carries why, so a scan can say so rather
    /// than reading "no main branch".
    Unread(String),
}

fn main_ref(repo: &Path) -> MainRef {
    let listed = crate::engine::ref_cache::git_text(
        repo,
        &["refs/heads"],
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/heads/main",
            "refs/heads/master",
        ],
    );
    match listed {
        // The patterns also match `refs/heads/main/<x>`, so only an exact
        // line names the branch itself.
        Ok(text) => {
            let has = |name: &str| text.lines().any(|line| line == name);
            if has("refs/heads/main") {
                MainRef::Found("main")
            } else if has("refs/heads/master") {
                MainRef::Found("master")
            } else {
                MainRef::Absent
            }
        }
        Err(reason) => MainRef::Unread(reason),
    }
}

fn divergence_from(repo: &Path, main: &MainRef, b: &str) -> Option<WorktreeDivergence> {
    let MainRef::Found(default_ref) = main else {
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

/// What a listing measures divergence against: main/master, and every scanned
/// branch's counts from one `for-each-ref` when that could be read.
struct Base {
    main: MainRef,
    /// `None` when the batch was not asked or did not answer; each worktree
    /// then measures itself, as it always did.
    batch: Option<HashMap<String, WorktreeDivergence>>,
}

impl Base {
    fn none() -> Self {
        Self {
            main: MainRef::Absent,
            batch: None,
        }
    }
}

/// Every branch's divergence from main in one `for-each-ref`, where the scan
/// used to run one `rev-list` per worktree. The `ahead-behind` atom needs git
/// 2.42; an older git, or any other failure, answers `None` and each worktree
/// falls back to its own `rev-list`.
///
/// A pattern matches its whole namespace (`refs/heads/main` also selects
/// `refs/heads/main/x`), so a row counts only when its refname is exactly a
/// branch that was asked for.
fn batch_divergence(
    repo: &Path,
    main: &MainRef,
    branches: &[&str],
) -> Option<HashMap<String, WorktreeDivergence>> {
    let MainRef::Found(main_name) = main else {
        return None;
    };
    if branches.is_empty() {
        return Some(HashMap::new());
    }
    let format = format!("--format=%(refname)%00%(ahead-behind:refs/heads/{main_name})");
    let patterns: Vec<String> = branches.iter().map(|b| format!("refs/heads/{b}")).collect();
    let mut args = vec!["for-each-ref", format.as_str()];
    args.extend(patterns.iter().map(String::as_str));
    let text = crate::engine::ref_cache::git_text(repo, &["refs/heads"], &args).ok()?;
    let mut out = HashMap::new();
    for line in text.lines() {
        let Some((refname, counts)) = line.split_once('\0') else {
            continue;
        };
        let Some(branch) = refname.strip_prefix("refs/heads/") else {
            continue;
        };
        if !branches.contains(&branch) {
            continue;
        }
        if let Some((ahead, behind)) = crate::engine::git_reader::parse_ahead_behind_pair(counts) {
            out.insert(branch.to_string(), WorktreeDivergence { ahead, behind });
        }
    }
    Some(out)
}

/// Lists every worktree of the repository, main entry first, with dirty-file
/// counts, diff deltas, and detected portless routes for the worktrees closest to the front.
pub fn list_worktrees(repo_path: &str) -> Result<Vec<WorktreeInfo>, String> {
    list_worktrees_scanned(repo_path, ScanDepth::Full)
}

/// Lists every worktree, measuring each scanned one to `depth`.
///
/// Identical asks share one scan. The sidebar's worktree panel and the Work
/// view both list on every activation, a few milliseconds apart, and each
/// used to scan every worktree. A caller joins a listing only while it has not
/// started reading — so no caller is ever handed an answer older than its own
/// request — and a caller arriving while one is reading waits for the one
/// follow-up listing everyone who arrived meanwhile shares. The shared scan
/// is as deep as its deepest caller asked. `Listing` depth is one process and
/// never waits for a scan.
pub fn list_worktrees_scanned(
    repo_path: &str,
    depth: ScanDepth,
) -> Result<Vec<WorktreeInfo>, String> {
    if depth == ScanDepth::Listing {
        return scan_listing(repo_path, depth);
    }
    // Validated before joining, so a refusal is this caller's own answer.
    let repo = validate_repo(repo_path)?;
    let key = FlightKey {
        repo,
        class: crate::engine::git_cli::current_admission(),
    };
    let (flight, leader, previous) = {
        let mut lanes = flight_lanes()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let lane = lanes.entry(key.clone()).or_default();
        match &lane.open {
            Some(open) => {
                let mut state = open.state.lock().unwrap_or_else(PoisonError::into_inner);
                state.depth = state.depth.max(depth);
                drop(state);
                (Arc::clone(open), false, None)
            }
            None => {
                let flight = Arc::new(Flight::new(depth));
                lane.open = Some(Arc::clone(&flight));
                (flight, true, lane.running.clone())
            }
        }
    };
    if !leader {
        return flight.wait();
    }

    // Let the other asks of the same moment arrive. Waiting out a listing
    // already reading serves the same purpose, and is required anyway: this
    // one must read after it.
    match previous {
        Some(running) => {
            let _ = running.wait();
        }
        None => std::thread::sleep(gather_window()),
    }
    let depth = {
        let mut lanes = flight_lanes()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let lane = lanes.entry(key.clone()).or_default();
        lane.open = None;
        lane.running = Some(Arc::clone(&flight));
        let state = flight.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.depth
    };

    /// Publishes whatever the leader ended with and frees the lane, so a
    /// leader that panics cannot strand the callers waiting on it.
    struct Land<'a> {
        key: &'a FlightKey,
        flight: &'a Arc<Flight>,
        outcome: Option<Result<Vec<WorktreeInfo>, String>>,
    }
    impl Drop for Land<'_> {
        fn drop(&mut self) {
            let outcome = self.outcome.take().unwrap_or_else(|| {
                Err("the shared worktree listing ended without a result".into())
            });
            {
                let mut lanes = flight_lanes()
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if let Some(lane) = lanes.get_mut(self.key) {
                    if lane
                        .running
                        .as_ref()
                        .is_some_and(|r| Arc::ptr_eq(r, self.flight))
                    {
                        lane.running = None;
                    }
                    if lane.running.is_none() && lane.open.is_none() {
                        lanes.remove(self.key);
                    }
                }
            }
            self.flight.publish(outcome);
        }
    }
    let mut land = Land {
        key: &key,
        flight: &flight,
        outcome: None,
    };
    let outcome = scan_listing(repo_path, depth);
    #[cfg(test)]
    before_publish_hook();
    land.outcome = Some(outcome.clone());
    drop(land);
    outcome
}

/// How long the first ask of a listing waits for the others of its moment.
const GATHER_WINDOW: Duration = Duration::from_millis(20);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct FlightKey {
    repo: PathBuf,
    /// A background caller never shares a user-facing one's scan, nor the
    /// other way round: the class decides what the gate may refuse.
    class: crate::engine::git_cli::Admission,
}

#[derive(Default)]
struct Lane {
    /// Still gathering: joinable.
    open: Option<Arc<Flight>>,
    /// Reading: not joinable; the next ask waits for it.
    running: Option<Arc<Flight>>,
}

struct Flight {
    state: Mutex<FlightState>,
    done: Condvar,
}

struct FlightState {
    depth: ScanDepth,
    result: Option<Result<Vec<WorktreeInfo>, String>>,
}

impl Flight {
    fn new(depth: ScanDepth) -> Self {
        Self {
            state: Mutex::new(FlightState {
                depth,
                result: None,
            }),
            done: Condvar::new(),
        }
    }

    fn wait(&self) -> Result<Vec<WorktreeInfo>, String> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        while state.result.is_none() {
            state = self
                .done
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        state
            .result
            .clone()
            .unwrap_or_else(|| Err("the shared worktree listing was lost".into()))
    }

    fn publish(&self, outcome: Result<Vec<WorktreeInfo>, String>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.result = Some(outcome);
        self.done.notify_all();
    }
}

fn flight_lanes() -> &'static Mutex<HashMap<FlightKey, Lane>> {
    static LANES: OnceLock<Mutex<HashMap<FlightKey, Lane>>> = OnceLock::new();
    LANES.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(not(test))]
fn gather_window() -> Duration {
    GATHER_WINDOW
}

#[cfg(test)]
thread_local! {
    static GATHER_OVERRIDE: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
    static BEFORE_PUBLISH: std::cell::RefCell<Option<Box<dyn FnMut()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn gather_window() -> Duration {
    GATHER_OVERRIDE
        .with(|cell| cell.get())
        .unwrap_or(GATHER_WINDOW)
}

#[cfg(test)]
fn before_publish_hook() {
    BEFORE_PUBLISH.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook();
        }
    });
}

/// One listing with no sharing: what [`list_worktrees_scanned`] runs.
fn scan_listing(repo_path: &str, depth: ScanDepth) -> Result<Vec<WorktreeInfo>, String> {
    let repo = validate_repo(repo_path)?;
    let stdout = git_text(&repo, &["worktree", "list", "--porcelain"])?;
    let parsed = parse_worktree_porcelain(&stdout);
    if depth == ScanDepth::Listing {
        return Ok(parsed
            .into_iter()
            .enumerate()
            .map(|(idx, entry)| info_from(idx, entry, WorktreeScan::default()))
            .collect());
    }

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
    // Asked once for the whole listing, and only when some scanned branch
    // is measured against it.
    let needs_main = depth == ScanDepth::Full
        && scan_targets.iter().any(|&idx| {
            parsed[idx]
                .branch
                .as_deref()
                .is_some_and(|branch| !is_main_name(branch))
        });
    let base = if needs_main {
        let main = main_ref(&repo);
        let branches: Vec<&str> = scan_targets
            .iter()
            .filter_map(|&idx| parsed[idx].branch.as_deref())
            .filter(|branch| !is_main_name(branch))
            .collect();
        let batch = batch_divergence(&repo, &main, &branches);
        Base { main, batch }
    } else {
        Base::none()
    };
    let mut metrics: HashMap<usize, WorktreeScan> = HashMap::new();
    for wave in scan_targets.chunks(DIRTY_SCAN_FAN_OUT) {
        std::thread::scope(|scope| {
            let handles: Vec<_> = wave
                .iter()
                .map(|&idx| {
                    let entry = &parsed[idx];
                    let repo = &repo;
                    let base = &base;
                    let handle = scope.spawn(move || {
                        crate::engine::git_cli::with_admission(class, || {
                            scan_worktree(repo, entry, depth, base)
                        })
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
            info_from(idx, entry, scan)
        })
        .collect())
}

fn info_from(idx: usize, entry: ParsedWorktree, scan: WorktreeScan) -> WorktreeInfo {
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
}

/// Fills in the dirty-file count of one listed worktree, as a
/// [`ScanDepth::Dirty`] listing would have, without scanning its siblings.
pub fn scan_dirty(info: &mut WorktreeInfo) {
    let entry = ParsedWorktree {
        path: info.path.clone(),
        branch: info.branch.clone(),
        is_bare: info.is_bare,
        ..ParsedWorktree::default()
    };
    let scan = if info.is_bare {
        WorktreeScan {
            note: Some("bare entry: no working tree to scan".into()),
            ..WorktreeScan::default()
        }
    } else {
        // The repository path is read only at `Full` depth.
        scan_worktree(
            Path::new(&info.path),
            &entry,
            ScanDepth::Dirty,
            &Base::none(),
        )
    };
    info.dirty_files = scan.dirty;
    info.scan_note = scan.note;
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

fn scan_worktree(
    repo: &Path,
    entry: &ParsedWorktree,
    depth: ScanDepth,
    base: &Base,
) -> WorktreeScan {
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
    let dirty = Some(count_status_entries(status.as_bytes()));
    if depth != ScanDepth::Full {
        return WorktreeScan {
            dirty,
            ..WorktreeScan::default()
        };
    }
    let branch = entry.branch.as_deref();
    let divergence = branch.and_then(|b| {
        if is_main_name(b) {
            Some(WorktreeDivergence::default())
        } else if let Some(batch) = &base.batch {
            // A branch the batch did not list is not a ref (unborn), which
            // its own `rev-list` could not measure either.
            batch.get(b).cloned()
        } else {
            divergence_from(repo, &base.main, b)
        }
    });
    let mut scan = WorktreeScan {
        dirty,
        diff_stat: measure_diff_stat(dir),
        divergence,
        routes: detect_worktree_routes(&entry.path, branch),
        note: None,
    };
    // The listing's one main/master lookup failed outside this scan's
    // failure window, so it is named here or not at all.
    if let (MainRef::Unread(reason), Some(b)) = (&base.main, branch) {
        if !is_main_name(b) {
            scan.note = Some(format!("some measurements were not taken: {reason}"));
        }
    }
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
            scan_worktree(&repo, &entry, ScanDepth::Full, &Base::none())
        });
        assert_eq!(scan.dirty, Some(0));
        assert_eq!(scan.diff_stat, None);
        let note = scan.note.unwrap_or_default();
        assert!(
            note.starts_with("some measurements were not taken: "),
            "{note}"
        );
        assert!(note.contains("forced by test"), "{note}");
        let whole = scan_worktree(&repo, &entry, ScanDepth::Full, &Base::none());
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
        /// Absent means false: only a GitPulse task worktree sets it.
        #[serde(default)]
        gitpulse_lane: bool,
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
            // Whether the kind names GitPulse's own task worktree is a
            // decision both implementations make from the same kind.
            assert_eq!(
                is_gitpulse_lane(&kind),
                case.gitpulse_lane,
                "is_gitpulse_lane {:?} — {}",
                case.path,
                case.why
            );
        }
        assert!(
            corpus.cases.iter().any(|case| case.gitpulse_lane),
            "the corpus must keep a GitPulse task worktree case"
        );
    }

    #[test]
    fn the_gitpulse_lane_location_is_the_one_the_layout_rule_recognises() {
        // The exclude pattern, the directory the provisioner joins, and the
        // kind the detector reports are one fact spelled three ways.
        assert_eq!(
            GITPULSE_LANE_DIR,
            format!(".{GITPULSE_LANE_KIND}/{WORKTREES_SEGMENT}")
        );
        let container = gitpulse_lane_container(Path::new("/repo"));
        let inside = container.join("fix-x-1a2b3c4d");
        let layout = agent_layout(inside.to_str().unwrap()).expect("a lane is an agent layout");
        assert!(is_gitpulse_lane(&layout.kind), "{layout:?}");
        assert_eq!(layout.slug, "fix-x-1a2b3c4d");
    }

    #[test]
    fn a_hand_made_worktree_is_refused_inside_the_gitpulse_lane_container() {
        for target in [
            "/repo/.gitpulse/worktrees/agent-lr3k2",
            "/repo/.gitpulse/worktrees",
            ".gitpulse/worktrees/x",
            "/repo/.GitPulse/worktrees/x",
            "C:\\repo\\.gitpulse\\worktrees\\x",
            "/repo/sub/../.gitpulse/worktrees/x",
        ] {
            let refused = refuse_gitpulse_lane_target(target)
                .expect_err(&format!("{target} must be refused"));
            assert!(refused.contains(GITPULSE_LANE_DIR), "{refused}");
        }
        for target in [
            "/repo-feature",
            "/repo/.claude/worktrees/session",
            "/repo/.gitpulse/hooks",
            "/repo/.gitpulsex/worktrees/x",
            "/repo/gitpulse/worktrees/x",
        ] {
            assert_eq!(refuse_gitpulse_lane_target(target), Ok(()), "{target}");
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

    fn spawned_with(cwds: &[PathBuf], needle: &str) -> usize {
        cwds.iter()
            .flat_map(|cwd| crate::engine::git_cli::spawn_log::spawns_in(cwd))
            .filter(|argv| argv.iter().any(|arg| arg == needle))
            .count()
    }

    /// The main/master lookup is a property of the repository: one per
    /// listing, not two processes for every worktree.
    #[test]
    fn a_full_listing_looks_up_main_once_whatever_the_worktree_count() {
        let (main, _parent, linked) = repo_with_worktrees(5);
        let root = main.path().canonicalize().unwrap();
        let listed = list_worktrees(root.to_str().unwrap()).expect("list");
        assert_eq!(listed.len(), 6);
        let mut cwds = linked.clone();
        cwds.push(root.clone());
        assert_eq!(spawned_with(&cwds, "--verify"), 0, "per-worktree probes");
        assert_eq!(
            spawned_with(&cwds, "rev-list"),
            0,
            "per-worktree divergence walks"
        );
        assert_eq!(
            spawned_with(std::slice::from_ref(&root), "refs/heads/main"),
            1
        );
        // And what it measures is what git measures.
        for info in &listed[1..] {
            let branch = info.branch.as_deref().unwrap();
            let counts = crate::engine::git_cli::git_text(
                &root,
                &[
                    "rev-list",
                    "--left-right",
                    "--count",
                    &format!("main...{branch}"),
                ],
            )
            .unwrap();
            let mut parts = counts.split_whitespace();
            let behind: usize = parts.next().unwrap().parse().unwrap();
            let ahead: usize = parts.next().unwrap().parse().unwrap();
            let measured = info.main_divergence.as_ref().expect("measured");
            assert_eq!((measured.ahead, measured.behind), (ahead, behind));
            assert_eq!(info.scan_note, None);
        }
    }

    /// When the batch cannot answer (an older git rejects the atom), each
    /// worktree measures itself and the numbers are the same.
    #[test]
    fn a_batch_that_cannot_answer_falls_back_to_each_worktrees_own_walk() {
        let (main, _parent, linked) = repo_with_worktrees(3);
        let root = main.path().canonicalize().unwrap();
        // Two commits on one agent branch, so the counts are not all zero.
        for n in 0..2 {
            std::fs::write(linked[1].join(format!("w{n}.txt")), "x").unwrap();
            git_in(&linked[1], &["add", "."]);
            git_in(&linked[1], &["commit", "-q", "-m", &format!("w{n}")]);
        }
        let batched = list_worktrees(root.to_str().unwrap()).expect("list");
        let format = "--format=%(refname)%00%(ahead-behind:refs/heads/main)";
        let before = spawned_with(std::slice::from_ref(&root), "rev-list");
        let walked = crate::engine::git_cli::with_forced_spawn_failure_of(format, || {
            list_worktrees(root.to_str().unwrap())
        })
        .expect("list");
        assert_eq!(
            spawned_with(std::slice::from_ref(&root), "rev-list") - before,
            3,
            "one walk per agent worktree"
        );
        let counts = |list: &[WorktreeInfo]| -> Vec<Option<WorktreeDivergence>> {
            list.iter().map(|w| w.main_divergence.clone()).collect()
        };
        assert_eq!(counts(&batched), counts(&walked));
        assert_eq!(
            batched[2].main_divergence,
            Some(WorktreeDivergence {
                ahead: 2,
                behind: 0
            })
        );
    }

    /// `refs/heads/main/x` is matched by the `refs/heads/main` pattern but is
    /// not a branch named main; master is the base then.
    #[test]
    fn main_is_an_exact_branch_name_not_a_prefix() {
        let dir = tempfile::TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-q", "-b", "master"]);
        git_in(dir.path(), &["commit", "-q", "--allow-empty", "-m", "base"]);
        git_in(dir.path(), &["branch", "main/feature"]);
        let root = dir.path().canonicalize().unwrap();
        assert_eq!(main_ref(&root), MainRef::Found("master"));
        git_in(dir.path(), &["branch", "-q", "-m", "master", "trunk"]);
        assert_eq!(main_ref(&root), MainRef::Absent);
    }

    /// A lookup that could not run is named on every worktree it would have
    /// measured, not read as "no main branch".
    #[test]
    fn a_failed_main_lookup_is_named_on_the_worktrees_it_left_unmeasured() {
        let (main, _parent, _linked) = repo_with_worktrees(2);
        let root = main.path().canonicalize().unwrap();
        let listed = crate::engine::git_cli::with_forced_spawn_failure_of("for-each-ref", || {
            list_worktrees(root.to_str().unwrap())
        })
        .expect("list");
        assert_eq!(listed[0].scan_note, None, "main itself needs no lookup");
        for info in &listed[1..] {
            assert_eq!(info.main_divergence, None);
            let note = info.scan_note.as_deref().unwrap_or_default();
            assert!(note.contains("forced by test"), "{note}");
        }
    }

    /// Each depth runs what it reads and nothing past it.
    #[test]
    fn each_scan_depth_spawns_only_what_it_measures() {
        let (main, _parent, linked) = repo_with_worktrees(3);
        let root = main.path().canonicalize().unwrap();
        let mut cwds = linked.clone();
        cwds.push(root.clone());

        let listing = list_worktrees_scanned(root.to_str().unwrap(), ScanDepth::Listing).unwrap();
        assert!(listing.iter().all(|w| w.dirty_files.is_none()));
        assert_eq!(spawned_with(&cwds, "status"), 0);

        let dirty = list_worktrees_scanned(root.to_str().unwrap(), ScanDepth::Dirty).unwrap();
        assert!(dirty.iter().all(|w| w.dirty_files == Some(0)));
        assert!(dirty
            .iter()
            .all(|w| w.diff_stat.is_none() && w.main_divergence.is_none()));
        assert_eq!(spawned_with(&cwds, "status"), 4);
        assert_eq!(spawned_with(&cwds, "--shortstat"), 0);
        assert_eq!(spawned_with(&cwds, "rev-list"), 0);
        assert_eq!(spawned_with(&cwds, "for-each-ref"), 0);
    }

    /// Filling in one worktree scans that worktree only.
    #[test]
    fn scanning_one_listed_worktree_leaves_its_siblings_alone() {
        let (main, _parent, linked) = repo_with_worktrees(2);
        std::fs::write(linked[1].join("new.txt"), "x").unwrap();
        let mut listed = list_worktrees_lite(main.path().to_str().unwrap()).unwrap();
        let mut target = listed.remove(2);
        scan_dirty(&mut target);
        assert_eq!(target.dirty_files, Some(1));
        assert_eq!(target.scan_note, None);
        let others = [main.path().canonicalize().unwrap(), linked[0].clone()];
        assert_eq!(spawned_with(&others, "status"), 0);
    }

    /// The wire names are the variant names in lowercase, and nothing else:
    /// a misspelt depth is refused rather than read as some other depth.
    #[test]
    fn scan_depth_reads_only_its_own_lowercase_names() {
        let read = |text: &str| serde_json::from_str::<ScanDepth>(text);
        assert_eq!(read(r#""listing""#).unwrap(), ScanDepth::Listing);
        assert_eq!(read(r#""dirty""#).unwrap(), ScanDepth::Dirty);
        assert_eq!(read(r#""full""#).unwrap(), ScanDepth::Full);
        for wrong in [r#""Dirty""#, r#""deep""#, r#""""#, "1", "null"] {
            assert!(read(wrong).is_err(), "{wrong} was accepted");
        }
    }

    /// `worktree list` processes run in `root`.
    fn listings_in(root: &Path) -> usize {
        crate::engine::git_cli::spawn_log::spawns_in(root)
            .iter()
            .filter(|argv| {
                argv.iter().any(|a| a == "list") && argv.iter().any(|a| a == "--porcelain")
            })
            .count()
    }

    /// Whether a listing of `root` is gathering callers right now.
    fn gathering(root: &Path) -> bool {
        let key = FlightKey {
            repo: validate_repo(root.to_str().unwrap()).unwrap(),
            class: crate::engine::git_cli::current_admission(),
        };
        let lanes = flight_lanes().lock().unwrap();
        lanes.get(&key).is_some_and(|lane| lane.open.is_some())
    }

    /// How many callers share the listing of `root` that is gathering.
    fn sharing(root: &Path) -> usize {
        let key = FlightKey {
            repo: validate_repo(root.to_str().unwrap()).unwrap(),
            class: crate::engine::git_cli::current_admission(),
        };
        let lanes = flight_lanes().lock().unwrap();
        // The lane and the leader hold one reference each; every joiner, one.
        lanes
            .get(&key)
            .and_then(|lane| lane.open.as_ref())
            .map_or(0, |open| Arc::strong_count(open) - 1)
    }

    fn as_json(listed: &[WorktreeInfo]) -> serde_json::Value {
        serde_json::to_value(listed).unwrap()
    }

    fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
        let started = std::time::Instant::now();
        while !ready() {
            assert!(started.elapsed() < Duration::from_secs(10), "never {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Starts a listing that gathers for `window` before reading.
    fn lead(
        root: &Path,
        depth: ScanDepth,
        window: Duration,
    ) -> std::thread::JoinHandle<Result<Vec<WorktreeInfo>, String>> {
        let path = root.to_str().unwrap().to_string();
        std::thread::spawn(move || {
            GATHER_OVERRIDE.with(|cell| cell.set(Some(window)));
            list_worktrees_scanned(&path, depth)
        })
    }

    /// The sidebar and the Work view ask for the same listing on every
    /// activation; asks that arrive together share one scan, which is as deep
    /// as the deepest of them.
    #[test]
    fn concurrent_listings_share_one_scan_at_the_deepest_depth() {
        let (main, _parent, linked) = repo_with_worktrees(3);
        let root = main.path().canonicalize().unwrap();
        let before = listings_in(&root);
        let leader = lead(&root, ScanDepth::Dirty, Duration::from_secs(2));
        wait_until("gathering", || gathering(&root));
        let joiners: Vec<_> = [ScanDepth::Full, ScanDepth::Dirty, ScanDepth::Full]
            .into_iter()
            .map(|depth| {
                let path = root.to_str().unwrap().to_string();
                std::thread::spawn(move || list_worktrees_scanned(&path, depth))
            })
            .collect();
        wait_until("every caller joined", || sharing(&root) == 4);
        let mut answers = vec![leader.join().unwrap().expect("leader")];
        for joiner in joiners {
            answers.push(joiner.join().unwrap().expect("joiner"));
        }
        assert_eq!(listings_in(&root) - before, 1, "one shared listing");
        let mut cwds = linked.clone();
        cwds.push(root.clone());
        assert_eq!(
            spawned_with(&cwds, "status"),
            linked.len() + 1,
            "one status per worktree"
        );
        for listed in &answers {
            assert_eq!(as_json(listed), as_json(&answers[0]));
            // The Dirty leader was raised to Full by its joiners.
            assert!(
                listed[1..].iter().all(|w| w.main_divergence.is_some()),
                "{listed:?}"
            );
        }
        assert!(!gathering(&root), "the lane was left open");
    }

    /// A caller arriving while a listing is reading never receives it: it
    /// waits for the next one, which reads after it arrived.
    #[test]
    fn a_listing_already_reading_is_never_handed_to_a_later_caller() {
        let (main, _parent, linked) = repo_with_worktrees(1);
        let root = main.path().canonicalize().unwrap();
        let before = listings_in(&root);
        let (computed_tx, computed_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let path = root.to_str().unwrap().to_string();
        let early = std::thread::spawn(move || {
            GATHER_OVERRIDE.with(|cell| cell.set(Some(Duration::ZERO)));
            BEFORE_PUBLISH.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    computed_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                }));
            });
            list_worktrees_scanned(&path, ScanDepth::Dirty)
        });
        computed_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the first listing read");
        // The worktree changes after the first listing read it.
        std::fs::write(linked[0].join("late.txt"), "late").unwrap();
        let path = root.to_str().unwrap().to_string();
        let late = std::thread::spawn(move || list_worktrees_scanned(&path, ScanDepth::Dirty));
        wait_until("queued behind the reading listing", || gathering(&root));
        // It does not read alongside the first: one scan of a repository at a
        // time.
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            listings_in(&root) - before,
            1,
            "the later listing read early"
        );
        release_tx.send(()).unwrap();
        let early = early.join().unwrap().expect("early");
        let late = late.join().unwrap().expect("late");
        assert_eq!(early[1].dirty_files, Some(0), "read before the change");
        assert_eq!(
            late[1].dirty_files,
            Some(1),
            "the late caller saw the change"
        );
    }

    /// A background listing never rides a user-facing one, nor the other way
    /// round, and a `Listing`-depth ask never waits for a scan.
    #[test]
    fn listings_share_only_within_one_admission_class_and_depth_listing_never_waits() {
        use crate::engine::git_cli::{with_admission, Admission};
        let (main, _parent, _linked) = repo_with_worktrees(1);
        let root = main.path().canonicalize().unwrap();
        let before = listings_in(&root);
        let leader = lead(&root, ScanDepth::Dirty, Duration::from_millis(500));
        wait_until("gathering", || gathering(&root));
        let path = root.to_str().unwrap().to_string();
        let started = std::time::Instant::now();
        let bare = list_worktrees_scanned(&path, ScanDepth::Listing).expect("listing");
        assert!(
            started.elapsed() < Duration::from_millis(400),
            "a bare listing waited {:?}",
            started.elapsed()
        );
        assert!(bare.iter().all(|w| w.dirty_files.is_none()));
        let background = std::thread::spawn(move || {
            with_admission(Admission::Background, || {
                list_worktrees_scanned(&path, ScanDepth::Dirty)
            })
        });
        background.join().unwrap().expect("background");
        leader.join().unwrap().expect("leader");
        assert_eq!(
            listings_in(&root) - before,
            3,
            "bare, background and reactive each listed"
        );
    }

    /// A failed listing reaches every caller sharing it, and frees the lane.
    #[test]
    fn a_failed_shared_listing_fails_every_caller_and_frees_the_lane() {
        let (main, _parent, _linked) = repo_with_worktrees(0);
        let root = main.path().canonicalize().unwrap();
        let leader = lead(&root, ScanDepth::Dirty, Duration::from_millis(300));
        wait_until("gathering", || gathering(&root));
        let path = root.to_str().unwrap().to_string();
        let joiner = std::thread::spawn(move || list_worktrees_scanned(&path, ScanDepth::Full));
        wait_until("the joiner joined", || sharing(&root) == 2);
        // Break the repository while the listing gathers.
        std::fs::write(root.join(".git/HEAD"), "garbage\n").unwrap();
        let led = leader.join().unwrap();
        let joined = joiner.join().unwrap();
        let led = led.expect_err("the broken repository listed");
        assert_eq!(Err(led), joined.map(|listed| as_json(&listed)));
        assert!(!gathering(&root));
    }
}
