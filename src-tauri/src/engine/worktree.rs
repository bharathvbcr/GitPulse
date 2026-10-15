//! Linked-worktree support.
//!
//! AI coding agents parallelize by checking a task out into its own worktree,
//! so a client that only understands "the repository" breaks down the moment
//! an agent's workflow starts. This module lists every worktree of a
//! repository — including the main checkout and bare entries — and creates and
//! removes them through the same validated, harness-gated paths as every other
//! write.

use crate::engine::git_cli::{
    git_text, git_text_with_timeout, plain_path_text, resolve_git_common_dir, resolve_git_dir,
    validate_repo,
};
use crate::engine::git_writer::is_full_oid;
use crate::engine::git_writer::validate_oid_or_revision;
use crate::engine::git_writer::validate_ref_name;
use crate::engine::git_writer::{GitWriter, MergeMode, SquashFailure};
use crate::lru_map::LruMap;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

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
    admit_worktree_pair(repo_path, worktree_path)?.authenticate()
}

/// A repository/worktree pair admitted from filesystem metadata alone.
///
/// Both paths are trusted checkouts sharing one common Git directory. Nothing
/// has run Git yet, so membership is *not* established: a hand-written gitfile
/// passes this step. [`AdmittedPair::authenticate`] is what proves it.
#[derive(Debug, Clone)]
pub struct AdmittedPair {
    repo: PathBuf,
    worktree: PathBuf,
    common: PathBuf,
}

/// Linked-worktree registry entries read before the metadata view gives up.
/// Far above any real family; past it the caller asks Git instead.
const MAX_REGISTRY_ENTRIES: usize = 1024;

/// A registry `gitdir` pointer is one path; anything longer is not one.
const MAX_GITDIR_POINTER: u64 = 4096;

/// The trust and common-directory checks of [`resolve_worktree_family`],
/// which run before it spawns Git and fail with the same messages.
pub fn admit_worktree_pair(repo_path: &str, worktree_path: &str) -> Result<AdmittedPair, String> {
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
    Ok(AdmittedPair {
        repo,
        worktree,
        common: repo_common,
    })
}

impl AdmittedPair {
    /// Every checkout the repository's own metadata names: the primary
    /// working tree and each registered linked worktree, plus the admitted
    /// pair itself. Read from the common directory, never from Git.
    ///
    /// A necessary-condition view, not an authentication: a caller may use it
    /// to conclude that *no* checkout holds some state, and must ask Git
    /// ([`Self::authenticate`]) for anything else. `None` when the metadata
    /// cannot account for the whole family — a primary working tree that does
    /// not hold the common directory directly, a configured `core.worktree`
    /// (also written by `--separate-git-dir`), an unreadable registry entry,
    /// or an oversized registry — so a caller falls back to Git rather than
    /// act on a partial list.
    pub fn metadata_checkouts(&self) -> Option<Vec<PathBuf>> {
        // Git lists the primary working tree first: the directory holding
        // `.git`, or the repository itself when it is bare. The candidate is
        // proved the way `repository_trust` proves a main checkout — its own
        // metadata must name this common directory as its private one.
        let primary = if self.common.file_name() == Some(std::ffi::OsStr::new(".git")) {
            self.common.parent()?.to_path_buf()
        } else {
            self.common.clone()
        };
        match crate::repository_trust::git_directories(&primary) {
            Ok((private, common)) if private == self.common && common == self.common => {}
            _ => return None,
        }
        // `core.worktree` (which `--separate-git-dir` also writes) moves the
        // primary working tree somewhere this layout cannot see. A possible
        // setting gives up — a false alarm costs one Git call.
        if config_mentions_worktree(&self.common).unwrap_or(true) {
            return None;
        }
        let mut checkouts = vec![primary, self.repo.clone(), self.worktree.clone()];

        let entries = match std::fs::read_dir(self.common.join("worktrees")) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(checkouts),
            Err(_) => return None,
        };
        for (index, entry) in entries.enumerate() {
            if index >= MAX_REGISTRY_ENTRIES {
                return None;
            }
            let entry_dir = entry.ok()?.path();
            let pointer = match read_registry_pointer(&entry_dir.join("gitdir")) {
                Ok(Some(pointer)) => pointer,
                // Not a registration (Git's own registry count skips these too).
                Ok(None) => continue,
                Err(()) => return None,
            };
            // Git writes `<checkout>/.git`; relative pointers are relative to
            // the registry entry.
            let pointer = entry_dir.join(pointer);
            let checkout = pointer.parent()?;
            match std::fs::canonicalize(checkout) {
                Ok(canonical) => checkouts.push(canonical),
                // Prunable: `git worktree list` drops it from the family too.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return None,
            }
        }
        Some(checkouts)
    }

    /// Proves membership with `git worktree list` and names the family.
    ///
    /// Git is asked once per registry state, not once per call: a family that
    /// Git has authenticated is remembered for as long as the repository's own
    /// metadata ([`Self::metadata_checkouts`]) names the same checkouts, so a
    /// worktree added, removed, moved or pruned is seen on the next call. A
    /// repository whose metadata cannot account for its whole family is never
    /// remembered and asks Git every time, as before. Trust and the common
    /// directory are checked by [`admit_worktree_pair`] on every call, before
    /// this one: a revoked grant is refused whatever is remembered here.
    pub fn authenticate(self) -> Result<WorktreeFamily, String> {
        let registry = self.registry_fingerprint();
        if let Some(registry) = &registry {
            if let Some(family) =
                remembered_family(&self.common, registry, &self.repo, &self.worktree)
            {
                return Ok(family);
            }
        }
        let common = self.common.clone();
        let family = self.authenticate_with_git()?;
        if let Some(registry) = registry {
            remember_family(common, registry, &family);
        }
        Ok(family)
    }

    /// The registry as the metadata view names it: sorted, without repeats, so
    /// two reads of an unchanged registry are equal whatever order the
    /// directory listed its entries in. `None` when the metadata cannot
    /// account for the whole family.
    fn registry_fingerprint(&self) -> Option<Vec<PathBuf>> {
        let mut checkouts = self.metadata_checkouts()?;
        checkouts.sort();
        checkouts.dedup();
        Some(checkouts)
    }

    fn authenticate_with_git(self) -> Result<WorktreeFamily, String> {
        let AdmittedPair {
            repo,
            worktree,
            common: repo_common,
        } = self;

        // IPC repository paths are UTF-8 strings, so an unrepresentable list
        // entry cannot equal either validated input and is safely ignored.
        let mut registered = Vec::new();
        for entry in read_worktree_entries(&repo)? {
            let Ok(canonical) = std::fs::canonicalize(&entry.path) else {
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

        // The pair shares one common directory (admission checked it), so only a
        // third checkout as the anchor needs its own read.
        let anchor_common = if anchor == repo || anchor == worktree {
            repo_common.clone()
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
}

/// What Git last said a repository's family is, and the registry state it said
/// it for. Kept per common directory, not per pair: every worktree of one
/// repository shares the answer, so eighteen worktrees cost one `git worktree
/// list`, not eighteen.
struct Remembered {
    registry: Vec<PathBuf>,
    /// Git's own listing, primary first.
    members: Vec<PathBuf>,
}

/// Families remembered at once. Far above the repositories one session opens;
/// past it the family used longest ago is dropped and its next call asks Git.
/// Only that one: emptying the whole memory on each new family made every
/// other family ask Git again.
const MAX_REMEMBERED_FAMILIES: usize = 256;

fn remembered_families() -> &'static Mutex<LruMap<PathBuf, Remembered>> {
    static REMEMBERED: OnceLock<Mutex<LruMap<PathBuf, Remembered>>> = OnceLock::new();
    REMEMBERED.get_or_init(|| Mutex::new(LruMap::new(MAX_REMEMBERED_FAMILIES)))
}

/// The remembered family for `common`, if the registry is still the one it was
/// authenticated against *and* both paths are in it. A pair outside the
/// remembered members is not answered here: Git decides, and says why.
fn remembered_family(
    common: &Path,
    registry: &[PathBuf],
    repo: &Path,
    worktree: &Path,
) -> Option<WorktreeFamily> {
    let mut held = remembered_families()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let remembered = held.get(common).filter(|r| r.registry == registry)?;
    let anchor = remembered.members.first()?.clone();
    if !remembered.members.iter().any(|path| path == repo)
        || !remembered.members.iter().any(|path| path == worktree)
    {
        return None;
    }
    Some(WorktreeFamily {
        anchor,
        worktree: worktree.to_path_buf(),
        members: remembered.members.clone(),
    })
}

fn remember_family(common: PathBuf, registry: Vec<PathBuf>, family: &WorktreeFamily) {
    let mut held = remembered_families()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    held.insert(
        common,
        Remembered {
            registry,
            members: family.members.clone(),
        },
    );
}

/// Largest repository `config` [`AdmittedPair::metadata_checkouts`] reads.
const MAX_CONFIG_SCAN: u64 = 64 * 1024;

/// Whether the common directory's `config` (or its `config.worktree`) may set
/// `core.worktree`: a line whose key is exactly `worktree`, or any `include`
/// section, which can pull that key in from a file not read here. `None` when
/// a file could not be read whole, which the caller treats as "yes".
///
/// Keys only, so `extensions.worktreeConfig` — common, and harmless here —
/// does not count.
fn config_mentions_worktree(common: &Path) -> Option<bool> {
    use std::io::Read;
    for name in ["config", "config.worktree"] {
        let file = match std::fs::File::open(common.join(name)) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        };
        let mut raw = Vec::new();
        file.take(MAX_CONFIG_SCAN + 1).read_to_end(&mut raw).ok()?;
        if raw.len() as u64 > MAX_CONFIG_SCAN {
            return None;
        }
        for line in String::from_utf8_lossy(&raw).lines() {
            let line = line.trim_start().to_ascii_lowercase();
            if line.starts_with('[') {
                if line.contains("include") {
                    return Some(true);
                }
                continue;
            }
            let key = line
                .split(|c: char| c == '=' || c.is_whitespace())
                .next()
                .unwrap_or_default();
            if key == "worktree" {
                return Some(true);
            }
        }
    }
    Some(false)
}

/// One registry entry's `gitdir` pointer: `Ok(None)` when the entry has none,
/// `Err(())` when it exists and could not be read whole.
fn read_registry_pointer(path: &Path) -> Result<Option<PathBuf>, ()> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    let mut raw = String::new();
    file.take(MAX_GITDIR_POINTER + 1)
        .read_to_string(&mut raw)
        .map_err(|_| ())?;
    if raw.len() as u64 > MAX_GITDIR_POINTER {
        return Err(());
    }
    let pointer = raw.trim_end_matches(['\n', '\r']);
    if pointer.is_empty() || pointer.contains('\0') {
        return Err(());
    }
    Ok(Some(PathBuf::from(pointer)))
}

/// How many worktrees get a dirty-file scan when listing. Worktrees number in
/// the low dozens even under heavy agent use; this cap keeps a pathological
/// tree from turning one listing call into hundreds of subprocess spawns.
const MAX_DIRTY_SCANS: usize = 32;

/// Worktrees one listing scans at a time. A full scan is one to three git
/// children (status; a diff when status found changes; a `rev-list` when the
/// batched divergence could not be read); an unbounded parallel walk put all 32 scans' children in the
/// spawn gate's queue at once, ahead of everything else the app was asking.
const DIRTY_SCAN_FAN_OUT: usize = 4;

use crate::engine::cow_clone::reflink_ignored_caches;
use crate::engine::portless::{detect_worktree_routes, WorktreeRouteInfo};
use crate::engine::worktree_hooks::run_repo_hooks;

/// Diff stats for uncommitted changes in a worktree (`HEAD±`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeDiffStat {
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
}

/// Divergence stats between a worktree branch and the repository's default branch (`base↕`).
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
    /// The paths the same `git status` read that produced `dirty_files`
    /// returned, so a caller that wants both does not read twice. Never
    /// crosses IPC: it is a hand-off between Rust callers, not part of the
    /// listing's contract.
    #[serde(skip)]
    pub status_read: StatusRead,
}

/// What reading one worktree's status produced, beside its count.
///
/// Three states, because they mean three different things to a collision
/// scan: a worktree that was never read must be read (or reported unread), one
/// whose read failed must be reported failed and not asked again inside the
/// same pass, and one that was read is not asked at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum StatusRead {
    /// No status read of this worktree is attached to the listing.
    #[default]
    NotRead,
    /// Repo-relative changed paths, cut at [`MAX_CHANGED_PATHS`] with
    /// `truncated` saying so.
    Read { paths: Vec<String>, truncated: bool },
    /// The read ran and failed, or was refused; the reason is the scan note.
    Failed(String),
}

/// One point in time past which a scan stops starting work and stops waiting
/// on what it started. Handed down through every stage of a pass, so each
/// stage asks the same clock rather than being given its own slice of it.
#[derive(Clone, Copy, Debug)]
pub struct ScanDeadline(Instant);

impl ScanDeadline {
    /// The deadline `budget` after `start`.
    pub fn after(start: Instant, budget: Duration) -> Self {
        Self(start.checked_add(budget).unwrap_or(start))
    }

    pub fn expired(&self) -> bool {
        Instant::now() >= self.0
    }

    pub fn remaining(&self) -> Duration {
        self.0.saturating_duration_since(Instant::now())
    }
}

/// The least a git read already begun is given once the deadline has nearly
/// passed. Cutting a status that is a few milliseconds from finishing would
/// report a reachable worktree as unreadable; this is the most a pass
/// overruns its deadline by, apart from the process-exit grace.
const STATUS_GRACE: Duration = Duration::from_millis(500);

/// One parsed block of `worktree list --porcelain -z`, before dirty counting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ParsedWorktree {
    pub(crate) path: String,
    pub(crate) head: String,
    pub(crate) branch: Option<String>,
    pub(crate) is_bare: bool,
    pub(crate) is_detached: bool,
    pub(crate) is_locked: bool,
    pub(crate) is_prunable: bool,
}

/// Every worktree of `repo`, from `git worktree list --porcelain -z`.
///
/// The one reader of the worktree list: the listing, the checked-out-branch
/// guards, the branch-cleanup plan and the agent-worktree reuse check all ask
/// here. Without `-z` a path containing a newline split into two fields (the
/// second read as an unknown line, or as a bogus entry), and a trailing space
/// was trimmed into a different directory.
pub(crate) fn read_worktree_entries(repo: &Path) -> Result<Vec<ParsedWorktree>, String> {
    let run =
        crate::engine::git_cli::git_captured(repo, &["worktree", "list", "--porcelain", "-z"])
            .and_then(|run| run.require_complete("Worktree listing"))?;
    if !run.success {
        return Err(format!(
            "Could not list this repository's worktrees: {}",
            String::from_utf8_lossy(&run.stderr).trim()
        ));
    }
    Ok(parse_worktree_porcelain_z(&String::from_utf8_lossy(
        &run.stdout,
    )))
}

/// Parses `git worktree list --porcelain -z` output.
///
/// Fields are NUL-terminated and a block ends with an empty field:
/// `worktree <path>`, `HEAD <oid>`, one of `branch <ref>` / `detached`,
/// optionally `bare`, `locked [reason]`, `prunable [reason]`. Unknown fields
/// are skipped rather than rejected so a newer git can add some without
/// breaking this parser. Values are taken verbatim: a path may end in a space.
fn parse_worktree_porcelain_z(raw: &str) -> Vec<ParsedWorktree> {
    let mut out: Vec<ParsedWorktree> = Vec::new();
    let mut current: Option<ParsedWorktree> = None;
    let flush = |current: &mut Option<ParsedWorktree>, out: &mut Vec<ParsedWorktree>| {
        if let Some(entry) = current.take() {
            if !entry.path.is_empty() {
                out.push(entry);
            }
        }
    };

    for field in raw.split('\0') {
        if field.is_empty() {
            flush(&mut current, &mut out);
            continue;
        }
        // A field with no payload ("detached", "bare", a bare "locked") has
        // no space to split at.
        let (key, value) = field.split_once(' ').unwrap_or((field, ""));
        if key == "worktree" {
            flush(&mut current, &mut out);
            current = Some(ParsedWorktree {
                path: value.to_string(),
                ..ParsedWorktree::default()
            });
            continue;
        }
        let Some(entry) = current.as_mut() else {
            continue;
        };
        match key {
            "HEAD" => entry.head = value.to_string(),
            "branch" => {
                entry.branch = Some(
                    value
                        .strip_prefix("refs/heads/")
                        .unwrap_or(value)
                        .to_string(),
                )
            }
            "detached" => entry.is_detached = true,
            "bare" => entry.is_bare = true,
            "locked" => entry.is_locked = true,
            "prunable" => entry.is_prunable = true,
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

/// The one `git status --porcelain -z` read of a worktree, which both the
/// dirty-file scan and the changed-path scan go through.
///
/// With a `deadline`, a read that has not started by then does not start, and
/// one in flight is given the time left (at least [`STATUS_GRACE`]) rather
/// than git's 90 s command timeout. Either way the caller gets an error that
/// says why, which is how a worktree it could not read stays "unknown".
fn read_porcelain(dir: &Path, deadline: Option<&ScanDeadline>) -> Result<String, String> {
    #[cfg(test)]
    status_probes::fire(dir);
    let args = ["status", "--porcelain", "-z"];
    match deadline {
        None => git_text(dir, &args),
        Some(deadline) if deadline.expired() => Err(DEADLINE_PASSED.to_string()),
        Some(deadline) => git_text_with_timeout(dir, &args, deadline.remaining().max(STATUS_GRACE)),
    }
}

/// Why a status that never started did not.
const DEADLINE_PASSED: &str = "the deadline passed before this worktree's status was read";

/// Test seam: runs a closure just before the status read of chosen worktrees,
/// so a test can make a read slow, or edit a worktree at a known moment,
/// without timing guesses. Keyed by canonical directory, so parallel tests in
/// their own temporary repositories never see each other's probes.
#[cfg(test)]
pub(crate) mod status_probes {
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, PoisonError};

    type Probe = Arc<dyn Fn(&Path) + Send + Sync>;
    static PROBES: Mutex<Vec<(u64, Vec<PathBuf>, Probe)>> = Mutex::new(Vec::new());
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    pub(super) fn fire(dir: &Path) {
        let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        let hits: Vec<Probe> = PROBES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(_, dirs, _)| dirs.contains(&canonical))
            .map(|(_, _, probe)| Arc::clone(probe))
            .collect();
        for probe in hits {
            probe(&canonical);
        }
    }

    /// Runs `body` with `probe` called before every status read in `dirs`.
    pub(crate) fn with_probe<T>(
        dirs: &[PathBuf],
        probe: impl Fn(&Path) + Send + Sync + 'static,
        body: impl FnOnce() -> T,
    ) -> T {
        struct Remove(u64);
        impl Drop for Remove {
            fn drop(&mut self) {
                PROBES
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .retain(|(id, _, _)| *id != self.0);
            }
        }
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dirs = dirs
            .iter()
            .map(|d| d.canonicalize().unwrap_or_else(|_| d.clone()))
            .collect();
        PROBES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((id, dirs, Arc::new(probe)));
        let _remove = Remove(id);
        body()
    }
}

/// Repo-relative paths with uncommitted changes in this worktree.
///
/// Porcelain only — no numstat — so collision scans stay cheap enough to run
/// across many worktrees. Past [`MAX_CHANGED_PATHS`] the vector is cut and
/// the caller sees `truncated`.
pub fn changed_paths(worktree_path: &str) -> Result<(Vec<String>, bool), String> {
    changed_paths_within(worktree_path, None)
}

/// [`changed_paths`] that stops waiting on git when `deadline` passes. A read
/// that is cut returns an error saying so, never an empty list.
pub fn changed_paths_within(
    worktree_path: &str,
    deadline: Option<&ScanDeadline>,
) -> Result<(Vec<String>, bool), String> {
    let repo = validate_repo(worktree_path)?;
    if !repo.is_dir() {
        return Err(format!(
            "worktree path is not a directory: {}",
            repo.display()
        ));
    }
    let stdout = read_porcelain(&repo, deadline)?;
    Ok(bounded_status_paths(stdout.as_bytes()))
}

/// The changed paths of one status read, cut at [`MAX_CHANGED_PATHS`], and
/// whether anything was cut. Stops parsing at the cap, so a worktree that
/// dirtied a hundred thousand files costs 257 strings, not a hundred thousand.
fn bounded_status_paths(bytes: &[u8]) -> (Vec<String>, bool) {
    let mut paths = status_paths_up_to(bytes, MAX_CHANGED_PATHS + 1);
    let truncated = paths.len() > MAX_CHANGED_PATHS;
    if truncated {
        paths.truncate(MAX_CHANGED_PATHS);
    }
    (paths, truncated)
}

/// Paths from `git status --porcelain -z`, including the origin of a rename,
/// until at least `limit` have been read.
fn status_paths_up_to(bytes: &[u8], limit: usize) -> Vec<String> {
    let mut paths = Vec::new();
    let mut i = 0usize;
    while i + 3 <= bytes.len() && paths.len() < limit {
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
    /// Adds diff stats (a `git diff` only where status found changes),
    /// divergence from the default branch, and portless routes.
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

/// The repository's default base as (short name, commit oid): `None` when no
/// candidate resolves, `Err` when the lookup itself could not be answered.
/// The one owner is [`crate::engine::git_reader::default_base`], the same
/// answer the branch list measures against.
type BaseRead = Result<Option<(String, String)>, String>;

fn read_base(repo: &Path) -> BaseRead {
    crate::engine::git_reader::default_base(repo)
        .checked()
        .map(|resolved| resolved.base)
}

/// Measures commit divergence (ahead/behind count) of `branch` from the
/// repository's default branch, whatever it is named.
pub fn measure_main_divergence(repo: &Path, branch: Option<&str>) -> Option<WorktreeDivergence> {
    let b = branch?;
    let (name, oid) = read_base(repo).ok()??;
    if b == name {
        return Some(WorktreeDivergence::default());
    }
    divergence_from(repo, &oid, b)
}

fn divergence_from(repo: &Path, base_oid: &str, b: &str) -> Option<WorktreeDivergence> {
    let stdout = git_text(
        repo,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{base_oid}...{b}"),
        ],
    )
    .ok()?;
    let mut parts = stdout.split_whitespace();
    let behind = parts.next()?.parse().ok()?;
    let ahead = parts.next()?.parse().ok()?;
    Some(WorktreeDivergence { ahead, behind })
}

/// What a listing measures divergence against: the default base, and every
/// scanned branch's counts from one `for-each-ref` when that could be read.
struct Base {
    default: BaseRead,
    /// `None` when the batch was not asked or did not answer; each worktree
    /// then measures itself, as it always did.
    batch: Option<HashMap<String, WorktreeDivergence>>,
}

impl Base {
    fn none() -> Self {
        Self {
            default: Ok(None),
            batch: None,
        }
    }
}

/// Every branch's divergence from the base commit `base_oid` in one
/// `for-each-ref`, where the scan used to run one `rev-list` per worktree. The
/// `ahead-behind` atom needs git 2.42; an older git, or any other failure,
/// answers `None` and each worktree falls back to its own `rev-list`.
///
/// A pattern matches its whole namespace (`refs/heads/x` also selects
/// `refs/heads/x/y`), so a row counts only when its refname is exactly a
/// branch that was asked for.
fn batch_divergence(
    repo: &Path,
    base_oid: &str,
    branches: &[&str],
) -> Option<HashMap<String, WorktreeDivergence>> {
    if branches.is_empty() {
        return Some(HashMap::new());
    }
    let format = format!("--format=%(refname)%00%(ahead-behind:{base_oid})");
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
    let parsed = read_worktree_entries(&repo)?;
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

    let class = crate::engine::git_cli::current_admission();
    // Asked once for the whole listing, and only when some scanned branch
    // is measured against it.
    let needs_base =
        depth == ScanDepth::Full && scan_targets.iter().any(|&idx| parsed[idx].branch.is_some());
    let base = if needs_base {
        let default = read_base(&repo);
        let batch = match &default {
            Ok(Some((name, oid))) => {
                let branches: Vec<&str> = scan_targets
                    .iter()
                    .filter_map(|&idx| parsed[idx].branch.as_deref())
                    .filter(|branch| branch != name)
                    .collect();
                batch_divergence(&repo, oid, &branches)
            }
            _ => None,
        };
        Base { default, batch }
    } else {
        Base::none()
    };
    let mut metrics = scan_in_waves(&scan_targets, class, None, |idx| {
        scan_worktree(&repo, &parsed[idx], depth, &base, None)
    });

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

/// Runs `scan` for each of `targets`, [`DIRTY_SCAN_FAN_OUT`] at a time, each
/// in `class`.
///
/// Scoped threads in bounded waves rather than rayon's pool: a pool thread
/// starts in the default admission class, so the scans of a listing asked for
/// as background work were promoted to refresh traffic.
///
/// With a `deadline`, no wave starts once it has passed. A target that was
/// never started has no entry in the result, which is how a caller tells "cut
/// off" from "scanned and found nothing"; the ones already running are bounded
/// by the same deadline inside their own status read.
fn scan_in_waves<F>(
    targets: &[usize],
    class: crate::engine::git_cli::Admission,
    deadline: Option<&ScanDeadline>,
    scan: F,
) -> HashMap<usize, WorktreeScan>
where
    F: Fn(usize) -> WorktreeScan + Sync,
{
    let mut metrics: HashMap<usize, WorktreeScan> = HashMap::new();
    for wave in targets.chunks(DIRTY_SCAN_FAN_OUT) {
        if deadline.is_some_and(ScanDeadline::expired) {
            break;
        }
        std::thread::scope(|scope| {
            let handles: Vec<_> = wave
                .iter()
                .map(|&idx| {
                    let scan = &scan;
                    let handle = scope
                        .spawn(move || crate::engine::git_cli::with_admission(class, || scan(idx)));
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
    metrics
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
        status_read: scan.status_read,
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
            None,
        )
    };
    info.dirty_files = scan.dirty;
    info.scan_note = scan.note;
    info.status_read = scan.status_read;
}

/// Reads the status of every scannable worktree of an already-made listing,
/// once, and attaches the answer to it: the dirty-file count, the changed
/// paths, or the reason there is neither. The listing and the read are separate
/// steps so a caller that wants the read to be the last thing it does can make
/// the listing first and the read when it has nothing else left to wait for.
///
/// Reads at most [`MAX_DIRTY_SCANS`] worktrees, [`DIRTY_SCAN_FAN_OUT`] at a
/// time, in the caller's admission class, and stops starting reads once
/// `deadline` has passed; one already running is given what time is left. A
/// worktree that was not read keeps `dirty_files: None` and a note that names
/// the deadline. Returns how many were cut off that way, so a caller can say
/// the pass was cut short instead of inferring it from a clock it read later.
pub fn read_dirty(list: &mut [WorktreeInfo], deadline: &ScanDeadline) -> usize {
    let targets: Vec<usize> = list
        .iter()
        .enumerate()
        .filter(|(_, info)| !info.is_bare)
        .map(|(idx, _)| idx)
        .take(MAX_DIRTY_SCANS)
        .collect();
    let entries: Vec<ParsedWorktree> = list
        .iter()
        .map(|info| ParsedWorktree {
            path: info.path.clone(),
            branch: info.branch.clone(),
            is_bare: info.is_bare,
            ..ParsedWorktree::default()
        })
        .collect();
    let class = crate::engine::git_cli::current_admission();
    let mut scans = scan_in_waves(&targets, class, Some(deadline), |idx| {
        // The repository path is read only at `Full` depth.
        scan_worktree(
            Path::new(&entries[idx].path),
            &entries[idx],
            ScanDepth::Dirty,
            &Base::none(),
            Some(deadline),
        )
    });
    let mut cut = 0;
    for (idx, info) in list.iter_mut().enumerate() {
        let scan = match scans.remove(&idx) {
            Some(scan) => scan,
            None if targets.contains(&idx) => WorktreeScan::cut_off(),
            None => WorktreeScan {
                note: Some(if info.is_bare {
                    "bare entry: no working tree to scan".to_string()
                } else {
                    format!("not scanned: past the {MAX_DIRTY_SCANS}-worktree scan limit")
                }),
                ..WorktreeScan::default()
            },
        };
        cut += usize::from(scan.cut);
        info.dirty_files = scan.dirty;
        info.scan_note = scan.note;
        info.status_read = scan.status_read;
    }
    cut
}

/// What one worktree's scan measured, and why anything is missing.
#[derive(Default)]
struct WorktreeScan {
    dirty: Option<usize>,
    diff_stat: Option<WorktreeDiffStat>,
    divergence: Option<WorktreeDivergence>,
    routes: Vec<WorktreeRouteInfo>,
    note: Option<String>,
    status_read: StatusRead,
    /// The deadline passed before this worktree's status was read, so it was
    /// not: neither scanned nor failed, and counted as cut off.
    cut: bool,
}

impl WorktreeScan {
    fn cut_off() -> Self {
        Self {
            note: Some(format!("not scanned: {DEADLINE_PASSED}")),
            cut: true,
            ..Self::default()
        }
    }
}

/// Whether a listed `HEAD` names a commit: git lists an unborn branch's head
/// as all zeros, and an entry it gave no head for is empty.
fn head_is_born(head: &str) -> bool {
    head.len() >= 40
        && head.bytes().all(|b| b.is_ascii_hexdigit())
        && head.bytes().any(|b| b != b'0')
}

fn scan_worktree(
    repo: &Path,
    entry: &ParsedWorktree,
    depth: ScanDepth,
    base: &Base,
    deadline: Option<&ScanDeadline>,
) -> WorktreeScan {
    let dir = Path::new(&entry.path);
    if !dir.is_dir() {
        let note = "the worktree directory is missing".to_string();
        return WorktreeScan {
            status_read: StatusRead::Failed(note.clone()),
            note: Some(note),
            ..WorktreeScan::default()
        };
    }
    // The measurements below swallow their own errors as "absent"; the
    // failure counter is what says whether one was a refusal rather than a
    // real absence (no upstream, no main branch).
    let failures = crate::engine::git_cli::process_failures();
    let status = match read_porcelain(dir, deadline) {
        Ok(status) => status,
        Err(reason) if reason == DEADLINE_PASSED => return WorktreeScan::cut_off(),
        Err(reason) => {
            return WorktreeScan {
                status_read: StatusRead::Failed(reason.clone()),
                note: Some(format!("status could not be read: {reason}")),
                ..WorktreeScan::default()
            }
        }
    };
    let dirty = Some(count_status_entries(status.as_bytes()));
    let (paths, truncated) = bounded_status_paths(status.as_bytes());
    let status_read = StatusRead::Read { paths, truncated };
    if depth != ScanDepth::Full {
        return WorktreeScan {
            dirty,
            status_read,
            ..WorktreeScan::default()
        };
    }
    let branch = entry.branch.as_deref();
    let divergence = branch.and_then(|b| {
        let Ok(Some((name, oid))) = &base.default else {
            return None;
        };
        if b == name {
            Some(WorktreeDivergence::default())
        } else if let Some(batch) = &base.batch {
            // A branch the batch did not list is not a ref (unborn), which
            // its own `rev-list` could not measure either.
            batch.get(b).cloned()
        } else {
            divergence_from(repo, oid, b)
        }
    });
    let mut scan = WorktreeScan {
        dirty,
        diff_stat: if dirty == Some(0) && head_is_born(&entry.head) {
            // `git status` just found HEAD, the index and the files in
            // agreement, which is exactly an empty `git diff HEAD`; asking
            // again cost a second child per worktree, and most worktrees in
            // a listing are clean. An unborn or unknown HEAD is measured.
            Some(WorktreeDiffStat::default())
        } else {
            measure_diff_stat(dir)
        },
        divergence,
        routes: detect_worktree_routes(&entry.path, branch),
        note: None,
        status_read,
        cut: false,
    };
    // The listing's one default-branch lookup failed outside this scan's
    // failure window, so it is named here or not at all. Without the base no
    // branch is known to be it, so every branch's divergence is unmeasured.
    if let (Err(reason), Some(_)) = (&base.default, branch) {
        scan.note = Some(format!("some measurements were not taken: {reason}"));
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

    // The spelling the gate judged ([`add_worktree_argv`]): a verbatim
    // `\\?\` path from a canonicalized checkout is not one git can create.
    let target_path = plain_path_text(target_path);
    let target_path = target_path.as_str();
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
    /// Some build cache could not be copied from the main checkout, or could
    /// not be checked: each one named, with what was copied. A cache that
    /// failed is absent from the worktree, never half there.
    pub cache_error: Option<String>,
    /// A `post_create` hook failed, timed out, or was refused.
    pub hook_error: Option<String>,
    /// Hooks were asked for and the repository's file lists some that were not
    /// approved, so none ran. Not a failure: the worktree is as plain as one
    /// made without hooks, and this says why.
    #[serde(default)]
    pub hook_notice: Option<String>,
}

/// Creates a linked worktree with optional Copy-on-Write cache cloning and,
/// when `run_hooks` is set, the repository's approved `post_create` hooks.
pub fn add_worktree_extended(
    repo_path: &str,
    target_path: &str,
    new_branch: Option<&str>,
    start_point: Option<&str>,
    detach: bool,
    cow_caches: bool,
    run_hooks: bool,
) -> Result<WorktreeCreated, String> {
    let path = add_worktree(repo_path, target_path, new_branch, start_point, detach)?;
    let mut created = WorktreeCreated {
        path,
        cache_error: None,
        hook_error: None,
        hook_notice: None,
    };
    if cow_caches {
        let repo = validate_repo(repo_path)?;
        created.cache_error = reflink_ignored_caches(
            &repo,
            Path::new(target_path),
            &crate::engine::cow_clone::CacheSelection::default(),
        )
        .err();
    }
    if run_hooks {
        let repo = validate_repo(repo_path)?;
        let branch_name = new_branch.unwrap_or("");
        match run_repo_hooks(
            &repo,
            Path::new(target_path),
            "post_create",
            &[
                ("GITPULSE_BRANCH", branch_name),
                ("GITPULSE_WORKTREE_PATH", target_path),
            ],
        ) {
            Ok(outcome) => created.hook_notice = outcome.skipped,
            Err(e) => created.hook_error = Some(e),
        }
    }
    Ok(created)
}

/// Outcome of merging a worktree branch and tearing down the worktree directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MergeTeardownResult {
    /// The merge landed on the target. An `Ok` result is only ever one that
    /// did; a merge that did not land is a [`MergeTeardownError`].
    pub merged: bool,
    pub merged_branch: String,
    pub target_branch: String,
    pub commits_merged: usize,
    /// The target's tip before the merge.
    pub before_oid: String,
    /// The target's tip after the merge, as read back. `None` only when git
    /// could not answer after a merge that had landed; the worktree is then
    /// kept and `removal_error` says so.
    pub after_oid: Option<String>,
    /// Where the lane was cut from, when [`write_base_record`] recorded it.
    pub base_oid: Option<String>,
    /// False when the merge landed but the worktree is still there; the
    /// reason is `removal_error`. This is a cleanup left to do, not a failed
    /// merge.
    pub worktree_removed: bool,
    pub removal_error: Option<String>,
    pub branch_deleted: bool,
    /// Why the branch is still there. Always set when `branch_deleted` is
    /// false.
    pub branch_error: Option<String>,
    /// A `post_merge` hook failed, timed out, or was refused. The merge and
    /// the teardown had already happened; this says the cleanup after them
    /// did not, where it used to be discarded.
    pub hook_error: Option<String>,
    /// Hooks the repository lists that were not approved, so did not run.
    #[serde(default)]
    pub hook_notice: Option<String>,
    /// The worktree is gone as far as git is concerned (`worktree_removed` is
    /// true), but the files moved aside for deletion could not all be deleted,
    /// or a leftover from an earlier teardown could not be cleared. Names the
    /// path. The next teardown retries it.
    #[serde(default)]
    pub cleanup_error: Option<String>,
    /// Leftovers of earlier teardowns the sweep before this one could not
    /// clear (a symlink, a directory held back, one that would not delete),
    /// and a cap that was hit. About the repository, not about this removal,
    /// so they are not in `cleanup_error`. Bounded: at most 17 short notes.
    #[serde(default)]
    pub sweep_problems: Vec<String>,
}

/// Why a merge and teardown did not merge. It crosses the IPC boundary as
/// `{code, message, ...}`: every variant carries a `message` fit to show, and
/// the code is what a caller (the wave) branches on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum MergeTeardownError {
    /// The merge conflicted. `rolled_back` says the main checkout is as it
    /// was; when false, `rollback_error` names what was left parked there.
    Conflict {
        message: String,
        paths: Vec<String>,
        total: usize,
        rolled_back: bool,
        rollback_error: Option<String>,
    },
    /// The branch is not at the commit the caller checked. Nothing was merged.
    Moved {
        message: String,
        branch: String,
        expected: String,
        actual: String,
    },
    /// The worktree was cut from a branch or a base the target no longer is.
    BaseMismatch {
        message: String,
        base_branch: String,
        target_branch: String,
        base_oid: String,
    },
    /// A squash is staged in the main checkout and not committed: this
    /// merge's own commit step failed, or an earlier one's did.
    SquashParked { message: String },
    /// Refused or failed before anything was merged.
    Refused { message: String },
}

impl MergeTeardownError {
    pub fn message(&self) -> &str {
        match self {
            Self::Conflict { message, .. }
            | Self::Moved { message, .. }
            | Self::BaseMismatch { message, .. }
            | Self::SquashParked { message }
            | Self::Refused { message } => message,
        }
    }
}

impl std::fmt::Display for MergeTeardownError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for MergeTeardownError {}

impl From<String> for MergeTeardownError {
    fn from(message: String) -> Self {
        Self::Refused { message }
    }
}

/// Written into a lane's private Git directory when it is provisioned: the
/// branch it was cut from and that branch's tip then, as `<branch>\n<oid>\n`.
pub const BASE_RECORD: &str = "gitpulse-base";

/// Records where `worktree` was cut from, next to the other per-worktree
/// state, never in its files where `git status` would show it.
pub fn write_base_record(worktree: &Path, branch: &str, oid: &str) -> Result<(), String> {
    validate_ref_name(branch)?;
    if !is_full_oid(oid) {
        return Err(format!(
            "{oid} is not a full commit id, so it is not recorded as a base"
        ));
    }
    let dir = resolve_git_dir(worktree)?;
    std::fs::write(dir.join(BASE_RECORD), format!("{branch}\n{oid}\n")).map_err(|e| {
        format!(
            "could not write {BASE_RECORD} for {}: {e}",
            worktree.display()
        )
    })
}

/// The `(branch, oid)` [`write_base_record`] recorded. `None` when there is
/// no record (a lane made before it existed, or by the Worktrees panel). A
/// record that is there and cannot be read is an error, not "no record": a
/// check that could not run must not pass as one that did.
fn read_base_record(worktree: &Path) -> Result<Option<(String, String)>, String> {
    let path = resolve_git_dir(worktree)?.join(BASE_RECORD);
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    let corrupt = |why: &str| format!("{} is not a branch and a commit id: {why}", path.display());
    if raw.len() > 4096 {
        return Err(corrupt("it is far larger than it should be"));
    }
    let text = String::from_utf8(raw).map_err(|_| corrupt("it is not text"))?;
    let mut lines = text.lines().map(str::trim);
    let (Some(branch), Some(oid), None) = (lines.next(), lines.next(), lines.next()) else {
        return Err(corrupt("it does not have exactly two lines"));
    };
    validate_ref_name(branch).map_err(|e| corrupt(&e))?;
    // A full id only: an abbreviation is whatever git resolves it to today.
    if !is_full_oid(oid) {
        return Err(corrupt(
            "the commit id is not a full 40 or 64 character hex id",
        ));
    }
    Ok(Some((branch.to_string(), oid.to_ascii_lowercase())))
}

/// How many times the removal of a merged worktree is tried. A scanner or an
/// indexer holding a file open (Windows) fails the first try and lets go.
const MAX_REMOVE_ATTEMPTS: usize = 3;
/// Waits before the second and third try.
const REMOVE_BACKOFF: [Duration; MAX_REMOVE_ATTEMPTS - 1] =
    [Duration::from_millis(100), Duration::from_millis(300)];

/// Merges a worktree branch into target_branch (defaulting to the repository's default branch) in the primary
/// repository checkout, tears down the worktree cleanly, and prunes the merged branch.
///
/// `gate` is asked about each git command before it runs, with git's
/// arguments as they actually run (no leading `git`): the merge of *this
/// worktree's* branch, the squash commit, the forced worktree removal and the
/// branch deletion. The caller used to ask once, up front, about
/// `merge --ff-only <target>` — a command this never ran, and spelled without
/// the program, so it was judged as a program named `merge` — and the three
/// later mutations were not asked about at all. Restoring a merge that
/// conflicted is asked about too (`merge --abort`, or `reset --merge` for a
/// squash). A refusal is honoured: nothing is run, and the error is a
/// `Conflict` with `rolled_back: false` carrying the gate's reason, leaving
/// the checkout parked for a person.
///
/// `expected_tip` is the commit the caller verified the branch at. When it is
/// given and the branch is anywhere else under the lock, nothing is merged.
///
/// The removal holds the repository lock across a rename, not a recursive
/// delete: the worktree directory is moved aside and its record removed under
/// the lock, and the files are deleted after it is released (see
/// [`remove_worktree_split`]). The gate is still asked about
/// `worktree remove --force <wt>`: the effect is the same and no new git verb
/// runs. A delete that fails leaves `worktree_removed: true` and a
/// `cleanup_error` naming the path; the next teardown's sweep retries it.
///
/// The result is `Err` only while nothing has landed. A merge that did land
/// is `Ok` whatever happens to the cleanup after it: a worktree or branch
/// that could not be removed is `worktree_removed: false` / `branch_deleted:
/// false` with the reason, never a "not merged".
pub fn merge_and_teardown_worktree(
    repo_path: &str,
    worktree_path: &str,
    target_branch: Option<&str>,
    squash: bool,
    expected_tip: Option<&str>,
    gate: &mut dyn FnMut(&[&str]) -> Result<(), String>,
) -> Result<MergeTeardownResult, MergeTeardownError> {
    merge_and_teardown_with(
        repo_path,
        worktree_path,
        target_branch,
        squash,
        expected_tip,
        gate,
        &mut std::thread::sleep,
    )
}

/// [`merge_and_teardown_worktree`] with the wait between removal attempts
/// passed in, so a test does not sleep through the backoff.
fn merge_and_teardown_with(
    repo_path: &str,
    worktree_path: &str,
    target_branch: Option<&str>,
    squash: bool,
    expected_tip: Option<&str>,
    gate: &mut dyn FnMut(&[&str]) -> Result<(), String>,
    sleep: &mut dyn FnMut(Duration),
) -> Result<MergeTeardownResult, MergeTeardownError> {
    merge_and_teardown_with_seams(
        repo_path,
        worktree_path,
        target_branch,
        squash,
        expected_tip,
        gate,
        sleep,
        &mut delete_dir_tree,
    )
}

/// [`merge_and_teardown_with`] with the deletion of the moved-aside worktree
/// passed in as well (`delete_aside`), so a test can hold it open and look at
/// what else can run meanwhile.
#[allow(clippy::too_many_arguments)]
fn merge_and_teardown_with_seams(
    repo_path: &str,
    worktree_path: &str,
    target_branch: Option<&str>,
    squash: bool,
    expected_tip: Option<&str>,
    gate: &mut dyn FnMut(&[&str]) -> Result<(), String>,
    sleep: &mut dyn FnMut(Duration),
    delete_aside: &mut dyn FnMut(&Path) -> std::io::Result<()>,
) -> Result<MergeTeardownResult, MergeTeardownError> {
    // The tip comes from the webview; it is a commit id or it is refused.
    let expected_tip = match expected_tip.map(str::trim) {
        None => None,
        Some(tip) if is_full_oid(tip) => Some(tip.to_ascii_lowercase()),
        Some(tip) => {
            return Err(format!(
                "The expected branch tip '{tip}' is not a full commit id; nothing was merged."
            )
            .into())
        }
    };
    let family = resolve_worktree_family(repo_path, worktree_path)?;
    if family.worktree == family.anchor {
        return Err(
            "Cannot merge and teardown the repository's primary checkout"
                .to_string()
                .into(),
        );
    }

    let repo = family.anchor.clone();
    let worktree = family.worktree.clone();

    // Files an earlier teardown moved aside and did not finish deleting: the
    // sweep is their retry. It takes no lock; it is outside the merge's own.
    let sweep_problems = sweep_teardown_asides(&repo).notes();

    // 1. Resolve branch name
    let branch_raw = git_text(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let branch = branch_raw.trim().to_string();
    if branch.is_empty() || branch == "HEAD" {
        return Err(
            "Worktree has a detached HEAD; cannot merge an unbranched checkout"
                .to_string()
                .into(),
        );
    }

    // No target means the repository's default branch. This action destroys
    // the lane, so the default is read uncached: a memoized answer from before
    // the read started failing, or before the default moved, must not decide
    // where the merge lands. An unreadable or absent default refuses; nothing
    // here guesses a branch name. Every check below still runs on the result.
    let resolved_default;
    let target = match target_branch {
        Some(target) => target,
        None => {
            let default = crate::engine::git_reader::compute_default_base(&repo)
                .checked()
                .map_err(|reason| {
                    format!("The default branch could not be read: {reason}. Nothing was merged.")
                })?;
            let (name, _) = default.base.ok_or_else(|| {
                "No default branch could be resolved; name a target branch. Nothing was merged."
                    .to_string()
            })?;
            resolved_default = name;
            resolved_default.as_str()
        }
    };
    validate_ref_name(target)?;

    // The merge runs in the main checkout, onto whatever it has checked out.
    // It used to run there regardless and then report `target`: a main
    // checkout on another branch took the merge, and the panel said "main".
    let main_is_on_target = || -> Result<(), String> {
        // A refused read is an error, not "a detached HEAD": naming the wrong
        // reason would send the user to fix the wrong thing.
        let current = crate::engine::git_reader::head_branch(&repo)?.unwrap_or_default();
        if current != target {
            return Err(format!(
                "The main checkout is on {}, not {target}. Switch it to {target} first; nothing was merged.",
                if current.is_empty() { "a detached HEAD".to_string() } else { current }
            ));
        }
        Ok(())
    };
    main_is_on_target()?;

    // 2. Pre-merge hooks
    let mut hook_notices: Vec<String> = Vec::new();
    hook_notices.extend(
        run_repo_hooks(
            &repo,
            &worktree,
            "pre_merge",
            &[
                ("GITPULSE_BRANCH", &branch),
                ("GITPULSE_WORKTREE_PATH", &worktree.to_string_lossy()),
            ],
        )?
        .skipped,
    );

    // Steps 3-6 hold one lock: the checks that the worktree is clean and that
    // the branch is what will be merged, the merge, and the removal and branch
    // deletion that destroy the worktree. They used to run in separate scopes,
    // so anything written between the check and the removal was destroyed with
    // it. The lock fences GitPulse's own writers; an outside process takes no
    // lock, so each destructive step re-reads the state it is about to destroy
    // and refuses, keeping the worktree and branch, when it moved.
    let _repo_lock = repo_mutation_lock(&repo);
    let _guard = _repo_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // The pre-merge hooks above may have switched the main checkout.
    main_is_on_target()?;

    // A merge, rebase or squash parked in the main checkout is somebody's
    // unfinished work. Merging over it would tangle the two, and rolling a
    // failure back would undo theirs.
    if let Some(kind) = crate::engine::repo_op::detect_kind(&repo)? {
        // A squash probe that could not read the index says so; that is a
        // read failure to report, not a squash known to be parked.
        if kind == crate::engine::repo_op::OperationKind::SquashMerge {
            let unread = crate::engine::repo_op::detect(&repo)?.and_then(|op| {
                op.warnings
                    .into_iter()
                    .find(|w| w.starts_with(crate::engine::repo_op::SQUASH_READ_FAILURE))
            });
            if let Some(why) = unread {
                return Err(format!(
                    "The main checkout's index could not be read ({why}), so it cannot be shown free of a parked squash; nothing was merged."
                )
                .into());
            }
        }
        return Err(
            if kind == crate::engine::repo_op::OperationKind::SquashMerge {
                MergeTeardownError::SquashParked {
                    message: format!(
                    "A squash merge is parked in the main checkout: its changes are staged and not \
                     committed. Commit them, or undo them with `git reset --merge`, before merging \
                     {branch}. Nothing was merged."
                ),
                }
            } else {
                MergeTeardownError::Refused {
                message: format!(
                    "A {} is in progress in the main checkout. Finish or abort it before merging {branch}; nothing was merged.",
                    kind.label()
                ),
            }
            },
        );
    }

    // Unmerged index entries with no merge in progress are the user's own (a
    // conflicted stash pop they are resolving). Git would refuse to merge over
    // them, and a rollback of that refusal would reset their resolution away,
    // so none of that is started. What a rollback later undoes is then
    // provably this call's: the checkout had none before it.
    let (unmerged, unmerged_total) =
        crate::engine::repo_op::conflicted_paths(&repo).map_err(|e| {
            format!(
                "The main checkout's unmerged files could not be checked: {e}. Nothing was merged."
            )
        })?;
    if unmerged_total > 0 {
        let shown: Vec<&str> = unmerged.iter().take(5).map(String::as_str).collect();
        return Err(format!(
            "The main checkout has {unmerged_total} unmerged file(s) with no merge in progress ({}{}). Resolve and commit them, or restore them, before merging {branch}; nothing was merged and nothing was reset.",
            shown.join(", "),
            if unmerged_total > shown.len() { ", ..." } else { "" }
        )
        .into());
    }

    // 3. Collision / uncommitted changes check
    let worktree_text = worktree.to_string_lossy().to_string();
    let (changed, _) = changed_paths(&worktree_text)?;
    if !changed.is_empty() {
        return Err(format!(
            "Worktree has {} uncommitted file changes; commit or stash them before merging",
            changed.len()
        )
        .into());
    }
    // The branch tip being merged. Anything that lands on the branch after
    // this is not in the merge and must not be deleted with it.
    let branch_ref = format!("refs/heads/{branch}");
    let merged_tip = git_text(&repo, &["rev-parse", "--verify", branch_ref.as_str()])?
        .trim()
        .to_string();
    if let Some(expected) = expected_tip {
        if expected != merged_tip {
            return Err(MergeTeardownError::Moved {
                message: format!(
                    "{branch} is at {merged_tip}, not the {expected} that was checked. \
                     It moved after the check, so nothing was merged and the worktree was kept."
                ),
                branch: branch.clone(),
                expected,
                actual: merged_tip,
            });
        }
    }

    // The target the merge lands on. An unborn one has no tip to merge onto.
    let target_ref = format!("refs/heads/{target}");
    let before_oid = git_text(&repo, &["rev-parse", "--verify", target_ref.as_str()])
        .map_err(|e| format!("{target} has no commits to merge onto ({e}); nothing was merged."))?
        .trim()
        .to_string();

    // The lane is merged into the branch it was cut from. A main checkout
    // switched to another branch since, or a target rewritten so that the
    // lane's base is no longer in it, would take the merge somewhere the
    // lane was never based on.
    let base_oid = match read_base_record(&worktree)? {
        None => None,
        Some((base_branch, base_oid)) => {
            let mismatch = |message: String| MergeTeardownError::BaseMismatch {
                message,
                base_branch: base_branch.clone(),
                target_branch: target.to_string(),
                base_oid: base_oid.clone(),
            };
            if base_oid.len() != before_oid.len() {
                return Err(format!(
                    "{branch}'s recorded base {base_oid} is not in this repository's object format ({}-character ids); nothing was merged.",
                    before_oid.len()
                )
                .into());
            }
            if base_branch != target {
                return Err(mismatch(format!(
                    "{branch} was cut from {base_branch}, but the merge target is {target}. Nothing was merged."
                )));
            }
            let ancestor = crate::engine::git_cli::git_captured(
                &repo,
                &[
                    "merge-base",
                    "--is-ancestor",
                    base_oid.as_str(),
                    target_ref.as_str(),
                ],
            )?;
            match ancestor.status_code {
                0 => {}
                1 => {
                    return Err(mismatch(format!(
                        "{branch} was cut from {base_oid} on {base_branch}, which {target} no longer contains: \
                         incompatible merge-base. Nothing was merged."
                    )))
                }
                code => {
                    return Err(format!(
                        "Could not check whether {target} still contains {base_oid} (git exit {code}): {}. Nothing was merged.",
                        String::from_utf8_lossy(&ancestor.stderr).trim()
                    )
                    .into())
                }
            }
            Some(base_oid)
        }
    };

    // Count commits being merged
    let commits_merged: usize = git_text(
        &repo,
        &["rev-list", "--count", &format!("{target}..{branch}")],
    )?
    .trim()
    .parse()
    .map_err(|_| format!("git did not report how many commits {branch} adds to {target}"))?;

    // 4. Merge in anchor repository
    if squash {
        // Nothing staged means the branch's changes are already on the
        // target and there is nothing to commit — not a failure. A failed
        // commit fails closed before anything is torn down: it used to be
        // discarded, and the next steps removed the worktree and
        // force-deleted the branch whose work was never committed.
        GitWriter::squash_merge_locked(&repo, &branch, gate).map_err(|failure| match failure {
            SquashFailure::Merge(e) => MergeTeardownError::Refused { message: e },
            SquashFailure::Conflict(e) => {
                failed_merge_error(gate, &repo, &branch, target, &merged_tip, e)
            }
            // Staged, not rolled back: `git reset --merge` is the user's call.
            SquashFailure::Commit(e) => MergeTeardownError::SquashParked {
                message: format!("{e} The worktree and the branch {branch} were kept on {target}."),
            },
        })?;
    } else {
        let fast_forward = merge_teardown_argv(&branch, false);
        let fast_forward: Vec<&str> = fast_forward.iter().map(String::as_str).collect();
        gate(&fast_forward)?;
        if let Err(ff_err) = git_text(&repo, &fast_forward) {
            // The same two guards the squash runs, before the commit a merge
            // commit is.
            GitWriter::require_commit_identity(&repo)?;
            let commit_msg = format!("Merge branch '{branch}' into {target}");
            let merge = [
                "merge",
                "--no-edit",
                branch.as_str(),
                "-m",
                commit_msg.as_str(),
            ];
            gate(&merge)?;
            if let Err(e) = git_text(&repo, &merge) {
                return Err(failed_merge_error(
                    gate,
                    &repo,
                    &branch,
                    target,
                    &merged_tip,
                    format!("Merge failed (ff error: {ff_err}): {e}"),
                ));
            }
        }
    }

    // From here the merge has landed. Nothing below returns `Err`: what it
    // cannot finish is reported on the result.
    let (after_oid, after_error) =
        match git_text(&repo, &["rev-parse", "--verify", target_ref.as_str()]) {
            Ok(tip) => (Some(tip.trim().to_string()), None),
            Err(e) => (
                None,
                Some(format!(
                    "the new tip of {target} could not be read ({e}), so the worktree and the branch were kept"
                )),
            ),
        };

    // What the destructive steps below may destroy is what was merged.
    let unchanged_since_merge = |repo: &Path, worktree_present: bool| -> Result<(), String> {
        let kept = format!(
            "The merge of {branch} into {target} is done; the worktree and the branch were kept"
        );
        if worktree_present {
            let (changed, _) = changed_paths(&worktree_text).map_err(|e| {
                format!("{kept}, because the worktree could not be re-checked: {e}")
            })?;
            if !changed.is_empty() {
                return Err(format!(
                    "{kept}: {} file change(s) appeared in the worktree after the merge was checked.",
                    changed.len()
                ));
            }
        }
        let tip = git_text(repo, &["rev-parse", "--verify", branch_ref.as_str()])
            .map_err(|e| format!("{kept}, because the branch could not be re-read: {e}"))?;
        if tip.trim() != merged_tip {
            return Err(format!(
                "{kept}: {branch} moved from {merged_tip} to {} after the merge was checked.",
                tip.trim()
            ));
        }
        Ok(())
    };

    // 5. Remove worktree
    let remove = remove_worktree_argv(&worktree_text, true);
    // That builder spells the program; the gate takes git's arguments.
    let remove: Vec<&str> = remove.iter().skip(1).map(String::as_str).collect();
    // The gate is asked about `worktree remove --force <wt>` and not about the
    // rename: the effect is the same (the lane is gone from the checkout and
    // from git's list) and no new git verb runs.
    let removal = match after_error {
        Some(kept) => Err(kept),
        None => gate(&remove)
            .and_then(|()| unchanged_since_merge(&repo, true))
            .and_then(|()| {
                remove_worktree_split(&repo, &worktree_text, true, &mut || {
                    remove_with_retry(&repo, &worktree_text, sleep)
                })
            }),
    };
    let (worktree_removed, removal_error, aside) = match removal {
        Ok(aside) => (true, None, aside),
        Err(e) => (false, Some(e), None),
    };

    // 6. Delete merged branch. A branch checked out in a worktree that is
    // still there cannot be deleted, and is not wanted deleted.
    let delete = ["branch", if squash { "-D" } else { "-d" }, branch.as_str()];
    let branch_result = if worktree_removed {
        gate(&delete)
            .and_then(|()| unchanged_since_merge(&repo, false))
            .and_then(|()| git_text(&repo, &delete).map(drop))
    } else {
        Err(format!(
            "{branch} was kept because its worktree is still there"
        ))
    };
    let (branch_deleted, branch_error) = match branch_result {
        Ok(()) => (true, None),
        Err(e) => (false, Some(e)),
    };
    drop(_guard);

    // The lane's files, moved aside under the lock, are deleted without it.
    let cleanup_error = finish_teardown(aside, delete_aside);

    // 7. Post-merge hooks
    let mut hook_error = None;
    match run_repo_hooks(
        &repo,
        &repo,
        "post_merge",
        &[
            ("GITPULSE_BRANCH", &branch),
            ("GITPULSE_TARGET_BRANCH", target),
        ],
    ) {
        Ok(outcome) => hook_notices.extend(outcome.skipped),
        Err(e) => hook_error = Some(e),
    }

    Ok(MergeTeardownResult {
        merged: true,
        merged_branch: branch,
        target_branch: target.to_string(),
        commits_merged,
        before_oid,
        after_oid,
        base_oid,
        worktree_removed,
        removal_error,
        branch_deleted,
        branch_error,
        hook_error,
        hook_notice: if hook_notices.is_empty() {
            None
        } else {
            Some(hook_notices.join(" "))
        },
        cleanup_error,
        sweep_problems,
    })
}

/// What a merge that failed after it started leaves the caller: a conflict,
/// with the checkout restored, or, when git failed for a reason that changed
/// nothing, a plain refusal. Runs under the repository lock.
fn failed_merge_error(
    gate: &mut dyn FnMut(&[&str]) -> Result<(), String>,
    repo: &Path,
    branch: &str,
    target: &str,
    merged_tip: &str,
    cause: String,
) -> MergeTeardownError {
    let (paths, total, listing_error) = match crate::engine::repo_op::conflicted_paths(repo) {
        Ok((paths, total)) => (paths, total, None),
        Err(e) => (Vec::new(), 0, Some(e)),
    };
    // A failure that left no operation and no unmerged entry changed
    // nothing (a dirty file the merge would overwrite, a refused hook). One
    // that could not be read is assumed to have.
    let started = listing_error.is_some()
        || total > 0
        || !matches!(crate::engine::repo_op::detect_kind(repo), Ok(None));
    let rollback = GitWriter::rollback_failed_merge_gated(repo, merged_tip, gate);
    if !started && rollback.is_ok() {
        return MergeTeardownError::Refused {
            message: format!("Merging {branch} into {target} failed; nothing was changed: {cause}"),
        };
    }
    let what = if total > 0 {
        let shown: Vec<&str> = paths.iter().take(5).map(String::as_str).collect();
        let more = if total > shown.len() {
            format!(" and {} more", total - shown.len())
        } else {
            String::new()
        };
        format!("conflicted in {total} file(s): {}{more}", shown.join(", "))
    } else {
        format!("failed part-way: {cause}")
    };
    let (rolled_back, rollback_error) = match rollback {
        Ok(()) => (true, None),
        Err(e) => (false, Some(e)),
    };
    let outcome = match &rollback_error {
        None => "It was rolled back; nothing was merged and the worktree and the branch were kept."
            .to_string(),
        Some(e) => format!(
            "It could not be rolled back ({e}): the main checkout is left in the middle of it. \
             Abort the merge there before merging anything else."
        ),
    };
    let listing_note = listing_error
        .map(|e| format!(" The conflicted files could not be listed: {e}."))
        .unwrap_or_default();
    MergeTeardownError::Conflict {
        message: format!("Merging {branch} into {target} {what}. {outcome}{listing_note}"),
        paths,
        total,
        rolled_back,
        rollback_error,
    }
}

/// A worktree path as git compares it: the directory beside it resolved (a
/// symlinked temp or home directory is written by git in its real form), and
/// its own name kept, so it can be told apart after the directory is gone.
fn lane_identity(path: &Path) -> PathBuf {
    match (
        path.parent().and_then(|parent| parent.canonicalize().ok()),
        path.file_name(),
    ) {
        (Some(parent), Some(name)) => parent.join(name),
        _ => path.to_path_buf(),
    }
}

/// Whether git still lists `worktree_text` as a worktree of `repo`.
fn still_listed(repo: &Path, worktree_text: &str) -> Result<bool, String> {
    let wanted = lane_identity(Path::new(worktree_text));
    Ok(read_worktree_entries(repo)?
        .iter()
        .any(|entry| lane_identity(Path::new(&entry.path)) == wanted))
}

/// Finishes the removal of a worktree whose directory is already gone (a
/// partial delete) by removing its own record, `<common>/worktrees/<id>`,
/// and nothing else. `worktree prune` does this for every stale record in the
/// repository, including a lane on a volume that is unmounted right now. The
/// record is found by its `gitdir` file pointing at this worktree, and a
/// locked one is left, since a lock is somebody's statement that it stays.
fn remove_own_worktree_record(repo: &Path, worktree_text: &str) -> Result<(), String> {
    let Some(record) = find_own_worktree_record(repo, worktree_text)? else {
        return Ok(());
    };
    if record.join("locked").exists() {
        return Err(format!(
            "its record {} is locked, so it was left",
            record.display()
        ));
    }
    std::fs::remove_dir_all(&record)
        .map_err(|e| format!("could not remove its record {}: {e}", record.display()))
}

/// The directories of `<common>/worktrees` (one record per linked worktree)
/// with the path each one's `gitdir` file points at, which is `<worktree>/.git`.
fn worktree_records(repo: &Path) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    let records = resolve_git_common_dir(repo)?.join("worktrees");
    let listing = std::fs::read_dir(&records)
        .map_err(|e| format!("could not read {}: {e}", records.display()))?;
    let mut found = Vec::new();
    for entry in listing {
        let record = entry
            .map_err(|e| format!("could not read {}: {e}", records.display()))?
            .path();
        // A record with no `gitdir` yet is one being made. One that cannot be
        // read is not skipped: a caller would then read it as pointing nowhere.
        let pointer = match std::fs::read(record.join("gitdir")) {
            Ok(pointer) => pointer,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(format!(
                    "could not read {}: {e}",
                    record.join("gitdir").display()
                ))
            }
        };
        if pointer.len() > 4096 {
            continue;
        }
        let pointer = String::from_utf8_lossy(&pointer);
        found.push((record, PathBuf::from(pointer.trim())));
    }
    Ok(found)
}

/// The record `<common>/worktrees/<id>` of the linked worktree at
/// `worktree_text`, found by its `gitdir` file pointing there. `None` for a
/// path no record names (the main checkout, a stranger).
fn find_own_worktree_record(repo: &Path, worktree_text: &str) -> Result<Option<PathBuf>, String> {
    let wanted = lane_identity(Path::new(worktree_text));
    Ok(worktree_records(repo)?
        .into_iter()
        .find(|(_, gitdir)| {
            gitdir
                .parent()
                .is_some_and(|dir| lane_identity(dir) == wanted)
        })
        .map(|(record, _)| record))
}

/// Directory under the git common directory that holds worktrees moved aside
/// for deletion, and the markers of the ones moved beside their old place.
pub(crate) const TEARDOWN_DIR: &str = "gitpulse-teardown";
/// Leftovers one sweep deletes at most, so a pile of them cannot hold up the
/// teardown that happened to find them. The rest wait for the next one.
const SWEEP_CAP: usize = 16;
/// How old another process's aside must be before a sweep takes it: that
/// process may still be deleting it.
const FOREIGN_ASIDE_MIN_AGE: Duration = Duration::from_secs(30);

/// Written inside a sibling aside (one beside the old place, which a marker
/// under the git directory names): the git common directory, then the token
/// the marker holds. The sweep deletes a marker's target only when both match,
/// so a marker alone, planted or corrupt, cannot point it at a person's folder.
const SENTINEL_FILE: &str = ".gitpulse-teardown-sentinel";
/// Written inside an aside that holds a worktree's files whose record is gone
/// and that could not be put back: the sweep refuses it and says so.
const HOLD_FILE: &str = ".gitpulse-teardown-hold";
/// A name dated further ahead than this is a bad clock or a bad name, not
/// another process still working: the age rule stops holding it off.
const FUTURE_SLACK: Duration = Duration::from_secs(3600);
/// Longest note kept per sweep problem.
const SWEEP_NOTE_LEN: usize = 400;

/// Asides this process is deleting right now; a sweep leaves them alone.
static ASIDES_IN_FLIGHT: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static ASIDE_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A worktree's files after the rename that took them out of the checkout:
/// the directory to delete, and the marker that names it when it was put
/// beside the old place instead of under [`TEARDOWN_DIR`]. Registered as in
/// flight for as long as it lives.
struct TeardownAside {
    path: PathBuf,
    marker: Option<PathBuf>,
}

impl TeardownAside {
    fn register(path: PathBuf, marker: Option<PathBuf>) -> Self {
        ASIDES_IN_FLIGHT
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(path.clone());
        Self { path, marker }
    }
}

impl Drop for TeardownAside {
    fn drop(&mut self) {
        let mut flight = ASIDES_IN_FLIGHT
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(at) = flight.iter().position(|path| path == &self.path) {
            flight.swap_remove(at);
        }
    }
}

#[cfg(unix)]
fn same_device(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev(),
        _ => false,
    }
}

/// Without a device number to compare, the aside goes beside the old place,
/// which is always the same volume.
#[cfg(not(unix))]
fn same_device(_a: &Path, _b: &Path) -> bool {
    false
}

/// A random token (the standard library's hasher is seeded randomly per
/// process) for the marker and sentinel of a sibling aside.
fn new_aside_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(ASIDE_SERIAL.load(std::sync::atomic::Ordering::Relaxed));
    hasher.write_u32(std::process::id());
    format!("{:016x}", hasher.finish())
}

/// `<pid>-<nanos>-<serial>`, the name of an aside and of its marker.
fn new_aside_name() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let serial = ASIDE_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}-{nanos}-{serial}", std::process::id())
}

/// The pid and the time (nanoseconds since the epoch) in an aside name, or
/// `None` when the name is not one of ours.
fn parse_aside_name(name: &str) -> Option<(u32, u128)> {
    let mut parts = name.split('-');
    let pid = parts.next()?.parse().ok()?;
    let nanos = parts.next()?.parse().ok()?;
    parts.next()?.parse::<u64>().ok()?;
    parts.next().is_none().then_some((pid, nanos))
}

/// Takes the linked worktree at `target_text` out of the checkout by renaming
/// its directory, then removes its record, so git no longer lists it. Caller
/// holds the repository lock; the rename and the record removal are all that
/// the lock is held for.
///
/// `Ok(None)` is "this one is not for the rename path" and nothing was
/// touched: not a directory, no record, a locked record, a submodule (git
/// refuses those on remove and must be the one to say so), a worktree with
/// uncommitted changes when `force` is off (same), an aside name that already
/// exists, or a rename that failed (a mount point, another volume, a handle
/// held open on Windows). The caller then removes it the way it always did.
/// `Err` is only a rename that could not be finished and was put back.
fn try_rename_aside(
    repo: &Path,
    target_text: &str,
    force: bool,
) -> Result<Option<TeardownAside>, String> {
    try_rename_aside_with(
        repo,
        target_text,
        force,
        &mut remove_own_worktree_record,
        &mut |from, to| std::fs::rename(from, to),
    )
}

/// [`try_rename_aside`] with the removal of the record and the rename that
/// puts the directory back passed in, so a test can make the second one fail
/// after the first has happened.
fn try_rename_aside_with(
    repo: &Path,
    target_text: &str,
    force: bool,
    remove_record: &mut dyn FnMut(&Path, &str) -> Result<(), String>,
    rename_back: &mut dyn FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<Option<TeardownAside>, String> {
    let target = Path::new(target_text);
    // A relative path would resolve against the working directory, here and
    // wherever a sweep later reads it back.
    if !target.is_absolute() || !std::fs::symlink_metadata(target).is_ok_and(|meta| meta.is_dir()) {
        return Ok(None);
    }
    let Some(record) = find_own_worktree_record(repo, target_text).ok().flatten() else {
        return Ok(None);
    };
    if record.join("locked").exists()
        || record.join("modules").exists()
        || target.join(".gitmodules").exists()
    {
        return Ok(None);
    }
    if !force && !changed_paths(target_text).is_ok_and(|(changed, _)| changed.is_empty()) {
        return Ok(None);
    }
    let Ok(common) = resolve_git_common_dir(repo) else {
        return Ok(None);
    };
    let holding = common.join(TEARDOWN_DIR);
    if std::fs::create_dir_all(&holding).is_err() {
        return Ok(None);
    }
    let name = new_aside_name();
    let token = new_aside_token();
    let (aside, marker) = match target.file_name() {
        Some(base) if !same_device(target, &holding) => {
            let Some(parent) = target.parent() else {
                return Ok(None);
            };
            let aside = parent.join(format!(
                ".{}.gitpulse-teardown-{name}",
                base.to_string_lossy()
            ));
            let marker = holding.join(format!("{name}.sibling"));
            // The aside's path, then the token the sentinel inside it repeats:
            // the sweep deletes what a marker names only when both agree.
            let text = format!("{}\n{token}", aside.to_string_lossy());
            if std::fs::write(&marker, text.as_bytes()).is_err() {
                return Ok(None);
            }
            (aside, Some(marker))
        }
        _ => (holding.join(&name), None),
    };
    // Unix `rename` replaces an empty directory, so a name already taken has
    // to be seen first.
    if std::fs::symlink_metadata(&aside).is_ok() {
        if let Some(marker) = &marker {
            let _ = std::fs::remove_file(marker);
        }
        return Ok(None);
    }
    let held = TeardownAside::register(aside, marker);
    if std::fs::rename(target, &held.path).is_err() {
        if let Some(marker) = &held.marker {
            let _ = std::fs::remove_file(marker);
        }
        return Ok(None);
    }
    let mut put_back = |why: String| -> String {
        match rename_back(&held.path, target) {
            Ok(()) => {
                if let Some(marker) = &held.marker {
                    let _ = std::fs::remove_file(marker);
                }
                format!("{why}; the worktree was put back")
            }
            Err(e) => {
                // The files are the person's and may no longer have a record:
                // a sweep must not take them. A hold file says so across
                // restarts; failing that, this process keeps the aside
                // registered for as long as it lives.
                if std::fs::write(held.path.join(HOLD_FILE), why.as_bytes()).is_err() {
                    std::mem::forget(TeardownAside::register(held.path.clone(), None));
                }
                format!(
                    "{why}; and putting the worktree back from {} failed: {e}. \
                     Its files are kept there and are not swept",
                    held.path.display()
                )
            }
        }
    };
    if held.marker.is_some() {
        let sentinel = format!("{}\n{token}", common.to_string_lossy());
        if let Err(e) = std::fs::write(held.path.join(SENTINEL_FILE), sentinel) {
            return Err(put_back(format!("could not mark the aside as ours: {e}")));
        }
    }
    if let Err(e) = remove_record(repo, target_text) {
        return Err(put_back(e));
    }
    match still_listed(repo, target_text) {
        Ok(false) => Ok(Some(held)),
        Ok(true) => Err(put_back(format!("git still lists {target_text}"))),
        Err(e) => Err(put_back(e)),
    }
}

/// Removes a worktree with the repository lock held only across a rename: the
/// directory is moved aside and its record removed, and the (slow) deletion of
/// the files is left to [`finish_teardown`] once the caller has let the lock
/// go. When the rename path is not available (see [`try_rename_aside`]),
/// `fallback` removes it in place, under the lock, as removal always did. One
/// owner for both the merge-and-teardown and the Worktrees panel's delete.
///
/// `Ok(Some(aside))` is a worktree that is gone from git's point of view and
/// whose files are still to be deleted; `Ok(None)` one that is gone entirely.
fn remove_worktree_split(
    repo: &Path,
    target_text: &str,
    force: bool,
    fallback: &mut dyn FnMut() -> Result<(), String>,
) -> Result<Option<TeardownAside>, String> {
    match try_rename_aside(repo, target_text, force)? {
        Some(aside) => Ok(Some(aside)),
        None => fallback().map(|()| None),
    }
}

/// The production deleter for [`finish_teardown`]. A named function, not
/// `std::fs::remove_dir_all` itself, which does not satisfy the seam's
/// higher-ranked `&Path` signature.
fn delete_dir_tree(path: &Path) -> std::io::Result<()> {
    std::fs::remove_dir_all(path)
}

/// Deletes the files [`remove_worktree_split`] moved aside, with the lock
/// released. A failure leaves the worktree removed (git no longer knows it)
/// and says where the files are; the next teardown's sweep retries them.
fn finish_teardown(
    aside: Option<TeardownAside>,
    delete_aside: &mut dyn FnMut(&Path) -> std::io::Result<()>,
) -> Option<String> {
    let aside = aside?;
    match delete_aside(&aside.path) {
        Ok(()) => {
            if let Some(marker) = &aside.marker {
                let _ = std::fs::remove_file(marker);
            }
            None
        }
        Err(e) => Some(format!(
            "The worktree is removed, but its files at {} could not be deleted: {e}. \
             The next teardown retries them.",
            aside.path.display()
        )),
    }
}

/// What one [`sweep_teardown_asides`] did.
#[derive(Debug, Default, PartialEq, Eq)]
struct SweepReport {
    removed: usize,
    /// A worktree record still points into it, so it is not leftover.
    kept: usize,
    /// Left alone, each with why: a symlink, one that could not be deleted,
    /// a marker that could not be read.
    problems: Vec<String>,
    /// Leftovers not looked at because the cap was reached.
    beyond_cap: usize,
}

impl SweepReport {
    /// What a person should hear about: each problem and the cap being hit,
    /// each bounded to [`SWEEP_NOTE_LEN`] characters. At most `SWEEP_CAP + 1`
    /// entries, since the sweep stops looking at the cap. Empty for a sweep
    /// with nothing to say.
    fn notes(&self) -> Vec<String> {
        let mut parts = self.problems.clone();
        if self.beyond_cap > 0 {
            parts.push(format!(
                "{} more leftover teardown director{} wait for the next teardown (the sweep deletes at most {SWEEP_CAP} at a time)",
                self.beyond_cap,
                if self.beyond_cap == 1 { "y" } else { "ies" }
            ));
        }
        parts
            .into_iter()
            .map(|note| match note.char_indices().nth(SWEEP_NOTE_LEN) {
                Some((at, _)) => format!("{}...", &note[..at]),
                None => note,
            })
            .collect()
    }

    /// [`SweepReport::notes`] as one line.
    #[cfg(test)]
    fn note(&self) -> Option<String> {
        let notes = self.notes();
        (!notes.is_empty()).then(|| notes.join("; "))
    }
}

/// Deletes what an earlier teardown moved aside and did not finish deleting
/// (a crash, or a delete that failed). This is the retry. It takes only
/// entries named like ours in `<common>/gitpulse-teardown` (and the
/// directories their `.sibling` markers name), at most [`SWEEP_CAP`] per call,
/// never one that a worktree record still points into or that this process is
/// deleting now or that another process made less than
/// [`FOREIGN_ASIDE_MIN_AGE`] ago, and never a symlink.
///
/// It runs at the start of every teardown, so a repository with nothing to
/// sweep pays one `read_dir` of a directory that usually does not exist. It
/// fails closed: when the worktree records cannot be read, so that "a record
/// points into it" cannot be checked, it deletes nothing and says so.
fn sweep_teardown_asides(repo: &Path) -> SweepReport {
    sweep_teardown_asides_from(repo, &|holding| {
        Ok(std::fs::read_dir(holding)?
            .map(|entry| {
                entry.map(|entry| {
                    (
                        entry.file_name().to_string_lossy().into_owned(),
                        entry.path(),
                    )
                })
            })
            .collect())
    })
}

/// A listing of the holding directory: `(name, path)` per entry, or the error
/// reading that entry, or the error listing it at all.
type EntryListing = std::io::Result<Vec<std::io::Result<(String, PathBuf)>>>;

/// [`sweep_teardown_asides`] with the listing of the holding directory passed
/// in, so a test can hand it an entry that failed to read.
fn sweep_teardown_asides_from(
    repo: &Path,
    list_entries: &dyn Fn(&Path) -> EntryListing,
) -> SweepReport {
    let mut report = SweepReport::default();
    let holding = match resolve_git_common_dir(repo) {
        Ok(common) => common.join(TEARDOWN_DIR),
        Err(e) => {
            report.problems.push(format!(
                "leftover teardown directories were not checked: {e}"
            ));
            return report;
        }
    };
    let listing = match list_entries(&holding) {
        Ok(listing) => listing,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return report,
        Err(e) => {
            report.problems.push(format!(
                "leftover teardown directories in {} were not checked: {e}",
                holding.display()
            ));
            return report;
        }
    };
    // (entry, the directory to delete, the marker to delete with it)
    let mut leftovers: Vec<(PathBuf, Option<PathBuf>, u32, u128)> = Vec::new();
    for entry in listing {
        let (name, path) = match entry {
            Ok(entry) => entry,
            Err(e) => {
                report.problems.push(format!(
                    "an entry of {} could not be read, so it was not looked at: {e}",
                    holding.display()
                ));
                continue;
            }
        };
        let (stem, is_marker) = match name.strip_suffix(".sibling") {
            Some(stem) => (stem, true),
            None => (name.as_str(), false),
        };
        let Some((pid, nanos)) = parse_aside_name(stem) else {
            continue;
        };
        if is_marker {
            leftovers.push((path.clone(), Some(path), pid, nanos));
        } else {
            leftovers.push((path, None, pid, nanos));
        }
    }
    leftovers.sort();
    // No `worktrees` directory is no linked worktrees. Any other failure to
    // read the records leaves "a record points into it" unchecked, and
    // nothing is deleted on a pass that cannot check it.
    let records = match worktree_records(repo) {
        Ok(records) => records,
        Err(e) => {
            let no_records = holding.parent().is_some_and(|common| {
                matches!(
                    std::fs::symlink_metadata(common.join("worktrees")),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound
                )
            });
            if !no_records {
                report.problems.push(format!(
                    "the worktree records could not be read, so no leftover was deleted this time: {e}"
                ));
                return report;
            }
            Vec::new()
        }
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let in_flight = ASIDES_IN_FLIGHT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    for (entry, marker, pid, nanos) in leftovers {
        if report.removed + report.kept + report.problems.len() >= SWEEP_CAP {
            report.beyond_cap += 1;
            continue;
        }
        // The directory to delete: the entry itself, or what its marker names
        // (the marker's token is checked against the sentinel further down).
        let mut marker_token = None;
        let target = match &marker {
            None => entry.clone(),
            Some(marker) => {
                let text = std::fs::read(marker).ok().filter(|text| text.len() <= 4096);
                let text = text.map(|text| String::from_utf8_lossy(&text).into_owned());
                let Some((named, token)) = text.as_deref().and_then(|text| text.rsplit_once('\n'))
                else {
                    report.problems.push(format!(
                        "could not read the marker {}, so what it names was left",
                        marker.display()
                    ));
                    continue;
                };
                marker_token = Some(token.to_string());
                let named = PathBuf::from(named);
                // Never resolved against the working directory.
                if !named.is_absolute() {
                    report.problems.push(format!(
                        "the marker {} names a relative path, so it was left",
                        marker.display()
                    ));
                    continue;
                }
                let stem = marker
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.strip_suffix(".sibling"))
                    .unwrap_or_default();
                let ours = named
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with('.')
                            && name.ends_with(&format!(".gitpulse-teardown-{stem}"))
                    });
                if !ours {
                    report.problems.push(format!(
                        "the marker {} names {}, which is not a teardown directory, so it was left",
                        marker.display(),
                        named.display()
                    ));
                    continue;
                }
                named
            }
        };
        if in_flight.contains(&target) {
            continue;
        }
        // Another process's fresh aside may still be in use by it. One dated
        // far ahead is not fresh, it is wrong.
        let dated_ahead = nanos > now.saturating_add(FUTURE_SLACK.as_nanos());
        if pid != std::process::id()
            && !dated_ahead
            && now.saturating_sub(nanos) < FOREIGN_ASIDE_MIN_AGE.as_nanos()
        {
            continue;
        }
        match std::fs::symlink_metadata(&target) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if let Some(marker) = &marker {
                    let _ = std::fs::remove_file(marker);
                }
                continue;
            }
            Err(e) => {
                report
                    .problems
                    .push(format!("could not look at {}: {e}", target.display()));
                continue;
            }
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                report.problems.push(format!(
                    "{} is not a plain directory, so it was left",
                    target.display()
                ));
                continue;
            }
            Ok(_) => {}
        }
        if target.join(HOLD_FILE).exists() {
            report.problems.push(format!(
                "{} holds a worktree's files that could not be put back, so it was left",
                target.display()
            ));
            continue;
        }
        if let Some(token) = &marker_token {
            let sentinel = std::fs::read(target.join(SENTINEL_FILE))
                .ok()
                .filter(|text| text.len() <= 4096)
                .map(|text| String::from_utf8_lossy(&text).into_owned());
            let common = holding
                .parent()
                .map(|dir| dir.to_string_lossy().into_owned());
            let ours = sentinel
                .as_deref()
                .and_then(|text| text.rsplit_once('\n'))
                .is_some_and(|(written, found)| {
                    found == token && Some(written) == common.as_deref()
                });
            if !ours {
                report.problems.push(format!(
                    "{} is named by a marker but was not marked as this repository's teardown, so it was left",
                    target.display()
                ));
                continue;
            }
        }
        let identity = lane_identity(&target);
        if records.iter().any(|(_, gitdir)| {
            gitdir.starts_with(&target)
                || lane_identity(gitdir.parent().unwrap_or(gitdir)) == identity
        }) {
            report.kept += 1;
            continue;
        }
        match std::fs::remove_dir_all(&target) {
            Ok(()) => {
                if let Some(marker) = &marker {
                    let _ = std::fs::remove_file(marker);
                }
                report.removed += 1;
            }
            Err(e) => report.problems.push(format!(
                "could not delete the leftover {}: {e}",
                target.display()
            )),
        }
    }
    report
}

/// Removes the merged worktree, trying up to [`MAX_REMOVE_ATTEMPTS`] times
/// with [`REMOVE_BACKOFF`] between. When a try fails after taking the
/// directory away (a partial delete), git still lists the worktree, and only
/// its own record is removed to finish it. Either way the removal is reported
/// as done only once git no longer lists the path. Caller holds the
/// repository lock.
fn remove_with_retry(
    repo: &Path,
    worktree_text: &str,
    sleep: &mut dyn FnMut(Duration),
) -> Result<(), String> {
    let mut last = String::new();
    for attempt in 0..MAX_REMOVE_ATTEMPTS {
        if attempt > 0 {
            sleep(REMOVE_BACKOFF[attempt - 1]);
        }
        match remove_worktree_locked(repo, worktree_text, true) {
            Ok(()) => {
                return if still_listed(repo, worktree_text)? {
                    Err(format!("git removed {worktree_text} but still lists it"))
                } else {
                    Ok(())
                };
            }
            Err(e) => last = e,
        }
        if !Path::new(worktree_text).exists() {
            remove_own_worktree_record(repo, worktree_text)
                .map_err(|e| format!("{last}; finishing the removal failed: {e}"))?;
            return if still_listed(repo, worktree_text)? {
                Err(format!("{last}; git still lists {worktree_text}"))
            } else {
                Ok(())
            };
        }
    }
    Err(format!(
        "The worktree could not be removed after {MAX_REMOVE_ATTEMPTS} attempts: {last}"
    ))
}

/// Git's arguments for the merge step of [`merge_and_teardown_worktree`].
/// A squash is the one [`GitWriter::merge_argv`] runs for every squash merge.
pub fn merge_teardown_argv(branch: &str, squash: bool) -> Vec<String> {
    if squash {
        GitWriter::merge_argv(branch, MergeMode::Squash)
            .into_iter()
            .map(String::from)
            .collect()
    } else {
        vec!["merge".into(), "--ff-only".into(), branch.into()]
    }
}

/// What a worktree removal left to say. The worktree itself is removed
/// whenever the call is `Ok`; this is only about the files after it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeRemoval {
    /// The files moved aside for deletion could not all be deleted, or a
    /// leftover from an earlier removal could not be cleared. Names the path;
    /// the next removal retries it.
    #[serde(default)]
    pub cleanup_error: Option<String>,
    /// What the sweep before this removal could not clear; see
    /// [`MergeTeardownResult::sweep_problems`].
    #[serde(default)]
    pub sweep_problems: Vec<String>,
}

/// Removes a linked worktree. Git refuses the main worktree itself.
///
/// The repository lock is held across a rename, not the deletion: see
/// [`remove_worktree_split`].
pub fn remove_worktree(
    repo_path: &str,
    target_path: &str,
    force: bool,
) -> Result<WorktreeRemoval, String> {
    remove_worktree_with(repo_path, target_path, force, &mut delete_dir_tree)
}

/// [`remove_worktree`] with the deletion of the moved-aside files passed in:
/// it is called once the lock is released, with the directory to delete (and
/// not at all when the removal was done in place), so a caller can tell the
/// lock-held time from the deletion's.
pub fn remove_worktree_with(
    repo_path: &str,
    target_path: &str,
    force: bool,
    delete_aside: &mut dyn FnMut(&Path) -> std::io::Result<()>,
) -> Result<WorktreeRemoval, String> {
    let repo = validate_repo(repo_path)?;
    let sweep_problems = sweep_teardown_asides(&repo).notes();
    let aside = {
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_target_path(target_path)?;
        // The directory is gone but its record is not: a teardown that was
        // cut short after the rename. Finishing it is removing that record;
        // `validate_repo` below would refuse a path that is not there.
        if finish_vanished_worktree(&repo, target_path)? {
            return Ok(WorktreeRemoval {
                cleanup_error: None,
                sweep_problems,
            });
        }
        let target = validate_repo(target_path)?;
        let target_text = plain_path_text(target.to_str().ok_or("Worktree path is not UTF-8")?);
        remove_worktree_split(&repo, &target_text, force, &mut || {
            remove_worktree_locked(&repo, target_path, force)
        })?
    };
    Ok(WorktreeRemoval {
        cleanup_error: finish_teardown(aside, delete_aside),
        sweep_problems,
    })
}

/// Whether `target_path` is a worktree whose directory is already gone, and
/// if so removes its record (and nothing else), under the lock the caller
/// holds. `false` for a path that exists, or that no record names, which the
/// removal then handles as it always did.
fn finish_vanished_worktree(repo: &Path, target_path: &str) -> Result<bool, String> {
    let text = plain_path_text(target_path);
    let missing = matches!(
        std::fs::symlink_metadata(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    );
    if !missing || find_own_worktree_record(repo, &text)?.is_none() {
        return Ok(false);
    }
    remove_own_worktree_record(repo, &text)?;
    if still_listed(repo, &text)? {
        return Err(format!(
            "git still lists {text} after its record was removed"
        ));
    }
    Ok(true)
}

/// [`remove_worktree`] for a caller that already holds `repo`'s mutation lock
/// (the merge-and-teardown span, which checks and removes under one lock).
fn remove_worktree_locked(repo: &Path, target_path: &str, force: bool) -> Result<(), String> {
    validate_target_path(target_path)?;
    // Git scans the target's status internally. Its config.worktree helpers
    // can execute even though the parent is the process's current directory.
    let target = validate_repo(target_path)?;
    let target_path = target.to_str().ok_or("Worktree path is not UTF-8")?;
    // `validate_repo` canonicalizes, which on Windows is the `\\?\` form git
    // does not take as an argument.
    let target_path = plain_path_text(target_path);
    let target_path = target_path.as_str();
    let mut args: Vec<&str> = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(target_path);
    git_text(repo, &args)?;
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
    argv.push(plain_path_text(target_path));
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
    argv.push(plain_path_text(target_path));
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

    /// A full family memory used to empty itself: the first family past the
    /// cap forgot every other one, so a session with more repositories than the
    /// cap asked Git again for all of them, and a parallel test's insert wiped
    /// a sibling test's warm entry between its cold and warm read. Only the
    /// least recently used family may go, and a hit is a use.
    #[test]
    fn a_full_family_memory_forgets_only_what_has_gone_unused() {
        fn family(root: &Path) -> (Vec<PathBuf>, WorktreeFamily) {
            let anchor = root.join("anchor");
            let members = vec![anchor.clone(), root.join("linked")];
            let family = WorktreeFamily {
                anchor,
                worktree: members[1].clone(),
                members: members.clone(),
            };
            (members, family)
        }
        let root = Path::new("/gp-family-lru-test");
        let (hot_members, hot_family) = family(&root.join("hot"));
        let hot_common = root.join("hot").join(".git");
        remember_family(hot_common.clone(), hot_members.clone(), &hot_family);

        for i in 0..MAX_REMEMBERED_FAMILIES * 3 {
            let (members, fam) = family(&root.join(format!("filler-{i}")));
            remember_family(root.join(format!("filler-{i}")).join(".git"), members, &fam);
            if i % 16 == 0 {
                assert_eq!(
                    remembered_family(&hot_common, &hot_members, &hot_members[0], &hot_members[1]),
                    Some(hot_family.clone()),
                    "the family in use was forgotten after {i} others arrived"
                );
            }
        }
        let (old_members, _) = family(&root.join("filler-0"));
        assert_eq!(
            remembered_family(
                &root.join("filler-0").join(".git"),
                &old_members,
                &old_members[0],
                &old_members[1]
            ),
            None,
            "the cap no longer bounds the memory"
        );
    }

    /// `path.canonicalize()` in the spelling git reports its worktrees in. The
    /// spawn log is keyed by the directory a child ran in, and git lists a
    /// worktree as `C:/...`, never `\\?\C:\...`, so a verbatim key matches
    /// nothing on Windows and a "spawned zero" assertion passes or fails for
    /// the wrong reason. Plain `canonicalize` off Windows.
    fn plain(path: impl AsRef<Path>) -> PathBuf {
        crate::engine::git_cli::canonicalize_plain(path.as_ref()).unwrap()
    }

    /// The fixtures below are written in `--porcelain`'s newline layout for
    /// readability; the parser reads the `-z` layout, whose NUL is that
    /// layout's newline and whose empty field is its blank line.
    fn parse_worktree_porcelain(raw: &str) -> Vec<ParsedWorktree> {
        parse_worktree_porcelain_z(&raw.replace('\n', "\0"))
    }

    /// A worktree path may hold a newline or end in a space. Without `-z` the
    /// first split into two fields and the second was trimmed into a
    /// different directory, so the listing named worktrees that do not exist.
    #[cfg(unix)]
    #[test]
    fn worktree_paths_with_a_newline_or_trailing_space_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        std::fs::create_dir(&main).unwrap();
        crate::test_support::git_in(&main, &["init", "-q", "-b", "main"]);
        crate::test_support::git_in(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let main = main.canonicalize().unwrap();
        let odd = [dir.path().join("trail "), dir.path().join("new\nline")];
        for (n, path) in odd.iter().enumerate() {
            crate::test_support::git_in(
                &main,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    &format!("b{n}"),
                    path.to_str().unwrap(),
                ],
            );
        }

        let entries = read_worktree_entries(&main).expect("entries");
        let paths: Vec<String> = entries.iter().map(|entry| entry.path.clone()).collect();
        assert_eq!(entries.len(), 3, "{paths:?}");
        for path in &odd {
            let canonical = path.canonicalize().unwrap();
            assert!(
                paths.iter().any(|listed| Path::new(listed) == canonical),
                "{canonical:?} did not round-trip through {paths:?}"
            );
        }
        let listed = list_worktrees(main.to_str().unwrap()).expect("listing");
        assert_eq!(listed.len(), 3);
        assert!(
            crate::engine::git_writer::checked_out_branches(&main)
                .unwrap()
                .contains("b1"),
            "the branch of the newline-path worktree was lost"
        );
    }

    /// The command gate and the writer take the same spelling, and it is one
    /// git accepts: a canonicalized checkout is `\\?\C:\...` on Windows, which
    /// `git worktree add` and `remove` refuse as an argument.
    #[cfg(windows)]
    #[test]
    fn worktree_argv_never_carries_a_verbatim_path() {
        let verbatim = r"\\?\C:\repo\.gitpulse\worktrees\lane";
        let plain = r"C:\repo\.gitpulse\worktrees\lane";
        assert_eq!(
            add_worktree_argv(verbatim, Some("b"), None, false),
            ["git", "worktree", "add", "-b", "b", plain]
        );
        assert_eq!(
            remove_worktree_argv(verbatim, true),
            ["git", "worktree", "remove", "--force", plain]
        );
    }

    /// A target that is already in canonical form still creates and removes a
    /// worktree: the case every managed attempt is in, since its checkout came
    /// from `validate_repo`.
    #[test]
    fn a_canonicalized_target_creates_and_removes_a_worktree() {
        let main = tempfile::TempDir::new().unwrap();
        git_in(main.path(), &["init", "-b", "main"]);
        git_in(
            main.path(),
            &["commit", "--allow-empty", "-q", "-m", "init"],
        );
        crate::test_support::trust_repo(main.path());
        let holder = tempfile::TempDir::new().unwrap();
        let target = holder.path().canonicalize().unwrap().join("lane");
        let target_text = target.to_str().unwrap();
        let repo = main.path().to_str().unwrap();
        add_worktree(repo, target_text, Some("lane"), None, false).expect("add");
        assert!(target.join(".git").exists(), "{target:?} was not created");
        remove_worktree(repo, target_text, false).expect("remove");
        assert!(!target.exists(), "{target:?} was not removed");
    }

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

    use crate::test_support::repo_with_worktrees;

    /// `diff` children a listing started in `dir`.
    fn diff_spawns_in(dir: &Path) -> usize {
        crate::engine::git_cli::spawn_log::spawns_in(dir)
            .iter()
            .filter(|argv| argv.iter().any(|arg| arg == "diff"))
            .count()
    }

    /// A full scan of a worktree whose `git status` came back empty does not
    /// start a second child to be told the diff against HEAD is empty, and a
    /// worktree with changes is still measured.
    #[test]
    fn a_full_scan_diffs_only_the_worktrees_status_found_changed() {
        let (main, _parent, linked) = repo_with_worktrees(2);
        std::fs::write(linked[1].join("seed.txt"), "seed\nand\nmore\n").unwrap();
        let listed = list_worktrees(main.path().to_str().unwrap()).expect("list");
        let of = |dir: &Path| {
            listed
                .iter()
                .find(|w| Path::new(&w.path) == dir)
                .expect("listed")
                .clone()
        };
        let clean = of(&linked[0]);
        assert_eq!(clean.dirty_files, Some(0));
        assert_eq!(clean.diff_stat, Some(WorktreeDiffStat::default()));
        assert_eq!(clean.scan_note, None);
        assert_eq!(diff_spawns_in(&linked[0]), 0, "a clean worktree was diffed");

        let changed = of(&linked[1]);
        assert_eq!(changed.dirty_files, Some(1));
        assert_eq!(
            diff_spawns_in(&linked[1]),
            1,
            "a changed worktree is diffed once"
        );
        let stat = changed.diff_stat.expect("measured");
        assert_eq!((stat.files_changed, stat.insertions), (1, 3), "{stat:?}");
    }

    /// The skipped diff is only ever the empty one: whenever status reports
    /// nothing, the listing's answer equals an unconditional `git diff HEAD`,
    /// across the states that change one without the other's knowledge.
    #[test]
    fn a_clean_status_and_a_measured_diff_always_agree() {
        let (main, _parent, linked) = repo_with_worktrees(1);
        let dir = linked[0].clone();
        type Step = (&'static str, Box<dyn Fn()>);
        let steps: Vec<Step> = vec![
            ("untouched", Box::new(|| {})),
            ("mtime moved, content not", {
                let dir = dir.clone();
                Box::new(move || {
                    let file = std::fs::File::options()
                        .write(true)
                        .open(dir.join("seed.txt"))
                        .unwrap();
                    file.set_modified(std::time::SystemTime::now() - Duration::from_secs(120))
                        .unwrap();
                })
            }),
            ("only an untracked file", {
                let dir = dir.clone();
                Box::new(move || std::fs::write(dir.join("loose.txt"), "x").unwrap())
            }),
            ("a staged change", {
                let dir = dir.clone();
                Box::new(move || {
                    std::fs::remove_file(dir.join("loose.txt")).unwrap();
                    std::fs::write(dir.join("seed.txt"), "staged").unwrap();
                    git_in(&dir, &["add", "seed.txt"]);
                })
            }),
            ("that change committed", {
                let dir = dir.clone();
                Box::new(move || git_in(&dir, &["commit", "-q", "-m", "next"]))
            }),
            ("reset back past it", {
                let dir = dir.clone();
                Box::new(move || git_in(&dir, &["reset", "-q", "--hard", "HEAD~1"]))
            }),
        ];
        let mut clean_states = 0;
        for (label, step) in steps {
            step();
            let listed = list_worktrees(main.path().to_str().unwrap()).expect("list");
            let wt = listed
                .iter()
                .find(|w| Path::new(&w.path) == dir)
                .expect("listed");
            assert_eq!(wt.scan_note, None, "{label}");
            if wt.dirty_files == Some(0) {
                clean_states += 1;
            }
            assert_eq!(wt.diff_stat, measure_diff_stat(&dir), "{label}");
        }
        assert!(clean_states >= 3, "the fixture never reached a clean state");
    }

    /// An unborn HEAD has nothing to diff against, so a clean status there
    /// is not taken as an empty diff; it is measured, as before.
    #[test]
    fn an_unborn_head_is_never_assumed_clean() {
        let (main, _parent, _linked) = repo_with_worktrees(0);
        let repo = plain(main.path());
        let entry = ParsedWorktree {
            path: repo.to_str().unwrap().to_string(),
            head: "0".repeat(40),
            branch: Some("main".into()),
            ..ParsedWorktree::default()
        };
        let before = diff_spawns_in(&repo);
        let scan = scan_worktree(&repo, &entry, ScanDepth::Full, &Base::none(), None);
        assert_eq!(scan.dirty, Some(0));
        assert_eq!(diff_spawns_in(&repo), before + 1);
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
        let repo = plain(main.path());
        let scan = crate::engine::git_cli::with_forced_spawn_failure_of("diff", || {
            scan_worktree(&repo, &entry, ScanDepth::Full, &Base::none(), None)
        });
        assert_eq!(scan.dirty, Some(0));
        assert_eq!(scan.diff_stat, None);
        let note = scan.note.unwrap_or_default();
        assert!(
            note.starts_with("some measurements were not taken: "),
            "{note}"
        );
        assert!(note.contains("forced by test"), "{note}");
        let whole = scan_worktree(&repo, &entry, ScanDepth::Full, &Base::none(), None);
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
        // Changed, so each scan starts both its children (status and diff):
        // a clean worktree's diff is not asked for.
        for dir in &linked {
            std::fs::write(dir.join("seed.txt"), "changed").unwrap();
        }
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
            elapsed < crate::test_support::coverage_relaxed(std::time::Duration::from_secs(5)),
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
        assert!(status_paths_up_to(b"", usize::MAX).is_empty());
        assert_eq!(
            status_paths_up_to(b" M src/lib.rs\0", usize::MAX),
            vec!["src/lib.rs"]
        );
        assert_eq!(
            status_paths_up_to(b"R  new.rs\0old.rs\0", usize::MAX),
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
        teardown_fixture_on(hooks, "main")
    }

    /// [`teardown_fixture`] on a repository whose only branch is `default`.
    fn teardown_fixture_on(
        hooks: Option<&str>,
        default: &str,
    ) -> (tempfile::TempDir, tempfile::TempDir) {
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
        git(repo, &["init", "-b", default, "."]);
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
        if let Some(hooks) = hooks {
            // These tests exercise what approved hooks do; consent itself is
            // tested in worktree_hooks.
            crate::engine::approve_worktree_hooks(repo, hooks).expect("approve fixture hooks");
        }
        let wt = tempfile::tempdir().expect("wt tempdir");
        let created = add_worktree_extended(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("feature-x"),
            None,
            false,
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
            false,
        )
        .unwrap();
        assert_eq!((created.cache_error, created.hook_error), (None, None));
    }

    /// Agent provisioning and the worktree dialog both ask for hooks; a
    /// repository's file that nobody approved is reported, never run.
    #[cfg(unix)]
    #[test]
    fn unapproved_post_create_hooks_do_not_run_from_provisioning() {
        let (dir, _) = teardown_fixture(None);
        let hooks = r#"{"worktree":{"post_create":["touch ran.marker"]}}"#;
        std::fs::create_dir_all(dir.path().join(".gitpulse")).unwrap();
        std::fs::write(dir.path().join(".gitpulse/hooks.json"), hooks).unwrap();
        let wt = tempfile::tempdir().unwrap();
        let created = add_worktree_extended(
            dir.path().to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("feature-q"),
            None,
            false,
            true,
            true,
        )
        .expect("the worktree itself was created");
        assert_eq!(created.hook_error, None);
        let notice = created.hook_notice.expect("the skip is reported");
        assert!(notice.contains("not been approved"), "{notice}");
        assert!(!wt.path().join("ran.marker").exists());

        // The same file, approved, runs; edited afterwards, it does not.
        crate::engine::approve_worktree_hooks(dir.path(), hooks).unwrap();
        let ok = tempfile::tempdir().unwrap();
        let created = add_worktree_extended(
            dir.path().to_str().unwrap(),
            ok.path().to_str().unwrap(),
            Some("feature-r"),
            None,
            false,
            false,
            true,
        )
        .unwrap();
        assert_eq!((created.hook_error, created.hook_notice), (None, None));
        assert!(ok.path().join("ran.marker").exists());

        std::fs::write(
            dir.path().join(".gitpulse/hooks.json"),
            r#"{"worktree":{"post_create":["touch ran.marker","touch extra.marker"]}}"#,
        )
        .unwrap();
        let changed = tempfile::tempdir().unwrap();
        let created = add_worktree_extended(
            dir.path().to_str().unwrap(),
            changed.path().to_str().unwrap(),
            Some("feature-s"),
            None,
            false,
            false,
            true,
        )
        .unwrap();
        assert!(created.hook_notice.is_some());
        assert!(!changed.path().join("extra.marker").exists());
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
            None,
            &mut |args: &[&str]| {
                asked.push(args.join(" "));
                Ok(())
            },
        )
        .unwrap_err()
        .to_string();
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
            None,
            &mut allow_all,
        )
        .unwrap_err()
        .to_string();
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
            None,
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
        // The merge had landed when the removal was refused, so this is a
        // merge with a cleanup left to do, not a merge that did not happen.
        let kept = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut |args: &[&str]| {
                if args.first() == Some(&"worktree") {
                    Err("policy: worktree removal denied".into())
                } else {
                    Ok(())
                }
            },
        )
        .expect("a landed merge is not reported as a failure");
        assert!(kept.merged && !kept.worktree_removed && !kept.branch_deleted);
        let reason = kept.removal_error.expect("the refusal is the reason");
        assert!(reason.contains("worktree removal denied"), "{reason}");
        assert!(kept
            .branch_error
            .expect("a kept branch says why")
            .contains("still there"));
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
            None,
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
        let created = add_worktree_extended(
            repo_path,
            wt_path,
            Some("feature-x"),
            None,
            false,
            false,
            false,
        )
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
        let res = merge_and_teardown_worktree(
            repo_path,
            wt_path,
            Some("main"),
            false,
            None,
            &mut allow_all,
        )
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

    /// Runs a teardown whose gate performs `outside_write` on the worktree
    /// when asked about the removal: an outside agent writing after the merge
    /// was checked and before the worktree is destroyed.
    #[cfg(unix)]
    fn teardown_with_outside_write(
        outside_write: impl Fn(&Path),
    ) -> (
        Result<MergeTeardownResult, MergeTeardownError>,
        tempfile::TempDir,
        tempfile::TempDir,
    ) {
        let (dir, wt) = teardown_fixture(None);
        let wt_path = wt.path().to_path_buf();
        let mut gate = |argv: &[&str]| -> Result<(), String> {
            if argv.first() == Some(&"worktree") && argv.get(1) == Some(&"remove") {
                outside_write(&wt_path);
            }
            Ok(())
        };
        let result = merge_and_teardown_worktree(
            dir.path().to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut gate,
        );
        (result, dir, wt)
    }

    #[cfg(unix)]
    #[test]
    fn a_file_written_after_the_check_keeps_the_worktree() {
        let (result, dir, wt) = teardown_with_outside_write(|wt| {
            std::fs::write(wt.join("outside.txt"), "an agent's work\n").unwrap();
        });
        // The merge had landed: the late write keeps the worktree, and says
        // so on a merged result instead of failing the merge.
        let result = result.expect("a landed merge is not reported as a failure");
        assert!(result.merged && !result.worktree_removed);
        let reason = result
            .removal_error
            .expect("the removal says why it stopped");
        assert!(reason.contains("kept"), "{reason}");
        assert!(result.branch_error.is_some());
        assert!(
            wt.path().join("outside.txt").exists(),
            "the file was destroyed"
        );
        assert!(
            branch_exists(dir.path(), "feature-x"),
            "the branch was deleted"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_commit_landing_after_the_check_keeps_the_worktree_and_branch() {
        let (result, dir, wt) = teardown_with_outside_write(|wt| {
            crate::test_support::git_in(wt, &["commit", "-q", "--allow-empty", "-m", "late"]);
        });
        let result = result.expect("a landed merge is not reported as a failure");
        assert!(result.merged && !result.worktree_removed);
        let reason = result
            .removal_error
            .expect("the removal says why it stopped");
        assert!(reason.contains("moved"), "{reason}");
        assert!(wt.path().exists(), "the worktree was removed");
        assert!(
            branch_exists(dir.path(), "feature-x"),
            "the unmerged commit's branch was deleted"
        );
    }

    /// `teardown_fixture` plus a README edit on both sides, so the merge of
    /// `feature-x` into `main` conflicts.
    fn conflicting_fixture() -> (tempfile::TempDir, tempfile::TempDir) {
        let (dir, wt) = teardown_fixture(None);
        std::fs::write(wt.path().join("README.md"), "lane edit\n").unwrap();
        crate::test_support::git_in(wt.path(), &["commit", "-qam", "lane edits README"]);
        std::fs::write(dir.path().join("README.md"), "main edit\n").unwrap();
        crate::test_support::git_in(dir.path(), &["commit", "-qam", "main edits README"]);
        (dir, wt)
    }

    fn main_checkout_is_idle(repo: &Path) {
        let porcelain = git_text(repo, &["status", "--porcelain"]).unwrap();
        assert!(
            porcelain.trim().is_empty(),
            "primary checkout dirty: {porcelain}"
        );
        let git_dir = crate::engine::git_cli::resolve_git_dir(repo).unwrap();
        for control in ["MERGE_HEAD", "SQUASH_MSG", "MERGE_MSG"] {
            assert!(
                !git_dir.join(control).exists(),
                "{control} was left in the primary checkout"
            );
        }
    }

    #[test]
    fn a_conflicting_plain_merge_leaves_the_primary_checkout_as_it_was() {
        let (dir, wt) = conflicting_fixture();
        let repo = dir.path();
        let before = git_text(repo, &["rev-parse", "HEAD"]).unwrap();
        let refused = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut allow_all,
        )
        .unwrap_err();
        let MergeTeardownError::Conflict {
            message,
            paths,
            total,
            rolled_back,
            rollback_error,
        } = refused
        else {
            panic!("a conflict was reported as {refused:?}");
        };
        assert_eq!(paths, ["README.md"], "{message}");
        assert_eq!((total, rolled_back, rollback_error), (1, true, None));
        main_checkout_is_idle(repo);
        assert_eq!(git_text(repo, &["rev-parse", "HEAD"]).unwrap(), before);
        assert!(wt.path().join("feature.txt").exists() && branch_exists(repo, "feature-x"));
    }

    #[test]
    fn a_conflicting_squash_merge_leaves_the_primary_checkout_as_it_was() {
        let (dir, wt) = conflicting_fixture();
        let repo = dir.path();
        let refused = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            true,
            None,
            &mut allow_all,
        )
        .unwrap_err();
        let MergeTeardownError::Conflict {
            paths, rolled_back, ..
        } = refused
        else {
            panic!("a squash conflict was reported as {refused:?}");
        };
        assert_eq!(paths, ["README.md"]);
        assert!(rolled_back);
        main_checkout_is_idle(repo);
        assert_eq!(crate::engine::repo_op::detect_kind(repo).unwrap(), None);
        assert!(wt.path().join("feature.txt").exists() && branch_exists(repo, "feature-x"));
    }

    /// Runs a teardown into `main` with the gate allowing everything.
    fn teardown(
        dir: &tempfile::TempDir,
        wt: &tempfile::TempDir,
        squash: bool,
        expected_tip: Option<&str>,
    ) -> Result<MergeTeardownResult, MergeTeardownError> {
        merge_and_teardown_worktree(
            dir.path().to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            squash,
            expected_tip,
            &mut allow_all,
        )
    }

    fn rev(repo: &Path, name: &str) -> String {
        git_text(repo, &["rev-parse", name])
            .unwrap()
            .trim()
            .to_string()
    }

    /// `main` gains a file the lane does not have, so `--ff-only` fails and
    /// the merge needs a merge commit, without conflicting.
    fn diverge_main(dir: &tempfile::TempDir) {
        std::fs::write(dir.path().join("main-only.txt"), "main work\n").unwrap();
        crate::test_support::git_in(dir.path(), &["add", "main-only.txt"]);
        crate::test_support::git_in(dir.path(), &["commit", "-qm", "main moves on"]);
    }

    #[test]
    fn a_rename_only_conflict_is_listed_and_rolled_back() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        crate::test_support::git_in(wt.path(), &["mv", "README.md", "lane-name.md"]);
        crate::test_support::git_in(wt.path(), &["commit", "-qm", "lane renames README"]);
        crate::test_support::git_in(repo, &["mv", "README.md", "main-name.md"]);
        crate::test_support::git_in(repo, &["commit", "-qm", "main renames README"]);
        for squash in [false, true] {
            let MergeTeardownError::Conflict {
                paths, rolled_back, ..
            } = teardown(&dir, &wt, squash, None).unwrap_err()
            else {
                panic!("a rename conflict was not reported as a conflict (squash: {squash})");
            };
            assert!(
                !paths.is_empty(),
                "no paths for a rename conflict (squash: {squash})"
            );
            assert!(rolled_back);
            main_checkout_is_idle(repo);
        }
    }

    #[test]
    fn a_successful_merge_reports_where_the_target_moved_from_and_to() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        let before = rev(repo, "main");
        let tip = rev(wt.path(), "HEAD");
        let result = teardown(&dir, &wt, false, Some(&tip)).unwrap();
        assert!(result.merged && result.worktree_removed && result.branch_deleted);
        assert_eq!(result.removal_error, None);
        assert_eq!(result.branch_error, None);
        assert_eq!(result.before_oid, before);
        assert_eq!(
            result.after_oid.as_deref(),
            Some(rev(repo, "main").as_str())
        );
        assert_eq!(result.after_oid.as_deref(), Some(tip.as_str()));
    }

    #[test]
    fn a_branch_that_moved_since_it_was_checked_is_not_merged() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        let before = rev(repo, "main");
        let checked = rev(wt.path(), "HEAD");
        crate::test_support::git_in(wt.path(), &["commit", "-q", "--allow-empty", "-m", "late"]);
        let actual = rev(wt.path(), "HEAD");
        let MergeTeardownError::Moved {
            branch,
            expected,
            actual: reported,
            ..
        } = teardown(&dir, &wt, false, Some(&checked)).unwrap_err()
        else {
            panic!("a moved branch was not reported as moved");
        };
        assert_eq!(
            (branch.as_str(), expected, reported),
            ("feature-x", checked, actual)
        );
        assert_eq!(rev(repo, "HEAD"), before, "something was merged");
        main_checkout_is_idle(repo);
        assert!(wt.path().join("feature.txt").exists() && branch_exists(repo, "feature-x"));
    }

    #[test]
    fn an_expected_tip_that_is_not_a_commit_id_is_refused() {
        let (dir, wt) = teardown_fixture(None);
        for bad in ["feature-x", "abc1234", "--upload-pack=x", ""] {
            let refused = teardown(&dir, &wt, false, Some(bad)).unwrap_err();
            assert!(
                matches!(refused, MergeTeardownError::Refused { .. }),
                "{bad:?} was {refused:?}"
            );
        }
        assert!(!dir.path().join("feature.txt").exists());
    }

    #[test]
    fn a_parked_operation_in_the_main_checkout_refuses_the_merge() {
        let (dir, wt) = conflicting_fixture();
        let repo = dir.path();
        // The user's own conflicted merge, parked in the main checkout.
        crate::test_support::git_in(repo, &["switch", "-qc", "other", "HEAD~1"]);
        std::fs::write(repo.join("README.md"), "other edit\n").unwrap();
        crate::test_support::git_in(repo, &["commit", "-qam", "other edits README"]);
        crate::test_support::git_in(repo, &["switch", "-q", "main"]);
        let merge = git_captured_status(repo, &["merge", "other"]);
        assert_ne!(merge, 0, "the fixture merge should conflict");
        let refused = teardown(&dir, &wt, false, None).unwrap_err();
        assert!(
            matches!(&refused, MergeTeardownError::Refused { message } if message.contains("merge")),
            "{refused:?}"
        );
        assert!(
            repo.join(".git/MERGE_HEAD").exists(),
            "the user's merge was rolled back"
        );
    }

    fn git_captured_status(repo: &Path, args: &[&str]) -> i32 {
        crate::engine::git_cli::git_captured(repo, args)
            .unwrap()
            .status_code
    }

    #[cfg(unix)]
    #[test]
    fn a_parked_squash_refuses_the_next_merge_and_keeps_the_lane() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        let hook = repo.join(".git/hooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\necho refusing this commit >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let parked = teardown(&dir, &wt, true, None).unwrap_err();
        assert!(
            matches!(&parked, MergeTeardownError::SquashParked { message } if message.contains("git reset --merge")),
            "{parked:?}"
        );
        assert_eq!(
            crate::engine::repo_op::detect_kind(repo).unwrap(),
            Some(crate::engine::repo_op::OperationKind::SquashMerge)
        );
        assert!(wt.path().join("feature.txt").exists() && branch_exists(repo, "feature-x"));

        let mut asked = Vec::new();
        let again = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            true,
            None,
            &mut |args: &[&str]| {
                asked.push(args.join(" "));
                Ok(())
            },
        )
        .unwrap_err();
        assert!(
            matches!(again, MergeTeardownError::SquashParked { .. }),
            "{again:?}"
        );
        assert!(asked.is_empty(), "a refused merge asked the gate {asked:?}");
    }

    #[test]
    fn the_target_switched_since_provision_refuses_naming_both_branches() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        let base = rev(repo, "main");
        write_base_record(wt.path(), "main", &base).unwrap();
        crate::test_support::git_in(repo, &["switch", "-qc", "elsewhere"]);
        let refused = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("elsewhere"),
            false,
            None,
            &mut allow_all,
        )
        .unwrap_err();
        let MergeTeardownError::BaseMismatch {
            message,
            base_branch,
            target_branch,
            ..
        } = &refused
        else {
            panic!("{refused:?}");
        };
        assert_eq!(
            (base_branch.as_str(), target_branch.as_str()),
            ("main", "elsewhere")
        );
        assert!(
            message.contains("main") && message.contains("elsewhere"),
            "{message}"
        );
        assert!(!repo.join("feature.txt").exists());
        assert!(wt.path().exists() && branch_exists(repo, "feature-x"));
    }

    #[test]
    fn a_target_rewritten_past_the_base_is_an_incompatible_merge_base() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        write_base_record(wt.path(), "main", &rev(repo, "main")).unwrap();
        crate::test_support::git_in(repo, &["commit", "-q", "--amend", "-m", "rewritten"]);
        let refused = teardown(&dir, &wt, false, None).unwrap_err();
        let MergeTeardownError::BaseMismatch { message, .. } = &refused else {
            panic!("{refused:?}");
        };
        assert!(message.contains("incompatible merge-base"), "{message}");
        assert!(!repo.join("feature.txt").exists());
        assert!(wt.path().exists() && branch_exists(repo, "feature-x"));
    }

    #[test]
    fn the_base_record_is_reported_and_an_unreadable_one_refuses() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        assert_eq!(read_base_record(wt.path()).unwrap(), None);
        let base = rev(repo, "main");
        write_base_record(wt.path(), "main", &base).unwrap();
        assert_eq!(
            read_base_record(wt.path()).unwrap(),
            Some(("main".to_string(), base.clone()))
        );
        let record = crate::engine::git_cli::resolve_git_dir(wt.path())
            .unwrap()
            .join(BASE_RECORD);
        std::fs::write(&record, "main\r\n").unwrap();
        let refused = teardown(&dir, &wt, false, None).unwrap_err();
        assert!(
            matches!(&refused, MergeTeardownError::Refused { message } if message.contains(BASE_RECORD)),
            "{refused:?}"
        );
        assert!(!repo.join("feature.txt").exists());

        std::fs::write(&record, format!("main\r\n{base}\r\n")).unwrap();
        let merged = teardown(&dir, &wt, false, None).unwrap();
        assert_eq!(merged.base_oid, Some(base));
    }

    #[test]
    fn a_lane_without_a_base_record_merges_with_no_base_reported() {
        let (dir, wt) = teardown_fixture(None);
        let merged = teardown(&dir, &wt, false, None).unwrap();
        assert_eq!(merged.base_oid, None);
    }

    /// The panel passes no target. On a repository whose default branch is
    /// `develop`, with no `main` or `master` at all, that is the resolved
    /// default, not a refusal for "main".
    #[test]
    fn a_null_target_merges_into_the_resolved_default() {
        let (dir, wt) = teardown_fixture_on(None, "develop");
        let merged = merge_and_teardown_worktree(
            dir.path().to_str().unwrap(),
            wt.path().to_str().unwrap(),
            None,
            false,
            None,
            &mut allow_all,
        )
        .expect("merged into the default branch");
        assert!(merged.merged);
        assert_eq!(merged.target_branch, "develop");
        assert!(dir.path().join("feature.txt").exists());
    }

    /// A destructive merge never guesses its target: a default that cannot be
    /// read, or that does not exist, refuses and leaves the lane whole.
    #[test]
    fn a_null_target_with_no_resolvable_default_is_refused() {
        let kept = |dir: &tempfile::TempDir, wt: &tempfile::TempDir| {
            assert!(wt.path().exists(), "the worktree was removed");
            assert!(
                branch_exists(dir.path(), "feature-x"),
                "the branch was deleted"
            );
            assert!(!dir.path().join("feature.txt").exists(), "something merged");
        };
        let merge = |dir: &tempfile::TempDir, wt: &tempfile::TempDir| {
            merge_and_teardown_worktree(
                dir.path().to_str().unwrap(),
                wt.path().to_str().unwrap(),
                None,
                false,
                None,
                &mut allow_all,
            )
        };

        let (dir, wt) = teardown_fixture_on(None, "develop");
        let refused = crate::engine::git_cli::with_forced_spawn_failure_of(
            "refs/remotes/origin/HEAD",
            || merge(&dir, &wt),
        )
        .expect_err("an unreadable default is not a default");
        let MergeTeardownError::Refused { message } = &refused else {
            panic!("{refused:?}")
        };
        assert!(message.contains("forced by test"), "{message}");
        assert!(message.contains("Nothing was merged"), "{message}");
        kept(&dir, &wt);

        let (dir, wt) = teardown_fixture_on(None, "develop");
        git_text(dir.path(), &["branch", "-m", "develop", "x"]).unwrap();
        let refused = merge(&dir, &wt).expect_err("no default branch resolves");
        let MergeTeardownError::Refused { message } = &refused else {
            panic!("{refused:?}")
        };
        assert!(message.contains("name a target"), "{message}");
        assert!(branch_exists(dir.path(), "x"));
        assert!(wt.path().exists() && branch_exists(dir.path(), "feature-x"));
    }

    /// The listing measures every worktree against the same base the branch
    /// list does. With `develop` as the remote's default and a `main` on a
    /// different commit, the two used to disagree.
    #[test]
    fn worktree_divergence_equals_the_branch_list_against_develop() {
        let (main, _parent, linked) = repo_with_worktrees(1);
        let root = plain(main.path());
        git_in(&root, &["checkout", "-q", "-b", "develop"]);
        git_in(
            &root,
            &["commit", "-q", "--allow-empty", "-m", "develop moves"],
        );
        git_in(&root, &["checkout", "-q", "main"]);
        let develop = rev(&root, "develop");
        git_in(
            &root,
            &["remote", "add", "origin", "https://example.com/r.git"],
        );
        git_in(
            &root,
            &["update-ref", "refs/remotes/origin/develop", &develop],
        );
        git_in(
            &root,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/develop",
            ],
        );
        for n in 0..2 {
            std::fs::write(linked[0].join(format!("w{n}.txt")), "x").unwrap();
            git_in(&linked[0], &["add", "."]);
            git_in(&linked[0], &["commit", "-q", "-m", &format!("w{n}")]);
        }

        let branches = crate::engine::git_reader::GitReader::list_branches(root.to_str().unwrap())
            .expect("branches");
        let lane = branches
            .iter()
            .find(|b| b.name == "agent/00")
            .expect("lane listed");
        assert_eq!(lane.compared_to.as_deref(), Some("develop"));
        let expected = (lane.commits_ahead_of_base, lane.commits_behind_base);
        assert_eq!(expected, (2, 1));

        let measured = |list: &[WorktreeInfo]| {
            list.iter()
                .find(|w| w.branch.as_deref() == Some("agent/00"))
                .and_then(|w| w.main_divergence.clone())
                .map(|d| (d.ahead, d.behind))
        };
        let batched = list_worktrees(root.to_str().unwrap()).expect("list");
        assert_eq!(measured(&batched), Some(expected), "batched");
        // The same counts when the batch is refused and each worktree walks.
        let format = format!("--format=%(refname)%00%(ahead-behind:{develop})");
        let walked = crate::engine::git_cli::with_forced_spawn_failure_of(&format, || {
            list_worktrees(root.to_str().unwrap())
        })
        .expect("list");
        assert_eq!(measured(&walked), Some(expected), "walked");
    }

    /// The test checkout's pre-commit guard, and the identity it refuses.
    fn guard_fixture_identity(repo: &Path) {
        std::fs::create_dir_all(repo.join(".githooks")).unwrap();
        std::fs::write(repo.join(".githooks/pre-commit"), "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::write(repo.join(".git/info/exclude"), ".githooks/\n").unwrap();
        // Not `test_support::git_in`, which refuses to persist this identity.
        git_text(repo, &["config", "user.name", "GitPulse Contract"]).unwrap();
        git_text(repo, &["config", "user.email", "contract@gitpulse.test"]).unwrap();
    }

    #[test]
    fn a_fixture_identity_is_refused_on_the_merge_commit_and_the_squash() {
        for squash in [false, true] {
            let (dir, wt) = teardown_fixture(None);
            let repo = dir.path();
            diverge_main(&dir);
            guard_fixture_identity(repo);
            let before = rev(repo, "main");
            let refused = teardown(&dir, &wt, squash, None).unwrap_err();
            assert!(
                matches!(&refused, MergeTeardownError::Refused { message } if message.contains("contract@gitpulse.test")),
                "squash {squash}: {refused:?}"
            );
            assert_eq!(
                rev(repo, "main"),
                before,
                "squash {squash}: a commit was made"
            );
            main_checkout_is_idle(repo);
            assert!(
                !repo.join("feature.txt").exists(),
                "squash {squash}: merged"
            );
            assert!(wt.path().exists() && branch_exists(repo, "feature-x"));
        }
    }

    #[test]
    fn a_diverged_plain_merge_makes_a_merge_commit_and_asks_the_gate_for_each_step() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        diverge_main(&dir);
        let wt_path = wt
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let mut asked = Vec::new();
        let result = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut |args: &[&str]| {
                asked.push(args.join(" "));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            asked,
            [
                "merge --ff-only feature-x".to_string(),
                "merge --no-edit feature-x -m Merge branch 'feature-x' into main".to_string(),
                format!("worktree remove --force {wt_path}"),
                "branch -d feature-x".to_string(),
            ]
        );
        assert!(result.worktree_removed && result.branch_deleted);
        assert_ne!(
            result.after_oid.as_deref(),
            Some(result.before_oid.as_str())
        );
        assert!(repo.join("feature.txt").exists() && repo.join("main-only.txt").exists());
    }

    /// Removal that git refuses (a locked worktree) after a merge that landed:
    /// the merge is reported, the worktree and branch are kept with reasons,
    /// and the removal was tried [`MAX_REMOVE_ATTEMPTS`] times, waiting
    /// [`REMOVE_BACKOFF`] between — on a clock the test supplies.
    #[test]
    fn a_removal_that_keeps_failing_is_retried_a_bounded_number_of_times() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        crate::test_support::git_in(repo, &["worktree", "lock", wt.path().to_str().unwrap()]);
        let mut waits = Vec::new();
        let result = merge_and_teardown_with(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut allow_all,
            &mut |wait| waits.push(wait),
        )
        .expect("a landed merge is not reported as a failure");
        assert!(result.merged && !result.worktree_removed && !result.branch_deleted);
        let reason = result
            .removal_error
            .expect("the removal says why it failed");
        assert!(
            reason.contains(&format!("after {MAX_REMOVE_ATTEMPTS} attempts")),
            "{reason}"
        );
        assert!(result.branch_error.is_some(), "a kept branch must say why");
        assert_eq!(
            waits, REMOVE_BACKOFF,
            "the retries did not back off as specified"
        );
        assert!(repo.join("feature.txt").exists(), "the merge was undone");
        assert!(wt.path().exists() && branch_exists(repo, "feature-x"));
    }

    /// A removal that fails once and then works leaves nothing behind, and a
    /// directory already gone is pruned rather than failed.
    #[test]
    fn a_worktree_directory_that_is_already_gone_is_pruned() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        std::fs::remove_dir_all(wt.path()).unwrap();
        let mut waits = Vec::new();
        remove_with_retry(repo, wt.path().to_str().unwrap(), &mut |wait| {
            waits.push(wait)
        })
        .unwrap();
        assert!(waits.is_empty(), "a vanished directory was waited for");
        assert_eq!(list_worktrees(repo.to_str().unwrap()).unwrap().len(), 1);
    }

    #[test]
    fn a_locked_lane_whose_directory_is_gone_is_reported_not_unregistered() {
        let (dir, wt) = teardown_fixture(None);
        crate::test_support::git_in(
            dir.path(),
            &["worktree", "lock", wt.path().to_str().unwrap()],
        );
        std::fs::remove_dir_all(wt.path()).unwrap();
        let refused =
            remove_with_retry(dir.path(), wt.path().to_str().unwrap(), &mut |_| {}).unwrap_err();
        assert!(refused.contains("locked"), "{refused}");
        assert_eq!(
            list_worktrees(dir.path().to_str().unwrap()).unwrap().len(),
            2,
            "the locked lane's record was removed"
        );
    }

    #[test]
    fn the_result_and_every_error_serialize_to_the_contract_shape() {
        let keys = |value: &serde_json::Value| -> Vec<String> {
            let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
            keys.sort();
            keys
        };
        let result = MergeTeardownResult {
            merged: true,
            merged_branch: "feature-x".into(),
            target_branch: "main".into(),
            commits_merged: 1,
            before_oid: "a".repeat(40),
            after_oid: Some("b".repeat(40)),
            base_oid: None,
            worktree_removed: false,
            removal_error: Some("locked".into()),
            branch_deleted: false,
            branch_error: Some("kept".into()),
            hook_error: None,
            hook_notice: None,
            cleanup_error: None,
            sweep_problems: Vec::new(),
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(
            keys(&value),
            [
                "after_oid",
                "base_oid",
                "before_oid",
                "branch_deleted",
                "branch_error",
                "cleanup_error",
                "commits_merged",
                "hook_error",
                "hook_notice",
                "merged",
                "merged_branch",
                "removal_error",
                "sweep_problems",
                "target_branch",
                "worktree_removed",
            ]
        );
        assert_eq!(value["base_oid"], serde_json::Value::Null);
        assert_eq!(
            serde_json::from_value::<MergeTeardownResult>(value).unwrap(),
            result
        );

        let cases: Vec<(MergeTeardownError, &str, Vec<&str>)> = vec![
            (
                MergeTeardownError::Conflict {
                    message: "m".into(),
                    paths: vec!["a".into()],
                    total: 1,
                    rolled_back: true,
                    rollback_error: None,
                },
                "conflict",
                vec![
                    "code",
                    "message",
                    "paths",
                    "rollback_error",
                    "rolled_back",
                    "total",
                ],
            ),
            (
                MergeTeardownError::Moved {
                    message: "m".into(),
                    branch: "b".into(),
                    expected: "e".into(),
                    actual: "a".into(),
                },
                "moved",
                vec!["actual", "branch", "code", "expected", "message"],
            ),
            (
                MergeTeardownError::BaseMismatch {
                    message: "m".into(),
                    base_branch: "x".into(),
                    target_branch: "y".into(),
                    base_oid: "o".into(),
                },
                "base_mismatch",
                vec![
                    "base_branch",
                    "base_oid",
                    "code",
                    "message",
                    "target_branch",
                ],
            ),
            (
                MergeTeardownError::SquashParked {
                    message: "m".into(),
                },
                "squash_parked",
                vec!["code", "message"],
            ),
            (
                MergeTeardownError::from("m".to_string()),
                "refused",
                vec!["code", "message"],
            ),
        ];
        for (error, code, expected_keys) in cases {
            let value = serde_json::to_value(&error).unwrap();
            assert_eq!(value["code"], code);
            assert_eq!(keys(&value), expected_keys, "{code}");
            assert_eq!(error.to_string(), "m");
            assert_eq!(
                serde_json::from_value::<MergeTeardownError>(value).unwrap(),
                error
            );
        }
    }

    /// A leftover `SQUASH_MSG` over an index that cannot be read is a read
    /// failure to report, not a squash known to be parked.
    #[test]
    fn an_unreadable_index_under_a_squash_message_is_reported_as_unreadable() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        std::fs::write(
            repo.join(".git/SQUASH_MSG"),
            "Squashed commit of the following:\n",
        )
        .unwrap();
        std::fs::write(repo.join(".git/index"), b"not an index").unwrap();
        let refused = teardown(&dir, &wt, false, None).unwrap_err();
        let MergeTeardownError::Refused { message } = &refused else {
            panic!("{refused:?}");
        };
        assert!(
            message.contains("could not be read")
                && message.contains("could not tell whether a squash is staged"),
            "{message}"
        );
        assert!(!message.contains("is parked"), "{message}");
    }

    fn spawned_with(cwds: &[PathBuf], needle: &str) -> usize {
        cwds.iter()
            .flat_map(|cwd| crate::engine::git_cli::spawn_log::spawns_in(cwd))
            .filter(|argv| argv.iter().any(|arg| arg == needle))
            .count()
    }

    /// The default-base lookup is a property of the repository: one per
    /// listing, not two processes for every worktree.
    #[test]
    fn a_full_listing_looks_up_main_once_whatever_the_worktree_count() {
        let (main, _parent, linked) = repo_with_worktrees(5);
        let root = plain(main.path());
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
        // The one lookup is the default base's: the remote HEAD symref is
        // what every computation of it starts with (git_reader's marker).
        assert!(
            spawned_with(std::slice::from_ref(&root), "refs/remotes/origin/HEAD") <= 1,
            "more than one default-base lookup"
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
        let root = plain(main.path());
        // Two commits on one agent branch, so the counts are not all zero.
        for n in 0..2 {
            std::fs::write(linked[1].join(format!("w{n}.txt")), "x").unwrap();
            git_in(&linked[1], &["add", "."]);
            git_in(&linked[1], &["commit", "-q", "-m", &format!("w{n}")]);
        }
        let batched = list_worktrees(root.to_str().unwrap()).expect("list");
        let format = format!(
            "--format=%(refname)%00%(ahead-behind:{})",
            rev(&root, "main")
        );
        let before = spawned_with(std::slice::from_ref(&root), "rev-list");
        let walked = crate::engine::git_cli::with_forced_spawn_failure_of(&format, || {
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
    /// not a branch named main; master is the base then, and `trunk` once
    /// master is renamed. (Re-homed from the deleted main-probe helper: the exact-name
    /// rule now lives in the default base the worktree listing reads.)
    #[test]
    fn main_is_an_exact_branch_name_not_a_prefix() {
        let dir = tempfile::TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-q", "-b", "master"]);
        git_in(dir.path(), &["commit", "-q", "--allow-empty", "-m", "base"]);
        git_in(dir.path(), &["branch", "main/feature"]);
        let root = plain(dir.path());
        let named = |repo: &Path| {
            crate::engine::git_reader::compute_default_base(repo)
                .base
                .map(|(name, _)| name)
        };
        assert_eq!(named(&root).as_deref(), Some("master"));
        git_in(dir.path(), &["branch", "-q", "-m", "master", "trunk"]);
        assert_eq!(named(&root).as_deref(), Some("trunk"));
    }

    /// A lookup that could not run is named on every worktree it would have
    /// measured, not read as "no default branch".
    #[test]
    fn a_failed_main_lookup_is_named_on_the_worktrees_it_left_unmeasured() {
        let (main, _parent, _linked) = repo_with_worktrees(2);
        let root = plain(main.path());
        let listed = crate::engine::git_cli::with_forced_spawn_failure_of("for-each-ref", || {
            list_worktrees(root.to_str().unwrap())
        })
        .expect("list");
        // Without the base no branch is known to be it, so the primary
        // checkout is unmeasured too and says why (it used to be exempt as
        // "main itself", a name this no longer assumes).
        assert_eq!(listed[0].main_divergence, None);
        let primary = listed[0].scan_note.as_deref().unwrap_or_default();
        assert!(primary.contains("forced by test"), "{primary}");
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
        let root = plain(main.path());
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
        let others = [plain(main.path()), linked[0].clone()];
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
            assert!(
                started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(10)),
                "never {what}"
            );
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
        let root = plain(main.path());
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
        let root = plain(main.path());
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
        // Bounded negative: poll the whole window and fail at the first read
        // that appears, rather than sampling once after a fixed sleep. The
        // positive control is below: once released the later listing does
        // read, so a counter that could never move would fail there.
        let window = std::time::Instant::now() + Duration::from_millis(150);
        while std::time::Instant::now() < window {
            assert_eq!(
                listings_in(&root) - before,
                1,
                "the later listing read early"
            );
            assert!(!late.is_finished(), "the later listing ended while queued");
            std::thread::sleep(Duration::from_millis(2));
        }
        release_tx.send(()).unwrap();
        let early = early.join().unwrap().expect("early");
        let late = late.join().unwrap().expect("late");
        assert_eq!(
            listings_in(&root) - before,
            2,
            "positive control: the later listing read once released"
        );
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
        let root = plain(main.path());
        let before = listings_in(&root);
        let leader = lead(&root, ScanDepth::Dirty, Duration::from_millis(500));
        wait_until("gathering", || gathering(&root));
        let path = root.to_str().unwrap().to_string();
        let started = std::time::Instant::now();
        let bare = list_worktrees_scanned(&path, ScanDepth::Listing).expect("listing");
        assert!(
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_millis(400)),
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
        let root = plain(main.path());
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

    /// The metadata view names exactly what `git worktree list` names — the
    /// primary checkout, live linked worktrees, no pruned ones — and gives up
    /// (`None`) where the layout cannot account for the primary checkout.
    #[test]
    fn metadata_checkouts_match_git_and_give_up_on_core_worktree() {
        let main = tempfile::TempDir::new().unwrap();
        git_in(main.path(), &["init", "-b", "main"]);
        std::fs::write(main.path().join("seed.txt"), "seed").unwrap();
        git_in(main.path(), &["add", "."]);
        git_in(main.path(), &["commit", "-m", "init"]);
        let parent = tempfile::TempDir::new().unwrap();
        let live = parent.path().join("live");
        let pruned = parent.path().join("pruned");
        for path in [&live, &pruned] {
            git_in(
                main.path(),
                &["worktree", "add", "--detach", path.to_str().unwrap()],
            );
        }
        crate::test_support::trust_repo(&live);
        std::fs::remove_dir_all(&pruned).unwrap();

        let repo = main.path().to_str().unwrap();
        let live_path = live.canonicalize().unwrap();
        let pair = admit_worktree_pair(repo, live_path.to_str().unwrap()).expect("admitted");
        let mut from_metadata = pair
            .metadata_checkouts()
            .expect("a plain layout is accounted for");
        from_metadata.sort();
        from_metadata.dedup();
        let mut from_git = pair.clone().authenticate().expect("authenticated").members;
        from_git.sort();
        assert_eq!(from_metadata, from_git);
        assert!(!from_metadata.iter().any(|path| path.ends_with("pruned")));

        git_in(
            main.path(),
            &["config", "extensions.worktreeConfig", "true"],
        );
        assert!(
            pair.metadata_checkouts().is_some(),
            "extensions.worktreeConfig does not move the primary checkout"
        );

        git_in(main.path(), &["config", "include.path", "elsewhere.cfg"]);
        assert_eq!(pair.metadata_checkouts(), None, "an include may set it");
        git_in(main.path(), &["config", "--unset", "include.path"]);
        assert!(pair.metadata_checkouts().is_some());

        git_in(main.path(), &["config", "core.worktree", repo]);
        assert_eq!(
            pair.metadata_checkouts(),
            None,
            "a configured core.worktree must send the caller to Git"
        );
    }

    // ---------------------------------------------------------------------
    // Adversarial cases for merge_and_teardown (card 22).
    // ---------------------------------------------------------------------

    fn adv_read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_else(|e| format!("<unreadable: {e}>"))
    }

    fn adv_base_file(wt: &Path) -> PathBuf {
        crate::engine::git_cli::resolve_git_dir(wt)
            .unwrap()
            .join(BASE_RECORD)
    }

    /// Unstaged edit to a tracked file the merge does not touch, plus an
    /// untracked file: both must survive a conflicting merge and its rollback.
    #[test]
    fn adv_unrelated_unstaged_and_untracked_work_survives_a_conflict() {
        for squash in [false, true] {
            let (dir, wt) = conflicting_fixture();
            let repo = dir.path();
            std::fs::write(repo.join("notes.txt"), "base\n").unwrap();
            crate::test_support::git_in(repo, &["add", "notes.txt"]);
            crate::test_support::git_in(repo, &["commit", "-qm", "notes"]);
            // The lane was cut before notes.txt, so it is main-only.
            std::fs::write(repo.join("notes.txt"), "user unstaged edit\n").unwrap();
            std::fs::write(repo.join("scratch.txt"), "user untracked\n").unwrap();
            let err = teardown(&dir, &wt, squash, None).unwrap_err();
            assert!(
                matches!(err, MergeTeardownError::Conflict { .. }),
                "squash={squash}: {err:?}"
            );
            assert_eq!(
                adv_read(&repo.join("notes.txt")),
                "user unstaged edit\n",
                "squash={squash}"
            );
            assert_eq!(
                adv_read(&repo.join("scratch.txt")),
                "user untracked\n",
                "squash={squash}"
            );
        }
    }

    /// A STAGED unrelated change in main, then a conflicting merge.
    #[test]
    fn adv_unrelated_staged_work_survives_a_conflict() {
        for squash in [false, true] {
            let (dir, wt) = conflicting_fixture();
            let repo = dir.path();
            std::fs::write(repo.join("staged.txt"), "user staged\n").unwrap();
            crate::test_support::git_in(repo, &["add", "staged.txt"]);
            let _ = teardown(&dir, &wt, squash, None);
            assert_eq!(
                adv_read(&repo.join("staged.txt")),
                "user staged\n",
                "squash={squash}: staged file lost from the working tree"
            );
            let status = git_text(repo, &["status", "--porcelain"]).unwrap();
            assert!(
                status.contains("A  staged.txt"),
                "squash={squash}: the user's staged add is no longer staged: {status:?}"
            );
        }
    }

    /// Unrelated staged work must not be swept into a merge commit or a squash
    /// commit that the user never asked to contain it.
    #[test]
    fn adv_unrelated_staged_work_is_not_committed_into_the_merge() {
        for squash in [false, true] {
            let (dir, wt) = teardown_fixture(None);
            let repo = dir.path();
            diverge_main(&dir);
            std::fs::write(repo.join("staged.txt"), "user staged\n").unwrap();
            crate::test_support::git_in(repo, &["add", "staged.txt"]);
            let result = teardown(&dir, &wt, squash, None);
            let tree = git_text(repo, &["ls-tree", "-r", "--name-only", "HEAD"]).unwrap();
            assert!(
                !tree.lines().any(|l| l == "staged.txt"),
                "squash={squash}: the user's staged file was committed into the merge: result={result:?}\n{tree}"
            );
        }
    }

    /// A merge that cannot start because of local edits to a file it touches.
    #[test]
    fn adv_dirty_file_the_merge_touches_is_left_alone() {
        for squash in [false, true] {
            let (dir, wt) = teardown_fixture(None);
            let repo = dir.path();
            diverge_main(&dir);
            std::fs::write(
                repo.join("feature.txt"),
                "user's own untracked feature.txt\n",
            )
            .unwrap();
            let err = teardown(&dir, &wt, squash, None);
            assert_eq!(
                adv_read(&repo.join("feature.txt")),
                "user's own untracked feature.txt\n",
                "squash={squash}: untracked file the merge would overwrite was lost: {err:?}"
            );
            assert!(
                wt.path().exists() && branch_exists(repo, "feature-x"),
                "squash={squash}"
            );
            assert!(
                err.is_err(),
                "squash={squash}: merged over an untracked file"
            );
        }
    }

    /// Unstaged edit to a tracked file the merge DOES touch.
    #[test]
    fn adv_unstaged_edit_to_a_file_the_merge_touches_is_kept() {
        for squash in [false, true] {
            let (dir, wt) = conflicting_fixture();
            let repo = dir.path();
            // README.md conflicts; the user also has an unstaged edit to it.
            std::fs::write(repo.join("README.md"), "user in-progress edit of README\n").unwrap();
            let _ = teardown(&dir, &wt, squash, None);
            assert_eq!(
                adv_read(&repo.join("README.md")),
                "user in-progress edit of README\n",
                "squash={squash}: unstaged edit overwritten"
            );
        }
    }

    /// The user is mid stash-pop: unmerged entries, no MERGE_HEAD. The merge
    /// refuses to start; the "no marker" rollback branch must not `reset
    /// --merge` the user's own conflicted files away.
    #[test]
    fn adv_a_users_unmerged_index_without_a_marker_is_not_reset() {
        for squash in [false, true] {
            let (dir, wt) = teardown_fixture(None);
            let repo = dir.path();
            std::fs::write(repo.join("README.md"), "stashed side\n").unwrap();
            crate::test_support::git_in(repo, &["stash", "push", "-q", "-m", "adv-card22-unique"]);
            std::fs::write(repo.join("README.md"), "head side\n").unwrap();
            crate::test_support::git_in(repo, &["commit", "-qam", "head moves"]);
            // Conflicting pop: exit status 1, leaves UU with no MERGE_HEAD.
            let _ = std::process::Command::new("git")
                .args(["stash", "pop"])
                .current_dir(repo)
                .output_locked();
            let unmerged_before = git_text(repo, &["ls-files", "--unmerged"]).unwrap();
            assert!(
                !unmerged_before.trim().is_empty(),
                "fixture did not conflict"
            );
            let user_file = adv_read(&repo.join("README.md"));
            assert!(user_file.contains("<<<<<<<"), "{user_file}");
            // The user has resolved by hand but not yet `git add`ed.
            std::fs::write(repo.join("README.md"), "hand resolved, precious\n").unwrap();
            let _ = teardown(&dir, &wt, squash, None);
            assert_eq!(
                adv_read(&repo.join("README.md")),
                "hand resolved, precious\n",
                "squash={squash}: the user's hand-resolved file was reset by the rollback"
            );
            crate::test_support::git_in(repo, &["stash", "drop", "-q"]);
        }
    }

    #[test]
    fn adv_expected_tip_forms() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        let tip = rev(wt.path(), "HEAD");
        let before = rev(repo, "main");
        let long65 = "a".repeat(65);
        let hex64 = "a".repeat(64);
        let hex41 = "a".repeat(41);
        // Only a full id (exactly 40 or 64 hex digits) names a commit for good,
        // so an odd length is refused as not-an-id, where it used to pass as an
        // id that happened never to match and come back "Moved".
        for bad in [
            tip[..7].to_string(),
            tip[..39].to_string(),
            "main".to_string(),
            "feature-x".to_string(),
            "".to_string(),
            "   ".to_string(),
            "HEAD".to_string(),
            format!("{tip}\n{tip}"),
            format!("{tip}\0"),
            long65,
            hex41,
        ] {
            let r = teardown(&dir, &wt, false, Some(&bad));
            assert!(r.is_err(), "tip {bad:?} merged: {r:?}");
            assert_eq!(rev(repo, "main"), before, "tip {bad:?} moved main");
            assert!(wt.path().exists() && branch_exists(repo, "feature-x"));
        }
        // 64 hex is a well-formed id, just not a sha1 tip: never equal, so Moved.
        let r = teardown(&dir, &wt, false, Some(&hex64));
        assert!(
            matches!(r, Err(MergeTeardownError::Moved { .. })),
            "64-hex tip: {r:?}"
        );
        assert_eq!(rev(repo, "main"), before);
        // Uppercase of the right tip is accepted (normalised).
        let r = teardown(&dir, &wt, false, Some(&tip.to_ascii_uppercase()));
        assert!(r.is_ok(), "uppercase tip refused: {r:?}");
    }

    #[test]
    fn adv_target_branch_names_are_refused_before_anything_runs() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        let before = rev(repo, "main");
        for bad in [
            "-x",
            "--abort",
            "a..b",
            "main\nfoo",
            "",
            " main",
            "main.lock",
            "@{-1}",
            "refs/heads/../main",
            "main\0",
            "ma in",
            "ma~in",
            "HEAD",
            "refs/heads/main",
        ] {
            let r = merge_and_teardown_worktree(
                repo.to_str().unwrap(),
                wt.path().to_str().unwrap(),
                Some(bad),
                false,
                None,
                &mut allow_all,
            );
            assert!(r.is_err(), "target {bad:?} was accepted: {r:?}");
            assert_eq!(rev(repo, "main"), before, "target {bad:?}");
            assert!(wt.path().exists(), "target {bad:?} tore the worktree down");
        }
    }

    #[test]
    fn adv_worktree_path_forms() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        let run = |p: &str| {
            merge_and_teardown_worktree(
                repo.to_str().unwrap(),
                p,
                Some("main"),
                false,
                None,
                &mut allow_all,
            )
        };
        let before = rev(repo, "main");
        // The main checkout itself, spelled several ways.
        for p in [
            repo.to_str().unwrap().to_string(),
            format!("{}/", repo.display()),
            format!("{}/.", repo.display()),
            format!(
                "{}/../{}",
                repo.display(),
                repo.file_name().unwrap().to_str().unwrap()
            ),
            format!("{}/.git", repo.display()),
            format!("{}/.git/worktrees", repo.display()),
        ] {
            let r = run(&p);
            assert!(r.is_err(), "primary checkout as {p:?} merged: {r:?}");
            assert!(repo.join(".git").exists());
            assert_eq!(rev(repo, "main"), before);
        }
        // Missing, empty, traversal into a non-worktree, NUL.
        for p in [
            "",
            "/nonexistent/zzz",
            "..",
            "../..",
            "relative/path",
            "a\0b",
        ] {
            let r = run(p);
            assert!(r.is_err(), "path {p:?} merged: {r:?}");
        }
        assert!(wt.path().exists());
        // A symlink to the real worktree is the same worktree: either accepted
        // and torn down cleanly, or refused; never a half state.
        #[cfg(unix)]
        {
            let link_dir = tempfile::tempdir().unwrap();
            let link = link_dir.path().join("link to wt");
            std::os::unix::fs::symlink(wt.path(), &link).unwrap();
            let r = run(link.to_str().unwrap());
            match r {
                Ok(res) => {
                    assert!(res.merged);
                    assert!(res.worktree_removed, "{:?}", res.removal_error);
                    assert!(!wt.path().exists());
                }
                Err(e) => {
                    assert!(wt.path().exists(), "{e:?}");
                }
            }
        }
    }

    #[test]
    fn adv_worktree_path_with_spaces_and_unicode() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("re po");
        std::fs::create_dir_all(&repo).unwrap();
        crate::test_support::git_in(&repo, &["init", "-q", "-b", "main", "."]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        crate::test_support::git_in(&repo, &["add", "."]);
        crate::test_support::git_in(&repo, &["commit", "-qm", "init"]);
        crate::test_support::trust_repo(&repo);
        let wt_path = dir.path().join("laneé 日本 \u{202e}x");
        add_worktree_extended(
            repo.to_str().unwrap(),
            wt_path.to_str().unwrap(),
            Some("feat-é"),
            None,
            false,
            false,
            false,
        )
        .expect("worktree");
        crate::test_support::trust_repo(&wt_path);
        std::fs::write(wt_path.join("b.txt"), "b\n").unwrap();
        crate::test_support::git_in(&wt_path, &["add", "."]);
        crate::test_support::git_in(&wt_path, &["commit", "-qm", "lane"]);
        let res = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            wt_path.to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut allow_all,
        )
        .expect("merge");
        assert!(res.worktree_removed, "{:?}", res.removal_error);
        assert!(res.branch_deleted, "{:?}", res.branch_error);
        assert!(repo.join("b.txt").exists());
    }

    fn adv_with_base_record(content: &[u8]) -> Result<Option<(String, String)>, String> {
        let (_dir, wt) = teardown_fixture(None);
        std::fs::write(adv_base_file(wt.path()), content).unwrap();
        read_base_record(wt.path())
    }

    #[test]
    fn adv_hostile_base_records_are_refused_not_trusted() {
        let oid = "a".repeat(40);
        let big = vec![b'a'; 1 << 20];
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("huge", big),
            ("nul in branch", format!("ma\0in\n{oid}\n").into_bytes()),
            (
                "nul in oid",
                format!("main\n{}\0\n", &oid[..39]).into_bytes(),
            ),
            ("three lines", format!("main\n{oid}\nextra\n").into_bytes()),
            ("one line", b"main\n".to_vec()),
            ("empty", Vec::new()),
            ("non-hex oid", b"main\nzzzz\n".to_vec()),
            ("non-utf8", vec![0xff, 0xfe, b'\n', b'a', b'\n']),
            ("flag-like branch", format!("--abort\n{oid}\n").into_bytes()),
            ("dotdot branch", format!("a..b\n{oid}\n").into_bytes()),
            (
                "oid 65 long",
                format!("main\n{}\n", "a".repeat(65)).into_bytes(),
            ),
            ("oid empty", b"main\n\n".to_vec()),
            ("blank line first", format!("\nmain\n{oid}\n").into_bytes()),
        ];
        for (name, bytes) in cases {
            let r = adv_with_base_record(&bytes);
            assert!(r.is_err(), "{name}: hostile base record accepted: {r:?}");
        }
        // Well-formed forms that must be accepted: CRLF (Windows), no final newline.
        assert_eq!(
            adv_with_base_record(format!("main\r\n{oid}\r\n").as_bytes()).unwrap(),
            Some(("main".into(), oid.clone()))
        );
        assert_eq!(
            adv_with_base_record(format!("main\n{oid}").as_bytes()).unwrap(),
            Some(("main".into(), oid.clone()))
        );
        // A 4-hex "oid" is syntactically a valid abbreviation. Is it accepted?
        let short = adv_with_base_record(b"main\nabcd\n");
        assert!(
            short.is_err(),
            "a 4-character oid was accepted as the base: {short:?}"
        );
    }

    /// A hostile record must refuse the merge, not skip the check.
    #[test]
    fn adv_a_corrupt_base_record_refuses_the_merge() {
        let (dir, wt) = teardown_fixture(None);
        std::fs::write(adv_base_file(wt.path()), b"main\nnot-an-oid\n").unwrap();
        let before = rev(dir.path(), "main");
        let r = teardown(&dir, &wt, false, None);
        assert!(r.is_err(), "{r:?}");
        assert_eq!(rev(dir.path(), "main"), before);
        assert!(wt.path().exists() && branch_exists(dir.path(), "feature-x"));
    }

    /// The base record is a directory, or a symlink to a record elsewhere.
    #[cfg(unix)]
    #[test]
    fn adv_base_record_as_a_directory_or_symlink() {
        let (dir, wt) = teardown_fixture(None);
        let path = adv_base_file(wt.path());
        let _ = std::fs::remove_file(&path);
        std::fs::create_dir(&path).unwrap();
        let r = teardown(&dir, &wt, false, None);
        assert!(
            r.is_err(),
            "a directory as the base record skipped the check: {r:?}"
        );
        std::fs::remove_dir(&path).unwrap();
        // Symlink to a record naming another branch: followed, and mismatch is caught.
        let other = tempfile::tempdir().unwrap();
        let target = other.path().join("rec");
        std::fs::write(&target, format!("other\n{}\n", "a".repeat(40))).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let r = teardown(&dir, &wt, false, None);
        assert!(
            matches!(r, Err(MergeTeardownError::BaseMismatch { .. })),
            "{r:?}"
        );
    }

    #[test]
    fn adv_a_conflict_of_ten_thousand_files_is_bounded() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        for side in [wt.path(), repo] {
            for i in 0..10_000 {
                let sub = side.join(format!("d{}", i % 100));
                std::fs::create_dir_all(&sub).unwrap();
                std::fs::write(
                    sub.join(format!("f{i}.txt")),
                    format!("{}\n", side.display()),
                )
                .unwrap();
            }
            crate::test_support::git_in(side, &["add", "-A"]);
            crate::test_support::git_in(side, &["commit", "-qm", "ten thousand"]);
        }
        for squash in [false, true] {
            let started = std::time::Instant::now();
            let err = teardown(&dir, &wt, squash, None).unwrap_err();
            let MergeTeardownError::Conflict {
                message,
                paths,
                total,
                rolled_back,
                ..
            } = err
            else {
                panic!("squash={squash}: {err:?}");
            };
            let json = serde_json::to_string(&MergeTeardownError::Conflict {
                message: message.clone(),
                paths: paths.clone(),
                total,
                rolled_back,
                rollback_error: None,
            })
            .unwrap();
            assert!(rolled_back, "squash={squash}");
            assert!(
                total >= 10_000,
                "squash={squash}: total {total} is a capped count"
            );
            assert!(
                paths.len() <= 1000,
                "squash={squash}: {} paths cross the IPC boundary",
                paths.len()
            );
            assert!(
                message.len() < 2000,
                "squash={squash}: message {} bytes",
                message.len()
            );
            assert!(
                json.len() < 200_000,
                "squash={squash}: error is {} bytes",
                json.len()
            );
            main_checkout_is_idle(repo);
            eprintln!("squash={squash} took {:?}", started.elapsed());
        }
    }

    #[test]
    fn adv_modify_delete_and_rename_delete_conflicts_list_paths() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        crate::test_support::git_in(wt.path(), &["rm", "-q", "README.md"]);
        crate::test_support::git_in(wt.path(), &["commit", "-qm", "lane deletes README"]);
        std::fs::write(repo.join("README.md"), "main edit\n").unwrap();
        crate::test_support::git_in(repo, &["commit", "-qam", "main edits README"]);
        for squash in [false, true] {
            let MergeTeardownError::Conflict {
                paths,
                total,
                rolled_back,
                ..
            } = teardown(&dir, &wt, squash, None).unwrap_err()
            else {
                panic!("squash={squash}");
            };
            assert!(
                !paths.is_empty() && total >= 1 && rolled_back,
                "squash={squash}"
            );
            main_checkout_is_idle(repo);
            assert_eq!(adv_read(&repo.join("README.md")), "main edit\n");
        }
    }

    /// index.lock held by another git process when the merge starts.
    #[test]
    fn adv_index_lock_held_during_the_merge() {
        for squash in [false, true] {
            let (dir, wt) = teardown_fixture(None);
            let repo = dir.path();
            diverge_main(&dir);
            let lock = repo.join(".git/index.lock");
            std::fs::write(&lock, b"").unwrap();
            let before = rev(repo, "main");
            let r = teardown(&dir, &wt, squash, None);
            assert!(
                r.is_err(),
                "squash={squash}: merged under a held index.lock: {r:?}"
            );
            assert!(
                lock.exists(),
                "squash={squash}: another process's index.lock was removed"
            );
            assert_eq!(rev(repo, "main"), before);
            assert!(
                wt.path().exists() && branch_exists(repo, "feature-x"),
                "squash={squash}"
            );
            std::fs::remove_file(&lock).unwrap();
            // And once the lock is gone the same call goes through.
            let r = teardown(&dir, &wt, squash, None).expect("retry after the lock cleared");
            assert!(r.merged && r.worktree_removed, "squash={squash}: {r:?}");
        }
    }

    #[test]
    fn adv_rollback_under_a_held_index_lock_is_an_error_not_clean() {
        for squash in [false, true] {
            let (dir, _wt) = conflicting_fixture();
            let repo = dir.path();
            let _ = std::process::Command::new("git")
                .args(if squash {
                    vec!["merge", "--squash", "feature-x"]
                } else {
                    vec!["merge", "--no-edit", "feature-x"]
                })
                .current_dir(repo)
                .output_locked();
            assert!(crate::engine::repo_op::detect_kind(repo).unwrap().is_some() || squash);
            let lock = repo.join(".git/index.lock");
            std::fs::write(&lock, b"").unwrap();
            let r = GitWriter::rollback_failed_merge_locked(repo);
            assert!(
                r.is_err(),
                "squash={squash}: rollback reported clean while index.lock blocked it (unmerged left: {:?})",
                git_text(repo, &["ls-files", "--unmerged"])
            );
            std::fs::remove_file(&lock).unwrap();
            GitWriter::rollback_failed_merge_locked(repo).expect("rollback after lock cleared");
            main_checkout_is_idle(repo);
        }
    }

    /// Two merge_and_teardown calls at once on two lanes of one repo.
    #[test]
    fn adv_two_concurrent_teardowns_serialise() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path().to_path_buf();
        let wt2 = tempfile::tempdir().unwrap();
        add_worktree_extended(
            repo.to_str().unwrap(),
            wt2.path().to_str().unwrap(),
            Some("feature-y"),
            None,
            false,
            false,
            false,
        )
        .unwrap();
        crate::test_support::trust_repo(wt2.path());
        std::fs::write(wt2.path().join("other.txt"), "lane y\n").unwrap();
        crate::test_support::git_in(wt2.path(), &["add", "."]);
        crate::test_support::git_in(wt2.path(), &["commit", "-qm", "lane y"]);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let run = |lane: PathBuf| {
            let repo = repo.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                merge_and_teardown_worktree(
                    repo.to_str().unwrap(),
                    lane.to_str().unwrap(),
                    Some("main"),
                    false,
                    None,
                    &mut allow_all,
                )
            })
        };
        let a = run(wt.path().to_path_buf());
        let b = run(wt2.path().to_path_buf());
        let (a, b) = (a.join().unwrap(), b.join().unwrap());
        let a = a.expect("lane x");
        let b = b.expect("lane y");
        assert!(
            a.merged && b.merged && a.worktree_removed && b.worktree_removed,
            "{a:?} {b:?}"
        );
        assert!(repo.join("feature.txt").exists() && repo.join("other.txt").exists());
        // Whichever merged second saw the first's tip as its before_oid.
        let (first, second) = if a.after_oid.as_deref() == Some(b.before_oid.as_str()) {
            (&a, &b)
        } else {
            (&b, &a)
        };
        assert_eq!(
            first.after_oid.as_deref(),
            Some(second.before_oid.as_str()),
            "{a:?} {b:?}"
        );
        main_checkout_is_idle(&repo);
    }

    /// The same lane torn down twice at once: one wins, the other refuses.
    #[test]
    fn adv_the_same_lane_twice_concurrently() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path().to_path_buf();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let run = || {
            let (repo, lane, barrier) = (repo.clone(), wt.path().to_path_buf(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                merge_and_teardown_worktree(
                    repo.to_str().unwrap(),
                    lane.to_str().unwrap(),
                    Some("main"),
                    false,
                    None,
                    &mut allow_all,
                )
            })
        };
        let (a, b) = (run(), run());
        let (a, b) = (a.join().unwrap(), b.join().unwrap());
        let oks = [&a, &b].iter().filter(|r| r.is_ok()).count();
        assert!(oks >= 1, "neither succeeded: {a:?} {b:?}");
        for res in [&a, &b].into_iter().flatten() {
            assert!(res.merged);
        }
        main_checkout_is_idle(&repo);
        assert!(repo.join("feature.txt").exists());
    }

    /// Crash after the merge but before teardown: the next call recovers.
    #[test]
    fn adv_recovers_after_a_crash_between_merge_and_teardown() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        crate::test_support::git_in(repo, &["merge", "--ff-only", "feature-x"]);
        let r = teardown(&dir, &wt, false, None).expect("recovery");
        assert!(r.merged && r.worktree_removed && r.branch_deleted, "{r:?}");
    }

    #[test]
    fn adv_recovers_after_a_squash_commit_that_landed_before_teardown() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        crate::test_support::git_in(repo, &["merge", "--squash", "feature-x"]);
        crate::test_support::git_in(repo, &["commit", "-qm", "squash by hand"]);
        assert_eq!(
            crate::engine::repo_op::detect_kind(repo).unwrap(),
            None,
            "a squash that was committed (with -m) is still reported as parked: SQUASH_MSG is stale"
        );
        let r = teardown(&dir, &wt, true, None);
        // Nothing to commit, branch's changes already on target: success or a
        // clean refusal; never leaving a half state.
        match r {
            Ok(res) => assert!(res.merged, "{res:?}"),
            Err(e) => assert!(wt.path().exists(), "{e:?}"),
        }
        main_checkout_is_idle(repo);
    }

    /// A squash with nothing to commit (the lane's change is already on main)
    /// returns Ok(false) with SQUASH_MSG still on disk, which repo_op now reads
    /// as a parked squash: the next teardown is refused for good.
    #[test]
    fn adv_a_squash_with_nothing_to_commit_does_not_park_main() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        // The same change lands on main by another route (a cherry-pick).
        crate::test_support::git_in(repo, &["cherry-pick", "feature-x"]);
        let first = teardown(&dir, &wt, true, None).expect("first squash teardown");
        assert!(first.merged, "{first:?}");
        assert_eq!(
            crate::engine::repo_op::detect_kind(repo).unwrap(),
            None,
            "main is reported as having a parked squash after a squash that had nothing to commit"
        );
        // A second lane must not be refused with SquashParked.
        let wt2 = tempfile::tempdir().unwrap();
        add_worktree_extended(
            repo.to_str().unwrap(),
            wt2.path().to_str().unwrap(),
            Some("feature-q"),
            None,
            false,
            false,
            false,
        )
        .unwrap();
        crate::test_support::trust_repo(wt2.path());
        std::fs::write(wt2.path().join("q.txt"), "q\n").unwrap();
        crate::test_support::git_in(wt2.path(), &["add", "."]);
        crate::test_support::git_in(wt2.path(), &["commit", "-qm", "q"]);
        let second = teardown(&dir, &wt2, true, None);
        assert!(second.is_ok(), "the next merge was refused: {second:?}");
    }

    /// Crash mid-merge (MERGE_HEAD present from the first call) refuses the next.
    #[test]
    fn adv_a_crash_mid_merge_makes_the_next_call_refuse_without_touching_it() {
        let (dir, wt) = conflicting_fixture();
        let repo = dir.path();
        let _ = std::process::Command::new("git")
            .args(["merge", "--no-edit", "feature-x"])
            .current_dir(repo)
            .output_locked();
        let marker = crate::engine::git_cli::resolve_git_dir(repo)
            .unwrap()
            .join("MERGE_HEAD");
        assert!(marker.exists());
        let before = adv_read(&repo.join("README.md"));
        let err = teardown(&dir, &wt, false, None).unwrap_err();
        assert!(matches!(err, MergeTeardownError::Refused { .. }), "{err:?}");
        assert!(
            marker.exists(),
            "the parked merge was aborted by the refusal"
        );
        assert_eq!(adv_read(&repo.join("README.md")), before);
    }

    /// The gate refuses the removal and the branch delete: Ok with reasons.
    #[test]
    fn adv_gate_refusing_removal_and_branch_delete_is_ok_with_reasons() {
        let (dir, wt) = teardown_fixture(None);
        let mut gate = |argv: &[&str]| -> Result<(), String> {
            if argv.first() == Some(&"worktree") || argv.first() == Some(&"branch") {
                Err("policy: denied".into())
            } else {
                Ok(())
            }
        };
        let res = merge_and_teardown_worktree(
            dir.path().to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut gate,
        )
        .expect("the merge landed");
        assert!(res.merged && !res.worktree_removed && !res.branch_deleted);
        assert!(res.removal_error.is_some() && res.branch_error.is_some());
        assert!(wt.path().exists() && branch_exists(dir.path(), "feature-x"));
    }

    /// Branch delete refused by the gate after a successful removal.
    #[test]
    fn adv_branch_delete_refused_after_removal() {
        let (dir, wt) = teardown_fixture(None);
        let mut gate = |argv: &[&str]| -> Result<(), String> {
            if argv.first() == Some(&"branch") {
                Err("policy: no branch delete".into())
            } else {
                Ok(())
            }
        };
        let res = merge_and_teardown_worktree(
            dir.path().to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut gate,
        )
        .unwrap();
        assert!(res.worktree_removed && !res.branch_deleted);
        assert!(res.branch_error.unwrap().contains("no branch delete"));
    }

    /// A locked worktree: removal fails every attempt, retried exactly 3 times.
    #[test]
    fn adv_a_locked_lane_is_reported_after_bounded_retries() {
        let (dir, wt) = teardown_fixture(None);
        crate::test_support::git_in(
            dir.path(),
            &["worktree", "lock", wt.path().to_str().unwrap()],
        );
        let mut sleeps = Vec::new();
        let res = merge_and_teardown_with(
            dir.path().to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut allow_all,
            &mut |d| sleeps.push(d),
        )
        .expect("merge landed");
        assert!(res.merged && !res.worktree_removed, "{res:?}");
        assert_eq!(sleeps.len(), 2, "{sleeps:?}");
        assert!(res.branch_error.is_some());
        assert!(wt.path().exists());
    }

    /// `worktree prune --expire now` after a failed removal prunes EVERY stale
    /// record. An unrelated lane whose directory is momentarily absent (an
    /// unmounted volume) must not lose its admin data.
    #[test]
    fn adv_the_removal_retry_prune_does_not_delete_an_unrelated_lanes_record() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        // An unrelated lane on a "volume" that is unmounted right now.
        let unrelated_parent = tempfile::tempdir().unwrap();
        let unrelated = unrelated_parent.path().join("lane-z");
        add_worktree_extended(
            repo.to_str().unwrap(),
            unrelated.to_str().unwrap(),
            Some("feature-z"),
            None,
            false,
            false,
            false,
        )
        .unwrap();
        let parked = unrelated_parent.path().join("lane-z-unmounted");
        std::fs::rename(&unrelated, &parked).unwrap();
        // The merged lane is locked so the first removal fails, then its
        // directory goes away (as a partial delete would leave it).
        crate::test_support::git_in(repo, &["worktree", "lock", wt.path().to_str().unwrap()]);
        let wt_dir = wt.path().to_path_buf();
        let mut sleeps = 0;
        let res = merge_and_teardown_with(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut allow_all,
            &mut |_| {
                sleeps += 1;
                let _ = std::fs::remove_dir_all(&wt_dir);
            },
        )
        .expect("merge landed");
        eprintln!("result: {res:?}");
        // The "volume" comes back: the lane must still be a registered worktree.
        std::fs::rename(&parked, &unrelated).unwrap();
        let listed = git_text(repo, &["worktree", "list", "--porcelain"]).unwrap();
        assert!(
            listed.contains("lane-z"),
            "the unrelated lane's worktree record was pruned by the removal retry:\n{listed}"
        );
    }

    /// Originally `adv_the_retry_prune_is_gated`, which recorded the defect
    /// that the retry ran a repo-wide `worktree prune --expire now` (and only
    /// asked that the gate see it). It flipped when the prune was removed: a
    /// prune drops every stale record, an unmounted unrelated lane's included.
    /// Now no prune reaches the gate or runs, and a partial removal deletes
    /// only this lane's own record.
    #[test]
    fn adv_removal_never_prunes_the_whole_repository() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        let lane = wt.path().to_str().unwrap().to_string();
        crate::test_support::git_in(repo, &["worktree", "lock", &lane]);
        let wt_dir = wt.path().to_path_buf();
        let mut asked: Vec<String> = Vec::new();
        let mut gate = |argv: &[&str]| -> Result<(), String> {
            asked.push(argv.join(" "));
            Ok(())
        };
        let result = merge_and_teardown_with(
            repo.to_str().unwrap(),
            &lane,
            Some("main"),
            false,
            None,
            &mut gate,
            &mut |_| {
                // The lock is lifted and the directory goes, as a partial delete leaves it.
                let _ = git_text(repo, &["worktree", "unlock", &lane]);
                let _ = std::fs::remove_dir_all(&wt_dir);
            },
        )
        .expect("the merge landed");
        assert!(
            !asked.iter().any(|a| a.contains("prune")),
            "a prune reached the gate: {asked:?}"
        );
        assert!(
            result.worktree_removed,
            "only this lane's record should have finished the removal: {:?}",
            result.removal_error
        );
        let listed = git_text(repo, &["worktree", "list", "--porcelain"]).unwrap();
        assert_eq!(listed.matches("worktree ").count(), 1, "{listed}");
        let records = crate::engine::git_cli::resolve_git_common_dir(repo)
            .unwrap()
            .join("worktrees");
        let left = std::fs::read_dir(&records).map(|d| d.count()).unwrap_or(0);
        assert_eq!(left, 0, "this lane's record was not removed");
    }

    /// Originally `adv_the_rollback_runs_commands_the_gate_never_saw`, which
    /// recorded the bypass: the rollback's `merge --abort` / `reset --merge`
    /// ran without the gate being asked. It flipped when the rollback was put
    /// to the gate, and then flipped again on review: a refusal is honoured,
    /// not overruled, because running a command the ledger recorded as
    /// blocked makes the audit trail false. Main is left parked, the wave
    /// halts on the `conflict` code, and the error carries the gate's reason.
    #[test]
    fn adv_the_rollback_is_shown_to_the_gate() {
        let (dir, wt) = conflicting_fixture();
        let repo = dir.path();
        let mut asked: Vec<String> = Vec::new();
        let mut allow = |argv: &[&str]| -> Result<(), String> {
            asked.push(argv.join(" "));
            Ok(())
        };
        let _ = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut allow,
        );
        assert!(
            asked
                .iter()
                .any(|a| a == "merge --abort" || a == "reset --merge"),
            "the rollback never reached the gate: {asked:?}"
        );
        main_checkout_is_idle(repo);

        // A gate that refuses the rollback is honoured: the command is asked
        // about, never run, and the error says why. Main stays parked.
        for squash in [false, true] {
            let (dir, wt) = conflicting_fixture();
            let repo = dir.path();
            let mut asked: Vec<String> = Vec::new();
            let refused = merge_and_teardown_worktree(
                repo.to_str().unwrap(),
                wt.path().to_str().unwrap(),
                Some("main"),
                squash,
                None,
                &mut |argv: &[&str]| {
                    asked.push(argv.join(" "));
                    if argv.contains(&"--abort") || argv.contains(&"reset") {
                        Err("policy: no aborts".into())
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();
            let MergeTeardownError::Conflict {
                message,
                rolled_back,
                rollback_error,
                ..
            } = refused
            else {
                panic!("{refused:?}");
            };
            assert!(!rolled_back, "squash={squash}: {message}");
            let reason = rollback_error.expect("the refusal is the rollback error");
            assert!(
                reason.contains("the gate refused") && reason.contains("no aborts"),
                "{reason}"
            );
            assert!(
                asked
                    .iter()
                    .any(|a| a == "merge --abort" || a == "reset --merge"),
                "squash={squash}: the rollback was never put to the gate: {asked:?}"
            );
            // Nothing ran: the conflict is still in the checkout.
            assert!(
                crate::engine::repo_op::detect_kind(repo).unwrap().is_some(),
                "squash={squash}: the refused rollback ran anyway"
            );
            assert!(!git_text(repo, &["ls-files", "--unmerged"])
                .unwrap()
                .trim()
                .is_empty());
        }
    }

    /// Originally `adv_a_gate_that_denies_reset_still_sees_a_reset_run`: with
    /// the user's own unmerged index (a conflicted stash pop) in the main
    /// checkout, a marker-less rollback used to `reset --merge` it away even
    /// under a gate that denies reset. The merge is now refused up front, so
    /// no reset is ever asked for or run, whatever the gate says.
    #[test]
    fn adv_a_denied_reset_never_runs_on_a_users_unmerged_index() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        std::fs::write(repo.join("README.md"), "stashed side\n").unwrap();
        crate::test_support::git_in(repo, &["stash", "push", "-q", "-m", "adv-card22-gate"]);
        std::fs::write(repo.join("README.md"), "head side\n").unwrap();
        crate::test_support::git_in(repo, &["commit", "-qam", "head moves"]);
        let _ = std::process::Command::new("git")
            .args(["stash", "pop"])
            .current_dir(repo)
            .output_locked();
        std::fs::write(repo.join("README.md"), "hand resolved\n").unwrap();
        let mut gate = |argv: &[&str]| -> Result<(), String> {
            if argv.first() == Some(&"reset") {
                Err("policy: reset denied".into())
            } else {
                Ok(())
            }
        };
        let _ = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut gate,
        );
        assert_eq!(
            adv_read(&repo.join("README.md")),
            "hand resolved\n",
            "a reset the gate denies ran anyway and discarded the working-tree file"
        );
        crate::test_support::git_in(repo, &["stash", "drop", "-q"]);
    }

    /// Untracked file inside the lane's path set: removal must not delete a
    /// file written between the check and the removal. (Covered by
    /// ft-e460b9c2; guarded here so the rework keeps it.)
    #[test]
    fn adv_a_file_added_to_the_lane_by_the_gate_is_not_destroyed() {
        let (dir, wt) = teardown_fixture(None);
        let lane = wt.path().to_path_buf();
        let mut gate = |argv: &[&str]| -> Result<(), String> {
            if argv.first() == Some(&"worktree") {
                std::fs::write(lane.join("late.txt"), "written after the check\n").unwrap();
            }
            Ok(())
        };
        let res = merge_and_teardown_worktree(
            dir.path().to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut gate,
        )
        .unwrap();
        assert!(res.merged && !res.worktree_removed, "{res:?}");
        assert!(wt.path().join("late.txt").exists());
    }

    /// A second worktree of `repo`, on its own branch, that the tests lock
    /// while a teardown is in progress.
    fn second_lane(repo: &Path) -> tempfile::TempDir {
        let lane = tempfile::tempdir().expect("second lane dir");
        add_worktree_extended(
            repo.to_str().unwrap(),
            lane.path().to_str().unwrap(),
            Some("other-lane"),
            None,
            false,
            false,
            false,
        )
        .expect("second worktree");
        crate::test_support::trust_repo(lane.path());
        lane
    }

    /// While a deleter is parked, in the way the real recursive delete would be,
    /// another mutation of the repository (`lock_worktree` on a second lane)
    /// and a listing both complete, and the listing no longer shows the lane
    /// being torn down.
    fn assert_repo_is_free_meanwhile(repo: &Path, other: &Path, gone: &Path) {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let repo_text = repo.to_str().unwrap().to_string();
        let other_text = other.to_str().unwrap().to_string();
        std::thread::spawn(move || {
            let locked = lock_worktree(&repo_text, &other_text, Some("contender"));
            let listed = list_worktrees_lite(&repo_text);
            let _ = done_tx.send((locked, listed));
        });
        let (locked, listed) = done_rx.recv_timeout(Duration::from_secs(10)).expect(
            "a mutation and a listing were still waiting ten seconds into the deletion: \
             the repository lock is held across it",
        );
        locked.expect("the contender's lock_worktree");
        let listed = listed.expect("listing");
        let gone = lane_identity(gone);
        assert!(
            listed
                .iter()
                .all(|entry| lane_identity(Path::new(&entry.path)) != gone),
            "the lane being deleted is still listed: {listed:?}"
        );
    }

    /// The deletion of the files runs after the repository lock is released:
    /// the deleter is parked, and meanwhile `lock_worktree` on another lane and
    /// a listing both finish. Before the split the removal ran under the lock,
    /// and the deleter was never reached.
    #[test]
    fn teardown_delete_runs_outside_lock() {
        let (dir, wt) = teardown_fixture(None);
        let other = second_lane(dir.path());
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<PathBuf>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let repo_text = dir.path().to_str().unwrap().to_string();
        let wt_text = wt.path().to_str().unwrap().to_string();
        let teardown = std::thread::spawn(move || {
            merge_and_teardown_with_seams(
                &repo_text,
                &wt_text,
                Some("main"),
                false,
                None,
                &mut allow_all,
                &mut |_| {},
                &mut |aside: &Path| {
                    entered_tx.send(aside.to_path_buf()).unwrap();
                    let _ = release_rx.recv_timeout(Duration::from_secs(30));
                    std::fs::remove_dir_all(aside)
                },
            )
        });
        let aside = entered_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the deleter was never reached: the removal did not move the lane aside");
        assert!(aside.exists(), "the aside directory is not there to delete");
        assert!(!wt.path().exists(), "the lane is still in place");
        assert_repo_is_free_meanwhile(dir.path(), other.path(), wt.path());
        release_tx.send(()).unwrap();
        let result = teardown.join().unwrap().expect("the merge landed");
        assert!(
            result.worktree_removed && result.branch_deleted,
            "{result:?}"
        );
        assert_eq!(result.cleanup_error, None);
        assert!(!aside.exists(), "the aside directory was left behind");
    }

    /// The listing never takes the repository lock, so it is not held up for
    /// the duration of a teardown. Pinned with the gate parked inside the
    /// locked span, which holds whichever the implementation's lock is.
    #[test]
    fn listing_not_blocked_by_teardown() {
        let (dir, wt) = teardown_fixture(None);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let repo_text = dir.path().to_str().unwrap().to_string();
        let wt_text = wt.path().to_str().unwrap().to_string();
        let teardown = std::thread::spawn(move || {
            merge_and_teardown_worktree(
                &repo_text,
                &wt_text,
                Some("main"),
                false,
                None,
                &mut |argv: &[&str]| {
                    if argv.first() == Some(&"worktree") {
                        entered_tx.send(()).unwrap();
                        let _ = release_rx.recv_timeout(Duration::from_secs(30));
                    }
                    Ok(())
                },
            )
        });
        entered_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the teardown never reached its removal");
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let repo_text = dir.path().to_str().unwrap().to_string();
        std::thread::spawn(move || {
            let _ = done_tx.send((list_worktrees_lite(&repo_text), list_worktrees(&repo_text)));
        });
        let (lite, full) = done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("a listing was blocked by a teardown in progress");
        assert_eq!(lite.expect("lite listing").len(), 2);
        assert_eq!(full.expect("full listing").len(), 2);
        release_tx.send(()).unwrap();
        let result = teardown.join().unwrap().expect("the merge landed");
        assert!(result.worktree_removed, "{result:?}");
    }

    fn teardown_holding_dir(repo: &Path) -> PathBuf {
        resolve_git_common_dir(repo).unwrap().join(TEARDOWN_DIR)
    }

    /// Entries in `<common>/gitpulse-teardown`, or none when it does not exist.
    fn teardown_entries(repo: &Path) -> Vec<String> {
        std::fs::read_dir(teardown_holding_dir(repo))
            .map(|listing| {
                listing
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A planted leftover named like ours, old enough that no sweep treats it
    /// as another process's work in progress.
    fn plant_aside(repo: &Path, name: &str) -> PathBuf {
        let aside = teardown_holding_dir(repo).join(name);
        std::fs::create_dir_all(aside.join("build")).unwrap();
        std::fs::write(aside.join("build/out.o"), "object\n").unwrap();
        aside
    }

    /// The Worktrees panel's delete goes through the same split: the lock is
    /// free while the files are deleted, and the lane is gone from the listing.
    #[test]
    fn panel_remove_worktree_uses_the_same_split() {
        let (dir, wt) = teardown_fixture(None);
        let other = second_lane(dir.path());
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<PathBuf>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let repo_text = dir.path().to_str().unwrap().to_string();
        let wt_text = wt.path().to_str().unwrap().to_string();
        let removal = std::thread::spawn(move || {
            remove_worktree_with(&repo_text, &wt_text, true, &mut |aside: &Path| {
                entered_tx.send(aside.to_path_buf()).unwrap();
                let _ = release_rx.recv_timeout(Duration::from_secs(30));
                std::fs::remove_dir_all(aside)
            })
        });
        let aside = entered_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the panel's removal never reached its deleter");
        assert!(aside.starts_with(teardown_holding_dir(dir.path())) || aside.exists());
        assert_repo_is_free_meanwhile(dir.path(), other.path(), wt.path());
        release_tx.send(()).unwrap();
        let removed = removal.join().unwrap().expect("removed");
        assert_eq!(removed, WorktreeRemoval::default());
        assert!(!aside.exists() && !wt.path().exists());
    }

    /// Without `force`, git's own refusal of a worktree with uncommitted work
    /// still applies: nothing is moved and the deleter is not called.
    #[test]
    fn panel_remove_without_force_keeps_a_dirty_lane() {
        let (dir, wt) = teardown_fixture(None);
        std::fs::write(wt.path().join("scratch.txt"), "not committed\n").unwrap();
        let mut deleted = Vec::new();
        let refused = remove_worktree_with(
            dir.path().to_str().unwrap(),
            wt.path().to_str().unwrap(),
            false,
            &mut |aside: &Path| {
                deleted.push(aside.to_path_buf());
                Ok(())
            },
        )
        .unwrap_err();
        assert!(!refused.is_empty());
        assert!(deleted.is_empty(), "{deleted:?}");
        assert!(wt.path().join("scratch.txt").exists());
        assert!(teardown_entries(dir.path()).is_empty());
    }

    /// Without `force`, a clean lane takes the rename path too.
    #[test]
    fn panel_remove_without_force_splits_a_clean_lane() {
        let (dir, wt) = teardown_fixture(None);
        let mut deleted = Vec::new();
        let removed = remove_worktree_with(
            dir.path().to_str().unwrap(),
            wt.path().to_str().unwrap(),
            false,
            &mut |aside: &Path| {
                deleted.push(aside.to_path_buf());
                std::fs::remove_dir_all(aside)
            },
        )
        .expect("removed");
        assert_eq!(removed.cleanup_error, None);
        assert_eq!(deleted.len(), 1, "{deleted:?}");
        assert!(!wt.path().exists());
        assert_eq!(
            list_worktrees(dir.path().to_str().unwrap()).unwrap().len(),
            1
        );
    }

    /// A deleter that fails: git no longer knows the lane, so it is removed and
    /// the branch is deleted; the failure is reported naming the path, the
    /// files stay where they were put, and the next removal sweeps them.
    #[test]
    fn a_failed_delete_is_reported_and_swept_by_the_next_teardown() {
        let (dir, wt) = teardown_fixture(None);
        let result = merge_and_teardown_with_seams(
            dir.path().to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut allow_all,
            &mut |_| {},
            &mut |_: &Path| Err(std::io::Error::other("disk on fire")),
        )
        .expect("the merge landed");
        assert!(
            result.worktree_removed && result.branch_deleted,
            "{result:?}"
        );
        assert_eq!(result.removal_error, None);
        let reported = result.cleanup_error.expect("the failed delete is reported");
        assert!(reported.contains("disk on fire"), "{reported}");
        let left = teardown_entries(dir.path());
        assert_eq!(left.len(), 1, "{left:?}");
        assert!(
            reported.contains(&left[0]),
            "the report does not name the aside {}: {reported}",
            left[0]
        );
        assert!(!wt.path().exists());

        // The next removal on this repository clears it.
        let other = second_lane(dir.path());
        let removed = remove_worktree(
            dir.path().to_str().unwrap(),
            other.path().to_str().unwrap(),
            true,
        )
        .expect("removed");
        assert_eq!(removed.cleanup_error, None, "{removed:?}");
        assert!(
            teardown_entries(dir.path()).is_empty(),
            "the sweep left {:?}",
            teardown_entries(dir.path())
        );
    }

    /// A rename that cannot happen (here, the holding directory refuses
    /// entries) falls back to removing the worktree in place, and the
    /// CARD 22 result is the same as ever: removed, branch deleted, nothing to
    /// clean up, the deleter never called.
    #[cfg(unix)]
    #[test]
    fn a_rename_that_fails_falls_back_to_removal_under_the_lock() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, wt) = teardown_fixture(None);
        let holding = teardown_holding_dir(dir.path());
        std::fs::create_dir_all(&holding).unwrap();
        std::fs::set_permissions(&holding, std::fs::Permissions::from_mode(0o555)).unwrap();
        let mut deleted = Vec::new();
        let result = merge_and_teardown_with_seams(
            dir.path().to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut allow_all,
            &mut |_| {},
            &mut |aside: &Path| {
                deleted.push(aside.to_path_buf());
                Ok(())
            },
        );
        std::fs::set_permissions(&holding, std::fs::Permissions::from_mode(0o755)).unwrap();
        let result = result.expect("the merge landed");
        assert!(
            result.worktree_removed && result.branch_deleted,
            "{result:?}"
        );
        assert_eq!(result.cleanup_error, None);
        assert!(
            deleted.is_empty(),
            "the deleter ran for an in-place removal"
        );
        assert!(!wt.path().exists());
        assert!(teardown_entries(dir.path()).is_empty());
    }

    /// A locked record is refused before anything is renamed: the directory
    /// stays where it is and nothing is put in the holding directory.
    #[test]
    fn a_locked_lane_is_refused_before_the_rename() {
        let (dir, wt) = teardown_fixture(None);
        crate::test_support::git_in(
            dir.path(),
            &["worktree", "lock", wt.path().to_str().unwrap()],
        );
        let mut deleted = Vec::new();
        let result = merge_and_teardown_with_seams(
            dir.path().to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut allow_all,
            &mut |_| {},
            &mut |aside: &Path| {
                deleted.push(aside.to_path_buf());
                Ok(())
            },
        )
        .expect("the merge landed");
        assert!(!result.worktree_removed, "{result:?}");
        assert!(result.removal_error.is_some());
        assert!(deleted.is_empty());
        assert!(
            wt.path().join("feature.txt").exists(),
            "the lane was touched"
        );
        assert!(teardown_entries(dir.path()).is_empty());
    }

    /// A worktree with a `.gitmodules` file is left to git: no rename, and
    /// whatever git says about it is the result.
    #[test]
    fn a_lane_with_submodules_is_not_renamed() {
        let (dir, wt) = teardown_fixture(None);
        std::fs::write(
            wt.path().join(".gitmodules"),
            "[submodule \"lib\"]\n\tpath = lib\n\turl = ./lib.git\n",
        )
        .unwrap();
        crate::test_support::git_in(wt.path(), &["add", ".gitmodules"]);
        crate::test_support::git_in(wt.path(), &["commit", "-m", "declare a submodule"]);
        let mut deleted = Vec::new();
        let result = merge_and_teardown_with_seams(
            dir.path().to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut allow_all,
            &mut |_| {},
            &mut |aside: &Path| {
                deleted.push(aside.to_path_buf());
                Ok(())
            },
        )
        .expect("the merge landed");
        assert!(deleted.is_empty(), "a submodule lane was renamed aside");
        assert_eq!(result.worktree_removed, !wt.path().exists(), "{result:?}");
    }

    #[test]
    fn the_sweep_deletes_a_planted_aside_no_record_points_to() {
        let (dir, _wt) = teardown_fixture(None);
        let aside = plant_aside(dir.path(), "1-1-1");
        let report = sweep_teardown_asides(dir.path());
        assert_eq!(report.removed, 1, "{report:?}");
        assert_eq!(report.note(), None);
        assert!(!aside.exists());
    }

    #[test]
    fn the_sweep_keeps_an_aside_a_worktree_record_points_into() {
        let (dir, _wt) = teardown_fixture(None);
        let aside = plant_aside(dir.path(), "1-2-2");
        let record = resolve_git_common_dir(dir.path())
            .unwrap()
            .join("worktrees/pointing-in");
        std::fs::create_dir_all(&record).unwrap();
        std::fs::write(
            record.join("gitdir"),
            format!("{}\n", aside.join(".git").display()),
        )
        .unwrap();
        let report = sweep_teardown_asides(dir.path());
        assert_eq!((report.removed, report.kept), (0, 1), "{report:?}");
        assert!(
            aside.join("build/out.o").exists(),
            "a live lane was deleted"
        );
    }

    /// A symlink named like an aside is never followed or deleted through.
    #[cfg(unix)]
    #[test]
    fn the_sweep_refuses_a_symlinked_aside() {
        let (dir, _wt) = teardown_fixture(None);
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("precious.txt"), "keep\n").unwrap();
        let holding = teardown_holding_dir(dir.path());
        std::fs::create_dir_all(&holding).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), holding.join("1-3-3")).unwrap();
        let report = sweep_teardown_asides(dir.path());
        assert_eq!(report.removed, 0, "{report:?}");
        assert_eq!(report.problems.len(), 1, "{report:?}");
        assert!(report.note().is_some());
        assert!(elsewhere.path().join("precious.txt").exists());
    }

    /// Past the cap the rest wait, and the cap being hit is said.
    #[test]
    fn the_sweep_stops_at_its_cap_and_says_so() {
        let (dir, _wt) = teardown_fixture(None);
        for n in 0..SWEEP_CAP + 4 {
            plant_aside(dir.path(), &format!("1-{}-{n}", 100 + n));
        }
        let report = sweep_teardown_asides(dir.path());
        assert_eq!(report.removed, SWEEP_CAP, "{report:?}");
        assert_eq!(report.beyond_cap, 4, "{report:?}");
        let note = report.note().expect("the cap is reported");
        assert!(note.contains("4 more"), "{note}");
        assert_eq!(teardown_entries(dir.path()).len(), 4);
        // The next one finishes the job.
        let again = sweep_teardown_asides(dir.path());
        assert_eq!((again.removed, again.beyond_cap), (4, 0), "{again:?}");
        assert!(teardown_entries(dir.path()).is_empty());
    }

    /// Another process's fresh aside may still be in use by it.
    #[test]
    fn the_sweep_leaves_a_young_aside_of_another_process() {
        let (dir, _wt) = teardown_fixture(None);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let foreign = std::process::id().wrapping_add(1);
        let aside = plant_aside(dir.path(), &format!("{foreign}-{now}-0"));
        let report = sweep_teardown_asides(dir.path());
        assert_eq!(report.removed, 0, "{report:?}");
        assert!(aside.exists());
    }

    /// A sibling aside (made when the worktree is on another volume than the
    /// git directory) is found through its marker and deleted with it.
    #[test]
    fn the_sweep_deletes_a_sibling_aside_through_its_marker() {
        let (dir, wt) = teardown_fixture(None);
        let beside = wt
            .path()
            .parent()
            .unwrap()
            .join(".lane.gitpulse-teardown-1-4-4");
        std::fs::create_dir_all(beside.join("build")).unwrap();
        let holding = teardown_holding_dir(dir.path());
        std::fs::create_dir_all(&holding).unwrap();
        let marker = holding.join("1-4-4.sibling");
        // What a teardown writes: the path and a token in the marker, the
        // repository's common directory and the same token in the aside.
        std::fs::write(&marker, format!("{}\n{}", beside.display(), "feed01")).unwrap();
        std::fs::write(
            beside.join(SENTINEL_FILE),
            format!(
                "{}\n{}",
                resolve_git_common_dir(dir.path()).unwrap().display(),
                "feed01"
            ),
        )
        .unwrap();
        let report = sweep_teardown_asides(dir.path());
        assert_eq!(report.removed, 1, "{report:?}");
        assert!(!beside.exists() && !marker.exists());
    }

    /// A marker and a sentinel that disagree on the token, or a sentinel from
    /// another repository, delete nothing.
    #[test]
    fn the_sweep_needs_the_sentinel_to_match_the_marker() {
        let (dir, wt) = teardown_fixture(None);
        let holding = teardown_holding_dir(dir.path());
        std::fs::create_dir_all(&holding).unwrap();
        let common = resolve_git_common_dir(dir.path()).unwrap();
        for (stem, written, token) in [
            ("1-4-5", format!("{}\nother", common.display()), "feed01"),
            (
                "1-4-6",
                "/some/other/repo/.git\nfeed01".to_string(),
                "feed01",
            ),
        ] {
            let beside = wt
                .path()
                .parent()
                .unwrap()
                .join(format!(".lane.gitpulse-teardown-{stem}"));
            std::fs::create_dir_all(&beside).unwrap();
            std::fs::write(beside.join(SENTINEL_FILE), written).unwrap();
            std::fs::write(
                holding.join(format!("{stem}.sibling")),
                format!("{}\n{token}", beside.display()),
            )
            .unwrap();
            let report = sweep_teardown_asides(dir.path());
            assert_eq!(report.removed, 0, "{stem}: {report:?}");
            assert!(
                beside.exists(),
                "{stem}: a marker deleted what was not marked"
            );
            std::fs::remove_dir_all(&beside).unwrap();
        }
    }

    /// The put-back that fails after the record is gone (the second step of
    /// the rename path) keeps the files: a hold file is written inside them,
    /// the error says where they are, and the sweep refuses them and says so.
    #[test]
    fn a_failed_put_back_keeps_the_files_from_the_sweep() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        let lane = wt.path().to_str().unwrap().to_string();
        let held = try_rename_aside_with(
            repo,
            &lane,
            true,
            &mut |repo, lane| {
                // The record goes, then the check that follows fails: the
                // rename path must undo its rename, and cannot.
                remove_own_worktree_record(repo, lane)?;
                Err("injected: the record check failed".to_string())
            },
            &mut |_, _| Err(std::io::Error::other("injected: put back failed")),
        );
        let why = held.err().expect("the rename path reports the failure");
        assert!(
            why.contains("not swept") && why.contains("put back"),
            "{why}"
        );
        let entries = teardown_entries(repo);
        assert_eq!(entries.len(), 1, "{entries:?}");
        let aside = teardown_holding_dir(repo).join(&entries[0]);
        assert!(aside.join("feature.txt").exists(), "the files were lost");
        assert!(aside.join(HOLD_FILE).exists());
        // Old enough that nothing but the hold file protects it.
        let report = sweep_teardown_asides(repo);
        assert_eq!(report.removed, 0, "{report:?}");
        assert_eq!(report.problems.len(), 1, "{report:?}");
        assert!(
            aside.join("feature.txt").exists(),
            "the sweep took held files"
        );
    }

    /// A marker is trusted only for a name that is ours: one pointing at any
    /// other directory deletes nothing.
    #[test]
    fn the_sweep_will_not_follow_a_marker_to_a_directory_that_is_not_ours() {
        let (dir, _wt) = teardown_fixture(None);
        let victim = tempfile::tempdir().unwrap();
        std::fs::write(victim.path().join("precious.txt"), "keep\n").unwrap();
        let holding = teardown_holding_dir(dir.path());
        std::fs::create_dir_all(&holding).unwrap();
        std::fs::write(
            holding.join("1-6-6.sibling"),
            victim.path().to_string_lossy().as_bytes(),
        )
        .unwrap();
        let report = sweep_teardown_asides(dir.path());
        assert_eq!(report.removed, 0, "{report:?}");
        assert_eq!(report.problems.len(), 1, "{report:?}");
        assert!(victim.path().join("precious.txt").exists());
    }

    /// Entries not named like ours are none of the sweep's business.
    #[test]
    fn the_sweep_ignores_what_it_did_not_make() {
        let (dir, _wt) = teardown_fixture(None);
        let holding = teardown_holding_dir(dir.path());
        std::fs::create_dir_all(holding.join("keep-me")).unwrap();
        std::fs::write(holding.join("notes.txt"), "mine\n").unwrap();
        let report = sweep_teardown_asides(dir.path());
        assert_eq!(report, SweepReport::default());
        assert!(holding.join("keep-me").exists() && holding.join("notes.txt").exists());
    }

    // ---- Adversarial cases for the teardown lock split (CARD 22 S5-11) ----

    fn now_nanos() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    fn add_lane_at(repo: &Path, at: &Path, branch: &str) {
        add_worktree_extended(
            repo.to_str().unwrap(),
            at.to_str().unwrap(),
            Some(branch),
            None,
            false,
            false,
            false,
        )
        .expect("worktree");
        crate::test_support::trust_repo(at);
    }

    /// Paths `git worktree list` reports, by lane identity.
    fn listed_lanes(repo: &Path) -> Vec<PathBuf> {
        git_text(repo, &["worktree", "list", "--porcelain"])
            .unwrap()
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .map(|path| lane_identity(Path::new(path)))
            .collect()
    }

    fn is_listed(repo: &Path, lane: &Path) -> bool {
        listed_lanes(repo).contains(&lane_identity(lane))
    }

    /// Removes the lane with a recording deleter that really deletes.
    fn remove_recording(
        repo: &Path,
        lane: &Path,
        force: bool,
    ) -> (Result<WorktreeRemoval, String>, Vec<PathBuf>) {
        let mut asides = Vec::new();
        let result = remove_worktree_with(
            repo.to_str().unwrap(),
            lane.to_str().unwrap(),
            force,
            &mut |aside: &Path| {
                asides.push(aside.to_path_buf());
                std::fs::remove_dir_all(aside)
            },
        );
        (result, asides)
    }

    /// 1. Sweep safety: a hard link and symlinks inside an aside are unlinked,
    ///    and what they reach is untouched.
    #[cfg(unix)]
    #[test]
    fn adv_sweep_unlinks_links_without_touching_what_they_reach() {
        use std::os::unix::fs::symlink;
        let (dir, _wt) = teardown_fixture(None);
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("precious.txt"), "keep\n").unwrap();
        std::fs::create_dir(outside.path().join("sub")).unwrap();
        std::fs::write(outside.path().join("sub/deep.txt"), "deep\n").unwrap();
        let aside = plant_aside(dir.path(), "1-11-1");
        std::fs::hard_link(
            outside.path().join("precious.txt"),
            aside.join("linked.txt"),
        )
        .unwrap();
        symlink(outside.path(), aside.join("home-link")).unwrap();
        symlink(outside.path().join("sub"), aside.join("build/sub-link")).unwrap();
        let report = sweep_teardown_asides(dir.path());
        assert_eq!(
            (report.removed, report.problems.len()),
            (1, 0),
            "{report:?}"
        );
        assert!(!aside.exists());
        assert_eq!(
            std::fs::read_to_string(outside.path().join("precious.txt")).unwrap(),
            "keep\n"
        );
        assert_eq!(
            std::fs::read_to_string(outside.path().join("sub/deep.txt")).unwrap(),
            "deep\n"
        );
    }

    /// 1. A pattern-named path that is a regular file is left and reported.
    #[test]
    fn adv_sweep_leaves_a_regular_file_named_like_an_aside() {
        let (dir, _wt) = teardown_fixture(None);
        let holding = teardown_holding_dir(dir.path());
        std::fs::create_dir_all(&holding).unwrap();
        std::fs::write(holding.join("1-12-1"), "not a directory\n").unwrap();
        let report = sweep_teardown_asides(dir.path());
        assert_eq!(report.removed, 0, "{report:?}");
        assert_eq!(report.problems.len(), 1, "{report:?}");
        assert!(holding.join("1-12-1").is_file());
    }

    /// 1. Pattern-looking things the sweep does not own are never visited: a
    ///    nested one under a user's folder in the holding directory, and a
    ///    sibling-pattern directory with no marker beside a worktree.
    #[test]
    fn adv_sweep_ignores_pattern_names_outside_its_own_listing() {
        let (dir, wt) = teardown_fixture(None);
        let holding = teardown_holding_dir(dir.path());
        std::fs::create_dir_all(holding.join("mine/1-13-1")).unwrap();
        std::fs::write(holding.join("mine/1-13-1/keep.txt"), "mine\n").unwrap();
        let beside = wt
            .path()
            .parent()
            .unwrap()
            .join(".docs.gitpulse-teardown-1-13-2");
        std::fs::create_dir_all(&beside).unwrap();
        std::fs::write(beside.join("keep.txt"), "mine\n").unwrap();
        let report = sweep_teardown_asides(dir.path());
        assert_eq!(report, SweepReport::default());
        assert!(holding.join("mine/1-13-1/keep.txt").exists() && beside.join("keep.txt").exists());
        std::fs::remove_dir_all(&beside).unwrap();
    }

    /// 1. A hostile or corrupt marker: `/`, a home directory, a path through
    ///    `..`, a symlink with a correct name, a name with a trailing newline.
    #[cfg(unix)]
    #[test]
    fn adv_sweep_refuses_hostile_markers() {
        use std::os::unix::fs::symlink;
        let (dir, _wt) = teardown_fixture(None);
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("precious.txt"), "keep\n").unwrap();
        let holding = teardown_holding_dir(dir.path());
        std::fs::create_dir_all(&holding).unwrap();
        let named = |stem: &str, text: String| {
            std::fs::write(holding.join(format!("{stem}.sibling")), text).unwrap();
        };
        named("1-14-1", "/".into());
        named("1-14-2", home.path().to_string_lossy().into_owned());
        named("1-14-3", format!("{}/..", home.path().display()));
        let link = home
            .path()
            .parent()
            .unwrap()
            .join(".x.gitpulse-teardown-1-14-4");
        let _ = std::fs::remove_file(&link);
        symlink(home.path(), &link).unwrap();
        named("1-14-4", link.to_string_lossy().into_owned());
        named("1-14-5", format!("{}\n", link.display()));
        named("1-14-6", String::new());
        named("1-14-7", "x".repeat(10_000));
        let report = sweep_teardown_asides(dir.path());
        let _ = std::fs::remove_file(&link);
        assert_eq!(report.removed, 0, "{report:?}");
        assert!(home.path().join("precious.txt").exists());
    }

    /// 1. A marker that names a correctly spelled directory in a user's own
    ///    folder deletes it: the sweep checks the spelling, not that the place
    ///    is one a teardown could have used. Needs a planted or corrupt marker
    ///    under the git directory.
    #[test]
    fn adv_a_marker_cannot_make_the_sweep_delete_a_users_own_pattern_named_folder() {
        let (dir, _wt) = teardown_fixture(None);
        let users = tempfile::tempdir().unwrap();
        let theirs = users.path().join(".docs.gitpulse-teardown-1-15-1");
        std::fs::create_dir_all(&theirs).unwrap();
        std::fs::write(theirs.join("precious.txt"), "keep\n").unwrap();
        let holding = teardown_holding_dir(dir.path());
        std::fs::create_dir_all(&holding).unwrap();
        std::fs::write(
            holding.join("1-15-1.sibling"),
            theirs.to_string_lossy().as_bytes(),
        )
        .unwrap();
        let _ = sweep_teardown_asides(dir.path());
        assert!(
            theirs.join("precious.txt").exists(),
            "the sweep deleted a directory only because a marker named it"
        );
    }

    /// 1. An aside whose name says it was made in the future (a clock that
    ///    went backwards, or a bad name) is not held off forever.
    #[test]
    fn adv_a_foreign_aside_dated_far_in_the_future_is_eventually_swept() {
        let (dir, _wt) = teardown_fixture(None);
        let foreign = std::process::id().wrapping_add(1);
        let tomorrow = now_nanos() + 24 * 3600 * 1_000_000_000;
        let aside = plant_aside(dir.path(), &format!("{foreign}-{tomorrow}-1"));
        let report = sweep_teardown_asides(dir.path());
        assert_eq!(report.removed, 1, "{report:?}");
        assert!(!aside.exists());
    }

    /// 1. A foreign aside older than the age rule is swept.
    #[test]
    fn adv_a_foreign_aside_past_the_age_rule_is_swept() {
        let (dir, _wt) = teardown_fixture(None);
        let foreign = std::process::id().wrapping_add(1);
        let old = now_nanos() - 31 * 1_000_000_000;
        let aside = plant_aside(dir.path(), &format!("{foreign}-{old}-1"));
        let report = sweep_teardown_asides(dir.path());
        assert_eq!(report.removed, 1, "{report:?}");
        assert!(!aside.exists());
    }

    /// 1. Two sweeps at once (two threads here; two processes behave the same
    ///    way) over the same leftovers must both come back clean: the one that
    ///    loses the race is not a failure to report.
    #[test]
    fn adv_concurrent_sweeps_do_not_report_each_other_as_failures() {
        let (dir, _wt) = teardown_fixture(None);
        for round in 0..40 {
            for n in 0..4 {
                let aside = plant_aside(dir.path(), &format!("1-16-{round}{n}"));
                for file in 0..200 {
                    std::fs::write(aside.join(format!("f{file}")), "x").unwrap();
                }
            }
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let reports: Vec<SweepReport> = (0..2)
                .map(|_| {
                    let barrier = barrier.clone();
                    let repo = dir.path().to_path_buf();
                    std::thread::spawn(move || {
                        barrier.wait();
                        sweep_teardown_asides(&repo)
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect();
            for report in &reports {
                assert!(
                    report.problems.is_empty(),
                    "round {round}: a sweep reported a problem that was only the other sweep: {report:?}"
                );
            }
            assert!(teardown_entries(dir.path()).is_empty());
        }
    }

    /// 1. An aside this process is deleting right now is not swept, marker or
    ///    no marker, even when its name is old enough for any other rule.
    #[test]
    fn adv_an_aside_in_flight_in_this_process_is_not_swept() {
        let (dir, _wt) = teardown_fixture(None);
        let aside = plant_aside(dir.path(), &format!("{}-1-17", std::process::id()));
        let held = TeardownAside::register(aside.clone(), None);
        let report = sweep_teardown_asides(dir.path());
        assert_eq!(report.removed, 0, "{report:?}");
        assert!(aside.join("build/out.o").exists());
        drop(held);
        let report = sweep_teardown_asides(dir.path());
        assert_eq!(report.removed, 1, "{report:?}");
    }

    /// 1/3. A sweep that runs while a teardown's deleter is parked (the real
    /// delete is in progress) leaves that teardown's aside alone.
    #[test]
    fn adv_a_sweep_during_a_live_delete_leaves_that_aside_alone() {
        let (dir, wt) = teardown_fixture(None);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<PathBuf>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let repo_text = dir.path().to_str().unwrap().to_string();
        let wt_text = wt.path().to_str().unwrap().to_string();
        let removal = std::thread::spawn(move || {
            remove_worktree_with(&repo_text, &wt_text, true, &mut |aside: &Path| {
                entered_tx.send(aside.to_path_buf()).unwrap();
                let _ = release_rx.recv_timeout(Duration::from_secs(30));
                std::fs::remove_dir_all(aside)
            })
        });
        let aside = entered_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("deleter");
        let report = sweep_teardown_asides(dir.path());
        assert_eq!(
            report,
            SweepReport::default(),
            "the sweep touched a live delete"
        );
        assert!(
            aside.join("feature.txt").exists(),
            "the sweep deleted a live aside"
        );
        release_tx.send(()).unwrap();
        assert_eq!(removal.join().unwrap().unwrap(), WorktreeRemoval::default());
    }

    /// 2. A worktree path with spaces and non-ASCII characters.
    #[test]
    fn adv_unicode_and_space_worktree_path_takes_the_split() {
        let (dir, _wt) = teardown_fixture(None);
        let parent = tempfile::tempdir().unwrap();
        let lane = parent.path().join("wörk tree ✓ 日本");
        add_lane_at(dir.path(), &lane, "uni-lane");
        std::fs::write(lane.join("ünï.txt"), "x").unwrap();
        let (result, asides) = remove_recording(dir.path(), &lane, true);
        assert_eq!(result.unwrap(), WorktreeRemoval::default());
        assert_eq!(asides.len(), 1, "{asides:?}");
        assert!(!lane.exists() && !is_listed(dir.path(), &lane));
        assert!(teardown_entries(dir.path()).is_empty());
    }

    /// 2. A long final component (the holding-directory aside has its own short
    ///    name, so the length of the worktree's does not matter).
    #[test]
    fn adv_a_very_long_worktree_name_takes_the_split() {
        let (dir, _wt) = teardown_fixture(None);
        let parent = tempfile::tempdir().unwrap();
        let lane = parent.path().join("l".repeat(240));
        add_lane_at(dir.path(), &lane, "long-lane");
        let (result, asides) = remove_recording(dir.path(), &lane, true);
        assert_eq!(result.unwrap(), WorktreeRemoval::default());
        assert_eq!(asides.len(), 1, "{asides:?}");
        assert!(!lane.exists() && !is_listed(dir.path(), &lane));
    }

    /// 2. A process whose cwd is inside the worktree: macOS allows the rename.
    #[cfg(unix)]
    #[test]
    fn adv_a_running_process_inside_the_worktree_does_not_stop_the_removal() {
        let (dir, wt) = teardown_fixture(None);
        let mut child = std::process::Command::new("sleep")
            .arg("120")
            .current_dir(wt.path())
            .spawn_locked()
            .expect("sleep");
        let (result, asides) = remove_recording(dir.path(), wt.path(), true);
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(result.unwrap(), WorktreeRemoval::default());
        assert_eq!(asides.len(), 1, "{asides:?}");
        assert!(!wt.path().exists() && !is_listed(dir.path(), wt.path()));
    }

    /// 2. A worktree holding a nested repository: forced, it goes whole; not
    ///    forced, git's refusal applies and nothing moves.
    #[test]
    fn adv_a_nested_repository_inside_the_worktree() {
        let (dir, wt) = teardown_fixture(None);
        let nested = wt.path().join("vendor/inner");
        std::fs::create_dir_all(&nested).unwrap();
        crate::test_support::git_in(&nested, &["init", "-b", "main", "."]);
        std::fs::write(nested.join("f"), "x").unwrap();
        crate::test_support::git_in(&nested, &["add", "."]);
        crate::test_support::git_in(
            &nested,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-m",
                "i",
            ],
        );
        let (refused, asides) = remove_recording(dir.path(), wt.path(), false);
        assert!(
            refused.is_err() && asides.is_empty(),
            "{refused:?} {asides:?}"
        );
        assert!(nested.join("f").exists() && teardown_entries(dir.path()).is_empty());
        let (result, asides) = remove_recording(dir.path(), wt.path(), true);
        assert_eq!(result.unwrap(), WorktreeRemoval::default());
        assert_eq!(asides.len(), 1);
        assert!(!wt.path().exists());
    }

    /// 2. A worktree path that is a symlink to the worktree: the real
    ///    directory goes, as `git worktree remove` of the resolved path did.
    #[cfg(unix)]
    #[test]
    fn adv_a_symlinked_worktree_path_removes_the_real_directory() {
        let (dir, wt) = teardown_fixture(None);
        let links = tempfile::tempdir().unwrap();
        let link = links.path().join("lane-link");
        std::os::unix::fs::symlink(wt.path(), &link).unwrap();
        let (result, _) = remove_recording(dir.path(), &link, true);
        assert_eq!(result.unwrap(), WorktreeRemoval::default());
        assert!(!wt.path().exists(), "the real directory is still there");
        assert!(!is_listed(dir.path(), wt.path()));
        assert!(teardown_entries(dir.path()).is_empty());
    }

    /// 2. Removing the checkout the call is made from (repo = the worktree
    ///    itself) leaves no half state: the directory is there exactly when git
    ///    lists it.
    #[test]
    fn adv_removing_the_worktree_the_call_is_rooted_in_is_not_half_done() {
        let (dir, wt) = teardown_fixture(None);
        let mut deleted = Vec::new();
        let result = remove_worktree_with(
            wt.path().to_str().unwrap(),
            wt.path().to_str().unwrap(),
            true,
            &mut |aside: &Path| {
                deleted.push(aside.to_path_buf());
                std::fs::remove_dir_all(aside)
            },
        );
        let here = wt.path().exists();
        let listed = is_listed(dir.path(), wt.path());
        eprintln!("self-removal: {result:?} dir_exists={here} listed={listed} deleted={deleted:?}");
        assert_eq!(
            here, listed,
            "half state: dir_exists={here} listed={listed} ({result:?})"
        );
        assert!(teardown_entries(dir.path()).is_empty() || deleted.is_empty());
    }

    /// 2. The main checkout is never renamed.
    #[test]
    fn adv_the_main_checkout_is_not_renamed() {
        let (dir, _wt) = teardown_fixture(None);
        let (result, asides) = remove_recording(dir.path(), dir.path(), true);
        assert!(result.is_err(), "{result:?}");
        assert!(asides.is_empty());
        assert!(dir.path().join("README.md").exists());
        assert!(teardown_entries(dir.path()).is_empty());
    }

    /// 2. A worktree whose parent is read-only cannot be renamed out; it falls
    ///    back to git's removal and leaves no aside or marker behind.
    #[cfg(unix)]
    #[test]
    fn adv_a_read_only_parent_falls_back_and_leaves_nothing_behind() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, _wt) = teardown_fixture(None);
        let parent = tempfile::tempdir().unwrap();
        let lane = parent.path().join("lane");
        add_lane_at(dir.path(), &lane, "ro-parent-lane");
        std::fs::set_permissions(parent.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        let (result, asides) = remove_recording(dir.path(), &lane, true);
        std::fs::set_permissions(parent.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!(
            "read-only parent: {result:?} asides={asides:?} dir={} listed={}",
            lane.exists(),
            is_listed(dir.path(), &lane)
        );
        assert!(
            asides.is_empty(),
            "renamed out of a read-only parent: {asides:?}"
        );
        assert!(teardown_entries(dir.path()).is_empty());
        // Git's own removal (the unchanged fallback) unlists the lane before
        // its rmdir fails: that half state is git's, as it was before the split.
    }

    /// 2/4. A read-only directory inside the worktree makes the delete fail
    /// after the worktree is already unlisted: reported, named, kept for the
    /// sweep, and cleared by the next removal once it can be.
    #[cfg(unix)]
    #[test]
    fn adv_an_undeletable_tree_is_reported_and_retried() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, wt) = teardown_fixture(None);
        let locked = wt.path().join("ro");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("f"), "x").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        let (result, _) = remove_recording(dir.path(), wt.path(), true);
        let first = result.expect("the worktree is removed");
        let left = teardown_entries(dir.path());
        let reported = first
            .cleanup_error
            .clone()
            .expect("the failed delete is reported");
        assert!(!is_listed(dir.path(), wt.path()), "still listed");
        assert_eq!(left.len(), 1, "{left:?}");
        assert!(reported.contains(&left[0]), "{reported}");
        let aside = teardown_holding_dir(dir.path()).join(&left[0]);
        // Still stuck: the next removal reports it again rather than hiding it.
        let other = tempfile::tempdir().unwrap();
        add_lane_at(dir.path(), other.path(), "undeletable-b");
        let (second, _) = remove_recording(dir.path(), other.path(), true);
        let second = second.expect("removed");
        assert!(
            !second.sweep_problems.is_empty() && second.cleanup_error.is_none(),
            "a leftover that cannot be deleted went quiet: {second:?}"
        );
        std::fs::set_permissions(aside.join("ro"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let third = tempfile::tempdir().unwrap();
        add_lane_at(dir.path(), third.path(), "undeletable-c");
        let (third, _) = remove_recording(dir.path(), third.path(), true);
        assert_eq!(third.unwrap(), WorktreeRemoval::default());
        assert!(teardown_entries(dir.path()).is_empty());
    }

    /// 3. Crash after the rename and before the record is removed: the aside
    ///    is swept by the next call, and the record left behind is consistent
    ///    (listed as a missing directory), clearable by prune.
    #[test]
    fn adv_crash_between_rename_and_record_removal_recovers() {
        let (dir, wt) = teardown_fixture(None);
        let holding = teardown_holding_dir(dir.path());
        std::fs::create_dir_all(&holding).unwrap();
        let aside = holding.join("1-20-1");
        std::fs::rename(wt.path(), &aside).unwrap();
        assert!(
            is_listed(dir.path(), wt.path()),
            "the record should still be there"
        );
        let other = second_lane(dir.path());
        let (result, _) = remove_recording(dir.path(), other.path(), true);
        assert_eq!(result.unwrap(), WorktreeRemoval::default());
        assert!(!aside.exists(), "the aside was not swept");
        // Retrying the same removal: what does the person get?
        let retry = remove_worktree(
            dir.path().to_str().unwrap(),
            wt.path().to_str().unwrap(),
            true,
        );
        let merge_retry = merge_and_teardown_worktree(
            dir.path().to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut allow_all,
        );
        eprintln!("retry after crash: panel={retry:?} merge={merge_retry:?}");
        prune_worktree(dir.path().to_str().unwrap()).unwrap();
        assert!(
            !is_listed(dir.path(), wt.path()),
            "prune did not clear the record"
        );
        assert!(branch_exists(dir.path(), "feature-x"));
    }

    /// 3. The retry of a teardown whose worktree directory was moved aside by
    ///    a crash must not be stuck: either it finishes, or it says what to do.
    #[test]
    fn adv_the_same_removal_retried_after_a_crash_finishes() {
        let (dir, wt) = teardown_fixture(None);
        let holding = teardown_holding_dir(dir.path());
        std::fs::create_dir_all(&holding).unwrap();
        std::fs::rename(wt.path(), holding.join("1-21-1")).unwrap();
        let retry = remove_worktree(
            dir.path().to_str().unwrap(),
            wt.path().to_str().unwrap(),
            true,
        );
        assert!(
            retry.is_ok(),
            "the retry of the same removal is refused: {retry:?}"
        );
        assert!(!is_listed(dir.path(), wt.path()));
    }

    /// 3. Crash after the record is gone and before the delete: the aside is
    ///    all that is left, and the sweep takes it.
    #[test]
    fn adv_crash_after_record_removal_leaves_only_an_aside_the_sweep_takes() {
        let (dir, wt) = teardown_fixture(None);
        let holding = teardown_holding_dir(dir.path());
        std::fs::create_dir_all(&holding).unwrap();
        let aside = holding.join("1-22-1");
        std::fs::rename(wt.path(), &aside).unwrap();
        git_text(dir.path(), &["worktree", "prune", "--expire", "now"]).unwrap();
        assert!(!is_listed(dir.path(), wt.path()));
        let report = sweep_teardown_asides(dir.path());
        assert_eq!(report.removed, 1, "{report:?}");
        assert!(!aside.exists());
    }

    /// 3. A delete that died half way: a partial tree is swept.
    #[test]
    fn adv_a_partially_deleted_tree_is_swept() {
        let (dir, _wt) = teardown_fixture(None);
        let aside = plant_aside(dir.path(), "1-23-1");
        std::fs::create_dir_all(aside.join("a/b/c")).unwrap();
        std::fs::write(aside.join("a/b/c/leaf"), "x").unwrap();
        std::fs::create_dir_all(aside.join("emptied")).unwrap();
        let report = sweep_teardown_asides(dir.path());
        assert_eq!(
            (report.removed, report.problems.len()),
            (1, 0),
            "{report:?}"
        );
        assert!(!aside.exists());
    }

    /// 4. The panel's removal with a failing delete: removed, unlisted,
    ///    the error names the aside.
    #[test]
    fn adv_panel_removal_with_a_failing_delete_reports_the_path() {
        let (dir, wt) = teardown_fixture(None);
        let removal = remove_worktree_with(
            dir.path().to_str().unwrap(),
            wt.path().to_str().unwrap(),
            true,
            &mut |_: &Path| Err(std::io::Error::other("disk on fire")),
        )
        .expect("removed");
        let left = teardown_entries(dir.path());
        let reported = removal.cleanup_error.expect("reported");
        assert!(
            reported.contains("disk on fire") && reported.contains(&left[0]),
            "{reported}"
        );
        assert!(!is_listed(dir.path(), wt.path()) && !wt.path().exists());
    }

    /// 4. A sweep problem (a symlinked leftover) is carried in `cleanup_error`
    ///    of a removal whose own delete succeeded. The field is "something to
    ///    clean up", not "this delete failed".
    #[cfg(unix)]
    #[test]
    fn adv_a_sweep_problem_shows_on_an_unrelated_successful_removal() {
        let (dir, wt) = teardown_fixture(None);
        let target = tempfile::tempdir().unwrap();
        let holding = teardown_holding_dir(dir.path());
        std::fs::create_dir_all(&holding).unwrap();
        std::os::unix::fs::symlink(target.path(), holding.join("1-24-1")).unwrap();
        let (result, asides) = remove_recording(dir.path(), wt.path(), true);
        let removal = result.unwrap();
        assert_eq!(asides.len(), 1);
        // Reported, but in its own field: `cleanup_error` is about this
        // removal's own files, and this removal's delete succeeded.
        assert_eq!(removal.cleanup_error, None, "{removal:?}");
        assert_eq!(removal.sweep_problems.len(), 1, "{removal:?}");
        assert!(target.path().exists());
    }

    /// 5. When the deleter starts: the lock is free, the branch is already
    ///    deleted (under the lock), the lane is unlisted, and the post-merge hook
    ///    has not run yet; it runs after the delete.
    #[test]
    fn adv_lock_released_before_delete_branch_deleted_under_it_hooks_after() {
        let marker_dir = tempfile::tempdir().unwrap();
        let ran = marker_dir.path().join("hook-ran");
        let hooks = format!(
            r#"{{"worktree":{{"post_merge":["touch {}"]}}}}"#,
            ran.display()
        );
        let (dir, wt) = teardown_fixture(Some(&hooks));
        let other = second_lane(dir.path());
        let mut observed = None;
        let result = merge_and_teardown_with_seams(
            dir.path().to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut allow_all,
            &mut |_| {},
            &mut |aside: &Path| {
                assert_repo_is_free_meanwhile(dir.path(), other.path(), wt.path());
                observed = Some((branch_exists(dir.path(), "feature-x"), ran.exists()));
                std::fs::remove_dir_all(aside)
            },
        )
        .expect("merged");
        assert_eq!(
            observed,
            Some((false, false)),
            "(branch still there, hook already ran)"
        );
        assert!(
            result.worktree_removed && result.branch_deleted && result.hook_error.is_none(),
            "{result:?}"
        );
        assert!(ran.exists(), "the post-merge hook never ran");
    }

    /// 5. Two removals of the same lane at once: one wins, the other is
    ///    refused cleanly, and nothing is left behind.
    #[test]
    fn adv_two_removals_of_one_lane_leave_nothing_behind() {
        let (dir, wt) = teardown_fixture(None);
        let results: Vec<_> = (0..2)
            .map(|_| {
                let repo = dir.path().to_str().unwrap().to_string();
                let lane = wt.path().to_str().unwrap().to_string();
                std::thread::spawn(move || remove_worktree(&repo, &lane, true))
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert!(results.iter().any(Result::is_ok), "{results:?}");
        assert!(!wt.path().exists() && !is_listed(dir.path(), wt.path()));
        assert!(teardown_entries(dir.path()).is_empty());
    }

    /// A small APFS volume mounted under a temp directory, so a worktree can
    /// sit on another device than the repository's git directory.
    #[cfg(target_os = "macos")]
    struct DiskImage {
        mount: PathBuf,
        _dir: tempfile::TempDir,
    }

    #[cfg(target_os = "macos")]
    impl DiskImage {
        fn attach() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let image = dir.path().join("volume.dmg");
            let mount = dir.path().join("mnt");
            std::fs::create_dir(&mount).unwrap();
            let run = |args: &[&std::ffi::OsStr]| {
                let out = std::process::Command::new("hdiutil")
                    .args(args)
                    .output_locked()
                    .expect("hdiutil");
                assert!(
                    out.status.success(),
                    "hdiutil {args:?}: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            };
            run(&[
                "create".as_ref(),
                "-size".as_ref(),
                "64m".as_ref(),
                "-fs".as_ref(),
                "APFS".as_ref(),
                "-volname".as_ref(),
                "gpxdev".as_ref(),
                image.as_os_str(),
            ]);
            run(&[
                "attach".as_ref(),
                "-nobrowse".as_ref(),
                "-mountpoint".as_ref(),
                mount.as_os_str(),
                image.as_os_str(),
            ]);
            let mount = mount.canonicalize().unwrap();
            Self { mount, _dir: dir }
        }
    }

    #[cfg(target_os = "macos")]
    impl Drop for DiskImage {
        fn drop(&mut self) {
            let _ = std::process::Command::new("hdiutil")
                .args(["detach", "-force"])
                .arg(&self.mount)
                .output_locked();
        }
    }

    /// 2. Cross-device: the aside goes beside the lane with a marker in the
    ///    holding directory, both are cleaned up after the delete.
    #[cfg(target_os = "macos")]
    #[test]
    fn adv_cross_device_worktree_gets_a_sibling_aside_and_marker() {
        let (dir, _wt) = teardown_fixture(None);
        let volume = DiskImage::attach();
        let lane = volume.mount.join("lane");
        add_lane_at(dir.path(), &lane, "xdev-lane");
        assert!(
            !same_device(&lane, teardown_holding_dir(dir.path()).parent().unwrap()),
            "not a cross-device fixture"
        );
        let mut seen = None;
        let result = remove_worktree_with(
            dir.path().to_str().unwrap(),
            lane.to_str().unwrap(),
            true,
            &mut |aside: &Path| {
                seen = Some((aside.to_path_buf(), teardown_entries(dir.path())));
                std::fs::remove_dir_all(aside)
            },
        )
        .expect("removed");
        assert_eq!(result, WorktreeRemoval::default());
        let (aside, markers) = seen.expect("the deleter ran");
        assert_eq!(aside.parent().unwrap(), volume.mount, "{aside:?}");
        assert!(aside
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(".lane.gitpulse-teardown-"));
        assert_eq!(markers.len(), 1, "{markers:?}");
        assert!(markers[0].ends_with(".sibling"));
        assert!(
            teardown_entries(dir.path()).is_empty(),
            "marker left behind"
        );
        assert!(!lane.exists() && !aside.exists() && !is_listed(dir.path(), &lane));
    }

    /// 2/3. Cross-device, delete fails: the sibling aside and its marker stay;
    /// the next removal sweeps both.
    #[cfg(target_os = "macos")]
    #[test]
    fn adv_cross_device_failed_delete_is_swept_through_its_marker() {
        let (dir, _wt) = teardown_fixture(None);
        let volume = DiskImage::attach();
        let lane = volume.mount.join("lane");
        add_lane_at(dir.path(), &lane, "xdev-lane");
        let removal = remove_worktree_with(
            dir.path().to_str().unwrap(),
            lane.to_str().unwrap(),
            true,
            &mut |_: &Path| Err(std::io::Error::other("disk on fire")),
        )
        .expect("removed");
        assert!(removal.cleanup_error.is_some());
        assert_eq!(teardown_entries(dir.path()).len(), 1);
        let beside: Vec<_> = std::fs::read_dir(&volume.mount)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains("gitpulse-teardown"))
            .collect();
        assert_eq!(beside.len(), 1, "{beside:?}");
        // The marker's name carries our pid and the time, so it is ours to
        // retry at once.
        let other = second_lane(dir.path());
        let (swept, _) = remove_recording(dir.path(), other.path(), true);
        assert_eq!(swept.unwrap(), WorktreeRemoval::default());
        assert!(teardown_entries(dir.path()).is_empty());
        assert!(
            !volume.mount.join(&beside[0]).exists(),
            "sibling aside not swept"
        );
    }

    /// 2. Cross-device with a final name so long the sibling aside's name
    ///    would exceed NAME_MAX: the rename cannot happen, so the removal is
    ///    in place as before, and no marker is left.
    #[cfg(target_os = "macos")]
    #[test]
    fn adv_cross_device_name_too_long_for_a_sibling_falls_back() {
        let (dir, _wt) = teardown_fixture(None);
        let volume = DiskImage::attach();
        let lane = volume.mount.join("n".repeat(250));
        add_lane_at(dir.path(), &lane, "xdev-long");
        let (result, asides) = remove_recording(dir.path(), &lane, true);
        assert_eq!(result.unwrap(), WorktreeRemoval::default());
        assert!(asides.is_empty(), "{asides:?}");
        assert!(teardown_entries(dir.path()).is_empty(), "marker leaked");
        assert!(!lane.exists() && !is_listed(dir.path(), &lane));
    }

    /// Records that cannot be read leave "a record points into it" unchecked,
    /// so the sweep deletes nothing that pass and says why.
    #[cfg(unix)]
    #[test]
    fn the_sweep_deletes_nothing_when_the_worktree_records_cannot_be_read() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, _wt) = teardown_fixture(None);
        let aside = plant_aside(dir.path(), "1-31-1");
        let records = resolve_git_common_dir(dir.path())
            .unwrap()
            .join("worktrees");
        std::fs::set_permissions(&records, std::fs::Permissions::from_mode(0o000)).unwrap();
        let report = sweep_teardown_asides(dir.path());
        std::fs::set_permissions(&records, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(report.removed, 0, "{report:?}");
        assert_eq!(report.problems.len(), 1, "{report:?}");
        assert!(report.problems[0].contains("records"), "{report:?}");
        assert!(aside.join("build/out.o").exists(), "deleted blind");
        // Readable again, the same pass succeeds.
        assert_eq!(sweep_teardown_asides(dir.path()).removed, 1);
    }

    /// A record whose `gitdir` cannot be read is not skipped as pointing
    /// nowhere: that could be the one pointing into the aside.
    #[cfg(unix)]
    #[test]
    fn the_sweep_deletes_nothing_when_one_records_gitdir_cannot_be_read() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, _wt) = teardown_fixture(None);
        let aside = plant_aside(dir.path(), "1-32-1");
        let records = resolve_git_common_dir(dir.path())
            .unwrap()
            .join("worktrees");
        let record = std::fs::read_dir(&records)
            .unwrap()
            .flatten()
            .next()
            .expect("the fixture's lane has a record")
            .path();
        let gitdir = record.join("gitdir");
        std::fs::set_permissions(&gitdir, std::fs::Permissions::from_mode(0o000)).unwrap();
        let report = sweep_teardown_asides(dir.path());
        std::fs::set_permissions(&gitdir, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            (report.removed, report.problems.len()),
            (0, 1),
            "{report:?}"
        );
        assert!(aside.exists());
    }

    /// No `worktrees` directory means no linked worktrees, which is not a
    /// failure to read them: the sweep goes ahead.
    #[test]
    fn the_sweep_goes_ahead_when_there_are_no_linked_worktrees() {
        let (dir, _wt) = teardown_fixture(None);
        let aside = plant_aside(dir.path(), "1-33-1");
        let records = resolve_git_common_dir(dir.path())
            .unwrap()
            .join("worktrees");
        std::fs::remove_dir_all(&records).unwrap();
        let report = sweep_teardown_asides(dir.path());
        assert_eq!(
            (report.removed, report.problems.len()),
            (1, 0),
            "{report:?}"
        );
        assert!(!aside.exists());
    }

    /// A directory entry that fails to read is reported, and the entries that
    /// did read are still looked at.
    #[test]
    fn the_sweep_reports_a_directory_entry_it_could_not_read() {
        let (dir, _wt) = teardown_fixture(None);
        let aside = plant_aside(dir.path(), "1-34-1");
        let path = aside.clone();
        let report = sweep_teardown_asides_from(dir.path(), &|_| {
            Ok(vec![
                Err(std::io::Error::other("injected: entry unreadable")),
                Ok(("1-34-1".to_string(), path.clone())),
            ])
        });
        assert_eq!(report.removed, 1, "{report:?}");
        assert_eq!(report.problems.len(), 1, "{report:?}");
        assert!(
            report.problems[0].contains("entry unreadable"),
            "{report:?}"
        );
        assert!(!aside.exists());
    }
}
